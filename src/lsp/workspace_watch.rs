//! Filesystem events forwarded to a language server for its own workspace.

use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
    sync::{
        mpsc::{self, Receiver},
        Arc,
    },
    time::{Duration, Instant, SystemTime},
};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::config::LanguageServerConfig;

/// LSP `FileChangeType` values are fixed by the wire protocol.
const CREATED: u8 = 1;
const CHANGED: u8 = 2;
const DELETED: u8 = 3;
const OPEN_DIRECTORY_POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkspaceFileChange {
    pub(super) path: PathBuf,
    pub(super) kind: u8,
}

/// Keeps the native watcher alive while its callback hands events to the LSP tick.
pub(super) struct WorkspaceFileWatcher {
    _watcher: RecommendedWatcher,
    events: Receiver<WorkspaceFileChange>,
    filter: Arc<WorkspaceWatchFilter>,
    open_directories: HashMap<PathBuf, HashMap<PathBuf, FileFingerprint>>,
    last_directory_poll: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileFingerprint {
    modified: Option<SystemTime>,
    len: u64,
}

#[derive(Debug)]
struct WorkspaceWatchFilter {
    root: PathBuf,
    extensions: HashSet<String>,
    filenames: HashSet<String>,
}

impl WorkspaceWatchFilter {
    fn new(root: &Path, config: &LanguageServerConfig) -> Self {
        let documents = config.documents();
        let extensions = documents
            .iter()
            .flat_map(|document| &document.file_extensions)
            .map(|extension| extension.trim_start_matches('.').to_ascii_lowercase())
            .collect();
        let filenames = documents
            .iter()
            .flat_map(|document| document.filenames.iter().cloned())
            .chain(config.root_markers.iter().cloned())
            .collect::<HashSet<_>>();
        Self {
            root: root.to_path_buf(),
            extensions,
            filenames,
        }
    }

    fn accepts(&self, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return false;
        };
        if relative.components().any(|component| {
            matches!(
                component,
                Component::Normal(name)
                    if name == ".git" || name == "target" || name == "node_modules"
            )
        }) {
            return false;
        }
        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        self.filenames.contains(filename)
            || path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| self.extensions.contains(&extension.to_ascii_lowercase()))
    }

    fn changes(&self, event: Event) -> Vec<WorkspaceFileChange> {
        let kind = match event.kind {
            EventKind::Create(_) => CREATED,
            EventKind::Modify(_) | EventKind::Any | EventKind::Other => CHANGED,
            EventKind::Remove(_) => DELETED,
            EventKind::Access(_) => return Vec::new(),
        };
        event
            .paths
            .into_iter()
            .filter(|path| self.accepts(path))
            .map(|path| WorkspaceFileChange { path, kind })
            .collect()
    }
}

impl WorkspaceFileWatcher {
    pub(super) fn new(root: &Path, config: &LanguageServerConfig) -> notify::Result<Self> {
        let filter = Arc::new(WorkspaceWatchFilter::new(root, config));
        let callback_filter = Arc::clone(&filter);
        let (sender, events) = mpsc::channel();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<Event>| match event {
                Ok(event) => {
                    for change in callback_filter.changes(event) {
                        if sender.send(change).is_err() {
                            break;
                        }
                    }
                }
                Err(error) => crate::log!("[lsp] workspace file watcher error: {error}"),
            })?;
        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok(Self {
            _watcher: watcher,
            events,
            filter,
            open_directories: HashMap::new(),
            last_directory_poll: Instant::now(),
        })
    }

    /// Restricted macOS environments can accept a native watch but never deliver events.
    /// Poll only an open document's ancestor directories, not the entire workspace tree.
    pub(super) fn watch_document(&mut self, path: &Path) {
        let mut current = path.parent();
        while let Some(directory) = current {
            if !directory.starts_with(&self.filter.root) {
                break;
            }
            self.open_directories
                .entry(directory.to_path_buf())
                .or_insert_with(|| directory_snapshot(&self.filter, directory));
            if directory == self.filter.root {
                break;
            }
            current = directory.parent();
        }
    }

    /// Coalesce repeated backend events while preserving create/delete meaning.
    pub(super) fn take_changes(&mut self) -> Vec<WorkspaceFileChange> {
        let mut changes = HashMap::<PathBuf, u8>::new();
        while let Ok(change) = self.events.try_recv() {
            record_change(&mut changes, change.path, change.kind);
        }
        if self.last_directory_poll.elapsed() >= OPEN_DIRECTORY_POLL_INTERVAL {
            self.last_directory_poll = Instant::now();
            for (directory, previous) in &mut self.open_directories {
                let current = directory_snapshot(&self.filter, directory);
                for (path, fingerprint) in &current {
                    match previous.get(path) {
                        None => record_change(&mut changes, path.clone(), CREATED),
                        Some(previous) if previous != fingerprint => {
                            record_change(&mut changes, path.clone(), CHANGED);
                        }
                        Some(_) => {}
                    }
                }
                for path in previous.keys() {
                    if !current.contains_key(path) {
                        record_change(&mut changes, path.clone(), DELETED);
                    }
                }
                *previous = current;
            }
        }
        changes
            .into_iter()
            .map(|(path, kind)| WorkspaceFileChange { path, kind })
            .collect()
    }
}

fn directory_snapshot(
    filter: &WorkspaceWatchFilter,
    directory: &Path,
) -> HashMap<PathBuf, FileFingerprint> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return HashMap::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if !filter.accepts(&path) {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            metadata.is_file().then_some((
                path,
                FileFingerprint {
                    modified: metadata.modified().ok(),
                    len: metadata.len(),
                },
            ))
        })
        .collect()
}

fn record_change(changes: &mut HashMap<PathBuf, u8>, path: PathBuf, next: u8) {
    changes
        .entry(path)
        .and_modify(|kind| {
            *kind = match (*kind, next) {
                (CREATED, CHANGED) => CREATED,
                (DELETED, CREATED) => CHANGED,
                (_, next) => next,
            };
        })
        .or_insert(next);
}

#[cfg(test)]
mod tests {
    use notify::{event::ModifyKind, Event, EventKind};

    use super::{WorkspaceWatchFilter, CHANGED};
    use crate::config::default_language_servers;

    #[test]
    fn rust_watcher_accepts_modules_and_manifests_but_not_generated_files() {
        let root = std::path::Path::new("/workspace");
        let config = default_language_servers().remove("rust").unwrap();
        let filter = WorkspaceWatchFilter::new(root, &config);

        for path in ["src/lib.rs", "src/nested/recap.RS", "Cargo.toml"] {
            assert!(filter.accepts(&root.join(path)), "should watch {path}");
        }
        for path in [
            "src/notes.md",
            "Cargo.lock",
            "target/generated.rs",
            ".git/hooks/check.rs",
            "node_modules/package/index.rs",
        ] {
            assert!(!filter.accepts(&root.join(path)), "should ignore {path}");
        }
        assert!(!filter.accepts(std::path::Path::new("/outside/lib.rs")));

        let event = Event {
            kind: EventKind::Modify(ModifyKind::Any),
            paths: vec![root.join("src/lib.rs"), root.join("target/generated.rs")],
            attrs: Default::default(),
        };
        let changes = filter.changes(event);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, root.join("src/lib.rs"));
        assert_eq!(changes[0].kind, CHANGED);
    }
}
