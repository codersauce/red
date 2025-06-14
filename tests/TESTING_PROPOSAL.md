# Proposal: Making Red Editor Testable

## Problem Statement

The current editor architecture makes integration testing difficult:
- Core editor methods (`execute`, `buffer_line`, `current_buffer`, etc.) are private
- Editor state (cursor position, mode) is not accessible for testing
- No way to simulate user actions programmatically

## Proposed Solution

Add conditional compilation attributes to expose testing APIs without affecting the production build.

### 1. Test-Only Public Methods

Add these methods to `src/editor.rs`:

```rust
#[cfg(test)]
impl Editor {
    /// Get current cursor position for testing
    pub fn test_cursor_position(&self) -> (usize, usize) {
        (self.cx, self.cy)
    }

    /// Get current mode for testing
    pub fn test_mode(&self) -> Mode {
        self.mode
    }

    /// Get current buffer line for testing
    pub fn test_buffer_line(&self) -> usize {
        self.buffer_line()
    }

    /// Execute an action for testing
    pub async fn test_execute_action(
        &mut self,
        action: Action,
    ) -> anyhow::Result<()> {
        let mut buffer = RenderBuffer::new(80, 24, Style::default());
        let runtime = Arc::new(Mutex::new(None));
        self.execute(&action, &mut buffer, runtime).await?;
        Ok(())
    }

    /// Get buffer contents for testing
    pub fn test_buffer_contents(&self) -> String {
        self.current_buffer().contents()
    }

    /// Get specific line contents for testing
    pub fn test_line_contents(&self, line: usize) -> Option<String> {
        self.current_buffer().line(line)
    }
}
```

### 2. Test Builder Pattern

Create a test-friendly editor builder:

```rust
#[cfg(test)]
pub struct TestEditorBuilder {
    content: String,
    cursor: Option<(usize, usize)>,
    mode: Option<Mode>,
    config: Option<Config>,
}

#[cfg(test)]
impl TestEditorBuilder {
    pub fn new() -> Self { ... }
    pub fn with_content(mut self, content: &str) -> Self { ... }
    pub fn with_cursor_at(mut self, x: usize, y: usize) -> Self { ... }
    pub fn with_mode(mut self, mode: Mode) -> Self { ... }
    pub fn build(self) -> Editor { ... }
}
```

### 3. Action Simulation Helpers

Create helpers for common test scenarios:

```rust
#[cfg(test)]
impl Editor {
    /// Simulate typing text in insert mode
    pub async fn test_type_text(&mut self, text: &str) -> anyhow::Result<()> {
        self.test_execute_action(Action::EnterMode(Mode::Insert)).await?;
        for ch in text.chars() {
            self.test_execute_action(Action::InsertCharAtCursorPos(ch)).await?;
        }
        self.test_execute_action(Action::EnterMode(Mode::Normal)).await?;
        Ok(())
    }

    /// Simulate a sequence of normal mode commands
    pub async fn test_normal_commands(&mut self, commands: &str) -> anyhow::Result<()> {
        // Parse and execute vim-like commands
        Ok(())
    }
}
```

## Benefits

1. **No Production Impact**: Test code is only compiled during testing
2. **Type Safe**: Leverages Rust's type system
3. **Maintainable**: Test APIs live alongside implementation
4. **Comprehensive**: Enables testing all editor functionality

## Example Test

With these changes, tests become straightforward:

```rust
#[tokio::test]
async fn test_basic_editing() {
    let mut editor = TestEditorBuilder::new()
        .with_content("Hello\nWorld")
        .build();
    
    // Verify initial state
    assert_eq!(editor.test_cursor_position(), (0, 0));
    assert_eq!(editor.test_mode(), Mode::Normal);
    
    // Move to end of first line and append
    editor.test_execute_action(Action::MoveToLineEnd).await.unwrap();
    editor.test_execute_action(Action::EnterMode(Mode::Insert)).await.unwrap();
    editor.test_type_text(", Rust!").await.unwrap();
    
    // Verify result
    assert_eq!(editor.test_line_contents(0), Some("Hello, Rust!".to_string()));
}
```

## Implementation Steps

1. Add test methods to Editor (minimal change)
2. Update EditorHarness to use new test methods
3. Write comprehensive tests for all commands
4. Add GitHub Actions workflow for running tests
5. Set up code coverage reporting

## Alternative Approaches Considered

1. **Make methods public**: Rejected - exposes internals unnecessarily
2. **Friend testing pattern**: Not available in Rust
3. **Separate test module**: Would require significant refactoring
4. **Terminal emulation testing**: Too complex for unit tests

## Conclusion

This approach provides a clean, maintainable way to test the editor without compromising its design or exposing internals in production builds.