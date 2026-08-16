use super::*;
use crate::{inline_assist::InlineCommentInput, lsp::LspManager};

fn editor() -> Editor {
    let config: Config = toml::from_str(include_str!("../../../default_config.toml")).unwrap();
    let mut editor = Editor::with_size(
        Box::new(LspManager::new(config.lsp.clone())),
        100,
        30,
        config,
        Theme::default(),
        vec![Buffer::new(
            Some("/workspace/src/sample.c".into()),
            "alpha\nbeta\n".into(),
        )],
    )
    .unwrap();
    editor.test_disable_terminal_output();
    editor
}

fn start(editor: &mut Editor, group: &str, request: &str, line: usize) {
    editor.park_inline_assist();
    let range = TextRange::new(TextPosition::new(line, 0), TextPosition::new(line + 1, 0));
    editor.inline_assist = Some(InlineAssistSession {
        buffer_id: editor.current_buffer().id(),
        window_id: editor.window_manager.active_stable_window_id().unwrap(),
        expected_revision: editor.current_buffer().revision(),
        range,
        expected_text: editor.current_buffer().text_in_range(range),
        scope: "test".into(),
        request_id: Some(request.into()),
        session_id: Some(format!("provider-{request}")),
        transaction_id: None,
        annotation_group_id: group.into(),
        has_result: false,
        result_request_id: None,
    });
    editor
        .begin_inline_history_turn(request, request, range)
        .unwrap();
    editor
        .inline_history
        .conversations
        .iter_mut()
        .find(|conversation| conversation.id == group)
        .unwrap()
        .cwd = "/workspace".into();
    editor.current_dialog = Some(Box::new(
        editor.inline_assist_popup("test", InlineAssistPopupState::Working),
    ));
}

fn finish(editor: &mut Editor, request: &str, replacement: Option<&str>) {
    editor.stage_background_inline_result(
        request,
        &format!("provider-{request}"),
        InlineAssistResult {
            needs_agent: None,
            replacement: replacement.map(str::to_owned),
            comments: vec![InlineCommentInput {
                start_line: 1,
                end_line: None,
                message: "Answer".into(),
            }],
        },
    );
}

fn row(editor: &mut Editor) -> String {
    let mut frame = RenderBuffer::new(
        editor.size.0 as usize,
        editor.size.1 as usize,
        &Style::default(),
    );
    editor.draw_commandline(&mut frame);
    let cells = &frame.cells[(frame.height - 1) * frame.width..];
    let mut text = String::new();
    let mut column = 0;
    while let Some(cell) = cells.get(column) {
        text.push_str(&cell.text);
        column += display_width(&cell.text).max(1);
    }
    text.trim_end().to_owned()
}

fn click(column: usize) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: column as u16,
        row: 29,
        modifiers: KeyModifiers::NONE,
    })
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

#[tokio::test]
async fn inline_completion_notice_links_to_background_result_without_applying_it() {
    let mut editor = editor();
    start(&mut editor, "first", "one", 0);
    start(&mut editor, "second", "two", 1);
    let cursor = editor.cursor_snapshot();
    finish(&mut editor, "one", Some("ALPHA\n"));
    assert_eq!(editor.cursor_snapshot(), cursor);
    assert_eq!(
        editor.inline_assist.as_ref().unwrap().request_id.as_deref(),
        Some("two")
    );
    assert_eq!(
        row(&mut editor),
        "Inline edit ready · [src/sample.c:1] · Space N"
    );
    let (columns, _) = editor.inline_completion.hit.clone().unwrap();
    assert!(editor
        .inline_completion_click(&click(columns.start - 1))
        .is_none());
    assert!(editor
        .inline_completion_click(&click(columns.end))
        .is_none());
    let expected = Action::OpenInlineCompletion("one".into());
    assert_eq!(
        editor.handle_event(&click(columns.start)).unwrap(),
        Some(KeyAction::Single(expected.clone()))
    );
    action(&mut editor, expected).await;
    assert_eq!(editor.current_buffer().contents(), "alpha\nbeta\n");
    assert_eq!(
        editor.inline_history.turn("one").unwrap().state,
        InlineTurnState::Ready
    );
    assert_eq!(
        editor.inline_assist.as_ref().unwrap().request_id.as_deref(),
        Some("one")
    );
    assert!(editor.inline_jobs.contains_key("second"));
    assert!(editor.inline_completion.notice.is_none());
}

#[tokio::test]
async fn inline_completion_shortcut_survives_notice_expiry() {
    let mut editor = editor();
    start(&mut editor, "first", "one", 0);
    editor.park_inline_assist();
    finish(&mut editor, "one", None);
    assert!(row(&mut editor).starts_with("Inline finished"));
    let columns = editor.inline_completion.hit.as_ref().unwrap().0.clone();
    assert!(editor.poll_inline_completion_notice(Instant::now() + NOTICE_DURATION));
    assert!(row(&mut editor).is_empty());
    assert!(editor
        .inline_completion_click(&click(columns.start))
        .is_none());
    assert!(!editor.poll_inline_completion_notice(Instant::now() + NOTICE_DURATION));
    let KeyAction::Nested(keys) = editor.config.keys.normal.get(" ").unwrap() else {
        panic!("leader keymap");
    };
    assert_eq!(
        keys.get("N"),
        Some(&KeyAction::Single(Action::OpenLatestInlineCompletion))
    );
    assert_eq!(keys.get("n"), Some(&KeyAction::Single(Action::NextBuffer)));
    assert_eq!(
        editor.handle_command("InlineLast", &Runtime::new()),
        vec![Action::OpenLatestInlineCompletion]
    );
    action(&mut editor, Action::OpenLatestInlineCompletion).await;
    assert_eq!(
        editor.inline_assist.as_ref().unwrap().request_id.as_deref(),
        Some("one")
    );
}

#[test]
fn inline_completion_notice_preserves_errors_commands_and_narrow_layouts() {
    let mut editor = editor();
    start(&mut editor, "first", "one", 0);
    editor.park_inline_assist();
    finish(&mut editor, "one", None);
    row(&mut editor);
    let columns = editor.inline_completion.hit.as_ref().unwrap().0.clone();
    editor.last_error = Some("important error".into());
    assert!(editor
        .inline_completion_click(&click(columns.start))
        .is_none());
    assert_eq!(row(&mut editor), "important error");
    assert!(editor.inline_completion.hit.is_none());
    editor.last_error = None;
    editor.mode = Mode::Command;
    editor.command = "write".into();
    assert_eq!(row(&mut editor), ":write");
    assert!(editor.inline_completion.hit.is_none());
    editor.mode = Mode::Normal;
    editor.inline_history.turn_mut("one").unwrap().location.file =
        "/workspace/very/long/界界界/sample.c".into();
    for width in 1..=100 {
        editor.size.0 = width;
        let text = row(&mut editor);
        assert!(display_width(&text) <= usize::from(width));
        assert!(editor
            .inline_completion
            .hit
            .as_ref()
            .is_none_or(|(columns, _)| columns.end <= usize::from(width)));
    }
    editor.size.0 = 24;
    assert!(row(&mut editor).ends_with("sample.c:1]"));
}

#[test]
fn inline_completion_notices_ignore_foreground_and_late_results() {
    let mut editor = editor();
    start(&mut editor, "first", "one", 0);
    editor.record_inline_failure("one", "failed");
    assert!(editor.inline_completion.latest.is_none());
    start(&mut editor, "second", "two", 1);
    editor.park_inline_assist();
    editor.record_inline_failure("two", "failed");
    assert!(row(&mut editor).starts_with("Inline failed"));
    let notice = editor.inline_completion.notice.clone();
    editor.record_inline_failure("two", "duplicate");
    finish(&mut editor, "two", None);
    assert_eq!(editor.inline_completion.notice, notice);
    start(&mut editor, "third", "three", 0);
    editor.park_inline_assist();
    finish(&mut editor, "three", None);
    assert_eq!(editor.inline_completion.latest.as_deref(), Some("three"));
    assert!(editor.inline_completion.hit.is_none());
}

#[test]
fn inline_completion_notice_does_not_expire_before_it_can_be_seen() {
    let mut editor = editor();
    start(&mut editor, "first", "one", 0);
    editor.park_inline_assist();
    editor.last_error = Some("important error".into());
    finish(&mut editor, "one", None);
    assert_eq!(row(&mut editor), "important error");
    assert!(!editor.poll_inline_completion_notice(Instant::now() + Duration::from_secs(60)));
    assert!(editor
        .inline_completion
        .notice
        .as_ref()
        .unwrap()
        .expires_at
        .is_none());
    editor.last_error = None;
    assert!(row(&mut editor).starts_with("Inline finished"));
    assert!(editor
        .inline_completion
        .notice
        .as_ref()
        .unwrap()
        .expires_at
        .is_some());
    assert!(editor.poll_inline_completion_notice(Instant::now() + NOTICE_DURATION));
}

#[tokio::test]
async fn inline_completion_reopens_an_older_request_in_history() {
    let mut editor = editor();
    start(&mut editor, "group", "one", 0);
    editor.park_inline_assist();
    finish(&mut editor, "one", None);
    start(&mut editor, "group", "two", 0);
    action(&mut editor, Action::OpenInlineCompletion("one".into())).await;
    assert!(editor.current_dialog.as_ref().unwrap().is_inline_history());
    assert!(editor
        .history_rows()
        .iter()
        .any(|row| row.key == super::super::inline_history::HistoryKey::Turn("one".into())));
    assert!(editor.inline_jobs.contains_key("group"));
    assert_eq!(
        editor.inline_history.turn("two").unwrap().state,
        InlineTurnState::Pending
    );
    assert_eq!(editor.current_buffer().contents(), "alpha\nbeta\n");
}

#[tokio::test]
async fn inline_completion_jump_commits_an_in_progress_insert_before_switching_buffers() {
    let mut editor = editor();
    start(&mut editor, "first", "one", 0);
    editor.park_inline_assist();
    editor.buffer_manager.add_buffer(Buffer::new(
        Some("/workspace/other.c".into()),
        "other\n".into(),
    ));
    editor.sync_to_window();
    action(&mut editor, Action::EnterMode(Mode::Insert)).await;
    action(&mut editor, Action::InsertCharAtCursorPos('x')).await;
    finish(&mut editor, "one", None);
    assert_eq!(editor.current_buffer().contents(), "xother\n");
    assert!(editor.transaction_active());
    action(&mut editor, Action::OpenLatestInlineCompletion).await;
    assert_eq!(editor.mode, Mode::Normal);
    assert_eq!(editor.current_buffer().contents(), "alpha\nbeta\n");
    action(&mut editor, Action::HideInlineAssist).await;
    action(&mut editor, Action::JumpBack).await;
    assert_eq!(editor.current_buffer().contents(), "xother\n");
    action(&mut editor, Action::Undo).await;
    assert_eq!(editor.current_buffer().contents(), "other\n");
}
