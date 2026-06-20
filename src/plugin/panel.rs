use std::collections::HashMap;

use crossterm::event::{Event, KeyCode, KeyModifiers};
use pulldown_cmark::{CodeBlockKind, Event as MarkdownEvent, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};

use crate::{
    editor::{render_buffer::RenderBuffer, Point},
    theme::{SelectionForegroundPriority, Style, Theme, ThemeStyleSpec},
    unicode_utils::{display_width, fit_display_width, truncate_display_width},
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
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
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            side: PanelSide::Left,
            width: 30,
            title: None,
            composer: None,
        }
    }
}

fn default_panel_width() -> usize {
    30
}

fn default_composer_rows() -> usize {
    3
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

/// Semantic role for one source-backed text-panel block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextPanelBlockKind {
    User,
    Agent,
    Error,
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

pub struct TextPanel {
    pub id: String,
    pub config: PanelConfig,
    pub blocks: Vec<TextPanelBlock>,
    pub scroll: usize,
    pub follow_tail: bool,
    composer: Option<TextPanelComposer>,
}

struct TextPanelComposer {
    config: TextPanelComposerConfig,
    draft: String,
    cursor: usize,
    focused: bool,
    enabled: bool,
    status: Option<String>,
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
        }
    }

    fn insert_text(&mut self, text: &str) {
        let byte = char_to_byte(&self.draft, self.cursor);
        self.draft.insert_str(byte, text);
        self.cursor += text.chars().count();
    }

    fn delete_previous_char(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = char_to_byte(&self.draft, self.cursor - 1);
        let end = char_to_byte(&self.draft, self.cursor);
        self.draft.replace_range(start..end, "");
        self.cursor -= 1;
    }

    fn delete_next_char(&mut self) {
        if self.cursor >= self.draft.chars().count() {
            return;
        }
        let start = char_to_byte(&self.draft, self.cursor);
        let end = char_to_byte(&self.draft, self.cursor + 1);
        self.draft.replace_range(start..end, "");
    }

    fn take_submission(&mut self) -> Option<String> {
        let text = self.draft.trim().to_string();
        if text.is_empty() {
            return None;
        }
        self.draft.clear();
        self.cursor = 0;
        self.focused = false;
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
            composer,
        }
    }

    fn update_blocks(&mut self, blocks: Vec<TextPanelBlock>, panel_height: usize) {
        self.blocks = blocks;
        self.clamp_scroll(panel_height);
        if self.follow_tail {
            self.scroll_to_bottom(panel_height);
        }
    }

    fn append_delta(&mut self, block_id: &str, delta: &str, panel_height: usize) {
        if let Some(block) = self.blocks.iter_mut().find(|block| block.id == block_id) {
            block.text.push_str(delta);
        } else {
            self.blocks.push(TextPanelBlock {
                id: block_id.to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Plain,
                text: delta.to_string(),
            });
        }
        if self.follow_tail {
            self.scroll_to_bottom(panel_height);
        } else {
            self.clamp_scroll(panel_height);
        }
    }

    fn move_scroll(&mut self, delta: isize, panel_height: usize) {
        let max_scroll = self.max_scroll(panel_height);
        self.scroll = self.scroll.saturating_add_signed(delta).min(max_scroll);
        self.follow_tail = self.scroll == max_scroll;
    }

    fn page_scroll(&mut self, delta: isize, panel_height: usize) {
        let page = self.visible_rows(panel_height).max(1) as isize;
        self.move_scroll(delta.saturating_mul(page), panel_height);
    }

    fn scroll_to_top(&mut self) {
        self.scroll = 0;
        self.follow_tail = false;
    }

    fn scroll_to_bottom(&mut self, panel_height: usize) {
        self.scroll = self.max_scroll(panel_height);
        self.follow_tail = true;
    }

    fn clamp_scroll(&mut self, panel_height: usize) {
        self.scroll = self.scroll.min(self.max_scroll(panel_height));
    }

    fn max_scroll(&self, panel_height: usize) -> usize {
        self.rendered_lines(self.config.width)
            .len()
            .saturating_sub(self.visible_rows(panel_height))
    }

    fn visible_rows(&self, panel_height: usize) -> usize {
        panel_height
            .saturating_sub(usize::from(self.config.title.is_some()))
            .saturating_sub(self.composer_height())
            .max(1)
    }

    fn composer_height(&self) -> usize {
        self.composer
            .as_ref()
            .map_or(0, |composer| composer.config.rows.max(1).saturating_add(2))
    }

    fn rendered_lines(&self, width: usize) -> Vec<RenderedTextLine> {
        let mut lines = Vec::new();
        for block in &self.blocks {
            if let Some((label, style)) = block_label(&block.kind) {
                lines.push(RenderedTextLine::plain(label.to_string(), style));
            }
            let mut block_lines = match block.format {
                TextPanelBlockFormat::Plain => {
                    let style = block_style(&block.kind);
                    wrap_text(&block.text, width)
                        .into_iter()
                        .map(|line| RenderedTextLine::plain(line, style))
                        .collect()
                }
                TextPanelBlockFormat::Markdown => render_markdown_lines(&block.text, width),
            };
            if block_lines.is_empty() {
                block_lines.push(RenderedTextLine::plain(
                    String::new(),
                    block_style(&block.kind),
                ));
            }
            lines.extend(block_lines);
            lines.push(RenderedTextLine::plain(
                String::new(),
                TextPanelSpanStyle::Text,
            ));
        }
        if lines.last().is_some_and(RenderedTextLine::is_empty) {
            lines.pop();
        }
        lines
    }

    fn copy_last_agent(&self) -> Option<String> {
        self.blocks
            .iter()
            .rev()
            .find(|block| block.kind == TextPanelBlockKind::Agent)
            .map(|block| block.text.clone())
    }

    fn copy_all(&self) -> String {
        self.blocks
            .iter()
            .map(|block| match block.kind {
                TextPanelBlockKind::User => format!("User:\n{}", block.text),
                TextPanelBlockKind::Agent => format!("Codex:\n{}", block.text),
                TextPanelBlockKind::Error => format!("Error:\n{}", block.text),
                TextPanelBlockKind::Text => block.text.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TextPanelSpanStyle {
    User,
    Agent,
    Error,
    Text,
    Heading,
    Strong,
    Emphasis,
    InlineCode,
    Link,
    Quote,
    Code,
    Muted,
}

struct RenderedTextLine {
    spans: Vec<RenderedTextSpan>,
}

struct RenderedTextSpan {
    text: String,
    style: TextPanelSpanStyle,
}

impl RenderedTextLine {
    fn plain(text: String, style: TextPanelSpanStyle) -> Self {
        Self {
            spans: vec![RenderedTextSpan { text, style }],
        }
    }

    fn is_empty(&self) -> bool {
        self.spans.iter().all(|span| span.text.is_empty())
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
    ) {
        if let Some(panel) = self.text_panels.get_mut(id) {
            panel.update_blocks(blocks, panel_height);
        }
    }

    pub fn append_text_panel(
        &mut self,
        id: &str,
        block_id: &str,
        delta: &str,
        panel_height: usize,
    ) {
        if let Some(panel) = self.text_panels.get_mut(id) {
            panel.append_delta(block_id, delta, panel_height);
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

    pub fn focus_panel(&mut self, id: &str) -> bool {
        if self.panels.contains_key(id) || self.text_panels.contains_key(id) {
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
        self.focused = None;
    }

    pub fn focused_panel_id(&self) -> Option<&str> {
        self.focused.as_deref()
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

    pub fn handle_focused_key(&mut self, action: &str, panel_height: usize) -> Option<PanelEvent> {
        let focused = self.focused.clone()?;
        if let Some(panel) = self.text_panels.get_mut(&focused) {
            match action {
                "composer_focus" => {
                    if let Some(composer) = panel.composer.as_mut() {
                        if composer.enabled {
                            composer.focused = true;
                        }
                    }
                }
                "up" => panel.move_scroll(-1, panel_height),
                "down" => panel.move_scroll(1, panel_height),
                "page_up" => panel.page_scroll(-1, panel_height),
                "page_down" => panel.page_scroll(1, panel_height),
                "top" => panel.scroll_to_top(),
                "bottom" => panel.scroll_to_bottom(panel_height),
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
        let Some(panel) = self.text_panels.get_mut(id) else {
            return false;
        };
        let Some(composer) = panel.composer.as_mut() else {
            return false;
        };
        if !composer.enabled {
            return false;
        }
        self.focused = Some(id.to_string());
        composer.focused = true;
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
        true
    }

    pub fn handle_focused_text_input(&mut self, event: &Event) -> Option<PanelEvent> {
        let focused = self.focused.clone()?;
        let panel = self.text_panels.get_mut(&focused)?;
        let composer = panel.composer.as_mut()?;
        if !composer.focused || !composer.enabled {
            return None;
        }

        let mut action = "composer_input";
        let mut text = None;
        match event {
            Event::Paste(pasted) => composer.insert_text(pasted),
            Event::Key(key) => match key.code {
                KeyCode::Esc => {
                    composer.focused = false;
                    action = "composer_blur";
                }
                KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    text = composer.take_submission();
                    action = "submit";
                }
                KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    text = composer.take_submission();
                    action = "submit";
                }
                KeyCode::Enter => composer.insert_text("\n"),
                KeyCode::Backspace => composer.delete_previous_char(),
                KeyCode::Delete => composer.delete_next_char(),
                KeyCode::Left => composer.cursor = composer.cursor.saturating_sub(1),
                KeyCode::Right => {
                    composer.cursor = (composer.cursor + 1).min(composer.draft.chars().count())
                }
                KeyCode::Home => composer.cursor = 0,
                KeyCode::End => composer.cursor = composer.draft.chars().count(),
                KeyCode::Char(c)
                    if key.modifiers == KeyModifiers::NONE
                        || key.modifiers == KeyModifiers::SHIFT =>
                {
                    composer.insert_text(&c.to_string());
                }
                _ => return None,
            },
            _ => return None,
        }

        if action == "submit" && text.is_none() {
            return None;
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
        let width = placement.width.saturating_sub(2).max(1);
        let (lines, cursor_line, cursor_column) =
            wrapped_text_with_cursor(&composer.draft, composer.cursor, width);
        let visible_rows = composer.config.rows.max(1);
        let first_line = lines.len().saturating_sub(visible_rows);
        let row = cursor_line.saturating_sub(first_line).min(visible_rows - 1);
        let composer_top = placement
            .height
            .saturating_sub(composer.config.rows.max(1).saturating_add(1));
        Some((
            placement.x + 2 + cursor_column.min(width),
            composer_top + row,
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
        if let Some(panel) = self.text_panels.get_mut(&placement.id) {
            self.focused = Some(placement.id.clone());
            let composer_top = placement.height.saturating_sub(panel.composer_height());
            let action = if y >= composer_top
                && panel
                    .composer
                    .as_ref()
                    .is_some_and(|composer| composer.enabled)
            {
                if let Some(composer) = panel.composer.as_mut() {
                    composer.focused = true;
                    composer.cursor = composer.draft.chars().count();
                }
                "composer_focus"
            } else {
                if let Some(composer) = panel.composer.as_mut() {
                    composer.focused = false;
                }
                "select"
            };
            return Some(PanelEvent {
                panel_id: placement.id,
                action: action.to_string(),
                selected_index: 0,
                row: None,
                text: None,
            });
        }
        let panel = self.panels.get_mut(&placement.id)?;

        self.focused = Some(placement.id.clone());
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

            let width = config.width.min(terminal_width);
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

            let width = config.width.min(buffer.width);
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

    render_panel_separator(
        buffer,
        position,
        width,
        height,
        panel.config.side,
        editor_style,
    );
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

    let title_rows = usize::from(panel.config.title.is_some());
    let composer_height = panel.composer_height();
    let content_height = height.saturating_sub(composer_height);
    if let Some(title) = &panel.config.title {
        let title_style = Style {
            bold: true,
            ..theme.style.clone()
        };
        buffer.set_text(
            position.x,
            0,
            &fit_display_width(title, width),
            &title_style,
        );
    }

    let lines = panel.rendered_lines(width);
    for (offset, line) in lines
        .iter()
        .skip(panel.scroll)
        .take(content_height.saturating_sub(title_rows))
        .enumerate()
    {
        render_text_spans(buffer, position.x, title_rows + offset, width, line, theme);
    }

    if let Some(composer) = &panel.composer {
        render_text_panel_composer(buffer, composer, position, width, content_height, theme);
    }

    render_panel_separator(
        buffer,
        position,
        width,
        height,
        panel.config.side,
        &theme.style,
    );
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
        TextPanelSpanStyle::Strong => {
            let mut style = theme.style.clone();
            style.bold = true;
            style
        }
        TextPanelSpanStyle::Emphasis => {
            let mut style = theme.style.clone();
            style.italic = true;
            style
        }
        TextPanelSpanStyle::InlineCode | TextPanelSpanStyle::Code => {
            scoped("markup.raw.block.markdown")
        }
        TextPanelSpanStyle::Link => scoped("markup.underline.link.markdown"),
        TextPanelSpanStyle::Quote | TextPanelSpanStyle::Muted => theme.ui_style.muted.clone(),
    }
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
    let divider = format!(
        "{} {}",
        "─".repeat(width.saturating_sub(16)),
        if composer.enabled { "a edit" } else { "busy" }
    );
    buffer.set_text(
        position.x,
        top,
        &fit_display_width(&divider, width),
        &theme.ui_style.muted,
    );

    let rows = composer.config.rows.max(1);
    let content_width = width.saturating_sub(2).max(1);
    let (lines, _, _) = wrapped_text_with_cursor(&composer.draft, composer.cursor, content_width);
    let first_line = lines.len().saturating_sub(rows);
    for row in 0..rows {
        let y = top + 1 + row;
        let line = lines
            .get(first_line + row)
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
    let status = composer.status.as_deref().unwrap_or({
        if composer.focused {
            "Ctrl-s send · Esc leave"
        } else {
            "a edit · x clear"
        }
    });
    buffer.set_text(
        position.x,
        top + rows + 1,
        &fit_display_width(status, width),
        &theme.ui_style.muted,
    );
}

fn render_panel_separator(
    buffer: &mut RenderBuffer,
    position: Point,
    width: usize,
    height: usize,
    side: PanelSide,
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
        TextPanelBlockKind::User => Some(("❯ You", TextPanelSpanStyle::User)),
        TextPanelBlockKind::Agent => Some(("◆ Codex", TextPanelSpanStyle::Agent)),
        TextPanelBlockKind::Error => Some(("⚠ Error", TextPanelSpanStyle::Error)),
        TextPanelBlockKind::Text => None,
    }
}

fn block_style(kind: &TextPanelBlockKind) -> TextPanelSpanStyle {
    match kind {
        TextPanelBlockKind::User => TextPanelSpanStyle::User,
        TextPanelBlockKind::Agent => TextPanelSpanStyle::Agent,
        TextPanelBlockKind::Error => TextPanelSpanStyle::Error,
        TextPanelBlockKind::Text => TextPanelSpanStyle::Text,
    }
}

fn render_markdown_lines(text: &str, width: usize) -> Vec<RenderedTextLine> {
    let mut builder = MarkdownLineBuilder::new(width);
    let mut styles = vec![TextPanelSpanStyle::Agent];
    let mut list_numbers = Vec::<Option<u64>>::new();
    let mut in_code_block = false;

    for event in Parser::new(text) {
        match event {
            MarkdownEvent::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { .. } => {
                    builder.ensure_block_break();
                    builder.append("◆ ", TextPanelSpanStyle::Heading);
                    styles.push(TextPanelSpanStyle::Heading);
                }
                Tag::BlockQuote(_) => {
                    builder.ensure_block_break();
                    builder.append("│ ", TextPanelSpanStyle::Quote);
                    styles.push(TextPanelSpanStyle::Quote);
                }
                Tag::CodeBlock(kind) => {
                    builder.ensure_block_break();
                    let language = match kind {
                        CodeBlockKind::Fenced(language) => language.to_string(),
                        CodeBlockKind::Indented => String::new(),
                    };
                    let header = if language.is_empty() {
                        "┌─ code".to_string()
                    } else {
                        format!("┌─ {language}")
                    };
                    builder.append(&header, TextPanelSpanStyle::Muted);
                    builder.newline();
                    in_code_block = true;
                }
                Tag::List(start) => list_numbers.push(start),
                Tag::Item => {
                    let depth = list_numbers.len().saturating_sub(1);
                    builder.append(&"  ".repeat(depth), TextPanelSpanStyle::Text);
                    let marker = match list_numbers.last_mut() {
                        Some(Some(number)) => {
                            let marker = format!("{number}. ");
                            *number += 1;
                            marker
                        }
                        _ => "• ".to_string(),
                    };
                    builder.append(&marker, TextPanelSpanStyle::User);
                }
                Tag::Emphasis => styles.push(TextPanelSpanStyle::Emphasis),
                Tag::Strong => styles.push(TextPanelSpanStyle::Strong),
                Tag::Link { .. } => styles.push(TextPanelSpanStyle::Link),
                _ => {}
            },
            MarkdownEvent::End(tag) => match tag {
                TagEnd::Paragraph | TagEnd::Heading(_) => builder.blank_line(),
                TagEnd::BlockQuote(_) => {
                    styles.pop();
                    builder.blank_line();
                }
                TagEnd::CodeBlock => {
                    builder.newline();
                    builder.append("└─", TextPanelSpanStyle::Muted);
                    builder.blank_line();
                    in_code_block = false;
                }
                TagEnd::List(_) => {
                    list_numbers.pop();
                    builder.blank_line();
                }
                TagEnd::Item => builder.newline(),
                TagEnd::Emphasis | TagEnd::Strong => {
                    styles.pop();
                }
                TagEnd::Link => {
                    styles.pop();
                    builder.append(" ↗", TextPanelSpanStyle::Link);
                }
                _ => {}
            },
            MarkdownEvent::Text(value) => {
                let style = if in_code_block {
                    TextPanelSpanStyle::Code
                } else {
                    *styles.last().unwrap_or(&TextPanelSpanStyle::Agent)
                };
                builder.append(&value, style);
            }
            MarkdownEvent::Code(value) => builder.append(&value, TextPanelSpanStyle::InlineCode),
            MarkdownEvent::SoftBreak => builder.append(" ", TextPanelSpanStyle::Agent),
            MarkdownEvent::HardBreak => builder.newline(),
            MarkdownEvent::Rule => {
                builder.ensure_block_break();
                builder.append(&"─".repeat(width.max(1)), TextPanelSpanStyle::Muted);
                builder.blank_line();
            }
            MarkdownEvent::Html(value) | MarkdownEvent::InlineHtml(value) => {
                builder.append(&value, TextPanelSpanStyle::Agent);
            }
            _ => {}
        }
    }
    builder.finish()
}

struct MarkdownLineBuilder {
    width: usize,
    lines: Vec<RenderedTextLine>,
    current: RenderedTextLine,
}

impl MarkdownLineBuilder {
    fn new(width: usize) -> Self {
        Self {
            width: width.max(1),
            lines: Vec::new(),
            current: RenderedTextLine { spans: Vec::new() },
        }
    }

    fn append(&mut self, text: &str, style: TextPanelSpanStyle) {
        for character in text.chars() {
            if character == '\n' {
                self.newline();
                continue;
            }
            let text = character.to_string();
            if self.current_width() > 0
                && self.current_width().saturating_add(display_width(&text)) > self.width
            {
                self.newline();
            }
            if let Some(last) = self.current.spans.last_mut() {
                if last.style == style {
                    last.text.push(character);
                    continue;
                }
            }
            self.current.spans.push(RenderedTextSpan { text, style });
        }
    }

    fn current_width(&self) -> usize {
        self.current
            .spans
            .iter()
            .map(|span| display_width(&span.text))
            .sum()
    }

    fn newline(&mut self) {
        self.lines.push(std::mem::replace(
            &mut self.current,
            RenderedTextLine { spans: Vec::new() },
        ));
    }

    fn blank_line(&mut self) {
        if !self.current.is_empty() {
            self.newline();
        }
        if !self.lines.last().is_some_and(RenderedTextLine::is_empty) {
            self.lines.push(RenderedTextLine { spans: Vec::new() });
        }
    }

    fn ensure_block_break(&mut self) {
        if !self.current.is_empty() {
            self.newline();
        }
    }

    fn finish(mut self) -> Vec<RenderedTextLine> {
        if !self.current.is_empty() {
            self.newline();
        }
        while self.lines.last().is_some_and(RenderedTextLine::is_empty) {
            self.lines.pop();
        }
        self.lines
    }
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for logical_line in text.split('\n') {
        if logical_line.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut remaining = logical_line;
        while !remaining.is_empty() {
            let visible = truncate_display_width(remaining, width.max(1));
            if visible.is_empty() {
                break;
            }
            let used = visible.len();
            lines.push(visible);
            remaining = &remaining[used..];
        }
    }
    lines
}

fn wrapped_text_with_cursor(
    text: &str,
    cursor: usize,
    width: usize,
) -> (Vec<String>, usize, usize) {
    let width = width.max(1);
    let cursor_byte = char_to_byte(text, cursor);
    let before = &text[..cursor_byte];
    let lines = wrap_text(text, width);
    let before_lines = wrap_text(before, width);
    let cursor_line = before_lines.len().saturating_sub(1);
    let cursor_column = before_lines
        .last()
        .map_or(0, |line| display_width(line))
        .min(width);
    (
        if lines.is_empty() {
            vec![String::new()]
        } else {
            lines
        },
        cursor_line,
        cursor_column,
    )
}

fn char_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(index, _)| index)
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
    use crossterm::event::KeyEvent;

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
            },
        );

        assert_eq!(manager.reserved_right_width(), 25);
    }

    #[test]
    fn right_text_panels_render_separator_on_their_left_edge() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "assistant".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 4,
                title: Some("AI".to_string()),
                composer: None,
            },
        );
        let style = Style::default();
        let theme = Theme {
            style: style.clone(),
            ..Theme::default()
        };
        let mut buffer = RenderBuffer::new(10, 5, &style);

        manager.render(&mut buffer, &theme);

        assert_eq!(buffer.cells[5].text, "│");
    }

    #[test]
    fn adjacent_right_panels_leave_separator_columns_between_them() {
        let mut manager = PanelManager::default();
        manager.create_panel(
            "outer".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 4,
                title: None,
                composer: None,
            },
        );
        manager.create_panel(
            "inner".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 3,
                title: None,
                composer: None,
            },
        );

        let placements = manager.panel_placements(20, 10);

        assert_eq!(placements[0].x, 16);
        assert_eq!(placements[1].x, 12);
    }

    #[test]
    fn text_panels_scroll_and_copy_source_blocks() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "assistant".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 12,
                title: Some("Ask AI".to_string()),
                composer: None,
            },
        );
        manager.update_text_panel(
            "assistant",
            vec![
                TextPanelBlock {
                    id: "user".to_string(),
                    kind: TextPanelBlockKind::User,
                    format: TextPanelBlockFormat::Plain,
                    text: "why?".to_string(),
                },
                TextPanelBlock {
                    id: "agent".to_string(),
                    kind: TextPanelBlockKind::Agent,
                    format: TextPanelBlockFormat::Plain,
                    text: "one\ntwo\nthree\nfour".to_string(),
                },
            ],
            3,
        );
        assert!(manager.focus_panel("assistant"));

        assert_eq!(
            manager.focused_text_for_copy(false).as_deref(),
            Some("one\ntwo\nthree\nfour")
        );
        assert_eq!(
            manager.focused_text_for_copy(true).as_deref(),
            Some("User:\nwhy?\n\nCodex:\none\ntwo\nthree\nfour")
        );

        manager.handle_focused_key("top", 3).unwrap();
        assert_eq!(manager.text_panels["assistant"].scroll, 0);
        manager.handle_focused_key("page_down", 3).unwrap();
        assert!(manager.text_panels["assistant"].scroll > 0);
    }

    #[test]
    fn text_panel_append_follows_tail_until_user_scrolls() {
        let mut panel = TextPanel::new(
            "assistant".to_string(),
            PanelConfig {
                width: 8,
                title: None,
                ..PanelConfig::default()
            },
        );
        panel.update_blocks(
            vec![TextPanelBlock {
                id: "agent".to_string(),
                kind: TextPanelBlockKind::Agent,
                format: TextPanelBlockFormat::Plain,
                text: "one\ntwo\nthree".to_string(),
            }],
            2,
        );
        let tail = panel.scroll;
        panel.append_delta("agent", "\nfour", 2);
        assert!(panel.scroll > tail);

        panel.scroll_to_top();
        panel.append_delta("agent", "\nfive", 2);
        assert_eq!(panel.scroll, 0);
        assert!(!panel.follow_tail);
    }

    #[test]
    fn text_panel_composer_accepts_multiline_input_and_submits() {
        let mut manager = PanelManager::default();
        manager.create_text_panel(
            "assistant".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 20,
                title: Some("Ask AI".to_string()),
                composer: Some(TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 3,
                }),
            },
        );
        assert!(manager.focus_text_panel_composer("assistant"));

        manager
            .handle_focused_text_input(&Event::Key(KeyEvent::new(
                KeyCode::Char('a'),
                KeyModifiers::NONE,
            )))
            .unwrap();
        manager
            .handle_focused_text_input(&Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .unwrap();
        manager
            .handle_focused_text_input(&Event::Key(KeyEvent::new(
                KeyCode::Char('b'),
                KeyModifiers::NONE,
            )))
            .unwrap();
        let event = manager
            .handle_focused_text_input(&Event::Key(KeyEvent::new(
                KeyCode::Char('s'),
                KeyModifiers::CONTROL,
            )))
            .unwrap();

        assert_eq!(event.action, "submit");
        assert_eq!(event.text.as_deref(), Some("a\nb"));
    }

    #[test]
    fn markdown_text_panels_render_semantic_markers() {
        let lines = render_markdown_lines(
            "# Heading\n\n- first\n- second\n\n**bold** and `code`\n",
            30,
        );
        let text = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.text.as_str())
            .collect::<String>();

        assert!(text.contains("◆ Heading"));
        assert!(text.contains("• first"));
        assert!(text.contains("bold and code"));
        assert!(!text.contains("**"));
        assert!(!text.contains('`'));
    }

    #[test]
    fn focused_panel_moves_selection() {
        let mut manager = PanelManager::default();
        manager.create_panel("tree".to_string(), PanelConfig::default());
        manager.update_panel("tree", vec![row("a"), row("b")]);
        assert!(manager.focus_panel("tree"));

        let event = manager.handle_focused_key("down", 10).unwrap();
        assert_eq!(event.selected_index, 1);
        assert_eq!(event.row.unwrap().id, "b");
    }

    #[test]
    fn focused_panel_scrolls_when_selection_moves_below_viewport() {
        let mut manager = PanelManager::default();
        manager.create_panel("tree".to_string(), PanelConfig::default());
        manager.update_panel("tree", vec![row("a"), row("b"), row("c"), row("d")]);
        assert!(manager.focus_panel("tree"));

        manager.handle_focused_key("down", 3).unwrap();
        manager.handle_focused_key("down", 3).unwrap();
        let event = manager.handle_focused_key("down", 3).unwrap();

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
