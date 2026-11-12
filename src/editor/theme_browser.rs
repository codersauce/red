//! Theme selection and appearance preferences, owned by the editor in both session modes.

use std::path::Path;

use anyhow::Context;
use futures::future::BoxFuture;
use serde_json::json;

use super::{Editor, RenderBuffer, Runtime, ThemeBrowserAction};
use crate::{
    assets::{self, RuntimeAssetKind},
    config::{AppearanceConfig, AppearanceMode, Config},
    highlighter::Highlighter,
    notification::NotificationSource,
    plugin::PickerHandle,
    theme::ThemeMode,
    ui::{ThemeBrowser, ThemeCatalogEntry, ThemeSettings},
};

const THEME_SELECTION_ISSUE: &str = "theme-selection";
const SUGGESTED_LIGHT: &str = "red-light.json";
const SUGGESTED_DARK: &str = "red.json";

fn preference(appearance: &AppearanceConfig, slot: ThemeMode) -> Option<&str> {
    match slot {
        ThemeMode::Light => appearance.light_theme.as_deref(),
        ThemeMode::Dark => appearance.dark_theme.as_deref(),
    }
}

fn terminal_theme_issue(slot: ThemeMode) -> (usize, &'static str) {
    match slot {
        ThemeMode::Light => (0, "terminal-light-theme"),
        ThemeMode::Dark => (1, "terminal-dark-theme"),
    }
}

impl Editor {
    pub(super) fn theme_config_dir(&self) -> &Path {
        self.config_path.parent().unwrap_or_else(|| Path::new("."))
    }

    pub(super) fn restore_theme_preview(&mut self) -> bool {
        let Some(theme) = self.theme_preview_base.take() else {
            return false;
        };
        match Highlighter::with_registry(&theme, self.highlighter.registry()) {
            Ok(highlighter) => {
                self.install_theme(theme, highlighter);
                true
            }
            Err(error) => {
                self.report_issue(
                    THEME_SELECTION_ISSUE.into(),
                    format!("Could not restore theme: {error:#}"),
                );
                false
            }
        }
    }

    pub(super) fn apply_terminal_preference(&mut self) -> bool {
        if self.theme_preview_base.is_some()
            || self.config.appearance.mode != AppearanceMode::Terminal
        {
            return false;
        }
        let Some(slot) = self.terminal_appearance else {
            return false;
        };
        let Some(name) = preference(&self.config.appearance, slot).map(str::to_owned) else {
            return false;
        };
        if self.committed_theme == name {
            self.clear_terminal_theme_issue(slot);
            return false;
        }
        match self.prepare_theme(&name) {
            Ok((theme, highlighter)) => {
                self.clear_terminal_theme_issue(slot);
                self.committed_theme = name;
                self.install_theme(theme, highlighter);
                true
            }
            Err(error) => {
                let (index, key) = terminal_theme_issue(slot);
                if self.terminal_theme_errors[index].as_deref() != Some(&name) {
                    self.report_issue(
                        key.into(),
                        format!(
                            "Could not apply {} theme {name}: {error:#}",
                            slot.label().to_lowercase()
                        ),
                    );
                    self.terminal_theme_errors[index] = Some(name);
                }
                false
            }
        }
    }

    fn clear_terminal_theme_issue(&mut self, slot: ThemeMode) {
        let (index, key) = terminal_theme_issue(slot);
        if self.terminal_theme_errors[index].take().is_some() {
            self.resolve_issue(&NotificationSource::Editor, key);
        }
    }

    pub(super) fn set_terminal_appearance(&mut self, appearance: Option<ThemeMode>) {
        self.terminal_appearance = appearance;
        if let Some(dialog) = &mut self.current_dialog {
            dialog.set_terminal_appearance(appearance);
        }
    }

    pub(super) async fn resolve_terminal_theme(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        if self.apply_terminal_preference() {
            let name = self.committed_theme.clone();
            self.notify_theme_browser_change(runtime, &name, false)
                .await?;
        }
        self.render(buffer)
    }

    // Keep plugin activation and notification state out of nested editor futures.
    #[inline(never)]
    pub(super) fn notify_theme_browser_change<'a>(
        &'a mut self,
        runtime: &'a mut Runtime,
        name: &'a str,
        persisted: bool,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            self.refresh_plugin_snapshots(runtime, false, false, true)?;
            self.plugin_registry
                .notify(
                    runtime,
                    "theme:changed",
                    json!({
                        "name": name,
                        "persisted": persisted,
                    }),
                )
                .await
        })
    }

    fn proposed_theme_pair(&self) -> AppearanceConfig {
        let current = self.theme.mode();
        let appearance = &self.config.appearance;
        AppearanceConfig {
            mode: appearance.mode,
            light_theme: appearance.light_theme.clone().or_else(|| {
                Some(
                    if current == ThemeMode::Light {
                        &self.committed_theme
                    } else {
                        SUGGESTED_LIGHT
                    }
                    .into(),
                )
            }),
            dark_theme: appearance.dark_theme.clone().or_else(|| {
                Some(
                    if current == ThemeMode::Dark {
                        &self.committed_theme
                    } else {
                        SUGGESTED_DARK
                    }
                    .into(),
                )
            }),
        }
    }

    fn open_theme_settings(&mut self, handle: Option<PickerHandle>) {
        let pair = self.proposed_theme_pair();
        let display = |slot| {
            let name = preference(&pair, slot).unwrap_or_default();
            let theme = self
                .load_theme(name)
                .ok()
                .map(|theme| theme.name)
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| name.to_owned());
            let label = if pair.mode == AppearanceMode::Fixed && name == self.committed_theme {
                " (current)"
            } else if preference(&self.config.appearance, slot).is_none() {
                " (suggested)"
            } else {
                ""
            };
            format!("{theme}{label}")
        };
        let settings = ThemeSettings::new(
            self,
            pair.mode == AppearanceMode::Terminal,
            display(ThemeMode::Light),
            display(ThemeMode::Dark),
            self.terminal_appearance,
            handle,
        );
        self.current_dialog = Some(Box::new(settings));
    }

    fn open_theme_list(
        &mut self,
        slot: Option<ThemeMode>,
        return_to_settings: bool,
        handle: Option<PickerHandle>,
    ) -> anyhow::Result<()> {
        let entries =
            assets::list_runtime_assets(RuntimeAssetKind::Theme, self.theme_config_dir())?;
        let entries = entries
            .into_iter()
            .map(|entry| {
                let mode =
                    slot.and_then(|_| self.load_theme(&entry.file).ok().map(|theme| theme.mode()));
                let detail = if entry.shadows.is_empty() {
                    entry.source.to_string()
                } else {
                    format!(
                        "{} shadows {} lower-priority source",
                        entry.source,
                        entry.shadows.len()
                    )
                };
                ThemeCatalogEntry {
                    name: entry.name.unwrap_or_else(|| entry.file.clone()),
                    file: entry.file,
                    mode,
                    detail,
                }
            })
            .collect();
        let pair = self.proposed_theme_pair();
        self.current_dialog = Some(Box::new(ThemeBrowser::new(
            self,
            entries,
            slot,
            return_to_settings,
            &self.committed_theme,
            pair,
            handle,
        )));
        Ok(())
    }

    fn current_theme_slot(&self) -> Option<ThemeMode> {
        (self.config.appearance.mode == AppearanceMode::Terminal).then(|| {
            self.terminal_appearance
                .unwrap_or_else(|| self.theme.mode())
        })
    }

    fn toggle_theme_switching(&mut self) -> anyhow::Result<()> {
        let path = &self.config_path;
        if self.config.appearance.mode == AppearanceMode::Terminal {
            Config::persist_appearance(
                path,
                Some(&self.committed_theme),
                Some(AppearanceMode::Fixed),
                None,
                None,
            )?;
            self.config.theme.clone_from(&self.committed_theme);
            self.config.appearance.mode = AppearanceMode::Fixed;
            for slot in [ThemeMode::Light, ThemeMode::Dark] {
                self.clear_terminal_theme_issue(slot);
            }
        } else {
            let mut pair = self.proposed_theme_pair();
            for slot in [ThemeMode::Light, ThemeMode::Dark] {
                let name = preference(&pair, slot).expect("proposed pair fills both slots");
                self.prepare_theme(name).with_context(|| {
                    format!(
                        "Could not load {} theme {name}",
                        slot.label().to_lowercase()
                    )
                })?;
            }
            Config::persist_appearance(
                path,
                None,
                Some(AppearanceMode::Terminal),
                pair.light_theme.as_deref(),
                pair.dark_theme.as_deref(),
            )?;
            pair.mode = AppearanceMode::Terminal;
            self.config.appearance = pair;
        }
        Ok(())
    }

    fn save_theme_choice(&mut self, name: &str, slot: Option<ThemeMode>) -> anyhow::Result<bool> {
        let (theme, highlighter) = self.prepare_theme(name)?;
        let path = &self.config_path;
        match slot {
            None => {
                Config::persist_appearance(path, Some(name), None, None, None)?;
                self.config.theme = name.into();
            }
            Some(ThemeMode::Light) => {
                Config::persist_appearance(path, None, None, Some(name), None)?;
                self.config.appearance.light_theme = Some(name.into());
            }
            Some(ThemeMode::Dark) => {
                Config::persist_appearance(path, None, None, None, Some(name))?;
                self.config.appearance.dark_theme = Some(name.into());
            }
        }
        if let Some(slot) = slot {
            self.clear_terminal_theme_issue(slot);
        }
        let apply = slot.is_none() || slot == self.terminal_appearance;
        if apply {
            self.theme_preview_base = None;
            self.committed_theme = name.into();
            self.install_theme(theme, highlighter);
        }
        Ok(apply)
    }

    pub(super) async fn handle_theme_browser_action(
        &mut self,
        action: &ThemeBrowserAction,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let previous = self.committed_theme.clone();
        let handle = self
            .current_dialog
            .as_ref()
            .and_then(|dialog| dialog.picker_handle());
        let mut restored = false;
        let mut persisted = false;
        let result = match action {
            ThemeBrowserAction::Open(new_handle) => {
                if new_handle.is_some_and(|handle| runtime.picker_plugin(handle).is_none()) {
                    return Ok(());
                }
                restored = self.restore_theme_preview();
                self.apply_terminal_preference();
                self.release_current_dialog_callbacks(runtime).await;
                self.current_dialog = None;
                let opened = self.open_theme_list(self.current_theme_slot(), false, *new_handle);
                if let (Err(_), Some(handle)) = (&opened, new_handle) {
                    runtime.release_picker(*handle);
                }
                opened
            }
            ThemeBrowserAction::BackToList => {
                self.open_theme_list(self.current_theme_slot(), false, handle)
            }
            ThemeBrowserAction::Settings => {
                restored = self.restore_theme_preview();
                self.apply_terminal_preference();
                self.open_theme_settings(handle);
                Ok(())
            }
            ThemeBrowserAction::Choose(slot) => self.open_theme_list(Some(*slot), true, handle),
            ThemeBrowserAction::ToggleAutomatic => self.toggle_theme_switching().map(|()| {
                self.apply_terminal_preference();
                self.open_theme_settings(handle);
            }),
            ThemeBrowserAction::Preview(Some(name)) => match self.prepare_theme(name) {
                Ok((theme, highlighter)) => {
                    if self.theme_preview_base.is_none() {
                        self.theme_preview_base = Some(self.theme.clone());
                    }
                    self.install_theme(theme, highlighter);
                    self.notify_theme_browser_change(runtime, name, false).await
                }
                Err(error) => {
                    restored = self.restore_theme_preview();
                    Err(error)
                }
            },
            ThemeBrowserAction::Preview(None) => {
                restored = self.restore_theme_preview();
                Ok(())
            }
            ThemeBrowserAction::Save {
                name,
                slot,
                return_to_settings,
            } => match self.save_theme_choice(name, *slot) {
                Ok(applied) => {
                    persisted = applied;
                    restored = self.restore_theme_preview();
                    self.apply_terminal_preference();
                    if *return_to_settings {
                        self.open_theme_settings(handle);
                    } else {
                        self.release_current_dialog_callbacks(runtime).await;
                        self.current_dialog = None;
                    }
                    Ok(())
                }
                Err(error) => {
                    restored = self.restore_theme_preview();
                    Err(error)
                }
            },
            ThemeBrowserAction::Cancel { return_to_settings } => {
                restored = self.restore_theme_preview();
                self.apply_terminal_preference();
                if *return_to_settings {
                    self.open_theme_settings(handle);
                } else {
                    self.release_current_dialog_callbacks(runtime).await;
                    self.current_dialog = None;
                }
                Ok(())
            }
        };
        match result {
            Ok(()) => self.resolve_issue(&NotificationSource::Editor, THEME_SELECTION_ISSUE),
            Err(error) => self.report_issue(
                THEME_SELECTION_ISSUE.into(),
                format!("Theme selection: {error:#}"),
            ),
        }
        if persisted || restored || previous != self.committed_theme {
            let name = self.committed_theme.clone();
            self.notify_theme_browser_change(runtime, &name, persisted)
                .await?;
        }
        self.render(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        buffer::Buffer,
        editor::{Action, PluginRequest},
        lsp::LspManager,
        theme::Style,
    };
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, TerminalAppearance};

    struct ThemeTest {
        editor: Editor,
        buffer: RenderBuffer,
        runtime: Runtime,
        directory: tempfile::TempDir,
    }

    impl ThemeTest {
        fn new(fixed: &str) -> Self {
            let config = Config {
                theme: fixed.into(),
                ..Config::default()
            };
            let lsp = Box::new(LspManager::new(config.lsp.clone()));
            let theme =
                crate::theme::parse_vscode_theme_contents(assets::bundled_theme(fixed).unwrap())
                    .unwrap();
            let mut editor = Editor::test_with_size(
                lsp,
                120,
                32,
                config,
                theme,
                vec![Buffer::new(None, "test".into())],
            )
            .unwrap();
            editor.test_disable_terminal_output();
            let directory = tempfile::tempdir().unwrap();
            editor.config_path = directory.path().join("config.toml");
            std::fs::write(
                &editor.config_path,
                format!("theme = \"{fixed}\" # keep\nwrap = false\n"),
            )
            .unwrap();
            Self {
                editor,
                buffer: RenderBuffer::new(120, 32, &Style::default()),
                runtime: Runtime::new(),
                directory,
            }
        }

        async fn event(&mut self, event: Event) {
            self.editor
                .process_editor_event(
                    event,
                    &mut self.buffer,
                    &mut self.runtime,
                    super::super::EventRenderMode::Immediate,
                )
                .await
                .unwrap();
        }

        async fn action(&mut self, action: ThemeBrowserAction) {
            self.editor
                .handle_theme_browser_action(&action, &mut self.buffer, &mut self.runtime)
                .await
                .unwrap();
        }

        async fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
            self.event(Event::Key(KeyEvent::new(code, modifiers))).await;
        }

        fn saved(&self) -> String {
            std::fs::read_to_string(&self.editor.config_path).unwrap()
        }
    }

    #[tokio::test]
    async fn theme_browser_keeps_default_fixed_until_the_checkbox_is_enabled() {
        let mut test = ThemeTest::new("latte.json");
        #[cfg(unix)]
        {
            // The configuration might be managed by a symlink from a dotfiles checkout.
            let target = test.directory.path().join("managed-config.toml");
            std::fs::rename(&test.editor.config_path, &target).unwrap();
            std::os::unix::fs::symlink(&target, &test.editor.config_path).unwrap();
        }
        let original = test.saved();
        test.event(Event::TerminalAppearance(TerminalAppearance::Dark))
            .await;
        assert_eq!(test.editor.committed_theme, "latte.json");
        test.action(ThemeBrowserAction::Open(None)).await;
        test.key(KeyCode::Char('t'), KeyModifiers::CONTROL).await;
        test.key(KeyCode::Down, KeyModifiers::NONE).await;
        assert_eq!(test.editor.config.appearance.mode, AppearanceMode::Fixed);
        assert_eq!(test.saved(), original);
        assert_eq!(
            test.editor.proposed_theme_pair().light_theme.as_deref(),
            Some("latte.json")
        );
        assert_eq!(
            test.editor.proposed_theme_pair().dark_theme.as_deref(),
            Some("red.json")
        );

        test.key(KeyCode::Char(' '), KeyModifiers::NONE).await;
        assert_eq!(test.editor.config.appearance.mode, AppearanceMode::Terminal);
        assert_eq!(test.editor.committed_theme, "red.json");
        assert!(test.saved().contains("theme = \"latte.json\" # keep"));
        test.key(KeyCode::Char(' '), KeyModifiers::NONE).await;
        assert_eq!(test.editor.config.appearance.mode, AppearanceMode::Fixed);
        assert!(test.saved().contains("theme = \"red.json\" # keep"));
        assert!(test.saved().contains("light_theme = \"latte.json\""));
        assert!(test.saved().contains("wrap = false"));
        #[cfg(unix)]
        assert!(std::fs::symlink_metadata(&test.editor.config_path)
            .unwrap()
            .is_symlink());
    }

    #[tokio::test]
    async fn theme_browser_resolves_latest_report_after_preview_save_or_cancel() {
        let mut test = ThemeTest::new("andromeda.json");
        test.action(ThemeBrowserAction::ToggleAutomatic).await;
        test.event(Event::TerminalAppearance(TerminalAppearance::Dark))
            .await;
        test.runtime
            .load_plugin(
                "theme_browser",
                include_str!("../../plugins/theme_browser.hk"),
            )
            .await
            .unwrap();
        test.runtime.execute_command("ThemeBrowser").await.unwrap();
        let Some(PluginRequest::Action(Action::ThemeBrowser(
            action @ ThemeBrowserAction::Open(Some(handle)),
        ))) = test.runtime.try_recv_request()
        else {
            panic!("expected an owned theme picker");
        };
        test.action(action).await;
        assert!(test.editor.theme_preview_base.is_none());
        test.action(ThemeBrowserAction::Preview(Some("red.json".into())))
            .await;
        test.event(Event::TerminalAppearance(TerminalAppearance::Light))
            .await;
        assert_eq!(
            test.editor.theme.name,
            test.editor.load_theme("red.json").unwrap().name
        );
        assert_eq!(test.editor.committed_theme, "andromeda.json");
        test.action(ThemeBrowserAction::Save {
            name: "red.json".into(),
            slot: Some(ThemeMode::Dark),
            return_to_settings: false,
        })
        .await;
        assert!(test.runtime.picker_plugin(handle).is_none());
        assert_eq!(test.editor.committed_theme, "red-light.json");
        assert!(test.saved().contains("light_theme = \"red-light.json\""));
        assert!(test.saved().contains("dark_theme = \"red.json\""));
        let saved = test.saved();

        test.action(ThemeBrowserAction::Open(None)).await;
        test.action(ThemeBrowserAction::Preview(Some("latte.json".into())))
            .await;
        test.event(Event::TerminalAppearance(TerminalAppearance::Dark))
            .await;
        test.key(KeyCode::Esc, KeyModifiers::NONE).await;
        assert_eq!(test.editor.committed_theme, "red.json");
        assert_eq!(test.saved(), saved);
    }

    #[tokio::test]
    async fn theme_browser_keeps_working_theme_and_preferences_on_failure() {
        let mut test = ThemeTest::new("red.json");
        test.editor.config.appearance = AppearanceConfig {
            mode: AppearanceMode::Terminal,
            light_theme: Some("missing.json".into()),
            dark_theme: Some("red.json".into()),
        };
        for _ in 0..2 {
            test.event(Event::TerminalAppearance(TerminalAppearance::Light))
                .await;
        }
        assert_eq!(test.editor.committed_theme, "red.json");
        assert_eq!(test.editor.notifications.records().count(), 1);
        let config = test.editor.config.appearance.clone();
        test.editor.config_path = test.directory.path().to_path_buf();
        test.action(ThemeBrowserAction::Open(None)).await;
        test.action(ThemeBrowserAction::Preview(Some("latte.json".into())))
            .await;
        test.action(ThemeBrowserAction::Save {
            name: "latte.json".into(),
            slot: Some(ThemeMode::Light),
            return_to_settings: false,
        })
        .await;
        assert_eq!(test.editor.config.appearance, config);
        assert_eq!(test.editor.committed_theme, "red.json");
        assert_eq!(test.editor.theme.mode(), ThemeMode::Dark);
        assert!(test.editor.current_dialog.is_some());
    }

    #[tokio::test]
    async fn theme_browser_tab_and_filter_preserve_query_and_allow_either_appearance() {
        let mut test = ThemeTest::new("red.json");
        test.action(ThemeBrowserAction::ToggleAutomatic).await;
        test.event(Event::TerminalAppearance(TerminalAppearance::Dark))
            .await;
        test.action(ThemeBrowserAction::Open(None)).await;

        test.event(Event::Paste("Latte".into())).await;
        assert!(test.editor.theme_preview_base.is_none());
        test.key(KeyCode::Tab, KeyModifiers::NONE).await;
        assert_eq!(test.editor.theme.mode(), ThemeMode::Light);
        test.key(KeyCode::BackTab, KeyModifiers::SHIFT).await;
        assert_eq!(test.editor.theme.mode(), ThemeMode::Dark);
        test.key(KeyCode::Char('r'), KeyModifiers::CONTROL).await;
        assert_eq!(test.editor.theme.mode(), ThemeMode::Light);
        test.key(KeyCode::Enter, KeyModifiers::NONE).await;
        assert_eq!(
            test.editor.config.appearance.dark_theme.as_deref(),
            Some("latte.json")
        );
        assert_eq!(test.editor.config.theme, "red.json");
        assert_eq!(test.editor.committed_theme, "latte.json");
        assert!(test.editor.current_dialog.is_none());
    }

    #[tokio::test]
    async fn detached_theme_browser_forgets_appearance_when_the_attachment_ends() {
        use crate::headless::InputEvent;
        let mut test = ThemeTest::new("red.json");
        test.editor.toggle_theme_switching().unwrap();
        let mut core = super::super::DetachedEditorCore::new(test.editor)
            .await
            .unwrap();
        core.input(InputEvent::TerminalAppearance {
            appearance: TerminalAppearance::Light,
        })
        .await
        .unwrap();
        assert_eq!(core.editor.committed_theme, "red-light.json");

        core.client_disconnected();
        assert_eq!(core.editor.terminal_appearance, None);
        assert_eq!(core.editor.committed_theme, "red-light.json");

        core.input(InputEvent::TerminalAppearance {
            appearance: TerminalAppearance::Dark,
        })
        .await
        .unwrap();
        assert_eq!(core.editor.committed_theme, "red.json");
    }

    #[test]
    fn theme_classification_uses_canvas_color_and_ignores_declared_type() {
        let theme = crate::theme::parse_vscode_theme_contents(
            assets::bundled_theme("kanagawa-lotus.json").unwrap(),
        )
        .unwrap();
        assert_eq!(theme.mode(), ThemeMode::Light);
        let mut theme = theme;
        theme.style.bg = Some(crate::color::Color::Rgba {
            r: 255,
            g: 255,
            b: 255,
            a: 125,
        });
        let (_, background) =
            super::super::rendering::resolve_cell_colors(&theme.style, &theme.style);
        assert_eq!(theme.mode() == ThemeMode::Light, background.is_light());
    }
}
