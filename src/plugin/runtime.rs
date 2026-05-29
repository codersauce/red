use std::{
    collections::HashMap,
    env, fs,
    path::PathBuf,
    process::Stdio,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
};

use deno_core::{
    error::AnyError, extension, op2, FastString, JsRuntime, PollEventLoopOptions, RuntimeOptions,
};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    time::timeout,
};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

use crate::{
    config::Config,
    editor::{PluginRequest, ACTION_DISPATCHER},
    log,
};

use super::loader::TsModuleLoader;

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
        let (sender, receiver) = mpsc::channel::<Task>();
        let mut n = 1;

        thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            let mut js_runtime = JsRuntime::new(RuntimeOptions {
                module_loader: Some(Rc::new(TsModuleLoader)),
                extensions: vec![js_runtime::init_ops_and_esm()],
                ..Default::default()
            });

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
                                            "Plugin error: {}",
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
                                            "Plugin error: {}",
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
            pump_v8_message_loop: true,
        })
        .await?;

    let scope = &mut runtime.handle_scope();
    // TODO: check if we'll need the return value
    let _value = result?.open(scope);

    Ok(())
}

#[op2]
fn op_editor_info(id: Option<i32>) -> Result<(), AnyError> {
    ACTION_DISPATCHER.send_request(PluginRequest::EditorInfo(id));
    Ok(())
}

#[op2]
fn op_open_picker(
    #[string] title: Option<String>,
    id: Option<i32>,
    #[serde] items: serde_json::Value,
) -> Result<(), AnyError> {
    let Value::Array(items) = items else {
        return Err(anyhow::anyhow!("Invalid items"));
    };
    ACTION_DISPATCHER.send_request(PluginRequest::OpenPicker(title, id, items));
    Ok(())
}

#[op2]
fn op_trigger_action(
    #[string] action: String,
    #[serde] params: Option<serde_json::Value>,
) -> Result<(), AnyError> {
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
    static ref CODEX_TURN_CANCELS: Mutex<HashMap<String, Arc<AtomicBool>>> = Mutex::new(HashMap::new());
    static ref CODEX_PENDING_REQUESTS: Mutex<HashMap<String, oneshot::Sender<serde_json::Value>>> = Mutex::new(HashMap::new());
    static ref CODEX_APP_SERVER_DAEMON_STARTED: AtomicBool = AtomicBool::new(false);
}

struct IntervalHandle {
    handle: tokio::task::JoinHandle<()>,
    cancel_sender: Option<tokio::sync::oneshot::Sender<()>>,
}

enum CodexAppServerClient {
    Process {
        child: tokio::process::Child,
        stdin: tokio::process::ChildStdin,
        lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    },
    WebSocket {
        stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
    },
}

impl CodexAppServerClient {
    async fn send(&mut self, message: serde_json::Value) -> Result<(), AnyError> {
        match self {
            CodexAppServerClient::Process { stdin, .. } => {
                send_app_server_process_message(stdin, message).await
            }
            CodexAppServerClient::WebSocket { stream } => {
                stream.send(Message::Text(message.to_string())).await?;
                Ok(())
            }
        }
    }

    async fn next_value(&mut self) -> Result<Option<serde_json::Value>, AnyError> {
        match self {
            CodexAppServerClient::Process { lines, .. } => {
                while let Some(line) = lines.next_line().await? {
                    if line.trim().is_empty() {
                        continue;
                    }
                    return Ok(Some(serde_json::from_str(&line)?));
                }
                Ok(None)
            }
            CodexAppServerClient::WebSocket { stream } => {
                while let Some(message) = stream.next().await {
                    match message? {
                        Message::Text(text) => return Ok(Some(serde_json::from_str(&text)?)),
                        Message::Binary(bytes) => return Ok(Some(serde_json::from_slice(&bytes)?)),
                        Message::Ping(_) | Message::Pong(_) => continue,
                        Message::Close(_) => return Ok(None),
                        Message::Frame(_) => continue,
                    }
                }
                Ok(None)
            }
        }
    }

    async fn shutdown(&mut self) {
        match self {
            CodexAppServerClient::Process { child, .. } => {
                let _ = child.kill().await;
            }
            CodexAppServerClient::WebSocket { stream } => {
                let _ = stream.close(None).await;
            }
        }
    }
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
fn op_set_timeout(delay: f64) -> Result<String, AnyError> {
    // Limit the number of concurrent timers per plugin runtime
    const MAX_TIMERS: usize = 1000;

    let mut timeouts = PENDING_TIMEOUTS.lock().unwrap();
    if timeouts.len() >= MAX_TIMERS {
        return Err(anyhow::anyhow!(
            "Too many timers, maximum {} allowed",
            MAX_TIMERS
        ));
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
fn op_clear_timeout(#[string] id: String) -> Result<(), AnyError> {
    let mut timeouts = PENDING_TIMEOUTS.lock().unwrap();
    timeouts.retain(|t| t.id != id);
    Ok(())
}

#[op2(async)]
#[string]
async fn op_set_interval(delay: f64, #[string] callback_id: String) -> Result<String, AnyError> {
    // Limit the number of concurrent timers per plugin runtime
    const MAX_TIMERS: usize = 1000;

    // Check combined limit of timeouts and intervals
    let timeout_count = PENDING_TIMEOUTS.lock().unwrap().len();
    let interval_count = INTERVALS.lock().unwrap().len();
    if timeout_count + interval_count >= MAX_TIMERS {
        return Err(anyhow::anyhow!(
            "Too many timers, maximum {} allowed",
            MAX_TIMERS
        ));
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
fn op_clear_interval(#[string] id: String) -> Result<(), AnyError> {
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
fn op_get_interval_callback_id(#[string] interval_id: String) -> Result<String, AnyError> {
    let callbacks = INTERVAL_CALLBACKS.lock().unwrap();
    callbacks
        .get(&interval_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Interval ID not found"))
}

#[op2(fast)]
fn op_buffer_insert(x: u32, y: u32, #[string] text: String) -> Result<(), AnyError> {
    ACTION_DISPATCHER.send_request(PluginRequest::BufferInsert {
        x: x as usize,
        y: y as usize,
        text,
    });
    Ok(())
}

#[op2(fast)]
fn op_buffer_delete(x: u32, y: u32, length: u32) -> Result<(), AnyError> {
    ACTION_DISPATCHER.send_request(PluginRequest::BufferDelete {
        x: x as usize,
        y: y as usize,
        length: length as usize,
    });
    Ok(())
}

#[op2(fast)]
fn op_buffer_replace(x: u32, y: u32, length: u32, #[string] text: String) -> Result<(), AnyError> {
    ACTION_DISPATCHER.send_request(PluginRequest::BufferReplace {
        x: x as usize,
        y: y as usize,
        length: length as usize,
        text,
    });
    Ok(())
}

#[op2(fast)]
fn op_get_cursor_position() -> Result<(), AnyError> {
    ACTION_DISPATCHER.send_request(PluginRequest::GetCursorPosition);
    Ok(())
}

#[op2(fast)]
fn op_set_cursor_position(x: u32, y: u32) -> Result<(), AnyError> {
    ACTION_DISPATCHER.send_request(PluginRequest::SetCursorPosition {
        x: x as usize,
        y: y as usize,
    });
    Ok(())
}

#[op2]
fn op_get_buffer_text(start_line: Option<u32>, end_line: Option<u32>) -> Result<(), AnyError> {
    ACTION_DISPATCHER.send_request(PluginRequest::GetBufferText {
        start_line: start_line.map(|l| l as usize),
        end_line: end_line.map(|l| l as usize),
    });
    Ok(())
}

#[op2]
fn op_get_config(#[string] key: Option<String>) -> Result<(), AnyError> {
    ACTION_DISPATCHER.send_request(PluginRequest::GetConfig { key });
    Ok(())
}

#[op2(fast)]
fn op_get_editor_state(request_id: i32) -> Result<(), AnyError> {
    ACTION_DISPATCHER.send_request(PluginRequest::GetEditorState { request_id });
    Ok(())
}

#[op2]
fn op_restore_editor_state(
    request_id: i32,
    #[serde] snapshot: serde_json::Value,
) -> Result<(), AnyError> {
    let snapshot = serde_json::from_value(snapshot)?;
    ACTION_DISPATCHER.send_request(PluginRequest::RestoreEditorState {
        request_id,
        snapshot,
    });
    Ok(())
}

#[op2(async)]
#[serde]
async fn op_codex_app_server_request(
    #[string] method: String,
    #[serde] mut params: serde_json::Value,
) -> Result<serde_json::Value, AnyError> {
    let endpoint = take_codex_app_server_endpoint(&mut params);
    let mut client = open_codex_app_server_client(endpoint.as_deref()).await?;

    let initialize = json!({
        "method": "initialize",
        "id": 0,
        "params": {
            "clientInfo": {
                "name": "red_codex_plugin",
                "title": "Red Codex Plugin",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    });
    client.send(initialize).await?;

    timeout(
        Duration::from_secs(30),
        read_app_server_response(&mut client, 0),
    )
    .await
    .map_err(|_| anyhow::anyhow!("codex app-server initialize timed out"))??;

    let initialized = json!({ "method": "initialized", "params": {} });
    let request = json!({ "method": method, "id": 1, "params": params });

    for message in [initialized, request] {
        client.send(message).await?;
    }

    let response = timeout(
        Duration::from_secs(30),
        read_app_server_response(&mut client, 1),
    )
    .await
    .map_err(|_| anyhow::anyhow!("codex app-server request timed out"))??;

    client.shutdown().await;
    Ok(response)
}

#[op2(async)]
#[serde]
async fn op_codex_app_server_run_turn(
    #[serde] params: serde_json::Value,
) -> Result<serde_json::Value, AnyError> {
    run_codex_turn_inner(params, None, None).await
}

async fn run_codex_turn_inner(
    mut params: serde_json::Value,
    stream: Option<(&str, &str)>,
    cancel_flag: Option<Arc<AtomicBool>>,
) -> Result<serde_json::Value, AnyError> {
    let endpoint = take_codex_app_server_endpoint(&mut params);
    let prompt = params
        .get("prompt")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow::anyhow!("codex run turn requires `prompt`"))?;
    let cwd = params.get("cwd").and_then(|value| value.as_str());
    let runtime_workspace_roots = params.get("runtimeWorkspaceRoots");
    let existing_thread_id = params.get("threadId").and_then(|value| value.as_str());

    let mut client = open_codex_app_server_client(endpoint.as_deref()).await?;

    client
        .send(json!({
            "method": "initialize",
            "id": 0,
            "params": {
                "clientInfo": {
                    "name": "red_codex_plugin",
                    "title": "Red Codex Plugin",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        }))
        .await?;
    timeout(
        Duration::from_secs(30),
        read_app_server_response(&mut client, 0),
    )
    .await
    .map_err(|_| anyhow::anyhow!("codex app-server initialize timed out"))??;

    client
        .send(json!({ "method": "initialized", "params": {} }))
        .await?;

    let thread_method = if existing_thread_id.is_some() {
        "thread/resume"
    } else {
        "thread/start"
    };
    let mut thread_params = serde_json::Map::new();
    if let Some(thread_id) = existing_thread_id {
        thread_params.insert("threadId".to_string(), json!(thread_id));
    }
    if let Some(cwd) = cwd {
        thread_params.insert("cwd".to_string(), json!(cwd));
    }
    if let Some(runtime_workspace_roots) = runtime_workspace_roots {
        thread_params.insert(
            "runtimeWorkspaceRoots".to_string(),
            runtime_workspace_roots.clone(),
        );
    }

    client
        .send(json!({
            "method": thread_method,
            "id": 1,
            "params": thread_params,
        }))
        .await?;
    let thread_response = timeout(
        Duration::from_secs(30),
        read_app_server_response(&mut client, 1),
    )
    .await
    .map_err(|_| anyhow::anyhow!("codex app-server thread bootstrap timed out"))??;
    let thread = thread_response
        .get("thread")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("codex app-server thread response omitted `thread`"))?;
    let thread_id = thread
        .get("id")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow::anyhow!("codex app-server thread response omitted `thread.id`"))?;
    if let Some((event, stream_id)) = stream {
        ACTION_DISPATCHER.send_request(PluginRequest::NotifyPlugins {
            event: event.to_string(),
            payload: json!({
                "streamId": stream_id,
                "kind": "thread",
                "thread": thread.clone(),
            }),
        });
    }

    let mut turn_params = serde_json::Map::new();
    turn_params.insert("threadId".to_string(), json!(thread_id));
    turn_params.insert(
        "input".to_string(),
        json!([{ "type": "text", "text": prompt, "text_elements": [] }]),
    );
    if let Some(cwd) = cwd {
        turn_params.insert("cwd".to_string(), json!(cwd));
    }
    if let Some(runtime_workspace_roots) = runtime_workspace_roots {
        turn_params.insert(
            "runtimeWorkspaceRoots".to_string(),
            runtime_workspace_roots.clone(),
        );
    }
    if let Some(additional_context) = params.get("additionalContext") {
        turn_params.insert("additionalContext".to_string(), additional_context.clone());
    }
    turn_params.insert("approvalPolicy".to_string(), json!("never"));

    client
        .send(json!({
            "method": "turn/start",
            "id": 2,
            "params": turn_params,
        }))
        .await?;
    let turn_response = timeout(
        Duration::from_secs(30),
        read_app_server_response(&mut client, 2),
    )
    .await
    .map_err(|_| anyhow::anyhow!("codex app-server turn start timed out"))??;
    if let Some((event, stream_id)) = stream {
        ACTION_DISPATCHER.send_request(PluginRequest::NotifyPlugins {
            event: event.to_string(),
            payload: json!({
                "streamId": stream_id,
                "kind": "turn",
                "turn": turn_response.get("turn").cloned().unwrap_or_else(|| turn_response.clone()),
            }),
        });
    }
    let turn = turn_response
        .get("turn")
        .cloned()
        .unwrap_or_else(|| turn_response.clone());
    let turn_id = turn
        .get("id")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();

    let mut notifications = Vec::new();
    let mut agent_text = String::new();
    let mut interrupt_sent = false;
    timeout(Duration::from_secs(300), async {
        loop {
            if !interrupt_sent
                && cancel_flag
                    .as_ref()
                    .is_some_and(|flag| flag.swap(false, Ordering::SeqCst))
            {
                client
                    .send(json!({
                        "method": "turn/interrupt",
                        "id": 3,
                        "params": {
                            "threadId": thread_id,
                            "turnId": turn_id.clone(),
                        }
                    }))
                    .await?;
                interrupt_sent = true;
                if let Some((event, stream_id)) = stream {
                    ACTION_DISPATCHER.send_request(PluginRequest::NotifyPlugins {
                        event: event.to_string(),
                        payload: json!({
                            "streamId": stream_id,
                            "kind": "cancelled",
                        }),
                    });
                }
            }

            let value = match timeout(Duration::from_millis(100), client.next_value()).await {
                Ok(value_result) => match value_result? {
                    Some(value) => value,
                    None => {
                        return Err(anyhow::anyhow!(
                            "codex app-server exited before turn completed"
                        ));
                    }
                },
                Err(_) => continue,
            };
            if is_app_server_request(&value) {
                let Some((event, stream_id)) = stream else {
                    return Err(anyhow::anyhow!(
                        "codex app-server requested interactive input without a streaming handler"
                    ));
                };
                handle_app_server_request(&mut client, event, stream_id, value).await?;
                continue;
            }
            if let Some((event, stream_id)) = stream {
                ACTION_DISPATCHER.send_request(PluginRequest::NotifyPlugins {
                    event: event.to_string(),
                    payload: json!({
                        "streamId": stream_id,
                        "kind": "notification",
                        "notification": value.clone(),
                    }),
                });
            }
            let method = value.get("method").and_then(|method| method.as_str());
            if let Some("item/agentMessage/delta") = method {
                if let Some(delta) = value
                    .get("params")
                    .and_then(|params| params.get("delta"))
                    .and_then(|delta| delta.as_str())
                {
                    agent_text.push_str(delta);
                }
            }
            if let Some("item/completed") = method {
                if let Some(item) = value.get("params").and_then(|params| params.get("item")) {
                    if item.get("type").and_then(|kind| kind.as_str()) == Some("agentMessage") {
                        if let Some(text) = item.get("text").and_then(|text| text.as_str()) {
                            agent_text = text.to_string();
                        }
                    }
                }
            }
            let completed = method == Some("turn/completed");
            notifications.push(value);
            if completed {
                return Ok::<(), AnyError>(());
            }
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("codex app-server turn timed out"))??;

    client.shutdown().await;
    Ok(json!({
        "thread": thread,
        "turn": turn,
        "agentText": agent_text,
        "notifications": notifications,
    }))
}

fn is_app_server_request(value: &serde_json::Value) -> bool {
    value.get("id").is_some()
        && value
            .get("method")
            .and_then(|method| method.as_str())
            .is_some_and(is_interactive_app_server_request_method)
        && value.get("result").is_none()
        && value.get("error").is_none()
}

fn is_interactive_app_server_request_method(method: &str) -> bool {
    matches!(
        method,
        "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "item/permissions/requestApproval"
            | "item/tool/requestUserInput"
    )
}

async fn handle_app_server_request(
    client: &mut CodexAppServerClient,
    event: &str,
    stream_id: &str,
    request: serde_json::Value,
) -> Result<(), AnyError> {
    let request_id = request
        .get("id")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("codex app-server request omitted `id`"))?;
    let method = request
        .get("method")
        .and_then(|method| method.as_str())
        .ok_or_else(|| anyhow::anyhow!("codex app-server request omitted `method`"))?
        .to_string();
    let params = request
        .get("params")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let (sender, receiver) = oneshot::channel();
    let key = codex_pending_request_key(stream_id, &request_id);
    CODEX_PENDING_REQUESTS
        .lock()
        .unwrap()
        .insert(key.clone(), sender);
    ACTION_DISPATCHER.send_request(PluginRequest::NotifyPlugins {
        event: event.to_string(),
        payload: json!({
            "streamId": stream_id,
            "kind": "request",
            "requestId": request_id.clone(),
            "method": method,
            "params": params,
        }),
    });

    let response = match timeout(Duration::from_secs(300), receiver).await {
        Ok(Ok(response)) => response,
        Ok(Err(_)) => {
            CODEX_PENDING_REQUESTS.lock().unwrap().remove(&key);
            return Err(anyhow::anyhow!(
                "codex app-server request response was dropped"
            ));
        }
        Err(_) => {
            CODEX_PENDING_REQUESTS.lock().unwrap().remove(&key);
            return Err(anyhow::anyhow!("codex app-server request timed out"));
        }
    };

    client
        .send(json!({
            "id": request_id,
            "result": response,
        }))
        .await
}

fn codex_pending_request_key(stream_id: &str, request_id: &serde_json::Value) -> String {
    format!("{stream_id}:{}", request_id)
}

async fn open_codex_app_server_client(
    endpoint: Option<&str>,
) -> Result<CodexAppServerClient, AnyError> {
    if let Some(endpoint) = endpoint.filter(|endpoint| !endpoint.trim().is_empty()) {
        if endpoint.starts_with("ws://") {
            let (stream, _) = connect_async(endpoint).await?;
            return Ok(CodexAppServerClient::WebSocket { stream });
        }
        return Err(anyhow::anyhow!(
            "unsupported codex app-server endpoint `{endpoint}`; expected ws://"
        ));
    }

    let mut child = spawn_codex_app_server_process().await?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to open codex app-server stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to open codex app-server stdout"))?;
    Ok(CodexAppServerClient::Process {
        child,
        stdin,
        lines: BufReader::new(stdout).lines(),
    })
}

async fn spawn_codex_app_server_process() -> Result<tokio::process::Child, AnyError> {
    let use_daemon = ensure_codex_app_server_daemon().await;
    let mut command = Command::new("codex");
    if use_daemon {
        command.args(["app-server", "proxy"]);
    } else {
        command.arg("app-server");
    }

    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .or_else(|error| {
            if !use_daemon {
                return Err(anyhow::anyhow!(
                    "failed to start `codex app-server`: {error}"
                ));
            }

            CODEX_APP_SERVER_DAEMON_STARTED.store(false, Ordering::SeqCst);
            log!("failed to start codex app-server proxy: {error}; falling back to stdio");
            Command::new("codex")
                .arg("app-server")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .map_err(|fallback_error| {
                    anyhow::anyhow!(
                        "failed to start `codex app-server proxy`: {error}; fallback \
                         `codex app-server` failed: {fallback_error}"
                    )
                })
        })
}

async fn ensure_codex_app_server_daemon() -> bool {
    if CODEX_APP_SERVER_DAEMON_STARTED.load(Ordering::SeqCst) {
        return true;
    }

    let start = Command::new("codex")
        .args(["app-server", "daemon", "start"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output();

    match timeout(Duration::from_secs(10), start).await {
        Ok(Ok(output)) if output.status.success() => {
            CODEX_APP_SERVER_DAEMON_STARTED.store(true, Ordering::SeqCst);
            true
        }
        Ok(Ok(output)) => {
            log!(
                "codex app-server daemon start exited with status {}; falling back to stdio",
                output.status
            );
            false
        }
        Ok(Err(error)) => {
            log!("failed to start codex app-server daemon: {error}; falling back to stdio");
            false
        }
        Err(_) => {
            log!("timed out starting codex app-server daemon; falling back to stdio");
            false
        }
    }
}

#[op2]
#[string]
fn op_codex_app_server_start_turn(
    #[string] event: String,
    #[serde] params: serde_json::Value,
) -> Result<String, AnyError> {
    let stream_id = Uuid::new_v4().to_string();
    let stream_id_for_thread = stream_id.clone();
    let event_for_error = event.clone();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    CODEX_TURN_CANCELS
        .lock()
        .unwrap()
        .insert(stream_id.clone(), Arc::clone(&cancel_flag));
    thread::spawn(move || {
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| anyhow::anyhow!("failed to start codex app-server runtime: {error}"))
            .and_then(|runtime| {
                runtime.block_on(run_codex_turn_streaming(
                    event,
                    stream_id_for_thread.clone(),
                    params,
                    cancel_flag,
                ))
            });
        CODEX_TURN_CANCELS
            .lock()
            .unwrap()
            .remove(&stream_id_for_thread);

        if let Err(error) = result {
            ACTION_DISPATCHER.send_request(PluginRequest::NotifyPlugins {
                event: event_for_error,
                payload: json!({
                    "streamId": stream_id_for_thread,
                    "kind": "error",
                    "error": error.to_string(),
                }),
            });
        }
    });

    Ok(stream_id)
}

#[op2(fast)]
fn op_codex_app_server_cancel_turn(#[string] stream_id: String) -> bool {
    if let Some(cancel_flag) = CODEX_TURN_CANCELS.lock().unwrap().get(&stream_id) {
        cancel_flag.store(true, Ordering::SeqCst);
        true
    } else {
        false
    }
}

#[op2]
fn op_codex_app_server_resolve_request(
    #[string] stream_id: String,
    #[serde] request_id: serde_json::Value,
    #[serde] response: serde_json::Value,
) -> bool {
    let key = codex_pending_request_key(&stream_id, &request_id);
    CODEX_PENDING_REQUESTS
        .lock()
        .unwrap()
        .remove(&key)
        .is_some_and(|sender| sender.send(response).is_ok())
}

async fn run_codex_turn_streaming(
    event: String,
    stream_id: String,
    params: serde_json::Value,
    cancel_flag: Arc<AtomicBool>,
) -> Result<(), AnyError> {
    let mut result =
        run_codex_turn_inner(params, Some((&event, &stream_id)), Some(cancel_flag)).await?;
    result
        .as_object_mut()
        .map(|object| object.insert("streamId".to_string(), json!(stream_id.clone())));
    ACTION_DISPATCHER.send_request(PluginRequest::NotifyPlugins {
        event,
        payload: json!({
            "streamId": stream_id,
            "kind": "completed",
            "result": result,
        }),
    });
    Ok(())
}

fn take_codex_app_server_endpoint(params: &mut serde_json::Value) -> Option<String> {
    params
        .as_object_mut()
        .and_then(|object| {
            object
                .remove("appServerEndpoint")
                .or_else(|| object.remove("app_server_endpoint"))
        })
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .or_else(|| env::var("RED_CODEX_APP_SERVER_ENDPOINT").ok())
}

async fn send_app_server_process_message(
    stdin: &mut tokio::process::ChildStdin,
    message: serde_json::Value,
) -> Result<(), AnyError> {
    stdin.write_all(message.to_string().as_bytes()).await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await?;
    Ok(())
}

async fn read_app_server_response(
    client: &mut CodexAppServerClient,
    id: i64,
) -> Result<serde_json::Value, AnyError> {
    while let Some(value) = client.next_value().await? {
        if value.get("id").and_then(|value| value.as_i64()) != Some(id) {
            continue;
        }

        if let Some(error) = value.get("error") {
            return Err(anyhow::anyhow!("codex app-server error: {error}"));
        }

        return Ok(value
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null));
    }

    Err(anyhow::anyhow!("codex app-server exited before responding"))
}

#[op2]
#[serde]
fn op_plugin_storage_get(
    #[string] plugin_name: String,
    #[string] key: String,
) -> Result<serde_json::Value, AnyError> {
    let values = read_plugin_storage(&plugin_name)?;
    Ok(values.get(&key).cloned().unwrap_or(serde_json::Value::Null))
}

#[op2]
fn op_plugin_storage_set(
    #[string] plugin_name: String,
    #[string] key: String,
    #[serde] value: serde_json::Value,
) -> Result<(), AnyError> {
    let mut values = read_plugin_storage(&plugin_name)?;
    values.insert(key, value);
    write_plugin_storage(&plugin_name, &values)
}

#[op2(fast)]
fn op_plugin_storage_delete(
    #[string] plugin_name: String,
    #[string] key: String,
) -> Result<(), AnyError> {
    let mut values = read_plugin_storage(&plugin_name)?;
    values.remove(&key);
    write_plugin_storage(&plugin_name, &values)
}

#[op2]
fn op_create_overlay(
    #[string] id: String,
    #[serde] config: serde_json::Value,
) -> Result<(), AnyError> {
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
) -> Result<(), AnyError> {
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
fn op_remove_overlay(#[string] id: String) -> Result<(), AnyError> {
    ACTION_DISPATCHER.send_request(PluginRequest::RemoveOverlay { id });
    Ok(())
}

#[op2]
fn op_create_panel(
    #[string] id: String,
    #[serde] config: serde_json::Value,
) -> Result<(), AnyError> {
    let config = serde_json::from_value(config)?;
    ACTION_DISPATCHER.send_request(PluginRequest::CreatePanel { id, config });
    Ok(())
}

#[op2]
fn op_update_panel(#[string] id: String, #[serde] rows: serde_json::Value) -> Result<(), AnyError> {
    let rows = serde_json::from_value(rows)?;
    ACTION_DISPATCHER.send_request(PluginRequest::UpdatePanel { id, rows });
    Ok(())
}

#[op2(fast)]
fn op_focus_panel(#[string] id: String) -> Result<(), AnyError> {
    ACTION_DISPATCHER.send_request(PluginRequest::FocusPanel { id });
    Ok(())
}

#[op2(fast)]
fn op_focus_editor() -> Result<(), AnyError> {
    ACTION_DISPATCHER.send_request(PluginRequest::FocusEditor);
    Ok(())
}

#[op2(fast)]
fn op_close_panel(#[string] id: String) -> Result<(), AnyError> {
    ACTION_DISPATCHER.send_request(PluginRequest::ClosePanel { id });
    Ok(())
}

#[op2]
fn op_create_plugin_window(
    #[string] plugin: String,
    #[string] id: String,
    #[serde] config: serde_json::Value,
) -> Result<(), AnyError> {
    let title = config
        .get("title")
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned);
    ACTION_DISPATCHER.send_request(PluginRequest::CreatePluginWindow { plugin, id, title });
    Ok(())
}

#[op2(fast)]
fn op_focus_plugin_window(#[string] plugin: String, #[string] id: String) -> Result<(), AnyError> {
    ACTION_DISPATCHER.send_request(PluginRequest::FocusPluginWindow { plugin, id });
    Ok(())
}

#[op2]
fn op_update_plugin_window(
    #[string] plugin: String,
    #[string] id: String,
    #[serde] render_state: serde_json::Value,
) -> Result<(), AnyError> {
    let render_state = serde_json::from_value(render_state)?;
    ACTION_DISPATCHER.send_request(PluginRequest::UpdatePluginWindow {
        plugin,
        id,
        render_state,
    });
    Ok(())
}

#[op2(fast)]
fn op_close_plugin_window(#[string] plugin: String, #[string] id: String) -> Result<(), AnyError> {
    ACTION_DISPATCHER.send_request(PluginRequest::ClosePluginWindow { plugin, id });
    Ok(())
}

#[op2(fast)]
fn op_list_directory(#[string] path: String, request_id: i32) -> Result<(), AnyError> {
    ACTION_DISPATCHER.send_request(PluginRequest::ListDirectory { path, request_id });
    Ok(())
}

#[op2(fast)]
fn op_watch_directory(#[string] path: String, watch_id: i32) -> Result<(), AnyError> {
    ACTION_DISPATCHER.send_request(PluginRequest::WatchDirectory { path, watch_id });
    Ok(())
}

#[op2(fast)]
fn op_unwatch_directory(watch_id: i32) -> Result<(), AnyError> {
    ACTION_DISPATCHER.send_request(PluginRequest::UnwatchDirectory { watch_id });
    Ok(())
}

#[op2(async)]
#[serde]
async fn op_get_git_diff(#[string] cwd: String) -> Result<serde_json::Value, AnyError> {
    let output = timeout(
        Duration::from_secs(10),
        Command::new("git")
            .args(["-C", &cwd, "diff", "--no-ext-diff", "HEAD", "--"])
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("git diff timed out"))?
    .map_err(|error| anyhow::anyhow!("failed to run git diff: {error}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Ok(json!({
            "text": "",
            "error": if stderr.is_empty() {
                format!("git diff exited with status {}", output.status)
            } else {
                stderr
            },
        }));
    }

    Ok(json!({
        "text": String::from_utf8_lossy(&output.stdout).to_string(),
        "error": null,
    }))
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
) -> Result<(), AnyError> {
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
        op_get_config,
        op_get_editor_state,
        op_restore_editor_state,
        op_codex_app_server_request,
        op_codex_app_server_run_turn,
        op_codex_app_server_start_turn,
        op_codex_app_server_cancel_turn,
        op_codex_app_server_resolve_request,
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
        op_create_plugin_window,
        op_focus_plugin_window,
        op_update_plugin_window,
        op_close_plugin_window,
        op_list_directory,
        op_watch_directory,
        op_unwatch_directory,
        op_get_git_diff,
    ],
    js = ["src/plugin/runtime.js"],
);

#[cfg(test)]
mod tests {
    use crate::editor::Action;

    use super::*;

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
    async fn test_runtime_command_metadata() {
        let mut runtime = Runtime::new();
        runtime
            .run(
                r#"
                    const plugin = globalThis.createPluginContext("codex");
                    plugin.addCommand("codex.open", () => "ok", {
                        title: "Open Codex Chat",
                        category: "Codex",
                        description: "Open or focus the Codex chat window.",
                    });

                    const metadata = globalThis.context.getCommandMetadata("codex.open");
                    if (metadata.owner !== "codex") {
                        throw new Error(`Expected owner codex, got ${metadata.owner}`);
                    }
                    if (metadata.title !== "Open Codex Chat") {
                        throw new Error(`Expected command title, got ${metadata.title}`);
                    }
                    if (!globalThis.context.getCommandsDetailed()["codex.open"]) {
                        throw new Error("Expected command in detailed command list");
                    }
                    if (typeof globalThis.context.codex.startTurn !== "function") {
                        throw new Error("Expected red.codex namespace on plugin context");
                    }
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
