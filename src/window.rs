use crate::editor::Point;

/// Represents a single window displaying a buffer
#[derive(Debug, Clone)]
pub struct Window {
    /// Index of the buffer being displayed
    pub buffer_index: usize,

    /// Position of the window within the terminal (x, y)
    pub position: Point,

    /// Size of the window (width, height)
    pub size: (usize, usize),

    /// Top line of viewport (for vertical scrolling)
    pub vtop: usize,

    /// Left column of viewport (for horizontal scrolling)
    pub vleft: usize,

    /// Cursor x position (column) within the buffer
    pub cx: usize,

    /// Cursor y position (line) within the viewport
    pub cy: usize,

    /// Whether this window is currently active
    pub active: bool,
}

impl Window {
    /// Creates a new window with the given buffer index and dimensions
    pub fn new(buffer_index: usize, position: Point, size: (usize, usize)) -> Self {
        Self {
            buffer_index,
            position,
            size,
            vtop: 0,
            vleft: 0,
            cx: 0,
            cy: 0,
            active: false,
        }
    }

    /// Returns the visible width of the window (accounting for borders if any)
    pub fn inner_width(&self) -> usize {
        self.size.0
    }

    /// Returns the visible height of the window (accounting for borders if any)
    pub fn inner_height(&self) -> usize {
        self.size.1
    }

    /// Checks if a terminal position is within this window
    pub fn contains_position(&self, x: usize, y: usize) -> bool {
        x >= self.position.x
            && x < self.position.x + self.size.0
            && y >= self.position.y
            && y < self.position.y + self.size.1
    }

    /// Converts terminal coordinates to window-local coordinates
    pub fn terminal_to_local(&self, term_x: usize, term_y: usize) -> Option<(usize, usize)> {
        if self.contains_position(term_x, term_y) {
            Some((term_x - self.position.x, term_y - self.position.y))
        } else {
            None
        }
    }

    /// Converts window-local coordinates to terminal coordinates
    pub fn local_to_terminal(&self, local_x: usize, local_y: usize) -> (usize, usize) {
        (self.position.x + local_x, self.position.y + local_y)
    }
}

/// Represents a split in the window layout
#[derive(Debug, Clone)]
pub enum Split {
    /// A leaf node containing a window
    Window(Window),

    /// A horizontal split (top/bottom)
    Horizontal {
        top: Box<Split>,
        bottom: Box<Split>,
        /// Position of the split (0.0 = top, 1.0 = bottom)
        ratio: f32,
    },

    /// A vertical split (left/right)
    Vertical {
        left: Box<Split>,
        right: Box<Split>,
        /// Position of the split (0.0 = left, 1.0 = right)
        ratio: f32,
    },
}

impl Split {
    /// Creates a new window split
    pub fn new_window(buffer_index: usize, position: Point, size: (usize, usize)) -> Self {
        Split::Window(Window::new(buffer_index, position, size))
    }

    /// Recursively finds all windows in the split tree
    pub fn windows(&self) -> Vec<&Window> {
        match self {
            Split::Window(w) => vec![w],
            Split::Horizontal { top, bottom, .. } => {
                let mut windows = top.windows();
                windows.extend(bottom.windows());
                windows
            }
            Split::Vertical { left, right, .. } => {
                let mut windows = left.windows();
                windows.extend(right.windows());
                windows
            }
        }
    }

    /// Recursively finds all windows in the split tree (mutable)
    pub fn windows_mut(&mut self) -> Vec<&mut Window> {
        match self {
            Split::Window(w) => vec![w],
            Split::Horizontal { top, bottom, .. } => {
                let mut windows = top.windows_mut();
                windows.extend(bottom.windows_mut());
                windows
            }
            Split::Vertical { left, right, .. } => {
                let mut windows = left.windows_mut();
                windows.extend(right.windows_mut());
                windows
            }
        }
    }

    /// Recalculates window positions and sizes based on the split tree
    pub fn layout(&mut self, position: Point, size: (usize, usize)) {
        match self {
            Split::Window(w) => {
                w.position = position;
                w.size = size;
            }
            Split::Horizontal { top, bottom, ratio } => {
                let split_y = (size.1 as f32 * *ratio) as usize;
                top.layout(position, (size.0, split_y));
                bottom.layout(
                    Point::new(position.x, position.y + split_y),
                    (size.0, size.1 - split_y),
                );
            }
            Split::Vertical { left, right, ratio } => {
                let split_x = (size.0 as f32 * *ratio) as usize;
                left.layout(position, (split_x, size.1));
                right.layout(
                    Point::new(position.x + split_x, position.y),
                    (size.0 - split_x, size.1),
                );
            }
        }
    }
}

/// Manages windows and their layout
pub struct WindowManager {
    /// The root of the split tree
    root: Split,

    /// Currently active window ID (index in the windows list)
    active_window_id: usize,
}

impl WindowManager {
    /// Creates a new WindowManager with a single window
    pub fn new(buffer_index: usize, terminal_size: (usize, usize)) -> Self {
        let mut root = Split::new_window(
            buffer_index,
            Point::new(0, 0),
            (terminal_size.0, terminal_size.1.saturating_sub(2)), // Leave room for status/command line
        );

        // Set the first window as active
        if let Split::Window(w) = &mut root {
            w.active = true;
        }

        Self {
            root,
            active_window_id: 0,
        }
    }

    /// Returns the currently active window
    pub fn active_window(&self) -> Option<&Window> {
        self.root.windows().get(self.active_window_id).copied()
    }

    /// Returns the currently active window (mutable)
    pub fn active_window_mut(&mut self) -> Option<&mut Window> {
        // For now, just handle the simple case of a single window
        match &mut self.root {
            Split::Window(w) if self.active_window_id == 0 => Some(w),
            _ => None, // TODO: implement for split windows
        }
    }

    /// Returns all windows
    pub fn windows(&self) -> Vec<&Window> {
        self.root.windows()
    }

    /// Returns all windows (mutable)
    pub fn windows_mut(&mut self) -> Vec<&mut Window> {
        self.root.windows_mut()
    }

    /// Updates the layout when terminal is resized
    pub fn resize(&mut self, terminal_size: (usize, usize)) {
        self.root.layout(
            Point::new(0, 0),
            (terminal_size.0, terminal_size.1.saturating_sub(2)),
        );
    }

    /// Sets the active window by ID
    pub fn set_active(&mut self, window_id: usize) {
        // Deactivate all windows
        for window in self.root.windows_mut() {
            window.active = false;
        }

        // Activate the selected window
        if let Some(window) = self.root.windows_mut().get_mut(window_id) {
            window.active = true;
            self.active_window_id = window_id;
        }
    }

    /// Finds the window at the given terminal position
    pub fn window_at_position(&self, x: usize, y: usize) -> Option<(usize, &Window)> {
        self.root
            .windows()
            .iter()
            .enumerate()
            .find(|(_, w)| w.contains_position(x, y))
            .map(|(id, w)| (id, *w))
    }

    /// Splits the active window horizontally
    pub fn split_horizontal(&mut self, _new_buffer_index: usize) -> Option<()> {
        // TODO: Implement window splitting
        // This will require rebuilding the split tree
        None
    }

    /// Splits the active window vertically
    pub fn split_vertical(&mut self, _new_buffer_index: usize) -> Option<()> {
        // TODO: Implement window splitting
        // This will require rebuilding the split tree
        None
    }

    /// Closes the active window
    pub fn close_window(&mut self) -> Option<()> {
        // TODO: Implement window closing
        // This will require rebuilding the split tree
        None
    }
}
