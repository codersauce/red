mod common;

use common::{EditorHarness, LspEvent, MockLsp, RecordingLsp};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use red::{
    agent_tools::{
        EditorActionName, EditorOpenTarget, EditorPosition, EditorSelectionKind, EditorTextEdit,
        EditorToolCall, EditorToolRequest,
    },
    agent_workspace::ProposalWorkspace,
    buffer::Buffer,
    clipboard::MemoryClipboardProvider,
    color::Color,
    config::{Config, KeyAction},
    editor::{Action, Content, Editor, Mode, SearchDirection},
    lsp::LspClient,
    plugin::{
        PanelConfig, PanelRow, PanelRowKind, PanelSegment, PanelSide, TextPanelComposerConfig,
    },
    preferences::PreferencesStore,
    theme::{Style, Theme},
    undo::EditOrigin,
};
use std::{
    env, fs,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

static COMMAND_COMPLETION_CWD_LOCK: Mutex<()> = Mutex::new(());

#[tokio::test]
async fn agent_editor_tools_navigate_select_and_stage_unicode_edits_without_touching_disk() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first.rs");
    let second = root.path().join("second.rs");
    fs::write(&first, "disk first\n").unwrap();
    fs::write(&second, "a😀b\nsecond\n").unwrap();
    let buffer = Buffer::new(
        Some(first.to_string_lossy().into_owned()),
        "unsaved first\n".to_string(),
    );
    let mut harness = EditorHarness::with_buffer(buffer);
    let workspace = Arc::new(Mutex::new(ProposalWorkspace::new(root.path()).unwrap()));
    harness
        .editor
        .test_set_agent_workspace(Arc::clone(&workspace));

    let opened = harness
        .editor
        .test_run_agent_editor_tool(EditorToolRequest {
            session_id: "session-1".to_string(),
            call: EditorToolCall::OpenFile {
                path: "second.rs".to_string(),
                line: 0,
                character: 1,
                target: EditorOpenTarget::Current,
            },
        })
        .await
        .unwrap();
    assert_eq!(opened["file"], "second.rs");
    assert_eq!(opened["cursor"]["line"], 0);
    assert_eq!(opened["cursor"]["character"], 1);

    let selected = harness
        .editor
        .test_run_agent_editor_tool(EditorToolRequest {
            session_id: "session-1".to_string(),
            call: EditorToolCall::SelectText {
                path: "second.rs".to_string(),
                start: EditorPosition {
                    line: 0,
                    character: 1,
                },
                end: EditorPosition {
                    line: 0,
                    character: 3,
                },
                kind: EditorSelectionKind::Character,
            },
        })
        .await
        .unwrap();
    assert_eq!(selected["selection"]["kind"], "character");
    assert_eq!(selected["selection"]["text"], "😀");
    assert_eq!(selected["selection"]["start"]["character"], 1);
    assert_eq!(selected["selection"]["end"]["character"], 3);
    let revision = selected["revision"].as_u64().unwrap();

    let staged = harness
        .editor
        .test_run_agent_editor_tool(EditorToolRequest {
            session_id: "session-1".to_string(),
            call: EditorToolCall::ApplyEdits {
                path: "second.rs".to_string(),
                expected_revision: revision,
                edits: vec![EditorTextEdit {
                    start: EditorPosition {
                        line: 0,
                        character: 1,
                    },
                    end: EditorPosition {
                        line: 0,
                        character: 3,
                    },
                    new_text: "λ".to_string(),
                }],
            },
        })
        .await
        .unwrap();
    assert_eq!(staged["ok"], true);
    assert!(!staged["hunks"].as_array().unwrap().is_empty());
    assert_eq!(harness.buffer_contents(), "a😀b\nsecond\n");
    assert_eq!(fs::read_to_string(&second).unwrap(), "a😀b\nsecond\n");
    assert_eq!(
        workspace
            .lock()
            .unwrap()
            .read("session-1", &second, None, None)
            .unwrap(),
        "aλb\nsecond\n"
    );

    let moved = harness
        .editor
        .test_run_agent_editor_tool(EditorToolRequest {
            session_id: "session-1".to_string(),
            call: EditorToolCall::RunEditorAction {
                action: EditorActionName::PreviousBuffer,
            },
        })
        .await
        .unwrap();
    assert_eq!(moved["file"], "first.rs");
}

#[tokio::test]
async fn agent_editor_tools_reject_workspace_escape_and_stale_edits() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("main.rs");
    fs::write(&file, "original\n").unwrap();
    let buffer = Buffer::new(
        Some(file.to_string_lossy().into_owned()),
        "unsaved\n".to_string(),
    );
    let mut harness = EditorHarness::with_buffer(buffer);
    harness.editor.test_set_agent_workspace(Arc::new(Mutex::new(
        ProposalWorkspace::new(root.path()).unwrap(),
    )));

    let escaped = harness
        .editor
        .test_run_agent_editor_tool(EditorToolRequest {
            session_id: "session-1".to_string(),
            call: EditorToolCall::OpenFile {
                path: "../outside.rs".to_string(),
                line: 0,
                character: 0,
                target: EditorOpenTarget::Current,
            },
        })
        .await
        .unwrap_err();
    assert!(escaped.to_string().contains("outside workspace"));

    let stale = harness
        .editor
        .test_run_agent_editor_tool(EditorToolRequest {
            session_id: "session-1".to_string(),
            call: EditorToolCall::ApplyEdits {
                path: "main.rs".to_string(),
                expected_revision: 999,
                edits: vec![EditorTextEdit {
                    start: EditorPosition {
                        line: 0,
                        character: 0,
                    },
                    end: EditorPosition {
                        line: 0,
                        character: 7,
                    },
                    new_text: "changed".to_string(),
                }],
            },
        })
        .await
        .unwrap_err();
    assert!(stale.to_string().contains("revision is stale"));
    assert_eq!(harness.buffer_contents(), "unsaved\n");
    assert_eq!(fs::read_to_string(file).unwrap(), "original\n");

    let secret = root.path().join(".env");
    fs::write(&secret, "TOKEN=must-not-be-exposed\n").unwrap();
    let blocked = harness
        .editor
        .test_run_agent_editor_tool(EditorToolRequest {
            session_id: "session-1".to_string(),
            call: EditorToolCall::OpenFile {
                path: ".env".to_string(),
                line: 0,
                character: 0,
                target: EditorOpenTarget::Current,
            },
        })
        .await
        .unwrap_err();
    assert!(blocked.to_string().contains("sensitive file"));
}

#[tokio::test]
async fn agent_editor_navigation_preserves_a_focused_conversation_composer() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first.rs");
    let second = root.path().join("second.rs");
    fs::write(&first, "first\n").unwrap();
    fs::write(&second, "second\n").unwrap();
    let buffer = Buffer::new(
        Some(first.to_string_lossy().into_owned()),
        "first\n".to_string(),
    );
    let mut harness = EditorHarness::with_buffer(buffer);
    harness.editor.test_set_agent_workspace(Arc::new(Mutex::new(
        ProposalWorkspace::new(root.path()).unwrap(),
    )));
    harness.editor.test_create_text_panel(
        "agent",
        PanelConfig {
            side: PanelSide::Right,
            width: 30,
            title: Some("Agent".to_string()),
            composer: Some(TextPanelComposerConfig {
                placeholder: "Ask a follow-up".to_string(),
                rows: 2,
            }),
            ..PanelConfig::default()
        },
    );
    assert!(harness.editor.test_focus_text_panel_composer("agent"));

    let state = harness
        .editor
        .test_run_agent_editor_tool(EditorToolRequest {
            session_id: "session-1".to_string(),
            call: EditorToolCall::OpenFile {
                path: "second.rs".to_string(),
                line: 0,
                character: 0,
                target: EditorOpenTarget::Current,
            },
        })
        .await
        .unwrap();

    assert_eq!(state["file"], "second.rs");
    assert_eq!(harness.editor.test_focused_panel_id(), Some("agent"));
    assert!(harness.render_cursor_position().is_some());
}

fn temp_file_path(name: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir()
        .join(format!("red-{name}-{}-{nanos}.txt", std::process::id()))
        .to_string_lossy()
        .into_owned()
}

async fn type_normal_keys(harness: &mut EditorHarness, keys: &str) {
    for key in keys.chars() {
        harness
            .execute_event(Event::Key(KeyEvent::new(
                KeyCode::Char(key),
                KeyModifiers::NONE,
            )))
            .await
            .unwrap();
    }
}

fn default_key_config() -> Config {
    toml::from_str(include_str!("../default_config.toml")).unwrap()
}

#[tokio::test]
async fn dot_repeats_a_direct_change_at_the_current_cursor() {
    let buffer = Buffer::new(None, "abc\ndef".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    type_normal_keys(&mut harness, "xj.").await;

    harness.assert_buffer_contents("bc\nef");
}

#[tokio::test]
async fn dot_repeats_an_insert_session_as_one_semantic_change() {
    let buffer = Buffer::new(None, "one\ntwo".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    type_normal_keys(&mut harness, "iX").await;
    command_key(&mut harness, KeyCode::Esc).await;
    type_normal_keys(&mut harness, "j.").await;

    harness.assert_buffer_contents("Xone\nXtwo");
    assert!(harness.is_normal());
}

#[tokio::test]
async fn dot_repeats_inserted_and_replaced_literal_periods() {
    let buffer = Buffer::new(None, "one\ntwo".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    type_normal_keys(&mut harness, "i.foo").await;
    command_key(&mut harness, KeyCode::Esc).await;
    type_normal_keys(&mut harness, "j.").await;

    harness.assert_buffer_contents(".fooone\ntw.fooo");

    let buffer = Buffer::new(None, "ab\ncd".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    type_normal_keys(&mut harness, "r.j.").await;

    harness.assert_buffer_contents(".b\n.d");

    let buffer = Buffer::new(None, "a.b\nc.d".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    type_normal_keys(&mut harness, "df.j.").await;

    harness.assert_buffer_contents("b\nd");
}

#[tokio::test]
async fn dot_recomputes_operator_motion_at_the_new_location() {
    let buffer = Buffer::new(None, "alpha beta\ngamma delta".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    type_normal_keys(&mut harness, "dwj.").await;

    harness.assert_buffer_contents("beta\ndelta");
}

#[tokio::test]
async fn count_before_dot_replays_the_completed_change_multiple_times() {
    let buffer = Buffer::new(None, "abcdef".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    type_normal_keys(&mut harness, "x2.").await;

    harness.assert_buffer_contents("def");
}

#[tokio::test]
async fn failed_change_does_not_replace_the_last_repeatable_change() {
    let buffer = Buffer::new(None, "a\n\nbc".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    type_normal_keys(&mut harness, "xjxj.").await;

    harness.assert_buffer_contents("\n\nc");
}

#[tokio::test]
async fn dot_covers_text_objects_replace_indent_and_open_line_changes() {
    let buffer = Buffer::new(None, "alpha beta\ngamma delta".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    type_normal_keys(&mut harness, "diwj.").await;
    harness.assert_buffer_contents(" beta\n delta");

    let buffer = Buffer::new(None, "ab\ncd".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    type_normal_keys(&mut harness, "rXj.").await;
    harness.assert_buffer_contents("Xb\nXd");

    let buffer = Buffer::new(None, "one\ntwo".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    type_normal_keys(&mut harness, ">>j.").await;
    harness.assert_buffer_contents("    one\n    two");

    let buffer = Buffer::new(None, "one\ntwo".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    type_normal_keys(&mut harness, "oX").await;
    command_key(&mut harness, KeyCode::Esc).await;
    type_normal_keys(&mut harness, "j.").await;
    harness.assert_buffer_contents("one\nX\ntwo\nX");
}

#[tokio::test]
async fn counted_replace_and_dot_recompute_at_the_new_cursor() {
    let buffer = Buffer::new(None, "abcd\nefgh".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    type_normal_keys(&mut harness, "3rXj.").await;

    harness.assert_buffer_contents("XXXd\nXXXh");
}

#[tokio::test]
async fn dot_repeats_linewise_paste_and_visual_block_insert() {
    let buffer = Buffer::new(None, "one\ntwo\nthree\nfour".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    type_normal_keys(&mut harness, "yyjpj.").await;
    harness.assert_buffer_contents("one\ntwo\none\none\nthree\nfour");

    let buffer = Buffer::new(None, "a\nb\nc\nd".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    harness
        .execute_event(Event::Key(KeyEvent::new(
            KeyCode::Char('v'),
            KeyModifiers::CONTROL,
        )))
        .await
        .unwrap();
    type_normal_keys(&mut harness, "jIX").await;
    command_key(&mut harness, KeyCode::Esc).await;
    type_normal_keys(&mut harness, "j.").await;
    harness.assert_buffer_contents("Xa\nXb\nXc\nXd");
}

#[tokio::test]
async fn macro_records_and_replays_normal_insert_and_motion_events() {
    let buffer = Buffer::new(None, "one\ntwo\nthree".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    type_normal_keys(&mut harness, "qaiX").await;
    command_key(&mut harness, KeyCode::Esc).await;
    type_normal_keys(&mut harness, "jq@a@@").await;

    harness.assert_buffer_contents("Xone\nXtwo\nXthree");
}

#[tokio::test]
async fn macro_records_literal_q_input_before_the_normal_mode_stop() {
    let buffer = Buffer::new(None, "one\ntwo".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    type_normal_keys(&mut harness, "qaiq").await;
    command_key(&mut harness, KeyCode::Esc).await;
    type_normal_keys(&mut harness, "jq@a").await;

    harness.assert_buffer_contents("qone\nqtwo");

    let buffer = Buffer::new(None, "ab\ncd".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    type_normal_keys(&mut harness, "qarqjq@a").await;

    harness.assert_buffer_contents("qb\nqd");
}

#[tokio::test]
async fn counted_macro_playback_runs_the_register_repeatedly() {
    let buffer = Buffer::new(None, "abc\ndef\nghi".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    type_normal_keys(&mut harness, "qaxjq2@a").await;

    harness.assert_buffer_contents("bc\nef\nhi");
}

#[tokio::test]
async fn macro_register_notation_can_be_inspected_and_edited() {
    let buffer = Buffer::new(None, "one\ntwo".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    harness
        .execute_action(Action::SetMacroRegister {
            register: 'a',
            keys: "i!<Esc>j".to_string(),
        })
        .await
        .unwrap();
    type_normal_keys(&mut harness, "@a@a").await;
    harness
        .execute_action(Action::PrintRegisters)
        .await
        .unwrap();

    harness.assert_buffer_contents("!one\n!two");
    assert!(harness
        .last_error()
        .is_some_and(|message| message.contains("a: i!<Esc>j")));
}

#[tokio::test]
async fn recursive_macro_stops_at_the_deterministic_depth_limit() {
    let buffer = Buffer::new(None, "text".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    harness
        .execute_action(Action::SetMacroRegister {
            register: 'a',
            keys: "@a".to_string(),
        })
        .await
        .unwrap();

    type_normal_keys(&mut harness, "@a").await;

    assert!(harness
        .last_error()
        .is_some_and(|message| message.contains("macro recursion limit")));
    harness.assert_buffer_contents("text");
}

#[tokio::test]
async fn named_mark_tracks_insertions_with_right_affinity_and_undo_redo() {
    let buffer = Buffer::new(None, "alpha\nbeta".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    type_normal_keys(&mut harness, "maiX").await;
    command_key(&mut harness, KeyCode::Esc).await;
    type_normal_keys(&mut harness, "`a").await;
    harness.assert_cursor_at(1, 0);

    type_normal_keys(&mut harness, "u`a").await;
    harness.assert_cursor_at(0, 0);

    harness.execute_action(Action::Redo).await.unwrap();
    type_normal_keys(&mut harness, "`a").await;
    harness.assert_cursor_at(1, 0);
}

#[tokio::test]
async fn mark_jumps_participate_in_the_jumplist_and_support_linewise_motion() {
    let buffer = Buffer::new(None, "  alpha\nbeta\ngamma".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    type_normal_keys(&mut harness, "maG'a").await;
    harness.assert_cursor_at(2, 0);

    type_normal_keys(&mut harness, "''").await;
    harness.assert_cursor_at(0, 2);
}

#[tokio::test]
async fn last_change_and_last_visual_marks_are_available() {
    let buffer = Buffer::new(None, "alpha\nbeta".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    type_normal_keys(&mut harness, "xG`.").await;
    harness.assert_cursor_at(0, 0);

    type_normal_keys(&mut harness, "vl").await;
    command_key(&mut harness, KeyCode::Esc).await;
    type_normal_keys(&mut harness, "G`<").await;
    harness.assert_cursor_at(0, 0);
    type_normal_keys(&mut harness, "`>").await;
    harness.assert_cursor_at(1, 0);
}

#[tokio::test]
async fn global_mark_reopens_a_closed_file_buffer() {
    let marked_path = temp_file_path("global-mark");
    let other_path = temp_file_path("global-mark-other");
    fs::write(&marked_path, "alpha\nbeta").unwrap();
    fs::write(&other_path, "other").unwrap();
    let buffer = Buffer::new(Some(marked_path.clone()), "alpha\nbeta".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    type_normal_keys(&mut harness, "jmA").await;
    harness
        .execute_action(Action::OpenFile(other_path.clone()))
        .await
        .unwrap();
    harness
        .execute_action(Action::OpenFile(marked_path.clone()))
        .await
        .unwrap();
    harness
        .execute_action(Action::DeleteBuffer(/*force*/ true))
        .await
        .unwrap();
    type_normal_keys(&mut harness, "`A").await;

    harness.assert_buffer_contents("alpha\nbeta");
    harness.assert_cursor_at(0, 1);
    fs::remove_file(marked_path).unwrap();
    fs::remove_file(other_path).unwrap();
}

#[tokio::test]
async fn mark_tracks_a_visual_block_multi_edit_transaction() {
    let mut harness = EditorHarness::with_content("a\nb\nc");
    harness.execute_action(Action::MoveDown).await.unwrap();
    harness.execute_action(Action::SetMark('a')).await.unwrap();
    harness.execute_action(Action::MoveUp).await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::VisualBlock))
        .await
        .unwrap();
    harness.execute_action(Action::MoveDown).await.unwrap();
    harness.execute_action(Action::InsertBlock).await.unwrap();
    harness
        .execute_action(Action::InsertCharAtCursorPos('X'))
        .await
        .unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();

    type_normal_keys(&mut harness, "`a").await;
    harness.assert_cursor_at(1, 1);
    harness.execute_action(Action::Undo).await.unwrap();
    type_normal_keys(&mut harness, "`a").await;
    harness.assert_cursor_at(0, 1);
    harness.execute_action(Action::Redo).await.unwrap();
    type_normal_keys(&mut harness, "`a").await;
    harness.assert_cursor_at(1, 1);
}

#[tokio::test]
async fn substitute_supports_current_whole_numeric_and_visual_ranges() {
    let mut harness = EditorHarness::with_content("foo foo\nFoo foo\nfoo foo");
    harness
        .execute_action(Action::Command("s/foo/one/".to_string()))
        .await
        .unwrap();
    harness.assert_buffer_contents("one foo\nFoo foo\nfoo foo");

    harness
        .execute_action(Action::Command("2,3s/foo/two/gi".to_string()))
        .await
        .unwrap();
    harness.assert_buffer_contents("one foo\ntwo two\ntwo two");

    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("one foo\nFoo foo\nfoo foo");

    harness
        .execute_action(Action::EnterMode(Mode::VisualLine))
        .await
        .unwrap();
    harness.execute_action(Action::MoveDown).await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
    harness
        .execute_action(Action::Command("'<,'>s/o/O/g".to_string()))
        .await
        .unwrap();
    harness.assert_buffer_contents("One fOO\nFOO fOO\nfoo foo");

    harness
        .execute_action(Action::Command("%s/foo/end/g".to_string()))
        .await
        .unwrap();
    harness.assert_buffer_contents("One fOO\nFOO fOO\nend end");
}

#[tokio::test]
async fn confirmed_substitute_tracks_each_match_and_is_one_undo_transaction() {
    let mut harness = EditorHarness::with_content("foo foo\nalpha beta\nfoo gamma");
    harness
        .execute_action(Action::Command("%s/foo/bar/gc".to_string()))
        .await
        .unwrap();
    harness.assert_buffer_contents("foo foo\nalpha beta\nfoo gamma");
    harness.assert_cursor_at(0, 0);
    let first_match = harness.render_cursor_position().unwrap();

    type_normal_keys(&mut harness, "y").await;
    harness.assert_cursor_at(4, 0);
    assert_eq!(
        harness.render_cursor_position(),
        Some((first_match.0 + 4, first_match.1))
    );

    type_normal_keys(&mut harness, "n").await;
    harness.assert_cursor_at(0, 2);
    assert_eq!(
        harness.render_cursor_position(),
        Some((first_match.0, first_match.1 + 2))
    );

    type_normal_keys(&mut harness, "a").await;
    harness.assert_buffer_contents("bar foo\nalpha beta\nbar gamma");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("foo foo\nalpha beta\nfoo gamma");
}

#[tokio::test]
async fn confirmed_substitute_scrolls_to_an_offscreen_match() {
    let content = (0..10)
        .map(|line| {
            if matches!(line, 0 | 9) {
                "foo".to_string()
            } else {
                format!("line {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let buffer = Buffer::new(None, content);
    let mut harness = EditorHarness::with_config_and_size(
        buffer,
        Config::default(),
        /*width*/ 40,
        /*height*/ 5,
    );
    harness
        .execute_action(Action::Command("%s/foo/bar/gc".to_string()))
        .await
        .unwrap();
    let first_match = harness.render_cursor_position().unwrap();

    type_normal_keys(&mut harness, "y").await;

    assert_eq!(harness.buffer_line(), 9);
    assert_eq!(harness.viewport_top(), 9);
    assert_eq!(harness.render_cursor_position(), Some(first_match));
}

#[tokio::test]
async fn substitute_uses_rust_regex_captures_and_escaped_delimiters() {
    let mut harness = EditorHarness::with_content("path/a-12 path/b-34");
    harness
        .execute_action(Action::Command(
            r"s/path\/([a-z])-(\d+)/$1:$2/g".to_string(),
        ))
        .await
        .unwrap();

    harness.assert_buffer_contents("a:12 b:34");
}

#[tokio::test]
async fn substitute_does_not_match_the_carriage_return_in_crlf_buffers() {
    let buffer = Buffer::new(None, "abc\r\ndef\r\n".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    harness
        .execute_action(Action::Command("%s/.$/X/".to_string()))
        .await
        .unwrap();

    harness.assert_buffer_contents("abX\r\ndeX\r\n");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("abc\r\ndef\r\n");
}

#[tokio::test]
async fn agent_proposal_stays_out_of_buffer_and_disk_until_attributed_acceptance() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("proposal.txt");
    fs::write(&path, "disk\n").unwrap();
    let buffer = Buffer::new(
        Some(path.to_string_lossy().into_owned()),
        "unsaved\n".to_string(),
    );
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    let mut workspace = ProposalWorkspace::new(temp.path()).unwrap();
    workspace
        .sync_visible_file(&path, /*revision*/ 0, "unsaved\n".to_string())
        .unwrap();
    workspace.begin_turn("session-1", "turn-1".to_string());
    workspace
        .write("session-1", &path, "agent\n".to_string())
        .unwrap();
    harness
        .editor
        .test_set_agent_workspace(Arc::new(Mutex::new(workspace)));

    let proposals = harness
        .editor
        .test_agent_proposals_payload("session-1")
        .unwrap();
    assert!(!proposals["files"][0]["hunks"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(harness.editor.test_agent_gutter_sign(/*line*/ 0), Some("A"));

    harness.execute_action(Action::Save).await.unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "unsaved\n");
    harness.assert_buffer_contents("unsaved\n");

    harness
        .editor
        .test_accept_agent_proposal("session-1", &path, /*hunk_id*/ None)
        .await
        .unwrap();
    harness.assert_buffer_contents("agent\n");
    assert_eq!(fs::read_to_string(&path).unwrap(), "unsaved\n");
    assert_eq!(
        harness.editor.test_last_transaction_origin(),
        Some(&EditOrigin::Agent {
            session_id: "session-1".to_string(),
            turn_id: "turn-1".to_string(),
        })
    );

    harness.execute_action(Action::Save).await.unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), "agent\n");
}

#[cfg(unix)]
#[tokio::test]
async fn accepting_an_unopened_existing_file_seeds_the_disk_base_for_undo() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("unopened.txt");
    fs::write(&path, "disk base\n").unwrap();
    let mut workspace = ProposalWorkspace::new(temp.path()).unwrap();
    workspace.begin_turn("session-1", "turn-1".to_string());
    workspace
        .write("session-1", &path, "agent replacement\n".to_string())
        .unwrap();
    let mut harness = EditorHarness::with_content("scratch");
    harness
        .editor
        .test_set_agent_workspace(Arc::new(Mutex::new(workspace)));

    harness
        .editor
        .test_accept_agent_proposal("session-1", &path, /*hunk_id*/ None)
        .await
        .unwrap();

    harness.assert_buffer_contents("agent replacement\n");
    assert_eq!(fs::read_to_string(&path).unwrap(), "disk base\n");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("disk base\n");
    assert_eq!(fs::read_to_string(path).unwrap(), "disk base\n");
}

#[cfg(unix)]
#[tokio::test]
async fn accepting_an_unopened_proposal_keeps_it_pending_when_lsp_open_fails() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("unopened.txt");
    fs::write(&path, "disk base\n").unwrap();
    let mut workspace = ProposalWorkspace::new(temp.path()).unwrap();
    workspace.begin_turn("session-1", "turn-1".to_string());
    workspace
        .write("session-1", &path, "agent replacement\n".to_string())
        .unwrap();
    let workspace = Arc::new(Mutex::new(workspace));
    let lsp = RecordingLsp::failing_next_did_open();
    let events = lsp.events();
    let mut editor = Editor::with_size(
        Box::new(lsp),
        /*width*/ 80,
        /*height*/ 24,
        Config::default(),
        Theme::default(),
        vec![Buffer::new(None, "scratch".to_string())],
    )
    .unwrap();
    editor.test_disable_terminal_output();
    editor.test_set_agent_workspace(Arc::clone(&workspace));
    let mut harness = EditorHarness { editor };

    let error = harness
        .editor
        .test_accept_agent_proposal("session-1", &path, /*hunk_id*/ None)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("injected didOpen failure"));
    assert_eq!(
        workspace.lock().unwrap().pending_files("session-1"),
        std::slice::from_ref(&path)
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "disk base\n");
    harness.assert_buffer_contents("disk base\n");

    harness
        .editor
        .test_accept_agent_proposal("session-1", &path, /*hunk_id*/ None)
        .await
        .unwrap();

    assert!(workspace
        .lock()
        .unwrap()
        .pending_files("session-1")
        .is_empty());
    harness.assert_buffer_contents("agent replacement\n");
    assert_eq!(fs::read_to_string(&path).unwrap(), "disk base\n");
    let opened_path = path.to_string_lossy().into_owned();
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| matches!(event, LspEvent::DidOpen(file) if file == &opened_path))
            .count(),
        2
    );
}

#[tokio::test]
async fn format_on_save_restores_save_as_identity_and_insert_transaction_after_sync_failure() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.rs");
    let target = temp.path().join("target.py");
    fs::write(&source, "disk source\n").unwrap();
    let source_file = source.to_string_lossy().into_owned();
    let target_file = target.to_string_lossy().into_owned();
    let lsp = RecordingLsp::failing_next_did_open();
    let events = lsp.events();
    let mut config = Config::default();
    config.lsp.format_on_save = true;
    let mut editor = Editor::with_size(
        Box::new(lsp),
        /*width*/ 80,
        /*height*/ 24,
        config,
        Theme::default(),
        vec![Buffer::new(
            Some(source_file.clone()),
            "unsaved source\n".to_string(),
        )],
    )
    .unwrap();
    editor.test_disable_terminal_output();
    editor
        .test_execute_production_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    assert!(editor
        .test_current_buffer()
        .undo_history
        .is_transaction_active());

    let error = editor
        .test_execute_production_action(Action::SaveAs(target_file.clone()))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("injected didOpen failure"));
    assert!(editor.test_is_insert());
    assert!(editor
        .test_current_buffer()
        .undo_history
        .is_transaction_active());
    assert_eq!(
        editor.test_current_buffer().file.as_deref(),
        Some(source_file.as_str())
    );
    assert_eq!(editor.test_current_buffer().contents(), "unsaved source\n");
    assert_eq!(fs::read_to_string(&source).unwrap(), "disk source\n");
    assert!(!target.exists());
    let events = events.lock().unwrap();
    assert!(events
        .iter()
        .any(|event| matches!(event, LspEvent::DidOpen(file) if file == &target_file)));
    assert!(events
        .iter()
        .any(|event| matches!(event, LspEvent::DidOpen(file) if file == &source_file)));
}

#[cfg(unix)]
#[tokio::test]
async fn accepting_an_unopened_proposal_commits_before_a_failed_change_notification() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("unopened.txt");
    fs::write(&path, "disk base\n").unwrap();
    let mut workspace = ProposalWorkspace::new(temp.path()).unwrap();
    workspace.begin_turn("session-1", "turn-1".to_string());
    workspace
        .write("session-1", &path, "agent replacement\n".to_string())
        .unwrap();
    let workspace = Arc::new(Mutex::new(workspace));
    let mut editor = Editor::with_size(
        Box::new(RecordingLsp::failing_next_did_change()),
        /*width*/ 80,
        /*height*/ 24,
        Config::default(),
        Theme::default(),
        vec![Buffer::new(None, "scratch".to_string())],
    )
    .unwrap();
    editor.test_disable_terminal_output();
    editor.test_set_agent_workspace(Arc::clone(&workspace));
    let mut harness = EditorHarness { editor };

    harness
        .editor
        .test_accept_agent_proposal("session-1", &path, /*hunk_id*/ None)
        .await
        .unwrap();

    harness.assert_buffer_contents("agent replacement\n");
    assert!(workspace
        .lock()
        .unwrap()
        .pending_files("session-1")
        .is_empty());
    assert!(harness
        .last_error()
        .is_some_and(|error| error.contains("change notification failed")));
    assert_eq!(fs::read_to_string(path).unwrap(), "disk base\n");
}

#[cfg(unix)]
#[tokio::test]
async fn unopened_proposal_review_accept_and_reject_refuse_unsafe_disk_sources() {
    use nix::{sys::stat::Mode, unistd::mkfifo};

    for source in ["symlink", "fifo", "oversized"] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("proposal.txt");
        fs::write(&path, "disk base\n").unwrap();
        let mut workspace = ProposalWorkspace::new(temp.path()).unwrap();
        workspace
            .write("session-1", &path, "agent replacement\n".to_string())
            .unwrap();
        fs::remove_file(&path).unwrap();
        match source {
            "symlink" => {
                let outside = temp.path().join("outside.txt");
                fs::write(&outside, "outside secret\n").unwrap();
                std::os::unix::fs::symlink(outside, &path).unwrap();
            }
            "fifo" => mkfifo(&path, Mode::S_IRUSR | Mode::S_IWUSR).unwrap(),
            "oversized" => fs::write(&path, "x".repeat(1024 * 1024)).unwrap(),
            _ => unreachable!(),
        }
        let mut harness = EditorHarness::with_content("scratch");
        harness
            .editor
            .test_set_agent_workspace(Arc::new(Mutex::new(workspace)));

        let proposals = harness
            .editor
            .test_agent_proposals_payload("session-1")
            .unwrap();
        assert_eq!(proposals["files"][0]["conflict"], true);
        assert!(proposals["files"][0]["message"]
            .as_str()
            .unwrap()
            .contains("Unable to review this agent proposal safely"));
        assert!(harness
            .editor
            .test_accept_agent_proposal("session-1", &path, /*hunk_id*/ None)
            .await
            .is_err());
        assert!(harness
            .editor
            .test_reject_agent_proposal("session-1", &path, /*hunk_id*/ None)
            .is_err());
        harness.assert_buffer_contents("scratch");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn unsafe_open_buffer_does_not_block_an_unrelated_agent_proposal() {
    let temp = tempfile::tempdir().unwrap();
    let safe = temp.path().join("safe.txt");
    let linked = temp.path().join("linked.txt");
    let outside = tempfile::NamedTempFile::new().unwrap();
    fs::write(&safe, "safe base\n").unwrap();
    fs::write(outside.path(), "outside secret\n").unwrap();
    std::os::unix::fs::symlink(outside.path(), &linked).unwrap();
    let mut workspace = ProposalWorkspace::new(temp.path()).unwrap();
    workspace
        .write("session-1", &safe, "agent replacement\n".to_string())
        .unwrap();
    let buffer = Buffer::new(
        Some(linked.to_string_lossy().into_owned()),
        "outside secret\n".to_string(),
    );
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    harness
        .editor
        .test_set_agent_workspace(Arc::new(Mutex::new(workspace)));

    let proposals = harness
        .editor
        .test_agent_proposals_payload("session-1")
        .unwrap();

    assert_eq!(proposals["files"].as_array().unwrap().len(), 1);
    assert_eq!(
        proposals["files"][0]["path"].as_str(),
        Some(safe.to_string_lossy().as_ref())
    );
}

#[cfg(unix)]
#[tokio::test]
async fn replaced_workspace_root_cannot_expose_an_outside_buffer_to_the_agent() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    let moved = temp.path().join("original-workspace");
    let outside = temp.path().join("outside");
    let source = root.join("source.txt");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(&source, "workspace base\n").unwrap();
    fs::write(outside.join("source.txt"), "outside secret\n").unwrap();
    let mut workspace = ProposalWorkspace::new(&root).unwrap();
    workspace
        .write("session-1", &source, "agent replacement\n".to_string())
        .unwrap();
    fs::rename(&root, &moved).unwrap();
    std::os::unix::fs::symlink(&outside, &root).unwrap();
    let buffer = Buffer::new(
        Some(source.to_string_lossy().into_owned()),
        "outside secret\n".to_string(),
    );
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    harness
        .editor
        .test_set_agent_workspace(Arc::new(Mutex::new(workspace)));

    let error = harness
        .editor
        .test_agent_proposals_payload("session-1")
        .unwrap_err()
        .to_string();

    assert!(error.contains("workspace root cannot be opened safely"));
}

#[cfg(unix)]
#[tokio::test]
async fn closing_a_buffer_removes_its_stale_agent_visible_contents() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("source.txt");
    fs::write(&path, "disk base\n").unwrap();
    let mut workspace = ProposalWorkspace::new(temp.path()).unwrap();
    workspace
        .sync_visible_file(&path, /*revision*/ 7, "stale unsaved\n".to_string())
        .unwrap();
    let workspace = Arc::new(Mutex::new(workspace));
    let mut harness = EditorHarness::with_content("scratch");
    harness
        .editor
        .test_set_agent_workspace(Arc::clone(&workspace));

    harness
        .editor
        .test_agent_proposals_payload("session-1")
        .unwrap();
    fs::write(&path, "fresh disk\n").unwrap();

    assert_eq!(
        workspace
            .lock()
            .unwrap()
            .read("session-2", &path, None, None)
            .unwrap(),
        "fresh disk\n"
    );
}

#[tokio::test]
async fn crash_session_restores_dirty_undo_and_pending_proposal_without_writing_disk() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("recovery.txt");
    fs::write(&path, "base\n").unwrap();
    let buffer = Buffer::new(
        Some(path.to_string_lossy().into_owned()),
        "base\n".to_string(),
    );
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    type_normal_keys(&mut harness, "iuser ").await;
    command_key(&mut harness, KeyCode::Esc).await;

    let mut workspace = ProposalWorkspace::new(temp.path()).unwrap();
    workspace
        .sync_visible_file(&path, /*revision*/ 1, "user base\n".to_string())
        .unwrap();
    workspace.begin_turn("session-1", "turn-1".to_string());
    workspace
        .write("session-1", &path, "agent base\n".to_string())
        .unwrap();
    harness
        .editor
        .test_set_agent_workspace(Arc::new(Mutex::new(workspace)));
    let snapshot = harness.editor.test_session_snapshot();

    fs::write(&path, "external\n").unwrap();
    let mut restored_buffers = Editor::buffers_from_session_snapshot(&snapshot);
    let mut restored = EditorHarness::with_config(restored_buffers.remove(0), default_key_config());
    let divergences = restored.editor.restore_session_snapshot(&snapshot).unwrap();

    restored.assert_buffer_contents("user base\n");
    assert!(restored.is_dirty());
    assert_eq!(divergences.len(), 1);
    assert!(divergences[0].diff.contains("external"));
    let archived = restored.editor.test_agent_proposals_payload("").unwrap();
    assert_eq!(archived["files"][0]["session_id"], "session-1");
    assert!(!archived["files"][0]["hunks"].as_array().unwrap().is_empty());
    let replacement = restored
        .editor
        .test_agent_proposals_payload("replacement-session")
        .unwrap();
    assert_eq!(replacement["files"][0]["session_id"], "replacement-session");
    assert!(!replacement["files"][0]["hunks"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(fs::read_to_string(&path).unwrap(), "external\n");

    restored.execute_action(Action::Undo).await.unwrap();
    restored.assert_buffer_contents("base\n");
    assert_eq!(fs::read_to_string(path).unwrap(), "external\n");
}

#[cfg(unix)]
#[tokio::test]
async fn crash_recovery_keeps_transcript_in_memory_when_preferences_are_unsafe() {
    let temp = tempfile::tempdir().unwrap();
    let outside = temp.path().join("outside-preferences.json");
    let preferences_path = temp.path().join("preferences.json");
    let recovered_path = temp.path().join("recovered.txt");
    fs::write(&outside, "outside secret").unwrap();
    fs::write(&recovered_path, "disk base\n").unwrap();
    std::os::unix::fs::symlink(&outside, &preferences_path).unwrap();
    let buffer = Buffer::new(
        Some(recovered_path.to_string_lossy().into_owned()),
        "recovered text\n".to_string(),
    );
    let mut source = EditorHarness::with_config(buffer, default_key_config());
    let mut snapshot = source.editor.test_session_snapshot();
    snapshot.agent_transcript = Some("You: recover me\nAgent: retained\n".to_string());
    let restored_buffers = Editor::buffers_from_session_snapshot(&snapshot);
    let preferences = PreferencesStore::load(&preferences_path);
    let mut editor = Editor::with_size_and_preferences(
        Box::new(MockLsp),
        /*width*/ 80,
        /*height*/ 24,
        default_key_config(),
        Theme::default(),
        restored_buffers,
        preferences,
    )
    .unwrap();
    editor.test_disable_terminal_output();

    fs::write(&recovered_path, "external change\n").unwrap();
    let divergences = editor.restore_session_snapshot(&snapshot).unwrap();

    let recovered = editor.test_session_snapshot();
    assert_eq!(divergences.len(), 1);
    assert_eq!(
        recovered.agent_transcript.as_deref(),
        Some("You: recover me\nAgent: retained\n")
    );
    assert_eq!(recovered.buffers[0].contents, "recovered text\n");
    assert_eq!(fs::read_to_string(outside).unwrap(), "outside secret");
    let restored = EditorHarness { editor };
    assert!(restored.last_error().is_some_and(|message| {
        message.contains("changed on disk") && message.contains("could not be persisted")
    }));
}

#[tokio::test]
async fn crash_session_finalizes_an_active_insert_transaction_in_the_snapshot() {
    let buffer = Buffer::new(None, "base\n".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    type_normal_keys(&mut harness, "iuser ").await;
    assert!(harness.is_insert());

    let snapshot = harness.editor.test_session_snapshot();
    let mut restored_buffers = Editor::buffers_from_session_snapshot(&snapshot);
    let mut restored = EditorHarness::with_config(restored_buffers.remove(0), default_key_config());

    restored.assert_buffer_contents("user base\n");
    restored.execute_action(Action::Undo).await.unwrap();
    restored.assert_buffer_contents("base\n");
    assert!(harness.is_insert());
}

#[tokio::test]
async fn unchanged_recovery_snapshots_are_skipped_and_failures_back_off() {
    let directory = tempfile::tempdir().unwrap();
    let store = red::session::SessionStore::for_owner(directory.path(), "editor-one").unwrap();
    let buffer = Buffer::new(None, "base\n".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    harness.editor.set_session_store(store.clone());

    harness
        .editor
        .test_persist_session_snapshot(/*force*/ true, /*due*/ true);
    let generation = store.load().unwrap().generation;
    harness
        .editor
        .test_persist_session_snapshot(/*force*/ false, /*due*/ true);
    assert_eq!(store.load().unwrap().generation, generation);

    let blocked_root = directory.path().join("not-a-directory");
    fs::write(&blocked_root, "blocked").unwrap();
    let blocked = red::session::SessionStore::for_owner(&blocked_root, "editor-two").unwrap();
    harness.editor.set_session_store(blocked);
    harness
        .editor
        .test_persist_session_snapshot(/*force*/ false, /*due*/ true);
    std::thread::sleep(std::time::Duration::from_millis(25));
    harness
        .editor
        .test_persist_session_snapshot(/*force*/ false, /*due*/ false);
    assert!(harness.editor.test_session_snapshot_is_backing_off());

    let warning = harness.commandline_row();
    assert!(warning.contains("Crash recovery is not being saved"));

    harness.editor.test_set_last_error("a newer LSP error");
    let status = harness.commandline_row();
    assert!(status.contains("a newer LSP error"));
    assert!(status.contains("Crash recovery is not being saved"));
    harness.editor.test_set_size(/*width*/ 8, /*height*/ 4);
    assert_eq!(harness.commandline_row(), "a newer ");

    harness
        .execute_action(Action::Command("1".to_string()))
        .await
        .unwrap();
    harness.editor.test_set_size(/*width*/ 120, /*height*/ 24);
    assert!(harness
        .commandline_row()
        .contains("Crash recovery is not being saved"));

    harness.editor.set_session_store(store);
    harness
        .editor
        .test_persist_session_snapshot(/*force*/ true, /*due*/ true);
    assert!(!harness
        .commandline_row()
        .contains("Crash recovery is not being saved"));
}

#[tokio::test]
async fn periodic_recovery_snapshot_materializes_shared_buffer_contents() {
    let directory = tempfile::tempdir().unwrap();
    let store = red::session::SessionStore::for_owner(directory.path(), "editor-one").unwrap();
    let expected = format!("prefix 👋 {}\n", "text ".repeat(64 * 1024));
    let buffer = Buffer::new(None, expected.clone());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    harness.editor.set_session_store(store.clone());

    harness
        .editor
        .test_persist_session_snapshot(/*force*/ false, /*due*/ true);
    harness.editor.test_finish_session_snapshot();

    assert_eq!(store.load().unwrap().buffers[0].contents, expected);
}

#[tokio::test]
async fn proposal_only_mutations_trigger_a_periodic_recovery_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let store = red::session::SessionStore::for_owner(directory.path(), "editor-one").unwrap();
    let path = directory.path().join("proposal.txt");
    fs::write(&path, "base\n").unwrap();
    let workspace = Arc::new(Mutex::new(
        ProposalWorkspace::new(directory.path()).unwrap(),
    ));
    let buffer = Buffer::new(None, "base\n".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    harness
        .editor
        .test_set_agent_workspace(Arc::clone(&workspace));
    harness.editor.set_session_store(store.clone());
    harness
        .editor
        .test_persist_session_snapshot(/*force*/ true, /*due*/ true);
    let generation = store.load().unwrap().generation;

    workspace
        .lock()
        .unwrap()
        .sync_visible_file(&path, /*revision*/ 0, "base\n".to_string())
        .unwrap();
    workspace
        .lock()
        .unwrap()
        .write("session-1", &path, "proposed\n".to_string())
        .unwrap();
    harness
        .editor
        .test_persist_session_snapshot(/*force*/ false, /*due*/ true);
    harness.editor.test_finish_session_snapshot();

    let snapshot = store.load().unwrap();
    assert_eq!(snapshot.generation, generation + 1);
    let restored = ProposalWorkspace::from_snapshot(snapshot.agent_workspace.unwrap());
    assert_eq!(restored.pending_files("session-1"), [path]);
}

fn tree_rows() -> Vec<PanelRow> {
    ["root", "src", "main.rs"]
        .into_iter()
        .map(|id| PanelRow {
            id: id.to_string(),
            path: Some(id.to_string()),
            expanded: Some(false),
            kind: if id.ends_with(".rs") {
                PanelRowKind::File
            } else {
                PanelRowKind::Directory
            },
            segments: vec![PanelSegment {
                text: id.to_string(),
                style: None,
                semantic: None,
            }],
            right_segments: vec![],
        })
        .collect()
}

fn add_tree_panel(harness: &mut EditorHarness) {
    harness.editor.test_create_panel(
        "tree",
        PanelConfig {
            side: PanelSide::Left,
            width: 20,
            title: None,
            composer: None,
            header_actions: Vec::new(),
        },
    );
    harness.editor.test_update_panel("tree", tree_rows());
}

async fn command_key(harness: &mut EditorHarness, code: KeyCode) {
    harness
        .execute_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
        .await
        .unwrap();
}

struct CurrentDirGuard {
    original: PathBuf,
    _lock: MutexGuard<'static, ()>,
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        env::set_current_dir(&self.original).unwrap();
    }
}

fn command_completion_temp_dir(name: &str) -> (PathBuf, CurrentDirGuard) {
    let lock = COMMAND_COMPLETION_CWD_LOCK.lock().unwrap();
    let original = env::current_dir().unwrap();
    let root = env::temp_dir().join(format!(
        "red-command-completion-{name}-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&root).unwrap();
    env::set_current_dir(&root).unwrap();
    (
        root,
        CurrentDirGuard {
            original,
            _lock: lock,
        },
    )
}

#[tokio::test]
async fn command_history_recalls_previous_commands_with_up_and_down() {
    let mut harness = EditorHarness::with_content("");
    harness
        .execute_action(Action::Command("alpha-one".to_string()))
        .await
        .unwrap();
    harness
        .execute_action(Action::Command("beta-two".to_string()))
        .await
        .unwrap();
    harness.set_commandline(Mode::Command, "");

    command_key(&mut harness, KeyCode::Up).await;
    assert_eq!(harness.commandline_text(), "beta-two");

    command_key(&mut harness, KeyCode::Up).await;
    assert_eq!(harness.commandline_text(), "alpha-one");

    command_key(&mut harness, KeyCode::Up).await;
    assert_eq!(harness.commandline_text(), "alpha-one");

    command_key(&mut harness, KeyCode::Down).await;
    assert_eq!(harness.commandline_text(), "beta-two");

    command_key(&mut harness, KeyCode::Down).await;
    assert_eq!(harness.commandline_text(), "");
}

#[tokio::test]
async fn command_history_filters_by_typed_prefix() {
    let mut harness = EditorHarness::with_content("");
    for command in ["buffer-next", "write", "buffer-delete"] {
        harness
            .execute_action(Action::Command(command.to_string()))
            .await
            .unwrap();
    }
    harness.set_commandline(Mode::Command, "b");

    command_key(&mut harness, KeyCode::Up).await;
    assert_eq!(harness.commandline_text(), "buffer-delete");

    command_key(&mut harness, KeyCode::Up).await;
    assert_eq!(harness.commandline_text(), "buffer-next");

    command_key(&mut harness, KeyCode::Down).await;
    assert_eq!(harness.commandline_text(), "buffer-delete");

    command_key(&mut harness, KeyCode::Down).await;
    assert_eq!(harness.commandline_text(), "b");
}

#[tokio::test]
async fn command_history_editing_recalled_command_resets_prefix_session() {
    let mut harness = EditorHarness::with_content("");
    harness
        .execute_action(Action::Command("buffer-delete".to_string()))
        .await
        .unwrap();
    harness.set_commandline(Mode::Command, "b");

    command_key(&mut harness, KeyCode::Up).await;
    assert_eq!(harness.commandline_text(), "buffer-delete");

    command_key(&mut harness, KeyCode::Char('x')).await;
    assert_eq!(harness.commandline_text(), "buffer-deletex");

    command_key(&mut harness, KeyCode::Up).await;
    assert_eq!(harness.commandline_text(), "buffer-deletex");
}

#[tokio::test]
async fn whitespace_only_commands_are_not_saved_to_history() {
    let mut harness = EditorHarness::with_content("");
    harness
        .execute_action(Action::Command("   ".to_string()))
        .await
        .unwrap();
    harness.set_commandline(Mode::Command, "");

    command_key(&mut harness, KeyCode::Up).await;

    assert_eq!(harness.commandline_text(), "");
}

#[tokio::test]
async fn edit_without_file_argument_reloads_current_file() {
    let path = temp_file_path("edit-reload");
    fs::write(&path, "one\ntwo\nthree\n").unwrap();
    let buffer = Buffer::new(Some(path.clone()), "one\ntwo\nthree\n".to_string());
    let mut harness = EditorHarness::with_buffer(buffer);
    harness.execute_action(Action::MoveDown).await.unwrap();
    fs::write(&path, "one\nchanged\nthree\n").unwrap();

    harness
        .execute_action(Action::Command("e".to_string()))
        .await
        .unwrap();

    assert_eq!(harness.buffer_contents(), "one\nchanged\nthree\n");
    assert_eq!(harness.cursor_position(), (0, 1));
    assert!(!harness.is_dirty());
    fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn edit_without_force_refuses_to_reload_dirty_current_file() {
    let path = temp_file_path("edit-reload-dirty");
    fs::write(&path, "one\ntwo\n").unwrap();
    let buffer = Buffer::new(Some(path.clone()), "one\ntwo\n".to_string());
    let mut harness = EditorHarness::with_buffer(buffer);
    harness
        .execute_action(Action::InsertCharAtCursorPos('x'))
        .await
        .unwrap();
    fs::write(&path, "one\nchanged\n").unwrap();

    harness
        .execute_action(Action::Command("e".to_string()))
        .await
        .unwrap();

    assert_eq!(harness.buffer_contents(), "xone\ntwo\n");
    assert_eq!(
        harness.last_error(),
        Some("E37: No write since last change (add ! to override)")
    );
    assert!(harness.is_dirty());
    fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn edit_with_force_reloads_dirty_current_file() {
    let path = temp_file_path("edit-reload-force");
    fs::write(&path, "one\ntwo\n").unwrap();
    let buffer = Buffer::new(Some(path.clone()), "one\ntwo\n".to_string());
    let mut harness = EditorHarness::with_buffer(buffer);
    harness.execute_action(Action::MoveDown).await.unwrap();
    harness
        .execute_action(Action::InsertCharAtCursorPos('x'))
        .await
        .unwrap();
    fs::write(&path, "one\nchanged\n").unwrap();

    harness
        .execute_action(Action::Command("e!".to_string()))
        .await
        .unwrap();

    assert_eq!(harness.buffer_contents(), "one\nchanged\n");
    assert_eq!(harness.cursor_position(), (1, 1));
    assert!(!harness.is_dirty());
    fs::remove_file(path).unwrap();
}

#[test]
fn command_tab_completes_edit_file_argument() {
    let (root, _guard) = command_completion_temp_dir("edit");
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("sample.txt"), "").unwrap();
    let mut harness = EditorHarness::with_content("");
    harness.set_commandline(Mode::Command, "e sr");

    harness.editor.test_complete_command_path_next();

    assert_eq!(harness.commandline_text(), "e src/");
    drop(_guard);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn command_tab_preserves_relative_path_prefix() {
    let (root, _guard) = command_completion_temp_dir("relative-prefix");
    fs::create_dir(root.join("src")).unwrap();
    let mut harness = EditorHarness::with_content("");
    harness.set_commandline(Mode::Command, "e ./sr");

    harness.editor.test_complete_command_path_next();

    assert_eq!(harness.commandline_text(), "e ./src/");
    drop(_guard);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn command_tab_completes_dot_to_current_directory_prefix() {
    let (root, _guard) = command_completion_temp_dir("dot");
    let mut harness = EditorHarness::with_content("");
    harness.set_commandline(Mode::Command, "e .");

    harness.editor.test_complete_command_path_next();

    assert_eq!(harness.commandline_text(), "e ./");
    drop(_guard);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn command_tab_cycles_file_matches_and_backtab_reverses() {
    let (root, _guard) = command_completion_temp_dir("cycle");
    fs::write(root.join("src_a.rs"), "").unwrap();
    fs::write(root.join("src_b.rs"), "").unwrap();
    let mut harness = EditorHarness::with_content("");
    harness.set_commandline(Mode::Command, "e src");

    harness.editor.test_complete_command_path_next();
    assert_eq!(harness.commandline_text(), "e src_a.rs");

    harness.editor.test_complete_command_path_next();
    assert_eq!(harness.commandline_text(), "e src_b.rs");

    harness.editor.test_complete_command_path_previous();
    assert_eq!(harness.commandline_text(), "e src_a.rs");
    drop(_guard);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn command_tab_sorts_directories_before_files() {
    let (root, _guard) = command_completion_temp_dir("directories-first");
    fs::create_dir(root.join("app")).unwrap();
    fs::write(root.join("alpha.txt"), "").unwrap();
    let mut harness = EditorHarness::with_content("");
    harness.set_commandline(Mode::Command, "e a");

    harness.editor.test_complete_command_path_next();

    assert_eq!(harness.commandline_text(), "e app/");
    drop(_guard);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn command_tab_completes_file_arguments_for_split_vsplit_and_write() {
    let (root, _guard) = command_completion_temp_dir("file-commands");
    fs::create_dir(root.join("target")).unwrap();

    for command in [
        "sp ta",
        "vs ta",
        "w ta",
        "write ta",
        "split ta",
        "vsplit ta",
    ] {
        let mut harness = EditorHarness::with_content("");
        harness.set_commandline(Mode::Command, command);

        harness.editor.test_complete_command_path_next();

        let command_name = command.split_once(' ').unwrap().0;
        assert_eq!(
            harness.commandline_text(),
            format!("{command_name} target/")
        );
    }
    drop(_guard);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn command_tab_ignores_non_file_commands() {
    let (root, _guard) = command_completion_temp_dir("non-file");
    fs::create_dir(root.join("src")).unwrap();
    let mut harness = EditorHarness::with_content("");
    harness.set_commandline(Mode::Command, "q sr");

    harness.editor.test_complete_command_path_next();

    assert_eq!(harness.commandline_text(), "q sr");
    drop(_guard);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn command_tab_key_event_completes_file_argument() {
    let root = env::temp_dir().join(format!(
        "red-command-completion-event-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(root.join("target")).unwrap();
    let mut harness = EditorHarness::with_content("");
    harness.set_commandline(Mode::Command, &format!("e {}/ta", root.display()));

    command_key(&mut harness, KeyCode::Tab).await;

    assert_eq!(
        harness.commandline_text(),
        format!("e {}/target/", root.display())
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn wrap_commands_toggle_line_wrapping() {
    let mut harness = EditorHarness::with_content("short");
    assert!(harness.wrap());

    harness
        .execute_action(Action::Command("nowrap".to_string()))
        .await
        .unwrap();
    assert!(!harness.wrap());

    harness
        .execute_action(Action::Command("wrap".to_string()))
        .await
        .unwrap();
    assert!(harness.wrap());
}

#[tokio::test]
async fn submitted_commands_are_persisted_to_preferences() {
    let dir = std::env::temp_dir().join(format!("red-command-history-{}", uuid::Uuid::new_v4()));
    let path = dir.join("preferences.json");
    let lsp = Box::new(MockLsp) as Box<dyn LspClient>;
    let config = Config::default();
    let buffer = Buffer::new(None, String::new());
    let mut editor = Editor::with_size_and_preferences(
        lsp,
        80,
        24,
        config,
        Theme::default(),
        vec![buffer],
        PreferencesStore::load(&path),
    )
    .unwrap();
    editor.test_disable_terminal_output();

    editor
        .test_execute_production_action(Action::Command("persist-me".to_string()))
        .await
        .unwrap();

    let store = PreferencesStore::load(&path);
    assert_eq!(store.command_history(), ["persist-me"]);
    fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn test_insert_mode() {
    let mut harness = EditorHarness::with_content("Hello World");

    // Debug: Check initial cursor position and buffer state
    println!("Initial cursor position: {:?}", harness.cursor_position());
    println!("Number of lines: {}", harness.line_count());
    if let Some(line) = harness.line_contents(0) {
        println!("Line 0 content: {:?}", line);
    }

    // Enter insert mode with 'i'
    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.assert_mode(Mode::Insert);

    // Debug: Check cursor position after entering insert mode
    println!(
        "Cursor position after entering insert mode: {:?}",
        harness.cursor_position()
    );

    // Type some text
    harness.type_text("Hi ").await.unwrap();

    // Debug: Check actual buffer contents
    let contents = harness.buffer_contents();
    println!("Actual buffer contents: {:?}", contents);
    println!("Buffer length: {}", contents.len());
    println!("Ends with newline: {}", contents.ends_with('\n'));

    harness.assert_buffer_contents("Hi Hello World");

    // Exit insert mode (ESC)
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
    harness.assert_mode(Mode::Normal);
}

#[tokio::test]
async fn test_append_mode() {
    let mut harness = EditorHarness::with_content("Hello World");

    // Move cursor to 'o' in 'Hello' (position 4)
    for _ in 0..4 {
        harness.execute_action(Action::MoveRight).await.unwrap();
    }

    // Enter append mode with 'a' - should insert after current character
    harness.execute_action(Action::MoveRight).await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.assert_mode(Mode::Insert);

    // Type text
    harness.type_text(" there").await.unwrap();
    harness.assert_buffer_contents("Hello there World");

    // Exit insert mode
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
}

#[tokio::test]
async fn test_open_line_below() {
    let mut harness = EditorHarness::with_content("Line 1\nLine 2");

    // Open line below with 'o' - InsertLineBelowCursor
    harness
        .execute_action(Action::InsertLineBelowCursor)
        .await
        .unwrap();
    harness.assert_mode(Mode::Insert);

    // Should have created a new line and moved cursor there
    harness.assert_cursor_at(0, 1);

    // Type on the new line
    harness.type_text("New line").await.unwrap();
    harness.assert_buffer_contents("Line 1\nNew line\nLine 2");

    // Exit insert mode
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
}

#[tokio::test]
async fn test_enter_on_opened_indented_blank_line_preserves_indentation() {
    let mut harness = EditorHarness::with_content("fn name() {\n    let a = 1;\n}");

    harness.execute_action(Action::MoveDown).await.unwrap();
    harness
        .execute_action(Action::InsertLineBelowCursor)
        .await
        .unwrap();
    harness.assert_cursor_at(4, 2);

    harness.execute_action(Action::InsertNewLine).await.unwrap();

    harness.assert_cursor_at(4, 3);
    harness.assert_buffer_contents("fn name() {\n    let a = 1;\n    \n    \n}");
}

#[tokio::test]
async fn test_enter_on_existing_whitespace_only_line_preserves_indentation() {
    let mut harness = EditorHarness::with_content("    \nnext");

    harness
        .execute_action(Action::SetCursor(3, 0))
        .await
        .unwrap();
    harness.execute_action(Action::InsertNewLine).await.unwrap();

    harness.assert_cursor_at(4, 1);
    harness.assert_buffer_contents("   \n     \nnext");
}

#[tokio::test]
async fn test_open_line_above() {
    let mut harness = EditorHarness::with_content("Line 1\nLine 2");

    // Move to second line
    harness.execute_action(Action::MoveDown).await.unwrap();
    println!(
        "After MoveDown - cursor at: {:?}",
        harness.cursor_position()
    );

    // Open line above with 'O' - InsertLineAtCursor
    harness
        .execute_action(Action::InsertLineAtCursor)
        .await
        .unwrap();
    println!(
        "After InsertLineAtCursor - cursor at: {:?}",
        harness.cursor_position()
    );
    println!("Buffer contents: {:?}", harness.buffer_contents());
    harness.assert_mode(Mode::Insert);

    // Should have created a new line above and moved cursor there
    harness.assert_cursor_at(0, 1);

    // Type on the new line
    harness.type_text("Middle line").await.unwrap();
    harness.assert_buffer_contents("Line 1\nMiddle line\nLine 2");

    // Exit insert mode
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
}

#[tokio::test]
async fn test_delete_char() {
    let mut harness = EditorHarness::with_content("Hello World");

    // Delete character under cursor with 'x'
    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    harness.assert_buffer_contents("ello World");

    // Move to space and delete
    harness
        .execute_action(Action::MoveToNextWord)
        .await
        .unwrap();
    harness.execute_action(Action::MoveLeft).await.unwrap();
    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    harness.assert_buffer_contents("elloWorld");
}

#[tokio::test]
async fn test_delete_line() {
    let mut harness = EditorHarness::with_content("Line 1\nLine 2\nLine 3");

    // Move to second line
    harness.execute_action(Action::MoveDown).await.unwrap();

    // Delete line with 'dd'
    println!("Before delete: {:?}", harness.buffer_contents());
    println!("Cursor at: {:?}", harness.cursor_position());
    println!("Line under cursor: {:?}", harness.current_line());
    harness
        .execute_action(Action::DeleteCurrentLine)
        .await
        .unwrap();
    println!("After delete: {:?}", harness.buffer_contents());
    println!("Cursor at after: {:?}", harness.cursor_position());
    println!("Line under cursor after: {:?}", harness.current_line());
    harness.assert_buffer_contents("Line 1\nLine 3");

    // Cursor should be on what was line 3
    harness.assert_cursor_at(0, 1);
}

#[tokio::test]
async fn test_delete_to_end_of_line() {
    let mut harness = EditorHarness::with_config(
        Buffer::new(None, "Hello World Test".to_string()),
        default_key_config(),
    );

    type_normal_keys(&mut harness, "wD").await;

    harness.assert_buffer_contents("Hello ");
}

#[tokio::test]
async fn test_change_word() {
    let mut harness = EditorHarness::with_content("Hello World Test");

    // Change word with 'cw' - delete word then enter insert mode
    harness.execute_action(Action::DeleteWord).await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.assert_mode(Mode::Insert);

    // Type replacement
    harness.type_text("Hi ").await.unwrap();
    harness.assert_buffer_contents("Hi World Test");

    // Exit insert mode
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
}

#[tokio::test]
async fn visual_change_replaces_selection_and_undoes_as_one_transaction() {
    let clipboard_text = Arc::new(Mutex::new(None));
    let buffer = Buffer::new(None, "alpha beta".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    harness
        .editor
        .test_set_clipboard(Box::new(MemoryClipboardProvider::from(
            clipboard_text.clone(),
        )));

    type_normal_keys(&mut harness, "vwc").await;

    harness.assert_mode(Mode::Insert);
    harness.assert_buffer_contents("eta");
    assert_eq!(clipboard_text.lock().unwrap().as_deref(), Some("alpha b"));

    type_normal_keys(&mut harness, "REPLACED").await;
    command_key(&mut harness, KeyCode::Esc).await;

    harness.assert_mode(Mode::Normal);
    harness.assert_buffer_contents("REPLACEDeta");

    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("alpha beta");
    harness.execute_action(Action::Redo).await.unwrap();
    harness.assert_buffer_contents("REPLACEDeta");
}

#[tokio::test]
async fn visual_line_change_leaves_one_replacement_line() {
    let buffer = Buffer::new(None, "one\ntwo\nthree".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    type_normal_keys(&mut harness, "Vjc").await;

    harness.assert_mode(Mode::Insert);
    harness.assert_buffer_contents("\nthree");
    type_normal_keys(&mut harness, "X").await;
    command_key(&mut harness, KeyCode::Esc).await;

    harness.assert_buffer_contents("X\nthree");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("one\ntwo\nthree");
}

#[tokio::test]
async fn visual_block_change_replaces_each_selected_row() {
    let buffer = Buffer::new(None, "abcd\nefgh\nijkl".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    harness
        .execute_event(Event::Key(KeyEvent::new(
            KeyCode::Char('v'),
            KeyModifiers::CONTROL,
        )))
        .await
        .unwrap();
    type_normal_keys(&mut harness, "jjlc").await;

    harness.assert_mode(Mode::Insert);
    harness.assert_buffer_contents("cd\ngh\nkl");
    type_normal_keys(&mut harness, "X").await;
    command_key(&mut harness, KeyCode::Esc).await;

    harness.assert_buffer_contents("Xcd\nXgh\nXkl");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("abcd\nefgh\nijkl");
    harness.execute_action(Action::Redo).await.unwrap();
    harness.assert_buffer_contents("Xcd\nXgh\nXkl");
}

#[tokio::test]
async fn visual_block_change_uses_buffer_rows_after_scrolling() {
    let content = (0..40)
        .map(|line| format!("abcd-{line:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let buffer = Buffer::new(None, content.clone());
    let mut harness = EditorHarness::with_config_and_size(buffer, default_key_config(), 80, 10);
    harness
        .execute_action(Action::SetCursor(0, 30))
        .await
        .unwrap();

    harness
        .execute_event(Event::Key(KeyEvent::new(
            KeyCode::Char('v'),
            KeyModifiers::CONTROL,
        )))
        .await
        .unwrap();
    type_normal_keys(&mut harness, "jlcX").await;
    command_key(&mut harness, KeyCode::Esc).await;

    assert_eq!(
        harness.line_contents(30).unwrap().trim_end_matches('\n'),
        "Xcd-30"
    );
    assert_eq!(
        harness.line_contents(31).unwrap().trim_end_matches('\n'),
        "Xcd-31"
    );
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents(&content);
}

#[tokio::test]
async fn visual_change_inserts_after_multicodepoint_graphemes() {
    let family = "👨‍👩‍👧‍👦";
    let buffer = Buffer::new(None, format!("{family} alpha beta"));
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    type_normal_keys(&mut harness, "ll").await;
    harness.assert_cursor_at(2, 0);
    type_normal_keys(&mut harness, "vwcX").await;
    command_key(&mut harness, KeyCode::Esc).await;

    harness.assert_buffer_contents(&format!("{family} Xeta"));
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents(&format!("{family} alpha beta"));
}

#[tokio::test]
async fn test_delete_inner_word_key_sequence() {
    let mut harness = EditorHarness::with_content("alpha beta gamma");
    harness
        .execute_action(Action::MoveToNextWord)
        .await
        .unwrap();

    type_normal_keys(&mut harness, "diw").await;

    harness.assert_buffer_contents("alpha  gamma");
    harness.assert_cursor_at(6, 0);
}

#[tokio::test]
async fn test_delete_inner_word_excludes_macro_bang_from_identifier() {
    let mut harness = EditorHarness::with_content("println!(\"hi\");");

    type_normal_keys(&mut harness, "diw").await;

    harness.assert_buffer_contents("!(\"hi\");");
    harness.assert_cursor_at(0, 0);
}

#[tokio::test]
async fn test_visual_inner_word_excludes_macro_bang_from_identifier() {
    let mut config = Config::default();
    config.keys.normal.insert(
        "v".to_string(),
        KeyAction::Single(Action::EnterMode(Mode::Visual)),
    );
    config.keys.visual.insert(
        "x".to_string(),
        KeyAction::Multiple(vec![Action::Delete, Action::EnterMode(Mode::Normal)]),
    );
    let buffer = Buffer::new(None, "println!(\"hi\");".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    type_normal_keys(&mut harness, "viwx").await;

    harness.assert_buffer_contents("!(\"hi\");");
    harness.assert_cursor_at(0, 0);
}

#[tokio::test]
async fn test_delete_around_word_key_sequence() {
    let mut harness = EditorHarness::with_content("alpha beta gamma");
    harness
        .execute_action(Action::MoveToNextWord)
        .await
        .unwrap();

    type_normal_keys(&mut harness, "daw").await;

    harness.assert_buffer_contents("alpha gamma");
    harness.assert_cursor_at(6, 0);
}

#[tokio::test]
async fn test_change_inner_word_key_sequence() {
    let mut harness = EditorHarness::with_content("alpha beta gamma");
    harness
        .execute_action(Action::MoveToNextWord)
        .await
        .unwrap();

    type_normal_keys(&mut harness, "ciw").await;

    harness.assert_mode(Mode::Insert);
    harness.type_text("BETA").await.unwrap();
    harness.assert_buffer_contents("alpha BETA gamma");
}

#[tokio::test]
async fn test_delete_inner_and_around_nested_parens() {
    let mut harness = EditorHarness::with_content("foo(bar(baz), qux)");
    for _ in 0..8 {
        harness.execute_action(Action::MoveRight).await.unwrap();
    }

    type_normal_keys(&mut harness, "di(").await;
    harness.assert_buffer_contents("foo(bar(), qux)");

    let mut harness = EditorHarness::with_content("foo(bar(baz), qux)");
    for _ in 0..8 {
        harness.execute_action(Action::MoveRight).await.unwrap();
    }

    type_normal_keys(&mut harness, "da(").await;
    harness.assert_buffer_contents("foo(bar, qux)");
}

#[tokio::test]
async fn test_delete_inner_multiline_braces() {
    let mut harness = EditorHarness::with_content("fn main() {\n    call(arg);\n}");
    harness.execute_action(Action::MoveDown).await.unwrap();
    harness
        .execute_action(Action::MoveToFirstLineChar)
        .await
        .unwrap();

    type_normal_keys(&mut harness, "di{").await;

    harness.assert_buffer_contents("fn main() {}");
    harness.assert_cursor_at(11, 0);
}

#[tokio::test]
async fn test_delete_text_object_aliases() {
    let mut harness = EditorHarness::with_content("items[alpha]");
    for _ in 0..7 {
        harness.execute_action(Action::MoveRight).await.unwrap();
    }
    type_normal_keys(&mut harness, "di]").await;
    harness.assert_buffer_contents("items[]");

    let mut harness = EditorHarness::with_content("block{alpha}");
    for _ in 0..7 {
        harness.execute_action(Action::MoveRight).await.unwrap();
    }
    type_normal_keys(&mut harness, "diB").await;
    harness.assert_buffer_contents("block{}");

    let mut harness = EditorHarness::with_content("Option<alpha>");
    for _ in 0..8 {
        harness.execute_action(Action::MoveRight).await.unwrap();
    }
    type_normal_keys(&mut harness, "di>").await;
    harness.assert_buffer_contents("Option<>");

    let mut harness = EditorHarness::with_content("let c = 'x';");
    for _ in 0..9 {
        harness.execute_action(Action::MoveRight).await.unwrap();
    }
    type_normal_keys(&mut harness, "di'").await;
    harness.assert_buffer_contents("let c = '';");

    let mut harness = EditorHarness::with_content("cmd `alpha`");
    for _ in 0..6 {
        harness.execute_action(Action::MoveRight).await.unwrap();
    }
    type_normal_keys(&mut harness, "di`").await;
    harness.assert_buffer_contents("cmd ``");
}

#[tokio::test]
async fn test_q_text_object_alias_selects_double_quotes() {
    let mut config = Config::default();
    config.keys.normal.insert(
        "v".to_string(),
        KeyAction::Single(Action::EnterMode(Mode::Visual)),
    );
    config.keys.visual.insert(
        "x".to_string(),
        KeyAction::Multiple(vec![Action::Delete, Action::EnterMode(Mode::Normal)]),
    );

    let buffer = Buffer::new(None, "let s = \"hello\";".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);
    for _ in 0..10 {
        harness.execute_action(Action::MoveRight).await.unwrap();
    }

    type_normal_keys(&mut harness, "viqx").await;

    harness.assert_buffer_contents("let s = \"\";");

    let mut harness = EditorHarness::with_content("let s = \"hello\";");
    for _ in 0..10 {
        harness.execute_action(Action::MoveRight).await.unwrap();
    }

    type_normal_keys(&mut harness, "diq").await;

    harness.assert_buffer_contents("let s = \"\";");
}

#[tokio::test]
async fn test_delete_inner_and_around_quotes() {
    let mut harness = EditorHarness::with_content("let s = \"hello world\";");
    for _ in 0..10 {
        harness.execute_action(Action::MoveRight).await.unwrap();
    }

    type_normal_keys(&mut harness, "di\"").await;
    harness.assert_buffer_contents("let s = \"\";");

    let mut harness = EditorHarness::with_content("let s = \"hello world\";");
    for _ in 0..10 {
        harness.execute_action(Action::MoveRight).await.unwrap();
    }

    type_normal_keys(&mut harness, "da\"").await;
    harness.assert_buffer_contents("let s = ;");
}

#[tokio::test]
async fn test_invalid_operator_motion_does_not_edit() {
    let mut harness = EditorHarness::with_content("alpha beta");

    type_normal_keys(&mut harness, "diz").await;

    harness.assert_buffer_contents("alpha beta");
    harness.assert_mode(Mode::Normal);
    assert_eq!(harness.last_error(), Some("invalid operator motion"));
}

#[tokio::test]
async fn operator_line_counts_delete_yank_and_change_as_one_edit() {
    let buffer = Buffer::new(None, "one\ntwo\nthree\nfour".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    type_normal_keys(&mut harness, "2dd").await;
    harness.assert_buffer_contents("three\nfour");
    type_normal_keys(&mut harness, "u").await;
    harness.assert_buffer_contents("one\ntwo\nthree\nfour");

    let buffer = Buffer::new(None, "one\ntwo\nthree\nfour".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    type_normal_keys(&mut harness, "2yyGp").await;
    harness.assert_buffer_contents("one\ntwo\nthree\nfour\none\ntwo");

    let buffer = Buffer::new(None, "one\ntwo\nthree\nfour".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    type_normal_keys(&mut harness, "d2d").await;
    harness.assert_buffer_contents("three\nfour");

    let buffer = Buffer::new(None, "one\ntwo\nthree".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    type_normal_keys(&mut harness, "2ccX").await;
    command_key(&mut harness, KeyCode::Esc).await;
    harness.assert_buffer_contents("X\nthree");
    type_normal_keys(&mut harness, "u").await;
    harness.assert_buffer_contents("one\ntwo\nthree");
}

#[tokio::test]
async fn operator_and_motion_counts_multiply_for_words_and_character_motions() {
    for keys in ["2dw", "d2w"] {
        let buffer = Buffer::new(None, "one two three four five".to_string());
        let mut harness = EditorHarness::with_config(buffer, default_key_config());
        type_normal_keys(&mut harness, keys).await;
        harness.assert_buffer_contents("three four five");
    }

    let buffer = Buffer::new(None, "one two three four five six".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    type_normal_keys(&mut harness, "2d2w").await;
    harness.assert_buffer_contents("five six");

    for keys in ["2df.", "d2f."] {
        let buffer = Buffer::new(None, "a.b.c.d".to_string());
        let mut harness = EditorHarness::with_config(buffer, default_key_config());
        type_normal_keys(&mut harness, keys).await;
        harness.assert_buffer_contents("c.d");
    }

    let buffer = Buffer::new(None, "a.b.c.d".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    type_normal_keys(&mut harness, "d2t.").await;
    harness.assert_buffer_contents(".c.d");

    let buffer = Buffer::new(None, "α β γ δ".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    type_normal_keys(&mut harness, "c2wX").await;
    command_key(&mut harness, KeyCode::Esc).await;
    harness.assert_buffer_contents("X γ δ");

    for (contents, keys, expected) in [
        ("one two", "d2w", ""),
        ("α β", "d2w", ""),
        ("one x", "dw", "x"),
    ] {
        let buffer = Buffer::new(None, contents.to_string());
        let mut harness = EditorHarness::with_config(buffer, default_key_config());
        type_normal_keys(&mut harness, keys).await;
        harness.assert_buffer_contents(expected);
    }
}

#[tokio::test]
async fn counted_operator_survives_dot_and_macro_replay() {
    let buffer = Buffer::new(None, "one two three\nfour five six".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    type_normal_keys(&mut harness, "d2wj.").await;
    harness.assert_buffer_contents("three\nsix");

    let buffer = Buffer::new(None, "one two three\nfour five six".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    type_normal_keys(&mut harness, "qad2wjq@a").await;
    harness.assert_buffer_contents("three\nsix");
}

#[tokio::test]
async fn zz_centers_an_interior_line_and_clamps_at_file_edges() {
    let content = (0..40)
        .map(|line| format!("line-{line:02}"))
        .collect::<Vec<_>>()
        .join("\n");

    let buffer = Buffer::new(None, content.clone());
    let mut harness = EditorHarness::with_config_and_size(buffer, default_key_config(), 80, 10);
    type_normal_keys(&mut harness, "jzz").await;
    assert_eq!(harness.viewport_top(), 0);
    assert_eq!(harness.buffer_line(), 1);

    harness
        .execute_action(Action::SetCursor(0, 20))
        .await
        .unwrap();
    type_normal_keys(&mut harness, "zz").await;
    assert_eq!(harness.viewport_top(), 16);
    assert_eq!(harness.buffer_line(), 20);
    assert_eq!(harness.render_cursor_position().unwrap().1, 4);

    harness
        .execute_action(Action::SetCursor(0, 39))
        .await
        .unwrap();
    type_normal_keys(&mut harness, "zz").await;
    assert_eq!(harness.viewport_top(), 32);
    assert_eq!(harness.buffer_line(), 39);
    assert_eq!(harness.render_cursor_position().unwrap().1, 7);
}

#[tokio::test]
async fn test_delete_till_forward_accepts_any_target_character() {
    for (content, keys, expected) in [
        ("alpha.beta", "dt.", ".beta"),
        ("alpha beta", "dtb", "beta"),
        ("alpha¶beta", "dt¶", "¶beta"),
    ] {
        let mut harness = EditorHarness::with_content(content);

        type_normal_keys(&mut harness, keys).await;

        harness.assert_buffer_contents(expected);
        harness.assert_cursor_at(0, 0);
    }
}

#[tokio::test]
async fn test_delete_till_adjacent_target_deletes_current_character() {
    let mut harness = EditorHarness::with_content("a.alpha");

    type_normal_keys(&mut harness, "dt.").await;

    harness.assert_buffer_contents(".alpha");
    harness.assert_cursor_at(0, 0);
    assert_eq!(harness.last_error(), None);
}

#[tokio::test]
async fn test_delete_till_missing_target_does_not_edit() {
    let mut harness = EditorHarness::with_content("alpha beta");

    type_normal_keys(&mut harness, "dt.").await;

    harness.assert_buffer_contents("alpha beta");
    harness.assert_cursor_at(0, 0);
    assert_eq!(harness.last_error(), Some("character not found"));
}

#[tokio::test]
async fn find_and_till_forward_move_to_the_requested_character() {
    let mut harness = EditorHarness::with_content("alpha.beta.gamma");

    type_normal_keys(&mut harness, "f.").await;
    harness.assert_cursor_at(5, 0);

    let mut harness = EditorHarness::with_content("alpha.beta.gamma");
    type_normal_keys(&mut harness, "t.").await;
    harness.assert_cursor_at(4, 0);
}

#[tokio::test]
async fn counted_find_and_till_forward_use_the_nth_match() {
    let mut harness = EditorHarness::with_content("alpha.beta.gamma");

    type_normal_keys(&mut harness, "2f.").await;
    harness.assert_cursor_at(10, 0);

    let mut harness = EditorHarness::with_content("alpha.beta.gamma");
    type_normal_keys(&mut harness, "2t.").await;
    harness.assert_cursor_at(9, 0);
}

#[tokio::test]
async fn delete_and_change_accept_find_forward_suffixes() {
    let mut harness = EditorHarness::with_content("alpha.beta");
    type_normal_keys(&mut harness, "df.").await;
    harness.assert_buffer_contents("beta");

    let mut harness = EditorHarness::with_content("alpha.beta");
    type_normal_keys(&mut harness, "cf.").await;
    harness.assert_mode(Mode::Insert);
    harness.type_text("X").await.unwrap();
    harness.assert_buffer_contents("Xbeta");
}

#[tokio::test]
async fn change_till_forward_keeps_the_target_character() {
    let mut harness = EditorHarness::with_content("alpha.beta");

    type_normal_keys(&mut harness, "ct.").await;
    harness.assert_mode(Mode::Insert);
    harness.type_text("X").await.unwrap();

    harness.assert_buffer_contents("X.beta");
}

#[tokio::test]
async fn yank_accepts_find_and_till_forward_suffixes() {
    let mut harness = EditorHarness::with_content("alpha.beta");
    let clipboard_text = Arc::new(Mutex::new(None));
    harness
        .editor
        .test_set_clipboard(Box::new(MemoryClipboardProvider::from(
            clipboard_text.clone(),
        )));

    type_normal_keys(&mut harness, "yf.").await;
    assert_eq!(clipboard_text.lock().unwrap().as_deref(), Some("alpha."));
    harness.assert_buffer_contents("alpha.beta");

    let mut harness = EditorHarness::with_content("alpha.beta");
    let clipboard_text = Arc::new(Mutex::new(None));
    harness
        .editor
        .test_set_clipboard(Box::new(MemoryClipboardProvider::from(
            clipboard_text.clone(),
        )));

    type_normal_keys(&mut harness, "yt.").await;
    assert_eq!(clipboard_text.lock().unwrap().as_deref(), Some("alpha"));
    harness.assert_buffer_contents("alpha.beta");
}

#[tokio::test]
async fn visual_find_and_till_forward_extend_the_selection() {
    let buffer = Buffer::new(None, "alpha.beta".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    type_normal_keys(&mut harness, "vf.").await;
    harness.assert_mode(Mode::Visual);
    harness.assert_cursor_at(5, 0);
    type_normal_keys(&mut harness, "x").await;
    harness.assert_buffer_contents("beta");

    let buffer = Buffer::new(None, "alpha.beta".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    type_normal_keys(&mut harness, "vt.").await;
    harness.assert_mode(Mode::Visual);
    harness.assert_cursor_at(4, 0);
    type_normal_keys(&mut harness, "x").await;
    harness.assert_buffer_contents(".beta");
}

#[tokio::test]
async fn missing_find_forward_target_does_not_move_or_edit() {
    let mut harness = EditorHarness::with_content("alpha beta");

    type_normal_keys(&mut harness, "f.").await;

    harness.assert_buffer_contents("alpha beta");
    harness.assert_cursor_at(0, 0);
    assert_eq!(harness.last_error(), Some("character not found"));
}

#[tokio::test]
async fn test_delete_and_change_line_key_sequences() {
    let mut harness = EditorHarness::with_content("one\ntwo\nthree");
    harness.execute_action(Action::MoveDown).await.unwrap();

    type_normal_keys(&mut harness, "dd").await;

    harness.assert_buffer_contents("one\nthree");
    harness.assert_cursor_at(0, 1);

    let mut harness = EditorHarness::with_content("one\ntwo\nthree");
    harness.execute_action(Action::MoveDown).await.unwrap();

    type_normal_keys(&mut harness, "cc").await;

    harness.assert_mode(Mode::Insert);
    harness.type_text("changed").await.unwrap();
    harness.assert_buffer_contents("one\nchanged\nthree");
}

#[tokio::test]
async fn test_yank_line_key_sequence_pastes_linewise() {
    let mut harness = EditorHarness::with_content("one\ntwo\nthree");
    harness.execute_action(Action::MoveDown).await.unwrap();

    type_normal_keys(&mut harness, "yy").await;

    harness.assert_buffer_contents("one\ntwo\nthree");
    assert!(!harness.is_dirty());
    harness.assert_cursor_at(0, 1);

    harness.execute_action(Action::Paste).await.unwrap();
    harness.assert_buffer_contents("one\ntwo\ntwo\nthree");

    let mut harness = EditorHarness::with_content("one\ntwo\nthree");
    harness.execute_action(Action::MoveDown).await.unwrap();

    type_normal_keys(&mut harness, "yy").await;
    harness.execute_action(Action::MoveDown).await.unwrap();
    harness.execute_action(Action::PasteBefore).await.unwrap();

    harness.assert_buffer_contents("one\ntwo\ntwo\nthree");
}

#[tokio::test]
async fn yanking_default_register_writes_system_clipboard() {
    let mut harness = EditorHarness::with_content("one\ntwo\nthree");
    let clipboard_text = Arc::new(Mutex::new(None));
    harness
        .editor
        .test_set_clipboard(Box::new(MemoryClipboardProvider::from(
            clipboard_text.clone(),
        )));
    harness.execute_action(Action::MoveDown).await.unwrap();

    type_normal_keys(&mut harness, "yy").await;

    assert_eq!(clipboard_text.lock().unwrap().as_deref(), Some("two\n"));
}

#[tokio::test]
async fn deleting_default_register_writes_system_clipboard() {
    let mut harness = EditorHarness::with_content("one\ntwo\nthree");
    let clipboard_text = Arc::new(Mutex::new(None));
    harness
        .editor
        .test_set_clipboard(Box::new(MemoryClipboardProvider::from(
            clipboard_text.clone(),
        )));
    harness.execute_action(Action::MoveDown).await.unwrap();

    harness
        .execute_action(Action::DeleteCurrentLine)
        .await
        .unwrap();

    assert_eq!(clipboard_text.lock().unwrap().as_deref(), Some("two\n"));
}

#[tokio::test]
async fn paste_reads_external_system_clipboard_text() {
    let mut harness = EditorHarness::with_content("abc");
    harness
        .editor
        .test_set_clipboard(Box::new(MemoryClipboardProvider::with_text("system")));

    harness.execute_action(Action::PasteBefore).await.unwrap();

    harness.assert_buffer_contents("systemabc");
}

#[tokio::test]
async fn pending_key_sequences_use_waiting_cursor_state() {
    let mut config = Config::default();
    config.keys.normal.insert(
        "g".to_string(),
        KeyAction::Nested(
            [("g".to_string(), KeyAction::Single(Action::MoveToTop))]
                .into_iter()
                .collect(),
        ),
    );
    config
        .keys
        .normal
        .insert("j".to_string(), KeyAction::Single(Action::MoveDown));
    let buffer = Buffer::new(None, "one\ntwo\nthree".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    type_normal_keys(&mut harness, "g").await;
    assert!(harness.is_waiting_for_key_sequence());

    type_normal_keys(&mut harness, "g").await;
    assert!(!harness.is_waiting_for_key_sequence());

    type_normal_keys(&mut harness, "d").await;
    assert!(harness.is_waiting_for_key_sequence());

    type_normal_keys(&mut harness, "d").await;
    assert!(!harness.is_waiting_for_key_sequence());

    type_normal_keys(&mut harness, "2").await;
    assert!(harness.is_waiting_for_key_sequence());

    type_normal_keys(&mut harness, "j").await;
    assert!(!harness.is_waiting_for_key_sequence());

    harness
        .execute_action(Action::EnterMode(Mode::Visual))
        .await
        .unwrap();
    type_normal_keys(&mut harness, "i").await;
    assert!(harness.is_waiting_for_key_sequence());
}

#[tokio::test]
async fn literal_space_key_starts_leader_sequence() {
    let mut config = Config::default();
    config.keys.normal.insert(
        " ".to_string(),
        KeyAction::Nested(
            [("t".to_string(), KeyAction::Single(Action::MoveToBottom))]
                .into_iter()
                .collect(),
        ),
    );
    let buffer = Buffer::new(None, "one\ntwo\nthree".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    type_normal_keys(&mut harness, " ").await;
    assert!(harness.is_waiting_for_key_sequence());

    type_normal_keys(&mut harness, "t").await;

    assert!(!harness.is_waiting_for_key_sequence());
    harness.assert_cursor_at(0, 2);
}

#[tokio::test]
async fn named_space_key_still_starts_leader_sequence() {
    let mut config = Config::default();
    config.keys.normal.insert(
        "Space".to_string(),
        KeyAction::Nested(
            [("t".to_string(), KeyAction::Single(Action::MoveToBottom))]
                .into_iter()
                .collect(),
        ),
    );
    let buffer = Buffer::new(None, "one\ntwo\nthree".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    type_normal_keys(&mut harness, " ").await;
    assert!(harness.is_waiting_for_key_sequence());

    type_normal_keys(&mut harness, "t").await;

    assert!(!harness.is_waiting_for_key_sequence());
    harness.assert_cursor_at(0, 2);
}

#[tokio::test]
async fn ctrl_space_keeps_named_key_binding() {
    let mut config = Config::default();
    config.keys.insert.insert(
        "Ctrl-Space".to_string(),
        KeyAction::Single(Action::MoveToBottom),
    );
    let buffer = Buffer::new(None, "one\ntwo\nthree".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness
        .execute_event(Event::Key(KeyEvent::new(
            KeyCode::Char(' '),
            KeyModifiers::CONTROL,
        )))
        .await
        .unwrap();

    harness.assert_cursor_at(0, 2);
}

#[tokio::test]
async fn test_change_line() {
    let mut harness = EditorHarness::with_content("Line 1\nLine 2\nLine 3");

    // Move to second line
    harness.execute_action(Action::MoveDown).await.unwrap();

    // Change line with 'cc' - delete line content and enter insert mode
    harness
        .execute_action(Action::MoveToLineStart)
        .await
        .unwrap();
    let line_len = harness.current_line().unwrap().trim_end().len();
    for _ in 0..line_len {
        harness
            .execute_action(Action::DeleteCharAtCursorPos)
            .await
            .unwrap();
    }
    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.assert_mode(Mode::Insert);

    // Type replacement
    harness.type_text("Changed line").await.unwrap();
    harness.assert_buffer_contents("Line 1\nChanged line\nLine 3");

    // Exit insert mode
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
}

#[tokio::test]
async fn test_replace_char() {
    let mut harness = EditorHarness::with_content("Hello World");

    // Replace character with 'r' - delete char and insert new one
    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    harness
        .execute_action(Action::InsertCharAtCursorPos('J'))
        .await
        .unwrap();
    harness.assert_buffer_contents("Jello World");
    harness.assert_mode(Mode::Normal); // Should stay in normal mode
}

#[tokio::test]
async fn test_insert_at_line_start() {
    let mut harness = EditorHarness::with_content("    Hello World");

    // Move cursor to middle
    harness
        .execute_action(Action::MoveToNextWord)
        .await
        .unwrap();

    // Insert at start of line with 'I' - move to start and enter insert
    harness
        .execute_action(Action::MoveToLineStart)
        .await
        .unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.assert_mode(Mode::Insert);
    harness.assert_cursor_at(0, 0);

    // Type text
    harness.type_text("Start: ").await.unwrap();
    harness.assert_buffer_contents("Start:     Hello World");

    // Exit insert mode
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
}

#[tokio::test]
async fn test_insert_key_escape_without_insert_stays_on_original_character() {
    let mut config = Config::default();
    config.keys.normal.insert(
        "i".to_string(),
        KeyAction::Single(Action::EnterMode(Mode::Insert)),
    );
    config.keys.insert.insert(
        "Esc".to_string(),
        KeyAction::Single(Action::EnterMode(Mode::Normal)),
    );
    let buffer = Buffer::new(None, "abc".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);
    harness.execute_action(Action::MoveRight).await.unwrap();
    let start = harness.render_cursor_position().unwrap();

    type_normal_keys(&mut harness, "i").await;
    harness
        .execute_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
        .await
        .unwrap();

    harness.assert_mode(Mode::Normal);
    harness.assert_cursor_at(1, 0);
    assert_eq!(harness.render_cursor_position(), Some(start));
}

#[tokio::test]
async fn test_append_key_positions_cursor_after_current_character() {
    let mut config = Config::default();
    config.keys.normal.insert(
        "a".to_string(),
        KeyAction::Multiple(vec![Action::EnterMode(Mode::Insert), Action::MoveRight]),
    );
    config.keys.insert.insert(
        "Esc".to_string(),
        KeyAction::Single(Action::EnterMode(Mode::Normal)),
    );
    let buffer = Buffer::new(None, "abc".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);
    let start = harness.render_cursor_position().unwrap();

    type_normal_keys(&mut harness, "a").await;

    harness.assert_mode(Mode::Insert);
    harness.assert_cursor_at(1, 0);
    assert_eq!(
        harness.render_cursor_position(),
        Some((start.0 + 1, start.1))
    );

    harness.type_text("X").await.unwrap();
    harness.assert_buffer_contents("aXbc");

    harness
        .execute_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
        .await
        .unwrap();
    harness.assert_mode(Mode::Normal);
    harness.assert_cursor_at(1, 0);
}

#[tokio::test]
async fn test_append_key_escape_without_insert_returns_to_original_character() {
    let mut config = Config::default();
    config.keys.normal.insert(
        "a".to_string(),
        KeyAction::Multiple(vec![Action::EnterMode(Mode::Insert), Action::MoveRight]),
    );
    config.keys.insert.insert(
        "Esc".to_string(),
        KeyAction::Single(Action::EnterMode(Mode::Normal)),
    );
    let buffer = Buffer::new(None, "abc".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);
    let start = harness.render_cursor_position().unwrap();

    type_normal_keys(&mut harness, "a").await;
    harness
        .execute_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
        .await
        .unwrap();

    harness.assert_mode(Mode::Normal);
    harness.assert_cursor_at(0, 0);
    assert_eq!(harness.render_cursor_position(), Some(start));
}

#[tokio::test]
async fn test_append_line_key_positions_cursor_after_line_end() {
    let mut config = Config::default();
    config.keys.normal.insert(
        "A".to_string(),
        KeyAction::Multiple(vec![
            Action::MoveToLineEnd,
            Action::EnterMode(Mode::Insert),
            Action::MoveRight,
        ]),
    );
    config.keys.insert.insert(
        "Esc".to_string(),
        KeyAction::Single(Action::EnterMode(Mode::Normal)),
    );
    let buffer = Buffer::new(None, "abc".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);
    let start = harness.render_cursor_position().unwrap();

    type_normal_keys(&mut harness, "A").await;

    harness.assert_mode(Mode::Insert);
    harness.assert_cursor_at(3, 0);
    assert_eq!(
        harness.render_cursor_position(),
        Some((start.0 + 3, start.1))
    );

    harness.type_text("X").await.unwrap();
    harness.assert_buffer_contents("abcX");

    harness
        .execute_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
        .await
        .unwrap();
    harness.assert_mode(Mode::Normal);
    harness.assert_cursor_at(3, 0);
}

#[tokio::test]
async fn test_append_line_key_escape_without_insert_returns_to_last_character() {
    let mut config = Config::default();
    config.keys.normal.insert(
        "A".to_string(),
        KeyAction::Multiple(vec![
            Action::MoveToLineEnd,
            Action::EnterMode(Mode::Insert),
            Action::MoveRight,
        ]),
    );
    config.keys.insert.insert(
        "Esc".to_string(),
        KeyAction::Single(Action::EnterMode(Mode::Normal)),
    );
    let buffer = Buffer::new(None, "abc".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);
    let start = harness.render_cursor_position().unwrap();

    type_normal_keys(&mut harness, "A").await;
    harness
        .execute_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
        .await
        .unwrap();

    harness.assert_mode(Mode::Normal);
    harness.assert_cursor_at(2, 0);
    assert_eq!(
        harness.render_cursor_position(),
        Some((start.0 + 2, start.1))
    );
}

#[tokio::test]
async fn test_append_at_line_end() {
    let mut harness = EditorHarness::with_content("Hello World");

    // Append at end of line with 'A' - move to end and enter insert
    harness.execute_action(Action::MoveToLineEnd).await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.execute_action(Action::MoveRight).await.unwrap();
    harness.assert_mode(Mode::Insert);

    // Type text
    harness.type_text(" Test").await.unwrap();
    harness.assert_buffer_contents("Hello World Test");

    // Exit insert mode
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
}

#[tokio::test]
async fn test_escape_from_insert_clamps_to_last_line_character() {
    let mut harness = EditorHarness::with_content("Hello");

    harness.execute_action(Action::MoveToLineEnd).await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.execute_action(Action::MoveRight).await.unwrap();
    harness.assert_cursor_at(5, 0);

    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
    harness.assert_cursor_at(4, 0);
}

#[tokio::test]
async fn test_delete_word() {
    let mut harness = EditorHarness::with_content("Hello World Test");

    // Delete word with 'dw'
    harness.execute_action(Action::DeleteWord).await.unwrap();
    harness.assert_buffer_contents("World Test");

    // Delete another word (including space)
    harness.execute_action(Action::DeleteWord).await.unwrap();
    harness.assert_buffer_contents("Test");
}

#[tokio::test]
async fn test_join_lines() {
    for (contents, keys, expected, cursor) in [
        ("alpha\n    beta", "J", "alpha beta", (5, 0)),
        ("alpha\n    ) tail", "J", "alpha) tail", (5, 0)),
        ("alpha \n    beta", "J", "alpha beta", (6, 0)),
        ("α\u{0301}\r\n    β", "J", "α\u{0301} β", (1, 0)),
        (
            "one\n  two\n    three\nfour",
            "3J",
            "one two three\nfour",
            (7, 0),
        ),
        ("alpha \n    beta", "gJ", "alpha     beta", (6, 0)),
        (
            "one\n  two\n    three\nfour",
            "VjjJ",
            "one two three\nfour",
            (7, 0),
        ),
        (
            "one \n  two\n    three\nfour",
            "VjjgJ",
            "one   two    three\nfour",
            (9, 0),
        ),
        (
            "one \n  two\n    three\nfour",
            "3gJ",
            "one   two    three\nfour",
            (9, 0),
        ),
    ] {
        let mut harness = EditorHarness::with_config(
            Buffer::new(None, contents.to_string()),
            default_key_config(),
        );

        type_normal_keys(&mut harness, keys).await;

        harness.assert_buffer_contents(expected);
        harness.assert_cursor_at(cursor.0, cursor.1);
        harness.assert_mode(Mode::Normal);
    }
}

#[tokio::test]
async fn join_lines_is_one_undoable_repeatable_change() {
    let mut harness = EditorHarness::with_config(
        Buffer::new(None, "one\n  two\nthree\n  four".to_string()),
        default_key_config(),
    );

    type_normal_keys(&mut harness, "Jj.").await;
    harness.assert_buffer_contents("one two\nthree four");

    type_normal_keys(&mut harness, "u").await;
    harness.assert_buffer_contents("one two\nthree\n  four");
    type_normal_keys(&mut harness, "u").await;
    harness.assert_buffer_contents("one\n  two\nthree\n  four");
}

#[tokio::test]
async fn join_lines_survives_macro_replay_and_eof() {
    let mut harness = EditorHarness::with_config(
        Buffer::new(None, "one\n  two\nthree\n  four".to_string()),
        default_key_config(),
    );

    type_normal_keys(&mut harness, "qaJjq@a").await;
    harness.assert_buffer_contents("one two\nthree four");

    type_normal_keys(&mut harness, "GJ").await;
    harness.assert_buffer_contents("one two\nthree four");
}

#[tokio::test]
async fn join_ex_command_supports_count_and_bang() {
    for (contents, command, expected) in [
        ("one\n  two\nthree", "join", "one two\nthree"),
        ("one\n  two\nthree\nfour", "j 3", "one two three\nfour"),
        ("one \n  two\nthree", "join!", "one   two\nthree"),
        ("one \n  two\nthree\nfour", "j! 3", "one   twothree\nfour"),
    ] {
        let mut harness = EditorHarness::with_config(
            Buffer::new(None, contents.to_string()),
            default_key_config(),
        );

        harness
            .execute_action(Action::Command(command.to_string()))
            .await
            .unwrap();

        harness.assert_buffer_contents(expected);
    }
}

#[tokio::test]
async fn test_undo_redo() {
    let mut harness = EditorHarness::with_content("Hello World");

    // Make a change
    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    harness.assert_buffer_contents("ello World");

    // Undo with 'u'
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("Hello World");

    harness.execute_action(Action::Redo).await.unwrap();
    harness.assert_buffer_contents("ello World");
}

#[tokio::test]
async fn test_undo_multi_character_insert_session() {
    let mut harness = EditorHarness::with_content("");

    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.type_text("hello").await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();

    harness.assert_buffer_contents("hello\n");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("\n");
    harness.execute_action(Action::Redo).await.unwrap();
    harness.assert_buffer_contents("hello\n");
}

#[tokio::test]
async fn test_undo_insert_backspace_session() {
    let mut harness = EditorHarness::with_content("");

    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.type_text("abc").await.unwrap();
    harness
        .execute_action(Action::DeletePreviousChar)
        .await
        .unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();

    harness.assert_buffer_contents("ab\n");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("\n");
    harness.execute_action(Action::Redo).await.unwrap();
    harness.assert_buffer_contents("ab\n");
}

#[tokio::test]
async fn test_backspace_at_line_start_joins_with_previous_line() {
    let mut harness = EditorHarness::with_content("abc\ndef");

    harness.execute_action(Action::MoveDown).await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness
        .execute_action(Action::DeletePreviousChar)
        .await
        .unwrap();

    harness.assert_buffer_contents("abcdef");
    harness.assert_cursor_at(3, 0);
}

#[tokio::test]
async fn test_undo_delete_range_and_word() {
    let mut harness = EditorHarness::with_content("hello world");

    harness.execute_action(Action::DeleteWord).await.unwrap();
    harness.assert_buffer_contents("world");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("hello world");

    harness
        .execute_action(Action::DeleteRange(0, 0, 5, 0))
        .await
        .unwrap();
    harness.assert_buffer_contents(" world");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("hello world");
}

#[tokio::test]
async fn test_undo_delete_current_line() {
    let mut harness = EditorHarness::with_content("one\ntwo\nthree");

    harness.execute_action(Action::MoveDown).await.unwrap();
    harness
        .execute_action(Action::DeleteCurrentLine)
        .await
        .unwrap();
    harness.assert_buffer_contents("one\nthree");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("one\ntwo\nthree");

    let mut harness = EditorHarness::with_content("single");
    harness
        .execute_action(Action::DeleteCurrentLine)
        .await
        .unwrap();
    harness.assert_buffer_contents("");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("single");
}

#[tokio::test]
async fn test_delete_current_line_yanks_for_linewise_paste_before() {
    let mut harness = EditorHarness::with_content("one\ntwo\nthree");

    harness.execute_action(Action::MoveDown).await.unwrap();
    harness
        .execute_action(Action::DeleteCurrentLine)
        .await
        .unwrap();
    harness.assert_buffer_contents("one\nthree");

    harness
        .execute_action(Action::MoveToLineStart)
        .await
        .unwrap();
    harness.execute_action(Action::PasteBefore).await.unwrap();
    harness.assert_buffer_contents("one\ntwo\nthree");
}

#[tokio::test]
async fn test_undo_multiline_insert_and_unicode() {
    let mut harness = EditorHarness::with_content("");

    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.type_text("a👋").await.unwrap();
    harness.execute_action(Action::InsertNewLine).await.unwrap();
    harness.type_text("é").await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();

    harness.assert_buffer_contents("a👋\né\n");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("\n");
    harness.execute_action(Action::Redo).await.unwrap();
    harness.assert_buffer_contents("a👋\né\n");
}

#[tokio::test]
async fn test_redo_stack_clears_after_new_edit() {
    let mut harness = EditorHarness::with_content("abc");

    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    harness.assert_buffer_contents("bc");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("abc");
    harness
        .execute_action(Action::InsertCharAtCursorPos('z'))
        .await
        .unwrap();
    harness.assert_buffer_contents("zabc");
    harness.execute_action(Action::Redo).await.unwrap();
    harness.assert_buffer_contents("zabc");
}

#[tokio::test]
async fn undo_tree_preserves_and_traverses_sibling_branches() {
    let mut harness = EditorHarness::with_content("abc");
    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    harness.execute_action(Action::Undo).await.unwrap();
    harness
        .execute_action(Action::InsertCharAtCursorPos('z'))
        .await
        .unwrap();
    harness.execute_action(Action::Undo).await.unwrap();

    harness
        .execute_action(Action::SelectPreviousUndoBranch)
        .await
        .unwrap();
    harness.execute_action(Action::Redo).await.unwrap();
    harness.assert_buffer_contents("bc");

    harness.execute_action(Action::Undo).await.unwrap();
    harness
        .execute_action(Action::SelectNextUndoBranch)
        .await
        .unwrap();
    harness.execute_action(Action::Redo).await.unwrap();
    harness.assert_buffer_contents("zabc");
}

#[tokio::test]
async fn selective_revert_applies_only_when_the_post_image_still_matches() {
    let mut harness = EditorHarness::with_content("abc");
    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    let transaction_id = harness.editor.test_undo_tree()[0].transaction_id.clone();
    harness.execute_action(Action::MoveToLineEnd).await.unwrap();
    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    harness.assert_buffer_contents("b");

    harness
        .execute_action(Action::RevertTransaction(transaction_id))
        .await
        .unwrap();
    harness.assert_buffer_contents("ab");
    assert!(harness.editor.test_undo_tree().len() >= 3);

    let mut harness = EditorHarness::with_content("abc");
    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    let transaction_id = harness.editor.test_undo_tree()[0].transaction_id.clone();
    harness
        .execute_action(Action::InsertCharAtCursorPos('X'))
        .await
        .unwrap();
    harness
        .execute_action(Action::RevertTransaction(transaction_id))
        .await
        .unwrap();
    harness.assert_buffer_contents("Xbc");
    assert!(harness
        .last_error()
        .is_some_and(|message| message.contains("revert conflict")));
}

#[tokio::test]
async fn selective_revert_accepts_adjacent_insertions_from_one_insert_transaction() {
    let mut harness = EditorHarness::with_content("");

    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.type_text("abc").await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
    harness.assert_buffer_contents("abc\n");
    let transaction_id = harness.editor.test_undo_tree()[0].transaction_id.clone();

    harness
        .execute_action(Action::RevertTransaction(transaction_id))
        .await
        .unwrap();

    harness.assert_buffer_contents("\n");
    assert!(!harness
        .last_error()
        .is_some_and(|message| message.contains("revert conflict")));
}

#[tokio::test]
async fn selective_revert_shifts_a_replacement_past_a_later_left_edge_insertion() {
    let mut harness = EditorHarness::with_content("abc");
    harness.execute_action(Action::MoveRight).await.unwrap();
    harness
        .execute_action(Action::ReplaceCharsAtCursor {
            character: 'B',
            count: 1,
        })
        .await
        .unwrap();
    harness.assert_buffer_contents("aBc");
    let transaction_id = harness.editor.test_undo_tree()[0].transaction_id.clone();
    harness
        .execute_action(Action::InsertCharAtCursorPos('!'))
        .await
        .unwrap();
    harness.assert_buffer_contents("a!Bc");

    harness
        .execute_action(Action::RevertTransaction(transaction_id))
        .await
        .unwrap();

    harness.assert_buffer_contents("a!bc");
    assert!(!harness
        .last_error()
        .is_some_and(|message| message.contains("revert conflict")));
}

#[tokio::test]
async fn test_undo_does_not_create_new_undo_entries() {
    let mut harness = EditorHarness::with_content("abc");

    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    harness.execute_action(Action::Undo).await.unwrap();
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("abc");
}

#[tokio::test]
async fn test_undo_indent_and_unindent() {
    let mut harness = EditorHarness::with_content("line");

    harness.execute_action(Action::IndentLine).await.unwrap();
    harness.assert_buffer_contents("    line");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("line");

    harness.execute_action(Action::IndentLine).await.unwrap();
    harness.execute_action(Action::UnindentLine).await.unwrap();
    harness.assert_buffer_contents("line");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("    line");
}

#[tokio::test]
async fn test_undo_visual_char_line_and_block_delete() {
    let mut harness = EditorHarness::with_content("abcde");
    harness
        .execute_action(Action::EnterMode(Mode::Visual))
        .await
        .unwrap();
    harness.execute_action(Action::MoveRight).await.unwrap();
    harness.execute_action(Action::MoveRight).await.unwrap();
    harness.execute_action(Action::Delete).await.unwrap();
    harness.assert_buffer_contents("de");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("abcde");

    let mut harness = EditorHarness::with_content("one\ntwo\nthree");
    harness
        .execute_action(Action::EnterMode(Mode::VisualLine))
        .await
        .unwrap();
    harness.execute_action(Action::MoveDown).await.unwrap();
    harness.execute_action(Action::Delete).await.unwrap();
    harness.assert_buffer_contents("three");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("one\ntwo\nthree");

    let mut harness = EditorHarness::with_content("abc\ndef");
    harness
        .execute_action(Action::EnterMode(Mode::VisualBlock))
        .await
        .unwrap();
    harness.execute_action(Action::MoveRight).await.unwrap();
    harness.execute_action(Action::MoveDown).await.unwrap();
    harness.execute_action(Action::Delete).await.unwrap();
    harness.assert_buffer_contents("c\nf");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("abc\ndef");
}

#[tokio::test]
async fn test_visual_block_insert_undoes_and_redoes_as_one_transaction() {
    let mut harness = EditorHarness::with_content("impl\nfn\nColor\n}\n}");

    harness
        .execute_action(Action::EnterMode(Mode::VisualBlock))
        .await
        .unwrap();
    for _ in 0..4 {
        harness.execute_action(Action::MoveDown).await.unwrap();
    }
    harness.execute_action(Action::InsertBlock).await.unwrap();
    harness
        .execute_action(Action::InsertCharAtCursorPos(' '))
        .await
        .unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();

    harness.assert_buffer_contents(" impl\n fn\n Color\n }\n }");

    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("impl\nfn\nColor\n}\n}");

    harness.execute_action(Action::Redo).await.unwrap();
    harness.assert_buffer_contents(" impl\n fn\n Color\n }\n }");
}

#[tokio::test]
async fn test_visual_block_insert_coalesces_replayed_change_notifications() {
    let path = temp_file_path("visual-block-insert-lsp");
    let lsp = RecordingLsp::default();
    let events = lsp.events();
    let config = Config::default();
    let theme = Theme::default();
    let buffer = Buffer::new(Some(path.clone()), "impl\nfn\nColor\n}\n}".to_string());
    let mut editor = Editor::with_size(Box::new(lsp), 80, 24, config, theme, vec![buffer]).unwrap();
    editor.test_disable_terminal_output();
    let mut harness = EditorHarness { editor };

    harness
        .execute_action(Action::EnterMode(Mode::VisualBlock))
        .await
        .unwrap();
    for _ in 0..4 {
        harness.execute_action(Action::MoveDown).await.unwrap();
    }
    harness.execute_action(Action::InsertBlock).await.unwrap();
    harness
        .execute_action(Action::InsertCharAtCursorPos(' '))
        .await
        .unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();

    let did_change_count = events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| matches!(event, LspEvent::DidChange(file) if file == &path))
        .count();
    assert_eq!(
        did_change_count, 2,
        "expected one notification for the initial insert and one coalesced replay notification"
    );

    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn test_visual_block_insert_clears_selection_background_after_apply() {
    let mut harness = EditorHarness::with_content("impl\nfn\nColor\n}\n}");
    let selection_bg = Color::Rgb {
        r: 12,
        g: 34,
        b: 56,
    };
    harness.editor.theme.selection_style = Some(Style {
        bg: Some(selection_bg),
        ..Default::default()
    });

    harness
        .execute_action(Action::EnterMode(Mode::VisualBlock))
        .await
        .unwrap();
    for _ in 0..4 {
        harness.execute_action(Action::MoveDown).await.unwrap();
    }
    harness.execute_action(Action::InsertBlock).await.unwrap();
    harness
        .execute_action(Action::InsertCharAtCursorPos(' '))
        .await
        .unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();

    for y in 0..5 {
        for x in 0..40 {
            assert_ne!(
                harness.render_cell_bg(x, y).unwrap(),
                Some(selection_bg),
                "selection background leaked at ({x}, {y}) after block insert"
            );
        }
    }
}

#[tokio::test]
async fn test_visual_line_selection_uses_buffer_lines_after_scrolling() {
    let content = (0..40)
        .map(|line| format!("line-{line:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut harness = EditorHarness::with_content(&content);

    harness
        .execute_action(Action::SetCursor(0, 30))
        .await
        .unwrap();
    assert_eq!(harness.viewport_top(), 9);
    harness.assert_cursor_at(0, 30);

    harness
        .execute_action(Action::EnterMode(Mode::VisualLine))
        .await
        .unwrap();
    harness.execute_action(Action::MoveDown).await.unwrap();
    harness.execute_action(Action::Delete).await.unwrap();

    let remaining = harness.buffer_contents();
    assert!(
        !remaining.contains("line-30\nline-31"),
        "visual line delete should remove the scrolled-to buffer lines"
    );
    assert!(
        remaining.contains("line-21"),
        "visual line delete should not use viewport-relative rows as buffer lines"
    );
}

#[tokio::test]
async fn visual_line_delete_whole_scrolled_buffer_repositions_cursor_safely() {
    let content = (0..40)
        .map(|line| format!("line-{line:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let buffer = Buffer::new(None, content.clone());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());

    type_normal_keys(&mut harness, "ggVGx").await;

    harness.assert_buffer_contents("");
    harness.assert_cursor_at(0, 0);
    assert_eq!(harness.viewport_top(), 0);
    harness.assert_mode(Mode::Normal);

    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents(&content);
}

#[tokio::test]
async fn visual_paste_replaces_whole_document_from_system_clipboard() {
    let content = (0..40)
        .map(|line| format!("line-{line:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let clipboard_text = Arc::new(Mutex::new(Some("replacement".to_string())));
    let buffer = Buffer::new(None, content.clone());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    harness
        .editor
        .test_set_clipboard(Box::new(MemoryClipboardProvider::from(
            clipboard_text.clone(),
        )));

    type_normal_keys(&mut harness, "ggVGp").await;

    harness.assert_buffer_contents("replacement");
    harness.assert_cursor_at(0, 0);
    harness.assert_mode(Mode::Normal);
    assert_eq!(
        clipboard_text.lock().unwrap().as_deref(),
        Some(content.as_str())
    );

    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents(&content);
    harness.execute_action(Action::Redo).await.unwrap();
    harness.assert_buffer_contents("replacement");
}

#[tokio::test]
async fn visual_line_paste_replaces_large_interior_selection_with_one_line() {
    let content = (1..=20)
        .map(|line| format!("line-{line:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let buffer = Buffer::new(None, content);
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    harness
        .editor
        .test_set_clipboard(Box::new(MemoryClipboardProvider::with_text(
            "node dist/src/cli.js plan validate examples/hello-world.yaml",
        )));
    harness
        .execute_action(Action::SetCursor(0, 2))
        .await
        .unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::VisualLine))
        .await
        .unwrap();
    for _ in 0..5 {
        harness.execute_action(Action::MoveDown).await.unwrap();
    }

    type_normal_keys(&mut harness, "p").await;

    harness.assert_buffer_contents(
        "line-01\nline-02\nnode dist/src/cli.js plan validate examples/hello-world.yaml\nline-09\nline-10\nline-11\nline-12\nline-13\nline-14\nline-15\nline-16\nline-17\nline-18\nline-19\nline-20",
    );
    harness.assert_cursor_at(0, 2);
    harness.assert_mode(Mode::Normal);
}

#[tokio::test]
async fn visual_line_paste_replaces_small_interior_selection_with_many_lines() {
    let content = (1..=8)
        .map(|line| format!("line-{line:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let buffer = Buffer::new(None, content);
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    harness
        .editor
        .test_set_clipboard(Box::new(MemoryClipboardProvider::with_text(
            "replacement-a\nreplacement-b\nreplacement-c",
        )));
    harness
        .execute_action(Action::SetCursor(0, 2))
        .await
        .unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::VisualLine))
        .await
        .unwrap();

    type_normal_keys(&mut harness, "p").await;

    harness.assert_buffer_contents(
        "line-01\nline-02\nreplacement-a\nreplacement-b\nreplacement-c\nline-04\nline-05\nline-06\nline-07\nline-08",
    );
    harness.assert_cursor_at(0, 2);
    harness.assert_mode(Mode::Normal);
}

#[tokio::test]
async fn visual_uppercase_p_preserves_system_clipboard() {
    let clipboard_text = Arc::new(Mutex::new(Some("replacement".to_string())));
    let buffer = Buffer::new(None, "one\ntwo\nthree".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    harness
        .editor
        .test_set_clipboard(Box::new(MemoryClipboardProvider::from(
            clipboard_text.clone(),
        )));

    type_normal_keys(&mut harness, "ggVGP").await;

    harness.assert_buffer_contents("replacement");
    harness.assert_mode(Mode::Normal);
    assert_eq!(
        clipboard_text.lock().unwrap().as_deref(),
        Some("replacement")
    );
}

#[tokio::test]
async fn visual_paste_replaces_and_captures_a_unicode_grapheme() {
    let family = "👨‍👩‍👧‍👦";
    let clipboard_text = Arc::new(Mutex::new(Some("X".to_string())));
    let buffer = Buffer::new(None, format!("a{family}b"));
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    harness
        .editor
        .test_set_clipboard(Box::new(MemoryClipboardProvider::from(
            clipboard_text.clone(),
        )));
    harness.execute_action(Action::MoveRight).await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Visual))
        .await
        .unwrap();

    type_normal_keys(&mut harness, "p").await;

    harness.assert_buffer_contents("aXb");
    harness.assert_cursor_at(1, 0);
    assert_eq!(clipboard_text.lock().unwrap().as_deref(), Some(family));
}

#[tokio::test]
async fn visual_paste_matches_selection_and_register_kinds() {
    let sources = [
        Content::charwise("Q".to_string()),
        Content::linewise("X\nY\n".to_string()),
        Content::blockwise("XY\nUV\n".to_string()),
    ];

    for ((source, expected), cursor) in sources
        .iter()
        .cloned()
        .zip([
            "pre Q post\nsecond\nthird",
            "pre \nX\nY\n post\nsecond\nthird",
            "pre XY post\nsecoUVnd\nthird",
        ])
        .zip([(4, 0), (0, 1), (4, 0)])
    {
        let mut harness = EditorHarness::with_config(
            Buffer::new(None, "pre abc post\nsecond\nthird".to_string()),
            default_key_config(),
        );
        harness.editor.test_set_default_register(source);
        for _ in 0..4 {
            harness.execute_action(Action::MoveRight).await.unwrap();
        }
        harness
            .execute_action(Action::EnterMode(Mode::Visual))
            .await
            .unwrap();
        for _ in 0..2 {
            harness.execute_action(Action::MoveRight).await.unwrap();
        }

        harness.execute_action(Action::Paste).await.unwrap();

        harness.assert_buffer_contents(expected);
        harness.assert_cursor_at(cursor.0, cursor.1);
    }

    for (source, expected) in
        sources
            .iter()
            .cloned()
            .zip(["one\nQ\nfour", "one\nX\nY\nfour", "one\nXY\nUV\nfour"])
    {
        let mut harness = EditorHarness::with_config(
            Buffer::new(None, "one\ntwo\nthree\nfour".to_string()),
            default_key_config(),
        );
        harness.editor.test_set_default_register(source);
        harness.execute_action(Action::MoveDown).await.unwrap();
        harness
            .execute_action(Action::EnterMode(Mode::VisualLine))
            .await
            .unwrap();
        harness.execute_action(Action::MoveDown).await.unwrap();

        harness.execute_action(Action::Paste).await.unwrap();

        harness.assert_buffer_contents(expected);
        harness.assert_cursor_at(0, 1);
    }

    for ((source, expected), cursor) in sources
        .into_iter()
        .zip([
            "Q11zz\nQ22yy\nQ33xx",
            "11zz\n22yy\n33xx\nX\nY",
            "XY11zz\nUV22yy\n33xx",
        ])
        .zip([(0, 0), (0, 3), (0, 0)])
    {
        let mut harness = EditorHarness::with_config(
            Buffer::new(None, "aa11zz\nbb22yy\ncc33xx".to_string()),
            default_key_config(),
        );
        harness.editor.test_set_default_register(source);
        harness
            .execute_action(Action::EnterMode(Mode::VisualBlock))
            .await
            .unwrap();
        harness.execute_action(Action::MoveRight).await.unwrap();
        harness.execute_action(Action::MoveDown).await.unwrap();
        harness.execute_action(Action::MoveDown).await.unwrap();

        harness.execute_action(Action::Paste).await.unwrap();

        harness.assert_buffer_contents(expected);
        harness.assert_cursor_at(cursor.0, cursor.1);
    }
}

#[tokio::test]
async fn visual_paste_emits_one_change_notification() {
    let path = temp_file_path("visual-paste-lsp");
    let lsp = RecordingLsp::default();
    let events = lsp.events();
    let buffer = Buffer::new(Some(path.clone()), "one\ntwo\nthree".to_string());
    let mut editor = Editor::with_size(
        Box::new(lsp),
        80,
        24,
        default_key_config(),
        Theme::default(),
        vec![buffer],
    )
    .unwrap();
    editor.test_disable_terminal_output();
    editor.test_set_clipboard(Box::new(MemoryClipboardProvider::default()));
    editor.test_set_default_register(Content::charwise("replacement".to_string()));
    let mut harness = EditorHarness { editor };
    harness
        .execute_action(Action::EnterMode(Mode::VisualLine))
        .await
        .unwrap();
    harness.execute_action(Action::MoveDown).await.unwrap();

    harness.execute_action(Action::Paste).await.unwrap();

    let did_change_count = events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| matches!(event, LspEvent::DidChange(file) if file == &path))
        .count();
    assert_eq!(did_change_count, 1);

    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn bracketed_paste_inserts_multiline_text_once() {
    let path = temp_file_path("bracketed-paste-lsp");
    let lsp = RecordingLsp::default();
    let events = lsp.events();
    let buffer = Buffer::new(Some(path.clone()), "\n".to_string());
    let mut editor = Editor::with_size(
        Box::new(lsp),
        80,
        24,
        default_key_config(),
        Theme::default(),
        vec![buffer],
    )
    .unwrap();
    editor.test_disable_terminal_output();
    let mut harness = EditorHarness { editor };
    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();

    harness
        .execute_event(Event::Paste("alpha\r\nbeta 👋".to_string()))
        .await
        .unwrap();

    harness.assert_buffer_contents("alpha\nbeta 👋\n");
    harness.assert_cursor_at(6, 1);
    let did_change_count = events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| matches!(event, LspEvent::DidChange(file) if file == &path))
        .count();
    assert_eq!(did_change_count, 1);

    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("\n");

    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn disabled_lsp_skips_document_change_notifications() {
    let path = temp_file_path("disabled-lsp-change");
    let lsp = RecordingLsp::default();
    let events = lsp.events();
    let mut config = default_key_config();
    config.lsp.enabled = false;
    let buffer = Buffer::new(Some(path.clone()), "text".to_string());
    let mut editor = Editor::with_size(
        Box::new(lsp),
        80,
        24,
        config,
        Theme::default(),
        vec![buffer],
    )
    .unwrap();
    editor.test_disable_terminal_output();
    let mut harness = EditorHarness { editor };

    harness
        .execute_action(Action::InsertCharAtCursorPos('x'))
        .await
        .unwrap();

    harness.assert_buffer_contents("xtext");
    assert!(events.lock().unwrap().iter().all(|event| {
        !matches!(event, LspEvent::DidOpen(file) | LspEvent::DidChange(file) if file == &path)
    }));
}

#[tokio::test]
async fn bracketed_paste_uses_first_line_in_command_mode() {
    let mut harness = EditorHarness::with_content("safe");
    harness.set_commandline(Mode::Command, "");

    harness
        .execute_event(Event::Paste("q\r\nj".to_string()))
        .await
        .unwrap();

    assert_eq!(harness.commandline_text(), "q");
    harness.assert_mode(Mode::Command);
}

#[tokio::test]
async fn bracketed_paste_uses_first_line_in_search_mode() {
    let mut harness = EditorHarness::with_content("alpha beta");
    harness
        .execute_action(Action::EnterSearch(SearchDirection::Forward))
        .await
        .unwrap();

    harness
        .execute_event(Event::Paste("alpha\r\nbeta".to_string()))
        .await
        .unwrap();

    assert_eq!(harness.commandline_text(), "alpha");
    harness.assert_mode(Mode::Search);
}

#[tokio::test]
async fn bracketed_paste_is_ignored_in_normal_mode() {
    let mut harness = EditorHarness::with_content("safe");

    harness
        .execute_event(Event::Paste("iddanger".to_string()))
        .await
        .unwrap();

    harness.assert_buffer_contents("safe");
    harness.assert_mode(Mode::Normal);
}

#[tokio::test]
async fn bracketed_paste_cancels_pending_normal_key_sequence() {
    let mut harness = EditorHarness::with_content("safe word");
    type_normal_keys(&mut harness, "d").await;
    assert!(harness.is_waiting_for_key_sequence());

    harness
        .execute_event(Event::Paste("ignored".to_string()))
        .await
        .unwrap();
    type_normal_keys(&mut harness, "w").await;

    assert!(!harness.is_waiting_for_key_sequence());
    harness.assert_buffer_contents("safe word");
    harness.assert_mode(Mode::Normal);
}

#[tokio::test]
async fn test_undo_paste_and_paste_before() {
    let mut harness = EditorHarness::with_content("hello world");

    harness
        .execute_action(Action::EnterMode(Mode::Visual))
        .await
        .unwrap();
    for _ in 0..5 {
        harness.execute_action(Action::MoveRight).await.unwrap();
    }
    harness.execute_action(Action::Delete).await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
    harness.assert_buffer_contents("world");
    harness.execute_action(Action::MoveToLineEnd).await.unwrap();
    harness.execute_action(Action::Paste).await.unwrap();
    harness.assert_buffer_contents("worldhello ");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("world");

    harness
        .execute_action(Action::MoveToLineStart)
        .await
        .unwrap();
    harness.execute_action(Action::PasteBefore).await.unwrap();
    harness.assert_buffer_contents("hello world");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("world");
}

#[tokio::test]
async fn test_undo_insert_text_action() {
    let mut harness = EditorHarness::with_content("abc");
    let content = Content::charwise("ZZ".to_string());

    harness
        .execute_action(Action::InsertText {
            x: 1,
            y: 0,
            content,
        })
        .await
        .unwrap();
    harness.assert_buffer_contents("aZZbc");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("abc");
}

#[tokio::test]
async fn test_undo_history_is_per_buffer() {
    let lsp = Box::new(MockLsp) as Box<dyn LspClient + Send>;
    let config = Config::default();
    let theme = Theme::default();
    let buffers = vec![
        Buffer::new(None, "one".to_string()),
        Buffer::new(None, "two".to_string()),
    ];
    let mut editor = Editor::with_size(lsp, 80, 24, config, theme, buffers).unwrap();
    editor.test_disable_terminal_output();
    let mut harness = EditorHarness { editor };

    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    harness.assert_buffer_contents("ne");
    harness.execute_action(Action::NextBuffer).await.unwrap();
    harness.assert_buffer_contents("two");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("two");
    harness
        .execute_action(Action::PreviousBuffer)
        .await
        .unwrap();
    harness.assert_buffer_contents("ne");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("one");
}

#[tokio::test]
async fn test_buffer_delete_removes_current_buffer_from_list() {
    let lsp = Box::new(MockLsp) as Box<dyn LspClient + Send>;
    let config = Config::default();
    let theme = Theme::default();
    let buffers = vec![
        Buffer::new(Some("one.rs".to_string()), "one".to_string()),
        Buffer::new(Some("two.rs".to_string()), "two".to_string()),
        Buffer::new(Some("three.rs".to_string()), "three".to_string()),
    ];
    let mut editor = Editor::with_size(lsp, 80, 24, config, theme, buffers).unwrap();
    editor.test_disable_terminal_output();
    let mut harness = EditorHarness { editor };

    harness.execute_action(Action::NextBuffer).await.unwrap();
    harness
        .execute_action(Action::Command("bd".to_string()))
        .await
        .unwrap();

    assert_eq!(harness.buffer_names(), vec!["one.rs", "three.rs"]);
    assert_eq!(harness.current_buffer_index(), 1);
    harness.assert_buffer_contents("three");
}

#[tokio::test]
async fn test_buffer_delete_requires_force_for_dirty_buffer() {
    let lsp = Box::new(MockLsp) as Box<dyn LspClient + Send>;
    let config = Config::default();
    let theme = Theme::default();
    let buffers = vec![
        Buffer::new(Some("one.rs".to_string()), "one".to_string()),
        Buffer::new(Some("two.rs".to_string()), "two".to_string()),
    ];
    let mut editor = Editor::with_size(lsp, 80, 24, config, theme, buffers).unwrap();
    editor.test_disable_terminal_output();
    let mut harness = EditorHarness { editor };

    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    harness
        .execute_action(Action::Command("bd".to_string()))
        .await
        .unwrap();

    assert_eq!(harness.buffer_names(), vec!["one.rs", "two.rs"]);
    assert_eq!(
        harness.last_error(),
        Some("No write since last change (add ! to override)")
    );
    harness.assert_buffer_contents("ne");

    harness
        .execute_action(Action::Command("bd!".to_string()))
        .await
        .unwrap();

    assert_eq!(harness.buffer_names(), vec!["two.rs"]);
    harness.assert_buffer_contents("two");
}

#[tokio::test]
async fn test_preview_theme_reports_missing_theme_without_changing_buffer() {
    let mut harness = EditorHarness::with_content("abc");

    harness
        .execute_action(Action::PreviewTheme(
            "definitely-missing-theme.json".to_string(),
        ))
        .await
        .unwrap();

    assert_eq!(
        harness.last_error(),
        Some("Theme file definitely-missing-theme.json not found")
    );
    harness.assert_buffer_contents("abc");
}

#[tokio::test]
async fn test_dirty_clears_when_undo_returns_to_clean_revision() {
    let mut harness = EditorHarness::with_content("abc");
    assert!(!harness.is_dirty());

    harness
        .execute_action(Action::InsertCharAtCursorPos('z'))
        .await
        .unwrap();
    assert!(harness.is_dirty());

    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("abc");
    assert!(!harness.is_dirty());

    harness.execute_action(Action::Redo).await.unwrap();
    harness.assert_buffer_contents("zabc");
    assert!(harness.is_dirty());
}

#[tokio::test]
async fn test_dirty_checkpoint_moves_after_save() {
    let path = temp_file_path("dirty-save");
    fs::write(&path, "abc").unwrap();

    let buffer = Buffer::new(Some(path.clone()), "abc".to_string());
    let mut harness = EditorHarness::with_buffer(buffer);

    harness
        .execute_action(Action::InsertCharAtCursorPos('z'))
        .await
        .unwrap();
    assert!(harness.is_dirty());
    harness.execute_action(Action::Save).await.unwrap();
    assert!(!harness.is_dirty());

    harness
        .execute_action(Action::InsertCharAtCursorPos('y'))
        .await
        .unwrap();
    assert!(harness.is_dirty());
    harness.execute_action(Action::Undo).await.unwrap();
    assert!(!harness.is_dirty());

    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn test_save_during_insert_keeps_saved_buffer_clean_on_escape() {
    let path = temp_file_path("dirty-save-insert");
    fs::write(&path, "abc").unwrap();

    let buffer = Buffer::new(Some(path.clone()), "abc".to_string());
    let mut harness = EditorHarness::with_buffer(buffer);

    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.type_text("z").await.unwrap();
    assert!(harness.is_dirty());

    harness.execute_action(Action::Save).await.unwrap();
    assert!(!harness.is_dirty());

    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
    harness.assert_buffer_contents("zabc");
    assert!(!harness.is_dirty());

    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn test_dirty_remains_after_undoing_past_saved_revision() {
    let path = temp_file_path("dirty-past-save");
    fs::write(&path, "abc").unwrap();

    let buffer = Buffer::new(Some(path.clone()), "abc".to_string());
    let mut harness = EditorHarness::with_buffer(buffer);

    harness
        .execute_action(Action::InsertCharAtCursorPos('z'))
        .await
        .unwrap();
    harness.execute_action(Action::Save).await.unwrap();
    assert!(!harness.is_dirty());

    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("abc");
    assert!(harness.is_dirty());

    let _ = fs::remove_file(path);
}

#[test]
fn test_right_panel_reserves_editor_window_width() {
    let mut harness = EditorHarness::with_content("abcdef");

    harness.editor.test_create_panel(
        "tree",
        PanelConfig {
            side: PanelSide::Right,
            width: 20,
            title: None,
            composer: None,
            header_actions: Vec::new(),
        },
    );

    let (position, size) = harness.editor.test_active_window_bounds().unwrap();
    assert_eq!(position.x, 0);
    assert_eq!(size.0, 59);
}

#[test]
fn focused_panel_hides_editor_cursor_until_focus_returns() {
    let mut harness = EditorHarness::with_content("abcdef");
    let editor_cursor = harness.render_cursor_position();
    add_tree_panel(&mut harness);

    assert!(harness.editor.test_focus_panel("tree"));
    assert_eq!(harness.render_cursor_position(), None);

    harness.editor.test_close_panel("tree");
    assert_eq!(harness.render_cursor_position(), editor_cursor);
}

#[tokio::test]
async fn focused_panel_commandline_receives_text_before_panel_shortcuts() {
    let buffer = Buffer::new(None, "abcdef".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    add_tree_panel(&mut harness);
    assert!(harness.editor.test_focus_panel("tree"));

    harness
        .execute_event(Event::Key(KeyEvent::new(
            KeyCode::Char(':'),
            KeyModifiers::NONE,
        )))
        .await
        .unwrap();
    harness.assert_mode(Mode::Command);

    harness
        .execute_event(Event::Key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
        )))
        .await
        .unwrap();

    assert_eq!(harness.commandline_text(), "q");
    assert_eq!(harness.editor.test_focused_panel_id(), Some("tree"));
}

#[tokio::test]
async fn focused_panel_does_not_fall_through_to_editing_keys() {
    let buffer = Buffer::new(None, "abcdef".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    add_tree_panel(&mut harness);
    assert!(harness.editor.test_focus_panel("tree"));

    harness
        .execute_event(Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        )))
        .await
        .unwrap();

    harness.assert_buffer_contents("abcdef");
    assert_eq!(harness.editor.test_focused_panel_id(), Some("tree"));
}

#[test]
fn focused_panel_allows_ctrl_e_neotree_toggle() {
    let buffer = Buffer::new(None, "abcdef".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    add_tree_panel(&mut harness);
    assert!(harness.editor.test_focus_panel("tree"));

    let action = harness
        .editor
        .test_handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('e'),
            KeyModifiers::CONTROL,
        )))
        .unwrap();

    assert_eq!(
        action,
        Some(KeyAction::Single(Action::PluginCommand(
            "NeoTree".to_string()
        )))
    );
}

#[test]
fn focused_agent_panel_keeps_leader_available_until_the_composer_is_focused() {
    let buffer = Buffer::new(None, "abcdef".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    harness.editor.test_create_text_panel(
        "agent",
        PanelConfig {
            side: PanelSide::Right,
            width: 40,
            title: Some("Agent".to_string()),
            composer: Some(TextPanelComposerConfig {
                placeholder: "Ask".to_string(),
                rows: 2,
            }),
            header_actions: Vec::new(),
        },
    );
    assert!(harness.editor.test_focus_panel("agent"));

    let action = harness
        .editor
        .test_handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char(' '),
            KeyModifiers::NONE,
        )))
        .unwrap();
    let Some(KeyAction::Nested(leader)) = action else {
        panic!("expected Space to start the leader sequence from the conversation, got {action:?}");
    };
    assert_eq!(
        leader.get("A"),
        Some(&KeyAction::Single(Action::PluginCommand(
            "Agent".to_string()
        )))
    );

    assert!(harness.editor.test_focus_text_panel_composer("agent"));
    let action = harness
        .editor
        .test_handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char(' '),
            KeyModifiers::NONE,
        )))
        .unwrap();
    assert!(matches!(
        action,
        Some(KeyAction::Multiple(actions))
            if actions.iter().any(|action| matches!(
                action,
                Action::NotifyPlugins(name, payload)
                    if name == "panel:event:agent" && payload["action"] == "composer_input"
            ))
    ));
    assert!(harness.render_cursor_position().is_some());
}

#[tokio::test]
async fn escape_from_focused_panel_restores_editor_cursor() {
    let mut harness = EditorHarness::with_content("abcdef");
    add_tree_panel(&mut harness);
    let editor_cursor = harness.render_cursor_position();
    assert!(harness.editor.test_focus_panel("tree"));

    harness
        .execute_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
        .await
        .unwrap();

    assert_eq!(harness.editor.test_focused_panel_id(), None);
    assert_eq!(harness.render_cursor_position(), editor_cursor);
}

#[tokio::test]
async fn next_and_previous_window_cycle_through_focused_panels() {
    let mut harness = EditorHarness::with_content("abcdef");
    add_tree_panel(&mut harness);

    harness.execute_action(Action::NextWindow).await.unwrap();
    assert_eq!(harness.editor.test_focused_panel_id(), Some("tree"));

    harness.execute_action(Action::NextWindow).await.unwrap();
    assert_eq!(harness.editor.test_focused_panel_id(), None);

    harness
        .execute_action(Action::PreviousWindow)
        .await
        .unwrap();
    assert_eq!(harness.editor.test_focused_panel_id(), Some("tree"));
}

#[tokio::test]
async fn window_cycle_uses_left_windows_right_visual_groups() {
    let mut harness = EditorHarness::with_content("abcdef");
    add_tree_panel(&mut harness);
    harness.editor.test_create_panel(
        "right",
        PanelConfig {
            side: PanelSide::Right,
            width: 20,
            title: None,
            composer: None,
            header_actions: Vec::new(),
        },
    );
    harness.execute_action(Action::SplitVertical).await.unwrap();
    assert_eq!(harness.active_window_id(), 1);

    harness.execute_action(Action::NextWindow).await.unwrap();
    assert_eq!(harness.editor.test_focused_panel_id(), Some("right"));

    harness.execute_action(Action::NextWindow).await.unwrap();
    assert_eq!(harness.editor.test_focused_panel_id(), Some("tree"));

    harness.execute_action(Action::NextWindow).await.unwrap();
    assert_eq!(harness.editor.test_focused_panel_id(), None);
    assert_eq!(harness.active_window_id(), 0);
}

#[tokio::test]
async fn focused_panel_routes_ctrl_w_w_into_focus_cycle() {
    let buffer = Buffer::new(None, "abcdef".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    add_tree_panel(&mut harness);
    assert!(harness.editor.test_focus_panel("tree"));

    harness
        .execute_event(Event::Key(KeyEvent::new(
            KeyCode::Char('w'),
            KeyModifiers::CONTROL,
        )))
        .await
        .unwrap();
    harness
        .execute_event(Event::Key(KeyEvent::new(
            KeyCode::Char('w'),
            KeyModifiers::NONE,
        )))
        .await
        .unwrap();

    assert_eq!(harness.editor.test_focused_panel_id(), None);
}

#[tokio::test]
async fn ctrl_w_w_focuses_agent_composer_and_makes_cursor_visible() {
    let buffer = Buffer::new(None, "abcdef".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    harness.editor.test_create_text_panel(
        "agent",
        PanelConfig {
            side: PanelSide::Right,
            width: 40,
            title: Some("Agent".to_string()),
            composer: Some(TextPanelComposerConfig {
                placeholder: "Ask".to_string(),
                rows: 2,
            }),
            header_actions: Vec::new(),
        },
    );
    let editor_cursor = harness.render_cursor_position();

    harness
        .execute_event(Event::Key(KeyEvent::new(
            KeyCode::Char('w'),
            KeyModifiers::CONTROL,
        )))
        .await
        .unwrap();
    harness
        .execute_event(Event::Key(KeyEvent::new(
            KeyCode::Char('w'),
            KeyModifiers::NONE,
        )))
        .await
        .unwrap();

    assert_eq!(harness.editor.test_focused_panel_id(), Some("agent"));
    let composer_cursor = harness.render_cursor_position();
    assert!(composer_cursor.is_some());
    assert_ne!(composer_cursor, editor_cursor);

    let action = harness
        .editor
        .test_handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        )))
        .unwrap();
    assert!(matches!(
        action,
        Some(KeyAction::Multiple(actions))
            if actions.iter().any(|action| matches!(
                action,
                Action::NotifyPlugins(name, payload)
                    if name == "panel:event:agent" && payload["action"] == "composer_input"
            ))
    ));

    let action = harness
        .editor
        .test_handle_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
        .unwrap();
    assert!(matches!(
        action,
        Some(KeyAction::Multiple(actions))
            if actions.iter().any(|action| matches!(
                action,
                Action::NotifyPlugins(name, payload)
                    if name == "panel:event:agent" && payload["action"] == "composer_blur"
            ))
    ));
    assert_eq!(harness.editor.test_focused_panel_id(), Some("agent"));
    assert_eq!(harness.render_cursor_position(), None);
}

#[tokio::test]
async fn mouse_click_inside_panel_focuses_and_selects_row() {
    let mut harness = EditorHarness::with_content("abcdef");
    add_tree_panel(&mut harness);

    harness
        .execute_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 2,
            modifiers: KeyModifiers::NONE,
        }))
        .await
        .unwrap();

    assert_eq!(harness.editor.test_focused_panel_id(), Some("tree"));
    assert_eq!(
        harness.editor.test_focused_panel_selected_index("tree"),
        Some(2)
    );
    assert_eq!(harness.render_cursor_position(), None);
}

#[tokio::test]
async fn mouse_click_in_editor_clears_panel_focus() {
    let mut harness = EditorHarness::with_content("abcdef");
    add_tree_panel(&mut harness);
    assert!(harness.editor.test_focus_panel("tree"));

    harness
        .execute_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 25,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }))
        .await
        .unwrap();

    assert_eq!(harness.editor.test_focused_panel_id(), None);
    assert!(harness.render_cursor_position().is_some());
}

#[test]
fn passive_mouse_events_over_editor_do_not_clear_focused_agent_composer() {
    let buffer = Buffer::new(None, "abcdef".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    harness.editor.test_create_text_panel(
        "agent",
        PanelConfig {
            side: PanelSide::Right,
            width: 40,
            title: Some("Agent".to_string()),
            composer: Some(TextPanelComposerConfig {
                placeholder: "Ask".to_string(),
                rows: 2,
            }),
            header_actions: Vec::new(),
        },
    );
    assert!(harness.editor.test_focus_text_panel_composer("agent"));
    let cursor = harness.render_cursor_position();

    for kind in [
        MouseEventKind::Moved,
        MouseEventKind::Up(MouseButton::Left),
        MouseEventKind::Drag(MouseButton::Left),
    ] {
        let action = harness
            .editor
            .test_handle_event(Event::Mouse(MouseEvent {
                kind,
                column: 10,
                row: 1,
                modifiers: KeyModifiers::NONE,
            }))
            .unwrap();

        assert_eq!(action, None);
        assert_eq!(harness.editor.test_focused_panel_id(), Some("agent"));
        assert_eq!(harness.render_cursor_position(), cursor);
    }
}

#[tokio::test]
async fn only_window_hides_auxiliary_panels_and_preserves_agent_draft() {
    let buffer = Buffer::new(None, "abcdef".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    add_tree_panel(&mut harness);
    harness.editor.test_create_text_panel(
        "agent",
        PanelConfig {
            side: PanelSide::Right,
            width: 24,
            title: Some("Agent".to_string()),
            composer: Some(TextPanelComposerConfig {
                placeholder: "Ask".to_string(),
                rows: 2,
            }),
            header_actions: Vec::new(),
        },
    );
    assert!(harness.editor.test_focus_text_panel_composer("agent"));
    harness
        .editor
        .test_handle_event(Event::Paste("keep this follow-up".to_string()))
        .unwrap();
    harness.execute_action(Action::SplitVertical).await.unwrap();
    harness
        .execute_action(Action::PreviousWindow)
        .await
        .unwrap();
    assert_eq!(harness.editor.test_focused_panel_id(), None);

    harness
        .execute_event(Event::Key(KeyEvent::new(
            KeyCode::Char('w'),
            KeyModifiers::CONTROL,
        )))
        .await
        .unwrap();
    harness
        .execute_event(Event::Key(KeyEvent::new(
            KeyCode::Char('o'),
            KeyModifiers::NONE,
        )))
        .await
        .unwrap();

    assert_eq!(harness.window_count(), 1);
    assert_eq!(harness.editor.test_focused_panel_id(), None);
    assert!(!harness.editor.test_focus_panel("tree"));
    assert!(!harness.editor.test_focus_text_panel_composer("agent"));
    assert_eq!(
        harness.editor.test_active_window_bounds(),
        Some((red::editor::Point::new(0, 0), (80, 22)))
    );

    assert!(harness.editor.test_set_panel_visible("agent", true));
    assert!(harness.editor.test_focus_text_panel_composer("agent"));
    assert!((0..24).any(|row| {
        harness
            .editor
            .test_render_row(row)
            .unwrap()
            .contains("keep this follow-up")
    }));
}

#[tokio::test]
async fn test_dirty_isolated_per_buffer() {
    let lsp = Box::new(MockLsp) as Box<dyn LspClient + Send>;
    let config = Config::default();
    let theme = Theme::default();
    let buffers = vec![
        Buffer::new(None, "one".to_string()),
        Buffer::new(None, "two".to_string()),
    ];
    let mut editor = Editor::with_size(lsp, 80, 24, config, theme, buffers).unwrap();
    editor.test_disable_terminal_output();
    let mut harness = EditorHarness { editor };

    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    assert!(harness.is_dirty());

    harness.execute_action(Action::NextBuffer).await.unwrap();
    assert!(!harness.is_dirty());
    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    assert!(harness.is_dirty());
    harness.execute_action(Action::Undo).await.unwrap();
    assert!(!harness.is_dirty());

    harness
        .execute_action(Action::PreviousBuffer)
        .await
        .unwrap();
    assert!(harness.is_dirty());
    harness.execute_action(Action::Undo).await.unwrap();
    assert!(!harness.is_dirty());
}

#[tokio::test]
async fn test_paste() {
    let mut harness = EditorHarness::with_content("Hello World");

    // Delete a word (should be yanked to clipboard)
    harness.execute_action(Action::DeleteWord).await.unwrap();
    harness.assert_buffer_contents("World");

    // Move to end and paste with 'p'
    harness.execute_action(Action::MoveToLineEnd).await.unwrap();
    harness.execute_action(Action::Paste).await.unwrap();
    // This depends on clipboard/register implementation
    // For now, let's just verify it doesn't crash
}

#[tokio::test]
async fn test_yank_and_paste() {
    let mut harness = EditorHarness::with_content("Line 1\nLine 2\nLine 3");

    // Yank action exists
    harness.execute_action(Action::Yank).await.unwrap();

    // Move down and paste
    harness.execute_action(Action::MoveDown).await.unwrap();
    harness.execute_action(Action::Paste).await.unwrap();
    // This depends on clipboard/register implementation
}

#[tokio::test]
async fn test_direct_open_line_below_groups_insert_undo() {
    let mut harness = EditorHarness::with_content("Line 1\nLine 2");

    harness
        .execute_action(Action::InsertLineBelowCursor)
        .await
        .unwrap();
    harness.type_text("New line").await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();

    harness.assert_buffer_contents("Line 1\nNew line\nLine 2");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("Line 1\nLine 2");
}

#[tokio::test]
async fn test_editing_empty_buffer() {
    let mut harness = EditorHarness::new();

    // Enter insert mode in empty buffer
    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.type_text("First line").await.unwrap();
    harness.assert_buffer_contents("First line\n");

    // Exit and create new line below
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
    harness
        .execute_action(Action::InsertLineBelowCursor)
        .await
        .unwrap();
    harness.type_text("Second line").await.unwrap();
    harness.assert_buffer_contents("First line\nSecond line\n");
}

#[tokio::test]
async fn test_delete_at_end_of_file() {
    let mut harness = EditorHarness::with_content("Line 1\nLine 2");

    // Move to last line
    harness.execute_action(Action::MoveToBottom).await.unwrap();
    println!(
        "After MoveToBottom: cursor at {:?}",
        harness.cursor_position()
    );
    println!("Current line: {:?}", harness.current_line());

    // Try to delete line at end of file
    harness
        .execute_action(Action::DeleteCurrentLine)
        .await
        .unwrap();
    println!("After delete: {:?}", harness.buffer_contents());
    harness.assert_buffer_contents("Line 1\n");
}

#[tokio::test]
async fn test_change_to_end_of_line() {
    let mut harness = EditorHarness::with_config(
        Buffer::new(None, "Hello World Test".to_string()),
        default_key_config(),
    );

    type_normal_keys(&mut harness, "wC").await;
    harness.assert_mode(Mode::Insert);
    harness.type_text("Universe").await.unwrap();
    harness.assert_buffer_contents("Hello Universe");
}

#[tokio::test]
async fn vim_editing_shortcuts_honor_counts_and_register_kinds() {
    let cases = [
        ("one two\nthree four\nfive", "w2D", "one \nfive"),
        ("one two\nthree four\nfive", "w2CX", "one X\nfive"),
        ("  one two\nnext", "SX", "  X\nnext"),
        ("one two", "w2sX", "one Xo"),
        ("one two", "wX", "onetwo"),
        ("one two", "wY$p", "one twotwo"),
        ("abc", "xp", "bac"),
        ("abc", "xuU", "bc"),
    ];

    for (contents, keys, expected) in cases {
        let mut harness = EditorHarness::with_config(
            Buffer::new(None, contents.to_string()),
            default_key_config(),
        );

        type_normal_keys(&mut harness, keys).await;
        if harness.is_insert() {
            command_key(&mut harness, KeyCode::Esc).await;
        }

        harness.assert_buffer_contents(expected);
    }
}

#[tokio::test]
async fn vim_operator_motions_cover_line_edges_backward_words_and_vertical_lines() {
    for (contents, keys, expected) in [
        ("one two three", "wd$", "one "),
        ("one two three", "wdb", "two three"),
        ("one\ntwo\nthree", "dj", "three"),
        ("one\ntwo\nthree", "jdk", "three"),
        ("one two three", "cwX", "X two three"),
        ("one two three four", "c2wX", "X three four"),
        ("one two", "wcwX", "one X"),
        ("  one\n    two\nthree", "cjX", "  X\nthree"),
    ] {
        let mut harness = EditorHarness::with_config(
            Buffer::new(None, contents.to_string()),
            default_key_config(),
        );

        type_normal_keys(&mut harness, keys).await;
        if harness.is_insert() {
            command_key(&mut harness, KeyCode::Esc).await;
        }

        harness.assert_buffer_contents(expected);
    }
}

#[tokio::test]
async fn vim_line_end_changes_repeat_at_the_new_cursor() {
    for (keys, expected) in [("wCX", "one X\nthree X"), ("wD", "one \nthree ")] {
        let mut harness = EditorHarness::with_config(
            Buffer::new(None, "one two\nthree four".to_string()),
            default_key_config(),
        );
        type_normal_keys(&mut harness, keys).await;
        if harness.is_insert() {
            command_key(&mut harness, KeyCode::Esc).await;
        }
        type_normal_keys(&mut harness, "jw.").await;
        if harness.is_insert() {
            command_key(&mut harness, KeyCode::Esc).await;
        }
        harness.assert_buffer_contents(expected);
    }
}

#[tokio::test]
async fn vim_backward_character_and_end_word_motions_work_in_normal_and_operator_modes() {
    let mut harness = EditorHarness::with_config(
        Buffer::new(None, "alpha.beta.gamma".to_string()),
        default_key_config(),
    );
    type_normal_keys(&mut harness, "$F.").await;
    harness.assert_cursor_at(10, 0);

    let mut harness = EditorHarness::with_config(
        Buffer::new(None, "alpha.beta.gamma".to_string()),
        default_key_config(),
    );
    type_normal_keys(&mut harness, "$T.").await;
    harness.assert_cursor_at(11, 0);

    let mut harness = EditorHarness::with_config(
        Buffer::new(None, "alpha.beta.gamma".to_string()),
        default_key_config(),
    );
    type_normal_keys(&mut harness, "f.f.,").await;
    harness.assert_cursor_at(5, 0);

    for (contents, keys, expected) in [
        ("alpha.beta.gamma", "$dF.", "alpha.betaa"),
        ("alpha.beta.gamma", "$dT.", "alpha.beta.a"),
        ("alpha beta gamma", "de", " beta gamma"),
        ("alpha beta gamma", "wdb", "beta gamma"),
    ] {
        let mut harness = EditorHarness::with_config(
            Buffer::new(None, contents.to_string()),
            default_key_config(),
        );
        type_normal_keys(&mut harness, keys).await;
        harness.assert_buffer_contents(expected);
    }

    for (keys, cursor) in [("e", 4), ("E", 9), ("$ge", 9), ("$gE", 9), ("$B", 11)] {
        let mut harness = EditorHarness::with_config(
            Buffer::new(None, "alpha.beta gamma".to_string()),
            default_key_config(),
        );
        type_normal_keys(&mut harness, keys).await;
        harness.assert_cursor_at(cursor, 0);
    }
}

#[tokio::test]
async fn vim_case_changes_and_visual_replace_are_transactional() {
    for (contents, keys, expected) in [
        ("alpha beta", "~", "Alpha beta"),
        ("alpha beta", "gUiw", "ALPHA beta"),
        ("ALPHA beta", "guiw", "alpha beta"),
        ("aLpHa beta", "g~iw", "AlPhA beta"),
        ("alpha beta", "viwU", "ALPHA beta"),
        ("ALPHA beta", "viwu", "alpha beta"),
        ("aLpHa beta", "viw~", "AlPhA beta"),
        ("alpha beta", "viwrX", "XXXXX beta"),
    ] {
        let mut harness = EditorHarness::with_config(
            Buffer::new(None, contents.to_string()),
            default_key_config(),
        );

        type_normal_keys(&mut harness, keys).await;

        harness.assert_buffer_contents(expected);
        harness.assert_mode(Mode::Normal);
        type_normal_keys(&mut harness, "u").await;
        harness.assert_buffer_contents(contents);
    }
}

#[tokio::test]
async fn vim_word_and_character_repeat_actions_remain_remappable() {
    let mut config = default_key_config();
    config.keys.normal.insert(
        "W".to_string(),
        KeyAction::Single(Action::MoveToNextBigWord),
    );
    config.keys.normal.insert(
        ";".to_string(),
        KeyAction::Single(Action::RepeatCharSearch(1)),
    );

    let mut harness =
        EditorHarness::with_config(Buffer::new(None, "alpha.beta gamma".to_string()), config);
    type_normal_keys(&mut harness, "W").await;
    harness.assert_cursor_at(11, 0);

    let mut config = default_key_config();
    config.keys.normal.insert(
        ";".to_string(),
        KeyAction::Single(Action::RepeatCharSearch(1)),
    );
    let mut harness =
        EditorHarness::with_config(Buffer::new(None, "alpha.beta.gamma".to_string()), config);
    type_normal_keys(&mut harness, "f.;").await;
    harness.assert_cursor_at(10, 0);
}

#[tokio::test]
async fn visual_replace_accepts_a_shifted_terminal_key_event() {
    let mut harness = EditorHarness::with_config(
        Buffer::new(None, "alpha beta".to_string()),
        default_key_config(),
    );
    type_normal_keys(&mut harness, "viwr").await;

    harness
        .execute_event(Event::Key(KeyEvent::new(
            KeyCode::Char('X'),
            KeyModifiers::SHIFT,
        )))
        .await
        .unwrap();

    harness.assert_buffer_contents("XXXXX beta");
    harness.assert_mode(Mode::Normal);
}

#[tokio::test]
async fn visual_multiline_and_block_replace_and_case_changes_preserve_line_breaks() {
    let cases = [
        ("vjrX", "XXXXX\nXeta\ngamma"),
        ("VjrX", "XXXXX\nXXXX\ngamma"),
        ("VjU", "ALPHA\nBETA\ngamma"),
    ];
    for (keys, expected) in cases {
        let mut harness = EditorHarness::with_config(
            Buffer::new(None, "alpha\nbeta\ngamma".to_string()),
            default_key_config(),
        );
        type_normal_keys(&mut harness, keys).await;
        harness.assert_buffer_contents(expected);
    }

    for (suffix, expected) in [
        ("jlrX", "XXpha\nXXta\ngamma"),
        ("jlU", "ALpha\nBEta\ngamma"),
    ] {
        let mut harness = EditorHarness::with_config(
            Buffer::new(None, "alpha\nbeta\ngamma".to_string()),
            default_key_config(),
        );
        harness
            .execute_event(Event::Key(KeyEvent::new(
                KeyCode::Char('v'),
                KeyModifiers::CONTROL,
            )))
            .await
            .unwrap();
        type_normal_keys(&mut harness, suffix).await;
        harness.assert_buffer_contents(expected);
    }
}

#[tokio::test]
async fn vim_half_page_keys_move_the_cursor_by_half_a_viewport() {
    let contents = (0..40)
        .map(|line| format!("line-{line:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut harness = EditorHarness::with_config_and_size(
        Buffer::new(None, contents),
        default_key_config(),
        80,
        12,
    );

    harness
        .execute_event(Event::Key(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
        )))
        .await
        .unwrap();
    assert_eq!(harness.buffer_line(), 5);

    harness
        .execute_event(Event::Key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        )))
        .await
        .unwrap();
    assert_eq!(harness.buffer_line(), 0);
}
