use std::{env, rc::Rc, sync::mpsc, thread};

use deno_ast::{MediaType, ParseParams, SourceTextInfo};
use deno_core::{
    error::AnyError, extension, futures::FutureExt, op2, url::Url, FastString, JsRuntime,
    ModuleLoadResponse, ModuleLoader, ModuleSource, ModuleSourceCode, ModuleSpecifier,
    PollEventLoopOptions, RequestedModuleType, ResolutionKind, RuntimeOptions,
};
use serde_json::json;
use tokio::sync::oneshot;

use crate::{
    editor::{PluginRequest, ACTION_DISPATCHER},
    log,
};

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

            let _ = for task in receiver {
                let res: anyhow::Result<()> = runtime.block_on(async {
                    match task {
                        Task::LoadModule { code, responder } => {
                            let specifier = Url::parse(&format!("file:///module-{n}.ts"))?;
                            n += 1;
                            log!("Code: {}", code.get(0..100).unwrap_or(&code));
                            let mod_id = js_runtime
                                .load_main_module(&specifier, Some(code.into()))
                                .await?;
                            log!("Loaded module: {}", mod_id);
                            let result = js_runtime.mod_evaluate(mod_id);
                            log!("Running event loop");

                            js_runtime
                                .run_event_loop(PollEventLoopOptions::default())
                                .await?;
                            log!("Event loop done");

                            result.await?;
                            log!("Module evaluated");
                            responder.send(Ok(())).unwrap();
                        }
                        Task::Execute { code, responder } => {
                            let start = std::time::Instant::now();
                            let code: FastString = code.into();
                            js_runtime.execute_script("<anon>", code)?;
                            log!("Script executed in {:?}", start.elapsed());
                            responder.send(Ok(())).unwrap();
                        }
                    }
                    log!("Done with code");
                    Ok(())
                });
                log!("response: {:?}", res);
            };
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
        serde_json::from_str(&action)?
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
                .map(|m| m.as_str().unwrap_or("???"))
                .collect::<Vec<_>>();
            log!("{}", arr.join(" "));
        }
        _ => log!("{:?}", msg),
    }
}

extension!(
    js_runtime,
    ops = [op_trigger_action, op_log],
    js = ["src/plugin/runtime.js"],
);

struct TsModuleLoader;

impl ModuleLoader for TsModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        _kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, AnyError> {
        deno_core::resolve_import(specifier, referrer).map_err(|e| e.into())
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        _maybe_referrer: Option<&ModuleSpecifier>,
        _is_dyn_import: bool,
        _requested_module_type: RequestedModuleType,
    ) -> ModuleLoadResponse {
        let module_specifier = module_specifier.clone();
        ModuleLoadResponse::Async(
            async move {
                let path = match module_specifier.to_file_path() {
                    Ok(path) => path,
                    Err(e) => {
                        return Err(anyhow::anyhow!(
                            "Cannot convert module specifier to file path: {:?}",
                            e
                        ));
                    }
                };

                // Determine what the MediaType is (this is done based on the file
                // extension) and whether transpiling is required.
                let media_type = MediaType::from_path(&path);
                let (module_type, should_transpile) = match MediaType::from_path(&path) {
                    MediaType::JavaScript | MediaType::Mjs | MediaType::Cjs => {
                        (deno_core::ModuleType::JavaScript, false)
                    }
                    MediaType::Jsx => (deno_core::ModuleType::JavaScript, true),
                    MediaType::TypeScript
                    | MediaType::Mts
                    | MediaType::Cts
                    | MediaType::Dts
                    | MediaType::Dmts
                    | MediaType::Dcts
                    | MediaType::Tsx => (deno_core::ModuleType::JavaScript, true),
                    MediaType::Json => (deno_core::ModuleType::Json, false),
                    _ => panic!("Unknown extension {:?}", path.extension()),
                };

                // Read the file, transpile if necessary.
                let code = std::fs::read_to_string(&path)?;
                let code = if should_transpile {
                    let parsed = deno_ast::parse_module(ParseParams {
                        specifier: module_specifier.to_string(),
                        text_info: SourceTextInfo::from_string(code),
                        media_type,
                        capture_tokens: false,
                        scope_analysis: false,
                        maybe_syntax: None,
                    })?;
                    parsed.transpile(&Default::default())?.text
                } else {
                    code
                };

                // Load and return module.
                let module = ModuleSource::new(
                    module_type,
                    ModuleSourceCode::String(code.into()),
                    &Url::parse(&module_specifier.to_string())?,
                );

                Ok(module)
            }
            .boxed_local(),
        )
    }
}

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

    #[test]
    fn test_action_serialization() {
        let action = Action::MoveUp;
        let json = serde_json::to_string(&action).unwrap();
        println!("{}", json);

        let action = Action::Print("Hello, world!".to_string());
        let json = serde_json::to_string(&action).unwrap();
        println!("{}", json);

        let action = serde_json::from_str::<Action>(r#""MoveUp""#).unwrap();
        println!("{:?}", action);

        let action = serde_json::from_str::<Action>("{\"Print\":\"Hello, world!\"}").unwrap();
        println!("{:?}", action);
    }
}
