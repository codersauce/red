use crate::editor::Point;

#[derive(Debug, Clone, Copy)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

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

    /// X offset of the viewport (for horizontal positioning)
    pub vx: usize,
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
            vx: 0,
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
                // Reserve 1 row for the horizontal separator
                let available_height = size.1.saturating_sub(1);
                let split_y = (available_height as f32 * *ratio) as usize;

                top.layout(position, (size.0, split_y));
                // Bottom window starts after the separator
                bottom.layout(
                    Point::new(position.x, position.y + split_y + 1),
                    (size.0, available_height - split_y),
                );
            }
            Split::Vertical { left, right, ratio } => {
                // Reserve 1 column for the vertical separator
                let available_width = size.0.saturating_sub(1);
                let split_x = (available_width as f32 * *ratio) as usize;

                left.layout(position, (split_x, size.1));
                // Right window starts after the separator
                right.layout(
                    Point::new(position.x + split_x + 1, position.y),
                    (available_width - split_x, size.1),
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
        let mut current_id = 0;
        Self::get_window_mut_recursive(&mut self.root, &mut current_id, self.active_window_id)
    }

    fn get_window_mut_recursive<'a>(
        node: &'a mut Split,
        current_id: &mut usize,
        target_id: usize,
    ) -> Option<&'a mut Window> {
        match node {
            Split::Window(window) => {
                if *current_id == target_id {
                    Some(window)
                } else {
                    *current_id += 1;
                    None
                }
            }
            Split::Horizontal { top, bottom, .. } => {
                if let Some(window) = Self::get_window_mut_recursive(top, current_id, target_id) {
                    return Some(window);
                }
                Self::get_window_mut_recursive(bottom, current_id, target_id)
            }
            Split::Vertical { left, right, .. } => {
                if let Some(window) = Self::get_window_mut_recursive(left, current_id, target_id) {
                    return Some(window);
                }
                Self::get_window_mut_recursive(right, current_id, target_id)
            }
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
    pub fn split_horizontal(&mut self, new_buffer_index: usize) -> Option<()> {
        use crate::log;
        log!(
            "WindowManager::split_horizontal called with buffer {}",
            new_buffer_index
        );

        // Get the current terminal bounds from the root split
        let (width, height) = self.get_terminal_bounds();
        log!("Terminal bounds: {}x{}", width, height);
        log!("Active window id before split: {}", self.active_window_id);

        let new_root =
            self.split_node(&self.root, self.active_window_id, new_buffer_index, true)?;
        self.root = new_root;
        self.root.layout(Point::new(0, 0), (width, height));

        // Update active window to the new window
        let windows = self.root.windows();
        log!("Window count after split: {}", windows.len());

        // The new window should be the bottom one in the split we just created
        // Since we're doing a depth-first traversal, it should be right after the original window
        self.active_window_id = self.active_window_id + 1;
        self.set_active(self.active_window_id);
        log!("Active window id after split: {}", self.active_window_id);

        Some(())
    }

    /// Splits the active window vertically
    pub fn split_vertical(&mut self, new_buffer_index: usize) -> Option<()> {
        use crate::log;
        log!(
            "WindowManager::split_vertical called with buffer {}",
            new_buffer_index
        );

        // Get the current terminal bounds from the root split
        let (width, height) = self.get_terminal_bounds();
        log!("Active window id before split: {}", self.active_window_id);

        let new_root =
            self.split_node(&self.root, self.active_window_id, new_buffer_index, false)?;
        self.root = new_root;
        self.root.layout(Point::new(0, 0), (width, height));

        // Update active window to the new window
        let windows = self.root.windows();
        log!("Window count after split: {}", windows.len());

        // The new window should be the right one in the split we just created
        // Since we're doing a depth-first traversal, it should be right after the original window
        self.active_window_id = self.active_window_id + 1;
        self.set_active(self.active_window_id);
        log!("Active window id after split: {}", self.active_window_id);

        Some(())
    }

    /// Closes the active window
    pub fn close_window(&mut self) -> Option<()> {
        use crate::log;

        // Can't close if there's only one window
        let window_count = self.root.windows().len();
        if window_count <= 1 {
            log!("Cannot close the last window");
            return None;
        }

        log!(
            "Closing window {} of {}",
            self.active_window_id,
            window_count
        );

        // Get the terminal bounds before modification
        let (width, height) = self.get_terminal_bounds();

        // Remove the window from the tree
        if let Some(new_root) = self.remove_window(&self.root, self.active_window_id) {
            self.root = new_root;
            self.root.layout(Point::new(0, 0), (width, height));

            // Update active window ID
            let new_window_count = self.root.windows().len();
            if self.active_window_id >= new_window_count {
                self.active_window_id = new_window_count - 1;
            }
            self.set_active(self.active_window_id);

            log!("Window closed. New window count: {}", new_window_count);
            Some(())
        } else {
            log!("Failed to close window");
            None
        }
    }

    /// Removes a window from the split tree and returns the new root
    fn remove_window(&self, node: &Split, target_id: usize) -> Option<Split> {
        let mut current_id = 0;
        self.remove_window_recursive(node, &mut current_id, target_id)
    }

    fn remove_window_recursive(
        &self,
        node: &Split,
        current_id: &mut usize,
        target_id: usize,
    ) -> Option<Split> {
        match node {
            Split::Window(_) => {
                if *current_id == target_id {
                    // This window should be removed - return None to signal removal
                    None
                } else {
                    *current_id += 1;
                    Some(node.clone())
                }
            }
            Split::Horizontal { top, bottom, .. } => {
                let new_top = self.remove_window_recursive(top, current_id, target_id);
                let new_bottom = self.remove_window_recursive(bottom, current_id, target_id);

                match (new_top, new_bottom) {
                    (Some(t), Some(b)) => {
                        // Both children remain - keep the split
                        Some(Split::Horizontal {
                            top: Box::new(t),
                            bottom: Box::new(b),
                            ratio: 0.5, // Reset ratio for simplicity
                        })
                    }
                    (Some(remaining), None) | (None, Some(remaining)) => {
                        // One child was removed - replace this split with the remaining child
                        Some(remaining)
                    }
                    (None, None) => {
                        // Both children removed (shouldn't happen)
                        None
                    }
                }
            }
            Split::Vertical { left, right, .. } => {
                let new_left = self.remove_window_recursive(left, current_id, target_id);
                let new_right = self.remove_window_recursive(right, current_id, target_id);

                match (new_left, new_right) {
                    (Some(l), Some(r)) => {
                        // Both children remain - keep the split
                        Some(Split::Vertical {
                            left: Box::new(l),
                            right: Box::new(r),
                            ratio: 0.5, // Reset ratio for simplicity
                        })
                    }
                    (Some(remaining), None) | (None, Some(remaining)) => {
                        // One child was removed - replace this split with the remaining child
                        Some(remaining)
                    }
                    (None, None) => {
                        // Both children removed (shouldn't happen)
                        None
                    }
                }
            }
        }
    }

    /// Get the active window ID
    pub fn active_window_id(&self) -> usize {
        self.active_window_id
    }

    /// Find the window in the given direction from the active window
    pub fn find_window_in_direction(&self, direction: Direction) -> Option<usize> {
        let windows = self.root.windows();
        let active_window = self.active_window()?;

        let mut best_candidate: Option<(usize, i32)> = None; // (window_id, distance)

        for (id, window) in windows.iter().enumerate() {
            if id == self.active_window_id {
                continue;
            }

            // Calculate relative position
            let (dx, dy) = match direction {
                Direction::Left => {
                    // Window should be to the left
                    if window.position.x + window.size.0 <= active_window.position.x {
                        let dx = active_window.position.x as i32
                            - (window.position.x + window.size.0) as i32;
                        let dy = (window.position.y as i32 - active_window.position.y as i32).abs();
                        (dx, dy)
                    } else {
                        continue;
                    }
                }
                Direction::Right => {
                    // Window should be to the right
                    if window.position.x >= active_window.position.x + active_window.size.0 {
                        let dx = window.position.x as i32
                            - (active_window.position.x + active_window.size.0) as i32;
                        let dy = (window.position.y as i32 - active_window.position.y as i32).abs();
                        (dx, dy)
                    } else {
                        continue;
                    }
                }
                Direction::Up => {
                    // Window should be above
                    if window.position.y + window.size.1 <= active_window.position.y {
                        let dy = active_window.position.y as i32
                            - (window.position.y + window.size.1) as i32;
                        let dx = (window.position.x as i32 - active_window.position.x as i32).abs();
                        (dx, dy)
                    } else {
                        continue;
                    }
                }
                Direction::Down => {
                    // Window should be below
                    if window.position.y >= active_window.position.y + active_window.size.1 {
                        let dy = window.position.y as i32
                            - (active_window.position.y + active_window.size.1) as i32;
                        let dx = (window.position.x as i32 - active_window.position.x as i32).abs();
                        (dx, dy)
                    } else {
                        continue;
                    }
                }
            };

            // Calculate distance (prefer windows that are directly in line)
            let distance = match direction {
                Direction::Left | Direction::Right => dx + dy * 10, // Penalize vertical offset
                Direction::Up | Direction::Down => dy + dx * 10,    // Penalize horizontal offset
            };

            // Update best candidate if this is closer
            match best_candidate {
                None => best_candidate = Some((id, distance)),
                Some((_, best_distance)) => {
                    if distance < best_distance {
                        best_candidate = Some((id, distance));
                    }
                }
            }
        }

        best_candidate.map(|(id, _)| id)
    }

    /// Get the total terminal bounds by finding the maximum extents
    fn get_terminal_bounds(&self) -> (usize, usize) {
        let windows = self.root.windows();
        if windows.is_empty() {
            return (80, 24); // Default size
        }

        let mut max_x = 0;
        let mut max_y = 0;

        for window in windows {
            max_x = max_x.max(window.position.x + window.size.0);
            max_y = max_y.max(window.position.y + window.size.1);
        }

        (max_x, max_y)
    }

    /// Helper method to split a node in the tree
    fn split_node(
        &self,
        node: &Split,
        target_window_id: usize,
        new_buffer_index: usize,
        horizontal: bool,
    ) -> Option<Split> {
        let mut current_id = 0;
        self.split_node_recursive(
            node,
            &mut current_id,
            target_window_id,
            new_buffer_index,
            horizontal,
        )
    }

    fn split_node_recursive(
        &self,
        node: &Split,
        current_id: &mut usize,
        target_window_id: usize,
        new_buffer_index: usize,
        horizontal: bool,
    ) -> Option<Split> {
        use crate::log;
        match node {
            Split::Window(window) => {
                log!(
                    "split_node_recursive: Checking window {} (target: {})",
                    *current_id,
                    target_window_id
                );
                if *current_id == target_window_id {
                    log!("  Found target window to split!");
                    // This is the window to split
                    let mut new_window =
                        Window::new(new_buffer_index, window.position, window.size);
                    new_window.active = false;

                    let mut old_window = window.clone();
                    old_window.active = false;

                    if horizontal {
                        Some(Split::Horizontal {
                            top: Box::new(Split::Window(old_window)),
                            bottom: Box::new(Split::Window(new_window)),
                            ratio: 0.5,
                        })
                    } else {
                        Some(Split::Vertical {
                            left: Box::new(Split::Window(old_window)),
                            right: Box::new(Split::Window(new_window)),
                            ratio: 0.5,
                        })
                    }
                } else {
                    *current_id += 1;
                    Some(Split::Window(window.clone()))
                }
            }
            Split::Horizontal { top, bottom, ratio } => {
                let new_top = self.split_node_recursive(
                    top,
                    current_id,
                    target_window_id,
                    new_buffer_index,
                    horizontal,
                )?;
                let new_bottom = self.split_node_recursive(
                    bottom,
                    current_id,
                    target_window_id,
                    new_buffer_index,
                    horizontal,
                )?;
                Some(Split::Horizontal {
                    top: Box::new(new_top),
                    bottom: Box::new(new_bottom),
                    ratio: *ratio,
                })
            }
            Split::Vertical { left, right, ratio } => {
                let new_left = self.split_node_recursive(
                    left,
                    current_id,
                    target_window_id,
                    new_buffer_index,
                    horizontal,
                )?;
                let new_right = self.split_node_recursive(
                    right,
                    current_id,
                    target_window_id,
                    new_buffer_index,
                    horizontal,
                )?;
                Some(Split::Vertical {
                    left: Box::new(new_left),
                    right: Box::new(new_right),
                    ratio: *ratio,
                })
            }
        }
    }
}
