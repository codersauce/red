mod common;

use common::EditorHarness;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use red::{
    buffer::Buffer,
    config::{Config, KeyAction},
    editor::{Action, Mode, SearchDirection},
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
    assert_eq!(harness.render_cursor_position(), Some((4, 1)));
}

#[tokio::test]
async fn test_line_end_goal_renders_on_final_wide_grapheme_cell() {
    let mut harness = EditorHarness::with_content("x\na你");

    harness.execute_action(Action::MoveToLineEnd).await.unwrap();
    harness.execute_action(Action::MoveDown).await.unwrap();

    harness.assert_cursor_at(1, 1);
    assert_eq!(harness.render_cursor_position(), Some((5, 1)));
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
