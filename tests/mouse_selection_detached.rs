mod common;

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use red::{
    buffer::Buffer,
    clipboard::MemoryClipboardProvider,
    config::Config,
    editor::DetachedEditorCore,
    headless::{InputEvent, KeyCode, KeyKind, RenderDelta},
    theme::Style,
};

fn style_at(frame: &RenderDelta, row: usize, column: usize) -> Style {
    let line = frame.lines.iter().find(|line| line.row == row).unwrap();
    let mut start = 0;
    for span in &line.spans {
        let end = start + span.text.chars().count();
        if column < end {
            return span.style.clone();
        }
        start = end;
    }
    panic!("missing cell {column},{row}");
}

#[tokio::test]
async fn forwarded_mouse_selection_survives_release_and_yanks_the_highlighted_text() {
    let mut config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    config.plugins.clear();
    config.splash = Some(false);
    config.scrolloff = Some(0);
    let clipboard = MemoryClipboardProvider::default();
    let clipboard_text = clipboard.shared_text();
    let mut harness = common::EditorHarness::with_config(
        Buffer::new(None, "alpha beta gamma\nsecond line".to_string()),
        config,
    );
    harness.editor.test_set_clipboard(Box::new(clipboard));
    let mut core = DetachedEditorCore::new(harness.editor).await.unwrap();
    let before = core.snapshot(None);
    let original_background = style_at(&before, 0, 11).bg;

    for (kind, column) in [
        (MouseEventKind::Down(MouseButton::Left), 10),
        (MouseEventKind::Drag(MouseButton::Left), 12),
        (MouseEventKind::Up(MouseButton::Left), 13),
    ] {
        // Exercise the same serialized DTO an attached terminal forwards.
        let encoded = serde_json::to_vec(&InputEvent::Mouse {
            event: MouseEvent {
                kind,
                column,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
        })
        .unwrap();
        let event = serde_json::from_slice(&encoded).unwrap();
        let delta = core.input(event).await.unwrap();
        if matches!(kind, MouseEventKind::Drag(_)) {
            assert!(delta.lines.iter().any(|line| line.row == 0));
            assert_ne!(
                style_at(&core.snapshot(None), 0, 11).bg,
                original_background
            );
        }
    }

    let released = core.snapshot(None);
    assert!(released
        .lines
        .iter()
        .any(|line| line.text.contains("VISUAL")));
    for column in 10..=13 {
        assert_ne!(style_at(&released, 0, column).bg, original_background);
    }
    assert_eq!(style_at(&released, 0, 14).bg, original_background);
    // A reattaching client receives the retained selection in a full snapshot.
    assert_eq!(core.snapshot(None), released);
    core.input(InputEvent::Key {
        code: KeyCode::Character('y'),
        modifiers: Vec::new(),
        key_kind: KeyKind::Press,
    })
    .await
    .unwrap();
    assert_eq!(clipboard_text.lock().unwrap().as_deref(), Some("beta"));
    assert_eq!(
        style_at(&core.snapshot(None), 0, 11).bg,
        original_background
    );
}

#[tokio::test]
async fn disconnect_cancels_captured_autoscroll_without_discarding_the_selection() {
    let config = Config {
        scrolloff: Some(0),
        splash: Some(false),
        ..Config::default()
    };
    let content = (0..40).map(|row| format!("line {row}\n")).collect();
    let harness =
        common::EditorHarness::with_config_and_size(Buffer::new(None, content), config, 40, 10);
    let mut core = DetachedEditorCore::new(harness.editor).await.unwrap();
    for (kind, row) in [
        (MouseEventKind::Down(MouseButton::Left), 1),
        (MouseEventKind::Drag(MouseButton::Left), 9),
    ] {
        core.input(InputEvent::Mouse {
            event: MouseEvent {
                kind,
                column: 6,
                row,
                modifiers: KeyModifiers::NONE,
            },
        })
        .await
        .unwrap();
    }
    let before_disconnect = core.snapshot(None);
    assert!(before_disconnect
        .lines
        .iter()
        .any(|line| line.text.contains("VISUAL")));
    core.client_disconnected();
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    core.tick().await.unwrap();
    let reattached = core.snapshot(None);
    assert_eq!(reattached.cursor, before_disconnect.cursor);
    assert_eq!(reattached.lines[0], before_disconnect.lines[0]);
    assert!(reattached
        .lines
        .iter()
        .any(|line| line.text.contains("VISUAL")));
    // A late release from a replacement client cannot resume the old gesture.
    core.input(InputEvent::Mouse {
        event: MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 12,
            row: 2,
            modifiers: KeyModifiers::NONE,
        },
    })
    .await
    .unwrap();
    assert_eq!(core.snapshot(None).cursor, before_disconnect.cursor);
}
