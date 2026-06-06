mod common;

use common::{EditorHarness, MockLsp};
use red::{
    buffer::Buffer,
    config::Config,
    editor::{Action, Content, Editor, Mode},
    lsp::LspClient,
    plugin::{PanelConfig, PanelSide},
    theme::Theme,
};
use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

fn temp_file_path(name: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir()
        .join(format!("red-{name}-{}-{nanos}.txt", std::process::id()))
        .to_string_lossy()
        .into_owned()
}

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
    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.assert_mode(Mode::Insert);

    // Debug: Check cursor position after entering insert mode
    println!(
        "Cursor position after entering insert mode: {:?}",
        harness.cursor_position()
    );

    // Type some text
    harness.type_text("Hi ").await.unwrap();

    // Debug: Check actual buffer contents
    let contents = harness.buffer_contents();
    println!("Actual buffer contents: {:?}", contents);
    println!("Buffer length: {}", contents.len());
    println!("Ends with newline: {}", contents.ends_with('\n'));

    harness.assert_buffer_contents("Hi Hello World");

    // Exit insert mode (ESC)
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
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
    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.assert_mode(Mode::Insert);

    // Type text
    harness.type_text(" there").await.unwrap();
    harness.assert_buffer_contents("Hello there World");

    // Exit insert mode
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
}

#[tokio::test]
async fn test_open_line_below() {
    let mut harness = EditorHarness::with_content("Line 1\nLine 2");

    // Open line below with 'o' - InsertLineBelowCursor
    harness
        .execute_action(Action::InsertLineBelowCursor)
        .await
        .unwrap();
    harness.assert_mode(Mode::Insert);

    // Should have created a new line and moved cursor there
    harness.assert_cursor_at(0, 1);

    // Type on the new line
    harness.type_text("New line").await.unwrap();
    harness.assert_buffer_contents("Line 1\nNew line\nLine 2");

    // Exit insert mode
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
}

#[tokio::test]
async fn test_open_line_above() {
    let mut harness = EditorHarness::with_content("Line 1\nLine 2");

    // Move to second line
    harness.execute_action(Action::MoveDown).await.unwrap();
    println!(
        "After MoveDown - cursor at: {:?}",
        harness.cursor_position()
    );

    // Open line above with 'O' - InsertLineAtCursor
    harness
        .execute_action(Action::InsertLineAtCursor)
        .await
        .unwrap();
    println!(
        "After InsertLineAtCursor - cursor at: {:?}",
        harness.cursor_position()
    );
    println!("Buffer contents: {:?}", harness.buffer_contents());
    harness.assert_mode(Mode::Insert);

    // Should have created a new line above and moved cursor there
    harness.assert_cursor_at(0, 1);

    // Type on the new line
    harness.type_text("Middle line").await.unwrap();
    harness.assert_buffer_contents("Line 1\nMiddle line\nLine 2");

    // Exit insert mode
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
}

#[tokio::test]
async fn test_delete_char() {
    let mut harness = EditorHarness::with_content("Hello World");

    // Delete character under cursor with 'x'
    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    harness.assert_buffer_contents("ello World");

    // Move to space and delete
    harness
        .execute_action(Action::MoveToNextWord)
        .await
        .unwrap();
    harness.execute_action(Action::MoveLeft).await.unwrap();
    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
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
    harness
        .execute_action(Action::DeleteCurrentLine)
        .await
        .unwrap();
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
    harness
        .execute_action(Action::MoveToNextWord)
        .await
        .unwrap();

    // Delete to end of line with 'D' - not a direct action, so delete from cursor to end
    // This would typically be a composed action in vim
    let (x, _) = harness.cursor_position();
    let line_content = harness.current_line().unwrap();
    let line_len = line_content.trim_end().len(); // Don't include newline

    // Delete all characters from cursor to end of line
    for _ in x..line_len {
        harness
            .execute_action(Action::DeleteCharAtCursorPos)
            .await
            .unwrap();
    }
    harness.assert_buffer_contents("Hello ");
}

#[tokio::test]
async fn test_change_word() {
    let mut harness = EditorHarness::with_content("Hello World Test");

    // Change word with 'cw' - delete word then enter insert mode
    harness.execute_action(Action::DeleteWord).await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.assert_mode(Mode::Insert);

    // Type replacement
    harness.type_text("Hi ").await.unwrap();
    harness.assert_buffer_contents("Hi World Test");

    // Exit insert mode
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
}

#[tokio::test]
async fn test_change_line() {
    let mut harness = EditorHarness::with_content("Line 1\nLine 2\nLine 3");

    // Move to second line
    harness.execute_action(Action::MoveDown).await.unwrap();

    // Change line with 'cc' - delete line content and enter insert mode
    harness
        .execute_action(Action::MoveToLineStart)
        .await
        .unwrap();
    let line_len = harness.current_line().unwrap().trim_end().len();
    for _ in 0..line_len {
        harness
            .execute_action(Action::DeleteCharAtCursorPos)
            .await
            .unwrap();
    }
    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.assert_mode(Mode::Insert);

    // Type replacement
    harness.type_text("Changed line").await.unwrap();
    harness.assert_buffer_contents("Line 1\nChanged line\nLine 3");

    // Exit insert mode
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
}

#[tokio::test]
async fn test_replace_char() {
    let mut harness = EditorHarness::with_content("Hello World");

    // Replace character with 'r' - delete char and insert new one
    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    harness
        .execute_action(Action::InsertCharAtCursorPos('J'))
        .await
        .unwrap();
    harness.assert_buffer_contents("Jello World");
    harness.assert_mode(Mode::Normal); // Should stay in normal mode
}

#[tokio::test]
async fn test_insert_at_line_start() {
    let mut harness = EditorHarness::with_content("    Hello World");

    // Move cursor to middle
    harness
        .execute_action(Action::MoveToNextWord)
        .await
        .unwrap();

    // Insert at start of line with 'I' - move to start and enter insert
    harness
        .execute_action(Action::MoveToLineStart)
        .await
        .unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.assert_mode(Mode::Insert);
    harness.assert_cursor_at(0, 0);

    // Type text
    harness.type_text("Start: ").await.unwrap();
    harness.assert_buffer_contents("Start:     Hello World");

    // Exit insert mode
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
}

#[tokio::test]
async fn test_append_at_line_end() {
    let mut harness = EditorHarness::with_content("Hello World");

    // Append at end of line with 'A' - move to end and enter insert
    harness.execute_action(Action::MoveToLineEnd).await.unwrap();
    harness.execute_action(Action::MoveRight).await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.assert_mode(Mode::Insert);

    // Type text
    harness.type_text(" Test").await.unwrap();
    harness.assert_buffer_contents("Hello World Test");

    // Exit insert mode
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
}

#[tokio::test]
async fn test_escape_from_insert_clamps_to_last_line_character() {
    let mut harness = EditorHarness::with_content("Hello");

    harness.execute_action(Action::MoveToLineEnd).await.unwrap();
    harness.execute_action(Action::MoveRight).await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.assert_cursor_at(5, 0);

    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
    harness.assert_cursor_at(4, 0);
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
    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    harness.assert_buffer_contents("ello World");

    // Undo with 'u'
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("Hello World");

    harness.execute_action(Action::Redo).await.unwrap();
    harness.assert_buffer_contents("ello World");
}

#[tokio::test]
async fn test_undo_multi_character_insert_session() {
    let mut harness = EditorHarness::with_content("");

    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.type_text("hello").await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();

    harness.assert_buffer_contents("hello\n");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("\n");
    harness.execute_action(Action::Redo).await.unwrap();
    harness.assert_buffer_contents("hello\n");
}

#[tokio::test]
async fn test_undo_insert_backspace_session() {
    let mut harness = EditorHarness::with_content("");

    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.type_text("abc").await.unwrap();
    harness
        .execute_action(Action::DeletePreviousChar)
        .await
        .unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();

    harness.assert_buffer_contents("ab\n");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("\n");
    harness.execute_action(Action::Redo).await.unwrap();
    harness.assert_buffer_contents("ab\n");
}

#[tokio::test]
async fn test_backspace_at_line_start_joins_with_previous_line() {
    let mut harness = EditorHarness::with_content("abc\ndef");

    harness.execute_action(Action::MoveDown).await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness
        .execute_action(Action::DeletePreviousChar)
        .await
        .unwrap();

    harness.assert_buffer_contents("abcdef");
    harness.assert_cursor_at(3, 0);
}

#[tokio::test]
async fn test_undo_delete_range_and_word() {
    let mut harness = EditorHarness::with_content("hello world");

    harness.execute_action(Action::DeleteWord).await.unwrap();
    harness.assert_buffer_contents("world");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("hello world");

    harness
        .execute_action(Action::DeleteRange(0, 0, 5, 0))
        .await
        .unwrap();
    harness.assert_buffer_contents(" world");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("hello world");
}

#[tokio::test]
async fn test_undo_delete_current_line() {
    let mut harness = EditorHarness::with_content("one\ntwo\nthree");

    harness.execute_action(Action::MoveDown).await.unwrap();
    harness
        .execute_action(Action::DeleteCurrentLine)
        .await
        .unwrap();
    harness.assert_buffer_contents("one\nthree");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("one\ntwo\nthree");

    let mut harness = EditorHarness::with_content("single");
    harness
        .execute_action(Action::DeleteCurrentLine)
        .await
        .unwrap();
    harness.assert_buffer_contents("");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("single");
}

#[tokio::test]
async fn test_delete_current_line_yanks_for_linewise_paste_before() {
    let mut harness = EditorHarness::with_content("one\ntwo\nthree");

    harness.execute_action(Action::MoveDown).await.unwrap();
    harness
        .execute_action(Action::DeleteCurrentLine)
        .await
        .unwrap();
    harness.assert_buffer_contents("one\nthree");

    harness
        .execute_action(Action::MoveToLineStart)
        .await
        .unwrap();
    harness.execute_action(Action::PasteBefore).await.unwrap();
    harness.assert_buffer_contents("one\ntwo\nthree");
}

#[tokio::test]
async fn test_undo_multiline_insert_and_unicode() {
    let mut harness = EditorHarness::with_content("");

    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.type_text("a👋").await.unwrap();
    harness.execute_action(Action::InsertNewLine).await.unwrap();
    harness.type_text("é").await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();

    harness.assert_buffer_contents("a👋\né\n");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("\n");
    harness.execute_action(Action::Redo).await.unwrap();
    harness.assert_buffer_contents("a👋\né\n");
}

#[tokio::test]
async fn test_redo_stack_clears_after_new_edit() {
    let mut harness = EditorHarness::with_content("abc");

    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    harness.assert_buffer_contents("bc");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("abc");
    harness
        .execute_action(Action::InsertCharAtCursorPos('z'))
        .await
        .unwrap();
    harness.assert_buffer_contents("zabc");
    harness.execute_action(Action::Redo).await.unwrap();
    harness.assert_buffer_contents("zabc");
}

#[tokio::test]
async fn test_undo_does_not_create_new_undo_entries() {
    let mut harness = EditorHarness::with_content("abc");

    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    harness.execute_action(Action::Undo).await.unwrap();
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("abc");
}

#[tokio::test]
async fn test_undo_indent_and_unindent() {
    let mut harness = EditorHarness::with_content("line");

    harness.execute_action(Action::IndentLine).await.unwrap();
    harness.assert_buffer_contents("    line");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("line");

    harness.execute_action(Action::IndentLine).await.unwrap();
    harness.execute_action(Action::UnindentLine).await.unwrap();
    harness.assert_buffer_contents("line");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("    line");
}

#[tokio::test]
async fn test_undo_visual_char_line_and_block_delete() {
    let mut harness = EditorHarness::with_content("abcde");
    harness
        .execute_action(Action::EnterMode(Mode::Visual))
        .await
        .unwrap();
    harness.execute_action(Action::MoveRight).await.unwrap();
    harness.execute_action(Action::MoveRight).await.unwrap();
    harness.execute_action(Action::Delete).await.unwrap();
    harness.assert_buffer_contents("de");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("abcde");

    let mut harness = EditorHarness::with_content("one\ntwo\nthree");
    harness
        .execute_action(Action::EnterMode(Mode::VisualLine))
        .await
        .unwrap();
    harness.execute_action(Action::MoveDown).await.unwrap();
    harness.execute_action(Action::Delete).await.unwrap();
    harness.assert_buffer_contents("three");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("one\ntwo\nthree");

    let mut harness = EditorHarness::with_content("abc\ndef");
    harness
        .execute_action(Action::EnterMode(Mode::VisualBlock))
        .await
        .unwrap();
    harness.execute_action(Action::MoveRight).await.unwrap();
    harness.execute_action(Action::MoveDown).await.unwrap();
    harness.execute_action(Action::Delete).await.unwrap();
    harness.assert_buffer_contents("c\nf");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("abc\ndef");
}

#[tokio::test]
async fn test_visual_line_selection_uses_buffer_lines_after_scrolling() {
    let content = (0..40)
        .map(|line| format!("line-{line:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut harness = EditorHarness::with_content(&content);

    harness
        .execute_action(Action::SetCursor(0, 30))
        .await
        .unwrap();
    assert_eq!(harness.viewport_top(), 9);
    harness.assert_cursor_at(0, 30);

    harness
        .execute_action(Action::EnterMode(Mode::VisualLine))
        .await
        .unwrap();
    harness.execute_action(Action::MoveDown).await.unwrap();
    harness.execute_action(Action::Delete).await.unwrap();

    let remaining = harness.buffer_contents();
    assert!(
        !remaining.contains("line-30\nline-31"),
        "visual line delete should remove the scrolled-to buffer lines"
    );
    assert!(
        remaining.contains("line-21"),
        "visual line delete should not use viewport-relative rows as buffer lines"
    );
}

#[tokio::test]
async fn test_undo_paste_and_paste_before() {
    let mut harness = EditorHarness::with_content("hello world");

    harness
        .execute_action(Action::EnterMode(Mode::Visual))
        .await
        .unwrap();
    for _ in 0..5 {
        harness.execute_action(Action::MoveRight).await.unwrap();
    }
    harness.execute_action(Action::Delete).await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
    harness.assert_buffer_contents("world");
    harness.execute_action(Action::MoveToLineEnd).await.unwrap();
    harness.execute_action(Action::Paste).await.unwrap();
    harness.assert_buffer_contents("worldhello ");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("world");

    harness
        .execute_action(Action::MoveToLineStart)
        .await
        .unwrap();
    harness.execute_action(Action::PasteBefore).await.unwrap();
    harness.assert_buffer_contents("hello world");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("world");
}

#[tokio::test]
async fn test_undo_insert_text_action() {
    let mut harness = EditorHarness::with_content("abc");
    let content = Content::charwise("ZZ".to_string());

    harness
        .execute_action(Action::InsertText {
            x: 1,
            y: 0,
            content,
        })
        .await
        .unwrap();
    harness.assert_buffer_contents("aZZbc");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("abc");
}

#[tokio::test]
async fn test_undo_history_is_per_buffer() {
    let lsp = Box::new(MockLsp) as Box<dyn LspClient + Send>;
    let config = Config::default();
    let theme = Theme::default();
    let buffers = vec![
        Buffer::new(None, "one".to_string()),
        Buffer::new(None, "two".to_string()),
    ];
    let mut editor = Editor::with_size(lsp, 80, 24, config, theme, buffers).unwrap();
    editor.test_disable_terminal_output();
    let mut harness = EditorHarness { editor };

    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    harness.assert_buffer_contents("ne");
    harness.execute_action(Action::NextBuffer).await.unwrap();
    harness.assert_buffer_contents("two");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("two");
    harness
        .execute_action(Action::PreviousBuffer)
        .await
        .unwrap();
    harness.assert_buffer_contents("ne");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("one");
}

#[tokio::test]
async fn test_buffer_delete_removes_current_buffer_from_list() {
    let lsp = Box::new(MockLsp) as Box<dyn LspClient + Send>;
    let config = Config::default();
    let theme = Theme::default();
    let buffers = vec![
        Buffer::new(Some("one.rs".to_string()), "one".to_string()),
        Buffer::new(Some("two.rs".to_string()), "two".to_string()),
        Buffer::new(Some("three.rs".to_string()), "three".to_string()),
    ];
    let mut editor = Editor::with_size(lsp, 80, 24, config, theme, buffers).unwrap();
    editor.test_disable_terminal_output();
    let mut harness = EditorHarness { editor };

    harness.execute_action(Action::NextBuffer).await.unwrap();
    harness
        .execute_action(Action::Command("bd".to_string()))
        .await
        .unwrap();

    assert_eq!(harness.buffer_names(), vec!["one.rs", "three.rs"]);
    assert_eq!(harness.current_buffer_index(), 1);
    harness.assert_buffer_contents("three");
}

#[tokio::test]
async fn test_buffer_delete_requires_force_for_dirty_buffer() {
    let lsp = Box::new(MockLsp) as Box<dyn LspClient + Send>;
    let config = Config::default();
    let theme = Theme::default();
    let buffers = vec![
        Buffer::new(Some("one.rs".to_string()), "one".to_string()),
        Buffer::new(Some("two.rs".to_string()), "two".to_string()),
    ];
    let mut editor = Editor::with_size(lsp, 80, 24, config, theme, buffers).unwrap();
    editor.test_disable_terminal_output();
    let mut harness = EditorHarness { editor };

    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    harness
        .execute_action(Action::Command("bd".to_string()))
        .await
        .unwrap();

    assert_eq!(harness.buffer_names(), vec!["one.rs", "two.rs"]);
    assert_eq!(
        harness.last_error(),
        Some("No write since last change (add ! to override)")
    );
    harness.assert_buffer_contents("ne");

    harness
        .execute_action(Action::Command("bd!".to_string()))
        .await
        .unwrap();

    assert_eq!(harness.buffer_names(), vec!["two.rs"]);
    harness.assert_buffer_contents("two");
}

#[tokio::test]
async fn test_preview_theme_reports_missing_theme_without_changing_buffer() {
    let mut harness = EditorHarness::with_content("abc");

    harness
        .execute_action(Action::PreviewTheme(
            "definitely-missing-theme.json".to_string(),
        ))
        .await
        .unwrap();

    assert_eq!(
        harness.last_error(),
        Some("Theme file definitely-missing-theme.json not found")
    );
    harness.assert_buffer_contents("abc");
}

#[tokio::test]
async fn test_dirty_clears_when_undo_returns_to_clean_revision() {
    let mut harness = EditorHarness::with_content("abc");
    assert!(!harness.is_dirty());

    harness
        .execute_action(Action::InsertCharAtCursorPos('z'))
        .await
        .unwrap();
    assert!(harness.is_dirty());

    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("abc");
    assert!(!harness.is_dirty());

    harness.execute_action(Action::Redo).await.unwrap();
    harness.assert_buffer_contents("zabc");
    assert!(harness.is_dirty());
}

#[tokio::test]
async fn test_dirty_checkpoint_moves_after_save() {
    let path = temp_file_path("dirty-save");
    fs::write(&path, "abc").unwrap();

    let buffer = Buffer::new(Some(path.clone()), "abc".to_string());
    let mut harness = EditorHarness::with_buffer(buffer);

    harness
        .execute_action(Action::InsertCharAtCursorPos('z'))
        .await
        .unwrap();
    assert!(harness.is_dirty());
    harness.execute_action(Action::Save).await.unwrap();
    assert!(!harness.is_dirty());

    harness
        .execute_action(Action::InsertCharAtCursorPos('y'))
        .await
        .unwrap();
    assert!(harness.is_dirty());
    harness.execute_action(Action::Undo).await.unwrap();
    assert!(!harness.is_dirty());

    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn test_save_during_insert_keeps_saved_buffer_clean_on_escape() {
    let path = temp_file_path("dirty-save-insert");
    fs::write(&path, "abc").unwrap();

    let buffer = Buffer::new(Some(path.clone()), "abc".to_string());
    let mut harness = EditorHarness::with_buffer(buffer);

    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.type_text("z").await.unwrap();
    assert!(harness.is_dirty());

    harness.execute_action(Action::Save).await.unwrap();
    assert!(!harness.is_dirty());

    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
    harness.assert_buffer_contents("zabc");
    assert!(!harness.is_dirty());

    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn test_dirty_remains_after_undoing_past_saved_revision() {
    let path = temp_file_path("dirty-past-save");
    fs::write(&path, "abc").unwrap();

    let buffer = Buffer::new(Some(path.clone()), "abc".to_string());
    let mut harness = EditorHarness::with_buffer(buffer);

    harness
        .execute_action(Action::InsertCharAtCursorPos('z'))
        .await
        .unwrap();
    harness.execute_action(Action::Save).await.unwrap();
    assert!(!harness.is_dirty());

    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("abc");
    assert!(harness.is_dirty());

    let _ = fs::remove_file(path);
}

#[test]
fn test_right_panel_reserves_editor_window_width() {
    let mut harness = EditorHarness::with_content("abcdef");

    harness.editor.test_create_panel(
        "tree",
        PanelConfig {
            side: PanelSide::Right,
            width: 20,
            title: None,
        },
    );

    let (position, size) = harness.editor.test_active_window_bounds().unwrap();
    assert_eq!(position.x, 0);
    assert_eq!(size.0, 59);
}

#[tokio::test]
async fn test_dirty_isolated_per_buffer() {
    let lsp = Box::new(MockLsp) as Box<dyn LspClient + Send>;
    let config = Config::default();
    let theme = Theme::default();
    let buffers = vec![
        Buffer::new(None, "one".to_string()),
        Buffer::new(None, "two".to_string()),
    ];
    let mut editor = Editor::with_size(lsp, 80, 24, config, theme, buffers).unwrap();
    editor.test_disable_terminal_output();
    let mut harness = EditorHarness { editor };

    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    assert!(harness.is_dirty());

    harness.execute_action(Action::NextBuffer).await.unwrap();
    assert!(!harness.is_dirty());
    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();
    assert!(harness.is_dirty());
    harness.execute_action(Action::Undo).await.unwrap();
    assert!(!harness.is_dirty());

    harness
        .execute_action(Action::PreviousBuffer)
        .await
        .unwrap();
    assert!(harness.is_dirty());
    harness.execute_action(Action::Undo).await.unwrap();
    assert!(!harness.is_dirty());
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
async fn test_direct_open_line_below_groups_insert_undo() {
    let mut harness = EditorHarness::with_content("Line 1\nLine 2");

    harness
        .execute_action(Action::InsertLineBelowCursor)
        .await
        .unwrap();
    harness.type_text("New line").await.unwrap();
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();

    harness.assert_buffer_contents("Line 1\nNew line\nLine 2");
    harness.execute_action(Action::Undo).await.unwrap();
    harness.assert_buffer_contents("Line 1\nLine 2");
}

#[tokio::test]
async fn test_editing_empty_buffer() {
    let mut harness = EditorHarness::new();

    // Enter insert mode in empty buffer
    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.type_text("First line").await.unwrap();
    harness.assert_buffer_contents("First line\n");

    // Exit and create new line below
    harness
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
    harness
        .execute_action(Action::InsertLineBelowCursor)
        .await
        .unwrap();
    harness.type_text("Second line").await.unwrap();
    harness.assert_buffer_contents("First line\nSecond line\n");
}

#[tokio::test]
async fn test_delete_at_end_of_file() {
    let mut harness = EditorHarness::with_content("Line 1\nLine 2");

    // Move to last line
    harness.execute_action(Action::MoveToBottom).await.unwrap();
    println!(
        "After MoveToBottom: cursor at {:?}",
        harness.cursor_position()
    );
    println!("Current line: {:?}", harness.current_line());

    // Try to delete line at end of file
    harness
        .execute_action(Action::DeleteCurrentLine)
        .await
        .unwrap();
    println!("After delete: {:?}", harness.buffer_contents());
    harness.assert_buffer_contents("Line 1\n");
}

#[tokio::test]
async fn test_change_to_end_of_line() {
    let mut harness = EditorHarness::with_content("Hello World Test");

    // Move to middle
    harness
        .execute_action(Action::MoveToNextWord)
        .await
        .unwrap();

    // Change to end of line with 'C' - delete to end and enter insert
    let (x, _) = harness.cursor_position();
    let line_len = harness.current_line().unwrap().trim_end().len();
    for _ in x..line_len {
        harness
            .execute_action(Action::DeleteCharAtCursorPos)
            .await
            .unwrap();
    }
    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    harness.assert_mode(Mode::Insert);

    // Type replacement
    harness.type_text("Universe").await.unwrap();
    harness.assert_buffer_contents("Hello Universe");
}
