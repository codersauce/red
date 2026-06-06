mod common;

use std::sync::{Arc, Mutex};

use common::{LspEvent, RecordingLsp};
use red::{
    buffer::Buffer,
    config::Config,
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
async fn apply_completion_strips_basic_snippet_markers() {
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
