use std::path::Path;

use crate::{log, lsp::LspClient};

#[derive(Debug)]
pub struct Buffer {
    pub file: Option<String>,
    pub lines: Vec<String>,
}

impl Buffer {
    pub fn new(file: Option<String>, contents: String) -> Self {
        let lines = contents.lines().map(|s| s.to_string()).collect();
        Self { file, lines }
    }

    pub async fn from_file(lsp: &mut LspClient, file: Option<String>) -> anyhow::Result<Self> {
        match &file {
            Some(file) => {
                let path = Path::new(file);
                if !path.exists() {
                    return Err(anyhow::anyhow!("file {:?} not found", file));
                }
                let contents = std::fs::read_to_string(file)?;
                lsp.did_open(file, &contents).await?;
                Ok(Self::new(Some(file.to_string()), contents.to_string()))
            }
            None => Ok(Self::new(file, "\n".to_string())),
        }
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

    pub fn insert_str(&mut self, x: usize, y: usize, s: &str) {
        s.chars().enumerate().for_each(|(i, c)| {
            self.insert(x + i, y, c);
        });
    }

    pub fn insert(&mut self, x: usize, y: usize, c: char) {
        if let Some(line) = self.lines.get_mut(y) {
            (*line).insert(x as usize, c);
        }
    }

    /// removes a character from the buffer
    pub fn remove(&mut self, x: usize, y: usize) {
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

    pub fn viewport(&self, vtop: usize, vheight: usize) -> String {
        let height = std::cmp::min(vtop + vheight, self.lines.len());
        self.lines[vtop..height].join("\n")
    }

    pub fn is_in_word(&self, (x, y): (usize, usize)) -> bool {
        let line = self.get(y).unwrap();
        if x >= line.len() {
            return false;
        }

        let c = line.chars().nth(x).unwrap();
        c.is_alphanumeric() || c == '_'
    }

    pub fn find_word_end(&self, (x, y): (usize, usize)) -> Option<(usize, usize)> {
        let line = self.get(y)?;
        let mut x = x;
        let mut chars = line.chars().skip(x);
        while let Some(c) = chars.next() {
            if x >= line.len() {
                return Some((x, y));
            }

            if !c.is_alphanumeric() && c != '_' {
                return Some((x, y));
            }

            x += 1;
        }
        None
    }

    pub fn find_word_start(&self, (x, y): (usize, usize)) -> Option<(usize, usize)> {
        let line = self.get(y)?;
        let mut x = x;
        let mut chars = line.chars().rev().skip(line.len() - x);

        while let Some(c) = chars.next() {
            if x == 0 {
                return Some((x, y));
            }

            if !c.is_alphanumeric() && c != '_' {
                return Some((x, y));
            }

            x -= 1;
        }
        Some((x, y))
    }

    pub fn find_next_word(&self, (x, y): (usize, usize)) -> Option<(usize, usize)> {
        let (mut x, mut y) = self.find_word_end((x, y))?;
        let line = self.get(y)?;

        let mut line = line[x..].to_string();

        loop {
            let mut chars = line.chars();

            while let Some(c) = chars.next() {
                if c.is_alphanumeric() || c == '_' {
                    return Some((x, y));
                }
                x += 1;
            }

            x = 0;
            y += 1;
            if y >= self.len() {
                return None;
            }
            line = self.get(y)?;
        }
    }

    fn char_at(&self, x: usize, y: usize) -> Option<char> {
        let line = self.get(y)?;
        line.chars().nth(x)
    }

    fn pos_left_of(&self, x: usize, y: usize) -> Option<(usize, usize)> {
        let mut x = x;
        let mut y = y;

        loop {
            if x == 0 {
                if y == 0 {
                    return None;
                }
                y -= 1;
                x = self.get(y)?.len();
            }

            if x == 0 {
                continue;
            }

            x -= 1;
            if self.char_at(x, y).is_some() {
                return Some((x, y));
            }
        }
    }

    pub fn find_prev_word(&self, (x, y): (usize, usize)) -> Option<(usize, usize)> {
        let (mut x, mut y) = self.find_word_start((x, y))?;

        loop {
            if x == 0 && y == 0 {
                return None;
            }

            if let Some(pos) = self.pos_left_of(x, y) {
                x = pos.0;
                y = pos.1;
            } else {
                return None;
            }

            if self.is_in_word((x, y)) {
                log!("found word at {:?}", (x, y));
                return self.find_word_start((x, y));
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_viewport() {
        let buffer = Buffer::new(
            Some("sample".to_string()),
            "a\nb\nc\nd\n\ne\n\nf".to_string(),
        );

        assert_eq!(buffer.viewport(0, 2), "a\nb");
    }

    #[test]
    fn test_viewport_with_small_buffer() {
        let buffer = Buffer::new(Some("sample".to_string()), "a\nb".to_string());
        assert_eq!(buffer.viewport(0, 5), "a\nb");
    }

    #[test]
    fn test_is_in_word() {
        let text = "use std::{\n    collections::HashMap,\n    io::{self, Write},\n};";
        let buffer = Buffer::new(None, text.to_string());

        assert!(buffer.is_in_word((0, 0)));
        assert!(buffer.is_in_word((1, 0)));
        assert!(buffer.is_in_word((2, 0)));
        assert!(!buffer.is_in_word((3, 0)));
        assert!(!buffer.is_in_word((7, 0)));
        assert!(!buffer.is_in_word((8, 0)));
    }

    #[test]
    fn test_find_word_end() {
        let text = "use std::{\n    collections::HashMap,\n    io::{self, Write},\n};";
        let buffer = Buffer::new(None, text.to_string());

        // "use "
        //  ^ ^
        let word_end = buffer.find_word_end((0, 0));
        assert_eq!(word_end.unwrap(), (3, 0));

        // "use "
        //     ^
        let word_end = buffer.find_word_end((3, 0));
        assert_eq!(word_end.unwrap(), (3, 0));

        // "use std::{"
        //      ^ ^
        let word_end = buffer.find_word_end((4, 0));
        assert_eq!(word_end.unwrap(), (7, 0));

        // "use std::{"
        //         ^
        let word_end = buffer.find_word_end((7, 0));
        assert_eq!(word_end.unwrap(), (7, 0));
    }

    #[test]
    fn test_find_word_start() {
        let text = "use std::{\n    collections::HashMap,\n    io::{self, Write},\n};";
        let buffer = Buffer::new(None, text.to_string());

        // "use "
        //  ^
        let word_start = buffer.find_word_start((0, 0));
        assert_eq!(word_start.unwrap(), (0, 0));

        // "use "
        //  ^^
        let word_start = buffer.find_word_start((2, 0));
        assert_eq!(word_start.unwrap(), (0, 0));

        // "use "
        //  ^^
        let word_start = buffer.find_word_start((1, 0));
        assert_eq!(word_start.unwrap(), (0, 0));

        // "use "
        //     ^
        let word_start = buffer.find_word_start((3, 0));
        assert_eq!(word_start.unwrap(), (0, 0));

        // "use std::{"
        //      ^ ^
        let word_start = buffer.find_word_end((4, 0));
        assert_eq!(word_start.unwrap(), (7, 0));

        // "use std::{"
        //         ^
        let word_start = buffer.find_word_end((7, 0));
        assert_eq!(word_start.unwrap(), (7, 0));
    }

    #[test]
    fn test_word_boundaries() {
        let text = "use std::{\n    collections::HashMap,\n    io::{self, Write},\n};";
        let buffer = Buffer::new(None, text.to_string());

        let word_start = buffer.find_word_start((0, 0));
        let word_end = buffer.find_word_end((0, 0));
        assert_eq!(word_start.unwrap(), (0, 0));
        assert_eq!(word_end.unwrap(), (3, 0));
        let word = &buffer.get(0).unwrap()[word_start.unwrap().0..word_end.unwrap().0];
        assert_eq!(word, "use");
    }

    #[test]
    fn test_find_next_word() {
        let text = "use std::{\n    collections::HashMap,\n    io::{self, Write},\n};";
        let buffer = Buffer::new(None, text.to_string());

        // this is how we behave
        let next_word = buffer.find_next_word((4, 0));
        assert_eq!(next_word.unwrap(), (4, 1)); // collections

        let next_word = buffer.find_next_word((7, 0));
        assert_eq!(next_word.unwrap(), (4, 1)); // collections

        // this is how neovim behaves
        //
        // let next_word = buffer.find_next_word((4, 0));
        // assert_eq!(next_word.unwrap(), (7, 0)); // ::
        //
        // let next_word = buffer.find_next_word((7, 0));
        // assert_eq!(next_word.unwrap(), (7, 0));
    }
}
