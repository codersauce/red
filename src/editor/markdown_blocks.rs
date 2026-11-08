//! Display rows that stand in for a range of Markdown source lines.
//!
//! A block is anchored on its first source line. While it renders, the other
//! lines in the range take no rows. The cursor inside the range, or a
//! non-Normal mode on that range, shows the source again. Rendering never
//! edits the buffer and never enters undo history.
//!
//! Parsed blocks are cached by document revision. Their rows are cached by
//! width, ASCII borders, wrapping, the reveal range, and the theme generation.

use std::{
    collections::{BTreeMap, HashMap},
    ops::Range,
    sync::Arc,
};

use super::display_layout::LineSegment;
use crate::{
    theme::{Style, Theme, ThemeStyleSpec},
    unicode_utils::{display_width, truncate_display_width},
};

#[path = "markdown_frontmatter.rs"]
mod frontmatter;
#[path = "markdown_mermaid.rs"]
mod mermaid;

/// Documents larger than this stay in source form, matching Markdown tables.
pub(super) const MAX_BLOCK_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;

/// What a rendered span should look like. Colors resolve from the theme at
/// paint time so a dark and a light theme share one row cache.
/// Paint roles for rendered block rows. Later phases construct the variants
/// that phase 0 only matches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(super) enum BlockRole {
    /// Body text. `editor.foreground`.
    Plain,
    /// Secondary text. `descriptionForeground`.
    Muted,
    /// Box drawing. `editorIndentGuide.activeBackground`.
    Border,
    /// A short heading on a rendered row.
    Label,
    /// Arrows and edges. `descriptionForeground`.
    Edge,
    /// Frontmatter rules. `editorIndentGuide.background`.
    Guide,
    /// Frontmatter keys. `support.type.property-name`.
    Key,
    /// Frontmatter values. `string`.
    Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BlockSpan {
    pub text: String,
    pub role: BlockRole,
}

impl BlockSpan {
    #[allow(dead_code)]
    pub(super) fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            role: BlockRole::Plain,
        }
    }

    #[allow(dead_code)]
    pub(super) fn muted(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            role: BlockRole::Muted,
        }
    }

    #[allow(dead_code)]
    pub(super) fn role(role: BlockRole, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            role,
        }
    }
}

/// One screen row of a rendered block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BlockRow {
    pub spans: Arc<[BlockSpan]>,
}

impl BlockRow {
    #[allow(dead_code)]
    pub(super) fn plain(text: impl Into<String>) -> Self {
        Self {
            spans: Arc::from([BlockSpan::plain(text)]),
        }
    }

    #[allow(dead_code)]
    pub(super) fn spans(spans: Vec<BlockSpan>) -> Self {
        Self {
            spans: Arc::from(spans),
        }
    }

    #[allow(dead_code)]
    pub(super) fn text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }
}

/// The row carried on a [`LineSegment`] so painting does not look it up again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BlockRowView {
    pub index: u16,
    pub rows: u16,
    pub spans: Arc<[BlockSpan]>,
}

impl BlockRowView {
    pub(super) fn text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }
}

/// A source range replaced by rendered rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BlockSpec {
    pub lines: Range<usize>,
    pub rows: Vec<BlockRow>,
    /// Set when the rows are produced from a Mermaid fence at layout time.
    mermaid: Option<mermaid::Region>,
    /// Set when the rows are produced from leading frontmatter at layout time.
    frontmatter: Option<Vec<(String, String)>>,
}

impl BlockSpec {
    #[allow(dead_code)]
    pub(super) fn new(lines: Range<usize>, rows: Vec<BlockRow>) -> Self {
        Self {
            lines,
            rows,
            mermaid: None,
            frontmatter: None,
        }
    }

    fn mermaid(lines: Range<usize>, region: mermaid::Region) -> Self {
        Self {
            lines,
            rows: Vec::new(),
            mermaid: Some(region),
            frontmatter: None,
        }
    }

    fn frontmatter(lines: Range<usize>, pairs: Vec<(String, String)>) -> Self {
        Self {
            lines,
            rows: Vec::new(),
            mermaid: None,
            frontmatter: Some(pairs),
        }
    }
}

/// Rows for anchors, and the source lines those anchors cover.
#[derive(Debug, Clone, Default)]
pub(super) struct BlockLayout {
    rows: BTreeMap<usize, Vec<LineSegment>>,
    covered: BTreeMap<usize, Range<usize>>,
}

impl BlockLayout {
    pub(super) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub(super) fn display_rows(&self, line: usize) -> Option<&[LineSegment]> {
        self.rows.get(&line).map(Vec::as_slice)
    }

    /// The line belongs to a block that is currently rendered, anchor included.
    pub(super) fn hides(&self, line: usize) -> bool {
        self.covered.contains_key(&line)
    }

    pub(super) fn anchor_of(&self, line: usize) -> Option<usize> {
        self.covered.get(&line).map(|range| range.start)
    }

    /// Last source line the viewport must load so collapsed blocks still leave
    /// room for the text that follows them.
    pub(super) fn source_end_for_viewport(
        &self,
        vtop: usize,
        height: usize,
        skipcol: usize,
        line_count: usize,
    ) -> usize {
        let mut line = vtop;
        let mut rows = 0usize;
        while rows < height && line < line_count {
            if let Some(range) = self.covered.get(&line).cloned() {
                let full = if line == range.start {
                    self.display_rows(line).map(|rows| rows.len()).unwrap_or(0)
                } else {
                    0
                };
                let produced = if line == vtop {
                    full.saturating_sub(skipcol)
                } else {
                    full
                };
                rows = rows.saturating_add(produced);
                line = range.end.max(line + 1);
                continue;
            }
            rows += 1;
            line += 1;
        }
        line
    }
}

pub(super) fn block_role_style(theme: &Theme, role: BlockRole) -> Style {
    let foreground = match role {
        BlockRole::Plain | BlockRole::Label => vec!["editor.foreground".to_string()],
        BlockRole::Muted | BlockRole::Edge => vec![
            "descriptionForeground".to_string(),
            "editor.foreground".to_string(),
        ],
        BlockRole::Border => vec![
            "editorIndentGuide.activeBackground".to_string(),
            "editorIndentGuide.background".to_string(),
            "descriptionForeground".to_string(),
            "editor.foreground".to_string(),
        ],
        BlockRole::Guide => vec![
            "editorIndentGuide.background".to_string(),
            "descriptionForeground".to_string(),
            "editor.foreground".to_string(),
        ],
        BlockRole::Key => vec![
            "scope:support.type.property-name".to_string(),
            "editor.foreground".to_string(),
        ],
        BlockRole::Value => vec!["scope:string".to_string(), "editor.foreground".to_string()],
    };
    let mut style = theme.resolve_style(&ThemeStyleSpec {
        foreground,
        background: vec!["editor.background".to_string()],
        bold: Some(role == BlockRole::Label),
        ..ThemeStyleSpec::default()
    });
    if style.bg.is_none() {
        style.bg = theme.style.bg;
    }
    if style.fg.is_none() {
        style.fg = theme.style.fg;
    }
    style
}

pub(super) fn block_line_segments(line: usize, rows: &[BlockRow]) -> Vec<LineSegment> {
    let count = u16::try_from(rows.len()).unwrap_or(u16::MAX);
    rows.iter()
        .take(usize::from(count))
        .enumerate()
        .map(|(index, row)| LineSegment {
            line,
            row: index,
            start_col: index,
            end_col: index,
            start_grapheme: 0,
            end_grapheme: 0,
            start_byte: 0,
            end_byte: 0,
            start_grapheme_col: 0,
            source_offset: 0,
            first_segment: index == 0,
            last_segment: index + 1 == usize::from(count),
            visual_offset: 0,
            concealed: Vec::new(),
            projection: None,
            projection_left: 0,
            image: None,
            block: Some(BlockRowView {
                index: u16::try_from(index).unwrap_or(u16::MAX),
                rows: count,
                spans: Arc::clone(&row.spans),
            }),
        })
        .collect()
}

pub(super) fn layout_blocks(
    specs: &[BlockSpec],
    reveal: Option<&Range<usize>>,
    width: usize,
    ascii: bool,
) -> BlockLayout {
    let mut layout = BlockLayout::default();
    let mut specs = specs.to_vec();
    specs.sort_by_key(|spec| (spec.lines.start, spec.lines.end));
    let mut occupied_until = 0usize;
    for spec in specs {
        let rendered = if let Some(region) = spec.mermaid.as_ref() {
            Some(mermaid::render(region, width, ascii))
        } else {
            spec.frontmatter
                .as_ref()
                .map(|pairs| frontmatter::render(pairs, width, ascii))
        };
        let rows = rendered.as_deref().unwrap_or(spec.rows.as_slice());
        if spec.lines.start >= spec.lines.end || rows.is_empty() {
            continue;
        }
        if spec.lines.start < occupied_until {
            continue;
        }
        if reveal.is_some_and(|range| ranges_overlap(&spec.lines, range)) {
            continue;
        }
        layout.rows.insert(
            spec.lines.start,
            block_line_segments(spec.lines.start, rows),
        );
        for line in spec.lines.clone() {
            layout.covered.insert(line, spec.lines.clone());
        }
        occupied_until = spec.lines.end;
    }
    layout
}

fn ranges_overlap(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

/// Skip `skip` display columns, then keep at most `width` columns.
pub(super) fn slice_block_text(text: &str, skip: usize, width: usize) -> String {
    if width == 0 || text.is_empty() {
        return String::new();
    }
    let mut column = 0usize;
    let mut start = 0usize;
    let mut started = skip == 0;
    for (index, grapheme) in unicode_segmentation::UnicodeSegmentation::grapheme_indices(text, true)
    {
        let grapheme_width = display_width(grapheme);
        if !started && column + grapheme_width > skip {
            start = index;
            started = true;
        }
        column += grapheme_width;
        if started && column >= skip + width {
            return truncate_display_width(&text[start..index + grapheme.len()], width);
        }
    }
    if !started {
        return String::new();
    }
    truncate_display_width(&text[start..], width)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BlockLayoutKey {
    width: usize,
    ascii: bool,
    wrap: bool,
    reveal_start: usize,
    reveal_end: usize,
}

pub(super) struct CachedBlocks {
    revision: u64,
    theme_generation: u64,
    /// Which rendered Markdown features produced `specs`.
    features: u8,
    specs: Vec<BlockSpec>,
    layouts: HashMap<BlockLayoutKey, Arc<BlockLayout>>,
}

impl super::Editor {
    pub(super) fn block_feature_bits(&self, window: &crate::window::Window) -> u8 {
        u8::from(self.markdown_mermaid_enabled(window))
            | (u8::from(self.markdown_frontmatter_enabled(window)) << 1)
    }

    /// Mermaid fences and leading frontmatter become blocks when their settings are on.
    fn detected_markdown_blocks(&self, window: &crate::window::Window) -> Vec<BlockSpec> {
        let Some(buffer) = self.buffer_manager.get(window.buffer_index) else {
            return Vec::new();
        };
        let text = buffer.contents();
        let mut blocks = Vec::new();
        if self.markdown_frontmatter_enabled(window) {
            if let Some(block) = frontmatter::find_block(&text) {
                blocks.push(block);
            }
        }
        if self.markdown_mermaid_enabled(window) {
            blocks.extend(mermaid::find_blocks(&text));
        }
        blocks
    }

    pub(super) fn markdown_mermaid_enabled(&self, window: &crate::window::Window) -> bool {
        window
            .markdown_mermaid
            .unwrap_or(self.config.markdown_mermaid.unwrap_or(true))
    }

    pub(super) fn toggle_markdown_mermaid(&mut self) {
        let enabled = self
            .window_manager
            .active_window()
            .is_some_and(|window| self.markdown_mermaid_enabled(window));
        if let Some(window) = self.window_manager.active_window_mut() {
            window.markdown_mermaid = Some(!enabled);
        }
        self.vleft = 0;
        self.skipcol = 0;
        self.markdown_block_generation = self.markdown_block_generation.wrapping_add(1);
        self.layout_cache.borrow_mut().clear();
    }

    pub(super) fn markdown_frontmatter_enabled(&self, window: &crate::window::Window) -> bool {
        window
            .markdown_frontmatter
            .unwrap_or(self.config.markdown_frontmatter.unwrap_or(true))
    }

    pub(super) fn toggle_markdown_frontmatter(&mut self) {
        let enabled = self
            .window_manager
            .active_window()
            .is_some_and(|window| self.markdown_frontmatter_enabled(window));
        if let Some(window) = self.window_manager.active_window_mut() {
            window.markdown_frontmatter = Some(!enabled);
        }
        self.vleft = 0;
        self.skipcol = 0;
        self.markdown_block_generation = self.markdown_block_generation.wrapping_add(1);
        self.layout_cache.borrow_mut().clear();
    }

    fn markdown_block_specs(&self, window: &crate::window::Window) -> Vec<BlockSpec> {
        #[cfg(test)]
        if let Some(blocks) = &self.markdown_block_override {
            return blocks.clone();
        }
        if self
            .highlight_language_id_for_buffer_index(window.buffer_index)
            .as_deref()
            != Some("markdown")
        {
            return Vec::new();
        }
        let Some(buffer) = self.buffer_manager.get(window.buffer_index) else {
            return Vec::new();
        };
        if buffer.byte_len() > MAX_BLOCK_DOCUMENT_BYTES {
            return Vec::new();
        }
        self.markdown_block_specs_cached(window)
    }

    fn markdown_block_specs_cached(&self, window: &crate::window::Window) -> Vec<BlockSpec> {
        let Some(buffer) = self.buffer_manager.get(window.buffer_index) else {
            return Vec::new();
        };
        let id = buffer.id();
        let revision = buffer.revision();
        let theme_generation = self.theme_generation;
        let features = self.block_feature_bits(window);
        if let Some(cached) = self.markdown_block_cache.borrow().get(&id) {
            if cached.revision == revision
                && cached.theme_generation == theme_generation
                && cached.features == features
            {
                return cached.specs.clone();
            }
        }
        let specs = self.detected_markdown_blocks(window);
        let mut cache = self.markdown_block_cache.borrow_mut();
        if cache.len() >= 8 && !cache.contains_key(&id) {
            cache.clear();
        }
        cache.insert(
            id,
            CachedBlocks {
                revision,
                theme_generation,
                features,
                specs: specs.clone(),
                layouts: HashMap::new(),
            },
        );
        specs
    }

    pub(super) fn markdown_block_layout(
        &self,
        window: &crate::window::Window,
    ) -> Option<Arc<BlockLayout>> {
        let specs = self.markdown_block_specs(window);
        if specs.is_empty() {
            return None;
        }
        let reveal = self.markdown_block_reveal_range(window);
        #[cfg(test)]
        if self.markdown_block_override.is_some() {
            return Some(Arc::new(layout_blocks(
                &specs,
                reveal.as_ref(),
                self.window_content_width(window),
                self.config.window_borders_ascii,
            )));
        }
        let buffer = self.buffer_manager.get(window.buffer_index)?;
        let mut cache = self.markdown_block_cache.borrow_mut();
        let cached = cache.get_mut(&buffer.id())?;
        let reveal_pair = reveal
            .as_ref()
            .map(|range| (range.start, range.end))
            .unwrap_or((usize::MAX, usize::MAX));
        let width = self.window_content_width(window);
        let ascii = self.config.window_borders_ascii;
        let key = BlockLayoutKey {
            width,
            ascii,
            wrap: window.wrap,
            reveal_start: reveal_pair.0,
            reveal_end: reveal_pair.1,
        };
        if cached.layouts.len() >= 4 && !cached.layouts.contains_key(&key) {
            cached.layouts.clear();
        }
        Some(
            cached
                .layouts
                .entry(key)
                .or_insert_with(|| Arc::new(layout_blocks(&specs, reveal.as_ref(), width, ascii)))
                .clone(),
        )
    }

    /// Lines whose rendered block should fall back to source.
    ///
    /// The cursor line (and a visual selection) reveals the block that contains
    /// it. Insert, visual, search, and operator-pending do the same, because
    /// those modes still report the cursor line here, matching images.
    pub(super) fn markdown_block_reveal_range(
        &self,
        window: &crate::window::Window,
    ) -> Option<Range<usize>> {
        if !window.active || self.markdown_block_specs(window).is_empty() {
            return None;
        }
        let line = window.vtop + window.cy;
        if self.has_multi_cursor_session() {
            return Some(0..self.current_buffer().len().saturating_add(1));
        }
        if self.is_visual() {
            let anchor = self.selection_start.map_or(line, |point| point.y);
            return Some(anchor.min(line)..anchor.max(line) + 1);
        }
        Some(line..line + 1)
    }

    pub(super) fn active_has_markdown_blocks(&self) -> bool {
        self.active_window_with_editor_view()
            .and_then(|window| self.markdown_block_layout(&window))
            .is_some_and(|layout| !layout.is_empty())
    }

    fn active_block_layout(&self) -> Option<Arc<BlockLayout>> {
        self.active_window_with_editor_view()
            .and_then(|window| self.markdown_block_layout(&window))
    }

    pub(super) fn markdown_block_hides_line(&self, line: usize) -> bool {
        self.active_block_layout()
            .is_some_and(|layout| layout.hides(line))
    }

    pub(super) fn markdown_block_anchor(&self, line: usize) -> Option<usize> {
        self.active_block_layout()?.anchor_of(line)
    }

    pub(super) fn markdown_block_display_rows(&self, line: usize) -> Option<Vec<LineSegment>> {
        self.active_block_layout()
            .and_then(|layout| layout.display_rows(line).map(ToOwned::to_owned))
    }

    /// The cursor line is inside a block and that block is showing source.
    pub(super) fn markdown_block_line_is_revealed(&self, line: usize) -> bool {
        let Some(window) = self.active_window_with_editor_view() else {
            return false;
        };
        self.markdown_block_specs(&window)
            .iter()
            .any(|spec| spec.lines.contains(&line))
            && !self.markdown_block_hides_line(line)
    }

    /// First source line after the rendered block that starts at `line + 1`.
    pub(super) fn markdown_line_after_block(&self, line: usize) -> Option<usize> {
        let last = self.last_navigable_line();
        let mut next = line.saturating_add(1);
        if next > last || !self.markdown_block_hides_line(next) {
            return None;
        }
        while next <= last && self.markdown_block_hides_line(next) {
            next += 1;
        }
        (next <= last).then_some(next)
    }

    /// Source line before the rendered block that ends at `line`.
    pub(super) fn markdown_line_before_block(&self, line: usize) -> Option<usize> {
        let mut previous = line.checked_sub(1)?;
        if !self.markdown_block_hides_line(previous) {
            return None;
        }
        while self.markdown_block_hides_line(previous) {
            match previous.checked_sub(1) {
                Some(earlier) => previous = earlier,
                // A block on the first line has no source row above it.
                None => return Some(previous),
            }
        }
        Some(previous)
    }

    #[cfg(test)]
    pub(super) fn test_set_markdown_blocks(&mut self, blocks: Vec<BlockSpec>) {
        self.markdown_block_override = Some(blocks);
        self.markdown_block_generation = self.markdown_block_generation.wrapping_add(1);
        self.layout_cache.borrow_mut().clear();
    }

    /// Draws one rendered block row. The source stays in the buffer.
    pub(super) fn paint_markdown_block_segment(
        &self,
        buffer: &mut crate::editor::RenderBuffer,
        window: &crate::window::Window,
        segment: &LineSegment,
        origin_x: usize,
        origin_y: usize,
        content_width: usize,
    ) -> bool {
        let Some(block) = &segment.block else {
            return false;
        };
        let editor_style = self.theme.style.clone();
        buffer.fill_rect(
            origin_x,
            origin_y,
            content_width,
            1,
            ' ',
            &editor_style,
            &self.theme,
        );
        let skip = if window.wrap { 0 } else { window.vleft };
        let mut seen = 0usize;
        let mut x = origin_x;
        let limit = origin_x.saturating_add(content_width);
        for span in block.spans.iter() {
            if x >= limit {
                break;
            }
            let width = display_width(&span.text);
            if seen.saturating_add(width) <= skip {
                seen += width;
                continue;
            }
            let text = slice_block_text(
                &span.text,
                skip.saturating_sub(seen),
                limit.saturating_sub(x),
            );
            if !text.is_empty() {
                let style = block_role_style(&self.theme, span.role);
                buffer.set_text(x, origin_y, &text, &style);
                x += display_width(&text);
            }
            seen += width;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::{
        buffer::Buffer,
        config::Config,
        editor::{
            display_layout::{layout_lines_with_tables, BreakIndentOptions, LayoutConfig},
            render_buffer::RenderBuffer,
            Action, Editor,
        },
        test_utils::EditorTestExt,
        theme::{parse_vscode_theme, Theme},
    };

    fn rows(text: &[&str]) -> Vec<BlockRow> {
        text.iter().copied().map(BlockRow::plain).collect()
    }

    fn config(height: usize, wrap: bool, vtop: usize, skipcol: usize) -> LayoutConfig {
        LayoutConfig {
            content_width: 20,
            height,
            wrap,
            vtop,
            vleft: 0,
            skipcol,
            break_indent: BreakIndentOptions::disabled(),
        }
    }

    fn source(prefix: usize, block_lines: usize) -> (Vec<String>, usize) {
        let mut lines = Vec::new();
        for index in 0..prefix {
            lines.push(format!("line-{index}\n"));
        }
        for index in 0..block_lines {
            lines.push(format!("hidden-{index}\n"));
        }
        lines.push("after\n".to_string());
        let count = lines.len();
        (lines, count)
    }

    #[test]
    fn reserves_rows_and_collapses_them_when_the_range_is_revealed() {
        let (lines, count) = source(1, 3);
        let specs = [BlockSpec::new(1..4, rows(&["ALPHA", "BETA", "GAMMA"]))];
        let hidden = layout_blocks(&specs, None, 20, false);
        let layout = layout_lines_with_tables(
            &lines,
            count,
            config(10, true, 0, 0),
            None,
            None,
            None,
            None,
            None,
            Some(&hidden),
        );
        let block_rows: Vec<_> = layout
            .rows
            .iter()
            .filter(|row| row.block.is_some())
            .collect();
        assert_eq!(block_rows.len(), 3);
        assert!(block_rows.iter().all(|row| row.line == 1));
        assert_eq!(block_rows[0].block.as_ref().unwrap().text(), "ALPHA");
        assert!(layout.rows.iter().all(|row| row.line != 2 && row.line != 3));
        assert_eq!(layout.rows.last().unwrap().line, 4);
        assert!(layout.rows.iter().any(|row| row.line == 0));

        let revealed = layout_blocks(&specs, Some(&(1..2)), 20, false);
        assert!(revealed.is_empty());
        let layout = layout_lines_with_tables(
            &lines,
            count,
            config(10, true, 0, 0),
            None,
            None,
            None,
            None,
            None,
            Some(&revealed),
        );
        assert!(layout.rows.iter().all(|row| row.block.is_none()));
        assert_eq!(
            layout.rows.iter().map(|row| row.line).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4]
        );
    }

    #[test]
    fn scrolls_inside_a_tall_block_with_skipcol() {
        let lines = vec!["anchor\n".to_string(), "tail\n".to_string()];
        let specs = [BlockSpec::new(0..1, rows(&["r0", "r1", "r2", "r3", "r4"]))];
        let plan = layout_blocks(&specs, None, 20, false);
        let scrolled = layout_lines_with_tables(
            &lines,
            2,
            config(10, true, 0, 2),
            None,
            None,
            None,
            None,
            None,
            Some(&plan),
        );
        let block_rows: Vec<_> = scrolled.rows.iter().filter(|row| row.line == 0).collect();
        assert_eq!(block_rows.len(), 3);
        assert_eq!(block_rows[0].start_col, 2);
        assert_eq!(block_rows[0].block.as_ref().unwrap().text(), "r2");
        assert_eq!(scrolled.rows.last().unwrap().line, 1);
    }

    #[test]
    fn places_a_block_at_the_top_and_clips_one_at_the_bottom() {
        let top_lines = vec!["only\n".to_string(), "next\n".to_string()];
        let top = layout_blocks(
            &[BlockSpec::new(0..1, rows(&["TOP-A", "TOP-B"]))],
            None,
            20,
            false,
        );
        let top_layout = layout_lines_with_tables(
            &top_lines,
            2,
            config(6, false, 0, 0),
            None,
            None,
            None,
            None,
            None,
            Some(&top),
        );
        assert!(top_layout.rows[0].block.is_some());
        assert_eq!(top_layout.rows[0].row, 0);
        assert_eq!(top_layout.rows[0].line, 0);

        let (lines, count) = source(2, 2);
        let bottom = layout_blocks(
            &[BlockSpec::new(2..4, rows(&["b0", "b1", "b2", "b3"]))],
            None,
            20,
            false,
        );
        let clipped = layout_lines_with_tables(
            &lines,
            count,
            config(4, false, 0, 0),
            None,
            None,
            None,
            None,
            None,
            Some(&bottom),
        );
        assert_eq!(clipped.rows.len(), 4);
        assert!(clipped.rows[0].block.is_none() && clipped.rows[1].block.is_none());
        assert!(clipped.rows[2].block.is_some() && clipped.rows[3].block.is_some());
        assert_eq!(clipped.rows[2].block.as_ref().unwrap().text(), "b0");
        assert_eq!(clipped.rows[3].block.as_ref().unwrap().text(), "b1");
        assert!(clipped.rows.iter().all(|row| row.line != 3));
    }

    #[test]
    fn wrap_changes_source_rows_and_leaves_the_block_intact() {
        let long = "a".repeat(40);
        let lines = vec![
            format!("{long}\n"),
            "inside\n".to_string(),
            "after\n".to_string(),
        ];
        let specs = [BlockSpec::new(1..2, rows(&["BLOCK"]))];
        let plan = layout_blocks(&specs, None, 20, false);
        let wrapped = layout_lines_with_tables(
            &lines,
            3,
            config(12, true, 0, 0),
            None,
            None,
            None,
            None,
            None,
            Some(&plan),
        );
        let plain = layout_lines_with_tables(
            &lines,
            3,
            config(12, false, 0, 0),
            None,
            None,
            None,
            None,
            None,
            Some(&plan),
        );
        let wrapped_source = wrapped.rows.iter().filter(|row| row.line == 0).count();
        let plain_source = plain.rows.iter().filter(|row| row.line == 0).count();
        assert!(wrapped_source > 1, "wrap should split the long line");
        assert_eq!(plain_source, 1);
        assert!(wrapped.rows.iter().any(|row| row.block.is_some()));
        assert!(plain.rows.iter().any(|row| row.block.is_some()));
        assert!(wrapped
            .rows
            .iter()
            .all(|row| row.line != 1 || row.block.is_some()));
    }

    #[test]
    fn role_colors_resolve_in_dark_and_light_themes() {
        let dark = parse_vscode_theme(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("themes/red.json")
                .to_str()
                .unwrap(),
        )
        .unwrap();
        let light = parse_vscode_theme(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("themes/github-light.json")
                .to_str()
                .unwrap(),
        )
        .unwrap();
        for theme in [&dark, &light] {
            for role in [
                BlockRole::Plain,
                BlockRole::Muted,
                BlockRole::Border,
                BlockRole::Guide,
                BlockRole::Key,
                BlockRole::Value,
                BlockRole::Edge,
            ] {
                let style = block_role_style(theme, role);
                assert!(style.fg.is_some(), "{role:?} needs a foreground");
                assert_ne!(style.fg, theme.style.bg);
            }
        }
        assert_ne!(
            block_role_style(&dark, BlockRole::Border).fg,
            block_role_style(&light, BlockRole::Border).fg
        );
    }

    fn markdown_editor(source: &str, width: usize, height: usize) -> Editor {
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
            vec![Buffer::new(Some("notes.md".into()), source.into())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor
    }

    #[tokio::test]
    async fn motion_skips_a_rendered_block_and_writes_no_undo_entry() {
        let source = "above\nhidden-a\nhidden-b\nbelow\n";
        let mut editor = markdown_editor(source, 40, 16);
        editor.test_set_markdown_blocks(vec![BlockSpec::new(1..3, rows(&["ONE", "TWO"]))]);
        let before = editor.test_buffer_contents();
        editor.test_execute_action(Action::MoveDown).await.unwrap();
        assert_eq!(
            editor.test_buffer_line(),
            3,
            "j lands on the line after the block"
        );
        assert_eq!(editor.test_buffer_contents(), before);
        assert!(editor.test_undo_tree().is_empty());

        editor.test_execute_action(Action::MoveUp).await.unwrap();
        assert_eq!(editor.test_buffer_line(), 0);

        editor.cy = 1;
        editor.vtop = 0;
        let window = editor.active_window_with_editor_view().unwrap();
        let revealed = editor.layout_for_window(&window);
        assert!(revealed.rows.iter().all(|row| row.block.is_none()));
        assert!(revealed.rows.iter().any(|row| row.line == 1));
        assert!(revealed.rows.iter().any(|row| row.line == 2));
        assert!(editor.test_undo_tree().is_empty());

        let mut frame = RenderBuffer::new(40, 16, &Theme::default().style);
        editor.cy = 0;
        editor.render(&mut frame).unwrap();
        let screen = frame
            .cells
            .chunks(frame.width)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("ONE"), "{screen}");
        assert!(screen.contains("above"), "{screen}");
        assert!(!screen.contains("hidden-a"), "{screen}");
    }

    #[test]
    fn a_collapsed_block_keeps_the_following_line_on_screen() {
        let mut source = String::from("title\n");
        for index in 0..40 {
            source.push_str(&format!("hidden-{index}\n"));
        }
        source.push_str("visible-after\n");
        let mut editor = markdown_editor(&source, 48, 16);
        editor.test_set_markdown_blocks(vec![BlockSpec::new(1..41, rows(&["BLOCK"]))]);
        let mut frame = RenderBuffer::new(48, 16, &Theme::default().style);
        editor.render(&mut frame).unwrap();
        let screen = frame
            .cells
            .chunks(frame.width)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("BLOCK"), "{screen}");
        assert!(screen.contains("visible-after"), "{screen}");
        assert!(!screen.contains("hidden-0"), "{screen}");
    }

    #[test]
    fn sideways_scroll_crops_a_block_row() {
        let mut editor = markdown_editor("above\nblock-line\n", 40, 10);
        editor.test_set_markdown_blocks(vec![BlockSpec::new(
            1..2,
            rows(&["ABCDEFGHIJKLMNOPQRSTUVWXYZ"]),
        )]);
        let mut window = editor.active_window_with_editor_view().unwrap();
        window.wrap = false;
        window.vleft = 4;
        let layout = editor.layout_for_window(&window);
        let segment = layout
            .rows
            .iter()
            .find(|row| row.block.is_some())
            .expect("block row");
        let mut frame = RenderBuffer::new(20, 1, &Theme::default().style);
        assert!(editor.paint_markdown_block_segment(&mut frame, &window, segment, 0, 0, 20));
        let screen = frame
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>();
        assert!(screen.contains("EFGHIJ"), "{screen}");
        assert!(!screen.contains("ABCD"), "{screen}");
    }
}
