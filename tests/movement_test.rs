use red::{Action, Editor};

#[tokio::test]
async fn test_move_right() {
    let state = r#"
    |fn main() {
        println!("Hello, world!");
    }
    "#;

    let mut editor = Editor::builder().state(state).build().unwrap();
    editor.send_action(&Action::MoveRight).await.unwrap();
    assert_eq!(editor.cursor_pos(), (1, 0));
}

#[tokio::test]
async fn test_move_right_line_edge_doesnt_move() {
    let state = r#"
    fn main() {
        println!("Hello, world!")|;
    }
    "#;

    let mut editor = Editor::builder().state(state).build().unwrap();
    assert_eq!(editor.cursor_pos(), (29, 1));
    editor.send_action(&Action::MoveRight).await.unwrap();
    assert_eq!(editor.cursor_pos(), (29, 1));
}

#[tokio::test]
async fn test_move_left() {
    let state = r#"
    |fn main() {
        println!("Hello, world!");
    }
    "#;

    let mut editor = Editor::builder().state(state).build().unwrap();
    editor.send_action(&Action::MoveRight).await.unwrap();
    editor.send_action(&Action::MoveLeft).await.unwrap();
    assert_eq!(editor.cursor_pos(), (0, 0));
}

#[tokio::test]
async fn test_move_left_line_edge_doesnt_move() {
    let state = r#"
    fn main() {
    |    println!("Hello, world!");
    }
    "#;

    let mut editor = Editor::builder().state(state).build().unwrap();
    assert_eq!(editor.cursor_pos(), (0, 1));
    editor.send_action(&Action::MoveLeft).await.unwrap();
    assert_eq!(editor.cursor_pos(), (0, 1));
}

#[tokio::test]
async fn test_move_next_word() {
    let state = r#"
    |fn main() {
        println!("Hello, world!");
    }
    "#;

    let mut editor = Editor::builder().state(state).build().unwrap();
    assert_eq!(editor.cursor_pos(), (0, 0));
    editor.send_action(&Action::MoveToNextWord).await.unwrap();
    assert_eq!(editor.cursor_pos(), (3, 0));
}

#[tokio::test]
async fn test_move_next_word_next_line() {
    let state = r#"
    fn ma|in() {
        println!("Hello, world!");
    }
    "#;

    let mut editor = Editor::builder().state(state).build().unwrap();
    assert_eq!(editor.cursor_pos(), (5, 0));
    editor.send_action(&Action::MoveToNextWord).await.unwrap();
    assert_eq!(editor.cursor_pos(), (4, 1));
}

#[tokio::test]
async fn test_move_down_with_smaller_line() {
    let state = r#"
    fn main() {
        println!|("Hello, world!");
    }
    "#;

    let mut editor = Editor::builder().state(state).build().unwrap();
    editor.send_action(&Action::MoveDown).await.unwrap();
    assert_eq!(editor.cursor_pos(), (0, 2));
}

#[tokio::test]
async fn test_move_down_restore_prev_pos() {
    let state = r#"
    fn main() {
        println!|("Hello, world!");
    }

    fn foo() {
        println!("Hello, world!");
    }
    "#;

    let mut editor = Editor::builder().state(state).build().unwrap();
    assert_eq!(editor.cursor_pos(), (12, 1));
    editor.send_action(&Action::MoveDown).await.unwrap();
    assert_eq!(editor.cursor_pos(), (0, 2));
    editor.send_action(&Action::MoveDown).await.unwrap();
    editor.send_action(&Action::MoveDown).await.unwrap();
    editor.send_action(&Action::MoveDown).await.unwrap();
    assert_eq!(editor.cursor_pos(), (12, 5));
}
