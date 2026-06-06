pub mod render_buffer;
pub mod rendering;

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet, VecDeque},
    io::{stdout, Write as _},
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::unicode_utils::{
    char_prefix, char_slice, char_suffix, char_to_grapheme, display_width, grapheme_len,
    grapheme_to_byte, grapheme_to_char, next_grapheme_boundary, prev_grapheme_boundary,
};

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
use unicode_segmentation::UnicodeSegmentation;

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
    theme::{parse_vscode_theme, Style, Theme},
    ui::{CompletionUI, Component, FilePicker, Info, Picker},
    undo::{CursorSnapshot, TextPosition, TextRange},
    utils::get_workspace_uri,
    window::{WindowManager, WindowManagerSnapshot},
};

pub static ACTION_DISPATCHER: Lazy<Dispatcher<PluginRequest, PluginResponse>> =
    Lazy::new(Dispatcher::new);

pub const DEFAULT_REGISTER: char = '"';
pub const ADD_TO_HISTORY_THRESHOLD: Duration = Duration::from_millis(100);

pub enum PluginRequest {
    Action(Action),
    EditorInfo(Option<i32>),
    OpenPicker(Option<String>, Option<i32>, Vec<Value>),
    OpenLivePicker(Option<String>, Option<i32>, Vec<Value>, Option<String>),
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
    GetEditorState {
        request_id: i32,
    },
    RestoreEditorState {
        request_id: i32,
        snapshot: EditorStateSnapshot,
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
    CreatePanel {
        id: String,
        config: plugin::PanelConfig,
    },
    UpdatePanel {
        id: String,
        rows: Vec<plugin::PanelRow>,
    },
    FocusPanel {
        id: String,
    },
    FocusEditor,
    ClosePanel {
        id: String,
    },
    ListDirectory {
        path: String,
        request_id: i32,
    },
    WatchDirectory {
        path: String,
        watch_id: i32,
    },
    UnwatchDirectory {
        watch_id: i32,
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

struct DirectoryWatcher {
    path: String,
    snapshot: Value,
    last_checked: Instant,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum Action {
    Quit(bool),
    Save,
    SaveAs(String),
    EnterMode(Mode),

    Undo,
    Redo,
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
    DeleteTextRange(TextRange),
    ChangeTextRange(TextRange),
    ChangeCurrentLine,
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
    DeleteBuffer(bool),
    FilePicker,
    ShowDialog,
    CloseDialog,
    ClearDiagnostics(String, Vec<usize>),
    RefreshDiagnostics,
    Refresh,
    Hover,
    Print(String),

    OpenPicker(Option<String>, Vec<String>, Option<i32>),
    OpenLivePicker(Option<String>, Vec<String>, Option<i32>, Option<String>),
    Picked(String, Option<i32>),
    PreviewTheme(String),
    SetTheme(String),
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

#[derive(Debug, Clone)]
pub struct HighlightSpan {
    pub start: usize,
    pub end: usize,
    pub order: usize,
    pub style: Style,
}

impl HighlightSpan {
    fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct HighlightCacheKey {
    buffer_index: usize,
    revision: u64,
    file: Option<String>,
    vtop: usize,
    height: usize,
}

#[derive(Debug, Clone)]
struct HighlightCacheEntry {
    spans: Vec<HighlightSpan>,
}

struct StyleCursor<'a> {
    spans: &'a [HighlightSpan],
    next: usize,
    active: Vec<usize>,
}

impl<'a> StyleCursor<'a> {
    fn new(spans: &'a [HighlightSpan]) -> Self {
        Self {
            spans,
            next: 0,
            active: Vec::new(),
        }
    }

    fn style_at(&mut self, pos: usize) -> Option<&'a Style> {
        while self
            .spans
            .get(self.next)
            .is_some_and(|span| span.start <= pos)
        {
            self.active.push(self.next);
            self.next += 1;
        }

        self.active.retain(|index| self.spans[*index].end > pos);

        self.active
            .iter()
            .copied()
            .min_by(|left, right| {
                let left = &self.spans[*left];
                let right = &self.spans[*right];
                left.len()
                    .cmp(&right.len())
                    .then_with(|| right.order.cmp(&left.order))
            })
            .map(|index| &self.spans[index].style)
    }
}

fn style_info_to_highlight_spans(style_info: Vec<StyleInfo>) -> Vec<HighlightSpan> {
    let mut spans = style_info
        .into_iter()
        .enumerate()
        .filter_map(|(order, style_info)| {
            (style_info.start < style_info.end).then_some(HighlightSpan {
                start: style_info.start,
                end: style_info.end,
                order,
                style: style_info.style,
            })
        })
        .collect::<Vec<_>>();

    spans.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| left.end.cmp(&right.end))
            .then_with(|| left.order.cmp(&right.order))
    });

    spans
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

    /// Documents already opened through LSP for this editor session.
    lsp_opened_documents: HashSet<String>,

    /// Editor configuration settings
    config: Config,

    /// Visual theme settings
    pub theme: Theme,

    /// Plugin system registry
    plugin_registry: PluginRegistry,

    /// Syntax highlighting engine
    highlighter: Highlighter,

    /// Cached syntax highlight spans for recently rendered viewport slices.
    highlight_cache: HashMap<HighlightCacheKey, HighlightCacheEntry>,

    /// All open buffers
    buffers: Vec<Buffer>,

    /// Index of the currently active buffer
    current_buffer_index: usize,

    /// Window manager handling splits and layout
    window_manager: WindowManager,

    /// Terminal output handle
    stdout: std::io::Stdout,

    /// Whether render operations should write terminal escape sequences
    terminal_output_enabled: bool,

    /// Whether the terminal window currently has focus.
    is_focused: bool,

    /// Incremented after full renders so event handling can avoid duplicate frames.
    render_generation: u64,

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

    /// Cursor position where the current insert session began.
    insert_entry_cursor: Option<CursorSnapshot>,

    /// Partial command being entered
    waiting_command: Option<String>,

    /// Next key action to process
    waiting_key_action: Option<KeyAction>,

    /// Actions that are pending while in visual mode
    pending_select_action: Option<ActionOnSelection>,

    /// Partially entered visual-mode text object, such as `i` in `viw`.
    pending_visual_text_object_scope: Option<TextObjectScope>,

    /// Partially entered normal-mode Vim operator, such as `d` in `diw`.
    pending_operator: Option<PendingOperator>,

    /// Executed actions
    actions: Vec<Action>,

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

    panel_manager: plugin::PanelManager,

    directory_watchers: HashMap<i32, DirectoryWatcher>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditOperator {
    Delete,
    Change,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingOperatorStep {
    Operator,
    TextObjectScope(TextObjectScope),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingOperator {
    operator: EditOperator,
    step: PendingOperatorStep,
}

impl PendingOperator {
    fn new(operator: EditOperator) -> Self {
        Self {
            operator,
            step: PendingOperatorStep::Operator,
        }
    }
}

impl EditOperator {
    fn as_char(self) -> char {
        match self {
            EditOperator::Delete => 'd',
            EditOperator::Change => 'c',
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextObjectScope {
    Inner,
    Around,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextObjectKind {
    Word,
    Delimited { open: char, close: char },
    Quote(char),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextUnitKind {
    Keyword,
    Punctuation,
    Symbol,
}

fn is_keyword_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn text_object_kind_for_key(c: char) -> Option<TextObjectKind> {
    match c {
        'w' => Some(TextObjectKind::Word),
        '(' | ')' | 'b' => Some(TextObjectKind::Delimited {
            open: '(',
            close: ')',
        }),
        '[' | ']' => Some(TextObjectKind::Delimited {
            open: '[',
            close: ']',
        }),
        '{' | '}' | 'B' => Some(TextObjectKind::Delimited {
            open: '{',
            close: '}',
        }),
        '<' | '>' => Some(TextObjectKind::Delimited {
            open: '<',
            close: '>',
        }),
        '"' | 'q' => Some(TextObjectKind::Quote('"')),
        '\'' | '`' => Some(TextObjectKind::Quote(c)),
        _ => None,
    }
}

fn text_unit_kind(c: char) -> Option<TextUnitKind> {
    if c.is_whitespace() {
        None
    } else if is_keyword_char(c) {
        Some(TextUnitKind::Keyword)
    } else if c.is_ascii_punctuation() {
        Some(TextUnitKind::Punctuation)
    } else {
        Some(TextUnitKind::Symbol)
    }
}

fn is_escaped_quote(chars: &[char], idx: usize) -> bool {
    let mut slash_count = 0;
    let mut prev = idx;
    while prev > 0 {
        prev -= 1;
        if chars[prev] != '\\' {
            break;
        }
        slash_count += 1;
    }
    slash_count % 2 == 1
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Content {
    kind: ContentKind,
    text: String,
}

impl Content {
    pub fn charwise(text: String) -> Self {
        Self {
            kind: ContentKind::Charwise,
            text,
        }
    }

    pub fn linewise(text: String) -> Self {
        Self {
            kind: ContentKind::Linewise,
            text,
        }
    }
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
            lsp_opened_documents: HashSet::new(),
            config,
            theme,
            plugin_registry,
            highlighter,
            highlight_cache: HashMap::new(),
            buffers,
            current_buffer_index: 0,
            window_manager,
            stdout,
            terminal_output_enabled: true,
            is_focused: true,
            render_generation: 0,
            size,
            vtop: 0,
            vleft: 0,
            cx: 0,
            cy: 0,
            prev_highlight_y: None,
            vx,
            mode: Mode::Normal,
            insert_entry_cursor: None,
            waiting_command: None,
            waiting_key_action: None,
            pending_select_action: None,
            pending_visual_text_object_scope: None,
            pending_operator: None,
            actions: vec![],
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
            panel_manager: plugin::PanelManager::default(),
            directory_watchers: HashMap::new(),
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

    fn set_active_window(&mut self, window_id: usize) -> bool {
        if window_id == self.window_manager.active_window_id() {
            return false;
        }

        self.sync_to_window();
        self.window_manager.set_active(window_id);
        self.sync_with_window();
        true
    }

    fn update_window_layout(
        &mut self,
        update: impl FnOnce(&mut WindowManager) -> Option<()>,
    ) -> bool {
        self.sync_to_window();
        if update(&mut self.window_manager).is_some() {
            self.sync_with_window();
            true
        } else {
            false
        }
    }

    fn resize_window_layout(&mut self, terminal_size: (usize, usize)) {
        self.sync_to_window();
        let (reserved_left, reserved_right) = self.reserved_panel_widths(terminal_size.0);
        self.window_manager.resize_with_origin(
            Point::new(reserved_left, 0),
            (
                terminal_size
                    .0
                    .saturating_sub(reserved_left)
                    .saturating_sub(reserved_right),
                terminal_size.1,
            ),
        );
        self.sync_with_window();
    }

    fn apply_panel_layout(&mut self) {
        self.sync_to_window();
        let (reserved_left, reserved_right) = self.reserved_panel_widths(self.size.0 as usize);
        self.window_manager.resize_with_origin(
            Point::new(reserved_left, 0),
            (
                (self.size.0 as usize)
                    .saturating_sub(reserved_left)
                    .saturating_sub(reserved_right),
                self.size.1 as usize,
            ),
        );
    }

    fn reserved_panel_widths(&self, terminal_width: usize) -> (usize, usize) {
        let max_reserved = terminal_width.saturating_sub(10);
        let reserved_left = self.panel_manager.reserved_left_width().min(max_reserved);
        let reserved_right = self
            .panel_manager
            .reserved_right_width()
            .min(max_reserved.saturating_sub(reserved_left));
        (reserved_left, reserved_right)
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
        (self.size.1 as usize).saturating_sub(2)
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
            return grapheme_len(line);
        }
        0
    }

    fn grapheme_to_char_on_line(&self, x: usize, y: usize) -> usize {
        self.current_buffer()
            .get(y)
            .map(|line| grapheme_to_char(line.trim_end_matches('\n'), x))
            .unwrap_or(x)
    }

    fn char_to_grapheme_on_line(&self, x: usize, y: usize) -> usize {
        self.current_buffer()
            .get(y)
            .map(|line| char_to_grapheme(line.trim_end_matches('\n'), x))
            .unwrap_or(x)
    }

    fn next_word_search_char_on_line(&self, x: usize, y: usize) -> usize {
        let Some(line) = self.current_buffer().get(y) else {
            return x;
        };
        let line = line.trim_end_matches('\n');
        if x > 0
            && line
                .graphemes(true)
                .nth(x)
                .is_some_and(|grapheme| grapheme.chars().all(char::is_whitespace))
        {
            grapheme_to_char(line, x - 1)
        } else {
            grapheme_to_char(line, x)
        }
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
        if let Some(line) = self.current_buffer().get(n) {
            let line = line.trim_end_matches('\n');
            return grapheme_len(line);
        }
        0
    }

    fn last_cell_for_line(&self, n: usize) -> usize {
        self.length_for_line(n).saturating_sub(1)
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
        self.current_buffer()
            .len()
            .saturating_add(1)
            .to_string()
            .len()
            + 1
    }

    fn gutter_width_for_buffer_index(&self, buffer_index: usize) -> usize {
        self.buffers
            .get(buffer_index)
            .map(|buffer| buffer.len().saturating_add(1).to_string().len() + 1)
            .unwrap_or_else(|| self.gutter_width())
    }

    fn gutter_width_for_window(&self, window: &crate::window::Window) -> usize {
        self.gutter_width_for_buffer_index(window.buffer_index)
    }

    pub fn highlight(&mut self, file: Option<&str>, code: &str) -> anyhow::Result<Vec<StyleInfo>> {
        self.highlighter.highlight_for_file(file, code)
    }

    fn highlight_spans(
        &mut self,
        file: Option<&str>,
        code: &str,
    ) -> anyhow::Result<Vec<HighlightSpan>> {
        self.highlight(file, code)
            .map(style_info_to_highlight_spans)
    }

    fn cached_viewport_highlight_spans(
        &mut self,
        key: HighlightCacheKey,
        file: Option<&str>,
        code: &str,
    ) -> anyhow::Result<Vec<HighlightSpan>> {
        if let Some(entry) = self.highlight_cache.get(&key) {
            return Ok(entry.spans.clone());
        }

        let spans = self.highlight_spans(file, code)?;
        if self.highlight_cache.len() >= 64 {
            self.highlight_cache.clear();
        }
        self.highlight_cache.insert(
            key,
            HighlightCacheEntry {
                spans: spans.clone(),
            },
        );
        Ok(spans)
    }

    fn fill_line(&mut self, buffer: &mut RenderBuffer, x: usize, y: usize, style: &Style) {
        let width = self.vwidth().saturating_sub(x);
        let line_fill = " ".repeat(width);
        buffer.set_text(x, y, &line_fill, style);
    }

    fn draw_line(&mut self, buffer: &mut RenderBuffer) {
        let line = self.viewport_line(self.cy).unwrap_or_default();
        let file = self.current_buffer().file.clone();
        let style_info = self
            .highlight_spans(file.as_deref(), &line)
            .unwrap_or_default();
        let default_style = self.theme.style.clone();
        let mut style_cursor = StyleCursor::new(&style_info);

        let mut x = self.vx;
        let mut iter = line.char_indices().peekable();

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
                if let Some(style) = style_cursor.style_at(pos) {
                    buffer.set_char(x, self.cy, c, style, &self.theme);
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

        let x = self.gutter_width() + display_width(line.trim_end_matches('\n')) + 5;

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

    fn max_cursor_x_for_line_length(&self, line_length: usize) -> usize {
        if self.is_insert() {
            line_length
        } else {
            line_length.saturating_sub(1)
        }
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
    fn last_navigable_line(&self) -> usize {
        self.current_buffer().last_navigable_line()
    }

    fn check_bounds(&mut self) -> bool {
        let old_position = (self.cx, self.cy, self.vtop);
        let last_line = if self.is_insert() {
            self.current_buffer().len()
        } else {
            self.last_navigable_line()
        };
        let viewport_height = self.vheight().max(1);
        let max_vtop = last_line.saturating_sub(viewport_height.saturating_sub(1));

        self.vtop = self.vtop.min(max_vtop);

        let buffer_line = (self.vtop + self.cy).min(last_line);
        self.cy = buffer_line
            .saturating_sub(self.vtop)
            .min(viewport_height.saturating_sub(1));

        let scrolloff = self
            .config
            .scrolloff
            .unwrap_or(0)
            .min(viewport_height.saturating_sub(1));
        if scrolloff > 0 {
            if buffer_line < self.vtop + scrolloff {
                self.vtop = buffer_line.saturating_sub(scrolloff);
            } else if buffer_line >= self.vtop + viewport_height.saturating_sub(scrolloff) {
                self.vtop = buffer_line
                    .saturating_add(scrolloff)
                    .saturating_add(1)
                    .saturating_sub(viewport_height);
            }

            self.vtop = self.vtop.min(max_vtop);
            self.cy = buffer_line.saturating_sub(self.vtop);
        }

        let max_cursor_x = self.max_cursor_x_for_line_length(self.line_length());

        if self.cx > max_cursor_x {
            self.cx = max_cursor_x;
        }
        let viewport_width = self.vwidth();
        if viewport_width > 0 && self.cx >= viewport_width {
            self.cx = viewport_width - 1;
        }

        old_position != (self.cx, self.cy, self.vtop)
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
            .execute(event::EnableFocusChange)?
            .execute(terminal::EnterAlternateScreen)?
            .execute(terminal::Clear(terminal::ClearType::All))?;

        let mut runtime = Runtime::new();
        for (name, path) in &self.config.plugins {
            let path = Config::path("plugins").join(path);
            self.plugin_registry
                .add(name, path.to_string_lossy().as_ref());
        }
        self.plugin_registry.initialize(&mut runtime).await?;
        self.plugin_registry
            .notify(&mut runtime, "editor:ready", json!({}))
            .await?;

        let mut buffer = RenderBuffer::new(
            self.size.0 as usize,
            self.size.1 as usize,
            &Style::default(),
        );
        self.ensure_current_buffer_lsp_opened().await?;
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

                    for (watch_id, payload) in self.poll_directory_watchers() {
                        self.plugin_registry
                            .notify(
                                &mut runtime,
                                &format!("filesystem:changed:{watch_id}"),
                                payload,
                            )
                            .await?;
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

                    // Always pump LSP responses. `recv_response` completes the
                    // initialize handshake and flushes queued didOpen/change
                    // messages, so it must not depend on diagnostic display.
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
                            PluginRequest::OpenLivePicker(title, id, items, initial_selection) => {
                                let items = items.iter().map(|v| match v {
                                    serde_json::Value::String(s) => s.clone(),
                                    val => val.to_string(),
                                }).collect();
                                self.execute(&Action::OpenLivePicker(title, items, id, initial_selection), &mut buffer, &mut runtime).await?;
                            }
                            PluginRequest::BufferInsert { x, y, text } => {
                                self.begin_transaction("plugin insert");
                                self.replace_range(
                                    TextRange::insertion(TextPosition::new(y, x)),
                                    &text,
                                );
                                self.commit_transaction(self.cursor_snapshot());
                                self.notify_change(&mut runtime).await?;
                                self.render(&mut buffer)?;
                            }
                            PluginRequest::BufferDelete { x, y, length } => {
                                self.begin_transaction("plugin delete");
                                self.replace_range(
                                    TextRange::new(
                                        TextPosition::new(y, x),
                                        TextPosition::new(y, x + length),
                                    ),
                                    "",
                                );
                                self.commit_transaction(self.cursor_snapshot());
                                self.notify_change(&mut runtime).await?;
                                self.render(&mut buffer)?;
                            }
                            PluginRequest::BufferReplace { x, y, length, text } => {
                                self.begin_transaction("plugin replace");
                                self.replace_range(
                                    TextRange::new(
                                        TextPosition::new(y, x),
                                        TextPosition::new(y, x + length),
                                    ),
                                    &text,
                                );
                                self.commit_transaction(self.cursor_snapshot());
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
                                    crate::unicode_utils::grapheme_to_column(line, self.cx)
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
                                    self.cx = crate::unicode_utils::column_to_grapheme(line, column);
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
                                        "scrolloff" => json!(self.config.scrolloff),
                                        "show_diagnostics" => json!(self.config.show_diagnostics),
                                        "startup_file_count" => json!(self.config.startup_file_count),
                                        "cwd" => json!(std::env::current_dir().ok().map(|path| path.to_string_lossy().to_string())),
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
                                        "scrolloff": self.config.scrolloff,
                                        "show_diagnostics": self.config.show_diagnostics,
                                        "startup_file_count": self.config.startup_file_count,
                                        "cwd": std::env::current_dir().ok().map(|path| path.to_string_lossy().to_string()),
                                        "keys": self.config.keys,
                                    })
                                };
                                self.plugin_registry
                                    .notify(&mut runtime, "config:value", json!({ "value": config_value }))
                                    .await?;
                            }
                            PluginRequest::GetEditorState { request_id } => {
                                let snapshot = self.editor_state_snapshot();
                                self.plugin_registry
                                    .notify(
                                        &mut runtime,
                                        &format!("editor:state:{request_id}"),
                                        serde_json::to_value(snapshot)?,
                                    )
                                    .await?;
                            }
                            PluginRequest::RestoreEditorState { request_id, snapshot } => {
                                let result = self
                                    .restore_editor_state(snapshot, &mut buffer)
                                    .await;
                                let payload = match result {
                                    Ok(result) => serde_json::to_value(result)?,
                                    Err(err) => json!({
                                        "restored": false,
                                        "openedFiles": [],
                                        "skippedFiles": [],
                                        "warnings": [err.to_string()],
                                    }),
                                };
                                self.plugin_registry
                                    .notify(
                                        &mut runtime,
                                        &format!("editor:restore:{request_id}"),
                                        payload,
                                    )
                                    .await?;
                                self.render(&mut buffer)?;
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
                            PluginRequest::CreatePanel { id, config } => {
                                self.panel_manager.create_panel(id, config);
                                self.apply_panel_layout();
                                self.render(&mut buffer)?;
                            }
                            PluginRequest::UpdatePanel { id, rows } => {
                                self.panel_manager.update_panel(&id, rows);
                                self.render(&mut buffer)?;
                            }
                            PluginRequest::FocusPanel { id } => {
                                self.panel_manager.focus_panel(&id);
                                self.render(&mut buffer)?;
                            }
                            PluginRequest::FocusEditor => {
                                self.panel_manager.focus_editor();
                                self.render(&mut buffer)?;
                            }
                            PluginRequest::ClosePanel { id } => {
                                self.panel_manager.close_panel(&id);
                                self.apply_panel_layout();
                                self.render(&mut buffer)?;
                            }
                            PluginRequest::ListDirectory { path, request_id } => {
                                let payload = directory_listing(&path);
                                self.plugin_registry
                                    .notify(
                                        &mut runtime,
                                        &format!("filesystem:directory:{request_id}"),
                                        payload,
                                    )
                                    .await?;
                            }
                            PluginRequest::WatchDirectory { path, watch_id } => {
                                self.directory_watchers.insert(watch_id, DirectoryWatcher {
                                    snapshot: directory_listing(&path),
                                    path,
                                    last_checked: Instant::now(),
                                });
                            }
                            PluginRequest::UnwatchDirectory { watch_id } => {
                                self.directory_watchers.remove(&watch_id);
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
                                self.resize_window_layout((width as usize, height as usize));
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

                            if self.handle_focus_event(&ev, &mut buffer)? {
                                continue;
                            }

                            let render_generation = self.render_generation;

                            if let Some(action) = self.handle_event(&ev)? {
                                if self.handle_key_action(&ev, &action, &mut buffer, &mut runtime).await? {
                                    break;
                                }
                            }

                            if self.render_generation == render_generation {
                                self.render(&mut buffer)?;
                            }
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

        let snapshot = self.editor_state_snapshot();
        if let Err(err) = self
            .plugin_registry
            .before_exit(&mut runtime, snapshot)
            .await
        {
            log!("Plugin beforeExit failed: {}", err);
        }
        if let Err(err) = self.plugin_registry.deactivate_all(&mut runtime).await {
            log!("Plugin deactivate failed: {}", err);
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

                    if method == "textDocument/diagnostic" && self.config.show_diagnostics {
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
                                let (completion_x, completion_y) =
                                    self.render_cursor_position().unwrap_or((self.cx, self.cy));
                                self.completion_ui.show_with_bounds(
                                    completion_response.items,
                                    completion_x,
                                    completion_y,
                                    self.size.0 as usize,
                                    self.size.1 as usize,
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
                    if self.config.show_diagnostics {
                        self.add_diagnostics(msg.uri.as_deref(), &msg.diagnostics)
                    } else {
                        None
                    }
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

        if self.panel_manager.focused_panel_id().is_some() {
            if let Some(action) = self.handle_panel_event(ev) {
                return Ok(Some(action));
            }

            let normal = self.config.keys.normal.clone();
            return Ok(self.event_to_key_action(&normal, ev));
        }

        Ok(match self.mode {
            Mode::Normal => self.handle_normal_event(ev),
            Mode::Insert => self.handle_insert_event(ev)?,
            Mode::Command => self.handle_command_event(ev),
            Mode::Search => self.handle_search_event(ev),
            Mode::Visual | Mode::VisualLine | Mode::VisualBlock => self.handle_visual_event(ev),
        })
    }

    fn handle_focus_event(
        &mut self,
        ev: &event::Event,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<bool> {
        match ev {
            Event::FocusLost => {
                if self.is_focused {
                    self.is_focused = false;
                    self.render(buffer)?;
                } else {
                    self.draw_cursor()?;
                }
                Ok(true)
            }
            Event::FocusGained => {
                if !self.is_focused {
                    self.is_focused = true;
                    self.render(buffer)?;
                } else {
                    self.draw_cursor()?;
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn handle_panel_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        let Event::Key(ref event) = ev else {
            return None;
        };

        let action = match event.code {
            KeyCode::Esc => {
                self.panel_manager.focus_editor();
                return Some(KeyAction::Single(Action::Refresh));
            }
            KeyCode::Up | KeyCode::Char('k') => "up",
            KeyCode::Down | KeyCode::Char('j') => "down",
            KeyCode::Left | KeyCode::Char('h') => "collapse",
            KeyCode::Right | KeyCode::Char('l') => "expand",
            KeyCode::Enter => "activate",
            _ => return None,
        };

        let height = self.size.1 as usize;
        self.panel_manager
            .handle_focused_key(action, height)
            .and_then(|event| {
                serde_json::to_value(&event).ok().map(|payload| {
                    KeyAction::Multiple(vec![
                        Action::NotifyPlugins(format!("panel:event:{}", event.panel_id), payload),
                        Action::Refresh,
                    ])
                })
            })
    }

    fn poll_directory_watchers(&mut self) -> Vec<(i32, Value)> {
        let now = Instant::now();
        let mut changes = Vec::new();

        for (watch_id, watcher) in self.directory_watchers.iter_mut() {
            if now.duration_since(watcher.last_checked) < Duration::from_millis(500) {
                continue;
            }
            watcher.last_checked = now;

            let next_snapshot = directory_listing(&watcher.path);
            if next_snapshot != watcher.snapshot {
                watcher.snapshot = next_snapshot.clone();
                changes.push((*watch_id, next_snapshot));
            }
        }

        changes
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
            "bd",
            "bdelete",
            "buffer-delete",
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

            if cmd == "bd" || cmd == "bdelete" || cmd == "buffer-delete" {
                actions.push(Action::DeleteBuffer(parsed.is_forced()));
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

    fn delete_last_char(text: &mut String) {
        text.pop();
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
                    Self::delete_last_char(&mut self.command);
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
                        Self::delete_last_char(&mut self.search_term);
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
        if let Some(action) = self.handle_visual_text_object_event(ev) {
            return Some(action);
        }

        let visual = self.config.keys.visual.clone();
        self.event_to_key_action(&visual, ev)
    }

    fn handle_visual_text_object_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        if !matches!(self.mode, Mode::Visual) {
            return None;
        }

        let Event::Key(KeyEvent { code, .. }) = ev else {
            return None;
        };

        if *code == KeyCode::Esc {
            self.pending_visual_text_object_scope = None;
            self.waiting_command = None;
            return None;
        }

        if let Some(scope) = self.pending_visual_text_object_scope.take() {
            let KeyCode::Char(c) = code else {
                return self.pending_visual_text_object_invalid();
            };

            self.waiting_command = None;
            let Some(kind) = text_object_kind_for_key(*c) else {
                return self.pending_visual_text_object_invalid();
            };

            let Some(range) = self.text_object_range(scope, kind) else {
                self.last_error = Some("text object not found".to_string());
                return Some(KeyAction::None);
            };

            if self.select_text_range(range) {
                return Some(KeyAction::Single(Action::Refresh));
            }
            self.last_error = Some("text object not found".to_string());
            return Some(KeyAction::None);
        }

        let KeyCode::Char(c) = code else {
            return None;
        };

        let scope = match c {
            'i' => TextObjectScope::Inner,
            'a' => TextObjectScope::Around,
            _ => return None,
        };

        self.pending_visual_text_object_scope = Some(scope);
        self.waiting_command = Some(c.to_string());
        Some(KeyAction::None)
    }

    fn pending_visual_text_object_invalid(&mut self) -> Option<KeyAction> {
        self.pending_visual_text_object_scope = None;
        self.waiting_command = None;
        self.last_error = Some("invalid text object".to_string());
        Some(KeyAction::None)
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
        if let Some(action) = self.handle_operator_event(ev) {
            return Some(action);
        }

        let normal = self.config.keys.normal.clone();
        self.event_to_key_action(&normal, ev)
    }

    fn handle_operator_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        let Event::Key(KeyEvent { code, .. }) = ev else {
            return None;
        };

        if *code == KeyCode::Esc {
            self.pending_operator = None;
            self.waiting_command = None;
            self.repeater = None;
            return Some(KeyAction::None);
        }

        let KeyCode::Char(c) = code else {
            return self.pending_operator_invalid();
        };

        if let Some(pending) = self.pending_operator.take() {
            self.waiting_command = None;
            return self.handle_pending_operator(pending, *c);
        }

        let operator = match c {
            'd' => EditOperator::Delete,
            'c' => EditOperator::Change,
            _ => return None,
        };

        self.pending_operator = Some(PendingOperator::new(operator));
        self.waiting_command = Some(c.to_string());
        self.repeater = None;
        Some(KeyAction::None)
    }

    fn handle_pending_operator(&mut self, pending: PendingOperator, c: char) -> Option<KeyAction> {
        match pending.step {
            PendingOperatorStep::Operator => match c {
                'd' if pending.operator == EditOperator::Delete => {
                    Some(KeyAction::Single(Action::DeleteCurrentLine))
                }
                'c' if pending.operator == EditOperator::Change => {
                    Some(KeyAction::Single(Action::ChangeCurrentLine))
                }
                'w' => self.operator_action_for_range(
                    pending.operator,
                    self.word_motion_range(),
                    "no word under cursor",
                ),
                'i' => {
                    self.waiting_command = Some(format!("{}i", pending.operator.as_char()));
                    self.pending_operator = Some(PendingOperator {
                        step: PendingOperatorStep::TextObjectScope(TextObjectScope::Inner),
                        ..pending
                    });
                    Some(KeyAction::None)
                }
                'a' => {
                    self.waiting_command = Some(format!("{}a", pending.operator.as_char()));
                    self.pending_operator = Some(PendingOperator {
                        step: PendingOperatorStep::TextObjectScope(TextObjectScope::Around),
                        ..pending
                    });
                    Some(KeyAction::None)
                }
                _ => self.pending_operator_invalid(),
            },
            PendingOperatorStep::TextObjectScope(scope) => {
                let Some(kind) = text_object_kind_for_key(c) else {
                    return self.pending_operator_invalid();
                };
                self.operator_action_for_range(
                    pending.operator,
                    self.text_object_range(scope, kind),
                    "text object not found",
                )
            }
        }
    }

    fn pending_operator_invalid(&mut self) -> Option<KeyAction> {
        self.pending_operator = None;
        self.waiting_command = None;
        self.repeater = None;
        self.last_error = Some("invalid operator motion".to_string());
        Some(KeyAction::None)
    }

    fn operator_action_for_range(
        &mut self,
        operator: EditOperator,
        range: Option<TextRange>,
        error: &str,
    ) -> Option<KeyAction> {
        self.pending_operator = None;
        self.waiting_command = None;
        let Some(range) = range else {
            self.last_error = Some(error.to_string());
            return Some(KeyAction::None);
        };

        let action = match operator {
            EditOperator::Delete => Action::DeleteTextRange(range),
            EditOperator::Change => Action::ChangeTextRange(range),
        };
        self.repeater = None;
        Some(KeyAction::Single(action))
    }

    fn cursor_text_position(&self) -> TextPosition {
        let line = self.buffer_line();
        TextPosition::new(line, self.grapheme_to_char_on_line(self.cx, line))
    }

    fn word_motion_range(&self) -> Option<TextRange> {
        let start = self.cursor_text_position();
        let (end_x, end_y) = self
            .current_buffer()
            .find_next_word((start.character, start.line))?;
        let end = TextPosition::new(end_y, end_x);
        (start != end).then(|| TextRange::new(start, end))
    }

    fn text_object_range(&self, scope: TextObjectScope, kind: TextObjectKind) -> Option<TextRange> {
        match kind {
            TextObjectKind::Word => self.word_text_object_range(scope),
            TextObjectKind::Delimited { open, close } => {
                self.delimited_text_object_range(scope, open, close)
            }
            TextObjectKind::Quote(quote) => self.quote_text_object_range(scope, quote),
        }
    }

    fn word_text_object_range(&self, scope: TextObjectScope) -> Option<TextRange> {
        let line_index = self.buffer_line();
        let line = self.current_buffer().get(line_index)?;
        let line = line.trim_end_matches('\n');
        let chars = line.chars().collect::<Vec<_>>();
        if chars.is_empty() {
            return None;
        }

        let cursor = self
            .grapheme_to_char_on_line(self.cx, line_index)
            .min(chars.len().saturating_sub(1));
        let target = if text_unit_kind(chars[cursor]).is_some() {
            cursor
        } else {
            (cursor..chars.len())
                .find(|idx| text_unit_kind(chars[*idx]).is_some())
                .or_else(|| {
                    (0..=cursor)
                        .rev()
                        .find(|idx| text_unit_kind(chars[*idx]).is_some())
                })?
        };

        let kind = text_unit_kind(chars[target])?;
        let mut start = target;
        while start > 0 && text_unit_kind(chars[start - 1]) == Some(kind) {
            start -= 1;
        }

        let mut end = target + 1;
        while end < chars.len() && text_unit_kind(chars[end]) == Some(kind) {
            end += 1;
        }

        if scope == TextObjectScope::Around {
            if end < chars.len() && chars[end].is_whitespace() {
                while end < chars.len() && chars[end].is_whitespace() {
                    end += 1;
                }
            } else {
                while start > 0 && chars[start - 1].is_whitespace() {
                    start -= 1;
                }
            }
        }

        Some(TextRange::new(
            TextPosition::new(line_index, start),
            TextPosition::new(line_index, end),
        ))
    }

    fn delimited_text_object_range(
        &self,
        scope: TextObjectScope,
        open: char,
        close: char,
    ) -> Option<TextRange> {
        let contents = self.current_buffer().contents();
        let chars = contents.chars().collect::<Vec<_>>();
        let cursor = self
            .current_buffer()
            .position_to_char_idx(self.cursor_text_position());

        let mut stack = Vec::new();
        let mut best_pair = None;

        for (idx, c) in chars.iter().copied().enumerate() {
            if c == open {
                stack.push(idx);
            } else if c == close {
                let Some(open_idx) = stack.pop() else {
                    continue;
                };
                if open_idx <= cursor && cursor <= idx {
                    if best_pair.is_none_or(|(best_open_idx, _)| open_idx > best_open_idx) {
                        best_pair = Some((open_idx, idx));
                    }
                }
            }
        }

        let (open_idx, close_idx) = best_pair?;
        let (start_idx, end_idx) = match scope {
            TextObjectScope::Inner => (open_idx + 1, close_idx),
            TextObjectScope::Around => (open_idx, close_idx + 1),
        };

        Some(TextRange::new(
            self.position_for_char_idx(start_idx),
            self.position_for_char_idx(end_idx),
        ))
    }

    fn quote_text_object_range(&self, scope: TextObjectScope, quote: char) -> Option<TextRange> {
        let line_index = self.buffer_line();
        let line = self.current_buffer().get(line_index)?;
        let line = line.trim_end_matches('\n');
        let chars = line.chars().collect::<Vec<_>>();
        let cursor = self.grapheme_to_char_on_line(self.cx, line_index);

        let quote_positions = chars
            .iter()
            .enumerate()
            .filter_map(|(idx, c)| (*c == quote && !is_escaped_quote(&chars, idx)).then_some(idx))
            .collect::<Vec<_>>();

        for pair in quote_positions.chunks(2) {
            if pair.len() != 2 {
                continue;
            }
            let start = pair[0];
            let end = pair[1];
            if start <= cursor && cursor <= end {
                let (range_start, range_end) = match scope {
                    TextObjectScope::Inner => (start + 1, end),
                    TextObjectScope::Around => (start, end + 1),
                };
                return Some(TextRange::new(
                    TextPosition::new(line_index, range_start),
                    TextPosition::new(line_index, range_end),
                ));
            }
        }

        None
    }

    fn position_for_char_idx(&self, char_idx: usize) -> TextPosition {
        let mut remaining = char_idx;
        let last_line = self.current_buffer().len();

        for line_index in 0..=last_line {
            let Some(line) = self.current_buffer().get(line_index) else {
                break;
            };
            let line_chars = line.chars().count();
            let line_content_chars = line.trim_end_matches('\n').chars().count();

            if remaining <= line_content_chars {
                return TextPosition::new(line_index, remaining);
            }

            if remaining < line_chars {
                return TextPosition::new(line_index, line_content_chars);
            }

            remaining = remaining.saturating_sub(line_chars);
        }

        TextPosition::new(last_line, self.length_for_line(last_line))
    }

    pub fn cleanup(&mut self) -> anyhow::Result<()> {
        write!(self.stdout, "\x1b]112\x1b\\")?;
        self.stdout
            .execute(terminal::LeaveAlternateScreen)?
            .execute(event::DisableFocusChange)?
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
                if self.vtop + self.cy < self.last_navigable_line() {
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

                    let byte_offset = grapheme_to_byte(line, self.cx);

                    // Find previous grapheme boundary
                    if let Some(prev_byte) = prev_grapheme_boundary(line, byte_offset) {
                        self.cx = crate::unicode_utils::byte_to_grapheme(line, prev_byte);
                    } else if self.cx > 0 {
                        self.cx = 0;
                    }

                    if self.cx < self.vleft {
                        self.cx = self.vleft;
                    }
                }
                self.draw_cursor()?;
                self.notify_cursor_move(runtime).await?;
            }
            Action::MoveRight => {
                // Move by grapheme clusters
                if let Some(line) = self.current_line_contents() {
                    let line = line.trim_end_matches('\n');
                    let max_graphemes = grapheme_len(line);
                    let max_cursor_x = self.max_cursor_x_for_line_length(max_graphemes);

                    if self.cx < max_cursor_x {
                        let current_byte = grapheme_to_byte(line, self.cx);

                        // Find next grapheme boundary
                        if let Some(next_byte) = next_grapheme_boundary(line, current_byte) {
                            self.cx = crate::unicode_utils::byte_to_grapheme(line, next_byte)
                                .min(max_cursor_x);
                        } else {
                            self.cx = max_cursor_x;
                        }
                    } else if self.cx > max_cursor_x {
                        self.cx = max_cursor_x;
                    }
                }
                self.draw_cursor()?;
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
                    self.cx = line
                        .trim_end_matches('\n')
                        .graphemes(true)
                        .position(|g| !g.chars().all(char::is_whitespace))
                        .unwrap_or(0);
                }
            }
            Action::MoveToLastLineChar => {
                if let Some(line) = self.current_line_contents() {
                    let line = line.trim_end_matches('\n');
                    let trailing = line
                        .graphemes(true)
                        .rev()
                        .position(|g| !g.chars().all(char::is_whitespace))
                        .unwrap_or(0);
                    self.cx = grapheme_len(line).saturating_sub(trailing + 1);
                }
            }
            Action::PageUp => {
                let target_line = self
                    .buffer_line()
                    .saturating_sub(self.vheight())
                    .min(self.last_navigable_line());
                self.vtop = target_line.saturating_sub(self.vheight().saturating_sub(1));
                self.cy = target_line.saturating_sub(self.vtop);
                self.render(buffer)?;
            }
            Action::PageDown => {
                let target_line =
                    (self.buffer_line() + self.vheight()).min(self.last_navigable_line());
                self.vtop = target_line.saturating_sub(self.vheight().saturating_sub(1));
                self.cy = target_line.saturating_sub(self.vtop);
                self.render(buffer)?;
            }
            Action::EnterMode(new_mode) => {
                add_to_history = false;
                let old_mode = self.mode;
                self.selection = None;
                self.pending_visual_text_object_scope = None;
                self.pending_operator = None;

                // check for a pending action to be executed on the selection
                let pending_select_action = self.pending_select_action.clone();
                if let Some(select_action) = pending_select_action {
                    self.execute(&select_action.action, buffer, runtime).await?;
                }

                if matches!(old_mode, Mode::Normal) && matches!(new_mode, Mode::Insert) {
                    self.insert_entry_cursor = Some(self.cursor_snapshot());
                    self.begin_transaction("insert");
                }

                if matches!(old_mode, Mode::Insert) && matches!(new_mode, Mode::Normal) {
                    if self.insert_entry_cursor.is_some_and(|entry| {
                        let y = self.buffer_line();
                        y > entry.y || (y == entry.y && self.cx > entry.x)
                    }) {
                        self.cx = self.cx.saturating_sub(1);
                    }
                    self.cx = self.cx.min(self.line_length().saturating_sub(1));
                    self.insert_entry_cursor = None;
                    let after_cursor = self.cursor_snapshot();
                    self.commit_transaction(after_cursor);
                    self.cancel_transaction_if_empty();
                }

                if matches!(new_mode, Mode::Search) {
                    self.search_term = String::new();
                }

                self.mode = *new_mode;

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

                if !matches!(old_mode, Mode::Normal) && matches!(new_mode, Mode::Normal) {
                    self.request_diagnostics().await?;
                }

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
                use crate::log;

                let started_transaction = !self.transaction_active();
                if started_transaction {
                    self.begin_transaction("insert char");
                }
                let line = self.buffer_line();
                let cx = self.cx;
                let char_cx = self.grapheme_to_char_on_line(cx, line);

                log!(
                    "InsertCharAtCursorPos - char: '{}' (U+{:04X}), cx: {}, line: {}",
                    c,
                    *c as u32,
                    cx,
                    line
                );

                // Log current line content before insertion
                if let Some(line_content) = self.current_buffer().get(line) {
                    log!("Line content before insert: {:?}", line_content);
                    log!("Line char count: {}", line_content.chars().count());
                }

                self.replace_range(
                    TextRange::insertion(TextPosition::new(line, char_cx)),
                    &c.to_string(),
                );
                self.notify_change(runtime).await?;

                // Move cursor by one character position (not display width)
                self.cx += grapheme_len(&c.to_string());
                if started_transaction {
                    self.commit_transaction(self.cursor_snapshot());
                }

                log!("Cursor after insert: cx = {}", self.cx);

                self.draw_line(buffer);
            }
            Action::DeleteCharAt(x, y) => {
                self.begin_transaction("delete char");
                self.replace_range(
                    TextRange::new(TextPosition::new(*y, *x), TextPosition::new(*y, *x + 1)),
                    "",
                );
                self.commit_transaction(self.cursor_snapshot());
                self.notify_change(runtime).await?;
                self.draw_line(buffer);
            }
            Action::DeleteRange(x0, y0, x1, y1) => {
                self.begin_transaction("delete range");
                self.replace_range(
                    TextRange::new(TextPosition::new(*y0, *x0), TextPosition::new(*y1, *x1)),
                    "",
                );
                self.commit_transaction(self.cursor_snapshot());
                self.notify_change(runtime).await?;
                self.render(buffer)?;
            }
            Action::DeleteTextRange(range) => {
                if self.delete_text_range(*range, "delete text object") {
                    self.notify_change(runtime).await?;
                }
                self.render(buffer)?;
            }
            Action::ChangeTextRange(range) => {
                if self.delete_text_range(*range, "change text object") {
                    self.notify_change(runtime).await?;
                }
                self.render(buffer)?;
                self.execute(&Action::EnterMode(Mode::Insert), buffer, runtime)
                    .await?;
            }
            Action::ChangeCurrentLine => {
                let line = self.buffer_line();
                let range = TextRange::new(
                    TextPosition::new(line, 0),
                    TextPosition::new(line, self.length_for_line(line)),
                );
                if self.delete_text_range(range, "change line") {
                    self.notify_change(runtime).await?;
                }
                self.render(buffer)?;
                self.execute(&Action::EnterMode(Mode::Insert), buffer, runtime)
                    .await?;
            }
            Action::DeleteCharAtCursorPos => {
                let cx = self.cx;
                let line = self.buffer_line();

                let deleted = self.current_buffer().get(line).and_then(|line_content| {
                    let line_content = line_content.trim_end_matches('\n');
                    line_content
                        .graphemes(true)
                        .nth(cx)
                        .map(|grapheme| grapheme.to_string())
                });

                if let Some(deleted) = deleted {
                    let started_transaction = !self.transaction_active();
                    if started_transaction {
                        self.begin_transaction("delete char");
                    }
                    let start = self.grapheme_to_char_on_line(cx, line);
                    let end = self.grapheme_to_char_on_line(cx + 1, line);
                    self.replace_range(
                        TextRange::new(
                            TextPosition::new(line, start),
                            TextPosition::new(line, end),
                        ),
                        "",
                    );
                    self.notify_change(runtime).await?;
                    if started_transaction {
                        self.commit_transaction(self.cursor_snapshot());
                    }
                    let _ = deleted;
                    self.draw_line(buffer);
                }
            }
            Action::ReplaceLineAt(y, contents) => {
                let line_len = self.length_for_line(*y);
                self.begin_transaction("replace line");
                self.replace_range(
                    TextRange::new(TextPosition::new(*y, 0), TextPosition::new(*y, line_len)),
                    contents,
                );
                self.commit_transaction(self.cursor_snapshot());
                self.notify_change(runtime).await?;
                self.draw_line(buffer);
            }
            Action::InsertNewLine => {
                let started_transaction = !self.transaction_active();
                if started_transaction {
                    self.begin_transaction("insert newline");
                }
                let spaces = self.current_line_indentation();

                let current_line = self.current_line_contents().unwrap_or_default();
                let current_line = current_line.trim_end();
                let current_line_len = grapheme_len(current_line);
                if self.cx > current_line_len {
                    self.cx = current_line_len;
                }
                let cursor_char = grapheme_to_char(current_line, self.cx);
                let before_cursor = char_prefix(current_line, cursor_char).to_string();
                let after_cursor = char_suffix(current_line, cursor_char).to_string();

                let line = self.buffer_line();
                self.replace_range(
                    TextRange::new(
                        TextPosition::new(line, 0),
                        TextPosition::new(line, current_line.chars().count()),
                    ),
                    &format!("{}\n{}{}", before_cursor, " ".repeat(spaces), after_cursor),
                );
                self.notify_change(runtime).await?;

                self.cx = spaces;
                self.cy += 1;

                if self.cy >= self.vheight() {
                    self.vtop += 1;
                    self.cy -= 1;
                }

                if started_transaction {
                    self.commit_transaction(self.cursor_snapshot());
                }
                self.render(buffer)?;
            }
            Action::SetWaitingKey(key_action) => {
                self.waiting_key_action = Some(*(key_action.clone()));
            }
            Action::DeleteCurrentLine => {
                let line = self.buffer_line();
                let end = if line < self.current_buffer().len() {
                    TextPosition::new(line + 1, 0)
                } else {
                    TextPosition::new(line, self.length_for_line(line))
                };
                let range = TextRange::new(TextPosition::new(line, 0), end);
                let deleted_text = self.current_buffer().text_in_range(range);
                self.registers
                    .insert(DEFAULT_REGISTER, Content::linewise(deleted_text));
                self.begin_transaction("delete line");
                self.replace_range(range, "");
                self.notify_change(runtime).await?;
                let target_line = line.min(self.current_buffer().len());
                self.vtop = self.vtop.min(target_line);
                self.cy = target_line.saturating_sub(self.vtop);
                self.cx = 0;
                self.commit_transaction(self.cursor_snapshot());
                self.render(buffer)?;
            }
            Action::Undo => {
                self.undo_transaction(buffer, runtime).await?;
            }
            Action::Redo => {
                self.redo_transaction(buffer, runtime).await?;
            }
            Action::InsertLineAt(y, contents) => {
                if let Some(contents) = contents {
                    self.begin_transaction("insert line");
                    self.replace_range(
                        TextRange::insertion(TextPosition::new(*y, 0)),
                        &format!("{}\n", contents),
                    );
                    self.commit_transaction(self.cursor_snapshot());
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
                use crate::log;

                let leading_spaces = self.current_line_indentation();
                let line = self.buffer_line();

                log!(
                    "InsertLineBelowCursor - line: {}, leading_spaces: {}, current cx: {}, cy: {}",
                    line,
                    leading_spaces,
                    self.cx,
                    self.cy
                );

                // Log current line content
                if let Some(line_content) = self.current_buffer().get(line) {
                    log!("Current line content: {:?}", line_content);
                    log!("Line char count: {}", line_content.chars().count());
                }

                let started_transaction = !self.transaction_active();
                if started_transaction {
                    self.begin_transaction("insert line below");
                }
                self.replace_range(
                    TextRange::insertion(TextPosition::new(line + 1, 0)),
                    &format!("{}\n", " ".repeat(leading_spaces)),
                );
                self.notify_change(runtime).await?;
                self.cy += 1;
                self.cx = leading_spaces;
                self.mode = Mode::Insert;

                if self.cy >= self.vheight() {
                    self.vtop += 1;
                    self.cy -= 1;
                }

                self.render(buffer)?;
            }
            Action::InsertLineAtCursor => {
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
                let started_transaction = !self.transaction_active();
                if started_transaction {
                    self.begin_transaction("insert line above");
                }
                self.replace_range(
                    TextRange::insertion(TextPosition::new(line, 0)),
                    &format!("{}\n", " ".repeat(leading_spaces)),
                );
                self.notify_change(runtime).await?;
                self.cx = leading_spaces;
                self.mode = Mode::Insert;
                self.render(buffer)?;
            }
            Action::MoveToTop => {
                self.vtop = 0;
                self.cy = 0;
                self.render(buffer)?;
            }
            Action::MoveToBottom => {
                let last_line = self.last_navigable_line();
                let line_count = last_line + 1;
                if line_count > self.vheight() {
                    self.cy = self.vheight() - 1;
                    self.vtop = line_count - self.vheight();
                    self.render(buffer)?;
                } else {
                    self.cy = last_line;
                }
            }
            Action::DeleteLineAt(y) => {
                let end = if *y < self.current_buffer().len() {
                    TextPosition::new(*y + 1, 0)
                } else {
                    TextPosition::new(*y, self.length_for_line(*y))
                };
                self.begin_transaction("delete line");
                self.replace_range(TextRange::new(TextPosition::new(*y, 0), end), "");
                self.commit_transaction(self.cursor_snapshot());
                self.notify_change(runtime).await?;
                self.render(buffer)?;
            }
            Action::DeletePreviousChar => {
                if self.cx == 0 && self.buffer_line() == 0 {
                    return Ok(false);
                }

                let started_transaction = !self.transaction_active();
                if started_transaction {
                    self.begin_transaction("delete previous char");
                }

                if self.cx > 0 {
                    // Get the current line to find the previous grapheme boundary
                    if let Some(line) = self.current_line_contents() {
                        let line = line.trim_end_matches('\n');
                        let current_byte = grapheme_to_byte(line, self.cx);

                        if let Some(prev_byte) =
                            crate::unicode_utils::prev_grapheme_boundary(line, current_byte)
                        {
                            let prev_grapheme_idx =
                                crate::unicode_utils::byte_to_grapheme(line, prev_byte);
                            let start_char = crate::unicode_utils::byte_to_char(line, prev_byte);
                            let end_char = crate::unicode_utils::byte_to_char(line, current_byte);
                            let line_num = self.buffer_line();
                            self.replace_range(
                                TextRange::new(
                                    TextPosition::new(line_num, start_char),
                                    TextPosition::new(line_num, end_char),
                                ),
                                "",
                            );
                            self.cx = prev_grapheme_idx;

                            self.notify_change(runtime).await?;
                            self.draw_line(buffer);
                        }
                    }
                } else if self.buffer_line() > 0 {
                    let line_num = self.buffer_line();
                    let previous_line = self.current_buffer().get(line_num - 1).unwrap_or_default();
                    let previous_line = previous_line.trim_end_matches('\n');
                    let previous_char_len = previous_line.chars().count();
                    let previous_grapheme_len = grapheme_len(previous_line);

                    self.replace_range(
                        TextRange::new(
                            TextPosition::new(line_num - 1, previous_char_len),
                            TextPosition::new(line_num, 0),
                        ),
                        "",
                    );
                    self.cx = previous_grapheme_len;
                    if self.cy > 0 {
                        self.cy -= 1;
                    } else {
                        self.vtop = self.vtop.saturating_sub(1);
                    }

                    self.notify_change(runtime).await?;
                    self.render(buffer)?;
                }

                if started_transaction {
                    self.commit_transaction(self.cursor_snapshot());
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
                self.lsp.workspace_symbol("").await?;
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
                            let new_buffer =
                                match Buffer::load_or_create(Some(log_file.to_string())).await {
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
                    self.ensure_current_buffer_lsp_opened().await?;
                    self.lsp
                        .goto_definition(&file, self.cx, self.cy + self.vtop)
                        .await?;
                }
            }
            Action::Hover => {
                if let Some(file) = self.current_buffer().file.clone() {
                    self.ensure_current_buffer_lsp_opened().await?;
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
                let target_y = (*y).min(self.last_navigable_line());

                if target_y < self.vtop {
                    self.vtop = target_y;
                } else if target_y >= self.vtop + self.vheight().max(1) {
                    self.vtop = target_y.saturating_sub(self.vheight().saturating_sub(1));
                }

                self.cy = target_y.saturating_sub(self.vtop);
                self.cx = *x;
                self.check_bounds();
                self.ensure_current_buffer_lsp_opened().await?;
                self.render(buffer)?;
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
                let line = self.buffer_line();
                let char_cx = self.next_word_search_char_on_line(self.cx, line);
                let next_word = self.current_buffer().find_next_word((char_cx, line));

                if let Some((x, y)) = next_word {
                    self.cx = self.char_to_grapheme_on_line(x, y);
                    if self.is_within_viewport(y) {
                        self.cy = y - self.vtop;
                    } else {
                        self.go_to_line(y + 1, buffer, runtime, GoToLinePosition::Top)
                            .await?;
                    }
                    self.draw_cursor()?;
                }
            }
            Action::MoveToPreviousWord => {
                let line = self.buffer_line();
                let char_cx = self.grapheme_to_char_on_line(self.cx, line);
                let previous_word = self.current_buffer().find_prev_word((char_cx, line));

                if let Some((x, y)) = previous_word {
                    self.cx = self.char_to_grapheme_on_line(x, y);
                    if self.is_within_viewport(y) {
                        self.cy = y - self.vtop;
                    } else {
                        self.go_to_line(y + 1, buffer, runtime, GoToLinePosition::Top)
                            .await?;
                    }
                    self.draw_cursor()?;
                }
            }
            Action::MoveLineToViewportBottom => {
                let line = self.buffer_line();
                if line > self.vtop + self.vheight() {
                    self.vtop = line.saturating_sub(self.vheight().saturating_sub(1));
                    self.cy = self.vheight() - 1;
                    self.render(buffer)?;
                }
            }
            Action::InsertTab => {
                // TODO: Tab configuration
                let tabsize = 4;
                let cx = self.cx;
                let line = self.buffer_line();
                let char_cx = self.grapheme_to_char_on_line(cx, line);
                let started_transaction = !self.transaction_active();
                if started_transaction {
                    self.begin_transaction("insert tab");
                }
                self.replace_range(
                    TextRange::insertion(TextPosition::new(line, char_cx)),
                    &" ".repeat(tabsize),
                );
                self.notify_change(runtime).await?;
                self.cx += tabsize;
                if started_transaction {
                    self.commit_transaction(self.cursor_snapshot());
                }
                self.draw_line(buffer);
            }
            Action::Save => {
                let resume_insert_transaction = self.commit_active_transaction_before_save();
                let save_result = self.current_buffer_mut().save();
                self.resume_insert_transaction_after_save(resume_insert_transaction);

                match save_result {
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
                }
            }
            Action::SaveAs(new_file_name) => {
                let resume_insert_transaction = self.commit_active_transaction_before_save();
                let save_result = self.current_buffer_mut().save_as(new_file_name);
                self.resume_insert_transaction_after_save(resume_insert_transaction);

                match save_result {
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
                let char_cx = self.grapheme_to_char_on_line(cx, line);

                if let Some((end_x, end_y)) = self.current_buffer().find_next_word((char_cx, line))
                {
                    self.begin_transaction("delete word");
                    self.replace_range(
                        TextRange::new(
                            TextPosition::new(line, char_cx),
                            TextPosition::new(end_y, end_x),
                        ),
                        "",
                    );
                    self.commit_transaction(self.cursor_snapshot());
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
            Action::DeleteBuffer(force) => {
                self.delete_current_buffer(buffer, *force).await?;
            }
            Action::OpenFile(path) => {
                if let Some(index) = self.buffers.iter().position(|b| b.name() == *path) {
                    self.set_current_buffer(buffer, index).await?;
                } else {
                    let new_buffer = match Buffer::load_or_create(Some(path.to_string())).await {
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
                self.request_diagnostics().await?;
                self.render(buffer)?;
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
            Action::OpenLivePicker(title, items, id, initial_selection) => {
                self.current_dialog = Some(Box::new(Picker::new_live(
                    title.clone(),
                    self,
                    items,
                    *id,
                    initial_selection.as_deref(),
                )));
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
            Action::PreviewTheme(theme_name) => {
                if let Err(err) = self.apply_theme(theme_name, false) {
                    self.last_error = Some(err.to_string());
                }
                self.render(buffer)?;
            }
            Action::SetTheme(theme_name) => {
                if let Err(err) = self.apply_theme(theme_name, true) {
                    self.last_error = Some(err.to_string());
                }
                self.render(buffer)?;
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
                    self.begin_transaction("delete selection");
                    if let Some((x0, y0)) = self.delete_selection() {
                        self.cx = x0;
                        self.cy = y0 - self.vtop;
                    }
                    self.commit_transaction(self.cursor_snapshot());
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
                self.insert_content_as_transaction(*x, *y, content);
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
                let char_cx = self.grapheme_to_char_on_line(cx, line);
                let started_transaction = !self.transaction_active();
                if started_transaction {
                    self.begin_transaction("insert string");
                }
                self.replace_range(TextRange::insertion(TextPosition::new(line, char_cx)), text);
                self.notify_change(runtime).await?;
                self.cx += grapheme_len(text);
                if started_transaction {
                    self.commit_transaction(self.cursor_snapshot());
                }
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

                self.begin_transaction("indent line");
                self.replace_range(
                    TextRange::insertion(TextPosition::new(line, 0)),
                    &" ".repeat(indent.shift_width),
                );
                self.commit_transaction(self.cursor_snapshot());
                self.notify_change(runtime).await?;
                self.render(buffer)?;
            }
            Action::UnindentLine => {
                let spaces = self.current_line_indentation();
                let chars_to_remove = std::cmp::min(spaces, self.indentation().shift_width);
                let line = self.buffer_line();

                self.begin_transaction("unindent line");
                self.replace_range(
                    TextRange::new(
                        TextPosition::new(line, 0),
                        TextPosition::new(line, chars_to_remove),
                    ),
                    "",
                );
                self.commit_transaction(self.cursor_snapshot());
                self.notify_change(runtime).await?;
                self.render(buffer)?;
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
                if self.update_window_layout(|windows| windows.split_horizontal(current_buffer)) {
                    log!("Window split successful");
                    self.render(buffer)?;
                } else {
                    log!("Window split failed");
                }
            }
            Action::SplitVertical => {
                log!("SplitVertical action triggered");
                let current_buffer = self.current_buffer_index;
                if self.update_window_layout(|windows| windows.split_vertical(current_buffer)) {
                    log!("Vertical split successful");
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
                match Buffer::load_or_create(Some(file.clone())).await {
                    Ok(new_buffer) => {
                        self.buffers.push(new_buffer);
                        let new_buffer_index = self.buffers.len() - 1;
                        if self.update_window_layout(|windows| {
                            windows.split_horizontal(new_buffer_index)
                        }) {
                            log!("Window split with new file successful");
                            self.request_diagnostics().await?;
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
                match Buffer::load_or_create(Some(file.clone())).await {
                    Ok(new_buffer) => {
                        self.buffers.push(new_buffer);
                        let new_buffer_index = self.buffers.len() - 1;
                        if self.update_window_layout(|windows| {
                            windows.split_vertical(new_buffer_index)
                        }) {
                            log!("Vertical split with new file successful");
                            self.request_diagnostics().await?;
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
                if self.update_window_layout(WindowManager::close_window) {
                    self.render(buffer)?;
                }
            }
            Action::NextWindow => {
                let window_count = self.window_manager.windows().len();
                if window_count > 1 {
                    let next_id = (self.window_manager.active_window_id() + 1) % window_count;
                    if self.set_active_window(next_id) {
                        self.request_diagnostics().await?;
                        self.render(buffer)?;
                    }
                }
            }
            Action::PreviousWindow => {
                let window_count = self.window_manager.windows().len();
                if window_count > 1 {
                    let current_id = self.window_manager.active_window_id();
                    let prev_id = if current_id == 0 {
                        window_count - 1
                    } else {
                        current_id - 1
                    };
                    if self.set_active_window(prev_id) {
                        self.request_diagnostics().await?;
                        self.render(buffer)?;
                    }
                }
            }
            Action::MoveWindowUp => {
                if let Some(target_id) = self
                    .window_manager
                    .find_window_in_direction(crate::window::Direction::Up)
                {
                    if self.set_active_window(target_id) {
                        self.request_diagnostics().await?;
                        self.render(buffer)?;
                    }
                }
            }
            Action::MoveWindowDown => {
                if let Some(target_id) = self
                    .window_manager
                    .find_window_in_direction(crate::window::Direction::Down)
                {
                    if self.set_active_window(target_id) {
                        self.request_diagnostics().await?;
                        self.render(buffer)?;
                    }
                }
            }
            Action::MoveWindowLeft => {
                if let Some(target_id) = self
                    .window_manager
                    .find_window_in_direction(crate::window::Direction::Left)
                {
                    if self.set_active_window(target_id) {
                        self.request_diagnostics().await?;
                        self.render(buffer)?;
                    }
                }
            }
            Action::MoveWindowRight => {
                if let Some(target_id) = self
                    .window_manager
                    .find_window_in_direction(crate::window::Direction::Right)
                {
                    if self.set_active_window(target_id) {
                        self.request_diagnostics().await?;
                        self.render(buffer)?;
                    }
                }
            }
            Action::ResizeWindowUp(amount) => {
                if self.update_window_layout(|windows| {
                    windows.resize_window(crate::window::Direction::Up, *amount)
                }) {
                    self.render(buffer)?;
                }
            }
            Action::ResizeWindowDown(amount) => {
                if self.update_window_layout(|windows| {
                    windows.resize_window(crate::window::Direction::Down, *amount)
                }) {
                    self.render(buffer)?;
                }
            }
            Action::ResizeWindowLeft(amount) => {
                if self.update_window_layout(|windows| {
                    windows.resize_window(crate::window::Direction::Left, *amount)
                }) {
                    self.render(buffer)?;
                }
            }
            Action::ResizeWindowRight(amount) => {
                if self.update_window_layout(|windows| {
                    windows.resize_window(crate::window::Direction::Right, *amount)
                }) {
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

        if self.check_bounds() {
            self.render(buffer)?;
        }

        if self.is_visual()
            && matches!(
                action,
                Action::MoveUp
                    | Action::MoveDown
                    | Action::MoveLeft
                    | Action::MoveRight
                    | Action::MoveToLineEnd
                    | Action::MoveToLineStart
                    | Action::MoveToFirstLineChar
                    | Action::MoveToLastLineChar
                    | Action::MoveToTop
                    | Action::MoveToBottom
                    | Action::GoToLine(_)
                    | Action::MoveTo(_, _)
            )
        {
            self.update_selection();
            self.render(buffer)?;
        }

        if add_to_history {
            self.save_to_history(action);
        }

        // Sync editor state back to the active window after executing actions
        // This ensures window state is updated even for actions that don't trigger a full render
        self.sync_to_window();

        // Always render after actions when in multi-window mode to ensure changes are visible
        if self.window_manager.windows().len() > 1 {
            self.render(buffer)?;
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
        let text_len = text.chars().count();
        let (x, text) = if overflow + 3 >= text_len {
            (x, text.to_string())
        } else {
            (0, format!("...{}", char_suffix(text, overflow)))
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

                match self.mode {
                    Mode::VisualLine => {
                        let end = if y1 < self.current_buffer().len() {
                            TextPosition::new(y1 + 1, 0)
                        } else {
                            TextPosition::new(y1, self.length_for_line(y1))
                        };
                        self.replace_range(TextRange::new(TextPosition::new(y0, 0), end), "");
                    }
                    Mode::VisualBlock => {
                        let min_x = std::cmp::min(x0, x1);
                        let max_x = std::cmp::max(x0, x1);

                        for y in y0..=y1 {
                            if let Some(line) = self.current_buffer().get(y) {
                                let line = line.trim_end_matches('\n');
                                let line_len = grapheme_len(line);
                                if min_x >= line_len {
                                    continue;
                                }
                                let start = self.grapheme_to_char_on_line(min_x, y);
                                let end =
                                    self.grapheme_to_char_on_line((max_x + 1).min(line_len), y);
                                self.replace_range(
                                    TextRange::new(
                                        TextPosition::new(y, start),
                                        TextPosition::new(y, end),
                                    ),
                                    "",
                                );
                            }
                        }
                    }
                    Mode::Visual => {
                        if y0 == y1 {
                            let start = self.grapheme_to_char_on_line(x0, y0);
                            let end = self.grapheme_to_char_on_line(x1 + 1, y0);
                            self.replace_range(
                                TextRange::new(
                                    TextPosition::new(y0, start),
                                    TextPosition::new(y0, end),
                                ),
                                "",
                            );
                        } else {
                            let start = self.grapheme_to_char_on_line(x0, y0);
                            let end = self.grapheme_to_char_on_line(x1 + 1, y1);
                            self.replace_range(
                                TextRange::new(
                                    TextPosition::new(y0, start),
                                    TextPosition::new(y1, end),
                                ),
                                "",
                            );
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
        let started_transaction = !self.transaction_active();
        if started_transaction {
            self.begin_transaction("paste");
        }
        self.insert_content(self.cx, self.buffer_line(), content, before);
        if started_transaction {
            self.commit_transaction(self.cursor_snapshot());
        }
    }

    fn insert_content(&mut self, x: usize, y: usize, content: &Content, before: bool) {
        match content.kind {
            ContentKind::Charwise => self.insert_charwise(x, y, content, before),
            ContentKind::Linewise => self.insert_linewise(y, content, before),
            ContentKind::Blockwise => self.insert_blockwise(x, y, content, before),
        }
    }

    fn insert_linewise(&mut self, y: usize, contents: &Content, before: bool) {
        let target_y = y + if before { 0 } else { 1 };
        let mut text = String::new();
        for line in contents.text.lines() {
            text.push_str(line);
            text.push('\n');
        }
        self.replace_range(TextRange::insertion(TextPosition::new(target_y, 0)), &text);
    }

    fn insert_blockwise(&mut self, x: usize, y: usize, contents: &Content, before: bool) {
        let lines: Vec<&str> = contents.text.lines().collect();
        let paste_x = if before { x } else { x + 1 };

        for (dy, line) in lines.iter().enumerate() {
            let y = y + dy;
            // Extend the buffer with empty lines if needed
            while self.current_buffer().len() <= y {
                self.replace_range(TextRange::insertion(TextPosition::new(y, 0)), "\n");
            }

            let current_line = self.current_buffer().get(y).unwrap_or_default();
            let current_line = current_line.trim_end_matches('\n');
            let mut new_line = current_line.to_string();

            // Extend the line with spaces if needed
            while grapheme_len(&new_line) < paste_x {
                new_line.push(' ');
            }

            // Insert the block text
            let paste_byte = grapheme_to_byte(&new_line, paste_x);
            new_line.insert_str(paste_byte, line);
            self.replace_range(
                TextRange::new(
                    TextPosition::new(y, 0),
                    TextPosition::new(y, current_line.chars().count()),
                ),
                &new_line,
            );
        }
    }

    fn insert_charwise(&mut self, x: usize, y: usize, contents: &Content, before: bool) {
        let insert_x = self.grapheme_to_char_on_line(x, y);
        let insertion = if before {
            insert_x
        } else {
            let after_x = self.grapheme_to_char_on_line(x + 1, y);
            self.cx += 1;
            after_x
        };
        self.replace_range(
            TextRange::insertion(TextPosition::new(y, insertion)),
            &contents.text,
        );
    }

    fn insert_content_as_transaction(&mut self, x: usize, y: usize, content: &Content) {
        let started_transaction = !self.transaction_active();
        if started_transaction {
            self.begin_transaction("insert text");
        }
        self.insert_content(x, y, content, true);
        if started_transaction {
            self.commit_transaction(self.cursor_snapshot());
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
            self.ensure_current_buffer_lsp_opened().await?;
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

    async fn delete_current_buffer(
        &mut self,
        render_buffer: &mut RenderBuffer,
        force: bool,
    ) -> anyhow::Result<()> {
        if self.current_buffer().is_dirty() && !force {
            self.last_error = Some("No write since last change (add ! to override)".to_string());
            self.render(render_buffer)?;
            return Ok(());
        }

        self.sync_to_window();

        if self.buffers.len() == 1 {
            self.buffers[0] = Buffer::new(None, String::new());
            self.current_buffer_index = 0;
            self.cx = 0;
            self.cy = 0;
            self.vtop = 0;
            self.vleft = 0;
            self.vx = self.gutter_width() + 1;
            self.prev_highlight_y = None;

            for window in self.window_manager.windows_mut() {
                window.buffer_index = 0;
                window.cx = 0;
                window.cy = 0;
                window.vtop = 0;
                window.vleft = 0;
                window.vx = self.vx;
            }

            self.request_diagnostics().await?;
            return self.render(render_buffer);
        }

        let removed_index = self.current_buffer_index;
        let target_old_index = if removed_index + 1 < self.buffers.len() {
            removed_index + 1
        } else {
            removed_index - 1
        };

        self.buffers.remove(removed_index);

        let target_index = if target_old_index > removed_index {
            target_old_index - 1
        } else {
            target_old_index
        };
        self.current_buffer_index = target_index;

        let (target_cx, target_cy) = self.current_buffer().pos;
        let target_vtop = self.current_buffer().vtop;
        let target_vx = self.gutter_width() + 1;

        for window in self.window_manager.windows_mut() {
            if window.buffer_index == removed_index {
                window.buffer_index = target_index;
                window.cx = target_cx;
                window.cy = target_cy;
                window.vtop = target_vtop;
                window.vleft = 0;
                window.vx = target_vx;
            } else if window.buffer_index > removed_index {
                window.buffer_index -= 1;
            }
        }

        self.sync_with_window();
        self.prev_highlight_y = None;
        self.request_diagnostics().await?;
        self.render(render_buffer)
    }

    async fn request_diagnostics(&mut self) -> anyhow::Result<()> {
        if let Some(uri) = self.current_buffer().uri()? {
            self.ensure_current_buffer_lsp_opened().await?;
            self.lsp.request_diagnostics(&uri).await?;
        }
        Ok(())
    }

    async fn ensure_current_buffer_lsp_opened(&mut self) -> anyhow::Result<()> {
        let Some(file) = self.current_buffer().file.clone() else {
            return Ok(());
        };
        let Some(uri) = self.current_buffer().uri()? else {
            return Ok(());
        };
        if self.lsp_opened_documents.contains(&uri) {
            return Ok(());
        }
        let contents = self.current_buffer().contents();
        self.lsp.did_open(&file, &contents).await?;
        self.lsp_opened_documents.insert(uri);
        Ok(())
    }

    fn apply_theme(&mut self, theme_name: &str, update_config: bool) -> anyhow::Result<()> {
        let theme_path = Config::path("themes").join(theme_name);
        if !theme_path.exists() {
            anyhow::bail!("Theme file {} not found", theme_name);
        }

        let theme = parse_vscode_theme(&theme_path.to_string_lossy())?;
        let highlighter = Highlighter::new(&theme)?;
        self.theme = theme;
        self.highlighter = highlighter;
        self.highlight_cache.clear();
        if update_config {
            self.config.theme = theme_name.to_string();
            Config::persist_theme(theme_name)?;
        }
        Ok(())
    }

    async fn go_to_line(
        &mut self,
        line: usize,
        buffer: &mut RenderBuffer,
        _runtime: &mut Runtime,
        pos: GoToLinePosition,
    ) -> anyhow::Result<()> {
        if line == 0 {
            self.vtop = 0;
            self.cy = 0;
            self.render(buffer)?;
            return Ok(());
        }

        let y = line.saturating_sub(1).min(self.last_navigable_line());
        let viewport_height = self.vheight().max(1);

        self.vtop = match pos {
            GoToLinePosition::Top => y,
            GoToLinePosition::Center => y.saturating_sub(viewport_height / 2),
            GoToLinePosition::Bottom => y.saturating_sub(viewport_height.saturating_sub(1)),
        };
        self.cy = y.saturating_sub(self.vtop);
        self.check_bounds();
        self.render(buffer)?;

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
                        let click_x = *column as usize;
                        let click_y = *row as usize;

                        // Check if click is in a window
                        if let Some((window_id, window)) =
                            self.window_manager.window_at_position(click_x, click_y)
                        {
                            // Clone window data to avoid borrowing issues
                            let window = window.clone();
                            let window_buffer_index = window.buffer_index;
                            let window_vtop = window.vtop;

                            // Switch to the clicked window if it's not already active
                            self.set_active_window(window_id);

                            // Convert terminal coordinates to window-local coordinates
                            if let Some((local_x, local_y)) =
                                window.terminal_to_local(click_x, click_y)
                            {
                                // Adjust for the clicked window's gutter, not the active buffer's.
                                let gutter_width =
                                    self.gutter_width_for_buffer_index(window_buffer_index);
                                let buffer_x = local_x.saturating_sub(gutter_width + 1);
                                let buffer_y = window_vtop + local_y;

                                // Ensure y is within buffer bounds
                                let window_buffer = &self.buffers[window_buffer_index];
                                let y = if buffer_y >= window_buffer.len() {
                                    window_buffer.len().saturating_sub(1)
                                } else {
                                    buffer_y
                                };

                                return Some(KeyAction::Single(Action::SetCursor(buffer_x, y)));
                            }
                        }

                        // Fallback to global click handling if not in a window
                        let x = (*column as usize).saturating_sub(self.gutter_width() + 1);
                        let mut y = *row as usize + self.vtop;

                        if y >= self.current_buffer().len() {
                            y = self.current_buffer().len().saturating_sub(1);
                        }

                        Some(KeyAction::Single(Action::SetCursor(x, y)))
                    }
                    MouseEventKind::ScrollUp => {
                        let click_x = *column as usize;
                        let click_y = *row as usize;

                        // Check if scroll is in a window and switch to it
                        if let Some((window_id, _window)) =
                            self.window_manager.window_at_position(click_x, click_y)
                        {
                            self.set_active_window(window_id);
                        }

                        Some(KeyAction::Single(Action::ScrollUp))
                    }
                    MouseEventKind::ScrollDown => {
                        let click_x = *column as usize;
                        let click_y = *row as usize;

                        // Check if scroll is in a window and switch to it
                        if let Some((window_id, _window)) =
                            self.window_manager.window_at_position(click_x, click_y)
                        {
                            self.set_active_window(window_id);
                        }

                        Some(KeyAction::Single(Action::ScrollDown))
                    }
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

    fn cursor_snapshot(&self) -> CursorSnapshot {
        CursorSnapshot::new(self.cx, self.buffer_line(), self.vtop)
    }

    fn restore_cursor_snapshot(&mut self, snapshot: CursorSnapshot) {
        self.vtop = snapshot.vtop;
        self.cy = snapshot.y.saturating_sub(self.vtop);
        self.cx = snapshot.x;
        self.check_bounds();
    }

    fn begin_transaction(&mut self, label: impl Into<String>) {
        let before_cursor = self.cursor_snapshot();
        self.current_buffer_mut()
            .undo_history
            .begin_transaction(label, before_cursor);
    }

    fn transaction_active(&self) -> bool {
        self.current_buffer().undo_history.is_transaction_active()
    }

    fn commit_active_transaction_before_save(&mut self) -> bool {
        let was_active = self.transaction_active();
        if was_active {
            self.commit_transaction(self.cursor_snapshot());
        }
        was_active
    }

    fn resume_insert_transaction_after_save(&mut self, was_active: bool) {
        if was_active && self.is_insert() && !self.transaction_active() {
            self.begin_transaction("insert");
        }
    }

    fn replace_range(&mut self, range: TextRange, new_text: &str) {
        let old_text = self.current_buffer().text_in_range(range);
        if old_text == new_text {
            return;
        }
        self.current_buffer_mut().replace_range_raw(range, new_text);
        self.current_buffer_mut().undo_history.record_replace(
            range,
            old_text,
            new_text.to_string(),
        );
    }

    fn delete_text_range(&mut self, range: TextRange, label: &str) -> bool {
        let deleted_text = self.current_buffer().text_in_range(range);
        self.registers
            .insert(DEFAULT_REGISTER, Content::charwise(deleted_text.clone()));
        self.move_to_text_position(range.start);

        if deleted_text.is_empty() {
            return false;
        }

        self.begin_transaction(label);
        self.replace_range(range, "");
        self.move_to_text_position(range.start);
        self.commit_transaction(self.cursor_snapshot());
        true
    }

    fn move_to_text_position(&mut self, position: TextPosition) {
        let y = position.line.min(self.last_navigable_line());
        if !self.is_within_viewport(y) {
            self.vtop = y;
        }
        self.cy = y.saturating_sub(self.vtop);
        let char_x = position.character.min(self.length_for_line(y));
        self.cx = self.char_to_grapheme_on_line(char_x, y);
    }

    fn select_text_range(&mut self, range: TextRange) -> bool {
        let Some(end_position) = self.previous_text_position(range.end, range.start) else {
            return false;
        };

        let start = self.point_for_text_position(range.start);
        let end = self.point_for_text_position(end_position);

        self.selection_start = Some(start);
        self.set_selection(start, end);
        self.move_to_text_position(end_position);
        true
    }

    fn previous_text_position(
        &self,
        position: TextPosition,
        floor: TextPosition,
    ) -> Option<TextPosition> {
        let current_idx = self.current_buffer().position_to_char_idx(position);
        let floor_idx = self.current_buffer().position_to_char_idx(floor);
        (current_idx > floor_idx).then(|| self.position_for_char_idx(current_idx - 1))
    }

    fn point_for_text_position(&self, position: TextPosition) -> Point {
        Point::new(
            self.char_to_grapheme_on_line(position.character, position.line),
            position.line,
        )
    }

    fn commit_transaction(&mut self, after_cursor: CursorSnapshot) -> bool {
        let committed = self
            .current_buffer_mut()
            .undo_history
            .commit_transaction(after_cursor);
        self.current_buffer_mut().refresh_dirty_from_history();
        committed
    }

    fn cancel_transaction_if_empty(&mut self) {
        self.current_buffer_mut()
            .undo_history
            .cancel_transaction_if_empty();
    }

    async fn undo_transaction(
        &mut self,
        render_buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let buffer = self.current_buffer_mut();
        let mut history = std::mem::take(&mut buffer.undo_history);
        let cursor = history.undo(buffer);
        buffer.undo_history = history;
        buffer.refresh_dirty_from_history();

        if let Some(cursor) = cursor {
            self.restore_cursor_snapshot(cursor);
            self.notify_change(runtime).await?;
            self.render(render_buffer)?;
        }

        Ok(())
    }

    async fn redo_transaction(
        &mut self,
        render_buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let buffer = self.current_buffer_mut();
        let mut history = std::mem::take(&mut buffer.undo_history);
        let cursor = history.redo(buffer);
        buffer.undo_history = history;
        buffer.refresh_dirty_from_history();

        if let Some(cursor) = cursor {
            self.restore_cursor_snapshot(cursor);
            self.notify_change(runtime).await?;
            self.render(render_buffer)?;
        }

        Ok(())
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

    fn editor_state_snapshot(&mut self) -> EditorStateSnapshot {
        self.sync_to_window();
        let cwd = std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_default();
        let saved_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default();

        let mut visible_buffer_positions = HashMap::new();
        for window in self.window_manager.windows() {
            visible_buffer_positions.insert(
                window.buffer_index,
                (window.cx, window.vtop + window.cy, window.vtop),
            );
        }
        if let Some(window) = self.window_manager.active_window() {
            visible_buffer_positions.insert(
                window.buffer_index,
                (window.cx, window.vtop + window.cy, window.vtop),
            );
        }

        let buffers = self
            .buffers
            .iter()
            .enumerate()
            .filter_map(|(index, buffer)| {
                let path = buffer.file.clone()?;
                let (x, y, viewport_top) = visible_buffer_positions
                    .get(&index)
                    .copied()
                    .unwrap_or((buffer.pos.0, buffer.vtop + buffer.pos.1, buffer.vtop));
                Some(BufferStateSnapshot {
                    index,
                    path,
                    dirty: buffer.dirty,
                    cursor: CursorStateSnapshot { x, y },
                    viewport_top,
                })
            })
            .collect();

        EditorStateSnapshot {
            version: 1,
            cwd,
            saved_at,
            buffers,
            current_buffer_index: self.current_buffer_index,
            window_layout: self.window_manager.snapshot(),
        }
    }

    async fn restore_editor_state(
        &mut self,
        snapshot: EditorStateSnapshot,
        render_buffer: &mut RenderBuffer,
    ) -> anyhow::Result<RestoreResult> {
        if snapshot.version != 1 {
            return Ok(RestoreResult {
                restored: false,
                opened_files: Vec::new(),
                skipped_files: Vec::new(),
                warnings: vec![format!(
                    "Unsupported editor state version {}",
                    snapshot.version
                )],
            });
        }

        let mut opened_files = Vec::new();
        let mut skipped_files = Vec::new();
        let mut buffer_map = HashMap::new();
        let mut restored_buffers = Vec::new();

        for saved_buffer in &snapshot.buffers {
            if !std::path::Path::new(&saved_buffer.path).exists() {
                skipped_files.push(SkippedFile {
                    path: saved_buffer.path.clone(),
                    reason: "file does not exist".to_string(),
                });
                continue;
            }

            match Buffer::load_or_create(Some(saved_buffer.path.clone())).await {
                Ok(mut buffer) => {
                    let viewport_top = saved_buffer.viewport_top.min(buffer.last_navigable_line());
                    let cursor_y = saved_buffer.cursor.y.min(buffer.last_navigable_line());
                    let cursor_x = buffer
                        .get(cursor_y)
                        .map(|line| {
                            saved_buffer
                                .cursor
                                .x
                                .min(line.trim_end_matches('\n').chars().count())
                        })
                        .unwrap_or(0);
                    buffer.vtop = viewport_top;
                    buffer.pos = (cursor_x, cursor_y.saturating_sub(viewport_top));

                    buffer_map.insert(saved_buffer.index, restored_buffers.len());
                    opened_files.push(saved_buffer.path.clone());
                    restored_buffers.push(buffer);
                }
                Err(err) => skipped_files.push(SkippedFile {
                    path: saved_buffer.path.clone(),
                    reason: err.to_string(),
                }),
            }
        }

        if restored_buffers.is_empty() {
            return Ok(RestoreResult {
                restored: false,
                opened_files,
                skipped_files,
                warnings: vec!["No saved files could be restored".to_string()],
            });
        }

        self.buffers = restored_buffers;
        self.lsp_opened_documents.clear();
        self.current_buffer_index = buffer_map
            .get(&snapshot.current_buffer_index)
            .copied()
            .unwrap_or(0);

        self.window_manager = WindowManager::from_snapshot(
            &snapshot.window_layout,
            (self.size.0 as usize, self.size.1 as usize),
            &buffer_map,
        )
        .unwrap_or_else(|| {
            WindowManager::new(
                self.current_buffer_index,
                (self.size.0 as usize, self.size.1 as usize),
            )
        });

        if let Some(active_window) = self.window_manager.active_window() {
            self.current_buffer_index = active_window.buffer_index;
        }
        self.sync_with_window();
        self.check_bounds();
        self.request_diagnostics().await?;
        self.render(render_buffer)?;

        Ok(RestoreResult {
            restored: true,
            opened_files,
            skipped_files,
            warnings: Vec::new(),
        })
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
                        let line_len = line.chars().count();
                        let end = std::cmp::min(max_x + 1, line_len);
                        if min_x <= line_len {
                            text.push_str(char_slice(&line, min_x, end));
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
                    let end = if y == y1 {
                        x1
                    } else {
                        line.trim_end_matches('\n')
                            .chars()
                            .count()
                            .saturating_sub(1)
                    };
                    text.push_str(char_slice(&line, start, end + 1));
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
        let max_cursor_x = self.max_cursor_x_for_line_length(self.line_length());

        if self.cx > max_cursor_x {
            self.cx = max_cursor_x;
        }
    }

    fn start_selection(&mut self) {
        let (x, y) = (self.cx, self.buffer_line());
        self.selection_start = Some(Point::new(x, y));
        self.update_selection();
    }

    fn set_selection(&mut self, start: Point, end: Point) {
        self.selection = Some(Rect::new(start.x, start.y, end.x, end.y));
    }

    fn update_selection(&mut self) {
        self.fix_cursor_pos();
        let point = Point::new(self.cx, self.buffer_line());

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorStateSnapshot {
    pub version: u32,
    pub cwd: String,
    pub saved_at: u64,
    pub buffers: Vec<BufferStateSnapshot>,
    pub current_buffer_index: usize,
    pub window_layout: WindowManagerSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BufferStateSnapshot {
    pub index: usize,
    pub path: String,
    pub dirty: bool,
    pub cursor: CursorStateSnapshot,
    pub viewport_top: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorStateSnapshot {
    pub x: usize,
    pub y: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreResult {
    pub restored: bool,
    pub opened_files: Vec<String>,
    pub skipped_files: Vec<SkippedFile>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedFile {
    pub path: String,
    pub reason: String,
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
    path: Option<String>,
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
            path: buffer.file.clone(),
            dirty: buffer.is_dirty(),
        }
    }
}

fn directory_listing(path: &str) -> Value {
    let read_dir = match std::fs::read_dir(path) {
        Ok(read_dir) => read_dir,
        Err(err) => {
            return json!({
                "path": path,
                "entries": [],
                "error": err.to_string(),
            });
        }
    };

    let mut entries = read_dir
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let metadata = entry.metadata().ok()?;
            let kind = if metadata.is_dir() {
                "directory"
            } else if metadata.is_file() {
                "file"
            } else {
                "other"
            };
            Some(json!({
                "name": entry.file_name().to_string_lossy(),
                "path": entry.path().to_string_lossy(),
                "kind": kind,
            }))
        })
        .collect::<Vec<_>>();

    entries.sort_by(|a, b| {
        let kind_rank = |value: &Value| match value.get("kind").and_then(Value::as_str) {
            Some("directory") => 0,
            Some("file") => 1,
            _ => 2,
        };
        let a_name = a.get("name").and_then(Value::as_str).unwrap_or_default();
        let b_name = b.get("name").and_then(Value::as_str).unwrap_or_default();

        kind_rank(a)
            .cmp(&kind_rank(b))
            .then_with(|| a_name.to_lowercase().cmp(&b_name.to_lowercase()))
    });

    json!({
        "path": path,
        "entries": entries,
        "error": null,
    })
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

// These methods are made public for test utilities but hidden from docs.
impl Editor {
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
    pub fn test_buffer_names(&self) -> Vec<String> {
        self.buffers
            .iter()
            .map(|buffer| buffer.name().to_string())
            .collect()
    }

    #[doc(hidden)]
    pub fn test_current_buffer_index(&self) -> usize {
        self.current_buffer_index
    }

    #[doc(hidden)]
    pub async fn test_ensure_current_buffer_lsp_opened(&mut self) -> anyhow::Result<()> {
        self.ensure_current_buffer_lsp_opened().await
    }

    #[doc(hidden)]
    pub fn test_last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
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
    pub fn test_active_window_id(&self) -> usize {
        self.window_manager.active_window_id()
    }

    #[doc(hidden)]
    pub fn test_active_window_bounds(&self) -> Option<(Point, (usize, usize))> {
        self.window_manager
            .active_window()
            .map(|window| (window.position, window.size))
    }

    #[doc(hidden)]
    pub fn test_create_panel(&mut self, id: &str, config: plugin::PanelConfig) {
        self.panel_manager.create_panel(id.to_string(), config);
        self.apply_panel_layout();
        self.sync_with_window();
    }

    #[doc(hidden)]
    pub fn test_render_cursor_position(&self) -> Option<(usize, usize)> {
        self.render_cursor_position()
    }

    #[doc(hidden)]
    pub fn test_set_commandline(&mut self, mode: Mode, text: &str) {
        self.mode = mode;
        match mode {
            Mode::Command => self.command = text.to_string(),
            Mode::Search => self.search_term = text.to_string(),
            _ => {}
        }
    }

    #[doc(hidden)]
    pub fn test_set_last_error(&mut self, message: &str) {
        self.last_error = Some(message.to_string());
    }

    #[doc(hidden)]
    pub fn test_commandline_row(&mut self) -> String {
        let mut render_buffer = RenderBuffer::new(
            self.size.0 as usize,
            self.size.1 as usize,
            &Style::default(),
        );
        self.draw_commandline(&mut render_buffer);

        let y = self.size.1 as usize - 1;
        render_buffer.cells[y * render_buffer.width..(y + 1) * render_buffer.width]
            .iter()
            .map(|cell| cell.c)
            .collect()
    }

    #[doc(hidden)]
    pub fn test_statusline_row(&mut self) -> String {
        let mut render_buffer = RenderBuffer::new(
            self.size.0 as usize,
            self.size.1 as usize,
            &Style::default(),
        );
        self.draw_statusline(&mut render_buffer);

        let y = self.size.1 as usize - 2;
        render_buffer.cells[y * render_buffer.width..(y + 1) * render_buffer.width]
            .iter()
            .map(|cell| cell.c)
            .collect()
    }

    #[doc(hidden)]
    pub fn test_render_row(&mut self, y: usize) -> anyhow::Result<String> {
        let mut render_buffer = RenderBuffer::new(
            self.size.0 as usize,
            self.size.1 as usize,
            &Style::default(),
        );
        self.render(&mut render_buffer)?;

        Ok(
            render_buffer.cells[y * render_buffer.width..(y + 1) * render_buffer.width]
                .iter()
                .map(|cell| cell.c)
                .collect(),
        )
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

    #[doc(hidden)]
    pub fn test_disable_terminal_output(&mut self) {
        self.terminal_output_enabled = false;
    }

    #[doc(hidden)]
    pub async fn test_execute_production_action(&mut self, action: Action) -> anyhow::Result<()> {
        let mut render_buffer = RenderBuffer::new(
            self.size.0 as usize,
            self.size.1 as usize,
            &Style::default(),
        );
        let mut runtime = Runtime::new();
        self.execute(&action, &mut render_buffer, &mut runtime)
            .await?;
        Ok(())
    }

    #[doc(hidden)]
    pub async fn test_execute_event(&mut self, event: event::Event) -> anyhow::Result<()> {
        let mut render_buffer = RenderBuffer::new(
            self.size.0 as usize,
            self.size.1 as usize,
            &Style::default(),
        );
        let mut runtime = Runtime::new();

        if let Some(action) = self.handle_event(&event)? {
            self.handle_key_action(&event, &action, &mut render_buffer, &mut runtime)
                .await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn test_editor(width: usize, height: usize) -> Editor {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(None, "hello".to_string());
        let mut editor =
            Editor::with_size(lsp, width, height, config, Theme::default(), vec![buffer]).unwrap();
        editor.test_disable_terminal_output();
        editor
    }

    fn render_row(buffer: &RenderBuffer, y: usize) -> String {
        buffer.cells[y * buffer.width..(y + 1) * buffer.width]
            .iter()
            .map(|cell| cell.c)
            .collect()
    }

    #[tokio::test]
    async fn enter_command_mode_renders_prompt_immediately() {
        let mut editor = test_editor(20, 5);
        let mut render_buffer = RenderBuffer::new(20, 5, &Style::default());
        let mut runtime = Runtime::new();

        editor
            .execute(
                &Action::EnterMode(Mode::Command),
                &mut render_buffer,
                &mut runtime,
            )
            .await
            .unwrap();

        assert!(render_row(&render_buffer, 4).starts_with(':'));
    }

    #[tokio::test]
    async fn enter_search_mode_renders_prompt_immediately() {
        let mut editor = test_editor(20, 5);
        let mut render_buffer = RenderBuffer::new(20, 5, &Style::default());
        let mut runtime = Runtime::new();

        editor
            .execute(
                &Action::EnterMode(Mode::Search),
                &mut render_buffer,
                &mut runtime,
            )
            .await
            .unwrap();

        assert!(render_row(&render_buffer, 4).starts_with('/'));
    }

    #[test]
    fn focus_changes_repaint_synthetic_cursor_cell() {
        let mut editor = test_editor(20, 5);
        let mut render_buffer = RenderBuffer::new(20, 5, &Style::default());

        editor.render(&mut render_buffer).unwrap();
        let (x, y) = editor.render_cursor_position().unwrap();
        let cursor_index = y * render_buffer.width + x;
        let focused_style = render_buffer.cells[cursor_index].style.clone();

        assert!(editor.is_focused);

        editor
            .handle_focus_event(&Event::FocusLost, &mut render_buffer)
            .unwrap();
        let blurred_style = render_buffer.cells[cursor_index].style.clone();

        assert!(!editor.is_focused);
        assert_ne!(
            focused_style, blurred_style,
            "blur should repaint the real buffer cell instead of leaving the synthetic cursor"
        );

        editor
            .handle_focus_event(&Event::FocusGained, &mut render_buffer)
            .unwrap();
        let refocused_style = render_buffer.cells[cursor_index].style.clone();

        assert!(editor.is_focused);
        assert_eq!(refocused_style, focused_style);
    }

    #[test]
    fn draw_cursor_syncs_window_state_before_render_position() {
        let mut editor = test_editor(20, 5);
        let mut render_buffer = RenderBuffer::new(20, 5, &Style::default());

        editor.render(&mut render_buffer).unwrap();
        let start = editor.render_cursor_position().unwrap();

        editor.cx = 1;
        editor.draw_cursor().unwrap();

        assert_eq!(
            editor.render_cursor_position(),
            Some((start.0 + 1, start.1))
        );
    }

    #[test]
    fn directory_listing_sorts_directories_before_files() {
        let root = std::env::temp_dir().join(format!("red-dir-listing-{}", uuid::Uuid::new_v4()));
        let dir = root.join("src");
        let file = root.join("README.md");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&file, "readme").unwrap();

        let listing = directory_listing(&root.to_string_lossy());
        let entries = listing["entries"].as_array().unwrap();
        assert_eq!(entries[0]["kind"], "directory");
        assert_eq!(entries[0]["name"], "src");
        assert_eq!(entries[1]["kind"], "file");
        assert_eq!(entries[1]["name"], "README.md");

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn style_for_position_prefers_narrower_and_later_spans() {
        let outer_style = Style {
            fg: Some(Color::Rgb { r: 1, g: 1, b: 1 }),
            ..Style::default()
        };
        let inner_style = Style {
            fg: Some(Color::Rgb { r: 2, g: 2, b: 2 }),
            ..Style::default()
        };
        let later_style = Style {
            fg: Some(Color::Rgb { r: 3, g: 3, b: 3 }),
            ..Style::default()
        };
        let style_info = vec![
            StyleInfo {
                start: 0,
                end: 20,
                style: outer_style.clone(),
            },
            StyleInfo {
                start: 5,
                end: 10,
                style: inner_style.clone(),
            },
            StyleInfo {
                start: 5,
                end: 10,
                style: later_style.clone(),
            },
        ];

        let spans = style_info_to_highlight_spans(style_info);
        let mut cursor = StyleCursor::new(&spans);

        assert_eq!(cursor.style_at(2), Some(&outer_style));
        assert_eq!(cursor.style_at(6), Some(&later_style));
    }

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

    #[test]
    fn test_render_buffer_set_text_uses_display_width() {
        let mut buffer = RenderBuffer::new(5, 1, &Style::default());

        buffer.set_text(0, 0, "a👋b", &Style::default());

        let rendered = buffer.cells.iter().map(|cell| cell.c).collect::<String>();
        assert_eq!(rendered, "a👋 b ");
    }

    #[test]
    fn test_render_buffer_set_text_preserves_grapheme_clusters() {
        let mut buffer = RenderBuffer::new(5, 1, &Style::default());

        buffer.set_text(0, 0, "👨‍👩‍👧‍👦x", &Style::default());

        assert_eq!(buffer.cells[0].text, "👨‍👩‍👧‍👦");
        assert_eq!(buffer.cells[1].text, " ");
        assert_eq!(buffer.cells[2].text, "x");
    }

    #[test]
    fn test_render_buffer_set_text_preserves_combining_graphemes() {
        let mut buffer = RenderBuffer::new(3, 1, &Style::default());

        buffer.set_text(0, 0, "e\u{301}x", &Style::default());

        assert_eq!(buffer.cells[0].text, "e\u{301}");
        assert_eq!(buffer.cells[1].text, "x");
    }

    #[test]
    fn test_render_buffer_ignores_width_boundary_writes() {
        let mut buffer = RenderBuffer::new(2, 2, &Style::default());
        let unchanged = buffer.cells.clone();
        let theme = Theme::default();

        buffer.set_char(2, 0, 'x', &Style::default(), &theme);
        buffer.set_text(2, 0, "x", &Style::default());
        buffer._set_char(2, 0, 'x', &Style::default());

        assert_eq!(buffer.cells, unchanged);
    }

    #[test]
    fn test_delete_last_char_handles_multibyte_text() {
        let mut text = "ab👋".to_string();

        Editor::delete_last_char(&mut text);

        assert_eq!(text, "ab");
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
