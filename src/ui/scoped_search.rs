//! Live ripgrep search over an explicit, owned set of files.
//!
//! The normal picker renders results and previews. Query changes cancel the
//! previous process, and generation checks discard already-delivered results.

use super::{Component, Picker, PickerItem, PickerPreview, UiAction};
use crate::{
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    plugin::{LocationColumnEncoding, OpenLocationTarget, PluginLocation},
    theme::Theme,
};
use anyhow::{Context, Result};
use crossterm::event::Event;
use serde::Deserialize;
use serde_json::Value;
use std::{
    path::{Component as PathComponent, Path, PathBuf},
    process::Stdio,
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};
use tokio::{process::Command, task::JoinHandle};

const MAX_RESULTS: usize = 500;
const MAX_FILE_BYTES: u64 = 256 * 1024;
const DEBOUNCE: Duration = Duration::from_millis(100);

struct SearchResult {
    generation: u64,
    result: Result<Vec<PickerItem>, String>,
}

pub(crate) struct ScopedProjectSearch {
    picker: Picker,
    root: PathBuf,
    files: Vec<PathBuf>,
    generation: u64,
    pending: Option<(Instant, String)>,
    task: Option<JoinHandle<()>>,
    sender: Sender<SearchResult>,
    receiver: Receiver<SearchResult>,
}

impl ScopedProjectSearch {
    pub(crate) fn new(editor: &Editor, root: PathBuf, files: Vec<PathBuf>) -> Result<Self> {
        let root = root.canonicalize()?;
        validate_files(&root, &files)?;
        let (sender, receiver) = mpsc::channel();
        let mut picker = Picker::builder()
            .title("Find in Files")
            .structured_items(Vec::new())
            .external_filter()
            .placeholder("Search with ripgrep")
            .status("Literal · smart case · practice files")
            .select_action(|id| match serde_json::from_str::<PluginLocation>(&id) {
                Ok(location) => Action::OpenLocation(location, OpenLocationTarget::Current),
                Err(_) => Action::Print("search result is no longer available".into()),
            })
            .build(editor);
        picker.set_empty_message(Some("Type to search the practice project".into()));
        Ok(Self {
            picker,
            root,
            files,
            generation: 0,
            pending: None,
            task: None,
            sender,
            receiver,
        })
    }

    fn cancel(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }

    fn query_changed(&mut self) {
        self.cancel();
        self.generation = self.generation.wrapping_add(1);
        let query = self.picker.query().to_string();
        self.picker.replace_structured_items(Vec::new());
        self.picker.set_busy(!query.is_empty());
        self.picker.set_empty_message(Some(
            if query.is_empty() {
                "Type to search the practice project"
            } else {
                "Searching…"
            }
            .into(),
        ));
        self.picker
            .set_status(Some("Literal · smart case · practice files".into()));
        self.pending = (!query.is_empty()).then(|| (Instant::now() + DEBOUNCE, query));
    }

    fn start_pending(&mut self) -> bool {
        if self
            .pending
            .as_ref()
            .is_none_or(|(when, _)| Instant::now() < *when)
        {
            return false;
        }
        let (_, query) = self.pending.take().expect("pending search was checked");
        let root = self.root.clone();
        let files = self.files.clone();
        let generation = self.generation;
        let sender = self.sender.clone();
        self.task = Some(tokio::spawn(async move {
            let result = run_search(&root, &files, &query)
                .await
                .map_err(|error| format!("{error:#}"));
            let _ = sender.send(SearchResult { generation, result });
        }));
        true
    }

    fn apply_result(&mut self, result: SearchResult) -> bool {
        if result.generation != self.generation {
            return false;
        }
        self.task = None;
        self.picker.set_busy(false);
        match result.result {
            Ok(items) => {
                let count = items.len();
                self.picker.replace_structured_items(items);
                self.picker.set_empty_message(Some("No matches".into()));
                self.picker.set_status(Some(format!(
                    "{count} {} · literal · practice files",
                    if count == 1 { "match" } else { "matches" }
                )));
            }
            Err(error) => {
                self.picker.replace_structured_items(Vec::new());
                self.picker
                    .set_empty_message(Some("Search unavailable".into()));
                self.picker.set_status(Some(error));
            }
        }
        true
    }
}

impl Drop for ScopedProjectSearch {
    fn drop(&mut self) {
        self.cancel();
    }
}
impl Component for ScopedProjectSearch {
    fn shortcut_context(&self) -> &str {
        "Find in Files"
    }
    fn surface_actions(&self) -> Vec<UiAction> {
        self.picker.surface_actions()
    }
    fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
        let before = self.picker.query().to_string();
        let action = self.picker.handle_event(event);
        if self.picker.query() != before {
            self.query_changed();
            return action.or(Some(KeyAction::Single(Action::Refresh)));
        }
        action
    }
    fn tick(&mut self) -> Result<bool> {
        let mut changed = self.picker.tick()?;
        while let Ok(result) = self.receiver.try_recv() {
            changed |= self.apply_result(result);
        }
        changed |= self.start_pending();
        Ok(changed)
    }
    fn draw(&self, buffer: &mut RenderBuffer) -> Result<()> {
        self.picker.draw(buffer)
    }
    fn resize(&mut self, width: usize, height: usize) -> bool {
        self.picker.resize(width, height)
    }
    fn set_theme(&mut self, theme: &Theme) {
        self.picker.apply_theme(theme);
    }
    fn cursor_position(&self) -> Option<(usize, usize)> {
        self.picker.cursor_position()
    }
}

fn validate_files(root: &Path, files: &[PathBuf]) -> Result<()> {
    anyhow::ensure!(
        !files.is_empty() && files.len() <= 64,
        "invalid scoped search file set"
    );
    for relative in files {
        anyhow::ensure!(
            !relative.as_os_str().is_empty()
                && relative
                    .components()
                    .all(|c| matches!(c, PathComponent::Normal(_))),
            "invalid scoped search path"
        );
        let path = root.join(relative);
        let metadata = std::fs::symlink_metadata(&path)?;
        anyhow::ensure!(
            metadata.file_type().is_file()
                && metadata.len() <= MAX_FILE_BYTES
                && path.canonicalize()? == path,
            "scoped search file is unsafe or too large"
        );
    }
    Ok(())
}

async fn run_search(root: &Path, files: &[PathBuf], query: &str) -> Result<Vec<PickerItem>> {
    anyhow::ensure!(
        !query.is_empty() && query.len() <= 1024,
        "search query must be between 1 and 1024 bytes"
    );
    validate_files(root, files)?;
    let mut command = Command::new("rg");
    command
        .current_dir(root)
        .env_remove("RIPGREP_CONFIG_PATH")
        .args([
            "--no-config",
            "--json",
            "--color=never",
            "--no-heading",
            "--with-filename",
            "--line-number",
            "--column",
            "--smart-case",
            "--fixed-strings",
            "--no-ignore",
            "--no-follow",
            "--max-count=200",
            "--max-columns=500",
            "--max-columns-preview",
            "--",
            query,
        ])
        .args(files)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(5), command.output())
        .await
        .context("practice search timed out")?
        .context("could not run ripgrep; install rg to use project search")?;
    anyhow::ensure!(
        output.status.success() || output.status.code() == Some(1),
        "ripgrep: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    anyhow::ensure!(
        output.stdout.len() <= 4 * 1024 * 1024,
        "practice search output is too large"
    );
    let mut items = Vec::new();
    for line in output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let value: Value = serde_json::from_slice(line)?;
        if value["type"] != "match" {
            continue;
        }
        let data: MatchData = serde_json::from_value(value["data"].clone())?;
        items.push(match_item(root, files, data)?);
        if items.len() == MAX_RESULTS {
            break;
        }
    }
    Ok(items)
}

#[derive(Deserialize)]
struct TextField {
    text: String,
}
#[derive(Deserialize)]
struct Submatch {
    start: usize,
    end: usize,
}
#[derive(Deserialize)]
struct MatchData {
    path: TextField,
    lines: TextField,
    line_number: usize,
    submatches: Vec<Submatch>,
}

fn match_item(root: &Path, files: &[PathBuf], data: MatchData) -> Result<PickerItem> {
    let relative = PathBuf::from(&data.path.text);
    anyhow::ensure!(
        files.contains(&relative),
        "ripgrep returned a file outside the search scope"
    );
    let path = root.join(&relative).to_string_lossy().into_owned();
    let text = data.lines.text.trim_end_matches(['\r', '\n']);
    let line = data
        .line_number
        .checked_sub(1)
        .context("invalid ripgrep line")?;
    let first = data
        .submatches
        .first()
        .context("ripgrep match has no range")?;
    let mut detail_matches = Vec::new();
    let mut preview_matches = Vec::new();
    for found in &data.submatches {
        anyhow::ensure!(
            found.start <= found.end
                && found.end <= text.len()
                && text.is_char_boundary(found.start)
                && text.is_char_boundary(found.end),
            "invalid ripgrep match range"
        );
        detail_matches.push([
            text[..found.start].chars().count(),
            text[..found.end].chars().count(),
        ]);
        preview_matches.push([found.start, found.end]);
    }
    let location = PluginLocation {
        path: path.clone(),
        line,
        column: first.start,
        column_encoding: LocationColumnEncoding::Utf8Byte,
    };
    let label = relative
        .file_name()
        .context("search result has no filename")?
        .to_string_lossy()
        .into_owned();
    let parent = relative
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| format!("{}/", p.display()))
        .unwrap_or_default();
    Ok(PickerItem {
        id: serde_json::to_string(&location)?,
        icon: None,
        label,
        kind: Some("FileMatch".into()),
        annotation: Some(format!("{parent}:{}:{}", line + 1, first.start + 1)),
        detail: Some(text.into()),
        data: serde_json::json!({"location":location}),
        matches: Vec::new(),
        detail_matches,
        preview: Some(PickerPreview::Location {
            path,
            line: Some(line),
            column: Some(first.start),
            matches: preview_matches,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{buffer::Buffer, config::Config, lsp::LspManager, theme::Theme};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn data(path: &str, text: &str, start: usize, end: usize) -> MatchData {
        MatchData {
            path: TextField { text: path.into() },
            lines: TextField { text: text.into() },
            line_number: 3,
            submatches: vec![Submatch { start, end }],
        }
    }

    #[test]
    fn learn_search_match_keeps_byte_locations_and_character_highlights() {
        let root = Path::new("/owned");
        let files = vec![PathBuf::from("src/score.hk")];
        let item = match_item(
            root,
            &files,
            data("src/score.hk", "// 🎯 add_score\n", 8, 17),
        )
        .unwrap();
        assert_eq!(item.detail_matches, vec![[5, 14]]);
        let location: PluginLocation = serde_json::from_str(&item.id).unwrap();
        assert_eq!(location.column, 8);
        assert_eq!(location.line, 2);
        assert_eq!(location.column_encoding, LocationColumnEncoding::Utf8Byte);
        assert!(match_item(root, &files, data("../outside", "text", 0, 4)).is_err());
        assert!(match_item(root, &files, data("src/score.hk", "🎯", 1, 4)).is_err());
    }

    #[tokio::test]
    async fn learn_search_discards_stale_results_and_cancels_pending_work() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("score.hk"), "add_score\n").unwrap();
        let files = vec![PathBuf::from("score.hk")];
        let config = Config::default();
        let client = Box::new(LspManager::new(config.lsp.clone()));
        let editor = Editor::with_size(
            client,
            80,
            24,
            config,
            Theme::default(),
            vec![Buffer::new(None, String::new())],
        )
        .unwrap();
        let mut search = ScopedProjectSearch::new(&editor, root.clone(), files.clone()).unwrap();
        search.generation = 2;
        let item = match_item(&root, &files, data("score.hk", "add_score\n", 0, 9)).unwrap();
        assert!(!search.apply_result(SearchResult {
            generation: 1,
            result: Ok(vec![item.clone()])
        }));
        assert!(search.apply_result(SearchResult {
            generation: 2,
            result: Ok(vec![item])
        }));
        let event = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let Some(KeyAction::Multiple(actions)) = search.handle_event(&event) else {
            panic!("expected selection");
        };
        assert!(actions.iter().any(|action|matches!(action,Action::OpenLocation(location,OpenLocationTarget::Current) if location.path==root.join("score.hk").to_string_lossy())));
        assert!(!actions
            .iter()
            .any(|action| matches!(action, Action::RecordPickerHistory { .. })));
        let task = tokio::spawn(std::future::pending::<()>());
        let abort = task.abort_handle();
        search.task = Some(task);
        drop(search);
        tokio::task::yield_now().await;
        assert!(abort.is_finished());
    }
}
