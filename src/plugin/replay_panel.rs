//! Structured, editor-native presentation for source-backed PR Replay panels.
//!
//! Replay retains the original unified patch while projecting its old and new
//! source independently for Tree-sitter highlighting. The change list remains
//! pinned above an independently scrollable hunk and a compact status footer.

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
        parse_patch, GitObjectId, ReplayDemoStep, ReplayDraftState, ReplayLimits,
        ReplayReviewDraft, ReplayReviewDraftKind, ReplayReviewReceipt, ReplayReviewRole,
    },
    theme::{SelectionForegroundPriority, Style, Theme},
    ui::{ActionBar, ActionBarRole, ActionPriority, UiAction},
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
    pub(crate) author_workspace_available: bool,
    #[serde(default)]
    pub(crate) author_workspace_root: String,
    #[serde(default)]
    pub(crate) author_workspace_branch: String,
    #[serde(default)]
    pub(crate) draft_count: usize,
    #[serde(default)]
    pub(crate) drafts: Vec<ReplayReviewDraft>,
    #[serde(default)]
    pub(crate) receipts: Vec<ReplayReviewReceipt>,
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
    pub(crate) rationale_expanded: bool,
    #[serde(default)]
    pub(crate) horizontal_offset: usize,
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
            || model.receipts.len() > limits.max_steps
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
    pub(super) change_rows: usize,
    pub(super) change_gap_rows: usize,
    pub(super) current_change_rows: usize,
    pub(super) current_change_gap_rows: usize,
    pub(super) rationale_rows: usize,
    pub(super) status_rows: usize,
    pub(super) source_rows: usize,
    pub(super) diff_rows: usize,
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
                change_rows: 0,
                change_gap_rows: 0,
                current_change_rows: 0,
                current_change_gap_rows: 0,
                rationale_rows: 0,
                status_rows: 0,
                source_rows: 0,
                diff_rows: 0,
                footer_rows: 0,
            };
        }

        let footer_rows = 1;
        let content_height = available_height.saturating_sub(footer_rows);
        let section_spacing = usize::from(available_height >= 28);
        let preferred_header = replay_pinned_header_lines(state, width)
            .len()
            .saturating_add(section_spacing);
        let minimum_header = preferred_header.min(if content_height >= 8 { 3 } else { 1 });
        let source_rows =
            usize::from(!state.document.lines.is_empty() && content_height > minimum_header);
        let current_change_rows = if content_height >= 14 {
            replay_current_change_lines(state, width)
                .len()
                .min(if content_height >= 22 {
                    4
                } else if content_height >= 18 {
                    3
                } else {
                    2
                })
        } else {
            0
        };
        let current_change_gap_rows =
            usize::from(current_change_rows > 0 && available_height >= 33);
        let status_rows = usize::from(
            content_height
                >= minimum_header
                    .saturating_add(source_rows)
                    .saturating_add(current_change_rows)
                    .saturating_add(current_change_gap_rows)
                    .saturating_add(3),
        );
        let preferred_rationale_rows = if content_height >= 14 {
            2
        } else if content_height >= 8 {
            1
        } else {
            0
        };
        let mut rationale_rows = preferred_rationale_rows.min(
            content_height
                .saturating_sub(minimum_header)
                .saturating_sub(source_rows)
                .saturating_sub(current_change_rows)
                .saturating_sub(current_change_gap_rows)
                .saturating_sub(status_rows),
        );
        let minimum_diff = state
            .document
            .lines
            .len()
            .min(if content_height >= 14 { 6 } else { 1 })
            .min(
                content_height
                    .saturating_sub(minimum_header)
                    .saturating_sub(source_rows)
                    .saturating_sub(current_change_rows)
                    .saturating_sub(current_change_gap_rows)
                    .saturating_sub(rationale_rows)
                    .saturating_sub(status_rows),
            );
        let change_capacity = content_height
            .saturating_sub(minimum_header)
            .saturating_sub(minimum_diff)
            .saturating_sub(source_rows)
            .saturating_sub(current_change_rows)
            .saturating_sub(current_change_gap_rows)
            .saturating_sub(rationale_rows)
            .saturating_sub(status_rows)
            .saturating_sub(section_spacing);
        let preferred_change_rows = if available_height >= 33 {
            5
        } else if available_height >= 23 {
            4
        } else {
            3
        };
        let change_rows = if change_capacity >= 2 {
            state
                .model
                .steps
                .len()
                .min(preferred_change_rows)
                .min(change_capacity.saturating_sub(1))
        } else {
            0
        };
        let change_gap_rows = if change_rows > 0 { section_spacing } else { 0 };
        let changes_height = usize::from(change_rows > 0)
            .saturating_add(change_rows)
            .saturating_add(change_gap_rows)
            .saturating_add(current_change_rows)
            .saturating_add(current_change_gap_rows)
            .saturating_add(rationale_rows)
            .saturating_add(status_rows)
            .saturating_add(source_rows);
        let remaining = content_height.saturating_sub(changes_height);
        let header_rows = preferred_header.min(remaining.saturating_sub(minimum_diff));
        let mut diff_rows = state
            .document
            .lines
            .len()
            .min(remaining.saturating_sub(header_rows));

        if state.model.rationale_expanded {
            let requested_rows = replay_rationale_lines(state, width).len();
            let additional_rows = requested_rows.saturating_sub(rationale_rows);
            let used_rows = header_rows
                .saturating_add(usize::from(change_rows > 0))
                .saturating_add(change_rows)
                .saturating_add(change_gap_rows)
                .saturating_add(current_change_rows)
                .saturating_add(current_change_gap_rows)
                .saturating_add(rationale_rows)
                .saturating_add(status_rows)
                .saturating_add(source_rows)
                .saturating_add(diff_rows)
                .saturating_add(footer_rows);
            let unallocated_rows = available_height.saturating_sub(used_rows);
            let from_unallocated = additional_rows.min(unallocated_rows);
            let from_diff = additional_rows
                .saturating_sub(from_unallocated)
                .min(diff_rows.saturating_sub(1));
            rationale_rows = rationale_rows
                .saturating_add(from_unallocated)
                .saturating_add(from_diff);
            diff_rows = diff_rows.saturating_sub(from_diff);
        }

        Self {
            header_rows,
            change_rows,
            change_gap_rows,
            current_change_rows,
            current_change_gap_rows,
            rationale_rows,
            status_rows,
            source_rows,
            diff_rows,
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
    let title = if state.model.view == ReplayPanelView::Guide {
        format!("{title} · {}", state.model.mode.label())
    } else {
        title
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
        String::new()
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

    let mut header = replay_pinned_header_lines(state, width);
    if layout.header_rows < header.len() && layout.header_rows > 0 {
        header.truncate(layout.header_rows);
        if let Some(line) = header.iter_mut().rev().find(|line| !line.is_empty()) {
            let text = line
                .spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>();
            let style = line
                .spans
                .first()
                .map_or(TextPanelSpanStyle::Text, |span| span.style);
            *line = RenderedTextLine::plain(
                truncate_display_width_with_marker(
                    &format!("{text}…"),
                    width,
                    "…",
                    TruncationSide::Right,
                ),
                style,
            );
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

    let changes_top = position.y.saturating_add(layout.header_rows);
    if layout.change_rows > 0 {
        render_change_heading(
            buffer,
            state,
            position.x,
            changes_top,
            width,
            layout.change_rows,
            theme,
        );
        let first = replay_change_window_start(state, layout.change_rows);
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

    let current_change_top = changes_top
        .saturating_add(usize::from(layout.change_rows > 0))
        .saturating_add(layout.change_rows)
        .saturating_add(layout.change_gap_rows);
    let mut current_change = replay_current_change_lines(state, width);
    if layout.current_change_rows > 0 && current_change.len() > layout.current_change_rows {
        current_change.truncate(layout.current_change_rows);
        if let Some(last) = current_change.last_mut() {
            let text = last
                .spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>();
            *last = RenderedTextLine::plain(
                truncate_display_width_with_marker(
                    &format!("{text}…"),
                    width,
                    "…",
                    TruncationSide::Right,
                ),
                TextPanelSpanStyle::Strong,
            );
        }
    }
    for (offset, line) in current_change
        .iter()
        .take(layout.current_change_rows)
        .enumerate()
    {
        render_text_spans_on_surface(
            buffer,
            position.x,
            current_change_top.saturating_add(offset),
            width,
            line,
            theme,
            &theme.style,
        );
    }

    let rationale_top = current_change_top
        .saturating_add(layout.current_change_rows)
        .saturating_add(layout.current_change_gap_rows);
    for (offset, line) in replay_rationale_lines(state, width)
        .iter()
        .take(layout.rationale_rows)
        .enumerate()
    {
        render_text_spans_on_surface(
            buffer,
            position.x,
            rationale_top.saturating_add(offset),
            width,
            line,
            theme,
            &theme.style,
        );
    }

    let status_top = rationale_top.saturating_add(layout.rationale_rows);
    if layout.status_rows > 0 {
        render_replay_footer_status(buffer, &state.model, position.x, status_top, width, theme);
    }

    let source_top = status_top.saturating_add(layout.status_rows);
    if layout.source_rows > 0 {
        let hidden_above = viewport.scroll.min(state.document.lines.len());
        let hidden_below = state
            .document
            .lines
            .len()
            .saturating_sub(viewport.scroll.saturating_add(layout.diff_rows));
        let source = if let Some(step) = state.model.current_step() {
            let mut details = Vec::new();
            if let Some((index, count)) = state
                .model
                .current_file_position()
                .filter(|(_, count)| *count > 1)
            {
                details.push(if width >= 56 {
                    format!("file {index} of {count}")
                } else {
                    format!("{index}/{count}")
                });
            }
            if hidden_above > 0 {
                details.push(if width >= 64 {
                    format!("{hidden_above} above")
                } else {
                    format!("↑{hidden_above}")
                });
            }
            if hidden_below > 0 {
                details.push(if width >= 64 {
                    format!("{hidden_below} below")
                } else {
                    format!("↓{hidden_below}")
                });
            }
            if width >= 36 && (hidden_above > 0 || hidden_below > 0) {
                details.push("^D/^U".to_string());
            }
            let gutter_width = if width >= 72 { 13 } else { 7 };
            let code_width = width.saturating_sub(gutter_width);
            if width >= 56
                && state
                    .document
                    .lines
                    .iter()
                    .any(|line| line.kind != "hunk" && display_width(&line.text) > code_width)
            {
                details.push("H/L pan".to_string());
            }
            let prefix = if width >= 38 {
                "ORIGINAL HUNK · "
            } else {
                "ORIGINAL · "
            };
            let basename = step.path.rsplit('/').next().unwrap_or(&step.path);
            let minimum_path_width =
                display_width(basename).min(width.saturating_sub(display_width(prefix)));
            while !details.is_empty() {
                let candidate = details.join(" · ");
                let available = width
                    .saturating_sub(display_width(prefix))
                    .saturating_sub(display_width(&candidate))
                    .saturating_sub(1);
                if available >= minimum_path_width {
                    break;
                }
                details.pop();
            }
            let details = details.join(" · ");
            let path_width = width
                .saturating_sub(display_width(prefix))
                .saturating_sub(display_width(&details))
                .saturating_sub(1);
            let path = truncate_display_width_with_marker(
                &step.path,
                path_width,
                "…",
                TruncationSide::Left,
            );
            aligned_line(
                &format!("{prefix}{path}"),
                TextPanelSpanStyle::Link,
                &details,
                TextPanelSpanStyle::Muted,
                None,
                width,
            )
        } else {
            RenderedTextLine::plain(String::new(), TextPanelSpanStyle::Text)
        };
        render_text_spans_on_surface(
            buffer,
            position.x,
            source_top,
            width,
            &source,
            theme,
            &theme.style,
        );
    }

    let diff_top = source_top.saturating_add(layout.source_rows);
    let highlights = highlight_document(Some(&state.document), theme);
    let intraline = replay_intraline_highlights(&state.document, theme);
    for (offset, ((line, spans), changed_spans)) in state
        .document
        .lines
        .iter()
        .zip(highlights.iter())
        .zip(intraline.iter())
        .skip(viewport.scroll)
        .take(layout.diff_rows)
        .enumerate()
    {
        render_replay_diff_line(
            buffer,
            (position.x, diff_top.saturating_add(offset), width),
            line,
            spans,
            changed_spans,
            state.model.horizontal_offset,
            theme,
        );
    }

    let actions = replay_actions(&state.model, width);
    render_replay_action_bar(
        buffer,
        position.x,
        position.y.saturating_add(height.saturating_sub(1)),
        width,
        &actions,
        theme,
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
            ReplayReviewRole::Author if !model.author_workspace_root.is_empty() => {
                "YOUR PR · PR HEAD"
            }
            ReplayReviewRole::Author => "YOUR PR",
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

fn replay_pinned_header_lines(state: &ReplayPanelState, width: usize) -> Vec<RenderedTextLine> {
    let model = &state.model;
    let identity = if model.pull_request == 0 {
        "LOCAL BRANCH".to_string()
    } else {
        format!("#{}", model.pull_request)
    };
    let title = model.title.trim();
    let show_title = !title.is_empty()
        && title != model.branch
        && title.strip_prefix("Replay ") != Some(model.branch.as_str());
    let headline = if show_title {
        format!("{identity} · {title}")
    } else {
        identity
    };
    let role = match model.review_role.filter(|_| model.pull_request > 0) {
        Some(ReplayReviewRole::Author) if !model.author_workspace_root.is_empty() => {
            "YOUR PR · PR HEAD"
        }
        Some(ReplayReviewRole::Author) => "YOUR PR",
        Some(ReplayReviewRole::Reviewer) => "REVIEW",
        None => "",
    };
    let mut source = if model.pull_request == 0 {
        model.branch.clone()
    } else {
        format!("@{} · {}", model.author, model.branch)
    };
    if !model.head_commit.is_empty() {
        source.push_str(" · ");
        source.extend(model.head_commit.chars().take(7));
    }
    let mut progress = format!("{}/{} reviewed", model.reviewed_count(), model.steps.len());
    if !model.notes.is_empty() {
        let suffix = if model.notes.len() == 1 {
            "note"
        } else {
            "notes"
        };
        progress.push_str(&format!(" · {} {suffix}", model.notes.len()));
    }
    if model.draft_count > 0 {
        let suffix = if model.draft_count == 1 {
            "draft"
        } else {
            "drafts"
        };
        progress.push_str(&format!(" · {} {suffix}", model.draft_count));
    }

    vec![
        aligned_line(
            &headline,
            TextPanelSpanStyle::Strong,
            role,
            TextPanelSpanStyle::Heading,
            None,
            width,
        ),
        aligned_line(
            &source,
            TextPanelSpanStyle::Muted,
            &progress,
            TextPanelSpanStyle::Muted,
            None,
            width,
        ),
    ]
}

/// Keeps the complete selected original change readable outside the compact step list.
fn replay_current_change_lines(state: &ReplayPanelState, width: usize) -> Vec<RenderedTextLine> {
    let Some(step) = state.model.current_step() else {
        return Vec::new();
    };
    if width == 0 {
        return Vec::new();
    }

    let position = format!(
        "{:02} / {:02}",
        state.model.index.saturating_add(1),
        state.model.steps.len(),
    );
    let mut lines = vec![aligned_line(
        "ORIGINAL CHANGE",
        TextPanelSpanStyle::Heading,
        &position,
        TextPanelSpanStyle::Muted,
        None,
        width,
    )];
    let title = wrap_plain_text(&step.title, width, TextPanelSpanStyle::Strong);
    let truncated = title.len() > 3;
    lines.extend(title.into_iter().take(3));
    if let Some(last) = lines.last_mut().filter(|_| truncated) {
        let text = last
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();
        *last = RenderedTextLine::plain(
            truncate_display_width_with_marker(
                &format!("{text}…"),
                width,
                "…",
                TruncationSide::Right,
            ),
            TextPanelSpanStyle::Strong,
        );
    }
    lines
}

fn replay_rationale_lines(state: &ReplayPanelState, width: usize) -> Vec<RenderedTextLine> {
    let Some(step) = state.model.current_step() else {
        return Vec::new();
    };
    if width == 0 {
        return Vec::new();
    }

    let (label, rationale, body_style) = if state.model.hint_visible && !step.hint.is_empty() {
        ("HINT  ", step.hint.as_str(), TextPanelSpanStyle::Quote)
    } else {
        ("WHY   ", step.why.as_str(), TextPanelSpanStyle::Muted)
    };
    let label_width = display_width(label).min(width);
    let text_width = width.saturating_sub(label_width).max(1);
    let body = wrap_plain_text(rationale, text_width, body_style);
    let visible_rows = if state.model.rationale_expanded {
        body.len()
    } else {
        2
    };
    let overflow = body.len() > visible_rows;

    body.into_iter()
        .take(visible_rows)
        .enumerate()
        .map(|(index, line)| {
            let prefix = if index == 0 {
                truncate_display_width(label, width)
            } else {
                " ".repeat(label_width)
            };
            let mut spans = vec![RenderedTextSpan {
                text: prefix,
                style: if index == 0 {
                    TextPanelSpanStyle::Heading
                } else {
                    TextPanelSpanStyle::Text
                },
                syntax_style: None,
                link: None,
            }];
            let text = line
                .spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>();
            let text = if index.saturating_add(1) == visible_rows && overflow {
                truncate_display_width_with_marker(
                    &format!("{text}…"),
                    text_width,
                    "…",
                    TruncationSide::Right,
                )
            } else {
                truncate_display_width(&text, text_width)
            };
            if width > label_width {
                spans.push(RenderedTextSpan {
                    text,
                    style: body_style,
                    syntax_style: None,
                    link: None,
                });
            }
            RenderedTextLine { spans }
        })
        .collect()
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

/// Keep visible changes stable until the selected step crosses a list edge.
pub(super) fn replay_change_window_start(state: &ReplayPanelState, visible_rows: usize) -> usize {
    if visible_rows == 0 {
        return 0;
    }

    state
        .model
        .index
        .saturating_div(visible_rows)
        .saturating_mul(visible_rows)
        .min(state.model.steps.len().saturating_sub(visible_rows))
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
    let count = if model.receipts.is_empty() && model.drafts.len() == 1 {
        "1 draft".to_string()
    } else if model.receipts.is_empty() {
        format!("{} drafts", model.drafts.len())
    } else {
        format!(
            "{} drafts · {} posted",
            model.drafts.len(),
            model.receipts.len()
        )
    };
    lines.push(aligned_line(
        "LOCAL OUTBOX",
        TextPanelSpanStyle::Heading,
        &count,
        TextPanelSpanStyle::Muted,
        None,
        width,
    ));
    let privacy = if model.receipts.is_empty() {
        "Local only · nothing sent to GitHub"
    } else {
        "Local drafts stay private · posted comments have verified receipts"
    };
    lines.extend(wrap_plain_text(
        privacy,
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
        let publication = if draft.state == ReplayDraftState::Submitted {
            "POSTED"
        } else {
            "LOCAL"
        };
        lines.push(aligned_line(
            &label,
            if index == model.outbox_index {
                TextPanelSpanStyle::Strong
            } else {
                TextPanelSpanStyle::Text
            },
            publication,
            if draft.state == ReplayDraftState::Submitted {
                TextPanelSpanStyle::Heading
            } else {
                TextPanelSpanStyle::Muted
            },
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
        render_replay_footer_status(
            buffer,
            &state.model,
            position.x,
            position
                .y
                .saturating_add(height.saturating_sub(footer_rows)),
            width,
            theme,
        );
    }
    if footer_rows > 0 {
        let actions = replay_outbox_actions(&state.model);
        render_replay_action_bar(
            buffer,
            position.x,
            position.y.saturating_add(height.saturating_sub(1)),
            width,
            &actions,
            theme,
        );
    }
}

fn replay_outbox_actions(model: &ReplayPanelModel) -> Vec<UiAction> {
    let has_drafts = !model.drafts.is_empty();
    let selected_is_local = model
        .drafts
        .get(model.outbox_index)
        .is_some_and(|draft| draft.state == ReplayDraftState::Local);
    let can_publish = model.pull_request > 0
        && model.review_role.is_some()
        && !model.head_commit.is_empty()
        && model.drafts.iter().any(|draft| {
            draft.state == ReplayDraftState::Local && draft.kind != ReplayReviewDraftKind::CodeFix
        });
    let mut actions = Vec::new();
    if has_drafts {
        actions.push(
            UiAction::new("navigate_draft", "j/k", "Select")
                .with_priority(ActionPriority::Essential)
                .with_compact_label("Item"),
        );
    }
    actions.push(
        UiAction::new("outbox", "r", "Return")
            .with_priority(ActionPriority::Essential)
            .with_compact_label("Back"),
    );
    actions.push(
        UiAction::new("comment", "c", "Comment")
            .with_priority(ActionPriority::Essential)
            .with_compact_label("Note"),
    );
    if selected_is_local {
        actions.push(
            UiAction::new("edit_draft", "e", "Edit")
                .with_priority(ActionPriority::Primary)
                .with_compact_label("Edit"),
        );
        actions.push(
            UiAction::new("discard_draft", "d", "Discard")
                .with_priority(ActionPriority::Primary)
                .with_compact_label("Del"),
        );
    }
    actions.push(
        UiAction::new("summary", "s", "Summary")
            .with_priority(if has_drafts {
                ActionPriority::Secondary
            } else {
                ActionPriority::Essential
            })
            .with_compact_label("Sum"),
    );
    if has_drafts || !model.notes.is_empty() {
        actions.push(
            UiAction::new("save_review", "S", "Save")
                .with_priority(ActionPriority::Essential)
                .with_compact_label("Save"),
        );
    }
    if can_publish {
        actions.push(
            UiAction::new("publish_review", "P", "Publish")
                .with_priority(ActionPriority::Essential)
                .with_compact_label("Post"),
        );
    }
    actions.push(
        UiAction::new("load_review", "L", "Load")
            .with_priority(if has_drafts {
                ActionPriority::Secondary
            } else {
                ActionPriority::Essential
            })
            .with_compact_label("Load"),
    );
    if model.author_workspace_available {
        actions.push(
            UiAction::new("original_workspace", "W", "PR Head")
                .with_priority(ActionPriority::Primary)
                .with_compact_label("Head"),
        );
    }
    if model.review_role == Some(ReplayReviewRole::Author) {
        actions.push(
            UiAction::new("fix", "F", "Fix")
                .with_priority(ActionPriority::Secondary)
                .with_compact_label("Fix"),
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
    rect: (usize, usize, usize),
    line: &WorkspaceDocumentLine,
    highlights: &[crate::editor::StyleInfo],
    intraline: &[crate::editor::StyleInfo],
    horizontal_offset: usize,
    theme: &Theme,
) {
    let (x, y, width) = rect;
    if width == 0 || y >= buffer.height {
        return;
    }
    let line_style = diff_line_style(&line.kind, theme);
    buffer.set_text(x, y, &fit_display_width("", width), &line_style);

    if line.kind == "hunk" {
        let style = theme.ui_style.muted.with_bg(line_style.bg);
        let heading = line
            .text
            .strip_prefix("@@")
            .and_then(|range| range.find("@@"))
            .map_or(line.text.as_str(), |end| &line.text[..end + 4]);
        buffer.set_text(
            x.saturating_add(1),
            y,
            &truncate_display_width(heading, width.saturating_sub(1)),
            &style,
        );
        return;
    }

    let wide_gutter = width >= 72;
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
    let gutter_style = theme.ui_style.muted.with_bg(line_style.bg);
    buffer.set_text(
        x,
        y,
        &truncate_display_width(&gutter, gutter_width),
        &gutter_style,
    );
    if marker != " " {
        let marker_column = if wide_gutter { 10 } else { 5 };
        if marker_column < width {
            buffer.set_text(
                x.saturating_add(marker_column),
                y,
                marker,
                &change_kind_style(&line.kind, theme).with_bg(line_style.bg),
            );
        }
    }

    let code_width = width.saturating_sub(gutter_width);
    if code_width == 0 {
        return;
    }
    let code_x = x.saturating_add(gutter_width);
    let hidden_left = horizontal_offset > 0
        && line
            .text
            .chars()
            .any(|character| !character.is_whitespace());
    let leading_marker_width = usize::from(hidden_left);
    let available_width = code_width.saturating_sub(leading_marker_width);
    let hidden_right =
        display_width(&line.text) > horizontal_offset.saturating_add(available_width);
    let visible_width = available_width.saturating_sub(usize::from(hidden_right));
    let visible = display_slice(&line.text, horizontal_offset, visible_width);
    let mut displayed = String::new();
    if hidden_left {
        displayed.push('‹');
    }
    displayed.push_str(&visible.text);
    if hidden_right {
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
        (
            code_x.saturating_add(leading_marker_width),
            y,
            visible_width,
        ),
        &line.text,
        &visible,
        highlights,
        &line_style,
    );
    render_syntax_overlays(
        buffer,
        (
            code_x.saturating_add(leading_marker_width),
            y,
            visible_width,
        ),
        &line.text,
        &visible,
        intraline,
        &line_style,
    );
}

/// Emphasize only original identifiers that actually differ inside a paired hunk.
fn replay_intraline_highlights(
    document: &WorkspaceDocument,
    theme: &Theme,
) -> Vec<Vec<crate::editor::StyleInfo>> {
    let mut highlights = (0..document.lines.len())
        .map(|_| Vec::new())
        .collect::<Vec<_>>();
    let mut index = 0;

    while index < document.lines.len() {
        if !matches!(document.lines[index].kind.as_str(), "removed" | "added") {
            index += 1;
            continue;
        }

        let start = index;
        while index < document.lines.len()
            && matches!(document.lines[index].kind.as_str(), "removed" | "added")
        {
            index += 1;
        }
        let group = &document.lines[start..index];
        let removed = group
            .iter()
            .filter(|line| line.kind == "removed")
            .flat_map(|line| {
                identifier_ranges(&line.text)
                    .into_iter()
                    .map(|(start, end)| &line.text[start..end])
            })
            .collect::<HashSet<_>>();
        let added = group
            .iter()
            .filter(|line| line.kind == "added")
            .flat_map(|line| {
                identifier_ranges(&line.text)
                    .into_iter()
                    .map(|(start, end)| &line.text[start..end])
            })
            .collect::<HashSet<_>>();
        if removed.is_empty() || added.is_empty() {
            continue;
        }

        for (offset, line) in group.iter().enumerate() {
            let opposite = if line.kind == "removed" {
                &added
            } else {
                &removed
            };
            let line_style = diff_line_style(&line.kind, theme);
            for (token_start, token_end) in identifier_ranges(&line.text) {
                if !opposite.contains(&line.text[token_start..token_end]) {
                    highlights[start + offset].push(crate::editor::StyleInfo {
                        start: token_start,
                        end: token_end,
                        style: Style {
                            fg: diff_foreground(&line.kind, theme)
                                .or(theme.ui_style.picker_prompt.fg),
                            bg: line_style.bg,
                            bold: true,
                            italic: false,
                        },
                    });
                }
            }
        }
    }

    highlights
}

fn identifier_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = None;
    for (offset, character) in text.char_indices() {
        let continues_identifier = character == '_' || character.is_ascii_alphanumeric();
        if continues_identifier {
            if start.is_none() && (character == '_' || character.is_ascii_alphabetic()) {
                start = Some(offset);
            }
        } else if let Some(identifier_start) = start.take() {
            ranges.push((identifier_start, offset));
        }
    }
    if let Some(identifier_start) = start {
        ranges.push((identifier_start, text.len()));
    }
    ranges
}

fn render_replay_action_bar(
    buffer: &mut RenderBuffer,
    x: usize,
    y: usize,
    width: usize,
    actions: &[UiAction],
    theme: &Theme,
) {
    let layout = ActionBar::new(actions).render(buffer, x, y, width, theme, &theme.style);
    let mut column = x;

    for span in layout.spans {
        if span.role == ActionBarRole::Key {
            let foreground = theme.ui_style.picker_prompt.fg.or(theme.style.fg);
            let key_style = Style {
                fg: foreground,
                bg: theme.style.bg,
                bold: true,
                italic: false,
            };
            buffer.set_text(column, y, &span.text, &key_style);
        } else if span.role == ActionBarRole::Overflow {
            buffer.set_text(
                column,
                y,
                &span.text,
                &theme.ui_style.muted.clone().with_bg(theme.style.bg),
            );
        }
        column = column.saturating_add(display_width(&span.text));
    }
}

fn render_replay_footer_status(
    buffer: &mut RenderBuffer,
    model: &ReplayPanelModel,
    x: usize,
    y: usize,
    width: usize,
    theme: &Theme,
) {
    if width == 0 || y >= buffer.height {
        return;
    }

    let message = if model.notice.trim().is_empty() {
        model
            .current_completion()
            .map(|completion| completion.completion.as_str())
    } else if model.notice.trim().starts_with("Review restored") {
        None
    } else {
        Some(model.notice.trim())
    };
    let Some(message) = message else {
        buffer.set_text(x, y, &" ".repeat(width), &theme.style);
        return;
    };

    let normalized = message.to_ascii_lowercase();
    let (marker, semantic_colors): (&str, &[&str]) = if [
        "failed",
        "could not",
        "cannot",
        "unable",
        "expired",
        "does not match",
        "not posted",
        "conflict",
    ]
    .iter()
    .any(|pattern| normalized.contains(pattern))
    {
        ("✕ ", &["errorForeground", "editorError.foreground"])
    } else if [
        "already",
        "choose",
        "first changed",
        "last changed",
        "still",
    ]
    .iter()
    .any(|pattern| normalized.contains(pattern))
    {
        (
            "! ",
            &["editorWarning.foreground", "list.warningForeground"],
        )
    } else if [
        "saved",
        "restored",
        "applied",
        "reconstructed",
        "undid",
        "published",
        "posted",
    ]
    .iter()
    .any(|pattern| normalized.contains(pattern))
    {
        ("✓ ", &["gitDecoration.addedResourceForeground"])
    } else {
        ("· ", &["editorInfo.foreground"])
    };
    let foreground = semantic_colors
        .iter()
        .find_map(|role| theme.colors.get(*role).copied())
        .or(theme.ui_style.picker_prompt.fg);
    let marker_style = Style {
        fg: foreground,
        bg: theme.style.bg,
        bold: true,
        italic: false,
    };
    let text_style = theme.ui_style.muted.clone().with_bg(theme.style.bg);
    buffer.set_text(x, y, &" ".repeat(width), &text_style);

    let end = x.saturating_add(width);
    let column = render_change_segment(buffer, x, y, end, marker, &marker_style);
    let single_line = message.split_whitespace().collect::<Vec<_>>().join(" ");
    let visible = truncate_display_width_with_marker(
        &single_line,
        end.saturating_sub(column),
        "…",
        TruncationSide::Right,
    );
    render_change_segment(buffer, column, y, end, &visible, &text_style);
}

fn render_change_heading(
    buffer: &mut RenderBuffer,
    state: &ReplayPanelState,
    x: usize,
    y: usize,
    width: usize,
    visible_rows: usize,
    theme: &Theme,
) {
    let first = replay_change_window_start(state, visible_rows);
    let hidden_above = first;
    let hidden_below = state
        .model
        .steps
        .len()
        .saturating_sub(first.saturating_add(visible_rows));
    let mut progress = Vec::new();
    if width >= 56 {
        if let Some((index, count)) = state
            .model
            .current_file_position()
            .filter(|(_, count)| *count > 1)
        {
            progress.push(format!("file {index} of {count}"));
        }
    }
    if hidden_above > 0 {
        progress.push(if width >= 56 {
            format!("{hidden_above} above")
        } else {
            format!("↑{hidden_above}")
        });
    }
    if hidden_below > 0 {
        progress.push(if width >= 56 {
            format!("{hidden_below} below")
        } else {
            format!("↓{hidden_below}")
        });
    }
    let details = if progress.is_empty() {
        "─".to_string()
    } else {
        format!("{} ─", progress.join(" · "))
    };
    let prefix = "─ CHANGES ";
    let fill_width = width
        .saturating_sub(display_width(prefix))
        .saturating_sub(display_width(&details))
        .saturating_sub(1);
    let heading = format!("{prefix}{}", "─".repeat(fill_width));
    let line = aligned_line(
        &heading,
        TextPanelSpanStyle::Heading,
        &details,
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
        theme.ensure_text_contrast(&theme.selected_style(
            &theme.style,
            &selection,
            SelectionForegroundPriority::Selection,
        ))
    } else {
        theme.style.clone()
    };
    buffer.set_text(x, y, &fit_display_width("", width), &row_style);

    let noted = state.model.notes.iter().any(|note| note.index == index)
        || state
            .model
            .drafts
            .iter()
            .any(|draft| draft.step_id.as_deref() == Some(step.id.as_str()));
    let marker = if completion.is_some_and(|entry| entry.completion == "automatically applied") {
        "⊕"
    } else if completion.is_some() {
        "✓"
    } else if noted {
        "✎"
    } else {
        "○"
    };
    let marker_style = if marker == "✓" {
        change_kind_style("add", theme)
    } else if matches!(marker, "⊕" | "✎") {
        theme.ui_style.picker_prompt.clone()
    } else {
        theme.ui_style.muted.clone()
    }
    .with_bg(row_style.bg);
    let marker_style = if active {
        theme.ensure_text_contrast(&marker_style)
    } else {
        marker_style
    };
    let number = format!(" {:02} ", index.saturating_add(1));

    let mut column = x;
    let caret = if focused && active { "▶ " } else { "  " };
    let caret_style = theme.ui_style.picker_prompt.clone().with_bg(row_style.bg);
    let caret_style = if active {
        theme.ensure_text_contrast(&caret_style)
    } else {
        caret_style
    };
    let number_style = if active {
        theme.ensure_text_contrast(&row_style)
    } else {
        theme.ui_style.muted.clone().with_bg(row_style.bg)
    };
    column = render_change_segment(buffer, column, y, x + width, caret, &caret_style);
    column = render_change_segment(buffer, column, y, x + width, marker, &marker_style);
    column = render_change_segment(buffer, column, y, x + width, &number, &number_style);
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
    let has_multiple_files = model
        .current_file_position()
        .is_some_and(|(_, count)| count > 1);
    let mut actions = vec![
        UiAction::new("navigate", "j/k", "Change")
            .with_priority(if width >= 39 {
                ActionPriority::Essential
            } else {
                ActionPriority::Primary
            })
            .with_compact_label("Chg"),
        UiAction::new("edit", "i", "Edit")
            .with_priority(ActionPriority::Essential)
            .with_compact_label("Edit"),
        UiAction::new(
            "validate",
            "v",
            if width >= 82 { "Check edit" } else { "Check" },
        )
        .with_priority(if width >= 46 {
            ActionPriority::Essential
        } else {
            ActionPriority::Primary
        })
        .with_compact_label("Check"),
        UiAction::new(
            "apply",
            "a",
            if width >= 82 { "Apply hunk" } else { "Apply" },
        )
        .with_priority(ActionPriority::Essential)
        .with_compact_label("Apply"),
        UiAction::new("undo", "u", "Undo")
            .with_priority(ActionPriority::Essential)
            .with_compact_label(if width >= 72 { "Undo" } else { "↶" }),
        UiAction::new("zoom", "z", "Zoom")
            .with_priority(if width >= 75 {
                ActionPriority::Essential
            } else {
                ActionPriority::Primary
            })
            .with_compact_label("Zoom"),
        UiAction::new("help", "?", "Help")
            .with_priority(ActionPriority::Essential)
            .with_compact_label("Help"),
    ];

    if model.pull_request > 0 && model.review_role.is_some() {
        actions.insert(
            actions.len().saturating_sub(1),
            UiAction::new("comment", "c", "Note")
                .with_priority(if width >= 60 {
                    ActionPriority::Essential
                } else {
                    ActionPriority::Secondary
                })
                .with_compact_label("Note"),
        );
        actions.insert(
            actions.len().saturating_sub(1),
            UiAction::new("outbox", "r", "Outbox")
                .with_priority(if width >= 58 {
                    ActionPriority::Primary
                } else {
                    ActionPriority::Secondary
                })
                .with_compact_label("Outbox"),
        );
    }

    if model.author_workspace_available {
        actions.insert(
            actions.len().saturating_sub(1),
            UiAction::new("original_workspace", "W", "Worktree")
                .with_priority(if width >= 72 {
                    ActionPriority::Essential
                } else {
                    ActionPriority::Secondary
                })
                .with_compact_label("Worktree"),
        );
    }

    if has_multiple_files {
        actions.insert(
            5,
            UiAction::new("navigate_file", "h/l", "File")
                .with_priority(if width >= 56 {
                    ActionPriority::Essential
                } else {
                    ActionPriority::Primary
                })
                .with_compact_label("File"),
        );
    }

    if model.steps.len() > model.reviewed_count().saturating_add(1) {
        actions.insert(
            actions.len().saturating_sub(1),
            UiAction::new("next_unreviewed", "n", "Next")
                .with_priority(if width >= 86 {
                    ActionPriority::Essential
                } else {
                    ActionPriority::Secondary
                })
                .with_compact_label("Next"),
        );
    }

    while ActionBar::new(&actions).layout(width).hidden_count() > 0 {
        let removable = actions
            .iter()
            .enumerate()
            .filter(|(_, action)| action.priority != ActionPriority::Essential)
            .max_by_key(|(index, action)| (action.priority, *index))
            .map(|(index, _)| index)
            .or_else(|| {
                [
                    "next_unreviewed",
                    "original_workspace",
                    "comment",
                    "navigate_file",
                    "navigate",
                    "validate",
                    "zoom",
                ]
                .into_iter()
                .find_map(|id| actions.iter().position(|action| action.id == id))
            });
        let Some(index) = removable else {
            break;
        };
        actions.remove(index);
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{replay::replay_demo_plan, theme::parse_vscode_theme, ui::ActionBarLayout};
    use similar::TextDiff;

    fn has_visible_key(layout: &ActionBarLayout, key: &str) -> bool {
        layout
            .spans
            .iter()
            .any(|span| span.role == ActionBarRole::Key && span.text == key)
    }

    fn assert_labeled_actions(layout: &ActionBarLayout) {
        for (index, span) in layout.spans.iter().enumerate() {
            if span.role != ActionBarRole::Key {
                continue;
            }
            assert!(
                layout.spans[index + 1..]
                    .iter()
                    .take_while(|candidate| {
                        !matches!(candidate.role, ActionBarRole::Key | ActionBarRole::Overflow)
                    })
                    .any(|candidate| {
                        candidate.role == ActionBarRole::Label && !candidate.text.trim().is_empty()
                    }),
                "Replay action {} must never be displayed without its label: {}",
                span.text,
                layout.text(),
            );
        }
    }

    fn model() -> ReplayPanelModel {
        let plan = replay_demo_plan().expect("source-backed demo plan");
        ReplayPanelModel {
            pull_request: plan.pull_request,
            author: plan.author,
            branch: plan.branch,
            review_role: None,
            head_commit: String::new(),
            author_workspace_available: false,
            author_workspace_root: String::new(),
            author_workspace_branch: String::new(),
            draft_count: 0,
            drafts: Vec::new(),
            receipts: Vec::new(),
            outbox_index: 0,
            view: ReplayPanelView::Guide,
            title: plan.title,
            index: 0,
            mode: ReplayPanelMode::Challenge,
            hint_visible: false,
            rationale_expanded: false,
            horizontal_offset: 0,
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
    fn selected_original_change_retains_its_complete_wrapped_title() {
        let mut replay = model();
        replay.steps[0].title = "Pass app_server to handle_backtrack_overlay_event".to_string();
        let expected_title = replay.steps[0].title.clone();
        let state = ReplayPanelState::parse(&serde_json::to_string(&replay).unwrap()).unwrap();
        let width = 32;
        let height = 30;
        let layout = ReplayPanelLayout::calculate(&state, width, height);
        let lines = replay_current_change_lines(&state, width);

        assert_eq!(layout.current_change_rows, 3);
        assert!(lines[0]
            .spans
            .iter()
            .any(|span| span.text.contains("ORIGINAL CHANGE")));
        let rendered_title = lines[1..]
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.text.as_str()))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(rendered_title, expected_title);

        let theme = parse_vscode_theme("themes/red.json").unwrap();
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
        assert!(rows.iter().any(|row| row.contains("ORIGINAL CHANGE")));
        assert!(rows
            .iter()
            .any(|row| row.contains("handle_backtrack_overlay_event")));
    }

    #[test]
    fn narrow_original_hunk_never_truncates_the_actual_filename() {
        let state = state();
        let theme = parse_vscode_theme("themes/red.json").unwrap();

        for width in [30, 38, 46, 62] {
            let height = 30;
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
            assert!(
                rows.iter()
                    .any(|row| { row.starts_with("ORIGINAL") && row.contains("rendering.rs") }),
                "the exact original filename must remain visible at {width} columns: {rows:?}",
            );
        }
    }

    #[test]
    fn real_length_change_title_uses_three_readable_lines_at_half_width() {
        let mut replay = model();
        replay.steps[0].title = concat!(
            "Add metadata_resume_id to ",
            "thread_resume_rejoins_running_paginated_thread_with_initial_page",
        )
        .to_string();
        let expected_title = replay.steps[0].title.clone();
        let state = ReplayPanelState::parse(&serde_json::to_string(&replay).unwrap()).unwrap();
        let width = 49;
        let height = 26;
        let layout = ReplayPanelLayout::calculate(&state, width, height);
        let lines = replay_current_change_lines(&state, width);

        assert_eq!(layout.current_change_rows, 4);
        let title = lines[1..]
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.text.as_str()))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            title.split_whitespace().collect::<String>(),
            expected_title.split_whitespace().collect::<String>(),
        );

        let theme = parse_vscode_theme("themes/red.json").unwrap();
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
        assert!(
            rows.iter().any(|row| row.contains("initial_page")),
            "the complete original symbol must remain visible: {rows:?}",
        );
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
            (/*x*/ 0, /*y*/ 0, /*width*/ 60),
            added,
            &highlights[index],
            &[],
            /*horizontal_offset*/ 0,
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
            (/*x*/ 0, /*y*/ 0, /*width*/ 25),
            &line,
            &[],
            &[],
            /*horizontal_offset*/ 0,
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
    fn horizontal_diff_panning_reveals_exact_original_changed_arguments() {
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let line = WorkspaceDocumentLine {
            id: "long-original-call".to_string(),
            text: "            .handle_backtrack_overlay_event(tui, app_server, event)".to_string(),
            kind: "added".to_string(),
            new_line: Some(1295),
            ..WorkspaceDocumentLine::default()
        };
        let original = line.text.clone();
        let mut initial = RenderBuffer::new(/*width*/ 36, /*height*/ 1, &theme.style);
        render_replay_diff_line(
            &mut initial,
            (/*x*/ 0, /*y*/ 0, /*width*/ 36),
            &line,
            &[],
            &[],
            /*horizontal_offset*/ 0,
            &theme,
        );
        let initial_text = rendered_rows(&initial)[0].clone();
        assert!(initial_text.ends_with('›'));
        assert!(!initial_text.contains("app_server"));

        let offset = line
            .text
            .find("app_server")
            .expect("exact added original argument")
            .saturating_sub(6);
        let mut panned = RenderBuffer::new(/*width*/ 36, /*height*/ 1, &theme.style);
        render_replay_diff_line(
            &mut panned,
            (/*x*/ 0, /*y*/ 0, /*width*/ 36),
            &line,
            &[],
            &[],
            offset,
            &theme,
        );
        let panned_text = &rendered_rows(&panned)[0];
        assert!(panned_text.contains('‹'));
        assert!(panned_text.contains("app_server"));
        assert_eq!(line.text, original);
    }

    #[test]
    fn changed_arguments_receive_intraline_highlighting_without_mutating_the_patch() {
        let mut replay = model();
        replay.steps[0] = source_step(
            /*ordinal*/ 1,
            "src/editor/rendering.rs",
            "Pass app_server to handle_backtrack_overlay_event",
            "let _ = self.handle_backtrack_overlay_event(tui, event).await?;\n",
            concat!(
                "let _ = self\n",
                "    .handle_backtrack_overlay_event(tui, app_server, event)\n",
                "    .await?;\n",
            ),
        );
        let original_patch = replay.steps[0].diff.clone();
        let state = ReplayPanelState::parse(&serde_json::to_string(&replay).unwrap()).unwrap();
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let highlights = replay_intraline_highlights(&state.document, &theme);
        let (index, line) = state
            .document
            .lines
            .iter()
            .enumerate()
            .find(|(_, line)| line.kind == "added" && line.text.contains("app_server"))
            .expect("exact original added argument");

        assert!(highlights[index]
            .iter()
            .any(|span| &line.text[span.start..span.end] == "app_server" && span.style.bold));
        assert_eq!(state.model.steps[0].diff, original_patch);
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
        assert!(layout.header_rows >= 2);
        assert!(layout.diff_rows >= 6);
        assert_eq!(layout.change_rows, 4);
        assert_eq!(layout.rationale_rows, 2);
        assert_eq!(layout.status_rows, 1);
        assert_eq!(layout.source_rows, 1);
        assert_eq!(layout.footer_rows, 1);
        assert!(
            layout.header_rows
                + usize::from(layout.change_rows > 0)
                + layout.change_rows
                + layout.change_gap_rows
                + layout.current_change_rows
                + layout.current_change_gap_rows
                + layout.rationale_rows
                + layout.status_rows
                + layout.source_rows
                + layout.diff_rows
                + layout.footer_rows
                <= 23,
        );
        assert!(layout.diff_rows <= state.document.lines.len());
    }

    #[test]
    fn responsive_changes_use_three_four_or_five_pinned_rows() {
        let state = state();

        for (width, height, expected_changes) in [(39, 22, 3), (49, 26, 4), (69, 38, 5)] {
            let layout = ReplayPanelLayout::calculate(&state, width, height);
            assert_eq!(
                layout.change_rows, expected_changes,
                "unexpected visible change count at {width}×{height}",
            );
            assert_eq!(layout.rationale_rows, 2);
            assert_eq!(layout.status_rows, 1);
            assert_eq!(layout.source_rows, 1);
            assert_eq!(layout.footer_rows, 1);
            assert!(layout.diff_rows >= 6);
        }
    }

    #[test]
    fn normal_height_adds_section_breathing_room_without_cramping_short_terminals() {
        let state = state();

        let compact = ReplayPanelLayout::calculate(&state, /*width*/ 49, /*height*/ 26);
        assert_eq!(compact.header_rows, 2);
        assert_eq!(compact.change_gap_rows, 0);
        assert_eq!(compact.change_rows, 4);

        let spacious = ReplayPanelLayout::calculate(&state, /*width*/ 69, /*height*/ 38);
        assert_eq!(spacious.header_rows, 3);
        assert_eq!(spacious.change_gap_rows, 1);
        assert_eq!(spacious.change_rows, 5);
        assert_eq!(spacious.rationale_rows, 2);
        assert!(spacious.diff_rows >= 6);
    }

    #[test]
    fn hints_and_errors_never_move_the_changes_or_original_hunk() {
        let original = model();
        let mut changed = original.clone();
        changed.hint_visible = true;
        changed.notice = "Could not validate the exact original hunk.".to_string();

        let original = ReplayPanelState::parse(&serde_json::to_string(&original).unwrap()).unwrap();
        let changed = ReplayPanelState::parse(&serde_json::to_string(&changed).unwrap()).unwrap();
        let width = 50;
        let height = 22;
        let original_layout = ReplayPanelLayout::calculate(&original, width, height);
        let changed_layout = ReplayPanelLayout::calculate(&changed, width, height);
        assert_eq!(original_layout, changed_layout);

        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let mut before = RenderBuffer::new(width, height, &theme.style);
        let mut after = RenderBuffer::new(width, height, &theme.style);
        for (state, buffer) in [(&original, &mut before), (&changed, &mut after)] {
            render_replay_panel(
                buffer,
                state,
                Point::new(0, 0),
                width,
                height,
                ReplayPanelViewport {
                    scroll: 0,
                    focused: true,
                },
                &theme,
            );
        }

        let before = rendered_rows(&before);
        let after = rendered_rows(&after);
        let changes_row = original_layout.header_rows;
        let rationale_row = changes_row
            + usize::from(original_layout.change_rows > 0)
            + original_layout.change_rows
            + original_layout.change_gap_rows
            + original_layout.current_change_rows
            + original_layout.current_change_gap_rows;
        let status_row = rationale_row + original_layout.rationale_rows;
        let source_row = status_row + original_layout.status_rows;

        assert_eq!(before[changes_row], after[changes_row]);
        assert_eq!(before[changes_row + 1], after[changes_row + 1]);
        assert!(before[rationale_row].starts_with("WHY"));
        assert!(after[rationale_row].starts_with("HINT"));
        assert!(before[status_row].trim().is_empty());
        assert!(after[status_row].starts_with('✕'));
        assert!(after[status_row].contains("Could not validate"));
        assert_eq!(before[source_row], after[source_row]);
        assert_eq!(before[height - 1], after[height - 1]);
    }

    #[test]
    fn expanding_the_rationale_keeps_navigation_pinned_and_reveals_its_complete_text() {
        let mut replay = model();
        replay.steps[0].why = "Preserve the complete original author rationale so a reviewer can read every sentence, understand the design tradeoff, and reconstruct the change without guessing what the ellipsis omitted.".to_string();
        let collapsed = ReplayPanelState::parse(&serde_json::to_string(&replay).unwrap()).unwrap();
        replay.rationale_expanded = true;
        let expanded = ReplayPanelState::parse(&serde_json::to_string(&replay).unwrap()).unwrap();
        let width = 50;
        let height = 26;
        let collapsed_layout = ReplayPanelLayout::calculate(&collapsed, width, height);
        let expanded_layout = ReplayPanelLayout::calculate(&expanded, width, height);

        assert_eq!(collapsed_layout.header_rows, expanded_layout.header_rows);
        assert_eq!(collapsed_layout.change_rows, expanded_layout.change_rows);
        assert_eq!(collapsed_layout.status_rows, expanded_layout.status_rows);
        assert_eq!(collapsed_layout.footer_rows, expanded_layout.footer_rows);
        assert_eq!(collapsed_layout.rationale_rows, 2);
        assert!(expanded_layout.rationale_rows > collapsed_layout.rationale_rows);
        assert!(expanded_layout.diff_rows >= 1);

        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let mut buffer = RenderBuffer::new(width, height, &theme.style);
        render_replay_panel(
            &mut buffer,
            &expanded,
            Point::new(0, 0),
            width,
            height,
            ReplayPanelViewport {
                scroll: 0,
                focused: true,
            },
            &theme,
        );
        assert!(rendered_rows(&buffer)
            .iter()
            .any(|row| row.contains("ellipsis omitted.")));
    }

    #[test]
    fn visible_changes_do_not_recenter_until_selection_crosses_an_edge() {
        let mut replay = model();

        for (index, expected_first) in [(0, 0), (1, 0), (2, 0), (3, 2), (4, 2)] {
            replay.index = index;
            let state = ReplayPanelState::parse(&serde_json::to_string(&replay).unwrap()).unwrap();
            assert_eq!(
                replay_change_window_start(&state, /*visible_rows*/ 3),
                expected_first,
                "the pinned list moved before change {index} crossed its edge",
            );
        }
    }

    #[test]
    fn replay_footer_keys_use_themes_without_modifying_shared_action_bars() {
        let model = model();
        let width = 62;
        let actions = replay_actions(&model, width);
        let expected = ActionBar::new(&actions).layout(width);
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let mut buffer = RenderBuffer::new(width, /*height*/ 1, &theme.style);

        render_replay_action_bar(&mut buffer, 0, 0, width, &actions, &theme);

        let mut column = 0;
        for span in &expected.spans {
            if span.role == ActionBarRole::Key {
                assert_eq!(
                    buffer.cells[column].style.fg,
                    theme.ui_style.picker_prompt.fg.or(theme.style.fg),
                );
                assert!(buffer.cells[column].style.bold);
            }
            column += display_width(&span.text);
        }
        assert_labeled_actions(&expected);
        assert!(!expected.text().contains("[i]"));
    }

    #[test]
    fn replay_action_bar_keeps_complete_shortcuts_at_narrow_widths() {
        let actions = replay_actions(&model(), /*width*/ 30);
        let layout = ActionBar::new(&actions).layout(/*width*/ 30);
        let visible = layout.text();
        for key in ["i", "a", "u", "?"] {
            assert!(
                has_visible_key(&layout, key),
                "missing essential narrow Replay action {key}: {visible}",
            );
        }
        assert_labeled_actions(&layout);
        assert!(!visible.contains('['));
        assert!(display_width(&visible) <= 30);
    }

    #[test]
    fn normal_width_replay_actions_keep_meaningful_compact_labels() {
        let actions = replay_actions(&model(), /*width*/ 62);
        let layout = ActionBar::new(&actions).layout(/*width*/ 62);
        let visible = layout.text();

        for key in ["j/k", "i", "v", "a", "u", "?"] {
            assert!(
                has_visible_key(&layout, key),
                "missing meaningful Replay action {key}: {visible}",
            );
        }
        assert_labeled_actions(&layout);
        assert!(!visible.contains('['));
        assert!(visible.contains("v Check"));
        assert!(!visible.contains("Chk"));
        assert_eq!(layout.hidden_count(), 0);
    }

    #[test]
    fn wide_replay_footer_offers_reversible_zoom_without_hiding_core_actions() {
        let replay = multi_file_model();
        let width = 86;
        let actions = replay_actions(&replay, width);
        let layout = ActionBar::new(&actions).layout(width);

        for key in ["j/k", "i", "v", "a", "u", "h/l", "z", "?"] {
            assert!(
                has_visible_key(&layout, key),
                "missing focused-surface shortcut {key}: {}",
                layout.text(),
            );
        }
        assert_labeled_actions(&layout);
        assert_eq!(layout.hidden_count(), 0);
    }

    #[test]
    fn reviewer_footer_never_ends_in_a_dangling_overflow_count() {
        let mut replay = multi_file_model();
        replay.review_role = Some(ReplayReviewRole::Author);
        replay.head_commit = "b".repeat(40);
        replay.author_workspace_available = true;

        for width in [30, 46, 62, 79, 99] {
            let actions = replay_actions(&replay, width);
            let layout = ActionBar::new(&actions).layout(width);
            assert_eq!(
                layout.hidden_count(),
                0,
                "review shortcuts must be deliberately curated at {width} columns: {}",
                layout.text(),
            );
            assert!(!layout.text().contains("… +"));
            assert_labeled_actions(&layout);
        }
    }

    #[test]
    fn verified_author_guide_keeps_original_head_shortcut_and_learning_keys_visible() {
        let mut replay = model();
        replay.review_role = Some(ReplayReviewRole::Author);
        replay.head_commit = "b".repeat(40);
        replay.author_workspace_available = true;

        let actions = replay_actions(&replay, /*width*/ 46);
        let layout = ActionBar::new(&actions).layout(/*width*/ 46);
        let visible = layout.text();

        for key in ["j/k", "i", "v", "a", "u", "?"] {
            assert!(
                has_visible_key(&layout, key),
                "missing verified-author guide shortcut {key} at 46 columns: {visible}",
            );
        }
        assert_labeled_actions(&layout);
        assert!(display_width(&visible) <= 46);

        let author_actions = replay_actions(&replay, /*width*/ 87);
        let author_layout = ActionBar::new(&author_actions).layout(/*width*/ 87);
        assert!(
            has_visible_key(&author_layout, "W"),
            "the original PR worktree must be visible at a full author-pane width: {}",
            author_layout.text(),
        );

        let narrow_actions = replay_actions(&replay, /*width*/ 30);
        let narrow_layout = ActionBar::new(&narrow_actions).layout(/*width*/ 30);
        let narrow = narrow_layout.text();
        for key in ["i", "a", "u", "?"] {
            assert!(
                has_visible_key(&narrow_layout, key),
                "an author shortcut must not hide essential narrow-guide action {key}: {narrow}",
            );
        }
        assert_labeled_actions(&narrow_layout);
        assert!(display_width(&narrow) <= 30);
    }

    #[test]
    fn original_head_shortcut_is_not_offered_to_reviewers_or_local_sources() {
        let mut reviewer = model();
        reviewer.review_role = Some(ReplayReviewRole::Reviewer);
        reviewer.head_commit = "b".repeat(40);

        assert!(!replay_actions(&reviewer, /*width*/ 46)
            .iter()
            .any(|action| action.id == "original_workspace"));
        assert!(!replay_outbox_actions(&reviewer)
            .iter()
            .any(|action| action.id == "original_workspace"));
        assert!(!replay_actions(&model(), /*width*/ 46)
            .iter()
            .any(|action| action.id == "original_workspace"));
    }

    #[test]
    fn compact_replay_header_marks_omitted_reason_and_visible_diff_overflow() {
        let mut replay = model();
        replay.mode = ReplayPanelMode::Snippet;
        replay.steps[0].why = replay.steps[0].why.repeat(4);
        let state = ReplayPanelState::parse(&serde_json::to_string(&replay).unwrap()).unwrap();
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let width = 46;
        let height = 24;
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
        assert!(
            rows.iter().any(|row| row.starts_with("WHY"))
                && rows.iter().any(|row| row.trim_end().ends_with('…')),
            "a truncated explanation must never appear falsely complete",
        );
        assert!(
            rows.iter().any(|row| {
                row.contains("rendering.rs") && row.contains('↓') && row.contains("^D/^U")
            }),
            "the exact source must expose remaining changed lines and the scroll keys",
        );
    }

    #[test]
    fn empty_outbox_offers_only_actions_that_can_be_used() {
        let replay = model();
        let actions = replay_outbox_actions(&replay);
        let layout = ActionBar::new(&actions).layout(/*width*/ 46);
        let visible = layout.text();

        for key in ["c", "s", "L", "r"] {
            assert!(
                has_visible_key(&layout, key),
                "missing usable empty-outbox action {key}: {visible}",
            );
        }
        for key in ["S", "j/k", "e", "d"] {
            assert!(!has_visible_key(&layout, key));
        }
        assert_labeled_actions(&layout);
        assert_eq!(layout.hidden_count(), 0);
    }

    #[test]
    fn narrow_github_outbox_keeps_publish_save_and_return_visible() {
        let mut replay = model();
        replay.review_role = Some(ReplayReviewRole::Reviewer);
        replay.head_commit = "b".repeat(40);
        replay.drafts = vec![outbox_draft(
            ReplayReviewDraftKind::InlineComment,
            "Publish only the exact original review comment.",
        )];
        replay.draft_count = replay.drafts.len();
        let actions = replay_outbox_actions(&replay);
        let layout = ActionBar::new(&actions).layout(/*width*/ 46);
        let visible = layout.text();

        for key in ["j/k", "c", "S", "P", "r"] {
            assert!(
                has_visible_key(&layout, key),
                "missing essential original-PR outbox action {key}: {visible}"
            );
        }
        assert_labeled_actions(&layout);
        assert!(display_width(&visible) <= 46);
    }

    #[test]
    fn author_outbox_keeps_original_head_and_private_review_actions_visible() {
        let mut replay = model();
        replay.review_role = Some(ReplayReviewRole::Author);
        replay.head_commit = "b".repeat(40);
        replay.author_workspace_available = true;
        replay.drafts = vec![outbox_draft(
            ReplayReviewDraftKind::InlineComment,
            "Keep the author review linked to the original PR source.",
        )];
        replay.draft_count = replay.drafts.len();

        let actions = replay_outbox_actions(&replay);
        let layout = ActionBar::new(&actions).layout(/*width*/ 46);
        let visible = layout.text();

        for key in ["j/k", "c", "S", "P", "r"] {
            assert!(
                has_visible_key(&layout, key),
                "missing original-head author outbox action {key}: {visible}",
            );
        }
        assert_labeled_actions(&layout);

        let wide_layout = ActionBar::new(&actions).layout(/*width*/ 87);
        for key in ["e", "d", "W"] {
            assert!(
                has_visible_key(&wide_layout, key),
                "missing author action {key} at the default wide Replay size: {}",
                wide_layout.text(),
            );
        }
        assert!(display_width(&visible) <= 46);
    }

    #[test]
    fn local_and_fix_only_outboxes_never_offer_github_publication() {
        let mut local = model();
        local.drafts = vec![outbox_draft(
            ReplayReviewDraftKind::InlineComment,
            "Keep this local-range comment private.",
        )];
        local.draft_count = local.drafts.len();
        assert!(!replay_outbox_actions(&local)
            .iter()
            .any(|action| action.id == "publish_review"));

        let mut author = model();
        author.review_role = Some(ReplayReviewRole::Author);
        author.head_commit = "b".repeat(40);
        author.drafts = vec![outbox_draft(
            ReplayReviewDraftKind::CodeFix,
            "Keep this original-PR fix out of the GitHub review.",
        )];
        author.draft_count = author.drafts.len();
        assert!(!replay_outbox_actions(&author)
            .iter()
            .any(|action| action.id == "publish_review"));
    }

    #[test]
    fn submitted_comments_are_clearly_posted_and_cannot_be_edited_or_reposted() {
        let mut replay = model();
        replay.review_role = Some(ReplayReviewRole::Reviewer);
        replay.head_commit = "b".repeat(40);
        replay.view = ReplayPanelView::Outbox;
        let mut draft = outbox_draft(
            ReplayReviewDraftKind::InlineComment,
            "This comment was explicitly approved and posted.",
        );
        draft.state = ReplayDraftState::Submitted;
        replay.receipts = vec![ReplayReviewReceipt {
            id: 71,
            url: "https://github.com/example/replay/pull/482#pullrequestreview-71".to_string(),
            outcome: crate::replay::ReplayReviewOutcome::Comment,
            target_commit: draft.target_commit.clone(),
            viewer: "reviewer".to_string(),
            draft_ids: vec![draft.id.clone()],
            payload_digest: "a".repeat(64),
            submitted_at: "2026-07-27T20:00:00Z".to_string(),
        }];
        replay.drafts = vec![draft];
        replay.draft_count = replay.drafts.len();
        let state = ReplayPanelState::parse(&serde_json::to_string(&replay).unwrap())
            .expect("accept an original-head-linked posted review and its receipt");
        let lines = replay_outbox_lines(&state, /*width*/ 46)
            .into_iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let actions = replay_outbox_actions(&replay);

        assert!(lines.iter().any(|line| line.contains("1 posted")));
        assert!(lines.iter().any(|line| line.contains("POSTED")));
        assert!(lines.iter().any(|line| line.contains("verified receipts")));
        assert!(actions.iter().any(|action| action.id == "save_review"));
        assert!(actions.iter().any(|action| action.id == "outbox"));
        for action in ["edit_draft", "discard_draft", "publish_review"] {
            assert!(!actions.iter().any(|candidate| candidate.id == action));
        }
    }

    #[test]
    fn populated_outbox_prioritizes_edit_discard_and_return() {
        let mut replay = model();
        replay.drafts = vec![outbox_draft(
            ReplayReviewDraftKind::InlineComment,
            "Keep the original visible viewport bounded.",
        )];
        replay.draft_count = replay.drafts.len();
        let actions = replay_outbox_actions(&replay);
        let layout = ActionBar::new(&actions).layout(/*width*/ 46);
        let visible = layout.text();

        for key in ["j/k", "c", "S", "r"] {
            assert!(
                has_visible_key(&layout, key),
                "missing essential outbox action {key}"
            );
        }
        assert_labeled_actions(&layout);
        assert!(display_width(&visible) <= 46);
    }

    #[test]
    fn wide_outbox_exposes_private_review_save_and_load_without_hiding_editor_actions() {
        let mut replay = model();
        replay.drafts = vec![outbox_draft(
            ReplayReviewDraftKind::InlineComment,
            "Keep the original visible viewport bounded.",
        )];
        replay.draft_count = replay.drafts.len();
        let actions = replay_outbox_actions(&replay);
        let layout = ActionBar::new(&actions).layout(/*width*/ 120);
        let visible = layout.text();

        for key in ["j/k", "c", "e", "d", "s", "S", "L", "r"] {
            assert!(
                has_visible_key(&layout, key),
                "missing private review action {key}: {visible}",
            );
        }
        assert_labeled_actions(&layout);
        assert_eq!(layout.hidden_count(), 0);
    }

    #[test]
    fn finding_only_outbox_can_save_without_offering_invalid_draft_actions() {
        let mut replay = model();
        replay.notes.push(ReplayPanelNote {
            index: 0,
            text: "Preserve this private original-source observation.".to_string(),
        });
        let actions = replay_outbox_actions(&replay);
        let layout = ActionBar::new(&actions).layout(/*width*/ 46);
        let visible = layout.text();

        for key in ["S", "L", "r"] {
            assert!(
                has_visible_key(&layout, key),
                "missing private finding action {key}: {visible}",
            );
        }
        for key in ["j/k", "e", "d"] {
            assert!(!has_visible_key(&layout, key));
        }
        assert_labeled_actions(&layout);
        assert_eq!(layout.hidden_count(), 0);
    }

    #[test]
    fn multi_file_action_bar_shows_file_motion_without_hiding_essential_shortcuts() {
        let replay = multi_file_model();
        let actions = replay_actions(&replay, /*width*/ 46);
        let layout = ActionBar::new(&actions).layout(/*width*/ 46);
        let visible = layout.text();

        for key in ["j/k", "i", "v", "a", "u", "?"] {
            assert!(
                has_visible_key(&layout, key),
                "missing visible Replay action {key}: {visible}",
            );
        }
        assert_labeled_actions(&layout);
        assert_eq!(layout.hidden_count(), 0);

        let compact_actions = replay_actions(&replay, /*width*/ 30);
        let compact_layout = ActionBar::new(&compact_actions).layout(/*width*/ 30);
        let compact_visible = compact_layout.text();
        for key in ["i", "a", "u", "?"] {
            assert!(
                has_visible_key(&compact_layout, key),
                "missing essential narrow Replay action {key}: {compact_visible}",
            );
        }
        assert!(!has_visible_key(&compact_layout, "h/l"));
        assert_labeled_actions(&compact_layout);

        let wide_actions = replay_actions(&replay, /*width*/ 86);
        let wide_layout = ActionBar::new(&wide_actions).layout(/*width*/ 86);
        assert!(has_visible_key(&wide_layout, "j/k"));
        assert!(has_visible_key(&wide_layout, "h/l"));
        assert_eq!(wide_layout.hidden_count(), 0);
    }

    #[test]
    fn short_hunk_keeps_changes_pinned_above_complete_original_source() {
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
        let changes_row = layout.header_rows;
        let source_row = changes_row
            + usize::from(layout.change_rows > 0)
            + layout.change_rows
            + layout.change_gap_rows
            + layout.current_change_rows
            + layout.current_change_gap_rows
            + layout.rationale_rows
            + layout.status_rows;
        assert_eq!(layout.diff_rows, state.document.lines.len());
        assert!(rows[changes_row].starts_with("─ CHANGES"));
        assert!(rows[source_row].contains("src/editor/rendering.rs"));
        let last_source_row = source_row + layout.source_rows + layout.diff_rows - 1;
        assert!(rows[last_source_row].contains(
            state
                .document
                .lines
                .last()
                .expect("source-backed hunk")
                .text
                .trim(),
        ));
        assert!(rows[height - 1].contains("? Help"));
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
        let changes_row = layout.header_rows;
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
        assert!(rows.iter().any(|row| row.starts_with("▶ ○ 03 ")));
        assert!(!rows.iter().any(|row| row.starts_with("▶ ●")));
        assert!(!rows.iter().any(|row| row.contains(" ADD ")));
    }

    #[test]
    fn pending_change_shows_a_note_marker_only_for_real_local_findings() {
        let mut replay = model();
        replay.index = 1;
        replay.notes.push(ReplayPanelNote {
            index: 1,
            text: "Verify the changed original argument.".to_string(),
        });
        let state = ReplayPanelState::parse(&serde_json::to_string(&replay).unwrap()).unwrap();
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let mut buffer = RenderBuffer::new(/*width*/ 52, /*height*/ 35, &theme.style);

        render_replay_panel(
            &mut buffer,
            &state,
            Point::new(0, 0),
            /*width*/ 52,
            /*height*/ 35,
            ReplayPanelViewport {
                scroll: 0,
                focused: true,
            },
            &theme,
        );

        let rows = rendered_rows(&buffer);
        assert!(rows.iter().any(|row| row.starts_with("▶ ✎ 02 ")));
        assert!(rows.iter().any(|row| row.trim_start().starts_with("○ 01 ")));
        assert!(!rows.iter().any(|row| row.starts_with("▶ ✓ 02 ")));
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
                SelectionForegroundPriority::Selection,
            )
            .bg;
        let row = layout.header_rows + 1;
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
        let width = 62;
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

        let heading = &rendered_rows(&buffer)[layout.header_rows];
        assert!(heading.starts_with("─ CHANGES"));
        assert!(heading.contains("file 2 of 3"));
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
        assert!(!title.contains("03 / 05"));
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
        assert!(lines[0].ends_with("YOUR PR"));
        assert!(lines[1].contains("feat/viewport-diagnostics"));
        assert!(lines[1].contains("bbbbbbb"));
        assert!(lines[1].ends_with("0 / 5 reviewed"));
    }

    #[test]
    fn pinned_header_identifies_the_verified_original_author_as_your_pull_request() {
        let mut replay = model();
        replay.review_role = Some(ReplayReviewRole::Author);
        replay.head_commit = "b".repeat(40);
        let state = ReplayPanelState::parse(&serde_json::to_string(&replay).unwrap()).unwrap();
        let header = replay_pinned_header_lines(&state, /*width*/ 72);
        let role = header[0]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();
        let progress = header[1]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();

        assert!(role.ends_with("YOUR PR"));
        assert!(progress.ends_with("0/5 reviewed"));

        replay.author_workspace_root = "/workspace/original-pull-request".to_string();
        let state = ReplayPanelState::parse(&serde_json::to_string(&replay).unwrap()).unwrap();
        let role = replay_pinned_header_lines(&state, /*width*/ 72)[0]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();
        assert!(role.ends_with("YOUR PR · PR HEAD"));
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
    fn attached_original_author_workspace_is_unmistakable_in_the_replay_header() {
        let mut replay = model();
        replay.review_role = Some(ReplayReviewRole::Author);
        replay.head_commit = "b".repeat(40);
        replay.author_workspace_available = true;
        replay.author_workspace_root =
            "/workspace/repository.replay-author-pr-482-bbbbbbb".to_string();
        replay.author_workspace_branch = "replay/author/pr-482-bbbbbbb".to_string();

        let state = ReplayPanelState::parse(&serde_json::to_string(&replay).unwrap()).unwrap();
        let metadata = replay_header_lines(&state, /*width*/ 46)[0]
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();

        assert!(metadata.contains("#482"));
        assert!(metadata.ends_with("YOUR PR · PR HEAD"));
    }

    #[test]
    fn native_outbox_retains_original_anchor_selected_marker_and_pinned_actions() {
        let mut replay = model();
        replay.review_role = Some(ReplayReviewRole::Author);
        replay.head_commit = "b".repeat(40);
        replay.view = ReplayPanelView::Outbox;
        replay.notice = "Local review draft saved without sending to GitHub.".to_string();
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
        assert!(rows.iter().any(|row| row.contains("YOUR PR")));
        assert!(rows.iter().any(|row| row.contains("LOCAL OUTBOX")));
        assert!(rows
            .iter()
            .any(|row| row.contains("nothing sent to GitHub")));
        assert!(rows
            .iter()
            .any(|row| row.contains("Local review draft saved without sending")));
        assert!(rows.iter().any(|row| row.contains("▶ INLINE COMMENT")));
        assert!(rows
            .iter()
            .any(|row| row.contains("src/editor/rendering.rs:11-12")));
        assert!(rows.iter().any(|row| row.contains("RIGHT")));
        assert!(rows
            .iter()
            .any(|row| row.contains("Please test the original viewport boundary.")));
        assert!(rows[height - 1].contains("j/k"));
        assert!(rows[height - 1].contains("c "));
        assert!(rows[height - 1].contains("r "));
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
        assert!(scrolled_rows[height - 1].contains("r "));
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
        assert!(!rendered_rows(&focused)[0].contains("01 / 05"));
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
        let row = layout.header_rows + 1;
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
            let changes_row = layout.header_rows;
            let footer = &rows[height - 1];
            assert!(rows[changes_row].starts_with("─ CHANGES"));
            for shortcut in ["i ", "a ", "u ", "? "] {
                assert!(
                    footer.contains(shortcut),
                    "missing {shortcut} at {width} columns"
                );
            }
            assert!(!footer.contains("[i]"));
        }
    }
}
