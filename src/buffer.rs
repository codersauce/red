use std::path::Path;

use path_absolutize::Absolutize;

use crate::{
    log,
    lsp::{Diagnostic, LspClient, TextDocumentPublishDiagnostics},
};

#[derive(Debug)]
pub struct Buffer {
    pub file: Option<String>,
    pub lines: Vec<String>,
    pub dirty: bool,
    pub diagnostics: Vec<Diagnostic>,
    pub pos: (usize, usize),
    pub vtop: usize,

    // TODO: very hacky, we need to revisit this once we use a better underlying representation for
    // the buffer (and not a Vec<String>)
    pub has_newline_at_end: bool,
}

impl Buffer {
    pub fn new(file: Option<String>, contents: String) -> Self {
        let has_newline_at_end = contents.ends_with("\n");
        let lines = contents.lines().map(|s| s.to_string()).collect();
        Self {
            file,
            lines,
            dirty: false,
            diagnostics: vec![],
            pos: (0, 0),
            vtop: 0,
            has_newline_at_end,
        }
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

    pub fn contents(&self) -> String {
        let mut contents = self.lines.join("\n");
        if self.has_newline_at_end {
            contents += "\n";
        }
        contents
    }

    pub fn save(&mut self) -> anyhow::Result<String> {
        if let Some(file) = &self.file {
            let mut contents = self.lines.join("\n");
            if self.has_newline_at_end {
                contents += "\n";
            }
            std::fs::write(file, &contents)?;
            self.dirty = false;
            let message = format!(
                "{:?} {}L, {}B written",
                file,
                self.lines.len(),
                contents.as_bytes().len()
            );
            Ok(message)
        } else {
            Err(anyhow::anyhow!("No file name"))
        }
    }

    pub fn save_as(&mut self, new_file_name: &str) -> anyhow::Result<String> {
        let mut contents = self.lines.join("\n");
        if self.has_newline_at_end {
            contents += "\n";
        }
        std::fs::write(new_file_name, &contents)?;
        self.dirty = false;
        self.file = Some(new_file_name.to_string());
        let message = format!(
            "{:?} {}L, {}B written",
            new_file_name,
            self.lines.len(),
            contents.as_bytes().len()
        );
        Ok(message)
    }

    pub fn name(&self) -> &str {
        self.file.as_deref().unwrap_or("[No Name]")
    }

    pub fn uri(&self) -> anyhow::Result<Option<String>> {
        let Some(file) = &self.file else {
            return Ok(None);
        };
        Ok(Some(format!(
            "file://{}",
            Path::new(&file).absolutize()?.to_string_lossy()
        )))
    }

    pub fn offer_diagnostics(
        &mut self,
        msg: &TextDocumentPublishDiagnostics,
    ) -> anyhow::Result<()> {
        let Some(uri) = self.uri()? else {
            return Ok(());
        };

        if let Some(offered_uri) = &msg.uri {
            log!("offered: {offered_uri} and we are {uri}");
            if &uri != offered_uri {
                log!("skipping");
                return Ok(());
            }
        }

        self.diagnostics = msg
            .diagnostics
            .iter()
            .filter(|d| d.is_for(&uri))
            .cloned()
            .collect::<Vec<_>>();

        Ok(())
    }

    pub fn diagnostics_for_lines(
        &self,
        starting_line: usize,
        ending_line: usize,
    ) -> Vec<&Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| {
                let start = &d.range.start;
                start.line >= starting_line && start.line < ending_line
            })
            .collect::<Vec<_>>()
    }

    pub fn get(&self, line: usize) -> Option<String> {
        if self.lines.len() > line {
            return Some(self.lines[line].clone());
        }

        None
    }

    pub fn set(&mut self, line: usize, content: String) {
        if self.lines.len() > line {
            self.lines[line] = content;
            self.dirty = true;
        }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn insert_str(&mut self, x: usize, y: usize, s: &str) {
        s.chars().enumerate().for_each(|(i, c)| {
            self.insert(x + i, y, c);
        });
        self.dirty = true;
    }

    pub fn insert(&mut self, x: usize, y: usize, c: char) {
        if let Some(line) = self.lines.get_mut(y) {
            (*line).insert(x, c);
            self.dirty = true;
        }
    }

    /// removes a character from the buffer
    pub fn remove(&mut self, x: usize, y: usize) {
        if let Some(line) = self.lines.get_mut(y) {
            (*line).remove(x);
            self.dirty = true;
        }
    }

    #[allow(unused)]
    pub fn remove_range(&mut self, y: usize, start: usize, end: usize) {
        if let Some(line) = self.lines.get_mut(y) {
            (*line).replace_range(start..=end, "");
            self.dirty = true;
        }
    }

    pub fn insert_line(&mut self, y: usize, content: String) {
        self.lines.insert(y, content);
        self.dirty = true;
    }

    pub fn remove_line(&mut self, line: usize) {
        if self.len() > line {
            self.lines.remove(line);
            self.dirty = true;
        }
    }

    pub fn replace_line(&mut self, line: usize, new_line: String) {
        if line <= self.len() {
            self.lines[line] = new_line;
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
        let chars = line.chars().skip(x);
        for c in chars {
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
        let chars = line.chars().rev().skip(line.len() - x);

        for c in chars {
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
            for c in line.chars() {
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

    pub fn find_next(&self, query: &str, (x, y): (usize, usize)) -> Option<(usize, usize)> {
        let (mut x, mut y) = self.find_word_end((x, y))?;

        loop {
            if y >= self.len() {
                return None;
            }

            let line = self.get(y)?;
            if let Some(pos) = line[x..].find(query) {
                return Some((pos + x, y));
            }

            x = 0;
            y += 1;
        }
    }

    pub fn find_prev(&self, query: &str, (x, y): (usize, usize)) -> Option<(usize, usize)> {
        let (mut x, mut y) = self.find_word_start((x, y))?;

        loop {
            if y >= self.len() {
                return None;
            }

            let line = self.get(y)?;
            if let Some(pos) = line[..x].rfind(query) {
                return Some((pos, y));
            }

            if y == 0 {
                return None;
            }

            y -= 1;
            x = self.get(y)?.len();
        }
    }

    pub fn delete_word(&mut self, (x, y): (usize, usize)) {
        let start = self.find_word_start((x, y)).unwrap();
        let end = self.find_word_end((x, y)).unwrap();
        log!("deleting word from {:?} to {:?}", start, end);
        let line = self.get(y).unwrap();
        let rest = line[end.0..].to_string();
        self.lines[y] = format!("{}{}", &line[..start.0], &rest);
        self.dirty = true;
    }

    #[allow(unused)]
    pub fn delete_to_next_word(&mut self, (x, y): (usize, usize)) {
        let (fx, fy) = self.find_word_end((x, y)).unwrap();
        let line = self.get(y).unwrap();
        let rest = line[x..].to_string();
        self.lines[y] = line[..x].to_string();
        self.lines.insert(y + 1, rest);
        self.dirty = true;
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
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
