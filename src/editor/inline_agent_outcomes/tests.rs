use super::*;
use crate::{agent_tools::EditorToolCall, lsp::LspManager};

fn fixture(root: &Path) -> Editor {
    let first = root.join("first.rs");
    fs::write(&first, "old\n").unwrap();
    fs::write(root.join("second.rs"), "second\n").unwrap();
    let config = Config::default();
    let mut editor = Editor::with_size(
        Box::new(LspManager::new(config.lsp.clone())),
        100,
        30,
        config,
        Theme::default(),
        vec![Buffer::new(
            Some(first.to_string_lossy().into_owned()),
            "old\n".into(),
        )],
    )
    .unwrap();
    editor.test_disable_terminal_output();
    editor.agent_manager.set_root(Some(root.to_owned()));
    let range = TextRange::new(TextPosition::new(0, 0), TextPosition::new(1, 0));
    editor.inline_assist = Some(InlineAssistSession {
        parent_comment: None,
        allow_expansion: true,
        buffer_id: editor.current_buffer().id(),
        window_id: editor.window_manager.active_stable_window_id().unwrap(),
        expected_revision: editor.current_buffer().revision(),
        range,
        expected_text: "old\n".into(),
        scope: "function".into(),
        request_id: Some("inline-request".into()),
        session_id: None,
        transaction_id: None,
        annotation_group_id: "discussion".into(),
        has_result: true,
        result_request_id: Some("inline-request".into()),
    });
    editor
        .begin_inline_history_turn("inline-request", "Rename across two files", range)
        .unwrap();
    editor.inline_history.conversations[0].cwd = root.to_string_lossy().into_owned();
    let turn = editor.inline_history.turn_mut("inline-request").unwrap();
    turn.state = InlineTurnState::Completed;
    turn.result = Some(
        InlineAssistResult::from_tool("request_agent", json!({"reason":"Update both files."}))
            .unwrap(),
    );
    editor.inline_assist = None;
    editor
}

fn start(editor: &mut Editor, session: &str, turn: &str) {
    editor.staged_inline_agent_handoff = Some(StagedHandoff {
        comment_followup: None,
        request_id: "inline-request".into(),
    });
    let prompt = format!(
        "{}\nUser may edit the handoff before sending.",
        handoff_marker("inline-request")
    );
    editor
        .begin_inline_agent_outcome(session, turn, &prompt)
        .unwrap();
    editor.agent_manager.set_turn_id(session, turn);
    editor.agent_manager.mark_session_active(session);
    let root = editor.agent_manager.root().unwrap().to_owned();
    editor.agent_manager.begin_conversation(session, &root);
}

async fn write(editor: &mut Editor, session: &str, path: &str, content: &str) -> Value {
    let full = editor.agent_manager.root().unwrap().join(path);
    let revision = editor
        .file_buffer_index(&full)
        .map_or(0, |index| editor.buffer_manager[index].revision());
    editor
        .test_run_agent_editor_tool(EditorToolRequest {
            session_id: session.into(),
            call: EditorToolCall::WriteFile {
                path: path.into(),
                expected_revision: revision,
                content: content.into(),
            },
        })
        .await
        .unwrap()
}

fn outcome(editor: &Editor) -> &InlineAgentOutcome {
    editor
        .inline_history
        .turn("inline-request")
        .unwrap()
        .agent_outcomes
        .last()
        .unwrap()
}

#[tokio::test]
async fn inline_agent_outcome_groups_real_writes_and_survives_completion_and_recovery() {
    let root = tempfile::tempdir().unwrap();
    let mut editor = fixture(root.path());
    editor.begin_transaction("user's existing work");
    editor.replace_range(
        TextRange::insertion(TextPosition::new(0, 0)),
        "user prefix\n",
    );
    editor.commit_transaction(editor.cursor_snapshot());
    start(&mut editor, "agent", "turn-1");
    assert_eq!(
        write(&mut editor, "agent", "first.rs", "user prefix\nrenamed\n").await["saved"],
        true
    );
    assert_eq!(
        write(&mut editor, "agent", "second.rs", "second renamed\n").await["saved"],
        true
    );
    assert_eq!(
        write(
            &mut editor,
            "agent",
            "first.rs",
            "user prefix\nrenamed again\n"
        )
        .await["saved"],
        true
    );
    assert_eq!(outcome(&editor).files.len(), 2);
    assert_eq!(outcome(&editor).files[0].edits.len(), 1);
    assert_eq!(
        outcome(&editor).files[0].edits[0].before,
        "user prefix\nold\n"
    );
    assert_eq!(outcome(&editor).files[0].edits[0].transaction_ids.len(), 2);
    let (bridge, worker) = CodexBridge::channel(std::num::NonZeroUsize::new(4).unwrap());
    editor.agent_manager.set_bridge(bridge);
    worker
        .send(CodexEvent::MessageCompleted {
            session_id: "agent".into(),
            text: "Updated both files.".into(),
        })
        .await
        .unwrap();
    worker
        .send(CodexEvent::Completed {
            session_id: "agent".into(),
            stop_reason: "end_turn".into(),
        })
        .await
        .unwrap();
    let mut frame = RenderBuffer::new(100, 30, &Style::default());
    let mut runtime = Runtime::new();
    editor
        .service_background(&mut frame, &mut runtime)
        .await
        .unwrap();
    assert_eq!(outcome(&editor).state, InlineAgentState::Completed);
    assert_eq!(outcome(&editor).answer, "Updated both files.");
    assert!(!editor.agent_manager.is_session_active("agent"));
    assert_eq!(
        editor
            .inline_comments
            .iter()
            .filter(|comment| matches!(
                comment.origin,
                super::super::inline_comments::InlineCommentOrigin::AgentOutcome { .. }
            ))
            .count(),
        2
    );
    let return_file = editor.current_buffer().file.clone();
    editor
        .test_execute_production_action(Action::ViewInlineAgentChanges {
            request_id: "inline-request".into(),
            outcome: 0,
            change: 1,
        })
        .await
        .unwrap();
    assert_eq!(
        editor.current_buffer().file.as_deref(),
        root.path().join("second.rs").to_str()
    );
    editor
        .test_execute_production_action(Action::JumpBack)
        .await
        .unwrap();
    assert_eq!(editor.current_buffer().file, return_file);
    let mut recovered: InlineHistory =
        serde_json::from_slice(&serde_json::to_vec(&editor.inline_history).unwrap()).unwrap();
    recovered.recover();
    recovered.validate().unwrap();
    assert_eq!(
        recovered.turn("inline-request").unwrap().agent_outcomes,
        editor
            .inline_history
            .turn("inline-request")
            .unwrap()
            .agent_outcomes
    );
}

#[tokio::test]
async fn inline_agent_outcome_does_not_claim_interleaved_or_unrelated_edits() {
    let root = tempfile::tempdir().unwrap();
    let mut editor = fixture(root.path());
    start(&mut editor, "agent", "turn-1");
    write(&mut editor, "other-session", "second.rs", "unrelated\n").await;
    assert!(outcome(&editor).files.is_empty());
    write(&mut editor, "agent", "first.rs", "agent one\n").await;
    editor.begin_transaction("interleaved user edit");
    editor.replace_range(
        TextRange::insertion(TextPosition::new(0, 0)),
        "user added\n",
    );
    editor.commit_transaction(editor.cursor_snapshot());
    assert_eq!(
        editor
            .inline_agent_file_status(&outcome(&editor).files[0])
            .0,
        "Changed since Agent · unsaved"
    );
    write(&mut editor, "agent", "first.rs", "user added\nagent two\n").await;
    let file = &outcome(&editor).files[0];
    assert_eq!(file.edits.len(), 2);
    assert_eq!(file.edits[1].before, "user added\nagent one\n");
    editor.finish_inline_agent_outcome("agent", InlineAgentState::Cancelled, Some("Cancelled"));
    assert_eq!(outcome(&editor).files.len(), 1);
    assert_eq!(outcome(&editor).state, InlineAgentState::Cancelled);
    assert_eq!(
        fs::read_to_string(root.path().join("first.rs")).unwrap(),
        "user added\nagent two\n"
    );
}

#[test]
fn inline_agent_outcome_requires_the_staged_reference_and_bounds_storage() {
    let root = tempfile::tempdir().unwrap();
    let mut editor = fixture(root.path());
    editor.staged_inline_agent_handoff = Some(StagedHandoff {
        comment_followup: None,
        request_id: "inline-request".into(),
    });
    editor
        .begin_inline_agent_outcome("agent", "unrelated", "different request")
        .unwrap();
    assert!(editor
        .inline_history
        .turn("inline-request")
        .unwrap()
        .agent_outcomes
        .is_empty());
    start(&mut editor, "agent", "turn-1");
    assert!(editor
        .check_inline_agent_receipt_capacity(
            "agent",
            "before",
            &"x".repeat(MAX_AGENT_IMAGE_BYTES + 1)
        )
        .is_err());
    let mut recovered = editor.inline_history.clone();
    recovered.recover();
    assert_eq!(
        recovered.turn("inline-request").unwrap().agent_outcomes[0].state,
        InlineAgentState::Cancelled
    );
}

#[tokio::test]
async fn inline_agent_outcome_reports_unsaved_changes_and_restores_hidden_markers() {
    let root = tempfile::tempdir().unwrap();
    let mut editor = fixture(root.path());
    start(&mut editor, "agent", "turn-1");
    // The open buffer remains valid, but persisting over a directory must fail.
    fs::remove_file(root.path().join("first.rs")).unwrap();
    fs::create_dir(root.path().join("first.rs")).unwrap();
    let result = write(&mut editor, "agent", "first.rs", "applied but unsaved\n").await;
    assert_eq!(result["applied"], true);
    assert_eq!(result["saved"], false);
    assert!(!outcome(&editor).files[0].edits[0].saved);
    assert_eq!(
        editor
            .inline_agent_file_status(&outcome(&editor).files[0])
            .0,
        "Unsaved"
    );
    editor.finish_inline_agent_outcome("agent", InlineAgentState::Failed, Some("Could not save"));
    editor.clear_inline_comments();
    assert!(outcome(&editor).files[0].hidden);
    editor.sync_inline_change_summaries();
    assert!(!editor.inline_comments.iter().any(|comment| matches!(
        comment.origin,
        super::super::inline_comments::InlineCommentOrigin::AgentOutcome { .. }
    )));
    let mut frame = RenderBuffer::new(100, 30, &Style::default());
    let mut runtime = Runtime::new();
    assert!(editor
        .show_inline_history_annotations("inline-request", &mut frame, &mut runtime)
        .await
        .unwrap());
    assert!(!outcome(&editor).files[0].hidden);
    assert!(editor.inline_comments.iter().any(|comment| matches!(
        comment.origin,
        super::super::inline_comments::InlineCommentOrigin::AgentOutcome { .. }
    )));
    let turn = editor.inline_history.turn("inline-request").unwrap();
    let blocks = editor.inline_agent_history_blocks(turn, HistoryView::Changes, root.path());
    assert!(blocks
        .iter()
        .any(|block| matches!(block, HistoryBlock::FileLink { text, .. } if text == "first.rs:1")));
    assert!(blocks.iter().any(
        |block| matches!(block, HistoryBlock::Status(status) if status.text.contains("Unsaved"))
    ));
    assert!(blocks.iter().any(|block| matches!(block, HistoryBlock::Diff { before, after, .. } if before == "old\n" && after == "applied but unsaved\n")));
}
