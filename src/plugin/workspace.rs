//! Full-screen plugin workspace models and selection state.
//!
//! A [`WorkspaceModel`] is the plugin-owned snapshot of rows, sections, actions, and
//! detail content. [`WorkspaceManager`] owns focus and the currently selected row while
//! replacing models by stable workspace ID. Selection restoration is ID-based so
//! reordering rows does not silently move focus to unrelated content.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    config::PickerIconsConfig,
    editor::render_buffer::RenderBuffer,
    highlighter::{Highlighter, LanguageRegistry},
    theme::{DiffPalette, SelectionForegroundPriority, Style, SurfacePalette, Theme},
    ui::{ActionBar, ActionMenu, ActionPriority, IconCatalog, ScreenRect, UiAction},
    unicode_utils::{display_width, fit_display_width, truncate_display_width},
};

use super::PanelSegment;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct WorkspaceConfig {
    #[serde(default)]
    pub title: String,
    #[serde(default = "default_detail_ratio")]
    pub detail_ratio: u8,
    #[serde(default = "default_min_two_pane_width")]
    pub min_two_pane_width: usize,
    /// Minimum terminal height that can show rows above detail in a stacked layout.
    #[serde(default = "default_min_stacked_height")]
    pub min_stacked_height: usize,
    /// Whether structured detail documents wrap long lines initially.
    #[serde(default = "default_detail_wrap")]
    pub detail_wrap: bool,
    /// Whether navigation handled entirely by the detail pane is also sent to
    /// the owning plugin. Plugins only need this when they implement custom
    /// navigation behavior; line operations are always delivered.
    #[serde(default = "default_notify_detail_navigation")]
    pub notify_detail_navigation: bool,
}

fn default_detail_ratio() -> u8 {
    55
}

fn default_min_two_pane_width() -> usize {
    100
}

fn default_min_stacked_height() -> usize {
    16
}

fn default_detail_wrap() -> bool {
    true
}

fn default_notify_detail_navigation() -> bool {
    true
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            title: String::new(),
            detail_ratio: default_detail_ratio(),
            min_two_pane_width: default_min_two_pane_width(),
            min_stacked_height: default_min_stacked_height(),
            detail_wrap: default_detail_wrap(),
            notify_detail_navigation: default_notify_detail_navigation(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct WorkspaceModel {
    #[serde(default)]
    pub header: Vec<PanelSegment>,
    #[serde(default)]
    pub rows: Vec<WorkspaceRow>,
    #[serde(default)]
    pub detail: Vec<Vec<PanelSegment>>,
    /// Optional focusable document. Legacy `detail` lines remain supported for
    /// workspaces that only need a passive preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_document: Option<WorkspaceDocument>,
    #[serde(default)]
    pub footer: Vec<PanelSegment>,
    /// Structured actions supersede the legacy footer when present.
    #[serde(default)]
    pub actions: Vec<WorkspaceAction>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub detail_title: String,
}

/// Plugin actions are filtered locally, so cursor movement never requires a plugin round trip.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct WorkspaceAction {
    pub hint: UiAction,
    #[serde(default)]
    pub focus: String,
    #[serde(default)]
    pub sections: Vec<String>,
    #[serde(default)]
    pub selection: String,
    #[serde(default)]
    pub change_only: bool,
    #[serde(default)]
    pub hunk_only: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct WorkspaceDocument {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub lines: Vec<WorkspaceDocumentLine>,
    #[serde(default)]
    pub added: usize,
    #[serde(default)]
    pub removed: usize,
    #[serde(default)]
    pub total_lines: usize,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct WorkspaceDocumentLine {
    pub id: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub old_line: Option<usize>,
    #[serde(default)]
    pub new_line: Option<usize>,
    #[serde(default)]
    pub hunk_id: Option<String>,
    #[serde(default)]
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceFocus {
    #[default]
    Rows,
    Detail,
}

type WorkspaceRect = ScreenRect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceSeparator {
    Columns(WorkspaceRect),
    Stacked(WorkspaceRect),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceLayoutMode {
    Columns,
    Stacked,
    Focused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WorkspaceLayout {
    mode: WorkspaceLayoutMode,
    rows: Option<WorkspaceRect>,
    detail: Option<WorkspaceRect>,
    separator: Option<WorkspaceSeparator>,
}

impl WorkspaceLayout {
    fn calculate(workspace: &PluginWorkspace, height: usize, width: usize) -> Self {
        let body = WorkspaceRect {
            x: 0,
            y: 2,
            width,
            height: height.saturating_sub(3),
        };
        if let Some(focus) = workspace.zoomed {
            return Self {
                mode: WorkspaceLayoutMode::Focused,
                rows: (focus == WorkspaceFocus::Rows).then_some(body),
                detail: (focus == WorkspaceFocus::Detail).then_some(body),
                separator: None,
            };
        }
        if workspace.rows_hidden && workspace.model.detail_document.is_some() {
            return Self {
                mode: WorkspaceLayoutMode::Focused,
                rows: None,
                detail: Some(body),
                separator: None,
            };
        }
        if width >= workspace.config.min_two_pane_width {
            let maximum_rows_width = width.saturating_sub(21).max(20);
            let rows_width = width
                .saturating_mul(100usize.saturating_sub(workspace.config.detail_ratio as usize))
                / 100;
            let rows_width = workspace
                .rows_width
                .unwrap_or(rows_width.min(48))
                .clamp(20, maximum_rows_width)
                .min(width);
            let separator = WorkspaceRect {
                x: rows_width,
                y: body.y,
                width: usize::from(rows_width < width),
                height: body.height,
            };
            let detail_x = rows_width.saturating_add(separator.width);
            return Self {
                mode: WorkspaceLayoutMode::Columns,
                rows: Some(WorkspaceRect {
                    width: rows_width,
                    ..body
                }),
                detail: Some(WorkspaceRect {
                    x: detail_x,
                    width: width.saturating_sub(detail_x),
                    ..body
                }),
                separator: Some(WorkspaceSeparator::Columns(separator)),
            };
        }

        if height >= workspace.config.min_stacked_height && body.height >= 13 {
            let pane_height = body.height.saturating_sub(1);
            let maximum_rows_height = pane_height.saturating_sub(6);
            let rows_height = pane_height
                .saturating_mul(35)
                .saturating_div(100)
                .clamp(6, 11)
                .min(maximum_rows_height);
            let separator_y = body.y.saturating_add(rows_height);
            let detail_y = separator_y.saturating_add(1);
            return Self {
                mode: WorkspaceLayoutMode::Stacked,
                rows: Some(WorkspaceRect {
                    height: rows_height,
                    ..body
                }),
                detail: Some(WorkspaceRect {
                    y: detail_y,
                    height: body.height.saturating_sub(rows_height).saturating_sub(1),
                    ..body
                }),
                separator: Some(WorkspaceSeparator::Stacked(WorkspaceRect {
                    y: separator_y,
                    height: 1,
                    ..body
                })),
            };
        }

        Self {
            mode: WorkspaceLayoutMode::Focused,
            rows: (workspace.focus == WorkspaceFocus::Rows).then_some(body),
            detail: (workspace.focus == WorkspaceFocus::Detail).then_some(body),
            separator: None,
        }
    }

    fn pane(self, focus: WorkspaceFocus) -> Option<WorkspaceRect> {
        match focus {
            WorkspaceFocus::Rows => self.rows,
            WorkspaceFocus::Detail => self.detail,
        }
    }

    fn visible_rows(self, focus: WorkspaceFocus) -> usize {
        self.pane(focus)
            .map_or(1, |rect| {
                rect.content_height()
                    .saturating_sub(usize::from(focus == WorkspaceFocus::Detail))
            })
            .max(1)
    }

    fn detail_code_width(self, gutter_width: usize) -> usize {
        self.detail
            .map_or(1, |rect| rect.width.saturating_sub(gutter_width + 1).max(1))
    }

    fn focus_at(self, column: usize, row: usize) -> Option<WorkspaceFocus> {
        if self.mode == WorkspaceLayoutMode::Focused {
            return self
                .rows
                .filter(|rect| rect.contains(column, row))
                .map(|_| WorkspaceFocus::Rows)
                .or_else(|| {
                    self.detail
                        .filter(|rect| rect.contains(column, row))
                        .map(|_| WorkspaceFocus::Detail)
                });
        }
        if self.rows.is_some_and(|rect| rect.contains(column, row)) {
            Some(WorkspaceFocus::Rows)
        } else if self.detail.is_some_and(|rect| rect.contains(column, row)) {
            Some(WorkspaceFocus::Detail)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct WorkspaceRow {
    pub id: String,
    #[serde(default)]
    pub selectable: bool,
    #[serde(default)]
    pub depth: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub segments: Vec<PanelSegment>,
    #[serde(default)]
    pub right_segments: Vec<PanelSegment>,
    #[serde(default)]
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct WorkspaceEvent {
    pub workspace_id: String,
    pub action: String,
    pub selected_index: usize,
    pub row: Option<WorkspaceRow>,
    pub focus: WorkspaceFocus,
    pub detail_index: usize,
    pub detail_line: Option<WorkspaceDocumentLine>,
    pub detail_selection: Option<[usize; 2]>,
    pub detail_wrap: bool,
    /// Core-only delivery policy used by the editor and omitted from the
    /// plugin payload.
    #[serde(skip)]
    pub notify_plugin: bool,
}

#[derive(Debug)]
pub struct PluginWorkspace {
    id: String,
    config: WorkspaceConfig,
    model: WorkspaceModel,
    selected: usize,
    scroll: usize,
    focus: WorkspaceFocus,
    zoomed: Option<WorkspaceFocus>,
    detail_cursor: usize,
    detail_scroll: usize,
    detail_horizontal: usize,
    detail_wrap: bool,
    detail_selection_anchor: Option<usize>,
    key_prefix: Option<String>,
    detail_highlights: Vec<Vec<crate::editor::StyleInfo>>,
    detail_word_changes: Vec<Vec<Range<usize>>>,
    detail_visible: Vec<usize>,
    show_metadata: bool,
    action_menu: ActionMenu,
    rows_visible: Vec<usize>,
    collapsed_sections: HashSet<String>,
    filter: String,
    filtering: bool,
    rows_hidden: bool,
    rows_width: Option<usize>,
    dragging_separator: bool,
}

impl PluginWorkspace {
    fn new(id: String, config: WorkspaceConfig) -> Self {
        let detail_wrap = config.detail_wrap;
        Self {
            id,
            config,
            model: WorkspaceModel::default(),
            selected: 0,
            scroll: 0,
            focus: WorkspaceFocus::Rows,
            zoomed: None,
            detail_cursor: 0,
            detail_scroll: 0,
            detail_horizontal: 0,
            detail_wrap,
            detail_selection_anchor: None,
            key_prefix: None,
            detail_highlights: Vec::new(),
            detail_word_changes: Vec::new(),
            detail_visible: Vec::new(),
            show_metadata: false,
            action_menu: ActionMenu::default(),
            rows_visible: Vec::new(),
            collapsed_sections: HashSet::new(),
            filter: String::new(),
            filtering: false,
            rows_hidden: false,
            rows_width: None,
            dragging_separator: false,
        }
    }

    #[cfg(test)]
    fn update(&mut self, model: WorkspaceModel, theme: &Theme) {
        self.update_with_registry(model, theme, &Arc::new(LanguageRegistry::bundled()));
    }

    fn update_with_registry(
        &mut self,
        model: WorkspaceModel,
        theme: &Theme,
        registry: &Arc<LanguageRegistry>,
    ) {
        if model.detail_document.is_none() {
            self.focus = WorkspaceFocus::Rows;
            if self.zoomed == Some(WorkspaceFocus::Detail) {
                self.zoomed = None;
            }
            self.detail_selection_anchor = None;
        }
        let selected_id = self
            .model
            .rows
            .get(self.selected)
            .map(|row| row.id.as_str());
        let selected_path = self
            .model
            .rows
            .get(self.selected)
            .and_then(|row| row.path.as_deref());
        let selected = selected_id
            .and_then(|id| model.rows.iter().position(|row| row.id == id))
            .or_else(|| {
                selected_path.and_then(|path| {
                    model
                        .rows
                        .iter()
                        .position(|row| row.path.as_deref() == Some(path))
                })
            })
            .or_else(|| model.rows.iter().position(|row| row.selectable))
            .unwrap_or(0);
        let previous_document = self.model.detail_document.as_ref();
        let previous_line =
            previous_document.and_then(|document| document.lines.get(self.detail_cursor));
        let same_path = previous_document
            .zip(model.detail_document.as_ref())
            .is_some_and(|(previous, next)| previous.path == next.path);
        let restored_detail = same_path
            .then(|| {
                let previous = previous_line?;
                model
                    .detail_document
                    .as_ref()?
                    .lines
                    .iter()
                    .position(|line| {
                        line.kind == previous.kind
                            && line.text == previous.text
                            && (line.old_line == previous.old_line
                                || line.new_line == previous.new_line)
                    })
            })
            .flatten();
        let document_changed = self.model.detail_document != model.detail_document;
        if document_changed {
            self.detail_selection_anchor = None;
        }
        let first_change = model.detail_document.as_ref().and_then(|document| {
            document
                .lines
                .iter()
                .position(|line| matches!(line.kind.as_str(), "added" | "removed"))
        });
        self.detail_cursor = restored_detail.or(first_change).unwrap_or_else(|| {
            self.detail_cursor.min(
                model
                    .detail_document
                    .as_ref()
                    .map_or(0, |document| document.lines.len().saturating_sub(1)),
            )
        });
        if document_changed {
            self.detail_highlights =
                highlight_document(model.detail_document.as_ref(), theme, registry);
            self.detail_word_changes = word_changes(model.detail_document.as_ref());
        }
        self.model = model;
        self.rebuild_detail_visible();
        self.selected = selected;
        self.rebuild_rows();
        self.scroll = self.scroll.min(self.selected);
        self.detail_scroll = self.detail_scroll.min(self.detail_cursor);
    }

    fn move_selection(&mut self, delta: isize, visible_rows: usize) {
        if self.rows_visible.is_empty() {
            return;
        }
        let mut next = self
            .rows_visible
            .iter()
            .position(|index| *index == self.selected)
            .unwrap_or(0);
        loop {
            let candidate = next
                .saturating_add_signed(delta)
                .min(self.rows_visible.len() - 1);
            if candidate == next {
                break;
            }
            next = candidate;
            if self.row_selectable(self.rows_visible[next]) {
                self.selected = self.rows_visible[next];
                break;
            }
        }
        if next < self.scroll {
            self.scroll = next;
        } else if next >= self.scroll + visible_rows.max(1) {
            self.scroll = next.saturating_sub(visible_rows.saturating_sub(1));
        }
    }

    fn row_selectable(&self, index: usize) -> bool {
        self.model.rows.get(index).is_some_and(|row| {
            row.selectable || (!self.model.actions.is_empty() && row.id.starts_with("section:"))
        })
    }

    fn rebuild_rows(&mut self) {
        let query = self.filter.to_lowercase();
        let mut section = None;
        let mut pending_heading = None;
        self.rows_visible.clear();
        for (index, row) in self.model.rows.iter().enumerate() {
            if row.id.starts_with("section:") {
                section = Some(row.id.as_str());
                if query.is_empty() {
                    self.rows_visible.push(index);
                } else {
                    pending_heading = Some(index);
                }
                continue;
            }
            if query.is_empty()
                && section.is_some_and(|section| self.collapsed_sections.contains(section))
            {
                continue;
            }
            let matches = query.is_empty()
                || row
                    .path
                    .as_deref()
                    .is_some_and(|path| path.to_lowercase().contains(&query))
                || (!row.selectable
                    && row
                        .segments
                        .iter()
                        .any(|segment| segment.text.to_lowercase().contains(&query)));
            if matches {
                if let Some(heading) = pending_heading.take() {
                    self.rows_visible.push(heading);
                }
                self.rows_visible.push(index);
            }
        }
        if !self.rows_visible.contains(&self.selected) {
            self.selected = self
                .rows_visible
                .iter()
                .copied()
                .find(|index| self.model.rows[*index].selectable)
                .or_else(|| self.rows_visible.first().copied())
                .unwrap_or(0);
        }
        self.scroll = self.scroll.min(self.rows_visible.len().saturating_sub(1));
    }

    fn toggle_section(&mut self) {
        if let Some(section) = self
            .model
            .rows
            .iter()
            .take(self.selected + 1)
            .rev()
            .find(|row| row.id.starts_with("section:"))
            .map(|row| row.id.clone())
        {
            if !self.collapsed_sections.remove(&section) {
                self.collapsed_sections.insert(section.clone());
            }
            self.selected = self
                .model
                .rows
                .iter()
                .position(|row| row.id == section)
                .unwrap_or(self.selected);
            self.rebuild_rows();
        }
    }

    fn event(&self, action: String) -> WorkspaceEvent {
        let detail_line = self
            .model
            .detail_document
            .as_ref()
            .and_then(|document| document.lines.get(self.detail_cursor))
            .cloned();
        let detail_selection = self.detail_selection_anchor.map(|anchor| {
            [
                anchor.min(self.detail_cursor),
                anchor.max(self.detail_cursor),
            ]
        });
        let notify_plugin = self.config.notify_detail_navigation
            || !is_core_detail_interaction(self.focus, action.as_str());
        WorkspaceEvent {
            workspace_id: self.id.clone(),
            action,
            selected_index: self.selected,
            row: self
                .rows_visible
                .contains(&self.selected)
                .then(|| self.model.rows.get(self.selected).cloned())
                .flatten(),
            focus: self.focus,
            detail_index: self.detail_cursor,
            detail_line,
            detail_selection,
            detail_wrap: self.detail_wrap,
            notify_plugin,
        }
    }

    fn detail_len(&self) -> usize {
        self.model
            .detail_document
            .as_ref()
            .map_or(0, |document| document.lines.len())
    }

    fn rebuild_detail_visible(&mut self) {
        self.detail_visible = self
            .model
            .detail_document
            .as_ref()
            .map(|document| {
                let has_hunks = document.lines.iter().any(|line| line.kind == "hunk");
                document
                    .lines
                    .iter()
                    .enumerate()
                    .filter(|(_, line)| {
                        self.show_metadata
                            || !has_hunks
                            || line.kind != "meta"
                            || line.text.starts_with('\\')
                    })
                    .map(|(index, _)| index)
                    .collect()
            })
            .unwrap_or_default();
        if !self.detail_visible.contains(&self.detail_cursor) {
            self.detail_cursor = self
                .detail_visible
                .iter()
                .copied()
                .find(|index| *index >= self.detail_cursor)
                .or_else(|| self.detail_visible.last().copied())
                .unwrap_or(0);
        }
    }

    fn gutter_width(&self) -> usize {
        let maximum = self
            .model
            .detail_document
            .as_ref()
            .into_iter()
            .flat_map(|document| &document.lines)
            .flat_map(|line| [line.old_line, line.new_line])
            .flatten()
            .max()
            .unwrap_or(1);
        maximum.to_string().len().max(2) * 2 + 4
    }

    fn detail_line_at_visual_offset(&self, offset: usize, code_width: usize) -> usize {
        let Some(document) = self.model.detail_document.as_ref() else {
            return 0;
        };
        let mut remaining = offset;
        for &index in self
            .detail_visible
            .iter()
            .filter(|index| **index >= self.detail_scroll)
        {
            let line = &document.lines[index];
            let visual_rows = if self.detail_wrap {
                display_width(&line.text).max(1).div_ceil(code_width.max(1))
            } else {
                1
            };
            if remaining < visual_rows {
                return index;
            }
            remaining = remaining.saturating_sub(visual_rows);
        }
        document.lines.len().saturating_sub(1)
    }

    fn move_detail(&mut self, delta: isize, visible_rows: usize) {
        let len = self.detail_visible.len();
        if len == 0 {
            return;
        }
        let position = self
            .detail_visible
            .iter()
            .position(|index| *index == self.detail_cursor)
            .unwrap_or(0);
        self.detail_cursor = self.detail_visible[position
            .saturating_add_signed(delta)
            .min(len.saturating_sub(1))];
        if self.detail_cursor < self.detail_scroll {
            self.detail_scroll = self.detail_cursor;
        } else if self.detail_cursor >= self.detail_scroll + visible_rows.max(1) {
            self.detail_scroll = self
                .detail_cursor
                .saturating_sub(visible_rows.saturating_sub(1));
        }
    }

    fn ensure_detail_cursor_visible(&mut self, layout: WorkspaceLayout) {
        let visible_rows = layout.visible_rows(WorkspaceFocus::Detail);
        if self.detail_cursor < self.detail_scroll {
            self.detail_scroll = self.detail_cursor;
            return;
        }
        let code_width = layout.detail_code_width(self.gutter_width());
        let Some(document) = self.model.detail_document.as_ref() else {
            return;
        };
        let occupied = self
            .detail_visible
            .iter()
            .copied()
            .filter(|index| *index >= self.detail_scroll && *index <= self.detail_cursor)
            .map(|index| {
                let line = &document.lines[index];
                if self.detail_wrap {
                    display_width(&line.text).max(1).div_ceil(code_width)
                } else {
                    1
                }
            })
            .sum::<usize>();
        if occupied > visible_rows {
            self.detail_scroll = self.detail_cursor;
        }
    }

    fn move_to_hunk(&mut self, forward: bool, visible_rows: usize) {
        let Some(document) = self.model.detail_document.as_ref() else {
            return;
        };
        let current = document
            .lines
            .get(self.detail_cursor)
            .and_then(|line| line.hunk_id.as_deref());
        let target = if forward {
            document
                .lines
                .iter()
                .enumerate()
                .skip(self.detail_cursor.saturating_add(1))
                .find(|(_, line)| {
                    line.hunk_id
                        .as_deref()
                        .is_some_and(|id| Some(id) != current)
                })
                .map(|(index, _)| index)
        } else {
            document
                .lines
                .iter()
                .enumerate()
                .take(self.detail_cursor)
                .rev()
                .find(|(_, line)| {
                    line.hunk_id
                        .as_deref()
                        .is_some_and(|id| Some(id) != current)
                })
                .map(|(index, _)| index)
        };
        if let Some(target) = target {
            let current = self
                .detail_visible
                .iter()
                .position(|index| *index == self.detail_cursor)
                .unwrap_or(0);
            let target = self
                .detail_visible
                .iter()
                .position(|index| *index == target)
                .unwrap_or(current);
            self.move_detail(target as isize - current as isize, visible_rows);
        }
    }

    fn handle_action(&mut self, mut action: String, height: usize, width: usize) -> WorkspaceEvent {
        if self.filtering {
            match action.as_str() {
                "filter_accept" => self.filtering = false,
                "filter_cancel" => {
                    self.filtering = false;
                    self.filter.clear();
                }
                "filter_backspace" => {
                    self.filter.pop();
                }
                _ => {
                    if let Some(text) = action.strip_prefix("filter_text:") {
                        self.filter.push_str(text);
                    }
                }
            }
            self.rebuild_rows();
            return self.event(
                if self.filtering {
                    "noop"
                } else {
                    "filter_changed"
                }
                .to_string(),
            );
        }
        if self.action_menu.is_open() {
            let actions = self.actions();
            let Some(selected) = self.action_menu.handle(&action, &actions) else {
                return self.event("noop".to_string());
            };
            action = selected;
        } else if matches!(action.as_str(), "?" | "F1") && !self.model.actions.is_empty() {
            self.action_menu.open();
            return self.event("noop".to_string());
        }
        if action == "?" {
            self.action_menu.open();
            return self.event("noop".to_string());
        }
        if let Some(prefix) = self.key_prefix.take() {
            action = match (prefix.as_str(), action.as_str()) {
                ("ctrl_w", "w" | "ctrl_w") => "focus_next",
                ("ctrl_w", "W" | "p") => "focus_previous",
                ("ctrl_w", "h") => "focus_rows",
                ("ctrl_w", "l") => "focus_detail",
                ("ctrl_w", "o") => "toggle_rows",
                ("ctrl_w", "z") => "toggle_zoom",
                ("ctrl_w", "<") => "narrow_rows",
                ("ctrl_w", ">") => "widen_rows",
                ("ctrl_w", "c" | "q") => "escape",
                ("g", "g") => "first",
                ("[", "h") => "previous_hunk",
                ("]", "h") => "next_hunk",
                _ => "noop",
            }
            .to_string();
        } else if matches!(action.as_str(), "ctrl_w" | "g" | "[" | "]") {
            self.key_prefix = Some(action);
            return self.event("prefix".to_string());
        }

        if action == "escape" && self.detail_selection_anchor.is_some() {
            action = "cancel_selection".to_string();
        } else if self.focus == WorkspaceFocus::Detail {
            action = match action.as_str() {
                "h" => "left",
                "l" => "right",
                "0" if !self.detail_wrap => "horizontal_start",
                "$" if !self.detail_wrap => "horizontal_end",
                "W" => "toggle_wrap",
                "M" => "toggle_metadata",
                "v" => "visual",
                other => other,
            }
            .to_string();
        }

        if self.focus == WorkspaceFocus::Rows
            && action == "activate"
            && self
                .model
                .rows
                .get(self.selected)
                .is_some_and(|row| row.id.starts_with("section:"))
        {
            action = "collapse_section".to_string();
        }
        if self.focus == WorkspaceFocus::Rows {
            action = match action.as_str() {
                "/" => "filter",
                "C" => "collapse_section",
                other => other,
            }
            .to_string();
        }

        if matches!(
            action.as_str(),
            "toggle"
                | "back_toggle"
                | "focus_next"
                | "focus_previous"
                | "focus_rows"
                | "focus_detail"
                | "toggle_rows"
                | "narrow_rows"
                | "widen_rows"
        ) {
            self.zoomed = None;
        }
        let layout = WorkspaceLayout::calculate(self, height, width);
        let visible_rows = layout.visible_rows(self.focus);

        match action.as_str() {
            "filter" => self.filtering = true,
            "toggle_zoom" => self.toggle_zoom(),
            "collapse_section" => {
                self.toggle_section();
                action = "filter_changed".to_string();
            }
            "toggle_rows" => {
                self.rows_hidden = !self.rows_hidden;
                if self.rows_hidden && self.model.detail_document.is_some() {
                    self.focus = WorkspaceFocus::Detail;
                }
            }
            "narrow_rows" | "widen_rows" => {
                let current = layout.rows.map_or(36, |rect| rect.width);
                self.rows_width = Some(
                    current
                        .saturating_add_signed(if action == "narrow_rows" { -4 } else { 4 })
                        .clamp(20, width.saturating_sub(21).max(20)),
                );
            }
            "toggle" | "back_toggle" | "focus_next" | "focus_previous" => {
                if self.model.detail_document.is_some() {
                    self.focus = match self.focus {
                        WorkspaceFocus::Rows => WorkspaceFocus::Detail,
                        WorkspaceFocus::Detail => WorkspaceFocus::Rows,
                    };
                    if self.focus == WorkspaceFocus::Rows {
                        self.rows_hidden = false;
                    }
                }
            }
            "focus_rows" => {
                self.rows_hidden = false;
                self.focus = WorkspaceFocus::Rows;
            }
            "focus_detail" if self.model.detail_document.is_some() => {
                self.focus = WorkspaceFocus::Detail;
            }
            "up" => match self.focus {
                WorkspaceFocus::Rows => self.move_selection(-1, visible_rows),
                WorkspaceFocus::Detail => self.move_detail(-1, visible_rows),
            },
            "down" => match self.focus {
                WorkspaceFocus::Rows => self.move_selection(1, visible_rows),
                WorkspaceFocus::Detail => self.move_detail(1, visible_rows),
            },
            "half_page_up" => match self.focus {
                WorkspaceFocus::Rows => {
                    self.move_selection(-((visible_rows / 2).max(1) as isize), visible_rows)
                }
                WorkspaceFocus::Detail => {
                    self.move_detail(-((visible_rows / 2).max(1) as isize), visible_rows)
                }
            },
            "half_page_down" => match self.focus {
                WorkspaceFocus::Rows => {
                    self.move_selection((visible_rows / 2).max(1) as isize, visible_rows)
                }
                WorkspaceFocus::Detail => {
                    self.move_detail((visible_rows / 2).max(1) as isize, visible_rows)
                }
            },
            "page_up" => match self.focus {
                WorkspaceFocus::Rows => self.move_selection(-(visible_rows as isize), visible_rows),
                WorkspaceFocus::Detail => self.move_detail(-(visible_rows as isize), visible_rows),
            },
            "page_down" => match self.focus {
                WorkspaceFocus::Rows => self.move_selection(visible_rows as isize, visible_rows),
                WorkspaceFocus::Detail => self.move_detail(visible_rows as isize, visible_rows),
            },
            "first" => match self.focus {
                WorkspaceFocus::Rows => {
                    self.move_selection(-(self.selected as isize), visible_rows)
                }
                WorkspaceFocus::Detail => {
                    self.move_detail(-(self.detail_cursor as isize), visible_rows)
                }
            },
            "last" => match self.focus {
                WorkspaceFocus::Rows => {
                    self.move_selection(self.model.rows.len() as isize, visible_rows)
                }
                WorkspaceFocus::Detail => {
                    self.move_detail(self.detail_len() as isize, visible_rows)
                }
            },
            "previous_hunk" if self.focus == WorkspaceFocus::Detail => {
                self.move_to_hunk(false, visible_rows)
            }
            "next_hunk" if self.focus == WorkspaceFocus::Detail => {
                self.move_to_hunk(true, visible_rows)
            }
            "visual" if self.focus == WorkspaceFocus::Detail => {
                self.detail_selection_anchor = match self.detail_selection_anchor {
                    Some(_) => None,
                    None => Some(self.detail_cursor),
                };
            }
            "cancel_selection" => self.detail_selection_anchor = None,
            "toggle_metadata" if self.focus == WorkspaceFocus::Detail => {
                self.show_metadata = !self.show_metadata;
                self.rebuild_detail_visible();
                self.detail_scroll = self.detail_scroll.min(self.detail_cursor);
            }
            "toggle_wrap" if self.focus == WorkspaceFocus::Detail => {
                self.detail_wrap = !self.detail_wrap;
                if self.detail_wrap {
                    self.detail_horizontal = 0;
                }
            }
            "left" if self.focus == WorkspaceFocus::Detail && !self.detail_wrap => {
                self.detail_horizontal = self.detail_horizontal.saturating_sub(4);
            }
            "right" if self.focus == WorkspaceFocus::Detail && !self.detail_wrap => {
                self.detail_horizontal = self.detail_horizontal.saturating_add(4);
            }
            "horizontal_start" if self.focus == WorkspaceFocus::Detail => {
                self.detail_horizontal = 0;
            }
            "horizontal_end" if self.focus == WorkspaceFocus::Detail && !self.detail_wrap => {
                let max_width = self
                    .model
                    .detail_document
                    .as_ref()
                    .and_then(|document| {
                        document
                            .lines
                            .iter()
                            .map(|line| display_width(&line.text))
                            .max()
                    })
                    .unwrap_or_default();
                self.detail_horizontal =
                    max_width.saturating_sub(layout.detail_code_width(self.gutter_width()));
            }
            _ => {}
        }
        if self.focus == WorkspaceFocus::Detail {
            self.ensure_detail_cursor_visible(WorkspaceLayout::calculate(self, height, width));
        }
        self.event(action)
    }

    fn toggle_zoom(&mut self) {
        self.zoomed = if self.zoomed.is_some() {
            None
        } else {
            Some(self.focus)
        };
        self.dragging_separator = false;
    }

    fn actions(&self) -> Vec<UiAction> {
        if self.filtering {
            return vec![
                UiAction::new("filter_accept", "Enter", "apply filter")
                    .with_priority(ActionPriority::Essential),
                UiAction::new("filter_cancel", "Esc", "clear filter")
                    .with_priority(ActionPriority::Essential),
            ];
        }
        let detail = self.focus == WorkspaceFocus::Detail;
        let line = self
            .model
            .detail_document
            .as_ref()
            .and_then(|document| document.lines.get(self.detail_cursor));
        let row = self
            .rows_visible
            .contains(&self.selected)
            .then(|| self.model.rows.get(self.selected))
            .flatten();
        let section = if detail {
            line.map(|line| &line.data)
        } else {
            row.map(|row| &row.data)
        }
        .and_then(|data| data.get("section"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
        let selected = self.detail_selection_anchor.is_some();
        let scope = if detail {
            self.detail_selection_anchor
                .map(|anchor| format!("{} lines", anchor.abs_diff(self.detail_cursor) + 1))
                .unwrap_or_else(|| "line".to_string())
        } else {
            "file".to_string()
        };
        let mut actions = self
            .model
            .actions
            .iter()
            .filter(|action| {
                (action.focus.is_empty() || action.focus == if detail { "detail" } else { "rows" })
                    && (action.sections.is_empty()
                        || action.sections.iter().any(|candidate| candidate == section))
                    && match action.selection.as_str() {
                        "" => true,
                        "range" => selected,
                        "item" => {
                            if detail {
                                line.is_some()
                            } else {
                                row.is_some_and(|row| row.selectable)
                            }
                        }
                        "none" => !selected,
                        _ => false,
                    }
                    && (!action.change_only
                        || !detail
                        || selected
                        || line
                            .is_some_and(|line| matches!(line.kind.as_str(), "added" | "removed")))
                    && (!action.hunk_only || line.is_some_and(|line| line.hunk_id.is_some()))
            })
            .map(|action| {
                let mut hint = action.hint.clone();
                hint.label = hint.label.replace("{scope}", &scope);
                hint
            })
            .collect::<Vec<_>>();
        if !actions.is_empty() {
            if detail {
                actions.extend([
                    UiAction::new("visual", "v", "select").with_enabled(!selected),
                    UiAction::new("cancel_selection", "Esc", "clear selection")
                        .with_enabled(selected)
                        .with_priority(ActionPriority::Essential),
                    UiAction::new("previous_hunk", "[h", "previous hunk")
                        .with_priority(ActionPriority::Secondary),
                    UiAction::new("next_hunk", "]h", "next hunk")
                        .with_priority(ActionPriority::Secondary),
                    UiAction::new("toggle_wrap", "W", "wrap")
                        .with_priority(ActionPriority::Secondary),
                    UiAction::new("toggle_metadata", "M", "patch metadata")
                        .with_priority(ActionPriority::Secondary),
                ]);
            }
            if !detail {
                actions.push(UiAction::new("filter", "/", "filter files"));
                actions.push(
                    UiAction::new("collapse_section", "C", "collapse section")
                        .with_priority(ActionPriority::Secondary),
                );
            }
            actions.push(
                UiAction::new("toggle_rows", "Ctrl+w o", "toggle files")
                    .with_priority(ActionPriority::Secondary),
            );
            actions.push(
                UiAction::new("narrow_rows", "Ctrl+w <", "narrow files")
                    .with_priority(ActionPriority::Secondary),
            );
            actions.push(
                UiAction::new("widen_rows", "Ctrl+w >", "widen files")
                    .with_priority(ActionPriority::Secondary),
            );
            actions.push(
                UiAction::new("focus_next", "Tab", if detail { "files" } else { "diff" })
                    .with_enabled(self.model.detail_document.is_some()),
            );
            actions
                .push(UiAction::new("?", "?", "actions").with_priority(ActionPriority::Essential));
            actions.push(UiAction::new("q", "q", "close").with_priority(ActionPriority::Essential));
        }
        actions
    }
}

fn is_core_detail_interaction(focus: WorkspaceFocus, action: &str) -> bool {
    matches!(
        action,
        "prefix"
            | "noop"
            | "toggle"
            | "back_toggle"
            | "focus_next"
            | "focus_previous"
            | "focus_rows"
            | "focus_detail"
            | "filter"
            | "toggle_rows"
            | "toggle_zoom"
            | "narrow_rows"
            | "widen_rows"
    ) || (focus == WorkspaceFocus::Detail
        && matches!(
            action,
            "up" | "down"
                | "half_page_up"
                | "half_page_down"
                | "page_up"
                | "page_down"
                | "first"
                | "last"
                | "previous_hunk"
                | "next_hunk"
                | "visual"
                | "cancel_selection"
                | "toggle_wrap"
                | "toggle_metadata"
                | "left"
                | "right"
                | "horizontal_start"
                | "horizontal_end"
                | "mouse_up"
                | "mouse_down"
                | "mouse_left"
                | "mouse_right"
                | "mouse_click"
        ))
}

#[derive(Debug, Default)]
pub struct WorkspaceManager {
    active: Option<PluginWorkspace>,
    widths: HashMap<String, Option<usize>>,
}

impl WorkspaceManager {
    pub fn open(&mut self, id: String, config: WorkspaceConfig) {
        if let Some(active) = &self.active {
            self.widths.insert(active.id.clone(), active.rows_width);
        }
        let mut workspace = PluginWorkspace::new(id.clone(), config);
        workspace.rows_width = self.widths.get(&id).copied().flatten();
        self.active = Some(workspace);
    }

    pub fn update(&mut self, id: &str, model: WorkspaceModel, theme: &Theme) -> bool {
        self.update_with_registry(id, model, theme, &Arc::new(LanguageRegistry::bundled()))
    }

    /// Updates workspace content using the editor's current shared language registry.
    pub fn update_with_registry(
        &mut self,
        id: &str,
        model: WorkspaceModel,
        theme: &Theme,
        registry: &Arc<LanguageRegistry>,
    ) -> bool {
        let Some(workspace) = self.active.as_mut().filter(|workspace| workspace.id == id) else {
            return false;
        };
        workspace.update_with_registry(model, theme, registry);
        true
    }

    pub fn close(&mut self, id: &str) -> bool {
        if self
            .active
            .as_ref()
            .is_some_and(|workspace| workspace.id == id)
        {
            self.widths.insert(
                id.to_string(),
                self.active
                    .as_ref()
                    .and_then(|workspace| workspace.rows_width),
            );
            self.active = None;
            true
        } else {
            false
        }
    }

    pub fn update_theme(&mut self, theme: &Theme) {
        self.update_theme_with_registry(theme, &Arc::new(LanguageRegistry::bundled()));
    }

    /// Rebuilds workspace detail colors against the editor's shared language registry.
    pub fn update_theme_with_registry(&mut self, theme: &Theme, registry: &Arc<LanguageRegistry>) {
        if let Some(workspace) = self.active.as_mut() {
            workspace.detail_highlights =
                highlight_document(workspace.model.detail_document.as_ref(), theme, registry);
        }
    }

    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    pub(crate) fn toggle_zoom(&mut self) {
        if let Some(workspace) = self.active.as_mut() {
            workspace.toggle_zoom();
        }
    }

    pub fn is_filtering(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|workspace| workspace.filtering)
    }

    pub fn handle_action(
        &mut self,
        action: String,
        height: usize,
        width: usize,
    ) -> Option<WorkspaceEvent> {
        let workspace = self.active.as_mut()?;
        Some(workspace.handle_action(action, height, width))
    }

    pub fn handle_mouse(
        &mut self,
        action: &str,
        column: usize,
        row: usize,
        height: usize,
        width: usize,
    ) -> Option<WorkspaceEvent> {
        let workspace = self.active.as_mut()?;
        let layout = WorkspaceLayout::calculate(workspace, height, width);
        if action == "mouse_release" {
            workspace.dragging_separator = false;
            return Some(workspace.event("noop".to_string()));
        }
        if action == "mouse_click"
            && matches!(layout.separator, Some(WorkspaceSeparator::Columns(rect)) if rect.contains(column, row))
        {
            workspace.dragging_separator = true;
            return Some(workspace.event("noop".to_string()));
        }
        if action == "mouse_drag" && workspace.dragging_separator {
            workspace.rows_width = Some(column.clamp(20, width.saturating_sub(21).max(20)));
            return Some(workspace.event("noop".to_string()));
        }
        if let Some(focus) = layout.focus_at(column, row) {
            if focus == WorkspaceFocus::Rows || workspace.model.detail_document.is_some() {
                workspace.focus = focus;
            }
        }
        let visible_rows = layout.visible_rows(workspace.focus);
        match action {
            "mouse_up" => match workspace.focus {
                WorkspaceFocus::Rows => workspace.move_selection(-3, visible_rows),
                WorkspaceFocus::Detail => workspace.move_detail(-3, visible_rows),
            },
            "mouse_down" => match workspace.focus {
                WorkspaceFocus::Rows => workspace.move_selection(3, visible_rows),
                WorkspaceFocus::Detail => workspace.move_detail(3, visible_rows),
            },
            "mouse_left" if workspace.focus == WorkspaceFocus::Detail && !workspace.detail_wrap => {
                workspace.detail_horizontal = workspace.detail_horizontal.saturating_sub(4);
            }
            "mouse_right"
                if workspace.focus == WorkspaceFocus::Detail && !workspace.detail_wrap =>
            {
                workspace.detail_horizontal = workspace.detail_horizontal.saturating_add(4);
            }
            "mouse_click" => match workspace.focus {
                WorkspaceFocus::Rows => {
                    if let Some(offset) = layout.rows.and_then(|rect| rect.content_offset(row)) {
                        if let Some(candidate) = workspace
                            .rows_visible
                            .get(workspace.scroll.saturating_add(offset))
                            .copied()
                            .filter(|candidate| workspace.row_selectable(*candidate))
                        {
                            workspace.selected = candidate;
                        }
                    }
                }
                WorkspaceFocus::Detail => {
                    if let Some(offset) = layout
                        .detail
                        .and_then(|rect| rect.content_offset(row))
                        .and_then(|offset| offset.checked_sub(1))
                    {
                        workspace.detail_cursor = workspace.detail_line_at_visual_offset(
                            offset,
                            layout.detail_code_width(workspace.gutter_width()),
                        );
                    }
                }
            },
            _ => {}
        }
        if workspace.focus == WorkspaceFocus::Detail {
            workspace
                .ensure_detail_cursor_visible(WorkspaceLayout::calculate(workspace, height, width));
        }
        Some(workspace.event(action.to_string()))
    }

    pub fn render(&self, buffer: &mut RenderBuffer, theme: &Theme, icons: PickerIconsConfig) {
        let Some(workspace) = &self.active else {
            return;
        };
        let editor_style = &theme.style;
        for y in 0..buffer.height {
            buffer.set_text(0, y, &" ".repeat(buffer.width), editor_style);
        }
        if buffer.width < 4 || buffer.height < 4 {
            return;
        }

        let zoom_hint = if workspace.zoomed.is_some() {
            " · ZOOM · Ctrl-w z"
        } else {
            ""
        };
        let title = format!(" {}{} ", workspace.config.title, zoom_hint);
        buffer.set_text(
            1,
            0,
            &truncate_display_width(&title, buffer.width - 2),
            editor_style,
        );
        render_segments(
            buffer,
            (1, 1, buffer.width - 2),
            &workspace.model.header,
            editor_style,
            theme,
            false,
        );

        let layout = WorkspaceLayout::calculate(workspace, buffer.height, buffer.width);
        if let Some(rect) = layout.rows {
            render_row_pane(
                buffer,
                workspace,
                theme,
                icons,
                (rect.x, rect.width, rect.y, rect.height),
            );
        }

        match layout.separator {
            Some(WorkspaceSeparator::Columns(rect)) => {
                for y in rect.y..rect.y.saturating_add(rect.height) {
                    buffer.set_text(rect.x, y, "│", editor_style);
                }
            }
            Some(WorkspaceSeparator::Stacked(rect)) => {
                buffer.set_text(rect.x, rect.y, &"─".repeat(rect.width), editor_style);
            }
            None => {}
        }
        if let Some(rect) = layout.detail {
            render_detail_pane(
                buffer,
                workspace,
                theme,
                rect.x,
                rect.width,
                rect.y,
                rect.height,
            );
        }

        if workspace.model.actions.is_empty() {
            render_segments(
                buffer,
                (1, buffer.height - 1, buffer.width - 2),
                &workspace.model.footer,
                editor_style,
                theme,
                false,
            );
        } else {
            let actions = workspace.actions();
            let context = if workspace.filtering {
                "FILTER"
            } else if workspace.detail_selection_anchor.is_some() {
                "VISUAL"
            } else if workspace.focus == WorkspaceFocus::Detail {
                "DIFF"
            } else {
                "FILES"
            };
            ActionBar::new(&actions)
                .with_context(context)
                .with_status(
                    (!workspace.model.status.is_empty()).then_some(workspace.model.status.as_str()),
                )
                .render(
                    buffer,
                    1,
                    buffer.height - 1,
                    buffer.width - 2,
                    theme,
                    editor_style,
                );
            workspace.action_menu.render(buffer, theme, &actions);
        }
    }
}

fn highlight_document(
    document: Option<&WorkspaceDocument>,
    theme: &Theme,
    registry: &Arc<LanguageRegistry>,
) -> Vec<Vec<crate::editor::StyleInfo>> {
    let Some(document) = document else {
        return Vec::new();
    };
    let Some(mut highlighter) = Highlighter::with_registry(theme, Arc::clone(registry)).ok() else {
        return (0..document.lines.len()).map(|_| Vec::new()).collect();
    };

    // A unified diff interleaves two different programs. Feeding removed and
    // added lines to one parser makes replacements (especially multiline ones)
    // corrupt the syntax state for everything that follows. Parse an old-file
    // and new-file projection independently, then use the matching side for
    // each displayed line.
    let old = highlight_document_projection(document, &mut highlighter, false);
    let new = highlight_document_projection(document, &mut highlighter, true);
    document
        .lines
        .iter()
        .zip(old.into_iter().zip(new))
        .map(|(line, (old_spans, new_spans))| match line.kind.as_str() {
            "removed" => old_spans,
            "added" | "context" => new_spans,
            _ => Vec::new(),
        })
        .collect()
}

/// Pair replacement lines within a bounded change block. Pure additions/removals
/// retain their line tint; only actual replacements receive stronger word marks.
fn word_changes(document: Option<&WorkspaceDocument>) -> Vec<Vec<Range<usize>>> {
    let Some(document) = document else {
        return Vec::new();
    };
    let mut result = vec![Vec::new(); document.lines.len()];
    let mut index = 0;
    while index < document.lines.len() {
        if document.lines[index].kind != "removed" {
            index += 1;
            continue;
        }
        let old_start = index;
        while index < document.lines.len() && document.lines[index].kind == "removed" {
            index += 1;
        }
        let new_start = index;
        while index < document.lines.len() && document.lines[index].kind == "added" {
            index += 1;
        }
        let pairs = (new_start - old_start).min(index - new_start).min(128);
        for offset in 0..pairs {
            let old = &document.lines[old_start + offset].text;
            let new = &document.lines[new_start + offset].text;
            if old.len().saturating_add(new.len()) > 8192 {
                continue;
            }
            let old_words = old.split_word_bounds().collect::<Vec<_>>();
            let new_words = new.split_word_bounds().collect::<Vec<_>>();
            let diff = similar::TextDiff::from_slices(&old_words, &new_words);
            let (mut old_byte, mut new_byte) = (0, 0);
            for change in diff.iter_all_changes() {
                let length = change.value().len();
                match change.tag() {
                    similar::ChangeTag::Equal => {
                        old_byte += length;
                        new_byte += length;
                    }
                    similar::ChangeTag::Delete => {
                        result[old_start + offset].push(old_byte..old_byte + length);
                        old_byte += length;
                    }
                    similar::ChangeTag::Insert => {
                        result[new_start + offset].push(new_byte..new_byte + length);
                        new_byte += length;
                    }
                }
            }
        }
    }
    result
}

fn highlight_document_projection(
    document: &WorkspaceDocument,
    highlighter: &mut Highlighter,
    new_side: bool,
) -> Vec<Vec<crate::editor::StyleInfo>> {
    let source_lines = document
        .lines
        .iter()
        .map(|line| match (line.kind.as_str(), new_side) {
            ("context", _) | ("added", true) | ("removed", false) => line.text.as_str(),
            _ => "",
        })
        .collect::<Vec<_>>();
    let source = source_lines.join("\n");
    let spans = highlighter
        .highlight_for_file(Some(&document.path), &source)
        .unwrap_or_default();
    let mut result = (0..document.lines.len())
        .map(|_| Vec::new())
        .collect::<Vec<_>>();
    let mut line_start = 0;
    for (index, text) in source_lines.iter().enumerate() {
        let line_end = line_start + text.len();
        for span in spans
            .iter()
            .filter(|span| span.start < line_end && span.end > line_start)
        {
            result[index].push(crate::editor::StyleInfo {
                start: span.start.saturating_sub(line_start),
                end: span.end.min(line_end).saturating_sub(line_start),
                style: span.style.clone(),
            });
        }
        line_start = line_end.saturating_add(1);
    }
    result
}

fn render_row_pane(
    buffer: &mut RenderBuffer,
    workspace: &PluginWorkspace,
    theme: &Theme,
    icons: PickerIconsConfig,
    rect: (usize, usize, usize, usize),
) {
    let (x, width, top, height) = rect;
    if width == 0 || height == 0 {
        return;
    }
    let active = workspace.focus == WorkspaceFocus::Rows;
    let mut title_style = SurfacePalette::new(theme, &theme.style).primary;
    title_style.bold = active;
    let title = if workspace.filtering || !workspace.filter.is_empty() {
        format!(
            "{} Changes /{}",
            if active { "›" } else { " " },
            workspace.filter
        )
    } else {
        format!("{} Changes", if active { "›" } else { " " })
    };
    buffer.set_text(
        x + 1,
        top,
        &truncate_display_width(&title, width.saturating_sub(1)),
        &title_style,
    );
    let content_top = top + 1;
    let content_height = height.saturating_sub(1);
    if workspace.rows_visible.is_empty() && !workspace.filter.is_empty() && content_height > 0 {
        let muted = SurfacePalette::new(theme, &theme.style).muted;
        buffer.set_text(
            x + 1,
            content_top,
            &truncate_display_width("No matching files", width.saturating_sub(2)),
            &muted,
        );
    }
    for (screen_row, &row_index) in workspace
        .rows_visible
        .iter()
        .skip(workspace.scroll)
        .take(content_height)
        .enumerate()
    {
        let row = &workspace.model.rows[row_index];
        let y = content_top + screen_row;
        let selected = row_index == workspace.selected && workspace.row_selectable(row_index);
        let row_style = if selected && active {
            theme.selected_style(
                &theme.style,
                &theme.list_selection_style(),
                SelectionForegroundPriority::Selection,
            )
        } else {
            theme.style.clone()
        };
        buffer.set_text(x, y, &fit_display_width("", width), &row_style);
        let mut content_x = x + 1 + row.depth.saturating_mul(2);
        if row.id.starts_with("section:") && !workspace.model.actions.is_empty() {
            buffer.set_text(
                content_x,
                y,
                if workspace.collapsed_sections.contains(&row.id) {
                    "▸ "
                } else {
                    "▾ "
                },
                &row_style,
            );
            content_x += 2;
        }
        if let Some(path) = row.path.as_deref() {
            let icon = IconCatalog::file(path, icons.style);
            if !icon.glyph.is_empty() {
                let mut icon_style = row_style.clone();
                if icons.color {
                    icon_style.fg = icon.color.or(icon_style.fg);
                }
                if selected {
                    icon_style = theme.ensure_text_contrast(&icon_style);
                }
                buffer.set_text(content_x, y, &fit_display_width(icon.glyph, 2), &icon_style);
                content_x += 3;
            }
        }
        let right_width = row
            .right_segments
            .iter()
            .map(|segment| display_width(&segment.text))
            .sum::<usize>();
        render_segments(
            buffer,
            (
                content_x,
                y,
                width.saturating_sub(content_x.saturating_sub(x) + right_width + 2),
            ),
            &row.segments,
            &row_style,
            theme,
            selected,
        );
        if right_width > 0 && right_width + 1 < width {
            render_segments(
                buffer,
                (x + width.saturating_sub(right_width + 1), y, right_width),
                &row.right_segments,
                &row_style,
                theme,
                selected,
            );
        }
        if selected {
            buffer.set_text(x, y, if active { "›" } else { "·" }, &row_style);
        }
    }
}

fn render_detail_pane(
    buffer: &mut RenderBuffer,
    workspace: &PluginWorkspace,
    theme: &Theme,
    x: usize,
    width: usize,
    top: usize,
    height: usize,
) {
    if width < 4 || height == 0 {
        return;
    }
    let active = workspace.focus == WorkspaceFocus::Detail;
    let mut title_style = SurfacePalette::new(theme, &theme.style).primary;
    title_style.bold = active;
    let wrap_label = if workspace.detail_wrap {
        "wrap"
    } else {
        "nowrap"
    };
    let marker = if active { "›" } else { " " };
    let title = workspace.model.detail_document.as_ref().map_or_else(
        || {
            format!(
                " {marker} {} · {wrap_label}",
                if workspace.model.detail_title.is_empty() {
                    "Diff"
                } else {
                    &workspace.model.detail_title
                }
            )
        },
        |document| {
            let section = document
                .lines
                .iter()
                .find_map(|line| line.data.get("section").and_then(serde_json::Value::as_str))
                .unwrap_or("diff");
            format!(
                " {marker} {} · {section} · +{} −{}",
                document.path, document.added, document.removed
            )
        },
    );
    buffer.set_text(x, top, &truncate_display_width(&title, width), &title_style);
    let content_top = top + 2;
    let content_height = height.saturating_sub(2);
    let Some(document) = workspace.model.detail_document.as_ref() else {
        for (index, line) in workspace
            .model
            .detail
            .iter()
            .take(content_height)
            .enumerate()
        {
            render_segments(
                buffer,
                (x + 1, content_top + index, width.saturating_sub(2)),
                line,
                &theme.style,
                theme,
                false,
            );
        }
        return;
    };

    if height > 1 {
        let palette = SurfacePalette::new(theme, &theme.style);
        let hunks = document
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.kind == "hunk")
            .collect::<Vec<_>>();
        let current = hunks
            .iter()
            .rposition(|(index, _)| *index <= workspace.detail_cursor);
        let context = current
            .map(|position| {
                let header = &hunks[position].1.text;
                let symbol = header.split("@@").nth(2).unwrap_or_default().trim();
                format!(
                    "Hunk {} of {}{}{}",
                    position + 1,
                    hunks.len(),
                    if symbol.is_empty() { "" } else { " · " },
                    symbol
                )
            })
            .unwrap_or_else(|| "File details".to_string());
        let context = if document.truncated {
            format!(
                "{context} · first {}/{} lines · L full patch",
                document.lines.len(),
                document.total_lines
            )
        } else {
            format!("{context} · {wrap_label}")
        };
        buffer.set_text(
            x,
            top + 1,
            &fit_display_width(&format!(" {context}"), width),
            &palette.muted,
        );
    }

    let gutter_width = workspace.gutter_width().min(width.saturating_sub(1));
    let number_width = workspace.gutter_width().saturating_sub(4) / 2;
    let code_width = width.saturating_sub(gutter_width + 1).max(1);
    let selection = workspace.detail_selection_anchor.map(|anchor| {
        (
            anchor.min(workspace.detail_cursor),
            anchor.max(workspace.detail_cursor),
        )
    });
    let mut screen_row = 0;
    let palette = DiffPalette::new(theme);
    for &line_index in workspace
        .detail_visible
        .iter()
        .filter(|index| **index >= workspace.detail_scroll)
    {
        let line = &document.lines[line_index];
        if screen_row >= content_height {
            break;
        }
        let segments = if workspace.detail_wrap {
            wrapped_slices(&line.text, code_width)
        } else {
            vec![display_slice(
                &line.text,
                workspace.detail_horizontal,
                code_width,
            )]
        };
        for (segment_index, segment) in segments.into_iter().enumerate() {
            if screen_row >= content_height {
                break;
            }
            let y = content_top + screen_row;
            let selected =
                selection.is_some_and(|(start, end)| line_index >= start && line_index <= end);
            let cursor = active && line_index == workspace.detail_cursor;
            let mut line_style = diff_line_style(&line.kind, theme, &palette);
            if selected {
                line_style = theme.selected_style(
                    &line_style,
                    &theme.editor_selection_style(),
                    SelectionForegroundPriority::Content,
                );
            } else if cursor {
                let cursor_style = Style {
                    bg: theme
                        .line_highlight_style
                        .as_ref()
                        .and_then(|style| style.bg)
                        .or(theme.ui_style.picker_selected_item.bg),
                    ..Style::default()
                };
                line_style = theme.selected_style(
                    &line_style,
                    &cursor_style,
                    SelectionForegroundPriority::Content,
                );
            }
            buffer.set_text(x, y, &fit_display_width("", width), &line_style);
            if segment_index == 0 {
                let marker = match line.kind.as_str() {
                    "added" => "+",
                    "removed" => "−",
                    "hunk" => "@",
                    _ => " ",
                };
                let gutter = format!(
                    "{:>number_width$} {:>number_width$} {marker} ",
                    line.old_line.map_or(String::new(), |line| line.to_string()),
                    line.new_line.map_or(String::new(), |line| line.to_string()),
                );
                let mut gutter_style = line_style.clone();
                gutter_style.fg = match line.kind.as_str() {
                    "added" => Some(palette.added_marker),
                    "removed" => Some(palette.removed_marker),
                    _ => gutter_style.fg,
                };
                buffer.set_text(
                    x,
                    y,
                    &truncate_display_width(&gutter, gutter_width),
                    &gutter_style,
                );
            }
            let code_x = x + gutter_width;
            buffer.set_text(
                code_x,
                y,
                &fit_display_width(&segment.text, code_width),
                &line_style,
            );
            render_syntax_overlays(
                buffer,
                (code_x, y, code_width),
                &line.text,
                &segment,
                workspace
                    .detail_highlights
                    .get(line_index)
                    .map_or(&[], Vec::as_slice),
                &line_style,
            );
            if !selected && !cursor {
                let background = match line.kind.as_str() {
                    "added" => Some(palette.added_text),
                    "removed" => Some(palette.removed_text),
                    _ => None,
                };
                if let Some(background) = background {
                    for range in workspace
                        .detail_word_changes
                        .get(line_index)
                        .into_iter()
                        .flatten()
                    {
                        let start = range.start.max(segment.byte_start).min(segment.byte_end);
                        let end = range.end.min(segment.byte_end).max(start);
                        if start < end {
                            let first = display_width(&line.text[segment.byte_start..start]);
                            let last = display_width(&line.text[segment.byte_start..end]);
                            for column in first..last.min(code_width) {
                                buffer.set_bg(code_x + column, y, &background, theme);
                            }
                        }
                    }
                }
            }
            screen_row += 1;
        }
    }
}

#[derive(Debug)]
struct DisplaySlice {
    text: String,
    byte_start: usize,
    byte_end: usize,
}

fn wrapped_slices(text: &str, width: usize) -> Vec<DisplaySlice> {
    if text.is_empty() {
        return vec![DisplaySlice {
            text: String::new(),
            byte_start: 0,
            byte_end: 0,
        }];
    }
    let mut result = Vec::new();
    let mut column = 0;
    while column < display_width(text) {
        let slice = display_slice(text, column, width.max(1));
        if slice.byte_start == slice.byte_end {
            break;
        }
        column += display_width(&slice.text).max(1);
        result.push(slice);
    }
    result
}

fn display_slice(text: &str, start_column: usize, width: usize) -> DisplaySlice {
    let mut column = 0;
    let mut byte_start = text.len();
    let mut byte_end = text.len();
    let mut output = String::new();
    for (start, grapheme) in text.grapheme_indices(true) {
        let grapheme_width = display_width(grapheme);
        let end_column = column + grapheme_width;
        if end_column <= start_column {
            column = end_column;
            continue;
        }
        if byte_start == text.len() {
            byte_start = start;
        }
        if display_width(&output) + grapheme_width > width {
            break;
        }
        output.push_str(grapheme);
        byte_end = start + grapheme.len();
        column = end_column;
    }
    if byte_start == text.len() {
        byte_start = text.len();
        byte_end = text.len();
    }
    DisplaySlice {
        text: output,
        byte_start,
        byte_end,
    }
}

fn render_syntax_overlays(
    buffer: &mut RenderBuffer,
    rect: (usize, usize, usize),
    text: &str,
    visible: &DisplaySlice,
    spans: &[crate::editor::StyleInfo],
    line_style: &Style,
) {
    let (x, y, width) = rect;
    for span in spans {
        let start = span.start.max(visible.byte_start).min(visible.byte_end);
        let end = span.end.min(visible.byte_end).max(start);
        if start >= end || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            continue;
        }
        let offset = display_width(&text[visible.byte_start..start]);
        if offset >= width {
            continue;
        }
        let highlighted = truncate_display_width(&text[start..end], width - offset);
        let style = span.style.with_bg(line_style.bg);
        buffer.set_text(x + offset, y, &highlighted, &style);
    }
}

fn diff_line_style(kind: &str, theme: &Theme, palette: &DiffPalette) -> Style {
    match kind {
        "added" => palette.added.clone(),
        "removed" => palette.removed.clone(),
        "hunk" => palette.hunk.clone(),
        _ => theme.style.clone(),
    }
}

fn render_segments(
    buffer: &mut RenderBuffer,
    rect: (usize, usize, usize),
    segments: &[PanelSegment],
    editor_style: &Style,
    theme: &Theme,
    selected: bool,
) {
    let (mut x, y, width) = rect;
    let end = x.saturating_add(width).min(buffer.width);
    for segment in segments {
        if x >= end {
            break;
        }
        let text = truncate_display_width(&segment.text, end - x);
        let mut style = segment
            .semantic
            .as_ref()
            .map(|spec| theme.resolve_style(spec))
            .or_else(|| segment.style.clone())
            .unwrap_or_else(|| editor_style.clone())
            .with_bg(editor_style.bg);
        if selected {
            // Segment styles commonly carry the editor background so they can
            // render correctly on an unselected row. The selected row fill
            // must take precedence, otherwise each text segment punches a
            // dark hole through the selection background.
            style.bg = editor_style.bg;
            style = theme.ensure_text_contrast(&style);
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

    fn document() -> WorkspaceDocument {
        WorkspaceDocument {
            path: "src/main.rs".to_string(),
            lines: vec![
                WorkspaceDocumentLine {
                    id: "h1-header".to_string(),
                    text: "@@ -1 +1 @@".to_string(),
                    kind: "hunk".to_string(),
                    hunk_id: Some("h1".to_string()),
                    ..WorkspaceDocumentLine::default()
                },
                WorkspaceDocumentLine {
                    id: "h1-add".to_string(),
                    text: "let first = true;".to_string(),
                    kind: "added".to_string(),
                    new_line: Some(1),
                    hunk_id: Some("h1".to_string()),
                    ..WorkspaceDocumentLine::default()
                },
                WorkspaceDocumentLine {
                    id: "h2-header".to_string(),
                    text: "@@ -8 +8 @@".to_string(),
                    kind: "hunk".to_string(),
                    hunk_id: Some("h2".to_string()),
                    ..WorkspaceDocumentLine::default()
                },
                WorkspaceDocumentLine {
                    id: "h2-remove".to_string(),
                    text: "let second = false;".to_string(),
                    kind: "removed".to_string(),
                    old_line: Some(8),
                    hunk_id: Some("h2".to_string()),
                    ..WorkspaceDocumentLine::default()
                },
            ],
            ..WorkspaceDocument::default()
        }
    }

    fn model_with_document() -> WorkspaceModel {
        WorkspaceModel {
            rows: vec![row("file", true)],
            detail_document: Some(document()),
            ..WorkspaceModel::default()
        }
    }

    fn action_model() -> WorkspaceModel {
        let mut model = model_with_document();
        model.actions = vec![WorkspaceAction {
            hint: UiAction::new("s", "s", "stage {scope}"),
            focus: String::new(),
            sections: vec!["unstaged".to_string()],
            selection: String::new(),
            change_only: true,
            hunk_only: false,
        }];
        model.rows[0].path = Some("src/main.rs".to_string());
        model.rows[0].data = serde_json::json!({"section":"unstaged"});
        for line in &mut model.detail_document.as_mut().unwrap().lines {
            line.data = serde_json::json!({"section":"unstaged"});
        }
        model
    }

    #[test]
    fn word_highlights_cover_only_replaced_words() {
        let mut document = WorkspaceDocument::default();
        for (kind, text) in [
            ("removed", "let count = 1;"),
            ("added", "let count = 4;"),
            ("context", "unchanged"),
            ("added", "brand new line"),
        ] {
            document.lines.push(WorkspaceDocumentLine {
                kind: kind.to_string(),
                text: text.to_string(),
                ..Default::default()
            });
        }
        let ranges = word_changes(Some(&document));
        let changed = |index: usize| {
            ranges[index]
                .iter()
                .map(|range| &document.lines[index].text[range.clone()])
                .collect::<String>()
        };
        assert_eq!(changed(0), "1");
        assert_eq!(changed(1), "4");
        assert!(ranges[2].is_empty() && ranges[3].is_empty());
    }

    #[test]
    fn metadata_folding_preserves_patch_indices_and_local_action_scope() {
        let mut model = action_model();
        model.detail_document.as_mut().unwrap().lines.insert(
            0,
            WorkspaceDocumentLine {
                id: "meta".to_string(),
                kind: "meta".to_string(),
                text: "diff --git a/b b/b".to_string(),
                ..Default::default()
            },
        );
        let mut workspace = PluginWorkspace::new(
            "git".to_string(),
            WorkspaceConfig {
                notify_detail_navigation: false,
                ..Default::default()
            },
        );
        workspace.update(model, &Theme::default());
        assert!(!workspace.detail_visible.contains(&0));
        assert!(workspace
            .actions()
            .iter()
            .any(|action| action.label == "stage file"));
        workspace.handle_action("toggle".to_string(), 24, 120);
        assert_eq!(workspace.detail_cursor, 2);
        assert!(workspace
            .actions()
            .iter()
            .any(|action| action.label == "stage line"));
        workspace.handle_action("visual".to_string(), 24, 120);
        workspace.handle_action("down".to_string(), 24, 120);
        assert!(workspace
            .actions()
            .iter()
            .any(|action| action.label == "stage 2 lines"));
        let event = workspace.handle_action("M".to_string(), 24, 120);
        assert!(!event.notify_plugin);
        assert!(workspace.detail_visible.contains(&0));
        assert_eq!(event.detail_selection, Some([2, 3]));
    }

    #[test]
    fn filter_collapse_and_width_survive_model_updates() {
        let mut model = action_model();
        let mut heading = row("section:unstaged", false);
        heading.segments.push(PanelSegment {
            text: "Unstaged".to_string(),
            style: None,
            semantic: None,
        });
        model.rows.insert(0, heading);
        let mut other = row("other", true);
        other.path = Some("docs/readme.md".to_string());
        model.rows.push(other);
        let mut manager = WorkspaceManager::default();
        manager.open("git".to_string(), WorkspaceConfig::default());
        manager.update("git", model.clone(), &Theme::default());
        manager.handle_action("/".to_string(), 24, 120);
        manager.handle_action("filter_text:readme".to_string(), 24, 120);
        let event = manager
            .handle_action("filter_accept".to_string(), 24, 120)
            .unwrap();
        assert_eq!(event.row.unwrap().id, "other");
        assert_eq!(manager.active.as_ref().unwrap().rows_visible, vec![0, 2]);
        manager.handle_action("/".to_string(), 24, 120);
        manager.handle_action("filter_cancel".to_string(), 24, 120);
        manager.handle_action("C".to_string(), 24, 120);
        assert_eq!(manager.active.as_ref().unwrap().rows_visible, vec![0]);
        manager.update("git", model, &Theme::default());
        assert_eq!(manager.active.as_ref().unwrap().rows_visible, vec![0]);
        manager.handle_action("widen_rows".to_string(), 24, 120);
        let width = manager.active.as_ref().unwrap().rows_width;
        manager.close("git");
        manager.open("git".to_string(), WorkspaceConfig::default());
        assert_eq!(manager.active.as_ref().unwrap().rows_width, width);
    }

    #[test]
    fn reused_patch_index_does_not_restore_an_unrelated_line() {
        let mut workspace = PluginWorkspace::new("git".to_string(), WorkspaceConfig::default());
        let mut model = action_model();
        workspace.update(model.clone(), &Theme::default());
        workspace.detail_cursor = 3;
        workspace.detail_selection_anchor = Some(1);
        model.detail_document.as_mut().unwrap().lines[3].text = "different change".to_string();
        workspace.update(model, &Theme::default());
        assert_eq!(workspace.detail_cursor, 1);
        assert!(workspace.detail_selection_anchor.is_none());
    }

    #[test]
    fn one_dark_plain_text_and_syntax_keep_the_owning_background() {
        let theme = crate::theme::parse_vscode_theme("themes/one-dark-pro.json").unwrap();
        let mut model = model_with_document();
        model.header = vec![PanelSegment {
            text: "branch".to_string(),
            style: Some(theme.ui_style.popup_title.clone()),
            semantic: None,
        }];
        let mut manager = WorkspaceManager::default();
        manager.open("git".to_string(), WorkspaceConfig::default());
        manager.update("git", model, &theme);
        let mut buffer = RenderBuffer::new(120, 24, &theme.style);
        manager.render(&mut buffer, &theme, PickerIconsConfig::default());
        assert!(buffer.cells[120..240]
            .iter()
            .all(|cell| cell.style.bg == theme.style.bg));
        let added = buffer
            .cells
            .chunks(120)
            .find(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
                    .contains("let first = true")
            })
            .unwrap();
        let palette = DiffPalette::new(&theme);
        let detail_start = WorkspaceLayout::calculate(manager.active.as_ref().unwrap(), 24, 120)
            .detail
            .unwrap()
            .x;
        assert!(added[detail_start..]
            .iter()
            .all(|cell| cell.style.bg == palette.added.bg));
    }

    fn buffer_text(buffer: &RenderBuffer) -> String {
        buffer
            .cells
            .chunks(buffer.width)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn row(id: &str, selectable: bool) -> WorkspaceRow {
        WorkspaceRow {
            id: id.to_string(),
            selectable,
            depth: 0,
            path: None,
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
            &Theme::default(),
        );
        let event = manager.handle_action("down".to_string(), 20, 100).unwrap();
        assert_eq!(event.row.unwrap().id, "b");
        manager.update(
            "git",
            WorkspaceModel {
                rows: vec![row("b", true), row("c", true)],
                ..WorkspaceModel::default()
            },
            &Theme::default(),
        );
        let event = manager.handle_action("noop".to_string(), 20, 100).unwrap();
        assert_eq!(event.row.unwrap().id, "b");
    }

    #[test]
    fn detail_focus_supports_tab_ctrl_w_hunks_ranges_and_wrap_toggle() {
        let mut manager = WorkspaceManager::default();
        manager.open("git".to_string(), WorkspaceConfig::default());
        manager.update("git", model_with_document(), &Theme::default());

        let event = manager
            .handle_action("toggle".to_string(), 20, 100)
            .unwrap();
        assert_eq!(event.focus, WorkspaceFocus::Detail);
        assert!(event.detail_wrap);

        let event = manager.handle_action("]".to_string(), 20, 100).unwrap();
        assert_eq!(event.action, "prefix");
        let event = manager.handle_action("h".to_string(), 20, 100).unwrap();
        assert_eq!(event.action, "next_hunk");
        assert_eq!(event.detail_line.unwrap().hunk_id.as_deref(), Some("h2"));

        manager.handle_action("visual".to_string(), 20, 100);
        let event = manager.handle_action("up".to_string(), 20, 100).unwrap();
        assert_eq!(event.detail_selection, Some([1, 2]));

        let event = manager
            .handle_action("toggle_wrap".to_string(), 20, 100)
            .unwrap();
        assert!(!event.detail_wrap);
        manager.handle_action("ctrl_w".to_string(), 20, 100);
        let event = manager.handle_action("h".to_string(), 20, 100).unwrap();
        assert_eq!(event.action, "focus_rows");
        assert_eq!(event.focus, WorkspaceFocus::Rows);
    }

    #[test]
    fn core_owned_detail_navigation_skips_plugin_callbacks_but_keeps_operations() {
        let mut manager = WorkspaceManager::default();
        manager.open(
            "git".to_string(),
            WorkspaceConfig {
                notify_detail_navigation: false,
                ..WorkspaceConfig::default()
            },
        );
        manager.update("git", model_with_document(), &Theme::default());

        let focus = manager
            .handle_action("toggle".to_string(), 20, 100)
            .unwrap();
        assert!(!focus.notify_plugin);
        let movement = manager.handle_action("down".to_string(), 20, 100).unwrap();
        assert!(!movement.notify_plugin);
        assert!(
            manager
                .handle_action("s".to_string(), 20, 100)
                .unwrap()
                .notify_plugin
        );

        manager.handle_action("ctrl_w".to_string(), 20, 100);
        manager.handle_action("h".to_string(), 20, 100);
        assert!(
            manager
                .handle_action("down".to_string(), 20, 100)
                .unwrap()
                .notify_plugin
        );
        assert!(serde_json::to_value(movement)
            .unwrap()
            .get("notify_plugin")
            .is_none());
    }

    #[test]
    fn narrow_workspace_switches_between_full_width_rows_and_diff() {
        let theme = Theme::default();
        let mut manager = WorkspaceManager::default();
        manager.open("git".to_string(), WorkspaceConfig::default());
        manager.update("git", model_with_document(), &theme);
        let mut buffer = RenderBuffer::new(80, 12, &theme.style);

        manager.render(&mut buffer, &theme, PickerIconsConfig::default());
        let rows = buffer_text(&buffer);
        assert!(rows.contains("Changes"));
        assert!(!rows.contains("let first = true"));

        manager.handle_action("toggle".to_string(), 12, 80);
        manager.render(&mut buffer, &theme, PickerIconsConfig::default());
        let detail = buffer_text(&buffer);
        assert!(detail.contains("src/main.rs"));
        assert!(detail.contains("wrap"));
        assert!(detail.contains("let first = true"));
    }

    #[test]
    fn workspace_zoom_restores_responsive_layout_and_preferences() {
        let theme = Theme::default();
        let mut workspace = PluginWorkspace::new(
            "git".to_string(),
            WorkspaceConfig {
                notify_detail_navigation: false,
                ..WorkspaceConfig::default()
            },
        );
        workspace.update(model_with_document(), &theme);
        workspace.rows_width = Some(31);
        for (width, height) in [(120, 28), (80, 24), (60, 12)] {
            for focus in [WorkspaceFocus::Rows, WorkspaceFocus::Detail] {
                workspace.focus = focus;
                let normal = WorkspaceLayout::calculate(&workspace, height, width);
                workspace.handle_action("ctrl_w".to_string(), height, width);
                let event = workspace.handle_action("z".to_string(), height, width);
                assert_eq!(event.action, "toggle_zoom");
                assert!(!event.notify_plugin);
                let zoomed = WorkspaceLayout::calculate(&workspace, height, width);
                assert_eq!(zoomed.pane(focus).unwrap().width, width);
                assert_eq!(zoomed.pane(focus).unwrap().height, height - 3);
                assert!(zoomed.separator.is_none());
                assert_eq!(workspace.rows_width, Some(31));
                workspace.handle_action("toggle_zoom".to_string(), height, width);
                assert_eq!(
                    WorkspaceLayout::calculate(&workspace, height, width),
                    normal
                );
            }
        }
        workspace.rows_hidden = true;
        workspace.focus = WorkspaceFocus::Detail;
        workspace.toggle_zoom();
        workspace.toggle_zoom();
        assert!(workspace.rows_hidden);
        workspace.toggle_zoom();
        workspace.handle_action("focus_rows".to_string(), 24, 120);
        assert_eq!(workspace.zoomed, None);
        assert_eq!(workspace.focus, WorkspaceFocus::Rows);
        workspace.focus = WorkspaceFocus::Detail;
        workspace.toggle_zoom();
        workspace.update(WorkspaceModel::default(), &theme);
        assert_eq!(workspace.zoomed, None);
    }

    #[test]
    fn responsive_layout_uses_columns_stacking_and_focused_fallback() {
        let theme = Theme::default();
        let mut workspace = PluginWorkspace::new("git".to_string(), WorkspaceConfig::default());
        workspace.update(model_with_document(), &theme);

        let columns = WorkspaceLayout::calculate(&workspace, 24, 100);
        assert_eq!(columns.mode, WorkspaceLayoutMode::Columns);
        assert!(columns.rows.is_some_and(|rect| rect.width < 100));
        assert!(columns.detail.is_some_and(|rect| rect.width < 100));
        assert!(matches!(
            columns.separator,
            Some(WorkspaceSeparator::Columns(_))
        ));

        let stacked = WorkspaceLayout::calculate(&workspace, 24, 99);
        assert_eq!(stacked.mode, WorkspaceLayoutMode::Stacked);
        assert_eq!(stacked.rows.map(|rect| rect.width), Some(99));
        assert_eq!(stacked.detail.map(|rect| rect.width), Some(99));
        assert!(stacked.rows.unwrap().y < stacked.detail.unwrap().y);
        assert!(matches!(
            stacked.separator,
            Some(WorkspaceSeparator::Stacked(_))
        ));

        let focused = WorkspaceLayout::calculate(&workspace, 15, 80);
        assert_eq!(focused.mode, WorkspaceLayoutMode::Focused);
        assert!(focused.rows.is_some());
        assert!(focused.detail.is_none());
    }

    #[test]
    fn stacked_workspace_renders_both_panes_and_mouse_focuses_each() {
        let theme = Theme::default();
        let mut manager = WorkspaceManager::default();
        manager.open("git".to_string(), WorkspaceConfig::default());
        manager.update(
            "git",
            WorkspaceModel {
                rows: vec![row("first", true), row("second", true)],
                detail_document: Some(document()),
                ..WorkspaceModel::default()
            },
            &theme,
        );
        let mut buffer = RenderBuffer::new(80, 24, &theme.style);

        manager.render(&mut buffer, &theme, PickerIconsConfig::default());

        let rendered = buffer_text(&buffer);
        assert!(rendered.contains("Changes"));
        assert!(rendered.contains("src/main.rs"));
        assert!(rendered.contains("wrap"));
        assert!(rendered.contains("let first = true"));
        let layout = WorkspaceLayout::calculate(manager.active.as_ref().unwrap(), 24, 80);
        let rows = layout.rows.unwrap();
        let detail = layout.detail.unwrap();
        let separator = match layout.separator.unwrap() {
            WorkspaceSeparator::Stacked(rect) => rect,
            WorkspaceSeparator::Columns(_) => panic!("expected a stacked separator"),
        };
        assert_eq!(buffer.cells[separator.y * buffer.width].text, "─");

        let event = manager
            .handle_mouse("mouse_click", 1, rows.y + 2, 24, 80)
            .unwrap();
        assert_eq!(event.focus, WorkspaceFocus::Rows);
        assert_eq!(event.row.unwrap().id, "second");

        let event = manager
            .handle_mouse("mouse_click", 20, detail.y + 5, 24, 80)
            .unwrap();
        assert_eq!(event.focus, WorkspaceFocus::Detail);
        assert_eq!(event.detail_index, 3);
    }

    #[test]
    fn passive_detail_preview_keeps_actions_focused_on_the_selected_row() {
        let theme = Theme::default();
        let mut manager = WorkspaceManager::default();
        manager.open("git".to_string(), WorkspaceConfig::default());
        manager.update(
            "git",
            WorkspaceModel {
                rows: vec![row("untracked:new-file.txt", true)],
                detail: vec![vec![PanelSegment {
                    text: "No textual diff available.".to_string(),
                    style: None,
                    semantic: None,
                }]],
                ..WorkspaceModel::default()
            },
            &theme,
        );

        let layout = WorkspaceLayout::calculate(manager.active.as_ref().unwrap(), 24, 100);
        let detail = layout
            .detail
            .expect("wide workspace should show the preview pane");
        let click = manager
            .handle_mouse("mouse_click", detail.x + 2, detail.y + 1, 24, 100)
            .unwrap();
        assert_eq!(click.focus, WorkspaceFocus::Rows);

        let stage = manager.handle_action("s".to_string(), 24, 100).unwrap();
        assert_eq!(stage.focus, WorkspaceFocus::Rows);
        assert_eq!(stage.row.unwrap().id, "untracked:new-file.txt");
        assert!(stage.detail_line.is_none());
    }

    #[test]
    fn losing_the_detail_document_returns_focus_to_rows() {
        let mut manager = WorkspaceManager::default();
        manager.open("git".to_string(), WorkspaceConfig::default());
        manager.update("git", model_with_document(), &Theme::default());
        assert_eq!(
            manager
                .handle_action("toggle".to_string(), 20, 100)
                .unwrap()
                .focus,
            WorkspaceFocus::Detail
        );

        manager.update(
            "git",
            WorkspaceModel {
                rows: vec![row("file", true)],
                detail: vec![vec![]],
                ..WorkspaceModel::default()
            },
            &Theme::default(),
        );

        assert_eq!(
            manager
                .handle_action("s".to_string(), 20, 100)
                .unwrap()
                .focus,
            WorkspaceFocus::Rows
        );
    }

    #[test]
    fn selected_row_background_wins_over_segment_backgrounds() {
        let theme = crate::theme::parse_vscode_theme("src/fixtures/mocha.json").unwrap();
        let selected_background = theme
            .selected_style(
                &theme.style,
                &theme.list_selection_style(),
                SelectionForegroundPriority::Selection,
            )
            .bg;
        let mut selected_row = row("file", true);
        selected_row.segments = vec![
            PanelSegment {
                text: "main.rs".to_string(),
                style: Some(theme.style.clone()),
                semantic: None,
            },
            PanelSegment {
                text: " src".to_string(),
                style: Some(Style {
                    fg: selected_background,
                    bg: theme.style.bg,
                    ..Style::default()
                }),
                semantic: None,
            },
        ];
        selected_row.right_segments = vec![PanelSegment {
            text: "~".to_string(),
            style: Some(theme.style.clone()),
            semantic: None,
        }];
        let mut manager = WorkspaceManager::default();
        manager.open("git".to_string(), WorkspaceConfig::default());
        manager.update(
            "git",
            WorkspaceModel {
                rows: vec![selected_row],
                ..WorkspaceModel::default()
            },
            &theme,
        );
        let mut buffer = RenderBuffer::new(100, 12, &theme.style);

        manager.render(&mut buffer, &theme, PickerIconsConfig::default());

        let expected = selected_background;
        let selected_screen_row = 3;
        assert_eq!(
            buffer.cells[selected_screen_row * buffer.width + 1]
                .style
                .bg,
            expected
        );
        assert_eq!(
            buffer.cells[selected_screen_row * buffer.width + 38]
                .style
                .bg,
            expected
        );
        let path_cell = &buffer.cells[selected_screen_row * buffer.width + 8];
        assert!(
            crate::color::contrast_ratio(path_cell.style.fg.unwrap(), path_cell.style.bg.unwrap())
                >= crate::theme::MINIMUM_SELECTION_TEXT_CONTRAST
        );
    }

    #[test]
    fn diff_lines_compose_theme_backgrounds_with_syntax_foregrounds() {
        let theme = crate::theme::parse_vscode_theme("src/fixtures/mocha.json").unwrap();
        let mut manager = WorkspaceManager::default();
        manager.open("git".to_string(), WorkspaceConfig::default());
        manager.update("git", model_with_document(), &theme);
        manager.handle_action("toggle".to_string(), 12, 80);
        manager.handle_action("down".to_string(), 12, 80);
        let mut buffer = RenderBuffer::new(80, 12, &theme.style);

        manager.render(&mut buffer, &theme, PickerIconsConfig::default());

        let added_row = buffer
            .cells
            .chunks(buffer.width)
            .find(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
                    .contains("let first = true")
            })
            .unwrap();
        let expected_background = DiffPalette::new(&theme).added.bg;
        assert!(added_row
            .iter()
            .any(|cell| cell.style.bg == expected_background));
        assert!(added_row
            .iter()
            .any(|cell| cell.style.fg.is_some() && cell.style.fg != theme.style.fg));
    }
}
