use std::{
    path::Path,
    sync::{Arc, RwLock},
};

use path_absolutize::Absolutize;

use crate::{
    editor::Action,
    log,
    lsp::{Diagnostic, TextDocumentPublishDiagnostics},
};

#[derive(Debug)]
pub struct SharedBuffer(Arc<RwLock<Buffer>>);

impl SharedBuffer {
    pub fn new(buffer: Buffer) -> Self {
        Self(Arc::new(RwLock::new(buffer)))
    }

    pub fn name(&self) -> String {
        self.0.read().unwrap().name().to_string()
    }

    pub fn is_dirty(&self) -> bool {
        self.0.read().unwrap().is_dirty()
    }

    pub fn len(&self) -> usize {
        self.0.read().unwrap().len()
    }

    pub fn get(&self, line: usize) -> Option<String> {
        self.0.read().unwrap().get(line)
    }

    pub fn remove_line(&self, line: usize) {
        let contents = self.get(line);
        self.0.write().unwrap().remove_line(line);
    }

    pub fn lock(&self) -> anyhow::Result<std::sync::RwLockWriteGuard<Buffer>> {
        self.0
            .write()
            .map_err(|e| anyhow::anyhow!("lock failed: {:?}", e))
    }

    pub fn lock_read(&self) -> anyhow::Result<std::sync::RwLockReadGuard<Buffer>> {
        self.0
            .read()
            .map_err(|e| anyhow::anyhow!("lock failed: {:?}", e))
    }
}

impl Eq for SharedBuffer {}

impl PartialEq for SharedBuffer {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Clone for SharedBuffer {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl From<Buffer> for SharedBuffer {
    fn from(buffer: Buffer) -> Self {
        Self::new(buffer)
    }
}

#[derive(Debug)]
pub struct Buffer {
    pub file: Option<String>,
    pub lines: Vec<String>,
    pub dirty: bool,
    pub diagnostics: Vec<Diagnostic>,
    pub pos: (usize, usize),
    pub vtop: usize,
    pub undo_actions: Vec<Action>,
}

impl Buffer {
    pub fn new(file: Option<String>, contents: String) -> Self {
        let has_newline_at_end = contents.ends_with("\n");
        let mut lines = contents.lines().map(|s| s.to_string()).collect::<Vec<_>>();
        if has_newline_at_end {
            lines.push("".to_string());
        }
        Self {
            file,
            lines,
            dirty: false,
            diagnostics: vec![],
            pos: (0, 0),
            vtop: 0,
            undo_actions: vec![],
        }
    }

    #[allow(unused)]
    pub fn with_lines(file: Option<String>, lines: Vec<String>) -> Self {
        Self {
            file,
            lines,
            dirty: false,
            diagnostics: vec![],
            pos: (0, 0),
            vtop: 0,
            undo_actions: vec![],
        }
    }

    pub fn from_file(file: Option<String>) -> anyhow::Result<Self> {
        match &file {
            Some(file) => {
                let path = Path::new(file);
                if !path.exists() {
                    return Err(anyhow::anyhow!("file {:?} not found", file));
                }
                let contents = std::fs::read_to_string(file)?;
                // TODO: lsp.did_open(file, &contents).await?;
                Ok(Self::new(Some(file.to_string()), contents.to_string()))
            }
            None => Ok(Self::new(file, "\n".to_string())),
        }
    }

    pub fn contents(&self) -> String {
        self.lines.join("\n")
    }

    pub fn save(&mut self) -> anyhow::Result<String> {
        if let Some(file) = &self.file {
            let contents = self.contents();
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

    pub fn name(&self) -> &str {
        self.file.as_deref().unwrap_or("[No Name]")
    }

    pub fn uri(&self) -> anyhow::Result<Option<String>> {
        let Some(file) = &self.file else {
            return Ok(None);
        };
        Ok(Some(format!(
            "file://{}",
            Path::new(&file).absolutize()?.to_string_lossy().to_string()
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
            log!("offered: {offered_uri} but we are {uri}");
            if &uri != offered_uri {
                return Ok(());
            }
        }

        self.diagnostics = msg
            .diagnostics
            .iter()
            .filter(|d| d.is_for(&uri))
            .map(|d| d.clone())
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
            (*line).insert(x as usize, c);
            self.dirty = true;
        }
    }

    /// removes a character from the buffer
    pub fn remove(&mut self, x: usize, y: usize) {
        if let Some(line) = self.lines.get_mut(y) {
            (*line).remove(x as usize);
            self.dirty = true;
        }
    }

    pub fn insert_line(&mut self, y: usize, content: String) {
        self.lines.insert(y, content);
        self.dirty = true;
    }

    pub fn remove_line(&mut self, line: usize) {
        if self.len() > line {
            self.undo_actions
                .push(Action::InsertLineAt(line, self.get(line)));
            self.lines.remove(line);
            self.dirty = true;
        }
    }

    pub fn replace_line(&mut self, line: usize, new_line: String) {
        if line <= self.len() {
            self.lines[line] = new_line;
        }
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
        self.lines[y] = format!("{}{}", line[..start.0].to_string(), &rest);
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
