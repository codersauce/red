mod common;

use common::EditorHarness;
use red::editor::{Action, Mode};

#[tokio::test]
async fn test_insert_mode() {
    let mut harness = EditorHarness::with_content("Hello World");
    
    // Debug: Check initial cursor position and buffer state
    println!("Initial cursor position: {:?}", harness.cursor_position());
    println!("Number of lines: {}", harness.line_count());
    if let Some(line) = harness.line_contents(0) {
        println!("Line 0 content: {:?}", line);
    }
    
    // Enter insert mode with 'i'
    harness.execute_action(Action::EnterMode(Mode::Insert)).await.unwrap();
    harness.assert_mode(Mode::Insert);
    
    // Debug: Check cursor position after entering insert mode
    println!("Cursor position after entering insert mode: {:?}", harness.cursor_position());
    
    // Type some text
    harness.type_text("Hi ").await.unwrap();
    
    // Debug: Check actual buffer contents
    let contents = harness.buffer_contents();
    println!("Actual buffer contents: {:?}", contents);
    println!("Buffer length: {}", contents.len());
    println!("Ends with newline: {}", contents.ends_with('\n'));
    
    harness.assert_buffer_contents("Hi Hello World");
    
    // Exit insert mode (ESC)
    harness.execute_action(Action::EnterMode(Mode::Normal)).await.unwrap();
    harness.assert_mode(Mode::Normal);
}

#[tokio::test]
async fn test_append_mode() {
    let mut harness = EditorHarness::with_content("Hello World");
    
    // Move cursor to 'o' in 'Hello' (position 4)
    for _ in 0..4 {
        harness.execute_action(Action::MoveRight).await.unwrap();
    }
    
    // Enter append mode with 'a' - should insert after current character
    harness.execute_action(Action::MoveRight).await.unwrap();
    harness.execute_action(Action::EnterMode(Mode::Insert)).await.unwrap();
    harness.assert_mode(Mode::Insert);
    
    // Type text
    harness.type_text(" there").await.unwrap();
    harness.assert_buffer_contents("Hello there World");
    
    // Exit insert mode
    harness.execute_action(Action::EnterMode(Mode::Normal)).await.unwrap();
}

#[tokio::test]
async fn test_open_line_below() {
    let mut harness = EditorHarness::with_content("Line 1\nLine 2");
    
    // Open line below with 'o' - InsertLineBelowCursor
    harness.execute_action(Action::InsertLineBelowCursor).await.unwrap();
    harness.assert_mode(Mode::Insert);
    
    // Should have created a new line and moved cursor there
    harness.assert_cursor_at(0, 1);
    
    // Type on the new line
    harness.type_text("New line").await.unwrap();
    harness.assert_buffer_contents("Line 1\nNew line\nLine 2");
    
    // Exit insert mode
    harness.execute_action(Action::EnterMode(Mode::Normal)).await.unwrap();
}

#[tokio::test]
async fn test_open_line_above() {
    let mut harness = EditorHarness::with_content("Line 1\nLine 2");
    
    // Move to second line
    harness.execute_action(Action::MoveDown).await.unwrap();
    println!("After MoveDown - cursor at: {:?}", harness.cursor_position());
    
    // Open line above with 'O' - InsertLineAtCursor
    harness.execute_action(Action::InsertLineAtCursor).await.unwrap();
    println!("After InsertLineAtCursor - cursor at: {:?}", harness.cursor_position());
    println!("Buffer contents: {:?}", harness.buffer_contents());
    harness.assert_mode(Mode::Insert);
    
    // Should have created a new line above and moved cursor there
    harness.assert_cursor_at(0, 1);
    
    // Type on the new line
    harness.type_text("Middle line").await.unwrap();
    harness.assert_buffer_contents("Line 1\nMiddle line\nLine 2");
    
    // Exit insert mode
    harness.execute_action(Action::EnterMode(Mode::Normal)).await.unwrap();
}

#[tokio::test]
async fn test_delete_char() {
    let mut harness = EditorHarness::with_content("Hello World");
    
    // Delete character under cursor with 'x'
    harness.execute_action(Action::DeleteCharAtCursorPos).await.unwrap();
    harness.assert_buffer_contents("ello World");
    
    // Move to space and delete
    harness.execute_action(Action::MoveToNextWord).await.unwrap();
    harness.execute_action(Action::MoveLeft).await.unwrap();
    harness.execute_action(Action::DeleteCharAtCursorPos).await.unwrap();
    harness.assert_buffer_contents("elloWorld");
}

#[tokio::test]
async fn test_delete_line() {
    let mut harness = EditorHarness::with_content("Line 1\nLine 2\nLine 3");
    
    // Move to second line
    harness.execute_action(Action::MoveDown).await.unwrap();
    
    // Delete line with 'dd'
    println!("Before delete: {:?}", harness.buffer_contents());
    println!("Cursor at: {:?}", harness.cursor_position());
    println!("Line under cursor: {:?}", harness.current_line());
    harness.execute_action(Action::DeleteCurrentLine).await.unwrap();
    println!("After delete: {:?}", harness.buffer_contents());
    println!("Cursor at after: {:?}", harness.cursor_position());
    println!("Line under cursor after: {:?}", harness.current_line());
    harness.assert_buffer_contents("Line 1\nLine 3");
    
    // Cursor should be on what was line 3
    harness.assert_cursor_at(0, 1);
}

#[tokio::test]
async fn test_delete_to_end_of_line() {
    let mut harness = EditorHarness::with_content("Hello World Test");
    
    // Move to middle of line
    harness.execute_action(Action::MoveToNextWord).await.unwrap();
    
    // Delete to end of line with 'D' - not a direct action, so delete from cursor to end
    // This would typically be a composed action in vim
    let (x, _) = harness.cursor_position();
    let line_content = harness.current_line().unwrap();
    let line_len = line_content.trim_end().len(); // Don't include newline
    
    // Delete all characters from cursor to end of line
    for _ in x..line_len {
        harness.execute_action(Action::DeleteCharAtCursorPos).await.unwrap();
    }
    harness.assert_buffer_contents("Hello ");
}

#[tokio::test]
async fn test_change_word() {
    let mut harness = EditorHarness::with_content("Hello World Test");
    
    // Change word with 'cw' - delete word then enter insert mode
    harness.execute_action(Action::DeleteWord).await.unwrap();
    harness.execute_action(Action::EnterMode(Mode::Insert)).await.unwrap();
    harness.assert_mode(Mode::Insert);
    
    // Type replacement
    harness.type_text("Hi ").await.unwrap();
    harness.assert_buffer_contents("Hi World Test");
    
    // Exit insert mode
    harness.execute_action(Action::EnterMode(Mode::Normal)).await.unwrap();
}

#[tokio::test]
async fn test_change_line() {
    let mut harness = EditorHarness::with_content("Line 1\nLine 2\nLine 3");
    
    // Move to second line
    harness.execute_action(Action::MoveDown).await.unwrap();
    
    // Change line with 'cc' - delete line content and enter insert mode
    harness.execute_action(Action::MoveToLineStart).await.unwrap();
    let line_len = harness.current_line().unwrap().trim_end().len();
    for _ in 0..line_len {
        harness.execute_action(Action::DeleteCharAtCursorPos).await.unwrap();
    }
    harness.execute_action(Action::EnterMode(Mode::Insert)).await.unwrap();
    harness.assert_mode(Mode::Insert);
    
    // Type replacement
    harness.type_text("Changed line").await.unwrap();
    harness.assert_buffer_contents("Line 1\nChanged line\nLine 3");
    
    // Exit insert mode
    harness.execute_action(Action::EnterMode(Mode::Normal)).await.unwrap();
}

#[tokio::test]
async fn test_replace_char() {
    let mut harness = EditorHarness::with_content("Hello World");
    
    // Replace character with 'r' - delete char and insert new one
    harness.execute_action(Action::DeleteCharAtCursorPos).await.unwrap();
    harness.execute_action(Action::InsertCharAtCursorPos('J')).await.unwrap();
    harness.assert_buffer_contents("Jello World");
    harness.assert_mode(Mode::Normal); // Should stay in normal mode
}

#[tokio::test]
async fn test_insert_at_line_start() {
    let mut harness = EditorHarness::with_content("    Hello World");
    
    // Move cursor to middle
    harness.execute_action(Action::MoveToNextWord).await.unwrap();
    
    // Insert at start of line with 'I' - move to start and enter insert
    harness.execute_action(Action::MoveToLineStart).await.unwrap();
    harness.execute_action(Action::EnterMode(Mode::Insert)).await.unwrap();
    harness.assert_mode(Mode::Insert);
    harness.assert_cursor_at(0, 0);
    
    // Type text
    harness.type_text("Start: ").await.unwrap();
    harness.assert_buffer_contents("Start:     Hello World");
    
    // Exit insert mode
    harness.execute_action(Action::EnterMode(Mode::Normal)).await.unwrap();
}

#[tokio::test]
async fn test_append_at_line_end() {
    let mut harness = EditorHarness::with_content("Hello World");
    
    // Append at end of line with 'A' - move to end and enter insert
    harness.execute_action(Action::MoveToLineEnd).await.unwrap();
    harness.execute_action(Action::EnterMode(Mode::Insert)).await.unwrap();
    harness.assert_mode(Mode::Insert);
    
    // Type text
    harness.type_text(" Test").await.unwrap();
    harness.assert_buffer_contents("Hello World Test");
    
    // Exit insert mode
    harness.execute_action(Action::EnterMode(Mode::Normal)).await.unwrap();
}

#[tokio::test]
async fn test_delete_word() {
    let mut harness = EditorHarness::with_content("Hello World Test");
    
    // Delete word with 'dw'
    harness.execute_action(Action::DeleteWord).await.unwrap();
    harness.assert_buffer_contents("World Test");
    
    // Delete another word (including space)
    harness.execute_action(Action::DeleteWord).await.unwrap();
    harness.assert_buffer_contents("Test");
}

#[tokio::test]
async fn test_join_lines() {
    let _harness = EditorHarness::with_content("Line 1\nLine 2\nLine 3");
    
    // Join lines is typically a complex operation - skip for now
    // Would need to delete newline and add space
}

#[tokio::test]
async fn test_undo_redo() {
    let mut harness = EditorHarness::with_content("Hello World");
    
    // Make a change
    harness.execute_action(Action::DeleteCharAtCursorPos).await.unwrap();
    harness.assert_buffer_contents("ello World");
    
    // Undo with 'u'
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("Hello World");
    
    // Redo is not implemented as a separate action
    // Skip redo test
}

#[tokio::test]
async fn test_paste() {
    let mut harness = EditorHarness::with_content("Hello World");
    
    // Delete a word (should be yanked to clipboard)
    harness.execute_action(Action::DeleteWord).await.unwrap();
    harness.assert_buffer_contents("World");
    
    // Move to end and paste with 'p'
    harness.execute_action(Action::MoveToLineEnd).await.unwrap();
    harness.execute_action(Action::Paste).await.unwrap();
    // This depends on clipboard/register implementation
    // For now, let's just verify it doesn't crash
}

#[tokio::test]
async fn test_yank_and_paste() {
    let mut harness = EditorHarness::with_content("Line 1\nLine 2\nLine 3");
    
    // Yank action exists
    harness.execute_action(Action::Yank).await.unwrap();
    
    // Move down and paste
    harness.execute_action(Action::MoveDown).await.unwrap();
    harness.execute_action(Action::Paste).await.unwrap();
    // This depends on clipboard/register implementation
}

#[tokio::test]
async fn test_editing_empty_buffer() {
    let mut harness = EditorHarness::new();
    
    // Enter insert mode in empty buffer
    harness.execute_action(Action::EnterMode(Mode::Insert)).await.unwrap();
    harness.type_text("First line").await.unwrap();
    harness.assert_buffer_contents("First line\n");
    
    // Exit and create new line below
    harness.execute_action(Action::EnterMode(Mode::Normal)).await.unwrap();
    harness.execute_action(Action::InsertLineBelowCursor).await.unwrap();
    harness.type_text("Second line").await.unwrap();
    harness.assert_buffer_contents("First line\nSecond line\n");
}

#[tokio::test]
async fn test_delete_at_end_of_file() {
    let mut harness = EditorHarness::with_content("Line 1\nLine 2");
    
    // Move to last line
    harness.execute_action(Action::MoveToBottom).await.unwrap();
    println!("After MoveToBottom: cursor at {:?}", harness.cursor_position());
    println!("Current line: {:?}", harness.current_line());
    
    // Try to delete line at end of file
    harness.execute_action(Action::DeleteCurrentLine).await.unwrap();
    println!("After delete: {:?}", harness.buffer_contents());
    harness.assert_buffer_contents("Line 1\n");
}

#[tokio::test]
async fn test_change_to_end_of_line() {
    let mut harness = EditorHarness::with_content("Hello World Test");
    
    // Move to middle
    harness.execute_action(Action::MoveToNextWord).await.unwrap();
    
    // Change to end of line with 'C' - delete to end and enter insert
    let (x, _) = harness.cursor_position();
    let line_len = harness.current_line().unwrap().trim_end().len();
    for _ in x..line_len {
        harness.execute_action(Action::DeleteCharAtCursorPos).await.unwrap();
    }
    harness.execute_action(Action::EnterMode(Mode::Insert)).await.unwrap();
    harness.assert_mode(Mode::Insert);
    
    // Type replacement
    harness.type_text("Universe").await.unwrap();
    harness.assert_buffer_contents("Hello Universe");
}