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

use std::{collections::HashMap, time::Instant};

use crossterm::event::{Event, KeyCode, KeyModifiers};
use serde::{Deserialize, Serialize};

use super::markdown::{
    render_markdown_lines, wrap_plain_text, RenderedTextLine, RenderedTextSpan, TextPanelSpanStyle,
};
use super::text_link::{TextPanelLink, TextPanelLinkTarget};
use crate::{
    editor::{render_buffer::RenderBuffer, Point},
    theme::{SelectionForegroundPriority, Style, Theme, ThemeStyleSpec},
    ui::{
        normalize_prompt_newlines, paint_rich_text, wrap_text, FollowTailViewport, PromptBuffer,
        PromptInput, PROMPT_MAX_BYTES,
    },
    unicode_utils::{display_width, fit_display_width, truncate_display_width},
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

fn effective_panel_width(config: &PanelConfig, terminal_width: usize) -> usize {
    if matches!(config.side, PanelSide::Top | PanelSide::Bottom) {
        return terminal_width;
    }

    let max_width = if config.composer.is_some() {
        terminal_width.saturating_sub(11).max(1)
    } else {
        terminal_width
    };
    config.width.min(max_width)
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
    #[serde(default = "default_composer_rows")]
    pub rows: usize,
}

/// One clickable action rendered in a text-panel header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextPanelHeaderAction {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub compact_label: Option<String>,
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
    Agent,
    Error,
    /// Muted tool/progress timeline emitted while an agent turn runs.
    Activity,
    #[default]
    Text,
}

/// Presentation format for a source-backed text-panel block.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextPanelBlockFormat {
    #[default]
    Plain,
    Markdown,
}

/// One logical block in a text panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextPanelBlock {
    pub id: String,
    #[serde(default)]
    pub kind: TextPanelBlockKind,
    #[serde(default)]
    pub format: TextPanelBlockFormat,
    pub text: String,
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

pub struct TextPanel {
    pub id: String,
    pub config: PanelConfig,
    pub blocks: Vec<TextPanelBlock>,
    pub scroll: usize,
    pub follow_tail: bool,
    viewport: FollowTailViewport,
    composer: Option<TextPanelComposer>,
    status: Option<TextPanelStatus>,
    busy_since: Option<Instant>,
    selected_link: Option<u64>,
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

struct TextPanelComposer {
    config: TextPanelComposerConfig,
    prompt: PromptBuffer,
    focused: bool,
    enabled: bool,
    status: Option<String>,
    validation: Option<&'static str>,
}

impl TextPanelComposer {
    fn new(config: TextPanelComposerConfig) -> Self {
        Self {
            config,
            prompt: PromptBuffer::new(""),
            focused: false,
            enabled: true,
            status: None,
            validation: None,
        }
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
            blocks: Vec::new(),
            scroll: 0,
            follow_tail: true,
            viewport: FollowTailViewport::default(),
            composer,
            status: None,
            busy_since: None,
            selected_link: None,
        }
    }

    fn set_status(&mut self, status: Option<TextPanelStatus>) {
        self.busy_since = if status.as_ref().is_some_and(|status| status.busy) {
            self.busy_since.or_else(|| Some(Instant::now()))
        } else {
            None
        };
        self.status = status;
    }

    fn status_height(&self) -> usize {
        usize::from(self.status.is_some())
    }

    fn update_blocks(
        &mut self,
        blocks: Vec<TextPanelBlock>,
        panel_height: usize,
        panel_width: usize,
    ) {
        if blocks.is_empty() {
            self.viewport.reset();
            self.scroll = self.viewport.offset();
            self.follow_tail = self.viewport.is_following();
        }
        self.blocks = blocks;
        if self.follow_tail {
            self.scroll_to_bottom(panel_height, panel_width);
        } else {
            self.clamp_scroll(panel_height, panel_width);
        }
    }

    fn append_delta(
        &mut self,
        block_id: &str,
        delta: &str,
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

        if self.follow_tail {
            self.scroll_to_bottom(panel_height, panel_width);
        } else {
            self.clamp_scroll(panel_height, panel_width);
        }
    }

    fn move_scroll(&mut self, delta: isize, panel_height: usize, panel_width: usize) {
        let max_scroll = self.max_scroll(panel_height, panel_width);
        self.viewport.restore(self.scroll, self.follow_tail);
        self.viewport.move_by(delta, max_scroll);
        self.scroll = self.viewport.offset();
        self.follow_tail = self.viewport.is_following();
    }

    fn page_scroll(&mut self, delta: isize, panel_height: usize, panel_width: usize) {
        let page = self.visible_rows(panel_height).max(1) as isize;
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

    fn clamp_scroll(&mut self, panel_height: usize, panel_width: usize) {
        self.viewport.restore(self.scroll, self.follow_tail);
        self.viewport
            .clamp(self.max_scroll(panel_height, panel_width));
        self.scroll = self.viewport.offset();
        self.follow_tail = self.viewport.is_following();
    }

    fn max_scroll(&self, panel_height: usize, panel_width: usize) -> usize {
        self.rendered_lines(panel_width.max(1))
            .len()
            .saturating_sub(self.visible_rows(panel_height))
    }

    fn visible_rows(&self, panel_height: usize) -> usize {
        panel_height
            .saturating_sub(usize::from(
                self.config.title.is_some() || !self.config.header_actions.is_empty(),
            ))
            .saturating_sub(self.composer_height())
            .saturating_sub(self.status_height())
            .max(1)
    }

    fn composer_height(&self) -> usize {
        self.composer
            .as_ref()
            .map_or(0, |composer| composer.config.rows.max(1).saturating_add(2))
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

    fn links(&self, width: usize) -> Vec<(TextPanelLink, usize)> {
        let mut links = Vec::new();
        for (line_index, line) in self.rendered_lines(width).into_iter().enumerate() {
            for span in line.spans {
                let Some(link) = span.link else {
                    continue;
                };
                if links
                    .last()
                    .is_none_or(|(previous, _): &(TextPanelLink, usize)| previous.id != link.id)
                {
                    links.push((link, line_index));
                }
            }
        }
        links
    }

    fn select_link(&mut self, forward: bool, panel_height: usize, width: usize) -> bool {
        let links = self.links(width);
        if links.is_empty() {
            self.selected_link = None;
            return false;
        }
        let current = self
            .selected_link
            .and_then(|selected| links.iter().position(|(link, _)| link.id == selected));
        let index = match (current, forward) {
            (Some(index), true) => (index + 1) % links.len(),
            (Some(0), false) => links.len() - 1,
            (Some(index), false) => index - 1,
            (None, true) => 0,
            (None, false) => links.len() - 1,
        };
        let (link, line) = &links[index];
        self.selected_link = Some(link.id);
        self.viewport.restore(self.scroll, self.follow_tail);
        self.viewport.reveal(*line, self.visible_rows(panel_height));
        self.scroll = self.viewport.offset();
        self.follow_tail = self.viewport.is_following();
        true
    }

    fn selected_link_target(&self, width: usize) -> Option<TextPanelLinkTarget> {
        let selected = self.selected_link?;
        self.links(width)
            .into_iter()
            .find(|(link, _)| link.id == selected)
            .map(|(link, _)| link.target)
    }

    fn rendered_lines(&self, width: usize) -> Vec<RenderedTextLine> {
        let mut lines: Vec<RenderedTextLine> = Vec::new();
        for (block_index, block) in self.blocks.iter().enumerate() {
            if block.kind == TextPanelBlockKind::User {
                // A new user message starts a turn: separate it with a light
                // rule and mark its lines with an accent bar instead of a
                // one-line label.
                if let Some(last) = lines.last_mut() {
                    if last.is_empty() {
                        *last = turn_separator(width);
                    } else {
                        lines.push(turn_separator(width));
                    }
                }
                lines.push(RenderedTextLine::plain(
                    "▎ You".to_string(),
                    TextPanelSpanStyle::User,
                ));
                let content_width = width.saturating_sub(2).max(1);
                let mut block_lines = match block.format {
                    TextPanelBlockFormat::Plain => {
                        wrap_plain_text(&block.text, content_width, TextPanelSpanStyle::Text)
                    }
                    TextPanelBlockFormat::Markdown => {
                        render_markdown_lines(&block.text, content_width)
                    }
                };
                if block_lines.is_empty() {
                    block_lines.push(RenderedTextLine::plain(
                        String::new(),
                        TextPanelSpanStyle::Text,
                    ));
                }
                namespace_block_links(&mut block_lines, block_index);
                lines.extend(block_lines.into_iter().map(user_accented));
            } else {
                if let Some((label, style)) = block_label(&block.kind) {
                    lines.push(RenderedTextLine::plain(label.to_string(), style));
                }

                let style = block_style(&block.kind);
                let mut block_lines = match block.format {
                    TextPanelBlockFormat::Plain => wrap_plain_text(&block.text, width, style),
                    TextPanelBlockFormat::Markdown => render_markdown_lines(&block.text, width),
                };
                if block_lines.is_empty() {
                    block_lines.push(RenderedTextLine::plain(String::new(), style));
                }
                namespace_block_links(&mut block_lines, block_index);
                lines.extend(block_lines);
            }
            lines.push(RenderedTextLine::plain(
                String::new(),
                TextPanelSpanStyle::Text,
            ));
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
                });
            }
        }
        lines
    }
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
    pub selected: usize,
    pub scroll: usize,
}

impl PluginPanel {
    pub fn new(id: String, config: PanelConfig) -> Self {
        Self {
            id,
            config,
            rows: Vec::new(),
            selected: 0,
            scroll: 0,
        }
    }

    pub fn update_rows(&mut self, rows: Vec<PanelRow>) {
        self.rows = rows;
        if self.rows.is_empty() {
            self.selected = 0;
            self.scroll = 0;
        } else if self.selected >= self.rows.len() {
            self.selected = self.rows.len() - 1;
        }

        if self.scroll > self.selected {
            self.scroll = self.selected;
        }
    }

    pub fn move_selection(&mut self, delta: isize, panel_height: usize) {
        if self.rows.is_empty() {
            return;
        }

        let max_index = self.rows.len() - 1;
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
        if self.rows.is_empty() {
            self.selected = 0;
            self.scroll = 0;
            return;
        }

        let visible_rows = self.visible_rows(panel_height);
        let max_scroll = self.rows.len().saturating_sub(visible_rows);
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
            .min(self.rows.len() - 1);
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
        if self.rows.is_empty() {
            self.scroll_to_top();
            return;
        }

        self.selected = self.rows.len() - 1;
        self.scroll = self
            .rows
            .len()
            .saturating_sub(self.visible_rows(panel_height));
    }

    pub fn select_row_by_id(&mut self, row_id: &str, panel_height: usize) -> bool {
        let Some(index) = self.rows.iter().position(|row| row.id == row_id) else {
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
        self.rows.get(self.selected).cloned()
    }

    fn rows_start(&self) -> usize {
        usize::from(self.config.title.is_some())
    }

    fn visible_rows(&self, panel_height: usize) -> usize {
        panel_height.saturating_sub(self.rows_start()).max(1)
    }

    fn select_screen_row(&mut self, screen_y: usize) {
        let rows_start = self.rows_start();
        if screen_y < rows_start || self.rows.is_empty() {
            return;
        }

        let row_index = self.scroll + screen_y - rows_start;
        if row_index < self.rows.len() {
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

#[derive(Default)]
pub struct PanelManager {
    panels: HashMap<String, PluginPanel>,
    text_panels: HashMap<String, TextPanel>,
    default_layouts: HashMap<String, (PanelSide, usize)>,
    preferred_sizes: HashMap<String, PanelSizes>,
    z_order: Vec<String>,
    focused: Option<String>,
    animation_state: Vec<(String, u8, u64)>,
}

impl PanelManager {
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

    pub fn update_text_panel(
        &mut self,
        id: &str,
        blocks: Vec<TextPanelBlock>,
        panel_height: usize,
        terminal_width: usize,
    ) {
        if let Some(panel) = self.text_panels.get_mut(id) {
            let width = effective_panel_width(&panel.config, terminal_width);
            let panel_height = effective_panel_height(&panel.config, panel_height);
            panel.update_blocks(blocks, panel_height, width);
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
        if let Some(panel) = self.text_panels.get_mut(id) {
            let width = effective_panel_width(&panel.config, terminal_width);
            let panel_height = effective_panel_height(&panel.config, panel_height);
            panel.append_delta(block_id, delta, panel_height, width);
        }
    }

    pub fn update_panel(&mut self, id: &str, rows: Vec<PanelRow>) {
        if let Some(panel) = self.panels.get_mut(id) {
            panel.update_rows(rows);
        }
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
        }
        changed
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
            true
        } else {
            false
        }
    }

    pub fn select_row_by_id(&mut self, id: &str, row_id: &str, height: usize) -> bool {
        self.panels
            .get_mut(id)
            .is_some_and(|panel| panel.select_row_by_id(row_id, height))
    }

    pub fn focus_editor(&mut self) {
        if let Some(id) = self.focused.as_deref() {
            if let Some(composer) = self
                .text_panels
                .get_mut(id)
                .and_then(|panel| panel.composer.as_mut())
            {
                composer.focused = false;
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

    pub fn focused_text_panel_has_composer(&self) -> bool {
        self.focused
            .as_deref()
            .and_then(|id| self.text_panels.get(id))
            .is_some_and(|panel| panel.composer.is_some())
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

    pub fn reserved_left_width(&self) -> usize {
        self.z_order
            .iter()
            .filter_map(|id| self.panel_config(id))
            .filter(|config| config.side == PanelSide::Left)
            .map(|config| config.width.saturating_add(1))
            .sum()
    }

    pub fn reserved_right_width(&self) -> usize {
        self.z_order
            .iter()
            .filter_map(|id| self.panel_config(id))
            .filter(|config| config.side == PanelSide::Right)
            .map(|config| config.width.saturating_add(1))
            .sum()
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

    pub fn handle_focused_key(
        &mut self,
        action: &str,
        panel_height: usize,
        terminal_width: usize,
        scrolloff: usize,
    ) -> Option<PanelEvent> {
        let focused = self.focused.clone()?;
        if let Some(panel) = self.text_panels.get_mut(&focused) {
            let width = effective_panel_width(&panel.config, terminal_width);
            let panel_height = effective_panel_height(&panel.config, panel_height);
            match action {
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
                _ => {}
            }
            return Some(PanelEvent {
                panel_id: panel.id.clone(),
                action: action.to_string(),
                selected_index: panel.scroll,
                row: None,
                text: None,
            });
        }
        let panel = self.panels.get_mut(&focused)?;

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

    pub fn handle_mouse_scroll(
        &mut self,
        id: &str,
        delta: isize,
        panel_height: usize,
        terminal_width: usize,
        scrolloff: usize,
    ) -> Option<PanelEvent> {
        let action = if delta < 0 { "up" } else { "down" };
        if let Some(panel) = self.text_panels.get_mut(id) {
            let width = effective_panel_width(&panel.config, terminal_width);
            let panel_height = effective_panel_height(&panel.config, panel_height);
            panel.move_scroll(delta, panel_height, width);
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

    pub(crate) fn select_focused_text_link(
        &mut self,
        forward: bool,
        panel_height: usize,
        terminal_width: usize,
    ) -> bool {
        let Some(focused) = self.focused.clone() else {
            return false;
        };
        let Some(panel) = self.text_panels.get_mut(&focused) else {
            return false;
        };
        let width = effective_panel_width(&panel.config, terminal_width);
        let panel_height = effective_panel_height(&panel.config, panel_height);
        if let Some(composer) = panel.composer.as_mut() {
            composer.focused = false;
        }
        panel.select_link(forward, panel_height, width)
    }

    pub(crate) fn focused_text_link_target(
        &self,
        terminal_width: usize,
    ) -> Option<TextPanelLinkTarget> {
        let panel = self.text_panels.get(self.focused.as_deref()?)?;
        let width = effective_panel_width(&panel.config, terminal_width);
        panel.selected_link_target(width)
    }

    pub(crate) fn text_link_at_position(
        &mut self,
        x: usize,
        y: usize,
        terminal_width: usize,
        terminal_height: usize,
    ) -> Option<TextPanelLinkTarget> {
        let placement = self.panel_at_position(x, y, terminal_width, terminal_height)?;
        let panel = self.text_panels.get_mut(&placement.id)?;
        let title_rows =
            usize::from(panel.config.title.is_some() || !panel.config.header_actions.is_empty());
        let content_height = placement
            .height
            .saturating_sub(panel.composer_height())
            .saturating_sub(panel.status_height());
        let screen_row = y.saturating_sub(placement.y);
        if screen_row < title_rows || screen_row >= content_height {
            return None;
        }

        let lines = panel.rendered_lines(placement.width);
        let visible_rows = content_height.saturating_sub(title_rows);
        let max_scroll = lines.len().saturating_sub(visible_rows);
        let scroll = if panel.follow_tail {
            max_scroll
        } else {
            panel.scroll.min(max_scroll)
        };
        let line = lines.get(scroll + screen_row - title_rows)?;
        let column = x.saturating_sub(placement.x);
        let mut used = 0usize;
        for span in &line.spans {
            let end = used.saturating_add(display_width(&span.text));
            if column >= used && column < end {
                let link = span.link.as_ref()?;
                self.focused = Some(placement.id);
                panel.selected_link = Some(link.id);
                if let Some(composer) = panel.composer.as_mut() {
                    composer.focused = false;
                }
                return Some(link.target.clone());
            }
            used = end;
        }
        None
    }

    pub fn focused_text_for_copy(&self, all: bool) -> Option<String> {
        let panel = self.text_panels.get(self.focused.as_deref()?)?;
        if all {
            Some(panel.copy_all())
        } else {
            panel.copy_last_agent()
        }
    }

    pub fn focus_text_panel_composer(&mut self, id: &str) -> bool {
        if !self.z_order.iter().any(|panel_id| panel_id == id) {
            return false;
        }
        let Some(composer) = self
            .text_panels
            .get_mut(id)
            .and_then(|panel| panel.composer.as_mut())
        else {
            return false;
        };
        if !composer.enabled {
            return false;
        }
        composer.focused = true;
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
        }
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
        let panel = self.text_panels.get_mut(&focused)?;
        let panel_width = effective_panel_width(&panel.config, terminal_width);
        let composer = panel.composer.as_mut()?;
        if !composer.focused || !composer.enabled {
            return None;
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
                    || (key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('h' | 'j' | 'k' | 'g' | 'G' | 'w')))
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
        let (action, text) = match composer
            .prompt
            .handle_event(event, panel_width.saturating_sub(2).max(1))
        {
            PromptInput::Changed => {
                composer.validation = None;
                ("composer_input", None)
            }
            PromptInput::Submit => match composer.take_submission() {
                Some(text) => ("submit", Some(text)),
                None => ("composer_input", None),
            },
            PromptInput::Cancel => {
                composer.focused = false;
                ("composer_blur", None)
            }
            PromptInput::Unhandled
                if inserted_bytes > MAX_COMPOSER_BYTES.saturating_sub(previous_bytes) =>
            {
                composer.validation = Some("Prompt exceeds 128 KiB");
                ("composer_input", None)
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
        let panel = self.text_panels.get(self.focused.as_deref()?)?;
        let composer = panel.composer.as_ref()?;
        (composer.focused && composer.enabled).then(|| composer.prompt.mode())
    }

    pub fn focused_text_panel_cursor_position(
        &self,
        terminal_width: usize,
        terminal_height: usize,
    ) -> Option<(usize, usize)> {
        let id = self.focused.as_deref()?;
        let panel = self.text_panels.get(id)?;
        let composer = panel.composer.as_ref()?;
        if !composer.focused || !composer.enabled {
            return None;
        }
        let placement = self
            .panel_placements(terminal_width, terminal_height)
            .into_iter()
            .find(|placement| placement.id == id)?;
        let content_width = placement.width.saturating_sub(2).max(1);
        let wrapped = wrap_text(&composer.prompt.text(), content_width);
        let (row, column) = wrapped
            .positions
            .get(composer.prompt.cursor())
            .copied()
            .unwrap_or_default();
        let rows = composer.config.rows.max(1);
        let first = row.saturating_sub(rows.saturating_sub(1));
        let top = placement.height.saturating_sub(panel.composer_height());
        Some((
            placement.x.saturating_add(2).saturating_add(column),
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
        self.focused = Some(placement.id.clone());
        if let Some(panel) = self.text_panels.get_mut(&placement.id) {
            if y == placement.y {
                if let Some(action) = text_panel_header_action_at(
                    &panel.config,
                    placement.width,
                    x.saturating_sub(placement.x),
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

            let composer_top = placement
                .y
                .saturating_add(placement.height.saturating_sub(panel.composer_height()));
            let action = if y >= composer_top
                && panel
                    .composer
                    .as_ref()
                    .is_some_and(|composer| composer.enabled)
            {
                if let Some(composer) = panel.composer.as_mut() {
                    composer.focused = true;
                    let content_width = placement.width.saturating_sub(2).max(1);
                    let wrapped = wrap_text(&composer.prompt.text(), content_width);
                    let cursor_row = wrapped
                        .positions
                        .get(composer.prompt.cursor())
                        .map_or(0, |position| position.0);
                    let rows = composer.config.rows.max(1);
                    let first = cursor_row.saturating_sub(rows.saturating_sub(1));
                    let row = first.saturating_add(y.saturating_sub(composer_top + 1));
                    let column = x.saturating_sub(placement.x + 2);
                    if let Some((index, _)) = wrapped
                        .positions
                        .iter()
                        .enumerate()
                        .filter(|(_, position)| position.0 == row)
                        .min_by_key(|(_, position)| position.1.abs_diff(column))
                    {
                        composer.prompt.set_cursor(index);
                    }
                }
                "composer_focus"
            } else {
                if let Some(composer) = panel.composer.as_mut() {
                    composer.focused = false;
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
        panel.select_screen_row(y.saturating_sub(placement.y));

        Some(PanelEvent {
            panel_id: panel.id.clone(),
            action: "select".to_string(),
            selected_index: panel.selected,
            row: panel.selected_row(),
            text: None,
        })
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

        for id in &self.z_order {
            let Some(config) = self.panel_config(id) else {
                continue;
            };

            let (x, y, width, height) = match config.side {
                PanelSide::Left => {
                    let width = effective_panel_width(config, terminal_width)
                        .min(right_x.saturating_sub(left_x));
                    let x = left_x;
                    left_x = left_x.saturating_add(width.saturating_add(1));
                    (x, reserved_top, width, side_height)
                }
                PanelSide::Right => {
                    let width = effective_panel_width(config, terminal_width)
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
            render_panel_separator(
                buffer,
                position,
                placement.width,
                placement.height,
                config.side,
                &border_style,
                separator,
            );

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
    let visible_rows = height.saturating_sub(rows_start);
    for (screen_row, row) in panel
        .rows
        .iter()
        .skip(panel.scroll)
        .take(visible_rows)
        .enumerate()
    {
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

    for y in 0..height {
        buffer.set_text(
            position.x,
            position.y.saturating_add(y),
            &" ".repeat(width),
            &theme.style,
        );
    }

    let header_actions = text_panel_header_actions(&panel.config, width);
    let title_rows = usize::from(panel.config.title.is_some() || !header_actions.is_empty());
    let title_width = header_actions
        .first()
        .map_or(width, |(start, _, _)| start.saturating_sub(1));
    if let Some(title) = &panel.config.title {
        let title_style = Style {
            bold: true,
            ..theme.style.clone()
        };
        buffer.set_text(
            position.x,
            position.y,
            &fit_display_width(title, title_width),
            &title_style,
        );
    }
    for (start, _, label) in header_actions {
        let x = position.x + start;
        buffer.set_text(x, position.y, "[", &theme.ui_style.muted);
        buffer.set_text(x + 1, position.y, label, &theme.ui_style.picker_prompt);
        buffer.set_text(
            x + 1 + display_width(label),
            position.y,
            "]",
            &theme.ui_style.muted,
        );
    }

    let composer_height = panel.composer_height();
    let status_height = panel.status_height();
    let content_height = height
        .saturating_sub(composer_height)
        .saturating_sub(status_height);
    let visible_rows = content_height.saturating_sub(title_rows);
    let lines = panel.rendered_lines(width);
    let max_scroll = lines.len().saturating_sub(visible_rows);
    let scroll = panel.viewport.visible_offset(max_scroll);
    for (offset, line) in lines.iter().skip(scroll).take(visible_rows).enumerate() {
        render_text_spans(
            buffer,
            position.x,
            position.y.saturating_add(title_rows + offset),
            width,
            line,
            panel.selected_link,
            theme,
        );
    }

    if let Some(status) = &panel.status {
        render_text_panel_status(
            buffer,
            panel,
            status,
            position,
            width,
            content_height,
            theme,
        );
    }

    if let Some(composer) = &panel.composer {
        let overflow = match (scroll > 0, scroll < max_scroll) {
            (true, true) => TextPanelOverflow::Both,
            (true, false) => TextPanelOverflow::Above,
            (false, true) => TextPanelOverflow::Below,
            (false, false) => TextPanelOverflow::None,
        };
        render_text_panel_composer(
            buffer,
            composer,
            position,
            width,
            content_height + status_height,
            overflow,
            theme,
        );
    }
}

#[derive(Clone, Copy)]
enum TextPanelOverflow {
    None,
    Above,
    Below,
    Both,
}

fn render_text_panel_composer(
    buffer: &mut RenderBuffer,
    composer: &TextPanelComposer,
    position: Point,
    width: usize,
    top: usize,
    overflow: TextPanelOverflow,
    theme: &Theme,
) {
    if width == 0 {
        return;
    }
    let top = position.y.saturating_add(top);
    let divider = "─".repeat(width);
    buffer.set_text(
        position.x,
        top,
        &fit_display_width(&divider, width),
        &theme.ui_style.muted,
    );

    let rows = composer.config.rows.max(1);
    let content_width = width.saturating_sub(2).max(1);
    let wrapped = wrap_text(&composer.prompt.text(), content_width);
    let cursor_row = wrapped
        .positions
        .get(composer.prompt.cursor())
        .map_or(0, |position| position.0);
    let first = cursor_row.saturating_sub(rows.saturating_sub(1));
    for row in 0..rows {
        let y = top + 1 + row;
        let line = wrapped
            .rows
            .get(first + row)
            .map(String::as_str)
            .unwrap_or("");
        let text = if line.is_empty() && composer.prompt.text().is_empty() && row == 0 {
            composer.config.placeholder.as_str()
        } else {
            line
        };
        let style = if composer.enabled && composer.focused {
            &theme.ui_style.dialog
        } else {
            &theme.ui_style.muted
        };
        buffer.set_text(position.x, y, "›", &theme.ui_style.picker_prompt);
        buffer.set_text(
            position.x + 2,
            y,
            &fit_display_width(text, content_width),
            style,
        );
    }
    let hints = if composer.focused && composer.prompt.mode() == crate::editor::Mode::Normal {
        "NORMAL · j/k ↑/↓ scroll · i/a edit · Enter send · Esc nav"
    } else if composer.focused {
        "INSERT · ^J/^K scroll · ^g/^G ends · Ctrl+Enter send · Esc normal"
    } else {
        match overflow {
            TextPanelOverflow::Both => {
                "↑↓ more · j/k scroll · g/G ends · a edit · q close · ^C stop"
            }
            TextPanelOverflow::Below => {
                "↓ more · j/k scroll · G latest · a edit · q close · ^C stop"
            }
            TextPanelOverflow::Above => {
                "↑ history · j/k scroll · g oldest · a edit · q close · ^C stop"
            }
            TextPanelOverflow::None => "a edit · x clear · N new · q close · ^C stop",
        }
    };
    let status = composer.validation.or(composer.status.as_deref());
    let status = status.map_or_else(|| hints.to_string(), |status| format!("{status} · {hints}"));
    buffer.set_text(
        position.x,
        top + rows + 1,
        &fit_display_width(&status, width),
        &theme.ui_style.muted,
    );
}

fn render_text_panel_status(
    buffer: &mut RenderBuffer,
    panel: &TextPanel,
    status: &TextPanelStatus,
    position: Point,
    width: usize,
    y: usize,
    theme: &Theme,
) {
    if width == 0 {
        return;
    }
    let (text, style) = if status.busy {
        let elapsed_ms = panel
            .busy_since
            .map_or(0, |since| since.elapsed().as_millis() as u64);
        (
            format!(
                "{} {} · {}",
                spinner_frame(elapsed_ms),
                status.label,
                format_elapsed(elapsed_ms / 1000)
            ),
            &theme.ui_style.picker_prompt,
        )
    } else {
        (status.label.clone(), &theme.ui_style.muted)
    };
    buffer.set_text(
        position.x,
        position.y.saturating_add(y),
        &fit_display_width(&text, width),
        style,
    );
}

fn text_panel_header_actions(config: &PanelConfig, width: usize) -> Vec<(usize, &str, &str)> {
    let title_width = config.title.as_deref().map_or(0, display_width).min(5);
    let full_width = config
        .header_actions
        .iter()
        .map(|action| display_width(&action.label).saturating_add(2))
        .sum::<usize>()
        .saturating_add(config.header_actions.len().saturating_sub(1));
    let compact = full_width.saturating_add(title_width).saturating_add(1) > width;
    let mut labels = config
        .header_actions
        .iter()
        .map(|action| {
            let label = if compact {
                action.compact_label.as_deref().unwrap_or(&action.label)
            } else {
                &action.label
            };
            (action.id.as_str(), label)
        })
        .collect::<Vec<_>>();
    let mut used = labels
        .iter()
        .map(|(_, label)| display_width(label).saturating_add(2))
        .sum::<usize>()
        .saturating_add(labels.len().saturating_sub(1));
    while used > width && !labels.is_empty() {
        let (_, label) = labels.remove(0);
        used = used.saturating_sub(display_width(label).saturating_add(2));
        if !labels.is_empty() {
            used = used.saturating_sub(1);
        }
    }
    let mut start = width.saturating_sub(used);
    labels
        .into_iter()
        .map(|(action, label)| {
            let current = start;
            start = start.saturating_add(display_width(label).saturating_add(3));
            (current, action, label)
        })
        .collect()
}

fn text_panel_header_action_at(config: &PanelConfig, width: usize, x: usize) -> Option<&str> {
    text_panel_header_actions(config, width)
        .into_iter()
        .find(|(start, _, label)| {
            x >= *start && x < start.saturating_add(display_width(label).saturating_add(2))
        })
        .map(|(_, action, _)| action)
}

fn render_text_spans(
    buffer: &mut RenderBuffer,
    x: usize,
    y: usize,
    width: usize,
    line: &RenderedTextLine,
    selected_link: Option<u64>,
    theme: &Theme,
) {
    paint_rich_text(buffer, x, y, width, line, |span| {
        let base_style = text_panel_span_style(span.style, theme);
        let mut style = if let Some(syntax_style) = &span.syntax_style {
            Style {
                fg: syntax_style.fg.or(base_style.fg),
                bg: syntax_style.bg.or(base_style.bg).or(theme.style.bg),
                bold: syntax_style.bold || base_style.bold,
                italic: syntax_style.italic || base_style.italic,
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
        style
    });
}

fn text_panel_span_style(style: TextPanelSpanStyle, theme: &Theme) -> Style {
    let scoped = |scope: &str| {
        theme
            .get_style(scope)
            .unwrap_or_else(|| theme.style.clone())
    };
    match style {
        TextPanelSpanStyle::User => theme.ui_style.picker_prompt.clone(),
        TextPanelSpanStyle::Agent | TextPanelSpanStyle::Text => theme.style.clone(),
        TextPanelSpanStyle::Error => theme.ui_style.deprecated.clone(),
        TextPanelSpanStyle::Heading => {
            let mut style = scoped("heading.1.markdown");
            style.bold = true;
            style
        }
        TextPanelSpanStyle::Strong => Style {
            bold: true,
            ..theme.style.clone()
        },
        TextPanelSpanStyle::Emphasis => Style {
            italic: true,
            ..theme.style.clone()
        },
        TextPanelSpanStyle::Strikethrough => scoped("markup.strikethrough.markdown"),
        TextPanelSpanStyle::InlineCode | TextPanelSpanStyle::Code => {
            scoped("markup.raw.block.markdown")
        }
        TextPanelSpanStyle::Link => scoped("markup.underline.link.markdown"),
        TextPanelSpanStyle::Quote | TextPanelSpanStyle::Muted => theme.ui_style.muted.clone(),
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
        // User blocks render a rule + accent bar instead of a label.
        TextPanelBlockKind::User => None,
        TextPanelBlockKind::Agent => Some(("◆ Agent", TextPanelSpanStyle::Agent)),
        TextPanelBlockKind::Error => Some(("⚠ Error", TextPanelSpanStyle::Error)),
        TextPanelBlockKind::Activity | TextPanelBlockKind::Text => None,
    }
}

fn block_style(kind: &TextPanelBlockKind) -> TextPanelSpanStyle {
    match kind {
        TextPanelBlockKind::User => TextPanelSpanStyle::User,
        TextPanelBlockKind::Agent => TextPanelSpanStyle::Agent,
        TextPanelBlockKind::Error => TextPanelSpanStyle::Error,
        TextPanelBlockKind::Activity => TextPanelSpanStyle::Muted,
        TextPanelBlockKind::Text => TextPanelSpanStyle::Text,
    }
}

fn turn_separator(width: usize) -> RenderedTextLine {
    RenderedTextLine::plain("─".repeat(width.max(1)), TextPanelSpanStyle::Muted)
}

fn user_accented(line: RenderedTextLine) -> RenderedTextLine {
    let mut spans = vec![RenderedTextSpan {
        text: "▎ ".to_string(),
        style: TextPanelSpanStyle::User,
        syntax_style: None,
        link: None,
    }];
    spans.extend(line.spans);
    RenderedTextLine { spans }
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
    }
    if let Some(concrete) = &segment.style {
        style.fg = concrete.fg.or(style.fg);
        style.bg = concrete.bg.or(style.bg);
        style.bold = concrete.bold;
        style.italic = concrete.italic;
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

        assert_eq!(manager.reserved_left_width(), 25);
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

        assert_eq!(manager.reserved_right_width(), 25);
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
        assert_eq!(manager.reserved_left_width(), 13);
        assert_eq!(manager.reserved_right_width(), 13);
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

        assert_eq!(manager.reserved_right_width(), 10);
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
                },
                RenderedTextSpan {
                    text: "b".to_string(),
                    style: TextPanelSpanStyle::Code,
                    syntax_style: Some(Style {
                        bg: Some(syntax_background),
                        ..Style::default()
                    }),
                    link: None,
                },
            ],
        };
        let mut buffer = RenderBuffer::new(2, 1, &theme.style);

        render_text_spans(&mut buffer, 0, 0, 2, &line, None, &theme);

        assert_eq!(
            buffer.cells[0].style,
            Style {
                fg: Some(syntax_foreground),
                bg: Some(panel_background),
                bold: true,
                italic: true,
            }
        );
        assert_eq!(
            buffer.cells[1].style,
            Style {
                fg: Some(code_foreground),
                bg: Some(syntax_background),
                bold: true,
                italic: false,
            }
        );
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
            &Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
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
            (KeyCode::Char('j'), KeyModifiers::CONTROL),
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
        let mut buffer = RenderBuffer::new(80, 20, &theme.style);

        manager.render(&mut buffer, &theme);
        assert!((0..20).any(|row| row_text(&buffer, row).contains("INSERT")));

        manager.handle_focused_text_input(
            &Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            80,
        );
        manager.render(&mut buffer, &theme);
        assert!((0..20).any(|row| row_text(&buffer, row).contains("NORMAL")));

        let blur = manager
            .handle_focused_text_input(
                &Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
                80,
            )
            .unwrap();
        assert_eq!(blur.action, "composer_blur");
        assert!(!manager.focused_text_input_active());
    }

    #[test]
    fn text_panel_composer_shrinks_on_narrow_terminals_and_keeps_tail_visible() {
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
        let placement = manager.panel_at_position(29, 0, 30, 12).unwrap();
        assert_eq!(placement.width, 19);
        assert_eq!(placement.x, 11);
        assert!(manager.panel_at_position(9, 0, 30, 12).is_none());

        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(30, 12, &theme.style);
        manager.render(&mut buffer, &theme);
        assert!((1..6).any(|row| row_text(&buffer, row).contains("LATEST")));
        assert!((6..10).any(|row| row_text(&buffer, row).contains("Ask")));
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
                    },
                    TextPanelHeaderAction {
                        id: "new".to_string(),
                        label: "New".to_string(),
                        compact_label: Some("N".to_string()),
                    },
                    TextPanelHeaderAction {
                        id: "close".to_string(),
                        label: "×".to_string(),
                        compact_label: Some("×".to_string()),
                    },
                ],
            },
        );
        let theme = Theme::default();
        let mut wide = RenderBuffer::new(80, 20, &theme.style);
        manager.render(&mut wide, &theme);
        let wide_header = row_text(&wide, 0);
        assert!(wide_header.contains("Agent"));
        assert!(wide_header.contains("[Clear] [New] [×]"));

        for (label, expected) in [("[Clear]", "clear"), ("[New]", "new"), ("[×]", "close")] {
            let start = wide_header.find(label).unwrap();
            let column = display_width(&wide_header[..start]) + 1;
            let event = manager.focus_panel_at_position(column, 0, 80, 20).unwrap();
            assert_eq!(event.action, expected);
        }

        let mut narrow = RenderBuffer::new(30, 12, &theme.style);
        manager.render(&mut narrow, &theme);
        assert!(row_text(&narrow, 0).contains("[C] [N] [×]"));

        let actions = text_panel_header_actions(&manager.text_panels["agent"].config, 4);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].1, "close");
    }

    #[test]
    fn text_panel_links_support_keyboard_navigation_and_clicks() {
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

        assert!(manager.select_focused_text_link(true, 18, 80));
        assert_eq!(
            manager.focused_text_link_target(80),
            Some(TextPanelLinkTarget::ExternalUrl(
                "https://example.com".to_string()
            ))
        );
        assert!(manager.select_focused_text_link(true, 18, 80));
        assert_eq!(
            manager.focused_text_link_target(80),
            Some(TextPanelLinkTarget::File {
                path: "src/main.rs".to_string(),
                location: None,
            })
        );
        assert!(manager.select_focused_text_link(false, 18, 80));
        assert_eq!(
            manager.focused_text_link_target(80),
            Some(TextPanelLinkTarget::ExternalUrl(
                "https://example.com".to_string()
            ))
        );

        let placement = manager.panel_at_position(40, 0, 80, 20).unwrap();
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

        let event = manager.focus_panel_at_position(53, 15, 80, 20).unwrap();
        assert_eq!(event.action, "composer_focus");
        manager.handle_focused_text_input(&Event::Paste("X".to_string()), 80);

        let composer = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert_eq!(composer.prompt.text(), "first line\nsecXond line");
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

        assert!(manager.set_panel_visible("agent", false));
        assert_eq!(manager.reserved_right_width(), 0);
        assert_eq!(manager.focused_panel_id(), None);
        assert!(!manager.focus_text_panel_composer("agent"));

        assert!(manager.set_panel_visible("agent", true));
        assert_eq!(manager.reserved_right_width(), 25);
        assert!(manager.focus_text_panel_composer("agent"));
        let composer = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert_eq!(composer.prompt.text(), "keep this draft");
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
            100,
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
        let mut buffer = RenderBuffer::new(100, 15, &theme.style);

        manager.render(&mut buffer, &theme);

        let status_row = row_text(&buffer, 8);
        assert!(status_row.contains("⠋ Reading demo.txt · 0s"));
        assert!(row_text(&buffer, 9).contains("────"));
        assert!((1..8).any(|row| row_text(&buffer, row).contains("partial answer▌")));

        assert!(manager.set_text_panel_status("agent", None));
        let mut buffer = RenderBuffer::new(100, 15, &theme.style);
        manager.render(&mut buffer, &theme);
        assert!(!row_text(&buffer, 8).contains("Reading demo.txt"));
        assert!((1..9).any(|row| row_text(&buffer, row).contains("partial answer")));
        assert!(!(1..9).any(|row| row_text(&buffer, row).contains("partial answer▌")));
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
        assert!(joined.contains("▎ You"));
        assert!(joined.contains("✓ Read demo.txt"));
        assert!(!joined.contains("❯ You"));
        let separator_rows = rendered.iter().filter(|row| row.contains("────")).count();
        assert_eq!(separator_rows, 1);
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
        assert_eq!(manager.reserved_right_width(), 17);

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
            80,
        );
        assert!(manager.focus_panel("agent"));
        manager.handle_focused_key("top", 15, 80, 0).unwrap();

        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(80, 15, &theme.style);
        manager.render(&mut buffer, &theme);
        let top = (0..15)
            .map(|row| row_text(&buffer, row))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(top.contains("↓ more · j/k scroll · G latest"));

        manager.handle_focused_key("bottom", 15, 80, 0).unwrap();
        let mut buffer = RenderBuffer::new(80, 15, &theme.style);
        manager.render(&mut buffer, &theme);
        let bottom = (0..15)
            .map(|row| row_text(&buffer, row))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(bottom.contains("↑ history · j/k scroll · g oldest"));
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

        assert!(row_text(&buffer, 0).contains("│▎ You"));
        assert!(row_text(&buffer, 1).contains("│▎ hello"));
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
}
