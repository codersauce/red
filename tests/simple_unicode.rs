mod common;

use common::EditorHarness;
use red::editor::{Action, Mode};

#[tokio::test]
async fn test_simple_emoji() {
    let mut h = EditorHarness::new();

    // Enter insert mode
    h.execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();

    // Insert one character at a time to debug
    println!("Initial cursor: {:?}", h.cursor_position());

    // Insert 'H'
    h.execute_action(Action::InsertCharAtCursorPos('H'))
        .await
        .unwrap();
    println!("After 'H': cursor = {:?}", h.cursor_position());

    // Insert 'i'
    h.execute_action(Action::InsertCharAtCursorPos('i'))
        .await
        .unwrap();
    println!("After 'i': cursor = {:?}", h.cursor_position());

    // Insert '👋'
    h.execute_action(Action::InsertCharAtCursorPos('👋'))
        .await
        .unwrap();
    println!("After '👋': cursor = {:?}", h.cursor_position());

    // Check buffer contents
    let contents = h.buffer_contents();
    println!("Buffer contents: {:?}", contents);
    assert_eq!(contents, "Hi👋\n");
}

#[tokio::test]
async fn test_move_through_emoji() {
    let mut h = EditorHarness::with_content("Hi👋!");

    // Start at beginning
    h.execute_action(Action::MoveToLineStart).await.unwrap();
    println!("Start position: {:?}", h.cursor_position());

    // Move right through each character
    h.execute_action(Action::MoveRight).await.unwrap();
    println!("After first move right: {:?}", h.cursor_position());

    h.execute_action(Action::MoveRight).await.unwrap();
    println!("After second move right: {:?}", h.cursor_position());

    h.execute_action(Action::MoveRight).await.unwrap();
    println!(
        "After third move right (past emoji): {:?}",
        h.cursor_position()
    );

    h.execute_action(Action::MoveRight).await.unwrap();
    println!("After fourth move right: {:?}", h.cursor_position());
}
