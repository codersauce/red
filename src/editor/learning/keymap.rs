//! A real TOML keymap fragment, applied only to the temporary practice binding.

use super::*;
use serde::Deserialize;

pub(super) struct LearnKeymapState {
    original_f6: Option<KeyAction>,
    installed: bool,
}

impl LearnKeymapState {
    pub fn new(editor: &Editor) -> Self {
        Self {
            original_f6: editor.config.keys.normal.get("F6").cloned(),
            installed: false,
        }
    }

    pub fn restore(self, editor: &mut Editor) {
        if let Some(original) = self.original_f6 {
            editor.config.keys.normal.insert("F6".into(), original);
        } else {
            editor.config.keys.normal.remove("F6");
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PracticeConfig {
    keys: PracticeKeys,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PracticeKeys {
    normal: HashMap<String, KeyAction>,
}

fn practice_binding(contents: &str) -> anyhow::Result<KeyAction> {
    let fragment: PracticeConfig = toml::from_str(contents)?;
    let expected = KeyAction::Single(Action::ToggleWrap);
    anyhow::ensure!(
        fragment.keys.normal.len() == 1 && fragment.keys.normal.get("F6") == Some(&expected),
        "set only the practice F6 binding to ToggleWrap"
    );
    let mut parsed =
        Config::from_toml_with_overrides(crate::assets::DEFAULT_CONFIG, &[contents.to_owned()])?;
    parsed
        .keys
        .normal
        .remove("F6")
        .ok_or_else(|| anyhow::anyhow!("the practice binding was not loaded"))
}

impl Editor {
    pub(super) fn learn_keymap_installed(&self) -> bool {
        self.learn_session
            .as_ref()
            .and_then(|session| session.keymap.as_ref())
            .is_some_and(|state| state.installed)
            && self.config.keys.normal.get("F6") == Some(&KeyAction::Single(Action::ToggleWrap))
    }

    #[inline(never)]
    pub(super) fn intercept_learn_keymap_action<'a>(
        &'a mut self,
        action: &'a Action,
        buffer: &'a mut RenderBuffer,
    ) -> BoxFuture<'a, anyhow::Result<bool>> {
        Box::pin(async move {
            if !matches!(action, Action::Save)
                || self
                    .learn_session
                    .as_ref()
                    .is_none_or(|session| session.keymap.is_none())
            {
                return Ok(false);
            }
            let permitted = self.current_buffer().file.as_deref().is_some_and(|file| {
                self.learn_session
                    .as_ref()
                    .and_then(|session| session.workspace.as_ref())
                    .is_some_and(|workspace| {
                        workspace.permits_file(Path::new(file))
                            && same_file_path(Path::new(file), &workspace.path("keymap.toml"))
                    })
            });
            let result = if permitted {
                practice_binding(&self.current_buffer().contents())
            } else {
                Err(anyhow::anyhow!(
                    "practice config is outside the owned workspace"
                ))
            };
            match result {
                Ok(binding) => {
                    let resume_insert = self.commit_active_transaction_before_save();
                    let saved = self.current_buffer_mut().save();
                    self.resume_insert_transaction_after_save(resume_insert);
                    match saved {
                        Ok(_) => {
                            self.config.keys.normal.insert("F6".into(), binding);
                            self.learn_session
                                .as_mut()
                                .and_then(|session| session.keymap.as_mut())
                                .expect("keymap lesson was checked")
                                .installed = true;
                            self.set_notification_message(Severity::Success, Some("practice config saved; F6 now toggles wrapping for this lesson".into()));
                            self.observe_learn_action(action, buffer)?;
                        }
                        Err(error) => self.set_notification_message(
                            Severity::Error,
                            Some(format!("practice config: {error}")),
                        ),
                    }
                }
                Err(error) => self.set_notification_message(
                    Severity::Error,
                    Some(format!("practice config: {error:#}")),
                ),
            }
            self.render(buffer)?;
            Ok(true)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learn_keymap_accepts_only_the_intended_override() {
        assert_eq!(
            practice_binding("[keys.normal]\n\"F6\" = \"ToggleWrap\"\n").unwrap(),
            KeyAction::Single(Action::ToggleWrap)
        );
        for invalid in [
            "[keys.normal]\n\"F6\" = \"MoveRight\"\n",
            "[keys.normal]\n\"F6\" = \"ToggleWrap\"\n\"q\" = { Quit = true }\n",
            "theme = \"red.json\"\n[keys.normal]\n\"F6\" = \"ToggleWrap\"\n",
            "invalid [",
        ] {
            assert!(practice_binding(invalid).is_err());
        }
    }

    #[tokio::test]
    async fn learn_keymap_restores_an_existing_binding() {
        let mut config = Config::default();
        let original = KeyAction::Single(Action::MoveLeft);
        config.keys.normal.insert("F6".into(), original.clone());
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
            .start_learn_lesson(Lesson::DiscoverYourKeymap, &mut buffer, &mut runtime)
            .await
            .unwrap();
        editor
            .execute(&Action::Save, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert!(!editor.learn_keymap_installed());
        assert_eq!(editor.config.keys.normal.get("F6"), Some(&original));
        let path = editor.current_buffer().file.clone().unwrap();
        let replacement =
            crate::learn::personalization::KEYMAP_CONTENTS.replace("MoveRight", "ToggleWrap");
        let practice = Buffer::new(Some(path.clone()), replacement.clone());
        let id = practice.id();
        *editor.current_buffer_mut() = practice;
        let session = editor.learn_session.as_mut().unwrap();
        session.practice_buffer_id = id;
        session.step = PracticeStep::KeymapEdit;
        editor
            .execute(&Action::Save, &mut buffer, &mut runtime)
            .await
            .unwrap();
        assert!(editor.learn_keymap_installed());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), replacement);
        editor
            .finish_learn_lesson(&mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(editor.config.keys.normal.get("F6"), Some(&original));
        assert!(!Path::new(&path).exists());
        assert_eq!(editor.current_buffer().contents(), "original");
    }
}
