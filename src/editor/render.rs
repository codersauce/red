use crate::theme::Style;

#[derive(Debug, Clone)]
pub struct RenderBuffer {
    pub cells: Vec<Cell>,
    pub width: usize,
    #[allow(unused)]
    pub height: usize,
}

impl RenderBuffer {
    #[allow(unused)]
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

    pub fn new(width: usize, height: usize, default_style: Style) -> Self {
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

    pub fn set_char(&mut self, x: usize, y: usize, c: char, style: &Style) {
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

    pub fn set_text(&mut self, x: usize, y: usize, text: &str, style: &Style) {
        let pos = (y * self.width) + x;
        for (i, c) in text.chars().enumerate() {
            self.cells[pos + i] = Cell {
                c,
                style: style.clone(),
            }
        }
    }

    pub fn diff(&self, other: &RenderBuffer) -> Vec<Change> {
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

    pub fn dump(&self) -> String {
        let mut s = String::new();
        for (i, cell) in self.cells.iter().enumerate() {
            if i > 0 && i % self.width == 0 {
                s.push('\n');
            }
            if cell.c == ' ' {
                // pushes a unicode dot if space
                s.push(' ');
            } else {
                s.push(cell.c);
            }
        }

        format!("{s}\n")
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyleInfo {
    pub start: usize,
    pub end: usize,
    pub style: Style,
}

impl StyleInfo {
    pub fn contains(&self, pos: usize) -> bool {
        pos >= self.start && pos < self.end
    }
}
