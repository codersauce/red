mod common;

use common::EditorHarness;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use red::{
    buffer::Buffer,
    config::Config,
    editor::{Action, Mode},
};

async fn key(harness: &mut EditorHarness, code: KeyCode, modifiers: KeyModifiers) {
    harness
        .execute_event(Event::Key(KeyEvent::new(code, modifiers)))
        .await
        .unwrap();
}

#[tokio::test]
async fn ctrl_n_change_types_at_each_selected_occurrence_as_one_undo() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "foo foo foo_bar foo".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('c'), KeyModifiers::NONE).await;
    harness.type_text("bar").await.unwrap();
    key(&mut harness, KeyCode::Esc, KeyModifiers::NONE).await;

    harness.assert_mode(Mode::Normal);
    harness.assert_buffer_contents("bar bar foo_bar foo");

    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("foo foo foo_bar foo");
}

#[tokio::test]
async fn ctrl_n_wraps_without_adding_duplicate_selections() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "foo foo".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    for _ in 0..3 {
        key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    }
    key(&mut harness, KeyCode::Char('c'), KeyModifiers::NONE).await;
    harness.type_text("x").await.unwrap();
    key(&mut harness, KeyCode::Esc, KeyModifiers::NONE).await;

    harness.assert_buffer_contents("x x");
}

#[tokio::test]
async fn multi_cursor_insert_supports_backspace_and_paste() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "foo foo".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('c'), KeyModifiers::NONE).await;
    harness.type_text("ab").await.unwrap();
    key(&mut harness, KeyCode::Backspace, KeyModifiers::NONE).await;
    harness
        .execute_event(Event::Paste("Z".to_string()))
        .await
        .unwrap();
    key(&mut harness, KeyCode::Esc, KeyModifiers::NONE).await;

    harness.assert_buffer_contents("aZ aZ");
}

#[tokio::test]
async fn multi_cursor_i_inserts_at_each_selection_start_as_one_undo() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "café café".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('i'), KeyModifiers::NONE).await;
    harness.type_text("☕").await.unwrap();
    key(&mut harness, KeyCode::Esc, KeyModifiers::NONE).await;

    harness.assert_mode(Mode::Normal);
    harness.assert_buffer_contents("☕café ☕café");

    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("café café");
}

#[tokio::test]
async fn multi_cursor_a_appends_to_each_selection_end_as_one_undo() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "foo foo".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('a'), KeyModifiers::NONE).await;
    harness
        .execute_event(Event::Paste("_id".to_string()))
        .await
        .unwrap();
    key(&mut harness, KeyCode::Esc, KeyModifiers::NONE).await;

    harness.assert_mode(Mode::Normal);
    harness.assert_buffer_contents("foo_id foo_id");

    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("foo foo");
}
