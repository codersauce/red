use super::*;
use crate::{
    agent_tools::{EditorActionName, EditorToolCall, EditorToolRequest},
    buffer::Buffer,
    config::Config,
    editor::{Action, Editor, KeyAction},
    lsp::LspManager,
    plugin::{markdown_link_target, TextPanelLinkTarget},
    theme::Theme,
    undo::{TextPosition, TextRange},
};

fn fixture(root: &std::path::Path) -> Editor {
    let path = root.join("main.rs");
    std::fs::write(&path, "zero\none\ntwo\n").unwrap();
    let config = Config::default();
    let mut editor = Editor::with_size(
        Box::new(LspManager::new(config.lsp.clone())),
        100,
        30,
        config,
        Theme::default(),
        vec![Buffer::new(
            Some(path.to_string_lossy().into_owned()),
            "zero\none\ntwo\n".into(),
        )],
    )
    .unwrap();
    editor.test_disable_terminal_output();
    editor.test_set_agent_root(root);
    editor.agent_manager.begin_conversation("agent", root);
    editor.agent_manager.set_turn_id("agent", "turn-1");
    editor
}

fn request(call: EditorToolCall) -> EditorToolRequest {
    EditorToolRequest {
        session_id: "agent".into(),
        call,
    }
}

fn annotations(items: &[(usize, Option<usize>, &str)]) -> Vec<EditorAnnotationInput> {
    items
        .iter()
        .map(|(start_line, end_line, message)| EditorAnnotationInput {
            start_line: *start_line,
            end_line: *end_line,
            message: (*message).to_string(),
        })
        .collect()
}

#[tokio::test]
async fn agent_adds_navigates_and_dismisses_annotations_without_editing_source() {
    let root = tempfile::tempdir().unwrap();
    let mut editor = fixture(root.path());
    let contents = editor.current_buffer().contents();
    let revision = editor.current_buffer().revision();
    let dirty = editor.current_buffer().is_dirty();
    let undo = editor.current_buffer().undo_history.undo_tree();

    let added = editor
        .test_run_agent_editor_tool(request(EditorToolCall::AddAnnotations {
            path: "main.rs".into(),
            expected_revision: revision,
            annotations: annotations(&[(0, None, "First note"), (2, None, "Last note")]),
        }))
        .await
        .unwrap();
    let first = added["annotations"][0]["id"].as_str().unwrap().to_string();
    let href = added["annotations"][0]["href"].as_str().unwrap();
    assert_eq!(href, format!("red://annotation/{first}"));
    assert_eq!(
        markdown_link_target(href),
        Some(TextPanelLinkTarget::Annotation {
            id: Uuid::parse_str(&first).unwrap(),
        })
    );
    assert_eq!(added["annotations"].as_array().unwrap().len(), 2);
    assert_eq!(editor.current_buffer().contents(), contents);
    assert_eq!(editor.current_buffer().revision(), revision);
    assert_eq!(editor.current_buffer().is_dirty(), dirty);
    assert_eq!(editor.current_buffer().undo_history.undo_tree(), undo);

    let state = editor
        .test_run_agent_editor_tool(request(EditorToolCall::RunEditorAction {
            action: EditorActionName::NextAnnotation,
        }))
        .await
        .unwrap();
    assert_eq!(state["annotations"]["visible_count"], 2);
    assert_eq!(state["annotations"]["current"]["message"], "Last note");
    assert_eq!(state["cursor"]["line"], 2);
    let state = editor
        .test_run_agent_editor_tool(request(EditorToolCall::RunEditorAction {
            action: EditorActionName::PreviousAnnotation,
        }))
        .await
        .unwrap();
    assert_eq!(state["annotations"]["current"]["id"], first);

    let dismissed = editor
        .test_run_agent_editor_tool(request(EditorToolCall::DismissAnnotations {
            annotation_ids: vec![first],
        }))
        .await
        .unwrap();
    assert_eq!(dismissed["dismissed"].as_array().unwrap().len(), 1);
    assert_eq!(editor.agent_annotation_state()["visible_count"], 1);
    assert_eq!(editor.current_buffer().contents(), contents);
    assert_eq!(editor.current_buffer().revision(), revision);
}

#[tokio::test]
async fn transcript_annotation_links_switch_buffers_open_cards_and_expire_safely() {
    let root = tempfile::tempdir().unwrap();
    let other = root.path().join("other.rs");
    std::fs::write(&other, "elsewhere\n").unwrap();
    let mut editor = fixture(root.path());
    let revision = editor.current_buffer().revision();
    let added = editor
        .test_run_agent_editor_tool(request(EditorToolCall::AddAnnotations {
            path: "main.rs".into(),
            expected_revision: revision,
            annotations: annotations(&[(1, None, "Follow this value")]),
        }))
        .await
        .unwrap();
    let id = Uuid::parse_str(added["annotations"][0]["id"].as_str().unwrap()).unwrap();
    let target = markdown_link_target(added["annotations"][0]["href"].as_str().unwrap()).unwrap();

    editor
        .test_execute_production_action(Action::OpenFile(other.to_string_lossy().into_owned()))
        .await
        .unwrap();
    assert_eq!(editor.current_buffer().file.as_deref(), other.to_str());
    let action = editor.follow_text_panel_link(target.clone());
    assert_eq!(action, KeyAction::Single(Action::OpenAgentAnnotation(id)));
    let KeyAction::Single(action) = action else {
        unreachable!();
    };
    editor.test_execute_production_action(action).await.unwrap();
    assert!(editor.current_buffer().name().ends_with("main.rs"));
    assert_eq!(
        editor.agent_annotation_state()["current"]["id"],
        id.to_string()
    );
    assert!(editor.current_dialog.is_some());

    editor
        .test_run_agent_editor_tool(request(EditorToolCall::DismissAnnotations {
            annotation_ids: vec![id.to_string()],
        }))
        .await
        .unwrap();
    editor
        .test_execute_production_action(Action::OpenFile(other.to_string_lossy().into_owned()))
        .await
        .unwrap();
    let KeyAction::Single(action) = editor.follow_text_panel_link(target) else {
        unreachable!();
    };
    editor.test_execute_production_action(action).await.unwrap();
    assert_eq!(editor.current_buffer().file.as_deref(), other.to_str());
    assert_eq!(
        editor.test_last_error(),
        Some("Annotation is no longer available")
    );
}

#[tokio::test]
async fn agent_annotation_guards_revisions_ranges_messages_and_duplicate_dismissals() {
    let root = tempfile::tempdir().unwrap();
    let mut editor = fixture(root.path());
    let revision = editor.current_buffer().revision();
    for (expected_revision, annotations) in [
        (revision + 1, annotations(&[(0, None, "stale revision")])),
        (revision, annotations(&[(3, None, "outside")])),
        (revision, annotations(&[(2, Some(1), "backwards")])),
        (revision, annotations(&[(0, None, "\u{202e}hidden")])),
    ] {
        assert!(editor
            .test_run_agent_editor_tool(request(EditorToolCall::AddAnnotations {
                path: "main.rs".into(),
                expected_revision,
                annotations,
            }))
            .await
            .is_err());
    }
    assert!(editor
        .test_run_agent_editor_tool(request(EditorToolCall::AddAnnotations {
            path: "../outside.rs".into(),
            expected_revision: revision,
            annotations: annotations(&[(0, None, "outside workspace")]),
        }))
        .await
        .is_err());
    let added = editor
        .test_run_agent_editor_tool(request(EditorToolCall::AddAnnotations {
            path: "main.rs".into(),
            expected_revision: revision,
            annotations: annotations(&[(1, None, "valid")]),
        }))
        .await
        .unwrap();
    let id = added["annotations"][0]["id"].as_str().unwrap().to_string();
    assert!(editor
        .test_run_agent_editor_tool(request(EditorToolCall::DismissAnnotations {
            annotation_ids: vec![id.clone(), id],
        }))
        .await
        .is_err());
    assert_eq!(editor.agent_annotation_state()["visible_count"], 1);
}

#[tokio::test]
async fn excluded_agent_context_redacts_annotation_messages() {
    let root = tempfile::tempdir().unwrap();
    let mut editor = fixture(root.path());
    let revision = editor.current_buffer().revision();
    editor
        .test_run_agent_editor_tool(request(EditorToolCall::AddAnnotations {
            path: "main.rs".into(),
            expected_revision: revision,
            annotations: annotations(&[(1, None, "private annotation text")]),
        }))
        .await
        .unwrap();

    let sensitive = root.path().join(".env");
    editor
        .current_buffer_mut()
        .save_as(&sensitive.to_string_lossy())
        .unwrap();
    let state = editor.agent_editor_state(root.path());

    assert_eq!(state["context"]["included"], false);
    assert_eq!(state["annotations"]["visible_count"], 0);
    assert_eq!(state["annotations"]["current"], Value::Null);
    assert!(!state.to_string().contains("private annotation text"));
}

#[tokio::test]
async fn overlapping_agent_annotations_cycle_with_the_shared_annotation_controls() {
    let root = tempfile::tempdir().unwrap();
    let mut editor = fixture(root.path());
    let revision = editor.current_buffer().revision();
    let added = editor
        .test_run_agent_editor_tool(request(EditorToolCall::AddAnnotations {
            path: "main.rs".into(),
            expected_revision: revision,
            annotations: annotations(&[(1, None, "one"), (1, Some(2), "one through two")]),
        }))
        .await
        .unwrap();
    let first = added["annotations"][0]["id"].as_str().unwrap();
    let second = added["annotations"][1]["id"].as_str().unwrap();
    assert_eq!(editor.agent_annotation_state()["current"]["id"], first);

    let state = editor
        .test_run_agent_editor_tool(request(EditorToolCall::RunEditorAction {
            action: EditorActionName::NextOverlappingAnnotation,
        }))
        .await
        .unwrap();
    assert_eq!(state["annotations"]["current"]["id"], second);
    let state = editor
        .test_run_agent_editor_tool(request(EditorToolCall::RunEditorAction {
            action: EditorActionName::PreviousOverlappingAnnotation,
        }))
        .await
        .unwrap();
    assert_eq!(state["annotations"]["current"]["id"], first);
}

#[tokio::test]
async fn agent_annotations_recover_with_stable_ids_and_moved_anchors() {
    let root = tempfile::tempdir().unwrap();
    let mut original = fixture(root.path());
    let revision = original.current_buffer().revision();
    let added = original
        .test_run_agent_editor_tool(request(EditorToolCall::AddAnnotations {
            path: "main.rs".into(),
            expected_revision: revision,
            annotations: annotations(&[(1, None, "Track one")]),
        }))
        .await
        .unwrap();
    let id = added["annotations"][0]["id"].as_str().unwrap().to_string();
    original.begin_transaction("insert above annotation");
    original.replace_range(TextRange::insertion(TextPosition::new(0, 0)), "before\n");
    original.commit_transaction(original.cursor_snapshot());
    let snapshot = original.test_session_snapshot();
    assert_eq!(
        snapshot.agent_conversation.as_ref().unwrap().annotations[0].start_line,
        2
    );

    let config = Config::default();
    let buffers = Editor::buffers_from_session_snapshot(&snapshot);
    let mut restored = Editor::with_size(
        Box::new(LspManager::new(config.lsp.clone())),
        100,
        30,
        config,
        Theme::default(),
        buffers,
    )
    .unwrap();
    restored.test_disable_terminal_output();
    restored.restore_session_snapshot(&snapshot).unwrap();
    let comment = restored
        .inline_comments
        .iter()
        .find(|comment| comment.id.to_string() == id)
        .unwrap();
    assert_eq!(comment.lines(restored.current_buffer()), (2, 2));
    assert!(!comment.stale);
    assert_eq!(comment.message, "Track one");
}

#[tokio::test]
async fn agent_annotations_follow_save_as_for_dismissal_and_recovery() {
    let root = tempfile::tempdir().unwrap();
    let mut editor = fixture(root.path());
    let revision = editor.current_buffer().revision();
    let added = editor
        .test_run_agent_editor_tool(request(EditorToolCall::AddAnnotations {
            path: "main.rs".into(),
            expected_revision: revision,
            annotations: annotations(&[(0, None, "Dismiss me"), (2, None, "Recover me")]),
        }))
        .await
        .unwrap();
    let dismissed_id = added["annotations"][0]["id"].as_str().unwrap().to_string();
    let recovered_id = added["annotations"][1]["id"].as_str().unwrap().to_string();
    let renamed = root.path().join("renamed.rs");

    editor
        .current_buffer_mut()
        .save_as(&renamed.to_string_lossy())
        .unwrap();
    let snapshot = editor.test_session_snapshot();
    assert!(snapshot
        .agent_conversation
        .as_ref()
        .unwrap()
        .annotations
        .iter()
        .all(|annotation| annotation.path == renamed.to_string_lossy()));

    editor
        .test_run_agent_editor_tool(request(EditorToolCall::DismissAnnotations {
            annotation_ids: vec![dismissed_id],
        }))
        .await
        .unwrap();
    let snapshot = editor.test_session_snapshot();
    assert_eq!(
        snapshot.agent_conversation.as_ref().unwrap().annotations[0].id,
        recovered_id
    );

    let config = Config::default();
    let buffers = Editor::buffers_from_session_snapshot(&snapshot);
    let mut restored = Editor::with_size(
        Box::new(LspManager::new(config.lsp.clone())),
        100,
        30,
        config,
        Theme::default(),
        buffers,
    )
    .unwrap();
    restored.test_disable_terminal_output();
    restored.restore_session_snapshot(&snapshot).unwrap();
    assert!(restored
        .inline_comments
        .iter()
        .any(|comment| comment.id.to_string() == recovered_id));
}

#[tokio::test]
async fn changed_agent_annotation_source_is_stale_and_dismissal_is_recovery_safe() {
    let root = tempfile::tempdir().unwrap();
    let mut editor = fixture(root.path());
    let revision = editor.current_buffer().revision();
    let added = editor
        .test_run_agent_editor_tool(request(EditorToolCall::AddAnnotations {
            path: "main.rs".into(),
            expected_revision: revision,
            annotations: annotations(&[(1, None, "Track one")]),
        }))
        .await
        .unwrap();
    let id = added["annotations"][0]["id"].as_str().unwrap().to_string();
    editor.begin_transaction("change annotated source");
    editor.replace_range(
        TextRange::new(TextPosition::new(1, 0), TextPosition::new(1, 3)),
        "changed",
    );
    editor.commit_transaction(editor.cursor_snapshot());
    assert!(editor.inline_comments[0].stale);

    editor
        .test_run_agent_editor_tool(request(EditorToolCall::DismissAnnotations {
            annotation_ids: vec![id],
        }))
        .await
        .unwrap();
    assert!(editor
        .test_session_snapshot()
        .agent_conversation
        .unwrap()
        .annotations
        .is_empty());
}
