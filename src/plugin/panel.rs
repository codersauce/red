use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::markdown::{
    render_markdown_lines, wrap_plain_text, RenderedTextLine, TextPanelSpanStyle,
};
use crate::{
    editor::{render_buffer::RenderBuffer, Point},
    theme::{SelectionForegroundPriority, Style, Theme, ThemeStyleSpec},
    unicode_utils::{display_width, fit_display_width, truncate_display_width},
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
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            side: PanelSide::Left,
            width: 30,
            title: None,
        }
    }
}

fn default_panel_width() -> usize {
    30
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
}

/// Semantic role for a source-backed text-panel block.
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
}

impl TextPanel {
    fn new(id: String, config: PanelConfig) -> Self {
        Self {
            id,
            config,
            blocks: Vec::new(),
            scroll: 0,
            follow_tail: true,
        }
    }

    fn update_blocks(&mut self, blocks: Vec<TextPanelBlock>, panel_height: usize) {
        self.blocks = blocks;
        if self.follow_tail {
            self.scroll_to_bottom(panel_height);
        } else {
            self.clamp_scroll(panel_height);
        }
    }

    fn append_delta(&mut self, block_id: &str, delta: &str, panel_height: usize) {
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
            .max(1)
    }

    fn rendered_lines(&self, width: usize) -> Vec<RenderedTextLine> {
        let mut lines = Vec::new();
        for block in &self.blocks {
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
        })
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
        if let Some(panel) = self.text_panels.get(&placement.id) {
            return Some(PanelEvent {
                panel_id: panel.id.clone(),
                action: "select".to_string(),
                selected_index: panel.scroll,
                row: None,
            });
        }

        let panel = self.panels.get_mut(&placement.id)?;
        panel.select_screen_row(y.saturating_sub(placement.y));

        Some(PanelEvent {
            panel_id: panel.id.clone(),
            action: "select".to_string(),
            selected_index: panel.selected,
            row: panel.selected_row(),
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

    let title_rows = usize::from(panel.config.title.is_some());
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

    let visible_rows = height.saturating_sub(title_rows);
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

    render_panel_separator(
        buffer,
        position,
        width,
        height,
        &panel.config.side,
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
        TextPanelBlockKind::User => Some(("❯ You", TextPanelSpanStyle::User)),
        TextPanelBlockKind::Agent => Some(("◆ Agent", TextPanelSpanStyle::Agent)),
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
            },
        );
        manager.create_panel(
            "right".to_string(),
            PanelConfig {
                side: PanelSide::Right,
                width: 4,
                title: None,
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
        );
        let tail = panel.scroll;

        panel.append_delta("answer", "\nfour", 2);
        assert!(panel.scroll > tail);
        assert!(panel.follow_tail);

        panel.scroll_to_top();
        panel.append_delta("answer", "\nfive", 2);
        assert_eq!(panel.scroll, 0);
        assert!(!panel.follow_tail);
    }

    #[test]
    fn text_panel_append_creates_missing_agent_block_as_markdown() {
        let mut panel = TextPanel::new("agent".to_string(), PanelConfig::default());

        panel.append_delta("answer", "# Heading", 10);

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
        );
        assert!(manager.focus_panel("agent"));
        assert_eq!(manager.reserved_right_width(), 17);

        let top = manager.handle_focused_key("top", 4).unwrap();
        assert_eq!(top.selected_index, 0);
        assert!(top.row.is_none());
        manager.append_text_panel("agent", "answer", "\neight", 4);
        assert_eq!(manager.text_panels["agent"].scroll, 0);
        assert!(!manager.text_panels["agent"].follow_tail);

        let page = manager.handle_focused_key("page_down", 4).unwrap();
        assert!(page.selected_index > 0);
        let bottom = manager.handle_focused_key("bottom", 4).unwrap();
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
        );
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(16, 7, &theme.style);

        manager.render(&mut buffer, &theme);

        assert!(row_text(&buffer, 0).contains("│❯ You"));
        assert!(row_text(&buffer, 1).contains("│hello"));
        assert!(manager.panel_at_position(7, 0, 16, 7).is_none());
        assert!(manager.panel_at_position(8, 0, 16, 7).is_some());
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
