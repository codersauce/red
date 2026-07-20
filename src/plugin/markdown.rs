//! Width-aware Markdown rendering for source-backed text panels.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use unicode_segmentation::UnicodeSegmentation;

#[cfg(test)]
use super::text_link::TextPanelFileLocation;
use super::text_link::{
    linkify_source_locations, markdown_link_target, TextPanelLink, TextPanelLinkTarget,
};
use crate::{highlighter::Highlighter, theme::Style, unicode_utils::display_width};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextPanelSpanStyle {
    User,
    Agent,
    Error,
    Text,
    Heading,
    Strong,
    Emphasis,
    Strikethrough,
    InlineCode,
    Code,
    Link,
    Quote,
    Muted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RenderedTextSpan {
    pub(crate) text: String,
    pub(crate) style: TextPanelSpanStyle,
    pub(crate) syntax_style: Option<Style>,
    pub(crate) link: Option<TextPanelLink>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RenderedTextLine {
    pub(crate) spans: Vec<RenderedTextSpan>,
}

impl RenderedTextLine {
    pub(crate) fn plain(text: String, style: TextPanelSpanStyle) -> Self {
        Self {
            spans: vec![RenderedTextSpan {
                text,
                style,
                syntax_style: None,
                link: None,
            }],
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.spans.iter().all(|span| span.text.is_empty())
    }
}

#[derive(Clone, Debug)]
struct StyledToken {
    spans: Vec<RenderedTextSpan>,
    width: usize,
    whitespace: bool,
}

#[derive(Clone, Debug)]
struct ListState {
    next: Option<u64>,
}

#[derive(Clone, Debug)]
struct ItemState {
    marker: String,
    continuation: String,
    needs_marker: bool,
}

#[derive(Clone, Debug, Default)]
struct TableCell {
    spans: Vec<RenderedTextSpan>,
}

#[derive(Clone, Debug)]
struct TableState {
    alignments: Vec<Alignment>,
    header: Vec<TableCell>,
    rows: Vec<Vec<TableCell>>,
    current_row: Vec<TableCell>,
    current_cell: Option<TableCell>,
    in_header: bool,
}

impl TableState {
    fn new(alignments: Vec<Alignment>) -> Self {
        Self {
            alignments,
            header: Vec::new(),
            rows: Vec::new(),
            current_row: Vec::new(),
            current_cell: None,
            in_header: false,
        }
    }

    fn finish_cell(&mut self) {
        if let Some(cell) = self.current_cell.take() {
            self.current_row.push(cell);
        }
    }

    fn finish_row(&mut self) {
        self.finish_cell();
        if self.current_row.is_empty() {
            return;
        }
        let row = std::mem::take(&mut self.current_row);
        if self.in_header {
            self.header = row;
        } else {
            self.rows.push(row);
        }
    }
}

struct MarkdownRenderer<'a> {
    width: usize,
    lines: Vec<RenderedTextLine>,
    current: Vec<RenderedTextSpan>,
    styles: Vec<TextPanelSpanStyle>,
    links: Vec<Option<TextPanelLink>>,
    next_link_id: u64,
    lists: Vec<ListState>,
    items: Vec<ItemState>,
    quote_depth: usize,
    code: Option<(String, String)>,
    table: Option<TableState>,
    highlighter: Option<&'a mut Highlighter>,
}

impl<'a> MarkdownRenderer<'a> {
    fn new(width: usize, highlighter: Option<&'a mut Highlighter>) -> Self {
        Self {
            width,
            lines: Vec::new(),
            current: Vec::new(),
            styles: vec![TextPanelSpanStyle::Agent],
            links: Vec::new(),
            next_link_id: 0,
            lists: Vec::new(),
            items: Vec::new(),
            quote_depth: 0,
            code: None,
            table: None,
            highlighter,
        }
    }

    fn render(mut self, text: &str) -> Vec<RenderedTextLine> {
        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        options.insert(Options::ENABLE_TASKLISTS);
        options.insert(Options::ENABLE_STRIKETHROUGH);

        for event in Parser::new_ext(text, options) {
            self.event(event);
        }
        self.flush_current();
        while self.lines.last().is_some_and(RenderedTextLine::is_empty) {
            self.lines.pop();
        }
        self.lines
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.append_text(&text),
            Event::Code(text) | Event::InlineMath(text) => {
                self.append_linkified(&text, TextPanelSpanStyle::InlineCode);
            }
            Event::DisplayMath(text) => self.append(&text, TextPanelSpanStyle::Code),
            Event::Html(text) | Event::InlineHtml(text) => self.append_text(&text),
            Event::FootnoteReference(text) => {
                self.append("[", TextPanelSpanStyle::Muted);
                self.append(&text, TextPanelSpanStyle::Muted);
                self.append("]", TextPanelSpanStyle::Muted);
            }
            Event::SoftBreak => self.append(" ", self.current_style()),
            Event::HardBreak => self.flush_current(),
            Event::Rule => {
                self.flush_current();
                self.blank_line();
                let (prefix, _) = self.take_prefixes();
                let rule_width = self.width.saturating_sub(spans_width(&prefix));
                let mut line = RenderedTextLine { spans: prefix };
                push_span(
                    &mut line.spans,
                    "─".repeat(rule_width),
                    TextPanelSpanStyle::Muted,
                );
                self.lines.push(line);
                self.blank_line();
            }
            Event::TaskListMarker(checked) => {
                self.append(if checked { "☑ " } else { "☐ " }, TextPanelSpanStyle::Muted)
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { .. } => {
                self.flush_current();
                self.blank_line();
                self.append("▍ ", TextPanelSpanStyle::Heading);
                self.styles.push(TextPanelSpanStyle::Heading);
            }
            Tag::BlockQuote(_) => {
                self.flush_current();
                if self.quote_depth == 0 {
                    self.blank_line();
                }
                self.quote_depth += 1;
                self.styles.push(TextPanelSpanStyle::Quote);
            }
            Tag::CodeBlock(kind) => {
                self.flush_current();
                self.blank_line();
                let language = match kind {
                    CodeBlockKind::Fenced(language) => language
                        .split([',', ' ', '\t'])
                        .next()
                        .unwrap_or_default()
                        .to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                let title = if language.is_empty() {
                    "┌─ code".to_string()
                } else {
                    format!("┌─ {language}")
                };
                let (prefix, continuation) = self.take_prefixes();
                let spans = vec![RenderedTextSpan {
                    text: title,
                    style: TextPanelSpanStyle::Muted,
                    syntax_style: None,
                    link: None,
                }];
                self.lines
                    .extend(wrap_spans(&spans, self.width, &prefix, &continuation));
                self.code = Some((language, String::new()));
            }
            Tag::List(start) => {
                self.flush_current();
                if self.lists.is_empty() {
                    self.blank_line();
                }
                self.lists.push(ListState { next: start });
            }
            Tag::Item => {
                self.flush_current();
                let marker = match self.lists.last_mut().and_then(|list| list.next.as_mut()) {
                    Some(number) => {
                        let marker = format!("{number}. ");
                        *number = number.saturating_add(1);
                        marker
                    }
                    None => "• ".to_string(),
                };
                self.items.push(ItemState {
                    continuation: " ".repeat(display_width(&marker)),
                    marker,
                    needs_marker: true,
                });
            }
            Tag::Emphasis => self.styles.push(TextPanelSpanStyle::Emphasis),
            Tag::Strong => self.styles.push(TextPanelSpanStyle::Strong),
            Tag::Strikethrough => self.styles.push(TextPanelSpanStyle::Strikethrough),
            Tag::Link { dest_url, .. } => {
                self.styles.push(TextPanelSpanStyle::Link);
                let link = markdown_link_target(&dest_url).map(|target| self.new_link(target));
                self.links.push(link);
            }
            Tag::Image { .. } => self.styles.push(TextPanelSpanStyle::Link),
            Tag::Table(alignments) => {
                self.flush_current();
                self.blank_line();
                self.table = Some(TableState::new(alignments));
            }
            Tag::TableHead => {
                if let Some(table) = self.table.as_mut() {
                    table.in_header = true;
                    table.current_row.clear();
                }
            }
            Tag::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    table.current_row.clear();
                }
            }
            Tag::TableCell => {
                if let Some(table) = self.table.as_mut() {
                    table.current_cell = Some(TableCell::default());
                }
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush_current();
                if self.items.is_empty() && self.quote_depth == 0 {
                    self.blank_line();
                }
            }
            TagEnd::Heading(_) => {
                self.flush_current();
                self.styles.pop();
                self.blank_line();
            }
            TagEnd::BlockQuote(_) => {
                self.flush_current();
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.styles.pop();
                if self.quote_depth == 0 {
                    self.blank_line();
                }
            }
            TagEnd::CodeBlock => {
                let Some((language, code)) = self.code.take() else {
                    return;
                };
                let code_lines =
                    highlighted_code_lines(&language, &code, self.highlighter.as_deref_mut());
                let (_, continuation) = self.take_prefixes();
                let mut code_prefix = continuation;
                push_span(
                    &mut code_prefix,
                    "│ ".to_string(),
                    TextPanelSpanStyle::Muted,
                );
                for spans in code_lines {
                    self.lines.extend(wrap_verbatim(
                        &spans,
                        self.width,
                        &code_prefix,
                        &code_prefix,
                    ));
                }
                let (_, continuation) = self.take_prefixes();
                self.lines.extend(wrap_verbatim(
                    &[RenderedTextSpan {
                        text: "└─".to_string(),
                        style: TextPanelSpanStyle::Muted,
                        syntax_style: None,
                        link: None,
                    }],
                    self.width,
                    &continuation,
                    &continuation,
                ));
                self.blank_line();
            }
            TagEnd::List(_) => {
                self.flush_current();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.blank_line();
                }
            }
            TagEnd::Item => {
                self.flush_current();
                self.items.pop();
            }
            TagEnd::Link => {
                self.styles.pop();
                self.links.pop();
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Image => {
                self.styles.pop();
            }
            TagEnd::TableCell => {
                if let Some(table) = self.table.as_mut() {
                    table.finish_cell();
                }
            }
            TagEnd::TableHead => {
                if let Some(table) = self.table.as_mut() {
                    table.finish_row();
                    table.in_header = false;
                }
            }
            TagEnd::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    table.finish_row();
                }
            }
            TagEnd::Table => {
                let Some(table) = self.table.take() else {
                    return;
                };
                let (first, continuation) = self.take_prefixes();
                self.lines
                    .extend(render_table(table, self.width, &first, &continuation));
                self.blank_line();
            }
            _ => {}
        }
    }

    fn current_style(&self) -> TextPanelSpanStyle {
        self.styles
            .last()
            .copied()
            .unwrap_or(TextPanelSpanStyle::Agent)
    }

    fn append_text(&mut self, text: &str) {
        if let Some((_, code)) = self.code.as_mut() {
            code.push_str(text);
            return;
        }
        self.append_linkified(text, self.current_style());
    }

    fn append(&mut self, text: &str, style: TextPanelSpanStyle) {
        self.append_with_link(text, style, self.links.last().cloned().flatten());
    }

    fn append_linkified(&mut self, text: &str, style: TextPanelSpanStyle) {
        if let Some(link) = self.links.last().cloned().flatten() {
            self.append_with_link(text, style, Some(link));
            return;
        }
        for (fragment, target) in linkify_source_locations(text) {
            let link = target.map(|target| self.new_link(target));
            let fragment_style = if link.is_some() {
                TextPanelSpanStyle::Link
            } else {
                style
            };
            self.append_with_link(fragment, fragment_style, link);
        }
    }

    fn append_with_link(
        &mut self,
        text: &str,
        style: TextPanelSpanStyle,
        link: Option<TextPanelLink>,
    ) {
        if text.is_empty() {
            return;
        }
        if let Some(table) = self.table.as_mut() {
            if let Some(cell) = table.current_cell.as_mut() {
                push_span_with_link(&mut cell.spans, text.to_string(), style, link);
            }
            return;
        }
        push_span_with_link(&mut self.current, text.to_string(), style, link);
    }

    fn new_link(&mut self, target: TextPanelLinkTarget) -> TextPanelLink {
        let link = TextPanelLink {
            id: self.next_link_id,
            target,
        };
        self.next_link_id = self.next_link_id.saturating_add(1);
        link
    }

    fn flush_current(&mut self) {
        if self.current.is_empty() {
            return;
        }
        let current = std::mem::take(&mut self.current);
        let (first, continuation) = self.take_prefixes();
        self.lines
            .extend(wrap_spans(&current, self.width, &first, &continuation));
    }

    fn take_prefixes(&mut self) -> (Vec<RenderedTextSpan>, Vec<RenderedTextSpan>) {
        let mut first = Vec::new();
        let mut continuation = Vec::new();
        for _ in 0..self.quote_depth {
            push_span(&mut first, "│ ".to_string(), TextPanelSpanStyle::Quote);
            push_span(
                &mut continuation,
                "│ ".to_string(),
                TextPanelSpanStyle::Quote,
            );
        }
        for item in &mut self.items {
            let prefix = if item.needs_marker {
                item.needs_marker = false;
                item.marker.clone()
            } else {
                item.continuation.clone()
            };
            push_span(&mut first, prefix, TextPanelSpanStyle::User);
            push_span(
                &mut continuation,
                item.continuation.clone(),
                TextPanelSpanStyle::Text,
            );
        }
        (first, continuation)
    }

    fn blank_line(&mut self) {
        if self.lines.is_empty() || self.lines.last().is_some_and(RenderedTextLine::is_empty) {
            return;
        }
        self.lines.push(RenderedTextLine { spans: Vec::new() });
    }
}

pub(crate) fn wrap_plain_text(
    text: &str,
    width: usize,
    style: TextPanelSpanStyle,
) -> Vec<RenderedTextLine> {
    if width == 0 {
        return Vec::new();
    }
    let mut next_link_id = 0;
    text.split('\n')
        .flat_map(|line| {
            let spans = linkified_spans(line.trim_end_matches('\r'), style, &mut next_link_id);
            wrap_spans(&spans, width, &[], &[])
        })
        .collect()
}

fn linkified_spans(
    text: &str,
    style: TextPanelSpanStyle,
    next_link_id: &mut u64,
) -> Vec<RenderedTextSpan> {
    if text.is_empty() {
        return vec![RenderedTextSpan {
            text: String::new(),
            style,
            syntax_style: None,
            link: None,
        }];
    }
    linkify_source_locations(text)
        .into_iter()
        .map(|(text, target)| {
            let link = target.map(|target| {
                let link = TextPanelLink {
                    id: *next_link_id,
                    target,
                };
                *next_link_id = next_link_id.saturating_add(1);
                link
            });
            RenderedTextSpan {
                text: text.to_string(),
                style: if link.is_some() {
                    TextPanelSpanStyle::Link
                } else {
                    style
                },
                syntax_style: None,
                link,
            }
        })
        .collect()
}

pub(crate) fn render_markdown_lines(text: &str, width: usize) -> Vec<RenderedTextLine> {
    render_markdown_lines_with_highlighter(text, width, None)
}

pub(crate) fn render_markdown_lines_with_highlighter(
    text: &str,
    width: usize,
    highlighter: Option<&mut Highlighter>,
) -> Vec<RenderedTextLine> {
    if width == 0 || text.is_empty() {
        return Vec::new();
    }
    MarkdownRenderer::new(width, highlighter).render(text)
}

fn highlighted_code_lines(
    language: &str,
    code: &str,
    highlighter: Option<&mut Highlighter>,
) -> Vec<Vec<RenderedTextSpan>> {
    let styles = highlighter
        .and_then(|highlighter| {
            let language = highlighter.language_id_for_name(language)?;
            highlighter.highlight(language, code).ok()
        })
        .unwrap_or_default();
    let mut lines = Vec::new();
    let mut line_start = 0;

    for raw_line in code.split_inclusive('\n') {
        let line = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let line_end = line_start + line.len();
        let mut boundaries = vec![line_start, line_end];
        for style in &styles {
            let start = style.start.max(line_start).min(line_end);
            let end = style.end.max(line_start).min(line_end);
            if start < end && code.is_char_boundary(start) && code.is_char_boundary(end) {
                boundaries.push(start);
                boundaries.push(end);
            }
        }
        boundaries.sort_unstable();
        boundaries.dedup();

        let mut spans = Vec::new();
        for range in boundaries.windows(2) {
            let start = range[0];
            let end = range[1];
            if start == end {
                continue;
            }
            let syntax_style = styles
                .iter()
                .enumerate()
                .filter(|(_, style)| style.start <= start && style.end >= end)
                .min_by(|(left_order, left), (right_order, right)| {
                    (left.end - left.start)
                        .cmp(&(right.end - right.start))
                        .then_with(|| right_order.cmp(left_order))
                })
                .map(|(_, style)| style.style.clone());
            push_span_with_syntax(
                &mut spans,
                code[start..end].replace('\t', "    "),
                syntax_style,
            );
        }
        if spans.is_empty() {
            push_span_with_syntax(&mut spans, line.replace('\t', "    "), None);
        }
        lines.push(spans);
        line_start += raw_line.len();
    }

    lines
}

fn wrap_spans(
    spans: &[RenderedTextSpan],
    width: usize,
    first_prefix: &[RenderedTextSpan],
    continuation_prefix: &[RenderedTextSpan],
) -> Vec<RenderedTextLine> {
    if width == 0 {
        return Vec::new();
    }
    let tokens = styled_tokens(spans);
    if tokens.is_empty() {
        return vec![RenderedTextLine {
            spans: fit_prefix(first_prefix, width),
        }];
    }

    let mut lines = Vec::new();
    let mut prefix = fit_prefix(first_prefix, width.saturating_sub(1));
    let mut content_width = 0usize;
    let mut pending_space = None;

    for token in tokens {
        if token.whitespace {
            if content_width > 0 {
                pending_space = token
                    .spans
                    .first()
                    .map(|span| (span.style, span.syntax_style.clone(), span.link.clone()));
            }
            continue;
        }
        let prefix_width = spans_width(&prefix).saturating_sub(content_width);
        let available = width.saturating_sub(prefix_width).max(1);
        let space_width = usize::from(pending_space.is_some() && content_width > 0);
        if content_width > 0 && content_width + space_width + token.width > available {
            lines.push(RenderedTextLine { spans: prefix });
            prefix = fit_prefix(continuation_prefix, width.saturating_sub(1));
            content_width = 0;
            pending_space = None;
        }
        if let Some((style, syntax_style, link)) = pending_space.take() {
            if content_width > 0 {
                push_rendered_span(&mut prefix, " ".to_string(), style, syntax_style, link);
                content_width += 1;
            }
        }
        let prefix_width = spans_width(&prefix).saturating_sub(content_width);
        let available = width.saturating_sub(prefix_width).max(1);
        if token.width <= available.saturating_sub(content_width) {
            for span in token.spans {
                push_rendered_span(
                    &mut prefix,
                    span.text,
                    span.style,
                    span.syntax_style,
                    span.link,
                );
            }
            content_width += token.width;
            continue;
        }
        for span in token.spans {
            for grapheme in span.text.graphemes(true) {
                let grapheme_width = display_width(grapheme);
                let prefix_width = spans_width(&prefix).saturating_sub(content_width);
                let available = width.saturating_sub(prefix_width).max(1);
                if content_width > 0 && content_width + grapheme_width > available {
                    lines.push(RenderedTextLine { spans: prefix });
                    prefix = fit_prefix(continuation_prefix, width.saturating_sub(1));
                    content_width = 0;
                }
                let prefix_width = spans_width(&prefix).saturating_sub(content_width);
                if content_width == 0 && grapheme_width > width.saturating_sub(prefix_width) {
                    prefix.clear();
                }
                if grapheme_width > width {
                    push_rendered_span(
                        &mut prefix,
                        "…".to_string(),
                        span.style,
                        span.syntax_style.clone(),
                        span.link.clone(),
                    );
                    content_width += 1;
                } else {
                    push_rendered_span(
                        &mut prefix,
                        grapheme.to_string(),
                        span.style,
                        span.syntax_style.clone(),
                        span.link.clone(),
                    );
                    content_width += grapheme_width;
                }
            }
        }
    }
    lines.push(RenderedTextLine { spans: prefix });
    lines
}

fn wrap_verbatim(
    spans: &[RenderedTextSpan],
    width: usize,
    first_prefix: &[RenderedTextSpan],
    continuation_prefix: &[RenderedTextSpan],
) -> Vec<RenderedTextLine> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut current = fit_prefix(first_prefix, width.saturating_sub(1));
    let mut content_width = 0usize;
    for span in spans {
        for grapheme in span.text.graphemes(true) {
            let grapheme_width = display_width(grapheme);
            let prefix_width = spans_width(&current).saturating_sub(content_width);
            let available = width.saturating_sub(prefix_width).max(1);
            if content_width > 0 && content_width + grapheme_width > available {
                lines.push(RenderedTextLine { spans: current });
                current = fit_prefix(continuation_prefix, width.saturating_sub(1));
                content_width = 0;
            }
            let prefix_width = spans_width(&current).saturating_sub(content_width);
            if content_width == 0 && grapheme_width > width.saturating_sub(prefix_width) {
                current.clear();
            }
            if grapheme_width > width {
                push_rendered_span(
                    &mut current,
                    "…".to_string(),
                    span.style,
                    span.syntax_style.clone(),
                    span.link.clone(),
                );
                content_width += 1;
            } else {
                push_rendered_span(
                    &mut current,
                    grapheme.to_string(),
                    span.style,
                    span.syntax_style.clone(),
                    span.link.clone(),
                );
                content_width += grapheme_width;
            }
        }
    }
    lines.push(RenderedTextLine { spans: current });
    lines
}

fn styled_tokens(spans: &[RenderedTextSpan]) -> Vec<StyledToken> {
    let mut tokens = Vec::<StyledToken>::new();
    for span in spans {
        for grapheme in span.text.graphemes(true) {
            let whitespace = grapheme.chars().all(char::is_whitespace);
            let needs_token = tokens
                .last()
                .is_none_or(|token| token.whitespace != whitespace);
            if needs_token {
                tokens.push(StyledToken {
                    spans: Vec::new(),
                    width: 0,
                    whitespace,
                });
            }
            if let Some(token) = tokens.last_mut() {
                push_rendered_span(
                    &mut token.spans,
                    grapheme.to_string(),
                    span.style,
                    span.syntax_style.clone(),
                    span.link.clone(),
                );
                token.width += display_width(grapheme);
            }
        }
    }
    tokens
}

fn render_table(
    mut table: TableState,
    width: usize,
    first_prefix: &[RenderedTextSpan],
    continuation_prefix: &[RenderedTextSpan],
) -> Vec<RenderedTextLine> {
    let columns = table
        .alignments
        .len()
        .max(table.header.len())
        .max(table.rows.iter().map(Vec::len).max().unwrap_or_default());
    if columns == 0 || width == 0 {
        return Vec::new();
    }
    table.header.resize_with(columns, TableCell::default);
    table.alignments.resize(columns, Alignment::None);
    for row in &mut table.rows {
        row.resize_with(columns, TableCell::default);
    }

    let prefix_width = spans_width(first_prefix).max(spans_width(continuation_prefix));
    let available = width.saturating_sub(prefix_width);
    let gap = 2usize;
    let minimum_column = 6usize;
    let minimum_grid = columns * minimum_column + columns.saturating_sub(1) * gap;
    if available < minimum_grid {
        return render_table_records(table, width, first_prefix, continuation_prefix);
    }

    let mut widths = (0..columns)
        .map(|column| {
            std::iter::once(&table.header[column])
                .chain(table.rows.iter().map(|row| &row[column]))
                .map(cell_width)
                .max()
                .unwrap_or_default()
                .max(minimum_column)
        })
        .collect::<Vec<_>>();
    let budget = available.saturating_sub(columns.saturating_sub(1) * gap);
    while widths.iter().sum::<usize>() > budget {
        let Some((index, _)) = widths
            .iter()
            .enumerate()
            .filter(|(_, width)| **width > minimum_column)
            .max_by_key(|(_, width)| **width)
        else {
            return render_table_records(table, width, first_prefix, continuation_prefix);
        };
        widths[index] -= 1;
    }

    let mut lines = Vec::new();
    push_table_row(
        &mut lines,
        &table.header,
        &widths,
        &table.alignments,
        TextPanelSpanStyle::Heading,
        first_prefix,
        continuation_prefix,
        true,
    );
    let mut separator = fit_prefix(continuation_prefix, width);
    for (index, column_width) in widths.iter().enumerate() {
        if index > 0 {
            push_span(&mut separator, " ".repeat(gap), TextPanelSpanStyle::Muted);
        }
        push_span(
            &mut separator,
            "━".repeat(*column_width),
            TextPanelSpanStyle::Muted,
        );
    }
    lines.push(RenderedTextLine { spans: separator });
    for row in &table.rows {
        push_table_row(
            &mut lines,
            row,
            &widths,
            &table.alignments,
            TextPanelSpanStyle::Agent,
            continuation_prefix,
            continuation_prefix,
            false,
        );
    }
    lines
}

#[allow(clippy::too_many_arguments)]
fn push_table_row(
    lines: &mut Vec<RenderedTextLine>,
    row: &[TableCell],
    widths: &[usize],
    alignments: &[Alignment],
    default_style: TextPanelSpanStyle,
    first_prefix: &[RenderedTextSpan],
    continuation_prefix: &[RenderedTextSpan],
    force_style: bool,
) {
    let wrapped = row
        .iter()
        .zip(widths)
        .map(|(cell, width)| {
            let spans = if force_style {
                vec![RenderedTextSpan {
                    text: cell_text(cell),
                    style: default_style,
                    syntax_style: None,
                    link: None,
                }]
            } else if cell.spans.is_empty() {
                vec![RenderedTextSpan {
                    text: String::new(),
                    style: default_style,
                    syntax_style: None,
                    link: None,
                }]
            } else {
                cell.spans.clone()
            };
            wrap_spans(&spans, *width, &[], &[])
        })
        .collect::<Vec<_>>();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
    for row_line in 0..height {
        let mut output = if row_line == 0 {
            first_prefix.to_vec()
        } else {
            continuation_prefix.to_vec()
        };
        for (column, column_width) in widths.iter().enumerate() {
            if column > 0 {
                push_span(&mut output, "  ".to_string(), TextPanelSpanStyle::Muted);
            }
            let spans = wrapped[column]
                .get(row_line)
                .map(|line| line.spans.as_slice())
                .unwrap_or(&[]);
            let used = spans_width(spans);
            let remaining = column_width.saturating_sub(used);
            let (left, right) = match alignments[column] {
                Alignment::Center => (remaining / 2, remaining - remaining / 2),
                Alignment::Right => (remaining, 0),
                Alignment::Left | Alignment::None => (0, remaining),
            };
            push_span(&mut output, " ".repeat(left), TextPanelSpanStyle::Text);
            for span in spans {
                push_rendered_span(
                    &mut output,
                    span.text.clone(),
                    span.style,
                    span.syntax_style.clone(),
                    span.link.clone(),
                );
            }
            if column + 1 < widths.len() {
                push_span(&mut output, " ".repeat(right), TextPanelSpanStyle::Text);
            }
        }
        lines.push(RenderedTextLine { spans: output });
    }
}

fn render_table_records(
    table: TableState,
    width: usize,
    first_prefix: &[RenderedTextSpan],
    continuation_prefix: &[RenderedTextSpan],
) -> Vec<RenderedTextLine> {
    let mut lines = Vec::new();
    let mut first_line = true;
    for (row_index, row) in table.rows.iter().enumerate() {
        for (header, value) in table.header.iter().zip(row) {
            let label = cell_text(header);
            if label.is_empty() && value.spans.is_empty() {
                continue;
            }
            let prefix = if first_line {
                first_prefix
            } else {
                continuation_prefix
            };
            first_line = false;
            lines.extend(wrap_spans(
                &[RenderedTextSpan {
                    text: label,
                    style: TextPanelSpanStyle::Heading,
                    syntax_style: None,
                    link: None,
                }],
                width,
                prefix,
                continuation_prefix,
            ));

            let mut value_prefix = continuation_prefix.to_vec();
            push_span(
                &mut value_prefix,
                "  ".to_string(),
                TextPanelSpanStyle::Text,
            );
            let value_spans = if value.spans.is_empty() {
                vec![RenderedTextSpan {
                    text: "—".to_string(),
                    style: TextPanelSpanStyle::Muted,
                    syntax_style: None,
                    link: None,
                }]
            } else {
                value.spans.clone()
            };
            lines.extend(wrap_spans(
                &value_spans,
                width,
                &value_prefix,
                &value_prefix,
            ));
        }
        if row_index + 1 < table.rows.len() {
            let mut separator = fit_prefix(continuation_prefix, width);
            let remaining = width.saturating_sub(spans_width(&separator));
            push_span(
                &mut separator,
                "─".repeat(remaining),
                TextPanelSpanStyle::Muted,
            );
            lines.push(RenderedTextLine { spans: separator });
        }
    }
    lines
}

fn cell_text(cell: &TableCell) -> String {
    cell.spans
        .iter()
        .map(|span| span.text.as_str())
        .collect::<String>()
}

fn cell_width(cell: &TableCell) -> usize {
    display_width(&cell_text(cell))
}

fn spans_width(spans: &[RenderedTextSpan]) -> usize {
    spans.iter().map(|span| display_width(&span.text)).sum()
}

fn fit_prefix(spans: &[RenderedTextSpan], width: usize) -> Vec<RenderedTextSpan> {
    let mut out = Vec::new();
    let mut used = 0usize;
    for span in spans {
        for grapheme in span.text.graphemes(true) {
            let grapheme_width = display_width(grapheme);
            if used + grapheme_width > width {
                return out;
            }
            push_rendered_span(
                &mut out,
                grapheme.to_string(),
                span.style,
                span.syntax_style.clone(),
                span.link.clone(),
            );
            used += grapheme_width;
        }
    }
    out
}

fn push_span(spans: &mut Vec<RenderedTextSpan>, text: String, style: TextPanelSpanStyle) {
    push_span_with_link(spans, text, style, None);
}

fn push_span_with_link(
    spans: &mut Vec<RenderedTextSpan>,
    text: String,
    style: TextPanelSpanStyle,
    link: Option<TextPanelLink>,
) {
    push_rendered_span(spans, text, style, None, link);
}

fn push_rendered_span(
    spans: &mut Vec<RenderedTextSpan>,
    text: String,
    style: TextPanelSpanStyle,
    syntax_style: Option<Style>,
    link: Option<TextPanelLink>,
) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut() {
        if last.style == style && last.syntax_style == syntax_style && last.link == link {
            last.text.push_str(&text);
            return;
        }
    }
    spans.push(RenderedTextSpan {
        text,
        style,
        syntax_style,
        link,
    });
}

fn push_span_with_syntax(
    spans: &mut Vec<RenderedTextSpan>,
    text: String,
    syntax_style: Option<Style>,
) {
    if text.is_empty() {
        return;
    }
    push_rendered_span(spans, text, TextPanelSpanStyle::Code, syntax_style, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[RenderedTextLine]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.spans.iter().map(|span| span.text.as_str()).collect())
            .collect()
    }

    fn assert_fits(lines: &[RenderedTextLine], width: usize) {
        assert!(
            lines.iter().all(|line| spans_width(&line.spans) <= width),
            "a rendered line exceeded {width} columns: {:?}",
            plain(lines)
        );
    }

    #[test]
    fn plain_text_wraps_at_words_and_preserves_logical_blank_lines() {
        let lines = wrap_plain_text(
            "alpha beta gamma\n\nlast line",
            11,
            TextPanelSpanStyle::Text,
        );

        assert_eq!(plain(&lines), ["alpha beta", "gamma", "", "last line"]);
        assert_fits(&lines, 11);
    }

    #[test]
    fn unicode_and_long_tokens_wrap_without_splitting_graphemes() {
        let family = "👨‍👩‍👧‍👦";
        let lines = wrap_plain_text(
            &format!("A{family}B e\u{301} cafe abcdefghijklmnop"),
            6,
            TextPanelSpanStyle::Text,
        );
        let joined = plain(&lines).join("");

        assert!(joined.contains(family));
        assert!(joined.contains("e\u{301}"));
        assert!(joined.contains("abcdefghijklmnop"));
        assert_fits(&lines, 6);
    }

    #[test]
    fn markdown_renders_semantic_blocks_and_inline_styles() {
        let lines = render_markdown_lines(
            "# Accepted arguments\n\nA **strong** and *emphasized* ~~removed~~ `value` with a [link](https://example.com).\n\n> quoted words\n\n```rust\nfn main() {}\n```",
            44,
        );
        let output = plain(&lines).join("\n");

        assert!(output.contains("▍ Accepted arguments"));
        assert!(output.contains("│ quoted words"));
        assert!(output.contains("┌─ rust"));
        assert!(output.contains("│ fn main() {}"));
        assert!(output.contains("└─"));
        assert!(!output.contains("**"));
        assert!(!output.contains("~~"));
        assert!(!output.contains('`'));
        assert!(lines
            .iter()
            .flatten_spans()
            .any(|span| { span.text == "strong" && span.style == TextPanelSpanStyle::Strong }));
        assert!(lines.iter().flatten_spans().any(|span| {
            span.text == "emphasized" && span.style == TextPanelSpanStyle::Emphasis
        }));
        assert!(lines.iter().flatten_spans().any(|span| {
            span.text == "removed" && span.style == TextPanelSpanStyle::Strikethrough
        }));
        assert!(lines
            .iter()
            .flatten_spans()
            .any(|span| { span.text == "value" && span.style == TextPanelSpanStyle::InlineCode }));
        assert!(lines.iter().flatten_spans().any(|span| {
            span.text == "link"
                && span.style == TextPanelSpanStyle::Link
                && span.link.as_ref().is_some_and(|link| {
                    link.target
                        == TextPanelLinkTarget::ExternalUrl("https://example.com".to_string())
                })
        }));
        assert_fits(&lines, 44);
    }

    #[test]
    fn source_locations_are_links_in_plain_and_markdown_text() {
        let plain_lines = wrap_plain_text("See src/editor.rs:42:7.", 80, TextPanelSpanStyle::Text);
        let markdown_lines = render_markdown_lines("Open `README.md:8`.", 80);

        let plain_link = plain_lines
            .iter()
            .flatten_spans()
            .find_map(|span| span.link.as_ref())
            .unwrap();
        assert_eq!(
            plain_link.target,
            TextPanelLinkTarget::File {
                path: "src/editor.rs".to_string(),
                location: Some(TextPanelFileLocation {
                    line: 42,
                    column: 7,
                }),
            }
        );
        let markdown_link = markdown_lines
            .iter()
            .flatten_spans()
            .find_map(|span| span.link.as_ref())
            .unwrap();
        assert_eq!(
            markdown_link.target,
            TextPanelLinkTarget::File {
                path: "README.md".to_string(),
                location: Some(TextPanelFileLocation { line: 8, column: 1 }),
            }
        );
    }

    #[test]
    fn nested_lists_tasks_and_continuations_keep_indentation() {
        let lines = render_markdown_lines(
            "1. parent item that needs to wrap cleanly\n   - [x] nested complete item\n   - [ ] nested pending item",
            25,
        );
        let output = plain(&lines);

        assert_eq!(output[0], "1. parent item that needs");
        assert_eq!(output[1], "   to wrap cleanly");
        assert!(output.iter().any(|line| line.starts_with("   • ☑ nested")));
        assert!(output.iter().any(|line| line.starts_with("   • ☐ nested")));
        assert_fits(&lines, 25);
    }

    #[test]
    fn screenshot_like_arguments_table_becomes_a_readable_grid() {
        let markdown = "Accepted public arguments:\n\n| Argument | Meaning |\n|---|---|\n| `[FILES...]` | Zero or more files to open/edit. |\n| `-r, --root <PATH>` | Change working directory to PATH. |\n| `--agent-check` | Report Codex app-server readiness. |\n| `--detach[=<SESSION>]` | Start a detachable editor session. |\n| `--help` | Provided automatically by Clap. |";
        let lines = render_markdown_lines(markdown, 50);
        let output = plain(&lines);

        assert!(output
            .iter()
            .any(|line| line.contains("Argument") && line.contains("Meaning")));
        assert!(output.iter().any(|line| line.contains('━')));
        assert!(output.iter().any(|line| line.contains("[FILES...]")));
        assert!(output.iter().any(|line| line.contains("--agent-check")));
        assert!(output.iter().all(|line| !line.contains("|---|")));
        assert!(output.iter().all(|line| !line.starts_with('|')));
        assert_fits(&lines, 50);
    }

    #[test]
    fn narrow_table_falls_back_to_key_value_records() {
        let markdown = "| Argument | Meaning |\n|---|---|\n| `--root <PATH>` | Change working directory. |\n| `--help` | Show help. |";
        let lines = render_markdown_lines(markdown, 13);
        let output = plain(&lines);

        assert_eq!(output[0], "Argument");
        assert!(output.iter().any(|line| line.starts_with("  --root")));
        assert!(output.iter().any(|line| line == "Meaning"));
        assert!(output.iter().any(|line| line.contains('─')));
        assert!(output.iter().all(|line| !line.contains('━')));
        assert_fits(&lines, 13);
    }

    #[test]
    fn zero_width_is_safe_for_plain_and_markdown() {
        assert!(wrap_plain_text("hello", 0, TextPanelSpanStyle::Text).is_empty());
        assert!(render_markdown_lines("# hello\n\n| a | b |\n|-|-|\n| c | d |", 0).is_empty());
    }

    #[test]
    fn rendered_content_stays_within_every_narrow_width() {
        let markdown = "# 漢字 and emoji 👩‍💻\n\n- a verylongtokenwithnowhitespace\n\n| Argument | Meaning |\n|---|---|\n| `--root` | CJK 漢字 and emoji 👩‍💻 |\n\n```rust\nprintln!(\"👩‍💻 漢字\");\n```";

        for width in 1..=52 {
            let lines = render_markdown_lines(markdown, width);
            assert_fits(&lines, width);
        }
    }

    trait FlattenSpans<'a> {
        fn flatten_spans(self) -> Box<dyn Iterator<Item = &'a RenderedTextSpan> + 'a>;
    }

    impl<'a> FlattenSpans<'a> for std::slice::Iter<'a, RenderedTextLine> {
        fn flatten_spans(self) -> Box<dyn Iterator<Item = &'a RenderedTextSpan> + 'a> {
            Box::new(self.flat_map(|line| line.spans.iter()))
        }
    }
}
