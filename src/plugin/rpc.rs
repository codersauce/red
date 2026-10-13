//! Supervised JSON-RPC 2.0 stdio plugins.
//!
//! JSON-RPC plugins run as child processes that exchange bounded, newline-delimited
//! JSON-RPC 2.0 messages with Red over `stdin` and `stdout`. They share `RedHost` and
//! `RedPluginPolicy` with Husk plugins so commands, event subscriptions, UI handles,
//! one-shot requests, and transactional reload staging obey the same safety and
//! ownership invariants.

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        mpsc::{self, Receiver, RecvTimeoutError, TryRecvError},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    config::Config,
    editor::{ComposerCallback, ComposerCallbackKind, PickerCallback, PickerCallbackKind},
    log,
};

use super::{
    package::{PluginPackageManifest, PLUGIN_MANIFEST_FILE},
    registry::RED_HOST_API_VERSION,
    runtime::{CommandMetadata, PickerHandle, RedHost, RequestId},
    ComposerHandle,
};

const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024;
const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(5);
const JSONRPC_VERSION: &str = "2.0";
const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
const JSONRPC_INVALID_PARAMS: i64 = -32602;
const JSONRPC_INTERNAL_ERROR: i64 = -32000;

const INHERITED_ENVIRONMENT_KEYS: &[&str] = &[
    "HOME",
    "PATH",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "TMP",
    "TEMP",
    "LANG",
    "LC_ALL",
    "TERM",
    "SSH_AUTH_SOCK",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "SYSTEMROOT",
    "SystemRoot",
    "WINDIR",
    "COMSPEC",
    "ComSpec",
    "PATHEXT",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
];

/// Resolved launch specification for a JSON-RPC stdio plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcPluginSpec {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub data_dir: PathBuf,
}

#[derive(Debug)]
enum StdoutEvent {
    Line(Vec<u8>),
    Eof,
    Error(String),
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct InitializeResult {
    commands: Vec<DeclarativeCommand>,
    events: Vec<DeclarativeEvent>,
    config: Vec<DeclarativeConfig>,
}

#[derive(Debug, Deserialize)]
struct DeclarativeCommand {
    name: String,
    #[serde(default)]
    handler: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default = "default_visible")]
    visible: bool,
    #[serde(default)]
    scope: super::CommandScope,
    #[serde(default)]
    arguments: bool,
    #[serde(default)]
    completions: Vec<Vec<String>>,
}

const fn default_visible() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DeclarativeEvent {
    Name(String),
    Detailed {
        event: String,
        #[serde(default)]
        handler: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DeclarativeConfig {
    Key(String),
    Detailed {
        #[serde(default)]
        key: Option<String>,
        #[serde(default)]
        handler: Option<String>,
    },
}

#[derive(Debug, Serialize)]
struct OutboundRequest<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: Value,
}

#[derive(Debug, Serialize)]
struct OutboundResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<OutboundError>,
}

#[derive(Debug, Serialize)]
struct OutboundError {
    code: i64,
    message: String,
}

/// Active JSON-RPC stdio plugin process owned by [`super::Runtime`].
pub(crate) struct RpcPlugin {
    name: String,
    spec: RpcPluginSpec,
    child: Child,
    stdin: ChildStdin,
    stdout_rx: Receiver<StdoutEvent>,
    diagnostics: Arc<Mutex<Vec<u8>>>,
    next_request_id: u64,
    timeout: Duration,
    terminated: bool,
}

impl RpcPlugin {
    /// Spawns, initializes, and activates a JSON-RPC stdio plugin.
    pub(crate) fn spawn_and_activate(
        name: &str,
        spec: RpcPluginSpec,
        imported_state: Option<Value>,
        host: &mut RedHost,
    ) -> Result<Self> {
        let _ = fs::create_dir_all(&spec.data_dir);
        let mut command = Command::new(&spec.command);
        command
            .env_clear()
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for key in INHERITED_ENVIRONMENT_KEYS {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .env("RED_PLUGIN_ID", name)
            .env("RED_PLUGIN_PROTOCOL", "jsonrpc-2.0")
            .env("RED_HOST_API_VERSION", RED_HOST_API_VERSION)
            .env("RED_PLUGIN_DATA_DIR", &spec.data_dir);

        let mut child = command.spawn().with_context(|| {
            format!(
                "failed to start JSON-RPC plugin `{name}` from {}",
                spec.command.display()
            )
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("JSON-RPC plugin `{name}` stdin was not captured"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("JSON-RPC plugin `{name}` stdout was not captured"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("JSON-RPC plugin `{name}` stderr was not captured"))?;

        let stdout_rx = spawn_stdout_reader(stdout);
        let diagnostics = spawn_stderr_reader(name.to_string(), stderr);

        let mut plugin = Self {
            name: name.to_string(),
            spec,
            child,
            stdin,
            stdout_rx,
            diagnostics,
            next_request_id: 1,
            timeout: DEFAULT_RPC_TIMEOUT,
            terminated: false,
        };

        let init_params = json!({
            "plugin_id": name,
            "red_api_version": RED_HOST_API_VERSION,
            "data_dir": plugin.spec.data_dir,
            "snapshots": host.snapshots_json(),
        });
        let init_result = plugin.call_plugin("initialize", init_params, false, host)?;
        plugin.apply_initialize_result(init_result, host)?;

        if let Some(state) = imported_state {
            plugin.call_plugin(
                "lifecycle/stateImport",
                json!({ "state": state.clone() }),
                true,
                host,
            )?;
            plugin.call_plugin("lifecycle/activate", json!({ "state": state }), true, host)?;
        } else {
            plugin.call_plugin("lifecycle/activate", json!({ "state": null }), true, host)?;
        }

        Ok(plugin)
    }

    fn apply_initialize_result(&self, value: Value, host: &mut RedHost) -> Result<()> {
        if value.is_null() {
            return Ok(());
        }
        let Some(object) = value.as_object() else {
            return Ok(());
        };
        if !object.contains_key("commands")
            && !object.contains_key("events")
            && !object.contains_key("config")
        {
            return Ok(());
        }
        let parsed: InitializeResult = serde_json::from_value(value)
            .with_context(|| format!("invalid `initialize` result from plugin `{}`", self.name))?;
        for command in parsed.commands {
            let metadata = CommandMetadata {
                title: command.title,
                category: command.category,
                description: command.description,
                aliases: command.aliases,
                visible: command.visible,
                scope: command.scope,
                arguments: command.arguments,
                completions: command.completions,
            };
            host.add_rpc_command(&self.name, &command.name, command.handler, metadata)?;
        }
        for event in parsed.events {
            match event {
                DeclarativeEvent::Name(name) => {
                    host.add_rpc_event_listener(&self.name, &name, None)?;
                }
                DeclarativeEvent::Detailed { event, handler } => {
                    host.add_rpc_event_listener(&self.name, &event, handler)?;
                }
            }
        }
        for config in parsed.config {
            match config {
                DeclarativeConfig::Key(key) => {
                    host.add_rpc_config_binding(&self.name, Some(key), None)?;
                }
                DeclarativeConfig::Detailed { key, handler } => {
                    host.add_rpc_config_binding(&self.name, key, handler)?;
                }
            }
        }
        Ok(())
    }

    /// Exports state from the live process before a hot-reload swap.
    pub(crate) fn export_state(&mut self, host: &mut RedHost) -> Result<Option<Value>> {
        let result = self.call_plugin("lifecycle/exportState", json!({}), true, host)?;
        if result.is_null() {
            return Ok(None);
        }
        if let Some(state) = result.get("state") {
            return Ok(Some(state.clone()));
        }
        Ok(Some(result))
    }

    /// Runs graceful `lifecycle/deactivate` on the plugin process.
    pub(crate) fn deactivate(&mut self, host: &mut RedHost) -> Result<()> {
        if self.terminated {
            return Ok(());
        }
        self.call_plugin("lifecycle/deactivate", json!({}), true, host)
            .map(drop)
    }

    /// Runs `lifecycle/deactivate` and terminates the child process.
    pub(crate) fn deactivate_and_shutdown(&mut self, host: &mut RedHost) -> Result<()> {
        let result = self.deactivate(host);
        self.kill();
        result
    }

    /// Invokes `lifecycle/beforeExit` with the final editor state snapshot.
    pub(crate) fn before_exit(&mut self, snapshot: &Value, host: &mut RedHost) -> Result<()> {
        if self.terminated {
            return Ok(());
        }
        self.call_plugin(
            "lifecycle/beforeExit",
            json!({ "snapshot": snapshot }),
            true,
            host,
        )
        .map(drop)
    }

    /// Executes a command registered by this JSON-RPC plugin.
    pub(crate) fn execute_command(
        &mut self,
        name: &str,
        handler: Option<&str>,
        invocation: Value,
        host: &mut RedHost,
    ) -> Result<()> {
        let mut params = invocation;
        if let Some(object) = params.as_object_mut() {
            object.insert("command".to_string(), Value::String(name.to_string()));
            if let Some(handler) = handler {
                object.insert("handler".to_string(), Value::String(handler.to_string()));
            }
        }
        self.call_plugin("command/execute", params, false, host)
            .map(drop)
    }

    /// Delivers a subscribed editor event to this JSON-RPC plugin.
    pub(crate) fn notify_event(
        &mut self,
        event: &str,
        handler: Option<&str>,
        payload: &Value,
        host: &mut RedHost,
    ) -> Result<()> {
        let mut params = json!({
            "event": event,
            "payload": payload,
        });
        if let Some(handler) = handler {
            params["handler"] = Value::String(handler.to_string());
        }
        self.call_plugin("event/notify", params, true, host)
            .map(drop)
    }

    /// Delivers a callback-scoped picker event to this JSON-RPC plugin.
    pub(crate) fn notify_picker(
        &mut self,
        handle: PickerHandle,
        kind: PickerCallbackKind,
        handler: Option<&str>,
        event: PickerCallback,
        host: &mut RedHost,
    ) -> Result<()> {
        let kind_name = match kind {
            PickerCallbackKind::Selected => "selected",
            PickerCallbackKind::Cancelled => "cancelled",
            PickerCallbackKind::Changed => "changed",
            PickerCallbackKind::Query => "query",
            PickerCallbackKind::Action => "action",
        };
        let mut params = match event {
            PickerCallback::Selected(item) | PickerCallback::Changed(item) => json!({
                "handle": handle.get(),
                "kind": kind_name,
                "item": item,
            }),
            PickerCallback::Cancelled => json!({
                "handle": handle.get(),
                "kind": kind_name,
            }),
            PickerCallback::Query(query) => json!({
                "handle": handle.get(),
                "kind": kind_name,
                "query": query,
            }),
            PickerCallback::Action {
                action,
                item,
                query,
            } => json!({
                "handle": handle.get(),
                "kind": kind_name,
                "action": action,
                "item": item,
                "query": query,
            }),
        };
        if let Some(handler) = handler {
            params["handler"] = Value::String(handler.to_string());
        }
        self.call_plugin("picker/event", params, true, host)
            .map(drop)
    }

    /// Delivers a callback-scoped composer event to this JSON-RPC plugin.
    pub(crate) fn notify_composer(
        &mut self,
        handle: ComposerHandle,
        kind: ComposerCallbackKind,
        handler: Option<&str>,
        event: ComposerCallback,
        host: &mut RedHost,
    ) -> Result<()> {
        let kind_name = match kind {
            ComposerCallbackKind::Submitted => "submitted",
            ComposerCallbackKind::Cancelled => "cancelled",
        };
        let mut params = match event {
            ComposerCallback::Submitted(text) => json!({
                "handle": handle.get(),
                "kind": kind_name,
                "text": text,
            }),
            ComposerCallback::Cancelled => json!({
                "handle": handle.get(),
                "kind": kind_name,
            }),
        };
        if let Some(handler) = handler {
            params["handler"] = Value::String(handler.to_string());
        }
        self.call_plugin("composer/event", params, true, host)
            .map(drop)
    }

    /// Resolves a one-shot host request initiated by this JSON-RPC plugin.
    pub(crate) fn resolve_request(
        &mut self,
        request_id: RequestId,
        action: &str,
        handler: Option<&str>,
        deferred_rpc_id: Option<Value>,
        payload: &Value,
        host: &mut RedHost,
    ) -> Result<()> {
        if let Some(rpc_id) = deferred_rpc_id {
            self.write_response(OutboundResponse {
                jsonrpc: JSONRPC_VERSION,
                id: rpc_id,
                result: Some(payload.clone()),
                error: None,
            })?;
        }
        let mut params = json!({
            "request_id": request_id.get(),
            "action": action,
            "result": payload,
        });
        if let Some(handler) = handler {
            params["handler"] = Value::String(handler.to_string());
        }
        self.call_plugin("request/resolve", params, true, host)
            .map(drop)
    }

    /// Drains any unsolicited Plugin -> Host messages or process faults.
    pub(crate) fn poll_events(&mut self, host: &mut RedHost) -> Result<()> {
        if self.terminated {
            anyhow::bail!("JSON-RPC plugin `{}` is not running", self.name);
        }
        loop {
            match self.stdout_rx.try_recv() {
                Ok(StdoutEvent::Line(line)) => {
                    let message: Value = serde_json::from_slice(&line).with_context(|| {
                        format!("malformed JSON-RPC frame from plugin `{}`", self.name)
                    })?;
                    self.validate_jsonrpc_envelope(&message)?;
                    if message.get("method").is_some() {
                        self.handle_plugin_message(&message, host)?;
                    }
                }
                Ok(StdoutEvent::Eof) => {
                    self.terminated = true;
                    return Err(self.eof_error("background poll"));
                }
                Ok(StdoutEvent::Error(error)) => {
                    self.kill();
                    anyhow::bail!("JSON-RPC plugin `{}` stdout error: {error}", self.name);
                }
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => {
                    self.terminated = true;
                    return Err(self.eof_error("background poll"));
                }
            }
        }
    }

    fn call_plugin(
        &mut self,
        method: &str,
        params: Value,
        allow_method_not_found: bool,
        host: &mut RedHost,
    ) -> Result<Value> {
        if self.terminated {
            anyhow::bail!("JSON-RPC plugin `{}` has already exited", self.name);
        }
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1).max(1);
        let frame = serde_json::to_vec(&OutboundRequest {
            jsonrpc: JSONRPC_VERSION,
            id: request_id,
            method,
            params,
        })?;
        anyhow::ensure!(
            frame.len() <= MAX_FRAME_BYTES,
            "JSON-RPC host frame exceeds {MAX_FRAME_BYTES} bytes"
        );
        if let Err(error) = self
            .stdin
            .write_all(&frame)
            .and_then(|()| self.stdin.write_all(b"\n"))
            .and_then(|()| self.stdin.flush())
        {
            self.kill();
            return Err(self.io_error(method, error));
        }

        let deadline = Instant::now() + self.timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.kill();
                anyhow::bail!(
                    "JSON-RPC plugin `{}` timed out after {} ms during `{method}`",
                    self.name,
                    self.timeout.as_millis()
                );
            }
            match self.stdout_rx.recv_timeout(remaining) {
                Ok(StdoutEvent::Line(line)) => {
                    let message: Value = match serde_json::from_slice(&line) {
                        Ok(message) => message,
                        Err(error) => {
                            self.kill();
                            anyhow::bail!(
                                "malformed JSON-RPC frame from plugin `{}` during `{method}`: {error}",
                                self.name
                            );
                        }
                    };
                    if let Err(error) = self.validate_jsonrpc_envelope(&message) {
                        self.kill();
                        return Err(error);
                    }
                    if message.get("method").is_some() {
                        if let Err(error) = self.handle_plugin_message(&message, host) {
                            self.kill();
                            return Err(error);
                        }
                        continue;
                    }
                    if !response_matches_id(message.get("id"), request_id) {
                        continue;
                    }
                    if let Some(error) = message.get("error").filter(|value| !value.is_null()) {
                        let code = error.get("code").and_then(Value::as_i64);
                        if allow_method_not_found && code == Some(JSONRPC_METHOD_NOT_FOUND) {
                            return Ok(Value::Null);
                        }
                        let detail = error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown JSON-RPC error");
                        anyhow::bail!(
                            "JSON-RPC plugin `{}` failed in `{method}`: {detail}",
                            self.name
                        );
                    }
                    return Ok(message.get("result").cloned().unwrap_or(Value::Null));
                }
                Ok(StdoutEvent::Eof) | Err(RecvTimeoutError::Disconnected) => {
                    self.terminated = true;
                    return Err(self.eof_error(method));
                }
                Ok(StdoutEvent::Error(error)) => {
                    self.kill();
                    anyhow::bail!(
                        "JSON-RPC plugin `{}` stdout error during `{method}`: {error}",
                        self.name
                    );
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.kill();
                    anyhow::bail!(
                        "JSON-RPC plugin `{}` timed out after {} ms during `{method}`",
                        self.name,
                        self.timeout.as_millis()
                    );
                }
            }
        }
    }

    fn validate_jsonrpc_envelope(&self, message: &Value) -> Result<()> {
        let Some(object) = message.as_object() else {
            anyhow::bail!(
                "JSON-RPC frame from plugin `{}` must be an object",
                self.name
            );
        };
        if let Some(version) = object.get("jsonrpc") {
            anyhow::ensure!(
                version.as_str() == Some(JSONRPC_VERSION),
                "unsupported JSON-RPC version from plugin `{}`: {version}",
                self.name
            );
        }
        Ok(())
    }

    fn handle_plugin_message(&mut self, message: &Value, host: &mut RedHost) -> Result<()> {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                anyhow::anyhow!("JSON-RPC call from `{}` has no string method", self.name)
            })?;
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let id = message.get("id").cloned().filter(|id| !id.is_null());

        if matches!(method, "red/request" | "red::request")
            && params
                .get("defer_response")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            && id.is_some()
        {
            return match dispatch_host_method(&self.name, method, &params, id.clone(), host) {
                Ok(_) => Ok(()),
                Err(error) => {
                    if let Some(id) = id {
                        self.write_response(OutboundResponse {
                            jsonrpc: JSONRPC_VERSION,
                            id,
                            result: None,
                            error: Some(OutboundError {
                                code: JSONRPC_INTERNAL_ERROR,
                                message: error.to_string(),
                            }),
                        })?;
                    }
                    Err(error)
                }
            };
        }

        let outcome = dispatch_host_method(&self.name, method, &params, None, host);
        match (id, outcome) {
            (Some(id), Ok(result)) => self.write_response(OutboundResponse {
                jsonrpc: JSONRPC_VERSION,
                id,
                result: Some(result),
                error: None,
            }),
            (Some(id), Err(error)) => {
                let code = if error.to_string().starts_with("unknown Red host") {
                    JSONRPC_METHOD_NOT_FOUND
                } else {
                    JSONRPC_INVALID_PARAMS
                };
                let _ = self.write_response(OutboundResponse {
                    jsonrpc: JSONRPC_VERSION,
                    id,
                    result: None,
                    error: Some(OutboundError {
                        code,
                        message: error.to_string(),
                    }),
                });
                Err(error)
            }
            (None, Ok(_)) => Ok(()),
            (None, Err(error)) => Err(error),
        }
    }

    fn write_response(&mut self, response: OutboundResponse) -> Result<()> {
        let frame = serde_json::to_vec(&response)?;
        anyhow::ensure!(
            frame.len() <= MAX_FRAME_BYTES,
            "JSON-RPC response exceeds {MAX_FRAME_BYTES} bytes"
        );
        self.stdin.write_all(&frame)?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn eof_error(&self, stage: &str) -> anyhow::Error {
        let detail = self.stderr_summary();
        if detail.is_empty() {
            anyhow::anyhow!(
                "JSON-RPC plugin `{}` exited unexpectedly during `{stage}`",
                self.name
            )
        } else {
            anyhow::anyhow!(
                "JSON-RPC plugin `{}` exited unexpectedly during `{stage}`: {detail}",
                self.name
            )
        }
    }

    fn io_error(&self, stage: &str, error: std::io::Error) -> anyhow::Error {
        let detail = self.stderr_summary();
        if detail.is_empty() {
            anyhow::anyhow!(
                "failed to communicate with JSON-RPC plugin `{}` during `{stage}`: {error}",
                self.name
            )
        } else {
            anyhow::anyhow!(
                "failed to communicate with JSON-RPC plugin `{}` during `{stage}`: {error} ({detail})",
                self.name
            )
        }
    }

    fn stderr_summary(&self) -> String {
        self.diagnostics
            .lock()
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_string())
            .unwrap_or_default()
    }

    fn kill(&mut self) {
        if self.terminated {
            return;
        }
        self.terminated = true;
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl Drop for RpcPlugin {
    fn drop(&mut self) {
        self.kill();
    }
}

fn dispatch_host_method(
    plugin: &str,
    method: &str,
    params: &Value,
    deferred_rpc_id: Option<Value>,
    host: &mut RedHost,
) -> Result<Value> {
    match method {
        "red/execute" | "red::execute" => {
            let (action, args) = extract_action_and_args(params, method)?;
            host.execute_rpc(plugin, &action, &args)
        }
        "red/request" | "red::request" => {
            let (action, args) = extract_action_and_args(params, method)?;
            let handler = params
                .get("handler")
                .or_else(|| params.get("callback"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let request_id = host.request_rpc(plugin, &action, &args, handler, deferred_rpc_id)?;
            Ok(json!({ "request_id": request_id.get() }))
        }
        "red/addCommand" | "red/add_command" | "red::add_command" => {
            let (name, handler, metadata) = extract_command_registration(params)?;
            host.add_rpc_command(plugin, &name, handler, metadata)?;
            Ok(Value::Null)
        }
        "red/on" | "red::on" => {
            let (event, handler) = extract_event_registration(params)?;
            host.add_rpc_event_listener(plugin, &event, handler)?;
            Ok(Value::Null)
        }
        "red/config" | "red::config" => {
            let key = match params {
                Value::String(key) => Some(key.clone()),
                Value::Array(items) => items.first().and_then(Value::as_str).map(str::to_string),
                Value::Object(map) => map.get("key").and_then(Value::as_str).map(str::to_string),
                _ => None,
            };
            let handler = params
                .get("handler")
                .and_then(Value::as_str)
                .map(str::to_string);
            let request_id = host.add_rpc_config_binding(plugin, key, handler)?;
            Ok(json!({ "request_id": request_id.get() }))
        }
        "red/snapshot" => {
            let name = match params {
                Value::String(name) => name.as_str(),
                Value::Array(items) => items
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("`red/snapshot` requires a snapshot name"))?,
                Value::Object(map) => map
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("`red/snapshot` requires a `name` field"))?,
                _ => anyhow::bail!("`red/snapshot` requires a snapshot name"),
            };
            host.query_rpc(plugin, name)
        }
        "red/viewport_layout" | "red::viewport_layout" => host.query_rpc(plugin, "viewport_layout"),
        "red/windows" | "red::windows" => host.query_rpc(plugin, "windows"),
        "red/editor_info" | "red::editor_info" => host.query_rpc(plugin, "editor_info"),
        "red/setTimeout" | "red/set_timeout" => {
            let delay_ms = match params {
                Value::Number(number) => number.as_u64().unwrap_or(0),
                Value::Array(items) => items.first().and_then(Value::as_u64).unwrap_or(0),
                Value::Object(map) => map.get("delay_ms").and_then(Value::as_u64).unwrap_or(0),
                _ => 0,
            };
            let timer_id = host.schedule_timeout_rpc(delay_ms);
            Ok(json!({ "timer_id": timer_id }))
        }
        "red/cancelTimeout" | "red/cancel_timeout" => {
            let timer_id = match params {
                Value::String(id) => id.as_str(),
                Value::Array(items) => items
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("`red/cancelTimeout` requires a timer id"))?,
                Value::Object(map) => map
                    .get("timer_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("`red/cancelTimeout` requires `timer_id`"))?,
                _ => anyhow::bail!("`red/cancelTimeout` requires a timer id"),
            };
            host.cancel_timeout_rpc(timer_id);
            Ok(Value::Null)
        }
        "red/log" | "red::log" => {
            let message = match params {
                Value::String(message) => message.clone(),
                Value::Array(items) => items
                    .iter()
                    .map(|item| {
                        item.as_str()
                            .map_or_else(|| item.to_string(), str::to_string)
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
                Value::Object(map) => map
                    .get("message")
                    .and_then(Value::as_str)
                    .map_or_else(|| params.to_string(), str::to_string),
                other => other.to_string(),
            };
            host.log_rpc(plugin, &message);
            Ok(Value::Null)
        }
        "red/state" | "red::state" => {
            let key = match params {
                Value::String(key) => Some(key.as_str()),
                Value::Array(items) => items.first().and_then(Value::as_str),
                Value::Object(map) => map.get("key").and_then(Value::as_str),
                _ => None,
            };
            Ok(host.state_get_rpc(plugin, key))
        }
        "red/stateBool" | "red/state_bool" | "red::state_bool" => {
            let key = match params {
                Value::String(key) => key.as_str(),
                Value::Array(items) => items
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("`red/stateBool` requires a key"))?,
                Value::Object(map) => map
                    .get("key")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("`red/stateBool` requires `key`"))?,
                _ => anyhow::bail!("`red/stateBool` requires a key"),
            };
            Ok(Value::Bool(
                host.state_get_rpc(plugin, Some(key))
                    .as_bool()
                    .unwrap_or(false),
            ))
        }
        "red/stateSet" | "red/state_set" | "red::state_set" => {
            match params {
                Value::Array(items) if items.len() >= 2 => {
                    let key = items[0]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("`red/stateSet` key must be a string"))?;
                    host.state_set_rpc(plugin, Some(key.to_string()), items[1].clone());
                }
                Value::Object(map) if map.contains_key("key") && map.contains_key("value") => {
                    let key = map["key"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("`red/stateSet` key must be a string"))?;
                    host.state_set_rpc(plugin, Some(key.to_string()), map["value"].clone());
                }
                value => {
                    host.state_set_rpc(plugin, None, value.clone());
                }
            }
            Ok(Value::Null)
        }
        "red/statePatch" | "red/state_patch" | "red::state_patch" => {
            let patch = params.get("patch").unwrap_or(params);
            host.state_patch_rpc(plugin, patch.clone())?;
            Ok(Value::Null)
        }
        other => anyhow::bail!("unknown Red host RPC method `{other}`"),
    }
}

fn extract_action_and_args(params: &Value, method: &str) -> Result<(String, Vec<Value>)> {
    match params {
        Value::Object(map) => {
            let action = map
                .get("action")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`{method}` requires an `action` string"))?
                .to_string();
            let args = match map.get("args") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(items)) => items.clone(),
                Some(single) => vec![single.clone()],
            };
            Ok((action, args))
        }
        Value::Array(items) => {
            let action = items
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`{method}` argument 0 must be an action string"))?
                .to_string();
            Ok((action, items[1..].to_vec()))
        }
        Value::String(action) => Ok((action.clone(), Vec::new())),
        _ => anyhow::bail!("`{method}` params must be an object or array"),
    }
}

fn extract_command_registration(
    params: &Value,
) -> Result<(String, Option<String>, CommandMetadata)> {
    match params {
        Value::String(name) => Ok((name.clone(), None, CommandMetadata::default())),
        Value::Array(items) => {
            let name = items
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`red/addCommand` requires a command name"))?
                .to_string();
            let metadata = items
                .get(1)
                .filter(|value| !value.is_null())
                .cloned()
                .map(serde_json::from_value::<CommandMetadata>)
                .transpose()?
                .unwrap_or_default();
            Ok((name, None, metadata))
        }
        Value::Object(map) => {
            let name = map
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`red/addCommand` requires a `name` string"))?
                .to_string();
            let handler = map
                .get("handler")
                .and_then(Value::as_str)
                .map(str::to_string);
            let metadata = if let Some(metadata_value) = map.get("metadata") {
                serde_json::from_value::<CommandMetadata>(metadata_value.clone())?
            } else {
                let mut filtered = serde_json::Map::new();
                for key in [
                    "title",
                    "category",
                    "description",
                    "aliases",
                    "visible",
                    "scope",
                    "arguments",
                    "completions",
                ] {
                    if let Some(value) = map.get(key) {
                        filtered.insert(key.to_string(), value.clone());
                    }
                }
                serde_json::from_value::<CommandMetadata>(Value::Object(filtered))?
            };
            Ok((name, handler, metadata))
        }
        _ => anyhow::bail!("`red/addCommand` requires a command name or object"),
    }
}

fn extract_event_registration(params: &Value) -> Result<(String, Option<String>)> {
    match params {
        Value::String(event) => Ok((event.clone(), None)),
        Value::Array(items) => {
            let event = items
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`red/on` requires an event name"))?
                .to_string();
            let handler = items.get(1).and_then(Value::as_str).map(str::to_string);
            Ok((event, handler))
        }
        Value::Object(map) => {
            let event = map
                .get("event")
                .or_else(|| map.get("name"))
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`red/on` requires an `event` string"))?
                .to_string();
            let handler = map
                .get("handler")
                .and_then(Value::as_str)
                .map(str::to_string);
            Ok((event, handler))
        }
        _ => anyhow::bail!("`red/on` requires an event name or object"),
    }
}

fn response_matches_id(id: Option<&Value>, expected: u64) -> bool {
    match id {
        Some(Value::Number(number)) => number.as_u64() == Some(expected),
        Some(Value::String(text)) => text.parse::<u64>().ok() == Some(expected),
        _ => false,
    }
}

fn spawn_stdout_reader(stdout: ChildStdout) -> Receiver<StdoutEvent> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = Vec::new();
        loop {
            let available = match reader.fill_buf() {
                Ok([]) => {
                    if !line.is_empty() {
                        trim_line_end(&mut line);
                        if !line.is_empty() {
                            let _ = tx.send(StdoutEvent::Line(std::mem::take(&mut line)));
                        }
                    }
                    let _ = tx.send(StdoutEvent::Eof);
                    break;
                }
                Ok(buf) => buf,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    let _ = tx.send(StdoutEvent::Error(error.to_string()));
                    break;
                }
            };

            if let Some(newline_pos) = available.iter().position(|&byte| byte == b'\n') {
                let chunk_len = newline_pos + 1;
                if line.len() + chunk_len > MAX_FRAME_BYTES {
                    let _ = tx.send(StdoutEvent::Error(format!(
                        "JSON-RPC frame exceeds {MAX_FRAME_BYTES} bytes"
                    )));
                    break;
                }
                line.extend_from_slice(&available[..chunk_len]);
                reader.consume(chunk_len);
                trim_line_end(&mut line);
                if !line.is_empty()
                    && tx
                        .send(StdoutEvent::Line(std::mem::take(&mut line)))
                        .is_err()
                {
                    break;
                }
            } else {
                let chunk_len = available.len();
                if line.len() + chunk_len > MAX_FRAME_BYTES {
                    let _ = tx.send(StdoutEvent::Error(format!(
                        "JSON-RPC frame exceeds {MAX_FRAME_BYTES} bytes"
                    )));
                    break;
                }
                line.extend_from_slice(available);
                reader.consume(chunk_len);
            }
        }
    });
    rx
}

fn trim_line_end(line: &mut Vec<u8>) {
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
}

fn spawn_stderr_reader(
    plugin_name: String,
    mut stderr: std::process::ChildStderr,
) -> Arc<Mutex<Vec<u8>>> {
    let diagnostics = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&diagnostics);
    thread::spawn(move || {
        let mut chunk = [0_u8; 1024];
        while let Ok(read) = stderr.read(&mut chunk) {
            if read == 0 {
                break;
            }
            let text = String::from_utf8_lossy(&chunk[..read]);
            for line in text.lines() {
                if !line.trim().is_empty() {
                    log!("[PLUGIN:RPC:{plugin_name}] {line}");
                }
            }
            if let Ok(mut retained) = sink.lock() {
                retained.extend_from_slice(&chunk[..read]);
                if retained.len() > MAX_DIAGNOSTIC_BYTES {
                    let overflow = retained.len() - MAX_DIAGNOSTIC_BYTES;
                    retained.drain(..overflow);
                }
            }
        }
    });
    diagnostics
}

/// Checks whether a configured plugin path refers to a JSON-RPC stdio plugin.
#[must_use]
pub(crate) fn is_rpc_plugin(name: &str, plugin_path: &str) -> bool {
    resolve_rpc_plugin_spec(name, plugin_path)
        .ok()
        .flatten()
        .is_some()
}

/// Resolves a configured plugin path into an [`RpcPluginSpec`] when it targets a JSON-RPC plugin.
pub(crate) fn resolve_rpc_plugin_spec(
    name: &str,
    plugin_path: &str,
) -> Result<Option<RpcPluginSpec>> {
    if crate::assets::is_bundled_plugin_specifier(plugin_path) {
        return Ok(None);
    }
    let path = Path::new(plugin_path);
    if path.extension().and_then(|ext| ext.to_str()) == Some("hk")
        || path.file_name().and_then(|file| file.to_str()) == Some("Husk.toml")
    {
        return Ok(None);
    }

    if path.file_name().and_then(|file| file.to_str()) == Some(PLUGIN_MANIFEST_FILE)
        && path.is_file()
    {
        let root = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("plugin manifest has no parent directory"))?;
        let manifest = PluginPackageManifest::load(root)?;
        if manifest.rpc_manifest().is_some() {
            return spec_from_package_manifest(&manifest, root).map(Some);
        }
        return Ok(None);
    }

    let canonical_plugin = path.canonicalize().ok();
    for root in path.ancestors().skip(1) {
        if !root.join(PLUGIN_MANIFEST_FILE).is_file() {
            continue;
        }
        let manifest = PluginPackageManifest::load(root)?;
        if manifest.rpc_manifest().is_some() {
            if let Some(entry) = manifest.rpc_entry(root) {
                let canonical_entry = entry.canonicalize().ok();
                let matches = entry == path
                    || canonical_plugin
                        .as_ref()
                        .zip(canonical_entry.as_ref())
                        .is_some_and(|(left, right)| left == right);
                if matches {
                    return spec_from_package_manifest(&manifest, root).map(Some);
                }
            }
        }
        if manifest.husk_entry(root).is_some() {
            return Ok(None);
        }
    }

    if !path.is_file() {
        return Ok(None);
    }

    let declared_jsonrpc_in_package_json = path
        .parent()
        .map(|dir| dir.join("package.json"))
        .filter(|pkg| pkg.is_file())
        .and_then(|pkg| super::PluginMetadata::from_file(&pkg).ok())
        .and_then(|meta| meta.protocol)
        .is_some_and(|protocol| protocol == "jsonrpc" || protocol == "json-rpc");

    if declared_jsonrpc_in_package_json || is_executable_file(path) {
        ensure_executable(path)?;
        let cwd = path
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let data_dir = Config::config_dir().join("plugin-data").join(name);
        return Ok(Some(RpcPluginSpec {
            command: path.to_path_buf(),
            args: Vec::new(),
            cwd,
            data_dir,
        }));
    }

    Ok(None)
}

fn spec_from_package_manifest(
    manifest: &PluginPackageManifest,
    package_root: &Path,
) -> Result<RpcPluginSpec> {
    let (command, args) = manifest.rpc_command(package_root).ok_or_else(|| {
        anyhow::anyhow!(
            "plugin `{}` has no JSON-RPC executable for this platform",
            manifest.plugin.id
        )
    })?;
    ensure_executable_in_package(&command, package_root)?;
    let data_dir = Config::config_dir()
        .join("plugin-data")
        .join(manifest.plugin.id.as_str());
    Ok(RpcPluginSpec {
        command,
        args,
        cwd: package_root.to_path_buf(),
        data_dir,
    })
}

fn ensure_executable_in_package(command: &Path, package_root: &Path) -> Result<()> {
    let canonical_root = package_root
        .canonicalize()
        .with_context(|| format!("failed to resolve package root {}", package_root.display()))?;
    let canonical_command = command.canonicalize().with_context(|| {
        format!(
            "failed to resolve JSON-RPC executable {}",
            command.display()
        )
    })?;
    anyhow::ensure!(
        canonical_command.starts_with(&canonical_root),
        "JSON-RPC executable escapes package root: {}",
        command.display()
    );
    anyhow::ensure!(
        canonical_command.is_file(),
        "JSON-RPC executable is not a regular file: {}",
        command.display()
    );
    ensure_executable(&canonical_command)
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && (metadata.permissions().mode() & 0o111 != 0))
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
        && matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("exe" | "cmd" | "bat" | "ps1")
        )
}

#[cfg(unix)]
fn ensure_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = fs::metadata(path)?.permissions().mode();
    anyhow::ensure!(
        mode & 0o111 != 0,
        "JSON-RPC plugin executable is not marked executable: {}",
        path.display()
    );
    Ok(())
}

#[cfg(not(unix))]
fn ensure_executable(_path: &Path) -> Result<()> {
    Ok(())
}
