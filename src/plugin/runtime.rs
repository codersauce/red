use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use husk::{Host, Value};
use uuid::Uuid;

use crate::{
    config::{Config, PluginPermissions},
    editor::{Action, PluginRequest, ACTION_DISPATCHER},
    log,
};

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

#[derive(Default)]
struct RedHost {
    _process_permissions: HashMap<String, PluginPermissions>,
}

impl RedHost {
    fn new(process_permissions: HashMap<String, PluginPermissions>) -> Self {
        Self {
            _process_permissions: process_permissions,
        }
    }
}

impl Host for RedHost {
    fn log(&mut self, message: &str) {
        log!("[PLUGIN:HUSK] {}", message);
    }

    fn execute(&mut self, action: &str, args: &[Value]) -> anyhow::Result<Value> {
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

fn value_to_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Int(value) => u64::try_from(*value).ok(),
        Value::Float(value) if *value >= 0.0 => Some(*value as u64),
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
    use super::*;
    use crate::editor::{PluginRequest, PLUGIN_DISPATCHER_TEST_LOCK};

    #[tokio::test]
    async fn executes_husk_command_through_host() {
        let _lock = PLUGIN_DISPATCHER_TEST_LOCK.lock().await;
        while ACTION_DISPATCHER.try_recv_request().is_some() {}

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
}
