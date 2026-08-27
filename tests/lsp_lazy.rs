mod common;

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use common::{LspEvent, RecordingLsp};
use red::{
    buffer::Buffer,
    config::Config,
    editor::{Action, Editor},
    lsp::LspClient,
    test_utils::EditorTestExt,
    theme::Theme,
};

fn recording_editor(buffers: Vec<Buffer>) -> (Editor, Arc<Mutex<Vec<LspEvent>>>) {
    let lsp = RecordingLsp::default();
    let events = lsp.events();
    let lsp = Box::new(lsp) as Box<dyn LspClient + Send>;
    let config = Config::default();
    let theme = Theme::default();
    let mut editor = Editor::test_with_size(lsp, 80, 24, config, theme, buffers).unwrap();
    editor.test_disable_terminal_output();
    (editor, events)
}

fn recording_workspace_editor(
    root: &Path,
    buffers: Vec<Buffer>,
    format_on_save: bool,
) -> (Editor, Arc<Mutex<Vec<LspEvent>>>) {
    let mut config = Config::default();
    config.formatting.on_save = format_on_save;
    recording_workspace_editor_with_config(root, buffers, config)
}

fn recording_workspace_editor_with_config(
    root: &Path,
    buffers: Vec<Buffer>,
    config: Config,
) -> (Editor, Arc<Mutex<Vec<LspEvent>>>) {
    let lsp = RecordingLsp::with_workspace_root(root);
    let events = lsp.events();
    let lsp = Box::new(lsp) as Box<dyn LspClient + Send>;
    let mut editor =
        Editor::test_with_size(lsp, 80, 24, config, Theme::default(), buffers).unwrap();
    editor.test_disable_terminal_output();
    (editor, events)
}

fn recorded(events: &Arc<Mutex<Vec<LspEvent>>>) -> Vec<LspEvent> {
    events.lock().unwrap().clone()
}

#[tokio::test]
async fn constructing_editor_does_not_open_inactive_lsp_buffer() {
    let (_editor, events) = recording_editor(vec![
        Buffer::new(None, "notes".to_string()),
        Buffer::new(Some("src/main.rs".to_string()), "fn main() {}".to_string()),
    ]);

    assert_eq!(recorded(&events), Vec::<LspEvent>::new());
}

#[tokio::test]
async fn activating_current_lsp_buffer_opens_it_once() {
    let (mut editor, events) = recording_editor(vec![Buffer::new(
        Some("src/main.rs".to_string()),
        "fn main() {}".to_string(),
    )]);

    editor
        .test_ensure_current_buffer_lsp_opened()
        .await
        .unwrap();
    editor
        .test_ensure_current_buffer_lsp_opened()
        .await
        .unwrap();

    assert_eq!(
        recorded(&events),
        vec![LspEvent::DidOpen("src/main.rs".to_string())]
    );
}

#[tokio::test]
async fn switching_to_lsp_buffer_opens_it_without_reopening_on_later_switches() {
    let (mut editor, events) = recording_editor(vec![
        Buffer::new(None, "notes".to_string()),
        Buffer::new(Some("src/main.rs".to_string()), "fn main() {}".to_string()),
    ]);

    editor
        .test_execute_action(Action::NextBuffer)
        .await
        .unwrap();
    editor
        .test_execute_action(Action::PreviousBuffer)
        .await
        .unwrap();
    editor
        .test_execute_action(Action::NextBuffer)
        .await
        .unwrap();

    let events = recorded(&events);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, LspEvent::DidOpen(file) if file == "src/main.rs"))
            .count(),
        1
    );
}

#[tokio::test]
async fn hover_opens_active_lsp_buffer_before_request() {
    let (mut editor, events) = recording_editor(vec![Buffer::new(
        Some("src/main.rs".to_string()),
        "fn main() {}".to_string(),
    )]);

    editor.test_execute_action(Action::Hover).await.unwrap();
    editor.test_execute_action(Action::Hover).await.unwrap();

    assert_eq!(
        recorded(&events),
        vec![
            LspEvent::DidOpen("src/main.rs".to_string()),
            LspEvent::Hover("src/main.rs".to_string()),
            LspEvent::Hover("src/main.rs".to_string()),
        ]
    );
}

#[tokio::test]
async fn code_action_opens_a_loading_picker_before_the_server_responds() {
    let (mut editor, events) = recording_editor(vec![Buffer::new(
        Some("src/main.rs".to_string()),
        "fn main() {}".to_string(),
    )]);

    editor
        .test_execute_action(Action::CodeAction)
        .await
        .unwrap();

    let frame = (0..24)
        .map(|line| editor.test_render_row(line).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(frame.contains("Code actions"), "{frame}");
    assert!(
        frame.contains("Fetching available code actions..."),
        "{frame}"
    );
    assert!(frame.contains("Loading actions..."), "{frame}");
    assert!(
        frame
            .chars()
            .any(|character| "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(character)),
        "{frame}"
    );
    assert!(matches!(
        recorded(&events).as_slice(),
        [LspEvent::DidOpen(file), LspEvent::CodeAction { .. }] if file == "src/main.rs"
    ));
}

#[tokio::test]
async fn daily_driver_lsp_actions_open_active_buffer_and_use_utf16_cursor() {
    let (mut editor, events) = recording_editor(vec![Buffer::new(
        Some("src/main.rs".to_string()),
        "👋 call(value)".to_string(),
    )]);
    editor.test_execute_action(Action::MoveRight).await.unwrap();
    editor.test_execute_action(Action::MoveRight).await.unwrap();

    editor
        .test_execute_action(Action::FormatDocument)
        .await
        .unwrap();
    editor
        .test_execute_action(Action::CodeAction)
        .await
        .unwrap();
    editor
        .test_execute_action(Action::CloseDialog)
        .await
        .unwrap();
    editor
        .test_execute_action(Action::SignatureHelp)
        .await
        .unwrap();
    editor
        .test_execute_action(Action::RenameSymbol("renamed".to_string()))
        .await
        .unwrap();

    assert_eq!(
        recorded(&events),
        vec![
            LspEvent::DidOpen("src/main.rs".to_string()),
            LspEvent::FormatDocument("src/main.rs".to_string()),
            LspEvent::CodeAction {
                file: "src/main.rs".to_string(),
                range: red::lsp::Range {
                    start: red::lsp::Position {
                        line: 0,
                        character: 3,
                    },
                    end: red::lsp::Position {
                        line: 0,
                        character: 3,
                    },
                },
                diagnostic_count: 0,
            },
            LspEvent::SignatureHelp {
                file: "src/main.rs".to_string(),
                x: 3,
                y: 0,
            },
            LspEvent::Rename {
                file: "src/main.rs".to_string(),
                x: 3,
                y: 0,
                new_name: "renamed".to_string(),
            },
        ]
    );
}

#[tokio::test]
async fn rename_prompt_replaces_the_symbol_and_submits_one_utf16_aware_request() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    let (mut editor, events) = recording_editor(vec![Buffer::new(
        Some("src/main.rs".to_string()),
        "👋 old_name".to_string(),
    )]);
    editor.test_execute_action(Action::MoveRight).await.unwrap();
    editor.test_execute_action(Action::MoveRight).await.unwrap();

    editor
        .test_execute_action(Action::StartRename)
        .await
        .unwrap();
    for character in "new_name".chars() {
        editor
            .test_execute_event(Event::Key(KeyEvent::new(
                KeyCode::Char(character),
                KeyModifiers::NONE,
            )))
            .await
            .unwrap();
    }
    editor
        .test_execute_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
        .await
        .unwrap();

    assert_eq!(
        recorded(&events),
        vec![
            LspEvent::DidOpen("src/main.rs".to_string()),
            LspEvent::Rename {
                file: "src/main.rs".to_string(),
                x: 3,
                y: 0,
                new_name: "new_name".to_string(),
            },
        ]
    );
}

#[tokio::test]
async fn document_symbols_opens_active_lsp_buffer_before_request() {
    let (mut editor, events) = recording_editor(vec![Buffer::new(
        Some("src/main.rs".to_string()),
        "fn main() {}".to_string(),
    )]);

    let request_id = editor.test_request_document_symbols().await.unwrap();

    assert_eq!(request_id, 42);
    assert_eq!(
        recorded(&events),
        vec![
            LspEvent::DidOpen("src/main.rs".to_string()),
            LspEvent::DocumentSymbols("src/main.rs".to_string()),
        ]
    );
}

#[tokio::test]
async fn workspace_symbols_opens_active_lsp_buffer_before_request() {
    let (mut editor, events) = recording_editor(vec![Buffer::new(
        Some("src/main.rs".to_string()),
        "fn main() {}".to_string(),
    )]);

    let request_id = editor
        .test_request_workspace_symbols("needle")
        .await
        .unwrap();

    assert_eq!(request_id, 43);
    assert_eq!(
        recorded(&events),
        vec![
            LspEvent::DidOpen("src/main.rs".to_string()),
            LspEvent::WorkspaceSymbols("needle".to_string()),
        ]
    );
}

#[tokio::test]
async fn references_open_active_lsp_buffer_before_request() {
    let (mut editor, events) = recording_editor(vec![Buffer::new(
        Some("src/main.rs".to_string()),
        "fn main() {}".to_string(),
    )]);
    editor.test_execute_action(Action::MoveRight).await.unwrap();

    let request_id = editor.test_request_references().await.unwrap();

    assert_eq!(request_id, 44);
    assert_eq!(
        recorded(&events),
        vec![
            LspEvent::DidOpen("src/main.rs".to_string()),
            LspEvent::References {
                file: "src/main.rs".to_string(),
                x: 1,
                y: 0,
                include_declaration: true,
            },
        ]
    );
}

#[tokio::test]
async fn split_with_file_opens_new_active_lsp_buffer() {
    let (mut editor, events) = recording_editor(vec![Buffer::new(None, "notes".to_string())]);

    editor
        .test_execute_action(Action::SplitHorizontalWithFile("src/main.rs".to_string()))
        .await
        .unwrap();

    let events = recorded(&events);
    let expected = std::env::current_dir().unwrap().join("src/main.rs");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, LspEvent::DidOpen(file) if Path::new(file) == expected)),
        "expected split-created active buffer to open through LSP, got {events:?}"
    );
}

#[tokio::test]
async fn default_format_on_save_requests_once_before_writing_and_ignores_a_duplicate_save() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("format.rs");
    std::fs::write(&path, "value   \n").unwrap();
    let (mut editor, events) = recording_workspace_editor_with_config(
        root.path(),
        vec![Buffer::new(
            Some(path.to_string_lossy().into_owned()),
            "value   \n".to_string(),
        )],
        Config::default(),
    );

    editor.test_execute_action(Action::Save).await.unwrap();
    editor.test_execute_action(Action::Save).await.unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "value   \n");
    assert_eq!(
        recorded(&events),
        vec![
            LspEvent::DidOpen(path.to_string_lossy().into_owned()),
            LspEvent::DidChange(path.to_string_lossy().into_owned()),
            LspEvent::FormatDocument(path.to_string_lossy().into_owned()),
        ]
    );
    assert!(editor
        .test_last_error()
        .is_some_and(|error| error.contains("already pending")));
}

#[tokio::test]
async fn disabled_format_on_save_skips_lsp_for_save_and_save_as_but_allows_manual_format() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source.rs");
    let target = root.path().join("target.rs");
    std::fs::write(&path, "value   \n").unwrap();
    let config = Config::from_toml_with_overrides(
        "theme = \"red.json\"\n[keys]\n[lsp]\nformat_on_save = true\n[formatting]\non_save = false",
        &[],
    )
    .unwrap();
    let (mut editor, events) = recording_workspace_editor_with_config(
        root.path(),
        vec![Buffer::new(
            Some(path.to_string_lossy().into_owned()),
            "value   \n".to_string(),
        )],
        config,
    );

    editor.test_execute_action(Action::Save).await.unwrap();
    editor
        .test_execute_action(Action::SaveAs(target.to_string_lossy().into_owned()))
        .await
        .unwrap();

    assert_eq!(std::fs::read_to_string(path).unwrap(), "value   \n");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "value   \n");
    assert!(!recorded(&events)
        .iter()
        .any(|event| matches!(event, LspEvent::FormatDocument(_))));

    editor
        .test_execute_action(Action::FormatDocument)
        .await
        .unwrap();
    assert_eq!(
        recorded(&events)
            .into_iter()
            .filter(|event| matches!(event, LspEvent::FormatDocument(_)))
            .collect::<Vec<_>>(),
        vec![LspEvent::FormatDocument(
            target.to_string_lossy().into_owned()
        )],
    );
}

#[tokio::test]
async fn deleting_and_reopening_a_buffer_sends_close_then_fresh_open() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("close.rs");
    std::fs::write(&path, "value\n").unwrap();
    let file = path.to_string_lossy().into_owned();
    let (mut editor, events) = recording_workspace_editor(
        root.path(),
        vec![Buffer::new(Some(file.clone()), "value\n".to_string())],
        false,
    );
    editor
        .test_ensure_current_buffer_lsp_opened()
        .await
        .unwrap();
    editor
        .test_execute_action(Action::DeleteBuffer(true))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::OpenFile(file.clone()))
        .await
        .unwrap();
    editor
        .test_ensure_current_buffer_lsp_opened()
        .await
        .unwrap();

    let events = recorded(&events);
    let lifecycle = events
        .into_iter()
        .filter(|event| matches!(event, LspEvent::DidOpen(_) | LspEvent::DidClose(_)))
        .collect::<Vec<_>>();
    assert_eq!(
        lifecycle,
        vec![
            LspEvent::DidOpen(file.clone()),
            LspEvent::DidClose(file.clone()),
            LspEvent::DidOpen(file),
        ]
    );
}

#[tokio::test]
async fn save_as_closes_the_old_lsp_document_and_opens_the_new_identity() {
    let root = tempfile::tempdir().unwrap();
    let old = root.path().join("old.rs");
    let new = root.path().join("new.rs");
    std::fs::write(&old, "value\n").unwrap();
    let old_file = old.to_string_lossy().into_owned();
    let new_file = new.to_string_lossy().into_owned();
    let (mut editor, events) = recording_workspace_editor(
        root.path(),
        vec![Buffer::new(Some(old_file.clone()), "changed\n".to_string())],
        false,
    );
    editor
        .test_ensure_current_buffer_lsp_opened()
        .await
        .unwrap();

    editor
        .test_execute_action(Action::SaveAs(new_file.clone()))
        .await
        .unwrap();

    assert_eq!(std::fs::read_to_string(&new).unwrap(), "changed\n");
    assert_eq!(
        recorded(&events),
        vec![
            LspEvent::DidOpen(old_file.clone()),
            LspEvent::DidClose(old_file),
            LspEvent::DidOpen(new_file),
        ]
    );
}

#[tokio::test]
async fn workspace_edit_uri_with_parent_alias_updates_the_existing_dirty_buffer() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    let path = root.path().join("open.rs");
    std::fs::write(&path, "disk value\n").unwrap();
    let file = path.to_string_lossy().into_owned();
    let (mut editor, events) = recording_workspace_editor(
        root.path(),
        vec![Buffer::new(Some(file.clone()), "dirty value\n".to_string())],
        false,
    );
    let aliased = format!(
        "{}/src/../open.rs",
        red::lsp::file_uri(root.path())
            .unwrap()
            .trim_end_matches('/')
    );
    let operations = red::lsp::workspace_edit_operations(&serde_json::json!({
        "changes": { (aliased): [{
            "range": { "start": { "line": 0, "character": 6 }, "end": { "line": 0, "character": 11 } },
            "newText": "updated"
        }] }
    }))
    .unwrap();

    editor
        .test_execute_action(Action::ApplyLspWorkspaceEditOperations {
            operations,
            expected_revisions: Vec::new(),
            command: None,
            label: "alias edit".to_string(),
            response: Some(Box::new(red::lsp::ServerRequest {
                id: serde_json::json!(7),
                method: "workspace/applyEdit".to_string(),
                params: serde_json::json!({}),
                source: Some("mock".to_string()),
            })),
            save_after_uri: None,
            save_as: None,
            save_previous_file: None,
        })
        .await
        .unwrap();

    assert_eq!(editor.test_buffer_names().len(), 1);
    assert_eq!(editor.test_buffer_contents(), "dirty updated\n");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "disk value\n");
    assert!(recorded(&events).iter().any(|event| matches!(
        event,
        LspEvent::WorkspaceEditResponse { id, applied: true, .. } if id == &serde_json::json!(7)
    )));
}

#[tokio::test]
async fn server_workspace_edit_without_an_originating_root_fails_closed_for_an_open_target() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("open.rs");
    std::fs::write(&path, "disk value\n").unwrap();
    let file = path.to_string_lossy().into_owned();
    let (mut editor, events) = recording_editor(vec![Buffer::new(
        Some(file.clone()),
        "dirty value\n".to_string(),
    )]);
    let uri = red::lsp::file_uri(&path).unwrap();
    let operations = red::lsp::workspace_edit_operations(&serde_json::json!({
        "changes": { (uri): [{
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 11 } },
            "newText": "owned"
        }] }
    }))
    .unwrap();

    editor
        .test_execute_action(Action::ApplyLspWorkspaceEditOperations {
            operations,
            expected_revisions: Vec::new(),
            command: None,
            label: "untrusted edit".to_string(),
            response: Some(Box::new(red::lsp::ServerRequest {
                id: serde_json::json!(8),
                method: "workspace/applyEdit".to_string(),
                params: serde_json::json!({}),
                source: Some("missing".to_string()),
            })),
            save_after_uri: None,
            save_as: None,
            save_previous_file: None,
        })
        .await
        .unwrap();

    assert_eq!(editor.test_buffer_contents(), "dirty value\n");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "disk value\n");
    assert!(matches!(
        recorded(&events).as_slice(),
        [LspEvent::WorkspaceEditResponse { id, applied: false, failure_reason: Some(reason) }]
            if id == &serde_json::json!(8) && reason.contains("originating server")
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn server_workspace_edit_opens_and_syncs_an_unopened_dirty_buffer_before_success_reply() {
    let root = tempfile::tempdir().unwrap();
    let active = root.path().join("active.rs");
    let closed = root.path().join("closed café.rs");
    std::fs::write(&active, "fn active() {}\n").unwrap();
    std::fs::write(&closed, "👋 old\r\n").unwrap();
    let (mut editor, events) = recording_workspace_editor(
        root.path(),
        vec![Buffer::new(
            Some(active.to_string_lossy().into_owned()),
            "fn active() {}\n".to_string(),
        )],
        false,
    );
    let uri = red::lsp::file_uri(&closed).unwrap();
    let operations = red::lsp::workspace_edit_operations(&serde_json::json!({
        "changes": { (uri): [{
            "range": { "start": { "line": 0, "character": 3 }, "end": { "line": 0, "character": 6 } },
            "newText": "new"
        }] }
    }))
    .unwrap();
    let request = red::lsp::ServerRequest {
        id: serde_json::json!("edit-1"),
        method: "workspace/applyEdit".to_string(),
        params: serde_json::json!({}),
        source: Some("mock".to_string()),
    };

    editor
        .test_execute_action(Action::ApplyLspWorkspaceEditOperations {
            operations,
            expected_revisions: Vec::new(),
            command: None,
            label: "update closed file".to_string(),
            response: Some(Box::new(request)),
            save_after_uri: None,
            save_as: None,
            save_previous_file: None,
        })
        .await
        .unwrap();

    assert_eq!(std::fs::read_to_string(&closed).unwrap(), "👋 old\r\n");
    assert!(editor
        .test_buffer_names()
        .iter()
        .any(|name| name == closed.to_str().unwrap()));
    assert_eq!(
        recorded(&events),
        vec![
            LspEvent::DidOpen(closed.to_string_lossy().into_owned()),
            LspEvent::DidChange(closed.to_string_lossy().into_owned()),
            LspEvent::WorkspaceEditResponse {
                id: serde_json::json!("edit-1"),
                applied: true,
                failure_reason: None,
            },
        ]
    );
}

#[tokio::test]
async fn invalid_server_workspace_edit_reports_failure_without_opening_or_mutating_the_target() {
    let root = tempfile::tempdir().unwrap();
    let active = root.path().join("active.rs");
    let closed = root.path().join("closed.rs");
    std::fs::write(&active, "fn active() {}\n").unwrap();
    std::fs::write(&closed, "👋 old\n").unwrap();
    let (mut editor, events) = recording_workspace_editor(
        root.path(),
        vec![Buffer::new(
            Some(active.to_string_lossy().into_owned()),
            "fn active() {}\n".to_string(),
        )],
        false,
    );
    let operations = red::lsp::workspace_edit_operations(&serde_json::json!({
        "changes": { (red::lsp::file_uri(&closed).unwrap()): [{
            "range": { "start": { "line": 0, "character": 1 }, "end": { "line": 0, "character": 2 } },
            "newText": "broken"
        }] }
    }))
    .unwrap();

    editor
        .test_execute_action(Action::ApplyLspWorkspaceEditOperations {
            operations,
            expected_revisions: Vec::new(),
            command: None,
            label: "broken edit".to_string(),
            response: Some(Box::new(red::lsp::ServerRequest {
                id: serde_json::json!(2),
                method: "workspace/applyEdit".to_string(),
                params: serde_json::json!({}),
                source: Some("mock".to_string()),
            })),
            save_after_uri: None,
            save_as: None,
            save_previous_file: None,
        })
        .await
        .unwrap();

    assert_eq!(std::fs::read_to_string(&closed).unwrap(), "👋 old\n");
    assert_eq!(editor.test_buffer_names().len(), 1);
    let expected_reason = if cfg!(unix) {
        "UTF-16"
    } else {
        "no-follow filesystem support"
    };
    assert!(matches!(
        recorded(&events).as_slice(),
        [LspEvent::WorkspaceEditResponse { id, applied: false, failure_reason: Some(reason) }]
            if id == &serde_json::json!(2) && reason.contains(expected_reason)
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn resource_only_rename_closes_old_lsp_uri_and_opens_new_uri_without_losing_unsaved_text() {
    let root = tempfile::tempdir().unwrap();
    let old = root.path().join("old.rs");
    let new = root.path().join("new.rs");
    std::fs::write(&old, "disk\n").unwrap();
    let (mut editor, events) = recording_workspace_editor(
        root.path(),
        vec![Buffer::new(
            Some(old.to_string_lossy().into_owned()),
            "unsaved\n".to_string(),
        )],
        false,
    );
    let operations = red::lsp::workspace_edit_operations(&serde_json::json!({
        "documentChanges": [{
            "kind": "rename",
            "oldUri": red::lsp::file_uri(&old).unwrap(),
            "newUri": red::lsp::file_uri(&new).unwrap()
        }]
    }))
    .unwrap();

    editor
        .test_execute_action(Action::ApplyLspWorkspaceEditOperations {
            operations,
            expected_revisions: Vec::new(),
            command: None,
            label: "rename file".to_string(),
            response: Some(Box::new(red::lsp::ServerRequest {
                id: serde_json::json!(3),
                method: "workspace/applyEdit".to_string(),
                params: serde_json::json!({}),
                source: Some("mock".to_string()),
            })),
            save_after_uri: None,
            save_as: None,
            save_previous_file: None,
        })
        .await
        .unwrap();

    assert!(!old.exists());
    assert_eq!(std::fs::read_to_string(&new).unwrap(), "disk\n");
    assert_eq!(editor.test_buffer_contents(), "unsaved\n");
    assert_eq!(
        recorded(&events),
        vec![
            LspEvent::DidClose(old.to_string_lossy().into_owned()),
            LspEvent::DidOpen(new.to_string_lossy().into_owned()),
            LspEvent::WorkspaceEditResponse {
                id: serde_json::json!(3),
                applied: true,
                failure_reason: None,
            },
        ]
    );
}

#[cfg(not(unix))]
#[tokio::test]
async fn server_workspace_unopened_and_resource_edits_fail_closed_without_mutation() {
    let root = tempfile::tempdir().unwrap();
    let active = root.path().join("active.rs");
    let closed = root.path().join("closed café.rs");
    let renamed = root.path().join("renamed.rs");
    std::fs::write(&active, "disk active\n").unwrap();
    std::fs::write(&closed, "👋 old\r\n").unwrap();
    let (mut editor, events) = recording_workspace_editor(
        root.path(),
        vec![Buffer::new(
            Some(active.to_string_lossy().into_owned()),
            "unsaved active\n".to_string(),
        )],
        false,
    );
    let operations = red::lsp::workspace_edit_operations(&serde_json::json!({
        "changes": { (red::lsp::file_uri(&closed).unwrap()): [{
            "range": { "start": { "line": 0, "character": 3 }, "end": { "line": 0, "character": 6 } },
            "newText": "new"
        }] }
    }))
    .unwrap();

    editor
        .test_execute_action(Action::ApplyLspWorkspaceEditOperations {
            operations,
            expected_revisions: Vec::new(),
            command: None,
            label: "update closed file".to_string(),
            response: Some(Box::new(red::lsp::ServerRequest {
                id: serde_json::json!(4),
                method: "workspace/applyEdit".to_string(),
                params: serde_json::json!({}),
                source: Some("mock".to_string()),
            })),
            save_after_uri: None,
            save_as: None,
            save_previous_file: None,
        })
        .await
        .unwrap();

    assert_eq!(std::fs::read_to_string(&closed).unwrap(), "👋 old\r\n");
    assert_eq!(std::fs::read_to_string(&active).unwrap(), "disk active\n");
    assert_eq!(
        editor.test_buffer_names(),
        vec![active.to_string_lossy().into_owned()]
    );
    assert_eq!(editor.test_buffer_contents(), "unsaved active\n");
    assert!(matches!(
        recorded(&events).as_slice(),
        [LspEvent::WorkspaceEditResponse { id, applied: false, failure_reason: Some(reason) }]
            if id == &serde_json::json!(4) && reason.contains("no-follow filesystem support")
    ));
    events.lock().unwrap().clear();

    let operations = red::lsp::workspace_edit_operations(&serde_json::json!({
        "documentChanges": [{
            "kind": "rename",
            "oldUri": red::lsp::file_uri(&active).unwrap(),
            "newUri": red::lsp::file_uri(&renamed).unwrap()
        }]
    }))
    .unwrap();
    editor
        .test_execute_action(Action::ApplyLspWorkspaceEditOperations {
            operations,
            expected_revisions: Vec::new(),
            command: None,
            label: "rename file".to_string(),
            response: Some(Box::new(red::lsp::ServerRequest {
                id: serde_json::json!(5),
                method: "workspace/applyEdit".to_string(),
                params: serde_json::json!({}),
                source: Some("mock".to_string()),
            })),
            save_after_uri: None,
            save_as: None,
            save_previous_file: None,
        })
        .await
        .unwrap();

    assert_eq!(std::fs::read_to_string(&active).unwrap(), "disk active\n");
    assert!(!renamed.exists());
    assert_eq!(
        editor.test_buffer_names(),
        vec![active.to_string_lossy().into_owned()]
    );
    assert_eq!(editor.test_buffer_contents(), "unsaved active\n");
    assert!(matches!(
        recorded(&events).as_slice(),
        [LspEvent::WorkspaceEditResponse { id, applied: false, failure_reason: Some(reason) }]
            if id == &serde_json::json!(5) && reason.contains("no-follow filesystem support")
    ));
}

// Structured agent LSP tools share the same editor and transport fixtures.
use red::agent_tools::{EditorPosition, EditorToolCall, EditorToolRequest, LspDiagnosticScope};
use serde_json::{json, Value};

fn agent_lsp_fixture(root: &Path, capabilities: Value) -> (Editor, RecordingLsp) {
    let lsp = RecordingLsp::with_workspace_root(root).with_capabilities(capabilities);
    let path = root.join("main.rs");
    std::fs::write(&path, "👋 old\n").unwrap();
    let config = Config {
        show_diagnostics: false,
        ..Config::default()
    };
    let mut editor = Editor::test_with_size(
        Box::new(lsp.clone()),
        80,
        24,
        config,
        Theme::default(),
        vec![Buffer::new(
            Some(path.to_string_lossy().into_owned()),
            "👋 old\n".into(),
        )],
    )
    .unwrap();
    editor.test_disable_terminal_output();
    editor.test_set_agent_root(root);
    (editor, lsp)
}

fn agent_request(call: EditorToolCall) -> EditorToolRequest {
    EditorToolRequest {
        session_id: "agent-lsp".into(),
        call,
    }
}

fn rename_request(editor: &Editor) -> EditorToolRequest {
    let text = editor.test_current_buffer().contents();
    let character = text[..text.find("old").unwrap()].encode_utf16().count();
    agent_request(EditorToolCall::LspPreviewRename {
        path: "main.rs".into(),
        position: EditorPosition { line: 0, character },
        expected_revision: editor.test_current_buffer().revision(),
        new_name: "renamed".into(),
    })
}

fn rename_edit(root: &Path, name: &str, start: usize, end: usize) -> Value {
    json!({"changes": {red::lsp::file_uri(root.join(name)).unwrap(): [{
        "range": {"start": {"line": 0, "character": start}, "end": {"line": 0, "character": end}},
        "newText": "renamed"
    }]}})
}

fn diagnostics_request(path: Option<&str>, refresh: bool) -> EditorToolRequest {
    agent_request(EditorToolCall::LspDiagnostics {
        scope: if path.is_some() {
            LspDiagnosticScope::File
        } else {
            LspDiagnosticScope::Workspace
        },
        path: path.map(str::to_string),
        severity: None,
        source: None,
        code: None,
        range: None,
        offset: 0,
        limit: 100,
        expected_generation: None,
        refresh,
        wait_ms: 0,
    })
}

fn sample_diagnostic(message: &str) -> Value {
    json!({"range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 6}},
        "severity": 1, "source": "test", "code": "E1", "message": message})
}

#[tokio::test]
async fn agent_lsp_status_and_prepare_are_typed_and_do_not_move_the_cursor() {
    let root = tempfile::tempdir().unwrap();
    let (mut editor, lsp) = agent_lsp_fixture(
        root.path(),
        json!({"renameProvider": {"prepareProvider": true}}),
    );
    let status = editor
        .test_run_agent_editor_tool(agent_request(EditorToolCall::LspStatus {
            path: "main.rs".into(),
        }))
        .await
        .unwrap();
    assert_eq!(status["status"], "ready");
    assert_eq!(status["capabilities"]["prepare_rename"], true);
    assert!(
        recorded(&lsp.events()).is_empty(),
        "status must not start a server"
    );
    lsp.queue_result("textDocument/prepareRename", json!({"range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 6}}, "placeholder": "old"}));
    let revision = editor.test_current_buffer().revision();
    let result = editor
        .test_run_agent_editor_tool(agent_request(EditorToolCall::LspPrepareRename {
            path: "main.rs".into(),
            position: EditorPosition {
                line: 0,
                character: 3,
            },
            expected_revision: revision,
        }))
        .await
        .unwrap();
    assert_eq!(result["placeholder"], "old");
    assert_eq!(result["range"]["start"]["character"], 3);
    assert_eq!(editor.test_cx(), 0);
    assert_eq!(editor.test_current_buffer().revision(), revision);
}

#[tokio::test]
async fn agent_lsp_rejects_split_surrogates_and_unsupported_operations() {
    let root = tempfile::tempdir().unwrap();
    let (mut editor, lsp) = agent_lsp_fixture(root.path(), json!({"renameProvider": true}));
    let revision = editor.test_current_buffer().revision();
    let error = editor
        .test_run_agent_editor_tool(agent_request(EditorToolCall::LspPrepareRename {
            path: "main.rs".into(),
            position: EditorPosition {
                line: 0,
                character: 1,
            },
            expected_revision: revision,
        }))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("surrogate"));
    assert!(recorded(&lsp.events()).is_empty());
    let result = editor
        .test_run_agent_editor_tool(agent_request(EditorToolCall::LspPrepareRename {
            path: "main.rs".into(),
            position: EditorPosition {
                line: 0,
                character: 3,
            },
            expected_revision: revision,
        }))
        .await
        .unwrap();
    assert_eq!(result["status"], "unsupported");
    // A server supporting rename without prepare remains usable.
    lsp.queue_result(
        "textDocument/rename",
        rename_edit(root.path(), "main.rs", 3, 6),
    );
    let request = rename_request(&editor);
    let preview = editor.test_run_agent_editor_tool(request).await.unwrap();
    assert!(preview["plan_id"].is_string());
}

#[tokio::test]
#[cfg(unix)]
async fn agent_lsp_rename_previews_and_applies_across_files_without_saving() {
    let root = tempfile::tempdir().unwrap();
    let (mut editor, lsp) = agent_lsp_fixture(root.path(), json!({"renameProvider": true}));
    std::fs::write(root.path().join("other.rs"), "old\n").unwrap();
    editor.test_execute_action(Action::MoveDown).await.unwrap();
    editor
        .test_execute_action(Action::EnterMode(red::editor::Mode::Insert))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::InsertCharAtCursorPos('!'))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::EnterMode(red::editor::Mode::Normal))
        .await
        .unwrap();
    let dirty_before = editor.test_current_buffer().contents();
    let mut edit = rename_edit(root.path(), "main.rs", 4, 7);
    edit["changes"][red::lsp::file_uri(root.path().join("other.rs")).unwrap()] =
        rename_edit(root.path(), "other.rs", 0, 3)["changes"]
            [red::lsp::file_uri(root.path().join("other.rs")).unwrap()]
        .clone();
    lsp.queue_result("textDocument/rename", edit);
    let request = rename_request(&editor);
    let preview = editor.test_run_agent_editor_tool(request).await.unwrap();
    assert_eq!(preview["files"].as_array().unwrap().len(), 2);
    assert_eq!(editor.test_current_buffer().contents(), dirty_before);
    assert_eq!(preview["applied"], false);
    let plan_id = preview["plan_id"].as_str().unwrap().to_string();
    let result = editor
        .test_run_agent_editor_tool(agent_request(EditorToolCall::LspApplyEdit {
            plan_id: plan_id.clone(),
        }))
        .await
        .unwrap();
    assert_eq!(result["applied"], true);
    assert_eq!(result["saved"], false);
    assert_eq!(
        editor.test_current_buffer().contents(),
        dirty_before.replace("old", "renamed")
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("main.rs")).unwrap(),
        "👋 old\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("other.rs")).unwrap(),
        "old\n"
    );
    assert!(
        matches!(editor.test_last_transaction_origin(), Some(red::undo::EditOrigin::Agent { session_id, .. }) if session_id == "agent-lsp")
    );
    editor.test_execute_action(Action::Undo).await.unwrap();
    assert_eq!(editor.test_current_buffer().contents(), dirty_before);
    assert!(editor
        .test_run_agent_editor_tool(agent_request(EditorToolCall::LspApplyEdit { plan_id }))
        .await
        .is_err());
    assert!(lsp.saves().lock().unwrap().is_empty());
}

#[tokio::test]
async fn agent_lsp_preview_rejects_changed_buffers_and_restarted_servers() {
    for restart in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let (mut editor, lsp) = agent_lsp_fixture(root.path(), json!({"renameProvider": true}));
        lsp.queue_result(
            "textDocument/rename",
            rename_edit(root.path(), "main.rs", 3, 6),
        );
        let request = rename_request(&editor);
        let preview = editor.test_run_agent_editor_tool(request).await.unwrap();
        if restart {
            lsp.restart();
        } else {
            editor
                .test_execute_action(Action::EnterMode(red::editor::Mode::Insert))
                .await
                .unwrap();
            editor
                .test_execute_action(Action::InsertCharAtCursorPos('x'))
                .await
                .unwrap();
        }
        let before = editor.test_current_buffer().contents();
        let error = editor
            .test_run_agent_editor_tool(agent_request(EditorToolCall::LspApplyEdit {
                plan_id: preview["plan_id"].as_str().unwrap().into(),
            }))
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains(if restart { "server changed" } else { "stale" }),
            "{error}"
        );
        assert_eq!(editor.test_current_buffer().contents(), before);
    }
}

#[tokio::test]
#[cfg(unix)]
async fn agent_lsp_preview_rechecks_unopened_disk_targets_and_session_ownership() {
    let root = tempfile::tempdir().unwrap();
    let (mut editor, lsp) = agent_lsp_fixture(root.path(), json!({"renameProvider": true}));
    std::fs::write(root.path().join("other.rs"), "old\n").unwrap();
    lsp.queue_result(
        "textDocument/rename",
        rename_edit(root.path(), "other.rs", 0, 3),
    );
    let request = rename_request(&editor);
    let preview = editor.test_run_agent_editor_tool(request).await.unwrap();
    let call = EditorToolCall::LspApplyEdit {
        plan_id: preview["plan_id"].as_str().unwrap().into(),
    };
    let error = editor
        .test_run_agent_editor_tool(EditorToolRequest {
            session_id: "another-session".into(),
            call: call.clone(),
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("another session"));
    std::fs::write(root.path().join("other.rs"), "concurrent disk edit\n").unwrap();
    assert!(editor
        .test_run_agent_editor_tool(agent_request(call))
        .await
        .is_err());
    assert_eq!(editor.test_current_buffer().contents(), "👋 old\n");
    assert_eq!(
        std::fs::read_to_string(root.path().join("other.rs")).unwrap(),
        "concurrent disk edit\n"
    );
    assert_eq!(editor.test_buffer_names().len(), 1);
}

#[tokio::test]
async fn agent_lsp_rejects_unsafe_or_invalid_edits_before_mutation() {
    for target in [
        "../escape.rs",
        ".env",
        ".git/config",
        "ignored.rs",
        "other.rs",
    ] {
        let root = tempfile::tempdir().unwrap();
        let (mut editor, lsp) = agent_lsp_fixture(root.path(), json!({"renameProvider": true}));
        std::fs::write(root.path().join(".gitignore"), "ignored.rs\n").unwrap();
        std::fs::write(root.path().join("other.rs"), "old\n").unwrap();
        let mut edit = rename_edit(root.path(), "main.rs", 3, 6);
        edit["changes"][red::lsp::file_uri(root.path().join(target)).unwrap()] = json!([{
            "range": {"start": {"line": 900, "character": 0}, "end": {"line": 900, "character": 1}}, "newText": "bad"
        }]);
        lsp.queue_result("textDocument/rename", edit);
        let request = rename_request(&editor);
        assert!(
            editor.test_run_agent_editor_tool(request).await.is_err(),
            "{target}"
        );
        assert_eq!(editor.test_current_buffer().contents(), "👋 old\n");
        assert_eq!(editor.test_buffer_names().len(), 1);
    }
}

#[tokio::test]
async fn agent_lsp_receives_diagnostics_with_display_disabled_and_paginates() {
    let root = tempfile::tempdir().unwrap();
    let (mut editor, lsp) = agent_lsp_fixture(
        root.path(),
        json!({"diagnosticProvider": {"interFileDependencies": true, "workspaceDiagnostics": false}}),
    );
    lsp.queue_result(
        "textDocument/diagnostic",
        json!({"kind": "full", "items": [sample_diagnostic("first"), sample_diagnostic("second")]}),
    );
    let response = editor
        .test_run_agent_editor_tool(diagnostics_request(Some("main.rs"), true))
        .await
        .unwrap();
    assert_eq!(response["items"].as_array().unwrap().len(), 2);
    assert_eq!(response["items"][0]["range"]["start"]["character"], 3);
    assert_eq!(response["refresh_status"], "received");
    assert_eq!(response["workspace_complete"], false);
    assert_eq!(response["items"][0]["freshness"], "unversioned");
    let mut request = diagnostics_request(None, false);
    if let EditorToolCall::LspDiagnostics { limit, .. } = &mut request.call {
        *limit = 1;
    }
    let first = editor
        .test_run_agent_editor_tool(request.clone())
        .await
        .unwrap();
    assert_eq!(first["next_offset"], 1);
    if let EditorToolCall::LspDiagnostics {
        offset,
        expected_generation,
        ..
    } = &mut request.call
    {
        *offset = 1;
        *expected_generation = first["generation"].as_u64();
    }
    let second = editor
        .test_run_agent_editor_tool(request.clone())
        .await
        .unwrap();
    assert_eq!(second["items"][0]["message"], "second");
    lsp.queue_result(
        "textDocument/diagnostic",
        json!({"kind": "full", "items": []}),
    );
    editor
        .test_run_agent_editor_tool(diagnostics_request(Some("main.rs"), true))
        .await
        .unwrap();
    assert!(editor
        .test_run_agent_editor_tool(request)
        .await
        .unwrap_err()
        .to_string()
        .contains("restart at offset 0"));
}

#[tokio::test]
async fn agent_lsp_push_only_diagnostics_do_not_claim_refresh_or_cleanliness() {
    let root = tempfile::tempdir().unwrap();
    let (mut editor, _) = agent_lsp_fixture(root.path(), json!({}));
    let result = editor
        .test_run_agent_editor_tool(diagnostics_request(Some("main.rs"), true))
        .await
        .unwrap();
    assert_eq!(result["refresh_status"], "push_only");
    assert_eq!(result["documents"][0]["freshness"], "not_received");
    let mut request = diagnostics_request(Some("main.rs"), true);
    if let EditorToolCall::LspDiagnostics { wait_ms, .. } = &mut request.call {
        *wait_ms = 1;
    }
    let result = editor.test_run_agent_editor_tool(request).await.unwrap();
    assert_eq!(result["status"], "timeout");
    assert_eq!(result["ok"], false);
}

#[tokio::test]
async fn agent_lsp_queries_allow_typing_and_reject_late_results() {
    let root = tempfile::tempdir().unwrap();
    let (mut editor, lsp) = agent_lsp_fixture(root.path(), json!({"renameProvider": true}));
    let request = rename_request(&editor);
    let mut response = editor.test_start_agent_lsp_tool(request).await;
    assert!(matches!(
        response.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    editor
        .test_execute_action(Action::EnterMode(red::editor::Mode::Insert))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::InsertCharAtCursorPos('x'))
        .await
        .unwrap();
    editor.test_service_background().await.unwrap();
    assert!(response.await.unwrap().unwrap_err().contains("stale"));
    assert_eq!(editor.test_current_buffer().contents(), "x👋 old\n");
    lsp.reply_to_last(rename_edit(root.path(), "main.rs", 3, 6));
    editor.test_service_background().await.unwrap();
    assert_eq!(editor.test_current_buffer().contents(), "x👋 old\n");
    assert!(recorded(&lsp.events()).iter().any(|event| matches!(event, LspEvent::SendRequest { method, .. } if method == "textDocument/rename")));
}

#[tokio::test]
async fn agent_lsp_filters_diagnostics_and_hides_outside_related_locations() {
    let root = tempfile::tempdir().unwrap();
    let (mut editor, lsp) = agent_lsp_fixture(
        root.path(),
        json!({"diagnosticProvider": {"interFileDependencies": false, "workspaceDiagnostics": false}}),
    );
    let mut diagnostic = sample_diagnostic("broken\u{1b}[31m");
    diagnostic["relatedInformation"] = json!([{"location": {"uri": "file:///outside/secret.rs", "range": diagnostic["range"]}, "message": "hidden"}]);
    diagnostic["data"] = json!({"private_server_state": "not returned to the agent"});
    lsp.queue_result(
        "textDocument/diagnostic",
        json!({"kind": "full", "items": [diagnostic]}),
    );
    editor
        .test_run_agent_editor_tool(diagnostics_request(Some("main.rs"), true))
        .await
        .unwrap();
    let mut request = diagnostics_request(Some("main.rs"), false);
    if let EditorToolCall::LspDiagnostics {
        severity,
        source,
        code,
        range,
        ..
    } = &mut request.call
    {
        *severity = Some(1);
        *source = Some("test".into());
        *code = Some("E1".into());
        *range = Some(red::agent_tools::EditorLspRange {
            start: EditorPosition {
                line: 0,
                character: 3,
            },
            end: EditorPosition {
                line: 0,
                character: 3,
            },
        });
    }
    let result = editor
        .test_run_agent_editor_tool(request.clone())
        .await
        .unwrap();
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert!(result["items"][0]["related_information"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(result["items"][0].get("data").is_none());
    assert!(!result["items"][0]["message"]
        .as_str()
        .unwrap()
        .contains('\u{1b}'));
    if let EditorToolCall::LspDiagnostics { severity, .. } = &mut request.call {
        *severity = Some(2);
    }
    assert!(
        editor.test_run_agent_editor_tool(request).await.unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn agent_lsp_push_wait_preserves_push_diagnostics_after_an_empty_pull() {
    let root = tempfile::tempdir().unwrap();
    let (mut editor, lsp) = agent_lsp_fixture(root.path(), json!({}));
    let mut request = diagnostics_request(Some("main.rs"), true);
    if let EditorToolCall::LspDiagnostics { wait_ms, .. } = &mut request.call {
        *wait_ms = 1000;
    }
    let response = editor.test_start_agent_lsp_tool(request).await;
    lsp.queue_message(red::lsp::InboundMessage::Notification(red::lsp::ParsedNotification::PublishDiagnostics(
        serde_json::from_value(json!({"uri": red::lsp::file_uri(root.path().join("main.rs")).unwrap(), "diagnostics": [sample_diagnostic("push report")]})).unwrap()
    )));
    for _ in 0..3 {
        editor.test_service_background().await.unwrap();
    }
    let result = response.await.unwrap().unwrap();
    assert_eq!(result["refresh_status"], "received");
    assert_eq!(result["items"][0]["message"], "push report");
    // A normal UI pull still updates the shared cache without clearing push reports.
    let uri = red::lsp::file_uri(root.path().join("main.rs")).unwrap();
    lsp.queue_message(red::lsp::InboundMessage::Message(
        red::lsp::ResponseMessage {
            id: 7654321,
            result: json!({"kind": "full", "items": []}),
            request: Some(red::lsp::Request::new(
                "textDocument/diagnostic",
                json!({"textDocument": {"uri": uri}}),
            )),
        },
    ));
    editor.test_service_background().await.unwrap();
    let result = editor
        .test_run_agent_editor_tool(diagnostics_request(Some("main.rs"), false))
        .await
        .unwrap();
    assert_eq!(result["items"][0]["message"], "push report");
}

#[tokio::test]
async fn agent_lsp_dropped_call_and_resource_edits_cannot_mutate_buffers() {
    let root = tempfile::tempdir().unwrap();
    let (mut editor, lsp) = agent_lsp_fixture(root.path(), json!({"renameProvider": true}));
    let request = rename_request(&editor);
    let response = editor.test_start_agent_lsp_tool(request).await;
    drop(response);
    editor.test_service_background().await.unwrap();
    lsp.reply_to_last(rename_edit(root.path(), "main.rs", 3, 6));
    editor.test_service_background().await.unwrap();
    assert_eq!(editor.test_current_buffer().contents(), "👋 old\n");
    for result in [
        json!({"documentChanges": [{"kind": "delete", "uri": red::lsp::file_uri(root.path().join("main.rs")).unwrap()}]}),
        json!({"changes": {red::lsp::file_uri(root.path().join("main.rs")).unwrap(): [{"range": sample_diagnostic("")["range"], "newText": "\u{0}"}]}}),
    ] {
        lsp.queue_result("textDocument/rename", result);
        let request = rename_request(&editor);
        assert!(editor.test_run_agent_editor_tool(request).await.is_err());
        assert_eq!(editor.test_current_buffer().contents(), "👋 old\n");
        assert!(root.path().join("main.rs").exists());
    }
}
