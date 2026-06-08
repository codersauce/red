mod display_layout;
pub mod render_buffer;
pub mod rendering;

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet, VecDeque},
    fs,
    io::{stdout, Write as _},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::unicode_utils::{
    char_prefix, char_slice, char_suffix, char_to_grapheme, column_to_grapheme, display_width,
    grapheme_len, grapheme_to_byte, grapheme_to_char, grapheme_to_column, next_grapheme_boundary,
    prev_grapheme_boundary,
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
        self, Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    terminal, ExecutableCommand,
};
#[cfg(unix)]
use nix::sys::signal::{self, Signal};
#[cfg(unix)]
use nix::unistd::Pid;
use once_cell::sync::Lazy;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use unicode_segmentation::UnicodeSegmentation;

pub use render_buffer::RenderBuffer;

use crate::{
    buffer::{Buffer, SearchMatch},
    color::Color,
    command,
    config::{Config, KeyAction},
    dispatcher::Dispatcher,
    highlighter::Highlighter,
    log,
    lsp::{
        get_client_capabilities, Command as LspCommand, CompletionResponse, CompletionResponseItem,
        Diagnostic, InboundMessage, InlayHint, InsertTextFormat, LspClient, ParsedNotification,
        ProgressParams, ProgressToken, Range, ResponseMessage, ServerCapabilities,
        TextEdit as LspTextEdit,
    },
    matchit::{self, MatchDirection, MatchMotion},
    plugin::{self, PluginRegistry, Runtime},
    preferences::PreferencesStore,
    theme::{parse_vscode_theme, Style, Theme},
    ui::{CompletionUI, Component, FilePicker, Info, Picker},
    undo::{CursorSnapshot, TextPosition, TextRange},
    utils::{expand_user_path, get_workspace_uri},
    window::{WindowManager, WindowManagerSnapshot},
};

use self::display_layout::{layout_lines, wrap_line_segments, DisplayLayout, LayoutConfig};

pub static ACTION_DISPATCHER: Lazy<Dispatcher<PluginRequest, PluginResponse>> =
    Lazy::new(Dispatcher::new);

pub const DEFAULT_REGISTER: char = '"';
const JUMPLIST_SIZE: usize = 100;
const REPEATED_MOTION_DRAIN_BUDGET_MS: u64 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EditorViewState {
    vtop: usize,
    vleft: usize,
    skipcol: usize,
    cy: usize,
    wrap: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KeySignature {
    code: KeyCode,
    modifiers: KeyModifiers,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessedEvent {
    quit: bool,
    drain_repeated_motion: bool,
    repeat_signature: Option<KeySignature>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventRenderMode {
    Immediate,
    DeferredMotion,
}

fn expanded_path_string(path: &str) -> anyhow::Result<String> {
    Ok(expand_user_path(path)?.to_string_lossy().into_owned())
}

fn plugin_lsp_error(message: &str) -> Value {
    json!({
        "ok": false,
        "error": message,
    })
}

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
    GetViewportLayout {
        request_id: i32,
    },
    InlayHints {
        request_id: i32,
        range: Option<Range>,
    },
    SetDecorations {
        namespace: String,
        decorations: Vec<plugin::Decoration>,
    },
    ClearDecorations {
        namespace: String,
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
    DocumentSymbols {
        request_id: i32,
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
    GetGitStatus {
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SearchDirection {
    Forward,
    Backward,
}

impl SearchDirection {
    fn opposite(self) -> Self {
        match self {
            Self::Forward => Self::Backward,
            Self::Backward => Self::Forward,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum Action {
    Quit(bool),
    Save,
    SaveAs(String),
    EnterMode(Mode),
    EnterSearch(SearchDirection),

    Undo,
    Redo,
    InsertString(String),

    FindNext,
    FindPrevious,
    RepeatSearch,
    RepeatSearchOpposite,
    CommitSearch,
    CancelSearch,
    ClearSearchHighlight,
    SearchWordUnderCursor,

    MoveUp,
    MoveDown,
    MoveLeft,
    MoveRight,
    MoveToLineEnd,
    MoveToLineStart,
    MoveToFirstLineChar,
    MoveToLastLineChar,
    MoveScreenLineUp,
    MoveScreenLineDown,
    MoveToScreenLineEnd,
    MoveToScreenLineStart,
    MoveToScreenLineFirstNonBlank,
    MoveLineToViewportCenter,
    MoveLineToViewportBottom,
    MoveToBottom,
    MoveToTop,
    MoveTo(usize, usize),
    MoveToFilePos(String, usize, usize),
    MoveToNextWord,
    MoveToPreviousWord,
    MoveToFilePercent(usize),
    MatchitForward,
    MatchitBackward,
    MatchitPreviousUnmatched,
    MatchitNextUnmatched,
    MatchitSelectAround,

    PageDown,
    PageUp,
    ScrollUp,
    ScrollDown,
    ScrollViewLeft,
    ScrollViewRight,
    ScrollViewHalfPageLeft,
    ScrollViewHalfPageRight,
    ScrollCursorToViewStart,
    ScrollCursorToViewEnd,
    ToggleWrap,
    SetWrap(bool),

    DeletePreviousChar,
    DeleteCharAtCursorPos,
    DeleteCurrentLine,
    DeleteLineAt(usize),
    DeleteCharAt(usize, usize),
    DeleteRange(usize, usize, usize, usize),
    DeleteTextRange(TextRange),
    ChangeTextRange(TextRange),
    YankTextRange(TextRange),
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
    ReloadFile(bool),

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
    YankCurrentLine,
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
    RequestCompletionWithTrigger(char),
    ApplyCompletion {
        item: Box<CompletionResponseItem>,
        commit_character: Option<char>,
    },
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
    OnlyWindow,
}

#[allow(unused)]
pub enum GoToLinePosition {
    Top,
    Center,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum CursorGoal {
    DisplayCol(usize),
    LineEnd,
}

impl Default for CursorGoal {
    fn default() -> Self {
        Self::DisplayCol(0)
    }
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

    /// Last terminal cursor cell painted into the render buffer.
    last_rendered_cursor_position: Option<(usize, usize)>,

    /// Suppresses per-step repainting while queued repeated motions are drained.
    defer_motion_render: bool,

    /// Tracks whether deferred motion changed the visible viewport.
    deferred_motion_needs_full_render: bool,

    /// Terminal size (width, height)
    size: (u16, u16),

    /// Top line of viewport (for vertical scrolling)
    vtop: usize,

    /// Left column of viewport (for horizontal scrolling)
    vleft: usize,

    /// First skipped display column for wrapped long-line scrolling.
    skipcol: usize,

    /// Whether the active window wraps long lines.
    wrap: bool,

    /// Cursor x position (column)
    cx: usize,

    /// Cursor y position (line)
    cy: usize,

    /// Display-column goal used when moving vertically.
    cursor_goal: CursorGoal,

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

    /// Persistent preferences, including command-line history.
    preferences: PreferencesStore,

    /// Active command-history navigation state.
    command_history_navigation: Option<CommandHistoryNavigation>,

    /// Active command-line file completion state.
    command_completion: Option<CommandCompletionState>,

    /// Current search term
    search_term: String,

    /// Direction of the most recently committed search.
    search_direction: SearchDirection,

    /// Interactive search state while editing / or ?.
    active_search: Option<SearchSession>,

    /// Whether persistent hlsearch rendering has been cleared with :noh.
    search_highlights_suppressed: bool,

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

    /// Cursor locations remembered for CTRL-O/CTRL-I style jumps.
    jump_list: Vec<HistoryEntry>,

    /// Current position in `jump_list`; `jump_list.len()` means the live cursor
    /// is past the newest recorded jump.
    jump_index: usize,

    /// Pending render commands from plugins
    render_commands: VecDeque<RenderCommand>,

    /// Plugin overlay manager
    overlay_manager: plugin::OverlayManager,

    /// Persistent virtual text decorations owned by plugins.
    decoration_manager: plugin::DecorationManager,

    panel_manager: plugin::PanelManager,

    directory_watchers: HashMap<i32, DirectoryWatcher>,

    pending_plugin_document_symbols: HashMap<i64, i32>,
    pending_plugin_inlay_hints: HashMap<i64, i32>,
}

#[derive(Debug, Clone, PartialEq)]
struct HistoryEntry {
    file: Option<String>,
    x: usize,
    y: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandHistoryDirection {
    Previous,
    Next,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandHistoryNavigation {
    prefix: String,
    original: String,
    position: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandCompletionState {
    replacement_start: usize,
    replacement_end: usize,
    candidates: Vec<String>,
    selected: usize,
    needs_leading_space: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandCompletionContext {
    replacement_start: usize,
    replacement_end: usize,
    fragment: String,
    needs_leading_space: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionDirection {
    Next,
    Previous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PathCompletionCandidate {
    replacement: String,
    is_dir: bool,
}

#[derive(Debug, Clone)]
struct SearchSession {
    origin: HistoryEntry,
    origin_vtop: usize,
    direction: SearchDirection,
    draft: String,
    preview: Option<SearchMatch>,
}

#[derive(Debug, Clone, Copy)]
struct EditorEventSnapshot {
    mode: Mode,
    cx: usize,
    y: usize,
    vtop: usize,
    vleft: usize,
    skipcol: usize,
    wrap: bool,
    width: usize,
    height: usize,
    buffer_index: usize,
}

impl HistoryEntry {
    fn new(file: Option<String>, x: usize, y: usize) -> Self {
        Self { file, x, y }
    }

    fn same_location(&self, other: &Self) -> bool {
        self.file == other.file && self.x == other.x && self.y == other.y
    }

    fn moved_from(&self, other: &Self) -> bool {
        !self.same_location(other)
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
    Yank,
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
            EditOperator::Yank => 'y',
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
    fn is_waiting_for_key_sequence(&self) -> bool {
        self.waiting_key_action.is_some()
            || self.pending_operator.is_some()
            || self.pending_visual_text_object_scope.is_some()
            || self.repeater.is_some()
    }

    #[allow(unused)]
    pub fn with_size(
        lsp: Box<dyn LspClient>,
        width: usize,
        height: usize,
        config: Config,
        theme: Theme,
        buffers: Vec<Buffer>,
    ) -> anyhow::Result<Self> {
        Self::with_size_and_preferences(
            lsp,
            width,
            height,
            config,
            theme,
            buffers,
            PreferencesStore::in_memory(),
        )
    }

    #[allow(unused)]
    pub fn with_size_and_preferences(
        lsp: Box<dyn LspClient>,
        width: usize,
        height: usize,
        config: Config,
        theme: Theme,
        buffers: Vec<Buffer>,
        preferences: PreferencesStore,
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

        let mut window_manager = WindowManager::new(0, (width, height));
        let wrap = config.wrap.unwrap_or(true);
        for window in window_manager.windows_mut() {
            window.wrap = wrap;
        }
        let completion_ui = CompletionUI::with_theme(&theme);

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
            last_rendered_cursor_position: None,
            defer_motion_render: false,
            deferred_motion_needs_full_render: false,
            size,
            vtop: 0,
            vleft: 0,
            skipcol: 0,
            wrap,
            cx: 0,
            cy: 0,
            cursor_goal: CursorGoal::default(),
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
            preferences,
            command_history_navigation: None,
            command_completion: None,
            search_term: String::new(),
            search_direction: SearchDirection::Forward,
            active_search: None,
            search_highlights_suppressed: false,
            last_error: None,
            current_dialog: None,
            repeater: None,
            selection_start: None,
            selection: None,
            registers: HashMap::new(),
            diagnostics: HashMap::new(),
            completion_ui,
            indentation,
            jump_list: Vec::new(),
            jump_index: 0,
            render_commands: VecDeque::new(),
            overlay_manager: plugin::OverlayManager::new(),
            decoration_manager: plugin::DecorationManager::default(),
            panel_manager: plugin::PanelManager::default(),
            directory_watchers: HashMap::new(),
            pending_plugin_document_symbols: HashMap::new(),
            pending_plugin_inlay_hints: HashMap::new(),
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
        Self::new_with_preferences(lsp, config, theme, buffers, PreferencesStore::in_memory())
    }

    pub fn new_with_preferences(
        lsp: Box<dyn LspClient>,
        config: Config,
        theme: Theme,
        buffers: Vec<Buffer>,
        preferences: PreferencesStore,
    ) -> anyhow::Result<Self> {
        let size = terminal::size()?;
        Self::with_size_and_preferences(
            lsp,
            size.0 as usize,
            size.1 as usize,
            config,
            theme,
            buffers,
            preferences,
        )
    }

    /// Synchronizes the editor's state with the active window
    fn sync_with_window(&mut self) {
        if let Some(window) = self.window_manager.active_window() {
            self.current_buffer_index = window.buffer_index;
            self.vtop = window.vtop;
            self.vleft = window.vleft;
            self.skipcol = window.skipcol;
            self.wrap = window.wrap;
            self.cx = window.cx;
            self.cy = window.cy;
            self.cursor_goal = window.cursor_goal;
            self.vx = window.vx;
        }
    }

    /// Synchronizes the active window with the editor's state
    fn sync_to_window(&mut self) {
        if let Some(window) = self.window_manager.active_window_mut() {
            window.buffer_index = self.current_buffer_index;
            window.vtop = self.vtop;
            window.vleft = self.vleft;
            window.skipcol = self.skipcol;
            window.wrap = self.wrap;
            window.cx = self.cx;
            window.cy = self.cy;
            window.cursor_goal = self.cursor_goal;
            window.vx = self.vx;
        }
    }

    fn editor_view_state(&self) -> EditorViewState {
        EditorViewState {
            vtop: self.vtop,
            vleft: self.vleft,
            skipcol: self.skipcol,
            cy: self.cy,
            wrap: self.wrap,
        }
    }

    fn active_window_with_editor_view(&self) -> Option<crate::window::Window> {
        let mut window = self.window_manager.active_window()?.clone();
        window.buffer_index = self.current_buffer_index;
        window.vtop = self.vtop;
        window.vleft = self.vleft;
        window.skipcol = self.skipcol;
        window.wrap = self.wrap;
        window.cx = self.cx;
        window.cy = self.cy;
        window.cursor_goal = self.cursor_goal;
        window.vx = self.vx;
        Some(window)
    }

    fn finish_cursor_motion(
        &mut self,
        buffer: &mut RenderBuffer,
        preserve_cursor_goal: bool,
    ) -> anyhow::Result<()> {
        let before = self.editor_view_state();
        if !preserve_cursor_goal {
            self.refresh_cursor_goal();
        }
        self.fix_cursor_pos();
        self.sync_to_window();

        if self.defer_motion_render {
            if self.editor_view_state() != before {
                self.deferred_motion_needs_full_render = true;
            }
            return Ok(());
        }

        if self.editor_view_state() != before {
            self.render(buffer)
        } else if self.can_render_cursor_motion_delta() {
            self.render_cursor_motion_delta(buffer)
        } else if self.uses_synthetic_block_cursor() {
            self.render_motion_frame(buffer)
        } else {
            self.draw_cursor_preserving_cursor_goal()
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
        let line = self.buffers.get(window.buffer_index)?.get(buf_y)?;
        let display_col = grapheme_to_column(line.trim_end_matches('\n'), buf_x);
        let layout = self.layout_for_window(window);
        let segment = layout.segment_for_cursor(buf_y, display_col)?;
        Some((
            self.gutter_width_for_window(window)
                + 1
                + segment
                    .screen_col_for_display_col(display_col, self.window_content_width(window)),
            segment.row,
        ))
    }

    /// Get the effective viewport width for a window
    pub fn window_vwidth(&self, window: &crate::window::Window) -> usize {
        window.inner_width()
    }

    /// Get the effective viewport height for a window
    pub fn window_vheight(&self, window: &crate::window::Window) -> usize {
        window.inner_height()
    }

    fn window_content_width(&self, window: &crate::window::Window) -> usize {
        let gutter_width = self.gutter_width_for_window(window);
        window.inner_width().saturating_sub(gutter_width + 1)
    }

    fn layout_for_window(&self, window: &crate::window::Window) -> DisplayLayout {
        let Some(buffer) = self.buffers.get(window.buffer_index) else {
            return DisplayLayout {
                rows: Vec::new(),
                text: String::new(),
            };
        };
        let mut line_count = buffer.navigable_line_count();
        if window.active && self.is_insert() {
            line_count = line_count.max(self.buffer_line() + 1);
        }
        let end = window
            .vtop
            .saturating_add(window.inner_height())
            .min(line_count);
        let lines = (window.vtop..end)
            .filter_map(|line| buffer.get(line))
            .collect::<Vec<_>>();

        layout_lines(
            &lines,
            line_count,
            LayoutConfig {
                content_width: self.window_content_width(window),
                height: window.inner_height(),
                wrap: window.wrap,
                vtop: window.vtop,
                vleft: window.vleft,
                skipcol: window.skipcol,
            },
        )
    }

    fn plugin_viewport_layout_payload(&self) -> Value {
        let Some(window) = self.active_window_with_editor_view() else {
            return json!({
                "bufferIndex": self.current_buffer_index,
                "buffer_index": self.current_buffer_index,
                "windowId": self.window_manager.active_window_id(),
                "window_id": self.window_manager.active_window_id(),
                "rows": [],
            });
        };
        let layout = self.layout_for_window(&window);
        let buffer = &self.buffers[window.buffer_index];
        let gutter_width = self.gutter_width_for_window(&window);
        let content_start = gutter_width + 1;
        let content_width = self.window_content_width(&window);
        let indentation = self.indentation();
        let rows = layout
            .rows
            .iter()
            .map(|segment| {
                let text = buffer
                    .get(segment.line)
                    .unwrap_or_default()
                    .trim_end_matches('\n')
                    .to_string();
                json!({
                    "screenRow": segment.row,
                    "screen_row": segment.row,
                    "line": segment.line,
                    "startCol": segment.start_col,
                    "start_col": segment.start_col,
                    "endCol": segment.end_col,
                    "end_col": segment.end_col,
                    "startGrapheme": segment.start_grapheme,
                    "start_grapheme": segment.start_grapheme,
                    "endGrapheme": segment.end_grapheme,
                    "end_grapheme": segment.end_grapheme,
                    "firstSegment": segment.first_segment,
                    "first_segment": segment.first_segment,
                    "text": text,
                })
            })
            .collect::<Vec<_>>();

        json!({
            "bufferIndex": window.buffer_index,
            "buffer_index": window.buffer_index,
            "windowId": self.window_manager.active_window_id(),
            "window_id": self.window_manager.active_window_id(),
            "width": window.inner_width(),
            "height": window.inner_height(),
            "contentStart": content_start,
            "content_start": content_start,
            "contentWidth": content_width,
            "content_width": content_width,
            "vtop": window.vtop,
            "vleft": window.vleft,
            "skipcol": window.skipcol,
            "wrap": window.wrap,
            "cursor": {
                "x": window.cx,
                "y": window.vtop + window.cy,
                "screenRow": window.cy,
                "screen_row": window.cy,
            },
            "indentation": {
                "shiftWidth": indentation.shift_width,
                "shift_width": indentation.shift_width,
                "tabWidth": indentation.shift_width,
                "tab_width": indentation.shift_width,
            },
            "lineCount": buffer.navigable_line_count(),
            "line_count": buffer.navigable_line_count(),
            "rows": rows,
        })
    }

    fn active_content_width(&self) -> usize {
        self.window_manager
            .active_window()
            .map(|window| self.window_content_width(window))
            .unwrap_or_else(|| self.vwidth().saturating_sub(self.gutter_width() + 1))
    }

    fn sidescroll(&self) -> usize {
        self.config.sidescroll.unwrap_or(1).max(1)
    }

    fn sidescrolloff(&self, width: usize) -> usize {
        self.config
            .sidescrolloff
            .unwrap_or(0)
            .min(width.saturating_sub(1))
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
            self.active_search_text().unwrap_or(&self.search_term)
        }
    }

    fn current_cursor_display_col(&self) -> usize {
        if let Some(line) = self.current_line_contents() {
            return grapheme_to_column(line.trim_end_matches('\n'), self.cx);
        }

        self.cx
    }

    fn refresh_cursor_goal(&mut self) {
        self.cursor_goal = CursorGoal::DisplayCol(self.current_cursor_display_col());
    }

    fn line_goal_limit(&self, line: &str) -> usize {
        let line_width = display_width(line);
        if self.is_insert() {
            line_width
        } else {
            line_width.saturating_sub(1)
        }
    }

    pub(crate) fn display_col_for_cursor_goal(&self, line: &str, goal: CursorGoal) -> usize {
        match goal {
            CursorGoal::DisplayCol(display_col) => display_col.min(self.line_goal_limit(line)),
            CursorGoal::LineEnd => self.line_goal_limit(line),
        }
    }

    fn grapheme_for_cursor_goal(&self, line: &str, goal: CursorGoal) -> usize {
        match goal {
            CursorGoal::DisplayCol(display_col) => {
                let max_cursor_x = self.max_cursor_x_for_line_length(grapheme_len(line));
                column_to_grapheme(line, display_col).min(max_cursor_x)
            }
            CursorGoal::LineEnd => self.max_cursor_x_for_line_length(grapheme_len(line)),
        }
    }

    fn apply_cursor_goal_to_current_line(&mut self) {
        if let Some(line) = self.current_line_contents() {
            let line = line.trim_end_matches('\n');
            self.cx = self.grapheme_for_cursor_goal(line, self.cursor_goal);
        }
    }

    fn current_screen_segment_bounds(&self) -> Option<(usize, usize)> {
        let line_index = self.buffer_line();
        let line = self.current_line_contents()?;
        let line = line.trim_end_matches('\n');
        let display_col = grapheme_to_column(line, self.cx);
        let window = self.window_manager.active_window()?;
        let layout = self.layout_for_window(window);
        let segment = layout.segment_for_cursor(line_index, display_col)?;
        Some((segment.start_col, segment.end_col))
    }

    fn wrapped_line_segments_for_width(
        &self,
        line_index: usize,
        width: usize,
    ) -> Vec<self::display_layout::LineSegment> {
        let Some(line) = self.current_buffer().get(line_index) else {
            return Vec::new();
        };
        wrap_line_segments(line.trim_end_matches('\n'), line_index, width, 0)
    }

    fn visible_cursor_segment(&self, line_index: usize, display_col: usize) -> bool {
        let Some(window) = self.active_window_with_editor_view() else {
            return false;
        };
        self.layout_for_window(&window)
            .rows
            .iter()
            .any(|segment| segment.line == line_index && segment.contains_display_col(display_col))
    }

    fn scroll_wrapped_viewport_down_one_screen_line(&mut self) -> bool {
        if !self.wrap {
            return false;
        }
        let Some(window) = self.active_window_with_editor_view() else {
            return false;
        };
        let layout = self.layout_for_window(&window);
        let Some(next_top) = layout.rows.get(1).copied() else {
            return false;
        };

        self.vtop = next_top.line;
        self.skipcol = next_top.start_col;
        true
    }

    fn scroll_wrapped_viewport_up_one_screen_line(&mut self) -> bool {
        if !self.wrap {
            return false;
        }

        let width = self.active_content_width();
        if width == 0 {
            return false;
        }

        if self.skipcol > 0 {
            let segments = self.wrapped_line_segments_for_width(self.vtop, width);
            let current_index = segments
                .iter()
                .position(|segment| segment.start_col >= self.skipcol)
                .unwrap_or(segments.len());
            let Some(previous) = current_index
                .checked_sub(1)
                .and_then(|index| segments.get(index))
            else {
                self.skipcol = 0;
                return true;
            };
            self.skipcol = previous.start_col;
            return true;
        }

        let Some(previous_line) = self.vtop.checked_sub(1) else {
            return false;
        };
        let segments = self.wrapped_line_segments_for_width(previous_line, width);
        let Some(previous_top) = segments.last().copied() else {
            return false;
        };

        self.vtop = previous_top.line;
        self.skipcol = previous_top.start_col;
        true
    }

    fn ensure_wrapped_cursor_segment_visible(&mut self, delta: isize) {
        let width = self.active_content_width();
        if width == 0 {
            return;
        }

        let line_index = self.buffer_line();
        let display_col = self.current_cursor_display_col();
        let scroll_once = if delta > 0 {
            Self::scroll_wrapped_viewport_down_one_screen_line
        } else {
            Self::scroll_wrapped_viewport_up_one_screen_line
        };

        for _ in 0..self.vheight().max(1) {
            if self.visible_cursor_segment(line_index, display_col) {
                break;
            }
            if !scroll_once(self) {
                break;
            }
            self.cy = line_index.saturating_sub(self.vtop);
        }
    }

    fn wrapped_screen_line_target(
        &self,
        line_index: usize,
        display_col: usize,
        delta: isize,
        width: usize,
    ) -> Option<(usize, usize)> {
        if delta == 0 {
            return Some((line_index, display_col));
        }

        let segments = self.wrapped_line_segments_for_width(line_index, width);
        let current_index = segments
            .iter()
            .position(|segment| segment.contains_display_col(display_col))
            .or_else(|| segments.len().checked_sub(1))?;
        let current = segments[current_index];
        let offset = display_col.saturating_sub(current.start_col);

        if delta > 0 {
            let mut remaining = delta as usize;
            let mut line = line_index;
            let mut index = current_index;

            loop {
                let segments = self.wrapped_line_segments_for_width(line, width);
                let available_after = segments.len().saturating_sub(index + 1);
                if remaining <= available_after {
                    let target = segments[index + remaining];
                    return Some((
                        target.line,
                        target
                            .start_col
                            .saturating_add(offset)
                            .min(target.end_col.saturating_sub(1)),
                    ));
                }

                remaining = remaining.saturating_sub(available_after + 1);
                if line >= self.last_navigable_line() {
                    return Some((line_index, display_col));
                }
                line += 1;
                index = 0;
                if remaining == 0 {
                    let target = self
                        .wrapped_line_segments_for_width(line, width)
                        .first()?
                        .to_owned();
                    return Some((
                        target.line,
                        target
                            .start_col
                            .saturating_add(offset)
                            .min(target.end_col.saturating_sub(1)),
                    ));
                }
            }
        }

        let mut remaining = delta.unsigned_abs();
        let mut line = line_index;
        let mut index = current_index;

        loop {
            if remaining <= index {
                let target = self.wrapped_line_segments_for_width(line, width)[index - remaining];
                return Some((
                    target.line,
                    target
                        .start_col
                        .saturating_add(offset)
                        .min(target.end_col.saturating_sub(1)),
                ));
            }

            remaining = remaining.saturating_sub(index + 1);
            let Some(previous_line) = line.checked_sub(1) else {
                return Some((line_index, display_col));
            };
            line = previous_line;
            let segments = self.wrapped_line_segments_for_width(line, width);
            index = segments.len().saturating_sub(1);
            if remaining == 0 {
                let target = segments.get(index).copied()?;
                return Some((
                    target.line,
                    target
                        .start_col
                        .saturating_add(offset)
                        .min(target.end_col.saturating_sub(1)),
                ));
            }
        }
    }

    fn move_to_display_col_on_current_line(&mut self, display_col: usize) {
        if let Some(line) = self.current_line_contents() {
            let line = line.trim_end_matches('\n');
            self.cx = self.grapheme_for_cursor_goal(line, CursorGoal::DisplayCol(display_col));
        }
    }

    fn move_to_screen_line_start(&mut self) {
        if !self.wrap {
            self.cx = 0;
            return;
        }

        if let Some((start_col, _)) = self.current_screen_segment_bounds() {
            self.move_to_display_col_on_current_line(start_col);
        }
    }

    fn move_to_screen_line_end(&mut self) {
        if !self.wrap {
            self.cx = self.line_length().saturating_sub(1);
            return;
        }

        if let Some((_, end_col)) = self.current_screen_segment_bounds() {
            self.move_to_display_col_on_current_line(end_col.saturating_sub(1));
        }
    }

    fn move_to_screen_line_first_non_blank(&mut self) {
        if !self.wrap {
            if let Some(line) = self.current_line_contents() {
                self.cx = line
                    .trim_end_matches('\n')
                    .graphemes(true)
                    .position(|grapheme| !grapheme.chars().all(char::is_whitespace))
                    .unwrap_or(0);
            }
            return;
        }

        let Some((start_col, end_col)) = self.current_screen_segment_bounds() else {
            return;
        };
        let Some(line) = self.current_line_contents() else {
            return;
        };
        let line = line.trim_end_matches('\n');
        let target = line
            .graphemes(true)
            .enumerate()
            .find_map(|(index, grapheme)| {
                let col = grapheme_to_column(line, index);
                (col >= start_col && col < end_col && !grapheme.chars().all(char::is_whitespace))
                    .then_some(col)
            })
            .unwrap_or(start_col);
        self.move_to_display_col_on_current_line(target);
    }

    fn move_screen_line(&mut self, delta: isize) {
        let Some(window) = self.window_manager.active_window().cloned() else {
            return;
        };
        let line_index = self.buffer_line();
        let display_col = self.current_cursor_display_col();
        let width = self.active_content_width();
        if self.wrap {
            if let Some((target_line, target_display_col)) =
                self.wrapped_screen_line_target(line_index, display_col, delta, width)
            {
                if target_line < self.vtop {
                    self.vtop = target_line;
                    self.skipcol = 0;
                }
                self.cy = target_line.saturating_sub(self.vtop);
                self.move_to_display_col_on_current_line(target_display_col);
                self.refresh_cursor_goal();
                self.ensure_wrapped_cursor_segment_visible(delta);
            }
            return;
        }

        let layout = self.layout_for_window(&window);
        let Some(current_index) = layout.rows.iter().position(|segment| {
            segment.line == line_index && segment.contains_display_col(display_col)
        }) else {
            return;
        };
        let target_index = if delta < 0 {
            current_index.saturating_sub(delta.unsigned_abs())
        } else {
            (current_index + delta as usize).min(layout.rows.len().saturating_sub(1))
        };
        let Some(target) = layout.rows.get(target_index) else {
            return;
        };
        let target_display_col = target
            .start_col
            .saturating_add(display_col.saturating_sub(layout.rows[current_index].start_col))
            .min(target.end_col.saturating_sub(1));
        self.vtop = target.line.saturating_sub(self.cy);
        self.cy = target.line.saturating_sub(self.vtop);
        self.move_to_display_col_on_current_line(target_display_col);
        self.refresh_cursor_goal();
    }

    fn recompute_window_cursor_goals(&mut self) {
        let buffers = &self.buffers;
        for window in self.window_manager.windows_mut() {
            let buffer_y = window.vtop + window.cy;
            let display_col = buffers
                .get(window.buffer_index)
                .and_then(|buffer| buffer.get(buffer_y))
                .map(|line| grapheme_to_column(line.trim_end_matches('\n'), window.cx))
                .unwrap_or(window.cx);
            window.cursor_goal = CursorGoal::DisplayCol(display_col);
        }
    }

    fn should_refresh_cursor_goal_after(action: &Action) -> bool {
        !matches!(
            action,
            Action::MoveUp
                | Action::MoveDown
                | Action::PageUp
                | Action::PageDown
                | Action::MoveToLineEnd
                | Action::Command(_)
                | Action::PluginCommand(_)
                | Action::Quit(_)
                | Action::Save
                | Action::SaveAs(_)
                | Action::DumpHistory
                | Action::DumpBuffer
                | Action::DumpDiagnostics
                | Action::DumpCapabilities
                | Action::DumpTimers
                | Action::DoPing
                | Action::RefreshDiagnostics
                | Action::Refresh
                | Action::Hover
                | Action::Print(_)
                | Action::ShowProgress(_)
                | Action::NotifyPlugins(_, _)
                | Action::ViewLogs
                | Action::SetCursor(_, _)
                | Action::SetWaitingKey(_)
        )
    }

    fn event_snapshot(&self) -> EditorEventSnapshot {
        let (width, height) = self
            .window_manager
            .active_window()
            .map(|window| (window.inner_width(), window.inner_height()))
            .unwrap_or((self.vwidth(), self.vheight()));

        EditorEventSnapshot {
            mode: self.mode,
            cx: self.cx,
            y: self.cy + self.vtop,
            vtop: self.vtop,
            vleft: self.vleft,
            skipcol: self.skipcol,
            wrap: self.wrap,
            width,
            height,
            buffer_index: self.current_buffer_index,
        }
    }

    fn action_cause(action: &Action) -> String {
        let debug = format!("{action:?}");
        debug
            .split(['(', '{', ' '])
            .next()
            .unwrap_or("Action")
            .to_string()
    }

    async fn notify_editor_event_changes(
        &mut self,
        before: EditorEventSnapshot,
        runtime: &mut Runtime,
        cause: &str,
    ) -> anyhow::Result<()> {
        let after = self.event_snapshot();

        if before.mode != after.mode {
            let from = format!("{:?}", before.mode);
            let to = format!("{:?}", after.mode);
            let mode_info = serde_json::json!({
                "from": &from,
                "to": &to,
                "old_mode": &from,
                "new_mode": &to,
                "cause": cause,
            });
            self.plugin_registry
                .notify(runtime, "mode:changed", mode_info)
                .await?;
        }

        if before.cx != after.cx
            || before.y != after.y
            || before.vtop != after.vtop
            || before.buffer_index != after.buffer_index
        {
            let cursor_info = serde_json::json!({
                "from": {
                    "x": before.cx,
                    "y": before.y,
                },
                "to": {
                    "x": after.cx,
                    "y": after.y,
                },
                "x": after.cx,
                "y": after.y,
                "mode": format!("{:?}", after.mode),
                "cause": cause,
                "viewportTop": after.vtop,
                "viewport_top": after.vtop,
                "bufferIndex": after.buffer_index,
                "buffer_index": after.buffer_index,
            });
            self.plugin_registry
                .notify(runtime, "cursor:moved", cursor_info)
                .await?;
        }

        if before.vtop != after.vtop
            || before.vleft != after.vleft
            || before.skipcol != after.skipcol
            || before.wrap != after.wrap
            || before.width != after.width
            || before.height != after.height
            || before.buffer_index != after.buffer_index
        {
            let viewport_info = serde_json::json!({
                "vtop": after.vtop,
                "vleft": after.vleft,
                "skipcol": after.skipcol,
                "wrap": after.wrap,
                "width": after.width,
                "height": after.height,
                "bufferIndex": after.buffer_index,
                "buffer_index": after.buffer_index,
                "cause": cause,
            });
            self.plugin_registry
                .notify(runtime, "viewport:changed", viewport_info)
                .await?;
        }

        Ok(())
    }

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
        let max_vtop = if self.wrap {
            last_line
        } else {
            last_line.saturating_sub(viewport_height.saturating_sub(1))
        };

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
            let mut scrolloff_vtop = self.vtop;
            if buffer_line < scrolloff_vtop + scrolloff {
                scrolloff_vtop = buffer_line.saturating_sub(scrolloff);
            } else if buffer_line >= scrolloff_vtop + viewport_height.saturating_sub(scrolloff) {
                scrolloff_vtop = buffer_line
                    .saturating_add(scrolloff)
                    .saturating_add(1)
                    .saturating_sub(viewport_height);
            }

            scrolloff_vtop = scrolloff_vtop.min(max_vtop);
            if !self.wrap || self.buffer_line_visible_from(scrolloff_vtop, buffer_line) {
                self.vtop = scrolloff_vtop;
                self.cy = buffer_line.saturating_sub(self.vtop);
            }
        }

        let max_cursor_x = self.max_cursor_x_for_line_length(self.line_length());

        if self.cx > max_cursor_x {
            self.cx = max_cursor_x;
        }
        old_position != (self.cx, self.cy, self.vtop)
    }

    fn buffer_line_visible_from(&self, vtop: usize, buffer_line: usize) -> bool {
        let Some(mut window) = self.active_window_with_editor_view() else {
            return (vtop..vtop + self.vheight()).contains(&buffer_line);
        };
        window.vtop = vtop;
        window.cy = buffer_line.saturating_sub(vtop);

        self.layout_for_window(&window)
            .rows
            .iter()
            .any(|segment| segment.line == buffer_line)
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

        'editor_loop: loop {
            futures_timer::Delay::new(Duration::from_millis(10)).await;

            while event::poll(Duration::from_millis(0))? {
                let ev = event::read()?;
                let processed = self
                    .process_editor_event(ev, &mut buffer, &mut runtime, EventRenderMode::Immediate)
                    .await?;
                if processed.quit {
                    break 'editor_loop;
                }
                if let Some(signature) = processed
                    .drain_repeated_motion
                    .then_some(processed.repeat_signature)
                    .flatten()
                {
                    if self
                        .drain_repeated_motion_events(signature, &mut buffer, &mut runtime)
                        .await?
                    {
                        break 'editor_loop;
                    }
                }
            }

            // Poll for timer callbacks
            let timer_callbacks = crate::plugin::poll_timer_callbacks();
            for callback_request in timer_callbacks {
                if let PluginRequest::TimeoutCallback { timer_id } = callback_request {
                    log!(
                        "[TIMER] Processing timeout callback for timer: {}",
                        timer_id
                    );
                    self.plugin_registry
                        .notify(
                            &mut runtime,
                            "timeout:callback",
                            json!({ "timerId": timer_id }),
                        )
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

            let dialog_changed = if let Some(current_dialog) = &mut self.current_dialog {
                current_dialog.tick()?
            } else {
                false
            };
            if dialog_changed {
                self.render(&mut buffer)?;
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
                Ok(None) => {}
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
                        let items = items
                            .iter()
                            .map(|v| match v {
                                serde_json::Value::String(s) => s.clone(),
                                val => val.to_string(),
                            })
                            .collect();
                        self.execute(
                            &Action::OpenPicker(title, items, id),
                            &mut buffer,
                            &mut runtime,
                        )
                        .await?;
                        // self.render(buffer)?;
                    }
                    PluginRequest::OpenLivePicker(title, id, items, initial_selection) => {
                        let items = items
                            .iter()
                            .map(|v| match v {
                                serde_json::Value::String(s) => s.clone(),
                                val => val.to_string(),
                            })
                            .collect();
                        self.execute(
                            &Action::OpenLivePicker(title, items, id, initial_selection),
                            &mut buffer,
                            &mut runtime,
                        )
                        .await?;
                    }
                    PluginRequest::BufferInsert { x, y, text } => {
                        self.begin_transaction("plugin insert");
                        self.replace_range(TextRange::insertion(TextPosition::new(y, x)), &text);
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
                    PluginRequest::GetBufferText {
                        start_line,
                        end_line,
                    } => {
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
                            .notify(
                                &mut runtime,
                                "buffer:text",
                                serde_json::json!({ "text": text }),
                            )
                            .await?;
                    }
                    PluginRequest::GetViewportLayout { request_id } => {
                        let payload = self.plugin_viewport_layout_payload();
                        self.plugin_registry
                            .notify(
                                &mut runtime,
                                &format!("viewport:layout:{request_id}"),
                                payload,
                            )
                            .await?;
                    }
                    PluginRequest::SetDecorations {
                        namespace,
                        decorations,
                    } => {
                        let current_buffer_index = self.current_buffer_index;
                        let decorations = decorations
                            .into_iter()
                            .map(|mut decoration| {
                                decoration.buffer_index.get_or_insert(current_buffer_index);
                                decoration
                            })
                            .collect();
                        if self.decoration_manager.set(namespace, decorations) {
                            self.render(&mut buffer)?;
                        }
                    }
                    PluginRequest::ClearDecorations { namespace } => {
                        if self.decoration_manager.clear(&namespace) {
                            self.render(&mut buffer)?;
                        }
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
                                "cwd" => json!(std::env::current_dir()
                                    .ok()
                                    .map(|path| path.to_string_lossy().to_string())),
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
                            .notify(
                                &mut runtime,
                                "config:value",
                                json!({ "value": config_value }),
                            )
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
                    PluginRequest::RestoreEditorState {
                        request_id,
                        snapshot,
                    } => {
                        let result = self.restore_editor_state(snapshot, &mut buffer).await;
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
                    PluginRequest::DocumentSymbols { request_id } => {
                        let event = format!("lsp:document_symbols:{request_id}");
                        let Some(file) = self.current_buffer().file.clone() else {
                            self.plugin_registry
                                .notify(
                                    &mut runtime,
                                    &event,
                                    plugin_lsp_error("current buffer is not file-backed"),
                                )
                                .await?;
                            continue;
                        };

                        let request_result: anyhow::Result<i64> = async {
                            self.ensure_current_buffer_lsp_opened().await?;
                            Ok(self.lsp.document_symbols(&file).await?)
                        }
                        .await;

                        match request_result {
                            Ok(lsp_request_id) if lsp_request_id > 0 => {
                                self.pending_plugin_document_symbols
                                    .insert(lsp_request_id, request_id);
                            }
                            Ok(_) => {
                                self.plugin_registry
                                    .notify(
                                        &mut runtime,
                                        &event,
                                        plugin_lsp_error(
                                            "no language server is available for this file",
                                        ),
                                    )
                                    .await?;
                            }
                            Err(err) => {
                                self.plugin_registry
                                    .notify(
                                        &mut runtime,
                                        &event,
                                        plugin_lsp_error(&err.to_string()),
                                    )
                                    .await?;
                            }
                        }
                    }
                    PluginRequest::InlayHints { request_id, range } => {
                        let event = format!("lsp:inlay_hints:{request_id}");
                        let Some(file) = self.current_buffer().file.clone() else {
                            self.plugin_registry
                                .notify(
                                    &mut runtime,
                                    &event,
                                    plugin_lsp_error("current buffer is not file-backed"),
                                )
                                .await?;
                            continue;
                        };

                        let range = range.unwrap_or_else(|| Range {
                            start: crate::lsp::Position {
                                line: 0,
                                character: 0,
                            },
                            end: crate::lsp::Position {
                                line: self.current_buffer().len().saturating_add(1),
                                character: 0,
                            },
                        });

                        let request_result: anyhow::Result<i64> = async {
                            self.ensure_current_buffer_lsp_opened().await?;
                            Ok(self.lsp.inlay_hint(&file, range).await?)
                        }
                        .await;

                        match request_result {
                            Ok(lsp_request_id) if lsp_request_id > 0 => {
                                self.pending_plugin_inlay_hints
                                    .insert(lsp_request_id, request_id);
                            }
                            Ok(_) => {
                                self.plugin_registry
                                    .notify(
                                        &mut runtime,
                                        &event,
                                        plugin_lsp_error(
                                            "no language server is available for this file",
                                        ),
                                    )
                                    .await?;
                            }
                            Err(err) => {
                                self.plugin_registry
                                    .notify(
                                        &mut runtime,
                                        &event,
                                        plugin_lsp_error(&err.to_string()),
                                    )
                                    .await?;
                            }
                        }
                    }
                    PluginRequest::GetTextDisplayWidth { text } => {
                        let width = crate::unicode_utils::display_width(&text);
                        self.plugin_registry
                            .notify(
                                &mut runtime,
                                "text:display_width",
                                json!({ "width": width }),
                            )
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
                            .notify(
                                &mut runtime,
                                "char:display_column",
                                json!({ "column": display_col }),
                            )
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
                            .notify(
                                &mut runtime,
                                "display:char_index",
                                json!({ "index": char_index }),
                            )
                            .await?;
                    }
                    PluginRequest::IntervalCallback { interval_id } => {
                        self.plugin_registry
                            .notify(
                                &mut runtime,
                                "interval:callback",
                                json!({ "intervalId": interval_id }),
                            )
                            .await?;
                    }
                    PluginRequest::TimeoutCallback { timer_id } => {
                        log!(
                            "[TIMER] Processing timeout callback for timer: {}",
                            timer_id
                        );
                        self.plugin_registry
                            .notify(
                                &mut runtime,
                                "timeout:callback",
                                json!({ "timerId": timer_id }),
                            )
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
                    PluginRequest::GetGitStatus { path, request_id } => {
                        let payload = git_status_listing(&path);
                        self.plugin_registry
                            .notify(&mut runtime, &format!("git:status:{request_id}"), payload)
                            .await?;
                    }
                    PluginRequest::WatchDirectory { path, watch_id } => {
                        self.directory_watchers.insert(
                            watch_id,
                            DirectoryWatcher {
                                snapshot: directory_listing(&path),
                                path,
                                last_checked: Instant::now(),
                            },
                        );
                    }
                    PluginRequest::UnwatchDirectory { watch_id } => {
                        self.directory_watchers.remove(&watch_id);
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

    async fn process_editor_event(
        &mut self,
        ev: event::Event,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
        render_mode: EventRenderMode,
    ) -> anyhow::Result<ProcessedEvent> {
        self.check_bounds();

        if let event::Event::Resize(width, height) = ev {
            self.size = (width, height);
            let max_y = height as usize - 2;
            if self.cy > max_y - 1 {
                self.cy = max_y - 1;
            }
            self.resize_window_layout((width as usize, height as usize));
            *buffer = RenderBuffer::new(
                self.size.0 as usize,
                self.size.1 as usize,
                &Style::default(),
            );
            // TODO: handle dialog resize
            self.current_dialog = None;
            self.render(buffer)?;

            let action = Action::NotifyPlugins(
                "editor:resize".to_string(),
                serde_json::to_value(self.size)?,
            );
            self.execute(&action, buffer, runtime).await?;
            return Ok(ProcessedEvent {
                quit: false,
                drain_repeated_motion: false,
                repeat_signature: None,
            });
        }

        if self.handle_focus_event(&ev, buffer)? {
            return Ok(ProcessedEvent {
                quit: false,
                drain_repeated_motion: false,
                repeat_signature: None,
            });
        }

        let render_generation = self.render_generation;
        let repeat_signature = Self::key_signature(&ev);
        let mut drain_repeated_motion = false;

        let from_waiting_key_action = self.waiting_key_action.is_some();
        if let Some(action) = self.handle_event(&ev)? {
            drain_repeated_motion =
                !from_waiting_key_action && self.should_drain_repeated_motion(&ev, &action);
            if self
                .handle_key_action(&ev, &action, buffer, runtime)
                .await?
            {
                return Ok(ProcessedEvent {
                    quit: true,
                    drain_repeated_motion: false,
                    repeat_signature: None,
                });
            }
        }

        if render_mode == EventRenderMode::Immediate && self.render_generation == render_generation
        {
            self.render(buffer)?;
        }

        Ok(ProcessedEvent {
            quit: false,
            drain_repeated_motion,
            repeat_signature,
        })
    }

    async fn drain_repeated_motion_events(
        &mut self,
        signature: KeySignature,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<bool> {
        let started = Instant::now();
        let mut deferred_motion = false;
        while started.elapsed() < Duration::from_millis(REPEATED_MOTION_DRAIN_BUDGET_MS) {
            if !event::poll(Duration::from_millis(0))? {
                break;
            }

            let ev = event::read()?;
            let same_key = Self::key_signature(&ev).is_some_and(|next| next == signature);
            if !same_key {
                if deferred_motion {
                    self.flush_deferred_motion_render(buffer)?;
                    deferred_motion = false;
                }
                let processed = self
                    .process_editor_event(ev, buffer, runtime, EventRenderMode::Immediate)
                    .await?;
                if processed.quit {
                    return Ok(true);
                }
                break;
            }

            self.defer_motion_render = true;
            let processed_result = self
                .process_editor_event(ev, buffer, runtime, EventRenderMode::DeferredMotion)
                .await;
            self.defer_motion_render = false;
            let processed = processed_result?;
            deferred_motion = true;
            if processed.quit {
                return Ok(true);
            }
            if !processed.drain_repeated_motion {
                break;
            }
        }

        // Repeated motion events are lossy once they exceed the frame budget.
        // This keeps a released key from continuing to walk through stale input.
        while event::poll(Duration::from_millis(0))? {
            let ev = event::read()?;
            if Self::key_signature(&ev).is_some_and(|next| next == signature) {
                continue;
            }

            if deferred_motion {
                self.flush_deferred_motion_render(buffer)?;
                deferred_motion = false;
            }

            let processed = self
                .process_editor_event(ev, buffer, runtime, EventRenderMode::Immediate)
                .await?;
            if processed.quit {
                return Ok(true);
            }
            break;
        }

        if deferred_motion {
            self.flush_deferred_motion_render(buffer)?;
        }

        Ok(false)
    }

    fn flush_deferred_motion_render(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.defer_motion_render = false;
        if self.deferred_motion_needs_full_render {
            self.deferred_motion_needs_full_render = false;
            self.render(buffer)
        } else if self.can_render_cursor_motion_delta() {
            self.render_cursor_motion_delta(buffer)
        } else if self.uses_synthetic_block_cursor() {
            self.render_motion_frame(buffer)
        } else {
            self.draw_cursor_preserving_cursor_goal()
        }
    }

    fn key_signature(ev: &event::Event) -> Option<KeySignature> {
        let event::Event::Key(KeyEvent {
            code, modifiers, ..
        }) = ev
        else {
            return None;
        };

        Some(KeySignature {
            code: *code,
            modifiers: *modifiers,
        })
    }

    fn should_drain_repeated_motion(&self, ev: &event::Event, action: &KeyAction) -> bool {
        Self::key_signature(ev).is_some()
            && self.is_normal()
            && self.repeater.is_none()
            && self.pending_operator.is_none()
            && self.waiting_key_action.is_none()
            && self.key_action_is_pure_motion(action)
    }

    fn key_action_is_pure_motion(&self, action: &KeyAction) -> bool {
        match action {
            KeyAction::Single(action) => Self::action_is_pure_motion(action),
            KeyAction::Multiple(actions) => actions.iter().all(Self::action_is_pure_motion),
            KeyAction::Repeating(_, action) => self.key_action_is_pure_motion(action),
            KeyAction::None | KeyAction::Nested(_) => false,
        }
    }

    fn action_is_pure_motion(action: &Action) -> bool {
        matches!(
            action,
            Action::MoveUp
                | Action::MoveDown
                | Action::MoveLeft
                | Action::MoveRight
                | Action::MoveScreenLineUp
                | Action::MoveScreenLineDown
                | Action::MoveToScreenLineEnd
                | Action::MoveToScreenLineStart
                | Action::MoveToScreenLineFirstNonBlank
                | Action::MoveToNextWord
                | Action::MoveToPreviousWord
                | Action::MatchitForward
                | Action::MatchitBackward
                | Action::MatchitPreviousUnmatched
                | Action::MatchitNextUnmatched
        )
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

    fn completion_filter_for_response(&self, msg: &ResponseMessage) -> Option<String> {
        let req = msg.request.as_ref()?;
        let params = req.params.as_object()?;
        let position = params.get("position")?.as_object()?;
        let request_line = position.get("line")?.as_u64()? as usize;
        let request_character = position.get("character")?.as_u64()? as usize;

        if request_line != self.buffer_line() {
            return None;
        }

        let current_character = self.grapheme_to_char_on_line(self.cx, self.buffer_line());
        if current_character < request_character {
            return None;
        }

        let line = self.current_buffer().get(request_line)?;
        Some(
            line.chars()
                .skip(request_character)
                .take(current_character - request_character)
                .collect(),
        )
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

                    if method == "textDocument/documentSymbol" {
                        if let Some(request_id) =
                            self.pending_plugin_document_symbols.remove(&msg.id)
                        {
                            let payload = match self.plugin_document_symbols_payload(msg) {
                                Ok(payload) => payload,
                                Err(err) => plugin_lsp_error(&err.to_string()),
                            };
                            return Some(Action::NotifyPlugins(
                                format!("lsp:document_symbols:{request_id}"),
                                payload,
                            ));
                        }
                    }

                    if method == "textDocument/inlayHint" {
                        if let Some(request_id) = self.pending_plugin_inlay_hints.remove(&msg.id) {
                            let payload = match self.plugin_inlay_hints_payload(msg) {
                                Ok(payload) => payload,
                                Err(err) => plugin_lsp_error(&err.to_string()),
                            };
                            return Some(Action::NotifyPlugins(
                                format!("lsp:inlay_hints:{request_id}"),
                                payload,
                            ));
                        }
                    }

                    if method == "textDocument/completion" {
                        if msg.result.is_null() {
                            // TODO: retry?
                            return None;
                        }

                        if let Some(request_uri) = response_text_document_uri(msg) {
                            let current_uri = self.current_buffer().uri().ok().flatten();
                            if current_uri.as_deref() != Some(request_uri) {
                                log!(
                                    "ignoring stale completion response for {request_uri}: current uri is {current_uri:?}"
                                );
                                return None;
                            }
                        }

                        match serde_json::from_value::<CompletionResponse>(msg.result.clone()) {
                            Ok(completion_response) => {
                                let items = completion_response.items();
                                if items.is_empty() || self.mode != Mode::Insert {
                                    return None;
                                }
                                let (completion_x, completion_y) =
                                    self.render_cursor_position().unwrap_or((self.cx, self.cy));
                                self.completion_ui.set_theme(&self.theme);
                                self.completion_ui.show_with_bounds(
                                    items,
                                    completion_x,
                                    completion_y,
                                    self.size.0 as usize,
                                    self.size.1 as usize,
                                );
                                if let Some(filter) = self.completion_filter_for_response(msg) {
                                    self.completion_ui.set_filter(&filter);
                                }
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
                            serde_json::Value::Array(ref arr) => {
                                arr.first().and_then(|value| value.as_object())?
                            }
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
            let action = current_dialog.handle_event(ev);
            if action.is_some() || !current_dialog.allows_event_passthrough() {
                return Ok(action);
            }
        }

        if self.is_command() {
            return Ok(self.handle_command_event(ev));
        }

        if self.is_search() {
            return Ok(self.handle_search_event(ev));
        }

        if self.panel_manager.focused_panel_id().is_some() {
            if let Some(action) = self.handle_panel_event(ev) {
                return Ok(Some(action));
            }

            if let Some(action) = self.panel_global_key_action(ev) {
                return Ok(Some(action));
            }

            if matches!(ev, Event::Mouse(_)) {
                self.panel_manager.focus_editor();
            } else {
                return Ok(None);
            }
        }

        if matches!(ev, Event::Mouse(_)) {
            if let Some(action) = self.handle_panel_event(ev) {
                return Ok(Some(action));
            }
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
        match ev {
            Event::Key(event) => {
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
                    KeyCode::Char(' ') => "toggle",
                    KeyCode::Char('q') => "close",
                    KeyCode::Char('R') => "refresh",
                    _ => return None,
                };

                let height = self.size.1 as usize;
                self.panel_manager
                    .handle_focused_key(action, height)
                    .and_then(Self::panel_event_key_action)
            }
            Event::Mouse(event) => self.handle_panel_mouse_event(event),
            _ => None,
        }
    }

    fn handle_panel_mouse_event(&mut self, event: &MouseEvent) -> Option<KeyAction> {
        let x = event.column as usize;
        let y = event.row as usize;
        let width = self.size.0 as usize;
        let height = self.size.1 as usize;

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => self
                .panel_manager
                .focus_panel_at_position(x, y, width, height)
                .and_then(Self::panel_event_key_action),
            MouseEventKind::ScrollUp => {
                let id = self
                    .panel_manager
                    .panel_at_position(x, y, width, height)?
                    .id;
                self.panel_manager.focus_panel(&id);
                self.panel_manager
                    .handle_focused_key("up", height)
                    .and_then(Self::panel_event_key_action)
            }
            MouseEventKind::ScrollDown => {
                let id = self
                    .panel_manager
                    .panel_at_position(x, y, width, height)?
                    .id;
                self.panel_manager.focus_panel(&id);
                self.panel_manager
                    .handle_focused_key("down", height)
                    .and_then(Self::panel_event_key_action)
            }
            _ => None,
        }
    }

    fn panel_event_key_action(event: plugin::panel::PanelEvent) -> Option<KeyAction> {
        serde_json::to_value(&event).ok().map(|payload| {
            KeyAction::Multiple(vec![
                Action::NotifyPlugins(format!("panel:event:{}", event.panel_id), payload),
                Action::Refresh,
            ])
        })
    }

    fn panel_global_key_action(&self, ev: &event::Event) -> Option<KeyAction> {
        let key = Self::key_string_for_event(ev)?;
        let action = self.config.keys.normal.get(&key).cloned().or_else(|| {
            matches!(key.as_str(), "Tab")
                .then(|| self.config.keys.normal.get("Tab").cloned())
                .flatten()
        })?;

        Self::key_action_enters_term(&action).then_some(action)
    }

    fn key_action_enters_term(action: &KeyAction) -> bool {
        match action {
            KeyAction::Single(Action::EnterMode(Mode::Command | Mode::Search)) => true,
            KeyAction::Multiple(actions) => actions
                .iter()
                .any(|action| matches!(action, Action::EnterMode(Mode::Command | Mode::Search))),
            _ => false,
        }
    }

    fn key_string_for_event(ev: &Event) -> Option<String> {
        let Event::Key(KeyEvent {
            code, modifiers, ..
        }) = ev
        else {
            return None;
        };

        let key = match code {
            KeyCode::Char(' ') => "Space".to_string(),
            KeyCode::Char(c) => format!("{c}"),
            _ => format!("{code:?}"),
        };

        Some(match *modifiers {
            KeyModifiers::CONTROL => format!("Ctrl-{key}"),
            KeyModifiers::ALT => format!("Alt-{key}"),
            _ => key,
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
            "noh",
            "nohlsearch",
            "wrap",
            "nowrap",
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
                } else {
                    actions.push(Action::ReloadFile(parsed.is_forced()));
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
                actions.push(Action::OnlyWindow);
            }

            if cmd == "noh" || cmd == "nohlsearch" {
                actions.push(Action::ClearSearchHighlight);
            }

            if cmd == "wrap" {
                actions.push(Action::SetWrap(true));
            }

            if cmd == "nowrap" {
                actions.push(Action::SetWrap(false));
            }
        }
        actions
    }

    fn delete_last_char(text: &mut String) {
        text.pop();
    }

    fn search_uses_case_insensitive(&self, pattern: &str) -> bool {
        self.config.search.ignorecase
            && !(self.config.search.smartcase && pattern.chars().any(char::is_uppercase))
    }

    fn compile_search_regex(&self, pattern: &str) -> anyhow::Result<Regex> {
        RegexBuilder::new(pattern)
            .case_insensitive(self.search_uses_case_insensitive(pattern))
            .build()
            .map_err(|err| anyhow::anyhow!("invalid search pattern: {err}"))
    }

    fn search_matches(&self, pattern: &str) -> anyhow::Result<Vec<SearchMatch>> {
        if pattern.is_empty() {
            return Ok(Vec::new());
        }

        let regex = self.compile_search_regex(pattern)?;
        Ok(self.current_buffer().regex_matches(&regex))
    }

    fn search_match_in_direction(
        &self,
        matches: &[SearchMatch],
        origin: &HistoryEntry,
        direction: SearchDirection,
        wrap: bool,
    ) -> Option<SearchMatch> {
        match direction {
            SearchDirection::Forward => matches
                .iter()
                .copied()
                .find(|match_| {
                    let origin_x = self.grapheme_to_char_on_line(origin.x, origin.y);
                    match_.start_y > origin.y
                        || (match_.start_y == origin.y && match_.start_x > origin_x)
                })
                .or_else(|| wrap.then(|| matches.first().copied()).flatten()),
            SearchDirection::Backward => matches
                .iter()
                .rev()
                .copied()
                .find(|match_| {
                    let origin_x = self.grapheme_to_char_on_line(origin.x, origin.y);
                    match_.start_y < origin.y
                        || (match_.start_y == origin.y && match_.start_x < origin_x)
                })
                .or_else(|| wrap.then(|| matches.last().copied()).flatten()),
        }
    }

    fn move_to_search_match(&mut self, match_: SearchMatch) {
        let y = match_.start_y.min(self.last_navigable_line());
        let viewport_height = self.vheight().max(1);
        self.vtop = y.saturating_sub(viewport_height / 2);
        self.cy = y.saturating_sub(self.vtop);
        self.cx = self.char_to_grapheme_on_line(match_.start_x, y);
        self.check_bounds();
        self.refresh_cursor_goal();
        self.sync_to_window();
    }

    fn restore_search_origin(&mut self, origin: &HistoryEntry, vtop: usize) {
        self.vtop = vtop.min(self.last_navigable_line());
        let y = origin.y.min(self.last_navigable_line());
        self.cy = y.saturating_sub(self.vtop);
        self.cx = origin.x;
        self.check_bounds();
        self.refresh_cursor_goal();
        self.sync_to_window();
    }

    fn begin_search(&mut self, direction: SearchDirection) {
        self.active_search = Some(SearchSession {
            origin: self.current_history_entry(),
            origin_vtop: self.vtop,
            direction,
            draft: String::new(),
            preview: None,
        });
        self.mode = Mode::Search;
        self.waiting_command = None;
        self.repeater = None;
        self.last_error = None;
    }

    fn update_search_preview(&mut self) {
        let Some(session) = self.active_search.clone() else {
            return;
        };

        if session.draft.is_empty() || !self.config.search.incsearch {
            self.restore_search_origin(&session.origin, session.origin_vtop);
            if let Some(active_search) = &mut self.active_search {
                active_search.preview = None;
            }
            return;
        }

        let preview = match self.search_matches(&session.draft) {
            Ok(matches) => self.search_match_in_direction(
                &matches,
                &session.origin,
                session.direction,
                self.config.search.wrapscan,
            ),
            Err(err) => {
                self.last_error = Some(err.to_string());
                None
            }
        };

        if let Some(match_) = preview {
            self.last_error = None;
            self.move_to_search_match(match_);
        } else {
            self.restore_search_origin(&session.origin, session.origin_vtop);
        }

        if let Some(active_search) = &mut self.active_search {
            active_search.preview = preview;
        }
    }

    fn cancel_active_search(&mut self) {
        if let Some(session) = self.active_search.take() {
            self.restore_search_origin(&session.origin, session.origin_vtop);
        }
        self.mode = Mode::Normal;
        self.last_error = None;
    }

    fn active_search_text(&self) -> Option<&str> {
        self.active_search
            .as_ref()
            .map(|session| session.draft.as_str())
    }

    fn search_commandline_prefix(&self) -> &'static str {
        if self
            .active_search
            .as_ref()
            .is_some_and(|search| search.direction == SearchDirection::Backward)
        {
            "?"
        } else {
            "/"
        }
    }

    fn execute_search_direction(
        &mut self,
        direction: SearchDirection,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<bool> {
        let pattern = self
            .active_search
            .as_ref()
            .map(|search| search.draft.as_str())
            .unwrap_or(&self.search_term)
            .to_string();
        if pattern.is_empty() {
            return Ok(false);
        }

        let matches = match self.search_matches(&pattern) {
            Ok(matches) => matches,
            Err(err) => {
                self.last_error = Some(err.to_string());
                self.render(buffer)?;
                return Ok(false);
            }
        };
        let origin = self.current_history_entry();
        if let Some(match_) = self.search_match_in_direction(
            &matches,
            &origin,
            direction,
            self.config.search.wrapscan,
        ) {
            self.last_error = None;
            self.search_highlights_suppressed = false;
            self.move_to_search_match(match_);
            if let Some(active_search) = &mut self.active_search {
                active_search.preview = Some(match_);
            }
            self.render(buffer)?;
            return Ok(true);
        } else {
            self.last_error = Some(format!("pattern not found: {pattern}"));
            self.render(buffer)?;
        }

        Ok(false)
    }

    fn reset_command_history_navigation(&mut self) {
        self.command_history_navigation = None;
    }

    fn reset_command_completion(&mut self) {
        self.command_completion = None;
    }

    fn command_history_matches(&self, prefix: &str) -> Vec<usize> {
        self.preferences
            .command_history()
            .iter()
            .enumerate()
            .filter_map(|(index, command)| command.starts_with(prefix).then_some(index))
            .collect()
    }

    fn navigate_command_history(&mut self, direction: CommandHistoryDirection) {
        let mut navigation = self.command_history_navigation.take().unwrap_or_else(|| {
            let prefix = self.command.clone();
            let position = self.command_history_matches(&prefix).len();
            CommandHistoryNavigation {
                prefix,
                original: self.command.clone(),
                position,
            }
        });

        let matches = self.command_history_matches(&navigation.prefix);
        if matches.is_empty() {
            self.command_history_navigation = None;
            return;
        }

        match direction {
            CommandHistoryDirection::Previous => {
                navigation.position = navigation.position.saturating_sub(1);
                self.command =
                    self.preferences.command_history()[matches[navigation.position]].clone();
            }
            CommandHistoryDirection::Next => {
                if navigation.position + 1 < matches.len() {
                    navigation.position += 1;
                    self.command =
                        self.preferences.command_history()[matches[navigation.position]].clone();
                } else {
                    navigation.position = matches.len();
                    self.command = navigation.original.clone();
                }
            }
        }

        self.command_history_navigation = Some(navigation);
    }

    fn command_accepts_file_completion(command: &str) -> bool {
        matches!(
            command.trim_end_matches('!'),
            "e" | "edit" | "w" | "write" | "sp" | "split" | "vs" | "vsplit"
        )
    }

    fn command_completion_context(command: &str) -> Option<CommandCompletionContext> {
        let command_start = command
            .char_indices()
            .find_map(|(index, ch)| (!ch.is_whitespace()).then_some(index))?;
        let after_leading = &command[command_start..];
        let command_end = after_leading
            .char_indices()
            .find_map(|(index, ch)| ch.is_whitespace().then_some(command_start + index))
            .unwrap_or(command.len());
        let command_name = &command[command_start..command_end];
        if !Self::command_accepts_file_completion(command_name) {
            return None;
        }

        let args_start = command[command_end..]
            .char_indices()
            .find_map(|(index, ch)| (!ch.is_whitespace()).then_some(command_end + index));

        if let Some(replacement_start) = args_start {
            Some(CommandCompletionContext {
                replacement_start,
                replacement_end: command.len(),
                fragment: command[replacement_start..].to_string(),
                needs_leading_space: false,
            })
        } else {
            Some(CommandCompletionContext {
                replacement_start: command_end,
                replacement_end: command.len(),
                fragment: String::new(),
                needs_leading_space: true,
            })
        }
    }

    fn dot_directory_candidate(fragment: &str) -> Option<PathCompletionCandidate> {
        match fragment {
            "." => Some(PathCompletionCandidate {
                replacement: "./".to_string(),
                is_dir: true,
            }),
            ".." => Some(PathCompletionCandidate {
                replacement: "../".to_string(),
                is_dir: true,
            }),
            "~" if expand_user_path("~").is_ok() => Some(PathCompletionCandidate {
                replacement: "~/".to_string(),
                is_dir: true,
            }),
            _ => None,
        }
    }

    fn path_completion_candidates(fragment: &str) -> Vec<PathCompletionCandidate> {
        if let Some(candidate) = Self::dot_directory_candidate(fragment) {
            return vec![candidate];
        }

        let (directory_fragment, file_prefix) = fragment
            .rfind('/')
            .map(|index| (&fragment[..=index], &fragment[index + 1..]))
            .unwrap_or(("", fragment));
        let directory_path = if directory_fragment.is_empty() {
            PathBuf::from(".")
        } else {
            expand_user_path(directory_fragment)
                .unwrap_or_else(|_| PathBuf::from(directory_fragment))
        };

        let Ok(read_dir) = fs::read_dir(directory_path) else {
            return Vec::new();
        };

        let mut candidates = read_dir
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !name.starts_with(file_prefix) {
                    return None;
                }

                let is_dir = entry
                    .file_type()
                    .map(|file_type| file_type.is_dir())
                    .unwrap_or(false);
                let suffix = if is_dir { "/" } else { "" };
                Some(PathCompletionCandidate {
                    replacement: format!("{directory_fragment}{name}{suffix}"),
                    is_dir,
                })
            })
            .collect::<Vec<_>>();

        candidates.sort_by(|a, b| match (a.is_dir, b.is_dir) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => a.replacement.cmp(&b.replacement),
        });
        candidates
    }

    fn apply_command_completion_candidate(&mut self, candidate: &str) {
        let Some(completion) = self.command_completion.as_mut() else {
            return;
        };
        let replacement = if completion.needs_leading_space {
            format!(" {candidate}")
        } else {
            candidate.to_string()
        };
        self.command.replace_range(
            completion.replacement_start..completion.replacement_end,
            &replacement,
        );
        completion.replacement_end = completion.replacement_start + replacement.len();
    }

    fn complete_command_path(&mut self, direction: CompletionDirection) {
        if let Some(mut completion) = self.command_completion.take() {
            if completion.candidates.len() > 1 {
                completion.selected = match direction {
                    CompletionDirection::Next => {
                        (completion.selected + 1) % completion.candidates.len()
                    }
                    CompletionDirection::Previous => completion
                        .selected
                        .checked_sub(1)
                        .unwrap_or_else(|| completion.candidates.len() - 1),
                };
                self.command_completion = Some(completion);
                let candidate = self
                    .command_completion
                    .as_ref()
                    .and_then(|state| state.candidates.get(state.selected).cloned());
                if let Some(candidate) = candidate {
                    self.apply_command_completion_candidate(&candidate);
                }
                return;
            }
        }

        let Some(context) = Self::command_completion_context(&self.command) else {
            self.command_completion = None;
            return;
        };
        let candidates = Self::path_completion_candidates(&context.fragment)
            .into_iter()
            .map(|candidate| candidate.replacement)
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            self.command_completion = None;
            return;
        }

        let selected = match direction {
            CompletionDirection::Next => 0,
            CompletionDirection::Previous => candidates.len() - 1,
        };

        self.command_completion = Some(CommandCompletionState {
            replacement_start: context.replacement_start,
            replacement_end: context.replacement_end,
            candidates,
            selected,
            needs_leading_space: context.needs_leading_space,
        });
        let candidate = self
            .command_completion
            .as_ref()
            .and_then(|state| state.candidates.get(state.selected).cloned());
        if let Some(candidate) = candidate {
            self.apply_command_completion_candidate(&candidate);
        }
    }

    fn record_command_history(&mut self, command: &str) {
        if let Err(error) = self.preferences.record_command(command) {
            log!("failed to save command history: {error}");
        }
    }

    fn handle_command_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        if let Event::Key(ref event) = ev {
            let code = event.code;
            let _modifiers = event.modifiers;

            match code {
                KeyCode::Esc => {
                    self.command = String::new();
                    self.reset_command_history_navigation();
                    self.reset_command_completion();
                    return Some(KeyAction::Single(Action::EnterMode(Mode::Normal)));
                }
                KeyCode::Backspace => {
                    Self::delete_last_char(&mut self.command);
                    self.reset_command_history_navigation();
                    self.reset_command_completion();
                }
                KeyCode::Up => {
                    self.reset_command_completion();
                    self.navigate_command_history(CommandHistoryDirection::Previous);
                }
                KeyCode::Down => {
                    self.reset_command_completion();
                    self.navigate_command_history(CommandHistoryDirection::Next);
                }
                KeyCode::Tab => {
                    self.reset_command_history_navigation();
                    self.complete_command_path(CompletionDirection::Next);
                }
                KeyCode::BackTab => {
                    self.reset_command_history_navigation();
                    self.complete_command_path(CompletionDirection::Previous);
                }
                KeyCode::Enter => {
                    self.reset_command_history_navigation();
                    self.reset_command_completion();
                    if self.command.trim().is_empty() {
                        return Some(KeyAction::Single(Action::EnterMode(Mode::Normal)));
                    }
                    return Some(KeyAction::Multiple(vec![
                        Action::EnterMode(Mode::Normal),
                        Action::Command(self.command.clone()),
                    ]));
                }
                KeyCode::Char(c) => {
                    self.command.push(c);
                    self.reset_command_history_navigation();
                    self.reset_command_completion();
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
                let modifiers = event.modifiers;

                match (code, modifiers) {
                    (KeyCode::Esc, _) => {
                        return Some(KeyAction::Single(Action::CancelSearch));
                    }
                    (KeyCode::Backspace, _) => {
                        if let Some(active_search) = &mut self.active_search {
                            Self::delete_last_char(&mut active_search.draft);
                            active_search.preview = None;
                        }
                        self.update_search_preview();
                    }
                    (KeyCode::Enter, _) => {
                        return Some(KeyAction::Single(Action::CommitSearch));
                    }
                    (KeyCode::Char('g'), KeyModifiers::CONTROL) => {
                        return Some(KeyAction::Single(Action::FindNext));
                    }
                    (KeyCode::Char('t'), KeyModifiers::CONTROL) => {
                        return Some(KeyAction::Single(Action::FindPrevious));
                    }
                    (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                        if let Some(active_search) = &mut self.active_search {
                            active_search.draft.push(c);
                        }
                        self.update_search_preview();
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
                if *c == '%' {
                    let Some(range) = self.matchit_select_around_range() else {
                        self.last_error = Some("text object not found".to_string());
                        return Some(KeyAction::None);
                    };
                    if self.select_text_range(range) {
                        return Some(KeyAction::Single(Action::Refresh));
                    }
                    self.last_error = Some("text object not found".to_string());
                    return Some(KeyAction::None);
                }
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
            return if self.pending_operator.is_some() {
                self.pending_operator_invalid()
            } else {
                None
            };
        };

        if let Some(pending) = self.pending_operator.take() {
            self.waiting_command = None;
            return self.handle_pending_operator(pending, *c);
        }

        let operator = match c {
            'd' => EditOperator::Delete,
            'c' => EditOperator::Change,
            'y' => EditOperator::Yank,
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
                'y' if pending.operator == EditOperator::Yank => {
                    Some(KeyAction::Single(Action::YankCurrentLine))
                }
                'w' => self.operator_action_for_range(
                    pending.operator,
                    self.word_motion_range(),
                    "no word under cursor",
                ),
                '%' => self.operator_action_for_range(
                    pending.operator,
                    self.matchit_motion_range(MatchDirection::Forward),
                    "match not found",
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
            EditOperator::Yank => Action::YankTextRange(range),
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

    fn matchit_motion_range(&self, direction: MatchDirection) -> Option<TextRange> {
        let start = self.cursor_text_position();
        let motion = self.matchit_motion(direction)?;
        Some(motion.range_from(start))
    }

    fn matchit_motion(&self, direction: MatchDirection) -> Option<MatchMotion> {
        matchit::find_motion(
            &self.current_buffer().contents(),
            self.cursor_text_position(),
            self.current_language_id().as_deref(),
            &self.config.matchit,
            direction,
        )
    }

    fn unmatched_matchit_motion(&self, direction: MatchDirection) -> Option<MatchMotion> {
        matchit::find_unmatched_group(
            &self.current_buffer().contents(),
            self.cursor_text_position(),
            self.current_language_id().as_deref(),
            &self.config.matchit,
            direction,
        )
    }

    fn matchit_select_around_range(&self) -> Option<TextRange> {
        matchit::select_around(
            &self.current_buffer().contents(),
            self.cursor_text_position(),
            self.current_language_id().as_deref(),
            &self.config.matchit,
        )
    }

    fn current_language_id(&self) -> Option<String> {
        self.highlighter
            .language_id_for_file(self.current_buffer().file.as_deref())
            .map(str::to_string)
            .or_else(|| self.current_buffer().file_type())
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

    fn word_under_cursor(&self) -> Option<String> {
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
        if !is_keyword_char(chars[cursor]) {
            return None;
        }

        let mut start = cursor;
        while start > 0 && is_keyword_char(chars[start - 1]) {
            start -= 1;
        }

        let mut end = cursor + 1;
        while end < chars.len() && is_keyword_char(chars[end]) {
            end += 1;
        }

        Some(chars[start..end].iter().collect())
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
                if open_idx <= cursor
                    && cursor <= idx
                    && best_pair.is_none_or(|(best_open_idx, _)| open_idx > best_open_idx)
                {
                    best_pair = Some((open_idx, idx));
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

    fn leading_indentation(line: &str) -> usize {
        line.trim_end_matches(&['\r', '\n'][..])
            .chars()
            .take_while(|c| c.is_whitespace())
            .count()
    }

    fn previous_line_indentation(&self) -> usize {
        if self.buffer_line() > 0 {
            Self::leading_indentation(
                &self
                    .current_buffer()
                    .get(self.buffer_line() - 1)
                    .unwrap_or_default(),
            )
        } else {
            0
        }
    }

    fn current_line_indentation(&self) -> usize {
        Self::leading_indentation(&self.current_line_contents().unwrap_or_default())
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
        let event_snapshot_before_action = self.event_snapshot();
        let action_cause = Self::action_cause(action);
        let history_entry_before_action = self.current_history_entry();

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
                        self.apply_cursor_goal_to_current_line();
                        self.finish_cursor_motion(buffer, true)?;
                    }
                } else {
                    self.cy = self.cy.saturating_sub(1);
                    self.apply_cursor_goal_to_current_line();
                    self.finish_cursor_motion(buffer, true)?;
                }
            }
            Action::MoveDown => {
                if self.vtop + self.cy < self.last_navigable_line() {
                    self.cy += 1;
                    if self.cy >= self.vheight() {
                        // scroll if possible
                        self.vtop += 1;
                        self.cy -= 1;
                        self.apply_cursor_goal_to_current_line();
                        self.finish_cursor_motion(buffer, true)?;
                    } else {
                        self.apply_cursor_goal_to_current_line();
                        self.finish_cursor_motion(buffer, true)?;
                    }
                } else {
                    self.finish_cursor_motion(buffer, true)?;
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
                }
                self.finish_cursor_motion(buffer, false)?;
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
                self.finish_cursor_motion(buffer, false)?;
            }
            Action::MoveToLineStart => {
                self.cx = 0;
            }
            Action::MoveToLineEnd => {
                self.cx = self.line_length().saturating_sub(1);
                self.cursor_goal = CursorGoal::LineEnd;
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
            Action::MoveScreenLineUp => {
                self.move_screen_line(-1);
                self.finish_cursor_motion(buffer, true)?;
            }
            Action::MoveScreenLineDown => {
                self.move_screen_line(1);
                self.finish_cursor_motion(buffer, true)?;
            }
            Action::MoveToScreenLineStart => {
                self.move_to_screen_line_start();
                self.finish_cursor_motion(buffer, false)?;
            }
            Action::MoveToScreenLineFirstNonBlank => {
                self.move_to_screen_line_first_non_blank();
                self.finish_cursor_motion(buffer, false)?;
            }
            Action::MoveToScreenLineEnd => {
                self.move_to_screen_line_end();
                self.finish_cursor_motion(buffer, false)?;
            }
            Action::PageUp => {
                let target_line = self
                    .buffer_line()
                    .saturating_sub(self.vheight())
                    .min(self.last_navigable_line());
                self.vtop = target_line.saturating_sub(self.vheight().saturating_sub(1));
                self.cy = target_line.saturating_sub(self.vtop);
                self.apply_cursor_goal_to_current_line();
                self.render(buffer)?;
            }
            Action::PageDown => {
                let target_line =
                    (self.buffer_line() + self.vheight()).min(self.last_navigable_line());
                self.vtop = target_line.saturating_sub(self.vheight().saturating_sub(1));
                self.cy = target_line.saturating_sub(self.vtop);
                self.apply_cursor_goal_to_current_line();
                self.render(buffer)?;
            }
            Action::EnterSearch(direction) => {
                add_to_history = false;
                self.selection = None;
                self.pending_visual_text_object_scope = None;
                self.pending_operator = None;
                self.begin_search(*direction);
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
                    self.begin_search(SearchDirection::Forward);
                    self.render(buffer)?;
                    return Ok(false);
                }

                if matches!(new_mode, Mode::Command) {
                    self.reset_command_history_navigation();
                    self.reset_command_completion();
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

                if self
                    .current_dialog
                    .as_ref()
                    .map(|dialog| dialog.allows_event_passthrough())
                    .unwrap_or(false)
                {
                    self.render(buffer)?;
                } else {
                    self.draw_line(buffer);
                }
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
            Action::YankTextRange(range) => {
                if self.yank_text_range(*range) {
                    self.draw_commandline(buffer);
                }
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
                let current_line_without_ending = current_line.trim_end_matches(&['\r', '\n'][..]);
                let current_line_for_split =
                    if current_line_without_ending.chars().all(char::is_whitespace) {
                        current_line_without_ending
                    } else {
                        current_line_without_ending.trim_end()
                    };
                let current_line_len = grapheme_len(current_line_for_split);
                if self.cx > current_line_len {
                    self.cx = current_line_len;
                }
                let cursor_char = grapheme_to_char(current_line_for_split, self.cx);
                let before_cursor = char_prefix(current_line_for_split, cursor_char).to_string();
                let after_cursor = char_suffix(current_line_for_split, cursor_char).to_string();

                let line = self.buffer_line();
                self.replace_range(
                    TextRange::new(
                        TextPosition::new(line, 0),
                        TextPosition::new(line, current_line_without_ending.chars().count()),
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
                self.skipcol = 0;
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
                log!("--------------- JUMP LIST ------------------");
                for (idx, item) in self.jump_list.iter().enumerate() {
                    let marker = if idx == self.jump_index { ">" } else { " " };
                    log!(
                        "{} {:<25} | {:>2} {:>2}",
                        marker,
                        item.file.as_deref().unwrap_or("<unnamed>"),
                        item.x,
                        item.y
                    );
                }
                if self.jump_index == self.jump_list.len() {
                    log!("> <current>");
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
                if let Some(log_file) = self.config.log_file.clone() {
                    let path = match expand_user_path(&log_file) {
                        Ok(path) => path,
                        Err(e) => {
                            self.last_error = Some(format!("Failed to open log file: {}", e));
                            return Ok(false);
                        }
                    };
                    let log_file = path.to_string_lossy().into_owned();
                    if path.exists() {
                        // Check if the log file is already open
                        if let Some(index) = self.buffers.iter().position(|b| b.name() == log_file)
                        {
                            self.set_current_buffer(buffer, index).await?;
                        } else {
                            let new_buffer = match Buffer::load_or_create(Some(log_file)).await {
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
                self.vleft = 0;
                self.skipcol = 0;
            }
            Action::Command(cmd) => {
                log!("Handling command: {cmd}");
                self.record_command_history(cmd);

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
                self.check_bounds();
                self.render(buffer)?;
            }
            Action::MoveToFilePos(file, x, y) => {
                if self.current_buffer().file != Some(file.clone()) {
                    self.execute_with_tracking(
                        &Action::OpenFile(file.clone()),
                        buffer,
                        runtime,
                        false,
                    )
                    .await?;
                }

                self.execute_with_tracking(&Action::MoveTo(*x, *y), buffer, runtime, false)
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
                self.refresh_cursor_goal();
                self.sync_to_window();
                self.ensure_current_buffer_lsp_opened().await?;
                self.render(buffer)?;
            }
            Action::ScrollUp => {
                let scroll_lines = self.config.mouse_scroll_lines.unwrap_or(3);
                let old_vtop = self.vtop;
                self.vtop = self.vtop.saturating_sub(scroll_lines);
                let scrolled_lines = old_vtop - self.vtop;
                if scrolled_lines > 0 {
                    let viewport_height = self.vheight().max(1);
                    let scrolloff = self
                        .config
                        .scrolloff
                        .unwrap_or(0)
                        .min(viewport_height.saturating_sub(1));
                    let max_cy = viewport_height.saturating_sub(scrolloff).saturating_sub(1);
                    self.cy = self.cy.saturating_add(scrolled_lines).min(max_cy);
                    self.sync_to_window();
                    self.render(buffer)?;
                }
            }
            Action::ScrollDown => {
                let scroll_lines = self.config.mouse_scroll_lines.unwrap_or(3);
                let viewport_height = self.vheight().max(1);
                let max_vtop = self
                    .last_navigable_line()
                    .saturating_sub(viewport_height.saturating_sub(1));
                let old_vtop = self.vtop;
                self.vtop = self.vtop.saturating_add(scroll_lines).min(max_vtop);
                let scrolled_lines = self.vtop - old_vtop;
                if scrolled_lines > 0 {
                    let scrolloff = self
                        .config
                        .scrolloff
                        .unwrap_or(0)
                        .min(viewport_height.saturating_sub(1));
                    self.cy = self.cy.saturating_sub(scrolled_lines).max(scrolloff);
                    self.sync_to_window();
                    self.render(buffer)?;
                }
            }
            Action::ScrollViewLeft => {
                if !self.wrap {
                    self.vleft = self.vleft.saturating_sub(self.sidescroll());
                    self.render(buffer)?;
                }
            }
            Action::ScrollViewRight => {
                if !self.wrap {
                    self.vleft = self.vleft.saturating_add(self.sidescroll());
                    self.render(buffer)?;
                }
            }
            Action::ScrollViewHalfPageLeft => {
                if !self.wrap {
                    self.vleft = self.vleft.saturating_sub(self.active_content_width() / 2);
                    self.render(buffer)?;
                }
            }
            Action::ScrollViewHalfPageRight => {
                if !self.wrap {
                    self.vleft = self.vleft.saturating_add(self.active_content_width() / 2);
                    self.render(buffer)?;
                }
            }
            Action::ScrollCursorToViewStart => {
                if !self.wrap {
                    let display_col = self.current_cursor_display_col();
                    self.vleft = display_col;
                    self.render(buffer)?;
                }
            }
            Action::ScrollCursorToViewEnd => {
                if !self.wrap {
                    let display_col = self.current_cursor_display_col();
                    self.vleft =
                        display_col.saturating_sub(self.active_content_width().saturating_sub(1));
                    self.render(buffer)?;
                }
            }
            Action::ToggleWrap => {
                self.wrap = !self.wrap;
                self.vleft = 0;
                self.skipcol = 0;
                self.render(buffer)?;
            }
            Action::SetWrap(wrap) => {
                self.wrap = *wrap;
                self.vleft = 0;
                self.skipcol = 0;
                self.render(buffer)?;
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
                    self.finish_cursor_motion(buffer, false)?;
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
                    self.finish_cursor_motion(buffer, false)?;
                }
            }
            Action::MoveToFilePercent(percent) => {
                let percent = (*percent).clamp(1, 100);
                let line_count = self.current_buffer().navigable_line_count();
                let line = (percent * line_count).div_ceil(100);
                self.go_to_line(line, buffer, runtime, GoToLinePosition::Center)
                    .await?;
                self.move_to_first_non_blank_on_current_line();
                self.finish_cursor_motion(buffer, false)?;
            }
            Action::MatchitForward => {
                self.move_to_matchit_motion(MatchDirection::Forward, buffer)?;
            }
            Action::MatchitBackward => {
                self.move_to_matchit_motion(MatchDirection::Backward, buffer)?;
            }
            Action::MatchitPreviousUnmatched => {
                self.move_to_unmatched_matchit_group(MatchDirection::Backward, buffer)?;
            }
            Action::MatchitNextUnmatched => {
                self.move_to_unmatched_matchit_group(MatchDirection::Forward, buffer)?;
            }
            Action::MatchitSelectAround => {
                if let Some(range) = self.matchit_select_around_range() {
                    if self.select_text_range(range) {
                        self.render(buffer)?;
                    }
                } else {
                    self.last_error = Some("text object not found".to_string());
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
                if self
                    .current_dialog
                    .as_ref()
                    .map(|dialog| dialog.allows_event_passthrough())
                    .unwrap_or(false)
                {
                    self.render(buffer)?;
                } else {
                    self.draw_line(buffer);
                }
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
                        let saved_file = self
                            .current_buffer()
                            .file
                            .clone()
                            .unwrap_or_else(|| new_file_name.clone());

                        // Notify plugins about file save
                        let save_info = serde_json::json!({
                            "file": saved_file,
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
            Action::CommitSearch => {
                add_to_history = false;
                let Some(session) = self.active_search.clone() else {
                    return Ok(false);
                };
                if session.draft.is_empty() {
                    self.active_search = None;
                    self.mode = Mode::Normal;
                    self.render(buffer)?;
                } else {
                    let matches = match self.search_matches(&session.draft) {
                        Ok(matches) => matches,
                        Err(err) => {
                            self.restore_search_origin(&session.origin, session.origin_vtop);
                            self.last_error = Some(err.to_string());
                            self.render(buffer)?;
                            return Ok(false);
                        }
                    };
                    let Some(match_) = self.search_match_in_direction(
                        &matches,
                        &session.origin,
                        session.direction,
                        self.config.search.wrapscan,
                    ) else {
                        self.restore_search_origin(&session.origin, session.origin_vtop);
                        self.last_error = Some(format!("pattern not found: {}", session.draft));
                        self.render(buffer)?;
                        return Ok(false);
                    };

                    self.search_term = session.draft;
                    self.search_direction = session.direction;
                    self.search_highlights_suppressed = false;
                    self.active_search = None;
                    self.mode = Mode::Normal;
                    self.move_to_search_match(match_);
                    self.save_to_history(session.origin);
                    self.render(buffer)?;
                    self.notify_search_highlighted(runtime, "CommitSearch")
                        .await?;
                }
            }
            Action::CancelSearch => {
                add_to_history = false;
                self.cancel_active_search();
                self.render(buffer)?;
            }
            Action::FindPrevious => {
                if self.active_search.is_some() {
                    add_to_history = false;
                }
                let persistent_search = self.active_search.is_none();
                if self.execute_search_direction(SearchDirection::Backward, buffer)?
                    && persistent_search
                {
                    self.notify_search_highlighted(runtime, "FindPrevious")
                        .await?;
                }
            }
            Action::FindNext => {
                if self.active_search.is_some() {
                    add_to_history = false;
                }
                let persistent_search = self.active_search.is_none();
                if self.execute_search_direction(SearchDirection::Forward, buffer)?
                    && persistent_search
                {
                    self.notify_search_highlighted(runtime, "FindNext").await?;
                }
            }
            Action::RepeatSearch => {
                if self.execute_search_direction(self.search_direction, buffer)? {
                    self.notify_search_highlighted(runtime, "RepeatSearch")
                        .await?;
                }
            }
            Action::RepeatSearchOpposite => {
                if self.execute_search_direction(self.search_direction.opposite(), buffer)? {
                    self.notify_search_highlighted(runtime, "RepeatSearchOpposite")
                        .await?;
                }
            }
            Action::ClearSearchHighlight => {
                self.search_highlights_suppressed = true;
                self.active_search = None;
                self.render(buffer)?;
                self.notify_search_cleared(runtime).await?;
            }
            Action::SearchWordUnderCursor => {
                if let Some(search_term) = self.word_under_cursor() {
                    self.search_term = search_term;
                    self.search_direction = SearchDirection::Forward;
                    self.search_highlights_suppressed = false;
                    if self.execute_search_direction(SearchDirection::Forward, buffer)? {
                        self.notify_search_highlighted(runtime, "SearchWordUnderCursor")
                            .await?;
                    }
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
                let path = match expanded_path_string(path) {
                    Ok(path) => path,
                    Err(e) => {
                        self.last_error = Some(e.to_string());
                        return Ok(false);
                    }
                };
                if let Some(index) = self.buffers.iter().position(|b| b.name() == path) {
                    self.set_current_buffer(buffer, index).await?;
                } else {
                    let new_buffer = match Buffer::load_or_create(Some(path.clone())).await {
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
            Action::ReloadFile(force) => {
                if self.current_buffer().is_dirty() && !force {
                    self.last_error =
                        Some("E37: No write since last change (add ! to override)".to_string());
                    self.render(buffer)?;
                    return Ok(false);
                }

                match self.current_buffer_mut().reload_from_file() {
                    Ok(msg) => {
                        self.last_error = Some(msg);
                        self.check_bounds();
                        self.sync_to_window();
                        self.render(buffer)?;
                    }
                    Err(e) => {
                        self.last_error = Some(e.to_string());
                        self.render(buffer)?;
                        return Ok(false);
                    }
                }
                self.notify_change(runtime).await?;
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
            Action::YankCurrentLine => {
                if self.yank_current_line() {
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
                self.request_completion(None).await?;
            }
            Action::RequestCompletionWithTrigger(trigger_character) => {
                self.request_completion(Some(*trigger_character)).await?;
            }
            Action::ApplyCompletion {
                item,
                commit_character,
            } => {
                self.apply_completion(item, *commit_character, runtime)
                    .await?;
                self.render(buffer)?;
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
                if let Some(entry) = self.jump_back_entry() {
                    log!("jumping back to {entry:?}");
                    let action = self.action_for_history_entry(&entry);
                    self.execute_with_tracking(&action, buffer, runtime, false)
                        .await?;
                }
            }
            Action::JumpForward => {
                add_to_history = false;
                if let Some(entry) = self.jump_forward_entry() {
                    log!("jumping forward to {entry:?}");
                    let action = self.action_for_history_entry(&entry);
                    self.execute_with_tracking(&action, buffer, runtime, false)
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
                let file = match expanded_path_string(file) {
                    Ok(file) => file,
                    Err(e) => {
                        self.last_error = Some(format!("Failed to open file: {}", e));
                        return Ok(false);
                    }
                };
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
                let file = match expanded_path_string(file) {
                    Ok(file) => file,
                    Err(e) => {
                        self.last_error = Some(format!("Failed to open file: {}", e));
                        return Ok(false);
                    }
                };
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
                if self.update_window_layout(WindowManager::balance_windows) {
                    self.render(buffer)?;
                }
            }
            Action::MaximizeWindow => {
                if self.update_window_layout(WindowManager::maximize_window) {
                    self.render(buffer)?;
                }
            }
            Action::OnlyWindow => {
                if self.update_window_layout(WindowManager::only_window) {
                    self.render(buffer)?;
                }
            }
        }

        let bounds_changed = self.check_bounds();

        if Self::should_refresh_cursor_goal_after(action) {
            self.refresh_cursor_goal();
        }

        if bounds_changed {
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
                    | Action::MoveToFilePercent(_)
                    | Action::MatchitForward
                    | Action::MatchitBackward
                    | Action::MatchitPreviousUnmatched
                    | Action::MatchitNextUnmatched
            )
        {
            self.update_selection();
            self.render(buffer)?;
        }

        if add_to_history && Self::records_jump(action) {
            self.save_to_history(history_entry_before_action);
        }

        // Sync editor state back to the active window after executing actions
        // This ensures window state is updated even for actions that don't trigger a full render
        self.sync_to_window();

        // Always render after actions when in multi-window mode to ensure changes are visible
        if self.window_manager.windows().len() > 1 {
            self.render(buffer)?;
        }

        self.notify_editor_event_changes(event_snapshot_before_action, runtime, &action_cause)
            .await?;

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

    fn current_history_entry(&self) -> HistoryEntry {
        HistoryEntry::new(self.current_file_name(), self.cx, self.buffer_line())
    }

    fn records_jump(action: &Action) -> bool {
        matches!(
            action,
            Action::FindNext
                | Action::FindPrevious
                | Action::RepeatSearch
                | Action::RepeatSearchOpposite
                | Action::SearchWordUnderCursor
                | Action::PageDown
                | Action::PageUp
                | Action::MoveToBottom
                | Action::MoveToTop
                | Action::MoveToFilePercent(_)
                | Action::MatchitForward
                | Action::MatchitBackward
                | Action::MatchitPreviousUnmatched
                | Action::MatchitNextUnmatched
                | Action::MoveTo(_, _)
                | Action::MoveToFilePos(_, _, _)
                | Action::OpenFile(_)
                | Action::NextBuffer
                | Action::PreviousBuffer
        )
    }

    fn action_for_history_entry(&self, entry: &HistoryEntry) -> Action {
        match &entry.file {
            Some(file) if self.current_buffer().file.as_ref() != Some(file) => {
                Action::MoveToFilePos(file.clone(), entry.x, entry.y + 1)
            }
            _ => Action::MoveTo(entry.x, entry.y + 1),
        }
    }

    fn save_to_history(&mut self, entry: HistoryEntry) {
        let current = self.current_history_entry();
        if entry.same_location(&current) {
            return;
        }

        if self.jump_index < self.jump_list.len() {
            self.jump_list.truncate(self.jump_index + 1);
        }

        if let Some(prev) = self.jump_list.last() {
            if !entry.moved_from(prev) {
                self.jump_index = self.jump_list.len();
                return;
            }
        }

        self.push_history_entry(entry);
    }

    fn push_current_history_entry(&mut self) {
        let entry = self.current_history_entry();
        if self
            .jump_list
            .last()
            .is_some_and(|prev| prev.same_location(&entry))
        {
            self.jump_index = self.jump_list.len();
            return;
        }
        self.push_history_entry(entry);
    }

    fn push_history_entry(&mut self, entry: HistoryEntry) {
        self.jump_list.push(entry);
        if self.jump_list.len() > JUMPLIST_SIZE {
            self.jump_list.remove(0);
        }
        self.jump_index = self.jump_list.len();
    }

    fn jump_back_entry(&mut self) -> Option<HistoryEntry> {
        if self.jump_index == self.jump_list.len() {
            if self.jump_list.is_empty() {
                return None;
            }
            self.push_current_history_entry();
            if self.jump_list.len() < 2 {
                return None;
            }
            self.jump_index = self.jump_list.len() - 2;
        } else {
            if self.jump_index == 0 {
                return None;
            }
            self.jump_index -= 1;
        }

        self.jump_list.get(self.jump_index).cloned()
    }

    fn jump_forward_entry(&mut self) -> Option<HistoryEntry> {
        if self.jump_list.is_empty() || self.jump_index >= self.jump_list.len().saturating_sub(1) {
            return None;
        }

        self.jump_index += 1;
        self.jump_list.get(self.jump_index).cloned()
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

    fn yank_current_line(&mut self) -> bool {
        let Some(line) = self.current_buffer().get(self.buffer_line()) else {
            return false;
        };

        self.registers
            .insert(DEFAULT_REGISTER, Content::linewise(line));
        true
    }

    fn yank_text_range(&mut self, range: TextRange) -> bool {
        let text = self.current_buffer().text_in_range(range);
        if text.is_empty() {
            return false;
        }

        self.registers
            .insert(DEFAULT_REGISTER, Content::charwise(text));
        true
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

    async fn notify_search_highlighted(
        &mut self,
        runtime: &mut Runtime,
        source: &str,
    ) -> anyhow::Result<()> {
        let payload = serde_json::json!({
            "term": &self.search_term,
            "direction": format!("{:?}", self.search_direction),
            "source": source,
        });
        self.plugin_registry
            .notify(runtime, "search:highlighted", payload)
            .await?;
        Ok(())
    }

    async fn notify_search_cleared(&mut self, runtime: &mut Runtime) -> anyhow::Result<()> {
        let payload = serde_json::json!({
            "term": &self.search_term,
        });
        self.plugin_registry
            .notify(runtime, "search:cleared", payload)
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
        self.vleft = 0;
        self.skipcol = 0;
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
            self.skipcol = 0;
            self.vx = self.gutter_width() + 1;
            self.prev_highlight_y = None;

            for window in self.window_manager.windows_mut() {
                window.buffer_index = 0;
                window.cx = 0;
                window.cy = 0;
                window.cursor_goal = CursorGoal::default();
                window.vtop = 0;
                window.vleft = 0;
                window.skipcol = 0;
                window.wrap = self.wrap;
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
                window.cursor_goal = CursorGoal::default();
                window.vtop = target_vtop;
                window.vleft = 0;
                window.skipcol = 0;
                window.wrap = self.wrap;
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
            self.skipcol = 0;
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

    fn plugin_document_symbols_payload(&self, response: &ResponseMessage) -> anyhow::Result<Value> {
        let file = response_text_document_uri(response)
            .map(|uri| self.uri_to_file(uri))
            .or_else(|| self.current_file_name())
            .ok_or_else(|| anyhow::anyhow!("document symbol response did not include a file"))?;
        let symbols = self.normalize_document_symbols(&response.result, &file)?;

        Ok(json!({
            "ok": true,
            "file": file,
            "symbols": symbols,
        }))
    }

    fn plugin_inlay_hints_payload(&self, response: &ResponseMessage) -> anyhow::Result<Value> {
        let file = response_text_document_uri(response)
            .map(|uri| self.uri_to_file(uri))
            .or_else(|| self.current_file_name())
            .ok_or_else(|| anyhow::anyhow!("inlay hint response did not include a file"))?;
        let hints = self.normalize_inlay_hints(&response.result)?;

        Ok(json!({
            "ok": true,
            "file": file,
            "hints": hints,
        }))
    }

    fn normalize_inlay_hints(&self, result: &Value) -> anyhow::Result<Vec<InlayHint>> {
        if result.is_null() {
            return Ok(Vec::new());
        }

        serde_json::from_value(result.clone()).map_err(Into::into)
    }

    fn normalize_document_symbols(
        &self,
        result: &Value,
        fallback_file: &str,
    ) -> anyhow::Result<Vec<PluginDocumentSymbol>> {
        if result.is_null() {
            return Ok(Vec::new());
        }

        let values = result
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("document symbol response was not an array"))?;
        let mut symbols = Vec::new();
        for value in values {
            self.push_normalized_symbol(value, fallback_file, 0, &mut symbols)?;
        }
        Ok(symbols)
    }

    fn push_normalized_symbol(
        &self,
        value: &Value,
        fallback_file: &str,
        depth: usize,
        symbols: &mut Vec<PluginDocumentSymbol>,
    ) -> anyhow::Result<()> {
        if value.get("location").is_some() {
            symbols.push(self.normalized_symbol_information(value, depth)?);
            return Ok(());
        }

        let symbol = normalized_document_symbol(value, fallback_file, depth)?;
        symbols.push(symbol);

        if let Some(children) = value.get("children").and_then(Value::as_array) {
            for child in children {
                self.push_normalized_symbol(child, fallback_file, depth + 1, symbols)?;
            }
        }

        Ok(())
    }

    fn normalized_symbol_information(
        &self,
        value: &Value,
        depth: usize,
    ) -> anyhow::Result<PluginDocumentSymbol> {
        let location = value
            .get("location")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("symbol information did not include a location"))?;
        let uri = required_string(location, "uri")?;
        let range = required_range(location.get("range"), "location.range")?;
        let kind = required_kind(value)?;

        Ok(PluginDocumentSymbol {
            name: required_string_value(value, "name")?.to_string(),
            detail: value
                .get("containerName")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            kind,
            kind_name: symbol_kind_name(kind).to_string(),
            file: self.uri_to_file(uri),
            range: range.clone(),
            selection_range: range,
            depth,
        })
    }

    fn uri_to_file(&self, uri: &str) -> String {
        let prefix = format!("{}/", get_workspace_uri());
        if let Some(file) = uri.strip_prefix(&prefix) {
            return file.to_string();
        }

        if let Some(file) = uri.strip_prefix("file://") {
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
        if let Event::Key(KeyEvent {
            code: KeyCode::Char('%'),
            modifiers: KeyModifiers::NONE | KeyModifiers::SHIFT,
            ..
        }) = ev
        {
            if let Some(percent) = self.repeater.take() {
                return Some(KeyAction::Single(Action::MoveToFilePercent(
                    percent as usize,
                )));
            }
        }

        if self.handle_repeater(ev) {
            return None;
        }

        let key_action = match ev {
            event::Event::Key(KeyEvent {
                code, modifiers, ..
            }) => {
                let key = match code {
                    KeyCode::Char(' ') if *modifiers == KeyModifiers::NONE => " ".to_string(),
                    KeyCode::Char(' ') => "Space".to_string(),
                    KeyCode::Char(c) => format!("{c}"),
                    _ => format!("{code:?}"),
                };

                let key = match *modifiers {
                    KeyModifiers::CONTROL => format!("Ctrl-{key}"),
                    KeyModifiers::ALT => format!("Alt-{key}"),
                    _ => key,
                };

                mappings
                    .get(&key)
                    .cloned()
                    .or_else(|| {
                        (matches!(code, KeyCode::Char(' ')) && *modifiers == KeyModifiers::NONE)
                            .then(|| mappings.get("Space").cloned())
                            .flatten()
                    })
                    .or_else(|| {
                        matches!(code, KeyCode::Tab)
                            .then(|| mappings.get("Tab").cloned())
                            .flatten()
                    })
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
                                let content_x = local_x.saturating_sub(gutter_width + 1);
                                let layout = self.layout_for_window(&window);
                                let (buffer_x, buffer_y) =
                                    if let Some(segment) = layout.row(local_y) {
                                        let display_col = segment.start_col + content_x;
                                        let line = self.buffers[window_buffer_index]
                                            .get(segment.line)
                                            .unwrap_or_default();
                                        (
                                            column_to_grapheme(
                                                line.trim_end_matches('\n'),
                                                display_col,
                                            ),
                                            segment.line,
                                        )
                                    } else {
                                        (content_x, window_vtop + local_y)
                                    };

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

    fn move_to_matchit_motion(
        &mut self,
        direction: MatchDirection,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<()> {
        if let Some(motion) = self.matchit_motion(direction) {
            self.move_to_text_position(motion.target);
            self.finish_cursor_motion(buffer, false)?;
        } else {
            self.last_error = Some("match not found".to_string());
        }
        Ok(())
    }

    fn move_to_unmatched_matchit_group(
        &mut self,
        direction: MatchDirection,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<()> {
        if let Some(motion) = self.unmatched_matchit_motion(direction) {
            self.move_to_text_position(motion.target);
            self.finish_cursor_motion(buffer, false)?;
        } else {
            self.last_error = Some("match not found".to_string());
        }
        Ok(())
    }

    fn move_to_first_non_blank_on_current_line(&mut self) {
        if let Some(line) = self.current_line_contents() {
            self.cx = line
                .trim_end_matches('\n')
                .graphemes(true)
                .position(|grapheme| !grapheme.chars().all(char::is_whitespace))
                .unwrap_or(0);
        }
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

        self.recompute_window_cursor_goals();

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

        self.ensure_cursor_visible();
    }

    fn ensure_cursor_visible(&mut self) {
        let width = self.active_content_width();
        if width == 0 {
            return;
        }

        let buffer_line = self.buffer_line();
        let line = self.current_line_contents().unwrap_or_default();
        let line = line.trim_end_matches('\n');
        let display_col = grapheme_to_column(line, self.cx);

        if !self.wrap {
            self.skipcol = 0;
            let off = self.sidescrolloff(width);
            let right_edge = self.vleft + width;
            if display_col < self.vleft + off {
                self.vleft = display_col.saturating_sub(off + self.sidescroll().saturating_sub(1));
            } else if display_col >= right_edge.saturating_sub(off) {
                self.vleft = display_col
                    .saturating_add(off)
                    .saturating_add(self.sidescroll())
                    .saturating_sub(width);
            }
            return;
        }

        self.vleft = 0;
        let height = self.vheight().max(1);
        if buffer_line < self.vtop {
            self.vtop = buffer_line;
            self.skipcol = 0;
        }

        if buffer_line == self.vtop {
            let target_segment = display_col / width;
            let first_segment = self.skipcol / width;
            if target_segment < first_segment {
                self.skipcol = target_segment * width;
            } else if target_segment >= first_segment + height {
                self.skipcol = target_segment
                    .saturating_sub(height.saturating_sub(1))
                    .saturating_mul(width);
            }
        } else {
            let mut visible = false;
            if let Some(window) = self.active_window_with_editor_view() {
                let layout = self.layout_for_window(&window);
                visible = layout
                    .rows
                    .iter()
                    .any(|segment| segment.line == buffer_line);
            }

            if !visible {
                self.vtop = buffer_line;
                let target_segment = display_col / width;
                self.skipcol = target_segment
                    .saturating_sub(height.saturating_sub(1))
                    .saturating_mul(width);
            }
        }

        self.cy = buffer_line.saturating_sub(self.vtop);
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
            Action::RequestCompletionWithTrigger(c),
        ])))
    }

    async fn request_completion(&mut self, trigger_character: Option<char>) -> anyhow::Result<()> {
        if !self.is_insert() {
            return Ok(());
        }

        if let Some(uri) = self.current_buffer().uri()? {
            self.ensure_current_buffer_lsp_opened().await?;
            let line = self.buffer_line();
            let character = self.grapheme_to_char_on_line(self.cx, line);
            self.lsp
                .request_completion(&uri, line, character, trigger_character)
                .await?;
        }

        Ok(())
    }

    async fn apply_completion(
        &mut self,
        item: &CompletionResponseItem,
        commit_character: Option<char>,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let resume_insert_transaction = self.transaction_active();
        if resume_insert_transaction {
            self.commit_transaction(self.cursor_snapshot());
        }

        self.begin_transaction("apply completion");

        let mut edits = Vec::new();
        if let Some(text_edit) = &item.text_edit {
            edits.push(completion_edit_from_lsp(
                text_edit,
                item.insert_text_format.as_ref(),
                true,
            ));
        } else {
            let text = item.insert_text.as_deref().unwrap_or(&item.label);
            let line = self.buffer_line();
            let character = self.grapheme_to_char_on_line(self.cx, line);
            edits.push(completion_edit(
                TextRange::insertion(TextPosition::new(line, character)),
                text,
                item.insert_text_format.as_ref(),
                true,
            ));
        }
        if let Some(additional_text_edits) = &item.additional_text_edits {
            for text_edit in additional_text_edits {
                edits.push(completion_edit_from_lsp(text_edit, None, false));
            }
        }

        edits.sort_by(|a, b| compare_text_positions_desc(a.range.start, b.range.start));

        let mut cursor_position = None;
        for edit in edits {
            self.replace_range(edit.range, &edit.new_text);

            if edit.is_main {
                let cursor_offset = edit
                    .cursor_offset
                    .unwrap_or_else(|| edit.new_text.chars().count());
                cursor_position = Some(offset_text_position(
                    edit.range.start,
                    &edit.new_text,
                    cursor_offset,
                ));
            } else if let Some(cursor) = cursor_position {
                cursor_position = Some(transform_text_position_after_edit(
                    cursor,
                    edit.range,
                    &edit.new_text,
                ));
            }
        }

        let cursor_position = cursor_position.unwrap_or_else(|| self.cursor_text_position());
        self.move_to_text_position(cursor_position);

        if let Some(c) = commit_character {
            let line = self.buffer_line();
            let character = self.grapheme_to_char_on_line(self.cx, line);
            self.replace_range(
                TextRange::insertion(TextPosition::new(line, character)),
                &c.to_string(),
            );
            self.move_to_text_position(TextPosition::new(line, character + 1));
        }

        self.notify_change(runtime).await?;
        self.commit_transaction(self.cursor_snapshot());

        if resume_insert_transaction && self.is_insert() {
            self.begin_transaction("insert");
        }

        if let Some(command) = &item.command {
            self.execute_lsp_command(command).await?;
        }

        Ok(())
    }

    async fn execute_lsp_command(&mut self, command: &LspCommand) -> anyhow::Result<()> {
        let params = json!({
            "command": command.command,
            "arguments": command.arguments.clone().unwrap_or_default(),
        });

        self.lsp
            .send_request("workspace/executeCommand", params, false)
            .await?;

        Ok(())
    }
}

#[derive(Debug)]
struct CompletionEdit {
    range: TextRange,
    new_text: String,
    cursor_offset: Option<usize>,
    is_main: bool,
}

fn completion_edit_from_lsp(
    text_edit: &LspTextEdit,
    insert_text_format: Option<&InsertTextFormat>,
    is_main: bool,
) -> CompletionEdit {
    completion_edit(
        text_range_from_lsp(&text_edit.range),
        &text_edit.new_text,
        insert_text_format,
        is_main,
    )
}

fn completion_edit(
    range: TextRange,
    text: &str,
    insert_text_format: Option<&InsertTextFormat>,
    is_main: bool,
) -> CompletionEdit {
    let (new_text, cursor_offset) = if matches!(insert_text_format, Some(InsertTextFormat::Snippet))
    {
        snippet_to_plain_text(text)
    } else {
        (text.to_string(), None)
    };

    CompletionEdit {
        range,
        new_text,
        cursor_offset,
        is_main,
    }
}

fn text_range_from_lsp(range: &crate::lsp::Range) -> TextRange {
    TextRange::new(
        TextPosition::new(range.start.line, range.start.character),
        TextPosition::new(range.end.line, range.end.character),
    )
}

fn response_text_document_uri(response: &ResponseMessage) -> Option<&str> {
    response
        .request
        .as_ref()?
        .params
        .as_object()?
        .get("textDocument")?
        .as_object()?
        .get("uri")?
        .as_str()
}

fn normalized_document_symbol(
    value: &Value,
    file: &str,
    depth: usize,
) -> anyhow::Result<PluginDocumentSymbol> {
    let range = required_range(value.get("range"), "range")?;
    let selection_range = required_range(value.get("selectionRange"), "selectionRange")
        .unwrap_or_else(|_| range.clone());
    let kind = required_kind(value)?;

    Ok(PluginDocumentSymbol {
        name: required_string_value(value, "name")?.to_string(),
        detail: value
            .get("detail")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        kind,
        kind_name: symbol_kind_name(kind).to_string(),
        file: file.to_string(),
        range,
        selection_range,
        depth,
    })
}

fn required_string<'a>(value: &'a Map<String, Value>, key: &str) -> anyhow::Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing string field `{key}`"))
}

fn required_string_value<'a>(value: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing string field `{key}`"))
}

fn required_kind(value: &Value) -> anyhow::Result<i32> {
    let kind = value
        .get("kind")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow::anyhow!("missing numeric field `kind`"))?;
    if (1..=26).contains(&kind) {
        Ok(kind as i32)
    } else {
        Err(anyhow::anyhow!("invalid symbol kind `{kind}`"))
    }
}

fn required_range(value: Option<&Value>, label: &str) -> anyhow::Result<Range> {
    let value = value.ok_or_else(|| anyhow::anyhow!("missing range field `{label}`"))?;
    Ok(serde_json::from_value(value.clone())?)
}

fn symbol_kind_name(kind: i32) -> &'static str {
    match kind {
        1 => "File",
        2 => "Module",
        3 => "Namespace",
        4 => "Package",
        5 => "Class",
        6 => "Method",
        7 => "Property",
        8 => "Field",
        9 => "Constructor",
        10 => "Enum",
        11 => "Interface",
        12 => "Function",
        13 => "Variable",
        14 => "Constant",
        15 => "String",
        16 => "Number",
        17 => "Boolean",
        18 => "Array",
        19 => "Object",
        20 => "Key",
        21 => "Null",
        22 => "EnumMember",
        23 => "Struct",
        24 => "Event",
        25 => "Operator",
        26 => "TypeParameter",
        _ => "Unknown",
    }
}

fn compare_text_positions_desc(a: TextPosition, b: TextPosition) -> Ordering {
    b.line.cmp(&a.line).then(b.character.cmp(&a.character))
}

fn offset_text_position(start: TextPosition, text: &str, char_offset: usize) -> TextPosition {
    let mut line = start.line;
    let mut character = start.character;

    for c in text.chars().take(char_offset) {
        if c == '\n' {
            line += 1;
            character = 0;
        } else {
            character += 1;
        }
    }

    TextPosition::new(line, character)
}

fn transform_text_position_after_edit(
    position: TextPosition,
    range: TextRange,
    new_text: &str,
) -> TextPosition {
    if compare_text_positions(position, range.start).is_lt() {
        return position;
    }

    let new_end = offset_text_position(range.start, new_text, new_text.chars().count());
    if compare_text_positions(position, range.end).is_le() {
        return new_end;
    }

    if position.line == range.end.line {
        return TextPosition::new(
            new_end.line,
            new_end
                .character
                .saturating_add(position.character.saturating_sub(range.end.character)),
        );
    }

    let old_lines = range.end.line.saturating_sub(range.start.line);
    let new_lines = new_end.line.saturating_sub(range.start.line);
    let line = if new_lines >= old_lines {
        position.line.saturating_add(new_lines - old_lines)
    } else {
        position.line.saturating_sub(old_lines - new_lines)
    };

    TextPosition::new(line, position.character)
}

fn compare_text_positions(a: TextPosition, b: TextPosition) -> Ordering {
    a.line.cmp(&b.line).then(a.character.cmp(&b.character))
}

fn snippet_to_plain_text(snippet: &str) -> (String, Option<usize>) {
    let chars = snippet.chars().collect::<Vec<_>>();
    let mut output = String::new();
    let mut first_placeholder = None;
    let mut final_cursor = None;
    let mut i = 0;

    while i < chars.len() {
        if chars[i] != '$' {
            output.push(chars[i]);
            i += 1;
            continue;
        }

        if i + 1 >= chars.len() {
            output.push(chars[i]);
            i += 1;
            continue;
        }

        match chars[i + 1] {
            '$' => {
                output.push('$');
                i += 2;
            }
            '0' => {
                final_cursor = Some(output.chars().count());
                i += 2;
            }
            c if c.is_ascii_digit() => {
                first_placeholder.get_or_insert(output.chars().count());
                i += 2;
            }
            '{' => {
                if let Some((next, index, default_text)) = parse_snippet_placeholder(&chars, i + 2)
                {
                    let cursor = output.chars().count();
                    if index == 0 {
                        final_cursor = Some(cursor);
                    } else {
                        first_placeholder.get_or_insert(cursor);
                    }
                    output.push_str(&default_text);
                    i = next;
                } else {
                    output.push(chars[i]);
                    i += 1;
                }
            }
            _ => {
                output.push(chars[i]);
                i += 1;
            }
        }
    }

    let cursor = first_placeholder.or(final_cursor);
    (output, cursor)
}

fn parse_snippet_placeholder(chars: &[char], start: usize) -> Option<(usize, usize, String)> {
    let mut i = start;
    let mut index = String::new();
    while i < chars.len() && chars[i].is_ascii_digit() {
        index.push(chars[i]);
        i += 1;
    }

    if index.is_empty() {
        return None;
    }

    let index = index.parse::<usize>().ok()?;
    let mut default_text = String::new();

    match chars.get(i) {
        Some('}') => Some((i + 1, index, default_text)),
        Some(':') => {
            i += 1;
            while i < chars.len() && chars[i] != '}' {
                default_text.push(chars[i]);
                i += 1;
            }
            (i < chars.len()).then_some((i + 1, index, default_text))
        }
        _ => None,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginDocumentSymbol {
    pub name: String,
    pub detail: Option<String>,
    pub kind: i32,
    pub kind_name: String,
    pub file: String,
    pub range: Range,
    pub selection_range: Range,
    pub depth: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct EditorInfo {
    buffers: Vec<BufferInfo>,
    theme: Theme,
    size: (u16, u16),
    vtop: usize,
    vleft: usize,
    skipcol: usize,
    wrap: bool,
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
            skipcol: editor.skipcol,
            wrap: editor.wrap,
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

fn git_status_listing(path: &str) -> Value {
    let search_dir = git_search_dir(path);
    let root_output = Command::new("git")
        .arg("-C")
        .arg(&search_dir)
        .args(["rev-parse", "--show-toplevel"])
        .output();

    let root_output = match root_output {
        Ok(output) if output.status.success() => output,
        Ok(_) => {
            return json!({
                "root": null,
                "statuses": [],
                "error": null,
            });
        }
        Err(err) => {
            return json!({
                "root": null,
                "statuses": [],
                "error": err.to_string(),
            });
        }
    };

    let root = String::from_utf8_lossy(&root_output.stdout)
        .trim()
        .to_string();
    if root.is_empty() {
        return json!({
            "root": null,
            "statuses": [],
            "error": null,
        });
    }

    let status_output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args([
            "status",
            "--porcelain=v1",
            "-z",
            "--ignored=matching",
            "--untracked-files=normal",
        ])
        .output();

    match status_output {
        Ok(output) if output.status.success() => {
            let statuses = parse_git_status_records(&output.stdout, &root);
            json!({
                "root": normalize_plugin_path(&root),
                "statuses": statuses,
                "error": null,
            })
        }
        Ok(output) => json!({
            "root": normalize_plugin_path(&root),
            "statuses": [],
            "error": String::from_utf8_lossy(&output.stderr).trim(),
        }),
        Err(err) => json!({
            "root": normalize_plugin_path(&root),
            "statuses": [],
            "error": err.to_string(),
        }),
    }
}

fn git_search_dir(path: &str) -> String {
    let path = Path::new(path);
    if path.is_dir() {
        return path.to_string_lossy().into_owned();
    }
    path.parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string())
}

fn parse_git_status_records(output: &[u8], root: &str) -> Vec<Value> {
    let mut statuses = Vec::new();
    let records = output
        .split(|byte| *byte == b'\0')
        .filter(|record| !record.is_empty())
        .collect::<Vec<_>>();
    let mut index = 0;

    while index < records.len() {
        let record = records[index];
        if record.len() < 4 {
            index += 1;
            continue;
        }

        let x = record[0] as char;
        let y = record[1] as char;
        let status = classify_git_status(x, y);
        let path = normalize_plugin_path(&String::from_utf8_lossy(&record[3..]));
        let absolute_path = normalize_plugin_path(&Path::new(root).join(&path).to_string_lossy());
        statuses.push(json!({
            "path": path,
            "absolute_path": absolute_path,
            "status": status,
        }));

        index += if matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C') {
            2
        } else {
            1
        };
    }

    statuses
}

fn normalize_plugin_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn classify_git_status(x: char, y: char) -> &'static str {
    if x == '?' && y == '?' {
        return "untracked";
    }
    if x == '!' && y == '!' {
        return "ignored";
    }
    if matches!(x, 'U' | 'A' | 'D') && matches!(y, 'U' | 'A' | 'D') {
        return "conflict";
    }
    if matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C') {
        return "renamed";
    }
    if x == 'D' || y == 'D' {
        return "deleted";
    }
    if x == 'A' || y == 'A' {
        return "added";
    }
    if matches!(x, 'M' | 'T') || matches!(y, 'M' | 'T') {
        return "modified";
    }
    if x != ' ' {
        return "staged";
    }
    "modified"
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
    pub async fn test_request_document_symbols(&mut self) -> anyhow::Result<i64> {
        let Some(file) = self.current_buffer().file.clone() else {
            return Ok(0);
        };
        self.ensure_current_buffer_lsp_opened().await?;
        Ok(self.lsp.document_symbols(&file).await?)
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
    pub fn test_vleft(&self) -> usize {
        self.vleft
    }

    #[doc(hidden)]
    pub fn test_skipcol(&self) -> usize {
        self.skipcol
    }

    #[doc(hidden)]
    pub fn test_wrap(&self) -> bool {
        self.wrap
    }

    #[doc(hidden)]
    pub fn test_set_viewport_cursor(&mut self, vtop: usize, cx: usize, cy: usize) {
        self.vtop = vtop;
        self.cx = cx;
        self.cy = cy;
    }

    #[doc(hidden)]
    pub fn test_active_window_id(&self) -> usize {
        self.window_manager.active_window_id()
    }

    #[doc(hidden)]
    pub fn test_window_count(&self) -> usize {
        self.window_manager.windows().len()
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
    pub fn test_update_panel(&mut self, id: &str, rows: Vec<plugin::PanelRow>) {
        self.panel_manager.update_panel(id, rows);
    }

    #[doc(hidden)]
    pub fn test_focus_panel(&mut self, id: &str) -> bool {
        self.panel_manager.focus_panel(id)
    }

    #[doc(hidden)]
    pub fn test_focused_panel_id(&self) -> Option<&str> {
        self.panel_manager.focused_panel_id()
    }

    #[doc(hidden)]
    pub fn test_focused_panel_selected_index(&self, id: &str) -> Option<usize> {
        self.panel_manager.selected_index(id)
    }

    #[doc(hidden)]
    pub fn test_close_panel(&mut self, id: &str) {
        self.panel_manager.close_panel(id);
        self.apply_panel_layout();
        self.sync_with_window();
    }

    #[doc(hidden)]
    pub fn test_render_cursor_position(&self) -> Option<(usize, usize)> {
        self.render_cursor_position()
    }

    #[doc(hidden)]
    pub fn test_is_waiting_for_key_sequence(&self) -> bool {
        self.is_waiting_for_key_sequence()
    }

    #[doc(hidden)]
    pub fn test_set_commandline(&mut self, mode: Mode, text: &str) {
        self.mode = mode;
        self.reset_command_completion();
        match mode {
            Mode::Command => self.command = text.to_string(),
            Mode::Search => self.search_term = text.to_string(),
            _ => {}
        }
    }

    #[doc(hidden)]
    pub fn test_complete_command_path_next(&mut self) {
        self.complete_command_path(CompletionDirection::Next);
    }

    #[doc(hidden)]
    pub fn test_complete_command_path_previous(&mut self) {
        self.complete_command_path(CompletionDirection::Previous);
    }

    #[doc(hidden)]
    pub fn test_commandline_text(&self) -> &str {
        match self.mode {
            Mode::Command => &self.command,
            Mode::Search => self.active_search_text().unwrap_or(&self.search_term),
            _ => "",
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
    pub fn test_render_cell_bg(&mut self, x: usize, y: usize) -> anyhow::Result<Option<Color>> {
        let mut render_buffer = RenderBuffer::new(
            self.size.0 as usize,
            self.size.1 as usize,
            &Style::default(),
        );
        self.render(&mut render_buffer)?;

        Ok(render_buffer
            .cells
            .get(y * render_buffer.width + x)
            .and_then(|cell| cell.style.bg))
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
    use std::path::PathBuf;

    static EVENT_RECORDER_TEST_LOCK: Lazy<tokio::sync::Mutex<()>> =
        Lazy::new(|| tokio::sync::Mutex::new(()));

    fn drain_plugin_requests() {
        while ACTION_DISPATCHER.try_recv_request().is_some() {}
    }

    fn collect_print_requests() -> Vec<String> {
        let mut prints = Vec::new();
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::Action(Action::Print(message)) = request {
                prints.push(message);
            }
        }
        prints
    }

    async fn install_event_recorder(editor: &mut Editor, runtime: &mut Runtime) {
        drain_plugin_requests();
        let plugin_path =
            std::env::temp_dir().join(format!("red-event-recorder-{}.js", uuid::Uuid::new_v4()));
        std::fs::write(
            &plugin_path,
            r#"
                export function activate(red) {
                    red.on("cursor:moved", (event) => {
                        red.execute("Print", `cursor:${event.cause}:${event.from.x},${event.from.y}->${event.to.x},${event.to.y}:${event.mode}`);
                    });
                    red.on("mode:changed", (event) => {
                        red.execute("Print", `mode:${event.cause}:${event.from}->${event.to}`);
                    });
                    red.on("search:highlighted", (event) => {
                        red.execute("Print", `search:${event.source}:${event.term}:${event.direction}`);
                    });
                    red.on("search:cleared", (event) => {
                        red.execute("Print", `cleared:${event.term}`);
                    });
                }
            "#,
        )
        .unwrap();

        editor
            .plugin_registry
            .add("event_recorder", plugin_path.to_string_lossy().as_ref());
        editor.plugin_registry.initialize(runtime).await.unwrap();
        drain_plugin_requests();
    }

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

    #[test]
    fn plugin_decorations_render_in_leading_whitespace() {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(None, "fn main() {\n    let x = 1;\n}".to_string());
        let mut editor =
            Editor::with_size(lsp, 30, 8, config, Theme::default(), vec![buffer]).unwrap();
        editor.test_disable_terminal_output();
        let layout = editor.plugin_viewport_layout_payload();
        let content_start = layout["contentStart"].as_u64().unwrap() as usize;
        let style = Style {
            fg: Some(Color::Rgb {
                r: 120,
                g: 120,
                b: 120,
            }),
            ..Style::default()
        };

        editor.decoration_manager.set(
            "guides".to_string(),
            vec![crate::plugin::Decoration {
                buffer_index: Some(0),
                anchor: crate::plugin::DecorationAnchor::Column,
                line: 1,
                column: 0,
                text: "xxxxxx".to_string(),
                style: style.clone(),
                priority: 1,
                repeat_linebreak: true,
                only_whitespace: true,
            }],
        );

        let mut render_buffer = RenderBuffer::new(30, 8, &Style::default());
        editor.render(&mut render_buffer).unwrap();

        assert_eq!(render_buffer.cells[30 + content_start].c, 'x');
        assert_eq!(render_buffer.cells[30 + content_start + 3].c, 'x');
        assert_eq!(render_buffer.cells[30 + content_start + 4].c, 'l');
    }

    #[test]
    fn plugin_decorations_render_on_blank_lines() {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(None, "fn main() {\n\n    let x = 1;\n}".to_string());
        let mut editor =
            Editor::with_size(lsp, 30, 8, config, Theme::default(), vec![buffer]).unwrap();
        editor.test_disable_terminal_output();
        let layout = editor.plugin_viewport_layout_payload();
        let content_start = layout["contentStart"].as_u64().unwrap() as usize;
        let style = Style {
            fg: Some(Color::Rgb {
                r: 120,
                g: 120,
                b: 120,
            }),
            ..Style::default()
        };

        editor.decoration_manager.set(
            "guides".to_string(),
            vec![crate::plugin::Decoration {
                buffer_index: Some(0),
                anchor: crate::plugin::DecorationAnchor::Column,
                line: 1,
                column: 0,
                text: "x   ".to_string(),
                style,
                priority: 1,
                repeat_linebreak: true,
                only_whitespace: true,
            }],
        );

        let mut render_buffer = RenderBuffer::new(30, 8, &Style::default());
        editor.render(&mut render_buffer).unwrap();

        assert_eq!(render_buffer.cells[30 + content_start].c, 'x');
    }

    #[test]
    fn parse_git_status_records_normalizes_statuses() {
        let output = b" M src/editor.rs\0?? plugins/neotree.js\0!! target/\0";
        let statuses = parse_git_status_records(output, "/repo");

        assert_eq!(statuses[0]["path"], "src/editor.rs");
        assert_eq!(statuses[0]["absolute_path"], "/repo/src/editor.rs");
        assert_eq!(statuses[0]["status"], "modified");
        assert_eq!(statuses[1]["status"], "untracked");
        assert_eq!(statuses[2]["status"], "ignored");
    }

    #[test]
    fn parse_git_status_records_skips_rename_source() {
        let output = b"R  new-name.rs\0old-name.rs\0";
        let statuses = parse_git_status_records(output, "/repo");

        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0]["path"], "new-name.rs");
        assert_eq!(statuses[0]["status"], "renamed");
    }

    fn test_home_dir() -> PathBuf {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .expect("HOME or USERPROFILE should be set for tests")
    }

    fn completion_item(label: &str) -> CompletionResponseItem {
        CompletionResponseItem {
            label: label.to_string(),
            kind: None,
            detail: None,
            documentation: None,
            deprecated: None,
            preselect: None,
            sort_text: None,
            filter_text: None,
            insert_text: None,
            insert_text_format: None,
            text_edit: None,
            additional_text_edits: None,
            command: None,
            data: None,
            commit_characters: None,
        }
    }

    #[tokio::test]
    async fn completion_dialog_allows_typing_to_continue() {
        let mut editor = test_editor(40, 10);
        let mut render_buffer = RenderBuffer::new(40, 10, &Style::default());
        let mut runtime = Runtime::new();

        editor
            .execute(
                &Action::EnterMode(Mode::Insert),
                &mut render_buffer,
                &mut runtime,
            )
            .await
            .unwrap();
        let mut completion = CompletionUI::new();
        completion.show(vec![completion_item("alpha")], 0, 0);
        editor.current_dialog = Some(Box::new(completion));

        let event = Event::Key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        if let Some(action) = editor.handle_event(&event).unwrap() {
            editor
                .handle_key_action(&event, &action, &mut render_buffer, &mut runtime)
                .await
                .unwrap();
        }

        assert_eq!(editor.current_buffer().contents(), "zhello");
    }

    #[tokio::test]
    async fn completion_dialog_redraws_typed_text_in_active_window() {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let content = (0..30)
            .map(|line| {
                if line == 22 {
                    "    config_file.".to_string()
                } else {
                    format!("line {line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let buffer = Buffer::new(None, content);
        let mut editor =
            Editor::with_size(lsp, 60, 12, config, Theme::default(), vec![buffer]).unwrap();
        editor.test_disable_terminal_output();
        let mut render_buffer = RenderBuffer::new(60, 12, &Style::default());
        let mut runtime = Runtime::new();

        editor.render(&mut render_buffer).unwrap();
        for _ in 0..22 {
            editor
                .execute(&Action::MoveDown, &mut render_buffer, &mut runtime)
                .await
                .unwrap();
        }
        editor
            .execute(&Action::MoveToLineEnd, &mut render_buffer, &mut runtime)
            .await
            .unwrap();
        editor
            .execute(
                &Action::EnterMode(Mode::Insert),
                &mut render_buffer,
                &mut runtime,
            )
            .await
            .unwrap();
        editor.cx += 1;
        editor.sync_to_window();
        let (completion_x, completion_y) = editor.render_cursor_position().unwrap();
        let mut completion = CompletionUI::new();
        completion.show(vec![completion_item("alpha")], completion_x, completion_y);
        editor.current_dialog = Some(Box::new(completion));
        editor.render(&mut render_buffer).unwrap();

        let event = Event::Key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        if let Some(action) = editor.handle_event(&event).unwrap() {
            editor
                .handle_key_action(&event, &action, &mut render_buffer, &mut runtime)
                .await
                .unwrap();
        }

        let active_row = editor.window_manager.active_window().unwrap().cy;
        let rendered_row = render_row(&render_buffer, active_row);
        assert!(
            rendered_row.contains("config_file.e"),
            "expected active row {active_row} to show typed text, got {rendered_row:?}"
        );
    }

    #[test]
    fn completion_response_filter_uses_text_typed_after_request_position() {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(None, "config_file.as".to_string());
        let mut editor =
            Editor::with_size(lsp, 60, 12, config, Theme::default(), vec![buffer]).unwrap();
        editor.cx = "config_file.as".chars().count();

        let request = crate::lsp::Request {
            id: 1,
            method: "textDocument/completion".to_string(),
            params: serde_json::json!({
                "position": {
                    "line": 0,
                    "character": "config_file.".chars().count()
                }
            }),
            timestamp: std::time::Instant::now(),
        };
        let response = ResponseMessage {
            id: 1,
            result: serde_json::Value::Null,
            request: Some(request),
        };

        assert_eq!(
            editor.completion_filter_for_response(&response).as_deref(),
            Some("as")
        );
    }

    #[test]
    fn completion_dialog_keeps_editor_cursor_visible() {
        let mut editor = test_editor(40, 10);
        let cursor_before = editor.render_cursor_position();

        let mut completion = CompletionUI::new();
        completion.show(vec![completion_item("alpha")], 0, 0);
        editor.current_dialog = Some(Box::new(completion));

        assert_eq!(editor.render_cursor_position(), cursor_before);
    }

    #[test]
    fn completion_dialog_keeps_synthetic_cursor_painted() {
        let mut editor = test_editor(40, 10);
        let mut render_buffer = RenderBuffer::new(40, 10, &Style::default());

        editor.render(&mut render_buffer).unwrap();
        let (x, y) = editor.render_cursor_position().unwrap();
        let cursor_index = y * render_buffer.width + x;
        let cursor_style = render_buffer.cells[cursor_index].style.clone();

        let mut completion = CompletionUI::new();
        completion.show(vec![completion_item("alpha")], x, y);
        editor.current_dialog = Some(Box::new(completion));

        let mut overlay_buffer = RenderBuffer::new(40, 10, &Style::default());
        editor.render(&mut overlay_buffer).unwrap();

        assert_eq!(overlay_buffer.cells[cursor_index].style, cursor_style);
    }

    #[test]
    fn document_symbols_payload_flattens_hierarchical_symbols() {
        let editor = test_editor(40, 10);
        let response = ResponseMessage {
            id: 1,
            result: serde_json::json!([
                {
                    "name": "App",
                    "detail": "struct",
                    "kind": 23,
                    "range": {
                        "start": { "line": 1, "character": 0 },
                        "end": { "line": 8, "character": 1 }
                    },
                    "selectionRange": {
                        "start": { "line": 1, "character": 7 },
                        "end": { "line": 1, "character": 10 }
                    },
                    "children": [
                        {
                            "name": "render",
                            "kind": 6,
                            "range": {
                                "start": { "line": 3, "character": 4 },
                                "end": { "line": 7, "character": 5 }
                            },
                            "selectionRange": {
                                "start": { "line": 3, "character": 7 },
                                "end": { "line": 3, "character": 13 }
                            }
                        }
                    ]
                }
            ]),
            request: Some(crate::lsp::Request::new(
                "textDocument/documentSymbol",
                serde_json::json!({
                    "textDocument": {
                        "uri": "file:///tmp/project/src/app.ts"
                    }
                }),
            )),
        };

        let payload = editor.plugin_document_symbols_payload(&response).unwrap();
        let symbols = payload["symbols"].as_array().unwrap();

        assert_eq!(payload["ok"], true);
        assert_eq!(payload["file"], "/tmp/project/src/app.ts");
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0]["name"], "App");
        assert_eq!(symbols[0]["kindName"], "Struct");
        assert_eq!(symbols[0]["depth"], 0);
        assert_eq!(symbols[0]["selectionRange"]["start"]["character"], 7);
        assert_eq!(symbols[1]["name"], "render");
        assert_eq!(symbols[1]["kindName"], "Method");
        assert_eq!(symbols[1]["depth"], 1);
    }

    #[test]
    fn document_symbols_payload_accepts_flat_symbol_information() {
        let editor = test_editor(40, 10);
        let response = ResponseMessage {
            id: 1,
            result: serde_json::json!([
                {
                    "name": "build",
                    "kind": 12,
                    "containerName": "tools",
                    "location": {
                        "uri": "file:///tmp/project/src/build.ts",
                        "range": {
                            "start": { "line": 4, "character": 2 },
                            "end": { "line": 9, "character": 3 }
                        }
                    }
                }
            ]),
            request: Some(crate::lsp::Request::new(
                "textDocument/documentSymbol",
                serde_json::json!({
                    "textDocument": {
                        "uri": "file:///tmp/project/src/index.ts"
                    }
                }),
            )),
        };

        let payload = editor.plugin_document_symbols_payload(&response).unwrap();
        let symbols = payload["symbols"].as_array().unwrap();

        assert_eq!(payload["file"], "/tmp/project/src/index.ts");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0]["name"], "build");
        assert_eq!(symbols[0]["detail"], "tools");
        assert_eq!(symbols[0]["kindName"], "Function");
        assert_eq!(symbols[0]["file"], "/tmp/project/src/build.ts");
        assert_eq!(symbols[0]["selectionRange"]["start"]["line"], 4);
    }

    #[test]
    fn inlay_hints_payload_preserves_hint_labels() {
        let editor = test_editor(40, 10);
        let response = ResponseMessage {
            id: 1,
            result: serde_json::json!([
                {
                    "position": { "line": 2, "character": 16 },
                    "label": [{ "value": ": PathBuf" }],
                    "kind": 1,
                    "paddingLeft": true
                }
            ]),
            request: Some(crate::lsp::Request::new(
                "textDocument/inlayHint",
                serde_json::json!({
                    "textDocument": {
                        "uri": "file:///tmp/project/src/main.rs"
                    }
                }),
            )),
        };

        let payload = editor.plugin_inlay_hints_payload(&response).unwrap();
        let hints = payload["hints"].as_array().unwrap();

        assert_eq!(payload["ok"], true);
        assert_eq!(payload["file"], "/tmp/project/src/main.rs");
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0]["position"]["line"], 2);
        assert_eq!(hints[0]["label"][0]["value"], ": PathBuf");
        assert_eq!(hints[0]["kind"], 1);
    }

    #[tokio::test]
    async fn mouse_set_cursor_renders_clicked_column_before_action_sync() {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(None, "abcdefghijklmnop\nshort".to_string());
        let mut editor =
            Editor::with_size(lsp, 40, 10, config, Theme::default(), vec![buffer]).unwrap();
        editor.test_disable_terminal_output();
        let mut render_buffer = RenderBuffer::new(40, 10, &Style::default());
        let mut runtime = Runtime::new();

        editor.render(&mut render_buffer).unwrap();
        for _ in 0..10 {
            editor
                .execute(&Action::MoveRight, &mut render_buffer, &mut runtime)
                .await
                .unwrap();
        }
        editor.render(&mut render_buffer).unwrap();
        let (goal_x, _) = editor.render_cursor_position().unwrap();
        let cursor_style = render_buffer.cells[goal_x].style.clone();

        editor
            .execute(&Action::SetCursor(2, 1), &mut render_buffer, &mut runtime)
            .await
            .unwrap();

        let (clicked_x, clicked_y) = editor.render_cursor_position().unwrap();
        assert_eq!(clicked_y, 1);
        let clicked_index = render_buffer.width + clicked_x;
        let stale_goal_x = clicked_x + 2;
        let stale_goal_index = render_buffer.width + stale_goal_x;

        assert_eq!(render_buffer.cells[clicked_index].style, cursor_style);
        assert_ne!(render_buffer.cells[stale_goal_index].style, cursor_style);
    }

    #[tokio::test]
    async fn cursor_moved_event_fires_for_next_word_motion() {
        let _guard = EVENT_RECORDER_TEST_LOCK.lock().await;
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(None, "alpha beta".to_string());
        let mut editor =
            Editor::with_size(lsp, 40, 10, config, Theme::default(), vec![buffer]).unwrap();
        editor.test_disable_terminal_output();
        let mut render_buffer = RenderBuffer::new(40, 10, &Style::default());
        let mut runtime = Runtime::new();
        install_event_recorder(&mut editor, &mut runtime).await;

        editor
            .execute(&Action::MoveToNextWord, &mut render_buffer, &mut runtime)
            .await
            .unwrap();

        assert!(collect_print_requests()
            .iter()
            .any(|message| message == "cursor:MoveToNextWord:0,0->6,0:Normal"));
    }

    #[tokio::test]
    async fn search_highlight_and_clear_emit_plugin_events() {
        let _guard = EVENT_RECORDER_TEST_LOCK.lock().await;
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(None, "alpha beta\nalpha gamma".to_string());
        let mut editor =
            Editor::with_size(lsp, 40, 10, config, Theme::default(), vec![buffer]).unwrap();
        editor.test_disable_terminal_output();
        let mut render_buffer = RenderBuffer::new(40, 10, &Style::default());
        let mut runtime = Runtime::new();
        install_event_recorder(&mut editor, &mut runtime).await;

        editor
            .execute(
                &Action::SearchWordUnderCursor,
                &mut render_buffer,
                &mut runtime,
            )
            .await
            .unwrap();
        let prints = collect_print_requests();
        assert!(prints
            .iter()
            .any(|message| message == "search:SearchWordUnderCursor:alpha:Forward"));

        editor
            .execute(
                &Action::ClearSearchHighlight,
                &mut render_buffer,
                &mut runtime,
            )
            .await
            .unwrap();
        let prints = collect_print_requests();
        assert!(prints.iter().any(|message| message == "cleared:alpha"));
    }

    #[tokio::test]
    async fn cancel_search_emits_mode_changed_event() {
        let _guard = EVENT_RECORDER_TEST_LOCK.lock().await;
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(None, "alpha".to_string());
        let mut editor =
            Editor::with_size(lsp, 40, 10, config, Theme::default(), vec![buffer]).unwrap();
        editor.test_disable_terminal_output();
        let mut render_buffer = RenderBuffer::new(40, 10, &Style::default());
        let mut runtime = Runtime::new();
        install_event_recorder(&mut editor, &mut runtime).await;

        editor
            .execute(
                &Action::EnterSearch(SearchDirection::Forward),
                &mut render_buffer,
                &mut runtime,
            )
            .await
            .unwrap();
        collect_print_requests();

        editor
            .execute(&Action::CancelSearch, &mut render_buffer, &mut runtime)
            .await
            .unwrap();

        assert!(collect_print_requests()
            .iter()
            .any(|message| message == "mode:CancelSearch:Search->Normal"));
    }

    #[tokio::test]
    async fn open_file_action_expands_home_path_once() {
        let home = test_home_dir();
        let dir_name = format!(".red-open-home-{}", uuid::Uuid::new_v4());
        let dir = home.join(&dir_name);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("config.toml");
        std::fs::write(&file, "theme = \"mist\"\n").unwrap();

        let mut editor = test_editor(40, 10);
        let mut render_buffer = RenderBuffer::new(40, 10, &Style::default());
        let mut runtime = Runtime::new();
        let action_path = format!("~/{dir_name}/config.toml");

        editor
            .execute(
                &Action::OpenFile(action_path.clone()),
                &mut render_buffer,
                &mut runtime,
            )
            .await
            .unwrap();

        assert_eq!(editor.current_buffer().contents(), "theme = \"mist\"\n");
        assert_eq!(
            editor.current_buffer().file,
            Some(file.to_string_lossy().into_owned())
        );
        assert_eq!(editor.buffers.len(), 2);

        editor
            .execute(
                &Action::OpenFile(action_path),
                &mut render_buffer,
                &mut runtime,
            )
            .await
            .unwrap();

        assert_eq!(editor.buffers.len(), 2);

        std::fs::remove_dir_all(dir).unwrap();
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
    fn row_snapshot_diff_only_reports_requested_rows() {
        let style = Style::default();
        let mut buffer = RenderBuffer::new(8, 3, &style);
        buffer.set_text(0, 0, "before", &style);
        buffer.set_text(0, 1, "before", &style);

        let snapshot = buffer.snapshot_rows(&[1]);
        buffer.set_text(0, 0, "after", &style);
        buffer.set_text(0, 1, "after", &style);

        let changes = buffer.diff_row_snapshots(&snapshot);

        assert!(!changes.is_empty());
        assert!(changes.iter().all(|change| change.y == 1));
    }

    #[test]
    fn repeated_motion_drain_only_accepts_plain_normal_motion() {
        let mut editor = test_editor(20, 5);
        let event = Event::Key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
        let word_motion = KeyAction::Multiple(vec![Action::MoveToNextWord]);

        assert!(editor.should_drain_repeated_motion(&event, &word_motion));
        assert!(!editor.should_drain_repeated_motion(
            &event,
            &KeyAction::Multiple(vec![Action::EnterMode(Mode::Insert)])
        ));

        editor.repeater = Some(2);
        assert!(!editor.should_drain_repeated_motion(&event, &word_motion));
    }

    #[tokio::test]
    async fn nested_motion_does_not_enter_repeated_motion_drain() {
        let mut editor = test_editor(20, 5);
        editor.config.keys.normal.insert(
            "g".to_string(),
            KeyAction::Nested(HashMap::from([(
                "j".to_string(),
                KeyAction::Single(Action::MoveScreenLineDown),
            )])),
        );
        let mut buffer = RenderBuffer::new(20, 5, &Style::default());
        let mut runtime = Runtime::new();

        let processed = editor
            .process_editor_event(
                Event::Key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE)),
                &mut buffer,
                &mut runtime,
                EventRenderMode::Immediate,
            )
            .await
            .unwrap();
        assert!(!processed.drain_repeated_motion);
        assert!(editor.waiting_key_action.is_some());

        let processed = editor
            .process_editor_event(
                Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
                &mut buffer,
                &mut runtime,
                EventRenderMode::Immediate,
            )
            .await
            .unwrap();

        assert!(!processed.drain_repeated_motion);
    }

    #[test]
    fn render_diff_preserves_line_end_cursor_goal() {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(
            None,
            "abcdefghijklmnop\nabcdefghijkl\nabcdefghijklmnop".to_string(),
        );
        let mut editor =
            Editor::with_size(lsp, 40, 10, config, Theme::default(), vec![buffer]).unwrap();
        editor.test_disable_terminal_output();

        editor.cy = 1;
        editor.cx = 11;
        editor.cursor_goal = CursorGoal::LineEnd;
        editor.sync_to_window();

        editor.render_diff(Vec::new()).unwrap();
        editor.cy = 2;
        editor.apply_cursor_goal_to_current_line();

        assert_eq!(editor.cx, 15);
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
