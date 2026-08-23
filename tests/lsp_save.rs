mod common;

use common::mock_lsp::{RecordingLsp, SavedDocument};
use red::{
    buffer::Buffer,
    config::Config,
    editor::{Action, Editor},
    test_utils::EditorTestExt,
    theme::Theme,
};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

fn editor(root: &Path, path: &Path, text: &str) -> (Editor, Arc<Mutex<Vec<SavedDocument>>>) {
    let lsp = RecordingLsp::with_workspace_root(root);
    let saves = lsp.saves();
    let mut config = Config::default();
    config.formatting.on_save = false;
    let mut editor = Editor::test_with_size(
        Box::new(lsp),
        80,
        24,
        config,
        Theme::default(),
        vec![Buffer::new(
            Some(path.to_string_lossy().into_owned()),
            text.to_string(),
        )],
    )
    .unwrap();
    editor.test_disable_terminal_output();
    (editor, saves)
}

#[tokio::test]
async fn did_save_follows_successful_save_and_save_as_but_not_failed_writes() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source.rs");
    let target = root.path().join("target.rs");
    let text = "fn main() {}\n";
    std::fs::write(&source, text).unwrap();
    let (mut editor, saves) = editor(root.path(), &source, text);
    editor.test_execute_action(Action::Save).await.unwrap();
    editor
        .test_execute_action(Action::SaveAs(target.to_string_lossy().into_owned()))
        .await
        .unwrap();
    editor
        .test_execute_action(Action::SaveAs(root.path().to_string_lossy().into_owned()))
        .await
        .unwrap();
    let saves = saves.lock().unwrap();
    assert_eq!(saves.len(), 2);
    for (saved, path) in saves.iter().zip([source, target]) {
        assert_eq!(Path::new(&saved.file), path);
        assert_eq!(saved.text, text);
        assert_eq!(saved.disk_text.as_deref(), Some(text));
    }
}

#[tokio::test]
async fn externally_modified_file_is_preserved_and_not_reported_to_lsp() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source.rs");
    let recovered = root.path().join("recovered.rs");
    std::fs::write(&source, "base\n").unwrap();
    let (mut editor, saves) = editor(root.path(), &source, "base\n");

    editor.test_type_text("local ").await.unwrap();
    std::fs::write(&source, "disk\n").unwrap();
    editor.test_execute_action(Action::Save).await.unwrap();

    assert_eq!(std::fs::read_to_string(&source).unwrap(), "disk\n");
    assert_eq!(editor.test_buffer_contents(), "local base\n");
    assert!(editor.test_current_buffer().is_dirty());
    assert!(editor
        .test_last_error()
        .is_some_and(|error| error.contains("changed on disk")));
    assert!(saves.lock().unwrap().is_empty());

    editor
        .test_execute_action(Action::SaveAs(recovered.to_string_lossy().into_owned()))
        .await
        .unwrap();

    assert_eq!(std::fs::read_to_string(&source).unwrap(), "disk\n");
    assert_eq!(std::fs::read_to_string(&recovered).unwrap(), "local base\n");
    assert!(!editor.test_current_buffer().is_dirty());
    let saves = saves.lock().unwrap();
    assert_eq!(saves.len(), 1);
    assert_eq!(Path::new(&saves[0].file), recovered);
}

#[tokio::test]
async fn did_save_contains_the_final_formatted_text() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("format.rs");
    std::fs::write(&path, "value   \n").unwrap();
    let (mut editor, saves) = editor(root.path(), &path, "value   \n");
    let uri = red::lsp::file_uri(&path).unwrap();
    let operations = red::lsp::workspace_edit_operations(&serde_json::json!({
        "changes": { (uri.clone()): [{
            "range": { "start": { "line": 0, "character": 5 }, "end": { "line": 0, "character": 8 } },
            "newText": ""
        }] }
    })).unwrap();
    editor
        .test_execute_action(Action::ApplyLspWorkspaceEditOperations {
            operations,
            expected_revisions: Vec::new(),
            command: None,
            label: "format".to_string(),
            response: None,
            save_after_uri: Some(uri),
            save_as: None,
            save_previous_file: None,
        })
        .await
        .unwrap();
    let saves = saves.lock().unwrap();
    assert_eq!(saves.len(), 1);
    assert_eq!(saves[0].text, "value\n");
    assert_eq!(saves[0].disk_text.as_deref(), Some("value\n"));
}
