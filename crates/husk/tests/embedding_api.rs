use husk::{CallContext, Engine, Limits, NativeError, NativeModule, OwnedValue, ReplOutcome};

#[derive(Debug, Default)]
struct AppState {
    calls: usize,
}

fn sample_module() -> husk::NativeModule<AppState> {
    NativeModule::builder("sample")
        .typed_function(
            "add",
            |context: &mut CallContext<'_, AppState>,
             left: i32,
             right: i32|
             -> Result<i32, NativeError> {
                context.data_mut().calls += 1;
                Ok(left + right)
            },
        )
        .build()
        .unwrap()
}

#[test]
fn external_embedder_compiles_once_instantiates_many_and_calls_typed_rust() {
    let engine = Engine::builder()
        .register_module(sample_module())
        .unwrap()
        .build()
        .unwrap();
    let compiled = engine
        .compile_source(
            "calculator",
            "scripts/calculator.hk",
            "fn main() -> i32 { return sample::add(20, 22); }",
        )
        .unwrap();

    let mut first = engine
        .instantiate(compiled.clone(), AppState::default())
        .unwrap();
    let mut second = engine.instantiate(compiled, AppState::default()).unwrap();

    assert_eq!(first.call("main", &[]).unwrap(), OwnedValue::I64(42));
    assert_eq!(first.data().calls, 1);
    assert_eq!(second.data().calls, 0);
    assert_eq!(second.call("main", &[]).unwrap(), OwnedValue::I64(42));
    assert_eq!(second.data().calls, 1);
    assert_ne!(first.generation(), second.generation());
}

#[test]
fn typed_conversion_failure_has_module_function_argument_and_source_context() {
    let engine = Engine::builder()
        .register_module(sample_module())
        .unwrap()
        .typecheck(false)
        .build()
        .unwrap();
    let compiled = engine
        .compile_source(
            "bad-call",
            "scripts/bad-call.hk",
            r#"fn main() { sample::add("not an integer", 2); }"#,
        )
        .unwrap();
    let mut instance = engine.instantiate(compiled, AppState::default()).unwrap();

    let error = instance.call("main", &[]).unwrap_err().to_string();
    assert!(error.contains("sample::add"), "{error}");
    assert!(error.contains("argument 0 expected I32"), "{error}");
    assert!(error.contains("scripts/bad-call.hk:1:"), "{error}");
}

#[test]
fn engine_rejects_duplicate_module_roots() {
    let error = Engine::builder()
        .register_module(sample_module())
        .unwrap()
        .register_module(sample_module())
        .err()
        .unwrap()
        .to_string();

    assert!(
        error.contains("duplicate native module `sample`"),
        "{error}"
    );
}

#[test]
fn native_execution_uses_semantic_local_ids_for_lexical_shadowing() {
    let engine = Engine::<()>::builder().build().unwrap();
    let compiled = engine
        .compile_source(
            "shadowing",
            "scripts/shadowing.hk",
            r#"
                fn main() -> i32 {
                    let value = 1;
                    {
                        let value = 2;
                    }
                    value
                }
            "#,
        )
        .unwrap();
    let mut instance = engine.instantiate(compiled, ()).unwrap();

    assert_eq!(instance.call("main", &[]).unwrap(), OwnedValue::I64(1));
}

#[test]
fn native_call_budget_stops_before_entering_an_extra_host_call() {
    let limits = Limits {
        native_calls_per_call: 1,
        ..Limits::default()
    };
    let engine = Engine::builder()
        .register_module(sample_module())
        .unwrap()
        .limits(limits)
        .build()
        .unwrap();
    let compiled = engine
        .compile_source(
            "budget",
            "scripts/budget.hk",
            r#"
                fn main() -> i32 {
                    sample::add(1, 2);
                    sample::add(20, 22)
                }
            "#,
        )
        .unwrap();
    let mut instance = engine.instantiate(compiled, AppState::default()).unwrap();

    let error = instance.call("main", &[]).unwrap_err().to_string();
    assert!(error.contains("host-call budget exhausted"), "{error}");
    assert_eq!(instance.data().calls, 1);
}

#[test]
fn lazy_range_iteration_is_bounded_by_the_instruction_budget() {
    let limits = Limits {
        instructions_per_call: 32,
        ..Limits::default()
    };
    let engine = Engine::<()>::builder().limits(limits).build().unwrap();
    let compiled = engine
        .compile_source(
            "lazy-range",
            "scripts/lazy-range.hk",
            r#"
                fn main() {
                    for value in 0..1000000000000 {}
                }
            "#,
        )
        .unwrap();
    let mut instance = engine.instantiate(compiled, ()).unwrap();

    let error = instance.call("main", &[]).unwrap_err().to_string();
    assert!(error.contains("instruction budget exhausted"), "{error}");
}

#[test]
fn completed_call_frames_release_non_escaping_heap_cells() {
    let limits = Limits {
        instructions_per_call: 10_000,
        max_heap_bytes: 16 * 1024,
        ..Limits::default()
    };
    let engine = Engine::<()>::builder().limits(limits).build().unwrap();
    let compiled = engine
        .compile_source(
            "frame-reclamation",
            "scripts/frame-reclamation.hk",
            r#"
                fn inspect(values: [i32]) -> i32 {
                    values.len()
                }

                fn main() -> i32 {
                    let values = [
                        0, 1, 2, 3, 4, 5, 6, 7, 8, 9,
                        10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
                    ];
                    let total = 0;
                    for iteration in 0..200 {
                        total += inspect(values);
                    }
                    total
                }
            "#,
        )
        .unwrap();
    let mut instance = engine.instantiate(compiled, ()).unwrap();

    assert_eq!(instance.call("main", &[]).unwrap(), OwnedValue::I64(4_000));
}

#[test]
fn call_depth_and_detached_value_limits_are_enforced() {
    let limits = Limits {
        max_call_depth: 3,
        max_value_bytes: 128,
        ..Limits::default()
    };
    let engine = Engine::<()>::builder().limits(limits).build().unwrap();
    let compiled = engine
        .compile_source(
            "limits",
            "scripts/limits.hk",
            r#"
                fn recurse(value: i32) -> i32 {
                    if value == 0 {
                        return 0;
                    }
                    recurse(value - 1)
                }

                fn large() -> String {
                    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
                }
            "#,
        )
        .unwrap();
    let mut instance = engine.instantiate(compiled, ()).unwrap();

    let depth_error = instance
        .call("recurse", &[OwnedValue::I32(10)])
        .unwrap_err()
        .to_string();
    assert!(depth_error.contains("call depth exceeded"), "{depth_error}");

    let value_error = instance.call("large", &[]).unwrap_err().to_string();
    assert!(
        value_error.contains("128-byte boundary limit"),
        "{value_error}"
    );
}

#[test]
fn retained_function_root_limit_is_enforced() {
    let limits = Limits {
        max_callback_roots: 1,
        ..Limits::default()
    };
    let engine = Engine::<()>::builder().limits(limits).build().unwrap();
    let compiled = engine
        .compile_source(
            "roots",
            "scripts/roots.hk",
            "fn make(value: i32) -> fn() -> i32 { || value }",
        )
        .unwrap();
    let mut instance = engine.instantiate(compiled, ()).unwrap();

    let first = instance
        .capture_function("make", &[OwnedValue::I32(1)])
        .unwrap();
    let error = instance
        .capture_function("make", &[OwnedValue::I32(2)])
        .unwrap_err()
        .to_string();
    assert!(error.contains("function root limit exceeded"), "{error}");
    assert!(instance.release_function(first).unwrap());
}

#[test]
fn repl_preserves_items_locals_and_native_module_state() {
    let engine = Engine::builder()
        .register_module(sample_module())
        .unwrap()
        .build()
        .unwrap();
    let mut repl = engine.repl(AppState::default()).unwrap();

    assert_eq!(
        repl.submit("fn add_one(value: i32) -> i32 {").unwrap(),
        ReplOutcome::Incomplete
    );
    assert_eq!(
        repl.submit("fn add_one(value: i32) -> i32 { value + 1 }")
            .unwrap(),
        ReplOutcome::Defined
    );
    assert_eq!(
        repl.submit("let mut value = sample::add(20, 20);").unwrap(),
        ReplOutcome::Value(OwnedValue::Unit)
    );
    assert_eq!(repl.data().calls, 1);
    assert_eq!(
        repl.submit("value = add_one(value)").unwrap(),
        ReplOutcome::Value(OwnedValue::I64(41))
    );
    assert_eq!(
        repl.submit("value + 1").unwrap(),
        ReplOutcome::Value(OwnedValue::I64(42))
    );
}

#[test]
fn repl_closures_keep_stable_function_targets_after_new_definitions() {
    let engine = Engine::<()>::builder().build().unwrap();
    let mut repl = engine.repl(()).unwrap();

    assert_eq!(
        repl.submit("fn zed() -> i32 { 41 }").unwrap(),
        ReplOutcome::Defined
    );
    assert_eq!(
        repl.submit("let callback = || zed() + 1;").unwrap(),
        ReplOutcome::Value(OwnedValue::Unit)
    );
    assert_eq!(
        repl.submit("fn alpha() -> i32 { 0 }").unwrap(),
        ReplOutcome::Defined
    );
    assert_eq!(
        repl.submit("callback()").unwrap(),
        ReplOutcome::Value(OwnedValue::I64(42))
    );
}

#[test]
fn repl_does_not_commit_failed_script_state() {
    let engine = Engine::<()>::builder().build().unwrap();
    let mut repl = engine.repl(()).unwrap();

    repl.submit("let mut value = 7;").unwrap();
    let error = repl.submit("value = value / 0").unwrap_err().to_string();
    assert!(error.contains("division by zero"), "{error}");
    assert_eq!(
        repl.submit("value").unwrap(),
        ReplOutcome::Value(OwnedValue::I64(7))
    );
}
