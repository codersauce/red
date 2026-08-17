use super::*;
use crate::{inline_assist::InlineCommentInput, lsp::LspManager};

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

fn range(start: usize, end: usize) -> TextRange {
    TextRange::new(TextPosition::new(start, 0), TextPosition::new(end, 0))
}

fn start(editor: &mut Editor, group: &str, request: Option<&str>, target: TextRange) {
    editor.park_inline_assist();
    editor.inline_assist = Some(InlineAssistSession {
        parent_comment: None,
        allow_expansion: false,
        buffer_id: editor.current_buffer().id(),
        window_id: editor.window_manager.active_stable_window_id().unwrap(),
        expected_revision: editor.current_buffer().revision(),
        range: target,
        expected_text: editor.current_buffer().text_in_range(target),
        scope: "test".into(),
        request_id: request.map(str::to_string),
        session_id: request.map(|request| format!("provider-{request}")),
        transaction_id: None,
        annotation_group_id: group.into(),
        has_result: false,
        result_request_id: None,
    });
    let state = if let Some(request) = request {
        editor
            .begin_inline_history_turn(request, &format!("Question {request}"), target)
            .unwrap();
        InlineAssistPopupState::Working
    } else {
        InlineAssistPopupState::Prompt {
            initial: "Saved question".into(),
            refining: false,
        }
    };
    editor.current_dialog = Some(Box::new(editor.inline_assist_popup("test", state)));
    editor.sync_inline_activity();
}

fn result(replacement: Option<&str>) -> InlineAssistResult {
    InlineAssistResult {
        expanded_scope: None,
        needs_agent: None,
        replacement: replacement.map(str::to_string),
        comments: vec![InlineCommentInput {
            start_line: 1,
            end_line: None,
            message: "Retained answer".into(),
        }],
    }
}

async fn action(editor: &mut Editor, action: Action) {
    editor
        .execute(
            &action,
            &mut RenderBuffer::new(100, 30, &Style::default()),
            &mut Runtime::new(),
        )
        .await
        .unwrap();
}

fn popup(editor: &Editor) -> InlineAssistPopupState {
    editor
        .current_dialog
        .as_ref()
        .unwrap()
        .inline_assist_state()
        .unwrap()
}

#[tokio::test]
async fn inline_draft_survives_click_away_and_source_relocation() {
    let mut editor = editor("alpha\nbeta\n");
    start(&mut editor, "draft", None, range(1, 2));
    editor
        .current_dialog
        .as_mut()
        .unwrap()
        .handle_event(&Event::Paste(" extended".into()));
    let click = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: u16::MAX,
        row: u16::MAX,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(
        editor.current_dialog.as_mut().unwrap().handle_event(&click),
        Some(KeyAction::Single(Action::HideInlineAssist))
    );
    action(&mut editor, Action::HideInlineAssist).await;
    assert!(editor.inline_assist.is_some());
    assert!(!editor.has_parked_inline_draft("draft"));
    action(&mut editor, Action::SaveInlineAssistDraft).await;
    assert!(editor.inline_assist.is_none());
    assert!(editor.has_parked_inline_draft("draft"));
    editor.begin_transaction("insert above");
    editor.replace_range(TextRange::insertion(TextPosition::new(0, 0)), "new\n");
    assert!(editor.commit_transaction(editor.cursor_snapshot()));
    action(&mut editor, Action::OpenInlineJob("draft".into())).await;
    assert_eq!(editor.inline_assist.as_ref().unwrap().range, range(2, 3));
    assert!(
        matches!(popup(&editor), InlineAssistPopupState::Prompt { initial, .. } if initial == "Saved question extended")
    );
}

#[tokio::test]
async fn inline_empty_prompt_closes_without_creating_a_draft() {
    let mut editor = editor("alpha\n");
    for text in ["", "   "] {
        action(&mut editor, Action::InlineAssist).await;
        editor
            .current_dialog
            .as_mut()
            .unwrap()
            .handle_event(&Event::Paste(text.into()));
        action(&mut editor, Action::HideInlineAssist).await;
        assert!(editor.current_dialog.is_none());
        assert!(editor.inline_assist.is_none());
        assert!(editor.inline_jobs.is_empty());
        assert!(editor.inline_comments.is_empty());
    }
    action(&mut editor, Action::InlineAssist).await;
    action(&mut editor, Action::OpenInlineHistory).await;
    assert!(editor.inline_jobs.is_empty());
    assert!(editor.history_rows().is_empty());
}

#[tokio::test]
async fn inline_draft_close_can_edit_save_or_delete_without_losing_prior_results() {
    let mut editor = editor("alpha\n");
    start(&mut editor, "discussion", Some("one"), range(0, 1));
    editor.park_inline_assist();
    editor.stage_background_inline_result("one", "provider-one", result(None));
    action(&mut editor, Action::OpenInlineJob("discussion".into())).await;
    action(&mut editor, Action::RefineInlineAssist).await;
    editor
        .current_dialog
        .as_mut()
        .unwrap()
        .handle_event(&Event::Paste("unfinished follow-up".into()));
    action(&mut editor, Action::HideInlineAssist).await;
    let edit = editor
        .current_dialog
        .as_mut()
        .unwrap()
        .handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('e'),
            KeyModifiers::NONE,
        )));
    assert_eq!(edit, Some(KeyAction::Single(Action::Refresh)));
    assert!(
        matches!(popup(&editor), InlineAssistPopupState::Prompt { initial, .. } if initial == "unfinished follow-up")
    );
    action(&mut editor, Action::HideInlineAssist).await;
    assert_eq!(
        editor
            .current_dialog
            .as_mut()
            .unwrap()
            .handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE
            ))),
        Some(KeyAction::Single(Action::DiscardInlineAssistDraft))
    );
    action(&mut editor, Action::DiscardInlineAssistDraft).await;
    assert!(editor.current_dialog.is_none());
    assert!(!editor.has_parked_inline_draft("discussion"));
    assert_eq!(
        editor.inline_history.turn("one").unwrap().state,
        InlineTurnState::Completed
    );
    assert_eq!(editor.inline_comment_group_count("discussion"), 1);
    action(&mut editor, Action::OpenInlineJob("discussion".into())).await;
    action(&mut editor, Action::RefineInlineAssist).await;
    editor.park_inline_assist();
    assert!(!editor.has_parked_inline_draft("discussion"));
    assert!(editor.inline_jobs.contains_key("discussion"));
    assert_eq!(editor.inline_comment_group_count("discussion"), 1);
}

#[tokio::test]
async fn inline_background_result_waits_for_explicit_apply_and_keeps_other_jobs() {
    let mut editor = editor("alpha\nbeta\n");
    start(&mut editor, "first", Some("one"), range(0, 1));
    start(&mut editor, "second", Some("two"), range(1, 2));
    assert!(editor.inline_submission_target().is_err());
    action(&mut editor, Action::OpenInlineActivity).await;
    editor.stage_background_inline_result("one", "provider-one", result(Some("ALPHA\n")));
    assert!(editor.current_dialog.as_ref().unwrap().is_inline_history());
    assert_eq!(editor.current_buffer().contents(), "alpha\nbeta\n");
    assert!(!editor.current_buffer().is_dirty());
    assert_eq!(
        editor.inline_history.turn("one").unwrap().state,
        InlineTurnState::Ready
    );
    assert!(editor
        .inline_history
        .turn("one")
        .unwrap()
        .answer_text()
        .contains("not applied"));
    let items = editor.history_rows();
    assert_eq!(items[0].group, "second");
    assert!(items[0].running);
    assert!(items[1].label.contains("sample.c:1–1"));
    assert!(items[1].label.contains("alpha"));
    action(
        &mut editor,
        Action::InlineHistoryAction(crate::inline_history::HistoryAction::Close),
    )
    .await;
    assert_eq!(
        editor.inline_job_on_comment_line(0).as_deref(),
        Some("first")
    );
    action(&mut editor, Action::OpenInlineJob("first".into())).await;
    assert_eq!(
        popup(&editor),
        InlineAssistPopupState::Ready { stale: false }
    );
    action(&mut editor, Action::ApplyPendingInlineAssist).await;
    assert_eq!(editor.current_buffer().contents(), "ALPHA\nbeta\n");
    assert_eq!(
        editor.inline_history.turn("one").unwrap().state,
        InlineTurnState::Completed
    );
    action(&mut editor, Action::ApplyPendingInlineAssist).await;
    assert_eq!(editor.current_buffer().contents(), "ALPHA\nbeta\n");
    assert_eq!(
        editor.inline_history.turn("two").unwrap().state,
        InlineTurnState::Pending
    );
    assert!(editor.inline_jobs.contains_key("second"));
}

#[tokio::test]
async fn inline_stale_result_is_retained_and_recheck_uses_tracked_source() {
    let mut editor = editor("alpha\nbeta\n");
    start(&mut editor, "first", Some("one"), range(0, 1));
    action(&mut editor, Action::HideInlineAssist).await;
    editor.begin_transaction("insert above");
    editor.replace_range(TextRange::insertion(TextPosition::new(0, 0)), "new\n");
    assert!(editor.commit_transaction(editor.cursor_snapshot()));
    editor.stage_background_inline_result("one", "provider-one", result(Some("ALPHA\n")));
    action(&mut editor, Action::OpenInlineJob("first".into())).await;
    assert_eq!(
        popup(&editor),
        InlineAssistPopupState::Ready { stale: true }
    );
    assert_eq!(editor.inline_submission_target().unwrap(), range(1, 2));
    action(&mut editor, Action::RefineInlineAssist).await;
    assert!(
        matches!(popup(&editor), InlineAssistPopupState::Prompt { initial, .. } if initial.contains("Question one"))
    );
    action(&mut editor, Action::CancelInlineAssistRefine).await;
    assert_eq!(
        popup(&editor),
        InlineAssistPopupState::Ready { stale: true }
    );
    assert!(editor
        .apply_inline_result(
            "one",
            "provider-one",
            &result(Some("ALPHA\n")),
            &mut RenderBuffer::new(100, 30, &Style::default()),
            &mut Runtime::new()
        )
        .await
        .is_err());
    assert_eq!(editor.current_buffer().contents(), "new\nalpha\nbeta\n");
    editor.begin_transaction("delete target");
    editor.replace_range(range(1, 2), "");
    assert!(editor.commit_transaction(editor.cursor_snapshot()));
    assert!(editor.inline_submission_target().is_err());
}

#[tokio::test]
async fn inline_cancel_and_late_results_are_isolated_to_their_request() {
    let mut editor = editor("alpha\nbeta\n");
    start(&mut editor, "first", Some("one"), range(0, 1));
    start(&mut editor, "second", Some("two"), range(1, 2));
    action(&mut editor, Action::CancelInlineAssist).await;
    assert_eq!(
        editor.inline_history.turn("two").unwrap().state,
        InlineTurnState::Cancelled
    );
    editor.stage_background_inline_result("two", "provider-two", result(Some("BETA\n")));
    editor.stage_background_inline_result("one", "wrong-provider", result(None));
    assert_eq!(
        editor.inline_history.turn("one").unwrap().state,
        InlineTurnState::Pending
    );
    editor.stage_background_inline_result("one", "provider-one", result(None));
    editor.record_inline_failure("one", "late failure");
    editor.stage_background_inline_result("one", "provider-one", result(Some("wrong\n")));
    assert_eq!(
        editor.inline_history.turn("one").unwrap().state,
        InlineTurnState::Completed
    );
    assert!(editor
        .inline_history
        .turn("one")
        .unwrap()
        .result
        .as_ref()
        .unwrap()
        .replacement
        .is_none());
    assert_eq!(editor.current_buffer().contents(), "alpha\nbeta\n");
}

#[tokio::test]
async fn inline_ready_result_survives_recovery_and_can_be_applied_once() {
    let mut original = editor("alpha\nbeta\n");
    start(&mut original, "first", Some("one"), range(0, 1));
    original.park_inline_assist();
    original.stage_background_inline_result("one", "provider-one", result(Some("ALPHA\n")));
    start(&mut original, "second", Some("two"), range(1, 2));
    let mut recovered = editor("alpha\nbeta\n");
    recovered.inline_history =
        serde_json::from_str(&serde_json::to_string(&original.inline_history).unwrap()).unwrap();
    recovered.inline_history.recover();
    assert_eq!(
        recovered.inline_history.turn("one").unwrap().state,
        InlineTurnState::Ready
    );
    assert_eq!(
        recovered.inline_history.turn("two").unwrap().state,
        InlineTurnState::Cancelled
    );
    action(&mut recovered, Action::OpenInlineJob("first".into())).await;
    assert_eq!(
        popup(&recovered),
        InlineAssistPopupState::Ready { stale: false }
    );
    action(&mut recovered, Action::ApplyPendingInlineAssist).await;
    assert_eq!(recovered.current_buffer().contents(), "ALPHA\nbeta\n");
    action(&mut recovered, Action::ApplyPendingInlineAssist).await;
    assert_eq!(
        recovered.inline_history.turn("one").unwrap().state,
        InlineTurnState::Completed
    );
}

#[tokio::test]
async fn inline_activity_marker_reopens_with_the_mouse_and_survives_comment_clear() {
    let mut editor = editor("alpha\nbeta\n");
    start(&mut editor, "first", Some("one"), range(1, 2));
    action(&mut editor, Action::HideInlineAssist).await;
    action(&mut editor, Action::ClearInlineComments).await;
    assert_eq!(
        editor.inline_job_on_comment_line(1).as_deref(),
        Some("first")
    );
    editor.sync_to_window();
    let window = editor.active_window_with_editor_view().unwrap();
    let row = editor.layout_for_window(&window).inline_comments[0].row;
    editor
        .test_execute_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: (window.position.x + editor.gutter_width_for_window(&window) + 2) as u16,
            row: editor.window_to_terminal_y(&window, row) as u16,
            modifiers: KeyModifiers::NONE,
        }))
        .await
        .unwrap();
    assert_eq!(
        editor.inline_assist.as_ref().unwrap().annotation_group_id,
        "first"
    );
    assert_eq!(popup(&editor), InlineAssistPopupState::Working);
    assert!(!editor.current_buffer().is_dirty());
}

#[tokio::test]
async fn inline_background_explanation_publishes_in_its_own_buffer_without_taking_focus() {
    let mut editor = editor("alpha\nbeta\n");
    let original_id = editor.current_buffer().id();
    start(&mut editor, "first", Some("one"), range(0, 1));
    editor.park_inline_assist();
    let other = editor.buffer_manager.add_buffer(Buffer::new(
        Some("/workspace/other.c".into()),
        "other\n".into(),
    ));
    editor
        .set_current_buffer(&mut RenderBuffer::new(100, 30, &Style::default()), other)
        .await
        .unwrap();
    start(&mut editor, "second", Some("two"), range(0, 1));
    let cursor = editor.cursor_snapshot();
    let selected = editor.active_inline_comment;
    editor.stage_background_inline_result("one", "provider-one", result(None));
    assert_eq!(editor.buffer_manager.active_index(), other);
    assert_eq!(editor.cursor_snapshot(), cursor);
    assert_eq!(editor.active_inline_comment, selected);
    assert_eq!(popup(&editor), InlineAssistPopupState::Working);
    assert_eq!(
        editor.inline_assist.as_ref().unwrap().request_id.as_deref(),
        Some("two")
    );
    assert_eq!(
        editor.inline_history.turn("one").unwrap().state,
        InlineTurnState::Completed
    );
    assert_eq!(
        editor
            .inline_history
            .turn("one")
            .unwrap()
            .comment_locations
            .len(),
        1
    );
    let original = editor
        .buffer_manager
        .iter()
        .find(|buffer| buffer.id() == original_id)
        .unwrap();
    assert_eq!(
        editor.inline_comment_display_messages(original),
        vec![(0, "Retained answer".into())]
    );
    assert_eq!(original.contents(), "alpha\nbeta\n");
    assert!(!original.is_dirty());
    assert_eq!(editor.current_buffer().contents(), "other\n");
    assert!(!editor.current_buffer().is_dirty());
    editor.inline_comments.clear();
    editor.restore_inline_history_comments();
    assert!(editor
        .inline_comments
        .iter()
        .any(|comment| comment.anchor.buffer_id == original_id
            && comment.message == "Retained answer"));
}

#[tokio::test]
async fn inline_old_ready_explanations_publish_when_reopened_but_edits_still_wait() {
    let mut editor = editor("alpha\nbeta\n");
    start(&mut editor, "first", Some("one"), range(0, 1));
    editor.park_inline_assist();
    let turn = editor.inline_history.turn_mut("one").unwrap();
    turn.state = InlineTurnState::Ready;
    turn.result = Some(result(Some("alpha\n")));
    turn.session_id = Some("provider-one".into());
    action(&mut editor, Action::OpenInlineActivity).await;
    assert_eq!(
        editor.inline_history.turn("one").unwrap().state,
        InlineTurnState::Completed
    );
    assert_eq!(
        editor.inline_comment_display_messages(editor.current_buffer()),
        vec![(0, "Retained answer".into())]
    );
    assert!(!editor.current_buffer().is_dirty());
    action(&mut editor, Action::ClearInlineComments).await;
    editor.sync_inline_activity();
    assert!(editor
        .inline_comment_display_messages(editor.current_buffer())
        .is_empty());
    action(
        &mut editor,
        Action::InlineHistoryAction(crate::inline_history::HistoryAction::Close),
    )
    .await;
    start(&mut editor, "second", Some("two"), range(1, 2));
    editor.park_inline_assist();
    editor.stage_background_inline_result("two", "provider-two", result(Some("BETA\n")));
    assert_eq!(
        editor.inline_history.turn("two").unwrap().state,
        InlineTurnState::Ready
    );
    assert_eq!(editor.current_buffer().contents(), "alpha\nbeta\n");
}

#[tokio::test]
async fn inline_changed_source_retains_the_answer_without_attaching_it_incorrectly() {
    let mut editor = editor("alpha\nbeta\n");
    start(&mut editor, "first", Some("one"), range(0, 1));
    editor.park_inline_assist();
    editor.begin_transaction("change source");
    editor.replace_range(range(0, 1), "changed\n");
    assert!(editor.commit_transaction(editor.cursor_snapshot()));
    editor.stage_background_inline_result("one", "provider-one", result(None));
    assert_eq!(
        editor.inline_history.turn("one").unwrap().status(),
        "answered"
    );
    assert_eq!(editor.inline_comment_group_count("first"), 0);
    assert!(editor
        .inline_comment_display_messages(editor.current_buffer())
        .iter()
        .any(|(_, message)| message.contains("Retained answer")));
    action(&mut editor, Action::OpenInlineJob("first".into())).await;
    assert!(matches!(
        popup(&editor),
        InlineAssistPopupState::AnswerRetained(_)
    ));
    assert_eq!(editor.current_buffer().contents(), "changed\nbeta\n");
    editor.begin_transaction("restore source");
    editor.replace_range(range(0, 1), "alpha\n");
    assert!(editor.commit_transaction(editor.cursor_snapshot()));
    action(&mut editor, Action::OpenInlineActivity).await;
    assert_eq!(
        editor.inline_history.turn("one").unwrap().state,
        InlineTurnState::Completed
    );
    assert_eq!(editor.inline_comment_group_count("first"), 1);
}

#[tokio::test]
async fn inline_running_marker_animates_at_the_shared_interval_and_stops_on_completion() {
    let mut editor = editor("alpha\nbeta\n");
    start(&mut editor, "first", Some("one"), range(0, 1));
    editor.park_inline_assist();
    let since = editor.inline_activity_animation.since;
    let interval = Duration::from_millis(SPINNER_FRAME_INTERVAL_MS);
    let cursor = editor.cursor_snapshot();
    let history = editor.inline_history.clone();
    let id = editor.active_inline_comment;
    assert!(editor.inline_comments[0].message.starts_with("⠋ Working"));
    assert!(!editor.poll_inline_activity_animation(since + interval - Duration::from_millis(1)));
    assert!(editor.poll_inline_activity_animation(since + interval));
    assert!(editor.inline_comments[0].message.starts_with("⠙ Working"));
    assert!(!editor.poll_inline_activity_animation(since + interval));
    assert!(editor.poll_inline_activity_animation(since + interval * 2));
    assert!(editor.inline_comments[0].message.starts_with("⠹ Working"));
    assert_eq!(editor.active_inline_comment, id);
    assert_eq!(editor.cursor_snapshot(), cursor);
    assert_eq!(editor.inline_history, history);
    assert!(!editor.current_buffer().is_dirty());
    editor.stage_background_inline_result("one", "provider-one", result(None));
    assert!(editor.inline_activity_animation.running.is_empty());
    assert!(!editor.poll_inline_activity_animation(since + interval * 3));
    assert_eq!(
        editor.inline_comment_display_messages(editor.current_buffer()),
        vec![(0, "Retained answer".into())]
    );
}

#[tokio::test]
async fn inline_spinner_tick_uses_decoration_repaint_without_rebuilding_the_surface() {
    let mut editor = editor("alpha\nbeta\n");
    start(&mut editor, "first", Some("one"), range(0, 1));
    editor.park_inline_assist();
    let mut frame = RenderBuffer::new(100, 30, &Style::default());
    editor.render(&mut frame).unwrap();
    let full_renders = editor.full_render_count;
    let generation = editor.render_generation;
    editor.inline_activity_animation.since =
        Instant::now() - Duration::from_millis(SPINNER_FRAME_INTERVAL_MS);
    editor
        .service_background(&mut frame, &mut Runtime::new())
        .await
        .unwrap();
    assert_eq!(editor.full_render_count, full_renders);
    assert_eq!(editor.render_generation, generation + 1);
    let mut full = frame.clone();
    editor.render(&mut full).unwrap();
    assert_eq!(frame.cells, full.cells);
}

#[test]
fn inline_spinner_does_not_request_repaints_for_offscreen_or_collapsed_markers() {
    let source = (0..80)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    let mut editor = editor(&source);
    start(&mut editor, "first", Some("one"), range(60, 61));
    editor.park_inline_assist();
    let since = editor.inline_activity_animation.since;
    let interval = Duration::from_millis(SPINNER_FRAME_INTERVAL_MS);
    let activity_id = editor.active_inline_comment;
    assert!(!editor.poll_inline_activity_animation(since + interval));
    editor.vtop = 60;
    editor.cy = 0;
    editor.sync_to_window();
    assert!(editor.poll_inline_activity_animation(since + interval * 2));
    let comment = editor.make_inline_comment(
        60,
        60,
        "Selected answer".into(),
        InlineCommentOrigin::Sample,
    );
    editor.active_inline_comment = Some(comment.id);
    editor.inline_comments.push(comment);
    editor.layout_cache.borrow_mut().clear();
    assert!(!editor.poll_inline_activity_animation(since + interval * 3));
    editor.active_inline_comment = activity_id;
    editor.layout_cache.borrow_mut().clear();
    assert!(editor.poll_inline_activity_animation(since + interval * 4));
}
