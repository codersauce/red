//! Inline Markdown concealment for wrapping and cursor columns.
//!
//! Table rows already project away pipes and inline syntax. Paragraphs still
//! wrapped on the raw line, so a hidden URL consumed screen columns the user
//! could not see. This module records the syntax that should occupy no cells —
//! emphasis and strike markers, link brackets and destinations, image markup,
//! and code-span backticks — and display layout skips those bytes when it
//! measures a row.
//!
//! The buffer stays the raw document. A cursor that steps into a concealed span,
//! or an insert, visual, search, or operator-pending edit, reveals that line so
//! the cell under the cursor is the byte that will be changed.

use std::{ops::Range, sync::Arc};

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use unicode_segmentation::UnicodeSegmentation as _;

use super::display_layout::{Concealment, LineConcealments};
use super::markdown_tables::MAX_TABLE_DOCUMENT_BYTES;
use crate::unicode_utils::trim_line_ending;

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

/// Concealed byte ranges for every line in `source`.
pub(super) fn conceal_markdown(source: &str) -> LineConcealments {
    let mut offsets = vec![0];
    offsets.extend(source.match_indices('\n').map(|(index, _)| index + 1));
    let mut hidden = vec![Vec::new(); offsets.len()];
    let mut stack = Vec::new();
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
        match event {
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
    LineConcealments::new(hidden.into_iter().map(Concealment::new).collect())
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

#[derive(Debug)]
pub(super) struct CachedConcealment {
    revision: u64,
    lines: Arc<LineConcealments>,
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

    pub(super) fn markdown_line_concealments(
        &self,
        window: &crate::window::Window,
    ) -> Option<Arc<LineConcealments>> {
        if !self.markdown_conceal_enabled(window) {
            return None;
        }
        let buffer = self.buffer_manager.get(window.buffer_index)?;
        if buffer.byte_len() > MAX_TABLE_DOCUMENT_BYTES {
            return None;
        }
        let mut cache = self.markdown_conceal_cache.borrow_mut();
        if cache.len() >= 8 && !cache.contains_key(&buffer.id()) {
            cache.clear();
        }
        let cached = cache
            .entry(buffer.id())
            .or_insert_with(|| CachedConcealment {
                revision: buffer.revision(),
                lines: Arc::new(conceal_markdown(&buffer.contents())),
            });
        if cached.revision != buffer.revision() {
            *cached = CachedConcealment {
                revision: buffer.revision(),
                lines: Arc::new(conceal_markdown(&buffer.contents())),
            };
        }
        Some(cached.lines.clone())
    }

    /// Lines whose inline syntax is shown raw so the cursor's byte is visible.
    pub(super) fn markdown_conceal_reveal(
        &self,
        window: &crate::window::Window,
    ) -> Option<Range<usize>> {
        if !window.active || !self.markdown_conceal_enabled(window) {
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
        if self.is_insert() || self.is_search() || self.file_vim.pending() {
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
}
