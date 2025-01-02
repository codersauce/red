use deno_ast::{MediaType, ParseParams, SourceTextInfo};
use deno_core::{
    error::AnyError, futures::FutureExt, url::Url, ModuleLoadResponse, ModuleLoader, ModuleSource,
    ModuleSourceCode, ModuleSpecifier, RequestedModuleType, ResolutionKind,
};

pub struct TsModuleLoader;

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
                let (extension, code, media_type) = if module_specifier.scheme() == "http"
                    || module_specifier.scheme() == "https"
                {
                    let code = reqwest::get(module_specifier.as_str())
                        .await?
                        .text()
                        .await?;

                    let media_type = MediaType::from_specifier(&module_specifier);
                    let extension = media_type.as_ts_extension();

                    (Some(extension.to_string()), code, media_type)
                } else {
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

                    // Read the file, transpile if necessary.
                    let code = std::fs::read_to_string(&path)?;

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
                    _ => panic!("Unknown extension {:?}", extension),
                };

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
                    &Url::parse(module_specifier.as_ref())?,
                );

                Ok(module)
            }
            .boxed_local(),
        )
    }
}
