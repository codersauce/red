use std::{collections::HashMap, time::Instant};

use crossterm::event::{Event, KeyCode, KeyModifiers};
use serde::{Deserialize, Serialize};

use super::markdown::{
    render_markdown_lines, wrap_plain_text, RenderedTextLine, RenderedTextSpan, TextPanelSpanStyle,
};
use crate::{
    editor::{render_buffer::RenderBuffer, Point},
    theme::{SelectionForegroundPriority, Style, Theme, ThemeStyleSpec},
    ui::{normalize_newlines, wrap_text},
    unicode_utils::{
        display_width, fit_display_width, grapheme_len, grapheme_to_byte, truncate_display_width,
    },
};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelSide {
    #[default]
    Left,
    Right,
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
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            side: PanelSide::Left,
            width: 30,
            title: None,
            composer: None,
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
    let max_width = if config.composer.is_some() {
        terminal_width.saturating_sub(11).max(1)
    } else {
        terminal_width
    };
    config.width.min(max_width)
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
    composer: Option<TextPanelComposer>,
    status: Option<TextPanelStatus>,
    busy_since: Option<Instant>,
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

const MAX_COMPOSER_BYTES: usize = 128 * 1024;

struct TextPanelComposer {
    config: TextPanelComposerConfig,
    draft: String,
    cursor: usize,
    focused: bool,
    enabled: bool,
    status: Option<String>,
    validation: Option<&'static str>,
    history: Vec<String>,
    history_index: Option<usize>,
    saved_draft: Option<String>,
}

impl TextPanelComposer {
    fn new(config: TextPanelComposerConfig) -> Self {
        Self {
            config,
            draft: String::new(),
            cursor: 0,
            focused: false,
            enabled: true,
            status: None,
            validation: None,
            history: Vec::new(),
            history_index: None,
            saved_draft: None,
        }
    }

    fn insert(&mut self, text: &str) {
        let text = normalize_newlines(text);
        if text.len() > MAX_COMPOSER_BYTES.saturating_sub(self.draft.len()) {
            self.validation = Some("Prompt exceeds 128 KiB");
            return;
        }
        let offset = grapheme_to_byte(&self.draft, self.cursor);
        self.draft.insert_str(offset, &text);
        self.cursor = grapheme_len(&self.draft[..offset + text.len()]);
        self.validation = None;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = grapheme_to_byte(&self.draft, self.cursor - 1);
        let end = grapheme_to_byte(&self.draft, self.cursor);
        self.draft.replace_range(start..end, "");
        self.cursor -= 1;
        self.validation = None;
    }

    fn delete(&mut self) {
        if self.cursor >= grapheme_len(&self.draft) {
            return;
        }
        let start = grapheme_to_byte(&self.draft, self.cursor);
        let end = grapheme_to_byte(&self.draft, self.cursor + 1);
        self.draft.replace_range(start..end, "");
        self.validation = None;
    }

    fn take_submission(&mut self) -> Option<String> {
        if self.draft.trim().is_empty() {
            self.validation = Some("Prompt is empty");
            return None;
        }
        self.cursor = 0;
        self.validation = None;
        let text = std::mem::take(&mut self.draft);
        self.history.retain(|entry| entry != &text);
        self.history.insert(0, text.clone());
        self.history.truncate(50);
        self.history_index = None;
        self.saved_draft = None;
        Some(text)
    }

    fn history_previous(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let index = self.history_index.map_or(0, |index| {
            index.saturating_add(1).min(self.history.len() - 1)
        });
        if self.history_index.is_none() {
            self.saved_draft = Some(self.draft.clone());
        }
        self.history_index = Some(index);
        self.draft = self.history[index].clone();
        self.cursor = grapheme_len(&self.draft);
    }

    fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index == 0 {
            self.history_index = None;
            self.draft = self.saved_draft.take().unwrap_or_default();
        } else {
            self.history_index = Some(index - 1);
            self.draft = self.history[index - 1].clone();
        }
        self.cursor = grapheme_len(&self.draft);
    }

    fn move_vertical(&mut self, delta: isize, width: usize) {
        let wrapped = wrap_text(&self.draft, width.max(1));
        let (row, column) = wrapped
            .positions
            .get(self.cursor)
            .copied()
            .unwrap_or_default();
        let target = row.saturating_add_signed(delta);
        if let Some((index, _)) = wrapped
            .positions
            .iter()
            .enumerate()
            .filter(|(_, position)| position.0 == target)
            .min_by_key(|(_, position)| position.1.abs_diff(column))
        {
            self.cursor = index;
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
            composer,
            status: None,
            busy_since: None,
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
    }

    fn page_scroll(&mut self, delta: isize, panel_height: usize, panel_width: usize) {
        let page = self.visible_rows(panel_height).max(1) as isize;
        self.move_scroll(delta.saturating_mul(page), panel_height, panel_width);
    }

    fn scroll_to_top(&mut self) {
        self.scroll = 0;
        self.follow_tail = false;
    }

    fn scroll_to_bottom(&mut self, panel_height: usize, panel_width: usize) {
        self.scroll = self.max_scroll(panel_height, panel_width);
        self.follow_tail = true;
    }

    fn clamp_scroll(&mut self, panel_height: usize, panel_width: usize) {
        self.scroll = self.scroll.min(self.max_scroll(panel_height, panel_width));
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

    fn rendered_lines(&self, width: usize) -> Vec<RenderedTextLine> {
        let mut lines: Vec<RenderedTextLine> = Vec::new();
        for block in &self.blocks {
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
                });
            }
        }
        lines
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
            panel.append_delta(block_id, delta, panel_height, width);
        }
    }

    pub fn update_panel(&mut self, id: &str, rows: Vec<PanelRow>) {
        if let Some(panel) = self.panels.get_mut(id) {
            panel.update_rows(rows);
        }
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

    pub fn handle_focused_key(
        &mut self,
        action: &str,
        panel_height: usize,
        terminal_width: usize,
    ) -> Option<PanelEvent> {
        let focused = self.focused.clone()?;
        if let Some(panel) = self.text_panels.get_mut(&focused) {
            let width = effective_panel_width(&panel.config, terminal_width);
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
        composer.draft.clear();
        composer.cursor = 0;
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

        let mut action = "composer_input";
        let mut text = None;
        match event {
            Event::Paste(pasted) => composer.insert(pasted),
            Event::Key(key) => match (key.code, key.modifiers) {
                (KeyCode::Esc, _) => {
                    composer.focused = false;
                    action = "composer_blur";
                }
                (KeyCode::Enter, modifiers) if modifiers.contains(KeyModifiers::SHIFT) => {
                    composer.insert("\n");
                }
                (KeyCode::Char('j' | 'J'), modifiers)
                    if modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    composer.insert("\n");
                }
                (KeyCode::Char('p' | 'P'), modifiers)
                    if modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    composer.history_previous();
                }
                (KeyCode::Char('n' | 'N'), modifiers)
                    if modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    composer.history_next();
                }
                (KeyCode::Enter, _) => {
                    text = composer.take_submission();
                    action = "submit";
                }
                (KeyCode::Backspace, _) => composer.backspace(),
                (KeyCode::Delete, _) => composer.delete(),
                (KeyCode::Left, _) => composer.cursor = composer.cursor.saturating_sub(1),
                (KeyCode::Right, _) => {
                    composer.cursor = (composer.cursor + 1).min(grapheme_len(&composer.draft));
                }
                (KeyCode::Up, _) => {
                    composer.move_vertical(-1, panel_width.saturating_sub(2));
                }
                (KeyCode::Down, _) => {
                    composer.move_vertical(1, panel_width.saturating_sub(2));
                }
                (KeyCode::Home, _) => composer.cursor = 0,
                (KeyCode::End, _) => composer.cursor = grapheme_len(&composer.draft),
                (KeyCode::Tab, _) => composer.insert("\t"),
                (KeyCode::Char(character), modifiers)
                    if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    composer.insert(&character.to_string());
                }
                _ => return None,
            },
            _ => return None,
        }

        Some(PanelEvent {
            panel_id: panel.id.clone(),
            action: action.to_string(),
            selected_index: panel.scroll,
            row: None,
            text,
        })
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
        let wrapped = wrap_text(&composer.draft, content_width);
        let (row, column) = wrapped
            .positions
            .get(composer.cursor)
            .copied()
            .unwrap_or_default();
        let rows = composer.config.rows.max(1);
        let first = row.saturating_sub(rows.saturating_sub(1));
        let top = placement.height.saturating_sub(panel.composer_height());
        Some((
            placement.x.saturating_add(2).saturating_add(column),
            top.saturating_add(1)
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
                    let wrapped = wrap_text(&composer.draft, content_width);
                    let cursor_row = wrapped
                        .positions
                        .get(composer.cursor)
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
                        composer.cursor = index;
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

    fn panel_placements(
        &self,
        terminal_width: usize,
        terminal_height: usize,
    ) -> Vec<PanelPlacement> {
        let mut placements = Vec::new();
        let mut left_x: usize = 0;
        let mut right_x = terminal_width;
        let height = terminal_height.saturating_sub(2);

        for id in &self.z_order {
            let Some(config) = self.panel_config(id) else {
                continue;
            };

            let width = effective_panel_width(config, terminal_width);
            let x = match config.side {
                PanelSide::Left => {
                    let x = left_x;
                    left_x = left_x.saturating_add(width.saturating_add(1));
                    x
                }
                PanelSide::Right => {
                    right_x = right_x.saturating_sub(width);
                    let x = right_x;
                    right_x = right_x.saturating_sub(1);
                    x
                }
            };

            placements.push(PanelPlacement {
                id: id.clone(),
                x,
                y: 0,
                width,
                height,
            });
        }

        placements
    }

    pub fn render(&self, buffer: &mut RenderBuffer, theme: &Theme) {
        let mut left_x: usize = 0;
        let mut right_x = buffer.width;

        for id in &self.z_order {
            let Some(config) = self.panel_config(id) else {
                continue;
            };

            let width = effective_panel_width(config, buffer.width);
            let (x, separator_x) = match config.side {
                PanelSide::Left => {
                    let x = left_x;
                    left_x = left_x.saturating_add(width.saturating_add(1));
                    (x, x.checked_add(width))
                }
                PanelSide::Right => {
                    right_x = right_x.saturating_sub(width);
                    let x = right_x;
                    right_x = right_x.saturating_sub(1);
                    (x, x.checked_sub(1))
                }
            };

            if let Some(separator_x) = separator_x.filter(|x| *x < buffer.width) {
                for y in 0..buffer.height.saturating_sub(2) {
                    buffer.set_text(separator_x, y, " ", &theme.style);
                }
            }

            if let Some(panel) = self.panels.get(id) {
                render_panel(buffer, panel, Point::new(x, 0), width, theme);
            } else if let Some(panel) = self.text_panels.get(id) {
                render_text_panel(buffer, panel, Point::new(x, 0), width, theme);
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
    theme: &Theme,
) {
    if width == 0 || buffer.height <= 2 {
        return;
    }

    let height = buffer.height.saturating_sub(2);
    let editor_style = &theme.style;
    let selection_style = theme.list_selection_style();
    let selected_style = theme.selected_style(
        editor_style,
        &selection_style,
        SelectionForegroundPriority::Selection,
    );
    let title_style = Style {
        bold: true,
        ..editor_style.clone()
    };

    for y in 0..height {
        buffer.set_text(position.x, y, &" ".repeat(width), editor_style);
    }

    if let Some(title) = &panel.config.title {
        buffer.set_text(
            position.x,
            0,
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
        let y = rows_start + screen_row;
        let index = panel.scroll + screen_row;
        let selected = index == panel.selected;
        if selected {
            buffer.set_text(position.x, y, &" ".repeat(width), &selected_style);
        }

        render_row_segments(buffer, position.x, y, width, row, theme, selected);
    }
}

fn render_text_panel(
    buffer: &mut RenderBuffer,
    panel: &TextPanel,
    position: Point,
    width: usize,
    theme: &Theme,
) {
    if width == 0 || buffer.height <= 2 {
        return;
    }

    let height = buffer.height.saturating_sub(2);
    for y in 0..height {
        buffer.set_text(position.x, y, &" ".repeat(width), &theme.style);
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
            0,
            &fit_display_width(title, title_width),
            &title_style,
        );
    }
    for (start, _, label) in header_actions {
        let x = position.x + start;
        buffer.set_text(x, 0, "[", &theme.ui_style.muted);
        buffer.set_text(x + 1, 0, label, &theme.ui_style.picker_prompt);
        buffer.set_text(x + 1 + display_width(label), 0, "]", &theme.ui_style.muted);
    }

    let composer_height = panel.composer_height();
    let status_height = panel.status_height();
    let content_height = height
        .saturating_sub(composer_height)
        .saturating_sub(status_height);
    let visible_rows = content_height.saturating_sub(title_rows);
    let lines = panel.rendered_lines(width);
    let max_scroll = lines.len().saturating_sub(visible_rows);
    let scroll = if panel.follow_tail {
        max_scroll
    } else {
        panel.scroll.min(max_scroll)
    };
    for (offset, line) in lines.iter().skip(scroll).take(visible_rows).enumerate() {
        render_text_spans(buffer, position.x, title_rows + offset, width, line, theme);
    }

    if let Some(status) = &panel.status {
        render_text_panel_status(buffer, panel, status, position, width, content_height, theme);
    }

    if let Some(composer) = &panel.composer {
        render_text_panel_composer(
            buffer,
            composer,
            position,
            width,
            content_height + status_height,
            theme,
        );
    }

    render_panel_separator(
        buffer,
        position,
        width,
        height,
        &panel.config.side,
        &theme.style,
    );
}

fn render_text_panel_composer(
    buffer: &mut RenderBuffer,
    composer: &TextPanelComposer,
    position: Point,
    width: usize,
    top: usize,
    theme: &Theme,
) {
    if width == 0 {
        return;
    }
    let divider = "─".repeat(width);
    buffer.set_text(
        position.x,
        top,
        &fit_display_width(&divider, width),
        &theme.ui_style.muted,
    );

    let rows = composer.config.rows.max(1);
    let content_width = width.saturating_sub(2).max(1);
    let wrapped = wrap_text(&composer.draft, content_width);
    let cursor_row = wrapped
        .positions
        .get(composer.cursor)
        .map_or(0, |position| position.0);
    let first = cursor_row.saturating_sub(rows.saturating_sub(1));
    for row in 0..rows {
        let y = top + 1 + row;
        let line = wrapped
            .rows
            .get(first + row)
            .map(String::as_str)
            .unwrap_or("");
        let text = if line.is_empty() && composer.draft.is_empty() && row == 0 {
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
    let hints = if composer.focused {
        "Esc nav · Enter send · ^J newline · ^P/^N history"
    } else {
        "a edit · x clear · N new · q close · ^C stop"
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
    buffer.set_text(position.x, y, &fit_display_width(&text, width), style);
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
    theme: &Theme,
) {
    let mut used = 0;
    for span in &line.spans {
        if used >= width {
            break;
        }
        let text = truncate_display_width(&span.text, width - used);
        if text.is_empty() {
            continue;
        }
        let style = text_panel_span_style(span.style, theme);
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
) {
    let separator_x = match side {
        PanelSide::Left => position.x.checked_add(width),
        PanelSide::Right => position.x.checked_sub(1),
    };
    let Some(separator_x) = separator_x.filter(|x| *x < buffer.width) else {
        return;
    };
    for y in 0..height {
        buffer.set_text(separator_x, y, "│", style);
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
    }];
    spans.extend(line.spans);
    RenderedTextLine { spans }
}

fn render_row_segments(
    buffer: &mut RenderBuffer,
    x: usize,
    y: usize,
    width: usize,
    row: &PanelRow,
    theme: &Theme,
    selected: bool,
) {
    let right_width = segments_width(&row.right_segments).min(width);
    let gap = usize::from(right_width > 0 && right_width < width);
    let left_width = width.saturating_sub(right_width).saturating_sub(gap);

    render_segments(buffer, x, y, left_width, &row.segments, theme, selected);

    if right_width > 0 {
        let right_x = x + width.saturating_sub(right_width);
        render_segments(
            buffer,
            right_x,
            y,
            right_width,
            &row.right_segments,
            theme,
            selected,
        );
    }
}

fn render_segments(
    buffer: &mut RenderBuffer,
    x: usize,
    y: usize,
    max_width: usize,
    segments: &[PanelSegment],
    theme: &Theme,
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

        let style = segment_style(segment, theme, selected);
        buffer.set_text(x + used, y, &text, &style);
        used += display_width(&text);
    }
}

fn segment_style(segment: &PanelSegment, theme: &Theme, selected: bool) -> Style {
    let mut style = theme.style.clone();
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
    fn left_panels_reserve_width_with_separator() {
        let mut manager = PanelManager::default();
        manager.create_panel(
            "tree".to_string(),
            PanelConfig {
                side: PanelSide::Left,
                width: 24,
                title: None,
                composer: None,
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
                header_actions: Vec::new(),
            },
        );

        assert_eq!(manager.reserved_right_width(), 25);
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
        let submitted = manager
            .handle_focused_text_input(
                &Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
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
        assert_eq!(recalled.draft, "one 👨‍👩‍👧\ntwo\n世");
        manager.handle_focused_text_input(
            &Event::Key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            80,
        );
        let restored = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert_eq!(restored.draft, "draft");
        assert!(manager.focused_text_panel_cursor_position(80, 20).is_some());
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
                header_actions: Vec::new(),
            },
        );
        assert!(manager.focus_text_panel_composer("agent"));
        manager.handle_focused_text_input(&Event::Paste("first line\nsecond line".to_string()), 80);

        let event = manager.focus_panel_at_position(53, 15, 80, 20).unwrap();
        assert_eq!(event.action, "composer_focus");
        manager.handle_focused_text_input(&Event::Paste("X".to_string()), 80);

        let composer = manager.text_panels["agent"].composer.as_ref().unwrap();
        assert_eq!(composer.draft, "first line\nsecXond line");
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
        assert_eq!(composer.draft, "keep this draft");
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

        let rendered = (0..22).map(|row| row_text(&buffer, row)).collect::<Vec<_>>();
        let joined = rendered.join("\n");
        assert!(joined.contains("▎ You"));
        assert!(joined.contains("✓ Read demo.txt"));
        assert!(!joined.contains("❯ You"));
        let separator_rows = rendered
            .iter()
            .filter(|row| row.contains("────"))
            .count();
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

        let top = manager.handle_focused_key("top", 4, 16).unwrap();
        assert_eq!(top.selected_index, 0);
        assert!(top.row.is_none());
        manager.append_text_panel("agent", "answer", "\neight", 4, 16);
        assert_eq!(manager.text_panels["agent"].scroll, 0);
        assert!(!manager.text_panels["agent"].follow_tail);

        let page = manager.handle_focused_key("page_down", 4, 16).unwrap();
        assert!(page.selected_index > 0);
        let bottom = manager.handle_focused_key("bottom", 4, 16).unwrap();
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

        let event = manager.handle_focused_key("down", 10, 80).unwrap();
        assert_eq!(event.selected_index, 1);
        assert_eq!(event.row.unwrap().id, "b");
    }

    #[test]
    fn focused_panel_scrolls_when_selection_moves_below_viewport() {
        let mut manager = PanelManager::default();
        manager.create_panel("tree".to_string(), PanelConfig::default());
        manager.update_panel("tree", vec![row("a"), row("b"), row("c"), row("d")]);
        assert!(manager.focus_panel("tree"));

        manager.handle_focused_key("down", 3, 80).unwrap();
        manager.handle_focused_key("down", 3, 80).unwrap();
        let event = manager.handle_focused_key("down", 3, 80).unwrap();

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

        assert_eq!(row_text(&buffer, 0), "src      M");
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

        assert_eq!(row_text(&buffer, 0), "abcd M");
    }
}
