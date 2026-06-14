use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
};

use crossterm::event::{self};

use crate::{
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    log,
};

use super::{Component, Picker, PickerPreview};

pub struct FilePicker {
    picker: Picker,
    receiver: Option<Receiver<Result<Vec<String>, String>>>,
    root_path: PathBuf,
}

impl FilePicker {
    pub fn new(editor: &Editor, root_path: PathBuf) -> anyhow::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let load_root = root_path.clone();
        std::thread::spawn(move || {
            let result = load_file_picker_items(&load_root).map_err(|err| err.to_string());
            _ = sender.send(result);
        });

        Ok(Self::loading_with_root(editor, root_path, receiver))
    }

    #[cfg(test)]
    fn loading(editor: &Editor, receiver: Receiver<Result<Vec<String>, String>>) -> Self {
        Self::loading_with_root(editor, PathBuf::from("."), receiver)
    }

    fn loading_with_root(
        editor: &Editor,
        root_path: PathBuf,
        receiver: Receiver<Result<Vec<String>, String>>,
    ) -> Self {
        let mut picker = Picker::builder()
            .title("Find Files")
            .items(vec![])
            .select_action(Action::OpenFile)
            .build(editor);
        picker.set_empty_message(Some("Loading files...".to_string()));

        FilePicker {
            picker,
            receiver: Some(receiver),
            root_path,
        }
    }
}

impl Component for FilePicker {
    fn tick(&mut self) -> anyhow::Result<bool> {
        let Some(receiver) = self.receiver.take() else {
            return Ok(false);
        };

        match receiver.try_recv() {
            Ok(Ok(files)) => {
                let previews = file_previews(&self.root_path, &files);
                self.picker.replace_items_with_previews(files, previews);
                self.picker
                    .set_empty_message(Some("No matching files".to_string()));
                Ok(true)
            }
            Ok(Err(err)) => {
                log!("file picker load failed: {}", err);
                self.picker.replace_items(vec![]);
                self.picker
                    .set_empty_message(Some("Failed to load files".to_string()));
                Ok(true)
            }
            Err(TryRecvError::Empty) => {
                self.receiver = Some(receiver);
                Ok(false)
            }
            Err(TryRecvError::Disconnected) => {
                self.picker.replace_items(vec![]);
                self.picker
                    .set_empty_message(Some("Failed to load files".to_string()));
                Ok(true)
            }
        }
    }

    fn handle_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        self.picker.handle_event(ev)
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.picker.draw(buffer)
    }

    fn cursor_position(&self) -> Option<(usize, usize)> {
        self.picker.cursor_position()
    }
}

fn load_file_picker_items(root_path: &Path) -> anyhow::Result<Vec<String>> {
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
    Ok(files)
}

fn file_previews(root_path: &Path, files: &[String]) -> HashMap<String, PickerPreview> {
    files
        .iter()
        .map(|file| {
            (
                file.clone(),
                PickerPreview::Location {
                    path: root_path.join(file).to_string_lossy().into_owned(),
                    line: None,
                    column: None,
                    matches: Vec::new(),
                },
            )
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    use crate::{
        buffer::Buffer,
        config::{Config, KeyAction},
        editor::Editor,
        lsp::LspManager,
        theme::{Style, Theme},
    };

    fn test_editor() -> Editor {
        let config = Config::default();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(None, String::new());

        Editor::with_size(lsp, 80, 24, config, Theme::default(), vec![buffer]).unwrap()
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn buffer_text(buffer: &RenderBuffer) -> String {
        buffer
            .cells
            .chunks(buffer.width)
            .map(|row| row.iter().map(|cell| cell.c).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn file_picker_draws_loading_message_before_files_arrive() {
        let editor = test_editor();
        let (_sender, receiver) = mpsc::channel();
        let picker = FilePicker::loading(&editor, receiver);
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        assert!(buffer_text(&buffer).contains("Loading files..."));
    }

    #[test]
    fn file_picker_populates_items_after_load_finishes() {
        let editor = test_editor();
        let (sender, receiver) = mpsc::channel();
        let mut picker = FilePicker::loading(&editor, receiver);

        picker.handle_event(&key(KeyCode::Char('m')));
        sender.send(Ok(vec!["src/main.rs".to_string()])).unwrap();

        assert!(picker.tick().unwrap());
        assert_eq!(
            picker.handle_event(&key(KeyCode::Enter)),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::OpenFile("src/main.rs".to_string()),
            ]))
        );
    }

    #[test]
    fn file_picker_shows_preview_for_selected_file() {
        let editor = test_editor();
        let root =
            std::env::temp_dir().join(format!("red-file-picker-preview-{}", std::process::id()));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        let (sender, receiver) = mpsc::channel();
        let mut picker = FilePicker::loading_with_root(&editor, root.clone(), receiver);
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());

        sender.send(Ok(vec!["src/main.rs".to_string()])).unwrap();

        assert!(picker.tick().unwrap());
        picker.draw(&mut buffer).unwrap();

        assert!(buffer_text(&buffer).contains("fn main() {}"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn file_picker_draws_error_message_after_load_fails() {
        let editor = test_editor();
        let (sender, receiver) = mpsc::channel();
        let mut picker = FilePicker::loading(&editor, receiver);
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());

        sender.send(Err("boom".to_string())).unwrap();

        assert!(picker.tick().unwrap());
        picker.draw(&mut buffer).unwrap();

        assert!(buffer_text(&buffer).contains("Failed to load files"));
    }
}
