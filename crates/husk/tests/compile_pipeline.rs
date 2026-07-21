use husk::{
    CompileLimits, CompileOptions, CompiledProgram, FunctionDescriptor, ModuleDescriptor,
    ParameterDescriptor, SemanticProfile, TypeDescriptor, Version,
};
use husk_runtime::{Host, Value, Vm};

#[derive(Debug, Default)]
struct RecordingHost {
    actions: Vec<String>,
}

impl Host for RecordingHost {
    fn log(&mut self, _message: &str) {}

    fn call_module(
        &mut self,
        _plugin: &str,
        path: &str,
        args: &[Value],
    ) -> Option<anyhow::Result<Value>> {
        (path == "test::record").then(|| {
            let action = args
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("test::record expects a string"))?;
            self.actions.push(action.to_string());
            Ok(Value::Unit)
        })
    }
}

#[test]
fn compiled_artifact_keeps_source_syntax_semantics_and_source_map() {
    let program = CompiledProgram::compile_at(
        "checked",
        "scripts/checked.hk",
        "fn answer() -> i32 { return 42; }",
        &CompileOptions::default(),
    )
    .unwrap();

    assert_eq!(program.name(), "checked");
    assert_eq!(program.source().name(), "scripts/checked.hk");
    assert_eq!(program.source_map().sources().len(), 1);
    assert!(program.source_map().get("scripts/checked.hk").is_some());
    assert_eq!(program.syntax().items.len(), 1);
    assert!(program.semantic_result().is_some());
    assert_eq!(program.semantic_profile(), SemanticProfile::Native);
    let hir = program.hir_functions();
    assert_eq!(hir.len(), 1);
    assert_eq!(hir[0].qualified_name, "answer");
    assert!(hir[0].node_count > 0);
}

#[test]
fn function_ids_are_stable_across_source_declaration_order() {
    let options = CompileOptions::default();
    let first = CompiledProgram::compile_at(
        "stable-ids",
        "scripts/stable-ids.hk",
        "fn alpha() {} fn beta() {}",
        &options,
    )
    .unwrap();
    let second = CompiledProgram::compile_at(
        "stable-ids",
        "scripts/stable-ids.hk",
        "fn beta() {} fn alpha() {}",
        &options,
    )
    .unwrap();

    let ids = |program: &CompiledProgram| {
        program
            .hir_functions()
            .into_iter()
            .map(|function| (function.qualified_name, function.id.raw()))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    assert_eq!(ids(&first), ids(&second));
}

#[test]
fn vm_activates_a_precompiled_artifact_without_receiving_source_text() {
    let program = CompiledProgram::compile_at(
        "precompiled",
        "plugins/precompiled.hk",
        r#"fn activate() { test::record("Activated"); }"#,
        &CompileOptions::legacy_runtime_compatibility(),
    )
    .unwrap();
    let mut vm = Vm::new();
    let mut host = RecordingHost::default();

    vm.load_compiled_plugin("precompiled", program, &mut host)
        .unwrap();

    assert_eq!(host.actions, ["Activated"]);
}

#[test]
fn parser_and_semantic_diagnostics_keep_the_requested_path_and_byte_location() {
    let parse_error = CompiledProgram::compile_at(
        "broken-parse",
        "workspace/broken-parse.hk",
        "fn broken( {",
        &CompileOptions::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(parse_error.contains("HUSK-P0001"), "{parse_error}");
    assert!(
        parse_error.contains("workspace/broken-parse.hk:1:"),
        "{parse_error}"
    );

    let semantic_error = CompiledProgram::compile_at(
        "broken-types",
        "workspace/broken-types.hk",
        "fn choose() -> bool {\n    return 42;\n}",
        &CompileOptions::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(semantic_error.contains("HUSK-T0001"), "{semantic_error}");
    assert!(
        semantic_error.contains("workspace/broken-types.hk:2:"),
        "{semantic_error}"
    );
    assert!(semantic_error.contains("return type"), "{semantic_error}");
}

#[test]
fn native_profile_rejects_javascript_only_source() {
    let error = CompiledProgram::compile_at(
        "native",
        "scripts/native.hk",
        "fn browser_only() { let value = js { window.location }; }",
        &CompileOptions::default(),
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("HUSK-T0001"), "{error}");
    assert!(
        error.contains("only available in the legacy JavaScript profile"),
        "{error}"
    );
}

#[test]
fn compile_limits_fail_with_source_aware_diagnostics() {
    let options = CompileOptions {
        limits: CompileLimits {
            max_source_bytes: 8,
            max_top_level_items: 1,
        },
        ..CompileOptions::default()
    };
    let error = CompiledProgram::compile_at(
        "limited",
        "scripts/limited.hk",
        "fn too_large() {}",
        &options,
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("HUSK-C0001"), "{error}");
    assert!(error.contains("scripts/limited.hk:1:1"), "{error}");
}

#[test]
fn typed_module_descriptor_supplies_semantic_declarations() {
    let module = ModuleDescriptor::new(
        "sample",
        Version::new(1, 0, 0),
        vec![
            FunctionDescriptor::new(
                "add",
                vec![
                    ParameterDescriptor::new("left", TypeDescriptor::I32).unwrap(),
                    ParameterDescriptor::new("right", TypeDescriptor::I32).unwrap(),
                ],
                TypeDescriptor::I32,
            )
            .unwrap(),
        ],
        Vec::new(),
    )
    .unwrap();
    let options = CompileOptions::default().with_module(module.clone());
    let program = CompiledProgram::compile_at(
        "uses-module",
        "scripts/uses-module.hk",
        "fn total() -> i32 { return sample::add(20, 22); }",
        &options,
    )
    .unwrap();

    assert_eq!(program.modules(), [module]);

    let error = CompiledProgram::compile_at(
        "bad-module-call",
        "scripts/bad-module-call.hk",
        r#"fn total() -> i32 { return sample::add("twenty", 22); }"#,
        &options,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("HUSK-T0001"), "{error}");
    assert!(error.contains("argument"), "{error}");
}
