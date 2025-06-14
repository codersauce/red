use std::{
    collections::HashMap,
    env,
    rc::Rc,
    sync::{mpsc, Mutex},
    thread,
};

use deno_core::{
    error::AnyError, extension, op2, url::Url, FastString, JsRuntime, PollEventLoopOptions,
    RuntimeOptions,
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
                let location = if let (Some(line), Some(column)) = (frame.line_number, frame.column_number) {
                    format!("{}:{}:{}", 
                        frame.file_name.as_deref().unwrap_or("<anonymous>"),
                        line,
                        column
                    )
                } else {
                    frame.file_name.as_deref().unwrap_or("<anonymous>").to_string()
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
                                    responder.send(Err(anyhow::anyhow!("Plugin error: {}", formatted_error))).unwrap();
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
                                    responder.send(Err(anyhow::anyhow!("Plugin error: {}", formatted_error))).unwrap();
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
    let specifier = Url::parse(name)?;
    let mod_id = js_runtime
        .load_main_module(&specifier, Some(code.into()))
        .await?;
    let result = js_runtime.mod_evaluate(mod_id);

    js_runtime
        .run_event_loop(PollEventLoopOptions::default())
        .await?;

    result.await?;

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
fn op_log(#[serde] msg: serde_json::Value) {
    match msg {
        serde_json::Value::String(s) => log!("{}", s),
        serde_json::Value::Array(arr) => {
            let arr = arr
                .iter()
                .map(|m| match m {
                    serde_json::Value::String(s) => s.to_string(),
                    _ => format!("{:?}", m),
                })
                .collect::<Vec<_>>();
            log!("{}", arr.join(" "));
        }
        _ => log!("{:?}", msg),
    }
}

lazy_static::lazy_static! {
    static ref TIMEOUTS: Mutex<HashMap<String, tokio::task::JoinHandle<()>>> = Mutex::new(HashMap::new());
}

#[op2(async)]
#[string]
async fn op_set_timeout(delay: f64) -> Result<String, AnyError> {
    // Limit the number of concurrent timers per plugin runtime
    const MAX_TIMERS: usize = 1000;
    
    let mut timeouts = TIMEOUTS.lock().unwrap();
    if timeouts.len() >= MAX_TIMERS {
        return Err(anyhow::anyhow!("Too many timers, maximum {} allowed", MAX_TIMERS));
    }
    
    let id = Uuid::new_v4().to_string();
    let id_clone = id.clone();
    let handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(delay as u64)).await;
        // Clean up the handle from the map after completion
        TIMEOUTS.lock().unwrap().remove(&id_clone);
    });
    timeouts.insert(id.clone(), handle);
    Ok(id)
}

#[op2(fast)]
fn op_clear_timeout(#[string] id: String) -> Result<(), AnyError> {
    if let Some(handle) = TIMEOUTS.lock().unwrap().remove(&id) {
        handle.abort();
    }
    Ok(())
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

extension!(
    js_runtime,
    ops = [
        op_editor_info,
        op_open_picker,
        op_trigger_action,
        op_log,
        op_set_timeout,
        op_clear_timeout,
        op_buffer_insert,
        op_buffer_delete,
        op_buffer_replace,
        op_get_cursor_position,
        op_set_cursor_position,
        op_get_buffer_text,
        op_get_config,
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
