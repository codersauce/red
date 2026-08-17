mod common;

use std::sync::{Arc, Mutex};

use common::{LspEvent, RecordingLsp};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use red::{
    buffer::Buffer,
    config::{Config, KeyAction},
    editor::{Action, Editor, Mode},
    lsp::{
        Command, CompletionResponseItem, InsertTextFormat, LspClient, Position, Range, TextEdit,
    },
    test_utils::EditorTestExt,
    theme::Theme,
};
use serde_json::json;

fn recording_editor(buffer: Buffer) -> (Editor, Arc<Mutex<Vec<LspEvent>>>) {
    let lsp = RecordingLsp::default();
    let events = lsp.events();
    let lsp = Box::new(lsp) as Box<dyn LspClient + Send>;
    let config = Config::default();
    let theme = Theme::default();
    let mut editor = Editor::with_size(lsp, 80, 24, config, theme, vec![buffer]).unwrap();
    editor.test_disable_terminal_output();
    (editor, events)
}

fn recorded(events: &Arc<Mutex<Vec<LspEvent>>>) -> Vec<LspEvent> {
    events.lock().unwrap().clone()
}

fn item(label: &str) -> CompletionResponseItem {
    CompletionResponseItem {
        label: label.to_string(),
        label_details: None,
        kind: None,
        detail: None,
        documentation: None,
        deprecated: None,
        preselect: None,
        sort_text: None,
        filter_text: None,
        insert_text: None,
        insert_text_format: None,
        text_edit: None,
        additional_text_edits: None,
        command: None,
        data: None,
        commit_characters: None,
    }
}

fn range(start_line: usize, start: usize, end_line: usize, end: usize) -> Range {
    Range {
        start: Position {
            line: start_line,
            character: start,
        },
        end: Position {
            line: end_line,
            character: end,
        },
    }
}

fn text_edit(range: Range, new_text: &str) -> TextEdit {
    TextEdit {
        range,
        new_text: new_text.to_string(),
    }
}

#[tokio::test]
async fn request_completion_sends_invoked_context_from_insert_mode() {
    let (mut editor, events) = recording_editor(Buffer::new(
        Some("src/main.rs".to_string()),
        "foo".to_string(),
    ));

    editor
        .test_execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::SetCursor(3, 0))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::RequestCompletion)
        .await
        .unwrap();

    assert!(
        recorded(&events).iter().any(|event| {
            matches!(
                event,
                LspEvent::RequestCompletion {
                    line: 0,
                    character: 3,
                    trigger_character: None,
                    ..
                }
            )
        }),
        "expected manual completion request, got {:?}",
        recorded(&events)
    );
}

#[tokio::test]
async fn request_completion_sends_trigger_character_context() {
    let (mut editor, events) = recording_editor(Buffer::new(
        Some("src/main.rs".to_string()),
        "value.".to_string(),
    ));

    editor
        .test_execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::SetCursor(6, 0))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::RequestCompletionWithTrigger('.'))
        .await
        .unwrap();

    assert!(
        recorded(&events).iter().any(|event| {
            matches!(
                event,
                LspEvent::RequestCompletion {
                    line: 0,
                    character: 6,
                    trigger_character: Some('.'),
                    ..
                }
            )
        }),
        "expected trigger completion request, got {:?}",
        recorded(&events)
    );
}

#[tokio::test]
async fn request_completion_uses_utf16_position_after_an_emoji() {
    let (mut editor, events) = recording_editor(Buffer::new(
        Some("src/main.rs".to_string()),
        "😀 target".to_string(),
    ));
    editor
        .test_execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::SetCursor(2, 0))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::RequestCompletion)
        .await
        .unwrap();

    assert!(recorded(&events).iter().any(|event| matches!(
        event,
        LspEvent::RequestCompletion {
            line: 0,
            character: 3,
            ..
        }
    )));
}

#[tokio::test]
async fn apply_completion_uses_text_edit_additional_edits_and_one_undo_step() {
    let (mut editor, _) = recording_editor(Buffer::new(None, "mod stuff;\nfoo\n".to_string()));
    let mut completion = item("Foo");
    completion.text_edit = Some(text_edit(range(1, 0, 1, 3), "Foo"));
    completion.additional_text_edits =
        Some(vec![text_edit(range(0, 0, 0, 0), "use crate::Foo;\n")]);

    editor
        .test_execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::SetCursor(3, 1))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::ApplyCompletion {
            item: Box::new(completion),
            commit_character: None,
        })
        .await
        .unwrap();

    assert_eq!(
        editor.test_buffer_contents(),
        "use crate::Foo;\nmod stuff;\nFoo\n"
    );
    assert_eq!(editor.test_cursor_position(), (3, 2));

    editor.test_execute_action(Action::Undo).await.unwrap();
    assert_eq!(editor.test_buffer_contents(), "mod stuff;\nfoo\n");
}

#[tokio::test]
async fn apply_completion_converts_utf16_main_and_additional_edits_on_crlf_text() {
    let (mut editor, _) = recording_editor(Buffer::new(None, "😀 use\r\n😀 old\r\n".to_string()));
    let mut completion = item("new");
    completion.text_edit = Some(text_edit(range(1, 3, 1, 6), "new"));
    completion.additional_text_edits = Some(vec![text_edit(range(0, 3, 0, 6), "mod")]);
    editor
        .test_execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::SetCursor(5, 1))
        .await
        .unwrap();

    editor
        .test_execute_action(Action::ApplyCompletion {
            item: Box::new(completion),
            commit_character: None,
        })
        .await
        .unwrap();

    assert_eq!(editor.test_buffer_contents(), "😀 mod\r\n😀 new\r\n");
}

#[tokio::test]
async fn invalid_and_overlapping_completion_edits_leave_the_buffer_unchanged() {
    for (main, additional, expected_error) in [
        (range(0, 1, 0, 2), None, "splits a UTF-16 character"),
        (
            range(0, 3, 0, 6),
            Some(text_edit(range(0, 4, 0, 6), "overlap")),
            "overlap",
        ),
    ] {
        let (mut editor, _) = recording_editor(Buffer::new(None, "😀 old".to_string()));
        let mut completion = item("new");
        completion.text_edit = Some(text_edit(main, "new"));
        completion.additional_text_edits = additional.map(|edit| vec![edit]);
        editor
            .test_execute_action(Action::EnterMode(Mode::Insert))
            .await
            .unwrap();

        editor
            .test_execute_action(Action::ApplyCompletion {
                item: Box::new(completion),
                commit_character: None,
            })
            .await
            .unwrap();

        assert_eq!(editor.test_buffer_contents(), "😀 old");
        assert!(editor
            .test_last_error()
            .is_some_and(|error| error.contains(expected_error)));
    }
}

#[tokio::test]
async fn apply_completion_selects_the_first_snippet_placeholder() {
    let (mut editor, _) = recording_editor(Buffer::new(None, "call".to_string()));
    let mut completion = item("println");
    completion.insert_text = Some("println!(\"${1:value}\");$0".to_string());
    completion.insert_text_format = Some(InsertTextFormat::Snippet);

    editor
        .test_execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::ApplyCompletion {
            item: Box::new(completion),
            commit_character: None,
        })
        .await
        .unwrap();

    assert_eq!(editor.test_buffer_contents(), "println!(\"value\");call");
    assert_eq!(editor.test_cursor_position(), (10, 0));
    let (cursor_x, cursor_y) = editor.test_render_cursor_position().unwrap();
    let selected_background = editor.test_render_cell_bg(cursor_x, cursor_y).unwrap();
    let following_background = editor.test_render_cell_bg(cursor_x + 5, cursor_y).unwrap();
    assert_ne!(selected_background, following_background);
    assert!(editor.test_is_insert());

    editor
        .test_execute_action(Action::InsertCharAtCursorPos('x'))
        .await
        .unwrap();
    assert_eq!(editor.test_buffer_contents(), "println!(\"x\");call");

    editor
        .test_execute_event(Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)))
        .await
        .unwrap();
    assert_eq!(editor.test_cursor_position(), (14, 0));
    assert!(editor.test_is_insert());
}

#[tokio::test]
async fn snippet_arguments_support_replacement_forward_and_backward_navigation() {
    let (mut editor, _) = recording_editor(Buffer::new(None, String::new()));
    let mut completion = item("spawn_asteroid");
    completion.insert_text = Some("self.spawn_asteroid(${1:d}, ${2:pos}, ${3:size})$0".to_string());
    completion.insert_text_format = Some(InsertTextFormat::Snippet);

    editor
        .test_execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::ApplyCompletion {
            item: Box::new(completion),
            commit_character: None,
        })
        .await
        .unwrap();

    assert_eq!(
        editor.test_buffer_contents().trim_end_matches('\n'),
        "self.spawn_asteroid(d, pos, size)"
    );
    assert_eq!(editor.test_cursor_position(), (20, 0));

    editor.test_type_text("drawer").await.unwrap();
    assert_eq!(
        editor.test_buffer_contents().trim_end_matches('\n'),
        "self.spawn_asteroid(drawer, pos, size)"
    );

    editor
        .test_execute_event(Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)))
        .await
        .unwrap();
    assert_eq!(editor.test_cursor_position(), (28, 0));

    editor
        .test_execute_event(Event::Paste("origin_point".to_string()))
        .await
        .unwrap();
    assert_eq!(
        editor.test_buffer_contents().trim_end_matches('\n'),
        "self.spawn_asteroid(drawer, origin_point, size)"
    );

    editor
        .test_execute_event(Event::Key(KeyEvent::new(
            KeyCode::BackTab,
            KeyModifiers::SHIFT,
        )))
        .await
        .unwrap();
    assert_eq!(editor.test_cursor_position(), (20, 0));
    editor
        .test_execute_action(Action::InsertCharAtCursorPos('d'))
        .await
        .unwrap();

    editor
        .test_execute_event(Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)))
        .await
        .unwrap();
    assert_eq!(editor.test_cursor_position(), (23, 0));
    editor
        .test_execute_event(Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::InsertString("AsteroidSize::Large".to_string()))
        .await
        .unwrap();
    assert_eq!(
        editor.test_buffer_contents().trim_end_matches('\n'),
        "self.spawn_asteroid(d, origin_point, AsteroidSize::Large)"
    );

    editor
        .test_execute_event(Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)))
        .await
        .unwrap();
    assert_eq!(
        editor.test_cursor_position(),
        (
            editor
                .test_buffer_contents()
                .trim_end_matches('\n')
                .chars()
                .count(),
            0
        )
    );
    assert!(editor.test_is_insert());
}

#[tokio::test]
async fn snippet_placeholders_follow_unicode_and_additional_import_edits() {
    let (mut editor, _) = recording_editor(Buffer::new(None, "😀\nspawn".to_string()));
    let mut completion = item("spawn");
    completion.text_edit = Some(text_edit(range(1, 0, 1, 5), "spawn(${2:位置}, ${1:🚀})$0"));
    completion.additional_text_edits = Some(vec![text_edit(range(0, 0, 0, 0), "use game;\n")]);
    completion.insert_text_format = Some(InsertTextFormat::Snippet);

    editor
        .test_execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::ApplyCompletion {
            item: Box::new(completion),
            commit_character: None,
        })
        .await
        .unwrap();

    assert_eq!(
        editor.test_buffer_contents(),
        "use game;\n😀\nspawn(位置, 🚀)"
    );
    assert_eq!(editor.test_cursor_position(), (10, 2));
    editor
        .test_execute_action(Action::InsertCharAtCursorPos('x'))
        .await
        .unwrap();
    assert_eq!(
        editor.test_buffer_contents(),
        "use game;\n😀\nspawn(位置, x)"
    );

    editor
        .test_execute_event(Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)))
        .await
        .unwrap();
    assert_eq!(editor.test_cursor_position(), (6, 2));
    editor
        .test_execute_action(Action::InsertString("position".to_string()))
        .await
        .unwrap();
    assert_eq!(
        editor.test_buffer_contents(),
        "use game;\n😀\nspawn(position, x)"
    );
}

#[tokio::test]
async fn backspace_removes_selected_placeholder_and_escape_ends_navigation() {
    let (mut editor, _) = recording_editor(Buffer::new(None, String::new()));
    let mut completion = item("call");
    completion.insert_text = Some("call(${1:value}, ${2:next})$0".to_string());
    completion.insert_text_format = Some(InsertTextFormat::Snippet);

    editor
        .test_execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::ApplyCompletion {
            item: Box::new(completion),
            commit_character: None,
        })
        .await
        .unwrap();
    editor
        .test_execute_event(Event::Key(KeyEvent::new(
            KeyCode::Backspace,
            KeyModifiers::NONE,
        )))
        .await
        .unwrap();
    assert_eq!(
        editor.test_buffer_contents().trim_end_matches('\n'),
        "call(, next)"
    );

    editor
        .test_execute_event(Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)))
        .await
        .unwrap();
    assert_eq!(editor.test_cursor_position(), (7, 0));
    editor
        .test_execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::InsertCharAtCursorPos('x'))
        .await
        .unwrap();
    assert!(editor.test_buffer_contents().contains("next"));
    assert!(editor.test_buffer_contents().contains('x'));
}

#[tokio::test]
async fn snippet_commit_character_follows_call_without_replacing_first_argument() {
    let (mut editor, _) = recording_editor(Buffer::new(None, String::new()));
    let mut completion = item("call");
    completion.insert_text = Some("call(${1:value})$0".to_string());
    completion.insert_text_format = Some(InsertTextFormat::Snippet);

    editor
        .test_execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::ApplyCompletion {
            item: Box::new(completion),
            commit_character: Some(';'),
        })
        .await
        .unwrap();

    assert_eq!(
        editor.test_buffer_contents().trim_end_matches('\n'),
        "call(value);"
    );
    assert_eq!(editor.test_cursor_position(), (5, 0));
    editor
        .test_execute_action(Action::InsertCharAtCursorPos('x'))
        .await
        .unwrap();
    assert_eq!(
        editor.test_buffer_contents().trim_end_matches('\n'),
        "call(x);"
    );
}

#[tokio::test]
async fn snippet_final_cursor_without_placeholders_does_not_capture_tab() {
    let (mut editor, _) = recording_editor(Buffer::new(None, String::new()));
    let mut completion = item("call");
    completion.insert_text = Some("call($0)".to_string());
    completion.insert_text_format = Some(InsertTextFormat::Snippet);

    editor
        .test_execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::ApplyCompletion {
            item: Box::new(completion),
            commit_character: None,
        })
        .await
        .unwrap();

    assert_eq!(
        editor.test_buffer_contents().trim_end_matches('\n'),
        "call()"
    );
    assert_eq!(editor.test_cursor_position(), (5, 0));
    let event = Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert!(!matches!(
        editor.test_handle_event(event).unwrap(),
        Some(KeyAction::Single(Action::NextSnippetPlaceholder))
    ));
    let before_tab = editor.test_buffer_contents();
    editor.test_execute_action(Action::InsertTab).await.unwrap();
    assert_ne!(editor.test_buffer_contents(), before_tab);
    assert!(editor.test_buffer_contents().starts_with("call("));
}

#[tokio::test]
async fn undoing_snippet_completion_clears_placeholder_navigation() {
    let (mut editor, _) = recording_editor(Buffer::new(None, "before".to_string()));
    let mut completion = item("call");
    completion.text_edit = Some(text_edit(range(0, 0, 0, 6), "call(${1:value})$0"));
    completion.insert_text_format = Some(InsertTextFormat::Snippet);

    editor
        .test_execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::ApplyCompletion {
            item: Box::new(completion),
            commit_character: None,
        })
        .await
        .unwrap();
    assert_eq!(editor.test_buffer_contents(), "call(value)");

    editor.test_execute_action(Action::Undo).await.unwrap();
    assert_eq!(editor.test_buffer_contents(), "before");
    editor
        .test_execute_action(Action::InsertCharAtCursorPos('x'))
        .await
        .unwrap();
    assert!(editor.test_buffer_contents().contains("before"));
}

#[tokio::test]
async fn an_open_completion_menu_accepts_before_snippet_navigation() {
    let (mut editor, _) = recording_editor(Buffer::new(None, "alpha\nseed".to_string()));
    let mut completion = item("call");
    completion.text_edit = Some(text_edit(
        range(1, 0, 1, 4),
        "call(${1:value}, ${2:next})$0",
    ));
    completion.insert_text_format = Some(InsertTextFormat::Snippet);

    editor
        .test_execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::ApplyCompletion {
            item: Box::new(completion),
            commit_character: None,
        })
        .await
        .unwrap();
    editor
        .test_execute_action(Action::InsertCharAtCursorPos('a'))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::RequestCompletion)
        .await
        .unwrap();

    editor
        .test_execute_event(Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)))
        .await
        .unwrap();
    assert_eq!(editor.test_buffer_contents(), "alpha\ncall(alpha, next)");

    editor
        .test_execute_event(Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)))
        .await
        .unwrap();
    assert_eq!(editor.test_cursor_position(), (12, 1));
}

#[tokio::test]
async fn moving_outside_snippet_cancels_selected_argument_replacement() {
    let (mut editor, _) = recording_editor(Buffer::new(None, "prefix seed".to_string()));
    let mut completion = item("call");
    completion.text_edit = Some(text_edit(range(0, 7, 0, 11), "call(${1:value})$0"));
    completion.insert_text_format = Some(InsertTextFormat::Snippet);

    editor
        .test_execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::ApplyCompletion {
            item: Box::new(completion),
            commit_character: None,
        })
        .await
        .unwrap();
    editor
        .test_execute_action(Action::SetCursor(0, 0))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::InsertCharAtCursorPos('x'))
        .await
        .unwrap();

    assert_eq!(editor.test_buffer_contents(), "xprefix call(value)");
}

#[tokio::test]
async fn apply_completion_runs_lsp_command_after_edits() {
    let (mut editor, events) = recording_editor(Buffer::new(None, "foo".to_string()));
    let mut completion = item("bar");
    completion.text_edit = Some(text_edit(range(0, 0, 0, 3), "bar"));
    completion.command = Some(Command {
        title: "organize imports".to_string(),
        command: "rust-analyzer.applySourceChange".to_string(),
        arguments: Some(vec![json!({ "id": 1 })]),
    });

    editor
        .test_execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::ApplyCompletion {
            item: Box::new(completion),
            commit_character: None,
        })
        .await
        .unwrap();

    assert!(
        recorded(&events).iter().any(|event| {
            matches!(
                event,
                LspEvent::SendRequest { method, params }
                    if method == "workspace/executeCommand"
                        && params["command"] == "rust-analyzer.applySourceChange"
            )
        }),
        "expected executeCommand request, got {:?}",
        recorded(&events)
    );
}
