use crate::{
    assets,
    buffer::Buffer,
    config::Config,
    editor::Editor,
    lsp::LspManager,
    plugin::{PluginRegistry, Runtime},
    theme::parse_vscode_theme_contents,
};

pub async fn run() -> anyhow::Result<()> {
    let mut config = Config::from_user_toml_with_overrides("", &[])?;

    let themes = assets::bundled_theme_files();
    anyhow::ensure!(!themes.is_empty(), "no bundled themes were found");
    for (file, contents) in themes {
        parse_vscode_theme_contents(contents)
            .map_err(|error| anyhow::anyhow!("failed to parse bundled theme {file}: {error}"))?;
    }

    let mut registry = PluginRegistry::new();
    for (name, file) in &config.plugins {
        let specifier = assets::bundled_plugin_specifier(file)
            .ok_or_else(|| anyhow::anyhow!("default plugin {name} is not bundled: {file}"))?;
        registry.add(name, &specifier);
    }

    let theme_contents = assets::bundled_theme(&config.theme)
        .ok_or_else(|| anyhow::anyhow!("default theme is not bundled: {}", config.theme))?;
    let theme = parse_vscode_theme_contents(theme_contents)?;
    let permissions = std::mem::take(&mut config.plugin_permissions);
    let lsp = Box::new(LspManager::new(config.lsp.clone()));
    let editor = Editor::with_size(
        lsp,
        80,
        24,
        config,
        theme,
        vec![Buffer::new(None, String::new())],
    )?;

    let mut runtime = Runtime::try_new_with_permissions(permissions)?;
    editor.refresh_plugin_snapshots(&mut runtime, true, true, true)?;
    registry.initialize(&mut runtime).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::editor::{ACTION_DISPATCHER, PLUGIN_DISPATCHER_TEST_LOCK};

    #[tokio::test]
    async fn bundled_runtime_initializes_with_production_snapshots() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        while ACTION_DISPATCHER.try_recv_request().is_some() {}

        let result = super::run().await;
        while ACTION_DISPATCHER.try_recv_request().is_some() {}

        result.unwrap();
    }
}
