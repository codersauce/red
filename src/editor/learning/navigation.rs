//! Real file navigation confined to the lesson's owned fixture set.

use super::*;
use crate::learn::navigation as fixture;
use crate::ui::ScopedProjectSearch;

impl Editor {
    fn learn_navigation_failure(
        &mut self,
        error: impl std::fmt::Display,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<bool> {
        self.set_notification_message(
            Severity::Error,
            Some(format!(
                "practice navigation: {error}; use :tutorial restart to reset the files"
            )),
        );
        self.render(buffer)?;
        Ok(true)
    }

    pub(super) fn learn_navigation_path(&self, path: &str) -> Option<PathBuf> {
        let session = self.learn_session.as_ref()?;
        if !session.lesson.is_navigation_practice() {
            return None;
        }
        let workspace = session.workspace.as_ref()?;
        let requested = Path::new(path);
        let candidate = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            workspace.root().join(requested)
        };
        if !workspace.permits_file(&candidate) || !candidate.is_file() {
            return None;
        }
        fixture::FILES
            .iter()
            .any(|(name, _)| same_file_path(&candidate, &workspace.path(name)))
            .then(|| candidate.canonicalize().ok())
            .flatten()
    }

    pub(super) fn learn_navigation_file_is(&self, name: &str) -> bool {
        self.learn_navigation_buffer_is(self.current_buffer(), name)
    }

    fn learn_navigation_buffer_is(&self, candidate: &Buffer, name: &str) -> bool {
        let Some(expected) = fixture::FILES
            .iter()
            .find_map(|(path, text)| (*path == name).then_some(*text))
        else {
            return false;
        };
        candidate
            .file
            .as_deref()
            .and_then(|path| self.learn_navigation_path(path))
            .is_some_and(|path| {
                self.learn_session
                    .as_ref()
                    .and_then(|session| session.workspace.as_ref())
                    .is_some_and(|workspace| same_file_path(&path, &workspace.path(name)))
            })
            && candidate.contents() == expected
    }

    pub(super) fn learn_workspace_pair_visible(&self) -> bool {
        if self
            .learn_session
            .as_ref()
            .is_none_or(|session| session.lesson != Lesson::ArrangeYourWorkspace)
            || self.window_manager.window_count() != 2
        {
            return false;
        }
        let windows = self.window_manager.windows();
        ["README.md", "src/score.hk"].into_iter().all(|name| {
            windows.iter().any(|window| {
                self.learn_navigation_buffer_is(&self.buffer_manager[window.buffer_index], name)
            })
        })
    }

    #[inline(never)]
    pub(super) fn intercept_learn_navigation_action<'a>(
        &'a mut self,
        action: &'a Action,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> BoxFuture<'a, anyhow::Result<bool>> {
        Box::pin(async move {
            let before = self.event_snapshot();
            match action {
                Action::NextBuffer | Action::PreviousBuffer => {
                    // The real buffer manager also contains the suspended user
                    // buffers. Cycle only the owned, already-open fixture files.
                    let indices = self
                        .buffer_manager
                        .iter()
                        .enumerate()
                        .filter(|(_, candidate)| {
                            candidate
                                .file
                                .as_deref()
                                .is_some_and(|path| self.learn_navigation_path(path).is_some())
                        })
                        .map(|(index, _)| index)
                        .collect::<Vec<_>>();
                    let Some(position) = indices
                        .iter()
                        .position(|index| *index == self.buffer_manager.active_index())
                    else {
                        return Ok(true);
                    };
                    let next = if matches!(action, Action::NextBuffer) {
                        (position + 1) % indices.len()
                    } else {
                        (position + indices.len() - 1) % indices.len()
                    };
                    self.set_current_buffer(buffer, indices[next]).await?;
                }
                Action::PluginCommand(name) if name == "ProjectSearch" => {
                    let root = self
                        .learn_session
                        .as_ref()
                        .and_then(|session| session.workspace.as_ref())
                        .expect("navigation workspace was checked")
                        .root()
                        .to_path_buf();
                    let files = fixture::FILES
                        .iter()
                        .map(|(name, _)| PathBuf::from(name))
                        .collect();
                    self.release_current_dialog_callbacks(runtime);
                    let search = match ScopedProjectSearch::new(self, root, files) {
                        Ok(search) => search,
                        Err(error) => return self.learn_navigation_failure(error, buffer),
                    };
                    self.current_dialog = Some(Box::new(search));
                }
                Action::OpenLocation(location, plugin::OpenLocationTarget::Current) => {
                    let Some(path) = self.learn_navigation_path(&location.path) else {
                        return Ok(true);
                    };
                    let previous = self.current_jump_entry();
                    let (index, _, _) = match self
                        .load_or_reuse_file_buffer(&path.to_string_lossy())
                        .await
                    {
                        Ok(opened) => opened,
                        Err(error) => return self.learn_navigation_failure(error, buffer),
                    };
                    let Some(line) = self.buffer_manager[index].get(location.line) else {
                        return Ok(true);
                    };
                    let line = line.trim_end_matches('\n');
                    if location.column > line.len() || !line.is_char_boundary(location.column) {
                        self.set_quiet_message(Some("search result is stale; search again".into()));
                        self.render(buffer)?;
                        return Ok(true);
                    }
                    let character = line[..location.column].chars().count();
                    self.set_current_buffer(buffer, index).await?;
                    self.move_to_text_position(TextPosition::new(location.line, character));
                    self.check_bounds();
                    self.sync_to_window();
                    self.save_to_history(previous);
                }
                Action::FilePicker => {
                    let root = self
                        .learn_session
                        .as_ref()
                        .and_then(|session| session.workspace.as_ref())
                        .expect("navigation workspace was checked")
                        .root()
                        .to_path_buf();
                    self.release_current_dialog_callbacks(runtime);
                    let picker = match FilePicker::new_scoped(self, root) {
                        Ok(picker) => picker,
                        Err(error) => return self.learn_navigation_failure(error, buffer),
                    };
                    self.current_dialog = Some(Box::new(picker));
                }
                Action::OpenFile(path) => {
                    let Some(path) = self.learn_navigation_path(path) else {
                        self.set_quiet_message(Some(
                            "choose a file inside the practice project".into(),
                        ));
                        self.render(buffer)?;
                        return Ok(true);
                    };
                    // Use the production loader and buffer-switching behavior,
                    // without notifying user plugins about a disposable file.
                    let (index, _, _) = match self
                        .load_or_reuse_file_buffer(&path.to_string_lossy())
                        .await
                    {
                        Ok(opened) => opened,
                        Err(error) => return self.learn_navigation_failure(error, buffer),
                    };
                    self.set_current_buffer(buffer, index).await?;
                }
                _ => return Ok(false),
            }
            self.sync_to_window();
            self.notify_editor_event_changes(before, runtime, "LearnNavigation")
                .await?;
            self.observe_learn_action(action, buffer)?;
            self.render(buffer)?;
            Ok(true)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn learn_workspace_confines_buffers_and_restores_zoom_and_panels() {
        let config = Config::default();
        let client = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            client,
            140,
            38,
            config,
            Theme::default(),
            vec![
                Buffer::new(None, "original one".into()),
                Buffer::new(None, "original two".into()),
            ],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        let mut buffer = RenderBuffer::new(140, 38, &Style::default());
        let mut runtime = Runtime::new();
        editor.window_manager.split_vertical(1).unwrap();
        editor.sync_with_window();
        let original_windows = editor
            .window_manager
            .windows()
            .iter()
            .map(|window| window.id)
            .collect::<Vec<_>>();
        editor.test_create_panel("original-pane", plugin::PanelConfig::default());
        assert!(editor.test_focus_panel("original-pane"));
        editor.toggle_pane_zoom();
        assert!(
            matches!(&editor.zoomed_pane, Some(FocusTarget::Panel(id)) if id == "original-pane")
        );
        editor
            .start_learn_lesson(Lesson::ArrangeYourWorkspace, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert!(!editor.panel_manager.is_visible("original-pane"));
        editor
            .execute(&Action::NextBuffer, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert!(editor.learn_navigation_file_is("README.md"));
        editor
            .execute(&Action::SplitVertical, &mut buffer, &mut runtime)
            .await
            .unwrap();
        editor
            .execute(
                &Action::OpenFile("src/score.hk".into()),
                &mut buffer,
                &mut runtime,
            )
            .await
            .unwrap();
        assert!(editor.learn_workspace_pair_visible());
        assert_eq!(
            editor.learn_session.as_ref().unwrap().step,
            PracticeStep::WorkspaceFocus
        );
        editor
            .execute(&Action::NextWindow, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert!(editor.learn_navigation_file_is("README.md"));
        editor
            .execute(&Action::TogglePaneZoom, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(
            editor
                .window_manager
                .windows()
                .iter()
                .filter(|window| editor.window_manager.is_presented(window.id))
                .count(),
            1
        );
        assert_eq!(
            editor.learn_session.as_ref().unwrap().step,
            PracticeStep::WorkspaceRestore
        );
        editor
            .execute(&Action::TogglePaneZoom, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(
            editor.learn_session.as_ref().unwrap().step,
            PracticeStep::Complete
        );
        assert_eq!(
            editor
                .window_manager
                .windows()
                .iter()
                .filter(|window| editor.window_manager.is_presented(window.id))
                .count(),
            2
        );
        editor
            .finish_learn_lesson(&mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(
            editor
                .window_manager
                .windows()
                .iter()
                .map(|window| window.id)
                .collect::<Vec<_>>(),
            original_windows
        );
        assert_eq!(editor.buffer_manager.len(), 2);
        assert_eq!(editor.current_buffer().contents(), "original two");
        assert_eq!(editor.test_focused_panel_id(), Some("original-pane"));
        assert!(
            matches!(&editor.zoomed_pane, Some(FocusTarget::Panel(id)) if id == "original-pane")
        );
    }

    #[tokio::test]
    async fn learn_search_missing_fixture_is_recoverable() {
        let config = Config::default();
        let client = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(
            client,
            100,
            30,
            config,
            Theme::default(),
            vec![Buffer::new(None, "original".into())],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        let mut buffer = RenderBuffer::new(100, 30, &Style::default());
        let mut runtime = Runtime::new();
        editor
            .start_learn_lesson(Lesson::SearchTheProject, &mut buffer, &mut runtime)
            .await
            .unwrap();
        let missing = editor
            .learn_session
            .as_ref()
            .unwrap()
            .workspace
            .as_ref()
            .unwrap()
            .path("src/score.hk");
        std::fs::remove_file(missing).unwrap();
        editor
            .execute(
                &Action::PluginCommand("ProjectSearch".into()),
                &mut buffer,
                &mut runtime,
            )
            .await
            .unwrap();
        assert!(editor.current_dialog.is_none());
        assert!(editor
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains(":tutorial restart")));
        assert_eq!(
            editor.learn_session.as_ref().unwrap().step,
            PracticeStep::SearchOpen
        );
        editor
            .restart_learn_lesson(&mut buffer, &mut runtime)
            .await
            .unwrap();
        assert!(editor.learn_navigation_path("src/score.hk").is_some());
        editor
            .finish_learn_lesson(&mut buffer, &mut runtime)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn learn_file_navigation_uses_owned_files_and_restores_the_original_buffers() {
        let config = Config::default();
        let client = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut original = Buffer::new(None, "original unsaved text".into());
        original.dirty = true;
        let original_id = original.id();
        let mut editor =
            Editor::with_size(client, 140, 38, config, Theme::default(), vec![original]).unwrap();
        editor.test_disable_terminal_output();
        editor
            .preferences
            .record_picker_query("find_files", "user query")
            .unwrap();
        let mut buffer = RenderBuffer::new(140, 38, &Style::default());
        let mut runtime = Runtime::new();
        editor
            .start_learn_lesson(Lesson::OpenAFileByName, &mut buffer, &mut runtime)
            .await
            .unwrap();
        let root = editor
            .learn_session
            .as_ref()
            .unwrap()
            .workspace
            .as_ref()
            .unwrap()
            .root()
            .to_path_buf();
        let outside = tempfile::NamedTempFile::new().unwrap();
        assert!(editor
            .learn_navigation_path(outside.path().to_str().unwrap())
            .is_none());
        assert!(editor.learn_navigation_path("../outside.hk").is_none());
        editor
            .execute(
                &Action::OpenFile(outside.path().to_string_lossy().into_owned()),
                &mut buffer,
                &mut runtime,
            )
            .await
            .unwrap();
        assert!(editor.learn_navigation_file_is("README.md"));
        editor
            .execute(&Action::FilePicker, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(
            editor.current_dialog.as_ref().unwrap().shortcut_context(),
            "Files"
        );
        assert_eq!(
            editor.learn_session.as_ref().unwrap().step,
            PracticeStep::FilesSource
        );
        editor
            .execute(&Action::CloseDialog, &mut buffer, &mut runtime)
            .await
            .unwrap();
        editor
            .execute(
                &Action::OpenFile("tests/score.hk".into()),
                &mut buffer,
                &mut runtime,
            )
            .await
            .unwrap();
        assert_eq!(
            editor.learn_session.as_ref().unwrap().step,
            PracticeStep::FilesSource
        );
        for (file, step) in [
            ("src/score.hk", PracticeStep::FilesTests),
            ("tests/score.hk", PracticeStep::FilesReturn),
            ("README.md", PracticeStep::Complete),
        ] {
            editor
                .execute(&Action::OpenFile(file.into()), &mut buffer, &mut runtime)
                .await
                .unwrap();
            assert_eq!(editor.learn_session.as_ref().unwrap().step, step);
        }
        for &(name, text) in fixture::FILES {
            assert_eq!(std::fs::read_to_string(root.join(name)).unwrap(), text);
        }
        assert_eq!(editor.picker_history("find_files"), ["user query"]);
        editor
            .finish_learn_lesson(&mut buffer, &mut runtime)
            .await
            .unwrap();
        assert!(!root.exists());
        assert_eq!(editor.buffer_manager.len(), 1);
        assert_eq!(editor.current_buffer().id(), original_id);
        assert!(editor.current_buffer().is_dirty());
        assert_eq!(editor.current_buffer().contents(), "original unsaved text");
    }
}
