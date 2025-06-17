mod common;

use common::EditorHarness;
use red::editor::{Action, Mode};

#[tokio::test]
async fn test_insert_line_below_with_emoji() {
    // Create a buffer with emoji content
    let mut harness = EditorHarness::with_content("💕 hearts");

    // Try to open a line below with 'o' - InsertLineBelowCursor
    harness
        .execute_action(Action::InsertLineBelowCursor)
        .await
        .unwrap();

    // Should have created a new line and moved cursor there
    harness.assert_mode(Mode::Insert);
    harness.assert_cursor_at(0, 1);

    // Type on the new line
    harness.type_text("test").await.unwrap();
    harness.assert_buffer_contents("💕 hearts\ntest");
}

#[tokio::test]
async fn test_cursor_movement_with_emoji() {
    // Create a buffer with emoji content
    let mut harness = EditorHarness::with_content("💕 hearts");

    // Move cursor to the right multiple times
    for _ in 0..9 {
        harness.execute_action(Action::MoveRight).await.unwrap();
    }

    // Should be at the end of the line
    let (x, _) = harness.cursor_position();
    assert_eq!(x, 8); // "💕 hearts" has 8 characters
}

#[tokio::test]
async fn test_delete_char_with_emoji() {
    // Create a buffer with emoji content
    let mut harness = EditorHarness::with_content("💕 hearts");

    // Delete the emoji
    harness
        .execute_action(Action::DeleteCharAtCursorPos)
        .await
        .unwrap();

    harness.assert_buffer_contents(" hearts");
}

#[tokio::test]
async fn test_insert_at_end_of_emoji_line() {
    // Create a buffer with emoji content
    let mut harness = EditorHarness::with_content("💕 hearts");

    // Move to end of line
    harness.execute_action(Action::MoveToLineEnd).await.unwrap();

    // Enter insert mode
    harness
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();

    // Type some text
    harness.type_text(" emoji").await.unwrap();
    harness.assert_buffer_contents("💕 hearts emoji");
}

#[tokio::test]
async fn test_multi_emoji_handling() {
    // Create a buffer with multiple emojis
    let mut harness = EditorHarness::with_content("👨‍👩‍👧‍👦 family 💕 hearts 🌟 star");

    // Move cursor across the line
    for _ in 0..20 {
        harness.execute_action(Action::MoveRight).await.unwrap();
    }

    // Test inserting line below
    harness
        .execute_action(Action::InsertLineBelowCursor)
        .await
        .unwrap();

    harness.type_text("new line").await.unwrap();
    harness.assert_buffer_contents("👨‍👩‍👧‍👦 family 💕 hearts 🌟 star\nnew line");
}
