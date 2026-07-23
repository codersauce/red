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

const SAME_NAMED_MATH_COMPONENT: &str = r#"
    (component
        (core module $m
            (func (export "add") (param i32 i32) (result i32)
                local.get 0
                local.get 1
                i32.add)
            (func (export "subtract") (param i32 i32) (result i32)
                local.get 0
                local.get 1
                i32.sub))
        (core instance $i (instantiate $m))
        (func $add
            (param "left" s32)
            (param "right" s32)
            (result s32)
            (canon lift (core func $i "add")))
        (func $subtract
            (param "left" s32)
            (param "right" s32)
            (result s32)
            (canon lift (core func $i "subtract")))
        (instance $math
            (export "add" (func $add))
            (export "subtract" (func $subtract)))
        (export "example:math/math@1.0.0" (instance $math)))
"#;

const RESOURCE_COMPONENT: &str = include_str!("../../husk-wasm/tests/fixtures/resource.wat");

fn same_named_interface_engine() -> Engine<()> {
    let component = WasmComponent::compile_bytes(
        "math",
        Version::new(1, 0, 0),
        SAME_NAMED_MATH_COMPONENT.as_bytes(),
        WasmCompileOptions::default(),
    )
    .unwrap();

    assert_eq!(component.descriptor().interfaces[0].name, "math");

    Engine::<()>::builder()
        .register_wasm_component(component)
        .unwrap()
        .build()
        .unwrap()
}

#[test]
fn same_named_component_interface_dispatches_from_module_root() {
    let engine = same_named_interface_engine();
    let compiled = engine
        .compile_source("main", "main.hk", "fn main() -> i32 { math::add(20, 22) }")
        .unwrap();
    let mut instance = engine.instantiate(compiled, ()).unwrap();

    assert_eq!(instance.call("main", &[]).unwrap(), OwnedValue::I64(42));
}

#[test]
fn same_named_component_interface_supports_individual_function_imports() {
    let engine = same_named_interface_engine();
    let compiled = engine
        .compile_source(
            "main",
            "main.hk",
            "use math::add;\nfn main() -> i32 { add(20, 22) }",
        )
        .unwrap();
    let mut instance = engine.instantiate(compiled, ()).unwrap();

    assert_eq!(instance.call("main", &[]).unwrap(), OwnedValue::I64(42));
}

#[test]
fn same_named_component_interface_supports_grouped_function_imports() {
    let engine = same_named_interface_engine();
    let compiled = engine
        .compile_source(
            "main",
            "main.hk",
            "use math::{add, subtract};\nfn main() -> i32 { add(subtract(44, 2), 0) }",
        )
        .unwrap();
    let mut instance = engine.instantiate(compiled, ()).unwrap();

    assert_eq!(instance.call("main", &[]).unwrap(), OwnedValue::I64(42));
}

#[test]
fn same_named_component_interface_rejects_duplicated_module_paths() {
    let engine = same_named_interface_engine();

    assert!(
        engine
            .compile_source(
                "main",
                "main.hk",
                "fn main() -> i32 { math::math::add(20, 22) }",
            )
            .is_err()
    );
    assert!(
        engine
            .compile_source(
                "main",
                "main.hk",
                "use math::math::add;\nfn main() -> i32 { add(20, 22) }",
            )
            .is_err()
    );
}

#[test]
fn same_named_resource_interface_dispatches_from_module_root() {
    let component = WasmComponent::compile_bytes(
        "factory",
        Version::new(1, 0, 0),
        RESOURCE_COMPONENT.as_bytes(),
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
            r#"
                use factory::{consume_item, item_value, new_item};

                fn main() -> i32 {
                    let item = new_item();
                    let value = item_value(item);
                    consume_item(item);
                    value
                }
            "#,
        )
        .unwrap();
    let mut instance = engine.instantiate(compiled, ()).unwrap();

    assert_eq!(instance.call("main", &[]).unwrap(), OwnedValue::I64(100));
}

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

#[test]
fn resource_constructor_borrow_and_own_transfer_work_in_husk_source() {
    let component = WasmComponent::compile_bytes(
        "resources",
        Version::new(1, 0, 0),
        RESOURCE_COMPONENT.as_bytes(),
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
            r#"
                fn main() -> i32 {
                    let item = resources::factory::new_item();
                    let value = resources::factory::item_value(item);
                    resources::factory::consume_item(item);
                    value
                }
            "#,
        )
        .unwrap();
    let mut instance = engine.instantiate(compiled, ()).unwrap();
    assert_eq!(instance.call("main", &[]).unwrap(), OwnedValue::I64(100));
}
