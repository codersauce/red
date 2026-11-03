//! Keystroke coverage for the experimental Helix model.
//!
//! These sequences are original. They drive `EditorHarness::execute_event`
//! through the normal key path.

mod common;

use common::EditorHarness;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use red::{buffer::Buffer, config::Config, editor::Action};

fn vim_config() -> Config {
    toml::from_str(include_str!("../default_config.toml")).unwrap()
}

async fn press(harness: &mut EditorHarness, keys: &str) {
    for key in keys.chars() {
        harness
            .execute_event(Event::Key(KeyEvent::new(
                KeyCode::Char(key),
                KeyModifiers::NONE,
            )))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn vim_model_keeps_a_one_scalar_selection_on_the_cursor() {
    let mut harness = EditorHarness::with_config(
        Buffer::new(None, "hello\n".to_string()),
        vim_config(),
    );
    harness.execute_action(Action::MoveRight).await.unwrap();
    harness.assert_cursor_at(1, 0);
    assert_eq!(harness.editor.test_felix_selection(), (1, 1, 1));
    press(&mut harness, "x").await;
    harness.assert_buffer_contents("hllo\n");
    assert_eq!(harness.editor.test_felix_selection(), (1, 1, 1));
}
