mod common;

use std::time::Instant;

use common::EditorHarness;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use red::{buffer::Buffer, config::Config, editor::RenderBuffer, theme::Style};

async fn key(harness: &mut EditorHarness, code: KeyCode) {
    harness
        .execute_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
        .await
        .unwrap();
}

async fn text(harness: &mut EditorHarness, text: &str) {
    for character in text.chars() {
        key(harness, KeyCode::Char(character)).await;
    }
}

fn dialog_text(harness: &mut EditorHarness) -> String {
    let mut buffer = RenderBuffer::new(100, 24, &Style::default());
    harness.editor.render(&mut buffer).unwrap();
    buffer
        .cells
        .chunks(buffer.width)
        .take(buffer.height - 2)
        .flat_map(|row| row.iter().map(|cell| cell.c).chain(['\n']))
        .collect()
}

#[tokio::test]
async fn message_history_routes_real_keys_through_search_acknowledgement_and_clear() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let mut harness = EditorHarness::with_config_and_size(
        Buffer::new(None, "unchanged buffer".to_string()),
        config,
        100,
        24,
    );

    for command in [":bad-one", ":bad-two"] {
        text(&mut harness, command).await;
        key(&mut harness, KeyCode::Enter).await;
    }
    let status = harness.commandline_row();
    assert!(status.contains("unknown command \"bad-two\""), "{status}");
    assert!(status.contains("2 active"), "{status}");

    text(&mut harness, " m").await;
    let dialog = dialog_text(&mut harness);
    assert!(dialog.contains("2 active · 2 retained"), "{dialog}");
    assert!(dialog.contains("bad-one") && dialog.contains("bad-two"));

    text(&mut harness, "/bad-one").await;
    key(&mut harness, KeyCode::Enter).await;
    let dialog = dialog_text(&mut harness);
    assert!(dialog.contains("bad-one") && !dialog.contains("bad-two"));
    key(&mut harness, KeyCode::Enter).await;
    let counts = harness.editor.notifications().counts(Instant::now());
    assert_eq!((counts.active, counts.total), (1, 2));

    key(&mut harness, KeyCode::Char('f')).await;
    assert!(dialog_text(&mut harness).contains("No matching messages"));
    key(&mut harness, KeyCode::Char('D')).await;
    let counts = harness.editor.notifications().counts(Instant::now());
    assert_eq!((counts.active, counts.total), (1, 1));
    key(&mut harness, KeyCode::Esc).await;

    text(&mut harness, " m").await;
    let dialog = dialog_text(&mut harness);
    assert!(dialog.contains("bad-two") && !dialog.contains("bad-one"));
    key(&mut harness, KeyCode::Enter).await;
    key(&mut harness, KeyCode::Char('D')).await;
    assert!(dialog_text(&mut harness).contains("0 active · 0 retained"));
    key(&mut harness, KeyCode::Esc).await;
    harness.assert_buffer_contents("unchanged buffer");
}
