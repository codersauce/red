use std::{env, rc::Rc, sync::mpsc, thread};

use deno_core::{
    error::AnyError, extension, op2, url::Url, FastString, JsRuntime, PollEventLoopOptions,
    RuntimeOptions,
};
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::{
    editor::{PluginRequest, ACTION_DISPATCHER},
    log,
};

use super::loader::TsModuleLoader;

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
                                    responder.send(Err(e)).unwrap();
                                }
                            }
                        }
                        Task::Execute { code, responder } => {
                            match run(&mut js_runtime, code).await {
                                Ok(_) => {
                                    responder.send(Ok(())).unwrap();
                                }
                                Err(e) => {
                                    responder.send(Err(e)).unwrap();
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
    let value = runtime
        .with_event_loop_promise(
            Box::pin(async move { result }),
            PollEventLoopOptions::default(),
        )
        .await?;
    let scope = &mut runtime.handle_scope();
    // TODO: check if we'll need the return value
    let _value = value.open(scope);

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

extension!(
    js_runtime,
    ops = [op_editor_info, op_open_picker, op_trigger_action, op_log],
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
