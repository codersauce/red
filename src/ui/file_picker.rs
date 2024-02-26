use std::{
    fs,
    path::{Path, PathBuf},
};

use crossterm::event::{self};

use crate::{
    config::KeyAction,
    editor::{Editor, RenderBuffer},
    log,
};

use super::{Component, Picker};

pub struct FilePicker {
    picker: Picker,
}

impl FilePicker {
    pub fn new(editor: &Editor, root_path: PathBuf) -> anyhow::Result<Self> {
        let root_path = root_path.to_string_lossy().to_string();
        let ignore = read_gitignore(&root_path)?;
        let mut files = list_files(&root_path)?
            .iter()
            .filter(|f| !is_ignored(&ignore, f))
            .map(|f| {
                f.strip_prefix(&root_path)
                    .unwrap()
                    .trim_start_matches('/')
                    .to_string()
            })
            .collect::<Vec<_>>();
        files.sort();

        log!("files: {:?}", files);

        let picker = Picker::new(Some("Find Files".to_string()), editor, files, None);

        Ok(FilePicker { picker })
    }
}

impl Component for FilePicker {
    fn handle_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        self.picker.handle_event(ev)
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.picker.draw(buffer)
    }

    fn cursor_position(&self) -> Option<(u16, u16)> {
        self.picker.cursor_position()
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
