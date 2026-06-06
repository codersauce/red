mod common;

use std::sync::{Arc, Mutex};

use common::{LspEvent, RecordingLsp};
use red::{
    buffer::Buffer,
    config::Config,
    editor::{Action, Editor},
    lsp::LspClient,
    test_utils::EditorTestExt,
    theme::Theme,
};

fn recording_editor(buffers: Vec<Buffer>) -> (Editor, Arc<Mutex<Vec<LspEvent>>>) {
    let lsp = RecordingLsp::default();
    let events = lsp.events();
    let lsp = Box::new(lsp) as Box<dyn LspClient + Send>;
    let config = Config::default();
    let theme = Theme::default();
    let mut editor = Editor::with_size(lsp, 80, 24, config, theme, buffers).unwrap();
    editor.test_disable_terminal_output();
    (editor, events)
}

fn recorded(events: &Arc<Mutex<Vec<LspEvent>>>) -> Vec<LspEvent> {
    events.lock().unwrap().clone()
}

#[tokio::test]
async fn constructing_editor_does_not_open_inactive_lsp_buffer() {
    let (_editor, events) = recording_editor(vec![
        Buffer::new(None, "notes".to_string()),
        Buffer::new(Some("src/main.rs".to_string()), "fn main() {}".to_string()),
    ]);

    assert_eq!(recorded(&events), Vec::<LspEvent>::new());
}

#[tokio::test]
async fn activating_current_lsp_buffer_opens_it_once() {
    let (mut editor, events) = recording_editor(vec![Buffer::new(
        Some("src/main.rs".to_string()),
        "fn main() {}".to_string(),
    )]);

    editor
        .test_ensure_current_buffer_lsp_opened()
        .await
        .unwrap();
    editor
        .test_ensure_current_buffer_lsp_opened()
        .await
        .unwrap();

    assert_eq!(
        recorded(&events),
        vec![LspEvent::DidOpen("src/main.rs".to_string())]
    );
}

#[tokio::test]
async fn switching_to_lsp_buffer_opens_it_without_reopening_on_later_switches() {
    let (mut editor, events) = recording_editor(vec![
        Buffer::new(None, "notes".to_string()),
        Buffer::new(Some("src/main.rs".to_string()), "fn main() {}".to_string()),
    ]);

    editor
        .test_execute_action(Action::NextBuffer)
        .await
        .unwrap();
    editor
        .test_execute_action(Action::PreviousBuffer)
        .await
        .unwrap();
    editor
        .test_execute_action(Action::NextBuffer)
        .await
        .unwrap();

    let events = recorded(&events);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, LspEvent::DidOpen(file) if file == "src/main.rs"))
            .count(),
        1
    );
}

#[tokio::test]
async fn hover_opens_active_lsp_buffer_before_request() {
    let (mut editor, events) = recording_editor(vec![Buffer::new(
        Some("src/main.rs".to_string()),
        "fn main() {}".to_string(),
    )]);

    editor.test_execute_action(Action::Hover).await.unwrap();
    editor.test_execute_action(Action::Hover).await.unwrap();

    assert_eq!(
        recorded(&events),
        vec![
            LspEvent::DidOpen("src/main.rs".to_string()),
            LspEvent::Hover("src/main.rs".to_string()),
            LspEvent::Hover("src/main.rs".to_string()),
        ]
    );
}

#[tokio::test]
async fn split_with_file_opens_new_active_lsp_buffer() {
    let (mut editor, events) = recording_editor(vec![Buffer::new(None, "notes".to_string())]);

    editor
        .test_execute_action(Action::SplitHorizontalWithFile("src/main.rs".to_string()))
        .await
        .unwrap();

    let events = recorded(&events);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, LspEvent::DidOpen(file) if file == "src/main.rs")),
        "expected split-created active buffer to open through LSP, got {events:?}"
    );
}
