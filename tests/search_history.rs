mod common;

use common::{EditorHarness, MockLsp};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use red::{
    buffer::Buffer,
    clipboard::DisabledClipboardProvider,
    config::Config,
    editor::{Action, Editor, Mode},
    preferences::PreferencesStore,
    theme::Theme,
};

fn default_config() -> Config {
    toml::from_str(include_str!("../default_config.toml")).unwrap()
}

fn with_preferences(
    contents: &str,
    config: Config,
    preferences: PreferencesStore,
) -> EditorHarness {
    let mut editor = Editor::with_size_and_preferences(
        Box::new(MockLsp),
        /*width*/ 80,
        /*height*/ 24,
        config,
        Theme::default(),
        vec![Buffer::new(None, contents.to_string())],
        preferences,
    )
    .unwrap();
    editor.test_disable_terminal_output();
    editor.test_set_clipboard(Box::new(DisabledClipboardProvider));
    EditorHarness { editor }
}

fn with_history(contents: &str, patterns: &[&str]) -> EditorHarness {
    let mut preferences = PreferencesStore::in_memory();
    for pattern in patterns {
        preferences.record_search(pattern).unwrap();
    }
    with_preferences(contents, default_config(), preferences)
}

async fn key(harness: &mut EditorHarness, code: KeyCode) {
    harness
        .execute_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
        .await
        .unwrap();
}

async fn control(harness: &mut EditorHarness, character: char) {
    harness
        .execute_event(Event::Key(KeyEvent::new(
            KeyCode::Char(character),
            KeyModifiers::CONTROL,
        )))
        .await
        .unwrap();
}

async fn paste(harness: &mut EditorHarness, text: &str) {
    harness
        .execute_event(Event::Paste(text.to_string()))
        .await
        .unwrap();
}

async fn start(harness: &mut EditorHarness, prompt: char) {
    key(harness, KeyCode::Char(prompt)).await;
    harness.assert_mode(Mode::Search);
}

async fn submit(harness: &mut EditorHarness, prompt: char, pattern: &str) {
    start(harness, prompt).await;
    paste(harness, pattern).await;
    key(harness, KeyCode::Enter).await;
    harness.assert_mode(Mode::Normal);
}

#[tokio::test]
async fn search_history_is_shared_by_both_prompts_and_separate_from_commands() {
    let mut harness = with_history("alpha-one\nbeta-two\nalpha-three", &[]);
    submit(&mut harness, '/', "alpha-one").await;
    submit(&mut harness, '?', "beta-two").await;
    submit(&mut harness, '/', "alpha-three").await;
    harness
        .execute_action(Action::Command("write-history-only".to_string()))
        .await
        .unwrap();

    for prompt in ['/', '?'] {
        start(&mut harness, prompt).await;
        key(&mut harness, KeyCode::Down).await;
        assert_eq!(harness.commandline_text(), "");
        for expected in ["alpha-three", "beta-two", "alpha-one", "alpha-one"] {
            key(&mut harness, KeyCode::Up).await;
            assert_eq!(harness.commandline_text(), expected);
        }
        for expected in ["beta-two", "alpha-three", "", ""] {
            key(&mut harness, KeyCode::Down).await;
            assert_eq!(harness.commandline_text(), expected);
        }
        key(&mut harness, KeyCode::Esc).await;
    }

    key(&mut harness, KeyCode::Char(':')).await;
    key(&mut harness, KeyCode::Up).await;
    assert_eq!(harness.commandline_text(), "write-history-only");
}

#[tokio::test]
async fn search_history_filters_by_prefix_and_restores_the_original_draft() {
    let mut harness = with_history("", &["alpha-one", "beta-two", "alpha-three"]);
    start(&mut harness, '?').await;
    paste(&mut harness, "alp").await;

    control(&mut harness, 'p').await;
    assert_eq!(harness.commandline_text(), "alpha-three");
    key(&mut harness, KeyCode::Up).await;
    assert_eq!(harness.commandline_text(), "alpha-one");
    control(&mut harness, 'n').await;
    assert_eq!(harness.commandline_text(), "alpha-three");
    key(&mut harness, KeyCode::Down).await;
    assert_eq!(harness.commandline_text(), "alp");
    key(&mut harness, KeyCode::Esc).await;

    start(&mut harness, '/').await;
    paste(&mut harness, "no-match").await;
    key(&mut harness, KeyCode::Up).await;
    key(&mut harness, KeyCode::Down).await;
    assert_eq!(harness.commandline_text(), "no-match");
}

#[tokio::test]
async fn search_history_edits_start_a_new_prefix_session() {
    let mut harness = with_history("", &["alpha-one", "alpha-two"]);

    for edit in [
        Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
        Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        Event::Paste("x\nignored".to_string()),
    ] {
        start(&mut harness, '/').await;
        paste(&mut harness, "alp").await;
        key(&mut harness, KeyCode::Up).await;
        assert_eq!(harness.commandline_text(), "alpha-two");
        harness.execute_event(edit).await.unwrap();
        let edited = harness.commandline_text().to_string();

        key(&mut harness, KeyCode::Up).await;
        key(&mut harness, KeyCode::Down).await;
        assert_eq!(harness.commandline_text(), edited);
        key(&mut harness, KeyCode::Esc).await;
    }
}

#[tokio::test]
async fn recalled_search_previews_and_commits_in_the_current_direction() {
    let mut harness = with_history("alpha\nalpha\nmiddle\nalpha\nalpha", &[]);
    submit(&mut harness, '/', "alpha").await;
    harness
        .execute_action(Action::SetCursor(0, 2))
        .await
        .unwrap();

    start(&mut harness, '?').await;
    key(&mut harness, KeyCode::Up).await;
    harness.assert_cursor_at(0, 1);
    assert!(harness.commandline_row().starts_with("?alpha"));
    key(&mut harness, KeyCode::Enter).await;
    key(&mut harness, KeyCode::Char('n')).await;
    harness.assert_cursor_at(0, 0);
    key(&mut harness, KeyCode::Char('N')).await;
    harness.assert_cursor_at(0, 1);

    start(&mut harness, '/').await;
    key(&mut harness, KeyCode::Up).await;
    harness.assert_cursor_at(0, 3);
    key(&mut harness, KeyCode::Down).await;
    harness.assert_cursor_at(0, 1);
    key(&mut harness, KeyCode::Up).await;
    key(&mut harness, KeyCode::Esc).await;
    harness.assert_cursor_at(0, 1);
    key(&mut harness, KeyCode::Char('n')).await;
    harness.assert_cursor_at(0, 0);
}

#[tokio::test]
async fn unchanged_history_recall_preserves_manual_preview_navigation() {
    let mut harness = with_history("start\nalpha\nalpha\nalpha", &["alpha"]);
    start(&mut harness, '/').await;
    key(&mut harness, KeyCode::Up).await;
    harness.assert_cursor_at(0, 1);
    control(&mut harness, 'g').await;
    harness.assert_cursor_at(0, 2);
    key(&mut harness, KeyCode::Up).await;
    harness.assert_cursor_at(0, 2);
    control(&mut harness, 't').await;
    harness.assert_cursor_at(0, 1);
}

#[tokio::test]
async fn search_history_respects_disabled_incremental_search() {
    let mut preferences = PreferencesStore::in_memory();
    preferences.record_search("alpha").unwrap();
    let mut config = default_config();
    config.search.incsearch = false;
    let mut harness = with_preferences("start\nalpha\nalpha", config, preferences);

    start(&mut harness, '/').await;
    key(&mut harness, KeyCode::Up).await;
    assert_eq!(harness.commandline_text(), "alpha");
    harness.assert_cursor_at(0, 0);
    key(&mut harness, KeyCode::Enter).await;
    harness.assert_cursor_at(0, 1);
}

#[tokio::test]
async fn submitted_searches_persist_but_cancelled_and_empty_drafts_do_not() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("preferences.json");
    let mut harness = with_preferences(
        "alpha beta",
        default_config(),
        PreferencesStore::load(&path),
    );

    submit(&mut harness, '/', "alpha").await;
    submit(&mut harness, '?', "alpha").await;
    submit(&mut harness, '/', "missing").await;
    assert!(harness.last_error().unwrap().contains("Pattern not found"));
    submit(&mut harness, '?', "[").await;
    assert!(harness.last_error().is_some());
    start(&mut harness, '/').await;
    paste(&mut harness, "cancelled").await;
    key(&mut harness, KeyCode::Esc).await;
    submit(&mut harness, '?', "").await;
    submit(&mut harness, '/', "   ").await;

    let reloaded = PreferencesStore::load(&path);
    assert_eq!(reloaded.search_history(), ["alpha", "missing", "[", "   "]);
    assert!(reloaded.command_history().is_empty());

    let mut reopened = with_preferences("alpha beta", default_config(), reloaded);
    start(&mut reopened, '?').await;
    for expected in ["   ", "[", "missing", "alpha"] {
        key(&mut reopened, KeyCode::Up).await;
        assert_eq!(reopened.commandline_text(), expected);
    }
}
