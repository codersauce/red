//! Split-tree ownership, stable window identity, viewport state, and terminal layout.
//!
//! [`WindowManager`] stores windows in a recursive [`Split`] tree. Tree-order indexes
//! are transient navigation handles, while [`WindowId`] remains stable across sibling
//! insertion and removal and is the correct identity for plugin resources. Layout
//! propagates terminal bounds through split ratios and updates each leaf window.
//!
//! Window cursor `x` values are grapheme indices and vertical goals are terminal display
//! columns. Buffer mutation and cursor validity remain editor responsibilities; this
//! module only owns per-window presentation and split topology.

use crate::{
    buffer::BufferId,
    editor::{CursorGoal, Point},
    undo::TextPosition,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_WINDOW_ID: AtomicU64 = AtomicU64::new(0);

/// Session-stable identity for a window.
///
/// Unlike the tree-order indexes accepted by the existing `WindowManager` API,
/// this value does not change when another window is split or closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WindowId(pub u64);

impl WindowId {
    fn next() -> Self {
        Self(NEXT_WINDOW_ID.fetch_add(1, Ordering::Relaxed))
    }
}

/// Spatial direction used for window navigation, movement, and split resizing.
#[derive(Debug, Clone, Copy)]
pub enum Direction {
    /// Toward smaller terminal rows.
    Up,
    /// Toward larger terminal rows.
    Down,
    /// Toward smaller terminal columns.
    Left,
    /// Toward larger terminal columns.
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DividerAxis {
    Vertical,
    Horizontal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SplitBranch {
    First,
    Second,
}

/// Stable split-tree path and original geometry for one draggable window divider.
#[derive(Debug, Clone)]
pub(crate) struct WindowDivider {
    axis: DividerAxis,
    path: Vec<SplitBranch>,
    origin: Point,
    size: (usize, usize),
}

impl WindowDivider {
    pub(crate) fn is_vertical(&self) -> bool {
        self.axis == DividerAxis::Vertical
    }

    pub(crate) fn coordinate_delta(&self, from: Point, to: Point) -> isize {
        let (from, to) = match self.axis {
            DividerAxis::Vertical => (from.x, to.x),
            DividerAxis::Horizontal => (from.y, to.y),
        };
        isize::try_from(to)
            .unwrap_or(isize::MAX)
            .saturating_sub(isize::try_from(from).unwrap_or(isize::MAX))
    }
}

/// Current terminal-cell segment occupied by one captured split divider.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DividerSpan {
    axis: DividerAxis,
    origin: Point,
    length: usize,
}

impl DividerSpan {
    /// Checks one separator cell without confusing neighboring split junctions.
    pub(crate) fn contains(&self, x: usize, y: usize) -> bool {
        match self.axis {
            DividerAxis::Vertical => {
                x == self.origin.x
                    && y >= self.origin.y
                    && y < self.origin.y.saturating_add(self.length)
            }
            DividerAxis::Horizontal => {
                y == self.origin.y
                    && x >= self.origin.x
                    && x < self.origin.x.saturating_add(self.length)
            }
        }
    }

    pub(crate) fn moved_by(&self, delta: isize) -> Point {
        match self.axis {
            DividerAxis::Vertical => {
                Point::new(self.origin.x.saturating_add_signed(delta), self.origin.y)
            }
            DividerAxis::Horizontal => {
                Point::new(self.origin.x, self.origin.y.saturating_add_signed(delta))
            }
        }
    }
}

/// Represents a single window displaying a buffer
#[derive(Debug, Clone)]
pub struct Window {
    /// Stable identity for this window within the current editor session.
    pub id: WindowId,

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

    /// First skipped display column when wrap mode scrolls within a long line.
    pub skipcol: usize,

    /// Whether this window wraps long lines.
    pub wrap: bool,

    /// Cursor x position (column) within the buffer
    pub cx: usize,

    /// Cursor y position (line) within the viewport
    pub cy: usize,

    /// Display-column goal used when moving vertically.
    pub(crate) cursor_goal: CursorGoal,

    /// Whether this window is currently active
    pub active: bool,

    /// X offset of the viewport (for horizontal positioning)
    pub vx: usize,

    /// Cursor locations remembered for CTRL-O/CTRL-I navigation in this window.
    pub(crate) jump_list: Box<JumpList>,
}

/// One edit-tracked destination in a window's jumplist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JumpEntry {
    pub(crate) buffer_id: BufferId,
    pub(crate) char_index: usize,
    pub(crate) fallback: TextPosition,
}

/// Window-local jumplist and its current traversal position.
#[derive(Debug, Clone, Default)]
pub(crate) struct JumpList {
    pub(crate) entries: Vec<JumpEntry>,
    pub(crate) index: usize,
}

impl Window {
    /// Creates a new window with the given buffer index and dimensions
    pub fn new(buffer_index: usize, position: Point, size: (usize, usize)) -> Self {
        Self::new_with_id(WindowId::next(), buffer_index, position, size)
    }

    fn new_with_id(
        id: WindowId,
        buffer_index: usize,
        position: Point,
        size: (usize, usize),
    ) -> Self {
        Self {
            id,
            buffer_index,
            position,
            size,
            vtop: 0,
            vleft: 0,
            skipcol: 0,
            wrap: true,
            cx: 0,
            cy: 0,
            cursor_goal: CursorGoal::default(),
            active: false,
            vx: 0,
            jump_list: Box::default(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertical_split_divider_hit_testing_finds_the_actual_shared_column() {
        let mut manager = WindowManager::new(/*buffer_index*/ 0, (80, 26));
        manager.split_vertical(/*new_buffer_index*/ 1).unwrap();

        let divider = manager
            .divider_at_position(/*x*/ 39, /*y*/ 8)
            .expect("the rendered vertical split should be draggable");

        assert_eq!(divider.axis, DividerAxis::Vertical);
        assert!(divider.path.is_empty());
        assert!(manager.divider_at_position(/*x*/ 38, /*y*/ 8).is_none());
        assert!(manager.divider_at_position(/*x*/ 40, /*y*/ 8).is_none());
        assert!(manager.divider_at_position(/*x*/ 39, /*y*/ 24).is_none());
    }

    #[test]
    fn horizontal_split_divider_hit_testing_finds_the_actual_shared_row() {
        let mut manager = WindowManager::new(/*buffer_index*/ 0, (80, 26));
        manager.split_horizontal(/*new_buffer_index*/ 1).unwrap();

        let divider = manager
            .divider_at_position(/*x*/ 12, /*y*/ 11)
            .expect("the rendered horizontal split should be draggable");

        assert_eq!(divider.axis, DividerAxis::Horizontal);
        assert!(manager.divider_at_position(/*x*/ 12, /*y*/ 10).is_none());
        assert!(manager.divider_at_position(/*x*/ 12, /*y*/ 12).is_none());
    }

    #[test]
    fn vertical_divider_span_follows_the_captured_split() {
        let mut manager = WindowManager::new(/*buffer_index*/ 0, (80, 26));
        manager.split_vertical(/*new_buffer_index*/ 1).unwrap();
        let divider = manager
            .divider_at_position(/*x*/ 39, /*y*/ 8)
            .expect("the original split must be draggable");

        let original = manager
            .divider_span(&divider)
            .expect("the original divider must have a visible span");
        assert!(original.contains(/*x*/ 39, /*y*/ 0));
        assert!(original.contains(/*x*/ 39, /*y*/ 23));
        assert!(!original.contains(/*x*/ 38, /*y*/ 8));
        assert!(!original.contains(/*x*/ 39, /*y*/ 24));

        assert!(manager.resize_divider(&divider, /*x*/ 54, /*y*/ 8));
        let moved = manager
            .divider_span(&divider)
            .expect("the same captured split must retain a visible span");
        assert!(moved.contains(/*x*/ 54, /*y*/ 0));
        assert!(moved.contains(/*x*/ 54, /*y*/ 23));
        assert!(!moved.contains(/*x*/ 39, /*y*/ 8));
    }

    #[test]
    fn horizontal_divider_span_follows_the_captured_split() {
        let mut manager = WindowManager::new(/*buffer_index*/ 0, (80, 26));
        manager.split_horizontal(/*new_buffer_index*/ 1).unwrap();
        let divider = manager
            .divider_at_position(/*x*/ 12, /*y*/ 11)
            .expect("the original split must be draggable");

        let original = manager
            .divider_span(&divider)
            .expect("the original divider must have a visible span");
        assert!(original.contains(/*x*/ 0, /*y*/ 11));
        assert!(original.contains(/*x*/ 79, /*y*/ 11));
        assert!(!original.contains(/*x*/ 12, /*y*/ 10));
        assert!(!original.contains(/*x*/ 80, /*y*/ 11));

        assert!(manager.resize_divider(&divider, /*x*/ 12, /*y*/ 16));
        let moved = manager
            .divider_span(&divider)
            .expect("the same captured split must retain a visible span");
        assert!(moved.contains(/*x*/ 0, /*y*/ 16));
        assert!(moved.contains(/*x*/ 79, /*y*/ 16));
        assert!(!moved.contains(/*x*/ 12, /*y*/ 11));
    }

    #[test]
    fn dragging_a_vertical_divider_preserves_window_identity_and_focus() {
        let mut manager = WindowManager::new(/*buffer_index*/ 0, (80, 26));
        manager.split_vertical(/*new_buffer_index*/ 1).unwrap();
        let active_id = manager.active_stable_window_id();
        let divider = manager.divider_at_position(/*x*/ 39, /*y*/ 5).unwrap();

        assert!(manager.resize_divider(&divider, /*x*/ 54, /*y*/ 5));

        assert_eq!(manager.active_stable_window_id(), active_id);
        assert_eq!(manager.windows()[0].size.0, 54);
        assert_eq!(manager.windows()[1].position.x, 55);
        assert_eq!(manager.windows()[1].size.0, 25);
        assert!(manager.divider_at_position(/*x*/ 54, /*y*/ 5).is_some());
    }

    #[test]
    fn vim_width_resize_grows_and_shrinks_either_side_by_exact_cells() {
        for active_window in [0, 1] {
            let mut manager = WindowManager::new(/*buffer_index*/ 0, (80, 26));
            manager.split_vertical(/*new_buffer_index*/ 1).unwrap();
            manager.set_active(active_window);
            let before = manager.active_window().unwrap().size.0;

            assert!(manager.resize_window_by_cells(Direction::Right, /*amount*/ 3));
            assert_eq!(manager.active_window().unwrap().size.0, before + 3);

            assert!(manager.resize_window_by_cells(Direction::Left, /*amount*/ 3));
            assert_eq!(manager.active_window().unwrap().size.0, before);
            assert_eq!(manager.active_window_id(), active_window);
        }
    }

    #[test]
    fn vim_height_resize_grows_and_shrinks_either_side_by_exact_cells() {
        for active_window in [0, 1] {
            let mut manager = WindowManager::new(/*buffer_index*/ 0, (80, 26));
            manager.split_horizontal(/*new_buffer_index*/ 1).unwrap();
            manager.set_active(active_window);
            let before = manager.active_window().unwrap().size.1;

            assert!(manager.resize_window_by_cells(Direction::Down, /*amount*/ 2));
            assert_eq!(manager.active_window().unwrap().size.1, before + 2);

            assert!(manager.resize_window_by_cells(Direction::Up, /*amount*/ 2));
            assert_eq!(manager.active_window().unwrap().size.1, before);
            assert_eq!(manager.active_window_id(), active_window);
        }
    }

    #[test]
    fn dragging_a_nested_divider_preserves_unrelated_split_ratios() {
        let mut manager = nested_window_manager();
        let active_id = manager.active_stable_window_id();
        let divider = manager
            .divider_at_position(/*x*/ 5, /*y*/ 11)
            .expect("the nested left-hand horizontal divider should be draggable");

        assert_eq!(divider.axis, DividerAxis::Horizontal);
        assert_eq!(divider.path, vec![SplitBranch::First]);
        assert!(manager.resize_divider(&divider, /*x*/ 5, /*y*/ 16));

        assert_eq!(manager.active_stable_window_id(), active_id);
        assert!(contains_split_ratio(&manager.root, 0.3));
        assert!(matches!(
            &manager.root,
            Split::Vertical { ratio, .. } if (*ratio - 0.5).abs() < f32::EPSILON
        ));
        assert!(manager.divider_at_position(/*x*/ 5, /*y*/ 16).is_some());
    }

    #[test]
    fn nested_divider_span_excludes_the_outer_split_and_follows_resize() {
        let mut manager = nested_window_manager();
        let divider = manager
            .divider_at_position(/*x*/ 5, /*y*/ 11)
            .expect("the nested divider must be draggable");

        let original = manager
            .divider_span(&divider)
            .expect("the nested divider must have a visible span");
        assert!(original.contains(/*x*/ 0, /*y*/ 11));
        assert!(original.contains(/*x*/ 38, /*y*/ 11));
        assert!(!original.contains(/*x*/ 39, /*y*/ 11));
        assert!(!original.contains(/*x*/ 40, /*y*/ 11));

        assert!(manager.resize_divider(&divider, /*x*/ 5, /*y*/ 16));
        let moved = manager
            .divider_span(&divider)
            .expect("the nested divider must follow its current split geometry");
        assert!(moved.contains(/*x*/ 0, /*y*/ 16));
        assert!(moved.contains(/*x*/ 38, /*y*/ 16));
        assert!(!moved.contains(/*x*/ 39, /*y*/ 16));
        assert!(!moved.contains(/*x*/ 5, /*y*/ 11));
    }

    #[test]
    fn dragging_a_split_preserves_nonzero_editor_origin() {
        let mut manager = WindowManager::new(/*buffer_index*/ 0, (80, 26));
        manager.split_vertical(/*new_buffer_index*/ 1).unwrap();
        manager.resize_with_origin(Point::new(/*x*/ 20, /*y*/ 3), (60, 25));
        let divider = manager
            .divider_at_position(/*x*/ 49, /*y*/ 8)
            .expect("a panel-offset editor divider should remain draggable");

        assert!(manager.resize_divider(&divider, /*x*/ 55, /*y*/ 8));

        assert_eq!(manager.windows()[0].position, Point::new(20, 3));
        assert_eq!(manager.windows()[0].size.0, 35);
        assert_eq!(manager.windows()[1].position, Point::new(56, 3));
        assert!(manager.windows().into_iter().all(|window| {
            window.position.x >= 20
                && window.position.x.saturating_add(window.size.0) <= 80
                && window.position.y >= 3
                && window.position.y.saturating_add(window.size.1) <= 26
        }));
    }

    #[test]
    fn extreme_divider_drags_preserve_nonempty_child_windows() {
        let mut manager = WindowManager::new(/*buffer_index*/ 0, (8, 8));
        manager.split_vertical(/*new_buffer_index*/ 1).unwrap();
        let divider = manager.divider_at_position(/*x*/ 3, /*y*/ 2).unwrap();

        assert!(manager.resize_divider(&divider, /*x*/ 0, /*y*/ 2));
        assert!(manager
            .windows()
            .into_iter()
            .all(|window| window.size.0 >= 1));
        assert!(manager.resize_divider(&divider, usize::MAX, /*y*/ 2));
        assert!(manager
            .windows()
            .into_iter()
            .all(|window| window.size.0 >= 1));
    }

    #[test]
    fn resize_reaches_nested_split_after_outer_membership_check() {
        let mut manager = WindowManager::new(0, (80, 26));
        manager.split_vertical(0).unwrap();
        manager.set_active(0);
        manager.split_horizontal(0).unwrap();

        assert_eq!(manager.active_window_id(), 1);
        let before_height = manager.active_window().unwrap().size.1;

        assert!(manager.resize_window(Direction::Up, 1).is_some());

        let after_height = manager.active_window().unwrap().size.1;
        assert!(
            after_height > before_height,
            "bottom-left nested window should grow upward"
        );
    }

    #[test]
    fn resizing_single_window_reports_noop() {
        let mut manager = WindowManager::new(0, (80, 26));

        assert!(manager.resize_window(Direction::Right, 1).is_none());
    }

    #[test]
    fn stable_window_ids_survive_tree_reordering() {
        let mut manager = WindowManager::new(0, (80, 26));
        let original_id = manager.active_stable_window_id().unwrap();
        manager.split_vertical(1).unwrap();
        let new_id = manager.active_stable_window_id().unwrap();

        manager.set_active(0);
        manager.close_window().unwrap();

        assert_ne!(original_id, new_id);
        assert_eq!(manager.active_window_id(), 0);
        assert_eq!(manager.active_stable_window_id(), Some(new_id));
        assert_eq!(manager.window_index(new_id), Some(0));
        assert!(manager.window(original_id).is_none());
    }

    #[test]
    fn split_ids_are_never_reused_after_close() {
        let mut manager = WindowManager::new(0, (80, 26));
        manager.split_vertical(1).unwrap();
        let closed_id = manager.active_stable_window_id().unwrap();
        manager.close_window().unwrap();
        manager.split_vertical(2).unwrap();

        assert!(manager.active_stable_window_id().unwrap() > closed_id);
    }

    #[test]
    fn close_first_window_in_split_keeps_sibling() {
        let mut manager = WindowManager::new(0, (80, 26));
        manager.split_vertical(0).unwrap();
        manager.set_active(0);

        assert!(manager.close_window().is_some());

        assert_eq!(manager.windows().len(), 1);
        assert_eq!(manager.active_window_id(), 0);
    }

    #[test]
    fn balance_windows_restores_even_split_ratios() {
        let mut manager = WindowManager::new(0, (80, 26));
        manager.split_vertical(0).unwrap();
        manager.set_active(0);

        assert!(manager.resize_window(Direction::Right, 4).is_some());
        let resized_width = manager.active_window().unwrap().size.0;

        assert!(manager.balance_windows().is_some());

        assert!(manager.active_window().unwrap().size.0 < resized_width);
        assert_eq!(manager.active_window().unwrap().size.0, 39);
        assert_eq!(manager.active_window_id(), 0);
    }

    #[test]
    fn maximize_window_expands_active_window() {
        let mut manager = WindowManager::new(0, (80, 26));
        manager.split_vertical(0).unwrap();
        manager.set_active(1);
        let initial_width = manager.active_window().unwrap().size.0;

        assert!(manager.maximize_window().is_some());

        assert!(manager.active_window().unwrap().size.0 > initial_width);
        assert_eq!(manager.active_window_id(), 1);
    }

    #[test]
    fn only_window_collapses_to_active_window() {
        let mut manager = WindowManager::new(0, (80, 26));
        manager.split_vertical(1).unwrap();
        manager.active_window_mut().unwrap().vtop = 3;
        manager.active_window_mut().unwrap().cx = 4;
        manager.active_window_mut().unwrap().cy = 5;

        assert!(manager.only_window().is_some());

        assert_eq!(manager.windows().len(), 1);
        assert_eq!(manager.active_window_id(), 0);
        let window = manager.active_window().unwrap();
        assert_eq!(window.buffer_index, 1);
        assert_eq!(window.vtop, 3);
        assert_eq!(window.cx, 4);
        assert_eq!(window.cy, 5);
        assert_eq!(window.position, Point::new(0, 0));
        assert_eq!(window.size, (80, 24));
        assert!(window.active);
    }

    #[test]
    fn snapshot_round_trips_split_layout() {
        let mut manager = WindowManager::new(0, (80, 26));
        manager.split_vertical(1).unwrap();
        manager.active_window_mut().unwrap().vtop = 12;

        let snapshot = manager.snapshot();
        let original_ids = manager
            .windows()
            .into_iter()
            .map(|window| window.id)
            .collect::<Vec<_>>();
        let buffer_map = HashMap::from([(0, 3), (1, 4)]);
        let restored = WindowManager::from_snapshot(&snapshot, (100, 30), &buffer_map).unwrap();

        assert_eq!(restored.windows().len(), 2);
        assert_eq!(restored.active_window_id(), manager.active_window_id());
        assert_eq!(restored.active_window().unwrap().buffer_index, 4);
        assert_eq!(restored.active_window().unwrap().vtop, 12);
        assert!(restored
            .windows()
            .into_iter()
            .all(|window| !original_ids.contains(&window.id)));
    }

    #[test]
    fn indexed_window_access_matches_tree_order() {
        let mut manager = WindowManager::new(0, (80, 26));
        manager.split_vertical(1).unwrap();
        manager.set_active(0);
        manager.split_horizontal(2).unwrap();

        let windows = manager.windows();
        assert_eq!(windows.len(), 3);
        assert_eq!(manager.window_count(), windows.len());
        for (index, window) in windows.iter().enumerate() {
            assert_eq!(
                manager.window_at_index(index).map(|found| found.id),
                Some(window.id)
            );
            assert_eq!(manager.window_index(window.id), Some(index));
            assert_eq!(
                manager.window(window.id).map(|found| found.id),
                Some(window.id)
            );
        }
        assert!(manager.window_at_index(windows.len()).is_none());
    }

    fn nested_window_manager() -> WindowManager {
        let mut manager = WindowManager::new(0, (80, 26));
        manager.split_vertical(1).unwrap();
        manager.split_horizontal(2).unwrap();
        manager.set_active(0);
        manager.split_horizontal(3).unwrap();

        let Split::Vertical { right, .. } = &mut manager.root else {
            panic!("expected a vertical outer split");
        };
        let Split::Horizontal { ratio, .. } = right.as_mut() else {
            panic!("expected a horizontal right-hand split");
        };
        *ratio = 0.3;
        manager.resize((80, 26));
        manager
    }

    fn contains_split_ratio(split: &Split, expected: f32) -> bool {
        match split {
            Split::Window(_) => false,
            Split::Horizontal { top, bottom, ratio } => {
                (*ratio - expected).abs() < f32::EPSILON
                    || contains_split_ratio(top, expected)
                    || contains_split_ratio(bottom, expected)
            }
            Split::Vertical { left, right, ratio } => {
                (*ratio - expected).abs() < f32::EPSILON
                    || contains_split_ratio(left, expected)
                    || contains_split_ratio(right, expected)
            }
        }
    }

    #[test]
    fn move_window_to_each_edge_preserves_identity_state_and_unaffected_ratios() {
        for direction in [
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
        ] {
            let mut manager = nested_window_manager();
            let original_ids = manager
                .windows()
                .into_iter()
                .map(|window| window.id)
                .collect::<Vec<_>>();
            let window = manager.active_window_mut().unwrap();
            window.vtop = 7;
            window.vleft = 4;
            window.skipcol = 3;
            window.wrap = false;
            window.cx = 9;
            window.cy = 2;
            window.cursor_goal = CursorGoal::DisplayCol(11);
            window.vx = 5;
            let original_id = window.id;

            assert!(manager.move_window_to_edge(direction).is_some());

            let moved = manager.active_window().unwrap();
            assert_eq!(moved.id, original_id);
            assert_eq!(moved.buffer_index, 3);
            assert_eq!(moved.vtop, 7);
            assert_eq!(moved.vleft, 4);
            assert_eq!(moved.skipcol, 3);
            assert!(!moved.wrap);
            assert_eq!(moved.cx, 9);
            assert_eq!(moved.cy, 2);
            assert_eq!(moved.cursor_goal, CursorGoal::DisplayCol(11));
            assert_eq!(moved.vx, 5);
            assert!(moved.active);

            match direction {
                Direction::Left => {
                    assert_eq!(moved.position, Point::new(0, 0));
                    assert_eq!(moved.size, (39, 24));
                }
                Direction::Right => {
                    assert_eq!(moved.position, Point::new(40, 0));
                    assert_eq!(moved.size, (40, 24));
                }
                Direction::Up => {
                    assert_eq!(moved.position, Point::new(0, 0));
                    assert_eq!(moved.size, (80, 11));
                }
                Direction::Down => {
                    assert_eq!(moved.position, Point::new(0, 12));
                    assert_eq!(moved.size, (80, 12));
                }
            }

            let mut remaining_ids = manager
                .windows()
                .into_iter()
                .map(|window| window.id)
                .collect::<Vec<_>>();
            let mut expected_ids = original_ids;
            remaining_ids.sort_unstable();
            expected_ids.sort_unstable();
            assert_eq!(remaining_ids, expected_ids);
            assert_eq!(manager.window_count(), 4);
            assert!(contains_split_ratio(&manager.root, 0.3));
            assert_eq!(
                manager.window_index(original_id),
                Some(manager.active_window_id())
            );
        }
    }

    #[test]
    fn moving_a_single_window_to_any_edge_is_a_no_op() {
        for direction in [
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
        ] {
            let mut manager = WindowManager::new(0, (80, 26));
            let before = manager.snapshot();

            assert!(manager.move_window_to_edge(direction).is_none());
            assert_eq!(manager.snapshot(), before);
        }
    }

    #[test]
    fn moving_a_window_already_at_the_full_edge_is_a_no_op() {
        for direction in [
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
        ] {
            let mut manager = nested_window_manager();
            assert!(manager.move_window_to_edge(direction).is_some());
            let before = manager.snapshot();

            assert!(manager.move_window_to_edge(direction).is_none());
            assert_eq!(manager.snapshot(), before);
        }
    }

    #[test]
    fn move_window_to_edge_preserves_nonzero_layout_origin() {
        let mut manager = nested_window_manager();
        manager.resize_with_origin(Point::new(20, 2), (60, 28));

        assert!(manager.move_window_to_edge(Direction::Left).is_some());

        let moved = manager.active_window().unwrap();
        assert_eq!(moved.position, Point::new(20, 2));
        assert_eq!(moved.size, (29, 26));
        assert!(manager.windows().into_iter().all(|window| {
            window.position.x >= 20
                && window.position.x + window.size.0 <= 80
                && window.position.y >= 2
                && window.position.y + window.size.1 <= 28
        }));
    }

    #[test]
    fn moved_window_layout_round_trips_through_snapshot() {
        let mut manager = nested_window_manager();
        manager.active_window_mut().unwrap().vtop = 12;
        manager.move_window_to_edge(Direction::Right).unwrap();
        let snapshot = manager.snapshot();
        let buffer_map = HashMap::from([(0, 0), (1, 1), (2, 2), (3, 3)]);

        let restored = WindowManager::from_snapshot(&snapshot, (80, 26), &buffer_map).unwrap();

        assert_eq!(restored.snapshot(), snapshot);
        assert_eq!(restored.active_window().unwrap().buffer_index, 3);
        assert_eq!(restored.active_window().unwrap().vtop, 12);
        assert!(contains_split_ratio(&restored.root, 0.3));
    }

    #[test]
    fn moving_windows_in_a_tiny_layout_does_not_panic() {
        let mut manager = WindowManager::new(0, (2, 3));
        manager.split_vertical(1).unwrap();

        assert!(manager.move_window_to_edge(Direction::Up).is_some());
        assert_eq!(manager.window_count(), 2);
        assert!(manager.active_window().unwrap().active);
    }
}

/// Represents a split in the window layout
#[derive(Debug, Clone)]
pub enum Split {
    /// A leaf node containing a window
    Window(Window),

    /// A horizontal split (top/bottom)
    Horizontal {
        /// Upper subtree.
        top: Box<Split>,
        /// Lower subtree.
        bottom: Box<Split>,
        /// Position of the split (0.0 = top, 1.0 = bottom)
        ratio: f32,
    },

    /// A vertical split (left/right)
    Vertical {
        /// Left subtree.
        left: Box<Split>,
        /// Right subtree.
        right: Box<Split>,
        /// Position of the split (0.0 = left, 1.0 = right)
        ratio: f32,
    },
}

/// Serializable split topology and per-leaf viewport state.
///
/// Stable window IDs are rebuilt on restore; `active_window_id` in the manager snapshot
/// refers to tree order for backward compatibility.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SplitSnapshot {
    /// One leaf displaying a saved buffer index.
    Window {
        /// Buffer index in the snapshot's buffer table.
        #[serde(alias = "bufferIndex")]
        buffer_index: usize,
        /// First visible buffer line.
        vtop: usize,
        /// First visible display column in non-wrapping mode.
        vleft: usize,
        /// Display columns skipped within a wrapped logical line.
        #[serde(default)]
        skipcol: usize,
        /// Whether long lines wrap.
        #[serde(default = "default_wrap")]
        wrap: bool,
        /// Grapheme cursor index.
        cx: usize,
        /// Cursor row relative to `vtop`.
        cy: usize,
        /// Legacy viewport x offset.
        vx: usize,
    },
    /// Top-and-bottom child snapshots.
    Horizontal {
        /// Relative portion assigned to the top subtree.
        ratio: f32,
        /// Upper subtree.
        top: Box<SplitSnapshot>,
        /// Lower subtree.
        bottom: Box<SplitSnapshot>,
    },
    /// Left-and-right child snapshots.
    Vertical {
        /// Relative portion assigned to the left subtree.
        ratio: f32,
        /// Left subtree.
        left: Box<SplitSnapshot>,
        /// Right subtree.
        right: Box<SplitSnapshot>,
    },
}

fn default_wrap() -> bool {
    true
}

/// Serializable window tree plus the active tree-order index.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WindowManagerSnapshot {
    /// Active window's tree-order index at capture time.
    #[serde(alias = "activeWindowId")]
    pub active_window_id: usize,
    /// Complete saved split topology.
    pub root: SplitSnapshot,
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

    /// Removes a leaf without recreating windows or changing surviving split ratios.
    fn detach_window(self, target_id: WindowId) -> Result<(Option<Self>, Window), Box<Self>> {
        match self {
            Self::Window(window) => {
                if window.id == target_id {
                    Ok((None, window))
                } else {
                    Err(Box::new(Self::Window(window)))
                }
            }
            Self::Horizontal { top, bottom, ratio } => match (*top).detach_window(target_id) {
                Ok((Some(top), window)) => Ok((
                    Some(Self::Horizontal {
                        top: Box::new(top),
                        bottom,
                        ratio,
                    }),
                    window,
                )),
                Ok((None, window)) => Ok((Some(*bottom), window)),
                Err(top) => match (*bottom).detach_window(target_id) {
                    Ok((Some(bottom), window)) => Ok((
                        Some(Self::Horizontal {
                            top,
                            bottom: Box::new(bottom),
                            ratio,
                        }),
                        window,
                    )),
                    Ok((None, window)) => Ok((Some(*top), window)),
                    Err(bottom) => Err(Box::new(Self::Horizontal { top, bottom, ratio })),
                },
            },
            Self::Vertical { left, right, ratio } => match (*left).detach_window(target_id) {
                Ok((Some(left), window)) => Ok((
                    Some(Self::Vertical {
                        left: Box::new(left),
                        right,
                        ratio,
                    }),
                    window,
                )),
                Ok((None, window)) => Ok((Some(*right), window)),
                Err(left) => match (*right).detach_window(target_id) {
                    Ok((Some(right), window)) => Ok((
                        Some(Self::Vertical {
                            left,
                            right: Box::new(right),
                            ratio,
                        }),
                        window,
                    )),
                    Ok((None, window)) => Ok((Some(*left), window)),
                    Err(right) => Err(Box::new(Self::Vertical { left, right, ratio })),
                },
            },
        }
    }

    fn snapshot(&self) -> SplitSnapshot {
        match self {
            Split::Window(window) => SplitSnapshot::Window {
                buffer_index: window.buffer_index,
                vtop: window.vtop,
                vleft: window.vleft,
                skipcol: window.skipcol,
                wrap: window.wrap,
                cx: window.cx,
                cy: window.cy,
                vx: window.vx,
            },
            Split::Horizontal { top, bottom, ratio } => SplitSnapshot::Horizontal {
                ratio: *ratio,
                top: Box::new(top.snapshot()),
                bottom: Box::new(bottom.snapshot()),
            },
            Split::Vertical { left, right, ratio } => SplitSnapshot::Vertical {
                ratio: *ratio,
                left: Box::new(left.snapshot()),
                right: Box::new(right.snapshot()),
            },
        }
    }

    fn from_snapshot(snapshot: &SplitSnapshot, buffer_map: &HashMap<usize, usize>) -> Option<Self> {
        match snapshot {
            SplitSnapshot::Window {
                buffer_index,
                vtop,
                vleft,
                skipcol,
                wrap,
                cx,
                cy,
                vx,
            } => {
                let mapped_buffer = *buffer_map.get(buffer_index)?;
                let mut window = Window::new(mapped_buffer, Point::new(0, 0), (0, 0));
                window.vtop = *vtop;
                window.vleft = *vleft;
                window.skipcol = *skipcol;
                window.wrap = *wrap;
                window.cx = *cx;
                window.cy = *cy;
                window.vx = *vx;
                Some(Split::Window(window))
            }
            SplitSnapshot::Horizontal { top, bottom, ratio } => Some(Split::Horizontal {
                ratio: *ratio,
                top: Box::new(Self::from_snapshot(top, buffer_map)?),
                bottom: Box::new(Self::from_snapshot(bottom, buffer_map)?),
            }),
            SplitSnapshot::Vertical { left, right, ratio } => Some(Split::Vertical {
                ratio: *ratio,
                left: Box::new(Self::from_snapshot(left, buffer_map)?),
                right: Box::new(Self::from_snapshot(right, buffer_map)?),
            }),
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
        let mut root = Split::Window(Window::new_with_id(
            WindowId::next(),
            buffer_index,
            Point::new(0, 0),
            (terminal_size.0, terminal_size.1.saturating_sub(2)), // Leave room for status/command line
        ));

        // Set the first window as active
        if let Split::Window(w) = &mut root {
            w.active = true;
        }

        Self {
            root,
            active_window_id: 0,
        }
    }

    /// Captures split topology and per-window viewport state.
    pub fn snapshot(&self) -> WindowManagerSnapshot {
        WindowManagerSnapshot {
            active_window_id: self.active_window_id,
            root: self.root.snapshot(),
        }
    }

    /// Restores a window manager after remapping saved buffer indexes.
    ///
    /// Returns `None` when every saved leaf refers to a buffer that was not restored.
    /// Terminal geometry is recomputed rather than trusted from the snapshot.
    pub fn from_snapshot(
        snapshot: &WindowManagerSnapshot,
        terminal_size: (usize, usize),
        buffer_map: &HashMap<usize, usize>,
    ) -> Option<Self> {
        let mut root = Split::from_snapshot(&snapshot.root, buffer_map)?;
        root.layout(
            Point::new(0, 0),
            (terminal_size.0, terminal_size.1.saturating_sub(2)),
        );

        let mut manager = Self {
            root,
            active_window_id: 0,
        };
        let window_count = manager.root.windows().len();
        if window_count == 0 {
            return None;
        }
        for window in manager.root.windows_mut() {
            window.id = WindowId::next();
        }
        manager.set_active(snapshot.active_window_id.min(window_count - 1));
        Some(manager)
    }

    /// Returns the currently active window
    pub fn active_window(&self) -> Option<&Window> {
        self.window_at_index(self.active_window_id)
    }

    /// Returns the currently active window (mutable)
    pub fn active_window_mut(&mut self) -> Option<&mut Window> {
        let mut current_id = 0;
        Self::get_window_mut_recursive(&mut self.root, &mut current_id, self.active_window_id)
    }

    /// Returns the stable identity of the active window.
    pub fn active_stable_window_id(&self) -> Option<WindowId> {
        self.active_window().map(|window| window.id)
    }

    /// Finds a window by its stable identity.
    pub fn window(&self, id: WindowId) -> Option<&Window> {
        Self::find_window_recursive(&self.root, id, &mut 0).map(|(_, window)| window)
    }

    /// Returns the current tree-order index for a stable window identity.
    pub fn window_index(&self, id: WindowId) -> Option<usize> {
        Self::find_window_recursive(&self.root, id, &mut 0).map(|(index, _)| index)
    }

    /// Returns a window by its current tree-order index.
    pub fn window_at_index(&self, index: usize) -> Option<&Window> {
        Self::get_window_recursive(&self.root, &mut 0, index)
    }

    /// Returns the number of windows without allocating a flattened list.
    pub fn window_count(&self) -> usize {
        Self::count_windows(&self.root)
    }

    fn get_window_recursive<'a>(
        node: &'a Split,
        current_id: &mut usize,
        target_id: usize,
    ) -> Option<&'a Window> {
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
                Self::get_window_recursive(top, current_id, target_id)
                    .or_else(|| Self::get_window_recursive(bottom, current_id, target_id))
            }
            Split::Vertical { left, right, .. } => {
                Self::get_window_recursive(left, current_id, target_id)
                    .or_else(|| Self::get_window_recursive(right, current_id, target_id))
            }
        }
    }

    fn find_window_recursive<'a>(
        node: &'a Split,
        id: WindowId,
        current_id: &mut usize,
    ) -> Option<(usize, &'a Window)> {
        match node {
            Split::Window(window) => {
                let index = *current_id;
                *current_id += 1;
                (window.id == id).then_some((index, window))
            }
            Split::Horizontal { top, bottom, .. } => {
                Self::find_window_recursive(top, id, current_id)
                    .or_else(|| Self::find_window_recursive(bottom, id, current_id))
            }
            Split::Vertical { left, right, .. } => {
                Self::find_window_recursive(left, id, current_id)
                    .or_else(|| Self::find_window_recursive(right, id, current_id))
            }
        }
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
        self.resize_with_origin(Point::new(0, 0), terminal_size);
    }

    /// Recomputes layout under an explicit terminal origin.
    pub fn resize_with_origin(&mut self, position: Point, terminal_size: (usize, usize)) {
        self.root.layout(
            position,
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

    /// Finds the split divider actually painted at a terminal-cell position.
    pub(crate) fn divider_at_position(&self, x: usize, y: usize) -> Option<WindowDivider> {
        let (origin, size) = self.layout_geometry()?;
        let mut path = Vec::new();
        Self::find_divider(&self.root, origin, size, x, y, &mut path)
    }

    /// Resolves a captured divider against the current split-tree geometry.
    pub(crate) fn divider_span(&self, divider: &WindowDivider) -> Option<DividerSpan> {
        let (origin, size) = self.layout_geometry()?;
        Self::find_divider_span(&self.root, origin, size, &divider.path, divider.axis)
    }

    /// Resizes only the captured divider while preserving root origin and focus.
    pub(crate) fn resize_divider(&mut self, divider: &WindowDivider, x: usize, y: usize) -> bool {
        let Some((root_origin, root_size)) = self.layout_geometry() else {
            return false;
        };

        let (available, requested, preferred_minimum) = match divider.axis {
            DividerAxis::Vertical => (
                divider.size.0.saturating_sub(1),
                x.saturating_sub(divider.origin.x),
                10usize,
            ),
            DividerAxis::Horizontal => (
                divider.size.1.saturating_sub(1),
                y.saturating_sub(divider.origin.y),
                3usize,
            ),
        };
        if available < 2 {
            return false;
        }

        let minimum = if available >= preferred_minimum.saturating_mul(2) {
            preferred_minimum
        } else {
            1
        };
        let split = requested.clamp(minimum, available.saturating_sub(minimum));
        let next_ratio = (split as f32 + 0.5) / available as f32;
        let Some(ratio) = Self::divider_ratio_mut(&mut self.root, &divider.path, divider.axis)
        else {
            return false;
        };
        if (*ratio - next_ratio).abs() <= f32::EPSILON {
            return false;
        }

        *ratio = next_ratio;
        self.root.layout(root_origin, root_size);
        self.set_active(self.active_window_id);
        true
    }

    /// Grows or shrinks the active split by terminal cells, independent of its side.
    pub(crate) fn resize_window_by_cells(&mut self, direction: Direction, amount: usize) -> bool {
        if amount == 0 {
            return false;
        }

        let (axis, grow) = match direction {
            Direction::Right => (DividerAxis::Vertical, true),
            Direction::Left => (DividerAxis::Vertical, false),
            Direction::Down => (DividerAxis::Horizontal, true),
            Direction::Up => (DividerAxis::Horizontal, false),
        };
        let Some(window) = self.active_window().cloned() else {
            return false;
        };
        let Some((divider, position, positive_edge)) = self
            .window_divider_on_edge(&window, axis, /*positive_edge*/ true)
            .map(|(divider, position)| (divider, position, true))
            .or_else(|| {
                self.window_divider_on_edge(&window, axis, /*positive_edge*/ false)
                    .map(|(divider, position)| (divider, position, false))
            })
        else {
            return false;
        };

        let toward_higher_coordinates = grow == positive_edge;
        let (x, y) = match axis {
            DividerAxis::Vertical => (
                if toward_higher_coordinates {
                    position.x.saturating_add(amount)
                } else {
                    position.x.saturating_sub(amount)
                },
                position.y,
            ),
            DividerAxis::Horizontal => (
                position.x,
                if toward_higher_coordinates {
                    position.y.saturating_add(amount)
                } else {
                    position.y.saturating_sub(amount)
                },
            ),
        };

        self.resize_divider(&divider, x, y)
    }

    fn window_divider_on_edge(
        &self,
        window: &Window,
        axis: DividerAxis,
        positive_edge: bool,
    ) -> Option<(WindowDivider, Point)> {
        match axis {
            DividerAxis::Vertical => {
                let x = if positive_edge {
                    window.position.x.saturating_add(window.size.0)
                } else {
                    window.position.x.checked_sub(1)?
                };
                let end = window.position.y.saturating_add(window.size.1);
                (window.position.y..end).find_map(|y| {
                    let divider = self.divider_at_position(x, y)?;
                    (divider.axis == axis).then_some((divider, Point::new(x, y)))
                })
            }
            DividerAxis::Horizontal => {
                let y = if positive_edge {
                    window.position.y.saturating_add(window.size.1)
                } else {
                    window.position.y.checked_sub(1)?
                };
                let end = window.position.x.saturating_add(window.size.0);
                (window.position.x..end).find_map(|x| {
                    let divider = self.divider_at_position(x, y)?;
                    (divider.axis == axis).then_some((divider, Point::new(x, y)))
                })
            }
        }
    }

    fn find_divider(
        node: &Split,
        origin: Point,
        size: (usize, usize),
        x: usize,
        y: usize,
        path: &mut Vec<SplitBranch>,
    ) -> Option<WindowDivider> {
        if x < origin.x
            || x >= origin.x.saturating_add(size.0)
            || y < origin.y
            || y >= origin.y.saturating_add(size.1)
        {
            return None;
        }

        match node {
            Split::Window(_) => None,
            Split::Vertical { left, right, ratio } => {
                let available_width = size.0.saturating_sub(1);
                let split_x = (available_width as f32 * *ratio) as usize;
                let separator_x = origin.x.saturating_add(split_x);
                if x == separator_x {
                    return Some(WindowDivider {
                        axis: DividerAxis::Vertical,
                        path: path.clone(),
                        origin,
                        size,
                    });
                }

                let (branch, child, child_origin, child_size) = if x < separator_x {
                    (SplitBranch::First, left.as_ref(), origin, (split_x, size.1))
                } else {
                    (
                        SplitBranch::Second,
                        right.as_ref(),
                        Point::new(separator_x.saturating_add(1), origin.y),
                        (available_width.saturating_sub(split_x), size.1),
                    )
                };
                path.push(branch);
                let found = Self::find_divider(child, child_origin, child_size, x, y, path);
                path.pop();
                found
            }
            Split::Horizontal { top, bottom, ratio } => {
                let available_height = size.1.saturating_sub(1);
                let split_y = (available_height as f32 * *ratio) as usize;
                let separator_y = origin.y.saturating_add(split_y);
                if y == separator_y {
                    return Some(WindowDivider {
                        axis: DividerAxis::Horizontal,
                        path: path.clone(),
                        origin,
                        size,
                    });
                }

                let (branch, child, child_origin, child_size) = if y < separator_y {
                    (SplitBranch::First, top.as_ref(), origin, (size.0, split_y))
                } else {
                    (
                        SplitBranch::Second,
                        bottom.as_ref(),
                        Point::new(origin.x, separator_y.saturating_add(1)),
                        (size.0, available_height.saturating_sub(split_y)),
                    )
                };
                path.push(branch);
                let found = Self::find_divider(child, child_origin, child_size, x, y, path);
                path.pop();
                found
            }
        }
    }

    fn find_divider_span(
        node: &Split,
        origin: Point,
        size: (usize, usize),
        path: &[SplitBranch],
        axis: DividerAxis,
    ) -> Option<DividerSpan> {
        match node {
            Split::Window(_) => None,
            Split::Vertical { left, right, ratio } => {
                let available_width = size.0.saturating_sub(1);
                let split_x = (available_width as f32 * *ratio) as usize;
                let separator_x = origin.x.saturating_add(split_x);

                if let Some((branch, remaining)) = path.split_first() {
                    let (child, child_origin, child_size) = match branch {
                        SplitBranch::First => (left.as_ref(), origin, (split_x, size.1)),
                        SplitBranch::Second => (
                            right.as_ref(),
                            Point::new(separator_x.saturating_add(1), origin.y),
                            (available_width.saturating_sub(split_x), size.1),
                        ),
                    };
                    return Self::find_divider_span(
                        child,
                        child_origin,
                        child_size,
                        remaining,
                        axis,
                    );
                }

                (axis == DividerAxis::Vertical && size.0 > 1 && size.1 > 0).then_some(DividerSpan {
                    axis,
                    origin: Point::new(separator_x, origin.y),
                    length: size.1,
                })
            }
            Split::Horizontal { top, bottom, ratio } => {
                let available_height = size.1.saturating_sub(1);
                let split_y = (available_height as f32 * *ratio) as usize;
                let separator_y = origin.y.saturating_add(split_y);

                if let Some((branch, remaining)) = path.split_first() {
                    let (child, child_origin, child_size) = match branch {
                        SplitBranch::First => (top.as_ref(), origin, (size.0, split_y)),
                        SplitBranch::Second => (
                            bottom.as_ref(),
                            Point::new(origin.x, separator_y.saturating_add(1)),
                            (size.0, available_height.saturating_sub(split_y)),
                        ),
                    };
                    return Self::find_divider_span(
                        child,
                        child_origin,
                        child_size,
                        remaining,
                        axis,
                    );
                }

                (axis == DividerAxis::Horizontal && size.0 > 0 && size.1 > 1).then_some(
                    DividerSpan {
                        axis,
                        origin: Point::new(origin.x, separator_y),
                        length: size.0,
                    },
                )
            }
        }
    }

    fn divider_ratio_mut<'a>(
        node: &'a mut Split,
        path: &[SplitBranch],
        axis: DividerAxis,
    ) -> Option<&'a mut f32> {
        let Some((branch, remaining)) = path.split_first() else {
            return match (node, axis) {
                (Split::Vertical { ratio, .. }, DividerAxis::Vertical)
                | (Split::Horizontal { ratio, .. }, DividerAxis::Horizontal) => Some(ratio),
                _ => None,
            };
        };

        let child = match (node, branch) {
            (Split::Vertical { left, .. }, SplitBranch::First) => left.as_mut(),
            (Split::Vertical { right, .. }, SplitBranch::Second) => right.as_mut(),
            (Split::Horizontal { top, .. }, SplitBranch::First) => top.as_mut(),
            (Split::Horizontal { bottom, .. }, SplitBranch::Second) => bottom.as_mut(),
            (Split::Window(_), _) => return None,
        };
        Self::divider_ratio_mut(child, remaining, axis)
    }

    /// Splits the active window horizontally
    pub fn split_horizontal(&mut self, new_buffer_index: usize) -> Option<()> {
        use crate::log;
        log!(
            "WindowManager::split_horizontal called with buffer {}",
            new_buffer_index
        );

        // Get the current terminal bounds from the root split
        let (origin, (width, height)) = self.layout_geometry()?;
        log!("Terminal bounds: {}x{}", width, height);
        log!("Active window id before split: {}", self.active_window_id);

        let new_window_id = WindowId::next();
        let new_root = self.split_node(
            &self.root,
            self.active_window_id,
            new_window_id,
            new_buffer_index,
            true,
        )?;
        self.root = new_root;
        self.root.layout(origin, (width, height));

        // Update active window to the new window
        let windows = self.root.windows();
        log!("Window count after split: {}", windows.len());

        // The new window should be the bottom one in the split we just created
        // Since we're doing a depth-first traversal, it should be right after the original window
        self.active_window_id += 1;
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
        let (origin, (width, height)) = self.layout_geometry()?;
        log!("Active window id before split: {}", self.active_window_id);

        let new_window_id = WindowId::next();
        let new_root = self.split_node(
            &self.root,
            self.active_window_id,
            new_window_id,
            new_buffer_index,
            false,
        )?;
        self.root = new_root;
        self.root.layout(origin, (width, height));

        // Update active window to the new window
        let windows = self.root.windows();
        log!("Window count after split: {}", windows.len());

        // The new window should be the right one in the split we just created
        // Since we're doing a depth-first traversal, it should be right after the original window
        self.active_window_id += 1;
        self.set_active(self.active_window_id);
        log!("Active window id after split: {}", self.active_window_id);

        Some(())
    }

    /// Moves the active window to the requested full-height or full-width outer edge.
    ///
    /// Returns `None` when there is only one window or the active window already
    /// occupies the requested edge.
    pub fn move_window_to_edge(&mut self, direction: Direction) -> Option<()> {
        let active_window = self.active_window()?;
        let active_id = active_window.id;

        let already_at_edge = match (&self.root, direction) {
            (Split::Window(_), _) => true,
            (Split::Vertical { left, .. }, Direction::Left) => {
                matches!(left.as_ref(), Split::Window(window) if window.id == active_id)
            }
            (Split::Vertical { right, .. }, Direction::Right) => {
                matches!(right.as_ref(), Split::Window(window) if window.id == active_id)
            }
            (Split::Horizontal { top, .. }, Direction::Up) => {
                matches!(top.as_ref(), Split::Window(window) if window.id == active_id)
            }
            (Split::Horizontal { bottom, .. }, Direction::Down) => {
                matches!(bottom.as_ref(), Split::Window(window) if window.id == active_id)
            }
            _ => false,
        };
        if already_at_edge {
            return None;
        }

        let windows = self.root.windows();
        let origin_x = windows.iter().map(|window| window.position.x).min()?;
        let origin_y = windows.iter().map(|window| window.position.y).min()?;
        let max_x = windows
            .iter()
            .map(|window| window.position.x.saturating_add(window.size.0))
            .max()?;
        let max_y = windows
            .iter()
            .map(|window| window.position.y.saturating_add(window.size.1))
            .max()?;
        let origin = Point::new(origin_x, origin_y);
        let size = (
            max_x.saturating_sub(origin_x),
            max_y.saturating_sub(origin_y),
        );

        let placeholder = Split::Window(active_window.clone());
        let root = std::mem::replace(&mut self.root, placeholder);
        let (remaining, window) = match root.detach_window(active_id) {
            Ok((Some(remaining), window)) => (remaining, window),
            Ok((None, window)) => {
                self.root = Split::Window(window);
                return None;
            }
            Err(root) => {
                self.root = *root;
                return None;
            }
        };

        self.root = match direction {
            Direction::Left => Split::Vertical {
                left: Box::new(Split::Window(window)),
                right: Box::new(remaining),
                ratio: 0.5,
            },
            Direction::Right => Split::Vertical {
                left: Box::new(remaining),
                right: Box::new(Split::Window(window)),
                ratio: 0.5,
            },
            Direction::Up => Split::Horizontal {
                top: Box::new(Split::Window(window)),
                bottom: Box::new(remaining),
                ratio: 0.5,
            },
            Direction::Down => Split::Horizontal {
                top: Box::new(remaining),
                bottom: Box::new(Split::Window(window)),
                ratio: 0.5,
            },
        };
        self.root.layout(origin, size);
        self.set_active(self.window_index(active_id)?);
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
        let (origin, (width, height)) = self.layout_geometry()?;

        // Remove the window from the tree
        if let Some(new_root) = self.remove_window(&self.root, self.active_window_id) {
            self.root = new_root;
            self.root.layout(origin, (width, height));

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
        #[allow(clippy::only_used_in_recursion)]
        let _ = &self; // Clippy false positive - we need &self for method access
        match node {
            Split::Window(_) => {
                if *current_id == target_id {
                    // This window should be removed - return None to signal removal
                    *current_id += 1;
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

    /// Resize the active window in the given direction
    pub fn resize_window(&mut self, direction: Direction, amount: usize) -> Option<()> {
        use crate::log;

        // Get the terminal bounds before modification
        let (origin, (width, height)) = self.layout_geometry()?;

        // Find the split containing the active window and adjust its ratio
        let active_id = self.active_window_id;
        let active_window = self.active_window()?;
        let window_info = (
            active_window.position.x,
            active_window.position.y,
            active_window.size.0,
            active_window.size.1,
        );

        log!(
            "Attempting to resize window {} in direction {:?} by {}",
            active_id,
            direction,
            amount
        );
        log!(
            "Active window at ({}, {}) with size {}x{}",
            window_info.0,
            window_info.1,
            window_info.2,
            window_info.3
        );

        if Self::adjust_split_ratio(&mut self.root, active_id, direction, amount, window_info) {
            // Recalculate layout after adjusting ratios
            self.root.layout(origin, (width, height));
            log!(
                "Window resized successfully in direction {:?} by {}",
                direction,
                amount
            );
            Some(())
        } else {
            log!(
                "Could not resize window in direction {:?} - no matching split found",
                direction
            );
            None
        }
    }

    /// Resets every split ratio so windows share space evenly.
    pub fn balance_windows(&mut self) -> Option<()> {
        if self.root.windows().len() <= 1 {
            return None;
        }

        let (origin, (width, height)) = self.layout_geometry()?;
        Self::balance_split(&mut self.root);
        self.root.layout(origin, (width, height));
        self.set_active(self.active_window_id);
        Some(())
    }

    fn balance_split(node: &mut Split) {
        match node {
            Split::Window(_) => {}
            Split::Horizontal { top, bottom, ratio } => {
                *ratio = 0.5;
                Self::balance_split(top);
                Self::balance_split(bottom);
            }
            Split::Vertical { left, right, ratio } => {
                *ratio = 0.5;
                Self::balance_split(left);
                Self::balance_split(right);
            }
        }
    }

    /// Expands the active window along each ancestor split while preserving layout.
    pub fn maximize_window(&mut self) -> Option<()> {
        if self.root.windows().len() <= 1 {
            return None;
        }

        let (origin, (width, height)) = self.layout_geometry()?;
        let mut current_id = 0;
        let maximized =
            Self::maximize_window_recursive(&mut self.root, &mut current_id, self.active_window_id);

        if maximized {
            self.root.layout(origin, (width, height));
            self.set_active(self.active_window_id);
            Some(())
        } else {
            None
        }
    }

    fn maximize_window_recursive(
        node: &mut Split,
        current_id: &mut usize,
        target_id: usize,
    ) -> bool {
        match node {
            Split::Window(_) => {
                let found = *current_id == target_id;
                *current_id += 1;
                found
            }
            Split::Horizontal { top, bottom, ratio } => {
                if Self::maximize_window_recursive(top, current_id, target_id) {
                    *ratio = 0.9;
                    true
                } else if Self::maximize_window_recursive(bottom, current_id, target_id) {
                    *ratio = 0.1;
                    true
                } else {
                    false
                }
            }
            Split::Vertical { left, right, ratio } => {
                if Self::maximize_window_recursive(left, current_id, target_id) {
                    *ratio = 0.9;
                    true
                } else if Self::maximize_window_recursive(right, current_id, target_id) {
                    *ratio = 0.1;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Collapses the layout to just the active window, matching Vim's `:only`.
    pub fn only_window(&mut self) -> Option<()> {
        let window_count = self.root.windows().len();
        if window_count <= 1 {
            return None;
        }

        let mut window = self.active_window()?.clone();
        let (origin, (width, height)) = self.layout_geometry()?;
        window.position = origin;
        window.size = (width, height);
        window.active = true;

        self.root = Split::Window(window);
        self.active_window_id = 0;
        Some(())
    }

    /// Adjust the split ratio for the window in the given direction
    fn adjust_split_ratio(
        node: &mut Split,
        target_id: usize,
        direction: Direction,
        amount: usize,
        _window_info: (usize, usize, usize, usize),
    ) -> bool {
        let mut current_id = 0;
        Self::adjust_split_ratio_recursive(node, &mut current_id, target_id, direction, amount)
    }

    fn adjust_split_ratio_recursive(
        node: &mut Split,
        current_id: &mut usize,
        target_id: usize,
        direction: Direction,
        amount: usize,
    ) -> bool {
        use crate::log;

        match node {
            Split::Window(_) => {
                *current_id += 1;
                false
            }
            Split::Horizontal { top, bottom, ratio } => {
                log!("Found horizontal split with ratio {}", ratio);

                // Check if target window is in top
                let subtree_start = *current_id;
                let in_top = Self::window_in_subtree_from(top, subtree_start, target_id);

                if in_top {
                    log!(
                        "Target window {} is in top half of horizontal split",
                        target_id
                    );
                    // Target is in top, check if we should adjust this split
                    match direction {
                        Direction::Down => {
                            // User wants to expand window downward, increase top size
                            let new_ratio = (*ratio + amount as f32 * 0.05).min(0.9);
                            log!("Expanding top window downward: {} -> {}", ratio, new_ratio);
                            *ratio = new_ratio;
                            return true; // Successfully adjusted
                        }
                        Direction::Up => {
                            // User wants to shrink window upward, decrease top size
                            let new_ratio = (*ratio - amount as f32 * 0.05).max(0.1);
                            log!("Shrinking top window upward: {} -> {}", ratio, new_ratio);
                            *ratio = new_ratio;
                            return true; // Successfully adjusted
                        }
                        _ => {
                            log!("Direction {:?} doesn't apply to horizontal split, searching subtree", direction);
                            // Try to adjust within the top subtree
                            let mut child_id = subtree_start;
                            return Self::adjust_split_ratio_recursive(
                                top,
                                &mut child_id,
                                target_id,
                                direction,
                                amount,
                            );
                        }
                    }
                }

                *current_id = subtree_start + Self::count_windows(top);

                let bottom_start = *current_id;
                let in_bottom = Self::window_in_subtree_from(bottom, bottom_start, target_id);

                if in_bottom {
                    // Target is in bottom, check if we should adjust this split
                    match direction {
                        Direction::Up => {
                            // User wants to expand window upward, decrease top size (increase bottom)
                            *ratio = (*ratio - amount as f32 * 0.05).max(0.1);
                            return true; // Successfully adjusted
                        }
                        Direction::Down => {
                            // User wants to shrink window downward, increase top size (decrease bottom)
                            *ratio = (*ratio + amount as f32 * 0.05).min(0.9);
                            return true; // Successfully adjusted
                        }
                        _ => {
                            // Try to adjust within the bottom subtree
                            let mut child_id = bottom_start;
                            return Self::adjust_split_ratio_recursive(
                                bottom,
                                &mut child_id,
                                target_id,
                                direction,
                                amount,
                            );
                        }
                    }
                }

                *current_id = bottom_start + Self::count_windows(bottom);
                false
            }
            Split::Vertical { left, right, ratio } => {
                log!("Found vertical split with ratio {}", ratio);

                // Check if target window is in left
                let subtree_start = *current_id;
                let in_left = Self::window_in_subtree_from(left, subtree_start, target_id);

                if in_left {
                    log!(
                        "Target window {} is in left half of vertical split",
                        target_id
                    );
                    // Target is in left, check if we should adjust this split
                    match direction {
                        Direction::Right => {
                            // User wants to expand window rightward, increase left size
                            let new_ratio = (*ratio + amount as f32 * 0.05).min(0.9);
                            log!(
                                "Expanding left window rightward: {} -> {}",
                                ratio,
                                new_ratio
                            );
                            *ratio = new_ratio;
                            return true; // Successfully adjusted
                        }
                        Direction::Left => {
                            // User wants to shrink window leftward, decrease left size
                            let new_ratio = (*ratio - amount as f32 * 0.05).max(0.1);
                            log!("Shrinking left window leftward: {} -> {}", ratio, new_ratio);
                            *ratio = new_ratio;
                            return true; // Successfully adjusted
                        }
                        _ => {
                            log!(
                                "Direction {:?} doesn't apply to vertical split, searching subtree",
                                direction
                            );
                            // Try to adjust within the left subtree
                            let mut child_id = subtree_start;
                            return Self::adjust_split_ratio_recursive(
                                left,
                                &mut child_id,
                                target_id,
                                direction,
                                amount,
                            );
                        }
                    }
                }

                *current_id = subtree_start + Self::count_windows(left);

                let right_start = *current_id;
                let in_right = Self::window_in_subtree_from(right, right_start, target_id);

                if in_right {
                    // Target is in right, check if we should adjust this split
                    match direction {
                        Direction::Left => {
                            // User wants to expand window leftward, decrease left size (increase right)
                            *ratio = (*ratio - amount as f32 * 0.05).max(0.1);
                            return true; // Successfully adjusted
                        }
                        Direction::Right => {
                            // User wants to shrink window rightward, increase left size (decrease right)
                            *ratio = (*ratio + amount as f32 * 0.05).min(0.9);
                            return true; // Successfully adjusted
                        }
                        _ => {
                            // Try to adjust within the right subtree
                            let mut child_id = right_start;
                            return Self::adjust_split_ratio_recursive(
                                right,
                                &mut child_id,
                                target_id,
                                direction,
                                amount,
                            );
                        }
                    }
                }

                *current_id = right_start + Self::count_windows(right);
                false
            }
        }
    }

    fn count_windows(node: &Split) -> usize {
        match node {
            Split::Window(_) => 1,
            Split::Horizontal { top, bottom, .. }
            | Split::Vertical {
                left: top,
                right: bottom,
                ..
            } => Self::count_windows(top) + Self::count_windows(bottom),
        }
    }

    fn window_in_subtree_from(node: &Split, start_id: usize, target_id: usize) -> bool {
        let mut current_id = start_id;
        Self::window_in_subtree(node, &mut current_id, target_id)
    }

    /// Check if a window with the given ID is in the subtree
    fn window_in_subtree(node: &Split, current_id: &mut usize, target_id: usize) -> bool {
        match node {
            Split::Window(_) => {
                let found = *current_id == target_id;
                *current_id += 1;
                found
            }
            Split::Horizontal { top, bottom, .. } => {
                if Self::window_in_subtree(top, current_id, target_id) {
                    return true;
                }
                Self::window_in_subtree(bottom, current_id, target_id)
            }
            Split::Vertical { left, right, .. } => {
                if Self::window_in_subtree(left, current_id, target_id) {
                    return true;
                }
                Self::window_in_subtree(right, current_id, target_id)
            }
        }
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

    /// Returns the current editor origin and dimensions without including docked panes.
    fn layout_geometry(&self) -> Option<(Point, (usize, usize))> {
        let windows = self.root.windows();
        if windows.is_empty() {
            return None;
        }

        let mut min_x = usize::MAX;
        let mut min_y = usize::MAX;
        let mut max_x = 0;
        let mut max_y = 0;

        for window in windows {
            min_x = min_x.min(window.position.x);
            min_y = min_y.min(window.position.y);
            max_x = max_x.max(window.position.x.saturating_add(window.size.0));
            max_y = max_y.max(window.position.y.saturating_add(window.size.1));
        }

        Some((
            Point::new(min_x, min_y),
            (max_x.saturating_sub(min_x), max_y.saturating_sub(min_y)),
        ))
    }

    /// Helper method to split a node in the tree
    fn split_node(
        &self,
        node: &Split,
        target_window_id: usize,
        new_window_id: WindowId,
        new_buffer_index: usize,
        horizontal: bool,
    ) -> Option<Split> {
        let mut current_id = 0;
        self.split_node_recursive(
            node,
            &mut current_id,
            target_window_id,
            new_window_id,
            new_buffer_index,
            horizontal,
        )
    }

    fn split_node_recursive(
        &self,
        node: &Split,
        current_id: &mut usize,
        target_window_id: usize,
        new_window_id: WindowId,
        new_buffer_index: usize,
        horizontal: bool,
    ) -> Option<Split> {
        #[allow(clippy::only_used_in_recursion)]
        let _ = &self; // Clippy false positive - we need &self for method access
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
                    *current_id += 1;
                    // This is the window to split
                    let mut new_window = Window::new_with_id(
                        new_window_id,
                        new_buffer_index,
                        window.position,
                        window.size,
                    );
                    new_window.active = false;
                    new_window.wrap = window.wrap;
                    new_window.jump_list = window.jump_list.clone();

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
                    new_window_id,
                    new_buffer_index,
                    horizontal,
                )?;
                let new_bottom = self.split_node_recursive(
                    bottom,
                    current_id,
                    target_window_id,
                    new_window_id,
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
                    new_window_id,
                    new_buffer_index,
                    horizontal,
                )?;
                let new_right = self.split_node_recursive(
                    right,
                    current_id,
                    target_window_id,
                    new_window_id,
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
