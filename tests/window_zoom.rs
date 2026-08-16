mod common;

use common::EditorHarness;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use red::{
    buffer::Buffer,
    config::Config,
    editor::{Action, Mode, Point},
    plugin::{
        PanelConfig, PanelSide, TextPanelBlock, TextPanelBlockFormat, TextPanelBlockKind,
        TextPanelComposerConfig,
    },
};

fn harness() -> EditorHarness {
    let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
    EditorHarness::with_config(
        Buffer::new(None, "editor-only-marker\n".to_string()),
        config,
    )
}

async fn key(harness: &mut EditorHarness, code: KeyCode, modifiers: KeyModifiers) {
    harness
        .execute_event(Event::Key(KeyEvent::new(code, modifiers)))
        .await
        .unwrap();
}

async fn chord(harness: &mut EditorHarness, key_name: char) {
    key(harness, KeyCode::Char('w'), KeyModifiers::CONTROL).await;
    key(harness, KeyCode::Char(key_name), KeyModifiers::NONE).await;
}

fn content(harness: &mut EditorHarness, height: usize) -> String {
    (0..height)
        .map(|y| harness.render_row(y).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

fn block(text: &str) -> TextPanelBlock {
    TextPanelBlock {
        id: "answer".to_string(),
        kind: TextPanelBlockKind::Agent,
        format: TextPanelBlockFormat::Plain,
        text: text.to_string(),
    }
}

#[tokio::test]
async fn window_zoom_restores_nested_splits_and_docked_panes() {
    let mut editor = harness();
    editor.execute_action(Action::SplitVertical).await.unwrap();
    editor
        .execute_action(Action::SplitHorizontal)
        .await
        .unwrap();
    editor
        .execute_action(Action::ResizeWindowUp(2))
        .await
        .unwrap();
    editor.editor.test_create_panel(
        "tree",
        PanelConfig {
            title: Some("tree-only-marker".to_string()),
            width: 20,
            ..PanelConfig::default()
        },
    );
    let bounds = editor.editor.test_active_window_bounds();
    let before = editor.editor.test_session_snapshot();

    chord(&mut editor, 'z').await;
    assert_eq!(
        editor.editor.test_active_window_bounds(),
        Some((Point::new(0, 0), (80, 22)))
    );
    assert!(editor.statusline_row().contains("ZOOM"));
    assert_eq!(editor.editor.test_window_count(), 3);
    assert!(!content(&mut editor, 22).contains("tree-only-marker"));
    let during = editor.editor.test_session_snapshot();
    assert_eq!(during.window_layout, before.window_layout);
    assert_eq!(during.panels, before.panels);

    chord(&mut editor, 'z').await;
    assert_eq!(editor.editor.test_active_window_bounds(), bounds);
    assert!(!editor.statusline_row().contains("ZOOM"));
    assert!(content(&mut editor, 22).contains("tree-only-marker"));
    assert_eq!(
        editor.editor.test_session_snapshot().window_layout,
        before.window_layout
    );
}

#[tokio::test]
async fn window_zoom_survives_resize_and_restores_before_layout_changes() {
    let mut editor = harness();
    editor.execute_action(Action::SplitVertical).await.unwrap();
    let before = editor.editor.test_session_snapshot().window_layout;
    chord(&mut editor, 'z').await;
    editor.execute_event(Event::Resize(100, 30)).await.unwrap();
    assert_eq!(
        editor.editor.test_active_window_bounds(),
        Some((Point::new(0, 0), (100, 28)))
    );
    chord(&mut editor, 'z').await;
    assert_eq!(editor.editor.test_session_snapshot().window_layout, before);
    assert!(editor.editor.test_active_window_bounds().unwrap().1 .0 < 100);

    for action in [
        Action::NextWindow,
        Action::BalanceWindows,
        Action::ResizeWindowLeft(1),
        Action::SplitHorizontal,
    ] {
        chord(&mut editor, 'z').await;
        assert!(editor.statusline_row().contains("ZOOM"));
        editor.execute_action(action).await.unwrap();
        assert!(!editor.statusline_row().contains("ZOOM"));
    }
    chord(&mut editor, 'z').await;
    editor.execute_action(Action::CloseWindow).await.unwrap();
    assert!(!editor.statusline_row().contains("ZOOM"));
}

#[tokio::test]
async fn pane_zoom_covers_all_dock_edges_and_preserves_saved_layout() {
    for side in [
        PanelSide::Left,
        PanelSide::Right,
        PanelSide::Top,
        PanelSide::Bottom,
    ] {
        for text in [false, true] {
            let mut editor = harness();
            let config = PanelConfig {
                side,
                width: 12,
                title: Some("pane-only-marker".to_string()),
                ..PanelConfig::default()
            };
            if text {
                editor.editor.test_create_text_panel("target", config);
                editor
                    .editor
                    .test_update_text_panel("target", vec![block("stream contents")]);
            } else {
                editor.editor.test_create_panel("target", config);
            }
            assert!(editor.editor.test_focus_panel("target"));
            editor.execute_action(Action::Refresh).await.unwrap();
            let before = editor.editor.test_session_snapshot();
            chord(&mut editor, 'z').await;
            assert!(editor.statusline_row().contains("ZOOM"));
            let screen = content(&mut editor, 22);
            assert!(screen.contains("pane-only-marker"));
            assert!(!screen.contains("editor-only-marker"));
            assert_eq!(editor.editor.test_panel_layout("target"), Some((side, 12)));
            chord(&mut editor, 'z').await;
            let after = editor.editor.test_session_snapshot();
            assert_eq!(after.window_layout, before.window_layout);
            assert_eq!(after.panels.panels[0].side, before.panels.panels[0].side);
            assert_eq!(
                after.panels.panels[0].vertical_size,
                before.panels.panels[0].vertical_size
            );
            assert_eq!(
                after.panels.panels[0].horizontal_size,
                before.panels.panels[0].horizontal_size
            );
            assert_eq!(editor.editor.test_focused_panel_id(), Some("target"));
        }
    }
}

#[tokio::test]
async fn agent_pane_zoom_preserves_draft_stream_and_composer_focus() {
    let mut editor = harness();
    editor.editor.test_create_text_panel(
        "agent",
        PanelConfig {
            side: PanelSide::Right,
            width: 25,
            title: Some("Agent".to_string()),
            composer: Some(TextPanelComposerConfig {
                placeholder: "Ask".to_string(),
                rows: 3,
            }),
            ..PanelConfig::default()
        },
    );
    assert!(editor.editor.test_focus_text_panel_composer("agent"));
    editor
        .execute_event(Event::Paste("draft survives zoom".to_string()))
        .await
        .unwrap();
    chord(&mut editor, 'z').await;
    editor
        .editor
        .test_update_text_panel("agent", vec![block("stream arrived while zoomed")]);
    let cursor = editor.editor.test_render_cursor_position().unwrap();
    assert!(cursor.0 < 80 && cursor.1 < 22);
    assert!(content(&mut editor, 22).contains("stream arrived while zoomed"));
    chord(&mut editor, 'z').await;
    let saved = editor.editor.test_session_snapshot().panels;
    let agent = saved
        .panels
        .iter()
        .find(|panel| panel.id == "agent")
        .unwrap();
    assert_eq!(
        agent.text.as_ref().unwrap().composer.as_ref().unwrap().text,
        "draft survives zoom"
    );
    assert_eq!(editor.editor.test_focused_panel_id(), Some("agent"));
    assert!(editor.editor.test_render_cursor_position().is_some());

    chord(&mut editor, 'z').await;
    editor.editor.test_close_panel("agent");
    editor.execute_action(Action::Refresh).await.unwrap();
    assert!(!editor.statusline_row().contains("ZOOM"));
    assert!(content(&mut editor, 22).contains("editor-only-marker"));
}

#[tokio::test]
async fn window_zoom_keeps_edits_and_leaves_legacy_commands_intact() {
    let mut editor = harness();
    chord(&mut editor, 'z').await;
    assert!(!editor.statusline_row().contains("ZOOM"));
    editor.execute_action(Action::SplitVertical).await.unwrap();
    chord(&mut editor, 'z').await;
    editor
        .execute_action(Action::EnterMode(Mode::Insert))
        .await
        .unwrap();
    editor
        .execute_action(Action::InsertString("new ".to_string()))
        .await
        .unwrap();
    editor
        .execute_action(Action::EnterMode(Mode::Normal))
        .await
        .unwrap();
    chord(&mut editor, 'z').await;
    assert!(editor.buffer_contents().contains("new "));
    let ordinary_width = editor.editor.test_active_window_bounds().unwrap().1 .0;
    chord(&mut editor, '_').await;
    assert!(editor.editor.test_active_window_bounds().unwrap().1 .0 > ordinary_width);
    assert!(!editor.statusline_row().contains("ZOOM"));
    chord(&mut editor, 'o').await;
    assert_eq!(editor.editor.test_window_count(), 1);
}

#[tokio::test]
async fn window_zoom_is_cleared_by_session_restore_and_panel_replacement() {
    let mut editor = harness();
    editor.execute_action(Action::SplitVertical).await.unwrap();
    let snapshot = editor.editor.test_session_snapshot();
    chord(&mut editor, 'z').await;
    editor.editor.restore_session_snapshot(&snapshot).unwrap();
    editor.execute_action(Action::Refresh).await.unwrap();
    assert!(!editor.statusline_row().contains("ZOOM"));
    assert_eq!(editor.editor.test_window_count(), 2);
    editor
        .editor
        .test_create_panel("target", PanelConfig::default());
    assert!(editor.editor.test_focus_panel("target"));
    chord(&mut editor, 'z').await;
    editor
        .editor
        .test_create_panel("target", PanelConfig::default());
    assert!(!editor.statusline_row().contains("ZOOM"));
}
