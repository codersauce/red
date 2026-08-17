//! Real file navigation confined to the lesson's owned fixture set.

use super::*;
use crate::learn::navigation as fixture;

impl Editor {
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
        let Some(expected) = fixture::FILES
            .iter()
            .find_map(|(path, text)| (*path == name).then_some(*text))
        else {
            return false;
        };
        self.current_buffer()
            .file
            .as_deref()
            .and_then(|path| self.learn_navigation_path(path))
            .is_some_and(|path| {
                self.learn_session
                    .as_ref()
                    .and_then(|session| session.workspace.as_ref())
                    .is_some_and(|workspace| same_file_path(&path, &workspace.path(name)))
            })
            && self.current_buffer().contents() == expected
    }

    #[inline(never)]
    pub(super) fn intercept_learn_navigation_action<'a>(
        &'a mut self,
        action: &'a Action,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> BoxFuture<'a, anyhow::Result<bool>> {
        Box::pin(async move {
            match action {
                Action::FilePicker => {
                    let root = self
                        .learn_session
                        .as_ref()
                        .and_then(|session| session.workspace.as_ref())
                        .expect("navigation workspace was checked")
                        .root()
                        .to_path_buf();
                    self.release_current_dialog_callbacks(runtime);
                    self.current_dialog = Some(Box::new(FilePicker::new_scoped(self, root)?));
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
                    let (index, _, _) = self
                        .load_or_reuse_file_buffer(&path.to_string_lossy())
                        .await?;
                    self.set_current_buffer(buffer, index).await?;
                }
                _ => return Ok(false),
            }
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
