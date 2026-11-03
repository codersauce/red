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
    // Helix 25.07.1 selects the word and its trailing space. The cursor
    // block sits on that space.
    assert_eq!(markup(&harness), "#[hello| ]#world\n");
    press(&mut harness, "c").await;
    harness.assert_mode(Mode::Insert);
    press(&mut harness, "X").await;
    press_code(&mut harness, KeyCode::Esc).await;
    harness.assert_mode(Mode::Normal);
    harness.assert_buffer_contents("Xworld\n");
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
async fn helix_insert_cursor_matches_helix_and_o_drops_the_old_selection() {
    let mut harness = helix("hello world\n  alpha\nfoo bar foo\n");
    press(&mut harness, "i").await;
    harness.assert_mode(Mode::Insert);
    assert_eq!(harness.editor.test_felix_selection(), (0, 0, 1));
    press_code(&mut harness, KeyCode::Esc).await;

    press(&mut harness, "a").await;
    assert_eq!(harness.editor.test_felix_selection(), (0, 1, 1));
    press_code(&mut harness, KeyCode::Esc).await;

    press(&mut harness, "A").await;
    assert_eq!(harness.editor.test_felix_selection(), (11, 11, 1));
    press_code(&mut harness, KeyCode::Esc).await;

    press(&mut harness, "jI").await;
    assert_eq!(harness.buffer_line(), 1);
    assert_eq!(harness.editor.test_felix_selection().2, 1);
    let head = harness.editor.test_felix_selection().1;
    assert_eq!(head, harness.editor.test_felix_selection().0);
    press_code(&mut harness, KeyCode::Esc).await;

    let mut harness = helix("hello world\n");
    press(&mut harness, "wi").await;
    harness.assert_mode(Mode::Insert);
    assert_eq!(harness.editor.test_felix_selection(), (5, 0, 1));
    press_code(&mut harness, KeyCode::Esc).await;

    let mut harness = helix("hello world\n");
    press(&mut harness, "wa").await;
    assert_eq!(harness.editor.test_felix_selection(), (0, 6, 1));
    press_code(&mut harness, KeyCode::Esc).await;

    let mut harness = helix("hello world\n");
    press(&mut harness, "o").await;
    harness.assert_mode(Mode::Insert);
    assert_eq!(harness.buffer_line(), 1);
    assert_eq!(harness.editor.test_felix_selection().2, 1);
    assert_eq!(
        harness.editor.test_felix_selection().0,
        harness.editor.test_felix_selection().1
    );
    assert!(
        !markup(&harness).contains("#[hello"),
        "o must not leave a selection on the original line, markup {}",
        markup(&harness)
    );
    press_code(&mut harness, KeyCode::Esc).await;

    let mut harness = helix("hello\nworld\n");
    press(&mut harness, "C").await;
    press(&mut harness, "o").await;
    assert_eq!(harness.editor.test_felix_selection().2, 2);
    assert!(
        !markup(&harness).contains("#[hello"),
        "multi o must not keep the old lines selected, markup {}",
        markup(&harness)
    );
}

#[tokio::test]
async fn helix_escape_leaves_select_mode_and_keeps_the_selection() {
    let mut harness = helix("abcdef\n");
    press(&mut harness, "v").await;
    harness.assert_mode(Mode::Select);
    press_code(&mut harness, KeyCode::Esc).await;
    harness.assert_mode(Mode::Normal);
    assert_eq!(harness.editor.test_felix_selection(), (0, 0, 1));

    press(&mut harness, "vl").await;
    harness.assert_mode(Mode::Select);
    assert_eq!(markup(&harness), "#[a|b]#cdef\n");
    press_code(&mut harness, KeyCode::Esc).await;
    harness.assert_mode(Mode::Normal);
    assert_eq!(markup(&harness), "#[a|b]#cdef\n");

    // `v` toggles back to normal and still keeps the range.
    press(&mut harness, "v").await;
    harness.assert_mode(Mode::Select);
    press(&mut harness, "v").await;
    harness.assert_mode(Mode::Normal);
    assert_eq!(markup(&harness), "#[a|b]#cdef\n");

    // A motion then `v` leaves select. The next motion replaces the range
    // instead of extending it, so the mode change is real.
    let mut harness = helix("abcdef\n");
    press(&mut harness, "vlv").await;
    harness.assert_mode(Mode::Normal);
    assert_eq!(markup(&harness), "#[a|b]#cdef\n");
    press(&mut harness, "l").await;
    harness.assert_mode(Mode::Normal);
    assert_eq!(markup(&harness), "ab#[|c]#def\n");
}

#[tokio::test]
async fn helix_escape_leaves_select_from_a_count_a_pending_key_or_a_menu() {
    let mut harness = helix("abcdef\n");
    press(&mut harness, "v3").await;
    press_code(&mut harness, KeyCode::Esc).await;
    harness.assert_mode(Mode::Normal);
    assert_eq!(harness.editor.test_felix_selection(), (0, 0, 1));

    press(&mut harness, "v3l").await;
    press_code(&mut harness, KeyCode::Esc).await;
    harness.assert_mode(Mode::Normal);
    assert_eq!(markup(&harness), "#[abc|d]#ef\n");

    // A pending find and an open keymap menu used to swallow Esc and `v`.
    let mut harness = helix("abcdef\n");
    press(&mut harness, "vf").await;
    press_code(&mut harness, KeyCode::Esc).await;
    harness.assert_mode(Mode::Normal);
    assert_eq!(harness.editor.test_felix_selection(), (0, 0, 1));

    press(&mut harness, "vg").await;
    press_code(&mut harness, KeyCode::Esc).await;
    harness.assert_mode(Mode::Normal);
    press(&mut harness, "v").await;
    harness.assert_mode(Mode::Select);
    press(&mut harness, "g").await;
    press(&mut harness, "v").await;
    harness.assert_mode(Mode::Normal);

    let mut harness = helix("foo bar foo\n");
    press_sequence(&mut harness, "%sfoo{enter}").await;
    press_code(&mut harness, KeyCode::Esc).await;
    harness.assert_mode(Mode::Normal);
    let (primary, ranges) = harness.editor.test_felix_ranges();
    assert_eq!(primary, 0);
    assert_eq!(ranges, vec![(0, 2), (8, 10)]);
}

#[tokio::test]
async fn helix_mouse_click_and_scroll_move_the_selection() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

    let mut harness = helix("hello world\n");
    press(&mut harness, "w").await;
    assert_eq!(markup(&harness), "#[hello| ]#world\n");

    // Default gutter plus the padding column puts buffer column 0 at screen
    // column 4. 'w' in "hello world" is six scalars later.
    let column = 10u16;
    let row = 0u16;
    let click = |kind, modifiers| {
        Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers,
        })
    };
    harness
        .execute_event(click(
            MouseEventKind::Down(MouseButton::Left),
            KeyModifiers::NONE,
        ))
        .await
        .unwrap();
    harness
        .execute_event(click(
            MouseEventKind::Up(MouseButton::Left),
            KeyModifiers::NONE,
        ))
        .await
        .unwrap();
    assert_eq!(
        markup(&harness),
        "hello #[|w]#orld\n",
        "a click has to move the selection, otherwise the next motion jumps backward"
    );
    press(&mut harness, "w").await;
    assert!(
        !markup(&harness).starts_with("#[hello"),
        "w must continue from the click, markup {}",
        markup(&harness)
    );

    let text = (0..40)
        .map(|line| format!("word{line:02} rest"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut harness = helix(&text);
    press(&mut harness, "w").await;
    let before = markup(&harness);
    for _ in 0..4 {
        harness
            .execute_event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            }))
            .await
            .unwrap();
    }
    assert_ne!(
        markup(&harness),
        before,
        "scrolling the cursor out of the scrolloff region updates the selection"
    );
    let line = harness.buffer_line();
    press(&mut harness, "w").await;
    assert_eq!(
        harness.buffer_line(),
        line,
        "w stays on the scrolled line instead of jumping back"
    );

    let mut harness = helix("hello world\n");
    press(&mut harness, "w").await;
    harness
        .execute_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 14,
            row,
            modifiers: KeyModifiers::ALT,
        }))
        .await
        .unwrap();
    assert_eq!(
        harness.editor.test_felix_selection().2,
        2,
        "alt-click adds a cursor, markup {}",
        markup(&harness)
    );

    let mut harness = helix("hello world\n");
    harness
        .execute_event(click(
            MouseEventKind::Down(MouseButton::Left),
            KeyModifiers::NONE,
        ))
        .await
        .unwrap();
    harness
        .execute_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: column + 4,
            row,
            modifiers: KeyModifiers::NONE,
        }))
        .await
        .unwrap();
    let (anchor, head, count) = harness.editor.test_felix_selection();
    assert_eq!(count, 1);
    assert_eq!(anchor.min(head), 6, "drag starts at the press");
    assert!(
        head > anchor,
        "drag extends the primary, markup {}",
        markup(&harness)
    );
}

#[tokio::test]
async fn till_backward_on_a_run_covers_the_cursor_and_the_previous_match() {
    let mut harness = helix(
        " --> -----[Free this sentence of its brackets!]-----\n --> ------Free this sentence of its dashes!------\n",
    );
    press_sequence(&mut harness, "f-f[dglF]djf-t-dglT-d").await;
    assert_eq!(
        harness.buffer_contents(),
        " Free this sentence of its brackets!\n --> ------Free this sentence of its dashes!--\n",
        "{}",
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
    harness.assert_mode(Mode::Select);
    press(&mut harness, "ll").await;
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
    assert_eq!(anchor, head);
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

fn json_escape(text: &str) -> String {
    let mut out = String::from('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other if other.is_control() => out.push_str(&format!("\\u{:04x}", other as u32)),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn event_for_spec(spec: &str) -> Event {
    let (spec, modifiers) = if let Some(rest) = spec.strip_prefix("alt-") {
        (rest, KeyModifiers::ALT)
    } else if let Some(rest) = spec.strip_prefix("ctrl-") {
        (rest, KeyModifiers::CONTROL)
    } else {
        (spec, KeyModifiers::NONE)
    };
    let code = match spec {
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "bspace" => KeyCode::Backspace,
        "tab" => KeyCode::Tab,
        "space" => KeyCode::Char(' '),
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        other => KeyCode::Char(other.chars().next().expect("key spec")),
    };
    Event::Key(KeyEvent::new(code, modifiers))
}

async fn press_sequence(harness: &mut EditorHarness, keys: &str) {
    let mut rest = keys;
    while !rest.is_empty() {
        if let Some(spec) = rest.strip_prefix('{') {
            let end = spec.find('}').expect("key token");
            harness
                .execute_event(event_for_spec(&spec[..end]))
                .await
                .unwrap();
            rest = &spec[end + 1..];
        } else {
            let character = rest.chars().next().unwrap();
            harness
                .execute_event(Event::Key(KeyEvent::new(
                    KeyCode::Char(character),
                    KeyModifiers::NONE,
                )))
                .await
                .unwrap();
            rest = &rest[character.len_utf8()..];
        }
    }
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Normal => "normal",
        Mode::Insert => "insert",
        Mode::Command => "command",
        Mode::Search => "search",
        Mode::Select => "select",
        Mode::Visual => "visual",
        Mode::VisualLine => "visual-line",
        Mode::VisualBlock => "visual-block",
    }
}

/// Dump Helix-mode state for `scripts/helix-diff`. Set `HELIX_DUMP_CASES`
/// to a TSV file (`id`, base64 text, keys, path) and `HELIX_DUMP_OUT` to the
/// JSON destination. Without those variables the test does nothing.
#[tokio::test]
async fn dump_helix_state_for_the_differential_harness() {
    let Some(cases_path) = std::env::var_os("HELIX_DUMP_CASES") else {
        return;
    };
    let out_path = std::env::var("HELIX_DUMP_OUT").expect("HELIX_DUMP_OUT");
    let raw = std::fs::read_to_string(cases_path).unwrap();
    let mut body = String::from("{\"results\":[\n");
    let mut first = true;
    for line in raw.lines() {
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
        let id = fields.next().unwrap();
        let text = String::from_utf8(
            base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                fields.next().unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        let keys = fields.next().unwrap();
        let path = fields.next().unwrap_or("");
        let file = Some(if path.is_empty() {
            "fixture.txt".to_string()
        } else {
            path.to_string()
        });
        let mut harness =
            EditorHarness::with_config_and_size(Buffer::new(file, text), helix_config(), 100, 24);
        press_sequence(&mut harness, keys).await;
        let (primary, ranges) = harness.editor.test_felix_ranges();
        let ranges_json = ranges
            .iter()
            .map(|(anchor, head)| format!("{{\"anchor\":{anchor},\"head\":{head}}}"))
            .collect::<Vec<_>>()
            .join(",");
        if !first {
            body.push_str(",\n");
        }
        first = false;
        body.push_str(&format!(
            "{{\"id\":{},\"mode\":{},\"text\":{},\"primary\":{primary},\"ranges\":[{ranges_json}]}}",
            json_escape(id),
            json_escape(mode_name(harness.mode())),
            json_escape(&harness.buffer_contents()),
        ));
    }
    body.push_str("\n]}\n");
    std::fs::write(out_path, body).unwrap();
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

include!("fixtures/helix_captured.rs");
