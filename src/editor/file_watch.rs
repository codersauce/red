//! Native open-buffer file watches with a bounded metadata-polling fallback.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant, SystemTime},
};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

use crate::{
    buffer::{BufferId, ExternalFileChange},
    notification::Severity,
    plugin::Runtime,
    undo::{TextPosition, TextRange},
};

use super::{Editor, RenderBuffer};

const OPEN_FILE_POLL_INTERVAL: Duration = Duration::from_millis(250);

struct NativeFileWatcher {
    watcher: RecommendedWatcher,
    events: Receiver<notify::Result<Event>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FileFingerprint {
    Missing,
    Unavailable,
    Present {
        len: u64,
        modified: Option<SystemTime>,
        #[cfg(unix)]
        device: u64,
        #[cfg(unix)]
        inode: u64,
        #[cfg(unix)]
        changed_seconds: i64,
        #[cfg(unix)]
        changed_nanoseconds: i64,
    },
}

impl FileFingerprint {
    fn capture(path: &Path) -> Self {
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Self::Missing,
            Err(_) => return Self::Unavailable,
        };

        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt as _;

        Self::Present {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            changed_seconds: metadata.ctime(),
            #[cfg(unix)]
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug)]
struct TrackedFile {
    path: PathBuf,
    fingerprint: FileFingerprint,
}

/// Watches every named, non-scratch buffer regardless of workspace or LSP configuration.
pub(super) struct OpenFileWatcher {
    native: Option<NativeFileWatcher>,
    native_attempted: bool,
    directories: HashSet<PathBuf>,
    files: HashMap<BufferId, TrackedFile>,
    last_poll: Instant,
}

impl Default for OpenFileWatcher {
    fn default() -> Self {
        Self {
            native: None,
            native_attempted: false,
            directories: HashSet::new(),
            files: HashMap::new(),
            last_poll: Instant::now(),
        }
    }
}

impl OpenFileWatcher {
    /// Returns tracked buffers whose disk state may have changed since the previous tick.
    pub(super) fn poll(&mut self, watched: &[(BufferId, PathBuf)]) -> Vec<BufferId> {
        self.sync_directories(watched);

        let live = watched.iter().map(|(id, _)| *id).collect::<HashSet<_>>();
        self.files.retain(|id, _| live.contains(id));
        for (id, path) in watched {
            match self.files.get_mut(id) {
                Some(tracked) if tracked.path == *path => {}
                Some(tracked) => {
                    tracked.path.clone_from(path);
                    tracked.fingerprint = FileFingerprint::capture(path);
                }
                None => {
                    self.files.insert(
                        *id,
                        TrackedFile {
                            path: path.clone(),
                            fingerprint: FileFingerprint::capture(path),
                        },
                    );
                }
            }
        }

        let mut changed = HashSet::new();
        if let Some(native) = &self.native {
            while let Ok(event) = native.events.try_recv() {
                match event {
                    Ok(event) => {
                        for (id, tracked) in &self.files {
                            if event
                                .paths
                                .iter()
                                .any(|path| tracked.path == *path || tracked.path.starts_with(path))
                            {
                                changed.insert(*id);
                            }
                        }
                    }
                    Err(error) => crate::log!("[editor] open-file watcher error: {error}"),
                }
            }
        }

        let poll_fallback = self.last_poll.elapsed() >= OPEN_FILE_POLL_INTERVAL;
        if poll_fallback {
            self.last_poll = Instant::now();
        }
        for (id, tracked) in &mut self.files {
            if poll_fallback || changed.contains(id) {
                let current = FileFingerprint::capture(&tracked.path);
                if current != tracked.fingerprint {
                    changed.insert(*id);
                }
                tracked.fingerprint = current;
            }
        }

        watched
            .iter()
            .filter_map(|(id, _)| changed.contains(id).then_some(*id))
            .collect()
    }

    fn sync_directories(&mut self, watched: &[(BufferId, PathBuf)]) {
        if !watched.is_empty() && self.native.is_none() && !self.native_attempted {
            self.native_attempted = true;
            let (sender, events) = mpsc::channel();
            match notify::recommended_watcher(move |event| {
                let _ = sender.send(event);
            }) {
                Ok(watcher) => self.native = Some(NativeFileWatcher { watcher, events }),
                Err(error) => {
                    crate::log!("[editor] native open-file watcher unavailable: {error}")
                }
            }
        }

        let desired = watched
            .iter()
            .filter_map(|(_, path)| existing_parent(path))
            .collect::<HashSet<_>>();
        let Some(native) = &mut self.native else {
            self.directories.clear();
            return;
        };

        for directory in self.directories.difference(&desired) {
            if let Err(error) = native.watcher.unwatch(directory) {
                crate::log!("[editor] failed to stop watching {:?}: {error}", directory);
            }
        }
        for directory in desired.difference(&self.directories) {
            if let Err(error) = native.watcher.watch(directory, RecursiveMode::NonRecursive) {
                crate::log!("[editor] failed to watch {:?}: {error}", directory);
            }
        }
        self.directories = desired;
    }

    #[cfg(test)]
    pub(super) fn force_poll(&mut self) {
        self.last_poll = Instant::now() - OPEN_FILE_POLL_INTERVAL;
    }
}

fn existing_parent(path: &Path) -> Option<PathBuf> {
    let mut parent = path.parent()?;
    while !parent.is_dir() {
        parent = parent.parent()?;
    }
    Some(parent.to_path_buf())
}

impl Editor {
    /// Keeps diff construction and confirmation state out of the recursive action future.
    #[inline(never)]
    pub(super) fn open_disk_conflict_dialog(&mut self, runtime: &mut Runtime) -> bool {
        let Some(file) = self.current_buffer().file.clone() else {
            self.set_legacy_message(Some("No file name".to_string()));
            return false;
        };
        let disk = match std::fs::read_to_string(&file) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                "[file was deleted on disk]\n".to_string()
            }
            Err(error) => {
                self.set_legacy_message(Some(error.to_string()));
                return false;
            }
        };
        let local = self.current_buffer().contents();
        let diff = similar::TextDiff::configure()
            .timeout(Duration::from_millis(250))
            .diff_lines(&disk, &local)
            .unified_diff()
            .header("file on disk", "editor buffer")
            .to_string();
        if diff.is_empty() {
            self.current_buffer_mut().set_external_file_change(None);
            self.set_notification_message(
                Severity::Success,
                Some("The editor buffer already matches the file on disk".to_string()),
            );
            return false;
        }
        const MAX_CONFLICT_DIFF_CHARS: usize = 16_384;
        let visible_diff = super::truncate_chars(&diff, MAX_CONFLICT_DIFF_CHARS);
        let truncation = if visible_diff.len() == diff.len() {
            ""
        } else {
            "\n… [diff truncated]"
        };
        let message = format!(
            "{file}\n\n{visible_diff}{truncation}\n\nOverwrite replaces the disk version. Keep edits preserves both versions; use :e! to discard your edits or :w <file> to save elsewhere."
        );
        self.release_current_dialog_callbacks(runtime);
        self.current_dialog = Some(Box::new(super::Confirmation::new_actions(
            self,
            "File changed on disk",
            message,
            "Overwrite",
            "Keep edits",
            super::Action::ForceSave,
            super::Action::Print("Local edits kept; the file on disk was not changed".to_string()),
        )));
        true
    }

    /// Reloads clean external edits and leaves dirty or deleted files explicitly conflicted.
    pub(super) async fn service_open_file_changes(
        &mut self,
        render_buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let watched = self
            .buffer_manager
            .iter()
            .filter(|buffer| !self.scratch_buffers.contains_key(&buffer.id()))
            .filter_map(|buffer| {
                buffer
                    .file
                    .as_ref()
                    .map(|path| (buffer.id(), PathBuf::from(path)))
            })
            .collect::<Vec<_>>();
        let mut changed = self.open_file_watcher.poll(&watched);
        for buffer in self.buffer_manager.iter() {
            if buffer.has_external_file_conflict()
                && !buffer.is_dirty()
                && buffer.external_file_change() != Some(ExternalFileChange::Deleted)
                && !changed.contains(&buffer.id())
            {
                changed.push(buffer.id());
            }
        }

        let mut needs_render = false;
        for id in changed {
            let Some(index) = self
                .buffer_manager
                .iter()
                .position(|buffer| buffer.id() == id)
            else {
                continue;
            };
            let change = match self.buffer_manager[index].detect_external_file_change() {
                Ok(change) => change,
                Err(error) => {
                    crate::log!(
                        "[editor] failed to inspect {:?} after a filesystem event: {error}",
                        self.buffer_manager[index].file
                    );
                    continue;
                }
            };

            let Some(change) = change else {
                needs_render |= self.buffer_manager[index].set_external_file_change(None);
                continue;
            };
            if self.buffer_manager[index].is_dirty() || change == ExternalFileChange::Deleted {
                if self.buffer_manager[index].set_external_file_change(Some(change)) {
                    let path = self.buffer_manager[index].name().to_string();
                    let verb = match change {
                        ExternalFileChange::Modified => "changed",
                        ExternalFileChange::Created => "was created",
                        ExternalFileChange::Deleted => "was deleted",
                    };
                    self.set_notification_message(
                        Severity::Warning,
                        Some(format!(
                            "{path} {verb} on disk; :diffdisk compares, :e! reloads, :w <file> saves elsewhere, :w! overwrites"
                        )),
                    );
                    self.plugin_registry
                        .notify(
                            runtime,
                            "file:external_change",
                            serde_json::json!({
                                "file": path,
                                "buffer_index": index,
                                "document_id": id,
                                "change": match change {
                                    ExternalFileChange::Modified => "modified",
                                    ExternalFileChange::Created => "created",
                                    ExternalFileChange::Deleted => "deleted",
                                },
                                "dirty": self.buffer_manager[index].is_dirty(),
                            }),
                        )
                        .await?;
                    needs_render = true;
                }
                continue;
            }

            if self.reload_changed_file(index, runtime).await? {
                needs_render = true;
            }
        }

        if needs_render {
            self.render(render_buffer)?;
        }
        Ok(())
    }

    async fn reload_changed_file(
        &mut self,
        index: usize,
        runtime: &mut Runtime,
    ) -> anyhow::Result<bool> {
        let (path, contents) = match self.buffer_manager[index].read_backing_file() {
            Ok(result) => result,
            Err(error) => {
                crate::log!(
                    "[editor] failed to reload {:?} after an external edit: {error}",
                    self.buffer_manager[index].file
                );
                return Ok(false);
            }
        };
        if self.buffer_manager[index].is_dirty() {
            return Ok(false);
        }

        let original = self.buffer_manager.active_index();
        let original_view = (self.cx, self.cy, self.vtop, self.vleft, self.skipcol);
        if index != original {
            self.select_buffer_for_lsp_edit(index);
        }
        let resume_insert_transaction = self.commit_active_transaction_before_save();
        let end = self.current_buffer().char_idx_to_position(usize::MAX);
        self.begin_transaction("reload externally changed file");
        self.replace_range(TextRange::new(TextPosition::new(0, 0), end), &contents);
        self.current_buffer_mut().file = Some(path.clone());
        self.commit_transaction(self.cursor_snapshot());
        self.current_buffer_mut().mark_saved();
        self.resume_insert_transaction_after_save(resume_insert_transaction);

        if index != original {
            self.select_buffer_for_lsp_edit(original);
            (self.cx, self.cy, self.vtop, self.vleft, self.skipcol) = original_view;
        }
        self.check_bounds();
        self.sync_to_window();
        self.notify_buffer_change(index, runtime).await?;
        self.plugin_registry
            .notify(
                runtime,
                "file:reloaded",
                serde_json::json!({
                    "file": path,
                    "buffer_index": index,
                    "document_id": self.buffer_manager[index].id(),
                    "external": true,
                }),
            )
            .await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        buffer::{Buffer, ExternalFileChange},
        config::Config,
        editor::{Action, Editor, RenderBuffer},
        lsp::LspManager,
        plugin::Runtime,
        theme::{Style, Theme},
        undo::{TextPosition, TextRange},
    };

    use super::OpenFileWatcher;

    fn editor_with_buffers(buffers: Vec<Buffer>) -> Editor {
        let config = Config::default();
        let mut editor = Editor::test_with_size(
            Box::new(LspManager::new(config.lsp.clone())),
            /*width*/ 80,
            /*height*/ 24,
            config,
            Theme::default(),
            buffers,
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor
    }

    async fn poll_editor(editor: &mut Editor) {
        editor.open_file_watcher.force_poll();
        let mut frame = RenderBuffer::new(/*width*/ 80, /*height*/ 24, &Style::default());
        let mut runtime = Runtime::new();
        editor
            .service_open_file_changes(&mut frame, &mut runtime)
            .await
            .unwrap();
    }

    #[test]
    fn polling_detects_rewrites_deletions_and_recreation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("watched.rs");
        std::fs::write(&path, "before\n").unwrap();
        let buffer = Buffer::new(Some(path.to_string_lossy().into_owned()), "before\n".into());
        let watched = [(buffer.id(), path.clone())];
        let mut watcher = OpenFileWatcher::default();

        assert!(watcher.poll(&watched).is_empty());
        std::fs::write(&path, "changed\n").unwrap();
        watcher.force_poll();
        assert_eq!(watcher.poll(&watched), [buffer.id()]);

        std::fs::remove_file(&path).unwrap();
        watcher.force_poll();
        assert_eq!(watcher.poll(&watched), [buffer.id()]);

        std::fs::write(&path, "restored\n").unwrap();
        watcher.force_poll();
        assert_eq!(watcher.poll(&watched), [buffer.id()]);
    }

    #[test]
    fn closing_a_buffer_stops_reporting_its_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("watched.rs");
        std::fs::write(&path, "before\n").unwrap();
        let buffer = Buffer::new(Some(path.to_string_lossy().into_owned()), "before\n".into());
        let mut watcher = OpenFileWatcher::default();

        assert!(watcher.poll(&[(buffer.id(), path.clone())]).is_empty());
        std::fs::write(path, "changed\n").unwrap();
        watcher.force_poll();
        assert!(watcher.poll(&[]).is_empty());
    }

    #[tokio::test]
    async fn external_edits_reload_clean_buffers_through_undoable_transactions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("watched.rs");
        std::fs::write(&path, "before\n").unwrap();
        let source = Buffer::new(Some(path.to_string_lossy().into_owned()), "before\n".into());
        let mut editor = editor_with_buffers(vec![source]);

        poll_editor(&mut editor).await;
        std::fs::write(&path, "outside\n").unwrap();
        poll_editor(&mut editor).await;

        assert_eq!(editor.current_buffer().contents(), "outside\n");
        assert!(!editor.current_buffer().is_dirty());
        assert!(!editor.current_buffer().has_external_file_conflict());

        editor
            .test_execute_production_action(Action::Undo)
            .await
            .unwrap();
        assert_eq!(editor.current_buffer().contents(), "before\n");
        assert!(editor.current_buffer().is_dirty());
    }

    #[tokio::test]
    async fn external_edits_mark_dirty_buffers_without_touching_either_version() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("watched.rs");
        std::fs::write(&path, "before\n").unwrap();
        let source = Buffer::new(Some(path.to_string_lossy().into_owned()), "before\n".into());
        let mut editor = editor_with_buffers(vec![source]);

        poll_editor(&mut editor).await;
        editor.buffer_manager[0]
            .replace_range_raw(TextRange::insertion(TextPosition::new(0, 0)), "local ");
        std::fs::write(&path, "outside\n").unwrap();
        poll_editor(&mut editor).await;

        assert_eq!(editor.current_buffer().contents(), "local before\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "outside\n");
        assert_eq!(
            editor.current_buffer().external_file_change(),
            Some(ExternalFileChange::Modified)
        );
        assert!(editor.test_statusline_row().contains("[CONFLICT]"));
    }

    #[tokio::test]
    async fn deleted_clean_files_remain_open_with_a_distinct_conflict_marker() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("watched.rs");
        std::fs::write(&path, "before\n").unwrap();
        let source = Buffer::new(Some(path.to_string_lossy().into_owned()), "before\n".into());
        let mut editor = editor_with_buffers(vec![source]);

        poll_editor(&mut editor).await;
        std::fs::remove_file(&path).unwrap();
        poll_editor(&mut editor).await;

        assert_eq!(editor.current_buffer().contents(), "before\n");
        assert!(!editor.current_buffer().is_dirty());
        assert_eq!(
            editor.current_buffer().external_file_change(),
            Some(ExternalFileChange::Deleted)
        );
        assert!(editor.test_statusline_row().contains("[DELETED]"));
    }

    #[tokio::test]
    async fn newly_created_files_reload_pristine_missing_file_buffers() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("created.rs");
        let source = Buffer::load_or_create(Some(path.to_string_lossy().into_owned()))
            .await
            .unwrap();
        let mut editor = editor_with_buffers(vec![source]);

        poll_editor(&mut editor).await;
        std::fs::write(&path, "created outside\n").unwrap();
        poll_editor(&mut editor).await;

        assert_eq!(editor.current_buffer().contents(), "created outside\n");
        assert!(!editor.current_buffer().is_dirty());
        assert!(!editor.current_buffer().has_external_file_conflict());
    }

    #[tokio::test]
    async fn background_buffer_reloads_do_not_change_focus_or_active_cursor() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.rs");
        let second = directory.path().join("second.rs");
        std::fs::write(&first, "first\n").unwrap();
        std::fs::write(&second, "second\n").unwrap();
        let mut editor = editor_with_buffers(vec![
            Buffer::new(Some(first.to_string_lossy().into_owned()), "first\n".into()),
            Buffer::new(
                Some(second.to_string_lossy().into_owned()),
                "second\n".into(),
            ),
        ]);
        let active_id = editor.current_buffer().id();
        editor.cx = 2;

        poll_editor(&mut editor).await;
        std::fs::write(&second, "outside\n").unwrap();
        poll_editor(&mut editor).await;

        assert_eq!(editor.current_buffer().id(), active_id);
        assert_eq!(editor.cx, 2);
        assert_eq!(editor.buffer_manager[1].contents(), "outside\n");
        assert!(!editor.buffer_manager[1].is_dirty());
    }

    #[tokio::test]
    async fn restoring_the_original_disk_contents_clears_a_conflict() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("watched.rs");
        std::fs::write(&path, "before\n").unwrap();
        let source = Buffer::new(Some(path.to_string_lossy().into_owned()), "before\n".into());
        let mut editor = editor_with_buffers(vec![source]);

        poll_editor(&mut editor).await;
        editor.buffer_manager[0]
            .replace_range_raw(TextRange::insertion(TextPosition::new(0, 0)), "local ");
        std::fs::write(&path, "outside\n").unwrap();
        poll_editor(&mut editor).await;
        assert!(editor.current_buffer().has_external_file_conflict());

        std::fs::write(&path, "before\n").unwrap();
        poll_editor(&mut editor).await;

        assert_eq!(editor.current_buffer().contents(), "local before\n");
        assert!(!editor.current_buffer().has_external_file_conflict());
    }
}
