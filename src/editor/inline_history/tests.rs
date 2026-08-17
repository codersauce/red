use super::*;
use crate::{inline_assist::InlineCommentInput, lsp::LspManager};

#[tokio::test]
async fn word_backspace_updates_the_editor_owned_inline_history_query() {
    for modifiers in [KeyModifiers::ALT, KeyModifiers::CONTROL] {
        let mut editor = editor("unchanged\n");
        let mut frame = RenderBuffer::new(100, 30, &Style::default());
        let mut runtime = Runtime::new();
        editor
            .open_inline_history(&mut frame, &mut runtime)
            .await
            .unwrap();
        editor
            .handle_inline_history_action(&HistoryAction::Search, &mut frame, &mut runtime)
            .await
            .unwrap();
        editor
            .handle_inline_history_action(
                &HistoryAction::Query("one 👨‍👩‍👧e\u{301}".into()),
                &mut frame,
                &mut runtime,
            )
            .await
            .unwrap();
        let event = Event::Key(KeyEvent::new(KeyCode::Backspace, modifiers));
        let Some(KeyAction::Single(Action::InlineHistoryAction(action))) =
            editor.current_dialog.as_mut().unwrap().handle_event(&event)
        else {
            panic!("inline history search did not handle word backspace");
        };
        editor
            .handle_inline_history_action(&action, &mut frame, &mut runtime)
            .await
            .unwrap();
        let browser = editor.inline_history_browser.as_ref().unwrap();
        assert_eq!(browser.query, "one ");
        assert!(browser.searching);
        assert_eq!(editor.current_buffer().contents(), "unchanged\n");
    }
}

fn editor(text: &str) -> Editor {
    let config = Config::default();
    let mut editor = Editor::with_size(
        Box::new(LspManager::new(config.lsp.clone())),
        100,
        30,
        config,
        Theme::default(),
        vec![Buffer::new(Some("/workspace/sample.c".into()), text.into())],
    )
    .unwrap();
    editor.test_disable_terminal_output();
    editor
}

fn begin(editor: &mut Editor, group: &str, request: &str, range: TextRange, prompt: &str) {
    editor.inline_assist = Some(InlineAssistSession {
        parent_comment: None,
        allow_expansion: false,
        buffer_id: editor.current_buffer().id(),
        window_id: editor.window_manager.active_stable_window_id().unwrap(),
        expected_revision: editor.current_buffer().revision(),
        range,
        expected_text: editor.current_buffer().text_in_range(range),
        scope: "test".into(),
        request_id: Some(request.into()),
        session_id: Some("provider".into()),
        transaction_id: None,
        annotation_group_id: group.into(),
        has_result: false,
        result_request_id: None,
    });
    editor
        .begin_inline_history_turn(request, prompt, range)
        .unwrap();
}

async fn complete(editor: &mut Editor, request: &str, replacement: Option<&str>, message: &str) {
    let result = InlineAssistResult {
        expanded_scope: None,
        needs_agent: None,
        replacement: replacement.map(str::to_string),
        comments: vec![InlineCommentInput {
            start_line: 1,
            end_line: None,
            message: message.into(),
        }],
    };
    editor
        .apply_inline_result(
            request,
            "provider",
            &result,
            &mut RenderBuffer::new(100, 30, &Style::default()),
            &mut Runtime::new(),
        )
        .await
        .unwrap();
}

fn line_range(start: usize, end: usize) -> TextRange {
    TextRange::new(TextPosition::new(start, 0), TextPosition::new(end, 0))
}

fn agent_draft(editor: &Editor) -> String {
    editor
        .panel_manager
        .snapshot(100)
        .panels
        .into_iter()
        .find(|panel| panel.id == "agent-conversation")
        .and_then(|panel| panel.text)
        .and_then(|text| text.composer)
        .map(|composer| composer.text)
        .unwrap_or_default()
}

#[tokio::test]
async fn inline_handoff_reopens_a_restored_hidden_agent_pane_without_losing_its_draft() {
    let mut editor = editor("alpha\nbeta\n");
    editor.panel_manager.create_text_panel(
        "agent-conversation".into(),
        plugin::PanelConfig {
            composer: Some(plugin::TextPanelComposerConfig {
                placeholder: "Ask".into(),
                rows: 3,
            }),
            ..plugin::PanelConfig::default()
        },
    );
    editor
        .panel_manager
        .load_text_panel_draft("agent-conversation", "unsent Agent draft", None)
        .unwrap();
    editor
        .panel_manager
        .set_panel_visible("agent-conversation", false);
    let saved = editor.panel_manager.snapshot(100);
    editor.panel_manager = plugin::panel::PanelManager::default();
    editor.panel_manager.stage_restore(saved);

    let mut runtime = Runtime::new();
    runtime
        .load_plugin("agent", include_str!("../../../plugins/agent.hk"))
        .await
        .unwrap();
    let mut frame = RenderBuffer::new(100, 30, &Style::default());
    begin(
        &mut editor,
        "discussion",
        "request",
        line_range(0, 1),
        "Compare with the last commit",
    );
    let result =
        InlineAssistResult::from_tool("request_agent", json!({"reason": "Read the Git diff."}))
            .unwrap();
    editor
        .apply_inline_result("request", "provider", &result, &mut frame, &mut runtime)
        .await
        .unwrap();
    let handoff = editor.inline_handoff_prompt("discussion").unwrap();
    editor
        .execute(&Action::EscalateInlineAssist, &mut frame, &mut runtime)
        .await
        .unwrap();
    editor
        .service_background(&mut frame, &mut runtime)
        .await
        .unwrap();

    assert!(
        editor.panel_manager.is_visible("agent-conversation"),
        "{:?}",
        editor.last_error
    );
    assert_eq!(editor.mode, Mode::Normal);
    assert!(editor.selection.is_none());
    assert_eq!(agent_draft(&editor), "unsent Agent draft");
    let Some(KeyAction::Multiple(confirm)) = editor.current_dialog.as_mut().and_then(|dialog| {
        dialog.handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('y'),
            KeyModifiers::NONE,
        )))
    }) else {
        panic!("expected draft replacement confirmation");
    };
    for action in confirm {
        editor
            .execute(&action, &mut frame, &mut runtime)
            .await
            .unwrap();
    }
    assert_eq!(agent_draft(&editor), handoff);
    assert_eq!(
        editor
            .staged_inline_agent_handoff
            .as_ref()
            .map(|handoff| handoff.request_id.as_str()),
        Some("request")
    );
    assert_eq!(
        editor.panel_manager.focused_panel_id(),
        Some("agent-conversation")
    );
    assert_eq!(editor.current_buffer().contents(), "alpha\nbeta\n");
    assert!(
        runtime.try_recv_request().is_none(),
        "handoff must not send a prompt"
    );
}

#[tokio::test]
async fn inline_handoff_reveals_an_existing_hidden_pane_and_clears_editor_zoom() {
    let mut editor = editor("alpha\n");
    editor.panel_manager.create_text_panel(
        "agent-conversation".into(),
        plugin::PanelConfig {
            composer: Some(plugin::TextPanelComposerConfig {
                placeholder: "Ask".into(),
                rows: 3,
            }),
            ..plugin::PanelConfig::default()
        },
    );
    editor
        .panel_manager
        .set_panel_visible("agent-conversation", false);
    editor.zoomed_pane = Some(FocusTarget::Window(
        editor.window_manager.active_stable_window_id().unwrap(),
    ));
    editor
        .execute(
            &Action::StageInlineAssistHandoff {
                comment_followup: None,
                request_id: None,
                prompt: "reviewable handoff".into(),
                expected_draft: None,
            },
            &mut RenderBuffer::new(100, 30, &Style::default()),
            &mut Runtime::new(),
        )
        .await
        .unwrap();
    assert!(editor.panel_manager.is_visible("agent-conversation"));
    assert!(editor.zoomed_pane.is_none());
    assert_eq!(agent_draft(&editor), "reviewable handoff");
    assert_eq!(
        editor.panel_manager.focused_panel_id(),
        Some("agent-conversation")
    );
}

#[tokio::test]
async fn inline_handoff_keeps_the_discussion_when_agent_is_unavailable() {
    let mut editor = editor("alpha\n");
    begin(
        &mut editor,
        "discussion",
        "request",
        line_range(0, 1),
        "Do this in Agent",
    );
    editor.current_dialog = Some(Box::new(editor.inline_assist_popup(
        "test",
        InlineAssistPopupState::NeedsAgent("Read another file".into()),
    )));
    editor
        .execute(
            &Action::EscalateInlineAssist,
            &mut RenderBuffer::new(100, 30, &Style::default()),
            &mut Runtime::new(),
        )
        .await
        .unwrap();
    assert!(editor
        .last_error
        .as_deref()
        .unwrap()
        .contains("Agent is unavailable"));
    assert!(editor.inline_assist.is_some());
    assert!(editor.current_dialog.is_some());
    assert_eq!(editor.mode, Mode::Normal);
    assert!(editor.selection.is_none());
    assert_eq!(editor.current_buffer().contents(), "alpha\n");
}

#[test]
fn inline_prompt_history_is_workspace_scoped_recent_deduplicated_and_recoverable() {
    let mut editor = editor("alpha\n");
    for (group, request, prompt, time) in [
        ("first", "one", "old prompt", 1),
        ("second", "two", "new prompt", 2),
        ("first", "three", "old prompt", 3),
        ("foreign", "four", "foreign prompt", 4),
    ] {
        begin(&mut editor, group, request, line_range(0, 1), prompt);
        editor
            .inline_history
            .turn_mut(request)
            .unwrap()
            .created_at_ms = time;
    }
    editor.inline_history.conversations.last_mut().unwrap().cwd = "/another-workspace".into();
    assert_eq!(editor.inline_prompt_history(), ["old prompt", "new prompt"]);
    let snapshot = serde_json::to_vec(&editor.inline_history).unwrap();
    editor.inline_history = serde_json::from_slice(&snapshot).unwrap();
    assert_eq!(editor.inline_prompt_history(), ["old prompt", "new prompt"]);
    for index in 0..60 {
        let request = format!("request-{index}");
        let prompt = format!("prompt-{index}");
        begin(&mut editor, "bounded", &request, line_range(0, 1), &prompt);
        editor
            .inline_history
            .turn_mut(&request)
            .unwrap()
            .created_at_ms = 10 + index;
    }
    let history = editor.inline_prompt_history();
    assert_eq!(history.len(), 50);
    assert_eq!(history.first().unwrap(), "prompt-59");
    assert_eq!(history.last().unwrap(), "prompt-10");
}

#[tokio::test]
async fn inline_broader_edit_keeps_the_previous_answer_and_stages_the_actual_request() {
    let mut editor = editor("alpha\nbeta\n");
    let range = line_range(0, 1);
    begin(
        &mut editor,
        "discussion",
        "advice",
        range,
        "How can we improve this?",
    );
    complete(&mut editor, "advice", None, "Extract a helper.").await;
    let comment_id = editor.inline_comments[0].id;
    begin(
        &mut editor,
        "discussion",
        "implement",
        range,
        "Do it for me",
    );
    let result =
        InlineAssistResult::from_tool("request_agent", json!({"reason": "Update the caller too."}))
            .unwrap();
    let mut frame = RenderBuffer::new(100, 30, &Style::default());
    let mut runtime = Runtime::new();
    editor
        .apply_inline_result("implement", "provider", &result, &mut frame, &mut runtime)
        .await
        .unwrap();
    assert_eq!(editor.inline_comments[0].id, comment_id);
    assert_eq!(editor.current_buffer().contents(), "alpha\nbeta\n");
    assert!(!editor.current_buffer().is_dirty());
    assert_eq!(
        editor.inline_history.turn("advice").unwrap().disposition,
        InlineDisposition::Kept
    );
    assert_eq!(
        editor.inline_history.turn("implement").unwrap().status(),
        "needs Agent"
    );
    assert!(matches!(
        editor.inline_assist_result_state(),
        InlineAssistPopupState::NeedsAgent(_)
    ));
    let prompt = editor.inline_handoff_prompt("discussion").unwrap();
    assert!(prompt.contains("Latest user request:\nDo it for me"));
    assert!(prompt.contains("Extract a helper."));
    assert!(prompt.contains("Update the caller too."));
    editor
        .execute(&Action::ViewInlineAssistAnswer, &mut frame, &mut runtime)
        .await
        .unwrap();
    assert_eq!(
        editor
            .current_dialog
            .as_mut()
            .unwrap()
            .handle_event(&Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))),
        Some(KeyAction::Single(Action::CancelInlineAssistRefine))
    );
    editor.inline_comments.clear();
    editor.restore_inline_history_comments();
    assert_eq!(editor.inline_comments.len(), 1);
    assert_eq!(editor.inline_comments[0].message, "Extract a helper.");
}

#[tokio::test]
async fn history_retains_refinements_answers_and_hidden_comments() {
    let mut editor = editor("alpha\nbeta\n");
    begin(
        &mut editor,
        "conversation",
        "first",
        line_range(0, 1),
        "Explain alpha",
    );
    editor
        .inline_history
        .append_answer("first", "An explanation.");
    complete(&mut editor, "first", None, "First note").await;
    begin(
        &mut editor,
        "conversation",
        "second",
        line_range(0, 1),
        "Be more specific",
    );
    complete(&mut editor, "second", None, "Second note").await;
    editor.close_inline_assist_session();
    assert_eq!(editor.inline_history.conversations[0].turns.len(), 2);
    assert_eq!(
        editor.inline_history.turn("first").unwrap().disposition,
        InlineDisposition::Superseded
    );
    assert_eq!(
        editor.inline_history.turn("first").unwrap().answer_text(),
        "An explanation."
    );
    editor.dismiss_inline_comment();
    assert_eq!(
        editor
            .inline_history
            .turn("second")
            .unwrap()
            .hidden_comments,
        vec![0]
    );
    editor.restore_inline_history_comments();
    assert!(editor.inline_comments.is_empty());
    assert!(!editor.current_buffer().is_dirty());
}

#[tokio::test]
async fn history_preview_is_reversible_and_does_not_edit() {
    let mut editor = editor("alpha\nbeta\ngamma\n");
    begin(
        &mut editor,
        "one",
        "one",
        line_range(0, 1),
        "First question",
    );
    complete(&mut editor, "one", None, "First note").await;
    begin(
        &mut editor,
        "two",
        "two",
        line_range(1, 2),
        "Second question",
    );
    complete(&mut editor, "two", None, "Second note").await;
    editor.close_inline_assist_session();
    editor.move_to_text_position(TextPosition::new(2, 2));
    let origin = editor.current_jump_entry();
    let ids = editor
        .inline_comments
        .iter()
        .map(|comment| comment.id)
        .collect::<Vec<_>>();
    let history_len = editor.active_jump_list().entries.len();
    let mut frame = RenderBuffer::new(100, 30, &Style::default());
    let mut runtime = Runtime::new();
    editor
        .open_inline_history(&mut frame, &mut runtime)
        .await
        .unwrap();
    assert_eq!(
        editor
            .inline_comment_display_messages(editor.current_buffer())
            .len(),
        1
    );
    assert!(
        editor.inline_comment_display_messages(editor.current_buffer())[0]
            .1
            .contains("Second note")
    );
    editor
        .handle_inline_history_action(&HistoryAction::Next, &mut frame, &mut runtime)
        .await
        .unwrap();
    assert!(
        editor.inline_comment_display_messages(editor.current_buffer())[0]
            .1
            .contains("First note")
    );
    editor
        .handle_inline_history_action(&HistoryAction::Close, &mut frame, &mut runtime)
        .await
        .unwrap();
    assert_eq!(editor.current_jump_entry().char_index, origin.char_index);
    assert_eq!(editor.active_jump_list().entries.len(), history_len);
    assert_eq!(
        editor
            .inline_comments
            .iter()
            .map(|comment| comment.id)
            .collect::<Vec<_>>(),
        ids
    );
    assert_eq!(editor.current_buffer().contents(), "alpha\nbeta\ngamma\n");
    assert!(!editor.current_buffer().is_dirty());
}

#[tokio::test]
async fn history_tracks_source_changes_deletion_and_undo() {
    let mut editor = editor("alpha\nbeta\ngamma\n");
    begin(&mut editor, "one", "one", line_range(1, 2), "Explain beta");
    complete(&mut editor, "one", None, "Beta note").await;
    editor.close_inline_assist_session();
    editor.begin_transaction("insert above");
    editor.replace_range(TextRange::insertion(TextPosition::new(0, 0)), "new\n");
    editor.commit_transaction(editor.cursor_snapshot());
    let (_, range, state) = editor
        .resolve_history_turn(editor.inline_history.turn("one").unwrap())
        .unwrap();
    assert_eq!(state, InlineSourceState::Unchanged);
    assert_eq!(range.start.line, 2);
    editor.begin_transaction("remove target");
    editor.replace_range(range, "");
    editor.commit_transaction(editor.cursor_snapshot());
    assert_eq!(
        editor
            .resolve_history_turn(editor.inline_history.turn("one").unwrap())
            .unwrap()
            .2,
        InlineSourceState::Detached
    );
    editor
        .test_execute_production_action(Action::Undo)
        .await
        .unwrap();
    assert_eq!(
        editor
            .resolve_history_turn(editor.inline_history.turn("one").unwrap())
            .unwrap()
            .2,
        InlineSourceState::Unchanged
    );
}

#[tokio::test]
async fn removing_one_comment_target_detaches_it_without_losing_the_conversation() {
    let mut editor = editor("alpha\nbeta\ngamma\n");
    begin(
        &mut editor,
        "one",
        "one",
        line_range(0, 3),
        "Review the function",
    );
    let result = InlineAssistResult {
        expanded_scope: None,
        needs_agent: None,
        replacement: None,
        comments: vec![InlineCommentInput {
            start_line: 2,
            end_line: None,
            message: "Beta note".into(),
        }],
    };
    editor
        .apply_inline_result(
            "one",
            "provider",
            &result,
            &mut RenderBuffer::new(100, 30, &Style::default()),
            &mut Runtime::new(),
        )
        .await
        .unwrap();
    editor.close_inline_assist_session();
    editor.begin_transaction("remove beta");
    editor.replace_range(line_range(1, 2), "");
    editor.commit_transaction(editor.cursor_snapshot());
    assert_eq!(
        editor
            .resolve_history_turn(editor.inline_history.turn("one").unwrap())
            .unwrap()
            .2,
        InlineSourceState::Changed
    );
    assert_eq!(
        editor
            .resolve_history_comment(editor.inline_history.turn("one").unwrap(), 0)
            .unwrap()
            .2,
        InlineSourceState::Detached
    );
    assert!(editor
        .inline_comment_display_messages(editor.current_buffer())
        .is_empty());
    editor
        .test_execute_production_action(Action::Undo)
        .await
        .unwrap();
    assert_eq!(
        editor.inline_comment_display_messages(editor.current_buffer())[0].1,
        "Beta note"
    );
    assert_eq!(
        editor.inline_comments[0].lines(editor.current_buffer()),
        (1, 1)
    );
    assert_eq!(
        editor
            .inline_history
            .turn("one")
            .unwrap()
            .location
            .range
            .start
            .line,
        0
    );
    assert_eq!(editor.inline_history.conversations.len(), 1);
}

#[tokio::test]
async fn history_snapshot_recovers_without_provider_or_old_buffer_ids() {
    let mut original = editor("alpha\nbeta\n");
    begin(
        &mut original,
        "one",
        "completed",
        line_range(0, 1),
        "Explain alpha",
    );
    complete(&mut original, "completed", None, "Alpha note").await;
    begin(
        &mut original,
        "two",
        "pending",
        line_range(1, 2),
        "Explain beta",
    );
    let snapshot = original.test_session_snapshot();
    let encoded = serde_json::to_vec(&snapshot).unwrap();
    let snapshot: SessionSnapshot = serde_json::from_slice(&encoded).unwrap();
    assert!(snapshot
        .inline_history
        .turn("completed")
        .unwrap()
        .location
        .buffer_id
        .is_none());
    let mut restored = editor("alpha\nbeta\n");
    restored.restore_session_snapshot(&snapshot).unwrap();
    assert_eq!(
        restored.inline_history.turn("pending").unwrap().state,
        InlineTurnState::Cancelled
    );
    assert_eq!(restored.inline_comments.len(), 1);
    assert_eq!(restored.inline_comments[0].message, "Alpha note");
    assert!(restored.inline_assist.is_none());
    assert_eq!(
        restored
            .resolve_history_turn(restored.inline_history.turn("completed").unwrap())
            .unwrap()
            .2,
        InlineSourceState::Unchanged
    );
    restored.config.persist_inline_history = Some(false);
    assert!(restored
        .test_session_snapshot()
        .inline_history
        .conversations
        .is_empty());
}

#[tokio::test]
async fn recheck_starts_a_new_provider_session_with_recovered_context() {
    let mut editor = editor("alpha\nbeta\n");
    begin(&mut editor, "one", "one", line_range(0, 1), "Explain alpha");
    complete(&mut editor, "one", None, "Alpha note").await;
    let mut frame = RenderBuffer::new(100, 30, &Style::default());
    let mut runtime = Runtime::new();
    editor
        .open_inline_history(&mut frame, &mut runtime)
        .await
        .unwrap();
    editor
        .handle_inline_history_action(&HistoryAction::Recheck, &mut frame, &mut runtime)
        .await
        .unwrap();
    let assist = editor.inline_assist.as_ref().unwrap();
    assert_eq!(assist.annotation_group_id, "one");
    assert!(assist.session_id.is_none());
    assert_eq!(assist.expected_text, "alpha\n");
    assert!(editor
        .recovered_inline_context("one")
        .contains("Alpha note"));
}

#[tokio::test]
async fn history_resolve_forget_and_export_preserve_explicit_intent() {
    let mut editor = editor("alpha\nbeta\n");
    begin(&mut editor, "one", "one", line_range(0, 1), "Explain alpha");
    complete(&mut editor, "one", None, "Alpha note").await;
    let mut frame = RenderBuffer::new(100, 30, &Style::default());
    let mut runtime = Runtime::new();
    editor
        .open_inline_history(&mut frame, &mut runtime)
        .await
        .unwrap();
    editor
        .handle_inline_history_action(&HistoryAction::Resolve, &mut frame, &mut runtime)
        .await
        .unwrap();
    assert!(editor.inline_history.conversations[0].resolved);
    editor
        .handle_inline_history_action(&HistoryAction::Close, &mut frame, &mut runtime)
        .await
        .unwrap();
    assert!(editor.inline_comments.is_empty());
    editor
        .open_inline_history(&mut frame, &mut runtime)
        .await
        .unwrap();
    editor
        .handle_inline_history_action(&HistoryAction::Resolve, &mut frame, &mut runtime)
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory
        .path()
        .join("history.json")
        .to_string_lossy()
        .into_owned();
    editor
        .handle_inline_history_action(
            &HistoryAction::Export(path.clone()),
            &mut frame,
            &mut runtime,
        )
        .await
        .unwrap();
    let exported: InlineHistory = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(exported.conversations.len(), 1);
    editor
        .handle_inline_history_action(
            &HistoryAction::Export(path.clone()),
            &mut frame,
            &mut runtime,
        )
        .await
        .unwrap();
    assert!(editor
        .last_error
        .as_deref()
        .unwrap()
        .contains("could not export"));
    editor
        .handle_inline_history_action(&HistoryAction::Forget, &mut frame, &mut runtime)
        .await
        .unwrap();
    assert_eq!(editor.inline_history.conversations.len(), 1);
    editor
        .handle_inline_history_action(&HistoryAction::ConfirmForget, &mut frame, &mut runtime)
        .await
        .unwrap();
    assert!(editor.inline_history.conversations.is_empty());
    assert!(editor.inline_history.sources.is_empty());
}

#[tokio::test]
async fn history_follows_file_identity_changes_and_reopens_closed_buffers() {
    let mut editor = editor("alpha\nbeta\n");
    begin(&mut editor, "one", "one", line_range(0, 1), "Explain alpha");
    complete(&mut editor, "one", None, "Alpha note").await;
    let old_id = editor.current_buffer().id();
    editor.current_buffer_mut().file = Some("/workspace/renamed.c".into());
    editor.refresh_inline_history_paths();
    assert_eq!(
        editor.inline_history.conversations[0].file,
        "/workspace/renamed.c"
    );
    editor.detach_inline_history_buffer(old_id);
    editor.inline_comments.clear();
    editor.buffer_manager.replace_buffers(vec![Buffer::new(
        Some("/workspace/renamed.c".into()),
        "alpha\nbeta\n".into(),
    )]);
    editor.rebind_inline_history_file("/workspace/renamed.c");
    assert_ne!(
        editor
            .inline_history
            .turn("one")
            .unwrap()
            .location
            .buffer_id,
        Some(old_id)
    );
    assert_eq!(editor.inline_comments.len(), 1);
}

#[tokio::test]
async fn deleted_duplicate_source_is_not_reattached_to_another_occurrence() {
    let mut editor = editor("first\nsame\nsecond\nsame\nlast\n");
    begin(
        &mut editor,
        "one",
        "one",
        line_range(1, 2),
        "Explain the first same",
    );
    complete(&mut editor, "one", None, "First occurrence").await;
    editor.close_inline_assist_session();
    editor.begin_transaction("delete first occurrence");
    editor.replace_range(line_range(1, 2), "");
    editor.commit_transaction(editor.cursor_snapshot());
    assert_eq!(
        editor
            .resolve_history_comment(editor.inline_history.turn("one").unwrap(), 0)
            .unwrap()
            .2,
        InlineSourceState::Detached
    );
    assert!(editor
        .inline_comment_display_messages(editor.current_buffer())
        .is_empty());
}

#[tokio::test]
async fn ordinary_undo_and_redo_update_the_recorded_edit_outcome() {
    let mut editor = editor("alpha\nbeta\n");
    begin(&mut editor, "one", "edit", line_range(0, 1), "Rename alpha");
    complete(&mut editor, "edit", Some("renamed\n"), "Renamed value").await;
    editor.close_inline_assist_session();
    editor
        .test_execute_production_action(Action::Undo)
        .await
        .unwrap();
    assert_eq!(
        editor.inline_history.turn("edit").unwrap().disposition,
        InlineDisposition::Undone
    );
    editor
        .test_execute_production_action(Action::Redo)
        .await
        .unwrap();
    assert_eq!(
        editor.inline_history.turn("edit").unwrap().disposition,
        InlineDisposition::Kept
    );
    assert_eq!(editor.current_buffer().contents(), "renamed\nbeta\n");
}

async fn run_history_action(editor: &mut Editor, action: HistoryAction) {
    editor
        .handle_inline_history_action(
            &action,
            &mut RenderBuffer::new(100, 30, &Style::default()),
            &mut Runtime::new(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn inline_history_scroll_and_noop_navigation_keep_the_existing_detail() {
    let mut editor = editor("fn demo() {}\n");
    begin(&mut editor, "group", "one", line_range(0, 1), "Explain");
    complete(&mut editor, "one", None, &"More detail.\n\n".repeat(40)).await;
    editor
        .open_inline_history(
            &mut RenderBuffer::new(100, 30, &Style::default()),
            &mut Runtime::new(),
        )
        .await
        .unwrap();
    let refreshed = editor.inline_history_browser.as_ref().unwrap().refreshed_at;
    run_history_action(&mut editor, HistoryAction::ScrollDown).await;
    let scroll = editor.inline_history_browser.as_ref().unwrap().scroll;
    assert!(scroll > 0);
    for action in [
        HistoryAction::Next,
        HistoryAction::Previous,
        HistoryAction::Select(0),
        HistoryAction::Collapse,
    ] {
        run_history_action(&mut editor, action).await;
        let browser = editor.inline_history_browser.as_ref().unwrap();
        assert_eq!(browser.scroll, scroll);
        assert_eq!(browser.refreshed_at, refreshed);
    }
    run_history_action(&mut editor, HistoryAction::ScrollUp).await;
    assert_eq!(editor.inline_history_browser.as_ref().unwrap().scroll, 0);
    assert_eq!(
        editor.inline_history_browser.as_ref().unwrap().refreshed_at,
        refreshed
    );
}

#[tokio::test]
async fn inline_history_cycles_all_rich_views_without_editing_source() {
    let mut editor = editor("fn old() {}\n");
    begin(&mut editor, "group", "edit", line_range(0, 1), "Rename it");
    complete(
        &mut editor,
        "edit",
        Some("fn new() {}\n"),
        "**Renamed** the function.",
    )
    .await;
    editor.inline_history.conversations[0].cwd = "/workspace".into();
    editor
        .open_inline_history(
            &mut RenderBuffer::new(100, 30, &Style::default()),
            &mut Runtime::new(),
        )
        .await
        .unwrap();
    let original = editor.current_buffer().contents();
    let revision = editor.current_buffer().revision();
    for view in [
        HistoryView::Conversation,
        HistoryView::Reviewed,
        HistoryView::Before,
        HistoryView::Compare,
        HistoryView::Changes,
    ] {
        assert_eq!(editor.inline_history_browser.as_ref().unwrap().view, view);
        let turn = editor.inline_history.turn("edit").unwrap();
        let detail = editor.history_turn_detail(turn, InlineSourceState::Unchanged);
        assert_eq!(detail.location.as_deref(), Some("sample.c:1"));
        assert!(detail.can_jump);
        assert_eq!(detail.open_label, "review changes");
        assert!(detail
            .statuses
            .iter()
            .any(|status| status.text == "Unsaved"));
        assert!(detail.blocks.iter().any(|block| matches!(
            (view, block),
            (HistoryView::Conversation, HistoryBlock::Markdown(_))
                | (
                    HistoryView::Reviewed | HistoryView::Before,
                    HistoryBlock::Code { .. }
                )
                | (
                    HistoryView::Compare | HistoryView::Changes,
                    HistoryBlock::Diff { .. }
                )
        )));
        run_history_action(&mut editor, HistoryAction::CycleView).await;
        assert_eq!(editor.current_buffer().contents(), original);
        assert_eq!(editor.current_buffer().revision(), revision);
    }
    assert_eq!(
        editor.inline_history_browser.as_ref().unwrap().view,
        HistoryView::Conversation
    );
}

#[tokio::test]
async fn inline_history_file_link_navigates_without_applying_or_saving() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("other.c");
    std::fs::write(&destination, "first\nsecond\n").unwrap();
    let mut editor = editor("original\n");
    begin(&mut editor, "group", "one", line_range(0, 1), "Explain");
    complete(&mut editor, "one", None, "See another file").await;
    editor
        .open_inline_history(
            &mut RenderBuffer::new(100, 30, &Style::default()),
            &mut Runtime::new(),
        )
        .await
        .unwrap();
    run_history_action(
        &mut editor,
        HistoryAction::FollowFile {
            path: destination.to_string_lossy().into_owned(),
            line: Some(2),
            column: Some(2),
        },
    )
    .await;
    assert!(editor.inline_history_browser.is_none());
    assert_eq!(
        editor.current_buffer().file.as_deref(),
        destination.to_str()
    );
    assert_eq!(editor.buffer_line(), 1);
    assert_eq!(editor.current_buffer().contents(), "first\nsecond\n");
    assert_eq!(
        std::fs::read_to_string(destination).unwrap(),
        "first\nsecond\n"
    );
    assert_eq!(
        editor.inline_history.turn("one").unwrap().state,
        InlineTurnState::Completed
    );
}

#[tokio::test]
async fn inline_history_open_selects_the_requested_overlapping_result() {
    let mut editor = editor("alpha\nbeta\ngamma\n");
    let target = line_range(0, 3);
    let mut frame = RenderBuffer::new(100, 30, &Style::default());
    let mut runtime = Runtime::new();
    begin(&mut editor, "greeting", "hello", target, "hi");
    editor.inline_history.append_answer("hello", "Hello there");
    editor
        .apply_inline_result(
            "hello",
            "provider",
            &InlineAssistResult {
                expanded_scope: None,
                replacement: None,
                needs_agent: None,
                comments: Vec::new(),
            },
            &mut frame,
            &mut runtime,
        )
        .await
        .unwrap();
    editor.park_inline_assist();
    begin(
        &mut editor,
        "explanation",
        "explain",
        target,
        "Explain the function",
    );
    editor
        .apply_inline_result(
            "explain",
            "provider",
            &InlineAssistResult {
                expanded_scope: None,
                replacement: None,
                needs_agent: None,
                comments: (1..=3)
                    .map(|line| InlineCommentInput {
                        start_line: line,
                        end_line: None,
                        message: format!("Explanation {line}"),
                    })
                    .collect(),
            },
            &mut frame,
            &mut runtime,
        )
        .await
        .unwrap();
    editor
        .open_inline_job("greeting", &mut frame, &mut runtime)
        .await
        .unwrap();
    let greeting = editor.current_inline_comment_id().unwrap();
    assert!(
        editor.inline_comment_display_messages(editor.current_buffer())[0]
            .1
            .contains("Hello there")
    );

    editor
        .open_inline_history(&mut frame, &mut runtime)
        .await
        .unwrap();
    let index = editor
        .history_rows()
        .iter()
        .position(|row| row.key == HistoryKey::Turn("explain".into()))
        .unwrap();
    run_history_action(&mut editor, HistoryAction::Select(index)).await;
    run_history_action(&mut editor, HistoryAction::Open).await;
    assert_eq!(
        editor.inline_assist.as_ref().unwrap().annotation_group_id,
        "explanation"
    );
    let (selected, ordinal, count) = editor.current_inline_navigation().unwrap();
    assert_ne!(selected, greeting);
    assert_eq!((ordinal, count), (2, 4));
    assert!(
        editor.inline_comment_display_messages(editor.current_buffer())[0]
            .1
            .contains("Explanation 1")
    );
    let previous = editor
        .current_dialog
        .as_mut()
        .unwrap()
        .handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('['),
            KeyModifiers::NONE,
        )));
    assert_eq!(
        previous,
        Some(KeyAction::Single(
            Action::NavigateOverlappingInlineComment {
                id: selected,
                backwards: true,
                open: true
            }
        ))
    );
    editor
        .execute(&Action::ViewInlineAssistAnswer, &mut frame, &mut runtime)
        .await
        .unwrap();
    assert_eq!(
        editor
            .current_dialog
            .as_mut()
            .unwrap()
            .handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Char('['),
                KeyModifiers::NONE,
            ))),
        previous
    );
    let Some(KeyAction::Single(previous)) = previous else {
        panic!("expected navigation")
    };
    editor
        .execute(&previous, &mut frame, &mut runtime)
        .await
        .unwrap();
    assert_eq!(editor.current_inline_comment_id(), Some(greeting));
    assert_eq!(
        editor.inline_assist.as_ref().unwrap().annotation_group_id,
        "greeting"
    );
    assert_eq!(editor.current_buffer().contents(), "alpha\nbeta\ngamma\n");
}

#[tokio::test]
async fn inline_history_opens_the_selected_older_answer_not_the_latest() {
    let mut editor = editor("alpha\n");
    begin(
        &mut editor,
        "conversation",
        "first",
        line_range(0, 1),
        "First question",
    );
    complete(&mut editor, "first", None, "First answer").await;
    begin(
        &mut editor,
        "conversation",
        "second",
        line_range(0, 1),
        "Second question",
    );
    complete(&mut editor, "second", None, "Second answer").await;
    editor
        .open_inline_history(
            &mut RenderBuffer::new(100, 30, &Style::default()),
            &mut Runtime::new(),
        )
        .await
        .unwrap();
    run_history_action(&mut editor, HistoryAction::Expand).await;
    let index = editor
        .history_rows()
        .iter()
        .position(|row| row.key == HistoryKey::Turn("first".into()))
        .unwrap();
    run_history_action(&mut editor, HistoryAction::Select(index)).await;
    run_history_action(&mut editor, HistoryAction::Open).await;
    let mut frame = RenderBuffer::new(100, 30, &Style::default());
    editor.render(&mut frame).unwrap();
    let text = frame.cells.iter().map(|cell| cell.c).collect::<String>();
    assert!(text.contains("First answer"));
    assert!(!text.contains("Second answer"));
    assert_eq!(
        editor
            .current_dialog
            .as_mut()
            .unwrap()
            .handle_event(&Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))),
        Some(KeyAction::Single(Action::OpenInlineHistory))
    );
    assert_eq!(editor.current_buffer().contents(), "alpha\n");
}

#[tokio::test]
async fn inline_history_unifies_drafts_running_jobs_ready_edits_and_completed_turns() {
    let mut editor = editor("alpha\nbeta\ngamma\n");
    begin(
        &mut editor,
        "done",
        "done",
        line_range(0, 1),
        "Explain alpha",
    );
    complete(&mut editor, "done", None, "Alpha note").await;
    editor.close_inline_assist_session();
    begin(
        &mut editor,
        "ready",
        "ready",
        line_range(1, 2),
        "Rename beta",
    );
    editor.park_inline_assist();
    editor.stage_background_inline_result(
        "ready",
        "provider",
        InlineAssistResult {
            expanded_scope: None,
            replacement: Some("BETA\n".into()),
            comments: Vec::new(),
            needs_agent: None,
        },
    );
    begin(
        &mut editor,
        "running",
        "running",
        line_range(2, 3),
        "Explain gamma",
    );
    editor.park_inline_assist();
    let mut frame = RenderBuffer::new(100, 30, &Style::default());
    let mut runtime = Runtime::new();
    editor
        .execute(&Action::InlineAssist, &mut frame, &mut runtime)
        .await
        .unwrap();
    let draft_group = editor
        .inline_assist
        .as_ref()
        .unwrap()
        .annotation_group_id
        .clone();
    editor
        .current_dialog
        .as_mut()
        .unwrap()
        .handle_event(&Event::Paste("polychromatic zebras".into()));
    editor
        .execute(&Action::HideInlineAssist, &mut frame, &mut runtime)
        .await
        .unwrap();
    editor
        .execute(&Action::SaveInlineAssistDraft, &mut frame, &mut runtime)
        .await
        .unwrap();
    editor
        .execute(&Action::OpenInlineActivity, &mut frame, &mut runtime)
        .await
        .unwrap();
    assert!(editor.current_dialog.as_ref().unwrap().is_inline_history());
    let rows = editor.history_rows();
    assert_eq!(
        rows.iter()
            .map(|row| row.group.as_str())
            .collect::<Vec<_>>(),
        vec!["running", "ready", &draft_group, "done"]
    );
    assert!(rows[0].running);
    assert!(rows[1].label.contains("sample.c:2–2"));
    assert!(rows[1].label.contains("beta"));
    run_history_action(&mut editor, HistoryAction::Query("plychrmtczbrs".into())).await;
    assert_eq!(editor.history_rows().len(), 1);
    assert_eq!(
        editor.history_rows()[0].key,
        HistoryKey::Draft(draft_group.clone())
    );
    run_history_action(&mut editor, HistoryAction::Open).await;
    assert!(editor.inline_history_browser.is_none());
    assert_eq!(
        editor.inline_assist.as_ref().unwrap().annotation_group_id,
        draft_group
    );
    assert!(
        matches!(editor.current_dialog.as_ref().unwrap().inline_assist_state(),
        Some(InlineAssistPopupState::Prompt { initial, .. }) if initial == "polychromatic zebras")
    );
    assert_eq!(editor.current_buffer().contents(), "alpha\nbeta\ngamma\n");
}

#[tokio::test]
async fn inline_history_live_refresh_preserves_selection_scroll_and_source_position() {
    let mut editor = editor("alpha\nbeta\n");
    begin(&mut editor, "one", "one", line_range(0, 1), "First request");
    editor.park_inline_assist();
    begin(
        &mut editor,
        "two",
        "two",
        line_range(1, 2),
        "Second request",
    );
    let mut frame = RenderBuffer::new(100, 30, &Style::default());
    let mut runtime = Runtime::new();
    editor
        .open_inline_history(&mut frame, &mut runtime)
        .await
        .unwrap();
    let index = editor
        .history_rows()
        .iter()
        .position(|row| row.key == HistoryKey::Turn("one".into()))
        .unwrap();
    run_history_action(&mut editor, HistoryAction::Select(index)).await;
    editor.inline_history_browser.as_mut().unwrap().scroll = 1;
    let cursor = editor.cursor_snapshot();
    let viewport = (editor.vtop, editor.vleft, editor.skipcol);
    let selected = editor
        .inline_history_browser
        .as_ref()
        .unwrap()
        .selected
        .clone();
    let animation_started = editor
        .inline_history_browser
        .as_ref()
        .unwrap()
        .animation_started;
    editor.inline_history.append_answer(
        "one",
        &format!(
            "Streaming answer now visible\n\n{}",
            "More detail.\n\n".repeat(20)
        ),
    );
    editor.mark_inline_history_dirty();
    editor.stage_background_inline_result(
        "two",
        "provider",
        InlineAssistResult {
            expanded_scope: None,
            replacement: None,
            comments: Vec::new(),
            needs_agent: None,
        },
    );
    editor.inline_history_browser.as_mut().unwrap().refreshed_at =
        Instant::now() - Duration::from_secs(1);
    editor
        .refresh_live_inline_history(&mut frame, &mut runtime)
        .await
        .unwrap();
    let browser = editor.inline_history_browser.as_ref().unwrap();
    assert_eq!(browser.selected, selected);
    assert_eq!(browser.scroll, 1);
    assert_eq!(browser.animation_started, animation_started);
    assert!(!browser.dirty);
    assert_eq!(editor.cursor_snapshot(), cursor);
    assert_eq!((editor.vtop, editor.vleft, editor.skipcol), viewport);
    assert!(frame
        .cells
        .iter()
        .map(|cell| cell.c)
        .collect::<String>()
        .contains("Streaming answer now visible"));
    assert_eq!(
        editor.inline_history.turn("two").unwrap().state,
        InlineTurnState::Completed
    );
}

#[tokio::test]
async fn inline_history_restores_dismissed_annotations_without_reapplying_the_edit() {
    let mut editor = editor("alpha\nbeta\n");
    begin(&mut editor, "one", "edit", line_range(0, 1), "Rename alpha");
    complete(&mut editor, "edit", Some("renamed\n"), "Renamed value").await;
    let revision = editor.current_buffer().revision();
    let transaction = editor
        .current_buffer()
        .undo_history
        .latest_transaction()
        .unwrap()
        .id
        .clone();
    editor.clear_inline_comments();
    let mut frame = RenderBuffer::new(100, 30, &Style::default());
    let mut runtime = Runtime::new();
    editor
        .execute(
            &Action::RestoreInlineAssistAnnotations,
            &mut frame,
            &mut runtime,
        )
        .await
        .unwrap();
    assert_eq!(editor.inline_comment_group_count("one"), 1);
    assert!(editor
        .inline_history
        .turn("edit")
        .unwrap()
        .hidden_comments
        .is_empty());
    editor.clear_inline_comments();
    editor
        .open_inline_history(&mut frame, &mut runtime)
        .await
        .unwrap();
    run_history_action(&mut editor, HistoryAction::ShowAnnotations).await;
    assert!(editor.inline_history_browser.is_none());
    assert_eq!(
        editor.inline_comment_display_messages(editor.current_buffer()),
        vec![(0, "‹ 2/2 › · Space v\nRenamed value".into())]
    );
    assert_eq!(editor.current_buffer().contents(), "renamed\nbeta\n");
    assert_eq!(editor.current_buffer().revision(), revision);
    assert_eq!(
        editor
            .current_buffer()
            .undo_history
            .latest_transaction()
            .unwrap()
            .id,
        transaction
    );
}

#[tokio::test]
async fn inline_history_restores_an_older_turn_and_remembers_it_across_recovery() {
    let mut editor = editor("alpha\nbeta\n");
    begin(
        &mut editor,
        "one",
        "first",
        line_range(0, 1),
        "First request",
    );
    complete(&mut editor, "first", None, "First annotation").await;
    begin(
        &mut editor,
        "one",
        "second",
        line_range(0, 1),
        "Second request",
    );
    complete(&mut editor, "second", None, "Second annotation").await;
    editor.close_inline_assist_session();
    editor.clear_inline_comments();
    editor.inline_history.conversations[0].resolved = true;
    let mut frame = RenderBuffer::new(100, 30, &Style::default());
    let mut runtime = Runtime::new();
    editor
        .open_inline_history(&mut frame, &mut runtime)
        .await
        .unwrap();
    run_history_action(&mut editor, HistoryAction::Expand).await;
    let index = editor
        .history_rows()
        .iter()
        .position(|row| row.key == HistoryKey::Turn("first".into()))
        .unwrap();
    run_history_action(&mut editor, HistoryAction::Select(index)).await;
    run_history_action(&mut editor, HistoryAction::ShowAnnotations).await;
    assert!(!editor.inline_history.conversations[0].resolved);
    assert_eq!(
        editor.inline_history.conversations[0]
            .visible_request
            .as_deref(),
        Some("first")
    );
    assert_eq!(
        editor.inline_history.turn("first").unwrap().disposition,
        InlineDisposition::Superseded
    );
    assert_eq!(
        editor.inline_history.turn("second").unwrap().disposition,
        InlineDisposition::Kept
    );
    assert_eq!(
        editor.inline_comment_display_messages(editor.current_buffer()),
        vec![(0, "First annotation".into())]
    );
    let mut restored = self::editor("alpha\nbeta\n");
    restored.inline_history =
        serde_json::from_slice(&serde_json::to_vec(&editor.inline_history).unwrap()).unwrap();
    restored.inline_history.validate().unwrap();
    restored.inline_history.recover();
    restored.restore_inline_history_comments();
    assert_eq!(
        restored.inline_comment_display_messages(restored.current_buffer()),
        vec![(0, "First annotation".into())]
    );
    restored.clear_inline_comments();
    restored.restore_inline_history_comments();
    assert!(restored
        .inline_comment_display_messages(restored.current_buffer())
        .is_empty());
    assert!(!restored.current_buffer().is_dirty());
}

#[tokio::test]
async fn inline_history_restoration_skips_detached_ranges_and_marks_changed_source() {
    let mut editor = editor("alpha\nbeta\ngamma\n");
    begin(
        &mut editor,
        "one",
        "one",
        line_range(0, 2),
        "Review both lines",
    );
    let result = InlineAssistResult {
        expanded_scope: None,
        replacement: None,
        needs_agent: None,
        comments: vec![
            InlineCommentInput {
                start_line: 1,
                end_line: None,
                message: "Alpha note".into(),
            },
            InlineCommentInput {
                start_line: 2,
                end_line: None,
                message: "Beta note".into(),
            },
        ],
    };
    let mut frame = RenderBuffer::new(100, 30, &Style::default());
    let mut runtime = Runtime::new();
    editor
        .apply_inline_result("one", "provider", &result, &mut frame, &mut runtime)
        .await
        .unwrap();
    editor.close_inline_assist_session();
    editor.clear_inline_comments();
    editor.begin_transaction("change reviewed source");
    editor.replace_range(line_range(1, 2), "");
    editor.replace_range(
        TextRange::new(TextPosition::new(0, 0), TextPosition::new(0, 5)),
        "ALPHA",
    );
    assert!(editor.commit_transaction(editor.cursor_snapshot()));
    let revision = editor.current_buffer().revision();
    assert!(editor
        .show_inline_history_annotations("one", &mut frame, &mut runtime)
        .await
        .unwrap());
    assert_eq!(editor.inline_comment_group_count("one"), 1);
    assert!(editor
        .inline_comments
        .iter()
        .any(|comment| comment.message == "Alpha note" && comment.stale));
    assert_eq!(editor.current_buffer().revision(), revision);
    assert_eq!(editor.current_buffer().contents(), "ALPHA\ngamma\n");
}

#[tokio::test]
async fn inline_history_dismiss_after_restoring_an_older_turn_stays_hidden() {
    let mut editor = editor("alpha\nbeta\n");
    begin(
        &mut editor,
        "one",
        "first",
        line_range(0, 1),
        "First request",
    );
    complete(&mut editor, "first", None, "First annotation").await;
    begin(
        &mut editor,
        "one",
        "second",
        line_range(0, 1),
        "Second request",
    );
    complete(&mut editor, "second", None, "Second annotation").await;
    editor.close_inline_assist_session();
    let mut frame = RenderBuffer::new(100, 30, &Style::default());
    let mut runtime = Runtime::new();
    assert!(editor
        .show_inline_history_annotations("first", &mut frame, &mut runtime)
        .await
        .unwrap());
    editor
        .open_inline_job("one", &mut frame, &mut runtime)
        .await
        .unwrap();
    editor
        .execute(&Action::UndoInlineAssist, &mut frame, &mut runtime)
        .await
        .unwrap();
    editor.restore_inline_history_comments();
    assert_eq!(
        editor.inline_history.turn("first").unwrap().hidden_comments,
        vec![0]
    );
    assert!(editor
        .inline_comment_display_messages(editor.current_buffer())
        .is_empty());
    assert_eq!(editor.current_buffer().contents(), "alpha\nbeta\n");
}
