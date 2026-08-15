//! Painting of editor, plugin, diagnostic, and window state into a [`RenderBuffer`].
//!
//! This module translates the editor's logical layout into terminal cells and computes
//! incremental frame output. It owns clipping, style precedence, gutter and window
//! chrome, wrapped rows, cursor placement, and terminal attribute transitions. It does
//! not mutate buffer text or decide input behavior.
//!
//! Rendering caches are keyed by stable buffer identity inputs and content revisions.
//! Any feature that changes visible state without changing text must also advance the
//! editor's render generation or request an explicit render.

use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use chrono::Local;
use crossterm::{
    cursor::{self, MoveTo},
    style, terminal, QueueableCommand as _,
};
use unicode_segmentation::UnicodeSegmentation as _;

use crate::{
    color::{blend_color, ensure_minimum_contrast, Color},
    config::{CursorShape, FormattingProvider, KeyAction, PickerIconStyle, StatuslineSection},
    editor::RenderCommand,
    lsp::{Diagnostic, DiagnosticSeverity},
    plugin::DecorationAnchor,
    splash,
    theme::{SelectionForegroundPriority, Style, Theme},
    ui::IconCatalog,
    undo::{TextPosition, TextRange},
    unicode_utils::{
        char_prefix, display_width, display_width_with_tabs, fit_display_width,
        grapheme_to_column_with_tabs, trim_line_ending, truncate_display_width,
    },
    utils::{expand_user_path, get_workspace_path},
    window::WindowId,
};

use super::{
    adjust_color_brightness, diagnostic_foreground, diagnostic_priority,
    display_layout::{DisplayLayout, InlineCommentContent},
    render_buffer::Change,
    Editor, Mode, Point, Rect, RenderBuffer, StatuslineGitChanges, StyleCursor,
    GUTTER_SIGN_COLUMN_WIDTH, MAX_HIGHLIGHT_SLICE_BYTES,
};

fn diagnostic_row(diagnostics: &[&Diagnostic], available_width: usize) -> Option<String> {
    let diagnostic = diagnostics.first()?;
    if available_width == 0 {
        return None;
    }

    let indicator = "■".repeat(diagnostics.len());
    let message = diagnostic.message.replace('\n', " ");
    let message = message.trim();
    let row = if message.is_empty() {
        indicator
    } else {
        format!("{indicator} {message}")
    };

    if display_width(&row) <= available_width {
        return Some(fit_display_width(&row, available_width));
    }

    if available_width == 1 {
        return Some(truncate_display_width(&row, available_width));
    }

    let mut row = truncate_display_width(&row, available_width - 1);
    row.push('…');
    Some(fit_display_width(&row, available_width))
}

fn diagnostics_by_visible_line(
    diagnostics: &[Diagnostic],
    visible_start: usize,
    visible_end: usize,
) -> HashMap<usize, Vec<&Diagnostic>> {
    let mut by_line: HashMap<usize, Vec<&Diagnostic>> = diagnostics
        .iter()
        .filter(|diagnostic| (visible_start..=visible_end).contains(&diagnostic.range.start.line))
        .fold(HashMap::new(), |mut by_line, diagnostic| {
            by_line
                .entry(diagnostic.range.start.line)
                .or_default()
                .push(diagnostic);
            by_line
        });
    for diagnostics in by_line.values_mut() {
        diagnostics.sort_by(|left, right| {
            diagnostic_priority(right.severity.as_ref())
                .cmp(&diagnostic_priority(left.severity.as_ref()))
        });
    }
    by_line
}

fn statusline_file_name(name: &str) -> &str {
    name.strip_prefix("./").unwrap_or(name)
}

#[derive(Clone)]
struct StatuslineSegment {
    text: String,
    style: Style,
    accents: Vec<StatuslineAccent>,
}

#[derive(Clone)]
struct StatuslineAccent {
    column: usize,
    text: String,
    color: Color,
    minimum_contrast: Option<f32>,
}

struct StatuslineContext<'a> {
    mode: String,
    filename: String,
    file_path: Option<String>,
    position: String,
    syntax: Option<String>,
    git_branch: Option<String>,
    diagnostics: Option<(usize, usize)>,
    git_changes: Option<StatuslineGitChanges>,
    lsp_status: Option<String>,
    current_symbol: Option<String>,
    selection: Option<String>,
    recording: Option<char>,
    search_matches: Option<(usize, usize)>,
    indentation: String,
    encoding: &'static str,
    line_endings: &'static str,
    read_only: bool,
    modified: bool,
    workspace: String,
    relative_path: Option<String>,
    buffer_index: String,
    window_index: String,
    file_size: String,
    agent_activity: Option<String>,
    formatter: Option<&'a str>,
    clock: String,
}

fn statusline_segment(
    section: StatuslineSection,
    context: &StatuslineContext<'_>,
    theme: &Theme,
    icon_style: PickerIconStyle,
    color_icons: bool,
) -> Option<StatuslineSegment> {
    let (text, accents) = match section {
        StatuslineSection::Mode => (format!(" {} ", context.mode), Vec::new()),
        StatuslineSection::GitBranch => {
            let branch = context.git_branch.as_deref()?;
            let glyph = match icon_style {
                PickerIconStyle::NerdFont => "",
                PickerIconStyle::Unicode => "⑂",
                PickerIconStyle::Ascii => "git",
                PickerIconStyle::None => "",
            };
            (statusline_icon_label(glyph, branch), Vec::new())
        }
        StatuslineSection::Filename => (format!(" {} ", context.filename), Vec::new()),
        StatuslineSection::Syntax => {
            let syntax = context.syntax.as_deref()?;
            let icon = IconCatalog::file(
                context.file_path.as_deref().unwrap_or(&context.filename),
                icon_style,
            );
            let accents = (color_icons && !icon.glyph.is_empty())
                .then_some(icon.color)
                .flatten()
                .map(|color| StatuslineAccent {
                    column: 1,
                    text: icon.glyph.to_string(),
                    color,
                    minimum_contrast: None,
                })
                .into_iter()
                .collect();
            (statusline_icon_label(icon.glyph, syntax), accents)
        }
        StatuslineSection::Position => (context.position.clone(), Vec::new()),
        StatuslineSection::Diagnostics => {
            let (errors, warnings) = context.diagnostics?;
            statusline_diagnostics_label(errors, warnings, icon_style, color_icons, theme)?
        }
        StatuslineSection::GitChanges => {
            let changes = context.git_changes?;
            let text = format!(
                "+{} ~{} -{}",
                changes.added, changes.modified, changes.deleted
            );
            (
                statusline_icon_label(
                    statusline_section_icon(StatuslineSection::GitChanges, icon_style),
                    &text,
                ),
                Vec::new(),
            )
        }
        StatuslineSection::LspStatus => (
            statusline_icon_label(
                statusline_section_icon(StatuslineSection::LspStatus, icon_style),
                context.lsp_status.as_deref()?,
            ),
            Vec::new(),
        ),
        StatuslineSection::CurrentSymbol => (
            statusline_icon_label(
                statusline_section_icon(StatuslineSection::CurrentSymbol, icon_style),
                context.current_symbol.as_deref()?,
            ),
            Vec::new(),
        ),
        StatuslineSection::Selection => (
            statusline_icon_label(
                statusline_section_icon(StatuslineSection::Selection, icon_style),
                context.selection.as_deref()?,
            ),
            Vec::new(),
        ),
        StatuslineSection::Recording => {
            let label = format!("recording @{}", context.recording?);
            (
                statusline_icon_label(
                    statusline_section_icon(StatuslineSection::Recording, icon_style),
                    &label,
                ),
                Vec::new(),
            )
        }
        StatuslineSection::SearchMatches => {
            let (current, total) = context.search_matches?;
            let label = format!("{current}/{total}");
            (
                statusline_icon_label(
                    statusline_section_icon(StatuslineSection::SearchMatches, icon_style),
                    &label,
                ),
                Vec::new(),
            )
        }
        StatuslineSection::Indentation => (
            statusline_icon_label(
                statusline_section_icon(StatuslineSection::Indentation, icon_style),
                &context.indentation,
            ),
            Vec::new(),
        ),
        StatuslineSection::Encoding => (
            statusline_icon_label(
                statusline_section_icon(StatuslineSection::Encoding, icon_style),
                context.encoding,
            ),
            Vec::new(),
        ),
        StatuslineSection::LineEndings => (
            statusline_icon_label(
                statusline_section_icon(StatuslineSection::LineEndings, icon_style),
                context.line_endings,
            ),
            Vec::new(),
        ),
        StatuslineSection::ReadOnly => {
            if !context.read_only {
                return None;
            }
            (
                statusline_icon_label(
                    statusline_section_icon(StatuslineSection::ReadOnly, icon_style),
                    "RO",
                ),
                Vec::new(),
            )
        }
        StatuslineSection::Modified => {
            if !context.modified {
                return None;
            }
            (
                statusline_icon_label(
                    statusline_section_icon(StatuslineSection::Modified, icon_style),
                    "modified",
                ),
                Vec::new(),
            )
        }
        StatuslineSection::Workspace => (
            statusline_icon_label(
                statusline_section_icon(StatuslineSection::Workspace, icon_style),
                &context.workspace,
            ),
            Vec::new(),
        ),
        StatuslineSection::RelativePath => (
            statusline_icon_label(
                statusline_section_icon(StatuslineSection::RelativePath, icon_style),
                context.relative_path.as_deref()?,
            ),
            Vec::new(),
        ),
        StatuslineSection::BufferIndex => (
            statusline_icon_label(
                statusline_section_icon(StatuslineSection::BufferIndex, icon_style),
                &context.buffer_index,
            ),
            Vec::new(),
        ),
        StatuslineSection::WindowIndex => (
            statusline_icon_label(
                statusline_section_icon(StatuslineSection::WindowIndex, icon_style),
                &context.window_index,
            ),
            Vec::new(),
        ),
        StatuslineSection::FileSize => (
            statusline_icon_label(
                statusline_section_icon(StatuslineSection::FileSize, icon_style),
                &context.file_size,
            ),
            Vec::new(),
        ),
        StatuslineSection::AgentActivity => (
            statusline_icon_label(
                statusline_section_icon(StatuslineSection::AgentActivity, icon_style),
                context.agent_activity.as_deref()?,
            ),
            Vec::new(),
        ),
        StatuslineSection::Formatter => (
            statusline_icon_label(
                statusline_section_icon(StatuslineSection::Formatter, icon_style),
                context.formatter?,
            ),
            Vec::new(),
        ),
        StatuslineSection::Clock => (
            statusline_icon_label(
                statusline_section_icon(StatuslineSection::Clock, icon_style),
                &context.clock,
            ),
            Vec::new(),
        ),
    };

    Some(StatuslineSegment {
        text,
        style: Style::default(),
        accents,
    })
}

pub(crate) fn statusline_section_icon(
    section: StatuslineSection,
    style: PickerIconStyle,
) -> &'static str {
    if style == PickerIconStyle::None {
        return "";
    }
    match (section, style) {
        (StatuslineSection::Mode, _) => "N",
        (StatuslineSection::GitBranch, PickerIconStyle::NerdFont) => "",
        (StatuslineSection::GitBranch, PickerIconStyle::Unicode) => "⑂",
        (StatuslineSection::GitBranch, PickerIconStyle::Ascii) => "git",
        (StatuslineSection::Diagnostics, PickerIconStyle::NerdFont) => "󰒡",
        (StatuslineSection::Diagnostics, PickerIconStyle::Unicode) => "⚠",
        (StatuslineSection::Diagnostics, PickerIconStyle::Ascii) => "diag",
        (StatuslineSection::GitChanges, PickerIconStyle::NerdFont) => "󰊢",
        (StatuslineSection::GitChanges, PickerIconStyle::Unicode) => "Δ",
        (StatuslineSection::GitChanges, PickerIconStyle::Ascii) => "git",
        (StatuslineSection::LspStatus, PickerIconStyle::NerdFont) => "",
        (StatuslineSection::LspStatus, PickerIconStyle::Unicode) => "⚙",
        (StatuslineSection::LspStatus, PickerIconStyle::Ascii) => "lsp",
        (StatuslineSection::CurrentSymbol, _) => "ƒ",
        (StatuslineSection::Selection, PickerIconStyle::NerdFont) => "󰒉",
        (StatuslineSection::Selection, PickerIconStyle::Unicode) => "↔",
        (StatuslineSection::Selection, PickerIconStyle::Ascii) => "sel",
        (StatuslineSection::Recording, PickerIconStyle::NerdFont) => "󰑊",
        (StatuslineSection::Recording, PickerIconStyle::Unicode) => "●",
        (StatuslineSection::Recording, PickerIconStyle::Ascii) => "rec",
        (StatuslineSection::SearchMatches, PickerIconStyle::NerdFont) => "",
        (StatuslineSection::SearchMatches, PickerIconStyle::Unicode) => "⌕",
        (StatuslineSection::SearchMatches, PickerIconStyle::Ascii) => "find",
        (StatuslineSection::Indentation, PickerIconStyle::NerdFont) => "󰌒",
        (StatuslineSection::Indentation, PickerIconStyle::Unicode) => "⇥",
        (StatuslineSection::Indentation, PickerIconStyle::Ascii) => "spc",
        (StatuslineSection::Encoding, PickerIconStyle::NerdFont) => "󰅩",
        (StatuslineSection::Encoding, PickerIconStyle::Unicode) => "文",
        (StatuslineSection::Encoding, PickerIconStyle::Ascii) => "enc",
        (StatuslineSection::LineEndings, PickerIconStyle::NerdFont) => "󰌑",
        (StatuslineSection::LineEndings, PickerIconStyle::Unicode) => "↵",
        (StatuslineSection::LineEndings, PickerIconStyle::Ascii) => "eol",
        (StatuslineSection::ReadOnly, PickerIconStyle::NerdFont) => "",
        (StatuslineSection::ReadOnly, PickerIconStyle::Unicode) => "🔒",
        (StatuslineSection::ReadOnly, PickerIconStyle::Ascii) => "ro",
        (StatuslineSection::Modified, PickerIconStyle::Ascii) => "+",
        (StatuslineSection::Modified, _) => "●",
        (StatuslineSection::Workspace, PickerIconStyle::NerdFont) => "󰉋",
        (StatuslineSection::Workspace, PickerIconStyle::Unicode) => "▣",
        (StatuslineSection::Workspace, PickerIconStyle::Ascii) => "ws",
        (StatuslineSection::RelativePath, PickerIconStyle::NerdFont) => "󰈔",
        (StatuslineSection::RelativePath, PickerIconStyle::Unicode) => "↳",
        (StatuslineSection::RelativePath, PickerIconStyle::Ascii) => "path",
        (StatuslineSection::BufferIndex, PickerIconStyle::NerdFont) => "󰓩",
        (StatuslineSection::BufferIndex, PickerIconStyle::Unicode) => "▤",
        (StatuslineSection::BufferIndex, PickerIconStyle::Ascii) => "buf",
        (StatuslineSection::WindowIndex, PickerIconStyle::NerdFont) => "󰖲",
        (StatuslineSection::WindowIndex, PickerIconStyle::Unicode) => "□",
        (StatuslineSection::WindowIndex, PickerIconStyle::Ascii) => "win",
        (StatuslineSection::FileSize, PickerIconStyle::NerdFont) => "󰉉",
        (StatuslineSection::FileSize, PickerIconStyle::Unicode) => "≋",
        (StatuslineSection::FileSize, PickerIconStyle::Ascii) => "size",
        (StatuslineSection::AgentActivity, PickerIconStyle::NerdFont) => "󰚩",
        (StatuslineSection::AgentActivity, PickerIconStyle::Unicode) => "✦",
        (StatuslineSection::AgentActivity, PickerIconStyle::Ascii) => "agent",
        (StatuslineSection::Formatter, PickerIconStyle::NerdFont) => "󰉼",
        (StatuslineSection::Formatter, PickerIconStyle::Unicode) => "≡",
        (StatuslineSection::Formatter, PickerIconStyle::Ascii) => "fmt",
        (StatuslineSection::Clock, PickerIconStyle::NerdFont) => "",
        (StatuslineSection::Clock, PickerIconStyle::Unicode) => "◷",
        (StatuslineSection::Clock, PickerIconStyle::Ascii) => "time",
        (StatuslineSection::Position, PickerIconStyle::NerdFont) => "󰆤",
        (StatuslineSection::Position, PickerIconStyle::Unicode) => "⌖",
        (StatuslineSection::Position, PickerIconStyle::Ascii) => "pos",
        (StatuslineSection::Filename | StatuslineSection::Syntax, _) => "",
        (_, PickerIconStyle::None) => "",
    }
}

fn statusline_diagnostics_label(
    errors: usize,
    warnings: usize,
    style: PickerIconStyle,
    color_icons: bool,
    theme: &Theme,
) -> Option<(String, Vec<StatuslineAccent>)> {
    if errors == 0 && warnings == 0 {
        return None;
    }

    let error_color = theme
        .colors
        .get("editorError.foreground")
        .copied()
        .or_else(|| theme.error_style.as_ref().and_then(|style| style.fg))
        .unwrap_or(Color::Rgb {
            r: 242,
            g: 85,
            b: 90,
        });
    let warning_color = theme
        .colors
        .get("editorWarning.foreground")
        .copied()
        .unwrap_or(Color::Rgb {
            r: 213,
            g: 164,
            b: 88,
        });
    let (error_marker, warning_marker, marker_gap, count_gap) = match style {
        PickerIconStyle::NerdFont => ("", "", "  ", " "),
        PickerIconStyle::Unicode => ("●", "▲", "  ", " "),
        PickerIconStyle::Ascii | PickerIconStyle::None => ("E", "W", " ", ""),
    };

    let mut text = String::from(" ");
    let mut accents = Vec::with_capacity(2);
    let mut has_badge = false;
    for (marker, count, color) in [
        (error_marker, errors, error_color),
        (warning_marker, warnings, warning_color),
    ] {
        if count == 0 {
            continue;
        }
        if has_badge {
            text.push_str(marker_gap);
        }
        let column = display_width(&text);
        text.push_str(marker);
        if color_icons && style != PickerIconStyle::None {
            accents.push(StatuslineAccent {
                column,
                text: marker.to_string(),
                color,
                minimum_contrast: Some(3.0),
            });
        }
        text.push_str(count_gap);
        text.push_str(&count.to_string());
        has_badge = true;
    }
    text.push(' ');

    Some((text, accents))
}

fn statusline_icon_label(glyph: &str, label: &str) -> String {
    if glyph.is_empty() {
        format!(" {label} ")
    } else {
        format!(" {glyph} {label} ")
    }
}

pub(crate) fn statusline_slot_style(theme: &Theme, index: usize) -> Style {
    let base = &theme.statusline_style.inner_style;
    let prominent = &theme.statusline_style.outer_style;
    match index {
        0 => prominent.clone(),
        1 => statusline_context_style(base, prominent),
        _ => base.clone(),
    }
}

fn statusline_segments(
    sections: impl IntoIterator<Item = StatuslineSection>,
    context: &StatuslineContext<'_>,
    theme: &Theme,
    icon_style: PickerIconStyle,
    color_icons: bool,
) -> Vec<StatuslineSegment> {
    sections
        .into_iter()
        .filter_map(|section| statusline_segment(section, context, theme, icon_style, color_icons))
        .enumerate()
        .map(|(index, mut segment)| {
            segment.style = statusline_slot_style(theme, index);
            segment
        })
        .collect()
}

fn statusline_context_style(base: &Style, prominent: &Style) -> Style {
    let (Some(base_bg), Some(accent)) = (base.bg, prominent.bg) else {
        return base.clone();
    };
    let Color::Rgb { r, g, b } = blend_color(accent, base_bg) else {
        unreachable!("blend_color always normalizes its result to RGB");
    };
    let bg = blend_color(Color::Rgba { r, g, b, a: 58 }, base_bg);
    let fg = ensure_minimum_contrast(accent, bg, 3.0);
    Style {
        fg: Some(fg),
        bg: Some(bg),
        bold: true,
        ..base.clone()
    }
}

fn draw_statusline_left(
    buffer: &mut RenderBuffer,
    y: usize,
    limit: usize,
    segments: &[StatuslineSegment],
    base_style: &Style,
    separator: char,
) {
    let separator = separator.to_string();
    let separator_width = display_width(&separator);
    let mut x = 0;

    for (index, segment) in segments.iter().enumerate() {
        if x >= limit {
            break;
        }
        let next_style = segments
            .get(index + 1)
            .map(|next| &next.style)
            .unwrap_or(base_style);
        let has_separator = segment.style.bg != next_style.bg;
        let text_width = display_width(&segment.text);
        let desired_width = text_width + if has_separator { separator_width } else { 0 };
        let remaining = limit - x;
        if desired_width > remaining {
            let text = truncate_display_width(&segment.text, remaining);
            let visible_width = display_width(&text);
            draw_statusline_segment(buffer, x, y, &text, visible_width, segment);
            break;
        }

        draw_statusline_segment(buffer, x, y, &segment.text, text_width, segment);
        x += text_width;
        if has_separator {
            let transition = Style {
                fg: segment.style.bg,
                bg: next_style.bg,
                ..Default::default()
            };
            buffer.set_text(x, y, &separator, &transition);
            x += separator_width;
        }
    }
}

fn draw_statusline_right(
    buffer: &mut RenderBuffer,
    y: usize,
    width: usize,
    mut segments: Vec<StatuslineSegment>,
    base_style: &Style,
    separator: char,
) -> usize {
    let separator = separator.to_string();
    let separator_width = display_width(&separator);
    while segments.len() > 1
        && statusline_right_width(&segments, base_style, separator_width) > width
    {
        segments.remove(0);
    }
    if let Some(segment) = segments.first_mut() {
        let leading_width = if segment.style.bg != base_style.bg {
            separator_width
        } else {
            0
        };
        let available_text = width.saturating_sub(leading_width);
        if display_width(&segment.text) > available_text {
            segment.text = truncate_display_width(&segment.text, available_text);
        }
    }

    let total_width = statusline_right_width(&segments, base_style, separator_width).min(width);
    let start = width - total_width;
    let mut x = start;
    let mut previous_style = base_style;
    for segment in &segments {
        if segment.style.bg != previous_style.bg {
            let transition = Style {
                fg: segment.style.bg,
                bg: previous_style.bg,
                ..Default::default()
            };
            buffer.set_text(x, y, &separator, &transition);
            x += separator_width;
        }
        let text_width = display_width(&segment.text);
        draw_statusline_segment(buffer, x, y, &segment.text, text_width, segment);
        x += text_width;
        previous_style = &segment.style;
    }
    start
}

fn statusline_right_width(
    segments: &[StatuslineSegment],
    base_style: &Style,
    separator_width: usize,
) -> usize {
    let mut previous_style = base_style;
    segments.iter().fold(0, |width, segment| {
        let transition = if segment.style.bg != previous_style.bg {
            separator_width
        } else {
            0
        };
        previous_style = &segment.style;
        width + transition + display_width(&segment.text)
    })
}

fn draw_statusline_segment(
    buffer: &mut RenderBuffer,
    x: usize,
    y: usize,
    text: &str,
    visible_width: usize,
    segment: &StatuslineSegment,
) {
    buffer.set_text(x, y, text, &segment.style);
    for accent in &segment.accents {
        let accent_width = display_width(&accent.text);
        if accent.column.saturating_add(accent_width) > visible_width {
            continue;
        }
        let color = match (accent.minimum_contrast, segment.style.bg) {
            (Some(minimum_contrast), Some(background)) => {
                ensure_minimum_contrast(accent.color, background, minimum_contrast)
            }
            _ => accent.color,
        };
        let accent_style = Style {
            fg: Some(color),
            ..segment.style.clone()
        };
        buffer.set_text(x + accent.column, y, &accent.text, &accent_style);
    }
}

fn statusline_git_search_dir(file: Option<&str>) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(file) = file else {
        return cwd;
    };
    let path = expand_user_path(file).unwrap_or_else(|_| PathBuf::from(file));
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    let search_dir = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(&path).to_path_buf()
    };
    search_dir.canonicalize().unwrap_or(search_dir)
}

fn git_head_path(search_dir: &Path) -> Option<PathBuf> {
    for ancestor in search_dir.ancestors() {
        let dot_git = ancestor.join(".git");
        if dot_git.is_dir() {
            return Some(dot_git.join("HEAD"));
        }
        let Ok(contents) = fs::read_to_string(&dot_git) else {
            continue;
        };
        let Some(git_dir) = contents.trim().strip_prefix("gitdir:") else {
            continue;
        };
        let git_dir = PathBuf::from(git_dir.trim());
        let git_dir = if git_dir.is_absolute() {
            git_dir
        } else {
            ancestor.join(git_dir)
        };
        return Some(git_dir.join("HEAD"));
    }
    None
}

fn git_branch_from_head(search_dir: &Path) -> Option<String> {
    let head = fs::read_to_string(git_head_path(search_dir)?).ok()?;
    let head = head.trim();
    let branch = head
        .strip_prefix("ref: refs/heads/")
        .or_else(|| head.strip_prefix("ref: "))
        .unwrap_or_else(|| head.get(..head.len().min(8)).unwrap_or(head));
    (!branch.is_empty()).then(|| compact_git_branch(branch))
}

fn compact_git_branch(branch: &str) -> String {
    const MAX_WIDTH: usize = 25;
    if display_width(branch) <= MAX_WIDTH {
        return branch.to_string();
    }
    let parts = branch.split('/').collect::<Vec<_>>();
    let compact = if parts.len() >= 3 {
        parts[..parts.len() - 1]
            .iter()
            .map(|part| part.chars().next().unwrap_or('?').to_string())
            .chain(std::iter::once(parts[parts.len() - 1].to_string()))
            .collect::<Vec<_>>()
            .join("/")
    } else {
        branch.to_string()
    };
    if display_width(&compact) <= MAX_WIDTH {
        compact
    } else {
        let mut compact = truncate_display_width(&compact, MAX_WIDTH - 1);
        compact.push('…');
        compact
    }
}

fn git_repository_root(search_dir: &Path) -> Option<PathBuf> {
    search_dir
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .map(Path::to_path_buf)
}

fn git_changes_from_status(repository_root: &Path) -> Option<StatuslineGitChanges> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository_root)
        .args(["status", "--porcelain=v1", "--untracked-files=normal"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| git_changes_from_porcelain(&String::from_utf8_lossy(&output.stdout)))
        .flatten()
}

fn git_changes_from_porcelain(status: &str) -> Option<StatuslineGitChanges> {
    let mut changes = StatuslineGitChanges::default();
    for line in status.lines() {
        let bytes = line.as_bytes();
        if bytes.len() < 2 {
            continue;
        }
        let (index, worktree) = (bytes[0], bytes[1]);
        if (index == b'?' && worktree == b'?') || index == b'A' || worktree == b'A' {
            changes.added += 1;
        } else if index == b'D' || worktree == b'D' {
            changes.deleted += 1;
        } else {
            changes.modified += 1;
        }
    }
    (changes.added + changes.modified + changes.deleted > 0).then_some(changes)
}

fn statusline_relative_path(file: Option<&str>, workspace_root: &Path) -> Option<String> {
    let file = file?;
    let path = expand_user_path(file).ok()?;
    let absolute = if path.is_absolute() {
        path
    } else {
        get_workspace_path().join(path)
    };
    Some(
        absolute
            .strip_prefix(workspace_root)
            .unwrap_or(&absolute)
            .to_string_lossy()
            .into_owned(),
    )
}

fn statusline_workspace_name(workspace_root: &Path) -> String {
    workspace_root
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|name| !name.is_empty())
        .unwrap_or("workspace")
        .to_string()
}

fn statusline_file_is_read_only(file: Option<&str>) -> bool {
    file.and_then(|file| expand_user_path(file).ok())
        .and_then(|path| fs::metadata(path).ok())
        .is_some_and(|metadata| metadata.permissions().readonly())
}

fn statusline_file_size(bytes: usize) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let bytes = bytes as f64;
    if bytes >= GIB {
        format!("{:.1} GB", bytes / GIB)
    } else if bytes >= MIB {
        format!("{:.1} MB", bytes / MIB)
    } else if bytes >= KIB {
        format!("{:.1} KB", bytes / KIB)
    } else {
        format!("{} B", bytes as usize)
    }
}

fn statusline_symbol_at(buffer: &crate::buffer::Buffer, cursor_line: usize) -> Option<String> {
    let first_line = cursor_line.saturating_sub(199);
    (first_line..=cursor_line)
        .rev()
        .filter_map(|line| buffer.get(line))
        .find_map(|line| statusline_symbol_from_declaration(trim_line_ending(&line)))
}

fn statusline_symbol_from_declaration(line: &str) -> Option<String> {
    let mut declaration = line.trim_start();
    if let Some(heading) = declaration.strip_prefix('#') {
        let heading = heading.trim_start_matches('#').trim();
        return (!heading.is_empty()).then(|| heading.to_string());
    }
    for prefix in ["pub(crate) ", "pub(super) ", "pub ", "export ", "default "] {
        if let Some(rest) = declaration.strip_prefix(prefix) {
            declaration = rest;
            break;
        }
    }
    if let Some(rest) = declaration.strip_prefix("const fn ") {
        declaration = rest;
        let name = declaration
            .split(|character: char| {
                !(character.is_alphanumeric() || matches!(character, '_' | ':' | '-'))
            })
            .next()
            .unwrap_or_default();
        return (!name.is_empty()).then(|| name.to_string());
    }
    for prefix in ["async ", "unsafe "] {
        if let Some(rest) = declaration.strip_prefix(prefix) {
            declaration = rest;
        }
    }
    for keyword in [
        "fn ",
        "def ",
        "function ",
        "class ",
        "struct ",
        "enum ",
        "trait ",
        "interface ",
        "type ",
        "mod ",
    ] {
        if let Some(rest) = declaration.strip_prefix(keyword) {
            let name = rest
                .split(|character: char| {
                    !(character.is_alphanumeric() || matches!(character, '_' | ':' | '-'))
                })
                .next()
                .unwrap_or_default();
            return (!name.is_empty()).then(|| name.to_string());
        }
    }
    if let Some(rest) = declaration.strip_prefix("impl ") {
        let name = rest
            .split(|character: char| matches!(character, '{' | '<') || character.is_whitespace())
            .next()
            .unwrap_or_default();
        return (!name.is_empty()).then(|| format!("impl {name}"));
    }
    for keyword in ["const ", "let ", "var "] {
        let Some(rest) = declaration.strip_prefix(keyword) else {
            continue;
        };
        let (name, value) = rest.split_once('=')?;
        if value.contains("=>") || value.trim_start().starts_with("function") {
            let name = name.trim();
            return (!name.is_empty()).then(|| name.to_string());
        }
    }
    None
}

fn decoration_local_x(
    decoration: &crate::plugin::Decoration,
    segment: &super::display_layout::LineSegment,
    line_width: usize,
    line_is_blank: bool,
    content_width: usize,
) -> Option<usize> {
    match decoration.anchor {
        DecorationAnchor::Column => {
            if !segment.first_segment && !decoration.repeat_linebreak {
                return None;
            }

            if !segment.first_segment && decoration.repeat_linebreak {
                Some(decoration.column)
            } else if decoration.only_whitespace && line_is_blank {
                (decoration.column >= segment.start_col)
                    .then(|| decoration.column.saturating_sub(segment.start_col))
            } else if segment.contains_display_col(decoration.column) {
                Some(decoration.column.saturating_sub(segment.start_col))
            } else {
                None
            }
        }
        DecorationAnchor::Eol => {
            if segment.end_col < line_width {
                return None;
            }

            Some(segment.visual_offset + line_width.saturating_sub(segment.start_col))
        }
        DecorationAnchor::RightAlign => {
            if segment.end_col < line_width {
                return None;
            }

            let decoration_width = display_width(&decoration.text);
            Some(content_width.saturating_sub(decoration_width))
        }
    }
}

use super::display_layout::leading_whitespace_display_width;

fn queue_cell_attributes(output: &mut impl io::Write, cell_style: &Style) -> anyhow::Result<()> {
    if cell_style.bold {
        output.queue(style::SetAttribute(style::Attribute::Bold))?;
    } else {
        output.queue(style::SetAttribute(style::Attribute::NormalIntensity))?;
    }

    if cell_style.italic {
        output.queue(style::SetAttribute(style::Attribute::Italic))?;
    } else {
        output.queue(style::SetAttribute(style::Attribute::NoItalic))?;
    }

    Ok(())
}

pub(super) fn resolve_cell_colors(cell_style: &Style, theme_style: &Style) -> (Color, Color) {
    let theme_bg = theme_style.bg.unwrap_or(Color::Rgb { r: 0, g: 0, b: 0 });
    let theme_fg = theme_style.fg.unwrap_or(Color::Rgb {
        r: 255,
        g: 255,
        b: 255,
    });
    let fg = cell_style
        .fg
        .map_or(theme_fg, |fg| blend_color(fg, theme_bg));
    let bg = cell_style
        .bg
        .map_or(theme_bg, |bg| blend_color(bg, theme_bg));

    (fg, bg)
}

fn cursor_style_for_shape(shape: CursorShape) -> cursor::SetCursorStyle {
    match shape {
        CursorShape::Default => cursor::SetCursorStyle::DefaultUserShape,
        CursorShape::BlinkingBlock => cursor::SetCursorStyle::BlinkingBlock,
        CursorShape::SteadyBlock => cursor::SetCursorStyle::SteadyBlock,
        CursorShape::BlinkingUnderscore => cursor::SetCursorStyle::BlinkingUnderScore,
        CursorShape::SteadyUnderscore => cursor::SetCursorStyle::SteadyUnderScore,
        CursorShape::BlinkingBar => cursor::SetCursorStyle::BlinkingBar,
        CursorShape::SteadyBar => cursor::SetCursorStyle::SteadyBar,
    }
}

impl Editor {
    fn queue_theme_cursor_color(&mut self) -> anyhow::Result<()> {
        let surface = self
            .last_rendered_cursor_surface
            .as_ref()
            .unwrap_or(&self.theme.style);
        let cursor_color = self.theme.terminal_cursor_color(surface);
        write!(self.stdout, "\x1b]12;{}\x1b\\", cursor_color)?;

        Ok(())
    }

    fn update_terminal_cursor_surface(&mut self, buffer: &RenderBuffer) {
        self.last_rendered_cursor_surface = self
            .render_cursor_position()
            .and_then(|(x, y)| {
                (x < buffer.width && y < buffer.height)
                    .then(|| buffer.cells.get(y * buffer.width + x))
                    .flatten()
            })
            .map(|cell| cell.style.clone());
    }

    /// Returns the cells changed since the last rendered frame. The previous
    /// frame is updated after its diff has been sent to the terminal, so later
    /// partial renders can continue to draw into the caller-owned buffer.
    fn render_buffer_changes<'a>(&mut self, buffer: &'a RenderBuffer) -> Vec<Change<'a>> {
        let previous = self.previous_render_buffer.get_or_insert_with(|| {
            RenderBuffer::new(buffer.width, buffer.height, &Style::default())
        });

        if previous.width != buffer.width || previous.height != buffer.height {
            *previous = RenderBuffer::new(buffer.width, buffer.height, &Style::default());
        }

        if self.force_full_redraw {
            return buffer
                .cells
                .iter()
                .enumerate()
                .map(|(position, cell)| Change {
                    x: position % buffer.width,
                    y: position / buffer.width,
                    cell,
                })
                .collect();
        }

        buffer.diff(previous)
    }

    fn commit_render_buffer_changes(&mut self, changes: &[Change<'_>]) {
        self.previous_render_buffer
            .as_mut()
            .expect("render buffer diff requires a previous frame")
            .apply_changes(changes);
        self.force_full_redraw = false;
    }

    /// Renders the entire editor state to the terminal
    /// This is the main entry point for all rendering operations
    pub fn render(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        if self.defer_motion_render {
            self.request_motion_render(super::MotionRender::Full);
            return Ok(());
        }

        let _span = super::perf::PerfSpan::start("render:full");
        #[cfg(test)]
        {
            self.full_render_count += 1;
        }
        let prepare_span = super::perf::PerfSpan::start("render:prepare");
        self.update_gutter_width();
        self.apply_panel_layout();
        self.fix_cursor_pos();
        self.check_bounds();
        self.sync_to_window();
        drop(prepare_span);
        // Render all windows
        let windows_span = super::perf::PerfSpan::start("render:windows");
        let window_count = self.window_manager.window_count();
        for window_id in 0..window_count {
            self.render_window(buffer, window_id)?;
        }
        drop(windows_span);

        // Render window separators
        self.render_all_window_separators(buffer)?;

        // Startup splash over the pristine scratch window (docs/SPLASH.md)
        self.render_splash(buffer);

        let mut active_panel_dividers = Vec::with_capacity(3);
        if let Some(super::DividerDrag {
            target: super::DividerResizeTarget::Panel { id, .. },
            ..
        }) = self.divider_drag.as_ref()
        {
            active_panel_dividers.push(id.as_str());
        }
        if let Some(mode) = self.pane_resize_mode.as_ref() {
            for target in [mode.vertical.as_ref(), mode.horizontal.as_ref()]
                .into_iter()
                .flatten()
            {
                if let super::DividerResizeTarget::Panel { id, .. } = target {
                    active_panel_dividers.push(id.as_str());
                }
            }
        }
        let panels_span = super::perf::PerfSpan::start("render:panels");
        self.panel_manager.render_with_active_dividers(
            buffer,
            &self.theme,
            &active_panel_dividers,
            self.config.window_borders_ascii,
        );
        drop(panels_span);

        // Render global UI elements
        let chrome_span = super::perf::PerfSpan::start("render:chrome");
        self.render_ui_chrome(buffer)?;
        // A modal workspace replaces editor chrome but remains below dialogs
        // and overlays so prompts and transient menus stay interactive.
        self.workspace_manager
            .render(buffer, &self.theme, self.picker_icons());
        if self.workspace_manager.is_active()
            && (self.last_error.is_some()
                || self.session_manager.warning().is_some()
                || self.config_diagnostics_banner().is_some())
        {
            self.draw_commandline(buffer);
        }
        self.render_dialog(buffer)?;

        // Render all plugins
        self.render_from_plugins(buffer)?;
        drop(chrome_span);

        // Update overlay positions and render them
        let overlays_span = super::perf::PerfSpan::start("render:overlays+cursor");
        self.update_and_render_overlays(buffer)?;

        self.update_terminal_cursor_surface(buffer);
        self.render_cursor_cell(buffer);
        self.last_rendered_cursor_position = self.render_cursor_position();
        drop(overlays_span);

        // Flush changes to terminal
        let diff_span = super::perf::PerfSpan::start("render:diff+flush");
        let changes = self.render_buffer_changes(buffer);
        self.render_diff(&changes)?;
        self.commit_render_buffer_changes(&changes);
        drop(diff_span);
        self.last_rendered_window = self.window_manager.active_stable_window_id();
        self.render_generation = self.render_generation.wrapping_add(1);

        Ok(())
    }

    pub(crate) fn uses_synthetic_block_cursor(&self) -> bool {
        let dialog_allows_editor_cursor = self
            .current_dialog
            .as_ref()
            .map(|dialog| dialog.allows_event_passthrough())
            .unwrap_or(true);

        self.is_focused
            && dialog_allows_editor_cursor
            && !self.has_term()
            && !self.panel_manager.has_focused_panel()
            && !self.is_waiting_for_key_sequence()
            && matches!(
                self.mode,
                Mode::Normal | Mode::Visual | Mode::VisualLine | Mode::VisualBlock
            )
    }

    fn render_cursor_cell(&self, buffer: &mut RenderBuffer) {
        if self.workspace_manager.is_active() || !self.uses_synthetic_block_cursor() {
            return;
        }

        let Some((x, y)) = self.render_cursor_position() else {
            return;
        };
        if x >= buffer.width || y >= buffer.height {
            return;
        }

        let pos = y * buffer.width + x;
        let Some(cell) = buffer.cells.get_mut(pos) else {
            return;
        };

        let cursor_style = self.theme.synthetic_cursor_style(&cell.style);
        cell.style = cursor_style;
    }

    pub(crate) fn render_motion_frame(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        if self.defer_motion_render {
            self.request_motion_render(super::MotionRender::Window);
            return Ok(());
        }
        if !self.can_reuse_editor_surfaces() {
            return self.render(buffer);
        }
        let _span = super::perf::PerfSpan::start("render:motion_frame");
        self.render_editor_frame(buffer, /*all_windows*/ false)
    }

    /// Repaint editor-owned decorations without rebuilding unchanged docked panes.
    pub(crate) fn render_editor_windows_frame(
        &mut self,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<()> {
        if self.defer_motion_render {
            self.request_motion_render(super::MotionRender::EditorWindows);
            return Ok(());
        }
        if !self.can_reuse_editor_surfaces() {
            return self.render(buffer);
        }
        let _span = super::perf::PerfSpan::start("render:editor_windows");
        self.render_editor_frame(buffer, /*all_windows*/ true)
    }

    fn can_reuse_editor_surfaces(&self) -> bool {
        self.previous_render_buffer.is_some()
            && !self.force_full_redraw
            && self.last_rendered_window == self.window_manager.active_stable_window_id()
            && self.current_dialog.is_none()
            && !self.keymap_hints_visible
            && !self.panel_manager.has_focused_panel()
            && !self.workspace_manager.is_active()
            && !self.overlay_manager.has_visible_content()
            && self.render_commands.is_empty()
            && !self.has_term()
    }

    fn render_editor_frame(
        &mut self,
        buffer: &mut RenderBuffer,
        all_windows: bool,
    ) -> anyhow::Result<()> {
        self.update_gutter_width();
        self.fix_cursor_pos();
        self.sync_to_window();
        if all_windows {
            for window_id in 0..self.window_manager.window_count() {
                self.render_window(buffer, window_id)?;
            }
            self.render_all_window_separators(buffer)?;
        } else {
            self.render_window(buffer, self.window_manager.active_window_id())?;
        }
        self.render_ui_chrome(buffer)?;
        self.render_dialog(buffer)?;
        self.update_and_render_overlays(buffer)?;
        self.update_terminal_cursor_surface(buffer);
        self.render_cursor_cell(buffer);
        self.last_rendered_cursor_position = self.render_cursor_position();

        let changes = self.render_buffer_changes(buffer);
        self.render_diff(&changes)?;
        self.commit_render_buffer_changes(&changes);
        self.last_rendered_window = self.window_manager.active_stable_window_id();
        self.render_generation = self.render_generation.wrapping_add(1);

        Ok(())
    }

    pub(crate) fn can_render_cursor_motion_delta(&self) -> bool {
        self.terminal_output_enabled
            && self.can_reuse_editor_surfaces()
            && !self.relative_line_numbers_enabled()
            && self.uses_synthetic_block_cursor()
            && self.current_dialog.is_none()
            && !self.panel_manager.has_focused_panel()
            && !self.is_visual()
            && self.active_search.is_none()
            && (self.search_term.is_empty()
                || !self.config.search.hlsearch
                || self.search_highlights_suppressed)
            && !self.overlay_manager.has_visible_content()
            && !self.active_buffer_has_diagnostics()
    }

    fn active_buffer_has_diagnostics(&self) -> bool {
        let Ok(Some(uri)) = self.current_buffer().uri() else {
            return false;
        };

        self.diagnostics
            .get(&uri)
            .is_some_and(|diagnostics| !diagnostics.is_empty())
    }

    pub(crate) fn render_cursor_motion_delta(
        &mut self,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<()> {
        let _span = super::perf::PerfSpan::start("render:motion_delta");
        self.update_gutter_width();
        self.fix_cursor_pos();
        self.sync_to_window();

        let new_cursor_position = self.render_cursor_position();
        let active_window_id = self.window_manager.active_window_id();
        let matching_bracket_rows = self
            .window_manager
            .window_at_index(active_window_id)
            .cloned()
            .map(|window| {
                self.matching_bracket_points(&window)
                    .into_iter()
                    .map(|point| point.y)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut rows = Vec::with_capacity(
            4 + self.last_rendered_bracket_rows.len() + matching_bracket_rows.len(),
        );
        if let Some((_, y)) = self.last_rendered_cursor_position {
            rows.push(y);
        }
        if let Some((_, y)) = new_cursor_position {
            rows.push(y);
        }
        rows.extend(self.last_rendered_bracket_rows.iter().copied());
        rows.extend(matching_bracket_rows.iter().copied());

        let status_y = (self.size.1 as usize).saturating_sub(2);
        let command_y = (self.size.1 as usize).saturating_sub(1);
        rows.push(status_y);
        rows.push(command_y);

        // The content renderer walks one forward-only StyleCursor across the
        // rows, so they must be unique and in increasing order; a duplicate
        // row (same-row motion) or an earlier row (upward motion) would
        // otherwise re-render after its spans were already consumed, losing
        // its syntax highlighting.
        rows.sort_unstable();
        rows.dedup();

        let snapshots = buffer.snapshot_rows(&rows);
        self.render_window_rows(buffer, active_window_id, &rows)?;
        self.draw_statusline(buffer);
        self.draw_commandline(buffer);
        self.update_terminal_cursor_surface(buffer);
        self.render_cursor_cell(buffer);

        let changes = buffer.diff_row_snapshots(&snapshots);
        self.render_diff(&changes)?;
        self.commit_render_buffer_changes(&changes);
        self.last_rendered_cursor_position = new_cursor_position;
        self.last_rendered_bracket_rows = matching_bracket_rows;
        self.render_generation = self.render_generation.wrapping_add(1);

        Ok(())
    }

    /// Flushes one complete, document-aware frame after an edit.
    pub(super) fn render_edited_window_rows(
        &mut self,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<()> {
        self.render(buffer)
    }

    fn render_window_rows(
        &mut self,
        buffer: &mut RenderBuffer,
        window_id: usize,
        terminal_rows: &[usize],
    ) -> anyhow::Result<()> {
        let window_data = self.window_manager.window_at_index(window_id).cloned();
        let Some(window) = window_data else {
            return Ok(());
        };

        let local_rows = terminal_rows
            .iter()
            .filter_map(|row| row.checked_sub(window.position.y))
            .filter_map(|row| row.checked_sub(self.window_content_top(&window)))
            .filter(|row| *row < self.window_content_height(&window))
            .collect::<Vec<_>>();
        if local_rows.is_empty() {
            return Ok(());
        }

        self.render_gutter_rows_in_window(buffer, &window, window_id, &local_rows);
        self.render_main_content_rows_in_window(buffer, &window, &local_rows)?;
        self.render_line_highlight_rows_in_window(buffer, &window, &local_rows);
        self.render_matching_brackets_in_window(buffer, &window, Some(terminal_rows));

        Ok(())
    }

    fn render_gutter_rows_in_window(
        &mut self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
        window_id: usize,
        local_rows: &[usize],
    ) {
        let layout = self.layout_for_window(window);
        let window_buffer = &self.buffer_manager[window.buffer_index];
        let mut line_count = window_buffer.navigable_line_count();
        if self.window_manager.active_window_id() == window_id && self.is_insert() {
            line_count = line_count.max(window.vtop + window.cy + 1);
        }

        for &row in local_rows {
            self.render_gutter_row_in_window(buffer, window, &layout, line_count, row);
        }
    }

    fn render_gutter_row_in_window(
        &self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
        layout: &DisplayLayout,
        line_count: usize,
        row: usize,
    ) {
        if layout.inline_comment_row(row).is_some() {
            let style = &self.theme.style;
            let term_y = self.window_to_terminal_y(window, row);
            let width = self.gutter_width_for_window(window) + 1;
            buffer.fill_rect(window.position.x, term_y, width, 1, ' ', style, &self.theme);
            if width > 1 {
                let guide = if self.config.window_borders_ascii {
                    ":"
                } else {
                    "┆"
                };
                buffer.set_text(
                    window.position.x + width - 2,
                    term_y,
                    guide,
                    &self.theme.inline_comment_guide_style(),
                );
            }
            return;
        }
        let number_width = self.line_number_width_for_window(window);
        let gutter_style = self.theme.gutter_style.fallback_bg(&self.theme.style);
        let segment = layout.row(row).filter(|segment| segment.first_segment);
        let cursor_line = window.vtop + window.cy;
        let is_cursor_line =
            segment.is_some_and(|segment| segment.line < line_count && segment.line == cursor_line);
        let line_number = segment
            .filter(|segment| segment.line < line_count)
            .map(|segment| {
                if self.relative_line_numbers_enabled() {
                    if is_cursor_line {
                        segment.line + 1
                    } else {
                        segment.line.abs_diff(cursor_line)
                    }
                } else {
                    segment.line + 1
                }
            });
        let number_text = line_number
            .map(|line_number| {
                if self.relative_line_numbers_enabled() && is_cursor_line {
                    format!("{line_number:<number_width$} ")
                } else {
                    format!("{line_number:>number_width$} ")
                }
            })
            .unwrap_or_else(|| " ".repeat(number_width + 1));
        let text = format!("{}{number_text}", " ".repeat(GUTTER_SIGN_COLUMN_WIDTH));
        let term_x = window.position.x;
        let term_y = self.window_to_terminal_y(window, row);
        buffer.set_text(term_x, term_y, &text, &gutter_style);
        if is_cursor_line {
            buffer.set_text(
                term_x + GUTTER_SIGN_COLUMN_WIDTH,
                term_y,
                &number_text,
                &self.theme.current_line_number_style(),
            );
        }

        let Some(segment) = segment else {
            return;
        };
        let Some(sign) = self
            .gutter_sign_manager
            .visible_sign(window.buffer_index, segment.line)
        else {
            return;
        };
        buffer.set_text(
            term_x,
            term_y,
            &sign.text,
            &sign.style.fallback_bg(&gutter_style),
        );
    }

    fn render_main_content_rows_in_window(
        &mut self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
        local_rows: &[usize],
    ) -> anyhow::Result<()> {
        let layout = self.layout_for_window(window);
        let style_info = self.viewport_highlight_spans(
            window.buffer_index,
            window.vtop,
            self.window_content_height(window),
        )?;
        let theme_style = self.theme.style.clone();
        let mut style_cursor = StyleCursor::new(&style_info);
        let gutter_width = self.gutter_width_for_window(window);
        let content_start = gutter_width + 1;
        let content_width = self.window_content_width(window);
        let tab_width = self
            .indentation_for_buffer_index(window.buffer_index)
            .shift_width
            .max(1);
        let mut cached_line: Option<(usize, String)> = None;

        for &row in local_rows {
            let term_y = self.window_to_terminal_y(window, row);
            let term_x = self.window_to_terminal_x(window, content_start);

            if let Some(comment) = layout.inline_comment_row(row) {
                self.render_inline_comment_row_in_window(buffer, window, comment);
                continue;
            }
            self.fill_line_in_window(buffer, term_x, term_y, content_width, &theme_style);

            let Some(segment) = layout.row(row) else {
                continue;
            };
            if cached_line.as_ref().map(|(line, _)| *line) != Some(segment.line) {
                cached_line = self.buffer_manager[window.buffer_index]
                    .get(segment.line)
                    .map(|line| (segment.line, line));
            }
            let Some((_, line)) = cached_line.as_ref() else {
                continue;
            };
            let line = trim_line_ending(line);
            let mut grapheme_col = segment.start_grapheme_col;
            for (byte_offset, grapheme) in
                line[segment.start_byte..segment.end_byte].grapheme_indices(true)
            {
                if grapheme_col < segment.start_col {
                    grapheme_col += if grapheme == "\t" {
                        tab_width - (grapheme_col % tab_width)
                    } else {
                        display_width(grapheme)
                    };
                    continue;
                }
                let local_x =
                    segment.visual_offset + grapheme_col.saturating_sub(segment.start_col);
                if local_x >= content_width {
                    break;
                }

                let style = style_cursor
                    .style_at(segment.source_offset + segment.start_byte + byte_offset)
                    .unwrap_or(&theme_style);
                let term_x = self.window_to_terminal_x(window, content_start + local_x);
                if grapheme == "\t" {
                    let tab_span = tab_width - (grapheme_col % tab_width);
                    buffer.set_text(term_x, term_y, &" ".repeat(tab_span), style);
                } else {
                    buffer.set_text(term_x, term_y, grapheme, style);
                }

                grapheme_col += if grapheme == "\t" {
                    tab_width - (grapheme_col % tab_width)
                } else {
                    display_width(grapheme)
                };
            }
            self.render_decorations_for_segment(
                buffer,
                window,
                segment,
                line,
                content_start,
                content_width,
            );
        }

        Ok(())
    }

    fn render_line_highlight_rows_in_window(
        &mut self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
        local_rows: &[usize],
    ) {
        if self.is_visual() || self.current_dialog.is_some() || !window.active {
            return;
        }
        let Some(ref style) = self.theme.line_highlight_style else {
            return;
        };
        let Some(bg) = style.bg else {
            return;
        };

        let layout = self.layout_for_window(window);
        let buffer_y = window.vtop + window.cy;
        let gutter_width = self.gutter_width_for_window(window);
        let start_x = window.position.x + gutter_width + 1;
        let end_x = window.position.x + window.inner_width() - 1;

        for segment in layout
            .rows
            .iter()
            .filter(|segment| segment.line == buffer_y && local_rows.contains(&segment.row))
        {
            let term_y = self.window_to_terminal_y(window, segment.row);
            buffer.set_bg_for_range(
                Point::new(start_x, term_y),
                Point::new(end_x, term_y),
                &bg,
                &self.theme,
            );
        }
    }

    /// Renders a single window
    /// Whether the startup splash should render this frame. Visibility is
    /// recomputed per render; once its conditions fail after it has been
    /// shown, the splash is latched off for the rest of the session.
    fn splash_should_render(&mut self) -> bool {
        if self.splash_dismissed {
            return false;
        }
        if !self.config.splash.unwrap_or(true) || self.config.startup_file_count != 0 {
            return false;
        }
        let pristine = self.window_manager.windows().len() == 1
            && self.buffer_manager.len() == 1
            && self.buffer_manager[0].is_unnamed()
            && self.buffer_manager[0].is_blank()
            && !self.buffer_manager[0].is_dirty();
        if !pristine {
            if self.splash_shown {
                self.splash_dismissed = true;
            }
            return false;
        }
        true
    }

    fn render_splash(&mut self, buffer: &mut RenderBuffer) {
        if !self.splash_should_render() {
            return;
        }
        let window_data = {
            let windows = self.window_manager.windows();
            let active = self.window_manager.active_window_id();
            windows.get(active).map(|window| (*window).clone())
        };
        let Some(window) = window_data else {
            return;
        };
        let content_width = self.window_content_width(&window);
        let content_height = self.window_content_height(&window);
        let Some(block) = splash::block(content_width, content_height, env!("CARGO_PKG_VERSION"))
        else {
            return;
        };
        self.splash_shown = true;
        let palette = splash::palette(&self.theme);
        let block_width = block.iter().map(splash::Line::width).max().unwrap_or(0);
        let content_start = self.gutter_width_for_window(&window) + 1;
        let x0 = self.window_to_terminal_x(&window, content_start)
            + content_width.saturating_sub(block_width) / 2;
        let top = content_height.saturating_sub(block.len()) / 2;
        for (row, line) in block.iter().enumerate() {
            let term_y = self.window_to_terminal_y(&window, top + row);
            let mut x = x0;
            for span in &line.spans {
                buffer.set_text(x, term_y, &span.text, palette.style(span.role));
                x += span.text.chars().count();
            }
        }
    }

    fn render_window(&mut self, buffer: &mut RenderBuffer, window_id: usize) -> anyhow::Result<()> {
        // Clone the window data to avoid borrowing issues
        if let Some(window) = self.window_manager.window_at_index(window_id).cloned() {
            self.render_window_bar(buffer, &window);

            // Render the gutter for this window
            self.render_gutter_in_window(buffer, &window, window_id)?;

            // Render the window content with proper boundaries
            self.render_main_content_in_window(buffer, &window)?;

            // Render overlays within window bounds
            self.render_overlays_in_window(buffer, &window)?;
        }

        Ok(())
    }

    fn render_window_bar(&self, buffer: &mut RenderBuffer, window: &crate::window::Window) {
        let Some(rendered) = self
            .window_bar_manager
            .render(window.id, window.inner_width())
        else {
            return;
        };

        let mut base_style = rendered.style.resolve(&self.theme);
        base_style.fg = base_style.fg.or(self.theme.style.fg);
        base_style.bg = base_style.bg.or(self.theme.style.bg);
        let blank = " ".repeat(window.inner_width());
        buffer.set_text(window.position.x, window.position.y, &blank, &base_style);

        let mut x = window.position.x;
        let end = window.position.x + window.inner_width();
        for segment in rendered.segments {
            if x >= end {
                break;
            }
            let mut style = segment.style.resolve(&self.theme);
            style.fg = style.fg.or(base_style.fg);
            style.bg = style.bg.or(base_style.bg);
            buffer.set_text(x, window.position.y, &segment.text, &style);
            x += display_width(&segment.text);
        }
    }

    /// Render all window separators based on the split tree
    fn render_all_window_separators(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let separator_style = Style {
            fg: Some(Color::Rgb {
                r: 100,
                g: 100,
                b: 100,
            }),
            bg: None,
            bold: false,
            italic: false,
        };
        let mut active_dividers = Vec::with_capacity(3);
        if let Some(super::DividerDrag {
            target: super::DividerResizeTarget::Window { divider },
            ..
        }) = self.divider_drag.as_ref()
        {
            if let Some(span) = self.window_manager.divider_span(divider) {
                active_dividers.push(span);
            }
        }
        if let Some(mode) = self.pane_resize_mode.as_ref() {
            for target in [mode.vertical.as_ref(), mode.horizontal.as_ref()]
                .into_iter()
                .flatten()
            {
                if let super::DividerResizeTarget::Window { divider } = target {
                    if let Some(span) = self.window_manager.divider_span(divider) {
                        active_dividers.push(span);
                    }
                }
            }
        }
        let active_separator_style = (!active_dividers.is_empty()).then(|| {
            self.theme
                .active_divider_style(&separator_style, &self.theme.style)
        });

        // Get terminal size for bounds checking
        let (term_width, term_height) = (self.size.0 as usize, self.size.1 as usize);

        // Get all windows to find separators
        let windows = self.window_manager.windows();
        if windows.len() <= 1 {
            return Ok(());
        }

        // Use ASCII or Unicode characters based on configuration
        let use_ascii = self.config.window_borders_ascii;

        let left_positions = windows
            .iter()
            .map(|window| window.position.x)
            .collect::<HashSet<_>>();
        let top_positions = windows
            .iter()
            .map(|window| window.position.y)
            .collect::<HashSet<_>>();

        // Group each shared edge and its full extent in one pass. Looking up
        // the adjacent left/top position avoids comparing every window pair.
        let mut vertical_lines = HashMap::<usize, (usize, usize)>::new();
        let mut horizontal_lines = HashMap::<usize, (usize, usize)>::new();
        for window in &windows {
            let right = window.position.x + window.size.0;
            if left_positions.contains(&right.saturating_add(1)) {
                let extent = vertical_lines
                    .entry(right)
                    .or_insert((window.position.y, window.position.y + window.size.1));
                extent.0 = extent.0.min(window.position.y);
                extent.1 = extent.1.max(window.position.y + window.size.1);
            }

            let bottom = window.position.y + window.size.1;
            if top_positions.contains(&bottom.saturating_add(1)) {
                let extent = horizontal_lines
                    .entry(bottom)
                    .or_insert((window.position.x, window.position.x + window.size.0));
                extent.0 = extent.0.min(window.position.x);
                extent.1 = extent.1.max(window.position.x + window.size.0);
            }
        }

        // Pass 1: Draw basic segments into a temporary grid
        let mut temp_grid: HashMap<(usize, usize), char> = HashMap::new();

        // Draw vertical lines
        for (x, (y_start, y_end)) in &vertical_lines {
            for y in *y_start..*y_end {
                temp_grid.insert((*x, y), if use_ascii { '|' } else { '│' });
            }
        }

        // Draw horizontal lines, marking overlaps as cross
        for (y, (x_start, x_end)) in &horizontal_lines {
            for x in *x_start..*x_end {
                if let Some(existing) = temp_grid.get(&(x, *y)) {
                    if *existing == '|' || *existing == '│' {
                        // Overlap - mark as cross
                        temp_grid.insert((x, *y), if use_ascii { '+' } else { '┼' });
                    }
                } else {
                    temp_grid.insert((x, *y), if use_ascii { '-' } else { '─' });
                }
            }
        }

        // Helper functions to check if a character has vertical/horizontal components
        let has_vertical_component = |c: char| -> bool {
            matches!(
                c,
                '│' | '|' | '┼' | '+' | '├' | '┤' | '┬' | '┴' | '┌' | '┐' | '└' | '┘'
            )
        };

        let has_horizontal_component = |c: char| -> bool {
            matches!(
                c,
                '─' | '-' | '┼' | '+' | '┬' | '┴' | '├' | '┤' | '┌' | '┐' | '└' | '┘'
            )
        };

        // Pass 2: Refine intersections based on adjacent cells and draw each
        // final character directly, avoiding a second full grid allocation.
        for (x, y) in temp_grid.keys() {
            // Check adjacent cells
            let connects_up = if *y > 0 {
                temp_grid
                    .get(&(*x, y.saturating_sub(1)))
                    .map(|&c| has_vertical_component(c))
                    .unwrap_or(false)
            } else {
                false
            };

            let connects_down = if *y < term_height - 1 {
                temp_grid
                    .get(&(*x, y + 1))
                    .map(|&c| has_vertical_component(c))
                    .unwrap_or(false)
            } else {
                false
            };

            let connects_left = if *x > 0 {
                temp_grid
                    .get(&(x.saturating_sub(1), *y))
                    .map(|&c| has_horizontal_component(c))
                    .unwrap_or(false)
            } else {
                false
            };

            let connects_right = if *x < term_width - 1 {
                temp_grid
                    .get(&(x + 1, *y))
                    .map(|&c| has_horizontal_component(c))
                    .unwrap_or(false)
            } else {
                false
            };

            // Select the appropriate character based on connections
            let junction_char = if use_ascii {
                // ASCII mode
                if connects_up || connects_down || connects_left || connects_right {
                    if (connects_up || connects_down) && (connects_left || connects_right) {
                        '+' // Any junction or cross
                    } else if connects_up || connects_down {
                        '|' // Vertical line
                    } else {
                        '-' // Horizontal line
                    }
                } else {
                    '+' // Isolated point (shouldn't happen)
                }
            } else {
                // Unicode mode
                match (connects_up, connects_down, connects_left, connects_right) {
                    // Four-way cross
                    (true, true, true, true) => '┼',
                    // T-junctions
                    (true, true, true, false) => '┤', // T-junction right
                    (true, true, false, true) => '├', // T-junction left
                    (true, false, true, true) => '┴', // T-junction bottom
                    (false, true, true, true) => '┬', // T-junction top
                    // Corners
                    (true, false, false, true) => '└', // Corner bottom-left
                    (true, false, true, false) => '┘', // Corner bottom-right
                    (false, true, false, true) => '┌', // Corner top-left
                    (false, true, true, false) => '┐', // Corner top-right
                    // Straight lines
                    (true, true, false, false) => '│', // Vertical only
                    (false, false, true, true) => '─', // Horizontal only
                    // Single connections (line ends)
                    (true, false, false, false) => '│', // Vertical from top
                    (false, true, false, false) => '│', // Vertical to bottom
                    (false, false, true, false) => '─', // Horizontal from left
                    (false, false, false, true) => '─', // Horizontal to right
                    // No connections (shouldn't happen in practice)
                    (false, false, false, false) => '·', // Isolated point
                }
            };

            let style = if active_dividers.iter().any(|span| span.contains(*x, *y)) {
                active_separator_style.as_ref().unwrap_or(&separator_style)
            } else {
                &separator_style
            };
            buffer.set_char(*x, *y, junction_char, style, &self.theme);
        }

        Ok(())
    }

    fn render_from_plugins(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        while let Some(cmd) = self.render_commands.pop_front() {
            match cmd {
                RenderCommand::BufferText { x, y, text, style } => {
                    buffer.set_text(x, y, &text, &style);
                }
            }
        }

        Ok(())
    }

    fn update_and_render_overlays(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let cursor_pos = self.render_cursor_position().map(|(x, y)| Point::new(x, y));

        // Update positions for all overlays
        self.overlay_manager.update_positions(
            self.size.0 as usize,
            self.size.1 as usize,
            cursor_pos,
        );

        // Render all dirty overlays
        self.overlay_manager.render_all(buffer);

        Ok(())
    }

    /// Renders the main editor content (text buffer) within a window
    fn render_main_content_in_window(
        &mut self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
    ) -> anyhow::Result<()> {
        let layout = self.layout_for_window(window);
        let style_info = self.viewport_highlight_spans(
            window.buffer_index,
            window.vtop,
            self.window_content_height(window),
        )?;
        let theme_style = self.theme.style.clone();
        let mut style_cursor = StyleCursor::new(&style_info);

        let gutter_width = self.gutter_width_for_window(window);
        let content_start = gutter_width + 1;
        let content_width = self.window_content_width(window);
        let tab_width = self
            .indentation_for_buffer_index(window.buffer_index)
            .shift_width
            .max(1);
        let mut cached_line: Option<(usize, String)> = None;

        for segment in &layout.rows {
            let term_y = self.window_to_terminal_y(window, segment.row);
            let term_x = self.window_to_terminal_x(window, gutter_width + 1);
            self.fill_line_in_window(buffer, term_x, term_y, content_width, &theme_style);

            if cached_line.as_ref().map(|(line, _)| *line) != Some(segment.line) {
                cached_line = self.buffer_manager[window.buffer_index]
                    .get(segment.line)
                    .map(|line| (segment.line, line));
            }
            let Some((_, line)) = cached_line.as_ref() else {
                continue;
            };
            let line = trim_line_ending(line);
            let mut grapheme_col = segment.start_grapheme_col;
            for (byte_offset, grapheme) in
                line[segment.start_byte..segment.end_byte].grapheme_indices(true)
            {
                if grapheme_col < segment.start_col {
                    grapheme_col += if grapheme == "\t" {
                        tab_width - (grapheme_col % tab_width)
                    } else {
                        display_width(grapheme)
                    };
                    continue;
                }
                let local_x =
                    segment.visual_offset + grapheme_col.saturating_sub(segment.start_col);
                if local_x >= content_width {
                    break;
                }

                let style = style_cursor
                    .style_at(segment.source_offset + segment.start_byte + byte_offset)
                    .unwrap_or(&theme_style);
                let term_x = self.window_to_terminal_x(window, content_start + local_x);
                let term_y = self.window_to_terminal_y(window, segment.row);
                if grapheme == "\t" {
                    let tab_span = tab_width - (grapheme_col % tab_width);
                    buffer.set_text(term_x, term_y, &" ".repeat(tab_span), style);
                } else {
                    buffer.set_text(term_x, term_y, grapheme, style);
                }

                grapheme_col += if grapheme == "\t" {
                    tab_width - (grapheme_col % tab_width)
                } else {
                    display_width(grapheme)
                };
            }
            self.render_decorations_for_segment(
                buffer,
                window,
                segment,
                line,
                content_start,
                content_width,
            );
        }

        for comment in &layout.inline_comments {
            self.render_inline_comment_row_in_window(buffer, window, comment);
        }

        for y in layout.screen_height()..self.window_content_height(window) {
            let term_y = self.window_to_terminal_y(window, y);
            let term_x = self.window_to_terminal_x(window, gutter_width + 1);
            self.fill_line_in_window(buffer, term_x, term_y, content_width, &theme_style);
        }

        Ok(())
    }

    fn render_inline_comment_row_in_window(
        &mut self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
        comment: &super::display_layout::InlineCommentRow,
    ) {
        let term_y = self.window_to_terminal_y(window, comment.row);
        let content_start = self.gutter_width_for_window(window) + 1;
        let term_x = self.window_to_terminal_x(window, content_start);
        let content_width = self.window_content_width(window);
        let editor_style = self.theme.style.clone();
        let comment_style = self.theme.inline_comment_style();
        let block_width = comment.block_width.min(content_width);
        self.fill_line_in_window(buffer, term_x, term_y, content_width, &editor_style);
        let half_block = match comment.content {
            InlineCommentContent::TopEdge => Some('▄'),
            InlineCommentContent::BottomEdge => Some('▀'),
            InlineCommentContent::Text(_) => None,
        };
        if let Some(glyph) = half_block.filter(|_| !self.config.window_borders_ascii) {
            let edge_style = Style {
                fg: comment_style.bg,
                bg: editor_style.bg,
                ..Style::default()
            };
            buffer.fill_rect(
                term_x,
                term_y,
                block_width,
                1,
                glyph,
                &edge_style,
                &self.theme,
            );
            return;
        }
        self.fill_line_in_window(buffer, term_x, term_y, block_width, &comment_style);
        buffer.set_text(
            term_x + comment.text_offset,
            term_y,
            comment.content.text(),
            &comment_style,
        );
    }

    fn render_decorations_for_segment(
        &self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
        segment: &super::display_layout::LineSegment,
        line: &str,
        content_start: usize,
        content_width: usize,
    ) {
        let mut decorations = self
            .decoration_manager
            .decorations_for_line(window.buffer_index, segment.line)
            .peekable();
        if decorations.peek().is_none() {
            return;
        }

        let tab_width = self.indentation().shift_width.max(1);
        let leading_width = leading_whitespace_display_width(line, tab_width);
        let line_is_blank = line.trim().is_empty();
        let line_width = display_width_with_tabs(line, tab_width);

        for decoration in decorations {
            let Some(mut local_x) = decoration_local_x(
                decoration,
                segment,
                line_width,
                line_is_blank,
                content_width,
            ) else {
                continue;
            };

            if local_x >= content_width {
                continue;
            }

            let term_y = self.window_to_terminal_y(window, segment.row);
            for grapheme in decoration.text.graphemes(true) {
                if local_x >= content_width {
                    break;
                }

                let grapheme_width = display_width(grapheme).max(1);
                // The cell at `local_x` displays the line's display column
                // `start_col + (local_x - visual_offset)`. Cells inside the
                // break-indent virtual area (`line_col` of None) display
                // nothing and count as whitespace, so indent guides repeat
                // there like in vim; cells showing wrapped text are past the
                // leading whitespace and must not be painted over.
                let line_col = (local_x >= segment.visual_offset)
                    .then(|| segment.start_col + local_x - segment.visual_offset);
                let over_whitespace = line_is_blank
                    || line_col.is_none()
                    || line_col.is_some_and(|c| c < leading_width);
                if !decoration.only_whitespace || over_whitespace {
                    let term_x = self.window_to_terminal_x(window, content_start + local_x);
                    buffer.set_text(term_x, term_y, grapheme, &decoration.style);
                }

                local_x += grapheme_width;
            }
        }
    }

    /// Fill a line with the given style within window bounds
    fn fill_line_in_window(
        &mut self,
        buffer: &mut RenderBuffer,
        x: usize,
        y: usize,
        width: usize,
        style: &Style,
    ) {
        buffer.fill_rect(x, y, width, 1, ' ', style, &self.theme);
    }

    /// Renders overlays like selections, search highlights, diagnostics within a window
    fn render_overlays_in_window(
        &mut self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
    ) -> anyhow::Result<()> {
        // Only render overlays if this window is active
        if !window.active {
            return Ok(());
        }

        // Render diagnostics within window bounds
        self.render_diagnostics_in_window(buffer, window)?;

        // Render current line highlight
        if !self.is_visual() && self.current_dialog.is_none() && window.active {
            if let Some(ref style) = self.theme.line_highlight_style {
                let Some(bg) = style.bg else {
                    return Ok(());
                };
                let layout = self.layout_for_window(window);
                let buffer_y = window.vtop + window.cy;
                for segment in layout.rows.iter().filter(|segment| {
                    segment.line == buffer_y && segment.row < self.window_content_height(window)
                }) {
                    let term_y = self.window_to_terminal_y(window, segment.row);
                    let gutter_width = self.gutter_width_for_window(window);
                    let start_x = window.position.x + gutter_width + 1;
                    let end_x = window.position.x + window.inner_width() - 1;

                    buffer.set_bg_for_range(
                        Point::new(start_x, term_y),
                        Point::new(end_x, term_y),
                        &bg,
                        &self.theme,
                    );
                }
            }
        }

        self.render_search_highlights_in_window(buffer, window)?;
        self.render_matching_brackets_in_window(buffer, window, None);

        // Render selection last so its contrast guarantee is not overwritten by search highlights.
        if self.is_visual() && window.active {
            self.update_selection();

            if let Some(selection) = self.selection {
                let points = self.selected_cells_in_window(&Some(selection), window);
                buffer.apply_selection_for_points(
                    points,
                    &self.theme.editor_selection_style(),
                    &self.theme,
                    SelectionForegroundPriority::Selection,
                );
            }
        }

        Ok(())
    }

    pub(crate) fn matching_bracket_positions(&mut self) -> Option<[TextPosition; 2]> {
        if !matches!(
            self.mode,
            Mode::Normal | Mode::Insert | Mode::Visual | Mode::VisualLine | Mode::VisualBlock
        ) {
            return None;
        }

        let cursor = self.cursor_text_position();
        let buffer = self.buffer_manager.active_buffer()?;
        let bracket = if crate::matchit::BracketMatchCache::is_configured_delimiter(
            buffer,
            cursor,
            &self.config.matchit,
        ) {
            cursor
        } else if matches!(self.mode, Mode::Insert) {
            let previous = TextPosition::new(cursor.line, cursor.character.checked_sub(1)?);
            crate::matchit::BracketMatchCache::is_configured_delimiter(
                buffer,
                previous,
                &self.config.matchit,
            )
            .then_some(previous)?
        } else {
            return None;
        };
        let matching = crate::matchit::BracketMatchCache::matching_position(
            &mut self.bracket_match_cache,
            buffer,
            bracket,
            &self.config.matchit,
        )?;

        Some([bracket, matching])
    }

    fn matching_bracket_points(&mut self, window: &crate::window::Window) -> Vec<Point> {
        if !window.active || self.current_dialog.is_some() {
            return Vec::new();
        }
        let Some(positions) = self.matching_bracket_positions() else {
            return Vec::new();
        };

        let mut points = Vec::with_capacity(positions.len());
        for position in positions {
            let Some(line) = self
                .buffer_manager
                .get(window.buffer_index)
                .and_then(|buffer| buffer.get(position.line))
            else {
                continue;
            };
            let line = trim_line_ending(&line);
            let tab_width = self.tab_width_for_buffer_index(window.buffer_index);
            let start_col =
                display_width_with_tabs(char_prefix(line, position.character), tab_width);
            let end_col = display_width_with_tabs(
                char_prefix(line, position.character.saturating_add(1)),
                tab_width,
            );
            points.extend(self.display_col_range_points_in_window(
                window,
                position.line,
                start_col,
                end_col,
            ));
        }

        points
    }

    fn render_matching_brackets_in_window(
        &mut self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
        terminal_rows: Option<&[usize]>,
    ) {
        let mut points = self.matching_bracket_points(window);
        if terminal_rows.is_none() {
            self.last_rendered_bracket_rows = points.iter().map(|point| point.y).collect();
            self.last_rendered_bracket_rows.sort_unstable();
            self.last_rendered_bracket_rows.dedup();
        }
        if let Some(terminal_rows) = terminal_rows {
            points.retain(|point| terminal_rows.contains(&point.y));
        }
        if points.is_empty() {
            return;
        }

        buffer.apply_selection_for_points(
            points,
            &self.theme.editor_bracket_match_style(),
            &self.theme,
            SelectionForegroundPriority::Content,
        );
    }

    fn render_search_highlights_in_window(
        &mut self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
    ) -> anyhow::Result<()> {
        if !window.active {
            return Ok(());
        }

        let active_search = self.active_search.clone();
        let pattern = active_search
            .as_ref()
            .map(|search| search.draft.as_str())
            .filter(|draft| !draft.is_empty())
            .or_else(|| {
                (self.config.search.hlsearch
                    && !self.search_highlights_suppressed
                    && !self.search_term.is_empty())
                .then_some(self.search_term.as_str())
            })
            .map(str::to_string);
        let Some(pattern) = pattern else {
            return Ok(());
        };

        let layout = self.layout_for_window(window);
        let Some(visible_start) = layout.rows.first().map(|segment| segment.line) else {
            return Ok(());
        };
        let Some(visible_end) = layout.rows.last().map(|segment| segment.line) else {
            return Ok(());
        };
        if self
            .buffer_manager
            .get(window.buffer_index)
            .is_some_and(|buffer| {
                (visible_start..=visible_end).any(|line| {
                    buffer.line_range_byte_len(line, line + 1) > MAX_HIGHLIGHT_SLICE_BYTES
                })
            })
        {
            return Ok(());
        }

        let matches = match self.search_matches(&pattern) {
            Ok(matches) => matches,
            Err(_) => return Ok(()),
        };
        let first_visible = matches.partition_point(|match_| match_.end_y < visible_start);
        let current_match = active_search.as_ref().and_then(|search| search.preview);
        let current_start = current_match.map(|match_| (match_.start_x, match_.start_y));
        let cursor_start = (!self.is_search()).then(|| {
            (
                self.grapheme_to_char_on_line(self.cx, self.buffer_line()),
                self.buffer_line(),
            )
        });
        let selection_style = self.theme.editor_selection_style();
        let match_bg = self
            .theme
            .find_match_style
            .as_ref()
            .and_then(|style| style.bg);
        let highlight_bg = self
            .theme
            .find_match_highlight_style
            .as_ref()
            .and_then(|style| style.bg)
            .or(match_bg);
        for match_ in matches[first_visible..]
            .iter()
            .copied()
            .take_while(|match_| match_.start_y <= visible_end)
        {
            let is_current = current_start
                .or(cursor_start)
                .is_some_and(|start| start == (match_.start_x, match_.start_y));
            let bg = if is_current { match_bg } else { highlight_bg };
            let start_y = match_.start_y.max(visible_start);
            let end_y = match_.end_y.min(visible_end);

            for line_index in start_y..=end_y {
                let line = self
                    .buffer_manager
                    .get(window.buffer_index)
                    .and_then(|buffer| buffer.get(line_index))
                    .unwrap_or_default();
                let line = trim_line_ending(&line);
                let line_len = line.chars().count();
                let start_x = if line_index == match_.start_y {
                    match_.start_x
                } else {
                    0
                };
                let end_x = if line_index == match_.end_y {
                    match_.end_x
                } else {
                    line_len
                };
                if end_x <= start_x {
                    continue;
                }

                let tab_width = self.tab_width_for_buffer_index(window.buffer_index);
                let start_col = display_width_with_tabs(char_prefix(line, start_x), tab_width);
                let end_col = display_width_with_tabs(char_prefix(line, end_x), tab_width);
                let points =
                    self.display_col_range_points_in_window(window, line_index, start_col, end_col);
                if let Some(bg) = bg {
                    buffer.set_bg_for_points(points, &bg, &self.theme);
                } else {
                    buffer.apply_selection_for_points(
                        points,
                        &selection_style,
                        &self.theme,
                        SelectionForegroundPriority::Selection,
                    );
                }
            }
        }

        Ok(())
    }

    fn display_col_range_points_in_window(
        &self,
        window: &crate::window::Window,
        line_index: usize,
        start_col: usize,
        end_col: usize,
    ) -> Vec<Point> {
        if end_col <= start_col {
            return Vec::new();
        }

        let layout = self.layout_for_window(window);
        let gutter_width = self.gutter_width_for_window(window);
        let content_start = gutter_width + 1;
        let content_width = self.window_content_width(window);
        let mut points = Vec::new();

        for segment in layout
            .rows
            .iter()
            .filter(|segment| segment.line == line_index)
        {
            let start = start_col.max(segment.start_col);
            let end = end_col.min(segment.end_col);
            if end <= start {
                continue;
            }

            for col in start..end {
                let local_x = segment.visual_offset + col.saturating_sub(segment.start_col);
                if local_x >= content_width {
                    continue;
                }
                points.push(Point::new(
                    self.window_to_terminal_x(window, content_start + local_x),
                    self.window_to_terminal_y(window, segment.row),
                ));
            }
        }

        points
    }

    /// Renders a single diagnostic entry
    fn render_line_diagnostics(
        &self,
        buffer: &mut RenderBuffer,
        diagnostics: &[&Diagnostic],
        y: usize,
        x: usize,
        available_width: usize,
        style: &Style,
    ) -> anyhow::Result<()> {
        if let Some(row) = diagnostic_row(diagnostics, available_width) {
            buffer.set_text(x, y, &row, style);
        }

        Ok(())
    }

    /// Renders diagnostic information within a specific window
    fn render_diagnostics_in_window(
        &mut self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
    ) -> anyhow::Result<()> {
        // Get the buffer for this window
        let window_buffer = &self.buffer_manager[window.buffer_index];

        // Get current buffer URI
        let Some(uri) = window_buffer.uri()? else {
            return Ok(());
        };

        // Get diagnostics for current buffer
        let Some(diagnostics) = self.diagnostics.get(&uri) else {
            return Ok(());
        };

        let layout = self.layout_for_window(window);
        let Some(visible_start) = layout.rows.first().map(|segment| segment.line) else {
            return Ok(());
        };
        let Some(visible_end) = layout.rows.last().map(|segment| segment.line) else {
            return Ok(());
        };
        let diagnostics_by_line =
            diagnostics_by_visible_line(diagnostics, visible_start, visible_end);

        // Render diagnostics for visible lines in this window
        for (line_num, diagnostics) in diagnostics_by_line {
            let severity = diagnostics
                .first()
                .and_then(|diagnostic| diagnostic.severity.as_ref());
            let diagnostic_style = Style {
                fg: diagnostic_foreground(&self.theme, severity),
                bg: adjust_color_brightness(self.theme.style.bg, 10),
                italic: true,
                ..Style::default()
            };
            let Some(segment) = layout
                .rows
                .iter()
                .rev()
                .find(|segment| segment.line == line_num)
            else {
                continue;
            };

            // Get the line content to determine where to place the diagnostic
            let Some(line) = window_buffer.get(line_num) else {
                continue;
            };

            // Calculate diagnostic indicator position within window
            let gutter_width = self.gutter_width_for_window(window);
            let line_width = display_width_with_tabs(
                trim_line_ending(&line),
                self.tab_width_for_buffer_index(window.buffer_index),
            );
            if line_width > segment.end_col {
                continue;
            }
            let content_end = gutter_width + 1 + line_width.saturating_sub(segment.start_col);
            let indicator_x = content_end + 5; // Add some padding

            // Skip if diagnostic would be outside window
            if indicator_x >= window.inner_width() {
                continue;
            }

            // Available width for diagnostic message within window
            let available_width = window.inner_width() - indicator_x;
            if available_width < 3 {
                // Minimum space for indicator
                continue;
            }

            // Convert to terminal coordinates
            let term_x = self.window_to_terminal_x(window, indicator_x);
            let term_y = self.window_to_terminal_y(window, segment.row);

            // Render diagnostic indicator and truncated message
            self.render_line_diagnostics(
                buffer,
                &diagnostics[..],
                term_y,
                term_x,
                available_width,
                &diagnostic_style,
            )?;
        }

        Ok(())
    }

    /// Convert selected cells to window-relative coordinates
    fn selected_cells_in_window(
        &self,
        selection: &Option<Rect>,
        window: &crate::window::Window,
    ) -> Vec<Point> {
        let Some(selection) = selection else {
            return vec![];
        };

        let mut cells = Vec::new();

        for y in selection.y0..=selection.y1 {
            let (start_x, end_x) = match self.mode {
                Mode::Visual => {
                    if y == selection.y0 && y == selection.y1 {
                        (selection.x0, selection.x1)
                    } else if y == selection.y0 {
                        (selection.x0, self.last_cell_for_line(y))
                    } else if y == selection.y1 {
                        (0, selection.x1)
                    } else {
                        (0, self.last_cell_for_line(y))
                    }
                }
                Mode::VisualLine => (0, self.last_cell_for_line(y)),
                Mode::VisualBlock => (selection.x0, selection.x1),
                _ => unreachable!(),
            };

            let Some(line) = self.buffer_manager[window.buffer_index].get(y) else {
                continue;
            };
            let line = trim_line_ending(&line);
            let tab_width = self.tab_width_for_buffer_index(window.buffer_index);
            let start_col = grapheme_to_column_with_tabs(line, start_x, tab_width);
            let end_col = grapheme_to_column_with_tabs(line, end_x.saturating_add(1), tab_width);
            cells.extend(self.display_col_range_points_in_window(window, y, start_col, end_col));
            if line.is_empty() && start_x == 0 && end_x == 0 {
                cells.extend(self.display_col_range_points_in_window(window, y, 0, 1));
            }
        }

        cells
    }

    /// Renders UI chrome (gutter, statusline, command line)
    fn render_ui_chrome(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        // Don't render global gutter - each window renders its own gutter
        // self.render_gutter(buffer)?;

        // Render status line
        self.draw_statusline(buffer);

        // Render command line if needed
        self.draw_commandline(buffer);

        Ok(())
    }

    fn render_dialog(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        if let Some(current_dialog) = &self.current_dialog {
            current_dialog.draw(buffer)?;
        }

        if self.keymap_hints_visible {
            if let Some(KeyAction::Nested(mappings)) = &self.waiting_key_action {
                let hints =
                    crate::command_palette::keymap_hints(&self.keymap_hint_prefix, mappings);
                crate::ui::draw_keymap_hints(
                    buffer,
                    &self.theme,
                    &self.keymap_hint_prefix.join(" "),
                    &hints,
                )?;
            }
        }

        Ok(())
    }

    pub fn render_diff(&mut self, change_set: &[Change<'_>]) -> anyhow::Result<()> {
        if !self.terminal_output_enabled {
            self.draw_cursor_preserving_cursor_goal()?;
            return Ok(());
        }

        if change_set.is_empty() {
            self.set_cursor_style()?;
            self.draw_cursor_preserving_cursor_goal()?;
            self.flush_terminal_output()?;
            return Ok(());
        }

        self.stdout.queue(cursor::Hide)?;
        self.stdout.queue(terminal::DisableLineWrap)?;

        let mut i = 0;
        let mut text = String::new();
        while i < change_set.len() {
            let change = &change_set[i];
            let x = change.x;
            let y = change.y;
            let style = change.cell.style.clone();

            self.stdout.queue(MoveTo(x as u16, y as u16))?;
            self.queue_cell_style(&style)?;

            let mut next_x = x;
            text.clear();

            while i < change_set.len() {
                let change = &change_set[i];
                if change.y != y || change.x != next_x || change.cell.style != style {
                    break;
                }

                let cell_width = display_width(change.cell.text.as_str()).max(1);
                text.push_str(change.cell.text.as_str());
                next_x += cell_width;
                i += 1;

                while cell_width > 1 && i < change_set.len() {
                    let padding = &change_set[i];
                    if padding.y != y || padding.x >= next_x || padding.cell.text != " " {
                        break;
                    }
                    i += 1;
                }
            }

            self.stdout.queue(style::Print(text.as_str()))?;
        }

        self.stdout.queue(terminal::EnableLineWrap)?;
        self.set_cursor_style()?;
        self.draw_cursor_preserving_cursor_goal()?;
        self.flush_terminal_output()?;

        Ok(())
    }

    fn queue_cell_style(&mut self, cell_style: &Style) -> anyhow::Result<()> {
        let (fg, bg) = resolve_cell_colors(cell_style, &self.theme.style);
        self.stdout.queue(style::SetBackgroundColor(bg.into()))?;
        self.stdout.queue(style::SetForegroundColor(fg.into()))?;
        queue_cell_attributes(&mut self.stdout, cell_style)?;

        Ok(())
    }

    pub fn draw_statusline(&mut self, buffer: &mut RenderBuffer) {
        if self.size.0 == 0 || self.size.1 < 2 {
            return;
        }

        let left_sections = self.config.statusline.left.clone();
        let right_sections = self.config.statusline.right.clone();
        let configured = |candidate| {
            left_sections
                .iter()
                .chain(&right_sections)
                .any(|section| *section == candidate)
        };

        let (
            filename,
            file_path,
            dirty,
            position,
            buffer_index,
            cursor_line,
            byte_len,
            line_endings,
        ) = if let Some(window) = self.window_manager.active_window() {
            let window_buffer = &self.buffer_manager[window.buffer_index];
            let dirty = window_buffer.is_dirty();
            let window_count = self.window_manager.window_count();
            let window_indicator =
                if window_count > 1 && !configured(StatuslineSection::WindowIndex) {
                    format!(
                        " [{}/{}] ",
                        self.window_manager.active_window_id() + 1,
                        window_count
                    )
                } else {
                    " ".to_string()
                };
            (
                statusline_file_name(window_buffer.name()).to_string(),
                window_buffer.file.clone(),
                dirty,
                format!(
                    " {}:{}{}",
                    window.vtop + window.cy + 1,
                    window.cx + 1,
                    window_indicator
                ),
                window.buffer_index,
                window.vtop + window.cy,
                window_buffer.byte_len(),
                if window_buffer
                    .get(0)
                    .is_some_and(|line| line.ends_with("\r\n"))
                {
                    "CRLF"
                } else {
                    "LF"
                },
            )
        } else {
            let current = self.current_buffer();
            (
                statusline_file_name(current.name()).to_string(),
                current.file.clone(),
                current.is_dirty(),
                format!(" {}:{} ", self.vtop + self.cy + 1, self.cx + 1),
                self.buffer_manager.active_index(),
                self.vtop + self.cy,
                current.byte_len(),
                if current.get(0).is_some_and(|line| line.ends_with("\r\n")) {
                    "CRLF"
                } else {
                    "LF"
                },
            )
        };

        let term_width = self.size.0 as usize;
        let y = self.size.1 as usize - 2;
        let clear_line = " ".repeat(term_width);
        buffer.set_text(0, y, &clear_line, &self.theme.statusline_style.inner_style);

        let wants_git = configured(StatuslineSection::GitBranch)
            || configured(StatuslineSection::GitChanges)
            || configured(StatuslineSection::Workspace)
            || configured(StatuslineSection::RelativePath);
        if wants_git {
            self.refresh_statusline_git(
                file_path.as_deref(),
                configured(StatuslineSection::GitChanges),
            );
        }
        let workspace_root = self
            .statusline_git_cache
            .repository_root
            .clone()
            .or_else(|| {
                file_path
                    .as_deref()
                    .and_then(|file| self.lsp.workspace_root_for_file(file))
            })
            .unwrap_or_else(get_workspace_path);
        let diagnostics = configured(StatuslineSection::Diagnostics)
            .then(|| self.statusline_diagnostic_counts(buffer_index))
            .flatten();
        let current_symbol = configured(StatuslineSection::CurrentSymbol)
            .then(|| statusline_symbol_at(&self.buffer_manager[buffer_index], cursor_line))
            .flatten();
        let selection = configured(StatuslineSection::Selection)
            .then(|| self.statusline_selection_label())
            .flatten();
        let search_matches = configured(StatuslineSection::SearchMatches)
            .then(|| self.statusline_search_position())
            .flatten();
        let lsp_status = configured(StatuslineSection::LspStatus)
            .then(|| {
                file_path
                    .as_deref()
                    .and_then(|file| self.lsp.server_name_for_file(file))
            })
            .flatten();
        let formatter = configured(StatuslineSection::Formatter)
            .then(|| {
                let file = file_path.as_deref()?;
                if self.config.formatting.provider != FormattingProvider::Lsp {
                    if let Some(definition) = self.formatter_definition_for_file(file) {
                        let path = expand_user_path(file).ok()?;
                        if crate::formatter::is_available(definition, &path) {
                            return Some(if definition.name.trim().is_empty() {
                                definition.command.as_str()
                            } else {
                                definition.name.as_str()
                            });
                        }
                        if self.config.formatting.provider == FormattingProvider::External {
                            return Some("no fmt");
                        }
                    }
                }
                self.lsp.server_capabilities_for_file(file).map(|_| {
                    if self.lsp.supports_document_formatting(file) {
                        "fmt"
                    } else {
                        "no fmt"
                    }
                })
            })
            .flatten();
        let agent_activity = configured(StatuslineSection::AgentActivity)
            .then(|| self.statusline_agent_activity())
            .flatten();
        let syntax = configured(StatuslineSection::Syntax)
            .then(|| self.highlight_language_id_for_buffer_index(buffer_index))
            .flatten();
        let show_modified_separately = configured(StatuslineSection::Modified);
        let read_only = statusline_file_is_read_only(file_path.as_deref());
        let relative_path = statusline_relative_path(file_path.as_deref(), &workspace_root);
        let context = StatuslineContext {
            mode: if self.pane_resize_mode.is_some() {
                "RESIZE".to_string()
            } else {
                format_mode_name(&self.mode)
            },
            filename: if dirty && !show_modified_separately {
                format!("{filename} [+]")
            } else {
                filename
            },
            file_path,
            position,
            syntax,
            git_branch: self.statusline_git_cache.branch.clone(),
            diagnostics,
            git_changes: self.statusline_git_cache.changes,
            lsp_status,
            current_symbol,
            selection,
            recording: self
                .macro_recording
                .as_ref()
                .map(|recording| recording.register),
            search_matches,
            indentation: format!("spaces:{}", self.indentation().shift_width),
            encoding: "utf-8",
            line_endings,
            read_only,
            modified: dirty,
            workspace: statusline_workspace_name(&workspace_root),
            relative_path,
            buffer_index: format!("{}/{}", buffer_index + 1, self.buffer_manager.len().max(1)),
            window_index: format!(
                "{}/{}",
                self.window_manager.active_window_id() + 1,
                self.window_manager.window_count().max(1)
            ),
            file_size: statusline_file_size(byte_len),
            agent_activity,
            formatter,
            clock: Local::now().format("%H:%M").to_string(),
        };

        let base_style = self.theme.statusline_style.inner_style.clone();
        let icons = self.config.statusline.icons;
        let left = statusline_segments(
            left_sections,
            &context,
            &self.theme,
            icons.style,
            icons.color,
        );
        let mut right = statusline_segments(
            right_sections,
            &context,
            &self.theme,
            icons.style,
            icons.color,
        );
        right.reverse();

        let left_separator = self.theme.statusline_style.outer_chars[1];
        let right_separator = self.theme.statusline_style.outer_chars[2];
        let right_start =
            draw_statusline_right(buffer, y, term_width, right, &base_style, right_separator);
        draw_statusline_left(buffer, y, right_start, &left, &base_style, left_separator);
    }

    fn refresh_statusline_git(&mut self, file: Option<&str>, load_changes: bool) {
        const CACHE_TTL: Duration = Duration::from_secs(2);

        let search_dir = statusline_git_search_dir(file);
        let now = Instant::now();
        let cache_is_fresh = self.statusline_git_cache.search_dir.as_ref() == Some(&search_dir)
            && self
                .statusline_git_cache
                .refreshed_at
                .is_some_and(|refreshed| now.duration_since(refreshed) < CACHE_TTL)
            && (!load_changes || self.statusline_git_cache.changes_loaded);
        if cache_is_fresh {
            return;
        }

        let repository_root = git_repository_root(&search_dir);
        let branch = git_branch_from_head(&search_dir);
        let changes = load_changes
            .then(|| repository_root.as_deref().and_then(git_changes_from_status))
            .flatten();
        self.statusline_git_cache.search_dir = Some(search_dir);
        self.statusline_git_cache.repository_root = repository_root;
        self.statusline_git_cache.branch = branch;
        self.statusline_git_cache.changes = changes;
        self.statusline_git_cache.changes_loaded = load_changes;
        self.statusline_git_cache.refreshed_at = Some(now);
    }

    fn statusline_diagnostic_counts(&self, buffer_index: usize) -> Option<(usize, usize)> {
        let uri = self
            .buffer_manager
            .get(buffer_index)?
            .uri()
            .ok()
            .flatten()?;
        let diagnostics = self.diagnostics.get(&uri)?;
        let (mut errors, mut warnings) = (0, 0);
        for diagnostic in diagnostics {
            match diagnostic.severity.as_ref() {
                Some(DiagnosticSeverity::Error) => errors += 1,
                Some(DiagnosticSeverity::Warning) => warnings += 1,
                _ => {}
            }
        }
        (errors + warnings > 0).then_some((errors, warnings))
    }

    fn statusline_selection_label(&self) -> Option<String> {
        let selection = self.selection?;
        let (_, y0, _, y1) = selection.into();
        if self.mode == Mode::VisualLine || y0 != y1 {
            return Some(format!("{} lines", y0.abs_diff(y1) + 1));
        }
        self.selected_text()
            .map(|text| format!("{} chars", text.chars().count()))
    }

    fn statusline_search_position(&mut self) -> Option<(usize, usize)> {
        let active_search = self.active_search.clone();
        let pattern = active_search
            .as_ref()
            .map(|search| search.draft.as_str())
            .filter(|pattern| !pattern.is_empty())
            .or_else(|| (!self.search_term.is_empty()).then_some(self.search_term.as_str()))?
            .to_string();
        let matches = self.search_matches(&pattern).ok()?;
        if matches.is_empty() {
            return None;
        }
        let current = active_search
            .and_then(|search| search.preview)
            .map(|preview| (preview.start_y, preview.start_x))
            .unwrap_or((
                self.buffer_line(),
                self.grapheme_to_char_on_line(self.cx, self.buffer_line()),
            ));
        let index = matches
            .partition_point(|match_| (match_.start_y, match_.start_x) < current)
            .min(matches.len().saturating_sub(1));
        Some((index + 1, matches.len()))
    }

    fn statusline_agent_activity(&self) -> Option<String> {
        if self.agent_manager.has_active_sessions() {
            return Some("following".to_string());
        }
        self.agent_manager.has_bridge().then(|| "idle".to_string())
    }

    pub fn draw_commandline(&mut self, buffer: &mut RenderBuffer) {
        let style = &self.theme.style;
        let width = self.size.0 as usize;
        if width == 0 || self.size.1 == 0 {
            return;
        }

        let y = self.size.1 as usize - 1;
        let clear_line = " ".repeat(width);
        buffer.set_text(0, y, &clear_line, style);

        if !self.has_term() {
            let wc = if let Some(ref waiting_command) = self.waiting_command {
                waiting_command.clone()
            } else if let Some(ref repeater) = self.repeater {
                format!("{}", repeater)
            } else {
                String::new()
            };
            let wc_width = if wc.is_empty() { 0 } else { 10.min(width) };

            let mut messages = Vec::new();
            if let Some(error) = self.last_error.as_deref() {
                messages.push(error.to_string());
            }
            if let Some(warning) = self.session_manager.warning() {
                messages.push(warning.to_string());
            }
            if let Some(warning) = self.config_diagnostics_banner() {
                messages.push(warning);
            }
            let last_error = (!messages.is_empty()).then(|| messages.join(" | "));
            if let Some(last_error) = last_error {
                let width = width.saturating_sub(wc_width);
                let last_error = last_error.replace(['\r', '\n'], " ");
                let last_error = fit_display_width(&last_error, width);
                buffer.set_text(0, y, &last_error, style);
            }

            if wc_width > 0 {
                let wc = fit_display_width(&wc, wc_width);
                buffer.set_text(width.saturating_sub(wc_width), y, &wc, style);
            }

            return;
        }

        let text = if self.is_command() {
            &self.command
        } else {
            self.active_search_text().unwrap_or(&self.search_term)
        };
        let prefix = if self.is_command() {
            ":"
        } else {
            self.search_commandline_prefix()
        };
        let cmdline = format!("{}{}", prefix, text);
        buffer.set_text(0, y, &cmdline, style);
    }

    /// Renders the gutter with line numbers for a specific window
    fn render_gutter_in_window(
        &mut self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
        window_id: usize,
    ) -> anyhow::Result<()> {
        let layout = self.layout_for_window(window);
        let window_buffer = &self.buffer_manager[window.buffer_index];
        let mut line_count = window_buffer.navigable_line_count();
        if self.window_manager.active_window_id() == window_id && self.is_insert() {
            line_count = line_count.max(window.vtop + window.cy + 1);
        }

        for y in 0..self.window_content_height(window) {
            self.render_gutter_row_in_window(buffer, window, &layout, line_count, y);
        }

        Ok(())
    }

    pub fn draw_cursor(&mut self) -> anyhow::Result<()> {
        self.draw_cursor_with_goal_refresh(true)
    }

    pub(crate) fn draw_cursor_preserving_cursor_goal(&mut self) -> anyhow::Result<()> {
        self.draw_cursor_with_goal_refresh(false)
    }

    fn draw_cursor_with_goal_refresh(&mut self, refresh_goal: bool) -> anyhow::Result<()> {
        if refresh_goal {
            self.refresh_cursor_goal();
        }
        self.fix_cursor_pos();
        self.sync_to_window();

        if !self.terminal_output_enabled {
            return Ok(());
        }

        if !self.is_focused {
            self.stdout.queue(cursor::Hide)?;
            #[cfg(test)]
            {
                self.pending_terminal_cursor = Some(super::TerminalCursorState::Hidden);
            }
            return Ok(());
        }

        self.set_cursor_style()?;

        if self.uses_synthetic_block_cursor() {
            self.stdout.queue(cursor::Hide)?;
            #[cfg(test)]
            {
                self.pending_terminal_cursor = Some(super::TerminalCursorState::Hidden);
            }
            return Ok(());
        }

        let cursor_pos = self.render_cursor_position();

        if let Some((x, y)) = cursor_pos {
            self.stdout.queue(cursor::Show)?;
            self.stdout.queue(cursor::MoveTo(x as u16, y as u16))?;
            #[cfg(test)]
            {
                self.pending_terminal_cursor = Some(super::TerminalCursorState::Visible((x, y)));
            }
        } else {
            self.stdout.queue(cursor::Hide)?;
            #[cfg(test)]
            {
                self.pending_terminal_cursor = Some(super::TerminalCursorState::Hidden);
            }
        }

        Ok(())
    }

    pub(crate) fn render_cursor_position(&self) -> Option<(usize, usize)> {
        if let Some(current_dialog) = &self.current_dialog {
            if let Some(cursor_position) = current_dialog.cursor_position() {
                return Some(cursor_position);
            }
            if !current_dialog.allows_event_passthrough() {
                return None;
            }
        }

        if self.panel_manager.has_focused_panel() && !self.has_term() {
            return self
                .panel_manager
                .focused_text_panel_cursor_position(self.size.0 as usize, self.size.1 as usize);
        }

        if self.has_term() {
            Some((
                display_width(self.term()) + 1,
                (self.size.1 as usize).saturating_sub(1),
            ))
        } else {
            // Get the active window to calculate cursor position
            if let Some(window) = self.window_manager.active_window() {
                let buffer_y = window.vtop + window.cy;

                // Calculate the actual display column for the cursor
                let display_col =
                    if let Some(line) = self.buffer_manager[window.buffer_index].get(buffer_y) {
                        let line = trim_line_ending(&line);
                        self.display_col_for_cursor_goal(line, window.cursor_goal)
                    } else {
                        window.cx
                    };

                let layout = self.layout_for_window(window);
                let segment = layout.segment_for_cursor(buffer_y, display_col)?;
                let gutter_width = self.gutter_width_for_window(window);
                let term_x = window.position.x
                    + gutter_width
                    + 1
                    + segment
                        .screen_col_for_display_col(display_col, self.window_content_width(window));
                let term_y = self.window_to_terminal_y(window, segment.row);
                Some((term_x, term_y))
            } else {
                // Fallback to old behavior if no active window
                let display_col = if let Some(line) = self.viewport_line(self.cy) {
                    let line = trim_line_ending(&line);
                    self.display_col_for_cursor_goal(line, self.cursor_goal)
                } else {
                    self.cx
                };
                Some(((self.vx + display_col), self.cy))
            }
        }
    }

    /// Returns the inclusive terminal rows occupied by the visible part of a text range.
    pub(crate) fn render_text_range_rows_in_window(
        &self,
        window_id: WindowId,
        range: TextRange,
    ) -> Option<(usize, usize)> {
        let window = self.window_manager.window(window_id)?;
        let layout = self.layout_for_window(window);
        let last_line = range.end.line.saturating_sub(usize::from(
            range.end.character == 0 && range.end.line > range.start.line,
        ));
        let first = layout
            .rows
            .iter()
            .find(|segment| (range.start.line..=last_line).contains(&segment.line))?;
        let last = layout
            .rows
            .iter()
            .rev()
            .find(|segment| (range.start.line..=last_line).contains(&segment.line))?;
        Some((
            self.window_to_terminal_y(window, first.row),
            self.window_to_terminal_y(window, last.row),
        ))
    }

    pub(crate) fn active_cursor_shape(&self) -> CursorShape {
        if self.is_waiting_for_key_sequence() {
            return self.config.cursor.waiting;
        }

        let mode = if let Some(dialog) = self.current_dialog.as_ref() {
            dialog.cursor_mode().unwrap_or(self.mode)
        } else {
            self.panel_manager
                .focused_text_panel_cursor_mode()
                .unwrap_or(self.mode)
        };
        match mode {
            Mode::Normal => self.config.cursor.normal,
            Mode::Command => self.config.cursor.command,
            Mode::Insert => self.config.cursor.insert,
            Mode::Search => self.config.cursor.search,
            Mode::Visual => self.config.cursor.visual,
            Mode::VisualLine => self.config.cursor.visual_line,
            Mode::VisualBlock => self.config.cursor.visual_block,
        }
    }

    fn set_cursor_style(&mut self) -> anyhow::Result<()> {
        if !self.terminal_output_enabled {
            return Ok(());
        }

        self.queue_theme_cursor_color()?;
        self.stdout
            .queue(cursor_style_for_shape(self.active_cursor_shape()))?;

        Ok(())
    }

    fn update_gutter_width(&mut self) {
        self.vx = self.gutter_width() + 1;
    }
}

fn format_mode_name(mode: &Mode) -> String {
    match mode {
        Mode::Normal => "NORMAL".to_string(),
        Mode::Insert => "INSERT".to_string(),
        Mode::Command => "COMMAND".to_string(),
        Mode::Search => "SEARCH".to_string(),
        Mode::Visual => "VISUAL".to_string(),
        Mode::VisualLine => "V-LINE".to_string(),
        Mode::VisualBlock => "V-BLOCK".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        buffer::Buffer,
        config::Config,
        editor::display_layout::LineSegment,
        lsp::{LspManager, Position, Range},
        plugin::{Decoration, DecorationAnchor},
        theme::Theme,
        ui::CompletionUI,
    };

    fn rendering_test_editor(source: Buffer) -> Editor {
        let config = Config::default();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let mut editor =
            Editor::with_size(lsp, 60, 12, config, Theme::default(), vec![source]).unwrap();
        editor.test_disable_terminal_output();
        editor
    }

    #[test]
    fn partial_inline_comment_repaint_fills_only_the_padded_block() {
        let mut editor = rendering_test_editor(Buffer::new(None, "alpha\nbeta\n".to_string()));
        editor.cy = 1;
        editor.add_sample_inline_comment();
        editor.inline_comments[0].message = "note".to_string();
        editor.layout_cache.borrow_mut().clear();
        editor.sync_to_window();
        let window = editor.active_window_with_editor_view().unwrap();
        let layout = editor.layout_for_window(&window);
        let rows = layout
            .inline_comments
            .iter()
            .map(|comment| comment.row)
            .collect::<Vec<_>>();
        let mut frame = RenderBuffer::new(60, 12, &editor.theme.inline_comment_style());
        editor.render_gutter_rows_in_window(&mut frame, &window, 0, &rows);
        editor
            .render_main_content_rows_in_window(&mut frame, &window, &rows)
            .unwrap();
        let background = editor.theme.inline_comment_style().bg;
        let content_start = editor.gutter_width_for_window(&window) + 1;
        for comment in &layout.inline_comments {
            let y = editor.window_to_terminal_y(&window, comment.row);
            let cells = &frame.cells[y * 60..(y + 1) * 60];
            assert!(cells[..content_start]
                .iter()
                .all(|cell| cell.style.bg == editor.theme.style.bg));
            let block_cells = &cells[content_start..content_start + comment.block_width];
            match &comment.content {
                InlineCommentContent::Text(_) => {
                    assert!(block_cells.iter().all(|cell| cell.style.bg == background))
                }
                InlineCommentContent::TopEdge | InlineCommentContent::BottomEdge => {
                    let glyph = if comment.content == InlineCommentContent::TopEdge {
                        '▄'
                    } else {
                        '▀'
                    };
                    assert!(block_cells.iter().all(|cell| cell.c == glyph
                        && cell.style.fg == background
                        && cell.style.bg == editor.theme.style.bg));
                }
            }
            assert!(cells[content_start + comment.block_width..]
                .iter()
                .all(|cell| cell.style.bg == editor.theme.style.bg));
            assert_eq!(cells[content_start - 2].text, "┆");
        }
    }

    #[test]
    fn workspace_render_keeps_commandline_messages_visible() {
        let mut editor = rendering_test_editor(Buffer::new(None, String::new()));
        editor.workspace_manager.open(
            "git-dashboard".to_string(),
            crate::plugin::WorkspaceConfig {
                title: "Git".to_string(),
                ..crate::plugin::WorkspaceConfig::default()
            },
        );
        editor.workspace_manager.update(
            "git-dashboard",
            crate::plugin::WorkspaceModel {
                footer: vec![crate::plugin::PanelSegment {
                    text: "s stage  q close".to_string(),
                    style: None,
                    semantic: None,
                }],
                ..crate::plugin::WorkspaceModel::default()
            },
            &editor.theme,
        );
        editor.last_error = Some("fatal: index.lock already exists".to_string());
        let mut buffer = RenderBuffer::new(60, 12, &Style::default());

        editor.render(&mut buffer).unwrap();

        let commandline = buffer
            .cells
            .chunks(buffer.width)
            .last()
            .unwrap()
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>();
        assert!(commandline.contains("fatal: index.lock already exists"));
        assert!(!commandline.contains("s stage"));
    }

    #[test]
    fn focused_agent_composer_uses_its_prompt_local_cursor_shape() {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

        let mut config = Config::default();
        config.cursor.normal = CursorShape::SteadyBlock;
        config.cursor.insert = CursorShape::SteadyBar;
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            lsp,
            60,
            12,
            config,
            Theme::default(),
            vec![Buffer::new(None, "background".to_string())],
        )
        .unwrap();
        editor.test_create_text_panel(
            "agent",
            crate::plugin::PanelConfig {
                side: crate::plugin::PanelSide::Right,
                width: 32,
                composer: Some(crate::plugin::TextPanelComposerConfig {
                    placeholder: "Ask".to_string(),
                    rows: 2,
                }),
                ..crate::plugin::PanelConfig::default()
            },
        );
        assert!(editor.test_focus_text_panel_composer("agent"));

        assert_eq!(editor.active_cursor_shape(), CursorShape::SteadyBar);
        editor.panel_manager.handle_focused_text_input(
            &Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            60,
        );
        assert_eq!(editor.active_cursor_shape(), CursorShape::SteadyBlock);
    }

    fn rendered_rows(buffer: &RenderBuffer) -> Vec<String> {
        buffer
            .cells
            .chunks(buffer.width)
            .map(|row| row.iter().map(|cell| cell.text.as_str()).collect())
            .collect()
    }

    fn diagnostic(message: &str) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 1,
                },
            },
            severity: None,
            code: None,
            source: None,
            message: message.to_string(),
            related_information: None,
            data: None,
            tags: None,
        }
    }

    fn diagnostic_with_severity(message: &str, severity: DiagnosticSeverity) -> Diagnostic {
        let mut diagnostic = diagnostic(message);
        diagnostic.severity = Some(severity);
        diagnostic
    }

    fn statusline_test_theme() -> Theme {
        let mut theme = Theme::default();
        theme.statusline_style.inner_style = Style {
            fg: Some(Color::Rgb {
                r: 220,
                g: 220,
                b: 220,
            }),
            bg: Some(Color::Rgb {
                r: 20,
                g: 30,
                b: 40,
            }),
            ..Style::default()
        };
        theme.statusline_style.outer_style = Style {
            fg: Some(Color::Rgb { r: 0, g: 0, b: 0 }),
            bg: Some(Color::Rgb {
                r: 80,
                g: 180,
                b: 220,
            }),
            ..Style::default()
        };
        theme.colors.insert(
            "editorError.foreground".to_string(),
            Color::Rgb {
                r: 242,
                g: 85,
                b: 90,
            },
        );
        theme.colors.insert(
            "editorWarning.foreground".to_string(),
            Color::Rgb {
                r: 213,
                g: 164,
                b: 88,
            },
        );
        theme
    }

    fn rendered_statusline(editor: &mut Editor, width: usize, height: usize) -> RenderBuffer {
        let mut buffer = RenderBuffer::new(width, height, &Style::default());
        editor.draw_statusline(&mut buffer);
        buffer
    }

    fn statusline_cell<'a>(
        buffer: &'a RenderBuffer,
        height: usize,
        text: &str,
    ) -> &'a super::super::render_buffer::Cell {
        buffer.cells[(height - 2) * buffer.width..(height - 1) * buffer.width]
            .iter()
            .find(|cell| cell.text == text)
            .unwrap_or_else(|| panic!("statusline cell {text:?} was not rendered"))
    }

    fn segment(start_col: usize, end_col: usize, first_segment: bool) -> LineSegment {
        LineSegment {
            line: 0,
            row: 0,
            start_col,
            end_col,
            start_grapheme: 0,
            end_grapheme: 0,
            start_byte: 0,
            end_byte: 0,
            start_grapheme_col: start_col,
            source_offset: 0,
            first_segment,
            visual_offset: 0,
        }
    }

    fn decoration(anchor: DecorationAnchor, text: &str) -> Decoration {
        Decoration {
            buffer_index: Some(0),
            anchor,
            line: 0,
            column: 0,
            text: text.to_string(),
            style: Style::default(),
            priority: 0,
            repeat_linebreak: false,
            only_whitespace: false,
        }
    }

    #[test]
    fn diagnostic_row_fits_available_display_width() {
        let diagnostic = diagnostic("wide 👋 diagnostic 世界 message");
        let diagnostics = vec![&diagnostic];
        let row = diagnostic_row(&diagnostics, 12).unwrap();

        assert_eq!(display_width(&row), 12);
        assert!(row.ends_with('…'));
    }

    #[test]
    fn diagnostic_row_handles_cramped_width() {
        let diagnostic = diagnostic("message");
        let diagnostics = vec![&diagnostic, &diagnostic, &diagnostic];
        let row = diagnostic_row(&diagnostics, 2).unwrap();

        assert_eq!(display_width(&row), 2);
    }

    #[test]
    fn diagnostics_grouping_ignores_offscreen_lines() {
        let mut first_visible = diagnostic("first visible");
        first_visible.range.start.line = 4;
        let mut second_visible = diagnostic("second visible");
        second_visible.range.start.line = 4;
        let mut offscreen = diagnostic("offscreen");
        offscreen.range.start.line = 400;
        let diagnostics = vec![offscreen, first_visible, second_visible];

        let by_line = diagnostics_by_visible_line(&diagnostics, 3, 8);

        assert_eq!(by_line.len(), 1);
        assert_eq!(by_line[&4].len(), 2);
        assert_eq!(by_line[&4][0].message, "first visible");
        assert_eq!(by_line[&4][1].message, "second visible");
    }

    #[test]
    fn git_porcelain_changes_are_grouped_for_the_statusline() {
        let changes = git_changes_from_porcelain(
            " M src/editor.rs\nA  src/new.rs\nD  src/old.rs\n?? notes.txt\n",
        )
        .unwrap();

        assert_eq!(
            changes,
            StatuslineGitChanges {
                added: 2,
                modified: 1,
                deleted: 1,
            }
        );
        assert_eq!(git_changes_from_porcelain(""), None);
    }

    #[test]
    fn current_symbol_recognizes_common_declarations() {
        assert_eq!(
            statusline_symbol_from_declaration("pub async fn render_frame() {"),
            Some("render_frame".to_string())
        );
        assert_eq!(
            statusline_symbol_from_declaration("const fn capacity() -> usize {"),
            Some("capacity".to_string())
        );
        assert_eq!(
            statusline_symbol_from_declaration("const openPanel = () => {}"),
            Some("openPanel".to_string())
        );
        assert_eq!(
            statusline_symbol_from_declaration("## Configuration"),
            Some("Configuration".to_string())
        );
    }

    #[test]
    fn file_sizes_use_compact_binary_units() {
        assert_eq!(statusline_file_size(42), "42 B");
        assert_eq!(statusline_file_size(1536), "1.5 KB");
        assert_eq!(statusline_file_size(2 * 1024 * 1024), "2.0 MB");
    }

    #[test]
    fn edited_window_rows_commit_a_single_complete_frame() {
        let source = Buffer::new(None, "hello\nworld\n".to_string());
        let mut editor = rendering_test_editor(source);
        let mut buffer = RenderBuffer::new(60, 12, &Style::default());

        editor.render(&mut buffer).unwrap();
        let previous_generation = editor.render_generation;

        editor.render_edited_window_rows(&mut buffer).unwrap();

        assert_eq!(
            editor.render_generation,
            previous_generation.wrapping_add(1),
            "edited rows must commit a final frame instead of forcing a second viewport render"
        );
        assert_eq!(
            editor.previous_render_buffer.as_ref().unwrap().cells,
            buffer.cells,
            "the committed frame must match the visible render buffer"
        );
        assert_eq!(
            editor.last_rendered_cursor_position,
            editor.render_cursor_position(),
            "the committed frame must update the rendered cursor"
        );
    }

    #[test]
    fn reopening_a_shorter_commit_buffer_does_not_reuse_stale_layout_bytes() {
        let name = "[Git Commit].gitcommit";
        let mut editor = rendering_test_editor(Buffer::new(Some(name.to_string()), "x".repeat(61)));
        let mut buffer = RenderBuffer::new(60, 12, &Style::default());

        editor.render(&mut buffer).unwrap();
        editor.buffer_manager[0] = Buffer::new(Some(name.to_string()), "y".repeat(60));

        editor.render(&mut buffer).unwrap();

        assert!(rendered_rows(&buffer)
            .iter()
            .any(|row| row.contains("yyyy")));
    }

    #[test]
    fn edited_window_rows_preserve_completion_dialog() {
        let source = Buffer::new(None, "hello\nworld\n".to_string());
        let mut editor = rendering_test_editor(source);
        let mut completion = CompletionUI::new();
        let item = serde_json::from_value(serde_json::json!({ "label": "alpha" })).unwrap();
        completion.show(vec![item], 0, 0);
        editor.current_dialog = Some(Box::new(completion));
        let mut buffer = RenderBuffer::new(60, 12, &Style::default());

        editor.render(&mut buffer).unwrap();
        assert!(rendered_rows(&buffer)
            .iter()
            .any(|row| row.contains("alpha")));

        editor.render_edited_window_rows(&mut buffer).unwrap();

        assert!(
            rendered_rows(&buffer)
                .iter()
                .any(|row| row.contains("alpha")),
            "edited window rows must repaint the active completion dialog"
        );
    }

    #[test]
    fn edited_window_rows_preserve_all_visible_diagnostics() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("diagnostics.rs");
        let source = Buffer::new(
            Some(path.to_string_lossy().into_owned()),
            "first\nsecond\nthird\n".to_string(),
        );
        let mut editor = rendering_test_editor(source);
        let uri = editor.current_buffer().uri().unwrap().unwrap();
        let first = diagnostic("first visible diagnostic");
        let mut second = diagnostic("second visible diagnostic");
        second.range.start.line = 1;
        second.range.end.line = 1;
        editor.diagnostics.insert(uri, vec![first, second]);
        let mut buffer = RenderBuffer::new(60, 12, &Style::default());

        editor.render(&mut buffer).unwrap();
        let rows = rendered_rows(&buffer);
        assert!(rows
            .iter()
            .any(|row| row.contains("first visible diagnostic")));
        assert!(rows
            .iter()
            .any(|row| row.contains("second visible diagnostic")));

        editor.render_edited_window_rows(&mut buffer).unwrap();

        let rows = rendered_rows(&buffer);
        assert!(
            rows.iter()
                .any(|row| row.contains("first visible diagnostic")),
            "edited window rows must repaint the first visible diagnostic"
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("second visible diagnostic")),
            "edited window rows must repaint diagnostics beyond the cursor line"
        );
    }

    #[test]
    fn statusline_file_name_omits_dot_slash_prefix() {
        assert_eq!(statusline_file_name("./src/color.rs"), "src/color.rs");
    }

    #[test]
    fn statusline_file_name_preserves_other_paths() {
        assert_eq!(statusline_file_name("src/color.rs"), "src/color.rs");
        assert_eq!(
            statusline_file_name("/Users/fcoury/code/red/src/color.rs"),
            "/Users/fcoury/code/red/src/color.rs"
        );
        assert_eq!(statusline_file_name("[No Name]"), "[No Name]");
    }

    #[test]
    fn configurable_statusline_sections_render_in_the_requested_sides() {
        let mut config = Config::default();
        config.statusline.left = vec![StatuslineSection::Syntax, StatuslineSection::Mode];
        config.statusline.right = vec![StatuslineSection::Position, StatuslineSection::Filename];
        config.statusline.icons.style = PickerIconStyle::Ascii;
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let source = Buffer::new(
            Some("config.toml".to_string()),
            "theme = 'red'\n".to_string(),
        );
        let mut editor =
            Editor::with_size(lsp, 60, 12, config, Theme::default(), vec![source]).unwrap();
        let row = editor.test_statusline_row();

        let syntax = row.find("T toml").expect("syntax belongs on the left");
        let mode = row.find("NORMAL").expect("mode belongs after syntax");
        let filename = row
            .find("config.toml")
            .expect("filename belongs on the right");
        let position = row.rfind("1:1").expect("position belongs at the edge");
        assert!(syntax < mode);
        assert!(mode < filename);
        assert!(filename < position);
    }

    #[test]
    fn contextual_statusline_color_is_between_base_and_prominent_bands() {
        let base = Style {
            fg: Some(Color::Rgb {
                r: 220,
                g: 220,
                b: 220,
            }),
            bg: Some(Color::Rgb {
                r: 20,
                g: 30,
                b: 40,
            }),
            ..Style::default()
        };
        let prominent = Style {
            fg: Some(Color::Rgb { r: 0, g: 0, b: 0 }),
            bg: Some(Color::Rgb {
                r: 80,
                g: 180,
                b: 220,
            }),
            ..Style::default()
        };

        let contextual = statusline_context_style(&base, &prominent);

        assert_ne!(contextual.bg, base.bg);
        assert_ne!(contextual.bg, prominent.bg);
        assert!(contextual.bold);
    }

    #[test]
    fn statusline_colors_are_assigned_by_edge_position() {
        let mut theme = Theme::default();
        theme.statusline_style.inner_style = Style {
            fg: Some(Color::Rgb {
                r: 220,
                g: 220,
                b: 220,
            }),
            bg: Some(Color::Rgb {
                r: 20,
                g: 30,
                b: 40,
            }),
            ..Style::default()
        };
        theme.statusline_style.outer_style = Style {
            fg: Some(Color::Rgb { r: 0, g: 0, b: 0 }),
            bg: Some(Color::Rgb {
                r: 80,
                g: 180,
                b: 220,
            }),
            ..Style::default()
        };

        let edge = statusline_slot_style(&theme, 0);
        let second = statusline_slot_style(&theme, 1);
        let third = statusline_slot_style(&theme, 2);
        let fourth = statusline_slot_style(&theme, 3);

        assert_eq!(edge, theme.statusline_style.outer_style);
        assert_ne!(second.bg, edge.bg);
        assert_ne!(second.bg, third.bg);
        assert_eq!(third, theme.statusline_style.inner_style);
        assert_eq!(fourth, theme.statusline_style.inner_style);
    }

    #[test]
    fn statusline_diagnostics_hide_empty_severities_and_color_visible_markers() {
        let theme = statusline_test_theme();

        let (text, accents) =
            statusline_diagnostics_label(1, 2, PickerIconStyle::NerdFont, true, &theme)
                .expect("nonzero diagnostics should render");
        assert_eq!(text, "  1   2 ");
        assert_eq!(accents.len(), 2);
        assert_eq!(accents[0].text, "");
        assert_eq!(accents[1].text, "");

        let (errors_only, accents) =
            statusline_diagnostics_label(3, 0, PickerIconStyle::Unicode, true, &theme)
                .expect("errors should render without an empty warning badge");
        assert_eq!(errors_only, " ● 3 ");
        assert_eq!(accents.len(), 1);

        let (warnings_only, accents) =
            statusline_diagnostics_label(0, 4, PickerIconStyle::Ascii, false, &theme)
                .expect("warnings should render without an empty error badge");
        assert_eq!(warnings_only, " W4 ");
        assert!(accents.is_empty());

        let (icon_free, accents) =
            statusline_diagnostics_label(2, 1, PickerIconStyle::None, true, &theme)
                .expect("icon-free diagnostics should retain portable labels");
        assert_eq!(icon_free, " E2 W1 ");
        assert!(accents.is_empty());

        assert!(
            statusline_diagnostics_label(0, 0, PickerIconStyle::NerdFont, true, &theme).is_none()
        );
    }

    #[test]
    fn statusline_diagnostic_accents_respect_segment_truncation() {
        let theme = statusline_test_theme();
        let (text, accents) =
            statusline_diagnostics_label(1, 2, PickerIconStyle::NerdFont, true, &theme).unwrap();
        let segment = StatuslineSegment {
            text: text.clone(),
            style: statusline_slot_style(&theme, 1),
            accents,
        };
        let truncated = truncate_display_width(&text, 5);
        let mut buffer = RenderBuffer::new(5, 1, &Style::default());

        draw_statusline_segment(&mut buffer, 0, 0, &truncated, 5, &segment);

        assert!(buffer.cells.iter().any(|cell| cell.text == ""));
        assert!(buffer.cells.iter().all(|cell| cell.text != ""));
    }

    #[test]
    fn hidden_left_statusline_sections_reassign_visible_slot_styles() {
        const WIDTH: usize = 80;
        const HEIGHT: usize = 12;

        let mut config = Config::default();
        config.statusline.left = vec![
            StatuslineSection::Mode,
            StatuslineSection::Diagnostics,
            StatuslineSection::Filename,
        ];
        config.statusline.right.clear();
        let theme = statusline_test_theme();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let source = Buffer::new(Some("zeta.py".to_string()), "value = 1\n".to_string());
        let mut editor =
            Editor::with_size(lsp, WIDTH, HEIGHT, config, theme.clone(), vec![source]).unwrap();
        let uri = editor.current_buffer().uri().unwrap().unwrap();

        let clean = rendered_statusline(&mut editor, WIDTH, HEIGHT);
        assert_eq!(
            statusline_cell(&clean, HEIGHT, "z").style,
            statusline_slot_style(&theme, 1),
            "filename should occupy the second visible slot when diagnostics are absent"
        );

        editor.diagnostics.insert(
            uri,
            vec![
                diagnostic_with_severity("error", DiagnosticSeverity::Error),
                diagnostic_with_severity("warning", DiagnosticSeverity::Warning),
            ],
        );
        let diagnosed = rendered_statusline(&mut editor, WIDTH, HEIGHT);
        assert_eq!(
            statusline_cell(&diagnosed, HEIGHT, "z").style,
            statusline_slot_style(&theme, 2),
            "filename should occupy the third visible slot when diagnostics are present"
        );

        let diagnostic_style = statusline_slot_style(&theme, 1);
        let diagnostic_background = diagnostic_style.bg.unwrap();
        let expected_error = ensure_minimum_contrast(
            theme.colors["editorError.foreground"],
            diagnostic_background,
            3.0,
        );
        let expected_warning = ensure_minimum_contrast(
            theme.colors["editorWarning.foreground"],
            diagnostic_background,
            3.0,
        );
        assert_eq!(
            statusline_cell(&diagnosed, HEIGHT, "").style.fg,
            Some(expected_error)
        );
        assert_eq!(
            statusline_cell(&diagnosed, HEIGHT, "").style.fg,
            Some(expected_warning)
        );
    }

    #[test]
    fn hidden_right_statusline_sections_reassign_visible_slot_styles() {
        const WIDTH: usize = 80;
        const HEIGHT: usize = 12;

        let mut config = Config::default();
        config.statusline.left.clear();
        config.statusline.right = vec![
            StatuslineSection::Position,
            StatuslineSection::Diagnostics,
            StatuslineSection::Filename,
        ];
        let theme = statusline_test_theme();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let source = Buffer::new(Some("zeta.py".to_string()), "value = 1\n".to_string());
        let mut editor =
            Editor::with_size(lsp, WIDTH, HEIGHT, config, theme.clone(), vec![source]).unwrap();
        let uri = editor.current_buffer().uri().unwrap().unwrap();

        let clean = rendered_statusline(&mut editor, WIDTH, HEIGHT);
        assert_eq!(
            statusline_cell(&clean, HEIGHT, "z").style,
            statusline_slot_style(&theme, 1)
        );

        editor.diagnostics.insert(
            uri,
            vec![diagnostic_with_severity(
                "warning",
                DiagnosticSeverity::Warning,
            )],
        );
        let diagnosed = rendered_statusline(&mut editor, WIDTH, HEIGHT);
        assert_eq!(
            statusline_cell(&diagnosed, HEIGHT, "z").style,
            statusline_slot_style(&theme, 2)
        );
    }

    #[test]
    fn git_branch_reads_regular_and_worktree_head_files() {
        let repository = tempfile::tempdir().unwrap();
        fs::create_dir(repository.path().join(".git")).unwrap();
        fs::write(
            repository.path().join(".git/HEAD"),
            "ref: refs/heads/feature/statusline\n",
        )
        .unwrap();
        let nested = repository.path().join("src/editor");
        fs::create_dir_all(&nested).unwrap();

        assert_eq!(
            git_branch_from_head(&nested).as_deref(),
            Some("feature/statusline")
        );

        let worktree = tempfile::tempdir().unwrap();
        let git_dir = tempfile::tempdir().unwrap();
        fs::write(
            worktree.path().join(".git"),
            format!("gitdir: {}\n", git_dir.path().display()),
        )
        .unwrap();
        fs::write(git_dir.path().join("HEAD"), "0123456789abcdef\n").unwrap();

        assert_eq!(
            git_branch_from_head(worktree.path()).as_deref(),
            Some("01234567")
        );
    }

    #[test]
    fn long_git_branch_keeps_hierarchy_and_identity_compact() {
        assert_eq!(
            compact_git_branch("feature/frontend/configurable-statusline-with-icons"),
            "f/f/configurable-statusl…"
        );
    }

    #[test]
    fn statusline_preserves_the_edge_position_on_a_narrow_terminal() {
        let mut config = Config::default();
        config.statusline.left = vec![StatuslineSection::Mode, StatuslineSection::Filename];
        config.statusline.right = vec![StatuslineSection::Position, StatuslineSection::Syntax];
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let source = Buffer::new(
            Some("config.toml".to_string()),
            "value = true\n".to_string(),
        );
        let mut editor =
            Editor::with_size(lsp, 8, 5, config, Theme::default(), vec![source]).unwrap();

        let row = editor.test_statusline_row();

        assert_eq!(display_width(&row), 8);
        assert!(row.ends_with(" 1:1 "), "{row:?}");
    }

    #[test]
    fn eol_decoration_renders_only_on_final_segment() {
        let decoration = decoration(DecorationAnchor::Eol, " => PathBuf");

        assert_eq!(
            decoration_local_x(&decoration, &segment(0, 8, true), 16, false, 20),
            None
        );
        assert_eq!(
            decoration_local_x(&decoration, &segment(8, 16, false), 16, false, 20),
            Some(8)
        );
    }

    #[test]
    fn right_aligned_decoration_uses_display_width() {
        let decoration = decoration(DecorationAnchor::RightAlign, "=> 👋");

        assert_eq!(
            decoration_local_x(&decoration, &segment(0, 8, true), 8, false, 12),
            Some(7)
        );
    }

    #[test]
    fn search_highlight_skips_common_matches_on_an_oversized_wrapped_line() {
        let config = Config::default();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let contents = format!("{}\n", "x".repeat(MAX_HIGHLIGHT_SLICE_BYTES + 1));
        let source = Buffer::new(Some("large.txt".to_string()), contents);
        let mut editor =
            Editor::with_size(lsp, 80, 24, config, Theme::default(), vec![source]).unwrap();
        editor.search_term = "x".to_string();
        let window = editor.window_manager.active_window().unwrap().clone();
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());

        editor
            .render_search_highlights_in_window(&mut buffer, &window)
            .unwrap();

        assert!(editor.search_match_cache.is_none());
    }

    #[test]
    fn per_window_render_leaves_separator_to_split_renderer() {
        let config = Config {
            window_borders_ascii: true,
            ..Config::default()
        };
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let source = Buffer::new(None, "content\n".to_string());
        let mut editor =
            Editor::with_size(lsp, 40, 10, config, Theme::default(), vec![source]).unwrap();
        editor.window_manager.split_vertical(0).unwrap();
        let left = editor.window_manager.window_at_index(0).unwrap();
        let separator_x = left.position.x + left.size.0;
        let separator_height = left.size.1;
        let mut buffer = RenderBuffer::new(40, 10, &Style::default());

        editor.render_window(&mut buffer, 0).unwrap();

        assert!(
            (0..separator_height).all(|y| buffer.cells[y * buffer.width + separator_x].c == ' ')
        );

        editor.render_all_window_separators(&mut buffer).unwrap();

        assert!(
            (0..separator_height).all(|y| buffer.cells[y * buffer.width + separator_x].c == '|')
        );
    }

    #[test]
    fn nested_split_separators_preserve_unicode_junctions() {
        let config = Config::default();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let source = Buffer::new(None, "content\n".to_string());
        let mut editor =
            Editor::with_size(lsp, 40, 10, config, Theme::default(), vec![source]).unwrap();
        editor.window_manager.split_vertical(0).unwrap();
        editor.window_manager.set_active(0);
        editor.window_manager.split_horizontal(0).unwrap();
        editor.window_manager.set_active(0);
        editor.window_manager.split_vertical(0).unwrap();
        let windows = editor.window_manager.windows();
        let top_left = windows[0];
        let top_right = windows[1];
        let bottom_left = windows[2];
        let inner_x = top_left.position.x + top_left.size.0;
        let outer_x = bottom_left.position.x + bottom_left.size.0;
        let horizontal_y = top_right.position.y + top_right.size.1;
        let mut buffer = RenderBuffer::new(40, 10, &Style::default());

        editor.render_all_window_separators(&mut buffer).unwrap();

        let cell = |x: usize, y: usize| buffer.cells[y * buffer.width + x].c;
        assert_eq!(cell(inner_x, 1), '│');
        assert_eq!(cell(outer_x, 1), '│');
        assert_eq!(cell(1, horizontal_y), '─');
        assert_eq!(cell(inner_x, horizontal_y), '┴');
        assert_eq!(cell(outer_x, horizontal_y), '┤');
    }

    #[test]
    fn active_split_dividers_use_the_theme_accent_in_ascii_and_unicode() {
        let accent = Color::Rgb {
            r: 203,
            g: 166,
            b: 247,
        };

        for use_ascii in [false, true] {
            for vertical in [false, true] {
                let config = Config {
                    window_borders_ascii: use_ascii,
                    ..Config::default()
                };
                let lsp = Box::new(LspManager::new(config.lsp.clone()));
                let source = Buffer::new(None, "content\n".to_string());
                let mut theme = Theme::default();
                theme.colors.insert("sash.hoverBorder".to_string(), accent);
                let mut editor =
                    Editor::with_size(lsp, 40, 10, config, theme, vec![source]).unwrap();
                let (x, y, expected) = if vertical {
                    editor.window_manager.split_vertical(0).unwrap();
                    (19, 2, if use_ascii { '|' } else { '│' })
                } else {
                    editor.window_manager.split_horizontal(0).unwrap();
                    (5, 3, if use_ascii { '-' } else { '─' })
                };
                let divider = editor
                    .window_manager
                    .divider_at_position(x, y)
                    .expect("the visible split must be draggable");
                editor.divider_drag = Some(super::super::DividerDrag {
                    target: super::super::DividerResizeTarget::Window {
                        divider: divider.clone(),
                    },
                    last_position: Point::new(x, y),
                });
                let mut buffer = RenderBuffer::new(40, 10, &Style::default());

                editor.render_all_window_separators(&mut buffer).unwrap();

                let active = &buffer.cells[y * buffer.width + x];
                assert_eq!(active.c, expected);
                assert_eq!(active.style.fg, Some(accent));
                assert!(active.style.bold);

                editor.divider_drag = None;
                editor.render_all_window_separators(&mut buffer).unwrap();

                let released = &buffer.cells[y * buffer.width + x];
                assert_eq!(released.c, expected);
                assert_eq!(
                    released.style.fg,
                    Some(Color::Rgb {
                        r: 100,
                        g: 100,
                        b: 100,
                    }),
                );
                assert!(!released.style.bold);

                let target = super::super::DividerResizeTarget::Window { divider };
                editor.pane_resize_mode = Some(super::super::PaneResizeMode {
                    vertical: vertical.then_some(target.clone()),
                    horizontal: (!vertical).then_some(target),
                });
                editor.render_all_window_separators(&mut buffer).unwrap();

                let keyboard_active = &buffer.cells[y * buffer.width + x];
                assert_eq!(keyboard_active.c, expected);
                assert_eq!(keyboard_active.style.fg, Some(accent));
                assert!(keyboard_active.style.bold);
            }
        }
    }

    #[test]
    fn active_nested_divider_preserves_junctions_and_unrelated_split_styles() {
        let accent = Color::Rgb {
            r: 203,
            g: 166,
            b: 247,
        };
        let config = Config::default();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let source = Buffer::new(None, "content\n".to_string());
        let mut theme = Theme::default();
        theme.colors.insert("sash.hoverBorder".to_string(), accent);
        let mut editor = Editor::with_size(lsp, 40, 10, config, theme, vec![source]).unwrap();
        editor.window_manager.split_vertical(0).unwrap();
        editor.window_manager.set_active(0);
        editor.window_manager.split_horizontal(0).unwrap();
        editor.window_manager.set_active(0);
        editor.window_manager.split_vertical(0).unwrap();
        let windows = editor.window_manager.windows();
        let top_left = windows[0];
        let top_right = windows[1];
        let bottom_left = windows[2];
        let inner_x = top_left.position.x + top_left.size.0;
        let outer_x = bottom_left.position.x + bottom_left.size.0;
        let horizontal_y = top_right.position.y + top_right.size.1;
        let divider = editor
            .window_manager
            .divider_at_position(/*x*/ 1, horizontal_y)
            .expect("the nested horizontal separator must be draggable");
        editor.divider_drag = Some(super::super::DividerDrag {
            target: super::super::DividerResizeTarget::Window { divider },
            last_position: Point::new(/*x*/ 1, horizontal_y),
        });
        let mut buffer = RenderBuffer::new(40, 10, &Style::default());

        editor.render_all_window_separators(&mut buffer).unwrap();

        let cell = |x: usize, y: usize| &buffer.cells[y * buffer.width + x];
        assert_eq!(cell(1, horizontal_y).c, '─');
        assert_eq!(cell(1, horizontal_y).style.fg, Some(accent));
        assert_eq!(cell(inner_x, horizontal_y).c, '┴');
        assert_eq!(cell(inner_x, horizontal_y).style.fg, Some(accent));
        assert_eq!(cell(outer_x, horizontal_y).c, '┤');
        assert_ne!(cell(outer_x, horizontal_y).style.fg, Some(accent));
        assert_eq!(cell(inner_x, 1).c, '│');
        assert_ne!(cell(inner_x, 1).style.fg, Some(accent));
    }

    #[test]
    fn theme_change_repaints_cells_with_unchanged_unresolved_styles() {
        let config = Config::default();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let source = Buffer::new(None, String::new());
        let mut editor =
            Editor::with_size(lsp, 4, 2, config, Theme::default(), vec![source]).unwrap();
        let buffer = RenderBuffer::new(4, 2, &Style::default());
        editor.previous_render_buffer = Some(buffer.clone());
        editor.force_full_redraw = true;

        let changes = editor.render_buffer_changes(&buffer);

        assert_eq!(changes.len(), buffer.width * buffer.height);
        editor.commit_render_buffer_changes(&changes);
        assert!(!editor.force_full_redraw);
    }

    #[test]
    fn queue_cell_attributes_sets_and_clears_tracked_attributes() {
        let mut output = Vec::new();

        queue_cell_attributes(
            &mut output,
            &Style {
                bold: true,
                italic: true,
                ..Style::default()
            },
        )
        .unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(
            output.contains("\x1b[1m"),
            "bold style should emit bold attribute"
        );
        assert!(
            output.contains("\x1b[3m"),
            "italic style should emit italic attribute"
        );

        let mut output = Vec::new();
        queue_cell_attributes(&mut output, &Style::default()).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(
            output.contains("\x1b[22m"),
            "plain style should clear bold/dim intensity"
        );
        assert!(
            output.contains("\x1b[23m"),
            "plain style should clear italic attribute"
        );
    }
}
