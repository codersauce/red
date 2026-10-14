//! Plugin discovery, dependency ordering, quarantine, command dispatch, and transactional reload.
//!
//! [`PluginRegistry`] is the lifecycle authority above the Husk [`Runtime`]. Plugins
//! begin pending, become active only after metadata, compatibility, source, semantic, and
//! activation checks, and otherwise enter a diagnostic [`PluginStatus`]. A required
//! dependency failure cascades to active dependents.
//!
//! Hot reload stages replacement activation and teardown effects in the runtime. A
//! successful reload commits them in lifecycle order; a failure leaves the previous VM,
//! callbacks, commands, and state active and records a reload error. Callers should
//! inspect status rather than assuming a changed source file became live.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime};

use crate::editor::{Action, EditorStateSnapshot, PluginRequest};
use anyhow::Context as _;
use async_trait::async_trait;
use rayon::prelude::*;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

use super::package::{PluginPackageManifest, PLUGIN_MANIFEST_FILE};
use super::process::inherited_environment;
use super::rpc::{
    RpcConnection, RpcErrorObject, RpcIncoming, RpcInitializeParams, RpcRequester,
    RpcStatuslineSide, CLIPBOARD_WRITE_METHOD, CONFIG_GET_METHOD, DOCUMENT_APPLY_METHOD,
    DOCUMENT_CREATE_METHOD, DOCUMENT_EVENT_METHOD, DOCUMENT_SHOW_METHOD, DOCUMENT_SNAPSHOT_METHOD,
    DOCUMENT_UNDO_METHOD, DOCUMENT_UNWATCH_METHOD, DOCUMENT_WATCH_METHOD, MAX_OUTSTANDING_REQUESTS,
    MAX_QUEUED_BYTES, MESSAGE_SHOW_METHOD, PANEL_EVENT_METHOD, PICKER_EVENT_METHOD,
    PICKER_OPEN_METHOD, STATUSLINE_CLEAR_METHOD, STATUSLINE_SET_METHOD, STORAGE_DELETE_METHOD,
    STORAGE_GET_METHOD, STORAGE_SET_METHOD, TEXT_PANEL_CREATE_METHOD, TEXT_PANEL_DISPOSE_METHOD,
    TEXT_PANEL_FOCUS_METHOD, TEXT_PANEL_SET_ACTIONS_METHOD, TEXT_PANEL_SET_CONTENT_METHOD,
};
use super::{
    CommandMetadata, CommandScope, PluginMetadata, RegisteredPluginCommand, RequestId, Runtime,
};

const PARALLEL_PLUGIN_STARTUP_MIN: usize = 4;
const RPC_INITIALIZE_TIMEOUT: Duration = Duration::from_secs(5);
const RPC_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const RPC_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const RPC_EVENT_METHOD: &str = "event/notify";
const RPC_EDITOR_EVENTS: &[&str] = &["editor:ready", "file:opened", "file:saved"];
const MAX_RPC_TEXT_PANELS: usize = 16;
const MAX_RPC_DOCUMENT_WATCHES: usize = 2;
const RPC_DOCUMENT_EVENT_INTERVAL: Duration = Duration::from_millis(50);
type RpcInbox = Arc<Mutex<mpsc::Receiver<RpcIncoming>>>;

/// Lifecycle authority for configured Husk plugins.
pub struct PluginRegistry {
    plugins: Vec<(String, String)>,
    metadata: HashMap<String, PluginMetadata>,
    initialized: bool,
    statuses: HashMap<String, PluginStatus>,
    pending_plugins: usize,
    modified_at: HashMap<String, PluginModification>,
    last_hot_reload_poll: Instant,
    rpc_plugins: HashMap<String, ActiveRpcPlugin>,
    rpc_activations: HashMap<String, PendingRpcPlugin>,
    rpc_retirements: HashMap<String, JoinHandle<()>>,
    rpc_text_panels: HashMap<String, HashMap<String, RpcTextPanel>>,
    rpc_pickers: HashMap<super::PickerHandle, RpcPicker>,
    next_rpc_picker_handle: i32,
    rpc_statuslines: BTreeMap<String, RpcStatuslineContribution>,
    rpc_statusline_dirty: bool,
}

struct RpcStatuslineContribution {
    side: RpcStatuslineSide,
    text: String,
}

struct RpcPicker {
    owner: String,
    id: String,
}

struct RpcTextPanel {
    host_id: String,
    actions: HashSet<String>,
}

fn is_rpc_text_panel_host_id(id: &str) -> bool {
    id.strip_prefix("rpc-text-panel-")
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
}

struct ActiveRpcPlugin {
    connection: RpcConnection,
    incoming: RpcInbox,
    commands: HashMap<String, CommandMetadata>,
    events: HashSet<String>,
    document_watches: HashMap<u64, RpcDocumentWatch>,
    closed_documents: Vec<u64>,
    command: Option<PendingRpcCommand>,
}

#[derive(Default)]
struct RpcDocumentWatch {
    pending_revision: Option<u64>,
    ready_at: Option<Instant>,
}

struct PendingRpcCommand {
    id: tokio::task::Id,
    execution: RpcCommandExecution,
    task: JoinHandle<anyhow::Result<Value>>,
    document: crate::buffer::BufferId,
}

impl Drop for PendingRpcCommand {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct PendingRpcPlugin {
    connection: RpcConnection,
    incoming: RpcInbox,
    task: JoinHandle<anyhow::Result<Value>>,
    manifest: PluginPackageManifest,
    events: HashSet<String>,
    path: String,
    previous_metadata: Option<PluginMetadata>,
    command: Option<PendingRpcInvocation>,
    notifications: Vec<(String, Value)>,
    notification_bytes: usize,
}

struct PendingRpcInvocation {
    id: tokio::task::Id,
    name: String,
    raw_args: String,
    document: crate::buffer::BufferId,
}

/// Identifies one command wait without keeping a retired connection alive.
#[derive(Clone)]
pub(crate) struct RpcCommandToken {
    id: tokio::task::Id,
    owner: String,
    incoming: Weak<Mutex<mpsc::Receiver<RpcIncoming>>>,
}

impl RpcCommandToken {
    pub(crate) fn id(&self) -> tokio::task::Id {
        self.id
    }

    pub(crate) fn is_pending(&self, registry: &PluginRegistry) -> bool {
        let active = registry
            .rpc_plugins
            .get(&self.owner)
            .and_then(|plugin| plugin.command.as_ref())
            .is_some_and(|command| command.id == self.id);
        active
            || registry
                .rpc_activations
                .get(&self.owner)
                .and_then(|plugin| plugin.command.as_ref())
                .is_some_and(|command| command.id == self.id)
    }

    pub(crate) fn is_current(&self, registry: &PluginRegistry) -> bool {
        let active = registry
            .rpc_plugins
            .get(&self.owner)
            .is_some_and(|plugin| Weak::ptr_eq(&self.incoming, &Arc::downgrade(&plugin.incoming)));
        active
            || registry
                .rpc_activations
                .get(&self.owner)
                .is_some_and(|plugin| {
                    Weak::ptr_eq(&self.incoming, &Arc::downgrade(&plugin.incoming))
                })
    }
}

#[async_trait]
pub(crate) trait RpcHost {
    async fn respond(&mut self, owner: &str, incoming: RpcIncoming) -> anyhow::Result<()>;
}

pub(crate) struct RpcCommandExecution {
    owner: String,
    request: RpcRequester,
    incoming: RpcInbox,
    params: Value,
}

impl RpcCommandExecution {
    pub(crate) async fn run(&mut self, host: &mut impl RpcHost) -> anyhow::Result<Value> {
        let request =
            self.request
                .request("command/execute", self.params.clone(), RPC_COMMAND_TIMEOUT);
        tokio::pin!(request);
        let mut incoming = self.incoming.lock().await;
        loop {
            tokio::select! {
                result = &mut request => return result,
                incoming = incoming.recv() => {
                    let incoming = incoming.context("RPC incoming channel closed")?;
                    host.respond(&self.owner, incoming).await?;
                }
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RpcActivation {
    protocol_version: u32,
    #[serde(default)]
    commands: Vec<RpcCommand>,
    #[serde(default)]
    events: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RpcCommand {
    name: String,
    #[serde(default)]
    metadata: CommandMetadata,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RpcCommandResult {
    message: Option<String>,
}

/// Host API version used for plugin compatibility checks.
pub const RED_HOST_API_VERSION: &str = "0.19.0";
pub(crate) const SUPPORTED_HOST_API_VERSIONS: &[&str] = &[
    "0.4.0",
    "0.6.0",
    "0.7.0",
    "0.8.0",
    "0.9.0",
    "0.10.0",
    "0.11.0",
    "0.12.0",
    "0.14.0",
    "0.16.0",
    "0.17.0",
    "0.18.0",
    RED_HOST_API_VERSION,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PluginModification {
    source: Option<SystemTime>,
    metadata: Option<SystemTime>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "status")]
/// Observable lifecycle state of a configured plugin.
pub enum PluginStatus {
    /// Registered but not yet considered for activation.
    Pending,
    /// Loaded, activated, and eligible for callbacks.
    Active,
    /// Previous version remains active because a staged reload failed.
    ActiveWithReloadError {
        /// Source path that failed to replace the active version.
        path: String,
        /// Parse, typecheck, activation, migration, or teardown failure.
        diagnostic: String,
    },
    /// Explicitly disabled by configuration.
    Disabled,
    /// Unloaded after a lifecycle or runtime failure.
    Quarantined {
        /// Lifecycle phase that failed.
        stage: String,
        /// Source or metadata path associated with the failure.
        path: String,
        /// Human-readable failure detail.
        diagnostic: String,
    },
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginRegistry {
    /// Creates an empty, uninitialized registry.
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
            metadata: HashMap::new(),
            initialized: false,
            statuses: HashMap::new(),
            pending_plugins: 0,
            modified_at: HashMap::new(),
            last_hot_reload_poll: Instant::now(),
            rpc_plugins: HashMap::new(),
            rpc_activations: HashMap::new(),
            rpc_retirements: HashMap::new(),
            rpc_text_panels: HashMap::new(),
            rpc_pickers: HashMap::new(),
            next_rpc_picker_handle: -1,
            rpc_statuslines: BTreeMap::new(),
            rpc_statusline_dirty: false,
        }
    }

    pub(crate) fn set_rpc_statusline(
        &mut self,
        owner: &str,
        side: RpcStatuslineSide,
        text: String,
    ) {
        self.rpc_statuslines
            .insert(owner.to_string(), RpcStatuslineContribution { side, text });
        self.rpc_statusline_dirty = true;
    }

    pub(crate) fn clear_rpc_statusline(&mut self, owner: &str) {
        self.rpc_statusline_dirty |= self.rpc_statuslines.remove(owner).is_some();
    }

    pub(crate) fn take_rpc_statusline_dirty(&mut self) -> bool {
        std::mem::take(&mut self.rpc_statusline_dirty)
    }

    pub(crate) fn rpc_statuslines(&self, side: RpcStatuslineSide) -> impl Iterator<Item = &str> {
        self.rpc_statuslines
            .values()
            .filter(move |contribution| contribution.side == side)
            .map(|contribution| contribution.text.as_str())
    }

    pub(crate) fn watch_rpc_document(
        &mut self,
        owner: &str,
        document_id: u64,
    ) -> std::result::Result<(), RpcErrorObject> {
        let plugin = self
            .rpc_plugins
            .get_mut(owner)
            .ok_or_else(|| RpcErrorObject {
                code: -32005,
                message: "RPC plugin connection is not active".to_string(),
                data: None,
            })?;
        if plugin.document_watches.contains_key(&document_id) {
            return Ok(());
        }
        if plugin.document_watches.len() >= MAX_RPC_DOCUMENT_WATCHES {
            return Err(RpcErrorObject {
                code: -32004,
                message: format!(
                    "RPC plugin may watch at most {MAX_RPC_DOCUMENT_WATCHES} documents"
                ),
                data: None,
            });
        }
        plugin
            .document_watches
            .insert(document_id, RpcDocumentWatch::default());
        Ok(())
    }

    pub(crate) fn unwatch_rpc_document(
        &mut self,
        owner: &str,
        document_id: u64,
    ) -> std::result::Result<(), RpcErrorObject> {
        let plugin = self
            .rpc_plugins
            .get_mut(owner)
            .ok_or_else(|| RpcErrorObject {
                code: -32005,
                message: "RPC plugin connection is not active".to_string(),
                data: None,
            })?;
        plugin.document_watches.remove(&document_id);
        Ok(())
    }

    pub(crate) fn queue_rpc_document_changed(&mut self, document_id: u64, revision: u64) {
        let ready_at = Instant::now() + RPC_DOCUMENT_EVENT_INTERVAL;
        for plugin in self.rpc_plugins.values_mut() {
            let Some(watch) = plugin.document_watches.get_mut(&document_id) else {
                continue;
            };
            watch.pending_revision = Some(revision);
            watch.ready_at.get_or_insert(ready_at);
        }
    }

    async fn send_rpc_document_event(&mut self, runtime: &mut Runtime, owner: &str, params: Value) {
        let Some(plugin) = self.rpc_plugins.get(owner) else {
            return;
        };
        if let Err(error) = plugin
            .connection
            .notify(DOCUMENT_EVENT_METHOD, params)
            .await
        {
            let path = self.plugin_path(owner);
            self.quarantine(runtime, owner, &path, "runtime", error.to_string());
        }
    }

    pub(crate) async fn flush_rpc_document_events(&mut self, runtime: &mut Runtime) {
        let now = Instant::now();
        let mut events = Vec::new();
        for (owner, plugin) in &mut self.rpc_plugins {
            for document_id in plugin.closed_documents.drain(..) {
                events.push((owner.clone(), document_id, None));
            }
            for (&document_id, watch) in &mut plugin.document_watches {
                if !watch.ready_at.is_some_and(|ready_at| ready_at <= now) {
                    continue;
                }
                watch.ready_at = None;
                if let Some(revision) = watch.pending_revision.take() {
                    events.push((owner.clone(), document_id, Some(revision)));
                }
            }
        }
        for (owner, document_id, revision) in events {
            let params = match revision {
                Some(revision) => json!({
                    "document_id": document_id.to_string(),
                    "event": "changed",
                    "revision": revision.to_string(),
                }),
                None => json!({
                    "document_id": document_id.to_string(),
                    "event": "closed",
                }),
            };
            self.send_rpc_document_event(runtime, &owner, params).await;
        }
    }

    pub(crate) fn queue_rpc_document_closed(&mut self, document_id: u64) {
        for plugin in self.rpc_plugins.values_mut() {
            if plugin.document_watches.remove(&document_id).is_some() {
                plugin.closed_documents.push(document_id);
            }
        }
    }

    pub(crate) fn register_rpc_text_panel(
        &mut self,
        owner: &str,
        id: &str,
        actions: HashSet<String>,
    ) -> anyhow::Result<(String, Option<String>)> {
        let panels = self.rpc_text_panels.entry(owner.to_string()).or_default();
        if !panels.contains_key(id) {
            anyhow::ensure!(
                panels.len() < MAX_RPC_TEXT_PANELS,
                "plugin may own at most {MAX_RPC_TEXT_PANELS} text panels"
            );
        }
        let host_id = format!("rpc-text-panel-{}", uuid::Uuid::new_v4());
        let replaced = panels
            .insert(
                id.to_string(),
                RpcTextPanel {
                    host_id: host_id.clone(),
                    actions,
                },
            )
            .map(|panel| panel.host_id);
        Ok((host_id, replaced))
    }

    pub(crate) fn rpc_text_panel(&self, owner: &str, id: &str) -> Option<String> {
        self.rpc_text_panels
            .get(owner)?
            .get(id)
            .map(|panel| panel.host_id.clone())
    }

    pub(crate) fn update_rpc_text_panel_actions(
        &mut self,
        owner: &str,
        id: &str,
        actions: HashSet<String>,
    ) -> Option<String> {
        let panel = self.rpc_text_panels.get_mut(owner)?.get_mut(id)?;
        panel.actions = actions;
        Some(panel.host_id.clone())
    }

    pub(crate) fn take_rpc_text_panel(&mut self, owner: &str, id: &str) -> Option<String> {
        self.rpc_text_panels
            .get_mut(owner)?
            .remove(id)
            .map(|panel| panel.host_id)
    }

    fn close_rpc_text_panels(&mut self, runtime: &Runtime, owner: &str) {
        let Some(panels) = self.rpc_text_panels.remove(owner) else {
            return;
        };
        for panel in panels.into_values() {
            runtime.send_request(PluginRequest::ClosePanel { id: panel.host_id });
        }
    }

    pub(crate) fn register_rpc_picker(&mut self, owner: &str, id: String) -> super::PickerHandle {
        let handle = super::PickerHandle::from_raw(self.next_rpc_picker_handle);
        self.next_rpc_picker_handle = self.next_rpc_picker_handle.checked_sub(1).unwrap_or(-1);
        self.rpc_pickers.insert(
            handle,
            RpcPicker {
                owner: owner.to_string(),
                id,
            },
        );
        handle
    }

    pub(crate) fn release_picker(
        &mut self,
        runtime: &mut Runtime,
        handle: super::PickerHandle,
    ) -> bool {
        let rpc = self.rpc_pickers.remove(&handle).is_some();
        runtime.release_picker(handle) || rpc
    }

    fn close_rpc_pickers(&mut self, runtime: &Runtime, owner: &str) {
        let handles = self
            .rpc_pickers
            .iter()
            .filter(|(_, picker)| picker.owner == owner)
            .map(|(handle, _)| *handle)
            .collect::<Vec<_>>();
        for handle in handles {
            self.rpc_pickers.remove(&handle);
            runtime.send_request(PluginRequest::ClosePicker { id: handle.get() });
        }
    }

    /// Registers a plugin source and eagerly reads adjacent metadata.
    ///
    /// Metadata failures quarantine the plugin immediately but do not abort
    /// discovery of unrelated plugins.
    pub fn add(&mut self, name: &str, path: &str) {
        self.plugins.push((name.to_string(), path.to_string()));
        self.set_status(name.to_string(), PluginStatus::Pending);
        self.modified_at
            .insert(name.to_string(), plugin_modification(path));

        match plugin_metadata(name, path) {
            Ok(metadata) => {
                self.metadata.insert(name.to_string(), metadata);
            }
            Err(error) => {
                let diagnostic = format!("failed to load plugin metadata: {error}");
                crate::log!("Plugin `{name}` quarantined during metadata: {diagnostic}");
                self.set_status(
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
    /// Returns current lifecycle status keyed by plugin name.
    pub fn statuses(&self) -> &HashMap<String, PluginStatus> {
        &self.statuses
    }

    fn set_status(&mut self, name: String, status: PluginStatus) {
        let was_pending = matches!(self.statuses.get(&name), Some(PluginStatus::Pending));
        let is_pending = matches!(&status, PluginStatus::Pending);
        if was_pending != is_pending {
            if is_pending {
                self.pending_plugins += 1;
            } else {
                self.pending_plugins -= 1;
            }
        }
        self.statuses.insert(name, status);
    }

    /// Activates plugins in dependency order and quarantines independent failures.
    pub async fn initialize(&mut self, runtime: &mut Runtime) -> anyhow::Result<()> {
        let mut pending = self.plugins.clone();
        pending.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        let mut precompiled = if pending.len() >= PARALLEL_PLUGIN_STARTUP_MIN {
            let typecheck_enabled = runtime.typecheck_enabled();
            pending
                .par_iter()
                .filter(|(name, path)| {
                    !is_husk_package(path)
                        && !is_rpc_package(path)
                        && matches!(self.statuses.get(name), Some(PluginStatus::Pending))
                        && self
                            .metadata
                            .get(name)
                            .is_none_or(|metadata| !is_lazy(metadata))
                })
                .map(|(name, path)| {
                    let source = plugin_source(path).and_then(|source| {
                        super::runtime::compile_startup_plugin(
                            name,
                            &plugin_display_path(path),
                            &source,
                            typecheck_enabled,
                        )
                    });
                    (name.clone(), source)
                })
                .collect::<HashMap<_, _>>()
        } else {
            HashMap::new()
        };
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
                if is_lazy(&metadata) {
                    progressed = true;
                    continue;
                }
                if self.activate_initial_rpc(runtime, &name, &plugin).await {
                    progressed = true;
                    continue;
                }
                let result = if let Some(program) = precompiled.remove(&name) {
                    program.and_then(|program| runtime.load_precompiled_plugin(&name, program))
                } else {
                    Box::pin(self.load_plugin(runtime, &name, &plugin)).await
                };
                match result {
                    Ok(()) => {
                        self.set_status(name, PluginStatus::Active);
                    }
                    Err(error) => {
                        self.quarantine(
                            runtime,
                            &name,
                            &plugin,
                            diagnostic_stage(&error),
                            error.to_string(),
                        );
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
        self.quarantine_one(runtime, name, path, stage, diagnostic);
        let mut unavailable = HashSet::from([name.to_string()]);

        loop {
            let dependents = self
                .plugins
                .iter()
                .filter(|(dependent, _)| !unavailable.contains(dependent))
                .filter(|(dependent, _)| {
                    matches!(
                        self.statuses.get(dependent),
                        Some(PluginStatus::Active | PluginStatus::ActiveWithReloadError { .. })
                    )
                })
                .filter_map(|(dependent, dependent_path)| {
                    let dependency = self
                        .metadata
                        .get(dependent)?
                        .dependencies
                        .keys()
                        .find(|dependency| unavailable.contains(*dependency))?;
                    Some((
                        dependent.clone(),
                        dependent_path.clone(),
                        dependency.clone(),
                    ))
                })
                .collect::<Vec<_>>();
            if dependents.is_empty() {
                break;
            }
            for (dependent, dependent_path, dependency) in dependents {
                self.quarantine_one(
                    runtime,
                    &dependent,
                    &dependent_path,
                    "dependency",
                    format!("required plugin `{dependency}` is not active"),
                );
                unavailable.insert(dependent);
            }
        }
    }

    fn quarantine_one(
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
        self.close_rpc_text_panels(runtime, name);
        self.close_rpc_pickers(runtime, name);
        self.clear_rpc_statusline(name);
        self.rpc_plugins.remove(name);
        if let Some(pending) = self.rpc_activations.remove(name) {
            pending.task.abort();
        }
        crate::log!("Plugin `{name}` quarantined during {stage}: {diagnostic}");
        self.set_status(
            name.to_string(),
            PluginStatus::Quarantined {
                stage: stage.to_string(),
                path: plugin_display_path(path),
                diagnostic,
            },
        );
    }

    async fn load_plugin(
        &mut self,
        runtime: &mut Runtime,
        name: &str,
        path: &str,
    ) -> anyhow::Result<()> {
        load_husk_plugin(runtime, name, path).await
    }

    fn prepare_rpc_plugin(
        &self,
        runtime: &Runtime,
        name: &str,
        root: &Path,
        manifest: &PluginPackageManifest,
        path: String,
        previous_metadata: Option<PluginMetadata>,
    ) -> anyhow::Result<PendingRpcPlugin> {
        let events = validate_rpc_manifest_events(manifest)?;
        let rpc = manifest.rpc.as_ref().expect("RPC manifest checked above");
        let executable = manifest.rpc_command(root).ok_or_else(|| {
            anyhow::anyhow!(
                "JSON-RPC plugin `{}` has no executable for `{}`",
                manifest.plugin.id,
                crate::language::host_target()
            )
        })?;
        let mut command = Command::new(executable);
        command.env_clear().args(&rpc.args).current_dir(root);
        for (key, value) in inherited_environment(&runtime.inherited_environment_keys(name))? {
            command.env(key, value);
        }
        let (connection, incoming) = RpcConnection::spawn(command)?;
        let request = connection.requester();
        let params = RpcInitializeParams {
            protocol_versions: vec![1],
            red_api_version: RED_HOST_API_VERSION.to_string(),
            plugin_id: manifest.plugin.id.to_string(),
            host_methods: vec![
                DOCUMENT_CREATE_METHOD.to_string(),
                DOCUMENT_SHOW_METHOD.to_string(),
                DOCUMENT_WATCH_METHOD.to_string(),
                DOCUMENT_UNWATCH_METHOD.to_string(),
                DOCUMENT_SNAPSHOT_METHOD.to_string(),
                DOCUMENT_APPLY_METHOD.to_string(),
                DOCUMENT_UNDO_METHOD.to_string(),
                CONFIG_GET_METHOD.to_string(),
                STORAGE_GET_METHOD.to_string(),
                STORAGE_SET_METHOD.to_string(),
                STORAGE_DELETE_METHOD.to_string(),
                TEXT_PANEL_CREATE_METHOD.to_string(),
                TEXT_PANEL_SET_ACTIONS_METHOD.to_string(),
                TEXT_PANEL_SET_CONTENT_METHOD.to_string(),
                TEXT_PANEL_FOCUS_METHOD.to_string(),
                TEXT_PANEL_DISPOSE_METHOD.to_string(),
                CLIPBOARD_WRITE_METHOD.to_string(),
                MESSAGE_SHOW_METHOD.to_string(),
                STATUSLINE_SET_METHOD.to_string(),
                STATUSLINE_CLEAR_METHOD.to_string(),
                PICKER_OPEN_METHOD.to_string(),
            ],
        };
        let task = tokio::spawn(async move {
            request
                .request(
                    "initialize",
                    serde_json::to_value(params)?,
                    RPC_INITIALIZE_TIMEOUT,
                )
                .await
        });
        Ok(PendingRpcPlugin {
            connection,
            incoming: Arc::new(Mutex::new(incoming)),
            task,
            manifest: manifest.clone(),
            events,
            path,
            previous_metadata,
            command: None,
            notifications: Vec::new(),
            notification_bytes: 0,
        })
    }

    fn start_rpc_plugin(
        &mut self,
        runtime: &Runtime,
        name: &str,
        root: &Path,
        manifest: &PluginPackageManifest,
        path: String,
        previous_metadata: Option<PluginMetadata>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.rpc_activations.contains_key(name),
            "RPC plugin `{name}` is already starting"
        );
        let pending =
            self.prepare_rpc_plugin(runtime, name, root, manifest, path, previous_metadata)?;
        self.rpc_activations.insert(name.to_string(), pending);
        Ok(())
    }

    fn start_rpc_path(
        &mut self,
        runtime: &Runtime,
        name: &str,
        path: &str,
        previous_metadata: Option<PluginMetadata>,
    ) -> anyhow::Result<bool> {
        let Some((root, manifest)) = external_rpc_package(path)? else {
            return Ok(false);
        };
        self.start_rpc_plugin(
            runtime,
            name,
            &root,
            &manifest,
            path.to_string(),
            previous_metadata,
        )?;
        Ok(true)
    }

    fn cancel_rpc_activation(&mut self, name: &str) -> Option<PluginMetadata> {
        self.rpc_activations.remove(name).and_then(|pending| {
            pending.task.abort();
            pending.previous_metadata
        })
    }

    fn retain_active_rpc_after_reload_error(
        &mut self,
        name: &str,
        path: &str,
        previous_metadata: Option<PluginMetadata>,
        diagnostic: String,
    ) -> bool {
        if !self.rpc_plugins.contains_key(name) {
            return false;
        }
        if let Some(metadata) = previous_metadata {
            self.metadata.insert(name.to_string(), metadata);
        }
        self.set_status(
            name.to_string(),
            PluginStatus::ActiveWithReloadError {
                path: plugin_display_path(path),
                diagnostic,
            },
        );
        true
    }

    async fn activate_initial_rpc(
        &mut self,
        runtime: &mut Runtime,
        name: &str,
        path: &str,
    ) -> bool {
        match self.start_rpc_path(runtime, name, path, None) {
            Ok(true) => self.await_rpc_activation(runtime, name).await,
            Ok(false) => return false,
            Err(error) => self.quarantine(
                runtime,
                name,
                path,
                diagnostic_stage(&error),
                error.to_string(),
            ),
        }
        true
    }

    fn reload_rpc_plugin(
        &mut self,
        runtime: &Runtime,
        name: &str,
        path: &str,
        root: &Path,
        manifest: &PluginPackageManifest,
        previous_metadata: Option<PluginMetadata>,
    ) {
        let previous_metadata = self.cancel_rpc_activation(name).or(previous_metadata);
        if !self.rpc_plugins.contains_key(name) {
            self.set_status(name.to_string(), PluginStatus::Pending);
            return;
        }
        if let Err(error) = self.start_rpc_plugin(
            runtime,
            name,
            root,
            manifest,
            path.to_string(),
            previous_metadata.clone(),
        ) {
            self.retain_active_rpc_after_reload_error(
                name,
                path,
                previous_metadata,
                error.to_string(),
            );
        }
    }

    async fn finish_rpc_activation(
        &mut self,
        runtime: &mut Runtime,
        name: String,
        pending: PendingRpcPlugin,
        result: anyhow::Result<Value>,
    ) {
        let PendingRpcPlugin {
            connection,
            incoming,
            task: _,
            manifest,
            events,
            path,
            previous_metadata,
            command,
            notifications,
            notification_bytes: _,
        } = pending;
        let activation = result
            .and_then(|value| {
                serde_json::from_value(value)
                    .context("RPC plugin returned invalid initialization metadata")
            })
            .and_then(|activation| {
                self.validate_rpc_activation(runtime, &name, &manifest, &events, activation)
            });
        let commands = match activation {
            Ok(commands) => commands,
            Err(error) => {
                self.reject_rpc_activation(runtime, &name, &path, previous_metadata, error);
                return;
            }
        };
        if let Err(error) = connection.finish_initialize().await {
            self.reject_rpc_activation(runtime, &name, &path, previous_metadata, error);
            return;
        }
        let mut plugin = ActiveRpcPlugin {
            connection,
            incoming,
            commands,
            events,
            document_watches: HashMap::new(),
            closed_documents: Vec::new(),
            command: None,
        };
        if let Err(error) =
            Self::begin_activated_rpc_command(runtime, &name, &mut plugin, command).await
        {
            self.reject_rpc_activation(runtime, &name, &path, previous_metadata, error);
            return;
        }
        for (event, payload) in notifications {
            if let Err(error) = plugin
                .connection
                .notify(RPC_EVENT_METHOD, json!({"name": event, "payload": payload}))
                .await
            {
                self.reject_rpc_activation(runtime, &name, &path, previous_metadata, error);
                return;
            }
        }
        if let Some(previous) = self.rpc_plugins.insert(name.clone(), plugin) {
            self.close_rpc_text_panels(runtime, &name);
            self.close_rpc_pickers(runtime, &name);
            self.clear_rpc_statusline(&name);
            self.retire_rpc_plugin(&name, previous);
        }
        self.set_status(name, PluginStatus::Active);
    }

    fn reject_rpc_activation(
        &mut self,
        runtime: &mut Runtime,
        name: &str,
        path: &str,
        previous_metadata: Option<PluginMetadata>,
        error: anyhow::Error,
    ) {
        if !self.retain_active_rpc_after_reload_error(
            name,
            path,
            previous_metadata,
            error.to_string(),
        ) {
            self.quarantine(
                runtime,
                name,
                path,
                diagnostic_stage(&error),
                error.to_string(),
            );
        }
    }

    fn retire_rpc_plugin(&mut self, name: &str, mut plugin: ActiveRpcPlugin) {
        plugin.command.take();
        let ActiveRpcPlugin {
            connection,
            incoming,
            commands: _,
            events: _,
            document_watches: _,
            closed_documents: _,
            command: _,
        } = plugin;
        let task = tokio::spawn(async move {
            let _incoming = incoming;
            if let Err(error) = connection
                .shutdown(RPC_SHUTDOWN_TIMEOUT, RPC_SHUTDOWN_TIMEOUT)
                .await
            {
                crate::log!("RPC plugin shutdown failed: {error}");
            }
        });
        if let Some(previous) = self.rpc_retirements.insert(name.to_string(), task) {
            previous.abort();
        }
    }

    fn rpc_command_execution_for(
        owner: &str,
        plugin: &ActiveRpcPlugin,
        name: String,
        raw_args: String,
    ) -> RpcCommandExecution {
        RpcCommandExecution {
            owner: owner.to_string(),
            request: plugin.connection.requester(),
            incoming: Arc::clone(&plugin.incoming),
            params: json!({
                "name": name,
                "args": raw_args.split_whitespace().collect::<Vec<_>>(),
                "raw_args": raw_args,
            }),
        }
    }

    async fn begin_activated_rpc_command(
        runtime: &Runtime,
        owner: &str,
        plugin: &mut ActiveRpcPlugin,
        command: Option<PendingRpcInvocation>,
    ) -> anyhow::Result<()> {
        let Some(PendingRpcInvocation {
            id,
            name,
            raw_args,
            document,
        }) = command
        else {
            return Ok(());
        };
        if !raw_args.is_empty()
            && !plugin
                .commands
                .get(&name)
                .is_some_and(|metadata| metadata.arguments)
        {
            runtime.send_request(PluginRequest::Action(Action::PrintWarning(format!(
                "unknown plugin command `{name} {raw_args}`"
            ))));
            return Ok(());
        }
        let execution = Self::rpc_command_execution_for(owner, plugin, name, raw_args);
        plugin.command = Some(Self::begin_rpc_command(execution, Some(id), document).await?);
        Ok(())
    }

    async fn begin_rpc_command(
        execution: RpcCommandExecution,
        id: Option<tokio::task::Id>,
        document: crate::buffer::BufferId,
    ) -> anyhow::Result<PendingRpcCommand> {
        let request = execution.request.clone();
        let params = execution.params.clone();
        let response = request
            .begin_request("command/execute", params, RPC_COMMAND_TIMEOUT)
            .await?;
        let task = tokio::spawn(response.wait());
        let id = id.unwrap_or_else(|| task.id());
        Ok(PendingRpcCommand {
            id,
            execution,
            task,
            document,
        })
    }

    fn validate_rpc_activation(
        &self,
        runtime: &Runtime,
        plugin: &str,
        manifest: &PluginPackageManifest,
        events: &HashSet<String>,
        activation: RpcActivation,
    ) -> anyhow::Result<HashMap<String, CommandMetadata>> {
        let commands = validate_rpc_declarations(manifest, events, activation)?;
        for command in commands.keys() {
            if let Some(owner) = runtime.command_plugin(command) {
                anyhow::bail!("command `{command}` is already registered by plugin `{owner}`");
            }
            if let Some((owner, _)) = self.rpc_plugins.iter().find(|(owner, active)| {
                owner.as_str() != plugin && active.commands.contains_key(command)
            }) {
                anyhow::bail!("command `{command}` is already registered by plugin `{owner}`");
            }
        }
        Ok(commands)
    }

    /// Executes a plugin command and quarantines its owner on callback failure.
    pub async fn execute(&mut self, runtime: &mut Runtime, command: &str) -> anyhow::Result<()> {
        Box::pin(self.execute_inner(runtime, command)).await
    }

    async fn execute_inner(&mut self, runtime: &mut Runtime, command: &str) -> anyhow::Result<()> {
        let pending = self
            .pending_command_plugin(command)
            .or_else(|| self.pending_command_plugin(crate::command::split_invocation(command).0));
        if self.command_plugin(runtime, command).is_none() {
            self.ensure_command_registered(runtime, command).await;
            if let Some((owner, _)) = &pending {
                self.await_rpc_activation(runtime, owner).await;
            }
        }
        if let Some((owner, name, raw_args)) = self.resolve_rpc_command(runtime, command) {
            self.execute_rpc_command(runtime, owner, name, raw_args)
                .await;
            return Ok(());
        }
        let owner = runtime.command_plugin(command);
        let Some(owner) = owner else {
            let warning = pending
                .and_then(|(plugin, _)| self.statuses.get(&plugin))
                .and_then(|status| match status {
                    PluginStatus::Quarantined { diagnostic, .. } => Some(diagnostic.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| format!("unknown plugin command `{command}`"));
            runtime.send_request(PluginRequest::Action(Action::PrintWarning(warning)));
            return Ok(());
        };
        if let Err(error) = runtime.execute_command(command).await {
            crate::log!("Plugin command `{command}` failed: {error:?}");
            let path = self.plugin_path(&owner);
            self.quarantine(runtime, &owner, &path, "runtime", error.to_string());
        }
        Ok(())
    }

    async fn execute_rpc_command(
        &mut self,
        runtime: &mut Runtime,
        owner: String,
        name: String,
        raw_args: String,
    ) {
        let mut execution = self.rpc_command_execution(owner, name, raw_args);
        let result = execution.run(&mut UnavailableRpcHost).await;
        self.finish_rpc_command(runtime, &execution, result);
    }

    pub(crate) async fn start_rpc_command(
        &mut self,
        runtime: &mut Runtime,
        command: &str,
        document: crate::buffer::BufferId,
    ) -> anyhow::Result<Option<RpcCommandToken>> {
        if self.command_plugin(runtime, command).is_none() {
            let pending = self.pending_command_plugin(command).or_else(|| {
                self.pending_command_plugin(crate::command::split_invocation(command).0)
            });
            if let Some((owner, path)) = pending.filter(|(_, path)| is_rpc_plugin_path(path)) {
                if !self.rpc_activations.contains_key(&owner) {
                    self.activate_one(runtime, &owner, &path).await;
                }
                if self.rpc_activations.contains_key(&owner) {
                    return self.start_pending_rpc_command(&owner, command, document);
                }
                return Ok(None);
            }
        }
        let Some(execution) = self.prepare_rpc_command(runtime, command).await else {
            return Ok(None);
        };
        let plugin = self
            .rpc_plugins
            .get(&execution.owner)
            .expect("active RPC owner");
        anyhow::ensure!(
            plugin.command.is_none(),
            "RPC plugin `{}` is busy",
            execution.owner
        );
        let owner = execution.owner.clone();
        let incoming = Arc::downgrade(&execution.incoming);
        let command = Self::begin_rpc_command(execution, None, document).await?;
        let token = RpcCommandToken {
            id: command.id,
            owner: owner.clone(),
            incoming,
        };
        self.rpc_plugins
            .get_mut(&owner)
            .expect("active RPC owner")
            .command = Some(command);
        Ok(Some(token))
    }

    fn start_pending_rpc_command(
        &mut self,
        owner: &str,
        command: &str,
        document: crate::buffer::BufferId,
    ) -> anyhow::Result<Option<RpcCommandToken>> {
        let pending = self
            .rpc_activations
            .get_mut(owner)
            .expect("pending RPC plugin activation");
        anyhow::ensure!(
            pending.command.is_none() && pending.notifications.is_empty(),
            "RPC plugin `{owner}` is busy"
        );
        let (name, raw_args) = crate::command::split_invocation(command);
        let id = pending.task.id();
        pending.command = Some(PendingRpcInvocation {
            id,
            name: name.to_string(),
            raw_args: raw_args.to_string(),
            document,
        });
        Ok(Some(RpcCommandToken {
            id,
            owner: owner.to_string(),
            incoming: Arc::downgrade(&pending.incoming),
        }))
    }

    fn queue_rpc_notification(
        &mut self,
        owner: &str,
        event: &str,
        payload: Value,
    ) -> anyhow::Result<()> {
        let params = json!({"name": event, "payload": &payload});
        let bytes = RpcConnection::notification_size(RPC_EVENT_METHOD, &params)?;
        let pending = self
            .rpc_activations
            .get_mut(owner)
            .context("RPC plugin activation is not pending")?;
        anyhow::ensure!(
            pending.notifications.len() < MAX_OUTSTANDING_REQUESTS,
            "RPC plugin `{owner}` has too many queued events"
        );
        let notification_bytes = pending
            .notification_bytes
            .checked_add(bytes)
            .context("RPC queued event size overflow")?;
        anyhow::ensure!(
            notification_bytes <= MAX_QUEUED_BYTES,
            "RPC plugin `{owner}` queued events exceed {MAX_QUEUED_BYTES} bytes"
        );
        pending.notification_bytes = notification_bytes;
        pending.notifications.push((event.to_string(), payload));
        Ok(())
    }

    pub(crate) async fn poll_rpc_lifecycle(&mut self, runtime: &mut Runtime) {
        let completed = self
            .rpc_activations
            .iter()
            .filter(|(_, plugin)| plugin.task.is_finished())
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        for name in completed {
            self.await_rpc_activation(runtime, &name).await;
        }
        self.rpc_retirements.retain(|_, task| !task.is_finished());
    }

    async fn await_rpc_activation(&mut self, runtime: &mut Runtime, name: &str) {
        let Some(mut pending) = self.rpc_activations.remove(name) else {
            return;
        };
        let result = (&mut pending.task)
            .await
            .map_err(anyhow::Error::from)
            .and_then(std::convert::identity);
        self.finish_rpc_activation(runtime, name.to_string(), pending, result)
            .await;
    }

    pub(crate) async fn poll_rpc_commands(&mut self, runtime: &mut Runtime) {
        let completed = self
            .rpc_plugins
            .values_mut()
            .filter_map(|plugin| {
                plugin
                    .command
                    .as_ref()
                    .filter(|command| command.task.is_finished())?;
                plugin.command.take()
            })
            .collect::<Vec<_>>();
        for mut command in completed {
            let result = (&mut command.task)
                .await
                .map_err(anyhow::Error::from)
                .and_then(std::convert::identity);
            self.finish_rpc_command(runtime, &command.execution, result);
        }
    }

    pub(crate) fn rpc_command_document(&self, owner: &str) -> Option<crate::buffer::BufferId> {
        let active = self
            .rpc_plugins
            .get(owner)
            .and_then(|plugin| plugin.command.as_ref())
            .map(|command| command.document);
        active.or_else(|| {
            self.rpc_activations
                .get(owner)?
                .command
                .as_ref()
                .map(|command| command.document)
        })
    }

    pub(crate) async fn prepare_rpc_command(
        &mut self,
        runtime: &mut Runtime,
        command: &str,
    ) -> Option<RpcCommandExecution> {
        if self.command_plugin(runtime, command).is_none() {
            self.ensure_command_registered(runtime, command).await;
        }
        let (owner, name, raw_args) = self.resolve_rpc_command(runtime, command)?;
        Some(self.rpc_command_execution(owner, name, raw_args))
    }

    fn rpc_command_execution(
        &self,
        owner: String,
        name: String,
        raw_args: String,
    ) -> RpcCommandExecution {
        let plugin = self
            .rpc_plugins
            .get(&owner)
            .expect("resolved RPC command owner must be active");
        Self::rpc_command_execution_for(&owner, plugin, name, raw_args)
    }

    pub(crate) fn finish_rpc_command(
        &mut self,
        runtime: &mut Runtime,
        execution: &RpcCommandExecution,
        result: anyhow::Result<Value>,
    ) {
        // A retired connection cannot publish into its replacement.
        if !self
            .rpc_plugins
            .get(&execution.owner)
            .is_some_and(|plugin| Arc::ptr_eq(&plugin.incoming, &execution.incoming))
        {
            return;
        }
        let owner = &execution.owner;
        match result.and_then(decode_rpc_command_result) {
            Ok(Some(action)) => runtime.send_request(PluginRequest::Action(action)),
            Ok(None) => {}
            Err(error) if error.downcast_ref::<RpcErrorObject>().is_some() => {
                runtime.send_request(PluginRequest::Action(Action::PrintWarning(
                    error.to_string(),
                )));
            }
            Err(error) => {
                let path = self.plugin_path(owner);
                self.quarantine(runtime, owner, &path, "runtime", error.to_string());
            }
        }
    }

    pub(crate) fn take_rpc_host_request(
        &mut self,
        runtime: &mut Runtime,
    ) -> Option<(String, RpcIncoming)> {
        for (owner, plugin) in &self.rpc_plugins {
            let Ok(mut incoming) = plugin.incoming.try_lock() else {
                continue;
            };
            match incoming.try_recv() {
                Ok(request) => return Some((owner.clone(), request)),
                Err(mpsc::error::TryRecvError::Empty) => {}
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    let owner = owner.clone();
                    drop(incoming);
                    self.quarantine_rpc(runtime, &owner, anyhow::anyhow!("RPC connection closed"));
                    return None;
                }
            }
        }
        None
    }

    pub(crate) fn quarantine_rpc(
        &mut self,
        runtime: &mut Runtime,
        owner: &str,
        error: anyhow::Error,
    ) {
        let path = self.plugin_path(owner);
        self.quarantine(runtime, owner, &path, "runtime", error.to_string());
    }

    fn resolve_rpc_command(
        &self,
        runtime: &Runtime,
        input: &str,
    ) -> Option<(String, String, String)> {
        let (invoked_name, raw_args) = crate::command::split_invocation(input);
        if runtime.command_plugin(invoked_name).is_some() {
            return None;
        }
        if let Some((plugin, name)) = self.rpc_plugins.iter().find_map(|(plugin, active)| {
            active
                .commands
                .get_key_value(input)
                .map(|(name, _)| (plugin, name))
        }) {
            return Some((plugin.clone(), name.clone(), String::new()));
        }
        self.rpc_plugins.iter().find_map(|(plugin, active)| {
            active
                .commands
                .get_key_value(invoked_name)
                .filter(|(_, metadata)| metadata.arguments)
                .map(|(name, _)| (plugin.clone(), name.clone(), raw_args.to_string()))
        })
    }

    fn plugin_path(&self, plugin: &str) -> String {
        self.plugins
            .iter()
            .find(|(name, _)| name == plugin)
            .map(|(_, path)| path.clone())
            .unwrap_or_default()
    }

    pub(crate) fn command_plugin(&self, runtime: &Runtime, command: &str) -> Option<String> {
        runtime.command_plugin(command).or_else(|| {
            self.resolve_rpc_command(runtime, command)
                .map(|(plugin, _, _)| plugin)
        })
    }

    pub(crate) fn command_scope(&self, runtime: &Runtime, command: &str) -> Option<CommandScope> {
        runtime.command_scope(command).or_else(|| {
            let (plugin, name, _) = self.resolve_rpc_command(runtime, command)?;
            self.rpc_plugins
                .get(&plugin)?
                .commands
                .get(&name)
                .map(|metadata| metadata.scope)
        })
    }

    pub(crate) fn registered_commands(&self, runtime: &Runtime) -> Vec<RegisteredPluginCommand> {
        if self.rpc_plugins.is_empty() && !self.has_pending_rpc_plugin() {
            return runtime.registered_commands();
        }
        self.combined_registered_commands(runtime)
    }

    fn combined_registered_commands(&self, runtime: &Runtime) -> Vec<RegisteredPluginCommand> {
        let mut commands = runtime
            .registered_commands()
            .into_iter()
            .map(|command| (command.name.clone(), command))
            .collect::<HashMap<_, _>>();
        for (plugin, active) in &self.rpc_plugins {
            for (name, metadata) in &active.commands {
                commands
                    .entry(name.clone())
                    .or_insert_with(|| RegisteredPluginCommand {
                        name: name.clone(),
                        plugin: plugin.clone(),
                        metadata: metadata.clone(),
                    });
            }
        }
        let pending = self
            .plugins
            .iter()
            .filter(|(plugin, path)| {
                matches!(self.statuses.get(plugin), Some(PluginStatus::Pending))
                    && is_rpc_plugin_path(path)
            })
            .flat_map(|(plugin, _)| {
                self.metadata
                    .get(plugin)
                    .into_iter()
                    .flat_map(|metadata| &metadata.activation_events)
                    .filter_map(|event| event.strip_prefix("onCommand:"))
                    .filter(|name| {
                        self.pending_command_plugin(name)
                            .is_some_and(|(owner, _)| owner == *plugin)
                    })
                    .map(|name| RegisteredPluginCommand {
                        name: name.to_string(),
                        plugin: plugin.clone(),
                        metadata: CommandMetadata::default(),
                    })
            });
        for command in pending {
            commands.entry(command.name.clone()).or_insert(command);
        }
        let mut commands = commands.into_values().collect::<Vec<_>>();
        commands.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        commands
    }

    fn has_pending_rpc_plugin(&self) -> bool {
        self.plugins.iter().any(|(plugin, path)| {
            matches!(self.statuses.get(plugin), Some(PluginStatus::Pending))
                && is_rpc_plugin_path(path)
        })
    }

    pub(crate) fn has_command(&self, runtime: &Runtime, command: &str) -> bool {
        self.command_plugin(runtime, command).is_some() || self.has_pending_command(command)
    }

    /// Activates the pending plugin that declares `command`, if any.
    pub(crate) async fn ensure_command_registered(&mut self, runtime: &mut Runtime, command: &str) {
        if self.command_plugin(runtime, command).is_some() {
            return;
        }
        if let Some((name, path)) = self
            .pending_command_plugin(command)
            .or_else(|| self.pending_command_plugin(crate::command::split_invocation(command).0))
        {
            if !self.rpc_activations.contains_key(&name) {
                self.activate_one(runtime, &name, &path).await;
            }
        }
    }

    pub(crate) fn has_pending_command(&self, command: &str) -> bool {
        self.pending_command_plugin(command).is_some()
            || self
                .pending_command_plugin(crate::command::split_invocation(command).0)
                .is_some()
    }

    fn pending_command_plugin(&self, command: &str) -> Option<(String, String)> {
        self.plugins
            .iter()
            .filter(|(name, _)| {
                matches!(self.statuses.get(name), Some(PluginStatus::Pending))
                    && self.metadata.get(name).is_some_and(|metadata| {
                        metadata
                            .activation_events
                            .iter()
                            .any(|event| event.strip_prefix("onCommand:") == Some(command))
                    })
            })
            .min_by_key(|(name, path)| (is_rpc_plugin_path(path), name))
            .cloned()
    }

    /// Broadcasts an event while isolating and quarantining per-plugin failures.
    pub async fn notify(
        &mut self,
        runtime: &mut Runtime,
        event: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<()> {
        let _span = crate::editor::perf::PerfSpan::with_detail("notify", event);
        if let Some(panel_id) = event.strip_prefix("panel:event:") {
            if self.notify_rpc_text_panel(runtime, panel_id, &args).await
                || is_rpc_text_panel_host_id(panel_id)
            {
                return Ok(());
            }
        }
        if self.pending_plugins != 0 {
            let pending = self
                .plugins
                .iter()
                .filter(|(name, _)| matches!(self.statuses.get(name), Some(PluginStatus::Pending)))
                .filter(|(name, _)| {
                    self.metadata.get(name).is_some_and(|metadata| {
                        metadata.activation_events.iter().any(|activation| {
                            activation == event || activation == &format!("onEvent:{event}")
                        })
                    })
                })
                .cloned()
                .collect::<Vec<_>>();
            for (name, path) in pending {
                self.activate_for_event(runtime, &name, &path, event, &args)
                    .await;
            }
        }
        for (plugin, error) in runtime.notify_isolated(event, args.clone()) {
            let path = self
                .plugins
                .iter()
                .find(|(name, _)| name == &plugin)
                .map(|(_, path)| path.clone())
                .unwrap_or_default();
            self.quarantine(runtime, &plugin, &path, "runtime", error.to_string());
        }
        self.notify_rpc_plugins(runtime, event, &args).await;
        Ok(())
    }

    async fn notify_rpc_text_panel(
        &mut self,
        runtime: &mut Runtime,
        host_id: &str,
        payload: &Value,
    ) -> bool {
        let action = payload
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some((owner, id, deliver)) = self.rpc_text_panels.iter().find_map(|(owner, panels)| {
            panels.iter().find_map(|(id, panel)| {
                (panel.host_id == host_id).then(|| {
                    (
                        owner.clone(),
                        id.clone(),
                        action == "close" || panel.actions.contains(action),
                    )
                })
            })
        }) else {
            return false;
        };
        if !deliver {
            return true;
        }
        let Some(plugin) = self.rpc_plugins.get(&owner) else {
            return true;
        };
        if let Err(error) = plugin
            .connection
            .notify(PANEL_EVENT_METHOD, json!({"id": id, "action": action}))
            .await
        {
            let path = self.plugin_path(&owner);
            self.quarantine(runtime, &owner, &path, "runtime", error.to_string());
        }
        true
    }

    async fn activate_for_event(
        &mut self,
        runtime: &mut Runtime,
        name: &str,
        path: &str,
        event: &str,
        payload: &Value,
    ) {
        if !is_rpc_plugin_path(path) {
            self.activate_one(runtime, name, path).await;
            return;
        }
        if !self.rpc_activations.contains_key(name) {
            self.activate_one(runtime, name, path).await;
        }
        if self.rpc_activations.contains_key(name) {
            if let Err(error) = self.queue_rpc_notification(name, event, payload.clone()) {
                self.quarantine(runtime, name, path, "runtime", error.to_string());
            }
        }
    }

    async fn notify_rpc_plugins(&mut self, runtime: &mut Runtime, event: &str, payload: &Value) {
        let plugins = self
            .rpc_plugins
            .iter()
            .filter(|(_, plugin)| plugin.events.contains(event))
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        for plugin in plugins {
            let Some(active) = self.rpc_plugins.get(&plugin) else {
                continue;
            };
            let result = active
                .connection
                .notify(RPC_EVENT_METHOD, json!({"name": event, "payload": payload}))
                .await;
            if let Err(error) = result {
                let path = self.plugin_path(&plugin);
                self.quarantine(runtime, &plugin, &path, "runtime", error.to_string());
            }
        }
    }

    /// Sends an event only to one plugin and quarantines it on failure.
    pub async fn notify_plugin(
        &mut self,
        runtime: &mut Runtime,
        plugin: &str,
        event: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<()> {
        let _span = crate::editor::perf::PerfSpan::with_detail("notify_plugin", event);
        if matches!(self.statuses.get(plugin), Some(PluginStatus::Pending)) {
            if let Some((name, path)) = self
                .plugins
                .iter()
                .find(|(name, _)| name == plugin)
                .cloned()
            {
                self.activate_one(runtime, &name, &path).await;
            }
        }
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

    async fn activate_one(&mut self, runtime: &mut Runtime, name: &str, path: &str) {
        let metadata = self
            .metadata
            .get(name)
            .cloned()
            .unwrap_or_else(|| PluginMetadata::minimal(name.to_string()));
        if let Some((stage, diagnostic)) = self.activation_error(&metadata) {
            self.quarantine(runtime, name, path, stage, diagnostic);
            return;
        }
        match self.start_rpc_path(runtime, name, path, None) {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => {
                self.quarantine(
                    runtime,
                    name,
                    path,
                    diagnostic_stage(&error),
                    error.to_string(),
                );
                return;
            }
        }
        match Box::pin(self.load_plugin(runtime, name, path)).await {
            Ok(()) => {
                self.set_status(name.to_string(), PluginStatus::Active);
            }
            Err(error) => self.quarantine(
                runtime,
                name,
                path,
                diagnostic_stage(&error),
                error.to_string(),
            ),
        }
    }

    async fn notify_rpc_picker(
        &mut self,
        runtime: &mut Runtime,
        handle: super::PickerHandle,
        event: crate::editor::PickerCallback,
    ) -> anyhow::Result<bool> {
        let picker = self.rpc_pickers.get(&handle).expect("RPC picker exists");
        let params = match &event {
            crate::editor::PickerCallback::Selected(item) => {
                json!({"id": picker.id, "event": "selected", "item": item})
            }
            crate::editor::PickerCallback::Cancelled => {
                json!({"id": picker.id, "event": "cancelled"})
            }
            _ => return Ok(true),
        };
        let owner = picker.owner.clone();
        self.rpc_pickers.remove(&handle);
        let Some(plugin) = self.rpc_plugins.get(&owner) else {
            return Ok(true);
        };
        if let Err(error) = plugin.connection.notify(PICKER_EVENT_METHOD, params).await {
            let path = self.plugin_path(&owner);
            self.quarantine(runtime, &owner, &path, "runtime", error.to_string());
        }
        Ok(true)
    }

    /// Delivers a callback-scoped picker event only to the plugin that opened it.
    ///
    /// Terminal callbacks are consumed even when the callback fails.
    pub async fn notify_picker(
        &mut self,
        runtime: &mut Runtime,
        handle: super::PickerHandle,
        event: crate::editor::PickerCallback,
    ) -> anyhow::Result<bool> {
        if self.rpc_pickers.contains_key(&handle) {
            return self.notify_rpc_picker(runtime, handle, event).await;
        }
        let owner = runtime.picker_plugin(handle);
        match runtime.notify_picker(handle, event) {
            Ok(resolved) => Ok(resolved),
            Err(error) => {
                crate::log!(
                    "{}",
                    serde_json::json!({
                        "event": "plugin_picker_callback_failed",
                        "plugin": owner.as_deref(),
                        "picker_handle": handle.get(),
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

    /// Delivers a terminal composer result only to the plugin that opened it.
    ///
    /// The callback is consumed even when it fails, so submissions cannot be
    /// replayed accidentally.
    pub async fn notify_composer(
        &mut self,
        runtime: &mut Runtime,
        handle: super::ComposerHandle,
        event: crate::editor::ComposerCallback,
    ) -> anyhow::Result<bool> {
        let owner = runtime.composer_plugin(handle);
        match runtime.notify_composer(handle, event) {
            Ok(resolved) => Ok(resolved),
            Err(error) => {
                crate::log!(
                    "{}",
                    serde_json::json!({
                        "event": "plugin_composer_callback_failed",
                        "plugin": owner.as_deref(),
                        "composer_handle": handle.get(),
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

    /// Resolves a one-shot plugin request and quarantines a failing callback.
    ///
    /// A callback failure still returns `true` because the request ID was
    /// consumed and must not be retried.
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

    /// Invokes `before_exit` with the final editor snapshot after initialization.
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

    /// Runs plugin teardown and marks the registry uninitialized.
    pub async fn deactivate_all(&mut self, runtime: &mut Runtime) -> anyhow::Result<()> {
        if !self.initialized {
            return Ok(());
        }

        for (_, pending) in std::mem::take(&mut self.rpc_activations) {
            pending.task.abort();
        }
        for (name, plugin) in std::mem::take(&mut self.rpc_plugins) {
            self.retire_rpc_plugin(&name, plugin);
        }
        for (_, task) in std::mem::take(&mut self.rpc_retirements) {
            let _ = task.await;
        }
        self.rpc_text_panels.clear();
        self.rpc_pickers.clear();
        self.rpc_statusline_dirty |= !self.rpc_statuslines.is_empty();
        self.rpc_statuslines.clear();
        runtime.deactivate_all().await?;
        self.initialized = false;
        Ok(())
    }

    /// Transactionally reloads every enabled plugin in dependency order.
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
                let previous = self
                    .cancel_rpc_activation(&name)
                    .or_else(|| previous_metadata.get(&name).cloned());
                if !self.retain_active_rpc_after_reload_error(&name, &path, previous, error.clone())
                {
                    self.quarantine(runtime, &name, &path, "metadata", error);
                }
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

    /// Polls filesystem-backed sources and reloads changed plugins and dependents.
    ///
    /// Polling is internally rate-limited and ignores embedded plugin URIs.
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
            if !self.retain_active_rpc_after_reload_error(
                name,
                path,
                previous_metadata,
                error.clone(),
            ) {
                self.quarantine(runtime, name, path, "metadata", error);
            }
            return;
        }
        let metadata = self
            .metadata
            .get(name)
            .cloned()
            .unwrap_or_else(|| PluginMetadata::minimal(name.to_string()));
        if let Some((stage, diagnostic)) = self.activation_error(&metadata) {
            if !self.retain_active_rpc_after_reload_error(
                name,
                path,
                previous_metadata,
                diagnostic.clone(),
            ) {
                self.quarantine(runtime, name, path, stage, diagnostic);
            }
            return;
        }
        match external_rpc_package(path) {
            Ok(Some((root, manifest))) => {
                self.reload_rpc_plugin(
                    runtime,
                    name,
                    path,
                    &root,
                    &manifest,
                    previous_metadata.clone(),
                );
                return;
            }
            Err(error)
                if self.retain_active_rpc_after_reload_error(
                    name,
                    path,
                    previous_metadata.clone(),
                    error.to_string(),
                ) =>
            {
                return;
            }
            Ok(None) | Err(_) => {}
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
                self.set_status(name.to_string(), PluginStatus::Active);
            }
            Err(error) => {
                crate::log!("Plugin `{name}` hot reload rejected: {error}");
                if was_active {
                    if let Some(metadata) = previous_metadata {
                        self.metadata.insert(name.to_string(), metadata);
                    } else {
                        self.metadata.remove(name);
                    }
                    self.set_status(
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

struct UnavailableRpcHost;

#[async_trait]
impl RpcHost for UnavailableRpcHost {
    async fn respond(&mut self, _owner: &str, incoming: RpcIncoming) -> anyhow::Result<()> {
        match incoming {
            RpcIncoming::Request(request) => {
                let error = RpcErrorObject::method_not_found(&request.method);
                request.respond(Err(error)).await
            }
            RpcIncoming::Notification(notification) => {
                crate::log!(
                    "Ignoring unsupported RPC plugin notification `{}`",
                    notification.method
                );
                Ok(())
            }
        }
    }
}

fn validate_rpc_declarations(
    manifest: &PluginPackageManifest,
    events: &HashSet<String>,
    activation: RpcActivation,
) -> anyhow::Result<HashMap<String, CommandMetadata>> {
    anyhow::ensure!(
        activation.protocol_version == 1,
        "invalid RPC protocol version"
    );
    let returned_event_count = activation.events.len();
    let returned_events = activation.events.into_iter().collect::<HashSet<_>>();
    anyhow::ensure!(
        returned_events.len() == returned_event_count,
        "RPC plugin returned duplicate events"
    );
    anyhow::ensure!(
        returned_events == *events,
        "RPC plugin events do not match [activation].events"
    );
    let declared = manifest
        .activation
        .commands
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    anyhow::ensure!(
        declared.len() == manifest.activation.commands.len(),
        "RPC manifest declares duplicate commands"
    );
    let mut commands = HashMap::new();
    for command in activation.commands {
        super::runtime::validate_command_arguments(
            command.metadata.arguments,
            &command.metadata.completions,
        )?;
        anyhow::ensure!(
            commands
                .insert(command.name.clone(), command.metadata)
                .is_none(),
            "RPC plugin returned duplicate command `{}`",
            command.name
        );
    }
    anyhow::ensure!(
        commands.keys().cloned().collect::<HashSet<_>>() == declared,
        "RPC plugin commands do not match [activation].commands"
    );
    Ok(commands)
}

fn validate_rpc_manifest_events(
    manifest: &PluginPackageManifest,
) -> anyhow::Result<HashSet<String>> {
    let events = manifest
        .activation
        .events
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    anyhow::ensure!(
        events.len() == manifest.activation.events.len(),
        "RPC manifest declares duplicate events"
    );
    for event in &events {
        anyhow::ensure!(
            RPC_EDITOR_EVENTS.contains(&event.as_str()),
            "RPC event `{event}` is not supported by protocol version 1"
        );
    }
    Ok(events)
}

fn decode_rpc_command_result(value: Value) -> anyhow::Result<Option<Action>> {
    if value.is_null() {
        return Ok(None);
    }
    let result: RpcCommandResult =
        serde_json::from_value(value).context("RPC command returned an invalid result")?;
    Ok(result.message.map(Action::Print))
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
    if let Some((_, package)) = external_plugin_package(plugin)? {
        return Ok(PluginMetadata::from_package(&package));
    }
    if is_husk_package(plugin) {
        // A standalone Husk package has no Red package metadata, so it loads
        // eagerly with the same minimal metadata as a single-file plugin.
        return Ok(PluginMetadata::minimal(name.to_string()));
    }
    let Some(path) = plugin_metadata_path(plugin).filter(|path| path.exists()) else {
        return Ok(PluginMetadata::minimal(name.to_string()));
    };
    PluginMetadata::from_file(&path)
}

fn is_rpc_plugin_path(path: &str) -> bool {
    Path::new(path).ends_with(PLUGIN_MANIFEST_FILE)
}

fn external_plugin_package(
    plugin: &str,
) -> anyhow::Result<Option<(std::path::PathBuf, PluginPackageManifest)>> {
    if crate::assets::is_bundled_plugin_specifier(plugin) {
        return Ok(None);
    }
    let plugin_path = Path::new(plugin);
    let canonical_plugin = plugin_path.canonicalize().ok();
    for root in plugin_path.ancestors().skip(1) {
        if !root.join(PLUGIN_MANIFEST_FILE).is_file() {
            continue;
        }
        let package = PluginPackageManifest::load(root)?;
        let manifest_path = root.join(PLUGIN_MANIFEST_FILE);
        let matches_rpc = package.rpc.is_some()
            && plugin_paths_match(plugin_path, canonical_plugin.as_deref(), &manifest_path);
        if matches_rpc {
            return Ok(Some((root.to_path_buf(), package)));
        }
        let Some(entry) = package.husk_entry(root) else {
            continue;
        };
        if plugin_paths_match(plugin_path, canonical_plugin.as_deref(), &entry) {
            return Ok(Some((root.to_path_buf(), package)));
        }
    }
    Ok(None)
}

fn plugin_paths_match(plugin: &Path, canonical_plugin: Option<&Path>, candidate: &Path) -> bool {
    candidate == plugin
        || canonical_plugin
            .zip(candidate.canonicalize().ok().as_deref())
            .is_some_and(|(plugin, candidate)| plugin == candidate)
}

fn external_rpc_package(
    plugin: &str,
) -> anyhow::Result<Option<(std::path::PathBuf, PluginPackageManifest)>> {
    Ok(external_plugin_package(plugin)?.filter(|(_, manifest)| manifest.rpc.is_some()))
}

fn plugin_modification(plugin: &str) -> PluginModification {
    if let Ok(Some((root, _))) = external_plugin_package(plugin) {
        return PluginModification {
            source: fs::metadata(plugin)
                .and_then(|metadata| metadata.modified())
                .ok(),
            metadata: fs::metadata(root.join(PLUGIN_MANIFEST_FILE))
                .and_then(|metadata| metadata.modified())
                .ok(),
        };
    }
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
    let compatible = host_api_requirement_is_supported(&requirement)?;
    anyhow::ensure!(
        compatible,
        "plugin requires Red host API `{requirement}`, but this release supports {}; see docs/PLUGIN_API.md",
        SUPPORTED_HOST_API_VERSIONS.join(", ")
    );
    Ok(())
}

pub(crate) fn host_api_requirement_is_supported(requirement: &VersionReq) -> anyhow::Result<bool> {
    Ok(SUPPORTED_HOST_API_VERSIONS
        .iter()
        .map(|version| Version::parse(version))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .any(|version| requirement.matches(&version)))
}

pub(crate) fn host_api_requirement_requires_at_least(
    requirement: &VersionReq,
    introduced: &str,
) -> anyhow::Result<bool> {
    let introduced = Version::parse(introduced)?;
    let supported = SUPPORTED_HOST_API_VERSIONS
        .iter()
        .map(|version| Version::parse(version))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(supported
        .iter()
        .any(|version| version >= &introduced && requirement.matches(version))
        && supported
            .iter()
            .all(|version| version >= &introduced || !requirement.matches(version)))
}

fn diagnostic_stage(error: &anyhow::Error) -> &'static str {
    if error.downcast_ref::<husk_diagnostics::Report>().is_some() {
        "compile"
    } else if error.downcast_ref::<std::io::Error>().is_some() {
        "source"
    } else {
        "activation"
    }
}

fn plugin_source(plugin: &str) -> anyhow::Result<Cow<'static, str>> {
    if crate::assets::is_bundled_plugin_specifier(plugin) {
        return crate::assets::bundled_plugin_contents(plugin)
            .map(Cow::Borrowed)
            .ok_or_else(|| anyhow::anyhow!("bundled plugin `{plugin}` was not found"));
    }

    Ok(Cow::Owned(fs::read_to_string(plugin)?))
}

async fn load_husk_plugin(runtime: &mut Runtime, name: &str, plugin: &str) -> anyhow::Result<()> {
    if is_husk_package(plugin) {
        return runtime.load_plugin_package(name, Path::new(plugin)).await;
    }
    let source = plugin_source(plugin)?;
    runtime
        .load_plugin_at(name, plugin_display_path(plugin), &source)
        .await
}

fn is_rpc_package(plugin: &str) -> bool {
    external_rpc_package(plugin).ok().flatten().is_some()
}

fn is_husk_package(plugin: &str) -> bool {
    !crate::assets::is_bundled_plugin_specifier(plugin)
        && Path::new(plugin).file_name().and_then(|name| name.to_str()) == Some("Husk.toml")
}

fn is_lazy(metadata: &PluginMetadata) -> bool {
    !metadata.activation_events.is_empty()
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
    use crate::editor::{Action, PluginRequest, ACTION_DISPATCHER};
    use std::time::Duration;

    fn drain_requests() {
        while ACTION_DISPATCHER.try_recv_request().is_some() {}
    }

    #[test]
    fn bundled_plugin_sources_borrow_immutable_embedded_assets() {
        let specifier = crate::assets::bundled_plugin_specifier("agent.hk").unwrap();
        let source = plugin_source(&specifier).unwrap();

        assert!(matches!(source, Cow::Borrowed(_)));
        assert!(!source.is_empty());
    }

    #[test]
    fn rpc_command_messages_use_the_existing_print_action() {
        let action = decode_rpc_command_result(serde_json::json!({"message": "hello"}))
            .unwrap()
            .unwrap();

        assert!(matches!(action, Action::Print(message) if message == "hello"));
    }

    #[test]
    fn rpc_quarantine_clears_only_the_owners_ui_resources() {
        drain_requests();
        let mut registry = PluginRegistry::new();
        let mut runtime = Runtime::new();
        let owner_id = registry
            .register_rpc_text_panel("owner", "preview", HashSet::new())
            .unwrap()
            .0;
        let observer_id = registry
            .register_rpc_text_panel("observer", "preview", HashSet::new())
            .unwrap()
            .0;
        let owner_picker = registry.register_rpc_picker("owner", "choices".to_string());
        let observer_picker = registry.register_rpc_picker("observer", "choices".to_string());
        assert!(owner_picker.get() < 0 && observer_picker.get() < 0);
        assert!(!runtime.release_picker(owner_picker));
        registry.set_rpc_statusline(
            "owner",
            RpcStatuslineSide::Right,
            "owner status".to_string(),
        );
        registry.set_rpc_statusline(
            "observer",
            RpcStatuslineSide::Right,
            "observer status".to_string(),
        );

        registry.quarantine_rpc(&mut runtime, "owner", anyhow::anyhow!("connection closed"));

        assert_eq!(registry.rpc_text_panel("owner", "preview"), None);
        assert_eq!(
            registry.rpc_text_panel("observer", "preview"),
            Some(observer_id)
        );
        assert_eq!(
            registry
                .rpc_statuslines(RpcStatuslineSide::Right)
                .collect::<Vec<_>>(),
            vec!["observer status"]
        );
        assert!(!registry.rpc_pickers.contains_key(&owner_picker));
        assert!(registry.rpc_pickers.contains_key(&observer_picker));
        assert!(matches!(
            runtime.try_recv_request(),
            Some(PluginRequest::ClosePanel { id }) if id == owner_id
        ));
        assert!(matches!(
            runtime.try_recv_request(),
            Some(PluginRequest::ClosePicker { id }) if id == owner_picker.get()
        ));
        assert!(runtime.try_recv_request().is_none());
        assert!(matches!(
            registry.statuses().get("owner"),
            Some(PluginStatus::Quarantined { stage, .. }) if stage == "runtime"
        ));
    }

    #[test]
    fn pending_rpc_commands_are_discoverable_before_activation() {
        let root = tempfile_dir("pending-rpc-command");
        let manifest = root.join(PLUGIN_MANIFEST_FILE);
        fs::write(
            &manifest,
            r#"
                schema_version = 1

                [plugin]
                id = "pending-rpc"
                name = "Pending RPC"
                version = "0.1.0"
                red_api = "^0.19.0"

                [rpc]
                command = "rpc-test"

                [activation]
                commands = ["RpcHello"]
            "#,
        )
        .unwrap();
        let mut registry = PluginRegistry::new();
        registry.add("z-pending-rpc", manifest.to_str().unwrap());
        registry.add("pending-rpc", manifest.to_str().unwrap());

        let commands = registry.registered_commands(&Runtime::new());

        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "RpcHello");
        assert_eq!(commands[0].plugin, "pending-rpc");
        assert_eq!(commands[0].metadata, CommandMetadata::default());
        assert_eq!(
            registry.pending_command_plugin("RpcHello"),
            Some(("pending-rpc".to_string(), manifest.display().to_string()))
        );
    }

    #[test]
    fn pending_husk_command_takes_precedence_over_pending_rpc() {
        let root = tempfile_dir("pending-rpc-husk-collision");
        let manifest = root.join(PLUGIN_MANIFEST_FILE);
        fs::write(
            &manifest,
            r#"
                schema_version = 1

                [plugin]
                id = "pending-rpc"
                name = "Pending RPC"
                version = "0.1.0"
                red_api = "^0.19.0"

                [rpc]
                command = "rpc-test"

                [activation]
                commands = ["RpcHello"]
            "#,
        )
        .unwrap();
        let husk = root.join("lazy.hk");
        fs::write(&husk, "pub fn activate() {}\n").unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"name":"lazy-husk","activation_events":["onCommand:RpcHello"]}"#,
        )
        .unwrap();
        let mut registry = PluginRegistry::new();
        registry.add("pending-rpc", manifest.to_str().unwrap());
        registry.add("z-lazy-husk", husk.to_str().unwrap());

        assert!(registry.registered_commands(&Runtime::new()).is_empty());
        assert_eq!(
            registry.pending_command_plugin("RpcHello"),
            Some(("z-lazy-husk".to_string(), husk.display().to_string()))
        );
    }

    #[tokio::test]
    async fn pending_plugin_count_tracks_activation_quarantine_and_lazy_events() {
        let eager_directory = tempfile_dir("pending-count-eager");
        let eager = eager_directory.join("plugin.hk");
        fs::write(&eager, "pub fn activate() {}\n").unwrap();
        let invalid_directory = tempfile_dir("pending-count-invalid");
        let invalid = invalid_directory.join("plugin.hk");
        fs::write(&invalid, "pub fn activate() {}\n").unwrap();
        fs::write(invalid_directory.join("package.json"), "invalid metadata").unwrap();
        let lazy = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/example-plugin/index.hk");

        let mut registry = PluginRegistry::new();
        registry.add("invalid", invalid.to_str().unwrap());
        assert_eq!(registry.pending_plugins, 0);
        registry.add("eager", eager.to_str().unwrap());
        registry.add("example-plugin", lazy.to_str().unwrap());
        assert_eq!(registry.pending_plugins, 2);

        let mut runtime = Runtime::new();
        registry.initialize(&mut runtime).await.unwrap();
        assert_eq!(registry.pending_plugins, 1);
        assert!(matches!(
            registry.statuses().get("example-plugin"),
            Some(PluginStatus::Pending)
        ));

        registry
            .notify(&mut runtime, "cursor:moved", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(registry.pending_plugins, 1);
        registry
            .notify(&mut runtime, "editor:ready", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(registry.pending_plugins, 0);
        assert!(matches!(
            registry.statuses().get("example-plugin"),
            Some(PluginStatus::Active)
        ));
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
        drain_requests();

        let dir = tempfile_dir("husk-command");
        let plugin = dir.join("plugin.hk");
        let manifest = dir.join(PLUGIN_MANIFEST_FILE);
        fs::write(
            &manifest,
            r#"
                schema_version = 1

                [plugin]
                id = "rpc-test"
                name = "RPC Test"
                version = "0.1.0"
                red_api = "^0.19.0"

                [rpc]
                command = "missing-rpc-test"

                [activation]
                commands = ["Hello"]
            "#,
        )
        .unwrap();
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
        registry.add("rpc-test", manifest.to_str().unwrap());
        registry.add("test", plugin.to_str().unwrap());
        let mut runtime = Runtime::new();

        registry.initialize(&mut runtime).await.unwrap();
        assert!(matches!(
            registry.statuses().get("rpc-test"),
            Some(PluginStatus::Pending)
        ));
        assert!(registry
            .registered_commands(&runtime)
            .iter()
            .any(|command| command.name == "Hello" && command.plugin == "test"));
        registry.execute(&mut runtime, "Hello").await.unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::Action(Action::Print(message)) => {
                assert_eq!(message, "hello from registry");
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn lazily_activates_external_package_with_nested_husk_manifest() {
        drain_requests();

        let root = tempfile_dir("external-husk-package");
        let husk_root = root.join("husk");
        fs::create_dir_all(husk_root.join("src")).unwrap();
        fs::write(
            root.join(PLUGIN_MANIFEST_FILE),
            r#"
                schema_version = 1

                [plugin]
                id = "external-husk-package"
                name = "External Husk Package"
                version = "0.1.0"
                red_api = "^0.6.0"
                husk_manifest = "husk/Husk.toml"

                [activation]
                commands = ["ExternalHello"]
            "#,
        )
        .unwrap();
        fs::write(
            husk_root.join("Husk.toml"),
            r#"
                schema_version = 1

                [package]
                name = "external-husk-package"
                version = "0.1.0"
                entry = "src/main.hk"
            "#,
        )
        .unwrap();
        fs::write(
            husk_root.join("src/main.hk"),
            r#"
                pub fn activate() {
                    red::add_command("ExternalHello", hello);
                }

                fn hello() {
                    red::execute("Print", "hello from external package");
                }
            "#,
        )
        .unwrap();

        let mut registry = PluginRegistry::new();
        registry.add(
            "external-husk-package",
            husk_root.join("Husk.toml").to_str().unwrap(),
        );
        let mut runtime = Runtime::new();

        registry.initialize(&mut runtime).await.unwrap();
        assert_eq!(
            registry.statuses().get("external-husk-package"),
            Some(&PluginStatus::Pending)
        );

        registry
            .execute(&mut runtime, "ExternalHello")
            .await
            .unwrap();

        assert_eq!(
            registry.statuses().get("external-husk-package"),
            Some(&PluginStatus::Active)
        );
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message == "hello from external package"
        ));
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
        assert!(
            matches!(
                registry.statuses().get("test"),
                Some(PluginStatus::Quarantined { stage, .. }) if stage == "runtime"
            ),
            "{:?}",
            registry.statuses()
        );
    }

    #[tokio::test]
    async fn runtime_failure_quarantines_active_transitive_dependents() {
        let dependency_dir = tempfile_dir("runtime-failure-dependency");
        let dependency = dependency_dir.join("plugin.hk");
        fs::write(
            &dependency,
            r#"
                pub fn activate() { red::add_command("DependencyFail", fail); }
                fn fail() { red::execute(1); }
            "#,
        )
        .unwrap();
        fs::write(
            dependency_dir.join("package.json"),
            r#"{"name":"dependency","version":"1.0.0"}"#,
        )
        .unwrap();
        let middle_dir = tempfile_dir("runtime-failure-middle");
        let middle = middle_dir.join("plugin.hk");
        fs::write(&middle, "pub fn activate() {}").unwrap();
        fs::write(
            middle_dir.join("package.json"),
            r#"{"name":"middle","version":"1.0.0","dependencies":{"dependency":"^1.0.0"}}"#,
        )
        .unwrap();
        let dependent_dir = tempfile_dir("runtime-failure-dependent");
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
            r#"{"name":"dependent","dependencies":{"middle":"^1.0.0"}}"#,
        )
        .unwrap();
        let mut registry = PluginRegistry::new();
        registry.add("dependent", dependent.to_str().unwrap());
        registry.add("middle", middle.to_str().unwrap());
        registry.add("dependency", dependency.to_str().unwrap());
        let mut runtime = Runtime::new();
        registry.initialize(&mut runtime).await.unwrap();
        assert_eq!(
            runtime.command_plugin("DependentCommand").as_deref(),
            Some("dependent")
        );

        registry
            .execute(&mut runtime, "DependencyFail")
            .await
            .unwrap();

        assert!(matches!(
            registry.statuses().get("dependency"),
            Some(PluginStatus::Quarantined { stage, .. }) if stage == "runtime"
        ));
        assert!(matches!(
            registry.statuses().get("middle"),
            Some(PluginStatus::Quarantined { stage, diagnostic, .. })
                if stage == "dependency" && diagnostic.contains("`dependency`")
        ));
        assert!(matches!(
            registry.statuses().get("dependent"),
            Some(PluginStatus::Quarantined { stage, diagnostic, .. })
                if stage == "dependency" && diagnostic.contains("`middle`")
        ));
        assert_eq!(runtime.command_plugin("DependencyFail"), None);
        assert_eq!(runtime.command_plugin("DependentCommand"), None);
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
    async fn parallel_startup_preserves_activation_order_and_quarantines_compile_failures() {
        drain_requests();
        let directory = tempfile_dir("parallel-startup-order");
        let mut registry = PluginRegistry::new();
        for name in ["zeta", "alpha", "broken", "middle", "omega"] {
            let path = directory.join(format!("{name}.hk"));
            let source = if name == "broken" {
                "fn activate( {".to_string()
            } else {
                format!("pub fn activate() {{ red::execute(\"Print\", \"{name}\"); }}")
            };
            fs::write(&path, source).unwrap();
            registry.add(name, path.to_str().unwrap());
        }
        let mut runtime = Runtime::new();

        registry.initialize(&mut runtime).await.unwrap();

        for expected in ["alpha", "middle", "omega", "zeta"] {
            assert!(matches!(
                ACTION_DISPATCHER.recv_request(),
                PluginRequest::Action(Action::Print(message)) if message == expected
            ));
            assert_eq!(
                registry.statuses().get(expected),
                Some(&PluginStatus::Active)
            );
        }
        assert!(matches!(
            registry.statuses().get("broken"),
            Some(PluginStatus::Quarantined { stage, .. }) if stage == "compile"
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
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
        metadata.red_api_version = Some("^0.4.0".to_string());
        check_api_compatibility(&metadata).unwrap();

        metadata.red_api_version = Some("^0.6.0".to_string());
        check_api_compatibility(&metadata).unwrap();

        metadata.red_api_version = Some("^0.16.0".to_string());
        check_api_compatibility(&metadata).unwrap();

        metadata.red_api_version = Some("^0.17.0".to_string());
        check_api_compatibility(&metadata).unwrap();

        metadata.red_api_version = Some("^0.18.0".to_string());
        check_api_compatibility(&metadata).unwrap();

        metadata.red_api_version = Some("^0.19.0".to_string());
        check_api_compatibility(&metadata).unwrap();

        metadata.red_api_version = Some("^0.5.0".to_string());
        let error = check_api_compatibility(&metadata).unwrap_err().to_string();

        assert!(error.contains("^0.5.0"));
        assert!(error.contains(RED_HOST_API_VERSION));
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
            Some(&PluginStatus::Pending)
        );
        registry
            .execute(&mut runtime, "ExampleCommand")
            .await
            .unwrap();
        assert_eq!(
            registry.statuses().get("example-plugin"),
            Some(&PluginStatus::Active)
        );
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message == "Hello from the example Husk plugin!"
        ));
    }

    #[tokio::test]
    async fn duplicate_commands_have_deterministic_plugin_name_precedence() {
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

        fs::write(&metadata, r#"{"name":"future","red_api_version":"^0.4.0"}"#).unwrap();
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
            r#"{"name":"metadata","red_api_version":"^0.4.0"}"#,
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
