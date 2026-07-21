use husk::{Engine, OwnedValue, Version, WasmCompileOptions, WasmComponent, WasmLimits};

const MATH_COMPONENT: &str = r#"
    (component
        (core module $m
            (func (export "add") (param i32 i32) (result i32)
                local.get 0
                local.get 1
                i32.add))
        (core instance $i (instantiate $m))
        (func $add
            (param "left" s32)
            (param "right" s32)
            (result s32)
            (canon lift (core func $i "add")))
        (instance $api
            (export "add" (func $add)))
        (export "example:math/api@1.0.0" (instance $api)))
"#;

#[test]
fn component_descriptor_typechecks_and_dispatches_through_engine() {
    let component = WasmComponent::compile_bytes(
        "math",
        Version::new(1, 0, 0),
        MATH_COMPONENT.as_bytes(),
        WasmCompileOptions::default(),
    )
    .unwrap();
    assert!(component.actual_imports().is_empty());
    assert_eq!(component.descriptor().interfaces[0].name, "api");

    let engine = Engine::<()>::builder()
        .register_wasm_component(component)
        .unwrap()
        .build()
        .unwrap();
    let compiled = engine
        .compile_source(
            "main",
            "main.hk",
            "fn main() -> i32 { math::api::add(20, 22) }",
        )
        .unwrap();
    let mut instance = engine.instantiate(compiled, ()).unwrap();
    assert_eq!(instance.call("main", &[]).unwrap(), OwnedValue::I64(42));
}

#[test]
fn component_stores_are_isolated_between_husk_instances() {
    let component = WasmComponent::compile_bytes(
        "math",
        Version::new(1, 0, 0),
        MATH_COMPONENT.as_bytes(),
        WasmCompileOptions::default(),
    )
    .unwrap();
    let engine = Engine::<()>::builder()
        .register_wasm_component(component)
        .unwrap()
        .build()
        .unwrap();
    let compiled = engine
        .compile_source(
            "main",
            "main.hk",
            "fn main() -> i32 { math::api::add(1, 2) }",
        )
        .unwrap();
    let mut first = engine.instantiate(compiled.clone(), ()).unwrap();
    let mut second = engine.instantiate(compiled, ()).unwrap();
    assert_ne!(first.generation(), second.generation());
    assert_eq!(first.call("main", &[]).unwrap(), OwnedValue::I64(3));
    assert_eq!(second.call("main", &[]).unwrap(), OwnedValue::I64(3));
}

#[test]
fn direct_wasm_instance_api_remains_available_for_hosts() {
    let component = WasmComponent::compile_bytes(
        "math",
        Version::new(1, 0, 0),
        MATH_COMPONENT.as_bytes(),
        WasmCompileOptions::default(),
    )
    .unwrap();
    let mut instance = component.instantiate(WasmLimits::default()).unwrap();
    assert_eq!(
        instance
            .call("api::add", &[OwnedValue::I32(19), OwnedValue::I32(23)])
            .unwrap(),
        OwnedValue::I32(42)
    );
}
