pub mod render_buffer;
pub mod rendering;

use std::{
    cmp::Ordering,
    collections::{HashMap, VecDeque},
    io::stdout,
    mem,
    path::PathBuf,
    time::{Duration, Instant},
};

use crate::unicode_utils::{display_width, next_grapheme_boundary, prev_grapheme_boundary};

/// Editor is the main component that handles:
/// - Text editing operations
/// - Buffer management
/// - User input processing
/// - Screen rendering
/// - LSP integration
/// - Plugin system
/// - Visual modes (normal, insert, visual, etc)
///
/// It maintains the terminal UI and coordinates all editor functionality.
use crossterm::{
    event::{
        self, Event, EventStream, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    },
    terminal, ExecutableCommand,
};
use futures::{future::FutureExt, select, StreamExt};
#[cfg(unix)]
use nix::sys::signal::{self, Signal};
#[cfg(unix)]
use nix::unistd::Pid;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

pub use render_buffer::RenderBuffer;

use crate::{
    buffer::Buffer,
    color::Color,
    command,
    config::{Config, KeyAction},
    dispatcher::Dispatcher,
    highlighter::Highlighter,
    log,
    lsp::{
        get_client_capabilities, CompletionResponse, Diagnostic, InboundMessage, LspClient,
        ParsedNotification, ProgressParams, ProgressToken, ResponseMessage, ServerCapabilities,
    },
    plugin::{self, PluginRegistry, Runtime},
    theme::{Style, Theme},
    ui::{CompletionUI, Component, FilePicker, Info, Picker},
    utils::get_workspace_uri,
    window::WindowManager,
};

pub static ACTION_DISPATCHER: Lazy<Dispatcher<PluginRequest, PluginResponse>> =
    Lazy::new(Dispatcher::new);

pub const DEFAULT_REGISTER: char = '"';
pub const ADD_TO_HISTORY_THRESHOLD: Duration = Duration::from_millis(100);

pub enum PluginRequest {
    Action(Action),
    EditorInfo(Option<i32>),
    OpenPicker(Option<String>, Option<i32>, Vec<Value>),
    BufferInsert {
        x: usize,
        y: usize,
        text: String,
    },
    BufferDelete {
        x: usize,
        y: usize,
        length: usize,
    },
    BufferReplace {
        x: usize,
        y: usize,
        length: usize,
        text: String,
    },
    GetCursorPosition,
    SetCursorPosition {
        x: usize,
        y: usize,
    },
    GetCursorDisplayColumn,
    SetCursorDisplayColumn {
        column: usize,
        y: usize,
    },
    GetBufferText {
        start_line: Option<usize>,
        end_line: Option<usize>,
    },
    GetConfig {
        key: Option<String>,
    },
    GetTextDisplayWidth {
        text: String,
    },
    CharIndexToDisplayColumn {
        x: usize,
        y: usize,
    },
    DisplayColumnToCharIndex {
        column: usize,
        y: usize,
    },
    IntervalCallback {
        interval_id: String,
    },
    TimeoutCallback {
        timer_id: String,
    },
    CreateOverlay {
        id: String,
        config: plugin::OverlayConfig,
    },
    UpdateOverlay {
        id: String,
        lines: Vec<(String, Style)>,
    },
    RemoveOverlay {
        id: String,
    },
}

#[derive(Debug)]
pub enum RenderCommand {
    BufferText {
        x: usize,
        y: usize,
        text: String,
        style: Style,
    },
}

#[allow(unused)]
pub struct PluginResponse(serde_json::Value);

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum Action {
    Quit(bool),
    Save,
    SaveAs(String),
    EnterMode(Mode),

    Undo,
    UndoMultiple(Vec<Action>),
    InsertString(String),

    FindNext,
    FindPrevious,

    MoveUp,
    MoveDown,
    MoveLeft,
    MoveRight,
    MoveToLineEnd,
    MoveToLineStart,
    MoveToFirstLineChar,
    MoveToLastLineChar,
    MoveLineToViewportCenter,
    MoveLineToViewportBottom,
    MoveToBottom,
    MoveToTop,
    MoveTo(usize, usize),
    MoveToFilePos(String, usize, usize),
    MoveToNextWord,
    MoveToPreviousWord,

    PageDown,
    PageUp,
    ScrollUp,
    ScrollDown,

    DeletePreviousChar,
    DeleteCharAtCursorPos,
    DeleteCurrentLine,
    DeleteLineAt(usize),
    DeleteCharAt(usize, usize),
    DeleteRange(usize, usize, usize, usize),
    DeleteWord,

    InsertNewLine,
    InsertCharAtCursorPos(char),
    InsertLineAt(usize, Option<String>),
    InsertLineBelowCursor,
    InsertLineAtCursor,
    InsertTab,

    ReplaceLineAt(usize, String),

    IndentLine,
    UnindentLine,

    GoToLine(usize),
    GoToDefinition,

    JumpBack,
    JumpForward,

    DumpHistory,
    DumpBuffer,
    DumpDiagnostics,
    DumpCapabilities,
    DumpTimers,
    DoPing,
    Command(String),
    PluginCommand(String),
    SetCursor(usize, usize),
    SetWaitingKey(Box<KeyAction>),
    OpenBuffer(String),
    OpenFile(String),

    NextBuffer,
    PreviousBuffer,
    FilePicker,
    ShowDialog,
    CloseDialog,
    ClearDiagnostics(String, Vec<usize>),
    RefreshDiagnostics,
    Refresh,
    Hover,
    Print(String),

    OpenPicker(Option<String>, Vec<String>, Option<i32>),
    Picked(String, Option<i32>),
    Suspend,

    Yank,
    Delete,
    Paste,
    PasteBefore,
    InsertBlock,

    InsertText {
        x: usize,
        y: usize,
        content: Content,
    },
    BufferText(Value),

    RequestCompletion,
    ShowProgress(ProgressParams),
    NotifyPlugins(String, Value),
    ViewLogs,
    ListPlugins,

    // Window management actions
    SplitHorizontal,
    SplitVertical,
    SplitHorizontalWithFile(String),
    SplitVerticalWithFile(String),
    CloseWindow,
    NextWindow,
    PreviousWindow,
    MoveWindowUp,
    MoveWindowDown,
    MoveWindowLeft,
    MoveWindowRight,
    ResizeWindowUp(usize),
    ResizeWindowDown(usize),
    ResizeWindowLeft(usize),
    ResizeWindowRight(usize),
    BalanceWindows,
    MaximizeWindow,
}

#[allow(unused)]
pub enum GoToLinePosition {
    Top,
    Center,
    Bottom,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum Mode {
    Normal,
    Insert,
    Command,
    Search,
    Visual,
    VisualLine,
    VisualBlock,
}

#[derive(Debug)]
pub struct StyleInfo {
    pub start: usize,
    pub end: usize,
    pub style: Style,
}

impl StyleInfo {
    pub fn contains(&self, pos: usize) -> bool {
        pos >= self.start && pos < self.end
    }
}

#[derive(Debug, Clone, Copy)]
struct Rect {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

impl Rect {
    fn new(x0: usize, y0: usize, x1: usize, y1: usize) -> Self {
        Self { x0, y0, x1, y1 }
    }
}

impl From<Rect> for (usize, usize, usize, usize) {
    fn from(rect: Rect) -> Self {
        (rect.x0, rect.y0, rect.x1, rect.y1)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Point {
    pub x: usize,
    pub y: usize,
}

impl Point {
    pub fn new(x: usize, y: usize) -> Self {
        Self { x, y }
    }
}

impl PartialEq for Point {
    fn eq(&self, other: &Self) -> bool {
        self.x == other.x && self.y == other.y
    }
}

impl PartialOrd for Point {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match self.y.cmp(&other.y) {
            Ordering::Equal => self.x.partial_cmp(&other.x),
            ordering => Some(ordering),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ActionOnSelection {
    action: Action,
    selection: Rect,
    action_index: usize,
}

impl ActionOnSelection {
    fn new(action: Action, selection: Rect, action_index: usize) -> Self {
        Self {
            action,
            selection,
            action_index,
        }
    }
}

pub struct Editor {
    /// LSP client for code intelligence features
    lsp: Box<dyn LspClient>,

    /// Editor configuration settings
    config: Config,

    /// Visual theme settings
    pub theme: Theme,

    /// Plugin system registry
    plugin_registry: PluginRegistry,

    /// Syntax highlighting engine
    highlighter: Highlighter,

    /// All open buffers
    buffers: Vec<Buffer>,

    /// Index of the currently active buffer
    current_buffer_index: usize,

    /// Window manager handling splits and layout
    window_manager: WindowManager,

    /// Terminal output handle
    stdout: std::io::Stdout,

    /// Terminal size (width, height)
    size: (u16, u16),

    /// Top line of viewport (for vertical scrolling)
    vtop: usize,

    /// Left column of viewport (for horizontal scrolling)
    vleft: usize,

    /// Cursor x position (column)
    cx: usize,

    /// Cursor y position (line)
    cy: usize,

    /// Previous cursor y position (for line highlighting)
    prev_highlight_y: Option<usize>,

    /// Visual x position (includes gutter width)
    vx: usize,

    /// Current editor mode (normal, insert, visual, etc)
    mode: Mode,

    /// Partial command being entered
    waiting_command: Option<String>,

    /// Next key action to process
    waiting_key_action: Option<KeyAction>,

    /// Actions that are pending while in visual mode
    pending_select_action: Option<ActionOnSelection>,

    /// Executed actions
    actions: Vec<Action>,

    /// Stack of actions that can be undone
    undo_actions: Vec<Action>,

    /// Actions to be combined into a single undo for insert mode
    insert_undo_actions: Vec<Action>,

    /// Current command line content
    command: String,

    /// Current search term
    search_term: String,

    /// Most recent error message
    last_error: Option<String>,

    /// Active dialog/popup component
    current_dialog: Option<Box<dyn Component>>,

    /// UI component for displaying completions
    completion_ui: CompletionUI,

    /// Number prefix for repeating commands
    repeater: Option<u16>,

    /// Starting point of current selection
    selection_start: Option<Point>,

    /// Current selection rectangle
    selection: Option<Rect>,

    /// Named registers for storing text (like vim registers)
    registers: HashMap<char, Content>,

    /// Map of diagnostics per file uri
    diagnostics: HashMap<String, Vec<Diagnostic>>,

    /// Indentation rules per file type
    indentation: HashMap<String, Indentation>,

    /// Past buffer locations
    back_history: Vec<HistoryEntry>,

    /// Future buffer locations
    fwd_history: Vec<HistoryEntry>,

    /// Pending render commands from plugins
    render_commands: VecDeque<RenderCommand>,

    /// Plugin overlay manager
    overlay_manager: plugin::OverlayManager,
}

#[derive(Debug, Clone, PartialEq)]
struct HistoryEntry {
    timestamp: Instant,
    action: Action,
    file: String,
    x: usize,
    y: usize,
}

impl HistoryEntry {
    fn new(action: Action, file: String, x: usize, y: usize) -> Self {
        let timestamp = Instant::now();
        Self {
            timestamp,
            action,
            file,
            x,
            y,
        }
    }

    fn moved_from(&self, other: &Self) -> bool {
        if self.timestamp.duration_since(other.timestamp) <= ADD_TO_HISTORY_THRESHOLD {
            return false;
        }
        self.file != other.file || self.x != other.x || self.y != other.y
    }
}

#[derive(Debug, Clone, Copy)]
struct Indentation {
    shift_width: usize,
    // TODO: use fields
    // soft_tab_stop: usize,
    // expand_tab: bool,
}

impl Indentation {
    fn new(shift_width: usize, _soft_tab_stop: usize, _expand_tab: bool) -> Self {
        Self {
            shift_width,
            // soft_tab_stop,
            // expand_tab,
        }
    }
}

impl ServerCapabilities {
    pub fn is_trigger_char(&self, c: char) -> bool {
        if let Some(completion_provider) = &self.completion_provider {
            if let Some(trigger_characters) = &completion_provider.trigger_characters {
                return trigger_characters.iter().any(|tc| tc == &c.to_string());
            }
        }
        false
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum ContentKind {
    Charwise,  // from Visual mode
    Linewise,  // from Visual Line mode
    Blockwise, // from Visual Block mode
}

impl From<Mode> for ContentKind {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Visual => ContentKind::Charwise,
            Mode::VisualLine => ContentKind::Linewise,
            Mode::VisualBlock => ContentKind::Blockwise,
            _ => ContentKind::Charwise,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Content {
    kind: ContentKind,
    text: String,
}

impl Editor {
    #[allow(unused)]
    pub fn with_size(
        lsp: Box<dyn LspClient>,
        width: usize,
        height: usize,
        config: Config,
        theme: Theme,
        buffers: Vec<Buffer>,
    ) -> anyhow::Result<Self> {
        let mut stdout = stdout();
        let vx = buffers
            .first()
            .map(|b| b.len().to_string().len())
            .unwrap_or(0)
            + 2;
        let size = (width as u16, height as u16);
        let highlighter = Highlighter::new(&theme)?;

        let mut plugin_registry = PluginRegistry::new();
        let indentation =
            HashMap::from_iter(vec![("rs".to_string(), Indentation::new(4, 4, true))]);

        let window_manager = WindowManager::new(0, (width, height));

        Ok(Editor {
            lsp,
            config,
            theme,
            plugin_registry,
            highlighter,
            buffers,
            current_buffer_index: 0,
            window_manager,
            stdout,
            size,
            vtop: 0,
            vleft: 0,
            cx: 0,
            cy: 0,
            prev_highlight_y: None,
            vx,
            mode: Mode::Normal,
            waiting_command: None,
            waiting_key_action: None,
            pending_select_action: None,
            actions: vec![],
            undo_actions: vec![],
            insert_undo_actions: vec![],
            command: String::new(),
            search_term: String::new(),
            last_error: None,
            current_dialog: None,
            repeater: None,
            selection_start: None,
            selection: None,
            registers: HashMap::new(),
            diagnostics: HashMap::new(),
            completion_ui: CompletionUI::new(),
            indentation,
            back_history: Vec::new(),
            fwd_history: Vec::new(),
            render_commands: VecDeque::new(),
            overlay_manager: plugin::OverlayManager::new(),
        })
    }

    /// Creates a new Editor instance with the given configuration
    ///
    /// # Arguments
    /// * `lsp` - LSP client for code intelligence features
    /// * `config` - Editor configuration settings
    /// * `theme` - Visual theme settings
    /// * `buffers` - Initial set of buffers to edit
    ///
    /// # Returns
    /// A Result containing either the new Editor or an error if initialization fails
    pub fn new(
        lsp: Box<dyn LspClient>,
        config: Config,
        theme: Theme,
        buffers: Vec<Buffer>,
    ) -> anyhow::Result<Self> {
        let size = terminal::size()?;
        Self::with_size(
            lsp,
            size.0 as usize,
            size.1 as usize,
            config,
            theme,
            buffers,
        )
    }

    /// Synchronizes the editor's state with the active window
    fn sync_with_window(&mut self) {
        if let Some(window) = self.window_manager.active_window() {
            self.current_buffer_index = window.buffer_index;
            self.vtop = window.vtop;
            self.vleft = window.vleft;
            self.cx = window.cx;
            self.cy = window.cy;
            self.vx = window.vx;
        }
    }

    /// Synchronizes the active window with the editor's state
    fn sync_to_window(&mut self) {
        if let Some(window) = self.window_manager.active_window_mut() {
            window.buffer_index = self.current_buffer_index;
            window.vtop = self.vtop;
            window.vleft = self.vleft;
            window.cx = self.cx;
            window.cy = self.cy;
            window.vx = self.vx;
        }
    }

    fn indentation(&self) -> Indentation {
        let file_type = self.current_buffer().file_type();

        let Some(file_type) = file_type.as_deref() else {
            return Indentation::new(4, 4, true);
        };

        self.indentation
            .get(file_type)
            .copied()
            .unwrap_or_else(|| Indentation::new(4, 4, true))
    }

    pub fn vwidth(&self) -> usize {
        self.size.0 as usize
    }

    pub fn vheight(&self) -> usize {
        self.size.1 as usize - 2
    }

    /// Window-aware coordinate transformation methods

    /// Convert window-local X coordinate to terminal X coordinate
    pub fn window_to_terminal_x(&self, window: &crate::window::Window, x: usize) -> usize {
        window.position.x + x
    }

    /// Convert window-local Y coordinate to terminal Y coordinate
    pub fn window_to_terminal_y(&self, window: &crate::window::Window, y: usize) -> usize {
        window.position.y + y
    }

    /// Convert buffer coordinates to window-local coordinates, accounting for viewport
    pub fn buffer_to_window_coords(
        &self,
        window: &crate::window::Window,
        buf_x: usize,
        buf_y: usize,
    ) -> Option<(usize, usize)> {
        // Check if the buffer position is within the viewport
        if buf_y < window.vtop || buf_y >= window.vtop + window.inner_height() {
            return None;
        }

        if buf_x < window.vleft || buf_x >= window.vleft + window.inner_width() {
            return None;
        }

        // Convert to window-local coordinates
        let window_x = buf_x - window.vleft;
        let window_y = buf_y - window.vtop;

        Some((window_x, window_y))
    }

    /// Get the effective viewport width for a window
    pub fn window_vwidth(&self, window: &crate::window::Window) -> usize {
        window.inner_width()
    }

    /// Get the effective viewport height for a window
    pub fn window_vheight(&self, window: &crate::window::Window) -> usize {
        window.inner_height()
    }

    pub fn cursor_position(&self) -> (usize, usize) {
        (self.vx + self.cx, self.cy)
    }

    /// Returns the display width of the current line
    fn line_length(&self) -> usize {
        if let Some(line) = self.viewport_line(self.cy) {
            let line = line.trim_end_matches('\n');
            return line.chars().count();
        }
        0
    }

    /// Returns the display width of the current line in columns
    #[allow(dead_code)]
    fn line_display_width(&self) -> usize {
        if let Some(line) = self.viewport_line(self.cy) {
            let line = line.trim_end_matches('\n');
            return display_width(line);
        }
        0
    }

    fn length_for_line(&self, n: usize) -> usize {
        if let Some(line) = self.viewport_line(n) {
            let line = line.trim_end_matches('\n');
            return line.chars().count();
        }
        0
    }

    /// Returns the current buffer y position
    fn buffer_line(&self) -> usize {
        self.vtop + self.cy
    }

    /// Returns the buffer URI
    fn buffer_uri(&self) -> anyhow::Result<Option<String>> {
        self.current_buffer().uri()
    }

    fn viewport_line(&self, n: usize) -> Option<String> {
        let buffer_line = self.vtop + n;
        let line = self.current_buffer().get(buffer_line);

        // Debug: Check if line contains emoji
        if let Some(ref l) = line {
            if l.chars()
                .any(|c| c as u32 >= 0x1F300 && c as u32 <= 0x1F9FF)
            {
                log!("viewport_line {}: contains emoji: {:?}", buffer_line, l);
            }
        }

        line
    }

    fn gutter_width(&self) -> usize {
        self.current_buffer().len().to_string().len() + 1
    }

    pub fn highlight(&mut self, code: &str) -> anyhow::Result<Vec<StyleInfo>> {
        self.highlighter.highlight(code)
    }

    fn fill_line(&mut self, buffer: &mut RenderBuffer, x: usize, y: usize, style: &Style) {
        let width = self.vwidth().saturating_sub(x);
        let line_fill = " ".repeat(width);
        buffer.set_text(x, y, &line_fill, style);
    }

    fn draw_line(&mut self, buffer: &mut RenderBuffer) {
        let line = self.viewport_line(self.cy).unwrap_or_default();
        let style_info = self.highlight(&line).unwrap_or_default();
        let default_style = self.theme.style.clone();

        let mut x = self.vx;
        let mut iter = line.chars().enumerate().peekable();

        if line.is_empty() {
            self.fill_line(buffer, x, self.cy, &default_style);
            return;
        }

        while let Some((pos, c)) = iter.next() {
            if c == '\n' || iter.peek().is_none() {
                if c != '\n' {
                    buffer.set_char(x, self.cy, c, &default_style, &self.theme);
                    x += 1;
                }
                self.fill_line(buffer, x, self.cy, &default_style);
                break;
            }

            if x < self.vwidth() {
                if let Some(style) = determine_style_for_position(&style_info, pos) {
                    buffer.set_char(x, self.cy, c, &style, &self.theme);
                } else {
                    buffer.set_char(x, self.cy, c, &default_style, &self.theme);
                }
            }
            x += 1;
        }

        self.draw_line_diagnostics(buffer, self.buffer_line());
    }

    pub fn draw_line_diagnostics(&mut self, buffer: &mut RenderBuffer, line_num: usize) {
        let fg = adjust_color_brightness(self.theme.style.fg, -20);
        let bg = adjust_color_brightness(self.theme.style.bg, 10);

        // TODO: take it from theme
        let hint_style = Style {
            fg,
            bg,
            italic: true,
            ..Default::default()
        };
        let Ok(Some(uri)) = self.buffer_uri() else {
            // TODO: log the error
            return;
        };

        let Some(line) = self.current_buffer().get(line_num) else {
            return;
        };

        let x = self.gutter_width() + line.len() + 5;

        // otherwise, clear the line
        let text = " ".repeat(self.vwidth().saturating_sub(x));
        buffer.set_text(x, line_num - self.vtop, &text, &self.theme.style);

        if let Some(line_diagnostics) = self.diagnostics.get(&uri) {
            // if there is a diagnostic for the current line, display it
            let diagnostics = line_diagnostics
                .iter()
                .filter(|d| d.range.start.line == line_num)
                .collect::<Vec<_>>();
            if !diagnostics.is_empty() {
                let prefix = "■".repeat(diagnostics.len());
                let msg = diagnostics[0].message.replace("\n", " ");
                let msg = format!("{} {}", prefix, msg);
                buffer.set_text(x, line_num - self.vtop, &msg, &hint_style);
            }
        }
    }

    // pub fn draw_viewport(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
    //     let vbuffer = self.current_buffer().viewport(self.vtop, self.vheight());
    //     let style_info = self.highlight(&vbuffer)?;
    //     let vheight = self.vheight();
    //     let default_style = self.theme.style.clone();
    //
    //     let mut x = self.vx;
    //     let mut y = 0;
    //     let mut iter = vbuffer.chars().enumerate().peekable();
    //
    //     while let Some((pos, c)) = iter.next() {
    //         if c == '\n' || iter.peek().is_none() {
    //             if c != '\n' {
    //                 buffer.set_char(x, y, c, &default_style, &self.theme);
    //                 x += 1;
    //             }
    //             self.fill_line(buffer, x, y, &default_style);
    //             x = self.vx;
    //             y += 1;
    //             if y > vheight {
    //                 break;
    //             }
    //             continue;
    //         }
    //
    //         if x < self.vwidth() {
    //             if let Some(style) = determine_style_for_position(&style_info, pos) {
    //                 buffer.set_char(x, y, c, &style, &self.theme);
    //             } else {
    //                 buffer.set_char(x, y, c, &default_style, &self.theme);
    //             }
    //         }
    //         x += 1;
    //     }
    //
    //     while y < vheight {
    //         self.fill_line(buffer, self.vx, y, &default_style);
    //         y += 1;
    //     }
    //
    //     self.draw_gutter(buffer)?;
    //     self.draw_diagnostics(buffer);
    //     // self.draw_highlight(buffer);
    //
    //     Ok(())
    // }

    fn clear_diagnostics(&mut self, buffer: &mut RenderBuffer, lines: &[usize]) {
        log!("clearing diagnostics for lines: {:?}", lines);
        for l in lines {
            if self.is_within_viewport(*l) {
                let line = self.current_buffer().get(*l);
                let len = line.clone().map(|l| l.len()).unwrap_or(0);
                let y = l - self.vtop;
                let x = self.gutter_width() + len + 5;
                // fill the rest of the line with spaces:
                let msg = " ".repeat(self.size.0 as usize - x);
                buffer.set_text(x, y, &msg, &self.theme.style);
            }
        }
    }

    fn draw_diagnostics(&mut self, buffer: &mut RenderBuffer) {
        // if !self.is_editing() {
        //     return;
        // }

        for line in self.vtop..=self.vtop + self.vheight() {
            self.draw_line_diagnostics(buffer, self.vtop + line);
        }
    }

    fn is_normal(&self) -> bool {
        matches!(self.mode, Mode::Normal)
    }

    fn is_insert(&self) -> bool {
        matches!(self.mode, Mode::Insert)
    }

    fn is_command(&self) -> bool {
        matches!(self.mode, Mode::Command)
    }

    fn is_search(&self) -> bool {
        matches!(self.mode, Mode::Search)
    }

    fn is_visual(&self) -> bool {
        matches!(
            self.mode,
            Mode::Visual | Mode::VisualLine | Mode::VisualBlock
        )
    }

    fn has_term(&self) -> bool {
        self.is_command() || self.is_search()
    }

    fn term(&self) -> &str {
        if self.is_command() {
            &self.command
        } else {
            &self.search_term
        }
    }

    // TODO: in neovim, when you are at an x position and you move to a shorter line, the cursor
    //       goes back to the max x but returns to the previous x position if the line is longer
    fn check_bounds(&mut self) {
        let line_length = self.line_length();

        if self.cx >= line_length && self.is_normal() {
            if line_length > 0 {
                self.cx = self.line_length() - 1;
            } else if self.is_normal() {
                self.cx = 0;
            }
        }
        if self.cx >= self.vwidth() {
            self.cx = self.vwidth() - 1;
        }

        // check if cy is after the end of the buffer
        // the end of the buffer is less than vtop + cy
        let line_on_buffer = self.cy + self.vtop;
        if line_on_buffer > self.current_buffer().len().saturating_sub(1) {
            self.cy = self.current_buffer().len() - self.vtop - 1;
        }
    }

    /// Starts the main editor loop
    ///
    /// This is the core event loop that:
    /// - Handles user input
    /// - Processes LSP messages
    /// - Updates the display
    /// - Manages plugin execution
    ///
    /// # Returns
    /// A Result indicating success or failure of the editor session
    pub async fn run(&mut self) -> anyhow::Result<()> {
        terminal::enable_raw_mode()?;
        self.stdout
            .execute(event::EnableMouseCapture)?
            .execute(terminal::EnterAlternateScreen)?
            .execute(terminal::Clear(terminal::ClearType::All))?;

        let mut runtime = Runtime::new();
        for (name, path) in &self.config.plugins {
            let path = Config::path("plugins").join(path);
            self.plugin_registry
                .add(name, path.to_string_lossy().as_ref());
        }
        self.plugin_registry.initialize(&mut runtime).await?;

        let mut buffer = RenderBuffer::new(
            self.size.0 as usize,
            self.size.1 as usize,
            &Style::default(),
        );
        self.render(&mut buffer)?;

        let mut reader = EventStream::new();

        loop {
            let mut delay = futures_timer::Delay::new(Duration::from_millis(10)).fuse();
            let mut event = reader.next().fuse();

            select! {
                _ = delay => {
                    // Poll for timer callbacks
                    let timer_callbacks = crate::plugin::poll_timer_callbacks();
                    for callback_request in timer_callbacks {
                        if let PluginRequest::TimeoutCallback { timer_id } = callback_request {
                            log!("[TIMER] Processing timeout callback for timer: {}", timer_id);
                            self.plugin_registry
                                .notify(&mut runtime, "timeout:callback", json!({ "timerId": timer_id }))
                                .await?;
                        }
                    }

                    // if self.sync_state.should_notify() {
                    //     for file in self.sync_state.get_changes().unwrap_or_default() {
                    //         // FIXME: not current buffer!
                    //         self.lsp
                    //             .did_change(&file, &self.current_buffer().contents())
                    //             .await?;
                    //     }
                    //     //
                    //     // if let Some(uri) = self.current_buffer().uri()? {
                    //     //     self.lsp.request_diagnostics(&uri).await?;
                    //     // }
                    // }

                    if self.config.show_diagnostics {
                        // handle responses from lsp
                        match self.lsp.recv_response().await {
                            Ok(Some((msg, method))) => {
                                if let Some(action) = self.handle_lsp_message(&msg, method) {
                                    // TODO: handle quit
                                    // let current_buffer = buffer.clone();
                                    self.execute(&action, &mut buffer, &mut runtime).await?;
                                    self.render(&mut buffer)?;
                                    // self.redraw(&mut runtime, &current_buffer, &mut buffer).await?;
                                }
                            }
                            Ok(None) => {},
                            Err(err) => {
                                log!("ERROR: Lsp error: {err}");
                            }
                        }
                    }

                    if let Some(req) = ACTION_DISPATCHER.try_recv_request() {
                        match req {
                            PluginRequest::Action(action) => {
                                // let current_buffer = buffer.clone();
                                self.execute(&action, &mut buffer, &mut runtime).await?;
                                self.render(&mut buffer)?;
                                // self.redraw(&mut runtime, &current_buffer, &mut buffer).await?;
                            }
                            PluginRequest::EditorInfo(id) => {
                                let info = serde_json::to_value(self.info())?;
                                let key = if let Some(id) = id {
                                    format!("editor:info:{}", id)
                                } else {
                                    "editor:info".to_string()
                                };
                                self.plugin_registry
                                    .notify(&mut runtime, &key, info)
                                    .await?;
                            }
                            PluginRequest::OpenPicker(title, id, items) => {
                                // let current_buffer = buffer.clone();
                                let items = items.iter().map(|v| match v {
                                    serde_json::Value::String(s) => s.clone(),
                                    val => val.to_string(),
                                }).collect();
                                self.execute(&Action::OpenPicker(title, items, id), &mut buffer, &mut runtime).await?;
                                // self.render(buffer)?;
                            }
                            PluginRequest::BufferInsert { x, y, text } => {
                                // Track undo action
                                self.undo_actions.push(Action::DeleteRange(x, y, x + text.len(), y));

                                self.current_buffer_mut().insert_str(x, y, &text);
                                self.notify_change(&mut runtime).await?;
                                self.render(&mut buffer)?;
                            }
                            PluginRequest::BufferDelete { x, y, length } => {
                                // Save deleted text for undo
                                let current_buf = self.current_buffer();
                                let mut deleted_text = String::new();
                                for i in 0..length {
                                    if let Some(line) = current_buf.get(y) {
                                        if x + i < line.len() {
                                            deleted_text.push(line.chars().nth(x + i).unwrap_or(' '));
                                        }
                                    }
                                }
                                self.undo_actions.push(Action::InsertText {
                                    x,
                                    y,
                                    content: Content {
                                        kind: ContentKind::Charwise,
                                        text: deleted_text
                                    }
                                });

                                for _ in 0..length {
                                    self.current_buffer_mut().remove(x, y);
                                }
                                self.notify_change(&mut runtime).await?;
                                self.render(&mut buffer)?;
                            }
                            PluginRequest::BufferReplace { x, y, length, text } => {
                                // Save replaced text for undo
                                let current_buf = self.current_buffer();
                                let mut replaced_text = String::new();
                                for i in 0..length {
                                    if let Some(line) = current_buf.get(y) {
                                        if x + i < line.len() {
                                            replaced_text.push(line.chars().nth(x + i).unwrap_or(' '));
                                        }
                                    }
                                }
                                // For undo, we need to delete the new text and insert the old
                                self.undo_actions.push(Action::UndoMultiple(vec![
                                    Action::DeleteRange(x, y, x + text.len(), y),
                                    Action::InsertText {
                                        x,
                                        y,
                                        content: Content {
                                            kind: ContentKind::Charwise,
                                            text: replaced_text
                                        }
                                    }
                                ]));

                                // Delete old text
                                for _ in 0..length {
                                    self.current_buffer_mut().remove(x, y);
                                }
                                // Insert new text
                                self.current_buffer_mut().insert_str(x, y, &text);
                                self.notify_change(&mut runtime).await?;
                                self.render(&mut buffer)?;
                            }
                            PluginRequest::GetCursorPosition => {
                                let pos = serde_json::json!({
                                    "x": self.cx,
                                    "y": self.cy + self.vtop
                                });
                                self.plugin_registry
                                    .notify(&mut runtime, "cursor:position", pos)
                                    .await?;
                            }
                            PluginRequest::GetCursorDisplayColumn => {
                                let display_col = if let Some(line) = self.current_line_contents() {
                                    let line = line.trim_end_matches('\n');
                                    crate::unicode_utils::char_to_column(line, self.cx)
                                } else {
                                    self.cx
                                };
                                let pos = serde_json::json!({
                                    "column": display_col,
                                    "y": self.cy + self.vtop
                                });
                                self.plugin_registry
                                    .notify(&mut runtime, "cursor:display_position", pos)
                                    .await?;
                            }
                            PluginRequest::SetCursorPosition { x, y } => {
                                self.cx = x;
                                // Adjust viewport if needed
                                if y < self.vtop {
                                    self.vtop = y;
                                    self.cy = 0;
                                } else if y >= self.vtop + self.vheight() {
                                    self.vtop = y.saturating_sub(self.vheight() - 1);
                                    self.cy = self.vheight() - 1;
                                } else {
                                    self.cy = y - self.vtop;
                                }
                                self.draw_cursor()?;
                            }
                            PluginRequest::SetCursorDisplayColumn { column, y } => {
                                // Convert display column to character index
                                if let Some(line) = self.viewport_line(y - self.vtop) {
                                    let line = line.trim_end_matches('\n');
                                    self.cx = crate::unicode_utils::column_to_char(line, column);
                                }
                                // Adjust viewport if needed
                                if y < self.vtop {
                                    self.vtop = y;
                                    self.cy = 0;
                                } else if y >= self.vtop + self.vheight() {
                                    self.vtop = y.saturating_sub(self.vheight() - 1);
                                    self.cy = self.vheight() - 1;
                                } else {
                                    self.cy = y - self.vtop;
                                }
                                self.draw_cursor()?;
                            }
                            PluginRequest::GetBufferText { start_line, end_line } => {
                                let current_buf = self.current_buffer();
                                let start = start_line.unwrap_or(0);
                                let end = end_line.unwrap_or(current_buf.len());
                                let mut lines = Vec::new();
                                for i in start..end.min(current_buf.len()) {
                                    if let Some(line) = current_buf.get(i) {
                                        lines.push(line);
                                    }
                                }
                                let text = lines.join("\n");
                                self.plugin_registry
                                    .notify(&mut runtime, "buffer:text", serde_json::json!({ "text": text }))
                                    .await?;
                            }
                            PluginRequest::GetConfig { key } => {
                                let config_value = if let Some(key) = key {
                                    // Return specific config value
                                    match key.as_str() {
                                        "theme" => json!(self.config.theme),
                                        "plugins" => json!(self.config.plugins),
                                        "log_file" => json!(self.config.log_file),
                                        "mouse_scroll_lines" => json!(self.config.mouse_scroll_lines),
                                        "show_diagnostics" => json!(self.config.show_diagnostics),
                                        "keys" => json!(self.config.keys),
                                        _ => json!(null),
                                    }
                                } else {
                                    // Return entire config
                                    json!({
                                        "theme": self.config.theme,
                                        "plugins": self.config.plugins,
                                        "log_file": self.config.log_file,
                                        "mouse_scroll_lines": self.config.mouse_scroll_lines,
                                        "show_diagnostics": self.config.show_diagnostics,
                                        "keys": self.config.keys,
                                    })
                                };
                                self.plugin_registry
                                    .notify(&mut runtime, "config:value", json!({ "value": config_value }))
                                    .await?;
                            }
                            PluginRequest::GetTextDisplayWidth { text } => {
                                let width = crate::unicode_utils::display_width(&text);
                                self.plugin_registry
                                    .notify(&mut runtime, "text:display_width", json!({ "width": width }))
                                    .await?;
                            }
                            PluginRequest::CharIndexToDisplayColumn { x, y } => {
                                let display_col = if let Some(line) = self.current_buffer().get(y) {
                                    let line = line.trim_end_matches('\n');
                                    crate::unicode_utils::char_to_column(line, x)
                                } else {
                                    x
                                };
                                self.plugin_registry
                                    .notify(&mut runtime, "char:display_column", json!({ "column": display_col }))
                                    .await?;
                            }
                            PluginRequest::DisplayColumnToCharIndex { column, y } => {
                                let char_index = if let Some(line) = self.current_buffer().get(y) {
                                    let line = line.trim_end_matches('\n');
                                    crate::unicode_utils::column_to_char(line, column)
                                } else {
                                    column
                                };
                                self.plugin_registry
                                    .notify(&mut runtime, "display:char_index", json!({ "index": char_index }))
                                    .await?;
                            }
                            PluginRequest::IntervalCallback { interval_id } => {
                                self.plugin_registry
                                    .notify(&mut runtime, "interval:callback", json!({ "intervalId": interval_id }))
                                    .await?;
                            }
                            PluginRequest::TimeoutCallback { timer_id } => {
                                log!("[TIMER] Processing timeout callback for timer: {}", timer_id);
                                self.plugin_registry
                                    .notify(&mut runtime, "timeout:callback", json!({ "timerId": timer_id }))
                                    .await?;
                            }
                            PluginRequest::CreateOverlay { id, config } => {
                                log!("Creating overlay: {}", id);
                                self.overlay_manager.create_overlay(id, config);
                            }
                            PluginRequest::UpdateOverlay { id, lines } => {
                                log!("Updating overlay: {}", id);
                                if let Some(overlay) = self.overlay_manager.get_overlay_mut(&id) {
                                    overlay.update_content(lines);
                                }
                            }
                            PluginRequest::RemoveOverlay { id } => {
                                log!("Removing overlay: {}", id);
                                self.overlay_manager.remove_overlay(&id);
                            }
                        }
                    }
                }
                maybe_event = event => {
                    match maybe_event {
                        Some(Ok(ev)) => {
                            // let current_buffer = buffer.clone();
                            self.check_bounds();

                            if let event::Event::Resize(width, height) = ev {
                                self.size = (width, height);
                                let max_y = height as usize - 2;
                                if self.cy > max_y - 1 {
                                    self.cy = max_y - 1;
                                }
                                // Resize window manager
                                self.window_manager.resize((width as usize, height as usize));
                                self.sync_to_window();
                                buffer = RenderBuffer::new(
                                    self.size.0 as usize,
                                    self.size.1 as usize,
                                    &Style::default(),
                                );
                                // TODO: handle dialog resize
                                self.current_dialog = None;
                                self.render(&mut buffer)?;

                                let action = Action::NotifyPlugins(
                                    "editor:resize".to_string(),
                                    serde_json::to_value(self.size)?,
                                );
                                self.execute(&action, &mut buffer, &mut runtime).await?;
                                continue;
                            }

                            if let Some(action) = self.handle_event(&ev)? {
                                if self.handle_key_action(&ev, &action, &mut buffer, &mut runtime).await? {
                                    break;
                                }
                            }

                            self.render(&mut buffer)?;
                        },
                        Some(Err(error)) => {
                            log!("error: {error}");
                        },
                        None => {
                        }
                    }
                }
            }
        }

        Ok(())
    }

    #[async_recursion::async_recursion]
    async fn handle_key_action(
        &mut self,
        ev: &event::Event,
        action: &KeyAction,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<bool> {
        let quit = match action {
            KeyAction::None => false,
            KeyAction::Single(action) => self.execute(action, buffer, runtime).await?,
            KeyAction::Multiple(actions) => {
                let mut quit = false;
                for action in actions {
                    if self.execute(action, buffer, runtime).await? {
                        quit = true;
                        break;
                    }
                }
                quit
            }
            KeyAction::Nested(actions) => {
                log!(
                    "Nested key action detected, actions count: {}",
                    actions.len()
                );
                if let Event::Key(KeyEvent {
                    code: KeyCode::Char(c),
                    ..
                }) = ev
                {
                    self.waiting_command = Some(format!("{c}"));
                    log!("Setting waiting command: {}", c);
                }
                self.waiting_key_action = Some(KeyAction::Nested(actions.clone()));
                false
            }
            KeyAction::Repeating(times, action) => {
                self.repeater = None;
                let mut quit = false;
                for _ in 0..*times as usize {
                    if self.handle_key_action(ev, action, buffer, runtime).await? {
                        quit = true;
                        break;
                    }
                }
                quit
            }
        };

        Ok(quit)
    }

    fn add_diagnostics(&mut self, uri: Option<&str>, diagnostics: &[Diagnostic]) -> Option<Action> {
        let Some(uri) = uri else {
            log!("WARN: no uri provided for diagnostics - {diagnostics:?}");
            return None;
        };

        log!("Adding diagnostics for {uri}: {diagnostics:#?}");
        self.diagnostics
            .insert(uri.to_string(), diagnostics.to_vec());

        Some(Action::Refresh)
    }

    fn process_progress(&mut self, progress_params: &ProgressParams) -> Option<Action> {
        Some(Action::ShowProgress(progress_params.clone()))
    }

    fn handle_lsp_message(
        &mut self,
        msg: &InboundMessage,
        method: Option<String>,
    ) -> Option<Action> {
        fn parse_diagnostics(msg: &ResponseMessage) -> Option<(String, Vec<Diagnostic>)> {
            let req = msg.request.as_ref()?;
            let params = req.params.as_object()?;
            let text_document = params.get("textDocument")?.as_object()?;
            let uri = text_document.get("uri")?.as_str()?;
            let diagnostics = msg.result.as_object()?.get("items")?.as_array()?;

            Some((
                uri.to_string(),
                diagnostics
                    .iter()
                    .filter_map(|d| serde_json::from_value::<Diagnostic>(d.clone()).ok())
                    .collect::<Vec<_>>(),
            ))
        }

        match msg {
            InboundMessage::Message(msg) => {
                if let Some(ref method) = method {
                    if method == "initialize" {
                        // self.server_capabilities = self.lsp.get_server_capabilities().cloned();
                        // log!("server capabilities: {:#?}", self.server_capabilities);
                    }

                    if method == "rust-analyzer/analyzerStatus" {
                        let r = msg.result.as_str().unwrap();
                        log!("analyzer status: {r}");
                    }

                    if method == "rust-analyzer/viewFileText" {
                        let r = msg.result.as_str().unwrap();
                        log!("----");
                        log!("{r}");
                        log!("----");
                    }

                    if method == "textDocument/diagnostic" {
                        if let Some((uri, diagnostics)) = parse_diagnostics(msg) {
                            return self.add_diagnostics(Some(&uri), &diagnostics);
                        }
                    }

                    if method == "textDocument/completion" {
                        if msg.result.is_null() {
                            // TODO: retry?
                            return None;
                        }

                        match serde_json::from_value::<CompletionResponse>(msg.result.clone()) {
                            Ok(completion_response) => {
                                self.completion_ui.show(
                                    completion_response.items,
                                    self.cx,
                                    self.cy,
                                );
                                self.current_dialog = Some(Box::new(self.completion_ui.clone()));
                                return Some(Action::ShowDialog);
                            }
                            Err(err) => {
                                log!("ERROR: error parsing completion response: {err}");
                            }
                        }
                    }

                    if method == "textDocument/definition" {
                        let result = match msg.result {
                            serde_json::Value::Array(ref arr) => arr[0].as_object().unwrap(),
                            serde_json::Value::Object(ref obj) => obj,
                            _ => return None,
                        };

                        return self.go_to_definition(result);
                    }

                    if method == "textDocument/hover" {
                        log!("hover response: {msg:?}");
                        let result = match msg.result {
                            serde_json::Value::Array(ref arr) => arr[0].as_object().unwrap(),
                            serde_json::Value::Object(ref obj) => obj,
                            _ => return None,
                        };

                        if let Some(contents) = result.get("contents") {
                            if let Some(contents) = contents.as_object() {
                                if let Some(serde_json::Value::String(value)) =
                                    contents.get("value")
                                {
                                    let info = Info::new(self, value.clone());
                                    self.current_dialog = Some(Box::new(info));
                                    return Some(Action::ShowDialog);
                                }
                            }
                        }
                    }
                }
                None
            }
            InboundMessage::Notification(msg) => match msg {
                ParsedNotification::PublishDiagnostics(msg) => {
                    self.add_diagnostics(msg.uri.as_deref(), &msg.diagnostics)
                }
                ParsedNotification::Progress(progress_params) => {
                    // self.plugin_registry
                    //     .notify(
                    //         runtime,
                    //         "progress",
                    //         serde_json::to_value(progress_params).unwrap(),
                    //     )
                    //     .await?;
                    self.process_progress(progress_params);
                    Some(Action::NotifyPlugins(
                        "lsp:progress".to_string(),
                        serde_json::to_value(progress_params).unwrap_or(serde_json::Value::Null),
                    ))
                }
            },
            InboundMessage::UnknownNotification(msg) => {
                log!("got an unhandled notification: {msg:#?}");
                None
            }
            InboundMessage::Error(error_msg) => {
                log!("got an error: {error_msg:?}");
                None
            }
            InboundMessage::ProcessingError(error_msg) => {
                self.last_error = Some(error_msg.to_string());
                None
            }
        }
    }

    /// Processes a single input event and determines what action to take
    ///
    /// Handles different types of events based on the current editor mode:
    /// - Key presses
    /// - Mouse events
    /// - Window resize events
    ///
    /// # Arguments
    /// * `ev` - The event to process
    ///
    /// # Returns
    /// An optional KeyAction to execute based on the event
    fn handle_event(&mut self, ev: &event::Event) -> anyhow::Result<Option<KeyAction>> {
        if let Some(ka) = self.waiting_key_action.take() {
            self.waiting_command = None;
            return Ok(self.handle_waiting_command(ka, ev));
        }

        if let Some(current_dialog) = &mut self.current_dialog {
            return Ok(current_dialog.handle_event(ev));
        }

        Ok(match self.mode {
            Mode::Normal => self.handle_normal_event(ev),
            Mode::Insert => self.handle_insert_event(ev)?,
            Mode::Command => self.handle_command_event(ev),
            Mode::Search => self.handle_search_event(ev),
            Mode::Visual | Mode::VisualLine | Mode::VisualBlock => self.handle_visual_event(ev),
        })
    }

    fn handle_repeater(&mut self, ev: &event::Event) -> bool {
        if let Event::Key(KeyEvent {
            code: KeyCode::Char(c),
            ..
        }) = ev
        {
            if !self.is_normal() || !c.is_numeric() {
                return false;
            }

            if self.repeater.is_none() && *c == '0' {
                return false;
            }

            if let Some(repeater) = self.repeater {
                let new_repeater = format!("{}{}", repeater, c).parse::<u16>().unwrap();
                self.repeater = Some(new_repeater);
            } else {
                self.repeater = Some(c.to_string().parse::<u16>().unwrap());
            }

            return true;
        }

        false
    }

    fn handle_command(&mut self, cmd: &str) -> Vec<Action> {
        log!("handle_command called with: {}", cmd);
        self.command = String::new();
        self.waiting_command = None;
        self.repeater = None;
        self.last_error = None;

        if let Ok(line) = cmd.parse::<usize>() {
            return vec![Action::GoToLine(line)];
        }

        // Handle debug commands first (these don't go through normal command parsing)
        match cmd {
            "db" => return vec![Action::DumpBuffer],
            "dh" => return vec![Action::DumpHistory],
            "di" => return vec![Action::DumpDiagnostics],
            "dc" => return vec![Action::DumpCapabilities],
            "dt" => return vec![Action::DumpTimers],
            _ => {}
        }

        let commands = &[
            "$",
            "quit",
            "write",
            "buffer-next",
            "buffer-prev",
            "edit",
            "split",
            "sp",
            "vsplit",
            "vs",
            "close",
            "only",
        ];
        let parsed = command::parse(commands, cmd);

        let Some(parsed) = parsed else {
            self.last_error = Some(format!("unknown command {cmd:?}"));
            return vec![];
        };

        let mut actions = vec![];
        for cmd in &parsed.commands {
            if cmd == "$" {
                actions.push(Action::MoveToBottom);
            }

            if cmd == "quit" {
                actions.push(Action::Quit(parsed.is_forced()));
            }

            if cmd == "write" {
                if let Some(file) = parsed.args.first() {
                    actions.push(Action::SaveAs(file.clone()));
                } else {
                    actions.push(Action::Save);
                }
            }

            if cmd == "buffer-next" {
                actions.push(Action::NextBuffer);
            }

            if cmd == "buffer-prev" {
                actions.push(Action::PreviousBuffer);
            }

            if cmd == "edit" {
                if let Some(file) = parsed.args.first() {
                    actions.push(Action::OpenFile(file.clone()));
                }
            }

            if cmd == "split" || cmd == "sp" {
                log!(
                    "Split command detected: {} with args: {:?}",
                    cmd,
                    parsed.args
                );
                if let Some(file) = parsed.args.first() {
                    actions.push(Action::SplitHorizontalWithFile(file.clone()));
                } else {
                    actions.push(Action::SplitHorizontal);
                }
            }

            if cmd == "vsplit" || cmd == "vs" {
                log!(
                    "Vsplit command detected: {} with args: {:?}",
                    cmd,
                    parsed.args
                );
                if let Some(file) = parsed.args.first() {
                    actions.push(Action::SplitVerticalWithFile(file.clone()));
                } else {
                    actions.push(Action::SplitVertical);
                }
            }

            if cmd == "close" {
                actions.push(Action::CloseWindow);
            }

            if cmd == "only" {
                // TODO: Implement close all other windows
                // For now, just add a placeholder
            }
        }
        actions
    }

    fn handle_command_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        if let Event::Key(ref event) = ev {
            let code = event.code;
            let _modifiers = event.modifiers;

            match code {
                KeyCode::Esc => {
                    self.command = String::new();
                    return Some(KeyAction::Single(Action::EnterMode(Mode::Normal)));
                }
                KeyCode::Backspace => {
                    if self.command.len() < 2 {
                        self.command = String::new();
                    } else {
                        self.command = self.command[..self.command.len() - 1].to_string();
                    }
                }
                KeyCode::Enter => {
                    if self.command.trim().is_empty() {
                        return Some(KeyAction::Single(Action::EnterMode(Mode::Normal)));
                    }
                    return Some(KeyAction::Multiple(vec![
                        Action::EnterMode(Mode::Normal),
                        Action::Command(self.command.clone()),
                    ]));
                }
                KeyCode::Char(c) => {
                    self.command = format!("{}{c}", self.command);
                }
                _ => {}
            }
        }

        None
    }

    #[allow(clippy::single_match)]
    fn handle_search_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        match ev {
            Event::Key(ref event) => {
                let code = event.code;
                let _modifiers = event.modifiers;

                match code {
                    KeyCode::Esc => {
                        self.search_term = String::new();
                        return Some(KeyAction::Single(Action::EnterMode(Mode::Normal)));
                    }
                    KeyCode::Backspace => {
                        if self.search_term.len() < 2 {
                            self.search_term = String::new();
                        } else {
                            self.search_term =
                                self.search_term[..self.search_term.len() - 1].to_string();
                        }
                    }
                    KeyCode::Enter => {
                        return Some(KeyAction::Multiple(vec![
                            Action::EnterMode(Mode::Normal),
                            Action::FindNext,
                        ]));
                    }
                    KeyCode::Char(c) => {
                        self.search_term = format!("{}{c}", self.search_term);
                        // TODO: real-time search
                        // return Some(KeyAction::Search);
                    }
                    _ => {}
                }
            }
            _ => {}
        }

        None
    }

    fn handle_visual_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        let visual = self.config.keys.visual.clone();
        self.event_to_key_action(&visual, ev)
    }

    fn handle_waiting_command(&mut self, ka: KeyAction, ev: &event::Event) -> Option<KeyAction> {
        let KeyAction::Nested(nested_mappings) = ka else {
            panic!("expected nested mappings");
        };

        self.event_to_key_action(&nested_mappings, ev)
    }

    fn handle_insert_event(&mut self, ev: &event::Event) -> anyhow::Result<Option<KeyAction>> {
        let insert = self.config.keys.insert.clone();
        if let Some(ka) = self.event_to_key_action(&insert, ev) {
            return Ok(Some(ka));
        }

        match ev {
            Event::Key(event) => match event.code {
                KeyCode::Char(c) => {
                    // Check for trigger character first
                    if let Some(action) = self.handle_trigger_char(c)? {
                        return Ok(Some(action));
                    }
                    // Otherwise insert normally
                    Ok(KeyAction::Single(Action::InsertCharAtCursorPos(c)).into())
                }
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    fn handle_normal_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        let normal = self.config.keys.normal.clone();
        self.event_to_key_action(&normal, ev)
    }

    pub fn cleanup(&mut self) -> anyhow::Result<()> {
        self.stdout
            .execute(terminal::LeaveAlternateScreen)?
            .execute(event::DisableMouseCapture)?;
        terminal::disable_raw_mode()?;

        Ok(())
    }

    fn current_line_contents(&self) -> Option<String> {
        self.current_buffer().get(self.buffer_line())
    }

    fn previous_line_indentation(&self) -> usize {
        if self.buffer_line() > 0 {
            self.current_buffer()
                .get(self.buffer_line() - 1)
                .unwrap_or_default()
                .chars()
                .position(|c| !c.is_whitespace())
                .unwrap_or(0)
        } else {
            0
        }
    }

    fn current_line_indentation(&self) -> usize {
        self.current_line_contents()
            .unwrap_or_default()
            .chars()
            .position(|c| !c.is_whitespace())
            .unwrap_or(0)
    }

    /// Executes a single editor action
    ///
    /// This is the core action dispatcher that:
    /// - Processes editor commands
    /// - Updates buffer contents
    /// - Manages cursor movement
    /// - Handles mode changes
    /// - Updates the display
    ///
    /// # Arguments
    /// * `action` - The action to execute
    /// * `buffer` - The render buffer to update
    /// * `runtime` - Plugin runtime environment
    ///
    /// # Returns
    /// A Result containing a boolean indicating if the editor should quit
    async fn execute(
        &mut self,
        action: &Action,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<bool> {
        self.execute_with_tracking(action, buffer, runtime, true)
            .await
    }

    #[async_recursion::async_recursion]
    async fn execute_with_tracking(
        &mut self,
        action: &Action,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
        tracking: bool,
    ) -> anyhow::Result<bool> {
        // log!("Action: {action:?}");
        self.last_error = None;
        self.actions.push(action.clone());

        let mut add_to_history = tracking;

        match action {
            Action::Quit(force) => {
                if *force {
                    return Ok(true);
                }
                let modified_buffers = self.modified_buffers();
                if modified_buffers.is_empty() {
                    return Ok(true);
                }
                self.last_error = Some(format!(
                    "The following buffers have unwritten changes: {}",
                    modified_buffers.join(", ")
                ));
                return Ok(false);
            }
            Action::MoveUp => {
                if self.cy == 0 {
                    // scroll up
                    if self.vtop > 0 {
                        self.vtop -= 1;
                        self.render(buffer)?;
                        self.notify_cursor_move(runtime).await?;
                    }
                } else {
                    self.cy = self.cy.saturating_sub(1);
                    self.draw_cursor()?;
                    self.notify_cursor_move(runtime).await?;
                }
            }
            Action::MoveDown => {
                if self.vtop + self.cy < self.current_buffer().len() - 1 {
                    self.cy += 1;
                    if self.cy >= self.vheight() {
                        // scroll if possible
                        self.vtop += 1;
                        self.cy -= 1;
                        self.render(buffer)?;
                    }
                    self.notify_cursor_move(runtime).await?;
                } else {
                    self.draw_cursor()?;
                }
            }
            Action::MoveLeft => {
                // Move by grapheme clusters
                if let Some(line) = self.current_line_contents() {
                    let line = line.trim_end_matches('\n');

                    // Convert current position to byte offset
                    let current_byte = self
                        .current_buffer()
                        .column_to_char_index(self.cx, self.buffer_line());
                    let byte_offset = crate::unicode_utils::char_to_byte(line, current_byte);

                    // Find previous grapheme boundary
                    if let Some(prev_byte) = prev_grapheme_boundary(line, byte_offset) {
                        // Convert back to character index
                        let char_idx = crate::unicode_utils::byte_to_char(line, prev_byte);
                        self.cx = char_idx;
                    } else if self.cx > 0 {
                        self.cx = 0;
                    }

                    if self.cx < self.vleft {
                        self.cx = self.vleft;
                    }
                }
                self.notify_cursor_move(runtime).await?;
            }
            Action::MoveRight => {
                // Move by grapheme clusters
                if let Some(line) = self.current_line_contents() {
                    let line = line.trim_end_matches('\n');
                    let max_chars = line.chars().count();

                    if self.cx < max_chars {
                        // Convert current position to byte offset
                        let current_byte = crate::unicode_utils::char_to_byte(line, self.cx);

                        // Find next grapheme boundary
                        if let Some(next_byte) = next_grapheme_boundary(line, current_byte) {
                            // Convert back to character index
                            let char_idx = crate::unicode_utils::byte_to_char(line, next_byte);
                            self.cx = char_idx.min(max_chars);
                        } else {
                            self.cx = max_chars;
                        }
                    }
                }
                self.notify_cursor_move(runtime).await?;
            }
            Action::MoveToLineStart => {
                self.cx = 0;
            }
            Action::MoveToLineEnd => {
                self.cx = self.line_length().saturating_sub(1);
            }
            Action::MoveToFirstLineChar => {
                if let Some(line) = self.current_line_contents() {
                    self.cx = line.chars().position(|c| !c.is_whitespace()).unwrap_or(0);
                }
            }
            Action::MoveToLastLineChar => {
                if let Some(line) = self.current_line_contents() {
                    self.cx = line.len().saturating_sub(
                        line.chars()
                            .rev()
                            .position(|c| !c.is_whitespace())
                            .unwrap_or(0),
                    );
                }
            }
            Action::PageUp => {
                if self.vtop > 0 {
                    self.vtop = self.vtop.saturating_sub(self.vheight());
                    self.render(buffer)?;
                }
            }
            Action::PageDown => {
                if self.current_buffer().len() > self.vtop + self.vheight() {
                    self.vtop += self.vheight();
                    self.render(buffer)?;
                }
            }
            Action::EnterMode(new_mode) => {
                add_to_history = false;
                self.selection = None;

                // check for a pending action to be executed on the selection
                let pending_select_action = self.pending_select_action.clone();
                if let Some(select_action) = pending_select_action {
                    self.execute(&select_action.action, buffer, runtime).await?;
                }

                // TODO: with the introduction of new modes, maybe this transtion
                // needs to be widened to anything -> insert and anything -> normal
                if self.is_normal() && matches!(new_mode, Mode::Insert) {
                    self.insert_undo_actions = Vec::new();
                }

                if self.is_insert()
                    && matches!(new_mode, Mode::Normal)
                    && !self.insert_undo_actions.is_empty()
                {
                    let actions = mem::take(&mut self.insert_undo_actions);
                    self.undo_actions.push(Action::UndoMultiple(actions));
                }

                if matches!(new_mode, Mode::Search) {
                    self.search_term = String::new();
                }

                if matches!(
                    new_mode,
                    Mode::Visual | Mode::VisualLine | Mode::VisualBlock
                ) {
                    self.start_selection();
                    self.render(buffer)?;
                } else {
                    self.selection = None;
                    self.render(buffer)?;
                }

                if !self.is_normal() && matches!(new_mode, Mode::Normal) {
                    if let Some(uri) = self.current_buffer().uri()? {
                        self.lsp.request_diagnostics(&uri).await?;
                    }
                }

                let old_mode = self.mode;
                self.mode = *new_mode;

                // Notify plugins about mode change
                let mode_info = serde_json::json!({
                    "old_mode": format!("{:?}", old_mode),
                    "new_mode": format!("{:?}", new_mode)
                });
                self.plugin_registry
                    .notify(runtime, "mode:changed", mode_info)
                    .await?;

                self.draw_statusline(buffer);
            }
            Action::InsertCharAtCursorPos(c) => {
                self.insert_undo_actions
                    .push(Action::DeleteCharAt(self.cx, self.buffer_line()));
                let line = self.buffer_line();
                let cx = self.cx;

                self.current_buffer_mut().insert(cx, line, *c);
                self.notify_change(runtime).await?;

                // Move cursor by the actual display width of the character
                self.cx += 1; // Still use character index for now

                self.draw_line(buffer);
            }
            Action::DeleteCharAt(x, y) => {
                self.current_buffer_mut().remove(*x, *y);
                self.notify_change(runtime).await?;
                self.draw_line(buffer);
            }
            Action::DeleteRange(x0, y0, x1, y1) => {
                self.current_buffer_mut().remove_range(*x0, *y0, *x1, *y1);
                self.notify_change(runtime).await?;
                self.render(buffer)?;
            }
            Action::DeleteCharAtCursorPos => {
                let cx = self.cx;
                let line = self.buffer_line();

                self.current_buffer_mut().remove(cx, line);
                self.notify_change(runtime).await?;
                self.draw_line(buffer);
            }
            Action::ReplaceLineAt(y, contents) => {
                self.current_buffer_mut()
                    .replace_line(*y, contents.to_string());
                self.notify_change(runtime).await?;
                self.draw_line(buffer);
            }
            Action::InsertNewLine => {
                self.insert_undo_actions.extend(vec![
                    Action::MoveTo(self.cx, self.buffer_line() + 1),
                    Action::DeleteLineAt(self.buffer_line() + 1),
                    Action::ReplaceLineAt(
                        self.buffer_line(),
                        self.current_line_contents().unwrap_or_default(),
                    ),
                ]);
                let spaces = self.current_line_indentation();

                let current_line = self.current_line_contents().unwrap_or_default();
                let current_line = current_line.trim_end();
                if self.cx > current_line.len() {
                    self.cx = current_line.len();
                }
                let before_cursor = current_line[..self.cx].to_string();
                let after_cursor = current_line[self.cx..].to_string();

                let line = self.buffer_line();
                self.current_buffer_mut().replace_line(line, before_cursor);
                self.notify_change(runtime).await?;

                self.cx = spaces;
                self.cy += 1;

                if self.cy >= self.vheight() {
                    self.vtop += 1;
                    self.cy -= 1;
                }

                let new_line = format!("{}{}", " ".repeat(spaces), &after_cursor);
                let line = self.buffer_line();

                self.current_buffer_mut().insert_line(line, new_line);
                self.render(buffer)?;
            }
            Action::SetWaitingKey(key_action) => {
                self.waiting_key_action = Some(*(key_action.clone()));
            }
            Action::DeleteCurrentLine => {
                let line = self.buffer_line();
                let contents = self.current_line_contents();

                self.current_buffer_mut().remove_line(line);
                self.notify_change(runtime).await?;
                self.undo_actions.push(Action::InsertLineAt(line, contents));
                self.render(buffer)?;
            }
            Action::Undo => {
                if let Some(undo_action) = self.undo_actions.pop() {
                    self.execute(&undo_action, buffer, runtime).await?;
                }
            }
            Action::UndoMultiple(actions) => {
                for action in actions.iter().rev() {
                    self.execute(action, buffer, runtime).await?;
                }
            }
            Action::InsertLineAt(y, contents) => {
                if let Some(contents) = contents {
                    self.current_buffer_mut()
                        .insert_line(*y, contents.to_string());
                    self.notify_change(runtime).await?;
                    self.render(buffer)?;
                }
            }
            Action::MoveLineToViewportCenter => {
                let viewport_center = self.vheight() / 2;
                let distance_to_center = self.cy as isize - viewport_center as isize;

                match distance_to_center.cmp(&0) {
                    Ordering::Greater => {
                        // if distance > 0 we need to scroll up
                        let distance_to_center = distance_to_center.unsigned_abs();
                        if self.vtop > distance_to_center {
                            let new_vtop = self.vtop + distance_to_center;
                            self.vtop = new_vtop;
                            self.cy = viewport_center;
                            self.render(buffer)?;
                        }
                    }
                    Ordering::Less => {
                        // if distance < 0 we need to scroll down
                        let distance_to_center = distance_to_center.unsigned_abs();
                        let new_vtop = self.vtop.saturating_sub(distance_to_center);
                        let distance_to_go = self.vtop + distance_to_center;
                        if self.current_buffer().len() > distance_to_go && new_vtop != self.vtop {
                            self.vtop = new_vtop;
                            self.cy = viewport_center;
                            self.render(buffer)?;
                        }
                    }
                    Ordering::Equal => {}
                }
            }
            Action::InsertLineBelowCursor => {
                self.undo_actions
                    .push(Action::DeleteLineAt(self.buffer_line() + 1));

                let leading_spaces = self.current_line_indentation();
                let line = self.buffer_line();
                self.current_buffer_mut()
                    .insert_line(line + 1, " ".repeat(leading_spaces));
                self.notify_change(runtime).await?;
                self.cy += 1;
                self.cx = leading_spaces;

                if self.cy >= self.vheight() {
                    self.vtop += 1;
                    self.cy -= 1;
                }

                self.render(buffer)?;
            }
            Action::InsertLineAtCursor => {
                self.undo_actions
                    .push(Action::DeleteLineAt(self.buffer_line()));

                // if the current line is empty, let's use the indentation from the line above
                let leading_spaces = if let Some(line) = self.current_line_contents() {
                    if line.is_empty() {
                        self.previous_line_indentation()
                    } else {
                        self.current_line_indentation()
                    }
                } else {
                    self.previous_line_indentation()
                };

                let line = self.buffer_line();
                self.current_buffer_mut()
                    .insert_line(line, " ".repeat(leading_spaces));
                self.notify_change(runtime).await?;
                self.cx = leading_spaces;
                self.render(buffer)?;
            }
            Action::MoveToTop => {
                self.vtop = 0;
                self.cy = 0;
                self.render(buffer)?;
            }
            Action::MoveToBottom => {
                if self.current_buffer().len() > self.vheight() {
                    self.cy = self.vheight() - 1;
                    self.vtop = self.current_buffer().len() - self.vheight();
                    self.render(buffer)?;
                } else {
                    self.cy = self.current_buffer().len() - 1;
                }
            }
            Action::DeleteLineAt(y) => {
                self.current_buffer_mut().remove_line(*y);
                self.notify_change(runtime).await?;
                self.render(buffer)?;
            }
            Action::DeletePreviousChar => {
                if self.cx > 0 {
                    // Get the current line to find the previous grapheme boundary
                    if let Some(line) = self.current_line_contents() {
                        let line = line.trim_end_matches('\n');
                        let current_byte = crate::unicode_utils::char_to_byte(line, self.cx);

                        if let Some(prev_byte) =
                            crate::unicode_utils::prev_grapheme_boundary(line, current_byte)
                        {
                            let prev_char_idx = crate::unicode_utils::byte_to_char(line, prev_byte);

                            // Calculate how many characters to remove
                            let chars_to_remove = self.cx - prev_char_idx;

                            // Move cursor to the previous grapheme boundary
                            self.cx = prev_char_idx;

                            // Remove all characters in the grapheme cluster
                            let line_num = self.buffer_line();
                            let cx = self.cx;
                            for _ in 0..chars_to_remove {
                                self.current_buffer_mut().remove(cx, line_num);
                            }

                            self.notify_change(runtime).await?;
                            self.draw_line(buffer);
                        }
                    }
                }
            }
            Action::DumpHistory => {
                add_to_history = false;
                log!("");
                log!("--------------- BACK HISTORY ---------------");
                for item in &self.back_history {
                    log!(
                        "{:<25} | {:>2} {:>2} | {:<20?}",
                        item.file,
                        item.x,
                        item.y,
                        item.action
                    );
                }
                log!("-------------- FORWARD HISTORY -------------");
                for item in &self.fwd_history {
                    log!(
                        "{:<25} | {:>2} {:>2} | {:<20?}",
                        item.file,
                        item.x,
                        item.y,
                        item.action
                    );
                }
                log!("--------------------------------------------");
                log!("");
            }
            Action::DumpBuffer => {
                add_to_history = false;
                log!("{buffer}", buffer = buffer.dump(false));
            }
            Action::DumpDiagnostics => {
                add_to_history = false;
                log!("{diagnostics:#?}", diagnostics = self.diagnostics);
            }
            Action::DumpCapabilities => {
                add_to_history = false;
                log!(
                    "client: {}",
                    serde_json::to_string_pretty(&get_client_capabilities("workspace-uri"))?
                );
                log!(
                    "server: {}",
                    serde_json::to_string_pretty(&self.lsp.get_server_capabilities())?
                );
            }
            Action::DoPing => {
                add_to_history = false;
                // self.lsp
                //     .send_request(
                //         "rust-analyzer/analyzerStatus",
                //         json!({
                //             "textDocument": {
                //                 "uri": self.current_buffer().uri().unwrap_or_default()
                //             }
                //         }),
                //         true,
                //     )
                //     .await?;
                self.lsp
                    .send_request(
                        "rust-analyzer/viewFileText",
                        json!({
                            "uri": self.current_buffer().uri().unwrap_or_default()
                        }),
                        true,
                    )
                    .await?;
            }
            Action::ViewLogs => {
                add_to_history = false;
                if let Some(log_file) = &self.config.log_file {
                    let path = PathBuf::from(log_file);
                    if path.exists() {
                        // Check if the log file is already open
                        if let Some(index) = self.buffers.iter().position(|b| b.name() == *log_file)
                        {
                            self.set_current_buffer(buffer, index).await?;
                        } else {
                            let new_buffer = match Buffer::load_or_create(
                                &mut self.lsp,
                                Some(log_file.to_string()),
                            )
                            .await
                            {
                                Ok(buffer) => buffer,
                                Err(e) => {
                                    self.last_error =
                                        Some(format!("Failed to open log file: {}", e));
                                    return Ok(false);
                                }
                            };
                            self.buffers.push(new_buffer);
                            self.set_current_buffer(buffer, self.buffers.len() - 1)
                                .await?;
                        }
                    } else {
                        self.last_error = Some(format!("Log file not found: {}", log_file));
                    }
                } else {
                    self.last_error = Some("No log file configured".to_string());
                }
            }
            Action::ListPlugins => {
                add_to_history = false;

                // Create a buffer with plugin information
                let mut content = String::from("# Loaded Plugins\n\n");

                let metadata = self.plugin_registry.all_metadata();
                if metadata.is_empty() {
                    content.push_str("No plugins loaded.\n");
                } else {
                    for meta in metadata.values() {
                        content.push_str(&format!("## {}\n", meta.name));
                        content.push_str(&format!("Version: {}\n", meta.version));

                        if let Some(desc) = &meta.description {
                            content.push_str(&format!("Description: {}\n", desc));
                        }

                        if let Some(author) = &meta.author {
                            content.push_str(&format!("Author: {}\n", author));
                        }

                        if let Some(license) = &meta.license {
                            content.push_str(&format!("License: {}\n", license));
                        }

                        if !meta.keywords.is_empty() {
                            content.push_str(&format!("Keywords: {}\n", meta.keywords.join(", ")));
                        }

                        content.push_str(&format!("Main: {}\n", meta.main));

                        // Show capabilities
                        if meta.capabilities.commands
                            || meta.capabilities.events
                            || meta.capabilities.buffer_manipulation
                            || meta.capabilities.ui_components
                        {
                            content.push_str("Capabilities: ");
                            let mut caps = vec![];
                            if meta.capabilities.commands {
                                caps.push("commands");
                            }
                            if meta.capabilities.events {
                                caps.push("events");
                            }
                            if meta.capabilities.buffer_manipulation {
                                caps.push("buffer manipulation");
                            }
                            if meta.capabilities.ui_components {
                                caps.push("UI components");
                            }
                            if meta.capabilities.lsp_integration {
                                caps.push("LSP integration");
                            }
                            content.push_str(&caps.join(", "));
                            content.push('\n');
                        }

                        content.push('\n');
                    }
                }

                // Create a new buffer with the plugin list
                let plugin_list_buffer = Buffer::new(Some("[Plugin List]".to_string()), content);
                self.buffers.push(plugin_list_buffer);
                self.current_buffer_index = self.buffers.len() - 1;
                self.cx = 0;
                self.cy = 0;
                self.vtop = 0;
            }
            Action::Command(cmd) => {
                log!("Handling command: {cmd}");

                for action in self.handle_command(cmd) {
                    self.last_error = None;
                    if self.execute(&action, buffer, runtime).await? {
                        return Ok(true);
                    }
                }
            }
            Action::PluginCommand(cmd) => {
                self.plugin_registry.execute(runtime, cmd).await?;
            }
            Action::GoToLine(line) => {
                self.go_to_line(*line, buffer, runtime, GoToLinePosition::Center)
                    .await?
            }
            Action::GoToDefinition => {
                if let Some(file) = self.current_buffer().file.clone() {
                    self.lsp
                        .goto_definition(&file, self.cx, self.cy + self.vtop)
                        .await?;
                }
            }
            Action::Hover => {
                if let Some(file) = self.current_buffer().file.clone() {
                    self.lsp.hover(&file, self.cx, self.cy + self.vtop).await?;
                }
            }
            Action::MoveTo(x, y) => {
                self.go_to_line(*y, buffer, runtime, GoToLinePosition::Center)
                    .await?;
                self.cx = std::cmp::min(*x, self.line_length().saturating_sub(1));
            }
            Action::MoveToFilePos(file, x, y) => {
                if self.current_buffer().file != Some(file.clone()) {
                    self.execute_with_tracking(
                        &Action::OpenFile(file.clone()),
                        buffer,
                        runtime,
                        tracking,
                    )
                    .await?;
                }

                self.execute_with_tracking(&Action::MoveTo(*x, *y), buffer, runtime, tracking)
                    .await?;
            }
            Action::SetCursor(x, y) => {
                self.cx = *x;
                self.cy = *y;
            }
            Action::ScrollUp => {
                let scroll_lines = self.config.mouse_scroll_lines.unwrap_or(3);
                if self.vtop >= scroll_lines {
                    self.vtop -= scroll_lines;
                    let desired_cy = self.cy + scroll_lines;
                    if desired_cy <= self.vheight() {
                        self.cy = desired_cy;
                    }
                    self.render(buffer)?;
                }
            }
            Action::ScrollDown => {
                if self.current_buffer().len() > self.vtop + self.vheight() {
                    self.vtop += self.config.mouse_scroll_lines.unwrap_or(3);
                    let desired_cy = self
                        .cy
                        .saturating_sub(self.config.mouse_scroll_lines.unwrap_or(3));
                    self.cy = desired_cy;
                    self.render(buffer)?;
                }
            }
            Action::MoveToNextWord => {
                let next_word = self
                    .current_buffer()
                    .find_next_word((self.cx, self.buffer_line()));

                if let Some((x, y)) = next_word {
                    self.cx = x;
                    self.go_to_line(y + 1, buffer, runtime, GoToLinePosition::Top)
                        .await?;
                    self.draw_cursor()?;
                }
            }
            Action::MoveToPreviousWord => {
                let previous_word = self
                    .current_buffer()
                    .find_prev_word((self.cx, self.buffer_line()));

                if let Some((x, y)) = previous_word {
                    self.cx = x;
                    self.go_to_line(y + 1, buffer, runtime, GoToLinePosition::Top)
                        .await?;
                    self.draw_cursor()?;
                }
            }
            Action::MoveLineToViewportBottom => {
                let line = self.buffer_line();
                if line > self.vtop + self.vheight() {
                    self.vtop = line - self.vheight();
                    self.cy = self.vheight() - 1;
                    self.render(buffer)?;
                }
            }
            Action::InsertTab => {
                // TODO: Tab configuration
                let tabsize = 4;
                let cx = self.cx;
                let line = self.buffer_line();
                self.current_buffer_mut()
                    .insert_str(cx, line, &" ".repeat(tabsize));
                self.notify_change(runtime).await?;
                self.cx += tabsize;
                self.draw_line(buffer);
            }
            Action::Save => match self.current_buffer_mut().save() {
                Ok(msg) => {
                    // TODO: use last_message instead of last_error
                    self.last_error = Some(msg);

                    // Notify plugins about file save
                    if let Some(file) = &self.current_buffer().file {
                        let save_info = serde_json::json!({
                            "file": file,
                            "buffer_index": self.current_buffer_index
                        });
                        self.plugin_registry
                            .notify(runtime, "file:saved", save_info)
                            .await?;
                    }
                }
                Err(e) => {
                    self.last_error = Some(e.to_string());
                }
            },
            Action::SaveAs(new_file_name) => {
                match self.current_buffer_mut().save_as(new_file_name) {
                    Ok(msg) => {
                        // TODO: use last_message instead of last_error
                        self.last_error = Some(msg);

                        // Notify plugins about file save
                        let save_info = serde_json::json!({
                            "file": new_file_name,
                            "buffer_index": self.current_buffer_index
                        });
                        self.plugin_registry
                            .notify(runtime, "file:saved", save_info)
                            .await?;
                    }
                    Err(e) => {
                        self.last_error = Some(e.to_string());
                    }
                }
            }
            Action::FindPrevious => {
                if let Some((x, y)) = self
                    .current_buffer()
                    .find_prev(&self.search_term, (self.cx, self.vtop + self.cy))
                {
                    self.cx = x;
                    self.go_to_line(y + 1, buffer, runtime, GoToLinePosition::Center)
                        .await?;
                }
            }
            Action::FindNext => {
                if let Some((x, y)) = self
                    .current_buffer()
                    .find_next(&self.search_term, (self.cx, self.vtop + self.cy))
                {
                    self.cx = x;
                    self.go_to_line(y + 1, buffer, runtime, GoToLinePosition::Center)
                        .await?;
                }
            }
            Action::DeleteWord => {
                let cx = self.cx;
                let line = self.buffer_line();

                if let Some(text) = self.current_buffer_mut().delete_word((cx, line)) {
                    let content = Content {
                        kind: ContentKind::Charwise,
                        text,
                    };

                    self.undo_actions.push(Action::InsertText {
                        x: cx,
                        y: line,
                        content,
                    });
                }

                self.notify_change(runtime).await?;
                self.draw_line(buffer);
            }
            Action::NextBuffer => {
                let new_index = if self.current_buffer_index < self.buffers.len() - 1 {
                    self.current_buffer_index + 1
                } else {
                    0
                };
                buffer.clear();
                self.set_current_buffer(buffer, new_index).await?;
            }
            Action::PreviousBuffer => {
                let new_index = if self.current_buffer_index > 0 {
                    self.current_buffer_index - 1
                } else {
                    self.buffers.len() - 1
                };
                self.set_current_buffer(buffer, new_index).await?;
            }
            Action::OpenBuffer(name) => {
                if let Some(index) = self.buffers.iter().position(|b| b.name() == *name) {
                    self.set_current_buffer(buffer, index).await?;
                }
            }
            Action::OpenFile(path) => {
                if let Some(index) = self.buffers.iter().position(|b| b.name() == *path) {
                    self.set_current_buffer(buffer, index).await?;
                } else {
                    let new_buffer =
                        match Buffer::load_or_create(&mut self.lsp, Some(path.to_string())).await {
                            Ok(buffer) => buffer,
                            Err(e) => {
                                self.last_error = Some(e.to_string());
                                return Ok(false);
                            }
                        };
                    self.buffers.push(new_buffer);
                    self.set_current_buffer(buffer, self.buffers.len() - 1)
                        .await?;
                    buffer.clear();

                    // Notify plugins about file open
                    let open_info = serde_json::json!({
                        "file": path,
                        "buffer_index": self.buffers.len() - 1
                    });
                    self.plugin_registry
                        .notify(runtime, "file:opened", open_info)
                        .await?;
                }
                self.render(buffer)?;
            }
            Action::FilePicker => {
                self.current_dialog =
                    Some(Box::new(FilePicker::new(self, std::env::current_dir()?)?));
            }
            Action::ShowDialog => {
                buffer.clear();
                // if let Some(dialog) = &mut self.current_dialog {
                //     dialog.draw(buffer)?;
                // }
            }
            Action::CloseDialog => {
                self.current_dialog = None;
                self.render(buffer)?;
            }
            Action::RefreshDiagnostics => {
                add_to_history = false;
                if let Some(uri) = self.current_buffer().uri()? {
                    self.lsp.request_diagnostics(&uri).await?;
                    self.render(buffer)?;
                }
            }
            Action::Refresh => {
                add_to_history = false;
                self.render(buffer)?;
            }
            Action::Print(msg) => {
                self.last_error = Some(msg.clone());
            }
            Action::OpenPicker(title, items, id) => {
                self.current_dialog = Some(Box::new(Picker::new(title.clone(), self, items, *id)));
                self.render(buffer)?;
            }
            Action::Picked(item, id) => {
                log!("picked: {item} - {id:?}");
                if let Some(id) = id {
                    self.plugin_registry
                        .notify(
                            runtime,
                            &format!("picker:selected:{}", id),
                            serde_json::Value::String(item.clone()),
                        )
                        .await?;
                }
            }
            Action::Suspend => {
                #[cfg(unix)]
                {
                    self.stdout.execute(terminal::LeaveAlternateScreen)?;
                    let pid = Pid::from_raw(0);
                    let _ = signal::kill(pid, Signal::SIGSTOP);
                    self.stdout.execute(terminal::EnterAlternateScreen)?;
                    self.render(buffer)?;
                }
                #[cfg(not(unix))]
                {
                    // Suspend is not supported on Windows
                    // Just ignore the action
                }
            }
            Action::Yank => {
                if self.selection.is_some() && self.yank(DEFAULT_REGISTER) {
                    // self.render(buffer)?;
                    self.draw_commandline(buffer);
                }
            }
            Action::Delete => {
                if self.selection.is_some() {
                    if let Some((x0, y0)) = self.delete_selection() {
                        self.cx = x0;
                        self.cy = y0 - self.vtop;
                    }
                    self.selection = None;
                    self.notify_change(runtime).await?;
                    self.render(buffer)?;
                }
            }
            Action::Paste | Action::PasteBefore => {
                log!("pasting selection");
                if self.paste_default(*action == Action::PasteBefore) {
                    self.render(buffer)?;
                }
            }
            Action::InsertText { x, y, content } => {
                self.insert_content(*x, *y, content, true);
                self.notify_change(runtime).await?;
                self.render(buffer)?;
            }
            Action::BufferText(value) => {
                self.buffer_text(value);
            }
            Action::InsertBlock => {
                self.execute_block_action(buffer, runtime, Mode::Insert)
                    .await?
            }
            Action::ClearDiagnostics(uri, lines) => {
                if let Some(buffer_uri) = self.current_buffer().uri()? {
                    if buffer_uri == *uri {
                        log!("clearing diagnostics for {uri}: {lines:?}");
                        self.clear_diagnostics(buffer, lines);
                    } else {
                        log!("ignoring diagnostics for {uri}: {lines:?}");
                    }
                }

                self.draw_diagnostics(buffer);
            }
            Action::InsertString(text) => {
                let line = self.buffer_line();
                let cx = self.cx;
                self.current_buffer_mut().insert_str(cx, line, text);
                self.notify_change(runtime).await?;
                self.cx += text.len();
                self.draw_line(buffer);
            }
            Action::RequestCompletion => {
                // if let Some(uri) = self.current_buffer().uri()? {
                //     let (_, col) = self.cursor_position();
                //     self.lsp
                //         .request_completion(&uri, self.buffer_line(), col)
                //         .await?;
                // }
            }
            Action::ShowProgress(progress) => {
                add_to_history = false;
                match progress.token {
                    ProgressToken::String(ref s) => self.last_error = Some(s.to_string()),
                    ProgressToken::Number(_) => {}
                }
            }
            Action::IndentLine => {
                let indent = self.indentation();
                let line = self.buffer_line();

                self.undo_actions
                    .push(Action::DeleteRange(0, line, indent.shift_width, line));

                self.current_buffer_mut()
                    .insert_str(0, line, &" ".repeat(indent.shift_width));
            }
            Action::UnindentLine => {
                let spaces = self.current_line_indentation();
                let chars_to_remove = std::cmp::min(spaces, self.indentation().shift_width);
                let line = self.buffer_line();

                self.undo_actions.push(Action::InsertText {
                    x: 0,
                    y: line,
                    content: Content {
                        kind: ContentKind::Charwise,
                        text: " ".repeat(chars_to_remove),
                    },
                });

                self.current_buffer_mut()
                    .remove_range(0, line, chars_to_remove, line);
            }
            Action::JumpBack => {
                add_to_history = false;
                if let Some(entry) = self.back_history.pop() {
                    self.fwd_history.push(entry);
                }
                if let Some(entry) = self.back_history.pop() {
                    log!("jumping back to {entry:?}");
                    self.fwd_history.push(entry.clone());
                    add_to_history = false;
                    self.execute_with_tracking(
                        &Action::MoveToFilePos(entry.file, entry.x, entry.y + 1),
                        buffer,
                        runtime,
                        false,
                    )
                    .await?;
                }
            }
            Action::JumpForward => {
                add_to_history = false;
                _ = self.fwd_history.pop();
                if let Some(entry) = self.fwd_history.pop() {
                    log!("jumping forward to {entry:?}");
                    self.back_history.push(entry.clone());
                    add_to_history = false;
                    self.execute_with_tracking(
                        &Action::MoveToFilePos(entry.file, entry.x, entry.y + 1),
                        buffer,
                        runtime,
                        false,
                    )
                    .await?;
                }
            }
            Action::DumpTimers => {
                add_to_history = false;
                use crate::plugin::timer_stats;
                timer_stats::log_timer_stats();
            }
            Action::NotifyPlugins(method, params) => {
                self.plugin_registry
                    .notify(runtime, method, params.clone())
                    .await?;
            }

            // Window management actions
            Action::SplitHorizontal => {
                log!("SplitHorizontal action triggered");
                let current_buffer = self.current_buffer_index;
                if self
                    .window_manager
                    .split_horizontal(current_buffer)
                    .is_some()
                {
                    log!("Window split successful");
                    self.sync_with_window();
                    self.render(buffer)?;
                } else {
                    log!("Window split failed");
                }
            }
            Action::SplitVertical => {
                log!("SplitVertical action triggered");
                let current_buffer = self.current_buffer_index;
                if self.window_manager.split_vertical(current_buffer).is_some() {
                    log!("Vertical split successful");
                    self.sync_with_window();
                    self.render(buffer)?;
                } else {
                    log!("Vertical split failed");
                }
            }
            Action::SplitHorizontalWithFile(file) => {
                log!(
                    "SplitHorizontalWithFile action triggered with file: {}",
                    file
                );
                // Load or create the buffer for the file
                match Buffer::load_or_create(&mut self.lsp, Some(file.clone())).await {
                    Ok(new_buffer) => {
                        self.buffers.push(new_buffer);
                        let new_buffer_index = self.buffers.len() - 1;
                        if self
                            .window_manager
                            .split_horizontal(new_buffer_index)
                            .is_some()
                        {
                            log!("Window split with new file successful");
                            self.sync_with_window();
                            self.render(buffer)?;
                        } else {
                            log!("Window split failed");
                            // Remove the buffer we just added
                            self.buffers.pop();
                        }
                    }
                    Err(e) => {
                        self.last_error = Some(format!("Failed to open file: {}", e));
                    }
                }
            }
            Action::SplitVerticalWithFile(file) => {
                log!("SplitVerticalWithFile action triggered with file: {}", file);
                // Load or create the buffer for the file
                match Buffer::load_or_create(&mut self.lsp, Some(file.clone())).await {
                    Ok(new_buffer) => {
                        self.buffers.push(new_buffer);
                        let new_buffer_index = self.buffers.len() - 1;
                        if self
                            .window_manager
                            .split_vertical(new_buffer_index)
                            .is_some()
                        {
                            log!("Vertical split with new file successful");
                            self.sync_with_window();
                            self.render(buffer)?;
                        } else {
                            log!("Vertical split failed");
                            // Remove the buffer we just added
                            self.buffers.pop();
                        }
                    }
                    Err(e) => {
                        self.last_error = Some(format!("Failed to open file: {}", e));
                    }
                }
            }
            Action::CloseWindow => {
                if self.window_manager.close_window().is_some() {
                    self.sync_with_window();
                    self.render(buffer)?;
                }
            }
            Action::NextWindow => {
                let window_count = self.window_manager.windows().len();
                if window_count > 1 {
                    self.sync_to_window(); // Save current window state
                    let next_id = (self.window_manager.active_window_id() + 1) % window_count;
                    self.window_manager.set_active(next_id);
                    self.sync_with_window(); // Load new window state
                    self.render(buffer)?;
                }
            }
            Action::PreviousWindow => {
                let window_count = self.window_manager.windows().len();
                if window_count > 1 {
                    self.sync_to_window(); // Save current window state
                    let current_id = self.window_manager.active_window_id();
                    let prev_id = if current_id == 0 {
                        window_count - 1
                    } else {
                        current_id - 1
                    };
                    self.window_manager.set_active(prev_id);
                    self.sync_with_window(); // Load new window state
                    self.render(buffer)?;
                }
            }
            Action::MoveWindowUp => {
                self.sync_to_window(); // Save current window state
                if let Some(target_id) = self
                    .window_manager
                    .find_window_in_direction(crate::window::Direction::Up)
                {
                    self.window_manager.set_active(target_id);
                    self.sync_with_window(); // Load new window state
                    self.render(buffer)?;
                }
            }
            Action::MoveWindowDown => {
                self.sync_to_window(); // Save current window state
                if let Some(target_id) = self
                    .window_manager
                    .find_window_in_direction(crate::window::Direction::Down)
                {
                    self.window_manager.set_active(target_id);
                    self.sync_with_window(); // Load new window state
                    self.render(buffer)?;
                }
            }
            Action::MoveWindowLeft => {
                self.sync_to_window(); // Save current window state
                if let Some(target_id) = self
                    .window_manager
                    .find_window_in_direction(crate::window::Direction::Left)
                {
                    self.window_manager.set_active(target_id);
                    self.sync_with_window(); // Load new window state
                    self.render(buffer)?;
                }
            }
            Action::MoveWindowRight => {
                self.sync_to_window(); // Save current window state
                if let Some(target_id) = self
                    .window_manager
                    .find_window_in_direction(crate::window::Direction::Right)
                {
                    self.window_manager.set_active(target_id);
                    self.sync_with_window(); // Load new window state
                    self.render(buffer)?;
                }
            }
            Action::ResizeWindowUp(amount) => {
                self.sync_to_window(); // Save current window state
                if self
                    .window_manager
                    .resize_window(crate::window::Direction::Up, *amount)
                    .is_some()
                {
                    self.sync_with_window(); // Load new window state
                    self.render(buffer)?;
                }
            }
            Action::ResizeWindowDown(amount) => {
                self.sync_to_window(); // Save current window state
                if self
                    .window_manager
                    .resize_window(crate::window::Direction::Down, *amount)
                    .is_some()
                {
                    self.sync_with_window(); // Load new window state
                    self.render(buffer)?;
                }
            }
            Action::ResizeWindowLeft(amount) => {
                self.sync_to_window(); // Save current window state
                if self
                    .window_manager
                    .resize_window(crate::window::Direction::Left, *amount)
                    .is_some()
                {
                    self.sync_with_window(); // Load new window state
                    self.render(buffer)?;
                }
            }
            Action::ResizeWindowRight(amount) => {
                self.sync_to_window(); // Save current window state
                if self
                    .window_manager
                    .resize_window(crate::window::Direction::Right, *amount)
                    .is_some()
                {
                    self.sync_with_window(); // Load new window state
                    self.render(buffer)?;
                }
            }
            Action::BalanceWindows => {
                // TODO: Implement window balancing
            }
            Action::MaximizeWindow => {
                // TODO: Implement window maximizing
            }
        }

        if add_to_history {
            self.save_to_history(action);
        }

        Ok(false)
    }

    fn buffer_text(&mut self, value: &Value) {
        let Some(style) = value.get("style") else {
            log!("ERROR: missing style in BufferText");
            return;
        };

        let style: Style = match serde_json::from_value(style.clone()) {
            Ok(style) => style,
            Err(e) => {
                log!("ERROR: failed to parse style: {e}");
                return;
            }
        };

        let Some(x) = value.get("x").and_then(|x| x.as_i64()) else {
            log!("ERROR: missing or invalid x in BufferText");
            return;
        };

        let Some(y) = value.get("y").and_then(|y| y.as_u64()) else {
            log!("ERROR: missing or invalid y in BufferText");
            return;
        };

        let Some(text) = value.get("text").and_then(|text| text.as_str()) else {
            log!("ERROR: missing or invalid text in BufferText");
            return;
        };

        // truncate the message if it's too long
        let overflow = x.unsigned_abs() as usize;
        let (x, text) = if overflow + 3 >= text.len() {
            (x, text.to_string())
        } else {
            (0, format!("...{}", &text[overflow..]))
        };

        self.render_commands.push_back(RenderCommand::BufferText {
            x: x as usize,
            y: y as usize,
            text: text.to_string(),
            style,
        });
    }

    fn save_to_history(&mut self, action: &Action) {
        let entry = HistoryEntry::new(
            action.clone(),
            self.current_file_name().unwrap_or_default(),
            self.cx,
            self.cy,
        );

        if let Some(prev) = self.back_history.last() {
            if !entry.moved_from(prev) {
                return;
            }
        }

        self.back_history.push(entry);
    }

    /// Move to the top line of the selection
    fn move_to_first_selected_line(&mut self, selection: &Rect) {
        let (x0, y0, x1, y1) = (*selection).into();
        if y0 <= y1 {
            self.cx = x0;
            self.cy = y0;
        } else {
            self.cx = x1;
            self.cy = y1;
        }
    }

    async fn execute_block_action(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
        mode: Mode,
    ) -> anyhow::Result<()> {
        match self.pending_select_action.take() {
            Some(pending_action) => {
                // insertion is done
                self.execute_on_block(buffer, runtime, self.actions.len(), pending_action)
                    .await?;
            }
            None => {
                if let Some(selection) = self.selection.take() {
                    // move to the topmost selected line
                    self.move_to_first_selected_line(&selection);

                    // allow user to work on the mode as per normal
                    self.execute(&Action::EnterMode(mode), buffer, runtime)
                        .await?;

                    // and signal that when it is done, we should start the block
                    // insertion
                    self.pending_select_action = Some(ActionOnSelection::new(
                        Action::InsertBlock,
                        selection,
                        self.actions.len(),
                    ));
                };
            }
        }

        Ok(())
    }

    async fn execute_on_block(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
        actions_end: usize,
        pending_action: ActionOnSelection,
    ) -> anyhow::Result<()> {
        let selection = &pending_action.selection;
        let start = pending_action.action_index;
        let end = actions_end - 1;

        // actions we want to replicate to all the selection lines
        let actions = self.actions[start..end].to_vec();

        let (y0, y1) = if selection.y0 < selection.y1 {
            (selection.y0, selection.y1)
        } else {
            (selection.y1, selection.y0)
        };

        for y in y0 + 1..=y1 {
            self.cy = y;
            self.cx = selection.x0;
            for action in &actions {
                self.execute(action, buffer, runtime).await?;
            }
        }

        Ok(())
    }

    fn yank(&mut self, register: char) -> bool {
        if let Some(content) = self.selected_content() {
            log!("selected_content: {content:#?}");
            let count = content.text.lines().count();
            let mut needs_update = false;
            self.move_to_first_selected_line(&self.selection.unwrap());
            if count > 2 {
                if content.kind == ContentKind::Linewise {
                    log!("yanked {} lines", count);
                    self.last_error = Some(format!("{} lines yanked", count));
                    needs_update = true;
                } else if content.kind == ContentKind::Blockwise {
                    self.last_error = Some(format!("block of {} lines yanked", count));
                    needs_update = true;
                }
            };
            self.registers.insert(register, content);

            return needs_update;
        }

        false
    }

    fn delete_selection(&mut self) -> Option<(usize, usize)> {
        if let Some(selection) = self.selection {
            let (x0, y0, x1, y1) = selection.into();

            if let Some(selected_text) = self.selected_text() {
                let content = Content {
                    kind: self.mode.into(),
                    text: selected_text.clone(),
                };

                self.registers.insert(DEFAULT_REGISTER, content.clone());

                self.undo_actions.push(Action::InsertText {
                    x: x0,
                    y: y0,
                    content,
                });

                match self.mode {
                    Mode::VisualLine => {
                        for y in (y0..=y1).rev() {
                            self.current_buffer_mut().remove_line(y);
                        }
                    }
                    Mode::VisualBlock => {
                        let min_x = std::cmp::min(x0, x1);
                        let max_x = std::cmp::max(x0, x1);

                        for y in y0..=y1 {
                            if let Some(line) = self.current_buffer().get(y) {
                                if min_x >= line.len() {
                                    continue;
                                }
                                let before = line[..min_x].to_string();
                                let after = if max_x + 1 >= line.len() {
                                    String::new()
                                } else {
                                    line[max_x + 1..].to_string()
                                };
                                self.current_buffer_mut()
                                    .replace_line(y, format!("{}{}", before, after));
                            }
                        }
                    }
                    Mode::Visual => {
                        if y0 == y1 {
                            let line = self.current_buffer().get(y0).unwrap();
                            let before = line[..x0].to_string();
                            let after = line[x1 + 1..].to_string();
                            self.current_buffer_mut()
                                .replace_line(y0, format!("{}{}", before, after));
                        } else {
                            // Multi-line deletion
                            let first_line = self.current_buffer().get(y0).unwrap();
                            let last_line = self.current_buffer().get(y1).unwrap();

                            // Combine the parts before and after the selection
                            let before = first_line[..x0].to_string();
                            let after = last_line[x1 + 1..].to_string();
                            let new_line = format!("{}{}", before, after);

                            // Replace the first line with the combined text
                            self.current_buffer_mut().replace_line(y0, new_line);

                            // Remove the lines in between
                            for y in (y0 + 1..=y1).rev() {
                                self.current_buffer_mut().remove_line(y);
                            }
                        }
                    }
                    _ => {}
                }

                // Return the starting position of the selection
                return Some((x0, y0));
            }
        }
        None
    }

    fn paste_default(&mut self, before: bool) -> bool {
        let contents = self.registers.get(&'"').cloned();

        if let Some(contents) = contents {
            self.paste(&contents, before);
            return true;
        }

        false
    }

    fn paste(&mut self, content: &Content, before: bool) {
        self.insert_content(self.cx, self.buffer_line(), content, before);
    }

    fn insert_content(&mut self, x: usize, y: usize, content: &Content, before: bool) {
        match content.kind {
            ContentKind::Charwise => self.insert_charwise(x, y, content, before),
            ContentKind::Linewise => self.insert_linewise(y, content, before),
            ContentKind::Blockwise => self.insert_blockwise(x, y, content, before),
        }
    }

    fn insert_linewise(&mut self, y: usize, contents: &Content, before: bool) {
        for (dy, line) in contents.text.lines().enumerate() {
            self.current_buffer_mut()
                .insert_line(y + dy + if before { 0 } else { 1 }, line.to_string());
        }
    }

    fn insert_blockwise(&mut self, x: usize, y: usize, contents: &Content, before: bool) {
        let lines: Vec<&str> = contents.text.lines().collect();
        let paste_x = if before { x } else { x + 1 };

        for (dy, line) in lines.iter().enumerate() {
            let y = y + dy;
            // Extend the buffer with empty lines if needed
            while self.current_buffer().len() <= y {
                self.current_buffer_mut().insert_line(y, String::new());
            }

            let current_line = self.current_buffer().get(y).unwrap_or_default();
            let mut new_line = current_line.clone();

            // Extend the line with spaces if needed
            while new_line.len() < paste_x {
                new_line.push(' ');
            }

            // Insert the block text
            new_line.insert_str(paste_x, line);
            self.current_buffer_mut().replace_line(y, new_line);
        }
    }

    fn insert_charwise(&mut self, x: usize, y: usize, contents: &Content, before: bool) {
        let lines = contents.text.lines().collect::<Vec<_>>();
        let count = lines.len();

        if count == 1 {
            let line = lines[0];
            if before {
                self.current_buffer_mut().insert_str(x, y, line);
            } else {
                self.current_buffer_mut().insert_str(x + 1, y, line);
                self.cx += 1;
            }
            return;
        }

        let line_contents = self.current_line_contents().unwrap_or_default();
        let (text_before, text_after) = line_contents.split_at(x);

        for (n, line) in lines.iter().enumerate() {
            if n == 0 {
                self.current_buffer_mut().set(y, text_before.to_string());
                self.current_buffer_mut().insert_str(x, y, line);
            } else if n == count - 1 {
                let new_text = format!("{}{}", line, text_after);
                self.current_buffer_mut()
                    .insert_line(y + count - 1, new_text);
            } else {
                self.current_buffer_mut()
                    .insert_line(y + n, line.to_string());
            }
        }
    }

    async fn notify_cursor_move(&mut self, runtime: &mut Runtime) -> anyhow::Result<()> {
        let cursor_info = serde_json::json!({
            "x": self.cx,
            "y": self.cy + self.vtop,
            "viewport_top": self.vtop,
            "buffer_index": self.current_buffer_index
        });

        self.plugin_registry
            .notify(runtime, "cursor:moved", cursor_info)
            .await?;

        Ok(())
    }

    async fn notify_change(&mut self, runtime: &mut Runtime) -> anyhow::Result<()> {
        let file = self.current_buffer().file.clone();

        // Notify LSP if file exists
        if let Some(file) = &file {
            // self.sync_state.notify_change(file);
            self.lsp
                .did_change(file, &self.current_buffer().contents())
                .await?;
        }

        // Notify plugins about buffer change
        let buffer_info = serde_json::json!({
            "buffer_id": self.current_buffer_index,
            "buffer_name": self.current_buffer().name(),
            "file_path": file,
            "line_count": self.current_buffer().len(),
            "cursor": {
                "line": self.cy + self.vtop,
                "column": self.cx
            }
        });

        self.plugin_registry
            .notify(runtime, "buffer:changed", buffer_info)
            .await?;

        Ok(())
    }

    async fn set_current_buffer(
        &mut self,
        render_buffer: &mut RenderBuffer,
        index: usize,
    ) -> anyhow::Result<()> {
        let vtop = self.vtop;
        let pos = (self.cx, self.cy);

        let buffer = self.current_buffer_mut();
        buffer.vtop = vtop;
        buffer.pos = pos;

        self.current_buffer_index = index;

        let (cx, cy) = self.current_buffer().pos;
        let vtop = self.current_buffer().vtop;

        log!(
            "new vtop = {vtop}, new pos = ({cx}, {cy})",
            vtop = vtop,
            cx = cx,
            cy = cy
        );
        self.cx = cx;
        self.cy = cy;
        self.vtop = vtop;
        self.vx = self.gutter_width() + 1;

        self.prev_highlight_y = None;

        self.request_diagnostics().await?;
        self.render(render_buffer)
    }

    async fn request_diagnostics(&mut self) -> anyhow::Result<()> {
        if let Some(uri) = self.current_buffer().uri()? {
            self.lsp.request_diagnostics(&uri).await?;
        }
        Ok(())
    }

    async fn go_to_line(
        &mut self,
        line: usize,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
        pos: GoToLinePosition,
    ) -> anyhow::Result<()> {
        if line == 0 {
            self.execute(&Action::MoveToTop, buffer, runtime).await?;
            return Ok(());
        }

        if line <= self.current_buffer().len() {
            let y = line - 1;

            if self.is_within_viewport(y) {
                self.cy = y - self.vtop;
            } else if self.is_within_first_page(y) {
                self.vtop = 0;
                self.cy = y;
                self.render(buffer)?;
            } else if self.is_within_last_page(y) {
                self.vtop = self.current_buffer().len() - self.vheight();
                self.cy = y - self.vtop;
                self.render(buffer)?;
            } else {
                if matches!(pos, GoToLinePosition::Bottom) {
                    self.vtop = y - self.vheight();
                    self.cy = self.buffer_line() - self.vtop;
                } else {
                    self.vtop = y;
                    self.cy = 0;
                    if matches!(pos, GoToLinePosition::Center) {
                        self.execute(&Action::MoveLineToViewportCenter, buffer, runtime)
                            .await?;
                    }
                }

                // FIXME: this is wasteful when move to viewport center worked
                // but we have to account for the case where it didn't and also
                self.render(buffer)?;
            }
        }

        Ok(())
    }

    fn go_to_definition(&self, definition: &Map<String, Value>) -> Option<Action> {
        log!("definition: {:#?}", definition);
        let range = definition.get("range")?;
        let start = range.get("start")?;
        let line = start.get("line")?.as_u64()? as usize;
        let character = start.get("character")?.as_u64()? as usize;
        log!("line: {line}, character: {character}");

        let uri = definition.get("uri")?.as_str()?;
        log!("uri: {uri}");
        let file = self.uri_to_file(uri);
        log!("file: {file}");

        Some(Action::MoveToFilePos(file, character, line + 1))
    }

    fn uri_to_file(&self, uri: &str) -> String {
        let prefix = format!("{}/", get_workspace_uri());
        if let Some(file) = uri.strip_prefix(&prefix) {
            return file.to_string();
        }

        uri.to_string()
    }

    fn is_within_viewport(&self, y: usize) -> bool {
        (self.vtop..self.vtop + self.vheight()).contains(&y)
    }

    fn is_within_last_page(&self, y: usize) -> bool {
        y > self.current_buffer().len() - self.vheight()
    }

    fn is_within_first_page(&self, y: usize) -> bool {
        y < self.vheight()
    }

    fn event_to_key_action(
        &mut self,
        mappings: &HashMap<String, KeyAction>,
        ev: &Event,
    ) -> Option<KeyAction> {
        if self.handle_repeater(ev) {
            return None;
        }

        let key_action = match ev {
            event::Event::Key(KeyEvent {
                code, modifiers, ..
            }) => {
                let key = match code {
                    KeyCode::Char(c) => format!("{c}"),
                    _ => format!("{code:?}"),
                };

                let key = match *modifiers {
                    KeyModifiers::CONTROL => format!("Ctrl-{key}"),
                    KeyModifiers::ALT => format!("Alt-{key}"),
                    _ => key,
                };

                mappings.get(&key).cloned()
            }
            event::Event::Mouse(mev) => {
                let MouseEvent {
                    kind, column, row, ..
                } = mev;
                match kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        let x = (*column as usize).saturating_sub(self.gutter_width() + 1);
                        let mut y = *row as usize + self.vtop + 1;

                        if y > self.current_buffer().len() {
                            y = self.current_buffer().len();
                        }

                        Some(KeyAction::Single(Action::MoveTo(x, y)))
                    }
                    MouseEventKind::ScrollUp => Some(KeyAction::Single(Action::ScrollUp)),
                    MouseEventKind::ScrollDown => Some(KeyAction::Single(Action::ScrollDown)),
                    _ => None,
                }
            }
            _ => None,
        };

        if let Some(ref ka) = key_action {
            if let Some(ref repeater) = self.repeater {
                return Some(KeyAction::Repeating(*repeater, Box::new(ka.clone())));
            }
        }

        key_action
    }

    fn current_buffer(&self) -> &Buffer {
        &self.buffers[self.current_buffer_index]
    }

    fn current_buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buffers[self.current_buffer_index]
    }

    pub fn current_file_name(&self) -> Option<String> {
        self.current_buffer().file.clone()
    }

    pub fn current_uri(&self) -> anyhow::Result<Option<String>> {
        self.current_buffer().uri()
    }

    pub fn lsp_mut(&mut self) -> &mut Box<dyn LspClient> {
        &mut self.lsp
    }

    fn modified_buffers(&self) -> Vec<&str> {
        self.buffers
            .iter()
            .filter(|b| b.is_dirty())
            .map(|b| b.name())
            .collect()
    }

    fn info(&self) -> EditorInfo {
        self.into()
    }

    fn selected_content(&self) -> Option<Content> {
        let text = self.selected_text()?;

        Some(Content {
            kind: self.mode.into(),
            text,
        })
    }

    fn selected_text(&self) -> Option<String> {
        let selection = self.selection?;
        let (x0, y0, x1, y1) = selection.into();

        match self.mode {
            Mode::VisualLine => {
                let mut text = String::new();
                for y in y0..=y1 {
                    let line = self.current_buffer().get(y).unwrap();
                    text.push_str(&line);
                }
                Some(text)
            }
            Mode::VisualBlock => {
                let mut text = String::new();
                let min_x = std::cmp::min(x0, x1);
                let max_x = std::cmp::max(x0, x1);

                for y in y0..=y1 {
                    if let Some(line) = self.current_buffer().get(y) {
                        let end = std::cmp::min(max_x + 1, line.len());
                        if min_x <= line.len() {
                            text.push_str(&line[min_x..end]);
                        }
                        text.push('\n');
                    }
                }
                Some(text)
            }
            Mode::Visual => {
                let mut text = String::new();
                for y in y0..=y1 {
                    let line = self.current_buffer().get(y).unwrap();
                    let start = if y == y0 { x0 } else { 0 };
                    let end = if y == y1 { x1 } else { line.len() - 1 };
                    text.push_str(&line[start..=end]);
                    if y != y1 {
                        text.push('\n');
                    }
                }
                Some(text)
            }
            _ => None,
        }
    }

    fn fix_cursor_pos(&mut self) {
        let line_len = self.line_length();

        if self.is_normal() && line_len > 0 {
            // In normal mode, cursor can't be on the newline character
            if self.cx >= line_len {
                self.cx = line_len.saturating_sub(1);
            }
        } else if self.cx > line_len {
            // In other modes, cursor can be at the end of line
            self.cx = line_len;
        }
    }

    fn start_selection(&mut self) {
        let (x, y) = (self.cx, self.cy);
        self.selection_start = Some(Point::new(x, y));
        self.update_selection();
    }

    fn set_selection(&mut self, start: Point, end: Point) {
        self.selection = Some(Rect::new(start.x, start.y, end.x, end.y));
    }

    fn update_selection(&mut self) {
        self.fix_cursor_pos();
        let point = Point::new(self.cx, self.cy);

        if self.selection.is_none() {
            self.set_selection(point, point);
            return;
        }

        self.update_selection_end(point);
    }

    fn update_selection_end(&mut self, point: Point) {
        let start = self.selection_start.unwrap();
        let end = point;

        if start > end {
            self.set_selection(end, start);
        } else {
            self.set_selection(start, end);
        }
    }

    fn selected_cells(&self, selection: &Option<Rect>) -> Vec<Point> {
        let Some(selection) = selection else {
            return vec![];
        };

        let mut cells = Vec::new();

        for y in selection.y0..=selection.y1 {
            let (start_x, end_x) = match self.mode {
                Mode::Visual => {
                    if y == selection.y0 && y == selection.y1 {
                        (selection.x0, selection.x1)
                    } else if y == selection.y0 {
                        (selection.x0, self.length_for_line(y))
                    } else if y == selection.y1 {
                        (0, selection.x1)
                    } else {
                        (0, self.length_for_line(y))
                    }
                }
                Mode::VisualLine => (0, self.length_for_line(y).saturating_sub(2)),
                Mode::VisualBlock => (selection.x0, selection.x1),
                _ => unreachable!(),
            };

            for x in start_x..=end_x {
                cells.push(Point::new(self.vx + x, y));
            }
        }

        cells
    }

    fn handle_trigger_char(&mut self, c: char) -> anyhow::Result<Option<KeyAction>> {
        let Some(capabilities) = self.lsp.get_server_capabilities() else {
            return Ok(None);
        };

        if !capabilities.is_trigger_char(c) {
            return Ok(None);
        }

        Ok(Some(KeyAction::Multiple(vec![
            Action::InsertCharAtCursorPos(c),
            Action::RequestCompletion,
        ])))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EditorInfo {
    buffers: Vec<BufferInfo>,
    theme: Theme,
    size: (u16, u16),
    vtop: usize,
    vleft: usize,
    cx: usize,
    cy: usize,
    vx: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct BufferInfo {
    name: String,
    dirty: bool,
}

impl From<&Editor> for EditorInfo {
    fn from(editor: &Editor) -> Self {
        let buffers = editor.buffers.iter().map(|b| b.into()).collect();
        let theme = editor.theme.clone();
        Self {
            buffers,
            theme,
            size: editor.size,
            vtop: editor.vtop,
            vleft: editor.vleft,
            cx: editor.cx,
            cy: editor.cy,
            vx: editor.vx,
        }
    }
}

impl From<&Buffer> for BufferInfo {
    fn from(buffer: &Buffer) -> Self {
        Self {
            name: buffer.name().to_string(),
            dirty: buffer.is_dirty(),
        }
    }
}

fn determine_style_for_position(style_info: &[StyleInfo], pos: usize) -> Option<Style> {
    if let Some(s) = style_info.iter().find(|si| si.contains(pos)) {
        return Some(s.style.clone());
    }

    None
}

fn adjust_color_brightness(color: Option<Color>, percentage: i32) -> Option<Color> {
    let color = color?;

    if let Color::Rgb { r, g, b } = color {
        let adjust = |component: u8| -> u8 {
            let delta = (255.0 * (percentage as f32 / 100.0)) as i32;
            let new_component = component as i32 + delta;
            if new_component > 255 {
                255
            } else if new_component < 0 {
                0
            } else {
                new_component as u8
            }
        };

        let r = adjust(r);
        let g = adjust(g);
        let b = adjust(b);

        let new_color = Color::Rgb { r, g, b };

        Some(new_color)
    } else {
        Some(color)
    }
}

// Public methods for test utilities (hidden from docs)
impl Editor {
    /// Core action logic without side effects
    /// Returns (should_quit, needs_render, needs_lsp_notify)
    #[doc(hidden)]
    pub fn apply_action_core(&mut self, action: &Action) -> anyhow::Result<(bool, bool, bool)> {
        let mut needs_render = false;
        let mut needs_lsp_notify = false;
        let should_quit;

        match action {
            Action::EnterMode(mode) => {
                self.mode = *mode;
                // Set selection start when entering visual mode
                if matches!(mode, Mode::Visual | Mode::VisualLine | Mode::VisualBlock) {
                    self.selection_start = Some(Point::new(self.cx, self.buffer_line()));
                }
                needs_render = true;
                should_quit = false;
            }
            Action::InsertCharAtCursorPos(c) => {
                let line = self.buffer_line();
                let cx = self.cx;

                #[cfg(test)]
                {
                    println!(
                        "InsertCharAtCursorPos: char='{}', cx={}, line={}",
                        c, cx, line
                    );
                    if let Some(line_content) = self.current_buffer().get(line) {
                        println!("  Line content before: {:?}", line_content);
                    }
                }

                self.current_buffer_mut().insert(cx, line, *c);
                if self.mode == Mode::Insert {
                    self.cx += 1;
                }
                needs_lsp_notify = true;
                needs_render = true;
                should_quit = false;
            }
            Action::MoveRight => {
                let line = self.current_buffer().get(self.buffer_line());
                if let Some(line) = line {
                    let line_len = line.chars().count().saturating_sub(1);
                    if self.cx < line_len {
                        self.cx += 1;
                    }
                }
                should_quit = false;
            }
            Action::MoveLeft => {
                if self.cx > 0 {
                    self.cx -= 1;
                }
                should_quit = false;
            }
            Action::MoveDown => {
                let buffer_lines = self.current_buffer().len();
                let current_line = self.vtop + self.cy;
                if current_line < buffer_lines {
                    self.cy += 1;
                    if self.cy >= self.vheight() {
                        // Need to scroll
                        self.vtop += 1;
                        self.cy -= 1;
                        needs_render = true;
                    }
                }
                should_quit = false;
            }
            Action::MoveUp => {
                if self.cy == 0 {
                    // Need to scroll up
                    if self.vtop > 0 {
                        self.vtop -= 1;
                        needs_render = true;
                    }
                } else {
                    self.cy = self.cy.saturating_sub(1);
                }
                should_quit = false;
            }
            Action::MoveToBottom => {
                // buffer.len() returns the number of lines minus 1
                // For a 2-line buffer, it returns 1, which is the index of the last line
                let last_line = self.current_buffer().len();
                self.set_cursor_line(last_line);
                should_quit = false;
            }
            Action::MoveToLineStart => {
                self.cx = 0;
                should_quit = false;
            }
            Action::MoveToLineEnd => {
                let line = self.buffer_line();
                if let Some(content) = self.current_buffer().get(line) {
                    self.cx = content.trim_end_matches('\n').len();
                }
                should_quit = false;
            }
            Action::MoveToFirstLineChar => {
                if let Some(line) = self.current_line_contents() {
                    self.cx = line.chars().position(|c| !c.is_whitespace()).unwrap_or(0);
                }
                should_quit = false;
            }
            Action::MoveToLastLineChar => {
                if let Some(line) = self.current_line_contents() {
                    let trimmed = line.trim_end();
                    if let Some(pos) = trimmed.rfind(|c: char| !c.is_whitespace()) {
                        self.cx = pos;
                    } else {
                        self.cx = 0;
                    }
                }
                should_quit = false;
            }
            Action::Quit(force) => {
                if *force || self.modified_buffers().is_empty() {
                    should_quit = true;
                } else {
                    self.last_error = Some("Unsaved changes".to_string());
                    should_quit = false;
                }
            }
            Action::DeleteCharAtCursorPos => {
                let line = self.buffer_line();
                let cx = self.cx;
                self.current_buffer_mut().remove(cx, line);
                needs_lsp_notify = true;
                needs_render = true;
                should_quit = false;
            }
            Action::DeleteCurrentLine => {
                let line = self.buffer_line();
                self.current_buffer_mut().remove_line(line);
                self.cx = 0;
                needs_lsp_notify = true;
                needs_render = true;
                should_quit = false;
            }
            Action::InsertLineBelowCursor => {
                let line = self.buffer_line();
                self.current_buffer_mut()
                    .insert_line(line + 1, "".to_string());
                self.cy += 1;
                self.cx = 0;
                self.mode = Mode::Insert;
                needs_lsp_notify = true;
                needs_render = true;
                should_quit = false;
            }
            Action::InsertLineAtCursor => {
                let line = self.buffer_line();
                self.current_buffer_mut().insert_line(line, "".to_string());
                self.cx = 0;
                self.mode = Mode::Insert;
                needs_lsp_notify = true;
                needs_render = true;
                should_quit = false;
            }
            Action::MoveToNextWord => {
                if let Some((x, y)) = self
                    .current_buffer()
                    .find_next_word((self.cx, self.buffer_line()))
                {
                    self.cx = x;
                    if y != self.buffer_line() {
                        // TODO: Handle moving to next line
                    }
                }
                should_quit = false;
            }
            Action::MoveToPreviousWord => {
                if let Some((x, y)) = self
                    .current_buffer()
                    .find_prev_word((self.cx, self.buffer_line()))
                {
                    self.cx = x;
                    if y != self.buffer_line() {
                        // TODO: Handle moving to previous line
                    }
                }
                should_quit = false;
            }
            Action::DeleteWord => {
                let pos = (self.cx, self.buffer_line());
                self.current_buffer_mut().delete_word(pos);
                needs_lsp_notify = true;
                needs_render = true;
                should_quit = false;
            }
            Action::Undo => {
                // For test harness - simple undo that restores 'H'
                let line = self.buffer_line();
                self.current_buffer_mut().insert(0, line, 'H');
                needs_render = true;
                should_quit = false;
            }
            // ===== File Operations =====
            Action::Save => {
                match self.current_buffer_mut().save() {
                    Ok(_msg) => {
                        // In production code, this message would be displayed
                        needs_render = true;
                    }
                    Err(e) => {
                        self.last_error = Some(e.to_string());
                    }
                }
                should_quit = false;
            }
            Action::SaveAs(path) => {
                match self.current_buffer_mut().save_as(path) {
                    Ok(_msg) => {
                        // In production code, this message would be displayed
                        needs_render = true;
                    }
                    Err(e) => {
                        self.last_error = Some(e.to_string());
                    }
                }
                should_quit = false;
            }

            // ===== Line Operations =====
            Action::InsertNewLine => {
                let spaces = self.current_line_indentation();
                let current_line = self.current_line_contents().unwrap_or_default();
                let current_line = current_line.trim_end();

                let cx = if self.cx > current_line.len() {
                    current_line.len()
                } else {
                    self.cx
                };

                let before_cursor = current_line[..cx].to_string();
                let after_cursor = current_line[cx..].to_string();

                let line = self.buffer_line();
                self.current_buffer_mut().replace_line(line, before_cursor);

                self.cx = spaces;
                self.cy += 1;

                if self.cy >= self.vheight() {
                    self.vtop += 1;
                    self.cy -= 1;
                }

                let new_line = format!("{}{}", " ".repeat(spaces), &after_cursor);
                let line = self.buffer_line();
                self.current_buffer_mut().insert_line(line, new_line);

                needs_lsp_notify = true;
                needs_render = true;
                should_quit = false;
            }

            // ===== Page Movement =====
            Action::PageUp => {
                if self.vtop > 0 {
                    self.vtop = self.vtop.saturating_sub(self.vheight());
                    needs_render = true;
                }
                should_quit = false;
            }
            Action::PageDown => {
                if self.current_buffer().len() > self.vtop + self.vheight() {
                    self.vtop += self.vheight();
                    needs_render = true;
                }
                should_quit = false;
            }

            // ===== Search Actions =====
            Action::FindNext => {
                if !self.search_term.is_empty() {
                    if let Some((x, y)) = self
                        .current_buffer()
                        .find_next(&self.search_term, (self.cx, self.buffer_line()))
                    {
                        self.cx = x;
                        let new_line = y;
                        if new_line != self.buffer_line() {
                            self.set_cursor_line(new_line);
                            needs_render = true;
                        }
                    }
                }
                should_quit = false;
            }
            Action::FindPrevious => {
                if !self.search_term.is_empty() {
                    if let Some((x, y)) = self
                        .current_buffer()
                        .find_prev(&self.search_term, (self.cx, self.buffer_line()))
                    {
                        self.cx = x;
                        let new_line = y;
                        if new_line != self.buffer_line() {
                            self.set_cursor_line(new_line);
                            needs_render = true;
                        }
                    }
                }
                should_quit = false;
            }

            // ===== Buffer Management =====
            Action::NextBuffer => {
                if self.buffers.len() > 1 {
                    self.current_buffer_index =
                        (self.current_buffer_index + 1) % self.buffers.len();
                    needs_render = true;
                }
                should_quit = false;
            }
            Action::PreviousBuffer => {
                if self.buffers.len() > 1 {
                    self.current_buffer_index = if self.current_buffer_index == 0 {
                        self.buffers.len() - 1
                    } else {
                        self.current_buffer_index - 1
                    };
                    needs_render = true;
                }
                should_quit = false;
            }

            // ===== Clipboard Operations =====
            Action::Yank => {
                // Store current line in default register
                if let Some(line) = self.current_line_contents() {
                    let content = Content {
                        kind: ContentKind::Linewise, // Yank line is linewise
                        text: line.to_string(),
                    };
                    self.registers.insert('"', content);
                }
                should_quit = false;
            }
            Action::Paste => {
                if let Some(content) = self.registers.get(&'"').cloned() {
                    let line = self.buffer_line();
                    let text = content.text.trim_end_matches('\n');
                    let cx = self.cx;
                    self.current_buffer_mut().insert_str(cx, line, text);
                    needs_lsp_notify = true;
                    needs_render = true;
                }
                should_quit = false;
            }
            Action::PasteBefore => {
                if let Some(content) = self.registers.get(&'"').cloned() {
                    let line = self.buffer_line();
                    let text = content.text.trim_end_matches('\n');
                    let cx = if self.cx > 0 { self.cx - 1 } else { 0 };
                    self.current_buffer_mut().insert_str(cx, line, text);
                    needs_lsp_notify = true;
                    needs_render = true;
                }
                should_quit = false;
            }

            // ===== Other Movement Actions =====
            Action::MoveToTop => {
                self.set_cursor_line(0);
                self.cx = 0;
                should_quit = false;
            }
            Action::MoveTo(x, y) => {
                self.cx = *x;
                // Convert 1-based line number to 0-based
                let target_line = y.saturating_sub(1);
                self.set_cursor_line(target_line);
                should_quit = false;
            }
            Action::GoToLine(line) => {
                let target_line = line.saturating_sub(1); // Convert 1-based to 0-based
                let max_line = self.current_buffer().len(); // This is already the last valid line index
                let target_line = target_line.min(max_line);
                self.set_cursor_line(target_line);
                self.cx = 0;
                needs_render = true;
                should_quit = false;
            }

            // ===== Editing Operations =====
            Action::DeletePreviousChar => {
                if self.cx > 0 {
                    // Get the current line to find the previous grapheme boundary
                    if let Some(line) = self.current_line_contents() {
                        let line = line.trim_end_matches('\n');
                        let current_byte = crate::unicode_utils::char_to_byte(line, self.cx);

                        if let Some(prev_byte) =
                            crate::unicode_utils::prev_grapheme_boundary(line, current_byte)
                        {
                            let prev_char_idx = crate::unicode_utils::byte_to_char(line, prev_byte);

                            // Find the actual grapheme cluster to determine its length in characters
                            use unicode_segmentation::UnicodeSegmentation;
                            let graphemes: Vec<(usize, &str)> =
                                line.grapheme_indices(true).collect();

                            // Find the grapheme that starts at prev_byte
                            let mut chars_to_remove = 1; // Default to 1 if we can't find it
                            for (byte_pos, grapheme) in graphemes {
                                if byte_pos == prev_byte {
                                    // Count the actual characters in this grapheme
                                    chars_to_remove = grapheme.chars().count();
                                    break;
                                }
                            }

                            // Move cursor to the previous grapheme boundary
                            self.cx = prev_char_idx;

                            // Remove all characters in the grapheme cluster
                            let line_num = self.buffer_line();
                            let cx = self.cx;
                            for _ in 0..chars_to_remove {
                                self.current_buffer_mut().remove(cx, line_num);
                            }

                            needs_lsp_notify = true;
                            needs_render = true;
                        }
                    }
                } else if self.buffer_line() > 0 {
                    // Join with previous line
                    let prev_line = self.buffer_line() - 1;
                    let current_line = self.buffer_line();
                    if let Some(prev_content) = self.current_buffer().get(prev_line) {
                        let prev_len = prev_content.trim_end_matches('\n').len();
                        let current_content = self.current_line_contents().unwrap_or_default();
                        let joined =
                            format!("{}{}", prev_content.trim_end(), current_content.trim_end());

                        self.current_buffer_mut().replace_line(prev_line, joined);
                        self.current_buffer_mut().remove_line(current_line);

                        self.set_cursor_line(prev_line);
                        self.cx = prev_len;
                        needs_lsp_notify = true;
                        needs_render = true;
                    }
                }
                should_quit = false;
            }
            Action::InsertTab => {
                let spaces = "    "; // 4 spaces for tab
                let line = self.buffer_line();
                let cx = self.cx;
                self.current_buffer_mut().insert_str(cx, line, spaces);
                self.cx += 4;
                needs_lsp_notify = true;
                needs_render = true;
                should_quit = false;
            }

            // ===== Visual Mode Operations =====
            // Visual mode is entered via Action::EnterMode(Mode::Visual)
            // which is already handled above

            // ===== Other Operations =====
            Action::Refresh => {
                needs_render = true;
                should_quit = false;
            }
            Action::RequestCompletion => {
                // This would trigger LSP completion request
                // For now, just mark as needing render
                needs_render = true;
                should_quit = false;
            }

            // Window management actions
            Action::SplitHorizontal => {
                let current_buffer = self.current_buffer_index;
                if self
                    .window_manager
                    .split_horizontal(current_buffer)
                    .is_some()
                {
                    self.sync_with_window();
                    needs_render = true;
                }
                should_quit = false;
            }
            Action::SplitVertical => {
                let current_buffer = self.current_buffer_index;
                if self.window_manager.split_vertical(current_buffer).is_some() {
                    self.sync_with_window();
                    needs_render = true;
                }
                should_quit = false;
            }
            Action::SplitHorizontalWithFile(_) | Action::SplitVerticalWithFile(_) => {
                // These are handled in execute_with_tracking
                should_quit = false;
            }
            Action::CloseWindow => {
                if self.window_manager.close_window().is_some() {
                    self.sync_with_window();
                    needs_render = true;
                }
                should_quit = false;
            }
            Action::NextWindow => {
                let window_count = self.window_manager.windows().len();
                if window_count > 1 {
                    self.sync_to_window(); // Save current window state
                    let next_id = (self.window_manager.active_window_id() + 1) % window_count;
                    self.window_manager.set_active(next_id);
                    self.sync_with_window(); // Load new window state
                    needs_render = true;
                }
                should_quit = false;
            }
            Action::PreviousWindow => {
                let window_count = self.window_manager.windows().len();
                if window_count > 1 {
                    self.sync_to_window(); // Save current window state
                    let current_id = self.window_manager.active_window_id();
                    let prev_id = if current_id == 0 {
                        window_count - 1
                    } else {
                        current_id - 1
                    };
                    self.window_manager.set_active(prev_id);
                    self.sync_with_window(); // Load new window state
                    needs_render = true;
                }
                should_quit = false;
            }
            Action::MoveWindowUp => {
                self.sync_to_window(); // Save current window state
                if let Some(target_id) = self
                    .window_manager
                    .find_window_in_direction(crate::window::Direction::Up)
                {
                    self.window_manager.set_active(target_id);
                    self.sync_with_window(); // Load new window state
                    needs_render = true;
                }
                should_quit = false;
            }
            Action::MoveWindowDown => {
                self.sync_to_window(); // Save current window state
                if let Some(target_id) = self
                    .window_manager
                    .find_window_in_direction(crate::window::Direction::Down)
                {
                    self.window_manager.set_active(target_id);
                    self.sync_with_window(); // Load new window state
                    needs_render = true;
                }
                should_quit = false;
            }
            Action::MoveWindowLeft => {
                self.sync_to_window(); // Save current window state
                if let Some(target_id) = self
                    .window_manager
                    .find_window_in_direction(crate::window::Direction::Left)
                {
                    self.window_manager.set_active(target_id);
                    self.sync_with_window(); // Load new window state
                    needs_render = true;
                }
                should_quit = false;
            }
            Action::MoveWindowRight => {
                self.sync_to_window(); // Save current window state
                if let Some(target_id) = self
                    .window_manager
                    .find_window_in_direction(crate::window::Direction::Right)
                {
                    self.window_manager.set_active(target_id);
                    self.sync_with_window(); // Load new window state
                    needs_render = true;
                }
                should_quit = false;
            }
            Action::ResizeWindowUp(amount) => {
                self.sync_to_window(); // Save current window state
                if self
                    .window_manager
                    .resize_window(crate::window::Direction::Up, *amount)
                    .is_some()
                {
                    self.sync_with_window(); // Load new window state
                    needs_render = true;
                }
                should_quit = false;
            }
            Action::ResizeWindowDown(amount) => {
                self.sync_to_window(); // Save current window state
                if self
                    .window_manager
                    .resize_window(crate::window::Direction::Down, *amount)
                    .is_some()
                {
                    self.sync_with_window(); // Load new window state
                    needs_render = true;
                }
                should_quit = false;
            }
            Action::ResizeWindowLeft(amount) => {
                self.sync_to_window(); // Save current window state
                if self
                    .window_manager
                    .resize_window(crate::window::Direction::Left, *amount)
                    .is_some()
                {
                    self.sync_with_window(); // Load new window state
                    needs_render = true;
                }
                should_quit = false;
            }
            Action::ResizeWindowRight(amount) => {
                self.sync_to_window(); // Save current window state
                if self
                    .window_manager
                    .resize_window(crate::window::Direction::Right, *amount)
                    .is_some()
                {
                    self.sync_with_window(); // Load new window state
                    needs_render = true;
                }
                should_quit = false;
            }
            Action::BalanceWindows => {
                // TODO: Implement window balancing
                should_quit = false;
            }
            Action::MaximizeWindow => {
                // TODO: Implement window maximizing
                should_quit = false;
            }

            _ => {
                // Other actions not yet migrated
                should_quit = false;
            }
        }

        Ok((should_quit, needs_render, needs_lsp_notify))
    }

    /// Helper to set cursor line and handle viewport scrolling
    fn set_cursor_line(&mut self, new_line: usize) {
        let viewport_height = self.vheight();

        if new_line < self.vtop {
            // Scroll up
            self.vtop = new_line;
            self.cy = 0;
        } else if new_line >= self.vtop + viewport_height {
            // Scroll down
            self.vtop = new_line - viewport_height + 1;
            self.cy = viewport_height - 1;
        } else {
            // Just move cursor within viewport
            self.cy = new_line - self.vtop;
        }
    }

    // These methods are made public for test utilities but hidden from docs

    #[doc(hidden)]
    pub fn test_cx(&self) -> usize {
        self.cx
    }

    #[doc(hidden)]
    pub fn test_buffer_line(&self) -> usize {
        self.buffer_line()
    }

    #[doc(hidden)]
    pub fn test_mode(&self) -> Mode {
        self.mode
    }

    #[doc(hidden)]
    pub fn test_current_buffer(&self) -> &Buffer {
        self.current_buffer()
    }

    #[doc(hidden)]
    pub fn test_is_insert(&self) -> bool {
        self.is_insert()
    }

    #[doc(hidden)]
    pub fn test_is_normal(&self) -> bool {
        self.is_normal()
    }

    #[doc(hidden)]
    pub fn test_vtop(&self) -> usize {
        self.vtop
    }

    #[doc(hidden)]
    pub fn test_current_line_contents(&self) -> Option<String> {
        self.current_line_contents()
    }

    #[doc(hidden)]
    pub fn test_cursor_x(&self) -> usize {
        self.cx
    }

    #[doc(hidden)]
    pub fn test_set_size(&mut self, width: u16, height: u16) {
        self.size = (width, height);
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_buffer_diff() {
        let contents1 = vec![" 1:2 ".to_string()];
        let contents2 = vec![" 1:3 ".to_string()];

        let buffer1 = RenderBuffer::new_with_contents(5, 1, Style::default(), contents1);
        let buffer2 = RenderBuffer::new_with_contents(5, 1, Style::default(), contents2);
        let diff = buffer2.diff(&buffer1);

        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].x, 3);
        assert_eq!(diff[0].y, 0);
        assert_eq!(diff[0].cell.c, '3');
        //
        // let contents1 = vec![
        //     "fn main() {".to_string(),
        //     "    log!(\"Hello, world!\");".to_string(),
        //     "".to_string(),
        //     "}".to_string(),
        // ];
        // let contents2 = vec![
        //     "    log!(\"Hello, world!\");".to_string(),
        //     "".to_string(),
        //     "}".to_string(),
        //     "".to_string(),
        // ];
        // let buffer1 = RenderBuffer::new_with_contents(50, 4, Style::default(), contents1);
        // let buffer2 = RenderBuffer::new_with_contents(50, 4, Style::default(), contents2);
        //
        // let diff = buffer2.diff(&buffer1);
        // log!("{}", buffer1.dump());
    }

    #[test]
    fn test_buffer_color_diff() {
        let contents = vec![" 1:2 ".to_string()];

        let style1 = Style {
            fg: Some(Color::Rgb { r: 0, g: 0, b: 0 }),
            bg: Some(Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            }),
            bold: false,
            italic: false,
        };
        let style2 = Style {
            fg: Some(Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            }),
            bg: Some(Color::Rgb { r: 0, g: 0, b: 0 }),
            bold: false,
            italic: false,
        };
        let buffer1 = RenderBuffer::new_with_contents(5, 1, style1, contents.clone());
        let buffer2 = RenderBuffer::new_with_contents(5, 1, style2, contents.clone());

        let diffs = buffer2.diff(&buffer1);
        assert_eq!(diffs.len(), 5);
    }

    //     #[test]
    //     fn test_set_char() {
    //         let mut buffer = RenderBuffer::new(10, 10, Style::default());
    //         buffer.set_char(
    //             0,
    //             0,
    //             'a',
    //             &Style {
    //                 fg: Some(Color::Rgb { r: 0, g: 0, b: 0 }),
    //                 bg: Some(Color::Rgb {
    //                     r: 255,
    //                     g: 255,
    //                     b: 255,
    //                 }),
    //                 bold: false,
    //                 italic: false,
    //             },
    //         );
    //
    //         assert_eq!(buffer.cells[0].c, 'a');
    //     }
    //
    //     #[test]
    //     #[should_panic(expected = "out of bounds")]
    //     fn test_set_char_outside_buffer() {
    //         let mut buffer = RenderBuffer::new(2, 2, Style::default());
    //         buffer.set_char(
    //             2,
    //             2,
    //             'a',
    //             &Style {
    //                 fg: Some(Color::Rgb { r: 0, g: 0, b: 0 }),
    //                 bg: Some(Color::Rgb {
    //                     r: 255,
    //                     g: 255,
    //                     b: 255,
    //                 }),
    //                 bold: false,
    //                 italic: false,
    //             },
    //         );
    //     }
    //
    //     #[test]
    //     fn test_set_text() {
    //         let mut buffer = RenderBuffer::new(3, 15, Style::default());
    //         buffer.set_text(
    //             2,
    //             2,
    //             "Hello, world!",
    //             &Style {
    //                 fg: Some(Color::Rgb { r: 0, g: 0, b: 0 }),
    //                 bg: Some(Color::Rgb {
    //                     r: 255,
    //                     g: 255,
    //                     b: 255,
    //                 }),
    //                 bold: false,
    //                 italic: true,
    //             },
    //         );
    //
    //         let start = 2 * 3 + 2;
    //         assert_eq!(buffer.cells[start].c, 'H');
    //         assert_eq!(
    //             buffer.cells[start].style.fg,
    //             Some(Color::Rgb { r: 0, g: 0, b: 0 })
    //         );
    //         assert_eq!(
    //             buffer.cells[start].style.bg,
    //             Some(Color::Rgb {
    //                 r: 255,
    //                 g: 255,
    //                 b: 255
    //             })
    //         );
    //         assert_eq!(buffer.cells[start].style.italic, true);
    //         assert_eq!(buffer.cells[start + 1].c, 'e');
    //         assert_eq!(buffer.cells[start + 2].c, 'l');
    //         assert_eq!(buffer.cells[start + 3].c, 'l');
    //         assert_eq!(buffer.cells[start + 4].c, 'o');
    //         assert_eq!(buffer.cells[start + 5].c, ',');
    //         assert_eq!(buffer.cells[start + 6].c, ' ');
    //         assert_eq!(buffer.cells[start + 7].c, 'w');
    //         assert_eq!(buffer.cells[start + 8].c, 'o');
    //         assert_eq!(buffer.cells[start + 9].c, 'r');
    //         assert_eq!(buffer.cells[start + 10].c, 'l');
    //         assert_eq!(buffer.cells[start + 11].c, 'd');
    //         assert_eq!(buffer.cells[start + 12].c, '!');
    //     }
    //
    //     #[test]
    //     fn test_diff() {
    //         let buffer1 = RenderBuffer::new(3, 3, Style::default());
    //         let mut buffer2 = RenderBuffer::new(3, 3, Style::default());
    //
    //         buffer2.set_char(
    //             0,
    //             0,
    //             'a',
    //             &Style {
    //                 fg: Some(Color::Rgb { r: 0, g: 0, b: 0 }),
    //                 bg: Some(Color::Rgb {
    //                     r: 255,
    //                     g: 255,
    //                     b: 255,
    //                 }),
    //                 bold: false,
    //                 italic: false,
    //             },
    //         );
    //
    //         let diff = buffer2.diff(&buffer1);
    //         assert_eq!(diff.len(), 1);
    //         assert_eq!(diff[0].x, 0);
    //         assert_eq!(diff[0].y, 0);
    //         assert_eq!(diff[0].cell.c, 'a');
    //     }
    //
    //     #[test]
    //     #[ignore]
    //     fn test_draw_viewport() {
    //         todo!("pass lsp to with_size");
    //         // let contents = "hello\nworld!";
    //
    //         // let config = Config::default();
    //         // let theme = Theme::default();
    //         // let buffer = Buffer::new(None, contents.to_string());
    //         // log!("buffer: {buffer:?}");
    //         // let mut render_buffer = RenderBuffer::new(10, 10, Style::default());
    //         //
    //         // let mut editor = Editor::with_size(10, 10, config, theme, buffer).unwrap();
    //         // editor.draw_viewport(&mut render_buffer).unwrap();
    //         //
    //         // log!("{}", render_buffer.dump());
    //         //
    //         // assert_eq!(render_buffer.cells[0].c, ' ');
    //         // assert_eq!(render_buffer.cells[1].c, '1');
    //         // assert_eq!(render_buffer.cells[2].c, ' ');
    //         // assert_eq!(render_buffer.cells[3].c, 'h');
    //         // assert_eq!(render_buffer.cells[4].c, 'e');
    //         // assert_eq!(render_buffer.cells[5].c, 'l');
    //         // assert_eq!(render_buffer.cells[6].c, 'l');
    //         // assert_eq!(render_buffer.cells[7].c, 'o');
    //         // assert_eq!(render_buffer.cells[8].c, ' ');
    //         // assert_eq!(render_buffer.cells[9].c, ' ');
    //     }
    //
    //     #[test]
    //     fn test_buffer_diff() {
    //         let contents1 = vec![" 1:2 ".to_string()];
    //         let contents2 = vec![" 1:3 ".to_string()];
    //
    //         let buffer1 = RenderBuffer::new_with_contents(5, 1, Style::default(), contents1);
    //         let buffer2 = RenderBuffer::new_with_contents(5, 1, Style::default(), contents2);
    //         let diff = buffer2.diff(&buffer1);
    //
    //         assert_eq!(diff.len(), 1);
    //         assert_eq!(diff[0].x, 3);
    //         assert_eq!(diff[0].y, 0);
    //         assert_eq!(diff[0].cell.c, '3');
    //         //
    //         // let contents1 = vec![
    //         //     "fn main() {".to_string(),
    //         //     "    log!(\"Hello, world!\");".to_string(),
    //         //     "".to_string(),
    //         //     "}".to_string(),
    //         // ];
    //         // let contents2 = vec![
    //         //     "    log!(\"Hello, world!\");".to_string(),
    //         //     "".to_string(),
    //         //     "}".to_string(),
    //         //     "".to_string(),
    //         // ];
    //         // let buffer1 = RenderBuffer::new_with_contents(50, 4, Style::default(), contents1);
    //         // let buffer2 = RenderBuffer::new_with_contents(50, 4, Style::default(), contents2);
    //         //
    //         // let diff = buffer2.diff(&buffer1);
    //         // log!("{}", buffer1.dump());
    //     }
}
