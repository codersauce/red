mod common;

use common::EditorHarness;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use red::{
    buffer::Buffer,
    clipboard::MemoryClipboardProvider,
    config::Config,
    editor::{Action, Content, Mode},
};

async fn key(harness: &mut EditorHarness, code: KeyCode, modifiers: KeyModifiers) {
    harness
        .execute_event(Event::Key(KeyEvent::new(code, modifiers)))
        .await
        .unwrap();
}

async fn assert_vertical_selection_change(
    contents: &str,
    cursor_x: usize,
    selection_keys: &[(KeyCode, KeyModifiers)],
    expected: &str,
) {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, contents.to_string());
    let mut harness = EditorHarness::with_config(buffer, config);
    harness
        .execute_action(Action::MoveTo(cursor_x, 0))
        .await
        .unwrap();
    key(&mut harness, KeyCode::Down, KeyModifiers::CONTROL).await;
    for (code, modifiers) in selection_keys {
        key(&mut harness, *code, *modifiers).await;
    }
    key(&mut harness, KeyCode::Char('c'), KeyModifiers::NONE).await;
    harness.type_text("X").await.unwrap();
    key(&mut harness, KeyCode::Esc, KeyModifiers::NONE).await;

    harness.assert_buffer_contents(expected);
}

#[tokio::test]
async fn shift_arrows_expand_vertical_cursors_like_visual_multi() {
    assert_vertical_selection_change(
        "abcdef\nabcdef",
        2,
        &[(KeyCode::Right, KeyModifiers::SHIFT)],
        "abXef\nabXef",
    )
    .await;
    assert_vertical_selection_change(
        "abcdef\nabcdef",
        2,
        &[(KeyCode::Left, KeyModifiers::SHIFT)],
        "aXdef\naXdef",
    )
    .await;
}

#[tokio::test]
async fn tab_enters_extend_mode_and_tab_again_collapses_at_each_head() {
    assert_vertical_selection_change(
        "abcdef\nabcdef",
        2,
        &[(KeyCode::Tab, KeyModifiers::NONE)],
        "abXdef\nabXdef",
    )
    .await;

    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "abcdef\nabcdef".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);
    harness.execute_action(Action::MoveTo(2, 0)).await.unwrap();
    key(&mut harness, KeyCode::Down, KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Tab, KeyModifiers::NONE).await;
    key(&mut harness, KeyCode::Char('e'), KeyModifiers::NONE).await;
    key(&mut harness, KeyCode::Tab, KeyModifiers::NONE).await;
    key(&mut harness, KeyCode::Char('i'), KeyModifiers::NONE).await;
    harness.type_text("X").await.unwrap();
    key(&mut harness, KeyCode::Esc, KeyModifiers::NONE).await;

    harness.assert_buffer_contents("abcdeXf\nabcdeXf");
}

#[tokio::test]
async fn extend_mode_supports_word_and_line_motions() {
    assert_vertical_selection_change(
        "abcdef\nabcdef",
        2,
        &[
            (KeyCode::Tab, KeyModifiers::NONE),
            (KeyCode::Char('e'), KeyModifiers::NONE),
        ],
        "abX\nabX",
    )
    .await;
    assert_vertical_selection_change(
        "foo bar\nfoo baz",
        0,
        &[
            (KeyCode::Tab, KeyModifiers::NONE),
            (KeyCode::Char('w'), KeyModifiers::NONE),
        ],
        "Xar\nXaz",
    )
    .await;
    assert_vertical_selection_change(
        "abcdef\nabcdef",
        2,
        &[
            (KeyCode::Tab, KeyModifiers::NONE),
            (KeyCode::Char('0'), KeyModifiers::NONE),
        ],
        "Xdef\nXdef",
    )
    .await;
    assert_vertical_selection_change(
        "abcdef\nabcdef",
        2,
        &[
            (KeyCode::Tab, KeyModifiers::NONE),
            (KeyCode::Char('$'), KeyModifiers::SHIFT),
        ],
        "abX\nabX",
    )
    .await;
}

#[tokio::test]
async fn o_flips_each_extend_mode_anchor() {
    assert_vertical_selection_change(
        "abcdef\nabcdef",
        2,
        &[
            (KeyCode::Tab, KeyModifiers::NONE),
            (KeyCode::Char('l'), KeyModifiers::NONE),
            (KeyCode::Char('o'), KeyModifiers::NONE),
            (KeyCode::Char('h'), KeyModifiers::NONE),
            (KeyCode::Char('h'), KeyModifiers::NONE),
        ],
        "Xef\nXef",
    )
    .await;
}

#[tokio::test]
async fn ctrl_n_promotes_each_vertical_cursor_to_its_word() {
    assert_vertical_selection_change(
        "foo bar\nfoo baz",
        0,
        &[(KeyCode::Char('n'), KeyModifiers::CONTROL)],
        "X bar\nX baz",
    )
    .await;
}

#[tokio::test]
async fn extend_mode_moves_by_grapheme_and_selects_complete_unicode_clusters() {
    assert_vertical_selection_change(
        "a👨‍👩‍👧b\na👨‍👩‍👧b",
        1,
        &[(KeyCode::Right, KeyModifiers::SHIFT)],
        "aX\naX",
    )
    .await;
}

#[tokio::test]
async fn ctrl_down_skips_shorter_lines_and_inserts_at_each_vertical_cursor() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "abcdef\nab\n\nabcdef".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);
    harness.execute_action(Action::MoveTo(4, 0)).await.unwrap();

    key(&mut harness, KeyCode::Down, KeyModifiers::CONTROL).await;

    assert!(harness.statusline_row().contains("MULTI 2/2"));
    harness.assert_cursor_at(4, 3);

    key(&mut harness, KeyCode::Char('i'), KeyModifiers::NONE).await;
    harness.type_text("X").await.unwrap();
    key(&mut harness, KeyCode::Esc, KeyModifiers::NONE).await;

    harness.assert_buffer_contents("abcdXef\nab\n\nabcdXef");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("abcdef\nab\n\nabcdef");
}

#[tokio::test]
async fn vertical_cursor_direction_changes_activate_existing_cursors_before_adding() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "abcd\nabcd\nabcd\nabcd".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);
    harness.execute_action(Action::MoveTo(2, 2)).await.unwrap();
    harness.assert_cursor_at(2, 1);

    key(&mut harness, KeyCode::Down, KeyModifiers::CONTROL).await;
    harness.assert_cursor_at(2, 2);
    key(&mut harness, KeyCode::Up, KeyModifiers::CONTROL).await;
    harness.assert_cursor_at(2, 1);
    assert!(harness.statusline_row().contains("MULTI 1/2"));
    key(&mut harness, KeyCode::Up, KeyModifiers::CONTROL).await;

    harness.assert_cursor_at(2, 0);
    assert!(harness.statusline_row().contains("MULTI 1/3"));
}

#[tokio::test]
async fn ctrl_up_orders_cursors_by_document_position() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "abcd\nabcd".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);
    harness.execute_action(Action::MoveTo(2, 2)).await.unwrap();

    key(&mut harness, KeyCode::Up, KeyModifiers::CONTROL).await;

    harness.assert_cursor_at(2, 0);
    assert!(harness.statusline_row().contains("MULTI 1/2"));
}

#[tokio::test]
async fn vertical_cursors_include_empty_lines_at_column_zero() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "a\n\nb".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    key(&mut harness, KeyCode::Down, KeyModifiers::CONTROL).await;
    harness.assert_cursor_at(0, 1);
    key(&mut harness, KeyCode::Down, KeyModifiers::CONTROL).await;

    harness.assert_cursor_at(0, 2);
    assert!(harness.statusline_row().contains("MULTI 3/3"));
}

#[tokio::test]
async fn vertical_cursors_preserve_display_columns_across_tabs_and_spaces() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "\tabc\n    abc\n\tabc".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);
    harness.execute_action(Action::MoveTo(1, 0)).await.unwrap();

    key(&mut harness, KeyCode::Down, KeyModifiers::CONTROL).await;
    harness.assert_cursor_at(4, 1);
    key(&mut harness, KeyCode::Down, KeyModifiers::CONTROL).await;

    harness.assert_cursor_at(1, 2);
    assert!(harness.statusline_row().contains("MULTI 3/3"));
}

#[tokio::test]
async fn vertical_cursor_at_a_document_boundary_is_a_noop() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "only line".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    key(&mut harness, KeyCode::Down, KeyModifiers::CONTROL).await;

    assert!(!harness.statusline_row().contains("MULTI"));
    harness.assert_cursor_at(0, 0);
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
async fn visual_ctrl_n_uses_the_exact_selection_and_adds_the_next_occurrence() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "foobar foo foobar".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    key(&mut harness, KeyCode::Char('v'), KeyModifiers::NONE).await;
    key(&mut harness, KeyCode::Char('l'), KeyModifiers::NONE).await;
    key(&mut harness, KeyCode::Char('l'), KeyModifiers::NONE).await;
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;

    harness.assert_mode(Mode::Normal);
    assert!(harness.statusline_row().contains("MULTI 2/2"));

    key(&mut harness, KeyCode::Char('c'), KeyModifiers::NONE).await;
    harness.type_text("X").await.unwrap();
    key(&mut harness, KeyCode::Esc, KeyModifiers::NONE).await;

    harness.assert_buffer_contents("Xbar X foobar");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("foobar foo foobar");
}

#[tokio::test]
async fn visual_ctrl_n_preserves_the_visual_seed_for_gv() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "foo foo".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    key(&mut harness, KeyCode::Char('v'), KeyModifiers::NONE).await;
    key(&mut harness, KeyCode::Char('l'), KeyModifiers::NONE).await;
    key(&mut harness, KeyCode::Char('l'), KeyModifiers::NONE).await;
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Esc, KeyModifiers::NONE).await;
    key(&mut harness, KeyCode::Char('g'), KeyModifiers::NONE).await;
    key(&mut harness, KeyCode::Char('v'), KeyModifiers::NONE).await;

    harness.assert_mode(Mode::Visual);
    key(&mut harness, KeyCode::Char('c'), KeyModifiers::NONE).await;
    harness.type_text("X").await.unwrap();
    key(&mut harness, KeyCode::Esc, KeyModifiers::NONE).await;

    harness.assert_buffer_contents("X foo");
}

#[tokio::test]
async fn visual_ctrl_n_keeps_multiline_selections_in_visual_mode() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "foo\nfoo".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    key(&mut harness, KeyCode::Char('v'), KeyModifiers::NONE).await;
    key(&mut harness, KeyCode::Down, KeyModifiers::NONE).await;
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;

    harness.assert_mode(Mode::Visual);
    assert!(!harness.statusline_row().contains("MULTI"));
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

#[tokio::test]
async fn d_deletes_selected_occurrences_into_the_default_register_as_one_undo() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "café café café".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('d'), KeyModifiers::NONE).await;

    harness.assert_mode(Mode::Normal);
    harness.assert_buffer_contents("  café");
    assert!(harness.statusline_row().contains("MULTI 2/2"));
    harness.assert_cursor_at(1, 0);
    harness
        .execute_action(Action::PrintRegisters)
        .await
        .unwrap();
    assert_eq!(harness.last_error(), Some("\": café\ncafé"));

    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("café café café");
}

#[tokio::test]
async fn x_deletes_selected_occurrences_without_replacing_the_default_register() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "foo foo".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);
    harness
        .editor
        .test_set_default_register(Content::charwise("seed".to_string()));

    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('x'), KeyModifiers::NONE).await;

    harness.assert_buffer_contents(" ");
    assert!(harness.statusline_row().contains("MULTI 2/2"));
    harness
        .execute_action(Action::PrintRegisters)
        .await
        .unwrap();
    assert_eq!(harness.last_error(), Some("\": seed"));
}

#[tokio::test]
async fn deleting_adjacent_selections_merges_the_collapsed_cursors() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "..x".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('d'), KeyModifiers::NONE).await;

    harness.assert_buffer_contents("x");
    assert!(harness.statusline_row().contains("MULTI 1/1"));
    harness.assert_cursor_at(0, 0);
}

#[tokio::test]
async fn deleted_selections_leave_editable_cursors_across_lines() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "foo\nfoo\nkeep".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('d'), KeyModifiers::NONE).await;
    key(&mut harness, KeyCode::Char('i'), KeyModifiers::NONE).await;
    harness.type_text("x").await.unwrap();
    key(&mut harness, KeyCode::Esc, KeyModifiers::NONE).await;

    harness.assert_buffer_contents("x\nx\nkeep");
    assert!(harness.statusline_row().contains("MULTI 2/2"));

    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("\n\nkeep");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("foo\nfoo\nkeep");
}

#[tokio::test]
async fn p_and_shift_p_replace_selected_occurrences_and_reselect_them() {
    for (paste, modifiers) in [
        (KeyCode::Char('p'), KeyModifiers::NONE),
        (KeyCode::Char('P'), KeyModifiers::SHIFT),
    ] {
        let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
        let buffer = Buffer::new(None, "foo foo".to_string());
        let mut harness = EditorHarness::with_config(buffer, config);
        harness
            .editor
            .test_set_clipboard(Box::new(MemoryClipboardProvider::with_text("X")));

        key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
        key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
        key(&mut harness, paste, modifiers).await;

        harness.assert_buffer_contents("X X");
        assert!(harness.statusline_row().contains("MULTI 2/2"));
        harness.assert_cursor_at(2, 0);

        harness.execute_action(Action::Undo).await.unwrap();
        harness.assert_buffer_contents("foo foo");
    }
}

#[tokio::test]
async fn p_and_shift_p_paste_after_and_before_collapsed_cursors() {
    for (paste, modifiers, expected, cursor) in [
        (KeyCode::Char('p'), KeyModifiers::NONE, " XX", (2, 0)),
        (KeyCode::Char('P'), KeyModifiers::SHIFT, "XX ", (1, 0)),
    ] {
        let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
        let buffer = Buffer::new(None, "foo foo".to_string());
        let mut harness = EditorHarness::with_config(buffer, config);
        harness
            .editor
            .test_set_clipboard(Box::new(MemoryClipboardProvider::with_text("X")));

        key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
        key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
        key(&mut harness, KeyCode::Char('x'), KeyModifiers::NONE).await;
        key(&mut harness, paste, modifiers).await;

        harness.assert_buffer_contents(expected);
        assert!(harness.statusline_row().contains("MULTI 2/2"));
        harness.assert_cursor_at(cursor.0, cursor.1);
    }
}

#[tokio::test]
async fn multi_cursor_delete_payloads_survive_clipboard_sync_and_paste_in_order() {
    let mut config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    config.search.ignorecase = true;
    config.search.smartcase = false;
    let buffer = Buffer::new(None, "foo FOO".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);
    let clipboard = MemoryClipboardProvider::default();
    let clipboard_text = clipboard.shared_text();
    harness.editor.test_set_clipboard(Box::new(clipboard));

    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('d'), KeyModifiers::NONE).await;
    assert_eq!(clipboard_text.lock().unwrap().as_deref(), Some("foo\nFOO"));

    key(&mut harness, KeyCode::Char('P'), KeyModifiers::SHIFT).await;

    harness.assert_buffer_contents("fooFOO ");
    assert!(harness.statusline_row().contains("MULTI 2/2"));
    harness.assert_cursor_at(3, 0);
}

#[tokio::test]
async fn blockwise_payload_mismatch_preserves_unmatched_selected_regions() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "foo foo foo".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);
    harness
        .editor
        .test_set_default_register(Content::blockwise("A\nBB".to_string()));

    for _ in 0..3 {
        key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    }
    key(&mut harness, KeyCode::Char('p'), KeyModifiers::NONE).await;

    harness.assert_buffer_contents("A BB foo");
    assert!(harness.statusline_row().contains("MULTI 3/3"));
    harness.assert_cursor_at(7, 0);
}

#[tokio::test]
async fn blockwise_payload_mismatch_inserts_nothing_at_extra_collapsed_cursors() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "foo foo foo".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);
    harness
        .editor
        .test_set_default_register(Content::blockwise("A\nBB".to_string()));

    for _ in 0..3 {
        key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    }
    key(&mut harness, KeyCode::Char('x'), KeyModifiers::NONE).await;
    key(&mut harness, KeyCode::Char('p'), KeyModifiers::NONE).await;

    harness.assert_buffer_contents(" A BB");
    assert!(harness.statusline_row().contains("MULTI 3/3"));
    harness.assert_cursor_at(4, 0);
}

#[tokio::test]
async fn external_clipboard_changes_replace_structured_multi_cursor_payloads() {
    let mut config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    config.search.ignorecase = true;
    config.search.smartcase = false;
    let buffer = Buffer::new(None, "foo FOO".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);
    let clipboard = MemoryClipboardProvider::default();
    let clipboard_text = clipboard.shared_text();
    harness.editor.test_set_clipboard(Box::new(clipboard));

    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('d'), KeyModifiers::NONE).await;
    *clipboard_text.lock().unwrap() = Some("X".to_string());
    key(&mut harness, KeyCode::Char('P'), KeyModifiers::SHIFT).await;

    harness.assert_buffer_contents("XX ");
    assert!(harness.statusline_row().contains("MULTI 2/2"));
}

#[tokio::test]
async fn linewise_register_pastes_once_per_selected_region_and_merges_same_line_cursors() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "foo foo\ntail".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);
    harness
        .editor
        .test_set_default_register(Content::linewise("AA\n".to_string()));

    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('p'), KeyModifiers::NONE).await;

    harness.assert_buffer_contents("foo foo\nAA\nAA\ntail");
    assert!(harness.statusline_row().contains("MULTI 1/1"));
    harness.assert_cursor_at(0, 1);

    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("foo foo\ntail");
}

#[tokio::test]
async fn y_yanks_ordered_unicode_regions_and_collapses_to_their_starts() {
    let mut config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    config.search.ignorecase = true;
    config.search.smartcase = false;
    let buffer = Buffer::new(None, "café CAFÉ tail".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);
    let clipboard = MemoryClipboardProvider::default();
    let clipboard_text = clipboard.shared_text();
    harness.editor.test_set_clipboard(Box::new(clipboard));

    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('y'), KeyModifiers::NONE).await;

    harness.assert_buffer_contents("café CAFÉ tail");
    assert!(harness.statusline_row().contains("MULTI 2/2"));
    harness.assert_cursor_at(5, 0);
    assert_eq!(
        clipboard_text.lock().unwrap().as_deref(),
        Some("café\nCAFÉ")
    );
    harness
        .execute_action(Action::PrintRegisters)
        .await
        .unwrap();
    assert_eq!(harness.last_error(), Some("\": café\nCAFÉ"));
}

#[tokio::test]
async fn yanked_multi_cursor_payloads_paste_back_in_region_order() {
    let mut config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    config.search.ignorecase = true;
    config.search.smartcase = false;
    let buffer = Buffer::new(None, "foo FOO".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('y'), KeyModifiers::NONE).await;
    key(&mut harness, KeyCode::Char('p'), KeyModifiers::NONE).await;

    harness.assert_buffer_contents("ffoooo FFOOOO");
    assert!(harness.statusline_row().contains("MULTI 2/2"));
}

#[tokio::test]
async fn y_respects_skipped_regions_and_does_not_create_an_undo_entry() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    let buffer = Buffer::new(None, "foo foo foo".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    harness.execute_action(Action::MoveTo(10, 0)).await.unwrap();
    key(&mut harness, KeyCode::Char('a'), KeyModifiers::NONE).await;
    harness.type_text("!").await.unwrap();
    key(&mut harness, KeyCode::Esc, KeyModifiers::NONE).await;
    harness.execute_action(Action::MoveTo(0, 0)).await.unwrap();
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    key(&mut harness, KeyCode::Char('q'), KeyModifiers::NONE).await;
    key(&mut harness, KeyCode::Char('y'), KeyModifiers::NONE).await;

    harness
        .execute_action(Action::PrintRegisters)
        .await
        .unwrap();
    assert_eq!(harness.last_error(), Some("\": foo\nfoo"));
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("foo foo foo");
}
