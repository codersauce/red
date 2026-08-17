//! Crash-safe recovery, scoped to one owned practice file.

use super::*;
use crate::learn::personalization::{RECOVERY_CONTENTS, RECOVERY_RESULT};

#[derive(Default)]
pub(super) struct LearnRecoveryState {
    pub saved: bool,
    pub restored: bool,
}

impl Editor {
    fn learn_recovery_paths(&self) -> anyhow::Result<(PathBuf, SessionStore)> {
        let workspace = self
            .learn_session
            .as_ref()
            .filter(|session| session.lesson == Lesson::KeepYourPlace)
            .and_then(|session| session.workspace.as_ref())
            .ok_or_else(|| anyhow::anyhow!("no recovery practice is active"))?;
        let file = workspace.path("practice.txt");
        anyhow::ensure!(
            workspace.permits_file(&file)
                && workspace.permits_file(&workspace.path("recovery/latest.json"))
                && workspace.permits_file(&workspace.path("recovery/previous.json"))
                && self
                    .current_buffer()
                    .file
                    .as_deref()
                    .is_some_and(|current| { same_file_path(Path::new(current), &file) }),
            "practice recovery path is outside the owned workspace"
        );
        Ok((file, SessionStore::new(workspace.path("recovery"))))
    }

    fn learn_recovery_snapshot(&mut self) -> anyhow::Result<()> {
        let (file, store) = self.learn_recovery_paths()?;
        anyhow::ensure!(
            self.learn_session
                .as_ref()
                .is_some_and(|session| { session.step == PracticeStep::RecoverySnapshot })
                && self.current_buffer().is_dirty()
                && self.current_buffer().contents() == RECOVERY_RESULT,
            "first change TODO to DONE without saving"
        );
        let disk = std::fs::read_to_string(&file)?;
        anyhow::ensure!(
            disk == RECOVERY_CONTENTS,
            "practice file changed on disk; use :tutorial restart"
        );
        self.sync_to_window();
        let (saved, _) = Self::capture_recovery_buffer(
            self.current_buffer(),
            0,
            (self.cx, self.vtop + self.cy, self.vtop),
            true,
        );
        let mut windows = WindowManager::new(0, (self.size.0.into(), self.size.1.into()));
        if let Some(window) = windows.active_window_mut() {
            window.cx = self.cx;
            window.cy = self.cy;
            window.vtop = self.vtop;
            window.wrap = self.wrap;
        }
        // Construct only the owned state. Capturing the whole editor and then
        // deleting unrelated fields could leak user text into a practice file.
        let mut snapshot = SessionSnapshot {
            version: SESSION_SCHEMA_VERSION,
            generation: 0,
            cwd: file
                .parent()
                .expect("owned file has a parent")
                .to_string_lossy()
                .into_owned(),
            saved_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)?
                .as_millis()
                .try_into()?,
            buffers: vec![saved],
            current_buffer_index: 0,
            window_layout: windows.snapshot(),
            panels: Default::default(),
            registers: Default::default(),
            jumps: Vec::new(),
            jump_index: 0,
            window_jumps: Vec::new(),
            local_marks: Vec::new(),
            global_marks: Vec::new(),
            special_marks: Vec::new(),
            last_visual_selections: Vec::new(),
            agent_transcript: None,
            agent_conversation: None,
            inline_history: Default::default(),
            legacy_agent_workspace: None,
            agent_session_resumable: false,
            plugin_extensions: Default::default(),
            legacy_extensions: Default::default(),
        };
        store.write(&mut snapshot)?;
        // Do not discard the in-memory edit unless its durable copy can load.
        self.validate_learn_recovery_snapshot(&store.load()?, &file)?;
        self.replace_learn_recovery_buffer(Buffer::new(
            Some(file.to_string_lossy().into_owned()),
            disk,
        ));
        self.learn_session
            .as_mut()
            .unwrap()
            .recovery
            .as_mut()
            .unwrap()
            .saved = true;
        Ok(())
    }

    fn validate_learn_recovery_snapshot(
        &self,
        snapshot: &SessionSnapshot,
        file: &Path,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            snapshot.buffers.len() == 1
                && snapshot.current_buffer_index == 0
                && snapshot.buffers[0].index == 0
                && snapshot.buffers[0]
                    .path
                    .as_deref()
                    .is_some_and(|path| same_file_path(Path::new(path), file))
                && snapshot.buffers[0].contents == RECOVERY_RESULT
                && snapshot.buffers[0].saved_contents.as_deref() == Some(RECOVERY_CONTENTS)
                && snapshot.buffers[0].dirty,
            "snapshot does not contain the owned practice edit; use :tutorial restart"
        );
        Ok(())
    }

    fn restore_learn_recovery(&mut self) -> anyhow::Result<()> {
        let (file, store) = self.learn_recovery_paths()?;
        anyhow::ensure!(
            self.learn_session
                .as_ref()
                .is_some_and(|session| { session.step == PracticeStep::RecoveryRestore }),
            "take the practice snapshot first"
        );
        let snapshot = store.load()?;
        self.validate_learn_recovery_snapshot(&snapshot, &file)?;
        anyhow::ensure!(
            std::fs::read_to_string(&file)? == RECOVERY_CONTENTS,
            "practice file changed on disk; use :tutorial restart"
        );
        // This is the production --resume buffer constructor. The full editor
        // restore deliberately is not called: it also restores plugin storage,
        // agent conversations, and the user's window layout.
        let recovered = Self::buffers_from_session_snapshot(&snapshot).remove(0);
        self.replace_learn_recovery_buffer(recovered);
        self.learn_session
            .as_mut()
            .unwrap()
            .recovery
            .as_mut()
            .unwrap()
            .restored = true;
        Ok(())
    }

    fn replace_learn_recovery_buffer(&mut self, replacement: Buffer) {
        let index = self.buffer_manager.active_index();
        let old_id = self.current_buffer().id();
        let new_id = replacement.id();
        let position = replacement.pos;
        let viewport_top = replacement.vtop;
        self.buffer_manager[index] = replacement;
        self.learn_session.as_mut().unwrap().practice_buffer_id = new_id;
        self.lsp_coordinator.forget_buffer(old_id);
        self.local_marks.remove(&old_id);
        self.special_marks.retain(|(id, _), _| *id != old_id);
        self.last_visual_selections.remove(&old_id);
        self.forget_jumps_for_buffer(old_id);
        self.last_semantic_change = None;
        self.pending_semantic_change = None;
        self.mode = Mode::Normal;
        self.selection = None;
        self.selection_start = None;
        self.cx = position.0;
        self.cy = position.1;
        self.vtop = viewport_top;
        self.vleft = 0;
        self.skipcol = 0;
        self.highlight_cache.clear();
        self.layout_cache.borrow_mut().clear();
        self.sync_to_window();
        self.check_bounds();
        self.force_full_redraw = true;
    }

    pub(super) fn intercept_learn_recovery_action(
        &mut self,
        action: &Action,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<bool> {
        if !matches!(
            action,
            Action::SnapshotLearnRecovery | Action::RestoreLearnRecovery
        ) {
            return Ok(false);
        }
        let result = match action {
            Action::SnapshotLearnRecovery => self.learn_recovery_snapshot(),
            Action::RestoreLearnRecovery => self.restore_learn_recovery(),
            _ => unreachable!(),
        };
        match result {
            Ok(()) => {
                self.set_quiet_message(None);
                self.observe_learn_action(action, buffer)?;
            }
            Err(error) => {
                self.current_dialog = Some(Box::new(
                    HoverInfo::new(
                        self,
                        format!("Recovery could not continue.\n\n{error:#}\n\nYour current buffer has not been discarded. Close this report to retry, or use :tutorial restart to recreate the owned workspace."),
                        crate::ui::HoverInfoFormat::Plaintext,
                        Vec::new(),
                    ).with_label("Practice recovery"),
                ));
            }
        }
        self.render(buffer)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn learn_recovery_roundtrip_preserves_unsaved_history_and_real_store() {
        let config = Config::default();
        let client = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            client,
            100,
            30,
            config,
            Theme::default(),
            vec![Buffer::new(None, "private original buffer".into())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        let original_id = editor.current_buffer().id();
        let directory = tempfile::tempdir().unwrap();
        let real_store = SessionStore::new(directory.path());
        editor.set_session_store(real_store.clone());
        let mut buffer = RenderBuffer::new(100, 30, &Style::default());
        let mut runtime = Runtime::new();
        editor
            .start_learn_lesson(Lesson::KeepYourPlace, &mut buffer, &mut runtime)
            .await
            .unwrap();
        editor.test_finish_session_snapshot();
        let real_before = std::fs::read(real_store.latest_path()).unwrap();
        assert!(editor.learn_recovery_snapshot().is_err());
        for action in editor.handle_command("%s/TODO/DONE/", &runtime) {
            editor
                .execute(&action, &mut buffer, &mut runtime)
                .await
                .unwrap();
        }
        assert_eq!(
            editor.learn_session.as_ref().unwrap().step,
            PracticeStep::RecoverySnapshot
        );
        editor
            .execute(&Action::SnapshotLearnRecovery, &mut buffer, &mut runtime)
            .await
            .unwrap();
        let (file, store) = editor.learn_recovery_paths().unwrap();
        assert_eq!(editor.current_buffer().contents(), RECOVERY_CONTENTS);
        assert!(!editor.current_buffer().is_dirty());
        let snapshot = std::fs::read_to_string(store.latest_path()).unwrap();
        assert!(!snapshot.contains("private original buffer"));
        assert_eq!(
            std::fs::read(real_store.latest_path()).unwrap(),
            real_before
        );
        editor
            .execute(&Action::RestoreLearnRecovery, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(editor.current_buffer().contents(), RECOVERY_RESULT);
        assert!(editor.current_buffer().is_dirty());
        editor
            .execute(&Action::Undo, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(editor.current_buffer().contents(), RECOVERY_CONTENTS);
        editor
            .execute(&Action::Redo, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(
            editor.learn_session.as_ref().unwrap().step,
            PracticeStep::Complete
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), RECOVERY_CONTENTS);
        editor
            .finish_learn_lesson(&mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(editor.current_buffer().id(), original_id);
        assert_eq!(
            editor.current_buffer().contents(),
            "private original buffer"
        );
        assert!(!file.exists());
    }
}
