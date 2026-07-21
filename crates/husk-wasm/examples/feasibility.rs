use std::time::Instant;

use husk_types::Version;
use husk_value::OwnedValue;
use husk_wasm::{WasmCompileOptions, WasmComponent, WasmLimits};

const COMPONENT: &str = r#"
    (component
        (core module $m
            (func (export "add") (param i32 i32) (result i32)
                local.get 0 local.get 1 i32.add))
        (core instance $i (instantiate $m))
        (func (export "add")
            (param "left" s32)
            (param "right" s32)
            (result s32)
            (canon lift (core func $i "add"))))
"#;

fn main() -> anyhow::Result<()> {
    const CALLS: u32 = 100_000;

    let started = Instant::now();
    let component = WasmComponent::compile_bytes(
        "math",
        Version::new(1, 0, 0),
        COMPONENT.as_bytes(),
        WasmCompileOptions::default(),
    )?;
    let compile = started.elapsed();

    let started = Instant::now();
    let mut instance = component.instantiate(WasmLimits::default())?;
    let instantiate = started.elapsed();

    let started = Instant::now();
    for value in 0..CALLS {
        let result = instance.call("add", &[OwnedValue::I32(value as i32), OwnedValue::I32(1)])?;
        assert_eq!(result, OwnedValue::I32(value as i32 + 1));
    }
    let calls = started.elapsed();

    println!("component_input_bytes={}", COMPONENT.len());
    println!("cold_compile_us={}", compile.as_micros());
    println!("instantiate_us={}", instantiate.as_micros());
    println!("calls={CALLS}");
    println!("call_loop_us={}", calls.as_micros());
    println!(
        "mean_call_ns={}",
        calls.as_nanos().checked_div(u128::from(CALLS)).unwrap()
    );
    Ok(())
}
