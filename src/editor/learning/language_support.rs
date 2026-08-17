//! Read-only local language inventory for the setup lesson.

use super::*;
use crate::{
    assets::RuntimeAssetKind,
    ui::{HoverInfo, HoverInfoFormat},
};

impl Editor {
    fn learn_language_support_report(&self, config_dir: &Path) -> String {
        let ready = self
            .current_buffer()
            .file
            .as_deref()
            .is_some_and(|file| self.lsp.server_capabilities_for_file(file).is_some());
        let syntax = self
            .highlight_language_id_for_buffer_index(self.buffer_manager.active_index())
            .unwrap_or_else(|| "off or unrecognized".into());
        let mut lines = vec![
            "READ-ONLY LOCAL CHECK".into(),
            format!(
                "Syntax: {syntax} ({} available definitions)",
                self.highlighter.language_ids().len()
            ),
            format!(
                "Practice Husk server: {}",
                if ready {
                    "initialized"
                } else {
                    "not initialized"
                }
            ),
            format!(
                "Practice parser diagnostic: {}",
                if self.learn_diagnostic_present() {
                    "received"
                } else {
                    "not received"
                }
            ),
            String::new(),
            "YOUR CONFIGURED SERVER COMMANDS".into(),
            "A command found on disk is not proof that its server is running.".into(),
        ];
        if let Some(session) = self.learn_session.as_ref() {
            let config = session.original_language.original_config();
            if !config.enabled {
                lines.push("Language servers are disabled in your configuration.".into());
            }
            let mut servers = config.servers.iter().collect::<Vec<_>>();
            servers.sort_by_key(|(name, _)| *name);
            if servers.is_empty() {
                lines.push("No language servers configured.".into());
            }
            for (name, server) in servers {
                let command = server.command.replace(['\r', '\n'], " ");
                lines.push(format!(
                    "{name}: {} — {command}",
                    if command_available(&server.command) {
                        "command found"
                    } else {
                        "command missing"
                    }
                ));
            }
        }
        lines.extend([String::new(), "INSTALLED LANGUAGE PACKS".into()]);
        match plugin::package::PluginPackageManager::new(config_dir.to_path_buf()).list() {
            Ok(packages) => {
                let packages = packages
                    .into_iter()
                    .filter(|package| package.has_languages)
                    .collect::<Vec<_>>();
                if packages.is_empty() {
                    lines.push(
                        "No external language packs installed. Bundled languages still work."
                            .into(),
                    );
                }
                for package in packages {
                    lines.push(format!(
                        "{} {} — {} — {}",
                        package.name,
                        package.version,
                        if package.enabled && package.compatible {
                            "enabled"
                        } else if !package.compatible {
                            "incompatible"
                        } else {
                            "disabled"
                        },
                        package.languages.join(", ")
                    ));
                }
            }
            Err(error) => lines.push(format!("Inventory unavailable: {error:#}")),
        }
        if let Ok(plugins) =
            crate::assets::list_runtime_assets(RuntimeAssetKind::Plugin, config_dir)
        {
            lines.push(format!(
                "{} effective runtime plugins available.",
                plugins.len()
            ));
        }
        lines.extend([String::new(),"NEXT STEPS IN YOUR OWN WORKSPACE".into(),":syntax chooses buffer-local highlighting.".into(),":plugins opens the language-pack manager. Installation and native-grammar approval are separate, explicit choices.".into(),":languages reload validates changed language definitions before applying them.".into(),"This report did not download, install, approve, or start anything for your project.".into()]);
        lines.join("\n")
    }

    pub(super) fn intercept_learn_language_support(
        &mut self,
        action: &Action,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<bool> {
        if !matches!(action, Action::ListPlugins)
            || self
                .learn_session
                .as_ref()
                .is_none_or(|session| session.lesson != Lesson::CheckLanguageSupport)
        {
            return Ok(false);
        }
        let report = self.learn_language_support_report(&Config::config_dir());
        self.release_current_dialog_callbacks(runtime);
        self.current_dialog = Some(Box::new(
            HoverInfo::new(self, report, HoverInfoFormat::Plaintext, Vec::new())
                .with_label("Language support"),
        ));
        self.observe_learn_action(action, buffer)?;
        self.render(buffer)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn learn_language_report_distinguishes_configuration_from_readiness() {
        let mut config = Config::default();
        config.lsp.enabled = false;
        let mut missing = crate::config::default_language_servers()
            .remove("husk")
            .unwrap();
        missing.command = "red-learn-deliberately-missing-server".into();
        missing
            .env
            .insert("PRIVATE_TEST_VALUE".into(), "must not appear".into());
        config.lsp.servers = HashMap::from([("example".into(), missing)]);
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
            .start_learn_lesson(Lesson::SaveAPracticeFile, &mut buffer, &mut runtime)
            .await
            .unwrap();
        let root = editor
            .learn_session
            .as_ref()
            .unwrap()
            .workspace
            .as_ref()
            .unwrap()
            .path("absent-config");
        let report = editor.learn_language_support_report(&root);
        assert!(report.contains("not initialized"));
        assert!(report.contains("example: command missing"));
        assert!(report.contains("disabled in your configuration"));
        assert!(report.contains("No external language packs installed"));
        assert!(!report.contains("must not appear"));
        assert!(!root.exists());
        editor
            .finish_learn_lesson(&mut buffer, &mut runtime)
            .await
            .unwrap();
    }
}
