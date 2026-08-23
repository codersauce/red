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
    assert!(harness.statusline_row().contains("MULTI-I 2/2"));
    harness.type_text("☕").await.unwrap();
    key(&mut harness, KeyCode::Esc, KeyModifiers::NONE).await;

    harness.assert_mode(Mode::Normal);
    assert!(harness.statusline_row().contains("MULTI 2/2"));
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

#[tokio::test]
async fn multi_cursor_navigation_wraps_and_reports_the_active_selection() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "foo foo foo foo".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    assert!(harness.statusline_row().contains("MULTI 2/2"));
    harness.assert_cursor_at(6, 0);

    key(&mut harness, KeyCode::Char('N'), KeyModifiers::SHIFT).await;
    assert!(harness.statusline_row().contains("MULTI 1/2"));
    harness.assert_cursor_at(2, 0);

    key(&mut harness, KeyCode::Char('N'), KeyModifiers::SHIFT).await;
    assert!(harness.statusline_row().contains("MULTI 3/3"));
    harness.assert_cursor_at(14, 0);

    key(&mut harness, KeyCode::Char('n'), KeyModifiers::NONE).await;
    assert!(harness.statusline_row().contains("MULTI 1/3"));
    harness.assert_cursor_at(2, 0);
}

#[tokio::test]
async fn q_skips_in_the_last_direction_and_shift_q_removes_the_active_selection() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "foo foo foo foo".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('N'), KeyModifiers::SHIFT).await;
    key(&mut harness, KeyCode::Char('q'), KeyModifiers::NONE).await;
    assert!(harness.statusline_row().contains("MULTI 2/2"));
    harness.assert_cursor_at(14, 0);

    key(&mut harness, KeyCode::Char('Q'), KeyModifiers::SHIFT).await;
    assert!(harness.statusline_row().contains("MULTI 1/1"));
    harness.assert_cursor_at(6, 0);

    key(&mut harness, KeyCode::Char('Q'), KeyModifiers::SHIFT).await;
    assert!(!harness.statusline_row().contains("MULTI"));
}

#[tokio::test]
async fn skipped_occurrences_are_excluded_from_the_following_edit() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "foo foo foo foo".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('q'), KeyModifiers::NONE).await;
    key(&mut harness, KeyCode::Char('c'), KeyModifiers::NONE).await;
    harness.type_text("x").await.unwrap();
    key(&mut harness, KeyCode::Esc, KeyModifiers::NONE).await;

    harness.assert_buffer_contents("x foo x foo");
}

#[tokio::test]
async fn multi_cursor_navigation_does_not_replace_normal_search_state() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "foo bar foo bar".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    harness.execute_action(Action::MoveTo(4, 0)).await.unwrap();
    harness
        .execute_action(Action::SearchWordUnderCursor)
        .await
        .unwrap();
    harness.execute_action(Action::MoveTo(0, 0)).await.unwrap();

    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::NONE).await;
    key(&mut harness, KeyCode::Esc, KeyModifiers::NONE).await;
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::NONE).await;

    harness.assert_cursor_at(12, 0);
}
