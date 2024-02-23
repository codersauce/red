use std::{env, rc::Rc, sync::mpsc, thread};

use deno_ast::{MediaType, ParseParams, SourceTextInfo};
use deno_core::{
    error::AnyError, extension, futures::FutureExt, op2, url::Url, JsRuntime, ModuleLoadResponse,
    ModuleLoader, ModuleSource, ModuleSourceCode, ModuleSpecifier, PollEventLoopOptions,
    RequestedModuleType, ResolutionKind, RuntimeOptions,
};
use tokio::sync::oneshot;

struct Task {
    code: String,
    responder: oneshot::Sender<Result<(), AnyError>>,
}

pub struct Runtime {
    sender: mpsc::Sender<Task>,
}

impl Runtime {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel::<Task>();

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
                runtime.block_on(async {
                    let specifier = Url::parse("file:///main.ts")?;
                    let code: String = task.code.to_string();
                    let mod_id = js_runtime
                        .load_main_module(&specifier, Some(code.into()))
                        .await?;
                    let result = js_runtime.mod_evaluate(mod_id);
                    js_runtime
                        .run_event_loop(PollEventLoopOptions::default())
                        .await?;
                    result.await?;

                    let _ = task.responder.send(Ok(()));

                    Ok::<(), AnyError>(())
                });
            }
        });

        Runtime { sender }
    }

    pub async fn run(&mut self, code: &str) -> Result<(), AnyError> {
        let (tx, rx) = oneshot::channel();
        let _ = self.sender.send(Task {
            code: code.to_string(),
            responder: tx,
        });
        rx.await.unwrap()
    }
}

#[op2]
fn op_trigger_action(
    #[string] action: String,
    #[serde] params: serde_json::Value,
) -> Result<(), AnyError> {
    println!("Triggering action: {}", action);
    println!("Params: {}", params);

    Ok(())
}

extension!(
    js_runtime,
    ops = [op_trigger_action],
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
                let path = module_specifier.to_file_path().unwrap();

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
    use super::*;

    #[tokio::test]
    async fn test_runtime_plugin() {
        let mut runtime = Runtime::new();
        runtime
            .run(
                r#"
                    import { activate } from '/home/fcoury/.config/red/plugins/start.js';
                    activate();
                "#,
            )
            .await
            .unwrap();
    }
}
