use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::editor::EditorStateSnapshot;

use super::{PluginMetadata, Runtime};

pub struct PluginRegistry {
    plugins: Vec<(String, String)>,
    metadata: HashMap<String, PluginMetadata>,
    initialized: bool,
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
            metadata: HashMap::new(),
            initialized: false,
        }
    }

    pub fn add(&mut self, name: &str, path: &str) {
        self.plugins.push((name.to_string(), path.to_string()));

        let plugin_path = Path::new(path);
        if let Some(dir) = plugin_path.parent() {
            let package_json = dir.join("package.json");
            if package_json.exists() {
                match PluginMetadata::from_file(&package_json) {
                    Ok(metadata) => {
                        self.metadata.insert(name.to_string(), metadata);
                    }
                    Err(error) => {
                        crate::log!("Failed to load metadata for plugin {}: {}", name, error);
                        self.metadata
                            .insert(name.to_string(), PluginMetadata::minimal(name.to_string()));
                    }
                }
            } else {
                self.metadata
                    .insert(name.to_string(), PluginMetadata::minimal(name.to_string()));
            }
        } else {
            self.metadata
                .insert(name.to_string(), PluginMetadata::minimal(name.to_string()));
        }
    }

    /// Get metadata for a specific plugin.
    pub fn get_metadata(&self, name: &str) -> Option<&PluginMetadata> {
        self.metadata.get(name)
    }

    /// Get all plugin metadata.
    pub fn all_metadata(&self) -> &HashMap<String, PluginMetadata> {
        &self.metadata
    }

    pub async fn initialize(&mut self, runtime: &mut Runtime) -> anyhow::Result<()> {
        self.ensure_plugin_files_exist()?;

        for (name, plugin) in &self.plugins {
            let source = plugin_source(plugin)?;
            runtime
                .load_plugin_at(name, plugin_display_path(plugin), &source)
                .await?;
        }
        self.initialized = true;

        Ok(())
    }

    fn ensure_plugin_files_exist(&self) -> anyhow::Result<()> {
        for (name, plugin) in &self.plugins {
            if crate::assets::is_bundled_plugin_specifier(plugin) {
                continue;
            }

            let path = Path::new(plugin);
            if path.is_file() {
                continue;
            }

            let issue = if path.exists() {
                "that path exists, but it is not a file"
            } else {
                "that file does not exist"
            };

            return Err(anyhow::anyhow!(
                "Could not load plugin `{}`.\n\nRed was asked to load this plugin file, but {}:\n  {}\n\nCheck the `{}` entry under `[plugins]` in your config.toml, or put the plugin file back at that path.",
                name,
                issue,
                path.display(),
                name
            ));
        }

        Ok(())
    }

    pub async fn execute(&mut self, runtime: &mut Runtime, command: &str) -> anyhow::Result<()> {
        runtime.execute_command(command).await
    }

    pub async fn notify(
        &self,
        runtime: &mut Runtime,
        event: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<()> {
        let _span = crate::editor::perf::PerfSpan::with_detail("notify", event);
        runtime.notify(event, args).await
    }

    pub async fn before_exit(
        &self,
        runtime: &mut Runtime,
        snapshot: EditorStateSnapshot,
    ) -> anyhow::Result<()> {
        if !self.initialized {
            return Ok(());
        }

        runtime.before_exit(serde_json::to_value(snapshot)?).await
    }

    pub async fn deactivate_all(&mut self, runtime: &mut Runtime) -> anyhow::Result<()> {
        if !self.initialized {
            return Ok(());
        }

        runtime.deactivate_all().await?;
        self.initialized = false;
        Ok(())
    }

    pub async fn reload(&mut self, runtime: &mut Runtime) -> anyhow::Result<()> {
        self.deactivate_all(runtime).await?;
        self.initialize(runtime).await?;
        Ok(())
    }
}

fn plugin_source(plugin: &str) -> anyhow::Result<String> {
    if crate::assets::is_bundled_plugin_specifier(plugin) {
        return crate::assets::bundled_plugin_contents(plugin)
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("bundled plugin `{plugin}` was not found"));
    }

    Ok(fs::read_to_string(plugin)?)
}

fn plugin_display_path(plugin: &str) -> String {
    plugin
        .strip_prefix("red-bundled:///")
        .unwrap_or(plugin)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{Action, PluginRequest, ACTION_DISPATCHER, PLUGIN_DISPATCHER_TEST_LOCK};
    use std::time::Duration;

    fn drain_requests() {
        while ACTION_DISPATCHER.try_recv_request().is_some() {}
    }

    #[tokio::test]
    async fn reports_missing_plugin_path() {
        let mut registry = PluginRegistry::new();
        registry.add("missing", "/tmp/red-missing-plugin.hk");
        let mut runtime = Runtime::new();

        let error = registry.initialize(&mut runtime).await.unwrap_err();
        let message = error.to_string();

        assert!(message.contains("Could not load plugin `missing`"));
        assert!(message.contains("`[plugins]`"));
    }

    #[tokio::test]
    async fn executes_husk_command() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let dir = tempfile_dir("husk-command");
        let plugin = dir.join("plugin.hk");
        fs::write(
            &plugin,
            r#"
                pub fn activate() {
                    red::add_command("Hello", hello);
                }

                fn hello() {
                    red::execute("Print", "hello from registry");
                }
            "#,
        )
        .unwrap();
        let mut registry = PluginRegistry::new();
        registry.add("test", plugin.to_str().unwrap());
        let mut runtime = Runtime::new();

        registry.initialize(&mut runtime).await.unwrap();
        registry.execute(&mut runtime, "Hello").await.unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::Action(Action::Print(message)) => {
                assert_eq!(message, "hello from registry");
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    fn tempfile_dir(prefix: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "red-{prefix}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
