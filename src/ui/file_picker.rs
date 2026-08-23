//! Asynchronous workspace file discovery with ignore rules and bounded preview loading.
//!
//! [`FilePicker`] walks from the workspace root without blocking the editor loop, streams
//! discovered paths into a picker, and reads previews on demand. Ignore files and hidden
//! entries follow the configured walker policy; this feature is discovery UI rather than
//! a security boundary for opening paths.

use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
};

use crossterm::event::{self};
use fuzzy_matcher::skim::SkimMatcherV2;

use crate::{
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    log,
    theme::Theme,
    workspace_paths::{discover_workspace_paths, WorkspacePathOptions},
};

use super::{
    picker::PickerFilterHighlights,
    picker_matching::{match_path, path_match_highlights, PathCandidate},
    Component, Picker, PickerItem, PickerPreview,
};

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
        let score_matcher = SkimMatcherV2::default();
        let highlight_matcher = SkimMatcherV2::default();
        let mut picker = Picker::builder()
            .title("Find Files")
            .items(vec![])
            .filter_action(move |item, query| file_match_score(&score_matcher, item, query))
            .incremental_filter()
            .filter_tie_breaker(|item| item.id.len())
            .filter_highlight_action(move |item, query| {
                file_match_highlights(&highlight_matcher, item, query)
            })
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
                let items = files
                    .into_iter()
                    .map(|path| {
                        let relative = Path::new(&path);
                        let label = relative
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.clone());
                        let annotation = relative
                            .parent()
                            .filter(|parent| !parent.as_os_str().is_empty())
                            .map(|parent| parent.to_string_lossy().into_owned());
                        PickerItem {
                            id: path.clone(),
                            icon: None,
                            label,
                            kind: Some("FilePath".to_string()),
                            annotation,
                            detail: None,
                            data: serde_json::Value::Null,
                            matches: Vec::new(),
                            detail_matches: Vec::new(),
                            preview: Some(PickerPreview::Location {
                                path: self.root_path.join(path).to_string_lossy().into_owned(),
                                line: None,
                                column: None,
                                matches: Vec::new(),
                            }),
                        }
                    })
                    .collect();
                self.picker.replace_structured_items(items);
                self.picker
                    .set_empty_message(Some("No matching files".to_string()));
            }
            Err(err) => {
                log!("file picker load failed: {}", err);
                self.picker.replace_structured_items(vec![]);
                self.picker
                    .set_empty_message(Some("Failed to load files".to_string()));
            }
        }
        true
    }
}

impl Component for FilePicker {
    fn shortcut_context(&self) -> &str {
        "Files"
    }
    fn surface_actions(&self) -> Vec<super::UiAction> {
        let mut actions = self.picker.surface_actions();
        actions.extend(super::reference_actions(&[
            ("Files", "Ctrl+e", "Toggle hidden files"),
            ("Files", ">", "Open commands when the query is empty"),
        ]));
        actions
    }

    fn tick(&mut self) -> anyhow::Result<bool> {
        let mut changed = false;
        loop {
            match self.receiver.try_recv() {
                Ok(load) => changed |= self.apply_load(load),
                Err(TryRecvError::Empty) => return Ok(changed),
                Err(TryRecvError::Disconnected) => {
                    self.picker.replace_structured_items(vec![]);
                    self.picker
                        .set_empty_message(Some("Failed to load files".to_string()));
                    return Ok(true);
                }
            }
        }
    }

    fn handle_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        if self.picker.query().is_empty()
            && matches!(
                ev,
                event::Event::Key(key)
                    if key.code == event::KeyCode::Char('>')
                        && !key.modifiers.intersects(
                            event::KeyModifiers::CONTROL | event::KeyModifiers::ALT
                        )
            )
        {
            return Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::CommandPalette,
            ]));
        }
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

fn file_match_score(matcher: &SkimMatcherV2, item: &PickerItem, query: &str) -> Option<i64> {
    match_path(
        matcher,
        PathCandidate::new(&item.id, &item.label, item.annotation.as_deref()),
        query,
    )
    .map(|matched| matched.score)
}

fn file_match_highlights(
    matcher: &SkimMatcherV2,
    item: &PickerItem,
    query: &str,
) -> PickerFilterHighlights {
    path_match_highlights(
        matcher,
        PathCandidate::new(&item.id, &item.label, item.annotation.as_deref()),
        query,
    )
}

fn load_file_picker_items(
    root_path: &Path,
    visibility: FilePickerVisibility,
) -> anyhow::Result<Vec<String>> {
    let (entries, _) = discover_workspace_paths(
        root_path,
        WorkspacePathOptions {
            hidden: visibility.hidden,
            ignored: visibility.ignored,
            directories: false,
            max_entries: None,
        },
    )?;
    Ok(entries.into_iter().map(|entry| entry.path).collect())
}

#[cfg(test)]
mod tests {
    use std::{
        fs, thread,
        time::{Duration, Instant},
    };

    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    use crate::{
        buffer::Buffer,
        color::Color,
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
        test_editor_with_theme(Theme::default())
    }

    fn test_editor_with_theme(theme: Theme) -> Editor {
        let config = Config::default();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let buffer = Buffer::new(None, String::new());

        Editor::with_size(lsp, 80, 24, config, theme, vec![buffer]).unwrap()
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

    fn selected_file(picker: &mut FilePicker) -> String {
        let Some(KeyAction::Multiple(actions)) = picker.handle_event(&key(KeyCode::Enter)) else {
            panic!("expected file picker selection actions");
        };
        actions
            .into_iter()
            .find_map(|action| match action {
                Action::OpenFile(path) => Some(path),
                _ => None,
            })
            .expect("file picker selection should open a file")
    }

    fn picker_item(path: &str) -> PickerItem {
        let path = Path::new(path);
        PickerItem {
            id: path.to_string_lossy().into_owned(),
            icon: None,
            label: path.file_name().unwrap().to_string_lossy().into_owned(),
            kind: Some("FilePath".to_string()),
            annotation: path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .map(|parent| parent.to_string_lossy().into_owned()),
            detail: None,
            data: serde_json::Value::Null,
            matches: Vec::new(),
            detail_matches: Vec::new(),
            preview: None,
        }
    }

    fn rendered_text_start(buffer: &RenderBuffer, needle: &str) -> usize {
        for (row_index, row) in buffer.cells.chunks(buffer.width).enumerate() {
            let text = row.iter().map(|cell| cell.c).collect::<String>();
            if let Some(byte_column) = text.find(needle) {
                let column = text[..byte_column].chars().count();
                return row_index * buffer.width + column;
            }
        }
        panic!("{needle:?} was not rendered");
    }

    #[test]
    #[ignore = "manual performance benchmark for large workspaces"]
    fn file_picker_large_workspace_performance() {
        const SAMPLES: usize = 5;
        const QUERIES: [&str; 5] = [
            "thread",
            "agent",
            "config",
            "workspace",
            "codex-rs/core/src",
        ];

        let editor = test_editor();
        let benchmark_root = std::env::var_os("RED_FILE_PICKER_BENCH_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let files = if std::env::var_os("RED_FILE_PICKER_BENCH_ROOT").is_some() {
            let started = Instant::now();
            let files = load_file_picker_items(&benchmark_root, FilePickerVisibility::default())
                .expect("benchmark workspace should be readable");
            eprintln!(
                "file-picker benchmark discovery: files={} elapsed={:?} root={}",
                files.len(),
                started.elapsed(),
                benchmark_root.display(),
            );
            files
        } else {
            const NAMES: [&str; 12] = [
                "thread",
                "agent",
                "config",
                "workspace",
                "client",
                "protocol",
                "session",
                "approval",
                "terminal",
                "message",
                "history",
                "review",
            ];
            (0..12_000)
                .map(|index| {
                    format!(
                        "codex-rs/crate_{:02}/src/{}_{index:05}.rs",
                        index % 96,
                        NAMES[index % NAMES.len()],
                    )
                })
                .collect()
        };
        assert!(files.len() >= 5_000, "benchmark requires a large workspace");

        let mut installation_samples = Vec::with_capacity(SAMPLES);
        let mut query_samples = vec![Vec::with_capacity(SAMPLES); QUERIES.len()];
        let mut draw_samples = vec![Vec::with_capacity(SAMPLES); QUERIES.len()];
        let mut slowest_keystrokes = Vec::with_capacity(SAMPLES);

        for _ in 0..SAMPLES {
            let (sender, receiver) = mpsc::channel();
            let mut picker =
                FilePicker::loading_with_root(&editor, benchmark_root.clone(), sender, receiver);
            send_load(&picker, picker.load_generation, Ok(files.clone()));
            let started = Instant::now();
            assert!(picker.tick().expect("benchmark load should succeed"));
            installation_samples.push(started.elapsed());
            let mut buffer = RenderBuffer::new(80, 24, &Style::default());
            let mut slowest_keystroke = Duration::ZERO;

            for (query_index, query) in QUERIES.iter().enumerate() {
                while !picker.picker.query().is_empty() {
                    picker.handle_event(&key(KeyCode::Backspace));
                }
                let mut query_elapsed = Duration::ZERO;
                let mut draw_elapsed = Duration::ZERO;
                for character in query.chars() {
                    let started = Instant::now();
                    picker.handle_event(&key(KeyCode::Char(character)));
                    let elapsed = started.elapsed();
                    query_elapsed += elapsed;
                    slowest_keystroke = slowest_keystroke.max(elapsed);
                    let started = Instant::now();
                    picker
                        .draw(&mut buffer)
                        .expect("benchmark draw should succeed");
                    draw_elapsed += started.elapsed();
                }
                query_samples[query_index].push(query_elapsed);
                draw_samples[query_index].push(draw_elapsed);
            }
            slowest_keystrokes.push(slowest_keystroke);
        }

        let median = |samples: &mut [Duration]| {
            samples.sort_unstable();
            samples[samples.len() / 2]
        };
        eprintln!(
            "file-picker benchmark: files={} samples={SAMPLES} install={:?} slowest_key={:?}",
            files.len(),
            median(&mut installation_samples),
            median(&mut slowest_keystrokes),
        );
        for (index, query) in QUERIES.iter().enumerate() {
            eprintln!(
                "file-picker benchmark query={query:?}: filtering={:?} drawing={:?}",
                median(&mut query_samples[index]),
                median(&mut draw_samples[index]),
            );
        }
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
    fn file_picker_filters_full_paths_but_displays_basename_and_parent() {
        let editor = test_editor();
        let mut picker = FilePicker::loading(&editor);
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());

        for character in "husk-parser".chars() {
            picker.handle_event(&key(KeyCode::Char(character)));
        }
        send_load(
            &picker,
            picker.load_generation,
            Ok(vec!["crates/husk-parser/src/lib.rs".to_string()]),
        );

        assert!(picker.tick().unwrap());
        picker.draw(&mut buffer).unwrap();
        let text = buffer_text(&buffer);
        assert!(text.contains("lib.rs crates/husk-parser/src"), "{text}");
        assert!(text.contains("1/1"), "{text}");
        assert_eq!(
            picker.handle_event(&key(KeyCode::Enter)),
            Some(KeyAction::Multiple(vec![
                Action::RecordPickerHistory {
                    key: "find_files".to_string(),
                    query: "husk-parser".to_string(),
                },
                Action::CloseDialog,
                Action::OpenFile("crates/husk-parser/src/lib.rs".to_string()),
            ]))
        );
    }

    #[test]
    fn file_picker_prefers_shorter_paths_when_match_scores_are_equal() {
        let editor = test_editor();
        let mut picker = FilePicker::loading(&editor);
        for character in "main".chars() {
            picker.handle_event(&key(KeyCode::Char(character)));
        }
        send_load(
            &picker,
            picker.load_generation,
            Ok(vec![
                "crates/husk-cli/src/main.rs".to_string(),
                "crates/husk-lsp/src/main.rs".to_string(),
                "examples/external-hello-plugin/src/main.hk".to_string(),
                "plugins/git_core/src/main.hk".to_string(),
                "plugins/neotree_core/src/main.hk".to_string(),
                "src/main.rs".to_string(),
                "tools/husk-standalone/smoke/src/main.rs".to_string(),
            ]),
        );

        assert!(picker.tick().unwrap());
        for expected in [
            "src/main.rs",
            "crates/husk-cli/src/main.rs",
            "crates/husk-lsp/src/main.rs",
            "plugins/git_core/src/main.hk",
            "plugins/neotree_core/src/main.hk",
            "tools/husk-standalone/smoke/src/main.rs",
            "examples/external-hello-plugin/src/main.hk",
        ] {
            assert_eq!(selected_file(&mut picker), expected);
            picker.handle_event(&key(KeyCode::Down));
        }
    }

    #[test]
    fn file_picker_prefers_filename_matches_over_directory_matches() {
        let editor = test_editor();
        let mut picker = FilePicker::loading(&editor);
        for character in "main".chars() {
            picker.handle_event(&key(KeyCode::Char(character)));
        }
        send_load(
            &picker,
            picker.load_generation,
            Ok(vec![
                "main/lib.rs".to_string(),
                "deep/src/main.rs".to_string(),
                "src/domain/mainland.rs".to_string(),
            ]),
        );

        assert!(picker.tick().unwrap());
        assert_eq!(selected_file(&mut picker), "deep/src/main.rs");
    }

    #[test]
    fn file_picker_prefers_filename_matches_over_shared_parent_matches() {
        let editor = test_editor();
        let mut picker = FilePicker::loading(&editor);
        for character in "recap".chars() {
            picker.handle_event(&key(KeyCode::Char(character)));
        }
        send_load(
            &picker,
            picker.load_generation,
            Ok(vec![
                "codex.fcoury-recap/src/lib.rs".to_string(),
                "codex.fcoury-recap/src/thread_recap.rs".to_string(),
                "codex.fcoury-recap/src/recap.rs".to_string(),
            ]),
        );

        assert!(picker.tick().unwrap());
        assert_eq!(
            selected_file(&mut picker),
            "codex.fcoury-recap/src/recap.rs"
        );
        picker.handle_event(&key(KeyCode::Down));
        assert_eq!(
            selected_file(&mut picker),
            "codex.fcoury-recap/src/thread_recap.rs"
        );
        picker.handle_event(&key(KeyCode::Down));
        assert_eq!(selected_file(&mut picker), "codex.fcoury-recap/src/lib.rs");
    }

    #[test]
    fn file_match_highlights_filename_characters() {
        let highlights = file_match_highlights(
            &SkimMatcherV2::default(),
            &picker_item("src/main.rs"),
            "main",
        );

        assert_eq!(highlights.label, vec![[0, 4]]);
        assert!(highlights.annotation.is_empty());
    }

    #[test]
    fn file_match_highlights_parent_and_filename_characters() {
        let highlights = file_match_highlights(
            &SkimMatcherV2::default(),
            &picker_item("src/main.rs"),
            "smn",
        );

        assert_eq!(highlights.label, vec![[0, 1], [3, 4]]);
        assert_eq!(highlights.annotation, vec![[0, 1]]);
    }

    #[test]
    fn file_match_highlights_use_character_indices_for_unicode() {
        let highlights = file_match_highlights(
            &SkimMatcherV2::default(),
            &picker_item("src/mäin.rs"),
            "min",
        );

        assert_eq!(highlights.label, vec![[0, 1], [2, 4]]);
    }

    #[test]
    fn file_picker_renders_query_matches_with_the_list_highlight_color() {
        let match_color = Color::Rgb {
            r: 255,
            g: 204,
            b: 102,
        };
        let selected_background = Color::Rgb { r: 0, g: 0, b: 0 };
        let mut theme = Theme::default();
        theme
            .colors
            .insert("list.highlightForeground".to_string(), match_color);
        theme.ui_style.picker_item = Style {
            fg: Some(Color::Rgb { r: 0, g: 0, b: 0 }),
            bg: Some(Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            }),
            ..Style::default()
        };
        theme.ui_style.picker_selected_item = Style {
            fg: Some(Color::Rgb {
                r: 80,
                g: 250,
                b: 123,
            }),
            bg: Some(selected_background),
            ..Style::default()
        };
        let editor = test_editor_with_theme(theme);
        let mut picker = FilePicker::loading(&editor);
        let mut buffer = RenderBuffer::new(80, 24, &Style::default());
        for character in "main".chars() {
            picker.handle_event(&key(KeyCode::Char(character)));
        }
        send_load(
            &picker,
            picker.load_generation,
            Ok(vec!["src/main.rs".to_string()]),
        );

        assert!(picker.tick().unwrap());
        picker.draw(&mut buffer).unwrap();
        let start = rendered_text_start(&buffer, "main.rs src");
        let row_background = buffer.cells[start + 4].style.bg;

        for cell in &buffer.cells[start..start + 4] {
            assert_eq!(cell.style.fg, Some(match_color));
            assert_eq!(cell.style.bg, row_background);
        }
        assert_ne!(buffer.cells[start + 4].style.fg, Some(match_color));
    }

    #[test]
    fn file_picker_preserves_a_stronger_parent_path_match() {
        let editor = test_editor();
        let mut picker = FilePicker::loading(&editor);
        for character in "src".chars() {
            picker.handle_event(&key(KeyCode::Char(character)));
        }
        send_load(
            &picker,
            picker.load_generation,
            Ok(vec![
                "lib/source.rs".to_string(),
                "src/source.rs".to_string(),
                "src/main.rs".to_string(),
            ]),
        );

        assert!(picker.tick().unwrap());
        assert_eq!(selected_file(&mut picker), "src/main.rs");
        picker.handle_event(&key(KeyCode::Down));
        assert_eq!(selected_file(&mut picker), "src/source.rs");
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
            files.into_iter().map(PathBuf::from).collect::<Vec<_>>(),
            vec![
                PathBuf::from("nested").join("important.tmp"),
                PathBuf::from("nested").join("root-only.txt"),
                PathBuf::from("visible.txt"),
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
    fn leading_greater_than_switches_file_picker_to_command_palette() {
        let editor = test_editor();
        let mut picker = FilePicker::loading(&editor);

        assert_eq!(
            picker.handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Char('>'),
                KeyModifiers::SHIFT,
            ))),
            Some(KeyAction::Multiple(vec![
                Action::CloseDialog,
                Action::CommandPalette,
            ]))
        );
    }

    #[test]
    fn greater_than_after_file_query_remains_part_of_the_query() {
        let editor = test_editor();
        let mut picker = FilePicker::loading(&editor);
        picker.handle_event(&key(KeyCode::Char('s')));

        picker.handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('>'),
            KeyModifiers::SHIFT,
        )));

        assert_eq!(picker.picker.query(), "s>");
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
