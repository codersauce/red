use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
};

use anyhow::Context;
use crossterm::event::{self};
use ignore::{DirEntry, WalkBuilder};

use crate::{
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    log,
    theme::Theme,
};

use super::{Component, Picker, PickerPreview};

pub struct FilePicker {
    picker: Picker,
    receiver: Receiver<FilePickerLoad>,
    sender: mpsc::Sender<FilePickerLoad>,
    root_path: PathBuf,
    visibility: FilePickerVisibility,
    load_generation: u64,
}

struct FilePickerLoad {
    generation: u64,
    result: Result<Vec<String>, String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FilePickerVisibility {
    hidden: bool,
    ignored: bool,
}

impl FilePickerVisibility {
    fn toggle_all(&mut self) {
        self.hidden = !self.hidden;
        self.ignored = self.hidden;
    }

    fn status(self) -> Option<String> {
        (self.hidden || self.ignored).then(|| "hidden ignored".to_string())
    }
}

impl FilePicker {
    pub fn new(editor: &Editor, root_path: PathBuf) -> anyhow::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let mut picker = Self::loading_with_root(editor, root_path, sender, receiver);
        picker.start_load();
        Ok(picker)
    }

    #[cfg(test)]
    fn loading(editor: &Editor) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self::loading_with_root(editor, PathBuf::from("."), sender, receiver)
    }

    fn loading_with_root(
        editor: &Editor,
        root_path: PathBuf,
        sender: mpsc::Sender<FilePickerLoad>,
        receiver: Receiver<FilePickerLoad>,
    ) -> Self {
        let mut picker = Picker::builder()
            .title("Find Files")
            .items(vec![])
            .history_key("find_files")
            .select_action(Action::OpenFile)
            .build(editor);
        picker.set_empty_message(Some("Loading files...".to_string()));

        FilePicker {
            picker,
            receiver,
            sender,
            root_path,
            visibility: FilePickerVisibility::default(),
            load_generation: 0,
        }
    }

    fn start_load(&mut self) {
        self.load_generation = self.load_generation.wrapping_add(1);
        let generation = self.load_generation;
        let root_path = self.root_path.clone();
        let visibility = self.visibility;
        let sender = self.sender.clone();
        self.picker
            .set_empty_message(Some("Loading files...".to_string()));
        self.picker.set_status(visibility.status());

        std::thread::spawn(move || {
            let result =
                load_file_picker_items(&root_path, visibility).map_err(|err| err.to_string());
            _ = sender.send(FilePickerLoad { generation, result });
        });
    }

    fn apply_load(&mut self, load: FilePickerLoad) -> bool {
        if load.generation != self.load_generation {
            return false;
        }

        match load.result {
            Ok(files) => {
                let previews = file_previews(&self.root_path, &files);
                self.picker.replace_items_with_previews(files, previews);
                self.picker
                    .set_empty_message(Some("No matching files".to_string()));
            }
            Err(err) => {
                log!("file picker load failed: {}", err);
                self.picker.replace_items(vec![]);
                self.picker
                    .set_empty_message(Some("Failed to load files".to_string()));
            }
        }
        true
    }
}

impl Component for FilePicker {
    fn tick(&mut self) -> anyhow::Result<bool> {
        let mut changed = false;
        loop {
            match self.receiver.try_recv() {
                Ok(load) => changed |= self.apply_load(load),
                Err(TryRecvError::Empty) => return Ok(changed),
                Err(TryRecvError::Disconnected) => {
                    self.picker.replace_items(vec![]);
                    self.picker
                        .set_empty_message(Some("Failed to load files".to_string()));
                    return Ok(true);
                }
            }
        }
    }

    fn handle_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        if matches!(
            ev,
            event::Event::Key(key)
                if key.code == event::KeyCode::Char('e')
                    && key.modifiers.contains(event::KeyModifiers::CONTROL)
        ) {
            self.visibility.toggle_all();
            self.start_load();
            return Some(KeyAction::Single(Action::Refresh));
        }
        self.picker.handle_event(ev)
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.picker.draw(buffer)
    }

    fn resize(&mut self, viewport_width: usize, viewport_height: usize) -> bool {
        self.picker.resize(viewport_width, viewport_height)
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.picker.apply_theme(theme);
    }

    fn cursor_position(&self) -> Option<(usize, usize)> {
        self.picker.cursor_position()
    }
}

fn load_file_picker_items(
    root_path: &Path,
    visibility: FilePickerVisibility,
) -> anyhow::Result<Vec<String>> {
    let honor_ignores = !visibility.ignored;
    let mut builder = WalkBuilder::new(root_path);
    builder
        .hidden(!visibility.hidden)
        .ignore(honor_ignores)
        .git_ignore(honor_ignores)
        .git_global(honor_ignores)
        .git_exclude(honor_ignores)
        .follow_links(false)
        .filter_entry(not_vcs_metadata);

    let mut files = Vec::new();
    for result in builder.build() {
        let entry = result.with_context(|| format!("failed to walk {}", root_path.display()))?;
        if !entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
            continue;
        }
        let relative_path = entry.path().strip_prefix(root_path).with_context(|| {
            format!(
                "failed to make {} relative to {}",
                entry.path().display(),
                root_path.display()
            )
        })?;
        files.push(relative_path.to_string_lossy().into_owned());
    }
    files.sort_unstable();

    log!("files: {:?}", files);
    Ok(files)
}

fn not_vcs_metadata(entry: &DirEntry) -> bool {
    entry.depth() == 0 || !matches!(entry.file_name().to_str(), Some(".git" | ".bare"))
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

#[cfg(test)]
mod tests {
    use std::{fs, thread, time::Duration};

    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    use crate::{
        buffer::Buffer,
        config::{Config, KeyAction},
        editor::Editor,
        lsp::LspManager,
        theme::{Style, Theme},
    };

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("red-file-picker-{name}-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            _ = fs::remove_dir_all(&self.0);
        }
    }

    fn test_editor() -> Editor {
        let config = Config::default();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(None, String::new());

        Editor::with_size(lsp, 80, 24, config, Theme::default(), vec![buffer]).unwrap()
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl_key(character: char) -> Event {
        Event::Key(KeyEvent::new(
            KeyCode::Char(character),
            KeyModifiers::CONTROL,
        ))
    }

    fn send_load(picker: &FilePicker, generation: u64, result: Result<Vec<String>, String>) {
        picker
            .sender
            .send(FilePickerLoad { generation, result })
            .unwrap();
    }

    fn wait_for_load(picker: &mut FilePicker) {
        for _ in 0..100 {
            if picker.tick().unwrap() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("file picker load did not finish");
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
        let picker = FilePicker::loading(&editor);
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());

        picker.draw(&mut buffer).unwrap();

        assert!(buffer_text(&buffer).contains("Loading files..."));
    }

    #[test]
    fn file_picker_populates_items_after_load_finishes() {
        let editor = test_editor();
        let mut picker = FilePicker::loading(&editor);

        picker.handle_event(&key(KeyCode::Char('m')));
        send_load(
            &picker,
            picker.load_generation,
            Ok(vec!["src/main.rs".to_string()]),
        );

        assert!(picker.tick().unwrap());
        assert_eq!(
            picker.handle_event(&key(KeyCode::Enter)),
            Some(KeyAction::Multiple(vec![
                Action::RecordPickerHistory {
                    key: "find_files".to_string(),
                    query: "m".to_string(),
                },
                Action::CloseDialog,
                Action::OpenFile("src/main.rs".to_string()),
            ]))
        );
    }

    #[test]
    fn file_picker_shows_preview_for_selected_file() {
        let editor = test_editor();
        let root = TestDir::new("preview");
        fs::create_dir_all(root.path().join("src")).unwrap();
        fs::write(root.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let (sender, receiver) = mpsc::channel();
        let mut picker =
            FilePicker::loading_with_root(&editor, root.path().to_path_buf(), sender, receiver);
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());

        send_load(
            &picker,
            picker.load_generation,
            Ok(vec!["src/main.rs".to_string()]),
        );

        assert!(picker.tick().unwrap());
        picker.draw(&mut buffer).unwrap();

        assert!(buffer_text(&buffer).contains("fn main() {}"));
    }

    #[test]
    fn file_picker_draws_error_message_after_load_fails() {
        let editor = test_editor();
        let mut picker = FilePicker::loading(&editor);
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());

        send_load(&picker, picker.load_generation, Err("boom".to_string()));

        assert!(picker.tick().unwrap());
        picker.draw(&mut buffer).unwrap();

        assert!(buffer_text(&buffer).contains("Failed to load files"));
    }

    #[test]
    fn file_discovery_honors_hidden_and_ignore_filters() {
        let root = TestDir::new("visibility");
        fs::create_dir_all(root.path().join(".git")).unwrap();
        fs::create_dir_all(root.path().join(".bare")).unwrap();
        fs::create_dir_all(root.path().join(".hidden-dir")).unwrap();
        fs::create_dir_all(root.path().join("nested")).unwrap();
        fs::write(
            root.path().join(".gitignore"),
            "ignored.log\n/root-only.txt\n",
        )
        .unwrap();
        fs::write(
            root.path().join("nested/.gitignore"),
            "*.tmp\n!important.tmp\n",
        )
        .unwrap();
        for file in [
            "visible.txt",
            ".hidden.txt",
            ".hidden-dir/secret.txt",
            "ignored.log",
            "root-only.txt",
            "nested/root-only.txt",
            "nested/drop.tmp",
            "nested/important.tmp",
            ".git/config",
            ".bare/data",
        ] {
            fs::write(root.path().join(file), file).unwrap();
        }

        let files = load_file_picker_items(root.path(), FilePickerVisibility::default()).unwrap();

        assert_eq!(
            files,
            vec![
                "nested/important.tmp".to_string(),
                "nested/root-only.txt".to_string(),
                "visible.txt".to_string(),
            ]
        );
    }

    #[test]
    fn expanded_file_discovery_includes_hidden_and_ignored_but_not_vcs_metadata() {
        let root = TestDir::new("expanded");
        fs::create_dir_all(root.path().join(".git")).unwrap();
        fs::create_dir_all(root.path().join(".bare")).unwrap();
        fs::write(root.path().join(".gitignore"), "ignored.log\n").unwrap();
        fs::write(root.path().join(".hidden.txt"), "hidden").unwrap();
        fs::write(root.path().join("ignored.log"), "ignored").unwrap();
        fs::write(root.path().join("visible.txt"), "visible").unwrap();
        fs::write(root.path().join(".git/config"), "git").unwrap();
        fs::write(root.path().join(".bare/data"), "bare").unwrap();

        let files = load_file_picker_items(
            root.path(),
            FilePickerVisibility {
                hidden: true,
                ignored: true,
            },
        )
        .unwrap();

        assert_eq!(
            files,
            vec![
                ".gitignore".to_string(),
                ".hidden.txt".to_string(),
                "ignored.log".to_string(),
                "visible.txt".to_string(),
            ]
        );
    }

    #[test]
    fn ctrl_e_toggles_hidden_and_ignored_files_and_preserves_query() {
        let editor = test_editor();
        let root = TestDir::new("toggle");
        fs::create_dir_all(root.path().join(".git")).unwrap();
        fs::write(root.path().join(".gitignore"), "ignored-match.txt\n").unwrap();
        fs::write(root.path().join("visible-match.txt"), "visible").unwrap();
        fs::write(root.path().join("ignored-match.txt"), "ignored").unwrap();
        let mut picker = FilePicker::new(&editor, root.path().to_path_buf()).unwrap();
        wait_for_load(&mut picker);
        picker.handle_event(&key(KeyCode::Char('m')));

        assert_eq!(
            picker.handle_event(&ctrl_key('e')),
            Some(KeyAction::Single(Action::Refresh))
        );
        assert_eq!(
            picker.visibility,
            FilePickerVisibility {
                hidden: true,
                ignored: true,
            }
        );
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());
        picker.draw(&mut buffer).unwrap();
        assert!(buffer_text(&buffer).contains("hidden ignored"));
        wait_for_load(&mut picker);

        let expanded_selection = picker.handle_event(&key(KeyCode::Enter));
        assert_eq!(
            expanded_selection,
            Some(KeyAction::Multiple(vec![
                Action::RecordPickerHistory {
                    key: "find_files".to_string(),
                    query: "m".to_string(),
                },
                Action::CloseDialog,
                Action::OpenFile("ignored-match.txt".to_string()),
            ]))
        );

        picker.handle_event(&ctrl_key('e'));
        assert_eq!(picker.visibility, FilePickerVisibility::default());
        wait_for_load(&mut picker);
        assert_eq!(
            picker.handle_event(&key(KeyCode::Enter)),
            Some(KeyAction::Multiple(vec![
                Action::RecordPickerHistory {
                    key: "find_files".to_string(),
                    query: "m".to_string(),
                },
                Action::CloseDialog,
                Action::OpenFile("visible-match.txt".to_string()),
            ]))
        );
    }

    #[test]
    fn stale_file_discovery_results_do_not_replace_the_latest_generation() {
        let editor = test_editor();
        let mut picker = FilePicker::loading(&editor);
        picker.load_generation = 2;
        send_load(&picker, 1, Ok(vec!["stale.txt".to_string()]));
        send_load(&picker, 2, Ok(vec!["current.txt".to_string()]));

        assert!(picker.tick().unwrap());
        assert_eq!(
            picker.handle_event(&key(KeyCode::Enter)),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::OpenFile("current.txt".to_string()),
            ]))
        );
    }
}
