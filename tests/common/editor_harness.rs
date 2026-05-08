use std::path::PathBuf;

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

    pub fn active_window_id(&self) -> usize {
        self.editor.test_active_window_id()
    }

    pub fn render_cursor_position(&self) -> Option<(usize, usize)> {
        self.editor.test_render_cursor_position()
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
        assert_eq!(harness.render_cursor_position(), Some((5, 21)));
    }
}
