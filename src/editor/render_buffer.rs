use crate::{
    color::{blend_color, Color},
    log,
    theme::{SelectionForegroundPriority, Style, Theme},
    unicode_utils::display_width,
};
use unicode_segmentation::UnicodeSegmentation;

use super::Point;

#[derive(Debug)]
pub struct Change<'a> {
    pub x: usize,
    pub y: usize,
    pub cell: &'a Cell,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub c: char,
    pub text: String,
    pub style: Style,
}

impl Cell {
    fn new(c: char, style: Style) -> Self {
        Self {
            c,
            text: c.to_string(),
            style,
        }
    }

    fn from_grapheme(grapheme: &str, style: Style) -> Self {
        Self {
            c: grapheme.chars().next().unwrap_or(' '),
            text: grapheme.to_string(),
            style,
        }
    }

    /// In-place assignment that reuses the cell's `text` allocation. These
    /// run thousands of times per frame, so avoiding a fresh `String` per
    /// cell matters.
    fn set_grapheme(&mut self, grapheme: &str, style: &Style) {
        self.c = grapheme.chars().next().unwrap_or(' ');
        self.text.clear();
        self.text.push_str(grapheme);
        self.style = style.clone();
    }

    fn set_char_in_place(&mut self, c: char, style: &Style) {
        self.c = c;
        self.text.clear();
        self.text.push(c);
        self.style = style.clone();
    }
}

#[derive(Debug, Clone)]
pub struct RenderBuffer {
    pub cells: Vec<Cell>,
    pub width: usize,
    #[allow(unused)]
    pub height: usize,
}

impl RenderBuffer {
    pub fn new(width: usize, height: usize, default_style: &Style) -> Self {
        let cells = vec![Cell::new(' ', default_style.clone()); width * height];

        RenderBuffer {
            cells,
            width,
            height,
        }
    }

    pub fn new_with_contents(
        width: usize,
        height: usize,
        style: Style,
        contents: Vec<String>,
    ) -> Self {
        let mut cells = vec![];

        for line in contents {
            for grapheme in line.graphemes(true) {
                let grapheme_width = display_width(grapheme);
                if grapheme_width == 0 {
                    continue;
                }
                cells.push(Cell::from_grapheme(grapheme, style.clone()));
                for _ in 1..grapheme_width {
                    cells.push(Cell::new(' ', style.clone()));
                }
            }
            for _ in 0..width.saturating_sub(display_width(&line)) {
                cells.push(Cell::new(' ', style.clone()));
            }
        }

        RenderBuffer {
            cells,
            width,
            height,
        }
    }

    /// Clears the buffer with the given style
    pub fn clear(&mut self) {
        self.cells = vec![Cell::new(' ', Style::default()); self.width * self.height];
    }

    pub fn write_string(
        &mut self,
        x: usize,
        y: usize,
        text: &str,
        color: Option<Color>,
    ) -> anyhow::Result<()> {
        let style = Style {
            fg: color,
            bg: None,
            bold: false,
            italic: false,
        };
        self.set_text(x, y, text, &style);
        Ok(())
    }

    pub fn _set_char(&mut self, x: usize, y: usize, c: char, style: &Style) {
        if x >= self.width || y >= self.height {
            return;
        }
        let pos = (y * self.width) + x;
        if pos >= self.cells.len() {
            return;
        }
        self.cells[pos] = Cell::new(c, style.clone());
    }

    pub fn set_bg_for_points(&mut self, points: Vec<Point>, bg: &Color, theme: &Theme) {
        for point in points {
            self.set_bg(point.x, point.y, bg, theme);
        }
    }

    pub(crate) fn apply_selection_for_points(
        &mut self,
        points: Vec<Point>,
        selection: &Style,
        theme: &Theme,
        foreground_priority: SelectionForegroundPriority,
    ) {
        for point in points {
            if point.x >= self.width || point.y >= self.height {
                continue;
            }
            let position = point.y * self.width + point.x;
            let Some(cell) = self.cells.get_mut(position) else {
                continue;
            };
            cell.style = theme.selected_style(&cell.style, selection, foreground_priority);
        }
    }

    pub fn set_bg_for_range(&mut self, start: Point, end: Point, bg: &Color, theme: &Theme) {
        for y in start.y..=end.y {
            for x in start.x..=end.x {
                self.set_bg(x, y, bg, theme);
            }
        }
    }

    pub fn set_bg(&mut self, x: usize, y: usize, bg: &Color, theme: &Theme) {
        if x >= self.width || y >= self.height {
            return;
        }
        let pos = (y * self.width) + x;
        if pos >= self.cells.len() {
            return;
        }

        // Blend RGBA colors with the background if necessary
        let bg = match bg {
            Color::Rgba { r, g, b, a } => blend_color(
                Color::Rgba {
                    r: *r,
                    g: *g,
                    b: *b,
                    a: *a,
                },
                theme.style.bg.unwrap_or(Color::Rgb { r: 0, g: 0, b: 0 }),
            ),
            _ => *bg,
        };

        self.cells[pos].style.bg = Some(bg);
    }

    pub fn set_char(&mut self, x: usize, y: usize, c: char, style: &Style, theme: &Theme) {
        if x >= self.width || y >= self.height {
            return;
        }
        let pos = (y * self.width) + x;
        if pos >= self.cells.len() {
            return;
        }

        // Blend RGBA colors with the background if necessary
        let bg = style.bg.map(|color| match color {
            Color::Rgba { r, g, b, a } => blend_color(
                Color::Rgba { r, g, b, a },
                theme.style.bg.unwrap_or(Color::Rgb { r: 0, g: 0, b: 0 }),
            ),
            _ => color,
        });

        self.cells[pos].set_char_in_place(
            c,
            &Style {
                fg: style.fg,
                bg,
                bold: style.bold,
                italic: style.italic,
            },
        );
    }

    pub fn set_text(&mut self, x: usize, y: usize, text: &str, style: &Style) {
        if x >= self.width || y >= self.height {
            return;
        }

        let mut cell_x = x;
        for grapheme in text.graphemes(true) {
            if cell_x >= self.width {
                break;
            }

            let grapheme_width = display_width(grapheme);
            if grapheme_width == 0 {
                continue;
            }

            let pos = (y * self.width) + cell_x;
            if pos >= self.cells.len() {
                log!("WARN: pos >= self.cells.len()");
                break;
            }
            self.cells[pos].set_grapheme(grapheme, style);

            for offset in 1..grapheme_width {
                let pad_x = cell_x + offset;
                if pad_x >= self.width {
                    break;
                }
                let pad_pos = (y * self.width) + pad_x;
                if pad_pos >= self.cells.len() {
                    log!("WARN: pad_pos >= self.cells.len()");
                    break;
                }
                self.cells[pad_pos].set_char_in_place(' ', style);
            }

            cell_x += grapheme_width;
        }
    }

    pub fn dump_diff(&self, changes: &[Change]) -> String {
        let mut s = String::new();

        for y in 0..self.height {
            for x in 0..self.width {
                if let Some(change) = changes.iter().find(|c| c.x == x && c.y == y) {
                    s.push_str(&change.cell.text);
                } else {
                    s.push('·');
                }
            }
            s.push('\n');
        }

        s
    }

    pub fn diff(&self, other: &RenderBuffer) -> Vec<Change<'_>> {
        let mut changes = vec![];
        for (pos, cell) in self.cells.iter().enumerate() {
            if *cell != other.cells[pos] {
                let y = pos / self.width;
                let x = pos % self.width;

                changes.push(Change { x, y, cell });
            }
        }

        changes
    }

    pub fn snapshot_rows(&self, rows: &[usize]) -> Vec<(usize, Vec<Cell>)> {
        let mut snapshots = Vec::with_capacity(rows.len());
        let mut seen = Vec::with_capacity(rows.len());

        for &row in rows {
            if row >= self.height || seen.contains(&row) {
                continue;
            }
            seen.push(row);

            let start = row * self.width;
            let end = start + self.width;
            snapshots.push((row, self.cells[start..end].to_vec()));
        }

        snapshots
    }

    pub fn diff_row_snapshots(&self, snapshots: &[(usize, Vec<Cell>)]) -> Vec<Change<'_>> {
        let mut changes = Vec::new();

        for (row, old_cells) in snapshots {
            if *row >= self.height {
                continue;
            }
            let start = row * self.width;
            for (x, old_cell) in old_cells.iter().enumerate().take(self.width) {
                let pos = start + x;
                if pos >= self.cells.len() {
                    break;
                }
                let cell = &self.cells[pos];
                if cell != old_cell {
                    changes.push(Change { x, y: *row, cell });
                }
            }
        }

        changes
    }

    pub fn dump(&self, show_style_changes: bool) -> String {
        let mut s = String::new();
        let mut current_syle = None;
        for (i, cell) in self.cells.iter().enumerate() {
            if i % self.width == 0 {
                s.push('\n');
            }
            if cell.text == " " {
                // pushes a unicode dot if space
                s.push('·');
            } else if show_style_changes {
                if let Some(ref style) = current_syle {
                    if *style != cell.style {
                        s.push('|');
                        current_syle = Some(cell.style.clone());
                    } else {
                        s.push_str(&cell.text);
                    }
                } else {
                    s.push_str(&cell.text);
                    current_syle = Some(cell.style.clone());
                }
            } else {
                s.push_str(&cell.text);
            }
        }

        s
    }

    /// Applies a frame diff while retaining the allocations owned by this
    /// buffer. Only changed cells are cloned into the previous-frame buffer.
    pub(crate) fn apply_changes(&mut self, changes: &[Change<'_>]) {
        for change in changes {
            let pos = (change.y * self.width) + change.x;
            self.cells[pos] = change.cell.clone();
        }
    }
}
