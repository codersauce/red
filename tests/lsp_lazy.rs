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
    let mut editor = Editor::with_size(lsp, 80, 24, config, theme, buffers).unwrap();
    editor.test_disable_terminal_output();
    (editor, events)
}

fn recording_workspace_editor(
    root: &Path,
    buffers: Vec<Buffer>,
    format_on_save: bool,
) -> (Editor, Arc<Mutex<Vec<LspEvent>>>) {
    let lsp = RecordingLsp::with_workspace_root(root);
    let events = lsp.events();
    let lsp = Box::new(lsp) as Box<dyn LspClient + Send>;
    let mut config = Config::default();
    config.lsp.format_on_save = format_on_save;
    let mut editor = Editor::with_size(lsp, 80, 24, config, Theme::default(), buffers).unwrap();
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
    assert!(
        events
            .iter()
            .any(|event| matches!(event, LspEvent::DidOpen(file) if file == "src/main.rs")),
        "expected split-created active buffer to open through LSP, got {events:?}"
    );
}

#[tokio::test]
async fn format_on_save_requests_once_before_writing_and_ignores_a_duplicate_save() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("format.rs");
    std::fs::write(&path, "value   \n").unwrap();
    let (mut editor, events) = recording_workspace_editor(
        root.path(),
        vec![Buffer::new(
            Some(path.to_string_lossy().into_owned()),
            "value   \n".to_string(),
        )],
        true,
    );

    editor.test_execute_action(Action::Save).await.unwrap();
    editor.test_execute_action(Action::Save).await.unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "value   \n");
    assert_eq!(
        recorded(&events),
        vec![
            LspEvent::DidOpen(path.to_string_lossy().into_owned()),
            LspEvent::FormatDocument(path.to_string_lossy().into_owned()),
        ]
    );
    assert!(editor
        .test_last_error()
        .is_some_and(|error| error.contains("already pending")));
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
        })
        .await
        .unwrap();

    assert_eq!(std::fs::read_to_string(&closed).unwrap(), "👋 old\n");
    assert_eq!(editor.test_buffer_names().len(), 1);
    assert!(matches!(
        recorded(&events).as_slice(),
        [LspEvent::WorkspaceEditResponse { id, applied: false, failure_reason: Some(reason) }]
            if id == &serde_json::json!(2) && reason.contains("UTF-16")
    ));
}

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
