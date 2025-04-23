use std::collections::HashSet;

use crate::{
    color::{blend_color, Color},
    log,
    theme::{Style, Theme},
};

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
    pub style: Style,
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
        let cells = vec![
            Cell {
                c: ' ',
                style: default_style.clone(),
            };
            width * height
        ];

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
            for c in line.chars() {
                cells.push(Cell {
                    c,
                    style: style.clone(),
                });
            }
            for _ in 0..width.saturating_sub(line.len()) {
                cells.push(Cell {
                    c: ' ',
                    style: style.clone(),
                });
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
        self.cells = vec![
            Cell {
                c: ' ',
                style: Style::default(),
            };
            self.width * self.height
        ];
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
        if x > self.width || y > self.height {
            return;
        }
        let pos = (y * self.width) + x;
        if pos >= self.cells.len() {
            return;
        }
        self.cells[pos] = Cell {
            c,
            style: style.clone(),
        };
    }

    pub fn set_bg_for_points(&mut self, points: Vec<Point>, bg: &Color, theme: &Theme) {
        for point in points {
            self.set_bg(point.x, point.y, bg, theme);
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
        if x > self.width || y > self.height {
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
        if x > self.width || y > self.height {
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

        self.cells[pos] = Cell {
            c,
            style: Style {
                fg: style.fg,
                bg,
                bold: style.bold,
                italic: style.italic,
            },
        };
    }

    pub fn set_text(&mut self, x: usize, y: usize, text: &str, style: &Style) {
        let pos = (y * self.width) + x;
        for (i, c) in text.chars().enumerate() {
            if x + i >= self.width {
                break;
            }
            if pos + i >= self.cells.len() {
                log!("WARN: pos + i >= self.cells.len()");
                break;
            }
            self.cells[pos + i] = Cell {
                c,
                style: style.clone(),
            }
        }
    }

    pub fn dump_diff(&self, changes: &[Change]) -> String {
        let mut s = String::new();

        for y in 0..self.height {
            for x in 0..self.width {
                if let Some(change) = changes.iter().find(|c| c.x == x && c.y == y) {
                    s.push(change.cell.c);
                } else {
                    s.push('·');
                }
            }
            s.push('\n');
        }

        s
    }

    pub fn diff(&self, other: &RenderBuffer) -> Vec<Change> {
        let mut changes = vec![];
        
        // If width or height differs, we need to compare all cells
        if self.width != other.width || self.height != other.height {
            for (pos, cell) in self.cells.iter().enumerate() {
                if pos >= other.cells.len() || *cell != other.cells[pos] {
                    let y = pos / self.width;
                    let x = pos % self.width;
                    changes.push(Change { x, y, cell });
                }
            }
            return changes;
        }
        
        // Fast path: group changes by line for better terminal rendering
        // Most terminals render more efficiently when given a series of changes on the same line
        let mut changed_lines = HashSet::new();
        
        // First scan: identify changed lines
        for (pos, cell) in self.cells.iter().enumerate() {
            if *cell != other.cells[pos] {
                let y = pos / self.width;
                changed_lines.insert(y);
            }
        }
        
        // Second scan: process changes line by line
        for &y in &changed_lines {
            // Get all changes on this line
            let start_pos = y * self.width;
            let end_pos = start_pos + self.width;
            
            for pos in start_pos..end_pos {
                if pos >= self.cells.len() || pos >= other.cells.len() {
                    continue;
                }
                
                if self.cells[pos] != other.cells[pos] {
                    let x = pos % self.width;
                    changes.push(Change { x, y, cell: &self.cells[pos] });
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
            if cell.c == ' ' {
                // pushes a unicode dot if space
                s.push('·');
            } else if show_style_changes {
                if let Some(ref style) = current_syle {
                    if *style != cell.style {
                        s.push('|');
                        current_syle = Some(cell.style.clone());
                    } else {
                        s.push(cell.c);
                    }
                } else {
                    s.push(cell.c);
                    current_syle = Some(cell.style.clone());
                }
            } else {
                s.push(cell.c);
            }
        }

        s
    }

    #[allow(unused)]
    fn apply(&mut self, diff: Vec<Change<'_>>) {
        for change in diff {
            let pos = (change.y * self.width) + change.x;
            self.cells[pos] = Cell {
                c: change.cell.c,
                style: change.cell.style.clone(),
            };
        }
    }
}
