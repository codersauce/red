//! Structured, editor-native presentation for source-backed PR Replay panels.
//!
//! Replay retains the original unified patch while projecting its old and new
//! source independently for Tree-sitter highlighting. Short hunks retain their
//! natural height, long hunks scroll, and only the compact footer stays pinned.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::{
    markdown::{wrap_plain_text, RenderedTextLine, RenderedTextSpan, TextPanelSpanStyle},
    panel::render_text_spans_on_surface,
    workspace::{
        diff_foreground, diff_line_style, display_slice, highlight_document,
        render_syntax_overlays, WorkspaceDocument, WorkspaceDocumentLine,
    },
};
use crate::{
    editor::{render_buffer::RenderBuffer, Point},
    replay::{parse_patch, ReplayDemoStep, ReplayLimits},
    theme::{SelectionForegroundPriority, Style, Theme},
    ui::{ActionBar, ActionPriority, UiAction},
    unicode_utils::{
        display_width, fit_display_width, truncate_display_width,
        truncate_display_width_with_marker, TruncationSide,
    },
};

/// Learning mode represented by a structured Replay coach.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReplayPanelMode {
    /// Keep the exercise focused on manually reconstructing its exact hunk.
    #[default]
    Challenge,
    /// Additionally expose the resulting original-author source.
    Snippet,
}

impl ReplayPanelMode {
    const fn label(self) -> &'static str {
        match self {
            Self::Challenge => "CHALLENGE",
            Self::Snippet => "SNIPPET",
        }
    }
}

/// Completion attributed to one original-author Replay step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplayPanelCompletion {
    pub(crate) index: usize,
    pub(crate) completion: String,
}

/// A private reviewer observation retained only in the preview session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplayPanelNote {
    pub(crate) index: usize,
    pub(crate) text: String,
}

/// Validated, source-backed state for the dedicated PR Replay presentation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplayPanelModel {
    pub(crate) pull_request: u64,
    pub(crate) author: String,
    pub(crate) branch: String,
    pub(crate) title: String,
    pub(crate) index: usize,
    #[serde(default)]
    pub(crate) mode: ReplayPanelMode,
    #[serde(default)]
    pub(crate) hint_visible: bool,
    #[serde(default)]
    pub(crate) help_visible: bool,
    #[serde(default)]
    pub(crate) notice: String,
    #[serde(default)]
    pub(crate) notes: Vec<ReplayPanelNote>,
    #[serde(default)]
    pub(crate) completions: Vec<ReplayPanelCompletion>,
    pub(crate) steps: Vec<ReplayDemoStep>,
}

impl ReplayPanelModel {
    pub(crate) fn current_step(&self) -> Option<&ReplayDemoStep> {
        self.steps.get(self.index)
    }

    fn completion(&self, index: usize) -> Option<&ReplayPanelCompletion> {
        self.completions
            .iter()
            .find(|completion| completion.index == index)
    }

    fn current_completion(&self) -> Option<&ReplayPanelCompletion> {
        self.completion(self.index)
    }

    fn reviewed_count(&self) -> usize {
        self.completions
            .iter()
            .filter(|completion| completion.index < self.steps.len())
            .map(|completion| completion.index)
            .collect::<HashSet<_>>()
            .len()
    }

    fn current_file_position(&self) -> Option<(usize, usize)> {
        let current = self.current_step()?;
        let mut paths = HashSet::with_capacity(self.steps.len());
        let mut current_position = 0;

        for step in &self.steps {
            if paths.insert(step.path.as_str()) && step.path == current.path {
                current_position = paths.len();
            }
        }

        Some((current_position, paths.len()))
    }
}

/// Parsed presentation and its independently syntax-highlightable source hunk.
#[derive(Debug, Clone)]
pub(super) struct ReplayPanelState {
    pub(super) model: ReplayPanelModel,
    pub(super) document: WorkspaceDocument,
}

impl ReplayPanelState {
    /// Parse once at the plugin boundary and reject invalid or unrelated hunks.
    pub(super) fn parse(text: &str) -> Option<Self> {
        let limits = ReplayLimits::default();
        if text.len() > limits.max_patch_bytes {
            return None;
        }
        let model = serde_json::from_str::<ReplayPanelModel>(text).ok()?;
        if model.steps.len() > limits.max_steps || model.current_step().is_none() {
            return None;
        }
        let document = replay_document(&model)?;
        Some(Self { model, document })
    }
}

/// Width- and height-aware distribution of natural-height chrome and diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ReplayPanelLayout {
    pub(super) header_rows: usize,
    pub(super) diff_rows: usize,
    pub(super) change_gap_rows: usize,
    pub(super) change_rows: usize,
    pub(super) footer_rows: usize,
}

impl ReplayPanelLayout {
    pub(super) fn calculate(
        state: &ReplayPanelState,
        width: usize,
        available_height: usize,
    ) -> Self {
        if width == 0 || available_height == 0 {
            return Self {
                header_rows: 0,
                diff_rows: 0,
                change_gap_rows: 0,
                change_rows: 0,
                footer_rows: 0,
            };
        }

        let footer_rows = if available_height >= 4 { 2 } else { 1 };
        let content_height = available_height.saturating_sub(footer_rows);
        let preferred_header = replay_header_lines(state, width).len();
        let minimum_header = preferred_header.min(if content_height >= 6 { 4 } else { 1 });
        let minimum_diff = state
            .document
            .lines
            .len()
            .min(5)
            .min(content_height.saturating_sub(minimum_header));
        let change_capacity = content_height
            .saturating_sub(minimum_header)
            .saturating_sub(minimum_diff);
        let preferred_change_gap = usize::from(change_capacity >= 3);
        let change_rows = if change_capacity >= 2 {
            state.model.steps.len().min(7).min(
                change_capacity
                    .saturating_sub(1)
                    .saturating_sub(preferred_change_gap),
            )
        } else {
            0
        };
        let change_gap_rows = preferred_change_gap * usize::from(change_rows > 0);
        let changes_height = change_gap_rows
            .saturating_add(change_rows)
            .saturating_add(usize::from(change_rows > 0));
        let remaining = content_height.saturating_sub(changes_height);
        let header_rows = preferred_header.min(remaining.saturating_sub(minimum_diff));
        let diff_rows = state
            .document
            .lines
            .len()
            .min(remaining.saturating_sub(header_rows));

        Self {
            header_rows,
            diff_rows,
            change_gap_rows,
            change_rows,
            footer_rows,
        }
    }
}

/// Paint the native panel title and selected hunk position on one calm surface.
pub(super) fn render_replay_panel_title(
    buffer: &mut RenderBuffer,
    state: &ReplayPanelState,
    title: &str,
    position: Point,
    width: usize,
    theme: &Theme,
) {
    let position_label = format!(
        "{:02} / {:02}",
        state.model.index.saturating_add(1),
        state.model.steps.len(),
    );
    let line = aligned_line(
        title,
        TextPanelSpanStyle::Strong,
        &position_label,
        TextPanelSpanStyle::Muted,
        None,
        width,
    );
    render_text_spans_on_surface(
        buffer,
        position.x,
        position.y,
        width,
        &line,
        theme,
        &theme.style,
    );
}

/// Render all structured Replay chrome inside an already-painted panel body.
pub(super) fn render_replay_panel(
    buffer: &mut RenderBuffer,
    state: &ReplayPanelState,
    position: Point,
    width: usize,
    height: usize,
    scroll: usize,
    theme: &Theme,
) {
    let layout = ReplayPanelLayout::calculate(state, width, height);
    if width == 0 || height == 0 {
        return;
    }

    let mut header = replay_header_lines(state, width);
    if layout.header_rows < header.len() && layout.header_rows > 0 {
        let path = header.pop();
        header.truncate(layout.header_rows.saturating_sub(1));
        if let Some(path) = path {
            header.push(path);
        }
    }
    for (offset, line) in header.iter().take(layout.header_rows).enumerate() {
        render_text_spans_on_surface(
            buffer,
            position.x,
            position.y.saturating_add(offset),
            width,
            line,
            theme,
            &theme.style,
        );
    }

    let diff_top = position.y.saturating_add(layout.header_rows);
    let highlights = highlight_document(Some(&state.document), theme);
    for (offset, (line, spans)) in state
        .document
        .lines
        .iter()
        .zip(highlights.iter())
        .skip(scroll)
        .take(layout.diff_rows)
        .enumerate()
    {
        render_replay_diff_line(
            buffer,
            position.x,
            diff_top.saturating_add(offset),
            width,
            line,
            spans,
            theme,
        );
    }

    let changes_top = diff_top
        .saturating_add(layout.diff_rows)
        .saturating_add(layout.change_gap_rows);
    if layout.change_rows > 0 {
        render_change_heading(buffer, position.x, changes_top, width, theme);
        let first = state
            .model
            .index
            .saturating_sub(layout.change_rows / 2)
            .min(state.model.steps.len().saturating_sub(layout.change_rows));
        for (row, (index, step)) in state
            .model
            .steps
            .iter()
            .enumerate()
            .skip(first)
            .take(layout.change_rows)
            .enumerate()
        {
            render_change_row(
                buffer,
                state,
                step,
                index,
                position.x,
                changes_top.saturating_add(row + 1),
                width,
                theme,
            );
        }
    }

    let footer_top = position
        .y
        .saturating_add(height.saturating_sub(layout.footer_rows));
    if layout.footer_rows > 1 {
        buffer.set_text(
            position.x,
            footer_top,
            &"─".repeat(width),
            &theme.ui_style.muted.with_bg(theme.style.bg),
        );
    }
    let actions = replay_actions();
    ActionBar::new(&actions).render(
        buffer,
        position.x,
        position.y.saturating_add(height.saturating_sub(1)),
        width,
        theme,
        &theme.style,
    );
}

fn replay_document(model: &ReplayPanelModel) -> Option<WorkspaceDocument> {
    let step = model.current_step()?;
    let patch = parse_patch(&step.diff, ReplayLimits::default()).ok()?;
    if patch.files.len() != 1 {
        return None;
    }
    let file = patch.files.first()?;
    if file.path()?.to_string_lossy() != step.path || file.hunks.is_empty() {
        return None;
    }

    let mut lines = Vec::new();
    let mut current_hunk = None;
    let mut old_line = 0usize;
    let mut new_line = 0usize;
    for raw in step.diff.lines() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.starts_with("diff --git ") {
            current_hunk = None;
            continue;
        }
        if line.starts_with("@@ ") {
            let hunk = file.hunks.iter().find(|hunk| hunk.header == line)?;
            old_line = hunk.old_range.start;
            new_line = hunk.new_range.start;
            lines.push(WorkspaceDocumentLine {
                id: format!("{}:hunk:{}", step.id, lines.len()),
                text: hunk.header.clone(),
                kind: "hunk".to_string(),
                ..WorkspaceDocumentLine::default()
            });
            current_hunk = Some(hunk);
            continue;
        }
        if current_hunk.is_none() || line.starts_with("\\ No newline at end of file") {
            continue;
        }

        let (kind, text, old, new) = if let Some(text) = line.strip_prefix(' ') {
            let old = old_line;
            let new = new_line;
            old_line = old_line.saturating_add(1);
            new_line = new_line.saturating_add(1);
            ("context", text, Some(old), Some(new))
        } else if let Some(text) = line.strip_prefix('-') {
            let old = old_line;
            old_line = old_line.saturating_add(1);
            ("removed", text, Some(old), None)
        } else if let Some(text) = line.strip_prefix('+') {
            let new = new_line;
            new_line = new_line.saturating_add(1);
            ("added", text, None, Some(new))
        } else {
            return None;
        };
        lines.push(WorkspaceDocumentLine {
            id: format!("{}:line:{}", step.id, lines.len()),
            text: text.to_string(),
            kind: kind.to_string(),
            old_line: old,
            new_line: new,
            ..WorkspaceDocumentLine::default()
        });
    }

    if model.mode == ReplayPanelMode::Snippet {
        lines.push(WorkspaceDocumentLine {
            id: format!("{}:result", step.id),
            text: "ORIGINAL AUTHOR SOURCE".to_string(),
            kind: "hunk".to_string(),
            ..WorkspaceDocumentLine::default()
        });
        for (index, text) in step.after.lines().enumerate() {
            lines.push(WorkspaceDocumentLine {
                id: format!("{}:result:{index}", step.id),
                text: text.to_string(),
                kind: "context".to_string(),
                new_line: Some(index.saturating_add(1)),
                ..WorkspaceDocumentLine::default()
            });
        }
    }

    Some(WorkspaceDocument {
        path: step.path.clone(),
        lines,
    })
}

fn replay_header_lines(state: &ReplayPanelState, width: usize) -> Vec<RenderedTextLine> {
    let Some(step) = state.model.current_step() else {
        return Vec::new();
    };
    let model = &state.model;
    let progress = format!(
        "{} / {} reviewed",
        model.reviewed_count(),
        model.steps.len()
    );
    let metadata = if model.notes.is_empty() {
        format!("#{} · @{}", model.pull_request, model.author)
    } else {
        let suffix = if model.notes.len() == 1 {
            "note"
        } else {
            "notes"
        };
        format!(
            "#{} · @{} · {} {suffix}",
            model.pull_request,
            model.author,
            model.notes.len(),
        )
    };
    let mut lines = vec![
        aligned_line(
            &metadata,
            TextPanelSpanStyle::Strong,
            &progress,
            TextPanelSpanStyle::Muted,
            None,
            width,
        ),
        RenderedTextLine::plain(
            truncate_display_width_with_marker(&model.branch, width, "…", TruncationSide::Right),
            TextPanelSpanStyle::Muted,
        ),
        RenderedTextLine::plain(String::new(), TextPanelSpanStyle::Text),
        aligned_line(
            "CURRENT CHANGE",
            TextPanelSpanStyle::Heading,
            model.mode.label(),
            TextPanelSpanStyle::Muted,
            None,
            width,
        ),
        RenderedTextLine::plain(
            truncate_display_width_with_marker(&step.title, width, "…", TruncationSide::Right),
            TextPanelSpanStyle::Strong,
        ),
        RenderedTextLine::plain(String::new(), TextPanelSpanStyle::Text),
        RenderedTextLine::plain("WHY".to_string(), TextPanelSpanStyle::Heading),
    ];
    lines.extend(
        wrap_plain_text(&step.why, width.max(1), TextPanelSpanStyle::Muted)
            .into_iter()
            .take(2),
    );

    if state.model.hint_visible {
        let hint = format!("HINT  {}", step.hint);
        lines.extend(
            wrap_plain_text(&hint, width.max(1), TextPanelSpanStyle::Quote)
                .into_iter()
                .take(2),
        );
    }
    if state.model.help_visible {
        lines.extend(
            wrap_plain_text(
                "j/k scroll · h/l step · Ctrl-w H/J/K/L dock · Space R h hint · m mode · u undo in source",
                width.max(1),
                TextPanelSpanStyle::Muted,
            )
            .into_iter()
            .take(2),
        );
    }
    if !model.notice.is_empty() {
        lines.extend(
            wrap_plain_text(&model.notice, width.max(1), TextPanelSpanStyle::Quote)
                .into_iter()
                .take(2),
        );
    } else if let Some(completion) = model.current_completion() {
        let status = format!("✓ {}", completion.completion);
        lines.push(RenderedTextLine::plain(
            truncate_display_width_with_marker(&status, width, "…", TruncationSide::Right),
            TextPanelSpanStyle::Muted,
        ));
    }

    lines.push(RenderedTextLine::plain(
        String::new(),
        TextPanelSpanStyle::Text,
    ));
    let file_progress = model
        .current_file_position()
        .filter(|(_, count)| *count > 1)
        .map_or_else(String::new, |(index, count)| {
            format!("{index}/{count} files")
        });
    lines.push(aligned_line(
        &step.path,
        TextPanelSpanStyle::Link,
        &file_progress,
        TextPanelSpanStyle::Muted,
        None,
        width,
    ));
    lines
}

fn aligned_line(
    left: &str,
    left_style: TextPanelSpanStyle,
    right: &str,
    right_style: TextPanelSpanStyle,
    right_syntax_style: Option<Style>,
    width: usize,
) -> RenderedTextLine {
    if width == 0 {
        return RenderedTextLine { spans: Vec::new() };
    }
    let preferred_right = display_width(right);
    let show_right = preferred_right.saturating_add(2) < width;
    let right_width = if show_right { preferred_right } else { 0 };
    let left_width = width
        .saturating_sub(right_width)
        .saturating_sub(usize::from(show_right));
    let left = truncate_display_width_with_marker(left, left_width, "…", TruncationSide::Right);
    let mut spans = vec![RenderedTextSpan {
        text: left.clone(),
        style: left_style,
        syntax_style: None,
        link: None,
    }];
    if show_right {
        spans.push(RenderedTextSpan {
            text: " ".repeat(width.saturating_sub(display_width(&left) + right_width)),
            style: TextPanelSpanStyle::Text,
            syntax_style: None,
            link: None,
        });
        spans.push(RenderedTextSpan {
            text: right.to_string(),
            style: right_style,
            syntax_style: right_syntax_style,
            link: None,
        });
    }
    RenderedTextLine { spans }
}

fn render_replay_diff_line(
    buffer: &mut RenderBuffer,
    x: usize,
    y: usize,
    width: usize,
    line: &WorkspaceDocumentLine,
    highlights: &[crate::editor::StyleInfo],
    theme: &Theme,
) {
    if width == 0 || y >= buffer.height {
        return;
    }
    let line_style = diff_line_style(&line.kind, theme);
    buffer.set_text(x, y, &fit_display_width("", width), &line_style);

    if line.kind == "hunk" {
        let style = theme.ui_style.muted.with_bg(line_style.bg);
        buffer.set_text(
            x.saturating_add(1),
            y,
            &truncate_display_width(&line.text, width.saturating_sub(1)),
            &style,
        );
        return;
    }

    let wide_gutter = width >= 52;
    let gutter_width = if wide_gutter {
        13.min(width)
    } else {
        7.min(width)
    };
    let marker = match line.kind.as_str() {
        "added" => "+",
        "removed" => "−",
        _ => " ",
    };
    let gutter = if wide_gutter {
        format!(
            "{:>4} {:>4} {marker} ",
            line.old_line.map_or(String::new(), |line| line.to_string()),
            line.new_line.map_or(String::new(), |line| line.to_string()),
        )
    } else {
        format!(
            "{:>4} {marker} ",
            line.new_line
                .or(line.old_line)
                .map_or(String::new(), |line| line.to_string()),
        )
    };
    let mut gutter_style = theme.ui_style.muted.with_bg(line_style.bg);
    gutter_style.fg = diff_foreground(&line.kind, theme).or(gutter_style.fg);
    buffer.set_text(
        x,
        y,
        &truncate_display_width(&gutter, gutter_width),
        &gutter_style,
    );

    let code_width = width.saturating_sub(gutter_width);
    if code_width == 0 {
        return;
    }
    let code_x = x.saturating_add(gutter_width);
    let clipped = display_width(&line.text) > code_width;
    let visible = display_slice(
        &line.text,
        /*start_column*/ 0,
        code_width.saturating_sub(usize::from(clipped)),
    );
    let mut displayed = visible.text.clone();
    if clipped {
        displayed.push('›');
    }
    buffer.set_text(
        code_x,
        y,
        &fit_display_width(&displayed, code_width),
        &line_style,
    );
    render_syntax_overlays(
        buffer,
        (code_x, y, code_width),
        &line.text,
        &visible,
        highlights,
        &line_style,
    );
}

fn render_change_heading(
    buffer: &mut RenderBuffer,
    x: usize,
    y: usize,
    width: usize,
    theme: &Theme,
) {
    let line = RenderedTextLine::plain("CHANGES".to_string(), TextPanelSpanStyle::Heading);
    render_text_spans_on_surface(buffer, x, y, width, &line, theme, &theme.style);
}

#[allow(clippy::too_many_arguments)]
fn render_change_row(
    buffer: &mut RenderBuffer,
    state: &ReplayPanelState,
    step: &ReplayDemoStep,
    index: usize,
    x: usize,
    y: usize,
    width: usize,
    theme: &Theme,
) {
    if width == 0 || y >= buffer.height {
        return;
    }
    let active = index == state.model.index;
    let completion = state.model.completion(index);
    let selection = theme.list_selection_style();
    let row_style = if active {
        theme.selected_style(
            &theme.style,
            &selection,
            SelectionForegroundPriority::Content,
        )
    } else {
        theme.style.clone()
    };
    buffer.set_text(x, y, &fit_display_width("", width), &row_style);

    let marker = if completion.is_some_and(|entry| entry.completion == "automatically applied") {
        "⊕"
    } else if completion.is_some() {
        "✓"
    } else if active {
        "●"
    } else {
        "○"
    };
    let marker_style = if marker == "✓" {
        change_kind_style("add", theme)
    } else if active || marker == "⊕" {
        theme.ui_style.picker_prompt.clone()
    } else {
        theme.ui_style.muted.clone()
    }
    .with_bg(row_style.bg);
    let number = format!(" {:02} ", index.saturating_add(1));

    let mut column = x;
    column = render_change_segment(buffer, column, y, x + width, marker, &marker_style);
    column = render_change_segment(
        buffer,
        column,
        y,
        x + width,
        &number,
        &theme.ui_style.muted.with_bg(row_style.bg),
    );
    column = render_change_segment(buffer, column, y, x + width, " ", &row_style);
    let title = truncate_display_width_with_marker(
        &step.title,
        x.saturating_add(width).saturating_sub(column),
        "…",
        TruncationSide::Right,
    );
    render_change_segment(buffer, column, y, x + width, &title, &row_style);
}

fn render_change_segment(
    buffer: &mut RenderBuffer,
    x: usize,
    y: usize,
    end: usize,
    text: &str,
    style: &Style,
) -> usize {
    if x >= end {
        return end;
    }
    let visible = truncate_display_width(text, end.saturating_sub(x));
    buffer.set_text(x, y, &visible, style);
    x.saturating_add(display_width(&visible))
}

fn change_kind_style(kind: &str, theme: &Theme) -> Style {
    let role = match kind.to_ascii_lowercase().as_str() {
        "add" | "added" | "add_file" => "added",
        "remove" | "removed" | "delete" | "delete_file" => "removed",
        _ => "hunk",
    };
    Style {
        fg: diff_foreground(role, theme).or(theme.ui_style.picker_prompt.fg),
        bg: None,
        bold: true,
        italic: false,
    }
}

fn replay_actions() -> Vec<UiAction> {
    vec![
        UiAction::new("edit", "[i]", "Source")
            .with_priority(ActionPriority::Essential)
            .with_compact_label(""),
        UiAction::new("validate", "[v]", "Check")
            .with_priority(ActionPriority::Essential)
            .with_compact_label(""),
        UiAction::new("apply", "[a]", "Preview")
            .with_priority(ActionPriority::Essential)
            .with_compact_label(""),
        UiAction::new("navigate", "[h/l]", "Step")
            .with_priority(ActionPriority::Essential)
            .with_compact_label(""),
        UiAction::new("help", "[?]", "Help")
            .with_priority(ActionPriority::Essential)
            .with_compact_label(""),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{replay::replay_demo_plan, theme::parse_vscode_theme};
    use similar::TextDiff;

    fn model() -> ReplayPanelModel {
        let plan = replay_demo_plan().expect("source-backed demo plan");
        ReplayPanelModel {
            pull_request: plan.pull_request,
            author: plan.author,
            branch: plan.branch,
            title: plan.title,
            index: 0,
            mode: ReplayPanelMode::Challenge,
            hint_visible: false,
            help_visible: false,
            notice: String::new(),
            notes: Vec::new(),
            completions: Vec::new(),
            steps: plan.steps,
        }
    }

    fn state() -> ReplayPanelState {
        let text = serde_json::to_string(&model()).expect("serializable replay model");
        ReplayPanelState::parse(&text).expect("valid source-backed replay state")
    }

    fn source_step(
        ordinal: usize,
        path: &str,
        title: &str,
        before: &str,
        after: &str,
    ) -> ReplayDemoStep {
        let diff = format!(
            "diff --git a/{path} b/{path}\n{}",
            TextDiff::from_lines(before, after)
                .unified_diff()
                .context_radius(3)
                .header(&format!("a/{path}"), &format!("b/{path}")),
        );

        ReplayDemoStep {
            id: format!("fixture-{ordinal}"),
            ordinal,
            path: path.to_string(),
            kind: "change".to_string(),
            title: title.to_string(),
            why: "Keep the original source hunk attached to its learning step.".to_string(),
            task: "Reconstruct the original source change.".to_string(),
            hint: String::new(),
            before: before.to_string(),
            after: after.to_string(),
            diff,
        }
    }

    fn multi_file_model() -> ReplayPanelModel {
        let rendering_before = "pub fn render_visible() -> usize {\n    0\n}\n";
        let rendering_after = "pub fn render_visible() -> usize {\n    1\n}\n";
        let rendering_bounded = "pub fn render_visible() -> usize {\n    1_usize.min(250)\n}\n";
        let editor_before = "pub fn render_editor() -> usize {\n    0\n}\n";
        let editor_after = "pub fn render_editor() -> usize {\n    render_visible()\n}\n";
        let test_before = "#[test]\nfn renders_visible_rows() {\n    assert_eq!(0, 0);\n}\n";
        let test_after =
            "#[test]\nfn renders_visible_rows() {\n    assert_eq!(render_visible(), 1);\n}\n";
        let test_bounded = concat!(
            "#[test]\n",
            "fn renders_visible_rows() {\n",
            "    assert_eq!(render_visible(), 1);\n",
            "    assert!(render_visible() <= 250);\n",
            "}\n",
        );
        let mut replay = model();
        replay.index = 1;
        replay.steps = vec![
            source_step(
                1,
                "src/editor/rendering.rs",
                "Capture the visible viewport",
                rendering_before,
                rendering_after,
            ),
            source_step(
                2,
                "src/editor.rs",
                "Thread the viewport through the editor",
                editor_before,
                editor_after,
            ),
            source_step(
                3,
                "src/editor/rendering.rs",
                "Bound visible rendering work",
                rendering_after,
                rendering_bounded,
            ),
            source_step(
                4,
                "tests/rendering.rs",
                "Cover visible editor rows",
                test_before,
                test_after,
            ),
            source_step(
                5,
                "tests/rendering.rs",
                "Cover the visible rendering bound",
                test_after,
                test_bounded,
            ),
        ];
        replay
    }

    fn rendered_rows(buffer: &RenderBuffer) -> Vec<String> {
        buffer
            .cells
            .chunks(buffer.width)
            .map(|row| row.iter().map(|cell| cell.text.as_str()).collect())
            .collect()
    }

    #[test]
    fn structured_model_rejects_invalid_step_and_nonmatching_patch_path() {
        let mut replay = model();
        replay.index = replay.steps.len();
        let text = serde_json::to_string(&replay).unwrap();
        assert!(ReplayPanelState::parse(&text).is_none());

        let mut replay = model();
        replay.steps[0].path = "src/unrelated.rs".to_string();
        let text = serde_json::to_string(&replay).unwrap();
        assert!(ReplayPanelState::parse(&text).is_none());
    }

    #[test]
    fn replay_hunk_omits_git_boilerplate_and_retains_exact_numbered_source() {
        let state = state();
        assert_eq!(state.document.path, "src/editor/rendering.rs");
        assert!(state
            .document
            .lines
            .iter()
            .any(|line| line.kind == "hunk" && line.text.starts_with("@@ -")));
        let added = state
            .document
            .lines
            .iter()
            .find(|line| line.kind == "added" && line.text.contains("visible_start"))
            .expect("original added source line");
        assert!(added.old_line.is_none());
        assert!(added.new_line.is_some());
        assert!(state.document.lines.iter().all(|line| {
            !line.text.starts_with("diff --git ")
                && !line.text.starts_with("--- a/")
                && !line.text.starts_with("+++ b/")
        }));
    }

    #[test]
    fn replay_diff_uses_language_syntax_without_losing_added_line_background() {
        let state = state();
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let highlights = highlight_document(Some(&state.document), &theme);
        let (index, added) = state
            .document
            .lines
            .iter()
            .enumerate()
            .find(|(_, line)| line.kind == "added" && line.text.contains("usize"))
            .expect("added typed Rust source");
        assert!(!highlights[index].is_empty(), "Rust syntax was not parsed");

        let mut buffer = RenderBuffer::new(/*width*/ 60, /*height*/ 1, &theme.style);
        render_replay_diff_line(
            &mut buffer,
            /*x*/ 0,
            /*y*/ 0,
            /*width*/ 60,
            added,
            &highlights[index],
            &theme,
        );
        let added_background = diff_line_style("added", &theme).bg;
        assert!(added_background.is_some());
        assert!(buffer
            .cells
            .iter()
            .any(|cell| cell.text == "+" && cell.style.bg == added_background));
        assert!(buffer.cells.iter().any(|cell| {
            cell.style.bg == added_background
                && cell.style.fg != diff_line_style("added", &theme).fg
        }));
    }

    #[test]
    fn changed_rust_hunks_highlight_removed_and_added_source_independently() {
        let mut replay = model();
        let before = "pub fn original(value: usize) -> usize {\n    value\n}\n";
        let after = "pub fn updated(value: usize) -> usize {\n    value + 1\n}\n";
        let step = &mut replay.steps[0];
        step.before = before.to_string();
        step.after = after.to_string();
        step.kind = "change".to_string();
        step.diff = format!(
            "diff --git a/{} b/{}\n{}",
            step.path,
            step.path,
            TextDiff::from_lines(before, after)
                .unified_diff()
                .context_radius(3)
                .header(&format!("a/{}", step.path), &format!("b/{}", step.path)),
        );
        let text = serde_json::to_string(&replay).unwrap();
        let state = ReplayPanelState::parse(&text).expect("complete changed Rust hunk");
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let highlights = highlight_document(Some(&state.document), &theme);

        for (kind, expected_source) in [("removed", "original"), ("added", "updated")] {
            let (index, line) = state
                .document
                .lines
                .iter()
                .enumerate()
                .find(|(_, line)| line.kind == kind && line.text.contains(expected_source))
                .expect("independently projected source line");
            assert!(
                !highlights[index].is_empty(),
                "{kind} Rust source lost its Tree-sitter token styles",
            );
            assert_eq!(
                line.old_line.is_some(),
                kind == "removed",
                "old line numbers belong only to removed source",
            );
            assert_eq!(
                line.new_line.is_some(),
                kind == "added",
                "new line numbers belong only to added source",
            );
        }
    }

    #[test]
    fn long_source_lines_keep_their_indentation_and_end_with_a_truncation_marker() {
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let line = WorkspaceDocumentLine {
            id: "long-source".to_string(),
            text: "        diagnostics.filter(|diagnostic| visible_start <= diagnostic.line)"
                .to_string(),
            kind: "added".to_string(),
            new_line: Some(12),
            ..WorkspaceDocumentLine::default()
        };
        let mut buffer = RenderBuffer::new(/*width*/ 25, /*height*/ 2, &theme.style);

        render_replay_diff_line(
            &mut buffer,
            /*x*/ 0,
            /*y*/ 0,
            /*width*/ 25,
            &line,
            &[],
            &theme,
        );

        let first = buffer.cells[..25]
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>();
        let second = buffer.cells[25..]
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>();
        assert!(first.contains("  12 +         "));
        assert!(first.ends_with('›'));
        assert!(second.trim().is_empty());
    }

    #[test]
    fn snippet_mode_adds_syntax_highlightable_original_author_source() {
        let mut replay = model();
        replay.mode = ReplayPanelMode::Snippet;
        let text = serde_json::to_string(&replay).unwrap();
        let state = ReplayPanelState::parse(&text).unwrap();
        assert!(state
            .document
            .lines
            .iter()
            .any(|line| { line.kind == "hunk" && line.text == "ORIGINAL AUTHOR SOURCE" }));
        assert!(state.document.lines.iter().any(|line| {
            line.kind == "context" && line.text.contains("pub fn diagnostics_by_visible_line")
        }));
    }

    #[test]
    fn compact_layout_keeps_hunk_steps_and_footer_visible() {
        let state = state();
        let layout = ReplayPanelLayout::calculate(&state, /*width*/ 46, /*height*/ 23);
        assert!(layout.header_rows >= 4);
        assert!(layout.diff_rows >= 5);
        assert_eq!(layout.change_rows, 5);
        assert_eq!(layout.footer_rows, 2);
        assert!(
            layout.header_rows
                + layout.diff_rows
                + layout.change_gap_rows
                + usize::from(layout.change_rows > 0)
                + layout.change_rows
                + layout.footer_rows
                <= 23,
        );
        assert!(layout.diff_rows <= state.document.lines.len());
    }

    #[test]
    fn replay_action_bar_keeps_complete_shortcuts_at_narrow_widths() {
        let actions = replay_actions();
        let layout = ActionBar::new(&actions).layout(/*width*/ 28);
        let visible = layout.text();
        assert!(visible.contains("[i]"));
        assert!(visible.contains("[v]"));
        assert!(visible.contains("[a]"));
        assert!(visible.contains("[h/l]"));
        assert!(visible.contains("[?]"));
        assert!(display_width(&visible) <= 28);
        assert_eq!(layout.hidden_count(), 0);
    }

    #[test]
    fn short_hunk_separates_original_source_and_changes_by_one_blank_row() {
        let state = state();
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let width = 46;
        let height = 40;
        let layout = ReplayPanelLayout::calculate(&state, width, height);
        let mut buffer = RenderBuffer::new(width, height, &theme.style);

        render_replay_panel(
            &mut buffer,
            &state,
            Point::new(0, 0),
            width,
            height,
            /*scroll*/ 0,
            &theme,
        );

        let rows = rendered_rows(&buffer);
        let changes_row = layout.header_rows + layout.diff_rows + layout.change_gap_rows;
        assert_eq!(layout.diff_rows, state.document.lines.len());
        assert_eq!(layout.change_gap_rows, 1);
        assert!(rows[changes_row].starts_with("CHANGES"));
        assert!(rows[changes_row - 1].trim().is_empty());
        assert!(rows[changes_row - 2].contains(
            state
                .document
                .lines
                .last()
                .expect("source-backed hunk")
                .text
                .trim(),
        ));
        assert!(rows[height - 1].contains("[?]"));
        assert!(!rows[height - 1].contains("LOCAL"));
    }

    #[test]
    fn replay_chrome_preserves_one_continuous_surface_background() {
        let state = state();
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let width = 46;
        let height = 40;
        let layout = ReplayPanelLayout::calculate(&state, width, height);
        let mut buffer = RenderBuffer::new(width, height, &theme.style);

        render_replay_panel(
            &mut buffer,
            &state,
            Point::new(0, 0),
            width,
            height,
            /*scroll*/ 0,
            &theme,
        );

        for row in 0..layout.header_rows {
            assert!(buffer.cells[row * width..(row + 1) * width]
                .iter()
                .all(|cell| cell.style.bg == theme.style.bg));
        }
        let changes_row = layout.header_rows + layout.diff_rows + layout.change_gap_rows;
        assert!(buffer.cells[changes_row * width..(changes_row + 1) * width]
            .iter()
            .all(|cell| cell.style.bg == theme.style.bg));
    }

    #[test]
    fn manual_and_automatic_completion_keep_distinct_progress_markers() {
        let mut replay = model();
        replay.index = 2;
        replay.completions = vec![
            ReplayPanelCompletion {
                index: 0,
                completion: "manually reconstructed".to_string(),
            },
            ReplayPanelCompletion {
                index: 1,
                completion: "automatically applied".to_string(),
            },
        ];
        let text = serde_json::to_string(&replay).unwrap();
        let state = ReplayPanelState::parse(&text).unwrap();
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let mut buffer = RenderBuffer::new(/*width*/ 46, /*height*/ 35, &theme.style);

        render_replay_panel(
            &mut buffer,
            &state,
            Point::new(0, 0),
            /*width*/ 46,
            /*height*/ 35,
            /*scroll*/ 0,
            &theme,
        );

        let rows = rendered_rows(&buffer);
        assert!(rows.iter().any(|row| row.starts_with("✓ 01 ")));
        assert!(rows.iter().any(|row| row.starts_with("⊕ 02 ")));
        assert!(rows.iter().any(|row| row.starts_with("● 03 ")));
        assert!(!rows.iter().any(|row| row.contains(" ADD ")));
    }

    #[test]
    fn active_change_selection_paints_the_entire_terminal_row() {
        let state = state();
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let width = 46;
        let height = 35;
        let layout = ReplayPanelLayout::calculate(&state, width, height);
        let mut buffer = RenderBuffer::new(width, height, &theme.style);

        render_replay_panel(
            &mut buffer,
            &state,
            Point::new(0, 0),
            width,
            height,
            /*scroll*/ 0,
            &theme,
        );

        let selection = theme.list_selection_style();
        let expected_background = theme
            .selected_style(
                &theme.style,
                &selection,
                SelectionForegroundPriority::Content,
            )
            .bg;
        let row = layout.header_rows + layout.diff_rows + layout.change_gap_rows + 1;
        assert!(buffer.cells[row * width..(row + 1) * width]
            .iter()
            .all(|cell| cell.style.bg == expected_background));
    }

    #[test]
    fn multi_file_steps_show_source_provenance_without_a_file_tree() {
        let replay = multi_file_model();
        let text = serde_json::to_string(&replay).unwrap();
        let state = ReplayPanelState::parse(&text).unwrap();
        let lines = replay_header_lines(&state, /*width*/ 46);
        let source = lines
            .last()
            .expect("source provenance line")
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();

        assert_eq!(state.model.current_file_position(), Some((2, 3)));
        assert!(source.contains("src/editor.rs"));
        assert!(source.contains("2/3 files"));
        assert!(state
            .document
            .lines
            .iter()
            .any(|line| line.text.contains("render_visible()")));
    }

    #[test]
    fn native_title_distinguishes_selected_position_from_reviewed_progress() {
        let mut replay = model();
        replay.index = 2;
        replay.completions = vec![ReplayPanelCompletion {
            index: 0,
            completion: "manually reconstructed".to_string(),
        }];
        let text = serde_json::to_string(&replay).unwrap();
        let state = ReplayPanelState::parse(&text).unwrap();
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let mut buffer = RenderBuffer::new(/*width*/ 46, /*height*/ 1, &theme.style);

        render_replay_panel_title(
            &mut buffer,
            &state,
            "PR REPLAY",
            Point::new(0, 0),
            /*width*/ 46,
            &theme,
        );

        let title = &rendered_rows(&buffer)[0];
        let metadata = replay_header_lines(&state, /*width*/ 46)[0]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();
        assert!(title.starts_with("PR REPLAY"));
        assert!(title.ends_with("03 / 05"));
        assert!(metadata.contains("#482 · @original-author"));
        assert!(metadata.ends_with("1 / 5 reviewed"));
        assert!(!title.contains("reviewed"));
    }

    #[test]
    fn challenge_mode_and_rationale_are_attached_to_the_current_exercise() {
        let state = state();
        let lines = replay_header_lines(&state, /*width*/ 46)
            .into_iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let change_heading = lines
            .iter()
            .position(|line| line.starts_with("CURRENT CHANGE"))
            .expect("current exercise heading");
        let rationale = lines
            .iter()
            .position(|line| line == "WHY")
            .expect("explicit original-author rationale");

        assert!(lines[change_heading].ends_with("CHALLENGE"));
        assert!(lines[change_heading + 1].contains("Capture the visible viewport"));
        assert!(lines[rationale + 1].contains("Diagnostics must be evaluated"));
        assert!(lines[1].contains("feat/viewport-diagnostics"));
        assert!(!lines[1].contains("CHALLENGE"));
    }

    #[test]
    fn duplicate_and_invalid_completion_records_do_not_inflate_review_progress() {
        let mut replay = model();
        replay.completions = vec![
            ReplayPanelCompletion {
                index: 0,
                completion: "manually reconstructed".to_string(),
            },
            ReplayPanelCompletion {
                index: 0,
                completion: "automatically applied".to_string(),
            },
            ReplayPanelCompletion {
                index: replay.steps.len(),
                completion: "manually reconstructed".to_string(),
            },
        ];

        assert_eq!(replay.reviewed_count(), 1);
    }

    #[test]
    fn narrow_and_tall_panels_keep_source_changes_and_all_shortcuts_visible() {
        let state = state();
        let theme = parse_vscode_theme("themes/red.json").unwrap();

        for (width, height) in [(42, 30), (46, 48)] {
            let layout = ReplayPanelLayout::calculate(&state, width, height);
            let mut buffer = RenderBuffer::new(width, height, &theme.style);
            render_replay_panel(
                &mut buffer,
                &state,
                Point::new(0, 0),
                width,
                height,
                /*scroll*/ 0,
                &theme,
            );

            let rows = rendered_rows(&buffer);
            let changes_row = layout.header_rows + layout.diff_rows + layout.change_gap_rows;
            let footer = &rows[height - 1];
            assert!(rows[changes_row].starts_with("CHANGES"));
            assert_eq!(layout.change_gap_rows, 1);
            assert!(rows[changes_row - 1].trim().is_empty());
            for shortcut in ["[i]", "[v]", "[a]", "[h/l]", "[?]"] {
                assert!(
                    footer.contains(shortcut),
                    "missing {shortcut} at {width} columns"
                );
            }
            assert!(!footer.contains("… +"));
        }
    }
}
