use crate::log;

pub struct Buffer {
    pub file: Option<String>,
    pub lines: Vec<String>,
}

impl Buffer {
    pub fn from_file(file: Option<String>) -> Self {
        let lines = match &file {
            Some(file) => std::fs::read_to_string(file)
                .unwrap()
                .lines()
                .map(|s| s.to_string())
                .collect(),
            None => vec![],
        };

        Self { file, lines }
    }

    pub fn get(&self, line: usize) -> Option<String> {
        if self.lines.len() > line {
            return Some(self.lines[line].clone());
        }

        None
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn insert(&mut self, x: u16, y: usize, c: char) {
        if let Some(line) = self.lines.get_mut(y) {
            (*line).insert(x as usize, c);
        }
    }

    pub fn remove(&mut self, x: u16, y: usize) {
        if let Some(line) = self.lines.get_mut(y) {
            (*line).remove(x as usize);
        }
    }

    pub fn insert_line(&mut self, y: usize, content: String) {
        self.lines.insert(y, content);
    }

    pub fn remove_line(&mut self, line: usize) {
        if self.len() > line {
            self.lines.remove(line);
        }
    }
}
