//! Inline Markdown concealment for wrapping and cursor columns.
//!
//! Table rows already project away pipes and inline syntax. Paragraphs still
//! wrapped on the raw line, so a hidden URL consumed screen columns the user
//! could not see. This module records the syntax that should occupy no cells —
//! emphasis and strike markers, link brackets and destinations, image markup,
//! code-span backticks, and GitHub alert markers — and display layout skips
//! those bytes when it measures a row. A `> [!NOTE]` line conceals into a
//! colored label, and the `>` gutter of that quote takes the same color.
//! Leaf draws the same five alert kinds; Red keeps each source line on its
//! own row so the cursor still edits the raw marker.
//!
//! Balanced inline HTML tags on the same line are concealed the same way.
//! The tag names follow leaf's classifier (`b`/`strong`, `i`/`em`, `s`/`del`,
//! `u`/`ins`, `mark`, `code`, `br`), plus `strike`, `kbd`, `sub`, and `sup`.
//! HTML blocks, code spans, and fences stay raw.
//!
//! `==highlight==` follows leaf's marker rules when extended marks are on:
//! the pair must be non-empty and must not start or end with a space. Red
//! hides the markers only. It does not pad the text the way leaf does.
//!
//! The buffer stays the raw document. A cursor that steps into a concealed span,
//! or an insert, visual, search, or operator-pending edit, reveals that line so
//! the cell under the cursor is the byte that will be changed.

use std::{ops::Range, sync::Arc};

use pulldown_cmark::{BlockQuoteKind, Event, Options, Parser, Tag, TagEnd};
use unicode_segmentation::UnicodeSegmentation as _;

use super::display_layout::{Concealment, LineConcealments, LineSegment};
use super::markdown_tables::MAX_TABLE_DOCUMENT_BYTES;
use crate::color::Color;
use crate::theme::{Style, Theme};
use crate::unicode_utils::{display_width_with_tabs, trim_line_ending};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Emphasis,
    Strong,
    Strike,
    Link,
    Image,
}

struct Frame {
    kind: Kind,
    start: usize,
    end: usize,
    visible: Vec<Range<usize>>,
}

/// One GitHub alert kind. The names match leaf's callouts and GitHub's syntax.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AlertKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl AlertKind {
    fn from_pulldown(kind: BlockQuoteKind) -> Self {
        match kind {
            BlockQuoteKind::Note => Self::Note,
            BlockQuoteKind::Tip => Self::Tip,
            BlockQuoteKind::Important => Self::Important,
            BlockQuoteKind::Warning => Self::Warning,
            BlockQuoteKind::Caution => Self::Caution,
        }
    }

    fn needle(self) -> &'static str {
        match self {
            Self::Note => "[!note]",
            Self::Tip => "[!tip]",
            Self::Important => "[!important]",
            Self::Warning => "[!warning]",
            Self::Caution => "[!caution]",
        }
    }

    /// Label drawn in place of `> [!KIND]`. The symbols are single-cell stand-ins
    /// for leaf's `[i]` / `[*]` / `[!]` / `[x]` icons.
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Note => "ℹ Note",
            Self::Tip => "✦ Tip",
            Self::Important => "◆ Important",
            Self::Warning => "⚠ Warning",
            Self::Caution => "× Caution",
        }
    }

    fn color_key(self) -> &'static str {
        match self {
            Self::Note => "editorInfo.foreground",
            Self::Tip => "terminal.ansiGreen",
            Self::Important => "terminal.ansiMagenta",
            Self::Warning => "editorWarning.foreground",
            Self::Caution => "editorError.foreground",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct AlertGutter {
    byte: usize,
    kind: AlertKind,
}

#[derive(Clone, Copy, Debug)]
struct AlertLabel {
    kind: AlertKind,
    /// Byte of the concealed `>` that the label replaces.
    byte: usize,
}

#[derive(Clone, Debug, Default)]
struct AlertLine {
    gutters: Vec<AlertGutter>,
    label: Option<AlertLabel>,
}

/// Per-line alert gutters and marker labels. Indexed like [`LineConcealments`].
#[derive(Clone, Debug, Default)]
pub(super) struct AlertMap {
    lines: Vec<AlertLine>,
}

impl AlertMap {
    fn line(&self, index: usize) -> Option<&AlertLine> {
        self.lines
            .get(index)
            .filter(|line| line.label.is_some() || !line.gutters.is_empty())
    }
}

struct ConcealedSource {
    lines: LineConcealments,
    alerts: AlertMap,
    html: HtmlMap,
}

/// Concealed byte ranges for every line in `source`.
#[cfg(test)]
fn conceal_markdown(source: &str) -> LineConcealments {
    parse_concealment(source, true).lines
}

fn parse_concealment(source: &str, extended_marks: bool) -> ConcealedSource {
    let mut offsets = vec![0];
    offsets.extend(source.match_indices('\n').map(|(index, _)| index + 1));
    let mut hidden = vec![Vec::new(); offsets.len()];
    let mut stack = Vec::new();
    let mut alerts = Vec::new();
    let mut protected = Vec::new();
    let mut code_blocks = 0u32;
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_GFM;
    for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
        let opening_code = matches!(event, Event::Start(Tag::CodeBlock(_)));
        if opening_code {
            code_blocks += 1;
        }
        let inside_code = code_blocks > 0;
        if inside_code || matches!(event, Event::Html(_) | Event::Code(_)) {
            protected.push(range.clone());
        } else if let Event::InlineHtml(_) = &event {
            if source
                .get(range.clone())
                .is_some_and(|text| text.trim_start().starts_with("<!--"))
            {
                protected.push(range.clone());
            }
        }
        if matches!(event, Event::End(TagEnd::CodeBlock)) {
            code_blocks = code_blocks.saturating_sub(1);
        }
        if inside_code {
            continue;
        }
        match event {
            Event::Start(Tag::BlockQuote(Some(kind))) => {
                alerts.push((range, AlertKind::from_pulldown(kind)));
            }
            Event::Start(tag) => {
                if let Some(kind) = kind_of_tag(&tag) {
                    stack.push(Frame {
                        kind,
                        start: range.start,
                        end: range.end,
                        visible: Vec::new(),
                    });
                }
            }
            Event::End(tag) => {
                let Some(kind) = kind_of_end(&tag) else {
                    continue;
                };
                if let Some(mut frame) = stack.pop_if(|frame| frame.kind == kind) {
                    frame.start = frame.start.min(range.start);
                    frame.end = frame.end.max(range.end);
                    let visible = frame.visible;
                    let gaps = complement(frame.start..frame.end, &visible);
                    if let Some(parent) = stack.last_mut() {
                        parent.visible.extend(visible);
                    }
                    for gap in gaps {
                        push_gap(source, &offsets, &mut hidden, gap);
                    }
                }
            }
            Event::Text(_) | Event::InlineHtml(_) | Event::SoftBreak | Event::HardBreak => {
                if let Some(frame) = stack.last_mut() {
                    frame.visible.push(range);
                }
            }
            Event::Code(_) => conceal_code_span(source, range, &mut stack, &offsets, &mut hidden),
            _ => {}
        }
    }
    let alert_lines = record_alerts(source, &offsets, &alerts, &mut hidden);
    let mut html = record_html(source, &offsets, &protected, &mut hidden);
    if extended_marks {
        record_extended_marks(source, &offsets, &protected, &mut hidden, &mut html);
    }
    ConcealedSource {
        lines: LineConcealments::new(hidden.into_iter().map(Concealment::new).collect()),
        alerts: AlertMap { lines: alert_lines },
        html,
    }
}

fn record_alerts(
    source: &str,
    offsets: &[usize],
    alerts: &[(Range<usize>, AlertKind)],
    hidden: &mut [Vec<Range<usize>>],
) -> Vec<AlertLine> {
    let mut lines = vec![AlertLine::default(); offsets.len()];
    for (range, kind) in alerts {
        let Some(first) = line_index_at(offsets, range.start) else {
            continue;
        };
        let last = line_index_at(offsets, range.end.saturating_sub(1)).unwrap_or(first);
        let content = line_content_range(source, offsets, first);
        let text = source.get(content.clone()).unwrap_or("");
        let Some(marker) = alert_marker(text, *kind) else {
            continue;
        };
        push_gap(
            source,
            offsets,
            hidden,
            (content.start + marker.gt)..content.end,
        );
        if let Some(line) = lines.get_mut(first) {
            line.label = Some(AlertLabel {
                kind: *kind,
                byte: marker.gt,
            });
        }
        for index in first..=last {
            let content = line_content_range(source, offsets, index);
            let text = source.get(content.clone()).unwrap_or("");
            let Some(byte) = quote_markers(text).get(marker.depth - 1).copied() else {
                continue;
            };
            if let Some(line) = lines.get_mut(index) {
                line.gutters.push(AlertGutter { byte, kind: *kind });
            }
        }
    }
    lines
}

struct AlertMarker {
    /// Byte of this alert's `>` on the marker line.
    gt: usize,
    /// 1-based index of that `>` among the line's quote markers.
    depth: usize,
}

/// Exact GitHub alert marker: case-insensitive kind, nothing else on the line.
fn alert_marker(line: &str, kind: AlertKind) -> Option<AlertMarker> {
    let needle = kind.needle();
    let lower = line.to_ascii_lowercase();
    let tag = lower.find(needle)?;
    if !line[tag + needle.len()..].trim().is_empty() {
        return None;
    }
    let prefix = line[..tag].trim_end();
    let gt = prefix.rfind('>')?;
    if line[gt + 1..tag].chars().any(|character| character != ' ') {
        return None;
    }
    let markers = quote_markers(line);
    let depth = markers.iter().position(|byte| *byte == gt)? + 1;
    Some(AlertMarker { gt, depth })
}

/// Byte offsets of blockquote `>` markers at the start of a line.
fn quote_markers(line: &str) -> Vec<usize> {
    let bytes = line.as_bytes();
    let mut index = 0;
    let mut spaces = 0;
    while index < bytes.len() && bytes[index] == b' ' && spaces < 3 {
        index += 1;
        spaces += 1;
    }
    let mut markers = Vec::new();
    loop {
        if index >= bytes.len() || bytes[index] != b'>' {
            break;
        }
        markers.push(index);
        index += 1;
        if index < bytes.len() && bytes[index] == b' ' {
            index += 1;
        }
    }
    markers
}

fn line_index_at(offsets: &[usize], byte: usize) -> Option<usize> {
    if offsets.is_empty() {
        return None;
    }
    Some(
        offsets
            .partition_point(|offset| *offset <= byte)
            .saturating_sub(1),
    )
}

fn kind_of_tag(tag: &Tag<'_>) -> Option<Kind> {
    match tag {
        Tag::Emphasis => Some(Kind::Emphasis),
        Tag::Strong => Some(Kind::Strong),
        Tag::Strikethrough => Some(Kind::Strike),
        Tag::Link { .. } => Some(Kind::Link),
        Tag::Image { .. } => Some(Kind::Image),
        _ => None,
    }
}

fn kind_of_end(tag: &TagEnd) -> Option<Kind> {
    match tag {
        TagEnd::Emphasis => Some(Kind::Emphasis),
        TagEnd::Strong => Some(Kind::Strong),
        TagEnd::Strikethrough => Some(Kind::Strike),
        TagEnd::Link => Some(Kind::Link),
        TagEnd::Image => Some(Kind::Image),
        _ => None,
    }
}

fn conceal_code_span(
    source: &str,
    range: Range<usize>,
    stack: &mut [Frame],
    offsets: &[usize],
    hidden: &mut [Vec<Range<usize>>],
) {
    let Some(text) = source.get(range.clone()) else {
        return;
    };
    let open = text.bytes().take_while(|byte| *byte == b'`').count();
    let close = text.bytes().rev().take_while(|byte| *byte == b'`').count();
    if open == 0 || close == 0 || open + close > text.len() {
        if let Some(frame) = stack.last_mut() {
            frame.visible.push(range);
        }
        return;
    }
    push_gap(source, offsets, hidden, range.start..range.start + open);
    push_gap(source, offsets, hidden, range.end - close..range.end);
    if let Some(frame) = stack.last_mut() {
        frame.visible.push(range.start + open..range.end - close);
    }
}

fn complement(outer: Range<usize>, visible: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut visible = visible
        .iter()
        .filter(|range| {
            range.start < range.end && range.end > outer.start && range.start < outer.end
        })
        .cloned()
        .collect::<Vec<_>>();
    visible.sort_by_key(|range| (range.start, range.end));
    let mut cursor = outer.start;
    let mut gaps = Vec::new();
    for range in visible {
        let start = range.start.clamp(outer.start, outer.end);
        let end = range.end.clamp(outer.start, outer.end);
        if cursor < start {
            gaps.push(cursor..start);
        }
        cursor = cursor.max(end);
    }
    if cursor < outer.end {
        gaps.push(cursor..outer.end);
    }
    gaps
}

fn push_gap(source: &str, offsets: &[usize], hidden: &mut [Vec<Range<usize>>], gap: Range<usize>) {
    if gap.start >= gap.end || offsets.is_empty() {
        return;
    }
    let first = offsets
        .partition_point(|offset| *offset <= gap.start)
        .saturating_sub(1);
    let last = offsets
        .partition_point(|offset| *offset < gap.end)
        .saturating_sub(1);
    let Some(lines) = hidden.get_mut(first..=last) else {
        return;
    };
    for (offset, hidden_line) in lines.iter_mut().enumerate() {
        let content = line_content_range(source, offsets, first + offset);
        let start = gap.start.max(content.start);
        let end = gap.end.min(content.end);
        if start < end {
            hidden_line.push(start - content.start..end - content.start);
        }
    }
}

fn line_content_range(source: &str, offsets: &[usize], line: usize) -> Range<usize> {
    let start = offsets.get(line).copied().unwrap_or(source.len());
    let mut end = offsets.get(line + 1).copied().unwrap_or(source.len());
    if start >= source.len() {
        return source.len()..source.len();
    }
    end = end.min(source.len());
    if end > start && source.as_bytes()[end - 1] == b'\n' {
        end -= 1;
    }
    if end > start && source.as_bytes()[end - 1] == b'\r' {
        end -= 1;
    }
    start..end
}

/// Inline HTML effects. Tag names follow leaf's classifier, extended with
/// `strike`, `kbd`, `sub`, and `sup`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HtmlKind {
    Bold,
    Italic,
    Underline,
    Strike,
    Mark,
    Code,
    Kbd,
    Sub,
    Sup,
    Br,
}

#[derive(Clone, Copy, Debug)]
struct HtmlSpan {
    start: usize,
    end: usize,
    kind: HtmlKind,
}

#[derive(Clone, Debug, Default)]
struct HtmlLine {
    spans: Vec<HtmlSpan>,
}

#[derive(Clone, Debug, Default)]
struct HtmlMap {
    lines: Vec<HtmlLine>,
}

impl HtmlMap {
    fn line(&self, index: usize) -> Option<&HtmlLine> {
        self.lines.get(index).filter(|line| !line.spans.is_empty())
    }
}

/// Combining long stroke. `Style` has no strikethrough bit, so `<s>` paints
/// this over the visible grapheme and the cell stays one column wide.
const STRIKE_OVERLAY: char = '\u{0336}';

fn record_html(
    source: &str,
    offsets: &[usize],
    protected: &[Range<usize>],
    hidden: &mut [Vec<Range<usize>>],
) -> HtmlMap {
    let mut lines = Vec::with_capacity(offsets.len());
    for index in 0..offsets.len() {
        let content = line_content_range(source, offsets, index);
        let text = source.get(content.clone()).unwrap_or("");
        let local = protected_on_line(protected, content.clone());
        let (gaps, spans) = scan_html_line(text, &local);
        for gap in gaps {
            push_gap(
                source,
                offsets,
                hidden,
                content.start + gap.start..content.start + gap.end,
            );
        }
        lines.push(HtmlLine { spans });
    }
    HtmlMap { lines }
}

/// `==highlight==` pairs. The acceptance rules are leaf's (`markers.rs`):
/// non-empty, no newline, no leading or trailing space. An open with no
/// close stops the scan. Red does not insert the padding spaces leaf draws.
fn record_extended_marks(
    source: &str,
    offsets: &[usize],
    protected: &[Range<usize>],
    hidden: &mut [Vec<Range<usize>>],
    html: &mut HtmlMap,
) {
    for index in 0..offsets.len() {
        let content = line_content_range(source, offsets, index);
        let text = source.get(content.clone()).unwrap_or("");
        let mut skip = protected_on_line(protected, content.clone());
        if let Some(hidden_line) = hidden.get(index) {
            skip.extend(hidden_line.iter().cloned());
        }
        let (gaps, spans) = scan_mark_line(text, &skip);
        for gap in gaps {
            push_gap(
                source,
                offsets,
                hidden,
                content.start + gap.start..content.start + gap.end,
            );
        }
        if let Some(line) = html.lines.get_mut(index) {
            line.spans.extend(spans);
        }
    }
}

struct MarkHit {
    open_pos: usize,
    close_rel: usize,
}

fn valid_mark_content(content: &str) -> bool {
    !content.is_empty()
        && !content.contains('\n')
        && !content.starts_with(' ')
        && !content.ends_with(' ')
}

fn find_valid_mark(text: &str, skip: &[Range<usize>], base: usize) -> Option<MarkHit> {
    for (open_pos, _) in text.match_indices("==") {
        let open = base + open_pos;
        if ranges_overlap(skip, open, open + 2) {
            continue;
        }
        let after = open_pos + 2;
        let close_rel = text[after..].find("==")?;
        if !valid_mark_content(&text[after..after + close_rel]) {
            continue;
        }
        let close = base + after + close_rel;
        if ranges_overlap(skip, open, close + 2) {
            continue;
        }
        return Some(MarkHit {
            open_pos,
            close_rel,
        });
    }
    None
}

fn scan_mark_line(text: &str, skip: &[Range<usize>]) -> (Vec<Range<usize>>, Vec<HtmlSpan>) {
    let mut gaps = Vec::new();
    let mut spans = Vec::new();
    let mut rest = 0;
    while rest < text.len() {
        let Some(hit) = find_valid_mark(&text[rest..], skip, rest) else {
            break;
        };
        let open = rest + hit.open_pos;
        let content_start = open + 2;
        let content_end = content_start + hit.close_rel;
        gaps.push(open..content_start);
        gaps.push(content_end..content_end + 2);
        spans.push(HtmlSpan {
            start: content_start,
            end: content_end,
            kind: HtmlKind::Mark,
        });
        rest = content_end + 2;
    }
    (gaps, spans)
}

fn protected_on_line(protected: &[Range<usize>], content: Range<usize>) -> Vec<Range<usize>> {
    let mut local = Vec::new();
    for range in protected {
        let start = range.start.max(content.start);
        let end = range.end.min(content.end);
        if start < end {
            local.push(start - content.start..end - content.start);
        }
    }
    local
}

fn protected_end_at(protected: &[Range<usize>], index: usize) -> Option<usize> {
    protected
        .iter()
        .filter(|range| range.contains(&index))
        .map(|range| range.end)
        .max()
}

fn ranges_overlap(ranges: &[Range<usize>], start: usize, end: usize) -> bool {
    ranges
        .iter()
        .any(|range| start < range.end && range.start < end)
}

struct OpenHtml {
    kind: HtmlKind,
    tag_start: usize,
    content_start: usize,
}

struct ParsedTag {
    start: usize,
    end: usize,
    kind: HtmlKind,
    open: bool,
}

fn scan_html_line(text: &str, protected: &[Range<usize>]) -> (Vec<Range<usize>>, Vec<HtmlSpan>) {
    let mut hidden = Vec::new();
    let mut spans = Vec::new();
    let mut stack = Vec::new();
    let mut index = 0;
    while index < text.len() {
        if let Some(end) = protected_end_at(protected, index) {
            index = end;
            continue;
        }
        if text[index..].starts_with("<!--") {
            index = text[index + 4..]
                .find("-->")
                .map_or(text.len(), |offset| index + 4 + offset + 3);
            continue;
        }
        if !text[index..].starts_with('<') {
            index += text[index..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        let Some(tag) = parse_html_tag(text, index) else {
            index += 1;
            continue;
        };
        if ranges_overlap(protected, tag.start, tag.end) {
            index += 1;
            continue;
        }
        index = consume_html_tag(tag, &mut stack, &mut hidden, &mut spans);
    }
    (hidden, spans)
}

fn consume_html_tag(
    tag: ParsedTag,
    stack: &mut Vec<OpenHtml>,
    hidden: &mut Vec<Range<usize>>,
    spans: &mut Vec<HtmlSpan>,
) -> usize {
    let literal = stack
        .last()
        .is_some_and(|open| matches!(open.kind, HtmlKind::Code | HtmlKind::Kbd));
    if literal {
        if !tag.open && stack.last().is_some_and(|open| open.kind == tag.kind) {
            close_html_tag(&tag, stack, hidden, spans);
        }
        return tag.end;
    }
    if tag.kind == HtmlKind::Br {
        hidden.push(tag.start..tag.end);
        return tag.end;
    }
    if tag.open {
        stack.push(OpenHtml {
            kind: tag.kind,
            tag_start: tag.start,
            content_start: tag.end,
        });
        return tag.end;
    }
    if stack.last().is_some_and(|open| open.kind == tag.kind) {
        close_html_tag(&tag, stack, hidden, spans);
    }
    tag.end
}

fn close_html_tag(
    tag: &ParsedTag,
    stack: &mut Vec<OpenHtml>,
    hidden: &mut Vec<Range<usize>>,
    spans: &mut Vec<HtmlSpan>,
) {
    let Some(open) = stack.pop() else {
        return;
    };
    hidden.push(open.tag_start..open.content_start);
    hidden.push(tag.start..tag.end);
    if open.content_start < tag.start {
        spans.push(HtmlSpan {
            start: open.content_start,
            end: tag.start,
            kind: open.kind,
        });
    }
}

/// Leaf's `normalize_html_tag`, plus quoted `>` inside attributes.
fn parse_html_tag(text: &str, start: usize) -> Option<ParsedTag> {
    let rest = text.get(start..)?;
    if !rest.starts_with('<') {
        return None;
    }
    let mut quote = None;
    let mut end_rel = None;
    for (offset, character) in rest.char_indices().skip(1) {
        match (quote, character) {
            (None, '"' | '\'') => quote = Some(character),
            (Some(open), character) if character == open => quote = None,
            (None, '>') => {
                end_rel = Some(offset);
                break;
            }
            _ => {}
        }
    }
    let end_rel = end_rel?;
    let inner = &rest[1..end_rel];
    let (body, open) = match inner.strip_prefix('/') {
        Some(body) => (body, false),
        None => (inner, true),
    };
    let body = body.trim_end();
    let body = body.trim_end_matches('/').trim_end();
    if body.is_empty() || body.starts_with(|character: char| character.is_ascii_whitespace()) {
        return None;
    }
    let name_len = body
        .find(|character: char| !character.is_ascii_alphanumeric())
        .unwrap_or(body.len());
    let name = &body[..name_len];
    if name.is_empty() {
        return None;
    }
    if name_len < body.len()
        && !body[name_len..].starts_with(|character: char| character.is_ascii_whitespace())
    {
        return None;
    }
    Some(ParsedTag {
        start,
        end: start + end_rel + 1,
        kind: classify_html_name(name)?,
        open,
    })
}

fn classify_html_name(name: &str) -> Option<HtmlKind> {
    if name.eq_ignore_ascii_case("b") || name.eq_ignore_ascii_case("strong") {
        Some(HtmlKind::Bold)
    } else if name.eq_ignore_ascii_case("i") || name.eq_ignore_ascii_case("em") {
        Some(HtmlKind::Italic)
    } else if name.eq_ignore_ascii_case("s")
        || name.eq_ignore_ascii_case("del")
        || name.eq_ignore_ascii_case("strike")
    {
        Some(HtmlKind::Strike)
    } else if name.eq_ignore_ascii_case("u") || name.eq_ignore_ascii_case("ins") {
        Some(HtmlKind::Underline)
    } else if name.eq_ignore_ascii_case("mark") {
        Some(HtmlKind::Mark)
    } else if name.eq_ignore_ascii_case("code") {
        Some(HtmlKind::Code)
    } else if name.eq_ignore_ascii_case("kbd") {
        Some(HtmlKind::Kbd)
    } else if name.eq_ignore_ascii_case("sub") {
        Some(HtmlKind::Sub)
    } else if name.eq_ignore_ascii_case("sup") {
        Some(HtmlKind::Sup)
    } else if name.eq_ignore_ascii_case("br") {
        Some(HtmlKind::Br)
    } else {
        None
    }
}

/// Leaf's `to_superscript` (`src/markdown/latex.rs`). Unmapped characters stay.
fn to_superscript(character: char) -> char {
    match character {
        '0' => '⁰',
        '1' => '¹',
        '2' => '²',
        '3' => '³',
        '4' => '⁴',
        '5' => '⁵',
        '6' => '⁶',
        '7' => '⁷',
        '8' => '⁸',
        '9' => '⁹',
        '+' => '⁺',
        '-' | '−' => '⁻',
        '=' => '⁼',
        '(' => '⁽',
        ')' => '⁾',
        'n' => 'ⁿ',
        'i' => 'ⁱ',
        _ => character,
    }
}

/// Leaf's `to_subscript`. Unmapped characters stay.
fn to_subscript(character: char) -> char {
    match character {
        '0' => '₀',
        '1' => '₁',
        '2' => '₂',
        '3' => '₃',
        '4' => '₄',
        '5' => '₅',
        '6' => '₆',
        '7' => '₇',
        '8' => '₈',
        '9' => '₉',
        '+' => '₊',
        '-' | '−' => '₋',
        '=' => '₌',
        '(' => '₍',
        ')' => '₎',
        'a' => 'ₐ',
        'e' => 'ₑ',
        'i' => 'ᵢ',
        'j' => 'ⱼ',
        'k' => 'ₖ',
        'n' => 'ₙ',
        'o' => 'ₒ',
        'p' => 'ₚ',
        'r' => 'ᵣ',
        's' => 'ₛ',
        't' => 'ₜ',
        'u' => 'ᵤ',
        'x' => 'ₓ',
        _ => character,
    }
}

fn mapped_script(grapheme: &str, superscript: bool) -> String {
    let mut chars = grapheme.chars();
    let Some(character) = chars.next() else {
        return grapheme.to_string();
    };
    if chars.next().is_some() {
        return grapheme.to_string();
    }
    let mapped = if superscript {
        to_superscript(character)
    } else {
        to_subscript(character)
    };
    if mapped == character {
        grapheme.to_string()
    } else {
        mapped.to_string()
    }
}

fn mark_background(theme: &Theme) -> Option<Color> {
    theme
        .find_match_highlight_style
        .as_ref()
        .and_then(|style| style.bg)
        .or_else(|| {
            theme
                .colors
                .get("editor.findMatchHighlightBackground")
                .copied()
        })
}

fn paint_background(color: Color, theme: &Theme) -> Color {
    match color {
        Color::Rgba { r, g, b, a } => crate::color::blend_color(
            Color::Rgba { r, g, b, a },
            theme.style.bg.unwrap_or(Color::Rgb { r: 0, g: 0, b: 0 }),
        ),
        other => other,
    }
}

fn apply_code_style(theme: &Theme, style: &mut Style) {
    let Some(code) = theme.get_style("markup.inline.raw") else {
        return;
    };
    if let Some(fg) = code.fg {
        style.fg = Some(fg);
    }
    if let Some(bg) = code.bg {
        style.bg = Some(paint_background(bg, theme));
    }
    style.bold |= code.bold;
    style.italic |= code.italic;
    style.underline |= code.underline;
}

fn apply_kbd_style(theme: &Theme, style: &mut Style) {
    let fg = theme.colors.get("badge.foreground").copied();
    let bg = theme.colors.get("badge.background").copied();
    if fg.is_none() && bg.is_none() {
        style.underline = true;
        return;
    }
    if let Some(fg) = fg {
        style.fg = Some(fg);
    }
    if let Some(bg) = bg {
        style.bg = Some(paint_background(bg, theme));
    }
}

fn apply_html_styles(
    theme: &Theme,
    spans: &[HtmlSpan],
    byte: usize,
    grapheme: &str,
    cell: &mut super::render_buffer::Cell,
) {
    let mut style = cell.style.clone();
    let mut text = grapheme.to_string();
    let mut strike = false;
    let mut touched = false;
    for span in spans {
        if byte < span.start || byte >= span.end {
            continue;
        }
        touched = true;
        match span.kind {
            HtmlKind::Bold => style.bold = true,
            HtmlKind::Italic => style.italic = true,
            HtmlKind::Underline => style.underline = true,
            HtmlKind::Strike => strike = true,
            HtmlKind::Mark => {
                if let Some(bg) = mark_background(theme) {
                    style.bg = Some(paint_background(bg, theme));
                }
            }
            HtmlKind::Code => apply_code_style(theme, &mut style),
            HtmlKind::Kbd => apply_kbd_style(theme, &mut style),
            HtmlKind::Sub => text = mapped_script(grapheme, false),
            HtmlKind::Sup => text = mapped_script(grapheme, true),
            HtmlKind::Br => {}
        }
    }
    if !touched {
        return;
    }
    if strike && !text.contains(STRIKE_OVERLAY) {
        text.push(STRIKE_OVERLAY);
    }
    cell.style = style;
    if text != grapheme {
        cell.c = text.chars().next().unwrap_or(' ');
        cell.text = text;
    }
}

#[derive(Debug)]
pub(super) struct CachedConcealment {
    revision: u64,
    extended_marks: bool,
    conceal: bool,
    math: bool,
    lines: Arc<LineConcealments>,
    alerts: Arc<AlertMap>,
    html: Arc<HtmlMap>,
}

pub(super) fn alert_color(theme: &Theme, kind: AlertKind) -> Color {
    theme
        .colors
        .get(kind.color_key())
        .copied()
        .or_else(|| theme.get_style("markup.quote").and_then(|style| style.fg))
        .or(theme.style.fg)
        .unwrap_or(Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        })
}

impl super::Editor {
    pub(super) fn markdown_conceal_enabled(&self, window: &crate::window::Window) -> bool {
        window.wrap
            && window
                .markdown_conceal
                .unwrap_or(self.config.markdown_conceal.unwrap_or(true))
            && self
                .highlight_language_id_for_buffer_index(window.buffer_index)
                .as_deref()
                == Some("markdown")
    }

    pub(super) fn markdown_extended_marks_enabled(&self, window: &crate::window::Window) -> bool {
        window
            .markdown_extended_marks
            .unwrap_or(self.config.markdown_extended_marks.unwrap_or(true))
    }

    pub(super) fn markdown_line_concealments(
        &self,
        window: &crate::window::Window,
    ) -> Option<Arc<LineConcealments>> {
        let conceal = self.markdown_conceal_enabled(window);
        let math = self.markdown_math_active(window);
        if !conceal && !math {
            return None;
        }
        let buffer = self.buffer_manager.get(window.buffer_index)?;
        if buffer.byte_len() > MAX_TABLE_DOCUMENT_BYTES {
            return None;
        }
        let extended_marks = self.markdown_extended_marks_enabled(window);
        let id = buffer.id();
        let revision = buffer.revision();
        let source = buffer.contents();
        let mut cache = self.markdown_conceal_cache.borrow_mut();
        if cache.len() >= 8 && !cache.contains_key(&id) {
            cache.clear();
        }
        let build = || {
            let parsed = if conceal {
                parse_concealment(&source, extended_marks)
            } else {
                ConcealedSource {
                    lines: LineConcealments::empty(&source),
                    alerts: AlertMap::default(),
                    html: HtmlMap::default(),
                }
            };
            let mut lines = parsed.lines;
            if math {
                lines = lines.with_swaps(super::markdown_blocks::swaps_by_line(&source));
            }
            CachedConcealment {
                revision,
                extended_marks,
                conceal,
                math,
                lines: Arc::new(lines),
                alerts: Arc::new(parsed.alerts),
                html: Arc::new(parsed.html),
            }
        };
        let cached = cache.entry(id).or_insert_with(&build);
        if cached.revision != revision
            || cached.extended_marks != extended_marks
            || cached.conceal != conceal
            || cached.math != math
        {
            *cached = build();
        }
        Some(cached.lines.clone())
    }

    fn markdown_alert_map(&self, window: &crate::window::Window) -> Option<Arc<AlertMap>> {
        if !self.markdown_conceal_enabled(window) {
            return None;
        }
        let buffer = self.buffer_manager.get(window.buffer_index)?;
        self.markdown_line_concealments(window)?;
        self.markdown_conceal_cache
            .borrow()
            .get(&buffer.id())
            .map(|cached| cached.alerts.clone())
    }

    /// Colors alert gutters and draws a marker label over a concealed `> [!KIND]`.
    pub(super) fn paint_markdown_alert_segment(
        &self,
        buffer: &mut super::render_buffer::RenderBuffer,
        window: &crate::window::Window,
        segment: &LineSegment,
        line: &str,
    ) {
        let Some(alerts) = self.markdown_alert_map(window) else {
            return;
        };
        let Some(alert) = alerts.line(segment.line) else {
            return;
        };
        let x = self.window_to_terminal_x(window, self.gutter_width_for_window(window) + 1);
        let y = self.window_to_terminal_y(window, segment.row);
        let width = self.window_content_width(window);
        let tab_width = self
            .indentation_for_buffer_index(window.buffer_index)
            .shift_width
            .max(1);
        for gutter in &alert.gutters {
            if gutter.byte > line.len() {
                continue;
            }
            let column = display_width_with_tabs(&line[..gutter.byte], tab_width);
            if segment.hides_source_column(column)
                || column < segment.start_col
                || column >= segment.end_col
            {
                continue;
            }
            let local = segment.visual_offset + segment.visible_columns_before(column);
            if local >= width {
                continue;
            }
            let cell_x = x + local;
            let pos = y * buffer.width + cell_x;
            if let Some(cell) = buffer.cells.get_mut(pos) {
                cell.style.fg = Some(alert_color(&self.theme, gutter.kind));
            }
        }
        let Some(label) = alert.label else {
            return;
        };
        if !segment.first_segment || label.byte > line.len() {
            return;
        }
        let column = display_width_with_tabs(&line[..label.byte], tab_width);
        if !segment.hides_source_column(column) || column < segment.start_col {
            return;
        }
        let local = segment.visual_offset + segment.visible_columns_before(column);
        if local >= width {
            return;
        }
        let style = Style {
            fg: Some(alert_color(&self.theme, label.kind)),
            bg: self.theme.style.bg,
            bold: true,
            ..Style::default()
        };
        buffer.set_text(x + local, y, label.kind.label(), &style);
    }

    fn markdown_html_map(&self, window: &crate::window::Window) -> Option<Arc<HtmlMap>> {
        if !self.markdown_conceal_enabled(window) {
            return None;
        }
        let buffer = self.buffer_manager.get(window.buffer_index)?;
        self.markdown_line_concealments(window)?;
        self.markdown_conceal_cache
            .borrow()
            .get(&buffer.id())
            .map(|cached| cached.html.clone())
    }

    fn markdown_line_revealed(&self, window: &crate::window::Window, line: usize) -> bool {
        self.markdown_conceal_reveal(window)
            .is_some_and(|range| range.contains(&line))
            || self
                .markdown_reveal_range(window)
                .is_some_and(|range| range.contains(&line))
            || (window.active && self.image_line_is_raw(line))
    }

    /// Styles the text between balanced inline HTML tags on this row.
    pub(super) fn paint_markdown_html_segment(
        &self,
        buffer: &mut super::render_buffer::RenderBuffer,
        window: &crate::window::Window,
        segment: &LineSegment,
        line: &str,
    ) {
        if self.markdown_line_revealed(window, segment.line) {
            return;
        }
        let Some(html) = self.markdown_html_map(window) else {
            return;
        };
        let Some(styled) = html.line(segment.line) else {
            return;
        };
        let x = self.window_to_terminal_x(window, self.gutter_width_for_window(window) + 1);
        let y = self.window_to_terminal_y(window, segment.row);
        let width = self.window_content_width(window);
        let tab_width = self
            .indentation_for_buffer_index(window.buffer_index)
            .shift_width
            .max(1);
        let mut byte = 0;
        while byte < line.len() {
            let Some(grapheme) = line[byte..].graphemes(true).next() else {
                break;
            };
            let next = byte + grapheme.len();
            let covered = styled
                .spans
                .iter()
                .any(|span| byte >= span.start && byte < span.end);
            if covered {
                let column = display_width_with_tabs(&line[..byte], tab_width);
                let on_row = !segment.hides_source_column(column)
                    && column >= segment.start_col
                    && column < segment.end_col;
                if on_row {
                    let local = segment.visual_offset + segment.visible_columns_before(column);
                    if local < width {
                        let pos = y * buffer.width + x + local;
                        if let Some(cell) = buffer.cells.get_mut(pos) {
                            apply_html_styles(&self.theme, &styled.spans, byte, grapheme, cell);
                        }
                    }
                }
            }
            byte = next;
        }
    }

    /// Lines whose inline syntax is shown raw so the cursor's byte is visible.
    pub(super) fn markdown_conceal_reveal(
        &self,
        window: &crate::window::Window,
    ) -> Option<Range<usize>> {
        if !window.active {
            return None;
        }
        let conceal = self.markdown_conceal_enabled(window);
        let math = self.markdown_math_active(window);
        if !conceal && !math {
            return None;
        }
        let line = window.vtop + window.cy;
        if self.has_multi_cursor_session() {
            return Some(0..self.current_buffer().len());
        }
        if self.is_visual() {
            let anchor = self.selection_start.map_or(line, |point| point.y);
            return Some(anchor.min(line)..anchor.max(line) + 1);
        }
        if self.is_insert()
            || self.is_search()
            || self.pending_operator.is_some()
            || self.pending_replace
            || self.pending_character_motion.is_some()
        {
            return Some(line..line + 1);
        }
        let concealments = self.markdown_line_concealments(window)?;
        let text = self.buffer_manager.get(window.buffer_index)?.get(line)?;
        let text = trim_line_ending(&text);
        let byte = text
            .grapheme_indices(true)
            .nth(window.cx)
            .map_or(text.len(), |(byte, _)| byte);
        concealments
            .line(line)
            .hides_byte(byte)
            .then_some(line..line + 1)
    }

    pub(super) fn toggle_markdown_extended_marks(&mut self) {
        let enabled = self
            .window_manager
            .active_window()
            .map(|window| {
                window
                    .markdown_extended_marks
                    .unwrap_or(self.config.markdown_extended_marks.unwrap_or(true))
            })
            .unwrap_or(true);
        if let Some(window) = self.window_manager.active_window_mut() {
            window.markdown_extended_marks = Some(!enabled);
        }
        self.vleft = 0;
        self.skipcol = 0;
        self.set_legacy_message(Some(
            if enabled {
                "Markdown highlight marks disabled"
            } else {
                "Markdown highlight marks enabled"
            }
            .to_string(),
        ));
    }

    pub(super) fn toggle_markdown_conceal(&mut self) {
        let enabled = self
            .window_manager
            .active_window()
            .map(|window| {
                window
                    .markdown_conceal
                    .unwrap_or(self.config.markdown_conceal.unwrap_or(true))
            })
            .unwrap_or(true);
        if let Some(window) = self.window_manager.active_window_mut() {
            window.markdown_conceal = Some(!enabled);
        }
        self.vleft = 0;
        self.skipcol = 0;
        self.set_legacy_message(Some(
            if enabled {
                "Markdown conceal disabled"
            } else {
                "Markdown conceal enabled"
            }
            .to_string(),
        ));
    }

    pub(super) fn concealment_for_line(&self, line: usize) -> Concealment {
        let Some(window) = self.active_window_with_editor_view() else {
            return Concealment::default();
        };
        let revealed = self
            .markdown_conceal_reveal(&window)
            .is_some_and(|range| range.contains(&line))
            || self
                .markdown_reveal_range(&window)
                .is_some_and(|range| range.contains(&line))
            || self.image_line_is_raw(line);
        if revealed {
            return Concealment::default();
        }
        self.markdown_line_concealments(&window)
            .map(|lines| lines.line(line))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        buffer::Buffer,
        config::Config,
        editor::{render_buffer::RenderBuffer, Action, Editor, Mode},
        test_utils::EditorTestExt,
        theme::{Style, Theme},
    };
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn hidden_slices(source: &str) -> Vec<String> {
        let lines = conceal_markdown(source);
        let mut offsets = vec![0];
        offsets.extend(source.match_indices('\n').map(|(index, _)| index + 1));
        let mut slices = Vec::new();
        for (index, _) in offsets.iter().enumerate() {
            let content = line_content_range(source, &offsets, index);
            let text = &source[content];
            let concealment = lines.line(index);
            if concealment.is_empty() {
                continue;
            }
            // Re-read ranges through the public hide check by scanning bytes.
            let mut start = None;
            for byte in 0..=text.len() {
                let hidden = byte < text.len() && concealment.hides_byte(byte);
                match (start, hidden) {
                    (None, true) => start = Some(byte),
                    (Some(from), false) => {
                        slices.push(text[from..byte].to_string());
                        start = None;
                    }
                    _ => {}
                }
            }
        }
        slices
    }

    #[test]
    fn hides_link_emphasis_code_image_and_strike_markers() {
        assert_eq!(
            hidden_slices("See [docs](https://example.com/a) now"),
            vec!["[", "](https://example.com/a)"]
        );
        assert_eq!(
            hidden_slices("**bold** and *it*"),
            vec!["**", "**", "*", "*"]
        );
        assert_eq!(hidden_slices("use `code` here"), vec!["`", "`"]);
        assert_eq!(
            hidden_slices("![alt](https://example.com/img.png)"),
            vec!["![", "](https://example.com/img.png)"]
        );
        assert_eq!(hidden_slices("~~gone~~"), vec!["~~", "~~"]);
        let nested = visible_text("**[docs](https://example.com)**");
        assert_eq!(nested, "docs");
    }

    fn visible_text(source: &str) -> String {
        let lines = conceal_markdown(source);
        let mut offsets = vec![0];
        offsets.extend(source.match_indices('\n').map(|(index, _)| index + 1));
        let mut visible = String::new();
        for (index, _) in offsets.iter().enumerate() {
            let text = &source[line_content_range(source, &offsets, index)];
            let concealment = lines.line(index);
            for (byte, character) in text.char_indices() {
                if !concealment.hides_byte(byte) {
                    visible.push(character);
                }
            }
        }
        visible
    }

    #[test]
    fn leaves_code_fences_escapes_and_reference_definitions_visible() {
        assert!(hidden_slices("```\n[docs](https://example.com)\n```\n").is_empty());
        assert!(hidden_slices(r"\*not bold\* and \[not](https://example.com)").is_empty());
        let definition = hidden_slices("[id]: https://example.com/target\n");
        assert!(
            definition
                .iter()
                .all(|slice| !slice.contains("example.com")),
            "{definition:?}"
        );
    }

    #[test]
    fn multiline_emphasis_hides_each_delimiter_on_its_own_line() {
        let source = "**bold\nstill**\n";
        let lines = conceal_markdown(source);
        assert!(lines.line(0).hides_byte(0));
        assert!(!lines.line(0).hides_byte(2));
        assert!(lines
            .line(1)
            .hides_byte(source.lines().nth(1).unwrap().find('*').unwrap()));
        assert!(!lines.line(1).hides_byte(0));
    }

    fn markdown_editor(path: &str, source: &str, width: usize, height: usize) -> Editor {
        let mut config: Config = toml::from_str(include_str!("../../default_config.toml")).unwrap();
        config.lsp.servers.clear();
        config.clipboard.enabled = false;
        config.scrolloff = Some(0);
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            lsp,
            width,
            height,
            config,
            Theme::default(),
            vec![Buffer::new(Some(path.into()), source.into())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor
    }

    fn flat(screen: &str) -> String {
        screen
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect()
    }

    fn screen(editor: &mut Editor) -> String {
        let mut frame = RenderBuffer::new(
            editor.size.0 as usize,
            editor.size.1 as usize,
            &Style::default(),
        );
        editor.render(&mut frame).unwrap();
        frame
            .cells
            .chunks(frame.width)
            .take(editor.size.1 as usize - 2)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    async fn key(editor: &mut Editor, code: KeyCode) {
        editor
            .test_execute_event(crossterm::event::Event::Key(KeyEvent::new(
                code,
                KeyModifiers::NONE,
            )))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn wrapped_markdown_uses_visible_width_and_screen_lines() {
        let url = "https://example.com/docs/conceal/aware/wrapping/path";
        let source = format!("alpha bravo [guide]({url}) charlie delta echo foxtrot golf\n");
        let mut editor = markdown_editor("notes.md", &source, 36, 14);
        let window = editor.active_window_with_editor_view().unwrap();
        let width = editor.window_content_width(&window);
        let concealed = editor.layout_for_window(&window);
        let concealed_rows = concealed
            .rows
            .iter()
            .filter(|segment| segment.line == 0)
            .count();
        editor
            .test_execute_action(Action::ToggleMarkdownConceal)
            .await
            .unwrap();
        let window = editor.active_window_with_editor_view().unwrap();
        let raw_rows = editor
            .layout_for_window(&window)
            .rows
            .iter()
            .filter(|segment| segment.line == 0)
            .count();
        assert!(
            concealed_rows < raw_rows,
            "visible rows {concealed_rows} should be fewer than raw rows {raw_rows} at width {width}"
        );
        editor
            .test_execute_action(Action::ToggleMarkdownConceal)
            .await
            .unwrap();
        let rendered = flat(&screen(&mut editor));
        assert!(rendered.contains("guide"), "{rendered}");
        assert!(!rendered.contains("example.com"), "{rendered}");

        let origin = editor.cx;
        editor
            .test_execute_action(Action::MoveScreenLineDown)
            .await
            .unwrap();
        assert_eq!(editor.buffer_line(), 0);
        assert!(editor.cx > origin);
        let landed = editor.current_line_contents().unwrap();
        let landed = trim_line_ending(&landed);
        let byte = landed
            .grapheme_indices(true)
            .nth(editor.cx)
            .map(|(byte, _)| byte)
            .unwrap_or(landed.len());
        let window = editor.active_window_with_editor_view().unwrap();
        let map = editor.markdown_line_concealments(&window).unwrap();
        assert!(
            !map.line(0).hides_byte(byte),
            "screen motion landed in concealed syntax at byte {byte}"
        );
        editor
            .test_execute_action(Action::MoveScreenLineUp)
            .await
            .unwrap();
        assert_eq!(editor.cx, origin);

        key(&mut editor, KeyCode::Char('f')).await;
        key(&mut editor, KeyCode::Char('[')).await;
        let revealed = flat(&screen(&mut editor));
        assert!(
            revealed.contains("example.com"),
            "entering the link should reveal its source: {revealed}"
        );
        assert_eq!(editor.current_buffer().contents(), source);
        let before = editor.cx;
        key(&mut editor, KeyCode::Char('x')).await;
        assert_ne!(editor.current_buffer().contents(), source);
        assert!(editor.current_buffer().contents().contains("guide]("));
        assert!(editor.cx <= before);
    }

    #[tokio::test]
    async fn insert_reveals_only_the_cursor_line_and_other_languages_stay_raw() {
        let source = "See [docs](https://example.com/a).\nNext [more](https://example.com/b).\n";
        let mut editor = markdown_editor("notes.md", source, 80, 12);
        key(&mut editor, KeyCode::Char('i')).await;
        assert_eq!(editor.mode, Mode::Insert);
        let rendered = flat(&screen(&mut editor));
        assert!(rendered.contains("https://example.com/a"), "{rendered}");
        assert!(
            !rendered.contains("https://example.com/b"),
            "the following line stays concealed: {rendered}"
        );
        key(&mut editor, KeyCode::Esc).await;
        let rendered = flat(&screen(&mut editor));
        assert!(!rendered.contains("example.com"), "{rendered}");

        let mut rust = markdown_editor("lib.rs", source, 80, 12);
        let rendered = flat(&screen(&mut rust));
        assert!(rendered.contains("https://example.com/a"), "{rendered}");
    }

    #[test]
    fn search_highlight_follows_visible_columns_on_other_lines() {
        let source = "cursor line\nSee [docs](https://example.com) tail\n";
        let mut editor = markdown_editor("notes.md", source, 80, 12);
        let highlight = crate::color::Color::Rgb {
            r: 20,
            g: 40,
            b: 60,
        };
        let marked = Style {
            bg: Some(highlight),
            ..Style::default()
        };
        editor.theme.find_match_style = Some(marked.clone());
        editor.theme.find_match_highlight_style = Some(marked);
        editor.search_term = "tail".to_string();
        let line = "See [docs](https://example.com) tail";
        let tail = line.find("tail").unwrap();
        let tail_col = crate::unicode_utils::display_width(&line[..tail]);
        let window = editor.active_window_with_editor_view().unwrap();
        let segment = editor
            .layout_for_window(&window)
            .rows
            .iter()
            .find(|segment| segment.line == 1)
            .unwrap()
            .clone();
        let spans = segment.visible_screen_spans(tail_col, tail_col + 4);
        assert_eq!(
            spans,
            vec![(segment.visual_offset + 9, segment.visual_offset + 13)]
        );
        assert_ne!(spans[0].0, tail_col);

        let mut frame = RenderBuffer::new(80, 12, &Style::default());
        editor.render(&mut frame).unwrap();
        let row = segment.row;
        let term_y = editor.window_to_terminal_y(&window, row);
        let origin =
            editor.window_to_terminal_x(&window, editor.gutter_width_for_window(&window) + 1);
        let highlighted: String = (spans[0].0..spans[0].1)
            .map(|local| {
                let cell = &frame.cells[term_y * frame.width + origin + local];
                assert_eq!(cell.style.bg, Some(highlight), "{local}: {}", cell.text);
                cell.text.clone()
            })
            .collect();
        assert_eq!(highlighted, "tail");
        let raw = &frame.cells[term_y * frame.width + origin + tail_col];
        assert_ne!(raw.style.bg, Some(highlight));
    }

    #[test]
    fn alert_markers_are_concealed_and_other_quotes_stay_visible() {
        let note = conceal_markdown("> [!NOTE]\n> body\n");
        assert!(note.line(0).hides_byte(0));
        assert!(note.line(0).hides_byte(2));
        assert!(!note.line(1).hides_byte(0), "the body gutter stays a cell");

        let tight = conceal_markdown(">[!tip]\n> next\n");
        assert!(tight.line(0).hides_byte(0));
        assert!(!tight.line(1).hides_byte(0));

        let caution = conceal_markdown("> [!CaUtIoN]\n> care\n");
        assert!(caution.line(0).hides_byte(0));

        let unknown = conceal_markdown("> [!NOPE]\n> body\n");
        assert!(
            !unknown.line(0).hides_byte(0),
            "an unknown kind stays a normal quote"
        );

        let trailing = conceal_markdown("> [!NOTE] extra\n");
        assert!(!trailing.line(0).hides_byte(0));

        let nested = conceal_markdown("> [!NOTE]\n> > [!WARNING]\n> > inner\n");
        assert!(nested.line(0).hides_byte(0));
        assert!(!nested.line(1).hides_byte(0), "the outer gutter stays");
        assert!(
            nested.line(1).hides_byte(2),
            "the inner marker is concealed"
        );
        assert!(!nested.line(2).hides_byte(0));
        assert!(!nested.line(2).hides_byte(2));

        let fenced = conceal_markdown("```\n> [!NOTE]\n```\n");
        assert!(!fenced.line(1).hides_byte(0));
    }

    fn content_rows(editor: &mut Editor) -> Vec<String> {
        let mut frame = RenderBuffer::new(
            editor.size.0 as usize,
            editor.size.1 as usize,
            &Style::default(),
        );
        editor.render(&mut frame).unwrap();
        let window = editor.active_window_with_editor_view().unwrap();
        let origin =
            editor.window_to_terminal_x(&window, editor.gutter_width_for_window(&window) + 1);
        let width = editor.window_content_width(&window);
        let height = editor.window_content_height(&window);
        (0..height)
            .map(|row| {
                let y = editor.window_to_terminal_y(&window, row);
                frame.cells[y * frame.width + origin..y * frame.width + origin + width]
                    .iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn cell_fg(editor: &mut Editor, line: usize, column: usize) -> Option<crate::color::Color> {
        let mut frame = RenderBuffer::new(
            editor.size.0 as usize,
            editor.size.1 as usize,
            &Style::default(),
        );
        editor.render(&mut frame).unwrap();
        let window = editor.active_window_with_editor_view().unwrap();
        let segment = editor
            .layout_for_window(&window)
            .rows
            .iter()
            .find(|segment| segment.line == line)
            .unwrap()
            .clone();
        let y = editor.window_to_terminal_y(&window, segment.row);
        let origin =
            editor.window_to_terminal_x(&window, editor.gutter_width_for_window(&window) + 1);
        let local = segment.visual_offset + segment.visible_columns_before(column);
        frame.cells[y * frame.width + origin + local].style.fg
    }

    fn source_line(source: &str, text: &str) -> usize {
        source
            .lines()
            .position(|line| line == text)
            .unwrap_or_else(|| panic!("missing line {text}"))
    }

    #[tokio::test]
    async fn github_alerts_render_labels_colors_and_reveal_on_the_marker() {
        let source = "\
intro
> [!NOTE]
> noted

> [!TIP]
> tipped

> [!IMPORTANT]
> important

> [!WARNING]
> warned

> [!CAUTION]
> cautioned

> [!NOPE]
> plain

> [!NOTE]
> outer
> > [!WARNING]
> > nested
tail
";
        let note_line = source_line(source, "> [!NOTE]");
        let body_line = source_line(source, "> noted");
        let tip_line = source_line(source, "> [!TIP]");
        let nope_line = source_line(source, "> [!NOPE]");
        let nested_marker = source_line(source, "> > [!WARNING]");
        let nested_body = source_line(source, "> > nested");
        let mut editor = markdown_editor("notes.md", source, 80, 40);
        editor.theme = crate::theme::parse_vscode_theme("themes/red.json").unwrap();
        let rows = content_rows(&mut editor);
        assert_eq!(rows[note_line], "ℹ Note");
        assert_eq!(rows[tip_line], "✦ Tip");
        assert_eq!(rows[source_line(source, "> [!IMPORTANT]")], "◆ Important");
        assert_eq!(rows[source_line(source, "> [!WARNING]")], "⚠ Warning");
        assert_eq!(rows[source_line(source, "> [!CAUTION]")], "× Caution");
        assert!(
            rows[nope_line].contains("[!NOPE]"),
            "unknown kind stays visible: {}",
            rows[nope_line]
        );
        assert!(
            !rows.iter().any(|row| row.contains("[!NOTE]")),
            "the marker stays concealed off the cursor: {rows:?}"
        );
        assert_eq!(rows[nested_marker], "> ⚠ Warning");
        assert_eq!(rows[nested_body], "> > nested");

        let note = alert_color(&editor.theme, AlertKind::Note);
        let warning = alert_color(&editor.theme, AlertKind::Warning);
        assert_eq!(
            note, editor.theme.colors["editorInfo.foreground"],
            "Note uses editorInfo.foreground"
        );
        assert_eq!(cell_fg(&mut editor, note_line, 0), Some(note));
        assert_eq!(
            cell_fg(&mut editor, body_line, 0),
            Some(note),
            "body gutter"
        );
        assert_eq!(
            cell_fg(&mut editor, nested_marker, 0),
            Some(note),
            "outer gutter"
        );
        assert_eq!(
            cell_fg(&mut editor, nested_body, 2),
            Some(warning),
            "inner gutter"
        );

        editor.theme = crate::theme::parse_vscode_theme("themes/github-light.json").unwrap();
        let light_note = alert_color(&editor.theme, AlertKind::Note);
        let quote = editor
            .theme
            .get_style("markup.quote")
            .and_then(|style| style.fg);
        assert_eq!(
            light_note,
            quote.unwrap(),
            "Note falls back to markup.quote"
        );
        assert!(!editor.theme.colors.contains_key("editorInfo.foreground"));
        let light_tip = alert_color(&editor.theme, AlertKind::Tip);
        assert_eq!(light_tip, editor.theme.colors["terminal.ansiGreen"]);
        assert_eq!(cell_fg(&mut editor, note_line, 0), Some(light_note));
        assert_eq!(cell_fg(&mut editor, tip_line, 0), Some(light_tip));

        key(&mut editor, KeyCode::Char('j')).await;
        let revealed = content_rows(&mut editor);
        assert!(
            revealed[note_line].contains("[!NOTE]"),
            "the cursor line shows the marker: {}",
            revealed[note_line]
        );
        assert!(!revealed[note_line].contains('ℹ'));
        assert_eq!(revealed[tip_line], "✦ Tip", "the next alert stays a label");
        assert_eq!(editor.current_buffer().contents(), source);
    }

    fn line_visible(source: &str, index: usize) -> String {
        let lines = conceal_markdown(source);
        let mut offsets = vec![0];
        offsets.extend(source.match_indices('\n').map(|(offset, _)| offset + 1));
        let text = &source[line_content_range(source, &offsets, index)];
        let concealment = lines.line(index);
        text.char_indices()
            .filter(|(byte, _)| !concealment.hides_byte(*byte))
            .map(|(_, character)| character)
            .collect()
    }

    #[test]
    fn inline_html_tags_conceal_balanced_pairs_only() {
        assert_eq!(
            visible_text("See <b>bold</b> and <strong>s</strong>"),
            "See bold and s"
        );
        assert_eq!(
            visible_text("See <i>i</i> <em>e</em> <u>u</u> <ins>n</ins>"),
            "See i e u n"
        );
        assert_eq!(
            visible_text("See <s>s</s> <del>d</del> <strike>k</strike>"),
            "See s d k"
        );
        assert_eq!(
            visible_text("See <mark>m</mark> <code>c</code> <kbd>k</kbd>"),
            "See m c k"
        );
        assert_eq!(
            visible_text("See <sub>h2o</sub> <sup>n2</sup>"),
            "See h2o n2"
        );
        assert_eq!(visible_text("See <br> next <br/> more"), "See  next  more");
        assert_eq!(visible_text("See <b><i>x</i></b>"), "See x");
        assert_eq!(visible_text("See <b>open"), "See <b>open");
        assert_eq!(visible_text("See <b>one</b> <i>two"), "See one <i>two");
        assert_eq!(visible_text("See `<b>no</b>` here"), "See <b>no</b> here");
        assert!(visible_text("```\n<b>no</b>\n```\n").contains("<b>no</b>"));
        assert_eq!(
            visible_text("See <code>a <b>b</b></code> end"),
            "See a <b>b</b> end"
        );
        assert_eq!(
            visible_text("See <kbd>a <b>b</b></kbd> end"),
            "See a <b>b</b> end"
        );
        assert_eq!(visible_text("See <span>x</span>"), "See <span>x</span>");
        assert_eq!(
            visible_text("See <!-- <b>x</b> --> later"),
            "See <!-- <b>x</b> --> later"
        );
        assert_eq!(visible_text("See <b title=\"a>b\">z</b>"), "See z");
        assert_eq!(visible_text("See <STRONG>x</STRONG>"), "See x");
        assert_eq!(
            line_visible("<div>\nblock <b>stay</b>\n</div>\n", 1),
            "block <b>stay</b>"
        );
        assert_eq!(line_visible("<b>\n\nSee <b>x</b>\n", 0), "<b>");
        assert_eq!(line_visible("<b>\n\nSee <b>x</b>\n", 2), "See x");
    }

    fn after_open(line: &str, open: &str) -> usize {
        line.find(open).unwrap() + open.len()
    }

    fn content_cell(editor: &mut Editor, line: usize, column: usize) -> (String, Style) {
        let mut frame = RenderBuffer::new(
            editor.size.0 as usize,
            editor.size.1 as usize,
            &Style::default(),
        );
        editor.render(&mut frame).unwrap();
        let window = editor.active_window_with_editor_view().unwrap();
        let segment = editor
            .layout_for_window(&window)
            .rows
            .iter()
            .find(|segment| segment.line == line)
            .unwrap()
            .clone();
        let y = editor.window_to_terminal_y(&window, segment.row);
        let origin =
            editor.window_to_terminal_x(&window, editor.gutter_width_for_window(&window) + 1);
        let local = segment.visual_offset + segment.visible_columns_before(column);
        let cell = &frame.cells[y * frame.width + origin + local];
        (cell.text.clone(), cell.style.clone())
    }

    #[tokio::test]
    async fn inline_html_renders_styles_reveal_and_wraps_without_the_tags() {
        let source = "\
intro
See <b>bold</b> and <strong>strong</strong> here
See <i>italic</i> and <em>em</em> here
See <u>under</u> and <ins>ins</ins> here
See <s>gone</s> here
See <mark>marked</mark> here
See <code>code</code> and <kbd>key</kbd> here
See <sub>h2o</sub> and <sup>n2z</sup> here
See <br> break
See <b><i>both</i></b> nested
See <b>open only
See `<b>coded</b>` span
See <code>a <b>inner</b></code> raw
See <span>plain</span> unknown
";
        let mut editor = markdown_editor("notes.md", source, 80, 24);
        editor.theme = crate::theme::parse_vscode_theme("themes/red.json").unwrap();
        let rows = content_rows(&mut editor);
        let bold_line = "See <b>bold</b> and <strong>strong</strong> here";
        let bold = source_line(source, bold_line);
        assert_eq!(rows[bold], "See bold and strong here");
        let (text, style) = content_cell(&mut editor, bold, after_open(bold_line, "<b>"));
        assert_eq!(text, "b");
        assert!(style.bold, "bold tag");
        let (_, strong) = content_cell(&mut editor, bold, after_open(bold_line, "<strong>"));
        assert!(strong.bold, "strong tag");

        let italic_line = "See <i>italic</i> and <em>em</em> here";
        let italic = source_line(source, italic_line);
        assert_eq!(rows[italic], "See italic and em here");
        assert!(
            content_cell(&mut editor, italic, after_open(italic_line, "<i>"))
                .1
                .italic
        );
        assert!(
            content_cell(&mut editor, italic, after_open(italic_line, "<em>"))
                .1
                .italic
        );

        let under_line = "See <u>under</u> and <ins>ins</ins> here";
        let under = source_line(source, under_line);
        assert_eq!(rows[under], "See under and ins here");
        assert!(
            content_cell(&mut editor, under, after_open(under_line, "<u>"))
                .1
                .underline
        );
        assert!(
            content_cell(&mut editor, under, after_open(under_line, "<ins>"))
                .1
                .underline
        );

        let strike_line = "See <s>gone</s> here";
        let strike = source_line(source, strike_line);
        assert_eq!(rows[strike].replace(STRIKE_OVERLAY, ""), "See gone here");
        assert_eq!(
            content_cell(&mut editor, strike, after_open(strike_line, "<s>")).0,
            format!("g{STRIKE_OVERLAY}")
        );

        let marked_line = "See <mark>marked</mark> here";
        let marked = source_line(source, marked_line);
        assert_eq!(rows[marked], "See marked here");
        let mark_bg = editor
            .theme
            .find_match_highlight_style
            .as_ref()
            .and_then(|style| style.bg);
        assert_eq!(
            content_cell(&mut editor, marked, after_open(marked_line, "<mark>"))
                .1
                .bg,
            mark_bg
        );

        let code_line = "See <code>code</code> and <kbd>key</kbd> here";
        let code = source_line(source, code_line);
        assert_eq!(rows[code], "See code and key here");
        let code_fg = editor
            .theme
            .get_style("markup.inline.raw")
            .and_then(|style| style.fg);
        assert_eq!(
            content_cell(&mut editor, code, after_open(code_line, "<code>"))
                .1
                .fg,
            code_fg
        );
        let (_, kbd) = content_cell(&mut editor, code, after_open(code_line, "<kbd>"));
        assert_eq!(kbd.fg, editor.theme.colors.get("badge.foreground").copied());
        assert_eq!(kbd.bg, editor.theme.colors.get("badge.background").copied());

        let scripts = source_line(source, "See <sub>h2o</sub> and <sup>n2z</sup> here");
        assert_eq!(rows[scripts], "See h₂ₒ and ⁿ²z here");

        let breaks = source_line(source, "See <br> break");
        assert_eq!(rows[breaks], "See  break");

        let nested_line = "See <b><i>both</i></b> nested";
        let nested = source_line(source, nested_line);
        assert_eq!(rows[nested], "See both nested");
        let (_, both) = content_cell(&mut editor, nested, nested_line.find("both").unwrap());
        assert!(both.bold && both.italic, "nested bold and italic");

        let open = source_line(source, "See <b>open only");
        assert_eq!(rows[open], "See <b>open only");
        let coded = source_line(source, "See `<b>coded</b>` span");
        assert!(rows[coded].contains("<b>coded</b>"), "{}", rows[coded]);
        let raw = source_line(source, "See <code>a <b>inner</b></code> raw");
        assert_eq!(rows[raw], "See a <b>inner</b> raw");
        let unknown = source_line(source, "See <span>plain</span> unknown");
        assert_eq!(rows[unknown], "See <span>plain</span> unknown");

        key(&mut editor, KeyCode::Char('j')).await;
        key(&mut editor, KeyCode::Char('f')).await;
        key(&mut editor, KeyCode::Char('<')).await;
        let revealed = content_rows(&mut editor);
        assert!(
            revealed[bold].contains("<b>"),
            "the tag under the cursor is raw: {}",
            revealed[bold]
        );
        assert_eq!(revealed[italic], "See italic and em here");
        assert_eq!(editor.current_buffer().contents(), source);

        let wrapped = format!("Go {}\n", "<b>ab</b> ".repeat(12));
        let mut editor = markdown_editor("notes.md", &wrapped, 40, 12);
        let window = editor.active_window_with_editor_view().unwrap();
        let concealed_rows = editor
            .layout_for_window(&window)
            .rows
            .iter()
            .filter(|segment| segment.line == 0)
            .count();
        editor
            .test_execute_action(Action::ToggleMarkdownConceal)
            .await
            .unwrap();
        let window = editor.active_window_with_editor_view().unwrap();
        let raw_rows = editor
            .layout_for_window(&window)
            .rows
            .iter()
            .filter(|segment| segment.line == 0)
            .count();
        assert!(
            concealed_rows < raw_rows,
            "visible rows {concealed_rows} should be fewer than raw rows {raw_rows}"
        );
    }

    #[test]
    fn extended_marks_conceal_leaf_pairs_and_leave_literals() {
        assert_eq!(
            visible_text("before ==marked== after"),
            "before marked after"
        );
        assert_eq!(visible_text("==one== ==two=="), "one two");
        assert_eq!(visible_text("==unclosed"), "==unclosed");
        assert_eq!(visible_text("===="), "====");
        assert_eq!(visible_text("== text =="), "== text ==");
        assert_eq!(
            visible_text("x == y and ==marked== end"),
            "x == y and marked end"
        );
        assert_eq!(visible_text("==héllo=="), "héllo");
        assert_eq!(visible_text("see `==code==` here"), "see ==code== here");
        assert!(visible_text("```\n==no==\n```\n").contains("==no=="));
        assert_eq!(visible_text("<!-- ==x== -->"), "<!-- ==x== -->");
        assert_eq!(line_visible("<div>\n==stay==\n</div>\n", 1), "==stay==");
        assert_eq!(visible_text("see [a](http://x.com/==b==) end"), "see a end");
        assert_eq!(
            visible_text("see ==[a](http://x.com)== end"),
            "see ==a== end"
        );
    }

    #[tokio::test]
    async fn extended_marks_paint_reveal_and_follow_the_setting() {
        let source = "\
intro
See ==marked== here
";
        let marked_line = "See ==marked== here";
        let mut editor = markdown_editor("notes.md", source, 80, 12);
        editor.theme = crate::theme::parse_vscode_theme("themes/red.json").unwrap();
        let line = source_line(source, marked_line);
        let column = marked_line.find("marked").unwrap();
        let rows = content_rows(&mut editor);
        assert_eq!(rows[line], "See marked here");
        let expected =
            mark_background(&editor.theme).map(|color| paint_background(color, &editor.theme));
        assert!(expected.is_some());
        assert_eq!(content_cell(&mut editor, line, column).1.bg, expected);
        let revision = editor.current_buffer().revision();

        editor.config.markdown_extended_marks = Some(false);
        let raw = content_rows(&mut editor);
        assert_eq!(raw[line], marked_line);
        assert_eq!(editor.current_buffer().revision(), revision);
        editor.config.markdown_extended_marks = Some(true);
        assert_eq!(content_rows(&mut editor)[line], "See marked here");

        let mut light = markdown_editor("notes.md", source, 80, 12);
        light.theme = crate::theme::parse_vscode_theme("themes/github-light.json").unwrap();
        let light_bg =
            mark_background(&light.theme).map(|color| paint_background(color, &light.theme));
        assert_ne!(light_bg, expected);
        assert_eq!(content_cell(&mut light, line, column).1.bg, light_bg);

        editor
            .test_execute_action(Action::ToggleMarkdownExtendedMarks)
            .await
            .unwrap();
        assert_eq!(content_rows(&mut editor)[line], marked_line);
        editor
            .test_execute_action(Action::ToggleMarkdownExtendedMarks)
            .await
            .unwrap();
        assert_eq!(content_rows(&mut editor)[line], "See marked here");

        key(&mut editor, KeyCode::Char('j')).await;
        key(&mut editor, KeyCode::Char('f')).await;
        key(&mut editor, KeyCode::Char('=')).await;
        let revealed = content_rows(&mut editor);
        assert!(
            revealed[line].contains("=="),
            "the cursor on a marker shows the source: {}",
            revealed[line]
        );
        assert_eq!(editor.current_buffer().contents(), source);

        let wrapped = format!("Go {}\n", "==ab== ".repeat(12));
        let mut editor = markdown_editor("notes.md", &wrapped, 40, 12);
        let window = editor.active_window_with_editor_view().unwrap();
        let concealed_rows = editor
            .layout_for_window(&window)
            .rows
            .iter()
            .filter(|segment| segment.line == 0)
            .count();
        editor
            .test_execute_action(Action::ToggleMarkdownExtendedMarks)
            .await
            .unwrap();
        let window = editor.active_window_with_editor_view().unwrap();
        let raw_rows = editor
            .layout_for_window(&window)
            .rows
            .iter()
            .filter(|segment| segment.line == 0)
            .count();
        assert!(
            concealed_rows < raw_rows,
            "visible rows {concealed_rows} should be fewer than raw rows {raw_rows}"
        );
    }
}
