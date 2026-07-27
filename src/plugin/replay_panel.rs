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
    replay::{
        parse_patch, GitObjectId, ReplayDemoStep, ReplayLimits, ReplayReviewDraft,
        ReplayReviewDraftKind, ReplayReviewRole,
    },
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

/// Editor-native surface shown within the dedicated Replay pane.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReplayPanelView {
    /// The original diff, source guidance, and learning-step list.
    #[default]
    Guide,
    /// The recoverable, original-source-anchored local review outbox.
    Outbox,
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
    #[serde(default)]
    pub(crate) review_role: Option<ReplayReviewRole>,
    #[serde(default)]
    pub(crate) head_commit: String,
    #[serde(default)]
    pub(crate) draft_count: usize,
    #[serde(default)]
    pub(crate) drafts: Vec<ReplayReviewDraft>,
    #[serde(default)]
    pub(crate) outbox_index: usize,
    #[serde(default)]
    pub(crate) view: ReplayPanelView,
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
        if model.steps.len() > limits.max_steps
            || model.draft_count > limits.max_steps
            || model.drafts.len() > limits.max_steps
            || model.current_step().is_none()
            || (!model.head_commit.is_empty() && GitObjectId::parse(&model.head_commit).is_err())
            || (!model.drafts.is_empty() && model.outbox_index >= model.drafts.len())
            || model
                .drafts
                .iter()
                .any(|draft| draft.text.len() > limits.max_note_bytes)
        {
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

/// Scroll and keyboard-focus state for one rendered Replay source viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ReplayPanelViewport {
    pub(super) scroll: usize,
    pub(super) focused: bool,
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
    focused: bool,
    theme: &Theme,
) {
    let title = if focused {
        format!("▌ {title}")
    } else {
        title.to_string()
    };
    let position_label = if state.model.view == ReplayPanelView::Outbox {
        if state.model.drafts.is_empty() {
            "OUTBOX".to_string()
        } else {
            format!(
                "{:02} / {:02}",
                state.model.outbox_index.saturating_add(1),
                state.model.drafts.len(),
            )
        }
    } else {
        format!(
            "{:02} / {:02}",
            state.model.index.saturating_add(1),
            state.model.steps.len(),
        )
    };
    let line = aligned_line(
        &title,
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

    let right_width = display_width(&position_label);
    let title_width = if right_width.saturating_add(2) < width {
        width.saturating_sub(right_width).saturating_sub(1)
    } else {
        width
    };
    let title = truncate_display_width_with_marker(&title, title_width, "…", TruncationSide::Right);
    let foreground = if focused {
        theme
            .colors
            .get("panelTitle.activeForeground")
            .copied()
            .or_else(|| theme.colors.get("editorCursor.foreground").copied())
            .or_else(|| theme.colors.get("focusBorder").copied())
            .or(theme.ui_style.picker_prompt.fg)
    } else {
        theme
            .colors
            .get("panelTitle.inactiveForeground")
            .copied()
            .or_else(|| theme.colors.get("sideBarTitle.foreground").copied())
            .or(theme.ui_style.muted.fg)
    };
    let title_style = Style {
        fg: foreground.or(theme.style.fg),
        bg: theme.style.bg,
        bold: true,
        italic: false,
    };
    buffer.set_text(position.x, position.y, &title, &title_style);
}

/// Render all structured Replay chrome inside an already-painted panel body.
pub(super) fn render_replay_panel(
    buffer: &mut RenderBuffer,
    state: &ReplayPanelState,
    position: Point,
    width: usize,
    height: usize,
    viewport: ReplayPanelViewport,
    theme: &Theme,
) {
    if width == 0 || height == 0 {
        return;
    }
    if state.model.view == ReplayPanelView::Outbox {
        render_replay_outbox(buffer, state, position, width, height, viewport, theme);
        return;
    }
    let layout = ReplayPanelLayout::calculate(state, width, height);

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
        .skip(viewport.scroll)
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
        render_change_heading(buffer, state, position.x, changes_top, width, theme);
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
                viewport.focused,
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
    let actions = replay_actions(&state.model, width);
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
        } else {
            let text = line.strip_prefix('+')?;
            let new = new_line;
            new_line = new_line.saturating_add(1);
            ("added", text, None, Some(new))
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
    let identity = if model.pull_request == 0 {
        "LOCAL BRANCH".to_string()
    } else {
        format!("#{} · @{}", model.pull_request, model.author)
    };
    let mut metadata = if model.notes.is_empty() {
        identity
    } else {
        let suffix = if model.notes.len() == 1 {
            "note"
        } else {
            "notes"
        };
        format!("{identity} · {} {suffix}", model.notes.len())
    };
    if model.draft_count > 0 {
        let suffix = if model.draft_count == 1 {
            "draft"
        } else {
            "drafts"
        };
        metadata.push_str(&format!(" · {} {suffix}", model.draft_count));
    }
    let verified_role = model.review_role.filter(|_| model.pull_request > 0);
    let mut branch = model.branch.clone();
    if !model.head_commit.is_empty() {
        let short = model.head_commit.chars().take(7).collect::<String>();
        let suffix = format!(" · {short}");
        let branch_width = if verified_role.is_some() {
            width
                .saturating_sub(display_width(&progress))
                .saturating_sub(1)
        } else {
            width
        };
        let name_width = branch_width.saturating_sub(display_width(&suffix));
        branch = format!(
            "{}{}",
            truncate_display_width_with_marker(
                &model.branch,
                name_width,
                "…",
                TruncationSide::Right,
            ),
            suffix,
        );
    }
    let mut lines = if let Some(role) = verified_role {
        let label = match role {
            ReplayReviewRole::Reviewer => "REVIEW",
            ReplayReviewRole::Author => "AUTHOR",
        };
        vec![
            aligned_line(
                &metadata,
                TextPanelSpanStyle::Strong,
                label,
                TextPanelSpanStyle::Heading,
                None,
                width,
            ),
            aligned_line(
                &branch,
                TextPanelSpanStyle::Muted,
                &progress,
                TextPanelSpanStyle::Muted,
                None,
                width,
            ),
        ]
    } else {
        vec![
            aligned_line(
                &metadata,
                TextPanelSpanStyle::Strong,
                &progress,
                TextPanelSpanStyle::Muted,
                None,
                width,
            ),
            RenderedTextLine::plain(
                truncate_display_width_with_marker(&branch, width, "…", TruncationSide::Right),
                TextPanelSpanStyle::Muted,
            ),
        ]
    };
    let title = model.title.trim();
    if !title.is_empty()
        && title != model.branch
        && title.strip_prefix("Replay ") != Some(model.branch.as_str())
    {
        lines.push(RenderedTextLine::plain(
            truncate_display_width_with_marker(title, width, "…", TruncationSide::Right),
            TextPanelSpanStyle::Text,
        ));
    }
    lines.extend([
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
    ]);
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
                "j/k scroll · h/l step · [/] file · a apply · u undo · c comment · r outbox · Ctrl-w H/J/K/L dock · Space R h hint",
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

/// Returns the complete scrollable row count of the selected Replay surface.
pub(super) fn replay_content_line_count(state: &ReplayPanelState, width: usize) -> usize {
    if state.model.view == ReplayPanelView::Outbox {
        replay_outbox_lines(state, width).len()
    } else {
        state.document.lines.len()
    }
}

/// Returns native viewport rows while retaining the selected surface's footer.
pub(super) fn replay_visible_rows(state: &ReplayPanelState, width: usize, height: usize) -> usize {
    if state.model.view == ReplayPanelView::Outbox {
        height
            .saturating_sub(replay_outbox_footer_rows(height))
            .max(1)
    } else {
        ReplayPanelLayout::calculate(state, width, height)
            .diff_rows
            .max(1)
    }
}

/// Locates the actual native outbox selection marker for terminal cursor focus.
pub(super) fn replay_outbox_selected_row(state: &ReplayPanelState, width: usize) -> usize {
    replay_outbox_lines(state, width)
        .iter()
        .position(|line| {
            line.spans
                .first()
                .is_some_and(|span| span.text.starts_with('▶'))
        })
        .unwrap_or(/*outbox_heading_row*/ 3)
}

fn replay_outbox_footer_rows(height: usize) -> usize {
    if height > 2 {
        2
    } else {
        usize::from(height > 0)
    }
}

fn replay_outbox_lines(state: &ReplayPanelState, width: usize) -> Vec<RenderedTextLine> {
    let model = &state.model;
    let mut lines = replay_header_lines(state, width)
        .into_iter()
        .take(2)
        .collect::<Vec<_>>();
    lines.push(RenderedTextLine::plain(
        String::new(),
        TextPanelSpanStyle::Text,
    ));
    let count = if model.drafts.len() == 1 {
        "1 draft".to_string()
    } else {
        format!("{} drafts", model.drafts.len())
    };
    lines.push(aligned_line(
        "LOCAL OUTBOX",
        TextPanelSpanStyle::Heading,
        &count,
        TextPanelSpanStyle::Muted,
        None,
        width,
    ));
    lines.extend(wrap_plain_text(
        "Local only · nothing sent to GitHub",
        width.max(1),
        TextPanelSpanStyle::Muted,
    ));
    lines.push(RenderedTextLine::plain(
        String::new(),
        TextPanelSpanStyle::Text,
    ));

    if model.drafts.is_empty() {
        let message = if model.review_role == Some(ReplayReviewRole::Author) {
            "No review drafts yet. Use c for a comment, s for a summary, or F for a proposed fix."
        } else {
            "No review drafts yet. Use c for a comment or s for a review summary."
        };
        lines.extend(wrap_plain_text(
            message,
            width.max(1),
            TextPanelSpanStyle::Muted,
        ));
        return lines;
    }

    for (index, draft) in model.drafts.iter().enumerate() {
        let marker = if index == model.outbox_index {
            "▶"
        } else {
            "○"
        };
        let kind = match draft.kind {
            ReplayReviewDraftKind::InlineComment => "INLINE COMMENT",
            ReplayReviewDraftKind::CodeFix => "PROPOSED PR FIX",
            ReplayReviewDraftKind::ReviewSummary => "REVIEW SUMMARY",
        };
        let label = format!("{marker} {kind}");
        lines.push(aligned_line(
            &label,
            if index == model.outbox_index {
                TextPanelSpanStyle::Strong
            } else {
                TextPanelSpanStyle::Text
            },
            "LOCAL",
            TextPanelSpanStyle::Muted,
            None,
            width,
        ));
        if let Some(anchor) = &draft.anchor {
            let mut line_suffix = format!(":{}", anchor.start_line);
            if anchor.end_line > anchor.start_line {
                line_suffix.push_str(&format!("-{}", anchor.end_line));
            }
            let side = match anchor.side {
                crate::replay::ReplayDiffSide::Left => "LEFT",
                crate::replay::ReplayDiffSide::Right => "RIGHT",
            };
            let source_width = width.saturating_sub(display_width(side)).saturating_sub(1);
            let path_width = source_width.saturating_sub(display_width(&line_suffix));
            let path = truncate_display_width_with_marker(
                &anchor.path.to_string_lossy(),
                path_width,
                "…",
                TruncationSide::Left,
            );
            let source = format!("{path}{line_suffix}");
            lines.push(aligned_line(
                &source,
                TextPanelSpanStyle::Link,
                side,
                TextPanelSpanStyle::Muted,
                None,
                width,
            ));
        }
        lines.extend(wrap_plain_text(
            &draft.text,
            width.max(1),
            TextPanelSpanStyle::Text,
        ));
        lines.push(RenderedTextLine::plain(
            String::new(),
            TextPanelSpanStyle::Text,
        ));
    }
    lines
}

fn render_replay_outbox(
    buffer: &mut RenderBuffer,
    state: &ReplayPanelState,
    position: Point,
    width: usize,
    height: usize,
    viewport: ReplayPanelViewport,
    theme: &Theme,
) {
    let footer_rows = replay_outbox_footer_rows(height);
    let visible_rows = height.saturating_sub(footer_rows);
    for (offset, line) in replay_outbox_lines(state, width)
        .iter()
        .skip(viewport.scroll)
        .take(visible_rows)
        .enumerate()
    {
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

    if footer_rows > 1 {
        buffer.set_text(
            position.x,
            position
                .y
                .saturating_add(height.saturating_sub(footer_rows)),
            &"─".repeat(width),
            &theme.ui_style.muted.with_bg(theme.style.bg),
        );
    }
    if footer_rows > 0 {
        let actions = replay_outbox_actions(&state.model);
        ActionBar::new(&actions).render(
            buffer,
            position.x,
            position.y.saturating_add(height.saturating_sub(1)),
            width,
            theme,
            &theme.style,
        );
    }
}

fn replay_outbox_actions(model: &ReplayPanelModel) -> Vec<UiAction> {
    let mut actions = vec![
        UiAction::new("navigate_draft", "[h/l]", "Select")
            .with_priority(ActionPriority::Essential)
            .with_compact_label(""),
        UiAction::new("comment", "[c]", "Comment")
            .with_priority(ActionPriority::Essential)
            .with_compact_label(""),
        UiAction::new("summary", "[s]", "Summary")
            .with_priority(ActionPriority::Secondary)
            .with_compact_label(""),
        UiAction::new("edit_draft", "[e]", "Edit")
            .with_priority(ActionPriority::Secondary)
            .with_compact_label(""),
        UiAction::new("discard_draft", "[d]", "Discard")
            .with_priority(ActionPriority::Secondary)
            .with_compact_label(""),
        UiAction::new("outbox", "[r]", "Return")
            .with_priority(ActionPriority::Essential)
            .with_compact_label(""),
    ];
    if model.review_role == Some(ReplayReviewRole::Author) {
        actions.insert(
            actions.len().saturating_sub(1),
            UiAction::new("fix", "[F]", "Fix")
                .with_priority(ActionPriority::Secondary)
                .with_compact_label(""),
        );
    }
    actions
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
    state: &ReplayPanelState,
    x: usize,
    y: usize,
    width: usize,
    theme: &Theme,
) {
    let file_progress = state
        .model
        .current_file_position()
        .filter(|(_, count)| *count > 1)
        .map_or_else(String::new, |(index, count)| {
            format!("{index}/{count} files")
        });
    let line = aligned_line(
        "CHANGES",
        TextPanelSpanStyle::Heading,
        &file_progress,
        TextPanelSpanStyle::Muted,
        None,
        width,
    );
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
    focused: bool,
    theme: &Theme,
) {
    if width == 0 || y >= buffer.height {
        return;
    }
    let active = index == state.model.index;
    let completion = state.model.completion(index);
    let mut selection = theme.list_selection_style();
    if !focused {
        selection.bg = theme
            .colors
            .get("list.inactiveSelectionBackground")
            .copied()
            .or_else(|| theme.colors.get("editor.selectionBackground").copied())
            .or(selection.bg);
        selection.fg = theme
            .colors
            .get("list.inactiveSelectionForeground")
            .copied()
            .or(theme.ui_style.muted.fg)
            .or(selection.fg);
    }
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
    let caret = if focused && active { "▶ " } else { "  " };
    let caret_style = theme.ui_style.picker_prompt.clone().with_bg(row_style.bg);
    column = render_change_segment(buffer, column, y, x + width, caret, &caret_style);
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

fn replay_actions(model: &ReplayPanelModel, width: usize) -> Vec<UiAction> {
    let mut actions = vec![
        UiAction::new("edit", "[i]", "Source")
            .with_priority(ActionPriority::Essential)
            .with_compact_label(""),
        UiAction::new("validate", "[v]", "Check")
            .with_priority(ActionPriority::Essential)
            .with_compact_label(""),
        UiAction::new("apply", "[a]", "Apply")
            .with_priority(ActionPriority::Essential)
            .with_compact_label(""),
        UiAction::new("undo", "[u]", "Undo")
            .with_priority(ActionPriority::Essential)
            .with_compact_label(""),
        UiAction::new("navigate", "[h/l]", "Step")
            .with_priority(ActionPriority::Essential)
            .with_compact_label(""),
        UiAction::new("help", "[?]", "Help")
            .with_priority(ActionPriority::Essential)
            .with_compact_label(""),
    ];

    if model.pull_request > 0 && model.review_role.is_some() {
        actions.insert(
            actions.len().saturating_sub(1),
            UiAction::new("comment", "[c]", "Comment")
                .with_priority(ActionPriority::Secondary)
                .with_compact_label(""),
        );
        actions.insert(
            actions.len().saturating_sub(1),
            UiAction::new("outbox", "[r]", "Outbox")
                .with_priority(ActionPriority::Secondary)
                .with_compact_label(""),
        );
    }

    if model
        .current_file_position()
        .is_some_and(|(_, count)| count > 1)
    {
        actions.insert(
            5,
            UiAction::new("navigate_file", "[/]", "File")
                .with_priority(ActionPriority::Essential)
                .with_compact_label(""),
        );
        if ActionBar::new(&actions).layout(width).hidden_count() > 0 {
            actions.remove(5);
        }
    }

    actions
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
            review_role: None,
            head_commit: String::new(),
            draft_count: 0,
            drafts: Vec::new(),
            outbox_index: 0,
            view: ReplayPanelView::Guide,
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

    fn outbox_draft(kind: ReplayReviewDraftKind, text: &str) -> ReplayReviewDraft {
        let target_commit = GitObjectId::parse(&"b".repeat(40)).unwrap();
        let anchor = if kind == ReplayReviewDraftKind::ReviewSummary {
            None
        } else {
            Some(crate::replay::ReplayReviewAnchor {
                target_commit: target_commit.clone(),
                path: "src/editor/rendering.rs".into(),
                old_path: Some("src/editor/rendering.rs".into()),
                side: crate::replay::ReplayDiffSide::Right,
                start_line: 11,
                end_line: 12,
                hunk_digest: "original-hunk-digest".to_string(),
            })
        };
        ReplayReviewDraft {
            id: format!("native-outbox-{kind:?}"),
            target_commit,
            step_id: anchor.as_ref().map(|_| "fixture-original-step".to_string()),
            path: anchor.as_ref().map(|anchor| anchor.path.clone()),
            kind,
            origin: crate::replay::ReplayDraftOrigin::Human,
            state: crate::replay::ReplayDraftState::Local,
            anchor,
            text: text.to_string(),
            created_at_ms: 1,
            updated_at_ms: 1,
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
        let actions = replay_actions(&model(), /*width*/ 30);
        let layout = ActionBar::new(&actions).layout(/*width*/ 30);
        let visible = layout.text();
        assert!(visible.contains("[i]"));
        assert!(visible.contains("[v]"));
        assert!(visible.contains("[a]"));
        assert!(visible.contains("[u]"));
        assert!(visible.contains("[h/l]"));
        assert!(visible.contains("[?]"));
        assert!(display_width(&visible) <= 30);
        assert_eq!(layout.hidden_count(), 0);
    }

    #[test]
    fn multi_file_action_bar_shows_file_motion_without_hiding_essential_shortcuts() {
        let replay = multi_file_model();
        let actions = replay_actions(&replay, /*width*/ 46);
        let layout = ActionBar::new(&actions).layout(/*width*/ 46);
        let visible = layout.text();

        for key in ["[i]", "[v]", "[a]", "[u]", "[h/l]", "[/]", "[?]"] {
            assert!(visible.contains(key), "missing visible replay action {key}");
        }
        assert_eq!(layout.hidden_count(), 0);

        let compact_actions = replay_actions(&replay, /*width*/ 30);
        let compact_layout = ActionBar::new(&compact_actions).layout(/*width*/ 30);
        let compact_visible = compact_layout.text();
        for key in ["[i]", "[v]", "[a]", "[u]", "[h/l]", "[?]"] {
            assert!(
                compact_visible.contains(key),
                "missing essential narrow replay action {key}"
            );
        }
        assert!(!compact_visible.contains("[/]"));
        assert_eq!(compact_layout.hidden_count(), 0);
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
            ReplayPanelViewport {
                scroll: 0,
                focused: true,
            },
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
            ReplayPanelViewport {
                scroll: 0,
                focused: true,
            },
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
            ReplayPanelViewport {
                scroll: 0,
                focused: true,
            },
            &theme,
        );

        let rows = rendered_rows(&buffer);
        assert!(rows.iter().any(|row| row.trim_start().starts_with("✓ 01 ")));
        assert!(rows.iter().any(|row| row.trim_start().starts_with("⊕ 02 ")));
        assert!(rows.iter().any(|row| row.starts_with("▶ ● 03 ")));
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
            ReplayPanelViewport {
                scroll: 0,
                focused: true,
            },
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
    fn multi_file_change_heading_keeps_current_file_progress_on_the_original_surface() {
        let replay = multi_file_model();
        let text = serde_json::to_string(&replay).unwrap();
        let state = ReplayPanelState::parse(&text).unwrap();
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
            ReplayPanelViewport {
                scroll: 0,
                focused: true,
            },
            &theme,
        );

        let heading =
            &rendered_rows(&buffer)[layout.header_rows + layout.diff_rows + layout.change_gap_rows];
        assert!(heading.starts_with("CHANGES"));
        assert!(heading.contains("2/3 files"));
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
            /*focused*/ false,
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
    fn original_author_role_and_pinned_head_are_visible_without_hiding_review_progress() {
        let mut replay = model();
        replay.review_role = Some(ReplayReviewRole::Author);
        replay.head_commit = "b".repeat(40);
        replay.draft_count = 2;
        let state = ReplayPanelState::parse(&serde_json::to_string(&replay).unwrap()).unwrap();
        let lines = replay_header_lines(&state, /*width*/ 64)
            .into_iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        assert!(lines[0].contains("#482 · @original-author"));
        assert!(lines[0].contains("2 drafts"));
        assert!(lines[0].ends_with("AUTHOR"));
        assert!(lines[1].contains("feat/viewport-diagnostics"));
        assert!(lines[1].contains("bbbbbbb"));
        assert!(lines[1].ends_with("0 / 5 reviewed"));
    }

    #[test]
    fn narrow_author_header_preserves_original_head_commit_and_review_progress() {
        let mut replay = model();
        replay.review_role = Some(ReplayReviewRole::Author);
        replay.branch = "fcoury/tui-paginated-history".to_string();
        replay.head_commit = "15c49574d325c0cb783a12cadab7b25fb089ed3e".to_string();
        let state = ReplayPanelState::parse(&serde_json::to_string(&replay).unwrap()).unwrap();
        let branch = replay_header_lines(&state, /*width*/ 46)[1]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();

        assert!(branch.contains("15c4957"));
        assert!(branch.ends_with("0 / 5 reviewed"));
        assert!(branch.contains('…'));
    }

    #[test]
    fn native_outbox_retains_original_anchor_selected_marker_and_pinned_actions() {
        let mut replay = model();
        replay.review_role = Some(ReplayReviewRole::Author);
        replay.head_commit = "b".repeat(40);
        replay.view = ReplayPanelView::Outbox;
        replay.drafts = vec![outbox_draft(
            ReplayReviewDraftKind::InlineComment,
            "Please test the original viewport boundary.",
        )];
        replay.draft_count = replay.drafts.len();
        let state = ReplayPanelState::parse(&serde_json::to_string(&replay).unwrap()).unwrap();
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let width = 56;
        let height = 18;
        let mut buffer = RenderBuffer::new(width, height, &theme.style);

        render_replay_panel_title(
            &mut buffer,
            &state,
            "PR REPLAY",
            Point::new(0, 0),
            width,
            /*focused*/ true,
            &theme,
        );
        render_replay_panel(
            &mut buffer,
            &state,
            Point::new(0, 1),
            width,
            height - 1,
            ReplayPanelViewport {
                scroll: 0,
                focused: true,
            },
            &theme,
        );

        let rows = rendered_rows(&buffer);
        assert!(rows[0].starts_with("▌ PR REPLAY"));
        assert!(rows[0].ends_with("01 / 01"));
        assert!(rows.iter().any(|row| row.contains("AUTHOR")));
        assert!(rows.iter().any(|row| row.contains("LOCAL OUTBOX")));
        assert!(rows
            .iter()
            .any(|row| row.contains("nothing sent to GitHub")));
        assert!(rows.iter().any(|row| row.contains("▶ INLINE COMMENT")));
        assert!(rows
            .iter()
            .any(|row| row.contains("src/editor/rendering.rs:11-12")));
        assert!(rows.iter().any(|row| row.contains("RIGHT")));
        assert!(rows
            .iter()
            .any(|row| row.contains("Please test the original viewport boundary.")));
        assert!(rows[height - 1].contains("[h/l]"));
        assert!(rows[height - 1].contains("[c]"));
        assert!(rows[height - 1].contains("[r]"));
        assert_eq!(
            replay_outbox_selected_row(&state, width),
            replay_outbox_lines(&state, width)
                .iter()
                .position(|line| {
                    line.spans
                        .first()
                        .is_some_and(|span| span.text.starts_with('▶'))
                })
                .unwrap(),
        );
    }

    #[test]
    fn native_outbox_keeps_the_return_action_pinned_while_long_drafts_scroll() {
        let mut replay = model();
        replay.review_role = Some(ReplayReviewRole::Reviewer);
        replay.head_commit = "b".repeat(40);
        replay.view = ReplayPanelView::Outbox;
        replay.drafts = (0..8)
            .map(|index| {
                outbox_draft(
                    ReplayReviewDraftKind::InlineComment,
                    &format!("Review draft {index} remains pinned to the original source."),
                )
            })
            .collect();
        replay.draft_count = replay.drafts.len();
        let state = ReplayPanelState::parse(&serde_json::to_string(&replay).unwrap()).unwrap();
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let width = 46;
        let height = 12;
        let mut top = RenderBuffer::new(width, height, &theme.style);
        let mut scrolled = RenderBuffer::new(width, height, &theme.style);
        let max_scroll = replay_content_line_count(&state, width)
            .saturating_sub(replay_visible_rows(&state, width, height));

        render_replay_panel(
            &mut top,
            &state,
            Point::new(0, 0),
            width,
            height,
            ReplayPanelViewport {
                scroll: 0,
                focused: true,
            },
            &theme,
        );
        render_replay_panel(
            &mut scrolled,
            &state,
            Point::new(0, 0),
            width,
            height,
            ReplayPanelViewport {
                scroll: max_scroll,
                focused: true,
            },
            &theme,
        );

        let top_rows = rendered_rows(&top);
        let scrolled_rows = rendered_rows(&scrolled);
        assert!(max_scroll > 0);
        assert!(top_rows.iter().any(|row| row.contains("LOCAL OUTBOX")));
        assert!(scrolled_rows
            .iter()
            .any(|row| row.contains("Review draft 7")));
        assert_eq!(top_rows[height - 1], scrolled_rows[height - 1]);
        assert!(scrolled_rows[height - 1].contains("[r]"));
    }

    #[test]
    fn narrow_native_outbox_preserves_original_filename_line_range_and_diff_side() {
        let mut replay = model();
        replay.review_role = Some(ReplayReviewRole::Author);
        replay.head_commit = "b".repeat(40);
        replay.view = ReplayPanelView::Outbox;
        let mut draft = outbox_draft(
            ReplayReviewDraftKind::InlineComment,
            "Keep the original replay notification bounded.",
        );
        let path = std::path::PathBuf::from(
            "codex-rs/app-server/src/request_processors/token_usage_replay.rs",
        );
        draft.path = Some(path.clone());
        draft.anchor.as_mut().unwrap().path = path;
        draft.anchor.as_mut().unwrap().start_line = 84;
        draft.anchor.as_mut().unwrap().end_line = 86;
        replay.drafts = vec![draft];
        replay.draft_count = replay.drafts.len();
        let state = ReplayPanelState::parse(&serde_json::to_string(&replay).unwrap()).unwrap();
        let source = replay_outbox_lines(&state, /*width*/ 46)
            .into_iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
            })
            .find(|line| line.contains("token_usage_replay.rs"))
            .expect("retain the original changed filename in a narrow pane");

        assert!(source.starts_with('…'));
        assert!(source.contains("token_usage_replay.rs:84-86"));
        assert!(source.ends_with("RIGHT"));
    }

    #[test]
    fn nonauthor_pull_request_header_is_explicitly_marked_as_review_only() {
        let mut replay = model();
        replay.review_role = Some(ReplayReviewRole::Reviewer);
        replay.head_commit = "b".repeat(40);
        let state = ReplayPanelState::parse(&serde_json::to_string(&replay).unwrap()).unwrap();
        let metadata = replay_header_lines(&state, /*width*/ 64)[0]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();

        assert!(metadata.contains("#482 · @original-author"));
        assert!(metadata.ends_with("REVIEW"));
        assert!(!metadata.contains("AUTHOR"));
    }

    #[test]
    fn structured_replay_panel_refuses_unpinned_or_truncated_original_head_identity() {
        let mut replay = model();
        replay.review_role = Some(ReplayReviewRole::Author);
        replay.head_commit = "bbbbbbb".to_string();

        assert!(ReplayPanelState::parse(&serde_json::to_string(&replay).unwrap()).is_none());
    }

    #[test]
    fn original_pull_request_title_is_visible_above_the_current_change() {
        let mut replay = model();
        replay.title = "feat(tui): paginate session history by scrollback budget".to_string();
        let state = ReplayPanelState::parse(&serde_json::to_string(&replay).unwrap()).unwrap();
        let lines = replay_header_lines(&state, /*width*/ 64)
            .into_iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let title = lines
            .iter()
            .position(|line| line == "feat(tui): paginate session history by scrollback budget")
            .expect("original pull request title");
        let current_change = lines
            .iter()
            .position(|line| line.starts_with("CURRENT CHANGE"))
            .expect("current learning step");

        assert_eq!(title, 2);
        assert!(title < current_change);
    }

    #[test]
    fn local_branch_review_is_not_presented_as_a_fictional_pull_request() {
        let mut replay = model();
        replay.pull_request = 0;
        replay.author = "local".to_string();
        replay.branch = "feature/replay".to_string();
        let state = ReplayPanelState::parse(&serde_json::to_string(&replay).unwrap()).unwrap();
        let lines = replay_header_lines(&state, /*width*/ 46);
        let metadata = lines[0]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();
        let branch = lines[1]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();

        assert!(metadata.starts_with("LOCAL BRANCH"));
        assert!(!metadata.contains("#0"));
        assert_eq!(branch, "feature/replay");
    }

    #[test]
    fn focused_replay_title_uses_a_structural_marker_and_theme_foreground() {
        let state = state();
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let mut focused = RenderBuffer::new(/*width*/ 46, /*height*/ 1, &theme.style);
        let mut unfocused = RenderBuffer::new(/*width*/ 46, /*height*/ 1, &theme.style);

        render_replay_panel_title(
            &mut focused,
            &state,
            "PR REPLAY",
            Point::new(0, 0),
            /*width*/ 46,
            /*focused*/ true,
            &theme,
        );
        render_replay_panel_title(
            &mut unfocused,
            &state,
            "PR REPLAY",
            Point::new(0, 0),
            /*width*/ 46,
            /*focused*/ false,
            &theme,
        );

        assert!(rendered_rows(&focused)[0].starts_with("▌ PR REPLAY"));
        assert!(rendered_rows(&focused)[0].ends_with("01 / 05"));
        assert!(rendered_rows(&unfocused)[0].starts_with("PR REPLAY"));
        assert_eq!(
            focused.cells[0].style.fg,
            theme.colors.get("editorCursor.foreground").copied(),
        );
        assert_eq!(
            unfocused.cells[0].style.fg,
            theme.colors.get("sideBarTitle.foreground").copied(),
        );
    }

    #[test]
    fn step_focus_caret_is_independent_of_semantic_completion() {
        let mut replay = model();
        replay.completions = vec![ReplayPanelCompletion {
            index: 0,
            completion: "manually reconstructed".to_string(),
        }];
        let state = ReplayPanelState::parse(&serde_json::to_string(&replay).unwrap()).unwrap();
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let width = 46;
        let height = 35;
        let mut focused = RenderBuffer::new(width, height, &theme.style);
        let mut unfocused = RenderBuffer::new(width, height, &theme.style);

        render_replay_panel(
            &mut focused,
            &state,
            Point::new(0, 0),
            width,
            height,
            ReplayPanelViewport {
                scroll: 0,
                focused: true,
            },
            &theme,
        );
        render_replay_panel(
            &mut unfocused,
            &state,
            Point::new(0, 0),
            width,
            height,
            ReplayPanelViewport {
                scroll: 0,
                focused: false,
            },
            &theme,
        );

        let focused_rows = rendered_rows(&focused);
        let unfocused_rows = rendered_rows(&unfocused);
        assert!(focused_rows.iter().any(|row| row.starts_with("▶ ✓ 01 ")));
        assert!(unfocused_rows.iter().any(|row| row.starts_with("  ✓ 01 ")));
        assert!(!unfocused_rows.iter().any(|row| row.starts_with('▶')));

        let layout = ReplayPanelLayout::calculate(&state, width, height);
        let row = layout.header_rows + layout.diff_rows + layout.change_gap_rows + 1;
        assert_ne!(
            focused.cells[row * width].style.bg,
            unfocused.cells[row * width].style.bg,
        );
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
                ReplayPanelViewport {
                    scroll: 0,
                    focused: true,
                },
                &theme,
            );

            let rows = rendered_rows(&buffer);
            let changes_row = layout.header_rows + layout.diff_rows + layout.change_gap_rows;
            let footer = &rows[height - 1];
            assert!(rows[changes_row].starts_with("CHANGES"));
            assert_eq!(layout.change_gap_rows, 1);
            assert!(rows[changes_row - 1].trim().is_empty());
            for shortcut in ["[i]", "[v]", "[a]", "[u]", "[h/l]", "[?]"] {
                assert!(
                    footer.contains(shortcut),
                    "missing {shortcut} at {width} columns"
                );
            }
            assert!(!footer.contains("… +"));
        }
    }
}
