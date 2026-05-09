use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{
    editor::{render_buffer::RenderBuffer, Point},
    theme::Style,
    unicode_utils::fit_display_width,
};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelSide {
    #[default]
    Left,
    Right,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
pub struct PanelRow {
    pub id: String,
    pub label: String,
    pub path: Option<String>,
    pub depth: usize,
    pub expanded: Option<bool>,
    pub kind: PanelRowKind,
    pub style: Option<Style>,
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
    }

    pub fn move_selection(&mut self, delta: isize, height: usize) {
        if self.rows.is_empty() {
            return;
        }

        let max_index = self.rows.len() - 1;
        self.selected = self.selected.saturating_add_signed(delta).min(max_index);

        if self.selected < self.scroll {
            self.scroll = self.selected;
        }

        let visible_rows = height.saturating_sub(1).max(1);
        if self.selected >= self.scroll + visible_rows {
            self.scroll = self.selected.saturating_sub(visible_rows - 1);
        }
    }

    pub fn selected_row(&self) -> Option<PanelRow> {
        self.rows.get(self.selected).cloned()
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

    pub fn focus_editor(&mut self) {
        self.focused = None;
    }

    pub fn focused_panel_id(&self) -> Option<&str> {
        self.focused.as_deref()
    }

    pub fn reserved_left_width(&self) -> usize {
        self.panels
            .values()
            .filter(|panel| panel.config.side == PanelSide::Left)
            .map(|panel| panel.config.width.saturating_add(1))
            .sum()
    }

    pub fn handle_focused_key(&mut self, action: &str, height: usize) -> Option<PanelEvent> {
        let focused = self.focused.clone()?;
        let panel = self.panels.get_mut(&focused)?;

        match action {
            "up" => panel.move_selection(-1, height),
            "down" => panel.move_selection(1, height),
            _ => {}
        }

        Some(PanelEvent {
            panel_id: panel.id.clone(),
            action: action.to_string(),
            selected_index: panel.selected,
            row: panel.selected_row(),
        })
    }

    pub fn render(&self, buffer: &mut RenderBuffer, editor_style: &Style) {
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

            render_panel(buffer, panel, Point::new(x, 0), width, editor_style);
        }
    }
}

fn render_panel(
    buffer: &mut RenderBuffer,
    panel: &PluginPanel,
    position: Point,
    width: usize,
    editor_style: &Style,
) {
    if width == 0 || buffer.height <= 2 {
        return;
    }

    let height = buffer.height.saturating_sub(2);
    let selected_style = editor_style.inverted();
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
        let style = if index == panel.selected {
            selected_style.clone()
        } else {
            row.style.clone().unwrap_or_else(|| editor_style.clone())
        };
        let icon = match row.kind {
            PanelRowKind::Directory if row.expanded.unwrap_or(false) => "-",
            PanelRowKind::Directory => "+",
            PanelRowKind::File => " ",
        };
        let indent = "  ".repeat(row.depth);
        let text = format!("{indent}{icon} {}", row.label);
        buffer.set_text(position.x, y, &fit_display_width(&text, width), &style);
    }

    if position.x + width < buffer.width {
        for y in 0..height {
            buffer.set_text(position.x + width, y, "│", editor_style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str) -> PanelRow {
        PanelRow {
            id: id.to_string(),
            label: id.to_string(),
            path: None,
            depth: 0,
            expanded: None,
            kind: PanelRowKind::File,
            style: None,
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
            },
        );

        assert_eq!(manager.reserved_left_width(), 25);
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
}
