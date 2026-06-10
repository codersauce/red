use deno_ast::{MediaType, ParseParams};
use deno_core::{
    error::ModuleLoaderError, futures::FutureExt, ModuleLoadOptions, ModuleLoadReferrer,
    ModuleLoadResponse, ModuleLoader, ModuleResolveResponse, ModuleSource, ModuleSourceCode,
    ModuleSpecifier, ResolutionKind,
};

use crate::assets;

pub struct TsModuleLoader;

impl ModuleLoader for TsModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        _kind: ResolutionKind,
    ) -> ModuleResolveResponse {
        deno_core::resolve_import(specifier, referrer).map_err(ModuleLoaderError::from_err)
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        _maybe_referrer: Option<&ModuleLoadReferrer>,
        _options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        let module_specifier = module_specifier.clone();

        ModuleLoadResponse::Async(
            async move {
                let (extension, code, media_type) =
                    if assets::is_bundled_plugin_specifier(module_specifier.as_str()) {
                        let code = assets::bundled_plugin_contents(module_specifier.as_str())
                            .ok_or_else(|| {
                                ModuleLoaderError::generic(format!(
                                    "Bundled plugin module `{}` was not found",
                                    module_specifier
                                ))
                            })?
                            .to_string();
                        let media_type = MediaType::from_specifier(&module_specifier);
                        let extension = media_type.as_ts_extension();

                        (Some(extension.to_string()), code, media_type)
                    } else if module_specifier.scheme() == "http"
                        || module_specifier.scheme() == "https"
                    {
                        let code = reqwest::get(module_specifier.as_str())
                            .await
                            .map_err(|err| ModuleLoaderError::generic(err.to_string()))?
                            .text()
                            .await
                            .map_err(|err| ModuleLoaderError::generic(err.to_string()))?;

                        let media_type = MediaType::from_specifier(&module_specifier);
                        let extension = media_type.as_ts_extension();

                        (Some(extension.to_string()), code, media_type)
                    } else {
                        let path = match module_specifier.to_file_path() {
                            Ok(path) => path,
                            Err(e) => {
                                return Err(ModuleLoaderError::generic(format!(
                                    "Cannot convert module specifier to file path: {:?}",
                                    e
                                )));
                            }
                        };

                        // Determine what the MediaType is (this is done based on the file
                        // extension) and whether transpiling is required.
                        let media_type = MediaType::from_path(&path);

                        // Read the file, transpile if necessary.
                        let code = std::fs::read_to_string(&path).map_err(|err| {
                            ModuleLoaderError::generic(format!(
                                "Could not read plugin module `{}`: {}",
                                path.display(),
                                err
                            ))
                        })?;

                        (
                            path.extension().map(|e| e.to_str().unwrap().to_string()),
                            code,
                            media_type,
                        )
                    };

                let (module_type, should_transpile) = match media_type {
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
                    _ => {
                        return Err(ModuleLoaderError::generic(format!(
                            "Unknown extension {:?}",
                            extension
                        )));
                    }
                };

                let code = if should_transpile {
                    let parsed = deno_ast::parse_module(ParseParams {
                        specifier: module_specifier.clone(),
                        text: code.into(),
                        media_type,
                        capture_tokens: false,
                        scope_analysis: false,
                        maybe_syntax: None,
                    })
                    .map_err(ModuleLoaderError::from_err)?;
                    let transpile_options = Default::default();
                    let transpile_result = parsed
                        .transpile(&transpile_options, &Default::default(), &Default::default())
                        .map_err(ModuleLoaderError::from_err)?;
                    transpile_result.into_source().text
                } else {
                    code
                };

                // Load and return module.
                let module = ModuleSource::new(
                    module_type,
                    ModuleSourceCode::String(code.into()),
                    &module_specifier,
                    None,
                );

                Ok(module)
            }
            .boxed_local(),
        )
    }
}
