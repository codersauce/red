mod display_layout;
pub(crate) mod perf;
pub mod render_buffer;
pub mod rendering;

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet, VecDeque},
    ffi::OsStr,
    fs,
    io::{stdout, Write as _},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::unicode_utils::{
    char_prefix, char_slice, char_suffix, char_to_grapheme, column_to_grapheme_with_tabs,
    display_width, display_width_with_tabs, grapheme_len, grapheme_to_byte, grapheme_to_char,
    grapheme_to_column_with_tabs, next_grapheme_boundary, prev_grapheme_boundary, trim_line_ending,
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
        self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    },
    terminal, ExecutableCommand,
};
use husk::RequestId;
#[cfg(unix)]
use nix::sys::signal::{self, Signal};
#[cfg(unix)]
use nix::unistd::Pid;
use once_cell::sync::Lazy;
use path_absolutize::Absolutize;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use unicode_segmentation::UnicodeSegmentation;

pub use render_buffer::RenderBuffer;

use crate::{
    acp::{start_bridge, AcpBridge, AcpProcessSpec, BridgeCommand, BridgeEvent},
    agent_workspace::{
        ProposalAcpHost, ProposalDisposition, ProposalWorkspace, StagedProposalAcceptance,
    },
    buffer::{Buffer, BufferId, SearchMatch},
    clipboard::{ClipboardProvider, DisabledClipboardProvider, NativeClipboardProvider},
    color::Color,
    command,
    config::{Config, KeyAction},
    dispatcher::Dispatcher,
    highlighter::Highlighter,
    log,
    lsp::{
        apply_workspace_resource_operations, file_path as lsp_file_path, get_client_capabilities,
        normalized_file_path as lsp_normalized_file_path, prepare_workspace_edit,
        text_edit_char_range, workspace_edit_operations, Command as LspCommand, CompletionResponse,
        CompletionResponseItem, Diagnostic, DocumentEdit as LspDocumentEdit, InboundMessage,
        InlayHint, InsertTextFormat, Location, LspClient, OpenWorkspaceDocument,
        ParsedNotification, ProgressParams, ProgressToken, Range, ResponseMessage,
        ServerCapabilities, ServerRequest as LspServerRequest, TextEdit as LspTextEdit,
        WorkspaceEditOperation as LspWorkspaceEditOperation, MAX_WORKSPACE_EDIT_TOTAL_BYTES,
    },
    matchit::{self, MatchDirection, MatchMotion},
    plugin::{self, PluginRegistry, Runtime},
    preferences::PreferencesStore,
    session::{
        capture_session_disk_fingerprint, detect_disk_divergence, read_session_disk_contents,
        RecoveryDivergence, SessionAnchorAffinity, SessionBufferSnapshot, SessionDiskFingerprint,
        SessionJump, SessionMark, SessionSnapshot, SessionStore, SESSION_SCHEMA_VERSION,
    },
    theme::{parse_vscode_theme, parse_vscode_theme_contents, Style, Theme},
    ui::{
        AgentComposer, CompletionUI, Component, FilePicker, Info, InputPrompt, LegacyPickerOptions,
        Picker, PickerItem, PickerOptions, PickerPreview, PickerUpdate,
    },
    undo::{AppliedTextEdit, CursorSnapshot, EditOrigin, RevertEdit, TextPosition, TextRange},
    utils::{expand_user_path, get_workspace_path},
    window::{WindowId, WindowManager, WindowManagerSnapshot},
};

use self::display_layout::{
    layout_lines, leading_whitespace_display_width, wrap_line_segments, BreakIndentOptions,
    DisplayLayout, LayoutConfig,
};

pub static ACTION_DISPATCHER: Lazy<Dispatcher<PluginRequest, PluginResponse>> =
    Lazy::new(Dispatcher::new);
#[cfg(test)]
pub(crate) static PLUGIN_DISPATCHER_TEST_LOCK: Lazy<tokio::sync::Mutex<()>> =
    Lazy::new(|| tokio::sync::Mutex::new(()));

pub const DEFAULT_REGISTER: char = '"';
const JUMPLIST_SIZE: usize = 100;
const REPEATED_MOTION_DRAIN_BUDGET_MS: u64 = 50;
const PLUGIN_REQUESTS_PER_TICK: usize = 64;
const ACP_EVENTS_PER_TICK: usize = 64;
const GUTTER_SIGN_COLUMN_WIDTH: usize = 2;
const MAX_HIGHLIGHT_SLICE_BYTES: usize = 512 * 1024;
const MAX_PLUGIN_VIEWPORT_LINE_CHARS: usize = 64 * 1024;
const AGENT_BRIDGE_CAPACITY: usize = 64;
const MACRO_MAX_REPLAY_DEPTH: usize = 20;
const MACRO_MAX_REPLAY_EVENTS: usize = 10_000;
const SESSION_SNAPSHOT_INTERVAL: Duration = Duration::from_secs(5);
type SessionSnapshotGeneration = (u64, Option<u64>);
type SessionSnapshotWriter = std::thread::JoinHandle<anyhow::Result<SessionSnapshotGeneration>>;

fn normalize_terminal_paste(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Command lines, searches, and pickers are single-line inputs. Keep a pasted
/// newline from accidentally accepting or executing their current contents.
fn pasted_input_line(text: &str) -> String {
    normalize_terminal_paste(text)
        .split('\n')
        .next()
        .unwrap_or_default()
        .to_string()
}

fn openai_api_key_ready(
    session_key: Option<&str>,
    configured_key: Option<&str>,
    inherited_key: Option<&OsStr>,
) -> bool {
    if let Some(value) = session_key {
        return !value.trim().is_empty();
    }
    if let Some(value) = configured_key {
        return !value.trim().is_empty();
    }
    inherited_key.is_some_and(|value| !value.to_string_lossy().trim().is_empty())
}

fn normalize_macro_register(register: char) -> Option<char> {
    (register.is_ascii_alphanumeric()).then(|| register.to_ascii_lowercase())
}

fn single_macro_token_character(token: &str) -> anyhow::Result<char> {
    let mut characters = token.chars();
    let character = characters
        .next()
        .ok_or_else(|| anyhow::anyhow!("macro modifier is missing a key"))?;
    anyhow::ensure!(
        characters.next().is_none(),
        "macro modifier must contain exactly one key"
    );
    Ok(character)
}

fn text_position_for_char_index(text: &str, char_index: usize) -> TextPosition {
    let mut line = 0;
    let mut character = 0;
    for value in text.chars().take(char_index) {
        if value == '\n' {
            line += 1;
            character = 0;
        } else {
            character += 1;
        }
    }
    TextPosition::new(line, character)
}

fn parse_substitute_segment(input: &str, delimiter: char) -> anyhow::Result<(String, &str)> {
    let mut value = String::new();
    let mut escaped = false;
    for (offset, character) in input.char_indices() {
        if escaped {
            if character != delimiter {
                value.push('\\');
            }
            value.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == delimiter {
            let next = offset + character.len_utf8();
            return Ok((value, &input[next..]));
        }
        value.push(character);
    }
    anyhow::bail!("substitute is missing a closing {delimiter}")
}

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

#[derive(Debug, Clone)]
struct SemanticChange {
    events: Vec<Event>,
}

#[derive(Debug)]
struct PendingSemanticChange {
    buffer_id: BufferId,
    base_revision: u64,
    events: Vec<Event>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingMacroAction {
    Record,
    Play,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingMarkAction {
    Set,
    JumpExact,
    JumpLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnchorAffinity {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditAnchor {
    buffer_id: BufferId,
    file: Option<String>,
    char_index: usize,
    fallback: TextPosition,
    affinity: AnchorAffinity,
}

#[derive(Debug)]
struct MacroRecording {
    register: char,
    events: Vec<Event>,
}

#[derive(Debug, Clone)]
struct MacroReplayEvent {
    event: Event,
    depth: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubstituteCommand {
    start_line: usize,
    end_line: usize,
    pattern: String,
    replacement: String,
    replace_all: bool,
    case_insensitive: bool,
    confirm: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubstituteDecision {
    Yes,
    No,
    All,
    Quit,
    Last,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedSubstitution {
    start_char: usize,
    end_char: usize,
    replacement: String,
}

#[derive(Debug, Clone)]
struct SubstituteConfirmation {
    substitutions: Vec<PlannedSubstitution>,
    current: usize,
    accepted: Vec<PlannedSubstitution>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum FocusTarget {
    Panel(String),
    Window(WindowId),
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

fn plugin_json(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(plugin_json).collect()),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (snake_case_key(&key), plugin_json(value)))
                .collect(),
        ),
        value => value,
    }
}

fn agent_event_payload(event: BridgeEvent) -> (&'static str, Value) {
    match event {
        BridgeEvent::SessionCreated { session_id } => (
            "agent:session_created",
            json!({ "session_id": session_id.to_string() }),
        ),
        BridgeEvent::Update { session_id, text } => (
            "agent:update",
            json!({ "session_id": session_id.to_string(), "text": text }),
        ),
        BridgeEvent::ProposalsChanged { session_id } => (
            "agent:proposals_changed",
            json!({ "session_id": session_id.to_string() }),
        ),
        BridgeEvent::Completed {
            session_id,
            stop_reason,
        } => (
            "agent:completed",
            json!({
                "session_id": session_id.to_string(),
                "stop_reason": stop_reason,
            }),
        ),
        BridgeEvent::Cancelled { session_id } => (
            "agent:cancelled",
            json!({ "session_id": session_id.to_string() }),
        ),
        BridgeEvent::PermissionRequested {
            request_id,
            session_id,
            tool_call,
            options,
        } => (
            "agent:permission_requested",
            plugin_json(json!({
                "request_id": request_id,
                "session_id": session_id.to_string(),
                "tool_call": tool_call,
                "options": options,
            })),
        ),
        BridgeEvent::Failed {
            session_id,
            message,
        } => (
            "agent:error",
            json!({
                "session_id": session_id.map(|session_id| session_id.to_string()),
                "message": message,
            }),
        ),
    }
}

fn snake_case_key(key: &str) -> String {
    let chars = key.chars().collect::<Vec<_>>();
    let mut result = String::with_capacity(key.len());
    for (index, ch) in chars.iter().copied().enumerate() {
        if ch.is_ascii_uppercase() {
            let previous_is_lower = index > 0 && chars[index - 1].is_ascii_lowercase();
            let next_is_lower = chars
                .get(index + 1)
                .is_some_and(|next| next.is_ascii_lowercase());
            if index > 0 && (previous_is_lower || next_is_lower) {
                result.push('_');
            }
            result.push(ch.to_ascii_lowercase());
        } else {
            result.push(ch);
        }
    }
    result
}

pub enum PluginRequest {
    Action(Action),
    AgentOpenApiKeyPrompt,
    AgentUseCodex,
    AgentNewSession {
        cwd: PathBuf,
    },
    AgentPrompt {
        session_id: String,
        text: String,
    },
    AgentCancel {
        session_id: String,
    },
    AgentCloseSession {
        session_id: String,
    },
    AgentArchiveSession {
        session_id: String,
    },
    AgentProposals {
        session_id: String,
        request_id: RequestId,
    },
    AgentAcceptProposal {
        session_id: String,
        path: PathBuf,
        hunk_id: Option<String>,
    },
    AgentRejectProposal {
        session_id: String,
        path: PathBuf,
        hunk_id: Option<String>,
    },
    AgentPermissionResponse {
        request_id: String,
        option_id: Option<String>,
    },
    EditHistory {
        request_id: RequestId,
    },
    EditorInfo(RequestId),
    OpenPicker(Option<String>, Option<i32>, Vec<Value>),
    OpenLivePicker(Option<String>, Option<i32>, Vec<Value>, LegacyPickerOptions),
    OpenLocation {
        location: plugin::PluginLocation,
        target: plugin::OpenLocationTarget,
    },
    OpenAgentComposer {
        owner: String,
        title: Option<String>,
        id: i32,
        query: String,
        history: Vec<String>,
    },
    OpenDynamicPicker {
        title: Option<String>,
        id: i32,
        items: Vec<PickerItem>,
        options: PickerOptions,
    },
    UpdatePickerItems {
        id: i32,
        items: Vec<PickerItem>,
    },
    UpdatePickerQuery {
        id: i32,
        query: String,
    },
    UpdatePickerStatus {
        id: i32,
        status: Option<String>,
    },
    UpdatePickerPreview {
        id: i32,
        preview: Option<PickerPreview>,
    },
    ClosePicker {
        id: i32,
    },
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
    GetCursorPosition {
        request_id: RequestId,
    },
    SetCursorPosition {
        x: usize,
        y: usize,
    },
    GetCursorDisplayColumn {
        request_id: RequestId,
    },
    SetCursorDisplayColumn {
        column: usize,
        y: usize,
    },
    GetBufferText {
        request_id: RequestId,
        start_line: Option<usize>,
        end_line: Option<usize>,
    },
    GetSelection {
        request_id: RequestId,
    },
    OpenScratchBuffer {
        request_id: RequestId,
        name: String,
        text: String,
    },
    CloseScratchBuffer {
        buffer_index: usize,
    },
    GetViewportLayout {
        request_id: RequestId,
    },
    GetWindows {
        request_id: RequestId,
    },
    InlayHints {
        request_id: RequestId,
        range: Option<Range>,
    },
    SetDecorations {
        namespace: String,
        decorations: Vec<plugin::Decoration>,
    },
    ClearDecorations {
        namespace: String,
    },
    SetGutterSigns {
        namespace: String,
        signs: Vec<plugin::GutterSign>,
    },
    ClearGutterSigns {
        namespace: String,
    },
    GetConfig {
        request_id: RequestId,
        key: Option<String>,
    },
    GetPluginStorage {
        plugin: String,
        key: String,
        request_id: RequestId,
    },
    SetPluginStorage {
        plugin: String,
        key: String,
        value: serde_json::Value,
    },
    GetEditorState {
        request_id: RequestId,
    },
    RestoreEditorState {
        request_id: RequestId,
        snapshot: EditorStateSnapshot,
    },
    DocumentSymbols {
        request_id: RequestId,
        buffer_index: Option<usize>,
    },
    ResolveThemeStyle {
        request_id: RequestId,
        spec: crate::theme::ThemeStyleSpec,
    },
    ListRuntimeAssets {
        kind: crate::assets::RuntimeAssetKind,
        request_id: RequestId,
    },
    WorkspaceSymbols {
        request_id: RequestId,
        query: String,
    },
    References {
        request_id: RequestId,
        include_declaration: bool,
    },
    GetTextDisplayWidth {
        request_id: RequestId,
        text: String,
    },
    CharIndexToDisplayColumn {
        request_id: RequestId,
        x: usize,
        y: usize,
    },
    DisplayColumnToCharIndex {
        request_id: RequestId,
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
    CreateTextPanel {
        id: String,
        config: plugin::PanelConfig,
    },
    UpdateTextPanel {
        id: String,
        blocks: Vec<plugin::TextPanelBlock>,
    },
    AppendTextPanel {
        id: String,
        block_id: String,
        delta: String,
    },
    SelectPanelRow {
        id: String,
        row_id: String,
    },
    FocusPanel {
        id: String,
    },
    FocusEditor,
    ClosePanel {
        id: String,
    },
    OpenWorkspace {
        id: String,
        config: plugin::WorkspaceConfig,
    },
    UpdateWorkspace {
        id: String,
        model: plugin::WorkspaceModel,
    },
    CloseWorkspace {
        id: String,
    },
    CreateWindowBar {
        id: String,
        config: plugin::WindowBarConfig,
    },
    UpdateWindowBar {
        id: String,
        window_id: u64,
        segments: Vec<plugin::WindowBarSegment>,
    },
    CloseWindowBar {
        id: String,
        window_id: Option<u64>,
    },
    ListDirectory {
        path: String,
        request_id: RequestId,
    },
    GetGitStatus {
        path: String,
        request_id: RequestId,
    },
    WatchDirectory {
        path: String,
        watch_id: i32,
        recursive: bool,
        interval_ms: u64,
    },
    UnwatchDirectory {
        watch_id: i32,
    },
}

impl PluginRequest {
    /// Variant name used by the `RED_PERF` instrumentation.
    fn label(&self) -> &'static str {
        match self {
            Self::Action(_) => "Action",
            Self::AgentOpenApiKeyPrompt => "AgentOpenApiKeyPrompt",
            Self::AgentUseCodex => "AgentUseCodex",
            Self::AgentNewSession { .. } => "AgentNewSession",
            Self::AgentPrompt { .. } => "AgentPrompt",
            Self::AgentCancel { .. } => "AgentCancel",
            Self::AgentCloseSession { .. } => "AgentCloseSession",
            Self::AgentArchiveSession { .. } => "AgentArchiveSession",
            Self::AgentProposals { .. } => "AgentProposals",
            Self::AgentAcceptProposal { .. } => "AgentAcceptProposal",
            Self::AgentRejectProposal { .. } => "AgentRejectProposal",
            Self::AgentPermissionResponse { .. } => "AgentPermissionResponse",
            Self::EditHistory { .. } => "EditHistory",
            Self::EditorInfo(_) => "EditorInfo",
            Self::OpenPicker(..) => "OpenPicker",
            Self::OpenLivePicker(..) => "OpenLivePicker",
            Self::OpenLocation { .. } => "OpenLocation",
            Self::OpenAgentComposer { .. } => "OpenAgentComposer",
            Self::OpenDynamicPicker { .. } => "OpenDynamicPicker",
            Self::UpdatePickerItems { .. } => "UpdatePickerItems",
            Self::UpdatePickerQuery { .. } => "UpdatePickerQuery",
            Self::UpdatePickerStatus { .. } => "UpdatePickerStatus",
            Self::UpdatePickerPreview { .. } => "UpdatePickerPreview",
            Self::ClosePicker { .. } => "ClosePicker",
            Self::BufferInsert { .. } => "BufferInsert",
            Self::BufferDelete { .. } => "BufferDelete",
            Self::BufferReplace { .. } => "BufferReplace",
            Self::GetCursorPosition { .. } => "GetCursorPosition",
            Self::SetCursorPosition { .. } => "SetCursorPosition",
            Self::GetCursorDisplayColumn { .. } => "GetCursorDisplayColumn",
            Self::SetCursorDisplayColumn { .. } => "SetCursorDisplayColumn",
            Self::GetBufferText { .. } => "GetBufferText",
            Self::GetSelection { .. } => "GetSelection",
            Self::OpenScratchBuffer { .. } => "OpenScratchBuffer",
            Self::CloseScratchBuffer { .. } => "CloseScratchBuffer",
            Self::GetViewportLayout { .. } => "GetViewportLayout",
            Self::GetWindows { .. } => "GetWindows",
            Self::InlayHints { .. } => "InlayHints",
            Self::SetDecorations { .. } => "SetDecorations",
            Self::ClearDecorations { .. } => "ClearDecorations",
            Self::SetGutterSigns { .. } => "SetGutterSigns",
            Self::ClearGutterSigns { .. } => "ClearGutterSigns",
            Self::GetConfig { .. } => "GetConfig",
            Self::GetPluginStorage { .. } => "GetPluginStorage",
            Self::SetPluginStorage { .. } => "SetPluginStorage",
            Self::GetEditorState { .. } => "GetEditorState",
            Self::RestoreEditorState { .. } => "RestoreEditorState",
            Self::DocumentSymbols { .. } => "DocumentSymbols",
            Self::ResolveThemeStyle { .. } => "ResolveThemeStyle",
            Self::ListRuntimeAssets { .. } => "ListRuntimeAssets",
            Self::WorkspaceSymbols { .. } => "WorkspaceSymbols",
            Self::References { .. } => "References",
            Self::GetTextDisplayWidth { .. } => "GetTextDisplayWidth",
            Self::CharIndexToDisplayColumn { .. } => "CharIndexToDisplayColumn",
            Self::DisplayColumnToCharIndex { .. } => "DisplayColumnToCharIndex",
            Self::IntervalCallback { .. } => "IntervalCallback",
            Self::TimeoutCallback { .. } => "TimeoutCallback",
            Self::CreateOverlay { .. } => "CreateOverlay",
            Self::UpdateOverlay { .. } => "UpdateOverlay",
            Self::RemoveOverlay { .. } => "RemoveOverlay",
            Self::CreatePanel { .. } => "CreatePanel",
            Self::UpdatePanel { .. } => "UpdatePanel",
            Self::CreateTextPanel { .. } => "CreateTextPanel",
            Self::UpdateTextPanel { .. } => "UpdateTextPanel",
            Self::AppendTextPanel { .. } => "AppendTextPanel",
            Self::SelectPanelRow { .. } => "SelectPanelRow",
            Self::FocusPanel { .. } => "FocusPanel",
            Self::FocusEditor => "FocusEditor",
            Self::ClosePanel { .. } => "ClosePanel",
            Self::OpenWorkspace { .. } => "OpenWorkspace",
            Self::UpdateWorkspace { .. } => "UpdateWorkspace",
            Self::CloseWorkspace { .. } => "CloseWorkspace",
            Self::CreateWindowBar { .. } => "CreateWindowBar",
            Self::UpdateWindowBar { .. } => "UpdateWindowBar",
            Self::CloseWindowBar { .. } => "CloseWindowBar",
            Self::ListDirectory { .. } => "ListDirectory",
            Self::GetGitStatus { .. } => "GetGitStatus",
            Self::WatchDirectory { .. } => "WatchDirectory",
            Self::UnwatchDirectory { .. } => "UnwatchDirectory",
        }
    }
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
    recursive: bool,
    interval: Duration,
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
    SelectPreviousUndoBranch,
    SelectNextUndoBranch,
    RevertTransaction(String),
    RepeatLastChange,
    PlayMacro(char),
    SetMark(char),
    JumpToMark {
        mark: char,
        linewise: bool,
    },
    Substitute(SubstituteCommand),
    ConfirmSubstitute(SubstituteDecision),
    SetMacroRegister {
        register: char,
        keys: String,
    },
    PrintRegisters,
    InsertString(String),
    InsertPastedText(String),

    FindNext,
    FindPrevious,
    FindCharForward {
        target: char,
        count: u16,
    },
    TillCharForward {
        target: char,
        count: u16,
    },
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
    OpenLocation(plugin::PluginLocation, plugin::OpenLocationTarget),
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
    DeleteCurrentLines(u16),
    DeleteLineAt(usize),
    DeleteCharAt(usize, usize),
    DeleteRange(usize, usize, usize, usize),
    DeleteTextRange(TextRange),
    ChangeTextRange(TextRange),
    YankTextRange(TextRange),
    ChangeCurrentLine,
    ChangeCurrentLines(u16),
    DeleteWord,

    InsertNewLine,
    InsertCharAtCursorPos(char),
    InsertLineAt(usize, Option<String>),
    InsertLineBelowCursor,
    InsertLineAtCursor,
    InsertTab,

    ReplaceCharsAtCursor {
        character: char,
        count: u16,
    },

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
    SetAgentApiKey(String),
    ClearDiagnostics(String, Vec<usize>),
    RefreshDiagnostics,
    Refresh,
    Hover,
    FormatDocument,
    CodeAction,
    SignatureHelp,
    StartRename,
    RenameSymbol(String),
    ApplyLspWorkspaceEdit {
        documents: Vec<LspDocumentEdit>,
        expected_revisions: Vec<(String, u64)>,
        command: Option<Box<LspCommand>>,
        label: String,
    },
    ApplyLspWorkspaceEditOperations {
        operations: Vec<LspWorkspaceEditOperation>,
        expected_revisions: Vec<(String, u64)>,
        command: Option<Box<LspCommand>>,
        label: String,
        response: Option<Box<LspServerRequest>>,
        save_after_uri: Option<String>,
        save_as: Option<String>,
        save_previous_file: Option<String>,
    },
    CompleteLspFormatSave {
        buffer_id: BufferId,
        uri: String,
        save_as: Option<String>,
        previous_file: Option<String>,
        warning: Option<String>,
    },
    RestoreLspFormatSave {
        buffer_id: BufferId,
        uri: String,
        previous_file: Option<String>,
        warning: String,
    },
    RespondLspWorkspaceEdit {
        request: Box<LspServerRequest>,
        applied: bool,
        failure_reason: Option<String>,
    },
    Print(String),

    OpenPicker(Option<String>, Vec<String>, Option<i32>),
    OpenLivePicker(
        Option<String>,
        Vec<String>,
        Option<i32>,
        LegacyPickerOptions,
    ),
    Picked(String, Option<i32>),
    RecordPickerHistory {
        key: String,
        query: String,
    },
    PreviewTheme(String),
    SetTheme(String),
    Suspend,

    Yank,
    YankCurrentLine,
    YankCurrentLines(u16),
    Delete,
    ChangeSelection,
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
    NotifyPlugin(String, String, Value),
    ResolvePluginRequest(i64, Value),
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
    /// Original capture length in bytes. Used to pick the most specific
    /// (shortest) span; kept separate from start/end so spans clipped to a
    /// viewport slice keep their true specificity.
    pub priority: usize,
    pub style: Style,
}

/// Highlight spans for a parsed slice of a buffer (the viewport plus a
/// margin), so scrolling line-by-line reuses one tree-sitter parse instead
/// of re-parsing per scrolled line.
#[derive(Debug, Clone)]
struct ViewportHighlightEntry {
    revision: u64,
    file: Option<String>,
    /// First buffer line included in the parse.
    start_line: usize,
    /// Byte offset of each parsed line within the parsed text, plus a final
    /// sentinel equal to the parsed text's total length.
    line_offsets: Vec<usize>,
    /// Spans relative to the parsed text, sorted by start.
    spans: Vec<HighlightSpan>,
}

/// Everything that determines the result of `layout_for_window`. Layout is
/// requested many times while drawing a single frame; this key lets those
/// calls share one computation.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct LayoutCacheKey {
    buffer_index: usize,
    revision: u64,
    /// Distinguishes different buffers reusing an index (`revision` restarts
    /// at 0 for a fresh buffer), mirroring `HighlightCacheKey`.
    file: Option<String>,
    vtop: usize,
    vleft: usize,
    skipcol: usize,
    wrap: bool,
    content_width: usize,
    content_height: usize,
    line_count_override: Option<usize>,
    break_indent: BreakIndentOptions,
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
                left.priority
                    .cmp(&right.priority)
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
                priority: style_info.end - style_info.start,
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

    /// Optional native ACP owner connected to the bundled Husk agent surface.
    agent_bridge: Option<AcpBridge>,
    agent_task: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
    agent_workspace: Option<Arc<Mutex<ProposalWorkspace>>>,
    agent_active_sessions: HashSet<String>,
    /// OpenAI credential supplied for this editor process only; never serialized or logged.
    agent_session_api_key: Option<String>,
    /// Prevents secret-prompt input from entering event traces, macros, or semantic replay.
    agent_secret_input_active: bool,

    /// Core-owned crash recovery store. It is optional in tests and embedded uses.
    session_store: Option<SessionStore>,
    last_session_snapshot: Instant,
    last_session_snapshot_generation: Option<SessionSnapshotGeneration>,
    session_snapshot_writer: Option<SessionSnapshotWriter>,

    /// Syntax highlighting engine
    highlighter: Highlighter,

    /// Cached syntax highlight spans per buffer for the most recently parsed
    /// viewport-plus-margin slice.
    highlight_cache: HashMap<usize, ViewportHighlightEntry>,

    /// Memoized display layout, shared by the many per-frame layout queries.
    layout_cache: std::cell::RefCell<HashMap<LayoutCacheKey, std::sync::Arc<DisplayLayout>>>,

    /// All open buffers
    buffers: Vec<Buffer>,

    /// Latest buffer revision successfully delivered to LSP and plugins.
    ///
    /// Action handlers still flush eagerly when UI ordering requires it. The production
    /// dispatcher compares this map with the buffer revision after every action so a new
    /// edit path cannot accidentally omit external change notification.
    notified_buffer_revisions: HashMap<BufferId, u64>,

    /// Index of the currently active buffer
    current_buffer_index: usize,

    /// Window manager handling splits and layout
    window_manager: WindowManager,

    /// Terminal output handle
    stdout: std::io::BufWriter<std::io::Stdout>,

    /// Whether render operations should write terminal escape sequences
    terminal_output_enabled: bool,

    /// Whether the terminal window currently has focus.
    is_focused: bool,

    /// Suppresses the mouse-down used to reactivate the terminal while a
    /// non-editor surface already owns logical focus.
    suppress_reactivation_click: bool,

    /// Incremented after full renders so event handling can avoid duplicate frames.
    render_generation: u64,

    /// Last terminal cursor cell painted into the render buffer.
    last_rendered_cursor_position: Option<(usize, usize)>,

    /// Last rendered surface under the terminal-owned cursor.
    ///
    /// Insert mode and input components use the terminal cursor rather than
    /// the synthetic block cursor, so OSC cursor colors must be repaired
    /// against the actual cell they are drawn over.
    last_rendered_cursor_surface: Option<Style>,

    /// Suppresses per-step repainting while queued repeated motions are drained.
    defer_motion_render: bool,

    /// Tracks whether deferred motion changed the visible viewport.
    deferred_motion_needs_full_render: bool,

    /// Earliest state before a repeated-motion batch and the latest cause.
    /// The final snapshot is read when the batch flushes, so state events are
    /// delivered once with the final cursor/viewport position.
    deferred_plugin_event: Option<(EditorEventSnapshot, String)>,

    /// Nesting depth for visual-block replay. Replay edits should update the
    /// buffer and current transaction without repainting or notifying per row.
    block_replay_depth: usize,

    /// Whether a buffer change notification was deferred during block replay.
    block_replay_change_deferred: bool,

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

    /// Partially entered forward character motion, such as f followed by a target.
    pending_character_motion: Option<PendingCharacterMotion>,

    /// Whether Normal-mode `r` is waiting for its replacement character.
    pending_replace: bool,

    /// Executed actions
    actions: Vec<Action>,

    /// Input recipe for the last completed content-changing command.
    ///
    /// Unlike macros, this is scoped to one semantic change and is finalized only when
    /// the edit transaction returns to a stable Normal-mode boundary.
    last_semantic_change: Option<SemanticChange>,
    pending_semantic_change: Option<PendingSemanticChange>,
    replaying_semantic_change: bool,

    /// Vim-style macro state. Register contents are stored as editable key notation;
    /// `events` is only the in-progress recording.
    pending_macro_action: Option<PendingMacroAction>,
    macro_recording: Option<MacroRecording>,
    last_played_macro: Option<char>,
    macro_replay_depth: usize,
    macro_instructions_remaining: usize,
    macro_replay_queue: VecDeque<MacroReplayEvent>,

    /// Buffer-local, file-global, and special Vim marks. Anchors use flat Unicode
    /// character coordinates and are transformed at the canonical edit boundary.
    pending_mark_action: Option<PendingMarkAction>,
    local_marks: HashMap<BufferId, HashMap<char, EditAnchor>>,
    global_marks: HashMap<char, EditAnchor>,
    special_marks: HashMap<(BufferId, char), EditAnchor>,
    substitute_confirmation: Option<SubstituteConfirmation>,

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

    /// Matches for the most recently rendered or previewed search.
    search_match_cache: Option<SearchMatchCache>,

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

    /// System clipboard bridge for the default register.
    clipboard: Box<dyn ClipboardProvider>,

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

    gutter_sign_manager: plugin::GutterSignManager,

    panel_manager: plugin::PanelManager,

    workspace_manager: plugin::WorkspaceManager,

    window_bar_manager: plugin::WindowBarManager,

    directory_watchers: HashMap<i32, DirectoryWatcher>,

    pending_plugin_document_symbols: HashMap<i64, PendingDocumentSymbols>,
    pending_plugin_workspace_symbols: HashMap<i64, RequestId>,
    pending_plugin_references: HashMap<i64, RequestId>,
    pending_plugin_inlay_hints: HashMap<i64, RequestId>,
    pending_lsp_edit_requests: HashMap<i64, PendingLspEdit>,
    pending_lsp_format_saves: HashMap<i64, PendingLspFormatSave>,
    pending_lsp_revision_snapshots: HashMap<i64, Vec<(String, u64)>>,
    completion_snapshot: Option<PendingLspEdit>,
}

/// Terminal-independent owner used by the local detach protocol. It keeps the real
/// editor, Husk runtime, LSP client, ACP task, buffers, and render state in the server
/// process while replaceable clients only send normalized input and paint rows.
pub struct DetachedEditorCore {
    editor: Editor,
    runtime: Runtime,
    render_buffer: RenderBuffer,
    revision: u64,
    rows: Vec<String>,
    row_spans: Vec<Vec<crate::headless::StyledSpan>>,
    cursor: (usize, usize),
    stopped: bool,
    pending_paste: String,
}

impl DetachedEditorCore {
    pub async fn new(mut editor: Editor) -> anyhow::Result<Self> {
        editor.terminal_output_enabled = false;
        let mut runtime =
            Runtime::try_new_with_permissions(editor.config.plugin_permissions.clone())?;
        runtime.set_typecheck_enabled(!editor.config.disable_plugin_typecheck);
        editor.refresh_plugin_snapshots(&mut runtime, true, true, true)?;
        for (name, path) in &editor.config.plugins {
            let path = Config::resolve_plugin_path(path);
            editor.plugin_registry.add(name, path.as_str());
        }
        editor.plugin_registry.initialize(&mut runtime).await?;
        editor
            .plugin_registry
            .notify(&mut runtime, "editor:ready", json!({}))
            .await?;
        if let Some(transcript) = editor
            .preferences
            .plugin_storage("agent", "transcript")
            .and_then(Value::as_str)
        {
            editor
                .plugin_registry
                .notify(
                    &mut runtime,
                    "agent:transcript_restored",
                    json!({ "transcript": transcript }),
                )
                .await?;
        }
        editor.ensure_current_buffer_lsp_opened().await?;
        let mut render_buffer = RenderBuffer::new(
            editor.size.0 as usize,
            editor.size.1 as usize,
            &Style::default(),
        );
        editor.render(&mut render_buffer)?;
        let rows = render_text_rows(&render_buffer);
        let row_spans = render_styled_rows(&render_buffer, &editor.theme.style);
        let cursor = editor
            .render_cursor_position()
            .unwrap_or((editor.cx, editor.cy));
        Ok(Self {
            editor,
            runtime,
            render_buffer,
            revision: 0,
            rows,
            row_spans,
            cursor,
            stopped: false,
            pending_paste: String::new(),
        })
    }

    #[must_use]
    pub fn snapshot(&self, last_revision: Option<u64>) -> crate::headless::RenderDelta {
        crate::headless::RenderDelta {
            revision: self.revision,
            lines: if last_revision == Some(self.revision) {
                Vec::new()
            } else {
                self.rows
                    .iter()
                    .enumerate()
                    .map(|(row, text)| crate::headless::LinePatch {
                        row,
                        text: text.clone(),
                        spans: self.row_spans.get(row).cloned().unwrap_or_default(),
                    })
                    .collect()
            },
            cursor: self.cursor,
        }
    }

    pub async fn input(
        &mut self,
        event: crate::headless::InputEvent,
    ) -> anyhow::Result<crate::headless::RenderDelta> {
        let event = match event {
            crate::headless::InputEvent::PasteChunk { text, final_chunk } => {
                let next_size = self.pending_paste.len().saturating_add(text.len());
                if next_size > crate::headless::MAX_PENDING_PASTE_BYTES {
                    self.pending_paste.clear();
                    anyhow::bail!(
                        "detach paste exceeds {} bytes before completion",
                        crate::headless::MAX_PENDING_PASTE_BYTES
                    );
                }
                self.pending_paste.push_str(&text);
                if !final_chunk {
                    return Ok(self.snapshot(Some(self.revision)));
                }
                Event::Paste(std::mem::take(&mut self.pending_paste))
            }
            event => {
                self.pending_paste.clear();
                detached_input_to_crossterm(event)
            }
        };
        let processed = self
            .editor
            .process_editor_event(
                event,
                &mut self.render_buffer,
                &mut self.runtime,
                EventRenderMode::Immediate,
            )
            .await?;
        self.stopped = processed.quit;
        self.editor
            .service_background(&mut self.render_buffer, &mut self.runtime)
            .await?;
        self.editor.persist_session_snapshot(/*force*/ false);
        self.finish_render()
    }

    pub async fn resize(
        &mut self,
        columns: u16,
        rows: u16,
    ) -> anyhow::Result<crate::headless::RenderDelta> {
        self.editor
            .process_editor_event(
                Event::Resize(columns, rows),
                &mut self.render_buffer,
                &mut self.runtime,
                EventRenderMode::Immediate,
            )
            .await?;
        self.editor
            .service_background(&mut self.render_buffer, &mut self.runtime)
            .await?;
        self.finish_render()
    }

    pub async fn focus(&mut self, focused: bool) -> anyhow::Result<crate::headless::RenderDelta> {
        let event = if focused {
            Event::FocusGained
        } else {
            Event::FocusLost
        };
        self.editor
            .process_editor_event(
                event,
                &mut self.render_buffer,
                &mut self.runtime,
                EventRenderMode::Immediate,
            )
            .await?;
        self.editor
            .service_background(&mut self.render_buffer, &mut self.runtime)
            .await?;
        self.finish_render()
    }

    /// Advance terminal-independent background work while no client is attached.
    ///
    /// This is the ownership guarantee behind detach: plugin processes, LSP messages,
    /// timers, directory watches, and ACP events keep flowing in the core process.
    pub async fn tick(&mut self) -> anyhow::Result<Option<crate::headless::RenderDelta>> {
        let render_generation = self.editor.render_generation;
        self.editor
            .service_background(&mut self.render_buffer, &mut self.runtime)
            .await?;
        self.editor.persist_session_snapshot(/*force*/ false);
        if self.editor.render_generation == render_generation {
            perf::increment("detach:idle_tick", 1);
            return Ok(None);
        }
        perf::increment("detach:rendered_tick", 1);
        self.finish_render().map(Some)
    }

    #[must_use]
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    pub fn persist(&mut self) {
        self.editor.persist_session_snapshot(/*force*/ true);
    }

    pub fn clear_pending_paste(&mut self) {
        self.pending_paste.clear();
    }

    pub async fn shutdown(&mut self) {
        self.editor.shutdown_services(&mut self.runtime).await;
    }

    fn finish_render(&mut self) -> anyhow::Result<crate::headless::RenderDelta> {
        let _span = perf::PerfSpan::start("detach:serialize_frame");
        let next_rows = render_text_rows(&self.render_buffer);
        let next_row_spans = render_styled_rows(&self.render_buffer, &self.editor.theme.style);
        let next_cursor = self
            .editor
            .render_cursor_position()
            .unwrap_or((self.editor.cx, self.editor.cy));
        let changed = next_rows != self.rows
            || next_row_spans != self.row_spans
            || next_cursor != self.cursor;
        if changed {
            self.revision = self.revision.saturating_add(1);
        }
        let lines: Vec<_> = next_rows
            .iter()
            .enumerate()
            .filter(|(row, text)| {
                self.rows.get(*row) != Some(*text)
                    || self.row_spans.get(*row) != next_row_spans.get(*row)
            })
            .map(|(row, text)| crate::headless::LinePatch {
                row,
                text: text.clone(),
                spans: next_row_spans.get(row).cloned().unwrap_or_default(),
            })
            .collect();
        perf::gauge_max("detach:changed_rows", lines.len() as u64);
        self.rows = next_rows;
        self.row_spans = next_row_spans;
        self.cursor = next_cursor;
        Ok(crate::headless::RenderDelta {
            revision: self.revision,
            lines,
            cursor: self.cursor,
        })
    }
}

fn render_text_rows(buffer: &RenderBuffer) -> Vec<String> {
    buffer
        .cells
        .chunks(buffer.width.max(1))
        .map(|row| {
            let mut text = String::new();
            let mut column = 0;
            while let Some(cell) = row.get(column) {
                text.push_str(&cell.text);
                column += display_width(&cell.text).max(1);
            }
            text
        })
        .collect()
}

fn render_styled_rows(
    buffer: &RenderBuffer,
    theme_style: &Style,
) -> Vec<Vec<crate::headless::StyledSpan>> {
    buffer
        .cells
        .chunks(buffer.width.max(1))
        .map(|row| {
            let mut spans: Vec<crate::headless::StyledSpan> = Vec::new();
            let mut column = 0;
            while let Some(cell) = row.get(column) {
                let (fg, bg) = rendering::resolve_cell_colors(&cell.style, theme_style);
                let style = Style {
                    fg: Some(fg),
                    bg: Some(bg),
                    bold: cell.style.bold,
                    italic: cell.style.italic,
                };
                if let Some(span) = spans.last_mut().filter(|span| span.style == style) {
                    span.text.push_str(&cell.text);
                } else {
                    spans.push(crate::headless::StyledSpan {
                        text: cell.text.clone(),
                        style,
                    });
                }
                column += display_width(&cell.text).max(1);
            }
            spans
        })
        .collect()
}

fn detached_input_to_crossterm(event: crate::headless::InputEvent) -> Event {
    match event {
        crate::headless::InputEvent::Paste { text }
        | crate::headless::InputEvent::PasteChunk { text, .. } => Event::Paste(text),
        crate::headless::InputEvent::Mouse { event } => Event::Mouse(event),
        crate::headless::InputEvent::Key { code, modifiers } => {
            let code = match code {
                crate::headless::KeyCode::Character(character) => KeyCode::Char(character),
                crate::headless::KeyCode::Enter => KeyCode::Enter,
                crate::headless::KeyCode::Backspace => KeyCode::Backspace,
                crate::headless::KeyCode::Escape => KeyCode::Esc,
                crate::headless::KeyCode::Tab => KeyCode::Tab,
                crate::headless::KeyCode::BackTab => KeyCode::BackTab,
                crate::headless::KeyCode::Delete => KeyCode::Delete,
                crate::headless::KeyCode::Left => KeyCode::Left,
                crate::headless::KeyCode::Right => KeyCode::Right,
                crate::headless::KeyCode::Up => KeyCode::Up,
                crate::headless::KeyCode::Down => KeyCode::Down,
                crate::headless::KeyCode::Home => KeyCode::Home,
                crate::headless::KeyCode::End => KeyCode::End,
                crate::headless::KeyCode::PageUp => KeyCode::PageUp,
                crate::headless::KeyCode::PageDown => KeyCode::PageDown,
            };
            let mut key_modifiers = KeyModifiers::NONE;
            for modifier in modifiers {
                key_modifiers |= match modifier {
                    crate::headless::KeyModifier::Control => KeyModifiers::CONTROL,
                    crate::headless::KeyModifier::Alt => KeyModifiers::ALT,
                    crate::headless::KeyModifier::Shift => KeyModifiers::SHIFT,
                };
            }
            Event::Key(KeyEvent::new(code, key_modifiers))
        }
    }
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

#[derive(Debug, Clone)]
struct SearchMatchCache {
    buffer_id: BufferId,
    revision: u64,
    pattern: String,
    case_insensitive: bool,
    matches: Arc<[SearchMatch]>,
}

#[derive(Debug, Clone)]
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
    window_id: Option<WindowId>,
    window_ids: Vec<WindowId>,
}

#[derive(Debug, Clone)]
struct PendingDocumentSymbols {
    plugin_request_id: RequestId,
    buffer_index: usize,
    revision: u64,
}

#[derive(Debug, Clone)]
struct PendingLspEdit {
    buffer_id: BufferId,
    revision: u64,
    uri: String,
}

#[derive(Debug, Clone)]
struct PendingLspFormatSave {
    save_as: Option<String>,
    previous_file: Option<String>,
}

enum FormatOnSaveRequest {
    Pending,
    Save { warning: Option<String> },
    Cancelled,
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
    FindForward,
    TillForward,
    TextObjectScope(TextObjectScope),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingOperator {
    operator: EditOperator,
    step: PendingOperatorStep,
    operator_count: u16,
    motion_count: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForwardCharacterMotion {
    Find,
    Till,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingCharacterMotion {
    kind: ForwardCharacterMotion,
    count: u16,
}

impl PendingOperator {
    fn new(operator: EditOperator, operator_count: u16) -> Self {
        Self {
            operator,
            step: PendingOperatorStep::Operator,
            operator_count,
            motion_count: None,
        }
    }

    fn count(self) -> u16 {
        self.operator_count
            .saturating_mul(self.motion_count.unwrap_or(1))
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

struct VisualPastePlan {
    text: String,
    cursor: TextPosition,
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

    pub fn blockwise(text: String) -> Self {
        Self {
            kind: ContentKind::Blockwise,
            text,
        }
    }
}

impl Editor {
    fn clipboard_provider_for_config(config: &Config) -> Box<dyn ClipboardProvider> {
        if !config.clipboard.enabled {
            return Box::new(DisabledClipboardProvider);
        }

        match NativeClipboardProvider::new() {
            Ok(provider) => Box::new(provider),
            Err(error) => {
                log!("system clipboard disabled: {error}");
                Box::new(DisabledClipboardProvider)
            }
        }
    }

    #[doc(hidden)]
    pub fn test_set_clipboard(&mut self, clipboard: Box<dyn ClipboardProvider>) {
        self.clipboard = clipboard;
    }

    fn is_waiting_for_key_sequence(&self) -> bool {
        self.waiting_key_action.is_some()
            || self.pending_operator.is_some()
            || self.pending_character_motion.is_some()
            || self.pending_replace
            || self.pending_visual_text_object_scope.is_some()
            || self.pending_macro_action.is_some()
            || self.pending_mark_action.is_some()
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
        // Buffer terminal output so a full-screen repaint is one write
        // syscall instead of one per ~1KB of escape sequences.
        let mut stdout = std::io::BufWriter::with_capacity(1 << 20, stdout());
        let vx = buffers
            .first()
            .map(|b| b.len().to_string().len())
            .unwrap_or(0)
            + 2;
        let size = (width as u16, height as u16);
        let highlighter = Highlighter::new(&theme)?;

        let mut plugin_registry = PluginRegistry::new();
        let indentation = HashMap::from_iter(
            [
                ("rs", 4),
                ("js", 2),
                ("jsx", 2),
                ("mjs", 2),
                ("cjs", 2),
                ("ts", 2),
                ("tsx", 2),
                ("json", 2),
                ("jsonc", 2),
                ("toml", 2),
                ("yaml", 2),
                ("yml", 2),
                ("sh", 2),
                ("bash", 2),
                ("zsh", 2),
                ("py", 4),
                ("pyw", 4),
            ]
            .map(|(file_type, shift_width)| {
                (
                    file_type.to_string(),
                    Indentation::new(shift_width, shift_width, true),
                )
            }),
        );

        let mut window_manager = WindowManager::new(0, (width, height));
        let wrap = config.wrap.unwrap_or(true);
        for window in window_manager.windows_mut() {
            window.wrap = wrap;
        }
        let completion_ui = CompletionUI::with_theme(&theme);
        let clipboard = Self::clipboard_provider_for_config(&config);

        let notified_buffer_revisions = buffers
            .iter()
            .map(|buffer| (buffer.id(), buffer.revision()))
            .collect();

        Ok(Editor {
            lsp,
            lsp_opened_documents: HashSet::new(),
            config,
            theme,
            plugin_registry,
            agent_bridge: None,
            agent_task: None,
            agent_workspace: None,
            agent_active_sessions: HashSet::new(),
            agent_session_api_key: None,
            agent_secret_input_active: false,
            session_store: None,
            last_session_snapshot: Instant::now(),
            last_session_snapshot_generation: None,
            session_snapshot_writer: None,
            highlighter,
            highlight_cache: HashMap::new(),
            layout_cache: std::cell::RefCell::new(HashMap::new()),
            buffers,
            notified_buffer_revisions,
            current_buffer_index: 0,
            window_manager,
            stdout,
            terminal_output_enabled: true,
            is_focused: true,
            suppress_reactivation_click: false,
            render_generation: 0,
            last_rendered_cursor_position: None,
            last_rendered_cursor_surface: None,
            defer_motion_render: false,
            deferred_motion_needs_full_render: false,
            deferred_plugin_event: None,
            block_replay_depth: 0,
            block_replay_change_deferred: false,
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
            pending_character_motion: None,
            pending_replace: false,
            actions: vec![],
            last_semantic_change: None,
            pending_semantic_change: None,
            replaying_semantic_change: false,
            pending_macro_action: None,
            macro_recording: None,
            last_played_macro: None,
            macro_replay_depth: 0,
            macro_instructions_remaining: 0,
            macro_replay_queue: VecDeque::new(),
            pending_mark_action: None,
            local_marks: HashMap::new(),
            global_marks: HashMap::new(),
            special_marks: HashMap::new(),
            substitute_confirmation: None,
            command: String::new(),
            preferences,
            command_history_navigation: None,
            command_completion: None,
            search_term: String::new(),
            search_direction: SearchDirection::Forward,
            active_search: None,
            search_match_cache: None,
            search_highlights_suppressed: false,
            last_error: None,
            current_dialog: None,
            repeater: None,
            selection_start: None,
            selection: None,
            registers: HashMap::new(),
            clipboard,
            diagnostics: HashMap::new(),
            completion_ui,
            indentation,
            jump_list: Vec::new(),
            jump_index: 0,
            render_commands: VecDeque::new(),
            overlay_manager: plugin::OverlayManager::new(),
            decoration_manager: plugin::DecorationManager::default(),
            gutter_sign_manager: plugin::GutterSignManager::default(),
            panel_manager: plugin::PanelManager::default(),
            workspace_manager: plugin::WorkspaceManager::default(),
            window_bar_manager: plugin::WindowBarManager::default(),
            directory_watchers: HashMap::new(),
            pending_plugin_document_symbols: HashMap::new(),
            pending_plugin_workspace_symbols: HashMap::new(),
            pending_plugin_references: HashMap::new(),
            pending_plugin_inlay_hints: HashMap::new(),
            pending_lsp_edit_requests: HashMap::new(),
            pending_lsp_format_saves: HashMap::new(),
            pending_lsp_revision_snapshots: HashMap::new(),
            completion_snapshot: None,
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
        // Apply scrolloff now so a motion that scrolls renders exactly once
        // instead of drawing a pre-scroll frame that check_bounds immediately
        // invalidates.
        self.check_bounds();
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

    fn focus_ring(&self) -> Vec<FocusTarget> {
        let mut targets = self
            .panel_manager
            .focusable_ids_for_side(plugin::PanelSide::Left)
            .into_iter()
            .map(FocusTarget::Panel)
            .collect::<Vec<_>>();
        targets.extend(
            self.window_manager
                .windows()
                .into_iter()
                .map(|window| FocusTarget::Window(window.id)),
        );
        targets.extend(
            self.panel_manager
                .focusable_ids_for_side(plugin::PanelSide::Right)
                .into_iter()
                .map(FocusTarget::Panel),
        );
        targets
    }

    fn current_focus_target(&self) -> Option<FocusTarget> {
        self.panel_manager
            .focused_panel_id()
            .map(|id| FocusTarget::Panel(id.to_string()))
            .or_else(|| {
                self.window_manager
                    .active_stable_window_id()
                    .map(FocusTarget::Window)
            })
    }

    fn next_focus_target(&self, forward: bool) -> Option<FocusTarget> {
        let targets = self.focus_ring();
        if targets.len() <= 1 {
            return None;
        }
        let current = self.current_focus_target()?;
        let index = targets.iter().position(|target| target == &current)?;
        let next = if forward {
            (index + 1) % targets.len()
        } else if index == 0 {
            targets.len() - 1
        } else {
            index - 1
        };
        targets.get(next).cloned()
    }

    fn focus_target(&mut self, target: &FocusTarget) -> bool {
        match target {
            FocusTarget::Panel(id) => {
                if self.panel_manager.focused_panel_id() == Some(id.as_str()) {
                    return false;
                }
                self.panel_manager.focus_panel(id)
            }
            FocusTarget::Window(id) => {
                let had_focused_panel = self.panel_manager.has_focused_panel();
                self.panel_manager.focus_editor();
                let switched_window = self
                    .window_manager
                    .window_index(*id)
                    .is_some_and(|index| self.set_active_window(index));
                had_focused_panel || switched_window
            }
        }
    }

    async fn cycle_focus(
        &mut self,
        forward: bool,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<()> {
        let Some(target) = self.next_focus_target(forward) else {
            return Ok(());
        };
        let previous_window = self.window_manager.active_stable_window_id();
        if !self.focus_target(&target) {
            return Ok(());
        }
        if previous_window != self.window_manager.active_stable_window_id() {
            self.request_diagnostics().await?;
        }
        self.render(buffer)
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
        self.indentation_for_buffer_index(self.current_buffer_index)
    }

    fn indentation_for_buffer_index(&self, buffer_index: usize) -> Indentation {
        let file_type = self
            .buffers
            .get(buffer_index)
            .and_then(|buffer| buffer.file_type());

        let Some(file_type) = file_type.as_deref() else {
            return Indentation::new(4, 4, true);
        };

        self.indentation
            .get(file_type)
            .copied()
            .unwrap_or_else(|| Indentation::new(4, 4, true))
    }

    fn tab_width_for_buffer_index(&self, buffer_index: usize) -> usize {
        self.indentation_for_buffer_index(buffer_index)
            .shift_width
            .max(1)
    }

    fn active_tab_width(&self) -> usize {
        self.tab_width_for_buffer_index(self.current_buffer_index)
    }

    fn break_indent_options_for_buffer_index(&self, buffer_index: usize) -> BreakIndentOptions {
        BreakIndentOptions {
            enabled: self.config.breakindent.unwrap_or(true),
            tab_width: self
                .indentation_for_buffer_index(buffer_index)
                .shift_width
                .max(1),
        }
    }

    pub fn vwidth(&self) -> usize {
        self.size.0 as usize
    }

    pub fn vheight(&self) -> usize {
        self.window_manager
            .active_window()
            .map(|window| self.window_content_height(window))
            .unwrap_or_else(|| (self.size.1 as usize).saturating_sub(2))
    }

    pub(crate) fn picker_input_position(&self) -> crate::config::PickerInputPosition {
        self.config.picker.input_position
    }

    /// Window-aware coordinate transformation methods
    /// Convert window-local X coordinate to terminal X coordinate
    pub fn window_to_terminal_x(&self, window: &crate::window::Window, x: usize) -> usize {
        window.position.x + x
    }

    /// Convert window-local Y coordinate to terminal Y coordinate
    pub fn window_to_terminal_y(&self, window: &crate::window::Window, y: usize) -> usize {
        window.position.y + self.window_content_top(window) + y
    }

    /// Convert buffer coordinates to window-local coordinates, accounting for viewport
    pub fn buffer_to_window_coords(
        &self,
        window: &crate::window::Window,
        buf_x: usize,
        buf_y: usize,
    ) -> Option<(usize, usize)> {
        let line = self.buffers.get(window.buffer_index)?.get(buf_y)?;
        let display_col = grapheme_to_column_with_tabs(
            line.trim_end_matches('\n'),
            buf_x,
            self.tab_width_for_buffer_index(window.buffer_index),
        );
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
        self.window_content_height(window)
    }

    fn window_content_top(&self, window: &crate::window::Window) -> usize {
        self.window_bar_manager.reserved_top_height(window.id)
    }

    fn window_content_height(&self, window: &crate::window::Window) -> usize {
        window
            .inner_height()
            .saturating_sub(self.window_content_top(window))
    }

    fn window_content_width(&self, window: &crate::window::Window) -> usize {
        let gutter_width = self.gutter_width_for_window(window);
        window.inner_width().saturating_sub(gutter_width + 1)
    }

    fn layout_for_window(&self, window: &crate::window::Window) -> std::sync::Arc<DisplayLayout> {
        let Some(buffer) = self.buffers.get(window.buffer_index) else {
            return std::sync::Arc::new(DisplayLayout { rows: Vec::new() });
        };
        let mut line_count = buffer.navigable_line_count();
        let mut line_count_override = None;
        if window.active && self.is_insert() {
            line_count_override = Some(self.buffer_line() + 1);
            line_count = line_count.max(self.buffer_line() + 1);
        }

        let break_indent = self.break_indent_options_for_buffer_index(window.buffer_index);
        let key = LayoutCacheKey {
            buffer_index: window.buffer_index,
            revision: buffer.revision(),
            file: buffer.file.clone(),
            vtop: window.vtop,
            vleft: window.vleft,
            skipcol: window.skipcol,
            wrap: window.wrap,
            content_width: self.window_content_width(window),
            content_height: self.window_content_height(window),
            line_count_override,
            break_indent,
        };
        if let Some(layout) = self.layout_cache.borrow().get(&key) {
            return layout.clone();
        }

        let _span = perf::PerfSpan::start("layout_for_window:miss");
        let end = window
            .vtop
            .saturating_add(self.window_content_height(window))
            .min(line_count);
        let lines = (window.vtop..end)
            .filter_map(|line| buffer.get(line))
            .collect::<Vec<_>>();

        let layout = std::sync::Arc::new(layout_lines(
            &lines,
            line_count,
            LayoutConfig {
                content_width: self.window_content_width(window),
                height: self.window_content_height(window),
                wrap: window.wrap,
                vtop: window.vtop,
                vleft: window.vleft,
                skipcol: window.skipcol,
                break_indent,
            },
        ));

        let mut cache = self.layout_cache.borrow_mut();
        if cache.len() >= 32 {
            cache.clear();
        }
        cache.insert(key, layout.clone());
        layout
    }

    fn plugin_viewport_layout_payload(&self) -> Value {
        let Some(window) = self.active_window_with_editor_view() else {
            return json!({
                "buffer_index": self.current_buffer_index,
                "window_id": self.window_manager.active_stable_window_id().map(|id| id.0),
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
                let text = if !segment.first_segment {
                    String::new()
                } else if buffer.line_range_byte_len(segment.line, segment.line + 1)
                    > MAX_HIGHLIGHT_SLICE_BYTES
                {
                    buffer.line_prefix_contents(segment.line, MAX_PLUGIN_VIEWPORT_LINE_CHARS)
                } else {
                    buffer.get(segment.line).unwrap_or_default()
                };
                let text = text.trim_end_matches(['\r', '\n']);
                let indent_width =
                    leading_whitespace_display_width(text, indentation.shift_width.max(1));
                json!({
                    "screen_row": segment.row,
                    "line": segment.line,
                    "start_col": segment.start_col,
                    "end_col": segment.end_col,
                    "start_grapheme": segment.start_grapheme,
                    "end_grapheme": segment.end_grapheme,
                    "first_segment": segment.first_segment,
                    "indent_width": indent_width,
                    "visual_offset": segment.visual_offset,
                    "text": text,
                })
            })
            .collect::<Vec<_>>();

        json!({
            "buffer_index": window.buffer_index,
            "window_id": window.id.0,
            "width": window.inner_width(),
            "height": self.window_content_height(&window),
            "content_top": self.window_content_top(&window),
            "content_start": content_start,
            "content_width": content_width,
            "vtop": window.vtop,
            "vleft": window.vleft,
            "skipcol": window.skipcol,
            "wrap": window.wrap,
            "cursor": {
                "x": window.cx,
                "y": window.vtop + window.cy,
                "lsp_character": self.lsp_character_for_cursor(window.buffer_index, window.vtop + window.cy, window.cx),
                "screen_row": window.cy,
            },
            "indentation": {
                "shift_width": indentation.shift_width,
                "tab_width": indentation.shift_width,
            },
            "line_count": buffer.navigable_line_count(),
            "revision": buffer.revision(),
            "file": buffer.file,
            "rows": rows,
        })
    }

    pub(crate) fn refresh_plugin_snapshots(
        &self,
        runtime: &mut Runtime,
        viewport: bool,
        windows: bool,
        editor_info: bool,
    ) -> anyhow::Result<()> {
        if viewport {
            runtime.set_snapshot("viewport_layout", self.plugin_viewport_layout_payload());
        }
        if windows {
            runtime.set_snapshot("windows", self.plugin_windows_payload());
        }
        if editor_info {
            runtime.set_snapshot("editor_info", serde_json::to_value(self.info())?);
        }
        Ok(())
    }

    fn plugin_windows_payload(&self) -> Value {
        let active_id = self.window_manager.active_stable_window_id();
        let windows = self
            .window_manager
            .windows()
            .into_iter()
            .filter_map(|window| {
                let buffer = self.buffers.get(window.buffer_index)?;
                let cursor_y = window.vtop + window.cy;
                let lsp_character =
                    self.lsp_character_for_cursor(window.buffer_index, cursor_y, window.cx);
                let content_top = self.window_content_top(window);
                let content_width = self.window_content_width(window);
                let content_height = self.window_content_height(window);
                Some(json!({
                    "id": window.id.0,
                    "window_id": window.id.0,
                    "active": Some(window.id) == active_id,
                    "buffer_index": window.buffer_index,
                    "buffer_path": buffer.file,
                    "file": buffer.file,
                    "name": buffer.name(),
                    "revision": buffer.revision(),
                    "bounds": {
                        "x": window.position.x,
                        "y": window.position.y,
                        "width": window.inner_width(),
                        "height": window.inner_height(),
                    },
                    "content_bounds": {
                        "x": window.position.x,
                        "y": window.position.y + content_top,
                        "width": content_width,
                        "height": content_height,
                    },
                    "x": window.position.x,
                    "y": window.position.y,
                    "width": window.inner_width(),
                    "height": window.inner_height(),
                    "content_top": content_top,
                    "content_width": content_width,
                    "content_height": content_height,
                    "vtop": window.vtop,
                    "vleft": window.vleft,
                    "viewport": {
                        "top": window.vtop,
                        "left": window.vleft,
                    },
                    "cursor": {
                        "x": window.cx,
                        "y": cursor_y,
                        "lsp_character": lsp_character,
                    },
                    "lsp_position": {
                        "line": cursor_y,
                        "character": lsp_character,
                    },
                }))
            })
            .collect::<Vec<_>>();
        json!({ "windows": windows })
    }

    fn lsp_character_for_cursor(
        &self,
        buffer_index: usize,
        line: usize,
        grapheme_index: usize,
    ) -> usize {
        self.buffers
            .get(buffer_index)
            .and_then(|buffer| buffer.get(line))
            .map(|text| {
                text.graphemes(true)
                    .take(grapheme_index)
                    .flat_map(str::chars)
                    .map(char::len_utf16)
                    .sum()
            })
            .unwrap_or(grapheme_index)
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
            return display_width_with_tabs(line, self.active_tab_width());
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
        self.current_buffer().get(buffer_line)
    }

    fn gutter_width(&self) -> usize {
        GUTTER_SIGN_COLUMN_WIDTH
            + self
                .current_buffer()
                .len()
                .saturating_add(1)
                .to_string()
                .len()
    }

    fn gutter_width_for_buffer_index(&self, buffer_index: usize) -> usize {
        self.buffers
            .get(buffer_index)
            .map(|buffer| {
                GUTTER_SIGN_COLUMN_WIDTH + buffer.len().saturating_add(1).to_string().len()
            })
            .unwrap_or_else(|| self.gutter_width())
    }

    fn line_number_width_for_window(&self, window: &crate::window::Window) -> usize {
        self.gutter_width_for_window(window)
            .saturating_sub(GUTTER_SIGN_COLUMN_WIDTH)
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

    /// Returns highlight spans positioned relative to the viewport text
    /// (buffer lines `vtop..vtop + height` concatenated, as produced by
    /// `layout_lines`).
    ///
    /// Internally a larger slice (viewport plus margin) is parsed and cached
    /// per buffer, so scrolling line-by-line slices the cached spans instead
    /// of running tree-sitter on every scrolled line.
    fn viewport_highlight_spans(
        &mut self,
        buffer_index: usize,
        vtop: usize,
        height: usize,
    ) -> anyhow::Result<Vec<HighlightSpan>> {
        let Some(buffer) = self.buffers.get(buffer_index) else {
            return Ok(Vec::new());
        };
        let revision = buffer.revision();
        let file = buffer.file.clone();
        let line_count = buffer.len();
        if vtop >= line_count {
            return Ok(Vec::new());
        }
        let viewport_end = (vtop + height).min(line_count);

        let cached = self.highlight_cache.get(&buffer_index);
        let same_document =
            cached.is_some_and(|entry| entry.revision == revision && entry.file == file);
        let covered = same_document
            && cached.is_some_and(|entry| {
                let parse_end = entry.start_line + entry.line_offsets.len().saturating_sub(1);
                entry.start_line <= vtop && parse_end >= viewport_end
            });

        if !covered {
            let _span = perf::PerfSpan::start("highlight:miss");
            // An edit invalidates the cache every keystroke, so keep the
            // margin small there; a same-document miss means scrolling, where
            // a screenful of margin makes held j/k mostly cache hits.
            let margin = if same_document || cached.is_none() {
                height
            } else {
                8
            };
            let mut parse_start = vtop.saturating_sub(margin);
            let mut parse_end = (vtop + height + margin).min(line_count);

            if buffer.line_range_byte_len(parse_start, parse_end) > MAX_HIGHLIGHT_SLICE_BYTES {
                parse_start = vtop;
                parse_end = viewport_end;
                if buffer.line_range_byte_len(parse_start, parse_end) > MAX_HIGHLIGHT_SLICE_BYTES {
                    self.highlight_cache.remove(&buffer_index);
                    return Ok(Vec::new());
                }
            }

            let mut text = String::new();
            let mut line_offsets = Vec::with_capacity(parse_end - parse_start + 1);
            for line in parse_start..parse_end {
                line_offsets.push(text.len());
                if let Some(line) = buffer.get(line) {
                    text.push_str(&line);
                }
            }
            line_offsets.push(text.len());

            let spans = self.highlight_spans(file.as_deref(), &text)?;
            if self.highlight_cache.len() >= 32 {
                self.highlight_cache.clear();
            }
            self.highlight_cache.insert(
                buffer_index,
                ViewportHighlightEntry {
                    revision,
                    file,
                    start_line: parse_start,
                    line_offsets,
                    spans,
                },
            );
        }

        let entry = &self.highlight_cache[&buffer_index];
        let last_offset_index = entry.line_offsets.len() - 1;
        let start_byte = entry.line_offsets[(vtop - entry.start_line).min(last_offset_index)];
        let end_byte = entry.line_offsets[(viewport_end - entry.start_line).min(last_offset_index)];

        Ok(entry
            .spans
            .iter()
            .filter(|span| span.end > start_byte && span.start < end_byte)
            .map(|span| HighlightSpan {
                start: span.start.saturating_sub(start_byte),
                end: span.end - start_byte,
                order: span.order,
                priority: span.priority,
                style: span.style.clone(),
            })
            .collect())
    }

    fn fill_line(&mut self, buffer: &mut RenderBuffer, x: usize, y: usize, style: &Style) {
        let width = self.vwidth().saturating_sub(x);
        let line_fill = " ".repeat(width);
        buffer.set_text(x, y, &line_fill, style);
    }

    fn draw_line(&mut self, buffer: &mut RenderBuffer) {
        let line = self.viewport_line(self.cy).unwrap_or_default();
        let file = self.current_buffer().file.clone();
        let style_info = if line.len() > MAX_HIGHLIGHT_SLICE_BYTES {
            Vec::new()
        } else {
            self.highlight_spans(file.as_deref(), &line)
                .unwrap_or_default()
        };
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
            if x >= self.vwidth() {
                break;
            }
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
        let Some(line_diagnostics) = self.diagnostics.get(&uri) else {
            return;
        };
        let diagnostics = line_diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.range.start.line == line_num)
            .collect::<Vec<_>>();
        if diagnostics.is_empty() {
            return;
        }

        let Some(line) = self.current_buffer().get(line_num) else {
            return;
        };

        let x = self.gutter_width()
            + display_width_with_tabs(line.trim_end_matches('\n'), self.active_tab_width())
            + 5;

        // otherwise, clear the line
        let text = " ".repeat(self.vwidth().saturating_sub(x));
        buffer.set_text(x, line_num - self.vtop, &text, &self.theme.style);

        let prefix = "■".repeat(diagnostics.len());
        let msg = diagnostics[0].message.replace("\n", " ");
        let msg = format!("{} {}", prefix, msg);
        buffer.set_text(x, line_num - self.vtop, &msg, &hint_style);
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
            return grapheme_to_column_with_tabs(
                trim_line_ending(&line),
                self.cx,
                self.active_tab_width(),
            );
        }

        self.cx
    }

    fn refresh_cursor_goal(&mut self) {
        self.cursor_goal = CursorGoal::DisplayCol(self.current_cursor_display_col());
    }

    fn line_goal_limit(&self, line: &str) -> usize {
        let line_width = display_width_with_tabs(line, self.active_tab_width());
        if self.is_insert() {
            line_width
        } else {
            line_width.saturating_sub(1)
        }
    }

    pub(crate) fn display_col_for_cursor_goal(&self, line: &str, goal: CursorGoal) -> usize {
        match goal {
            CursorGoal::DisplayCol(display_col) => {
                let required_width = display_col.saturating_add(usize::from(!self.is_insert()));
                let tab_width = self.active_tab_width();
                let mut width = 0;
                for grapheme in line.graphemes(true) {
                    width += if grapheme == "\t" {
                        tab_width - (width % tab_width)
                    } else {
                        display_width(grapheme)
                    };
                    if width >= required_width {
                        return display_col;
                    }
                }

                display_col.min(if self.is_insert() {
                    width
                } else {
                    width.saturating_sub(1)
                })
            }
            CursorGoal::LineEnd => self.line_goal_limit(line),
        }
    }

    fn grapheme_for_cursor_goal(&self, line: &str, goal: CursorGoal) -> usize {
        match goal {
            CursorGoal::DisplayCol(display_col) => {
                let max_cursor_x = self.max_cursor_x_for_line_length(grapheme_len(line));
                column_to_grapheme_with_tabs(line, display_col, self.active_tab_width())
                    .min(max_cursor_x)
            }
            CursorGoal::LineEnd => self.max_cursor_x_for_line_length(grapheme_len(line)),
        }
    }

    fn apply_cursor_goal_to_current_line(&mut self) {
        if let Some(line) = self.current_line_contents() {
            let line = trim_line_ending(&line);
            self.cx = self.grapheme_for_cursor_goal(line, self.cursor_goal);
        }
    }

    fn current_screen_segment_bounds(&self) -> Option<(usize, usize)> {
        let line_index = self.buffer_line();
        let line = self.current_line_contents()?;
        let line = trim_line_ending(&line);
        let display_col = grapheme_to_column_with_tabs(line, self.cx, self.active_tab_width());
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
        wrap_line_segments(
            trim_line_ending(&line),
            line_index,
            width,
            0,
            self.break_indent_options_for_buffer_index(self.current_buffer_index),
        )
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
        // Preserve the screen column across rows: with break-indent the same
        // screen x maps to different line columns on different rows.
        let screen_x = current.visual_offset + display_col.saturating_sub(current.start_col);
        let target_col = |target: &self::display_layout::LineSegment| {
            target
                .start_col
                .saturating_add(screen_x.saturating_sub(target.visual_offset))
                .min(target.end_col.saturating_sub(1))
        };

        if delta > 0 {
            let mut remaining = delta as usize;
            let mut line = line_index;
            let mut index = current_index;

            loop {
                let segments = self.wrapped_line_segments_for_width(line, width);
                let available_after = segments.len().saturating_sub(index + 1);
                if remaining <= available_after {
                    let target = segments[index + remaining];
                    return Some((target.line, target_col(&target)));
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
                    return Some((target.line, target_col(&target)));
                }
            }
        }

        let mut remaining = delta.unsigned_abs();
        let mut line = line_index;
        let mut index = current_index;

        loop {
            if remaining <= index {
                let target = self.wrapped_line_segments_for_width(line, width)[index - remaining];
                return Some((target.line, target_col(&target)));
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
                return Some((target.line, target_col(&target)));
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
                let col = grapheme_to_column_with_tabs(line, index, self.active_tab_width());
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
        let tab_widths = (0..buffers.len())
            .map(|buffer_index| self.tab_width_for_buffer_index(buffer_index))
            .collect::<Vec<_>>();
        for window in self.window_manager.windows_mut() {
            let buffer_y = window.vtop + window.cy;
            let tab_width = tab_widths.get(window.buffer_index).copied().unwrap_or(4);
            let display_col = buffers
                .get(window.buffer_index)
                .and_then(|buffer| buffer.get(buffer_y))
                .map(|line| {
                    grapheme_to_column_with_tabs(line.trim_end_matches('\n'), window.cx, tab_width)
                })
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
                | Action::FormatDocument
                | Action::CodeAction
                | Action::SignatureHelp
                | Action::StartRename
                | Action::RenameSymbol(_)
                | Action::Print(_)
                | Action::ShowProgress(_)
                | Action::NotifyPlugins(_, _)
                | Action::NotifyPlugin(_, _, _)
                | Action::ResolvePluginRequest(_, _)
                | Action::ViewLogs
                | Action::SetCursor(_, _)
                | Action::SetWaitingKey(_)
                | Action::SetAgentApiKey(_)
        )
    }

    fn event_snapshot(&self) -> EditorEventSnapshot {
        let (width, height) = self
            .window_manager
            .active_window()
            .map(|window| (window.inner_width(), self.window_content_height(window)))
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
            window_id: self.window_manager.active_stable_window_id(),
            window_ids: self
                .window_manager
                .windows()
                .into_iter()
                .map(|window| window.id)
                .collect(),
        }
    }

    fn action_cause(action: &Action) -> String {
        if matches!(action, Action::SetAgentApiKey(_)) {
            return "SetAgentApiKey".to_string();
        }
        if matches!(action, Action::NotifyPlugin(_, _, _))
            || matches!(action, Action::NotifyPlugins(method, _) if method.starts_with("composer:"))
        {
            return "AgentComposer".to_string();
        }
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
        perf::gauge_max("plugin_window_count", after.window_ids.len() as u64);
        let cursor_changed = before.cx != after.cx
            || before.y != after.y
            || before.vtop != after.vtop
            || before.buffer_index != after.buffer_index;
        let viewport_changed = before.vtop != after.vtop
            || before.vleft != after.vleft
            || before.skipcol != after.skipcol
            || before.wrap != after.wrap
            || before.width != after.width
            || before.height != after.height
            || before.buffer_index != after.buffer_index;
        let windows_changed = before.window_ids != after.window_ids
            || before.window_id != after.window_id
            || before.buffer_index != after.buffer_index
            || before.width != after.width
            || before.height != after.height;
        self.refresh_plugin_snapshots(
            runtime,
            cursor_changed || viewport_changed,
            windows_changed,
            false,
        )?;

        let current_window_ids = after.window_ids.iter().copied().collect::<HashSet<_>>();
        for window_id in before
            .window_ids
            .iter()
            .copied()
            .filter(|window_id| !current_window_ids.contains(window_id))
        {
            self.window_bar_manager.close_window(window_id);
            self.plugin_registry
                .notify(
                    runtime,
                    "window:closed",
                    json!({
                        "window_id": window_id.0,
                        "cause": cause,
                    }),
                )
                .await?;
        }

        if before.window_id != after.window_id {
            self.plugin_registry
                .notify(
                    runtime,
                    "window:focused",
                    json!({
                        "window_id": after.window_id.map(|id| id.0),
                        "buffer_index": after.buffer_index,
                        "cause": cause,
                    }),
                )
                .await?;
        }

        if before.window_ids != after.window_ids
            || before.window_id != after.window_id
            || before.width != after.width
            || before.height != after.height
        {
            let mut payload = self.plugin_windows_payload();
            if let Some(object) = payload.as_object_mut() {
                object.insert("cause".to_string(), json!(cause));
            }
            self.plugin_registry
                .notify(runtime, "window:layout_changed", payload)
                .await?;
        }

        if before.window_id == after.window_id && before.buffer_index != after.buffer_index {
            self.plugin_registry
                .notify(
                    runtime,
                    "window:buffer_changed",
                    json!({
                        "window_id": after.window_id.map(|id| id.0),
                        "buffer_index": after.buffer_index,
                        "cause": cause,
                    }),
                )
                .await?;
        }

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

        if cursor_changed {
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
                "viewport_top": after.vtop,
                "buffer_index": after.buffer_index,
                "window_id": after.window_id.map(|id| id.0),
                "lsp_character": self.cursor_lsp_position().character,
            });
            self.plugin_registry
                .notify(runtime, "cursor:moved", cursor_info)
                .await?;
        }

        if viewport_changed {
            let viewport_info = serde_json::json!({
                "vtop": after.vtop,
                "vleft": after.vleft,
                "skipcol": after.skipcol,
                "wrap": after.wrap,
                "width": after.width,
                "height": after.height,
                "buffer_index": after.buffer_index,
                "window_id": after.window_id.map(|id| id.0),
                "cause": cause,
            });
            self.plugin_registry
                .notify(runtime, "viewport:changed", viewport_info)
                .await?;
        }

        Ok(())
    }

    async fn flush_deferred_plugin_event(&mut self, runtime: &mut Runtime) -> anyhow::Result<()> {
        let Some((before, cause)) = self.deferred_plugin_event.take() else {
            return Ok(());
        };
        perf::increment("plugin_event_batches", 1);
        self.notify_editor_event_changes(before, runtime, &cause)
            .await
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
            let top_scrolloff = scrolloff.min(buffer_line);
            let bottom_scrolloff = scrolloff.min(last_line.saturating_sub(buffer_line));
            let mut scrolloff_vtop = self.vtop;
            if buffer_line < scrolloff_vtop + top_scrolloff {
                scrolloff_vtop = buffer_line.saturating_sub(top_scrolloff);
            } else if buffer_line
                >= scrolloff_vtop + viewport_height.saturating_sub(bottom_scrolloff)
            {
                scrolloff_vtop = buffer_line
                    .saturating_add(bottom_scrolloff)
                    .saturating_add(1)
                    .saturating_sub(viewport_height);
            }

            scrolloff_vtop = scrolloff_vtop.min(max_vtop);
            if !self.wrap || self.buffer_line_visible_from(scrolloff_vtop, buffer_line) {
                self.vtop = scrolloff_vtop;
                self.cy = buffer_line.saturating_sub(self.vtop);
            }
        }

        self.clamp_cursor_to_line();
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

    fn sync_agent_visible_buffers(
        &self,
        workspace: &Arc<Mutex<ProposalWorkspace>>,
    ) -> anyhow::Result<()> {
        let mut workspace = workspace
            .lock()
            .map_err(|_| anyhow::anyhow!("proposal workspace lock is poisoned"))?;
        let root = workspace.root().to_path_buf();
        let mut skipped = 0;
        let files = self.buffers.iter().filter_map(|buffer| {
            let file = buffer.file.as_deref()?;
            let path = match Path::new(file).absolutize() {
                Ok(path) => path.to_path_buf(),
                Err(_) => {
                    skipped += 1;
                    return None;
                }
            };
            path.starts_with(&root)
                .then(|| (path, buffer.revision(), buffer.contents()))
        });
        skipped += workspace.replace_visible_files(files)?;
        if skipped > 0 {
            log!(
                "{}",
                json!({
                    "event": "agent_visible_buffers_skipped",
                    "level": "warn",
                    "service": "red",
                    "workspace": root,
                    "count": skipped,
                })
            );
        }
        Ok(())
    }

    fn agent_file_state(
        &self,
        workspace: &ProposalWorkspace,
        path: &Path,
    ) -> anyhow::Result<(u64, String)> {
        let normalized = path.absolutize()?.to_path_buf();
        if let Some(buffer) = self.buffers.iter().find(|buffer| {
            buffer.file.as_deref().is_some_and(|file| {
                Path::new(file)
                    .absolutize()
                    .is_ok_and(|candidate| candidate == normalized)
            })
        }) {
            return Ok((buffer.revision(), buffer.contents()));
        }
        Ok((
            0,
            workspace
                .read_current_file(&normalized)?
                .unwrap_or_default(),
        ))
    }

    fn agent_proposals_payload(&mut self, session_id: &str) -> anyhow::Result<Value> {
        let Some(workspace) = self.agent_workspace.clone() else {
            self.gutter_sign_manager.clear("agent-proposals");
            self.decoration_manager.clear("agent-proposals");
            return Ok(json!({ "files": [] }));
        };
        self.sync_agent_visible_buffers(&workspace)?;
        let mut workspace = workspace
            .lock()
            .map_err(|_| anyhow::anyhow!("proposal workspace lock is poisoned"))?;
        workspace.adopt_recovered_sessions(session_id);
        let mut files = Vec::new();
        let mut signs = Vec::new();
        let mut decorations = Vec::new();
        for proposal_session in workspace.review_sessions(session_id) {
            for path in workspace.pending_files(&proposal_session) {
                let (revision, contents) = match self.agent_file_state(&workspace, &path) {
                    Ok(state) => state,
                    Err(_) => {
                        files.push(json!({
                            "session_id": proposal_session,
                            "path": path,
                            "revision": 0,
                            "conflict": true,
                            "message": "Unable to review this agent proposal safely; pending changes were left intact",
                            "hunks": [],
                        }));
                        continue;
                    }
                };
                match workspace.hunks(&proposal_session, &path, &contents) {
                    Ok(hunks) => {
                        if let Some(buffer_index) = self.buffers.iter().position(|buffer| {
                            buffer.file.as_deref().is_some_and(|file| {
                                Path::new(file)
                                    .absolutize()
                                    .is_ok_and(|candidate| candidate == path)
                            })
                        }) {
                            for hunk in &hunks {
                                let line = self.buffers[buffer_index]
                                    .char_idx_to_position(hunk.old_start)
                                    .line;
                                signs.push(plugin::GutterSign {
                                    buffer_index,
                                    line,
                                    text: "A".to_string(),
                                    style: Style::default(),
                                    priority: 50,
                                });
                                let preview = hunk.new_text.lines().next().unwrap_or_default();
                                decorations.push(plugin::Decoration {
                                    buffer_index: Some(buffer_index),
                                    anchor: plugin::DecorationAnchor::Eol,
                                    line,
                                    column: 0,
                                    text: format!("  + {}", char_prefix(preview, 80)),
                                    style: Style::default(),
                                    priority: 50,
                                    repeat_linebreak: false,
                                    only_whitespace: false,
                                });
                            }
                        }
                        files.push(json!({
                            "session_id": proposal_session,
                            "path": path,
                            "revision": revision,
                            "conflict": false,
                            "hunks": hunks,
                        }));
                    }
                    Err(error) => {
                        if let Some(buffer_index) = self.buffers.iter().position(|buffer| {
                            buffer.file.as_deref().is_some_and(|file| {
                                Path::new(file)
                                    .absolutize()
                                    .is_ok_and(|candidate| candidate == path)
                            })
                        }) {
                            signs.push(plugin::GutterSign {
                                buffer_index,
                                line: 0,
                                text: "!".to_string(),
                                style: Style::default(),
                                priority: 60,
                            });
                        }
                        files.push(json!({
                            "session_id": proposal_session,
                            "path": path,
                            "revision": revision,
                            "conflict": true,
                            "message": error.to_string(),
                            "hunks": [],
                        }));
                    }
                }
            }
        }
        drop(workspace);
        self.gutter_sign_manager
            .set("agent-proposals".to_string(), signs);
        self.decoration_manager
            .set("agent-proposals".to_string(), decorations);
        Ok(json!({ "files": files }))
    }

    async fn apply_agent_disposition(
        &mut self,
        acceptance: StagedProposalAcceptance,
        render_buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        match acceptance.disposition().clone() {
            ProposalDisposition::Applied {
                path,
                contents,
                current_contents,
                session_id,
                turn_id,
                created,
                ..
            } => {
                let normalized = path.absolutize()?.to_path_buf();
                let index = self.buffers.iter().position(|buffer| {
                    buffer.file.as_deref().is_some_and(|file| {
                        Path::new(file)
                            .absolutize()
                            .is_ok_and(|candidate| candidate == normalized)
                    })
                });
                let index = if let Some(index) = index {
                    index
                } else {
                    self.buffers.push(Buffer::new(
                        Some(normalized.to_string_lossy().into_owned()),
                        current_contents,
                    ));
                    self.buffers.len() - 1
                };
                if index != self.current_buffer_index {
                    self.set_current_buffer(render_buffer, index).await?;
                }
                self.commit_agent_acceptance(acceptance)?;
                let end = self
                    .current_buffer()
                    .char_idx_to_position(self.current_buffer().contents().chars().count());
                self.begin_transaction_with_origin(
                    "accept agent proposal",
                    EditOrigin::Agent {
                        session_id: session_id.clone(),
                        turn_id: turn_id.clone(),
                    },
                );
                self.replace_range(
                    TextRange::new(TextPosition::new(/*line*/ 0, /*character*/ 0), end),
                    &contents,
                );
                self.commit_transaction(self.cursor_snapshot());
                if let Err(error) = self.notify_change(runtime).await {
                    self.warn_agent_proposal_post_apply("buffer_changed", &error.to_string());
                }
                if let Err(error) = self.render(render_buffer) {
                    self.warn_agent_proposal_post_apply("render", &error.to_string());
                }
                if let Some(workspace) = self.agent_workspace.clone() {
                    if let Err(error) = self.sync_agent_visible_buffers(&workspace) {
                        self.warn_agent_proposal_post_apply("workspace_sync", &error.to_string());
                    }
                }
                if let Err(error) = self
                    .plugin_registry
                    .notify(
                        runtime,
                        "agent:proposal_applied",
                        json!({
                            "session_id": session_id,
                            "turn_id": turn_id,
                            "path": path,
                            "created": created,
                        }),
                    )
                    .await
                {
                    self.warn_agent_proposal_post_apply("plugin_notification", &error.to_string());
                }
            }
            ProposalDisposition::Conflict {
                path,
                base,
                current,
                proposed,
            } => {
                self.commit_agent_acceptance(acceptance)?;
                self.plugin_registry
                    .notify(
                        runtime,
                        "agent:proposal_conflict",
                        json!({
                            "path": path,
                            "base": base,
                            "current": current,
                            "proposed": proposed,
                        }),
                    )
                    .await?;
            }
            ProposalDisposition::NoChanges => {
                self.commit_agent_acceptance(acceptance)?;
                self.plugin_registry
                    .notify(runtime, "agent:proposal_unchanged", json!({}))
                    .await?;
            }
        }
        Ok(())
    }

    fn warn_agent_proposal_post_apply(&mut self, stage: &str, error: &str) {
        log!(
            "{}",
            json!({
                "event": "agent_proposal_notification_failed",
                "level": "warn",
                "service": "red",
                "stage": stage,
                "error": error,
            })
        );
        let action = match stage {
            "buffer_changed" => "change notification",
            "workspace_sync" => "workspace sync",
            "plugin_notification" => "plugin notification",
            other => other,
        };
        self.last_error = Some(format!(
            "Agent proposal applied, but {action} failed: {error}"
        ));
    }

    fn commit_agent_acceptance(&self, acceptance: StagedProposalAcceptance) -> anyhow::Result<()> {
        let workspace = self
            .agent_workspace
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no proposal workspace is active"))?;
        workspace
            .lock()
            .map_err(|_| anyhow::anyhow!("proposal workspace lock is poisoned"))?
            .commit_acceptance(acceptance)
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
        let _perf_session = perf::PerfSession::start();
        let interactive_startup = perf::PerfSpan::start("startup:interactive");
        terminal::enable_raw_mode()?;
        self.stdout
            .execute(event::EnableMouseCapture)?
            .execute(event::EnableFocusChange)?
            .execute(event::EnableBracketedPaste)?
            .execute(terminal::EnterAlternateScreen)?
            .execute(terminal::Clear(terminal::ClearType::All))?;

        let mut runtime;
        {
            let plugin_startup = perf::PerfSpan::start("startup:plugins");
            runtime = Runtime::try_new_with_permissions(self.config.plugin_permissions.clone())?;
            runtime.set_typecheck_enabled(!self.config.disable_plugin_typecheck);
            self.refresh_plugin_snapshots(&mut runtime, true, true, true)?;
            for (name, path) in &self.config.plugins {
                let path = Config::resolve_plugin_path(path);
                self.plugin_registry.add(name, path.as_str());
            }
            self.plugin_registry.initialize(&mut runtime).await?;
            self.plugin_registry
                .notify(&mut runtime, "editor:ready", json!({}))
                .await?;
            if let Some(transcript) = self
                .preferences
                .plugin_storage("agent", "transcript")
                .and_then(Value::as_str)
            {
                self.plugin_registry
                    .notify(
                        &mut runtime,
                        "agent:transcript_restored",
                        json!({ "transcript": transcript }),
                    )
                    .await?;
            }
            drop(plugin_startup);
        }

        let mut buffer = RenderBuffer::new(
            self.size.0 as usize,
            self.size.1 as usize,
            &Style::default(),
        );
        self.ensure_current_buffer_lsp_opened().await?;
        self.render(&mut buffer)?;
        drop(interactive_startup);
        let mut pending_events = VecDeque::new();

        'editor_loop: loop {
            // Wait for input, but at most 10ms so LSP messages, timers, and
            // plugin requests are still serviced on a steady tick. Unlike an
            // unconditional sleep, this wakes the moment a key arrives.
            if pending_events.is_empty() {
                event::poll(Duration::from_millis(10))?;
            }

            while let Some(ev) = Self::read_ready_event(&mut pending_events)? {
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
                    perf::increment("repeated_motion_batches", 1);
                    if self
                        .drain_repeated_motion_events(
                            signature,
                            &mut pending_events,
                            &mut buffer,
                            &mut runtime,
                        )
                        .await?
                    {
                        break 'editor_loop;
                    }
                    // Give plugin effects, timers, and LSP responses one turn
                    // before another held-key batch is drained.
                    break;
                }
            }
            self.suppress_reactivation_click = false;

            self.service_background(&mut buffer, &mut runtime).await?;
            self.persist_session_snapshot(/*force*/ false);
        }

        self.shutdown_services(&mut runtime).await;

        Ok(())
    }

    async fn shutdown_services(&mut self, runtime: &mut Runtime) {
        drop(self.agent_bridge.take());
        if let Some(task) = self.agent_task.take() {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => log!("ACP adapter shutdown failed: {error}"),
                Err(error) => log!("ACP adapter task failed: {error}"),
            }
        }

        let snapshot = self.editor_state_snapshot();
        if let Err(err) = self.plugin_registry.before_exit(runtime, snapshot).await {
            log!("Plugin beforeExit failed: {}", err);
        }
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::SetPluginStorage { plugin, key, value } => {
                    if let Err(err) = self.preferences.set_plugin_storage(&plugin, &key, value) {
                        log!("Plugin storage flush failed: {}", err);
                    }
                }
                request => {
                    log!(
                        "Dropping plugin request during shutdown: {}",
                        request.label()
                    );
                }
            }
        }
        self.persist_session_snapshot(/*force*/ true);
        if let Err(err) = self.plugin_registry.deactivate_all(runtime).await {
            log!("Plugin deactivate failed: {}", err);
        }
    }

    fn abort_agent_bridge(&mut self) {
        drop(self.agent_bridge.take());
        if let Some(task) = self.agent_task.take() {
            task.abort();
        }
        self.agent_active_sessions.clear();
    }

    fn select_codex_agent_backend(&mut self) {
        self.config.agent.adapter = Some("codex".to_string());
        self.config.agent.command = None;
        self.config.agent.args.clear();
        self.agent_session_api_key = None;
        self.config.agent.env.remove("OPENAI_API_KEY");
        self.abort_agent_bridge();
    }

    async fn service_background(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        // Poll for timer callbacks
        let timer_callbacks = crate::plugin::poll_timer_callbacks();
        for callback_request in timer_callbacks {
            if let PluginRequest::TimeoutCallback { timer_id } = callback_request {
                self.plugin_registry
                    .notify(runtime, "timeout:callback", json!({ "timer_id": timer_id }))
                    .await?;
            }
        }

        for event in runtime.poll_process_events() {
            let Some(process_id) = event.get("process_id").and_then(Value::as_str) else {
                continue;
            };
            self.plugin_registry
                .notify(runtime, &format!("process:{process_id}"), event)
                .await?;
        }

        for (watch_id, payload) in self.poll_directory_watchers() {
            self.plugin_registry
                .notify(runtime, &format!("filesystem:changed:{watch_id}"), payload)
                .await?;
        }
        self.plugin_registry.poll_hot_reload(runtime).await;

        let mut proposal_sessions = Vec::new();
        for _ in 0..ACP_EVENTS_PER_TICK {
            let Some(event) = self.agent_bridge.as_mut().and_then(AcpBridge::try_recv) else {
                break;
            };
            if let BridgeEvent::Completed { session_id, .. }
            | BridgeEvent::Failed {
                session_id: Some(session_id),
                ..
            } = &event
            {
                self.agent_active_sessions.remove(session_id.0.as_ref());
            }
            match &event {
                BridgeEvent::Update { session_id, .. }
                    if !self.agent_active_sessions.contains(session_id.0.as_ref()) =>
                {
                    continue;
                }
                BridgeEvent::PermissionRequested {
                    request_id,
                    session_id,
                    ..
                } if !self.agent_active_sessions.contains(session_id.0.as_ref()) => {
                    if let Some(bridge) = &self.agent_bridge {
                        let _ = bridge.try_send(BridgeCommand::PermissionResponse {
                            request_id: request_id.clone(),
                            option_id: None,
                        });
                    }
                    continue;
                }
                _ => {}
            }
            if let BridgeEvent::Update { session_id, .. }
            | BridgeEvent::Completed { session_id, .. }
            | BridgeEvent::ProposalsChanged { session_id } = &event
            {
                let session_id = session_id.to_string();
                if !proposal_sessions.contains(&session_id) {
                    proposal_sessions.push(session_id);
                }
            }
            if matches!(event, BridgeEvent::ProposalsChanged { .. }) {
                continue;
            }
            let (name, payload) = agent_event_payload(event);
            self.plugin_registry.notify(runtime, name, payload).await?;
        }
        for session_id in proposal_sessions {
            self.plugin_registry
                .notify(
                    runtime,
                    "agent:proposals_changed",
                    json!({ "session_id": session_id }),
                )
                .await?;
        }
        if self
            .agent_task
            .as_ref()
            .is_some_and(tokio::task::JoinHandle::is_finished)
            && self
                .agent_bridge
                .as_ref()
                .is_none_or(|bridge| !bridge.has_pending_events())
        {
            let _ = self
                .agent_task
                .take()
                .expect("finished ACP task must exist")
                .await;
            self.agent_bridge = None;
            self.agent_active_sessions.clear();
            self.plugin_registry
                .notify(
                    runtime,
                    "agent:session_lost",
                    json!({ "message": "ACP adapter stopped" }),
                )
                .await?;
        }

        let dialog_changed = if let Some(current_dialog) = &mut self.current_dialog {
            current_dialog.tick()?
        } else {
            false
        };
        if dialog_changed {
            self.render(buffer)?;
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

        // Coalesce background work (LSP messages, plugin requests) into a
        // single render at the end of the tick instead of one per item.
        let mut needs_render = false;
        let mut needs_motion_render = false;
        let mut agent_proposal_applied = false;

        // Always pump LSP responses. `recv_response` completes the
        // initialize handshake and flushes queued didOpen/change
        // messages, so it must not depend on diagnostic display.
        match self.lsp.recv_response().await {
            Ok(Some((msg, method))) => {
                if let Some(action) = self.handle_lsp_message(&msg, method) {
                    // Numeric progress tokens (e.g. rust-analyzer indexing)
                    // don't change anything the editor core draws; plugins
                    // that visualize them request their own redraws.
                    let progress_only = matches!(
                        &action,
                        Action::ShowProgress(progress)
                            if matches!(progress.token, ProgressToken::Number(_))
                    );
                    // TODO: handle quit
                    let generation_before = self.render_generation;
                    self.execute(&action, buffer, runtime).await?;
                    if !progress_only && self.render_generation == generation_before {
                        needs_render = true;
                    }
                }
            }
            Ok(None) => {}
            Err(err) => {
                log!("ERROR: Lsp error: {err}");
            }
        }

        // Startup refreshes form short request chains. Drain a bounded batch so each
        // operation does not wait for a separate 10 ms editor tick.
        for _ in 0..PLUGIN_REQUESTS_PER_TICK {
            if self
                .agent_task
                .as_ref()
                .is_some_and(tokio::task::JoinHandle::is_finished)
                && self
                    .agent_bridge
                    .as_ref()
                    .is_some_and(AcpBridge::has_pending_events)
            {
                break;
            }
            let Some(req) = ACTION_DISPATCHER.try_recv_request() else {
                break;
            };
            let _span = perf::PerfSpan::with_detail("drain", req.label());
            match req {
                PluginRequest::Action(action) => {
                    // let current_buffer = buffer.clone();
                    self.execute(&action, buffer, runtime).await?;
                    needs_render = true;
                    // self.redraw(runtime, &current_buffer, buffer).await?;
                }
                PluginRequest::AgentOpenApiKeyPrompt => {
                    self.agent_secret_input_active = true;
                    self.current_dialog = Some(Box::new(InputPrompt::secret(
                        self,
                        "OpenAI API key (session only)",
                        Action::SetAgentApiKey,
                    )));
                    self.render(buffer)?;
                }
                PluginRequest::AgentUseCodex => {
                    if crate::agent_check::find_executable_on_path("codex").is_none() {
                        self.plugin_registry
                            .notify(
                                runtime,
                                "agent:error",
                                json!({
                                    "message": "Codex CLI was not found; install Codex, run `codex login`, and try again"
                                }),
                            )
                            .await?;
                        continue;
                    }
                    self.select_codex_agent_backend();
                    self.plugin_registry
                        .notify(runtime, "agent:backend_ready", json!({}))
                        .await?;
                }
                PluginRequest::AgentNewSession { cwd } => {
                    if self
                        .agent_task
                        .as_ref()
                        .is_some_and(tokio::task::JoinHandle::is_finished)
                    {
                        let _ = self
                            .agent_task
                            .take()
                            .expect("finished ACP task must exist")
                            .await;
                        self.agent_bridge = None;
                        self.agent_active_sessions.clear();
                        self.plugin_registry
                            .notify(
                                runtime,
                                "agent:session_lost",
                                json!({ "message": "ACP adapter stopped" }),
                            )
                            .await?;
                    }
                    let built_in_openai = self.config.agent.command.is_none()
                        && self.config.agent.adapter.as_deref() == Some("openai");
                    let built_in_codex = self.config.agent.command.is_none()
                        && self.config.agent.adapter.as_deref() == Some("codex");
                    let inherited_api_key = std::env::var_os("OPENAI_API_KEY");
                    let openai_key_ready = openai_api_key_ready(
                        self.agent_session_api_key.as_deref(),
                        self.config
                            .agent
                            .env
                            .get("OPENAI_API_KEY")
                            .map(String::as_str),
                        inherited_api_key.as_deref(),
                    );
                    let result = if self.config.disable_ai {
                        Err(anyhow::anyhow!(
                            "agent support is disabled by `disable_ai = true`"
                        ))
                    } else if built_in_openai && !openai_key_ready {
                        Err(anyhow::anyhow!(
                            "OpenAI API key required; choose OpenAI setup to enter a session-only key"
                        ))
                    } else if let Some(error) = self
                        .agent_workspace
                        .as_ref()
                        .and_then(|workspace| match workspace.lock() {
                            Ok(workspace) => match cwd.absolutize() {
                                Ok(cwd) if workspace.root() == cwd.as_ref() => None,
                                Ok(cwd) => Some(anyhow::anyhow!(
                                    "ACP session root `{}` does not match the active proposal workspace `{}`",
                                    cwd.display(),
                                    workspace.root().display()
                                )),
                                Err(error) => Some(anyhow::Error::new(error)),
                            },
                            Err(_) => Some(anyhow::anyhow!(
                                "proposal workspace lock is poisoned"
                            )),
                        })
                    {
                        Err(error)
                    } else {
                        if self.agent_bridge.is_none() {
                            let command = crate::agent_check::resolve_adapter_command(&self.config)
                                .ok_or_else(|| {
                                    anyhow::anyhow!(
                                        "no ACP adapter is configured; run `red --agent-check`"
                                    )
                                });
                            match command {
                                Ok(command) => {
                                    let start = (|| -> anyhow::Result<_> {
                                        let workspace = match &self.agent_workspace {
                                            Some(workspace) => Arc::clone(workspace),
                                            None => {
                                                Arc::new(Mutex::new(ProposalWorkspace::new(&cwd)?))
                                            }
                                        };
                                        self.sync_agent_visible_buffers(&workspace)?;
                                        let mut spec = AcpProcessSpec::new(command)
                                            .args(self.config.agent.args.clone())
                                            .current_dir(cwd.clone());
                                        spec.environment.extend(
                                            self.config
                                                .agent
                                                .env
                                                .clone()
                                                .into_iter()
                                                .map(|(key, value)| (key.into(), value.into())),
                                        );
                                        if built_in_openai {
                                            if let Some(api_key) = &self.agent_session_api_key {
                                                spec.environment.insert(
                                                    "OPENAI_API_KEY".into(),
                                                    api_key.clone().into(),
                                                );
                                            }
                                            spec = spec.authentication_method("openai_api_key");
                                        } else if built_in_codex {
                                            spec = spec.authentication_method("codex_login");
                                        }
                                        let capacity = NonZeroUsize::new(AGENT_BRIDGE_CAPACITY)
                                            .expect("agent bridge capacity is non-zero");
                                        let host = ProposalAcpHost::new(Arc::clone(&workspace));
                                        let spawned = start_bridge(spec, host, capacity)?;
                                        self.agent_workspace = Some(workspace);
                                        Ok(spawned)
                                    })();
                                    match start {
                                        Ok((bridge, task)) => {
                                            self.agent_bridge = Some(bridge);
                                            self.agent_task = Some(task);
                                            Ok(())
                                        }
                                        Err(error) => Err(error),
                                    }
                                }
                                Err(error) => Err(error),
                            }
                        } else {
                            Ok(())
                        }
                    };
                    if let Err(error) = result {
                        self.plugin_registry
                            .notify(
                                runtime,
                                "agent:error",
                                json!({ "message": error.to_string() }),
                            )
                            .await?;
                        continue;
                    }
                    let Some(bridge) = &self.agent_bridge else {
                        continue;
                    };
                    if bridge
                        .send(BridgeCommand::NewSession { cwd })
                        .await
                        .is_err()
                    {
                        self.abort_agent_bridge();
                        self.plugin_registry
                            .notify(
                                runtime,
                                "agent:session_lost",
                                json!({ "message": "ACP adapter stopped" }),
                            )
                            .await?;
                    }
                }
                PluginRequest::AgentPrompt { session_id, text } => {
                    if self.agent_bridge.is_none()
                        || self
                            .agent_task
                            .as_ref()
                            .is_some_and(tokio::task::JoinHandle::is_finished)
                    {
                        self.abort_agent_bridge();
                        self.plugin_registry
                            .notify(
                                runtime,
                                "agent:session_lost",
                                json!({
                                    "session_id": session_id,
                                    "prompt": text,
                                    "message": "no ACP session is running"
                                }),
                            )
                            .await?;
                        continue;
                    }
                    if self.agent_active_sessions.contains(&session_id) {
                        self.last_error =
                            Some("an ACP prompt is already active for this session".to_string());
                        needs_render = true;
                        continue;
                    }
                    let turn_id = uuid::Uuid::new_v4().to_string();
                    if let Some(workspace) = self.agent_workspace.clone() {
                        if let Err(error) = self.sync_agent_visible_buffers(&workspace) {
                            self.plugin_registry
                                .notify(
                                    runtime,
                                    "agent:error",
                                    json!({
                                        "session_id": session_id,
                                        "message": error.to_string()
                                    }),
                                )
                                .await?;
                            continue;
                        }
                        workspace
                            .lock()
                            .map_err(|_| anyhow::anyhow!("proposal workspace lock is poisoned"))?
                            .begin_turn(&session_id, turn_id.clone());
                    }
                    self.plugin_registry
                        .notify(
                            runtime,
                            "agent:turn_started",
                            json!({ "session_id": session_id, "turn_id": turn_id }),
                        )
                        .await?;
                    self.agent_active_sessions.insert(session_id.clone());
                    let Some(bridge) = &self.agent_bridge else {
                        continue;
                    };
                    if let Err(error) = bridge
                        .send(BridgeCommand::Prompt {
                            session_id: agent_client_protocol_schema::v1::SessionId::new(
                                session_id.clone(),
                            ),
                            text,
                        })
                        .await
                    {
                        let prompt = match error.0 {
                            BridgeCommand::Prompt { text, .. } => text,
                            _ => String::new(),
                        };
                        self.abort_agent_bridge();
                        self.plugin_registry
                            .notify(
                                runtime,
                                "agent:session_lost",
                                json!({
                                    "session_id": session_id,
                                    "prompt": prompt,
                                    "message": "ACP adapter stopped"
                                }),
                            )
                            .await?;
                    }
                }
                PluginRequest::AgentCancel { session_id } => {
                    let Some(bridge) = &self.agent_bridge else {
                        continue;
                    };
                    if bridge
                        .send(BridgeCommand::Cancel {
                            session_id: agent_client_protocol_schema::v1::SessionId::new(
                                session_id,
                            ),
                        })
                        .await
                        .is_err()
                    {
                        self.plugin_registry
                            .notify(
                                runtime,
                                "agent:error",
                                json!({ "message": "ACP adapter stopped" }),
                            )
                            .await?;
                    }
                }
                PluginRequest::AgentCloseSession { session_id } => {
                    self.agent_active_sessions.remove(&session_id);
                    if let Some(workspace) = &self.agent_workspace {
                        workspace
                            .lock()
                            .map_err(|_| anyhow::anyhow!("proposal workspace lock is poisoned"))?
                            .archive_session(&session_id);
                    }
                    let Some(bridge) = &self.agent_bridge else {
                        continue;
                    };
                    if bridge
                        .send(BridgeCommand::CloseSession {
                            session_id: agent_client_protocol_schema::v1::SessionId::new(
                                session_id,
                            ),
                        })
                        .await
                        .is_err()
                    {
                        self.plugin_registry
                            .notify(
                                runtime,
                                "agent:error",
                                json!({ "message": "ACP adapter stopped" }),
                            )
                            .await?;
                    }
                }
                PluginRequest::AgentArchiveSession { session_id } => {
                    self.agent_active_sessions.remove(&session_id);
                    if let Some(workspace) = &self.agent_workspace {
                        workspace
                            .lock()
                            .map_err(|_| anyhow::anyhow!("proposal workspace lock is poisoned"))?
                            .archive_session(&session_id);
                    }
                }
                PluginRequest::AgentProposals {
                    session_id,
                    request_id,
                } => {
                    let mut payload = match self.agent_proposals_payload(&session_id) {
                        Ok(payload) => payload,
                        Err(_) => {
                            let message =
                                "Unable to review agent proposals safely; pending changes were left intact";
                            self.last_error = Some(message.to_string());
                            needs_render = true;
                            json!({ "files": [], "error": message })
                        }
                    };
                    payload["request_id"] = json!(request_id.get());
                    self.plugin_registry
                        .resolve_request(runtime, request_id, payload)
                        .await?;
                }
                PluginRequest::AgentAcceptProposal {
                    session_id,
                    path,
                    hunk_id,
                } => {
                    let acceptance = (|| -> anyhow::Result<_> {
                        let workspace = self
                            .agent_workspace
                            .clone()
                            .ok_or_else(|| anyhow::anyhow!("no proposal workspace is active"))?;
                        self.sync_agent_visible_buffers(&workspace)?;
                        let workspace = workspace
                            .lock()
                            .map_err(|_| anyhow::anyhow!("proposal workspace lock is poisoned"))?;
                        let (revision, contents) = self.agent_file_state(&workspace, &path)?;
                        Ok(if let Some(hunk_id) = hunk_id.as_deref() {
                            workspace.stage_accept_hunk(
                                &session_id,
                                &path,
                                hunk_id,
                                revision,
                                &contents,
                            )?
                        } else {
                            workspace.stage_accept_all(&session_id, &path, revision, &contents)?
                        })
                    })();
                    let acceptance = match acceptance {
                        Ok(acceptance) => acceptance,
                        Err(_) => {
                            self.last_error = Some(
                                "Unable to accept agent proposal safely; pending changes were left intact"
                                    .to_string(),
                            );
                            needs_render = true;
                            continue;
                        }
                    };
                    let applied = matches!(
                        acceptance.disposition(),
                        ProposalDisposition::Applied { .. }
                    );
                    if self
                        .apply_agent_disposition(acceptance, buffer, runtime)
                        .await
                        .is_err()
                    {
                        self.last_error = Some(
                            "Unable to accept agent proposal safely; pending changes were left intact"
                                .to_string(),
                        );
                        needs_render = true;
                        continue;
                    }
                    agent_proposal_applied |= applied;
                }
                PluginRequest::AgentRejectProposal {
                    session_id,
                    path,
                    hunk_id,
                } => {
                    let rejected = (|| -> anyhow::Result<()> {
                        let workspace = self
                            .agent_workspace
                            .clone()
                            .ok_or_else(|| anyhow::anyhow!("no proposal workspace is active"))?;
                        self.sync_agent_visible_buffers(&workspace)?;
                        let mut workspace = workspace
                            .lock()
                            .map_err(|_| anyhow::anyhow!("proposal workspace lock is poisoned"))?;
                        let (revision, contents) = self.agent_file_state(&workspace, &path)?;
                        if let Some(hunk_id) = hunk_id.as_deref() {
                            workspace.reject_hunk(
                                &session_id,
                                &path,
                                hunk_id,
                                revision,
                                &contents,
                            )?;
                        } else {
                            workspace.reject_all(&session_id, &path, revision, &contents)?;
                        }
                        Ok(())
                    })();
                    if rejected.is_err() {
                        self.last_error = Some(
                            "Unable to reject agent proposal safely; pending changes were left intact"
                                .to_string(),
                        );
                        needs_render = true;
                        continue;
                    }
                    self.plugin_registry
                        .notify(
                            runtime,
                            "agent:proposals_changed",
                            json!({ "session_id": session_id }),
                        )
                        .await?;
                }
                PluginRequest::AgentPermissionResponse {
                    request_id,
                    option_id,
                } => {
                    let Some(bridge) = &self.agent_bridge else {
                        continue;
                    };
                    if bridge
                        .send(BridgeCommand::PermissionResponse {
                            request_id,
                            option_id,
                        })
                        .await
                        .is_err()
                    {
                        self.plugin_registry
                            .notify(
                                runtime,
                                "agent:error",
                                json!({ "message": "ACP adapter stopped" }),
                            )
                            .await?;
                    }
                }
                PluginRequest::EditHistory { request_id } => {
                    let payload = json!({
                        "request_id": request_id.get(),
                        "entries": self.current_buffer().undo_history.undo_tree(),
                    });
                    self.plugin_registry
                        .resolve_request(runtime, request_id, payload)
                        .await?;
                }
                PluginRequest::OpenLocation { location, target } => {
                    self.execute(&Action::OpenLocation(location, target), buffer, runtime)
                        .await?;
                }
                PluginRequest::EditorInfo(request_id) => {
                    let mut info = serde_json::to_value(self.info())?;
                    info["request_id"] = json!(request_id.get());
                    self.plugin_registry
                        .resolve_request(runtime, request_id, info)
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
                    self.execute(&Action::OpenPicker(title, items, id), buffer, runtime)
                        .await?;
                    // self.render(buffer)?;
                }
                PluginRequest::OpenLivePicker(title, id, items, options) => {
                    let items = items
                        .iter()
                        .map(|v| match v {
                            serde_json::Value::String(s) => s.clone(),
                            val => val.to_string(),
                        })
                        .collect();
                    self.execute(
                        &Action::OpenLivePicker(title, items, id, options),
                        buffer,
                        runtime,
                    )
                    .await?;
                }
                PluginRequest::OpenAgentComposer {
                    owner,
                    title,
                    id,
                    query,
                    history,
                } => {
                    if id == 802 {
                        if let Err(error) = self.preferences.remove_picker_history("picker:802") {
                            log!("failed to remove legacy agent picker history: {error}");
                        }
                    }
                    self.current_dialog = Some(Box::new(AgentComposer::new(
                        self, title, id, query, history, owner,
                    )));
                    needs_render = true;
                }
                PluginRequest::OpenDynamicPicker {
                    title,
                    id,
                    items,
                    options,
                } => {
                    let history_key = Self::picker_history_key(&title, Some(id));
                    let mut picker = Picker::new_dynamic(title, self, items, id, options);
                    if let Some(history_key) = history_key {
                        let history = self.picker_history(&history_key).to_vec();
                        picker.set_history(history_key, history);
                    }
                    self.current_dialog = Some(Box::new(picker));
                    needs_render = true;
                }
                PluginRequest::UpdatePickerItems { id, items } => {
                    if let Some(dialog) = &mut self.current_dialog {
                        dialog.update_picker(id, PickerUpdate::Items(items));
                    }
                    needs_render = true;
                }
                PluginRequest::UpdatePickerQuery { id, query } => {
                    if let Some(dialog) = &mut self.current_dialog {
                        dialog.update_picker(id, PickerUpdate::Query(query));
                    }
                    needs_render = true;
                }
                PluginRequest::UpdatePickerStatus { id, status } => {
                    if let Some(dialog) = &mut self.current_dialog {
                        dialog.update_picker(id, PickerUpdate::Status(status));
                    }
                    needs_render = true;
                }
                PluginRequest::UpdatePickerPreview { id, preview } => {
                    if let Some(dialog) = &mut self.current_dialog {
                        dialog.update_picker(id, PickerUpdate::Preview(preview));
                    }
                    needs_render = true;
                }
                PluginRequest::ClosePicker { id } => {
                    if self
                        .current_dialog
                        .as_ref()
                        .is_some_and(|dialog| dialog.picker_id() == Some(id))
                    {
                        self.current_dialog = None;
                        needs_render = true;
                    }
                }
                PluginRequest::BufferInsert { x, y, text } => {
                    self.begin_transaction("plugin insert");
                    self.replace_range(TextRange::insertion(TextPosition::new(y, x)), &text);
                    self.commit_transaction(self.cursor_snapshot());
                    self.notify_change(runtime).await?;
                    needs_render = true;
                }
                PluginRequest::BufferDelete { x, y, length } => {
                    self.begin_transaction("plugin delete");
                    self.replace_range(
                        TextRange::new(TextPosition::new(y, x), TextPosition::new(y, x + length)),
                        "",
                    );
                    self.commit_transaction(self.cursor_snapshot());
                    self.notify_change(runtime).await?;
                    needs_render = true;
                }
                PluginRequest::BufferReplace { x, y, length, text } => {
                    self.begin_transaction("plugin replace");
                    self.replace_range(
                        TextRange::new(TextPosition::new(y, x), TextPosition::new(y, x + length)),
                        &text,
                    );
                    self.commit_transaction(self.cursor_snapshot());
                    self.notify_change(runtime).await?;
                    needs_render = true;
                }
                PluginRequest::GetCursorPosition { request_id } => {
                    let pos = serde_json::json!({
                        "x": self.cx,
                        "y": self.cy + self.vtop
                    });
                    self.plugin_registry
                        .resolve_request(runtime, request_id, pos)
                        .await?;
                }
                PluginRequest::GetCursorDisplayColumn { request_id } => {
                    let display_col = if let Some(line) = self.current_line_contents() {
                        let line = line.trim_end_matches('\n');
                        grapheme_to_column_with_tabs(line, self.cx, self.active_tab_width())
                    } else {
                        self.cx
                    };
                    let pos = serde_json::json!({
                        "column": display_col,
                        "y": self.cy + self.vtop
                    });
                    self.plugin_registry
                        .resolve_request(runtime, request_id, pos)
                        .await?;
                }
                PluginRequest::SetCursorPosition { x, y } => {
                    self.cx = x;
                    let viewport_height = self.vheight().max(1);
                    // Adjust viewport if needed
                    if y < self.vtop {
                        self.vtop = y;
                        self.cy = 0;
                    } else if y >= self.vtop + viewport_height {
                        self.vtop = y.saturating_sub(viewport_height - 1);
                        self.cy = viewport_height - 1;
                    } else {
                        self.cy = y - self.vtop;
                    }
                    self.draw_cursor()?;
                }
                PluginRequest::SetCursorDisplayColumn { column, y } => {
                    // Convert display column to character index
                    if let Some(line) = self.viewport_line(y.saturating_sub(self.vtop)) {
                        let line = line.trim_end_matches('\n');
                        self.cx =
                            column_to_grapheme_with_tabs(line, column, self.active_tab_width());
                    }
                    let viewport_height = self.vheight().max(1);
                    // Adjust viewport if needed
                    if y < self.vtop {
                        self.vtop = y;
                        self.cy = 0;
                    } else if y >= self.vtop + viewport_height {
                        self.vtop = y.saturating_sub(viewport_height - 1);
                        self.cy = viewport_height - 1;
                    } else {
                        self.cy = y - self.vtop;
                    }
                    self.draw_cursor()?;
                }
                PluginRequest::GetBufferText {
                    request_id,
                    start_line,
                    end_line,
                } => {
                    let current_buf = self.current_buffer();
                    let text = match (start_line, end_line) {
                        (None, None) => current_buf.contents(),
                        (start, end) => current_buf
                            .line_range_contents(start.unwrap_or(0), end.unwrap_or(usize::MAX)),
                    };
                    self.plugin_registry
                        .resolve_request(runtime, request_id, serde_json::json!({ "text": text }))
                        .await?;
                }
                PluginRequest::GetSelection { request_id } => {
                    let selection = self.selection.map(|selection| {
                        json!({
                            "start": { "x": selection.x0, "y": selection.y0 },
                            "end": { "x": selection.x1, "y": selection.y1 },
                            "buffer_index": self.current_buffer_index,
                            "mode": format!("{:?}", self.mode),
                        })
                    });
                    self.plugin_registry
                        .resolve_request(runtime, request_id, selection.unwrap_or(Value::Null))
                        .await?;
                }
                PluginRequest::OpenScratchBuffer {
                    request_id,
                    name,
                    text,
                } => {
                    self.buffers.push(Buffer::new(Some(name), text));
                    let buffer_index = self.buffers.len() - 1;
                    self.set_current_buffer(buffer, buffer_index).await?;
                    self.plugin_registry
                        .resolve_request(
                            runtime,
                            request_id,
                            json!({ "buffer_index": buffer_index }),
                        )
                        .await?;
                    needs_render = true;
                }
                PluginRequest::CloseScratchBuffer { buffer_index } => {
                    if buffer_index == self.current_buffer_index {
                        self.delete_current_buffer(buffer, true).await?;
                        needs_render = true;
                    }
                }
                PluginRequest::GetViewportLayout { request_id } => {
                    let mut payload = self.plugin_viewport_layout_payload();
                    payload["request_id"] = json!(request_id.get());
                    self.plugin_registry
                        .resolve_request(runtime, request_id, payload)
                        .await?;
                }
                PluginRequest::GetWindows { request_id } => {
                    let payload = self.plugin_windows_payload();
                    self.plugin_registry
                        .resolve_request(runtime, request_id, payload)
                        .await?;
                }
                PluginRequest::SetDecorations {
                    namespace,
                    decorations,
                } => {
                    let current_buffer_index = self.current_buffer_index;
                    let active_buffer_only = decorations.iter().all(|decoration| {
                        decoration
                            .buffer_index
                            .is_none_or(|buffer_index| buffer_index == current_buffer_index)
                    });
                    let decorations = decorations
                        .into_iter()
                        .map(|mut decoration| {
                            decoration.buffer_index.get_or_insert(current_buffer_index);
                            decoration
                        })
                        .collect();
                    if self.decoration_manager.set(namespace, decorations) {
                        if active_buffer_only && self.window_manager.windows().len() == 1 {
                            needs_motion_render = true;
                        } else {
                            needs_render = true;
                        }
                    }
                }
                PluginRequest::ClearDecorations { namespace } => {
                    if self.decoration_manager.clear(&namespace) {
                        if self.window_manager.windows().len() == 1 {
                            needs_motion_render = true;
                        } else {
                            needs_render = true;
                        }
                    }
                }
                PluginRequest::SetGutterSigns { namespace, signs } => {
                    if self.gutter_sign_manager.set(namespace, signs) {
                        if self.window_manager.windows().len() == 1 {
                            needs_motion_render = true;
                        } else {
                            needs_render = true;
                        }
                    }
                }
                PluginRequest::ClearGutterSigns { namespace } => {
                    if self.gutter_sign_manager.clear(&namespace) {
                        if self.window_manager.windows().len() == 1 {
                            needs_motion_render = true;
                        } else {
                            needs_render = true;
                        }
                    }
                }
                PluginRequest::GetConfig { request_id, key } => {
                    let config_value = if let Some(key) = key {
                        // Return specific config value
                        match key.as_str() {
                            "theme" => json!(self.config.theme),
                            "plugins" => json!(self.config.plugins),
                            "plugin_config" => json!(self.config.plugin_config),
                            "log_file" => json!(self.config.log_file),
                            "mouse_scroll_lines" => json!(self.config.mouse_scroll_lines),
                            "scrolloff" => json!(self.config.scrolloff),
                            "show_diagnostics" => json!(self.config.show_diagnostics),
                            "startup_file_count" => json!(self.config.startup_file_count),
                            "cwd" => json!(std::env::current_dir()
                                .ok()
                                .map(|path| path.to_string_lossy().to_string())),
                            "executable" => json!(std::env::current_exe()
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
                            "plugin_config": self.config.plugin_config,
                            "log_file": self.config.log_file,
                            "mouse_scroll_lines": self.config.mouse_scroll_lines,
                            "scrolloff": self.config.scrolloff,
                            "show_diagnostics": self.config.show_diagnostics,
                            "startup_file_count": self.config.startup_file_count,
                            "cwd": std::env::current_dir().ok().map(|path| path.to_string_lossy().to_string()),
                            "executable": std::env::current_exe().ok().map(|path| path.to_string_lossy().to_string()),
                            "keys": self.config.keys,
                        })
                    };
                    self.plugin_registry
                        .resolve_request(runtime, request_id, json!({ "value": config_value }))
                        .await?;
                }
                PluginRequest::GetPluginStorage {
                    plugin,
                    key,
                    request_id,
                } => {
                    let value = self
                        .preferences
                        .plugin_storage(&plugin, &key)
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    self.plugin_registry
                        .resolve_request(runtime, request_id, json!({ "value": value }))
                        .await?;
                }
                PluginRequest::SetPluginStorage { plugin, key, value } => {
                    self.preferences.set_plugin_storage(&plugin, &key, value)?;
                }
                PluginRequest::GetEditorState { request_id } => {
                    let snapshot = self.editor_state_snapshot();
                    self.plugin_registry
                        .resolve_request(runtime, request_id, serde_json::to_value(snapshot)?)
                        .await?;
                }
                PluginRequest::RestoreEditorState {
                    request_id,
                    snapshot,
                } => {
                    let before = self.event_snapshot();
                    let result = self.restore_editor_state(snapshot, buffer).await;
                    let restored = result.as_ref().is_ok_and(|result| result.restored);
                    if restored {
                        self.notify_editor_event_changes(before, runtime, "RestoreEditorState")
                            .await?;
                        let mut restored_payload = self.plugin_windows_payload();
                        if let Some(object) = restored_payload.as_object_mut() {
                            object.insert("cause".to_string(), json!("RestoreEditorState"));
                        }
                        self.plugin_registry
                            .notify(runtime, "editor:state_restored", restored_payload)
                            .await?;
                    }
                    let payload = match result {
                        Ok(result) => serde_json::to_value(result)?,
                        Err(err) => json!({
                            "restored": false,
                            "opened_files": [],
                            "skipped_files": [],
                            "warnings": [err.to_string()],
                        }),
                    };
                    self.plugin_registry
                        .resolve_request(runtime, request_id, payload)
                        .await?;
                    needs_render = true;
                }
                PluginRequest::DocumentSymbols {
                    request_id,
                    buffer_index,
                } => {
                    let buffer_index = buffer_index.unwrap_or(self.current_buffer_index);
                    let Some(target_buffer) = self.buffers.get(buffer_index) else {
                        self.plugin_registry
                            .resolve_request(
                                runtime,
                                request_id,
                                plugin_lsp_error("requested buffer does not exist"),
                            )
                            .await?;
                        continue;
                    };
                    let Some(file) = target_buffer.file.clone() else {
                        self.plugin_registry
                            .resolve_request(
                                runtime,
                                request_id,
                                plugin_lsp_error("requested buffer is not file-backed"),
                            )
                            .await?;
                        continue;
                    };
                    let revision = target_buffer.revision();

                    let request_result: anyhow::Result<i64> = async {
                        self.ensure_buffer_lsp_opened(buffer_index).await?;
                        Ok(self.lsp.document_symbols(&file).await?)
                    }
                    .await;

                    match request_result {
                        Ok(lsp_request_id) if lsp_request_id > 0 => {
                            self.pending_plugin_document_symbols.insert(
                                lsp_request_id,
                                PendingDocumentSymbols {
                                    plugin_request_id: request_id,
                                    buffer_index,
                                    revision,
                                },
                            );
                        }
                        Ok(_) => {
                            self.plugin_registry
                                .resolve_request(
                                    runtime,
                                    request_id,
                                    plugin_lsp_error(
                                        "no language server is available for this file",
                                    ),
                                )
                                .await?;
                        }
                        Err(err) => {
                            self.plugin_registry
                                .resolve_request(
                                    runtime,
                                    request_id,
                                    plugin_lsp_error(&err.to_string()),
                                )
                                .await?;
                        }
                    }
                }
                PluginRequest::ResolveThemeStyle { request_id, spec } => {
                    let payload = serde_json::to_value(self.theme.resolve_style(&spec))?;
                    self.plugin_registry
                        .resolve_request(runtime, request_id, payload)
                        .await?;
                }
                PluginRequest::ListRuntimeAssets { kind, request_id } => {
                    let payload =
                        match crate::assets::list_runtime_assets(kind, &Config::config_dir()) {
                            Ok(entries) => json!({
                                "kind": kind.dir_name(),
                                "entries": entries
                                    .into_iter()
                                    .map(|entry| json!({
                                        "file": entry.file,
                                        "name": entry.name,
                                        "source": entry.source.to_string(),
                                        "shadows": entry
                                            .shadows
                                            .into_iter()
                                            .map(|source| source.to_string())
                                            .collect::<Vec<_>>(),
                                    }))
                                    .collect::<Vec<_>>(),
                                "error": null,
                            }),
                            Err(err) => json!({
                                "kind": kind.dir_name(),
                                "entries": [],
                                "error": err.to_string(),
                            }),
                        };
                    self.plugin_registry
                        .resolve_request(runtime, request_id, payload)
                        .await?;
                }
                PluginRequest::WorkspaceSymbols { request_id, query } => {
                    let Some(file) = self.current_buffer().file.clone() else {
                        self.plugin_registry
                            .resolve_request(
                                runtime,
                                request_id,
                                plugin_lsp_error("current buffer is not file-backed"),
                            )
                            .await?;
                        continue;
                    };

                    let request_result: anyhow::Result<i64> = async {
                        self.ensure_current_buffer_lsp_opened().await?;
                        Ok(self.lsp.workspace_symbol_for_file(&file, &query).await?)
                    }
                    .await;

                    match request_result {
                        Ok(lsp_request_id) if lsp_request_id > 0 => {
                            self.pending_plugin_workspace_symbols
                                .insert(lsp_request_id, request_id);
                        }
                        Ok(_) => {
                            self.plugin_registry
                                .resolve_request(
                                    runtime,
                                    request_id,
                                    plugin_lsp_error(
                                        "no language server is available for this file",
                                    ),
                                )
                                .await?;
                        }
                        Err(err) => {
                            self.plugin_registry
                                .resolve_request(
                                    runtime,
                                    request_id,
                                    plugin_lsp_error(&err.to_string()),
                                )
                                .await?;
                        }
                    }
                }
                PluginRequest::References {
                    request_id,
                    include_declaration,
                } => {
                    let Some(file) = self.current_buffer().file.clone() else {
                        self.plugin_registry
                            .resolve_request(
                                runtime,
                                request_id,
                                plugin_lsp_error("current buffer is not file-backed"),
                            )
                            .await?;
                        continue;
                    };
                    let position = self.cursor_lsp_position();

                    let request_result: anyhow::Result<i64> = async {
                        self.ensure_current_buffer_lsp_opened().await?;
                        Ok(self
                            .lsp
                            .references(
                                &file,
                                position.character,
                                position.line,
                                include_declaration,
                            )
                            .await?)
                    }
                    .await;

                    match request_result {
                        Ok(lsp_request_id) if lsp_request_id > 0 => {
                            self.pending_plugin_references
                                .insert(lsp_request_id, request_id);
                        }
                        Ok(_) => {
                            self.plugin_registry
                                .resolve_request(
                                    runtime,
                                    request_id,
                                    plugin_lsp_error(
                                        "no language server is available for this file",
                                    ),
                                )
                                .await?;
                        }
                        Err(err) => {
                            self.plugin_registry
                                .resolve_request(
                                    runtime,
                                    request_id,
                                    plugin_lsp_error(&err.to_string()),
                                )
                                .await?;
                        }
                    }
                }
                PluginRequest::InlayHints { request_id, range } => {
                    let Some(file) = self.current_buffer().file.clone() else {
                        self.plugin_registry
                            .resolve_request(
                                runtime,
                                request_id,
                                plugin_lsp_error("current buffer is not file-backed"),
                            )
                            .await?;
                        continue;
                    };

                    let range = range.unwrap_or_else(|| {
                        let contents = self.current_buffer().contents();
                        let line = contents
                            .as_bytes()
                            .iter()
                            .filter(|byte| **byte == b'\n')
                            .count();
                        let character = contents
                            .rsplit('\n')
                            .next()
                            .unwrap_or_default()
                            .trim_end_matches('\r')
                            .chars()
                            .map(char::len_utf16)
                            .sum();
                        Range {
                            start: crate::lsp::Position {
                                line: 0,
                                character: 0,
                            },
                            end: crate::lsp::Position { line, character },
                        }
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
                                .resolve_request(
                                    runtime,
                                    request_id,
                                    plugin_lsp_error(
                                        "no language server is available for this file",
                                    ),
                                )
                                .await?;
                        }
                        Err(err) => {
                            self.plugin_registry
                                .resolve_request(
                                    runtime,
                                    request_id,
                                    plugin_lsp_error(&err.to_string()),
                                )
                                .await?;
                        }
                    }
                }
                PluginRequest::GetTextDisplayWidth { request_id, text } => {
                    let width = crate::unicode_utils::display_width(&text);
                    self.plugin_registry
                        .resolve_request(runtime, request_id, json!({ "width": width }))
                        .await?;
                }
                PluginRequest::CharIndexToDisplayColumn { request_id, x, y } => {
                    let display_col = if let Some(line) = self.current_buffer().get(y) {
                        let line = line.trim_end_matches('\n');
                        crate::unicode_utils::char_to_column(line, x)
                    } else {
                        x
                    };
                    self.plugin_registry
                        .resolve_request(runtime, request_id, json!({ "column": display_col }))
                        .await?;
                }
                PluginRequest::DisplayColumnToCharIndex {
                    request_id,
                    column,
                    y,
                } => {
                    let char_index = if let Some(line) = self.current_buffer().get(y) {
                        let line = line.trim_end_matches('\n');
                        crate::unicode_utils::column_to_char(line, column)
                    } else {
                        column
                    };
                    self.plugin_registry
                        .resolve_request(runtime, request_id, json!({ "index": char_index }))
                        .await?;
                }
                PluginRequest::IntervalCallback { interval_id } => {
                    self.plugin_registry
                        .notify(
                            runtime,
                            "interval:callback",
                            json!({ "interval_id": interval_id }),
                        )
                        .await?;
                }
                PluginRequest::TimeoutCallback { timer_id } => {
                    self.plugin_registry
                        .notify(runtime, "timeout:callback", json!({ "timer_id": timer_id }))
                        .await?;
                }
                PluginRequest::CreateOverlay { id, config } => {
                    log!("Creating overlay: {}", id);
                    self.overlay_manager.create_overlay(id, config);
                }
                PluginRequest::UpdateOverlay { id, lines } => {
                    if let Some(overlay) = self.overlay_manager.get_overlay_mut(&id) {
                        if overlay.update_content(lines) {
                            needs_render = true;
                        }
                    }
                }
                PluginRequest::RemoveOverlay { id } => {
                    log!("Removing overlay: {}", id);
                    if self.overlay_manager.remove_overlay(&id).is_some() {
                        needs_render = true;
                    }
                }
                PluginRequest::CreatePanel { id, config } => {
                    self.panel_manager.create_panel(id, config);
                    self.apply_panel_layout();
                    needs_render = true;
                }
                PluginRequest::UpdatePanel { id, rows } => {
                    self.panel_manager.update_panel(&id, rows);
                    needs_render = true;
                }
                PluginRequest::CreateTextPanel { id, config } => {
                    self.panel_manager.create_text_panel(id, config);
                    self.apply_panel_layout();
                    needs_render = true;
                }
                PluginRequest::UpdateTextPanel { id, blocks } => {
                    self.panel_manager.update_text_panel(
                        &id,
                        blocks,
                        usize::from(self.size.1.saturating_sub(2)),
                    );
                    needs_render = true;
                }
                PluginRequest::AppendTextPanel {
                    id,
                    block_id,
                    delta,
                } => {
                    self.panel_manager.append_text_panel(
                        &id,
                        &block_id,
                        &delta,
                        usize::from(self.size.1.saturating_sub(2)),
                    );
                    needs_render = true;
                }
                PluginRequest::SelectPanelRow { id, row_id } => {
                    if self.panel_manager.select_row_by_id(
                        &id,
                        &row_id,
                        usize::from(self.size.1.saturating_sub(2)),
                    ) {
                        needs_render = true;
                    }
                }
                PluginRequest::FocusPanel { id } => {
                    self.panel_manager.focus_panel(&id);
                    needs_render = true;
                }
                PluginRequest::FocusEditor => {
                    self.panel_manager.focus_editor();
                    needs_render = true;
                }
                PluginRequest::ClosePanel { id } => {
                    self.panel_manager.close_panel(&id);
                    self.apply_panel_layout();
                    needs_render = true;
                }
                PluginRequest::OpenWorkspace { id, config } => {
                    self.workspace_manager.open(id, config);
                    needs_render = true;
                }
                PluginRequest::UpdateWorkspace { id, model } => {
                    if self.workspace_manager.update(&id, model) {
                        needs_render = true;
                    }
                }
                PluginRequest::CloseWorkspace { id } => {
                    if self.workspace_manager.close(&id) {
                        needs_render = true;
                    }
                }
                PluginRequest::CreateWindowBar { id, config } => {
                    if self.window_bar_manager.create(id, config) {
                        needs_render = true;
                    }
                }
                PluginRequest::UpdateWindowBar {
                    id,
                    window_id,
                    segments,
                } => {
                    if self
                        .window_bar_manager
                        .update(&id, WindowId(window_id), segments)
                    {
                        if Some(WindowId(window_id))
                            == self.window_manager.active_stable_window_id()
                        {
                            needs_motion_render = true;
                        } else {
                            needs_render = true;
                        }
                    }
                }
                PluginRequest::CloseWindowBar { id, window_id } => {
                    let changed = match window_id {
                        Some(window_id) => self
                            .window_bar_manager
                            .clear_window(&id, WindowId(window_id)),
                        None => self.window_bar_manager.close(&id),
                    };
                    if changed {
                        needs_render = true;
                    }
                }
                PluginRequest::ListDirectory { path, request_id } => {
                    let payload = directory_listing(&path);
                    self.plugin_registry
                        .resolve_request(runtime, request_id, payload)
                        .await?;
                }
                PluginRequest::GetGitStatus { path, request_id } => {
                    let payload = git_status_listing(&path);
                    self.plugin_registry
                        .resolve_request(runtime, request_id, payload)
                        .await?;
                }
                PluginRequest::WatchDirectory {
                    path,
                    watch_id,
                    recursive,
                    interval_ms,
                } => {
                    self.directory_watchers.insert(
                        watch_id,
                        DirectoryWatcher {
                            snapshot: directory_snapshot(&path, recursive),
                            path,
                            last_checked: Instant::now(),
                            recursive,
                            interval: Duration::from_millis(interval_ms.max(100)),
                        },
                    );
                }
                PluginRequest::UnwatchDirectory { watch_id } => {
                    self.directory_watchers.remove(&watch_id);
                }
            }
        }
        if needs_render {
            if let Err(error) = self.render(buffer) {
                if agent_proposal_applied {
                    self.warn_agent_proposal_post_apply("render", &error.to_string());
                } else {
                    return Err(error);
                }
            }
        } else if needs_motion_render {
            self.render_motion_frame(buffer)?;
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
        let sensitive_input = self.agent_secret_input_active
            || self
                .current_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.is_sensitive_input());
        let _span = perf::enabled().then(|| {
            let detail = if sensitive_input {
                format!("sensitive_input {render_mode:?}")
            } else {
                format!("{:?} {render_mode:?}", ev)
            };
            perf::PerfSpan::with_detail("event", detail)
        });
        let initial_bounds_span = perf::PerfSpan::start("event:initial_bounds");
        self.check_bounds();
        drop(initial_bounds_span);

        if let event::Event::Resize(width, height) = ev {
            self.size = (width, height);
            let max_y = (height as usize).saturating_sub(2);
            self.cy = self.cy.min(max_y.saturating_sub(1));
            self.resize_window_layout((width as usize, height as usize));
            *buffer = RenderBuffer::new(
                self.size.0 as usize,
                self.size.1 as usize,
                &Style::default(),
            );
            let viewport_width = self.vwidth();
            let viewport_height = self.vheight();
            let dialog_resized = if let Some(dialog) = &mut self.current_dialog {
                dialog.resize(viewport_width, viewport_height)
            } else {
                false
            };
            if self.current_dialog.is_some() && !dialog_resized {
                self.current_dialog = None;
            }
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
        let started_in_normal = self.is_normal();
        let semantic_can_start = started_in_normal || self.is_visual();
        let was_recording_macro = self.macro_recording.is_some();
        let resolve_span = perf::PerfSpan::start("event:resolve_action");
        let action = self.handle_event(&ev)?;
        if !sensitive_input {
            if was_recording_macro && self.macro_recording.is_some() {
                self.record_macro_event(&ev);
            }
            self.record_semantic_change_event(
                &ev,
                action.as_ref(),
                started_in_normal,
                semantic_can_start,
            );
        }
        drop(resolve_span);
        if let Some(action) = action {
            drain_repeated_motion =
                !from_waiting_key_action && self.should_drain_repeated_motion(&ev, &action);
            let action_span = perf::PerfSpan::start("event:handle_action");
            let should_quit = self
                .handle_key_action(&ev, &action, buffer, runtime)
                .await?;
            drop(action_span);
            if should_quit {
                self.finish_semantic_change_event();
                return Ok(ProcessedEvent {
                    quit: true,
                    drain_repeated_motion: false,
                    repeat_signature: None,
                });
            }
        }
        let semantic_span = perf::PerfSpan::start("event:finish_semantic_change");
        self.finish_semantic_change_event();
        drop(semantic_span);

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
        pending_events: &mut VecDeque<Event>,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<bool> {
        let started = Instant::now();
        let mut deferred_motion = false;
        while started.elapsed() < Duration::from_millis(REPEATED_MOTION_DRAIN_BUDGET_MS) {
            let Some(ev) = Self::read_ready_event(pending_events)? else {
                break;
            };
            let same_key = Self::key_signature(&ev).is_some_and(|next| next == signature);
            if !same_key {
                if deferred_motion {
                    self.flush_deferred_plugin_event(runtime).await?;
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
        while let Some(ev) = Self::read_ready_event(pending_events)? {
            if Self::key_signature(&ev).is_some_and(|next| next == signature) {
                continue;
            }

            if deferred_motion {
                self.flush_deferred_plugin_event(runtime).await?;
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
            self.flush_deferred_plugin_event(runtime).await?;
            self.flush_deferred_motion_render(buffer)?;
        }

        Ok(false)
    }

    /// Reads the next ready terminal event while collapsing only adjacent
    /// resize notifications. Crossterm can emit resize events in batches;
    /// rendering every intermediate size makes terminal divider drags laggy.
    /// Keeping non-resize events queued preserves their original ordering.
    fn read_ready_event(pending_events: &mut VecDeque<Event>) -> anyhow::Result<Option<Event>> {
        let first = if let Some(event) = pending_events.pop_front() {
            event
        } else {
            if !event::poll(Duration::from_millis(0))? {
                return Ok(None);
            }
            event::read()?
        };

        if matches!(first, Event::Resize(_, _)) {
            while event::poll(Duration::from_millis(0))? {
                pending_events.push_back(event::read()?);
            }
        }

        Ok(Some(Self::coalesce_resize_run(first, pending_events)))
    }

    fn coalesce_resize_run(first: Event, pending_events: &mut VecDeque<Event>) -> Event {
        let Event::Resize(mut width, mut height) = first else {
            return first;
        };

        while let Some(Event::Resize(next_width, next_height)) = pending_events.front() {
            width = *next_width;
            height = *next_height;
            pending_events.pop_front();
        }

        Event::Resize(width, height)
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

    fn record_semantic_change_event(
        &mut self,
        event: &Event,
        action: Option<&KeyAction>,
        started_in_normal: bool,
        semantic_can_start: bool,
    ) {
        if self.replaying_semantic_change || !Self::event_is_replayable_input(event) {
            return;
        }

        if started_in_normal && action.is_some_and(Self::key_action_repeats_last_change) {
            self.pending_semantic_change = None;
            return;
        }

        if self.pending_semantic_change.is_none() {
            if !semantic_can_start {
                return;
            }
            self.pending_semantic_change = Some(PendingSemanticChange {
                buffer_id: self.current_buffer().id(),
                base_revision: self.current_buffer().revision(),
                events: Vec::new(),
            });
        }

        if let Some(change) = &mut self.pending_semantic_change {
            change.events.push(event.clone());
        }
    }

    fn key_action_repeats_last_change(action: &KeyAction) -> bool {
        match action {
            KeyAction::Single(Action::RepeatLastChange) => true,
            KeyAction::Multiple(actions) => actions
                .iter()
                .any(|action| matches!(action, Action::RepeatLastChange)),
            KeyAction::Repeating(_, action) => Self::key_action_repeats_last_change(action),
            KeyAction::None | KeyAction::Single(_) | KeyAction::Nested(_) => false,
        }
    }

    fn record_macro_event(&mut self, event: &Event) {
        let Some(recording) = &mut self.macro_recording else {
            return;
        };
        if Self::macro_event_notation(event).is_some() {
            recording.events.push(event.clone());
        }
    }

    fn finish_semantic_change_event(&mut self) {
        if self.replaying_semantic_change {
            return;
        }
        let Some(change) = &self.pending_semantic_change else {
            return;
        };
        if change.buffer_id != self.current_buffer().id() {
            self.pending_semantic_change = None;
            return;
        }

        let pending_input = !self.is_normal()
            || self.waiting_key_action.is_some()
            || self.pending_operator.is_some()
            || self.pending_character_motion.is_some()
            || self.pending_replace
            || self.pending_visual_text_object_scope.is_some()
            || self.pending_macro_action.is_some()
            || self.pending_mark_action.is_some()
            || self.repeater.is_some()
            || self.transaction_active();
        if pending_input {
            return;
        }

        let change = self
            .pending_semantic_change
            .take()
            .expect("completed semantic change must exist");
        if self.current_buffer().revision() != change.base_revision && !change.events.is_empty() {
            self.last_semantic_change = Some(SemanticChange {
                events: change.events,
            });
        }
    }

    fn event_is_replayable_input(event: &Event) -> bool {
        matches!(
            event,
            Event::Paste(_)
                | Event::Key(KeyEvent {
                    kind: KeyEventKind::Press | KeyEventKind::Repeat,
                    ..
                })
        )
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
                | Action::FindCharForward { .. }
                | Action::TillCharForward { .. }
                | Action::MatchitForward
                | Action::MatchitBackward
                | Action::MatchitPreviousUnmatched
                | Action::MatchitNextUnmatched
        )
    }

    fn action_is_selection_motion(action: &Action) -> bool {
        matches!(
            action,
            Action::FindNext
                | Action::FindPrevious
                | Action::RepeatSearch
                | Action::RepeatSearchOpposite
                | Action::SearchWordUnderCursor
                | Action::MoveUp
                | Action::MoveDown
                | Action::MoveLeft
                | Action::MoveRight
                | Action::MoveToLineEnd
                | Action::MoveToLineStart
                | Action::MoveToFirstLineChar
                | Action::MoveToLastLineChar
                | Action::MoveScreenLineUp
                | Action::MoveScreenLineDown
                | Action::MoveToScreenLineEnd
                | Action::MoveToScreenLineStart
                | Action::MoveToScreenLineFirstNonBlank
                | Action::MoveToBottom
                | Action::MoveToTop
                | Action::MoveTo(_, _)
                | Action::MoveToNextWord
                | Action::MoveToPreviousWord
                | Action::FindCharForward { .. }
                | Action::TillCharForward { .. }
                | Action::MoveToFilePercent(_)
                | Action::MatchitForward
                | Action::MatchitBackward
                | Action::MatchitPreviousUnmatched
                | Action::MatchitNextUnmatched
                | Action::PageDown
                | Action::PageUp
                | Action::GoToLine(_)
        )
    }

    fn selection_motion_subset(action: &KeyAction) -> Option<KeyAction> {
        match action {
            KeyAction::Single(action) if Self::action_is_selection_motion(action) => {
                Some(KeyAction::Single(action.clone()))
            }
            KeyAction::Multiple(actions)
                if !actions.is_empty() && actions.iter().all(Self::action_is_selection_motion) =>
            {
                Some(KeyAction::Multiple(actions.clone()))
            }
            KeyAction::Nested(mappings) => {
                let mappings = mappings
                    .iter()
                    .filter_map(|(key, action)| {
                        Self::selection_motion_subset(action).map(|action| (key.clone(), action))
                    })
                    .collect::<HashMap<_, _>>();
                (!mappings.is_empty()).then_some(KeyAction::Nested(mappings))
            }
            KeyAction::Repeating(times, action) => Self::selection_motion_subset(action)
                .map(|action| KeyAction::Repeating(*times, Box::new(action))),
            KeyAction::None | KeyAction::Single(_) | KeyAction::Multiple(_) => None,
        }
    }

    fn merge_key_mappings(
        mappings: &mut HashMap<String, KeyAction>,
        overrides: &HashMap<String, KeyAction>,
    ) {
        for (key, action) in overrides {
            if let (Some(KeyAction::Nested(mappings)), KeyAction::Nested(overrides)) =
                (mappings.get_mut(key), action)
            {
                Self::merge_key_mappings(mappings, overrides);
            } else {
                mappings.insert(key.clone(), action.clone());
            }
        }
    }

    fn visual_key_mappings(&self) -> HashMap<String, KeyAction> {
        let mut mappings = self
            .config
            .keys
            .normal
            .iter()
            .filter_map(|(key, action)| {
                Self::selection_motion_subset(action).map(|action| (key.clone(), action))
            })
            .collect::<HashMap<_, _>>();

        Self::merge_key_mappings(&mut mappings, &self.config.keys.visual);
        match self.mode {
            Mode::VisualLine => {
                Self::merge_key_mappings(&mut mappings, &self.config.keys.visual_line)
            }
            Mode::VisualBlock => {
                Self::merge_key_mappings(&mut mappings, &self.config.keys.visual_block)
            }
            Mode::Visual | Mode::Normal | Mode::Insert | Mode::Command | Mode::Search => {}
        }
        mappings
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

    async fn replay_last_semantic_change(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let Some(change) = self.last_semantic_change.clone() else {
            self.last_error = Some("no change to repeat".to_string());
            return Ok(());
        };

        self.pending_semantic_change = None;
        self.replaying_semantic_change = true;
        let result = async {
            for event in &change.events {
                if let Some(action) = self.handle_event(event)? {
                    if self
                        .handle_key_action(event, &action, buffer, runtime)
                        .await?
                    {
                        anyhow::bail!("repeated change attempted to quit the editor");
                    }
                }
            }
            Ok(())
        }
        .await;
        self.replaying_semantic_change = false;
        result
    }

    async fn play_macro(
        &mut self,
        register: char,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let Some(register) = normalize_macro_register(register) else {
            self.last_error = Some("macro register must be a letter or digit".to_string());
            return Ok(());
        };
        let depth = self.macro_replay_depth.saturating_add(1);
        if depth > MACRO_MAX_REPLAY_DEPTH {
            self.last_error = Some(format!(
                "macro recursion limit ({MACRO_MAX_REPLAY_DEPTH}) reached"
            ));
            return Ok(());
        }
        let Some(content) = self.registers.get(&register) else {
            self.last_error = Some(format!("macro register @{register} is empty"));
            return Ok(());
        };
        let events = match Self::macro_events_from_notation(&content.text) {
            Ok(events) => events,
            Err(error) => {
                self.last_error = Some(error.to_string());
                return Ok(());
            }
        };

        self.last_played_macro = Some(register);

        for event in events.into_iter().rev() {
            self.macro_replay_queue
                .push_front(MacroReplayEvent { event, depth });
        }

        // A nested @ invocation only expands the active replay queue. The outermost
        // invocation owns the iterative driver, so self-recursive registers cannot
        // consume the async call stack before the deterministic limits fire.
        if self.macro_replay_depth > 0 {
            return Ok(());
        }

        self.macro_instructions_remaining = MACRO_MAX_REPLAY_EVENTS;
        let result = async {
            while let Some(replay) = self.macro_replay_queue.pop_front() {
                if self.macro_instructions_remaining == 0 {
                    self.last_error = Some(format!(
                        "macro instruction limit ({MACRO_MAX_REPLAY_EVENTS}) reached"
                    ));
                    self.macro_replay_queue.clear();
                    break;
                }
                self.macro_instructions_remaining -= 1;
                self.macro_replay_depth = replay.depth;
                if let Some(action) = self.handle_event(&replay.event)? {
                    if self
                        .handle_key_action(&replay.event, &action, buffer, runtime)
                        .await?
                    {
                        anyhow::bail!("macro attempted to quit the editor");
                    }
                }
            }
            Ok(())
        }
        .await;
        self.macro_replay_depth = 0;
        self.macro_replay_queue.clear();
        result
    }

    fn macro_event_notation(event: &Event) -> Option<String> {
        let Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) = event
        else {
            return None;
        };

        if *modifiers == KeyModifiers::CONTROL {
            return match code {
                KeyCode::Char(character) => Some(format!("<C-{character}>")),
                _ => None,
            };
        }
        if *modifiers == KeyModifiers::ALT {
            return match code {
                KeyCode::Char(character) => Some(format!("<A-{character}>")),
                _ => None,
            };
        }
        match code {
            KeyCode::Char('<') => Some("<lt>".to_string()),
            KeyCode::Char(character) => Some(character.to_string()),
            KeyCode::Esc => Some("<Esc>".to_string()),
            KeyCode::Enter => Some("<CR>".to_string()),
            KeyCode::Backspace => Some("<BS>".to_string()),
            KeyCode::Tab => Some("<Tab>".to_string()),
            KeyCode::BackTab => Some("<S-Tab>".to_string()),
            KeyCode::Up => Some("<Up>".to_string()),
            KeyCode::Down => Some("<Down>".to_string()),
            KeyCode::Left => Some("<Left>".to_string()),
            KeyCode::Right => Some("<Right>".to_string()),
            KeyCode::Home => Some("<Home>".to_string()),
            KeyCode::End => Some("<End>".to_string()),
            KeyCode::PageUp => Some("<PageUp>".to_string()),
            KeyCode::PageDown => Some("<PageDown>".to_string()),
            _ => None,
        }
    }

    fn macro_events_from_notation(notation: &str) -> anyhow::Result<Vec<Event>> {
        let mut events = Vec::new();
        let mut cursor = 0;
        while cursor < notation.len() {
            let suffix = &notation[cursor..];
            if suffix.starts_with('<') {
                let end = suffix
                    .find('>')
                    .ok_or_else(|| anyhow::anyhow!("macro notation has an unterminated key"))?;
                let token = &suffix[1..end];
                let (code, modifiers) = match token {
                    "lt" => (KeyCode::Char('<'), KeyModifiers::NONE),
                    "Esc" => (KeyCode::Esc, KeyModifiers::NONE),
                    "CR" | "Enter" => (KeyCode::Enter, KeyModifiers::NONE),
                    "BS" => (KeyCode::Backspace, KeyModifiers::NONE),
                    "Tab" => (KeyCode::Tab, KeyModifiers::NONE),
                    "S-Tab" => (KeyCode::BackTab, KeyModifiers::SHIFT),
                    "Up" => (KeyCode::Up, KeyModifiers::NONE),
                    "Down" => (KeyCode::Down, KeyModifiers::NONE),
                    "Left" => (KeyCode::Left, KeyModifiers::NONE),
                    "Right" => (KeyCode::Right, KeyModifiers::NONE),
                    "Home" => (KeyCode::Home, KeyModifiers::NONE),
                    "End" => (KeyCode::End, KeyModifiers::NONE),
                    "PageUp" => (KeyCode::PageUp, KeyModifiers::NONE),
                    "PageDown" => (KeyCode::PageDown, KeyModifiers::NONE),
                    _ if token.starts_with("C-") => (
                        KeyCode::Char(single_macro_token_character(&token[2..])?),
                        KeyModifiers::CONTROL,
                    ),
                    _ if token.starts_with("A-") => (
                        KeyCode::Char(single_macro_token_character(&token[2..])?),
                        KeyModifiers::ALT,
                    ),
                    _ => anyhow::bail!("unsupported macro key notation <{token}>"),
                };
                events.push(Event::Key(KeyEvent::new(code, modifiers)));
                cursor += end + 1;
            } else {
                let character = suffix
                    .chars()
                    .next()
                    .expect("non-empty macro notation suffix has a character");
                events.push(Event::Key(KeyEvent::new(
                    KeyCode::Char(character),
                    KeyModifiers::NONE,
                )));
                cursor += character.len_utf8();
            }
        }
        Ok(events)
    }

    fn add_diagnostics(&mut self, uri: Option<&str>, diagnostics: &[Diagnostic]) -> Option<Action> {
        let Some(uri) = uri else {
            log!("WARN: no uri provided for diagnostics - {diagnostics:?}");
            return None;
        };

        let uri = lsp_normalized_file_path(uri)
            .ok()
            .and_then(|path| crate::lsp::file_uri(path).ok())
            .unwrap_or_else(|| uri.to_string());
        log!("Adding diagnostics for {uri}: {diagnostics:#?}");
        self.diagnostics.insert(uri, diagnostics.to_vec());

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

        let line = self.current_buffer().get(request_line)?;
        let mut units = 0usize;
        let mut request_char_index = None;
        for (index, character) in line.trim_end_matches(['\r', '\n']).chars().enumerate() {
            if units == request_character {
                request_char_index = Some(index);
                break;
            }
            units += character.len_utf16();
            if units > request_character {
                return None;
            }
        }
        let request_character = request_char_index.or_else(|| {
            (units == request_character)
                .then(|| line.trim_end_matches(['\r', '\n']).chars().count())
        })?;
        let current_character = self.grapheme_to_char_on_line(self.cx, self.buffer_line());
        if current_character < request_character {
            return None;
        }

        Some(
            line.chars()
                .skip(request_character)
                .take(current_character - request_character)
                .collect(),
        )
    }

    fn take_pending_plugin_request(&mut self, method: &str, id: i64) -> Option<RequestId> {
        Some(match method {
            "textDocument/documentSymbol" => {
                self.pending_plugin_document_symbols
                    .remove(&id)?
                    .plugin_request_id
            }
            "workspace/symbol" => self.pending_plugin_workspace_symbols.remove(&id)?,
            "textDocument/references" => self.pending_plugin_references.remove(&id)?,
            "textDocument/inlayHint" => self.pending_plugin_inlay_hints.remove(&id)?,
            _ => return None,
        })
    }

    fn pending_lsp_edit_is_current(&self, pending: &PendingLspEdit) -> bool {
        self.buffers.iter().any(|buffer| {
            buffer.id() == pending.buffer_id
                && buffer.revision() == pending.revision
                && buffer
                    .uri()
                    .ok()
                    .flatten()
                    .as_deref()
                    .is_some_and(|uri| uri == pending.uri)
        })
    }

    fn formatting_action(&mut self, response: &ResponseMessage) -> Option<Action> {
        let pending = self.pending_lsp_edit_requests.remove(&response.id)?;
        let save = self.pending_lsp_format_saves.remove(&response.id);
        if !self.pending_lsp_edit_is_current(&pending) {
            let warning = if save.as_ref().is_some_and(|save| save.save_as.is_some()) {
                "format response is stale; Save As cancelled".to_string()
            } else {
                "format response is stale; buffer changed".to_string()
            };
            self.last_error = Some(warning.clone());
            return save.filter(|save| save.save_as.is_some()).map(|save| {
                Action::RestoreLspFormatSave {
                    buffer_id: pending.buffer_id,
                    uri: pending.uri,
                    previous_file: save.previous_file,
                    warning,
                }
            });
        }
        if response.result.is_null() {
            return save.map(|save| Action::CompleteLspFormatSave {
                buffer_id: pending.buffer_id,
                uri: pending.uri,
                save_as: save.save_as,
                previous_file: save.previous_file,
                warning: None,
            });
        }
        let edits = match serde_json::from_value::<Vec<LspTextEdit>>(response.result.clone()) {
            Ok(edits) => edits,
            Err(error) => {
                let warning = if save.as_ref().is_some_and(|save| save.save_as.is_some()) {
                    format!("invalid LSP formatting response; Save As cancelled: {error}")
                } else {
                    format!("invalid LSP formatting response: {error}")
                };
                self.last_error = Some(warning.clone());
                return save.filter(|save| save.save_as.is_some()).map(|save| {
                    Action::RestoreLspFormatSave {
                        buffer_id: pending.buffer_id,
                        uri: pending.uri,
                        previous_file: save.previous_file,
                        warning,
                    }
                });
            }
        };
        if edits.is_empty() {
            return save.map(|save| Action::CompleteLspFormatSave {
                buffer_id: pending.buffer_id,
                uri: pending.uri,
                save_as: save.save_as,
                previous_file: save.previous_file,
                warning: None,
            });
        }
        if let Some(error) = save.as_ref().and_then(|save| {
            save.save_as.as_ref()?;
            let contents = self
                .buffers
                .iter()
                .find(|buffer| buffer.id() == pending.buffer_id)?
                .contents();
            crate::lsp::apply_text_edits(&contents, &edits).err()
        }) {
            let warning = format!("invalid LSP formatting edits; Save As cancelled: {error}");
            self.last_error = Some(warning.clone());
            let save = save?;
            return Some(Action::RestoreLspFormatSave {
                buffer_id: pending.buffer_id,
                uri: pending.uri,
                previous_file: save.previous_file,
                warning,
            });
        }
        let uri = pending.uri.clone();
        let previous_file = save.as_ref().and_then(|save| save.previous_file.clone());
        Some(self.workspace_edit_action(
            vec![LspWorkspaceEditOperation::Document {
                edit: LspDocumentEdit {
                    uri: uri.clone(),
                    version: None,
                    edits,
                },
            }],
            vec![(uri.clone(), pending.revision)],
            None,
            "format document".to_string(),
            None,
            save.as_ref().map(|_| uri),
            save.and_then(|save| save.save_as),
            previous_file,
        ))
    }

    fn code_action_picker(&mut self, response: &ResponseMessage) -> Option<Action> {
        let pending = self.pending_lsp_edit_requests.remove(&response.id)?;
        let revision_snapshot = self.pending_lsp_revision_snapshots.remove(&response.id);
        if !self.pending_lsp_edit_is_current(&pending) {
            self.last_error = Some("code-action response is stale; buffer changed".to_string());
            return None;
        }
        let values = match &response.result {
            Value::Null => return None,
            Value::Array(values) => values,
            _ => {
                self.last_error = Some("invalid LSP code-action response".to_string());
                return None;
            }
        };

        let mut items = Vec::new();
        let mut actions = HashMap::new();
        for (index, value) in values.iter().enumerate() {
            if value.get("disabled").is_some() {
                continue;
            }
            let Some(title) = value.get("title").and_then(Value::as_str) else {
                continue;
            };
            let operations = match value.get("edit") {
                Some(edit) => match workspace_edit_operations(edit) {
                    Ok(operations) => operations,
                    Err(error) => {
                        self.last_error = Some(format!("invalid LSP code-action edit: {error}"));
                        continue;
                    }
                },
                None => Vec::new(),
            };
            let command_value = value
                .get("command")
                .filter(|command| command.is_object())
                .cloned()
                .or_else(|| value.get("command").is_some().then(|| value.clone()));
            let command = match command_value {
                Some(command) => match serde_json::from_value::<LspCommand>(command) {
                    Ok(command) => Some(command),
                    Err(error) => {
                        self.last_error = Some(format!("invalid LSP code-action command: {error}"));
                        continue;
                    }
                },
                None => None,
            };
            if operations.is_empty() && command.is_none() {
                continue;
            }

            let expected_revisions = match self.revisions_for_workspace_response(
                &operations,
                revision_snapshot.as_deref(),
                &pending.uri,
            ) {
                Ok(revisions) => revisions,
                Err(error) => {
                    self.last_error = Some(error);
                    continue;
                }
            };
            if let Err(error) = self.validate_workspace_edit_origin(&operations, &pending.uri) {
                self.last_error = Some(error);
                continue;
            }

            let kind = value
                .get("kind")
                .and_then(Value::as_str)
                .map(|kind| format!(" [{kind}]"))
                .unwrap_or_default();
            let preferred = if value.get("isPreferred").and_then(Value::as_bool) == Some(true) {
                " *"
            } else {
                ""
            };
            let item = format!("{}. {}{}{}", index + 1, title, kind, preferred);
            items.push(item.clone());
            actions.insert(
                item,
                self.workspace_edit_action(
                    operations,
                    expected_revisions,
                    command,
                    title.to_string(),
                    None,
                    None,
                    None,
                    None,
                ),
            );
        }

        if items.is_empty() {
            if self.last_error.is_none() {
                self.last_error = Some("no applicable LSP code actions".to_string());
            }
            return None;
        }
        let picker = Picker::builder()
            .title("Code actions")
            .items(items)
            .select_action(move |item| {
                actions.get(&item).cloned().unwrap_or_else(|| {
                    Action::Print("code action is no longer available".to_string())
                })
            })
            .build(self);
        self.current_dialog = Some(Box::new(picker));
        Some(Action::ShowDialog)
    }

    fn signature_help_action(&mut self, response: &ResponseMessage) -> Option<Action> {
        let signatures = response.result.get("signatures")?.as_array()?;
        let active = response
            .result
            .get("activeSignature")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let signature = signatures.get(active).or_else(|| signatures.first())?;
        let label = signature.get("label")?.as_str()?;
        let documentation = signature.get("documentation").and_then(|documentation| {
            documentation
                .as_str()
                .or_else(|| documentation.get("value").and_then(Value::as_str))
        });
        let text = match documentation {
            Some(documentation) if !documentation.is_empty() => {
                format!("{label}\n\n{documentation}")
            }
            _ => label.to_string(),
        };
        self.current_dialog = Some(Box::new(Info::new(self, text)));
        Some(Action::ShowDialog)
    }

    #[allow(clippy::too_many_arguments)]
    fn workspace_edit_action(
        &self,
        operations: Vec<LspWorkspaceEditOperation>,
        expected_revisions: Vec<(String, u64)>,
        command: Option<LspCommand>,
        label: String,
        response: Option<LspServerRequest>,
        save_after_uri: Option<String>,
        save_as: Option<String>,
        save_previous_file: Option<String>,
    ) -> Action {
        if response.is_none()
            && save_after_uri.is_none()
            && operations
                .iter()
                .all(|operation| operation.document().is_some())
        {
            return Action::ApplyLspWorkspaceEdit {
                documents: operations
                    .into_iter()
                    .filter_map(|operation| match operation {
                        LspWorkspaceEditOperation::Document { edit } => Some(edit),
                        _ => None,
                    })
                    .collect(),
                expected_revisions,
                command: command.map(Box::new),
                label,
            };
        }
        Action::ApplyLspWorkspaceEditOperations {
            operations,
            expected_revisions,
            command: command.map(Box::new),
            label,
            response: response.map(Box::new),
            save_after_uri,
            save_as,
            save_previous_file,
        }
    }

    fn open_buffer_revision_snapshot(&self) -> Vec<(String, u64)> {
        self.buffers
            .iter()
            .filter_map(|buffer| Some((buffer.uri().ok().flatten()?, buffer.revision())))
            .collect()
    }

    fn revisions_for_workspace_operations(
        &self,
        operations: &[LspWorkspaceEditOperation],
    ) -> Vec<(String, u64)> {
        operations
            .iter()
            .filter_map(LspWorkspaceEditOperation::document)
            .filter_map(|document| {
                let target = lsp_normalized_file_path(&document.uri).ok()?;
                self.buffers
                    .iter()
                    .find(|buffer| {
                        buffer
                            .uri()
                            .ok()
                            .flatten()
                            .and_then(|uri| lsp_normalized_file_path(&uri).ok())
                            .is_some_and(|uri| uri == target)
                    })
                    .map(|buffer| (document.uri.clone(), buffer.revision()))
            })
            .collect()
    }

    fn validate_workspace_edit_origin(
        &self,
        operations: &[LspWorkspaceEditOperation],
        origin_uri: &str,
    ) -> Result<(), String> {
        let origin = lsp_normalized_file_path(origin_uri).map_err(|error| error.to_string())?;
        let root = self
            .lsp
            .workspace_root_for_file(&origin)
            .ok_or_else(|| format!("LSP response has no originating workspace for {origin}"))?;
        let root = root
            .absolutize()
            .map_err(|error| error.to_string())?
            .to_path_buf();
        for uri in operations.iter().flat_map(|operation| match operation {
            LspWorkspaceEditOperation::Document { edit } => vec![edit.uri.as_str()],
            LspWorkspaceEditOperation::Create { uri, .. }
            | LspWorkspaceEditOperation::Delete { uri, .. } => vec![uri.as_str()],
            LspWorkspaceEditOperation::Rename {
                old_uri, new_uri, ..
            } => vec![old_uri.as_str(), new_uri.as_str()],
        }) {
            let path =
                PathBuf::from(lsp_normalized_file_path(uri).map_err(|error| error.to_string())?);
            if !path.starts_with(&root) {
                return Err(format!(
                    "LSP response target is outside its originating workspace {}: {}",
                    root.display(),
                    path.display()
                ));
            }
        }
        Ok(())
    }

    fn revisions_for_workspace_response(
        &self,
        operations: &[LspWorkspaceEditOperation],
        snapshot: Option<&[(String, u64)]>,
        requesting_uri: &str,
    ) -> Result<Vec<(String, u64)>, String> {
        let requesting_path = lsp_normalized_file_path(requesting_uri).ok();
        operations
            .iter()
            .filter_map(LspWorkspaceEditOperation::document)
            .filter_map(|document| {
                let path = lsp_normalized_file_path(&document.uri).ok()?;
                let open = self.buffers.iter().any(|buffer| {
                    buffer
                        .uri()
                        .ok()
                        .flatten()
                        .and_then(|uri| lsp_normalized_file_path(&uri).ok())
                        .is_some_and(|candidate| candidate == path)
                });
                open.then_some((document.uri.clone(), path))
            })
            .map(|(uri, path)| {
                let revision = snapshot.and_then(|snapshot| {
                    snapshot.iter().find_map(|(snapshot_uri, revision)| {
                        (lsp_normalized_file_path(snapshot_uri).ok().as_deref()
                            == Some(path.as_str()))
                        .then_some(*revision)
                    })
                });
                if let Some(revision) = revision {
                    return Ok((uri, revision));
                }
                if requesting_path.as_deref() == Some(path.as_str()) {
                    return self
                        .buffers
                        .iter()
                        .find_map(|buffer| {
                            let candidate = buffer
                                .uri()
                                .ok()
                                .flatten()
                                .and_then(|uri| lsp_normalized_file_path(&uri).ok())?;
                            (candidate == path).then_some((uri.clone(), buffer.revision()))
                        })
                        .ok_or_else(|| format!("LSP response target is no longer open: {path}"));
                }
                Err(format!(
                    "LSP response targets open file {path} without a request-time revision snapshot"
                ))
            })
            .collect()
    }

    fn rename_action(&mut self, response: &ResponseMessage) -> Option<Action> {
        let pending = self.pending_lsp_edit_requests.remove(&response.id)?;
        let revision_snapshot = self.pending_lsp_revision_snapshots.remove(&response.id);
        if !self.pending_lsp_edit_is_current(&pending) {
            self.last_error = Some("rename response is stale; buffer changed".to_string());
            return None;
        }
        if response.result.is_null() {
            self.last_error = Some("language server returned no rename edit".to_string());
            return None;
        }
        let operations = match workspace_edit_operations(&response.result) {
            Ok(operations) => operations,
            Err(error) => {
                self.last_error = Some(format!("invalid LSP rename response: {error}"));
                return None;
            }
        };
        let expected_revisions = match self.revisions_for_workspace_response(
            &operations,
            revision_snapshot.as_deref(),
            &pending.uri,
        ) {
            Ok(revisions) => revisions,
            Err(error) => {
                self.last_error = Some(error);
                return None;
            }
        };
        if let Err(error) = self.validate_workspace_edit_origin(&operations, &pending.uri) {
            self.last_error = Some(error);
            return None;
        }
        Some(self.workspace_edit_action(
            operations,
            expected_revisions,
            None,
            "rename symbol".to_string(),
            None,
            None,
            None,
            None,
        ))
    }

    fn server_workspace_edit_action(&mut self, request: &LspServerRequest) -> Action {
        let Some(edit) = request.params.get("edit") else {
            return Action::RespondLspWorkspaceEdit {
                request: Box::new(request.clone()),
                applied: false,
                failure_reason: Some("workspace/applyEdit request is missing edit".to_string()),
            };
        };
        let operations = match workspace_edit_operations(edit) {
            Ok(operations) => operations,
            Err(error) => {
                return Action::RespondLspWorkspaceEdit {
                    request: Box::new(request.clone()),
                    applied: false,
                    failure_reason: Some(error.to_string()),
                };
            }
        };
        let expected_revisions = self.revisions_for_workspace_operations(&operations);
        let label = request
            .params
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or("language server edit")
            .to_string();
        self.workspace_edit_action(
            operations,
            expected_revisions,
            None,
            label,
            Some(request.clone()),
            None,
            None,
            None,
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
                        if let Some(pending) = self.pending_plugin_document_symbols.remove(&msg.id)
                        {
                            let payload = match self.plugin_document_symbols_payload(msg, &pending)
                            {
                                Ok(payload) => payload,
                                Err(err) => plugin_lsp_error(&err.to_string()),
                            };
                            return Some(Action::ResolvePluginRequest(
                                pending.plugin_request_id.get(),
                                payload,
                            ));
                        }
                    }

                    if method == "workspace/symbol" {
                        if let Some(request_id) =
                            self.pending_plugin_workspace_symbols.remove(&msg.id)
                        {
                            let payload = match self.plugin_workspace_symbols_payload(msg) {
                                Ok(payload) => payload,
                                Err(err) => plugin_lsp_error(&err.to_string()),
                            };
                            return Some(Action::ResolvePluginRequest(request_id.get(), payload));
                        }
                    }

                    if method == "textDocument/references" {
                        if let Some(request_id) = self.pending_plugin_references.remove(&msg.id) {
                            let payload = match self.plugin_references_payload(msg) {
                                Ok(payload) => payload,
                                Err(err) => plugin_lsp_error(&err.to_string()),
                            };
                            return Some(Action::ResolvePluginRequest(request_id.get(), payload));
                        }
                    }

                    if method == "textDocument/inlayHint" {
                        if let Some(request_id) = self.pending_plugin_inlay_hints.remove(&msg.id) {
                            let mut payload = match self.plugin_inlay_hints_payload(msg) {
                                Ok(payload) => payload,
                                Err(err) => plugin_lsp_error(&err.to_string()),
                            };
                            payload["request_id"] = json!(request_id.get());
                            return Some(Action::ResolvePluginRequest(request_id.get(), payload));
                        }
                    }

                    if method == "textDocument/formatting" {
                        return self.formatting_action(msg);
                    }

                    if method == "textDocument/codeAction" {
                        return self.code_action_picker(msg);
                    }

                    if method == "textDocument/signatureHelp" {
                        return self.signature_help_action(msg);
                    }

                    if method == "textDocument/rename" {
                        return self.rename_action(msg);
                    }

                    if method == "textDocument/completion" {
                        let pending = self.pending_lsp_edit_requests.remove(&msg.id);
                        if pending
                            .as_ref()
                            .is_some_and(|pending| !self.pending_lsp_edit_is_current(pending))
                        {
                            self.last_error =
                                Some("completion response is stale; buffer changed".to_string());
                            return None;
                        }
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
                                self.completion_snapshot = pending.or_else(|| {
                                    Some(PendingLspEdit {
                                        buffer_id: self.current_buffer().id(),
                                        revision: self.current_buffer().revision(),
                                        uri: self.current_buffer().uri().ok().flatten()?,
                                    })
                                });
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
                            serde_json::Value::Array(ref arr) => {
                                arr.first().and_then(|value| value.as_object())?
                            }
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
                        plugin_json(
                            serde_json::to_value(progress_params)
                                .unwrap_or(serde_json::Value::Null),
                        ),
                    ))
                }
            },
            InboundMessage::UnknownNotification(msg) => {
                log!("got an unhandled notification: {msg:#?}");
                None
            }
            InboundMessage::ServerRequest(request) => (request.method == "workspace/applyEdit")
                .then(|| self.server_workspace_edit_action(request)),
            InboundMessage::Error(error_msg) => {
                log!("got an error: {error_msg:?}");
                let id = error_msg.id?;
                if let Some(request_id) = self.take_pending_plugin_request(method.as_deref()?, id) {
                    return Some(Action::ResolvePluginRequest(
                        request_id.get(),
                        plugin_lsp_error(&error_msg.message),
                    ));
                }
                let pending = self.pending_lsp_edit_requests.remove(&id);
                let save = self.pending_lsp_format_saves.remove(&id);
                self.pending_lsp_revision_snapshots.remove(&id);
                self.last_error = Some(error_msg.message.clone());
                let save_as = save.as_ref().is_some_and(|save| save.save_as.is_some());
                if (error_msg.code == -32601 || save_as)
                    && method.as_deref() == Some("textDocument/formatting")
                {
                    if let (Some(pending), Some(save)) = (pending, save) {
                        if self.pending_lsp_edit_is_current(&pending) {
                            let warning = format!(
                                "format-on-save unavailable; saved unformatted: {}",
                                error_msg.message
                            );
                            log!(
                                "{}",
                                json!({
                                    "event": "lsp_format_on_save_fallback",
                                    "level": "warn",
                                    "service": "red",
                                    "stage": "response",
                                    "error": error_msg.message,
                                })
                            );
                            return Some(Action::CompleteLspFormatSave {
                                buffer_id: pending.buffer_id,
                                uri: pending.uri,
                                save_as: save.save_as,
                                previous_file: save.previous_file,
                                warning: Some(warning),
                            });
                        }
                        if save.save_as.is_some() {
                            let warning = format!(
                                "format response is stale; Save As cancelled: {}",
                                error_msg.message
                            );
                            self.last_error = Some(warning.clone());
                            return Some(Action::RestoreLspFormatSave {
                                buffer_id: pending.buffer_id,
                                uri: pending.uri,
                                previous_file: save.previous_file,
                                warning,
                            });
                        }
                    }
                }
                None
            }
            InboundMessage::RequestError { id, error } => {
                if let Some(request_id) = method
                    .as_deref()
                    .and_then(|method| self.take_pending_plugin_request(method, *id))
                {
                    Some(Action::ResolvePluginRequest(
                        request_id.get(),
                        plugin_lsp_error(&error.to_string()),
                    ))
                } else {
                    let pending = self.pending_lsp_edit_requests.remove(id);
                    let save = self.pending_lsp_format_saves.remove(id);
                    self.pending_lsp_revision_snapshots.remove(id);
                    self.last_error = Some(error.to_string());
                    let save_as = save.as_ref().is_some_and(|save| save.save_as.is_some());
                    if (matches!(
                        error,
                        crate::lsp::LspError::RequestTimeout(_)
                            | crate::lsp::LspError::ProtocolError(_)
                    ) || save_as)
                        && method.as_deref() == Some("textDocument/formatting")
                    {
                        if let (Some(pending), Some(save)) = (pending, save) {
                            if self.pending_lsp_edit_is_current(&pending) {
                                let warning = format!(
                                    "format-on-save unavailable; saved unformatted: {error}"
                                );
                                log!(
                                    "{}",
                                    json!({
                                        "event": "lsp_format_on_save_fallback",
                                        "level": "warn",
                                        "service": "red",
                                        "stage": "response",
                                        "error": error.to_string(),
                                    })
                                );
                                return Some(Action::CompleteLspFormatSave {
                                    buffer_id: pending.buffer_id,
                                    uri: pending.uri,
                                    save_as: save.save_as,
                                    previous_file: save.previous_file,
                                    warning: Some(warning),
                                });
                            }
                            if save.save_as.is_some() {
                                let warning =
                                    format!("format response is stale; Save As cancelled: {error}");
                                self.last_error = Some(warning.clone());
                                return Some(Action::RestoreLspFormatSave {
                                    buffer_id: pending.buffer_id,
                                    uri: pending.uri,
                                    previous_file: save.previous_file,
                                    warning,
                                });
                            }
                        }
                    }
                    None
                }
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
        if self.consume_reactivation_click(ev) {
            return Ok(None);
        }

        if self.substitute_confirmation.is_some() {
            return Ok(self.handle_substitute_confirmation_event(ev));
        }

        if matches!(ev, Event::Paste(_)) {
            self.waiting_key_action = None;
            self.waiting_command = None;
            self.repeater = None;
            self.pending_operator = None;
            self.pending_character_motion = None;
            self.pending_visual_text_object_scope = None;
        }

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

        if self.workspace_manager.is_active() {
            return Ok(self.handle_workspace_event(ev));
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

    fn handle_workspace_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        let Event::Key(event) = ev else {
            return None;
        };
        let action = match event.code {
            KeyCode::Up | KeyCode::Char('k') => "up".to_string(),
            KeyCode::Down | KeyCode::Char('j') => "down".to_string(),
            KeyCode::PageUp => "page_up".to_string(),
            KeyCode::PageDown => "page_down".to_string(),
            KeyCode::Enter => "activate".to_string(),
            KeyCode::Esc => "escape".to_string(),
            KeyCode::Tab => "toggle".to_string(),
            KeyCode::BackTab => "back_toggle".to_string(),
            KeyCode::Char(c) => c.to_string(),
            _ => return None,
        };
        let event = self
            .workspace_manager
            .handle_action(action, self.size.1 as usize)?;
        let id = event.workspace_id.clone();
        serde_json::to_value(event).ok().map(|payload| {
            KeyAction::Multiple(vec![
                Action::NotifyPlugins(format!("workspace:event:{id}"), payload),
                Action::Refresh,
            ])
        })
    }

    fn handle_focus_event(
        &mut self,
        ev: &event::Event,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<bool> {
        match ev {
            Event::FocusLost => {
                self.suppress_reactivation_click = false;
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
                    self.suppress_reactivation_click = self.panel_manager.has_focused_panel()
                        || self
                            .current_dialog
                            .as_ref()
                            .is_some_and(|dialog| !dialog.allows_event_passthrough());
                    self.render(buffer)?;
                } else {
                    self.draw_cursor()?;
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn consume_reactivation_click(&mut self, ev: &event::Event) -> bool {
        if !self.suppress_reactivation_click {
            return false;
        }

        if matches!(
            ev,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(_),
                ..
            })
        ) {
            self.suppress_reactivation_click = false;
            return true;
        }

        false
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
                    KeyCode::PageUp => "page_up",
                    KeyCode::PageDown => "page_down",
                    KeyCode::Char('b') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                        "page_up"
                    }
                    KeyCode::Char('f') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                        "page_down"
                    }
                    KeyCode::Char('g') => "top",
                    KeyCode::Char('G') => "bottom",
                    KeyCode::Left | KeyCode::Char('h') => "collapse",
                    KeyCode::Right | KeyCode::Char('l') => "expand",
                    KeyCode::Enter => "activate",
                    KeyCode::Char(' ') => "toggle",
                    KeyCode::Char('q') => "close",
                    KeyCode::Char('R') => "refresh",
                    _ => return None,
                };

                let panel_height = usize::from(self.size.1.saturating_sub(2));
                self.panel_manager
                    .handle_focused_key(action, panel_height)
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
                    .handle_focused_key("up", height.saturating_sub(2))
                    .and_then(Self::panel_event_key_action)
            }
            MouseEventKind::ScrollDown => {
                let id = self
                    .panel_manager
                    .panel_at_position(x, y, width, height)?
                    .id;
                self.panel_manager.focus_panel(&id);
                self.panel_manager
                    .handle_focused_key("down", height.saturating_sub(2))
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

        if key == "Ctrl-w" && matches!(action, KeyAction::Nested(_)) {
            return Some(action);
        }

        Self::key_action_runs_from_panel(&action).then_some(action)
    }

    fn key_action_runs_from_panel(action: &KeyAction) -> bool {
        match action {
            KeyAction::Single(
                Action::EnterMode(Mode::Command | Mode::Search)
                | Action::PluginCommand(_)
                | Action::NextWindow
                | Action::PreviousWindow,
            ) => true,
            KeyAction::Multiple(actions) => actions.iter().any(|action| {
                matches!(
                    action,
                    Action::EnterMode(Mode::Command | Mode::Search) | Action::PluginCommand(_)
                )
            }),
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
            if now.duration_since(watcher.last_checked) < watcher.interval {
                continue;
            }
            watcher.last_checked = now;

            let next_snapshot = directory_snapshot(&watcher.path, watcher.recursive);
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
            if (!self.is_normal() && !self.is_visual()) || !c.is_numeric() {
                return false;
            }

            if self.repeater.is_none() && *c == '0' {
                return false;
            }

            if let Some(repeater) = self.repeater {
                let digit = c.to_digit(10).unwrap_or(0) as u16;
                self.repeater = Some(repeater.saturating_mul(10).saturating_add(digit));
            } else {
                self.repeater = c.to_digit(10).and_then(|digit| u16::try_from(digit).ok());
            }

            return true;
        }

        false
    }

    fn parse_substitute_command(&self, command: &str) -> anyhow::Result<Option<SubstituteCommand>> {
        let Some(substitute_index) = command.find('s') else {
            return Ok(None);
        };
        let range = &command[..substitute_index];
        let valid_range = range.is_empty()
            || range == "%"
            || range == "'<,'>"
            || range
                .split(',')
                .all(|value| !value.is_empty() && value.chars().all(|c| c.is_ascii_digit()));
        if !valid_range {
            return Ok(None);
        }

        let body = &command[substitute_index + 1..];
        let Some(delimiter) = body.chars().next() else {
            anyhow::bail!("substitute is missing a delimiter");
        };
        if delimiter.is_ascii_alphanumeric() || delimiter.is_whitespace() || delimiter == '\\' {
            return Ok(None);
        }
        let body = &body[delimiter.len_utf8()..];
        let (pattern, remainder) = parse_substitute_segment(body, delimiter)?;
        let (replacement, flags) = parse_substitute_segment(remainder, delimiter)?;
        anyhow::ensure!(!pattern.is_empty(), "substitute pattern cannot be empty");
        anyhow::ensure!(
            flags.chars().all(|flag| matches!(flag, 'g' | 'i' | 'c')),
            "unsupported substitute flags: {flags}"
        );

        let (start_line, end_line) = if range.is_empty() {
            let line = self.buffer_line();
            (line, line)
        } else if range == "%" {
            (0, self.last_navigable_line())
        } else if range == "'<,'>" {
            let buffer_id = self.current_buffer().id();
            let start = self
                .special_marks
                .get(&(buffer_id, '<'))
                .ok_or_else(|| anyhow::anyhow!("last visual range is not set"))?;
            let end = self
                .special_marks
                .get(&(buffer_id, '>'))
                .ok_or_else(|| anyhow::anyhow!("last visual range is not set"))?;
            (start.fallback.line, end.fallback.line)
        } else {
            let lines = range
                .split(',')
                .map(str::parse::<usize>)
                .collect::<Result<Vec<_>, _>>()?;
            let start = lines[0];
            let end = *lines.get(1).unwrap_or(&start);
            anyhow::ensure!(
                start > 0 && end > 0,
                "substitute line numbers are one-based"
            );
            anyhow::ensure!(start <= end, "substitute range start exceeds its end");
            (start - 1, end - 1)
        };

        Ok(Some(SubstituteCommand {
            start_line,
            end_line,
            pattern,
            replacement,
            replace_all: flags.contains('g'),
            case_insensitive: flags.contains('i'),
            confirm: flags.contains('c'),
        }))
    }

    fn handle_command(&mut self, cmd: &str, runtime: &Runtime) -> Vec<Action> {
        log!("handle_command called with: {}", cmd);
        self.command = String::new();
        self.waiting_command = None;
        self.repeater = None;
        self.last_error = None;

        if let Ok(line) = cmd.parse::<usize>() {
            return vec![Action::GoToLine(line)];
        }

        match self.parse_substitute_command(cmd) {
            Ok(Some(command)) => return vec![Action::Substitute(command)],
            Ok(None) => {}
            Err(error) => {
                self.last_error = Some(error.to_string());
                return Vec::new();
            }
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

        if cmd == "registers" {
            return vec![Action::PrintRegisters];
        }
        if cmd == "undotree" {
            let items = self
                .current_buffer()
                .undo_history
                .undo_tree()
                .into_iter()
                .map(|entry| {
                    format!(
                        "{}{} {} {:?}",
                        "  ".repeat(entry.parent.map_or(0, |_| 1)),
                        if entry.current { "*" } else { " " },
                        entry.label,
                        entry.origin
                    )
                })
                .collect();
            return vec![Action::OpenPicker(
                Some("Undo tree".to_string()),
                items,
                /*id*/ None,
            )];
        }
        if let Some(assignment) = cmd.strip_prefix("register ") {
            let Some((register, keys)) = assignment.split_once(' ') else {
                self.last_error = Some("usage: register <name> <key-notation>".to_string());
                return Vec::new();
            };
            let mut register_chars = register.chars();
            let Some(register) = register_chars.next() else {
                self.last_error = Some("macro register must be a letter or digit".to_string());
                return Vec::new();
            };
            if register_chars.next().is_some() || normalize_macro_register(register).is_none() {
                self.last_error = Some("macro register must be a letter or digit".to_string());
                return Vec::new();
            }
            return vec![Action::SetMacroRegister {
                register,
                keys: keys.to_string(),
            }];
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
            if runtime.command_plugin(cmd).is_some() {
                return vec![Action::PluginCommand(cmd.to_string())];
            }
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

    fn plan_substitutions(
        &self,
        command: &SubstituteCommand,
    ) -> anyhow::Result<Vec<PlannedSubstitution>> {
        anyhow::ensure!(
            command.start_line <= self.last_navigable_line(),
            "substitute range starts past the end of the buffer"
        );
        let end_line = command.end_line.min(self.last_navigable_line());
        let regex = RegexBuilder::new(&command.pattern)
            .case_insensitive(command.case_insensitive)
            .build()
            .map_err(|error| anyhow::anyhow!("invalid substitute pattern: {error}"))?;
        let mut substitutions = Vec::new();
        for line_index in command.start_line..=end_line {
            let line = self.current_buffer().get(line_index).unwrap_or_default();
            let line = line.trim_end_matches('\n');
            let line_start = self
                .current_buffer()
                .position_to_char_idx(TextPosition::new(line_index, /*character*/ 0));
            for captures in regex.captures_iter(line) {
                let matched = captures
                    .get(/*index*/ 0)
                    .expect("regex captures always include the full match");
                let mut replacement = String::new();
                captures.expand(&command.replacement, &mut replacement);
                substitutions.push(PlannedSubstitution {
                    start_char: line_start + line[..matched.start()].chars().count(),
                    end_char: line_start + line[..matched.end()].chars().count(),
                    replacement,
                });
                if !command.replace_all {
                    break;
                }
            }
        }
        Ok(substitutions)
    }

    fn show_substitute_confirmation(
        &mut self,
        confirmation: &SubstituteConfirmation,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<()> {
        let Some(substitution) = confirmation.substitutions.get(confirmation.current) else {
            return Ok(());
        };
        let position = self
            .current_buffer()
            .char_idx_to_position(substitution.start_char);
        self.move_to_text_position(position);
        self.refresh_cursor_goal();
        self.last_error = Some(format!(
            "replace with {:?}? (y/n/a/q/l)",
            substitution.replacement
        ));
        self.render(buffer)
    }

    async fn apply_substitutions(
        &mut self,
        mut substitutions: Vec<PlannedSubstitution>,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        if substitutions.is_empty() {
            self.last_error = Some("pattern not found".to_string());
            self.render(buffer)?;
            return Ok(());
        }
        substitutions.sort_by_key(|substitution| substitution.start_char);
        self.begin_transaction("substitute");
        for substitution in substitutions.into_iter().rev() {
            let range = TextRange::new(
                self.current_buffer()
                    .char_idx_to_position(substitution.start_char),
                self.current_buffer()
                    .char_idx_to_position(substitution.end_char),
            );
            self.replace_range(range, &substitution.replacement);
        }
        self.commit_transaction(self.cursor_snapshot());
        self.notify_change(runtime).await?;
        self.render(buffer)
    }

    fn handle_substitute_confirmation_event(&mut self, event: &Event) -> Option<KeyAction> {
        let Event::Key(KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) = event
        else {
            return Some(KeyAction::None);
        };
        let decision = match code {
            KeyCode::Char('y') => SubstituteDecision::Yes,
            KeyCode::Char('n') => SubstituteDecision::No,
            KeyCode::Char('a') => SubstituteDecision::All,
            KeyCode::Char('q') | KeyCode::Esc => SubstituteDecision::Quit,
            KeyCode::Char('l') => SubstituteDecision::Last,
            _ => return Some(KeyAction::None),
        };
        Some(KeyAction::Single(Action::ConfirmSubstitute(decision)))
    }

    fn compile_search_regex(&self, pattern: &str) -> anyhow::Result<Regex> {
        RegexBuilder::new(pattern)
            .case_insensitive(self.search_uses_case_insensitive(pattern))
            .build()
            .map_err(|err| anyhow::anyhow!("invalid search pattern: {err}"))
    }

    fn search_matches(&mut self, pattern: &str) -> anyhow::Result<Arc<[SearchMatch]>> {
        if pattern.is_empty() {
            return Ok(Arc::from([]));
        }

        let case_insensitive = self.search_uses_case_insensitive(pattern);
        let buffer = self.current_buffer();
        let buffer_id = buffer.id();
        let revision = buffer.revision();
        if let Some(cache) = &self.search_match_cache {
            if cache.buffer_id == buffer_id
                && cache.revision == revision
                && cache.pattern == pattern
                && cache.case_insensitive == case_insensitive
            {
                return Ok(Arc::clone(&cache.matches));
            }
        }

        let regex = self.compile_search_regex(pattern)?;
        let matches = Arc::from(buffer.regex_matches(&regex));
        self.search_match_cache = Some(SearchMatchCache {
            buffer_id,
            revision,
            pattern: pattern.to_string(),
            case_insensitive,
            matches: Arc::clone(&matches),
        });
        Ok(matches)
    }

    fn search_match_in_direction(
        &self,
        matches: &[SearchMatch],
        origin: &HistoryEntry,
        direction: SearchDirection,
        wrap: bool,
    ) -> Option<SearchMatch> {
        let origin_x = self.grapheme_to_char_on_line(origin.x, origin.y);
        match direction {
            SearchDirection::Forward => matches
                .iter()
                .copied()
                .find(|match_| {
                    match_.start_y > origin.y
                        || (match_.start_y == origin.y && match_.start_x > origin_x)
                })
                .or_else(|| wrap.then(|| matches.first().copied()).flatten()),
            SearchDirection::Backward => matches
                .iter()
                .rev()
                .copied()
                .find(|match_| {
                    match_.start_y < origin.y
                        || (match_.start_y == origin.y && match_.start_x < origin_x)
                })
                .or_else(|| wrap.then(|| matches.last().copied()).flatten()),
        }
    }

    fn search_match_from_origin(
        &mut self,
        pattern: &str,
        origin: &HistoryEntry,
        direction: SearchDirection,
        wrap: bool,
    ) -> anyhow::Result<Option<SearchMatch>> {
        let viewport_end = self.vtop.saturating_add(self.vheight());
        let has_oversized_line = self
            .current_buffer()
            .line_range_byte_len(origin.y, origin.y.saturating_add(1))
            > MAX_HIGHLIGHT_SLICE_BYTES
            || (self.vtop..viewport_end).any(|line| {
                self.current_buffer()
                    .line_range_byte_len(line, line.saturating_add(1))
                    > MAX_HIGHLIGHT_SLICE_BYTES
            });

        if has_oversized_line {
            let regex = self.compile_search_regex(pattern)?;
            let origin_x = self.grapheme_to_char_on_line(origin.x, origin.y);
            return Ok(self.current_buffer().regex_match_from(
                &regex,
                (origin_x, origin.y),
                direction == SearchDirection::Backward,
                wrap,
            ));
        }

        let matches = self.search_matches(pattern)?;
        Ok(self.search_match_in_direction(&matches, origin, direction, wrap))
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

        let preview = match self.search_match_from_origin(
            &session.draft,
            &session.origin,
            session.direction,
            self.config.search.wrapscan,
        ) {
            Ok(preview) => preview,
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

        let origin = self.current_history_entry();
        let matched = match self.search_match_from_origin(
            &pattern,
            &origin,
            direction,
            self.config.search.wrapscan,
        ) {
            Ok(matched) => matched,
            Err(err) => {
                self.last_error = Some(err.to_string());
                self.render(buffer)?;
                return Ok(false);
            }
        };
        if let Some(match_) = matched {
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

    pub(crate) fn picker_history(&self, key: &str) -> &[String] {
        self.preferences.picker_history(key)
    }

    fn picker_history_key(title: &Option<String>, id: Option<i32>) -> Option<String> {
        id.map(|id| format!("picker:{id}")).or_else(|| {
            title
                .as_deref()
                .filter(|title| !title.trim().is_empty())
                .map(|title| format!("picker:{title}"))
        })
    }

    fn record_picker_history(&mut self, key: &str, query: &str) {
        if let Err(error) = self.preferences.record_picker_query(key, query) {
            log!("failed to save picker history: {error}");
        }
    }

    fn handle_command_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        if let Event::Paste(text) = ev {
            self.command.push_str(&pasted_input_line(text));
            self.reset_command_history_navigation();
            self.reset_command_completion();
            return None;
        }

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
            Event::Paste(text) => {
                if let Some(active_search) = &mut self.active_search {
                    active_search.draft.push_str(&pasted_input_line(text));
                    active_search.preview = None;
                }
                self.update_search_preview();
            }
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
        if let Some(action) = self.handle_character_motion_event(ev) {
            return Some(action);
        }

        if let Some(action) = self.handle_visual_text_object_event(ev) {
            return Some(action);
        }

        let visual = self.visual_key_mappings();
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
            Event::Paste(text) => {
                let text = normalize_terminal_paste(text);
                if text.is_empty() {
                    return Ok(None);
                }

                if self.current_dialog.is_some() {
                    Ok(Some(KeyAction::Multiple(vec![
                        Action::CloseDialog,
                        Action::InsertPastedText(text),
                    ])))
                } else {
                    Ok(Some(KeyAction::Single(Action::InsertPastedText(text))))
                }
            }
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

        if let Some(action) = self.handle_character_motion_event(ev) {
            return Some(action);
        }

        if let Some(action) = self.handle_replace_event(ev) {
            return Some(action);
        }

        if let Some(action) = self.handle_mark_event(ev) {
            return Some(action);
        }

        if let Some(action) = self.handle_macro_event(ev) {
            return Some(action);
        }

        let normal = self.config.keys.normal.clone();
        self.event_to_key_action(&normal, ev)
    }

    fn handle_replace_event(&mut self, event: &Event) -> Option<KeyAction> {
        let Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) = event
        else {
            return None;
        };
        if self.pending_replace {
            self.pending_replace = false;
            self.waiting_command = None;
            if *code == KeyCode::Esc {
                self.repeater = None;
                return Some(KeyAction::None);
            }
            if *modifiers != KeyModifiers::NONE {
                self.last_error = Some("replacement must be a character".to_string());
                self.repeater = None;
                return Some(KeyAction::None);
            }
            let KeyCode::Char(character) = code else {
                self.last_error = Some("replacement must be a character".to_string());
                self.repeater = None;
                return Some(KeyAction::None);
            };
            return Some(KeyAction::Single(Action::ReplaceCharsAtCursor {
                character: *character,
                count: self.repeater.take().unwrap_or(1),
            }));
        }
        if *code == KeyCode::Char('r') && *modifiers == KeyModifiers::NONE {
            self.pending_replace = true;
            self.waiting_command = Some("r".to_string());
            return Some(KeyAction::None);
        }
        None
    }

    fn handle_mark_event(&mut self, event: &Event) -> Option<KeyAction> {
        let Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) = event
        else {
            return None;
        };

        if let Some(pending) = self.pending_mark_action.take() {
            self.waiting_command = None;
            if *code == KeyCode::Esc {
                return Some(KeyAction::None);
            }
            let KeyCode::Char(mark) = code else {
                return self.invalid_mark();
            };
            let valid = match pending {
                PendingMarkAction::Set => mark.is_ascii_alphabetic(),
                PendingMarkAction::JumpExact | PendingMarkAction::JumpLine => {
                    mark.is_ascii_alphabetic() || matches!(mark, '.' | '<' | '>' | '\'')
                }
            };
            if !valid {
                return self.invalid_mark();
            }
            return Some(KeyAction::Single(match pending {
                PendingMarkAction::Set => Action::SetMark(*mark),
                PendingMarkAction::JumpExact => Action::JumpToMark {
                    mark: *mark,
                    linewise: false,
                },
                PendingMarkAction::JumpLine => Action::JumpToMark {
                    mark: *mark,
                    linewise: true,
                },
            }));
        }

        if *modifiers != KeyModifiers::NONE {
            return None;
        }
        let pending = match code {
            KeyCode::Char('m') => PendingMarkAction::Set,
            KeyCode::Char('`') => PendingMarkAction::JumpExact,
            KeyCode::Char('\'') => PendingMarkAction::JumpLine,
            _ => return None,
        };
        self.pending_mark_action = Some(pending);
        self.waiting_command = Some(
            match pending {
                PendingMarkAction::Set => "m",
                PendingMarkAction::JumpExact => "`",
                PendingMarkAction::JumpLine => "'",
            }
            .to_string(),
        );
        Some(KeyAction::None)
    }

    fn invalid_mark(&mut self) -> Option<KeyAction> {
        self.pending_mark_action = None;
        self.waiting_command = None;
        self.last_error = Some("mark must be a letter or a supported special mark".to_string());
        Some(KeyAction::None)
    }

    fn handle_macro_event(&mut self, event: &Event) -> Option<KeyAction> {
        let Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        }) = event
        else {
            return None;
        };

        if self.macro_recording.is_some()
            && *code == KeyCode::Char('q')
            && *modifiers == KeyModifiers::NONE
        {
            self.finish_macro_recording();
            return Some(KeyAction::None);
        }

        if let Some(pending) = self.pending_macro_action.take() {
            self.waiting_command = None;
            if *code == KeyCode::Esc {
                self.repeater = None;
                return Some(KeyAction::None);
            }
            let KeyCode::Char(register) = code else {
                return self.invalid_macro_register();
            };
            return match pending {
                PendingMacroAction::Record => {
                    self.start_macro_recording(*register);
                    Some(KeyAction::None)
                }
                PendingMacroAction::Play => {
                    let register = if *register == '@' {
                        let Some(register) = self.last_played_macro else {
                            self.last_error = Some("no previously played macro".to_string());
                            self.repeater = None;
                            return Some(KeyAction::None);
                        };
                        register
                    } else {
                        let Some(register) = normalize_macro_register(*register) else {
                            return self.invalid_macro_register();
                        };
                        register
                    };
                    let action = KeyAction::Single(Action::PlayMacro(register));
                    Some(if let Some(count) = self.repeater.take() {
                        KeyAction::Repeating(count, Box::new(action))
                    } else {
                        action
                    })
                }
            };
        }

        if *modifiers != KeyModifiers::NONE {
            return None;
        }
        match code {
            KeyCode::Char('q') => {
                self.pending_macro_action = Some(PendingMacroAction::Record);
                self.waiting_command = Some("q".to_string());
                Some(KeyAction::None)
            }
            KeyCode::Char('@') => {
                self.pending_macro_action = Some(PendingMacroAction::Play);
                self.waiting_command = Some("@".to_string());
                Some(KeyAction::None)
            }
            _ => None,
        }
    }

    fn start_macro_recording(&mut self, register: char) {
        let append = register.is_ascii_uppercase();
        let Some(register) = normalize_macro_register(register) else {
            self.invalid_macro_register();
            return;
        };
        let events = if append {
            self.registers
                .get(&register)
                .and_then(|content| Self::macro_events_from_notation(&content.text).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        self.macro_recording = Some(MacroRecording { register, events });
    }

    fn finish_macro_recording(&mut self) {
        let Some(recording) = self.macro_recording.take() else {
            return;
        };
        let notation = recording
            .events
            .iter()
            .filter_map(Self::macro_event_notation)
            .collect::<String>();
        self.set_register(recording.register, Content::charwise(notation));
    }

    fn invalid_macro_register(&mut self) -> Option<KeyAction> {
        self.pending_macro_action = None;
        self.waiting_command = None;
        self.repeater = None;
        self.last_error = Some("macro register must be a letter or digit".to_string());
        Some(KeyAction::None)
    }

    fn handle_character_motion_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        let Event::Key(KeyEvent {
            code, modifiers, ..
        }) = ev
        else {
            return None;
        };

        if *code == KeyCode::Esc {
            if self.pending_character_motion.take().is_some() {
                self.waiting_command = None;
                self.repeater = None;
                return Some(KeyAction::None);
            }
            return None;
        }

        if let Some(pending) = self.pending_character_motion.take() {
            self.waiting_command = None;
            let KeyCode::Char(target) = code else {
                return self.pending_character_motion_invalid();
            };
            if !matches!(*modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT) {
                return self.pending_character_motion_invalid();
            }
            let action = match pending.kind {
                ForwardCharacterMotion::Find => Action::FindCharForward {
                    target: *target,
                    count: pending.count,
                },
                ForwardCharacterMotion::Till => Action::TillCharForward {
                    target: *target,
                    count: pending.count,
                },
            };
            return Some(KeyAction::Single(action));
        }

        if *modifiers != KeyModifiers::NONE {
            return None;
        }
        let KeyCode::Char(c @ ('f' | 't')) = code else {
            return None;
        };
        let kind = if *c == 'f' {
            ForwardCharacterMotion::Find
        } else {
            ForwardCharacterMotion::Till
        };
        self.pending_character_motion = Some(PendingCharacterMotion {
            kind,
            count: self.repeater.take().unwrap_or(1),
        });
        self.waiting_command = Some(c.to_string());
        Some(KeyAction::None)
    }

    fn pending_character_motion_invalid(&mut self) -> Option<KeyAction> {
        self.pending_character_motion = None;
        self.waiting_command = None;
        self.repeater = None;
        self.last_error = Some("invalid character motion".to_string());
        Some(KeyAction::None)
    }

    fn handle_operator_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        let Event::Key(KeyEvent { code, .. }) = ev else {
            return None;
        };

        if *code == KeyCode::Esc {
            if self.pending_operator.take().is_some() {
                self.waiting_command = None;
                self.repeater = None;
                return Some(KeyAction::None);
            }
            return None;
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

        let operator_count = self.repeater.take().unwrap_or(1);
        self.pending_operator = Some(PendingOperator::new(operator, operator_count));
        self.waiting_command = Some(c.to_string());
        Some(KeyAction::None)
    }

    fn handle_pending_operator(&mut self, pending: PendingOperator, c: char) -> Option<KeyAction> {
        match pending.step {
            PendingOperatorStep::Operator => match c {
                '0'..='9' if c != '0' || pending.motion_count.is_some() => {
                    let digit = c.to_digit(10).unwrap_or(0) as u16;
                    let motion_count = pending
                        .motion_count
                        .unwrap_or(0)
                        .saturating_mul(10)
                        .saturating_add(digit);
                    self.waiting_command =
                        Some(format!("{}{}", pending.operator.as_char(), motion_count));
                    self.pending_operator = Some(PendingOperator {
                        motion_count: Some(motion_count),
                        ..pending
                    });
                    Some(KeyAction::None)
                }
                'd' if pending.operator == EditOperator::Delete => Some(KeyAction::Single(
                    Action::DeleteCurrentLines(pending.count()),
                )),
                'c' if pending.operator == EditOperator::Change => Some(KeyAction::Single(
                    Action::ChangeCurrentLines(pending.count()),
                )),
                'y' if pending.operator == EditOperator::Yank => {
                    Some(KeyAction::Single(Action::YankCurrentLines(pending.count())))
                }
                'w' => self.operator_action_for_range(
                    pending.operator,
                    self.word_motion_range(pending.count()),
                    "no word under cursor",
                ),
                '%' => self.operator_action_for_range(
                    pending.operator,
                    self.matchit_motion_range(MatchDirection::Forward),
                    "match not found",
                ),
                'f' => {
                    self.waiting_command = Some(format!("{}f", pending.operator.as_char()));
                    self.pending_operator = Some(PendingOperator {
                        step: PendingOperatorStep::FindForward,
                        ..pending
                    });
                    Some(KeyAction::None)
                }
                't' => {
                    self.waiting_command = Some(format!("{}t", pending.operator.as_char()));
                    self.pending_operator = Some(PendingOperator {
                        step: PendingOperatorStep::TillForward,
                        ..pending
                    });
                    Some(KeyAction::None)
                }
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
            PendingOperatorStep::FindForward => self.operator_action_for_range(
                pending.operator,
                self.find_forward_motion_range(c, pending.count()),
                "character not found",
            ),
            PendingOperatorStep::TillForward => self.operator_action_for_range(
                pending.operator,
                self.till_forward_motion_range(c, pending.count()),
                "character not found",
            ),
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

    fn cursor_lsp_position(&self) -> crate::lsp::Position {
        let position = self.cursor_text_position();
        let character = self
            .current_buffer()
            .get(position.line)
            .map(|line| {
                line.chars()
                    .take(position.character)
                    .map(char::len_utf16)
                    .sum()
            })
            .unwrap_or(position.character);
        crate::lsp::Position {
            line: position.line,
            character,
        }
    }

    fn symbol_under_cursor(&self) -> String {
        let position = self.cursor_text_position();
        let Some(line) = self.current_buffer().get(position.line) else {
            return String::new();
        };
        let characters = line.chars().collect::<Vec<_>>();
        let is_symbol = |character: char| character.is_alphanumeric() || character == '_';
        let mut cursor = position.character.min(characters.len());
        if cursor == characters.len()
            || !characters
                .get(cursor)
                .is_some_and(|value| is_symbol(*value))
        {
            cursor = cursor.saturating_sub(1);
        }
        if !characters
            .get(cursor)
            .is_some_and(|value| is_symbol(*value))
        {
            return String::new();
        }
        let mut start = cursor;
        while start > 0 && is_symbol(characters[start - 1]) {
            start -= 1;
        }
        let mut end = cursor + 1;
        while end < characters.len() && is_symbol(characters[end]) {
            end += 1;
        }
        characters[start..end].iter().collect()
    }

    fn word_motion_range(&self, count: u16) -> Option<TextRange> {
        let start = self.cursor_text_position();
        let buffer = self.current_buffer();
        let characters = buffer.contents().chars().collect::<Vec<_>>();
        let mut end = buffer.position_to_char_idx(start);
        let word_kind = |character: char| {
            if character.is_whitespace() {
                0
            } else if character.is_alphanumeric() || character == '_' {
                1
            } else {
                2
            }
        };
        for _ in 0..count {
            let Some(&character) = characters.get(end) else {
                break;
            };
            let kind = word_kind(character);
            while characters
                .get(end)
                .is_some_and(|&next| word_kind(next) == kind)
            {
                end += 1;
            }
            while characters.get(end).is_some_and(|next| next.is_whitespace()) {
                end += 1;
            }
        }
        let end = buffer.char_idx_to_position(end);
        (start != end).then(|| TextRange::new(start, end))
    }

    fn forward_character_match(&self, target: char, count: u16) -> Option<TextPosition> {
        let start = self.cursor_text_position();
        let line = self.current_buffer().get(start.line)?;
        let line = trim_line_ending(&line);
        let search_start = start.character.saturating_add(1);
        let target_offset = char_suffix(line, search_start)
            .chars()
            .enumerate()
            .filter_map(|(offset, candidate)| (candidate == target).then_some(offset))
            .nth(usize::from(count.saturating_sub(1)))?;
        Some(TextPosition::new(start.line, search_start + target_offset))
    }

    fn find_forward_motion_range(&self, target: char, count: u16) -> Option<TextRange> {
        let start = self.cursor_text_position();
        let target = self.forward_character_match(target, count)?;
        let end = TextPosition::new(target.line, target.character.saturating_add(1));
        Some(TextRange::new(start, end))
    }

    fn till_forward_motion_range(&self, target: char, count: u16) -> Option<TextRange> {
        let start = self.cursor_text_position();
        let end = self.forward_character_match(target, count)?;
        Some(TextRange::new(start, end))
    }

    fn forward_character_target(
        &self,
        target: char,
        count: u16,
        kind: ForwardCharacterMotion,
    ) -> Option<TextPosition> {
        let target = self.forward_character_match(target, count)?;
        match kind {
            ForwardCharacterMotion::Find => Some(target),
            ForwardCharacterMotion::Till => Some(TextPosition::new(
                target.line,
                target.character.saturating_sub(1),
            )),
        }
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

    fn invalidate_terminal_render_state(&mut self, buffer: &mut RenderBuffer) {
        *buffer = RenderBuffer::new(
            self.size.0 as usize,
            self.size.1 as usize,
            &Style::default(),
        );
        self.last_rendered_cursor_position = None;
        self.last_rendered_cursor_surface = None;
    }

    pub fn cleanup(&mut self) -> anyhow::Result<()> {
        write!(self.stdout, "\x1b]112\x1b\\")?;
        self.stdout
            .execute(terminal::LeaveAlternateScreen)?
            .execute(event::DisableBracketedPaste)?
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
        let sensitive_action = matches!(action, Action::SetAgentApiKey(_))
            || matches!(action, Action::NotifyPlugin(_, _, _))
            || matches!(action, Action::NotifyPlugins(method, _) if method.starts_with("composer:"));
        if !sensitive_action {
            self.actions.push(action.clone());
        }
        // The action history is only read back by visual-block replication,
        // which records absolute indices in `pending_select_action`. Trim the
        // front when nothing is recording so the history can't grow without
        // bound over a long session.
        const MAX_ACTION_HISTORY: usize = 1024;
        if self.pending_select_action.is_none() && self.actions.len() > MAX_ACTION_HISTORY {
            let excess = self.actions.len() - MAX_ACTION_HISTORY / 2;
            self.actions.drain(..excess);
        }

        let mut add_to_history = tracking;
        let action_buffer_id = self.current_buffer().id();
        let action_buffer_revision = self.current_buffer().revision();
        self.notified_buffer_revisions
            .entry(action_buffer_id)
            .or_insert(action_buffer_revision);
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
            Action::FindCharForward { target, count } => {
                self.move_to_forward_character(
                    *target,
                    *count,
                    ForwardCharacterMotion::Find,
                    buffer,
                )?;
            }
            Action::TillCharForward { target, count } => {
                self.move_to_forward_character(
                    *target,
                    *count,
                    ForwardCharacterMotion::Till,
                    buffer,
                )?;
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
                self.pending_character_motion = None;
                self.begin_search(*direction);
                self.render(buffer)?;
            }
            Action::EnterMode(new_mode) => {
                add_to_history = false;
                let old_mode = self.mode;
                if matches!(
                    old_mode,
                    Mode::Visual | Mode::VisualLine | Mode::VisualBlock
                ) && !matches!(
                    new_mode,
                    Mode::Visual | Mode::VisualLine | Mode::VisualBlock
                ) {
                    self.capture_last_visual_marks();
                }
                self.selection = None;
                self.pending_visual_text_object_scope = None;
                self.pending_operator = None;
                self.pending_character_motion = None;

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
                let replace_span = perf::PerfSpan::start("edit:replace_char");
                let started_transaction = !self.transaction_active();
                if started_transaction {
                    self.begin_transaction("insert char");
                }
                let line = self.buffer_line();
                let cx = self.cx;
                let char_cx = self.grapheme_to_char_on_line(cx, line);

                self.replace_range(
                    TextRange::insertion(TextPosition::new(line, char_cx)),
                    &c.to_string(),
                );
                drop(replace_span);
                self.notify_change(runtime).await?;

                // Move cursor by one character position (not display width)
                self.cx += grapheme_len(&c.to_string());
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
                    let draw_span = perf::PerfSpan::start("edit:draw_line");
                    self.draw_line(buffer);
                    drop(draw_span);
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
                if self.begin_change_range(*range, "change text object", false) {
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
                if self.begin_change_range(range, "change line", true) {
                    self.notify_change(runtime).await?;
                }
                self.render(buffer)?;
                self.execute(&Action::EnterMode(Mode::Insert), buffer, runtime)
                    .await?;
            }
            Action::ChangeCurrentLines(count) => {
                let range = self.current_line_range(*count, false);
                if self.begin_change_range(range, "change lines", true) {
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
            Action::ReplaceCharsAtCursor { character, count } => {
                let line = self.buffer_line();
                let line_length = self.length_for_line(line);
                let available = line_length.saturating_sub(self.cx);
                let count = usize::from(*count);
                if count == 0 || count > available {
                    self.last_error = Some("not enough characters to replace".to_string());
                } else {
                    let start = self.grapheme_to_char_on_line(self.cx, line);
                    let end = self.grapheme_to_char_on_line(self.cx + count, line);
                    self.begin_transaction("replace chars");
                    self.replace_range(
                        TextRange::new(
                            TextPosition::new(line, start),
                            TextPosition::new(line, end),
                        ),
                        &character.to_string().repeat(count),
                    );
                    self.commit_transaction(self.cursor_snapshot());
                    self.notify_change(runtime).await?;
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
                self.set_default_register(Content::linewise(deleted_text));
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
            Action::DeleteCurrentLines(count) => {
                let range = self.current_line_range(*count, true);
                if self.delete_linewise_range(range, "delete lines") {
                    self.notify_change(runtime).await?;
                }
                self.cx = 0;
                self.render(buffer)?;
            }
            Action::Undo => {
                self.undo_transaction(buffer, runtime).await?;
            }
            Action::Redo => {
                self.redo_transaction(buffer, runtime).await?;
            }
            Action::SelectPreviousUndoBranch => {
                add_to_history = false;
                self.last_error = self
                    .current_buffer_mut()
                    .undo_history
                    .select_previous_branch()
                    .map(|(selected, total)| {
                        format!("undo branch {selected}/{total} selected; redo to traverse")
                    })
                    .or_else(|| Some("no alternate undo branch".to_string()));
                self.draw_commandline(buffer);
            }
            Action::SelectNextUndoBranch => {
                add_to_history = false;
                self.last_error = self
                    .current_buffer_mut()
                    .undo_history
                    .select_next_branch()
                    .map(|(selected, total)| {
                        format!("undo branch {selected}/{total} selected; redo to traverse")
                    })
                    .or_else(|| Some("no alternate undo branch".to_string()));
                self.draw_commandline(buffer);
            }
            Action::RevertTransaction(transaction_id) => {
                let mut edits = match self
                    .current_buffer()
                    .undo_history
                    .prepare_revert(transaction_id, self.current_buffer())
                {
                    Ok(edits) => edits,
                    Err(error) => {
                        self.last_error = Some(format!("revert conflict: {error}"));
                        self.draw_commandline(buffer);
                        return Ok(false);
                    }
                };
                edits.sort_by_key(|edit| edit.start_char);
                self.begin_transaction(format!("revert {transaction_id}"));
                for RevertEdit {
                    start_char,
                    end_char,
                    replacement,
                } in edits.into_iter().rev()
                {
                    let range = TextRange::new(
                        self.current_buffer().char_idx_to_position(start_char),
                        self.current_buffer().char_idx_to_position(end_char),
                    );
                    self.replace_range(range, &replacement);
                }
                self.commit_transaction(self.cursor_snapshot());
                self.notify_change(runtime).await?;
                self.render(buffer)?;
            }
            Action::RepeatLastChange => {
                add_to_history = false;
                self.replay_last_semantic_change(buffer, runtime).await?;
            }
            Action::PlayMacro(register) => {
                add_to_history = false;
                self.play_macro(*register, buffer, runtime).await?;
            }
            Action::SetMark(mark) => {
                add_to_history = false;
                self.set_named_mark(*mark);
            }
            Action::JumpToMark { mark, linewise } => {
                if *mark == '\'' {
                    add_to_history = false;
                    if let Some(entry) = self.jump_back_entry() {
                        let action = self.action_for_history_entry(&entry);
                        self.execute_with_tracking(
                            &action, buffer, runtime, /*tracking*/ false,
                        )
                        .await?;
                    } else {
                        self.last_error = Some("previous jump is not set".to_string());
                    }
                } else {
                    let current_buffer_id = self.current_buffer().id();
                    let anchor = if mark.is_ascii_lowercase() {
                        self.local_marks
                            .get(&current_buffer_id)
                            .and_then(|marks| marks.get(mark))
                            .cloned()
                    } else if mark.is_ascii_uppercase() {
                        self.global_marks.get(mark).cloned()
                    } else {
                        self.special_marks.get(&(current_buffer_id, *mark)).cloned()
                    };
                    let Some(mut anchor) = anchor else {
                        self.last_error = Some(format!("mark {mark} is not set"));
                        return Ok(false);
                    };

                    if let Some(index) = self
                        .buffers
                        .iter()
                        .position(|candidate| candidate.id() == anchor.buffer_id)
                    {
                        if index != self.current_buffer_index {
                            self.set_current_buffer(buffer, index).await?;
                        }
                        anchor.fallback = self
                            .current_buffer()
                            .char_idx_to_position(anchor.char_index);
                    } else if mark.is_ascii_uppercase() {
                        let Some(file) = anchor.file.clone() else {
                            self.last_error = Some(format!("marked buffer for {mark} disappeared"));
                            return Ok(false);
                        };
                        if !Path::new(&file).exists() {
                            self.last_error = Some(format!("marked file for {mark} disappeared"));
                            return Ok(false);
                        }
                        self.execute_with_tracking(
                            &Action::OpenFile(file),
                            buffer,
                            runtime,
                            /*tracking*/ false,
                        )
                        .await?;
                        anchor.buffer_id = self.current_buffer().id();
                        anchor.char_index =
                            self.current_buffer().position_to_char_idx(anchor.fallback);
                        self.global_marks.insert(*mark, anchor.clone());
                    } else {
                        self.last_error = Some(format!("marked buffer for {mark} disappeared"));
                        return Ok(false);
                    }

                    let mut position = anchor.fallback;
                    if *linewise {
                        position.character = 0;
                    }
                    self.move_to_text_position(position);
                    if *linewise {
                        self.move_to_first_non_blank_on_current_line();
                    }
                    self.finish_cursor_motion(buffer, /*preserve_cursor_goal*/ false)?;
                }
            }
            Action::Substitute(command) => {
                let substitutions = match self.plan_substitutions(command) {
                    Ok(substitutions) => substitutions,
                    Err(error) => {
                        self.last_error = Some(error.to_string());
                        self.render(buffer)?;
                        return Ok(false);
                    }
                };
                if command.confirm && !substitutions.is_empty() {
                    let confirmation = SubstituteConfirmation {
                        substitutions,
                        current: 0,
                        accepted: Vec::new(),
                    };
                    self.show_substitute_confirmation(&confirmation, buffer)?;
                    self.substitute_confirmation = Some(confirmation);
                } else {
                    self.apply_substitutions(substitutions, buffer, runtime)
                        .await?;
                }
            }
            Action::ConfirmSubstitute(decision) => {
                add_to_history = false;
                let Some(mut confirmation) = self.substitute_confirmation.take() else {
                    self.last_error = Some("no substitute confirmation is active".to_string());
                    return Ok(false);
                };
                match decision {
                    SubstituteDecision::Yes | SubstituteDecision::Last => {
                        if let Some(substitution) = confirmation
                            .substitutions
                            .get(confirmation.current)
                            .cloned()
                        {
                            confirmation.accepted.push(substitution);
                        }
                        confirmation.current += 1;
                    }
                    SubstituteDecision::No => confirmation.current += 1,
                    SubstituteDecision::All => {
                        confirmation.accepted.extend(
                            confirmation.substitutions[confirmation.current..]
                                .iter()
                                .cloned(),
                        );
                        confirmation.current = confirmation.substitutions.len();
                    }
                    SubstituteDecision::Quit => {}
                }
                let finished = matches!(
                    decision,
                    SubstituteDecision::All | SubstituteDecision::Quit | SubstituteDecision::Last
                ) || confirmation.current >= confirmation.substitutions.len();
                if finished {
                    if confirmation.accepted.is_empty() {
                        self.last_error = Some("0 substitutions".to_string());
                        self.render(buffer)?;
                    } else {
                        self.apply_substitutions(confirmation.accepted, buffer, runtime)
                            .await?;
                    }
                } else {
                    self.show_substitute_confirmation(&confirmation, buffer)?;
                    self.substitute_confirmation = Some(confirmation);
                }
            }
            Action::SetMacroRegister { register, keys } => {
                add_to_history = false;
                let Some(register) = normalize_macro_register(*register) else {
                    self.last_error = Some("macro register must be a letter or digit".to_string());
                    return Ok(false);
                };
                if let Err(error) = Self::macro_events_from_notation(keys) {
                    self.last_error = Some(error.to_string());
                    return Ok(false);
                }
                self.set_register(register, Content::charwise(keys.clone()));
                self.draw_commandline(buffer);
            }
            Action::PrintRegisters => {
                add_to_history = false;
                let mut registers = self
                    .registers
                    .iter()
                    .map(|(register, content)| (*register, content.text.clone()))
                    .collect::<Vec<_>>();
                registers.sort_by_key(|(register, _)| *register);
                self.last_error = Some(if registers.is_empty() {
                    "no registers".to_string()
                } else {
                    registers
                        .into_iter()
                        .map(|(register, text)| format!("{register}: {text}"))
                        .collect::<Vec<_>>()
                        .join("  ")
                });
                self.draw_commandline(buffer);
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
                let viewport_height = self.vheight().max(1);
                let viewport_center = viewport_height / 2;
                let line = self.buffer_line();
                let max_vtop = self
                    .last_navigable_line()
                    .saturating_sub(viewport_height.saturating_sub(1));
                self.vtop = line.saturating_sub(viewport_center).min(max_vtop);
                self.cy = line.saturating_sub(self.vtop);
                self.render(buffer)?;
            }
            Action::InsertLineBelowCursor => {
                let leading_spaces = self.current_line_indentation();
                let line = self.buffer_line();

                log!(
                    "InsertLineBelowCursor - line: {}, leading_spaces: {}, current cx: {}, cy: {}",
                    line,
                    leading_spaces,
                    self.cx,
                    self.cy
                );

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
                let viewport_height = self.vheight().max(1);
                if line_count > viewport_height {
                    self.cy = viewport_height - 1;
                    self.vtop = line_count - viewport_height;
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
                    for (plugin_name, meta) in metadata {
                        content.push_str(&format!("## {}\n", meta.name));
                        content.push_str(&format!("Version: {}\n", meta.version));
                        if let Some(status) = self.plugin_registry.statuses().get(plugin_name) {
                            content.push_str(&format!("Status: {status:?}\n"));
                            if let plugin::PluginStatus::Quarantined {
                                path, diagnostic, ..
                            } = status
                            {
                                content.push_str(&format!("Source: {path}\n"));
                                content.push_str(&format!("Diagnostic: {diagnostic}\n"));
                                content.push_str(
                                    "Actions: fix and save to retry, remove from config to disable, or open docs/PLUGIN_API.md\n",
                                );
                            }
                        }

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

                for action in self.handle_command(cmd, runtime) {
                    self.last_error = None;
                    if self.execute(&action, buffer, runtime).await? {
                        return Ok(true);
                    }
                }
                self.render(buffer)?;
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
                    let position = self.cursor_lsp_position();
                    self.ensure_current_buffer_lsp_opened().await?;
                    self.lsp
                        .goto_definition(&file, position.character, position.line)
                        .await?;
                }
            }
            Action::Hover => {
                if let Some(file) = self.current_buffer().file.clone() {
                    let position = self.cursor_lsp_position();
                    self.ensure_current_buffer_lsp_opened().await?;
                    self.lsp
                        .hover(&file, position.character, position.line)
                        .await?;
                }
            }
            Action::FormatDocument => {
                if let Some(file) = self.current_buffer().file.clone() {
                    let Some(uri) = self.current_buffer().uri()? else {
                        return Ok(false);
                    };
                    let pending = PendingLspEdit {
                        buffer_id: self.current_buffer().id(),
                        revision: self.current_buffer().revision(),
                        uri,
                    };
                    self.ensure_current_buffer_lsp_opened().await?;
                    let indentation = self.indentation();
                    let request_id = self
                        .lsp
                        .format_document_with_options(&file, indentation.shift_width, true)
                        .await?;
                    if request_id > 0 {
                        self.pending_lsp_edit_requests.insert(request_id, pending);
                    } else {
                        self.last_error =
                            Some("no language server is available for this file".to_string());
                    }
                }
            }
            Action::CodeAction => {
                if let Some(file) = self.current_buffer().file.clone() {
                    let Some(uri) = self.current_buffer().uri()? else {
                        return Ok(false);
                    };
                    let position = self.cursor_lsp_position();
                    let range = Range {
                        start: position,
                        end: position,
                    };
                    let diagnostics = self
                        .diagnostics
                        .get(&uri)
                        .into_iter()
                        .flatten()
                        .filter(|diagnostic| {
                            let start = (
                                diagnostic.range.start.line,
                                diagnostic.range.start.character,
                            );
                            let end = (diagnostic.range.end.line, diagnostic.range.end.character);
                            let cursor = (position.line, position.character);
                            start <= cursor && cursor <= end
                        })
                        .cloned()
                        .collect();
                    let pending = PendingLspEdit {
                        buffer_id: self.current_buffer().id(),
                        revision: self.current_buffer().revision(),
                        uri,
                    };
                    let revisions = self.open_buffer_revision_snapshot();
                    self.ensure_current_buffer_lsp_opened().await?;
                    let request_id = self.lsp.code_action(&file, range, diagnostics).await?;
                    if request_id > 0 {
                        self.pending_lsp_edit_requests.insert(request_id, pending);
                        self.pending_lsp_revision_snapshots
                            .insert(request_id, revisions);
                    } else {
                        self.last_error =
                            Some("no language server is available for this file".to_string());
                    }
                }
            }
            Action::SignatureHelp => {
                if let Some(file) = self.current_buffer().file.clone() {
                    let position = self.cursor_lsp_position();
                    self.ensure_current_buffer_lsp_opened().await?;
                    self.lsp
                        .signature_help(&file, position.character, position.line)
                        .await?;
                }
            }
            Action::StartRename => {
                if self.current_buffer().file.is_some() {
                    let initial = self.symbol_under_cursor();
                    self.current_dialog = Some(Box::new(InputPrompt::new(
                        self,
                        "Rename symbol",
                        initial,
                        Action::RenameSymbol,
                    )));
                    self.render(buffer)?;
                }
            }
            Action::RenameSymbol(new_name) => {
                if let Some(file) = self.current_buffer().file.clone() {
                    let Some(uri) = self.current_buffer().uri()? else {
                        return Ok(false);
                    };
                    let position = self.cursor_lsp_position();
                    let pending = PendingLspEdit {
                        buffer_id: self.current_buffer().id(),
                        revision: self.current_buffer().revision(),
                        uri,
                    };
                    let revisions = self.open_buffer_revision_snapshot();
                    self.ensure_current_buffer_lsp_opened().await?;
                    let request_id = self
                        .lsp
                        .rename(&file, position.character, position.line, new_name)
                        .await?;
                    if request_id > 0 {
                        self.pending_lsp_edit_requests.insert(request_id, pending);
                        self.pending_lsp_revision_snapshots
                            .insert(request_id, revisions);
                    } else {
                        self.last_error =
                            Some("no language server is available for this file".to_string());
                    }
                }
            }
            Action::ApplyLspWorkspaceEdit {
                documents,
                expected_revisions,
                command,
                label,
            } => {
                let operations = documents
                    .iter()
                    .cloned()
                    .map(|edit| LspWorkspaceEditOperation::Document { edit })
                    .collect::<Vec<_>>();
                self.apply_lsp_workspace_edit(
                    &operations,
                    expected_revisions,
                    command.as_deref(),
                    label,
                    None,
                    None,
                    None,
                    None,
                    buffer,
                    runtime,
                )
                .await?;
            }
            Action::ApplyLspWorkspaceEditOperations {
                operations,
                expected_revisions,
                command,
                label,
                response,
                save_after_uri,
                save_as,
                save_previous_file,
            } => {
                self.apply_lsp_workspace_edit(
                    operations,
                    expected_revisions,
                    command.as_deref(),
                    label,
                    response.as_deref(),
                    save_after_uri.as_deref(),
                    save_as.as_deref(),
                    save_previous_file.clone(),
                    buffer,
                    runtime,
                )
                .await?;
            }
            Action::CompleteLspFormatSave {
                buffer_id,
                uri,
                save_as,
                previous_file,
                warning,
            } => {
                self.complete_lsp_format_save(
                    *buffer_id,
                    uri,
                    save_as.as_deref(),
                    previous_file.clone(),
                    warning.as_deref(),
                    runtime,
                )
                .await?;
            }
            Action::RestoreLspFormatSave {
                buffer_id,
                uri,
                previous_file,
                warning,
            } => {
                self.restore_lsp_format_save_identity(*buffer_id, uri, previous_file.clone())
                    .await;
                self.last_error = Some(warning.clone());
            }
            Action::RespondLspWorkspaceEdit {
                request,
                applied,
                failure_reason,
            } => {
                self.lsp
                    .respond_workspace_edit(request, *applied, failure_reason.as_deref())
                    .await?;
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
            Action::OpenLocation(location, target) => {
                let path = match expanded_path_string(&location.path).and_then(|path| {
                    Ok(Path::new(&path)
                        .absolutize()?
                        .to_string_lossy()
                        .into_owned())
                }) {
                    Ok(path) => path,
                    Err(error) => {
                        self.last_error = Some(error.to_string());
                        return Ok(false);
                    }
                };
                let existing_index = self.buffers.iter().position(|item| {
                    Path::new(item.name())
                        .absolutize()
                        .is_ok_and(|candidate| candidate == Path::new(&path))
                });
                let (buffer_index, added_buffer) = if let Some(index) = existing_index {
                    (index, false)
                } else {
                    let new_buffer = match Buffer::load_or_create(Some(path.clone())).await {
                        Ok(new_buffer) => new_buffer,
                        Err(error) => {
                            self.last_error = Some(error.to_string());
                            return Ok(false);
                        }
                    };
                    self.buffers.push(new_buffer);
                    (self.buffers.len() - 1, true)
                };

                let opened =
                    match target {
                        plugin::OpenLocationTarget::Current => {
                            self.set_current_buffer(buffer, buffer_index).await?;
                            true
                        }
                        plugin::OpenLocationTarget::Horizontal => self
                            .update_window_layout(|windows| windows.split_horizontal(buffer_index)),
                        plugin::OpenLocationTarget::Vertical => self
                            .update_window_layout(|windows| windows.split_vertical(buffer_index)),
                    };
                if !opened {
                    if added_buffer {
                        self.buffers.pop();
                    }
                    self.last_error =
                        Some("Unable to open location in requested target".to_string());
                    return Ok(false);
                }

                if added_buffer {
                    self.plugin_registry
                        .notify(
                            runtime,
                            "file:opened",
                            json!({ "file": path, "buffer_index": buffer_index }),
                        )
                        .await?;
                }

                let target_line = location
                    .line
                    .min(self.current_buffer().len().saturating_sub(1));
                let target_column = self
                    .current_buffer()
                    .get(target_line)
                    .map(|line| {
                        let line = line.trim_end_matches('\n');
                        match location.column_encoding {
                            plugin::LocationColumnEncoding::Utf8Byte => {
                                crate::unicode_utils::byte_to_grapheme(line, location.column)
                            }
                            plugin::LocationColumnEncoding::Utf16 => {
                                utf16_to_grapheme(line, location.column)
                            }
                        }
                    })
                    .unwrap_or_default();
                self.execute_with_tracking(
                    &Action::MoveTo(target_column, target_line + 1),
                    buffer,
                    runtime,
                    /*tracking*/ false,
                )
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
                let last_line = self.last_navigable_line();
                let max_vtop = if self.wrap {
                    last_line
                } else {
                    last_line.saturating_sub(viewport_height.saturating_sub(1))
                };
                let old_vtop = self.vtop;
                let new_vtop = self.vtop.saturating_add(scroll_lines).min(max_vtop);
                let scrolled_lines = new_vtop.saturating_sub(old_vtop);
                if scrolled_lines > 0 {
                    self.vtop = new_vtop;
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
                let viewport_height = self.vheight().max(1);
                if line > self.vtop + viewport_height {
                    self.vtop = line.saturating_sub(viewport_height - 1);
                    self.cy = viewport_height - 1;
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
                let format_warning = if self.config.lsp.format_on_save {
                    let request = self.request_format_on_save(None).await;
                    let request = match request {
                        Ok(request) => request,
                        Err(error) => {
                            self.resume_insert_transaction_after_save(resume_insert_transaction);
                            return Err(error);
                        }
                    };
                    match request {
                        FormatOnSaveRequest::Pending => {
                            self.resume_insert_transaction_after_save(resume_insert_transaction);
                            return Ok(false);
                        }
                        FormatOnSaveRequest::Save { warning } => warning,
                        FormatOnSaveRequest::Cancelled => {
                            self.resume_insert_transaction_after_save(resume_insert_transaction);
                            return Ok(false);
                        }
                    }
                } else {
                    None
                };
                let save_result = self.current_buffer_mut().save();
                self.resume_insert_transaction_after_save(resume_insert_transaction);

                match save_result {
                    Ok(msg) => {
                        // TODO: use last_message instead of last_error
                        self.last_error = Some(format_warning.unwrap_or(msg));

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
                let previous_uri = self.current_buffer().uri()?;
                let previous_file = self.current_buffer().file.clone();
                let resume_insert_transaction = self.commit_active_transaction_before_save();
                let format_warning = if self.config.lsp.format_on_save {
                    let request = self
                        .request_format_on_save(Some(new_file_name.clone()))
                        .await;
                    let request = match request {
                        Ok(request) => request,
                        Err(error) => {
                            self.resume_insert_transaction_after_save(resume_insert_transaction);
                            return Err(error);
                        }
                    };
                    match request {
                        FormatOnSaveRequest::Pending => {
                            self.resume_insert_transaction_after_save(resume_insert_transaction);
                            return Ok(false);
                        }
                        FormatOnSaveRequest::Save { warning } => warning,
                        FormatOnSaveRequest::Cancelled => {
                            self.resume_insert_transaction_after_save(resume_insert_transaction);
                            return Ok(false);
                        }
                    }
                } else {
                    None
                };
                let save_result = self.current_buffer_mut().save_as(new_file_name);
                self.resume_insert_transaction_after_save(resume_insert_transaction);

                match save_result {
                    Ok(msg) => {
                        // TODO: use last_message instead of last_error
                        self.last_error = Some(format_warning.unwrap_or(msg));
                        if let Err(error) = self
                            .sync_lsp_document_identity(
                                previous_uri.as_deref(),
                                self.current_buffer_index,
                            )
                            .await
                        {
                            if !self.config.lsp.format_on_save
                                || !matches!(
                                    error.downcast_ref::<crate::lsp::LspError>(),
                                    Some(
                                        crate::lsp::LspError::ProtocolError(_)
                                            | crate::lsp::LspError::RequestTimeout(_)
                                    )
                                )
                            {
                                return Err(error);
                            }
                            log!(
                                "{}",
                                json!({
                                    "event": "lsp_format_on_save_fallback",
                                    "level": "warn",
                                    "service": "red",
                                    "stage": "sync_document_identity",
                                    "error": error.to_string(),
                                })
                            );
                            self.last_error = Some(format!(
                                "format-on-save unavailable; saved unformatted: {error}"
                            ));
                        }
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
                        if self.config.lsp.format_on_save {
                            if let Some(target_uri) = self.current_buffer().uri()? {
                                self.restore_lsp_format_save_identity(
                                    self.current_buffer().id(),
                                    &target_uri,
                                    previous_file,
                                )
                                .await;
                            }
                        }
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
                self.render(buffer)?;
            }
            Action::CloseDialog => {
                self.current_dialog = None;
                self.agent_secret_input_active = false;
                self.render(buffer)?;
            }
            Action::SetAgentApiKey(api_key) => {
                add_to_history = false;
                self.agent_secret_input_active = false;
                let api_key = api_key.trim();
                if api_key.is_empty() || api_key.len() > 16 * 1024 {
                    self.last_error = Some("invalid OpenAI API key".to_string());
                } else {
                    self.agent_session_api_key = Some(api_key.to_string());
                    self.config.agent.adapter = Some("openai".to_string());
                    self.config.agent.command = None;
                    self.config.agent.args.clear();
                    self.abort_agent_bridge();
                    self.plugin_registry
                        .notify(runtime, "agent:credential_ready", json!({}))
                        .await?;
                }
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
                let history_key = Self::picker_history_key(title, *id);
                let mut picker = Picker::new(title.clone(), self, items, *id);
                if let Some(history_key) = history_key {
                    let history = self.picker_history(&history_key).to_vec();
                    picker.set_history(history_key, history);
                }
                self.current_dialog = Some(Box::new(picker));
                self.render(buffer)?;
            }
            Action::OpenLivePicker(title, items, id, options) => {
                let history_key = Self::picker_history_key(title, *id);
                let mut picker =
                    Picker::new_live_with_options(title.clone(), self, items, *id, options.clone());
                if let Some(history_key) = history_key {
                    let history = self.picker_history(&history_key).to_vec();
                    picker.set_history(history_key, history);
                }
                self.current_dialog = Some(Box::new(picker));
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
            Action::RecordPickerHistory { key, query } => {
                add_to_history = false;
                self.record_picker_history(key, query);
            }
            Action::PreviewTheme(theme_name) => {
                match self.apply_theme(theme_name, false) {
                    Ok(()) => {
                        self.refresh_plugin_snapshots(runtime, false, false, true)?;
                        self.plugin_registry
                            .notify(
                                runtime,
                                "theme:changed",
                                json!({ "name": theme_name, "persisted": false }),
                            )
                            .await?;
                    }
                    Err(err) => self.last_error = Some(err.to_string()),
                }
                self.render(buffer)?;
            }
            Action::SetTheme(theme_name) => {
                match self.apply_theme(theme_name, true) {
                    Ok(()) => {
                        self.refresh_plugin_snapshots(runtime, false, false, true)?;
                        self.plugin_registry
                            .notify(
                                runtime,
                                "theme:changed",
                                json!({ "name": theme_name, "persisted": true }),
                            )
                            .await?;
                    }
                    Err(err) => self.last_error = Some(err.to_string()),
                }
                self.render(buffer)?;
            }
            Action::Suspend => {
                #[cfg(unix)]
                {
                    self.cleanup()?;
                    let pid = Pid::from_raw(/*raw*/ 0);
                    signal::kill(pid, Signal::SIGSTOP)?;
                    terminal::enable_raw_mode()?;
                    self.stdout
                        .execute(event::EnableMouseCapture)?
                        .execute(event::EnableFocusChange)?
                        .execute(event::EnableBracketedPaste)?
                        .execute(terminal::EnterAlternateScreen)?
                        .execute(terminal::Clear(terminal::ClearType::All))?;
                    self.invalidate_terminal_render_state(buffer);
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
            Action::YankCurrentLines(count) => {
                let range = self.current_line_range(*count, true);
                let text = self.current_buffer().text_in_range(range);
                if !text.is_empty() {
                    self.set_default_register(Content::linewise(text));
                    self.draw_commandline(buffer);
                }
            }
            Action::Delete => {
                if self.selection.is_some() {
                    self.begin_transaction("delete selection");
                    if let Some((x0, y0)) = self.delete_selection() {
                        let y0 = y0.min(self.last_navigable_line());
                        if !self.is_within_viewport(y0) {
                            self.vtop = y0;
                        }
                        self.cx = x0;
                        self.cy = y0.saturating_sub(self.vtop);
                        self.fix_cursor_pos();
                    }
                    self.commit_transaction(self.cursor_snapshot());
                    self.selection = None;
                    self.notify_change(runtime).await?;
                    self.render(buffer)?;
                }
            }
            Action::ChangeSelection => {
                self.change_selection(buffer, runtime).await?;
            }
            Action::Paste | Action::PasteBefore => {
                log!("pasting selection");
                if self.is_visual() && self.selection.is_some() {
                    if self.paste_over_selection(*action == Action::PasteBefore) {
                        self.notify_change(runtime).await?;
                    }
                } else if self.paste_default(*action == Action::PasteBefore) {
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
            Action::InsertPastedText(text) => {
                let line = self.buffer_line();
                let char_cx = self.grapheme_to_char_on_line(self.cx, line);
                let start = TextPosition::new(line, char_cx);
                let end = self.current_buffer().range_for_text(start, text).end;
                let started_transaction = !self.transaction_active();
                if started_transaction {
                    self.begin_transaction("paste");
                }
                self.replace_range(TextRange::insertion(start), text);
                self.move_to_insert_text_position(end);
                if started_transaction {
                    self.commit_transaction(self.cursor_snapshot());
                }
                self.notify_change(runtime).await?;
                self.render(buffer)?;
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
            Action::NotifyPlugin(plugin, method, params) => {
                self.plugin_registry
                    .notify_plugin(runtime, plugin, method, params.clone())
                    .await?;
            }
            Action::ResolvePluginRequest(request_id, payload) => {
                self.plugin_registry
                    .resolve_request(runtime, RequestId::from_raw(*request_id), payload.clone())
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
                self.cycle_focus(true, buffer).await?;
            }
            Action::PreviousWindow => {
                self.cycle_focus(false, buffer).await?;
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

        let bounds_span = perf::PerfSpan::start("edit:post_action_bounds");
        let bounds_changed = self.check_bounds();

        if Self::should_refresh_cursor_goal_after(action) {
            self.refresh_cursor_goal();
        }
        drop(bounds_span);

        if bounds_changed {
            self.render(buffer)?;
        }

        if self.is_visual() && Self::action_is_selection_motion(action) {
            self.update_selection();
            self.render(buffer)?;
        }

        if add_to_history && Self::records_jump(action) {
            self.save_to_history(history_entry_before_action);
        }

        if self.current_buffer().id() == action_buffer_id
            && self.current_buffer().revision() != action_buffer_revision
        {
            self.flush_change_notification(runtime).await?;
        }

        // Sync editor state back to the active window after executing actions
        // This ensures window state is updated even for actions that don't trigger a full render
        self.sync_to_window();

        // Always render after actions when in multi-window mode to ensure changes are visible
        if self.window_manager.windows().len() > 1 {
            self.render(buffer)?;
        }

        if self.defer_motion_render {
            if let Some((_, cause)) = &mut self.deferred_plugin_event {
                *cause = action_cause;
            } else {
                self.deferred_plugin_event = Some((event_snapshot_before_action, action_cause));
            }
            perf::increment("plugin_events_coalesced", 1);
        } else {
            self.notify_editor_event_changes(event_snapshot_before_action, runtime, &action_cause)
                .await?;
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
                | Action::JumpToMark { .. }
                | Action::OpenLocation(_, _)
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
        let (x, y) = if y0 <= y1 { (x0, y0) } else { (x1, y1) };
        if !self.is_within_viewport(y) {
            self.vtop = y;
        }
        self.cx = x;
        self.cy = y.saturating_sub(self.vtop);
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

                    if matches!(mode, Mode::Insert) {
                        self.insert_entry_cursor = Some(self.cursor_snapshot());
                        if !self.transaction_active() {
                            self.begin_transaction("insert block");
                        }
                    }

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

    async fn change_selection(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let Some(selection) = self.selection else {
            return Ok(());
        };
        let mode = self.mode;
        if !matches!(mode, Mode::Visual | Mode::VisualLine | Mode::VisualBlock) {
            return Ok(());
        }

        let (x0, y0, x1, y1) = selection.into();
        let insertion_x = if matches!(mode, Mode::VisualBlock) {
            x0.min(x1)
        } else {
            x0
        };
        let preserve_line = matches!(mode, Mode::VisualLine) && y1 < self.current_buffer().len();

        self.begin_transaction("change selection");
        if self.delete_selection().is_none() {
            self.cancel_transaction_if_empty();
            return Ok(());
        }

        if preserve_line {
            self.replace_range(TextRange::insertion(TextPosition::new(y0, 0)), "\n");
        }

        let insertion_y = y0.min(self.current_buffer().len());
        if !self.is_within_viewport(insertion_y) {
            self.vtop = insertion_y;
        }
        self.cy = insertion_y.saturating_sub(self.vtop);
        self.cx = insertion_x.min(self.length_for_line(insertion_y));
        self.selection = None;
        self.notify_change(runtime).await?;

        if matches!(mode, Mode::VisualBlock) {
            self.selection = Some(Rect::new(insertion_x, y0, insertion_x, y1));
            self.execute_block_action(buffer, runtime, Mode::Insert)
                .await?;
        } else {
            self.insert_entry_cursor = Some(self.cursor_snapshot());
            self.execute(&Action::EnterMode(Mode::Insert), buffer, runtime)
                .await?;
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
        let end = actions_end.saturating_sub(1);

        // Actions to replicate to the remaining selected lines. `actions_end`
        // includes the recursive `InsertBlock` action that completed replay,
        // and the preceding action is usually the `Esc`/`EnterMode(Normal)`
        // that triggered completion. Replaying that mode transition would
        // commit the active insert transaction per row.
        let mut actions = if start <= end && end <= self.actions.len() {
            self.actions[start..end].to_vec()
        } else {
            Vec::new()
        };
        if matches!(actions.last(), Some(Action::EnterMode(Mode::Normal))) {
            actions.pop();
        }

        let (y0, y1) = if selection.y0 < selection.y1 {
            (selection.y0, selection.y1)
        } else {
            (selection.y1, selection.y0)
        };

        let mut scratch_buffer = buffer.clone();
        let previous_terminal_output_enabled = self.terminal_output_enabled;
        self.terminal_output_enabled = false;
        self.block_replay_depth += 1;

        let mut replay_result = Ok(());
        for y in y0 + 1..=y1 {
            if !self.is_within_viewport(y) {
                self.vtop = y;
            }
            self.cy = y.saturating_sub(self.vtop);
            self.cx = selection.x0;
            for action in &actions {
                if let Err(error) = self.execute(action, &mut scratch_buffer, runtime).await {
                    replay_result = Err(error);
                    break;
                }
            }
            if replay_result.is_err() {
                break;
            }
        }

        self.block_replay_depth = self.block_replay_depth.saturating_sub(1);
        self.terminal_output_enabled = previous_terminal_output_enabled;

        if let Err(error) = replay_result {
            if self.block_replay_depth == 0 {
                self.block_replay_change_deferred = false;
            }
            return Err(error);
        }

        if self.block_replay_depth == 0 && self.block_replay_change_deferred {
            self.block_replay_change_deferred = false;
            self.notify_change(runtime).await?;
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
            self.set_register(register, content);

            return needs_update;
        }

        false
    }

    fn set_register(&mut self, register: char, content: Content) {
        if register == DEFAULT_REGISTER {
            self.write_system_clipboard(&content.text);
        }
        self.registers.insert(register, content);
    }

    fn set_default_register(&mut self, content: Content) {
        self.set_register(DEFAULT_REGISTER, content);
    }

    fn write_system_clipboard(&mut self, text: &str) {
        if !self.config.clipboard.enabled || !self.config.clipboard.sync_on_yank {
            return;
        }

        if let Err(error) = self.clipboard.set_text(text) {
            log!("failed to write system clipboard: {error}");
        }
    }

    fn refresh_default_register_from_system_clipboard(&mut self) {
        if !self.config.clipboard.enabled || !self.config.clipboard.sync_on_paste {
            return;
        }

        let text = match self.clipboard.get_text() {
            Ok(Some(text)) => text,
            Ok(None) => return,
            Err(error) => {
                log!("failed to read system clipboard: {error}");
                return;
            }
        };

        if self
            .registers
            .get(&DEFAULT_REGISTER)
            .is_some_and(|content| content.text == text)
        {
            return;
        }

        self.registers
            .insert(DEFAULT_REGISTER, Content::charwise(text));
    }

    fn yank_current_line(&mut self) -> bool {
        let Some(line) = self.current_buffer().get(self.buffer_line()) else {
            return false;
        };

        self.set_default_register(Content::linewise(line));
        true
    }

    fn yank_text_range(&mut self, range: TextRange) -> bool {
        let text = self.current_buffer().text_in_range(range);
        if text.is_empty() {
            return false;
        }

        self.set_default_register(Content::charwise(text));
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

                self.set_default_register(content.clone());

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
        self.refresh_default_register_from_system_clipboard();
        let contents = self.registers.get(&DEFAULT_REGISTER).cloned();

        if let Some(contents) = contents {
            self.paste(&contents, before);
            return true;
        }

        false
    }

    fn paste_over_selection(&mut self, preserve_default_register: bool) -> bool {
        self.refresh_default_register_from_system_clipboard();
        let Some(source) = self.registers.get(&DEFAULT_REGISTER).cloned() else {
            return false;
        };
        let Some(replaced) = self.selected_content() else {
            return false;
        };
        let Some(plan) = self.visual_paste_plan(&source) else {
            return false;
        };

        let original = self.current_buffer().contents();
        let end = self.position_for_char_idx(original.chars().count());
        self.begin_transaction("visual paste");
        self.replace_range(TextRange::new(TextPosition::new(0, 0), end), &plan.text);
        self.selection = None;
        self.move_to_text_position(plan.cursor);
        self.fix_cursor_pos();
        if !preserve_default_register {
            self.set_default_register(replaced);
        }
        self.commit_transaction(self.cursor_snapshot());
        true
    }

    fn visual_paste_plan(&self, source: &Content) -> Option<VisualPastePlan> {
        let selection = self.selection?;
        let (x0, y0, x1, y1) = selection.into();
        let mut lines = self
            .current_buffer()
            .contents()
            .split('\n')
            .map(str::to_string)
            .collect::<Vec<_>>();
        if lines.is_empty() {
            lines.push(String::new());
        }

        let cursor = match self.mode {
            Mode::Visual => self.plan_charwise_visual_paste(&mut lines, x0, y0, x1, y1, source)?,
            Mode::VisualLine => {
                let replacement: Vec<String> = match source.kind {
                    ContentKind::Charwise => source.text.split('\n').map(str::to_string).collect(),
                    ContentKind::Linewise | ContentKind::Blockwise => {
                        source.text.lines().map(str::to_string).collect()
                    }
                };
                lines.splice(y0..=y1, replacement);
                TextPosition::new(y0, 0)
            }
            Mode::VisualBlock => {
                self.plan_blockwise_visual_paste(&mut lines, x0, y0, x1, y1, source)?
            }
            _ => return None,
        };

        if lines.is_empty() {
            lines.push(String::new());
        }
        Some(VisualPastePlan {
            text: lines.join("\n"),
            cursor,
        })
    }

    fn plan_charwise_visual_paste(
        &self,
        lines: &mut Vec<String>,
        x0: usize,
        y0: usize,
        x1: usize,
        y1: usize,
        source: &Content,
    ) -> Option<TextPosition> {
        let start = self.grapheme_to_char_on_line(x0, y0);
        let end = self.grapheme_to_char_on_line(x1 + 1, y1);
        let prefix = char_prefix(lines.get(y0)?, start).to_string();
        let suffix = char_suffix(lines.get(y1)?, end).to_string();

        match source.kind {
            ContentKind::Charwise => {
                let source_lines = source.text.split('\n').collect::<Vec<_>>();
                let replacement = if source_lines.len() == 1 {
                    vec![format!("{prefix}{}{suffix}", source_lines[0])]
                } else {
                    let mut replacement = Vec::with_capacity(source_lines.len());
                    replacement.push(format!("{prefix}{}", source_lines[0]));
                    replacement.extend(
                        source_lines[1..source_lines.len() - 1]
                            .iter()
                            .map(|line| (*line).to_string()),
                    );
                    replacement.push(format!("{}{suffix}", source_lines[source_lines.len() - 1]));
                    replacement
                };
                lines.splice(y0..=y1, replacement);

                let cursor = if source.text.is_empty() {
                    TextPosition::new(y0, prefix.chars().count())
                } else if source_lines.len() == 1 {
                    TextPosition::new(
                        y0,
                        prefix.chars().count() + source_lines[0].chars().count().saturating_sub(1),
                    )
                } else {
                    TextPosition::new(
                        y0 + source_lines.len() - 1,
                        source_lines[source_lines.len() - 1]
                            .chars()
                            .count()
                            .saturating_sub(1),
                    )
                };
                Some(cursor)
            }
            ContentKind::Linewise => {
                let mut replacement = vec![prefix];
                replacement.extend(source.text.lines().map(str::to_string));
                replacement.push(suffix);
                let cursor = TextPosition::new(y0 + 1, 0);
                lines.splice(y0..=y1, replacement);
                Some(cursor)
            }
            ContentKind::Blockwise => {
                lines.splice(y0..=y1, [format!("{prefix}{suffix}")]);
                let paste_x = grapheme_len(&prefix);
                for (offset, block_line) in source.text.lines().enumerate() {
                    insert_at_grapheme_column(lines, y0 + offset, paste_x, block_line);
                }
                Some(TextPosition::new(y0, prefix.chars().count()))
            }
        }
    }

    fn plan_blockwise_visual_paste(
        &self,
        lines: &mut Vec<String>,
        x0: usize,
        y0: usize,
        x1: usize,
        y1: usize,
        source: &Content,
    ) -> Option<TextPosition> {
        let min_x = x0.min(x1);
        let max_x = x0.max(x1);
        let top_line = lines.get(y0)?.clone();
        let top_start = grapheme_to_byte(&top_line, min_x.min(grapheme_len(&top_line)));
        let top_end = grapheme_to_byte(&top_line, (max_x + 1).min(grapheme_len(&top_line)));
        let top_prefix = top_line[..top_start].to_string();
        let top_suffix = top_line[top_end..].to_string();

        match source.kind {
            ContentKind::Charwise if source.text.contains('\n') => {
                for y in (y0 + 1)..=y1 {
                    remove_grapheme_columns(lines, y, min_x, max_x);
                }
                let source_lines = source.text.split('\n').collect::<Vec<_>>();
                let mut replacement = Vec::with_capacity(source_lines.len());
                replacement.push(format!("{top_prefix}{}", source_lines[0]));
                replacement.extend(
                    source_lines[1..source_lines.len() - 1]
                        .iter()
                        .map(|line| (*line).to_string()),
                );
                replacement.push(format!(
                    "{}{top_suffix}",
                    source_lines[source_lines.len() - 1]
                ));
                lines.splice(y0..=y0, replacement);
                Some(TextPosition::new(y0, top_prefix.chars().count()))
            }
            ContentKind::Charwise => {
                for y in y0..=y1 {
                    remove_grapheme_columns(lines, y, min_x, max_x);
                    insert_at_grapheme_column(lines, y, min_x, &source.text);
                }
                Some(TextPosition::new(y0, min_x))
            }
            ContentKind::Linewise => {
                for y in y0..=y1 {
                    remove_grapheme_columns(lines, y, min_x, max_x);
                }
                let insertion = source.text.lines().map(str::to_string).collect::<Vec<_>>();
                lines.splice((y1 + 1)..(y1 + 1), insertion);
                Some(TextPosition::new(y1 + 1, 0))
            }
            ContentKind::Blockwise => {
                for y in y0..=y1 {
                    remove_grapheme_columns(lines, y, min_x, max_x);
                }
                for (offset, block_line) in source.text.lines().enumerate() {
                    insert_at_grapheme_column(lines, y0 + offset, min_x, block_line);
                }
                Some(TextPosition::new(y0, min_x))
            }
        }
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
        let lines = contents.text.lines().collect::<Vec<_>>().join("\n");
        let after_unterminated_last_line = !before
            && y == self.current_buffer().len()
            && self
                .current_buffer()
                .get(y)
                .is_some_and(|line| !line.ends_with('\n'));
        let mut text = String::new();
        if after_unterminated_last_line {
            text.push('\n');
            text.push_str(&lines);
        } else {
            text.push_str(&lines);
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
        if self.block_replay_depth > 0 {
            self.block_replay_change_deferred = true;
            return Ok(());
        }

        let file = self.current_buffer().file.clone();

        // Notify LSP if enabled and the buffer has a file.
        if self.config.lsp.enabled {
            if let Some(file) = &file {
                self.ensure_current_buffer_lsp_opened().await?;
                self.lsp
                    .did_change(file, self.current_buffer().contents())
                    .await?;
            }
        }

        // Notify plugins about buffer change
        let buffer_info = serde_json::json!({
            "buffer_id": self.current_buffer_index,
            "buffer_name": self.current_buffer().name(),
            "file_path": file,
            "revision": self.current_buffer().revision(),
            "line_count": self.current_buffer().len(),
            "cursor": {
                "line": self.cy + self.vtop,
                "column": self.cx
            }
        });

        self.plugin_registry
            .notify(runtime, "buffer:changed", buffer_info)
            .await?;

        self.notified_buffer_revisions
            .insert(self.current_buffer().id(), self.current_buffer().revision());

        Ok(())
    }

    async fn flush_change_notification(&mut self, runtime: &mut Runtime) -> anyhow::Result<()> {
        let revision = self.current_buffer().revision();
        if self
            .notified_buffer_revisions
            .get(&self.current_buffer().id())
            .is_some_and(|notified| *notified == revision)
        {
            return Ok(());
        }

        self.notify_change(runtime).await
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
        let removed_id = self.current_buffer().id();
        let removed_uri = self.current_buffer().uri()?;
        if let Some(uri) = removed_uri.as_deref() {
            let still_open = self.buffers.iter().enumerate().any(|(index, buffer)| {
                index != self.current_buffer_index
                    && buffer.uri().ok().flatten().as_deref() == Some(uri)
            });
            if !still_open && self.lsp_opened_documents.remove(uri) {
                if let Ok(file) = lsp_file_path(uri) {
                    self.lsp.did_close(&file).await?;
                }
                self.diagnostics.remove(uri);
            }
        }
        self.notified_buffer_revisions.remove(&removed_id);

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
        self.ensure_buffer_lsp_opened(self.current_buffer_index)
            .await
    }

    async fn ensure_buffer_lsp_opened(&mut self, buffer_index: usize) -> anyhow::Result<()> {
        let Some(buffer) = self.buffers.get(buffer_index) else {
            return Ok(());
        };
        let Some(file) = buffer.file.clone() else {
            return Ok(());
        };
        let Some(uri) = buffer.uri()? else {
            return Ok(());
        };
        if self.lsp_opened_documents.contains(&uri) {
            return Ok(());
        }
        let contents = buffer.contents();
        self.lsp.did_open(&file, &contents).await?;
        self.lsp_opened_documents.insert(uri);
        Ok(())
    }

    async fn sync_lsp_document_identity(
        &mut self,
        previous_uri: Option<&str>,
        buffer_index: usize,
    ) -> anyhow::Result<()> {
        let current_uri = self
            .buffers
            .get(buffer_index)
            .and_then(|buffer| buffer.uri().ok().flatten());
        if previous_uri == current_uri.as_deref() {
            return Ok(());
        }
        if let Some(previous_uri) = previous_uri {
            if self.lsp_opened_documents.remove(previous_uri) {
                if let Ok(file) = lsp_file_path(previous_uri) {
                    self.lsp.did_close(&file).await?;
                }
            }
            if let Some(diagnostics) = self.diagnostics.remove(previous_uri) {
                if let Some(current_uri) = current_uri.as_ref() {
                    self.diagnostics.insert(current_uri.clone(), diagnostics);
                }
            }
        }
        self.ensure_buffer_lsp_opened(buffer_index).await
    }

    fn apply_theme(&mut self, theme_name: &str, update_config: bool) -> anyhow::Result<()> {
        let Some(theme_asset) = crate::assets::resolve_theme(theme_name, &Config::config_dir())
        else {
            anyhow::bail!("Theme file {} not found", theme_name);
        };
        let theme = if let Some(path) = theme_asset.path() {
            parse_vscode_theme(&path.to_string_lossy())?
        } else {
            parse_vscode_theme_contents(&theme_asset.read_to_string()?)?
        };
        let highlighter = Highlighter::new(&theme)?;
        self.theme = theme;
        self.highlighter = highlighter;
        self.highlight_cache.clear();
        self.completion_ui.set_theme(&self.theme);
        if let Some(dialog) = &mut self.current_dialog {
            dialog.set_theme(&self.theme);
        }
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

    fn plugin_document_symbols_payload(
        &self,
        response: &ResponseMessage,
        pending: &PendingDocumentSymbols,
    ) -> anyhow::Result<Value> {
        let file = response_text_document_uri(response)
            .map(|uri| self.uri_to_file(uri))
            .or_else(|| self.current_file_name())
            .ok_or_else(|| anyhow::anyhow!("document symbol response did not include a file"))?;
        let symbols = self.normalize_document_symbols(&response.result, &file)?;

        Ok(json!({
            "ok": true,
            "file": file,
            "buffer_index": pending.buffer_index,
            "revision": pending.revision,
            "symbols": symbols,
        }))
    }

    fn plugin_workspace_symbols_payload(
        &self,
        response: &ResponseMessage,
    ) -> anyhow::Result<Value> {
        let symbols = self.normalize_workspace_symbols(&response.result)?;

        Ok(json!({
            "ok": true,
            "symbols": symbols,
        }))
    }

    fn plugin_references_payload(&self, response: &ResponseMessage) -> anyhow::Result<Value> {
        let request = response
            .request
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("references response did not include its request"))?;
        let params = request
            .params
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("references request params were not an object"))?;
        let text_document = params
            .get("textDocument")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("references request did not include a text document"))?;
        let file = self.uri_to_file(required_string(text_document, "uri")?);
        let position: crate::lsp::Position =
            serde_json::from_value(params.get("position").cloned().ok_or_else(|| {
                anyhow::anyhow!("references request did not include a position")
            })?)?;
        let references = self.normalize_locations(&response.result)?;

        Ok(json!({
            "ok": true,
            "file": file,
            "position": position,
            "references": references,
        }))
    }

    fn plugin_inlay_hints_payload(&self, response: &ResponseMessage) -> anyhow::Result<Value> {
        let file = response_text_document_uri(response)
            .map(|uri| self.uri_to_file(uri))
            .or_else(|| self.current_file_name())
            .ok_or_else(|| anyhow::anyhow!("inlay hint response did not include a file"))?;
        let hints = plugin_json(serde_json::to_value(
            self.normalize_inlay_hints(&response.result)?,
        )?);

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
        for (index, value) in values.iter().enumerate() {
            self.push_normalized_symbol(value, fallback_file, 0, None, index, &mut symbols)?;
        }
        Ok(symbols)
    }

    fn normalize_workspace_symbols(
        &self,
        result: &Value,
    ) -> anyhow::Result<Vec<PluginDocumentSymbol>> {
        if result.is_null() {
            return Ok(Vec::new());
        }

        result
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("workspace symbol response was not an array"))?
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let name = required_string_value(value, "name")?;
                let id = format!("root:{index}:{name}");
                self.normalized_symbol_information(value, 0, id, None)
            })
            .collect()
    }

    fn normalize_locations(&self, result: &Value) -> anyhow::Result<Vec<PluginLocation>> {
        if result.is_null() {
            return Ok(Vec::new());
        }

        let locations: Vec<Location> = serde_json::from_value(result.clone())?;
        Ok(locations
            .into_iter()
            .map(|location| PluginLocation {
                file: self.uri_to_file(&location.uri),
                range: location.range,
            })
            .collect())
    }

    fn push_normalized_symbol(
        &self,
        value: &Value,
        fallback_file: &str,
        depth: usize,
        parent_id: Option<&str>,
        index: usize,
        symbols: &mut Vec<PluginDocumentSymbol>,
    ) -> anyhow::Result<()> {
        let name = required_string_value(value, "name")?;
        let id = format!("{}:{index}:{name}", parent_id.unwrap_or("root"));
        if value.get("location").is_some() {
            symbols.push(self.normalized_symbol_information(
                value,
                depth,
                id,
                parent_id.map(ToString::to_string),
            )?);
            return Ok(());
        }

        let symbol = normalized_document_symbol(
            value,
            fallback_file,
            depth,
            id.clone(),
            parent_id.map(ToString::to_string),
        )?;
        symbols.push(symbol);

        if let Some(children) = value.get("children").and_then(Value::as_array) {
            for (child_index, child) in children.iter().enumerate() {
                self.push_normalized_symbol(
                    child,
                    fallback_file,
                    depth + 1,
                    Some(&id),
                    child_index,
                    symbols,
                )?;
            }
        }

        Ok(())
    }

    fn normalized_symbol_information(
        &self,
        value: &Value,
        depth: usize,
        id: String,
        parent_id: Option<String>,
    ) -> anyhow::Result<PluginDocumentSymbol> {
        let location = value
            .get("location")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("symbol information did not include a location"))?;
        let uri = required_string(location, "uri")?;
        let range = required_range(location.get("range"), "location.range")?;
        let kind = required_kind(value)?;

        Ok(PluginDocumentSymbol {
            id,
            parent_id,
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
        if let Ok(file) = lsp_file_path(uri) {
            let path = Path::new(&file);
            if let Ok(relative) = path.strip_prefix(get_workspace_path()) {
                return relative.to_string_lossy().into_owned();
            }
            return file;
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

                            let local_y = click_y.saturating_sub(window.position.y);
                            if local_y < self.window_content_top(&window) {
                                let local_x = click_x.saturating_sub(window.position.x);
                                if let Some(rendered) = self
                                    .window_bar_manager
                                    .render(window.id, window.inner_width())
                                {
                                    if let Some(region) =
                                        rendered.hit_regions.iter().find(|region| {
                                            local_x >= region.start_column
                                                && local_x < region.end_column
                                        })
                                    {
                                        return Some(KeyAction::Single(Action::NotifyPlugins(
                                            format!("window_bar:action:{}", rendered.bar_id),
                                            json!({
                                                "window_id": window.id.0,
                                                "segment_id": region.segment_id,
                                                "action": region.action,
                                            }),
                                        )));
                                    }
                                }
                                return Some(KeyAction::None);
                            }

                            // Convert terminal coordinates to window-local coordinates
                            if let Some((local_x, local_y)) =
                                window.terminal_to_local(click_x, click_y)
                            {
                                let local_y = local_y - self.window_content_top(&window);
                                // Adjust for the clicked window's gutter, not the active buffer's.
                                let gutter_width =
                                    self.gutter_width_for_buffer_index(window_buffer_index);
                                let content_x = local_x.saturating_sub(gutter_width + 1);
                                let layout = self.layout_for_window(&window);
                                let (buffer_x, buffer_y) = if let Some(segment) =
                                    layout.row(local_y)
                                {
                                    // Clicks inside the break-indent area
                                    // snap to the row's first character.
                                    let display_col = segment.start_col
                                        + content_x.saturating_sub(segment.visual_offset);
                                    let line = self.buffers[window_buffer_index]
                                        .get(segment.line)
                                        .unwrap_or_default();
                                    (
                                        column_to_grapheme_with_tabs(
                                            line.trim_end_matches('\n'),
                                            display_col,
                                            self.tab_width_for_buffer_index(window_buffer_index),
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
        self.begin_transaction_with_origin(label, EditOrigin::User);
    }

    fn begin_transaction_with_origin(&mut self, label: impl Into<String>, origin: EditOrigin) {
        let before_cursor = self.cursor_snapshot();
        self.current_buffer_mut()
            .undo_history
            .begin_transaction_with_origin(label, before_cursor, origin);
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

    fn anchor_at_char(&self, char_index: usize, affinity: AnchorAffinity) -> EditAnchor {
        EditAnchor {
            buffer_id: self.current_buffer().id(),
            file: self.current_buffer().file.clone(),
            char_index,
            fallback: self.current_buffer().char_idx_to_position(char_index),
            affinity,
        }
    }

    fn cursor_anchor(&self, affinity: AnchorAffinity) -> EditAnchor {
        let char_index = self
            .current_buffer()
            .position_to_char_idx(self.cursor_text_position());
        self.anchor_at_char(char_index, affinity)
    }

    fn set_named_mark(&mut self, mark: char) {
        let anchor = self.cursor_anchor(AnchorAffinity::Right);
        if mark.is_ascii_lowercase() {
            self.local_marks
                .entry(anchor.buffer_id)
                .or_default()
                .insert(mark, anchor);
        } else {
            self.global_marks.insert(mark, anchor);
        }
    }

    fn set_special_mark_at_char(
        &mut self,
        mark: char,
        char_index: usize,
        affinity: AnchorAffinity,
    ) {
        let anchor = self.anchor_at_char(char_index, affinity);
        self.special_marks.insert((anchor.buffer_id, mark), anchor);
    }

    fn capture_last_visual_marks(&mut self) {
        let Some(selection) = self.selection else {
            return;
        };
        let (x0, y0, x1, y1) = selection.into();
        let start = TextPosition::new(y0, self.grapheme_to_char_on_line(x0, y0));
        let end = TextPosition::new(y1, self.grapheme_to_char_on_line(x1, y1));
        let start_char = self.current_buffer().position_to_char_idx(start);
        let end_char = self.current_buffer().position_to_char_idx(end);
        self.set_special_mark_at_char('<', start_char, AnchorAffinity::Left);
        self.set_special_mark_at_char('>', end_char, AnchorAffinity::Right);
    }

    fn transform_anchor_for_edit(
        anchor: &mut EditAnchor,
        start_char: usize,
        end_char: usize,
        new_char_len: usize,
    ) {
        let replaced_len = end_char.saturating_sub(start_char);
        anchor.char_index = if anchor.char_index < start_char {
            anchor.char_index
        } else if replaced_len == 0 && anchor.char_index == start_char {
            match anchor.affinity {
                AnchorAffinity::Left => start_char,
                AnchorAffinity::Right => start_char.saturating_add(new_char_len),
            }
        } else if anchor.char_index >= end_char {
            anchor
                .char_index
                .saturating_sub(replaced_len)
                .saturating_add(new_char_len)
        } else {
            match anchor.affinity {
                AnchorAffinity::Left => start_char,
                AnchorAffinity::Right => start_char.saturating_add(new_char_len),
            }
        };
    }

    fn update_anchors_for_edit(&mut self, edit: AppliedTextEdit) {
        let buffer_id = self.current_buffer().id();
        if let Some(marks) = self.local_marks.get_mut(&buffer_id) {
            for anchor in marks.values_mut() {
                Self::transform_anchor_for_edit(
                    anchor,
                    edit.start_char,
                    edit.end_char,
                    edit.new_char_len,
                );
            }
        }
        for anchor in self.global_marks.values_mut() {
            if anchor.buffer_id == buffer_id {
                Self::transform_anchor_for_edit(
                    anchor,
                    edit.start_char,
                    edit.end_char,
                    edit.new_char_len,
                );
            }
        }
        for ((anchor_buffer_id, _), anchor) in &mut self.special_marks {
            if *anchor_buffer_id == buffer_id {
                Self::transform_anchor_for_edit(
                    anchor,
                    edit.start_char,
                    edit.end_char,
                    edit.new_char_len,
                );
            }
        }

        let buffer = self.current_buffer();
        let fallback_positions = self
            .local_marks
            .get(&buffer_id)
            .into_iter()
            .flat_map(|marks| marks.values())
            .chain(
                self.global_marks
                    .values()
                    .filter(|anchor| anchor.buffer_id == buffer_id),
            )
            .chain(
                self.special_marks
                    .iter()
                    .filter(|((anchor_buffer_id, _), _)| *anchor_buffer_id == buffer_id)
                    .map(|(_, anchor)| anchor),
            )
            .map(|anchor| {
                (
                    anchor.char_index,
                    buffer.char_idx_to_position(anchor.char_index),
                )
            })
            .collect::<HashMap<_, _>>();
        let update_fallback = |anchor: &mut EditAnchor| {
            if let Some(position) = fallback_positions.get(&anchor.char_index) {
                anchor.fallback = *position;
            }
        };
        if let Some(marks) = self.local_marks.get_mut(&buffer_id) {
            marks.values_mut().for_each(update_fallback);
        }
        self.global_marks
            .values_mut()
            .filter(|anchor| anchor.buffer_id == buffer_id)
            .for_each(update_fallback);
        self.special_marks
            .iter_mut()
            .filter(|((anchor_buffer_id, _), _)| *anchor_buffer_id == buffer_id)
            .map(|(_, anchor)| anchor)
            .for_each(update_fallback);
    }

    fn replace_range(&mut self, range: TextRange, new_text: &str) {
        let old_text = self.current_buffer().text_in_range(range);
        if old_text == new_text {
            return;
        }
        assert!(
            self.transaction_active(),
            "editor content mutations must occur inside an edit transaction"
        );
        let edit = AppliedTextEdit {
            start_char: self.current_buffer().position_to_char_idx(range.start),
            end_char: self.current_buffer().position_to_char_idx(range.end),
            new_char_len: new_text.chars().count(),
        };
        self.current_buffer_mut().replace_range_raw(range, new_text);
        self.update_anchors_for_edit(edit);
        self.set_special_mark_at_char('.', edit.start_char, AnchorAffinity::Left);
        self.current_buffer_mut().undo_history.record_replace(
            range,
            edit.start_char,
            old_text,
            new_text.to_string(),
        );
    }

    fn delete_text_range(&mut self, range: TextRange, label: &str) -> bool {
        let deleted_text = self.current_buffer().text_in_range(range);
        self.set_default_register(Content::charwise(deleted_text.clone()));
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

    fn current_line_range(&self, count: u16, include_line_ending: bool) -> TextRange {
        let line = self.buffer_line();
        let last_line = line
            .saturating_add(usize::from(count.saturating_sub(1)))
            .min(self.current_buffer().len());
        let end = if include_line_ending && last_line < self.current_buffer().len() {
            TextPosition::new(last_line + 1, 0)
        } else {
            TextPosition::new(last_line, self.length_for_line(last_line))
        };
        TextRange::new(TextPosition::new(line, 0), end)
    }

    fn delete_linewise_range(&mut self, range: TextRange, label: &str) -> bool {
        let deleted_text = self.current_buffer().text_in_range(range);
        self.set_default_register(Content::linewise(deleted_text.clone()));
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

    fn begin_change_range(&mut self, range: TextRange, label: &str, linewise: bool) -> bool {
        let deleted_text = self.current_buffer().text_in_range(range);
        let content = if linewise {
            Content::linewise(deleted_text.clone())
        } else {
            Content::charwise(deleted_text.clone())
        };
        self.set_default_register(content);
        self.move_to_text_position(range.start);

        if deleted_text.is_empty() {
            return false;
        }

        self.begin_transaction(label);
        self.replace_range(range, "");
        self.move_to_text_position(range.start);
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

    /// Insert mode permits the cursor on the empty line after a trailing
    /// newline. Normal motions intentionally clamp to the last navigable line.
    fn move_to_insert_text_position(&mut self, position: TextPosition) {
        let y = position.line.min(self.current_buffer().len());
        if !self.is_within_viewport(y) {
            self.vtop = y;
        }
        self.cy = y.saturating_sub(self.vtop);
        let char_x = position.character.min(self.length_for_line(y));
        self.cx = self.char_to_grapheme_on_line(char_x, y);
    }

    fn move_to_forward_character(
        &mut self,
        target: char,
        count: u16,
        kind: ForwardCharacterMotion,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<()> {
        if let Some(position) = self.forward_character_target(target, count, kind) {
            self.move_to_text_position(position);
            self.finish_cursor_motion(buffer, false)?;
        } else {
            self.last_error = Some("character not found".to_string());
        }
        Ok(())
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
        let outcome = history.undo(buffer);
        buffer.undo_history = history;
        buffer.refresh_dirty_from_history();

        if let Some((cursor, edits)) = outcome {
            for edit in edits {
                self.update_anchors_for_edit(edit);
                self.set_special_mark_at_char('.', edit.start_char, AnchorAffinity::Left);
            }
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
        let outcome = history.redo(buffer);
        buffer.undo_history = history;
        buffer.refresh_dirty_from_history();

        if let Some((cursor, edits)) = outcome {
            for edit in edits {
                self.update_anchors_for_edit(edit);
                self.set_special_mark_at_char('.', edit.start_char, AnchorAffinity::Left);
            }
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

    pub fn set_session_store(&mut self, store: SessionStore) {
        self.session_store = Some(store);
        self.last_session_snapshot = Instant::now();
        self.last_session_snapshot_generation = None;
    }

    #[doc(hidden)]
    pub fn test_persist_session_snapshot(&mut self, force: bool, due: bool) {
        if due {
            self.last_session_snapshot = Instant::now() - SESSION_SNAPSHOT_INTERVAL;
        }
        self.persist_session_snapshot(force);
    }

    #[doc(hidden)]
    #[must_use]
    pub fn test_session_snapshot_is_backing_off(&self) -> bool {
        self.last_session_snapshot.elapsed() < SESSION_SNAPSHOT_INTERVAL
    }

    #[doc(hidden)]
    pub fn test_finish_session_snapshot(&mut self) {
        if let Some(writer) = self.session_snapshot_writer.take() {
            self.finish_session_snapshot(writer);
        }
    }

    pub fn buffers_from_session_snapshot(snapshot: &SessionSnapshot) -> Vec<Buffer> {
        snapshot
            .buffers
            .iter()
            .map(|saved| {
                let mut buffer = Buffer::from_session_snapshot(
                    saved.path.clone(),
                    saved.contents.clone(),
                    saved.dirty,
                    saved.revision,
                    saved.undo_history.clone(),
                );
                buffer.vtop = saved.viewport_top;
                buffer.pos = (
                    saved.cursor_x,
                    saved.cursor_y.saturating_sub(saved.viewport_top),
                );
                buffer
            })
            .collect()
    }

    pub fn restore_session_snapshot(
        &mut self,
        snapshot: &SessionSnapshot,
    ) -> anyhow::Result<Vec<RecoveryDivergence>> {
        anyhow::ensure!(
            snapshot.version == SESSION_SCHEMA_VERSION,
            "session snapshot was not migrated to the current schema"
        );
        anyhow::ensure!(
            self.buffers.len() == snapshot.buffers.len(),
            "session buffer count does not match the reconstructed editor"
        );
        let divergences = detect_disk_divergence(snapshot);
        let buffer_map = snapshot
            .buffers
            .iter()
            .enumerate()
            .map(|(position, buffer)| (buffer.index, position))
            .collect::<HashMap<_, _>>();
        self.current_buffer_index = buffer_map
            .get(&snapshot.current_buffer_index)
            .copied()
            .unwrap_or_default();
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
        self.registers = snapshot.registers.clone();
        self.jump_list = snapshot
            .jumps
            .iter()
            .map(|jump| HistoryEntry {
                file: jump.file.clone(),
                x: jump.x,
                y: jump.y,
            })
            .collect();
        self.jump_index = snapshot.jump_index.min(self.jump_list.len());
        self.local_marks.clear();
        self.global_marks.clear();
        self.special_marks.clear();
        for mark in &snapshot.local_marks {
            if let Some(anchor) = self.restore_session_mark(mark, &buffer_map) {
                self.local_marks
                    .entry(anchor.buffer_id)
                    .or_default()
                    .insert(mark.name, anchor);
            }
        }
        for mark in &snapshot.global_marks {
            if let Some(anchor) = self.restore_session_mark(mark, &buffer_map) {
                self.global_marks.insert(mark.name, anchor);
            }
        }
        for mark in &snapshot.special_marks {
            if let Some(anchor) = self.restore_session_mark(mark, &buffer_map) {
                self.special_marks
                    .insert((anchor.buffer_id, mark.name), anchor);
            }
        }
        self.agent_workspace = snapshot
            .agent_workspace
            .clone()
            .map(|workspace| Arc::new(Mutex::new(ProposalWorkspace::from_snapshot(workspace))));
        if let Some(transcript) = &snapshot.agent_transcript {
            let transcript_persisted = if let Err(error) = self.preferences.set_plugin_storage(
                "agent",
                "transcript",
                Value::String(transcript.clone()),
            ) {
                log!(
                    "{}",
                    json!({
                        "event": "recovery_transcript_persistence_failed",
                        "level": "warn",
                        "service": "red",
                        "error": error.to_string(),
                    })
                );
                false
            } else {
                true
            };
            if !snapshot.agent_session_resumable {
                self.last_error = Some(if transcript_persisted {
                    "Recovered agent transcript as archived context; start a new session to continue"
                        .to_string()
                } else {
                    "Recovered agent transcript as archived context, but it could not be persisted; start a new session to continue"
                        .to_string()
                });
            } else if !transcript_persisted {
                self.last_error = Some(
                    "Recovered buffers and agent transcript; transcript could not be persisted"
                        .to_string(),
                );
            }
        }
        if !divergences.is_empty() {
            let mut warning = format!(
                "Recovered unsaved state; {} file(s) changed on disk (see recovery report)",
                divergences.len()
            );
            if self
                .last_error
                .as_deref()
                .is_some_and(|message| message.contains("could not be persisted"))
            {
                warning.push_str("; agent transcript could not be persisted");
            }
            self.last_error = Some(warning);
        }
        self.recompute_window_cursor_goals();
        self.sync_with_window();
        self.check_bounds();
        Ok(divergences)
    }

    fn restore_session_mark(
        &self,
        mark: &SessionMark,
        buffer_map: &HashMap<usize, usize>,
    ) -> Option<EditAnchor> {
        let buffer_index = *buffer_map.get(&mark.buffer_index)?;
        let buffer_id = self.buffers.get(buffer_index)?.id();
        Some(EditAnchor {
            buffer_id,
            file: mark.file.clone(),
            char_index: mark.char_index,
            fallback: mark.fallback,
            affinity: match mark.affinity {
                SessionAnchorAffinity::Left => AnchorAffinity::Left,
                SessionAnchorAffinity::Right => AnchorAffinity::Right,
            },
        })
    }

    fn durable_session_snapshot(
        &mut self,
        include_disk_contents: bool,
    ) -> (SessionSnapshot, Vec<Option<SessionDiskFingerprint>>) {
        self.sync_to_window();
        let cwd = std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_default();
        let saved_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or_default();
        let mut visible_buffer_positions = HashMap::new();
        for window in self.window_manager.windows() {
            visible_buffer_positions.insert(
                window.buffer_index,
                (window.cx, window.vtop + window.cy, window.vtop),
            );
        }
        let (buffers, disk_fingerprints) = self
            .buffers
            .iter()
            .enumerate()
            .map(|(index, buffer)| {
                let (cursor_x, cursor_y, viewport_top) = visible_buffer_positions
                    .get(&index)
                    .copied()
                    .unwrap_or((buffer.pos.0, buffer.vtop + buffer.pos.1, buffer.vtop));
                let mut undo_history = buffer.undo_history.clone();
                undo_history.commit_transaction(CursorSnapshot::new(
                    cursor_x,
                    cursor_y,
                    viewport_top,
                ));
                let disk_fingerprint = buffer.file.as_deref().and_then(|path| {
                    capture_session_disk_fingerprint(Path::new(path))
                        .ok()
                        .flatten()
                });
                (
                    SessionBufferSnapshot {
                        index,
                        path: buffer.file.clone(),
                        // Periodic snapshots flatten the structurally shared Rope on the
                        // writer thread so large open buffers cannot stall input.
                        contents: if include_disk_contents {
                            buffer.contents()
                        } else {
                            String::new()
                        },
                        dirty: buffer.dirty,
                        revision: buffer.revision(),
                        cursor_x,
                        cursor_y,
                        viewport_top,
                        undo_history,
                        disk_contents: if include_disk_contents {
                            buffer.file.as_deref().zip(disk_fingerprint).and_then(
                                |(path, fingerprint)| {
                                    read_session_disk_contents(Path::new(path), fingerprint).ok()
                                },
                            )
                        } else {
                            None
                        },
                    },
                    disk_fingerprint,
                )
            })
            .unzip();
        let buffer_indices = self
            .buffers
            .iter()
            .enumerate()
            .map(|(index, buffer)| (buffer.id(), index))
            .collect::<HashMap<_, _>>();
        let local_marks = self
            .local_marks
            .values()
            .flat_map(|marks| marks.iter())
            .filter_map(|(name, anchor)| self.snapshot_mark(*name, anchor, &buffer_indices))
            .collect();
        let global_marks = self
            .global_marks
            .iter()
            .filter_map(|(name, anchor)| self.snapshot_mark(*name, anchor, &buffer_indices))
            .collect();
        let special_marks = self
            .special_marks
            .iter()
            .filter_map(|((_, name), anchor)| self.snapshot_mark(*name, anchor, &buffer_indices))
            .collect();
        let agent_workspace = self
            .agent_workspace
            .as_ref()
            .and_then(|workspace| workspace.lock().ok().map(|workspace| workspace.snapshot()));
        let agent_transcript = self
            .preferences
            .plugin_storage("agent", "transcript")
            .and_then(Value::as_str)
            .map(str::to_string);

        (
            SessionSnapshot {
                version: SESSION_SCHEMA_VERSION,
                generation: 0,
                cwd,
                saved_at_ms,
                buffers,
                current_buffer_index: self.current_buffer_index,
                window_layout: self.window_manager.snapshot(),
                registers: self.registers.clone(),
                jumps: self
                    .jump_list
                    .iter()
                    .map(|jump| SessionJump {
                        file: jump.file.clone(),
                        x: jump.x,
                        y: jump.y,
                    })
                    .collect(),
                jump_index: self.jump_index,
                local_marks,
                global_marks,
                special_marks,
                agent_transcript,
                agent_workspace,
                agent_session_resumable: false,
            },
            disk_fingerprints,
        )
    }

    fn snapshot_mark(
        &self,
        name: char,
        anchor: &EditAnchor,
        buffer_indices: &HashMap<BufferId, usize>,
    ) -> Option<SessionMark> {
        Some(SessionMark {
            name,
            buffer_index: *buffer_indices.get(&anchor.buffer_id)?,
            file: anchor.file.clone(),
            char_index: anchor.char_index,
            fallback: anchor.fallback,
            affinity: match anchor.affinity {
                AnchorAffinity::Left => SessionAnchorAffinity::Left,
                AnchorAffinity::Right => SessionAnchorAffinity::Right,
            },
        })
    }

    fn persist_session_snapshot(&mut self, force: bool) {
        let Some(store) = self.session_store.clone() else {
            return;
        };
        if let Some(writer) = self.session_snapshot_writer.take() {
            if force || writer.is_finished() {
                self.finish_session_snapshot(writer);
            } else {
                self.session_snapshot_writer = Some(writer);
                return;
            }
        }
        if !force && self.last_session_snapshot.elapsed() < SESSION_SNAPSHOT_INTERVAL {
            return;
        }
        self.last_session_snapshot = Instant::now();
        let snapshot_generation = (
            self.render_generation,
            self.agent_workspace.as_ref().and_then(|workspace| {
                workspace
                    .lock()
                    .ok()
                    .map(|workspace| workspace.generation())
            }),
        );
        if !force && self.last_session_snapshot_generation == Some(snapshot_generation) {
            return;
        }
        let _span = perf::PerfSpan::start("session:snapshot");
        let content_snapshots = self
            .buffers
            .iter()
            .map(Buffer::contents_snapshot)
            .collect::<Vec<_>>();
        let (mut snapshot, disk_fingerprints) =
            self.durable_session_snapshot(/*include_disk_contents*/ false);
        let writer = std::thread::spawn(move || {
            for ((buffer, fingerprint), contents) in snapshot
                .buffers
                .iter_mut()
                .zip(disk_fingerprints)
                .zip(content_snapshots)
            {
                buffer.contents = contents.to_string();
                buffer.disk_contents =
                    buffer
                        .path
                        .as_deref()
                        .zip(fingerprint)
                        .and_then(|(path, fingerprint)| {
                            read_session_disk_contents(Path::new(path), fingerprint).ok()
                        });
            }
            store.write(&mut snapshot)?;
            Ok(snapshot_generation)
        });
        if force {
            self.finish_session_snapshot(writer);
        } else {
            self.session_snapshot_writer = Some(writer);
        }
    }

    fn finish_session_snapshot(&mut self, writer: SessionSnapshotWriter) {
        match writer.join() {
            Ok(Ok(snapshot_generation)) => {
                self.last_session_snapshot_generation = Some(snapshot_generation);
            }
            Ok(Err(error)) => log!(
                "{}",
                json!({
                    "event": "session_snapshot_failed",
                    "level": "error",
                    "service": "red",
                    "stage": "write",
                    "error": error.to_string(),
                })
            ),
            Err(_) => log!(
                "{}",
                json!({
                    "event": "session_snapshot_failed",
                    "level": "error",
                    "service": "red",
                    "stage": "worker",
                    "error": "snapshot worker panicked",
                })
            ),
        }
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
                        let line = line.trim_end_matches('\n');
                        let line_len = grapheme_len(line);
                        if min_x <= line_len {
                            let start = self.grapheme_to_char_on_line(min_x, y);
                            let end = self.grapheme_to_char_on_line((max_x + 1).min(line_len), y);
                            text.push_str(char_slice(line, start, end));
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
                    let start = if y == y0 {
                        self.grapheme_to_char_on_line(x0, y)
                    } else {
                        0
                    };
                    let end = if y == y1 {
                        self.grapheme_to_char_on_line(x1 + 1, y)
                    } else {
                        line.trim_end_matches('\n').chars().count()
                    };
                    text.push_str(char_slice(&line, start, end));
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
        self.clamp_cursor_to_line();
        self.ensure_cursor_visible();
    }

    fn clamp_cursor_to_line(&mut self) {
        let line = self.buffer_line();
        let buffer = self.current_buffer();
        if buffer.line_range_byte_len(line, line.saturating_add(1)) > MAX_HIGHLIGHT_SLICE_BYTES {
            // The final grapheme in a truncated prefix can absorb combining
            // characters from the suffix. One extra complete grapheme proves
            // the cursor is in bounds without counting a multi-megabyte line.
            let prefix = buffer.line_prefix_contents(line, self.cx.saturating_add(4096));
            if grapheme_len(trim_line_ending(&prefix)) > self.cx.saturating_add(1) {
                return;
            }
        }

        let max_cursor_x = self.max_cursor_x_for_line_length(self.line_length());
        self.cx = self.cx.min(max_cursor_x);
    }

    fn ensure_cursor_visible(&mut self) {
        let width = self.active_content_width();
        if width == 0 {
            return;
        }

        let buffer_line = self.buffer_line();
        let line = self.current_line_contents().unwrap_or_default();
        let line = line.trim_end_matches('\n');
        let display_col = grapheme_to_column_with_tabs(line, self.cx, self.active_tab_width());

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
        let Some(file) = self.current_buffer().file.as_deref() else {
            return Ok(None);
        };
        let Some(capabilities) = self.lsp.server_capabilities_for_file(file) else {
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
            let position = self.cursor_lsp_position();
            let pending = PendingLspEdit {
                buffer_id: self.current_buffer().id(),
                revision: self.current_buffer().revision(),
                uri: uri.clone(),
            };
            let request_id = self
                .lsp
                .request_completion(&uri, position.line, position.character, trigger_character)
                .await?;
            if request_id > 0 {
                self.pending_lsp_edit_requests.insert(request_id, pending);
            }
        }

        Ok(())
    }

    async fn apply_completion(
        &mut self,
        item: &CompletionResponseItem,
        commit_character: Option<char>,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        if self
            .completion_snapshot
            .take()
            .is_some_and(|pending| !self.pending_lsp_edit_is_current(&pending))
        {
            self.last_error = Some("completion item is stale; buffer changed".to_string());
            return Ok(());
        }

        let contents = self.current_buffer().contents();
        let validation_edits = item
            .text_edit
            .iter()
            .chain(item.additional_text_edits.iter().flatten())
            .cloned()
            .collect::<Vec<_>>();
        if let Err(error) = crate::lsp::apply_text_edits(&contents, &validation_edits) {
            self.last_error = Some(format!("invalid LSP completion edit: {error}"));
            return Ok(());
        }
        let resume_insert_transaction = self.transaction_active();
        if resume_insert_transaction {
            self.commit_transaction(self.cursor_snapshot());
        }

        self.begin_transaction("apply completion");

        let mut edits = Vec::new();
        if let Some(text_edit) = &item.text_edit {
            edits.push(completion_edit_from_lsp(
                &contents,
                text_edit,
                item.insert_text_format.as_ref(),
                true,
            )?);
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
                edits.push(completion_edit_from_lsp(&contents, text_edit, None, false)?);
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
            self.execute_lsp_command(command, None).await?;
        }

        Ok(())
    }

    async fn execute_lsp_command(
        &mut self,
        command: &LspCommand,
        source: Option<&str>,
    ) -> anyhow::Result<()> {
        let params = json!({
            "command": command.command,
            "arguments": command.arguments.clone().unwrap_or_default(),
        });

        if let Some(source) = source {
            self.lsp
                .send_request_for_source(source, "workspace/executeCommand", params, false)
                .await?;
        } else if let Some(file) = self.current_buffer().file.clone() {
            self.lsp
                .send_request_for_file(&file, "workspace/executeCommand", params, false)
                .await?;
        } else {
            self.lsp
                .send_request("workspace/executeCommand", params, false)
                .await?;
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn apply_lsp_workspace_edit(
        &mut self,
        operations: &[LspWorkspaceEditOperation],
        expected_revisions: &[(String, u64)],
        command: Option<&LspCommand>,
        label: &str,
        response: Option<&LspServerRequest>,
        save_after_uri: Option<&str>,
        save_as: Option<&str>,
        save_previous_file: Option<String>,
        render_buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let touched_paths = operations
            .iter()
            .flat_map(|operation| match operation {
                LspWorkspaceEditOperation::Document { edit } => vec![edit.uri.as_str()],
                LspWorkspaceEditOperation::Create { uri, .. }
                | LspWorkspaceEditOperation::Delete { uri, .. } => vec![uri.as_str()],
                LspWorkspaceEditOperation::Rename {
                    old_uri, new_uri, ..
                } => {
                    vec![old_uri.as_str(), new_uri.as_str()]
                }
            })
            .map(lsp_normalized_file_path)
            .collect::<Result<HashSet<_>, _>>()?;
        let touched_bytes = self
            .buffers
            .iter()
            .filter(|buffer| {
                buffer
                    .uri()
                    .ok()
                    .flatten()
                    .and_then(|uri| lsp_normalized_file_path(&uri).ok())
                    .is_some_and(|path| touched_paths.contains(&path))
            })
            .try_fold(0usize, |total, buffer| total.checked_add(buffer.byte_len()));
        if touched_bytes.is_none_or(|bytes| bytes > MAX_WORKSPACE_EDIT_TOTAL_BYTES) {
            let reason = format!(
                "LSP workspace edit exceeds {MAX_WORKSPACE_EDIT_TOTAL_BYTES} bytes of open-buffer content"
            );
            self.last_error = Some(reason.clone());
            if let Some(response) = response {
                self.lsp
                    .respond_workspace_edit(response, false, Some(&reason))
                    .await?;
            } else {
                self.complete_failed_format_save(
                    save_after_uri,
                    save_as,
                    save_previous_file.clone(),
                    &reason,
                    runtime,
                )
                .await?;
            }
            return Ok(());
        }
        let open_documents = self
            .buffers
            .iter()
            .enumerate()
            .filter_map(|(index, buffer)| {
                let uri = buffer.uri().ok().flatten()?;
                let path = lsp_normalized_file_path(&uri).ok()?;
                if !touched_paths.contains(&path) {
                    return None;
                }
                let version = buffer
                    .file
                    .as_deref()
                    .and_then(|file| self.lsp.document_version(file));
                Some(OpenWorkspaceDocument {
                    index,
                    uri,
                    contents: buffer.contents(),
                    revision: buffer.revision(),
                    version,
                    dirty: buffer.is_dirty(),
                })
            })
            .collect();
        let workspace_root = if let Some(request) = response {
            self.lsp.workspace_root_for_request(request)
        } else {
            operations
                .iter()
                .map(|operation| match operation {
                    LspWorkspaceEditOperation::Document { edit } => &edit.uri,
                    LspWorkspaceEditOperation::Create { uri, .. }
                    | LspWorkspaceEditOperation::Delete { uri, .. } => uri,
                    LspWorkspaceEditOperation::Rename { old_uri, .. } => old_uri,
                })
                .next()
                .and_then(|uri| lsp_file_path(uri).ok())
                .and_then(|file| self.lsp.workspace_root_for_file(&file))
                .or_else(|| {
                    self.current_buffer()
                        .file
                        .as_deref()
                        .and_then(|file| self.lsp.workspace_root_for_file(file))
                })
        };
        if response.is_some() && workspace_root.is_none() {
            let reason =
                "LSP workspace edit cannot be applied because its originating server is unavailable"
                    .to_string();
            self.last_error = Some(reason.clone());
            if let Some(response) = response {
                self.lsp
                    .respond_workspace_edit(response, false, Some(&reason))
                    .await?;
            }
            return Ok(());
        }
        let prepared = match prepare_workspace_edit(
            operations,
            expected_revisions,
            open_documents,
            workspace_root.as_deref(),
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                let reason = format!("invalid LSP workspace edit: {error}");
                self.last_error = Some(reason.clone());
                if let Some(response) = response {
                    self.lsp
                        .respond_workspace_edit(response, false, Some(&reason))
                        .await?;
                } else {
                    self.complete_failed_format_save(
                        save_after_uri,
                        save_as,
                        save_previous_file.clone(),
                        &reason,
                        runtime,
                    )
                    .await?;
                }
                return Ok(());
            }
        };
        let buffer_paths = prepared
            .documents
            .iter()
            .map(|document| lsp_file_path(&document.uri))
            .collect::<Result<Vec<_>, _>>()?;
        if let Err(error) = apply_workspace_resource_operations(&prepared) {
            let reason = format!("LSP resource operation failed: {error}");
            self.last_error = Some(reason.clone());
            if let Some(response) = response {
                self.lsp
                    .respond_workspace_edit(response, false, Some(&reason))
                    .await?;
            } else {
                self.complete_failed_format_save(
                    save_after_uri,
                    save_as,
                    save_previous_file.clone(),
                    &reason,
                    runtime,
                )
                .await?;
            }
            return Ok(());
        }

        let original_index = self.current_buffer_index;
        let original_view = (self.cx, self.cy, self.vtop, self.vleft, self.skipcol);
        let mut changed = Vec::new();
        let mut renamed = Vec::new();
        let mut newly_opened = 0usize;
        for (document, file) in prepared.documents.into_iter().zip(buffer_paths) {
            let index = document.index.unwrap_or_else(|| {
                newly_opened += 1;
                self.buffers.push(Buffer::new(
                    Some(file.clone()),
                    document.original_contents.clone(),
                ));
                self.buffers.len() - 1
            });
            if let Some(original_uri) = &document.original_uri {
                if original_uri != &document.uri {
                    renamed.push((original_uri.clone(), file.clone(), index));
                }
            }
            self.buffers[index].file = Some(file);
            if !document.text_changed {
                continue;
            }

            self.select_buffer_for_lsp_edit(index);
            if self.transaction_active() {
                self.commit_transaction(self.cursor_snapshot());
            }
            let end = self
                .current_buffer()
                .char_idx_to_position(self.current_buffer().contents().chars().count());
            self.begin_transaction_with_origin(
                label,
                EditOrigin::Lsp {
                    server: "language server".to_string(),
                },
            );
            self.replace_range(
                TextRange::new(TextPosition::new(0, 0), end),
                &document.contents,
            );
            self.commit_transaction(self.cursor_snapshot());
            changed.push(index);
        }
        for (uri, file, index) in &renamed {
            if let Ok(file) = lsp_file_path(uri) {
                if let Err(error) = self.lsp.did_close(&file).await {
                    self.last_error =
                        Some(format!("failed to close renamed LSP document: {error}"));
                }
            }
            self.lsp_opened_documents.remove(uri);
            let new_uri = crate::lsp::file_uri(file).ok();
            if let Some(new_uri) = &new_uri {
                if let Some(diagnostics) = self.diagnostics.remove(uri) {
                    self.diagnostics.insert(new_uri.clone(), diagnostics);
                }
            }
            if let Err(error) = self
                .lsp
                .did_open(file, &self.buffers[*index].contents())
                .await
            {
                self.last_error = Some(format!("failed to open renamed LSP document: {error}"));
            } else if let Some(new_uri) = new_uri {
                self.lsp_opened_documents.insert(new_uri);
            }
        }
        for index in changed {
            self.select_buffer_for_lsp_edit(index);
            if let Err(error) = self.notify_change(runtime).await {
                self.last_error =
                    Some(format!("LSP edit applied but notification failed: {error}"));
            }
        }
        self.select_buffer_for_lsp_edit(original_index);
        (self.cx, self.cy, self.vtop, self.vleft, self.skipcol) = original_view;
        self.check_bounds();
        if let Some(command) = command {
            if let Err(error) = self
                .execute_lsp_command(
                    command,
                    response.and_then(|request| request.source.as_deref()),
                )
                .await
            {
                self.last_error = Some(format!("LSP edit applied but command failed: {error}"));
            }
        }
        if let Some(response) = response {
            self.lsp
                .respond_workspace_edit(response, true, None)
                .await?;
        }
        if let Some(uri) = save_after_uri {
            if let Some(buffer_id) = self.buffers.iter().find_map(|buffer| {
                (buffer.uri().ok().flatten().as_deref() == Some(uri)).then_some(buffer.id())
            }) {
                self.complete_lsp_format_save(
                    buffer_id,
                    uri,
                    save_as,
                    save_previous_file,
                    /*warning*/ None,
                    runtime,
                )
                .await?;
            } else {
                self.last_error =
                    Some("formatted buffer is no longer open; save cancelled".to_string());
            }
        } else if newly_opened > 0 && self.last_error.is_none() {
            self.last_error = Some(format!(
                "LSP edit opened {newly_opened} unsaved buffer{}",
                if newly_opened == 1 { "" } else { "s" }
            ));
        }
        self.render(render_buffer)?;
        Ok(())
    }

    fn select_buffer_for_lsp_edit(&mut self, index: usize) {
        let previous = self.current_buffer_index;
        self.buffers[previous].pos = (self.cx, self.cy);
        self.buffers[previous].vtop = self.vtop;
        self.current_buffer_index = index;
        (self.cx, self.cy) = self.buffers[index].pos;
        self.vtop = self.buffers[index].vtop;
        self.vleft = 0;
        self.skipcol = 0;
    }

    async fn complete_lsp_format_save(
        &mut self,
        buffer_id: BufferId,
        uri: &str,
        save_as: Option<&str>,
        previous_file: Option<String>,
        warning: Option<&str>,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let Some(index) = self.buffers.iter().position(|buffer| {
            buffer.id() == buffer_id
                && buffer
                    .uri()
                    .ok()
                    .flatten()
                    .as_deref()
                    .is_some_and(|candidate| candidate == uri)
        }) else {
            self.last_error =
                Some("formatted buffer is no longer open; save cancelled".to_string());
            return Ok(());
        };
        let original = self.current_buffer_index;
        let original_view = (self.cx, self.cy, self.vtop, self.vleft, self.skipcol);
        self.select_buffer_for_lsp_edit(index);
        let previous_uri = self.current_buffer().uri()?;
        let result = if let Some(save_as) = save_as {
            self.current_buffer_mut().save_as(save_as)
        } else {
            self.current_buffer_mut().save()
        };
        match result {
            Ok(message) => {
                self.last_error = Some(warning.unwrap_or(&message).to_string());
                self.sync_lsp_document_identity(previous_uri.as_deref(), index)
                    .await?;
                let file = self.current_buffer().file.clone();
                self.plugin_registry
                    .notify(
                        runtime,
                        "file:saved",
                        json!({ "file": file, "buffer_index": index }),
                    )
                    .await?;
            }
            Err(error) => {
                if save_as.is_some() {
                    self.restore_lsp_format_save_identity(buffer_id, uri, previous_file)
                        .await;
                }
                self.last_error = Some(error.to_string());
            }
        }
        self.select_buffer_for_lsp_edit(original);
        (self.cx, self.cy, self.vtop, self.vleft, self.skipcol) = original_view;
        self.check_bounds();
        Ok(())
    }

    async fn restore_lsp_format_save_identity(
        &mut self,
        buffer_id: BufferId,
        target_uri: &str,
        previous_file: Option<String>,
    ) {
        let Some(index) = self
            .buffers
            .iter()
            .position(|buffer| buffer.id() == buffer_id)
        else {
            return;
        };
        self.buffers[index].file = previous_file;
        if let Err(error) = self
            .sync_lsp_document_identity(Some(target_uri), index)
            .await
        {
            log!(
                "{}",
                json!({
                    "event": "lsp_format_on_save_restore_failed",
                    "level": "warn",
                    "service": "red",
                    "stage": "sync_document_identity",
                    "error": error.to_string(),
                })
            );
        }
    }

    async fn complete_failed_format_save(
        &mut self,
        uri: Option<&str>,
        save_as: Option<&str>,
        previous_file: Option<String>,
        reason: &str,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let Some(uri) = uri else {
            return Ok(());
        };
        let Some(buffer_id) = self.buffers.iter().find_map(|buffer| {
            (buffer.uri().ok().flatten().as_deref() == Some(uri)).then_some(buffer.id())
        }) else {
            return Ok(());
        };
        let warning = format!("format-on-save unavailable; saved unformatted: {reason}");
        log!(
            "{}",
            json!({
                "event": "lsp_format_on_save_fallback",
                "level": "warn",
                "service": "red",
                "stage": "apply",
                "error": reason,
            })
        );
        self.complete_lsp_format_save(
            buffer_id,
            uri,
            save_as,
            previous_file,
            Some(&warning),
            runtime,
        )
        .await
    }

    async fn request_format_on_save(
        &mut self,
        save_as: Option<String>,
    ) -> anyhow::Result<FormatOnSaveRequest> {
        let buffer_id = self.current_buffer().id();
        if self.pending_lsp_format_saves.keys().any(|request_id| {
            self.pending_lsp_edit_requests
                .get(request_id)
                .is_some_and(|pending| pending.buffer_id == buffer_id)
        }) {
            self.last_error = Some("format-on-save is already pending for this buffer".to_string());
            return Ok(FormatOnSaveRequest::Pending);
        }
        let previous_uri = self.current_buffer().uri()?;
        let previous_file = self.current_buffer().file.clone();
        let file = match save_as.as_deref() {
            Some(save_as) => {
                let file = expand_user_path(save_as)?.to_string_lossy().into_owned();
                let target = Path::new(&file).absolutize()?.to_path_buf();
                let already_open = self.buffers.iter().enumerate().any(|(index, buffer)| {
                    index != self.current_buffer_index
                        && buffer.file.as_deref().is_some_and(|file| {
                            Path::new(file)
                                .absolutize()
                                .is_ok_and(|candidate| candidate == target)
                        })
                });
                if already_open {
                    self.last_error = Some(format!(
                        "Save As cancelled; destination is already open in another buffer: {}",
                        target.display()
                    ));
                    return Ok(FormatOnSaveRequest::Cancelled);
                }
                self.current_buffer_mut().file = Some(file.clone());
                file
            }
            None => {
                let Some(file) = self.current_buffer().file.clone() else {
                    return Ok(FormatOnSaveRequest::Save { warning: None });
                };
                file
            }
        };
        let uri = match self.current_buffer().uri() {
            Ok(Some(uri)) => uri,
            Ok(None) => {
                if save_as.is_some() {
                    self.current_buffer_mut().file = previous_file;
                }
                return Ok(FormatOnSaveRequest::Save { warning: None });
            }
            Err(error) => {
                if save_as.is_some() {
                    self.current_buffer_mut().file = previous_file;
                }
                return Err(error);
            }
        };
        let pending = PendingLspEdit {
            buffer_id,
            revision: self.current_buffer().revision(),
            uri,
        };
        let open_result = if save_as.is_some() {
            self.sync_lsp_document_identity(previous_uri.as_deref(), self.current_buffer_index)
                .await
        } else {
            self.ensure_current_buffer_lsp_opened().await
        };
        if let Err(error) = open_result {
            if matches!(
                error.downcast_ref::<crate::lsp::LspError>(),
                Some(
                    crate::lsp::LspError::ProtocolError(_)
                        | crate::lsp::LspError::RequestTimeout(_)
                )
            ) {
                log!(
                    "{}",
                    json!({
                        "event": "lsp_format_on_save_fallback",
                        "level": "warn",
                        "service": "red",
                        "stage": if save_as.is_some() { "sync_document_identity" } else { "did_open" },
                        "error": error.to_string(),
                    })
                );
                return Ok(FormatOnSaveRequest::Save {
                    warning: Some(format!(
                        "format-on-save unavailable; saved unformatted: {error}"
                    )),
                });
            }
            if save_as.is_some() {
                self.restore_lsp_format_save_identity(buffer_id, &pending.uri, previous_file)
                    .await;
            }
            return Err(error);
        }
        if self.lsp.server_capabilities_for_file(&file).is_some()
            && !self.lsp.supports_document_formatting(&file)
        {
            return Ok(FormatOnSaveRequest::Save { warning: None });
        }
        let indentation = self.indentation();
        let request = self
            .lsp
            .format_document_with_options(&file, indentation.shift_width, true)
            .await;
        let request_id = match request {
            Ok(request_id) => request_id,
            Err(
                error @ (crate::lsp::LspError::ProtocolError(_)
                | crate::lsp::LspError::RequestTimeout(_)),
            ) => {
                let message = format!("format-on-save unavailable; saved unformatted: {error}");
                log!(
                    "{}",
                    json!({
                        "event": "lsp_format_on_save_fallback",
                        "level": "warn",
                        "service": "red",
                        "stage": "request",
                        "error": error.to_string(),
                    })
                );
                return Ok(FormatOnSaveRequest::Save {
                    warning: Some(message),
                });
            }
            Err(error) => {
                if save_as.is_some() {
                    self.restore_lsp_format_save_identity(buffer_id, &pending.uri, previous_file)
                        .await;
                }
                return Err(error.into());
            }
        };
        if request_id == 0 {
            return Ok(FormatOnSaveRequest::Save { warning: None });
        }
        self.pending_lsp_edit_requests.insert(request_id, pending);
        self.pending_lsp_format_saves.insert(
            request_id,
            PendingLspFormatSave {
                save_as,
                previous_file,
            },
        );
        Ok(FormatOnSaveRequest::Pending)
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
    contents: &str,
    text_edit: &LspTextEdit,
    insert_text_format: Option<&InsertTextFormat>,
    is_main: bool,
) -> anyhow::Result<CompletionEdit> {
    let (start, end) = text_edit_char_range(contents, &text_edit.range)?;
    Ok(completion_edit(
        TextRange::new(
            text_position_for_char_index(contents, start),
            text_position_for_char_index(contents, end),
        ),
        &text_edit.new_text,
        insert_text_format,
        is_main,
    ))
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
    id: String,
    parent_id: Option<String>,
) -> anyhow::Result<PluginDocumentSymbol> {
    let range = required_range(value.get("range"), "range")?;
    let selection_range = required_range(value.get("selectionRange"), "selectionRange")
        .unwrap_or_else(|_| range.clone());
    let kind = required_kind(value)?;

    Ok(PluginDocumentSymbol {
        id,
        parent_id,
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

fn utf16_to_grapheme(line: &str, utf16_offset: usize) -> usize {
    let mut utf16_units = 0;
    let mut chars = 0;
    for character in line.chars() {
        let next = utf16_units + character.len_utf16();
        if next > utf16_offset {
            break;
        }
        utf16_units = next;
        chars += 1;
    }
    char_to_grapheme(line, chars)
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

fn insert_at_grapheme_column(lines: &mut Vec<String>, y: usize, x: usize, text: &str) {
    while lines.len() <= y {
        lines.push(String::new());
    }
    while grapheme_len(&lines[y]) < x {
        lines[y].push(' ');
    }
    let byte = grapheme_to_byte(&lines[y], x);
    lines[y].insert_str(byte, text);
}

fn remove_grapheme_columns(lines: &mut [String], y: usize, min_x: usize, max_x: usize) {
    let Some(line) = lines.get_mut(y) else {
        return;
    };
    let line_len = grapheme_len(line);
    if min_x >= line_len {
        return;
    }
    let start = grapheme_to_byte(line, min_x);
    let end = grapheme_to_byte(line, (max_x + 1).min(line_len));
    line.replace_range(start..end, "");
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
#[serde(rename_all = "snake_case")]
pub struct EditorStateSnapshot {
    pub version: u32,
    pub cwd: String,
    #[serde(alias = "savedAt")]
    pub saved_at: u64,
    pub buffers: Vec<BufferStateSnapshot>,
    #[serde(alias = "currentBufferIndex")]
    pub current_buffer_index: usize,
    #[serde(alias = "windowLayout")]
    pub window_layout: WindowManagerSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BufferStateSnapshot {
    pub index: usize,
    pub path: String,
    pub dirty: bool,
    pub cursor: CursorStateSnapshot,
    #[serde(alias = "viewportTop")]
    pub viewport_top: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorStateSnapshot {
    pub x: usize,
    pub y: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
#[serde(rename_all = "snake_case")]
pub struct PluginDocumentSymbol {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub detail: Option<String>,
    pub kind: i32,
    pub kind_name: String,
    pub file: String,
    pub range: Range,
    pub selection_range: Range,
    pub depth: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct PluginLocation {
    pub file: String,
    pub range: Range,
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

fn directory_snapshot(path: &str, recursive: bool) -> Value {
    if !recursive {
        return directory_listing(path);
    }

    const MAX_WATCH_ENTRIES: usize = 50_000;
    let root = std::path::Path::new(path);
    let mut pending = vec![root.to_path_buf()];
    let mut entries = Vec::new();
    while let Some(directory) = pending.pop() {
        let Ok(read_dir) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in read_dir.flatten() {
            if entries.len() >= MAX_WATCH_ENTRIES {
                break;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let entry_path = entry.path();
            let relative = entry_path.strip_prefix(root).unwrap_or(&entry_path);
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| (duration.as_secs(), duration.subsec_nanos()));
            entries.push(json!({
                "path": relative.to_string_lossy(),
                "directory": metadata.is_dir(),
                "length": metadata.len(),
                "modified": modified,
            }));
            if metadata.is_dir() {
                pending.push(entry_path);
            }
        }
        if entries.len() >= MAX_WATCH_ENTRIES {
            break;
        }
    }
    entries.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));
    json!({ "path": path, "entries": entries, "recursive": true })
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
    pub fn test_selection(&self) -> Option<(usize, usize, usize, usize)> {
        self.selection
            .map(|selection| (selection.x0, selection.y0, selection.x1, selection.y1))
    }

    #[doc(hidden)]
    pub fn test_set_default_register(&mut self, content: Content) {
        self.set_default_register(content);
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
    pub async fn test_request_workspace_symbols(&mut self, query: &str) -> anyhow::Result<i64> {
        let Some(file) = self.current_buffer().file.clone() else {
            return Ok(0);
        };
        self.ensure_current_buffer_lsp_opened().await?;
        Ok(self.lsp.workspace_symbol_for_file(&file, query).await?)
    }

    #[doc(hidden)]
    pub async fn test_request_references(&mut self) -> anyhow::Result<i64> {
        let Some(file) = self.current_buffer().file.clone() else {
            return Ok(0);
        };
        let position = self.cursor_text_position();
        self.ensure_current_buffer_lsp_opened().await?;
        Ok(self
            .lsp
            .references(&file, position.character, position.line, true)
            .await?)
    }

    #[doc(hidden)]
    pub fn test_last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    #[doc(hidden)]
    pub fn test_set_agent_workspace(&mut self, workspace: Arc<Mutex<ProposalWorkspace>>) {
        self.agent_workspace = Some(workspace);
    }

    #[doc(hidden)]
    pub fn test_agent_proposals_payload(&mut self, session_id: &str) -> anyhow::Result<Value> {
        self.agent_proposals_payload(session_id)
    }

    #[doc(hidden)]
    pub fn test_agent_gutter_sign(&self, line: usize) -> Option<&str> {
        self.gutter_sign_manager
            .visible_sign(self.current_buffer_index, line)
            .map(|sign| sign.text.as_str())
    }

    #[doc(hidden)]
    pub async fn test_accept_agent_proposal(
        &mut self,
        session_id: &str,
        path: &Path,
        hunk_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let workspace = self
            .agent_workspace
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no proposal workspace is active"))?;
        self.sync_agent_visible_buffers(&workspace)?;
        let acceptance = {
            let workspace = workspace
                .lock()
                .map_err(|_| anyhow::anyhow!("proposal workspace lock is poisoned"))?;
            let (revision, contents) = self.agent_file_state(&workspace, path)?;
            if let Some(hunk_id) = hunk_id {
                workspace.stage_accept_hunk(session_id, path, hunk_id, revision, &contents)?
            } else {
                workspace.stage_accept_all(session_id, path, revision, &contents)?
            }
        };
        let mut render_buffer = RenderBuffer::new(
            self.size.0 as usize,
            self.size.1 as usize,
            &Style::default(),
        );
        let mut runtime = Runtime::new();
        self.apply_agent_disposition(acceptance, &mut render_buffer, &mut runtime)
            .await
    }

    #[doc(hidden)]
    pub fn test_reject_agent_proposal(
        &mut self,
        session_id: &str,
        path: &Path,
        hunk_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let workspace = self
            .agent_workspace
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no proposal workspace is active"))?;
        self.sync_agent_visible_buffers(&workspace)?;
        let mut workspace = workspace
            .lock()
            .map_err(|_| anyhow::anyhow!("proposal workspace lock is poisoned"))?;
        let (revision, contents) = self.agent_file_state(&workspace, path)?;
        if let Some(hunk_id) = hunk_id {
            workspace.reject_hunk(session_id, path, hunk_id, revision, &contents)
        } else {
            workspace.reject_all(session_id, path, revision, &contents)
        }
    }

    #[doc(hidden)]
    pub fn test_last_transaction_origin(&self) -> Option<&EditOrigin> {
        self.current_buffer()
            .undo_history
            .latest_transaction()
            .map(|transaction| &transaction.origin)
    }

    #[doc(hidden)]
    pub fn test_undo_tree(&self) -> Vec<crate::undo::UndoTreeEntry> {
        self.current_buffer().undo_history.undo_tree()
    }

    #[doc(hidden)]
    pub fn test_session_snapshot(&mut self) -> SessionSnapshot {
        self.durable_session_snapshot(/*include_disk_contents*/ true)
            .0
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
    pub fn test_set_gutter_signs(&mut self, namespace: &str, signs: Vec<plugin::GutterSign>) {
        self.gutter_sign_manager.set(namespace.to_string(), signs);
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
        self.resize_window_layout((width as usize, height as usize));
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

        self.process_editor_event(
            event,
            &mut render_buffer,
            &mut runtime,
            EventRenderMode::Immediate,
        )
        .await?;

        Ok(())
    }

    #[doc(hidden)]
    pub fn test_handle_event(&mut self, event: event::Event) -> anyhow::Result<Option<KeyAction>> {
        self.handle_event(&event)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::path::PathBuf;

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

    #[test]
    fn openai_key_readiness_respects_session_config_and_environment_precedence() {
        assert!(openai_api_key_ready(
            Some("session-key"),
            Some("   "),
            Some(OsStr::new("inherited-key")),
        ));
        assert!(!openai_api_key_ready(
            Some("   "),
            Some("configured-key"),
            Some(OsStr::new("inherited-key")),
        ));
        assert!(!openai_api_key_ready(
            None,
            Some("   "),
            Some(OsStr::new("inherited-key")),
        ));
        assert!(openai_api_key_ready(
            None,
            None,
            Some(OsStr::new("inherited-key")),
        ));
    }

    #[tokio::test]
    async fn masked_agent_key_prompt_never_records_the_pasted_secret() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_plugin_requests();
        let secret = "test-secret-that-must-not-enter-editor-history";
        let mut editor = test_editor(/*width*/ 80, /*height*/ 10);
        editor.macro_recording = Some(MacroRecording {
            register: 'q',
            events: Vec::new(),
        });
        editor.agent_secret_input_active = true;
        editor.current_dialog = Some(Box::new(InputPrompt::secret(
            &editor,
            "OpenAI API key (session only)",
            Action::SetAgentApiKey,
        )));
        let mut buffer =
            RenderBuffer::new(/*width*/ 80, /*height*/ 10, &Style::default());
        let mut runtime = Runtime::new();

        editor
            .process_editor_event(
                Event::Paste(secret.to_string()),
                &mut buffer,
                &mut runtime,
                EventRenderMode::Immediate,
            )
            .await
            .unwrap();
        assert!(!render_text_rows(&buffer).join("\n").contains(secret));
        editor
            .process_editor_event(
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                &mut buffer,
                &mut runtime,
                EventRenderMode::Immediate,
            )
            .await
            .unwrap();

        assert_eq!(editor.agent_session_api_key.as_deref(), Some(secret));
        assert!(!editor.agent_secret_input_active);
        assert!(editor
            .macro_recording
            .as_ref()
            .is_some_and(|recording| recording.events.is_empty()));
        assert!(!format!("{:?}", editor.actions).contains(secret));
        assert_eq!(
            Editor::action_cause(&Action::SetAgentApiKey(secret.to_string())),
            "SetAgentApiKey"
        );
        assert!(!render_text_rows(&buffer).join("\n").contains(secret));
    }

    #[tokio::test]
    async fn setting_an_openai_key_replaces_a_live_custom_agent_bridge() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct AbortFlag(Arc<AtomicBool>);
        impl Drop for AbortFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_plugin_requests();
        let mut editor = test_editor(/*width*/ 80, /*height*/ 10);
        editor.config.agent.adapter = Some("codex".to_string());
        editor.config.agent.command = Some("stale-custom-adapter".to_string());
        editor.config.agent.args = vec!["--stale".to_string()];
        editor.agent_active_sessions.insert("session-1".to_string());
        let aborted = Arc::new(AtomicBool::new(false));
        let (bridge, worker) = AcpBridge::channel(NonZeroUsize::new(2).unwrap());
        editor.agent_bridge = Some(bridge);
        editor.agent_task = Some(tokio::spawn({
            let aborted = Arc::clone(&aborted);
            async move {
                let _worker = worker;
                let _flag = AbortFlag(aborted);
                std::future::pending::<()>().await;
                #[allow(unreachable_code)]
                Ok(())
            }
        }));
        tokio::task::yield_now().await;
        let mut buffer =
            RenderBuffer::new(/*width*/ 80, /*height*/ 10, &Style::default());
        let mut runtime = Runtime::new();

        editor
            .execute(
                &Action::SetAgentApiKey("session-key".to_string()),
                &mut buffer,
                &mut runtime,
            )
            .await
            .unwrap();
        tokio::task::yield_now().await;

        assert_eq!(editor.config.agent.adapter.as_deref(), Some("openai"));
        assert!(editor.config.agent.command.is_none());
        assert!(editor.config.agent.args.is_empty());
        assert_eq!(editor.agent_session_api_key.as_deref(), Some("session-key"));
        assert!(editor.agent_bridge.is_none());
        assert!(editor.agent_task.is_none());
        assert!(editor.agent_active_sessions.is_empty());
        assert!(aborted.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn selecting_codex_replaces_a_live_custom_agent_bridge() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct AbortFlag(Arc<AtomicBool>);
        impl Drop for AbortFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let mut editor = test_editor(/*width*/ 80, /*height*/ 10);
        editor.config.agent.adapter = Some("openai".to_string());
        editor.config.agent.command = Some("stale-custom-adapter".to_string());
        editor.config.agent.args = vec!["--stale".to_string()];
        editor
            .config
            .agent
            .env
            .insert("OPENAI_API_KEY".to_string(), "stale-config-key".to_string());
        editor.agent_session_api_key = Some("stale-session-key".to_string());
        editor.agent_active_sessions.insert("session-1".to_string());
        let aborted = Arc::new(AtomicBool::new(false));
        let (bridge, worker) = AcpBridge::channel(NonZeroUsize::new(2).unwrap());
        editor.agent_bridge = Some(bridge);
        editor.agent_task = Some(tokio::spawn({
            let aborted = Arc::clone(&aborted);
            async move {
                let _worker = worker;
                let _flag = AbortFlag(aborted);
                std::future::pending::<()>().await;
                #[allow(unreachable_code)]
                Ok(())
            }
        }));
        tokio::task::yield_now().await;

        editor.select_codex_agent_backend();
        tokio::task::yield_now().await;

        assert_eq!(editor.config.agent.adapter.as_deref(), Some("codex"));
        assert!(editor.config.agent.command.is_none());
        assert!(editor.config.agent.args.is_empty());
        assert!(!editor.config.agent.env.contains_key("OPENAI_API_KEY"));
        assert!(editor.agent_session_api_key.is_none());
        assert!(editor.agent_bridge.is_none());
        assert!(editor.agent_task.is_none());
        assert!(editor.agent_active_sessions.is_empty());
        assert!(aborted.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn agent_composer_never_uses_picker_history_or_records_prompt_input() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_plugin_requests();
        let mut editor = test_editor(/*width*/ 80, /*height*/ 14);
        editor.macro_recording = Some(MacroRecording {
            register: 'q',
            events: Vec::new(),
        });
        editor
            .preferences
            .record_picker_query("picker:802", "legacy agent prompt")
            .unwrap();
        let mut buffer =
            RenderBuffer::new(/*width*/ 80, /*height*/ 14, &Style::default());
        let mut runtime = Runtime::new();
        ACTION_DISPATCHER.send_request(PluginRequest::OpenAgentComposer {
            owner: "agent".to_string(),
            title: Some("Agent prompt".to_string()),
            id: 802,
            query: "inspect the workspace".to_string(),
            history: vec!["previous prompt".to_string()],
        });

        editor
            .service_background(&mut buffer, &mut runtime)
            .await
            .unwrap();

        let dialog = editor.current_dialog.as_ref().expect("agent composer");
        assert_eq!(dialog.picker_id(), None);
        assert!(dialog.is_sensitive_input());
        assert!(editor.picker_history("picker:802").is_empty());

        editor
            .process_editor_event(
                Event::Paste("\ninclude all unsaved changes".to_string()),
                &mut buffer,
                &mut runtime,
                EventRenderMode::Immediate,
            )
            .await
            .unwrap();
        editor
            .process_editor_event(
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                &mut buffer,
                &mut runtime,
                EventRenderMode::Immediate,
            )
            .await
            .unwrap();

        assert!(editor.current_dialog.is_none());
        assert!(editor
            .macro_recording
            .as_ref()
            .is_some_and(|recording| recording.events.is_empty()));
        assert!(editor.last_semantic_change.is_none());
        assert!(editor.picker_history("picker:802").is_empty());
        assert!(!format!("{:?}", editor.actions).contains("include all unsaved changes"));
        assert_eq!(
            Editor::action_cause(&Action::NotifyPlugin(
                "agent".to_string(),
                "composer:submitted:802".to_string(),
                json!("include all unsaved changes"),
            )),
            "AgentComposer"
        );
    }

    #[tokio::test]
    async fn opening_a_custom_agent_composer_preserves_unrelated_picker_history() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_plugin_requests();
        let mut editor = test_editor(/*width*/ 80, /*height*/ 14);
        editor
            .preferences
            .record_picker_query("picker:301", "an unrelated picker query")
            .unwrap();
        let mut buffer =
            RenderBuffer::new(/*width*/ 80, /*height*/ 14, &Style::default());
        let mut runtime = Runtime::new();
        ACTION_DISPATCHER.send_request(PluginRequest::OpenAgentComposer {
            owner: "custom".to_string(),
            title: Some("Custom prompt".to_string()),
            id: 301,
            query: String::new(),
            history: Vec::new(),
        });

        editor
            .service_background(&mut buffer, &mut runtime)
            .await
            .unwrap();

        assert_eq!(
            editor.picker_history("picker:301"),
            ["an unrelated picker query"]
        );
    }

    async fn enter_colon_command(
        editor: &mut Editor,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
        command: &str,
    ) {
        editor.mode = Mode::Command;
        editor.command = command.to_string();
        editor
            .process_editor_event(
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                buffer,
                runtime,
                EventRenderMode::Immediate,
            )
            .await
            .unwrap();
        editor.service_background(buffer, runtime).await.unwrap();
    }

    #[tokio::test]
    async fn colon_commands_dispatch_registered_plugins_and_preserve_builtin_precedence() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_plugin_requests();
        let mut editor = test_editor(/*width*/ 80, /*height*/ 10);
        let mut buffer =
            RenderBuffer::new(/*width*/ 80, /*height*/ 10, &Style::default());
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "colon_commands",
                r#"
                    pub fn activate() {
                        red::add_command("ExampleCommand", example);
                        red::add_command("wrap", shadow_builtin);
                    }

                    fn example() {
                        red::execute("Print", "hello from colon plugin");
                    }

                    fn shadow_builtin() {
                        red::execute("Print", "plugin shadowed built-in");
                    }
                "#,
            )
            .await
            .unwrap();

        enter_colon_command(&mut editor, &mut buffer, &mut runtime, "ExampleCommand").await;

        assert_eq!(
            editor.last_error.as_deref(),
            Some("hello from colon plugin")
        );
        assert!(render_text_rows(&buffer)[9].contains("hello from colon plugin"));

        editor.wrap = false;
        enter_colon_command(&mut editor, &mut buffer, &mut runtime, "wrap").await;

        assert!(editor.wrap);
        assert_ne!(
            editor.last_error.as_deref(),
            Some("plugin shadowed built-in")
        );
    }

    #[tokio::test]
    async fn unknown_colon_command_leaves_a_visible_error() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_plugin_requests();
        let mut editor = test_editor(/*width*/ 80, /*height*/ 10);
        let mut buffer =
            RenderBuffer::new(/*width*/ 80, /*height*/ 10, &Style::default());
        let mut runtime = Runtime::new();

        enter_colon_command(
            &mut editor,
            &mut buffer,
            &mut runtime,
            "DefinitelyNotACommand",
        )
        .await;

        assert_eq!(
            editor.last_error.as_deref(),
            Some("unknown command \"DefinitelyNotACommand\"")
        );
        assert!(render_text_rows(&buffer)[9].contains("unknown command \"DefinitelyNotACommand\""));
    }

    #[tokio::test]
    async fn plugin_buffer_text_requests_preserve_full_and_ranged_contents() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        for (contents, range, expected) in [
            ("", None, ""),
            ("a", None, "a"),
            ("a\n", None, "a\n"),
            ("a\nb", None, "a\nb"),
            ("α\r\nβ\n終", None, "α\r\nβ\n終"),
            ("zero\r\none\ntwo", Some((1, 3)), "one\ntwo"),
        ] {
            drain_plugin_requests();
            let config = Config::default();
            let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
            let mut editor = Editor::with_size(
                lsp,
                40,
                8,
                config,
                Theme::default(),
                vec![Buffer::new(
                    Some("test.txt".to_string()),
                    contents.to_string(),
                )],
            )
            .unwrap();
            editor.test_disable_terminal_output();
            let mut buffer = RenderBuffer::new(40, 8, &Style::default());
            let mut runtime = Runtime::new();
            let request = range.map_or_else(
                || "red::request(\"GetBufferText\", loaded);".to_string(),
                |(start, end)| format!("red::request(\"GetBufferText\", loaded, {start}, {end});"),
            );
            runtime
                .load_plugin(
                    "buffer_reader",
                    &format!(
                        r#"
                            pub fn activate() {{
                                red::add_command("Read", read);
                            }}

                            fn loaded(event: Json) {{
                                red::execute("Print", red::string(event.text, "missing"));
                            }}

                            fn read() {{
                                {request}
                            }}
                        "#
                    ),
                )
                .await
                .unwrap();
            runtime.execute_command("Read").await.unwrap();

            editor
                .service_background(&mut buffer, &mut runtime)
                .await
                .unwrap();

            assert_eq!(editor.last_error.as_deref(), Some(expected));
        }
    }

    #[test]
    fn detached_styled_rows_resolve_missing_and_translucent_colors() {
        let theme_style = Style {
            fg: Some(Color::Rgb {
                r: 205,
                g: 214,
                b: 244,
            }),
            bg: Some(Color::Rgb {
                r: 30,
                g: 30,
                b: 46,
            }),
            ..Style::default()
        };
        let mut buffer = RenderBuffer::new(4, 1, &Style::default());
        buffer.set_text(
            0,
            0,
            "fn",
            &Style {
                fg: Some(Color::Rgb {
                    r: 203,
                    g: 166,
                    b: 247,
                }),
                ..Style::default()
            },
        );
        buffer.set_text(
            2,
            0,
            "()",
            &Style {
                bg: Some(Color::Rgba {
                    r: 255,
                    g: 0,
                    b: 0,
                    a: 128,
                }),
                ..Style::default()
            },
        );

        let rows = render_styled_rows(&buffer, &theme_style);

        assert_eq!(rows[0].len(), 2);
        assert_eq!(
            rows[0][0].style.bg,
            Some(Color::Rgb {
                r: 30,
                g: 30,
                b: 46,
            })
        );
        assert_eq!(
            rows[0][1].style.fg,
            Some(Color::Rgb {
                r: 205,
                g: 214,
                b: 244,
            })
        );
        assert_eq!(
            rows[0][1].style.bg,
            Some(Color::Rgb {
                r: 142,
                g: 14,
                b: 22,
            })
        );
    }

    #[test]
    fn detached_rows_skip_wide_grapheme_padding_and_fit_the_terminal() {
        let mut buffer = RenderBuffer::new(10, 1, &Style::default());
        buffer.set_text(0, 0, "a👋漢👩‍💻b", &Style::default());

        let text_rows = render_text_rows(&buffer);
        let styled_rows = render_styled_rows(&buffer, &Style::default());
        let styled_text = styled_rows[0]
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();

        assert_eq!(text_rows[0], "a👋漢👩‍💻b  ");
        assert_eq!(styled_text, text_rows[0]);
        assert_eq!(display_width(&text_rows[0]), 10);
    }

    #[tokio::test]
    async fn production_resize_event_accepts_zero_and_tiny_dimensions() {
        let mut editor = test_editor(80, 24);
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());
        let mut runtime = Runtime::new();

        for (width, height) in [(0, 0), (1, 1), (1, 2), (2, 1), (80, 24)] {
            editor
                .process_editor_event(
                    Event::Resize(width, height),
                    &mut buffer,
                    &mut runtime,
                    EventRenderMode::Immediate,
                )
                .await
                .unwrap();

            assert_eq!(editor.size, (width, height));
            assert_eq!(buffer.width, width as usize);
            assert_eq!(buffer.height, height as usize);
            assert_eq!(buffer.cells.len(), width as usize * height as usize);
        }
    }

    #[test]
    fn detached_mouse_input_preserves_the_terminal_mouse_event() {
        let mouse = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 17,
            row: 6,
            modifiers: KeyModifiers::CONTROL,
        };

        assert_eq!(
            detached_input_to_crossterm(crate::headless::InputEvent::Mouse { event: mouse }),
            Event::Mouse(mouse)
        );
    }

    #[tokio::test]
    async fn detached_agent_composer_wraps_paste_and_keeps_the_cursor_in_bounds_after_resize() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_plugin_requests();
        let mut core = DetachedEditorCore::new(test_editor(/*width*/ 80, /*height*/ 24))
            .await
            .unwrap();
        ACTION_DISPATCHER.send_request(PluginRequest::OpenAgentComposer {
            owner: "agent".to_string(),
            title: Some("Agent prompt".to_string()),
            id: 802,
            query: format!("inspect {}", "x".repeat(160)),
            history: Vec::new(),
        });

        let opened = core.tick().await.unwrap().expect("composer render");
        assert!(opened.cursor.0 < 80);
        assert!(opened.cursor.1 < 24);

        let resized = core.resize(/*columns*/ 48, /*rows*/ 14).await.unwrap();
        assert!(resized.cursor.0 < 48);
        assert!(resized.cursor.1 < 14);

        let pasted = core
            .input(crate::headless::InputEvent::Paste {
                text: "\r\ninclude 漢字 and 👨‍👩‍👧\r\nTAIL".to_string(),
            })
            .await
            .unwrap();
        assert!(pasted.cursor.0 < 48);
        assert!(pasted.cursor.1 < 14);
        assert!(pasted.lines.iter().any(|line| line.text.contains("TAIL")));
        assert!(core.editor.picker_history("picker:802").is_empty());
    }

    #[tokio::test]
    async fn detached_agent_proposal_acceptance_survives_a_failed_review_refresh_render() {
        struct FailingDialog;

        impl crate::ui::Component for FailingDialog {
            fn draw(&self, _buffer: &mut RenderBuffer) -> anyhow::Result<()> {
                anyhow::bail!("injected render failure")
            }
        }

        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_plugin_requests();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("proposal.txt");
        std::fs::write(&path, "disk base\n").unwrap();
        let mut workspace = ProposalWorkspace::new(root.path()).unwrap();
        workspace.begin_turn("session-1", "turn-1".to_string());
        workspace
            .write("session-1", &path, "agent replacement\n".to_string())
            .unwrap();
        let workspace = Arc::new(Mutex::new(workspace));

        let mut config = Config::from_user_toml_with_overrides("", &[]).unwrap();
        config.plugins.retain(|name, _| name == "agent");
        config.lsp.enabled = false;
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let store = SessionStore::new(root.path().join("session"));
        let mut editor = Editor::with_size(
            lsp,
            /*width*/ 100,
            /*height*/ 24,
            config,
            Theme::default(),
            vec![Buffer::new(
                Some(path.to_string_lossy().into_owned()),
                "disk base\n".to_string(),
            )],
        )
        .unwrap();
        editor.test_set_agent_workspace(Arc::clone(&workspace));
        editor.set_session_store(store.clone());
        let mut core = DetachedEditorCore::new(editor).await.unwrap();

        core.editor
            .plugin_registry
            .notify(
                &mut core.runtime,
                "agent:session_created",
                json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        core.tick().await.unwrap();
        core.editor
            .plugin_registry
            .execute(&mut core.runtime, "AgentReview")
            .await
            .unwrap();
        core.tick().await.unwrap();
        core.editor.current_dialog = Some(Box::new(FailingDialog));
        core.editor.last_session_snapshot = Instant::now() - SESSION_SNAPSHOT_INTERVAL;
        ACTION_DISPATCHER.send_request(PluginRequest::AgentAcceptProposal {
            session_id: "session-1".to_string(),
            path: path.clone(),
            hunk_id: None,
        });

        core.tick()
            .await
            .expect("a post-accept review render failure must not terminate the detached editor");
        core.editor.test_finish_session_snapshot();

        assert_eq!(
            core.editor.current_buffer().contents(),
            "agent replacement\n"
        );
        assert!(core.editor.current_buffer().is_dirty());
        assert!(workspace
            .lock()
            .unwrap()
            .pending_files("session-1")
            .is_empty());
        assert!(core.editor.last_error.as_deref().is_some_and(|error| {
            error.contains("Agent proposal applied, but render failed")
                && error.contains("injected render failure")
        }));
        let snapshot = store.load().unwrap();
        assert_eq!(snapshot.buffers[0].contents, "agent replacement\n");
        assert!(snapshot.buffers[0].dirty);
        assert!(
            ProposalWorkspace::from_snapshot(snapshot.agent_workspace.unwrap())
                .pending_files("session-1")
                .is_empty()
        );

        ACTION_DISPATCHER.send_request(PluginRequest::OpenWorkspace {
            id: "normal-render".to_string(),
            config: crate::plugin::WorkspaceConfig::default(),
        });
        let error = core.tick().await.unwrap_err();
        assert!(error.to_string().contains("injected render failure"));
        drain_plugin_requests();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detached_agent_proposal_actions_keep_unsafe_pending_changes_recoverable() {
        use nix::{sys::stat::Mode as UnixMode, unistd::mkfifo};

        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        for source in ["symlink", "fifo", "oversized"] {
            drain_plugin_requests();
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("proposal.txt");
            let safe_path = root.path().join("safe.txt");
            std::fs::write(&path, "disk base\n").unwrap();
            std::fs::write(&safe_path, "safe disk base\n").unwrap();
            let mut workspace = ProposalWorkspace::new(root.path()).unwrap();
            workspace.begin_turn("session-1", "turn-1".to_string());
            workspace
                .write("session-1", &path, "agent replacement\n".to_string())
                .unwrap();
            workspace
                .write(
                    "session-1",
                    &safe_path,
                    "safe agent replacement\n".to_string(),
                )
                .unwrap();
            let workspace = Arc::new(Mutex::new(workspace));

            let mut config = Config::from_user_toml_with_overrides("", &[]).unwrap();
            config.plugins.retain(|name, _| name == "agent");
            config.lsp.enabled = false;
            let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
            let mut editor = Editor::with_size(
                lsp,
                /*width*/ 100,
                /*height*/ 24,
                config,
                Theme::default(),
                vec![Buffer::new(None, "scratch".to_string())],
            )
            .unwrap();
            editor.test_set_agent_workspace(Arc::clone(&workspace));
            let mut core = DetachedEditorCore::new(editor).await.unwrap();

            ACTION_DISPATCHER.send_request(PluginRequest::AgentCloseSession {
                session_id: "session-1".to_string(),
            });
            core.input(crate::headless::InputEvent::Key {
                code: crate::headless::KeyCode::Character('h'),
                modifiers: Vec::new(),
            })
            .await
            .expect("closing an ACP session should archive its pending proposals");
            {
                let workspace = workspace.lock().unwrap();
                assert_eq!(workspace.review_sessions(""), ["session-1"]);
                assert_eq!(
                    workspace.pending_files("session-1"),
                    [path.clone(), safe_path.clone()]
                );
            }

            std::fs::remove_file(&path).unwrap();
            match source {
                "symlink" => {
                    let outside = root.path().join("outside.txt");
                    std::fs::write(&outside, "outside secret\n").unwrap();
                    std::os::unix::fs::symlink(outside, &path).unwrap();
                }
                "fifo" => mkfifo(&path, UnixMode::S_IRUSR | UnixMode::S_IWUSR).unwrap(),
                "oversized" => {
                    std::fs::write(&path, "x".repeat(crate::acp::MAX_MESSAGE_BYTES)).unwrap();
                }
                _ => unreachable!(),
            }

            for character in ":AgentReview".chars() {
                core.input(crate::headless::InputEvent::Key {
                    code: crate::headless::KeyCode::Character(character),
                    modifiers: Vec::new(),
                })
                .await
                .expect("production command input should keep the detached editor alive");
            }
            let reviewed = core
                .input(crate::headless::InputEvent::Key {
                    code: crate::headless::KeyCode::Enter,
                    modifiers: Vec::new(),
                })
                .await
                .expect("unsafe proposal review should not terminate the detached editor");
            assert!(reviewed
                .lines
                .iter()
                .any(|line| { line.text.contains("Unable to review this agent proposal") }));
            let proposals = core.editor.agent_proposals_payload("").unwrap();
            assert!(proposals["files"].as_array().unwrap().iter().any(|file| {
                file["path"] == safe_path.to_string_lossy().as_ref()
                    && !file["hunks"].as_array().unwrap().is_empty()
            }));
            assert!(!reviewed
                .lines
                .iter()
                .any(|line| line.text.contains("outside secret")));
            assert!(!core.stopped);
            core.input(crate::headless::InputEvent::Key {
                code: crate::headless::KeyCode::Character('q'),
                modifiers: Vec::new(),
            })
            .await
            .expect("proposal review should remain dismissible");

            for (request, message) in [
                (
                    PluginRequest::AgentAcceptProposal {
                        session_id: "session-1".to_string(),
                        path: path.clone(),
                        hunk_id: None,
                    },
                    "Unable to accept agent proposal safely",
                ),
                (
                    PluginRequest::AgentRejectProposal {
                        session_id: "session-1".to_string(),
                        path: path.clone(),
                        hunk_id: None,
                    },
                    "Unable to reject agent proposal safely",
                ),
            ] {
                ACTION_DISPATCHER.send_request(request);
                let delta = core
                    .input(crate::headless::InputEvent::Key {
                        code: crate::headless::KeyCode::Character('h'),
                        modifiers: Vec::new(),
                    })
                    .await
                    .expect("unsafe proposal action should not terminate the detached editor");
                assert!(delta.lines.iter().any(|line| line.text.contains(message)));
                assert!(!delta
                    .lines
                    .iter()
                    .any(|line| line.text.contains("outside secret")));
                assert!(!core.stopped);
            }
            assert_eq!(
                workspace.lock().unwrap().pending_files("session-1"),
                [path, safe_path]
            );
            assert_eq!(core.editor.current_buffer().contents(), "scratch");
        }
        drain_plugin_requests();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detached_agent_prompt_recovers_after_a_live_adapter_is_lost() {
        use std::{
            os::unix::fs::PermissionsExt,
            sync::atomic::{AtomicBool, Ordering},
        };

        struct AbortFlag(Arc<AtomicBool>);
        impl Drop for AbortFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        for loss in ["finished", "missing"] {
            drain_plugin_requests();
            let cwd = std::env::current_dir().unwrap();
            let root = tempfile::Builder::new()
                .prefix(".red-acp-restart-")
                .tempdir_in(&cwd)
                .unwrap();
            let path = root.path().join("proposal.txt");
            let gate = root.path().join("adapter-ready");
            let prompt_file = root.path().join("prompt.txt");
            let adapter = root.path().join("restart-acp.py");
            std::fs::write(&path, "disk base\n").unwrap();
            std::fs::write(
                &adapter,
                r#"#!/usr/bin/env python3
import json
import os
import sys
import time

gate = os.environ["RED_TEST_ACP_GATE"]
prompt_file = os.environ["RED_TEST_ACP_PROMPT"]
for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialize":
        while not os.path.exists(gate):
            time.sleep(0.005)
        result = {"protocolVersion": 1, "agentCapabilities": {}}
    elif method == "session/new":
        result = {"sessionId": "replacement-session"}
    elif method == "session/prompt":
        prompt = request["params"]["prompt"]
        with open(prompt_file + ".tmp", "w", encoding="utf-8") as output:
            output.write("".join(block.get("text", "") for block in prompt))
        os.replace(prompt_file + ".tmp", prompt_file)
        result = {"stopReason": "end_turn"}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
"#,
            )
            .unwrap();
            std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o755)).unwrap();

            let mut workspace = ProposalWorkspace::new(&cwd).unwrap();
            workspace.begin_turn("session-1", "turn-1".to_string());
            workspace
                .write("session-1", &path, "agent replacement\n".to_string())
                .unwrap();
            let workspace = Arc::new(Mutex::new(workspace));

            let mut config = Config::from_user_toml_with_overrides("", &[]).unwrap();
            config.plugins.retain(|name, _| name == "agent");
            config.lsp.enabled = false;
            config.agent.command = Some(adapter.to_string_lossy().into_owned());
            config.agent.env.insert(
                "RED_TEST_ACP_GATE".to_string(),
                gate.to_string_lossy().into_owned(),
            );
            config.agent.env.insert(
                "RED_TEST_ACP_PROMPT".to_string(),
                prompt_file.to_string_lossy().into_owned(),
            );
            let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
            let mut editor = Editor::with_size(
                lsp,
                /*width*/ 100,
                /*height*/ 24,
                config,
                Theme::default(),
                vec![Buffer::new(None, "scratch".to_string())],
            )
            .unwrap();
            editor.test_set_agent_workspace(Arc::clone(&workspace));
            let mut core = DetachedEditorCore::new(editor).await.unwrap();
            core.editor
                .plugin_registry
                .notify(
                    &mut core.runtime,
                    "agent:session_created",
                    json!({ "session_id": "session-1" }),
                )
                .await
                .unwrap();
            core.tick().await.unwrap();

            let aborted = Arc::new(AtomicBool::new(false));
            let (bridge, worker) = AcpBridge::channel(NonZeroUsize::new(2).unwrap());
            if loss == "finished" {
                core.editor.agent_bridge = Some(bridge);
                core.editor.agent_task = Some(tokio::spawn(async move {
                    drop(worker);
                    anyhow::bail!("fixture adapter exited")
                }));
                while !core
                    .editor
                    .agent_task
                    .as_ref()
                    .is_some_and(tokio::task::JoinHandle::is_finished)
                {
                    tokio::task::yield_now().await;
                }
            } else {
                drop(bridge);
                core.editor.agent_task = Some(tokio::spawn({
                    let aborted = Arc::clone(&aborted);
                    async move {
                        let _worker = worker;
                        let _flag = AbortFlag(aborted);
                        std::future::pending::<()>().await;
                        #[allow(unreachable_code)]
                        Ok(())
                    }
                }));
                tokio::task::yield_now().await;
            }

            let generation = workspace.lock().unwrap().generation();
            ACTION_DISPATCHER.send_request(PluginRequest::AgentPrompt {
                session_id: "session-1".to_string(),
                text: "retry this exact prompt\nincluding line two".to_string(),
            });
            core.input(crate::headless::InputEvent::Key {
                code: crate::headless::KeyCode::Character('h'),
                modifiers: Vec::new(),
            })
            .await
            .expect("a lost ACP adapter must not terminate the detached editor");
            tokio::task::yield_now().await;

            {
                let workspace = workspace.lock().unwrap();
                assert_eq!(workspace.generation(), generation + 1);
                assert_eq!(workspace.review_sessions(""), ["session-1"]);
                assert_eq!(
                    workspace.pending_files("session-1"),
                    std::slice::from_ref(&path)
                );
            }
            assert_eq!(aborted.load(Ordering::SeqCst), loss == "missing");
            assert!(core.editor.agent_bridge.is_some());
            assert!(core.editor.agent_task.is_some());

            std::fs::write(&gate, "ready").unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while !prompt_file.exists() {
                    core.tick().await.unwrap();
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("the replacement ACP adapter should receive the saved prompt");
            assert_eq!(
                std::fs::read_to_string(&prompt_file).unwrap(),
                "retry this exact prompt\nincluding line two"
            );
            assert!(!core.stopped);

            drop(core.editor.agent_bridge.take());
            if let Some(task) = core.editor.agent_task.take() {
                task.abort();
            }
        }
        drain_plugin_requests();
    }

    #[tokio::test]
    async fn detached_input_processes_only_a_bounded_batch_of_acp_events() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_plugin_requests();
        let mut core = DetachedEditorCore::new(test_editor(/*width*/ 80, /*height*/ 24))
            .await
            .unwrap();
        let capacity = NonZeroUsize::new(ACP_EVENTS_PER_TICK + 1).unwrap();
        let (bridge, worker) = AcpBridge::channel(capacity);
        for index in 0..=ACP_EVENTS_PER_TICK {
            worker
                .send(BridgeEvent::Update {
                    session_id: agent_client_protocol_schema::v1::SessionId::new("session-1"),
                    text: index.to_string(),
                })
                .await
                .unwrap();
        }
        core.editor.agent_bridge = Some(bridge);

        core.input(crate::headless::InputEvent::Key {
            code: crate::headless::KeyCode::Character('l'),
            modifiers: Vec::new(),
        })
        .await
        .unwrap();

        assert!(matches!(
            core.editor.agent_bridge.as_mut().and_then(AcpBridge::try_recv),
            Some(BridgeEvent::Update { text, .. }) if text == ACP_EVENTS_PER_TICK.to_string()
        ));
        drain_plugin_requests();
    }

    #[tokio::test]
    async fn finished_agent_task_drains_all_buffered_events_before_session_loss() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_plugin_requests();
        let plugin_root = tempfile::tempdir().unwrap();
        let plugin = plugin_root.path().join("acp-event-recorder.hk");
        std::fs::write(
            &plugin,
            r#"
                pub fn activate() {
                    red::state_set("trace", "");
                    red::on("agent:update", update);
                    red::on("agent:completed", completed);
                    red::on("agent:session_lost", lost);
                }

                fn update(event: Json) {
                    red::state_set("trace", red::string(red::state("trace"), "") + red::string(event.text, "") + ",");
                }

                fn completed(event: Json) {
                    red::state_set("trace", red::string(red::state("trace"), "") + "completed,");
                }

                fn lost(event: Json) {
                    red::execute("Print", red::string(red::state("trace"), "") + "lost");
                }
            "#,
        )
        .unwrap();
        let mut editor = test_editor(/*width*/ 80, /*height*/ 24);
        let mut runtime = Runtime::new();
        editor
            .plugin_registry
            .add("acp_event_recorder", plugin.to_string_lossy().as_ref());
        editor
            .plugin_registry
            .initialize(&mut runtime)
            .await
            .unwrap();
        let event_count = ACP_EVENTS_PER_TICK + 8;
        let capacity = NonZeroUsize::new(event_count + 1).unwrap();
        let (bridge, worker) = AcpBridge::channel(capacity);
        for index in 0..event_count {
            worker
                .send(BridgeEvent::Update {
                    session_id: agent_client_protocol_schema::v1::SessionId::new("session-1"),
                    text: index.to_string(),
                })
                .await
                .unwrap();
        }
        worker
            .send(BridgeEvent::Completed {
                session_id: agent_client_protocol_schema::v1::SessionId::new("session-1"),
                stop_reason: "end_turn".to_string(),
            })
            .await
            .unwrap();
        drop(worker);
        editor.agent_bridge = Some(bridge);
        editor.agent_active_sessions.insert("session-1".to_string());
        editor.agent_task = Some(tokio::spawn(async move {
            anyhow::bail!("fixture adapter exited")
        }));
        while !editor
            .agent_task
            .as_ref()
            .is_some_and(tokio::task::JoinHandle::is_finished)
        {
            tokio::task::yield_now().await;
        }
        let mut buffer =
            RenderBuffer::new(/*width*/ 80, /*height*/ 24, &Style::default());
        ACTION_DISPATCHER.send_request(PluginRequest::AgentNewSession {
            cwd: plugin_root.path().to_path_buf(),
        });
        ACTION_DISPATCHER.send_request(PluginRequest::AgentPrompt {
            session_id: "session-1".to_string(),
            text: "defer until the buffered tail is delivered".to_string(),
        });

        editor
            .service_background(&mut buffer, &mut runtime)
            .await
            .unwrap();

        assert!(editor.agent_bridge.is_some());
        assert!(editor.agent_task.is_some());
        assert!(editor.agent_active_sessions.contains("session-1"));
        assert!(editor.last_error.is_none());
        assert!(matches!(
            ACTION_DISPATCHER.try_recv_request(),
            Some(PluginRequest::AgentNewSession { cwd }) if cwd == plugin_root.path()
        ));
        assert!(matches!(
            ACTION_DISPATCHER.try_recv_request(),
            Some(PluginRequest::AgentPrompt { session_id, text })
                if session_id == "session-1"
                    && text == "defer until the buffered tail is delivered"
        ));

        editor
            .service_background(&mut buffer, &mut runtime)
            .await
            .unwrap();

        let mut expected = (0..event_count)
            .map(|index| format!("{index},"))
            .collect::<String>();
        expected.push_str("completed,lost");
        assert_eq!(editor.last_error.as_deref(), Some(expected.as_str()));
        assert!(editor.agent_bridge.is_none());
        assert!(editor.agent_task.is_none());
        assert!(editor.agent_active_sessions.is_empty());
        drain_plugin_requests();
    }

    #[tokio::test]
    async fn completed_agent_turn_ignores_late_updates_and_preserves_proposal_events() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_plugin_requests();
        let plugin_root = tempfile::tempdir().unwrap();
        let probe = plugin_root.path().join("proposal-probe.hk");
        std::fs::write(
            &probe,
            r#"
                pub fn activate() { red::on("agent:proposals_changed", changed); }
                fn changed(event: Json) { red::execute("Print", "proposal event delivered"); }
            "#,
        )
        .unwrap();
        let mut config = Config::default();
        config.plugins.retain(|name, _| name == "agent");
        config.plugins.insert(
            "agent".to_string(),
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("plugins/agent.hk")
                .to_string_lossy()
                .into_owned(),
        );
        config.plugins.insert(
            "proposal_probe".to_string(),
            probe.to_string_lossy().into_owned(),
        );
        config.lsp.enabled = false;
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            lsp,
            /*width*/ 100,
            /*height*/ 24,
            config,
            Theme::default(),
            vec![Buffer::new(None, "scratch".to_string())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor.test_set_agent_workspace(Arc::new(Mutex::new(
            ProposalWorkspace::new(plugin_root.path()).unwrap(),
        )));
        let mut core = DetachedEditorCore::new(editor).await.unwrap();
        core.editor
            .plugin_registry
            .notify(
                &mut core.runtime,
                "agent:session_created",
                json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        core.tick().await.unwrap();
        core.editor
            .agent_active_sessions
            .insert("session-1".to_string());
        let (bridge, mut worker) = AcpBridge::channel(NonZeroUsize::new(8).unwrap());
        for event in [
            BridgeEvent::Update {
                session_id: agent_client_protocol_schema::v1::SessionId::new("session-1"),
                text: "live output".to_string(),
            },
            BridgeEvent::Completed {
                session_id: agent_client_protocol_schema::v1::SessionId::new("session-1"),
                stop_reason: "end_turn".to_string(),
            },
            BridgeEvent::Update {
                session_id: agent_client_protocol_schema::v1::SessionId::new("session-1"),
                text: "stale output must be ignored".to_string(),
            },
            BridgeEvent::PermissionRequested {
                request_id: "stale-permission".to_string(),
                session_id: agent_client_protocol_schema::v1::SessionId::new("session-1"),
                tool_call: json!({ "tool_call_id": "stale-tool" }),
                options: Vec::new(),
            },
            BridgeEvent::ProposalsChanged {
                session_id: agent_client_protocol_schema::v1::SessionId::new("session-1"),
            },
        ] {
            worker.send(event).await.unwrap();
        }
        core.editor.agent_bridge = Some(bridge);

        core.tick().await.unwrap();

        assert!(!core.editor.agent_active_sessions.contains("session-1"));
        assert!(matches!(
            worker.recv().await,
            Some(BridgeCommand::PermissionResponse { request_id, option_id })
                if request_id == "stale-permission" && option_id.is_none()
        ));
        assert!(core.editor.current_dialog.is_none());
        assert_eq!(
            core.editor
                .preferences
                .plugin_storage("agent", "transcript")
                .and_then(Value::as_str),
            Some("Agent: live output\n")
        );
        let panel = core
            .snapshot(None)
            .lines
            .into_iter()
            .map(|line| line.text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(panel.contains("live output"));
        assert!(!panel.contains("stale output must be ignored"));
        assert_eq!(
            core.editor.last_error.as_deref(),
            Some("proposal event delivered")
        );
        drain_plugin_requests();
    }

    #[tokio::test]
    async fn stale_permission_does_not_block_input_when_the_acp_command_queue_is_full() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_plugin_requests();
        let mut core = DetachedEditorCore::new(test_editor(/*width*/ 80, /*height*/ 24))
            .await
            .unwrap();
        let (bridge, mut worker) = AcpBridge::channel(NonZeroUsize::new(1).unwrap());
        bridge
            .send(BridgeCommand::Prompt {
                session_id: agent_client_protocol_schema::v1::SessionId::new("stalled-session"),
                text: "occupy the command queue".to_string(),
            })
            .await
            .unwrap();
        worker
            .send(BridgeEvent::PermissionRequested {
                request_id: "stale-permission".to_string(),
                session_id: agent_client_protocol_schema::v1::SessionId::new("stalled-session"),
                tool_call: json!({ "tool_call_id": "stale-tool" }),
                options: Vec::new(),
            })
            .await
            .unwrap();
        core.editor.agent_bridge = Some(bridge);

        tokio::time::timeout(
            Duration::from_secs(1),
            core.input(crate::headless::InputEvent::Key {
                code: crate::headless::KeyCode::Character('h'),
                modifiers: Vec::new(),
            }),
        )
        .await
        .expect("a stale permission must not block editor input")
        .unwrap();

        assert!(core.editor.current_dialog.is_none());
        assert!(matches!(
            worker.recv().await,
            Some(BridgeCommand::Prompt { session_id, text })
                if session_id.0.as_ref() == "stalled-session"
                    && text == "occupy the command queue"
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), worker.recv())
                .await
                .is_err()
        );
        drain_plugin_requests();
    }

    #[tokio::test]
    async fn concurrent_agent_prompt_preserves_the_active_proposal_turn_until_completion() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_plugin_requests();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("proposal.txt");
        std::fs::write(&path, "disk base\n").unwrap();
        let workspace = Arc::new(Mutex::new(ProposalWorkspace::new(root.path()).unwrap()));
        let mut editor = test_editor(/*width*/ 80, /*height*/ 24);
        editor.agent_workspace = Some(Arc::clone(&workspace));
        let (bridge, mut worker) = AcpBridge::channel(NonZeroUsize::new(8).unwrap());
        editor.agent_bridge = Some(bridge);
        let mut buffer =
            RenderBuffer::new(/*width*/ 80, /*height*/ 24, &Style::default());
        let mut runtime = Runtime::new();

        ACTION_DISPATCHER.send_request(PluginRequest::AgentPrompt {
            session_id: "session-1".to_string(),
            text: "first prompt".to_string(),
        });
        editor
            .service_background(&mut buffer, &mut runtime)
            .await
            .unwrap();
        assert!(matches!(
            worker.recv().await,
            Some(BridgeCommand::Prompt { text, .. }) if text == "first prompt"
        ));
        assert!(editor.agent_active_sessions.contains("session-1"));
        let first_turn = {
            let mut workspace = workspace.lock().unwrap();
            workspace
                .write("session-1", &path, "first edit\n".to_string())
                .unwrap();
            match workspace
                .stage_accept_all("session-1", &path, 0, "disk base\n")
                .unwrap()
                .disposition()
            {
                ProposalDisposition::Applied { turn_id, .. } => turn_id.clone(),
                other => panic!("expected attributed first edit, got {other:?}"),
            }
        };

        ACTION_DISPATCHER.send_request(PluginRequest::AgentCancel {
            session_id: "session-1".to_string(),
        });
        editor
            .service_background(&mut buffer, &mut runtime)
            .await
            .unwrap();
        assert!(matches!(
            worker.recv().await,
            Some(BridgeCommand::Cancel { session_id }) if session_id.0.as_ref() == "session-1"
        ));
        worker
            .send(BridgeEvent::Cancelled {
                session_id: agent_client_protocol_schema::v1::SessionId::new("session-1"),
            })
            .await
            .unwrap();
        editor
            .service_background(&mut buffer, &mut runtime)
            .await
            .unwrap();
        assert!(editor.agent_active_sessions.contains("session-1"));

        let generation = workspace.lock().unwrap().generation();
        ACTION_DISPATCHER.send_request(PluginRequest::AgentPrompt {
            session_id: "session-1".to_string(),
            text: "concurrent prompt".to_string(),
        });
        editor
            .service_background(&mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(workspace.lock().unwrap().generation(), generation);
        assert!(editor
            .last_error
            .as_deref()
            .is_some_and(|message| message.contains("already active")));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), worker.recv())
                .await
                .is_err()
        );
        let attributed_turn = {
            let mut workspace = workspace.lock().unwrap();
            workspace
                .write("session-1", &path, "first turn final edit\n".to_string())
                .unwrap();
            match workspace
                .stage_accept_all("session-1", &path, 0, "disk base\n")
                .unwrap()
                .disposition()
            {
                ProposalDisposition::Applied { turn_id, .. } => turn_id.clone(),
                other => panic!("expected attributed final edit, got {other:?}"),
            }
        };
        assert_eq!(attributed_turn, first_turn);

        worker
            .send(BridgeEvent::Completed {
                session_id: agent_client_protocol_schema::v1::SessionId::new("session-1"),
                stop_reason: "cancelled".to_string(),
            })
            .await
            .unwrap();
        editor
            .service_background(&mut buffer, &mut runtime)
            .await
            .unwrap();
        assert!(!editor.agent_active_sessions.contains("session-1"));
        ACTION_DISPATCHER.send_request(PluginRequest::AgentPrompt {
            session_id: "session-1".to_string(),
            text: "next prompt".to_string(),
        });
        editor
            .service_background(&mut buffer, &mut runtime)
            .await
            .unwrap();
        assert!(matches!(
            worker.recv().await,
            Some(BridgeCommand::Prompt { text, .. }) if text == "next prompt"
        ));
        drain_plugin_requests();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detached_prequeued_agent_restart_does_not_close_a_reused_replacement_session() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_plugin_requests();
        let cwd = std::env::current_dir().unwrap();
        let root = tempfile::Builder::new()
            .prefix(".red-acp-prequeued-restart-")
            .tempdir_in(&cwd)
            .unwrap();
        let path = root.path().join("proposal.txt");
        let gate = root.path().join("adapter-ready");
        let session_file = root.path().join("session-created");
        let prompt_file = root.path().join("prompt.txt");
        let close_file = root.path().join("unexpected-close.txt");
        let adapter = root.path().join("restart-acp.py");
        std::fs::write(&path, "disk base\n").unwrap();
        std::fs::write(
            &adapter,
            r#"#!/usr/bin/env python3
import json
import os
import sys
import time

gate = os.environ["RED_TEST_ACP_GATE"]
session_file = os.environ["RED_TEST_ACP_SESSION"]
prompt_file = os.environ["RED_TEST_ACP_PROMPT"]
close_file = os.environ["RED_TEST_ACP_CLOSE"]
for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialize":
        while not os.path.exists(gate):
            time.sleep(0.005)
        result = {"protocolVersion": 1, "agentCapabilities": {"sessionCapabilities": {"close": {}}}}
    elif method == "session/new":
        with open(session_file + ".tmp", "w", encoding="utf-8") as output:
            output.write("session-1")
        os.replace(session_file + ".tmp", session_file)
        result = {"sessionId": "session-1"}
    elif method == "session/prompt":
        prompt = request["params"]["prompt"]
        with open(prompt_file + ".tmp", "w", encoding="utf-8") as output:
            output.write("".join(block.get("text", "") for block in prompt))
        os.replace(prompt_file + ".tmp", prompt_file)
        result = {"stopReason": "end_turn"}
    elif method == "session/cancel":
        with open(close_file, "a", encoding="utf-8") as output:
            output.write("cancel\n")
        continue
    elif method == "session/close":
        with open(close_file, "a", encoding="utf-8") as output:
            output.write("close\n")
        result = {}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
"#,
        )
        .unwrap();
        std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut workspace = ProposalWorkspace::new(&cwd).unwrap();
        workspace.begin_turn("session-1", "turn-1".to_string());
        workspace
            .write("session-1", &path, "agent replacement\n".to_string())
            .unwrap();
        let workspace = Arc::new(Mutex::new(workspace));
        let mut config = Config::from_user_toml_with_overrides("", &[]).unwrap();
        config.plugins.retain(|name, _| name == "agent");
        config.plugins.insert(
            "agent".to_string(),
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("plugins/agent.hk")
                .to_string_lossy()
                .into_owned(),
        );
        config.lsp.enabled = false;
        config.agent.command = Some(adapter.to_string_lossy().into_owned());
        for (key, value) in [
            ("RED_TEST_ACP_GATE", gate.as_path()),
            ("RED_TEST_ACP_SESSION", session_file.as_path()),
            ("RED_TEST_ACP_PROMPT", prompt_file.as_path()),
            ("RED_TEST_ACP_CLOSE", close_file.as_path()),
        ] {
            config
                .agent
                .env
                .insert(key.to_string(), value.to_string_lossy().into_owned());
        }
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            lsp,
            /*width*/ 100,
            /*height*/ 24,
            config,
            Theme::default(),
            vec![Buffer::new(None, "scratch".to_string())],
        )
        .unwrap();
        editor.test_set_agent_workspace(Arc::clone(&workspace));
        let mut core = DetachedEditorCore::new(editor).await.unwrap();
        assert!(
            matches!(
                core.editor.plugin_registry.statuses().get("agent"),
                Some(crate::plugin::PluginStatus::Active)
            ),
            "agent plugin status: {:?}",
            core.editor.plugin_registry.statuses().get("agent")
        );
        core.editor
            .plugin_registry
            .notify(
                &mut core.runtime,
                "agent:session_created",
                json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        core.tick().await.unwrap();

        let (bridge, worker) = AcpBridge::channel(NonZeroUsize::new(2).unwrap());
        core.editor.agent_bridge = Some(bridge);
        core.editor.agent_task = Some(tokio::spawn(async move {
            drop(worker);
            anyhow::bail!("fixture adapter exited")
        }));
        while !core
            .editor
            .agent_task
            .as_ref()
            .is_some_and(tokio::task::JoinHandle::is_finished)
        {
            tokio::task::yield_now().await;
        }
        ACTION_DISPATCHER.send_request(PluginRequest::AgentNewSession { cwd: cwd.clone() });

        core.input(crate::headless::InputEvent::Key {
            code: crate::headless::KeyCode::Character('h'),
            modifiers: Vec::new(),
        })
        .await
        .expect("a prequeued ACP restart should remain recoverable");
        {
            let workspace = workspace.lock().unwrap();
            assert_eq!(
                workspace.pending_files("session-1"),
                std::slice::from_ref(&path)
            );
            assert_eq!(
                workspace.review_sessions(""),
                ["session-1"],
                "last editor status: {:?}, bridge: {}, task: {}",
                core.editor.last_error,
                core.editor.agent_bridge.is_some(),
                core.editor.agent_task.is_some()
            );
        }
        std::fs::write(&gate, "ready").unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !session_file.exists() {
                core.tick().await.unwrap();
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the replacement ACP session should start");
        assert!(!close_file.exists());

        for character in [' ', 'A'] {
            core.input(crate::headless::InputEvent::Key {
                code: crate::headless::KeyCode::Character(character),
                modifiers: Vec::new(),
            })
            .await
            .unwrap();
        }
        core.input(crate::headless::InputEvent::Paste {
            text: "keep the replacement alive".to_string(),
        })
        .await
        .unwrap();
        core.input(crate::headless::InputEvent::Key {
            code: crate::headless::KeyCode::Enter,
            modifiers: Vec::new(),
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !prompt_file.exists() {
                core.tick().await.unwrap();
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the reused replacement session should accept prompts");
        assert_eq!(
            std::fs::read_to_string(&prompt_file).unwrap(),
            "keep the replacement alive"
        );
        assert!(!close_file.exists());

        drop(core.editor.agent_bridge.take());
        if let Some(task) = core.editor.agent_task.take() {
            task.abort();
        }
        drain_plugin_requests();
    }

    #[tokio::test]
    async fn detached_paste_chunks_render_once_and_form_one_undoable_edit() {
        let mut editor = test_editor(80, 24);
        editor.mode = Mode::Insert;
        let mut core = DetachedEditorCore::new(editor).await.unwrap();
        let revision = core.revision;

        let first = core
            .input(crate::headless::InputEvent::PasteChunk {
                text: "large ".to_string(),
                final_chunk: false,
            })
            .await
            .unwrap();
        assert_eq!(first.revision, revision);
        assert!(first.lines.is_empty());

        let completed = core
            .input(crate::headless::InputEvent::PasteChunk {
                text: "paste".to_string(),
                final_chunk: true,
            })
            .await
            .unwrap();
        assert!(completed
            .lines
            .iter()
            .any(|line| line.text.contains("large pastehello")));

        core.editor.mode = Mode::Normal;
        core.editor
            .undo_transaction(&mut core.render_buffer, &mut core.runtime)
            .await
            .unwrap();
        let undone = core.finish_render().unwrap();
        assert!(undone.lines.iter().any(|line| line.text.contains("hello")));
        assert!(!undone
            .lines
            .iter()
            .any(|line| line.text.contains("large paste")));
    }

    #[tokio::test]
    async fn detached_resize_notifies_plugins_through_the_native_event_path() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_plugin_requests();
        let plugin_path =
            std::env::temp_dir().join(format!("red-detached-resize-{}.hk", uuid::Uuid::new_v4()));
        std::fs::write(
            &plugin_path,
            r#"
                pub fn activate() { red::on("editor:resize", resized); }
                fn resized(event: Json) { red::execute("Print", "resize observed"); }
            "#,
        )
        .unwrap();
        let mut editor = test_editor(80, 24);
        editor.config.plugins.insert(
            "resize_probe".to_string(),
            plugin_path.to_string_lossy().into_owned(),
        );
        let mut core = DetachedEditorCore::new(editor).await.unwrap();

        let delta = core.resize(52, 14).await.unwrap();

        assert_eq!(core.editor.last_error.as_deref(), Some("resize observed"));
        assert!(delta
            .lines
            .iter()
            .any(|line| line.text.contains("resize observed")));
        drain_plugin_requests();
    }

    #[test]
    fn coalesces_only_adjacent_resize_events() {
        let key = Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let mut pending = VecDeque::from([
            Event::Resize(100, 30),
            Event::Resize(120, 40),
            key.clone(),
            Event::Resize(140, 50),
        ]);

        assert_eq!(
            Editor::coalesce_resize_run(Event::Resize(80, 24), &mut pending),
            Event::Resize(120, 40)
        );
        assert_eq!(pending.pop_front(), Some(key));
        assert_eq!(pending.pop_front(), Some(Event::Resize(140, 50)));
    }

    #[test]
    fn leaves_non_resize_events_in_order() {
        let key = Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let mut pending = VecDeque::from([key.clone(), Event::Resize(120, 40)]);

        assert_eq!(
            Editor::coalesce_resize_run(Event::Resize(80, 24), &mut pending),
            Event::Resize(80, 24)
        );
        assert_eq!(pending.pop_front(), Some(key));
        assert_eq!(pending.pop_front(), Some(Event::Resize(120, 40)));
    }

    #[test]
    fn editor_snapshot_accepts_legacy_camel_case_fields() {
        let snapshot: EditorStateSnapshot = serde_json::from_value(json!({
            "version": 1,
            "cwd": "/repo",
            "savedAt": 42,
            "buffers": [{
                "index": 0,
                "path": "src/main.rs",
                "dirty": false,
                "cursor": { "x": 1, "y": 2 },
                "viewportTop": 3,
            }],
            "currentBufferIndex": 0,
            "windowLayout": {
                "activeWindowId": 0,
                "root": {
                    "kind": "window",
                    "bufferIndex": 0,
                    "vtop": 0,
                    "vleft": 0,
                    "cx": 0,
                    "cy": 0,
                    "vx": 0,
                },
            },
        }))
        .unwrap();

        assert_eq!(snapshot.saved_at, 42);
        assert_eq!(snapshot.current_buffer_index, 0);
        assert_eq!(snapshot.buffers[0].viewport_top, 3);
    }

    #[test]
    fn plugin_json_normalizes_nested_protocol_keys() {
        let payload = plugin_json(json!({
            "lspClient": { "workspaceRoot": "/repo" },
            "paddingLeft": true,
            "URLValue": 1,
        }));

        assert_eq!(payload["lsp_client"]["workspace_root"], "/repo");
        assert_eq!(payload["padding_left"], true);
        assert_eq!(payload["url_value"], 1);
    }

    async fn install_event_recorder(editor: &mut Editor, runtime: &mut Runtime) {
        drain_plugin_requests();
        let plugin_path =
            std::env::temp_dir().join(format!("red-event-recorder-{}.hk", uuid::Uuid::new_v4()));
        std::fs::write(
            &plugin_path,
            r#"
                pub fn activate() {
                    red::on("cursor:moved", cursor_moved);
                    red::on("mode:changed", mode_changed);
                    red::on("search:highlighted", search_highlighted);
                    red::on("search:cleared", search_cleared);
                }

                fn cursor_moved(event: Json) {
                    red::execute("RecordCursorMoved", event);
                }

                fn mode_changed(event: Json) {
                    red::execute("RecordModeChanged", event);
                }

                fn search_highlighted(event: Json) {
                    red::execute("RecordSearchHighlighted", event);
                }

                fn search_cleared(event: Json) {
                    red::execute("RecordSearchCleared", event);
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

    async fn install_theme_probe(editor: &mut Editor, runtime: &mut Runtime) {
        drain_plugin_requests();
        let plugin_path =
            std::env::temp_dir().join(format!("red-theme-probe-{}.hk", uuid::Uuid::new_v4()));
        std::fs::write(
            &plugin_path,
            r#"
                pub fn activate() {
                    red::on("theme:changed", theme_changed);
                }

                fn theme_changed(event: Json) {
                    red::execute("Print", red::editor_info().theme.name);
                }
            "#,
        )
        .unwrap();

        editor
            .plugin_registry
            .add("theme_probe", plugin_path.to_string_lossy().as_ref());
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

    #[test]
    fn terminal_reentry_invalidates_the_previous_frame_and_cursor_cache() {
        let mut editor = test_editor(/*width*/ 20, /*height*/ 5);
        let mut buffer = RenderBuffer::new(/*width*/ 20, /*height*/ 5, &Style::default());
        editor.render(&mut buffer).unwrap();
        assert!(buffer.cells.iter().any(|cell| cell.text != " "));
        assert!(editor.last_rendered_cursor_position.is_some());

        editor.invalidate_terminal_render_state(&mut buffer);

        assert!(buffer
            .cells
            .iter()
            .all(|cell| cell.text == " " && cell.style == Style::default()));
        assert_eq!(editor.last_rendered_cursor_position, None);
        assert_eq!(editor.last_rendered_cursor_surface, None);

        editor.render(&mut buffer).unwrap();
        assert!(buffer.cells.iter().any(|cell| cell.text != " "));
        assert!(editor.last_rendered_cursor_position.is_some());
    }

    #[test]
    #[should_panic(expected = "editor content mutations must occur inside an edit transaction")]
    fn recorded_edits_require_an_active_transaction() {
        let mut editor = test_editor(/*width*/ 80, /*height*/ 24);
        let position = TextPosition::new(/*line*/ 0, /*character*/ 0);

        editor.replace_range(TextRange::insertion(position), /*new_text*/ "x");
    }

    #[test]
    fn search_match_cache_reuses_matches_and_invalidates_on_revision_or_case_change() {
        let mut editor = test_editor(/*width*/ 80, /*height*/ 24);

        let initial = editor.search_matches("hello").unwrap();
        let reused = editor.search_matches("hello").unwrap();
        assert!(Arc::ptr_eq(&initial, &reused));
        assert_eq!(initial.len(), 1);

        editor.current_buffer_mut().insert_str(0, 0, "HELLO ");
        let edited = editor.search_matches("hello").unwrap();
        assert!(!Arc::ptr_eq(&initial, &edited));
        assert_eq!(edited.len(), 1);

        editor.config.search.ignorecase = true;
        let insensitive = editor.search_matches("hello").unwrap();
        assert!(!Arc::ptr_eq(&edited, &insensitive));
        assert_eq!(insensitive.len(), 2);

        editor.config.search.smartcase = true;
        let smartcase = editor.search_matches("HELLO").unwrap();
        assert_eq!(smartcase.len(), 1);
    }

    #[test]
    fn search_match_cache_distinguishes_buffers_with_the_same_revision() {
        let mut editor = test_editor(/*width*/ 80, /*height*/ 24);
        let first = editor.search_matches("hello").unwrap();

        editor
            .buffers
            .push(Buffer::new(None, "hello hello".to_string()));
        editor.current_buffer_index = 1;
        let second = editor.search_matches("hello").unwrap();

        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(second.len(), 2);
    }

    #[test]
    fn incremental_search_on_an_oversized_line_only_finds_the_next_match() {
        let mut editor = test_editor(/*width*/ 80, /*height*/ 24);
        editor.buffers[0] = Buffer::new(
            Some("large.txt".to_string()),
            format!("{}\n", "x".repeat(MAX_HIGHLIGHT_SLICE_BYTES + 1)),
        );
        editor.begin_search(SearchDirection::Forward);
        editor.active_search.as_mut().unwrap().draft = "x".to_string();

        editor.update_search_preview();

        let preview = editor.active_search.as_ref().unwrap().preview.unwrap();
        assert_eq!((preview.start_x, preview.start_y), (1, 0));
        assert!(editor.search_match_cache.is_none());
    }

    #[tokio::test]
    async fn buffer_change_flush_is_revisioned_and_idempotent() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_plugin_requests();

        let mut editor = test_editor(/*width*/ 80, /*height*/ 24);
        let mut runtime = Runtime::new();
        let plugin_path = std::env::temp_dir().join(format!(
            "red-buffer-change-recorder-{}.hk",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &plugin_path,
            r#"
                pub fn activate() {
                    red::on("buffer:changed", buffer_changed);
                }

                fn buffer_changed(_event: Json) {
                    red::execute("Print", "changed");
                }
            "#,
        )
        .unwrap();
        editor.plugin_registry.add(
            "buffer_change_recorder",
            plugin_path.to_string_lossy().as_ref(),
        );
        editor
            .plugin_registry
            .initialize(&mut runtime)
            .await
            .unwrap();
        drain_plugin_requests();

        let position = TextPosition::new(/*line*/ 0, /*character*/ 0);
        editor.begin_transaction("test edit");
        editor.replace_range(TextRange::insertion(position), /*new_text*/ "x");
        editor.commit_transaction(editor.cursor_snapshot());
        let revision = editor.current_buffer().revision();

        editor
            .flush_change_notification(&mut runtime)
            .await
            .unwrap();
        editor
            .flush_change_notification(&mut runtime)
            .await
            .unwrap();

        assert_eq!(collect_print_requests(), vec!["changed"]);
        assert_eq!(
            editor
                .notified_buffer_revisions
                .get(&editor.current_buffer().id()),
            Some(&revision)
        );
    }

    fn rust_test_editor(lines: usize, width: usize, height: usize) -> Editor {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let contents = (0..lines)
            .map(|i| format!("fn func_{i}() {{ let value_{i} = \"text {i}\"; }}\n"))
            .collect::<String>();
        let theme = parse_vscode_theme("themes/mocha.json").unwrap();
        let buffer = Buffer::new(Some("/tmp/red-highlight-test.rs".to_string()), contents);
        let mut editor =
            Editor::with_size(lsp, width, height, config, theme, vec![buffer]).unwrap();
        editor.test_disable_terminal_output();
        editor
    }

    fn span_shape(spans: &[HighlightSpan]) -> Vec<(usize, usize, Style)> {
        spans
            .iter()
            .map(|span| (span.start, span.end, span.style.clone()))
            .collect()
    }

    #[test]
    fn viewport_highlight_slice_matches_fresh_parse() {
        // Scrolling slices spans out of the cached padded parse; the result
        // must match what a cold parse at the same viewport produces.
        let (vtop, height) = (30, 20);
        let mut scrolled = rust_test_editor(200, 120, height + 2);
        scrolled
            .viewport_highlight_spans(0, vtop - 5, height)
            .unwrap();
        let sliced = scrolled.viewport_highlight_spans(0, vtop, height).unwrap();

        let mut fresh = rust_test_editor(200, 120, height + 2);
        let parsed = fresh.viewport_highlight_spans(0, vtop, height).unwrap();

        assert!(!parsed.is_empty(), "rust source should produce spans");
        assert_eq!(span_shape(&sliced), span_shape(&parsed));
    }

    #[test]
    fn viewport_highlight_cache_invalidates_on_edit() {
        let mut editor = rust_test_editor(100, 120, 22);
        let before = editor.viewport_highlight_spans(0, 10, 20).unwrap();
        assert!(!before.is_empty());

        editor.current_buffer_mut().insert_str(0, 10, "// ");
        let after = editor.viewport_highlight_spans(0, 10, 20).unwrap();
        assert_ne!(span_shape(&before), span_shape(&after));
    }

    #[test]
    fn viewport_highlight_handles_view_past_end_of_buffer() {
        let mut editor = rust_test_editor(5, 120, 22);
        assert!(editor
            .viewport_highlight_spans(0, 10, 20)
            .unwrap()
            .is_empty());
        assert!(!editor
            .viewport_highlight_spans(0, 0, 20)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn viewport_highlight_skips_an_oversized_physical_line() {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let contents = format!(
            "{{\"value\":\"{}\"}}\n",
            "x".repeat(MAX_HIGHLIGHT_SLICE_BYTES)
        );
        let buffer = Buffer::new(Some("large.json".to_string()), contents);
        let mut editor =
            Editor::with_size(lsp, 80, 24, config, Theme::default(), vec![buffer]).unwrap();

        assert!(editor
            .viewport_highlight_spans(0, 0, 22)
            .unwrap()
            .is_empty());
        assert!(!editor.highlight_cache.contains_key(&0));
    }

    #[test]
    fn plugin_viewport_layout_bounds_oversized_wrapped_line_text() {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(
            Some("large.txt".to_string()),
            format!("{}\n", "x".repeat(MAX_HIGHLIGHT_SLICE_BYTES + 1)),
        );
        let editor =
            Editor::with_size(lsp, 80, 24, config, Theme::default(), vec![buffer]).unwrap();

        let payload = editor.plugin_viewport_layout_payload();
        let rows = payload["rows"].as_array().unwrap();

        assert!(rows.len() > 1);
        assert_eq!(
            rows[0]["text"].as_str().unwrap().len(),
            MAX_PLUGIN_VIEWPORT_LINE_CHARS
        );
        assert!(rows
            .iter()
            .skip(1)
            .all(|row| row["text"].as_str() == Some("")));
    }

    #[test]
    fn viewport_highlight_drops_margin_before_skipping_a_normal_viewport() {
        let mut editor = rust_test_editor(1, 80, 24);
        let contents = format!(
            "// {}\nfn visible() {{ let value = \"highlighted\"; }}\n",
            "x".repeat(MAX_HIGHLIGHT_SLICE_BYTES)
        );
        editor.buffers[0] = Buffer::new(Some("margin.rs".to_string()), contents);

        let spans = editor.viewport_highlight_spans(0, 1, 1).unwrap();

        assert!(!spans.is_empty());
        let entry = editor.highlight_cache.get(&0).unwrap();
        assert_eq!(entry.start_line, 1);
        assert_eq!(entry.line_offsets.len(), 2);
    }

    #[test]
    fn wrapped_line_continuation_renders() {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let long_line =
            "let dialog = Some(Box::new(Picker::new(title.clone(), other, items, id)));";
        let contents = format!("short one\n{long_line}\nshort two\n");
        let buffer = Buffer::new(None, contents);
        // wrap defaults to on; 40 columns forces the long line to wrap.
        let mut editor =
            Editor::with_size(lsp, 40, 10, config, Theme::default(), vec![buffer]).unwrap();
        editor.test_disable_terminal_output();

        let mut render_buffer = RenderBuffer::new(40, 10, &Style::default());
        editor.render(&mut render_buffer).unwrap();

        let screen = (0..8)
            .map(|y| render_row(&render_buffer, y))
            .collect::<Vec<_>>();
        let content_start = editor.gutter_width() + 1;
        let wrapped = screen[1..4]
            .iter()
            .map(|row| row.chars().skip(content_start).collect::<String>())
            .map(|row| row.trim_end().to_string())
            .collect::<String>();
        assert_eq!(wrapped, long_line);
    }

    #[test]
    fn wrapped_line_continuation_renders_when_scrolled() {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let long_line = format!(
            "{}self.current_dialog = Some(Box::new(Picker::new(title.clone(), self, items, *id)));",
            " ".repeat(20)
        );
        let mut lines = (0..40)
            .map(|i| format!("    let foo_{i} = {i};\n"))
            .collect::<Vec<_>>();
        lines[20] = format!("{long_line}\n");
        let theme = parse_vscode_theme("themes/mocha.json").unwrap();
        let buffer = Buffer::new(Some("/tmp/red-wrap-test.rs".to_string()), lines.concat());
        let mut editor = Editor::with_size(lsp, 100, 30, config, theme, vec![buffer]).unwrap();
        editor.test_disable_terminal_output();

        editor.vtop = 7;
        editor.cy = 13;
        let mut render_buffer = RenderBuffer::new(100, 30, &Style::default());
        editor.render(&mut render_buffer).unwrap();

        let screen = (0..28)
            .map(|y| render_row(&render_buffer, y))
            .collect::<Vec<_>>();
        let all = screen.join("\n").replace(' ', "·");
        let flat = screen.join("").replace(' ', "");
        assert!(
            flat.contains("*id)));"),
            "wrapped continuation should be rendered when scrolled, got:\n{all}"
        );
    }

    fn render_row(buffer: &RenderBuffer, y: usize) -> String {
        buffer.cells[y * buffer.width..(y + 1) * buffer.width]
            .iter()
            .map(|cell| cell.c)
            .collect()
    }

    #[test]
    fn renders_tabs_as_aligned_spaces() {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(
            Some("/tmp/red-tab-render.lua".to_string()),
            "\talpha\n\t\tbeta\n".to_string(),
        );
        let mut editor =
            Editor::with_size(lsp, 40, 8, config, Theme::default(), vec![buffer]).unwrap();
        editor.test_disable_terminal_output();
        editor.cx = 1;

        let mut render_buffer = RenderBuffer::new(40, 8, &Style::default());
        editor.render(&mut render_buffer).unwrap();

        let content_start = editor.gutter_width() + 1;
        let first = render_row(&render_buffer, 0);
        let second = render_row(&render_buffer, 1);
        assert_eq!(first.find("alpha"), Some(content_start + 4));
        assert_eq!(second.find("beta"), Some(content_start + 8));
        assert!(!first.contains('\t'));
        assert!(!second.contains('\t'));
        let cursor = editor
            .buffer_to_window_coords(editor.window_manager.active_window().unwrap(), 1, 0)
            .unwrap();
        assert_eq!(cursor.0, content_start + 4);
    }

    fn install_test_window_bar(editor: &mut Editor) {
        let window_id = editor
            .window_manager
            .active_stable_window_id()
            .expect("test editor should have an active window");
        editor
            .window_bar_manager
            .create("test-bar".to_string(), plugin::WindowBarConfig::default());
        editor.window_bar_manager.update(
            "test-bar",
            window_id,
            vec![plugin::WindowBarSegment {
                id: Some("chrome".to_string()),
                text: "chrome".to_string(),
                style: plugin::WindowBarStyle::default(),
                tooltip: None,
                action: None,
            }],
        );
    }

    #[test]
    fn language_viewports_report_expected_indentation() {
        for (file, expected_width) in [
            ("fixture.js", 2),
            ("fixture.json", 2),
            ("fixture.yaml", 2),
            ("fixture.py", 4),
            ("fixture.rs", 4),
        ] {
            let config = Config::default();
            let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
            let buffer = Buffer::new(Some(file.to_string()), "content".to_string());
            let editor =
                Editor::with_size(lsp, 30, 8, config, Theme::default(), vec![buffer]).unwrap();

            let layout = editor.plugin_viewport_layout_payload();

            assert_eq!(
                layout["indentation"]["shift_width"],
                json!(expected_width),
                "unexpected shift width for {file}"
            );
            assert_eq!(
                layout["indentation"]["tab_width"],
                json!(expected_width),
                "unexpected tab width for {file}"
            );
        }
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
        let content_start = layout["content_start"].as_u64().unwrap() as usize;
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
    fn motion_delta_keeps_syntax_highlighting_on_same_row_moves() {
        // Moving within one screen row (e.g. `w`) re-renders that row twice
        // in a single delta pass; the shared StyleCursor must still style it.
        let mut editor = rust_test_editor(40, 120, 20);

        let mut render_buffer = RenderBuffer::new(120, 20, &Style::default());
        editor.cy = 5;
        editor.render(&mut render_buffer).unwrap();
        let layout = editor.plugin_viewport_layout_payload();
        let content_start = layout["content_start"].as_u64().unwrap() as usize;
        // Probe one past the cursor column so the synthetic block cursor's
        // fg/bg swap can't mask styling changes.
        let row5 = 5 * 120 + content_start + 1;
        let styled = render_buffer.cells[row5].style.fg;
        assert_ne!(styled, editor.theme.style.fg, "fn keyword should be styled");

        // Same-row motion (`w` within a line): the row appears twice in the
        // delta row list and must keep its styles.
        editor.last_rendered_cursor_position = Some((content_start, 5));
        editor.cx = 3;
        editor
            .render_cursor_motion_delta(&mut render_buffer)
            .unwrap();
        assert_eq!(
            render_buffer.cells[row5].style.fg, styled,
            "same-row delta render must keep highlighting"
        );

        // Upward motion (`k`/`b`): the delta rows arrive in decreasing order;
        // the upper row must still be styled.
        editor.last_rendered_cursor_position = Some((content_start, 10));
        editor.cy = 5;
        editor.cx = 0;
        editor
            .render_cursor_motion_delta(&mut render_buffer)
            .unwrap();
        assert_eq!(
            render_buffer.cells[row5].style.fg, styled,
            "upward delta render must keep highlighting"
        );
    }

    #[test]
    fn break_indent_aligns_wrapped_rows_on_screen() {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let contents = format!("marker\n{}{}\n", " ".repeat(4), "x".repeat(60));
        let buffer = Buffer::new(None, contents);
        let mut editor =
            Editor::with_size(lsp, 40, 8, config, Theme::default(), vec![buffer]).unwrap();
        editor.test_disable_terminal_output();

        let mut render_buffer = RenderBuffer::new(40, 8, &Style::default());
        editor.render(&mut render_buffer).unwrap();

        let screen = (0..6)
            .map(|y| render_row(&render_buffer, y))
            .collect::<Vec<_>>();
        let all = screen.join("\n");
        let content_col = screen[0].find("marker").expect("marker visible");

        // First row of the wrapped line: 4 columns of real indentation.
        let first_x = screen[1].find('x').expect("first segment visible");
        assert_eq!(first_x, content_col + 4, "got:\n{all}");

        // The continuation row aligns to the indentation instead of column 0.
        let continuation_x = screen[2].find('x').expect("continuation visible");
        assert_eq!(continuation_x, content_col + 4, "got:\n{all}");
    }

    #[test]
    fn only_whitespace_decorations_do_not_cover_wrapped_text() {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        // 8 leading spaces, long enough to wrap at 40 columns.
        let long_line = format!("{}let value = make(one, two, three, four);", " ".repeat(8));
        let contents = format!("fn main() {{\n{long_line}\n}}\n");
        let buffer = Buffer::new(None, contents);
        let mut editor =
            Editor::with_size(lsp, 40, 8, config, Theme::default(), vec![buffer]).unwrap();
        editor.test_disable_terminal_output();

        // Mirrors what the indent_guides plugin emits: one guide string
        // spanning the indentation, repeated on wrapped rows, whitespace-only.
        editor.decoration_manager.set(
            "guides".to_string(),
            vec![crate::plugin::Decoration {
                buffer_index: Some(0),
                anchor: crate::plugin::DecorationAnchor::Column,
                line: 1,
                column: 0,
                text: "│   │   ".to_string(),
                style: Style::default(),
                priority: 1,
                repeat_linebreak: true,
                only_whitespace: true,
            }],
        );

        let mut render_buffer = RenderBuffer::new(40, 8, &Style::default());
        editor.render(&mut render_buffer).unwrap();

        let screen = (0..6)
            .map(|y| render_row(&render_buffer, y))
            .collect::<Vec<_>>();
        let all = screen.join("\n");
        // The guide must render over the indentation on the first row...
        assert!(
            screen[1].contains('│'),
            "guides should render on the first segment, got:\n{all}"
        );
        // ...and the wrapped continuation text must stay intact.
        let flat = screen.join("").replace(' ', "");
        assert!(
            flat.contains("four);"),
            "wrapped continuation must not be covered by guides, got:\n{all}"
        );
        // With break-indent, the continuation's virtual indent repeats the
        // guides (like vim), while the wrapped text after it is untouched.
        let content_col = screen[0].find("fn main").expect("first line visible");
        let continuation = screen[2].chars().collect::<Vec<_>>();
        let guide_cols = continuation
            .iter()
            .enumerate()
            .filter(|(_, c)| **c == '│')
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        assert_eq!(
            guide_cols,
            vec![content_col, content_col + 4],
            "guides should repeat inside the virtual indent, got:\n{all}"
        );
        let text_x = continuation
            .iter()
            .position(|c| c.is_alphabetic())
            .expect("continuation text visible");
        assert_eq!(text_x, content_col + 8, "got:\n{all}");
    }

    #[test]
    fn selection_highlight_follows_break_indent_on_wrapped_rows() {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        // 4-space indent, wraps at 40 columns into one continuation row.
        let long_line = format!("{}{}", " ".repeat(4), "x".repeat(60));
        let contents = format!("marker\n{long_line}\ntail\n");
        let buffer = Buffer::new(None, contents);
        let mut editor =
            Editor::with_size(lsp, 40, 8, config, Theme::default(), vec![buffer]).unwrap();
        editor.test_disable_terminal_output();

        // Select the wrapped line line-wise; render_overlays derives the
        // selection rect from selection_start and the cursor.
        editor.cy = 1;
        editor.cx = 0;
        editor.mode = Mode::VisualLine;
        editor.selection_start = Some(Point::new(0, 1));

        let selection_bg = editor
            .theme
            .selected_style(
                &editor.theme.style,
                &editor.theme.editor_selection_style(),
                crate::theme::SelectionForegroundPriority::Selection,
            )
            .bg
            .unwrap();
        let layout = editor.plugin_viewport_layout_payload();
        let content_start = layout["content_start"].as_u64().unwrap() as usize;
        let rows = layout["rows"].as_array().unwrap();
        let continuation = &rows[2];
        assert_eq!(continuation["line"].as_u64(), Some(1), "row 2 wraps line 1");
        let offset = continuation["visual_offset"].as_u64().unwrap() as usize;
        let text_cells = (continuation["end_col"].as_u64().unwrap()
            - continuation["start_col"].as_u64().unwrap()) as usize;
        assert_eq!(offset, 4);

        // First segment: text cells are selected. Skip the first cell because
        // the synthetic cursor paints over it after selection rendering.
        assert_eq!(
            editor.test_render_cell_bg(content_start + 1, 1).unwrap(),
            Some(selection_bg)
        );
        // Continuation row: virtual indent cells are not selected...
        for x in 0..offset {
            assert_ne!(
                editor.test_render_cell_bg(content_start + x, 2).unwrap(),
                Some(selection_bg),
                "virtual indent cell {x} must not be highlighted"
            );
        }
        // ...but the wrapped text right after it is, end to end.
        assert_eq!(
            editor
                .test_render_cell_bg(content_start + offset, 2)
                .unwrap(),
            Some(selection_bg)
        );
        assert_eq!(
            editor
                .test_render_cell_bg(content_start + offset + text_cells - 1, 2)
                .unwrap(),
            Some(selection_bg)
        );
        // And the area past the wrapped text stays unhighlighted.
        assert_ne!(
            editor
                .test_render_cell_bg(content_start + offset + text_cells, 2)
                .unwrap(),
            Some(selection_bg)
        );
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
        let content_start = layout["content_start"].as_u64().unwrap() as usize;
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
    fn higher_priority_plugin_decoration_styles_blank_lines() {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(None, "fn main() {\n\n    let x = 1;\n}".to_string());
        let mut editor =
            Editor::with_size(lsp, 30, 8, config, Theme::default(), vec![buffer]).unwrap();
        editor.test_disable_terminal_output();
        let layout = editor.plugin_viewport_layout_payload();
        let content_start = layout["content_start"].as_u64().unwrap() as usize;
        let base_color = Color::Rgb {
            r: 80,
            g: 80,
            b: 80,
        };
        let active_color = Color::Rgb {
            r: 220,
            g: 220,
            b: 220,
        };

        editor.decoration_manager.set(
            "guides".to_string(),
            vec![
                crate::plugin::Decoration {
                    buffer_index: Some(0),
                    anchor: crate::plugin::DecorationAnchor::Column,
                    line: 1,
                    column: 0,
                    text: "│   │   ".to_string(),
                    style: Style {
                        fg: Some(base_color),
                        ..Style::default()
                    },
                    priority: 1,
                    repeat_linebreak: true,
                    only_whitespace: true,
                },
                crate::plugin::Decoration {
                    buffer_index: Some(0),
                    anchor: crate::plugin::DecorationAnchor::Column,
                    line: 1,
                    column: 4,
                    text: "│".to_string(),
                    style: Style {
                        fg: Some(active_color),
                        ..Style::default()
                    },
                    priority: 1024,
                    repeat_linebreak: true,
                    only_whitespace: true,
                },
            ],
        );

        let mut render_buffer = RenderBuffer::new(30, 8, &Style::default());
        editor.render(&mut render_buffer).unwrap();

        let guide = &render_buffer.cells[30 + content_start + 4];
        assert_eq!(guide.c, '│');
        assert_eq!(guide.style.fg, Some(active_color));
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
    fn completion_response_filter_converts_utf16_position_before_slicing() {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(None, "😀 config.as".to_string());
        let mut editor =
            Editor::with_size(lsp, 60, 12, config, Theme::default(), vec![buffer]).unwrap();
        editor.cx = "😀 config.as".graphemes(true).count();
        let response = ResponseMessage {
            id: 1,
            result: serde_json::Value::Null,
            request: Some(crate::lsp::Request::new(
                "textDocument/completion",
                serde_json::json!({ "position": { "line": 0, "character": 10 } }),
            )),
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

        let payload = editor
            .plugin_document_symbols_payload(
                &response,
                &PendingDocumentSymbols {
                    plugin_request_id: RequestId::from_raw(1),
                    buffer_index: 0,
                    revision: 0,
                },
            )
            .unwrap();
        let symbols = payload["symbols"].as_array().unwrap();

        assert_eq!(payload["ok"], true);
        assert_eq!(payload["file"], "/tmp/project/src/app.ts");
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0]["id"], "root:0:App");
        assert_eq!(symbols[0]["parent_id"], serde_json::Value::Null);
        assert_eq!(symbols[0]["name"], "App");
        assert_eq!(symbols[0]["kind_name"], "Struct");
        assert_eq!(symbols[0]["depth"], 0);
        assert_eq!(symbols[0]["selection_range"]["start"]["character"], 7);
        assert_eq!(symbols[1]["name"], "render");
        assert_eq!(symbols[1]["id"], "root:0:App:0:render");
        assert_eq!(symbols[1]["parent_id"], "root:0:App");
        assert_eq!(symbols[1]["kind_name"], "Method");
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

        let payload = editor
            .plugin_document_symbols_payload(
                &response,
                &PendingDocumentSymbols {
                    plugin_request_id: RequestId::from_raw(1),
                    buffer_index: 0,
                    revision: 0,
                },
            )
            .unwrap();
        let symbols = payload["symbols"].as_array().unwrap();

        assert_eq!(payload["file"], "/tmp/project/src/index.ts");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0]["name"], "build");
        assert_eq!(symbols[0]["detail"], "tools");
        assert_eq!(symbols[0]["kind_name"], "Function");
        assert_eq!(symbols[0]["file"], "/tmp/project/src/build.ts");
        assert_eq!(symbols[0]["selection_range"]["start"]["line"], 4);
    }

    #[test]
    fn workspace_symbols_payload_accepts_symbol_information() {
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
                "workspace/symbol",
                serde_json::json!({ "query": "build" }),
            )),
        };

        let payload = editor.plugin_workspace_symbols_payload(&response).unwrap();
        let symbols = payload["symbols"].as_array().unwrap();

        assert_eq!(payload["ok"], true);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0]["name"], "build");
        assert_eq!(symbols[0]["detail"], "tools");
        assert_eq!(symbols[0]["kind_name"], "Function");
        assert_eq!(symbols[0]["file"], "/tmp/project/src/build.ts");
        assert_eq!(symbols[0]["selection_range"]["start"]["line"], 4);
    }

    #[test]
    fn references_payload_normalizes_locations_and_request_origin() {
        let editor = test_editor(40, 10);
        let response = ResponseMessage {
            id: 1,
            result: serde_json::json!([
                {
                    "uri": "file:///tmp/project/src/main.rs",
                    "range": {
                        "start": { "line": 3, "character": 4 },
                        "end": { "line": 3, "character": 8 }
                    }
                },
                {
                    "uri": "file:///tmp/project/src/lib.rs",
                    "range": {
                        "start": { "line": 7, "character": 1 },
                        "end": { "line": 7, "character": 5 }
                    }
                }
            ]),
            request: Some(crate::lsp::Request::new(
                "textDocument/references",
                serde_json::json!({
                    "textDocument": { "uri": "file:///tmp/project/src/main.rs" },
                    "position": { "line": 3, "character": 5 },
                    "context": { "includeDeclaration": true }
                }),
            )),
        };

        let payload = editor.plugin_references_payload(&response).unwrap();
        let references = payload["references"].as_array().unwrap();

        assert_eq!(payload["ok"], true);
        assert_eq!(payload["file"], "/tmp/project/src/main.rs");
        assert_eq!(payload["position"]["line"], 3);
        assert_eq!(payload["position"]["character"], 5);
        assert_eq!(references.len(), 2);
        assert_eq!(references[0]["file"], "/tmp/project/src/main.rs");
        assert_eq!(references[1]["file"], "/tmp/project/src/lib.rs");
        assert_eq!(references[1]["range"]["start"]["line"], 7);
    }

    #[test]
    fn null_workspace_symbols_and_references_become_empty_lists() {
        let editor = test_editor(40, 10);
        let workspace_response = ResponseMessage {
            id: 1,
            result: Value::Null,
            request: Some(crate::lsp::Request::new(
                "workspace/symbol",
                serde_json::json!({ "query": "" }),
            )),
        };
        let references_response = ResponseMessage {
            id: 2,
            result: Value::Null,
            request: Some(crate::lsp::Request::new(
                "textDocument/references",
                serde_json::json!({
                    "textDocument": { "uri": "file:///tmp/project/src/main.rs" },
                    "position": { "line": 0, "character": 0 },
                    "context": { "includeDeclaration": true }
                }),
            )),
        };

        assert_eq!(
            editor
                .plugin_workspace_symbols_payload(&workspace_response)
                .unwrap()["symbols"],
            serde_json::json!([])
        );
        assert_eq!(
            editor
                .plugin_references_payload(&references_response)
                .unwrap()["references"],
            serde_json::json!([])
        );
    }

    #[test]
    fn workspace_symbol_timeout_resolves_pending_plugin_request() {
        let mut editor = test_editor(40, 10);
        editor
            .pending_plugin_workspace_symbols
            .insert(42, RequestId::from_raw(7));
        let message = InboundMessage::RequestError {
            id: 42,
            error: crate::lsp::LspError::RequestTimeout(std::time::Duration::from_secs(30)),
        };

        let action = editor.handle_lsp_message(&message, Some("workspace/symbol".to_string()));

        assert!(matches!(
            action,
            Some(Action::ResolvePluginRequest(request_id, payload))
                if request_id == 7
                    && payload["ok"] == false
                    && payload["error"].as_str().is_some_and(|error| error.contains("timed out"))
        ));
        assert!(editor.pending_plugin_workspace_symbols.is_empty());
    }

    #[test]
    fn non_plugin_lsp_timeout_sets_last_error() {
        let mut editor = test_editor(40, 10);
        let message = InboundMessage::RequestError {
            id: 42,
            error: crate::lsp::LspError::RequestTimeout(std::time::Duration::from_secs(30)),
        };

        let action = editor.handle_lsp_message(&message, Some("textDocument/hover".to_string()));

        assert!(action.is_none());
        assert!(editor
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("timed out")));
    }

    #[test]
    fn empty_hover_array_is_ignored() {
        let mut editor = test_editor(40, 10);
        let message = InboundMessage::Message(ResponseMessage {
            id: 42,
            result: serde_json::json!([]),
            request: Some(crate::lsp::Request::new(
                "textDocument/hover",
                serde_json::json!({}),
            )),
        });

        let action = editor.handle_lsp_message(&message, Some("textDocument/hover".to_string()));

        assert!(action.is_none());
    }

    fn lsp_test_editor(buffers: Vec<Buffer>) -> Editor {
        let mut config = Config::default();
        config.lsp.enabled = false;
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(lsp, 60, 12, config, Theme::default(), buffers).unwrap();
        editor.test_disable_terminal_output();
        editor
    }

    fn lsp_edit(start: (usize, usize), end: (usize, usize), text: &str) -> LspTextEdit {
        LspTextEdit {
            range: Range {
                start: crate::lsp::Position {
                    line: start.0,
                    character: start.1,
                },
                end: crate::lsp::Position {
                    line: end.0,
                    character: end.1,
                },
            },
            new_text: text.to_string(),
        }
    }

    #[test]
    fn formatting_response_creates_revision_checked_workspace_edit() {
        let path = std::env::temp_dir().join("red format café #1.rs");
        let mut editor = lsp_test_editor(vec![Buffer::new(
            Some(path.to_string_lossy().into_owned()),
            "👋   value".to_string(),
        )]);
        let uri = editor.buffers[0].uri().unwrap().unwrap();
        let revision = editor.buffers[0].revision();
        editor.pending_lsp_edit_requests.insert(
            41,
            PendingLspEdit {
                buffer_id: editor.buffers[0].id(),
                revision,
                uri: uri.clone(),
            },
        );
        let message = InboundMessage::Message(ResponseMessage {
            id: 41,
            result: serde_json::to_value(vec![lsp_edit((0, 2), (0, 5), " ")]).unwrap(),
            request: Some(crate::lsp::Request::new(
                "textDocument/formatting",
                serde_json::json!({ "textDocument": { "uri": uri } }),
            )),
        });

        let action =
            editor.handle_lsp_message(&message, Some("textDocument/formatting".to_string()));

        assert!(matches!(
            action,
            Some(Action::ApplyLspWorkspaceEdit {
                documents,
                expected_revisions,
                command: None,
                label,
            }) if documents.len() == 1
                && documents[0].uri.contains("caf%C3%A9%20%231.rs")
                && expected_revisions == vec![(documents[0].uri.clone(), revision)]
                && label == "format document"
        ));
        assert!(editor.pending_lsp_edit_requests.is_empty());
    }

    #[test]
    fn stale_formatting_response_is_rejected_without_an_action() {
        let path = std::env::temp_dir().join("red-stale-format.rs");
        let mut editor = lsp_test_editor(vec![Buffer::new(
            Some(path.to_string_lossy().into_owned()),
            "value".to_string(),
        )]);
        let uri = editor.buffers[0].uri().unwrap().unwrap();
        let revision = editor.buffers[0].revision();
        editor.pending_lsp_edit_requests.insert(
            42,
            PendingLspEdit {
                buffer_id: editor.buffers[0].id(),
                revision,
                uri: uri.clone(),
            },
        );
        editor.buffers[0]
            .replace_range_raw(TextRange::insertion(TextPosition::new(0, 0)), "dirty ");
        let message = InboundMessage::Message(ResponseMessage {
            id: 42,
            result: serde_json::to_value(vec![lsp_edit((0, 0), (0, 5), "formatted")]).unwrap(),
            request: Some(crate::lsp::Request::new(
                "textDocument/formatting",
                serde_json::json!({ "textDocument": { "uri": uri } }),
            )),
        });

        let action =
            editor.handle_lsp_message(&message, Some("textDocument/formatting".to_string()));

        assert!(action.is_none());
        assert_eq!(editor.buffers[0].contents(), "dirty value");
        assert!(editor
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("stale")));
    }

    #[test]
    fn code_action_response_opens_picker_with_safe_embedded_edit() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".red-root"), "").unwrap();
        let path = root.path().join("red code-action café.rs");
        let mut editor = workspace_lsp_test_editor(root.path(), &path, "let unused = 1;");
        let uri = editor.buffers[0].uri().unwrap().unwrap();
        let revision = editor.buffers[0].revision();
        editor.pending_lsp_edit_requests.insert(
            43,
            PendingLspEdit {
                buffer_id: editor.buffers[0].id(),
                revision,
                uri: uri.clone(),
            },
        );
        let message = InboundMessage::Message(ResponseMessage {
            id: 43,
            result: serde_json::json!([{
                "title": "Remove unused binding",
                "kind": "quickfix",
                "isPreferred": true,
                "edit": { "changes": { (uri.clone()): [{
                    "range": { "start": { "line": 0, "character": 4 }, "end": { "line": 0, "character": 13 } },
                    "newText": "_"
                }] } }
            }]),
            request: Some(crate::lsp::Request::new(
                "textDocument/codeAction",
                serde_json::json!({ "textDocument": { "uri": uri } }),
            )),
        });

        let action =
            editor.handle_lsp_message(&message, Some("textDocument/codeAction".to_string()));

        assert!(matches!(action, Some(Action::ShowDialog)));
        let selected = editor
            .current_dialog
            .as_mut()
            .unwrap()
            .handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )));
        let Some(KeyAction::Multiple(actions)) = selected else {
            panic!("expected code-action picker selection");
        };
        assert!(matches!(
            actions.last(),
            Some(Action::ApplyLspWorkspaceEdit {
                documents,
                expected_revisions,
                command: None,
                label,
            }) if documents.len() == 1
                && expected_revisions == &vec![(documents[0].uri.clone(), revision)]
                && label == "Remove unused binding"
        ));
    }

    #[test]
    fn signature_help_response_opens_documentation_dialog() {
        let mut editor = lsp_test_editor(vec![Buffer::new(None, "call(".to_string())]);
        let message = InboundMessage::Message(ResponseMessage {
            id: 44,
            result: serde_json::json!({
                "activeSignature": 0,
                "signatures": [{
                    "label": "call(value: String)",
                    "documentation": { "kind": "markdown", "value": "Example documentation" }
                }]
            }),
            request: Some(crate::lsp::Request::new(
                "textDocument/signatureHelp",
                serde_json::json!({}),
            )),
        });

        let action =
            editor.handle_lsp_message(&message, Some("textDocument/signatureHelp".to_string()));

        assert!(matches!(action, Some(Action::ShowDialog)));
        assert!(editor.current_dialog.is_some());
    }

    #[tokio::test]
    async fn workspace_edit_applies_atomically_to_dirty_unicode_buffers() {
        let first_path = std::env::temp_dir().join("red workspace café #1.rs");
        let second_path = std::env::temp_dir().join("red workspace second.rs");
        let mut first = Buffer::new(
            Some(first_path.to_string_lossy().into_owned()),
            "👋 value\r\n".to_string(),
        );
        first.replace_range_raw(TextRange::insertion(TextPosition::new(0, 0)), "dirty ");
        let second = Buffer::new(
            Some(second_path.to_string_lossy().into_owned()),
            "second value".to_string(),
        );
        let mut editor = lsp_test_editor(vec![first, second]);
        let first_uri = editor.buffers[0].uri().unwrap().unwrap();
        let second_uri = editor.buffers[1].uri().unwrap().unwrap();
        let revisions = vec![
            (first_uri.clone(), editor.buffers[0].revision()),
            (second_uri.clone(), editor.buffers[1].revision()),
        ];
        let documents = vec![
            LspDocumentEdit {
                uri: first_uri,
                version: None,
                edits: vec![lsp_edit((0, 9), (0, 14), "first")],
            },
            LspDocumentEdit {
                uri: second_uri,
                version: None,
                edits: vec![lsp_edit((0, 7), (0, 12), "updated")],
            },
        ];

        editor
            .test_execute_production_action(Action::ApplyLspWorkspaceEdit {
                documents,
                expected_revisions: revisions,
                command: None,
                label: "apply quick fix".to_string(),
            })
            .await
            .unwrap();

        assert_eq!(editor.buffers[0].contents(), "dirty 👋 first\r\n");
        assert_eq!(editor.buffers[1].contents(), "second updated");
        assert!(editor.buffers[0].is_dirty());
        assert!(editor.buffers[1].is_dirty());
        assert!(matches!(
            editor.buffers[0]
                .undo_history
                .latest_transaction()
                .map(|tx| &tx.origin),
            Some(EditOrigin::Lsp { .. })
        ));
        assert!(matches!(
            editor.buffers[1]
                .undo_history
                .latest_transaction()
                .map(|tx| &tx.origin),
            Some(EditOrigin::Lsp { .. })
        ));
    }

    #[tokio::test]
    async fn invalid_or_stale_workspace_edit_never_partially_mutates_buffers() {
        let first_path = std::env::temp_dir().join("red workspace first.rs");
        let second_path = std::env::temp_dir().join("red workspace emoji.rs");
        let mut editor = lsp_test_editor(vec![
            Buffer::new(
                Some(first_path.to_string_lossy().into_owned()),
                "first".to_string(),
            ),
            Buffer::new(
                Some(second_path.to_string_lossy().into_owned()),
                "👋 second".to_string(),
            ),
        ]);
        let first_uri = editor.buffers[0].uri().unwrap().unwrap();
        let second_uri = editor.buffers[1].uri().unwrap().unwrap();
        let revisions = vec![
            (first_uri.clone(), editor.buffers[0].revision()),
            (second_uri.clone(), editor.buffers[1].revision()),
        ];
        let documents = vec![
            LspDocumentEdit {
                uri: first_uri.clone(),
                version: None,
                edits: vec![lsp_edit((0, 0), (0, 5), "updated")],
            },
            LspDocumentEdit {
                uri: second_uri.clone(),
                version: None,
                edits: vec![lsp_edit((0, 1), (0, 2), "broken")],
            },
        ];

        editor
            .test_execute_production_action(Action::ApplyLspWorkspaceEdit {
                documents,
                expected_revisions: revisions.clone(),
                command: None,
                label: "invalid edit".to_string(),
            })
            .await
            .unwrap();

        assert_eq!(editor.buffers[0].contents(), "first");
        assert_eq!(editor.buffers[1].contents(), "👋 second");
        assert!(editor
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("UTF-16")));

        editor.last_error = None;
        editor
            .test_execute_production_action(Action::ApplyLspWorkspaceEdit {
                documents: vec![
                    LspDocumentEdit {
                        uri: first_uri.clone(),
                        version: None,
                        edits: vec![lsp_edit((0, 0), (0, 5), "updated")],
                    },
                    LspDocumentEdit {
                        uri: second_uri.clone(),
                        version: None,
                        edits: vec![
                            lsp_edit((0, 3), (0, 6), "one"),
                            lsp_edit((0, 5), (0, 8), "two"),
                        ],
                    },
                ],
                expected_revisions: revisions.clone(),
                command: None,
                label: "overlapping edit".to_string(),
            })
            .await
            .unwrap();

        assert_eq!(editor.buffers[0].contents(), "first");
        assert_eq!(editor.buffers[1].contents(), "👋 second");
        assert!(editor
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("overlap")));

        editor.last_error = None;
        editor.buffers[1]
            .replace_range_raw(TextRange::insertion(TextPosition::new(0, 0)), "dirty ");
        editor
            .test_execute_production_action(Action::ApplyLspWorkspaceEdit {
                documents: vec![
                    LspDocumentEdit {
                        uri: first_uri,
                        version: None,
                        edits: vec![lsp_edit((0, 0), (0, 5), "updated")],
                    },
                    LspDocumentEdit {
                        uri: second_uri,
                        version: None,
                        edits: vec![lsp_edit((0, 0), (0, 0), "updated ")],
                    },
                ],
                expected_revisions: revisions,
                command: None,
                label: "stale edit".to_string(),
            })
            .await
            .unwrap();

        assert_eq!(editor.buffers[0].contents(), "first");
        assert_eq!(editor.buffers[1].contents(), "dirty 👋 second");
        assert!(editor
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("stale")));
    }

    #[tokio::test]
    async fn versioned_workspace_edit_is_rejected_when_lsp_version_is_unknown() {
        let path = std::env::temp_dir().join("red workspace versioned.rs");
        let mut editor = lsp_test_editor(vec![Buffer::new(
            Some(path.to_string_lossy().into_owned()),
            "value".to_string(),
        )]);
        let uri = editor.buffers[0].uri().unwrap().unwrap();
        let revision = editor.buffers[0].revision();

        editor
            .test_execute_production_action(Action::ApplyLspWorkspaceEdit {
                documents: vec![LspDocumentEdit {
                    uri: uri.clone(),
                    version: Some(7),
                    edits: vec![lsp_edit((0, 0), (0, 5), "updated")],
                }],
                expected_revisions: vec![(uri, revision)],
                command: None,
                label: "versioned edit".to_string(),
            })
            .await
            .unwrap();

        assert_eq!(editor.buffers[0].contents(), "value");
        assert!(editor
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("version is stale")));
    }

    fn workspace_lsp_test_editor(root: &Path, path: &Path, contents: &str) -> Editor {
        let mut config = Config::default();
        config.lsp.enabled = true;
        config.lsp.servers = HashMap::from([(
            "rust".to_string(),
            crate::config::LanguageServerConfig {
                command: root
                    .join("missing-lsp-server")
                    .to_string_lossy()
                    .into_owned(),
                args: Vec::new(),
                language_id: "rust".to_string(),
                file_extensions: vec!["rs".to_string()],
                documents: Vec::new(),
                root_markers: vec![".red-root".to_string()],
                env: HashMap::new(),
                initialization_options: None,
                workspace_name: None,
            },
        )]);
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            lsp,
            60,
            12,
            config,
            Theme::default(),
            vec![Buffer::new(
                Some(path.to_string_lossy().into_owned()),
                contents.to_string(),
            )],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor
    }

    #[test]
    fn rename_response_creates_an_atomic_workspace_edit_action() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".red-root"), "").unwrap();
        let path = root.path().join("red rename café.rs");
        let mut editor = workspace_lsp_test_editor(root.path(), &path, "let old_name = 1;");
        let uri = editor.buffers[0].uri().unwrap().unwrap();
        let revision = editor.buffers[0].revision();
        editor.pending_lsp_edit_requests.insert(
            51,
            PendingLspEdit {
                buffer_id: editor.buffers[0].id(),
                revision,
                uri: uri.clone(),
            },
        );
        let response = InboundMessage::Message(ResponseMessage {
            id: 51,
            result: serde_json::json!({
                "changes": { (uri.clone()): [{
                    "range": { "start": { "line": 0, "character": 4 }, "end": { "line": 0, "character": 12 } },
                    "newText": "new_name"
                }] }
            }),
            request: Some(crate::lsp::Request::new(
                "textDocument/rename",
                serde_json::json!({ "textDocument": { "uri": uri } }),
            )),
        });

        let action = editor.handle_lsp_message(&response, Some("textDocument/rename".to_string()));

        assert!(matches!(
            action,
            Some(Action::ApplyLspWorkspaceEdit { documents, expected_revisions, label, .. })
                if documents.len() == 1
                    && expected_revisions == vec![(documents[0].uri.clone(), revision)]
                    && label == "rename symbol"
        ));
    }

    #[test]
    fn stale_rename_response_is_rejected_before_mutation() {
        let path = std::env::temp_dir().join("red-stale-rename.rs");
        let mut editor = lsp_test_editor(vec![Buffer::new(
            Some(path.to_string_lossy().into_owned()),
            "old_name".to_string(),
        )]);
        let uri = editor.buffers[0].uri().unwrap().unwrap();
        editor.pending_lsp_edit_requests.insert(
            52,
            PendingLspEdit {
                buffer_id: editor.buffers[0].id(),
                revision: editor.buffers[0].revision(),
                uri: uri.clone(),
            },
        );
        editor.buffers[0]
            .replace_range_raw(TextRange::insertion(TextPosition::new(0, 0)), "dirty ");
        let response = InboundMessage::Message(ResponseMessage {
            id: 52,
            result: serde_json::json!({ "changes": { (uri): [] } }),
            request: Some(crate::lsp::Request::new(
                "textDocument/rename",
                serde_json::json!({}),
            )),
        });

        assert!(editor
            .handle_lsp_message(&response, Some("textDocument/rename".to_string()))
            .is_none());
        assert_eq!(editor.buffers[0].contents(), "dirty old_name");
        assert!(editor
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("stale")));
    }

    #[test]
    fn rename_response_cannot_escape_the_originating_language_server_workspace() {
        let root_a = tempfile::tempdir().unwrap();
        let root_b = tempfile::tempdir().unwrap();
        std::fs::write(root_a.path().join(".red-root"), "").unwrap();
        std::fs::write(root_b.path().join(".red-root"), "").unwrap();
        let source = root_a.path().join("source.rs");
        let target = root_b.path().join("target.rs");
        std::fs::write(&source, "source").unwrap();
        std::fs::write(&target, "target").unwrap();
        let mut editor = workspace_lsp_test_editor(root_a.path(), &source, "source");
        let source_uri = editor.buffers[0].uri().unwrap().unwrap();
        let target_uri = crate::lsp::file_uri(&target).unwrap();
        let revision = editor.buffers[0].revision();
        editor.pending_lsp_edit_requests.insert(
            59,
            PendingLspEdit {
                buffer_id: editor.buffers[0].id(),
                revision,
                uri: source_uri.clone(),
            },
        );
        editor
            .pending_lsp_revision_snapshots
            .insert(59, vec![(source_uri.clone(), revision)]);
        let response = InboundMessage::Message(ResponseMessage {
            id: 59,
            result: serde_json::json!({
                "changes": { (target_uri): [{
                    "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 6 } },
                    "newText": "owned"
                }] }
            }),
            request: Some(crate::lsp::Request::new(
                "textDocument/rename",
                serde_json::json!({ "textDocument": { "uri": source_uri } }),
            )),
        });

        assert!(editor
            .handle_lsp_message(&response, Some("textDocument/rename".to_string()))
            .is_none());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "target");
        assert!(editor
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("outside its originating workspace")));
    }

    #[test]
    fn diagnostics_uri_aliases_normalize_to_the_current_buffer_uri() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("src")).unwrap();
        let file = root.path().join("main.rs");
        let editor_file = file.to_string_lossy().into_owned();
        let mut editor = lsp_test_editor(vec![Buffer::new(Some(editor_file), "value".to_string())]);
        let uri = crate::lsp::file_uri(&file).unwrap();
        let alias = format!(
            "{}/src/../main.rs",
            crate::lsp::file_uri(root.path())
                .unwrap()
                .trim_end_matches('/')
        );
        let diagnostic: Diagnostic = serde_json::from_value(serde_json::json!({
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 5 } },
            "message": "problem"
        }))
        .unwrap();

        editor.add_diagnostics(Some(&alias), &[diagnostic]);

        assert!(editor.diagnostics.contains_key(&uri));
        assert!(!editor.diagnostics.contains_key(&alias));
    }

    #[tokio::test]
    async fn delayed_rename_cannot_overwrite_an_unversioned_secondary_buffer_edited_after_request()
    {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".red-root"), "").unwrap();
        let first_path = root.path().join("red-rename-source.rs");
        let second_path = root.path().join("red-rename-secondary.rs");
        std::fs::create_dir(root.path().join("src")).unwrap();
        let mut editor = workspace_lsp_test_editor(root.path(), &first_path, "let source = 1;");
        editor.buffers.push(Buffer::new(
            Some(second_path.to_string_lossy().into_owned()),
            "let source = 2;".to_string(),
        ));
        let first_uri = editor.buffers[0].uri().unwrap().unwrap();
        let second_uri = editor.buffers[1].uri().unwrap().unwrap();
        let first_revision = editor.buffers[0].revision();
        let second_revision = editor.buffers[1].revision();
        editor.pending_lsp_edit_requests.insert(
            53,
            PendingLspEdit {
                buffer_id: editor.buffers[0].id(),
                revision: first_revision,
                uri: first_uri.clone(),
            },
        );
        editor.pending_lsp_revision_snapshots.insert(
            53,
            vec![
                (first_uri.clone(), first_revision),
                (second_uri.clone(), second_revision),
            ],
        );
        editor.buffers[1]
            .replace_range_raw(TextRange::insertion(TextPosition::new(0, 0)), "dirty ");
        let aliased_second_uri = format!(
            "{}/src/../red-rename-secondary.rs",
            crate::lsp::file_uri(root.path())
                .unwrap()
                .trim_end_matches('/')
        );
        let response = InboundMessage::Message(ResponseMessage {
            id: 53,
            result: serde_json::json!({
                "changes": {
                    (first_uri): [{
                        "range": { "start": { "line": 0, "character": 4 }, "end": { "line": 0, "character": 10 } },
                        "newText": "renamed"
                    }],
                    (aliased_second_uri): [{
                        "range": { "start": { "line": 0, "character": 4 }, "end": { "line": 0, "character": 10 } },
                        "newText": "renamed"
                    }]
                }
            }),
            request: Some(crate::lsp::Request::new(
                "textDocument/rename",
                serde_json::json!({}),
            )),
        });
        let action = editor
            .handle_lsp_message(&response, Some("textDocument/rename".to_string()))
            .expect("rename should produce an action with the request-time revisions");

        editor.test_execute_production_action(action).await.unwrap();

        assert_eq!(editor.buffers[0].contents(), "let source = 1;");
        assert_eq!(editor.buffers[1].contents(), "dirty let source = 2;");
        assert!(editor
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("stale")));
        assert!(editor.pending_lsp_revision_snapshots.is_empty());
    }

    #[test]
    fn server_workspace_edit_returns_an_ordered_action_and_invalid_input_returns_failure() {
        let path = std::env::temp_dir().join("red-server-edit.rs");
        let mut editor = lsp_test_editor(vec![Buffer::new(
            Some(path.to_string_lossy().into_owned()),
            "value".to_string(),
        )]);
        let uri = editor.buffers[0].uri().unwrap().unwrap();
        let message = InboundMessage::ServerRequest(LspServerRequest {
            id: serde_json::json!("server-1"),
            method: "workspace/applyEdit".to_string(),
            params: serde_json::json!({
                "label": "Update value",
                "edit": { "changes": { (uri.clone()): [{
                    "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 5 } },
                    "newText": "updated"
                }] } }
            }),
            source: Some("rust:/tmp/project".to_string()),
        });

        let action = editor.handle_lsp_message(&message, None);

        assert!(matches!(
            action,
            Some(Action::ApplyLspWorkspaceEditOperations { operations, response: Some(response), label, .. })
                if operations.len() == 1
                    && response.id == serde_json::json!("server-1")
                    && label == "Update value"
        ));

        let invalid = InboundMessage::ServerRequest(LspServerRequest {
            id: serde_json::json!(2),
            method: "workspace/applyEdit".to_string(),
            params: serde_json::json!({ "label": "Broken" }),
            source: Some("rust:/tmp/project".to_string()),
        });
        assert!(matches!(
            editor.handle_lsp_message(&invalid, None),
            Some(Action::RespondLspWorkspaceEdit { applied: false, failure_reason: Some(reason), .. })
                if reason.contains("missing edit")
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unopened_workspace_edit_becomes_an_attributed_dirty_buffer_without_writing_disk() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".red-root"), "").unwrap();
        let active = root.path().join("active.rs");
        let closed = root.path().join("closed café.rs");
        std::fs::write(&active, "fn active() {}\n").unwrap();
        std::fs::write(&closed, "👋 old\r\n").unwrap();
        let mut editor = workspace_lsp_test_editor(root.path(), &active, "fn active() {}\n");
        let closed_uri = crate::lsp::file_uri(&closed).unwrap();
        let operations = workspace_edit_operations(&serde_json::json!({
            "changes": { (closed_uri): [{
                "range": { "start": { "line": 0, "character": 3 }, "end": { "line": 0, "character": 6 } },
                "newText": "new"
            }] }
        }))
        .unwrap();

        editor
            .test_execute_production_action(Action::ApplyLspWorkspaceEditOperations {
                operations,
                expected_revisions: Vec::new(),
                command: None,
                label: "update closed file".to_string(),
                response: None,
                save_after_uri: None,
                save_as: None,
                save_previous_file: None,
            })
            .await
            .unwrap();

        assert_eq!(std::fs::read_to_string(&closed).unwrap(), "👋 old\r\n");
        assert_eq!(editor.buffers.len(), 2);
        assert_eq!(editor.buffers[1].contents(), "👋 new\r\n");
        assert!(editor.buffers[1].is_dirty());
        assert!(matches!(
            editor.buffers[1]
                .undo_history
                .latest_transaction()
                .map(|transaction| &transaction.origin),
            Some(EditOrigin::Lsp { .. })
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn resource_rename_updates_an_open_buffer_and_moves_disk_without_losing_unsaved_text() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".red-root"), "").unwrap();
        let old = root.path().join("old.rs");
        let new = root.path().join("new.rs");
        std::fs::write(&old, "disk contents\n").unwrap();
        let mut editor = workspace_lsp_test_editor(root.path(), &old, "unsaved contents\n");
        editor.buffers[0].dirty = true;
        let operations = workspace_edit_operations(&serde_json::json!({
            "documentChanges": [{ "kind": "rename", "oldUri": crate::lsp::file_uri(&old).unwrap(), "newUri": crate::lsp::file_uri(&new).unwrap() }]
        }))
        .unwrap();

        editor
            .test_execute_production_action(Action::ApplyLspWorkspaceEditOperations {
                operations,
                expected_revisions: Vec::new(),
                command: None,
                label: "rename file".to_string(),
                response: None,
                save_after_uri: None,
                save_as: None,
                save_previous_file: None,
            })
            .await
            .unwrap();

        assert!(!old.exists());
        assert_eq!(std::fs::read_to_string(&new).unwrap(), "disk contents\n");
        assert_eq!(
            editor.buffers[0].file.as_deref(),
            Some(new.to_str().unwrap())
        );
        assert_eq!(editor.buffers[0].contents(), "unsaved contents\n");
        assert!(editor.buffers[0].is_dirty());
    }

    #[cfg(not(unix))]
    #[tokio::test]
    async fn unopened_and_resource_workspace_edits_fail_closed_without_mutating_editor_or_disk() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".red-root"), "").unwrap();
        let active = root.path().join("active.rs");
        let closed = root.path().join("closed café.rs");
        let renamed = root.path().join("renamed.rs");
        std::fs::write(&active, "disk contents\n").unwrap();
        std::fs::write(&closed, "👋 old\r\n").unwrap();
        let mut editor = workspace_lsp_test_editor(root.path(), &active, "unsaved contents\n");
        editor.buffers[0].dirty = true;
        let operations = workspace_edit_operations(&serde_json::json!({
            "changes": { (crate::lsp::file_uri(&closed).unwrap()): [{
                "range": { "start": { "line": 0, "character": 3 }, "end": { "line": 0, "character": 6 } },
                "newText": "new"
            }] }
        }))
        .unwrap();

        editor
            .test_execute_production_action(Action::ApplyLspWorkspaceEditOperations {
                operations,
                expected_revisions: Vec::new(),
                command: None,
                label: "update closed file".to_string(),
                response: None,
                save_after_uri: None,
                save_as: None,
                save_previous_file: None,
            })
            .await
            .unwrap();

        assert_eq!(std::fs::read_to_string(&closed).unwrap(), "👋 old\r\n");
        assert_eq!(editor.buffers.len(), 1);
        assert_eq!(editor.buffers[0].contents(), "unsaved contents\n");
        assert!(editor.buffers[0].is_dirty());
        assert!(editor
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("no-follow filesystem support")));

        editor.last_error = None;
        let operations = workspace_edit_operations(&serde_json::json!({
            "documentChanges": [{
                "kind": "rename",
                "oldUri": crate::lsp::file_uri(&active).unwrap(),
                "newUri": crate::lsp::file_uri(&renamed).unwrap()
            }]
        }))
        .unwrap();
        editor
            .test_execute_production_action(Action::ApplyLspWorkspaceEditOperations {
                operations,
                expected_revisions: Vec::new(),
                command: None,
                label: "rename file".to_string(),
                response: None,
                save_after_uri: None,
                save_as: None,
                save_previous_file: None,
            })
            .await
            .unwrap();

        assert_eq!(std::fs::read_to_string(&active).unwrap(), "disk contents\n");
        assert!(!renamed.exists());
        assert_eq!(
            editor.buffers[0].file.as_deref(),
            Some(active.to_str().unwrap())
        );
        assert_eq!(editor.buffers[0].contents(), "unsaved contents\n");
        assert!(editor.buffers[0].is_dirty());
        assert!(editor
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("no-follow filesystem support")));
    }

    #[tokio::test]
    async fn format_on_save_response_applies_the_edit_before_writing_and_does_not_recurse() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("format.rs");
        std::fs::write(&path, "value   \n").unwrap();
        let mut editor = lsp_test_editor(vec![Buffer::new(
            Some(path.to_string_lossy().into_owned()),
            "value   \n".to_string(),
        )]);
        editor.config.lsp.format_on_save = true;
        let uri = editor.buffers[0].uri().unwrap().unwrap();
        editor.pending_lsp_edit_requests.insert(
            61,
            PendingLspEdit {
                buffer_id: editor.buffers[0].id(),
                revision: editor.buffers[0].revision(),
                uri: uri.clone(),
            },
        );
        editor.pending_lsp_format_saves.insert(
            61,
            PendingLspFormatSave {
                save_as: None,
                previous_file: None,
            },
        );
        let response = InboundMessage::Message(ResponseMessage {
            id: 61,
            result: serde_json::json!([{
                "range": { "start": { "line": 0, "character": 5 }, "end": { "line": 0, "character": 8 } },
                "newText": ""
            }]),
            request: Some(crate::lsp::Request::new(
                "textDocument/formatting",
                serde_json::json!({}),
            )),
        });
        let action = editor
            .handle_lsp_message(&response, Some("textDocument/formatting".to_string()))
            .expect("format response should produce an action");

        editor.test_execute_production_action(action).await.unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "value\n");
        assert_eq!(editor.buffers[0].contents(), "value\n");
        assert!(!editor.buffers[0].is_dirty());
        assert!(editor.pending_lsp_edit_requests.is_empty());
        assert!(editor.pending_lsp_format_saves.is_empty());
    }

    #[tokio::test]
    async fn format_on_save_can_create_a_named_unsaved_file_after_applying_edits() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("new.rs");
        let mut editor = lsp_test_editor(vec![Buffer::new(
            Some(path.to_string_lossy().into_owned()),
            "value   \n".to_string(),
        )]);
        editor.config.lsp.format_on_save = true;
        let uri = editor.buffers[0].uri().unwrap().unwrap();
        editor.pending_lsp_edit_requests.insert(
            64,
            PendingLspEdit {
                buffer_id: editor.buffers[0].id(),
                revision: editor.buffers[0].revision(),
                uri: uri.clone(),
            },
        );
        editor.pending_lsp_format_saves.insert(
            64,
            PendingLspFormatSave {
                save_as: None,
                previous_file: None,
            },
        );
        let response = InboundMessage::Message(ResponseMessage {
            id: 64,
            result: serde_json::json!([{
                "range": { "start": { "line": 0, "character": 5 }, "end": { "line": 0, "character": 8 } },
                "newText": ""
            }]),
            request: Some(crate::lsp::Request::new(
                "textDocument/formatting",
                serde_json::json!({ "textDocument": { "uri": uri } }),
            )),
        });
        let action = editor
            .handle_lsp_message(&response, Some("textDocument/formatting".to_string()))
            .expect("format response should produce an action");

        editor.test_execute_production_action(action).await.unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "value\n");
        assert_eq!(editor.buffers[0].contents(), "value\n");
        assert!(!editor.buffers[0].is_dirty());
    }

    #[cfg(unix)]
    fn delayed_formatter_editor(
        root: &Path,
        buffer: Buffer,
    ) -> (Editor, PathBuf, PathBuf, PathBuf) {
        let server = root.join("formatter.py");
        let ready = root.join("initialize-ready");
        let release = root.join("initialize-release");
        let events = root.join("lsp-events");
        std::fs::write(
            &server,
            r#"
import json
import pathlib
import sys
import time

ready, release, events = map(pathlib.Path, sys.argv[1:4])

def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        key, value = line.decode().split(":", 1)
        headers[key.lower()] = value.strip()
    return json.loads(sys.stdin.buffer.read(int(headers["content-length"])))

def send(message):
    body = json.dumps(message).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    sys.stdout.buffer.flush()

def record(message):
    method = message.get("method", "response")
    uri = message.get("params", {}).get("textDocument", {}).get("uri", "")
    with events.open("a") as output:
        output.write(method + " " + uri + "\n")

initialize = read_message()
record(initialize)
ready.write_text("ready")
deadline = time.monotonic() + 10
while not release.exists():
    if time.monotonic() > deadline:
        sys.exit(2)
    time.sleep(0.005)
if release.read_text() == "fail":
    send({"jsonrpc": "2.0", "id": initialize["id"], "error": {"code": -32002, "message": "initialize failed"}})
    sys.exit(0)
send({"jsonrpc": "2.0", "id": initialize["id"], "result": {"capabilities": {"documentFormattingProvider": True}}})

while True:
    message = read_message()
    if message is None:
        break
    record(message)
    method = message.get("method")
    if method == "textDocument/formatting":
        send({"jsonrpc": "2.0", "id": message["id"], "result": [{"range": {"start": {"line": 0, "character": 5}, "end": {"line": 0, "character": 8}}, "newText": ""}]})
        break
"#,
        )
        .unwrap();
        std::fs::write(root.join(".red-root"), "").unwrap();
        let mut config = Config::default();
        config.lsp.enabled = true;
        config.lsp.format_on_save = true;
        config.lsp.servers = HashMap::from([(
            "formatter".to_string(),
            crate::config::LanguageServerConfig {
                command: "python3".to_string(),
                args: vec![
                    server.to_string_lossy().into_owned(),
                    ready.to_string_lossy().into_owned(),
                    release.to_string_lossy().into_owned(),
                    events.to_string_lossy().into_owned(),
                ],
                language_id: "rust".to_string(),
                file_extensions: vec!["rs".to_string()],
                documents: Vec::new(),
                root_markers: vec![".red-root".to_string()],
                env: HashMap::new(),
                initialization_options: None,
                workspace_name: None,
            },
        )]);
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor =
            Editor::with_size(lsp, 60, 12, config, Theme::default(), vec![buffer]).unwrap();
        editor.test_disable_terminal_output();
        (editor, ready, release, events)
    }

    #[cfg(unix)]
    async fn wait_for_formatter_initialize(ready: &Path) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !ready.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("formatter initialize request should arrive");
    }

    #[cfg(unix)]
    fn cross_language_formatter_editor(root: &Path, buffer: Buffer) -> (Editor, PathBuf) {
        let server = root.join("cross-language-formatter.py");
        let events = root.join("cross-language-lsp-events");
        std::fs::write(
            &server,
            r#"
import json
import pathlib
import sys

language, events = sys.argv[1], pathlib.Path(sys.argv[2])

def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        key, value = line.decode().split(":", 1)
        headers[key.lower()] = value.strip()
    return json.loads(sys.stdin.buffer.read(int(headers["content-length"])))

def send(message):
    body = json.dumps(message).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method", "response")
    uri = message.get("params", {}).get("textDocument", {}).get("uri", "")
    with events.open("a") as output:
        output.write(language + " " + method + " " + uri + "\n")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"capabilities": {"documentFormattingProvider": True}}})
    elif method == "textDocument/formatting":
        send({"jsonrpc": "2.0", "id": message["id"], "result": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 12}}, "newText": language + " formatted"}]})
        break
"#,
        )
        .unwrap();
        std::fs::write(root.join(".red-root"), "").unwrap();
        let mut config = Config::default();
        config.lsp.enabled = true;
        config.lsp.format_on_save = true;
        config.lsp.servers = [("rust", "rs"), ("python", "py")]
            .into_iter()
            .map(|(language, extension)| {
                (
                    language.to_string(),
                    crate::config::LanguageServerConfig {
                        command: "python3".to_string(),
                        args: vec![
                            server.to_string_lossy().into_owned(),
                            language.to_string(),
                            events.to_string_lossy().into_owned(),
                        ],
                        language_id: language.to_string(),
                        file_extensions: vec![extension.to_string()],
                        documents: Vec::new(),
                        root_markers: vec![".red-root".to_string()],
                        env: HashMap::new(),
                        initialization_options: None,
                        workspace_name: None,
                    },
                )
            })
            .collect();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor =
            Editor::with_size(lsp, 60, 12, config, Theme::default(), vec![buffer]).unwrap();
        editor.test_disable_terminal_output();
        (editor, events)
    }

    #[cfg(unix)]
    async fn complete_delayed_format_save(editor: &mut Editor, path: &Path, expected: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((message, method)) = editor.lsp.recv_response().await.unwrap() {
                    if let Some(action) = editor.handle_lsp_message(&message, method) {
                        editor.test_execute_production_action(action).await.unwrap();
                    }
                }
                if std::fs::read_to_string(path).ok().as_deref() == Some(expected) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("formatted save should complete");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn format_on_save_waits_for_delayed_lsp_initialize_and_saves_once() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("delayed.rs");
        std::fs::write(&path, "disk value\n").unwrap();
        let file = path.to_string_lossy().into_owned();
        let (mut editor, ready, release, events) = delayed_formatter_editor(
            root.path(),
            Buffer::new(Some(file.clone()), "value   \n".to_string()),
        );

        editor
            .test_execute_production_action(Action::Save)
            .await
            .unwrap();
        wait_for_formatter_initialize(&ready).await;
        let disk_before_release = std::fs::read_to_string(&path).unwrap();
        editor
            .test_execute_production_action(Action::Save)
            .await
            .unwrap();
        std::fs::write(&release, "release").unwrap();

        assert_eq!(disk_before_release, "disk value\n");
        assert!(editor
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("already pending")));
        complete_delayed_format_save(&mut editor, &path, "value\n").await;

        let uri = crate::lsp::file_uri(&path).unwrap();
        let events = std::fs::read_to_string(events).unwrap();
        assert_eq!(events.matches("textDocument/formatting ").count(), 1);
        assert!(events.contains(&format!("textDocument/didOpen {uri}")));
        assert!(events.contains(&format!("textDocument/formatting {uri}")));
        assert_eq!(editor.buffers[0].contents(), "value\n");
        assert!(!editor.buffers[0].is_dirty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn first_save_as_uses_the_target_formatter_after_delayed_initialize() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("first-save.rs");
        let file = path.to_string_lossy().into_owned();
        let (mut editor, ready, release, events) =
            delayed_formatter_editor(root.path(), Buffer::new(None, "value   \n".to_string()));

        editor
            .test_execute_production_action(Action::SaveAs(file.clone()))
            .await
            .unwrap();
        wait_for_formatter_initialize(&ready).await;
        let existed_before_release = path.exists();
        std::fs::write(&release, "release").unwrap();

        assert!(!existed_before_release);
        complete_delayed_format_save(&mut editor, &path, "value\n").await;

        let uri = crate::lsp::file_uri(&path).unwrap();
        let events = std::fs::read_to_string(events).unwrap();
        assert_eq!(events.matches("textDocument/formatting ").count(), 1);
        assert!(events.contains(&format!("textDocument/didOpen {uri}")));
        assert!(events.contains(&format!("textDocument/formatting {uri}")));
        assert_eq!(editor.buffers[0].file.as_deref(), Some(file.as_str()));
        assert_eq!(editor.buffers[0].contents(), "value\n");
        assert!(!editor.buffers[0].is_dirty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn named_save_as_uses_the_destination_language_formatter() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.rs");
        let target = root.path().join("output.py");
        std::fs::write(&source, "disk source\n").unwrap();
        let (mut editor, events) = cross_language_formatter_editor(
            root.path(),
            Buffer::new(
                Some(source.to_string_lossy().into_owned()),
                "source value\n".to_string(),
            ),
        );

        editor.ensure_current_buffer_lsp_opened().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((message, method)) = editor.lsp.recv_response().await.unwrap() {
                    let initialized = method.as_deref() == Some("initialize");
                    editor.handle_lsp_message(&message, method);
                    if initialized {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("source formatter initialize response should arrive");

        editor
            .test_execute_production_action(Action::SaveAs(target.to_string_lossy().into_owned()))
            .await
            .unwrap();
        complete_delayed_format_save(&mut editor, &target, "python formatted\n").await;

        let source_uri = crate::lsp::file_uri(&source).unwrap();
        let target_uri = crate::lsp::file_uri(&target).unwrap();
        let events = std::fs::read_to_string(events).unwrap();
        assert!(events.contains(&format!("rust textDocument/didOpen {source_uri}")));
        assert!(events.contains(&format!("rust textDocument/didClose {source_uri}")));
        assert!(!events.contains("rust textDocument/formatting "));
        assert!(events.contains(&format!("python textDocument/didOpen {target_uri}")));
        assert!(events.contains(&format!("python textDocument/formatting {target_uri}")));
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "disk source\n");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "python formatted\n"
        );
        assert_eq!(
            editor.current_buffer().file.as_deref(),
            Some(target.to_string_lossy().as_ref())
        );
        assert_eq!(editor.current_buffer().contents(), "python formatted\n");
        assert!(!editor.current_buffer().is_dirty());
    }

    #[tokio::test]
    async fn format_on_save_refuses_an_already_open_save_as_destination() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.rs");
        let target = root.path().join("target.py");
        std::fs::write(&source, "disk source\n").unwrap();
        std::fs::write(&target, "disk target\n").unwrap();
        let source_file = source.to_string_lossy().into_owned();
        let target_file = target.to_string_lossy().into_owned();
        let mut editor = lsp_test_editor(vec![
            Buffer::new(Some(target_file.clone()), "open target\n".to_string()),
            Buffer::new(Some(source_file.clone()), "unsaved source\n".to_string()),
        ]);
        editor.config.lsp.format_on_save = true;
        editor.current_buffer_index = 1;
        editor.mode = Mode::Insert;
        editor.begin_transaction("insert");

        editor
            .test_execute_production_action(Action::SaveAs(target_file))
            .await
            .unwrap();

        assert!(editor.transaction_active());
        assert_eq!(
            editor.current_buffer().file.as_deref(),
            Some(source_file.as_str())
        );
        assert_eq!(editor.current_buffer().contents(), "unsaved source\n");
        assert_eq!(editor.buffers[0].contents(), "open target\n");
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "disk source\n");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "disk target\n");
        assert!(editor.pending_lsp_edit_requests.is_empty());
        assert!(editor.pending_lsp_format_saves.is_empty());
        assert!(editor
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("destination is already open")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn format_on_save_falls_back_when_lsp_initialization_failed_before_save() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("failed-initialize.rs");
        std::fs::write(&path, "disk value\n").unwrap();
        let file = path.to_string_lossy().into_owned();
        let (mut editor, ready, release, events) = delayed_formatter_editor(
            root.path(),
            Buffer::new(Some(file), "value   \n".to_string()),
        );

        editor.ensure_current_buffer_lsp_opened().await.unwrap();
        wait_for_formatter_initialize(&ready).await;
        std::fs::write(&release, "fail").unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((message, method)) = editor.lsp.recv_response().await.unwrap() {
                    let initialized = method.as_deref() == Some("initialize");
                    editor.handle_lsp_message(&message, method);
                    if initialized {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("formatter initialize failure should arrive");

        editor
            .test_execute_production_action(Action::Save)
            .await
            .expect("failed formatter initialization should fall back to an unformatted save");

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "value   \n");
        assert_eq!(editor.buffers[0].contents(), "value   \n");
        assert!(!editor.buffers[0].is_dirty());
        assert!(editor.pending_lsp_edit_requests.is_empty());
        assert!(editor.pending_lsp_format_saves.is_empty());
        assert!(editor.last_error.as_deref().is_some_and(|error| {
            error.contains("format-on-save unavailable; saved unformatted")
                && error.contains("language server initialization has failed")
        }));
        let events = std::fs::read_to_string(events).unwrap();
        assert!(!events.contains("textDocument/formatting "));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn format_on_save_falls_back_for_new_documents_after_lsp_initialization_failed() {
        for save_as in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let initial = root.path().join("initial.rs");
            std::fs::write(&initial, "initial value\n").unwrap();
            let (mut editor, ready, release, events) = delayed_formatter_editor(
                root.path(),
                Buffer::new(
                    Some(initial.to_string_lossy().into_owned()),
                    "initial value\n".to_string(),
                ),
            );

            editor.ensure_current_buffer_lsp_opened().await.unwrap();
            wait_for_formatter_initialize(&ready).await;
            std::fs::write(&release, "fail").unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Some((message, method)) = editor.lsp.recv_response().await.unwrap() {
                        let initialized = method.as_deref() == Some("initialize");
                        editor.handle_lsp_message(&message, method);
                        if initialized {
                            break;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("formatter initialize failure should arrive");

            let target = root.path().join(if save_as {
                "first-save.rs"
            } else {
                "second.rs"
            });
            if !save_as {
                std::fs::write(&target, "disk value\n").unwrap();
            }
            editor.buffers.push(Buffer::new(
                (!save_as).then(|| target.to_string_lossy().into_owned()),
                "value   \n".to_string(),
            ));
            editor.current_buffer_index = editor.buffers.len() - 1;

            let action = if save_as {
                Action::SaveAs(target.to_string_lossy().into_owned())
            } else {
                Action::Save
            };
            editor
                .test_execute_production_action(action)
                .await
                .expect("failed formatter initialization should not abort a new-document save");

            assert_eq!(std::fs::read_to_string(&target).unwrap(), "value   \n");
            assert_eq!(editor.current_buffer().contents(), "value   \n");
            assert!(!editor.current_buffer().is_dirty());
            assert!(editor.pending_lsp_edit_requests.is_empty());
            assert!(editor.pending_lsp_format_saves.is_empty());
            assert!(editor.last_error.as_deref().is_some_and(|error| {
                error.contains("format-on-save unavailable; saved unformatted")
                    && error.contains("language server initialization has failed")
            }));
            let target_uri = crate::lsp::file_uri(&target).unwrap();
            let events = std::fs::read_to_string(events).unwrap();
            assert!(!events.contains("textDocument/formatting "));
            assert!(!events.contains(&format!("textDocument/didOpen {target_uri}")));
        }
    }

    #[test]
    fn formatter_unavailable_timeout_and_initialization_failure_fall_back_to_the_pending_save() {
        for failure in ["method-not-found", "timeout", "initialization"] {
            let path = std::env::temp_dir().join(format!("red-format-fallback-{failure}.rs"));
            let mut editor = lsp_test_editor(vec![Buffer::new(
                Some(path.to_string_lossy().into_owned()),
                "value\n".to_string(),
            )]);
            let uri = editor.buffers[0].uri().unwrap().unwrap();
            editor.pending_lsp_edit_requests.insert(
                66,
                PendingLspEdit {
                    buffer_id: editor.buffers[0].id(),
                    revision: editor.buffers[0].revision(),
                    uri: uri.clone(),
                },
            );
            editor.pending_lsp_format_saves.insert(
                66,
                PendingLspFormatSave {
                    save_as: None,
                    previous_file: None,
                },
            );
            let message = match failure {
                "timeout" => InboundMessage::RequestError {
                    id: 66,
                    error: crate::lsp::LspError::RequestTimeout(std::time::Duration::from_secs(30)),
                },
                "initialization" => InboundMessage::RequestError {
                    id: 66,
                    error: crate::lsp::LspError::ProtocolError(
                        "language server initialization or transport failed".to_string(),
                    ),
                },
                _ => InboundMessage::Error(crate::lsp::ResponseError {
                    id: Some(66),
                    code: -32601,
                    message: "method not found".to_string(),
                    data: None,
                }),
            };

            let action =
                editor.handle_lsp_message(&message, Some("textDocument/formatting".to_string()));

            assert!(matches!(
                action,
                Some(Action::CompleteLspFormatSave { uri: candidate, save_as: None, warning: Some(_), .. }) if candidate == uri
            ));
            assert!(editor.pending_lsp_edit_requests.is_empty());
            assert!(editor.pending_lsp_format_saves.is_empty());
        }
    }

    #[tokio::test]
    async fn async_format_fallback_saves_the_original_buffer_and_preserves_its_warning() {
        for failure in [
            "method-not-found",
            "timeout",
            "protocol",
            "response",
            "request",
        ] {
            let root = tempfile::tempdir().unwrap();
            let target = root.path().join("target.py");
            std::fs::write(&target, "disk target\n").unwrap();
            let target_file = target.to_string_lossy().into_owned();
            let target_uri = crate::lsp::file_uri(&target).unwrap();
            let mut editor = lsp_test_editor(vec![
                Buffer::new(Some(target_file.clone()), "decoy buffer\n".to_string()),
                Buffer::new(Some(target_file.clone()), "unsaved source\n".to_string()),
            ]);
            let buffer_id = editor.buffers[1].id();
            editor.pending_lsp_edit_requests.insert(
                71,
                PendingLspEdit {
                    buffer_id,
                    revision: editor.buffers[1].revision(),
                    uri: target_uri.clone(),
                },
            );
            editor.pending_lsp_format_saves.insert(
                71,
                PendingLspFormatSave {
                    save_as: (failure == "response" || failure == "request")
                        .then(|| target_file.clone()),
                    previous_file: Some(root.path().join("source.rs").to_string_lossy().into()),
                },
            );
            let message = match failure {
                "method-not-found" => InboundMessage::Error(crate::lsp::ResponseError {
                    id: Some(71),
                    code: -32601,
                    message: "method not found".to_string(),
                    data: None,
                }),
                "timeout" => InboundMessage::RequestError {
                    id: 71,
                    error: crate::lsp::LspError::RequestTimeout(Duration::from_secs(30)),
                },
                "protocol" => InboundMessage::RequestError {
                    id: 71,
                    error: crate::lsp::LspError::ProtocolError("transport failed".to_string()),
                },
                "response" => InboundMessage::Error(crate::lsp::ResponseError {
                    id: Some(71),
                    code: -32603,
                    message: "formatter failed".to_string(),
                    data: None,
                }),
                _ => InboundMessage::RequestError {
                    id: 71,
                    error: crate::lsp::LspError::ServerError("formatter failed".to_string()),
                },
            };

            let action = editor
                .handle_lsp_message(&message, Some("textDocument/formatting".to_string()))
                .expect("formatter failure should complete the pending save");
            editor.test_execute_production_action(action).await.unwrap();

            assert_eq!(
                std::fs::read_to_string(&target).unwrap(),
                "unsaved source\n"
            );
            assert_eq!(editor.buffers[0].contents(), "decoy buffer\n");
            assert_eq!(editor.buffers[1].contents(), "unsaved source\n");
            assert!(!editor.buffers[1].is_dirty());
            assert!(editor.pending_lsp_edit_requests.is_empty());
            assert!(editor.pending_lsp_format_saves.is_empty());
            assert!(editor.last_error.as_deref().is_some_and(|error| {
                error.contains("format-on-save unavailable; saved unformatted")
            }));
        }
    }

    #[tokio::test]
    async fn stale_and_malformed_save_as_format_responses_restore_the_source_identity() {
        for failure in ["stale", "malformed", "invalid-edit", "stale-request"] {
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("source.rs");
            let target = root.path().join("target.py");
            std::fs::write(&source, "disk source\n").unwrap();
            let source_file = source.to_string_lossy().into_owned();
            let target_file = target.to_string_lossy().into_owned();
            let target_uri = crate::lsp::file_uri(&target).unwrap();
            let mut editor = lsp_test_editor(vec![Buffer::new(
                Some(target_file.clone()),
                "unsaved source\n".to_string(),
            )]);
            let revision = editor.buffers[0].revision();
            editor.pending_lsp_edit_requests.insert(
                72,
                PendingLspEdit {
                    buffer_id: editor.buffers[0].id(),
                    revision,
                    uri: target_uri,
                },
            );
            editor.pending_lsp_format_saves.insert(
                72,
                PendingLspFormatSave {
                    save_as: Some(target_file),
                    previous_file: Some(source_file.clone()),
                },
            );
            if failure == "stale" || failure == "stale-request" {
                editor.buffers[0]
                    .replace_range_raw(TextRange::insertion(TextPosition::new(0, 0)), "new ");
            }
            let message = if failure == "stale-request" {
                InboundMessage::RequestError {
                    id: 72,
                    error: crate::lsp::LspError::ServerError("formatter failed".to_string()),
                }
            } else {
                InboundMessage::Message(ResponseMessage {
                    id: 72,
                    result: match failure {
                        "malformed" => serde_json::json!({ "not": "edits" }),
                        "invalid-edit" => serde_json::json!([{
                            "range": {
                                "start": { "line": 99, "character": 0 },
                                "end": { "line": 99, "character": 1 }
                            },
                            "newText": "invalid"
                        }]),
                        _ => serde_json::json!([]),
                    },
                    request: Some(crate::lsp::Request::new(
                        "textDocument/formatting",
                        serde_json::json!({}),
                    )),
                })
            };

            let action = editor
                .handle_lsp_message(&message, Some("textDocument/formatting".to_string()))
                .expect("failed Save As formatter response should restore the source identity");
            assert!(matches!(action, Action::RestoreLspFormatSave { .. }));
            editor.test_execute_production_action(action).await.unwrap();

            assert!(!target.exists());
            assert_eq!(std::fs::read_to_string(&source).unwrap(), "disk source\n");
            assert_eq!(
                editor.current_buffer().file.as_deref(),
                Some(source_file.as_str())
            );
            assert!(editor
                .current_buffer()
                .contents()
                .contains("unsaved source"));
            assert!(editor.pending_lsp_edit_requests.is_empty());
            assert!(editor.pending_lsp_format_saves.is_empty());
            assert!(editor
                .last_error
                .as_deref()
                .is_some_and(|error| error.contains("Save As cancelled")));
        }
    }

    #[tokio::test]
    async fn failed_format_workspace_edit_completes_an_unformatted_save_as_with_warning() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target.py");
        let target_file = target.to_string_lossy().into_owned();
        let target_uri = crate::lsp::file_uri(&target).unwrap();
        let mut editor = lsp_test_editor(vec![Buffer::new(
            Some(target_file.clone()),
            "unsaved source\n".to_string(),
        )]);

        editor
            .test_execute_production_action(Action::ApplyLspWorkspaceEditOperations {
                operations: vec![LspWorkspaceEditOperation::Document {
                    edit: LspDocumentEdit {
                        uri: target_uri.clone(),
                        version: None,
                        edits: vec![LspTextEdit {
                            range: Range {
                                start: crate::lsp::Position {
                                    line: 99,
                                    character: 0,
                                },
                                end: crate::lsp::Position {
                                    line: 99,
                                    character: 1,
                                },
                            },
                            new_text: "invalid".to_string(),
                        }],
                    },
                }],
                expected_revisions: vec![(target_uri.clone(), editor.buffers[0].revision())],
                command: None,
                label: "format document".to_string(),
                response: None,
                save_after_uri: Some(target_uri),
                save_as: Some(target_file),
                save_previous_file: Some(root.path().join("source.rs").to_string_lossy().into()),
            })
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "unsaved source\n"
        );
        assert!(editor.last_error.as_deref().is_some_and(|error| {
            error.contains("format-on-save unavailable; saved unformatted")
                && error.contains("invalid LSP workspace edit")
        }));
    }

    #[tokio::test]
    async fn formatted_save_as_write_failure_restores_the_original_file_identity() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.rs");
        let target = root.path().join("target-directory");
        std::fs::write(&source, "disk source\n").unwrap();
        std::fs::create_dir(&target).unwrap();
        let source_file = source.to_string_lossy().into_owned();
        let target_file = target.to_string_lossy().into_owned();
        let target_uri = crate::lsp::file_uri(&target).unwrap();
        let mut editor = lsp_test_editor(vec![Buffer::new(
            Some(target_file.clone()),
            "value   \n".to_string(),
        )]);
        let revision = editor.buffers[0].revision();

        editor
            .test_execute_production_action(Action::ApplyLspWorkspaceEditOperations {
                operations: vec![LspWorkspaceEditOperation::Document {
                    edit: LspDocumentEdit {
                        uri: target_uri.clone(),
                        version: None,
                        edits: vec![LspTextEdit {
                            range: Range {
                                start: crate::lsp::Position {
                                    line: 0,
                                    character: 5,
                                },
                                end: crate::lsp::Position {
                                    line: 0,
                                    character: 8,
                                },
                            },
                            new_text: String::new(),
                        }],
                    },
                }],
                expected_revisions: vec![(target_uri.clone(), revision)],
                command: None,
                label: "format document".to_string(),
                response: None,
                save_after_uri: Some(target_uri),
                save_as: Some(target_file),
                save_previous_file: Some(source_file.clone()),
            })
            .await
            .unwrap();

        assert_eq!(editor.current_buffer().contents(), "value\n");
        assert_eq!(
            editor.current_buffer().file.as_deref(),
            Some(source_file.as_str())
        );
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "disk source\n");
        assert!(target.is_dir());
        assert!(editor.last_error.is_some());
    }

    #[tokio::test]
    async fn no_server_does_not_leave_manual_format_code_action_or_rename_pending() {
        let path = std::env::temp_dir().join("red-no-lsp.rs");
        let mut editor = lsp_test_editor(vec![Buffer::new(
            Some(path.to_string_lossy().into_owned()),
            "value".to_string(),
        )]);

        for action in [
            Action::FormatDocument,
            Action::CodeAction,
            Action::RenameSymbol("renamed".to_string()),
        ] {
            editor.test_execute_production_action(action).await.unwrap();
            assert!(editor.pending_lsp_edit_requests.is_empty());
            assert!(editor.pending_lsp_revision_snapshots.is_empty());
            assert!(editor
                .last_error
                .as_deref()
                .is_some_and(|error| error.contains("no language server")));
        }
    }

    #[test]
    fn stale_format_on_save_response_cancels_the_pending_save() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("stale-format.rs");
        std::fs::write(&path, "value\n").unwrap();
        let mut editor = lsp_test_editor(vec![Buffer::new(
            Some(path.to_string_lossy().into_owned()),
            "value\n".to_string(),
        )]);
        let uri = editor.buffers[0].uri().unwrap().unwrap();
        editor.pending_lsp_edit_requests.insert(
            62,
            PendingLspEdit {
                buffer_id: editor.buffers[0].id(),
                revision: editor.buffers[0].revision(),
                uri,
            },
        );
        editor.pending_lsp_format_saves.insert(
            62,
            PendingLspFormatSave {
                save_as: None,
                previous_file: None,
            },
        );
        editor.buffers[0]
            .replace_range_raw(TextRange::insertion(TextPosition::new(0, 0)), "dirty ");
        let response = InboundMessage::Message(ResponseMessage {
            id: 62,
            result: serde_json::json!([]),
            request: Some(crate::lsp::Request::new(
                "textDocument/formatting",
                serde_json::json!({}),
            )),
        });

        assert!(editor
            .handle_lsp_message(&response, Some("textDocument/formatting".to_string()))
            .is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "value\n");
        assert_eq!(editor.buffers[0].contents(), "dirty value\n");
        assert!(editor.pending_lsp_format_saves.is_empty());
        assert!(editor
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("stale")));
    }

    #[test]
    fn failed_format_on_save_response_cancels_the_save_without_writing() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("failed-format.rs");
        std::fs::write(&path, "disk value\n").unwrap();
        let mut editor = lsp_test_editor(vec![Buffer::new(
            Some(path.to_string_lossy().into_owned()),
            "unsaved value\n".to_string(),
        )]);
        let uri = editor.buffers[0].uri().unwrap().unwrap();
        editor.pending_lsp_edit_requests.insert(
            63,
            PendingLspEdit {
                buffer_id: editor.buffers[0].id(),
                revision: editor.buffers[0].revision(),
                uri,
            },
        );
        editor.pending_lsp_format_saves.insert(
            63,
            PendingLspFormatSave {
                save_as: None,
                previous_file: None,
            },
        );
        let message = InboundMessage::Error(crate::lsp::ResponseError {
            id: Some(63),
            code: -32603,
            message: "formatter failed".to_string(),
            data: None,
        });

        assert!(editor
            .handle_lsp_message(&message, Some("textDocument/formatting".to_string()))
            .is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "disk value\n");
        assert_eq!(editor.buffers[0].contents(), "unsaved value\n");
        assert!(editor.pending_lsp_edit_requests.is_empty());
        assert!(editor.pending_lsp_format_saves.is_empty());
        assert_eq!(editor.last_error.as_deref(), Some("formatter failed"));
    }

    #[test]
    fn retrigger_cancellation_clears_and_resolves_pending_plugin_request() {
        let mut editor = test_editor(40, 10);
        editor
            .pending_plugin_workspace_symbols
            .insert(42, RequestId::from_raw(7));
        let message = InboundMessage::Error(crate::lsp::ResponseError {
            id: Some(42),
            code: -32802,
            message: "server cancelled the request".to_string(),
            data: Some(serde_json::json!({ "retriggerRequest": true })),
        });

        let action = editor.handle_lsp_message(&message, Some("workspace/symbol".to_string()));

        assert!(matches!(action, Some(Action::ResolvePluginRequest(7, _))));
        assert!(!editor.pending_plugin_workspace_symbols.contains_key(&42));
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
        let _guard = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
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
    async fn repeated_motion_coalesces_plugin_cursor_events() {
        let _guard = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(None, "zero\none\ntwo\nthree".to_string());
        let mut editor =
            Editor::with_size(lsp, 40, 10, config, Theme::default(), vec![buffer]).unwrap();
        editor.test_disable_terminal_output();
        let mut render_buffer = RenderBuffer::new(40, 10, &Style::default());
        let mut runtime = Runtime::new();
        install_event_recorder(&mut editor, &mut runtime).await;

        editor.defer_motion_render = true;
        editor
            .execute(&Action::MoveDown, &mut render_buffer, &mut runtime)
            .await
            .unwrap();
        editor
            .execute(&Action::MoveDown, &mut render_buffer, &mut runtime)
            .await
            .unwrap();
        editor.defer_motion_render = false;
        editor
            .flush_deferred_plugin_event(&mut runtime)
            .await
            .unwrap();

        let prints = collect_print_requests();
        assert_eq!(prints, vec!["cursor:MoveDown:0,0->0,2:Normal".to_string()]);
    }

    #[tokio::test]
    async fn search_highlight_and_clear_emit_plugin_events() {
        let _guard = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
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
        let _guard = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
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
    async fn open_location_converts_utf8_bytes_and_reuses_buffers_for_splits() {
        let file =
            std::env::temp_dir().join(format!("red-open-location-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&file, "zero\né needle\n😀 target\n").unwrap();

        let mut editor = test_editor(/*width*/ 40, /*height*/ 10);
        let mut render_buffer =
            RenderBuffer::new(/*width*/ 40, /*height*/ 10, &Style::default());
        let mut runtime = Runtime::new();
        let location = plugin::PluginLocation {
            path: file.to_string_lossy().into_owned(),
            line: 1,
            column: 3,
            column_encoding: plugin::LocationColumnEncoding::Utf8Byte,
        };

        editor
            .execute(
                &Action::OpenLocation(location.clone(), plugin::OpenLocationTarget::Current),
                &mut render_buffer,
                &mut runtime,
            )
            .await
            .unwrap();

        assert_eq!(editor.buffer_line(), 1);
        assert_eq!(editor.cx, 2);
        assert_eq!(editor.buffers.len(), 2);
        assert!(!editor.jump_list.is_empty());

        editor
            .execute(
                &Action::OpenLocation(
                    plugin::PluginLocation {
                        path: file.to_string_lossy().into_owned(),
                        line: 2,
                        column: 3,
                        column_encoding: plugin::LocationColumnEncoding::Utf16,
                    },
                    plugin::OpenLocationTarget::Current,
                ),
                &mut render_buffer,
                &mut runtime,
            )
            .await
            .unwrap();

        assert_eq!(editor.buffer_line(), 2);
        assert_eq!(editor.cx, 2);

        editor
            .execute(
                &Action::OpenLocation(location, plugin::OpenLocationTarget::Horizontal),
                &mut render_buffer,
                &mut runtime,
            )
            .await
            .unwrap();

        assert_eq!(editor.buffers.len(), 2);
        assert_eq!(editor.test_window_count(), 2);

        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn cursor_lsp_position_uses_utf16_code_units() {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(None, "😀 target".to_string());
        let mut editor =
            Editor::with_size(lsp, 40, 10, config, Theme::default(), vec![buffer]).unwrap();
        editor.cx = 2;

        assert_eq!(
            editor.cursor_lsp_position(),
            crate::lsp::Position {
                line: 0,
                character: 3,
            }
        );
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
    async fn command_feedback_renders_after_prompt_closes() {
        for (command, dirty, expected) in [
            (
                "q",
                true,
                "The following buffers have unwritten changes: [No Name]",
            ),
            ("w", true, "No file name"),
            ("xyz", false, "unknown command \"xyz\""),
        ] {
            let mut editor = test_editor(80, 5);
            editor.current_buffer_mut().dirty = dirty;
            editor.mode = Mode::Command;
            editor.command = command.to_string();
            let mut render_buffer = RenderBuffer::new(80, 5, &Style::default());
            let mut runtime = Runtime::new();

            let processed = editor
                .process_editor_event(
                    Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                    &mut render_buffer,
                    &mut runtime,
                    EventRenderMode::Immediate,
                )
                .await
                .unwrap();

            assert!(!processed.quit, "command {command:?} unexpectedly quit");
            assert_eq!(editor.last_error.as_deref(), Some(expected));
            assert_eq!(render_row(&render_buffer, 4).trim_end(), expected);
        }
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
    fn focus_gain_activation_click_preserves_focused_panel() {
        let mut editor = test_editor(40, 10);
        editor.panel_manager.create_panel(
            "tree".to_string(),
            plugin::PanelConfig {
                side: plugin::PanelSide::Left,
                width: 10,
                title: None,
            },
        );
        assert!(editor.panel_manager.focus_panel("tree"));
        let mut render_buffer = RenderBuffer::new(40, 10, &Style::default());

        editor
            .handle_focus_event(&Event::FocusLost, &mut render_buffer)
            .unwrap();
        editor
            .handle_focus_event(&Event::FocusGained, &mut render_buffer)
            .unwrap();
        let activation_click = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 20,
            row: 1,
            modifiers: KeyModifiers::NONE,
        });

        assert!(editor.consume_reactivation_click(&activation_click));
        assert_eq!(editor.panel_manager.focused_panel_id(), Some("tree"));

        editor.suppress_reactivation_click = false;
        editor.handle_event(&activation_click).unwrap();
        assert_eq!(editor.panel_manager.focused_panel_id(), None);
    }

    #[test]
    fn focused_panel_repaints_the_editor_cursor_cell() {
        let mut editor = test_editor(40, 10);
        let mut render_buffer = RenderBuffer::new(40, 10, &Style::default());
        editor.render(&mut render_buffer).unwrap();
        let (x, y) = editor.render_cursor_position().unwrap();
        let cursor_index = y * render_buffer.width + x;
        let focused_style = render_buffer.cells[cursor_index].style.clone();

        editor.panel_manager.create_panel(
            "tree".to_string(),
            plugin::PanelConfig {
                side: plugin::PanelSide::Left,
                width: 10,
                title: None,
            },
        );
        editor.apply_panel_layout();
        assert!(editor.panel_manager.focus_panel("tree"));
        editor.render(&mut render_buffer).unwrap();

        assert_eq!(editor.render_cursor_position(), None);
        assert_ne!(
            render_buffer.cells[cursor_index].style, focused_style,
            "focusing a panel should repaint the synthetic editor cursor away"
        );
    }

    #[test]
    fn picker_cursor_position_survives_focus_loss_and_gain() {
        let mut editor = test_editor(40, 10);
        let picker = Picker::new(
            Some("Themes".to_string()),
            &editor,
            &["Lackluster".to_string()],
            Some(1),
        );
        editor.current_dialog = Some(Box::new(picker));
        let mut render_buffer = RenderBuffer::new(40, 10, &Style::default());
        editor.render(&mut render_buffer).unwrap();
        let cursor = editor.render_cursor_position();

        editor
            .handle_focus_event(&Event::FocusLost, &mut render_buffer)
            .unwrap();
        editor
            .handle_focus_event(&Event::FocusGained, &mut render_buffer)
            .unwrap();

        assert!(editor.is_focused);
        assert_eq!(editor.render_cursor_position(), cursor);
        assert!(!editor.uses_synthetic_block_cursor());
    }

    #[tokio::test]
    async fn theme_changed_plugins_receive_the_updated_editor_info_snapshot() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        let mut editor = test_editor(40, 10);
        let mut runtime = Runtime::new();
        editor
            .refresh_plugin_snapshots(&mut runtime, true, true, true)
            .unwrap();
        install_theme_probe(&mut editor, &mut runtime).await;
        let mut render_buffer = RenderBuffer::new(40, 10, &Style::default());

        editor
            .execute(
                &Action::PreviewTheme("lackluster.json".to_string()),
                &mut render_buffer,
                &mut runtime,
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::Action(Action::Print(theme)) => {
                assert_eq!(theme, "lackluster");
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[test]
    fn synthetic_cursor_keeps_contrast_during_full_and_delta_renders() {
        let config = Config::default();
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(None, "hello".to_string());
        let theme = parse_vscode_theme("themes/kanagawa.json").unwrap();
        let mut editor = Editor::with_size(lsp, 20, 5, config, theme, vec![buffer]).unwrap();
        editor.test_disable_terminal_output();
        let mut render_buffer = RenderBuffer::new(20, 5, &Style::default());

        editor.render(&mut render_buffer).unwrap();
        let (first_x, first_y) = editor.render_cursor_position().unwrap();
        let first_index = first_y * render_buffer.width + first_x;
        let first_cursor_style = render_buffer.cells[first_index].style.clone();
        let editor_bg = editor.theme.style.bg.unwrap();

        assert!(
            crate::color::contrast_ratio(first_cursor_style.bg.unwrap(), editor_bg)
                >= crate::theme::MINIMUM_CURSOR_STATE_CONTRAST
        );
        assert!(
            crate::color::contrast_ratio(
                first_cursor_style.fg.unwrap(),
                first_cursor_style.bg.unwrap()
            ) >= crate::theme::MINIMUM_CURSOR_TEXT_CONTRAST
        );

        editor.cx = 1;
        editor.cursor_goal = CursorGoal::DisplayCol(1);
        editor
            .render_cursor_motion_delta(&mut render_buffer)
            .unwrap();
        let (next_x, next_y) = editor.render_cursor_position().unwrap();
        let next_index = next_y * render_buffer.width + next_x;
        let next_cursor_style = &render_buffer.cells[next_index].style;

        assert_ne!(render_buffer.cells[first_index].style, first_cursor_style);
        assert!(
            crate::color::contrast_ratio(next_cursor_style.bg.unwrap(), editor_bg)
                >= crate::theme::MINIMUM_CURSOR_STATE_CONTRAST
        );
        assert!(
            crate::color::contrast_ratio(
                next_cursor_style.fg.unwrap(),
                next_cursor_style.bg.unwrap()
            ) >= crate::theme::MINIMUM_CURSOR_TEXT_CONTRAST
        );
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
    fn window_bar_reserves_the_first_row_from_gutter_content_and_cursor() {
        let mut editor = test_editor(20, 5);
        install_test_window_bar(&mut editor);
        let mut render_buffer = RenderBuffer::new(20, 5, &Style::default());

        editor.render(&mut render_buffer).unwrap();

        assert!(render_row(&render_buffer, 0).starts_with("chrome"));
        let content_row = render_row(&render_buffer, 1);
        assert!(content_row.contains("1 hello"), "{content_row:?}");
        assert_eq!(editor.render_cursor_position().map(|(_, y)| y), Some(1));
    }

    #[tokio::test]
    async fn line_end_delta_render_does_not_paint_the_cursor_on_window_bar() {
        let mut editor = test_editor(20, 5);
        install_test_window_bar(&mut editor);
        let mut render_buffer = RenderBuffer::new(20, 5, &Style::default());
        let mut runtime = Runtime::new();
        editor.render(&mut render_buffer).unwrap();
        let chrome_before = render_buffer.cells[..render_buffer.width].to_vec();

        editor
            .execute(&Action::MoveToLineEnd, &mut render_buffer, &mut runtime)
            .await
            .unwrap();
        editor
            .render_cursor_motion_delta(&mut render_buffer)
            .unwrap();

        assert_eq!(editor.render_cursor_position().map(|(_, y)| y), Some(1));
        assert_eq!(render_buffer.cells[..render_buffer.width], chrome_before);
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

    #[test]
    fn oversized_repeat_count_saturates_without_panicking() {
        let mut editor = test_editor(20, 5);

        for digit in "99999999999999999999".chars() {
            let event = Event::Key(KeyEvent::new(KeyCode::Char(digit), KeyModifiers::NONE));
            assert!(editor.handle_repeater(&event));
        }

        assert_eq!(editor.repeater, Some(u16::MAX));
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
    fn render_buffer_does_not_place_half_a_wide_grapheme_at_the_edge() {
        let mut buffer = RenderBuffer::new(3, 1, &Style::default());

        buffer.set_text(0, 0, "ab👋", &Style::default());

        assert_eq!(render_text_rows(&buffer)[0], "ab ");
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
