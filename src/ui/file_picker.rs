use std::{
    fs,
    path::{Path, PathBuf},
};

use crossterm::{
    event::{self, Event, KeyCode},
    style::Color,
};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

use crate::{
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    log,
    theme::Style,
};

use super::{dialog::BorderStyle, Component, Dialog, List};

pub struct FilePicker {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    files: Vec<String>,
    style: Style,
    list: List,
    dialog: Dialog,
    matcher: SkimMatcherV2,

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
        let ignore = read_gitignore(&root_path)?;
        let mut files = list_files(&root_path)?
            .iter()
            .map(|f| truncate(&f.strip_prefix(&root_path).unwrap().to_string(), width - 2))
            .filter(|f| !is_ignored(&ignore, f))
            .collect::<Vec<_>>();
        files.sort();

        let dialog = Dialog::new(x, y, width, height, &style, BorderStyle::None);
        let list = List::new(
            x,
            y,
            width,
            height - 2,
            files.clone(),
            &style,
            &selected_style,
        );

        Ok(FilePicker {
            x,
            y,
            width,
            height,
            style,
            files,
            list,
            dialog,
            matcher: SkimMatcherV2::default(),
            search: String::new(),
        })
    }

    pub fn filter(&mut self, term: &str) {
        log!("filtering with term: {}", term);
        let mut new_items = self
            .files
            .iter()
            .filter_map(|i| {
                if let Some(item) = self.matcher.fuzzy_indices(i, term) {
                    Some((i, item.0))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        log!("{:?}", new_items);
        new_items.sort_by(|a, b| b.1.cmp(&a.1));
        log!("{:?}", new_items);

        let new_items = new_items
            .iter()
            .map(|(item, _)| item.to_string())
            .collect::<Vec<_>>();
        self.list.set_items(new_items);
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
                    let mut search = self.search.clone();
                    search.truncate(self.search.len().saturating_sub(1));

                    self.filter(&search);
                    self.search = search;
                    None
                }
                KeyCode::Enter => Some(KeyAction::Multiple(vec![
                    Action::CloseDialog,
                    Action::OpenFile(self.list.selected_item()),
                ])),
                KeyCode::Char(c) => {
                    let search = format!("{}{}", &self.search, &c);
                    self.filter(&search);
                    self.search = search;
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

fn read_gitignore<P: AsRef<Path>>(dir: P) -> anyhow::Result<Vec<String>> {
    let path = dir.as_ref().join(".gitignore");
    if !path.exists() {
        return Ok(vec![]);
    }

    let content = fs::read_to_string(path)?;
    let mut ret = vec![".git".to_string()];
    ret.extend(
        content
            .lines()
            .map(|s| s.trim_start_matches("/").to_string()),
    );

    Ok(ret)
}

fn is_ignored(ignore: &[String], path: &str) -> bool {
    ignore.iter().any(|i| path.contains(i))
}

fn truncate(s: &str, max_width: usize) -> String {
    let s = s.trim_start_matches("/");
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
