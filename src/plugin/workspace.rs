//! Full-screen plugin workspace models and selection state.
//!
//! A [`WorkspaceModel`] is the plugin-owned snapshot of rows, sections, actions, and
//! detail content. [`WorkspaceManager`] owns focus and the currently selected row while
//! replacing models by stable workspace ID. Selection restoration is ID-based so
//! reordering rows does not silently move focus to unrelated content.

use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    color::Color,
    config::PickerIconsConfig,
    editor::render_buffer::RenderBuffer,
    highlighter::Highlighter,
    theme::{SelectionForegroundPriority, Style, Theme},
    ui::{picker_file_icon, picker_file_icon_color},
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
    /// Whether structured detail documents wrap long lines initially.
    #[serde(default = "default_detail_wrap")]
    pub detail_wrap: bool,
}

fn default_detail_ratio() -> u8 {
    55
}

fn default_min_two_pane_width() -> usize {
    100
}

fn default_detail_wrap() -> bool {
    true
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            title: String::new(),
            detail_ratio: default_detail_ratio(),
            min_two_pane_width: default_min_two_pane_width(),
            detail_wrap: default_detail_wrap(),
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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct WorkspaceDocument {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub lines: Vec<WorkspaceDocumentLine>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
}

#[derive(Debug)]
pub struct PluginWorkspace {
    id: String,
    config: WorkspaceConfig,
    model: WorkspaceModel,
    selected: usize,
    scroll: usize,
    focus: WorkspaceFocus,
    detail_cursor: usize,
    detail_scroll: usize,
    detail_horizontal: usize,
    detail_wrap: bool,
    detail_selection_anchor: Option<usize>,
    key_prefix: Option<String>,
    detail_highlights: Vec<Vec<crate::editor::StyleInfo>>,
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
            detail_cursor: 0,
            detail_scroll: 0,
            detail_horizontal: 0,
            detail_wrap,
            detail_selection_anchor: None,
            key_prefix: None,
            detail_highlights: Vec::new(),
        }
    }

    fn update(&mut self, model: WorkspaceModel, theme: &Theme) {
        let selected_id = self
            .model
            .rows
            .get(self.selected)
            .map(|row| row.id.as_str());
        let selected = selected_id
            .and_then(|id| model.rows.iter().position(|row| row.id == id))
            .or_else(|| model.rows.iter().position(|row| row.selectable))
            .unwrap_or(0);
        let detail_id = self
            .model
            .detail_document
            .as_ref()
            .and_then(|document| document.lines.get(self.detail_cursor))
            .map(|line| line.id.as_str());
        let restored_detail = detail_id.and_then(|id| {
            model
                .detail_document
                .as_ref()?
                .lines
                .iter()
                .position(|line| line.id == id)
        });
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
        self.detail_highlights = highlight_document(model.detail_document.as_ref(), theme);
        self.model = model;
        self.selected = selected;
        self.scroll = self.scroll.min(self.selected);
        self.detail_scroll = self.detail_scroll.min(self.detail_cursor);
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
        WorkspaceEvent {
            workspace_id: self.id.clone(),
            action,
            selected_index: self.selected,
            row: self.model.rows.get(self.selected).cloned(),
            focus: self.focus,
            detail_index: self.detail_cursor,
            detail_line,
            detail_selection,
            detail_wrap: self.detail_wrap,
        }
    }

    fn detail_len(&self) -> usize {
        self.model
            .detail_document
            .as_ref()
            .map_or(0, |document| document.lines.len())
    }

    fn detail_line_at_visual_offset(&self, offset: usize, code_width: usize) -> usize {
        let Some(document) = self.model.detail_document.as_ref() else {
            return 0;
        };
        let mut remaining = offset;
        for (index, line) in document.lines.iter().enumerate().skip(self.detail_scroll) {
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
        let len = self.detail_len();
        if len == 0 {
            return;
        }
        self.detail_cursor = self
            .detail_cursor
            .saturating_add_signed(delta)
            .min(len.saturating_sub(1));
        if self.detail_cursor < self.detail_scroll {
            self.detail_scroll = self.detail_cursor;
        } else if self.detail_cursor >= self.detail_scroll + visible_rows.max(1) {
            self.detail_scroll = self
                .detail_cursor
                .saturating_sub(visible_rows.saturating_sub(1));
        }
    }

    fn ensure_detail_cursor_visible(&mut self, height: usize, width: usize) {
        let visible_rows = height.saturating_sub(5).max(1);
        if self.detail_cursor < self.detail_scroll {
            self.detail_scroll = self.detail_cursor;
            return;
        }
        let two_pane = width >= self.config.min_two_pane_width;
        let left_width = if two_pane {
            width.saturating_mul(100usize.saturating_sub(self.config.detail_ratio as usize)) / 100
        } else {
            0
        };
        let code_width = width.saturating_sub(left_width).saturating_sub(15).max(1);
        let Some(document) = self.model.detail_document.as_ref() else {
            return;
        };
        let occupied = document
            .lines
            .iter()
            .skip(self.detail_scroll)
            .take(self.detail_cursor.saturating_sub(self.detail_scroll) + 1)
            .map(|line| {
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
            self.move_detail(target as isize - self.detail_cursor as isize, visible_rows);
        }
    }

    fn handle_action(&mut self, mut action: String, height: usize, width: usize) -> WorkspaceEvent {
        let visible_rows = height.saturating_sub(5).max(1);

        if let Some(prefix) = self.key_prefix.take() {
            action = match (prefix.as_str(), action.as_str()) {
                ("ctrl_w", "w" | "ctrl_w") => "focus_next",
                ("ctrl_w", "W" | "p") => "focus_previous",
                ("ctrl_w", "h") => "focus_rows",
                ("ctrl_w", "l") => "focus_detail",
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
                "v" => "visual",
                other => other,
            }
            .to_string();
        }

        match action.as_str() {
            "toggle" | "back_toggle" | "focus_next" | "focus_previous" => {
                if self.model.detail_document.is_some() {
                    self.focus = match self.focus {
                        WorkspaceFocus::Rows => WorkspaceFocus::Detail,
                        WorkspaceFocus::Detail => WorkspaceFocus::Rows,
                    };
                }
            }
            "focus_rows" => self.focus = WorkspaceFocus::Rows,
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
                self.detail_horizontal = max_width.saturating_sub(width / 2);
            }
            _ => {}
        }
        if self.focus == WorkspaceFocus::Detail {
            self.ensure_detail_cursor_visible(height, width);
        }
        self.event(action)
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

    pub fn update(&mut self, id: &str, model: WorkspaceModel, theme: &Theme) -> bool {
        let Some(workspace) = self.active.as_mut().filter(|workspace| workspace.id == id) else {
            return false;
        };
        workspace.update(model, theme);
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
        let two_pane = width >= workspace.config.min_two_pane_width;
        let left_width = if two_pane {
            width.saturating_mul(100usize.saturating_sub(workspace.config.detail_ratio as usize))
                / 100
        } else {
            width
        }
        .max(20)
        .min(width);
        if two_pane {
            workspace.focus = if column <= left_width {
                WorkspaceFocus::Rows
            } else {
                WorkspaceFocus::Detail
            };
        }
        let visible_rows = height.saturating_sub(5).max(1);
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
            "mouse_click" if row >= 3 => {
                let offset = row - 3;
                match workspace.focus {
                    WorkspaceFocus::Rows => {
                        let candidate = workspace.scroll.saturating_add(offset);
                        if workspace
                            .model
                            .rows
                            .get(candidate)
                            .is_some_and(|row| row.selectable)
                        {
                            workspace.selected = candidate;
                        }
                    }
                    WorkspaceFocus::Detail => {
                        let detail_x = if two_pane { left_width + 1 } else { 0 };
                        let detail_width = width.saturating_sub(detail_x);
                        let code_width = detail_width.saturating_sub(14).max(1);
                        workspace.detail_cursor =
                            workspace.detail_line_at_visual_offset(offset, code_width);
                    }
                }
            }
            _ => {}
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

        let title = format!(" {} ", workspace.config.title);
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
        let show_rows = two_pane || workspace.focus == WorkspaceFocus::Rows;
        let show_detail = two_pane || workspace.focus == WorkspaceFocus::Detail;

        if show_rows {
            render_row_pane(
                buffer,
                workspace,
                theme,
                icons,
                (0, left_width, body_top, body_height),
            );
        }

        if two_pane && left_width < buffer.width {
            for y in body_top..buffer.height.saturating_sub(1) {
                buffer.set_text(left_width, y, "│", editor_style);
            }
        }
        if show_detail {
            let detail_x = if two_pane { left_width + 1 } else { 0 };
            let detail_width = buffer.width.saturating_sub(detail_x);
            render_detail_pane(
                buffer,
                workspace,
                theme,
                detail_x,
                detail_width,
                body_top,
                body_height,
            );
        }

        render_segments(
            buffer,
            (1, buffer.height - 1, buffer.width - 2),
            &workspace.model.footer,
            editor_style,
            theme,
            false,
        );
    }
}

fn highlight_document(
    document: Option<&WorkspaceDocument>,
    theme: &Theme,
) -> Vec<Vec<crate::editor::StyleInfo>> {
    let Some(document) = document else {
        return Vec::new();
    };
    let Some(mut highlighter) = Highlighter::new(theme).ok() else {
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
    let mut title_style = theme.ui_style.popup_title.clone();
    title_style.bold = active;
    buffer.set_text(
        x + 1,
        top,
        if active { "› Changes" } else { "  Changes" },
        &title_style,
    );
    let content_top = top + 1;
    let content_height = height.saturating_sub(1);
    for (screen_row, row) in workspace
        .model
        .rows
        .iter()
        .skip(workspace.scroll)
        .take(content_height)
        .enumerate()
    {
        let y = content_top + screen_row;
        let selected = workspace.scroll + screen_row == workspace.selected && row.selectable;
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
        if let Some(path) = row.path.as_deref() {
            let icon = picker_file_icon(path, icons.style);
            if !icon.is_empty() {
                let mut icon_style = row_style.clone();
                if icons.color {
                    icon_style.fg = picker_file_icon_color(path).or(icon_style.fg);
                }
                if selected {
                    icon_style = theme.ensure_text_contrast(&icon_style);
                }
                buffer.set_text(content_x, y, &fit_display_width(icon, 2), &icon_style);
                content_x += 3;
            }
        }
        render_segments(
            buffer,
            (
                content_x,
                y,
                width.saturating_sub(content_x.saturating_sub(x) + 1),
            ),
            &row.segments,
            &row_style,
            theme,
            selected,
        );
        let right_width = row
            .right_segments
            .iter()
            .map(|segment| display_width(&segment.text))
            .sum::<usize>();
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
    let mut title_style = theme.ui_style.popup_title.clone();
    title_style.bold = active;
    let wrap_label = if workspace.detail_wrap {
        "wrap"
    } else {
        "nowrap"
    };
    let title = if active {
        format!(" › Diff  {wrap_label}")
    } else {
        format!("   Diff  {wrap_label}")
    };
    buffer.set_text(x, top, &truncate_display_width(&title, width), &title_style);
    let content_top = top + 1;
    let content_height = height.saturating_sub(1);
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

    let gutter_width = 13.min(width.saturating_sub(1));
    let code_width = width.saturating_sub(gutter_width + 1).max(1);
    let selection = workspace.detail_selection_anchor.map(|anchor| {
        (
            anchor.min(workspace.detail_cursor),
            anchor.max(workspace.detail_cursor),
        )
    });
    let mut screen_row = 0;
    for (line_index, line) in document
        .lines
        .iter()
        .enumerate()
        .skip(workspace.detail_scroll)
    {
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
            let mut line_style = diff_line_style(&line.kind, theme);
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
                    "{:>4} {:>4} {marker} ",
                    line.old_line.map_or(String::new(), |line| line.to_string()),
                    line.new_line.map_or(String::new(), |line| line.to_string()),
                );
                let mut gutter_style = line_style.clone();
                gutter_style.fg = diff_foreground(&line.kind, theme).or(gutter_style.fg);
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
        let style = span.style.fallback_bg(line_style);
        buffer.set_text(x + offset, y, &highlighted, &style);
    }
}

fn diff_line_style(kind: &str, theme: &Theme) -> Style {
    let mut style = theme.style.clone();
    style.bg = match kind {
        "added" => diff_background(
            theme,
            "diffEditor.insertedLineBackground",
            "gitDecoration.addedResourceForeground",
        ),
        "removed" => diff_background(
            theme,
            "diffEditor.removedLineBackground",
            "gitDecoration.deletedResourceForeground",
        ),
        "hunk" => theme
            .colors
            .get("diffEditor.diagonalFill")
            .copied()
            .or_else(|| {
                theme
                    .line_highlight_style
                    .as_ref()
                    .and_then(|style| style.bg)
            }),
        _ => style.bg,
    };
    style
}

fn diff_background(theme: &Theme, preferred: &str, fallback: &str) -> Option<Color> {
    theme.colors.get(preferred).copied().or_else(|| {
        theme
            .colors
            .get(fallback)
            .copied()
            .map(|color| match color {
                Color::Rgb { r, g, b } | Color::Rgba { r, g, b, .. } => {
                    Color::Rgba { r, g, b, a: 38 }
                }
            })
    })
}

fn diff_foreground(kind: &str, theme: &Theme) -> Option<Color> {
    let key = match kind {
        "added" => "gitDecoration.addedResourceForeground",
        "removed" => "gitDecoration.deletedResourceForeground",
        "hunk" => "gitDecoration.modifiedResourceForeground",
        _ => return None,
    };
    theme.colors.get(key).copied()
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
            .style
            .clone()
            .unwrap_or_else(|| editor_style.clone())
            .fallback_bg(editor_style);
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
        }
    }

    fn model_with_document() -> WorkspaceModel {
        WorkspaceModel {
            rows: vec![row("file", true)],
            detail_document: Some(document()),
            ..WorkspaceModel::default()
        }
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
        assert!(detail.contains("Diff  wrap"));
        assert!(detail.contains("let first = true"));
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

        let added_row = &buffer.cells[4 * buffer.width..5 * buffer.width];
        let expected_background = theme
            .colors
            .get("diffEditor.insertedLineBackground")
            .copied();
        assert!(added_row
            .iter()
            .any(|cell| cell.style.bg == expected_background));
        assert!(added_row
            .iter()
            .any(|cell| cell.style.fg.is_some() && cell.style.fg != theme.style.fg));
    }
}
