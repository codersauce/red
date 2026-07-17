mod common;

use common::EditorHarness;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use red::{
    buffer::Buffer,
    color::Color,
    config::{Config, KeyAction},
    editor::{Action, Mode, SearchDirection},
    theme::Style,
};
use std::collections::HashMap;

async fn type_normal_keys(harness: &mut EditorHarness, keys: &str) {
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

fn default_key_config() -> Config {
    toml::from_str(include_str!("../default_config.toml")).unwrap()
}

fn wrapped_long_line_content(line_count: usize) -> String {
    (1..=line_count)
        .map(|line| {
            format!(
                "Line {line:02} {}",
                "this is a long wrapped markdown-style paragraph ".repeat(8)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn wrapped_long_line_harness(line_count: usize) -> EditorHarness {
    let buffer = Buffer::new(None, wrapped_long_line_content(line_count));
    let config = Config {
        wrap: Some(true),
        ..Default::default()
    };
    EditorHarness::with_config_and_size(buffer, config, 48, 12)
}

#[tokio::test]
async fn test_basic_cursor_movement() {
    let mut harness = EditorHarness::with_content("Hello, World!\nThis is a test\nThird line");

    // Initial position
    harness.assert_cursor_at(0, 0);

    // Move right (l)
    harness.execute_action(Action::MoveRight).await.unwrap();
    harness.assert_cursor_at(1, 0);

    // Move down (j)
    harness.execute_action(Action::MoveDown).await.unwrap();
    harness.assert_cursor_at(1, 1);

    // Move left (h)
    harness.execute_action(Action::MoveLeft).await.unwrap();
    harness.assert_cursor_at(0, 1);

    // Move up (k)
    harness.execute_action(Action::MoveUp).await.unwrap();
    harness.assert_cursor_at(0, 0);
}

#[tokio::test]
async fn visual_modes_inherit_move_to_bottom() {
    for mode in [Mode::Visual, Mode::VisualLine, Mode::VisualBlock] {
        let buffer = Buffer::new(None, "one\ntwo\nthree\n".to_string());
        let mut harness = EditorHarness::with_config(buffer, default_key_config());
        harness
            .execute_action(Action::EnterMode(mode))
            .await
            .unwrap();

        type_normal_keys(&mut harness, "G").await;

        harness.assert_cursor_at(0, 2);
        assert_eq!(harness.selection(), Some((0, 0, 0, 2)));
    }
}

#[tokio::test]
async fn visual_mode_inherits_nested_normal_motions() {
    let buffer = Buffer::new(None, "one\ntwo\nthree".to_string());
    let mut config = default_key_config();
    config.keys.visual.insert(
        "g".to_string(),
        KeyAction::Nested(HashMap::from([(
            "%".to_string(),
            KeyAction::Single(Action::MatchitBackward),
        )])),
    );
    let mut harness = EditorHarness::with_config(buffer, config);
    harness.execute_action(Action::MoveToBottom).await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Visual))
        .await
        .unwrap();

    type_normal_keys(&mut harness, "gg").await;

    harness.assert_cursor_at(0, 0);
    assert_eq!(harness.selection(), Some((0, 0, 0, 2)));
}

#[tokio::test]
async fn inherited_word_motion_extends_visual_selection() {
    let buffer = Buffer::new(None, "one two three".to_string());
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    harness
        .execute_action(Action::EnterMode(Mode::Visual))
        .await
        .unwrap();

    type_normal_keys(&mut harness, "w").await;

    harness.assert_cursor_at(4, 0);
    assert_eq!(harness.selection(), Some((0, 0, 4, 0)));
}

#[tokio::test]
async fn visual_mode_inherits_normal_motion_counts() {
    let content = (0..10)
        .map(|line| format!("line-{line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let buffer = Buffer::new(None, content);
    let mut harness = EditorHarness::with_config(buffer, default_key_config());
    harness
        .execute_action(Action::EnterMode(Mode::Visual))
        .await
        .unwrap();

    type_normal_keys(&mut harness, "3j").await;

    harness.assert_cursor_at(0, 3);
    assert_eq!(harness.selection(), Some((0, 0, 0, 3)));
}

#[tokio::test]
async fn inherited_page_motion_extends_visual_selection() {
    let content = (0..50)
        .map(|line| format!("line-{line:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let buffer = Buffer::new(None, content);
    let mut harness = EditorHarness::with_config_and_size(buffer, default_key_config(), 80, 10);
    harness
        .execute_action(Action::EnterMode(Mode::Visual))
        .await
        .unwrap();

    harness
        .execute_event(Event::Key(KeyEvent::new(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        )))
        .await
        .unwrap();

    harness.assert_cursor_at(0, 8);
    assert_eq!(harness.selection(), Some((0, 0, 0, 8)));
}

#[tokio::test]
async fn inherited_screen_line_motion_extends_visual_selection() {
    let buffer = Buffer::new(None, "abcdefghijklmnopqrstuvwxyz".to_string());
    let mut config = default_key_config();
    config.wrap = Some(true);
    let mut harness = EditorHarness::with_config_and_size(buffer, config, 12, 5);
    harness
        .execute_action(Action::EnterMode(Mode::Visual))
        .await
        .unwrap();

    type_normal_keys(&mut harness, "gj").await;

    let (x, y) = harness.cursor_position();
    assert_eq!(y, 0);
    assert!(x > 0, "screen-line motion should advance on a wrapped line");
    assert_eq!(harness.selection(), Some((0, 0, x, 0)));
}

#[tokio::test]
async fn visual_keymaps_override_inherited_motions_by_mode() {
    let mut config = default_key_config();
    config
        .keys
        .normal
        .insert("Q".to_string(), KeyAction::Single(Action::MoveToBottom));
    config
        .keys
        .visual
        .insert("Q".to_string(), KeyAction::Single(Action::MoveToTop));
    config
        .keys
        .visual_block
        .insert("Q".to_string(), KeyAction::Single(Action::MoveToBottom));

    let buffer = Buffer::new(None, "one\ntwo\nthree".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);
    harness.execute_action(Action::MoveDown).await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Visual))
        .await
        .unwrap();
    type_normal_keys(&mut harness, "Q").await;
    harness.assert_cursor_at(0, 0);

    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::VisualBlock))
        .await
        .unwrap();
    type_normal_keys(&mut harness, "Q").await;
    harness.assert_cursor_at(0, 2);
}

#[tokio::test]
async fn visual_mode_does_not_inherit_non_motion_bindings() {
    let mut config = default_key_config();
    config.keys.normal.insert(
        "Q".to_string(),
        KeyAction::Single(Action::DeleteCharAtCursorPos),
    );
    let buffer = Buffer::new(None, "one".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);
    harness
        .execute_action(Action::EnterMode(Mode::Visual))
        .await
        .unwrap();

    type_normal_keys(&mut harness, "Q").await;

    harness.assert_buffer_contents("one");
    harness.assert_cursor_at(0, 0);
    assert_eq!(harness.selection(), Some((0, 0, 0, 0)));
}

#[tokio::test]
async fn test_line_movement() {
    let mut harness = EditorHarness::with_content("Hello, World!");

    // Move to end of line ($)
    harness.execute_action(Action::MoveToLineEnd).await.unwrap();
    harness.assert_cursor_at(12, 0); // "Hello, World!" is 13 chars, cursor on '!'

    // Move to start of line (0)
    harness
        .execute_action(Action::MoveToLineStart)
        .await
        .unwrap();
    harness.assert_cursor_at(0, 0);
}

#[tokio::test]
async fn test_wrap_renders_long_line_across_screen_rows() {
    let buffer = Buffer::new(None, "abcdefghijklmnop".to_string());
    let config = Config {
        wrap: Some(true),
        ..Default::default()
    };
    let mut harness = EditorHarness::with_config_and_size(buffer, config, 10, 6);

    let first_row = harness.render_row(0).unwrap();
    let second_row = harness.render_row(1).unwrap();

    assert_eq!(
        first_row.chars().skip(4).take(6).collect::<String>(),
        "abcdef"
    );
    assert_eq!(second_row.chars().take(4).collect::<String>(), "    ");
    assert_eq!(
        second_row.chars().skip(4).take(6).collect::<String>(),
        "ghijkl"
    );
}

#[tokio::test]
async fn test_nowrap_scrolls_horizontally_as_cursor_moves() {
    let buffer = Buffer::new(None, "abcdefghijklmnopqrstuvwxyz".to_string());
    let config = Config {
        wrap: Some(false),
        sidescroll: Some(1),
        sidescrolloff: Some(0),
        ..Default::default()
    };
    let mut harness = EditorHarness::with_config_and_size(buffer, config, 10, 6);

    for _ in 0..12 {
        harness.execute_action(Action::MoveRight).await.unwrap();
    }

    assert_eq!(harness.cursor_position(), (12, 0));
    assert_eq!(harness.viewport_left(), 7);
    let row = harness.render_row(0).unwrap();
    assert_eq!(row.chars().skip(4).take(6).collect::<String>(), "hijklm");

    for _ in 0..10 {
        harness.execute_action(Action::MoveLeft).await.unwrap();
    }

    assert_eq!(harness.cursor_position(), (2, 0));
    assert_eq!(harness.viewport_left(), 2);
}

#[tokio::test]
async fn test_screen_line_start_and_end_use_wrapped_segment() {
    let buffer = Buffer::new(None, "abcdefghijklmnop".to_string());
    let config = Config {
        wrap: Some(true),
        ..Default::default()
    };
    let mut harness = EditorHarness::with_config_and_size(buffer, config, 10, 6);

    for _ in 0..10 {
        harness.execute_action(Action::MoveRight).await.unwrap();
    }

    harness
        .execute_action(Action::MoveToScreenLineStart)
        .await
        .unwrap();
    harness.assert_cursor_at(6, 0);

    harness
        .execute_action(Action::MoveToScreenLineEnd)
        .await
        .unwrap();
    harness.assert_cursor_at(11, 0);
}

#[tokio::test]
async fn test_wrap_uses_skipcol_for_deep_wrapped_cursor() {
    let buffer = Buffer::new(None, "abcdefghijklmnopqrstuvwxyz0123456789".to_string());
    let config = Config {
        wrap: Some(true),
        ..Default::default()
    };
    let mut harness = EditorHarness::with_config_and_size(buffer, config, 10, 6);

    for _ in 0..24 {
        harness.execute_action(Action::MoveRight).await.unwrap();
    }

    assert!(harness.skipcol() > 0);
    assert_eq!(harness.viewport_left(), 0);
    assert!(harness.render_cursor_position().is_some());

    harness
        .execute_action(Action::MoveScreenLineDown)
        .await
        .unwrap();

    harness.assert_cursor_at(30, 0);
    assert!(harness.skipcol() > 0);
}

#[tokio::test]
async fn test_screen_line_down_updates_rendered_cursor_without_lag() {
    let content = format!(
        "{}\n{}",
        (1..=7)
            .map(|line| format!("Line {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
        "When this skill is invoked, the PR(s) to update may be specified explicitly, but in the common case, the PR(s) to update will be inferred from the branch / commit that the user is currently working on. "
            .repeat(3)
    );
    let buffer = Buffer::new(None, content);
    let config = Config {
        wrap: Some(true),
        ..Default::default()
    };
    let mut harness = EditorHarness::with_config_and_size(buffer, config, 80, 20);

    for _ in 0..7 {
        harness.execute_action(Action::MoveDown).await.unwrap();
    }

    let before = harness.render_cursor_position().unwrap();
    harness
        .execute_action(Action::MoveScreenLineDown)
        .await
        .unwrap();
    let after = harness.render_cursor_position().unwrap();

    harness.assert_cursor_at(76, 7);
    assert_eq!(after.1, before.1 + 1);
}

#[tokio::test]
async fn test_screen_line_down_reveals_hidden_wrapped_segment() {
    let content = format!("one\ntwo\nthree\n{}", "abcdefghijklmnop");
    let buffer = Buffer::new(None, content);
    let config = Config {
        wrap: Some(true),
        ..Default::default()
    };
    let mut harness = EditorHarness::with_config_and_size(buffer, config, 10, 6);

    for _ in 0..3 {
        harness.execute_action(Action::MoveDown).await.unwrap();
    }
    assert_eq!(harness.render_cursor_position(), Some((4, 3)));

    harness
        .execute_action(Action::MoveScreenLineDown)
        .await
        .unwrap();

    harness.assert_cursor_at(6, 3);
    assert_eq!(harness.render_cursor_position(), Some((4, 3)));
}

#[tokio::test]
async fn test_screen_line_up_returns_from_hidden_wrapped_segment() {
    let content = format!("one\ntwo\nthree\n{}", "abcdefghijklmnop");
    let buffer = Buffer::new(None, content);
    let config = Config {
        wrap: Some(true),
        ..Default::default()
    };
    let mut harness = EditorHarness::with_config_and_size(buffer, config, 10, 6);

    for _ in 0..3 {
        harness.execute_action(Action::MoveDown).await.unwrap();
    }
    harness
        .execute_action(Action::MoveScreenLineDown)
        .await
        .unwrap();
    harness
        .execute_action(Action::MoveScreenLineUp)
        .await
        .unwrap();

    harness.assert_cursor_at(0, 3);
    assert_eq!(harness.render_cursor_position(), Some((4, 2)));
}

#[tokio::test]
async fn test_screen_line_up_from_blank_line_paints_wrapped_target_segment() {
    let content = format!(
        "{}\n\nshort",
        "Make use of Markdown to format the pull request professionally. ".repeat(5)
    );
    let buffer = Buffer::new(None, content);
    let config = Config {
        wrap: Some(true),
        ..Default::default()
    };
    let mut harness = EditorHarness::with_config_and_size(buffer, config, 30, 8);

    harness.execute_action(Action::MoveDown).await.unwrap();
    harness.assert_cursor_at(0, 1);

    harness
        .execute_action(Action::MoveScreenLineUp)
        .await
        .unwrap();

    let (cx, cy) = harness.cursor_position();
    let rendered = harness.render_cursor_position().unwrap();

    assert_eq!(cy, 0);
    assert!(
        cx > 0,
        "screen-line up should land in the last wrapped segment"
    );
    assert!(
        rendered.1 > 0,
        "rendered cursor should be on the wrapped target segment, not the first segment"
    );
}

#[tokio::test]
async fn test_current_line_highlight_covers_all_visible_wrapped_segments() {
    let buffer = Buffer::new(
        None,
        "Make use of Markdown to format the pull request professionally. ".repeat(3),
    );
    let config = Config {
        wrap: Some(true),
        ..Default::default()
    };
    let mut harness = EditorHarness::with_config_and_size(buffer, config, 30, 8);
    let highlight = Color::Rgb {
        r: 12,
        g: 34,
        b: 56,
    };
    harness.editor.theme.line_highlight_style = Some(Style {
        bg: Some(highlight),
        ..Style::default()
    });

    for row in 0..3 {
        assert_eq!(
            harness.render_cell_bg(10, row).unwrap(),
            Some(highlight),
            "wrapped row {row} should use the current-line background"
        );
    }
}

#[tokio::test]
async fn test_nowrap_screen_line_start_after_toggle_moves_to_physical_line_start() {
    let buffer = Buffer::new(
        None,
        "When this skill is invoked, the PR(s) to update may be specified explicitly, but in the common case, the PR(s) to update will be inferred from the branch / commit that the user is currently working on. For ordinary Git usage, you may have to use a combination of `git branch` and `gh pr view <branch> --repo openai/codex --json number --jq '.number'` to determine the PR associated with the current branch / commit.".to_string(),
    );
    let config = Config {
        wrap: Some(true),
        sidescroll: Some(1),
        sidescrolloff: Some(0),
        ..Default::default()
    };
    let mut harness = EditorHarness::with_config_and_size(buffer, config, 40, 8);

    for _ in 0..6 {
        harness
            .execute_action(Action::MoveScreenLineDown)
            .await
            .unwrap();
    }
    let wrapped_cursor = harness.cursor_position().0;
    assert!(wrapped_cursor > 0);

    harness.execute_action(Action::ToggleWrap).await.unwrap();
    assert!(!harness.wrap());

    harness
        .execute_action(Action::MoveToScreenLineStart)
        .await
        .unwrap();

    harness.assert_cursor_at(0, 0);
    assert_eq!(harness.viewport_left(), 0);
}

#[tokio::test]
async fn test_next_word_keeps_cursor_visible_on_deep_wrapped_line() {
    let buffer = Buffer::new(
        None,
        "alpha beta gamma delta epsilon zeta eta theta iota kappa".to_string(),
    );
    let config = Config {
        wrap: Some(true),
        ..Default::default()
    };
    let mut harness = EditorHarness::with_config_and_size(buffer, config, 10, 4);

    for _ in 0..7 {
        harness
            .execute_action(Action::MoveToNextWord)
            .await
            .unwrap();
    }

    harness.assert_cursor_at(40, 0);
    assert!(harness.skipcol() > 0);
    assert!(harness.render_cursor_position().is_some());
}

#[tokio::test]
async fn test_word_movement() {
    let mut harness = EditorHarness::with_content("Hello world this is test");

    // Move to next word (w)
    harness
        .execute_action(Action::MoveToNextWord)
        .await
        .unwrap();
    harness.assert_cursor_at(6, 0); // Should be at 'w' of 'world'

    // Move to next word again
    harness
        .execute_action(Action::MoveToNextWord)
        .await
        .unwrap();
    harness.assert_cursor_at(12, 0); // Should be at 't' of 'this'

    // Move to previous word (b)
    harness
        .execute_action(Action::MoveToPreviousWord)
        .await
        .unwrap();
    harness.assert_cursor_at(6, 0); // Back at 'w' of 'world'
}

#[tokio::test]
async fn test_next_word_matches_nvim_on_delimiters() {
    let mut harness = EditorHarness::with_content("foo:bar baz");

    harness
        .execute_action(Action::MoveToNextWord)
        .await
        .unwrap();
    harness.assert_cursor_at(3, 0); // foo -> :

    harness
        .execute_action(Action::MoveToNextWord)
        .await
        .unwrap();
    harness.assert_cursor_at(4, 0); // : -> bar
}

#[tokio::test]
async fn test_next_word_from_prefix_punctuation_moves_to_keyword() {
    let mut harness = EditorHarness::with_content("&Config::path");

    harness
        .execute_action(Action::MoveToNextWord)
        .await
        .unwrap();

    harness.assert_cursor_at(1, 0); // & -> Config
}

#[tokio::test]
async fn test_word_movement_preserves_visible_viewport() {
    let content = (1..=20)
        .map(|line| {
            if line == 8 {
                "alpha beta gamma".to_string()
            } else {
                format!("Line {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut harness = EditorHarness::with_content(&content);
    harness.execute_action(Action::GoToLine(8)).await.unwrap();
    let viewport_top = harness.viewport_top();

    harness
        .execute_action(Action::MoveToNextWord)
        .await
        .unwrap();
    harness.assert_cursor_at(6, 7);
    assert_eq!(harness.viewport_top(), viewport_top);

    harness
        .execute_action(Action::MoveToPreviousWord)
        .await
        .unwrap();
    harness.assert_cursor_at(0, 7);
    assert_eq!(harness.viewport_top(), viewport_top);
}

#[tokio::test]
async fn test_search_word_under_cursor_moves_to_next_match() {
    let mut harness = EditorHarness::with_content("alpha beta alpha gamma alpha");

    harness
        .execute_action(Action::SearchWordUnderCursor)
        .await
        .unwrap();
    harness.assert_cursor_at(11, 0);

    harness.execute_action(Action::FindNext).await.unwrap();
    harness.assert_cursor_at(23, 0);

    harness.execute_action(Action::FindPrevious).await.unwrap();
    harness.assert_cursor_at(11, 0);
}

#[tokio::test]
async fn search_preview_moves_while_typing_and_escape_restores_origin() {
    let mut harness = EditorHarness::with_content("start\nalpha\nmiddle\nalpha");

    harness
        .execute_action(Action::EnterSearch(SearchDirection::Forward))
        .await
        .unwrap();
    type_normal_keys(&mut harness, "alp").await;

    harness.assert_cursor_at(0, 1);
    assert_eq!(harness.commandline_text(), "alp");

    harness
        .execute_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
        .await
        .unwrap();

    harness.assert_mode(Mode::Normal);
    harness.assert_cursor_at(0, 0);
}

#[tokio::test]
async fn search_prompt_cursor_follows_active_draft() {
    let mut harness = EditorHarness::with_content("wrap\nother");

    harness
        .execute_action(Action::EnterSearch(SearchDirection::Forward))
        .await
        .unwrap();
    type_normal_keys(&mut harness, "wrap").await;

    assert_eq!(harness.commandline_text(), "wrap");
    assert_eq!(harness.render_cursor_position(), Some((5, 23)));
}

#[tokio::test]
async fn search_enter_commits_preview_and_n_repeats_direction() {
    let mut harness = EditorHarness::with_content("alpha\nbeta\nalpha\nbeta\nalpha");

    harness
        .execute_action(Action::EnterSearch(SearchDirection::Forward))
        .await
        .unwrap();
    type_normal_keys(&mut harness, "alpha").await;
    harness.assert_cursor_at(0, 2);

    harness
        .execute_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
        .await
        .unwrap();
    harness.assert_mode(Mode::Normal);
    harness.assert_cursor_at(0, 2);

    harness.execute_action(Action::RepeatSearch).await.unwrap();
    harness.assert_cursor_at(0, 4);

    harness
        .execute_action(Action::RepeatSearchOpposite)
        .await
        .unwrap();
    harness.assert_cursor_at(0, 2);
}

#[tokio::test]
async fn backward_search_previews_previous_match() {
    let mut harness = EditorHarness::with_content("alpha\nbeta\nalpha\nbeta\nalpha");
    harness
        .execute_action(Action::SetCursor(0, 4))
        .await
        .unwrap();

    harness
        .execute_action(Action::EnterSearch(SearchDirection::Backward))
        .await
        .unwrap();
    type_normal_keys(&mut harness, "beta").await;

    harness.assert_cursor_at(0, 3);
    assert!(harness.commandline_row().starts_with("?beta"));
}

#[tokio::test]
async fn search_mouse_scroll_is_ignored_while_prompt_is_active() {
    let content = (0..80)
        .map(|line| format!("Line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let buffer = Buffer::new(None, content);
    let mut harness = EditorHarness::with_config(buffer, Config::default());

    harness
        .execute_action(Action::EnterSearch(SearchDirection::Forward))
        .await
        .unwrap();
    let viewport_top = harness.viewport_top();
    let cursor = harness.cursor_position();

    harness
        .execute_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 10,
            row: 10,
            modifiers: KeyModifiers::NONE,
        }))
        .await
        .unwrap();

    assert_eq!(harness.viewport_top(), viewport_top);
    assert_eq!(harness.cursor_position(), cursor);
}

#[tokio::test]
async fn search_highlights_visible_matches_and_nohlsearch_clears_them() {
    let mut harness = EditorHarness::with_content("alpha beta\nmiddle\nalpha gamma");

    harness
        .execute_action(Action::SearchWordUnderCursor)
        .await
        .unwrap();

    let first_row = harness.render_row(0).unwrap();
    let first_match_x = first_row.find("alpha").unwrap();
    let non_match_x = first_row.find("beta").unwrap();
    let default_bg = harness.render_cell_bg(non_match_x, 0).unwrap();
    assert_ne!(
        harness.render_cell_bg(first_match_x, 0).unwrap(),
        default_bg
    );

    harness
        .execute_action(Action::Command("noh".to_string()))
        .await
        .unwrap();

    assert_eq!(
        harness.render_cell_bg(first_match_x, 0).unwrap(),
        default_bg
    );
}

#[tokio::test]
async fn search_uses_rust_regex_and_case_options() {
    let mut config = Config::default();
    config.search.ignorecase = true;
    let buffer = Buffer::new(None, "start\nFOO\nf12".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    harness
        .execute_action(Action::EnterSearch(SearchDirection::Forward))
        .await
        .unwrap();
    type_normal_keys(&mut harness, "foo").await;
    harness.assert_cursor_at(0, 1);

    harness
        .execute_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
        .await
        .unwrap();
    harness
        .execute_action(Action::EnterSearch(SearchDirection::Forward))
        .await
        .unwrap();
    type_normal_keys(&mut harness, r"f\d+").await;
    harness.assert_cursor_at(0, 2);
}

#[tokio::test]
async fn search_preview_and_highlight_handle_wide_prefix_text() {
    let mut harness = EditorHarness::with_content("👋 alpha\nplain alpha");

    harness
        .execute_action(Action::EnterSearch(SearchDirection::Forward))
        .await
        .unwrap();
    type_normal_keys(&mut harness, "alpha").await;

    harness.assert_cursor_at(2, 0);
    let row = harness.render_row(0).unwrap();
    let match_x = row.find("alpha").unwrap();
    let default_x = row.find("👋").unwrap();
    let default_bg = harness.render_cell_bg(default_x, 0).unwrap();
    assert_ne!(harness.render_cell_bg(match_x, 0).unwrap(), default_bg);
}

#[tokio::test]
async fn test_search_word_under_cursor_keeps_underscore_in_keyword() {
    let mut harness = EditorHarness::with_content("foo_bar foo bar foo_bar");

    harness
        .execute_action(Action::SearchWordUnderCursor)
        .await
        .unwrap();

    harness.assert_cursor_at(16, 0);
}

#[tokio::test]
async fn test_search_word_under_cursor_ignores_punctuation() {
    let mut harness = EditorHarness::with_content("alpha ! alpha");
    harness.execute_action(Action::MoveTo(6, 0)).await.unwrap();

    harness
        .execute_action(Action::SearchWordUnderCursor)
        .await
        .unwrap();

    harness.assert_cursor_at(6, 0);
}

#[tokio::test]
async fn test_file_movement() {
    let mut harness = EditorHarness::with_content("Line 1\nLine 2\nLine 3\nLine 4\nLine 5");

    // Move to bottom of file (G)
    // buffer.len() returns len_lines() - 1, which is 4 for 5 lines
    // Last line index = buffer.len() = 4
    harness.execute_action(Action::MoveToBottom).await.unwrap();
    harness.assert_cursor_at(0, 4); // Last line is at index 4

    // Move to top of file (gg)
    harness.execute_action(Action::MoveToTop).await.unwrap();
    harness.assert_cursor_at(0, 0); // First line
}

#[tokio::test]
async fn jump_back_records_current_position_for_jump_forward() {
    let mut harness = EditorHarness::with_content("one\ntwo\nthree\nfour\nfive");

    harness.execute_action(Action::MoveTo(0, 3)).await.unwrap();
    harness.assert_cursor_at(0, 2);

    harness.execute_action(Action::JumpBack).await.unwrap();
    harness.assert_cursor_at(0, 0);

    harness.execute_action(Action::JumpForward).await.unwrap();
    harness.assert_cursor_at(0, 2);
}

#[tokio::test]
async fn repeated_jump_back_and_forward_do_not_skip_entries() {
    let mut harness = EditorHarness::with_content("one\ntwo\nthree\nfour\nfive");

    harness.execute_action(Action::MoveTo(0, 2)).await.unwrap();
    harness.execute_action(Action::MoveTo(0, 4)).await.unwrap();
    harness.execute_action(Action::MoveTo(0, 5)).await.unwrap();
    harness.assert_cursor_at(0, 4);

    harness.execute_action(Action::JumpBack).await.unwrap();
    harness.assert_cursor_at(0, 3);

    harness.execute_action(Action::JumpBack).await.unwrap();
    harness.assert_cursor_at(0, 1);

    harness.execute_action(Action::JumpForward).await.unwrap();
    harness.assert_cursor_at(0, 3);

    harness.execute_action(Action::JumpForward).await.unwrap();
    harness.assert_cursor_at(0, 4);
}

#[tokio::test]
async fn new_jump_from_middle_discards_forward_entries() {
    let mut harness = EditorHarness::with_content("one\ntwo\nthree\nfour\nfive");

    harness.execute_action(Action::MoveTo(0, 2)).await.unwrap();
    harness.execute_action(Action::MoveTo(0, 4)).await.unwrap();
    harness.assert_cursor_at(0, 3);

    harness.execute_action(Action::JumpBack).await.unwrap();
    harness.assert_cursor_at(0, 1);

    harness.execute_action(Action::MoveTo(0, 5)).await.unwrap();
    harness.assert_cursor_at(0, 4);

    harness.execute_action(Action::JumpForward).await.unwrap();
    harness.assert_cursor_at(0, 4);

    harness.execute_action(Action::JumpBack).await.unwrap();
    harness.assert_cursor_at(0, 1);
}

#[tokio::test]
async fn default_normal_keys_map_ctrl_o_and_tab_to_jumplist_navigation() {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    assert_eq!(
        config.keys.normal.get("Tab"),
        Some(&KeyAction::Single(Action::JumpForward))
    );
    let buffer = Buffer::new(None, "one\ntwo\nthree\nfour\nfive".to_string());
    let mut harness = EditorHarness::with_config(buffer, config);

    harness.execute_action(Action::MoveTo(0, 3)).await.unwrap();
    harness.assert_cursor_at(0, 2);

    harness
        .execute_event(Event::Key(KeyEvent::new(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL,
        )))
        .await
        .unwrap();
    harness.assert_cursor_at(0, 0);

    let tab_event = Event::Key(KeyEvent::from(KeyCode::Tab));
    harness.execute_event(tab_event).await.unwrap();
    harness.assert_cursor_at(0, 2);
}

#[tokio::test]
async fn test_movement_boundaries() {
    let mut harness = EditorHarness::with_content("abc\ndef");

    // Try to move left at start of buffer
    harness.assert_cursor_at(0, 0);
    harness.execute_action(Action::MoveLeft).await.unwrap();
    harness.assert_cursor_at(0, 0); // Should stay at (0, 0)

    // Try to move up at start of buffer
    harness.execute_action(Action::MoveUp).await.unwrap();
    harness.assert_cursor_at(0, 0); // Should stay at (0, 0)

    // Move to end of file
    harness.execute_action(Action::MoveToBottom).await.unwrap();
    harness.execute_action(Action::MoveToLineEnd).await.unwrap();
    // MoveToBottom goes to line 1 (last line) for "abc\ndef"
    // MoveToLineEnd on "def" puts us on the last character
    harness.assert_cursor_at(2, 1); // On 'f' in "def"

    // Try to move right at end of line
    harness.execute_action(Action::MoveRight).await.unwrap();
    harness.assert_cursor_at(2, 1); // Should stay on 'f' in normal mode

    // Try to move down at end of buffer (already at last line)
    harness.execute_action(Action::MoveDown).await.unwrap();
    harness.assert_cursor_at(2, 1); // Should stay at line 1
}

#[tokio::test]
async fn test_normal_cursor_clamps_when_moving_to_shorter_line() {
    let mut harness = EditorHarness::with_content("abcdef\nxy");

    harness.execute_action(Action::MoveToLineEnd).await.unwrap();
    harness.assert_cursor_at(5, 0);

    harness.execute_action(Action::MoveDown).await.unwrap();

    harness.assert_cursor_at(1, 1); // On 'y', not one past the line
}

#[tokio::test]
async fn test_vertical_movement_restores_cursor_goal_after_empty_line() {
    let mut harness = EditorHarness::with_content("abcdef\n\nabcdefghijkl");

    for _ in 0..5 {
        harness.execute_action(Action::MoveRight).await.unwrap();
    }
    harness.assert_cursor_at(5, 0);

    harness.execute_action(Action::MoveDown).await.unwrap();
    harness.assert_cursor_at(0, 1);

    harness.execute_action(Action::MoveDown).await.unwrap();
    harness.assert_cursor_at(5, 2);
}

#[tokio::test]
async fn test_vertical_movement_restores_cursor_goal_after_short_line() {
    let mut harness = EditorHarness::with_content("abcdef\nxy\nabcdefghijkl");

    for _ in 0..5 {
        harness.execute_action(Action::MoveRight).await.unwrap();
    }
    harness.assert_cursor_at(5, 0);

    harness.execute_action(Action::MoveDown).await.unwrap();
    harness.assert_cursor_at(1, 1);

    harness.execute_action(Action::MoveDown).await.unwrap();
    harness.assert_cursor_at(5, 2);
}

#[tokio::test]
async fn test_line_end_goal_tracks_each_target_line_end() {
    let mut harness = EditorHarness::with_content("x\na much longer line");

    harness.execute_action(Action::MoveToLineEnd).await.unwrap();
    harness.assert_cursor_at(0, 0);

    harness.execute_action(Action::MoveDown).await.unwrap();

    harness.assert_cursor_at(17, 1);
}

#[tokio::test]
async fn test_line_end_goal_survives_shorter_intermediate_line() {
    let mut harness =
        EditorHarness::with_content("abcdefghijklmnop\nabcdefghijkl\nabcdefghijklmnop");

    harness.execute_action(Action::MoveToLineEnd).await.unwrap();
    harness.assert_cursor_at(15, 0);

    harness.execute_action(Action::MoveDown).await.unwrap();
    harness.assert_cursor_at(11, 1);

    harness.execute_action(Action::MoveDown).await.unwrap();
    harness.assert_cursor_at(15, 2);
}

#[tokio::test]
async fn test_line_end_goal_survives_shorter_intermediate_line_from_keys() {
    let mut config = Config {
        scrolloff: Some(3),
        ..Default::default()
    };
    config.keys.normal.extend(HashMap::from([
        (
            "g".to_string(),
            KeyAction::Nested(HashMap::from([(
                "g".to_string(),
                KeyAction::Single(Action::MoveToTop),
            )])),
        ),
        ("j".to_string(), KeyAction::Single(Action::MoveDown)),
        ("$".to_string(), KeyAction::Single(Action::MoveToLineEnd)),
    ]));
    let buffer = Buffer::new(
        None,
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nabcdefghijklmnop\nabcdefghijkl\nabcdefghijklmnop\ntail"
            .to_string(),
    );
    let mut harness = EditorHarness::with_config(buffer, config);

    type_normal_keys(&mut harness, "gg8j$jj").await;

    harness.assert_cursor_at(15, 10);
}

#[tokio::test]
async fn test_last_line_char_resets_line_end_goal_to_display_column() {
    let mut harness = EditorHarness::with_content("abc   \nabcdefghijkl");

    harness.execute_action(Action::MoveToLineEnd).await.unwrap();
    harness
        .execute_action(Action::MoveToLastLineChar)
        .await
        .unwrap();
    harness.assert_cursor_at(2, 0);

    harness.execute_action(Action::MoveDown).await.unwrap();

    harness.assert_cursor_at(2, 1);
}

#[tokio::test]
async fn test_vertical_goal_can_render_inside_wide_grapheme() {
    let mut harness = EditorHarness::with_content("abc\n你");

    harness.execute_action(Action::MoveRight).await.unwrap();
    harness.execute_action(Action::MoveRight).await.unwrap();
    harness.assert_cursor_at(2, 0);

    harness.execute_action(Action::MoveDown).await.unwrap();

    harness.assert_cursor_at(0, 1);
    assert_eq!(harness.render_cursor_position(), Some((5, 1)));
}

#[tokio::test]
async fn test_line_end_goal_renders_on_final_wide_grapheme_cell() {
    let mut harness = EditorHarness::with_content("x\na你");

    harness.execute_action(Action::MoveToLineEnd).await.unwrap();
    harness.execute_action(Action::MoveDown).await.unwrap();

    harness.assert_cursor_at(1, 1);
    assert_eq!(harness.render_cursor_position(), Some((6, 1)));
}

#[tokio::test]
async fn test_first_last_line_char_movement() {
    let mut harness = EditorHarness::with_content("    Hello, World!    ");

    // Move to first non-whitespace character (^)
    harness
        .execute_action(Action::MoveToFirstLineChar)
        .await
        .unwrap();
    harness.assert_cursor_at(4, 0); // Should be at 'H'

    // Move to end, then to last non-whitespace character (g_)
    harness.execute_action(Action::MoveToLineEnd).await.unwrap();
    harness
        .execute_action(Action::MoveToLastLineChar)
        .await
        .unwrap();
    // "    Hello, World!    " - last non-whitespace is at position 16 (!)
    harness.assert_cursor_at(16, 0); // Should be at '!' (excluding trailing spaces)
}

#[tokio::test]
async fn test_page_movement() {
    // Create content with many lines
    let content = (0..50)
        .map(|i| format!("Line {}", i))
        .collect::<Vec<_>>()
        .join("\n");
    let mut harness = EditorHarness::with_content(&content);

    // Page down
    harness.execute_action(Action::PageDown).await.unwrap();
    // Exact position depends on viewport size, but cursor should have moved down
    let (_, y1) = harness.cursor_position();

    // Page down again
    harness.execute_action(Action::PageDown).await.unwrap();
    let (_, y2) = harness.cursor_position();
    assert!(y2 > y1, "Cursor should move down on PageDown");

    // Page up
    harness.execute_action(Action::PageUp).await.unwrap();
    let (_, y3) = harness.cursor_position();
    assert!(y3 < y2, "Cursor should move up on PageUp");
}

#[tokio::test]
async fn test_page_movement_uses_partial_pages_at_file_edges() {
    let content = (1..=10)
        .map(|line| format!("Line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut harness = EditorHarness::with_content(&content);
    harness.editor.test_set_size(80, 7);

    harness.execute_action(Action::GoToLine(8)).await.unwrap();
    harness.execute_action(Action::PageDown).await.unwrap();
    harness.assert_cursor_at(0, 9);
    assert_eq!(harness.current_line(), Some("Line 10".to_string()));

    harness.execute_action(Action::GoToLine(3)).await.unwrap();
    harness.execute_action(Action::PageUp).await.unwrap();
    harness.assert_cursor_at(0, 0);
    assert_eq!(harness.current_line(), Some("Line 1\n".to_string()));
}

#[tokio::test]
async fn test_page_render_applies_scrolloff_before_first_frame() {
    let content = (0..20)
        .map(|line| format!("line-{line:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let config = Config {
        scrolloff: Some(2),
        ..Default::default()
    };
    let buffer = Buffer::new(None, content);
    let mut harness = EditorHarness::with_config(buffer, config);
    harness.editor.test_set_size(80, 7);
    harness.set_viewport_cursor(1, 0, 4);

    let first_row = harness.render_row(0).unwrap();

    assert!(
        first_row.contains("line-03"),
        "first rendered frame should use the scrolloff-corrected viewport"
    );
    assert!(
        !first_row.contains("line-01"),
        "first rendered frame should not paint the stale pre-scrolloff viewport"
    );
}

#[tokio::test]
async fn test_ctrl_page_keys_apply_scrolloff_immediately() {
    let content = (0..40)
        .map(|line| format!("line-{line:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut config = Config {
        scrolloff: Some(2),
        ..Default::default()
    };
    config
        .keys
        .normal
        .insert("Ctrl-f".to_string(), KeyAction::Single(Action::PageDown));
    config
        .keys
        .normal
        .insert("Ctrl-b".to_string(), KeyAction::Single(Action::PageUp));
    let buffer = Buffer::new(None, content);
    let mut harness = EditorHarness::with_config(buffer, config);
    harness.editor.test_set_size(80, 7);

    harness
        .execute_event(Event::Key(KeyEvent::new(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        )))
        .await
        .unwrap();
    assert_eq!(harness.buffer_line(), 5);
    assert_eq!(harness.viewport_top(), 3);
    assert_eq!(harness.buffer_line() - harness.viewport_top(), 2);

    harness
        .execute_action(Action::SetCursor(0, 12))
        .await
        .unwrap();
    harness
        .execute_event(Event::Key(KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
        )))
        .await
        .unwrap();
    assert_eq!(harness.buffer_line(), 7);
    assert_eq!(harness.viewport_top(), 5);
    assert_eq!(harness.buffer_line() - harness.viewport_top(), 2);
}

#[tokio::test]
async fn test_goto_line() {
    let mut harness = EditorHarness::with_content("Line 1\nLine 2\nLine 3\nLine 4\nLine 5");

    // GoToLine appears to be 1-based like vim
    // Go to line 3
    harness.execute_action(Action::GoToLine(3)).await.unwrap();
    harness.assert_cursor_at(0, 2);

    // Go to line 5
    harness.execute_action(Action::GoToLine(5)).await.unwrap();
    harness.assert_cursor_at(0, 4);

    // Go to line 1
    harness.execute_action(Action::GoToLine(1)).await.unwrap();
    harness.assert_cursor_at(0, 0);
}

#[tokio::test]
async fn test_movement_clamps_to_last_real_line_with_trailing_newline() {
    let mut harness = EditorHarness::with_content("Line 1\nLine 2\nLine 3\n");

    for _ in 0..10 {
        harness.execute_action(Action::MoveDown).await.unwrap();
    }
    harness.assert_cursor_at(0, 2);
    assert_eq!(harness.current_line(), Some("Line 3\n".to_string()));

    harness.execute_action(Action::MoveToBottom).await.unwrap();
    harness.assert_cursor_at(0, 2);
    assert_eq!(harness.current_line(), Some("Line 3\n".to_string()));

    harness.execute_action(Action::GoToLine(999)).await.unwrap();
    harness.assert_cursor_at(0, 2);
    assert_eq!(harness.current_line(), Some("Line 3\n".to_string()));

    harness
        .execute_action(Action::MoveTo(0, 999))
        .await
        .unwrap();
    harness.assert_cursor_at(0, 2);
    assert_eq!(harness.current_line(), Some("Line 3\n".to_string()));

    harness
        .execute_action(Action::SetCursor(0, 999))
        .await
        .unwrap();
    harness.assert_cursor_at(0, 2);
    assert_eq!(harness.current_line(), Some("Line 3\n".to_string()));
}

#[tokio::test]
async fn test_scrolling_clamps_to_last_real_line() {
    let content = (1..=8)
        .map(|line| format!("Line {line}"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let mut harness = EditorHarness::with_content(&content);
    harness.editor.test_set_size(80, 6);

    for _ in 0..10 {
        harness.execute_action(Action::PageDown).await.unwrap();
    }
    assert!(harness.buffer_line() <= 7);
    assert_ne!(harness.current_line(), Some(String::new()));

    harness.execute_action(Action::MoveToBottom).await.unwrap();
    assert_eq!(harness.buffer_line(), 7);
    assert_eq!(harness.current_line(), Some("Line 8\n".to_string()));

    for _ in 0..10 {
        harness.execute_action(Action::ScrollDown).await.unwrap();
    }
    assert_eq!(harness.buffer_line(), 7);
    assert_eq!(harness.current_line(), Some("Line 8\n".to_string()));
}

#[tokio::test]
async fn test_wrapped_move_to_bottom_reaches_visible_last_line() {
    let mut harness = wrapped_long_line_harness(86);

    harness.execute_action(Action::MoveToBottom).await.unwrap();

    assert_eq!(harness.buffer_line(), 85);
    assert!(harness
        .current_line()
        .as_deref()
        .is_some_and(|line| line.starts_with("Line 86 ")));
    assert!(
        harness.render_cursor_position().is_some(),
        "last logical line should also be visible after G"
    );
}

#[tokio::test]
async fn test_wrapped_go_to_last_line_reaches_visible_last_line() {
    let mut harness = wrapped_long_line_harness(86);

    harness.execute_action(Action::GoToLine(86)).await.unwrap();

    assert_eq!(harness.buffer_line(), 85);
    assert!(
        harness.render_cursor_position().is_some(),
        ":$ should leave the last line visible in wrapped files"
    );
}

#[tokio::test]
async fn test_wrapped_page_down_reaches_end_of_large_wrapped_file() {
    let mut harness = wrapped_long_line_harness(86);

    for _ in 0..20 {
        harness.execute_action(Action::PageDown).await.unwrap();
    }

    assert_eq!(harness.buffer_line(), 85);
    assert!(
        harness.render_cursor_position().is_some(),
        "Ctrl-f should not stop before the visible end of a wrapped file"
    );
}

#[tokio::test]
async fn test_wrapped_move_down_reaches_end_of_large_wrapped_file() {
    let mut harness = wrapped_long_line_harness(86);

    for _ in 0..160 {
        harness.execute_action(Action::MoveDown).await.unwrap();
    }

    assert_eq!(harness.buffer_line(), 85);
    assert!(
        harness.render_cursor_position().is_some(),
        "holding j should not loop before the end of a wrapped file"
    );
}

#[tokio::test]
async fn test_wrapped_move_down_keeps_cursor_bottom_anchored() {
    let content = (0..30)
        .map(|line| {
            if matches!(line, 2 | 5) {
                format!("Line {line} {}", "wrapped segment ".repeat(8))
            } else {
                format!("Line {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    for scrolloff in [None, Some(2)] {
        let buffer = Buffer::new(None, content.clone());
        let config = Config {
            wrap: Some(true),
            scrolloff,
            ..Default::default()
        };
        let mut harness = EditorHarness::with_config_and_size(buffer, config, 40, 12);
        let mut previous_screen_row = harness.render_cursor_position().unwrap().1;

        for _ in 0..15 {
            harness.execute_action(Action::MoveDown).await.unwrap();
            let screen_row = harness.render_cursor_position().unwrap().1;
            if scrolloff.is_none() {
                assert!(
                    screen_row >= previous_screen_row,
                    "wrapped j motion moved the cursor from screen row \
                     {previous_screen_row} to {screen_row} at buffer line {} with vtop {}",
                    harness.buffer_line(),
                    harness.viewport_top()
                );
            } else if harness.viewport_top() > 0 {
                assert_ne!(
                    screen_row,
                    0,
                    "wrapped j motion snapped the cursor to the top at buffer line {} \
                     with vtop {} and scrolloff {scrolloff:?}",
                    harness.buffer_line(),
                    harness.viewport_top()
                );
            }
            previous_screen_row = screen_row;
        }

        assert!(harness.viewport_top() > 0);

        for _ in 0..15 {
            harness.execute_action(Action::MoveUp).await.unwrap();
            let screen_row = harness.render_cursor_position().unwrap().1;
            if scrolloff.is_none() {
                assert!(
                    screen_row <= previous_screen_row,
                    "wrapped k motion moved the cursor from screen row \
                     {previous_screen_row} to {screen_row} at buffer line {} with vtop {}",
                    harness.buffer_line(),
                    harness.viewport_top()
                );
            }
            previous_screen_row = screen_row;
        }

        assert_eq!(harness.buffer_line(), 0);
        assert_eq!(harness.viewport_top(), 0);
    }
}

#[tokio::test]
async fn test_movement_preserves_mode() {
    let mut harness = EditorHarness::with_content("Hello\nWorld");

    // Verify we start in normal mode
    harness.assert_mode(red::editor::Mode::Normal);

    // Move around
    harness.execute_action(Action::MoveRight).await.unwrap();
    harness.execute_action(Action::MoveDown).await.unwrap();

    // Should still be in normal mode
    harness.assert_mode(red::editor::Mode::Normal);
}

#[tokio::test]
async fn test_scroll_movement() {
    let content = (0..30)
        .map(|i| format!("Line {}", i))
        .collect::<Vec<_>>()
        .join("\n");
    let mut harness = EditorHarness::with_content(&content);

    // Scroll down
    harness.execute_action(Action::ScrollDown).await.unwrap();
    // Viewport should have scrolled, but exact behavior depends on implementation

    // Scroll up
    harness.execute_action(Action::ScrollUp).await.unwrap();
    // Viewport should have scrolled back
}

#[tokio::test]
async fn test_mouse_scroll_continues_after_cursor_reaches_scrolloff() {
    let content = (0..80)
        .map(|line| format!("Line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let config = Config {
        mouse_scroll_lines: Some(3),
        scrolloff: Some(3),
        ..Default::default()
    };
    let buffer = Buffer::new(None, content);
    let mut harness = EditorHarness::with_config(buffer, config);

    harness
        .execute_action(Action::SetCursor(0, 48))
        .await
        .unwrap();

    for _ in 0..8 {
        harness.execute_action(Action::ScrollDown).await.unwrap();
    }

    assert!(
        harness.viewport_top() > 44,
        "scroll down should continue once the cursor reaches scrolloff"
    );
    assert_eq!(
        harness.buffer_line() - harness.viewport_top(),
        3,
        "cursor should stay at the top scrolloff margin while scrolling down"
    );

    for _ in 0..9 {
        harness.execute_action(Action::ScrollUp).await.unwrap();
    }

    assert!(
        harness.viewport_top() < 30,
        "scroll up should continue once the cursor reaches scrolloff"
    );
    assert_eq!(
        harness.buffer_line() - harness.viewport_top(),
        18,
        "cursor should stay at the bottom scrolloff margin while scrolling up"
    );
}

#[tokio::test]
async fn test_mouse_scroll_down_at_wrapped_eof_does_not_underflow() {
    let content = (0..30)
        .map(|line| format!("Line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let config = Config {
        mouse_scroll_lines: Some(3),
        wrap: Some(true),
        ..Default::default()
    };
    let buffer = Buffer::new(None, content);
    let mut harness = EditorHarness::with_config_and_size(buffer, config, 20, 8);
    let last_line = harness.line_count();

    harness.set_viewport_cursor(last_line, 0, 0);
    harness.execute_action(Action::ScrollDown).await.unwrap();

    assert_eq!(harness.viewport_top(), last_line);
}

#[tokio::test]
async fn test_move_to_specific_position() {
    let mut harness = EditorHarness::with_content("Hello\nWorld\nTest");

    // MoveTo(x, y) where y is 1-based line number (like vim)
    // Move to position (3, 1) - line 1 (0-indexed = 0), column 3
    harness.execute_action(Action::MoveTo(3, 1)).await.unwrap();
    harness.assert_cursor_at(3, 0); // At 'l' in "Hello" (line 0)

    // Move to position (0, 3) - line 3 (0-indexed = 2), column 0
    harness.execute_action(Action::MoveTo(0, 3)).await.unwrap();
    harness.assert_cursor_at(0, 2); // At 'T' in "Test" (line 2)
}

#[tokio::test]
async fn test_percent_matches_next_bracket_on_line() {
    let mut harness = EditorHarness::with_content("if (a == (b * c) / d)");

    harness
        .execute_action(Action::MatchitForward)
        .await
        .unwrap();
    harness.assert_cursor_at(20, 0);

    harness
        .execute_action(Action::MatchitForward)
        .await
        .unwrap();
    harness.assert_cursor_at(3, 0);
}

#[tokio::test]
async fn test_percent_matches_nested_bracket_under_cursor() {
    let mut harness = EditorHarness::with_content("if (a == (b * c) / d)");

    harness.execute_action(Action::MoveTo(9, 1)).await.unwrap();
    harness
        .execute_action(Action::MatchitForward)
        .await
        .unwrap();
    harness.assert_cursor_at(15, 0);
}

#[tokio::test]
async fn test_counted_percent_jumps_to_file_percentage() {
    let content = (1..=100)
        .map(|line| format!("Line {line:03}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut harness = EditorHarness::with_content(&content);

    type_normal_keys(&mut harness, "50%").await;
    harness.assert_cursor_at(0, 49);
}

#[tokio::test]
async fn test_percent_matches_c_comment_delimiters() {
    let mut harness = EditorHarness::with_content("alpha /* beta */ gamma");

    harness.execute_action(Action::MoveTo(6, 1)).await.unwrap();
    harness
        .execute_action(Action::MatchitForward)
        .await
        .unwrap();
    harness.assert_cursor_at(14, 0);

    harness
        .execute_action(Action::MatchitBackward)
        .await
        .unwrap();
    harness.assert_cursor_at(6, 0);
}

#[tokio::test]
async fn test_percent_cycles_preprocessor_groups_linewise() {
    let mut harness = EditorHarness::with_content("#if FOO\nbody\n#else\nother\n#endif");

    harness
        .execute_action(Action::MatchitForward)
        .await
        .unwrap();
    harness.assert_cursor_at(0, 2);

    harness
        .execute_action(Action::MatchitForward)
        .await
        .unwrap();
    harness.assert_cursor_at(0, 4);

    harness
        .execute_action(Action::MatchitForward)
        .await
        .unwrap();
    harness.assert_cursor_at(0, 2);
}

#[tokio::test]
async fn test_percent_cycles_bash_matchit_groups() {
    let buffer = Buffer::new(
        Some("script.sh".to_string()),
        "if foo\nthen\n  echo yes\nelse\n  echo no\nfi".to_string(),
    );
    let mut harness = EditorHarness::with_config(buffer, Config::default());

    harness
        .execute_action(Action::MatchitForward)
        .await
        .unwrap();
    harness.assert_cursor_at(0, 3);

    harness
        .execute_action(Action::MatchitForward)
        .await
        .unwrap();
    harness.assert_cursor_at(0, 5);
}

#[tokio::test]
async fn test_percent_matches_html_like_tags() {
    let mut harness = EditorHarness::with_content("<section><div>hello</div></section>");

    harness.execute_action(Action::MoveTo(9, 1)).await.unwrap();
    harness
        .execute_action(Action::MatchitForward)
        .await
        .unwrap();
    harness.assert_cursor_at(19, 0);
}

#[tokio::test]
async fn test_operator_delete_percent_deletes_through_match() {
    let mut harness = EditorHarness::with_content("(alpha) beta");

    type_normal_keys(&mut harness, "d%").await;
    harness.assert_buffer_contents(" beta");
}
