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
