Red Editor Architecture Summary

Current Architecture

1. Single Buffer/View Model

- Editor maintains a Vec<Buffer> with a current_buffer_index
- Only one buffer is visible at a time
- Viewport is managed by vtop (vertical top) and vleft (horizontal left) for scrolling
- Cursor position tracked by cx (column) and cy (row) relative to viewport  


2. Rendering Pipeline

- RenderBuffer is a 2D grid of cells (char + style) representing the entire terminal
- Rendering happens in layers:  
  a. Main content (text buffer)  
  b. Overlays (selections, diagnostics)  
  c. UI chrome (gutter, statusline, commandline)  
  d. Dialogs/popups
- Differential rendering: only changed cells are sent to terminal  


3. Coordinate Systems

- Character indices (for rope data structure)
- Display columns (accounting for wide chars, tabs)
- Viewport coordinates (visible area)
- Terminal coordinates (absolute screen position)  


4. Event Loop

- Async event loop handles keyboard, mouse, LSP messages, and plugin callbacks
- Events are processed through mode-specific handlers
- Actions are dispatched and can be undone/redone  


Window/Split Support Plan

Phase 1: Window Management Core

1. Create Window struct to encapsulate:

- Buffer reference
- Viewport state (vtop, vleft)
- Cursor position (cx, cy)
- Size and position within terminal

2. Create WindowManager to handle:

- Window layout (splits, resizing)
- Active window tracking
- Window creation/deletion
- Focus management  


Phase 2: Layout System

1. Implement split types:

- Horizontal splits
- Vertical splits
- Nested splits (tree structure)

2. Layout algorithms:

- Calculate window positions/sizes
- Handle terminal resize
- Minimum window size constraints  


Phase 3: Rendering Updates

1. Modify rendering pipeline:

- Each window renders to its own region
- Composite windows into final RenderBuffer
- Handle window borders/separators

2. Update coordinate transformations:

- Window-local to terminal coordinates
- Mouse position to window mapping  


Phase 4: Command/Navigation

1. New commands:

- Split horizontal/vertical
- Close window
- Navigate between windows
- Resize windows

2. Update existing commands:

- Buffer operations work on active window
- Cursor movement constrained to window  


Phase 5: State Management

1. Per-window state:

- Mode (could have different modes per window)
- Selection
- Search state

2. Shared state:

- Buffers
- Registers
- Command history  


Key Files to Modify:

- src/editor.rs - Add WindowManager, update Editor struct
- src/window.rs (new) - Window and WindowManager implementation
- src/editor/rendering.rs - Update to render multiple windows
- src/buffer.rs - No changes needed (buffers remain independent)
- src/main.rs - Minor updates for initialization  


This approach maintains backward compatibility while adding window support incrementally.
