//! Capture the editor view before accepting a quit that preserves scratchpads.

use serde::Deserialize as _;

use super::{
    plugin_storage_value_for_workspace, session_buffer_position, Buffer, Editor,
    EditorStateSnapshot,
};
use crate::session::{SessionBufferSnapshot, SessionSnapshot, WorkspaceSession};
use crate::undo::{CursorSnapshot, UndoHistory};

impl Editor {
    /// Enables normal quit persistence and applies a workspace's saved view to its
    /// reconstructed buffers. Crash recovery is configured separately.
    pub fn set_workspace_session(
        &mut self,
        session: WorkspaceSession,
        snapshot: Option<&SessionSnapshot>,
    ) {
        self.workspace_session = Some(session);
        if let Some(snapshot) = snapshot.filter(|snapshot| !snapshot.buffers.is_empty()) {
            self.restore_session_layout(snapshot);
            self.recompute_window_cursor_goals();
            self.sync_with_window();
            self.check_bounds();
        }
    }

    pub(super) fn is_workspace_scratchpad(&self, buffer: &Buffer) -> bool {
        buffer.is_unnamed() && self.is_workspace_document(buffer)
    }

    fn is_workspace_document(&self, buffer: &Buffer) -> bool {
        !self.scratch_buffers.contains_key(&buffer.id())
            && !self
                .tutorial_controller
                .as_ref()
                .is_some_and(|tutorial| tutorial.practice_buffer_id == buffer.id())
    }

    pub(super) fn persist_workspace_on_quit(&mut self) -> bool {
        let Some(session) = &self.workspace_session else {
            return true;
        };
        let owns_workspace = session.owns_workspace();
        if !owns_workspace
            && !self.buffer_manager.iter().any(|buffer| {
                self.is_workspace_scratchpad(buffer) && (buffer.is_dirty() || !buffer.is_blank())
            })
        {
            return true;
        }

        let cwd = session.workspace().to_string_lossy().into_owned();
        self.sync_to_window();
        let positions = self.session_buffer_positions();
        let buffers = self
            .buffer_manager
            .iter()
            .enumerate()
            .filter(|(_, buffer)| self.is_workspace_document(buffer))
            .map(|(index, buffer)| {
                let (cursor_x, cursor_y, viewport_top) =
                    session_buffer_position(buffer, index, &positions);
                let dirty = buffer.is_dirty();
                let discard_edits = buffer.file.is_some() && dirty;
                let mut undo_history = UndoHistory::default();
                if !discard_edits {
                    undo_history = buffer.undo_history.clone();
                    undo_history.commit_transaction(CursorSnapshot::new(
                        cursor_x,
                        cursor_y,
                        viewport_top,
                    ));
                }
                SessionBufferSnapshot {
                    index,
                    path: buffer.file.clone(),
                    directory: buffer
                        .directory
                        .as_ref()
                        .map(|directory| directory.path.clone()),
                    contents: if buffer.is_unnamed()
                        || (!discard_edits && undo_history.node_count() > 1)
                    {
                        buffer.contents()
                    } else {
                        String::new()
                    },
                    saved_contents: if buffer.is_unnamed() {
                        buffer
                            .saved_contents_snapshot()
                            .map(|contents| contents.to_string())
                    } else {
                        None
                    },
                    dirty,
                    revision: buffer.revision(),
                    cursor_x,
                    cursor_y,
                    viewport_top,
                    undo_history,
                    disk_contents: None,
                }
            })
            .collect::<Vec<_>>();
        if buffers.is_empty() {
            return true;
        }
        let snapshot = SessionSnapshot::editor_view(
            cwd,
            buffers,
            self.buffer_manager.active_index(),
            self.window_manager.snapshot(),
            self.panel_manager.snapshot(usize::from(self.size.0)),
        );
        let session = self
            .workspace_session
            .as_ref()
            .expect("workspace session is configured");
        match session.save(snapshot, || self.legacy_workspace_snapshot(session)) {
            Ok(()) => true,
            Err(error) => {
                self.set_routine_error(Some(format!("Could not save workspace session: {error}")));
                false
            }
        }
    }

    // Explicit file launches can add scratchpads to an existing legacy session
    // before any argument-free launch has migrated it to the core store.
    fn legacy_workspace_snapshot(&self, session: &WorkspaceSession) -> Option<SessionSnapshot> {
        let saved = plugin_storage_value_for_workspace(
            &self.preferences,
            "session_restore",
            "latest",
            session.workspace(),
        )?;
        let saved = EditorStateSnapshot::deserialize(saved).ok()?;
        if !matches!(saved.version, 1 | 2)
            || std::path::Path::new(&saved.cwd) != session.workspace()
        {
            return None;
        }
        let buffers = saved
            .buffers
            .into_iter()
            .filter(|buffer| !buffer.dirty)
            .map(|buffer| SessionBufferSnapshot {
                index: buffer.index,
                path: Some(buffer.path),
                directory: None,
                contents: String::new(),
                saved_contents: None,
                dirty: false,
                revision: 0,
                cursor_x: buffer.cursor.x,
                cursor_y: buffer.cursor.y,
                viewport_top: buffer.viewport_top,
                undo_history: UndoHistory::default(),
                disk_contents: None,
            })
            .collect();
        Some(SessionSnapshot::editor_view(
            saved.cwd,
            buffers,
            saved.current_buffer_index,
            saved.window_layout,
            saved.panels,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Action, RenderBuffer, Runtime, Style};
    use super::*;
    use crate::{config::Config, lsp::LspManager, theme::Theme};

    async fn quit(editor: &mut Editor, forced: bool) -> bool {
        let mut frame = RenderBuffer::new(100, 30, &Style::default());
        editor
            .execute(&Action::Quit(forced), &mut frame, &mut Runtime::new())
            .await
            .unwrap()
    }

    fn editor(buffers: Vec<Buffer>) -> Editor {
        let config = Config::default();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let mut editor =
            Editor::with_size(lsp, 100, 30, config, Theme::default(), buffers).unwrap();
        editor.test_disable_terminal_output();
        editor
    }

    #[tokio::test]
    async fn quit_restores_scratchpad_split_and_undo_history() {
        use crate::editor::Mode;

        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        let mut session = WorkspaceSession::new(temp.path(), workspace.clone());
        assert!(session.claim_and_load().unwrap().is_none());
        let mut source = editor(vec![Buffer::new(None, "first scratch\n".into())]);
        source.set_workspace_session(session, None);
        for action in [
            Action::SplitVerticalNewBuffer,
            Action::EnterMode(Mode::Insert),
            Action::InsertCharAtCursorPos('a'),
            Action::EnterMode(Mode::Normal),
        ] {
            source.test_execute_production_action(action).await.unwrap();
        }
        assert!(source.current_buffer().is_dirty());
        assert!(quit(&mut source, false).await);
        drop(source);

        let mut session = WorkspaceSession::new(temp.path(), workspace);
        let saved = session.claim_and_load().unwrap().unwrap();
        let mut restored = editor(Editor::buffers_from_session_snapshot(&saved));
        restored.set_workspace_session(session, Some(&saved));
        assert_eq!(restored.test_window_count(), 2);
        assert_eq!(restored.buffer_manager[0].contents(), "first scratch\n");
        assert_eq!(restored.current_buffer().contents(), "a\n");
        restored
            .test_execute_production_action(Action::Undo)
            .await
            .unwrap();
        assert!(restored.current_buffer().is_blank());
    }

    #[tokio::test]
    async fn forced_quit_reopens_disk_text_and_keeps_scratchpad() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        let file = workspace.join("named.txt");
        std::fs::write(&file, "on disk\n").unwrap();
        let mut named = Buffer::new(Some(file.to_str().unwrap().into()), "on disk\n".into());
        named.insert(0, 0, 'X');
        let mut source = editor(vec![named, Buffer::new(None, "keep me\n".into())]);
        let mut session = WorkspaceSession::new(temp.path(), workspace.clone());
        session.claim_and_load().unwrap();
        source.set_workspace_session(session, None);
        assert!(!quit(&mut source, false).await);
        assert!(quit(&mut source, true).await);
        drop(source);

        std::fs::write(&file, "changed on disk\n").unwrap();
        let mut session = WorkspaceSession::new(temp.path(), workspace);
        let saved = session.claim_and_load().unwrap().unwrap();
        let buffers = Editor::buffers_from_session_snapshot(&saved);
        assert_eq!(buffers[0].contents(), "changed on disk\n");
        assert!(!buffers[0].is_dirty());
        assert_eq!(buffers[1].contents(), "keep me\n");
        assert_eq!(std::fs::read_to_string(file).unwrap(), "changed on disk\n");
    }

    #[tokio::test]
    async fn failed_session_write_keeps_the_editor_and_text_open() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        std::fs::write(workspace.join("workspace-sessions"), "not a directory").unwrap();
        let mut scratch = Buffer::new(None, String::new());
        scratch.insert(0, 0, 'X');
        let mut source = editor(vec![scratch]);
        source.set_workspace_session(WorkspaceSession::new(temp.path(), workspace), None);
        for forced in [false, true] {
            assert!(!quit(&mut source, forced).await);
            assert_eq!(source.current_buffer().contents(), "X\n");
        }
    }

    #[tokio::test]
    async fn secondary_editor_can_add_scratchpads_without_replacing_saved_view() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        let mut session = WorkspaceSession::new(temp.path(), workspace.clone());
        session.claim_and_load().unwrap();
        let mut primary = editor(vec![Buffer::new(None, "primary scratch\n".into())]);
        primary.set_workspace_session(session, None);

        let mut session = WorkspaceSession::new(temp.path(), workspace.clone());
        assert!(session.claim_and_load().unwrap().is_none());
        let mut secondary = editor(vec![Buffer::new(None, "secondary scratch\n".into())]);
        secondary.set_workspace_session(session, None);
        assert!(!quit(&mut secondary, false).await);
        assert!(quit(&mut primary, false).await);
        drop(primary);
        assert!(
            quit(&mut secondary, false).await,
            "secondary editor quit was blocked: {:?}",
            secondary.last_error
        );
        drop(secondary);

        let mut session = WorkspaceSession::new(temp.path(), workspace);
        let saved = session.claim_and_load().unwrap().unwrap();
        assert_eq!(saved.buffers.len(), 2);
        assert_eq!(saved.current_buffer_index, saved.buffers[0].index);
        assert_eq!(saved.buffers[0].contents, "primary scratch\n");
        assert_eq!(saved.buffers[1].contents, "secondary scratch\n");
    }
}
