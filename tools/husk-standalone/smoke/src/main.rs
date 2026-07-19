use husk::{CallContext, Engine, NativeError, NativeModule, OwnedValue, ScriptResult};

fn main() -> anyhow::Result<()> {
    let regex = NativeModule::<()>::builder("regex")
        .typed_function(
            "is_match",
            |_context: &mut CallContext<'_, ()>,
             pattern: String,
             input: String|
             -> Result<ScriptResult<bool, String>, NativeError> {
                Ok(regex::Regex::new(&pattern)
                    .map(|regex| regex.is_match(&input))
                    .map_err(|error| error.to_string())
                    .into())
            },
        )
        .build()?;
    let engine = Engine::builder().register_module(regex)?.build()?;
    let compiled = engine.compile_source(
        "standalone-smoke",
        "standalone-smoke.hk",
        r#"
            fn matches() -> Result<bool, String> {
                regex::is_match("^husk$", "husk")
            }
        "#,
    )?;
    let mut instance = engine.instantiate(compiled, ())?;

    assert_eq!(
        instance.call("matches", &[])?,
        OwnedValue::Variant {
            type_name: "Result".to_string(),
            case: "Ok".to_string(),
            fields: vec![OwnedValue::Bool(true)],
        }
    );
    Ok(())
}
