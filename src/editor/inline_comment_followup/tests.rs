use super::*;
use crate::{
    inline_assist::InlineCommentInput,
    inline_history::{InlineConversation, InlineHistoryTurn},
    lsp::LspManager,
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

fn range(start: usize, end: usize) -> TextRange {
    TextRange::new(TextPosition::new(start, 0), TextPosition::new(end, 0))
}

fn fixture() -> (Editor, uuid::Uuid) {
    let config = Config::default();
    let file = get_workspace_path()
        .join("followup-test.c")
        .to_string_lossy()
        .into_owned();
    let mut editor = Editor::with_size(
        Box::new(LspManager::new(config.lsp.clone())),
        100,
        30,
        config,
        Theme::default(),
        vec![Buffer::new(
            Some(file.clone()),
            "alpha\nbeta\ngamma\n".into(),
        )],
    )
    .unwrap();
    editor.test_disable_terminal_output();
    editor.agent_manager.set_root(Some(get_workspace_path()));
    let location = editor.history_location(range(1, 2));
    let source_id = editor.inline_history.retain_source("beta\n".into());
    let mut turn = InlineHistoryTurn::new(
        "parent".into(),
        "Explain the second line".into(),
        "beta\n".into(),
        location.clone(),
    );
    turn.state = InlineTurnState::Completed;
    turn.result = Some(InlineAssistResult {
        expanded_scope: None,
        needs_agent: None,
        replacement: None,
        comments: vec![InlineCommentInput {
            start_line: 1,
            end_line: None,
            message: "Selected exact note".into(),
        }],
    });
    turn.comment_locations.push(location.clone());
    turn.comment_source_ids.push(source_id);
    turn.comment_fingerprints.push(None);
    let mut newer = InlineHistoryTurn::new(
        "newer".into(),
        "Unrelated newer request".into(),
        "beta\n".into(),
        location,
    );
    newer.state = InlineTurnState::Completed;
    let mut earlier = InlineHistoryTurn::new(
        "earlier".into(),
        "Original discussion".into(),
        "beta\n".into(),
        editor.history_location(range(1, 2)),
    );
    earlier.state = InlineTurnState::Completed;
    editor
        .inline_history
        .conversations
        .push(InlineConversation {
            id: "parent-group".into(),
            cwd: get_workspace_path().to_string_lossy().into_owned(),
            file,
            turns: vec![earlier, turn, newer],
            resolved: false,
            visible_request: Some("parent".into()),
        });
    let comment = editor.make_inline_comment(
        1,
        1,
        "Selected exact note".into(),
        InlineCommentOrigin::Assist {
            group_id: "parent-group".into(),
            session_id: "provider".into(),
            request_id: "parent".into(),
            comment_index: 0,
        },
    );
    let id = comment.id;
    editor.inline_comments.push(comment);
    editor.active_inline_comment = Some(id);
    editor.move_to_text_position(TextPosition::new(1, 0));
    (editor, id)
}

fn frame() -> RenderBuffer {
    RenderBuffer::new(100, 30, &Style::default())
}
fn key(character: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))
}
fn draft(editor: &Editor) -> String {
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
async fn selected_comment_forks_exact_context_and_preserves_parent_annotations() {
    let (mut editor, id) = fixture();
    editor.show_inline_comment();
    assert_eq!(
        editor
            .current_dialog
            .as_mut()
            .unwrap()
            .handle_event(&key('i')),
        Some(KeyAction::Single(Action::AskInlineComment {
            id,
            in_agent: false
        }))
    );
    assert_eq!(
        editor
            .current_dialog
            .as_mut()
            .unwrap()
            .handle_event(&key('A')),
        Some(KeyAction::Single(Action::AskInlineComment {
            id,
            in_agent: true
        }))
    );
    editor
        .ask_inline_comment(id, false, &mut frame(), &mut Runtime::new())
        .await
        .unwrap();
    let session = editor.inline_assist.as_ref().unwrap();
    assert_ne!(session.annotation_group_id, "parent-group");
    assert_eq!(session.range, range(1, 2));
    assert!(!session.allow_expansion);
    let context = session.parent_comment.as_ref().unwrap();
    assert_eq!(context.request_id, "parent");
    assert_eq!(context.message, "Selected exact note");
    assert_eq!(context.source, "beta\n");
    assert!(!context.discussion.contains("Unrelated newer"));
    assert!(context.discussion.contains("Original discussion"));
    assert!(editor
        .inline_assist_context(range(1, 2))
        .unwrap()
        .contains("Selected exact note"));
    assert!(
        matches!(editor.current_dialog.as_ref().unwrap().inline_assist_state(), Some(InlineAssistPopupState::Prompt { initial, .. }) if initial.is_empty())
    );
    assert_eq!(editor.inline_history.conversations.len(), 1);
    editor
        .begin_inline_history_turn("child", "Why?", range(1, 2))
        .unwrap();
    editor.inline_assist.as_mut().unwrap().request_id = Some("child".into());
    let result = InlineAssistResult {
        expanded_scope: None,
        needs_agent: None,
        replacement: None,
        comments: vec![InlineCommentInput {
            start_line: 1,
            end_line: None,
            message: "New answer".into(),
        }],
    };
    editor
        .apply_inline_result(
            "child",
            "new-provider",
            &result,
            &mut frame(),
            &mut Runtime::new(),
        )
        .await
        .unwrap();
    assert!(editor
        .inline_comments
        .iter()
        .any(|comment| comment.id == id));
    assert!(editor
        .inline_comments
        .iter()
        .any(|comment| comment.message == "New answer"));
    let group = &editor.inline_assist.as_ref().unwrap().annotation_group_id;
    let handoff = editor.inline_handoff_prompt(group).unwrap();
    assert!(handoff.contains("Latest user request:\nWhy?"));
    assert!(handoff.contains("Selected comment:\n> Selected exact note"));
    assert!(handoff.contains("```c\nbeta\n```"));
    assert!(!handoff.contains("\"start_char\""));
    assert!(!handoff.contains("<recovered_inline_history>"));
    assert!(
        handoff.contains(&super::super::inline_agent_outcomes::handoff_marker(
            "child"
        ))
    );
    editor
        .inline_history
        .conversations
        .retain(|conversation| conversation.id != "parent-group");
    let recovered: crate::inline_history::InlineHistory =
        serde_json::from_slice(&serde_json::to_vec(&editor.inline_history).unwrap()).unwrap();
    recovered.validate().unwrap();
    assert_eq!(
        recovered
            .turn("child")
            .unwrap()
            .parent_comment
            .as_ref()
            .unwrap()
            .message,
        "Selected exact note"
    );
}

#[tokio::test]
async fn empty_comment_question_is_not_a_draft_and_existing_draft_is_parked() {
    let (mut editor, id) = fixture();
    let mut runtime = Runtime::new();
    editor
        .execute(&Action::InlineAssist, &mut frame(), &mut runtime)
        .await
        .unwrap();
    let old_group = editor
        .inline_assist
        .as_ref()
        .unwrap()
        .annotation_group_id
        .clone();
    editor.current_dialog = Some(Box::new(editor.inline_assist_popup(
        "old draft",
        InlineAssistPopupState::Prompt {
            initial: "Do not lose me".into(),
            refining: false,
        },
    )));
    editor
        .ask_inline_comment(id, false, &mut frame(), &mut runtime)
        .await
        .unwrap();
    assert!(
        matches!(&editor.inline_jobs[&old_group].state, InlineAssistPopupState::Prompt { initial, .. } if initial == "Do not lose me")
    );
    assert_eq!(
        editor
            .current_dialog
            .as_mut()
            .unwrap()
            .request_inline_assist_close(),
        Some(Action::DiscardInlineAssistDraft)
    );
    editor
        .execute(
            &Action::DiscardInlineAssistDraft,
            &mut frame(),
            &mut runtime,
        )
        .await
        .unwrap();
    assert_eq!(editor.inline_history.conversations.len(), 1);
    assert!(editor.inline_jobs.contains_key(&old_group));
}

#[tokio::test]
async fn changed_source_is_disclosed_and_detached_source_offers_agent() {
    let (mut editor, id) = fixture();
    editor.begin_transaction("change comment source");
    editor.replace_range(range(1, 2), "modified\n");
    editor.commit_transaction(editor.cursor_snapshot());
    let (context, target) = editor.selected_comment_context(id).unwrap();
    assert!(context.outdated);
    assert_eq!(target, Some(range(1, 2)));
    assert_eq!(context.source, "beta\n");
    editor.begin_transaction("delete comment source");
    editor.replace_range(range(1, 2), "");
    editor.commit_transaction(editor.cursor_snapshot());
    assert!(editor.selected_comment_context(id).unwrap().1.is_none());
    editor
        .ask_inline_comment(id, false, &mut frame(), &mut Runtime::new())
        .await
        .unwrap();
    assert!(editor.inline_assist.is_none());
    assert_eq!(
        editor
            .current_dialog
            .as_mut()
            .unwrap()
            .handle_event(&key('A')),
        Some(KeyAction::Single(Action::AskInlineComment {
            id,
            in_agent: true
        }))
    );
}

#[tokio::test]
async fn ask_agent_opens_the_composer_without_sending_or_creating_history() {
    let (mut editor, id) = fixture();
    let mut runtime = Runtime::new();
    runtime
        .load_plugin("agent", include_str!("../../../plugins/agent.hk"))
        .await
        .unwrap();
    editor
        .ask_inline_comment(id, true, &mut frame(), &mut runtime)
        .await
        .unwrap();
    editor
        .service_background(&mut frame(), &mut runtime)
        .await
        .unwrap();
    assert!(editor.panel_manager.is_visible("agent-conversation"));
    assert_eq!(
        editor.panel_manager.focused_panel_id(),
        Some("agent-conversation")
    );
    let prompt = draft(&editor);
    assert!(prompt.contains("Selected comment:\n> Selected exact note"));
    assert!(prompt.ends_with("My question: "));
    assert!(!prompt.contains("Unrelated newer request"));
    let staged = editor.staged_inline_agent_handoff.as_ref().unwrap();
    assert!(editor.inline_history.turn(&staged.request_id).is_none());
    assert_eq!(editor.inline_history.conversations.len(), 1);
    assert!(!editor.agent_manager.has_bridge());
}

#[tokio::test]
async fn agent_comment_followup_preserves_draft_and_materializes_only_on_send() {
    let (mut editor, id) = fixture();
    let context = editor.selected_comment_context(id).unwrap().0;
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
        .load_text_panel_draft("agent-conversation", "existing draft", None)
        .unwrap();
    let prompt = format!(
        "{}{}\nMy question",
        super::super::inline_agent_outcomes::handoff_marker("agent-child"),
        context.prompt_context()
    );
    let action = Action::StageInlineAssistHandoff {
        request_id: Some("agent-child".into()),
        comment_followup: Some(context),
        prompt: prompt.clone(),
        expected_draft: None,
    };
    let mut runtime = Runtime::new();
    editor
        .execute(&action, &mut frame(), &mut runtime)
        .await
        .unwrap();
    assert_eq!(draft(&editor), "existing draft");
    assert!(editor.staged_inline_agent_handoff.is_none());
    let Some(KeyAction::Multiple(actions)) = editor
        .current_dialog
        .as_mut()
        .unwrap()
        .handle_event(&key('y'))
    else {
        panic!("expected confirmation");
    };
    for action in actions {
        editor
            .execute(&action, &mut frame(), &mut runtime)
            .await
            .unwrap();
    }
    assert_eq!(draft(&editor), prompt);
    assert!(editor.inline_history.turn("agent-child").is_none());
    assert!(
        runtime.try_recv_request().is_none(),
        "draft staging must not send"
    );
    editor
        .begin_inline_agent_outcome("agent", "agent-turn", &prompt)
        .unwrap();
    let child = editor.inline_history.turn("agent-child").unwrap();
    assert_eq!(child.parent_comment.as_ref().unwrap().request_id, "parent");
    assert_eq!(child.agent_outcomes.len(), 1);
    assert!(editor
        .inline_history
        .turn("parent")
        .unwrap()
        .agent_outcomes
        .is_empty());
    assert!(editor
        .inline_comments
        .iter()
        .any(|comment| comment.id == id));
    editor.inline_history.validate().unwrap();
}

#[test]
fn comment_context_is_bounded_on_utf8_boundaries() {
    let (mut editor, id) = fixture();
    editor.inline_history.turn_mut("parent").unwrap().answer = "é".repeat(20_000);
    let source = "é".repeat(20_000);
    let source_id = editor.inline_history.retain_source(source);
    editor
        .inline_history
        .turn_mut("parent")
        .unwrap()
        .comment_source_ids[0] = source_id;
    let (context, _) = editor.selected_comment_context(id).unwrap();
    assert!(context.source_truncated);
    assert_eq!(context.source.len(), MAX_SOURCE_BYTES);
    assert!(context.discussion.len() <= MAX_DISCUSSION_BYTES);
    context.validate().unwrap();
}
