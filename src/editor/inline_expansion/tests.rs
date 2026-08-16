use super::*;
use crate::{inline_assist::InlineCommentInput, lsp::LspManager};

const SOURCE: &str = "preamble\nbody\nhelper\ntail\n";

fn editor(source: &str) -> Editor {
    let config = Config::default();
    let mut editor = Editor::with_size(
        Box::new(LspManager::new(config.lsp.clone())),
        100,
        30,
        config,
        Theme::default(),
        vec![Buffer::new(Some("/workspace/main.c".into()), source.into())],
    )
    .unwrap();
    editor.test_disable_terminal_output();
    editor
}

fn target() -> TextRange {
    TextRange::new(TextPosition::new(1, 0), TextPosition::new(2, 0))
}

fn begin(editor: &mut Editor, allow_expansion: bool) {
    let range = target();
    editor.inline_assist = Some(InlineAssistSession {
        allow_expansion,
        buffer_id: editor.current_buffer().id(),
        window_id: editor.window_manager.active_stable_window_id().unwrap(),
        expected_revision: editor.current_buffer().revision(),
        range,
        expected_text: editor.current_buffer().text_in_range(range),
        scope: "line 2".into(),
        request_id: Some("request".into()),
        session_id: Some("provider".into()),
        transaction_id: None,
        annotation_group_id: "discussion".into(),
        has_result: false,
        result_request_id: None,
    });
    editor
        .begin_inline_history_turn("request", "Update the body and helper", range)
        .unwrap();
    editor.current_dialog = Some(Box::new(
        editor.inline_assist_popup("line 2", InlineAssistPopupState::Working),
    ));
}

fn proposal(editor: &Editor) -> InlineAssistResult {
    InlineAssistResult::from_tool("propose_expanded_replacement", json!({
        "start_line":2, "end_line":3, "expected_revision":editor.current_buffer().revision(),
        "before":"body\nhelper\n", "replacement":"new body\nnew helper\n", "reason":"Update the helper with its caller.",
        "comments":[{"start_line":2,"message":"Updated helper"}]
    })).unwrap()
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

fn enter(editor: &mut Editor) -> Option<KeyAction> {
    editor
        .current_dialog
        .as_mut()
        .unwrap()
        .handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
}

#[tokio::test]
async fn inline_expansion_requires_review_and_applies_as_one_unsaved_undo() {
    let mut editor = editor(SOURCE);
    begin(&mut editor, true);
    let result = proposal(&editor);
    assert!(editor
        .apply_inline_result(
            "request",
            "provider",
            &result,
            &mut RenderBuffer::new(100, 30, &Style::default()),
            &mut Runtime::new()
        )
        .await
        .is_err());
    let (bridge, worker) = CodexBridge::channel(std::num::NonZeroUsize::new(2).unwrap());
    editor.agent_manager.set_bridge(bridge);
    worker
        .send(CodexEvent::InlineResult {
            request_id: "request".into(),
            session_id: "provider".into(),
            result,
        })
        .await
        .unwrap();
    editor
        .service_background(
            &mut RenderBuffer::new(100, 30, &Style::default()),
            &mut Runtime::new(),
        )
        .await
        .unwrap();
    assert!(matches!(
        editor.inline_assist_result_state(),
        InlineAssistPopupState::WiderReady { stale: false, .. }
    ));
    assert_eq!(editor.current_buffer().contents(), SOURCE);
    assert_eq!(editor.inline_submission_target().unwrap(), target());
    assert_eq!(
        editor.inline_history.turn("request").unwrap().before,
        "body\n"
    );
    assert!(editor
        .current_buffer()
        .undo_history
        .latest_transaction()
        .is_none());
    assert_eq!(
        enter(&mut editor),
        Some(KeyAction::Single(Action::ViewInlineAssistAnswer))
    );
    action(&mut editor, Action::ApplyPendingInlineAssist).await;
    assert_eq!(editor.current_buffer().contents(), SOURCE);
    let mut frame = RenderBuffer::new(100, 30, &Style::default());
    editor.render(&mut frame).unwrap();
    let text = frame.cells.iter().map(|cell| cell.c).collect::<String>();
    assert!(text.contains("Review wider edit"));
    assert!(text.contains("+new helper"));
    let approval = enter(&mut editor);
    assert_eq!(
        approval,
        Some(KeyAction::Single(Action::ApplyReviewedInlineAssist(
            "request".into()
        )))
    );
    let Some(KeyAction::Single(approval)) = approval else {
        panic!("missing approval")
    };
    action(&mut editor, approval).await;
    assert_eq!(
        editor.current_buffer().contents(),
        "preamble\nnew body\nnew helper\ntail\n"
    );
    assert!(editor.current_buffer().is_dirty());
    assert_eq!(
        editor.inline_history.turn("request").unwrap().before,
        "body\nhelper\n"
    );
    assert_eq!(
        editor.inline_history.turn("request").unwrap().state,
        InlineTurnState::Completed
    );
    assert_eq!(editor.inline_comment_group_count("discussion"), 1);
    action(&mut editor, Action::UndoInlineAssist).await;
    assert_eq!(editor.current_buffer().contents(), SOURCE);
    assert_eq!(editor.inline_comment_group_count("discussion"), 0);
}

#[tokio::test]
async fn inline_expansion_rejects_exact_selection_wrong_source_and_stale_delivery() {
    for case in ["selection", "source", "revision", "range", "stale"] {
        let mut editor = editor(SOURCE);
        begin(&mut editor, case != "selection");
        let mut result = proposal(&editor);
        let scope = result.expanded_scope.as_mut().unwrap();
        match case {
            "source" => scope.before = "wrong\nhelper\n".into(),
            "revision" => scope.expected_revision += 1,
            "range" => {
                scope.start_line = 3;
                scope.end_line = 4;
                scope.before = "helper\ntail\n".into();
            }
            "stale" => {
                editor.begin_transaction("user edit");
                editor.replace_range(TextRange::insertion(TextPosition::new(0, 0)), "new\n");
                editor.commit_transaction(editor.cursor_snapshot());
            }
            _ => {}
        }
        let expected = editor.current_buffer().contents();
        editor.stage_background_inline_result("request", "provider", result);
        let turn = editor.inline_history.turn("request").unwrap();
        assert_eq!(turn.state, InlineTurnState::Rejected, "{case}");
        assert!(turn.expanded_location.is_none());
        assert!(turn.answer_text().contains("Wider edit proposed"));
        action(
            &mut editor,
            Action::ApplyReviewedInlineAssist("request".into()),
        )
        .await;
        assert_eq!(editor.current_buffer().contents(), expected, "{case}");
    }
}

#[tokio::test]
async fn inline_expansion_cannot_be_approved_after_source_changes() {
    let mut editor = editor(SOURCE);
    begin(&mut editor, true);
    let result = proposal(&editor);
    editor.stage_background_inline_result("request", "provider", result);
    action(&mut editor, Action::ViewInlineAssistAnswer).await;
    let Some(KeyAction::Single(approval)) = enter(&mut editor) else {
        panic!("missing approval")
    };
    editor.begin_transaction("change helper");
    editor.replace_range(
        TextRange::new(TextPosition::new(2, 0), TextPosition::new(3, 0)),
        "user helper\n",
    );
    editor.commit_transaction(editor.cursor_snapshot());
    let expected = editor.current_buffer().contents();
    action(&mut editor, approval).await;
    assert_eq!(editor.current_buffer().contents(), expected);
    assert_eq!(
        editor.inline_history.turn("request").unwrap().state,
        InlineTurnState::Rejected
    );
}

#[tokio::test]
async fn inline_expansion_decline_and_recheck_keep_the_original_boundary() {
    let mut editor = editor(SOURCE);
    begin(&mut editor, true);
    editor.replace_inline_comment_group(
        "earlier",
        "provider",
        "earlier",
        0,
        &[InlineCommentInput {
            start_line: 2,
            end_line: None,
            message: "Earlier note".into(),
        }],
    );
    let result = proposal(&editor);
    editor.stage_background_inline_result("request", "provider", result);
    action(&mut editor, Action::RefineInlineAssist).await;
    assert_eq!(editor.inline_submission_target().unwrap(), target());
    action(&mut editor, Action::CancelInlineAssistRefine).await;
    action(&mut editor, Action::RejectPendingInlineAssist).await;
    assert_eq!(editor.current_buffer().contents(), SOURCE);
    assert_eq!(editor.inline_comment_group_count("earlier"), 1);
    assert_eq!(
        editor.inline_history.turn("request").unwrap().state,
        InlineTurnState::Rejected
    );
    assert!(editor.inline_assist.is_none());
}

#[tokio::test]
async fn inline_expansion_recovery_retains_review_and_relocates_exact_source() {
    let mut original = editor(SOURCE);
    begin(&mut original, true);
    let result = proposal(&original);
    original.park_inline_assist();
    original.stage_background_inline_result("request", "provider", result);
    let mut recovered = editor(&format!("inserted above\n{SOURCE}"));
    recovered.inline_history =
        serde_json::from_str(&serde_json::to_string(&original.inline_history).unwrap()).unwrap();
    recovered.inline_history.recover();
    action(&mut recovered, Action::OpenInlineJob("discussion".into())).await;
    assert!(matches!(
        recovered.inline_assist_result_state(),
        InlineAssistPopupState::WiderReady { stale: false, .. }
    ));
    assert_eq!(
        recovered.inline_submission_target().unwrap(),
        TextRange::new(TextPosition::new(2, 0), TextPosition::new(3, 0))
    );
    action(&mut recovered, Action::ApplyPendingInlineAssist).await;
    let Some(KeyAction::Single(approval)) = enter(&mut recovered) else {
        panic!("missing recovered approval")
    };
    action(&mut recovered, approval).await;
    assert_eq!(
        recovered.current_buffer().contents(),
        "inserted above\npreamble\nnew body\nnew helper\ntail\n"
    );
}
