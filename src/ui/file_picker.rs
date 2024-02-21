use std::{
    fs,
    path::{Path, PathBuf},
};

use crossterm::{
    event::{self, Event, KeyCode},
    style::Color,
};

use crate::{
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    log,
    theme::Style,
};

use super::{Component, Dialog, List};

pub struct FilePicker {
    root_path: PathBuf,
    search: String,
    width: usize,
    height: usize,
}

impl FilePicker {
    pub fn new(editor: &Editor, root_path: PathBuf) -> Self {
        let width = editor.vwidth();
        let height = editor.vheight();

        FilePicker {
            root_path,
            width,
            height,
            search: String::new(),
        }
    }
}

impl Component for FilePicker {
    fn handle_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        match ev {
            Event::Key(event) => match event.code {
                KeyCode::Esc => Some(KeyAction::Single(Action::CloseDialog)),
                KeyCode::Backspace => {
                    self.search.truncate(self.search.len().saturating_sub(1));
                    None
                }
                KeyCode::Char(c) => {
                    self.search += &c.to_string();
                    None
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let width = self.width * 80 / 100;
        let height = self.height * 80 / 100;
        let x = (self.width / 2) - (width / 2);
        let y = (self.height / 2) - (height / 2);

        let style = Style {
            fg: Some(Color::White),
            bg: Some(Color::Black),
            ..Default::default()
        };
        let selected_style = Style {
            fg: Some(Color::Black),
            bg: Some(Color::White),
            ..Default::default()
        };

        let root_path = self.root_path.to_string_lossy().to_string();
        let files = list_files(&self.root_path)?
            .iter()
            .map(|f| {
                let new_f = truncate(&f.strip_prefix(&root_path).unwrap().to_string(), width - 2);
                log!("{f} {new_f}");
                new_f
            })
            .collect::<Vec<_>>();

        let dialog = Dialog::new(x, y, width, height, &style);
        let list = List::new(x, y, width, height - 2, files, &style, &selected_style);

        dialog.draw(buffer)?;
        list.draw(buffer)?;

        buffer.set_text(x, y + height - 2, &"─".repeat(width), &style);
        buffer.set_text(x + 1, y + height - 1, &self.search, &style);

        // self.cx = x + 1 + self.search.len();
        // self.cy = y + height - 1;

        Ok(())
    }

    fn current_position(&self) -> Option<(u16, u16)> {
        let width = self.width * 80 / 100;
        let height = self.height * 80 / 100;
        let x = (self.width / 2) - (width / 2);
        let y = (self.height / 2) - (height / 2);

        let cx = x + 1 + self.search.len();
        let cy = y + height - 1;

        Some((cx as u16, cy as u16))
    }
}

fn truncate(s: &str, max_width: usize) -> String {
    if s.len() <= max_width {
        return s.to_string();
    }

    let mut result = String::with_capacity(max_width);
    for (i, c) in s.chars().enumerate() {
        if i == max_width - 1 {
            result.push_str("…");
            break;
        }

        result.push(c);
    }

    result
}

fn list_files<P: AsRef<Path>>(dir: P) -> anyhow::Result<Vec<String>> {
    let mut result = vec![];

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            result.extend(list_files(path)?);
            continue;
        }

        result.push(path.to_string_lossy().to_string());
    }

    Ok(result)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello world", 5), "hell…");
    }
}
