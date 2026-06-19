use std::collections::HashMap;

use serde::{Deserialize, Serialize};

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
    z_order: Vec<String>,
    focused: Option<String>,
}

impl PanelManager {
    pub fn create_panel(&mut self, id: String, config: PanelConfig) {
        self.panels
            .insert(id.clone(), PluginPanel::new(id.clone(), config));
        if !self.z_order.contains(&id) {
            self.z_order.push(id.clone());
        }
    }

    pub fn update_panel(&mut self, id: &str, rows: Vec<PanelRow>) {
        if let Some(panel) = self.panels.get_mut(id) {
            panel.update_rows(rows);
        }
    }

    pub fn close_panel(&mut self, id: &str) {
        self.panels.remove(id);
        self.z_order.retain(|panel_id| panel_id != id);
        if self.focused.as_deref() == Some(id) {
            self.focused = None;
        }
    }

    pub fn focus_panel(&mut self, id: &str) -> bool {
        if self.panels.contains_key(id) {
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

    pub fn selected_index(&self, id: &str) -> Option<usize> {
        self.panels.get(id).map(|panel| panel.selected)
    }

    pub fn reserved_left_width(&self) -> usize {
        self.panels
            .values()
            .filter(|panel| panel.config.side == PanelSide::Left)
            .map(|panel| panel.config.width.saturating_add(1))
            .sum()
    }

    pub fn reserved_right_width(&self) -> usize {
        self.panels
            .values()
            .filter(|panel| panel.config.side == PanelSide::Right)
            .map(|panel| panel.config.width.saturating_add(1))
            .sum()
    }

    pub fn handle_focused_key(&mut self, action: &str, panel_height: usize) -> Option<PanelEvent> {
        let focused = self.focused.clone()?;
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
        let panel = self.panels.get_mut(&placement.id)?;

        self.focused = Some(placement.id.clone());
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
            let Some(panel) = self.panels.get(id) else {
                continue;
            };

            let width = panel.config.width.min(terminal_width);
            let x = match panel.config.side {
                PanelSide::Left => {
                    let x = left_x;
                    left_x = left_x.saturating_add(width.saturating_add(1));
                    x
                }
                PanelSide::Right => {
                    right_x = right_x.saturating_sub(width);
                    right_x
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
            let Some(panel) = self.panels.get(id) else {
                continue;
            };

            let width = panel.config.width.min(buffer.width);
            let x = match panel.config.side {
                PanelSide::Left => {
                    let x = left_x;
                    left_x = left_x.saturating_add(width.saturating_add(1));
                    x
                }
                PanelSide::Right => {
                    right_x = right_x.saturating_sub(width);
                    right_x
                }
            };

            render_panel(buffer, panel, Point::new(x, 0), width, theme);
        }
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

    if position.x + width < buffer.width {
        for y in 0..height {
            buffer.set_text(position.x + width, y, "│", editor_style);
        }
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
