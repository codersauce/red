//! Side-panel models, source-backed text conversations, focus, scrolling, and hit testing.
//!
//! [`PanelManager`] owns plugin panels by stable ID and computes how their requested
//! widths reduce the editor viewport. Row panels contain selectable structured rows;
//! text panels retain source blocks and derive wrapped rendered rows for the current
//! width so streaming appends never destroy the authoritative text.
//!
//! Focus, composer drafts, tail-follow state, scroll offsets, and header hit regions are
//! manager-owned UI state. A plugin may replace content but must use the same panel ID to
//! preserve that lifecycle intentionally.

use std::{
    cell::RefCell,
    collections::{hash_map::DefaultHasher, HashMap},
    hash::{Hash, Hasher},
    ops::Range,
    sync::Arc,
    time::{Duration, Instant},
};

use crossterm::event::{Event, KeyCode, KeyModifiers};
use serde::{
    de::Error as _, ser::SerializeStruct, Deserialize, Deserializer, Serialize, Serializer,
};
use unicode_segmentation::UnicodeSegmentation;

use super::markdown::{
    render_code_lines, render_markdown_lines, wrap_plain_text, RenderedTextLine,
    RenderedTextLineBreak, RenderedTextSpan, TextPanelLineSelection, TextPanelSpanSelection,
    TextPanelSpanStyle,
};
use super::text_link::{TextPanelLink, TextPanelLinkTarget};
use super::TreePanelModel;
use crate::{
    buffer::BufferId,
    color::{blend_color, Color},
    editor::{render_buffer::RenderBuffer, Point, StyleInfo},
    highlighter::Highlighter,
    text_layout::{LayoutOptions, TextLayout},
    theme::{
        SelectionForegroundPriority, Style, SurfaceCardColors, SurfaceCardPalette, SurfacePalette,
        Theme, ThemeStyleSpec,
    },
    ui::{
        first_prompt_line, normalize_prompt_newlines, ActionBar, ActionPriority,
        FollowTailViewport, PromptBuffer, PromptInput, PromptKeyPolicy, UiAction, PROMPT_MAX_BYTES,
    },
    unicode_utils::{
        display_width, fit_display_width, grapheme_to_byte, truncate_display_width,
        truncate_display_width_from_end,
    },
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelSide {
    #[default]
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelSnapshotKind {
    Row,
    Text,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextPanelSnapshotFocus {
    #[default]
    Scrollback,
    Composer,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextPanelComposerSnapshot {
    pub text: String,
    pub cursor: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextPanelSessionSnapshot {
    #[serde(default)]
    pub follow_tail: bool,
    #[serde(default)]
    pub scroll_anchor: Option<usize>,
    #[serde(default)]
    pub cursor: usize,
    #[serde(default)]
    pub focus: TextPanelSnapshotFocus,
    #[serde(default)]
    pub composer: Option<TextPanelComposerSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelSessionSnapshot {
    pub id: String,
    pub kind: PanelSnapshotKind,
    #[serde(default)]
    pub visible: bool,
    #[serde(default)]
    pub z_index: Option<usize>,
    pub side: PanelSide,
    #[serde(default)]
    pub vertical_size: Option<usize>,
    #[serde(default)]
    pub horizontal_size: Option<usize>,
    #[serde(default)]
    pub selected_row_id: Option<String>,
    #[serde(default)]
    pub row_scroll: usize,
    #[serde(default)]
    pub text: Option<TextPanelSessionSnapshot>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelManagerSnapshot {
    #[serde(default)]
    pub panels: Vec<PanelSessionSnapshot>,
    #[serde(default)]
    pub focused: Option<String>,
}

#[derive(Debug, Clone)]
struct PendingPanelRestore {
    snapshot: PanelSessionSnapshot,
    shell_applied: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelConfig {
    #[serde(default)]
    pub side: PanelSide,
    #[serde(default = "default_panel_width")]
    pub width: usize,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub composer: Option<TextPanelComposerConfig>,
    #[serde(default)]
    pub header_actions: Vec<TextPanelHeaderAction>,
    #[serde(default)]
    pub surface: Option<ThemeStyleSpec>,
    #[serde(default)]
    pub border: Option<ThemeStyleSpec>,
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            side: PanelSide::Left,
            width: 30,
            title: None,
            composer: None,
            surface: None,
            border: None,
            header_actions: Vec::new(),
        }
    }
}

fn default_panel_width() -> usize {
    30
}

fn default_composer_rows() -> usize {
    3
}

/// Narrowest docked conversation pane that still reads comfortably.
pub const MIN_READABLE_PANE_WIDTH: usize = 32;
/// Editor columns a docked conversation pane leaves free. When a terminal is too
/// narrow for both, the pane stops docking and covers the editor while focused.
pub const MIN_COMFORTABLE_EDITOR_WIDTH: usize = 40;
/// Largest share of the terminal a conversation pane takes at its default width.
const MAX_DEFAULT_PANE_PERCENT: usize = 45;

/// Preferred docked width of a side panel. Conversation panes (side panels
/// with a composer) are responsive: at their plugin-declared width they take at
/// most 45% of the terminal, and a user-chosen width still leaves the editor
/// [`MIN_COMFORTABLE_EDITOR_WIDTH`] columns. No width or limit goes below
/// [`MIN_READABLE_PANE_WIDTH`]; [`PanelManager::side_panel_widths`] decides
/// which panes fit together and makes the rest compact.
///
/// `user_sized` means the width was chosen by the user (dragged, resized or
/// restored) rather than declared by the plugin.
fn effective_panel_width(config: &PanelConfig, user_sized: bool, terminal_width: usize) -> usize {
    if matches!(config.side, PanelSide::Top | PanelSide::Bottom) {
        return terminal_width;
    }
    if config.composer.is_none() {
        return config.width.min(terminal_width);
    }

    let responsive_limit = if user_sized {
        terminal_width.saturating_sub(MIN_COMFORTABLE_EDITOR_WIDTH + 1)
    } else {
        terminal_width.saturating_mul(MAX_DEFAULT_PANE_PERCENT) / 100
    };
    config
        .width
        .max(MIN_READABLE_PANE_WIDTH)
        .min(responsive_limit.max(MIN_READABLE_PANE_WIDTH))
        .min(terminal_width.saturating_sub(11).max(1))
}

fn effective_panel_height(config: &PanelConfig, available_height: usize) -> usize {
    if matches!(config.side, PanelSide::Top | PanelSide::Bottom) {
        config.width.min(available_height)
    } else {
        available_height
    }
}

/// Optional persistent input area rendered at the bottom of a text panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextPanelComposerConfig {
    #[serde(default)]
    pub placeholder: String,
    /// Minimum input rows; wrapped drafts grow within the available pane.
    #[serde(default = "default_composer_rows")]
    pub rows: usize,
}

/// Optional metadata beside a stable text-panel title. Updating it does not
/// replace the panel, its draft, or its focus and scroll state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextPanelHeaderDetail {
    pub text: String,
    #[serde(default)]
    pub secondary: String,
    /// A shorter complete label for important state, such as a pending model switch.
    #[serde(default)]
    pub compact_text: String,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub shortcut: Option<String>,
}

fn text_panel_action_visible_by_default() -> bool {
    true
}

/// One panel-owned action, optionally rendered as a header button.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextPanelHeaderAction {
    pub id: String,
    #[serde(default)]
    pub key: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_label: Option<String>,
    #[serde(default)]
    pub priority: ActionPriority,
    #[serde(default = "text_panel_action_visible_by_default")]
    pub enabled: bool,
    #[serde(default = "text_panel_action_visible_by_default")]
    pub show_in_header: bool,
}

impl TextPanelHeaderAction {
    fn ui_action(&self) -> UiAction {
        UiAction {
            id: self.id.clone(),
            key: self.key.clone(),
            label: self.label.clone(),
            group: self.group.clone(),
            description: self.description.clone(),
            compact_label: self.compact_label.clone(),
            compact_key: None,
            trigger: None,
            priority: self.priority,
            modes: Vec::new(),
            enabled: self.enabled,
        }
    }

    pub(crate) fn event(&self) -> Option<Event> {
        self.ui_action().event()
    }
}

impl Default for TextPanelHeaderAction {
    fn default() -> Self {
        Self {
            id: String::new(),
            key: String::new(),
            label: String::new(),
            group: String::new(),
            description: String::new(),
            compact_label: None,
            priority: ActionPriority::default(),
            enabled: true,
            show_in_header: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelRow {
    pub id: String,
    pub path: Option<String>,
    pub expanded: Option<bool>,
    pub kind: PanelRowKind,
    #[serde(default)]
    pub segments: Vec<PanelSegment>,
    #[serde(default)]
    pub right_segments: Vec<PanelSegment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelSegment {
    pub text: String,
    #[serde(default)]
    pub style: Option<Style>,
    #[serde(default)]
    pub semantic: Option<ThemeStyleSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelRowKind {
    File,
    Directory,
}

#[derive(Debug, Clone, Serialize)]
pub struct PanelEvent {
    pub panel_id: String,
    pub action: String,
    pub selected_index: usize,
    pub row: Option<PanelRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Semantic role for a source-backed text-panel block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextPanelBlockKind {
    User,
    /// A user message that is waiting to be sent, drawn as a dimmed prompt card.
    Queued,
    Agent,
    Error,
    /// Muted tool/progress timeline emitted while an agent turn runs.
    Activity,
    /// Muted, plugin-authored disclosure or other inline action.
    Action,
    #[default]
    Text,
}

/// Presentation format for a source-backed text-panel block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TextPanelBlockFormat {
    #[default]
    Plain,
    Markdown,
    Code(Option<String>),
}

/// One logical block in a text panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextPanelBlock {
    pub id: String,
    pub kind: TextPanelBlockKind,
    pub format: TextPanelBlockFormat,
    pub text: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TextPanelBlockWireFormat {
    #[default]
    Plain,
    Markdown,
    Code,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextPanelBlockWire {
    id: String,
    #[serde(default)]
    kind: TextPanelBlockKind,
    #[serde(default)]
    format: TextPanelBlockWireFormat,
    #[serde(default)]
    language: Option<String>,
    text: String,
}

impl<'de> Deserialize<'de> for TextPanelBlock {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = TextPanelBlockWire::deserialize(deserializer)?;
        let format = match wire.format {
            TextPanelBlockWireFormat::Plain if wire.language.is_some() => {
                return Err(D::Error::custom("language requires format code"));
            }
            TextPanelBlockWireFormat::Markdown if wire.language.is_some() => {
                return Err(D::Error::custom("language requires format code"));
            }
            TextPanelBlockWireFormat::Plain => TextPanelBlockFormat::Plain,
            TextPanelBlockWireFormat::Markdown => TextPanelBlockFormat::Markdown,
            TextPanelBlockWireFormat::Code => TextPanelBlockFormat::Code(wire.language),
        };
        Ok(Self {
            id: wire.id,
            kind: wire.kind,
            format,
            text: wire.text,
        })
    }
}

impl Serialize for TextPanelBlock {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let language = match &self.format {
            TextPanelBlockFormat::Code(language) => language.as_deref(),
            _ => None,
        };
        let mut block = serializer
            .serialize_struct("TextPanelBlock", if language.is_some() { 5 } else { 4 })?;
        block.serialize_field("id", &self.id)?;
        block.serialize_field("kind", &self.kind)?;
        block.serialize_field(
            "format",
            match &self.format {
                TextPanelBlockFormat::Plain => "plain",
                TextPanelBlockFormat::Markdown => "markdown",
                TextPanelBlockFormat::Code(_) => "code",
            },
        )?;
        if let Some(language) = language {
            block.serialize_field("language", language)?;
        }
        block.serialize_field("text", &self.text)?;
        block.end()
    }
}

/// Turn-scoped progress state rendered in a dedicated panel status row.
///
/// While `busy`, the core animates a spinner and shows the time elapsed since
/// the panel first became busy; `stream` appends a cursor to the last rendered
/// line to show that text is still arriving.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextPanelStatus {
    #[serde(default)]
    pub busy: bool,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub stream: bool,
}

/// Restorable identity and initial query for an embedded, workspace-scoped picker.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TextPanelContextOptions {
    pub key: String,
    pub query: String,
    /// Views the picker switches between, shown as a row of tabs.
    pub tabs: Vec<TextPanelContextTab>,
    /// The id of the selected tab.
    pub tab: String,
    /// The key that selects the next tab.
    pub tab_shortcut: String,
}

/// One view of an embedded picker; choosing it emits `context_tab` with its id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TextPanelContextTab {
    pub id: String,
    pub label: String,
    /// A shorter label for panes too narrow for every full one.
    pub compact_label: String,
}

/// Read-only conversation detail rendered inside an embedded session picker.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TextPanelPreview {
    pub session_id: String,
    pub prompt: String,
    pub response: String,
    pub error: String,
    pub loading: bool,
}

/// Foreground-first visual roles for a source-backed text panel.
type TextPanelPalette = SurfacePalette;

fn text_panel_palette(theme: &Theme, config: &PanelConfig) -> TextPanelPalette {
    SurfacePalette::new(theme, &panel_style(theme, config.surface.as_ref()))
}

/// Agent-specific overrides retain the shared surface card's contrast and geometry.
fn text_panel_prompt_palette(
    theme: &Theme,
    panel: &TextPanelPalette,
    selected: bool,
) -> SurfaceCardPalette {
    SurfaceCardPalette::new(
        panel,
        selected,
        SurfaceCardColors {
            background: theme_color(theme, &["red.agentPromptBackground"]),
            selected_background: theme_color(theme, &["red.agentPromptSelectedBackground"]),
            border: theme_color(theme, &["red.agentPromptBorder"]),
            selected_border: theme_color(theme, &["red.agentPromptSelectedBorder"]),
        },
    )
}

fn tint_color(color: Color, background: Color, alpha: u8) -> Color {
    let (Color::Rgb { r, g, b } | Color::Rgba { r, g, b, .. }) = color;
    blend_color(Color::Rgba { r, g, b, a: alpha }, background)
}

fn theme_color(theme: &Theme, candidates: &[&str]) -> Option<Color> {
    candidates
        .iter()
        .find_map(|candidate| theme.colors.get(*candidate).copied())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TextPanelContentMetrics {
    inset: usize,
    width: usize,
}

impl TextPanelContentMetrics {
    fn new(panel_width: usize) -> Self {
        let inset = usize::from(panel_width >= 3);
        Self {
            inset,
            width: panel_width.saturating_sub(inset.saturating_mul(2)).max(1),
        }
    }

    fn x(self, panel_x: usize) -> usize {
        panel_x.saturating_add(self.inset)
    }

    fn contains_x(self, panel_x: usize, x: usize) -> bool {
        let start = self.x(panel_x);
        x >= start && x < start.saturating_add(self.width)
    }

    fn column(self, panel_x: usize, x: usize) -> usize {
        x.saturating_sub(self.x(panel_x))
    }
}

fn text_panel_header_rows(config: &PanelConfig) -> usize {
    if config.title.is_some()
        || config
            .header_actions
            .iter()
            .any(|action| action.show_in_header)
    {
        2
    } else {
        0
    }
}

pub struct TextPanel {
    pub id: String,
    pub config: PanelConfig,
    /// The user chose this panel's width, so responsive sizing respects it.
    user_sized: bool,
    pub blocks: Vec<TextPanelBlock>,
    block_styles: Vec<Vec<StyleInfo>>,
    pub scroll: usize,
    pub follow_tail: bool,
    viewport: FollowTailViewport,
    composer: Option<TextPanelComposer>,
    status: Option<TextPanelStatus>,
    header_detail: Option<TextPanelHeaderDetail>,
    busy_since: Option<Instant>,
    selected_link: Option<u64>,
    scrollback: TextPanelScrollback,
    search: TextPanelSearch,
    last_focused_region: TextPanelFocusRegion,
    temporary_context: Option<TextPanelContext>,
    picker_memory: HashMap<String, TextPanelPickerMemory>,
    layout_cache: RefCell<Option<(usize, Arc<TextPanelLayout>)>>,
    search_cache: RefCell<Option<TextPanelSearchCache>>,
}

struct TextPanelContext {
    key: String,
    blocks: Vec<TextPanelBlock>,
    scroll: usize,
    follow_tail: bool,
    viewport: FollowTailViewport,
    status: Option<TextPanelStatus>,
    busy_since: Option<Instant>,
    selected_link: Option<u64>,
    scrollback: TextPanelScrollback,
    search: TextPanelSearch,
    last_focused_region: TextPanelFocusRegion,
    selected_action: Option<String>,
    selected_index: usize,
    restore_scroll: Option<usize>,
    preview: Option<TextPanelPreview>,
    tabs: Vec<TextPanelContextTab>,
    tab: String,
    tab_shortcut: String,
}

#[derive(Debug, Clone, Default)]
struct TextPanelPickerMemory {
    selected_action: Option<String>,
    selected_index: usize,
    scroll: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TextPanelPickerPreviewGeometry {
    list_width: usize,
    list_rows: usize,
    preview_column: usize,
    preview_row: usize,
    preview_width: usize,
    preview_rows: usize,
    side_by_side: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum TextPanelFocusRegion {
    #[default]
    Scrollback,
    Composer,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum TextPanelScrollbackMode {
    #[default]
    Normal,
    Visual,
    VisualLine,
}

#[derive(Debug, Clone, Default)]
struct TextPanelScrollback {
    focused: bool,
    initialized: bool,
    mode: TextPanelScrollbackMode,
    cursor: usize,
    preferred_column: Option<usize>,
    selection_anchor: Option<usize>,
    mouse_anchor: Option<usize>,
    mouse_dragging: bool,
    pending_find: Option<PendingScrollbackFind>,
    last_find: Option<ScrollbackFind>,
    pending_jump: Option<PendingScrollbackJump>,
}

#[derive(Debug, Clone, Copy)]
struct PendingScrollbackJump {
    direction: ScrollbackJumpDirection,
    count: usize,
}

#[derive(Debug, Clone, Copy)]
enum ScrollbackJumpDirection {
    Previous,
    Next,
}

#[derive(Debug, Clone, Copy)]
enum TextPanelSearchDirection {
    Forward,
    Backward,
}

impl TextPanelSearchDirection {
    fn reversed(self) -> Self {
        match self {
            Self::Forward => Self::Backward,
            Self::Backward => Self::Forward,
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Self::Forward => "/",
            Self::Backward => "?",
        }
    }
}

#[derive(Debug, Clone)]
struct TextPanelSearchQuery {
    text: String,
    direction: TextPanelSearchDirection,
}

#[derive(Debug, Clone, Copy)]
struct TextPanelSearchOrigin {
    cursor: usize,
    initialized: bool,
    preferred_column: Option<usize>,
    scroll: usize,
    follow_tail: bool,
    selected_link: Option<u64>,
}

#[derive(Debug)]
struct TextPanelSearchSession {
    input: PromptBuffer,
    query: TextPanelSearchQuery,
    origin: TextPanelSearchOrigin,
}

#[derive(Debug, Default)]
struct TextPanelSearch {
    active: Option<TextPanelSearchSession>,
    last: Option<TextPanelSearchQuery>,
    visible: bool,
}

impl TextPanelSearch {
    fn query(&self) -> Option<&TextPanelSearchQuery> {
        self.active
            .as_ref()
            .map(|session| &session.query)
            .or_else(|| self.visible.then_some(self.last.as_ref()).flatten())
    }
}

fn text_panel_search_match(
    matches: &[Range<usize>],
    cursor: usize,
    direction: TextPanelSearchDirection,
    count: usize,
) -> Option<usize> {
    let length = matches.len();
    if length == 0 {
        return None;
    }
    let step = count.max(1).saturating_sub(1) % length;
    Some(match direction {
        TextPanelSearchDirection::Forward => {
            (matches.partition_point(|found| found.start <= cursor) + step) % length
        }
        TextPanelSearchDirection::Backward => {
            (matches.partition_point(|found| found.start < cursor) + length - 1 - step) % length
        }
    })
}

fn current_text_panel_search_match(matches: &[Range<usize>], cursor: usize) -> Option<usize> {
    let index = matches.partition_point(|found| found.end <= cursor);
    matches
        .get(index)
        .is_some_and(|found| found.contains(&cursor))
        .then_some(index)
}

#[derive(Debug, Clone, Copy)]
struct PendingScrollbackFind {
    direction: ScrollbackFindDirection,
    till: bool,
    count: usize,
}

#[derive(Debug, Clone, Copy)]
struct ScrollbackFind {
    direction: ScrollbackFindDirection,
    till: bool,
    target: char,
}

#[derive(Debug, Clone, Copy)]
enum ScrollbackFindDirection {
    Forward,
    Backward,
}

#[derive(Debug, Clone)]
struct TextPanelLayoutCell {
    text: String,
    copy_prefix: String,
    column: usize,
    width: usize,
    link: Option<TextPanelLink>,
    virtual_space: bool,
}

#[derive(Debug, Clone)]
struct TextPanelLayoutLine {
    first: usize,
    cells: Vec<TextPanelLayoutCell>,
    break_after: RenderedTextLineBreak,
    selectable: bool,
    chrome_only: bool,
}

#[derive(Debug, Clone)]
struct TextPanelLayout {
    rendered: Vec<RenderedTextLine>,
    lines: Vec<TextPanelLayoutLine>,
    prompt_cards: Vec<TextPanelPromptCard>,
    searchable_rows: Vec<Range<usize>>,
    len: usize,
}

#[derive(Debug, Clone)]
struct TextPanelSearchCache {
    layout: Arc<TextPanelLayout>,
    query: String,
    matches: Arc<[Range<usize>]>,
}

struct TextPanelSearchCell {
    bytes: Range<usize>,
    offset: usize,
}

struct TextPanelSearchHighlights {
    matches: Arc<[Range<usize>]>,
    current: Option<usize>,
    normal_style: Style,
    current_style: Style,
}

impl TextPanelSearchHighlights {
    fn new(
        matches: Arc<[Range<usize>]>,
        cursor: usize,
        theme: &Theme,
        palette: &TextPanelPalette,
    ) -> Self {
        let fallback = Style {
            bg: Some(tint_color(
                palette.accent.fg.unwrap_or_default(),
                palette.surface.bg.unwrap_or_default(),
                90,
            )),
            ..Style::default()
        };
        Self {
            current: current_text_panel_search_match(&matches, cursor),
            matches,
            normal_style: theme
                .find_match_highlight_style
                .clone()
                .or_else(|| theme.find_match_style.clone())
                .unwrap_or(fallback),
            current_style: theme
                .find_match_style
                .clone()
                .unwrap_or_else(|| theme.list_selection_style()),
        }
    }

    fn style_at(&self, offset: usize) -> Option<&Style> {
        let index = self.matches.partition_point(|found| found.end <= offset);
        self.matches
            .get(index)
            .is_some_and(|found| found.contains(&offset))
            .then(|| {
                if self.current == Some(index) {
                    &self.current_style
                } else {
                    &self.normal_style
                }
            })
    }
}

/// The query and counter share one existing chrome row; long queries scroll to
/// keep their input cursor visible without changing transcript geometry.
struct TextPanelSearchBar {
    prefix: &'static str,
    text: String,
    suffix: String,
    suffix_column: usize,
    cursor_column: Option<usize>,
}

impl TextPanelSearchBar {
    fn new(
        search: &TextPanelSearch,
        matches: &[Range<usize>],
        cursor: usize,
        width: usize,
    ) -> Option<Self> {
        let query = search.query()?;
        let current = current_text_panel_search_match(matches, cursor).map_or(0, |index| index + 1);
        let suffix = if search.active.is_some() {
            format!("{current}/{}", matches.len())
        } else {
            format!("n/N · {current}/{}", matches.len())
        };
        let suffix_width = display_width(&suffix);
        let show_suffix = width >= suffix_width.saturating_add(5);
        let suffix_column = if show_suffix {
            width - suffix_width
        } else {
            width
        };
        let query_width = suffix_column.saturating_sub(if show_suffix { 2 } else { 1 });
        let (start, cursor_column) = if let Some(session) = &search.active {
            let cursor_byte = grapheme_to_byte(&query.text, session.input.cursor());
            let before = truncate_display_width_from_end(
                &query.text[..cursor_byte],
                query_width.saturating_sub(1),
            );
            (
                cursor_byte - before.len(),
                Some((1 + display_width(&before)).min(width.saturating_sub(1))),
            )
        } else {
            (0, None)
        };
        Some(Self {
            prefix: query.direction.prefix(),
            text: truncate_display_width(&query.text[start..], query_width),
            suffix: if show_suffix { suffix } else { String::new() },
            suffix_column,
            cursor_column,
        })
    }
}

struct TextPanelRendered {
    lines: Vec<RenderedTextLine>,
    prompt_cards: Vec<TextPanelPromptCard>,
    searchable_rows: Vec<Range<usize>>,
}

/// Rendered rows belonging to one source-backed user prompt, including its chrome.
#[derive(Debug, Clone)]
struct TextPanelPromptCard {
    block_index: usize,
    rows: Range<usize>,
    /// The message has not been sent yet.
    queued: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextPanelYank {
    pub(crate) text: String,
    pub(crate) linewise: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TextPanelScrollbackInput {
    Handled,
    Yank(TextPanelYank),
    OpenTurnActions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// The source text to copy from a conversation turn.
pub enum TextPanelTurnPart {
    Prompt,
    Answer,
}

/// Identifies the exact unsent draft a replacement confirmation approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextPanelDraftRevision {
    buffer_id: BufferId,
    revision: u64,
}

pub(crate) struct TextPanelTurnTarget {
    pub panel_id: String,
    pub prompt_id: String,
    pub number: usize,
    pub preview: String,
    pub has_answer: bool,
    pub can_reuse: bool,
}

pub(crate) enum TextPanelReuseOutcome {
    Loaded,
    Confirm(TextPanelDraftRevision),
}

#[derive(Debug, Clone, Copy)]
enum TextPanelMotion {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    FirstNonBlank,
    LineEnd,
    NextWord,
    PreviousWord,
    WordEnd,
    PreviousParagraph,
    NextParagraph,
    PageUp,
    PageDown,
    HalfPageUp,
    HalfPageDown,
    ViewportTop,
    ViewportMiddle,
    ViewportBottom,
    Top,
    Bottom,
}

const TEXT_PANEL_SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TEXT_PANEL_SPINNER_INTERVAL_MS: u64 = 120;

fn spinner_frame(elapsed_ms: u64) -> &'static str {
    let index = (elapsed_ms / TEXT_PANEL_SPINNER_INTERVAL_MS) as usize;
    TEXT_PANEL_SPINNER_FRAMES[index % TEXT_PANEL_SPINNER_FRAMES.len()]
}

fn format_elapsed(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    }
}

const MAX_COMPOSER_BYTES: usize = PROMPT_MAX_BYTES;
const COMPOSER_MAX_HEIGHT_PERCENT: usize = 70;
const COMPOSER_CHROME_ROWS: usize = 2;
const EMBEDDED_PICKER_CHROME_ROWS: usize = 3;
const EMBEDDED_PICKER_ITEM_INSET: usize = 2;
const EMBEDDED_PICKER_PREVIEW_MIN_COLUMNS: usize = 40;
const EMBEDDED_PICKER_PREVIEW_MIN_ROWS: usize = 5;

struct ComposerLayoutCache {
    buffer_id: BufferId,
    revision: u64,
    options: LayoutOptions,
    layout: Arc<TextLayout>,
}

struct TextPanelComposer {
    config: TextPanelComposerConfig,
    prompt: PromptBuffer,
    focused: bool,
    enabled: bool,
    status: Option<String>,
    validation: Option<&'static str>,
    temporary_context: Option<TextPanelComposerContext>,
    layout_cache: RefCell<Option<ComposerLayoutCache>>,
}

struct TextPanelComposerContext {
    config: TextPanelComposerConfig,
    prompt: PromptBuffer,
    focused: bool,
    enabled: bool,
    status: Option<String>,
    validation: Option<&'static str>,
}

impl TextPanelComposer {
    /// `content_width` excludes the panel inset, but includes the prompt marker.
    fn layout_options(content_width: usize) -> LayoutOptions {
        LayoutOptions::word(content_width.saturating_sub(2).max(1))
    }

    fn layout(&self, content_width: usize) -> Arc<TextLayout> {
        let options = Self::layout_options(content_width);
        if self.prompt.history_search_active() {
            return Arc::new(self.prompt.layout(options));
        }
        let buffer = self.prompt.buffer();
        let mut cache = self.layout_cache.borrow_mut();
        if let Some(cached) = cache.as_ref() {
            if cached.buffer_id == buffer.id()
                && cached.revision == buffer.revision()
                && cached.options == options
            {
                return Arc::clone(&cached.layout);
            }
        }
        let layout = Arc::new(self.prompt.layout(options));
        *cache = Some(ComposerLayoutCache {
            buffer_id: buffer.id(),
            revision: buffer.revision(),
            options,
            layout: Arc::clone(&layout),
        });
        layout
    }

    fn handle_event(&mut self, event: &Event, content_width: usize) -> PromptInput {
        self.prompt
            .handle_event_with_layout_options(event, Self::layout_options(content_width))
    }

    fn new(config: TextPanelComposerConfig) -> Self {
        Self {
            config,
            prompt: PromptBuffer::new("")
                .with_key_policy(PromptKeyPolicy::EnterSendsWithShellHistory),
            focused: false,
            enabled: true,
            status: None,
            validation: None,
            temporary_context: None,
            layout_cache: RefCell::new(None),
        }
    }

    fn enter_temporary_context(&mut self, placeholder: String) -> bool {
        if self.temporary_context.is_some() {
            return false;
        }
        let prompt = std::mem::replace(
            &mut self.prompt,
            PromptBuffer::new("").with_key_policy(PromptKeyPolicy::EnterSends),
        );
        self.temporary_context = Some(TextPanelComposerContext {
            config: self.config.clone(),
            prompt,
            focused: self.focused,
            enabled: self.enabled,
            status: self.status.take(),
            validation: self.validation.take(),
        });
        self.config.placeholder = placeholder;
        self.config.rows = 1;
        self.focused = true;
        self.enabled = true;
        self.validation = None;
        *self.layout_cache.borrow_mut() = None;
        true
    }

    fn exit_temporary_context(&mut self) -> bool {
        let Some(context) = self.temporary_context.take() else {
            return false;
        };
        self.config = context.config;
        self.prompt = context.prompt;
        self.focused = context.focused;
        self.enabled = context.enabled;
        self.status = context.status;
        self.validation = context.validation;
        *self.layout_cache.borrow_mut() = None;
        true
    }

    fn take_submission(&mut self) -> Option<String> {
        let Some(text) = self.prompt.take_submission() else {
            self.validation = Some("Prompt is empty");
            return None;
        };
        self.validation = None;
        Some(text)
    }
}

impl TextPanel {
    fn new(id: String, config: PanelConfig) -> Self {
        let composer = config.composer.clone().map(TextPanelComposer::new);
        Self {
            id,
            config,
            user_sized: false,
            blocks: Vec::new(),
            block_styles: Vec::new(),
            scroll: 0,
            follow_tail: true,
            viewport: FollowTailViewport::default(),
            composer,
            status: None,
            header_detail: None,
            busy_since: None,
            selected_link: None,
            scrollback: TextPanelScrollback::default(),
            search: TextPanelSearch::default(),
            last_focused_region: TextPanelFocusRegion::default(),
            temporary_context: None,
            picker_memory: HashMap::new(),
            layout_cache: RefCell::new(None),
            search_cache: RefCell::new(None),
        }
    }

    fn enter_temporary_context(
        &mut self,
        placeholder: String,
        options: TextPanelContextOptions,
    ) -> bool {
        if self.temporary_context.is_some() {
            return false;
        }
        let Some(composer) = self.composer.as_mut() else {
            return false;
        };
        if !composer.enter_temporary_context(placeholder) {
            return false;
        }
        composer.prompt.set_text(&options.query);
        let remembered = self
            .picker_memory
            .get(&options.key)
            .cloned()
            .unwrap_or_default();
        self.temporary_context = Some(TextPanelContext {
            key: options.key,
            blocks: std::mem::take(&mut self.blocks),
            scroll: self.scroll,
            follow_tail: self.follow_tail,
            viewport: self.viewport,
            status: self.status.take(),
            busy_since: self.busy_since.take(),
            selected_link: self.selected_link.take(),
            scrollback: std::mem::take(&mut self.scrollback),
            search: std::mem::take(&mut self.search),
            last_focused_region: self.last_focused_region,
            selected_action: remembered.selected_action,
            selected_index: remembered.selected_index,
            restore_scroll: Some(remembered.scroll),
            preview: None,
            tabs: options.tabs,
            tab: options.tab,
            tab_shortcut: options.tab_shortcut,
        });
        self.viewport = FollowTailViewport::default();
        self.viewport.scroll_to_top();
        self.scroll = 0;
        self.follow_tail = false;
        self.last_focused_region = TextPanelFocusRegion::Composer;
        self.invalidate_layout();
        true
    }

    fn exit_temporary_context(&mut self) -> bool {
        let Some(context) = self.temporary_context.take() else {
            return false;
        };
        let Some(composer) = self.composer.as_mut() else {
            self.temporary_context = Some(context);
            return false;
        };
        if !composer.exit_temporary_context() {
            self.temporary_context = Some(context);
            return false;
        }
        if !context.key.is_empty() {
            self.picker_memory.insert(
                context.key.clone(),
                TextPanelPickerMemory {
                    selected_action: context.selected_action,
                    selected_index: context.selected_index,
                    scroll: self.scroll,
                },
            );
        }
        self.blocks = context.blocks;
        self.scroll = context.scroll;
        self.follow_tail = context.follow_tail;
        self.viewport = context.viewport;
        self.status = context.status;
        self.busy_since = context.busy_since;
        self.selected_link = context.selected_link;
        self.scrollback = context.scrollback;
        self.search = context.search;
        self.last_focused_region = context.last_focused_region;
        self.invalidate_layout();
        true
    }

    fn set_status(&mut self, status: Option<TextPanelStatus>) {
        let stream_changed = self.status.as_ref().is_some_and(|status| status.stream)
            != status.as_ref().is_some_and(|status| status.stream);
        if stream_changed {
            self.invalidate_layout();
        }
        self.busy_since = if status.as_ref().is_some_and(|status| status.busy) {
            self.busy_since.or_else(|| Some(Instant::now()))
        } else {
            None
        };
        self.status = status;
    }

    fn status_height(&self) -> usize {
        self.status
            .as_ref()
            .map_or(0, |status| 1 + usize::from(status.busy))
    }

    fn picker_header_rows(&self, panel_height: usize) -> usize {
        if self.temporary_context.is_none() {
            return 0;
        }
        panel_height
            .saturating_sub(text_panel_header_rows(&self.config))
            .saturating_sub(self.status_height())
            .saturating_sub(1)
            .min(EMBEDDED_PICKER_CHROME_ROWS + usize::from(!self.picker_tabs().is_empty()))
    }

    fn picker_tabs(&self) -> &[TextPanelContextTab] {
        self.temporary_context
            .as_ref()
            .map_or(&[], |context| context.tabs.as_slice())
    }

    /// Selects a picker tab by id.
    fn select_picker_tab(&mut self, id: &str) -> Option<String> {
        let context = self.temporary_context.as_mut()?;
        let tab = context.tabs.iter().find(|tab| tab.id == id)?;
        context.tab = tab.id.clone();
        Some(context.tab.clone())
    }

    /// Selects the tab `step` places after the selected one, wrapping around.
    fn step_picker_tab(&mut self, step: isize) -> Option<String> {
        let context = self.temporary_context.as_mut()?;
        let count = context.tabs.len() as isize;
        if count == 0 {
            return None;
        }
        let current = context
            .tabs
            .iter()
            .position(|tab| tab.id == context.tab)
            .map_or(-1, |index| index as isize);
        let next = (current + step).rem_euclid(count) as usize;
        context.tab = context.tabs[next].id.clone();
        Some(context.tab.clone())
    }

    /// Pane actions not shown in the header, listed under the composer instead.
    fn composer_actions(&self) -> impl Iterator<Item = &TextPanelHeaderAction> {
        self.config
            .header_actions
            .iter()
            .filter(|action| !action.show_in_header && action.enabled)
    }

    fn composer_chrome_rows(&self) -> usize {
        COMPOSER_CHROME_ROWS + usize::from(self.composer_actions().next().is_some())
    }

    fn selected_picker_action(&self) -> Option<&str> {
        self.temporary_context.as_ref()?.selected_action.as_deref()
    }

    fn sync_picker_selection(&mut self, panel_height: usize, panel_width: usize) {
        let actions = self
            .blocks
            .iter()
            .filter(|block| block.kind == TextPanelBlockKind::Action)
            .map(|block| block.id.as_str())
            .collect::<Vec<_>>();
        let Some(context) = self.temporary_context.as_mut() else {
            return;
        };
        if actions.is_empty() {
            self.selected_link = None;
            return;
        }
        if !context
            .selected_action
            .as_deref()
            .is_some_and(|selected| actions.contains(&selected))
        {
            let index = context.selected_index.min(actions.len().saturating_sub(1));
            context.selected_action = actions.get(index).map(|action| (*action).to_string());
        }
        let Some(selected) = context.selected_action.clone() else {
            self.selected_link = None;
            return;
        };
        context.selected_index = actions
            .iter()
            .position(|action| *action == selected)
            .unwrap_or_default();
        let first_selected = context.selected_index == 0;
        if let Some(scroll) = context.restore_scroll.take() {
            self.scroll = scroll;
            self.viewport.restore(scroll, false);
        }
        let layout = self.layout(panel_width);
        let selected_rows = layout
            .rendered
            .iter()
            .enumerate()
            .filter_map(|(row, line)| {
                line.spans.iter().find_map(|span| {
                    let link = span.link.as_ref()?;
                    matches!(&link.target, TextPanelLinkTarget::PanelAction { block_id, .. } if block_id == &selected)
                        .then_some((row, link.id))
                })
            })
            .collect::<Vec<_>>();
        self.selected_link = selected_rows.first().map(|(_, link)| *link);
        let (Some((first, _)), Some((last, _))) = (selected_rows.first(), selected_rows.last())
        else {
            return;
        };
        let visible = self.visible_rows(panel_height, panel_width);
        if first_selected && *last < visible {
            // Keep the headings above the first row in view.
            self.scroll = 0;
        } else if *first < self.scroll {
            self.scroll = *first;
        } else if *last >= self.scroll.saturating_add(visible) {
            self.scroll = last.saturating_add(1).saturating_sub(visible);
        }
        self.follow_tail = false;
        self.viewport.restore(self.scroll, false);
    }

    fn move_picker_selection(&mut self, delta: isize, panel_height: usize, panel_width: usize) {
        let actions = self
            .blocks
            .iter()
            .filter(|block| block.kind == TextPanelBlockKind::Action)
            .map(|block| block.id.clone())
            .collect::<Vec<_>>();
        if actions.is_empty() {
            return;
        }
        let current = self
            .selected_picker_action()
            .and_then(|selected| actions.iter().position(|action| action == selected))
            .unwrap_or_default();
        let next = current
            .saturating_add_signed(delta)
            .min(actions.len().saturating_sub(1));
        if let Some(context) = self.temporary_context.as_mut() {
            context.selected_action = Some(actions[next].clone());
            context.selected_index = next;
        }
        self.sync_picker_selection(panel_height, panel_width);
    }

    fn search_bar_row(&self, height: usize) -> Option<usize> {
        if self.composer.is_some() {
            height.checked_sub(1)
        } else {
            (text_panel_header_rows(&self.config) > 0).then_some(0)
        }
    }

    fn update_blocks(
        &mut self,
        blocks: Vec<TextPanelBlock>,
        panel_height: usize,
        panel_width: usize,
    ) {
        self.update_blocks_with_highlighter(blocks, None, panel_height, panel_width);
    }

    fn update_blocks_with_highlighter(
        &mut self,
        blocks: Vec<TextPanelBlock>,
        highlighter: Option<&mut Highlighter>,
        panel_height: usize,
        panel_width: usize,
    ) {
        if blocks.is_empty() {
            self.viewport.reset();
            self.scroll = self.viewport.offset();
            self.follow_tail = self.viewport.is_following();
            // A picker starting over, as for a new scope or query, selects its first row.
            if let Some(context) = self.temporary_context.as_mut() {
                context.selected_action = None;
                context.selected_index = 0;
                context.restore_scroll = None;
            }
        }
        self.blocks = blocks;
        self.refresh_block_styles(highlighter);
        self.invalidate_layout();
        if self.temporary_context.is_some() {
            self.follow_tail = false;
            self.viewport.restore(self.scroll, false);
        }
        if self.follow_tail {
            self.scroll_to_bottom(panel_height, panel_width);
        } else {
            self.clamp_scroll(panel_height, panel_width);
        }
        let layout = self.layout(panel_width);
        self.scrollback.cursor = layout.clamp(self.scrollback.cursor);
        if layout.len == 0 {
            self.scrollback.initialized = false;
        }
        self.sync_picker_selection(panel_height, panel_width);
    }

    fn append_delta(
        &mut self,
        block_id: &str,
        delta: &str,
        panel_height: usize,
        panel_width: usize,
    ) {
        self.append_delta_with_highlighter(block_id, delta, None, panel_height, panel_width);
    }

    fn append_delta_with_highlighter(
        &mut self,
        block_id: &str,
        delta: &str,
        highlighter: Option<&mut Highlighter>,
        panel_height: usize,
        panel_width: usize,
    ) {
        if let Some(block) = self.blocks.iter_mut().find(|block| block.id == block_id) {
            block.text.push_str(delta);
        } else {
            self.blocks.push(TextPanelBlock {
                id: block_id.to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Markdown,
                text: delta.to_string(),
            });
        }
        self.refresh_block_styles(highlighter);
        self.invalidate_layout();

        if self.follow_tail {
            self.scroll_to_bottom(panel_height, panel_width);
        } else {
            self.clamp_scroll(panel_height, panel_width);
        }
        let layout = self.layout(panel_width);
        self.scrollback.cursor = layout.clamp(self.scrollback.cursor);
    }

    fn move_scroll(&mut self, delta: isize, panel_height: usize, panel_width: usize) {
        let max_scroll = self.max_scroll(panel_height, panel_width);
        self.viewport.restore(self.scroll, self.follow_tail);
        self.viewport.move_by(delta, max_scroll);
        self.scroll = self.viewport.offset();
        self.follow_tail = self.viewport.is_following();
    }

    fn page_scroll(&mut self, delta: isize, panel_height: usize, panel_width: usize) {
        let page = self.visible_rows(panel_height, panel_width).max(1) as isize;
        self.move_scroll(delta.saturating_mul(page), panel_height, panel_width);
    }

    fn scroll_to_top(&mut self) {
        self.viewport.scroll_to_top();
        self.scroll = self.viewport.offset();
        self.follow_tail = self.viewport.is_following();
    }

    fn scroll_to_bottom(&mut self, panel_height: usize, panel_width: usize) {
        self.viewport
            .follow(self.max_scroll(panel_height, panel_width));
        self.scroll = self.viewport.offset();
        self.follow_tail = self.viewport.is_following();
    }

    fn resume_tail_following(&mut self) {
        self.viewport.restore(self.scroll, true);
        self.follow_tail = true;
    }

    fn clamp_scroll(&mut self, panel_height: usize, panel_width: usize) {
        self.viewport.restore(self.scroll, self.follow_tail);
        self.viewport
            .clamp(self.max_scroll(panel_height, panel_width));
        self.scroll = self.viewport.offset();
        self.follow_tail = self.viewport.is_following();
    }

    fn max_scroll(&self, panel_height: usize, panel_width: usize) -> usize {
        self.layout(panel_width)
            .rendered
            .len()
            .saturating_sub(self.visible_rows(panel_height, panel_width))
    }

    fn visible_rows(&self, panel_height: usize, panel_width: usize) -> usize {
        let available = panel_height
            .saturating_sub(text_panel_header_rows(&self.config))
            .saturating_sub(self.picker_header_rows(panel_height))
            .saturating_sub(self.composer_height(panel_height, panel_width))
            .saturating_sub(self.status_height())
            .saturating_sub(self.action_footer_height())
            .max(1);
        self.picker_preview_geometry(panel_height, panel_width)
            .map_or(available, |geometry| geometry.list_rows)
            .max(1)
    }

    fn picker_preview_geometry(
        &self,
        panel_height: usize,
        panel_width: usize,
    ) -> Option<TextPanelPickerPreviewGeometry> {
        self.temporary_context.as_ref()?.preview.as_ref()?;
        let width = TextPanelContentMetrics::new(panel_width).width;
        let rows = panel_height
            .saturating_sub(text_panel_header_rows(&self.config))
            .saturating_sub(self.picker_header_rows(panel_height))
            .saturating_sub(self.status_height());
        if width > EMBEDDED_PICKER_PREVIEW_MIN_COLUMNS * 2 {
            let list_width = (width - 1) / 2;
            return Some(TextPanelPickerPreviewGeometry {
                list_width,
                list_rows: rows,
                preview_column: list_width + 1,
                preview_row: 0,
                preview_width: width.saturating_sub(list_width + 1),
                preview_rows: rows,
                side_by_side: true,
            });
        }
        if rows < EMBEDDED_PICKER_PREVIEW_MIN_ROWS + 4 {
            return None;
        }
        let preview_rows = (rows / 2)
            .max(EMBEDDED_PICKER_PREVIEW_MIN_ROWS)
            .min(rows.saturating_sub(4));
        let list_rows = rows.saturating_sub(preview_rows + 1);
        Some(TextPanelPickerPreviewGeometry {
            list_width: width,
            list_rows,
            preview_column: 0,
            preview_row: list_rows + 1,
            preview_width: width,
            preview_rows,
            side_by_side: false,
        })
    }

    fn action_footer_height(&self) -> usize {
        usize::from(
            self.action_shortcuts_active()
                && self.composer.is_none()
                && self
                    .config
                    .header_actions
                    .iter()
                    .any(|action| !action.show_in_header && action.enabled),
        )
    }

    fn action_shortcuts_active(&self) -> bool {
        self.scrollback.focused
            && self.scrollback.mode == TextPanelScrollbackMode::Normal
            && self.search.active.is_none()
            && self.scrollback.pending_find.is_none()
            && self.scrollback.pending_jump.is_none()
    }

    fn composer_height(&self, panel_height: usize, panel_width: usize) -> usize {
        if self.temporary_context.is_some() {
            return 0;
        }
        let Some(composer) = &self.composer else {
            return 0;
        };
        let available = panel_height
            .saturating_sub(text_panel_header_rows(&self.config))
            .saturating_sub(self.status_height());
        // Retain one transcript row. If even one input row and its chrome do
        // not fit, hide the composer rather than drawing outside the pane.
        let available = available.saturating_sub(1);
        let chrome = self.composer_chrome_rows();
        if available <= chrome {
            return 0;
        }
        let minimum = composer.config.rows.max(1).saturating_add(chrome);
        let cap = panel_height.saturating_mul(COMPOSER_MAX_HEIGHT_PERCENT) / 100;
        let desired = composer
            .layout(TextPanelContentMetrics::new(panel_width).width)
            .rows()
            .len()
            .saturating_add(chrome);
        desired.max(minimum).min(cap.max(minimum)).min(available)
    }

    fn copy_all(&self) -> String {
        self.blocks
            .iter()
            .filter(|block| !block.text.is_empty())
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn copy_last_agent(&self) -> Option<String> {
        self.blocks
            .iter()
            .rev()
            .find(|block| block.kind == TextPanelBlockKind::Agent && !block.text.is_empty())
            .map(|block| block.text.clone())
    }

    fn turn_text(&self, prompt_id: &str, part: TextPanelTurnPart) -> Option<String> {
        let start = self
            .blocks
            .iter()
            .position(|block| block.id == prompt_id && block.kind == TextPanelBlockKind::User)?;
        if part == TextPanelTurnPart::Prompt {
            return Some(self.blocks[start].text.clone());
        }
        let answer = self.blocks[start + 1..]
            .iter()
            .take_while(|block| block.kind != TextPanelBlockKind::User)
            .filter(|block| block.kind == TextPanelBlockKind::Agent && !block.text.is_empty())
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        (!answer.is_empty()).then_some(answer)
    }

    fn links(&self, width: usize) -> Vec<(TextPanelLink, Range<usize>)> {
        let mut links: Vec<(TextPanelLink, Range<usize>)> = Vec::new();
        let layout = self.layout(width);
        for line in &layout.lines {
            for (index, cell) in line.cells.iter().enumerate() {
                let Some(link) = cell.link.as_ref() else {
                    continue;
                };
                let offset = line.first + index;
                if let Some((previous, range)) = links.last_mut() {
                    if previous.id == link.id {
                        range.end = offset + 1;
                        continue;
                    }
                }
                links.push((link.clone(), offset..offset + 1));
            }
        }
        links
    }

    fn jump_to_link(
        &mut self,
        direction: ScrollbackJumpDirection,
        count: usize,
        panel_height: usize,
        width: usize,
    ) {
        let links = self.links(width);
        if links.is_empty() {
            self.selected_link = None;
            return;
        }
        let layout = self.layout(width);
        let cursor = layout.clamp(self.scrollback.cursor);
        let step = count.saturating_sub(1) % links.len();
        // Treat a soft-wrapped link as one destination, skipping it when the
        // cursor is anywhere inside its label.
        let index = match direction {
            ScrollbackJumpDirection::Next => {
                (links.partition_point(|(_, range)| range.start <= cursor) + step) % links.len()
            }
            ScrollbackJumpDirection::Previous => {
                (links.partition_point(|(_, range)| range.end <= cursor) + links.len() - 1 - step)
                    % links.len()
            }
        };
        let (link, range) = &links[index];
        self.selected_link = Some(link.id);
        self.scrollback.cursor = range.start;
        self.scrollback.initialized = true;
        self.scrollback.preferred_column = None;
        self.scrollback.mode = TextPanelScrollbackMode::Normal;
        self.scrollback.selection_anchor = None;
        self.follow_tail = false;
        self.reveal_scrollback_cursor(&layout, panel_height, width);
    }

    fn selected_link_target(&self, width: usize) -> Option<TextPanelLinkTarget> {
        let selected = self.selected_link?;
        self.links(width)
            .into_iter()
            .find(|(link, _)| link.id == selected)
            .map(|(link, _)| link.target)
    }

    fn build_rendered_lines(&self, width: usize) -> TextPanelRendered {
        let mut lines: Vec<RenderedTextLine> = Vec::new();
        let mut prompt_cards = Vec::new();
        let mut searchable_rows = Vec::new();
        for (block_index, block) in self.blocks.iter().enumerate() {
            let block_start = lines.len();
            if matches!(
                block.kind,
                TextPanelBlockKind::User | TextPanelBlockKind::Queued
            ) {
                let start = lines.len();
                let framed = width >= 7;
                let queued = block.kind == TextPanelBlockKind::Queued;
                lines.push(RenderedTextLine::chrome(
                    "▄".repeat(width),
                    TextPanelSpanStyle::User,
                ));
                let label = match (framed, queued) {
                    (true, true) => "  You · queued",
                    (false, true) => "queued",
                    (true, false) => "  You",
                    (false, false) => "You",
                };
                lines.push(RenderedTextLine::chrome(
                    label.to_string(),
                    TextPanelSpanStyle::User,
                ));
                let content_width = width.saturating_sub(if framed { 4 } else { 0 }).max(1);
                let text_style = if queued {
                    TextPanelSpanStyle::Muted
                } else {
                    TextPanelSpanStyle::Text
                };
                let mut block_lines = match &block.format {
                    TextPanelBlockFormat::Plain => {
                        wrap_plain_text(&block.text, content_width, text_style)
                    }
                    TextPanelBlockFormat::Markdown => {
                        render_markdown_lines(&block.text, content_width)
                    }
                    TextPanelBlockFormat::Code(_) => render_code_lines(
                        &block.text,
                        content_width,
                        self.block_styles
                            .get(block_index)
                            .map_or(&[], Vec::as_slice),
                    ),
                };
                if block_lines.is_empty() {
                    block_lines.push(RenderedTextLine::plain(
                        String::new(),
                        TextPanelSpanStyle::Text,
                    ));
                }
                if queued {
                    // Dim every format alike, including markdown and code.
                    for span in block_lines
                        .iter_mut()
                        .flat_map(|line| line.spans.iter_mut())
                    {
                        span.style = TextPanelSpanStyle::Muted;
                        span.syntax_style = None;
                    }
                }
                namespace_block_links(&mut block_lines, block_index);
                lines.extend(block_lines.into_iter().map(|line| {
                    if framed {
                        user_padded(line)
                    } else {
                        line
                    }
                }));
                lines.push(RenderedTextLine::chrome(
                    "▀".repeat(width),
                    TextPanelSpanStyle::User,
                ));
                prompt_cards.push(TextPanelPromptCard {
                    block_index,
                    rows: start..lines.len(),
                    queued,
                });
            } else {
                if let Some((label, style)) = block_label(&block.kind) {
                    lines.push(RenderedTextLine::chrome(label.to_string(), style));
                }

                let style = block_style(&block.kind);
                let mut block_lines = if block.kind == TextPanelBlockKind::Action {
                    let action_width = if self.temporary_context.is_some() {
                        width.saturating_sub(picker_item_text_inset(width))
                    } else {
                        width
                    };
                    block
                        .text
                        .lines()
                        .map(|line| {
                            RenderedTextLine::plain(
                                crate::unicode_utils::truncate_display_width_with_marker(
                                    line,
                                    action_width,
                                    "…",
                                    crate::unicode_utils::TruncationSide::Right,
                                ),
                                style,
                            )
                        })
                        .collect()
                } else {
                    match &block.format {
                        TextPanelBlockFormat::Plain => wrap_plain_text(&block.text, width, style),
                        TextPanelBlockFormat::Markdown => render_markdown_lines(&block.text, width),
                        TextPanelBlockFormat::Code(_) => render_code_lines(
                            &block.text,
                            width,
                            self.block_styles
                                .get(block_index)
                                .map_or(&[], Vec::as_slice),
                        ),
                    }
                };
                if block_lines.is_empty() {
                    block_lines.push(RenderedTextLine::plain(String::new(), style));
                }
                if block.kind == TextPanelBlockKind::Action {
                    for span in block_lines.iter_mut().flat_map(|line| &mut line.spans) {
                        span.link = Some(TextPanelLink {
                            id: 1,
                            target: TextPanelLinkTarget::PanelAction {
                                panel_id: self.id.clone(),
                                block_id: block.id.clone(),
                            },
                        });
                    }
                }
                namespace_block_links(&mut block_lines, block_index);
                lines.extend(block_lines);
            }
            if matches!(
                block.kind,
                TextPanelBlockKind::User | TextPanelBlockKind::Agent
            ) {
                searchable_rows.push(block_start..lines.len());
            }
            let continues_action = block.kind == TextPanelBlockKind::Action
                && self
                    .blocks
                    .get(block_index + 1)
                    .is_some_and(|next| next.kind == TextPanelBlockKind::Action);
            let picker_group_heading =
                self.temporary_context.is_some() && block.id.starts_with("session-browser:group:");
            if !picker_group_heading && (!continues_action || self.temporary_context.is_some()) {
                lines.push(RenderedTextLine::plain(
                    String::new(),
                    TextPanelSpanStyle::Text,
                ));
            }
        }
        if lines.last().is_some_and(RenderedTextLine::is_empty) {
            lines.pop();
        }
        if self.status.as_ref().is_some_and(|status| status.stream) {
            if let Some(last) = lines.last_mut() {
                last.spans.push(RenderedTextSpan {
                    text: "▌".to_string(),
                    style: TextPanelSpanStyle::User,
                    syntax_style: None,
                    link: None,
                    selection: TextPanelSpanSelection::Chrome,
                });
            }
        }
        TextPanelRendered {
            lines,
            prompt_cards,
            searchable_rows,
        }
    }
}

impl TextPanelLayout {
    fn new(rendered: Vec<RenderedTextLine>) -> Self {
        let mut next = 0usize;
        let lines = rendered
            .iter()
            .map(|line| {
                let first = next;
                let mut column = 0usize;
                let mut cells = Vec::new();
                let mut pending_copy_separator = String::new();
                for span in &line.spans {
                    match &span.selection {
                        TextPanelSpanSelection::Chrome => {
                            column = column.saturating_add(display_width(&span.text));
                        }
                        TextPanelSpanSelection::CopySeparator(copy) => {
                            pending_copy_separator.clone_from(copy);
                            column = column.saturating_add(display_width(&span.text));
                        }
                        TextPanelSpanSelection::Content => {
                            for grapheme in span.text.graphemes(true) {
                                let width = display_width(grapheme).max(1);
                                cells.push(TextPanelLayoutCell {
                                    text: grapheme.to_string(),
                                    copy_prefix: std::mem::take(&mut pending_copy_separator),
                                    column,
                                    width,
                                    link: span.link.clone(),
                                    virtual_space: false,
                                });
                                next = next.saturating_add(1);
                                column = column.saturating_add(width);
                            }
                        }
                    }
                }
                if !cells.is_empty() && line.break_after == RenderedTextLineBreak::SoftSpace {
                    cells.push(TextPanelLayoutCell {
                        text: " ".to_string(),
                        copy_prefix: String::new(),
                        column,
                        width: 0,
                        link: None,
                        virtual_space: true,
                    });
                    next = next.saturating_add(1);
                }
                let selectable = !cells.is_empty();
                let chrome_only = line.selection == TextPanelLineSelection::Chrome;
                TextPanelLayoutLine {
                    first,
                    cells,
                    break_after: line.break_after,
                    selectable,
                    chrome_only,
                }
            })
            .collect();
        Self {
            rendered,
            lines,
            prompt_cards: Vec::new(),
            searchable_rows: Vec::new(),
            len: next,
        }
    }

    fn clamp(&self, offset: usize) -> usize {
        offset.min(self.len.saturating_sub(1))
    }

    /// Matches literal visible text, retaining grapheme offsets for cursor movement
    /// and highlighting. Soft-wrapped rows join without introducing fake newlines.
    fn find_search_matches(&self, query: &str) -> Arc<[Range<usize>]> {
        let mut matches = Vec::new();
        if !query.is_empty() {
            for rows in &self.searchable_rows {
                let mut text = String::new();
                let mut cells = Vec::new();
                let mut previous_break = None;
                for row in rows.clone() {
                    let Some(line) = self.lines.get(row) else {
                        continue;
                    };
                    if line.chrome_only {
                        continue;
                    }
                    if previous_break == Some(RenderedTextLineBreak::Hard) {
                        text.push('\n');
                    }
                    for (index, cell) in line.cells.iter().enumerate() {
                        text.push_str(&cell.copy_prefix);
                        let start = text.len();
                        text.push_str(&cell.text);
                        cells.push(TextPanelSearchCell {
                            bytes: start..text.len(),
                            offset: line.first + index,
                        });
                    }
                    previous_break = Some(line.break_after);
                }
                for (start, found) in text.match_indices(query) {
                    let end = start + found.len();
                    let first = cells.partition_point(|cell| cell.bytes.end <= start);
                    let after = cells.partition_point(|cell| cell.bytes.start < end);
                    if first < after {
                        matches.push(cells[first].offset..cells[after - 1].offset + 1);
                    }
                }
            }
        }
        matches.into()
    }

    fn position(&self, offset: usize) -> Option<(usize, usize, usize)> {
        if self.len == 0 {
            return None;
        }
        let offset = self.clamp(offset);
        let row = self
            .lines
            .partition_point(|line| line.first <= offset)
            .checked_sub(1)?;
        let line = self.lines.get(row)?;
        let index = offset.checked_sub(line.first)?;
        let cell = line.cells.get(index)?;
        Some((row, index, cell.column))
    }

    fn offset_at(&self, row: usize, column: usize) -> Option<usize> {
        let line = self.lines.get(row)?;
        line.cells
            .iter()
            .enumerate()
            .filter(|(_, cell)| !cell.virtual_space)
            .min_by_key(|(_, cell)| {
                let end = cell.column.saturating_add(cell.width.saturating_sub(1));
                if column < cell.column {
                    cell.column - column
                } else {
                    column.saturating_sub(end)
                }
            })
            .map(|(index, _)| line.first.saturating_add(index))
    }

    fn nearest_offset_on_row(&self, row: usize, column: usize) -> Option<usize> {
        if let Some(offset) = self.offset_at(row, column) {
            return Some(offset);
        }
        (1..self.lines.len()).find_map(|distance| {
            row.checked_sub(distance)
                .and_then(|candidate| self.offset_at(candidate, column))
                .or_else(|| self.offset_at(row.saturating_add(distance), column))
        })
    }

    fn offset_at_or_after(&self, row: usize, column: usize) -> Option<usize> {
        (row..self.lines.len()).find_map(|candidate| self.offset_at(candidate, column))
    }

    fn offset_at_or_before(&self, row: usize, column: usize) -> Option<usize> {
        (0..=row.min(self.lines.len().saturating_sub(1)))
            .rev()
            .find_map(|candidate| self.offset_at(candidate, column))
    }

    fn line_bounds(&self, row: usize) -> Option<(usize, usize)> {
        let line = self.lines.get(row)?;
        let first = line.cells.iter().position(|cell| !cell.virtual_space)?;
        let last = line.cells.iter().rposition(|cell| !cell.virtual_space)?;
        Some((
            line.first.saturating_add(first),
            line.first.saturating_add(last),
        ))
    }

    fn logical_line_bounds(&self, row: usize) -> Option<(usize, usize)> {
        let mut first_row = row.min(self.lines.len().saturating_sub(1));
        while first_row > 0 && self.lines[first_row - 1].break_after != RenderedTextLineBreak::Hard
        {
            first_row -= 1;
        }
        let mut last_row = row.min(self.lines.len().saturating_sub(1));
        while last_row + 1 < self.lines.len()
            && self.lines[last_row].break_after != RenderedTextLineBreak::Hard
        {
            last_row += 1;
        }
        let start = (first_row..=last_row)
            .find_map(|candidate| self.line_bounds(candidate))?
            .0;
        let end = (first_row..=last_row)
            .rev()
            .find_map(|candidate| self.line_bounds(candidate))?
            .1;
        Some((start, end))
    }

    fn first_non_blank(&self, row: usize) -> Option<usize> {
        let line = self.lines.get(row)?;
        line.cells
            .iter()
            .position(|cell| !cell.text.chars().all(char::is_whitespace))
            .map(|index| line.first.saturating_add(index))
            .or_else(|| self.line_bounds(row).map(|(first, _)| first))
    }

    fn link_at(&self, offset: usize) -> Option<TextPanelLinkTarget> {
        let (row, index, _) = self.position(offset)?;
        self.lines
            .get(row)?
            .cells
            .get(index)?
            .link
            .as_ref()
            .map(|link| link.target.clone())
    }

    fn selected_text(&self, start: usize, end: usize, linewise: bool) -> String {
        if self.len == 0 {
            return String::new();
        }
        let mut start = self.clamp(start);
        let mut end = self.clamp(end);
        if start > end {
            std::mem::swap(&mut start, &mut end);
        }
        if linewise {
            let Some((start_row, _, _)) = self.position(start) else {
                return String::new();
            };
            let Some((end_row, _, _)) = self.position(end) else {
                return String::new();
            };
            if let Some((line_start, _)) = self.logical_line_bounds(start_row) {
                start = line_start;
            }
            if let Some((_, line_end)) = self.logical_line_bounds(end_row) {
                end = line_end;
            }
        }

        let Some((start_row, _, _)) = self.position(start) else {
            return String::new();
        };
        let Some((end_row, _, _)) = self.position(end) else {
            return String::new();
        };
        let mut output = String::new();
        for row in start_row..=end_row {
            if row > start_row && !self.lines[row - 1].chrome_only {
                match self.lines[row - 1].break_after {
                    RenderedTextLineBreak::Soft => {}
                    RenderedTextLineBreak::SoftSpace => {}
                    RenderedTextLineBreak::Hard => output.push('\n'),
                }
            }
            let line = &self.lines[row];
            let mut wrote_selected_cell = false;
            for (index, cell) in line.cells.iter().enumerate() {
                let offset = line.first.saturating_add(index);
                if offset >= start && offset <= end {
                    if wrote_selected_cell {
                        output.push_str(&cell.copy_prefix);
                    }
                    output.push_str(&cell.text);
                    wrote_selected_cell = true;
                }
            }
        }
        if linewise && !output.ends_with('\n') {
            output.push('\n');
        }
        output
    }
}

impl TextPanel {
    fn refresh_block_styles(&mut self, mut highlighter: Option<&mut Highlighter>) {
        self.block_styles = self
            .blocks
            .iter()
            .map(|block| {
                let TextPanelBlockFormat::Code(language) = &block.format else {
                    return Vec::new();
                };
                let Some(highlighter) = highlighter.as_deref_mut() else {
                    return Vec::new();
                };
                let Some(language) = language
                    .as_deref()
                    .and_then(|language| highlighter.language_id_for_name(language))
                    .map(str::to_owned)
                else {
                    return Vec::new();
                };
                highlighter
                    .highlight(&language, &block.text)
                    .unwrap_or_default()
            })
            .collect();
        self.invalidate_layout();
    }

    fn invalidate_layout(&self) {
        self.layout_cache.borrow_mut().take();
        self.search_cache.borrow_mut().take();
    }

    fn search_matches(&self, layout: &Arc<TextPanelLayout>, query: &str) -> Arc<[Range<usize>]> {
        if let Some(cache) = self.search_cache.borrow().as_ref() {
            if Arc::ptr_eq(&cache.layout, layout) && cache.query == query {
                return Arc::clone(&cache.matches);
            }
        }
        let matches = layout.find_search_matches(query);
        *self.search_cache.borrow_mut() = Some(TextPanelSearchCache {
            layout: Arc::clone(layout),
            query: query.to_string(),
            matches: Arc::clone(&matches),
        });
        matches
    }

    fn layout(&self, width: usize) -> Arc<TextPanelLayout> {
        let width = width.max(1);
        if let Some((cached_width, layout)) = self.layout_cache.borrow().as_ref() {
            if *cached_width == width {
                return Arc::clone(layout);
            }
        }

        let _span = crate::editor::perf::PerfSpan::start("panel:text_layout_miss");
        let mut content_width = TextPanelContentMetrics::new(width).width;
        if self
            .temporary_context
            .as_ref()
            .and_then(|context| context.preview.as_ref())
            .is_some()
            && content_width > EMBEDDED_PICKER_PREVIEW_MIN_COLUMNS * 2
        {
            content_width = (content_width - 1) / 2;
        }
        let rendered = self.build_rendered_lines(content_width);
        let mut layout = TextPanelLayout::new(rendered.lines);
        layout.prompt_cards = rendered.prompt_cards;
        layout.searchable_rows = rendered.searchable_rows;
        if self.status.as_ref().is_some_and(|status| status.stream) {
            if let Some(line) = layout.lines.last_mut() {
                if line.cells.last().is_some_and(|cell| cell.text == "▌") {
                    line.cells.pop();
                    layout.len = layout.len.saturating_sub(1);
                }
            }
        }
        let layout = Arc::new(layout);
        *self.layout_cache.borrow_mut() = Some((width, Arc::clone(&layout)));
        layout
    }

    /// The transcript cursor selects its enclosing turn. While composing, the newest
    /// prompt stays accented without adding a second, independent selection model.
    fn selected_prompt(&self, layout: &TextPanelLayout) -> Option<usize> {
        if self.scrollback.focused && self.scrollback.initialized {
            if let Some((row, _, _)) = layout.position(self.scrollback.cursor) {
                // A queued card has no turn yet; its turn actions belong to
                // the sent prompt above it.
                return layout
                    .prompt_cards
                    .iter()
                    .rev()
                    .find(|card| !card.queued && card.rows.start <= row)
                    .map(|card| card.block_index);
            }
        }
        // A queued message is not the turn being answered; keep the accent on
        // the newest sent prompt.
        layout
            .prompt_cards
            .iter()
            .rev()
            .find(|card| !card.queued)
            .map(|card| card.block_index)
    }

    fn jump_to_prompt(
        &mut self,
        direction: ScrollbackJumpDirection,
        count: usize,
        panel_height: usize,
        width: usize,
    ) {
        let layout = self.layout(width);
        let cursor = if self.follow_tail {
            // Following output starts the backward search after the entire transcript.
            layout.len
        } else if self.scrollback.initialized {
            layout.clamp(self.scrollback.cursor)
        } else {
            layout.offset_at_or_after(self.scroll, 0).unwrap_or(0)
        };
        let anchors = layout.prompt_cards.iter().filter_map(|card| {
            card.rows.clone().find_map(|row| {
                layout
                    .offset_at(row, 0)
                    .map(|offset| (card.rows.start, row, offset))
            })
        });
        let count = count.max(1);
        let target = match direction {
            ScrollbackJumpDirection::Previous => anchors
                .rev()
                .filter(|(_, _, offset)| *offset < cursor)
                .take(count)
                .last(),
            ScrollbackJumpDirection::Next => anchors
                .filter(|(_, _, offset)| *offset > cursor)
                .take(count)
                .last(),
        };
        let Some((card_start, row, offset)) = target else {
            return;
        };

        self.scrollback.cursor = offset;
        self.scrollback.initialized = true;
        self.scrollback.preferred_column = None;
        self.scrollback.selection_anchor = None;
        self.selected_link = None;

        // Include the card's heading and cap whenever the pane is tall enough.
        let visible_rows = self.visible_rows(panel_height, width);
        let max_scroll = layout.lines.len().saturating_sub(visible_rows);
        self.viewport.restore(card_start.min(max_scroll), false);
        self.viewport.reveal(row, visible_rows);
        self.scroll = self.viewport.offset();
        self.follow_tail = self.viewport.is_following();
    }

    fn begin_transcript_search(
        &mut self,
        direction: TextPanelSearchDirection,
        panel_height: usize,
        width: usize,
    ) {
        let scroll = self
            .viewport
            .visible_offset(self.max_scroll(panel_height, width));
        self.search.active = Some(TextPanelSearchSession {
            input: PromptBuffer::new(""),
            query: TextPanelSearchQuery {
                text: String::new(),
                direction,
            },
            origin: TextPanelSearchOrigin {
                cursor: self.scrollback.cursor,
                initialized: self.scrollback.initialized,
                preferred_column: self.scrollback.preferred_column,
                scroll,
                follow_tail: self.follow_tail,
                selected_link: self.selected_link,
            },
        });
        self.scrollback.pending_find = None;
        self.scrollback.pending_jump = None;
        self.viewport.restore(scroll, false);
        self.scroll = scroll;
        self.follow_tail = false;
    }

    fn restore_search_origin(&mut self, origin: TextPanelSearchOrigin, following: bool) {
        self.scrollback.cursor = origin.cursor;
        self.scrollback.initialized = origin.initialized;
        self.scrollback.preferred_column = origin.preferred_column;
        self.selected_link = origin.selected_link;
        self.viewport.restore(origin.scroll, following);
        self.scroll = origin.scroll;
        self.follow_tail = following;
    }

    fn cancel_transcript_search(&mut self) {
        if let Some(session) = self.search.active.take() {
            self.restore_search_origin(session.origin, session.origin.follow_tail);
        }
    }

    fn move_to_transcript_match(
        &mut self,
        layout: &TextPanelLayout,
        found: &Range<usize>,
        panel_height: usize,
        width: usize,
    ) {
        self.scrollback.cursor = layout.clamp(found.start);
        self.scrollback.initialized = true;
        self.scrollback.preferred_column = None;
        self.selected_link = None;
        self.follow_tail = false;
        self.reveal_scrollback_cursor(layout, panel_height, width);
    }

    fn preview_transcript_search(&mut self, panel_height: usize, width: usize) {
        let Some(session) = self.search.active.as_ref() else {
            return;
        };
        let origin = session.origin;
        let direction = session.query.direction;
        let layout = self.layout(width);
        let matches = self.search_matches(&layout, &session.query.text);
        let cursor = if origin.follow_tail {
            layout.len
        } else if origin.initialized {
            layout.clamp(origin.cursor)
        } else {
            layout.offset_at_or_after(origin.scroll, 0).unwrap_or(0)
        };
        self.restore_search_origin(origin, false);
        if let Some(index) = text_panel_search_match(&matches, cursor, direction, 1) {
            self.move_to_transcript_match(&layout, &matches[index], panel_height, width);
        }
    }

    fn finish_transcript_search(&mut self, panel_height: usize, width: usize) {
        if let Some(session) = self.search.active.as_mut() {
            if session.query.text.is_empty() {
                if let Some(last) = self.search.last.as_ref() {
                    session.query.text.clone_from(&last.text);
                }
            }
        }
        self.preview_transcript_search(panel_height, width);
        let Some(session) = self.search.active.take() else {
            return;
        };
        if session.query.text.is_empty() {
            self.restore_search_origin(session.origin, session.origin.follow_tail);
        } else {
            self.search.last = Some(session.query);
            self.search.visible = true;
        }
    }

    fn repeat_transcript_search(
        &mut self,
        reverse: bool,
        count: usize,
        panel_height: usize,
        width: usize,
    ) {
        let Some(query) = self.search.last.as_ref() else {
            return;
        };
        let direction = if reverse {
            query.direction.reversed()
        } else {
            query.direction
        };
        let layout = self.layout(width);
        let matches = self.search_matches(&layout, &query.text);
        let cursor = if self.follow_tail {
            layout.len
        } else {
            layout.clamp(self.scrollback.cursor)
        };
        self.search.visible = true;
        if let Some(index) = text_panel_search_match(&matches, cursor, direction, count) {
            self.move_to_transcript_match(&layout, &matches[index], panel_height, width);
        }
    }

    fn handle_transcript_search_input(
        &mut self,
        event: &Event,
        panel_height: usize,
        width: usize,
    ) -> bool {
        if self.search.active.is_none() {
            return false;
        }
        if let Event::Key(key) = event {
            if key.code == KeyCode::Esc
                || (matches!(key.code, KeyCode::Char('c' | 'g'))
                    && key.modifiers.contains(KeyModifiers::CONTROL))
            {
                self.cancel_transcript_search();
                return true;
            }
            if matches!(key.code, KeyCode::Enter | KeyCode::Char('\n'))
                || (key.code == KeyCode::Char('j') && key.modifiers.contains(KeyModifiers::CONTROL))
            {
                self.finish_transcript_search(panel_height, width);
                return true;
            }
        }
        let Some(session) = self.search.active.as_mut() else {
            return false;
        };
        match event {
            Event::Paste(text) => {
                session.input.insert(&first_prompt_line(text));
            }
            Event::Key(_) => {
                session
                    .input
                    .handle_event_with_layout_options(event, LayoutOptions::grapheme(width.max(1)));
                session.input.set_mode(crate::editor::Mode::Insert);
            }
            _ => return false,
        }
        let text = session.input.text();
        if text != session.query.text {
            session.query.text = text;
            self.preview_transcript_search(panel_height, width);
        }
        true
    }

    fn focus_scrollback(&mut self, width: usize) {
        self.cancel_transcript_search();
        if let Some(composer) = self.composer.as_mut() {
            composer.focused = false;
            composer.prompt.cancel_history_search();
        }
        self.scrollback.focused = true;
        self.scrollback.mode = TextPanelScrollbackMode::Normal;
        self.scrollback.selection_anchor = None;
        self.scrollback.pending_find = None;
        self.scrollback.pending_jump = None;
        let layout = self.layout(width);
        if !self.scrollback.initialized {
            self.scrollback.cursor = (self.scroll..layout.lines.len())
                .find_map(|row| layout.offset_at(row, 0))
                .unwrap_or_else(|| layout.clamp(self.scrollback.cursor));
            self.scrollback.initialized = layout.len > 0;
        } else {
            self.scrollback.cursor = layout.clamp(self.scrollback.cursor);
        }
    }

    fn blur_scrollback(&mut self) {
        self.cancel_transcript_search();
        self.scrollback.focused = false;
        self.scrollback.mode = TextPanelScrollbackMode::Normal;
        self.scrollback.selection_anchor = None;
        self.scrollback.mouse_anchor = None;
        self.scrollback.mouse_dragging = false;
        self.scrollback.pending_find = None;
        self.scrollback.pending_jump = None;
    }

    fn remember_focused_region(&mut self) {
        if self
            .composer
            .as_ref()
            .is_some_and(|composer| composer.focused)
        {
            self.last_focused_region = TextPanelFocusRegion::Composer;
        } else if self.scrollback.focused {
            self.last_focused_region = TextPanelFocusRegion::Scrollback;
        }
    }

    fn restore_focused_region(&mut self, width: usize) {
        if self.last_focused_region == TextPanelFocusRegion::Composer
            && self
                .composer
                .as_ref()
                .is_some_and(|composer| composer.enabled)
        {
            self.blur_scrollback();
            if let Some(composer) = self.composer.as_mut() {
                composer.focused = true;
            }
        } else {
            self.focus_scrollback(width);
        }
    }

    fn selection_bounds(&self, layout: &TextPanelLayout) -> Option<(usize, usize)> {
        let anchor = self.scrollback.selection_anchor?;
        let mut start = layout.clamp(anchor);
        let mut end = layout.clamp(self.scrollback.cursor);
        if start > end {
            std::mem::swap(&mut start, &mut end);
        }
        if self.scrollback.mode == TextPanelScrollbackMode::VisualLine {
            let (start_row, _, _) = layout.position(start)?;
            let (end_row, _, _) = layout.position(end)?;
            start = layout.logical_line_bounds(start_row)?.0;
            end = layout.logical_line_bounds(end_row)?.1;
        }
        Some((start, end))
    }

    fn reveal_scrollback_cursor(
        &mut self,
        layout: &TextPanelLayout,
        panel_height: usize,
        width: usize,
    ) {
        let Some((row, _, _)) = layout.position(self.scrollback.cursor) else {
            return;
        };
        self.viewport.restore(self.scroll, self.follow_tail);
        self.viewport
            .reveal(row, self.visible_rows(panel_height, width));
        self.scroll = self.viewport.offset();
        self.follow_tail = self.viewport.is_following();
    }

    fn move_scrollback(
        &mut self,
        motion: TextPanelMotion,
        count: usize,
        panel_height: usize,
        width: usize,
    ) {
        let layout = self.layout(width);
        if layout.len == 0 {
            self.scrollback.cursor = 0;
            return;
        }
        self.scrollback.cursor = layout.clamp(self.scrollback.cursor);
        let count = count.max(1);
        let (row, _, column) = layout.position(self.scrollback.cursor).unwrap_or_default();
        let visible_rows = self.visible_rows(panel_height, width).max(1);
        let target = match motion {
            TextPanelMotion::Left => self.scrollback.cursor.saturating_sub(count),
            TextPanelMotion::Right => self
                .scrollback
                .cursor
                .saturating_add(count)
                .min(layout.len - 1),
            TextPanelMotion::Up | TextPanelMotion::Down => {
                let goal = self.scrollback.preferred_column.unwrap_or(column);
                self.scrollback.preferred_column = Some(goal);
                let target_row = if matches!(motion, TextPanelMotion::Up) {
                    row.saturating_sub(count)
                } else {
                    row.saturating_add(count)
                        .min(layout.lines.len().saturating_sub(1))
                };
                if matches!(motion, TextPanelMotion::Up) {
                    layout.offset_at_or_before(target_row, goal)
                } else {
                    layout.offset_at_or_after(target_row, goal)
                }
                .unwrap_or(self.scrollback.cursor)
            }
            TextPanelMotion::LineStart => layout.line_bounds(row).map_or(0, |bounds| bounds.0),
            TextPanelMotion::FirstNonBlank => layout
                .first_non_blank(row)
                .unwrap_or(self.scrollback.cursor),
            TextPanelMotion::LineEnd => layout
                .line_bounds(row)
                .map_or(self.scrollback.cursor, |bounds| bounds.1),
            TextPanelMotion::NextWord => {
                word_motion(&layout, self.scrollback.cursor, count, WordMotion::Next)
            }
            TextPanelMotion::PreviousWord => {
                word_motion(&layout, self.scrollback.cursor, count, WordMotion::Previous)
            }
            TextPanelMotion::WordEnd => {
                word_motion(&layout, self.scrollback.cursor, count, WordMotion::End)
            }
            TextPanelMotion::PreviousParagraph | TextPanelMotion::NextParagraph => {
                let mut target_row = row;
                for _ in 0..count {
                    target_row = if matches!(motion, TextPanelMotion::PreviousParagraph) {
                        (0..target_row)
                            .rev()
                            .find(|candidate| {
                                !layout.lines[*candidate].cells.is_empty()
                                    && (*candidate == 0
                                        || layout.lines[candidate.saturating_sub(1)]
                                            .cells
                                            .is_empty())
                            })
                            .unwrap_or(0)
                    } else {
                        ((target_row + 1)..layout.lines.len())
                            .find(|candidate| {
                                !layout.lines[*candidate].cells.is_empty()
                                    && layout.lines[candidate.saturating_sub(1)].cells.is_empty()
                            })
                            .unwrap_or(layout.lines.len().saturating_sub(1))
                    };
                }
                layout
                    .nearest_offset_on_row(target_row, column)
                    .unwrap_or(self.scrollback.cursor)
            }
            TextPanelMotion::PageUp | TextPanelMotion::HalfPageUp => {
                let amount = if matches!(motion, TextPanelMotion::PageUp) {
                    visible_rows
                } else {
                    visible_rows.div_ceil(2)
                };
                let target_row = row.saturating_sub(amount.saturating_mul(count));
                layout
                    .offset_at_or_before(target_row, column)
                    .or_else(|| layout.offset_at_or_after(target_row, column))
                    .unwrap_or(self.scrollback.cursor)
            }
            TextPanelMotion::PageDown | TextPanelMotion::HalfPageDown => {
                let amount = if matches!(motion, TextPanelMotion::PageDown) {
                    visible_rows
                } else {
                    visible_rows.div_ceil(2)
                };
                let target_row = row
                    .saturating_add(amount.saturating_mul(count))
                    .min(layout.lines.len().saturating_sub(1));
                layout
                    .offset_at_or_after(target_row, column)
                    .or_else(|| layout.offset_at_or_before(target_row, column))
                    .unwrap_or(self.scrollback.cursor)
            }
            TextPanelMotion::ViewportTop => layout
                .offset_at_or_after(self.scroll, column)
                .or_else(|| layout.offset_at_or_before(self.scroll, column))
                .unwrap_or(self.scrollback.cursor),
            TextPanelMotion::ViewportMiddle => layout
                .nearest_offset_on_row(self.scroll.saturating_add(visible_rows / 2), column)
                .unwrap_or(self.scrollback.cursor),
            TextPanelMotion::ViewportBottom => layout
                .offset_at_or_before(
                    self.scroll
                        .saturating_add(visible_rows.saturating_sub(1))
                        .min(layout.lines.len().saturating_sub(1)),
                    column,
                )
                .or_else(|| layout.offset_at_or_after(self.scroll, column))
                .unwrap_or(self.scrollback.cursor),
            TextPanelMotion::Top => 0,
            TextPanelMotion::Bottom => layout.len - 1,
        };
        self.scrollback.cursor = layout.clamp(target);
        if !matches!(motion, TextPanelMotion::Up | TextPanelMotion::Down) {
            self.scrollback.preferred_column = None;
        }
        if matches!(motion, TextPanelMotion::Bottom) {
            self.scroll_to_bottom(panel_height, width);
        } else {
            self.follow_tail = false;
            self.reveal_scrollback_cursor(&layout, panel_height, width);
        }
        self.selected_link = None;
    }

    fn yank_scrollback(&mut self, width: usize) -> Option<TextPanelYank> {
        let layout = self.layout(width);
        let (start, end) = self.selection_bounds(&layout)?;
        let linewise = self.scrollback.mode == TextPanelScrollbackMode::VisualLine;
        let text = layout.selected_text(start, end, linewise);
        if text.is_empty() {
            return None;
        }
        self.scrollback.cursor = start;
        self.scrollback.mode = TextPanelScrollbackMode::Normal;
        self.scrollback.selection_anchor = None;
        Some(TextPanelYank { text, linewise })
    }

    fn find_in_scrollback(
        &mut self,
        find: ScrollbackFind,
        count: usize,
        panel_height: usize,
        width: usize,
    ) {
        let layout = self.layout(width);
        let Some((row, _, _)) = layout.position(self.scrollback.cursor) else {
            return;
        };
        let Some((line_start, line_end)) = layout.line_bounds(row) else {
            return;
        };
        let cells = layout_cells(&layout);
        let matches_target = |offset: usize| {
            cells
                .get(offset)
                .is_some_and(|cell| cell.text.starts_with(find.target))
        };
        let target = match find.direction {
            ScrollbackFindDirection::Forward => ((self.scrollback.cursor + 1)..=line_end)
                .filter(|offset| matches_target(*offset))
                .nth(count.saturating_sub(1))
                .map(|offset| {
                    if find.till {
                        offset.saturating_sub(1)
                    } else {
                        offset
                    }
                }),
            ScrollbackFindDirection::Backward => (line_start..self.scrollback.cursor)
                .rev()
                .filter(|offset| matches_target(*offset))
                .nth(count.saturating_sub(1))
                .map(|offset| {
                    if find.till {
                        offset.saturating_add(1).min(line_end)
                    } else {
                        offset
                    }
                }),
        };
        if let Some(target) = target {
            self.scrollback.cursor = target;
            self.scrollback.preferred_column = None;
            self.follow_tail = false;
            self.reveal_scrollback_cursor(&layout, panel_height, width);
            self.selected_link = None;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WordClass {
    Whitespace,
    Word,
    Punctuation,
}

#[derive(Debug, Clone, Copy)]
enum WordMotion {
    Next,
    Previous,
    End,
}

fn word_class(cell: &TextPanelLayoutCell) -> WordClass {
    let mut chars = cell.text.chars();
    let Some(character) = chars.next() else {
        return WordClass::Whitespace;
    };
    if character.is_whitespace() {
        WordClass::Whitespace
    } else if character.is_alphanumeric() || character == '_' {
        WordClass::Word
    } else {
        WordClass::Punctuation
    }
}

fn layout_cells(layout: &TextPanelLayout) -> Vec<&TextPanelLayoutCell> {
    layout.lines.iter().flat_map(|line| &line.cells).collect()
}

fn word_motion(layout: &TextPanelLayout, offset: usize, count: usize, motion: WordMotion) -> usize {
    let cells = layout_cells(layout);
    if cells.is_empty() {
        return 0;
    }
    let mut cursor = offset.min(cells.len() - 1);
    for _ in 0..count.max(1) {
        cursor = match motion {
            WordMotion::Next => {
                let class = word_class(cells[cursor]);
                let mut next = cursor.saturating_add(1);
                while next < cells.len() && word_class(cells[next]) == class {
                    next += 1;
                }
                while next < cells.len() && word_class(cells[next]) == WordClass::Whitespace {
                    next += 1;
                }
                next.min(cells.len() - 1)
            }
            WordMotion::Previous => {
                let mut previous = cursor.saturating_sub(1);
                while previous > 0 && word_class(cells[previous]) == WordClass::Whitespace {
                    previous -= 1;
                }
                let class = word_class(cells[previous]);
                while previous > 0 && word_class(cells[previous - 1]) == class {
                    previous -= 1;
                }
                previous
            }
            WordMotion::End => {
                let mut end = cursor;
                if end + 1 < cells.len() {
                    end += 1;
                }
                while end < cells.len() && word_class(cells[end]) == WordClass::Whitespace {
                    end += 1;
                }
                if end >= cells.len() {
                    cells.len() - 1
                } else {
                    let class = word_class(cells[end]);
                    while end + 1 < cells.len() && word_class(cells[end + 1]) == class {
                        end += 1;
                    }
                    end
                }
            }
        };
    }
    cursor
}

fn namespace_block_links(lines: &mut [RenderedTextLine], block_index: usize) {
    let namespace = (block_index as u64).saturating_add(1) << 32;
    for span in lines.iter_mut().flat_map(|line| &mut line.spans) {
        if let Some(link) = span.link.as_mut() {
            link.id |= namespace;
        }
    }
}

pub struct PluginPanel {
    pub id: String,
    pub config: PanelConfig,
    pub rows: Vec<PanelRow>,
    row_positions: HashMap<u64, usize>,
    tree: Option<TreePanelModel>,
    pub selected: usize,
    pub scroll: usize,
    search: Option<RowPanelSearch>,
    status: String,
}

struct RowPanelSearch {
    input: PromptBuffer,
    prefix: String,
    status: String,
    active: bool,
}

fn path_hash(path: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish()
}

impl PluginPanel {
    pub fn new(id: String, config: PanelConfig) -> Self {
        Self {
            id,
            config,
            rows: Vec::new(),
            row_positions: HashMap::new(),
            tree: None,
            selected: 0,
            scroll: 0,
            search: None,
            status: String::new(),
        }
    }

    pub fn update_rows(&mut self, rows: Vec<PanelRow>) {
        self.tree = None;
        self.row_positions.clear();
        self.row_positions.reserve(rows.len());
        for (index, row) in rows.iter().enumerate() {
            self.row_positions
                .entry(path_hash(&row.id))
                .or_insert(index);
        }
        self.rows = rows;
        if self.row_count() == 0 {
            self.selected = 0;
            self.scroll = 0;
        } else if self.selected >= self.row_count() {
            self.selected = self.row_count() - 1;
        }

        if self.scroll > self.selected {
            self.scroll = self.selected;
        }
    }

    fn update_tree(&mut self, model: TreePanelModel) {
        let selected_id = self.selected_row().map(|row| row.id);
        self.rows.clear();
        self.row_positions.clear();
        self.tree = Some(model);
        let count = self.row_count();
        if count == 0 {
            self.selected = 0;
            self.scroll = 0;
            return;
        }
        self.selected = selected_id
            .as_deref()
            .and_then(|id| self.tree.as_ref()?.position(id))
            .unwrap_or_else(|| self.selected.min(count - 1));
        if self.scroll > self.selected {
            self.scroll = self.selected;
        }
    }

    fn row_count(&self) -> usize {
        self.tree
            .as_ref()
            .map_or(self.rows.len(), TreePanelModel::len)
    }

    fn row(&self, index: usize) -> Option<PanelRow> {
        self.tree
            .as_ref()
            .map_or_else(|| self.rows.get(index).cloned(), |tree| tree.row(index))
    }

    pub fn move_selection(&mut self, delta: isize, panel_height: usize) {
        if self.row_count() == 0 {
            return;
        }

        let max_index = self.row_count() - 1;
        self.selected = self.selected.saturating_add_signed(delta).min(max_index);

        if self.selected < self.scroll {
            self.scroll = self.selected;
        }

        let visible_rows = self.visible_rows(panel_height);
        if self.selected >= self.scroll + visible_rows {
            self.scroll = self.selected.saturating_sub(visible_rows - 1);
        }
    }

    fn scroll_view(&mut self, delta: isize, panel_height: usize, scrolloff: usize) {
        if self.row_count() == 0 {
            self.selected = 0;
            self.scroll = 0;
            return;
        }

        let visible_rows = self.visible_rows(panel_height);
        let max_scroll = self.row_count().saturating_sub(visible_rows);
        let previous_scroll = self.scroll;
        self.scroll = self.scroll.saturating_add_signed(delta).min(max_scroll);
        if self.scroll == previous_scroll {
            return;
        }

        let scrolloff = scrolloff.min(visible_rows.saturating_sub(1) / 2);
        let first = self.scroll.saturating_add(scrolloff);
        let last = self
            .scroll
            .saturating_add(visible_rows.saturating_sub(scrolloff).saturating_sub(1))
            .min(self.row_count() - 1);
        self.selected = self.selected.clamp(first, last);
    }

    fn page_scroll(&mut self, direction: isize, panel_height: usize, scrolloff: usize) {
        let page_rows = self.visible_rows(panel_height).saturating_sub(2).max(1);
        let page_rows = isize::try_from(page_rows).unwrap_or(isize::MAX);
        self.scroll_view(direction.saturating_mul(page_rows), panel_height, scrolloff);
    }

    fn scroll_to_top(&mut self) {
        self.selected = 0;
        self.scroll = 0;
    }

    fn scroll_to_bottom(&mut self, panel_height: usize) {
        if self.row_count() == 0 {
            self.scroll_to_top();
            return;
        }

        self.selected = self.row_count() - 1;
        self.scroll = self
            .row_count()
            .saturating_sub(self.visible_rows(panel_height));
    }

    pub fn select_row_by_id(&mut self, row_id: &str, panel_height: usize) -> bool {
        let index = self.tree.as_ref().map_or_else(
            || {
                self.row_positions
                    .get(&path_hash(row_id))
                    .copied()
                    .filter(|index| self.rows.get(*index).is_some_and(|row| row.id == row_id))
                    .or_else(|| self.rows.iter().position(|row| row.id == row_id))
            },
            |tree| tree.position(row_id),
        );
        let Some(index) = index else {
            return false;
        };

        self.selected = index;
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }

        let visible_rows = self.visible_rows(panel_height);
        if self.selected >= self.scroll + visible_rows {
            self.scroll = self.selected.saturating_sub(visible_rows - 1);
        }

        true
    }

    pub fn selected_row(&self) -> Option<PanelRow> {
        self.row(self.selected)
    }

    fn rows_start(&self) -> usize {
        usize::from(self.config.title.is_some())
    }

    fn visible_rows(&self, panel_height: usize) -> usize {
        panel_height
            .saturating_sub(self.rows_start())
            .saturating_sub(usize::from(self.search.is_some()))
            .saturating_sub(usize::from(!self.status.is_empty()))
            .max(1)
    }

    fn select_screen_row(&mut self, screen_y: usize) {
        let rows_start = self.rows_start();
        if screen_y < rows_start || self.row_count() == 0 {
            return;
        }

        let row_index = self.scroll + screen_y - rows_start;
        if row_index < self.row_count() {
            self.selected = row_index;
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PanelAxisSize {
    default: Option<usize>,
    preferred: Option<usize>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PanelSizes {
    vertical: PanelAxisSize,
    horizontal: PanelAxisSize,
}

impl PanelSizes {
    fn axis(&self, side: PanelSide) -> &PanelAxisSize {
        if matches!(side, PanelSide::Left | PanelSide::Right) {
            &self.vertical
        } else {
            &self.horizontal
        }
    }

    fn axis_mut(&mut self, side: PanelSide) -> &mut PanelAxisSize {
        if matches!(side, PanelSide::Left | PanelSide::Right) {
            &mut self.vertical
        } else {
            &mut self.horizontal
        }
    }

    fn remember(&mut self, side: PanelSide, size: usize) {
        let axis = self.axis_mut(side);
        axis.default.get_or_insert(size);
        axis.preferred = Some(size);
    }
}

/// Transient presentation; logical visibility and saved docking remain unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum PanelPresentation {
    #[default]
    Docked,
    Hidden,
    Zoomed {
        id: String,
        size: (usize, usize),
    },
}

impl PanelPresentation {
    fn zoom_size(&self) -> Option<(usize, usize)> {
        match self {
            Self::Zoomed { size, .. } => Some(*size),
            _ => None,
        }
    }

    fn height(&self, id: &str, config: &PanelConfig, available_height: usize) -> usize {
        match self {
            Self::Zoomed { id: target, size } if target == id => size.1,
            _ => effective_panel_height(config, available_height),
        }
    }
}

const ROW_DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(400);

struct RowClick {
    panel_id: String,
    row_id: String,
    at: Instant,
}

struct TextLinkClick {
    panel_id: String,
    link_id: u64,
    at: Instant,
}

#[derive(Default)]
pub struct PanelManager {
    presentation: PanelPresentation,
    panels: HashMap<String, PluginPanel>,
    text_panels: HashMap<String, TextPanel>,
    default_layouts: HashMap<String, (PanelSide, usize)>,
    preferred_sizes: HashMap<String, PanelSizes>,
    z_order: Vec<String>,
    focused: Option<String>,
    animation_state: Vec<(String, u8, u64)>,
    pending_restore: HashMap<String, PendingPanelRestore>,
    pending_focused: Option<String>,
    last_row_click: Option<RowClick>,
    last_text_link_click: Option<TextLinkClick>,
}

impl PanelManager {
    pub(crate) fn set_presentation(&mut self, presentation: PanelPresentation) {
        if self.presentation == presentation {
            return;
        }
        if let Some((terminal_width, content_height)) = presentation
            .zoom_size()
            .or_else(|| self.presentation.zoom_size())
        {
            let widths = self
                .text_panels
                .keys()
                .map(|id| {
                    (
                        id.clone(),
                        (
                            self.text_panel_width_in(&self.presentation, id, terminal_width),
                            self.text_panel_width_in(&presentation, id, terminal_width),
                        ),
                    )
                })
                .collect::<HashMap<_, _>>();
            for panel in self.text_panels.values_mut() {
                let (old_width, width) = widths[&panel.id];
                let old_height = self
                    .presentation
                    .height(&panel.id, &panel.config, content_height);
                let height = presentation.height(&panel.id, &panel.config, content_height);
                if (old_width, old_height) == (width, height) {
                    continue;
                }
                if panel.follow_tail {
                    panel.scroll_to_bottom(height, width);
                    continue;
                }
                // Reflow by source offset, not by a row number from the old width.
                let old_layout = panel.layout(old_width);
                let anchor = (panel.scroll..old_layout.lines.len())
                    .find_map(|row| old_layout.offset_at(row, 0));
                let layout = panel.layout(width);
                panel.scroll = anchor
                    .and_then(|offset| layout.position(offset))
                    .map_or(0, |(row, _, _)| row);
                panel.clamp_scroll(height, width);
                if panel.scrollback.focused {
                    panel.reveal_scrollback_cursor(&layout, height, width);
                }
            }
        }
        self.presentation = presentation;
    }

    pub(crate) fn is_visible(&self, id: &str) -> bool {
        self.z_order.iter().any(|candidate| candidate == id) && self.panel_config(id).is_some()
    }

    /// Captures durable, plugin-independent pane state by stable resource ID.
    pub fn snapshot(&self, terminal_width: usize) -> PanelManagerSnapshot {
        let panels = self
            .panel_ids()
            .into_iter()
            .filter_map(|id| {
                let config = self.panel_config(&id)?;
                let visible = self.z_order.iter().position(|candidate| candidate == &id);
                let sizes = self.preferred_sizes.get(&id).copied().unwrap_or_default();
                let vertical_size = sizes.vertical.preferred.or_else(|| {
                    matches!(config.side, PanelSide::Left | PanelSide::Right)
                        .then_some(config.width)
                });
                let horizontal_size = sizes.horizontal.preferred.or_else(|| {
                    matches!(config.side, PanelSide::Top | PanelSide::Bottom)
                        .then_some(config.width)
                });

                if let Some(panel) = self.panels.get(&id) {
                    return Some(PanelSessionSnapshot {
                        id,
                        kind: PanelSnapshotKind::Row,
                        visible: visible.is_some(),
                        z_index: visible,
                        side: config.side,
                        vertical_size,
                        horizontal_size,
                        selected_row_id: panel.selected_row().map(|row| row.id),
                        row_scroll: panel.scroll,
                        text: None,
                    });
                }

                let panel = self.text_panels.get(&id)?;
                let width = self.text_panel_width(&panel.id, terminal_width);
                let layout = panel.layout(width);
                let scroll_anchor = (!panel.follow_tail).then(|| {
                    (panel.scroll..layout.lines.len())
                        .find_map(|row| layout.offset_at(row, 0))
                        .unwrap_or(0)
                });
                let composer_focused = panel.composer.as_ref().is_some_and(|composer| {
                    composer
                        .temporary_context
                        .as_ref()
                        .map_or(composer.focused, |context| context.focused)
                });
                let last_focused_region = panel
                    .temporary_context
                    .as_ref()
                    .map_or(panel.last_focused_region, |context| {
                        context.last_focused_region
                    });
                let focus =
                    if composer_focused || last_focused_region == TextPanelFocusRegion::Composer {
                        TextPanelSnapshotFocus::Composer
                    } else {
                        TextPanelSnapshotFocus::Scrollback
                    };
                let composer = panel.composer.as_ref().map(|composer| {
                    let prompt = composer
                        .temporary_context
                        .as_ref()
                        .map_or(&composer.prompt, |context| &context.prompt);
                    TextPanelComposerSnapshot {
                        text: prompt.text(),
                        cursor: prompt.cursor(),
                    }
                });
                Some(PanelSessionSnapshot {
                    id,
                    kind: PanelSnapshotKind::Text,
                    visible: visible.is_some(),
                    z_index: visible,
                    side: config.side,
                    vertical_size,
                    horizontal_size,
                    selected_row_id: None,
                    row_scroll: 0,
                    text: Some(TextPanelSessionSnapshot {
                        follow_tail: panel.follow_tail,
                        scroll_anchor,
                        cursor: panel.scrollback.cursor,
                        focus,
                        composer,
                    }),
                })
            })
            .collect();
        PanelManagerSnapshot {
            panels,
            focused: self.focused.clone(),
        }
    }

    /// Stages pane state until the owning plugins recreate their resources.
    pub fn stage_restore(&mut self, snapshot: PanelManagerSnapshot) {
        self.pending_restore = snapshot
            .panels
            .into_iter()
            .map(|snapshot| {
                (
                    snapshot.id.clone(),
                    PendingPanelRestore {
                        snapshot,
                        shell_applied: false,
                    },
                )
            })
            .collect();
        self.pending_focused = snapshot.focused;
        self.focus_editor();
    }

    /// Returns the still-unclaimed restoration intents sent to plugin owners.
    pub fn pending_restore_snapshot(&self) -> PanelManagerSnapshot {
        let mut panels = self
            .pending_restore
            .values()
            .map(|pending| pending.snapshot.clone())
            .collect::<Vec<_>>();
        panels.sort_by(|left, right| left.id.cmp(&right.id));
        PanelManagerSnapshot {
            panels,
            focused: self.pending_focused.clone(),
        }
    }

    /// Applies layout, visibility, and stacking after a plugin recreates a pane.
    pub fn apply_pending_shell_restore(&mut self, id: &str) -> bool {
        let Some(pending) = self.pending_restore.get(id) else {
            return false;
        };
        let snapshot = pending.snapshot.clone();
        let live_kind = if self.panels.contains_key(id) {
            PanelSnapshotKind::Row
        } else if self.text_panels.contains_key(id) {
            PanelSnapshotKind::Text
        } else {
            return false;
        };
        if live_kind != snapshot.kind {
            self.pending_restore.remove(id);
            return false;
        }

        let size = if matches!(snapshot.side, PanelSide::Left | PanelSide::Right) {
            snapshot.vertical_size
        } else {
            snapshot.horizontal_size
        }
        .or_else(|| self.panel_layout(id).map(|(_, size)| size))
        .unwrap_or_default();
        self.restore_panel_layout(
            id,
            snapshot.side,
            size,
            snapshot.vertical_size,
            snapshot.horizontal_size,
        );

        self.z_order.retain(|candidate| candidate != id);
        if snapshot.visible {
            let index = snapshot.z_index.unwrap_or(self.z_order.len());
            self.z_order
                .insert(index.min(self.z_order.len()), id.to_string());
        }
        if let Some(pending) = self.pending_restore.get_mut(id) {
            pending.shell_applied = true;
        }
        true
    }

    /// Applies content-relative state once the plugin has repopulated the pane.
    pub fn apply_pending_content_restore(
        &mut self,
        id: &str,
        panel_height: usize,
        terminal_width: usize,
    ) -> bool {
        let Some(pending) = self.pending_restore.get(id) else {
            return false;
        };
        if !pending.shell_applied {
            return false;
        }
        let snapshot = pending.snapshot.clone();

        let applied = match snapshot.kind {
            PanelSnapshotKind::Row => {
                let Some(panel) = self.panels.get_mut(id) else {
                    return false;
                };
                if let Some(row_id) = snapshot.selected_row_id.as_deref() {
                    if !panel.select_row_by_id(row_id, panel_height) {
                        return false;
                    }
                }
                panel.scroll = snapshot.row_scroll.min(panel.selected);
                true
            }
            PanelSnapshotKind::Text => {
                let Some(saved) = snapshot.text else {
                    return false;
                };
                let width = self.text_panel_width(id, terminal_width);
                let Some(panel) = self.text_panels.get_mut(id) else {
                    return false;
                };
                let panel_height = self
                    .presentation
                    .height(&panel.id, &panel.config, panel_height);
                if let (Some(composer), Some(saved_composer)) =
                    (panel.composer.as_mut(), saved.composer)
                {
                    composer.prompt.set_text(&saved_composer.text);
                    composer.prompt.set_cursor(saved_composer.cursor);
                }

                let layout = panel.layout(width);
                panel.scrollback.mode = TextPanelScrollbackMode::Normal;
                panel.scrollback.selection_anchor = None;
                panel.scrollback.cursor = layout.clamp(saved.cursor);
                panel.last_focused_region = match saved.focus {
                    TextPanelSnapshotFocus::Scrollback => TextPanelFocusRegion::Scrollback,
                    TextPanelSnapshotFocus::Composer => TextPanelFocusRegion::Composer,
                };
                if saved.follow_tail {
                    panel.scroll_to_bottom(panel_height, width);
                } else {
                    let row = saved
                        .scroll_anchor
                        .and_then(|anchor| layout.position(layout.clamp(anchor)))
                        .map_or(0, |(row, _, _)| row);
                    panel.scroll = row;
                    panel.follow_tail = false;
                    panel.clamp_scroll(panel_height, width);
                }
                true
            }
        };

        if applied {
            let should_focus = self.pending_focused.as_deref() == Some(id);
            self.pending_restore.remove(id);
            if should_focus {
                self.pending_focused = None;
                self.restore_panel_focus(id);
            }
        }
        applied
    }

    /// Applies a staged snapshot to resources that were already live when it loaded.
    pub fn apply_pending_to_existing(&mut self, panel_height: usize, terminal_width: usize) {
        for id in self.panel_ids() {
            self.apply_pending_shell_restore(&id);
            self.apply_pending_content_restore(&id, panel_height, terminal_width);
        }
    }

    pub fn create_panel(&mut self, id: String, config: PanelConfig) {
        self.default_layouts
            .insert(id.clone(), (config.side, config.width));
        self.remember_panel_size(&id, config.side, config.width);
        self.text_panels.remove(&id);
        self.panels
            .insert(id.clone(), PluginPanel::new(id.clone(), config));
        if !self.z_order.contains(&id) {
            self.z_order.push(id.clone());
        }
    }

    pub fn create_text_panel(&mut self, id: String, config: PanelConfig) {
        self.default_layouts
            .insert(id.clone(), (config.side, config.width));
        self.remember_panel_size(&id, config.side, config.width);
        self.panels.remove(&id);
        self.text_panels
            .insert(id.clone(), TextPanel::new(id.clone(), config));
        if !self.z_order.contains(&id) {
            self.z_order.push(id);
        }
    }

    pub fn set_text_panel_header_detail(
        &mut self,
        id: &str,
        detail: Option<TextPanelHeaderDetail>,
    ) -> bool {
        let Some(panel) = self.text_panels.get_mut(id) else {
            return false;
        };
        if panel.header_detail == detail {
            return false;
        }
        panel.header_detail = detail;
        true
    }

    pub fn set_text_panel_actions(
        &mut self,
        id: &str,
        actions: Vec<TextPanelHeaderAction>,
    ) -> bool {
        let Some(panel) = self.text_panels.get_mut(id) else {
            return false;
        };
        if panel.config.header_actions == actions {
            return false;
        }
        panel.config.header_actions = actions;
        true
    }

    pub fn update_text_panel(
        &mut self,
        id: &str,
        blocks: Vec<TextPanelBlock>,
        panel_height: usize,
        terminal_width: usize,
    ) {
        let width = self.text_panel_width(id, terminal_width);
        if let Some(panel) = self.text_panels.get_mut(id) {
            let panel_height = self
                .presentation
                .height(&panel.id, &panel.config, panel_height);
            panel.update_blocks(blocks, panel_height, width);
        }
    }

    pub fn update_text_panel_with_highlighter(
        &mut self,
        id: &str,
        blocks: Vec<TextPanelBlock>,
        highlighter: &mut Highlighter,
        panel_height: usize,
        terminal_width: usize,
    ) {
        let width = self.text_panel_width(id, terminal_width);
        if let Some(panel) = self.text_panels.get_mut(id) {
            let panel_height = self
                .presentation
                .height(&panel.id, &panel.config, panel_height);
            panel.update_blocks_with_highlighter(blocks, Some(highlighter), panel_height, width);
        }
    }

    pub fn rehighlight_text_panels(&mut self, highlighter: &mut Highlighter) {
        for panel in self.text_panels.values_mut() {
            panel.refresh_block_styles(Some(highlighter));
        }
    }

    pub fn append_text_panel(
        &mut self,
        id: &str,
        block_id: &str,
        delta: &str,
        panel_height: usize,
        terminal_width: usize,
    ) {
        let width = self.text_panel_width(id, terminal_width);
        if let Some(panel) = self.text_panels.get_mut(id) {
            let panel_height = self
                .presentation
                .height(&panel.id, &panel.config, panel_height);
            panel.append_delta(block_id, delta, panel_height, width);
        }
    }

    pub fn append_text_panel_with_highlighter(
        &mut self,
        id: &str,
        block_id: &str,
        delta: &str,
        highlighter: &mut Highlighter,
        panel_height: usize,
        terminal_width: usize,
    ) {
        let width = self.text_panel_width(id, terminal_width);
        if let Some(panel) = self.text_panels.get_mut(id) {
            let panel_height = self
                .presentation
                .height(&panel.id, &panel.config, panel_height);
            panel.append_delta_with_highlighter(
                block_id,
                delta,
                Some(highlighter),
                panel_height,
                width,
            );
        }
    }

    pub fn update_panel(&mut self, id: &str, rows: Vec<PanelRow>) {
        if let Some(panel) = self.panels.get_mut(id) {
            panel.update_rows(rows);
        }
    }

    /// Replaces a row panel with a compact tree while preserving its stable selection.
    pub fn update_tree_panel(&mut self, id: &str, model: TreePanelModel) {
        if let Some(panel) = self.panels.get_mut(id) {
            panel.update_tree(model);
        }
    }

    pub fn open_panel_search(&mut self, id: &str, initial: &str, prefix: &str) -> bool {
        let Some(panel) = self.panels.get_mut(id) else {
            return false;
        };
        panel.search = Some(RowPanelSearch {
            input: PromptBuffer::new(initial).with_key_policy(PromptKeyPolicy::EnterSends),
            prefix: prefix.to_string(),
            status: String::new(),
            active: true,
        });
        self.focused = Some(id.to_string());
        true
    }

    pub fn update_panel_search(&mut self, id: &str, status: String) -> bool {
        let Some(search) = self
            .panels
            .get_mut(id)
            .and_then(|panel| panel.search.as_mut())
        else {
            return false;
        };
        if search.status == status {
            return false;
        }
        search.status = status;
        true
    }

    /// Updates nonselectable footer text without rebuilding the filesystem tree.
    pub fn set_panel_status(&mut self, id: &str, status: String) -> bool {
        let Some(panel) = self.panels.get_mut(id) else {
            return false;
        };
        if panel.status == status {
            return false;
        }
        panel.status = status;
        true
    }

    pub fn keep_panel_search(&mut self, id: &str) -> bool {
        let Some(search) = self
            .panels
            .get_mut(id)
            .and_then(|panel| panel.search.as_mut())
        else {
            return false;
        };
        search.active = false;
        true
    }

    pub fn close_panel_search(&mut self, id: &str) -> bool {
        self.panels
            .get_mut(id)
            .is_some_and(|panel| panel.search.take().is_some())
    }

    pub(crate) fn focused_row_search_active(&self) -> bool {
        self.focused
            .as_deref()
            .and_then(|id| self.panels.get(id))
            .and_then(|panel| panel.search.as_ref())
            .is_some_and(|search| search.active)
    }

    pub(crate) fn handle_focused_row_search(
        &mut self,
        event: &Event,
        terminal_width: usize,
        panel_height: usize,
    ) -> Option<PanelEvent> {
        let id = self.focused.clone()?;
        let panel = self.panels.get_mut(&id)?;
        let search = panel.search.as_mut().filter(|search| search.active)?;
        let Event::Key(key) = event else {
            if let Event::Paste(_) = event {
                let outcome = search.input.handle_event_with_layout_options(
                    event,
                    LayoutOptions::grapheme(terminal_width.max(1)),
                );
                if outcome == PromptInput::Changed {
                    let text = search.input.text();
                    return Some(PanelEvent {
                        panel_id: id,
                        action: "search_query".to_string(),
                        selected_index: panel.selected,
                        row: panel.selected_row(),
                        text: Some(text),
                    });
                }
            }
            return None;
        };

        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let action = match key.code {
            KeyCode::Esc => "search_cancel",
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => "search_keep",
            KeyCode::Enter if control => "search_reveal",
            KeyCode::Up | KeyCode::Char('p') if key.code == KeyCode::Up || control => {
                panel.move_selection(-1, panel_height);
                "search_move"
            }
            KeyCode::Down | KeyCode::Char('n') if key.code == KeyCode::Down || control => {
                panel.move_selection(1, panel_height);
                "search_move"
            }
            _ => {
                let outcome = search.input.handle_event_with_layout_options(
                    event,
                    LayoutOptions::grapheme(terminal_width.max(1)),
                );
                match outcome {
                    PromptInput::Changed => "search_query",
                    PromptInput::Submit => "search_submit",
                    PromptInput::Cancel => "search_cancel",
                    PromptInput::Unhandled => return None,
                }
            }
        };
        let text = panel.search.as_ref().map(|search| search.input.text());
        Some(PanelEvent {
            panel_id: id,
            action: action.to_string(),
            selected_index: panel.selected,
            row: panel.selected_row(),
            text,
        })
    }

    /// Moves or resizes a stable panel without replacing its contents or focus.
    pub fn update_panel_layout(&mut self, id: &str, side: PanelSide, width: usize) -> bool {
        let changed = if let Some(panel) = self.panels.get_mut(id) {
            if panel.config.side == side && panel.config.width == width {
                return false;
            }
            panel.config.side = side;
            panel.config.width = width;
            true
        } else if let Some(panel) = self.text_panels.get_mut(id) {
            if panel.config.side == side && panel.config.width == width {
                return false;
            }
            panel.config.side = side;
            panel.config.width = width;
            true
        } else {
            false
        };

        if changed {
            self.remember_panel_size(id, side, width);
            self.mark_user_sized(id);
        }
        changed
    }

    fn mark_user_sized(&mut self, id: &str) {
        let default = self.default_layouts.get(id).copied();
        if let Some(panel) = self.text_panels.get_mut(id) {
            panel.user_sized = default != Some((panel.config.side, panel.config.width));
        }
    }

    fn is_user_sized(&self, id: &str) -> bool {
        self.text_panels
            .get(id)
            .is_some_and(|panel| panel.user_sized)
    }

    /// Applies a persisted layout, including preferences for both docking axes.
    pub fn restore_panel_layout(
        &mut self,
        id: &str,
        side: PanelSide,
        size: usize,
        vertical_size: Option<usize>,
        horizontal_size: Option<usize>,
    ) -> bool {
        let Some(config) = self.panel_config_mut(id) else {
            return false;
        };
        let changed = config.side != side || config.width != size;
        config.side = side;
        config.width = size;
        if let Some(vertical_size) = vertical_size {
            self.remember_panel_size(id, PanelSide::Left, vertical_size);
        }
        if let Some(horizontal_size) = horizontal_size {
            self.remember_panel_size(id, PanelSide::Top, horizontal_size);
        }
        self.mark_user_sized(id);
        changed
    }

    /// Returns the docking edge and requested size of a stable panel.
    pub fn panel_layout(&self, id: &str) -> Option<(PanelSide, usize)> {
        self.panel_config(id)
            .map(|config| (config.side, config.width))
    }

    /// Returns the last chosen panel size for the requested docking axis.
    pub fn panel_preferred_size(&self, id: &str, side: PanelSide) -> Option<usize> {
        self.preferred_sizes.get(id)?.axis(side).preferred
    }

    /// Returns the first configured panel size for the requested docking axis.
    pub fn panel_default_size(&self, id: &str, side: PanelSide) -> Option<usize> {
        self.preferred_sizes.get(id)?.axis(side).default
    }

    /// Returns the docking edge and size supplied when the panel was created.
    pub fn panel_default_layout(&self, id: &str) -> Option<(PanelSide, usize)> {
        self.default_layouts.get(id).copied()
    }

    /// Returns every live panel ID, including panels that are currently hidden.
    pub fn panel_ids(&self) -> Vec<String> {
        let mut ids = self
            .panels
            .keys()
            .chain(self.text_panels.keys())
            .cloned()
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }

    /// Restores a panel's creation layout and forgets sizes chosen on other axes.
    pub fn reset_panel_layout(&mut self, id: &str) -> bool {
        let Some((side, size)) = self.panel_default_layout(id) else {
            return false;
        };
        let changed = self.panel_layout(id) != Some((side, size));
        if let Some(panel) = self.panels.get_mut(id) {
            panel.config.side = side;
            panel.config.width = size;
        } else if let Some(panel) = self.text_panels.get_mut(id) {
            panel.config.side = side;
            panel.config.width = size;
        } else {
            return false;
        }
        self.preferred_sizes.remove(id);
        self.remember_panel_size(id, side, size);
        self.mark_user_sized(id);
        changed
    }

    fn remember_panel_size(&mut self, id: &str, side: PanelSide, size: usize) {
        self.preferred_sizes
            .entry(id.to_string())
            .or_default()
            .remember(side, size);
    }

    pub fn close_panel(&mut self, id: &str) {
        self.panels.remove(id);
        self.text_panels.remove(id);
        self.default_layouts.remove(id);
        self.preferred_sizes.remove(id);
        self.z_order.retain(|panel_id| panel_id != id);
        if self.focused.as_deref() == Some(id) {
            self.focused = None;
        }
    }

    pub fn set_panel_visible(&mut self, id: &str, visible: bool) -> bool {
        if !self.panels.contains_key(id) && !self.text_panels.contains_key(id) {
            return false;
        }

        if visible {
            if !self.z_order.iter().any(|panel_id| panel_id == id) {
                self.z_order.push(id.to_string());
            }
        } else {
            self.z_order.retain(|panel_id| panel_id != id);
            if self.focused.as_deref() == Some(id) {
                self.focus_editor();
            }
        }
        true
    }

    pub fn hide_all_panels(&mut self) -> Vec<String> {
        self.focus_editor();
        std::mem::take(&mut self.z_order)
    }

    pub fn focus_panel(&mut self, id: &str) -> bool {
        if self.z_order.iter().any(|panel_id| panel_id == id)
            && (self.panels.contains_key(id) || self.text_panels.contains_key(id))
        {
            self.focused = Some(id.to_string());
            let width = self.text_panel_width(id, usize::MAX);
            if let Some(panel) = self.text_panels.get_mut(id) {
                panel.focus_scrollback(width);
            }
            true
        } else {
            false
        }
    }

    pub fn restore_panel_focus(&mut self, id: &str) -> bool {
        if !self.z_order.iter().any(|panel_id| panel_id == id)
            || (!self.panels.contains_key(id) && !self.text_panels.contains_key(id))
        {
            return false;
        }

        self.focused = Some(id.to_string());
        let width = self.text_panel_width(id, usize::MAX);
        if let Some(panel) = self.text_panels.get_mut(id) {
            panel.restore_focused_region(width);
        }
        true
    }

    pub fn select_row_by_id(&mut self, id: &str, row_id: &str, height: usize) -> bool {
        self.panels
            .get_mut(id)
            .is_some_and(|panel| panel.select_row_by_id(row_id, height))
    }

    pub fn focus_editor(&mut self) {
        if let Some(id) = self.focused.as_deref() {
            if let Some(panel) = self.text_panels.get_mut(id) {
                panel.remember_focused_region();
                if let Some(composer) = panel.composer.as_mut() {
                    composer.focused = false;
                    composer.prompt.cancel_history_search();
                }
                panel.blur_scrollback();
            }
        }
        self.focused = None;
    }

    pub fn focused_panel_id(&self) -> Option<&str> {
        self.focused.as_deref()
    }

    pub fn focused_text_input_active(&self) -> bool {
        self.focused
            .as_deref()
            .and_then(|id| self.text_panels.get(id))
            .and_then(|panel| panel.composer.as_ref())
            .is_some_and(|composer| composer.focused && composer.enabled)
    }

    /// The navigation keymap eligible to open the editor command line.
    /// Local text entry and incomplete Vim commands retain their next key.
    pub(crate) fn command_mode_keymap(&self) -> Option<crate::editor::Mode> {
        use crate::editor::Mode;

        let id = self.focused.as_deref()?;
        let Some(panel) = self.text_panels.get(id) else {
            return (!self.focused_row_search_active()).then_some(Mode::Normal);
        };
        if panel.scrollback.focused {
            if panel.search.active.is_some()
                || panel.scrollback.pending_find.is_some()
                || panel.scrollback.pending_jump.is_some()
            {
                return None;
            }
        } else if let Some(composer) = panel
            .composer
            .as_ref()
            .filter(|composer| composer.focused && composer.enabled)
        {
            if composer.prompt.has_pending_input() {
                return None;
            }
        }
        let mode = self
            .focused_text_panel_cursor_mode()
            .unwrap_or(Mode::Normal);
        matches!(
            mode,
            Mode::Normal | Mode::Visual | Mode::VisualLine | Mode::VisualBlock
        )
        .then_some(mode)
    }

    pub fn focused_text_panel_has_composer(&self) -> bool {
        self.focused
            .as_deref()
            .and_then(|id| self.text_panels.get(id))
            .is_some_and(|panel| panel.composer.is_some())
    }

    pub(crate) fn shortcut_context(&self) -> String {
        let Some(panel) = self
            .focused
            .as_ref()
            .and_then(|id| self.text_panels.get(id))
        else {
            return "Panel".to_owned();
        };
        if panel.temporary_context.is_some() {
            return format!(
                "{} · Session picker",
                panel.config.title.as_deref().unwrap_or("Text panel")
            );
        }
        let mode = panel
            .composer
            .as_ref()
            .map(|composer| text_panel_composer_hints(composer, &panel.scrollback).0)
            .unwrap_or("READ");
        format!(
            "{} · {mode}",
            panel.config.title.as_deref().unwrap_or("Text panel")
        )
    }

    pub(crate) fn surface_actions(&self) -> Vec<UiAction> {
        let Some(panel) = self
            .focused
            .as_ref()
            .and_then(|id| self.text_panels.get(id))
        else {
            return Vec::new();
        };
        let mut actions = if panel.temporary_context.is_some() {
            let mut actions = vec![
                UiAction::new("picker_select", "Enter", "resume session")
                    .with_group("Session picker"),
                UiAction::new("picker_previous", "Up", "previous session")
                    .with_group("Session picker"),
                UiAction::new("picker_next", "Down", "next session").with_group("Session picker"),
                UiAction::new("picker_cancel", "Esc", "back to conversation")
                    .with_group("Session picker"),
                UiAction::new("session_preview", "Alt-p", "toggle session preview")
                    .with_group("Session picker"),
            ];
            if let Some(context) = panel
                .temporary_context
                .as_ref()
                .filter(|context| !context.tabs.is_empty() && !context.tab_shortcut.is_empty())
            {
                actions.push(
                    UiAction::new("context_tab", &context.tab_shortcut, "next scope")
                        .with_group("Session picker"),
                );
            }
            actions
        } else {
            let mut actions = if panel.action_shortcuts_active() {
                panel
                    .config
                    .header_actions
                    .iter()
                    .map(TextPanelHeaderAction::ui_action)
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            actions.extend(
                panel
                    .composer
                    .as_ref()
                    .map(|composer| {
                        text_panel_actions(composer, &panel.scrollback, TextPanelOverflow::None).1
                    })
                    .unwrap_or_else(|| crate::ui::surface_reference_actions("SCROLLBACK NORMAL")),
            );
            actions
        };
        if let Some(detail) = &panel.header_detail {
            if let (Some(action), Some(shortcut)) = (&detail.action, &detail.shortcut) {
                actions.retain(|action| action.key != *shortcut);
                let (label, description) = if action == "session_scope" {
                    ("scope", "Change which sessions are shown")
                } else {
                    ("model", "Change the agent model")
                };
                actions.push(
                    UiAction::new(action, shortcut, label)
                        .with_group("Conversation")
                        .with_description(description),
                );
            }
        }
        actions
    }

    pub(crate) fn activate_surface_action(&self, id: &str) -> Option<PanelEvent> {
        let panel = self.text_panels.get(self.focused.as_deref()?)?;
        let action = panel
            .config
            .header_actions
            .iter()
            .find(|action| action.id == id && action.enabled)?;
        Some(PanelEvent {
            panel_id: panel.id.clone(),
            action: action.id.clone(),
            selected_index: panel.scroll,
            row: None,
            text: None,
        })
    }

    pub(crate) fn owns_surface_action(&self, id: &str) -> bool {
        self.focused
            .as_deref()
            .and_then(|panel_id| self.text_panels.get(panel_id))
            .is_some_and(|panel| {
                panel
                    .config
                    .header_actions
                    .iter()
                    .any(|action| action.id == id)
            })
    }

    pub(crate) fn action_shortcut_event(&self, event: &Event) -> Option<PanelEvent> {
        let panel = self.text_panels.get(self.focused.as_deref()?)?;
        if !panel.action_shortcuts_active() {
            return None;
        }
        let action = panel
            .config
            .header_actions
            .iter()
            .find(|action| action.enabled && action.event().as_ref() == Some(event))?;
        Some(PanelEvent {
            panel_id: panel.id.clone(),
            action: action.id.clone(),
            selected_index: panel.scroll,
            row: None,
            text: None,
        })
    }

    pub fn focused_row_panel(&self) -> bool {
        self.focused
            .as_deref()
            .is_some_and(|id| self.panels.contains_key(id))
    }

    pub fn has_focused_panel(&self) -> bool {
        self.focused.is_some()
    }

    pub fn focusable_ids_for_side(&self, side: PanelSide) -> Vec<String> {
        let mut ids = self
            .z_order
            .iter()
            .filter(|id| {
                self.panel_config(id)
                    .is_some_and(|config| config.side == side)
            })
            .cloned()
            .collect::<Vec<_>>();
        if matches!(side, PanelSide::Right | PanelSide::Bottom) {
            ids.reverse();
        }
        ids
    }

    pub fn selected_index(&self, id: &str) -> Option<usize> {
        self.panels.get(id).map(|panel| panel.selected)
    }

    /// Columns docked panes take from the left of the editor at this terminal width.
    pub fn reserved_left_width(&self, terminal_width: usize) -> usize {
        self.reserved_side_width(PanelSide::Left, terminal_width)
    }

    /// Columns docked panes take from the right of the editor at this terminal width.
    pub fn reserved_right_width(&self, terminal_width: usize) -> usize {
        self.reserved_side_width(PanelSide::Right, terminal_width)
    }

    fn reserved_side_width(&self, side: PanelSide, terminal_width: usize) -> usize {
        self.side_panel_widths(terminal_width)
            .into_iter()
            .filter(|(_, panel_side, _)| *panel_side == side)
            .filter_map(|(_, _, width)| width)
            .map(|width| width.saturating_add(1))
            .sum()
    }

    /// Docked widths of the visible left and right panels, in z-order. Fixed
    /// panels keep their width; conversation panes then dock in order while
    /// each still gets [`MIN_READABLE_PANE_WIDTH`] columns and the editor keeps
    /// [`MIN_COMFORTABLE_EDITOR_WIDTH`]. A pane that does not fit is compact
    /// (`None`): it reserves nothing and covers the editor while focused. The
    /// editor's reservation, the placements, and text layout all use this one
    /// allocation, so they always agree.
    fn side_panel_widths(&self, terminal_width: usize) -> Vec<(String, PanelSide, Option<usize>)> {
        let side_panels = self
            .z_order
            .iter()
            .filter_map(|id| Some((id, self.panel_config(id)?)))
            .filter(|(_, config)| matches!(config.side, PanelSide::Left | PanelSide::Right))
            .collect::<Vec<_>>();
        let fixed: usize = side_panels
            .iter()
            .filter(|(_, config)| config.composer.is_none())
            .map(|(_, config)| effective_panel_width(config, false, terminal_width) + 1)
            .sum();
        let mut room = terminal_width.saturating_sub(fixed);
        side_panels
            .into_iter()
            .map(|(id, config)| {
                let width = if config.composer.is_none() {
                    Some(effective_panel_width(config, false, terminal_width))
                } else {
                    let preferred =
                        effective_panel_width(config, self.is_user_sized(id), terminal_width);
                    let width =
                        preferred.min(room.saturating_sub(MIN_COMFORTABLE_EDITOR_WIDTH + 1));
                    (width >= MIN_READABLE_PANE_WIDTH).then(|| {
                        room = room.saturating_sub(width + 1);
                        width
                    })
                };
                (id.clone(), config.side, width)
            })
            .collect()
    }

    /// Whether a visible conversation pane is too cramped to dock beside the
    /// editor and the panes docked before it.
    pub(crate) fn is_compact_panel(&self, id: &str, terminal_width: usize) -> bool {
        self.side_panel_widths(terminal_width)
            .into_iter()
            .any(|(panel, _, width)| panel == id && width.is_none())
    }

    /// Layout width of a text panel at this terminal width: the zoomed size, its
    /// docked width, or the full terminal for a compact pane (the width it has
    /// whenever it is on screen).
    pub(crate) fn text_panel_width(&self, id: &str, terminal_width: usize) -> usize {
        self.text_panel_width_in(&self.presentation, id, terminal_width)
    }

    fn text_panel_width_in(
        &self,
        presentation: &PanelPresentation,
        id: &str,
        terminal_width: usize,
    ) -> usize {
        if let PanelPresentation::Zoomed { id: target, size } = presentation {
            if target == id {
                return size.0;
            }
        }
        if let Some((_, _, width)) = self
            .side_panel_widths(terminal_width)
            .into_iter()
            .find(|(panel, _, _)| panel == id)
        {
            return width.unwrap_or(terminal_width);
        }
        self.panel_config(id).map_or(terminal_width, |config| {
            effective_panel_width(config, self.is_user_sized(id), terminal_width)
        })
    }

    /// Whether a panel offers a composer, which makes its docked width responsive.
    pub(crate) fn panel_has_composer(&self, id: &str) -> bool {
        self.panel_config(id)
            .is_some_and(|config| config.composer.is_some())
    }

    /// Returns the rows reserved by visible panes above the editor.
    pub fn reserved_top_height(&self) -> usize {
        self.z_order
            .iter()
            .filter_map(|id| self.panel_config(id))
            .filter(|config| config.side == PanelSide::Top)
            .map(|config| config.width.saturating_add(1))
            .sum()
    }

    /// Returns the rows reserved by visible panes below the editor.
    pub fn reserved_bottom_height(&self) -> usize {
        self.z_order
            .iter()
            .filter_map(|id| self.panel_config(id))
            .filter(|config| config.side == PanelSide::Bottom)
            .map(|config| config.width.saturating_add(1))
            .sum()
    }

    pub(crate) fn header_shortcut_event(&mut self, key: &str) -> Option<PanelEvent> {
        let panel = self.text_panels.get_mut(self.focused.as_deref()?)?;
        // Tab and Shift-Tab move between tabs, as does the declared shortcut.
        let step = panel
            .temporary_context
            .as_ref()
            .filter(|context| !context.tabs.is_empty())
            .and_then(|context| {
                if key == "Tab" || context.tab_shortcut.eq_ignore_ascii_case(key) {
                    Some(1)
                } else if matches!(key, "BackTab" | "Shift-BackTab" | "Shift-Tab") {
                    Some(-1)
                } else {
                    None
                }
            });
        if let Some(step) = step {
            let tab = panel.step_picker_tab(step)?;
            return Some(PanelEvent {
                panel_id: panel.id.clone(),
                action: "context_tab".to_string(),
                selected_index: panel.scroll,
                row: None,
                text: Some(tab),
            });
        }
        if panel.temporary_context.is_some() && key.eq_ignore_ascii_case("Alt-p") {
            return Some(PanelEvent {
                panel_id: panel.id.clone(),
                action: "session_preview".to_string(),
                selected_index: panel.scroll,
                row: None,
                text: panel.selected_picker_action().map(str::to_string),
            });
        }
        let action = panel
            .header_detail
            .as_ref()
            .filter(|detail| detail.shortcut.as_deref() == Some(key))
            .and_then(|detail| detail.action.as_ref())?;
        Some(PanelEvent {
            panel_id: panel.id.clone(),
            action: action.clone(),
            selected_index: panel.scroll,
            row: None,
            text: None,
        })
    }

    pub fn handle_focused_key(
        &mut self,
        action: &str,
        panel_height: usize,
        terminal_width: usize,
        scrolloff: usize,
    ) -> Option<PanelEvent> {
        let focused = self.focused.clone()?;
        let width = self.text_panel_width(&focused, terminal_width);
        if let Some(panel) = self.text_panels.get_mut(&focused) {
            let panel_height = self
                .presentation
                .height(&panel.id, &panel.config, panel_height);
            match action {
                "up" if panel.temporary_context.is_some() => {
                    panel.move_picker_selection(-1, panel_height, width);
                }
                "down" if panel.temporary_context.is_some() => {
                    panel.move_picker_selection(1, panel_height, width);
                }
                "up" => panel.move_scroll(-1, panel_height, width),
                "down" => panel.move_scroll(1, panel_height, width),
                "page_up" => {
                    panel.page_scroll(-1, panel_height, width);
                }
                "page_down" => {
                    panel.page_scroll(1, panel_height, width);
                }
                "top" => panel.scroll_to_top(),
                "bottom" => panel.scroll_to_bottom(panel_height, width),
                "composer_focus" => {
                    panel.blur_scrollback();
                    if let Some(composer) = panel.composer.as_mut() {
                        if composer.enabled {
                            composer.focused = true;
                        }
                    }
                }
                _ => {}
            }
            return Some(PanelEvent {
                panel_id: panel.id.clone(),
                action: action.to_string(),
                selected_index: panel.scroll,
                row: None,
                text: panel.selected_picker_action().map(str::to_string),
            });
        }
        let panel = self.panels.get_mut(&focused)?;
        let panel_height = self
            .presentation
            .height(&panel.id, &panel.config, panel_height);

        match action {
            "up" => panel.move_selection(-1, panel_height),
            "down" => panel.move_selection(1, panel_height),
            "page_up" => panel.page_scroll(-1, panel_height, scrolloff),
            "page_down" => panel.page_scroll(1, panel_height, scrolloff),
            "top" => panel.scroll_to_top(),
            "bottom" => panel.scroll_to_bottom(panel_height),
            _ => {}
        }

        Some(PanelEvent {
            panel_id: panel.id.clone(),
            action: action.to_string(),
            selected_index: panel.selected,
            row: panel.selected_row(),
            text: None,
        })
    }

    pub(crate) fn handle_focused_scrollback_input(
        &mut self,
        event: &Event,
        panel_height: usize,
        terminal_width: usize,
        count: usize,
    ) -> Option<TextPanelScrollbackInput> {
        let focused = self.focused.clone()?;
        let compact = self.is_compact_panel(&focused, terminal_width);
        let width = self.text_panel_width(&focused, terminal_width);
        let panel = self.text_panels.get_mut(&focused)?;
        if !panel.scrollback.focused {
            return None;
        }
        let panel_height = self
            .presentation
            .height(&panel.id, &panel.config, panel_height);
        if panel.handle_transcript_search_input(event, panel_height, width) {
            return Some(TextPanelScrollbackInput::Handled);
        }
        let Event::Key(key) = event else {
            return None;
        };

        if let Some(pending) = panel.scrollback.pending_jump.take() {
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
            {
                match key.code {
                    KeyCode::Char('p') => {
                        panel.jump_to_prompt(pending.direction, pending.count, panel_height, width);
                    }
                    KeyCode::Char('l') => {
                        panel.jump_to_link(pending.direction, pending.count, panel_height, width);
                    }
                    _ => {}
                }
            }
            return Some(TextPanelScrollbackInput::Handled);
        }

        if let Some(pending) = panel.scrollback.pending_find.take() {
            if key.code == KeyCode::Esc {
                return Some(TextPanelScrollbackInput::Handled);
            }
            let KeyCode::Char(target) = key.code else {
                return Some(TextPanelScrollbackInput::Handled);
            };
            let find = ScrollbackFind {
                direction: pending.direction,
                till: pending.till,
                target,
            };
            panel.find_in_scrollback(find, pending.count, panel_height, width);
            panel.scrollback.last_find = Some(find);
            return Some(TextPanelScrollbackInput::Handled);
        }

        if key.code == KeyCode::Esc && panel.scrollback.mode != TextPanelScrollbackMode::Normal {
            panel.scrollback.mode = TextPanelScrollbackMode::Normal;
            panel.scrollback.selection_anchor = None;
            return Some(TextPanelScrollbackInput::Handled);
        }
        if key.code == KeyCode::Esc {
            if panel.search.visible {
                panel.search.visible = false;
                return Some(TextPanelScrollbackInput::Handled);
            }
            let composer_enabled = panel
                .composer
                .as_ref()
                .is_some_and(|composer| composer.enabled);
            // A compact pane covers the editor, so Esc from the transcript steps
            // back out to it instead of cycling to the composer.
            if composer_enabled && !compact {
                panel.blur_scrollback();
                if let Some(composer) = panel.composer.as_mut() {
                    composer.focused = true;
                }
                return Some(TextPanelScrollbackInput::Handled);
            }
            return None;
        }
        if key.modifiers.is_empty() && matches!(key.code, KeyCode::Char('i' | 'a')) {
            let composer_enabled = panel
                .composer
                .as_ref()
                .is_some_and(|composer| composer.enabled);
            if composer_enabled {
                panel.blur_scrollback();
                if let Some(composer) = panel.composer.as_mut() {
                    composer.focused = true;
                    composer.prompt.set_mode(crate::editor::Mode::Normal);
                    let _ = composer.handle_event(event, TextPanelContentMetrics::new(width).width);
                }
                return Some(TextPanelScrollbackInput::Handled);
            }
        }
        if !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            match key.code {
                KeyCode::Char('m') if panel.scrollback.mode == TextPanelScrollbackMode::Normal => {
                    return Some(TextPanelScrollbackInput::OpenTurnActions);
                }
                KeyCode::Char('/' | '?')
                    if panel.scrollback.mode == TextPanelScrollbackMode::Normal
                        && (panel.composer.is_some()
                            || text_panel_header_rows(&panel.config) > 0) =>
                {
                    let direction = if key.code == KeyCode::Char('?') {
                        TextPanelSearchDirection::Backward
                    } else {
                        TextPanelSearchDirection::Forward
                    };
                    panel.begin_transcript_search(direction, panel_height, width);
                    return Some(TextPanelScrollbackInput::Handled);
                }
                KeyCode::Char('n' | 'N')
                    if panel.scrollback.mode == TextPanelScrollbackMode::Normal =>
                {
                    panel.repeat_transcript_search(
                        key.code == KeyCode::Char('N'),
                        count,
                        panel_height,
                        width,
                    );
                    return Some(TextPanelScrollbackInput::Handled);
                }
                KeyCode::Char('[' | ']')
                    if panel.scrollback.mode == TextPanelScrollbackMode::Normal =>
                {
                    panel.scrollback.pending_jump = Some(PendingScrollbackJump {
                        direction: if key.code == KeyCode::Char('[') {
                            ScrollbackJumpDirection::Previous
                        } else {
                            ScrollbackJumpDirection::Next
                        },
                        count,
                    });
                    return Some(TextPanelScrollbackInput::Handled);
                }
                KeyCode::Char('v') => {
                    panel.scrollback.mode =
                        if panel.scrollback.mode == TextPanelScrollbackMode::Visual {
                            TextPanelScrollbackMode::Normal
                        } else {
                            TextPanelScrollbackMode::Visual
                        };
                    panel.scrollback.selection_anchor = (panel.scrollback.mode
                        != TextPanelScrollbackMode::Normal)
                        .then_some(panel.scrollback.cursor);
                    return Some(TextPanelScrollbackInput::Handled);
                }
                KeyCode::Char('V') => {
                    panel.scrollback.mode =
                        if panel.scrollback.mode == TextPanelScrollbackMode::VisualLine {
                            TextPanelScrollbackMode::Normal
                        } else {
                            TextPanelScrollbackMode::VisualLine
                        };
                    panel.scrollback.selection_anchor = (panel.scrollback.mode
                        != TextPanelScrollbackMode::Normal)
                        .then_some(panel.scrollback.cursor);
                    return Some(TextPanelScrollbackInput::Handled);
                }
                KeyCode::Char('y') if panel.scrollback.mode != TextPanelScrollbackMode::Normal => {
                    return panel
                        .yank_scrollback(width)
                        .map(TextPanelScrollbackInput::Yank);
                }
                KeyCode::Enter
                    if key.modifiers.is_empty()
                        && panel.scrollback.mode != TextPanelScrollbackMode::Normal =>
                {
                    let yank = panel.yank_scrollback(width);
                    panel.scrollback.mode = TextPanelScrollbackMode::Normal;
                    panel.scrollback.selection_anchor = None;
                    panel.scrollback.mouse_anchor = None;
                    panel.scrollback.mouse_dragging = false;
                    panel.move_scrollback(TextPanelMotion::Bottom, 1, panel_height, width);
                    panel.scroll_to_bottom(panel_height, width);
                    if panel
                        .composer
                        .as_ref()
                        .is_some_and(|composer| composer.enabled)
                    {
                        panel.blur_scrollback();
                        if let Some(composer) = panel.composer.as_mut() {
                            composer.focused = true;
                        }
                        panel.last_focused_region = TextPanelFocusRegion::Composer;
                    }
                    return Some(yank.map_or(
                        TextPanelScrollbackInput::Handled,
                        TextPanelScrollbackInput::Yank,
                    ));
                }
                KeyCode::Char('f' | 'F' | 't' | 'T') => {
                    panel.scrollback.pending_find = Some(PendingScrollbackFind {
                        direction: if matches!(key.code, KeyCode::Char('F' | 'T')) {
                            ScrollbackFindDirection::Backward
                        } else {
                            ScrollbackFindDirection::Forward
                        },
                        till: matches!(key.code, KeyCode::Char('t' | 'T')),
                        count,
                    });
                    return Some(TextPanelScrollbackInput::Handled);
                }
                KeyCode::Char(';' | ',') => {
                    let Some(mut find) = panel.scrollback.last_find else {
                        return Some(TextPanelScrollbackInput::Handled);
                    };
                    if key.code == KeyCode::Char(',') {
                        find.direction = match find.direction {
                            ScrollbackFindDirection::Forward => ScrollbackFindDirection::Backward,
                            ScrollbackFindDirection::Backward => ScrollbackFindDirection::Forward,
                        };
                    }
                    panel.find_in_scrollback(find, count, panel_height, width);
                    return Some(TextPanelScrollbackInput::Handled);
                }
                _ => {}
            }
        }

        if key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            return None;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let motion = match key.code {
            KeyCode::Left | KeyCode::Char('h') if !control => TextPanelMotion::Left,
            KeyCode::Right | KeyCode::Char('l') if !control => TextPanelMotion::Right,
            KeyCode::Up | KeyCode::Char('k') if !control => TextPanelMotion::Up,
            KeyCode::Down | KeyCode::Char('j') if !control => TextPanelMotion::Down,
            KeyCode::Home | KeyCode::Char('0') if !control => TextPanelMotion::LineStart,
            KeyCode::Char('^') if !control => TextPanelMotion::FirstNonBlank,
            KeyCode::End | KeyCode::Char('$') if !control => TextPanelMotion::LineEnd,
            KeyCode::Char('w' | 'W') if !control => TextPanelMotion::NextWord,
            KeyCode::Char('b' | 'B') if !control => TextPanelMotion::PreviousWord,
            KeyCode::Char('e' | 'E') if !control => TextPanelMotion::WordEnd,
            KeyCode::Char('{') if !control => TextPanelMotion::PreviousParagraph,
            KeyCode::Char('}') if !control => TextPanelMotion::NextParagraph,
            KeyCode::PageUp => TextPanelMotion::PageUp,
            KeyCode::PageDown => TextPanelMotion::PageDown,
            KeyCode::Char('b') if control => TextPanelMotion::PageUp,
            KeyCode::Char('f') if control => TextPanelMotion::PageDown,
            KeyCode::Char('u') if control => TextPanelMotion::HalfPageUp,
            KeyCode::Char('d') if control => TextPanelMotion::HalfPageDown,
            KeyCode::Char('H') if !control => TextPanelMotion::ViewportTop,
            KeyCode::Char('M') if !control => TextPanelMotion::ViewportMiddle,
            KeyCode::Char('L') if !control => TextPanelMotion::ViewportBottom,
            KeyCode::Char('g') if !control => TextPanelMotion::Top,
            KeyCode::Char('G') if !control => TextPanelMotion::Bottom,
            _ => return None,
        };
        panel.move_scrollback(motion, count, panel_height, width);
        Some(TextPanelScrollbackInput::Handled)
    }

    pub fn handle_mouse_scroll(
        &mut self,
        id: &str,
        delta: isize,
        panel_height: usize,
        terminal_width: usize,
        scrolloff: usize,
    ) -> Option<PanelEvent> {
        let action = if delta < 0 { "up" } else { "down" };
        let width = self.text_panel_width(id, terminal_width);
        if let Some(panel) = self.text_panels.get_mut(id) {
            let panel_height = self
                .presentation
                .height(&panel.id, &panel.config, panel_height);
            panel.move_scroll(delta, panel_height, width);
            if panel.scrollback.focused {
                let layout = panel.layout(width);
                if let Some((row, _, column)) = layout.position(panel.scrollback.cursor) {
                    let visible = panel.visible_rows(panel_height, width);
                    let first = panel.scroll;
                    let last = first
                        .saturating_add(visible.saturating_sub(1))
                        .min(layout.lines.len().saturating_sub(1));
                    let target_row = row.clamp(first, last);
                    if target_row != row {
                        if let Some(offset) = layout.nearest_offset_on_row(target_row, column) {
                            panel.scrollback.cursor = offset;
                        }
                    }
                }
            }
            return Some(PanelEvent {
                panel_id: panel.id.clone(),
                action: action.to_string(),
                selected_index: panel.scroll,
                row: None,
                text: None,
            });
        }

        let panel = self.panels.get_mut(id)?;
        panel.scroll_view(delta, panel_height, scrolloff);
        Some(PanelEvent {
            panel_id: panel.id.clone(),
            action: action.to_string(),
            selected_index: panel.selected,
            row: panel.selected_row(),
            text: None,
        })
    }

    pub(crate) fn toggle_focused_text_region(&mut self, terminal_width: usize) -> bool {
        let Some(focused) = self.focused.clone() else {
            return false;
        };
        let Some(panel) = self.text_panels.get(&focused) else {
            return false;
        };
        if panel.temporary_context.is_some() {
            return false;
        }
        if panel
            .composer
            .as_ref()
            .is_some_and(|composer| composer.enabled && !composer.focused)
        {
            self.focus_composer(&focused, /*insert*/ false)
        } else {
            self.focus_focused_text_scrollback(terminal_width)
        }
    }

    pub(crate) fn focus_focused_text_scrollback(&mut self, terminal_width: usize) -> bool {
        let Some(focused) = self.focused.clone() else {
            return false;
        };
        let width = self.text_panel_width(&focused, terminal_width);
        let Some(panel) = self.text_panels.get_mut(&focused) else {
            return false;
        };
        panel.focus_scrollback(width);
        true
    }

    pub(crate) fn focused_text_link_target(
        &self,
        terminal_width: usize,
    ) -> Option<TextPanelLinkTarget> {
        let panel = self.text_panels.get(self.focused.as_deref()?)?;
        let width = self.text_panel_width(&panel.id, terminal_width);
        if panel.scrollback.focused {
            panel.layout(width).link_at(panel.scrollback.cursor)
        } else {
            panel.selected_link_target(width)
        }
    }

    pub(crate) fn text_link_at_position(
        &mut self,
        x: usize,
        y: usize,
        terminal_width: usize,
        terminal_height: usize,
    ) -> Option<TextPanelLinkTarget> {
        self.text_link_at_position_impl(x, y, terminal_width, terminal_height, false)
            .map(|(_, link)| link.target)
    }

    pub(crate) fn double_clicked_text_link_at_position(
        &mut self,
        x: usize,
        y: usize,
        terminal_width: usize,
        terminal_height: usize,
    ) -> Option<TextPanelLinkTarget> {
        let clicked = self.text_link_at_position_impl(x, y, terminal_width, terminal_height, false);
        let Some((panel_id, link)) = clicked else {
            self.last_text_link_click = None;
            return None;
        };

        let now = Instant::now();
        let is_double_click = self.last_text_link_click.take().is_some_and(|click| {
            click.panel_id == panel_id
                && click.link_id == link.id
                && now.duration_since(click.at) <= ROW_DOUBLE_CLICK_INTERVAL
        });
        if is_double_click {
            Some(link.target)
        } else {
            self.last_text_link_click = Some(TextLinkClick {
                panel_id,
                link_id: link.id,
                at: now,
            });
            None
        }
    }

    pub(crate) fn text_action_at_position(
        &mut self,
        x: usize,
        y: usize,
        terminal_width: usize,
        terminal_height: usize,
    ) -> Option<TextPanelLinkTarget> {
        self.text_link_at_position_impl(x, y, terminal_width, terminal_height, true)
            .map(|(_, link)| link.target)
    }

    fn text_link_at_position_impl(
        &mut self,
        x: usize,
        y: usize,
        terminal_width: usize,
        terminal_height: usize,
        action_only: bool,
    ) -> Option<(String, TextPanelLink)> {
        let placement = self.panel_at_position(x, y, terminal_width, terminal_height)?;
        let panel = self.text_panels.get_mut(&placement.id)?;
        let title_rows = text_panel_header_rows(&panel.config)
            .saturating_add(panel.picker_header_rows(placement.height));
        let metrics = TextPanelContentMetrics::new(placement.width);
        let content_height = placement
            .height
            .saturating_sub(panel.composer_height(placement.height, placement.width))
            .saturating_sub(panel.status_height());
        let screen_row = y.saturating_sub(placement.y);
        if screen_row < title_rows
            || screen_row >= content_height
            || !metrics.contains_x(placement.x, x)
        {
            return None;
        }
        let preview = panel.picker_preview_geometry(placement.height, placement.width);
        if preview.is_some_and(|geometry| {
            metrics.column(placement.x, x) >= geometry.list_width
                || screen_row.saturating_sub(title_rows) >= geometry.list_rows
        }) {
            return None;
        }

        let layout = panel.layout(placement.width);
        let lines = &layout.rendered;
        let visible_rows = preview.map_or_else(
            || content_height.saturating_sub(title_rows),
            |geometry| geometry.list_rows,
        );
        let max_scroll = lines.len().saturating_sub(visible_rows);
        let scroll = if panel.follow_tail {
            max_scroll
        } else {
            panel.scroll.min(max_scroll)
        };
        let line = lines.get(scroll + screen_row - title_rows)?;
        let picker_inset =
            if panel.temporary_context.is_some() && picker_action_link(line).is_some() {
                picker_item_text_inset(metrics.width)
            } else {
                0
            };
        let column = metrics.column(placement.x, x).checked_sub(picker_inset)?;
        let mut used = 0usize;
        for span in &line.spans {
            let end = used.saturating_add(display_width(&span.text));
            if column >= used && column < end {
                let link = span.link.as_ref()?;
                if action_only && !matches!(link.target, TextPanelLinkTarget::PanelAction { .. }) {
                    return None;
                }
                self.focused = Some(placement.id.clone());
                panel.selected_link = Some(link.id);
                if let TextPanelLinkTarget::PanelAction { block_id, .. } = &link.target {
                    if let Some(context) = panel.temporary_context.as_mut() {
                        context.selected_action = Some(block_id.clone());
                        context.selected_index = panel
                            .blocks
                            .iter()
                            .filter(|block| block.kind == TextPanelBlockKind::Action)
                            .position(|block| block.id == *block_id)
                            .unwrap_or_default();
                    }
                }
                if let Some(composer) = panel.composer.as_mut() {
                    composer.focused = false;
                }
                return Some((placement.id, link.clone()));
            }
            used = end;
        }
        None
    }

    pub fn focused_text_for_copy(&self, all: bool, terminal_width: usize) -> Option<String> {
        let panel = self.text_panels.get(self.focused.as_deref()?)?;
        if all {
            Some(panel.copy_all())
        } else {
            let width = self.text_panel_width(&panel.id, terminal_width);
            match panel.selected_prompt(&panel.layout(width)) {
                Some(index) => panel.turn_text(&panel.blocks[index].id, TextPanelTurnPart::Answer),
                None => panel.copy_last_agent(),
            }
        }
    }

    pub(crate) fn selected_text_turn(&self, terminal_width: usize) -> Option<TextPanelTurnTarget> {
        let panel = self.text_panels.get(self.focused.as_deref()?)?;
        let width = self.text_panel_width(&panel.id, terminal_width);
        let index = panel.selected_prompt(&panel.layout(width))?;
        let prompt = &panel.blocks[index];
        Some(TextPanelTurnTarget {
            panel_id: panel.id.clone(),
            prompt_id: prompt.id.clone(),
            number: panel.blocks[..=index]
                .iter()
                .filter(|block| block.kind == TextPanelBlockKind::User)
                .count(),
            preview: truncate_display_width(&first_prompt_line(&prompt.text), 52),
            has_answer: panel
                .turn_text(&prompt.id, TextPanelTurnPart::Answer)
                .is_some(),
            can_reuse: panel
                .composer
                .as_ref()
                .is_some_and(|composer| composer.enabled),
        })
    }

    pub(crate) fn text_turn_for_copy(
        &self,
        panel_id: &str,
        prompt_id: &str,
        part: TextPanelTurnPart,
    ) -> Option<String> {
        self.text_panels.get(panel_id)?.turn_text(prompt_id, part)
    }

    pub(crate) fn reuse_text_panel_prompt(
        &mut self,
        panel_id: &str,
        prompt_id: &str,
        expected_draft: Option<TextPanelDraftRevision>,
    ) -> Result<TextPanelReuseOutcome, &'static str> {
        let text = self
            .text_panels
            .get(panel_id)
            .and_then(|panel| panel.turn_text(prompt_id, TextPanelTurnPart::Prompt))
            .ok_or("selected prompt is no longer available")?;
        self.load_text_panel_draft(panel_id, &text, expected_draft)
    }

    /// Loads reviewable text without sending it or overwriting an unapproved draft.
    pub(crate) fn load_text_panel_draft(
        &mut self,
        panel_id: &str,
        text: &str,
        expected_draft: Option<TextPanelDraftRevision>,
    ) -> Result<TextPanelReuseOutcome, &'static str> {
        if !self.z_order.iter().any(|id| id == panel_id) {
            return Err("conversation pane is no longer visible");
        }
        let panel = self
            .text_panels
            .get_mut(panel_id)
            .ok_or("conversation is no longer available")?;
        let text = normalize_prompt_newlines(text);
        if text.len() > MAX_COMPOSER_BYTES {
            return Err("selected prompt exceeds 128 KiB");
        }
        let composer = panel
            .composer
            .as_mut()
            .filter(|composer| composer.enabled)
            .ok_or("conversation composer is unavailable")?;
        let revision = TextPanelDraftRevision {
            buffer_id: composer.prompt.buffer().id(),
            revision: composer.prompt.buffer().revision(),
        };
        if expected_draft.is_some_and(|expected| expected != revision) {
            return Err("composer draft changed; choose reuse again");
        }
        let current = composer.prompt.text();
        if current != text && !current.is_empty() && expected_draft.is_none() {
            return Ok(TextPanelReuseOutcome::Confirm(revision));
        }
        if current != text && !composer.prompt.replace_draft(&text) {
            return Err("could not load the selected prompt");
        }
        composer.prompt.set_mode(crate::editor::Mode::Insert);
        composer.validation = None;
        composer.focused = true;
        panel.blur_scrollback();
        panel.last_focused_region = TextPanelFocusRegion::Composer;
        self.focused = Some(panel_id.to_string());
        Ok(TextPanelReuseOutcome::Loaded)
    }

    /// Focuses a composer for typing, always in Insert mode.
    pub fn focus_text_panel_composer(&mut self, id: &str) -> bool {
        self.focus_composer(id, /*insert*/ true)
    }

    /// Moves focus to a composer. `insert` is for explicit "let me type" requests
    /// such as Space A; switching regions keeps the composer's own mode.
    fn focus_composer(&mut self, id: &str, insert: bool) -> bool {
        if !self.z_order.iter().any(|panel_id| panel_id == id) {
            return false;
        }
        let Some(panel) = self.text_panels.get_mut(id) else {
            return false;
        };
        let Some(composer) = panel.composer.as_mut() else {
            return false;
        };
        if !composer.enabled {
            return false;
        }
        if insert {
            composer.prompt.set_mode(crate::editor::Mode::Insert);
        }
        composer.focused = true;
        panel.blur_scrollback();
        panel.last_focused_region = TextPanelFocusRegion::Composer;
        self.focused = Some(id.to_string());
        true
    }

    pub fn set_text_panel_composer_state(
        &mut self,
        id: &str,
        enabled: bool,
        status: Option<String>,
    ) -> bool {
        let Some(composer) = self
            .text_panels
            .get_mut(id)
            .and_then(|panel| panel.composer.as_mut())
        else {
            return false;
        };
        composer.enabled = enabled;
        composer.status = status;
        if !enabled {
            composer.focused = false;
            composer.prompt.cancel_history_search();
        }
        true
    }

    /// Temporarily replaces a text panel's contents and composer while retaining its UI state.
    pub fn enter_text_panel_context(&mut self, id: &str, placeholder: String) -> bool {
        self.enter_text_panel_context_with_options(
            id,
            placeholder,
            TextPanelContextOptions::default(),
        )
    }

    /// Restores the workspace-specific picker selection while preserving the conversation.
    pub fn enter_text_panel_context_with_options(
        &mut self,
        id: &str,
        placeholder: String,
        options: TextPanelContextOptions,
    ) -> bool {
        let Some(panel) = self.text_panels.get_mut(id) else {
            return false;
        };
        if !panel.enter_temporary_context(placeholder, options) {
            return false;
        }
        panel.blur_scrollback();
        self.focused = Some(id.to_string());
        true
    }

    /// Restores the panel state retained by `enter_text_panel_context`.
    pub fn exit_text_panel_context(&mut self, id: &str) -> bool {
        let Some(panel) = self.text_panels.get_mut(id) else {
            return false;
        };
        panel.exit_temporary_context()
    }

    /// Changes an embedded picker's read-only detail without replacing conversation blocks.
    pub fn set_text_panel_preview(
        &mut self,
        id: &str,
        preview: Option<TextPanelPreview>,
        panel_height: usize,
        terminal_width: usize,
    ) -> bool {
        let width = self.text_panel_width(id, terminal_width);
        let Some(panel) = self.text_panels.get_mut(id) else {
            return false;
        };
        let height = self
            .presentation
            .height(&panel.id, &panel.config, panel_height);
        let Some(context) = panel.temporary_context.as_mut() else {
            return false;
        };
        if context.preview == preview {
            return false;
        }
        context.preview = preview;
        panel.invalidate_layout();
        panel.sync_picker_selection(height, width);
        true
    }

    pub fn set_text_panel_composer_history(&mut self, id: &str, history: Vec<String>) -> bool {
        let Some(composer) = self
            .text_panels
            .get_mut(id)
            .and_then(|panel| panel.composer.as_mut())
        else {
            return false;
        };
        composer.prompt.set_history(history);
        true
    }

    pub fn set_text_panel_status(&mut self, id: &str, status: Option<TextPanelStatus>) -> bool {
        let Some(panel) = self.text_panels.get_mut(id) else {
            return false;
        };
        panel.set_status(status);
        true
    }

    /// Advance spinner/elapsed state for visible busy panels.
    ///
    /// Returns true when the animation moved and the screen needs a repaint.
    pub fn poll_animation(&mut self) -> bool {
        let mut state = self
            .z_order
            .iter()
            .filter_map(|id| {
                let panel = self.text_panels.get(id)?;
                if !panel.status.as_ref()?.busy {
                    return None;
                }
                let elapsed_ms = panel.busy_since?.elapsed().as_millis() as u64;
                let frame = (elapsed_ms / TEXT_PANEL_SPINNER_INTERVAL_MS)
                    % TEXT_PANEL_SPINNER_FRAMES.len() as u64;
                Some((id.clone(), frame as u8, elapsed_ms / 1000))
            })
            .collect::<Vec<_>>();
        state.sort();
        if state == self.animation_state {
            false
        } else {
            self.animation_state = state;
            true
        }
    }

    pub fn clear_text_panel_composer(&mut self, id: &str) -> bool {
        let Some(composer) = self
            .text_panels
            .get_mut(id)
            .and_then(|panel| panel.composer.as_mut())
        else {
            return false;
        };
        composer.prompt.clear();
        composer.validation = None;
        true
    }

    pub fn handle_focused_text_input(
        &mut self,
        event: &Event,
        terminal_width: usize,
    ) -> Option<PanelEvent> {
        let focused = self.focused.clone()?;
        let panel_width = self.text_panel_width(&focused, terminal_width);
        let panel = self.text_panels.get_mut(&focused)?;
        let composer = panel.composer.as_mut()?;
        if !composer.focused || !composer.enabled {
            return None;
        }
        let temporary_context = composer.temporary_context.is_some();
        if temporary_context {
            if matches!(
                event,
                Event::Key(key)
                    if key.code == KeyCode::Esc
                        || (matches!(key.code, KeyCode::Char('c' | 'C'))
                            && key.modifiers.contains(KeyModifiers::CONTROL))
            ) {
                return Some(PanelEvent {
                    panel_id: panel.id.clone(),
                    action: "filter_cancel".to_string(),
                    selected_index: panel.scroll,
                    row: None,
                    text: None,
                });
            }
            if matches!(
                event,
                Event::Key(key)
                    if matches!(key.code, KeyCode::Up | KeyCode::Down)
                        || (key.modifiers.contains(KeyModifiers::CONTROL)
                            && matches!(key.code, KeyCode::Char('j' | 'k')))
            ) {
                return None;
            }
            if matches!(event, Event::Key(key) if key.code == KeyCode::Enter && key.modifiers.is_empty())
            {
                let selected = panel
                    .temporary_context
                    .as_ref()
                    .and_then(|context| context.selected_action.clone());
                return Some(PanelEvent {
                    panel_id: panel.id.clone(),
                    action: if selected.is_some() {
                        "activate_block"
                    } else {
                        "filter_submit"
                    }
                    .to_string(),
                    selected_index: panel.scroll,
                    row: None,
                    text: Some(selected.unwrap_or_else(|| composer.prompt.text())),
                });
            }
        }
        if matches!(
            event,
            Event::Key(key)
                if matches!(key.code, KeyCode::Char('c' | 'C'))
                    && key.modifiers.contains(KeyModifiers::CONTROL)
        ) {
            // Ctrl-C is a pane-level interrupt. Let the editor route it without
            // changing the focused composer's mode, draft, or cursor.
            return None;
        }
        let delegates_to_panel_navigation = matches!(
            event,
            Event::Key(key)
                if (composer.prompt.mode() == crate::editor::Mode::Normal
                    && key.modifiers.is_empty()
                    && matches!(key.code, KeyCode::Char('j' | 'k') | KeyCode::Up | KeyCode::Down))
                    || matches!(key.code, KeyCode::Tab | KeyCode::BackTab)
                    || (key.modifiers.contains(KeyModifiers::CONTROL)
                        && (matches!(key.code, KeyCode::Char('h' | 'k' | 'w'))
                            || (matches!(key.code, KeyCode::Char('g' | 'G'))
                                && !composer.prompt.history_search_active())))
        );
        if delegates_to_panel_navigation {
            // Let the panel-navigation layer scroll the conversation without
            // moving or editing the focused prompt.
            return None;
        }

        let previous_bytes = composer.prompt.text().len();
        let inserted_bytes = match event {
            Event::Paste(text) => normalize_prompt_newlines(text).len(),
            Event::Key(key) => match key.code {
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    character.len_utf8()
                }
                _ => 0,
            },
            _ => 0,
        };
        let (action, text) =
            match composer.handle_event(event, TextPanelContentMetrics::new(panel_width).width) {
                PromptInput::Changed => {
                    composer.validation = None;
                    let action = if temporary_context {
                        "filter_input"
                    } else {
                        "composer_input"
                    };
                    (action, temporary_context.then(|| composer.prompt.text()))
                }
                PromptInput::Submit => match composer.take_submission() {
                    Some(text) => {
                        panel.resume_tail_following();
                        ("submit", Some(text))
                    }
                    None => ("composer_input", None),
                },
                PromptInput::Cancel => {
                    if temporary_context {
                        return Some(PanelEvent {
                            panel_id: panel.id.clone(),
                            action: "filter_cancel".to_string(),
                            selected_index: panel.scroll,
                            row: None,
                            text: None,
                        });
                    }
                    composer.focused = false;
                    panel.scrollback.focused = true;
                    panel.scrollback.mode = TextPanelScrollbackMode::Normal;
                    panel.scrollback.selection_anchor = None;
                    let layout = panel.layout(panel_width);
                    panel.scrollback.cursor = layout.clamp(panel.scrollback.cursor);
                    ("composer_blur", None)
                }
                PromptInput::Unhandled
                    if inserted_bytes > MAX_COMPOSER_BYTES.saturating_sub(previous_bytes) =>
                {
                    composer.validation = Some("Prompt exceeds 128 KiB");
                    let action = if temporary_context {
                        "filter_input"
                    } else {
                        "composer_input"
                    };
                    (action, temporary_context.then(|| composer.prompt.text()))
                }
                PromptInput::Unhandled => return None,
            };

        Some(PanelEvent {
            panel_id: panel.id.clone(),
            action: action.to_string(),
            selected_index: panel.scroll,
            row: None,
            text,
        })
    }

    /// Returns the prompt-local editor mode while the docked composer owns focus.
    pub(crate) fn focused_text_panel_cursor_mode(&self) -> Option<crate::editor::Mode> {
        if self.focused_row_search_active() {
            return Some(crate::editor::Mode::Search);
        }
        let panel = self.text_panels.get(self.focused.as_deref()?)?;
        if panel.scrollback.focused {
            if panel.search.active.is_some() {
                return Some(crate::editor::Mode::Search);
            }
            return Some(match panel.scrollback.mode {
                TextPanelScrollbackMode::Normal => crate::editor::Mode::Normal,
                TextPanelScrollbackMode::Visual => crate::editor::Mode::Visual,
                TextPanelScrollbackMode::VisualLine => crate::editor::Mode::VisualLine,
            });
        }
        let composer = panel.composer.as_ref()?;
        (composer.focused && composer.enabled).then(|| composer.prompt.mode())
    }

    pub fn focused_text_panel_cursor_position(
        &self,
        terminal_width: usize,
        terminal_height: usize,
    ) -> Option<(usize, usize)> {
        let id = self.focused.as_deref()?;
        let placement = self
            .panel_placements(terminal_width, terminal_height)
            .into_iter()
            .find(|placement| placement.id == id)?;
        if let Some(panel) = self.panels.get(id) {
            let search = panel.search.as_ref().filter(|search| search.active)?;
            let text = search.input.text();
            let cursor_byte = grapheme_to_byte(&text, search.input.cursor());
            let before = truncate_display_width_from_end(
                &text[..cursor_byte],
                placement.width.saturating_sub(2),
            );
            return Some((
                placement.x.saturating_add(1 + display_width(&before)),
                placement
                    .y
                    .saturating_add(placement.height.saturating_sub(1)),
            ));
        }
        let panel = self.text_panels.get(id)?;
        if panel.scrollback.focused {
            let title_rows = text_panel_header_rows(&panel.config);
            let metrics = TextPanelContentMetrics::new(placement.width);
            let visible_rows = panel.visible_rows(placement.height, placement.width);
            let layout = panel.layout(placement.width);
            if panel.search.active.is_some() {
                let query = panel.search.query()?;
                let matches = panel.search_matches(&layout, &query.text);
                let bar = TextPanelSearchBar::new(
                    &panel.search,
                    &matches,
                    panel.scrollback.cursor,
                    metrics.width,
                )?;
                return Some((
                    metrics.x(placement.x) + bar.cursor_column?,
                    placement.y + panel.search_bar_row(placement.height)?,
                ));
            }
            let max_scroll = layout.lines.len().saturating_sub(visible_rows);
            let scroll = panel.viewport.visible_offset(max_scroll);
            let (row, _, column) = layout
                .position(panel.scrollback.cursor)
                .unwrap_or((scroll, 0, 0));
            if row < scroll || row >= scroll.saturating_add(visible_rows) {
                return None;
            }
            return Some((
                metrics.x(placement.x).saturating_add(column),
                placement
                    .y
                    .saturating_add(title_rows)
                    .saturating_add(row.saturating_sub(scroll)),
            ));
        }
        let composer = panel.composer.as_ref()?;
        if !composer.focused || !composer.enabled {
            return None;
        }
        let metrics = TextPanelContentMetrics::new(placement.width);
        if panel.temporary_context.is_some() {
            let text = composer.prompt.text();
            let byte = grapheme_to_byte(&text, composer.prompt.cursor());
            let column = display_width(&text[..byte]);
            return Some((
                metrics
                    .x(placement.x)
                    .saturating_add(2)
                    .saturating_add(column.min(metrics.width.saturating_sub(3))),
                placement
                    .y
                    .saturating_add(text_panel_header_rows(&panel.config)),
            ));
        }
        let wrapped = composer.layout(metrics.width);
        let position = wrapped
            .position(composer.prompt.display_cursor())
            .unwrap_or_default();
        let row = position.row;
        let column = position.column;
        let composer_height = panel.composer_height(placement.height, placement.width);
        let rows = composer_height.checked_sub(panel.composer_chrome_rows())?;
        if rows == 0 {
            return None;
        }
        let first = row.saturating_sub(rows.saturating_sub(1));
        let top = placement.height.saturating_sub(composer_height);
        Some((
            metrics
                .x(placement.x)
                .saturating_add(2)
                .saturating_add(column),
            placement
                .y
                .saturating_add(top)
                .saturating_add(1)
                .saturating_add(row.saturating_sub(first)),
        ))
    }

    pub fn focus_panel_at_position(
        &mut self,
        x: usize,
        y: usize,
        terminal_width: usize,
        terminal_height: usize,
    ) -> Option<PanelEvent> {
        let placement = self.panel_at_position(x, y, terminal_width, terminal_height)?;
        let previous_row_click = self.last_row_click.take();
        self.focused = Some(placement.id.clone());
        if let Some(panel) = self.text_panels.get_mut(&placement.id) {
            let metrics = TextPanelContentMetrics::new(placement.width);
            if y == placement.y && metrics.contains_x(placement.x, x) {
                let column = metrics.column(placement.x, x);
                if let Some((start, text)) = text_panel_header_detail_layout(panel, metrics.width) {
                    if column >= start + 3 && column < start + display_width(&text) {
                        if let Some(action) = panel
                            .header_detail
                            .as_ref()
                            .and_then(|detail| detail.action.as_ref())
                        {
                            return Some(PanelEvent {
                                panel_id: panel.id.clone(),
                                action: action.clone(),
                                selected_index: panel.scroll,
                                row: None,
                                text: None,
                            });
                        }
                    }
                }
                if let Some(action) = text_panel_header_action_at(
                    panel,
                    metrics.width,
                    metrics.column(placement.x, x),
                ) {
                    return Some(PanelEvent {
                        panel_id: panel.id.clone(),
                        action: action.to_string(),
                        selected_index: panel.scroll,
                        row: None,
                        text: None,
                    });
                }
            }

            let picker_top = placement
                .y
                .saturating_add(text_panel_header_rows(&panel.config));
            if !panel.picker_tabs().is_empty()
                && panel.picker_header_rows(placement.height) > 1
                && y == picker_top + 1
            {
                let column = metrics.column(placement.x, x);
                let tab = text_panel_picker_tabs_layout(panel, metrics.width)
                    .tabs
                    .into_iter()
                    .find(|(start, _, label)| {
                        column >= *start && column < start + display_width(label) + 2
                    })
                    .map(|(_, tab, _)| tab.id.clone())?;
                let tab = panel.select_picker_tab(&tab)?;
                return Some(PanelEvent {
                    panel_id: panel.id.clone(),
                    action: "context_tab".to_string(),
                    selected_index: panel.scroll,
                    row: None,
                    text: Some(tab),
                });
            }
            if panel.temporary_context.is_some() && y == picker_top {
                if let Some(composer) = panel.composer.as_mut() {
                    composer.focused = true;
                    let column = x.saturating_sub(metrics.x(placement.x).saturating_add(2));
                    composer.prompt.set_cursor(column);
                }
                panel.blur_scrollback();
                return Some(PanelEvent {
                    panel_id: panel.id.clone(),
                    action: "composer_focus".to_string(),
                    selected_index: panel.scroll,
                    row: None,
                    text: None,
                });
            }

            let composer_height = panel.composer_height(placement.height, placement.width);
            let composer_top = placement
                .y
                .saturating_add(placement.height.saturating_sub(composer_height));
            let chrome = panel.composer_chrome_rows();
            if composer_height > chrome
                && chrome > COMPOSER_CHROME_ROWS
                && y + 1 == composer_top + composer_height
            {
                let column = metrics.column(placement.x, x);
                return text_panel_composer_actions_layout(panel, metrics.width)
                    .into_iter()
                    .find(|(start, _, text)| {
                        column >= *start && column < start + display_width(text)
                    })
                    .map(|(_, action, _)| PanelEvent {
                        panel_id: panel.id.clone(),
                        action: action.id.clone(),
                        selected_index: panel.scroll,
                        row: None,
                        text: None,
                    });
            }
            let action = if composer_height > 0
                && y >= composer_top
                && panel
                    .composer
                    .as_ref()
                    .is_some_and(|composer| composer.enabled)
            {
                if let Some(composer) = panel.composer.as_mut() {
                    composer.focused = true;
                    let wrapped = composer.layout(metrics.width);
                    let cursor_row = wrapped
                        .position(composer.prompt.display_cursor())
                        .map_or(0, |position| position.row);
                    let rows = composer_height.saturating_sub(chrome);
                    let first = cursor_row.saturating_sub(rows.saturating_sub(1));
                    let row = first.saturating_add(y.saturating_sub(composer_top + 1));
                    let column = x.saturating_sub(metrics.x(placement.x).saturating_add(2));
                    if let Some(index) = wrapped.nearest_offset_on_row(row, column) {
                        composer.prompt.set_display_cursor(index);
                    }
                }
                panel.blur_scrollback();
                "composer_focus"
            } else {
                panel.focus_scrollback(placement.width);
                let title_rows = text_panel_header_rows(&panel.config)
                    .saturating_add(panel.picker_header_rows(placement.height));
                let content_height = placement
                    .height
                    .saturating_sub(composer_height)
                    .saturating_sub(panel.status_height());
                let screen_row = y.saturating_sub(placement.y);
                if screen_row >= title_rows && screen_row < content_height {
                    let layout = panel.layout(placement.width);
                    let visible_rows = content_height.saturating_sub(title_rows);
                    let max_scroll = layout.lines.len().saturating_sub(visible_rows);
                    let scroll = panel.viewport.visible_offset(max_scroll);
                    let row = scroll.saturating_add(screen_row.saturating_sub(title_rows));
                    let column = metrics.column(placement.x, x);
                    if let Some(offset) = layout.nearest_offset_on_row(row, column) {
                        panel.scrollback.cursor = offset;
                        panel.scrollback.mouse_anchor = Some(offset);
                        panel.scrollback.mouse_dragging = true;
                        panel.scrollback.mode = TextPanelScrollbackMode::Normal;
                        panel.scrollback.selection_anchor = None;
                        panel.selected_link = None;
                    }
                }
                "select"
            };
            return Some(PanelEvent {
                panel_id: panel.id.clone(),
                action: action.to_string(),
                selected_index: panel.scroll,
                row: None,
                text: None,
            });
        }

        let panel = self.panels.get_mut(&placement.id)?;
        let screen_row = y.saturating_sub(placement.y);
        let content_end = placement
            .height
            .saturating_sub(usize::from(panel.search.is_some()))
            .saturating_sub(usize::from(!panel.status.is_empty()));
        if screen_row >= content_end {
            return None;
        }
        panel.select_screen_row(screen_row);

        let clicked_row = screen_row
            .checked_sub(panel.rows_start())
            .and_then(|index| panel.row(panel.scroll.saturating_add(index)));
        let action = if let Some(row) = clicked_row {
            let now = Instant::now();
            let is_double_click = previous_row_click.is_some_and(|click| {
                click.panel_id == panel.id
                    && click.row_id == row.id
                    && now.duration_since(click.at) <= ROW_DOUBLE_CLICK_INTERVAL
            });
            if is_double_click {
                "activate"
            } else {
                self.last_row_click = Some(RowClick {
                    panel_id: panel.id.clone(),
                    row_id: row.id.clone(),
                    at: now,
                });
                "select"
            }
        } else {
            "select"
        };

        Some(PanelEvent {
            panel_id: panel.id.clone(),
            action: action.to_string(),
            selected_index: panel.selected,
            row: panel.selected_row(),
            text: None,
        })
    }

    pub(crate) fn drag_focused_text_selection(
        &mut self,
        x: usize,
        y: usize,
        terminal_width: usize,
        terminal_height: usize,
    ) -> bool {
        let Some(id) = self.focused.clone() else {
            return false;
        };
        let Some(placement) = self
            .panel_placements(terminal_width, terminal_height)
            .into_iter()
            .find(|placement| placement.id == id)
        else {
            return false;
        };
        let Some(panel) = self.text_panels.get_mut(&id) else {
            return false;
        };
        let Some(anchor) = panel.scrollback.mouse_anchor else {
            return false;
        };
        if !panel.scrollback.mouse_dragging {
            return false;
        }
        let title_rows = text_panel_header_rows(&panel.config);
        let metrics = TextPanelContentMetrics::new(placement.width);
        let content_height = placement
            .height
            .saturating_sub(panel.composer_height(placement.height, placement.width))
            .saturating_sub(panel.status_height());
        if content_height <= title_rows {
            return false;
        }
        let top = placement.y.saturating_add(title_rows);
        let bottom = placement.y.saturating_add(content_height.saturating_sub(1));
        if y < top {
            panel.move_scroll(-1, placement.height, placement.width);
        } else if y > bottom {
            panel.move_scroll(1, placement.height, placement.width);
        }
        let layout = panel.layout(placement.width);
        let visible_rows = content_height.saturating_sub(title_rows);
        let max_scroll = layout.lines.len().saturating_sub(visible_rows);
        let scroll = panel.viewport.visible_offset(max_scroll);
        let screen_row = y.clamp(top, bottom).saturating_sub(top);
        let row = scroll.saturating_add(screen_row);
        let column = x
            .clamp(
                metrics.x(placement.x),
                metrics
                    .x(placement.x)
                    .saturating_add(metrics.width.saturating_sub(1)),
            )
            .saturating_sub(metrics.x(placement.x));
        let Some(offset) = layout.nearest_offset_on_row(row, column) else {
            return false;
        };
        panel.scrollback.cursor = offset;
        if offset != anchor {
            panel.scrollback.mode = TextPanelScrollbackMode::Visual;
            panel.scrollback.selection_anchor = Some(anchor);
            panel.follow_tail = false;
        }
        true
    }

    pub(crate) fn finish_focused_text_selection(&mut self) -> bool {
        let Some(panel) = self
            .focused
            .as_deref()
            .and_then(|id| self.text_panels.get_mut(id))
        else {
            return false;
        };
        let dragging = panel.scrollback.mouse_dragging;
        panel.scrollback.mouse_dragging = false;
        panel.scrollback.mouse_anchor = None;
        dragging
    }

    pub fn panel_at_position(
        &self,
        x: usize,
        y: usize,
        terminal_width: usize,
        terminal_height: usize,
    ) -> Option<PanelPlacement> {
        if y >= terminal_height.saturating_sub(2) {
            return None;
        }

        self.panel_placements(terminal_width, terminal_height)
            .into_iter()
            .find(|placement| {
                y >= placement.y
                    && y < placement.y + placement.height
                    && x >= placement.x
                    && x < placement.x + placement.width
            })
    }

    /// Finds a visible pane divider without treating the pane contents as a grab target.
    pub(crate) fn panel_divider_at_position(
        &self,
        x: usize,
        y: usize,
        terminal_width: usize,
        terminal_height: usize,
    ) -> Option<PanelDivider> {
        if self.presentation != PanelPresentation::Docked {
            return None;
        }
        if x >= terminal_width || y >= terminal_height.saturating_sub(2) {
            return None;
        }

        self.panel_placements(terminal_width, terminal_height)
            .into_iter()
            .find_map(|placement| {
                let config = self.panel_config(&placement.id)?;
                let on_divider = match config.side {
                    PanelSide::Left => {
                        x == placement.x.saturating_add(placement.width)
                            && y >= placement.y
                            && y < placement.y.saturating_add(placement.height)
                    }
                    PanelSide::Right => {
                        placement.x.checked_sub(1) == Some(x)
                            && y >= placement.y
                            && y < placement.y.saturating_add(placement.height)
                    }
                    PanelSide::Top => {
                        y == placement.y.saturating_add(placement.height)
                            && x >= placement.x
                            && x < placement.x.saturating_add(placement.width)
                    }
                    PanelSide::Bottom => {
                        placement.y.checked_sub(1) == Some(y)
                            && x >= placement.x
                            && x < placement.x.saturating_add(placement.width)
                    }
                };

                on_divider.then_some(PanelDivider {
                    id: placement.id,
                    side: config.side,
                })
            })
    }

    fn panel_placements(
        &self,
        terminal_width: usize,
        terminal_height: usize,
    ) -> Vec<PanelPlacement> {
        match &self.presentation {
            PanelPresentation::Hidden => return Vec::new(),
            PanelPresentation::Zoomed { id, .. } => {
                return self
                    .is_visible(id)
                    .then(|| PanelPlacement {
                        id: id.clone(),
                        x: 0,
                        y: 0,
                        width: terminal_width,
                        height: terminal_height.saturating_sub(2),
                    })
                    .into_iter()
                    .collect();
            }
            PanelPresentation::Docked => {}
        }
        let mut placements = Vec::new();
        let mut left_x: usize = 0;
        let mut right_x = terminal_width;
        let content_height = terminal_height.saturating_sub(2);
        let reserved_top = self.reserved_top_height().min(content_height);
        let reserved_bottom = self
            .reserved_bottom_height()
            .min(content_height.saturating_sub(reserved_top));
        let side_height = content_height
            .saturating_sub(reserved_top)
            .saturating_sub(reserved_bottom);
        let mut top_y = 0usize;
        let mut bottom_y = content_height;

        let side_widths = self
            .side_panel_widths(terminal_width)
            .into_iter()
            .map(|(id, _, width)| (id, width))
            .collect::<HashMap<_, _>>();
        // Fixed-width side panels, like the file tree, stay at the outer edge,
        // and conversation panes sit between them and the editor.
        let mut order = self.z_order.iter().collect::<Vec<_>>();
        order.sort_by_key(|id| {
            self.panel_config(id).is_some_and(|config| {
                matches!(config.side, PanelSide::Left | PanelSide::Right)
                    && config.composer.is_some()
            })
        });
        for id in order {
            let Some(config) = self.panel_config(id) else {
                continue;
            };
            let side_width = side_widths.get(id).copied().flatten();
            if side_widths.contains_key(id) && side_width.is_none() {
                continue;
            }

            let (x, y, width, height) = match config.side {
                PanelSide::Left => {
                    let width = side_width
                        .unwrap_or_default()
                        .min(right_x.saturating_sub(left_x));
                    let x = left_x;
                    left_x = left_x.saturating_add(width.saturating_add(1));
                    (x, reserved_top, width, side_height)
                }
                PanelSide::Right => {
                    let width = side_width
                        .unwrap_or_default()
                        .min(right_x.saturating_sub(left_x));
                    right_x = right_x.saturating_sub(width);
                    let x = right_x;
                    right_x = right_x.saturating_sub(1);
                    (x, reserved_top, width, side_height)
                }
                PanelSide::Top => {
                    let height = config.width.min(reserved_top.saturating_sub(top_y));
                    let y = top_y;
                    top_y = top_y.saturating_add(height.saturating_add(1));
                    (0, y, terminal_width, height)
                }
                PanelSide::Bottom => {
                    let available =
                        bottom_y.saturating_sub(content_height.saturating_sub(reserved_bottom));
                    let height = config.width.min(available);
                    let y = bottom_y.saturating_sub(height);
                    bottom_y = y.saturating_sub(1);
                    (0, y, terminal_width, height)
                }
            };

            if width > 0 && height > 0 {
                placements.push(PanelPlacement {
                    id: id.clone(),
                    x,
                    y,
                    width,
                    height,
                });
            }
        }

        placements
    }

    pub fn render(&self, buffer: &mut RenderBuffer, theme: &Theme) {
        self.render_with_active_dividers(buffer, theme, &[], false);
    }

    /// Paints pane chrome while accenting the dividers captured by the editor.
    pub(crate) fn render_with_active_dividers(
        &self,
        buffer: &mut RenderBuffer,
        theme: &Theme,
        active_dividers: &[&str],
        use_ascii: bool,
    ) {
        for placement in self.panel_placements(buffer.width, buffer.height) {
            let _span =
                crate::editor::perf::PerfSpan::with_detail("panel:paint", placement.id.as_str());
            let Some(config) = self.panel_config(&placement.id) else {
                continue;
            };
            let position = Point::new(placement.x, placement.y);
            let is_active = active_dividers.contains(&placement.id.as_str());
            let border_style = panel_style(theme, config.border.as_ref());
            let border_style = if is_active {
                theme.active_divider_style(
                    &border_style,
                    &panel_style(theme, config.surface.as_ref()),
                )
            } else {
                border_style
            };
            let separator = if is_active
                || config.border.is_some()
                || self.text_panels.contains_key(&placement.id)
            {
                if matches!(config.side, PanelSide::Left | PanelSide::Right) {
                    if use_ascii {
                        "|"
                    } else {
                        "│"
                    }
                } else if use_ascii {
                    "-"
                } else {
                    "─"
                }
            } else {
                " "
            };
            if self.presentation == PanelPresentation::Docked {
                render_panel_separator(
                    buffer,
                    position,
                    placement.width,
                    placement.height,
                    config.side,
                    &border_style,
                    separator,
                );
            }

            if let Some(panel) = self.panels.get(&placement.id) {
                render_panel_at(
                    buffer,
                    panel,
                    position,
                    placement.width,
                    placement.height,
                    theme,
                );
            } else if let Some(panel) = self.text_panels.get(&placement.id) {
                render_text_panel(
                    buffer,
                    panel,
                    position,
                    placement.width,
                    placement.height,
                    theme,
                );
            }
        }
    }

    fn panel_config(&self, id: &str) -> Option<&PanelConfig> {
        self.panels
            .get(id)
            .map(|panel| &panel.config)
            .or_else(|| self.text_panels.get(id).map(|panel| &panel.config))
    }

    fn panel_config_mut(&mut self, id: &str) -> Option<&mut PanelConfig> {
        if let Some(panel) = self.panels.get_mut(id) {
            return Some(&mut panel.config);
        }
        self.text_panels.get_mut(id).map(|panel| &mut panel.config)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelPlacement {
    pub id: String,
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PanelDivider {
    pub(crate) id: String,
    pub(crate) side: PanelSide,
}

#[cfg(test)]
fn render_panel(
    buffer: &mut RenderBuffer,
    panel: &PluginPanel,
    position: Point,
    width: usize,
    theme: &Theme,
) {
    render_panel_at(
        buffer,
        panel,
        position,
        width,
        buffer.height.saturating_sub(2),
        theme,
    );
}

fn render_panel_at(
    buffer: &mut RenderBuffer,
    panel: &PluginPanel,
    position: Point,
    width: usize,
    height: usize,
    theme: &Theme,
) {
    if width == 0 || height == 0 {
        return;
    }

    let surface_style = panel_style(theme, panel.config.surface.as_ref());
    let selection_style = theme.list_selection_style();
    let selected_style = theme.selected_style(
        &surface_style,
        &selection_style,
        SelectionForegroundPriority::Selection,
    );
    let title_style = Style {
        bold: true,
        ..surface_style.clone()
    };

    for y in 0..height {
        buffer.set_text(
            position.x,
            position.y.saturating_add(y),
            &" ".repeat(width),
            &surface_style,
        );
    }

    if let Some(title) = &panel.config.title {
        buffer.set_text(
            position.x,
            position.y,
            &fit_display_width(title, width),
            &title_style,
        );
    }

    let rows_start = if panel.config.title.is_some() { 1 } else { 0 };
    let visible_rows = height
        .saturating_sub(rows_start)
        .saturating_sub(usize::from(panel.search.is_some()))
        .saturating_sub(usize::from(!panel.status.is_empty()));
    let mut paint_row = |screen_row: usize, row: &PanelRow| {
        let y = position.y.saturating_add(rows_start + screen_row);
        let index = panel.scroll + screen_row;
        let selected = index == panel.selected;
        if selected {
            buffer.set_text(position.x, y, &" ".repeat(width), &selected_style);
        }

        render_row_segments(
            buffer,
            Point::new(position.x, y),
            width,
            row,
            theme,
            &surface_style,
            selected,
        );
    };
    if let Some(tree) = panel.tree.as_ref() {
        for (screen_row, index) in (panel.scroll..tree.len()).take(visible_rows).enumerate() {
            if let Some(row) = tree.row(index) {
                paint_row(screen_row, &row);
            }
        }
    } else {
        for (screen_row, row) in panel
            .rows
            .iter()
            .skip(panel.scroll)
            .take(visible_rows)
            .enumerate()
        {
            paint_row(screen_row, row);
        }
    }
    let search_height = usize::from(panel.search.is_some());
    if !panel.status.is_empty() && height > search_height {
        buffer.set_text(
            position.x,
            position.y + height - search_height - 1,
            &fit_display_width(&panel.status, width),
            &surface_style,
        );
    }
    if let Some(search) = &panel.search {
        let row = position.y.saturating_add(height.saturating_sub(1));
        let prefix_width = display_width(&search.prefix);
        let status_width = display_width(&search.status);
        let show_status = width >= prefix_width + status_width + 4;
        let available = width
            .saturating_sub(prefix_width)
            .saturating_sub(if show_status { status_width + 1 } else { 0 });
        let text = search.input.text();
        let cursor_byte = grapheme_to_byte(&text, search.input.cursor());
        let before =
            truncate_display_width_from_end(&text[..cursor_byte], available.saturating_sub(1));
        let start = cursor_byte.saturating_sub(before.len());
        let query = truncate_display_width(&text[start..], available);
        let accent = theme.list_selection_style();
        buffer.set_text(position.x, row, &" ".repeat(width), &surface_style);
        buffer.set_text(position.x, row, &search.prefix, &accent);
        buffer.set_text(position.x + prefix_width, row, &query, &surface_style);
        if show_status {
            buffer.set_text(
                position.x + width - status_width,
                row,
                &search.status,
                &surface_style,
            );
        }
    }
}

fn render_text_panel(
    buffer: &mut RenderBuffer,
    panel: &TextPanel,
    position: Point,
    width: usize,
    height: usize,
    theme: &Theme,
) {
    if width == 0 || height == 0 {
        return;
    }
    let palette = text_panel_palette(theme, &panel.config);
    let metrics = TextPanelContentMetrics::new(width);
    let content_position = Point::new(metrics.x(position.x), position.y);

    for y in 0..height {
        buffer.set_text(
            position.x,
            position.y.saturating_add(y),
            &" ".repeat(width),
            &palette.surface,
        );
    }

    let header_actions = text_panel_header_actions_for_panel(panel, metrics.width);
    let title_rows = text_panel_header_rows(&panel.config);
    let title_width = header_actions
        .first()
        .map_or(metrics.width, |(start, _, _)| start.saturating_sub(1));
    if let Some(title) = &panel.config.title {
        let title_style = Style {
            bold: true,
            ..palette.primary.clone()
        };
        buffer.set_text(
            content_position.x,
            position.y,
            &fit_display_width(title, title_width),
            &title_style,
        );
    }
    if let Some((start, text)) = text_panel_header_detail_layout(panel, metrics.width) {
        buffer.set_text(
            content_position.x + start,
            position.y,
            &text,
            &palette.secondary,
        );
    }
    for (start, action, label) in header_actions {
        buffer.set_text(
            content_position.x + start + 1,
            position.y,
            &label,
            if action.enabled {
                &palette.secondary
            } else {
                &palette.muted
            },
        );
    }
    if title_rows > 0 {
        let divider = "─".repeat(width);
        buffer.set_text(
            position.x,
            position.y.saturating_add(1),
            &divider,
            &palette.divider,
        );
    }

    let composer_height = panel.composer_height(height, width);
    let status_height = panel.status_height();
    let action_footer_height = panel.action_footer_height();
    let picker_header_rows = panel.picker_header_rows(height);
    let content_height = height
        .saturating_sub(composer_height)
        .saturating_sub(status_height)
        .saturating_sub(action_footer_height);
    let available_rows = content_height
        .saturating_sub(title_rows)
        .saturating_sub(picker_header_rows);
    let preview_geometry = panel.picker_preview_geometry(height, width);
    let visible_rows = preview_geometry.map_or(available_rows, |geometry| geometry.list_rows);
    let list_width = preview_geometry.map_or(metrics.width, |geometry| geometry.list_width);
    if picker_header_rows > 0 {
        render_text_panel_picker_header(
            buffer,
            panel,
            content_position,
            metrics.width,
            title_rows,
            picker_header_rows,
            &palette,
        );
    }
    let layout = panel.layout(width);
    let max_scroll = layout.lines.len().saturating_sub(visible_rows);
    let scroll = panel.viewport.visible_offset(max_scroll);
    let selection = panel.selection_bounds(&layout);
    let search_matches = panel
        .scrollback
        .focused
        .then(|| panel.search.query())
        .flatten()
        .map(|query| panel.search_matches(&layout, &query.text));
    let search_highlights = search_matches.as_ref().map(|matches| {
        TextPanelSearchHighlights::new(
            Arc::clone(matches),
            panel.scrollback.cursor,
            theme,
            &palette,
        )
    });
    let selected_prompt = panel.selected_prompt(&layout);
    let normal_prompt = text_panel_prompt_palette(theme, &palette, false);
    let active_prompt = text_panel_prompt_palette(theme, &palette, true);
    for (offset, line) in layout
        .rendered
        .iter()
        .skip(scroll)
        .take(visible_rows)
        .enumerate()
    {
        let line_index = scroll.saturating_add(offset);
        let y = position
            .y
            .saturating_add(title_rows + picker_header_rows + offset);
        if panel.temporary_context.is_some() {
            if let Some(link) = picker_action_link(line) {
                let title = line_index == 0
                    || layout
                        .rendered
                        .get(line_index - 1)
                        .and_then(picker_action_link)
                        .is_none_or(|previous| previous.id != link.id);
                render_text_panel_picker_item(
                    buffer,
                    Point::new(content_position.x, y),
                    list_width,
                    line,
                    Some(link.id) == panel.selected_link,
                    title,
                    &palette,
                );
                continue;
            }
            if render_text_panel_picker_group(
                buffer,
                Point::new(content_position.x, y),
                list_width,
                line,
                &palette,
            ) {
                continue;
            }
        }
        let prompt_card = layout
            .prompt_cards
            .iter()
            .find(|card| card.rows.contains(&line_index));
        let prompt_palette = prompt_card.map(|card| {
            if selected_prompt == Some(card.block_index) {
                &active_prompt
            } else {
                &normal_prompt
            }
        });
        let framed_prompt = prompt_palette.is_some() && list_width >= 7;
        let edge_inset = usize::from(framed_prompt);
        if let (Some(card), Some(prompt)) = (prompt_card, prompt_palette) {
            let cap = if line_index == card.rows.start {
                Some(("▄", "╷"))
            } else if line_index + 1 == card.rows.end {
                Some(("▀", "╵"))
            } else {
                None
            };
            if let Some((cap, rail)) = cap {
                buffer.set_text(
                    content_position.x + edge_inset,
                    y,
                    &cap.repeat(list_width.saturating_sub(edge_inset * 2)),
                    &prompt.cap,
                );
                if framed_prompt {
                    render_prompt_rails(
                        buffer,
                        Point::new(content_position.x, y),
                        list_width,
                        rail,
                        &prompt.edge,
                    );
                }
                continue;
            }
            buffer.set_text(
                content_position.x + edge_inset,
                y,
                &" ".repeat(list_width.saturating_sub(edge_inset * 2)),
                &prompt.content.surface,
            );
        }
        render_text_spans(
            buffer,
            content_position.x,
            y,
            list_width.saturating_sub(usize::from(framed_prompt)),
            line,
            layout.lines.get(line_index).map_or(0, |line| line.first),
            layout
                .lines
                .get(line_index)
                .is_some_and(|line| line.selectable)
                .then_some(selection)
                .flatten(),
            panel.selected_link,
            search_highlights.as_ref(),
            theme,
            prompt_palette.map_or(&palette, |prompt| &prompt.content),
        );
        if framed_prompt {
            if let Some(prompt) = prompt_palette {
                render_prompt_rails(
                    buffer,
                    Point::new(content_position.x, y),
                    list_width,
                    "│",
                    &prompt.edge,
                );
            }
        }
    }

    if let (Some(geometry), Some(preview)) = (
        preview_geometry,
        panel
            .temporary_context
            .as_ref()
            .and_then(|context| context.preview.as_ref()),
    ) {
        render_text_panel_preview(
            buffer,
            Point::new(
                content_position.x,
                position.y.saturating_add(title_rows + picker_header_rows),
            ),
            geometry,
            preview,
            theme,
            &palette,
        );
    }

    if let Some(status) = &panel.status {
        render_text_panel_status(
            buffer,
            panel,
            status,
            content_position,
            metrics.width,
            content_height + status_height.saturating_sub(1),
            &palette,
        );
    }

    if action_footer_height > 0 && panel.scrollback.focused {
        let actions = panel
            .config
            .header_actions
            .iter()
            .map(|action| {
                let mut hint = action.ui_action();
                if action.show_in_header {
                    hint.priority = ActionPriority::Reference;
                }
                hint
            })
            .collect::<Vec<_>>();
        ActionBar::new(&actions)
            .with_context(panel.config.title.as_deref().unwrap_or("Text panel"))
            .render(
                buffer,
                content_position.x,
                position.y + content_height + status_height,
                metrics.width,
                theme,
                &palette.surface,
            );
    }

    if let Some(composer) = panel.composer.as_ref().filter(|_| composer_height > 0) {
        let overflow = match (scroll > 0, scroll < max_scroll) {
            (true, true) => TextPanelOverflow::Both,
            (true, false) => TextPanelOverflow::Above,
            (false, true) => TextPanelOverflow::Below,
            (false, false) => TextPanelOverflow::None,
        };
        let composer_top = position
            .y
            .saturating_add(content_height)
            .saturating_add(status_height)
            .saturating_add(action_footer_height);
        buffer.set_text(
            position.x,
            composer_top,
            &"─".repeat(width),
            if composer.focused {
                &palette.accent
            } else {
                &palette.divider
            },
        );
        let help_start = buffer.shortcut_help_regions.len();
        render_text_panel_composer(
            buffer,
            composer,
            &panel.scrollback,
            content_position,
            metrics.width,
            content_height + status_height,
            composer_height.saturating_sub(panel.composer_chrome_rows()),
            overflow,
            &palette,
            theme,
        );
        if panel.composer_chrome_rows() > COMPOSER_CHROME_ROWS {
            let y = composer_top + composer_height - 1;
            for (start, action, _) in text_panel_composer_actions_layout(panel, metrics.width) {
                let x = content_position.x + start;
                if start > 0 {
                    buffer.set_text(x - 2, y, "·", &palette.divider);
                }
                buffer.set_text(x, y, &action.key, &palette.accent);
                buffer.set_text(
                    x + display_width(&action.key) + 1,
                    y,
                    &action.label,
                    &palette.secondary,
                );
            }
        }
        let context = format!(
            "{} · {}",
            panel.config.title.as_deref().unwrap_or("Text panel"),
            text_panel_composer_hints(composer, &panel.scrollback).0
        );
        for help in &mut buffer.shortcut_help_regions[help_start..] {
            help.context.clone_from(&context);
        }
    }
    if let (Some(matches), Some(row)) = (search_matches, panel.search_bar_row(height)) {
        if let Some(bar) = TextPanelSearchBar::new(
            &panel.search,
            &matches,
            panel.scrollback.cursor,
            metrics.width,
        ) {
            render_text_panel_search_bar(
                buffer,
                Point::new(content_position.x, position.y + row),
                metrics.width,
                &bar,
                &palette,
            );
        }
    }
}

fn render_text_panel_picker_header(
    buffer: &mut RenderBuffer,
    panel: &TextPanel,
    position: Point,
    width: usize,
    title_rows: usize,
    rows: usize,
    palette: &TextPanelPalette,
) {
    let Some(composer) = panel.composer.as_ref() else {
        return;
    };
    let y = position.y.saturating_add(title_rows);
    let search = SurfaceCardPalette::new(palette, false, SurfaceCardColors::default());
    buffer.set_text(position.x, y, &" ".repeat(width), &search.content.surface);
    let prefix = "⌕ ";
    if width >= display_width(prefix) {
        buffer.set_text(position.x, y, prefix, &search.content.accent);
        let query = composer.prompt.text();
        let (text, style) = if query.is_empty() {
            (
                composer.config.placeholder.as_str(),
                &search.content.secondary,
            )
        } else {
            (query.as_str(), &search.content.primary)
        };
        buffer.set_text(
            position.x.saturating_add(display_width(prefix)),
            y,
            &fit_display_width(text, width.saturating_sub(display_width(prefix))),
            style,
        );
    }
    let mut row = 1;
    let tabs = !panel.picker_tabs().is_empty();
    if tabs && rows > row {
        render_text_panel_picker_tabs(
            buffer,
            panel,
            Point::new(position.x, y.saturating_add(row)),
            width,
            palette,
        );
        row += 1;
    }
    if rows > row {
        let mut hints = vec![
            ("↑↓", "select"),
            ("Enter", "resume"),
            ("Esc", "back"),
            ("Alt-P", "peek"),
        ];
        // The tabs row names its own shortcut.
        if !tabs {
            hints.push(("Alt-S", "scope"));
        }
        render_picker_hints(
            buffer,
            Point::new(position.x, y.saturating_add(row)),
            width,
            &hints,
            palette,
        );
        row += 1;
    }
    if rows > row {
        buffer.set_text(
            position.x,
            y.saturating_add(row),
            &"─".repeat(width),
            &palette.divider,
        );
    }
}

/// Paints whole `key label` hints, leaving out those that no longer fit.
fn render_picker_hints(
    buffer: &mut RenderBuffer,
    position: Point,
    width: usize,
    hints: &[(&str, &str)],
    palette: &TextPanelPalette,
) {
    const SEPARATOR: &str = "  ·  ";
    let mut used = 0usize;
    for (index, (key, label)) in hints.iter().enumerate() {
        let separator = if index == 0 {
            0
        } else {
            display_width(SEPARATOR)
        };
        let end = used + separator + display_width(key) + 1 + display_width(label);
        if end > width {
            break;
        }
        if separator > 0 {
            buffer.set_text(position.x + used, position.y, SEPARATOR, &palette.divider);
        }
        let x = position.x + used + separator;
        buffer.set_text(x, position.y, key, &palette.accent);
        buffer.set_text(
            x + display_width(key) + 1,
            position.y,
            label,
            &palette.secondary,
        );
        used = end;
    }
}

#[derive(Default)]
struct TextPanelPickerTabsLayout<'a> {
    /// Each tab's start column, the tab, and the label shown for it.
    tabs: Vec<(usize, &'a TextPanelContextTab, &'a str)>,
    shortcut: Option<(usize, &'a str)>,
}

/// Places the tabs, each padded by a cell on either side, then their shortcut.
/// Narrower panes use compact labels, and then show only the selected tab.
fn text_panel_picker_tabs_layout(panel: &TextPanel, width: usize) -> TextPanelPickerTabsLayout<'_> {
    let Some(context) = panel.temporary_context.as_ref() else {
        return TextPanelPickerTabsLayout::default();
    };
    fn compact(tab: &TextPanelContextTab) -> &str {
        if tab.compact_label.is_empty() {
            &tab.label
        } else {
            &tab.compact_label
        }
    }
    let selected = context
        .tabs
        .iter()
        .filter(|tab| tab.id == context.tab)
        .map(|tab| (tab, compact(tab)))
        .collect::<Vec<_>>();
    let candidates = [
        context
            .tabs
            .iter()
            .map(|tab| (tab, tab.label.as_str()))
            .collect::<Vec<_>>(),
        context.tabs.iter().map(|tab| (tab, compact(tab))).collect(),
        selected,
    ];
    // Tab always moves between tabs, so it is the hint shown.
    const SHORTCUT: &str = "Tab";
    let shortcut_width = display_width(SHORTCUT);
    let tabs_width = |tabs: &[(&TextPanelContextTab, &str)]| {
        tabs.iter()
            .map(|(_, label)| display_width(label) + 2)
            .sum::<usize>()
            + tabs.len().saturating_sub(1)
    };
    let tabs = candidates
        .iter()
        .find(|tabs| tabs_width(tabs) <= width)
        .or(candidates.last())
        .cloned()
        .unwrap_or_default();
    let used = tabs_width(&tabs);
    let mut start = 0;
    let tabs = tabs
        .into_iter()
        .map(|(tab, label)| {
            let current = start;
            start += display_width(label) + 3;
            (current, tab, label)
        })
        .collect();
    let shortcut = Some(used + 3)
        .filter(|start| start + shortcut_width <= width)
        .map(|start| (start, SHORTCUT));
    TextPanelPickerTabsLayout { tabs, shortcut }
}

fn render_text_panel_picker_tabs(
    buffer: &mut RenderBuffer,
    panel: &TextPanel,
    position: Point,
    width: usize,
    palette: &TextPanelPalette,
) {
    let selected = panel
        .temporary_context
        .as_ref()
        .map_or("", |context| context.tab.as_str());
    let card = SurfaceCardPalette::new(palette, true, SurfaceCardColors::default());
    let active = Style {
        bold: true,
        ..card.content.accent.clone()
    };
    let layout = text_panel_picker_tabs_layout(panel, width);
    for (start, tab, label) in layout.tabs {
        let style = if tab.id == selected {
            &active
        } else {
            &palette.secondary
        };
        buffer.set_text(
            position.x + start,
            position.y,
            &fit_display_width(&format!(" {label} "), width.saturating_sub(start)),
            style,
        );
    }
    if let Some((start, shortcut)) = layout.shortcut {
        buffer.set_text(position.x + start, position.y, shortcut, &palette.muted);
    }
}

/// Places the pane actions listed under the composer as `key label` pieces.
fn text_panel_composer_actions_layout(
    panel: &TextPanel,
    width: usize,
) -> Vec<(usize, &TextPanelHeaderAction, String)> {
    let mut placed = Vec::new();
    let mut used = 0usize;
    for action in panel.composer_actions() {
        let text = format!("{} {}", action.key, action.label);
        let start = if placed.is_empty() { 0 } else { used + 3 };
        let end = start + display_width(&text);
        if end > width {
            break;
        }
        placed.push((start, action, text));
        used = end;
    }
    placed
}

fn picker_item_text_inset(width: usize) -> usize {
    if width > EMBEDDED_PICKER_ITEM_INSET {
        EMBEDDED_PICKER_ITEM_INSET
    } else {
        0
    }
}

fn picker_action_link(line: &RenderedTextLine) -> Option<&TextPanelLink> {
    line.spans.iter().find_map(|span| {
        span.link
            .as_ref()
            .filter(|link| matches!(&link.target, TextPanelLinkTarget::PanelAction { .. }))
    })
}

fn render_text_panel_picker_group(
    buffer: &mut RenderBuffer,
    position: Point,
    width: usize,
    line: &RenderedTextLine,
    palette: &TextPanelPalette,
) -> bool {
    let heading = line
        .spans
        .iter()
        .map(|span| span.text.as_str())
        .collect::<String>();
    if !matches!(
        heading.as_str(),
        "Recent" | "This week" | "This month" | "Earlier"
    ) {
        return false;
    }
    let label = format!("●  {}", heading.to_uppercase());
    let label_width = display_width(&label);
    buffer.set_text(
        position.x,
        position.y,
        &fit_display_width(&label, width),
        &palette.accent,
    );
    if width > label_width.saturating_add(2) {
        let start = label_width.saturating_add(2);
        buffer.set_text(
            position.x.saturating_add(start),
            position.y,
            &"─".repeat(width.saturating_sub(start)),
            &palette.divider,
        );
    }
    true
}

fn render_text_panel_picker_item(
    buffer: &mut RenderBuffer,
    position: Point,
    width: usize,
    line: &RenderedTextLine,
    selected: bool,
    title: bool,
    palette: &TextPanelPalette,
) {
    let card = SurfaceCardPalette::new(palette, selected, SurfaceCardColors::default());
    buffer.set_text(
        position.x,
        position.y,
        &" ".repeat(width),
        &card.content.surface,
    );
    let inset = picker_item_text_inset(width);
    if inset > 0 {
        buffer.set_text(
            position.x,
            position.y,
            if selected { "▌" } else { "│" },
            &card.edge,
        );
    }
    let content_position = Point::new(position.x.saturating_add(inset), position.y);
    let content_width = width.saturating_sub(inset);
    let text = line
        .spans
        .iter()
        .map(|span| span.text.as_str())
        .collect::<String>();
    if title {
        let style = Style {
            bold: true,
            ..card.content.primary.clone()
        };
        buffer.set_text(
            content_position.x,
            content_position.y,
            &fit_display_width(&text, content_width),
            &style,
        );
        return;
    }

    let mut fragments = Vec::new();
    for (index, segment) in text.split(" · ").enumerate() {
        if index > 0 {
            fragments.push((" · ", &card.content.divider));
        }
        let style = if segment == "Red" || segment == "Continue in Red" {
            &card.content.accent
        } else if index == 0 {
            &card.content.secondary
        } else {
            &card.content.muted
        };
        fragments.push((segment, style));
    }
    render_picker_fragments(buffer, content_position, content_width, &fragments);
}

fn render_picker_fragments(
    buffer: &mut RenderBuffer,
    position: Point,
    width: usize,
    fragments: &[(&str, &Style)],
) {
    let mut used = 0usize;
    for &(text, style) in fragments {
        if used >= width {
            break;
        }
        let text = truncate_display_width(text, width.saturating_sub(used));
        buffer.set_text(position.x.saturating_add(used), position.y, &text, style);
        used = used.saturating_add(display_width(&text));
    }
}

fn render_text_panel_preview(
    buffer: &mut RenderBuffer,
    position: Point,
    geometry: TextPanelPickerPreviewGeometry,
    preview: &TextPanelPreview,
    theme: &Theme,
    palette: &TextPanelPalette,
) {
    if geometry.side_by_side {
        for offset in 0..geometry.preview_rows {
            buffer.set_text(
                position.x.saturating_add(geometry.list_width),
                position.y.saturating_add(offset),
                "│",
                &palette.divider,
            );
        }
    } else {
        buffer.set_text(
            position.x,
            position.y.saturating_add(geometry.list_rows),
            &"─".repeat(geometry.list_width),
            &palette.divider,
        );
    }

    let inset = usize::from(geometry.preview_width > 2);
    let x = position
        .x
        .saturating_add(geometry.preview_column)
        .saturating_add(inset);
    let y = position.y.saturating_add(geometry.preview_row);
    let width = geometry.preview_width.saturating_sub(inset);
    if width == 0 || geometry.preview_rows == 0 {
        return;
    }
    buffer.set_text(
        x,
        y,
        &fit_display_width("LAST EXCHANGE", width),
        &palette.accent,
    );
    if geometry.preview_rows == 1 {
        return;
    }
    if preview.loading || !preview.error.is_empty() {
        let (text, style) = if preview.loading {
            ("Loading preview…", &palette.secondary)
        } else {
            (preview.error.as_str(), &palette.error)
        };
        for (offset, line) in wrap_plain_text(text, width, TextPanelSpanStyle::Text)
            .into_iter()
            .take(geometry.preview_rows.saturating_sub(1))
            .enumerate()
        {
            let text = line
                .spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>();
            buffer.set_text(x, y.saturating_add(offset + 1), &text, style);
        }
        return;
    }

    let mut row = 1usize;
    buffer.set_text(x, y + row, "YOU", &palette.secondary);
    row += 1;
    let prompt = if preview.prompt.is_empty() {
        "No previous prompt"
    } else {
        preview.prompt.as_str()
    };
    let prompt_budget = geometry.preview_rows.saturating_sub(3).div_ceil(3).max(1);
    for line in wrap_plain_text(prompt, width, TextPanelSpanStyle::Text)
        .into_iter()
        .take(prompt_budget)
    {
        if row + 2 >= geometry.preview_rows {
            break;
        }
        render_text_spans(
            buffer,
            x,
            y + row,
            width,
            &line,
            0,
            None,
            None,
            None,
            theme,
            palette,
        );
        row += 1;
    }
    if geometry.preview_rows.saturating_sub(row) > 3 {
        row += 1;
    }
    if row >= geometry.preview_rows {
        return;
    }
    buffer.set_text(x, y + row, "AGENT", &palette.accent);
    row += 1;
    let response = if preview.response.is_empty() {
        "No response yet"
    } else {
        preview.response.as_str()
    };
    for line in render_markdown_lines(response, width) {
        if row >= geometry.preview_rows {
            break;
        }
        render_text_spans(
            buffer,
            x,
            y + row,
            width,
            &line,
            0,
            None,
            None,
            None,
            theme,
            palette,
        );
        row += 1;
    }
}

fn render_text_panel_search_bar(
    buffer: &mut RenderBuffer,
    position: Point,
    width: usize,
    bar: &TextPanelSearchBar,
    palette: &TextPanelPalette,
) {
    buffer.set_text(position.x, position.y, &" ".repeat(width), &palette.surface);
    if width == 0 {
        return;
    }
    buffer.set_text(position.x, position.y, bar.prefix, &palette.accent);
    buffer.set_text(position.x + 1, position.y, &bar.text, &palette.primary);
    buffer.set_text(
        position.x + bar.suffix_column,
        position.y,
        &bar.suffix,
        &palette.secondary,
    );
}

#[derive(Clone, Copy)]
enum TextPanelOverflow {
    None,
    Above,
    Below,
    Both,
}

#[derive(Clone, Copy)]
struct TextPanelShortcutHint {
    keys: &'static str,
    action: &'static str,
}

const SCROLLBACK_NORMAL_HINTS: &[TextPanelShortcutHint] = &[
    TextPanelShortcutHint {
        keys: "Tab",
        action: "edit",
    },
    TextPanelShortcutHint {
        keys: "m",
        action: "actions",
    },
    TextPanelShortcutHint {
        keys: "/?",
        action: "search",
    },
    TextPanelShortcutHint {
        keys: "[p/]p",
        action: "prompt",
    },
    TextPanelShortcutHint {
        keys: "[l/]l",
        action: "link",
    },
    TextPanelShortcutHint {
        keys: "G",
        action: "latest",
    },
    TextPanelShortcutHint {
        keys: "hjkl/arrows",
        action: "move",
    },
    TextPanelShortcutHint {
        keys: "y",
        action: "copy",
    },
    TextPanelShortcutHint {
        keys: "v/V",
        action: "select",
    },
];
const SCROLLBACK_VISUAL_HINTS: &[TextPanelShortcutHint] = &[
    TextPanelShortcutHint {
        keys: "Enter",
        action: "copy + latest",
    },
    TextPanelShortcutHint {
        keys: "motions",
        action: "extend",
    },
    TextPanelShortcutHint {
        keys: "y",
        action: "copy",
    },
    TextPanelShortcutHint {
        keys: "Esc",
        action: "cancel",
    },
];
const COMPOSER_NORMAL_HINTS: &[TextPanelShortcutHint] = &[
    TextPanelShortcutHint {
        keys: "Enter",
        action: "send",
    },
    TextPanelShortcutHint {
        keys: "i/a",
        action: "edit",
    },
    TextPanelShortcutHint {
        keys: "Tab",
        action: "transcript",
    },
    TextPanelShortcutHint {
        keys: "j/k ↑/↓",
        action: "scroll",
    },
    TextPanelShortcutHint {
        keys: "v",
        action: "select",
    },
    TextPanelShortcutHint {
        keys: "u",
        action: "undo",
    },
];
const COMPOSER_VISUAL_HINTS: &[TextPanelShortcutHint] = &[
    TextPanelShortcutHint {
        keys: "d/c",
        action: "edit",
    },
    TextPanelShortcutHint {
        keys: "y",
        action: "yank",
    },
    TextPanelShortcutHint {
        keys: "Esc",
        action: "normal",
    },
];
const COMPOSER_SEARCH_HINTS: &[TextPanelShortcutHint] = &[
    TextPanelShortcutHint {
        keys: "Enter",
        action: "find",
    },
    TextPanelShortcutHint {
        keys: "Esc",
        action: "cancel",
    },
];
const COMPOSER_HISTORY_SEARCH_HINTS: &[TextPanelShortcutHint] = &[
    TextPanelShortcutHint {
        keys: "^R/↑/↓",
        action: "matches",
    },
    TextPanelShortcutHint {
        keys: "Enter",
        action: "accept",
    },
    TextPanelShortcutHint {
        keys: "Esc/^G",
        action: "cancel",
    },
];
const COMPOSER_INSERT_HINTS: &[TextPanelShortcutHint] = &[
    TextPanelShortcutHint {
        keys: "Enter",
        action: "send",
    },
    TextPanelShortcutHint {
        keys: "^J",
        action: "newline",
    },
    TextPanelShortcutHint {
        keys: "Tab",
        action: "transcript",
    },
    TextPanelShortcutHint {
        keys: "Esc",
        action: "normal",
    },
    TextPanelShortcutHint {
        keys: "^K",
        action: "scroll",
    },
    TextPanelShortcutHint {
        keys: "↑/↓",
        action: "history",
    },
    TextPanelShortcutHint {
        keys: "^R",
        action: "history search",
    },
    TextPanelShortcutHint {
        keys: "^g/^G",
        action: "ends",
    },
];
const PANEL_NAVIGATION_HINTS: &[TextPanelShortcutHint] = &[
    TextPanelShortcutHint {
        keys: "a",
        action: "edit",
    },
    TextPanelShortcutHint {
        keys: "j/k",
        action: "scroll",
    },
    TextPanelShortcutHint {
        keys: "q",
        action: "close",
    },
    TextPanelShortcutHint {
        keys: "^C",
        action: "stop",
    },
    TextPanelShortcutHint {
        keys: "g/G",
        action: "ends",
    },
];

fn append_text_panel_piece(
    buffer: &mut RenderBuffer,
    position: Point,
    y: usize,
    width: usize,
    used: &mut usize,
    text: &str,
    style: &Style,
) {
    let remaining = width.saturating_sub(*used);
    if remaining == 0 || text.is_empty() {
        return;
    }
    let text = truncate_display_width(text, remaining);
    if text.is_empty() {
        return;
    }
    buffer.set_text(position.x.saturating_add(*used), y, &text, style);
    *used = used.saturating_add(display_width(&text));
}

fn text_panel_composer_hints(
    composer: &TextPanelComposer,
    scrollback: &TextPanelScrollback,
) -> (&'static str, &'static [TextPanelShortcutHint]) {
    if scrollback.focused {
        return match scrollback.mode {
            TextPanelScrollbackMode::Normal => ("SCROLLBACK NORMAL", SCROLLBACK_NORMAL_HINTS),
            TextPanelScrollbackMode::Visual => ("SCROLLBACK VISUAL", SCROLLBACK_VISUAL_HINTS),
            TextPanelScrollbackMode::VisualLine => {
                ("SCROLLBACK VISUAL LINE", SCROLLBACK_VISUAL_HINTS)
            }
        };
    }
    if composer.prompt.history_search_active() {
        return ("REVERSE SEARCH", COMPOSER_HISTORY_SEARCH_HINTS);
    }
    match (composer.focused, composer.prompt.mode()) {
        (true, crate::editor::Mode::Normal) => ("NORMAL", COMPOSER_NORMAL_HINTS),
        (
            true,
            crate::editor::Mode::Visual
            | crate::editor::Mode::VisualLine
            | crate::editor::Mode::VisualBlock,
        ) => ("VISUAL", COMPOSER_VISUAL_HINTS),
        (true, crate::editor::Mode::Search) => ("SEARCH", COMPOSER_SEARCH_HINTS),
        (true, _) => ("INSERT", COMPOSER_INSERT_HINTS),
        (false, _) => ("NAV", PANEL_NAVIGATION_HINTS),
    }
}

/// Full labels and grouping for the same actions used in the compact footer.
fn text_panel_hint_action(_mode: &str, hint: &TextPanelShortcutHint, index: usize) -> UiAction {
    let (group, label) = match (hint.keys, hint.action) {
        ("a" | "i/a", "edit") => ("Composer", "Edit the follow-up"),
        ("Tab", "edit") => ("Focus", "Focus the composer"),
        ("Tab", "transcript") => ("Focus", "Focus the transcript"),
        ("Enter", "send") => ("Composer", "Send the follow-up"),
        ("^J", "newline") => ("Composer", "Insert a new line"),
        ("m", "actions") => ("Conversation", "Actions for the selected turn"),
        ("/?", "search") => ("Search", "Search forward / backward"),
        ("[p/]p", "prompt") => ("Navigation", "Previous / next prompt"),
        ("[l/]l", "link") => ("Navigation", "Previous / next link"),
        ("G", "latest") => ("Navigation", "Jump to latest output"),
        ("hjkl/arrows", "move") => ("Navigation", "Move the transcript cursor"),
        ("j/k" | "j/k ↑/↓", "scroll") => ("Navigation", "Scroll down / up"),
        ("g/G" | "^g/^G", "ends") => ("Navigation", "Jump to first / latest output"),
        ("q", "close") => ("Pane", "Close the pane"),
        ("^C", "stop") => ("Conversation", "Stop the current turn"),
        ("v/V", "select") => ("Selection", "Select characters / lines"),
        ("motions", "extend") => ("Selection", "Extend the selection"),
        ("y", "copy" | "yank") => ("Selection", "Copy the selection"),
        ("u", "undo") => ("Editing", "Undo the last edit"),
        ("Esc", "normal") => ("Mode", "Return to Normal mode"),
        ("Esc", "cancel") => ("Mode", "Cancel the selection or search"),
        ("Enter", "find") => ("Search", "Accept the search"),
        ("Enter", "accept") => ("History", "Load the selected prompt"),
        ("^R/↑/↓", "matches") => ("History", "Select an older / newer match"),
        ("↑/↓", "history") => ("History", "Previous / next prompt"),
        ("^R", "history search") => ("History", "Search previous prompts"),
        ("Esc/^G", "cancel") => ("History", "Cancel history search"),
        _ => ("Actions", hint.action),
    };
    let action = UiAction::new(hint.keys, hint.keys, hint.action)
        .with_group(group)
        .with_description(label)
        .with_priority(if index == 0 || hint.keys == "Esc" {
            ActionPriority::Essential
        } else {
            ActionPriority::Secondary
        });
    match hint.keys {
        "Enter" => action.with_compact_key("↵"),
        "hjkl/arrows" => action.with_trigger("h"),
        "motions" => action.with_trigger("l"),
        "g/G" => action.with_trigger("G"),
        _ => action,
    }
}

pub(crate) fn all_text_panel_shortcuts() -> Vec<crate::ui::ShortcutEntry> {
    let mut entries = Vec::new();
    for (mode, hints) in [
        ("NAV", PANEL_NAVIGATION_HINTS),
        ("SCROLLBACK NORMAL", SCROLLBACK_NORMAL_HINTS),
        ("SCROLLBACK VISUAL", SCROLLBACK_VISUAL_HINTS),
        ("NORMAL", COMPOSER_NORMAL_HINTS),
        ("VISUAL", COMPOSER_VISUAL_HINTS),
        ("SEARCH", COMPOSER_SEARCH_HINTS),
        ("REVERSE SEARCH", COMPOSER_HISTORY_SEARCH_HINTS),
        ("INSERT", COMPOSER_INSERT_HINTS),
    ] {
        let mut actions = hints
            .iter()
            .enumerate()
            .map(|(index, hint)| text_panel_hint_action(mode, hint, index))
            .collect::<Vec<_>>();
        actions.extend(crate::ui::surface_reference_actions(mode));
        let mut mode_entries = crate::ui::ShortcutEntry::from_actions("Text panel", &actions);
        for entry in &mut mode_entries {
            entry.group = format!("Text panel · {mode} · {}", entry.group);
        }
        entries.extend(mode_entries);
    }
    entries
}

fn text_panel_actions(
    composer: &TextPanelComposer,
    scrollback: &TextPanelScrollback,
    overflow: TextPanelOverflow,
) -> (&'static str, Vec<UiAction>) {
    let (mode, hints) = text_panel_composer_hints(composer, scrollback);
    let mut actions = hints
        .iter()
        .enumerate()
        .map(|(index, hint)| text_panel_hint_action(mode, hint, index))
        .collect::<Vec<_>>();
    if !scrollback.focused {
        let hint = match overflow {
            TextPanelOverflow::Both => Some(("↑↓", "more")),
            TextPanelOverflow::Below => Some(("↓", "more")),
            TextPanelOverflow::Above => Some(("↑", "history")),
            TextPanelOverflow::None => None,
        };
        if let Some((key, label)) = hint {
            actions.push(
                UiAction::new("history-overflow", key, label)
                    .with_priority(ActionPriority::Secondary),
            );
        }
    }
    if composer.focused && !scrollback.focused {
        actions.extend(crate::ui::prompt_reference_actions(composer.prompt.mode()));
    } else {
        actions.extend(crate::ui::surface_reference_actions(mode));
    }
    (mode, actions)
}

#[allow(clippy::too_many_arguments)]
fn render_text_panel_composer_footer(
    buffer: &mut RenderBuffer,
    composer: &TextPanelComposer,
    scrollback: &TextPanelScrollback,
    position: Point,
    y: usize,
    width: usize,
    overflow: TextPanelOverflow,
    palette: &TextPanelPalette,
    theme: &Theme,
) {
    let (mode, actions) = text_panel_actions(composer, scrollback, overflow);
    let history_search_status = composer.prompt.history_search_query().map(|query| {
        if let Some((position, total)) = composer.prompt.history_search_match_position() {
            format!("{position}/{total} matches")
        } else if query.is_empty() {
            "type to search".to_string()
        } else {
            "no match".to_string()
        }
    });
    ActionBar::new(&actions)
        .with_context(mode)
        .with_status(
            composer
                .validation
                .or(history_search_status.as_deref())
                .or(composer.status.as_deref()),
        )
        .render(buffer, position.x, y, width, theme, &palette.surface);
}

#[allow(clippy::too_many_arguments)]
fn render_text_panel_composer(
    buffer: &mut RenderBuffer,
    composer: &TextPanelComposer,
    scrollback: &TextPanelScrollback,
    position: Point,
    width: usize,
    top: usize,
    rows: usize,
    overflow: TextPanelOverflow,
    palette: &TextPanelPalette,
    theme: &Theme,
) {
    if width == 0 {
        return;
    }
    let top = position.y.saturating_add(top);

    let content_width = TextPanelComposer::layout_options(width).width;
    let wrapped = composer.layout(width);
    let cursor_row = wrapped
        .position(composer.prompt.display_cursor())
        .map_or(0, |position| position.row);
    let first = cursor_row.saturating_sub(rows.saturating_sub(1));
    let active_row = cursor_row.saturating_sub(first).min(rows.saturating_sub(1));
    for row in 0..rows {
        let y = top + 1 + row;
        let line = wrapped.rows().get(first + row).map(|row| row.text.as_str());
        let placeholder = line.is_some_and(str::is_empty)
            && composer.prompt.display_text().is_empty()
            && row == 0;
        let text = if placeholder {
            composer.config.placeholder.as_str()
        } else {
            line.unwrap_or("")
        };
        let text_style = if placeholder || !composer.enabled {
            &palette.muted
        } else {
            &palette.primary
        };
        let (marker, marker_style) = if composer.focused && row == active_row {
            ("›", &palette.accent)
        } else if placeholder {
            ("›", &palette.muted)
        } else if line.is_some() {
            ("│", &palette.divider)
        } else {
            (" ", &palette.surface)
        };
        buffer.set_text(position.x, y, marker, marker_style);
        buffer.set_text(
            position.x + 2,
            y,
            &fit_display_width(text, content_width),
            text_style,
        );
    }
    render_text_panel_composer_footer(
        buffer,
        composer,
        scrollback,
        position,
        top + rows + 1,
        width,
        overflow,
        palette,
        theme,
    );
}

fn render_text_panel_status(
    buffer: &mut RenderBuffer,
    panel: &TextPanel,
    status: &TextPanelStatus,
    position: Point,
    width: usize,
    y: usize,
    palette: &TextPanelPalette,
) {
    if width == 0 {
        return;
    }
    let y = position.y.saturating_add(y);
    let mut used = 0usize;
    if status.busy {
        let elapsed_ms = panel
            .busy_since
            .map_or(0, |since| since.elapsed().as_millis() as u64);
        append_text_panel_piece(
            buffer,
            position,
            y,
            width,
            &mut used,
            spinner_frame(elapsed_ms),
            &palette.accent,
        );
        append_text_panel_piece(buffer, position, y, width, &mut used, " ", &palette.surface);
        append_text_panel_piece(
            buffer,
            position,
            y,
            width,
            &mut used,
            &status.label,
            &palette.primary,
        );
        append_text_panel_piece(
            buffer,
            position,
            y,
            width,
            &mut used,
            " · ",
            &palette.divider,
        );
        append_text_panel_piece(
            buffer,
            position,
            y,
            width,
            &mut used,
            &format_elapsed(elapsed_ms / 1000),
            &palette.muted,
        );
    } else {
        append_text_panel_piece(
            buffer,
            position,
            y,
            width,
            &mut used,
            &status.label,
            &palette.secondary,
        );
    }
}

fn text_panel_header_actions(
    config: &PanelConfig,
    width: usize,
) -> Vec<(usize, &TextPanelHeaderAction, String)> {
    text_panel_header_actions_with_title_width(
        config,
        width,
        config.title.as_deref().map_or(0, display_width).min(5),
    )
}

fn text_panel_header_actions_for_panel(
    panel: &TextPanel,
    width: usize,
) -> Vec<(usize, &TextPanelHeaderAction, String)> {
    let Some(detail) = &panel.header_detail else {
        return text_panel_header_actions(&panel.config, width);
    };
    let title_width = panel.config.title.as_deref().map_or(0, display_width)
        + 3
        + display_width(&detail.text)
        + if detail.secondary.is_empty() {
            0
        } else {
            3 + display_width(&detail.secondary)
        };
    text_panel_header_actions_with_title_width(&panel.config, width, title_width)
}

fn text_panel_header_actions_with_title_width(
    config: &PanelConfig,
    width: usize,
    title_width: usize,
) -> Vec<(usize, &TextPanelHeaderAction, String)> {
    let visible = config
        .header_actions
        .iter()
        .filter(|action| action.show_in_header)
        .collect::<Vec<_>>();
    let full_width = visible
        .iter()
        .map(|action| display_width(&text_panel_header_action_label(action)).saturating_add(2))
        .sum::<usize>()
        .saturating_add(visible.len().saturating_sub(1));
    let compact = full_width.saturating_add(title_width).saturating_add(1) > width;
    let mut labels = visible
        .into_iter()
        .map(|action| {
            let label = if compact {
                action
                    .compact_label
                    .as_deref()
                    .map(str::to_owned)
                    .unwrap_or_else(|| text_panel_header_action_label(action))
            } else {
                text_panel_header_action_label(action)
            };
            (action, label)
        })
        .collect::<Vec<_>>();
    let mut used = labels
        .iter()
        .map(|(_, label)| display_width(label).saturating_add(2))
        .sum::<usize>()
        .saturating_add(labels.len().saturating_sub(1));
    while used > width && !labels.is_empty() {
        let remove = labels
            .iter()
            .enumerate()
            .max_by_key(|(index, (action, _))| (action.priority, std::cmp::Reverse(*index)))
            .map_or(0, |(index, _)| index);
        let (_, label) = labels.remove(remove);
        used = used.saturating_sub(display_width(&label).saturating_add(2));
        if !labels.is_empty() {
            used = used.saturating_sub(1);
        }
    }
    let mut start = width.saturating_sub(used);
    labels
        .into_iter()
        .map(|(action, label)| {
            let current = start;
            start = start.saturating_add(display_width(&label).saturating_add(3));
            (current, action, label)
        })
        .collect()
}

/// Returns exactly the metadata cells painted in the header, also used for hit testing.
fn text_panel_header_detail_layout(panel: &TextPanel, width: usize) -> Option<(usize, String)> {
    let detail = panel.header_detail.as_ref()?;
    let title = panel.config.title.as_deref().unwrap_or_default();
    let end = text_panel_header_actions_for_panel(panel, width)
        .first()
        .map_or(width, |(start, _, _)| start.saturating_sub(1));
    let start = display_width(title);
    let available = end.saturating_sub(start);
    if available < 5 || detail.text.is_empty() {
        return None;
    }
    let text_width = available - 3;
    let full = if detail.secondary.is_empty() {
        detail.text.clone()
    } else {
        format!("{} · {}", detail.text, detail.secondary)
    };
    let text = if display_width(&full) <= text_width {
        full
    } else {
        crate::unicode_utils::truncate_display_width_with_marker(
            if detail.compact_text.is_empty() {
                &detail.text
            } else {
                &detail.compact_text
            },
            text_width,
            "…",
            crate::unicode_utils::TruncationSide::Right,
        )
    };
    Some((start, format!(" · {text}")))
}

fn text_panel_header_action_at(panel: &TextPanel, width: usize, x: usize) -> Option<&str> {
    text_panel_header_actions_for_panel(panel, width)
        .into_iter()
        .find(|(start, _, label)| {
            x >= *start && x < start.saturating_add(display_width(label).saturating_add(2))
        })
        .filter(|(_, action, _)| action.enabled)
        .map(|(_, action, _)| action.id.as_str())
}

fn text_panel_header_action_label(action: &TextPanelHeaderAction) -> String {
    if action.key.chars().count() == 1 && display_width(&action.label) > 1 {
        format!("{} {}", action.key, action.label)
    } else {
        action.label.clone()
    }
}

#[allow(clippy::too_many_arguments)]
fn render_text_spans(
    buffer: &mut RenderBuffer,
    x: usize,
    y: usize,
    width: usize,
    line: &RenderedTextLine,
    line_first: usize,
    selection: Option<(usize, usize)>,
    selected_link: Option<u64>,
    search: Option<&TextPanelSearchHighlights>,
    theme: &Theme,
    palette: &TextPanelPalette,
) {
    let mut column = 0usize;
    let mut offset = line_first;
    for span in &line.spans {
        let base_style = text_panel_span_style(span.style, theme, palette);
        let mut style = if let Some(syntax_style) = &span.syntax_style {
            Style {
                fg: syntax_style.fg.or(base_style.fg),
                bg: syntax_style.bg.or(base_style.bg).or(palette.surface.bg),
                bold: syntax_style.bold || base_style.bold,
                italic: syntax_style.italic || base_style.italic,
                underline: syntax_style.underline || base_style.underline,
            }
        } else {
            base_style
        };
        if span
            .link
            .as_ref()
            .is_some_and(|link| Some(link.id) == selected_link)
        {
            let selection = theme.list_selection_style();
            style = theme.selected_style(&style, &selection, SelectionForegroundPriority::Content);
        }
        for grapheme in span.text.graphemes(true) {
            let grapheme_width = display_width(grapheme).max(1);
            if column.saturating_add(grapheme_width) > width {
                return;
            }
            let mut grapheme_style = style.clone();
            let selectable = matches!(span.selection, TextPanelSpanSelection::Content);
            if selectable {
                if let Some(search_style) = search.and_then(|search| search.style_at(offset)) {
                    grapheme_style = theme.selected_style(
                        &grapheme_style,
                        search_style,
                        SelectionForegroundPriority::Content,
                    );
                }
            }
            if selectable && selection.is_some_and(|(start, end)| offset >= start && offset <= end)
            {
                let selected = theme.list_selection_style();
                grapheme_style = theme.selected_style(
                    &grapheme_style,
                    &selected,
                    SelectionForegroundPriority::Content,
                );
            }
            buffer.set_text(x.saturating_add(column), y, grapheme, &grapheme_style);
            column = column.saturating_add(grapheme_width);
            if selectable {
                offset = offset.saturating_add(1);
            }
        }
    }
}

fn text_panel_span_style(
    style: TextPanelSpanStyle,
    theme: &Theme,
    palette: &TextPanelPalette,
) -> Style {
    let scoped = |scope: &str, fallback: &Style, preserve_background: bool| {
        let scoped = theme.get_style(scope).unwrap_or_default();
        Style {
            fg: scoped.fg.or(fallback.fg),
            bg: if preserve_background {
                scoped.bg.or(palette.surface.bg)
            } else {
                palette.surface.bg
            },
            bold: scoped.bold || fallback.bold,
            italic: scoped.italic || fallback.italic,
            underline: scoped.underline || fallback.underline,
        }
    };
    match style {
        TextPanelSpanStyle::User => palette.accent.clone(),
        TextPanelSpanStyle::Agent | TextPanelSpanStyle::Text => palette.primary.clone(),
        TextPanelSpanStyle::Error => palette.error.clone(),
        TextPanelSpanStyle::Heading => {
            let mut style = scoped("heading.1.markdown", &palette.primary, false);
            style.bold = true;
            style
        }
        TextPanelSpanStyle::Strong => Style {
            bold: true,
            ..palette.primary.clone()
        },
        TextPanelSpanStyle::Emphasis => Style {
            italic: true,
            ..palette.primary.clone()
        },
        TextPanelSpanStyle::Strikethrough => {
            scoped("markup.strikethrough.markdown", &palette.secondary, false)
        }
        TextPanelSpanStyle::InlineCode | TextPanelSpanStyle::Code | TextPanelSpanStyle::Diff => {
            scoped("markup.raw.block.markdown", &palette.primary, true)
        }
        TextPanelSpanStyle::Link => Style {
            underline: true,
            ..scoped("markup.underline.link.markdown", &palette.accent, false)
        },
        TextPanelSpanStyle::Quote => palette.secondary.clone(),
        TextPanelSpanStyle::Muted => palette.muted.clone(),
    }
}

fn render_panel_separator(
    buffer: &mut RenderBuffer,
    position: Point,
    width: usize,
    height: usize,
    side: PanelSide,
    style: &Style,
    separator: &str,
) {
    match side {
        PanelSide::Left | PanelSide::Right => {
            let separator_x = if side == PanelSide::Left {
                position.x.checked_add(width)
            } else {
                position.x.checked_sub(1)
            };
            let Some(separator_x) = separator_x.filter(|x| *x < buffer.width) else {
                return;
            };
            for y in 0..height {
                buffer.set_text(separator_x, position.y.saturating_add(y), separator, style);
            }
        }
        PanelSide::Top | PanelSide::Bottom => {
            let separator_y = if side == PanelSide::Top {
                position.y.checked_add(height)
            } else {
                position.y.checked_sub(1)
            };
            let Some(separator_y) = separator_y.filter(|y| *y < buffer.height) else {
                return;
            };
            buffer.set_text(position.x, separator_y, &separator.repeat(width), style);
        }
    }
}

fn block_label(kind: &TextPanelBlockKind) -> Option<(&'static str, TextPanelSpanStyle)> {
    match kind {
        // User blocks render their own prompt card and label.
        TextPanelBlockKind::User | TextPanelBlockKind::Queued => None,
        TextPanelBlockKind::Agent => None,
        TextPanelBlockKind::Error => Some(("⚠ Error", TextPanelSpanStyle::Error)),
        TextPanelBlockKind::Activity | TextPanelBlockKind::Action | TextPanelBlockKind::Text => {
            None
        }
    }
}

fn block_style(kind: &TextPanelBlockKind) -> TextPanelSpanStyle {
    match kind {
        TextPanelBlockKind::User | TextPanelBlockKind::Queued => TextPanelSpanStyle::User,
        TextPanelBlockKind::Agent => TextPanelSpanStyle::Agent,
        TextPanelBlockKind::Error => TextPanelSpanStyle::Error,
        TextPanelBlockKind::Activity | TextPanelBlockKind::Action => TextPanelSpanStyle::Muted,
        TextPanelBlockKind::Text => TextPanelSpanStyle::Text,
    }
}

fn render_prompt_rails(
    buffer: &mut RenderBuffer,
    position: Point,
    width: usize,
    glyph: &str,
    style: &Style,
) {
    buffer.set_text(position.x, position.y, glyph, style);
    buffer.set_text(
        position.x + width.saturating_sub(1),
        position.y,
        glyph,
        style,
    );
}

fn user_padded(line: RenderedTextLine) -> RenderedTextLine {
    let break_after = line.break_after;
    let selection = line.selection;
    let mut spans = vec![RenderedTextSpan {
        text: "  ".to_string(),
        style: TextPanelSpanStyle::User,
        syntax_style: None,
        link: None,
        selection: TextPanelSpanSelection::Chrome,
    }];
    spans.extend(line.spans);
    RenderedTextLine {
        spans,
        break_after,
        selection,
    }
}

fn render_row_segments(
    buffer: &mut RenderBuffer,
    position: Point,
    width: usize,
    row: &PanelRow,
    theme: &Theme,
    surface_style: &Style,
    selected: bool,
) {
    let requested_right_width = segments_width(&row.right_segments);
    let right_inset = usize::from(requested_right_width > 0 && requested_right_width < width);
    let content_width = width.saturating_sub(right_inset);
    let right_width = requested_right_width.min(content_width);
    let gap = usize::from(right_width > 0 && right_width < content_width);
    let left_width = content_width
        .saturating_sub(right_width)
        .saturating_sub(gap);

    render_segments(
        buffer,
        position,
        left_width,
        &row.segments,
        theme,
        surface_style,
        selected,
    );

    if right_width > 0 {
        let right_x = position.x + content_width.saturating_sub(right_width);
        render_segments(
            buffer,
            Point::new(right_x, position.y),
            right_width,
            &row.right_segments,
            theme,
            surface_style,
            selected,
        );
    }
}

fn render_segments(
    buffer: &mut RenderBuffer,
    position: Point,
    max_width: usize,
    segments: &[PanelSegment],
    theme: &Theme,
    surface_style: &Style,
    selected: bool,
) {
    let mut used = 0;

    for segment in segments {
        if used >= max_width {
            break;
        }

        let remaining = max_width - used;
        let text = truncate_display_width(&segment.text, remaining);
        if text.is_empty() {
            continue;
        }

        let style = segment_style(segment, theme, surface_style, selected);
        buffer.set_text(position.x + used, position.y, &text, &style);
        used += display_width(&text);
    }
}

fn segment_style(
    segment: &PanelSegment,
    theme: &Theme,
    surface_style: &Style,
    selected: bool,
) -> Style {
    let mut style = surface_style.clone();
    if let Some(semantic) = &segment.semantic {
        let resolved = theme.resolve_style(semantic);
        style.fg = resolved.fg.or(style.fg);
        style.bg = resolved.bg.or(style.bg);
        style.bold |= resolved.bold;
        style.italic |= resolved.italic;
        style.underline |= resolved.underline;
    }
    if let Some(concrete) = &segment.style {
        style.fg = concrete.fg.or(style.fg);
        style.bg = concrete.bg.or(style.bg);
        style.bold = concrete.bold;
        style.italic = concrete.italic;
        style.underline = concrete.underline;
    }
    if selected {
        let selection_style = theme.list_selection_style();
        style = theme.selected_style(
            &style,
            &selection_style,
            SelectionForegroundPriority::Content,
        );
    }
    style
}

fn panel_style(theme: &Theme, semantic: Option<&ThemeStyleSpec>) -> Style {
    let mut style = theme.style.clone();
    if let Some(semantic) = semantic {
        let resolved = theme.resolve_style(semantic);
        style.fg = resolved.fg.or(style.fg);
        style.bg = resolved.bg.or(style.bg);
        style.bold |= resolved.bold;
        style.italic |= resolved.italic;
        style.underline |= resolved.underline;
    }
    style
}

fn segments_width(segments: &[PanelSegment]) -> usize {
    segments
        .iter()
        .map(|segment| display_width(&segment.text))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        color::{contrast_ratio, Color},
        theme::parse_vscode_theme,
    };

    #[test]
    fn inline_handoff_draft_requires_confirmation_and_is_undoable() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".into(),
            PanelConfig {
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".into(),
                    rows: 3,
                }),
                ..PanelConfig::default()
            },
        );
        assert!(matches!(
            manager.load_text_panel_draft("agent", "my unsent draft", None),
            Ok(TextPanelReuseOutcome::Loaded)
        ));
        let Ok(TextPanelReuseOutcome::Confirm(revision)) =
            manager.load_text_panel_draft("agent", "inline discussion", None)
        else {
            panic!("nonempty draft must require confirmation");
        };
        assert_eq!(
            manager.text_panels["agent"]
                .composer
                .as_ref()
                .unwrap()
                .prompt
                .text(),
            "my unsent draft"
        );
        manager
            .text_panels
            .get_mut("agent")
            .unwrap()
            .composer
            .as_mut()
            .unwrap()
            .prompt
            .insert("!");
        assert!(manager
            .load_text_panel_draft("agent", "inline discussion", Some(revision))
            .is_err());
        let Ok(TextPanelReuseOutcome::Confirm(revision)) =
            manager.load_text_panel_draft("agent", "inline discussion", None)
        else {
            panic!("confirmation required");
        };
        assert!(matches!(
            manager.load_text_panel_draft("agent", "inline discussion", Some(revision)),
            Ok(TextPanelReuseOutcome::Loaded)
        ));
        let composer = manager
            .text_panels
            .get_mut("agent")
            .unwrap()
            .composer
            .as_mut()
            .unwrap();
        assert_eq!(composer.prompt.text(), "inline discussion");
        assert!(composer.prompt.undo());
        assert_eq!(composer.prompt.text(), "my unsent draft!");
    }

    fn row(id: &str) -> PanelRow {
        PanelRow {
            id: id.to_string(),
            path: None,
            expanded: None,
            kind: PanelRowKind::File,
            segments: vec![PanelSegment {
                text: id.to_string(),
                style: None,
                semantic: None,
            }],
            right_segments: Vec::new(),
        }
    }

    fn row_text(buffer: &RenderBuffer, y: usize) -> String {
        (0..buffer.width)
            .map(|x| buffer.cells[y * buffer.width + x].text.as_str())
            .collect()
    }

    fn text_position(buffer: &RenderBuffer, needle: &str) -> Option<Point> {
        for y in 0..buffer.height {
            for x in 0..buffer.width {
                let mut candidate = String::new();
                for cell in &buffer.cells[y * buffer.width + x..(y + 1) * buffer.width] {
                    candidate.push_str(&cell.text);
                    if candidate == needle {
                        return Some(Point::new(x, y));
                    }
                    if !needle.starts_with(&candidate) {
                        break;
                    }
                }
            }
        }
        None
    }

    fn text_style<'a>(buffer: &'a RenderBuffer, needle: &str) -> &'a Style {
        let position = text_position(buffer, needle)
            .unwrap_or_else(|| panic!("missing rendered text {needle:?}"));
        &buffer.cells[position.y * buffer.width + position.x].style
    }

    #[test]
    fn pane_zoom_reflows_scrollback_without_losing_its_source_cursor() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "text".to_string(),
            PanelConfig {
                width: 18,
                ..PanelConfig::default()
            },
        );
        manager.update_text_panel(
            "text",
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Text,
                format: TextPanelBlockFormat::Plain,
                text: "word ".repeat(180),
            }],
            22,
            100,
        );
        assert!(manager.focus_panel("text"));
        let panel = manager.text_panels.get_mut("text").unwrap();
        let cursor = panel.layout(18).offset_at(15, 0).unwrap();
        panel.scrollback.cursor = cursor;
        panel.scrollback.selection_anchor = Some(cursor.saturating_sub(2));
        panel.scrollback.mode = TextPanelScrollbackMode::Visual;
        panel.scroll = 12;
        panel.follow_tail = false;
        panel.viewport.restore(12, false);

        manager.set_presentation(PanelPresentation::Zoomed {
            id: "text".to_string(),
            size: (100, 22),
        });
        assert!(manager
            .focused_text_panel_cursor_position(100, 24)
            .is_some());
        manager.append_text_panel("text", "answer", "more words", 22, 100);
        manager.set_presentation(PanelPresentation::Docked);
        let panel = &manager.text_panels["text"];
        assert_eq!(panel.scrollback.cursor, cursor);
        assert_eq!(
            panel.scrollback.selection_anchor,
            Some(cursor.saturating_sub(2))
        );
        assert!(!panel.follow_tail);
        assert!(manager
            .focused_text_panel_cursor_position(100, 24)
            .is_some());
    }

    #[test]
    fn pane_zoom_preserves_docking_and_uses_one_fullscreen_placement() {
        for side in [
            PanelSide::Left,
            PanelSide::Right,
            PanelSide::Top,
            PanelSide::Bottom,
        ] {
            for text in [false, true] {
                let mut manager = PanelManager::default();
                manager.create_panel("other".to_string(), PanelConfig::default());
                let config = PanelConfig {
                    side,
                    width: 12,
                    ..PanelConfig::default()
                };
                if text {
                    manager.create_text_panel("target".to_string(), config);
                } else {
                    manager.create_panel("target".to_string(), config);
                }
                assert!(manager.focus_panel("target"));
                let before = manager.snapshot(80);
                let placements = manager.panel_placements(80, 24);
                manager.set_presentation(PanelPresentation::Zoomed {
                    id: "target".to_string(),
                    size: (80, 22),
                });
                assert_eq!(
                    manager.panel_placements(80, 24),
                    vec![PanelPlacement {
                        id: "target".to_string(),
                        x: 0,
                        y: 0,
                        width: 80,
                        height: 22,
                    }]
                );
                assert_eq!(
                    manager.panel_at_position(79, 21, 80, 24).unwrap().id,
                    "target"
                );
                assert!(manager.panel_divider_at_position(12, 4, 80, 24).is_none());
                let config = manager.panel_config("target").unwrap();
                assert_eq!(manager.text_panel_width("target", 80), 80);
                assert_eq!(manager.presentation.height("target", config, 22), 22);
                assert_eq!(manager.snapshot(80), before);
                manager.set_presentation(PanelPresentation::Hidden);
                assert!(manager.panel_at_position(0, 0, 80, 24).is_none());
                manager.set_presentation(PanelPresentation::Docked);
                assert_eq!(manager.panel_placements(80, 24), placements);
                assert_eq!(manager.snapshot(80), before);
            }
        }
    }

    #[test]
    fn panel_configuration_round_trips_all_four_docking_edges() {
        for (name, side) in [
            ("left", PanelSide::Left),
            ("right", PanelSide::Right),
            ("top", PanelSide::Top),
            ("bottom", PanelSide::Bottom),
        ] {
            let config: PanelConfig = serde_json::from_value(serde_json::json!({
                "side": name,
                "width": 12,
            }))
            .expect("all four pane edges should be valid plugin configuration");

            assert_eq!(config.side, side);
            assert_eq!(serde_json::to_value(&config).unwrap()["side"], name);
        }
    }

    #[test]
    fn pane_snapshot_restores_deferred_visibility_focus_selection_and_draft() {
        let mut original = PanelManager::default();
        original.create_panel(
            "tree".to_string(),
            PanelConfig {
                side: PanelSide::Left,
                width: 24,
                ..PanelConfig::default()
            },
        );
        original.update_panel("tree", vec![row("src"), row("tests")]);
        assert!(original.select_row_by_id("tree", "tests", 20));
        assert!(original.set_panel_visible("tree", false));

        let agent_config = PanelConfig {
            side: PanelSide::Right,
            width: 42,
            composer: Some(TextPanelComposerConfig {
                placeholder: "Ask".to_string(),
                rows: 3,
            }),
            ..PanelConfig::default()
        };
        original.create_text_panel("agent".to_string(), agent_config.clone());
        original.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Markdown,
                text: "restored answer".to_string(),
            }],
            20,
            100,
        );
        let composer = original
            .text_panels
            .get_mut("agent")
            .unwrap()
            .composer
            .as_mut()
            .unwrap();
        assert!(composer.prompt.set_text("keep this draft"));
        composer.prompt.set_cursor(5);
        assert!(original.focus_text_panel_composer("agent"));

        let encoded = serde_json::to_vec(&original.snapshot(100)).unwrap();
        let snapshot: PanelManagerSnapshot = serde_json::from_slice(&encoded).unwrap();
        let mut restored = PanelManager::default();
        restored.stage_restore(snapshot);

        restored.create_text_panel("agent".to_string(), agent_config);
        assert!(restored.apply_pending_shell_restore("agent"));
        restored.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Markdown,
                text: "restored answer".to_string(),
            }],
            20,
            100,
        );
        assert!(restored.apply_pending_content_restore("agent", 20, 100));

        restored.create_panel(
            "tree".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 10,
                ..PanelConfig::default()
            },
        );
        assert!(restored.apply_pending_shell_restore("tree"));
        restored.update_panel("tree", vec![row("src")]);
        assert!(!restored.apply_pending_content_restore("tree", 20, 100));
        restored.update_panel("tree", vec![row("src"), row("tests")]);
        assert!(restored.apply_pending_content_restore("tree", 20, 100));

        assert_eq!(restored.focused.as_deref(), Some("agent"));
        assert!(!restored.z_order.iter().any(|id| id == "tree"));
        assert_eq!(restored.panels["tree"].selected_row().unwrap().id, "tests");
        assert_eq!(restored.panel_layout("tree"), Some((PanelSide::Left, 24)));
        let composer = restored.text_panels["agent"].composer.as_ref().unwrap();
        assert_eq!(composer.prompt.text(), "keep this draft");
        assert_eq!(composer.prompt.cursor(), 5);
        assert!(composer.focused);
    }

    #[test]
    fn left_panels_reserve_width_with_separator() {
        let mut manager = PanelManager::default();
        manager.create_panel(
            "tree".to_string(),
            PanelConfig {
                side: PanelSide::Left,
                width: 24,
                title: None,
                composer: None,
                surface: None,
                border: None,
                header_actions: Vec::new(),
            },
        );

        assert_eq!(manager.reserved_left_width(200), 25);
    }

    #[test]
    fn right_panels_reserve_width_with_separator() {
        let mut manager = PanelManager::default();
        manager.create_panel(
            "tree".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 24,
                title: None,
                composer: None,
                surface: None,
                border: None,
                header_actions: Vec::new(),
            },
        );

        assert_eq!(manager.reserved_right_width(200), 25);
    }

    #[test]
    fn top_text_panel_uses_full_terminal_width_and_a_horizontal_separator() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "inspector".to_string(),
            PanelConfig {
                side: PanelSide::Top,
                width: 4,
                title: Some("Inspector".to_string()),
                ..PanelConfig::default()
            },
        );
        manager.update_text_panel(
            "inspector",
            vec![TextPanelBlock {
                id: "details".to_string(),
                kind: TextPanelBlockKind::Text,
                format: TextPanelBlockFormat::Plain,
                text: "source-backed details".to_string(),
            }],
            /*panel_height*/ 10,
            /*terminal_width*/ 32,
        );

        assert_eq!(manager.reserved_top_height(), 5);
        assert_eq!(
            manager.panel_at_position(/*x*/ 31, /*y*/ 0, /*width*/ 32, /*height*/ 12),
            Some(PanelPlacement {
                id: "inspector".to_string(),
                x: 0,
                y: 0,
                width: 32,
                height: 4,
            }),
        );

        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(/*width*/ 32, /*height*/ 12, &theme.style);
        manager.render(&mut buffer, &theme);

        assert!(row_text(&buffer, 0).contains("Inspector"));
        assert!((1..4).any(|y| row_text(&buffer, y).contains("source-backed details")));
        assert_eq!(row_text(&buffer, 4), "─".repeat(32));
    }

    #[test]
    fn bottom_text_panel_renders_at_its_actual_vertical_origin() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "inspector".to_string(),
            PanelConfig {
                side: PanelSide::Bottom,
                width: 4,
                title: Some("Inspector".to_string()),
                ..PanelConfig::default()
            },
        );
        manager.update_text_panel(
            "inspector",
            vec![TextPanelBlock {
                id: "details".to_string(),
                kind: TextPanelBlockKind::Text,
                format: TextPanelBlockFormat::Plain,
                text: "bottom details".to_string(),
            }],
            /*panel_height*/ 10,
            /*terminal_width*/ 32,
        );

        assert_eq!(manager.reserved_bottom_height(), 5);
        assert_eq!(
            manager.panel_at_position(/*x*/ 31, /*y*/ 6, /*width*/ 32, /*height*/ 12),
            Some(PanelPlacement {
                id: "inspector".to_string(),
                x: 0,
                y: 6,
                width: 32,
                height: 4,
            }),
        );

        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(/*width*/ 32, /*height*/ 12, &theme.style);
        manager.render(&mut buffer, &theme);

        assert_eq!(row_text(&buffer, 5), "─".repeat(32));
        assert!(row_text(&buffer, 6).contains("Inspector"));
        assert!((7..10).any(|y| row_text(&buffer, y).contains("bottom details")));
    }

    #[test]
    fn four_docking_edges_share_terminal_space_without_overlapping() {
        let mut manager = PanelManager::default();
        for (id, side, width) in [
            ("top", PanelSide::Top, 4),
            ("left", PanelSide::Left, 12),
            ("right", PanelSide::Right, 12),
            ("bottom", PanelSide::Bottom, 4),
        ] {
            manager.create_panel(
                id.to_string(),
                PanelConfig {
                    side,
                    width,
                    ..PanelConfig::default()
                },
            );
        }

        assert_eq!(manager.reserved_top_height(), 5);
        assert_eq!(manager.reserved_bottom_height(), 5);
        assert_eq!(manager.reserved_left_width(200), 13);
        assert_eq!(manager.reserved_right_width(200), 13);
        assert_eq!(
            manager.panel_placements(/*terminal_width*/ 80, /*terminal_height*/ 24),
            vec![
                PanelPlacement {
                    id: "top".to_string(),
                    x: 0,
                    y: 0,
                    width: 80,
                    height: 4,
                },
                PanelPlacement {
                    id: "left".to_string(),
                    x: 0,
                    y: 5,
                    width: 12,
                    height: 12,
                },
                PanelPlacement {
                    id: "right".to_string(),
                    x: 68,
                    y: 5,
                    width: 12,
                    height: 12,
                },
                PanelPlacement {
                    id: "bottom".to_string(),
                    x: 0,
                    y: 18,
                    width: 80,
                    height: 4,
                },
            ],
        );
    }

    #[test]
    fn panel_divider_hit_testing_covers_all_four_docking_edges() {
        for (side, divider_x, divider_y) in [
            (PanelSide::Left, 6, 3),
            (PanelSide::Right, 33, 3),
            (PanelSide::Top, 8, 6),
            (PanelSide::Bottom, 8, 15),
        ] {
            let mut manager = PanelManager::default();
            manager.create_text_panel(
                "inspector".to_string(),
                PanelConfig {
                    side,
                    width: 6,
                    ..PanelConfig::default()
                },
            );

            let divider = manager
                .panel_divider_at_position(
                    divider_x, divider_y, /*terminal_width*/ 40, /*terminal_height*/ 24,
                )
                .expect("the actual pane boundary should be draggable");

            assert_eq!(divider.id, "inspector");
            assert_eq!(divider.side, side);
            assert!(manager
                .panel_divider_at_position(
                    /*x*/ 39, /*y*/ 23, /*terminal_width*/ 40,
                    /*terminal_height*/ 24,
                )
                .is_none());
        }
    }

    #[test]
    fn active_row_panel_dividers_appear_on_all_edges_and_restore_on_release() {
        let accent = Color::Rgb {
            r: 203,
            g: 166,
            b: 247,
        };

        for use_ascii in [false, true] {
            for (side, divider_x, divider_y) in [
                (PanelSide::Left, 6, 3),
                (PanelSide::Right, 33, 3),
                (PanelSide::Top, 8, 6),
                (PanelSide::Bottom, 8, 15),
            ] {
                let mut theme = Theme::default();
                theme.colors.insert("sash.hoverBorder".to_string(), accent);
                let mut manager = PanelManager::default();
                manager.create_panel(
                    "inspector".to_string(),
                    PanelConfig {
                        side,
                        width: 6,
                        ..PanelConfig::default()
                    },
                );
                let mut buffer =
                    RenderBuffer::new(/*width*/ 40, /*height*/ 24, &theme.style);
                let index = divider_y * buffer.width + divider_x;

                manager.render_with_active_dividers(&mut buffer, &theme, &[], use_ascii);
                let inactive = buffer.cells[index].clone();
                assert_eq!(inactive.c, ' ');

                manager.render_with_active_dividers(&mut buffer, &theme, &["inspector"], use_ascii);
                let active = &buffer.cells[index];
                let expected = match (side, use_ascii) {
                    (PanelSide::Left | PanelSide::Right, false) => '│',
                    (PanelSide::Left | PanelSide::Right, true) => '|',
                    (PanelSide::Top | PanelSide::Bottom, false) => '─',
                    (PanelSide::Top | PanelSide::Bottom, true) => '-',
                };

                assert_eq!(active.c, expected, "{side:?}, ASCII={use_ascii}");
                assert_eq!(active.style.fg, Some(accent));
                assert_eq!(active.style.bg, inactive.style.bg);
                assert!(active.style.bold);

                manager.render_with_active_dividers(&mut buffer, &theme, &[], use_ascii);
                assert_eq!(buffer.cells[index], inactive);
            }
        }
    }

    #[test]
    fn active_text_panel_divider_does_not_highlight_another_pane() {
        let accent = Color::Rgb {
            r: 203,
            g: 166,
            b: 247,
        };
        let mut theme = Theme::default();
        theme.colors.insert("sash.hoverBorder".to_string(), accent);
        let mut manager = PanelManager::default();
        for (id, side) in [("left", PanelSide::Left), ("right", PanelSide::Right)] {
            manager.create_text_panel(
                id.to_string(),
                PanelConfig {
                    side,
                    width: 6,
                    ..PanelConfig::default()
                },
            );
        }
        let mut buffer = RenderBuffer::new(/*width*/ 40, /*height*/ 24, &theme.style);
        let left = /*row*/ 3 * buffer.width + /*column*/ 6;
        let right = /*row*/ 3 * buffer.width + /*column*/ 33;
        manager.render(&mut buffer, &theme);
        let inactive_right = buffer.cells[right].clone();

        manager.render_with_active_dividers(&mut buffer, &theme, &["left"], false);

        assert_eq!(buffer.cells[left].c, '│');
        assert_eq!(buffer.cells[left].style.fg, Some(accent));
        assert!(buffer.cells[left].style.bold);
        assert_eq!(buffer.cells[right], inactive_right);

        manager.render_with_active_dividers(&mut buffer, &theme, &["left", "right"], false);
        assert_eq!(buffer.cells[left].style.fg, Some(accent));
        assert_eq!(buffer.cells[right].style.fg, Some(accent));
        assert!(buffer.cells[left].style.bold);
        assert!(buffer.cells[right].style.bold);
    }

    #[test]
    fn text_panel_composer_reuses_modal_operators_objects_and_visual_undo() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "modal".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 36,
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Edit".to_string(),
                    rows: 3,
                }),
                ..PanelConfig::default()
            },
        );
        assert!(manager.focus_text_panel_composer("modal"));
        manager
            .handle_focused_text_input(&Event::Paste("first (second word) tail".to_string()), 80);
        manager.handle_focused_text_input(
            &Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            )),
            80,
        );

        for character in "0f(ci(".chars() {
            let event = manager
                .handle_focused_text_input(
                    &Event::Key(crossterm::event::KeyEvent::new(
                        KeyCode::Char(character),
                        KeyModifiers::NONE,
                    )),
                    80,
                )
                .expect("modal key is consumed by composer");
            assert_eq!(event.action, "composer_input");
        }
        let composer = manager.text_panels["modal"].composer.as_ref().unwrap();
        assert_eq!(composer.prompt.text(), "first () tail");
        assert_eq!(composer.prompt.mode(), crate::editor::Mode::Insert);

        for code in [KeyCode::Esc, KeyCode::Char('u')] {
            manager.handle_focused_text_input(
                &Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)),
                80,
            );
        }
        assert_eq!(
            manager.text_panels["modal"]
                .composer
                .as_ref()
                .unwrap()
                .prompt
                .text(),
            "first (second word) tail"
        );
    }

    #[test]
    fn moving_a_text_panel_preserves_focus_blocks_and_composer_draft() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "inspector".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 24,
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 2,
                }),
                ..PanelConfig::default()
            },
        );
        manager.update_text_panel(
            "inspector",
            vec![TextPanelBlock {
                id: "details".to_string(),
                kind: TextPanelBlockKind::Text,
                format: TextPanelBlockFormat::Plain,
                text: "keep this content".to_string(),
            }],
            /*panel_height*/ 20,
            /*terminal_width*/ 80,
        );
        assert!(manager.focus_text_panel_composer("inspector"));
        manager.handle_focused_text_input(&Event::Paste("keep this draft".to_string()), 80);

        for (side, size) in [
            (PanelSide::Top, 6),
            (PanelSide::Bottom, 8),
            (PanelSide::Left, 20),
            (PanelSide::Right, 24),
        ] {
            assert!(manager.update_panel_layout("inspector", side, size));
            assert_eq!(manager.focused_panel_id(), Some("inspector"));
            assert_eq!(manager.panel_layout("inspector"), Some((side, size)));
            assert_eq!(
                manager.text_panels["inspector"].blocks[0].text,
                "keep this content"
            );
            let composer = manager.text_panels["inspector"]
                .composer
                .as_ref()
                .expect("moving a pane preserves its composer");
            assert!(composer.focused);
            assert_eq!(composer.prompt.text(), "keep this draft");
        }
    }

    #[test]
    fn temporary_text_panel_context_restores_transcript_and_unsent_draft() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                title: Some("Agent".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask a follow-up…".to_string(),
                    rows: 3,
                }),
                ..PanelConfig::default()
            },
        );
        manager.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Markdown,
                text: "Existing conversation".to_string(),
            }],
            20,
            80,
        );
        assert!(manager.focus_text_panel_composer("agent"));
        manager.handle_focused_text_input(&Event::Paste("unsent follow-up".to_string()), 80);

        assert!(manager.enter_text_panel_context("agent", "Search previous sessions…".to_string()));
        manager.update_text_panel(
            "agent",
            vec![
                TextPanelBlock {
                    id: "session-browser:group:Recent".to_string(),
                    kind: TextPanelBlockKind::Text,
                    format: TextPanelBlockFormat::Plain,
                    text: "Recent".to_string(),
                },
                TextPanelBlock {
                    id: "session:one".to_string(),
                    kind: TextPanelBlockKind::Action,
                    format: TextPanelBlockFormat::Plain,
                    text: "Previous session\n1h ago".to_string(),
                },
                TextPanelBlock {
                    id: "session:two".to_string(),
                    kind: TextPanelBlockKind::Action,
                    format: TextPanelBlockFormat::Plain,
                    text: "Older session\n2h ago".to_string(),
                },
            ],
            20,
            80,
        );
        let composer = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert_eq!(composer.config.placeholder, "Search previous sessions…");
        assert_eq!(composer.config.rows, 1);
        assert_eq!(composer.prompt.text(), "");
        assert_eq!(manager.text_panels["agent"].scroll, 0);
        assert!(!manager.text_panels["agent"].follow_tail);
        assert_eq!(manager.text_panels["agent"].composer_height(20, 30), 0);
        assert_eq!(
            manager.text_panels["agent"].selected_picker_action(),
            Some("session:one")
        );
        assert_eq!(
            manager
                .focused_text_panel_cursor_position(80, 20)
                .unwrap()
                .1,
            2
        );
        let theme = Theme::default();
        let mut frame = RenderBuffer::new(80, 20, &theme.style);
        manager.render(&mut frame, &theme);
        assert!(row_text(&frame, 2).contains("Search previous sessions"));
        assert!(row_text(&frame, 3).contains("select"));
        let group = text_position(&frame, "RECENT").expect("session cards have a recency heading");
        assert!(row_text(&frame, group.y).contains('─'));
        let first_session = text_position(&frame, "Previous session").unwrap();
        let older_session = text_position(&frame, "Older session").unwrap();
        assert_eq!(older_session.y, first_session.y + 3);
        assert_eq!(
            frame.cells[first_session.y * frame.width + first_session.x - 2].text,
            "▌"
        );
        assert_eq!(
            frame.cells[older_session.y * frame.width + older_session.x - 2].text,
            "│"
        );
        assert!(text_style(&frame, "Previous session").bold);
        assert!(!text_style(&frame, "1h ago").bold);
        assert!(text_style(&frame, "Enter").bold);
        assert!(!(0..20).any(|row| row_text(&frame, row).contains("unsent follow-up")));
        let snapshot = manager.snapshot(80);
        let saved = snapshot
            .panels
            .iter()
            .find(|panel| panel.id == "agent")
            .and_then(|panel| panel.text.as_ref())
            .and_then(|text| text.composer.as_ref())
            .unwrap();
        assert_eq!(saved.text, "unsent follow-up");

        let input = manager
            .handle_focused_text_input(&Event::Paste("needle".to_string()), 80)
            .unwrap();
        assert_eq!(input.action, "filter_input");
        assert_eq!(input.text.as_deref(), Some("needle"));
        for event in [
            crossterm::event::KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            crossterm::event::KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
        ] {
            assert!(manager
                .handle_focused_text_input(&Event::Key(event), 80)
                .is_none());
            manager.handle_focused_key("down", 20, 80, 0).unwrap();
        }
        assert_eq!(
            manager.text_panels["agent"].selected_picker_action(),
            Some("session:two")
        );
        let submit = manager
            .handle_focused_text_input(
                &Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                )),
                80,
            )
            .unwrap();
        assert_eq!(submit.action, "activate_block");
        assert_eq!(submit.text.as_deref(), Some("session:two"));
        let cancel = manager
            .handle_focused_text_input(
                &Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Char('c'),
                    KeyModifiers::CONTROL,
                )),
                80,
            )
            .unwrap();
        assert_eq!(cancel.action, "filter_cancel");
        assert!(!manager.toggle_focused_text_region(80));
        assert!(matches!(
            manager.text_action_at_position(first_session.x, first_session.y, 80, 20),
            Some(TextPanelLinkTarget::PanelAction { block_id, .. }) if block_id == "session:one"
        ));
        assert_eq!(
            manager.text_panels["agent"].selected_picker_action(),
            Some("session:one")
        );

        assert!(manager.exit_text_panel_context("agent"));
        let panel = &manager.text_panels["agent"];
        assert_eq!(panel.blocks.len(), 1);
        assert_eq!(panel.blocks[0].id, "answer");
        let composer = panel.composer.as_ref().unwrap();
        assert_eq!(composer.config.placeholder, "Ask a follow-up…");
        assert_eq!(composer.config.rows, 3);
        assert_eq!(composer.prompt.text(), "unsent follow-up");
        assert!(composer.focused);
    }

    #[test]
    fn temporary_picker_restores_selection_scroll_and_query_per_workspace() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                title: Some("Agent".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 2,
                }),
                ..PanelConfig::default()
            },
        );
        let blocks = |ids: &[&str]| {
            ids.iter()
                .map(|id| TextPanelBlock {
                    id: (*id).to_string(),
                    kind: TextPanelBlockKind::Action,
                    format: TextPanelBlockFormat::Plain,
                    text: format!("Session {id}\n1h ago"),
                })
                .collect::<Vec<_>>()
        };
        let alpha = TextPanelContextOptions {
            key: "/alpha".to_string(),
            query: "needle".to_string(),
            ..TextPanelContextOptions::default()
        };
        assert!(manager.enter_text_panel_context_with_options(
            "agent",
            "Search".to_string(),
            alpha.clone(),
        ));
        manager.update_text_panel("agent", blocks(&["one", "two", "three"]), 8, 80);
        manager.handle_focused_key("down", 8, 80, 0);
        manager.handle_focused_key("down", 8, 80, 0);
        let remembered_scroll = manager.text_panels["agent"].scroll;
        assert!(remembered_scroll > 0);
        assert_eq!(
            manager.text_panels["agent"].selected_picker_action(),
            Some("three")
        );
        assert!(manager.exit_text_panel_context("agent"));

        assert!(manager.enter_text_panel_context_with_options(
            "agent",
            "Search".to_string(),
            alpha.clone(),
        ));
        assert_eq!(
            manager.text_panels["agent"]
                .composer
                .as_ref()
                .unwrap()
                .prompt
                .text(),
            "needle"
        );
        manager.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "loading".to_string(),
                kind: TextPanelBlockKind::Text,
                format: TextPanelBlockFormat::Plain,
                text: "Loading…".to_string(),
            }],
            8,
            80,
        );
        manager.update_text_panel("agent", blocks(&["one", "two", "three"]), 8, 80);
        assert_eq!(
            manager.text_panels["agent"].selected_picker_action(),
            Some("three")
        );
        assert_eq!(manager.text_panels["agent"].scroll, remembered_scroll);
        assert!(manager.exit_text_panel_context("agent"));

        assert!(manager.enter_text_panel_context_with_options(
            "agent",
            "Search".to_string(),
            alpha,
        ));
        manager.update_text_panel("agent", blocks(&["one", "two"]), 8, 80);
        assert_eq!(
            manager.text_panels["agent"].selected_picker_action(),
            Some("two")
        );
        assert!(manager.exit_text_panel_context("agent"));

        assert!(manager.enter_text_panel_context_with_options(
            "agent",
            "Search".to_string(),
            TextPanelContextOptions {
                key: "/beta".to_string(),
                ..TextPanelContextOptions::default()
            },
        ));
        manager.update_text_panel("agent", blocks(&["one", "two"]), 8, 80);
        assert_eq!(
            manager.text_panels["agent"].selected_picker_action(),
            Some("one")
        );
    }

    #[test]
    fn file_tree_stays_outside_a_conversation_pane_on_the_same_side() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Left,
                width: 40,
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 2,
                }),
                ..PanelConfig::default()
            },
        );
        manager.create_panel(
            "tree".to_string(),
            PanelConfig {
                side: PanelSide::Left,
                width: 20,
                ..PanelConfig::default()
            },
        );
        let placements = manager.panel_placements(160, 40);
        let x = |id: &str| {
            placements
                .iter()
                .find(|placement| placement.id == id)
                .unwrap()
                .x
        };
        assert_eq!(x("tree"), 0);
        assert_eq!(x("agent"), 21);
    }

    #[test]
    fn picker_tabs_show_the_scope_and_switch_it_by_click_or_shortcut() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                title: Some("Agent".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 2,
                }),
                ..PanelConfig::default()
            },
        );
        let tab = |id: &str, label: &str, compact_label: &str| TextPanelContextTab {
            id: id.to_string(),
            label: label.to_string(),
            compact_label: compact_label.to_string(),
        };
        assert!(manager.enter_text_panel_context_with_options(
            "agent",
            "Search".to_string(),
            TextPanelContextOptions {
                tabs: vec![
                    tab("red", "Red", ""),
                    tab("workspace", "This workspace", "Here"),
                    tab("all", "All Codex", "All"),
                ],
                tab: "red".to_string(),
                tab_shortcut: "Alt-s".to_string(),
                ..TextPanelContextOptions::default()
            },
        ));
        let theme = Theme::default();
        manager.set_presentation(PanelPresentation::Zoomed {
            id: "agent".to_string(),
            size: (80, 20),
        });
        let mut buffer = RenderBuffer::new(80, 20, &theme.style);
        manager.render(&mut buffer, &theme);
        let tabs = text_position(&buffer, "This workspace").unwrap();
        let row = row_text(&buffer, tabs.y);
        assert!(row.contains(" Red   This workspace   All Codex    Tab"));
        // The tabs row replaces the scope hint, and every hint shown is whole.
        let hints = row_text(&buffer, tabs.y + 1);
        assert!(hints.contains("Alt-P peek"));
        assert!(!hints.contains("scope"));
        let red = text_position(&buffer, " Red ").unwrap();
        assert!(buffer.cells[red.y * buffer.width + red.x + 1].style.bold);

        let event = manager
            .focus_panel_at_position(tabs.x + 2, tabs.y, 80, 20)
            .unwrap();
        assert_eq!(event.action, "context_tab");
        assert_eq!(event.text.as_deref(), Some("workspace"));
        let event = manager.header_shortcut_event("Alt-s").unwrap();
        assert_eq!(event.text.as_deref(), Some("all"));
        let event = manager.header_shortcut_event("Alt-s").unwrap();
        assert_eq!(event.text.as_deref(), Some("red"));
        let event = manager.header_shortcut_event("Shift-BackTab").unwrap();
        assert_eq!(event.text.as_deref(), Some("all"));
        let event = manager.header_shortcut_event("Tab").unwrap();
        assert_eq!(event.text.as_deref(), Some("red"));

        // Narrower panes switch to compact labels, then to the selected tab.
        let panel = &manager.text_panels["agent"];
        let palette = text_panel_palette(&theme, &panel.config);
        for (width, expected) in [
            (30, " Red   Here   All    Tab"),
            (11, " Red    Tab"),
            (10, " Red"),
        ] {
            let mut narrow = RenderBuffer::new(80, 1, &theme.style);
            render_text_panel_picker_tabs(&mut narrow, panel, Point::new(0, 0), width, &palette);
            assert_eq!(row_text(&narrow, 0).trim_end(), expected);
        }
    }

    #[test]
    fn composer_lists_the_pane_actions_left_out_of_the_header() {
        let mut manager = PanelManager::default();
        let action =
            |id: &str, key: &str, label: &str, show_in_header: bool| TextPanelHeaderAction {
                id: id.to_string(),
                key: key.to_string(),
                label: label.to_string(),
                show_in_header,
                ..TextPanelHeaderAction::default()
            };
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                title: Some("Agent".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 2,
                }),
                header_actions: vec![
                    action("sessions", "Alt-s", "sessions", false),
                    action("new", "Alt-n", "new", false),
                    action("close", "Alt-w", "×", true),
                ],
                ..PanelConfig::default()
            },
        );
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(80, 20, &theme.style);
        manager.render(&mut buffer, &theme);
        assert!(row_text(&buffer, 0).contains("×"));
        assert!(!row_text(&buffer, 0).contains("sessions"));
        let actions = text_position(&buffer, "Alt-s sessions · Alt-n new").unwrap();
        // The row sits under the composer's own footer.
        assert!(row_text(&buffer, actions.y - 1).contains("NAV"));
        let new = text_position(&buffer, "Alt-n").unwrap();
        let event = manager
            .focus_panel_at_position(new.x, new.y, 80, 20)
            .unwrap();
        assert_eq!(event.action, "new");

        // The picker hides the composer, and the row with it.
        assert!(manager.enter_text_panel_context("agent", "Search".to_string()));
        let mut picker = RenderBuffer::new(80, 20, &theme.style);
        manager.render(&mut picker, &theme);
        assert!(text_position(&picker, "Alt-n").is_none());
    }

    #[test]
    fn temporary_picker_starts_over_at_its_first_row_and_heading() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                title: Some("Agent".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 2,
                }),
                ..PanelConfig::default()
            },
        );
        let blocks = |ids: &[&str]| {
            let mut blocks = vec![TextPanelBlock {
                id: "group".to_string(),
                kind: TextPanelBlockKind::Text,
                format: TextPanelBlockFormat::Plain,
                text: "Recent".to_string(),
            }];
            blocks.extend(ids.iter().map(|id| TextPanelBlock {
                id: (*id).to_string(),
                kind: TextPanelBlockKind::Action,
                format: TextPanelBlockFormat::Plain,
                text: format!("Session {id}\n1h ago"),
            }));
            blocks
        };
        assert!(manager.enter_text_panel_context("agent", "Search".to_string()));
        manager.update_text_panel("agent", blocks(&["one", "two", "three", "four"]), 10, 80);
        for _ in 0..3 {
            manager.handle_focused_key("down", 10, 80, 0);
        }
        assert!(manager.text_panels["agent"].scroll > 0);

        // A different list keeps the row position of a selection that went away.
        manager.update_text_panel("agent", blocks(&["five", "six", "seven", "eight"]), 10, 80);
        assert_eq!(
            manager.text_panels["agent"].selected_picker_action(),
            Some("eight")
        );
        // After an empty update it starts over at the first row.
        manager.update_text_panel("agent", Vec::new(), 10, 80);
        manager.update_text_panel("agent", blocks(&["five", "six", "seven", "eight"]), 10, 80);
        assert_eq!(
            manager.text_panels["agent"].selected_picker_action(),
            Some("five")
        );
        assert_eq!(manager.text_panels["agent"].scroll, 0);

        // Moving back up to the first row brings its heading back into view.
        for _ in 0..3 {
            manager.handle_focused_key("down", 10, 80, 0);
        }
        assert!(manager.text_panels["agent"].scroll > 0);
        for _ in 0..3 {
            manager.handle_focused_key("up", 10, 80, 0);
        }
        assert_eq!(manager.text_panels["agent"].scroll, 0);
    }

    #[test]
    fn temporary_picker_preview_reflows_without_affecting_session_hit_testing() {
        for (panel_width, side_by_side) in [(48, false), (100, true)] {
            let terminal_width = if side_by_side { 240 } else { 120 };
            let mut manager = PanelManager::default();
            manager.create_text_panel(
                "agent".to_string(),
                PanelConfig {
                    title: Some("Agent".to_string()),
                    width: panel_width,
                    composer: Some(TextPanelComposerConfig {
                        placeholder: "Ask".to_string(),
                        rows: 2,
                    }),
                    ..PanelConfig::default()
                },
            );
            assert!(manager.enter_text_panel_context("agent", "Search sessions".to_string()));
            manager.update_text_panel(
                "agent",
                vec![TextPanelBlock {
                    id: "session:one".to_string(),
                    kind: TextPanelBlockKind::Action,
                    format: TextPanelBlockFormat::Plain,
                    text: "Selected conversation\n1h ago · Red".to_string(),
                }],
                24,
                terminal_width,
            );
            assert!(manager.set_text_panel_preview(
                "agent",
                Some(TextPanelPreview {
                    session_id: "one".to_string(),
                    prompt: "Why is the server offline?".to_string(),
                    response: "The **startup hook** failed.".to_string(),
                    ..TextPanelPreview::default()
                }),
                24,
                terminal_width,
            ));
            let theme = Theme::default();
            let mut frame = RenderBuffer::new(terminal_width, 24, &theme.style);
            manager.render(&mut frame, &theme);
            let session = text_position(&frame, "Selected conversation").unwrap();
            let preview = text_position(&frame, "LAST EXCHANGE").unwrap();
            let prompt = text_position(&frame, "Why is the server offline?").unwrap();
            assert!(text_position(&frame, "startup hook").is_some());
            if side_by_side {
                assert!(preview.x > session.x);
                assert_eq!(preview.y, session.y);
            } else {
                assert!(preview.y > session.y);
            }
            assert!(matches!(
                manager.text_action_at_position(session.x, session.y, terminal_width, 24),
                Some(TextPanelLinkTarget::PanelAction { block_id, .. }) if block_id == "session:one"
            ));
            assert!(manager
                .text_action_at_position(prompt.x, prompt.y, terminal_width, 24)
                .is_none());
            let shortcut = manager.header_shortcut_event("Alt-p").unwrap();
            assert_eq!(shortcut.action, "session_preview");
            assert_eq!(shortcut.text.as_deref(), Some("session:one"));
            assert!(manager
                .surface_actions()
                .iter()
                .any(|action| action.key == "Alt-p"));
        }
    }

    #[test]
    fn panel_remembers_independent_vertical_and_horizontal_sizes() {
        let mut manager = PanelManager::default();
        manager.create_panel(
            "tree".to_string(),
            PanelConfig {
                side: PanelSide::Left,
                width: 24,
                ..PanelConfig::default()
            },
        );

        assert_eq!(
            manager.panel_default_size("tree", PanelSide::Left),
            Some(24)
        );
        assert!(manager.update_panel_layout("tree", PanelSide::Left, 31));
        assert!(manager.update_panel_layout("tree", PanelSide::Bottom, 8));
        assert!(manager.update_panel_layout("tree", PanelSide::Bottom, 11));

        assert_eq!(
            manager.panel_preferred_size("tree", PanelSide::Right),
            Some(31)
        );
        assert_eq!(
            manager.panel_default_size("tree", PanelSide::Right),
            Some(24)
        );
        assert_eq!(
            manager.panel_preferred_size("tree", PanelSide::Top),
            Some(11)
        );
        assert_eq!(manager.panel_default_size("tree", PanelSide::Top), Some(8));
    }

    #[test]
    fn restored_panel_layout_keeps_creation_defaults_and_both_axis_sizes() {
        let mut manager = PanelManager::default();
        manager.create_panel(
            "tree".to_string(),
            PanelConfig {
                side: PanelSide::Left,
                width: 24,
                ..PanelConfig::default()
            },
        );

        assert!(manager.restore_panel_layout("tree", PanelSide::Bottom, 11, Some(31), Some(11),));
        assert_eq!(
            manager.panel_default_layout("tree"),
            Some((PanelSide::Left, 24))
        );
        assert_eq!(
            manager.panel_preferred_size("tree", PanelSide::Right),
            Some(31)
        );
        assert_eq!(
            manager.panel_preferred_size("tree", PanelSide::Top),
            Some(11)
        );

        assert!(manager.reset_panel_layout("tree"));
        assert_eq!(manager.panel_layout("tree"), Some((PanelSide::Left, 24)));
        assert_eq!(
            manager.panel_preferred_size("tree", PanelSide::Right),
            Some(24)
        );
        assert_eq!(manager.panel_preferred_size("tree", PanelSide::Top), None);
    }

    #[test]
    fn four_sided_panel_geometry_is_safe_in_tiny_terminals() {
        for side in [
            PanelSide::Left,
            PanelSide::Right,
            PanelSide::Top,
            PanelSide::Bottom,
        ] {
            let mut manager = PanelManager::default();
            manager.create_panel(
                "tree".to_string(),
                PanelConfig {
                    side,
                    width: usize::MAX,
                    ..PanelConfig::default()
                },
            );

            let theme = Theme::default();
            for (width, height) in [(0, 0), (1, 1), (2, 2), (3, 3), (4, 4)] {
                let mut buffer = RenderBuffer::new(width, height, &theme.style);
                manager.render(&mut buffer, &theme);
                for placement in manager.panel_placements(width, height) {
                    assert!(placement.x.saturating_add(placement.width) <= width);
                    assert!(placement.y.saturating_add(placement.height) <= height);
                }
            }
        }
    }

    #[test]
    fn panel_separators_clear_stale_editor_cells_after_reflow() {
        let mut manager = PanelManager::default();
        manager.create_panel(
            "left".to_string(),
            PanelConfig {
                side: PanelSide::Left,
                width: 4,
                title: None,
                composer: None,
                surface: None,
                border: None,
                header_actions: Vec::new(),
            },
        );
        manager.create_panel(
            "right".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 4,
                title: None,
                composer: None,
                surface: None,
                border: None,
                header_actions: Vec::new(),
            },
        );
        let style = Style::default();
        let theme = Theme {
            style: style.clone(),
            ..Theme::default()
        };
        let mut buffer = RenderBuffer::new_with_contents(20, 5, style, vec!["x".repeat(20); 5]);

        manager.render(&mut buffer, &theme);

        for y in 0..3 {
            assert_eq!(buffer.cells[y * 20 + 4].text, " ");
            assert_eq!(buffer.cells[y * 20 + 15].text, " ");
        }
    }

    #[test]
    fn multiple_right_panels_keep_their_reserved_separator_columns() {
        let mut manager = PanelManager::default();
        for id in ["outer", "inner"] {
            manager.create_panel(
                id.to_string(),
                PanelConfig {
                    side: PanelSide::Right,
                    width: 4,
                    title: None,
                    composer: None,
                    surface: None,
                    border: None,
                    header_actions: Vec::new(),
                },
            );
        }

        assert_eq!(manager.reserved_right_width(200), 10);
        assert_eq!(manager.panel_at_position(16, 0, 20, 5).unwrap().id, "outer");
        assert!(manager.panel_at_position(15, 0, 20, 5).is_none());
        assert_eq!(manager.panel_at_position(11, 0, 20, 5).unwrap().id, "inner");
        assert!(manager.panel_at_position(10, 0, 20, 5).is_none());
    }

    #[test]
    fn text_panel_blocks_deserialize_semantic_role_and_format() {
        let block: TextPanelBlock = serde_json::from_value(serde_json::json!({
            "id": "agent:1",
            "kind": "agent",
            "format": "markdown",
            "text": "# Heading"
        }))
        .unwrap();

        assert_eq!(block.kind, TextPanelBlockKind::Agent);
        assert_eq!(block.format, TextPanelBlockFormat::Markdown);
        assert_eq!(block.text, "# Heading");
    }

    #[test]
    fn syntax_highlighted_text_panel_spans_preserve_panel_background() {
        let panel_foreground = Color::Rgb {
            r: 10,
            g: 20,
            b: 30,
        };
        let panel_background = Color::Rgb {
            r: 40,
            g: 50,
            b: 60,
        };
        let code_foreground = Color::Rgb {
            r: 70,
            g: 80,
            b: 90,
        };
        let syntax_foreground = Color::Rgb {
            r: 100,
            g: 110,
            b: 120,
        };
        let syntax_background = Color::Rgb {
            r: 130,
            g: 140,
            b: 150,
        };
        let theme = Theme {
            style: Style {
                fg: Some(panel_foreground),
                bg: Some(panel_background),
                ..Style::default()
            },
            token_styles: vec![crate::theme::TokenStyle {
                name: None,
                scope: vec!["markup.raw.block.markdown".to_string()],
                style: Style {
                    fg: Some(code_foreground),
                    bold: true,
                    ..Style::default()
                },
            }],
            ..Theme::default()
        };
        let line = RenderedTextLine {
            spans: vec![
                RenderedTextSpan {
                    text: "a".to_string(),
                    style: TextPanelSpanStyle::Code,
                    syntax_style: Some(Style {
                        fg: Some(syntax_foreground),
                        italic: true,
                        ..Style::default()
                    }),
                    link: None,
                    selection: TextPanelSpanSelection::Content,
                },
                RenderedTextSpan {
                    text: "b".to_string(),
                    style: TextPanelSpanStyle::Code,
                    syntax_style: Some(Style {
                        bg: Some(syntax_background),
                        ..Style::default()
                    }),
                    link: None,
                    selection: TextPanelSpanSelection::Content,
                },
            ],
            break_after: RenderedTextLineBreak::Hard,
            selection: TextPanelLineSelection::Semantic,
        };
        let mut buffer = RenderBuffer::new(2, 1, &theme.style);
        let palette = text_panel_palette(&theme, &PanelConfig::default());

        render_text_spans(
            &mut buffer,
            0,
            0,
            2,
            &line,
            0,
            None,
            None,
            None,
            &theme,
            &palette,
        );

        assert_eq!(
            buffer.cells[0].style,
            Style {
                fg: Some(syntax_foreground),
                bg: Some(panel_background),
                bold: true,
                italic: true,
                underline: false,
            }
        );
        assert_eq!(
            buffer.cells[1].style,
            Style {
                fg: Some(code_foreground),
                bg: Some(syntax_background),
                bold: true,
                italic: false,
                underline: false,
            }
        );
    }

    #[test]
    fn scrollback_highlight_skips_display_chrome() {
        let theme = Theme::default();
        let line = RenderedTextLine {
            spans: vec![
                RenderedTextSpan {
                    text: "│ ".to_string(),
                    style: TextPanelSpanStyle::Muted,
                    syntax_style: None,
                    link: None,
                    selection: TextPanelSpanSelection::Chrome,
                },
                RenderedTextSpan {
                    text: "x".to_string(),
                    style: TextPanelSpanStyle::Code,
                    syntax_style: None,
                    link: None,
                    selection: TextPanelSpanSelection::Content,
                },
            ],
            break_after: RenderedTextLineBreak::Hard,
            selection: TextPanelLineSelection::Semantic,
        };
        let mut normal = RenderBuffer::new(3, 1, &theme.style);
        let mut selected = RenderBuffer::new(3, 1, &theme.style);
        let palette = text_panel_palette(&theme, &PanelConfig::default());

        render_text_spans(
            &mut normal,
            0,
            0,
            3,
            &line,
            0,
            None,
            None,
            None,
            &theme,
            &palette,
        );
        render_text_spans(
            &mut selected,
            0,
            0,
            3,
            &line,
            0,
            Some((0, 0)),
            None,
            None,
            &theme,
            &palette,
        );

        assert_eq!(normal.cells[0].style, selected.cells[0].style);
        assert_eq!(normal.cells[1].style, selected.cells[1].style);
        assert_ne!(normal.cells[2].style, selected.cells[2].style);
    }

    #[test]
    fn text_panel_composer_edits_unicode_submits_and_recalls_history() {
        use crossterm::event::KeyEvent;

        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 32,
                title: Some("Agent".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask a follow-up…".to_string(),
                    rows: 3,
                }),
                surface: None,
                border: None,
                header_actions: Vec::new(),
            },
        );
        assert!(manager.focus_text_panel_composer("agent"));
        manager.handle_focused_text_input(&Event::Paste("one 👨‍👩‍👧\r\ntwo".to_string()), 80);
        manager.handle_focused_text_input(
            &Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)),
            80,
        );
        manager.handle_focused_text_input(
            &Event::Key(KeyEvent::new(KeyCode::Char('世'), KeyModifiers::NONE)),
            80,
        );
        let submitted = manager
            .handle_focused_text_input(
                &Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)),
                80,
            )
            .unwrap();
        assert_eq!(submitted.action, "submit");
        assert_eq!(submitted.text.as_deref(), Some("one 👨‍👩‍👧\ntwo\n世"));

        manager.handle_focused_text_input(&Event::Paste("draft".to_string()), 80);
        manager.handle_focused_text_input(
            &Event::Key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            80,
        );
        let recalled = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert_eq!(recalled.prompt.text(), "one 👨‍👩‍👧\ntwo\n世");
        manager.handle_focused_text_input(
            &Event::Key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            80,
        );
        let restored = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert_eq!(restored.prompt.text(), "draft");
        assert!(manager.focused_text_panel_cursor_position(80, 20).is_some());
    }

    #[test]
    fn conversation_panes_size_responsively_and_go_compact_when_cramped() {
        let mut manager = PanelManager::default();
        let conversation = PanelConfig {
            side: PanelSide::Right,
            width: 62,
            composer: Some(TextPanelComposerConfig {
                placeholder: "Ask".to_string(),
                rows: 3,
            }),
            ..PanelConfig::default()
        };
        manager.create_text_panel("agent".to_string(), conversation);
        let docked_width = |manager: &PanelManager, terminal: usize| {
            manager
                .panel_placements(terminal, 40)
                .into_iter()
                .find(|placement| placement.id == "agent")
                .map(|placement| placement.width)
        };

        // The plugin's width wins on wide terminals, then 45% of the terminal,
        // then the readable minimum, and below that the pane stops docking.
        for (terminal, width) in [(180, 62), (120, 54), (100, 45), (80, 36), (73, 32)] {
            assert!(!manager.is_compact_panel("agent", terminal), "{terminal}");
            assert_eq!(docked_width(&manager, terminal), Some(width), "{terminal}");
            assert_eq!(
                manager.reserved_right_width(terminal),
                width + 1,
                "{terminal}"
            );
            assert!(terminal - width > MIN_COMFORTABLE_EDITOR_WIDTH);
        }
        for terminal in [72, 60, 40] {
            assert!(manager.is_compact_panel("agent", terminal), "{terminal}");
            assert_eq!(docked_width(&manager, terminal), None);
            assert_eq!(manager.reserved_right_width(terminal), 0);
        }

        // A width the user chose is kept while the editor keeps 40 columns.
        assert!(manager.update_panel_layout("agent", PanelSide::Right, 70));
        assert_eq!(docked_width(&manager, 180), Some(70));
        assert_eq!(docked_width(&manager, 100), Some(59));
        assert!(manager.reset_panel_layout("agent"));
        assert_eq!(docked_width(&manager, 100), Some(45));

        // Panels without a composer keep their declared width.
        manager.create_panel(
            "tree".to_string(),
            PanelConfig {
                side: PanelSide::Left,
                width: 30,
                ..PanelConfig::default()
            },
        );
        assert_eq!(manager.reserved_left_width(80), 31);
        // The tree leaves too little room for both, so the conversation compacts.
        assert!(manager.is_compact_panel("agent", 100));
        assert!(!manager.is_compact_panel("agent", 120));
    }

    #[test]
    fn conversation_panes_never_dock_below_the_readable_minimum() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 62,
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 3,
                }),
                ..PanelConfig::default()
            },
        );
        // A user shrinking the pane below 32 columns still gets a readable pane.
        assert!(manager.update_panel_layout("agent", PanelSide::Right, 12));
        assert_eq!(
            manager.text_panel_width("agent", 180),
            MIN_READABLE_PANE_WIDTH
        );
        assert_eq!(
            manager.reserved_right_width(180),
            MIN_READABLE_PANE_WIDTH + 1
        );
        assert!(!manager.is_compact_panel("agent", 180));
    }

    #[test]
    fn several_conversation_panes_share_the_room_beside_the_editor() {
        let mut manager = PanelManager::default();
        for (id, side) in [
            ("left-chat", PanelSide::Left),
            ("right-chat", PanelSide::Right),
        ] {
            manager.create_text_panel(
                id.to_string(),
                PanelConfig {
                    side,
                    width: 62,
                    composer: Some(TextPanelComposerConfig {
                        placeholder: "Ask".to_string(),
                        rows: 3,
                    }),
                    ..PanelConfig::default()
                },
            );
        }
        let docked = |manager: &PanelManager, terminal: usize| {
            manager
                .panel_placements(terminal, 30)
                .into_iter()
                .map(|placement| placement.width + 1)
                .sum::<usize>()
        };
        for terminal in [200, 180, 140, 100, 80, 60] {
            let reserved =
                manager.reserved_left_width(terminal) + manager.reserved_right_width(terminal);
            // Placements and the editor's reservation always agree, and the
            // editor keeps its 40 columns whenever anything is docked.
            assert_eq!(docked(&manager, terminal), reserved, "{terminal}");
            if reserved > 0 {
                assert!(
                    terminal - reserved >= MIN_COMFORTABLE_EDITOR_WIDTH,
                    "{terminal}"
                );
            }
        }
        // Both fit on a wide terminal; at 100 columns only the first docks and
        // the second becomes compact instead of overlapping the editor.
        assert!(!manager.is_compact_panel("right-chat", 180));
        assert!(!manager.is_compact_panel("left-chat", 100));
        assert!(manager.is_compact_panel("right-chat", 100));
        assert_eq!(manager.text_panel_width("right-chat", 100), 100);
    }

    #[test]
    fn queued_cards_are_dimmed_in_every_format() {
        let mut manager = PanelManager::default();
        manager.create_text_panel("agent".to_string(), PanelConfig::default());
        let formats = [
            TextPanelBlockFormat::Plain,
            TextPanelBlockFormat::Markdown,
            TextPanelBlockFormat::Code(Some("rust".to_string())),
        ];
        let blocks = formats
            .iter()
            .enumerate()
            .map(|(index, format)| TextPanelBlock {
                id: format!("held-{index}"),
                kind: TextPanelBlockKind::Queued,
                format: format.clone(),
                text: "**bold** `code` fn main() {}".to_string(),
            })
            .collect();
        manager.update_text_panel("agent", blocks, 20, 120);
        let rendered = manager.text_panels["agent"].build_rendered_lines(40);
        let content = rendered
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .filter(|span| matches!(span.selection, TextPanelSpanSelection::Content))
            .filter(|span| !span.text.trim().is_empty())
            .collect::<Vec<_>>();
        assert!(!content.is_empty());
        assert!(
            content
                .iter()
                .all(|span| span.style == TextPanelSpanStyle::Muted && span.syntax_style.is_none()),
            "{content:#?}"
        );
    }

    #[test]
    fn turn_actions_on_a_queued_card_belong_to_the_sent_prompt_above_it() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 40,
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 2,
                }),
                ..PanelConfig::default()
            },
        );
        let block = |id: &str, kind, text: &str| TextPanelBlock {
            id: id.to_string(),
            kind,
            format: TextPanelBlockFormat::Plain,
            text: text.to_string(),
        };
        manager.update_text_panel(
            "agent",
            vec![
                block("sent", TextPanelBlockKind::User, "sent prompt"),
                block("answer", TextPanelBlockKind::Agent, "the answer"),
                block("held", TextPanelBlockKind::Queued, "held prompt"),
            ],
            20,
            120,
        );
        assert!(manager.focus_panel("agent"));
        assert!(manager.focus_focused_text_scrollback(120));
        // The transcript cursor sits on the queued card at the bottom.
        manager.handle_focused_scrollback_input(
            &Event::Key(crossterm::event::KeyEvent::from(KeyCode::Char('G'))),
            20,
            120,
            1,
        );
        let turn = manager.selected_text_turn(120).unwrap();
        assert_eq!(turn.prompt_id, "sent");
        assert!(turn.has_answer);
    }

    #[test]
    fn escape_from_a_compact_transcript_returns_to_the_editor() {
        use crossterm::event::KeyEvent;

        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 62,
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 3,
                }),
                ..PanelConfig::default()
            },
        );
        let escape = Event::Key(KeyEvent::from(KeyCode::Esc));
        // Docked: Esc in the transcript goes back to the composer, as before.
        assert!(manager.focus_panel("agent"));
        assert!(manager.focus_focused_text_scrollback(120));
        assert!(matches!(
            manager.handle_focused_scrollback_input(&escape, 20, 120, 1),
            Some(TextPanelScrollbackInput::Handled)
        ));
        // Compact: the pane covers the editor, so Esc leaves it to the editor.
        assert!(manager.focus_focused_text_scrollback(60));
        assert!(manager
            .handle_focused_scrollback_input(&escape, 20, 60, 1)
            .is_none());
    }

    #[test]
    fn refocusing_a_composer_lands_in_insert_mode_but_region_toggles_keep_its_mode() {
        use crossterm::event::KeyEvent;

        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 48,
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 3,
                }),
                ..PanelConfig::default()
            },
        );
        let mode = |manager: &PanelManager| {
            manager.text_panels["agent"]
                .composer
                .as_ref()
                .unwrap()
                .prompt
                .mode()
        };
        assert!(manager.focus_text_panel_composer("agent"));
        manager.handle_focused_text_input(&Event::Key(KeyEvent::from(KeyCode::Esc)), 80);
        assert_eq!(mode(&manager), crate::editor::Mode::Normal);

        assert!(manager.focus_focused_text_scrollback(80));
        assert!(manager.toggle_focused_text_region(80));
        assert_eq!(mode(&manager), crate::editor::Mode::Normal);

        assert!(manager.focus_focused_text_scrollback(80));
        assert!(manager.focus_text_panel_composer("agent"));
        assert_eq!(mode(&manager), crate::editor::Mode::Insert);
    }

    #[test]
    fn text_panel_composer_hydrates_arrow_and_reverse_search_history() {
        use crossterm::event::KeyEvent;

        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 48,
                title: Some("Agent".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask a follow-up…".to_string(),
                    rows: 3,
                }),
                ..PanelConfig::default()
            },
        );
        assert!(manager.set_text_panel_composer_history(
            "agent",
            vec![
                "deploy production".to_string(),
                "show status".to_string(),
                "deploy staging".to_string(),
                "deploy production".to_string(),
                "  ".to_string(),
            ],
        ));
        assert!(manager.focus_text_panel_composer("agent"));
        manager.handle_focused_text_input(&Event::Paste("draft".to_string()), 80);

        manager.handle_focused_text_input(
            &Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            80,
        );
        assert_eq!(
            manager.text_panels["agent"]
                .composer
                .as_ref()
                .unwrap()
                .prompt
                .text(),
            "deploy production"
        );
        manager.handle_focused_text_input(
            &Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            80,
        );
        assert_eq!(
            manager.text_panels["agent"]
                .composer
                .as_ref()
                .unwrap()
                .prompt
                .text(),
            "draft"
        );

        manager.handle_focused_text_input(
            &Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
            80,
        );
        let composer = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert_eq!(composer.prompt.text(), "draft");
        assert_eq!(composer.prompt.display_text(), "Search prompts: ");
        assert_eq!(composer.prompt.history_search_match_position(), None);

        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(80, 20, &theme.style);
        manager.render(&mut buffer, &theme);
        let initial = (0..20)
            .map(|row| row_text(&buffer, row))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(initial.contains("Search prompts:"), "{initial}");
        assert!(initial.contains("type to search"), "{initial}");
        assert!(!initial.contains("deploy production"), "{initial}");

        for character in ['d', 'e', 'p'] {
            manager.handle_focused_text_input(
                &Event::Key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)),
                80,
            );
        }
        manager.handle_focused_text_input(
            &Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
            80,
        );
        let composer = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert_eq!(composer.prompt.text(), "draft");
        assert_eq!(
            composer.prompt.display_text(),
            "Search prompts: dep\n↳ deploy staging"
        );
        assert_eq!(
            composer.prompt.history(),
            ["deploy production", "show status", "deploy staging"]
        );

        let mut buffer = RenderBuffer::new(80, 20, &theme.style);
        manager.render(&mut buffer, &theme);
        let rendered = (0..20)
            .map(|row| row_text(&buffer, row))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("^R/↑/↓ matches"), "{rendered}");
        assert!(rendered.contains("2/2 matches"), "{rendered}");
        assert!(rendered.contains("Search prompts: dep"), "{rendered}");
        assert!(rendered.contains("↳ deploy staging"), "{rendered}");
        let (cursor_x, cursor_y) = manager
            .focused_text_panel_cursor_position(80, 20)
            .expect("history query owns the visible cursor");
        let cursor_row = row_text(&buffer, cursor_y);
        let query_end = cursor_row
            .find("Search prompts: dep")
            .expect("cursor remains on the search field")
            + "Search prompts: dep".len();
        assert_eq!(cursor_x, display_width(&cursor_row[..query_end]));

        let accepted = manager
            .handle_focused_text_input(
                &Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                80,
            )
            .unwrap();
        assert_eq!(accepted.action, "composer_input");
        assert_eq!(accepted.text, None);
        let submitted = manager
            .handle_focused_text_input(
                &Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                80,
            )
            .unwrap();
        assert_eq!(submitted.action, "submit");
        assert_eq!(submitted.text.as_deref(), Some("deploy staging"));

        manager.handle_focused_text_input(
            &Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
            80,
        );
        assert!(manager.text_panels["agent"]
            .composer
            .as_ref()
            .unwrap()
            .prompt
            .history_search_active());
        manager.handle_focused_text_input(
            &Event::Key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)),
            80,
        );
        assert!(!manager.text_panels["agent"]
            .composer
            .as_ref()
            .unwrap()
            .prompt
            .history_search_active());
        manager.handle_focused_text_input(
            &Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
            80,
        );
        assert!(manager.text_panels["agent"]
            .composer
            .as_ref()
            .unwrap()
            .prompt
            .history_search_active());
        assert!(manager.focus_focused_text_scrollback(80));
        assert!(!manager.text_panels["agent"]
            .composer
            .as_ref()
            .unwrap()
            .prompt
            .history_search_active());
    }

    #[test]
    fn focused_composer_newline_shortcuts_take_priority_over_panel_navigation() {
        use crossterm::event::KeyEvent;

        for (code, modifiers) in [
            (KeyCode::Enter, KeyModifiers::ALT),
            (KeyCode::Enter, KeyModifiers::SHIFT),
            (KeyCode::Char('j'), KeyModifiers::CONTROL),
            (KeyCode::Char('\n'), KeyModifiers::NONE),
        ] {
            let mut manager = PanelManager::default();
            manager.create_text_panel(
                "agent".into(),
                PanelConfig {
                    composer: Some(TextPanelComposerConfig {
                        placeholder: "Ask".into(),
                        rows: 3,
                    }),
                    ..PanelConfig::default()
                },
            );
            assert!(manager.focus_text_panel_composer("agent"));
            manager.handle_focused_text_input(&Event::Paste("first".into()), 80);
            let event = manager
                .handle_focused_text_input(&Event::Key(KeyEvent::new(code, modifiers)), 80)
                .unwrap();
            assert_eq!(event.action, "composer_input");
            assert_eq!(
                manager.text_panels["agent"]
                    .composer
                    .as_ref()
                    .unwrap()
                    .prompt
                    .text(),
                "first\n"
            );
            let event = manager
                .handle_focused_text_input(
                    &Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                    80,
                )
                .unwrap();
            assert_eq!(event.action, "submit");
            assert_eq!(event.text.as_deref(), Some("first\n"));
        }
    }

    #[test]
    fn text_panel_submission_resumes_tail_following_after_manual_scroll() {
        use crossterm::event::KeyEvent;

        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 32,
                title: Some("Agent".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 3,
                }),
                ..PanelConfig::default()
            },
        );
        let history = TextPanelBlock {
            id: "history".to_string(),
            kind: TextPanelBlockKind::Agent,
            format: TextPanelBlockFormat::Plain,
            text: (1..=20)
                .map(|line| format!("history line {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
        };
        manager.update_text_panel("agent", vec![history.clone()], 8, 80);
        assert!(manager.focus_text_panel_composer("agent"));
        manager.handle_focused_key("top", 8, 80, 0).unwrap();
        assert_eq!(manager.text_panels["agent"].scroll, 0);
        assert!(!manager.text_panels["agent"].follow_tail);

        let empty = manager
            .handle_focused_text_input(
                &Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)),
                80,
            )
            .unwrap();
        assert_eq!(empty.action, "composer_input");
        assert!(!manager.text_panels["agent"].follow_tail);

        manager.handle_focused_text_input(&Event::Paste("next question".to_string()), 80);
        let submitted = manager
            .handle_focused_text_input(
                &Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)),
                80,
            )
            .unwrap();
        assert_eq!(submitted.action, "submit");
        assert_eq!(submitted.text.as_deref(), Some("next question"));
        assert!(manager.text_panels["agent"].follow_tail);

        manager.update_text_panel(
            "agent",
            vec![
                history,
                TextPanelBlock {
                    id: "question".to_string(),
                    kind: TextPanelBlockKind::User,
                    format: TextPanelBlockFormat::Plain,
                    text: "next question".to_string(),
                },
            ],
            8,
            80,
        );
        let panel = &manager.text_panels["agent"];
        assert_eq!(panel.scroll, panel.max_scroll(8, 32));

        manager.append_text_panel("agent", "answer", "streamed answer", 8, 80);
        let panel = &manager.text_panels["agent"];
        assert_eq!(panel.scroll, panel.max_scroll(8, 32));

        manager.handle_focused_key("up", 8, 80, 0).unwrap();
        let manual_scroll = manager.text_panels["agent"].scroll;
        assert!(!manager.text_panels["agent"].follow_tail);
        manager.append_text_panel("agent", "answer", "\nmore output", 8, 80);
        assert_eq!(manager.text_panels["agent"].scroll, manual_scroll);
    }

    #[test]
    fn text_panel_composer_uses_prompt_local_normal_and_insert_modes() {
        use crossterm::event::KeyEvent;

        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 52,
                title: Some("Agent".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 3,
                }),
                ..PanelConfig::default()
            },
        );
        assert!(manager.focus_text_panel_composer("agent"));
        manager.handle_focused_text_input(&Event::Paste("first second".to_string()), 80);
        assert!(manager
            .handle_focused_text_input(
                &Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
                80,
            )
            .is_none());
        assert!(manager.focused_text_input_active());
        assert_eq!(
            manager.text_panels["agent"]
                .composer
                .as_ref()
                .unwrap()
                .prompt
                .text(),
            "first second"
        );
        assert_eq!(
            manager.focused_text_panel_cursor_mode(),
            Some(crate::editor::Mode::Insert)
        );

        let escape = manager
            .handle_focused_text_input(
                &Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
                80,
            )
            .unwrap();
        assert_eq!(escape.action, "composer_input");
        assert_eq!(
            manager.focused_text_panel_cursor_mode(),
            Some(crate::editor::Mode::Normal)
        );

        for character in ['0', 'd', 'w'] {
            manager.handle_focused_text_input(
                &Event::Key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)),
                80,
            );
        }
        let composer = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert_eq!(composer.prompt.text(), "second");

        manager.handle_focused_text_input(
            &Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE)),
            80,
        );
        let composer = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert_eq!(composer.prompt.text(), "first second");

        manager.handle_focused_text_input(
            &Event::Key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT)),
            80,
        );
        manager.handle_focused_text_input(
            &Event::Key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::SHIFT)),
            80,
        );
        assert_eq!(
            manager.focused_text_panel_cursor_mode(),
            Some(crate::editor::Mode::Insert)
        );

        let submitted = manager
            .handle_focused_text_input(
                &Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)),
                80,
            )
            .unwrap();
        assert_eq!(submitted.action, "submit");
        assert_eq!(submitted.text.as_deref(), Some("first second!"));
        assert_eq!(
            manager.focused_text_panel_cursor_mode(),
            Some(crate::editor::Mode::Insert)
        );
    }

    #[test]
    fn text_panel_composer_delegates_navigation_keys_to_panel_scrolling() {
        use crossterm::event::KeyEvent;

        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 32,
                title: Some("Agent".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 3,
                }),
                ..PanelConfig::default()
            },
        );
        manager.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "history".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Plain,
                text: (1..=30)
                    .map(|line| format!("line {line}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            }],
            15,
            80,
        );
        assert!(manager.focus_text_panel_composer("agent"));
        manager.handle_focused_text_input(&Event::Paste("first\nsecond".to_string()), 80);
        manager.handle_focused_text_input(
            &Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            80,
        );
        let prompt_cursor = manager.text_panels["agent"]
            .composer
            .as_ref()
            .unwrap()
            .prompt
            .cursor();

        for code in [
            KeyCode::Char('j'),
            KeyCode::Char('k'),
            KeyCode::Up,
            KeyCode::Down,
        ] {
            assert!(
                manager
                    .handle_focused_text_input(
                        &Event::Key(KeyEvent::new(code, KeyModifiers::NONE)),
                        80,
                    )
                    .is_none(),
                "{code:?} should fall through to panel navigation"
            );
            assert_eq!(
                manager.text_panels["agent"]
                    .composer
                    .as_ref()
                    .unwrap()
                    .prompt
                    .cursor(),
                prompt_cursor,
                "{code:?} should not move the Normal-mode prompt cursor"
            );
        }

        manager
            .text_panels
            .get_mut("agent")
            .unwrap()
            .scroll_to_top();
        manager.handle_focused_key("down", 15, 80, 0).unwrap();
        assert_eq!(manager.text_panels["agent"].scroll, 1);

        manager
            .text_panels
            .get_mut("agent")
            .unwrap()
            .composer
            .as_mut()
            .unwrap()
            .prompt
            .set_mode(crate::editor::Mode::Insert);
        let insert_text = manager.text_panels["agent"]
            .composer
            .as_ref()
            .unwrap()
            .prompt
            .text();
        let insert_cursor = manager.text_panels["agent"]
            .composer
            .as_ref()
            .unwrap()
            .prompt
            .cursor();
        for (code, modifiers) in [
            (KeyCode::Char('h'), KeyModifiers::CONTROL),
            (KeyCode::Char('k'), KeyModifiers::CONTROL),
            (KeyCode::Char('g'), KeyModifiers::CONTROL),
            (KeyCode::Char('w'), KeyModifiers::CONTROL),
            (
                KeyCode::Char('G'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
        ] {
            assert!(
                manager
                    .handle_focused_text_input(&Event::Key(KeyEvent::new(code, modifiers)), 80)
                    .is_none(),
                "{modifiers:?}+{code:?} should fall through to panel navigation"
            );
            let prompt = &manager.text_panels["agent"]
                .composer
                .as_ref()
                .unwrap()
                .prompt;
            assert_eq!(prompt.text(), insert_text);
            assert_eq!(prompt.cursor(), insert_cursor);
        }

        let inserted = manager
            .handle_focused_text_input(
                &Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
                80,
            )
            .unwrap();
        assert_eq!(inserted.action, "composer_input");
        assert!(manager.text_panels["agent"]
            .composer
            .as_ref()
            .unwrap()
            .prompt
            .text()
            .contains('j'));
    }

    #[test]
    fn text_panel_composer_renders_its_local_mode_and_blurs_from_normal_mode() {
        use crossterm::event::KeyEvent;

        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 70,
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 2,
                }),
                ..PanelConfig::default()
            },
        );
        assert!(manager.focus_text_panel_composer("agent"));
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(160, 20, &theme.style);

        manager.render(&mut buffer, &theme);
        assert!((0..20).any(|row| row_text(&buffer, row).contains("INSERT")));

        manager.handle_focused_text_input(
            &Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            160,
        );
        manager.render(&mut buffer, &theme);
        assert!((0..20).any(|row| row_text(&buffer, row).contains("NORMAL")));

        let blur = manager
            .handle_focused_text_input(
                &Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
                160,
            )
            .unwrap();
        assert_eq!(blur.action, "composer_blur");
        assert!(!manager.focused_text_input_active());
    }

    #[test]
    fn text_panel_composer_covers_narrow_terminals_and_keeps_tail_visible() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 52,
                title: Some("Agent".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 2,
                }),
                surface: None,
                border: None,
                header_actions: Vec::new(),
            },
        );
        manager.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Plain,
                text: "first line\nsecond line\nthird line\nLATEST".to_string(),
            }],
            10,
            30,
        );
        // Too narrow to dock beside the editor: the pane reserves nothing and
        // only appears, full width, while it has focus (the editor zooms it).
        assert!(manager.is_compact_panel("agent", 30));
        assert_eq!(manager.reserved_right_width(30), 0);
        assert!(manager.panel_at_position(29, 0, 30, 12).is_none());
        manager.set_presentation(PanelPresentation::Zoomed {
            id: "agent".to_string(),
            size: (30, 10),
        });
        let placement = manager.panel_at_position(29, 0, 30, 12).unwrap();
        assert_eq!((placement.x, placement.width), (0, 30));

        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(30, 12, &theme.style);
        manager.render(&mut buffer, &theme);
        assert!((1..6).any(|row| row_text(&buffer, row).contains("LATEST")));
        assert!((6..10).any(|row| row_text(&buffer, row).contains("Ask")));
    }

    #[test]
    fn text_panel_model_header_preserves_state_and_uses_painted_hit_region() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent-conversation".into(),
            PanelConfig {
                side: PanelSide::Right,
                width: 62,
                title: Some("Agent".into()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".into(),
                    rows: 3,
                }),
                header_actions: vec![TextPanelHeaderAction {
                    id: "close".into(),
                    label: "×".into(),
                    compact_label: None,
                    ..TextPanelHeaderAction::default()
                }],
                ..PanelConfig::default()
            },
        );
        manager.focus_text_panel_composer("agent-conversation");
        let panel = manager.text_panels.get_mut("agent-conversation").unwrap();
        panel
            .composer
            .as_mut()
            .unwrap()
            .prompt
            .replace_draft("unfinished draft");
        let detail = TextPanelHeaderDetail {
            text: "model-with-a-long-name".into(),
            secondary: "high".into(),
            compact_text: String::new(),
            action: Some("model".into()),
            shortcut: Some("Alt-m".into()),
        };
        assert!(manager.set_text_panel_header_detail("agent-conversation", Some(detail.clone())));
        assert!(!manager.set_text_panel_header_detail("agent-conversation", Some(detail)));
        assert_eq!(
            manager.text_panels["agent-conversation"]
                .composer
                .as_ref()
                .unwrap()
                .prompt
                .text(),
            "unfinished draft"
        );
        assert!(manager.focused_text_input_active());
        let theme = Theme::default();
        let mut wide = RenderBuffer::new(100, 20, &theme.style);
        manager.render(&mut wide, &theme);
        let header = row_text(&wide, 0);
        assert!(header.contains("Agent · model-with-a-long-name · high"));
        let x = display_width(&header[..header.find("model-with").unwrap()]);
        assert_eq!(
            manager
                .focus_panel_at_position(x, 0, 100, 20)
                .unwrap()
                .action,
            "model"
        );
        assert!(manager.focused_text_input_active());
        let panel = &manager.text_panels["agent-conversation"];
        for width in 0..70 {
            if let Some((start, text)) = text_panel_header_detail_layout(panel, width) {
                let action_start = text_panel_header_actions_for_panel(panel, width)
                    .first()
                    .map_or(width, |(start, _, _)| *start);
                assert!(start + display_width(&text) < action_start);
            }
        }
        let (_, narrow) = text_panel_header_detail_layout(panel, 24).unwrap();
        assert!(narrow.ends_with('…'));
        assert!(!narrow.contains("high"));
    }

    #[test]
    fn pending_model_header_compacts_buttons_before_hiding_the_change() {
        let mut panel = TextPanel::new(
            "agent".into(),
            PanelConfig {
                title: Some("Agent".into()),
                header_actions: [
                    ("clear", "Clear", "C"),
                    ("new", "New", "N"),
                    ("close", "×", "×"),
                ]
                .into_iter()
                .map(|(id, label, compact)| TextPanelHeaderAction {
                    id: id.into(),
                    label: label.into(),
                    compact_label: Some(compact.into()),
                    ..TextPanelHeaderAction::default()
                })
                .collect(),
                ..Default::default()
            },
        );
        panel.header_detail = Some(TextPanelHeaderDetail {
            text: "smoke-deep".into(),
            secondary: "Next: smoke-fast · medium".into(),
            compact_text: "smoke-deep → smoke-fast".into(),
            action: Some("model".into()),
            shortcut: Some("Alt-m".into()),
        });
        assert!(text_panel_header_detail_layout(&panel, 59)
            .unwrap()
            .1
            .contains("Next: smoke-fast"));
        assert_eq!(
            text_panel_header_actions_for_panel(&panel, 59)
                .iter()
                .map(|(_, _, label)| label.as_str())
                .collect::<Vec<_>>(),
            ["C", "N", "×"]
        );
        assert!(text_panel_header_detail_layout(&panel, 42)
            .unwrap()
            .1
            .contains('→'));
        for (start, action, _) in text_panel_header_actions_for_panel(&panel, 60) {
            assert_eq!(
                text_panel_header_action_at(&panel, 60, start),
                Some(action.id.as_str())
            );
        }
    }

    #[test]
    fn text_panel_header_actions_render_full_and_compact_and_are_clickable() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 52,
                title: Some("Agent".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 2,
                }),
                surface: None,
                border: None,
                header_actions: vec![
                    TextPanelHeaderAction {
                        id: "clear".to_string(),
                        label: "Clear".to_string(),
                        compact_label: Some("C".to_string()),
                        ..TextPanelHeaderAction::default()
                    },
                    TextPanelHeaderAction {
                        id: "new".to_string(),
                        label: "New".to_string(),
                        compact_label: Some("N".to_string()),
                        ..TextPanelHeaderAction::default()
                    },
                    TextPanelHeaderAction {
                        id: "close".to_string(),
                        label: "×".to_string(),
                        compact_label: Some("×".to_string()),
                        ..TextPanelHeaderAction::default()
                    },
                ],
            },
        );
        let theme = Theme::default();
        let mut wide = RenderBuffer::new(80, 20, &theme.style);
        manager.render(&mut wide, &theme);
        let wide_header = row_text(&wide, 0);
        assert!(wide_header.contains("Agent"));
        assert!(wide_header.contains("Clear   New   ×"));

        for (label, expected) in [("Clear", "clear"), ("New", "new"), ("×", "close")] {
            let start = wide_header.find(label).unwrap();
            let column = display_width(&wide_header[..start]);
            let event = manager.focus_panel_at_position(column, 0, 80, 20).unwrap();
            assert_eq!(event.action, expected);
        }
        // Too narrow to dock: the pane covers the terminal while focused.
        assert!(manager.is_compact_panel("agent", 20));
        manager.set_presentation(PanelPresentation::Zoomed {
            id: "agent".to_string(),
            size: (20, 10),
        });
        let mut narrow = RenderBuffer::new(20, 12, &theme.style);
        manager.render(&mut narrow, &theme);
        assert!(row_text(&narrow, 0).contains("C   N   ×"));

        let actions = text_panel_header_actions(&manager.text_panels["agent"].config, 4);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].1.id, "close");
    }

    #[test]
    fn text_panel_code_blocks_preserve_source_and_refresh_highlights() {
        let source = "{\n  \"users\": [\n\n    true\n  ]\n}";
        let block: TextPanelBlock = serde_json::from_value(serde_json::json!({
            "id": "result",
            "format": "code",
            "language": "json",
            "text": source,
        }))
        .unwrap();
        assert_eq!(
            block.format,
            TextPanelBlockFormat::Code(Some("json".to_string()))
        );
        let serialized = serde_json::to_value(&block).unwrap();
        assert_eq!(serialized["format"], "code");
        assert_eq!(serialized["language"], "json");

        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let mut highlighter = Highlighter::new(&theme).unwrap();
        let mut manager = PanelManager::default();
        manager.create_text_panel("preview".into(), PanelConfig::default());
        manager.update_text_panel_with_highlighter(
            "preview",
            vec![block],
            &mut highlighter,
            /*panel_height*/ 20,
            /*terminal_width*/ 40,
        );
        let panel = &manager.text_panels["preview"];
        let layout = panel.layout(40);
        let rendered = layout
            .rendered
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert!(rendered.iter().any(|line| line == "  \"users\": ["));
        assert!(rendered.iter().any(String::is_empty));
        assert!(layout
            .rendered
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.syntax_style.is_some()));
        assert_eq!(panel.copy_all(), source);

        let old_layout = Arc::clone(&layout);
        let mut replacement = Highlighter::new(&theme).unwrap();
        manager.rehighlight_text_panels(&mut replacement);
        assert!(!Arc::ptr_eq(
            &old_layout,
            &manager.text_panels["preview"].layout(40)
        ));
    }

    #[test]
    fn text_panel_code_blocks_fall_back_and_reject_invalid_metadata() {
        for language in [None, Some("unsupported")]
            .into_iter()
            .map(|language| language.map(str::to_string))
        {
            let mut panel = TextPanel::new("preview".into(), PanelConfig::default());
            panel.update_blocks(
                vec![TextPanelBlock {
                    id: "result".into(),
                    kind: TextPanelBlockKind::Text,
                    format: TextPanelBlockFormat::Code(language),
                    text: "  one\n\n    two".into(),
                }],
                /*panel_height*/ 20,
                /*panel_width*/ 40,
            );
            let layout = panel.layout(40);
            assert!(layout
                .rendered
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| span.syntax_style.is_none()));
            assert_eq!(panel.copy_all(), "  one\n\n    two");
        }
        assert!(serde_json::from_value::<TextPanelBlock>(serde_json::json!({
            "id": "result",
            "format": "plain",
            "language": "json",
            "text": "{}",
        }))
        .is_err());
        assert!(serde_json::from_value::<TextPanelBlock>(serde_json::json!({
            "id": "result",
            "format": "code",
            "text": "{}",
            "unknown": true,
        }))
        .is_err());
    }

    #[test]
    fn text_panel_actions_share_shortcuts_footer_and_discovery() {
        let action = |id: &str, key: &str, enabled: bool| TextPanelHeaderAction {
            id: id.into(),
            key: key.into(),
            label: id.into(),
            enabled,
            show_in_header: false,
            ..TextPanelHeaderAction::default()
        };
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "preview".into(),
            PanelConfig {
                title: Some("Preview".into()),
                header_actions: vec![
                    action("run", "r", true),
                    action("apply", "a", false),
                    action("live", "", true),
                ],
                ..PanelConfig::default()
            },
        );
        assert_eq!(manager.text_panels["preview"].action_footer_height(), 0);
        assert!(manager.focus_panel("preview"));
        let panel = &manager.text_panels["preview"];
        assert_eq!(panel.action_footer_height(), 1);
        assert_eq!(
            manager
                .action_shortcut_event(&UiAction::new("", "r", "").event().unwrap())
                .unwrap()
                .action,
            "run"
        );
        assert!(manager.activate_surface_action("apply").is_none());
        assert_eq!(
            manager.activate_surface_action("live").unwrap().action,
            "live"
        );
        assert!(manager
            .surface_actions()
            .iter()
            .any(|action| { action.id == "live" && action.key.is_empty() && action.enabled }));

        manager
            .text_panels
            .get_mut("preview")
            .unwrap()
            .scrollback
            .mode = TextPanelScrollbackMode::Visual;
        assert!(manager
            .action_shortcut_event(&UiAction::new("", "r", "").event().unwrap())
            .is_none());
    }

    #[test]
    fn legacy_text_panel_actions_keep_clickable_defaults() {
        let action: TextPanelHeaderAction = serde_json::from_value(serde_json::json!({
            "id": "close",
            "label": "Close",
            "compact_label": "x",
        }))
        .unwrap();
        assert!(action.enabled);
        assert!(action.show_in_header);
        assert!(action.key.is_empty());
        assert!(
            serde_json::from_value::<TextPanelHeaderAction>(serde_json::json!({
                "id": "close",
                "label": "Close",
                "unknown": true,
            }))
            .is_err()
        );
    }

    #[test]
    fn text_panel_links_support_keyboard_navigation_and_clicks() {
        use crossterm::event::KeyEvent;

        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 40,
                title: Some("Agent".to_string()),
                composer: None,
                surface: None,
                border: None,
                header_actions: Vec::new(),
            },
        );
        manager.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Markdown,
                text: "[docs](https://example.com) and src/main.rs".to_string(),
            }],
            18,
            80,
        );
        assert!(manager.focus_panel("agent"));

        let press = |manager: &mut PanelManager, character| {
            assert!(matches!(
                manager.handle_focused_scrollback_input(
                    &Event::Key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)),
                    18,
                    80,
                    1,
                ),
                Some(TextPanelScrollbackInput::Handled)
            ));
        };
        press(&mut manager, 'G');
        press(&mut manager, ']');
        press(&mut manager, 'l');
        assert_eq!(
            manager.focused_text_link_target(80),
            Some(TextPanelLinkTarget::ExternalUrl(
                "https://example.com".to_string()
            ))
        );
        press(&mut manager, ']');
        press(&mut manager, 'l');
        assert_eq!(
            manager.focused_text_link_target(80),
            Some(TextPanelLinkTarget::File {
                path: "src/main.rs".to_string(),
                location: None,
            })
        );
        press(&mut manager, '[');
        press(&mut manager, 'l');
        assert_eq!(
            manager.focused_text_link_target(80),
            Some(TextPanelLinkTarget::ExternalUrl(
                "https://example.com".to_string()
            ))
        );

        let placement = manager.panel_at_position(40, 0, 80, 20).unwrap();
        assert_eq!(
            manager.text_action_at_position(placement.x + 1, 2, 80, 20),
            None
        );
        assert_eq!(
            manager.text_link_at_position(placement.x + 1, 2, 80, 20),
            Some(TextPanelLinkTarget::ExternalUrl(
                "https://example.com".to_string()
            ))
        );
        assert_eq!(
            manager.text_link_at_position(placement.x + 11, 2, 80, 20),
            Some(TextPanelLinkTarget::File {
                path: "src/main.rs".to_string(),
                location: None,
            })
        );

        assert_eq!(
            manager.double_clicked_text_link_at_position(placement.x + 1, 2, 80, 20),
            None
        );
        assert_eq!(
            manager.double_clicked_text_link_at_position(placement.x + 11, 2, 80, 20),
            None
        );
        assert_eq!(
            manager.double_clicked_text_link_at_position(placement.x + 1, 2, 80, 20),
            None
        );
        assert_eq!(
            manager.double_clicked_text_link_at_position(placement.x + 1, 2, 80, 20),
            Some(TextPanelLinkTarget::ExternalUrl(
                "https://example.com".to_string()
            ))
        );

        assert_eq!(
            manager.double_clicked_text_link_at_position(placement.x + 1, 2, 80, 20),
            None
        );
        manager.last_text_link_click.as_mut().unwrap().at -=
            ROW_DOUBLE_CLICK_INTERVAL + Duration::from_millis(1);
        assert_eq!(
            manager.double_clicked_text_link_at_position(placement.x + 1, 2, 80, 20),
            None
        );
    }

    #[test]
    fn composer_height_grows_with_wrapping_and_shrinks_with_the_draft() {
        let mut panel = TextPanel::new(
            "agent".into(),
            PanelConfig {
                title: Some("Agent".into()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".into(),
                    rows: 3,
                }),
                ..PanelConfig::default()
            },
        );
        assert_eq!(panel.composer_height(40, 40), 5);
        panel
            .composer
            .as_mut()
            .unwrap()
            .prompt
            .replace_draft(&"word ".repeat(40));
        let wide = panel.composer_height(40, 40);
        assert!(wide > 5);
        assert!(panel.composer_height(40, 20) > wide);
        assert_eq!(panel.composer_height(40, 8), 28);
        assert_eq!(panel.visible_rows(40, 8), 10);
        assert_eq!(panel.composer_height(7, 8), 4);
        assert_eq!(panel.visible_rows(7, 8), 1);
        assert_eq!(panel.composer_height(5, 8), 0);

        let composer = panel.composer.as_mut().unwrap();
        let first = composer.layout(20);
        assert!(Arc::ptr_eq(&first, &composer.layout(20)));
        composer.prompt.set_cursor(0);
        assert!(Arc::ptr_eq(&first, &composer.layout(20)));
        composer.prompt.replace_draft("short");
        assert!(!Arc::ptr_eq(&first, &composer.layout(20)));
        assert_eq!(panel.composer_height(40, 8), 5);
    }

    #[test]
    fn expanded_composer_render_click_cursor_and_submission_share_geometry() {
        use crossterm::event::KeyEvent;

        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".into(),
            PanelConfig {
                side: PanelSide::Right,
                width: 32,
                title: Some("Agent".into()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".into(),
                    rows: 3,
                }),
                ..PanelConfig::default()
            },
        );
        let draft = (0..30)
            .map(|line| format!("draft row {line:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(matches!(
            manager.load_text_panel_draft("agent", &draft, None),
            Ok(TextPanelReuseOutcome::Loaded)
        ));
        let placement = manager
            .panel_placements(80, 40)
            .into_iter()
            .find(|placement| placement.id == "agent")
            .unwrap();
        let height =
            manager.text_panels["agent"].composer_height(placement.height, placement.width);
        assert_eq!(height, placement.height * 70 / 100);
        let first_row = 30 - (height - COMPOSER_CHROME_ROWS);
        let x = TextPanelContentMetrics::new(placement.width).x(placement.x) + 2;
        let y = placement.y + placement.height - height + 1;
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(80, 40, &theme.style);
        manager.render(&mut buffer, &theme);
        assert_eq!(
            text_position(&buffer, &format!("draft row {first_row:02}")),
            Some(Point::new(x, y))
        );
        assert_eq!(
            text_position(&buffer, "draft row 29"),
            Some(Point::new(x, placement.y + placement.height - 2))
        );
        manager.focus_panel_at_position(x, y, 80, 40).unwrap();
        let composer = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert_eq!(composer.prompt.cursor(), first_row * "draft row 00\n".len());
        let (cursor_x, cursor_y) = manager.focused_text_panel_cursor_position(80, 40).unwrap();
        assert_eq!(cursor_x, x);
        assert!(cursor_y >= y && cursor_y < placement.y + placement.height - 1);
        // A short resized pane still reports a cursor inside its own bounds.
        if let Some((cursor_x, cursor_y)) = manager.focused_text_panel_cursor_position(24, 10) {
            assert!(cursor_x < 24 && cursor_y < 10);
        }
        let submitted = manager
            .handle_focused_text_input(
                &Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)),
                80,
            )
            .unwrap();
        assert_eq!(submitted.text.as_deref(), Some(draft.as_str()));
        assert_eq!(manager.text_panels["agent"].composer_height(40, 32), 5);
    }

    #[test]
    fn text_panel_composer_click_places_cursor_in_wrapped_text() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 32,
                title: Some("Agent".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 3,
                }),
                surface: None,
                border: None,
                header_actions: Vec::new(),
            },
        );
        assert!(manager.focus_text_panel_composer("agent"));
        manager.handle_focused_text_input(&Event::Paste("first line\nsecond line".to_string()), 80);

        let event = manager.focus_panel_at_position(54, 15, 80, 20).unwrap();
        assert_eq!(event.action, "composer_focus");
        manager.handle_focused_text_input(&Event::Paste("X".to_string()), 80);

        let composer = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert_eq!(composer.prompt.text(), "first line\nsecXond line");
    }

    #[test]
    fn word_wrapped_composer_render_navigation_and_click_use_the_same_width() {
        use crossterm::event::KeyEvent;

        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 11,
                title: None,
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 3,
                }),
                ..PanelConfig::default()
            },
        );
        // Conversation panes are never docked narrower than 32 columns, so an
        // 11-column pane is a compact pane covering an 11-column terminal.
        manager.set_presentation(PanelPresentation::Zoomed {
            id: "agent".to_string(),
            size: (11, 18),
        });
        assert!(manager.focus_text_panel_composer("agent"));
        manager.handle_focused_text_input(&Event::Paste("one two three".to_string()), 11);
        manager
            .text_panels
            .get_mut("agent")
            .unwrap()
            .composer
            .as_mut()
            .unwrap()
            .prompt
            .set_cursor(0);

        let placement = manager
            .panel_placements(11, 20)
            .into_iter()
            .find(|placement| placement.id == "agent")
            .unwrap();
        let x = TextPanelContentMetrics::new(placement.width).x(placement.x) + 2;
        let y = placement.y + placement.height
            - manager.text_panels["agent"].composer_height(placement.height, placement.width)
            + 1;
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(11, 20, &theme.style);
        manager.render(&mut buffer, &theme);
        assert_eq!(text_position(&buffer, "one two"), Some(Point::new(x, y)));
        assert_eq!(text_position(&buffer, "three"), Some(Point::new(x, y + 1)));

        let down = Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        manager.handle_focused_text_input(&down, 11);
        assert_eq!(
            manager.text_panels["agent"]
                .composer
                .as_ref()
                .unwrap()
                .prompt
                .cursor(),
            8
        );
        assert_eq!(
            manager.focused_text_panel_cursor_position(11, 20),
            Some((x, y + 1))
        );
        manager
            .focus_panel_at_position(x + 2, y + 1, 11, 20)
            .unwrap();
        manager.handle_focused_text_input(&Event::Paste("X".to_string()), 11);
        assert_eq!(
            manager.text_panels["agent"]
                .composer
                .as_ref()
                .unwrap()
                .prompt
                .text(),
            "one two thXree"
        );

        // At 18 columns the pane is compact and the editor zooms it while focused.
        manager.set_presentation(PanelPresentation::Zoomed {
            id: "agent".to_string(),
            size: (18, 18),
        });
        let (cursor_x, cursor_y) = manager.focused_text_panel_cursor_position(18, 20).unwrap();
        assert!(cursor_x < 18 && cursor_y < 20);
        let submitted = manager
            .handle_focused_text_input(
                &Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)),
                40,
            )
            .unwrap();
        assert_eq!(submitted.action, "submit");
        assert_eq!(submitted.text.as_deref(), Some("one two thXree"));
    }

    #[test]
    fn hidden_text_panel_preserves_draft_and_releases_layout() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 24,
                title: Some("Agent".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 2,
                }),
                surface: None,
                border: None,
                header_actions: Vec::new(),
            },
        );
        assert!(manager.focus_text_panel_composer("agent"));
        manager.handle_focused_text_input(&Event::Paste("keep this draft".to_string()), 80);
        manager
            .text_panels
            .get_mut("agent")
            .unwrap()
            .composer
            .as_mut()
            .unwrap()
            .prompt
            .set_cursor(5);

        assert!(manager.set_panel_visible("agent", false));
        assert_eq!(manager.reserved_right_width(200), 0);
        assert_eq!(manager.focused_panel_id(), None);
        assert!(!manager.focus_text_panel_composer("agent"));

        assert!(manager.set_panel_visible("agent", true));
        assert_eq!(
            manager.reserved_right_width(200),
            MIN_READABLE_PANE_WIDTH + 1
        );
        assert!(manager.restore_panel_focus("agent"));
        assert!(manager.focused_text_input_active());
        let composer = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert_eq!(composer.prompt.text(), "keep this draft");
        assert_eq!(composer.prompt.cursor(), 5);

        manager.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Plain,
                text: "abcdef".to_string(),
            }],
            20,
            80,
        );
        assert!(manager.focus_panel("agent"));
        manager
            .text_panels
            .get_mut("agent")
            .unwrap()
            .scrollback
            .cursor = 3;
        assert!(manager.set_panel_visible("agent", false));
        assert!(manager.set_panel_visible("agent", true));
        assert!(manager.restore_panel_focus("agent"));
        let panel = &manager.text_panels["agent"];
        assert!(panel.scrollback.focused);
        assert!(!panel.composer.as_ref().unwrap().focused);
        assert_eq!(panel.scrollback.cursor, 3);
    }

    #[test]
    fn empty_text_panel_update_resets_scroll_and_restores_tail_following() {
        let mut panel = TextPanel::new("agent".to_string(), PanelConfig::default());
        panel.update_blocks(
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Plain,
                text: "one\ntwo\nthree\nfour\nfive".to_string(),
            }],
            2,
            20,
        );
        panel.scroll_to_top();
        assert!(!panel.follow_tail);

        panel.update_blocks(Vec::new(), 2, 20);

        assert!(panel.blocks.is_empty());
        assert_eq!(panel.scroll, 0);
        assert!(panel.follow_tail);
    }

    #[test]
    fn text_panel_footer_keeps_shortcuts_visible_with_live_status() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 70,
                title: Some("Agent".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 2,
                }),
                surface: None,
                border: None,
                header_actions: Vec::new(),
            },
        );
        assert!(manager.set_text_panel_composer_state(
            "agent",
            true,
            Some("Working · 1 queued".to_string())
        ));
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(100, 15, &theme.style);

        manager.render(&mut buffer, &theme);

        assert!(row_text(&buffer, 9).contains("────"));
        assert!(!row_text(&buffer, 9).contains("a edit"));
        assert!(row_text(&buffer, 12).contains("Working · 1 queued"));
    }

    #[test]
    fn text_panel_status_row_shows_spinner_label_elapsed_and_stream_cursor() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 70,
                title: Some("Agent".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 2,
                }),
                surface: None,
                border: None,
                header_actions: Vec::new(),
            },
        );
        manager.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "agent:1".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Plain,
                text: "partial answer".to_string(),
            }],
            13,
            160,
        );
        assert!(manager.set_text_panel_status(
            "agent",
            Some(TextPanelStatus {
                busy: true,
                label: "Reading demo.txt".to_string(),
                stream: true,
            }),
        ));
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(160, 15, &theme.style);

        manager.render(&mut buffer, &theme);

        let status_row = row_text(&buffer, 8);
        assert!(status_row.contains("⠋ Reading demo.txt · 0s"));
        assert!(buffer.cells[7 * 160 + 90..8 * 160]
            .iter()
            .all(|cell| cell.text.trim().is_empty()));
        assert!(row_text(&buffer, 9).contains("────"));
        assert!((1..8).any(|row| row_text(&buffer, row).contains("partial answer▌")));

        assert!(manager.set_text_panel_status("agent", None));
        let mut buffer = RenderBuffer::new(160, 15, &theme.style);
        manager.render(&mut buffer, &theme);
        assert!(!row_text(&buffer, 8).contains("Reading demo.txt"));
        assert!((1..9).any(|row| row_text(&buffer, row).contains("partial answer")));
        assert!(!(1..9).any(|row| row_text(&buffer, row).contains("partial answer▌")));
    }

    #[test]
    fn action_blocks_support_plain_click_and_keyboard_activation() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 40,
                title: Some("Agent".to_string()),
                ..PanelConfig::default()
            },
        );
        manager.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "activity:1".to_string(),
                kind: TextPanelBlockKind::Action,
                format: TextPanelBlockFormat::Plain,
                text: "▸ Activity · 1 action".to_string(),
            }],
            18,
            80,
        );
        let placement = manager.panel_at_position(40, 0, 80, 20).unwrap();
        let expected = Some(TextPanelLinkTarget::PanelAction {
            panel_id: "agent".to_string(),
            block_id: "activity:1".to_string(),
        });
        assert_eq!(
            manager.text_action_at_position(placement.x + 1, 2, 80, 20),
            expected
        );
        assert!(manager.focus_panel("agent"));
        assert_eq!(manager.focused_text_link_target(80), expected);
    }

    #[test]
    fn action_blocks_are_truncated_trusted_links_without_extra_chrome() {
        let mut panel = TextPanel::new("agent".to_string(), PanelConfig::default());
        panel.blocks = vec![
            TextPanelBlock {
                id: "summary".to_string(),
                kind: TextPanelBlockKind::Action,
                format: TextPanelBlockFormat::Plain,
                text: "▸ Activity · 8 actions".to_string(),
            },
            TextPanelBlock {
                id: "details".to_string(),
                kind: TextPanelBlockKind::Action,
                format: TextPanelBlockFormat::Plain,
                text: "  ✓ Read a very long filename\n  View all details…".to_string(),
            },
            TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Markdown,
                text: "Answer".to_string(),
            },
        ];
        let rendered = panel.build_rendered_lines(18);
        assert_eq!(rendered.lines.len(), 5);
        assert!(rendered.lines[3].is_empty());
        for line in &rendered.lines[..3] {
            assert!(
                line.spans
                    .iter()
                    .map(|s| display_width(&s.text))
                    .sum::<usize>()
                    <= 18
            );
            assert!(line
                .spans
                .iter()
                .all(|s| matches!(s.link.as_ref().map(|l| &l.target),
                Some(TextPanelLinkTarget::PanelAction { panel_id, .. }) if panel_id == "agent")));
        }
        assert!(rendered.lines[4].spans.iter().any(|s| s.text == "Answer"));
        assert_eq!(panel.links(18).len(), 2);
    }

    #[test]
    fn busy_status_reserves_spacing_after_every_block_kind() {
        for kind in [
            TextPanelBlockKind::Agent,
            TextPanelBlockKind::Activity,
            TextPanelBlockKind::Action,
            TextPanelBlockKind::Error,
        ] {
            let mut panel = TextPanel::new("agent".to_string(), PanelConfig::default());
            panel.blocks = vec![TextPanelBlock {
                id: "tail".to_string(),
                kind,
                format: TextPanelBlockFormat::Plain,
                text: "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight".to_string(),
            }];
            panel.set_status(Some(TextPanelStatus {
                busy: true,
                label: "Working".to_string(),
                stream: false,
            }));
            panel.scroll_to_bottom(8, 40);
            let theme = Theme::default();
            let mut buffer = RenderBuffer::new(40, 8, &theme.style);
            render_text_panel(&mut buffer, &panel, Point::new(0, 0), 40, 8, &theme);
            assert!(row_text(&buffer, 5).contains("eight"));
            assert!(row_text(&buffer, 6).trim().is_empty());
            assert!(row_text(&buffer, 7).contains("Working"));
            assert!(!panel.layout(40).rendered.last().unwrap().is_empty());
        }
    }

    #[test]
    fn busy_status_leaves_a_blank_row_after_a_trailing_user_prompt() {
        let mut panel = TextPanel::new(
            "agent".to_string(),
            PanelConfig {
                width: 40,
                ..PanelConfig::default()
            },
        );
        panel.blocks = vec![
            TextPanelBlock {
                id: "agent:1".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Plain,
                text: "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine".to_string(),
            },
            TextPanelBlock {
                id: "user:2".to_string(),
                kind: TextPanelBlockKind::User,
                format: TextPanelBlockFormat::Plain,
                text: "follow up".to_string(),
            },
        ];

        assert!(!panel
            .layout(40)
            .rendered
            .last()
            .is_some_and(RenderedTextLine::is_empty));

        panel.set_status(Some(TextPanelStatus {
            busy: true,
            label: "Waiting for agent…".to_string(),
            stream: false,
        }));
        assert!(!panel
            .layout(40)
            .rendered
            .last()
            .is_some_and(RenderedTextLine::is_empty));
        panel.scroll_to_bottom(8, 40);

        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(40, 8, &theme.style);
        render_text_panel(&mut buffer, &panel, Point::new(0, 0), 40, 8, &theme);

        let status_y = text_position(&buffer, "Waiting for agent…").unwrap().y;
        assert_eq!(status_y, 7);
        assert!(row_text(&buffer, status_y - 1).trim().is_empty());
        assert_eq!(
            row_text(&buffer, status_y - 2).trim(),
            format!("╵{}╵", "▀".repeat(36))
        );
        assert!(row_text(&buffer, status_y - 3).contains("│ follow up"));

        panel.set_status(Some(TextPanelStatus {
            busy: false,
            label: "Ready".to_string(),
            stream: false,
        }));
        assert!(!panel
            .layout(40)
            .rendered
            .last()
            .is_some_and(RenderedTextLine::is_empty));
    }

    #[test]
    fn text_panel_states_preserve_prompt_surfaces_and_foreground_hierarchy() {
        let surface_foreground = Color::Rgb {
            r: 225,
            g: 230,
            b: 235,
        };
        let surface_background = Color::Rgb {
            r: 18,
            g: 20,
            b: 24,
        };
        let accent = Color::Rgb {
            r: 80,
            g: 190,
            b: 240,
        };
        let secondary = Color::Rgb {
            r: 180,
            g: 185,
            b: 195,
        };
        let muted = Color::Rgb {
            r: 130,
            g: 135,
            b: 145,
        };
        let error = Color::Rgb {
            r: 255,
            g: 105,
            b: 115,
        };
        let mut theme = Theme::default();
        theme
            .colors
            .insert("panel.foreground".to_string(), surface_foreground);
        theme
            .colors
            .insert("panel.background".to_string(), surface_background);
        theme
            .colors
            .insert("textLink.foreground".to_string(), accent);
        theme
            .colors
            .insert("descriptionForeground".to_string(), secondary);
        theme
            .colors
            .insert("editorLineNumber.foreground".to_string(), muted);
        theme
            .colors
            .insert("editorError.foreground".to_string(), error);
        theme.ui_style.popup.bg = Some(Color::Rgb { r: 70, g: 0, b: 0 });
        theme.ui_style.picker_prompt.bg = Some(Color::Rgb { r: 0, g: 70, b: 0 });
        theme.ui_style.dialog.bg = Some(Color::Rgb { r: 0, g: 0, b: 70 });
        theme.ui_style.muted.bg = Some(Color::Rgb { r: 70, g: 70, b: 0 });
        theme.ui_style.deprecated.bg = Some(Color::Rgb { r: 70, g: 0, b: 70 });

        let config = PanelConfig {
            side: PanelSide::Right,
            width: 72,
            title: Some("Agent".to_string()),
            composer: Some(TextPanelComposerConfig {
                placeholder: "Ask a follow-up…".to_string(),
                rows: 3,
            }),
            header_actions: Vec::new(),
            surface: Some(ThemeStyleSpec {
                foreground: vec!["panel.foreground".to_string()],
                background: vec!["panel.background".to_string()],
                bold: None,
                italic: None,
            }),
            border: None,
        };
        let palette = text_panel_palette(&theme, &config);
        let prompt_palette = text_panel_prompt_palette(&theme, &palette, true);
        let assert_surfaces = |buffer: &RenderBuffer| {
            let label_y = text_position(buffer, "You").unwrap().y;
            let body_y = text_position(buffer, "question").unwrap().y;
            assert_eq!(body_y, label_y + 1);
            assert_ne!(prompt_palette.content.surface.bg, Some(surface_background));
            for y in 0..buffer.height {
                for x in 0..buffer.width {
                    let expected =
                        if (label_y..=body_y).contains(&y) && (2..buffer.width - 2).contains(&x) {
                            prompt_palette.content.surface.bg
                        } else {
                            Some(surface_background)
                        };
                    assert_eq!(
                        buffer.cells[y * buffer.width + x].style.bg,
                        expected,
                        "unexpected background at ({x}, {y})"
                    );
                }
            }
            assert_eq!(text_style(buffer, "▄▄").fg, prompt_palette.cap.fg);
            assert_eq!(text_style(buffer, "▀▀").fg, prompt_palette.cap.fg);
            assert_eq!(text_style(buffer, "╷").fg, prompt_palette.edge.fg);
            assert_eq!(text_style(buffer, "╵").fg, prompt_palette.edge.fg);
            assert_eq!(
                text_style(buffer, "You").fg,
                prompt_palette.content.accent.fg
            );
        };
        let mut panel = TextPanel::new("agent".to_string(), config);
        panel.blocks = vec![
            TextPanelBlock {
                id: "user".to_string(),
                kind: TextPanelBlockKind::User,
                format: TextPanelBlockFormat::Plain,
                text: "question".to_string(),
            },
            TextPanelBlock {
                id: "activity".to_string(),
                kind: TextPanelBlockKind::Activity,
                format: TextPanelBlockFormat::Plain,
                text: "Worked for 13s".to_string(),
            },
            TextPanelBlock {
                id: "agent".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Plain,
                text: "partial answer".to_string(),
            },
            TextPanelBlock {
                id: "error".to_string(),
                kind: TextPanelBlockKind::Error,
                format: TextPanelBlockFormat::Plain,
                text: "startup failed".to_string(),
            },
        ];
        let composer = panel.composer.as_mut().unwrap();
        assert!(composer.prompt.set_text("first line\nsecond line"));
        composer.focused = true;
        panel.set_status(Some(TextPanelStatus {
            busy: true,
            label: "Reading demo.txt".to_string(),
            stream: true,
        }));
        let mut streaming = RenderBuffer::new(72, 22, &theme.style);

        render_text_panel(&mut streaming, &panel, Point::new(0, 0), 72, 22, &theme);

        assert_surfaces(&streaming);
        assert_eq!(
            text_style(&streaming, "Worked for 13s").fg,
            palette.muted.fg
        );
        assert_eq!(
            text_style(&streaming, "partial answer").fg,
            palette.primary.fg
        );
        assert_eq!(
            text_style(&streaming, "startup failed").fg,
            palette.error.fg
        );
        assert_eq!(
            text_style(&streaming, "Reading demo.txt").fg,
            palette.primary.fg
        );
        assert_eq!(text_style(&streaming, "0s").fg, palette.muted.fg);
        assert_eq!(text_style(&streaming, "second line").fg, palette.primary.fg);
        assert_eq!(text_style(&streaming, "INSERT").fg, palette.accent.fg);
        assert_eq!(text_style(&streaming, "^J").fg, palette.secondary.fg);
        assert!(text_style(&streaming, "^J").bold);
        assert_eq!(text_style(&streaming, "newline").fg, palette.muted.fg);
        assert_eq!(
            streaming
                .cells
                .iter()
                .filter(|cell| cell.text == "›")
                .count(),
            1
        );
        assert!(streaming.cells.iter().any(|cell| cell.text == "│"));

        panel.set_status(None);
        panel.composer.as_mut().unwrap().focused = false;
        panel.scrollback.focused = true;
        panel.scrollback.mode = TextPanelScrollbackMode::Normal;
        let mut scrollback = RenderBuffer::new(72, 22, &theme.style);
        render_text_panel(&mut scrollback, &panel, Point::new(0, 0), 72, 22, &theme);

        assert_surfaces(&scrollback);
        assert_eq!(
            text_style(&scrollback, "SCROLLBACK NORMAL").fg,
            palette.accent.fg
        );
        assert_eq!(text_style(&scrollback, "Tab").fg, palette.secondary.fg);
        assert_eq!(text_style(&scrollback, "edit").fg, palette.muted.fg);
    }

    #[test]
    fn text_panel_insets_content_and_separates_header_chrome() {
        let config = PanelConfig {
            title: Some("Agent".to_string()),
            composer: Some(TextPanelComposerConfig {
                placeholder: "Ask".to_string(),
                rows: 2,
            }),
            header_actions: vec![TextPanelHeaderAction {
                id: "close".to_string(),
                label: "×".to_string(),
                compact_label: Some("×".to_string()),
                ..TextPanelHeaderAction::default()
            }],
            ..PanelConfig::default()
        };
        let mut panel = TextPanel::new("agent".to_string(), config);
        panel.blocks = vec![TextPanelBlock {
            id: "agent:1".to_string(),
            kind: TextPanelBlockKind::Agent,
            format: TextPanelBlockFormat::Markdown,
            text: "body".to_string(),
        }];
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(40, 12, &theme.style);

        render_text_panel(&mut buffer, &panel, Point::new(0, 0), 40, 12, &theme);

        assert_eq!(text_position(&buffer, "Agent"), Some(Point::new(1, 0)));
        assert!(buffer.cells[40..80].iter().all(|cell| cell.text == "─"));
        assert_eq!(text_position(&buffer, "◆ Agent"), None);
        assert_eq!(text_position(&buffer, "body"), Some(Point::new(1, 2)));
        let composer_divider = (12 - panel.composer_height(12, 40)) * 40;
        assert!(buffer.cells[composer_divider..composer_divider + 40]
            .iter()
            .all(|cell| cell.text == "─"));
        assert_eq!(TextPanelContentMetrics::new(2).inset, 0);
        assert_eq!(TextPanelContentMetrics::new(2).width, 2);
    }

    #[test]
    fn text_panel_surface_is_stable_across_dark_light_and_high_contrast_themes() {
        for path in [
            "themes/tokyonight-storm.json",
            "themes/night-owl-light.json",
            "themes/community-material-theme-lighter-high-contrast.json",
        ] {
            let theme = parse_vscode_theme(path).unwrap();
            let config = PanelConfig {
                side: PanelSide::Right,
                width: 40,
                title: Some("Agent".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 3,
                }),
                ..PanelConfig::default()
            };
            let palette = text_panel_palette(&theme, &config);
            let mut panel = TextPanel::new("agent".to_string(), config);
            panel.blocks = vec![TextPanelBlock {
                id: "activity".to_string(),
                kind: TextPanelBlockKind::Activity,
                format: TextPanelBlockFormat::Plain,
                text: "Worked for 13s".to_string(),
            }];
            panel.composer.as_mut().unwrap().focused = true;
            panel.set_status(Some(TextPanelStatus {
                busy: true,
                label: "Thinking".to_string(),
                stream: true,
            }));
            let mut buffer = RenderBuffer::new(40, 12, &theme.style);

            render_text_panel(&mut buffer, &panel, Point::new(0, 0), 40, 12, &theme);

            assert!(
                buffer
                    .cells
                    .iter()
                    .all(|cell| cell.style.bg == palette.surface.bg),
                "text panel backgrounds diverged for {path}"
            );
            let background = palette.surface.bg.unwrap();
            assert!(contrast_ratio(palette.primary.fg.unwrap(), background) >= 4.5);
            assert!(contrast_ratio(palette.secondary.fg.unwrap(), background) >= 4.5);
            assert!(contrast_ratio(palette.accent.fg.unwrap(), background) >= 4.5);
            assert!(contrast_ratio(palette.muted.fg.unwrap(), background) >= 3.0);
        }
    }

    #[test]
    fn narrow_text_panel_footer_drops_whole_low_priority_shortcuts() {
        let mut panel = TextPanel::new(
            "agent".to_string(),
            PanelConfig {
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 2,
                }),
                ..PanelConfig::default()
            },
        );
        panel.composer.as_mut().unwrap().focused = true;
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(26, 6, &theme.style);

        render_text_panel(&mut buffer, &panel, Point::new(0, 0), 26, 6, &theme);

        let footer = row_text(&buffer, 5).trim_end().to_string();
        assert!(footer.contains("↵ send"), "{footer:?}");
        assert!(footer.contains("Esc normal"), "{footer:?}");
        assert!(!footer.contains("Ctrl+Ent"));
        assert!(!footer.ends_with('·'));
    }

    #[test]
    fn activity_blocks_render_muted_without_a_label_between_turns() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 40,
                title: None,
                composer: None,
                surface: None,
                border: None,
                header_actions: Vec::new(),
            },
        );
        manager.update_text_panel(
            "agent",
            vec![
                TextPanelBlock {
                    id: "user:1".to_string(),
                    kind: TextPanelBlockKind::User,
                    format: TextPanelBlockFormat::Plain,
                    text: "first".to_string(),
                },
                TextPanelBlock {
                    id: "activity:2".to_string(),
                    kind: TextPanelBlockKind::Activity,
                    format: TextPanelBlockFormat::Plain,
                    text: "✓ Read demo.txt".to_string(),
                },
                TextPanelBlock {
                    id: "user:3".to_string(),
                    kind: TextPanelBlockKind::User,
                    format: TextPanelBlockFormat::Plain,
                    text: "second".to_string(),
                },
            ],
            20,
            60,
        );
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(60, 22, &theme.style);

        manager.render(&mut buffer, &theme);

        let rendered = (0..22)
            .map(|row| row_text(&buffer, row))
            .collect::<Vec<_>>();
        let joined = rendered.join("\n");
        assert_eq!(joined.matches("│ You").count(), 2);
        assert_eq!(joined.matches("╷▄▄").count(), 2);
        assert_eq!(joined.matches("╵▀▀").count(), 2);
        assert!(joined.contains("✓ Read demo.txt"));
        let palette = text_panel_palette(&theme, &manager.text_panels["agent"].config);
        assert_eq!(text_style(&buffer, "✓ Read demo.txt").fg, palette.muted.fg);
        let first_y = text_position(&buffer, "first").unwrap().y;
        let activity_y = text_position(&buffer, "✓ Read demo.txt").unwrap().y;
        let second_y = text_position(&buffer, "second").unwrap().y;
        assert!(first_y < activity_y && activity_y < second_y);
        assert!(!joined.contains("▎ You"));
        assert!(!joined.contains("❯ You"));
        let separator_rows = rendered.iter().filter(|row| row.contains("────")).count();
        assert_eq!(separator_rows, 0);
    }

    #[test]
    fn text_panel_append_follows_tail_until_user_scrolls() {
        let mut panel = TextPanel::new(
            "agent".to_string(),
            PanelConfig {
                width: 8,
                title: None,
                ..PanelConfig::default()
            },
        );
        panel.update_blocks(
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Plain,
                text: "one\ntwo\nthree".to_string(),
            }],
            2,
            8,
        );
        let tail = panel.scroll;

        panel.append_delta("answer", "\nfour", 2, 8);
        assert!(panel.scroll > tail);
        assert!(panel.follow_tail);

        panel.scroll_to_top();
        panel.append_delta("answer", "\nfive", 2, 8);
        assert_eq!(panel.scroll, 0);
        assert!(!panel.follow_tail);
    }

    #[test]
    fn text_panel_append_creates_missing_agent_block_as_markdown() {
        let mut panel = TextPanel::new("agent".to_string(), PanelConfig::default());

        panel.append_delta("answer", "# Heading", 10, 40);

        assert_eq!(panel.blocks.len(), 1);
        assert_eq!(panel.blocks[0].id, "answer");
        assert_eq!(panel.blocks[0].kind, TextPanelBlockKind::Agent);
        assert_eq!(panel.blocks[0].format, TextPanelBlockFormat::Markdown);
        assert_eq!(panel.blocks[0].text, "# Heading");
    }

    #[test]
    fn focused_text_panel_supports_scrolling_and_preserves_manual_position() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 16,
                title: Some("Agent".to_string()),
                composer: None,
                surface: None,
                border: None,
                header_actions: Vec::new(),
            },
        );
        manager.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Plain,
                text: "one\ntwo\nthree\nfour\nfive\nsix\nseven".to_string(),
            }],
            4,
            16,
        );
        assert!(manager.focus_panel("agent"));
        assert_eq!(manager.reserved_right_width(200), 17);

        let top = manager.handle_focused_key("top", 4, 16, 0).unwrap();
        assert_eq!(top.selected_index, 0);
        assert!(top.row.is_none());
        manager.append_text_panel("agent", "answer", "\neight", 4, 16);
        assert_eq!(manager.text_panels["agent"].scroll, 0);
        assert!(!manager.text_panels["agent"].follow_tail);

        let page = manager.handle_focused_key("page_down", 4, 16, 0).unwrap();
        assert!(page.selected_index > 0);
        let bottom = manager.handle_focused_key("bottom", 4, 16, 0).unwrap();
        assert!(bottom.selected_index >= page.selected_index);
        assert!(manager.text_panels["agent"].follow_tail);
    }

    #[test]
    fn text_panel_footer_makes_offscreen_restored_history_discoverable() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 62,
                title: Some("Agent".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 3,
                }),
                ..PanelConfig::default()
            },
        );
        manager.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "restored".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Plain,
                text: (1..=30)
                    .map(|line| format!("restored line {line}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            }],
            15,
            140,
        );
        assert!(manager.focus_panel("agent"));
        manager.handle_focused_key("top", 15, 140, 0).unwrap();

        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(140, 15, &theme.style);
        manager.render(&mut buffer, &theme);
        let top = (0..15)
            .map(|row| row_text(&buffer, row))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(top.contains("SCROLLBACK NORMAL · Tab edit"));

        manager.handle_focused_key("bottom", 15, 140, 0).unwrap();
        let mut buffer = RenderBuffer::new(140, 15, &theme.style);
        manager.render(&mut buffer, &theme);
        let bottom = (0..15)
            .map(|row| row_text(&buffer, row))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(bottom.contains("SCROLLBACK NORMAL · Tab edit"));
    }

    #[test]
    fn text_panel_render_reflows_to_actual_width_and_keeps_latest_line_visible() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 52,
                title: Some("Agent".to_string()),
                composer: None,
                surface: None,
                border: None,
                header_actions: Vec::new(),
            },
        );
        manager.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Plain,
                text: "one\ntwo\nthree\nfour\nfive\nsix\nLATEST".to_string(),
            }],
            6,
            14,
        );
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(14, 8, &theme.style);

        manager.render(&mut buffer, &theme);

        assert_eq!(row_text(&buffer, 0).trim(), "Agent");
        assert!((1..6).any(|row| row_text(&buffer, row).contains("LATEST")));
    }

    #[test]
    fn right_text_panel_places_separator_to_its_left() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 8,
                title: None,
                composer: None,
                surface: None,
                border: None,
                header_actions: Vec::new(),
            },
        );
        manager.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "user".to_string(),
                kind: TextPanelBlockKind::User,
                format: TextPanelBlockFormat::Plain,
                text: "hello".to_string(),
            }],
            5,
            16,
        );
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(16, 7, &theme.style);

        manager.render(&mut buffer, &theme);

        assert!(row_text(&buffer, 0).contains("│ ▄▄▄▄▄▄"));
        assert!(row_text(&buffer, 1).contains("│ You"));
        assert!(row_text(&buffer, 2).contains("│ hello"));
        assert!(row_text(&buffer, 3).contains("│ ▀▀▀▀▀▀"));
        assert!(manager.panel_at_position(7, 0, 16, 7).is_none());
        assert!(manager.panel_at_position(8, 0, 16, 7).is_some());
    }

    #[test]
    fn focused_panel_moves_selection() {
        let mut manager = PanelManager::default();
        manager.create_panel("tree".to_string(), PanelConfig::default());
        manager.update_panel("tree", vec![row("a"), row("b")]);
        assert!(manager.focus_panel("tree"));

        let event = manager.handle_focused_key("down", 10, 80, 0).unwrap();
        assert_eq!(event.selected_index, 1);
        assert_eq!(event.row.unwrap().id, "b");
    }

    #[test]
    fn row_panel_search_edits_query_navigates_and_preserves_filter() {
        use crossterm::event::KeyEvent;

        let mut manager = PanelManager::default();
        manager.create_panel("tree".to_string(), PanelConfig::default());
        manager.update_panel("tree", vec![row("first"), row("second")]);
        assert!(manager.open_panel_search("tree", "", "/"));
        assert!(manager.focused_row_search_active());
        assert_eq!(manager.command_mode_keymap(), None);

        let typed = manager
            .handle_focused_row_search(
                &Event::Key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE)),
                30,
                12,
            )
            .unwrap();
        assert_eq!(typed.action, "search_query");
        assert_eq!(typed.text.as_deref(), Some("p"));

        let moved = manager
            .handle_focused_row_search(
                &Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
                30,
                12,
            )
            .unwrap();
        assert_eq!(moved.action, "search_move");
        assert_eq!(moved.row.unwrap().id, "second");

        let submitted = manager
            .handle_focused_row_search(
                &Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                30,
                12,
            )
            .unwrap();
        assert_eq!(submitted.action, "search_submit");
        assert_eq!(submitted.text.as_deref(), Some("p"));

        assert!(manager.update_panel_search("tree", "1".to_string()));
        assert!(manager.keep_panel_search("tree"));
        assert!(!manager.focused_row_search_active());
        assert_eq!(
            manager.command_mode_keymap(),
            Some(crate::editor::Mode::Normal)
        );
        assert!(manager.close_panel_search("tree"));
    }

    #[test]
    fn row_panel_status_preserves_rows_and_cannot_activate_hidden_files() {
        let mut manager = PanelManager::default();
        manager.create_panel("tree".to_string(), PanelConfig::default());
        manager.update_panel("tree", vec![row("first"), row("second"), row("third")]);
        manager.set_panel_status("tree", "Updating Git status…".to_string());
        manager.open_panel_search("tree", "pick", "/");
        let mut buffer = RenderBuffer::new(30, 5, &Style::default());
        manager.render(&mut buffer, &Theme::default());
        assert_eq!(row_text(&buffer, 0).trim(), "first");
        assert_eq!(row_text(&buffer, 1).trim(), "Updating Git status…");
        assert!(row_text(&buffer, 2).starts_with("/pick"));
        assert!(manager.focus_panel_at_position(1, 1, 30, 5).is_none());
        assert!(manager.focus_panel_at_position(1, 1, 30, 5).is_none());
        assert_eq!(manager.panels["tree"].selected, 0);
        manager.set_panel_status("tree", String::new());
        manager.render(&mut buffer, &Theme::default());
        assert_eq!(row_text(&buffer, 1).trim(), "second");
    }

    #[test]
    fn row_panel_search_renders_at_bottom_and_reserves_a_content_row() {
        let mut manager = PanelManager::default();
        manager.create_panel("tree".to_string(), PanelConfig::default());
        manager.update_panel("tree", vec![row("first"), row("second"), row("third")]);
        assert!(manager.open_panel_search("tree", "pick", "/"));
        assert!(manager.update_panel_search("tree", "3".to_string()));

        let style = Style::default();
        let theme = Theme {
            style: style.clone(),
            ..Theme::default()
        };
        let mut buffer = RenderBuffer::new(20, 5, &style);
        manager.render(&mut buffer, &theme);

        assert_eq!(row_text(&buffer, 0).trim(), "first");
        assert_eq!(row_text(&buffer, 1).trim(), "second");
        assert!(row_text(&buffer, 2).starts_with("/pick"));
        assert!(row_text(&buffer, 2).trim_end().ends_with('3'));
        assert!(manager.focused_text_panel_cursor_position(20, 5).is_some());
    }

    #[test]
    fn double_clicking_a_row_activates_it_once() {
        let mut manager = PanelManager::default();
        manager.create_panel("tree".to_string(), PanelConfig::default());
        manager.update_panel("tree", vec![row("a"), row("b")]);

        let first = manager.focus_panel_at_position(1, 1, 80, 20).unwrap();
        assert_eq!(first.action, "select");
        assert_eq!(first.row.unwrap().id, "b");

        let second = manager.focus_panel_at_position(2, 1, 80, 20).unwrap();
        assert_eq!(second.action, "activate");
        assert_eq!(second.row.unwrap().id, "b");

        let third = manager.focus_panel_at_position(1, 1, 80, 20).unwrap();
        assert_eq!(third.action, "select");
    }

    #[test]
    fn row_double_click_requires_the_same_row_within_its_time_window() {
        let mut manager = PanelManager::default();
        manager.create_panel("tree".to_string(), PanelConfig::default());
        manager.update_panel("tree", vec![row("a"), row("b")]);

        manager.focus_panel_at_position(1, 0, 80, 20).unwrap();
        let different_row = manager.focus_panel_at_position(1, 1, 80, 20).unwrap();
        assert_eq!(different_row.action, "select");

        manager.last_row_click.as_mut().unwrap().at -=
            ROW_DOUBLE_CLICK_INTERVAL + Duration::from_millis(1);
        let expired = manager.focus_panel_at_position(1, 1, 80, 20).unwrap();
        assert_eq!(expired.action, "select");
    }

    #[test]
    fn clicking_panel_header_or_empty_space_cannot_activate_a_row() {
        let mut manager = PanelManager::default();
        manager.create_panel(
            "tree".to_string(),
            PanelConfig {
                title: Some("Tree".to_string()),
                ..PanelConfig::default()
            },
        );
        manager.update_panel("tree", vec![row("a")]);

        for screen_row in [0, 0, 2, 2] {
            let event = manager
                .focus_panel_at_position(1, screen_row, 80, 20)
                .unwrap();
            assert_eq!(event.action, "select");
        }
    }

    #[test]
    fn focused_panel_scrolls_when_selection_moves_below_viewport() {
        let mut manager = PanelManager::default();
        manager.create_panel("tree".to_string(), PanelConfig::default());
        manager.update_panel("tree", vec![row("a"), row("b"), row("c"), row("d")]);
        assert!(manager.focus_panel("tree"));

        manager.handle_focused_key("down", 3, 80, 0).unwrap();
        manager.handle_focused_key("down", 3, 80, 0).unwrap();
        let event = manager.handle_focused_key("down", 3, 80, 0).unwrap();

        assert_eq!(event.selected_index, 3);
        assert_eq!(manager.panels["tree"].scroll, 1);

        let style = Style::default();
        let theme = Theme {
            style: style.clone(),
            ..Theme::default()
        };
        let mut buffer = RenderBuffer::new(10, 5, &style);
        manager.render(&mut buffer, &theme);
        assert_eq!(row_text(&buffer, 2).trim(), "d");
    }

    #[test]
    fn custom_tree_updates_preserve_selection_and_existing_panel_actions() {
        let mut spec = super::super::TreePanelSpec {
            root: PanelRow {
                kind: PanelRowKind::Directory,
                ..row("symbols")
            },
            children: vec![super::super::TreePanelChildren {
                parent: "symbols".into(),
                rows: vec![row("fn:first"), row("fn:second")],
            }],
            expanded: vec!["symbols".into()],
        };
        let mut manager = PanelManager::default();
        manager.create_panel("outline".into(), PanelConfig::default());
        manager.update_tree_panel("outline", TreePanelModel::new(spec.clone()).unwrap());
        manager.focus_panel("outline");
        manager.handle_focused_key("bottom", 5, 40, 0).unwrap();
        spec.children[0].rows.reverse();
        manager.update_tree_panel("outline", TreePanelModel::new(spec).unwrap());
        let event = manager.handle_focused_key("activate", 5, 40, 0).unwrap();
        assert_eq!(event.row.unwrap().id, "fn:second");
        assert_eq!(event.selected_index, 1);
        let click = manager.focus_panel_at_position(1, 2, 40, 7).unwrap();
        assert_eq!(click.row.unwrap().id, "fn:first");
    }

    #[test]
    fn virtual_tree_rows_remain_selectable_and_render_only_the_viewport() {
        let entries = (0..1_024)
            .map(|index| {
                serde_json::json!({
                    "name": format!("file-{index:04}.rs"),
                    "path": format!("./file-{index:04}.rs"),
                    "kind": "file",
                })
            })
            .collect::<Vec<_>>();
        let model = TreePanelModel::from_husk_values(&[
            husk_runtime::Value::String("/repo".to_string()),
            husk_runtime::Value::from_json(serde_json::json!([{
                "path": ".",
                "entries": entries,
            }])),
            husk_runtime::Value::from_json(serde_json::json!(["."])),
            husk_runtime::Value::from_json(serde_json::json!([])),
            husk_runtime::Value::from_json(serde_json::json!([])),
            husk_runtime::Value::String(String::new()),
            husk_runtime::Value::from_json(serde_json::json!([])),
        ])
        .unwrap();
        let mut manager = PanelManager::default();
        manager.create_panel(
            "tree".to_string(),
            PanelConfig {
                width: 24,
                ..PanelConfig::default()
            },
        );
        manager.update_tree_panel("tree", model);
        assert!(manager.focus_panel("tree"));

        let event = manager.handle_focused_key("bottom", 5, 40, 0).unwrap();
        assert_eq!(event.selected_index, 1_024);
        assert_eq!(event.row.unwrap().id, "./file-1023.rs");
        assert!(manager.panels["tree"].rows.is_empty());

        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(40, 7, &theme.style);
        manager.render(&mut buffer, &theme);
        assert!((0..5).any(|row| row_text(&buffer, row).contains("file-1023.rs")));
    }

    #[test]
    fn focused_panel_pages_rows_with_two_lines_of_overlap() {
        let mut manager = PanelManager::default();
        manager.create_panel(
            "tree".to_string(),
            PanelConfig {
                title: Some("Tree".to_string()),
                ..PanelConfig::default()
            },
        );
        manager.update_panel(
            "tree",
            (0..20).map(|index| row(&index.to_string())).collect(),
        );
        assert!(manager.focus_panel("tree"));

        let first = manager.handle_focused_key("page_down", 7, 80, 2).unwrap();
        assert_eq!(manager.panels["tree"].scroll, 4);
        assert_eq!(first.selected_index, 6);

        let second = manager.handle_focused_key("page_down", 7, 80, 2).unwrap();
        assert_eq!(manager.panels["tree"].scroll, 8);
        assert_eq!(second.selected_index, 10);

        let previous = manager.handle_focused_key("page_up", 7, 80, 2).unwrap();
        assert_eq!(manager.panels["tree"].scroll, 4);
        assert_eq!(previous.selected_index, 7);

        let bottom = manager.handle_focused_key("bottom", 7, 80, 2).unwrap();
        assert_eq!(manager.panels["tree"].scroll, 14);
        assert_eq!(bottom.selected_index, 19);

        let top = manager.handle_focused_key("top", 7, 80, 2).unwrap();
        assert_eq!(manager.panels["tree"].scroll, 0);
        assert_eq!(top.selected_index, 0);
    }

    #[test]
    fn mouse_scroll_moves_hovered_panel_without_changing_focus() {
        let mut manager = PanelManager::default();
        manager.create_panel("tree".to_string(), PanelConfig::default());
        manager.create_panel(
            "other".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                ..PanelConfig::default()
            },
        );
        manager.update_panel(
            "tree",
            (0..20).map(|index| row(&index.to_string())).collect(),
        );
        manager.update_panel("other", vec![row("other")]);
        assert!(manager.focus_panel("other"));

        let down = manager.handle_mouse_scroll("tree", 3, 6, 80, 1).unwrap();
        assert_eq!(manager.focused_panel_id(), Some("other"));
        assert_eq!(manager.panels["tree"].scroll, 3);
        assert_eq!(down.selected_index, 4);

        let up = manager.handle_mouse_scroll("tree", -2, 6, 80, 1).unwrap();
        assert_eq!(manager.panels["tree"].scroll, 1);
        assert_eq!(up.selected_index, 4);
    }

    #[test]
    fn row_panel_scroll_handles_empty_short_and_tiny_viewports() {
        let mut panel = PluginPanel::new("tree".to_string(), PanelConfig::default());

        panel.scroll_view(3, 4, 3);
        assert_eq!((panel.selected, panel.scroll), (0, 0));

        panel.update_rows(vec![row("a"), row("b")]);
        panel.scroll_view(3, 4, 3);
        assert_eq!((panel.selected, panel.scroll), (0, 0));

        panel.update_rows((0..5).map(|index| row(&index.to_string())).collect());
        panel.page_scroll(1, 1, usize::MAX);
        assert_eq!((panel.selected, panel.scroll), (1, 1));
        panel.page_scroll(-1, 1, usize::MAX);
        assert_eq!((panel.selected, panel.scroll), (0, 0));
    }

    #[test]
    fn update_rows_clamps_scroll_to_remaining_rows() {
        let mut panel = PluginPanel::new("tree".to_string(), PanelConfig::default());
        panel.update_rows((0..10).map(|i| row(&i.to_string())).collect());
        panel.selected = 8;
        panel.scroll = 6;

        panel.update_rows(vec![row("a"), row("b")]);

        assert_eq!(panel.selected, 1);
        assert_eq!(panel.scroll, 1);
    }

    #[test]
    fn select_row_by_id_scrolls_target_into_view() {
        let mut panel = PluginPanel::new("tree".to_string(), PanelConfig::default());
        panel.update_rows((0..10).map(|i| row(&i.to_string())).collect());

        assert!(panel.select_row_by_id("8", 5));

        assert_eq!(panel.selected, 8);
        assert_eq!(panel.scroll, 4);
    }

    #[test]
    fn select_row_by_id_preserves_selection_when_missing() {
        let mut panel = PluginPanel::new("tree".to_string(), PanelConfig::default());
        panel.update_rows(vec![row("a"), row("b")]);
        panel.selected = 1;

        assert!(!panel.select_row_by_id("missing", 10));

        assert_eq!(panel.selected, 1);
    }

    #[test]
    fn select_row_by_id_preserves_first_duplicate_and_recovers_from_hash_collisions() {
        let mut panel = PluginPanel::new("tree".to_string(), PanelConfig::default());
        panel.update_rows(vec![row("duplicate"), row("other"), row("duplicate")]);

        assert!(panel.select_row_by_id("duplicate", 10));
        assert_eq!(panel.selected, 0);

        panel.row_positions.insert(path_hash("duplicate"), 1);
        assert!(panel.select_row_by_id("duplicate", 10));
        assert_eq!(panel.selected, 0);
    }

    #[test]
    fn render_panel_right_aligns_badges() {
        let mut panel = PluginPanel::new("tree".to_string(), PanelConfig::default());
        let mut row = row("src");
        row.right_segments.push(PanelSegment {
            text: "M".to_string(),
            style: None,
            semantic: None,
        });
        panel.update_rows(vec![row]);

        let style = Style::default();
        let theme = Theme {
            style: style.clone(),
            ..Theme::default()
        };
        let mut buffer = RenderBuffer::new(10, 5, &style);
        render_panel(&mut buffer, &panel, Point::new(0, 0), 10, &theme);

        assert_eq!(row_text(&buffer, 0), "src     M ");
    }

    #[test]
    fn semantic_panel_segment_resolves_theme_color() {
        let directory_color = Color::Rgb {
            r: 137,
            g: 180,
            b: 250,
        };
        let mut theme = Theme::default();
        theme
            .colors
            .insert("symbolIcon.folderForeground".to_string(), directory_color);
        let mut panel = PluginPanel::new("tree".to_string(), PanelConfig::default());
        let mut directory_row = row("src");
        directory_row.segments[0].semantic = Some(ThemeStyleSpec {
            foreground: vec!["symbolIcon.folderForeground".to_string()],
            ..ThemeStyleSpec::default()
        });
        panel.update_rows(vec![row("other"), directory_row]);
        let mut buffer = RenderBuffer::new(10, 5, &theme.style);

        render_panel(&mut buffer, &panel, Point::new(0, 0), 10, &theme);

        assert_eq!(buffer.cells[10].style.fg, Some(directory_color));
    }

    #[test]
    fn render_panel_fills_selected_row() {
        let mut panel = PluginPanel::new("tree".to_string(), PanelConfig::default());
        panel.update_rows(vec![row("src")]);

        let style = Style {
            fg: Some(Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            }),
            bg: Some(Color::Rgb { r: 0, g: 0, b: 0 }),
            bold: false,
            italic: false,
            underline: false,
        };
        let theme = Theme {
            style: style.clone(),
            ..Theme::default()
        };
        let mut buffer = RenderBuffer::new(10, 5, &style);
        render_panel(&mut buffer, &panel, Point::new(0, 0), 10, &theme);

        let selected_bg = Some(Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        });
        assert_eq!(buffer.cells[9].style.bg, selected_bg);
    }

    #[test]
    fn row_panel_uses_its_theme_aware_surface_and_border_without_affecting_other_panels() {
        let theme = parse_vscode_theme("themes/kanso.json").unwrap();
        let sidebar_fg = theme.colors["sideBar.foreground"];
        let sidebar_bg = theme.colors["sideBar.background"];
        let sidebar_border = theme.colors["sideBar.border"];

        let mut manager = PanelManager::default();
        manager.create_panel(
            "neotree".to_string(),
            PanelConfig {
                width: 10,
                surface: Some(ThemeStyleSpec {
                    foreground: vec!["sideBar.foreground".to_string()],
                    background: vec!["sideBar.background".to_string()],
                    ..ThemeStyleSpec::default()
                }),
                border: Some(ThemeStyleSpec {
                    foreground: vec!["sideBar.border".to_string()],
                    background: vec!["editor.background".to_string()],
                    ..ThemeStyleSpec::default()
                }),
                ..PanelConfig::default()
            },
        );
        manager.update_panel("neotree", vec![row("selected"), row("plain")]);

        let mut buffer = RenderBuffer::new(11, 5, &theme.style);
        manager.render(&mut buffer, &theme);

        assert_eq!(buffer.cells[11].style.fg, Some(sidebar_fg));
        assert_eq!(buffer.cells[11].style.bg, Some(sidebar_bg));
        assert_eq!(buffer.cells[10].text, "│");
        assert_eq!(buffer.cells[10].style.fg, Some(sidebar_border));

        let mut manager = PanelManager::default();
        manager.create_panel(
            "other".to_string(),
            PanelConfig {
                width: 10,
                ..PanelConfig::default()
            },
        );
        manager.update_panel("other", vec![row("selected"), row("plain")]);

        let mut buffer = RenderBuffer::new(11, 5, &theme.style);
        manager.render(&mut buffer, &theme);

        assert_eq!(buffer.cells[11].style.fg, theme.style.fg);
        assert_eq!(buffer.cells[11].style.bg, theme.style.bg);
        assert_eq!(buffer.cells[10].text, " ");
        assert_eq!(buffer.cells[10].style, theme.style);
    }

    #[test]
    fn selected_panel_badge_leaves_highlighted_right_inset_for_glyph_overhang() {
        let mut panel = PluginPanel::new("tree".to_string(), PanelConfig::default());
        let mut row = row("src");
        row.right_segments.push(PanelSegment {
            text: "".to_string(),
            style: None,
            semantic: None,
        });
        panel.update_rows(vec![row]);

        let style = Style {
            fg: Some(Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            }),
            bg: Some(Color::Rgb { r: 0, g: 0, b: 0 }),
            bold: false,
            italic: false,
            underline: false,
        };
        let theme = Theme {
            style: style.clone(),
            ..Theme::default()
        };
        let mut buffer = RenderBuffer::new(10, 5, &style);
        render_panel(&mut buffer, &panel, Point::new(0, 0), 10, &theme);

        assert_eq!(buffer.cells[8].text, "");
        assert_eq!(buffer.cells[9].text, " ");
        assert_eq!(buffer.cells[8].style.bg, buffer.cells[9].style.bg);
        assert_ne!(buffer.cells[9].style.bg, style.bg);
    }

    #[test]
    fn selected_panel_segments_meet_contrast_with_kanso_theme() {
        let theme = parse_vscode_theme("themes/kanso.json").unwrap();
        let directory_color = theme.colors["list.highlightForeground"];
        let mut panel = PluginPanel::new("tree".to_string(), PanelConfig::default());
        let mut row = row("types");
        row.segments[0].style = Some(Style {
            fg: Some(directory_color),
            bg: theme.style.bg,
            ..Style::default()
        });
        panel.update_rows(vec![row]);
        let mut buffer = RenderBuffer::new(10, 5, &theme.style);

        render_panel(&mut buffer, &panel, Point::new(0, 0), 10, &theme);

        let selected = &buffer.cells[0].style;
        let selected_bg = selected.bg.unwrap();
        let selected_fg = selected.fg.unwrap();
        assert!(contrast_ratio(selected_bg, theme.style.bg.unwrap()) >= 3.0);
        assert!(contrast_ratio(selected_fg, selected_bg) >= 4.5);
        assert_ne!(selected_bg, theme.style.fg.unwrap());
        assert_ne!(selected_fg, Color::Rgb { r: 0, g: 0, b: 0 });
        assert_ne!(
            selected_fg,
            Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            }
        );
    }

    #[test]
    fn render_panel_clips_left_segments_for_right_badge() {
        let mut panel = PluginPanel::new("tree".to_string(), PanelConfig::default());
        let mut row = row("abcdef");
        row.right_segments.push(PanelSegment {
            text: "M".to_string(),
            style: None,
            semantic: None,
        });
        panel.update_rows(vec![row]);

        let style = Style::default();
        let theme = Theme {
            style: style.clone(),
            ..Theme::default()
        };
        let mut buffer = RenderBuffer::new(6, 5, &style);
        render_panel(&mut buffer, &panel, Point::new(0, 0), 6, &theme);

        assert_eq!(row_text(&buffer, 0), "abc M ");
    }

    #[test]
    fn text_panel_reuses_layout_until_content_or_stream_state_changes() {
        let mut panel = TextPanel::new("agent".to_string(), PanelConfig::default());
        panel.update_blocks(
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Markdown,
                text: "# Answer\n\nSome **formatted** text.".to_string(),
            }],
            20,
            40,
        );

        let initial = panel.layout(40);
        assert!(Arc::ptr_eq(&initial, &panel.layout(40)));

        panel.append_delta("answer", "\n\nMore text.", 20, 40);
        let appended = panel.layout(40);
        assert!(!Arc::ptr_eq(&initial, &appended));
        assert!(Arc::ptr_eq(&appended, &panel.layout(40)));

        panel.set_status(Some(TextPanelStatus {
            busy: true,
            label: "Working".to_string(),
            stream: false,
        }));
        assert!(Arc::ptr_eq(&appended, &panel.layout(40)));

        panel.set_status(Some(TextPanelStatus {
            busy: true,
            label: "Writing".to_string(),
            stream: true,
        }));
        assert!(!Arc::ptr_eq(&appended, &panel.layout(40)));
    }

    #[test]
    fn scrollback_selection_copy_ignores_soft_wraps_and_preserves_hard_breaks() {
        let mut panel = TextPanel::new("agent".to_string(), PanelConfig::default());
        panel.blocks = vec![TextPanelBlock {
            id: "answer".to_string(),
            kind: TextPanelBlockKind::Text,
            format: TextPanelBlockFormat::Plain,
            text: "alpha beta gamma\nsecond line".to_string(),
        }];

        let layout = panel.layout(7);
        assert!(layout.lines.len() > 2);
        assert_eq!(
            layout.selected_text(0, layout.len - 1, false),
            "alpha beta gamma\nsecond line"
        );

        let wide = panel.layout(40);
        assert_eq!(layout.len, wide.len);
        assert_eq!(
            layout.selected_text(6, 9, false),
            wide.selected_text(6, 9, false)
        );

        panel.blocks[0].kind = TextPanelBlockKind::User;
        let narrow_user = panel.layout(7);
        let wide_user = panel.layout(40);
        assert_eq!(narrow_user.len, wide_user.len);
        assert_eq!(
            narrow_user.selected_text(0, narrow_user.len - 1, false),
            "alpha beta gamma\nsecond line"
        );
    }

    #[test]
    fn text_panel_position_skips_empty_and_chrome_rows_at_shared_offsets() {
        let layout = TextPanelLayout::new(vec![
            RenderedTextLine::chrome("heading".to_string(), TextPanelSpanStyle::Text),
            RenderedTextLine::plain(String::new(), TextPanelSpanStyle::Text),
            RenderedTextLine::plain("alpha".to_string(), TextPanelSpanStyle::Text),
            RenderedTextLine::chrome("divider".to_string(), TextPanelSpanStyle::Text),
            RenderedTextLine::plain(String::new(), TextPanelSpanStyle::Text),
            RenderedTextLine::plain("界β".to_string(), TextPanelSpanStyle::Text),
            RenderedTextLine::chrome("footer".to_string(), TextPanelSpanStyle::Text),
        ]);

        assert_eq!(layout.position(0), Some((2, 0, 0)));
        assert_eq!(layout.position(4), Some((2, 4, 4)));
        assert_eq!(layout.position(5), Some((5, 0, 0)));
        assert_eq!(layout.position(6), Some((5, 1, 2)));
        assert_eq!(layout.position(usize::MAX), Some((5, 1, 2)));
    }

    #[test]
    fn text_panel_position_rejects_layouts_without_selectable_cells() {
        let layout = TextPanelLayout::new(vec![
            RenderedTextLine::chrome("heading".to_string(), TextPanelSpanStyle::Text),
            RenderedTextLine::plain(String::new(), TextPanelSpanStyle::Text),
        ]);

        assert_eq!(layout.position(0), None);
    }

    #[test]
    fn markdown_selection_omits_code_frames_and_preserves_code_blank_lines() {
        let mut panel = TextPanel::new("agent".to_string(), PanelConfig::default());
        panel.blocks = vec![TextPanelBlock {
            id: "answer".to_string(),
            kind: TextPanelBlockKind::Text,
            format: TextPanelBlockFormat::Markdown,
            text: "```bash\n/game\n\npwd\n```".to_string(),
        }];

        let layout = panel.layout(40);
        let copied = layout.selected_text(0, layout.len - 1, false);

        assert_eq!(copied, "/game\n\npwd");
        assert!(!copied.contains("bash"));
        assert!(!copied.contains(['┌', '│', '└']));
        assert_eq!(layout.len, "/gamepwd".graphemes(true).count());
        assert!(layout.lines.iter().any(|line| line.chrome_only));
    }

    #[test]
    fn markdown_selection_omits_heading_quote_rule_and_role_chrome() {
        let mut panel = TextPanel::new("agent".to_string(), PanelConfig::default());
        panel.blocks = vec![
            TextPanelBlock {
                id: "user".to_string(),
                kind: TextPanelBlockKind::User,
                format: TextPanelBlockFormat::Markdown,
                text: "# Heading\n\n> quoted **strong** and `inline`".to_string(),
            },
            TextPanelBlock {
                id: "agent".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Markdown,
                text: "---\n\nAfter".to_string(),
            },
        ];

        let layout = panel.layout(60);
        let copied = layout.selected_text(0, layout.len - 1, false);

        for content in ["Heading", "quoted strong and inline", "After"] {
            assert!(
                copied.contains(content),
                "missing semantic content {content:?}"
            );
        }
        for chrome in ["▎ You", "◆ Agent", "▍", "│", "─"] {
            assert!(!copied.contains(chrome), "copied display chrome {chrome:?}");
        }
    }

    #[test]
    fn markdown_selection_keeps_list_structure_and_link_labels() {
        let mut panel = TextPanel::new("agent".to_string(), PanelConfig::default());
        panel.blocks = vec![TextPanelBlock {
            id: "answer".to_string(),
            kind: TextPanelBlockKind::Text,
            format: TextPanelBlockFormat::Markdown,
            text: "- item\n- [x] done\n\n1. numbered\n\n[label](https://example.com)".to_string(),
        }];

        let layout = panel.layout(50);
        let copied = layout.selected_text(0, layout.len - 1, false);

        assert!(copied.contains("• item"));
        assert!(copied.contains("• ☑ done"));
        assert!(copied.contains("1. numbered"));
        assert!(copied.contains("label"));
        assert!(!copied.contains("https://example.com"));
    }

    #[test]
    fn markdown_table_selection_uses_tabs_and_omits_grid_padding() {
        let mut panel = TextPanel::new("agent".to_string(), PanelConfig::default());
        panel.blocks = vec![TextPanelBlock {
            id: "answer".to_string(),
            kind: TextPanelBlockKind::Text,
            format: TextPanelBlockFormat::Markdown,
            text: "| Name | Value |\n|---|---|\n| one | 1 |".to_string(),
        }];

        let layout = panel.layout(40);
        let copied = layout.selected_text(0, layout.len - 1, false);

        assert_eq!(copied, "Name\tValue\none\t1");
        assert!(!copied.contains('━'));
        assert!(!copied.contains("  "));
    }

    #[test]
    fn markdown_selection_is_stable_across_wrapping_and_unicode() {
        let mut panel = TextPanel::new("agent".to_string(), PanelConfig::default());
        panel.blocks = vec![TextPanelBlock {
            id: "answer".to_string(),
            kind: TextPanelBlockKind::Text,
            format: TextPanelBlockFormat::Markdown,
            text: "# Café 👩‍💻\n\n- outer item with enough words to wrap\n   - nested e\u{301} item\n\n> quoted words that also wrap\n\n```bash\necho 👩‍💻-abcdefghijk\n```"
                .to_string(),
        }];

        let narrow = panel.layout(14);
        let wide = panel.layout(80);
        let narrow_copy = narrow.selected_text(0, narrow.len - 1, false);
        let wide_copy = wide.selected_text(0, wide.len - 1, false);

        assert_eq!(narrow.len, wide.len);
        assert_eq!(narrow_copy, wide_copy);
        assert!(narrow_copy.contains("Café 👩‍💻"));
        assert!(narrow_copy.contains("  • nested e\u{301} item"));
        assert!(narrow_copy.contains("echo 👩‍💻-abcdefghijk"));
        assert!(!narrow_copy.contains(['▍', '│', '┌', '└']));
    }

    #[test]
    fn markdown_selection_keeps_visible_html_and_image_alt_text() {
        let mut panel = TextPanel::new("agent".to_string(), PanelConfig::default());
        panel.blocks = vec![TextPanelBlock {
            id: "answer".to_string(),
            kind: TextPanelBlockKind::Text,
            format: TextPanelBlockFormat::Markdown,
            text: "Press <kbd>Ctrl</kbd> beside ![diagram](https://example.com/image.png)."
                .to_string(),
        }];

        let layout = panel.layout(80);
        let copied = layout.selected_text(0, layout.len - 1, false);

        assert_eq!(copied, "Press <kbd>Ctrl</kbd> beside diagram.");
        assert!(!copied.contains("https://example.com/image.png"));
    }

    #[test]
    fn narrow_markdown_table_selection_omits_record_chrome() {
        let mut panel = TextPanel::new("agent".to_string(), PanelConfig::default());
        panel.blocks = vec![TextPanelBlock {
            id: "answer".to_string(),
            kind: TextPanelBlockKind::Text,
            format: TextPanelBlockFormat::Markdown,
            text: "| Name | Value |\n|---|---|\n| one | 1 |\n| two | 2 |".to_string(),
        }];

        let layout = panel.layout(10);
        let copied = layout.selected_text(0, layout.len - 1, false);

        for content in ["Name", "Value", "one", "1", "two", "2"] {
            assert!(copied.contains(content));
        }
        assert!(!copied.contains(['─', '━']));
        assert!(!copied.lines().any(|line| line.starts_with("  ")));
    }

    #[test]
    fn scrollback_focus_starts_on_the_first_visible_row_so_j_moves_down() {
        use crossterm::event::KeyEvent;

        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 20,
                title: Some("Agent".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 2,
                }),
                ..PanelConfig::default()
            },
        );
        manager.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Text,
                format: TextPanelBlockFormat::Plain,
                text: (1..=30)
                    .map(|line| format!("line {line}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            }],
            8,
            80,
        );
        assert!(manager.focus_text_panel_composer("agent"));
        assert!(manager.focus_focused_text_scrollback(80));

        let panel = &manager.text_panels["agent"];
        assert!(panel.scroll > 0);
        let before_row = panel
            .layout(20)
            .position(panel.scrollback.cursor)
            .unwrap()
            .0;
        assert_eq!(before_row, panel.scroll);

        manager
            .handle_focused_scrollback_input(
                &Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
                8,
                80,
                1,
            )
            .unwrap();

        let panel = &manager.text_panels["agent"];
        let after_row = panel
            .layout(20)
            .position(panel.scrollback.cursor)
            .unwrap()
            .0;
        assert_eq!(after_row, before_row + 1);
    }

    #[test]
    fn scrollback_j_moves_forward_across_empty_rows() {
        use crossterm::event::KeyEvent;

        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 20,
                ..PanelConfig::default()
            },
        );
        manager.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Text,
                format: TextPanelBlockFormat::Plain,
                text: "first\n\nthird".to_string(),
            }],
            10,
            80,
        );
        assert!(manager.focus_panel("agent"));
        manager
            .handle_focused_scrollback_input(
                &Event::Key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE)),
                10,
                80,
                1,
            )
            .unwrap();

        let before = manager.text_panels["agent"].scrollback.cursor;
        manager
            .handle_focused_scrollback_input(
                &Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
                10,
                80,
                1,
            )
            .unwrap();

        let panel = &manager.text_panels["agent"];
        assert!(panel.scrollback.cursor > before);
        let layout = panel.layout(20);
        let (row, _, _) = layout.position(panel.scrollback.cursor).unwrap();
        assert_eq!(
            layout.selected_text(panel.scrollback.cursor, panel.scrollback.cursor, false),
            "t"
        );
        assert_eq!(row, 2);
    }

    #[test]
    fn scrollback_escape_i_and_a_return_to_the_composer_in_one_keypress() {
        use crossterm::event::KeyEvent;

        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 30,
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 2,
                }),
                ..PanelConfig::default()
            },
        );
        manager.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Text,
                format: TextPanelBlockFormat::Plain,
                text: "answer".to_string(),
            }],
            10,
            80,
        );
        assert!(manager.focus_text_panel_composer("agent"));
        manager.handle_focused_text_input(&Event::Paste("abcd".to_string()), 80);

        assert!(manager.focus_focused_text_scrollback(80));
        manager
            .handle_focused_scrollback_input(
                &Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
                10,
                80,
                1,
            )
            .unwrap();
        assert_eq!(
            manager.focused_text_panel_cursor_mode(),
            Some(crate::editor::Mode::Insert)
        );

        for (key, expected_cursor) in [('i', 1), ('a', 2)] {
            let composer = manager
                .text_panels
                .get_mut("agent")
                .unwrap()
                .composer
                .as_mut()
                .unwrap();
            composer.prompt.set_cursor(1);
            composer.prompt.set_mode(crate::editor::Mode::Insert);
            assert!(manager.focus_focused_text_scrollback(80));

            manager
                .handle_focused_scrollback_input(
                    &Event::Key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE)),
                    10,
                    80,
                    1,
                )
                .unwrap();

            let panel = &manager.text_panels["agent"];
            let composer = panel.composer.as_ref().unwrap();
            assert!(composer.focused);
            assert!(!panel.scrollback.focused);
            assert_eq!(composer.prompt.mode(), crate::editor::Mode::Insert);
            assert_eq!(composer.prompt.cursor(), expected_cursor);
        }
    }

    #[test]
    fn scrollback_visual_line_yank_is_unicode_safe_and_linewise() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 20,
                ..PanelConfig::default()
            },
        );
        manager.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Text,
                format: TextPanelBlockFormat::Plain,
                text: "a👨‍👩‍👧‍👦e\u{301}\nsecond".to_string(),
            }],
            10,
            80,
        );
        assert!(manager.focus_panel("agent"));
        manager
            .handle_focused_scrollback_input(
                &Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Char('g'),
                    KeyModifiers::NONE,
                )),
                10,
                80,
                1,
            )
            .unwrap();
        manager
            .handle_focused_scrollback_input(
                &Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Char('V'),
                    KeyModifiers::SHIFT,
                )),
                10,
                80,
                1,
            )
            .unwrap();
        let yank = manager
            .handle_focused_scrollback_input(
                &Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Char('y'),
                    KeyModifiers::NONE,
                )),
                10,
                80,
                1,
            )
            .unwrap();

        assert_eq!(
            yank,
            TextPanelScrollbackInput::Yank(TextPanelYank {
                text: "a👨‍👩‍👧‍👦e\u{301}\n".to_string(),
                linewise: true,
            })
        );
    }

    #[test]
    fn scrollback_enter_copies_selection_and_returns_to_latest() {
        use crossterm::event::KeyEvent;

        for composer_enabled in [Some(true), Some(false), None] {
            for (selection, expected, linewise) in
                [('v', "alpha", false), ('V', "alpha beta\n", true)]
            {
                let mut manager = PanelManager::default();
                manager.create_text_panel(
                    "agent".into(),
                    PanelConfig {
                        side: PanelSide::Right,
                        width: 28,
                        composer: composer_enabled.map(|_| TextPanelComposerConfig {
                            placeholder: "Ask".into(),
                            rows: 3,
                        }),
                        ..PanelConfig::default()
                    },
                );
                if let Some(enabled) = composer_enabled {
                    assert!(matches!(
                        manager.load_text_panel_draft("agent", "unsent draft", None),
                        Ok(TextPanelReuseOutcome::Loaded)
                    ));
                    let composer = manager
                        .text_panels
                        .get_mut("agent")
                        .unwrap()
                        .composer
                        .as_mut()
                        .unwrap();
                    composer.enabled = enabled;
                    composer.prompt.set_mode(crate::editor::Mode::Normal);
                }
                manager.update_text_panel(
                    "agent",
                    vec![TextPanelBlock {
                        id: "answer".into(),
                        kind: TextPanelBlockKind::Text,
                        format: TextPanelBlockFormat::Plain,
                        text: format!("alpha beta\n{}latest", "more output\n".repeat(30)),
                    }],
                    12,
                    80,
                );
                assert!(manager.focus_panel("agent"));
                assert!(manager.focus_focused_text_scrollback(80));
                let press = |manager: &mut PanelManager, key| {
                    manager.handle_focused_scrollback_input(
                        &Event::Key(KeyEvent::new(key, KeyModifiers::NONE)),
                        12,
                        80,
                        1,
                    )
                };
                // Normal-mode Enter remains available to the link handler.
                assert_eq!(press(&mut manager, KeyCode::Enter), None);
                for key in ['g', selection, 'e'] {
                    assert_eq!(
                        press(&mut manager, KeyCode::Char(key)),
                        Some(TextPanelScrollbackInput::Handled)
                    );
                }
                assert!(!manager.text_panels["agent"].follow_tail);
                assert_eq!(
                    press(&mut manager, KeyCode::Enter),
                    Some(TextPanelScrollbackInput::Yank(TextPanelYank {
                        text: expected.into(),
                        linewise,
                    }))
                );
                let panel = &manager.text_panels["agent"];
                assert_eq!(panel.scrollback.mode, TextPanelScrollbackMode::Normal);
                assert!(panel.scrollback.selection_anchor.is_none());
                assert!(panel.follow_tail);
                assert_eq!(panel.scroll, panel.max_scroll(12, 28));
                assert_eq!(panel.scrollback.cursor, panel.layout(28).len - 1);
                assert_eq!(panel.scrollback.focused, composer_enabled != Some(true));
                if let Some(composer) = &panel.composer {
                    assert_eq!(composer.prompt.text(), "unsent draft");
                    assert_eq!(composer.prompt.mode(), crate::editor::Mode::Normal);
                    assert_eq!(composer.focused, composer_enabled == Some(true));
                }
            }
        }
    }

    #[test]
    fn scrollback_character_find_extends_visual_selection() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 20,
                ..PanelConfig::default()
            },
        );
        manager.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Text,
                format: TextPanelBlockFormat::Plain,
                text: "alpha beta".to_string(),
            }],
            10,
            80,
        );
        assert!(manager.focus_panel("agent"));
        let mut outcome = None;
        for key in ['g', 'v', 'f', 'b', 'y'] {
            outcome = manager.handle_focused_scrollback_input(
                &Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Char(key),
                    KeyModifiers::NONE,
                )),
                10,
                80,
                1,
            );
        }

        let panel = &manager.text_panels["agent"];
        assert_eq!(panel.scrollback.mode, TextPanelScrollbackMode::Normal);
        assert_eq!(panel.scrollback.cursor, 0);
        assert_eq!(
            outcome,
            Some(TextPanelScrollbackInput::Yank(TextPanelYank {
                text: "alpha b".to_string(),
                linewise: false,
            }))
        );
    }

    #[test]
    fn streaming_append_does_not_expand_an_active_scrollback_selection() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 20,
                ..PanelConfig::default()
            },
        );
        manager.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Text,
                format: TextPanelBlockFormat::Plain,
                text: "alpha beta".to_string(),
            }],
            10,
            80,
        );
        assert!(manager.focus_panel("agent"));
        for key in ['g', 'v', 'e'] {
            manager
                .handle_focused_scrollback_input(
                    &Event::Key(crossterm::event::KeyEvent::new(
                        KeyCode::Char(key),
                        KeyModifiers::NONE,
                    )),
                    10,
                    80,
                    1,
                )
                .unwrap();
        }
        manager.append_text_panel("agent", "answer", " gamma", 10, 80);

        let yank = manager
            .handle_focused_scrollback_input(
                &Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Char('y'),
                    KeyModifiers::NONE,
                )),
                10,
                80,
                1,
            )
            .unwrap();
        assert_eq!(
            yank,
            TextPanelScrollbackInput::Yank(TextPanelYank {
                text: "alpha".to_string(),
                linewise: false,
            })
        );
    }
}
