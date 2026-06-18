use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use husk::{Host, Value};
use uuid::Uuid;

use crate::{
    assets::RuntimeAssetKind,
    config::{Config, PluginPermissions},
    editor::{Action, PluginRequest, ACTION_DISPATCHER},
    log,
    plugin::process::{ProcessManager, ProcessSpawnOptions},
    ui::{PickerItem, PickerOptions},
};

use super::{Decoration, OverlayConfig, PanelConfig, PanelRow};

#[derive(Debug)]
struct PendingTimeout {
    id: String,
    expires_at: Instant,
}

lazy_static::lazy_static! {
    static ref PENDING_TIMEOUTS: Mutex<Vec<PendingTimeout>> = Mutex::new(Vec::new());
}

/// Poll timer callbacks scheduled by Husk plugins.
pub fn poll_timer_callbacks() -> Vec<PluginRequest> {
    let mut requests = Vec::new();
    let now = Instant::now();

    let mut timeouts = PENDING_TIMEOUTS.lock().unwrap();
    let mut index = 0;
    while index < timeouts.len() {
        if timeouts[index].expires_at <= now {
            let timeout = timeouts.remove(index);
            requests.push(PluginRequest::TimeoutCallback {
                timer_id: timeout.id,
            });
        } else {
            index += 1;
        }
    }

    requests
}

struct RedHost {
    process_manager: ProcessManager,
}

impl RedHost {
    fn new(process_permissions: HashMap<String, PluginPermissions>) -> Self {
        Self {
            process_manager: ProcessManager::new(process_permissions),
        }
    }

    fn poll_process_events(&mut self) -> Vec<serde_json::Value> {
        self.process_manager
            .poll_events()
            .into_iter()
            .filter_map(|event| serde_json::to_value(event).ok())
            .collect()
    }
}

impl Host for RedHost {
    fn log(&mut self, message: &str) {
        log!("[PLUGIN:HUSK] {}", message);
    }

    fn execute(&mut self, plugin: &str, action: &str, args: &[Value]) -> anyhow::Result<Value> {
        match action {
            "Print" => {
                let message = args.first().map(value_to_string).unwrap_or_default();
                ACTION_DISPATCHER.send_request(PluginRequest::Action(Action::Print(message)));
            }
            "FilePicker" => {
                ACTION_DISPATCHER.send_request(PluginRequest::Action(Action::FilePicker));
            }
            "ClearSearchHighlight" => {
                ACTION_DISPATCHER.send_request(PluginRequest::Action(Action::ClearSearchHighlight));
            }
            "RefreshDiagnostics" => {
                ACTION_DISPATCHER.send_request(PluginRequest::Action(Action::RefreshDiagnostics));
            }
            "Refresh" => {
                ACTION_DISPATCHER.send_request(PluginRequest::Action(Action::Refresh));
            }
            "ShowDialog" => {
                ACTION_DISPATCHER.send_request(PluginRequest::Action(Action::ShowDialog));
            }
            "CloseDialog" => {
                ACTION_DISPATCHER.send_request(PluginRequest::Action(Action::CloseDialog));
            }
            "GoToDefinition" => {
                ACTION_DISPATCHER.send_request(PluginRequest::Action(Action::GoToDefinition));
            }
            "Hover" => {
                ACTION_DISPATCHER.send_request(PluginRequest::Action(Action::Hover));
            }
            "ViewLogs" => {
                ACTION_DISPATCHER.send_request(PluginRequest::Action(Action::ViewLogs));
            }
            "ListPlugins" => {
                ACTION_DISPATCHER.send_request(PluginRequest::Action(Action::ListPlugins));
            }
            "PreviewTheme" => {
                let theme_name = args.first().map(value_to_string).unwrap_or_default();
                ACTION_DISPATCHER
                    .send_request(PluginRequest::Action(Action::PreviewTheme(theme_name)));
            }
            "SetTheme" => {
                let theme_name = args.first().map(value_to_string).unwrap_or_default();
                ACTION_DISPATCHER.send_request(PluginRequest::Action(Action::SetTheme(theme_name)));
            }
            "GetViewportLayout" => {
                let request_id = args.first().and_then(value_to_i32).unwrap_or(1);
                ACTION_DISPATCHER.send_request(PluginRequest::GetViewportLayout { request_id });
            }
            "InlayHints" => {
                let request_id = args.first().and_then(value_to_i32).unwrap_or(1);
                let range = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value)
                    .transpose()?;
                ACTION_DISPATCHER.send_request(PluginRequest::InlayHints { request_id, range });
            }
            "GetEditorInfo" => {
                let request_id = args.first().and_then(value_to_i32).unwrap_or(1);
                ACTION_DISPATCHER.send_request(PluginRequest::EditorInfo(Some(request_id)));
            }
            "GetConfig" => {
                let request_id = args.first().and_then(value_to_i32).unwrap_or(1);
                let key = args.get(1).and_then(Value::as_str).map(str::to_string);
                ACTION_DISPATCHER.send_request(PluginRequest::GetConfig { request_id, key });
            }
            "GetStorage" => {
                let request_id = args.first().and_then(value_to_i32).unwrap_or(1);
                let key = args
                    .get(1)
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("GetStorage requires a storage key"))?
                    .to_string();
                ACTION_DISPATCHER.send_request(PluginRequest::GetPluginStorage {
                    plugin: plugin.to_string(),
                    key,
                    request_id,
                });
            }
            "SetStorage" => {
                let key = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("SetStorage requires a storage key"))?
                    .to_string();
                let value = args
                    .get(1)
                    .map(value_to_json)
                    .unwrap_or(serde_json::Value::Null);
                ACTION_DISPATCHER.send_request(PluginRequest::SetPluginStorage {
                    plugin: plugin.to_string(),
                    key,
                    value,
                });
            }
            "RestoreEditorState" => {
                let request_id = args.first().and_then(value_to_i32).unwrap_or(1);
                let snapshot = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value)
                    .transpose()?
                    .ok_or_else(|| anyhow::anyhow!("RestoreEditorState requires a snapshot"))?;
                ACTION_DISPATCHER.send_request(PluginRequest::RestoreEditorState {
                    request_id,
                    snapshot,
                });
            }
            "GetWindows" => {
                let request_id = args.first().and_then(value_to_i32).unwrap_or(1);
                ACTION_DISPATCHER.send_request(PluginRequest::GetWindows { request_id });
            }
            "SetDecorations" => {
                let namespace = args
                    .first()
                    .and_then(Value::as_str)
                    .unwrap_or("default")
                    .to_string();
                let decorations = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<Vec<Decoration>>)
                    .transpose()?
                    .unwrap_or_default();
                ACTION_DISPATCHER.send_request(PluginRequest::SetDecorations {
                    namespace,
                    decorations,
                });
            }
            "ClearDecorations" => {
                let namespace = args
                    .first()
                    .and_then(Value::as_str)
                    .map_or_else(|| "default".to_string(), str::to_string);
                ACTION_DISPATCHER.send_request(PluginRequest::ClearDecorations { namespace });
            }
            "DocumentSymbols" => {
                let request_id = args.first().and_then(value_to_i32).unwrap_or(1);
                ACTION_DISPATCHER.send_request(PluginRequest::DocumentSymbols {
                    request_id,
                    buffer_index: None,
                });
            }
            "WorkspaceSymbols" => {
                let request_id = args.first().and_then(value_to_i32).unwrap_or(1);
                let query = args.get(1).map(value_to_query_string).unwrap_or_default();
                ACTION_DISPATCHER
                    .send_request(PluginRequest::WorkspaceSymbols { request_id, query });
            }
            "References" => {
                let request_id = args.first().and_then(value_to_i32).unwrap_or(1);
                ACTION_DISPATCHER.send_request(PluginRequest::References {
                    request_id,
                    include_declaration: true,
                });
            }
            "OpenDynamicPicker" => {
                let title = args.first().and_then(Value::as_str).map(str::to_string);
                let id = args.get(1).and_then(value_to_i32).unwrap_or(1);
                let items = args
                    .get(2)
                    .map(value_to_json)
                    .map(serde_json::from_value::<Vec<PickerItem>>)
                    .transpose()?
                    .unwrap_or_default();
                let options = args
                    .get(3)
                    .map(value_to_json)
                    .map(serde_json::from_value::<PickerOptions>)
                    .transpose()?
                    .unwrap_or_default();
                ACTION_DISPATCHER.send_request(PluginRequest::OpenDynamicPicker {
                    title,
                    id,
                    items,
                    options,
                });
            }
            "UpdatePickerItems" => {
                let id = args.first().and_then(value_to_i32).unwrap_or(1);
                let items = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<Vec<PickerItem>>)
                    .transpose()?
                    .unwrap_or_default();
                ACTION_DISPATCHER.send_request(PluginRequest::UpdatePickerItems { id, items });
            }
            "UpdatePickerQuery" => {
                let id = args.first().and_then(value_to_i32).unwrap_or(1);
                let query = args.get(1).map(value_to_string).unwrap_or_default();
                ACTION_DISPATCHER.send_request(PluginRequest::UpdatePickerQuery { id, query });
            }
            "UpdatePickerStatus" => {
                let id = args.first().and_then(value_to_i32).unwrap_or(1);
                let status = args.get(1).map(value_to_string);
                ACTION_DISPATCHER.send_request(PluginRequest::UpdatePickerStatus { id, status });
            }
            "ClosePicker" => {
                let id = args.first().and_then(value_to_i32).unwrap_or(1);
                ACTION_DISPATCHER.send_request(PluginRequest::ClosePicker { id });
            }
            "OpenLocation" => {
                let location = args
                    .first()
                    .map(value_to_json)
                    .map(serde_json::from_value)
                    .transpose()?
                    .ok_or_else(|| anyhow::anyhow!("OpenLocation requires a location object"))?;
                let target = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value)
                    .transpose()?
                    .unwrap_or_default();
                ACTION_DISPATCHER.send_request(PluginRequest::OpenLocation { location, target });
            }
            "OpenBuffer" => {
                let name = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("OpenBuffer requires a buffer name"))?
                    .to_string();
                ACTION_DISPATCHER.send_request(PluginRequest::Action(Action::OpenBuffer(name)));
            }
            "ListDirectory" => {
                let path = args
                    .first()
                    .and_then(Value::as_str)
                    .unwrap_or(".")
                    .to_string();
                let request_id = args.get(1).and_then(value_to_i32).unwrap_or(1);
                ACTION_DISPATCHER.send_request(PluginRequest::ListDirectory { path, request_id });
            }
            "GetGitStatus" => {
                let path = args
                    .first()
                    .and_then(Value::as_str)
                    .unwrap_or(".")
                    .to_string();
                let request_id = args.get(1).and_then(value_to_i32).unwrap_or(1);
                ACTION_DISPATCHER.send_request(PluginRequest::GetGitStatus { path, request_id });
            }
            "WatchDirectory" => {
                let path = args
                    .first()
                    .and_then(Value::as_str)
                    .unwrap_or(".")
                    .to_string();
                let watch_id = args.get(1).and_then(value_to_i32).unwrap_or(1);
                let recursive = args.get(2).and_then(Value::as_bool).unwrap_or(false);
                let interval_ms = args.get(3).and_then(value_to_u64).unwrap_or(250);
                ACTION_DISPATCHER.send_request(PluginRequest::WatchDirectory {
                    path,
                    watch_id,
                    recursive,
                    interval_ms,
                });
            }
            "UnwatchDirectory" => {
                let watch_id = args.first().and_then(value_to_i32).unwrap_or(1);
                ACTION_DISPATCHER.send_request(PluginRequest::UnwatchDirectory { watch_id });
            }
            "CreateOverlay" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("CreateOverlay requires an overlay id"))?
                    .to_string();
                let config = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<OverlayConfig>)
                    .transpose()?
                    .unwrap_or_default();
                ACTION_DISPATCHER.send_request(PluginRequest::CreateOverlay { id, config });
            }
            "UpdateOverlay" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("UpdateOverlay requires an overlay id"))?
                    .to_string();
                let lines = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value)
                    .transpose()?
                    .unwrap_or_default();
                ACTION_DISPATCHER.send_request(PluginRequest::UpdateOverlay { id, lines });
            }
            "RemoveOverlay" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("RemoveOverlay requires an overlay id"))?
                    .to_string();
                ACTION_DISPATCHER.send_request(PluginRequest::RemoveOverlay { id });
            }
            "CreatePanel" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("CreatePanel requires a panel id"))?
                    .to_string();
                let config = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<PanelConfig>)
                    .transpose()?
                    .unwrap_or_default();
                ACTION_DISPATCHER.send_request(PluginRequest::CreatePanel { id, config });
            }
            "UpdatePanel" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("UpdatePanel requires a panel id"))?
                    .to_string();
                let rows = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<Vec<PanelRow>>)
                    .transpose()?
                    .unwrap_or_default();
                ACTION_DISPATCHER.send_request(PluginRequest::UpdatePanel { id, rows });
            }
            "SelectPanelRow" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("SelectPanelRow requires a panel id"))?
                    .to_string();
                let row_id = args
                    .get(1)
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("SelectPanelRow requires a row id"))?
                    .to_string();
                ACTION_DISPATCHER.send_request(PluginRequest::SelectPanelRow { id, row_id });
            }
            "FocusPanel" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("FocusPanel requires a panel id"))?
                    .to_string();
                ACTION_DISPATCHER.send_request(PluginRequest::FocusPanel { id });
            }
            "FocusEditor" => {
                ACTION_DISPATCHER.send_request(PluginRequest::FocusEditor);
            }
            "ClosePanel" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("ClosePanel requires a panel id"))?
                    .to_string();
                ACTION_DISPATCHER.send_request(PluginRequest::ClosePanel { id });
            }
            "ListRuntimeAssets" => {
                let kind = match args.first().and_then(Value::as_str).unwrap_or("themes") {
                    "plugin" | "plugins" => RuntimeAssetKind::Plugin,
                    "theme" | "themes" => RuntimeAssetKind::Theme,
                    other => anyhow::bail!("unsupported runtime asset kind `{other}`"),
                };
                let request_id = args.get(1).and_then(value_to_i32).unwrap_or(1);
                ACTION_DISPATCHER
                    .send_request(PluginRequest::ListRuntimeAssets { kind, request_id });
            }
            "SpawnProcess" => {
                let options = args
                    .first()
                    .map(value_to_json)
                    .map(serde_json::from_value::<ProcessSpawnOptions>)
                    .transpose()?
                    .ok_or_else(|| anyhow::anyhow!("SpawnProcess requires process options"))?;
                return self
                    .process_manager
                    .spawn(plugin, options)
                    .map(Value::String);
            }
            "KillProcess" => {
                let process_id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("KillProcess requires a process id"))?;
                self.process_manager.kill(plugin, process_id)?;
            }
            "RecordCursorMoved" => {
                let event = first_json(args)?;
                let message = format!(
                    "cursor:{}:{},{}->{},{}:{}",
                    json_str(event, "cause"),
                    json_usize_at(event, &["from", "x"]),
                    json_usize_at(event, &["from", "y"]),
                    json_usize_at(event, &["to", "x"]),
                    json_usize_at(event, &["to", "y"]),
                    json_str(event, "mode")
                );
                ACTION_DISPATCHER.send_request(PluginRequest::Action(Action::Print(message)));
            }
            "RecordModeChanged" => {
                let event = first_json(args)?;
                let message = format!(
                    "mode:{}:{}->{}",
                    json_str(event, "cause"),
                    json_str(event, "from"),
                    json_str(event, "to")
                );
                ACTION_DISPATCHER.send_request(PluginRequest::Action(Action::Print(message)));
            }
            "RecordSearchHighlighted" => {
                let event = first_json(args)?;
                let message = format!(
                    "search:{}:{}:{}",
                    json_str(event, "source"),
                    json_str(event, "term"),
                    json_str(event, "direction")
                );
                ACTION_DISPATCHER.send_request(PluginRequest::Action(Action::Print(message)));
            }
            "RecordSearchCleared" => {
                let event = first_json(args)?;
                let message = format!("cleared:{}", json_str(event, "term"));
                ACTION_DISPATCHER.send_request(PluginRequest::Action(Action::Print(message)));
            }
            "SetTimeout" => {
                let delay_ms = args.first().and_then(value_to_u64).unwrap_or(0);
                let id = schedule_timeout(delay_ms);
                return Ok(Value::String(id));
            }
            other => {
                anyhow::bail!("unsupported Red host action `{other}`");
            }
        }

        Ok(Value::Unit)
    }
}

fn first_json(args: &[Value]) -> anyhow::Result<&serde_json::Value> {
    match args.first() {
        Some(Value::Json(value)) => Ok(value),
        _ => anyhow::bail!("host action expected a JSON event payload"),
    }
}

fn json_str<'a>(value: &'a serde_json::Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

fn json_usize_at(value: &serde_json::Value, path: &[&str]) -> usize {
    let mut cursor = value;
    for key in path {
        let Some(next) = cursor.get(key) else {
            return 0;
        };
        cursor = next;
    }
    cursor.as_u64().map_or(0, |value| value as usize)
}

fn schedule_timeout(delay_ms: u64) -> String {
    let id = Uuid::new_v4().to_string();
    PENDING_TIMEOUTS.lock().unwrap().push(PendingTimeout {
        id: id.clone(),
        expires_at: Instant::now() + Duration::from_millis(delay_ms),
    });
    id
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::Unit => String::new(),
        Value::Bool(value) => value.to_string(),
        Value::Int(value) => value.to_string(),
        Value::Float(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Json(value) => value.to_string(),
        Value::Callback(_) => "<callback>".to_string(),
    }
}

fn value_to_query_string(value: &Value) -> String {
    match value {
        Value::Json(value) => value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_string),
        value => value_to_string(value),
    }
}

fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Unit => serde_json::Value::Null,
        Value::Bool(value) => serde_json::Value::Bool(*value),
        Value::Int(value) => serde_json::Value::Number((*value).into()),
        Value::Float(value) => serde_json::Number::from_f64(*value)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::String(value) => serde_json::Value::String(value.clone()),
        Value::Json(value) => value.clone(),
        Value::Callback(_) => serde_json::Value::Null,
    }
}

fn value_to_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Int(value) => u64::try_from(*value).ok(),
        Value::Float(value) if *value >= 0.0 => Some(*value as u64),
        Value::String(value) => value.parse().ok(),
        _ => None,
    }
}

fn value_to_i32(value: &Value) -> Option<i32> {
    match value {
        Value::Int(value) => i32::try_from(*value).ok(),
        Value::Float(value) if *value >= 0.0 && *value <= f64::from(i32::MAX) => {
            Some(*value as i32)
        }
        Value::String(value) => value.parse().ok(),
        _ => None,
    }
}

#[derive(Clone)]
pub struct Runtime {
    inner: Arc<Mutex<RuntimeInner>>,
}

struct RuntimeInner {
    vm: husk::Vm,
    host: RedHost,
    anonymous_module_count: usize,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Runtime {
    pub fn new() -> Self {
        Self::try_new().expect("failed to initialize plugin runtime")
    }

    pub fn try_new() -> anyhow::Result<Self> {
        Self::try_new_with_permissions(HashMap::new())
    }

    pub fn new_with_permissions(process_permissions: HashMap<String, PluginPermissions>) -> Self {
        Self::try_new_with_permissions(process_permissions)
            .expect("failed to initialize plugin runtime")
    }

    pub fn try_new_with_permissions(
        process_permissions: HashMap<String, PluginPermissions>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            inner: Arc::new(Mutex::new(RuntimeInner {
                vm: husk::Vm::new(),
                host: RedHost::new(process_permissions),
                anonymous_module_count: 0,
            })),
        })
    }

    pub async fn load_plugin(&mut self, name: &str, source: &str) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let RuntimeInner { vm, host, .. } = &mut *inner;
        vm.load_plugin(name, source, host)
    }

    pub async fn add_module(&mut self, code: &str) -> anyhow::Result<()> {
        let name = {
            let mut inner = self.inner.lock().unwrap();
            inner.anonymous_module_count += 1;
            format!("module-{}", inner.anonymous_module_count)
        };
        self.load_plugin(&name, code).await
    }

    pub async fn run(&mut self, code: &str) -> anyhow::Result<()> {
        self.add_module(code).await
    }

    pub async fn execute_command(&mut self, command: &str) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let RuntimeInner { vm, host, .. } = &mut *inner;
        vm.execute_command(command, host)
    }

    pub async fn notify(&mut self, event: &str, args: serde_json::Value) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let RuntimeInner { vm, host, .. } = &mut *inner;
        vm.notify(event, args, host)
    }

    pub fn poll_process_events(&mut self) -> Vec<serde_json::Value> {
        let mut inner = self.inner.lock().unwrap();
        inner.host.poll_process_events()
    }

    pub async fn before_exit(&mut self, snapshot: serde_json::Value) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let RuntimeInner { vm, host, .. } = &mut *inner;
        vm.before_exit(snapshot, host)
    }

    pub async fn deactivate_all(&mut self) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let RuntimeInner { vm, host, .. } = &mut *inner;
        vm.deactivate_all(host)
    }
}

#[allow(dead_code)]
fn _keep_config_used(_: &Config) {}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::{
        editor::{PluginRequest, PLUGIN_DISPATCHER_TEST_LOCK},
        ui::PickerPresentation,
    };

    fn drain_requests() {
        while ACTION_DISPATCHER.try_recv_request().is_some() {}
    }

    fn sample_indent_layout() -> serde_json::Value {
        serde_json::json!({
            "bufferIndex": 3,
            "cursor": { "y": 2 },
            "indentation": {
                "shiftWidth": 4,
                "tabWidth": 4,
            },
            "rows": [
                { "line": 0, "text": "fn main() {", "firstSegment": true },
                { "line": 1, "text": "    if ok {", "firstSegment": true },
                { "line": 2, "text": "        call();", "firstSegment": true },
                { "line": 3, "text": "    }", "firstSegment": true },
                { "line": 4, "text": "}", "firstSegment": true }
            ]
        })
    }

    fn sample_symbol_payload() -> serde_json::Value {
        serde_json::json!({
            "ok": true,
            "symbols": [{
                "name": "main",
                "detail": "fn()",
                "kind": 12,
                "kindName": "Function",
                "file": "src/main.rs",
                "range": {
                    "start": { "line": 4, "character": 0 },
                    "end": { "line": 6, "character": 1 }
                },
                "selectionRange": {
                    "start": { "line": 4, "character": 3 },
                    "end": { "line": 4, "character": 7 }
                },
                "depth": 0
            }]
        })
    }

    async fn pump_process_events(runtime: &mut Runtime) -> anyhow::Result<()> {
        for event in runtime.poll_process_events() {
            let Some(process_id) = event
                .get("processId")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
            else {
                continue;
            };
            runtime
                .notify(&format!("process:{process_id}"), event)
                .await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn executes_husk_command_through_host() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let source = r#"
            pub fn activate() {
                red::add_command("Hello", hello);
            }

            fn hello() {
                red::execute("Print", "hello from husk");
            }
        "#;
        let mut runtime = Runtime::new();

        runtime.load_plugin("test", source).await.unwrap();
        runtime.execute_command("Hello").await.unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::Action(Action::Print(message)) => {
                assert_eq!(message, "hello from husk");
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn buffer_picker_lists_and_opens_existing_buffers() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "buffer_picker",
                include_str!("../../plugins/buffer_picker.hk"),
            )
            .await
            .unwrap();

        runtime.execute_command("BufferPicker").await.unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::EditorInfo(Some(request_id)) => assert_eq!(request_id, 701),
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .notify(
                "editor:info:701",
                serde_json::json!({
                    "buffers": [
                        { "name": "src/main.rs", "path": "src/main.rs", "dirty": false },
                        { "name": "[No Name]", "path": null, "dirty": true },
                    ],
                }),
            )
            .await
            .unwrap();

        let items = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenDynamicPicker {
                title, id, items, ..
            } => {
                assert_eq!(title.as_deref(), Some("Buffers"));
                assert_eq!(id, 701);
                assert_eq!(items[0].label, "src/main.rs");
                assert_eq!(items[1].label, "[No Name]");
                items
            }
            _ => panic!("unexpected plugin request"),
        };

        runtime
            .notify(
                "picker:selected:701",
                serde_json::to_value(&items[1]).unwrap(),
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::Action(Action::OpenBuffer(name)) => assert_eq!(name, "[No Name]"),
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn cool_search_clears_search_highlight_on_non_search_movement() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("cool_search", include_str!("../../plugins/cool_search.hk"))
            .await
            .unwrap();

        runtime
            .notify("search:highlighted", serde_json::json!({}))
            .await
            .unwrap();
        runtime
            .notify(
                "cursor:moved",
                serde_json::json!({
                    "mode": "Normal",
                    "cause": "FindNext",
                }),
            )
            .await
            .unwrap();

        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime
            .notify(
                "cursor:moved",
                serde_json::json!({
                    "mode": "Normal",
                    "cause": "MoveRight",
                }),
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::Action(Action::ClearSearchHighlight) => {}
            _ => panic!("unexpected plugin request"),
        }

        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn cool_search_clears_search_highlight_on_insert_mode() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("cool_search", include_str!("../../plugins/cool_search.hk"))
            .await
            .unwrap();

        runtime
            .notify("search:highlighted", serde_json::json!({}))
            .await
            .unwrap();
        runtime
            .notify(
                "mode:changed",
                serde_json::json!({
                    "from": "Normal",
                    "to": "Insert",
                }),
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::Action(Action::ClearSearchHighlight) => {}
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .notify(
                "cursor:moved",
                serde_json::json!({
                    "mode": "Normal",
                    "cause": "MoveRight",
                }),
            )
            .await
            .unwrap();

        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn indent_guides_requests_viewport_layout_on_activation_and_refresh_events() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "indent_guides",
                include_str!("../../plugins/indent_guides.hk"),
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetViewportLayout { request_id } => {
                assert_eq!(request_id, 1);
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .notify("buffer:changed", serde_json::json!({}))
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetViewportLayout { request_id } => {
                assert_eq!(request_id, 1);
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn indent_guides_renders_decorations_from_viewport_layout_response() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "indent_guides",
                include_str!("../../plugins/indent_guides.hk"),
            )
            .await
            .unwrap();
        let _ = ACTION_DISPATCHER.recv_request();

        runtime
            .notify("viewport:layout:1", sample_indent_layout())
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SetDecorations {
                namespace,
                decorations,
            } => {
                assert_eq!(namespace, "indent-guides");
                assert_eq!(decorations[0].buffer_index, Some(3));
                assert_eq!(decorations[0].line, 1);
                assert_eq!(decorations[0].text, "\u{2502}   ");
                assert!(decorations
                    .iter()
                    .any(|decoration| decoration.line == 2 && decoration.priority == 1024));
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn indent_guides_clears_decorations_on_deactivate() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "indent_guides",
                include_str!("../../plugins/indent_guides.hk"),
            )
            .await
            .unwrap();
        let _ = ACTION_DISPATCHER.recv_request();

        runtime.deactivate_all().await.unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ClearDecorations { namespace } => {
                assert_eq!(namespace, "indent-guides");
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn inlay_hints_requests_visible_range_and_sets_eol_decorations() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("inlay_hints", include_str!("../../plugins/inlay_hints.hk"))
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetViewportLayout { request_id } => assert_eq!(request_id, 901),
            _ => panic!("unexpected plugin request"),
        }
        runtime
            .notify("viewport:layout:901", sample_indent_layout())
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::EditorInfo(Some(request_id)) => assert_eq!(request_id, 902),
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::InlayHints { request_id, range } => {
                assert_eq!(request_id, 903);
                let range = range.unwrap();
                assert_eq!(range.start.line, 0);
                assert_eq!(range.end.line, 5);
            }
            _ => panic!("unexpected plugin request"),
        }
        runtime
            .notify(
                "editor:info:902",
                serde_json::json!({
                    "theme": {
                        "colors": {
                            "editorInlayHint.typeForeground": "#c8c8c8",
                            "editor.background": "#0a141e",
                        },
                        "gutterStyle": { "fg": null },
                    }
                }),
            )
            .await
            .unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        runtime
            .notify(
                "lsp:inlay_hints:903",
                serde_json::json!({
                    "ok": true,
                    "hints": [{
                        "kind": 1,
                        "position": { "line": 1, "character": 8 },
                        "label": [{ "value": ": String" }],
                    }],
                }),
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SetDecorations {
                namespace,
                decorations,
            } => {
                assert_eq!(namespace, "inlay-hints");
                assert_eq!(decorations.len(), 1);
                assert_eq!(decorations[0].line, 1);
                assert_eq!(decorations[0].anchor, crate::plugin::DecorationAnchor::Eol);
                assert_eq!(decorations[0].text, " => String");
                assert_eq!(decorations[0].priority, 1001);
                assert_eq!(
                    decorations[0].style.fg,
                    Some(crate::color::Color::Rgb {
                        r: 90,
                        g: 96,
                        b: 101,
                    })
                );
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn fidget_renders_lsp_progress_in_overlay() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("fidget", include_str!("../../plugins/fidget.hk"))
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::EditorInfo(Some(request_id)) => assert_eq!(request_id, 911),
            _ => panic!("unexpected plugin request"),
        }
        runtime
            .notify(
                "editor:info:911",
                serde_json::json!({ "size": [80, 24], "theme": { "uiStyle": {} } }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::CreateOverlay { id, .. } => assert_eq!(id, "fidget-progress"),
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdateOverlay { id, lines } => {
                assert_eq!(id, "fidget-progress");
                assert!(lines.is_empty());
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .notify(
                "lsp:progress",
                serde_json::json!({
                    "token": "index",
                    "value": {
                        "kind": "begin",
                        "title": "Indexing",
                        "message": "Loading",
                        "percentage": 25,
                    },
                    "lspClient": { "name": "rust_analyzer" },
                }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdateOverlay { id, lines } => {
                assert_eq!(id, "fidget-progress");
                assert_eq!(lines.len(), 2);
                assert_eq!(lines[0].0, "Loading (25%) Indexing");
                assert_eq!(lines[1].0, "rust-analyzer ⠋");
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn project_search_streams_rg_matches_into_picker() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new_with_permissions(HashMap::from([(
            "project_search".to_string(),
            PluginPermissions {
                process: vec!["rg".to_string()],
            },
        )]));
        runtime
            .load_plugin(
                "project_search",
                include_str!("../../plugins/project_search.hk"),
            )
            .await
            .unwrap();

        runtime.execute_command("ProjectSearch").await.unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(request_id, 302);
                assert_eq!(key.as_deref(), Some("cwd"));
            }
            _ => panic!("unexpected plugin request"),
        }
        runtime
            .notify("config:value:302", serde_json::json!({ "value": "." }))
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage {
                plugin,
                key,
                request_id,
            } => {
                assert_eq!(plugin, "project_search");
                assert_eq!(key, "history:.");
                assert_eq!(request_id, 303);
            }
            _ => panic!("unexpected plugin request"),
        }
        runtime
            .notify("storage:value:303", serde_json::json!({ "value": [] }))
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenDynamicPicker {
                title, id, options, ..
            } => {
                assert_eq!(title.as_deref(), Some("Find in Files"));
                assert_eq!(id, 301);
                assert!(options.external_filter);
                assert!(options
                    .actions
                    .iter()
                    .any(|action| action.action == "export"));
            }
            _ => panic!("unexpected plugin request"),
        }

        let query = ["project_search_", "process"].concat();
        runtime
            .notify("picker:query:301", serde_json::json!(query))
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(120)).await;
        for callback in poll_timer_callbacks() {
            if let PluginRequest::TimeoutCallback { timer_id } = callback {
                runtime
                    .notify(
                        "timeout:callback",
                        serde_json::json!({ "timerId": timer_id }),
                    )
                    .await
                    .unwrap();
            }
        }

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePickerItems { id, items } => {
                assert_eq!(id, 301);
                assert!(items.is_empty());
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePickerStatus { id, status } => {
                assert_eq!(id, 301);
                assert!(status
                    .as_deref()
                    .is_some_and(|status| status.starts_with("Searching (0/500)")));
            }
            _ => panic!("unexpected plugin request"),
        }

        let deadline = Instant::now() + Duration::from_secs(5);
        let item = loop {
            pump_process_events(&mut runtime).await.unwrap();
            for callback in poll_timer_callbacks() {
                if let PluginRequest::TimeoutCallback { timer_id } = callback {
                    runtime
                        .notify(
                            "timeout:callback",
                            serde_json::json!({ "timerId": timer_id }),
                        )
                        .await
                        .unwrap();
                }
            }
            let mut found = None;
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                if let PluginRequest::UpdatePickerItems { id, items } = request {
                    assert_eq!(id, 301);
                    if let Some(item) = items.first() {
                        found = Some(item.clone());
                        break;
                    }
                }
            }
            if let Some(item) = found {
                break item;
            }
            assert!(
                Instant::now() < deadline,
                "project search did not produce a picker item"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };

        assert!(
            item.label.ends_with("plugins/project_search.hk")
                || item.label == "plugins/project_search.hk"
        );
        assert_eq!(item.kind.as_deref(), Some("Match"));
        assert!(item
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains(&["project_search_", "process"].concat())));

        drain_requests();
        runtime
            .notify("picker:selected:301", serde_json::to_value(item).unwrap())
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SetPluginStorage {
                plugin, key, value, ..
            } => {
                assert_eq!(plugin, "project_search");
                assert_eq!(key, "history:.");
                assert_eq!(value, serde_json::json!([query]));
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ClosePicker { id } => assert_eq!(id, 301),
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenLocation { location, target } => {
                assert_eq!(location.path, "plugins/project_search.hk");
                assert_eq!(target, crate::plugin::OpenLocationTarget::Current);
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn session_restore_loads_matching_snapshot_and_saves_only_clean_buffers() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let snapshot = serde_json::json!({
            "version": 1,
            "cwd": "/tmp/project",
            "savedAt": 1,
            "buffers": [
                {
                    "index": 0,
                    "path": "src/main.rs",
                    "dirty": false,
                    "cursor": { "x": 0, "y": 0 },
                    "viewportTop": 0,
                },
                {
                    "index": 1,
                    "path": "scratch.rs",
                    "dirty": true,
                    "cursor": { "x": 0, "y": 0 },
                    "viewportTop": 0,
                }
            ],
            "currentBufferIndex": 0,
            "windowLayout": {
                "active_window_id": 0,
                "root": {
                    "kind": "window",
                    "buffer_index": 0,
                    "vtop": 0,
                    "vleft": 0,
                    "cx": 0,
                    "cy": 0,
                    "vx": 0,
                }
            }
        });
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "session_restore",
                include_str!("../../plugins/session_restore.hk"),
            )
            .await
            .unwrap();

        runtime
            .notify("editor:ready", serde_json::json!({}))
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(request_id, 801);
                assert_eq!(key.as_deref(), Some("startup_file_count"));
            }
            _ => panic!("unexpected plugin request"),
        }
        runtime
            .notify("config:value:801", serde_json::json!({ "value": 0 }))
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage {
                plugin,
                key,
                request_id,
            } => {
                assert_eq!(plugin, "session_restore");
                assert_eq!(key, "latest");
                assert_eq!(request_id, 802);
            }
            _ => panic!("unexpected plugin request"),
        }
        runtime
            .notify(
                "storage:value:802",
                serde_json::json!({ "value": snapshot.clone() }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(request_id, 803);
                assert_eq!(key.as_deref(), Some("cwd"));
            }
            _ => panic!("unexpected plugin request"),
        }
        runtime
            .notify(
                "config:value:803",
                serde_json::json!({ "value": "/tmp/project" }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::RestoreEditorState {
                request_id,
                snapshot,
            } => {
                assert_eq!(request_id, 804);
                assert_eq!(snapshot.buffers.len(), 2);
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime.before_exit(snapshot).await.unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SetPluginStorage {
                plugin, key, value, ..
            } => {
                assert_eq!(plugin, "session_restore");
                assert_eq!(key, "latest");
                assert_eq!(value["buffers"].as_array().unwrap().len(), 1);
                assert_eq!(value["buffers"][0]["path"], "src/main.rs");
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn neotree_renders_a_panel_expands_directories_and_opens_files() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("neotree", include_str!("../../plugins/neotree.hk"))
            .await
            .unwrap();

        runtime.execute_command("NeoTree").await.unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::CreatePanel { id, config } => {
                assert_eq!(id, "neotree");
                assert_eq!(config.side, crate::plugin::PanelSide::Left);
                assert_eq!(config.width, 30);
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePanel { id, rows } => {
                assert_eq!(id, "neotree");
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].id, "loading");
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::FocusPanel { id } => assert_eq!(id, "neotree"),
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(request_id, 503);
                assert_eq!(key.as_deref(), Some("cwd"));
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetWindows { request_id } => assert_eq!(request_id, 504),
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ListDirectory { path, request_id } => {
                assert_eq!(path, ".");
                assert_eq!(request_id, 501);
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetGitStatus { path, request_id } => {
                assert_eq!(path, ".");
                assert_eq!(request_id, 502);
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .notify(
                "filesystem:directory:501",
                serde_json::json!({
                    "path": ".",
                    "entries": [
                        { "name": "src", "path": "./src", "kind": "directory" },
                        { "name": "Cargo.toml", "path": "./Cargo.toml", "kind": "file" }
                    ],
                    "error": null
                }),
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::WatchDirectory {
                path,
                watch_id,
                recursive,
                ..
            } => {
                assert_eq!(path, ".");
                assert_eq!(watch_id, 700);
                assert!(!recursive);
            }
            _ => panic!("unexpected plugin request"),
        }
        let root_rows = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePanel { id, rows } => {
                assert_eq!(id, "neotree");
                assert_eq!(rows.len(), 3);
                assert_eq!(rows[0].id, ".");
                assert_eq!(rows[1].id, "./src");
                assert_eq!(rows[2].id, "./Cargo.toml");
                rows
            }
            _ => panic!("unexpected plugin request"),
        };

        let directory_row = serde_json::to_value(&root_rows[1]).unwrap();
        runtime
            .notify(
                "panel:event:neotree",
                serde_json::json!({
                    "action": "activate",
                    "row": directory_row,
                }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ListDirectory { path, request_id } => {
                assert_eq!(path, "./src");
                assert_eq!(request_id, 501);
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePanel { id, rows } => {
                assert_eq!(id, "neotree");
                assert_eq!(rows.len(), 3);
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .notify(
                "filesystem:directory:501",
                serde_json::json!({
                    "path": "./src",
                    "entries": [
                        { "name": "main.rs", "path": "./src/main.rs", "kind": "file" }
                    ],
                    "error": null
                }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::WatchDirectory {
                path,
                watch_id,
                recursive,
                ..
            } => {
                assert_eq!(path, "./src");
                assert_eq!(watch_id, 701);
                assert!(!recursive);
            }
            _ => panic!("unexpected plugin request"),
        }
        let expanded_rows = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePanel { id, rows } => {
                assert_eq!(id, "neotree");
                assert_eq!(rows.len(), 4);
                assert_eq!(rows[2].id, "./src/main.rs");
                rows
            }
            _ => panic!("unexpected plugin request"),
        };

        let file_row = serde_json::to_value(&expanded_rows[2]).unwrap();
        runtime
            .notify(
                "panel:event:neotree",
                serde_json::json!({
                    "action": "activate",
                    "row": file_row,
                }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenLocation { location, target } => {
                assert_eq!(location.path, "./src/main.rs");
                assert_eq!(target, crate::plugin::OpenLocationTarget::Current);
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UnwatchDirectory { watch_id } => assert_eq!(watch_id, 700),
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UnwatchDirectory { watch_id } => assert_eq!(watch_id, 701),
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ClosePanel { id } => assert_eq!(id, "neotree"),
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::FocusEditor => {}
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn neotree_reveals_the_active_file_and_renders_git_status() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("neotree", include_str!("../../plugins/neotree.hk"))
            .await
            .unwrap();

        runtime.execute_command("NeoTree").await.unwrap();
        drain_requests();

        runtime
            .notify("config:value:503", serde_json::json!({ "value": "/repo" }))
            .await
            .unwrap();
        drain_requests();

        runtime
            .notify(
                "windows:504",
                serde_json::json!({
                    "windows": [{
                        "active": true,
                        "bufferPath": "/repo/src/main.rs",
                    }],
                }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ListDirectory { path, request_id } => {
                assert_eq!(path, ".");
                assert_eq!(request_id, 501);
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .notify(
                "filesystem:directory:501",
                serde_json::json!({
                    "path": ".",
                    "entries": [
                        { "name": "src", "path": "./src", "kind": "directory" },
                    ],
                    "error": null,
                }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::WatchDirectory { path, watch_id, .. } => {
                assert_eq!(path, ".");
                assert_eq!(watch_id, 700);
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ListDirectory { path, request_id } => {
                assert_eq!(path, "./src");
                assert_eq!(request_id, 501);
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePanel { id, rows } => {
                assert_eq!(id, "neotree");
                assert_eq!(rows.len(), 2);
                assert!(rows[1].expanded.unwrap_or(false));
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .notify(
                "filesystem:directory:501",
                serde_json::json!({
                    "path": "./src",
                    "entries": [
                        { "name": "main.rs", "path": "./src/main.rs", "kind": "file" },
                    ],
                    "error": null,
                }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::WatchDirectory { path, watch_id, .. } => {
                assert_eq!(path, "./src");
                assert_eq!(watch_id, 701);
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePanel { id, rows } => {
                assert_eq!(id, "neotree");
                assert_eq!(rows[2].id, "./src/main.rs");
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SelectPanelRow { id, row_id } => {
                assert_eq!(id, "neotree");
                assert_eq!(row_id, "./src/main.rs");
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .notify(
                "git:status:502",
                serde_json::json!({
                    "root": "/repo",
                    "statuses": [{
                        "path": "src/main.rs",
                        "absolutePath": "/repo/src/main.rs",
                        "status": "modified",
                    }],
                    "error": null,
                }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePanel { id, rows } => {
                assert_eq!(id, "neotree");
                assert_eq!(rows[2].right_segments[0].text, "");
                assert!(rows[2].right_segments[0].semantic.is_some());
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn theme_browser_previews_restores_and_sets_selected_theme() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "theme_browser",
                include_str!("../../plugins/theme_browser.hk"),
            )
            .await
            .unwrap();

        runtime.execute_command("ThemeBrowser").await.unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(request_id, 602);
                assert_eq!(key.as_deref(), Some("theme"));
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ListRuntimeAssets { kind, request_id } => {
                assert_eq!(kind, RuntimeAssetKind::Theme);
                assert_eq!(request_id, 601);
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .notify(
                "config:value:602",
                serde_json::json!({ "value": "custom.json" }),
            )
            .await
            .unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime
            .notify(
                "runtime_assets:themes:601",
                serde_json::json!({
                    "kind": "themes",
                    "entries": [
                        {
                            "file": "mocha.json",
                            "name": "Mocha",
                            "source": "embedded",
                            "shadows": [],
                        },
                        {
                            "file": "custom.json",
                            "name": "Custom",
                            "source": "user",
                            "shadows": ["embedded"],
                        },
                        {
                            "file": "custom-dark.json",
                            "name": "Custom",
                            "source": "embedded",
                            "shadows": [],
                        }
                    ],
                    "error": null,
                }),
            )
            .await
            .unwrap();

        let items = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenDynamicPicker {
                title,
                id,
                items,
                options,
            } => {
                assert_eq!(title.as_deref(), Some("Themes"));
                assert_eq!(id, 601);
                assert_eq!(options.initial_selection.as_deref(), Some("custom.json"));
                assert_eq!(options.presentation, PickerPresentation::Compact);
                assert_eq!(items[0].label, "Mocha");
                assert_eq!(items[0].kind.as_deref(), Some("Theme"));
                assert_eq!(items[1].label, "Custom (custom.json)");
                assert_eq!(items[2].label, "Custom (custom-dark.json)");
                assert_eq!(items[1].annotation.as_deref(), Some("custom.json"));
                items
            }
            _ => panic!("unexpected plugin request"),
        };

        runtime
            .notify(
                "picker:changed:601",
                serde_json::to_value(&items[0]).unwrap(),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::Action(Action::PreviewTheme(theme)) => {
                assert_eq!(theme, "mocha.json");
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .notify("picker:cancelled:601", serde_json::Value::Null)
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::Action(Action::PreviewTheme(theme)) => {
                assert_eq!(theme, "custom.json");
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .notify(
                "picker:selected:601",
                serde_json::to_value(&items[1]).unwrap(),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::Action(Action::SetTheme(theme)) => {
                assert_eq!(theme, "custom.json");
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn lsp_symbols_requests_document_symbols_and_opens_picker() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("lsp_symbols", include_str!("../../plugins/lsp_symbols.hk"))
            .await
            .unwrap();

        runtime.execute_command("LspDocumentSymbols").await.unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::DocumentSymbols {
                request_id,
                buffer_index,
            } => {
                assert_eq!(request_id, 201);
                assert_eq!(buffer_index, None);
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .notify("lsp:document_symbols:201", sample_symbol_payload())
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenDynamicPicker {
                title, id, items, ..
            } => {
                assert_eq!(title.as_deref(), Some("Document Symbols"));
                assert_eq!(id, 201);
                assert_eq!(items[0].label, "Function main");
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn lsp_symbols_workspace_query_updates_picker() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("lsp_symbols", include_str!("../../plugins/lsp_symbols.hk"))
            .await
            .unwrap();

        runtime
            .execute_command("LspWorkspaceSymbols")
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenDynamicPicker { title, id, .. } => {
                assert_eq!(title.as_deref(), Some("Workspace Symbols"));
                assert_eq!(id, 202);
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::WorkspaceSymbols { request_id, query } => {
                assert_eq!(request_id, 202);
                assert_eq!(query, "");
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .notify("picker:query:202", serde_json::json!("main"))
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::WorkspaceSymbols { request_id, query } => {
                assert_eq!(request_id, 202);
                assert_eq!(query, "main");
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .notify("lsp:workspace_symbols:202", sample_symbol_payload())
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePickerItems { id, items } => {
                assert_eq!(id, 202);
                assert_eq!(items[0].label, "Function main");
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePickerStatus { id, status } => {
                assert_eq!(id, 202);
                assert_eq!(status.as_deref(), Some("1 symbols"));
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn lsp_symbols_picker_selection_opens_symbol_location() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("lsp_symbols", include_str!("../../plugins/lsp_symbols.hk"))
            .await
            .unwrap();
        runtime
            .notify("lsp:document_symbols:201", sample_symbol_payload())
            .await
            .unwrap();
        let item = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenDynamicPicker { items, .. } => {
                serde_json::to_value(&items[0]).unwrap()
            }
            _ => panic!("unexpected plugin request"),
        };

        runtime.notify("picker:selected:201", item).await.unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenLocation { location, target } => {
                assert_eq!(location.path, "src/main.rs");
                assert_eq!(location.line, 4);
                assert_eq!(location.column, 3);
                assert_eq!(
                    location.column_encoding,
                    crate::plugin::LocationColumnEncoding::Utf16
                );
                assert_eq!(target, crate::plugin::OpenLocationTarget::Current);
            }
            _ => panic!("unexpected plugin request"),
        }
    }
}
