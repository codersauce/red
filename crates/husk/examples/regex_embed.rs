use husk::{CallContext, Engine, NativeError, NativeModule, OwnedValue, ScriptResult};

#[derive(Default)]
struct AppState {
    regex_calls: usize,
}

fn main() -> anyhow::Result<()> {
    let regex = NativeModule::<AppState>::builder("regex")
        .typed_function(
            "is_match",
            |context: &mut CallContext<'_, AppState>,
             pattern: String,
             input: String|
             -> Result<ScriptResult<bool, String>, NativeError> {
                context.data_mut().regex_calls += 1;
                Ok(regex::Regex::new(&pattern)
                    .map(|regex| regex.is_match(&input))
                    .map_err(|error| error.to_string())
                    .into())
            },
        )
        .build()?;
    let engine = Engine::builder().register_module(regex)?.build()?;
    let compiled = engine.compile_source(
        "regex-example",
        "examples/regex-example.hk",
        r#"
            fn matches() -> Result<bool, String> {
                return regex::is_match("^husk$", "husk");
            }

            fn invalid_pattern() -> Result<bool, String> {
                return regex::is_match("[", "husk");
            }
        "#,
    )?;
    let mut instance = engine.instantiate(compiled, AppState::default())?;

    assert!(matches!(
        instance.call("matches", &[])?,
        OwnedValue::Variant { case, fields, .. }
            if case == "Ok" && fields == vec![OwnedValue::Bool(true)]
    ));
    assert!(matches!(
        instance.call("invalid_pattern", &[])?,
        OwnedValue::Variant { case, .. } if case == "Err"
    ));
    assert_eq!(instance.data().regex_calls, 2);
    Ok(())
}
