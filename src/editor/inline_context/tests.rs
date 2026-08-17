use super::*;
use crate::{agent_tools::EditorToolRequest, lsp::LspManager};

#[tokio::test]
async fn inline_context_dispatch_is_request_bound_and_does_not_move_focus() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let config = Config::default();
    let mut editor = Editor::with_size(
        Box::new(LspManager::new(config.lsp.clone())),
        100,
        30,
        config,
        Theme::default(),
        vec![
            Buffer::new(
                Some(root.join("main.c").to_string_lossy().into_owned()),
                "target\n".into(),
            ),
            Buffer::new(
                Some(root.join("helper.c").to_string_lossy().into_owned()),
                "unsaved helper\n".into(),
            ),
        ],
    )
    .unwrap();
    editor.test_disable_terminal_output();
    let range = TextRange::new(TextPosition::new(0, 0), TextPosition::new(1, 0));
    editor.inline_assist = Some(InlineAssistSession {
        parent_comment: None,
        allow_expansion: false,
        buffer_id: editor.current_buffer().id(),
        window_id: editor.window_manager.active_stable_window_id().unwrap(),
        expected_revision: editor.current_buffer().revision(),
        range,
        expected_text: "target\n".into(),
        scope: "test".into(),
        request_id: Some("request".into()),
        session_id: None,
        transaction_id: None,
        annotation_group_id: "group".into(),
        has_result: false,
        result_request_id: None,
    });
    editor
        .begin_inline_history_turn("request", "read helper", range)
        .unwrap();
    editor.inline_history.conversations[0].cwd = root.to_string_lossy().into_owned();
    let call = InlineContextCall::ReadFile {
        path: "helper.c".into(),
        start_line: 1,
        line_count: 200,
    };
    // The first read is allowed before InlineSessionCreated reaches the editor.
    assert!(editor
        .snapshot_inline_context("provider", "request", &call)
        .is_ok());
    editor.inline_assist.as_mut().unwrap().session_id = Some("provider".into());
    editor
        .inline_history
        .turn_mut("request")
        .unwrap()
        .session_id = Some("provider".into());
    editor.park_inline_assist();
    assert!(editor
        .snapshot_inline_context("wrong", "request", &call)
        .is_err());
    assert!(editor
        .snapshot_inline_context("provider", "old-request", &call)
        .is_err());
    let cursor = editor.cursor_snapshot();
    let buffer_id = editor.current_buffer().id();
    let (response, receive) = tokio::sync::oneshot::channel();
    editor.dispatch_inline_context_request(PendingEditorTool {
        request: EditorToolRequest {
            session_id: "provider".into(),
            call: EditorToolCall::InlineContext {
                request_id: "request".into(),
                call: call.clone(),
            },
        },
        response,
    });
    let result = receive.await.unwrap().unwrap();
    assert_eq!(result["content"], "unsaved helper\n");
    assert_eq!(editor.current_buffer().id(), buffer_id);
    assert_eq!(editor.cursor_snapshot(), cursor);
    assert!(!editor.agent_manager.has_playback_work());
    assert!(!root.join("helper.c").exists());
    let (bridge, worker) = CodexBridge::channel(std::num::NonZeroUsize::new(2).unwrap());
    editor.agent_manager.set_bridge(bridge);
    worker
        .send(CodexEvent::InlineContextRead {
            request_id: "request".into(),
            description: "Read helper.c:1–1 · editor revision 0".into(),
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
    let recovered: crate::inline_history::InlineHistory =
        serde_json::from_str(&serde_json::to_string(&editor.inline_history).unwrap()).unwrap();
    assert_eq!(
        recovered.turn("request").unwrap().context_reads,
        ["Read helper.c:1–1 · editor revision 0"]
    );
    editor
        .inline_history
        .finish("request", InlineTurnState::Cancelled, None);
    assert!(editor
        .snapshot_inline_context("provider", "request", &call)
        .is_err());
}
