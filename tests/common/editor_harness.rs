use std::path::PathBuf;

use crossterm::event::Event;
use red::{
    buffer::Buffer,
    config::Config,
    editor::{Action, Editor, Mode},
    lsp::LspClient,
    test_utils::EditorTestExt,
    theme::Theme,
};

use super::mock_lsp::MockLsp;

/// Test harness for editor integration tests
///
/// This provides a wrapper around the Editor that exposes test-friendly methods
/// for inspecting state and simulating user actions.
pub struct EditorHarness {
    pub editor: Editor,
}

impl EditorHarness {
    /// Create a new test harness with empty buffer
    pub fn new() -> Self {
        Self::with_content("")
    }

    /// Create a new test harness with initial content
    pub fn with_content(content: &str) -> Self {
        let buffer = Buffer::new(None, content.to_string());
        Self::with_buffer(buffer)
    }

    /// Create a new test harness with a specific buffer
    pub fn with_buffer(buffer: Buffer) -> Self {
        let lsp = Box::new(MockLsp) as Box<dyn LspClient + Send>;
        let config = Config::default();
        let theme = Theme::default();
        let mut editor = Editor::with_size(lsp, 80, 24, config, theme, vec![buffer]).unwrap();
        editor.test_disable_terminal_output();

        Self { editor }
    }

    /// Create a new test harness with custom configuration
    pub fn with_config(buffer: Buffer, config: Config) -> Self {
        let lsp = Box::new(MockLsp) as Box<dyn LspClient + Send>;
        let theme = Theme::default();
        let mut editor = Editor::with_size(lsp, 80, 24, config, theme, vec![buffer]).unwrap();
        editor.test_disable_terminal_output();

        Self { editor }
    }

    // Test helper methods using the new test APIs

    /// Execute an action on the editor
    pub async fn execute_action(&mut self, action: Action) -> anyhow::Result<()> {
        self.editor.test_execute_action(action).await
    }

    /// Execute a raw terminal event on the editor
    pub async fn execute_event(&mut self, event: Event) -> anyhow::Result<()> {
        self.editor.test_execute_event(event).await
    }

    /// Get current cursor position
    pub fn cursor_position(&self) -> (usize, usize) {
        self.editor.test_cursor_position()
    }

    /// Get current editor mode
    pub fn mode(&self) -> Mode {
        self.editor.test_mode()
    }

    /// Get buffer contents
    pub fn buffer_contents(&self) -> String {
        self.editor.test_buffer_contents()
    }

    /// Get line contents at specific index
    pub fn line_contents(&self, line: usize) -> Option<String> {
        self.editor.test_line_contents(line)
    }

    /// Get current line contents
    pub fn current_line(&self) -> Option<String> {
        self.editor.test_current_line()
    }

    /// Get number of lines in buffer
    pub fn line_count(&self) -> usize {
        self.editor.test_line_count()
    }

    /// Check if editor is in insert mode
    pub fn is_insert(&self) -> bool {
        self.editor.test_is_insert()
    }

    /// Check if editor is in normal mode
    pub fn is_normal(&self) -> bool {
        self.editor.test_is_normal()
    }

    pub fn is_dirty(&self) -> bool {
        self.editor.test_current_buffer().is_dirty()
    }

    pub fn buffer_names(&self) -> Vec<String> {
        self.editor.test_buffer_names()
    }

    pub fn current_buffer_index(&self) -> usize {
        self.editor.test_current_buffer_index()
    }

    pub fn last_error(&self) -> Option<&str> {
        self.editor.test_last_error()
    }

    /// Check if editor is in visual mode
    pub fn is_visual(&self) -> bool {
        self.editor.test_is_visual()
    }

    /// Type text in insert mode
    pub async fn type_text(&mut self, text: &str) -> anyhow::Result<()> {
        self.editor.test_type_text(text).await
    }

    /// Assert cursor is at expected position
    pub fn assert_cursor_at(&self, x: usize, y: usize) {
        let (cx, cy) = self.cursor_position();
        assert_eq!(
            (cx, cy),
            (x, y),
            "Expected cursor at ({}, {}), but was at ({}, {})",
            x,
            y,
            cx,
            cy
        );
    }

    /// Assert editor is in expected mode
    pub fn assert_mode(&self, mode: Mode) {
        assert_eq!(
            self.mode(),
            mode,
            "Expected mode {:?}, but was {:?}",
            mode,
            self.mode()
        );
    }

    /// Assert buffer has expected contents
    pub fn assert_buffer_contents(&self, expected: &str) {
        let actual = self.buffer_contents();
        assert_eq!(
            actual, expected,
            "Buffer contents mismatch\nExpected:\n{}\nActual:\n{}",
            expected, actual
        );
    }

    /// Assert line has expected contents
    pub fn assert_line_contents(&self, line: usize, expected: &str) {
        let actual = self.line_contents(line).unwrap_or_default();
        assert_eq!(actual, expected, "Line {} contents mismatch", line);
    }

    /// Get current buffer line (0-based line index where cursor is)
    pub fn buffer_line(&self) -> usize {
        self.editor.test_buffer_line()
    }

    pub fn viewport_top(&self) -> usize {
        self.editor.test_vtop()
    }

    pub fn set_viewport_cursor(&mut self, vtop: usize, cx: usize, cy: usize) {
        self.editor.test_set_viewport_cursor(vtop, cx, cy);
    }

    pub fn active_window_id(&self) -> usize {
        self.editor.test_active_window_id()
    }

    pub fn render_cursor_position(&self) -> Option<(usize, usize)> {
        self.editor.test_render_cursor_position()
    }

    pub fn is_waiting_for_key_sequence(&self) -> bool {
        self.editor.test_is_waiting_for_key_sequence()
    }

    pub fn set_commandline(&mut self, mode: Mode, text: &str) {
        self.editor.test_set_commandline(mode, text);
    }

    pub fn commandline_row(&mut self) -> String {
        self.editor.test_commandline_row()
    }

    pub fn commandline_text(&self) -> &str {
        self.editor.test_commandline_text()
    }

    pub fn statusline_row(&mut self) -> String {
        self.editor.test_statusline_row()
    }

    pub fn render_row(&mut self, y: usize) -> anyhow::Result<String> {
        self.editor.test_render_row(y)
    }
}

/// Test builder for setting up complex editor scenarios
pub struct EditorTestBuilder {
    content: String,
    config: Option<Config>,
    initial_mode: Option<Mode>,
    file_path: Option<PathBuf>,
}

impl EditorTestBuilder {
    pub fn new() -> Self {
        Self {
            content: String::new(),
            config: None,
            initial_mode: None,
            file_path: None,
        }
    }

    pub fn with_content(mut self, content: &str) -> Self {
        self.content = content.to_string();
        self
    }

    pub fn with_config(mut self, config: Config) -> Self {
        self.config = Some(config);
        self
    }

    pub fn with_file_path(mut self, path: PathBuf) -> Self {
        self.file_path = Some(path);
        self
    }

    pub fn build(self) -> EditorHarness {
        let file_path = self.file_path.map(|p| p.to_string_lossy().into_owned());
        let buffer = Buffer::new(file_path, self.content);

        if let Some(config) = self.config {
            EditorHarness::with_config(buffer, config)
        } else {
            EditorHarness::with_buffer(buffer)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_harness_creation() {
        let harness = EditorHarness::new();
        // Empty buffers have a single newline
        assert_eq!(harness.buffer_contents(), "\n");
        assert_eq!(harness.cursor_position(), (0, 0));
        assert!(harness.is_normal());
    }

    #[test]
    fn test_harness_with_content() {
        let harness = EditorHarness::with_content("Hello\nWorld");
        assert_eq!(harness.buffer_contents(), "Hello\nWorld");
        // buffer.len() returns len_lines() - 1
        assert_eq!(harness.line_count(), 1);
        // Lines include newlines
        assert_eq!(harness.line_contents(0), Some("Hello\n".to_string()));
        assert_eq!(harness.line_contents(1), Some("World".to_string()));
    }

    #[test]
    fn test_builder() {
        let harness = EditorTestBuilder::new()
            .with_content("Test content")
            .build();
        assert_eq!(harness.buffer_contents(), "Test content");
    }

    #[tokio::test]
    async fn test_mode_transition() {
        let mut harness = EditorHarness::new();
        harness.assert_mode(Mode::Normal);

        harness
            .execute_action(Action::EnterMode(Mode::Insert))
            .await
            .unwrap();
        harness.assert_mode(Mode::Insert);

        harness
            .execute_action(Action::EnterMode(Mode::Normal))
            .await
            .unwrap();
        harness.assert_mode(Mode::Normal);
    }

    #[tokio::test]
    async fn test_picker_opens_on_tiny_terminal() {
        let mut harness = EditorHarness::new();
        harness.editor.test_set_size(1, 2);

        harness
            .execute_action(Action::OpenPicker(
                Some("files".to_string()),
                vec!["alpha".to_string()],
                None,
            ))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_refresh_renders_on_tiny_terminal() {
        let mut harness = EditorHarness::new();
        harness.editor.test_set_size(1, 1);

        harness.execute_action(Action::Refresh).await.unwrap();

        harness.editor.test_set_size(0, 0);
        harness.execute_action(Action::Refresh).await.unwrap();
    }

    #[tokio::test]
    async fn test_window_switch_preserves_cursor_state() {
        let mut harness = EditorHarness::with_content("Line 1\nLine 2\nLine 3");

        harness.execute_action(Action::MoveDown).await.unwrap();
        harness.execute_action(Action::SplitVertical).await.unwrap();
        assert_eq!(harness.active_window_id(), 1);

        harness.execute_action(Action::MoveDown).await.unwrap();
        harness.execute_action(Action::MoveDown).await.unwrap();
        harness
            .execute_action(Action::PreviousWindow)
            .await
            .unwrap();
        assert_eq!(harness.active_window_id(), 0);
        harness.assert_cursor_at(0, 1);

        harness.execute_action(Action::NextWindow).await.unwrap();
        assert_eq!(harness.active_window_id(), 1);
        harness.assert_cursor_at(0, 2);
    }

    #[tokio::test]
    async fn test_window_cursor_clamps_to_last_real_line_with_trailing_newline() {
        let mut harness = EditorHarness::with_content("Line 1\nLine 2\nLine 3\n");

        harness.execute_action(Action::SplitVertical).await.unwrap();
        assert_eq!(harness.active_window_id(), 1);

        for _ in 0..10 {
            harness.execute_action(Action::MoveDown).await.unwrap();
        }

        harness.assert_cursor_at(0, 2);
        assert_eq!(harness.buffer_line(), 2);
        assert_eq!(harness.current_line(), Some("Line 3\n".to_string()));
        assert_eq!(harness.render_cursor_position(), Some((43, 2)));
    }

    #[tokio::test]
    async fn test_render_cursor_uses_active_window_buffer_line() {
        let content = (0..30)
            .map(|line| match line {
                22 => "a".to_string(),
                23 => "👋".to_string(),
                _ => "x".to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut harness = EditorHarness::with_content(&content);

        for _ in 0..22 {
            harness.execute_action(Action::MoveDown).await.unwrap();
        }
        harness.execute_action(Action::MoveRight).await.unwrap();

        assert_eq!(harness.buffer_line(), 22);
        assert_eq!(harness.render_cursor_position(), Some((4, 21)));
    }

    #[tokio::test]
    async fn test_mouse_click_preserves_visible_viewport() {
        let content = (0..60)
            .map(|line| format!("line-{line:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut harness = EditorHarness::with_content(&content);

        harness
            .execute_action(Action::SetCursor(0, 30))
            .await
            .unwrap();
        let viewport_top = harness.viewport_top();
        assert_eq!(viewport_top, 9);

        harness
            .execute_event(crossterm::event::Event::Mouse(
                crossterm::event::MouseEvent {
                    kind: crossterm::event::MouseEventKind::Down(
                        crossterm::event::MouseButton::Left,
                    ),
                    column: 4,
                    row: 12,
                    modifiers: crossterm::event::KeyModifiers::NONE,
                },
            ))
            .await
            .unwrap();

        assert_eq!(harness.viewport_top(), viewport_top);
        assert_eq!(harness.buffer_line(), 21);
        harness.assert_cursor_at(0, 21);
    }

    #[tokio::test]
    async fn test_mouse_click_honors_scrolloff() {
        let content = (0..60)
            .map(|line| format!("line-{line:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        let config = Config {
            scrolloff: Some(3),
            ..Default::default()
        };
        let buffer = Buffer::new(None, content);
        let mut harness = EditorHarness::with_config(buffer, config);

        harness
            .execute_action(Action::SetCursor(0, 30))
            .await
            .unwrap();
        assert_eq!(harness.viewport_top(), 12);

        harness
            .execute_event(crossterm::event::Event::Mouse(
                crossterm::event::MouseEvent {
                    kind: crossterm::event::MouseEventKind::Down(
                        crossterm::event::MouseButton::Left,
                    ),
                    column: 4,
                    row: 21,
                    modifiers: crossterm::event::KeyModifiers::NONE,
                },
            ))
            .await
            .unwrap();

        assert_eq!(harness.buffer_line(), 33);
        assert_eq!(harness.viewport_top(), 15);
        harness.assert_cursor_at(0, 33);
    }

    #[tokio::test]
    async fn test_inactive_window_uses_its_own_gutter_width() {
        let root =
            std::env::temp_dir().join(format!("red-window-gutter-{}.txt", uuid::Uuid::new_v4()));
        let content = (1..=10)
            .map(|line| format!("Line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&root, content).unwrap();

        let mut harness = EditorHarness::new();
        harness
            .execute_action(Action::SplitVerticalWithFile(
                root.to_string_lossy().into_owned(),
            ))
            .await
            .unwrap();
        harness
            .execute_action(Action::PreviousWindow)
            .await
            .unwrap();

        let row = harness.render_row(9).unwrap();
        let right_window_gutter = row.chars().skip(40).take(4).collect::<String>();
        assert_eq!(right_window_gutter, " 10 ");

        std::fs::remove_file(root).unwrap();
    }

    #[test]
    fn test_empty_buffer_gutter_hides_synthetic_trailing_line() {
        let mut harness = EditorHarness::new();

        let first_row = harness.render_row(0).unwrap();
        let second_row = harness.render_row(1).unwrap();

        assert_eq!(first_row.chars().take(3).collect::<String>(), " 1 ");
        assert_eq!(second_row.chars().take(3).collect::<String>(), "   ");
    }

    #[test]
    fn test_trailing_newline_gutter_hides_synthetic_trailing_line() {
        let mut harness = EditorHarness::with_content("Line 1\nLine 2\n");

        let first_row = harness.render_row(0).unwrap();
        let second_row = harness.render_row(1).unwrap();
        let third_row = harness.render_row(2).unwrap();

        assert_eq!(first_row.chars().take(3).collect::<String>(), " 1 ");
        assert_eq!(second_row.chars().take(3).collect::<String>(), " 2 ");
        assert_eq!(third_row.chars().take(3).collect::<String>(), "   ");
    }

    #[test]
    fn test_final_line_without_trailing_newline_renders_content() {
        let mut harness = EditorHarness::with_content("Line 1\nLine 2");

        let first_row = harness.render_row(0).unwrap();
        let second_row = harness.render_row(1).unwrap();
        let third_row = harness.render_row(2).unwrap();

        assert!(first_row.contains("Line 1"));
        assert!(second_row.contains("Line 2"));
        assert_eq!(third_row.chars().take(3).collect::<String>(), "   ");
    }

    #[tokio::test]
    async fn test_insert_mode_gutter_keeps_opened_trailing_line_visible() {
        let mut harness = EditorHarness::with_content("Line 1");

        harness
            .execute_action(Action::InsertLineBelowCursor)
            .await
            .unwrap();

        harness.assert_mode(Mode::Insert);
        harness.assert_cursor_at(0, 1);

        let second_row = harness.render_row(1).unwrap();
        assert_eq!(second_row.chars().take(3).collect::<String>(), " 2 ");
    }

    #[test]
    fn test_search_commandline_renders_search_text_on_small_width() {
        let mut harness = EditorHarness::new();
        harness.editor.test_set_size(8, 4);
        harness.set_commandline(Mode::Search, "👋x");

        assert_eq!(harness.commandline_row(), "/👋 x    ");
    }

    #[test]
    fn test_commandline_truncates_last_error_on_small_width() {
        let mut harness = EditorHarness::new();
        harness.editor.test_set_size(8, 4);
        harness
            .editor
            .test_set_last_error("LSP server error: INFO taplo: registered request handler");

        assert_eq!(harness.commandline_row(), "LSP serv");
    }

    #[test]
    fn test_commandline_cursor_uses_display_width() {
        let mut harness = EditorHarness::new();
        harness.set_commandline(Mode::Search, "👋x");

        assert_eq!(harness.render_cursor_position(), Some((4, 23)));
    }

    #[test]
    fn test_statusline_renders_on_small_width() {
        let mut harness = EditorHarness::with_content("content");
        harness.editor.test_set_size(8, 4);

        assert_eq!(harness.statusline_row().chars().count(), 8);
    }
}
