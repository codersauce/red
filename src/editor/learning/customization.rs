//! Editor-native personalization lessons with explicit persistence boundaries.

use super::*;
use crate::{assets::RuntimeAssetKind, ui::PickerItem};
use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};

pub(super) struct LearnThemeState {
    committed_theme: Theme,
    committed_name: String,
    pub previewed: bool,
    pub cancelled: bool,
    pub decided: bool,
}

impl LearnThemeState {
    pub fn new(editor: &Editor) -> Self {
        Self {
            committed_theme: editor.theme.clone(),
            committed_name: editor.config.theme.clone(),
            previewed: false,
            cancelled: false,
            decided: false,
        }
    }

    pub async fn restore(self, editor: &mut Editor, runtime: &mut Runtime) -> anyhow::Result<()> {
        editor.install_theme(self.committed_theme)?;
        editor
            .publish_learn_theme(&self.committed_name, false, runtime)
            .await
    }
}

impl Editor {
    async fn publish_learn_theme(
        &mut self,
        name: &str,
        persisted: bool,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        self.refresh_plugin_snapshots(runtime, false, false, true)?;
        self.plugin_registry
            .notify(
                runtime,
                "theme:changed",
                json!({"name":name,"persisted":persisted}),
            )
            .await
    }

    fn open_learn_theme_picker(&mut self, runtime: &mut Runtime) -> anyhow::Result<()> {
        let entries =
            crate::assets::list_runtime_assets(RuntimeAssetKind::Theme, &Config::config_dir())?;
        let initial = self
            .learn_session
            .as_ref()
            .and_then(|session| session.theme.as_ref())
            .expect("theme lesson was checked")
            .committed_name
            .clone();
        let items = entries
            .into_iter()
            .take(500)
            .map(|entry| PickerItem {
                id: entry.file.clone(),
                icon: None,
                label: entry.name.unwrap_or_else(|| entry.file.clone()),
                kind: Some("Theme".into()),
                annotation: Some(entry.file),
                detail: Some(entry.source.to_string()),
                data: Value::Null,
                matches: Vec::new(),
                detail_matches: Vec::new(),
                preview: None,
            })
            .collect::<Vec<_>>();
        anyhow::ensure!(!items.is_empty(), "no themes are available");
        let matcher = SkimMatcherV2::default();
        self.release_current_dialog_callbacks(runtime);
        self.current_dialog = Some(Box::new(
            Picker::builder()
                .title("Themes")
                .structured_items(items)
                .filter_action(move |item, query| {
                    if item.id.eq_ignore_ascii_case(query) {
                        Some(i64::MAX)
                    } else {
                        matcher
                            .fuzzy_match(&item.label, query)
                            .or_else(|| matcher.fuzzy_match(&item.id, query))
                    }
                })
                .initial_selection(initial)
                .placeholder("Filter themes")
                .status("Move to preview · Enter saves · Esc restores")
                .content_sized(90, 14)
                .change_action(Action::PreviewTheme)
                .cancel_action(|| Action::PluginCommand("LearnThemeCancel".into()))
                .select_action(Action::SetTheme)
                .build(self),
        ));
        Ok(())
    }

    #[inline(never)]
    pub(super) fn intercept_learn_customization_action<'a>(
        &'a mut self,
        action: &'a Action,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> BoxFuture<'a, anyhow::Result<bool>> {
        Box::pin(async move {
            let Some(session) = self
                .learn_session
                .as_ref()
                .filter(|session| session.theme.is_some())
            else {
                return Ok(false);
            };
            let deciding = session.step == PracticeStep::ThemeChoose;
            let result = match action {
                Action::PluginCommand(name) if name == "ThemeBrowser" => {
                    self.open_learn_theme_picker(runtime)
                }
                Action::PreviewTheme(name) => match self.apply_theme(name, false) {
                    Ok(()) => {
                        let state = self
                            .learn_session
                            .as_mut()
                            .and_then(|session| session.theme.as_mut())
                            .expect("theme lesson was checked");
                        state.previewed |= name != &state.committed_name;
                        self.publish_learn_theme(name, false, runtime).await
                    }
                    Err(error) => Err(error),
                },
                Action::SetTheme(name) => match self.apply_theme(name, true) {
                    Ok(()) => {
                        let state = self
                            .learn_session
                            .as_mut()
                            .and_then(|session| session.theme.as_mut())
                            .expect("theme lesson was checked");
                        state.committed_name = name.clone();
                        state.committed_theme = self.theme.clone();
                        state.decided |= deciding;
                        self.publish_learn_theme(name, true, runtime).await
                    }
                    Err(error) => Err(error),
                },
                Action::PluginCommand(name) if name == "LearnThemeCancel" => {
                    let state = self
                        .learn_session
                        .as_ref()
                        .and_then(|session| session.theme.as_ref())
                        .expect("theme lesson was checked");
                    let theme = state.committed_theme.clone();
                    let name = state.committed_name.clone();
                    self.install_theme(theme)?;
                    let state = self
                        .learn_session
                        .as_mut()
                        .and_then(|session| session.theme.as_mut())
                        .expect("theme lesson was checked");
                    state.cancelled |= state.previewed;
                    state.decided |= deciding;
                    self.publish_learn_theme(&name, false, runtime).await
                }
                _ => return Ok(false),
            };
            if let Err(error) = result {
                self.set_notification_message(
                    Severity::Error,
                    Some(format!("theme practice: {error:#}")),
                );
            } else {
                self.observe_learn_action(action, buffer)?;
            }
            self.render(buffer)?;
            Ok(true)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn learn_theme_cancel_and_exit_restore_the_snapshot() {
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
        let original = editor.config.theme.clone();
        let colors = editor.theme.colors.clone();
        editor
            .start_learn_lesson(Lesson::ChooseATheme, &mut buffer, &mut runtime)
            .await
            .unwrap();
        editor
            .execute(
                &Action::PluginCommand("ThemeBrowser".into()),
                &mut buffer,
                &mut runtime,
            )
            .await
            .unwrap();
        let alternate = if original == "lackluster.json" {
            "kanso.json"
        } else {
            "lackluster.json"
        };
        editor
            .execute(
                &Action::PreviewTheme(alternate.into()),
                &mut buffer,
                &mut runtime,
            )
            .await
            .unwrap();
        assert_eq!(
            editor.learn_session.as_ref().unwrap().step,
            PracticeStep::ThemeCancel
        );
        assert_eq!(editor.config.theme, original);
        assert_ne!(editor.theme.colors, colors);
        editor
            .execute(
                &Action::PluginCommand("LearnThemeCancel".into()),
                &mut buffer,
                &mut runtime,
            )
            .await
            .unwrap();
        assert_eq!(editor.theme.colors, colors);
        assert_eq!(
            editor.learn_session.as_ref().unwrap().step,
            PracticeStep::ThemeChoose
        );
        editor
            .execute(
                &Action::PreviewTheme(alternate.into()),
                &mut buffer,
                &mut runtime,
            )
            .await
            .unwrap();
        editor
            .finish_learn_lesson(&mut buffer, &mut runtime)
            .await
            .unwrap();
        assert_eq!(editor.theme.colors, colors);
        assert_eq!(editor.config.theme, original);
        assert_eq!(editor.current_buffer().contents(), "original");
    }
}
