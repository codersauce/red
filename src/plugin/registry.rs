use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use crate::editor::EditorStateSnapshot;
use husk::RequestId;
use semver::{Version, VersionReq};
use serde::Serialize;

use super::{PluginMetadata, Runtime};

pub struct PluginRegistry {
    plugins: Vec<(String, String)>,
    metadata: HashMap<String, PluginMetadata>,
    initialized: bool,
    statuses: HashMap<String, PluginStatus>,
    modified_at: HashMap<String, PluginModification>,
    last_hot_reload_poll: Instant,
}

pub const RED_HOST_API_VERSION: &str = "0.2.0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PluginModification {
    source: Option<SystemTime>,
    metadata: Option<SystemTime>,
}

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
        self.modified_at
            .insert(name.to_string(), plugin_modification(path));

        match plugin_metadata(name, path) {
            Ok(metadata) => {
                self.metadata.insert(name.to_string(), metadata);
            }
            Err(error) => {
                let diagnostic = format!("failed to load plugin metadata: {error}");
                crate::log!("Plugin `{name}` quarantined during metadata: {diagnostic}");
                self.statuses.insert(
                    name.to_string(),
                    PluginStatus::Quarantined {
                        stage: "metadata".to_string(),
                        path: plugin_metadata_path(path)
                            .map_or_else(|| path.to_string(), |path| path.display().to_string()),
                        diagnostic,
                    },
                );
                self.metadata
                    .insert(name.to_string(), PluginMetadata::minimal(name.to_string()));
            }
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
        pending.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        while !pending.is_empty() {
            let mut deferred = Vec::new();
            let mut progressed = false;
            for (name, plugin) in pending {
                if matches!(
                    self.statuses.get(&name),
                    Some(PluginStatus::Quarantined { .. } | PluginStatus::Disabled)
                ) {
                    progressed = true;
                    continue;
                }
                let metadata = self
                    .metadata
                    .get(&name)
                    .cloned()
                    .unwrap_or_else(|| PluginMetadata::minimal(name.clone()));
                if metadata.dependencies.keys().any(|dependency| {
                    matches!(self.statuses.get(dependency), Some(PluginStatus::Pending))
                }) {
                    deferred.push((name, plugin));
                    continue;
                }
                if let Some((stage, diagnostic)) = self.activation_error(&metadata) {
                    self.quarantine(runtime, &name, &plugin, stage, diagnostic);
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
        if let Err(error) = runtime.unload_plugin(name) {
            crate::log!(
                "{}",
                serde_json::json!({
                    "event": "plugin_teardown_failed",
                    "plugin": name,
                    "stage": "quarantine",
                    "error": error.to_string(),
                })
            );
        }
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

    pub async fn notify_plugin(
        &mut self,
        runtime: &mut Runtime,
        plugin: &str,
        event: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<()> {
        let _span = crate::editor::perf::PerfSpan::with_detail("notify_plugin", event);
        for (failed_plugin, error) in runtime.notify_plugin_isolated(plugin, event, args) {
            let path = self
                .plugins
                .iter()
                .find(|(name, _)| name == &failed_plugin)
                .map(|(_, path)| path.clone())
                .unwrap_or_default();
            self.quarantine(runtime, &failed_plugin, &path, "runtime", error.to_string());
        }
        Ok(())
    }

    pub async fn resolve_request(
        &mut self,
        runtime: &mut Runtime,
        request_id: RequestId,
        payload: serde_json::Value,
    ) -> anyhow::Result<bool> {
        let owner = runtime.request_plugin(request_id);
        match runtime.resolve_request(request_id, payload).await {
            Ok(resolved) => Ok(resolved),
            Err(error) => {
                crate::log!(
                    "{}",
                    serde_json::json!({
                        "event": "plugin_request_callback_failed",
                        "plugin": owner.as_deref(),
                        "request_id": request_id.get(),
                        "error": error.to_string(),
                    })
                );
                if let Some(owner) = owner {
                    let path = self
                        .plugins
                        .iter()
                        .find(|(name, _)| name == &owner)
                        .map(|(_, path)| path.clone())
                        .unwrap_or_default();
                    self.quarantine(runtime, &owner, &path, "runtime", error.to_string());
                }
                Ok(true)
            }
        }
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
        let selected = self
            .plugins
            .iter()
            .filter(|(name, _)| !matches!(self.statuses.get(name), Some(PluginStatus::Disabled)))
            .map(|(name, _)| name.clone())
            .collect();
        self.reload_selected(runtime, selected).await;
        Ok(())
    }

    async fn reload_selected(&mut self, runtime: &mut Runtime, selected: HashSet<String>) {
        let previous_metadata = self.metadata.clone();
        let mut pending = Vec::new();
        for (name, path) in self.plugins.clone() {
            if !selected.contains(&name)
                || matches!(self.statuses.get(&name), Some(PluginStatus::Disabled))
            {
                continue;
            }
            if let Err(error) = self.refresh_metadata(&name, &path) {
                self.quarantine(runtime, &name, &path, "metadata", error);
                continue;
            }
            pending.push((name, path));
        }
        pending.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));

        while !pending.is_empty() {
            let pending_names = pending
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<HashSet<_>>();
            let mut deferred = Vec::new();
            let mut progressed = false;
            for (name, path) in pending {
                let waits_for_dependency = self.metadata.get(&name).is_some_and(|metadata| {
                    metadata
                        .dependencies
                        .keys()
                        .any(|dependency| pending_names.contains(dependency.as_str()))
                });
                if waits_for_dependency {
                    deferred.push((name, path));
                    continue;
                }
                self.reload_one_with_metadata(
                    runtime,
                    &name,
                    &path,
                    previous_metadata.get(&name).cloned(),
                )
                .await;
                progressed = true;
            }
            if !progressed {
                for (name, path) in deferred.drain(..) {
                    self.quarantine(
                        runtime,
                        &name,
                        &path,
                        "dependency",
                        "dependency cycle prevents activation".to_string(),
                    );
                }
            }
            pending = deferred;
        }
    }

    pub async fn poll_hot_reload(&mut self, runtime: &mut Runtime) {
        if self.last_hot_reload_poll.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.last_hot_reload_poll = Instant::now();
        let mut affected = HashSet::new();
        for (name, path) in self.plugins.clone() {
            if crate::assets::is_bundled_plugin_specifier(&path) {
                continue;
            }
            let modified = plugin_modification(&path);
            let changed = self
                .modified_at
                .get(&name)
                .is_none_or(|previous| modified != *previous);
            if changed {
                self.modified_at.insert(name.clone(), modified);
                affected.insert(name);
            }
        }

        loop {
            let dependents = self
                .plugins
                .iter()
                .filter(|(name, _)| !affected.contains(name))
                .filter(|(name, _)| {
                    self.metadata.get(name).is_some_and(|metadata| {
                        metadata
                            .dependencies
                            .keys()
                            .any(|dependency| affected.contains(dependency))
                    })
                })
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>();
            if dependents.is_empty() {
                break;
            }
            affected.extend(dependents);
        }

        if !affected.is_empty() {
            self.reload_selected(runtime, affected).await;
        }
    }

    async fn reload_one_with_metadata(
        &mut self,
        runtime: &mut Runtime,
        name: &str,
        path: &str,
        previous_metadata: Option<PluginMetadata>,
    ) {
        if matches!(self.statuses.get(name), Some(PluginStatus::Disabled)) {
            return;
        }
        let was_active = matches!(
            self.statuses.get(name),
            Some(PluginStatus::Active | PluginStatus::ActiveWithReloadError { .. })
        );
        if let Err(error) = self.refresh_metadata(name, path) {
            self.quarantine(runtime, name, path, "metadata", error);
            return;
        }
        let metadata = self
            .metadata
            .get(name)
            .cloned()
            .unwrap_or_else(|| PluginMetadata::minimal(name.to_string()));
        if let Some((stage, diagnostic)) = self.activation_error(&metadata) {
            self.quarantine(runtime, name, path, stage, diagnostic);
            return;
        }
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
                if was_active {
                    if let Some(metadata) = previous_metadata {
                        self.metadata.insert(name.to_string(), metadata);
                    } else {
                        self.metadata.remove(name);
                    }
                    self.statuses.insert(
                        name.to_string(),
                        PluginStatus::ActiveWithReloadError {
                            path: plugin_display_path(path),
                            diagnostic: error.to_string(),
                        },
                    );
                } else {
                    self.quarantine(
                        runtime,
                        name,
                        path,
                        diagnostic_stage(&error),
                        error.to_string(),
                    );
                }
            }
        }
    }

    fn refresh_metadata(&mut self, name: &str, path: &str) -> Result<(), String> {
        let metadata = plugin_metadata(name, path)
            .map_err(|error| format!("failed to load plugin metadata: {error}"))?;
        self.metadata.insert(name.to_string(), metadata);
        Ok(())
    }

    fn activation_error(&self, metadata: &PluginMetadata) -> Option<(&'static str, String)> {
        let mut dependencies = metadata.dependencies.keys().collect::<Vec<_>>();
        dependencies.sort_unstable();
        let missing = dependencies
            .iter()
            .filter(|dependency| !self.statuses.contains_key(dependency.as_str()))
            .map(|dependency| dependency.as_str())
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Some((
                "dependency",
                format!("missing required plugins: {}", missing.join(", ")),
            ));
        }
        if let Some(dependency) = dependencies.iter().find(|dependency| {
            !matches!(
                self.statuses.get(dependency.as_str()),
                Some(PluginStatus::Active | PluginStatus::ActiveWithReloadError { .. })
            )
        }) {
            return Some((
                "dependency",
                format!("required plugin `{dependency}` is not active"),
            ));
        }
        for dependency in dependencies {
            let requirement = &metadata.dependencies[dependency];
            let Some(dependency_metadata) = self.metadata.get(dependency.as_str()) else {
                return Some((
                    "dependency",
                    format!("missing metadata for plugin `{dependency}`"),
                ));
            };
            let requirement = match VersionReq::parse(requirement) {
                Ok(requirement) => requirement,
                Err(error) => {
                    return Some((
                        "dependency",
                        format!("invalid version requirement for plugin `{dependency}`: {error}"),
                    ));
                }
            };
            let version = match Version::parse(&dependency_metadata.version) {
                Ok(version) => version,
                Err(error) => {
                    return Some((
                        "dependency",
                        format!(
                            "plugin `{dependency}` has an invalid version `{}`: {error}",
                            dependency_metadata.version
                        ),
                    ));
                }
            };
            if !requirement.matches(&version) {
                return Some((
                    "dependency",
                    format!(
                        "plugin `{dependency}` version {version} does not satisfy {requirement}"
                    ),
                ));
            }
        }
        check_api_compatibility(metadata)
            .err()
            .map(|error| ("version", error.to_string()))
    }
}

fn plugin_metadata_path(plugin: &str) -> Option<std::path::PathBuf> {
    (!crate::assets::is_bundled_plugin_specifier(plugin))
        .then(|| {
            Path::new(plugin)
                .parent()
                .map(|directory| directory.join("package.json"))
        })
        .flatten()
}

fn plugin_metadata(name: &str, plugin: &str) -> anyhow::Result<PluginMetadata> {
    let Some(path) = plugin_metadata_path(plugin).filter(|path| path.exists()) else {
        return Ok(PluginMetadata::minimal(name.to_string()));
    };
    PluginMetadata::from_file(&path)
}

fn plugin_modification(plugin: &str) -> PluginModification {
    PluginModification {
        source: fs::metadata(plugin)
            .and_then(|metadata| metadata.modified())
            .ok(),
        metadata: plugin_metadata_path(plugin).and_then(|path| {
            fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .ok()
        }),
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
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("red-missing-plugin.hk");
        let expected_error = fs::read_to_string(&missing).unwrap_err().to_string();
        let mut registry = PluginRegistry::new();
        registry.add("missing", missing.to_str().unwrap());
        let mut runtime = Runtime::new();

        registry.initialize(&mut runtime).await.unwrap();
        assert!(matches!(
            registry.statuses().get("missing"),
            Some(PluginStatus::Quarantined { stage, diagnostic, .. })
                if stage == "source" && diagnostic.contains(&expected_error)
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
    async fn targeted_plugin_notification_quarantines_only_the_failing_owner() {
        let dir = tempfile_dir("husk-targeted-notify-error");
        let owner = dir.join("owner.hk");
        let observer = dir.join("observer.hk");
        let source = r#"
            pub fn activate() { red::on("composer:submitted:802", fail); }
            fn fail(prompt: Json) { red::execute(1); }
        "#;
        fs::write(&owner, source).unwrap();
        fs::write(&observer, source).unwrap();
        let mut registry = PluginRegistry::new();
        registry.add("owner", owner.to_str().unwrap());
        registry.add("observer", observer.to_str().unwrap());
        let mut runtime = Runtime::new();

        registry.initialize(&mut runtime).await.unwrap();
        registry
            .notify_plugin(
                &mut runtime,
                "owner",
                "composer:submitted:802",
                serde_json::json!("private prompt"),
            )
            .await
            .unwrap();

        assert!(matches!(
            registry.statuses().get("owner"),
            Some(PluginStatus::Quarantined { stage, .. }) if stage == "runtime"
        ));
        assert_eq!(
            registry.statuses().get("observer"),
            Some(&PluginStatus::Active)
        );
    }

    #[tokio::test]
    async fn failed_request_callback_quarantines_only_its_owner_and_runs_teardown() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let owner_dir = tempfile_dir("request-owner");
        let owner = owner_dir.join("plugin.hk");
        fs::write(
            &owner,
            r#"
                pub fn activate() { red::add_command("OwnerAsk", ask); }
                fn ask() { red::request("GetConfig", loaded, "cwd"); }
                fn loaded(payload: Json) { red::execute("Print", 1 / 0); }
                fn deactivate() { red::execute("AgentCloseSession", "session-1"); }
            "#,
        )
        .unwrap();
        let observer_dir = tempfile_dir("request-observer");
        let observer = observer_dir.join("plugin.hk");
        fs::write(
            &observer,
            r#"
                pub fn activate() { red::add_command("Observer", run); }
                fn run() { red::execute("Print", "observer active"); }
            "#,
        )
        .unwrap();
        let mut registry = PluginRegistry::new();
        registry.add("owner", owner.to_str().unwrap());
        registry.add("observer", observer.to_str().unwrap());
        let mut runtime = Runtime::new();
        registry.initialize(&mut runtime).await.unwrap();
        registry.execute(&mut runtime, "OwnerAsk").await.unwrap();
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("cwd"));
                request_id
            }
            _ => panic!("expected owner config request"),
        };
        assert_eq!(runtime.request_plugin(request_id).as_deref(), Some("owner"));

        assert!(registry
            .resolve_request(
                &mut runtime,
                request_id,
                serde_json::json!({ "value": "/workspace" }),
            )
            .await
            .unwrap());

        assert!(matches!(
            registry.statuses().get("owner"),
            Some(PluginStatus::Quarantined { stage, diagnostic, .. })
                if stage == "runtime" && diagnostic.contains("integer division by zero")
        ));
        assert_eq!(
            registry.statuses().get("observer"),
            Some(&PluginStatus::Active)
        );
        assert_eq!(runtime.command_plugin("OwnerAsk"), None);
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentCloseSession { session_id } if session_id == "session-1"
        ));
        registry.execute(&mut runtime, "Observer").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "observer active"
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
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

    #[test]
    fn pre_one_minor_host_api_requirements_do_not_cross_minor_versions() {
        let mut metadata = PluginMetadata::minimal("composer-plugin".to_string());
        metadata.red_api_version = Some("^0.2.0".to_string());
        check_api_compatibility(&metadata).unwrap();

        metadata.red_api_version = Some("^0.1.0".to_string());
        let error = check_api_compatibility(&metadata).unwrap_err().to_string();

        assert!(error.contains("^0.1.0"));
        assert!(error.contains("0.2.0"));
        assert!(error.contains("docs/PLUGIN_API.md"));
    }

    #[tokio::test]
    async fn malformed_metadata_is_quarantined_before_activation() {
        let dir = tempfile_dir("invalid-metadata");
        let plugin = dir.join("plugin.hk");
        fs::write(&plugin, "pub fn activate() {}").unwrap();
        fs::write(dir.join("package.json"), "pub fn activate() {}").unwrap();
        let mut registry = PluginRegistry::new();
        registry.add("broken", plugin.to_str().unwrap());
        let mut runtime = Runtime::new();

        registry.initialize(&mut runtime).await.unwrap();

        assert!(matches!(
            registry.statuses().get("broken"),
            Some(PluginStatus::Quarantined { stage, diagnostic, .. })
                if stage == "metadata" && diagnostic.contains("failed to load plugin metadata")
        ));
    }

    #[tokio::test]
    async fn invalid_dependency_requirement_is_quarantined() {
        let dependency_dir = tempfile_dir("dependency-valid");
        let dependency = dependency_dir.join("plugin.hk");
        fs::write(&dependency, "pub fn activate() {}").unwrap();
        fs::write(
            dependency_dir.join("package.json"),
            r#"{"name":"dependency","version":"1.2.3"}"#,
        )
        .unwrap();
        let dependent_dir = tempfile_dir("dependency-invalid-requirement");
        let dependent = dependent_dir.join("plugin.hk");
        fs::write(&dependent, "pub fn activate() {}").unwrap();
        fs::write(
            dependent_dir.join("package.json"),
            r#"{"name":"dependent","dependencies":{"dependency":"definitely-not-semver"}}"#,
        )
        .unwrap();

        let mut registry = PluginRegistry::new();
        registry.add("dependency", dependency.to_str().unwrap());
        registry.add("dependent", dependent.to_str().unwrap());
        let mut runtime = Runtime::new();
        registry.initialize(&mut runtime).await.unwrap();

        assert_eq!(
            registry.statuses().get("dependency"),
            Some(&PluginStatus::Active)
        );
        assert!(matches!(
            registry.statuses().get("dependent"),
            Some(PluginStatus::Quarantined { stage, diagnostic, .. })
                if stage == "dependency" && diagnostic.contains("invalid version requirement")
        ));
    }

    #[tokio::test]
    async fn example_package_metadata_and_husk_entrypoint_activate_together() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/example-plugin");
        let entrypoint = root.join("index.hk");
        let mut registry = PluginRegistry::new();
        registry.add("example-plugin", entrypoint.to_str().unwrap());

        let metadata = registry.get_metadata("example-plugin").unwrap();
        assert_eq!(metadata.main, "index.hk");
        assert!(metadata.capabilities.commands);
        assert!(metadata.capabilities.events);

        let mut runtime = Runtime::new();
        registry.initialize(&mut runtime).await.unwrap();
        assert_eq!(
            registry.statuses().get("example-plugin"),
            Some(&PluginStatus::Active)
        );
        registry
            .execute(&mut runtime, "ExampleCommand")
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message == "Hello from the example Husk plugin!"
        ));
    }

    #[tokio::test]
    async fn duplicate_commands_have_deterministic_plugin_name_precedence() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let first_dir = tempfile_dir("duplicate-a");
        let first = first_dir.join("plugin.hk");
        fs::write(
            &first,
            r#"
                pub fn activate() { red::add_command("Shared", run); }
                fn run() { red::execute("Print", "first"); }
            "#,
        )
        .unwrap();
        let second_dir = tempfile_dir("duplicate-z");
        let second = second_dir.join("plugin.hk");
        fs::write(
            &second,
            r#"
                pub fn activate() { red::add_command("Shared", run); }
                fn run() { red::execute("Print", "second"); }
            "#,
        )
        .unwrap();
        let mut registry = PluginRegistry::new();
        registry.add("z-plugin", second.to_str().unwrap());
        registry.add("a-plugin", first.to_str().unwrap());
        let mut runtime = Runtime::new();

        registry.initialize(&mut runtime).await.unwrap();

        assert_eq!(
            registry.statuses().get("a-plugin"),
            Some(&PluginStatus::Active)
        );
        assert!(matches!(
            registry.statuses().get("z-plugin"),
            Some(PluginStatus::Quarantined { stage, diagnostic, .. })
                if matches!(stage.as_str(), "compile" | "activation")
                    && diagnostic.contains("already registered")
        ));
        registry.execute(&mut runtime, "Shared").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "first"
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
        let previous_metadata = registry.get_metadata("reload").cloned();
        registry
            .reload_one_with_metadata(
                &mut runtime,
                "reload",
                plugin.to_str().unwrap(),
                previous_metadata,
            )
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

    #[tokio::test]
    async fn reload_revalidates_api_metadata_and_recovers_a_fixed_quarantined_plugin() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let dir = tempfile_dir("reload-api-metadata");
        let plugin = dir.join("plugin.hk");
        let metadata = dir.join("package.json");
        fs::write(
            &plugin,
            r#"
                pub fn activate() { red::add_command("FutureCommand", run); }
                fn run() { red::execute("Print", "recovered"); }
            "#,
        )
        .unwrap();
        fs::write(
            &metadata,
            r#"{"name":"future","red_api_version":"^99.0.0"}"#,
        )
        .unwrap();
        let mut registry = PluginRegistry::new();
        registry.add("future", plugin.to_str().unwrap());
        let mut runtime = Runtime::new();
        registry.initialize(&mut runtime).await.unwrap();

        registry.reload(&mut runtime).await.unwrap();

        assert!(matches!(
            registry.statuses().get("future"),
            Some(PluginStatus::Quarantined { stage, .. }) if stage == "version"
        ));
        assert_eq!(runtime.command_plugin("FutureCommand"), None);

        fs::write(&metadata, r#"{"name":"future","red_api_version":"^0.2.0"}"#).unwrap();
        registry.reload(&mut runtime).await.unwrap();

        assert_eq!(
            registry.statuses().get("future"),
            Some(&PluginStatus::Active)
        );
        registry
            .execute(&mut runtime, "FutureCommand")
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "recovered"
        ));
    }

    #[tokio::test]
    async fn reload_revalidates_dependency_versions_in_dependency_order_and_recovers() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let dependency_dir = tempfile_dir("reload-dependency");
        let dependency = dependency_dir.join("plugin.hk");
        let dependency_metadata = dependency_dir.join("package.json");
        fs::write(&dependency, "pub fn activate() {}").unwrap();
        fs::write(
            &dependency_metadata,
            r#"{"name":"dependency","version":"1.2.0"}"#,
        )
        .unwrap();
        let dependent_dir = tempfile_dir("reload-dependent");
        let dependent = dependent_dir.join("plugin.hk");
        fs::write(
            &dependent,
            r#"
                pub fn activate() { red::add_command("DependentCommand", run); }
                fn run() { red::execute("Print", "dependent active"); }
            "#,
        )
        .unwrap();
        fs::write(
            dependent_dir.join("package.json"),
            r#"{"name":"dependent","dependencies":{"dependency":"^1.0.0"}}"#,
        )
        .unwrap();
        let mut registry = PluginRegistry::new();
        // Registration order is deliberately the reverse of dependency order.
        registry.add("dependent", dependent.to_str().unwrap());
        registry.add("dependency", dependency.to_str().unwrap());
        let mut runtime = Runtime::new();
        registry.initialize(&mut runtime).await.unwrap();
        assert_eq!(
            runtime.command_plugin("DependentCommand").as_deref(),
            Some("dependent")
        );

        fs::write(
            &dependency_metadata,
            r#"{"name":"dependency","version":"2.0.0"}"#,
        )
        .unwrap();
        registry.reload(&mut runtime).await.unwrap();

        assert_eq!(
            registry.statuses().get("dependency"),
            Some(&PluginStatus::Active)
        );
        assert!(matches!(
            registry.statuses().get("dependent"),
            Some(PluginStatus::Quarantined { stage, diagnostic, .. })
                if stage == "dependency" && diagnostic.contains("does not satisfy")
        ));
        assert_eq!(runtime.command_plugin("DependentCommand"), None);

        fs::write(
            &dependency_metadata,
            r#"{"name":"dependency","version":"1.4.0"}"#,
        )
        .unwrap();
        registry.reload(&mut runtime).await.unwrap();

        assert_eq!(
            registry.statuses().get("dependent"),
            Some(&PluginStatus::Active)
        );
        assert_eq!(
            runtime.command_plugin("DependentCommand").as_deref(),
            Some("dependent")
        );
    }

    #[tokio::test]
    async fn metadata_is_rolled_back_when_an_active_plugins_source_reload_fails() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let dependency_dir = tempfile_dir("metadata-rollback-dependency");
        let dependency = dependency_dir.join("plugin.hk");
        let dependency_metadata = dependency_dir.join("package.json");
        fs::write(&dependency, "pub fn activate() {}").unwrap();
        fs::write(
            &dependency_metadata,
            r#"{"name":"dependency","version":"1.2.0"}"#,
        )
        .unwrap();
        let dependent_dir = tempfile_dir("metadata-rollback-dependent");
        let dependent = dependent_dir.join("plugin.hk");
        fs::write(
            &dependent,
            r#"
                pub fn activate() { red::add_command("DependentCommand", run); }
                fn run() { red::execute("Print", "dependent active"); }
            "#,
        )
        .unwrap();
        fs::write(
            dependent_dir.join("package.json"),
            r#"{"name":"dependent","dependencies":{"dependency":"^1.0.0"}}"#,
        )
        .unwrap();
        let mut registry = PluginRegistry::new();
        registry.add("dependent", dependent.to_str().unwrap());
        registry.add("dependency", dependency.to_str().unwrap());
        let mut runtime = Runtime::new();
        registry.initialize(&mut runtime).await.unwrap();
        fs::write(&dependency, "fn activate( {").unwrap();
        fs::write(
            &dependency_metadata,
            r#"{"name":"dependency","version":"2.0.0"}"#,
        )
        .unwrap();

        registry.reload(&mut runtime).await.unwrap();

        assert!(matches!(
            registry.statuses().get("dependency"),
            Some(PluginStatus::ActiveWithReloadError { .. })
        ));
        assert_eq!(
            registry
                .get_metadata("dependency")
                .map(|metadata| metadata.version.as_str()),
            Some("1.2.0")
        );
        assert_eq!(
            registry.statuses().get("dependent"),
            Some(&PluginStatus::Active)
        );
        registry
            .execute(&mut runtime, "DependentCommand")
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "dependent active"
        ));
    }

    #[tokio::test]
    async fn hot_reload_revalidates_and_recovers_transitive_dependents() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let leaf_dir = tempfile_dir("hot-reload-leaf");
        let leaf = leaf_dir.join("plugin.hk");
        let leaf_metadata = leaf_dir.join("package.json");
        fs::write(&leaf, "pub fn activate() {}").unwrap();
        fs::write(&leaf_metadata, r#"{"name":"leaf","version":"1.2.0"}"#).unwrap();
        let middle_dir = tempfile_dir("hot-reload-middle");
        let middle = middle_dir.join("plugin.hk");
        fs::write(&middle, "pub fn activate() {}").unwrap();
        fs::write(
            middle_dir.join("package.json"),
            r#"{"name":"middle","version":"1.0.0","dependencies":{"leaf":"^1.0.0"}}"#,
        )
        .unwrap();
        let root_dir = tempfile_dir("hot-reload-root");
        let root = root_dir.join("plugin.hk");
        fs::write(
            &root,
            r#"
                pub fn activate() { red::add_command("RootCommand", run); }
                fn run() { red::execute("Print", "root active"); }
            "#,
        )
        .unwrap();
        fs::write(
            root_dir.join("package.json"),
            r#"{"name":"root","dependencies":{"middle":"^1.0.0"}}"#,
        )
        .unwrap();
        let mut registry = PluginRegistry::new();
        registry.add("root", root.to_str().unwrap());
        registry.add("middle", middle.to_str().unwrap());
        registry.add("leaf", leaf.to_str().unwrap());
        let mut runtime = Runtime::new();
        registry.initialize(&mut runtime).await.unwrap();
        assert_eq!(
            runtime.command_plugin("RootCommand").as_deref(),
            Some("root")
        );

        fs::write(&leaf_metadata, r#"{"name":"leaf","version":"2.0.0"}"#).unwrap();
        registry.modified_at.remove("leaf");
        registry.last_hot_reload_poll = Instant::now() - Duration::from_millis(300);
        registry.poll_hot_reload(&mut runtime).await;

        assert_eq!(registry.statuses().get("leaf"), Some(&PluginStatus::Active));
        assert!(matches!(
            registry.statuses().get("middle"),
            Some(PluginStatus::Quarantined { stage, diagnostic, .. })
                if stage == "dependency" && diagnostic.contains("does not satisfy")
        ));
        assert!(matches!(
            registry.statuses().get("root"),
            Some(PluginStatus::Quarantined { stage, diagnostic, .. })
                if stage == "dependency" && diagnostic.contains("not active")
        ));
        assert_eq!(runtime.command_plugin("RootCommand"), None);

        fs::write(&leaf_metadata, r#"{"name":"leaf","version":"1.4.0"}"#).unwrap();
        registry.modified_at.remove("leaf");
        registry.last_hot_reload_poll = Instant::now() - Duration::from_millis(300);
        registry.poll_hot_reload(&mut runtime).await;

        assert_eq!(registry.statuses().get("leaf"), Some(&PluginStatus::Active));
        assert_eq!(
            registry.statuses().get("middle"),
            Some(&PluginStatus::Active)
        );
        assert_eq!(registry.statuses().get("root"), Some(&PluginStatus::Active));
        registry.execute(&mut runtime, "RootCommand").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "root active"
        ));
    }

    #[tokio::test]
    async fn metadata_only_hot_reload_quarantines_incompatible_active_plugin() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let dir = tempfile_dir("reload-metadata-only");
        let plugin = dir.join("plugin.hk");
        let metadata = dir.join("package.json");
        fs::write(
            &plugin,
            r#"
                pub fn activate() { red::add_command("MetadataCommand", run); }
                fn run() { red::execute("Print", "active"); }
            "#,
        )
        .unwrap();
        fs::write(
            &metadata,
            r#"{"name":"metadata","red_api_version":"^0.2.0"}"#,
        )
        .unwrap();
        let mut registry = PluginRegistry::new();
        registry.add("metadata", plugin.to_str().unwrap());
        let mut runtime = Runtime::new();
        registry.initialize(&mut runtime).await.unwrap();
        fs::write(
            &metadata,
            r#"{"name":"metadata","red_api_version":"^99.0.0"}"#,
        )
        .unwrap();
        // Avoid filesystem timestamp-resolution dependencies while still exercising the
        // production metadata/source change comparison.
        registry.modified_at.remove("metadata");
        registry.last_hot_reload_poll = Instant::now() - Duration::from_millis(300);

        registry.poll_hot_reload(&mut runtime).await;

        assert!(matches!(
            registry.statuses().get("metadata"),
            Some(PluginStatus::Quarantined { stage, .. }) if stage == "version"
        ));
        assert_eq!(runtime.command_plugin("MetadataCommand"), None);
    }

    #[tokio::test]
    async fn quarantined_plugin_with_a_missing_dependency_cannot_reactivate_on_reload() {
        let dir = tempfile_dir("reload-missing-dependency");
        let plugin = dir.join("plugin.hk");
        fs::write(
            &plugin,
            r#"
                pub fn activate() { red::add_command("MissingDependencyCommand", run); }
                fn run() { red::execute("Print", "must not run"); }
            "#,
        )
        .unwrap();
        fs::write(
            dir.join("package.json"),
            r#"{"name":"dependent","dependencies":{"missing":"^1.0.0"}}"#,
        )
        .unwrap();
        let mut registry = PluginRegistry::new();
        registry.add("dependent", plugin.to_str().unwrap());
        let mut runtime = Runtime::new();
        registry.initialize(&mut runtime).await.unwrap();

        registry.reload(&mut runtime).await.unwrap();

        assert!(matches!(
            registry.statuses().get("dependent"),
            Some(PluginStatus::Quarantined { stage, diagnostic, .. })
                if stage == "dependency" && diagnostic.contains("missing required plugins")
        ));
        assert_eq!(runtime.command_plugin("MissingDependencyCommand"), None);
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
