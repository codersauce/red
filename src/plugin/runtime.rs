use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{mpsc, Mutex},
    thread,
};

use deno_core::{
    error::AnyError, extension, op2, FastString, JsRuntime, OpState, PollEventLoopOptions,
    RuntimeOptions,
};
use deno_error::JsError;
use json_comments::StripComments;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::{
    config::{Config, PluginPermissions},
    editor::{PluginRequest, ACTION_DISPATCHER},
    log,
    lsp::Range,
    ui::{PickerItem, PickerOptions, PickerPreview},
};

use super::{
    loader::TsModuleLoader,
    process::{ProcessEvent, ProcessManager, ProcessSpawnOptions},
    OpenLocationTarget, PluginLocation,
};

#[derive(Debug, thiserror::Error, JsError)]
#[class(generic)]
#[error(transparent)]
struct PluginOpError(#[from] AnyError);

impl From<serde_json::Error> for PluginOpError {
    fn from(error: serde_json::Error) -> Self {
        anyhow::Error::from(error).into()
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InlayHintsOptions {
    #[serde(default)]
    range: Option<Range>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReferencesOptions {
    #[serde(default = "default_include_declaration")]
    include_declaration: bool,
}

impl Default for ReferencesOptions {
    fn default() -> Self {
        Self {
            include_declaration: true,
        }
    }
}

fn default_include_declaration() -> bool {
    true
}

impl From<std::io::Error> for PluginOpError {
    fn from(error: std::io::Error) -> Self {
        anyhow::Error::from(error).into()
    }
}

/// Format JavaScript errors with stack traces for better debugging
fn format_js_error(error: &anyhow::Error) -> String {
    let error_str = error.to_string();

    // Check if it's a JavaScript error with a stack trace
    if let Some(js_error) = error.downcast_ref::<deno_core::error::JsError>() {
        let mut formatted = String::new();

        // Add the main error message
        if let Some(message) = &js_error.message {
            formatted.push_str(&format!("{}\n", message));
        }

        // Add stack frames if available
        if !js_error.frames.is_empty() {
            formatted.push_str("\nStack trace:\n");
            for frame in &js_error.frames {
                let location =
                    if let (Some(line), Some(column)) = (frame.line_number, frame.column_number) {
                        format!(
                            "{}:{}:{}",
                            frame.file_name.as_deref().unwrap_or("<anonymous>"),
                            line,
                            column
                        )
                    } else {
                        frame
                            .file_name
                            .as_deref()
                            .unwrap_or("<anonymous>")
                            .to_string()
                    };

                if let Some(func_name) = &frame.function_name {
                    formatted.push_str(&format!("  at {} ({})\n", func_name, location));
                } else {
                    formatted.push_str(&format!("  at {}\n", location));
                }
            }
        }

        // Log the full error details for debugging
        log!("Plugin error details: {}", formatted);

        formatted
    } else {
        // For non-JS errors, just return the error string
        error_str
    }
}

#[derive(Debug)]
enum Task {
    LoadModule {
        code: String,
        responder: oneshot::Sender<anyhow::Result<()>>,
    },
    Execute {
        code: String,
        responder: oneshot::Sender<anyhow::Result<()>>,
    },
}

pub struct Runtime {
    sender: mpsc::Sender<Task>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Runtime {
    pub fn new() -> Self {
        Self::new_with_permissions(HashMap::new())
    }

    pub fn new_with_permissions(process_permissions: HashMap<String, PluginPermissions>) -> Self {
        let (sender, receiver) = mpsc::channel::<Task>();
        let mut n = 1;

        thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            let mut js_runtime = JsRuntime::new(RuntimeOptions {
                module_loader: Some(Rc::new(TsModuleLoader)),
                extensions: vec![js_runtime::init()],
                ..Default::default()
            });
            js_runtime
                .op_state()
                .borrow_mut()
                .put(ProcessManager::new(process_permissions));

            for task in receiver {
                let _res: anyhow::Result<()> = runtime.block_on(async {
                    match task {
                        Task::LoadModule { code, responder } => {
                            match load_main_module(
                                &mut js_runtime,
                                &format!("file:///module-{n}.ts"),
                                code,
                            )
                            .await
                            {
                                Ok(_) => {
                                    n += 1;
                                    responder.send(Ok(())).unwrap();
                                }
                                Err(e) => {
                                    let formatted_error = format_js_error(&e);
                                    responder
                                        .send(Err(anyhow::anyhow!(
                                            "A plugin failed to load:\n{}",
                                            formatted_error
                                        )))
                                        .unwrap();
                                }
                            }
                        }
                        Task::Execute { code, responder } => {
                            match run(&mut js_runtime, code).await {
                                Ok(_) => {
                                    responder.send(Ok(())).unwrap();
                                }
                                Err(e) => {
                                    let formatted_error = format_js_error(&e);
                                    responder
                                        .send(Err(anyhow::anyhow!(
                                            "A plugin command failed:\n{}",
                                            formatted_error
                                        )))
                                        .unwrap();
                                }
                            }
                        }
                    }
                    // log!("Done with code");
                    Ok(())
                });
            }
        });

        Runtime { sender }
    }

    pub async fn add_module(&mut self, code: &str) -> anyhow::Result<()> {
        let (responder, rx) = oneshot::channel::<anyhow::Result<()>>();
        let code = code.to_string();

        self.sender.send(Task::LoadModule { code, responder })?;
        rx.await?
    }

    pub async fn run(&mut self, code: &str) -> anyhow::Result<()> {
        let (responder, rx) = oneshot::channel::<anyhow::Result<()>>();
        let code = code.to_string();

        self.sender.send(Task::Execute { code, responder })?;
        rx.await?
    }
}

async fn load_main_module(
    js_runtime: &mut JsRuntime,
    name: &str,
    code: String,
) -> anyhow::Result<()> {
    // Use Box::leak to create a 'static lifetime for the module name
    let module_name: &'static str = Box::leak(name.to_string().into_boxed_str());

    // Load the code as an ES module using the module loader
    let module_specifier = deno_core::resolve_url(module_name)?;

    // First, we need to register the module with the runtime
    let module_id = js_runtime
        .load_side_es_module_from_code(&module_specifier, FastString::from(code))
        .await?;

    // Instantiate and evaluate the module
    let evaluate = js_runtime.mod_evaluate(module_id);

    // Run the event loop to execute the module
    js_runtime
        .run_event_loop(PollEventLoopOptions::default())
        .await?;

    // Wait for the module evaluation to complete
    evaluate.await?;

    Ok(())
}

// https://github.com/denoland/deno_core/issues/388#issuecomment-1865422590
async fn run(runtime: &mut JsRuntime, code: String) -> anyhow::Result<()> {
    let code: FastString = code.into();
    let result = runtime.execute_script("<anon>", code);
    // let value = runtime
    //     .with_event_loop_promise(
    //         Box::pin(async move { result }),
    //         PollEventLoopOptions::default(),
    //     )
    //     .await?;

    runtime
        .run_event_loop(PollEventLoopOptions {
            wait_for_inspector: false,
        })
        .await?;

    // TODO: check if we'll need the return value
    let _value = result?;

    Ok(())
}

#[op2]
fn op_editor_info(id: Option<i32>) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::EditorInfo(id));
    Ok(())
}

#[op2]
fn op_open_picker(
    #[string] title: Option<String>,
    id: Option<i32>,
    #[serde] items: serde_json::Value,
) -> Result<(), PluginOpError> {
    let Value::Array(items) = items else {
        return Err(anyhow::anyhow!("Invalid items").into());
    };
    ACTION_DISPATCHER.send_request(PluginRequest::OpenPicker(title, id, items));
    Ok(())
}

#[op2]
fn op_open_live_picker(
    #[string] title: Option<String>,
    id: Option<i32>,
    #[serde] items: serde_json::Value,
    #[string] initial_selection: Option<String>,
) -> Result<(), PluginOpError> {
    let Value::Array(items) = items else {
        return Err(anyhow::anyhow!("Invalid items").into());
    };
    ACTION_DISPATCHER.send_request(PluginRequest::OpenLivePicker(
        title,
        id,
        items,
        initial_selection,
    ));
    Ok(())
}

#[op2]
#[string]
fn op_spawn_process(
    state: &mut OpState,
    #[string] plugin_name: String,
    #[serde] options: ProcessSpawnOptions,
) -> Result<String, PluginOpError> {
    Ok(state
        .borrow_mut::<ProcessManager>()
        .spawn(&plugin_name, options)?)
}

#[op2(fast)]
fn op_kill_process(
    state: &mut OpState,
    #[string] plugin_name: String,
    #[string] process_id: String,
) -> Result<(), PluginOpError> {
    state
        .borrow_mut::<ProcessManager>()
        .kill(&plugin_name, &process_id)?;
    Ok(())
}

#[op2]
#[serde]
fn op_poll_process_events(state: &mut OpState) -> Result<Vec<ProcessEvent>, PluginOpError> {
    Ok(state.borrow_mut::<ProcessManager>().poll_events())
}

#[op2]
fn op_open_location(
    #[serde] location: PluginLocation,
    #[serde] target: OpenLocationTarget,
) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::OpenLocation { location, target });
    Ok(())
}

#[op2]
fn op_open_dynamic_picker(
    #[string] title: Option<String>,
    id: i32,
    #[serde] items: Vec<PickerItem>,
    #[serde] options: PickerOptions,
) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::OpenDynamicPicker {
        title,
        id,
        items,
        options,
    });
    Ok(())
}

#[op2]
fn op_update_picker_items(id: i32, #[serde] items: Vec<PickerItem>) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::UpdatePickerItems { id, items });
    Ok(())
}

#[op2(fast)]
fn op_update_picker_query(id: i32, #[string] query: String) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::UpdatePickerQuery { id, query });
    Ok(())
}

#[op2]
fn op_update_picker_status(id: i32, #[string] status: Option<String>) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::UpdatePickerStatus { id, status });
    Ok(())
}

#[op2]
fn op_update_picker_preview(
    id: i32,
    #[serde] preview: Option<PickerPreview>,
) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::UpdatePickerPreview { id, preview });
    Ok(())
}

#[op2(fast)]
fn op_close_picker(id: i32) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::ClosePicker { id });
    Ok(())
}

#[op2(fast)]
fn op_lsp_document_symbols(request_id: i32) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::DocumentSymbols { request_id });
    Ok(())
}

#[op2(fast)]
fn op_lsp_workspace_symbols(request_id: i32, #[string] query: String) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::WorkspaceSymbols { request_id, query });
    Ok(())
}

#[op2]
fn op_lsp_references(
    request_id: i32,
    #[serde] options: serde_json::Value,
) -> Result<(), PluginOpError> {
    let options = if options.is_null() {
        ReferencesOptions::default()
    } else {
        serde_json::from_value(options)?
    };
    ACTION_DISPATCHER.send_request(PluginRequest::References {
        request_id,
        include_declaration: options.include_declaration,
    });
    Ok(())
}

#[op2]
fn op_lsp_inlay_hints(
    request_id: i32,
    #[serde] options: serde_json::Value,
) -> Result<(), PluginOpError> {
    let options = if options.is_null() {
        InlayHintsOptions::default()
    } else {
        serde_json::from_value(options)?
    };
    ACTION_DISPATCHER.send_request(PluginRequest::InlayHints {
        request_id,
        range: options.range,
    });
    Ok(())
}

#[op2]
#[serde]
fn op_list_themes() -> Result<serde_json::Value, PluginOpError> {
    Ok(json!(list_themes_in_dir(&Config::path("themes"))?))
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct ThemeListEntry {
    name: String,
    file: String,
}

#[derive(Deserialize)]
struct ThemeMetadata {
    name: Option<String>,
}

fn list_themes_in_dir(themes_dir: &Path) -> anyhow::Result<Vec<ThemeListEntry>> {
    if !themes_dir.exists() {
        return Ok(vec![]);
    }

    let mut themes = fs::read_dir(themes_dir)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                return None;
            }
            let file = path.file_name()?.to_str()?.to_string();
            let name = theme_name_from_file(&path)
                .ok()
                .flatten()
                .unwrap_or_else(|| file.clone());
            Some(ThemeListEntry { name, file })
        })
        .collect::<Vec<_>>();
    themes.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.file.cmp(&b.file))
    });
    Ok(themes)
}

fn theme_name_from_file(path: &Path) -> anyhow::Result<Option<String>> {
    let contents = fs::read_to_string(path)?;
    let contents = StripComments::new(contents.as_bytes());
    let metadata: ThemeMetadata = serde_json::from_reader(contents)?;
    Ok(metadata
        .name
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty()))
}

#[op2]
fn op_trigger_action(
    #[string] action: String,
    #[serde] params: Option<serde_json::Value>,
) -> Result<(), PluginOpError> {
    let action = if let Some(params) = params {
        log!("Triggering {action} with {params:?}");
        let json = json!({ action: params });
        serde_json::from_value(json)?
    } else {
        let json = json!(action);
        serde_json::from_value(json)?
    };

    log!("Action = {action:?}");
    ACTION_DISPATCHER.send_request(PluginRequest::Action(action));

    Ok(())
}

#[op2]
fn op_log(#[string] level: Option<String>, #[serde] msg: serde_json::Value) {
    let message = match msg {
        serde_json::Value::String(s) => s,
        serde_json::Value::Array(arr) => arr
            .iter()
            .map(|m| match m {
                serde_json::Value::String(s) => s.to_string(),
                _ => format!("{:?}", m),
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => format!("{:?}", msg),
    };

    // Map plugin log levels to our LogLevel enum
    match level.as_deref() {
        Some("debug") => log!("[PLUGIN:DEBUG] {}", message),
        Some("warn") => log!("[PLUGIN:WARN] {}", message),
        Some("error") => log!("[PLUGIN:ERROR] {}", message),
        _ => log!("[PLUGIN:INFO] {}", message),
    }
}

use std::time::{Duration, Instant};

#[derive(Debug)]
struct PendingTimeout {
    id: String,
    expires_at: Instant,
}

lazy_static::lazy_static! {
    static ref PENDING_TIMEOUTS: Mutex<Vec<PendingTimeout>> = Mutex::new(Vec::new());
    static ref INTERVALS: Mutex<HashMap<String, IntervalHandle>> = Mutex::new(HashMap::new());
    static ref INTERVAL_CALLBACKS: Mutex<HashMap<String, String>> = Mutex::new(HashMap::new());
}

struct IntervalHandle {
    handle: tokio::task::JoinHandle<()>,
    cancel_sender: Option<tokio::sync::oneshot::Sender<()>>,
}

pub fn poll_timer_callbacks() -> Vec<PluginRequest> {
    let mut requests = Vec::new();
    let now = Instant::now();

    let mut timeouts = PENDING_TIMEOUTS.lock().unwrap();
    let mut i = 0;
    while i < timeouts.len() {
        if timeouts[i].expires_at <= now {
            let timeout = timeouts.remove(i);
            log!("[TIMER] Timer {} expired, dispatching callback", timeout.id);
            requests.push(PluginRequest::TimeoutCallback {
                timer_id: timeout.id,
            });
        } else {
            i += 1;
        }
    }

    requests
}

#[op2]
#[string]
fn op_set_timeout(delay: f64) -> Result<String, PluginOpError> {
    // Limit the number of concurrent timers per plugin runtime
    const MAX_TIMERS: usize = 1000;

    let mut timeouts = PENDING_TIMEOUTS.lock().unwrap();
    if timeouts.len() >= MAX_TIMERS {
        return Err(anyhow::anyhow!("Too many timers, maximum {} allowed", MAX_TIMERS).into());
    }

    let id = Uuid::new_v4().to_string();
    let expires_at = Instant::now() + Duration::from_millis(delay as u64);

    log!(
        "[TIMER] Creating timeout {} with delay {}ms, expires at {:?}",
        id,
        delay,
        expires_at
    );

    timeouts.push(PendingTimeout {
        id: id.clone(),
        expires_at,
    });

    Ok(id)
}

#[op2(fast)]
fn op_clear_timeout(#[string] id: String) -> Result<(), PluginOpError> {
    let mut timeouts = PENDING_TIMEOUTS.lock().unwrap();
    timeouts.retain(|t| t.id != id);
    Ok(())
}

#[op2]
#[string]
fn op_set_interval(delay: f64, #[string] callback_id: String) -> Result<String, PluginOpError> {
    // Limit the number of concurrent timers per plugin runtime
    const MAX_TIMERS: usize = 1000;

    // Check combined limit of timeouts and intervals
    let timeout_count = PENDING_TIMEOUTS.lock().unwrap().len();
    let interval_count = INTERVALS.lock().unwrap().len();
    if timeout_count + interval_count >= MAX_TIMERS {
        return Err(anyhow::anyhow!("Too many timers, maximum {} allowed", MAX_TIMERS).into());
    }

    let id = Uuid::new_v4().to_string();
    let id_clone = id.clone();
    let (cancel_sender, mut cancel_receiver) = tokio::sync::oneshot::channel::<()>();

    // Store the callback ID for this interval
    INTERVAL_CALLBACKS
        .lock()
        .unwrap()
        .insert(id.clone(), callback_id);

    let handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(delay as u64));
        interval.tick().await; // First tick is immediate, skip it

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    // Send callback request to the editor
                    ACTION_DISPATCHER.send_request(PluginRequest::IntervalCallback {
                        interval_id: id_clone.clone()
                    });
                }
                _ = &mut cancel_receiver => {
                    // Interval was cancelled
                    break;
                }
            }
        }

        // Clean up
        INTERVAL_CALLBACKS.lock().unwrap().remove(&id_clone);
        INTERVALS.lock().unwrap().remove(&id_clone);
    });

    let mut intervals = INTERVALS.lock().unwrap();
    intervals.insert(
        id.clone(),
        IntervalHandle {
            handle,
            cancel_sender: Some(cancel_sender),
        },
    );

    Ok(id)
}

#[op2(fast)]
fn op_clear_interval(#[string] id: String) -> Result<(), PluginOpError> {
    // Remove from callbacks map
    INTERVAL_CALLBACKS.lock().unwrap().remove(&id);

    // Remove from intervals map and cancel
    if let Some(mut handle) = INTERVALS.lock().unwrap().remove(&id) {
        // Send cancellation signal
        if let Some(sender) = handle.cancel_sender.take() {
            let _ = sender.send(()); // Ignore error if receiver already dropped
        }
        // Abort the task
        handle.handle.abort();
    }
    Ok(())
}

#[op2]
#[string]
fn op_get_interval_callback_id(#[string] interval_id: String) -> Result<String, PluginOpError> {
    let callbacks = INTERVAL_CALLBACKS.lock().unwrap();
    Ok(callbacks
        .get(&interval_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Interval ID not found"))?)
}

#[op2(fast)]
fn op_buffer_insert(x: u32, y: u32, #[string] text: String) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::BufferInsert {
        x: x as usize,
        y: y as usize,
        text,
    });
    Ok(())
}

#[op2(fast)]
fn op_buffer_delete(x: u32, y: u32, length: u32) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::BufferDelete {
        x: x as usize,
        y: y as usize,
        length: length as usize,
    });
    Ok(())
}

#[op2(fast)]
fn op_buffer_replace(
    x: u32,
    y: u32,
    length: u32,
    #[string] text: String,
) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::BufferReplace {
        x: x as usize,
        y: y as usize,
        length: length as usize,
        text,
    });
    Ok(())
}

#[op2(fast)]
fn op_get_cursor_position() -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::GetCursorPosition);
    Ok(())
}

#[op2(fast)]
fn op_set_cursor_position(x: u32, y: u32) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::SetCursorPosition {
        x: x as usize,
        y: y as usize,
    });
    Ok(())
}

#[op2]
fn op_get_buffer_text(start_line: Option<u32>, end_line: Option<u32>) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::GetBufferText {
        start_line: start_line.map(|l| l as usize),
        end_line: end_line.map(|l| l as usize),
    });
    Ok(())
}

#[op2(fast)]
fn op_get_viewport_layout(request_id: i32) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::GetViewportLayout { request_id });
    Ok(())
}

#[op2]
fn op_set_decorations(
    #[string] namespace: String,
    #[serde] decorations: serde_json::Value,
) -> Result<(), PluginOpError> {
    let decorations = serde_json::from_value(decorations)?;
    ACTION_DISPATCHER.send_request(PluginRequest::SetDecorations {
        namespace,
        decorations,
    });
    Ok(())
}

#[op2(fast)]
fn op_clear_decorations(#[string] namespace: String) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::ClearDecorations { namespace });
    Ok(())
}

#[op2]
fn op_get_config(#[string] key: Option<String>) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::GetConfig { key });
    Ok(())
}

#[op2(fast)]
fn op_get_editor_state(request_id: i32) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::GetEditorState { request_id });
    Ok(())
}

#[op2]
fn op_restore_editor_state(
    request_id: i32,
    #[serde] snapshot: serde_json::Value,
) -> Result<(), PluginOpError> {
    let snapshot = serde_json::from_value(snapshot)?;
    ACTION_DISPATCHER.send_request(PluginRequest::RestoreEditorState {
        request_id,
        snapshot,
    });
    Ok(())
}

#[op2]
#[serde]
fn op_plugin_storage_get(
    #[string] plugin_name: String,
    #[string] key: String,
) -> Result<serde_json::Value, PluginOpError> {
    let values = read_plugin_storage(&plugin_name)?;
    Ok(values.get(&key).cloned().unwrap_or(serde_json::Value::Null))
}

#[op2]
fn op_plugin_storage_set(
    #[string] plugin_name: String,
    #[string] key: String,
    #[serde] value: serde_json::Value,
) -> Result<(), PluginOpError> {
    let mut values = read_plugin_storage(&plugin_name)?;
    values.insert(key, value);
    write_plugin_storage(&plugin_name, &values)
}

#[op2(fast)]
fn op_plugin_storage_delete(
    #[string] plugin_name: String,
    #[string] key: String,
) -> Result<(), PluginOpError> {
    let mut values = read_plugin_storage(&plugin_name)?;
    values.remove(&key);
    write_plugin_storage(&plugin_name, &values)
}

#[op2]
fn op_create_overlay(
    #[string] id: String,
    #[serde] config: serde_json::Value,
) -> Result<(), PluginOpError> {
    use crate::plugin::{OverlayAlignment, OverlayConfig};

    let align = match config
        .get("align")
        .and_then(|a| a.as_str())
        .unwrap_or("bottom")
    {
        "top" => OverlayAlignment::Top,
        "bottom" => OverlayAlignment::Bottom,
        "avoid_cursor" => OverlayAlignment::AvoidCursor,
        _ => OverlayAlignment::Bottom,
    };

    let x_padding = config
        .get("x_padding")
        .and_then(|p| p.as_u64())
        .unwrap_or(1) as usize;

    let y_padding = config
        .get("y_padding")
        .and_then(|p| p.as_u64())
        .unwrap_or(0) as usize;

    let relative = config
        .get("relative")
        .and_then(|r| r.as_str())
        .unwrap_or("editor")
        .to_string();

    let overlay_config = OverlayConfig {
        align,
        x_padding,
        y_padding,
        relative,
    };

    ACTION_DISPATCHER.send_request(PluginRequest::CreateOverlay {
        id,
        config: overlay_config,
    });
    Ok(())
}

#[op2]
fn op_update_overlay(
    #[string] id: String,
    #[serde] lines: serde_json::Value,
) -> Result<(), PluginOpError> {
    use crate::theme::Style;

    let lines = lines
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Lines must be an array"))?;

    let mut styled_lines = Vec::new();
    for line in lines {
        let text = line
            .get("text")
            .and_then(|t| t.as_str())
            .ok_or_else(|| anyhow::anyhow!("Line must have text field"))?
            .to_string();

        let style = if let Some(style_value) = line.get("style") {
            serde_json::from_value::<Style>(style_value.clone())?
        } else {
            Style::default()
        };

        styled_lines.push((text, style));
    }

    ACTION_DISPATCHER.send_request(PluginRequest::UpdateOverlay {
        id,
        lines: styled_lines,
    });
    Ok(())
}

#[op2(fast)]
fn op_remove_overlay(#[string] id: String) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::RemoveOverlay { id });
    Ok(())
}

#[op2]
fn op_create_panel(
    #[string] id: String,
    #[serde] config: serde_json::Value,
) -> Result<(), PluginOpError> {
    let config = serde_json::from_value(config)?;
    ACTION_DISPATCHER.send_request(PluginRequest::CreatePanel { id, config });
    Ok(())
}

#[op2]
fn op_update_panel(
    #[string] id: String,
    #[serde] rows: serde_json::Value,
) -> Result<(), PluginOpError> {
    let rows = serde_json::from_value(rows)?;
    ACTION_DISPATCHER.send_request(PluginRequest::UpdatePanel { id, rows });
    Ok(())
}

#[op2(fast)]
fn op_focus_panel(#[string] id: String) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::FocusPanel { id });
    Ok(())
}

#[op2(fast)]
fn op_focus_editor() -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::FocusEditor);
    Ok(())
}

#[op2(fast)]
fn op_close_panel(#[string] id: String) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::ClosePanel { id });
    Ok(())
}

#[op2(fast)]
fn op_list_directory(#[string] path: String, request_id: i32) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::ListDirectory { path, request_id });
    Ok(())
}

#[op2(fast)]
fn op_get_git_status(#[string] path: String, request_id: i32) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::GetGitStatus { path, request_id });
    Ok(())
}

#[op2(fast)]
fn op_watch_directory(#[string] path: String, watch_id: i32) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::WatchDirectory { path, watch_id });
    Ok(())
}

#[op2(fast)]
fn op_unwatch_directory(watch_id: i32) -> Result<(), PluginOpError> {
    ACTION_DISPATCHER.send_request(PluginRequest::UnwatchDirectory { watch_id });
    Ok(())
}

fn plugin_storage_path(plugin_name: &str) -> anyhow::Result<PathBuf> {
    let safe_name: String = plugin_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if safe_name.is_empty() {
        return Err(anyhow::anyhow!("plugin name cannot be empty"));
    }
    Ok(Config::path("state")
        .join("plugins")
        .join(format!("{safe_name}.json")))
}

fn read_plugin_storage(plugin_name: &str) -> anyhow::Result<serde_json::Map<String, Value>> {
    let path = plugin_storage_path(plugin_name)?;
    if !path.exists() {
        return Ok(serde_json::Map::new());
    }
    let contents = fs::read_to_string(path)?;
    if contents.trim().is_empty() {
        return Ok(serde_json::Map::new());
    }
    let value: Value = serde_json::from_str(&contents)?;
    Ok(value.as_object().cloned().unwrap_or_default())
}

fn write_plugin_storage(
    plugin_name: &str,
    values: &serde_json::Map<String, Value>,
) -> Result<(), PluginOpError> {
    let path = plugin_storage_path(plugin_name)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(values)?)?;
    Ok(())
}

extension!(
    js_runtime,
    ops = [
        op_editor_info,
        op_open_picker,
        op_open_live_picker,
        op_spawn_process,
        op_kill_process,
        op_poll_process_events,
        op_open_location,
        op_open_dynamic_picker,
        op_update_picker_items,
        op_update_picker_query,
        op_update_picker_status,
        op_update_picker_preview,
        op_close_picker,
        op_lsp_document_symbols,
        op_lsp_workspace_symbols,
        op_lsp_references,
        op_lsp_inlay_hints,
        op_list_themes,
        op_trigger_action,
        op_log,
        op_set_timeout,
        op_clear_timeout,
        op_set_interval,
        op_clear_interval,
        op_get_interval_callback_id,
        op_buffer_insert,
        op_buffer_delete,
        op_buffer_replace,
        op_get_cursor_position,
        op_set_cursor_position,
        op_get_buffer_text,
        op_get_viewport_layout,
        op_set_decorations,
        op_clear_decorations,
        op_get_config,
        op_get_editor_state,
        op_restore_editor_state,
        op_plugin_storage_get,
        op_plugin_storage_set,
        op_plugin_storage_delete,
        op_create_overlay,
        op_update_overlay,
        op_remove_overlay,
        op_create_panel,
        op_update_panel,
        op_focus_panel,
        op_focus_editor,
        op_close_panel,
        op_list_directory,
        op_get_git_status,
        op_watch_directory,
        op_unwatch_directory,
    ],
    js = ["src/plugin/runtime.js"],
);

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::editor::Action;

    use super::*;

    #[test]
    fn list_themes_reads_display_names_from_json() -> anyhow::Result<()> {
        let temp_dir =
            std::env::temp_dir().join(format!("red-theme-list-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&temp_dir)?;
        fs::write(
            temp_dir.join("z-mocha.json"),
            r#"{
                // VS Code themes commonly allow comments.
                "name": "Catppuccin Mocha",
                "colors": {},
                "tokenColors": []
            }"#,
        )?;
        fs::write(
            temp_dir.join("a-latte.json"),
            r#"{
                "name": "Catppuccin Latte",
                "colors": {},
                "tokenColors": []
            }"#,
        )?;
        fs::write(
            temp_dir.join("unnamed.json"),
            r#"{
                "colors": {},
                "tokenColors": []
            }"#,
        )?;
        fs::write(temp_dir.join("broken.json"), "{")?;
        fs::write(temp_dir.join("notes.txt"), r#"{ "name": "Ignored" }"#)?;

        let themes = list_themes_in_dir(&temp_dir)?;

        assert_eq!(
            themes,
            vec![
                ThemeListEntry {
                    name: "broken.json".to_string(),
                    file: "broken.json".to_string(),
                },
                ThemeListEntry {
                    name: "Catppuccin Latte".to_string(),
                    file: "a-latte.json".to_string(),
                },
                ThemeListEntry {
                    name: "Catppuccin Mocha".to_string(),
                    file: "z-mocha.json".to_string(),
                },
                ThemeListEntry {
                    name: "unnamed.json".to_string(),
                    file: "unnamed.json".to_string(),
                },
            ]
        );

        fs::remove_dir_all(temp_dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn theme_browser_model_displays_names_but_returns_files() -> anyhow::Result<()> {
        let module_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plugins")
            .join("theme_browser.js");
        let module_specifier = deno_core::ModuleSpecifier::from_file_path(&module_path)
            .map_err(|_| anyhow::anyhow!("failed to create module specifier"))?;

        let mut runtime = Runtime::new();
        runtime
            .add_module(&format!(
                r#"
                    import {{ buildThemePickerModel }} from "{module_specifier}";

                    const model = buildThemePickerModel([
                        {{ name: "Catppuccin Mocha", file: "mocha.json" }},
                        {{ name: "Catppuccin Latte", file: "latte.json" }},
                        {{ name: "Duplicate", file: "one.json" }},
                        {{ name: "Duplicate", file: "two.json" }},
                        "legacy.json",
                    ]);

                    const expectedLabels = [
                        "Catppuccin Mocha",
                        "Catppuccin Latte",
                        "Duplicate (one.json)",
                        "Duplicate (two.json)",
                        "legacy.json",
                    ];
                    if (JSON.stringify(model.labels) !== JSON.stringify(expectedLabels)) {{
                        throw new Error(`unexpected labels: ${{JSON.stringify(model.labels)}}`);
                    }}
                    if (model.filesByLabel.get("Catppuccin Mocha") !== "mocha.json") {{
                        throw new Error("display name did not map back to theme file");
                    }}
                    if (model.labelsByFile.get("latte.json") !== "Catppuccin Latte") {{
                        throw new Error("theme file did not map back to display name");
                    }}
                "#
            ))
            .await
    }

    #[tokio::test]
    async fn lsp_navigation_models_filter_symbols_and_current_references() -> anyhow::Result<()> {
        let module_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plugins")
            .join("lsp_symbols.ts");
        let module_specifier = deno_core::ModuleSpecifier::from_file_path(&module_path)
            .map_err(|_| anyhow::anyhow!("failed to create module specifier"))?;

        let mut runtime = Runtime::new();
        runtime
            .add_module(&format!(
                r#"
                    import {{
                        buildReferenceItems,
                        buildWorkspaceSymbolItems,
                        isCurrentReference,
                        symbolIcon,
                    }} from "{module_specifier}";

                    const range = {{
                        start: {{ line: 3, character: 2 }},
                        end: {{ line: 3, character: 7 }},
                    }};
                    const items = buildWorkspaceSymbolItems([
                        {{
                            name: "render",
                            kind: 12,
                            kindName: "Function",
                            file: "/tmp/project/src/app.ts",
                            range,
                            selectionRange: range,
                            depth: 0,
                        }},
                        {{
                            name: "state",
                            kind: 13,
                            kindName: "Variable",
                            file: "/tmp/project/src/app.ts",
                            range,
                            selectionRange: range,
                            depth: 0,
                        }},
                    ], {{ overrides: {{ function: "FN" }} }});
                    if (items.length !== 1 || items[0].label !== "FN render") {{
                        throw new Error(`unexpected workspace items: ${{JSON.stringify(items)}}`);
                    }}
                    if (items[0].kind !== "Function" || items[0].preview.path !== "/tmp/project/src/app.ts") {{
                        throw new Error("workspace item lost kind or preview metadata");
                    }}

                    const location = {{ file: "/tmp/project/src/app.ts", range }};
                    if (!isCurrentReference(location, location.file, {{ line: 3, character: 4 }})) {{
                        throw new Error("current reference was not detected");
                    }}
                    const references = buildReferenceItems([location]);
                    if (references[0].data.location !== location || references[0].kind !== "Reference") {{
                        throw new Error("reference item lost its location or kind");
                    }}
                    if (symbolIcon("Struct", {{ enabled: false }}) !== "") {{
                        throw new Error("disabled icons should be hidden");
                    }}
                    if (symbolIcon("Struct", {{ overrides: {{ struct: "" }} }}) !== "") {{
                        throw new Error("empty per-kind overrides should hide that icon");
                    }}
                    if (symbolIcon("Struct", {{ overrides: {{ struct: "custom" }} }}) !== "custom") {{
                        throw new Error("per-kind icon override was ignored");
                    }}
                "#
            ))
            .await
    }

    #[tokio::test]
    async fn test_runtime_plugin() {
        let mut runtime = Runtime::new();
        runtime
            .add_module(
                r#"
                    console.log("Hello, world!");
                "#,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_runtime_plugin_with_import() {
        let mut runtime = Runtime::new();
        runtime
            .add_module(
                r#"
                    // Test that ES module syntax works
                    export function testFunction() {
                        return "ES modules work!";
                    }
                    
                    console.log("ES module test:", testFunction());
                "#,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_runtime_file_typescript_import_transpiles() -> anyhow::Result<()> {
        let temp_dir =
            std::env::temp_dir().join(format!("red-typescript-plugin-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&temp_dir)?;

        let module_path = temp_dir.join("typed-plugin.ts");
        fs::write(
            &module_path,
            r#"
                export type PluginInfo = {
                    name: string;
                    commandCount: number;
                };

                const plugin: PluginInfo = {
                    name: "red",
                    commandCount: 41,
                };

                export const commandCount: number = plugin.commandCount + 1;
                export default plugin.name;
            "#,
        )?;

        let module_specifier = deno_core::ModuleSpecifier::from_file_path(&module_path)
            .map_err(|_| anyhow::anyhow!("failed to create module specifier"))?;

        let mut runtime = Runtime::new();
        let result = runtime
            .add_module(&format!(
                r#"
                    import pluginName, {{ commandCount }} from "{module_specifier}";

                    if (pluginName !== "red" || commandCount !== 42) {{
                        throw new Error(`unexpected plugin export: ${{pluginName}}/${{commandCount}}`);
                    }}
                "#
            ))
            .await;

        fs::remove_dir_all(&temp_dir)?;
        result
    }

    #[tokio::test]
    async fn test_fidget_model_matches_lsp_progress_flow() -> anyhow::Result<()> {
        let module_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plugins")
            .join("fidget.js");
        let module_specifier = deno_core::ModuleSpecifier::from_file_path(&module_path)
            .map_err(|_| anyhow::anyhow!("failed to create module specifier"))?;

        let mut runtime = Runtime::new();
        runtime
            .add_module(&format!(
                r#"
                    import {{ createFidgetModel }} from "{module_specifier}";

                    const model = createFidgetModel({{
                        renderLimit: 1,
                        groupSeparator: "--",
                    }});
                    const info = {{
                        size: [80, 24],
                        theme: {{
                            style: {{}},
                            ui_style: {{
                                muted: {{ fg: "muted" }},
                                popup_title: {{ fg: "header" }},
                            }},
                        }},
                    }};

                    const ignored = model.handleProgress({{
                        token: "not-work-done",
                        value: "anything",
                    }});
                    if (!ignored.ignored) {{
                        throw new Error("malformed progress was not ignored");
                    }}

                    model.handleProgress({{
                        token: "rustAnalyzer/Indexing",
                        value: {{
                            kind: "report",
                            title: "Indexing",
                            message: "17/21 (unicode_width)",
                            percentage: 80,
                        }},
                        lspClient: {{ name: "rust-analyzer", workspaceRoot: "/repo" }},
                    }});
                    model.handleProgress({{
                        token: "rustAnalyzer/Build",
                        value: {{
                            kind: "report",
                            title: "Build",
                            message: "cargo check",
                        }},
                        lspClient: {{ name: "rust-analyzer", workspaceRoot: "/repo" }},
                    }});

                    const activeLines = model.render(info, "⠋");
                    if (activeLines[activeLines.length - 1].text !== "rust-analyzer ⠋") {{
                        throw new Error(`unexpected active header: ${{JSON.stringify(activeLines)}}`);
                    }}
                    if (!activeLines.some((line) =>
                        line.text.includes("17/21 (unicode_width) (80%)") &&
                        line.text.includes("Indexing")
                    )) {{
                        throw new Error(`missing rendered progress item: ${{JSON.stringify(activeLines)}}`);
                    }}
                    if (activeLines.some((line) => line.text.includes("cargo check"))) {{
                        throw new Error(`render limit did not hide later items: ${{JSON.stringify(activeLines)}}`);
                    }}

                    model.handleProgress({{
                        token: "rustAnalyzer/Indexing",
                        value: {{ kind: "end" }},
                        lspClient: {{ name: "rust-analyzer", workspaceRoot: "/repo" }},
                    }});
                    const doneLines = model.render(info, "⠙");
                    if (doneLines[doneLines.length - 1].text !== "rust-analyzer ⠙") {{
                        throw new Error(`group should stay active while build is running: ${{JSON.stringify(doneLines)}}`);
                    }}
                    if (!doneLines.some((line) =>
                        line.text.includes("Completed") && line.text.includes("Indexing")
                    )) {{
                        throw new Error(`missing completed item: ${{JSON.stringify(doneLines)}}`);
                    }}

                    model.remove("rust-analyzer", "rustAnalyzer/Build");
                    const doneOnlyLines = model.render(info, "⠸");
                    if (doneOnlyLines[doneOnlyLines.length - 1].text !== "rust-analyzer ✔") {{
                        throw new Error(`done-only group should show done icon: ${{JSON.stringify(doneOnlyLines)}}`);
                    }}

                    model.remove("rust-analyzer", "rustAnalyzer/Indexing");
                    if (!model.isEmpty()) {{
                        throw new Error("model should be empty after removals");
                    }}
                "#
            ))
            .await
    }

    #[tokio::test]
    async fn test_runtime_plugin_top_level_await() {
        let mut runtime = Runtime::new();
        runtime
            .add_module(
                r#"
                    globalThis.topLevelAwaitValue = await Promise.resolve(42);

                    if (globalThis.topLevelAwaitValue !== 42) {
                        throw new Error(`unexpected top-level await value: ${globalThis.topLevelAwaitValue}`);
                    }
                "#,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_runtime_timer() {
        let mut runtime = Runtime::new();
        runtime
            .add_module(
                r#"
                    globalThis.timerFired = false;
                    
                    globalThis.setTimeout(() => {
                        globalThis.timerFired = true;
                        console.log("Timer fired!");
                    }, 10).then(timerId => {
                        console.log("Timer scheduled with ID:", timerId);
                    });
                    
                    // Check that timer hasn't fired immediately
                    console.log("Timer fired immediately?", globalThis.timerFired);
                "#,
            )
            .await
            .unwrap();

        // Wait for timer to fire
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Check that the timer callback was executed
        runtime
            .run(
                r#"
                    console.log("Timer fired after delay?", globalThis.timerFired);
                "#,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_runtime_error() {
        let mut runtime = Runtime::new();
        let result = runtime
            .add_module(
                r#"
                    throw new Error("This is an error");
                "#,
            )
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("This is an error"));
    }

    #[tokio::test]
    async fn test_runtime_execute() {
        let mut runtime = Runtime::new();
        runtime
            .run(
                r#"
                    console.log("Hello, world!");
                "#,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_runtime_execute_error() {
        let mut runtime = Runtime::new();
        let result = runtime
            .run(
                r#"
                    throw new Error("This is an error");
                "#,
            )
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("This is an error"));
    }

    #[test]
    fn test_action_serialization() {
        let action = Action::MoveUp;
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(json, r#""MoveUp""#);

        let action = Action::Print("Hello, world!".to_string());
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(json, r#"{"Print":"Hello, world!"}"#);

        let action = serde_json::from_str::<Action>(r#""MoveUp""#).unwrap();
        assert_eq!(action, Action::MoveUp);

        let action = serde_json::from_str::<Action>("{\"Print\":\"Hello, world!\"}").unwrap();
        assert_eq!(action, Action::Print("Hello, world!".to_string()));
    }
}
