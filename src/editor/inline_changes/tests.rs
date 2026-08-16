use super::*;
use crate::{inline_assist::InlineAssistResult, lsp::LspManager};

const BEFORE: &str = "old_name\nunchanged one\nunchanged two\nold_name\ntail\n";
const AFTER: &str = "new_name\nunchanged one\nunchanged two\nnew_name\ntail\n";

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

async fn apply(editor: &mut Editor) {
    apply_replacement(editor, AFTER).await;
}

async fn apply_replacement(editor: &mut Editor, replacement: &str) {
    let range = TextRange::new(TextPosition::new(0, 0), TextPosition::new(5, 0));
    editor.inline_assist = Some(InlineAssistSession {
        allow_expansion: false,
        buffer_id: editor.current_buffer().id(),
        window_id: editor.window_manager.active_stable_window_id().unwrap(),
        expected_revision: editor.current_buffer().revision(),
        range,
        expected_text: BEFORE.into(),
        scope: "function".into(),
        request_id: Some("request".into()),
        session_id: Some("provider".into()),
        transaction_id: None,
        annotation_group_id: "group".into(),
        has_result: false,
        result_request_id: None,
    });
    editor
        .begin_inline_history_turn("request", "Rename both calls", range)
        .unwrap();
    let result =
        InlineAssistResult::from_tool("submit_replacement", json!({"replacement": replacement}))
            .unwrap();
    editor
        .apply_inline_result(
            "request",
            "provider",
            &result,
            &mut RenderBuffer::new(100, 30, &Style::default()),
            &mut Runtime::new(),
        )
        .await
        .unwrap();
}

fn summary(editor: &Editor) -> Option<&super::super::inline_comments::InlineComment> {
    editor.inline_comments.iter().find(|comment| {
        matches!(&comment.origin,
        InlineCommentOrigin::ChangeSummary { request_id } if request_id == "request")
    })
}

fn key(editor: &mut Editor, code: KeyCode) -> Option<KeyAction> {
    editor
        .current_dialog
        .as_mut()
        .unwrap()
        .handle_event(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

#[tokio::test]
async fn inline_change_summary_survives_close_and_navigates_only_its_diff() {
    let mut editor = editor(&format!("{BEFORE}outside\n"));
    editor.begin_transaction("existing user edit");
    editor.replace_range(TextRange::insertion(TextPosition::new(5, 0)), "user ");
    editor.commit_transaction(editor.cursor_snapshot());
    apply(&mut editor).await;
    assert!(summary(&editor).unwrap().message.contains("2 location(s)"));
    let transaction = editor
        .current_buffer()
        .undo_history
        .latest_transaction()
        .unwrap()
        .id
        .clone();
    action(&mut editor, Action::KeepInlineAssist).await;
    assert!(editor.inline_assist.is_none());
    assert!(summary(&editor).unwrap().message.contains("buffer unsaved"));
    let id = summary(&editor).unwrap().id;
    action(&mut editor, Action::OpenInlineComment(id)).await;
    assert_eq!(key(&mut editor, KeyCode::Enter), None);
    let Some(KeyAction::Single(next)) = key(&mut editor, KeyCode::Char(']')) else {
        panic!("missing next change")
    };
    assert_eq!(
        editor
            .current_dialog
            .as_mut()
            .unwrap()
            .activate_surface_action("shortcut-]"),
        Some(KeyAction::Single(next.clone()))
    );
    action(&mut editor, next).await;
    assert_eq!(editor.buffer_line(), 3);
    assert_eq!(
        editor
            .current_buffer()
            .undo_history
            .latest_transaction()
            .unwrap()
            .id,
        transaction
    );
    let turn = editor.inline_history.turn("request").unwrap();
    assert!(turn.change_diff().contains("-old_name"));
    assert!(turn.change_diff().contains("+new_name"));
    assert!(!turn.change_diff().contains("user outside"));
    assert_eq!(turn.status(), "applied");
    editor.current_buffer_mut().mark_saved();
    editor.sync_inline_change_summaries();
    assert!(summary(&editor).unwrap().message.contains("buffer saved"));
}

#[tokio::test]
async fn inline_change_deletion_keeps_a_recoverable_summary() {
    let mut editor = editor(&format!("{BEFORE}outside\n"));
    apply_replacement(&mut editor, "").await;
    action(&mut editor, Action::KeepInlineAssist).await;
    assert_eq!(editor.current_buffer().contents(), "outside\n");
    assert!(summary(&editor).is_some());
    let serialized = serde_json::to_string(&editor.inline_history).unwrap();
    let mut recovered = self::editor("outside\n");
    recovered.inline_history = serde_json::from_str(&serialized).unwrap();
    recovered.restore_inline_history_comments();
    assert!(summary(&recovered).is_some());
    let id = summary(&recovered).unwrap().id;
    action(&mut recovered, Action::OpenInlineComment(id)).await;
    assert_eq!(recovered.buffer_line(), 0);
    assert!(recovered
        .inline_history
        .turn("request")
        .unwrap()
        .change_diff()
        .contains("-old_name"));
}

#[tokio::test]
async fn inline_change_history_open_and_source_changes_preserve_the_historical_diff() {
    let mut editor = editor(BEFORE);
    apply(&mut editor).await;
    action(&mut editor, Action::KeepInlineAssist).await;
    action(&mut editor, Action::OpenInlineHistory).await;
    action(
        &mut editor,
        Action::InlineHistoryAction(crate::inline_history::HistoryAction::Open),
    )
    .await;
    assert_eq!(key(&mut editor, KeyCode::Enter), None);
    assert!(key(&mut editor, KeyCode::Char(']')).is_some());
    let diff = editor.inline_history.turn("request").unwrap().change_diff();
    editor.begin_transaction("overlapping edit");
    editor.replace_range(
        TextRange::new(TextPosition::new(0, 0), TextPosition::new(1, 0)),
        "my_name\n",
    );
    editor.commit_transaction(editor.cursor_snapshot());
    action(
        &mut editor,
        Action::ViewInlineChanges {
            request_id: "request".into(),
            hunk: 1,
        },
    )
    .await;
    assert_eq!(key(&mut editor, KeyCode::Char(']')), None);
    assert_eq!(
        editor.inline_history.turn("request").unwrap().change_diff(),
        diff
    );
}

#[tokio::test]
async fn inline_change_summary_can_be_hidden_restored_and_recovered() {
    let mut editor = editor(BEFORE);
    apply(&mut editor).await;
    action(&mut editor, Action::KeepInlineAssist).await;
    editor.active_inline_comment = Some(summary(&editor).unwrap().id);
    editor.dismiss_inline_comment();
    editor.sync_inline_change_summaries();
    assert!(summary(&editor).is_none());
    assert!(editor
        .show_inline_history_annotations(
            "request",
            &mut RenderBuffer::new(100, 30, &Style::default()),
            &mut Runtime::new()
        )
        .await
        .unwrap());
    assert!(summary(&editor).is_some());
    let serialized = serde_json::to_string(&editor.inline_history).unwrap();
    let mut recovered = self::editor(AFTER);
    recovered.inline_history = serde_json::from_str(&serialized).unwrap();
    recovered.inline_history.validate().unwrap();
    recovered.restore_inline_history_comments();
    assert!(summary(&recovered).is_some());
    assert_eq!(
        recovered
            .inline_history
            .turn("request")
            .unwrap()
            .change_diff(),
        editor.inline_history.turn("request").unwrap().change_diff()
    );
    recovered
        .inline_history
        .turn_mut("request")
        .unwrap()
        .change_summary = None;
    recovered.restore_inline_history_comments();
    assert_eq!(
        recovered
            .inline_history
            .turn("request")
            .unwrap()
            .change_summary
            .as_ref()
            .unwrap()
            .hunks
            .len(),
        2
    );
}

#[tokio::test]
async fn inline_change_summary_does_not_replace_the_current_explanation() {
    let mut editor = editor(BEFORE);
    apply(&mut editor).await;
    let range = editor.inline_assist.as_ref().unwrap().range;
    let session = editor.inline_assist.as_mut().unwrap();
    session.request_id = Some("explanation".into());
    session.result_request_id = None;
    editor
        .begin_inline_history_turn("explanation", "Explain that change", range)
        .unwrap();
    let result = InlineAssistResult::from_tool(
        "submit_comments",
        json!({"comments":[{"start_line":1,"message":"Both calls use the new name."}]}),
    )
    .unwrap();
    editor
        .apply_inline_result(
            "explanation",
            "provider",
            &result,
            &mut RenderBuffer::new(100, 30, &Style::default()),
            &mut Runtime::new(),
        )
        .await
        .unwrap();
    assert!(summary(&editor).is_some());
    assert!(editor.inline_comments.iter().any(|comment| Some(comment.id) == editor.active_inline_comment
        && matches!(&comment.origin, InlineCommentOrigin::Assist { request_id, .. } if request_id == "explanation")));
}

#[tokio::test]
async fn inline_change_undo_is_request_bound_and_preserves_later_work() {
    let mut editor = editor(BEFORE);
    apply(&mut editor).await;
    action(&mut editor, Action::KeepInlineAssist).await;
    editor.begin_transaction("later user edit");
    editor.replace_range(TextRange::insertion(TextPosition::new(4, 0)), "user ");
    editor.commit_transaction(editor.cursor_snapshot());
    let later = editor.current_buffer().contents();
    action(&mut editor, Action::UndoInlineChange("request".into())).await;
    assert_eq!(editor.current_buffer().contents(), later);
    assert!(editor
        .last_error
        .as_deref()
        .unwrap()
        .contains("no longer the latest"));
    action(&mut editor, Action::Undo).await;
    action(&mut editor, Action::UndoInlineChange("request".into())).await;
    assert_eq!(editor.current_buffer().contents(), BEFORE);
    assert_eq!(
        editor.inline_history.turn("request").unwrap().status(),
        "undone"
    );
    assert!(summary(&editor).unwrap().message.contains("Undone"));
    assert_eq!(key(&mut editor, KeyCode::Char('u')), None);
}

#[test]
fn inline_change_hunks_handle_deletion_unicode_and_missing_final_newline() {
    use crate::inline_history::InlineChangeSummary;
    let deleted = InlineChangeSummary::new("gone\nkeep", "keep");
    assert_eq!(deleted.hunks.len(), 1);
    assert_eq!(
        (deleted.hunks[0].start_char, deleted.hunks[0].end_char),
        (0, 0)
    );
    let changed = InlineChangeSummary::new("α\nkeep\nold", "ββ\nkeep\nnew");
    assert_eq!(changed.hunks.len(), 2);
    assert_eq!(
        (changed.hunks[1].start_char, changed.hunks[1].end_char),
        (8, 11)
    );
}
