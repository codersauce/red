use std::{
    collections::HashMap,
    env,
    rc::Rc,
    sync::{mpsc, Mutex},
    thread,
};

use deno_core::{
    error::AnyError, extension, op2, FastString, JsRuntime, PollEventLoopOptions, RuntimeOptions,
};
use serde_json::{json, Value};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::{
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
        op_create_overlay,
        op_update_overlay,
        op_remove_overlay,
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
