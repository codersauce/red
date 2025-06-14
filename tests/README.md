# Integration Testing Plan for Red Editor

## Current Status

The test framework has been set up with:
- `MockLsp`: A mock implementation of the LSP client for testing
- `EditorHarness`: A test harness for creating editor instances
- Basic test structure for movement commands

## Challenges

The editor's current architecture makes integration testing difficult because:
1. Most editor methods are private
2. The `execute` method that processes actions is private
3. Editor state (cursor position, mode, etc.) is not publicly accessible
4. The main `run` method starts an interactive terminal session

## Proposed Solutions

### Short-term (Minimal Changes)
1. Add public getter methods to Editor for testing:
   ```rust
   #[cfg(test)]
   pub fn test_cursor_position(&self) -> (usize, usize)
   #[cfg(test)]
   pub fn test_mode(&self) -> Mode
   #[cfg(test)]
   pub fn test_execute_action(&mut self, action: Action) -> Result<()>
   ```

### Medium-term (Better Architecture)
1. Extract editor logic into a testable core that doesn't depend on terminal I/O
2. Create a public API for editor operations
3. Implement a command pattern that can be tested independently

### Long-term (Comprehensive Testing)
1. Full integration tests using terminal emulation
2. Property-based testing for editor operations
3. Fuzzing for robustness testing

## Test Categories to Implement

### 1. Movement Commands
- Basic movements: h, j, k, l
- Word movements: w, b, e, W, B, E
- Line movements: 0, ^, $, g_
- Paragraph movements: {, }
- File movements: gg, G, [n]G
- Screen movements: H, M, L, Ctrl-U, Ctrl-D, Ctrl-F, Ctrl-B

### 2. Editing Commands
- Insert modes: i, a, I, A, o, O
- Delete operations: x, X, d{motion}, dd, D
- Change operations: c{motion}, cc, C, s, S
- Replace: r, R
- Undo/Redo: u, Ctrl-R

### 3. Mode Transitions
- Normal -> Insert
- Insert -> Normal
- Normal -> Visual (v, V, Ctrl-V)
- Visual -> Normal
- Normal -> Command (:)
- Command -> Normal

### 4. Visual Mode Operations
- Character selection
- Line selection
- Block selection
- Operations on selections (d, c, y, etc.)

### 5. Search and Replace
- Forward search: /
- Backward search: ?
- Next/Previous: n, N
- Replace: :s/pattern/replacement/

### 6. File Operations
- Save: :w
- Quit: :q
- Save and quit: :wq
- Force quit: :q!
- Open file: :e
- Buffer navigation

### 7. Advanced Features
- Multiple cursors
- Macros
- Marks
- Registers
- Folding

## Implementation Strategy

1. Start with the most basic operations (movement, mode changes)
2. Add test helpers as needed
3. Gradually increase test coverage
4. Consider using snapshot testing for complex scenarios
5. Add performance benchmarks for critical operations

## Example Test Structure

```rust
#[test]
async fn test_basic_movement() {
    let mut editor = TestEditor::new("Hello\nWorld");
    
    // Start at (0, 0)
    assert_eq!(editor.cursor(), (0, 0));
    
    // Move right
    editor.execute(Action::MoveRight).await;
    assert_eq!(editor.cursor(), (1, 0));
    
    // Move down
    editor.execute(Action::MoveDown).await;
    assert_eq!(editor.cursor(), (1, 1));
}
```

## Next Steps

1. Propose API changes to make editor testable
2. Implement basic test helpers
3. Write tests for core functionality
4. Set up CI to run tests
5. Add code coverage reporting