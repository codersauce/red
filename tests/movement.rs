mod common;

use common::EditorHarness;
use red::editor::Action;

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
    harness.assert_cursor_at(13, 0); // "Hello, World!" is 13 chars, cursor after last char

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
    // MoveToLineEnd on "def" puts us at position 3
    harness.assert_cursor_at(3, 1); // After 'f' in "def"

    // Try to move right at end of line
    harness.execute_action(Action::MoveRight).await.unwrap();
    harness.assert_cursor_at(3, 1); // Should stay at position 3

    // Try to move down at end of buffer (already at last line)
    harness.execute_action(Action::MoveDown).await.unwrap();
    harness.assert_cursor_at(3, 1); // Should stay at line 1
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
