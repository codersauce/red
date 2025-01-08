mod common;

use std::{fs, process::Command};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use red::{buffer::Buffer, config::Config, editor::Editor, lsp::LspClient, theme::Theme};

fn _test_insert() {
    let session_name = "test_session";

    // tmux send-keys -t test_session 'iHello, World!' Enter
    // tmux capture-pane -t test_session -p > output.txt
    // tmux kill-session -t test_session

    // Create a new session
    let status = Command::new("tmux")
        .arg("new-session")
        .arg("-d")
        .arg("-s")
        .arg(session_name)
        .arg("./target/debug/red /tmp/test.rs")
        .status()
        .unwrap();

    println!("status: {:?}", status);

    let output = Command::new("tmux")
        .arg("capture-pane")
        .arg("-t")
        .arg(session_name)
        .arg("-p")
        .output()
        .unwrap();

    println!("output: {:?}", output);

    let status = Command::new("tmux")
        .arg("send-keys")
        .arg("-t")
        .arg(session_name)
        .arg("iHello, World!")
        .arg("Enter")
        .status();

    println!("status: {:?}", status);

    let output = Command::new("tmux")
        .arg("capture-pane")
        .arg("-t")
        .arg(session_name)
        .arg("-p")
        .arg("-N")
        .arg("-e")
        .output()
        .unwrap();

    println!("output: {:?}", output);
    fs::write("output.txt", output.stdout).unwrap();

    let _status = Command::new("tmux")
        .arg("kill-session")
        .arg("-t")
        .arg(session_name)
        .status()
        .unwrap();
}

async fn _test_selection() {
    let buffer = Buffer::new(
        None,
        "aaaaaaaaaa\nbbbbbbbbbbb\n\nvvvvvvvvvv\nxxxxxxxxxxx".to_string(),
    );
    let lsp = common::mock_lsp() as Box<dyn LspClient>;
    let mut editor = Editor::new(lsp, Config::default(), Theme::default(), vec![buffer]).unwrap();

    let _event = Event::Key(KeyEvent {
        code: KeyCode::Char('v'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    });

    // TODO: find a way to inject events into the editor
    editor.run().await.unwrap();
}
