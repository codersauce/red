use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use husk::{Host, RequestId, Value};
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
    Decoration, GutterSign, OverlayConfig, PanelConfig, PanelRow, WindowBarConfig, WindowBarSegment,
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
    snapshots: HashMap<String, Value>,
}

impl RedHost {
    fn new(process_permissions: HashMap<String, PluginPermissions>) -> Self {
        Self {
            process_manager: ProcessManager::new(process_permissions),
            snapshots: HashMap::new(),
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
            "SetCursorPosition" => {
                let x = args.first().and_then(value_to_u64).unwrap_or(0) as usize;
                let y = args.get(1).and_then(value_to_u64).unwrap_or(0) as usize;
                ACTION_DISPATCHER.send_request(PluginRequest::SetCursorPosition { x, y });
            }
            "CloseScratchBuffer" => {
                let buffer_index = args
                    .first()
                    .and_then(value_to_u64)
                    .and_then(|index| usize::try_from(index).ok())
                    .ok_or_else(|| anyhow::anyhow!("CloseScratchBuffer requires a buffer index"))?;
                ACTION_DISPATCHER.send_request(PluginRequest::CloseScratchBuffer { buffer_index });
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
                ACTION_DISPATCHER.send_request(PluginRequest::SetGutterSigns { namespace, signs });
            }
            "ClearGutterSigns" => {
                let namespace = args
                    .first()
                    .and_then(Value::as_str)
                    .map_or_else(|| "default".to_string(), str::to_string);
                ACTION_DISPATCHER.send_request(PluginRequest::ClearGutterSigns { namespace });
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
                ACTION_DISPATCHER.send_request(PluginRequest::CreateWindowBar { id, config });
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
                ACTION_DISPATCHER.send_request(PluginRequest::UpdateWindowBar {
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
                ACTION_DISPATCHER.send_request(PluginRequest::CloseWindowBar { id, window_id });
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
                ACTION_DISPATCHER.send_request(PluginRequest::OpenWorkspace { id, config });
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
                ACTION_DISPATCHER.send_request(PluginRequest::UpdateWorkspace { id, model });
            }
            "CloseWorkspace" => {
                let id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("CloseWorkspace requires a workspace id"))?
                    .to_string();
                ACTION_DISPATCHER.send_request(PluginRequest::CloseWorkspace { id });
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
                    json_str(&event, "cause"),
                    json_usize_at(&event, &["from", "x"]),
                    json_usize_at(&event, &["from", "y"]),
                    json_usize_at(&event, &["to", "x"]),
                    json_usize_at(&event, &["to", "y"]),
                    json_str(&event, "mode")
                );
                ACTION_DISPATCHER.send_request(PluginRequest::Action(Action::Print(message)));
            }
            "RecordModeChanged" => {
                let event = first_json(args)?;
                let message = format!(
                    "mode:{}:{}->{}",
                    json_str(&event, "cause"),
                    json_str(&event, "from"),
                    json_str(&event, "to")
                );
                ACTION_DISPATCHER.send_request(PluginRequest::Action(Action::Print(message)));
            }
            "RecordSearchHighlighted" => {
                let event = first_json(args)?;
                let message = format!(
                    "search:{}:{}:{}",
                    json_str(&event, "source"),
                    json_str(&event, "term"),
                    json_str(&event, "direction")
                );
                ACTION_DISPATCHER.send_request(PluginRequest::Action(Action::Print(message)));
            }
            "RecordSearchCleared" => {
                let event = first_json(args)?;
                let message = format!("cleared:{}", json_str(&event, "term"));
                ACTION_DISPATCHER.send_request(PluginRequest::Action(Action::Print(message)));
            }
            "SetTimeout" => {
                let delay_ms = args.first().and_then(value_to_u64).unwrap_or(0);
                let id = schedule_timeout(delay_ms);
                return Ok(Value::String(id));
            }
            "CancelTimeout" => {
                let timer_id = args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("CancelTimeout requires a timer id"))?;
                cancel_timeout(timer_id);
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
        ACTION_DISPATCHER.send_request(request);
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

fn schedule_timeout(delay_ms: u64) -> String {
    let id = Uuid::new_v4().to_string();
    PENDING_TIMEOUTS.lock().unwrap().push(PendingTimeout {
        id: id.clone(),
        expires_at: Instant::now() + Duration::from_millis(delay_ms),
    });
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
        let RuntimeInner { vm, host, .. } = &mut *inner;
        vm.load_plugin_at(name, path, source, host)
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

    pub async fn resolve_request(
        &mut self,
        request_id: RequestId,
        payload: serde_json::Value,
    ) -> anyhow::Result<bool> {
        let mut inner = self.inner.lock().unwrap();
        let RuntimeInner { vm, host, .. } = &mut *inner;
        vm.resolve_request(request_id, payload, host)
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

#[allow(dead_code)]
fn _keep_config_used(_: &Config) {}

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
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
    async fn barbecue_renders_breadcrumbs_and_opens_symbol_action() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        drain_requests();

        let mut runtime = Runtime::new();
        runtime.set_snapshot(
            "windows",
            serde_json::json!({
                "windows": [{
                    "window_id": 7,
                    "buffer_index": 2,
                    "buffer_path": "/repo/plugins/example.rs",
                    "revision": 4,
                    "cursor": { "x": 1, "y": 6 },
                    "lsp_position": { "line": 6, "character": 1 },
                }]
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
        while let Some(request) = ACTION_DISPATCHER.try_recv_request() {
            if let PluginRequest::DocumentSymbols {
                request_id,
                buffer_index,
            } = request
            {
                assert_eq!(buffer_index, Some(2));
                symbol_request_id = Some(request_id);
            }
        }
        let symbol_request_id = symbol_request_id.expect("expected symbol request");

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
