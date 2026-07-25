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
use unicode_segmentation::UnicodeSegmentation;

use super::markdown::{
    render_markdown_lines, render_markdown_lines_with_highlighter, wrap_plain_text,
    RenderedTextLine, RenderedTextSpan, TextPanelSpanStyle,
};
use super::text_link::{TextPanelLink, TextPanelLinkTarget};
use crate::{
    editor::{render_buffer::RenderBuffer, Point},
    highlighter::Highlighter,
    theme::{SelectionForegroundPriority, Style, Theme, ThemeStyleSpec},
    ui::{wrap_text, ModalComposer, ModalComposerMode, ModalComposerOutcome},
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum TextPanelFocus {
    #[default]
    Conversation,
    Composer,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TranscriptCursor {
    row: usize,
    grapheme: usize,
    preferred_column: usize,
}

pub struct TextPanel {
    pub id: String,
    pub config: PanelConfig,
    pub blocks: Vec<TextPanelBlock>,
    pub scroll: usize,
    pub follow_tail: bool,
    focus: TextPanelFocus,
    transcript_cursor: TranscriptCursor,
    composer: Option<TextPanelComposer>,
    status: Option<TextPanelStatus>,
    busy_since: Option<Instant>,
    selected_link: Option<u64>,
}

const TEXT_PANEL_SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TEXT_PANEL_ASCII_SPINNER_FRAMES: [&str; 4] = ["|", "/", "-", "\\"];
const TEXT_PANEL_SPINNER_INTERVAL_MS: u64 = 120;

fn spinner_frame(elapsed_ms: u64, use_ascii: bool) -> &'static str {
    let index = (elapsed_ms / TEXT_PANEL_SPINNER_INTERVAL_MS) as usize;
    if use_ascii {
        TEXT_PANEL_ASCII_SPINNER_FRAMES[index % TEXT_PANEL_ASCII_SPINNER_FRAMES.len()]
    } else {
        TEXT_PANEL_SPINNER_FRAMES[index % TEXT_PANEL_SPINNER_FRAMES.len()]
    }
}

fn format_elapsed(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    }
}

struct TextPanelComposer {
    config: TextPanelComposerConfig,
    composer: ModalComposer,
    focused: bool,
    enabled: bool,
    status: Option<String>,
}

impl TextPanelComposer {
    fn new(config: TextPanelComposerConfig) -> Self {
        Self {
            config,
            composer: ModalComposer::new("", Vec::new()),
            focused: false,
            enabled: true,
            status: None,
        }
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
            focus: TextPanelFocus::Conversation,
            transcript_cursor: TranscriptCursor::default(),
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

    fn focus_conversation(&mut self) {
        self.focus = TextPanelFocus::Conversation;
        if let Some(composer) = self.composer.as_mut() {
            composer.focused = false;
        }
    }

    fn focus_composer(&mut self) -> bool {
        let Some(composer) = self.composer.as_mut() else {
            return false;
        };
        if !composer.enabled {
            return false;
        }

        composer.focused = true;
        self.focus = TextPanelFocus::Composer;
        true
    }

    fn composer_is_focused(&self) -> bool {
        self.focus == TextPanelFocus::Composer
            && self
                .composer
                .as_ref()
                .is_some_and(|composer| composer.focused && composer.enabled)
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
            self.scroll = 0;
            self.follow_tail = true;
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
        self.scroll = self.scroll.saturating_add_signed(delta).min(max_scroll);
        self.follow_tail = self.scroll == max_scroll;
        self.sync_transcript_cursor(panel_height, panel_width);
    }

    fn page_scroll(&mut self, delta: isize, panel_height: usize, panel_width: usize) {
        let page = isize::try_from(self.visible_rows(panel_height)).unwrap_or(isize::MAX);
        self.move_transcript_cursor(delta.saturating_mul(page), panel_height, panel_width);
    }

    fn scroll_to_top(&mut self) {
        self.scroll = 0;
        self.follow_tail = false;
        self.transcript_cursor = TranscriptCursor::default();
    }

    fn scroll_to_bottom(&mut self, panel_height: usize, panel_width: usize) {
        self.scroll = self.max_scroll(panel_height, panel_width);
        self.follow_tail = true;
        self.sync_transcript_cursor(panel_height, panel_width);
    }

    fn clamp_scroll(&mut self, panel_height: usize, panel_width: usize) {
        self.scroll = self.scroll.min(self.max_scroll(panel_height, panel_width));
        self.sync_transcript_cursor(panel_height, panel_width);
    }

    fn sync_transcript_cursor(&mut self, panel_height: usize, panel_width: usize) {
        let lines = self.rendered_lines(panel_width.max(1));
        let visible_rows = self.visible_rows(panel_height);
        let max_scroll = lines.len().saturating_sub(visible_rows);
        self.scroll = self.scroll.min(max_scroll);

        if lines.is_empty() {
            self.transcript_cursor = TranscriptCursor::default();
            self.scroll = 0;
            return;
        }

        let last_row = lines.len().saturating_sub(1);
        if self.follow_tail {
            self.scroll = max_scroll;
            self.transcript_cursor.row = last_row;
        } else {
            let last_visible_row = self
                .scroll
                .saturating_add(visible_rows.saturating_sub(1))
                .min(last_row);
            self.transcript_cursor.row = self
                .transcript_cursor
                .row
                .min(last_row)
                .clamp(self.scroll, last_visible_row);
        }

        self.transcript_cursor.grapheme = transcript_grapheme_at_column(
            &lines[self.transcript_cursor.row],
            self.transcript_cursor.preferred_column,
        );
    }

    fn set_transcript_cursor(
        &mut self,
        row: usize,
        column: usize,
        panel_height: usize,
        panel_width: usize,
    ) {
        let lines = self.rendered_lines(panel_width.max(1));
        if lines.is_empty() {
            self.transcript_cursor = TranscriptCursor::default();
            self.scroll = 0;
            self.follow_tail = true;
            return;
        }

        let visible_rows = self.visible_rows(panel_height);
        let last_row = lines.len().saturating_sub(1);
        let row = row.min(last_row);
        let max_scroll = lines.len().saturating_sub(visible_rows);
        self.scroll = self.scroll.min(max_scroll);
        if row < self.scroll {
            self.scroll = row;
        } else if row >= self.scroll.saturating_add(visible_rows) {
            self.scroll = row.saturating_sub(visible_rows.saturating_sub(1));
        }

        self.transcript_cursor = TranscriptCursor {
            row,
            grapheme: transcript_grapheme_at_column(&lines[row], column),
            preferred_column: column,
        };
        self.follow_tail = row == last_row;
        if self.follow_tail {
            self.scroll = max_scroll;
        }
    }

    fn move_transcript_cursor(&mut self, delta: isize, panel_height: usize, panel_width: usize) {
        self.sync_transcript_cursor(panel_height, panel_width);
        let row = self.transcript_cursor.row.saturating_add_signed(delta);
        self.set_transcript_cursor(
            row,
            self.transcript_cursor.preferred_column,
            panel_height,
            panel_width,
        );
        self.selected_link = None;
    }

    fn move_transcript_horizontally(
        &mut self,
        delta: isize,
        panel_height: usize,
        panel_width: usize,
    ) {
        self.sync_transcript_cursor(panel_height, panel_width);
        let lines = self.rendered_lines(panel_width.max(1));
        let Some(line) = lines.get(self.transcript_cursor.row) else {
            return;
        };

        let last_grapheme = line
            .spans
            .iter()
            .flat_map(|span| span.text.graphemes(true))
            .count()
            .saturating_sub(1);
        self.transcript_cursor.grapheme = self
            .transcript_cursor
            .grapheme
            .saturating_add_signed(delta)
            .min(last_grapheme);
        self.transcript_cursor.preferred_column =
            transcript_grapheme_column(line, self.transcript_cursor.grapheme);
        self.selected_link = None;
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
        let selected_id = link.id;
        let line_index = *line;
        let column = self
            .rendered_lines(width)
            .get(line_index)
            .map(|line| {
                line.spans
                    .iter()
                    .take_while(|span| span.link.as_ref().is_none_or(|link| link.id != selected_id))
                    .map(|span| display_width(&span.text))
                    .sum()
            })
            .unwrap_or_default();

        self.set_transcript_cursor(line_index, column, panel_height, width);
        self.selected_link = Some(selected_id);
        self.follow_tail = false;
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
        self.rendered_lines_with_highlighter(width, None)
    }

    fn rendered_lines_with_highlighter(
        &self,
        width: usize,
        mut highlighter: Option<&mut Highlighter>,
    ) -> Vec<RenderedTextLine> {
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
                    TextPanelBlockFormat::Markdown => match highlighter.as_deref_mut() {
                        Some(highlighter) => render_markdown_lines_with_highlighter(
                            &block.text,
                            content_width,
                            Some(highlighter),
                        ),
                        None => render_markdown_lines(&block.text, content_width),
                    },
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
                    TextPanelBlockFormat::Markdown => match highlighter.as_deref_mut() {
                        Some(highlighter) => render_markdown_lines_with_highlighter(
                            &block.text,
                            width,
                            Some(highlighter),
                        ),
                        None => render_markdown_lines(&block.text, width),
                    },
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

fn transcript_grapheme_at_column(line: &RenderedTextLine, target_column: usize) -> usize {
    let mut column = 0usize;
    let mut last_grapheme = 0usize;

    for (index, grapheme) in line
        .spans
        .iter()
        .flat_map(|span| span.text.graphemes(true))
        .enumerate()
    {
        let next_column = column.saturating_add(display_width(grapheme));
        if target_column < next_column {
            return index;
        }
        column = next_column;
        last_grapheme = index;
    }

    last_grapheme
}

fn transcript_grapheme_column(line: &RenderedTextLine, grapheme_index: usize) -> usize {
    line.spans
        .iter()
        .flat_map(|span| span.text.graphemes(true))
        .take(grapheme_index)
        .map(display_width)
        .sum()
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

#[derive(Default)]
pub struct PanelManager {
    panels: HashMap<String, PluginPanel>,
    text_panels: HashMap<String, TextPanel>,
    z_order: Vec<String>,
    focused: Option<String>,
    animation_state: Vec<(String, u8, u64)>,
}

struct PanelRenderOptions<'a> {
    highlighter: Option<&'a mut Highlighter>,
    use_ascii: bool,
}

#[derive(Clone, Copy)]
struct TextPanelRenderStyle<'a> {
    theme: &'a Theme,
    surface: &'a Style,
}

impl PanelManager {
    pub fn create_panel(&mut self, id: String, config: PanelConfig) {
        self.text_panels.remove(&id);
        self.panels
            .insert(id.clone(), PluginPanel::new(id.clone(), config));
        if !self.z_order.contains(&id) {
            self.z_order.push(id.clone());
        }
    }

    pub fn create_text_panel(&mut self, id: String, config: PanelConfig) {
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

    /// Moves an existing panel without replacing its rows, draft, or focus.
    pub fn update_panel_layout(&mut self, id: &str, side: PanelSide, width: usize) -> bool {
        if let Some(panel) = self.panels.get_mut(id) {
            if panel.config.side == side && panel.config.width == width {
                return false;
            }
            panel.config.side = side;
            panel.config.width = width;
            return true;
        }
        if let Some(panel) = self.text_panels.get_mut(id) {
            if panel.config.side == side && panel.config.width == width {
                return false;
            }
            panel.config.side = side;
            panel.config.width = width;
            return true;
        }
        false
    }

    pub fn close_panel(&mut self, id: &str) {
        self.panels.remove(id);
        self.text_panels.remove(id);
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
            if let Some(panel) = self.text_panels.get_mut(id) {
                panel.focus_conversation();
            }
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
            if let Some(panel) = self.text_panels.get_mut(id) {
                panel.focus_conversation();
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
            .is_some_and(TextPanel::composer_is_focused)
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
        if side == PanelSide::Right {
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

    /// Number of rows reserved by visible panels docked above the editor.
    pub fn reserved_top_height(&self) -> usize {
        self.z_order
            .iter()
            .filter_map(|id| self.panel_config(id))
            .filter(|config| config.side == PanelSide::Top)
            .map(|config| config.width.saturating_add(1))
            .sum()
    }

    /// Number of rows reserved by visible panels docked below the editor.
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
                "up" => panel.move_transcript_cursor(-1, panel_height, width),
                "down" => panel.move_transcript_cursor(1, panel_height, width),
                "page_up" => {
                    panel.page_scroll(-1, panel_height, width);
                }
                "page_down" => {
                    panel.page_scroll(1, panel_height, width);
                }
                "top" => panel.scroll_to_top(),
                "bottom" => panel.scroll_to_bottom(panel_height, width),
                "left" | "collapse" => {
                    panel.move_transcript_horizontally(-1, panel_height, width);
                }
                "right" | "expand" => {
                    panel.move_transcript_horizontally(1, panel_height, width);
                }
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
        let panel_height = effective_panel_height(&panel.config, panel_height);

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
        let panel_height = effective_panel_height(&panel.config, panel_height);
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
        panel.focus_conversation();
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
                let selected_id = link.id;
                let target = link.target.clone();
                self.focused = Some(placement.id);
                panel.focus_conversation();
                panel.set_transcript_cursor(
                    scroll + screen_row - title_rows,
                    column,
                    placement.height,
                    placement.width,
                );
                panel.selected_link = Some(selected_id);
                return Some(target);
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
        let Some(panel) = self.text_panels.get_mut(id) else {
            return false;
        };
        if !panel.focus_composer() {
            return false;
        }
        self.focused = Some(id.to_string());
        true
    }

    pub fn set_text_panel_composer_state(
        &mut self,
        id: &str,
        enabled: bool,
        status: Option<String>,
    ) -> bool {
        let Some(panel) = self.text_panels.get_mut(id) else {
            return false;
        };
        let Some(composer) = panel.composer.as_mut() else {
            return false;
        };
        composer.enabled = enabled;
        composer.status = status;
        if !enabled {
            panel.focus_conversation();
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
        composer.composer.set_contents("")
    }

    pub fn handle_focused_text_input(
        &mut self,
        event: &Event,
        _terminal_width: usize,
    ) -> Option<PanelEvent> {
        let focused = self.focused.clone()?;
        let panel = self.text_panels.get_mut(&focused)?;
        if !panel.composer_is_focused() {
            return None;
        }

        if matches!(
            event,
            Event::Key(key)
                if matches!(key.code, KeyCode::Char('c' | 'C'))
                    && key.modifiers.contains(KeyModifiers::CONTROL)
        ) {
            panel.focus_conversation();
            return Some(PanelEvent {
                panel_id: panel.id.clone(),
                action: "composer_blur".to_string(),
                selected_index: panel.scroll,
                row: None,
                text: None,
            });
        }

        let composer = panel.composer.as_mut()?;
        let outcome = match event {
            Event::Paste(pasted) => composer.composer.handle_paste(pasted),
            Event::Key(key) => composer.composer.handle_key(*key),
            _ => return None,
        };

        let (action, text) = match outcome {
            ModalComposerOutcome::Submit => match composer.composer.take_submission() {
                Some(text) => ("submit", Some(text)),
                None => ("composer_input", None),
            },
            ModalComposerOutcome::Changed | ModalComposerOutcome::Rejected => {
                ("composer_input", None)
            }
            ModalComposerOutcome::Unhandled => return None,
        };

        Some(PanelEvent {
            panel_id: panel.id.clone(),
            action: action.to_string(),
            selected_index: panel.scroll,
            row: None,
            text,
        })
    }

    /// Returns the real Vim mode of the focused conversation or text composer.
    pub(crate) fn focused_text_panel_cursor_mode(&self) -> Option<crate::editor::Mode> {
        let id = self.focused.as_deref()?;
        let panel = self.text_panels.get(id)?;

        if panel.composer_is_focused() {
            let composer = panel.composer.as_ref()?;
            Some(match composer.composer.mode() {
                ModalComposerMode::Normal => crate::editor::Mode::Normal,
                ModalComposerMode::Insert => crate::editor::Mode::Insert,
                ModalComposerMode::Visual => crate::editor::Mode::Visual,
            })
        } else {
            Some(crate::editor::Mode::Normal)
        }
    }

    /// Returns the visible cursor in the focused conversation or text composer.
    pub fn focused_text_panel_cursor_position(
        &self,
        terminal_width: usize,
        terminal_height: usize,
    ) -> Option<(usize, usize)> {
        let id = self.focused.as_deref()?;
        let panel = self.text_panels.get(id)?;
        let placement = self
            .panel_placements(terminal_width, terminal_height)
            .into_iter()
            .find(|placement| placement.id == id)?;
        if placement.width == 0 || placement.height == 0 {
            return None;
        }

        if panel.composer_is_focused() {
            let composer = panel.composer.as_ref()?;
            let content_width = placement.width.saturating_sub(2).max(1);
            let draft = composer.composer.contents();
            let wrapped = wrap_text(&draft, content_width);
            let (row, column) = wrapped
                .positions
                .get(composer.composer.cursor_grapheme_index())
                .copied()
                .unwrap_or_default();
            let rows = composer.config.rows.max(1);
            let first = row.saturating_sub(rows.saturating_sub(1));
            let top = placement
                .y
                .saturating_add(placement.height.saturating_sub(panel.composer_height()));
            let x = placement.x.saturating_add(1).saturating_add(column);
            let y = top
                .saturating_add(1)
                .saturating_add(row.saturating_sub(first));

            return (x < placement.x.saturating_add(placement.width)
                && y < placement.y.saturating_add(placement.height))
            .then_some((x, y));
        }

        let title_rows =
            usize::from(panel.config.title.is_some() || !panel.config.header_actions.is_empty());
        let visible_rows = placement
            .height
            .saturating_sub(panel.composer_height())
            .saturating_sub(panel.status_height())
            .saturating_sub(title_rows);
        if visible_rows == 0 {
            return None;
        }

        let lines = panel.rendered_lines(placement.width);
        let max_scroll = lines.len().saturating_sub(visible_rows);
        let scroll = if panel.follow_tail {
            max_scroll
        } else {
            panel.scroll.min(max_scroll)
        };
        let last_visible_row = scroll.saturating_add(visible_rows.saturating_sub(1));
        let row = if lines.is_empty() {
            0
        } else {
            let last_row = lines.len().saturating_sub(1);
            let anchor = if panel.follow_tail {
                last_row
            } else {
                panel.transcript_cursor.row.min(last_row)
            };
            anchor.clamp(scroll, last_visible_row.min(last_row))
        };
        let column = lines
            .get(row)
            .map(|line| {
                let grapheme =
                    transcript_grapheme_at_column(line, panel.transcript_cursor.preferred_column);
                transcript_grapheme_column(line, grapheme)
            })
            .unwrap_or_default()
            .min(placement.width.saturating_sub(1));

        Some((
            placement.x.saturating_add(column),
            placement
                .y
                .saturating_add(title_rows)
                .saturating_add(row.saturating_sub(scroll)),
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
            let action = if y >= composer_top && panel.focus_composer() {
                if let Some(composer) = panel.composer.as_mut() {
                    let content_width = placement.width.saturating_sub(2).max(1);
                    let draft = composer.composer.contents();
                    let wrapped = wrap_text(&draft, content_width);
                    let cursor_row = wrapped
                        .positions
                        .get(composer.composer.cursor_grapheme_index())
                        .map_or(0, |position| position.0);
                    let rows = composer.config.rows.max(1);
                    let first = cursor_row.saturating_sub(rows.saturating_sub(1));
                    let row = first.saturating_add(y.saturating_sub(composer_top + 1));
                    let column = x.saturating_sub(placement.x.saturating_add(1));
                    if let Some((index, _)) = wrapped
                        .positions
                        .iter()
                        .enumerate()
                        .filter(|(_, position)| position.0 == row)
                        .min_by_key(|(_, position)| position.1.abs_diff(column))
                    {
                        composer.composer.set_cursor_grapheme_index(index);
                    }
                }
                "composer_focus"
            } else {
                panel.focus_conversation();
                let title_rows = usize::from(
                    panel.config.title.is_some() || !panel.config.header_actions.is_empty(),
                );
                let visible_rows = placement
                    .height
                    .saturating_sub(panel.composer_height())
                    .saturating_sub(panel.status_height())
                    .saturating_sub(title_rows);
                if visible_rows > 0 {
                    let lines = panel.rendered_lines(placement.width);
                    let max_scroll = lines.len().saturating_sub(visible_rows);
                    let scroll = if panel.follow_tail {
                        max_scroll
                    } else {
                        panel.scroll.min(max_scroll)
                    };
                    let offset = y
                        .saturating_sub(placement.y.saturating_add(title_rows))
                        .min(visible_rows.saturating_sub(1));
                    panel.set_transcript_cursor(
                        scroll.saturating_add(offset),
                        x.saturating_sub(placement.x),
                        placement.height,
                        placement.width,
                    );
                }
                panel.selected_link = None;
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
        self.render_with_options(
            buffer,
            theme,
            PanelRenderOptions {
                highlighter: None,
                use_ascii: false,
            },
        );
    }

    /// Renders theme-aware docks and syntax-highlighted fenced Markdown.
    pub fn render_with_highlighter(
        &self,
        buffer: &mut RenderBuffer,
        theme: &Theme,
        highlighter: &mut Highlighter,
        use_ascii: bool,
    ) {
        self.render_with_options(
            buffer,
            theme,
            PanelRenderOptions {
                highlighter: Some(highlighter),
                use_ascii,
            },
        );
    }

    fn render_with_options(
        &self,
        buffer: &mut RenderBuffer,
        theme: &Theme,
        mut options: PanelRenderOptions<'_>,
    ) {
        for placement in self.panel_placements(buffer.width, buffer.height) {
            let Some(config) = self.panel_config(&placement.id) else {
                continue;
            };
            let position = Point::new(placement.x, placement.y);
            let border_style = panel_style(theme, config.border.as_ref());
            let bordered = config.border.is_some() || self.text_panels.contains_key(&placement.id);
            let separator = if !bordered {
                " "
            } else {
                match (config.side, options.use_ascii) {
                    (PanelSide::Left | PanelSide::Right, false) => "│",
                    (PanelSide::Left | PanelSide::Right, true) => "|",
                    (PanelSide::Top | PanelSide::Bottom, false) => "─",
                    (PanelSide::Top | PanelSide::Bottom, true) => "-",
                }
            };
            render_panel_separator(
                buffer,
                position,
                placement.width,
                placement.height,
                &config.side,
                &border_style,
                separator,
            );

            if let Some(panel) = self.panels.get(&placement.id) {
                render_panel(
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
                    &mut options,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelPlacement {
    pub id: String,
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

fn render_panel(
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
    options: &mut PanelRenderOptions<'_>,
) {
    if width == 0 || height == 0 {
        return;
    }

    let surface_style = panel_style(theme, panel.config.surface.as_ref());
    let render_style = TextPanelRenderStyle {
        theme,
        surface: &surface_style,
    };
    for y in 0..height {
        buffer.set_text(
            position.x,
            position.y.saturating_add(y),
            &" ".repeat(width),
            &surface_style,
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
            ..surface_style.clone()
        };
        buffer.set_text(
            position.x,
            position.y,
            &fit_display_width(title, title_width),
            &title_style,
        );
    }
    let muted_style = theme.ui_style.muted.with_bg(surface_style.bg);
    let action_style = theme.ui_style.picker_prompt.with_bg(surface_style.bg);
    for (start, _, label) in header_actions {
        let x = position.x + start;
        buffer.set_text(x, position.y, "[", &muted_style);
        buffer.set_text(x + 1, position.y, label, &action_style);
        buffer.set_text(x + 1 + display_width(label), position.y, "]", &muted_style);
    }

    let composer_height = panel.composer_height();
    let status_height = panel.status_height();
    let content_height = height
        .saturating_sub(composer_height)
        .saturating_sub(status_height);
    let visible_rows = content_height.saturating_sub(title_rows);
    let lines = panel.rendered_lines_with_highlighter(width, options.highlighter.as_deref_mut());
    let max_scroll = lines.len().saturating_sub(visible_rows);
    let scroll = if panel.follow_tail {
        max_scroll
    } else {
        panel.scroll.min(max_scroll)
    };
    for (offset, line) in lines.iter().skip(scroll).take(visible_rows).enumerate() {
        render_text_spans(
            buffer,
            position.x,
            position.y.saturating_add(title_rows + offset),
            width,
            line,
            panel.selected_link,
            render_style,
        );
    }

    if panel.status.is_some() {
        render_text_panel_status(
            buffer,
            panel,
            position,
            width,
            content_height,
            render_style,
            options.use_ascii,
        );
    }

    if let Some(composer) = &panel.composer {
        render_text_panel_composer(
            buffer,
            composer,
            position,
            width,
            content_height + status_height,
            render_style,
            options.use_ascii,
        );
    }
}

fn text_panel_composer_hints(composer: &TextPanelComposer, width: usize) -> String {
    let (mode, candidates): (&str, [&str; 4]) = if composer.focused && composer.enabled {
        match composer.composer.mode() {
            ModalComposerMode::Insert => (
                "INSERT",
                [
                    "Enter newline | Ctrl+Enter send | Alt+Enter send | Esc normal | ^P/^N history",
                    "Ctrl+Enter send | Enter newline | Esc normal",
                    "Ctrl+Enter send",
                    "C-Enter send",
                ],
            ),
            ModalComposerMode::Normal => (
                "NORMAL",
                [
                    "Ctrl+Enter/Alt+Enter send | Enter send | i edit | u undo | ^P/^N history",
                    "Ctrl+Enter send | Enter send | i edit",
                    "Ctrl+Enter send | i edit",
                    "C-Enter send",
                ],
            ),
            ModalComposerMode::Visual => (
                "VISUAL",
                [
                    "Ctrl+Enter send | hjkl select | d/c/y operate | Esc normal",
                    "Ctrl+Enter send | hjkl select | Esc normal",
                    "Ctrl+Enter send",
                    "C-Enter send",
                ],
            ),
        }
    } else {
        (
            "READ",
            [
                "j/k navigate | i/a edit | Esc editor | x clear | N new",
                "j/k navigate | i/a edit | Esc editor",
                "j/k | i edit | Esc",
                "i edit",
            ],
        )
    };

    for shortcuts in candidates {
        let hint = format!("{mode}  {shortcuts}");
        if display_width(&hint) <= width {
            return hint;
        }
    }

    mode.to_string()
}

fn render_text_panel_composer(
    buffer: &mut RenderBuffer,
    composer: &TextPanelComposer,
    position: Point,
    width: usize,
    top: usize,
    render_style: TextPanelRenderStyle<'_>,
    use_ascii: bool,
) {
    if width == 0 {
        return;
    }
    let theme = render_style.theme;
    let surface_style = render_style.surface;
    let top = position.y.saturating_add(top);
    let divider = if use_ascii { "-" } else { "─" }.repeat(width);
    let divider_style = theme.ui_style.muted.with_bg(surface_style.bg);
    buffer.set_text(
        position.x,
        top,
        &fit_display_width(&divider, width),
        &divider_style,
    );

    let rows = composer.config.rows.max(1);
    let content_width = width.saturating_sub(2).max(1);
    let draft = composer.composer.contents();
    let wrapped = wrap_text(&draft, content_width);
    let cursor_row = wrapped
        .positions
        .get(composer.composer.cursor_grapheme_index())
        .map_or(0, |position| position.0);
    let first = cursor_row.saturating_sub(rows.saturating_sub(1));
    for row in 0..rows {
        let y = top + 1 + row;
        let line = wrapped
            .rows
            .get(first + row)
            .map(String::as_str)
            .unwrap_or("");
        let placeholder = line.is_empty() && draft.is_empty() && row == 0;
        let text = if placeholder {
            composer.config.placeholder.as_str()
        } else {
            line
        };
        let input_style = if composer.enabled && composer.focused {
            theme.ui_style.dialog.with_bg(surface_style.bg)
        } else {
            theme.ui_style.muted.with_bg(surface_style.bg)
        };
        let placeholder_style = theme.ui_style.muted.with_bg(surface_style.bg);
        let text_style = if placeholder {
            &placeholder_style
        } else {
            &input_style
        };
        buffer.set_text(position.x, y, &" ".repeat(width), &input_style);
        buffer.set_text(
            position.x.saturating_add(1),
            y,
            &fit_display_width(text, content_width),
            text_style,
        );
    }
    let hints = text_panel_composer_hints(composer, width);
    let status = composer
        .composer
        .validation_status()
        .or(composer.status.as_deref());
    let status = match status {
        Some(status) => format!("{status} | {hints}"),
        None => hints,
    };
    let footer_y = top + rows + 1;
    let footer_style = &theme.ui_style.muted;
    buffer.set_text(position.x, footer_y, &" ".repeat(width), footer_style);
    buffer.set_text(
        position.x,
        footer_y,
        &fit_display_width(&status, width),
        footer_style,
    );
}

fn render_text_panel_status(
    buffer: &mut RenderBuffer,
    panel: &TextPanel,
    position: Point,
    width: usize,
    y: usize,
    render_style: TextPanelRenderStyle<'_>,
    use_ascii: bool,
) {
    if width == 0 {
        return;
    }
    let theme = render_style.theme;
    let surface_style = render_style.surface;
    let Some(status) = panel.status.as_ref() else {
        return;
    };
    let (text, style) = if status.busy {
        let elapsed_ms = panel
            .busy_since
            .map_or(0, |since| since.elapsed().as_millis() as u64);
        (
            format!(
                "{} {} · {}",
                spinner_frame(elapsed_ms, use_ascii),
                status.label,
                format_elapsed(elapsed_ms / 1000)
            ),
            theme.ui_style.picker_prompt.with_bg(surface_style.bg),
        )
    } else {
        (
            status.label.clone(),
            theme.ui_style.muted.with_bg(surface_style.bg),
        )
    };
    buffer.set_text(
        position.x,
        position.y.saturating_add(y),
        &fit_display_width(&text, width),
        &style,
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
    render_style: TextPanelRenderStyle<'_>,
) {
    let theme = render_style.theme;
    let surface_style = render_style.surface;
    let mut used = 0;
    for span in &line.spans {
        if used >= width {
            break;
        }
        let text = truncate_display_width(&span.text, width - used);
        if text.is_empty() {
            continue;
        }
        let mut style = span
            .syntax_style
            .clone()
            .unwrap_or_else(|| text_panel_span_style(span.style, theme));
        style.fg = style.fg.or(surface_style.fg);
        style.bg = if span.syntax_style.is_some()
            || matches!(
                span.style,
                TextPanelSpanStyle::InlineCode | TextPanelSpanStyle::Code
            ) {
            style.bg.or(surface_style.bg)
        } else {
            surface_style.bg
        };
        if span
            .link
            .as_ref()
            .is_some_and(|link| Some(link.id) == selected_link)
        {
            let selection = theme.list_selection_style();
            style = theme.selected_style(&style, &selection, SelectionForegroundPriority::Content);
        }
        buffer.set_text(x + used, y, &text, &style);
        used += display_width(&text);
    }
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
    side: &PanelSide,
    style: &Style,
    separator: &str,
) {
    match side {
        PanelSide::Left | PanelSide::Right => {
            let separator_x = if *side == PanelSide::Left {
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
            let separator_y = if *side == PanelSide::Top {
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

    fn focused_agent_composer() -> PanelManager {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 56,
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
        manager
    }

    fn composer_key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(crossterm::event::KeyEvent::new(code, modifiers))
    }

    fn focused_agent_conversation(side: PanelSide) -> PanelManager {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side,
                width: if matches!(side, PanelSide::Top | PanelSide::Bottom) {
                    10
                } else {
                    24
                },
                title: Some("Agent".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask a follow-up…".to_string(),
                    rows: 2,
                }),
                ..PanelConfig::default()
            },
        );
        assert!(manager.focus_panel("agent"));
        manager
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
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Top,
                width: 4,
                title: Some("Top agent".to_string()),
                ..PanelConfig::default()
            },
        );
        manager.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Plain,
                text: "LATEST horizontal response".to_string(),
            }],
            10,
            32,
        );

        assert_eq!(manager.reserved_top_height(), 5);
        assert_eq!(
            manager.panel_at_position(31, 0, 32, 12),
            Some(PanelPlacement {
                id: "agent".to_string(),
                x: 0,
                y: 0,
                width: 32,
                height: 4,
            })
        );
        assert!(manager.panel_at_position(0, 4, 32, 12).is_none());

        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(32, 12, &theme.style);
        manager.render(&mut buffer, &theme);

        assert!(row_text(&buffer, 0).contains("Top agent"));
        assert!((1..4).any(|y| row_text(&buffer, y).contains("LATEST")));
        assert_eq!(row_text(&buffer, 4), "─".repeat(32));
    }

    #[test]
    fn bottom_text_panel_renders_at_its_actual_vertical_origin() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Bottom,
                width: 4,
                title: Some("Bottom agent".to_string()),
                ..PanelConfig::default()
            },
        );
        manager.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Plain,
                text: "LATEST bottom response".to_string(),
            }],
            10,
            32,
        );

        assert_eq!(manager.reserved_bottom_height(), 5);
        assert_eq!(
            manager.panel_at_position(31, 6, 32, 12),
            Some(PanelPlacement {
                id: "agent".to_string(),
                x: 0,
                y: 6,
                width: 32,
                height: 4,
            })
        );
        assert!(manager.panel_at_position(0, 5, 32, 12).is_none());

        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(32, 12, &theme.style);
        manager.render(&mut buffer, &theme);

        assert_eq!(row_text(&buffer, 5), "─".repeat(32));
        assert!(row_text(&buffer, 6).contains("Bottom agent"));
        assert!((7..10).any(|y| row_text(&buffer, y).contains("LATEST")));
        assert!(!(0..5).any(|y| row_text(&buffer, y).contains("LATEST")));
    }

    #[test]
    fn four_sided_panels_reserve_disjoint_editor_and_separator_regions() {
        let mut manager = PanelManager::default();
        for (id, side, width) in [
            ("left", PanelSide::Left, 5),
            ("top", PanelSide::Top, 3),
            ("bottom", PanelSide::Bottom, 4),
            ("right", PanelSide::Right, 4),
        ] {
            manager.create_panel(
                id.to_string(),
                PanelConfig {
                    side,
                    width,
                    title: Some(id.to_string()),
                    ..PanelConfig::default()
                },
            );
        }

        assert_eq!(manager.reserved_top_height(), 4);
        assert_eq!(manager.reserved_bottom_height(), 5);
        assert_eq!(manager.reserved_left_width(), 6);
        assert_eq!(manager.reserved_right_width(), 5);

        let placements = manager.panel_placements(30, 18);
        let placement = |id: &str| {
            placements
                .iter()
                .find(|placement| placement.id == id)
                .unwrap()
        };
        assert_eq!(placement("top").x, 0);
        assert_eq!(placement("top").y, 0);
        assert_eq!(placement("top").width, 30);
        assert_eq!(placement("top").height, 3);
        assert_eq!(placement("bottom").x, 0);
        assert_eq!(placement("bottom").y, 12);
        assert_eq!(placement("bottom").width, 30);
        assert_eq!(placement("bottom").height, 4);
        assert_eq!(placement("left").x, 0);
        assert_eq!(placement("left").y, 4);
        assert_eq!(placement("left").width, 5);
        assert_eq!(placement("left").height, 7);
        assert_eq!(placement("right").x, 26);
        assert_eq!(placement("right").y, 4);
        assert_eq!(placement("right").width, 4);
        assert_eq!(placement("right").height, 7);

        assert!(manager.panel_at_position(15, 3, 30, 18).is_none());
        assert!(manager.panel_at_position(15, 11, 30, 18).is_none());
        assert!(manager.panel_at_position(5, 7, 30, 18).is_none());
        assert!(manager.panel_at_position(25, 7, 30, 18).is_none());
        assert_eq!(
            manager.panel_at_position(29, 12, 30, 18).unwrap().id,
            "bottom"
        );

        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(30, 18, &theme.style);
        manager.render(&mut buffer, &theme);
        assert!(row_text(&buffer, 0).contains("top"));
        assert!(row_text(&buffer, 4).contains("left"));
        assert!(row_text(&buffer, 4).contains("righ"));
        assert!(row_text(&buffer, 12).contains("bottom"));
    }

    #[test]
    fn horizontal_composer_mouse_and_cursor_use_the_pane_vertical_origin() {
        for side in [PanelSide::Top, PanelSide::Bottom] {
            let mut manager = PanelManager::default();
            manager.create_text_panel(
                "agent".to_string(),
                PanelConfig {
                    side,
                    width: 7,
                    title: Some("Agent".to_string()),
                    composer: Some(TextPanelComposerConfig {
                        placeholder: "Ask".to_string(),
                        rows: 2,
                    }),
                    ..PanelConfig::default()
                },
            );

            let placement = manager.panel_placements(48, 18).remove(0);
            let composer_top = placement.y + placement.height - 4;
            let event = manager
                .focus_panel_at_position(1, composer_top + 1, 48, 18)
                .unwrap();
            assert_eq!(event.action, "composer_focus");
            manager.handle_focused_text_input(&Event::Paste("hé 👋".to_string()), 48);

            let (cursor_x, cursor_y) = manager.focused_text_panel_cursor_position(48, 18).unwrap();
            assert!(cursor_x < placement.width);
            assert!(cursor_y > composer_top);
            assert!(cursor_y < placement.y + placement.height);

            let theme = Theme::default();
            let mut buffer = RenderBuffer::new(48, 18, &theme.style);
            manager.render(&mut buffer, &theme);
            assert!(row_text(&buffer, placement.y).contains("Agent"));
            assert!(row_text(&buffer, composer_top + 1).contains("hé 👋"));
            assert!(!row_text(&buffer, composer_top + 1).contains('›'));
        }
    }

    #[test]
    fn moving_agent_between_docks_preserves_modal_draft_and_focus() {
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
                ..PanelConfig::default()
            },
        );
        assert!(manager.focus_text_panel_composer("agent"));
        manager.handle_focused_text_input(&Event::Paste("keep my draft 👋".to_string()), 48);

        assert!(manager.update_panel_layout("agent", PanelSide::Top, 7));
        assert_eq!(manager.reserved_right_width(), 0);
        assert_eq!(manager.reserved_top_height(), 8);
        assert_eq!(manager.focused_panel_id(), Some("agent"));
        assert!(manager.focused_text_input_active());
        assert_eq!(
            manager.text_panels["agent"]
                .composer
                .as_ref()
                .unwrap()
                .composer
                .contents(),
            "keep my draft 👋"
        );

        assert!(manager.update_panel_layout("agent", PanelSide::Bottom, 7));
        assert_eq!(manager.reserved_top_height(), 0);
        assert_eq!(manager.reserved_bottom_height(), 8);
        assert!(manager.focused_text_input_active());
        let placement = manager.panel_placements(48, 18).remove(0);
        let (_, cursor_y) = manager.focused_text_panel_cursor_position(48, 18).unwrap();
        assert!(cursor_y >= placement.y);
        assert!(cursor_y < placement.y + placement.height);
        assert_eq!(
            manager.text_panels["agent"]
                .composer
                .as_ref()
                .unwrap()
                .composer
                .contents(),
            "keep my draft 👋"
        );
    }

    #[test]
    fn oversized_four_sided_panels_are_clipped_on_tiny_terminals() {
        let mut manager = PanelManager::default();
        for (id, side) in [
            ("left", PanelSide::Left),
            ("top", PanelSide::Top),
            ("bottom", PanelSide::Bottom),
            ("right", PanelSide::Right),
        ] {
            manager.create_panel(
                id.to_string(),
                PanelConfig {
                    side,
                    width: 99,
                    ..PanelConfig::default()
                },
            );
        }

        let theme = Theme::default();
        for (width, height) in [(0, 0), (1, 1), (1, 2), (2, 3), (8, 5), (20, 8)] {
            let placements = manager.panel_placements(width, height);
            for (index, placement) in placements.iter().enumerate() {
                assert!(placement.x + placement.width <= width);
                assert!(placement.y + placement.height <= height.saturating_sub(2));
                for other in placements.iter().skip(index + 1) {
                    let separated = placement.x + placement.width <= other.x
                        || other.x + other.width <= placement.x
                        || placement.y + placement.height <= other.y
                        || other.y + other.height <= placement.y;
                    assert!(separated, "overlapping panes at {width}x{height}");
                }
            }

            let mut buffer = RenderBuffer::new(width, height, &theme.style);
            manager.render(&mut buffer, &theme);
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
    fn agent_conversation_confines_background_accents_to_its_footer_on_every_dock() {
        for theme_path in ["themes/kanso.json", "themes/github-light.json"] {
            let theme = parse_vscode_theme(theme_path).unwrap();
            let editor_background = theme.style.bg;
            let footer_background = theme.ui_style.muted.bg;

            assert_ne!(
                footer_background, editor_background,
                "{theme_path} should exercise a genuinely contrasting footer",
            );

            for side in [
                PanelSide::Left,
                PanelSide::Right,
                PanelSide::Top,
                PanelSide::Bottom,
            ] {
                for focus_composer in [false, true] {
                    let mut manager = PanelManager::default();
                    manager.create_text_panel(
                        "agent-conversation".to_string(),
                        PanelConfig {
                            side,
                            width: if matches!(side, PanelSide::Top | PanelSide::Bottom) {
                                14
                            } else {
                                42
                            },
                            title: Some("Agent".to_string()),
                            composer: Some(TextPanelComposerConfig {
                                placeholder: "Ask a follow-up…".to_string(),
                                rows: 2,
                            }),
                            surface: Some(ThemeStyleSpec {
                                foreground: vec![
                                    "sideBar.foreground".to_string(),
                                    "editor.foreground".to_string(),
                                ],
                                background: vec!["editor.background".to_string()],
                                ..ThemeStyleSpec::default()
                            }),
                            border: Some(ThemeStyleSpec {
                                foreground: vec![
                                    "sideBar.border".to_string(),
                                    "panel.border".to_string(),
                                ],
                                background: vec!["editor.background".to_string()],
                                ..ThemeStyleSpec::default()
                            }),
                            header_actions: vec![TextPanelHeaderAction {
                                id: "clear".to_string(),
                                label: "Clear".to_string(),
                                compact_label: Some("C".to_string()),
                            }],
                        },
                    );
                    manager.update_text_panel(
                        "agent-conversation",
                        vec![
                            TextPanelBlock {
                                id: "user:1".to_string(),
                                kind: TextPanelBlockKind::User,
                                format: TextPanelBlockFormat::Plain,
                                text: "Explain the current file".to_string(),
                            },
                            TextPanelBlock {
                                id: "agent:1".to_string(),
                                kind: TextPanelBlockKind::Agent,
                                format: TextPanelBlockFormat::Markdown,
                                text: "## Answer\n[Read the docs](https://example.com)\n> note"
                                    .to_string(),
                            },
                        ],
                        30,
                        100,
                    );
                    assert!(manager.set_text_panel_status(
                        "agent-conversation",
                        Some(TextPanelStatus {
                            busy: true,
                            label: "Reading the current file".to_string(),
                            stream: false,
                        }),
                    ));
                    if focus_composer {
                        assert!(manager.focus_text_panel_composer("agent-conversation"));
                    } else {
                        assert!(manager.focus_panel("agent-conversation"));
                    }

                    let mut buffer = RenderBuffer::new(100, 30, &theme.style);
                    manager.render(&mut buffer, &theme);
                    let placement = manager
                        .panel_placements(buffer.width, buffer.height)
                        .into_iter()
                        .find(|placement| placement.id == "agent-conversation")
                        .unwrap();
                    let footer_y = placement.y + placement.height - 1;

                    for y in placement.y..placement.y + placement.height {
                        for x in placement.x..placement.x + placement.width {
                            let expected_background = if y == footer_y {
                                footer_background
                            } else {
                                editor_background
                            };
                            assert_eq!(
                                buffer.cells[y * buffer.width + x].style.bg,
                                expected_background,
                                "{theme_path}, {side:?}, composer_focused={focus_composer}, \
                                 x={x}, y={y}",
                            );
                        }
                    }

                    let title = &buffer.cells[placement.y * buffer.width + placement.x];
                    assert_eq!(
                        title.style.fg,
                        theme.colors.get("sideBar.foreground").copied(),
                    );
                    assert!(title.style.bold);

                    let footer = row_text(&buffer, footer_y);
                    assert!(
                        footer.contains(if focus_composer { "INSERT" } else { "READ" }),
                        "the localized footer should retain Vim-mode guidance: {footer}",
                    );
                }
            }
        }
    }

    #[test]
    fn custom_text_panel_keeps_its_requested_surface_background() {
        let theme = parse_vscode_theme("themes/github-light.json").unwrap();
        let sidebar_background = theme.colors["sideBar.background"];
        assert_ne!(Some(sidebar_background), theme.style.bg);

        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "custom-conversation".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 36,
                title: Some("Custom conversation".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Custom prompt".to_string(),
                    rows: 2,
                }),
                surface: Some(ThemeStyleSpec {
                    foreground: vec!["sideBar.foreground".to_string()],
                    background: vec!["sideBar.background".to_string()],
                    ..ThemeStyleSpec::default()
                }),
                header_actions: vec![TextPanelHeaderAction {
                    id: "clear".to_string(),
                    label: "Clear".to_string(),
                    compact_label: Some("C".to_string()),
                }],
                ..PanelConfig::default()
            },
        );
        manager.update_text_panel(
            "custom-conversation",
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Markdown,
                text: "# Custom heading\n> Custom note".to_string(),
            }],
            20,
            80,
        );
        assert!(manager.focus_text_panel_composer("custom-conversation"));

        let mut buffer = RenderBuffer::new(80, 20, &theme.style);
        manager.render(&mut buffer, &theme);
        let placement = manager
            .panel_placements(buffer.width, buffer.height)
            .into_iter()
            .find(|placement| placement.id == "custom-conversation")
            .unwrap();
        let footer_y = placement.y + placement.height - 1;

        for y in placement.y..placement.y + placement.height {
            for x in placement.x..placement.x + placement.width {
                let expected_background = if y == footer_y {
                    theme.ui_style.muted.bg
                } else {
                    Some(sidebar_background)
                };
                assert_eq!(
                    buffer.cells[y * buffer.width + x].style.bg,
                    expected_background,
                    "custom text surfaces should remain intact at x={x}, y={y}",
                );
            }
        }
    }

    #[test]
    fn text_panel_renders_fenced_code_with_the_editors_real_syntax_styles() {
        let theme = parse_vscode_theme("themes/kanso.json").unwrap();
        let mut highlighter = Highlighter::new(&theme).unwrap();
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Left,
                width: 48,
                title: Some("Agent".to_string()),
                ..PanelConfig::default()
            },
        );
        manager.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "agent:1".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Markdown,
                text: "```rust\nfn main() {}\n```".to_string(),
            }],
            24,
            80,
        );

        let expected_keyword_style = manager.text_panels["agent"]
            .rendered_lines_with_highlighter(48, Some(&mut highlighter))
            .into_iter()
            .flat_map(|line| line.spans)
            .find(|span| span.text == "fn" && span.syntax_style.is_some())
            .and_then(|span| span.syntax_style)
            .expect("fenced Rust keywords should receive tree-sitter syntax styles");

        let mut buffer = RenderBuffer::new(80, 24, &theme.style);
        manager.render_with_highlighter(&mut buffer, &theme, &mut highlighter, false);
        let (row, column) = (0..buffer.height)
            .find_map(|row| {
                let text = row_text(&buffer, row);
                text.find("fn main")
                    .map(|byte| (row, display_width(&text[..byte])))
            })
            .expect("fenced Rust source should be visible in the conversation");

        assert_eq!(
            buffer.cells[row * buffer.width + column].style,
            expected_keyword_style.with_bg(expected_keyword_style.bg.or(theme.style.bg))
        );
    }

    #[test]
    fn ascii_panel_rendering_uses_portable_borders_dividers_and_spinner() {
        let theme = Theme::default();
        let mut highlighter = Highlighter::new(&theme).unwrap();
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "top".to_string(),
            PanelConfig {
                side: PanelSide::Top,
                width: 4,
                title: Some("Top".to_string()),
                ..PanelConfig::default()
            },
        );
        manager.create_text_panel(
            "agent".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 28,
                title: Some("Agent".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask a follow-up".to_string(),
                    rows: 2,
                }),
                ..PanelConfig::default()
            },
        );
        assert!(manager.set_text_panel_status(
            "agent",
            Some(TextPanelStatus {
                busy: true,
                label: "Working".to_string(),
                stream: false,
            }),
        ));

        let mut buffer = RenderBuffer::new(80, 24, &theme.style);
        manager.render_with_highlighter(&mut buffer, &theme, &mut highlighter, true);
        let placements = manager.panel_placements(buffer.width, buffer.height);
        let top = placements
            .iter()
            .find(|placement| placement.id == "top")
            .unwrap();
        let agent = placements
            .iter()
            .find(|placement| placement.id == "agent")
            .unwrap();

        assert_eq!(
            buffer.cells[(top.y + top.height) * buffer.width + top.x].text,
            "-"
        );
        assert_eq!(buffer.cells[agent.y * buffer.width + agent.x - 1].text, "|");
        let divider_y = agent.y + agent.height - manager.text_panels["agent"].composer_height();
        assert!(row_text(&buffer, divider_y).contains(&"-".repeat(agent.width)));
        let status_row = (agent.y..agent.y + agent.height)
            .map(|row| row_text(&buffer, row))
            .find(|row| row.contains("Working"))
            .expect("busy agent status should remain visible in ASCII mode");
        assert!(
            TEXT_PANEL_ASCII_SPINNER_FRAMES
                .iter()
                .any(|frame| status_row.contains(frame)),
            "busy status should use a portable ASCII animation: {status_row}"
        );
        assert!(
            !TEXT_PANEL_SPINNER_FRAMES
                .iter()
                .any(|frame| status_row.contains(frame)),
            "busy status should not contain a Unicode spinner in ASCII mode"
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
            &Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL)),
            80,
        );
        manager.handle_focused_text_input(
            &Event::Key(KeyEvent::new(KeyCode::Char('世'), KeyModifiers::NONE)),
            80,
        );
        assert!(manager
            .handle_focused_text_input(
                &Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
                80,
            )
            .is_none());
        assert_eq!(
            manager.text_panels["agent"]
                .composer
                .as_ref()
                .unwrap()
                .composer
                .contents(),
            "one 👨‍👩‍👧\ntwo\n世",
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
        assert_eq!(recalled.composer.contents(), "one 👨‍👩‍👧\ntwo\n世");
        manager.handle_focused_text_input(
            &Event::Key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            80,
        );
        let restored = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert_eq!(restored.composer.contents(), "draft");
        assert!(manager.focused_text_panel_cursor_position(80, 20).is_some());
    }

    #[test]
    fn focused_composer_reports_real_vim_cursor_mode_for_every_dock_side() {
        for side in [
            PanelSide::Left,
            PanelSide::Right,
            PanelSide::Top,
            PanelSide::Bottom,
        ] {
            let mut manager = PanelManager::default();
            manager.create_text_panel(
                "agent".to_string(),
                PanelConfig {
                    side,
                    width: 16,
                    title: Some("Agent".to_string()),
                    composer: Some(TextPanelComposerConfig {
                        placeholder: "Ask a follow-up…".to_string(),
                        rows: 3,
                    }),
                    ..PanelConfig::default()
                },
            );

            assert_eq!(manager.focused_text_panel_cursor_mode(), None);
            assert!(manager.focus_text_panel_composer("agent"));
            assert_eq!(
                manager.focused_text_panel_cursor_mode(),
                Some(crate::editor::Mode::Insert),
                "a newly focused {side:?} composer must own the Insert cursor",
            );

            manager.handle_focused_text_input(
                &Event::Paste("e\u{301} 👨‍👩‍👧 漢 123456789\nsecond line".to_string()),
                48,
            );

            let placement = manager
                .panel_placements(48, 24)
                .into_iter()
                .find(|placement| placement.id == "agent")
                .unwrap();
            let (cursor_x, cursor_y) = manager.focused_text_panel_cursor_position(48, 24).unwrap();

            assert!(
                (placement.x..placement.x + placement.width).contains(&cursor_x),
                "{side:?} cursor x={cursor_x} must remain inside its dock",
            );
            assert!(
                (placement.y..placement.y + placement.height).contains(&cursor_y),
                "{side:?} cursor y={cursor_y} must remain inside its dock",
            );

            manager.handle_focused_text_input(&composer_key(KeyCode::Esc, KeyModifiers::NONE), 48);
            assert_eq!(
                manager.focused_text_panel_cursor_mode(),
                Some(crate::editor::Mode::Normal),
                "Escape must switch the {side:?} composer to the Normal cursor",
            );

            manager.handle_focused_text_input(
                &composer_key(KeyCode::Char('v'), KeyModifiers::NONE),
                48,
            );
            assert_eq!(
                manager.focused_text_panel_cursor_mode(),
                Some(crate::editor::Mode::Visual),
                "v must switch the {side:?} composer to the Visual cursor",
            );

            manager.handle_focused_text_input(&composer_key(KeyCode::Esc, KeyModifiers::NONE), 48);
            manager.handle_focused_text_input(
                &composer_key(KeyCode::Char('i'), KeyModifiers::NONE),
                48,
            );
            assert_eq!(
                manager.focused_text_panel_cursor_mode(),
                Some(crate::editor::Mode::Insert),
                "i must restore the {side:?} composer's Insert cursor",
            );
        }
    }

    #[test]
    fn empty_conversation_has_visible_normal_cursor_on_every_dock_side() {
        for side in [
            PanelSide::Left,
            PanelSide::Right,
            PanelSide::Top,
            PanelSide::Bottom,
        ] {
            let mut manager = focused_agent_conversation(side);
            let placement = manager
                .panel_placements(64, 24)
                .into_iter()
                .find(|placement| placement.id == "agent")
                .unwrap();

            assert_eq!(
                manager.focused_text_panel_cursor_mode(),
                Some(crate::editor::Mode::Normal),
                "an empty {side:?} conversation must own a Normal cursor",
            );
            assert_eq!(
                manager.focused_text_panel_cursor_position(64, 24),
                Some((placement.x, placement.y + 1)),
                "an empty {side:?} conversation must use its first content row",
            );
            assert!(!manager.focused_text_input_active());

            assert!(manager.set_panel_visible("agent", false));
            assert_eq!(manager.focused_text_panel_cursor_mode(), None);
            assert_eq!(manager.focused_text_panel_cursor_position(64, 24), None);
        }
    }

    #[test]
    fn conversation_mouse_cursor_snaps_to_unicode_markdown_graphemes_on_every_side() {
        for side in [
            PanelSide::Left,
            PanelSide::Right,
            PanelSide::Top,
            PanelSide::Bottom,
        ] {
            let mut manager = focused_agent_conversation(side);
            manager.update_text_panel(
                "agent",
                vec![TextPanelBlock {
                    id: "answer".to_string(),
                    kind: TextPanelBlockKind::Text,
                    format: TextPanelBlockFormat::Markdown,
                    text: "e\u{301} 👨‍👩‍👧 漢 **x**\n\nsecond".to_string(),
                }],
                22,
                64,
            );
            let placement = manager
                .panel_placements(64, 24)
                .into_iter()
                .find(|placement| placement.id == "agent")
                .unwrap();
            let content_y = placement.y + 1;

            let family = manager
                .focus_panel_at_position(placement.x + 3, content_y, 64, 24)
                .unwrap();
            assert_eq!(family.action, "select");
            assert_eq!(
                manager.focused_text_panel_cursor_position(64, 24),
                Some((placement.x + 2, content_y)),
                "a {side:?} mouse click inside a family emoji must snap to its first cell",
            );

            manager.focus_panel_at_position(placement.x + 6, content_y, 64, 24);
            assert_eq!(
                manager.focused_text_panel_cursor_position(64, 24),
                Some((placement.x + 5, content_y)),
                "a {side:?} mouse click inside CJK must snap to its first cell",
            );

            manager.focus_panel_at_position(placement.x + placement.width - 1, content_y, 64, 24);
            assert_eq!(
                manager.focused_text_panel_cursor_position(64, 24),
                Some((placement.x + 8, content_y)),
                "a {side:?} click beyond Markdown text must clamp to its last grapheme",
            );
            assert_eq!(
                manager.focused_text_panel_cursor_mode(),
                Some(crate::editor::Mode::Normal),
            );
        }
    }

    #[test]
    fn transcript_vim_navigation_preserves_unicode_cursor_and_stream_following() {
        let mut manager = focused_agent_conversation(PanelSide::Right);
        let transcript = (0..24)
            .map(|index| format!("line-{index:02} 👨‍👩‍👧"))
            .collect::<Vec<_>>()
            .join("\n");
        manager.update_text_panel(
            "agent",
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Text,
                format: TextPanelBlockFormat::Plain,
                text: transcript,
            }],
            22,
            64,
        );
        let placement = manager
            .panel_placements(64, 24)
            .into_iter()
            .find(|placement| placement.id == "agent")
            .unwrap();

        assert!(manager.text_panels["agent"].follow_tail);
        manager.handle_focused_key("top", 22, 64, 0);
        assert_eq!(
            manager.focused_text_panel_cursor_position(64, 24),
            Some((placement.x, placement.y + 1)),
        );
        assert!(!manager.text_panels["agent"].follow_tail);

        manager.handle_focused_key("down", 22, 64, 0);
        assert_eq!(
            manager.focused_text_panel_cursor_position(64, 24),
            Some((placement.x, placement.y + 2)),
        );
        manager.handle_focused_key("expand", 22, 64, 0);
        manager.handle_focused_key("right", 22, 64, 0);
        assert_eq!(
            manager.focused_text_panel_cursor_position(64, 24),
            Some((placement.x + 2, placement.y + 2)),
        );
        manager.handle_focused_key("collapse", 22, 64, 0);
        assert_eq!(
            manager.focused_text_panel_cursor_position(64, 24),
            Some((placement.x + 1, placement.y + 2)),
        );

        manager.handle_focused_key("page_down", 22, 64, 0);
        assert!(manager.text_panels["agent"].scroll > 0);
        assert!(!manager.text_panels["agent"].follow_tail);

        manager.handle_focused_key("top", 22, 64, 0);
        manager.append_text_panel("agent", "answer", "\nmanual append", 22, 64);
        assert_eq!(manager.text_panels["agent"].scroll, 0);
        assert_eq!(
            manager.focused_text_panel_cursor_position(64, 24),
            Some((placement.x, placement.y + 1)),
        );

        manager.handle_focused_key("bottom", 22, 64, 0);
        let previous_scroll = manager.text_panels["agent"].scroll;
        manager.append_text_panel("agent", "answer", "\nlatest 👋", 22, 64);
        let panel = &manager.text_panels["agent"];
        assert!(panel.follow_tail);
        assert!(panel.scroll > previous_scroll);
        assert_eq!(
            panel.transcript_cursor.row,
            panel.rendered_lines(placement.width).len() - 1,
        );
        let (cursor_x, cursor_y) = manager.focused_text_panel_cursor_position(64, 24).unwrap();
        assert!((placement.x..placement.x + placement.width).contains(&cursor_x));
        assert!((placement.y..placement.y + placement.height).contains(&cursor_y));
    }

    #[test]
    fn composer_hints_prioritize_control_enter_in_every_mode_and_narrow_width() {
        let mut manager = focused_agent_composer();
        let composer = manager.text_panels["agent"].composer.as_ref().unwrap();
        let wide = text_panel_composer_hints(composer, 100);
        assert!(wide.contains("Ctrl+Enter send"));
        assert!(wide.contains("Alt+Enter send"));
        assert!(wide.contains("Enter newline"));

        let narrow = text_panel_composer_hints(composer, 24);
        assert!(narrow.contains("Ctrl+Enter send"));
        assert!(display_width(&narrow) <= 24);
        assert!(!narrow.contains("^S"));

        manager.handle_focused_text_input(&composer_key(KeyCode::Esc, KeyModifiers::NONE), 80);
        let normal =
            text_panel_composer_hints(manager.text_panels["agent"].composer.as_ref().unwrap(), 36);
        assert!(normal.starts_with("NORMAL"));
        assert!(normal.contains("Ctrl+Enter send"));

        manager
            .handle_focused_text_input(&composer_key(KeyCode::Char('v'), KeyModifiers::NONE), 80);
        let visual =
            text_panel_composer_hints(manager.text_panels["agent"].composer.as_ref().unwrap(), 30);
        assert!(visual.starts_with("VISUAL"));
        assert!(visual.contains("Ctrl+Enter send"));
        assert!(!visual.contains("^S"));
    }

    #[test]
    fn focused_text_panel_retains_normal_cursor_when_composer_blurs_or_disables() {
        let mut manager = focused_agent_composer();

        assert_eq!(
            manager.focused_text_panel_cursor_mode(),
            Some(crate::editor::Mode::Insert),
        );

        manager.focus_editor();
        assert_eq!(manager.focused_text_panel_cursor_mode(), None);
        assert_eq!(manager.focused_text_panel_cursor_position(80, 20), None);

        assert!(manager.focus_text_panel_composer("agent"));
        assert!(manager.set_text_panel_composer_state("agent", false, None));
        assert_eq!(
            manager.focused_text_panel_cursor_mode(),
            Some(crate::editor::Mode::Normal),
        );
        assert!(manager.focused_text_panel_cursor_position(80, 20).is_some());

        assert!(manager.set_text_panel_composer_state("agent", true, None));
        assert_eq!(
            manager.focused_text_panel_cursor_mode(),
            Some(crate::editor::Mode::Normal),
        );
        assert!(manager.focused_text_panel_cursor_position(80, 20).is_some());

        assert!(manager.focus_text_panel_composer("agent"));
        assert_eq!(
            manager.focused_text_panel_cursor_mode(),
            Some(crate::editor::Mode::Insert),
        );

        let blurred = manager
            .handle_focused_text_input(&composer_key(KeyCode::Char('c'), KeyModifiers::CONTROL), 80)
            .unwrap();

        assert_eq!(blurred.action, "composer_blur");
        assert_eq!(
            manager.focused_text_panel_cursor_mode(),
            Some(crate::editor::Mode::Normal),
        );
        assert!(manager.focused_text_panel_cursor_position(80, 20).is_some());
    }

    #[test]
    fn text_panel_insert_enter_creates_newline_and_escape_only_enters_normal() {
        let mut manager = focused_agent_composer();

        let entered = manager
            .handle_focused_text_input(&composer_key(KeyCode::Enter, KeyModifiers::NONE), 80)
            .unwrap();
        assert_eq!(entered.action, "composer_input");
        let composer = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert_eq!(composer.composer.contents(), "\n");
        assert_eq!(composer.composer.mode(), ModalComposerMode::Insert);

        let escaped = manager
            .handle_focused_text_input(&composer_key(KeyCode::Esc, KeyModifiers::NONE), 80)
            .unwrap();
        assert_eq!(escaped.action, "composer_input");
        let composer = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert!(composer.focused);
        assert_eq!(composer.composer.mode(), ModalComposerMode::Normal);
        assert_eq!(composer.composer.contents(), "\n");
    }

    #[test]
    fn text_panel_normal_enter_submits_and_restores_insert_mode() {
        let mut manager = focused_agent_composer();
        manager.handle_focused_text_input(&Event::Paste("first\nprompt".to_string()), 80);
        manager.handle_focused_text_input(&composer_key(KeyCode::Esc, KeyModifiers::NONE), 80);

        let submitted = manager
            .handle_focused_text_input(&composer_key(KeyCode::Enter, KeyModifiers::NONE), 80)
            .unwrap();

        assert_eq!(submitted.action, "submit");
        assert_eq!(submitted.text.as_deref(), Some("first\nprompt"));
        let composer = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert!(composer.focused);
        assert_eq!(composer.composer.mode(), ModalComposerMode::Insert);
        assert!(composer.composer.contents().is_empty());
    }

    #[test]
    fn text_panel_normal_mode_uses_real_buffer_operators_undo_and_redo() {
        let mut manager = focused_agent_composer();
        manager.handle_focused_text_input(&Event::Paste("one two three".to_string()), 80);
        for code in [
            KeyCode::Esc,
            KeyCode::Char('0'),
            KeyCode::Char('d'),
            KeyCode::Char('w'),
        ] {
            manager.handle_focused_text_input(&composer_key(code, KeyModifiers::NONE), 80);
        }
        let composer = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert_eq!(composer.composer.contents(), "two three");

        manager
            .handle_focused_text_input(&composer_key(KeyCode::Char('u'), KeyModifiers::NONE), 80);
        let composer = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert_eq!(composer.composer.contents(), "one two three");

        manager.handle_focused_text_input(
            &composer_key(KeyCode::Char('r'), KeyModifiers::CONTROL),
            80,
        );
        let composer = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert_eq!(composer.composer.contents(), "two three");
    }

    #[test]
    fn text_panel_control_c_blurs_without_discarding_the_real_buffer() {
        let mut manager = focused_agent_composer();
        manager.handle_focused_text_input(&Event::Paste("keep 👨‍👩‍👧".to_string()), 80);

        let blurred = manager
            .handle_focused_text_input(&composer_key(KeyCode::Char('c'), KeyModifiers::CONTROL), 80)
            .unwrap();

        assert_eq!(blurred.action, "composer_blur");
        let composer = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert!(!composer.focused);
        assert_eq!(composer.composer.contents(), "keep 👨‍👩‍👧");
    }

    #[test]
    fn text_panel_rejects_oversized_paste_without_losing_existing_draft() {
        let mut manager = focused_agent_composer();
        manager.handle_focused_text_input(&Event::Paste("draft".to_string()), 80);

        let rejected = manager
            .handle_focused_text_input(&Event::Paste("x".repeat(128 * 1024)), 80)
            .unwrap();

        assert_eq!(rejected.action, "composer_input");
        let composer = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert_eq!(composer.composer.contents(), "draft");
        assert_eq!(
            composer.composer.validation_status(),
            Some("Prompt exceeds 128 KiB")
        );
    }

    #[test]
    fn text_panel_composer_renders_plain_unprefixed_input_and_vim_mode() {
        let manager = focused_agent_composer();
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(80, 20, &theme.style);

        manager.render(&mut buffer, &theme);

        let placement = manager
            .panel_placements(80, 20)
            .into_iter()
            .find(|placement| placement.id == "agent")
            .unwrap();
        let panel = &manager.text_panels["agent"];
        let top = placement
            .y
            .saturating_add(placement.height.saturating_sub(panel.composer_height()));
        let input = row_text(&buffer, top + 1);
        let footer = row_text(&buffer, top + panel.composer_height() - 1);

        assert!(input.contains("Ask a follow-up…"));
        assert!(!input.contains('›'));
        assert!(!input.contains("> "));
        assert!(footer.contains("INSERT"));
        assert!(footer.contains("Ctrl+Enter send"));
        assert!(!footer.contains("^S send"));
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
    fn selected_text_panel_links_keep_their_confined_selection_accent() {
        let theme = parse_vscode_theme("themes/github-light.json").unwrap();
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "agent-conversation".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 40,
                title: Some("Agent".to_string()),
                surface: Some(ThemeStyleSpec {
                    foreground: vec!["sideBar.foreground".to_string()],
                    background: vec!["editor.background".to_string()],
                    ..ThemeStyleSpec::default()
                }),
                ..PanelConfig::default()
            },
        );
        manager.update_text_panel(
            "agent-conversation",
            vec![TextPanelBlock {
                id: "answer".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Markdown,
                text: "Read the [docs](https://example.com)".to_string(),
            }],
            20,
            80,
        );
        assert!(manager.focus_panel("agent-conversation"));
        assert!(manager.select_focused_text_link(true, 20, 80));

        let mut buffer = RenderBuffer::new(80, 20, &theme.style);
        manager.render(&mut buffer, &theme);
        let (row, column) = (0..buffer.height)
            .find_map(|row| {
                let text = row_text(&buffer, row);
                text.find("docs")
                    .map(|byte| (row, display_width(&text[..byte])))
            })
            .expect("the focused Markdown link should remain visible");
        let selected = &buffer.cells[row * buffer.width + column];
        let surface_style = panel_style(
            &theme,
            manager.text_panels["agent-conversation"]
                .config
                .surface
                .as_ref(),
        );
        let mut link_style = text_panel_span_style(TextPanelSpanStyle::Link, &theme);
        link_style.fg = link_style.fg.or(surface_style.fg);
        link_style.bg = surface_style.bg;
        let selected_style = theme.selected_style(
            &link_style,
            &theme.list_selection_style(),
            SelectionForegroundPriority::Content,
        );

        assert_ne!(selected.style.bg, theme.style.bg);
        assert_eq!(selected.style, selected_style);
        assert_eq!(
            buffer.cells[row * buffer.width + column.saturating_sub(1)]
                .style
                .bg,
            theme.style.bg,
            "selection must be confined to the linked text",
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

        let event = manager.focus_panel_at_position(52, 15, 80, 20).unwrap();
        assert_eq!(event.action, "composer_focus");
        manager.handle_focused_text_input(&Event::Paste("X".to_string()), 80);

        let composer = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert_eq!(composer.composer.contents(), "first line\nsecXond line");
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
        assert_eq!(composer.composer.contents(), "keep this draft");
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
        render_panel(&mut buffer, &panel, Point::new(0, 0), 10, 3, &theme);

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

        render_panel(&mut buffer, &panel, Point::new(0, 0), 10, 3, &theme);

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
        render_panel(&mut buffer, &panel, Point::new(0, 0), 10, 3, &theme);

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
        render_panel(&mut buffer, &panel, Point::new(0, 0), 10, 3, &theme);

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

        render_panel(&mut buffer, &panel, Point::new(0, 0), 10, 3, &theme);

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
        render_panel(&mut buffer, &panel, Point::new(0, 0), 6, 3, &theme);

        assert_eq!(row_text(&buffer, 0), "abc M ");
    }
}
