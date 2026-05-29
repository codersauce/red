use crate::{editor::Point, theme::Style};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

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

/// Identifies a plugin-owned split-tree window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginWindowId {
    pub plugin: String,
    pub window: String,
}

impl PluginWindowId {
    pub fn new(plugin: impl Into<String>, window: impl Into<String>) -> Self {
        Self {
            plugin: plugin.into(),
            window: window.into(),
        }
    }
}

/// Represents a plugin-owned window in the split tree.
#[derive(Debug, Clone)]
pub struct PluginWindow {
    pub id: PluginWindowId,
    pub title: Option<String>,
    pub render_state: Option<PluginWindowRenderState>,
    pub position: Point,
    pub size: (usize, usize),
    pub active: bool,
}

impl PluginWindow {
    pub fn new(
        id: PluginWindowId,
        title: Option<String>,
        position: Point,
        size: (usize, usize),
    ) -> Self {
        Self {
            id,
            title,
            render_state: None,
            position,
            size,
            active: false,
        }
    }

    pub fn contains_position(&self, x: usize, y: usize) -> bool {
        x >= self.position.x
            && x < self.position.x + self.size.0
            && y >= self.position.y
            && y < self.position.y + self.size.1
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginWindowRenderState {
    #[serde(default)]
    pub kind: PluginWindowContentKind,
    #[serde(default)]
    pub input_mode: PluginWindowInputMode,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub transcript: Vec<PluginWindowLine>,
    #[serde(default)]
    pub composer: Vec<PluginWindowLine>,
    #[serde(default)]
    pub composer_cursor: Option<PluginWindowCursor>,
    #[serde(default)]
    pub composer_selection: Option<PluginWindowSelection>,
    #[serde(default)]
    pub context_placeholders: Vec<PluginWindowContextPlaceholder>,
    #[serde(default)]
    pub scroll: usize,
    #[serde(default)]
    pub key_hints: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginWindowInputMode {
    #[default]
    Normal,
    Insert,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginWindowContentKind {
    #[default]
    Chat,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginWindowLine {
    pub text: String,
    #[serde(default)]
    pub role: Option<PluginWindowLineRole>,
    #[serde(default)]
    pub style: Option<Style>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginWindowLineRole {
    Default,
    Muted,
    User,
    Assistant,
    System,
    Success,
    Error,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginWindowCursor {
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginWindowSelection {
    pub start_line: usize,
    pub start_column: usize,
    pub end_line: usize,
    pub end_column: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginWindowContextPlaceholder {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowLeafKind {
    Editor,
    Plugin,
}

#[derive(Debug, Clone, Copy)]
pub struct WindowLeaf {
    pub kind: WindowLeafKind,
    pub position: Point,
    pub size: (usize, usize),
}

impl WindowLeaf {
    pub fn contains_position(&self, x: usize, y: usize) -> bool {
        x >= self.position.x
            && x < self.position.x + self.size.0
            && y >= self.position.y
            && y < self.position.y + self.size.1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn close_first_window_in_split_keeps_sibling() {
        let mut manager = WindowManager::new(0, (80, 26));
        manager.split_vertical(0).unwrap();
        manager.set_active(0);

        assert!(manager.close_window().is_some());

        assert_eq!(manager.windows().len(), 1);
        assert_eq!(manager.active_window_id(), 0);
    }

    #[test]
    fn snapshot_round_trips_split_layout() {
        let mut manager = WindowManager::new(0, (80, 26));
        manager.split_vertical(1).unwrap();
        manager.active_window_mut().unwrap().vtop = 12;

        let snapshot = manager.snapshot();
        let buffer_map = HashMap::from([(0, 3), (1, 4)]);
        let restored = WindowManager::from_snapshot(&snapshot, (100, 30), &buffer_map).unwrap();

        assert_eq!(restored.windows().len(), 2);
        assert_eq!(restored.active_window_id(), manager.active_window_id());
        assert_eq!(restored.active_window().unwrap().buffer_index, 4);
        assert_eq!(restored.active_window().unwrap().vtop, 12);
    }

    #[test]
    fn plugin_window_participates_in_split_layout() {
        let mut manager = WindowManager::new(0, (100, 30));
        manager
            .split_vertical_plugin(
                PluginWindowId::new("codex", "chat"),
                Some("Codex".to_string()),
            )
            .unwrap();

        assert_eq!(manager.leaf_count(), 2);
        assert_eq!(manager.windows().len(), 1);
        assert_eq!(manager.plugin_windows().len(), 1);
        assert_eq!(manager.active_leaf_kind(), Some(WindowLeafKind::Plugin));

        let editor = manager.windows()[0];
        let plugin = manager.plugin_windows()[0];
        assert_eq!(editor.position.x, 0);
        assert!(plugin.position.x > editor.position.x);
        assert_eq!(plugin.id, PluginWindowId::new("codex", "chat"));
    }

    #[test]
    fn snapshot_round_trips_plugin_window_placeholder() {
        let mut manager = WindowManager::new(0, (100, 30));
        manager
            .split_vertical_plugin(
                PluginWindowId::new("codex", "chat"),
                Some("Codex".to_string()),
            )
            .unwrap();

        let snapshot = manager.snapshot();
        let restored =
            WindowManager::from_snapshot(&snapshot, (100, 30), &HashMap::from([(0, 0)])).unwrap();

        assert_eq!(restored.leaf_count(), 2);
        assert_eq!(restored.windows().len(), 1);
        assert_eq!(restored.plugin_windows().len(), 1);
        assert_eq!(restored.active_leaf_kind(), Some(WindowLeafKind::Plugin));
        assert_eq!(
            restored.plugin_windows()[0].id,
            PluginWindowId::new("codex", "chat")
        );
    }

    #[test]
    fn marks_restored_windows_for_unavailable_plugins() {
        let mut manager = WindowManager::new(0, (100, 30));
        manager
            .split_vertical_plugin(
                PluginWindowId::new("missing", "chat"),
                Some("Missing Chat".to_string()),
            )
            .unwrap();

        let marked = manager.mark_unavailable_plugin_windows(["codex"].into_iter());

        assert_eq!(marked, 1);
        let render_state = manager.plugin_windows()[0].render_state.as_ref().unwrap();
        assert_eq!(render_state.title.as_deref(), Some("Missing Chat"));
        assert_eq!(render_state.status.as_deref(), Some("plugin unavailable"));
        assert_eq!(
            render_state.transcript[0].text,
            "Plugin `missing` is not available."
        );
    }

    #[test]
    fn leaves_available_plugin_windows_for_plugin_hydration() {
        let mut manager = WindowManager::new(0, (100, 30));
        manager
            .split_vertical_plugin(
                PluginWindowId::new("codex", "chat"),
                Some("Codex".to_string()),
            )
            .unwrap();

        let marked = manager.mark_unavailable_plugin_windows(["codex"].into_iter());

        assert_eq!(marked, 0);
        assert!(manager.plugin_windows()[0].render_state.is_none());
    }

    #[test]
    fn updates_plugin_window_render_state() {
        let mut manager = WindowManager::new(0, (100, 30));
        let id = PluginWindowId::new("codex", "chat");
        manager
            .split_vertical_plugin(id.clone(), Some("Codex".to_string()))
            .unwrap();

        let updated = manager.update_plugin_window(
            &id,
            PluginWindowRenderState {
                status: Some("idle".to_string()),
                transcript: vec![PluginWindowLine {
                    text: "assistant response".to_string(),
                    ..PluginWindowLine::default()
                }],
                composer: vec![PluginWindowLine {
                    text: "next prompt".to_string(),
                    ..PluginWindowLine::default()
                }],
                key_hints: vec!["Enter send".to_string()],
                ..PluginWindowRenderState::default()
            },
        );

        assert!(updated);
        let render_state = manager.plugin_windows()[0].render_state.as_ref().unwrap();
        assert_eq!(render_state.status.as_deref(), Some("idle"));
        assert_eq!(render_state.transcript[0].text, "assistant response");
        assert_eq!(render_state.composer[0].text, "next prompt");
    }

    #[test]
    fn deserializes_styled_transcript_line_from_plugin_json() {
        // Locks the JSON contract used by plugins/codex.ts: op_update_plugin_window
        // feeds this straight into PluginWindowRenderState with `?`, so the style
        // shape must match the editor's Style (externally-tagged Color enum), NOT
        // the `fg: string` shape advertised in types/red.d.ts.
        let json = r#"{
            "kind": "chat",
            "transcript": [
                {
                    "text": "You",
                    "style": {
                        "fg": { "Rgb": { "r": 136, "g": 192, "b": 208 } },
                        "bold": true,
                        "italic": false
                    }
                },
                { "text": "hi" }
            ]
        }"#;

        let state: PluginWindowRenderState =
            serde_json::from_str(json).expect("styled transcript line must deserialize");
        let styled = state.transcript[0]
            .style
            .as_ref()
            .expect("role label carries a style");
        assert!(styled.bold);
        assert_eq!(
            styled.fg,
            Some(crate::color::Color::Rgb {
                r: 136,
                g: 192,
                b: 208
            })
        );
        // Unstyled body line falls back to None (renderer substitutes the theme style).
        assert!(state.transcript[1].style.is_none());
    }

    #[test]
    fn deserializes_plugin_window_line_role_from_plugin_json() {
        let json = r#"{
            "kind": "chat",
            "transcript": [
                { "text": "› hi", "role": "user" },
                { "text": "• hello", "role": "assistant" },
                { "text": "muted", "role": "muted" }
            ]
        }"#;

        let state: PluginWindowRenderState =
            serde_json::from_str(json).expect("semantic line roles must deserialize");
        assert_eq!(state.transcript[0].role, Some(PluginWindowLineRole::User));
        assert_eq!(
            state.transcript[1].role,
            Some(PluginWindowLineRole::Assistant)
        );
        assert_eq!(state.transcript[2].role, Some(PluginWindowLineRole::Muted));
    }

    #[test]
    fn deserializes_plugin_window_input_mode_from_plugin_json() {
        let json = r#"{
            "kind": "chat",
            "inputMode": "insert",
            "composer": [{ "text": "draft" }]
        }"#;

        let state: PluginWindowRenderState =
            serde_json::from_str(json).expect("plugin input mode must deserialize");
        assert_eq!(state.input_mode, PluginWindowInputMode::Insert);
    }

    #[test]
    fn finds_plugin_leaf_at_terminal_position() {
        let mut manager = WindowManager::new(0, (80, 24));
        manager
            .split_vertical_plugin(
                PluginWindowId::new("codex", "chat"),
                Some("Codex".to_string()),
            )
            .unwrap();

        let (leaf_id, leaf) = manager.leaf_at_position(45, 1).unwrap();

        assert_eq!(leaf_id, 1);
        assert_eq!(leaf.kind, WindowLeafKind::Plugin);
    }
}

/// Represents a split in the window layout
#[derive(Debug, Clone)]
pub enum Split {
    /// A leaf node containing a window
    Window(Window),

    /// A leaf node containing plugin-owned UI
    PluginWindow(PluginWindow),

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SplitSnapshot {
    Window {
        buffer_index: usize,
        vtop: usize,
        vleft: usize,
        cx: usize,
        cy: usize,
        vx: usize,
    },
    PluginWindow {
        plugin: String,
        window: String,
        title: Option<String>,
    },
    Horizontal {
        ratio: f32,
        top: Box<SplitSnapshot>,
        bottom: Box<SplitSnapshot>,
    },
    Vertical {
        ratio: f32,
        left: Box<SplitSnapshot>,
        right: Box<SplitSnapshot>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WindowManagerSnapshot {
    pub active_window_id: usize,
    pub root: SplitSnapshot,
}

impl Split {
    /// Creates a new window split
    pub fn new_window(buffer_index: usize, position: Point, size: (usize, usize)) -> Self {
        Split::Window(Window::new(buffer_index, position, size))
    }

    pub fn new_plugin_window(
        id: PluginWindowId,
        title: Option<String>,
        position: Point,
        size: (usize, usize),
    ) -> Self {
        Split::PluginWindow(PluginWindow::new(id, title, position, size))
    }

    /// Recursively finds all windows in the split tree
    pub fn windows(&self) -> Vec<&Window> {
        match self {
            Split::Window(w) => vec![w],
            Split::PluginWindow(_) => Vec::new(),
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
            Split::PluginWindow(_) => Vec::new(),
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

    pub fn plugin_windows(&self) -> Vec<&PluginWindow> {
        match self {
            Split::Window(_) => Vec::new(),
            Split::PluginWindow(w) => vec![w],
            Split::Horizontal { top, bottom, .. } => {
                let mut windows = top.plugin_windows();
                windows.extend(bottom.plugin_windows());
                windows
            }
            Split::Vertical { left, right, .. } => {
                let mut windows = left.plugin_windows();
                windows.extend(right.plugin_windows());
                windows
            }
        }
    }

    pub fn leaves(&self) -> Vec<WindowLeaf> {
        match self {
            Split::Window(w) => vec![WindowLeaf {
                kind: WindowLeafKind::Editor,
                position: w.position,
                size: w.size,
            }],
            Split::PluginWindow(w) => vec![WindowLeaf {
                kind: WindowLeafKind::Plugin,
                position: w.position,
                size: w.size,
            }],
            Split::Horizontal { top, bottom, .. } => {
                let mut leaves = top.leaves();
                leaves.extend(bottom.leaves());
                leaves
            }
            Split::Vertical { left, right, .. } => {
                let mut leaves = left.leaves();
                leaves.extend(right.leaves());
                leaves
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
            Split::PluginWindow(w) => {
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

    fn snapshot(&self) -> SplitSnapshot {
        match self {
            Split::Window(window) => SplitSnapshot::Window {
                buffer_index: window.buffer_index,
                vtop: window.vtop,
                vleft: window.vleft,
                cx: window.cx,
                cy: window.cy,
                vx: window.vx,
            },
            Split::PluginWindow(window) => SplitSnapshot::PluginWindow {
                plugin: window.id.plugin.clone(),
                window: window.id.window.clone(),
                title: window.title.clone(),
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
                cx,
                cy,
                vx,
            } => {
                let mapped_buffer = *buffer_map.get(buffer_index)?;
                let mut window = Window::new(mapped_buffer, Point::new(0, 0), (0, 0));
                window.vtop = *vtop;
                window.vleft = *vleft;
                window.cx = *cx;
                window.cy = *cy;
                window.vx = *vx;
                Some(Split::Window(window))
            }
            SplitSnapshot::PluginWindow {
                plugin,
                window,
                title,
            } => Some(Split::new_plugin_window(
                PluginWindowId::new(plugin.clone(), window.clone()),
                title.clone(),
                Point::new(0, 0),
                (0, 0),
            )),
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

#[derive(Clone)]
enum NewLeaf {
    Editor {
        buffer_index: usize,
    },
    Plugin {
        id: PluginWindowId,
        title: Option<String>,
    },
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

    pub fn snapshot(&self) -> WindowManagerSnapshot {
        WindowManagerSnapshot {
            active_window_id: self.active_window_id,
            root: self.root.snapshot(),
        }
    }

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
        let leaf_count = manager.root.leaves().len();
        if window_count == 0 || leaf_count == 0 {
            return None;
        }
        manager.set_active(snapshot.active_window_id.min(leaf_count - 1));
        Some(manager)
    }

    /// Returns the currently active window
    pub fn active_window(&self) -> Option<&Window> {
        let mut current_id = 0;
        Self::get_window_recursive(&self.root, &mut current_id, self.active_window_id)
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
            Split::PluginWindow(_) => {
                *current_id += 1;
                None
            }
            Split::Horizontal { top, bottom, .. } => {
                if let Some(window) = Self::get_window_recursive(top, current_id, target_id) {
                    return Some(window);
                }
                Self::get_window_recursive(bottom, current_id, target_id)
            }
            Split::Vertical { left, right, .. } => {
                if let Some(window) = Self::get_window_recursive(left, current_id, target_id) {
                    return Some(window);
                }
                Self::get_window_recursive(right, current_id, target_id)
            }
        }
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
            Split::PluginWindow(_) => {
                *current_id += 1;
                None
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

    pub fn plugin_windows(&self) -> Vec<&PluginWindow> {
        self.root.plugin_windows()
    }

    pub fn active_plugin_window(&self) -> Option<&PluginWindow> {
        self.plugin_windows()
            .into_iter()
            .find(|window| window.active)
    }

    pub fn leaf_count(&self) -> usize {
        self.root.leaves().len()
    }

    pub fn leaves(&self) -> Vec<WindowLeaf> {
        self.root.leaves()
    }

    pub fn active_leaf(&self) -> Option<WindowLeaf> {
        self.root.leaves().get(self.active_window_id).copied()
    }

    pub fn active_leaf_kind(&self) -> Option<WindowLeafKind> {
        self.active_leaf().map(|leaf| leaf.kind)
    }

    pub fn leaf_at_position(&self, x: usize, y: usize) -> Option<(usize, WindowLeaf)> {
        self.root
            .leaves()
            .into_iter()
            .enumerate()
            .find(|(_, leaf)| leaf.contains_position(x, y))
    }

    pub fn plugin_window_leaf_id(&self, id: &PluginWindowId) -> Option<usize> {
        let mut current_id = 0;
        Self::plugin_window_leaf_id_recursive(&self.root, &mut current_id, id)
    }

    fn plugin_window_leaf_id_recursive(
        node: &Split,
        current_id: &mut usize,
        target_id: &PluginWindowId,
    ) -> Option<usize> {
        match node {
            Split::Window(_) => {
                *current_id += 1;
                None
            }
            Split::PluginWindow(window) => {
                let leaf_id = *current_id;
                *current_id += 1;
                (&window.id == target_id).then_some(leaf_id)
            }
            Split::Horizontal { top, bottom, .. } => {
                Self::plugin_window_leaf_id_recursive(top, current_id, target_id).or_else(|| {
                    Self::plugin_window_leaf_id_recursive(bottom, current_id, target_id)
                })
            }
            Split::Vertical { left, right, .. } => {
                Self::plugin_window_leaf_id_recursive(left, current_id, target_id)
                    .or_else(|| Self::plugin_window_leaf_id_recursive(right, current_id, target_id))
            }
        }
    }

    pub fn focus_plugin_window(&mut self, id: &PluginWindowId) -> bool {
        if let Some(leaf_id) = self.plugin_window_leaf_id(id) {
            self.set_active(leaf_id);
            true
        } else {
            false
        }
    }

    pub fn close_plugin_window(&mut self, id: &PluginWindowId) -> bool {
        if let Some(leaf_id) = self.plugin_window_leaf_id(id) {
            self.set_active(leaf_id);
            self.close_window().is_some()
        } else {
            false
        }
    }

    pub fn update_plugin_window(
        &mut self,
        id: &PluginWindowId,
        render_state: PluginWindowRenderState,
    ) -> bool {
        Self::update_plugin_window_recursive(&mut self.root, id, render_state)
    }

    pub fn mark_unavailable_plugin_windows<'a>(
        &mut self,
        available_plugins: impl IntoIterator<Item = &'a str>,
    ) -> usize {
        let available_plugins = available_plugins.into_iter().collect::<HashSet<_>>();
        Self::mark_unavailable_plugin_windows_recursive(&mut self.root, &available_plugins)
    }

    fn mark_unavailable_plugin_windows_recursive(
        node: &mut Split,
        available_plugins: &HashSet<&str>,
    ) -> usize {
        match node {
            Split::Window(_) => 0,
            Split::PluginWindow(window) => {
                if available_plugins.contains(window.id.plugin.as_str())
                    || window.render_state.is_some()
                {
                    return 0;
                }

                let title = window
                    .title
                    .clone()
                    .unwrap_or_else(|| window.id.window.clone());
                window.render_state = Some(PluginWindowRenderState {
                    title: Some(title),
                    status: Some("plugin unavailable".to_string()),
                    transcript: vec![
                        PluginWindowLine {
                            text: format!("Plugin `{}` is not available.", window.id.plugin),
                            ..PluginWindowLine::default()
                        },
                        PluginWindowLine {
                            text: "Install or enable the plugin to restore this window."
                                .to_string(),
                            ..PluginWindowLine::default()
                        },
                    ],
                    key_hints: vec!["Ctrl-w w focus next".to_string()],
                    ..PluginWindowRenderState::default()
                });
                1
            }
            Split::Horizontal { top, bottom, .. } => {
                Self::mark_unavailable_plugin_windows_recursive(top, available_plugins)
                    + Self::mark_unavailable_plugin_windows_recursive(bottom, available_plugins)
            }
            Split::Vertical { left, right, .. } => {
                Self::mark_unavailable_plugin_windows_recursive(left, available_plugins)
                    + Self::mark_unavailable_plugin_windows_recursive(right, available_plugins)
            }
        }
    }

    fn update_plugin_window_recursive(
        node: &mut Split,
        target_id: &PluginWindowId,
        render_state: PluginWindowRenderState,
    ) -> bool {
        match node {
            Split::Window(_) => false,
            Split::PluginWindow(window) => {
                if &window.id == target_id {
                    if let Some(title) = render_state.title.clone() {
                        window.title = Some(title);
                    }
                    window.render_state = Some(render_state);
                    true
                } else {
                    false
                }
            }
            Split::Horizontal { top, bottom, .. } => {
                Self::update_plugin_window_recursive(top, target_id, render_state.clone())
                    || Self::update_plugin_window_recursive(bottom, target_id, render_state)
            }
            Split::Vertical { left, right, .. } => {
                Self::update_plugin_window_recursive(left, target_id, render_state.clone())
                    || Self::update_plugin_window_recursive(right, target_id, render_state)
            }
        }
    }

    /// Updates the layout when terminal is resized
    pub fn resize(&mut self, terminal_size: (usize, usize)) {
        self.resize_with_origin(Point::new(0, 0), terminal_size);
    }

    pub fn resize_with_origin(&mut self, position: Point, terminal_size: (usize, usize)) {
        self.root.layout(
            position,
            (terminal_size.0, terminal_size.1.saturating_sub(2)),
        );
    }

    /// Sets the active window by ID
    pub fn set_active(&mut self, window_id: usize) {
        Self::set_active_recursive(&mut self.root, &mut 0, window_id);
        if window_id < self.leaf_count() {
            self.active_window_id = window_id;
        }
    }

    fn set_active_recursive(node: &mut Split, current_id: &mut usize, target_id: usize) {
        match node {
            Split::Window(window) => {
                window.active = *current_id == target_id;
                *current_id += 1;
            }
            Split::PluginWindow(window) => {
                window.active = *current_id == target_id;
                *current_id += 1;
            }
            Split::Horizontal { top, bottom, .. } => {
                Self::set_active_recursive(top, current_id, target_id);
                Self::set_active_recursive(bottom, current_id, target_id);
            }
            Split::Vertical { left, right, .. } => {
                Self::set_active_recursive(left, current_id, target_id);
                Self::set_active_recursive(right, current_id, target_id);
            }
        }
    }

    /// Finds the window at the given terminal position
    pub fn window_at_position(&self, x: usize, y: usize) -> Option<(usize, &Window)> {
        let mut current_id = 0;
        Self::window_at_position_recursive(&self.root, &mut current_id, x, y)
    }

    fn window_at_position_recursive<'a>(
        node: &'a Split,
        current_id: &mut usize,
        x: usize,
        y: usize,
    ) -> Option<(usize, &'a Window)> {
        match node {
            Split::Window(window) => {
                let id = *current_id;
                *current_id += 1;
                window.contains_position(x, y).then_some((id, window))
            }
            Split::PluginWindow(_) => {
                *current_id += 1;
                None
            }
            Split::Horizontal { top, bottom, .. } => {
                Self::window_at_position_recursive(top, current_id, x, y)
                    .or_else(|| Self::window_at_position_recursive(bottom, current_id, x, y))
            }
            Split::Vertical { left, right, .. } => {
                Self::window_at_position_recursive(left, current_id, x, y)
                    .or_else(|| Self::window_at_position_recursive(right, current_id, x, y))
            }
        }
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

        let new_root = self.split_node(
            &self.root,
            self.active_window_id,
            NewLeaf::Editor {
                buffer_index: new_buffer_index,
            },
            true,
        )?;
        self.root = new_root;
        self.root.layout(Point::new(0, 0), (width, height));

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
        let (width, height) = self.get_terminal_bounds();
        log!("Active window id before split: {}", self.active_window_id);

        let new_root = self.split_node(
            &self.root,
            self.active_window_id,
            NewLeaf::Editor {
                buffer_index: new_buffer_index,
            },
            false,
        )?;
        self.root = new_root;
        self.root.layout(Point::new(0, 0), (width, height));

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

    pub fn split_vertical_plugin(
        &mut self,
        id: PluginWindowId,
        title: Option<String>,
    ) -> Option<()> {
        let (width, height) = self.get_terminal_bounds();
        let new_root = self.split_node(
            &self.root,
            self.active_window_id,
            NewLeaf::Plugin { id, title },
            false,
        )?;
        self.root = new_root;
        self.root.layout(Point::new(0, 0), (width, height));
        self.active_window_id += 1;
        self.set_active(self.active_window_id);
        Some(())
    }

    /// Closes the active window
    pub fn close_window(&mut self) -> Option<()> {
        use crate::log;

        // Can't close if there's only one window
        let window_count = self.leaf_count();
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
            let new_window_count = self.leaf_count();
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
            Split::Window(_) | Split::PluginWindow(_) => {
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
        let (width, height) = self.get_terminal_bounds();

        // Find the split containing the active window and adjust its ratio
        let active_id = self.active_window_id;
        let active_window = self.active_leaf()?;
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
            self.root.layout(Point::new(0, 0), (width, height));
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
            Split::Window(_) | Split::PluginWindow(_) => {
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

                *current_id = subtree_start + Self::window_count(top);

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

                *current_id = bottom_start + Self::window_count(bottom);
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

                *current_id = subtree_start + Self::window_count(left);

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

                *current_id = right_start + Self::window_count(right);
                false
            }
        }
    }

    fn window_count(node: &Split) -> usize {
        match node {
            Split::Window(_) | Split::PluginWindow(_) => 1,
            Split::Horizontal { top, bottom, .. }
            | Split::Vertical {
                left: top,
                right: bottom,
                ..
            } => Self::window_count(top) + Self::window_count(bottom),
        }
    }

    fn window_in_subtree_from(node: &Split, start_id: usize, target_id: usize) -> bool {
        let mut current_id = start_id;
        Self::window_in_subtree(node, &mut current_id, target_id)
    }

    /// Check if a window with the given ID is in the subtree
    fn window_in_subtree(node: &Split, current_id: &mut usize, target_id: usize) -> bool {
        match node {
            Split::Window(_) | Split::PluginWindow(_) => {
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
        let windows = self.root.leaves();
        let active_window = self.active_leaf()?;

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
        let windows = self.root.leaves();
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
        new_leaf: NewLeaf,
        horizontal: bool,
    ) -> Option<Split> {
        let mut current_id = 0;
        self.split_node_recursive(
            node,
            &mut current_id,
            target_window_id,
            new_leaf,
            horizontal,
        )
    }

    fn split_node_recursive(
        &self,
        node: &Split,
        current_id: &mut usize,
        target_window_id: usize,
        new_leaf: NewLeaf,
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
                    // This is the window to split
                    let new_split = match new_leaf {
                        NewLeaf::Editor { buffer_index } => {
                            Split::Window(Window::new(buffer_index, window.position, window.size))
                        }
                        NewLeaf::Plugin { id, title } => {
                            Split::new_plugin_window(id, title, window.position, window.size)
                        }
                    };

                    let mut old_window = window.clone();
                    old_window.active = false;

                    if horizontal {
                        Some(Split::Horizontal {
                            top: Box::new(Split::Window(old_window)),
                            bottom: Box::new(new_split),
                            ratio: 0.5,
                        })
                    } else {
                        Some(Split::Vertical {
                            left: Box::new(Split::Window(old_window)),
                            right: Box::new(new_split),
                            ratio: 0.5,
                        })
                    }
                } else {
                    *current_id += 1;
                    Some(Split::Window(window.clone()))
                }
            }
            Split::PluginWindow(window) => {
                if *current_id == target_window_id {
                    let new_split = match new_leaf {
                        NewLeaf::Editor { buffer_index } => {
                            Split::Window(Window::new(buffer_index, window.position, window.size))
                        }
                        NewLeaf::Plugin { id, title } => {
                            Split::new_plugin_window(id, title, window.position, window.size)
                        }
                    };

                    let mut old_window = window.clone();
                    old_window.active = false;

                    if horizontal {
                        Some(Split::Horizontal {
                            top: Box::new(Split::PluginWindow(old_window)),
                            bottom: Box::new(new_split),
                            ratio: 0.5,
                        })
                    } else {
                        Some(Split::Vertical {
                            left: Box::new(Split::PluginWindow(old_window)),
                            right: Box::new(new_split),
                            ratio: 0.5,
                        })
                    }
                } else {
                    *current_id += 1;
                    Some(Split::PluginWindow(window.clone()))
                }
            }
            Split::Horizontal { top, bottom, ratio } => {
                let new_top = self.split_node_recursive(
                    top,
                    current_id,
                    target_window_id,
                    new_leaf.clone(),
                    horizontal,
                )?;
                let new_bottom = self.split_node_recursive(
                    bottom,
                    current_id,
                    target_window_id,
                    new_leaf,
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
                    new_leaf.clone(),
                    horizontal,
                )?;
                let new_right = self.split_node_recursive(
                    right,
                    current_id,
                    target_window_id,
                    new_leaf,
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
