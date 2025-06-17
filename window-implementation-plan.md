# Red Editor Window Implementation Completion Plan

## Overview
This plan outlines the remaining work needed to complete the window splitting functionality in the Red editor. The core window management is implemented, but the rendering system needs to be made window-aware.

## Phase 1: Window-Aware Rendering (Priority: Critical)

### 1.1 Update Coordinate Systems
- Modify `vheight()` and `vwidth()` to optionally take a window parameter
- Add window-aware coordinate transformation methods:
  ```rust
  fn window_to_terminal_x(&self, window: &Window, x: usize) -> usize
  fn window_to_terminal_y(&self, window: &Window, y: usize) -> usize
  fn buffer_to_window_coords(&self, window: &Window, buf_x: usize, buf_y: usize) -> Option<(usize, usize)>
  ```

### 1.2 Refactor render_main_content()
- Add window parameter to render_main_content() 
- Modify to render only within window boundaries:
  - Start x at window.position.x + gutter_width
  - Start y at window.position.y
  - Clip rendering at window boundaries
  - Translate all coordinates through window offset

### 1.3 Update render_window() Method
- Pass window boundaries to render_main_content()
- Implement proper clipping for overlays (diagnostics, selections)
- Add window border rendering

### 1.4 Fix Cursor Positioning
- Update cursor positioning to account for active window position
- Modify flush_to_terminal() to position cursor relative to window

## Phase 2: Window Borders and Separators

### 2.1 Implement Window Border Rendering
- Add `render_window_borders()` method
- Draw vertical separators: `│` character
- Draw horizontal separators: `─` character
- Draw intersection points: `┼`, `├`, `┤`, `┬`, `┴`
- Style borders with a subtle color from theme

### 2.2 Update Window Layout
- Reserve 1 character for borders in layout calculations
- Adjust window.size to account for border space
- Update inner_width() and inner_height() to subtract border space

## Phase 3: Complete Window Operations

### 3.1 Implement Window Closing
```rust
fn close_window(&mut self) -> Option<()> {
    // 1. Find parent of active window in split tree
    // 2. Replace parent with sibling window
    // 3. Recalculate layout
    // 4. Update active window ID if needed
    // 5. Handle edge case of closing last window
}
```

### 3.2 Implement Window Navigation (MoveWindow*)
- Add methods to find adjacent windows in each direction
- Implement focus switching based on spatial relationship
- Handle edge cases at boundaries

### 3.3 Implement Window Resizing
- Add resize operations to Split enum
- Implement ratio adjustment logic
- Add visual feedback during resize

## Phase 4: UI Component Updates

### 4.1 Update Status Line
- Make status line window-aware
- Show per-window information (buffer name, position)
- Highlight active window indicator

### 4.2 Update Dialogs and Pickers
- Center dialogs within active window (not full terminal)
- Implement global vs window-local dialog positioning
- Update completion widget positioning

### 4.3 Fix Mouse Event Handling
- Update mouse click handling to find target window
- Route mouse events to correct window
- Implement click-to-focus window

## Phase 5: Advanced Features

### 5.1 Different Buffers per Window
- Update window creation to optionally open different files
- Add `:split <filename>` support
- Implement buffer synchronization for same file in multiple windows

### 5.2 Window-Specific Viewport
- Ensure each window maintains independent scroll position
- Sync cursor position when switching to window with same buffer

### 5.3 Window Balancing
- Implement equal distribution of space
- Add smart resizing when terminal size changes

## Phase 6: Testing and Polish

### 6.1 Edge Case Handling
- Minimum window size enforcement (e.g., 10x3)
- Maximum split depth limits
- Terminal resize with multiple windows
- Very small terminal sizes

### 6.2 Performance Optimization
- Implement differential rendering for window borders
- Optimize coordinate transformations
- Cache window layout calculations

### 6.3 Configuration Options
- Add config for border style (single, double, none)
- Add config for minimum window size
- Add config for default split ratios

## Implementation Order and Time Estimates

1. **Week 1**: Phase 1 (Window-Aware Rendering)
   - Days 1-2: Coordinate system updates
   - Days 3-4: Refactor render_main_content
   - Day 5: Fix cursor positioning

2. **Week 2**: Phase 2-3 (Borders and Operations)
   - Days 1-2: Window borders
   - Days 3-4: Window closing
   - Day 5: Window navigation

3. **Week 3**: Phase 4-5 (UI Updates and Advanced Features)
   - Days 1-2: Status line and dialogs
   - Days 3-4: Mouse handling and multi-buffer
   - Day 5: Window balancing

4. **Week 4**: Phase 6 (Testing and Polish)
   - Days 1-3: Edge cases and bug fixes
   - Days 4-5: Performance and configuration

## Technical Considerations

### Rendering Architecture Changes
- Consider creating a `WindowContext` struct to pass rendering boundaries
- May need to refactor RenderBuffer to support clipping regions
- Consider abstracting coordinate transformations into a trait

### State Management
- Decide whether to keep viewport state in Editor or fully move to Window
- Consider impact on plugin API - may need window-aware plugin events
- Handle undo/redo across windows

### Compatibility
- Ensure single-window mode works exactly as before
- Keep plugin API stable or provide migration path
- Maintain config file compatibility

## Success Criteria
- [ ] Windows render in separate, non-overlapping regions
- [ ] Window borders clearly delineate boundaries  
- [ ] All cursor movements respect window boundaries
- [ ] Window operations (split, close, navigate) work reliably
- [ ] UI elements (status, dialogs) are window-aware
- [ ] Performance remains good with 4+ windows
- [ ] No regressions in single-window mode

## Current Status (as of implementation)
- ✅ Core window data structures implemented
- ✅ Window splitting creates proper tree structure  
- ✅ Window navigation (next/previous) works
- ✅ State synchronization between editor and windows
- ✅ Commands and keybindings integrated
- ✅ Phase 1.1: Coordinate transformation methods added
- ✅ Phase 1.2: render_main_content refactored to be window-aware
- ✅ Phase 1.4: Cursor positioning updated for active window
- ✅ Phase 2.1: Window borders with proper intersection characters
- ✅ Phase 2.2: Window layout accounts for separators
- ✅ Phase 3.1: Window closing implemented (`:close` or `Ctrl-w c/q`)
- ✅ Phase 3.2: Directional navigation (Ctrl-w h/j/k/l)
- ✅ Phase 5.1: Split commands support opening different files
- ✅ Phase 1.3: Overlays (diagnostics, selections, line highlights) are window-aware
- ✅ Phase 4.1: Status line is window-aware (shows active window info and indicator)
- ✅ Phase 3.3: Window resizing implemented (Ctrl-w <, >, +, -)
- ✅ Windows render in separate, non-overlapping regions
- ✅ Fixed: Gutter renders correctly for all windows
- ✅ Fixed: Correct window gets split when using vsplit
- ❌ Dialogs not window-aware (Phase 4.2)
- ❌ Mouse support for window selection not implemented (Phase 4.3)
- ❌ Window balancing not implemented
- ❌ Window maximizing not implemented