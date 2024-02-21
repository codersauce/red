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
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    style: Style,
    selected_style: Style,
    list: List,
    dialog: Dialog,

    search: String,
}

impl FilePicker {
    pub fn new(editor: &Editor, root_path: PathBuf) -> anyhow::Result<Self> {
        let total_width = editor.vwidth();
        let total_height = editor.vheight();

        let width = total_width * 80 / 100;
        let height = total_height * 80 / 100;
        let x = (total_width / 2) - (width / 2);
        let y = (total_height / 2) - (height / 2);

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

        let root_path = root_path.to_string_lossy().to_string();
        let files = list_files(&root_path)?
            .iter()
            .map(|f| truncate(&f.strip_prefix(&root_path).unwrap().to_string(), width - 2))
            .collect::<Vec<_>>();

        let dialog = Dialog::new(x, y, width, height, &style);
        let list = List::new(x, y, width, height - 2, files, &style, &selected_style);

        Ok(FilePicker {
            x,
            y,
            width,
            height,
            style,
            selected_style,
            list,
            dialog,

            search: String::new(),
        })
    }
}

impl Component for FilePicker {
    fn handle_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        match ev {
            Event::Key(event) => match event.code {
                KeyCode::Down => {
                    self.list.move_down();
                    None
                }
                KeyCode::Up => {
                    self.list.move_up();
                    None
                }
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
        self.dialog.draw(buffer)?;
        self.list.draw(buffer)?;

        buffer.set_text(
            self.x,
            self.y + self.height - 2,
            &"─".repeat(self.width),
            &self.style,
        );
        buffer.set_text(
            self.x + 1,
            self.y + self.height - 1,
            &self.search,
            &self.style,
        );

        Ok(())
    }

    fn cursor_position(&self) -> Option<(u16, u16)> {
        let cx = self.x + 1 + self.search.len();
        let cy = self.y + self.height - 1;

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
