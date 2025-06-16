mod common;

use common::EditorHarness;
use red::editor::{Action, Mode};
use red::test_utils::EditorTestExt;

#[tokio::test]
async fn test_emoji_cursor_movement() {
    let mut h = EditorHarness::new();

    // Insert emoji and text
    h.editor.test_type_text("Hello 👋 world").await.unwrap();
    h.execute_action(Action::MoveToLineStart).await.unwrap();

    // Move right through "Hello "
    for _ in 0..6 {
        h.execute_action(Action::MoveRight).await.unwrap();
    }

    // Cursor should be before the emoji
    let (x, _) = h.cursor_position();
    assert_eq!(x, 6, "Cursor should be at position 6 before emoji");

    // Move right once - should skip the entire emoji
    h.execute_action(Action::MoveRight).await.unwrap();
    let (x, _) = h.cursor_position();
    assert_eq!(x, 7, "Cursor should be at position 7 after emoji");

    // Move left once - should go back before the emoji
    h.execute_action(Action::MoveLeft).await.unwrap();
    let (x, _) = h.cursor_position();
    assert_eq!(x, 6, "Cursor should be back at position 6");
}

#[tokio::test]
async fn test_cjk_characters() {
    let mut h = EditorHarness::new();

    // Insert CJK characters
    h.editor.test_type_text("你好世界").await.unwrap();
    h.execute_action(Action::MoveToLineStart).await.unwrap();

    // Each CJK character should be treated as one unit for cursor movement
    h.execute_action(Action::MoveRight).await.unwrap();
    let (x, _) = h.cursor_position();
    assert_eq!(x, 1, "Cursor should be at character position 1");

    h.execute_action(Action::MoveRight).await.unwrap();
    let (x, _) = h.cursor_position();
    assert_eq!(x, 2, "Cursor should be at character position 2");
}

#[tokio::test]
async fn test_combining_characters() {
    let mut h = EditorHarness::new();

    // Insert text with combining characters (é as e + combining acute)
    h.editor.test_type_text("café").await.unwrap(); // This uses the combined form
    h.execute_action(Action::MoveToLineStart).await.unwrap();

    // Move through the word
    for i in 0..4 {
        let (x, _) = h.cursor_position();
        assert_eq!(x, i, "Cursor should be at position {}", i);
        h.execute_action(Action::MoveRight).await.unwrap();
    }

    // Should be at the end
    let (x, _) = h.cursor_position();
    assert_eq!(x, 4, "Cursor should be at the end");
}

#[tokio::test]
async fn test_mixed_width_characters() {
    let mut h = EditorHarness::new();

    // Insert mixed ASCII, emoji, and CJK
    h.editor.test_type_text("Hi👋你好!").await.unwrap();
    h.execute_action(Action::MoveToLineStart).await.unwrap();

    // Expected positions after each move right:
    // Start: 0
    // After 'H': 1
    // After 'i': 2
    // After '👋': 3 (emoji is one character)
    // After '你': 4
    // After '好': 5
    // After '!': 6

    let expected_positions = vec![0, 1, 2, 3, 4, 5, 6];

    for expected in expected_positions {
        let (x, _) = h.cursor_position();
        assert_eq!(x, expected, "Cursor should be at position {}", expected);
        if expected < 6 {
            h.execute_action(Action::MoveRight).await.unwrap();
        }
    }
}

#[tokio::test]
async fn test_delete_emoji() {
    let mut h = EditorHarness::new();

    // Insert text with emoji
    h.editor.test_type_text("Hello👋World").await.unwrap();
    h.execute_action(Action::MoveToLineStart).await.unwrap();

    // Move to position before emoji
    for _ in 0..5 {
        h.execute_action(Action::MoveRight).await.unwrap();
    }

    // Delete the emoji
    h.execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();

    // Check that the whole emoji was deleted
    let line = h.current_line().unwrap();
    assert_eq!(line, "HelloWorld\n", "Emoji should be completely deleted");
}

#[tokio::test]
async fn test_insert_at_emoji_boundary() {
    let mut h = EditorHarness::new();

    // Insert text with emoji
    h.editor.test_type_text("👋Hello").await.unwrap();
    h.execute_action(Action::MoveToLineStart).await.unwrap();

    // Move cursor after emoji
    h.execute_action(Action::MoveRight).await.unwrap();

    // Insert text
    h.execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    h.editor.test_type_text(" ").await.unwrap();

    // Check result
    let line = h.current_line().unwrap();
    assert_eq!(line, "👋 Hello\n", "Space should be inserted after emoji");
}

#[tokio::test]
async fn test_family_emoji_zwj_sequence() {
    let mut h = EditorHarness::new();

    // Insert family emoji (uses zero-width joiners)
    h.editor.test_type_text("👨‍👩‍👧‍👦").await.unwrap();
    h.execute_action(Action::MoveToLineStart).await.unwrap();

    // This complex emoji should be treated as one unit
    h.execute_action(Action::MoveRight).await.unwrap();
    let (x, _) = h.cursor_position();
    assert_eq!(x, 1, "Complex emoji should move as one unit");

    // Delete should remove the entire emoji
    h.execute_action(Action::DeletePreviousChar).await.unwrap();
    let line = h.current_line().unwrap();
    assert_eq!(line, "\n", "Entire emoji sequence should be deleted");
}

#[tokio::test]
async fn test_flag_emoji() {
    let mut h = EditorHarness::new();

    // Insert flag emojis (regional indicator sequences)
    h.editor.test_type_text("🇺🇸🇯🇵").await.unwrap();
    h.execute_action(Action::MoveToLineStart).await.unwrap();

    // Each flag should be one unit
    h.execute_action(Action::MoveRight).await.unwrap();
    let (x, _) = h.cursor_position();
    assert_eq!(x, 1, "First flag should be one unit");

    h.execute_action(Action::MoveRight).await.unwrap();
    let (x, _) = h.cursor_position();
    assert_eq!(x, 2, "Second flag should be one unit");
}

#[tokio::test]
async fn test_word_navigation_with_unicode() {
    let mut h = EditorHarness::new();

    // Insert text with various Unicode characters
    h.editor
        .test_type_text("hello 世界 emoji👋test")
        .await
        .unwrap();
    h.execute_action(Action::MoveToLineStart).await.unwrap();

    // Move to next word - should go to 世
    h.execute_action(Action::MoveToNextWord).await.unwrap();
    let (x, _) = h.cursor_position();
    assert_eq!(x, 6, "Should be at start of CJK word");

    // Move to next word - should go to emoji
    h.execute_action(Action::MoveToNextWord).await.unwrap();
    let (x, _) = h.cursor_position();
    assert_eq!(x, 9, "Should be at start of emoji word");

    // Move to next word - should go to emoji (it's treated as a separate word)
    h.execute_action(Action::MoveToNextWord).await.unwrap();
    let (x, _) = h.cursor_position();
    assert_eq!(x, 14, "Should be at emoji");
}
