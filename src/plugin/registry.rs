use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use crate::editor::EditorStateSnapshot;
use semver::{Version, VersionReq};
use serde::Serialize;

use super::{PluginMetadata, Runtime};

pub struct PluginRegistry {
    plugins: Vec<(String, String)>,
    metadata: HashMap<String, PluginMetadata>,
    initialized: bool,
    statuses: HashMap<String, PluginStatus>,
    modified_at: HashMap<String, SystemTime>,
    last_hot_reload_poll: Instant,
}

pub const RED_HOST_API_VERSION: &str = "0.1.0";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum PluginStatus {
    Pending,
    Active,
    ActiveWithReloadError {
        path: String,
        diagnostic: String,
    },
    Disabled,
    Quarantined {
        stage: String,
        path: String,
        diagnostic: String,
    },
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
            statuses: HashMap::new(),
            modified_at: HashMap::new(),
            last_hot_reload_poll: Instant::now(),
        }
    }

    pub fn add(&mut self, name: &str, path: &str) {
        self.plugins.push((name.to_string(), path.to_string()));
        self.statuses
            .insert(name.to_string(), PluginStatus::Pending);
        if let Ok(modified) = fs::metadata(path).and_then(|metadata| metadata.modified()) {
            self.modified_at.insert(name.to_string(), modified);
        }

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

    #[must_use]
    pub fn statuses(&self) -> &HashMap<String, PluginStatus> {
        &self.statuses
    }

    pub async fn initialize(&mut self, runtime: &mut Runtime) -> anyhow::Result<()> {
        let mut pending = self.plugins.clone();
        while !pending.is_empty() {
            let mut deferred = Vec::new();
            let mut progressed = false;
            for (name, plugin) in pending {
                let metadata = self
                    .metadata
                    .get(&name)
                    .cloned()
                    .unwrap_or_else(|| PluginMetadata::minimal(name.clone()));
                let missing_dependencies = metadata
                    .dependencies
                    .keys()
                    .filter(|dependency| !self.statuses.contains_key(*dependency))
                    .cloned()
                    .collect::<Vec<_>>();
                if !missing_dependencies.is_empty() {
                    self.quarantine(
                        runtime,
                        &name,
                        &plugin,
                        "dependency",
                        format!(
                            "missing required plugins: {}",
                            missing_dependencies.join(", ")
                        ),
                    );
                    progressed = true;
                    continue;
                }
                let failed_dependency = metadata.dependencies.keys().find(|dependency| {
                    matches!(
                        self.statuses.get(*dependency),
                        Some(PluginStatus::Quarantined { .. } | PluginStatus::Disabled)
                    )
                });
                if let Some(dependency) = failed_dependency {
                    self.quarantine(
                        runtime,
                        &name,
                        &plugin,
                        "dependency",
                        format!("required plugin `{dependency}` is not active"),
                    );
                    progressed = true;
                    continue;
                }
                if metadata.dependencies.keys().any(|dependency| {
                    !matches!(
                        self.statuses.get(dependency),
                        Some(PluginStatus::Active | PluginStatus::ActiveWithReloadError { .. })
                    )
                }) {
                    deferred.push((name, plugin));
                    continue;
                }
                let dependency_version_error = metadata.dependencies.iter().find_map(
                    |(dependency, requirement)| {
                        let dependency_metadata = self.metadata.get(dependency)?;
                        let requirement = VersionReq::parse(requirement).ok()?;
                        let version = Version::parse(&dependency_metadata.version).ok()?;
                        (!requirement.matches(&version)).then(|| {
                            format!(
                                "plugin `{dependency}` version {version} does not satisfy {requirement}"
                            )
                        })
                    },
                );
                if let Some(error) = dependency_version_error {
                    self.quarantine(runtime, &name, &plugin, "dependency", error);
                    progressed = true;
                    continue;
                }
                if let Err(error) = check_api_compatibility(&metadata) {
                    self.quarantine(runtime, &name, &plugin, "version", error.to_string());
                    progressed = true;
                    continue;
                }
                match plugin_source(&plugin) {
                    Ok(source) => match runtime
                        .load_plugin_at(&name, plugin_display_path(&plugin), &source)
                        .await
                    {
                        Ok(()) => {
                            self.statuses.insert(name, PluginStatus::Active);
                        }
                        Err(error) => self.quarantine(
                            runtime,
                            &name,
                            &plugin,
                            diagnostic_stage(&error),
                            error.to_string(),
                        ),
                    },
                    Err(error) => {
                        self.quarantine(runtime, &name, &plugin, "source", error.to_string())
                    }
                }
                progressed = true;
            }
            if !progressed {
                for (name, plugin) in deferred.drain(..) {
                    self.quarantine(
                        runtime,
                        &name,
                        &plugin,
                        "dependency",
                        "dependency cycle prevents activation".to_string(),
                    );
                }
            }
            pending = deferred;
        }
        self.initialized = true;
        Ok(())
    }

    fn quarantine(
        &mut self,
        runtime: &mut Runtime,
        name: &str,
        path: &str,
        stage: &str,
        diagnostic: String,
    ) {
        runtime.unload_plugin(name);
        crate::log!("Plugin `{name}` quarantined during {stage}: {diagnostic}");
        self.statuses.insert(
            name.to_string(),
            PluginStatus::Quarantined {
                stage: stage.to_string(),
                path: plugin_display_path(path),
                diagnostic,
            },
        );
    }

    pub async fn execute(&mut self, runtime: &mut Runtime, command: &str) -> anyhow::Result<()> {
        let owner = runtime.command_plugin(command);
        if let Err(error) = runtime.execute_command(command).await {
            crate::log!("Plugin command `{command}` failed: {error:?}");
            if let Some(owner) = owner {
                let path = self
                    .plugins
                    .iter()
                    .find(|(name, _)| name == &owner)
                    .map(|(_, path)| path.clone())
                    .unwrap_or_default();
                self.quarantine(runtime, &owner, &path, "runtime", error.to_string());
            }
        }
        Ok(())
    }

    pub async fn notify(
        &mut self,
        runtime: &mut Runtime,
        event: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<()> {
        let _span = crate::editor::perf::PerfSpan::with_detail("notify", event);
        for (plugin, error) in runtime.notify_isolated(event, args) {
            let path = self
                .plugins
                .iter()
                .find(|(name, _)| name == &plugin)
                .map(|(_, path)| path.clone())
                .unwrap_or_default();
            self.quarantine(runtime, &plugin, &path, "runtime", error.to_string());
        }
        Ok(())
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
        for (name, path) in self.plugins.clone() {
            self.reload_one(runtime, &name, &path).await;
        }
        Ok(())
    }

    pub async fn poll_hot_reload(&mut self, runtime: &mut Runtime) {
        if self.last_hot_reload_poll.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.last_hot_reload_poll = Instant::now();
        for (name, path) in self.plugins.clone() {
            if crate::assets::is_bundled_plugin_specifier(&path) {
                continue;
            }
            let Ok(modified) = fs::metadata(&path).and_then(|metadata| metadata.modified()) else {
                continue;
            };
            let changed = self
                .modified_at
                .get(&name)
                .is_none_or(|previous| modified > *previous);
            if changed {
                self.modified_at.insert(name.clone(), modified);
                self.reload_one(runtime, &name, &path).await;
            }
        }
    }

    async fn reload_one(&mut self, runtime: &mut Runtime, name: &str, path: &str) {
        let source = plugin_source(path);
        let result = match source {
            Ok(source) => {
                runtime
                    .load_plugin_at(name, plugin_display_path(path), &source)
                    .await
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => {
                self.statuses.insert(name.to_string(), PluginStatus::Active);
            }
            Err(error) => {
                crate::log!("Plugin `{name}` hot reload rejected: {error}");
                self.statuses.insert(
                    name.to_string(),
                    PluginStatus::ActiveWithReloadError {
                        path: plugin_display_path(path),
                        diagnostic: error.to_string(),
                    },
                );
            }
        }
    }
}

fn check_api_compatibility(metadata: &PluginMetadata) -> anyhow::Result<()> {
    let Some(requirement) = metadata.red_api_version.as_deref() else {
        return Ok(());
    };
    let requirement = VersionReq::parse(requirement)
        .map_err(|error| anyhow::anyhow!("invalid red_api_version `{requirement}`: {error}"))?;
    let current = Version::parse(RED_HOST_API_VERSION)?;
    anyhow::ensure!(
        requirement.matches(&current),
        "plugin requires Red host API `{requirement}`, but this release provides `{current}`; see docs/PLUGIN_API.md"
    );
    Ok(())
}

fn diagnostic_stage(error: &anyhow::Error) -> &'static str {
    if error.downcast_ref::<husk_diagnostics::Report>().is_some() {
        "compile"
    } else {
        "activation"
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

        registry.initialize(&mut runtime).await.unwrap();
        assert!(matches!(
            registry.statuses().get("missing"),
            Some(PluginStatus::Quarantined { stage, diagnostic, .. })
                if stage == "source" && diagnostic.contains("No such file")
        ));
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

    #[tokio::test]
    async fn plugin_command_errors_do_not_escape_registry() {
        let dir = tempfile_dir("husk-command-error");
        let plugin = dir.join("plugin.hk");
        fs::write(
            &plugin,
            r#"
                pub fn activate() {
                    red::add_command("Fail", fail);
                }

                fn fail() {
                    red::execute(1);
                }
            "#,
        )
        .unwrap();
        let mut registry = PluginRegistry::new();
        registry.add("test", plugin.to_str().unwrap());
        let mut runtime = Runtime::new();

        registry.initialize(&mut runtime).await.unwrap();

        registry.execute(&mut runtime, "Fail").await.unwrap();
        assert!(matches!(
            registry.statuses().get("test"),
            Some(PluginStatus::Quarantined { stage, .. }) if stage == "runtime"
        ));
    }

    #[tokio::test]
    async fn plugin_notify_errors_do_not_escape_registry() {
        let dir = tempfile_dir("husk-notify-error");
        let plugin = dir.join("plugin.hk");
        fs::write(
            &plugin,
            r#"
                pub fn activate() {
                    red::on("editor:ready", fail);
                }

                fn fail(event: Json) {
                    red::execute(1);
                }
            "#,
        )
        .unwrap();
        let mut registry = PluginRegistry::new();
        registry.add("test", plugin.to_str().unwrap());
        let mut runtime = Runtime::new();

        registry.initialize(&mut runtime).await.unwrap();

        registry
            .notify(&mut runtime, "editor:ready", serde_json::json!({}))
            .await
            .unwrap();
        assert!(matches!(
            registry.statuses().get("test"),
            Some(PluginStatus::Quarantined { stage, .. }) if stage == "runtime"
        ));
    }

    #[tokio::test]
    async fn bad_plugin_is_quarantined_while_unrelated_plugin_starts() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let dir = tempfile_dir("isolated-load");
        let bad = dir.join("bad.hk");
        let good = dir.join("good.hk");
        fs::write(&bad, "fn activate( {").unwrap();
        fs::write(
            &good,
            r#"
                pub fn activate() { red::add_command("StillWorks", run); }
                fn run() { red::execute("Print", "isolated"); }
            "#,
        )
        .unwrap();
        let mut registry = PluginRegistry::new();
        registry.add("bad", bad.to_str().unwrap());
        registry.add("good", good.to_str().unwrap());
        let mut runtime = Runtime::new();

        registry.initialize(&mut runtime).await.unwrap();
        assert!(matches!(
            registry.statuses().get("bad"),
            Some(PluginStatus::Quarantined { stage, .. }) if stage == "compile"
        ));
        assert_eq!(registry.statuses().get("good"), Some(&PluginStatus::Active));
        registry.execute(&mut runtime, "StillWorks").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "isolated"
        ));
    }

    #[tokio::test]
    async fn incompatible_api_version_is_quarantined_before_activation() {
        let dir = tempfile_dir("api-version");
        let plugin = dir.join("plugin.hk");
        fs::write(&plugin, "pub fn activate() {}").unwrap();
        fs::write(
            dir.join("package.json"),
            r#"{"name":"future","red_api_version":">=1.0.0"}"#,
        )
        .unwrap();
        let mut registry = PluginRegistry::new();
        registry.add("future", plugin.to_str().unwrap());
        let mut runtime = Runtime::new();

        registry.initialize(&mut runtime).await.unwrap();
        assert!(matches!(
            registry.statuses().get("future"),
            Some(PluginStatus::Quarantined { stage, diagnostic, .. })
                if stage == "version" && diagnostic.contains("docs/PLUGIN_API.md")
        ));
    }

    #[tokio::test]
    async fn broken_hot_reload_keeps_the_previous_plugin_active() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let dir = tempfile_dir("transactional-reload");
        let plugin = dir.join("plugin.hk");
        fs::write(
            &plugin,
            r#"
                pub fn activate() { red::add_command("Reloaded", run); }
                fn run() { red::execute("Print", "old"); }
            "#,
        )
        .unwrap();
        let mut registry = PluginRegistry::new();
        registry.add("reload", plugin.to_str().unwrap());
        let mut runtime = Runtime::new();
        registry.initialize(&mut runtime).await.unwrap();

        fs::write(&plugin, "fn activate( {").unwrap();
        registry
            .reload_one(&mut runtime, "reload", plugin.to_str().unwrap())
            .await;
        assert!(matches!(
            registry.statuses().get("reload"),
            Some(PluginStatus::ActiveWithReloadError { .. })
        ));
        registry.execute(&mut runtime, "Reloaded").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "old"
        ));
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
