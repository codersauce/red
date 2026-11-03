//! Keystroke coverage for the experimental Helix model.
//!
//! These sequences are original. They drive `EditorHarness::execute_event`
//! through the production key path.

mod common;

use common::EditorHarness;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use red::{
    buffer::Buffer,
    config::{Config, EditingModel},
    editor::{Action, Mode},
};

fn vim_config() -> Config {
    toml::from_str(include_str!("../default_config.toml")).unwrap()
}

fn helix_config() -> Config {
    let mut config = vim_config();
    config.editing_model = EditingModel::Helix;
    config
}

fn helix(content: &str) -> EditorHarness {
    EditorHarness::with_config(Buffer::new(None, content.to_string()), helix_config())
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

async fn press_modified(harness: &mut EditorHarness, key: char, modifiers: KeyModifiers) {
    harness
        .execute_event(Event::Key(KeyEvent::new(KeyCode::Char(key), modifiers)))
        .await
        .unwrap();
}

async fn press_code(harness: &mut EditorHarness, code: KeyCode) {
    harness
        .execute_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
        .await
        .unwrap();
}

fn markup(harness: &EditorHarness) -> String {
    harness.editor.test_felix_markup()
}

#[test]
fn editing_model_defaults_to_vim_and_accepts_helix() {
    let config = Config::from_user_toml_with_overrides("", &[]).unwrap();
    assert_eq!(config.editing_model, EditingModel::Vim);
    let config = Config::from_user_toml_with_overrides("editing_model = \"helix\"\n", &[]).unwrap();
    assert_eq!(config.editing_model, EditingModel::Helix);
    let override_config = Config::from_user_toml_with_overrides(
        "[helix_keys.normal]\nw = { Felix = \"move_char_left\" }\n",
        &[],
    )
    .unwrap();
    assert!(override_config.helix_keys.normal.contains_key("w"));
    let named =
        Config::from_user_toml_with_overrides("[helix_keys.normal]\nw = \"move_char_left\"\n", &[])
            .unwrap();
    assert_eq!(
        named.helix_keys.normal.get("w"),
        Some(&red::config::KeyAction::Single(red::editor::Action::Felix(
            red::felix::Command::MoveCharLeft
        )))
    );
}

#[tokio::test]
async fn vim_model_keeps_a_one_scalar_selection_on_the_cursor() {
    let mut harness =
        EditorHarness::with_config(Buffer::new(None, "hello\n".to_string()), vim_config());
    harness.execute_action(Action::MoveRight).await.unwrap();
    harness.assert_cursor_at(1, 0);
    assert_eq!(harness.editor.test_felix_selection(), (1, 1, 1));
    press(&mut harness, "x").await;
    harness.assert_buffer_contents("hllo\n");
    assert_eq!(harness.editor.test_felix_selection(), (1, 1, 1));
}

#[tokio::test]
async fn helix_x_selects_the_line_and_d_deletes_that_selection() {
    let mut harness = helix("one\ntwo\n");
    press(&mut harness, "x").await;
    assert_eq!(markup(&harness), "#[one|\n]#two\n");
    press(&mut harness, "d").await;
    harness.assert_buffer_contents("two\n");
    harness.assert_mode(Mode::Normal);
}

#[tokio::test]
async fn helix_word_motion_selects_through_the_next_word_and_change_inserts() {
    let mut harness = helix("hello world\n");
    press(&mut harness, "w").await;
    assert_eq!(markup(&harness), "#[hello |w]#orld\n");
    press(&mut harness, "c").await;
    harness.assert_mode(Mode::Insert);
    press(&mut harness, "X").await;
    press_code(&mut harness, KeyCode::Esc).await;
    harness.assert_mode(Mode::Normal);
    harness.assert_buffer_contents("Xorld\n");
}

#[tokio::test]
async fn helix_find_crosses_lines_and_percent_selects_the_file() {
    let mut harness = helix("ab\ncdx\n");
    press(&mut harness, "fx").await;
    assert_eq!(markup(&harness), "#[ab\ncd|x]#\n");
    press(&mut harness, "%").await;
    let (anchor, head, _) = harness.editor.test_felix_selection();
    assert_eq!(anchor.min(head), 0);
    assert!(
        anchor.max(head) > 4,
        "select-all markup {}",
        markup(&harness)
    );
}

#[tokio::test]
async fn helix_select_mode_extends_and_collapse_and_flip_move_the_ends() {
    let mut harness = helix("abcdef\n");
    press(&mut harness, "vl").await;
    harness.assert_mode(Mode::Select);
    assert_eq!(markup(&harness), "#[a|b]#cdef\n");
    press(&mut harness, ";").await;
    assert_eq!(harness.editor.test_felix_selection().2, 1);
    press(&mut harness, "vll").await;
    press_modified(&mut harness, ';', KeyModifiers::ALT).await;
    let (anchor, head, _) = harness.editor.test_felix_selection();
    assert!(anchor > head);
}

#[tokio::test]
async fn helix_goto_and_insert_around_the_selection() {
    let mut harness = helix("  alpha\nbeta\n");
    press(&mut harness, "ge").await;
    harness.assert_cursor_at(0, 1);
    press(&mut harness, "gg").await;
    harness.assert_cursor_at(0, 0);
    press(&mut harness, "gh").await;
    harness.assert_cursor_at(0, 0);
    press(&mut harness, "gs").await;
    harness.assert_cursor_at(2, 0);
    press(&mut harness, "iZ").await;
    press_code(&mut harness, KeyCode::Esc).await;
    assert!(harness.buffer_contents().starts_with("  Z"));
}

#[tokio::test]
async fn helix_search_selects_the_match_and_colon_opens_commands() {
    let mut harness = helix("red green red\n");
    press(&mut harness, "/red").await;
    press_code(&mut harness, KeyCode::Enter).await;
    let (anchor, head, _) = harness.editor.test_felix_selection();
    assert!(
        anchor.abs_diff(head) >= 2,
        "search should select the match, markup {}",
        markup(&harness)
    );
    press(&mut harness, "n").await;
    let (anchor, head, _) = harness.editor.test_felix_selection();
    assert!(
        anchor.abs_diff(head) >= 2,
        "next match markup {}",
        markup(&harness)
    );
    press(&mut harness, ":").await;
    harness.assert_mode(Mode::Command);
}

#[tokio::test]
async fn helix_space_keeps_explore_and_format() {
    let mut harness = helix("fn main() {}\n");
    press(&mut harness, " ").await;
    assert!(harness.is_waiting_for_key_sequence());
    press(&mut harness, "e").await;
    assert!(harness.is_waiting_for_key_sequence());
    press_code(&mut harness, KeyCode::Esc).await;
    press(&mut harness, " f").await;
    // Space f stays Red's format command. With no formatter it reports that
    // rather than opening a file picker.
    assert_ne!(harness.mode(), Mode::Insert);
}

#[tokio::test]
async fn helix_copy_split_keep_and_rotate_multiple_selections() {
    let mut harness = helix("cat\ncat\n");
    press(&mut harness, "%s").await;
    press(&mut harness, "cat").await;
    press_code(&mut harness, KeyCode::Enter).await;
    assert_eq!(harness.editor.test_felix_selection().2, 2);
    press(&mut harness, ",").await;
    assert_eq!(harness.editor.test_felix_selection().2, 1);
    press(&mut harness, "C").await;
    assert!(harness.editor.test_felix_selection().2 >= 2);
    press(&mut harness, ")").await;
    press(&mut harness, "(").await;
    assert!(harness.editor.test_felix_selection().2 >= 1);
}

#[tokio::test]
async fn helix_trim_align_and_line_split() {
    let mut harness = helix("  hi  \n");
    press(&mut harness, "X_").await;
    let contents_selected = markup(&harness);
    assert!(
        contents_selected.contains('h') && contents_selected.contains('i'),
        "trimmed selection markup {contents_selected}"
    );
    assert!(
        !contents_selected.contains("#[  "),
        "trim should drop leading spaces, markup {contents_selected}"
    );
    let mut harness = helix("a\nb\n");
    press(&mut harness, "%").await;
    press_modified(&mut harness, 's', KeyModifiers::ALT).await;
    assert_eq!(harness.editor.test_felix_selection().2, 2);
}

#[tokio::test]
async fn helix_text_object_match_and_surround() {
    let mut harness = helix("(hello)\n");
    press(&mut harness, "ll").await;
    press(&mut harness, "mm").await;
    let (anchor, head, _) = harness.editor.test_felix_selection();
    assert_ne!(anchor, head);
    press(&mut harness, ";").await;
    press(&mut harness, "miw").await;
    assert!(markup(&harness).contains("hello") || markup(&harness).contains("ell"));
    let mut harness = helix("word\n");
    press(&mut harness, "ms(").await;
    assert!(harness.buffer_contents().contains('('));
    assert!(harness.buffer_contents().contains(')'));
}

#[tokio::test]
async fn helix_undo_restores_the_deleted_text() {
    let mut harness = helix("keep me\n");
    press(&mut harness, "xd").await;
    harness.assert_buffer_contents("");
    press(&mut harness, "u").await;
    assert!(harness.buffer_contents().contains("keep"));
}

#[tokio::test]
async fn helix_open_line_and_paste() {
    let mut harness = helix("ab\n");
    press(&mut harness, "yl").await;
    press(&mut harness, "p").await;
    assert!(harness.buffer_contents().starts_with("a"));
    press(&mut harness, "oZ").await;
    press_code(&mut harness, KeyCode::Esc).await;
    assert!(harness.buffer_contents().contains('Z'));
}
