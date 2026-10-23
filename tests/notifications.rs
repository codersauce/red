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
async fn actionable_inbox_routes_real_keys_through_search_and_dismissal() {
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
    assert!(!status.contains(":messages"), "{status}");

    assert_eq!(harness.editor.notifications().records().count(), 0);
    for key in ["issue-one", "issue-two"] {
        harness
            .editor
            .publish_notification(
                red::notification::Notice::new(
                    red::notification::NotificationSource::Editor,
                    red::notification::Severity::Error,
                    key,
                )
                .with_key(key)
                .with_details("Retry the operation or discard the pending work"),
            )
            .unwrap();
    }

    text(&mut harness, " m").await;
    let dialog = dialog_text(&mut harness);
    assert!(dialog.contains("2 active · 2 retained"), "{dialog}");
    assert!(dialog.contains("issue-one") && dialog.contains("issue-two"));

    text(&mut harness, "/issue-one").await;
    key(&mut harness, KeyCode::Enter).await;
    let dialog = dialog_text(&mut harness);
    assert!(dialog.contains("issue-one") && !dialog.contains("issue-two"));
    key(&mut harness, KeyCode::Enter).await;
    let counts = harness.editor.notifications().inbox_counts(Instant::now());
    assert_eq!((counts.active, counts.total), (1, 1));

    // The dismissed issue is absent even though the search remains active.
    assert!(dialog_text(&mut harness).contains("No matching messages"));
    key(&mut harness, KeyCode::Char('D')).await;
    let counts = harness.editor.notifications().inbox_counts(Instant::now());
    assert_eq!((counts.active, counts.total), (1, 1));
    key(&mut harness, KeyCode::Esc).await;

    text(&mut harness, " m").await;
    let dialog = dialog_text(&mut harness);
    assert!(dialog.contains("issue-two") && !dialog.contains("issue-one"));
    key(&mut harness, KeyCode::Enter).await;
    key(&mut harness, KeyCode::Char('D')).await;
    assert!(dialog_text(&mut harness).contains("0 active · 0 retained"));
    key(&mut harness, KeyCode::Esc).await;
    harness.assert_buffer_contents("unchanged buffer");
}
