use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use husk::{CommandMetadata, Host, RequestId, Value};
use husk_diagnostics::{Diagnostic as HuskDiagnostic, Report as HuskReport, SourceFile};
use uuid::Uuid;

use crate::{
    assets::RuntimeAssetKind,
    config::{Config, PluginPermissions},
    editor::{Action, PluginRequest, ACTION_DISPATCHER},
    log,
    plugin::process::{ProcessManager, ProcessSpawnOptions},
    ui::{PickerItem, PickerOptions},
};

use super::{
    Decoration, GutterSign, OverlayConfig, PanelConfig, PanelRow, TextPanelBlock, TextPanelStatus,
    WindowBarConfig, WindowBarSegment,
};
use super::{WorkspaceConfig, WorkspaceModel};

#[derive(Debug)]
struct PendingTimeout {
    id: String,
    expires_at: Instant,
}

lazy_static::lazy_static! {
    static ref PENDING_TIMEOUTS: Mutex<Vec<PendingTimeout>> = Mutex::new(Vec::new());
}

const PLUGIN_INSTRUCTION_BUDGET: usize = 100_000;

const RED_HOST_DECLARATIONS: &str = r#"
type Json = JsValue;
extern "red" {
    mod global red {
        fn add_command();
        fn on();
        fn execute() -> JsValue;
        fn request() -> JsValue;
        fn viewport_layout() -> JsValue;
        fn windows() -> JsValue;
        fn editor_info() -> JsValue;
        fn log();
        fn state_bool() -> bool;
        fn state_set();
        fn state() -> JsValue;
        fn push() -> JsValue;
        fn unshift() -> JsValue;
        fn contains() -> bool;
        fn remove() -> JsValue;
        fn reverse() -> JsValue;
        fn join() -> String;
        fn range() -> [i32];
        fn len() -> i32;
        fn int() -> i32;
        fn bool() -> bool;
        fn string() -> String;
        fn text_field() -> String;
        fn utf8_byte_to_char_index() -> i32;
        fn blend_color() -> String;
        fn is_light_color() -> bool;
        fn char_at() -> String;
        fn trim() -> String;
        fn lower() -> String;
        fn split() -> [String];
        fn starts_with() -> bool;
        fn ends_with() -> bool;
        fn replace_all() -> String;
        fn trim_line_end() -> String;
        fn slice() -> String;
        fn is_whitespace() -> bool;
        fn char() -> String;
        fn null() -> JsValue;
        fn parse_json() -> JsValue;
    }
}
"#;

static RED_HOST_AST: OnceLock<husk_ast::File> = OnceLock::new();

/// Poll timer callbacks scheduled by Husk plugins.
pub fn poll_timer_callbacks() -> Vec<PluginRequest> {
    let mut requests = Vec::new();
    let now = Instant::now();

    let mut timeouts = PENDING_TIMEOUTS.lock().unwrap();
    timeouts.retain(|timeout| {
        if timeout.expires_at <= now {
            requests.push(PluginRequest::TimeoutCallback {
                timer_id: timeout.id.clone(),
            });
            false
        } else {
            true
        }
    });

    requests
}

struct RedHost {
    process_manager: ProcessManager,
    snapshots: HashMap<String, Value>,
    staged_effects: Option<Vec<StagedHostEffect>>,
    staged_replacement_start: Option<usize>,
    staged_teardown_start: Option<usize>,
}

enum StagedHostEffect {
    Request(Box<PluginRequest>),
    Log(String),
    ScheduleTimeout { id: String, delay_ms: u64 },
    CancelTimeout(String),
}

impl RedHost {
    fn new(process_permissions: HashMap<String, PluginPermissions>) -> Self {
        Self {
            process_manager: ProcessManager::new(process_permissions),
            snapshots: HashMap::new(),
            staged_effects: None,
            staged_replacement_start: None,
            staged_teardown_start: None,
        }
    }

    fn set_snapshot(&mut self, name: impl Into<String>, value: serde_json::Value) {
        self.snapshots.insert(name.into(), Value::from_json(value));
    }

    fn poll_process_events(&mut self) -> Vec<serde_json::Value> {
        self.process_manager
            .poll_events()
            .into_iter()
            .filter_map(|event| serde_json::to_value(event).ok())
            .collect()
    }

    fn begin_reload(&mut self) {
        self.staged_effects = Some(Vec::new());
        self.staged_replacement_start = None;
        self.staged_teardown_start = None;
    }

    fn commit_reload(&mut self) {
        let mut effects = self.staged_effects.take().unwrap_or_default();
        if let (Some(replacement), Some(teardown)) = (
            self.staged_replacement_start.take(),
            self.staged_teardown_start.take(),
        ) {
            if replacement <= teardown && teardown <= effects.len() {
                effects[replacement..].rotate_left(teardown - replacement);
            }
        }
        for effect in effects {
            match effect {
                StagedHostEffect::Request(request) => ACTION_DISPATCHER.send_request(*request),
                StagedHostEffect::Log(message) => log!("[PLUGIN:HUSK] {}", message),
                StagedHostEffect::ScheduleTimeout { id, delay_ms } => {
                    schedule_timeout_with_id(id, delay_ms);
                }
                StagedHostEffect::CancelTimeout(id) => cancel_timeout(&id),
            }
        }
    }

    fn rollback_reload(&mut self) {
        self.staged_effects = None;
        self.staged_replacement_start = None;
        self.staged_teardown_start = None;
    }

    fn send_request(&mut self, request: PluginRequest) {
        if let Some(effects) = &mut self.staged_effects {
            effects.push(StagedHostEffect::Request(Box::new(request)));
        } else {
            ACTION_DISPATCHER.send_request(request);
        }
    }

    fn schedule_timeout(&mut self, delay_ms: u64) -> String {
        let id = Uuid::new_v4().to_string();
        if let Some(effects) = &mut self.staged_effects {
            effects.push(StagedHostEffect::ScheduleTimeout {
                id: id.clone(),
                delay_ms,
            });
        } else {
            schedule_timeout_with_id(id.clone(), delay_ms);
        }
        id
    }

    fn cancel_timeout(&mut self, timer_id: &str) {
        if let Some(effects) = &mut self.staged_effects {
            effects.push(StagedHostEffect::CancelTimeout(timer_id.to_string()));
        } else {
            cancel_timeout(timer_id);
        }
    }
}

impl Host for RedHost {
    fn log(&mut self, message: &str) {
        if let Some(effects) = &mut self.staged_effects {
            effects.push(StagedHostEffect::Log(message.to_string()));
        } else {
            log!("[PLUGIN:HUSK] {}", message);
        }
    }

    fn begin_reload_replacement(&mut self) {
        self.staged_replacement_start = self.staged_effects.as_ref().map(Vec::len);
    }

    fn begin_reload_teardown(&mut self) {
        self.staged_teardown_start = self.staged_effects.as_ref().map(Vec::len);
    }

    fn execute(&mut self, plugin: &str, action: &str, args: &[Value]) -> anyhow::Result<Value> {
        match action {
            "Print" => {
                let message = args.first().map(value_to_string).unwrap_or_default();
                self.send_request(PluginRequest::Action(Action::Print(message)));
            }
            "FilePicker" => {
                self.send_request(PluginRequest::Action(Action::FilePicker));
            }
            "ClearSearchHighlight" => {
                self.send_request(PluginRequest::Action(Action::ClearSearchHighlight));
            }
            "RefreshDiagnostics" => {
                self.send_request(PluginRequest::Action(Action::RefreshDiagnostics));
            }
            "Refresh" => {
                self.send_request(PluginRequest::Action(Action::Refresh));
            }
            "ShowDialog" => {
                self.send_request(PluginRequest::Action(Action::ShowDialog));
            }
            "CloseDialog" => {
                self.send_request(PluginRequest::Action(Action::CloseDialog));
            }
            "GoToDefinition" => {
                self.send_request(PluginRequest::Action(Action::GoToDefinition));
            }
            "Hover" => {
                self.send_request(PluginRequest::Action(Action::Hover));
            }
            "ViewLogs" => {
                self.send_request(PluginRequest::Action(Action::ViewLogs));
            }
            "ListPlugins" => {
                self.send_request(PluginRequest::Action(Action::ListPlugins));
            }
            "PreviewTheme" => {
                let theme_name = args.first().map(value_to_string).unwrap_or_default();
                self.send_request(PluginRequest::Action(Action::PreviewTheme(theme_name)));
            }
            "SetTheme" => {
                let theme_name = args.first().map(value_to_string).unwrap_or_default();
                self.send_request(PluginRequest::Action(Action::SetTheme(theme_name)));
            }
            "AgentNewSession" => {
                let cwd = args
                    .first()
                    .and_then(Value::as_str)
                    .map_or_else(|| PathBuf::from("."), PathBuf::from);
                self.send_request(PluginRequest::AgentNewSession { cwd });
            }
            "AgentPrompt" => {
                let session_id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("AgentPrompt requires a session id"))?
                    .to_string();
                let text = args.get(1).map(value_to_string).unwrap_or_default();
                self.send_request(PluginRequest::AgentPrompt { session_id, text });
            }
            "AgentPromptWithContext" => {
                let session_id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("AgentPromptWithContext requires a session id"))?
                    .to_string();
                let text = args.get(1).map(value_to_string).unwrap_or_default();
                let context = args
                    .get(2)
                    .map(value_to_json)
                    .unwrap_or(serde_json::Value::Null);
                let uri = context
                    .get("uri")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("red-buffer://active")
                    .to_string();
                let context = context
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.send_request(PluginRequest::AgentPromptWithContext {
                    session_id,
                    text,
                    uri,
                    context,
                });
            }
            "AgentCancel" => {
                let session_id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("AgentCancel requires a session id"))?
                    .to_string();
                self.send_request(PluginRequest::AgentCancel { session_id });
            }
            "AgentCloseSession" => {
                let session_id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("AgentCloseSession requires a session id"))?
                    .to_string();
                self.send_request(PluginRequest::AgentCloseSession { session_id });
            }
            "AgentArchiveSession" => {
                let session_id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("AgentArchiveSession requires a session id"))?
                    .to_string();
                self.send_request(PluginRequest::AgentArchiveSession { session_id });
            }
            "AgentAcceptProposal" | "AgentRejectProposal" => {
                let session_id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("agent proposal action requires a session id"))?
                    .to_string();
                let path = args
                    .get(/*index*/ 1)
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("agent proposal action requires a path"))?;
                let hunk_id = args
                    .get(/*index*/ 2)
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                let request = if action == "AgentAcceptProposal" {
                    PluginRequest::AgentAcceptProposal {
                        session_id,
                        path: PathBuf::from(path),
                        hunk_id,
                    }
                } else {
                    PluginRequest::AgentRejectProposal {
                        session_id,
                        path: PathBuf::from(path),
                        hunk_id,
                    }
                };
                self.send_request(request);
            }
            "AgentPermissionResponse" => {
                let request_id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        anyhow::anyhow!("AgentPermissionResponse requires a request id")
                    })?
                    .to_string();
                let option_id = args
                    .get(/*index*/ 1)
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                self.send_request(PluginRequest::AgentPermissionResponse {
                    request_id,
                    option_id,
                });
            }
            "RevertTransaction" => {
                let transaction_id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("RevertTransaction requires an id"))?
                    .to_string();
                self.send_request(PluginRequest::Action(Action::RevertTransaction(
                    transaction_id,
                )));
            }
            "SetCursorPosition" => {
                let x = args.first().and_then(value_to_u64).unwrap_or(0) as usize;
                let y = args.get(1).and_then(value_to_u64).unwrap_or(0) as usize;
                self.send_request(PluginRequest::SetCursorPosition { x, y });
            }
            "CloseScratchBuffer" => {
                let buffer_index = args
                    .first()
                    .and_then(value_to_u64)
                    .and_then(|index| usize::try_from(index).ok())
                    .ok_or_else(|| anyhow::anyhow!("CloseScratchBuffer requires a buffer index"))?;
                self.send_request(PluginRequest::CloseScratchBuffer { buffer_index });
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
                self.send_request(PluginRequest::SetPluginStorage {
                    plugin: plugin.to_string(),
                    key,
                    value,
                });
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
                self.send_request(PluginRequest::SetDecorations {
                    namespace,
                    decorations,
                });
            }
            "ClearDecorations" => {
                let namespace = args
                    .first()
                    .and_then(Value::as_str)
                    .map_or_else(|| "default".to_string(), str::to_string);
                self.send_request(PluginRequest::ClearDecorations { namespace });
            }
            "SetGutterSigns" => {
                let namespace = args
                    .first()
                    .and_then(Value::as_str)
                    .unwrap_or("default")
                    .to_string();
                let signs = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<Vec<GutterSign>>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::SetGutterSigns { namespace, signs });
            }
            "ClearGutterSigns" => {
                let namespace = args
                    .first()
                    .and_then(Value::as_str)
                    .map_or_else(|| "default".to_string(), str::to_string);
                self.send_request(PluginRequest::ClearGutterSigns { namespace });
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
                self.send_request(PluginRequest::OpenDynamicPicker {
                    title,
                    id,
                    items,
                    options,
                });
            }
            "OpenAgentComposer" => {
                let title = args.first().and_then(Value::as_str).map(str::to_string);
                let id = args.get(1).and_then(value_to_i32).unwrap_or(1);
                let query = args.get(2).map(value_to_string).unwrap_or_default();
                let history = args
                    .get(3)
                    .map(value_to_json)
                    .map(serde_json::from_value::<Vec<String>>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::OpenAgentComposer {
                    owner: plugin.to_string(),
                    title,
                    id,
                    query,
                    history,
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
                self.send_request(PluginRequest::UpdatePickerItems { id, items });
            }
            "UpdatePickerQuery" => {
                let id = args.first().and_then(value_to_i32).unwrap_or(1);
                let query = args.get(1).map(value_to_string).unwrap_or_default();
                self.send_request(PluginRequest::UpdatePickerQuery { id, query });
            }
            "UpdatePickerStatus" => {
                let id = args.first().and_then(value_to_i32).unwrap_or(1);
                let status = args.get(1).map(value_to_string);
                self.send_request(PluginRequest::UpdatePickerStatus { id, status });
            }
            "ClosePicker" => {
                let id = args.first().and_then(value_to_i32).unwrap_or(1);
                self.send_request(PluginRequest::ClosePicker { id });
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
                self.send_request(PluginRequest::OpenLocation { location, target });
            }
            "OpenBuffer" => {
                let name = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("OpenBuffer requires a buffer name"))?
                    .to_string();
                self.send_request(PluginRequest::Action(Action::OpenBuffer(name)));
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
                self.send_request(PluginRequest::WatchDirectory {
                    path,
                    watch_id,
                    recursive,
                    interval_ms,
                });
            }
            "UnwatchDirectory" => {
                let watch_id = args.first().and_then(value_to_i32).unwrap_or(1);
                self.send_request(PluginRequest::UnwatchDirectory { watch_id });
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
                self.send_request(PluginRequest::CreateOverlay { id, config });
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
                self.send_request(PluginRequest::UpdateOverlay { id, lines });
            }
            "RemoveOverlay" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("RemoveOverlay requires an overlay id"))?
                    .to_string();
                self.send_request(PluginRequest::RemoveOverlay { id });
            }
            "CreateWindowBar" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("CreateWindowBar requires a bar id"))?
                    .to_string();
                let config = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<WindowBarConfig>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::CreateWindowBar { id, config });
            }
            "UpdateWindowBar" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("UpdateWindowBar requires a bar id"))?
                    .to_string();
                let window_id = args
                    .get(1)
                    .and_then(value_to_u64)
                    .ok_or_else(|| anyhow::anyhow!("UpdateWindowBar requires a window id"))?;
                let segments = args
                    .get(2)
                    .map(value_to_json)
                    .map(serde_json::from_value::<Vec<WindowBarSegment>>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::UpdateWindowBar {
                    id,
                    window_id,
                    segments,
                });
            }
            "CloseWindowBar" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("CloseWindowBar requires a bar id"))?
                    .to_string();
                let window_id = args.get(1).and_then(value_to_u64);
                self.send_request(PluginRequest::CloseWindowBar { id, window_id });
            }
            "OpenWorkspace" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("OpenWorkspace requires a workspace id"))?
                    .to_string();
                let config = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<WorkspaceConfig>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::OpenWorkspace { id, config });
            }
            "UpdateWorkspace" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("UpdateWorkspace requires a workspace id"))?
                    .to_string();
                let model = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<WorkspaceModel>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::UpdateWorkspace { id, model });
            }
            "CloseWorkspace" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("CloseWorkspace requires a workspace id"))?
                    .to_string();
                self.send_request(PluginRequest::CloseWorkspace { id });
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
                self.send_request(PluginRequest::CreatePanel { id, config });
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
                self.send_request(PluginRequest::UpdatePanel { id, rows });
            }
            "CreateTextPanel" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("CreateTextPanel requires a panel id"))?
                    .to_string();
                let config = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<PanelConfig>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::CreateTextPanel { id, config });
            }
            "UpdateTextPanel" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("UpdateTextPanel requires a panel id"))?
                    .to_string();
                let blocks = args
                    .get(1)
                    .map(value_to_json)
                    .map(serde_json::from_value::<Vec<TextPanelBlock>>)
                    .transpose()?
                    .unwrap_or_default();
                self.send_request(PluginRequest::UpdateTextPanel { id, blocks });
            }
            "AppendTextPanel" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("AppendTextPanel requires a panel id"))?
                    .to_string();
                let block_id = args
                    .get(1)
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("AppendTextPanel requires a block id"))?
                    .to_string();
                let delta = args.get(2).map(value_to_string).unwrap_or_default();
                self.send_request(PluginRequest::AppendTextPanel {
                    id,
                    block_id,
                    delta,
                });
            }
            "FocusTextPanelComposer" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("FocusTextPanelComposer requires a panel id"))?
                    .to_string();
                self.send_request(PluginRequest::FocusTextPanelComposer { id });
            }
            "SetTextPanelComposerState" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        anyhow::anyhow!("SetTextPanelComposerState requires a panel id")
                    })?
                    .to_string();
                let enabled = args.get(1).and_then(Value::as_bool).unwrap_or(true);
                let status = args.get(2).and_then(Value::as_str).map(str::to_string);
                self.send_request(PluginRequest::SetTextPanelComposerState {
                    id,
                    enabled,
                    status,
                });
            }
            "SetTextPanelStatus" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("SetTextPanelStatus requires a panel id"))?
                    .to_string();
                let status = match args.get(1).map(value_to_json) {
                    None | Some(serde_json::Value::Null) => None,
                    Some(value) => Some(serde_json::from_value::<TextPanelStatus>(value)?),
                };
                self.send_request(PluginRequest::SetTextPanelStatus { id, status });
            }
            "ClearTextPanelComposer" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("ClearTextPanelComposer requires a panel id"))?
                    .to_string();
                self.send_request(PluginRequest::ClearTextPanelComposer { id });
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
                self.send_request(PluginRequest::SelectPanelRow { id, row_id });
            }
            "FocusPanel" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("FocusPanel requires a panel id"))?
                    .to_string();
                self.send_request(PluginRequest::FocusPanel { id });
            }
            "FocusEditor" => {
                self.send_request(PluginRequest::FocusEditor);
            }
            "SetPanelVisible" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("SetPanelVisible requires a panel id"))?
                    .to_string();
                let visible = args.get(1).and_then(Value::as_bool).unwrap_or(true);
                self.send_request(PluginRequest::SetPanelVisible { id, visible });
            }
            "ClosePanel" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("ClosePanel requires a panel id"))?
                    .to_string();
                self.send_request(PluginRequest::ClosePanel { id });
            }
            "SpawnProcess" => {
                anyhow::ensure!(
                    self.staged_effects.is_none(),
                    "SpawnProcess is not allowed while a plugin reload is being staged"
                );
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
                anyhow::ensure!(
                    self.staged_effects.is_none(),
                    "KillProcess is not allowed while a plugin reload is being staged"
                );
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
                    json_str(&event, "cause"),
                    json_usize_at(&event, &["from", "x"]),
                    json_usize_at(&event, &["from", "y"]),
                    json_usize_at(&event, &["to", "x"]),
                    json_usize_at(&event, &["to", "y"]),
                    json_str(&event, "mode")
                );
                self.send_request(PluginRequest::Action(Action::Print(message)));
            }
            "RecordModeChanged" => {
                let event = first_json(args)?;
                let message = format!(
                    "mode:{}:{}->{}",
                    json_str(&event, "cause"),
                    json_str(&event, "from"),
                    json_str(&event, "to")
                );
                self.send_request(PluginRequest::Action(Action::Print(message)));
            }
            "RecordSearchHighlighted" => {
                let event = first_json(args)?;
                let message = format!(
                    "search:{}:{}:{}",
                    json_str(&event, "source"),
                    json_str(&event, "term"),
                    json_str(&event, "direction")
                );
                self.send_request(PluginRequest::Action(Action::Print(message)));
            }
            "RecordSearchCleared" => {
                let event = first_json(args)?;
                let message = format!("cleared:{}", json_str(&event, "term"));
                self.send_request(PluginRequest::Action(Action::Print(message)));
            }
            "SetTimeout" => {
                let delay_ms = args.first().and_then(value_to_u64).unwrap_or(0);
                let id = self.schedule_timeout(delay_ms);
                return Ok(Value::String(id));
            }
            "CancelTimeout" => {
                let timer_id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("CancelTimeout requires a timer id"))?;
                self.cancel_timeout(timer_id);
            }
            other => {
                anyhow::bail!("unsupported Red host action `{other}`");
            }
        }

        Ok(Value::Unit)
    }

    fn request(
        &mut self,
        plugin: &str,
        request_id: RequestId,
        action: &str,
        args: &[Value],
    ) -> anyhow::Result<()> {
        let request = match action {
            "GetViewportLayout" => PluginRequest::GetViewportLayout { request_id },
            "InlayHints" => {
                let range = args
                    .first()
                    .map(value_to_json)
                    .map(serde_json::from_value)
                    .transpose()?;
                PluginRequest::InlayHints { request_id, range }
            }
            "GetEditorInfo" => PluginRequest::EditorInfo(request_id),
            "EditHistory" => PluginRequest::EditHistory { request_id },
            "AgentProposals" => PluginRequest::AgentProposals {
                session_id: args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("AgentProposals requires a session id"))?
                    .to_string(),
                request_id,
            },
            "GetCursorPosition" => PluginRequest::GetCursorPosition { request_id },
            "GetCursorDisplayColumn" => PluginRequest::GetCursorDisplayColumn { request_id },
            "GetBufferText" => {
                let start_line = args
                    .first()
                    .and_then(value_to_u64)
                    .and_then(|line| usize::try_from(line).ok());
                let end_line = args
                    .get(1)
                    .and_then(value_to_u64)
                    .and_then(|line| usize::try_from(line).ok());
                PluginRequest::GetBufferText {
                    request_id,
                    start_line,
                    end_line,
                }
            }
            "GetSelection" => PluginRequest::GetSelection { request_id },
            "GetAgentContext" => PluginRequest::GetAgentContext { request_id },
            "OpenScratchBuffer" => PluginRequest::OpenScratchBuffer {
                request_id,
                name: args.first().map(value_to_string).unwrap_or_default(),
                text: args.get(1).map(value_to_string).unwrap_or_default(),
            },
            "GetConfig" => PluginRequest::GetConfig {
                request_id,
                key: args.first().and_then(Value::as_str).map(str::to_string),
            },
            "GetStorage" => {
                let key = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("GetStorage requires a storage key"))?
                    .to_string();
                PluginRequest::GetPluginStorage {
                    plugin: plugin.to_string(),
                    key,
                    request_id,
                }
            }
            "GetEditorState" => PluginRequest::GetEditorState { request_id },
            "RestoreEditorState" => {
                let snapshot = args
                    .first()
                    .map(value_to_json)
                    .map(serde_json::from_value)
                    .transpose()?
                    .ok_or_else(|| anyhow::anyhow!("RestoreEditorState requires a snapshot"))?;
                PluginRequest::RestoreEditorState {
                    request_id,
                    snapshot,
                }
            }
            "GetWindows" => PluginRequest::GetWindows { request_id },
            "DocumentSymbols" => {
                let buffer_index = args
                    .first()
                    .and_then(value_to_u64)
                    .and_then(|index| usize::try_from(index).ok());
                PluginRequest::DocumentSymbols {
                    request_id,
                    buffer_index,
                }
            }
            "WorkspaceSymbols" => PluginRequest::WorkspaceSymbols {
                request_id,
                query: args.first().map(value_to_query_string).unwrap_or_default(),
            },
            "References" => PluginRequest::References {
                request_id,
                include_declaration: args.first().and_then(Value::as_bool).unwrap_or(true),
            },
            "ResolveThemeStyle" => {
                let spec = args
                    .first()
                    .map(value_to_json)
                    .map(serde_json::from_value)
                    .transpose()?
                    .ok_or_else(|| anyhow::anyhow!("ResolveThemeStyle requires a style spec"))?;
                PluginRequest::ResolveThemeStyle { request_id, spec }
            }
            "ListRuntimeAssets" => {
                let kind = match args.first().and_then(Value::as_str).unwrap_or("themes") {
                    "plugin" | "plugins" => RuntimeAssetKind::Plugin,
                    "theme" | "themes" => RuntimeAssetKind::Theme,
                    other => anyhow::bail!("unsupported runtime asset kind: {other}"),
                };
                PluginRequest::ListRuntimeAssets { kind, request_id }
            }
            "GetTextDisplayWidth" => PluginRequest::GetTextDisplayWidth {
                request_id,
                text: args.first().map(value_to_string).unwrap_or_default(),
            },
            "CharIndexToDisplayColumn" => PluginRequest::CharIndexToDisplayColumn {
                request_id,
                x: args.first().and_then(value_to_u64).unwrap_or(0) as usize,
                y: args.get(1).and_then(value_to_u64).unwrap_or(0) as usize,
            },
            "DisplayColumnToCharIndex" => PluginRequest::DisplayColumnToCharIndex {
                request_id,
                column: args.first().and_then(value_to_u64).unwrap_or(0) as usize,
                y: args.get(1).and_then(value_to_u64).unwrap_or(0) as usize,
            },
            "ListDirectory" => PluginRequest::ListDirectory {
                path: args
                    .first()
                    .and_then(Value::as_str)
                    .unwrap_or(".")
                    .to_string(),
                request_id,
            },
            "GetGitStatus" => PluginRequest::GetGitStatus {
                path: args
                    .first()
                    .and_then(Value::as_str)
                    .unwrap_or(".")
                    .to_string(),
                request_id,
            },
            other => anyhow::bail!("unsupported Red host request: {other}"),
        };
        self.send_request(request);
        Ok(())
    }

    fn query(&mut self, _plugin: &str, query: &str) -> anyhow::Result<Value> {
        self.snapshots
            .get(query)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Husk host snapshot `{query}` is unavailable"))
    }
}

fn first_json(args: &[Value]) -> anyhow::Result<serde_json::Value> {
    match args.first() {
        Some(value) => Ok(value.to_json()),
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

fn schedule_timeout_with_id(id: String, delay_ms: u64) {
    PENDING_TIMEOUTS.lock().unwrap().push(PendingTimeout {
        id: id.clone(),
        expires_at: Instant::now() + Duration::from_millis(delay_ms),
    });
}

#[cfg(test)]
fn schedule_timeout(delay_ms: u64) -> String {
    let id = Uuid::new_v4().to_string();
    schedule_timeout_with_id(id.clone(), delay_ms);
    id
}

fn cancel_timeout(timer_id: &str) {
    PENDING_TIMEOUTS
        .lock()
        .unwrap()
        .retain(|timeout| timeout.id != timer_id);
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::Unit | Value::Null | Value::Missing(_) => String::new(),
        Value::Bool(value) => value.to_string(),
        Value::Int(value) => value.to_string(),
        Value::Float(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(_) | Value::Object(_) => value.to_json().to_string(),
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
        Value::Unit | Value::Null | Value::Missing(_) => serde_json::Value::Null,
        Value::Bool(value) => serde_json::Value::Bool(*value),
        Value::Int(value) => serde_json::Value::Number((*value).into()),
        Value::Float(value) => serde_json::Number::from_f64(*value)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::String(value) => serde_json::Value::String(value.clone()),
        Value::Array(_) | Value::Object(_) => value.to_json(),
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

/// A command currently registered by an active Husk plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredPluginCommand {
    /// Exact, case-sensitive command name.
    pub name: String,
    /// Plugin that owns the command.
    pub plugin: String,
    /// User-facing command information supplied during registration.
    pub metadata: CommandMetadata,
}

struct RuntimeInner {
    vm: husk::Vm,
    host: RedHost,
    anonymous_module_count: usize,
    typecheck_enabled: bool,
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
        let mut vm = husk::Vm::new();
        vm.set_instruction_budget(PLUGIN_INSTRUCTION_BUDGET);
        Ok(Self {
            inner: Arc::new(Mutex::new(RuntimeInner {
                vm,
                host: RedHost::new(process_permissions),
                anonymous_module_count: 0,
                typecheck_enabled: true,
            })),
        })
    }

    pub fn set_typecheck_enabled(&mut self, enabled: bool) {
        self.inner.lock().unwrap().typecheck_enabled = enabled;
    }

    pub async fn load_plugin(&mut self, name: &str, source: &str) -> anyhow::Result<()> {
        self.load_plugin_at(name, format!("plugins/{name}.hk"), source)
            .await
    }

    pub async fn load_plugin_at(
        &mut self,
        name: &str,
        path: impl Into<String>,
        source: &str,
    ) -> anyhow::Result<()> {
        let _span = crate::editor::perf::PerfSpan::with_detail("husk:load", name);
        let mut inner = self.inner.lock().unwrap();
        let path = path.into();
        if inner.typecheck_enabled {
            validate_plugin_source(name, &path, source)?;
        }
        let RuntimeInner { vm, host, .. } = &mut *inner;
        host.begin_reload();
        let result = vm.reload_plugin_at(name, path, source, host);
        if result.is_ok() {
            host.commit_reload();
        } else {
            host.rollback_reload();
        }
        result
    }

    pub fn unload_plugin(&mut self, name: &str) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let RuntimeInner { vm, host, .. } = &mut *inner;
        let result = vm.deactivate_plugin(name, host);
        host.process_manager.shutdown_plugin(name);
        result
    }

    #[must_use]
    pub fn command_plugin(&self, command: &str) -> Option<String> {
        self.inner
            .lock()
            .unwrap()
            .vm
            .commands()
            .get(command)
            .map(|callback| callback.plugin().to_string())
    }

    /// Returns the active plugin commands in a stable order for discovery UI.
    #[must_use]
    pub fn registered_commands(&self) -> Vec<RegisteredPluginCommand> {
        let inner = self.inner.lock().unwrap();
        let mut commands = inner
            .vm
            .commands()
            .iter()
            .map(|(name, callback)| RegisteredPluginCommand {
                name: name.clone(),
                plugin: callback.plugin().to_string(),
                metadata: inner.vm.command_metadata(name).cloned().unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        commands.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        commands
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
        let _span = crate::editor::perf::PerfSpan::with_detail("husk:command", command);
        let mut inner = self.inner.lock().unwrap();
        let RuntimeInner { vm, host, .. } = &mut *inner;
        vm.execute_command(command, host)
    }

    pub async fn notify(&mut self, event: &str, args: serde_json::Value) -> anyhow::Result<()> {
        let _span = crate::editor::perf::PerfSpan::with_detail("husk:notify", event);
        let mut inner = self.inner.lock().unwrap();
        let RuntimeInner { vm, host, .. } = &mut *inner;
        vm.notify(event, args, host)
    }

    pub fn notify_isolated(
        &mut self,
        event: &str,
        args: serde_json::Value,
    ) -> Vec<(String, anyhow::Error)> {
        let mut inner = self.inner.lock().unwrap();
        let RuntimeInner { vm, host, .. } = &mut *inner;
        vm.notify_isolated(event, args, host)
    }

    pub fn notify_plugin_isolated(
        &mut self,
        plugin: &str,
        event: &str,
        args: serde_json::Value,
    ) -> Vec<(String, anyhow::Error)> {
        let mut inner = self.inner.lock().unwrap();
        let RuntimeInner { vm, host, .. } = &mut *inner;
        vm.notify_plugin_isolated(plugin, event, args, host)
    }

    pub async fn resolve_request(
        &mut self,
        request_id: RequestId,
        payload: serde_json::Value,
    ) -> anyhow::Result<bool> {
        let mut inner = self.inner.lock().unwrap();
        let RuntimeInner { vm, host, .. } = &mut *inner;
        vm.resolve_request(request_id, payload, host)
    }

    #[must_use]
    pub fn request_plugin(&self, request_id: RequestId) -> Option<String> {
        self.inner
            .lock()
            .unwrap()
            .vm
            .request_plugin(request_id)
            .map(str::to_string)
    }

    pub fn set_snapshot(&mut self, name: impl Into<String>, value: serde_json::Value) {
        let mut inner = self.inner.lock().unwrap();
        inner.host.set_snapshot(name, value);
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

fn validate_plugin_source(name: &str, path: &str, source: &str) -> anyhow::Result<()> {
    let parsed = husk_parser::parse_str(source);
    let Some(file) = parsed.file.as_ref() else {
        return Ok(());
    };
    super::api::validate_parsed_source(name, path, source, file)?;
    if !parsed.errors.is_empty() {
        // The VM parser produces the canonical parse diagnostic and error code.
        return Ok(());
    }
    let host = RED_HOST_AST.get_or_init(|| {
        let parsed = husk_parser::parse_str(RED_HOST_DECLARATIONS);
        assert!(parsed.errors.is_empty(), "Red host declarations must parse");
        parsed
            .file
            .expect("Red host declarations must produce an AST")
    });
    let result = husk_semantic::analyze_file_with_declarations(file, std::slice::from_ref(host));
    let mut errors = result.symbols.errors.into_iter().chain(result.type_errors);
    let Some(first_error) = errors.next() else {
        return Ok(());
    };

    let source_file = SourceFile::new(path, source);
    let diagnostics = std::iter::once(first_error)
        .chain(errors)
        .map(|error| {
            HuskDiagnostic::new(
                "HUSK-T0001",
                error.message,
                source_file.clone(),
                error.span,
                "incompatible plugin expression",
            )
            .with_note(format!("while typechecking plugin `{name}`"))
        })
        .collect::<Vec<_>>();
    Err(anyhow::Error::new(HuskReport::from_diagnostics(
        diagnostics,
    )))
}

#[allow(dead_code)]
fn _keep_config_used(_: &Config) {}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        process::Command,
        time::{Duration, Instant},
    };

    use super::*;
    use crate::{
        color::Color,
        editor::{PluginRequest, PLUGIN_DISPATCHER_TEST_LOCK},
        ui::PickerPresentation,
    };

    fn drain_requests() {
        while ACTION_DISPATCHER.try_recv_request().is_some() {}
    }

    fn sample_indent_layout() -> serde_json::Value {
        serde_json::json!({
            "buffer_index": 3,
            "revision": 1,
            "vtop": 0,
            "width": 80,
            "height": 24,
            "cursor": { "x": 0, "y": 2 },
            "indentation": {
                "shift_width": 4,
                "tab_width": 4,
            },
            "rows": [
                { "line": 0, "text": "fn main() {", "first_segment": true },
                { "line": 1, "text": "    if ok {", "first_segment": true },
                { "line": 2, "text": "        call();", "first_segment": true },
                { "line": 3, "text": "    }", "first_segment": true },
                { "line": 4, "text": "}", "first_segment": true }
            ]
        })
    }

    fn non_tabstop_indent_layout() -> serde_json::Value {
        let mut layout = sample_indent_layout();
        layout["cursor"]["y"] = serde_json::json!(1);
        layout["rows"] = serde_json::json!([
            { "line": 0, "text": "fn main() {", "first_segment": true },
            {
                "line": 1,
                "text": format!("{}call();", " ".repeat(39)),
                "first_segment": true
            },
            { "line": 2, "text": "}", "first_segment": true }
        ]);
        layout
    }

    fn sample_indent_editor_info(normal: Color, active: Color) -> serde_json::Value {
        serde_json::json!({
            "theme": {
                "colors": {
                    "editorIndentGuide.background": normal,
                    "editorIndentGuide.activeBackground": active,
                    "editor.foreground": Color::Rgb { r: 220, g: 220, b: 220 },
                    "editor.background": Color::Rgb { r: 16, g: 16, b: 16 },
                },
                "style": {
                    "fg": Color::Rgb { r: 220, g: 220, b: 220 },
                    "bg": Color::Rgb { r: 16, g: 16, b: 16 },
                },
                "gutter_style": { "fg": null },
            }
        })
    }

    fn sample_symbol_payload() -> serde_json::Value {
        serde_json::json!({
            "ok": true,
            "symbols": [{
                "name": "main",
                "detail": "fn()",
                "kind": 12,
                "kind_name": "Function",
                "file": "src/main.rs",
                "range": {
                    "start": { "line": 4, "character": 0 },
                    "end": { "line": 6, "character": 1 }
                },
                "selection_range": {
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
                .get("process_id")
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
    async fn cancelled_timeout_never_reaches_the_editor_queue() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        let timer_id = schedule_timeout(0);

        cancel_timeout(&timer_id);

        assert!(!poll_timer_callbacks().into_iter().any(|request| {
            matches!(
                request,
                PluginRequest::TimeoutCallback { timer_id: id } if id == timer_id
            )
        }));
    }

    #[tokio::test]
    async fn polling_due_timeouts_preserves_order_and_pending_timers() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        let due = (0..128).map(|_| schedule_timeout(0)).collect::<Vec<_>>();
        let pending = schedule_timeout(60_000);

        let callbacks = poll_timer_callbacks()
            .into_iter()
            .filter_map(|request| match request {
                PluginRequest::TimeoutCallback { timer_id } if due.contains(&timer_id) => {
                    Some(timer_id)
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(callbacks, due);
        assert!(PENDING_TIMEOUTS
            .lock()
            .unwrap()
            .iter()
            .any(|timeout| timeout.id == pending));
        cancel_timeout(&pending);
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
    async fn registered_commands_include_owner_and_discovery_metadata() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        let source = r#"
            pub fn activate() {
                red::add_command("ProjectSearch", search, Json {
                    title: "Search project",
                    category: "Search",
                    aliases: ["ripgrep"],
                });
                red::add_command("BufferPicker", buffers);
            }

            fn search() {}
            fn buffers() {}
        "#;
        let mut runtime = Runtime::new();

        runtime.load_plugin("navigation", source).await.unwrap();

        let commands = runtime.registered_commands();
        assert_eq!(
            commands
                .iter()
                .map(|command| command.name.as_str())
                .collect::<Vec<_>>(),
            vec!["BufferPicker", "ProjectSearch"]
        );
        assert_eq!(commands[1].plugin, "navigation");
        assert_eq!(
            commands[1].metadata.title.as_deref(),
            Some("Search project")
        );
        assert_eq!(commands[1].metadata.category.as_deref(), Some("Search"));
        assert_eq!(commands[1].metadata.aliases, vec!["ripgrep"]);
    }

    #[tokio::test]
    async fn husk_can_drive_the_native_agent_bridge() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let source = r#"
            pub fn activate() {
                red::add_command("AgentStart", start);
                red::add_command("AgentAsk", ask);
                red::add_command("AgentStop", stop);
                red::add_command("AgentClose", close);
            }

            fn start() { red::execute("AgentNewSession", "/workspace"); }
            fn ask() { red::execute("AgentPrompt", "session-1", "hello"); }
            fn stop() { red::execute("AgentCancel", "session-1"); }
            fn close() { red::execute("AgentCloseSession", "session-1"); }
        "#;
        let mut runtime = Runtime::new();
        runtime.load_plugin("test", source).await.unwrap();

        runtime.execute_command("AgentStart").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));

        runtime.execute_command("AgentAsk").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentPrompt { session_id, text }
                if session_id == "session-1" && text == "hello"
        ));

        runtime.execute_command("AgentStop").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentCancel { session_id } if session_id == "session-1"
        ));

        runtime.execute_command("AgentClose").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentCloseSession { session_id } if session_id == "session-1"
        ));
    }

    #[tokio::test]
    async fn bundled_agent_command_opens_prompt_and_lazily_starts_session() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        runtime.execute_command("Agent").await.unwrap();
        let history_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage {
                plugin,
                key,
                request_id,
            } => {
                assert_eq!(plugin, "agent");
                assert_eq!(key, "prompt_history");
                request_id
            }
            _ => panic!("expected agent prompt-history request"),
        };
        runtime
            .resolve_request(history_request_id, serde_json::json!({ "value": [] }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::OpenAgentComposer { id: 802, .. }
        ));
        runtime
            .notify(
                "composer:submitted:802",
                serde_json::json!("explain the workspace"),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, value }
                if plugin == "agent"
                    && key == "prompt_history"
                    && value == serde_json::json!(["explain the workspace"])
        ));

        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("cwd"));
                request_id
            }
            _ => panic!("expected the pending prompt to request the workspace root"),
        };
        runtime
            .resolve_request(request_id, serde_json::json!({ "value": "/workspace" }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd }
                if cwd.as_path() == std::path::Path::new("/workspace")
        ));

        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-lazy" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::CreateTextPanel { id, .. } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, .. } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "Agent session started"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, blocks }
                if id == "agent-conversation"
                    && blocks.len() == 1
                    && blocks[0].text == "explain the workspace"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::FocusPanel { id } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelStatus { id, status: Some(status) }
                if id == "agent-conversation"
                    && status.busy
                    && status.label == "Waiting for agent…"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Refresh)
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, .. }
                if plugin == "agent" && key == "transcript"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentPrompt { session_id, text }
                if session_id == "session-lazy" && text == "explain the workspace"
        ));
    }

    #[tokio::test]
    async fn bundled_agent_plugin_creates_prompts_streams_and_cancels() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        runtime.execute_command("AgentStart").await.unwrap();
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("cwd"));
                request_id
            }
            _ => panic!("expected current-directory request"),
        };
        runtime
            .resolve_request(request_id, serde_json::json!({ "value": "/workspace" }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));

        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::CreateTextPanel { id, config }
                if id == "agent-conversation"
                    && config.side == crate::plugin::PanelSide::Right
                    && config.width == 62
                    && config.title.as_deref() == Some("Agent")
                    && config.header_actions.iter().map(|action| action.id.as_str()).eq(["clear", "new", "close"])
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, blocks }
                if id == "agent-conversation"
                    && blocks.len() == 1
                    && blocks[0].id == "empty"
                    && blocks[0].kind == crate::plugin::TextPanelBlockKind::Activity
                    && blocks[0].format == crate::plugin::TextPanelBlockFormat::Plain
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "Agent session started"
        ));

        runtime.execute_command("AgentPrompt").await.unwrap();
        let history_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage {
                plugin,
                key,
                request_id,
            } => {
                assert_eq!(plugin, "agent");
                assert_eq!(key, "prompt_history");
                request_id
            }
            _ => panic!("expected agent prompt-history request"),
        };
        runtime
            .resolve_request(
                history_request_id,
                serde_json::json!({ "value": ["previous prompt", "previous prompt", " \n "] }),
            )
            .await
            .unwrap();
        let (owner, title, query, history) = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenAgentComposer {
                owner,
                title,
                id: 802,
                query,
                history,
            } => (owner, title, query, history),
            _ => panic!("expected agent composer"),
        };
        assert_eq!(owner, "agent");
        assert_eq!(title.as_deref(), Some("Agent prompt"));
        assert!(query.is_empty());
        assert_eq!(history, ["previous prompt"]);
        runtime
            .notify(
                "composer:submitted:802",
                serde_json::json!("  inspect the workspace\ninclude all unsaved changes  "),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, blocks }
                if id == "agent-conversation"
                    && blocks.len() == 1
                    && blocks[0].id == "user:1"
                    && blocks[0].kind == crate::plugin::TextPanelBlockKind::User
                    && blocks[0].format == crate::plugin::TextPanelBlockFormat::Plain
                    && blocks[0].text == "  inspect the workspace\ninclude all unsaved changes  "
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::FocusPanel { id } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelStatus { id, status: Some(status) }
                if id == "agent-conversation"
                    && status.busy
                    && status.label == "Waiting for agent…"
                    && !status.stream
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Refresh)
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, .. }
                if plugin == "agent" && key == "transcript"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentPrompt { session_id, text }
                if session_id == "session-1"
                    && text == "  inspect the workspace\ninclude all unsaved changes  "
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, value }
                if plugin == "agent"
                    && key == "prompt_history"
                    && value == serde_json::json!([
                        "  inspect the workspace\ninclude all unsaved changes  ",
                        "previous prompt"
                    ])
        ));
        runtime
            .notify(
                "agent:update",
                serde_json::json!({
                    "session_id": "session-1",
                    "text": "streamed output",
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, blocks }
                if id == "agent-conversation"
                    && blocks.len() == 2
                    && blocks[1].id == "agent:2"
                    && blocks[1].kind == crate::plugin::TextPanelBlockKind::Agent
                    && blocks[1].format == crate::plugin::TextPanelBlockFormat::Markdown
                    && blocks[1].text.is_empty()
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelStatus { id, status: Some(status) }
                if id == "agent-conversation"
                    && status.busy
                    && status.label == "Writing…"
                    && status.stream
        ));
        runtime
            .notify(
                "agent:update",
                serde_json::json!({
                    "session_id": "session-1",
                    "text": " 👋\nnext line",
                }),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(70)).await;
        for callback in poll_timer_callbacks() {
            if let PluginRequest::TimeoutCallback { timer_id } = callback {
                runtime
                    .notify(
                        "timeout:callback",
                        serde_json::json!({ "timer_id": timer_id }),
                    )
                    .await
                    .unwrap();
            }
        }
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, value }
                if plugin == "agent"
                    && key == "transcript"
                    && value
                        == serde_json::json!("You:   inspect the workspace\ninclude all unsaved changes  \nAgent: streamed output 👋\nnext line")
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AppendTextPanel { id, block_id, delta }
                if id == "agent-conversation"
                    && block_id == "agent:2"
                    && delta == "streamed output 👋\nnext line"
        ));

        runtime
            .notify(
                "agent:update",
                serde_json::json!({
                    "session_id": "session-1",
                    "text": "\n\ncontinued",
                }),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(70)).await;
        for callback in poll_timer_callbacks() {
            if let PluginRequest::TimeoutCallback { timer_id } = callback {
                runtime
                    .notify(
                        "timeout:callback",
                        serde_json::json!({ "timer_id": timer_id }),
                    )
                    .await
                    .unwrap();
            }
        }
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, .. }
                if plugin == "agent" && key == "transcript"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AppendTextPanel { id, block_id, delta }
                if id == "agent-conversation"
                    && block_id == "agent:2"
                    && delta == "\n\ncontinued"
        ));

        let large_delta = "z".repeat(20_001);
        runtime
            .notify(
                "agent:update",
                serde_json::json!({
                    "session_id": "session-1",
                    "text": large_delta,
                }),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(70)).await;
        for callback in poll_timer_callbacks() {
            if let PluginRequest::TimeoutCallback { timer_id } = callback {
                runtime
                    .notify(
                        "timeout:callback",
                        serde_json::json!({ "timer_id": timer_id }),
                    )
                    .await
                    .unwrap();
            }
        }
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, value }
                if plugin == "agent"
                    && key == "transcript"
                    && value.as_str().is_some_and(|text| text.len() <= 20_000)
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, blocks }
                if id == "agent-conversation"
                    && blocks.iter().map(|block| block.text.len()).sum::<usize>() <= 20_000
                    && blocks.iter().any(|block| {
                        block.id == "agent:2"
                            && block.kind == crate::plugin::TextPanelBlockKind::Agent
                            && block.format == crate::plugin::TextPanelBlockFormat::Markdown
                    })
        ));

        runtime
            .notify(
                "agent:completed",
                serde_json::json!({
                    "session_id": "session-1",
                    "stop_reason": "completed",
                    "elapsed_ms": 3_723_000,
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, value }
                if plugin == "agent"
                    && key == "transcript"
                    && value.as_str().is_some_and(|text| text.ends_with('\n'))
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, blocks }
                if id == "agent-conversation"
                    && blocks.last().is_some_and(|block| {
                        block.kind == crate::plugin::TextPanelBlockKind::Activity
                            && block.text == "Worked for 1h 2m 3s"
                    })
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelStatus { id, status: None }
                if id == "agent-conversation"
        ));

        runtime.execute_command("AgentCancel").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentCancel { session_id } if session_id == "session-1"
        ));

        runtime.execute_command("AgentReview").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::OpenWorkspace { id, .. } if id == "agent-review"
        ));
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::AgentProposals {
                session_id,
                request_id,
            } => {
                assert_eq!(session_id, "session-1");
                request_id
            }
            _ => panic!("expected proposal review request"),
        };
        runtime
            .resolve_request(request_id, serde_json::json!({ "files": [] }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateWorkspace { id, .. } if id == "agent-review"
        ));

        runtime
            .notify(
                "agent:permission_requested",
                serde_json::json!({
                    "request_id": "permission-1",
                    "session_id": "session-1",
                    "tool_call": { "tool_call_id": "tool-1" },
                    "options": [{
                        "option_id": "allow-once-exact",
                        "name": "Allow once",
                        "kind": "allow_once",
                    }],
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::OpenDynamicPicker { id: 801, .. }
        ));
        runtime
            .notify(
                "picker:selected:801",
                serde_json::json!({
                    "data": { "option_id": "allow-once-exact" }
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentPermissionResponse {
                request_id,
                option_id: Some(option_id),
            } if request_id == "permission-1" && option_id == "allow-once-exact"
        ));

        runtime.execute_command("AgentHistory").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::OpenWorkspace { id, .. } if id == "agent-history"
        ));
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::EditHistory { request_id } => request_id,
            _ => panic!("expected attributed history request"),
        };
        runtime
            .resolve_request(request_id, serde_json::json!({ "entries": [] }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateWorkspace { id, .. } if id == "agent-history"
        ));
    }

    #[tokio::test]
    async fn bundled_agent_rejects_a_concurrent_prompt_without_closing_the_active_stream() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        drain_requests();

        runtime
            .notify("composer:submitted:802", serde_json::json!("first prompt"))
            .await
            .unwrap();
        let mut first_prompt = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            first_prompt |= matches!(
                request,
                PluginRequest::AgentPrompt { session_id, text }
                    if session_id == "session-1" && text == "first prompt"
            );
        }
        assert!(first_prompt);
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        drain_requests();
        runtime
            .notify(
                "agent:update",
                serde_json::json!({ "session_id": "session-1", "text": "original output" }),
            )
            .await
            .unwrap();
        drain_requests();
        runtime
            .notify(
                "agent:cancelled",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        let mut cancellation_notice = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::Action(Action::Print(message)) => {
                    cancellation_notice |= message == "Agent cancellation requested";
                }
                PluginRequest::AgentCloseSession { .. } => {
                    panic!("cancellation must not close an active stream before completion")
                }
                _ => {}
            }
        }
        assert!(cancellation_notice);
        runtime
            .notify(
                "agent:error",
                serde_json::json!({ "message": "replacement session could not be created" }),
            )
            .await
            .unwrap();
        let mut setup_status = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::Action(Action::Print(message)) => {
                    setup_status |= message.contains("setup failed while a turn is active");
                }
                PluginRequest::SetPluginStorage { plugin, key, .. }
                    if plugin == "agent" && key == "transcript" =>
                {
                    panic!("unscoped setup failure closed the active transcript")
                }
                PluginRequest::UpdateTextPanel { .. } | PluginRequest::AppendTextPanel { .. } => {
                    panic!("unscoped setup failure changed the active conversation")
                }
                _ => {}
            }
        }
        assert!(setup_status);

        runtime
            .notify(
                "composer:submitted:802",
                serde_json::json!("concurrent prompt"),
            )
            .await
            .unwrap();
        let mut history_saved = false;
        let mut status = false;
        let mut queued_visible = false;
        let mut refreshed = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::SetPluginStorage { plugin, key, value }
                    if plugin == "agent" && key == "prompt_history" =>
                {
                    history_saved = value.as_array().is_some_and(|history| {
                        history.first().and_then(serde_json::Value::as_str)
                            == Some("concurrent prompt")
                    });
                }
                PluginRequest::Action(Action::Print(message)) => {
                    status |= message.contains("turn is still running");
                }
                PluginRequest::UpdateTextPanel { blocks, .. } => {
                    queued_visible |= blocks.iter().any(|block| {
                        block.kind == crate::plugin::TextPanelBlockKind::User
                            && block.text == "concurrent prompt"
                    });
                }
                PluginRequest::Action(Action::Refresh) => {
                    refreshed = true;
                }
                PluginRequest::AgentPrompt { .. } | PluginRequest::AppendTextPanel { .. } => {
                    panic!("concurrent prompt started before the active turn completed")
                }
                _ => {}
            }
        }
        assert!(history_saved);
        assert!(status);
        assert!(queued_visible);
        assert!(refreshed);
        runtime
            .notify(
                "agent:update",
                serde_json::json!({ "session_id": "session-1", "text": " still original" }),
            )
            .await
            .unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        runtime
            .notify(
                "agent:completed",
                serde_json::json!({ "session_id": "session-1", "stop_reason": "end_turn" }),
            )
            .await
            .unwrap();
        let mut closed = false;
        let mut replacement_request_id = None;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::AgentCloseSession { session_id } => {
                    closed |= session_id == "session-1";
                }
                PluginRequest::GetConfig { request_id, key } => {
                    assert_eq!(key.as_deref(), Some("cwd"));
                    replacement_request_id = Some(request_id);
                }
                _ => {}
            }
        }
        assert!(closed, "completed cancelled stream must rotate its session");
        runtime
            .resolve_request(
                replacement_request_id.expect("queued prompt must request a replacement session"),
                serde_json::json!({ "value": "/workspace" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-2" }),
            )
            .await
            .unwrap();
        let mut replacement_dispatched = false;
        let mut dispatched_prompts = Vec::new();
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::UpdateTextPanel { blocks, .. } => {
                    assert!(
                        blocks
                            .iter()
                            .filter(|block| block.text == "concurrent prompt")
                            .count()
                            <= 1,
                        "a queued prompt must not duplicate during session rotation"
                    );
                }
                PluginRequest::AgentPrompt { session_id, text } => {
                    assert_ne!(session_id, "session-1");
                    dispatched_prompts.push((session_id.clone(), text.clone()));
                    replacement_dispatched = session_id == "session-2"
                        && text.ends_with("Follow-up:\nconcurrent prompt");
                }
                _ => {}
            }
        }
        assert!(
            replacement_dispatched,
            "expected queued prompt on replacement session, got {dispatched_prompts:?}"
        );
    }

    #[tokio::test]
    async fn bundled_agent_panel_submits_and_drains_followups_in_fifo_order() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        drain_requests();

        runtime
            .notify(
                "panel:event:agent-conversation",
                serde_json::json!({ "action": "submit", "text": "first prompt" }),
            )
            .await
            .unwrap();
        let mut first = false;
        let mut focused = false;
        let mut rendered = false;
        let mut busy = false;
        let mut refreshed = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::UpdateTextPanel { id, blocks } => {
                    rendered |= id == "agent-conversation"
                        && blocks.iter().any(|block| block.text == "first prompt");
                }
                PluginRequest::FocusPanel { id } => {
                    focused |= id == "agent-conversation";
                }
                PluginRequest::SetTextPanelStatus {
                    id,
                    status: Some(status),
                } => {
                    busy |= id == "agent-conversation"
                        && status.busy
                        && status.label == "Waiting for agent…";
                }
                PluginRequest::Action(Action::Refresh) => {
                    assert!(rendered, "the submitted text must be ready before refresh");
                    assert!(busy, "the busy status must be ready before refresh");
                    refreshed = true;
                }
                PluginRequest::AgentPrompt { session_id, text } => {
                    assert!(
                        refreshed,
                        "the conversation must render before agent dispatch"
                    );
                    first |= session_id == "session-1" && text == "first prompt";
                }
                _ => {}
            }
        }
        assert!(first);
        assert!(focused);
        assert!(rendered);
        assert!(busy);
        assert!(refreshed);

        runtime
            .notify(
                "agent:update",
                serde_json::json!({ "session_id": "session-1", "text": "first answer" }),
            )
            .await
            .unwrap();
        drain_requests();

        for text in ["second prompt", "third prompt"] {
            runtime
                .notify(
                    "panel:event:agent-conversation",
                    serde_json::json!({ "action": "submit", "text": text }),
                )
                .await
                .unwrap();
        }
        let mut queued = 0;
        let mut refreshes = 0;
        let mut second_visible = false;
        let mut third_visible = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::UpdateTextPanel { id, blocks } => {
                    assert_eq!(id, "agent-conversation");
                    second_visible |= blocks.iter().any(|block| {
                        block.kind == crate::plugin::TextPanelBlockKind::User
                            && block.text == "second prompt"
                    });
                    third_visible |= blocks.iter().any(|block| {
                        block.kind == crate::plugin::TextPanelBlockKind::User
                            && block.text == "third prompt"
                    });
                }
                PluginRequest::Action(Action::Refresh) => {
                    refreshes += 1;
                }
                PluginRequest::Action(Action::Print(message)) => {
                    queued += usize::from(message.contains("follow-up queued"));
                }
                PluginRequest::AgentPrompt { .. } => {
                    panic!("follow-ups must not start while the first turn is active")
                }
                _ => {}
            }
        }
        assert_eq!(queued, 2);
        assert_eq!(refreshes, 2);
        assert!(second_visible);
        assert!(third_visible);

        runtime
            .notify(
                "agent:update",
                serde_json::json!({ "session_id": "session-1", "text": " continues" }),
            )
            .await
            .unwrap();
        assert!(
            ACTION_DISPATCHER.try_recv_request().is_none(),
            "queueing must not end the active stream"
        );

        runtime
            .notify(
                "agent:completed",
                serde_json::json!({ "session_id": "session-1", "stop_reason": "end_turn" }),
            )
            .await
            .unwrap();
        let mut delivered_second = false;
        let mut refreshed_second = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            assert!(
                !matches!(&request, PluginRequest::FocusPanel { .. }),
                "queued follow-ups must not steal panel focus"
            );
            match request {
                PluginRequest::UpdateTextPanel { blocks, .. } => {
                    assert_eq!(
                        blocks
                            .iter()
                            .filter(|block| block.text == "second prompt")
                            .count(),
                        1,
                        "promoting a queued prompt must not duplicate its block"
                    );
                }
                PluginRequest::Action(Action::Refresh) => {
                    refreshed_second = true;
                }
                PluginRequest::AgentPrompt { session_id, text } => {
                    assert!(refreshed_second);
                    delivered_second = session_id == "session-1" && text == "second prompt";
                }
                _ => {}
            }
        }
        assert!(delivered_second);

        runtime
            .notify(
                "agent:update",
                serde_json::json!({ "session_id": "session-1", "text": "second answer" }),
            )
            .await
            .unwrap();
        let mut ordered_before_pending = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::UpdateTextPanel { blocks, .. } = request {
                let second_user = blocks
                    .iter()
                    .position(|block| block.text == "second prompt")
                    .unwrap();
                let second_agent = blocks
                    .iter()
                    .position(|block| {
                        block.kind == crate::plugin::TextPanelBlockKind::Agent
                            && block.id != "agent:2"
                    })
                    .unwrap();
                let third_user = blocks
                    .iter()
                    .position(|block| block.text == "third prompt")
                    .unwrap();
                ordered_before_pending = second_user < second_agent && second_agent < third_user;
            }
        }
        assert!(
            ordered_before_pending,
            "the active answer must render before later queued prompts"
        );

        runtime
            .notify(
                "agent:completed",
                serde_json::json!({ "session_id": "session-1", "stop_reason": "end_turn" }),
            )
            .await
            .unwrap();
        let mut delivered_third = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            assert!(
                !matches!(&request, PluginRequest::FocusPanel { .. }),
                "queued follow-ups must not steal panel focus"
            );
            if let PluginRequest::AgentPrompt { session_id, text } = request {
                delivered_third = session_id == "session-1" && text == "third prompt";
            }
        }
        assert!(delivered_third);
    }

    #[tokio::test]
    async fn bundled_agent_clear_only_resets_the_visible_view_and_stream_timer() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        drain_requests();
        runtime
            .notify(
                "panel:event:agent-conversation",
                serde_json::json!({ "action": "submit", "text": "keep the context" }),
            )
            .await
            .unwrap();
        drain_requests();
        runtime
            .notify(
                "agent:update",
                serde_json::json!({ "session_id": "session-1", "text": "first chunk" }),
            )
            .await
            .unwrap();
        drain_requests();

        runtime.execute_command("AgentClear").await.unwrap();

        let mut cleared = false;
        let mut status = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::UpdateTextPanel { id, blocks } => {
                    cleared |= id == "agent-conversation" && blocks.is_empty();
                }
                PluginRequest::SetTextPanelComposerState {
                    id,
                    enabled,
                    status: value,
                } => {
                    status |= id == "agent-conversation"
                        && enabled
                        && value
                            .as_deref()
                            .is_some_and(|value| value.contains("context preserved"));
                }
                PluginRequest::SetPluginStorage { plugin, key, value }
                    if plugin == "agent"
                        && key == "transcript"
                        && value == serde_json::json!("") =>
                {
                    panic!("clear must preserve the durable transcript")
                }
                PluginRequest::ClearTextPanelComposer { .. } => {
                    panic!("clear must preserve the current draft")
                }
                PluginRequest::AgentCloseSession { .. } => {
                    panic!("clear must preserve the active session")
                }
                _ => {}
            }
        }
        assert!(cleared);
        assert!(status);
        tokio::time::sleep(Duration::from_millis(70)).await;
        assert!(poll_timer_callbacks().is_empty());

        runtime
            .notify(
                "agent:update",
                serde_json::json!({ "session_id": "session-1", "text": "after clear" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, blocks }
                if id == "agent-conversation"
                    && blocks.len() == 1
                    && blocks[0].kind == crate::plugin::TextPanelBlockKind::Agent
        ));
        drain_requests();
    }

    #[tokio::test]
    async fn bundled_agent_open_creates_and_focuses_panel_without_starting_a_session() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        runtime.execute_command("AgentOpen").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::CreateTextPanel { id, .. } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, blocks }
                if id == "agent-conversation"
                    && blocks.len() == 1
                    && blocks[0].text.starts_with("No messages yet.")
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::FocusPanel { id } if id == "agent-conversation"
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime.execute_command("AgentOpen").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::FocusPanel { id } if id == "agent-conversation"
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn bundled_agent_close_reopens_without_recreating_and_new_resets_the_session() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        drain_requests();

        runtime.execute_command("AgentClose").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPanelVisible { id, visible: false } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::FocusEditor
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime.execute_command("AgentOpen").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPanelVisible { id, visible: true } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::FocusPanel { id } if id == "agent-conversation"
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime.execute_command("AgentClose").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPanelVisible { id, visible: false } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::FocusEditor
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime.execute_command("AgentPrompt").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPanelVisible { id, visible: true } if id == "agent-conversation"
        ));
        let history_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage {
                plugin,
                key,
                request_id,
            } => {
                assert_eq!(plugin, "agent");
                assert_eq!(key, "prompt_history");
                request_id
            }
            _ => panic!("expected the prompt-history request after reopening"),
        };
        runtime
            .resolve_request(history_request_id, serde_json::json!({ "value": [] }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::OpenAgentComposer { id: 802, .. }
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime.execute_command("AgentNew").await.unwrap();
        let mut closed = false;
        let mut cleared = false;
        let mut reset_storage = false;
        let mut reset_draft = false;
        let mut requested_history = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::AgentCloseSession { session_id } => {
                    closed |= session_id == "session-1";
                }
                PluginRequest::UpdateTextPanel { id, blocks } => {
                    cleared |= id == "agent-conversation" && blocks.is_empty();
                }
                PluginRequest::SetPluginStorage { plugin, key, value } => {
                    reset_storage |=
                        plugin == "agent" && key == "transcript" && value == serde_json::json!("");
                }
                PluginRequest::ClearTextPanelComposer { id } => {
                    reset_draft |= id == "agent-conversation";
                }
                PluginRequest::GetPluginStorage { plugin, key, .. } => {
                    requested_history |= plugin == "agent" && key == "prompt_history";
                }
                PluginRequest::CreateTextPanel { .. } => {
                    panic!("new must reuse the existing conversation panel")
                }
                _ => {}
            }
        }
        assert!(closed);
        assert!(cleared);
        assert!(reset_storage);
        assert!(reset_draft);
        assert!(requested_history);

        runtime
            .notify(
                "agent:update",
                serde_json::json!({ "session_id": "session-1", "text": "late output" }),
            )
            .await
            .unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn host_accepts_explicit_agent_context_and_exposes_context_requests() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let source = r#"
            pub fn activate() {
                red::add_command("Ask", ask);
                red::add_command("Context", context);
            }
            fn ask() {
                red::execute("AgentPromptWithContext", "session-1", "explain", Json {
                    uri: "file:///workspace/main.rs",
                    text: "fn main() {}",
                });
            }
            fn context() { red::request("GetAgentContext", loaded); }
            fn loaded(result: Json) {}
        "#;
        let mut runtime = Runtime::new();
        runtime.load_plugin("test", source).await.unwrap();

        runtime.execute_command("Ask").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentPromptWithContext { session_id, text, uri, context }
                if session_id == "session-1"
                    && text == "explain"
                    && uri == "file:///workspace/main.rs"
                    && context == "fn main() {}"
        ));
        runtime.execute_command("Context").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::GetAgentContext { .. }
        ));
    }

    #[tokio::test]
    async fn bundled_agent_rotates_a_cancelled_session_before_the_next_prompt() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        drain_requests();

        runtime
            .notify("composer:submitted:802", serde_json::json!("first prompt"))
            .await
            .unwrap();
        drain_requests();
        runtime
            .notify(
                "agent:completed",
                serde_json::json!({ "session_id": "session-1", "stop_reason": "cancelled" }),
            )
            .await
            .unwrap();
        let mut closed = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            closed |= matches!(
                request,
                PluginRequest::AgentCloseSession { session_id } if session_id == "session-1"
            );
        }
        assert!(
            closed,
            "cancelled session must be closed so proposals are archived"
        );

        runtime
            .notify("composer:submitted:802", serde_json::json!("next prompt"))
            .await
            .unwrap();
        let mut config_request = None;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::GetConfig { request_id, key } = request {
                assert_eq!(key.as_deref(), Some("cwd"));
                config_request = Some(request_id);
            }
        }
        runtime
            .resolve_request(
                config_request.expect("next prompt must request a replacement session"),
                serde_json::json!({ "value": "/workspace" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-2" }),
            )
            .await
            .unwrap();
        let mut replacement_prompt = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            replacement_prompt |= matches!(
                request,
                PluginRequest::AgentPrompt { session_id, text }
                    if session_id == "session-2"
                        && text.contains("Previous conversation (the last turn was interrupted):")
                        && text.ends_with("Follow-up:\nnext prompt")
            );
        }
        assert!(replacement_prompt);
    }

    #[tokio::test]
    async fn bundled_agent_rotates_when_completion_wins_the_cancellation_race() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        drain_requests();

        runtime
            .notify("composer:submitted:802", serde_json::json!("first prompt"))
            .await
            .unwrap();
        drain_requests();
        runtime
            .notify(
                "agent:completed",
                serde_json::json!({ "session_id": "session-1", "stop_reason": "end_turn" }),
            )
            .await
            .unwrap();
        drain_requests();
        runtime
            .notify(
                "agent:cancelled",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();

        let mut closed = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            closed |= matches!(
                request,
                PluginRequest::AgentCloseSession { session_id } if session_id == "session-1"
            );
        }
        assert!(closed, "late cancellation must close the unusable session");

        runtime
            .notify("composer:submitted:802", serde_json::json!("next prompt"))
            .await
            .unwrap();
        let mut config_request = None;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::GetConfig { request_id, key } = request {
                assert_eq!(key.as_deref(), Some("cwd"));
                config_request = Some(request_id);
            }
        }
        runtime
            .resolve_request(
                config_request.expect("next prompt must request a replacement session"),
                serde_json::json!({ "value": "/workspace" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-2" }),
            )
            .await
            .unwrap();
        let mut replacement_prompt = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            replacement_prompt |= matches!(
                request,
                PluginRequest::AgentPrompt { session_id, text }
                    if session_id == "session-2"
                        && text.contains("Previous conversation (the last turn was interrupted):")
                        && text.ends_with("Follow-up:\nnext prompt")
            );
        }
        assert!(replacement_prompt);
    }

    #[tokio::test]
    async fn bundled_agent_rotates_when_cancellation_wins_the_completion_race() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        drain_requests();

        runtime
            .notify("composer:submitted:802", serde_json::json!("first prompt"))
            .await
            .unwrap();
        drain_requests();
        runtime
            .notify(
                "agent:update",
                serde_json::json!({ "session_id": "session-1", "text": "streamed output" }),
            )
            .await
            .unwrap();
        drain_requests();
        runtime
            .notify(
                "agent:cancelled",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            assert!(
                !matches!(request, PluginRequest::AgentCloseSession { .. }),
                "cancellation must not close an active stream before completion"
            );
        }
        runtime
            .notify(
                "agent:completed",
                serde_json::json!({ "session_id": "session-1", "stop_reason": "end_turn" }),
            )
            .await
            .unwrap();

        let mut closed = false;
        let mut transcript_saved = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            match request {
                PluginRequest::AgentCloseSession { session_id } => {
                    closed |= session_id == "session-1";
                }
                PluginRequest::SetPluginStorage { plugin, key, value } => {
                    transcript_saved |= plugin == "agent"
                        && key == "transcript"
                        && value
                            == serde_json::json!("You: first prompt\nAgent: streamed output\n");
                }
                _ => {}
            }
        }
        assert!(closed, "completed turn must close the cancelled session");
        assert!(transcript_saved, "completed stream must remain in history");

        runtime
            .notify("composer:submitted:802", serde_json::json!("next prompt"))
            .await
            .unwrap();
        let mut config_request = None;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::GetConfig { request_id, key } = request {
                assert_eq!(key.as_deref(), Some("cwd"));
                config_request = Some(request_id);
            }
        }
        runtime
            .resolve_request(
                config_request.expect("next prompt must request a replacement session"),
                serde_json::json!({ "value": "/workspace" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-2" }),
            )
            .await
            .unwrap();
        let mut replacement_prompt = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            replacement_prompt |= matches!(
                request,
                PluginRequest::AgentPrompt { session_id, text }
                    if session_id == "session-2"
                        && text.contains("Previous conversation (the last turn was interrupted):")
                        && text.ends_with("Follow-up:\nnext prompt")
            );
        }
        assert!(replacement_prompt);
    }

    #[tokio::test]
    async fn bundled_agent_rotates_a_cancelled_session_after_other_terminal_events() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;

        for (event, payload, transcript_suffix) in [
            (
                "agent:completed",
                serde_json::json!({ "session_id": "session-1", "stop_reason": "max_tokens" }),
                "System: Agent stopped: max_tokens\n",
            ),
            (
                "agent:error",
                serde_json::json!({ "session_id": "session-1", "message": "turn failed" }),
                "Error: turn failed\n",
            ),
        ] {
            drain_requests();
            let mut runtime = Runtime::new();
            runtime
                .load_plugin("agent", include_str!("../../plugins/agent.hk"))
                .await
                .unwrap();
            runtime
                .notify(
                    "agent:session_created",
                    serde_json::json!({ "session_id": "session-1" }),
                )
                .await
                .unwrap();
            drain_requests();
            runtime
                .notify("composer:submitted:802", serde_json::json!("first prompt"))
                .await
                .unwrap();
            drain_requests();
            runtime
                .notify(
                    "agent:update",
                    serde_json::json!({ "session_id": "session-1", "text": "streamed output" }),
                )
                .await
                .unwrap();
            drain_requests();
            runtime
                .notify(
                    "agent:cancelled",
                    serde_json::json!({ "session_id": "session-1" }),
                )
                .await
                .unwrap();
            drain_requests();
            runtime.notify(event, payload).await.unwrap();

            let mut closed = false;
            let mut transcript_saved = false;
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                match request {
                    PluginRequest::AgentCloseSession { session_id } => {
                        closed |= session_id == "session-1";
                    }
                    PluginRequest::SetPluginStorage { plugin, key, value } => {
                        transcript_saved |= plugin == "agent"
                            && key == "transcript"
                            && value.as_str().is_some_and(|text| {
                                text.starts_with("You: first prompt\nAgent: streamed output\n")
                                    && text.ends_with(transcript_suffix)
                            });
                    }
                    _ => {}
                }
            }
            assert!(closed, "{event} must close the cancelled session");
            assert!(transcript_saved, "{event} must preserve streamed output");
        }
    }

    #[tokio::test]
    async fn bundled_agent_start_keeps_the_previous_session_until_replacement_is_created() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        drain_requests();

        runtime.execute_command("AgentStart").await.unwrap();
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("cwd"));
                request_id
            }
            _ => panic!("expected current-directory request"),
        };
        runtime
            .resolve_request(request_id, serde_json::json!({ "value": "/workspace" }))
            .await
            .unwrap();

        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime.execute_command("AgentCancel").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentCancel { session_id } if session_id == "session-1"
        ));
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-2" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentCloseSession { session_id } if session_id == "session-1"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, .. } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "Agent session started"
        ));
    }

    #[tokio::test]
    async fn bundled_agent_retries_an_unsent_prompt_after_the_live_session_is_lost() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        drain_requests();

        runtime
            .notify(
                "composer:submitted:802",
                serde_json::json!("retry this exact prompt"),
            )
            .await
            .unwrap();
        let mut saw_prompt = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::AgentPrompt { session_id, text } = request {
                assert_eq!(session_id, "session-1");
                assert_eq!(text, "retry this exact prompt");
                saw_prompt = true;
            }
        }
        assert!(saw_prompt);

        runtime
            .notify(
                "agent:session_lost",
                serde_json::json!({
                    "session_id": "session-1",
                    "prompt": "retry this exact prompt",
                    "message": "no Codex session is running"
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelStatus { id, status: None }
                if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentArchiveSession { session_id } if session_id == "session-1"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message == "Codex app-server stopped; retrying the saved prompt"
        ));
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("cwd"));
                request_id
            }
            _ => panic!("expected a current-directory request for the saved prompt"),
        };
        runtime
            .resolve_request(request_id, serde_json::json!({ "value": "/workspace" }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));

        runtime
            .notify(
                "agent:error",
                serde_json::json!({ "message": "Codex app-server stopped" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelStatus { id, status: None }
                if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, .. } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, .. }
                if plugin == "agent" && key == "transcript"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message.contains("prompt is preserved")
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::OpenDynamicPicker { id: 803, .. }
        ));

        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-2" }),
            )
            .await
            .unwrap();
        let blocks = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdateTextPanel { id, blocks } => {
                assert_eq!(id, "agent-conversation");
                blocks
            }
            _ => panic!("expected the restored conversation panel"),
        };
        assert_eq!(
            blocks
                .iter()
                .filter(|block| block.text == "retry this exact prompt")
                .count(),
            1
        );
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "Agent session started"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentPrompt { session_id, text }
                if session_id == "session-2" && text == "retry this exact prompt"
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn bundled_agent_opens_setup_when_the_adapter_exits_during_lazy_start() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        runtime.execute_command("Agent").await.unwrap();
        let history_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage { request_id, .. } => request_id,
            _ => panic!("expected the agent prompt-history request"),
        };
        runtime
            .resolve_request(history_request_id, serde_json::json!({ "value": [] }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::OpenAgentComposer { id: 802, .. }
        ));
        runtime
            .notify(
                "composer:submitted:802",
                serde_json::json!("keep this prompt"),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, .. }
                if plugin == "agent" && key == "prompt_history"
        ));
        let cwd_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("cwd"));
                request_id
            }
            _ => panic!("expected the lazy-start current-directory request"),
        };
        runtime
            .resolve_request(cwd_request_id, serde_json::json!({ "value": "/workspace" }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));

        runtime
            .notify(
                "agent:session_lost",
                serde_json::json!({ "message": "Codex app-server stopped" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelStatus { id, status: None }
                if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelStatus { id, status: None }
                if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, .. } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, .. }
                if plugin == "agent" && key == "transcript"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message.contains("prompt is preserved")
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::OpenDynamicPicker { id: 803, .. }
        ));

        runtime.execute_command("Agent").await.unwrap();
        let history_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage { request_id, .. } => request_id,
            _ => panic!("expected the saved-prompt history request"),
        };
        runtime
            .resolve_request(
                history_request_id,
                serde_json::json!({ "value": ["keep this prompt"] }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::OpenAgentComposer { query, .. } if query == "keep this prompt"
        ));
    }

    #[tokio::test]
    async fn bundled_agent_review_can_accept_an_archived_proposal_before_starting_a_session() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        runtime.execute_command("AgentReview").await.unwrap();

        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::OpenWorkspace { id, .. } if id == "agent-review"
        ));
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::AgentProposals {
                session_id,
                request_id,
            } => {
                assert!(session_id.is_empty());
                request_id
            }
            _ => panic!("expected archived proposal request"),
        };
        runtime
            .resolve_request(
                request_id,
                serde_json::json!({
                    "files": [{
                        "session_id": "archived-session",
                        "path": "/workspace/recovered.rs",
                        "conflict": false,
                        "hunks": [{
                            "id": "hunk-1",
                            "old_start": 0,
                            "old_end": 4,
                            "old_text": "base",
                            "new_text": "agent"
                        }]
                    }]
                }),
            )
            .await
            .unwrap();
        let model = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdateWorkspace { id, model } => {
                assert_eq!(id, "agent-review");
                model
            }
            _ => panic!("expected archived proposal workspace update"),
        };
        assert_eq!(model.rows[0].data["session_id"], "archived-session");
        assert_eq!(model.rows[1].data["session_id"], "archived-session");

        runtime
            .notify(
                "workspace:event:agent-review",
                serde_json::json!({
                    "action": "A",
                    "row": {
                        "data": {
                            "session_id": "archived-session",
                            "path": "/workspace/recovered.rs",
                            "hunk_id": ""
                        }
                    }
                }),
            )
            .await
            .unwrap();

        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentAcceptProposal {
                session_id,
                path,
                hunk_id: None,
            } if session_id == "archived-session" && path == Path::new("/workspace/recovered.rs")
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn bundled_agent_review_surfaces_a_safe_proposal_read_error() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        runtime.execute_command("AgentReview").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::OpenWorkspace { id, .. } if id == "agent-review"
        ));
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::AgentProposals { request_id, .. } => request_id,
            _ => panic!("expected proposal review request"),
        };

        runtime
            .resolve_request(
                request_id,
                serde_json::json!({
                    "files": [],
                    "error": "Unable to review agent proposals safely; pending changes were left intact"
                }),
            )
            .await
            .unwrap();

        let model = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdateWorkspace { id, model } => {
                assert_eq!(id, "agent-review");
                model
            }
            _ => panic!("expected proposal workspace update"),
        };
        assert_eq!(model.rows.len(), 1);
        assert_eq!(model.rows[0].id, "error");
        assert!(!model.rows[0].selectable);
        assert_eq!(
            model.rows[0].segments[0].text,
            "Unable to review agent proposals safely; pending changes were left intact"
        );
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn bundled_agent_ignores_late_events_from_a_replaced_session() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-1" }),
            )
            .await
            .unwrap();
        drain_requests();
        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-2" }),
            )
            .await
            .unwrap();
        drain_requests();

        for (event, payload) in [
            (
                "agent:update",
                serde_json::json!({ "session_id": "session-1", "text": "stale output" }),
            ),
            (
                "agent:completed",
                serde_json::json!({ "session_id": "session-1", "stop_reason": "end_turn" }),
            ),
            (
                "agent:cancelled",
                serde_json::json!({ "session_id": "session-1" }),
            ),
            (
                "agent:error",
                serde_json::json!({ "session_id": "session-1", "message": "stale error" }),
            ),
            (
                "agent:proposals_changed",
                serde_json::json!({ "session_id": "session-1" }),
            ),
            (
                "agent:permission_requested",
                serde_json::json!({
                    "session_id": "session-1",
                    "request_id": "stale-permission",
                    "options": [{"option_id": "allow", "name": "Allow", "kind": "allow_once"}]
                }),
            ),
        ] {
            runtime.notify(event, payload).await.unwrap();
        }

        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn composer_submission_is_delivered_only_to_the_plugin_that_opened_it() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "owner",
                r#"
                    pub fn activate() {
                        red::add_command("OpenComposer", open);
                        red::on("composer:submitted:919", submitted);
                    }
                    fn open() {
                        red::execute("OpenAgentComposer", "Private prompt", 919, "draft", ["recent"]);
                    }
                    fn submitted(prompt: Json) {
                        red::execute("Print", "owner:" + red::string(prompt, ""));
                    }
                "#,
            )
            .await
            .unwrap();
        runtime
            .load_plugin(
                "observer",
                r#"
                    pub fn activate() { red::on("composer:submitted:919", submitted); }
                    fn submitted(prompt: Json) {
                        red::execute("Print", "observer:" + red::string(prompt, ""));
                    }
                "#,
            )
            .await
            .unwrap();

        runtime.execute_command("OpenComposer").await.unwrap();
        let owner = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenAgentComposer {
                owner,
                title,
                id,
                query,
                history,
            } => {
                assert_eq!(title.as_deref(), Some("Private prompt"));
                assert_eq!(id, 919);
                assert_eq!(query, "draft");
                assert_eq!(history, ["recent"]);
                owner
            }
            _ => panic!("expected agent composer request"),
        };
        assert_eq!(owner, "owner");

        let failures = runtime.notify_plugin_isolated(
            &owner,
            "composer:submitted:919",
            serde_json::json!("private prompt\n  with whitespace  "),
        );

        assert!(failures.is_empty());
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message == "owner:private prompt\n  with whitespace  "
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn bundled_agent_plugin_bounds_history_preserves_text_and_ignores_picker_events() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        runtime.execute_command("Agent").await.unwrap();
        let history_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage {
                plugin,
                key,
                request_id,
            } => {
                assert_eq!(plugin, "agent");
                assert_eq!(key, "prompt_history");
                request_id
            }
            _ => panic!("expected agent prompt-history request"),
        };
        let expected_history = (0..50)
            .map(|index| format!("  prompt {index}\n    detail {index}  "))
            .collect::<Vec<_>>();
        let mut stored_history = (0..54)
            .map(|index| format!("  prompt {index}\n    detail {index}  "))
            .collect::<Vec<_>>();
        let duplicate = stored_history[0].clone();
        stored_history.insert(1, duplicate);
        stored_history.insert(2, " \n \t ".to_string());
        runtime
            .resolve_request(
                history_request_id,
                serde_json::json!({ "value": stored_history }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::OpenAgentComposer {
                owner,
                id: 802,
                title,
                query,
                history,
            } if owner == "agent"
                && title.as_deref() == Some("Agent prompt")
                && query.is_empty()
                && history == expected_history
        ));

        for (event, payload) in [
            ("picker:query:802", serde_json::json!("do not round-trip")),
            (
                "picker:action:802",
                serde_json::json!({ "action": "history_back" }),
            ),
            ("picker:selected:802", serde_json::json!({ "id": "submit" })),
            ("composer:cancelled:802", serde_json::json!({})),
        ] {
            runtime.notify(event, payload).await.unwrap();
            assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        }

        let submitted = expected_history[10].clone();
        runtime
            .notify(
                "composer:submitted:802",
                serde_json::json!(submitted.clone()),
            )
            .await
            .unwrap();
        let mut expected_saved = vec![submitted.clone()];
        expected_saved.extend(
            expected_history
                .iter()
                .filter(|entry| entry.as_str() != submitted)
                .take(49)
                .cloned(),
        );
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, value }
                if plugin == "agent"
                    && key == "prompt_history"
                    && value == serde_json::json!(expected_saved)
        ));
    }

    #[tokio::test]
    async fn bundled_agent_plugin_lazily_starts_preserves_prompt_and_announces_proposals() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        runtime.execute_command("Agent").await.unwrap();
        let history_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage {
                plugin,
                key,
                request_id,
            } => {
                assert_eq!(plugin, "agent");
                assert_eq!(key, "prompt_history");
                request_id
            }
            _ => panic!("expected agent prompt-history request"),
        };
        runtime
            .resolve_request(history_request_id, serde_json::json!({ "value": [] }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::OpenAgentComposer {
                owner,
                id: 802,
                title,
                query,
                history,
            } if owner == "agent"
                && title.as_deref() == Some("Agent prompt")
                && query.is_empty()
                && history.is_empty()
        ));

        runtime
            .notify(
                "composer:submitted:802",
                serde_json::json!("inspect unsaved changes"),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, value }
                if plugin == "agent"
                    && key == "prompt_history"
                    && value == serde_json::json!(["inspect unsaved changes"])
        ));
        let cwd_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("cwd"));
                request_id
            }
            _ => panic!("expected lazy agent current-directory request"),
        };
        runtime
            .resolve_request(cwd_request_id, serde_json::json!({ "value": "/workspace" }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));

        runtime
            .notify(
                "agent:error",
                serde_json::json!({ "message": "Codex login required" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelStatus { id, status: None }
                if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, .. } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, .. }
                if plugin == "agent" && key == "transcript"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message.contains("prompt is preserved")
        ));
        let items = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenDynamicPicker {
                title,
                id: 803,
                items,
                ..
            } => {
                assert_eq!(title.as_deref(), Some("Retry Codex"));
                items
            }
            _ => panic!("expected agent setup picker"),
        };
        assert_eq!(
            items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["retry"]
        );
        assert_eq!(
            items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            ["Retry the saved prompt"]
        );

        runtime
            .notify("picker:cancelled:803", serde_json::json!({}))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message == "Agent prompt saved. Press Space A when ready to retry"
        ));
        runtime.execute_command("Agent").await.unwrap();
        let history_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage {
                plugin,
                key,
                request_id,
            } => {
                assert_eq!(plugin, "agent");
                assert_eq!(key, "prompt_history");
                request_id
            }
            _ => panic!("expected saved-prompt history request"),
        };
        runtime
            .resolve_request(
                history_request_id,
                serde_json::json!({ "value": ["inspect unsaved changes"] }),
            )
            .await
            .unwrap();
        let (owner, title, query, history) = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenAgentComposer {
                owner,
                title,
                id: 802,
                query,
                history,
            } => (owner, title, query, history),
            _ => panic!("expected saved agent composer"),
        };
        assert_eq!(owner, "agent");
        assert_eq!(title.as_deref(), Some("Agent prompt"));
        assert_eq!(query, "inspect unsaved changes");
        assert_eq!(history, ["inspect unsaved changes"]);

        runtime
            .notify("picker:selected:803", serde_json::json!({ "id": "retry" }))
            .await
            .unwrap();
        let cwd_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("cwd"));
                request_id
            }
            _ => panic!("expected agent retry current-directory request"),
        };
        runtime
            .resolve_request(cwd_request_id, serde_json::json!({ "value": "/workspace" }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));

        runtime
            .notify(
                "agent:session_created",
                serde_json::json!({ "session_id": "session-lazy" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::CreateTextPanel { id, .. } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, .. } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "Agent session started"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, .. } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::FocusPanel { id } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelStatus { id, status: Some(status) }
                if id == "agent-conversation" && status.busy
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Refresh)
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, .. }
                if plugin == "agent" && key == "transcript"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentPrompt { session_id, text }
                if session_id == "session-lazy" && text == "inspect unsaved changes"
        ));

        runtime
            .notify(
                "agent:proposals_changed",
                serde_json::json!({ "session_id": "session-lazy" }),
            )
            .await
            .unwrap();
        let proposals_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::AgentProposals {
                session_id,
                request_id,
            } => {
                assert_eq!(session_id, "session-lazy");
                request_id
            }
            _ => panic!("expected pending-proposals request"),
        };
        runtime
            .resolve_request(
                proposals_request_id,
                serde_json::json!({
                    "files": [
                        { "hunks": [{}, {}] },
                        { "hunks": [{}] },
                    ]
                }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message == "Agent changes ready: 2 files, 3 hunks. Use :AgentReview to review before applying"
        ));
    }

    #[tokio::test]
    async fn bundled_agent_plugin_setup_actions_dispatch_and_cancel_keeps_prompt() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        runtime
            .notify("picker:selected:803", serde_json::json!({ "id": "retry" }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::GetConfig { key, .. } if key.as_deref() == Some("cwd")
        ));

        runtime
            .notify("picker:cancelled:803", serde_json::json!({}))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message == "Agent prompt saved. Press Space A when ready to retry"
        ));
    }

    #[tokio::test]
    async fn bundled_agent_plugin_legacy_start_failure_opens_setup() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        runtime.execute_command("AgentStart").await.unwrap();
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("cwd"));
                request_id
            }
            _ => panic!("expected agent current-directory request"),
        };
        runtime
            .resolve_request(request_id, serde_json::json!({ "value": "/workspace" }))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentNewSession { cwd } if cwd == Path::new("/workspace")
        ));
        runtime
            .notify(
                "agent:error",
                serde_json::json!({ "message": "Codex login required" }),
            )
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetTextPanelStatus { id, status: None }
                if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::UpdateTextPanel { id, .. } if id == "agent-conversation"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::SetPluginStorage { plugin, key, .. }
                if plugin == "agent" && key == "transcript"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message.contains("prompt is preserved")
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::OpenDynamicPicker { id: 803, .. }
        ));
    }

    #[tokio::test]
    async fn bundled_agent_plugin_restores_markdown_tables_and_blank_lines() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("agent", include_str!("../../plugins/agent.hk"))
            .await
            .unwrap();

        let markdown = "# Accepted arguments\n\n| Argument | Meaning |\n|---|---|\n| `--root` | Set the root |\n\nTrailing paragraph.";
        runtime
            .notify(
                "agent:transcript_restored",
                serde_json::json!({
                    "transcript": format!("You: list the arguments\nAgent: {markdown}\nSystem: Agent stopped: end_turn\n")
                }),
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdateTextPanel { id, blocks } => {
                assert_eq!(id, "agent-conversation");
                assert_eq!(blocks.len(), 2);
                assert_eq!(blocks[0].kind, crate::plugin::TextPanelBlockKind::User);
                assert_eq!(blocks[0].text, "list the arguments");
                assert_eq!(blocks[1].kind, crate::plugin::TextPanelBlockKind::Agent);
                assert_eq!(
                    blocks[1].format,
                    crate::plugin::TextPanelBlockFormat::Markdown
                );
                assert_eq!(blocks[1].text, markdown);
            }
            _ => panic!("expected restored text panel update"),
        }
    }

    #[tokio::test]
    async fn pinned_example_plugin_typechecks_and_activates() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin_at(
                "example",
                "examples/example-plugin/index.hk",
                include_str!("../../examples/example-plugin/index.hk"),
            )
            .await
            .unwrap();
        runtime.execute_command("ExampleCommand").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message))
                if message == "Hello from the example Husk plugin!"
        ));
    }

    #[test]
    fn plugin_source_validation_keeps_host_api_and_semantic_diagnostics() {
        let host_error = validate_plugin_source(
            "invalid-api",
            "plugins/invalid-api.hk",
            r#"pub fn activate() { red::execute("RemovedAction"); }"#,
        )
        .unwrap_err()
        .to_string();
        assert!(host_error.contains("HUSK-A0001"));
        assert!(host_error.contains("RemovedAction"));

        let semantic_error = validate_plugin_source(
            "invalid-type",
            "plugins/invalid-type.hk",
            r#"pub fn activate() { missing_name(); }"#,
        )
        .unwrap_err()
        .to_string();
        assert!(semantic_error.contains("HUSK-T0001"));
        assert!(semantic_error.contains("invalid-type"));

        assert!(validate_plugin_source(
            "invalid-parse",
            "plugins/invalid-parse.hk",
            "fn activate( {"
        )
        .is_ok());
    }

    #[tokio::test]
    async fn transactional_reload_uses_explicit_state_migration_hooks() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "stateful",
                r#"
                    pub fn activate() {
                        red::state_set("value", "preserved");
                    }
                    fn state_export() -> Json { return red::state("value"); }
                "#,
            )
            .await
            .unwrap();
        runtime
            .load_plugin(
                "stateful",
                r#"
                    pub fn activate() { red::add_command("Migrated", show); }
                    fn state_import(saved: Json) { red::state_set("value", saved); }
                    fn show() { red::execute("Print", red::string(red::state("value"), "missing")); }
                "#,
            )
            .await
            .unwrap();

        runtime.execute_command("Migrated").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "preserved"
        ));
    }

    #[tokio::test]
    async fn successful_reload_commits_old_teardown_before_replacement_activation_and_import() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "reload-order",
                r#"
                    pub fn activate() { red::state_set("value", "preserved"); }
                    fn state_export() -> Json { return red::state("value"); }
                    fn deactivate() { red::execute("ClosePanel", "shared-panel"); }
                "#,
            )
            .await
            .unwrap();

        runtime
            .load_plugin(
                "reload-order",
                r#"
                    pub fn activate() {
                        red::execute("CreatePanel", "shared-panel", PanelConfig {
                            side: "right",
                            width: 32,
                            title: "Replacement",
                        });
                    }
                    fn state_import(saved: Json) {
                        red::execute("Print", "import:" + red::string(saved, "missing"));
                    }
                "#,
            )
            .await
            .unwrap();

        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::ClosePanel { id } if id == "shared-panel"
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::CreatePanel { id, config }
                if id == "shared-panel" && config.title.as_deref() == Some("Replacement")
        ));
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "import:preserved"
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn failed_teardown_discards_replacement_effects_and_keeps_the_previous_plugin() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "reload-teardown-error",
                r#"
                    pub fn activate() {
                        red::state_set("value", "stable");
                        red::add_command("Stable", run);
                    }
                    fn run() { red::execute("Print", red::string(red::state("value"), "missing")); }
                    fn deactivate() {
                        red::state_set("value", "teardown-mutated");
                        red::execute("ClosePanel", "shared-panel");
                        red::execute("Print", 1 / 0);
                    }
                "#,
            )
            .await
            .unwrap();

        let error = runtime
            .load_plugin(
                "reload-teardown-error",
                r#"
                    pub fn activate() {
                        red::execute("CreatePanel", "shared-panel", PanelConfig {
                            side: "right",
                            width: 32,
                        });
                    }
                "#,
            )
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("integer division by zero"));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        runtime.execute_command("Stable").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "stable"
        ));
    }

    #[tokio::test]
    async fn failed_export_discards_staged_effects_and_keeps_live_plugin_state() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "reload-export-error",
                r#"
                    pub fn activate() {
                        red::state_set("value", "stable");
                        red::add_command("Stable", run);
                    }
                    fn run() { red::execute("Print", red::string(red::state("value"), "missing")); }
                    fn state_export() -> Json {
                        red::state_set("value", "export-mutated");
                        red::execute("ClosePanel", "shared-panel");
                        red::execute("Print", 1 / 0);
                        return red::state("value");
                    }
                "#,
            )
            .await
            .unwrap();

        let error = runtime
            .load_plugin("reload-export-error", "pub fn activate() {}")
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("integer division by zero"));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        runtime.execute_command("Stable").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "stable"
        ));
    }

    #[tokio::test]
    async fn failed_initial_activation_discards_all_staged_host_effects() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let timeout_count = PENDING_TIMEOUTS.lock().unwrap().len();
        let mut runtime = Runtime::new();

        let error = runtime
            .load_plugin(
                "initial-activation-error",
                r#"
                    pub fn activate() {
                        red::add_command("Leaked", run);
                        red::execute("Print", "must not leak");
                        red::request("GetConfig", loaded, "cwd");
                        red::execute("SetTimeout", 0);
                        red::execute("Print", 1 / 0);
                    }
                    fn run() {}
                    fn loaded(event: Json) {}
                "#,
            )
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("integer division by zero"));
        assert_eq!(runtime.command_plugin("Leaked"), None);
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        assert_eq!(PENDING_TIMEOUTS.lock().unwrap().len(), timeout_count);
    }

    #[tokio::test]
    async fn failed_reload_discards_staged_host_effects_and_keeps_previous_command() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let timeout_count = PENDING_TIMEOUTS.lock().unwrap().len();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "transactional",
                r#"
                    pub fn activate() { red::add_command("Stable", run); }
                    fn run() { red::execute("Print", "stable"); }
                "#,
            )
            .await
            .unwrap();

        let error = runtime
            .load_plugin(
                "transactional",
                r#"
                    pub fn activate() {
                        red::execute("Print", "must not leak");
                        red::request("GetConfig", loaded, "cwd");
                        red::execute("SetTimeout", 0);
                        red::execute("Print", 1 / 0);
                    }
                    fn loaded(event: Json) {}
                "#,
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("integer division by zero"));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        assert_eq!(PENDING_TIMEOUTS.lock().unwrap().len(), timeout_count);

        runtime.execute_command("Stable").await.unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::Action(Action::Print(message)) if message == "stable"
        ));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn failed_reload_cannot_kill_the_live_plugins_process() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new_with_permissions(HashMap::from([(
            "transactional-process".to_string(),
            PluginPermissions {
                process: vec!["/bin/sleep".to_string()],
            },
        )]));
        runtime
            .load_plugin(
                "transactional-process",
                r#"
                    pub fn activate() { red::add_command("Start", start); }
                    fn start() {
                        let id = red::execute("SpawnProcess", Process {
                            command: "/bin/sleep",
                            args: ["30"],
                        });
                        red::state_set("process_id", id);
                    }
                    fn deactivate() {
                        red::execute("KillProcess", red::state("process_id"));
                    }
                "#,
            )
            .await
            .unwrap();
        runtime.execute_command("Start").await.unwrap();
        assert_eq!(
            runtime
                .inner
                .lock()
                .unwrap()
                .host
                .process_manager
                .active_process_count("transactional-process"),
            1
        );

        let error = runtime
            .load_plugin("transactional-process", "pub fn activate() {}")
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("KillProcess is not allowed"));
        assert_eq!(
            runtime
                .inner
                .lock()
                .unwrap()
                .host
                .process_manager
                .active_process_count("transactional-process"),
            1
        );
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn unloading_a_failing_plugin_teardown_closes_its_session_and_kills_its_process() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let mut runtime = Runtime::new_with_permissions(HashMap::from([(
            "quarantined-process".to_string(),
            PluginPermissions {
                process: vec!["/bin/sleep".to_string()],
            },
        )]));
        runtime
            .load_plugin(
                "quarantined-process",
                r#"
                    pub fn activate() { red::add_command("Start", start); }
                    fn start() {
                        red::execute("SpawnProcess", Process {
                            command: "/bin/sleep",
                            args: ["30"],
                        });
                    }
                    fn deactivate() {
                        red::execute("AgentCloseSession", "session-1");
                        red::execute("Print", 1 / 0);
                    }
                "#,
            )
            .await
            .unwrap();
        runtime.execute_command("Start").await.unwrap();
        assert_eq!(
            runtime
                .inner
                .lock()
                .unwrap()
                .host
                .process_manager
                .active_process_count("quarantined-process"),
            1
        );

        let error = runtime
            .unload_plugin("quarantined-process")
            .unwrap_err()
            .to_string();

        assert!(error.contains("integer division by zero"));
        assert_eq!(runtime.command_plugin("Start"), None);
        assert_eq!(
            runtime
                .inner
                .lock()
                .unwrap()
                .host
                .process_manager
                .active_process_count("quarantined-process"),
            0
        );
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::AgentCloseSession { session_id } if session_id == "session-1"
        ));
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn husk_can_request_correlated_buffer_text() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let source = r#"
            pub fn activate() {
                red::add_command("Read", read);
            }

            fn loaded(event: Json) {}

            fn read() {
                red::request("GetBufferText", loaded, 2, 7);
            }
        "#;
        let mut runtime = Runtime::new();

        runtime.load_plugin("test", source).await.unwrap();
        runtime.execute_command("Read").await.unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetBufferText {
                request_id,
                start_line,
                end_line,
            } => {
                assert!(request_id.get() > 0);
                assert_eq!(start_line, Some(2));
                assert_eq!(end_line, Some(7));
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

        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::EditorInfo(request_id) => request_id,
            _ => panic!("unexpected plugin request"),
        };

        runtime
            .resolve_request(
                request_id,
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
    async fn indent_guides_reads_the_latest_viewport_snapshot() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime.set_snapshot(
            "editor_info",
            sample_indent_editor_info(
                Color::Rgb {
                    r: 40,
                    g: 41,
                    b: 42,
                },
                Color::Rgb {
                    r: 80,
                    g: 81,
                    b: 82,
                },
            ),
        );
        runtime.set_snapshot("viewport_layout", sample_indent_layout());
        runtime
            .load_plugin(
                "indent_guides",
                include_str!("../../plugins/indent_guides.hk"),
            )
            .await
            .unwrap();

        assert!(matches!(
            ACTION_DISPATCHER.try_recv_request(),
            Some(PluginRequest::SetDecorations { .. })
        ));

        let mut next_layout = sample_indent_layout();
        next_layout["cursor"]["y"] = serde_json::json!(3);
        runtime.set_snapshot("viewport_layout", next_layout);
        runtime
            .notify("buffer:changed", serde_json::json!({}))
            .await
            .unwrap();

        assert!(matches!(
            ACTION_DISPATCHER.try_recv_request(),
            Some(PluginRequest::SetDecorations { .. })
        ));
    }

    #[tokio::test]
    async fn indent_guides_renders_decorations_from_viewport_layout_response() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime.set_snapshot(
            "editor_info",
            sample_indent_editor_info(
                Color::Rgb {
                    r: 40,
                    g: 41,
                    b: 42,
                },
                Color::Rgb {
                    r: 80,
                    g: 81,
                    b: 82,
                },
            ),
        );
        runtime.set_snapshot("viewport_layout", sample_indent_layout());
        runtime
            .load_plugin(
                "indent_guides",
                include_str!("../../plugins/indent_guides.hk"),
            )
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
    async fn indent_guides_handles_non_tabstop_indentation() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime.set_snapshot(
            "editor_info",
            sample_indent_editor_info(
                Color::Rgb {
                    r: 40,
                    g: 41,
                    b: 42,
                },
                Color::Rgb {
                    r: 80,
                    g: 81,
                    b: 82,
                },
            ),
        );
        runtime.set_snapshot("viewport_layout", non_tabstop_indent_layout());
        runtime
            .load_plugin(
                "indent_guides",
                include_str!("../../plugins/indent_guides.hk"),
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SetDecorations { decorations, .. } => {
                let active = decorations
                    .iter()
                    .find(|decoration| decoration.priority == 1024)
                    .unwrap();
                assert_eq!(active.line, 1);
                assert_eq!(active.column, 32);
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn indent_guides_rebuild_theme_styles_without_layout_changes() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let original = Color::Rgb {
            r: 40,
            g: 41,
            b: 42,
        };
        let original_active = Color::Rgb {
            r: 80,
            g: 81,
            b: 82,
        };
        let updated = Color::Rgb {
            r: 90,
            g: 91,
            b: 92,
        };
        let updated_active = Color::Rgb {
            r: 120,
            g: 121,
            b: 122,
        };
        let mut runtime = Runtime::new();
        runtime.set_snapshot(
            "editor_info",
            sample_indent_editor_info(original, original_active),
        );
        runtime.set_snapshot("viewport_layout", sample_indent_layout());
        runtime
            .load_plugin(
                "indent_guides",
                include_str!("../../plugins/indent_guides.hk"),
            )
            .await
            .unwrap();

        let _ = ACTION_DISPATCHER.recv_request();
        runtime.set_snapshot(
            "editor_info",
            sample_indent_editor_info(updated, updated_active),
        );
        runtime
            .notify("theme:changed", serde_json::json!({ "name": "updated" }))
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SetDecorations { decorations, .. } => {
                assert_eq!(decorations[0].style.fg, Some(updated));
                assert_eq!(
                    decorations
                        .iter()
                        .find(|decoration| decoration.priority == 1024)
                        .unwrap()
                        .style
                        .fg,
                    Some(updated_active)
                );
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn indent_guides_clears_decorations_on_deactivate() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime.set_snapshot(
            "editor_info",
            sample_indent_editor_info(
                Color::Rgb {
                    r: 40,
                    g: 41,
                    b: 42,
                },
                Color::Rgb {
                    r: 80,
                    g: 81,
                    b: 82,
                },
            ),
        );
        runtime.set_snapshot("viewport_layout", sample_indent_layout());
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
        runtime.set_snapshot(
            "editor_info",
            serde_json::json!({
                "theme": {
                    "colors": {
                        "editorInlayHint.typeForeground": "#c8c8c8",
                        "editor.background": "#0a141e",
                    },
                    "gutter_style": { "fg": null },
                }
            }),
        );
        runtime.set_snapshot("viewport_layout", sample_indent_layout());
        runtime
            .load_plugin("inlay_hints", include_str!("../../plugins/inlay_hints.hk"))
            .await
            .unwrap();

        let _config_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key, None);
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        let hints_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::InlayHints { request_id, range } => {
                let range = range.unwrap();
                assert_eq!(range.start.line, 0);
                assert_eq!(range.end.line, 5);
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());
        runtime
            .resolve_request(
                hints_request_id,
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
    async fn inlay_hints_ignore_stale_layout_and_render_configured_parameter_hints() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime.set_snapshot(
            "editor_info",
            serde_json::json!({
                "theme": {
                    "colors": {
                        "editorInlayHint.typeForeground": "#c8c8c8",
                        "editor.background": "#0a141e"
                    },
                    "gutter_style": { "fg": null }
                }
            }),
        );
        runtime.set_snapshot("viewport_layout", sample_indent_layout());
        runtime
            .load_plugin("inlay_hints", include_str!("../../plugins/inlay_hints.hk"))
            .await
            .unwrap();
        let config_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, .. } => request_id,
            _ => panic!("unexpected plugin request"),
        };
        let _initial_hints_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::InlayHints { request_id, .. } => request_id,
            _ => panic!("unexpected plugin request"),
        };

        runtime
            .resolve_request(
                config_request_id,
                serde_json::json!({
                    "value": {
                        "plugin_config": {
                            "inlay_hints": { "parameter_hints": true }
                        }
                    }
                }),
            )
            .await
            .unwrap();
        let hints_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::InlayHints { request_id, .. } => request_id,
            _ => panic!("unexpected plugin request"),
        };

        runtime
            .resolve_request(
                hints_request_id,
                serde_json::json!({
                    "ok": true,
                    "hints": [
                        {
                            "kind": 1,
                            "position": { "line": 1, "character": 8 },
                            "label": ": String"
                        },
                        {
                            "kind": 2,
                            "position": { "line": 1, "character": 1 },
                            "label": "arg:"
                        },
                        {
                            "kind": 1,
                            "position": { "line": 1, "character": 3 },
                            "label": ": Number"
                        }
                    ]
                }),
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::SetDecorations { decorations, .. } => {
                assert_eq!(decorations.len(), 1);
                assert_eq!(decorations[0].text, " <- (arg) => Number,String");
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
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::EditorInfo(request_id) => request_id,
            _ => panic!("unexpected plugin request"),
        };
        runtime
            .resolve_request(
                request_id,
                serde_json::json!({ "size": [80, 24], "theme": { "ui_style": {} } }),
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
                    "lsp_client": { "name": "rust_analyzer" },
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
    async fn fidget_cancels_animation_and_completion_timers() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let timeout_count = PENDING_TIMEOUTS.lock().unwrap().len();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin("fidget", include_str!("../../plugins/fidget.hk"))
            .await
            .unwrap();
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::EditorInfo(request_id) => request_id,
            _ => panic!("unexpected plugin request"),
        };
        runtime
            .resolve_request(
                request_id,
                serde_json::json!({ "size": [80, 24], "theme": { "ui_style": {} } }),
            )
            .await
            .unwrap();
        drain_requests();

        runtime
            .notify(
                "lsp:progress",
                serde_json::json!({
                    "token": "index",
                    "value": { "kind": "begin", "title": "Indexing" }
                }),
            )
            .await
            .unwrap();
        assert_eq!(PENDING_TIMEOUTS.lock().unwrap().len(), timeout_count + 1);

        runtime
            .notify(
                "lsp:progress",
                serde_json::json!({
                    "token": "index",
                    "value": { "kind": "end", "message": "Done" }
                }),
            )
            .await
            .unwrap();
        assert_eq!(PENDING_TIMEOUTS.lock().unwrap().len(), timeout_count + 1);

        runtime.deactivate_all().await.unwrap();

        assert_eq!(PENDING_TIMEOUTS.lock().unwrap().len(), timeout_count);
    }

    #[tokio::test]
    async fn bundled_plugin_deactivation_cancels_pending_refresh_timers() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        for (name, source, event, payload) in [
            (
                "inlay_hints",
                include_str!("../../plugins/inlay_hints.hk"),
                "buffer:changed",
                serde_json::json!({}),
            ),
            (
                "barbecue",
                include_str!("../../plugins/barbecue.hk"),
                "buffer:changed",
                serde_json::json!({}),
            ),
            (
                "project_search",
                include_str!("../../plugins/project_search.hk"),
                "picker:query:301",
                serde_json::json!("needle"),
            ),
        ] {
            let timeout_count = PENDING_TIMEOUTS.lock().unwrap().len();
            let mut runtime = Runtime::new();
            runtime.set_snapshot("viewport_layout", sample_indent_layout());
            runtime.set_snapshot("windows", serde_json::json!({ "windows": [] }));
            runtime.set_snapshot(
                "editor_info",
                serde_json::json!({
                    "size": [80, 24],
                    "theme": { "ui_style": {}, "colors": {}, "gutter_style": {} }
                }),
            );
            runtime.load_plugin(name, source).await.unwrap();
            drain_requests();

            runtime.notify(event, payload).await.unwrap();
            assert_eq!(PENDING_TIMEOUTS.lock().unwrap().len(), timeout_count + 1);

            runtime.deactivate_all().await.unwrap();
            assert_eq!(PENDING_TIMEOUTS.lock().unwrap().len(), timeout_count);
            drain_requests();
        }
    }

    #[tokio::test]
    async fn project_search_cancels_pending_debounce_when_picker_closes() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let timeout_count = PENDING_TIMEOUTS.lock().unwrap().len();
        let mut runtime = Runtime::new();
        runtime
            .load_plugin(
                "project_search",
                include_str!("../../plugins/project_search.hk"),
            )
            .await
            .unwrap();

        runtime
            .notify("picker:query:301", serde_json::json!("needle"))
            .await
            .unwrap();
        assert_eq!(PENDING_TIMEOUTS.lock().unwrap().len(), timeout_count + 1);

        runtime
            .notify("picker:cancelled:301", serde_json::Value::Null)
            .await
            .unwrap();

        assert_eq!(PENDING_TIMEOUTS.lock().unwrap().len(), timeout_count);
    }

    #[tokio::test]
    async fn barbecue_renders_breadcrumbs_and_opens_symbol_action() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime.set_snapshot(
            "windows",
            serde_json::json!({
                "windows": [
                    {
                        "window_id": 7,
                        "buffer_index": 2,
                        "buffer_path": "/repo/plugins/example.rs",
                        "revision": 4,
                        "cursor": { "x": 1, "y": 6 },
                        "lsp_position": { "line": 6, "character": 1 },
                    },
                    {
                        "window_id": 8,
                        "buffer_index": 2,
                        "buffer_path": "/repo/plugins/example.rs",
                        "revision": 4,
                        "cursor": { "x": 1, "y": 6 },
                        "lsp_position": { "line": 6, "character": 1 },
                    }
                ]
            }),
        );
        runtime.set_snapshot(
            "editor_info",
            serde_json::json!({
                "theme": {
                    "style": {
                        "fg": null,
                        "bg": "#111111",
                        "bold": false,
                        "italic": false
                    }
                }
            }),
        );
        runtime
            .load_plugin("barbecue", include_str!("../../plugins/barbecue.hk"))
            .await
            .unwrap();
        assert!(matches!(
            ACTION_DISPATCHER.recv_request(),
            PluginRequest::CreateWindowBar { .. }
        ));
        let config_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, .. } => request_id,
            _ => panic!("unexpected plugin request"),
        };

        runtime
            .resolve_request(
                config_request_id,
                serde_json::json!({
                    "value": {
                        "cwd": "/repo",
                        "plugin_config": {
                            "barbecue": { "separator": "›" }
                        }
                    }
                }),
            )
            .await
            .unwrap();
        let mut symbol_request_id = None;
        let mut symbol_request_count = 0;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::DocumentSymbols {
                request_id,
                buffer_index,
            } = request
            {
                assert_eq!(buffer_index, Some(2));
                symbol_request_id = Some(request_id);
                symbol_request_count += 1;
            }
        }
        let symbol_request_id = symbol_request_id.expect("expected symbol request");
        assert_eq!(symbol_request_count, 1);

        runtime
            .resolve_request(
                symbol_request_id,
                serde_json::json!({
                    "ok": true,
                    "file": "/repo/plugins/example.rs",
                    "buffer_index": 2,
                    "revision": 4,
                    "symbols": [{
                        "id": "inner",
                        "parent_id": null,
                        "name": "inner",
                        "kind_name": "Function",
                        "file": "/repo/plugins/example.rs",
                        "range": {
                            "start": { "line": 5, "character": 0 },
                            "end": { "line": 8, "character": 0 }
                        },
                        "selection_range": {
                            "start": { "line": 5, "character": 11 },
                            "end": { "line": 5, "character": 16 }
                        }
                    }]
                }),
            )
            .await
            .unwrap();

        let mut saw_symbol = false;
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::UpdateWindowBar { segments, .. } = request {
                saw_symbol |= segments.iter().any(|segment| segment.text == "󰊕 inner");
            }
        }
        assert!(saw_symbol);

        runtime
            .notify(
                "window_bar:action:barbecue",
                serde_json::json!({ "action": "jump:2:inner" }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenLocation { location, .. } => {
                assert_eq!(location.path, "/repo/plugins/example.rs");
                assert_eq!(location.line, 5);
                assert_eq!(location.column, 11);
                assert_eq!(
                    location.column_encoding,
                    crate::plugin::LocationColumnEncoding::Utf16
                );
            }
            _ => panic!("unexpected plugin request"),
        }
    }

    #[tokio::test]
    async fn git_dashboard_streams_porcelain_status_into_workspace() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new_with_permissions(HashMap::from([(
            "git".to_string(),
            PluginPermissions {
                process: vec!["git".to_string()],
            },
        )]));
        runtime
            .load_plugin("git", include_str!("../../plugins/git.hk"))
            .await
            .unwrap();
        let mut saw_cwd = false;
        let mut saw_config = false;
        let mut saw_info = false;
        let mut cwd_request_id = None;
        let mut config_request_id = None;
        let mut info_request_id = None;
        for _ in 0..3 {
            match ACTION_DISPATCHER.recv_request() {
                PluginRequest::GetConfig { request_id, key } => {
                    if key.as_deref() == Some("cwd") {
                        cwd_request_id = Some(request_id);
                        saw_cwd = true;
                    } else {
                        assert_eq!(key, None);
                        config_request_id = Some(request_id);
                        saw_config = true;
                    }
                }
                PluginRequest::EditorInfo(request_id) => {
                    info_request_id = Some(request_id);
                    saw_info = true;
                }
                _ => panic!("unexpected plugin request"),
            }
        }
        assert!(saw_cwd && saw_config && saw_info);
        runtime
            .resolve_request(
                cwd_request_id.expect("expected cwd request"),
                serde_json::json!({ "value": "." }),
            )
            .await
            .unwrap();
        runtime
            .resolve_request(
                config_request_id.expect("expected config request"),
                serde_json::json!({ "value": { "executable": "red", "plugin_config": {} } }),
            )
            .await
            .unwrap();
        runtime
            .resolve_request(
                info_request_id.expect("expected editor info request"),
                serde_json::json!({
                    "theme": {
                        "style": { "fg": null, "bg": null, "bold": false, "italic": false },
                        "ui_style": {
                            "muted": { "fg": null, "bg": null, "bold": false, "italic": false },
                            "popup_title": { "fg": null, "bg": null, "bold": false, "italic": false }
                        },
                        "colors": {}
                    }
                }),
            )
            .await
            .unwrap();
        runtime.execute_command("GitDashboard").await.unwrap();

        loop {
            if let PluginRequest::OpenWorkspace { id, config } = ACTION_DISPATCHER.recv_request() {
                assert_eq!(id, "git-dashboard");
                assert_eq!(config.title, "Git");
                break;
            }
        }

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            pump_process_events(&mut runtime).await.unwrap();
            let mut found = false;
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                if let PluginRequest::UpdateWorkspace { id, model } = request {
                    assert_eq!(id, "git-dashboard");
                    assert!(!model.header.is_empty());
                    assert!(!model.rows.is_empty());
                    found = true;
                }
            }
            if found {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "git dashboard did not update workspace"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn git_signs_deduplicate_split_windows_and_apply_staged_configuration() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path();
        let file = root.join("tracked.txt");
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        fs::write(&file, "before\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args([
                "-c",
                "user.name=Red Test",
                "-c",
                "user.email=red@example.test",
                "commit",
                "-qm",
                "initial",
            ])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        fs::write(&file, "after\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());

        let mut runtime = Runtime::new_with_permissions(HashMap::from([(
            "git".to_string(),
            PluginPermissions {
                process: vec!["git".to_string()],
            },
        )]));
        runtime
            .load_plugin("git", include_str!("../../plugins/git.hk"))
            .await
            .unwrap();
        let mut cwd_request_id = None;
        let mut config_request_id = None;
        let mut info_request_id = None;
        for _ in 0..3 {
            match ACTION_DISPATCHER.recv_request() {
                PluginRequest::GetConfig { request_id, key } if key.as_deref() == Some("cwd") => {
                    cwd_request_id = Some(request_id);
                }
                PluginRequest::GetConfig {
                    request_id,
                    key: None,
                } => {
                    config_request_id = Some(request_id);
                }
                PluginRequest::EditorInfo(request_id) => info_request_id = Some(request_id),
                _ => panic!("unexpected plugin request"),
            }
        }
        runtime
            .resolve_request(
                cwd_request_id.unwrap(),
                serde_json::json!({ "value": root.display().to_string() }),
            )
            .await
            .unwrap();
        runtime
            .resolve_request(
                config_request_id.unwrap(),
                serde_json::json!({
                    "value": {
                        "executable": "red",
                        "plugin_config": {
                            "git": {
                                "staged_signs": { "change": "old" },
                                "signs_staged": { "change": "!" }
                            }
                        }
                    }
                }),
            )
            .await
            .unwrap();
        runtime
            .resolve_request(
                info_request_id.unwrap(),
                serde_json::json!({
                    "theme": {
                        "style": { "fg": null, "bg": null, "bold": false, "italic": false },
                        "ui_style": {
                            "muted": { "fg": null, "bg": null, "bold": false, "italic": false },
                            "popup_title": { "fg": null, "bg": null, "bold": false, "italic": false }
                        },
                        "colors": {}
                    }
                }),
            )
            .await
            .unwrap();
        runtime.execute_command("GitRefresh").await.unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut expected_sign_count = 0;
        loop {
            pump_process_events(&mut runtime).await.unwrap();
            while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                match request {
                    PluginRequest::GetWindows { request_id } => {
                        runtime
                            .resolve_request(
                                request_id,
                                serde_json::json!({
                                    "windows": [
                                        {
                                            "buffer_path": file.display().to_string(),
                                            "buffer_index": 7,
                                            "active": true
                                        },
                                        {
                                            "buffer_path": file.display().to_string(),
                                            "buffer_index": 7,
                                            "active": false
                                        }
                                    ]
                                }),
                            )
                            .await
                            .unwrap();
                    }
                    PluginRequest::SetGutterSigns { signs, .. } => {
                        expected_sign_count = signs
                            .iter()
                            .filter(|sign| {
                                sign.buffer_index == 7 && sign.text == "!" && sign.priority == 5
                            })
                            .count();
                    }
                    _ => {}
                }
            }
            let active_process_count = runtime
                .inner
                .lock()
                .unwrap()
                .host
                .process_manager
                .active_process_count("git");
            if expected_sign_count > 0 && active_process_count == 0 {
                assert_eq!(expected_sign_count, 1);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "configured staged gutter sign was not emitted"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn git_hunk_navigation_targets_changed_lines_and_reports_boundaries() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path();
        let file = root.join("tracked.txt");
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        let original = (1..=30)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        fs::write(&file, &original).unwrap();
        assert!(Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args([
                "-c",
                "user.name=Red Test",
                "-c",
                "user.email=red@example.test",
                "commit",
                "-qm",
                "initial",
            ])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        let modified = original
            .replace("line 14\n", "changed 14\n")
            .replace("line 26\n", "changed 26\n");
        fs::write(&file, &modified).unwrap();

        let mut runtime = Runtime::new_with_permissions(HashMap::from([(
            "git".to_string(),
            PluginPermissions {
                process: vec!["git".to_string()],
            },
        )]));
        runtime
            .load_plugin("git", include_str!("../../plugins/git.hk"))
            .await
            .unwrap();
        let mut cwd_request_id = None;
        let mut config_request_id = None;
        let mut info_request_id = None;
        for _ in 0..3 {
            match ACTION_DISPATCHER.recv_request() {
                PluginRequest::GetConfig { request_id, key } if key.as_deref() == Some("cwd") => {
                    cwd_request_id = Some(request_id);
                }
                PluginRequest::GetConfig {
                    request_id,
                    key: None,
                } => config_request_id = Some(request_id),
                PluginRequest::EditorInfo(request_id) => info_request_id = Some(request_id),
                _ => panic!("unexpected plugin request"),
            }
        }
        runtime
            .resolve_request(
                cwd_request_id.unwrap(),
                serde_json::json!({ "value": root.display().to_string() }),
            )
            .await
            .unwrap();
        runtime
            .resolve_request(
                config_request_id.unwrap(),
                serde_json::json!({ "value": { "executable": "red", "plugin_config": {} } }),
            )
            .await
            .unwrap();
        runtime
            .resolve_request(
                info_request_id.unwrap(),
                serde_json::json!({
                    "theme": {
                        "style": { "fg": null, "bg": null, "bold": false, "italic": false },
                        "ui_style": {
                            "muted": { "fg": null, "bg": null, "bold": false, "italic": false },
                            "popup_title": { "fg": null, "bg": null, "bold": false, "italic": false }
                        },
                        "colors": {}
                    }
                }),
            )
            .await
            .unwrap();

        for (command, cursor_line, expected) in [
            ("GitHunkNext", 0, Ok((0, 13))),
            ("GitHunkPrevious", 29, Ok((0, 25))),
            ("GitHunkNext", 25, Err("No next Git hunk".to_string())),
            (
                "GitHunkPrevious",
                13,
                Err("No previous Git hunk".to_string()),
            ),
            (
                "GitHunkStage",
                0,
                Err("No Git hunk under cursor".to_string()),
            ),
            (
                "GitHunkUnstage",
                0,
                Err("No Git hunk under cursor".to_string()),
            ),
            (
                "GitHunkReset",
                0,
                Err("No Git hunk under cursor".to_string()),
            ),
        ] {
            runtime.execute_command(command).await.unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let result = loop {
                pump_process_events(&mut runtime).await.unwrap();
                let mut result = None;
                while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
                    match request {
                        PluginRequest::GetWindows { request_id } => {
                            runtime
                                .resolve_request(
                                    request_id,
                                    serde_json::json!({
                                        "windows": [{
                                            "buffer_path": file.display().to_string(),
                                            "buffer_index": 7,
                                            "active": true
                                        }]
                                    }),
                                )
                                .await
                                .unwrap();
                        }
                        PluginRequest::GetSelection { request_id } => {
                            runtime
                                .resolve_request(request_id, serde_json::Value::Null)
                                .await
                                .unwrap();
                        }
                        PluginRequest::GetBufferText { request_id, .. } => {
                            runtime
                                .resolve_request(
                                    request_id,
                                    serde_json::json!({ "text": modified.clone() }),
                                )
                                .await
                                .unwrap();
                        }
                        PluginRequest::GetCursorPosition { request_id } => {
                            runtime
                                .resolve_request(
                                    request_id,
                                    serde_json::json!({ "x": 0, "y": cursor_line }),
                                )
                                .await
                                .unwrap();
                        }
                        PluginRequest::SetCursorPosition { x, y } => {
                            result = Some(Ok((x, y)));
                        }
                        PluginRequest::Action(Action::Print(message)) => {
                            result = Some(Err(message));
                        }
                        _ => {}
                    }
                }
                if let Some(result) = result {
                    break result;
                }
                assert!(Instant::now() < deadline, "hunk action did not complete");
                tokio::time::sleep(Duration::from_millis(10)).await;
            };
            assert_eq!(result, expected);
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

        let cwd_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("cwd"));
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        runtime
            .resolve_request(cwd_request_id, serde_json::json!({ "value": "." }))
            .await
            .unwrap();
        let storage_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage {
                plugin,
                key,
                request_id,
            } => {
                assert_eq!(plugin, "project_search");
                assert_eq!(key, "history_by_cwd");
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        runtime
            .resolve_request(storage_request_id, serde_json::json!({ "value": {} }))
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
                        serde_json::json!({ "timer_id": timer_id }),
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
                            serde_json::json!({ "timer_id": timer_id }),
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

        assert!(Path::new(&item.label).ends_with(Path::new("plugins").join("project_search.hk")));
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
                assert_eq!(key, "history_by_cwd");
                assert_eq!(value, serde_json::json!({ ".": [query] }));
            }
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ClosePicker { id } => assert_eq!(id, 301),
            _ => panic!("unexpected plugin request"),
        }
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::OpenLocation { location, target } => {
                assert_eq!(
                    PathBuf::from(location.path),
                    Path::new("plugins").join("project_search.hk")
                );
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
            "saved_at": 1,
            "buffers": [
                {
                    "index": 0,
                    "path": "src/main.rs",
                    "dirty": false,
                    "cursor": { "x": 0, "y": 0 },
                    "viewport_top": 0,
                },
                {
                    "index": 1,
                    "path": "scratch.rs",
                    "dirty": true,
                    "cursor": { "x": 0, "y": 0 },
                    "viewport_top": 0,
                }
            ],
            "current_buffer_index": 0,
            "window_layout": {
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
        let startup_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("startup_file_count"));
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        runtime
            .resolve_request(startup_request_id, serde_json::json!({ "value": 0 }))
            .await
            .unwrap();
        let storage_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetPluginStorage {
                plugin,
                key,
                request_id,
            } => {
                assert_eq!(plugin, "session_restore");
                assert_eq!(key, "latest");
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        runtime
            .resolve_request(
                storage_request_id,
                serde_json::json!({ "value": snapshot.clone() }),
            )
            .await
            .unwrap();
        let cwd_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("cwd"));
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        runtime
            .resolve_request(
                cwd_request_id,
                serde_json::json!({ "value": "/tmp/project" }),
            )
            .await
            .unwrap();
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::RestoreEditorState {
                request_id,
                snapshot,
            } => {
                assert!(request_id.get() > 0);
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
        let _cwd_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("cwd"));
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        let _windows_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetWindows { request_id } => request_id,
            _ => panic!("unexpected plugin request"),
        };
        let root_directory_request_id = loop {
            if let PluginRequest::ListDirectory { path, request_id } =
                ACTION_DISPATCHER.recv_request()
            {
                assert_eq!(path, ".");
                break request_id;
            }
        };
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetGitStatus { path, request_id } => {
                assert_eq!(path, ".");
                assert!(request_id.get() > 0);
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .resolve_request(
                root_directory_request_id,
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
        let src_directory_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ListDirectory { path, request_id } => {
                assert_eq!(path, "./src");
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePanel { id, rows } => {
                assert_eq!(id, "neotree");
                assert_eq!(rows.len(), 3);
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .resolve_request(
                src_directory_request_id,
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
        let mut cwd_request_id = None;
        let mut windows_request_id = None;
        let mut git_status_request_id = None;
        for _ in 0..7 {
            match ACTION_DISPATCHER.recv_request() {
                PluginRequest::GetConfig { request_id, .. } => cwd_request_id = Some(request_id),
                PluginRequest::GetWindows { request_id } => windows_request_id = Some(request_id),
                PluginRequest::GetGitStatus { request_id, .. } => {
                    git_status_request_id = Some(request_id)
                }
                _ => {}
            }
        }

        runtime
            .resolve_request(
                cwd_request_id.expect("expected cwd request"),
                serde_json::json!({ "value": "/repo" }),
            )
            .await
            .unwrap();

        runtime
            .resolve_request(
                windows_request_id.expect("expected windows request"),
                serde_json::json!({
                    "windows": [{
                        "active": true,
                        "buffer_path": "/repo/src/main.rs",
                    }],
                }),
            )
            .await
            .unwrap();
        let root_directory_request_id = loop {
            if let PluginRequest::ListDirectory { path, request_id } =
                ACTION_DISPATCHER.recv_request()
            {
                assert_eq!(path, ".");
                break request_id;
            }
        };

        runtime
            .resolve_request(
                root_directory_request_id,
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
        let src_directory_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ListDirectory { path, request_id } => {
                assert_eq!(path, "./src");
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePanel { id, rows } => {
                assert_eq!(id, "neotree");
                assert_eq!(rows.len(), 2);
                assert!(rows[1].expanded.unwrap_or(false));
            }
            _ => panic!("unexpected plugin request"),
        }

        runtime
            .resolve_request(
                src_directory_request_id,
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
            .resolve_request(
                git_status_request_id.expect("expected git status request"),
                serde_json::json!({
                    "root": "/repo",
                    "statuses": [{
                        "path": "src/main.rs",
                        "absolute_path": "/repo/src/main.rs",
                        "status": "modified",
                    }],
                    "status_index": {
                        "/repo": "modified",
                        "/repo/src": "modified",
                        "/repo/src/main.rs": "modified",
                    },
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
    async fn neotree_renders_a_large_git_status_listing_within_the_instruction_budget() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("neotree", include_str!("../../plugins/neotree.hk"))
            .await
            .unwrap();
        runtime.execute_command("NeoTree").await.unwrap();

        let mut directory_request = None;
        let mut status_request = None;
        for _ in 0..7 {
            match ACTION_DISPATCHER.recv_request() {
                PluginRequest::ListDirectory { path, request_id } => {
                    assert_eq!(path, ".");
                    directory_request = Some(request_id);
                }
                PluginRequest::GetGitStatus { path, request_id } => {
                    assert_eq!(path, ".");
                    status_request = Some(request_id);
                }
                _ => {}
            }
        }

        let mut entries = (0..120)
            .map(|index| {
                serde_json::json!({
                    "name": format!("dir-{index:03}"),
                    "path": format!("./dir-{index:03}"),
                    "kind": "directory",
                })
            })
            .collect::<Vec<_>>();
        entries.push(serde_json::json!({
            "name": "tracked.rs",
            "path": "./tracked.rs",
            "kind": "file",
        }));
        runtime
            .resolve_request(
                directory_request.expect("expected root directory request"),
                serde_json::json!({ "path": ".", "entries": entries, "error": null }),
            )
            .await
            .unwrap();
        drain_requests();

        let mut statuses = Vec::new();
        for index in 0..120 {
            for (offset, status) in [
                "ignored",
                "untracked",
                "modified",
                "added",
                "deleted",
                "renamed",
                "conflict",
                "staged",
            ]
            .into_iter()
            .enumerate()
            {
                statuses.push(serde_json::json!({
                    "path": format!("dir-{index:03}/nested/file-{offset}.rs"),
                    "absolute_path": format!("/repo/dir-{index:03}/nested/file-{offset}.rs"),
                    "status": status,
                }));
            }
        }
        statuses.push(serde_json::json!({
            "path": "tracked.rs",
            "absolute_path": "/repo/tracked.rs",
            "status": "modified",
        }));
        let status_index = crate::editor::git_status_index(&statuses, "/repo");

        runtime
            .resolve_request(
                status_request.expect("expected git status request"),
                serde_json::json!({
                    "root": "/repo",
                    "statuses": statuses,
                    "status_index": status_index,
                    "error": null,
                }),
            )
            .await
            .unwrap();

        let rows = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePanel { id, rows } => {
                assert_eq!(id, "neotree");
                rows
            }
            _ => panic!("expected neotree panel update"),
        };
        assert_eq!(rows.len(), 122);
        assert_eq!(rows[0].right_segments[0].text, "");
        assert!(rows[1..121]
            .iter()
            .all(|row| row.right_segments[0].text == ""));
        assert_eq!(rows[121].right_segments[0].text, "");
    }

    #[tokio::test]
    async fn neotree_caps_a_pathological_visible_listing_within_the_instruction_budget() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("neotree", include_str!("../../plugins/neotree.hk"))
            .await
            .unwrap();
        runtime.execute_command("NeoTree").await.unwrap();

        let mut directory_request = None;
        for _ in 0..7 {
            if let PluginRequest::ListDirectory { path, request_id } =
                ACTION_DISPATCHER.recv_request()
            {
                assert_eq!(path, ".");
                directory_request = Some(request_id);
            }
        }

        let entries = (0..1_000)
            .map(|index| {
                serde_json::json!({
                    "name": format!("file-{index:04}.rlib"),
                    "path": format!("./file-{index:04}.rlib"),
                    "kind": "file",
                })
            })
            .collect::<Vec<_>>();
        runtime
            .resolve_request(
                directory_request.expect("expected root directory request"),
                serde_json::json!({
                    "path": ".",
                    "entries": entries,
                    "truncated": true,
                    "error": null,
                }),
            )
            .await
            .unwrap();

        match ACTION_DISPATCHER.recv_request() {
            PluginRequest::WatchDirectory { path, .. } => assert_eq!(path, "."),
            _ => panic!("expected neotree directory watch"),
        }
        let rows = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePanel { id, rows } => {
                assert_eq!(id, "neotree");
                rows
            }
            _ => panic!("expected neotree panel update"),
        };
        assert_eq!(rows.len(), 201);
        assert!(rows.last().unwrap().path.is_none());
        assert_eq!(
            rows.last().unwrap().segments[1].text,
            "… tree limited to 200 rows"
        );
    }

    #[tokio::test]
    async fn neotree_renders_git_status_for_a_filesystem_root_repository() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime
            .load_plugin("neotree", include_str!("../../plugins/neotree.hk"))
            .await
            .unwrap();
        runtime.execute_command("NeoTree").await.unwrap();

        let mut directory_request = None;
        let mut status_request = None;
        for _ in 0..7 {
            match ACTION_DISPATCHER.recv_request() {
                PluginRequest::ListDirectory { path, request_id } => {
                    assert_eq!(path, ".");
                    directory_request = Some(request_id);
                }
                PluginRequest::GetGitStatus { path, request_id } => {
                    assert_eq!(path, ".");
                    status_request = Some(request_id);
                }
                _ => {}
            }
        }

        runtime
            .resolve_request(
                directory_request.expect("expected root directory request"),
                serde_json::json!({
                    "path": ".",
                    "entries": [{ "name": "src", "path": "./src", "kind": "directory" }],
                    "error": null,
                }),
            )
            .await
            .unwrap();
        drain_requests();

        let statuses = [serde_json::json!({
            "path": "src/main.rs",
            "absolute_path": "/src/main.rs",
            "status": "modified",
        })];
        let status_index = crate::editor::git_status_index(&statuses, "/");

        runtime
            .resolve_request(
                status_request.expect("expected git status request"),
                serde_json::json!({
                    "root": "/",
                    "statuses": statuses,
                    "status_index": status_index,
                    "error": null,
                }),
            )
            .await
            .unwrap();

        let rows = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::UpdatePanel { id, rows } => {
                assert_eq!(id, "neotree");
                rows
            }
            _ => panic!("expected neotree panel update"),
        };
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].right_segments[0].text, "");
        assert_eq!(rows[1].right_segments[0].text, "");
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

        let config_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::GetConfig { request_id, key } => {
                assert_eq!(key.as_deref(), Some("theme"));
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };
        let assets_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::ListRuntimeAssets { kind, request_id } => {
                assert_eq!(kind, RuntimeAssetKind::Theme);
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };

        runtime
            .resolve_request(
                config_request_id,
                serde_json::json!({ "value": "custom.json" }),
            )
            .await
            .unwrap();
        assert!(ACTION_DISPATCHER.try_recv_request().is_none());

        runtime
            .resolve_request(
                assets_request_id,
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
                assert_eq!(items[1].label, "Custom");
                assert_eq!(items[2].label, "Custom");
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

        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::DocumentSymbols {
                request_id,
                buffer_index,
            } => {
                assert_eq!(buffer_index, None);
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };

        runtime
            .resolve_request(request_id, sample_symbol_payload())
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
        let _initial_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::WorkspaceSymbols { request_id, query } => {
                assert_eq!(query, "");
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };

        runtime
            .notify("picker:query:202", serde_json::json!("main"))
            .await
            .unwrap();

        let query_request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::WorkspaceSymbols { request_id, query } => {
                assert_eq!(query, "main");
                request_id
            }
            _ => panic!("unexpected plugin request"),
        };

        runtime
            .resolve_request(query_request_id, sample_symbol_payload())
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
        runtime.execute_command("LspDocumentSymbols").await.unwrap();
        let request_id = match ACTION_DISPATCHER.recv_request() {
            PluginRequest::DocumentSymbols { request_id, .. } => request_id,
            _ => panic!("unexpected plugin request"),
        };
        runtime
            .resolve_request(request_id, sample_symbol_payload())
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
