use serde::{Deserialize, Serialize};

use crate::{
    editor::render_buffer::RenderBuffer,
    theme::Style,
    unicode_utils::{display_width, fit_display_width, truncate_display_width},
};

use super::PanelSegment;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceConfig {
    #[serde(default)]
    pub title: String,
    #[serde(default = "default_detail_ratio")]
    pub detail_ratio: u8,
    #[serde(default = "default_min_two_pane_width")]
    pub min_two_pane_width: usize,
}

fn default_detail_ratio() -> u8 {
    55
}

fn default_min_two_pane_width() -> usize {
    100
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            title: String::new(),
            detail_ratio: default_detail_ratio(),
            min_two_pane_width: default_min_two_pane_width(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceModel {
    #[serde(default)]
    pub header: Vec<PanelSegment>,
    #[serde(default)]
    pub rows: Vec<WorkspaceRow>,
    #[serde(default)]
    pub detail: Vec<Vec<PanelSegment>>,
    #[serde(default)]
    pub footer: Vec<PanelSegment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRow {
    pub id: String,
    #[serde(default)]
    pub selectable: bool,
    #[serde(default)]
    pub depth: usize,
    #[serde(default)]
    pub segments: Vec<PanelSegment>,
    #[serde(default)]
    pub right_segments: Vec<PanelSegment>,
    #[serde(default)]
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceEvent {
    pub workspace_id: String,
    pub action: String,
    pub selected_index: usize,
    pub row: Option<WorkspaceRow>,
}

#[derive(Debug)]
pub struct PluginWorkspace {
    id: String,
    config: WorkspaceConfig,
    model: WorkspaceModel,
    selected: usize,
    scroll: usize,
}

impl PluginWorkspace {
    fn new(id: String, config: WorkspaceConfig) -> Self {
        Self {
            id,
            config,
            model: WorkspaceModel::default(),
            selected: 0,
            scroll: 0,
        }
    }

    fn update(&mut self, model: WorkspaceModel) {
        let selected_id = self
            .model
            .rows
            .get(self.selected)
            .map(|row| row.id.as_str());
        let selected = selected_id
            .and_then(|id| model.rows.iter().position(|row| row.id == id))
            .or_else(|| model.rows.iter().position(|row| row.selectable))
            .unwrap_or(0);
        self.model = model;
        self.selected = selected;
        self.scroll = self.scroll.min(self.selected);
    }

    fn move_selection(&mut self, delta: isize, visible_rows: usize) {
        if self.model.rows.is_empty() {
            return;
        }
        let mut next = self.selected;
        loop {
            let candidate = next
                .saturating_add_signed(delta)
                .min(self.model.rows.len() - 1);
            if candidate == next {
                break;
            }
            next = candidate;
            if self.model.rows[next].selectable {
                self.selected = next;
                break;
            }
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + visible_rows.max(1) {
            self.scroll = self.selected.saturating_sub(visible_rows.saturating_sub(1));
        }
    }

    fn event(&self, action: String) -> WorkspaceEvent {
        WorkspaceEvent {
            workspace_id: self.id.clone(),
            action,
            selected_index: self.selected,
            row: self.model.rows.get(self.selected).cloned(),
        }
    }
}

#[derive(Debug, Default)]
pub struct WorkspaceManager {
    active: Option<PluginWorkspace>,
}

impl WorkspaceManager {
    pub fn open(&mut self, id: String, config: WorkspaceConfig) {
        self.active = Some(PluginWorkspace::new(id, config));
    }

    pub fn update(&mut self, id: &str, model: WorkspaceModel) -> bool {
        let Some(workspace) = self.active.as_mut().filter(|workspace| workspace.id == id) else {
            return false;
        };
        workspace.update(model);
        true
    }

    pub fn close(&mut self, id: &str) -> bool {
        if self
            .active
            .as_ref()
            .is_some_and(|workspace| workspace.id == id)
        {
            self.active = None;
            true
        } else {
            false
        }
    }

    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    pub fn handle_action(&mut self, action: String, height: usize) -> Option<WorkspaceEvent> {
        let workspace = self.active.as_mut()?;
        let visible_rows = height.saturating_sub(4);
        match action.as_str() {
            "up" => workspace.move_selection(-1, visible_rows),
            "down" => workspace.move_selection(1, visible_rows),
            "page_up" => workspace.move_selection(-(visible_rows as isize), visible_rows),
            "page_down" => workspace.move_selection(visible_rows as isize, visible_rows),
            _ => {}
        }
        Some(workspace.event(action))
    }

    pub fn render(&self, buffer: &mut RenderBuffer, editor_style: &Style) {
        let Some(workspace) = &self.active else {
            return;
        };
        for y in 0..buffer.height {
            buffer.set_text(0, y, &" ".repeat(buffer.width), editor_style);
        }
        if buffer.width < 4 || buffer.height < 4 {
            return;
        }

        let title = format!(" {} ", workspace.config.title);
        buffer.set_text(
            1,
            0,
            &truncate_display_width(&title, buffer.width - 2),
            editor_style,
        );
        render_segments(
            buffer,
            1,
            1,
            buffer.width - 2,
            &workspace.model.header,
            editor_style,
            false,
        );

        let body_top = 2;
        let body_height = buffer.height.saturating_sub(3);
        let two_pane = buffer.width >= workspace.config.min_two_pane_width;
        let left_width = if two_pane {
            buffer
                .width
                .saturating_mul(100usize.saturating_sub(workspace.config.detail_ratio as usize))
                / 100
        } else {
            buffer.width
        };
        let left_width = left_width.max(20).min(buffer.width);

        for (screen_row, row) in workspace
            .model
            .rows
            .iter()
            .skip(workspace.scroll)
            .take(body_height)
            .enumerate()
        {
            let y = body_top + screen_row;
            let selected = workspace.scroll + screen_row == workspace.selected && row.selectable;
            let x = 1 + row.depth.saturating_mul(2);
            render_segments(
                buffer,
                x,
                y,
                left_width.saturating_sub(x + 1),
                &row.segments,
                editor_style,
                selected,
            );
            let right_width = row
                .right_segments
                .iter()
                .map(|segment| display_width(&segment.text))
                .sum::<usize>();
            if right_width > 0 && right_width + 1 < left_width {
                render_segments(
                    buffer,
                    left_width.saturating_sub(right_width + 1),
                    y,
                    right_width,
                    &row.right_segments,
                    editor_style,
                    selected,
                );
            }
            if selected {
                let marker_style = row
                    .segments
                    .first()
                    .and_then(|segment| segment.style.as_ref())
                    .unwrap_or(editor_style);
                buffer.set_text(0, y, "›", marker_style);
            }
        }

        if two_pane && left_width < buffer.width {
            for y in body_top..buffer.height.saturating_sub(1) {
                buffer.set_text(left_width, y, "│", editor_style);
            }
            let detail_x = left_width + 2;
            for (index, line) in workspace.model.detail.iter().take(body_height).enumerate() {
                render_segments(
                    buffer,
                    detail_x,
                    body_top + index,
                    buffer.width.saturating_sub(detail_x + 1),
                    line,
                    editor_style,
                    false,
                );
            }
        }

        render_segments(
            buffer,
            1,
            buffer.height - 1,
            buffer.width - 2,
            &workspace.model.footer,
            editor_style,
            false,
        );
    }
}

fn render_segments(
    buffer: &mut RenderBuffer,
    mut x: usize,
    y: usize,
    width: usize,
    segments: &[PanelSegment],
    editor_style: &Style,
    selected: bool,
) {
    let end = x.saturating_add(width).min(buffer.width);
    for segment in segments {
        if x >= end {
            break;
        }
        let text = truncate_display_width(&segment.text, end - x);
        let mut style = segment
            .style
            .clone()
            .unwrap_or_else(|| editor_style.clone())
            .fallback_bg(editor_style);
        if selected {
            style.bold = true;
        }
        buffer.set_text(x, y, &text, &style);
        x += display_width(&text);
    }
    if selected && x < end {
        buffer.set_text(x, y, &fit_display_width("", end - x), editor_style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, selectable: bool) -> WorkspaceRow {
        WorkspaceRow {
            id: id.to_string(),
            selectable,
            depth: 0,
            segments: vec![],
            right_segments: vec![],
            data: serde_json::Value::Null,
        }
    }

    #[test]
    fn selection_skips_non_selectable_rows_and_survives_update() {
        let mut manager = WorkspaceManager::default();
        manager.open("git".to_string(), WorkspaceConfig::default());
        manager.update(
            "git",
            WorkspaceModel {
                rows: vec![row("heading", false), row("a", true), row("b", true)],
                ..WorkspaceModel::default()
            },
        );
        let event = manager.handle_action("down".to_string(), 20).unwrap();
        assert_eq!(event.row.unwrap().id, "b");
        manager.update(
            "git",
            WorkspaceModel {
                rows: vec![row("b", true), row("c", true)],
                ..WorkspaceModel::default()
            },
        );
        let event = manager.handle_action("noop".to_string(), 20).unwrap();
        assert_eq!(event.row.unwrap().id, "b");
    }
}
