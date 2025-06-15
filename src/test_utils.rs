/// Test utilities for the Red editor
/// This module provides test helpers without requiring feature flags
use crate::editor::{Action, Editor, Mode};

/// Extension trait for Editor that provides test-specific functionality
#[allow(async_fn_in_trait)]
pub trait EditorTestExt {
    /// Get current cursor position for testing
    fn test_cursor_position(&self) -> (usize, usize);

    /// Get current mode for testing
    fn test_mode(&self) -> Mode;

    /// Execute an action for testing - uses core logic only
    async fn test_execute_action(&mut self, action: Action) -> anyhow::Result<()>;

    /// Get buffer contents for testing
    fn test_buffer_contents(&self) -> String;

    /// Get specific line contents for testing
    fn test_line_contents(&self, line: usize) -> Option<String>;

    /// Get the number of lines in the current buffer
    fn test_line_count(&self) -> usize;

    /// Check if editor is in insert mode
    fn test_is_insert(&self) -> bool;

    /// Check if editor is in normal mode
    fn test_is_normal(&self) -> bool;

    /// Check if editor is in visual mode
    fn test_is_visual(&self) -> bool;

    /// Get viewport top line
    fn test_viewport_top(&self) -> usize;

    /// Simulate typing text in insert mode
    async fn test_type_text(&mut self, text: &str) -> anyhow::Result<()>;

    /// Get the current line under cursor
    fn test_current_line(&self) -> Option<String>;
}

impl EditorTestExt for Editor {
    fn test_cursor_position(&self) -> (usize, usize) {
        (self.test_cursor_x(), self.test_buffer_line())
    }

    fn test_mode(&self) -> Mode {
        self.test_mode()
    }

    async fn test_execute_action(&mut self, action: Action) -> anyhow::Result<()> {
        self.apply_action_core(&action)?;
        Ok(())
    }

    fn test_buffer_contents(&self) -> String {
        self.test_current_buffer().contents()
    }

    fn test_line_contents(&self, line: usize) -> Option<String> {
        self.test_current_buffer().get(line)
    }

    fn test_line_count(&self) -> usize {
        self.test_current_buffer().len()
    }

    fn test_is_insert(&self) -> bool {
        self.test_is_insert()
    }

    fn test_is_normal(&self) -> bool {
        self.test_is_normal()
    }

    fn test_is_visual(&self) -> bool {
        matches!(
            self.test_mode(),
            Mode::Visual | Mode::VisualLine | Mode::VisualBlock
        )
    }

    fn test_viewport_top(&self) -> usize {
        self.test_vtop()
    }

    async fn test_type_text(&mut self, text: &str) -> anyhow::Result<()> {
        if !self.test_is_insert() {
            self.test_execute_action(Action::EnterMode(Mode::Insert))
                .await?;
        }
        for ch in text.chars() {
            self.test_execute_action(Action::InsertCharAtCursorPos(ch))
                .await?;
        }
        Ok(())
    }

    fn test_current_line(&self) -> Option<String> {
        self.test_current_line_contents()
    }
}
