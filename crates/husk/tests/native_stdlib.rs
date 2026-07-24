use husk::{Engine, Limits, OwnedValue};

fn instance(source: &str) -> husk::Instance<()> {
    let engine = Engine::<()>::builder().build().unwrap();
    let compiled = engine
        .compile_source("stdlib", "stdlib.hk", source)
        .unwrap();
    engine.instantiate(compiled, ()).unwrap()
}

fn some(value: OwnedValue) -> OwnedValue {
    OwnedValue::Variant {
        type_name: "Option".to_string(),
        case: "Some".to_string(),
        fields: vec![value],
    }
}

fn none() -> OwnedValue {
    OwnedValue::Variant {
        type_name: "Option".to_string(),
        case: "None".to_string(),
        fields: Vec::new(),
    }
}

fn ok(value: OwnedValue) -> OwnedValue {
    OwnedValue::Variant {
        type_name: "Result".to_string(),
        case: "Ok".to_string(),
        fields: vec![value],
    }
}

fn err(value: &str) -> OwnedValue {
    OwnedValue::Variant {
        type_name: "Result".to_string(),
        case: "Err".to_string(),
        fields: vec![OwnedValue::String(value.to_string())],
    }
}

#[test]
fn parses_complete_signed_numbers_and_exposes_result_helpers() {
    let mut instance = instance(
        r#"
            fn parsed_i32(value: String) -> Result<i32, String> {
                value.parse::<i32>()
            }

            fn parsed_i64(value: String) -> Result<i64, String> {
                value.parse::<i64>()
            }

            fn parsed_f64(value: String) -> Result<f64, String> {
                value.parse::<f64>()
            }

            fn inferred(value: String) -> Result<i32, String> {
                value.parse()
            }

            fn fallback(value: String) -> i32 {
                value.trim().parse::<i32>().unwrap_or(7)
            }

            fn result_state(value: String) -> (bool, bool, Option<i32>, Option<String>) {
                let parsed = value.parse::<i32>();
                (parsed.is_ok(), parsed.is_err(), parsed.ok(), parsed.err())
            }

            fn propagated(value: String) -> Result<i32, String> {
                let parsed = value.parse::<i32>()?;
                Ok(parsed + 1)
            }

            fn narrowed(value: i64) -> Result<i32, String> {
                value.try_into::<i32>()
            }

            fn converted(value: i32) -> (i64, f64, String) {
                (value.into::<i64>(), value.into::<f64>(), value.into::<String>())
            }

            fn converted_static(value: i32) -> (i64, f64, String) {
                (i64::from(value), f64::from(value), String::from(value))
            }

            fn converted_bool(value: bool) -> String {
                String::from(value)
            }

            fn parsed_static(value: String) -> (Result<i32, String>, Result<i64, String>, Result<f64, String>) {
                (i32::try_from(value), i64::try_from(value), f64::try_from(value))
            }

            fn narrowed_static(value: i64) -> Result<i32, String> {
                i32::try_from(value)
            }

            fn exact_float(value: i64) -> Result<f64, String> {
                f64::try_from(value)
            }

            fn exact_float_method(value: i64) -> Result<f64, String> {
                value.try_into::<f64>()
            }
        "#,
    );

    assert_eq!(
        instance
            .call("parsed_i32", &[OwnedValue::String("+42".to_string())])
            .unwrap(),
        ok(OwnedValue::I64(42))
    );
    assert_eq!(
        instance
            .call("parsed_i64", &[OwnedValue::String("-42".to_string())])
            .unwrap(),
        ok(OwnedValue::I64(-42))
    );
    assert_eq!(
        instance
            .call("parsed_f64", &[OwnedValue::String("-1.25e2".to_string())])
            .unwrap(),
        ok(OwnedValue::F64(-125.0))
    );
    assert_eq!(
        instance
            .call("inferred", &[OwnedValue::String("12".to_string())])
            .unwrap(),
        ok(OwnedValue::I64(12))
    );
    assert_eq!(
        instance
            .call("fallback", &[OwnedValue::String(" 19 ".to_string())])
            .unwrap(),
        OwnedValue::I64(19)
    );
    assert_eq!(
        instance
            .call("fallback", &[OwnedValue::String("19x".to_string())])
            .unwrap(),
        OwnedValue::I64(7)
    );
    assert_eq!(
        instance
            .call("result_state", &[OwnedValue::String("8".to_string())])
            .unwrap(),
        OwnedValue::Tuple(vec![
            OwnedValue::Bool(true),
            OwnedValue::Bool(false),
            some(OwnedValue::I64(8)),
            none(),
        ])
    );
    let invalid = instance
        .call(
            "result_state",
            &[OwnedValue::String("2147483648".to_string())],
        )
        .unwrap();
    assert_eq!(
        invalid,
        OwnedValue::Tuple(vec![
            OwnedValue::Bool(false),
            OwnedValue::Bool(true),
            none(),
            some(OwnedValue::String(
                "number is outside the target range".to_string()
            )),
        ])
    );
    assert_eq!(
        instance
            .call("propagated", &[OwnedValue::String("41".to_string())])
            .unwrap(),
        ok(OwnedValue::I64(42))
    );
    assert_eq!(
        instance.call("narrowed", &[OwnedValue::I64(42)]).unwrap(),
        ok(OwnedValue::I64(42))
    );
    assert_eq!(
        instance
            .call("narrowed", &[OwnedValue::I64(i64::from(i32::MAX) + 1)])
            .unwrap(),
        err("number is outside the target range")
    );
    assert_eq!(
        instance
            .call("parsed_f64", &[OwnedValue::String("NaN".to_string())])
            .unwrap(),
        err("number must be finite")
    );
    assert_eq!(
        instance.call("converted", &[OwnedValue::I32(42)]).unwrap(),
        OwnedValue::Tuple(vec![
            OwnedValue::I64(42),
            OwnedValue::F64(42.0),
            OwnedValue::String("42".to_string()),
        ])
    );
    assert_eq!(
        instance
            .call("converted_static", &[OwnedValue::I32(42)])
            .unwrap(),
        OwnedValue::Tuple(vec![
            OwnedValue::I64(42),
            OwnedValue::F64(42.0),
            OwnedValue::String("42".to_string()),
        ])
    );
    assert_eq!(
        instance
            .call("converted_bool", &[OwnedValue::Bool(true)])
            .unwrap(),
        OwnedValue::String("true".to_string())
    );
    assert_eq!(
        instance
            .call("parsed_static", &[OwnedValue::String("42".to_string())])
            .unwrap(),
        OwnedValue::Tuple(vec![
            ok(OwnedValue::I64(42)),
            ok(OwnedValue::I64(42)),
            ok(OwnedValue::F64(42.0)),
        ])
    );
    assert_eq!(
        instance
            .call(
                "narrowed_static",
                &[OwnedValue::I64(i64::from(i32::MAX) + 1)]
            )
            .unwrap(),
        err("number is outside the target range")
    );
    assert_eq!(
        instance
            .call("exact_float", &[OwnedValue::I64(9_007_199_254_740_992)])
            .unwrap(),
        ok(OwnedValue::F64(9_007_199_254_740_992.0))
    );
    assert_eq!(
        instance
            .call(
                "exact_float_method",
                &[OwnedValue::I64(9_007_199_254_740_993)]
            )
            .unwrap(),
        err("number cannot be represented exactly as f64")
    );
}

#[test]
fn inexact_i64_to_f64_is_not_declared_as_an_infallible_conversion() {
    let engine = Engine::<()>::builder().build().unwrap();
    let error = engine
        .compile_source(
            "stdlib",
            "stdlib.hk",
            "fn invalid(value: i64) -> f64 { value.into::<f64>() }",
        )
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("the trait `From<i64>` is not implemented for `f64`"),
        "{error}"
    );
}

#[test]
fn native_array_sort_is_in_place_and_does_not_return_a_cloned_array() {
    let mut instance =
        instance("fn sorted(values: [i32]) { let mut values = values; values.sort() }");
    assert_eq!(
        instance
            .call(
                "sorted",
                &[OwnedValue::List(vec![
                    OwnedValue::I32(2),
                    OwnedValue::I32(1),
                ])],
            )
            .unwrap(),
        OwnedValue::Unit
    );
}

#[test]
fn option_helpers_and_array_access_are_safe_and_typed() {
    let mut instance = instance(
        r#"
            fn option_state(value: Option<i32>) -> (bool, bool, i32) {
                (value.is_some(), value.is_none(), value.unwrap_or(9))
            }

            fn array_state(values: [i32]) -> (bool, Option<i32>, Option<i32>, Option<i32>, bool) {
                (
                    values.is_empty(),
                    values.get(1),
                    values.first(),
                    values.last(),
                    values.contains(4),
                )
            }

            fn mutation_state() -> (Option<i32>, Option<i32>, Option<i32>, Option<i32>, String) {
                let mut values = [3, 1, 2];
                values.sort();
                let last = values.pop();
                values.reverse();
                let first = values.shift();
                let final_value = values.shift();
                let empty = values.shift();
                values.unshift(4);
                (last, first, final_value, empty, values.join(","))
            }
        "#,
    );

    assert_eq!(
        instance.call("option_state", &[none()]).unwrap(),
        OwnedValue::Tuple(vec![
            OwnedValue::Bool(false),
            OwnedValue::Bool(true),
            OwnedValue::I64(9),
        ])
    );
    assert_eq!(
        instance
            .call("option_state", &[some(OwnedValue::I32(4))])
            .unwrap(),
        OwnedValue::Tuple(vec![
            OwnedValue::Bool(true),
            OwnedValue::Bool(false),
            OwnedValue::I64(4),
        ])
    );
    assert_eq!(
        instance.call("mutation_state", &[]).unwrap(),
        OwnedValue::Tuple(vec![
            some(OwnedValue::I64(3)),
            some(OwnedValue::I64(2)),
            some(OwnedValue::I64(1)),
            none(),
            OwnedValue::String("4".to_string()),
        ])
    );
    assert_eq!(
        instance
            .call(
                "array_state",
                &[OwnedValue::List(vec![
                    OwnedValue::I32(2),
                    OwnedValue::I32(4),
                    OwnedValue::I32(8),
                ])],
            )
            .unwrap(),
        OwnedValue::Tuple(vec![
            OwnedValue::Bool(false),
            some(OwnedValue::I64(4)),
            some(OwnedValue::I64(2)),
            some(OwnedValue::I64(8)),
            OwnedValue::Bool(true),
        ])
    );
    assert_eq!(
        instance
            .call("array_state", &[OwnedValue::List(Vec::new())])
            .unwrap(),
        OwnedValue::Tuple(vec![
            OwnedValue::Bool(true),
            none(),
            none(),
            none(),
            OwnedValue::Bool(false),
        ])
    );
}

#[test]
fn string_helpers_preserve_unicode_and_use_rust_like_names() {
    let mut instance = instance(
        r#"
            fn text_state(value: String) -> (bool, String, String, bool, Option<String>, Option<String>) {
                (
                    value.is_empty(),
                    value.trim_start(),
                    value.trim_end(),
                    value.contains("é"),
                    value.strip_prefix("  "),
                    value.strip_suffix("  "),
                )
            }

            fn split_state(value: String) -> ([String], [String], Option<(String, String)>) {
                (value.split_whitespace(), value.lines(), value.rsplit_once(":"))
            }

            fn transformations(value: String) -> (String, String, String, String) {
                (
                    value.replace("é", "e"),
                    value.repeat(2),
                    value.to_lowercase(),
                    value.to_uppercase(),
                )
            }

            fn digits() -> (bool, bool, Option<i32>, Option<i32>) {
                ("7".is_ascii_digit(), "７".is_ascii_digit(), "f".to_digit(16), "z".to_digit(10))
            }
        "#,
    );

    assert_eq!(
        instance
            .call("text_state", &[OwnedValue::String("  héllo  ".to_string())])
            .unwrap(),
        OwnedValue::Tuple(vec![
            OwnedValue::Bool(false),
            OwnedValue::String("héllo  ".to_string()),
            OwnedValue::String("  héllo".to_string()),
            OwnedValue::Bool(true),
            some(OwnedValue::String("héllo  ".to_string())),
            some(OwnedValue::String("  héllo".to_string())),
        ])
    );
    assert_eq!(
        instance
            .call(
                "split_state",
                &[OwnedValue::String("one  two\nleft:right:last".to_string())],
            )
            .unwrap(),
        OwnedValue::Tuple(vec![
            OwnedValue::List(vec![
                OwnedValue::String("one".to_string()),
                OwnedValue::String("two".to_string()),
                OwnedValue::String("left:right:last".to_string()),
            ]),
            OwnedValue::List(vec![
                OwnedValue::String("one  two".to_string()),
                OwnedValue::String("left:right:last".to_string()),
            ]),
            some(OwnedValue::Tuple(vec![
                OwnedValue::String("one  two\nleft:right".to_string()),
                OwnedValue::String("last".to_string()),
            ])),
        ])
    );
    assert_eq!(
        instance
            .call("transformations", &[OwnedValue::String("Hé".to_string())],)
            .unwrap(),
        OwnedValue::Tuple(vec![
            OwnedValue::String("He".to_string()),
            OwnedValue::String("HéHé".to_string()),
            OwnedValue::String("hé".to_string()),
            OwnedValue::String("HÉ".to_string()),
        ])
    );
    assert_eq!(
        instance.call("digits", &[]).unwrap(),
        OwnedValue::Tuple(vec![
            OwnedValue::Bool(true),
            OwnedValue::Bool(false),
            some(OwnedValue::I64(15)),
            none(),
        ])
    );
}

#[test]
fn numeric_helpers_check_or_saturate_at_the_declared_width() {
    let mut instance = instance(
        r#"
            fn signed(value: i32) -> (i32, i32, i32, i32) {
                (value.abs(), value.min(4), value.max(4), value.clamp(-2, 2))
            }

            fn checked(value: i32) -> (Option<i32>, Option<i32>, Option<i32>) {
                (value.checked_add(1), value.checked_sub(1), value.checked_mul(2))
            }

            fn saturating(value: i32) -> (i32, i32, i32) {
                (value.saturating_add(1), value.saturating_sub(1), value.saturating_mul(2))
            }

            fn floating(value: f64) -> (f64, f64, f64) {
                (value.min(1.5), value.max(1.5), value.clamp(0.0, 1.0))
            }
        "#,
    );

    assert_eq!(
        instance.call("signed", &[OwnedValue::I32(-8)]).unwrap(),
        OwnedValue::Tuple(vec![
            OwnedValue::I64(8),
            OwnedValue::I64(-8),
            OwnedValue::I64(4),
            OwnedValue::I64(-2),
        ])
    );
    assert_eq!(
        instance
            .call("checked", &[OwnedValue::I32(i32::MAX)])
            .unwrap(),
        OwnedValue::Tuple(vec![
            none(),
            some(OwnedValue::I64(i32::MAX as i64 - 1)),
            none()
        ])
    );
    assert_eq!(
        instance
            .call("saturating", &[OwnedValue::I32(i32::MAX)])
            .unwrap(),
        OwnedValue::Tuple(vec![
            OwnedValue::I64(i64::from(i32::MAX)),
            OwnedValue::I64(i64::from(i32::MAX - 1)),
            OwnedValue::I64(i64::from(i32::MAX)),
        ])
    );
    assert_eq!(
        instance.call("floating", &[OwnedValue::F64(2.0)]).unwrap(),
        OwnedValue::Tuple(vec![
            OwnedValue::F64(1.5),
            OwnedValue::F64(2.0),
            OwnedValue::F64(1.0),
        ])
    );
}

#[test]
fn allocation_limits_and_invalid_numeric_bounds_fail_cleanly() {
    let mut instance = instance(
        r#"
            fn repeated(value: String, count: i32) -> String {
                value.repeat(count)
            }

            fn replaced(value: String, replacement: String) -> String {
                value.replace("", replacement)
            }

            fn invalid_clamp(minimum: f64) -> f64 {
                1.0.clamp(minimum, 1.0)
            }
        "#,
    );

    let negative = instance
        .call(
            "repeated",
            &[OwnedValue::String("a".to_string()), OwnedValue::I32(-1)],
        )
        .unwrap_err()
        .to_string();
    assert!(negative.contains("non-negative count"), "{negative}");

    let oversized = instance
        .call(
            "repeated",
            &[
                OwnedValue::String("a".to_string()),
                OwnedValue::I32(16 * 1024 * 1024 + 1),
            ],
        )
        .unwrap_err()
        .to_string();
    assert!(oversized.contains("exceeds 16777216 bytes"), "{oversized}");

    let replacement = "x".repeat(6 * 1024 * 1024);
    let oversized_replace = instance
        .call(
            "replaced",
            &[
                OwnedValue::String("ab".to_string()),
                OwnedValue::String(replacement),
            ],
        )
        .unwrap_err()
        .to_string();
    assert!(
        oversized_replace.contains("replace output exceeds 16777216 bytes"),
        "{oversized_replace}"
    );

    let invalid_clamp = instance
        .call("invalid_clamp", &[OwnedValue::F64(f64::NAN)])
        .unwrap_err()
        .to_string();
    assert!(
        invalid_clamp.contains("f64::clamp bounds must not be NaN"),
        "{invalid_clamp}"
    );
}

#[test]
fn configured_value_limit_applies_to_native_string_allocations() {
    let limits = Limits {
        max_value_bytes: 256,
        ..Limits::default()
    };
    let engine = Engine::<()>::builder().limits(limits).build().unwrap();
    let compiled = engine
        .compile_source(
            "stdlib",
            "stdlib.hk",
            "fn repeated(value: String, count: i32) -> String { value.repeat(count) }",
        )
        .unwrap();
    let mut instance = engine.instantiate(compiled, ()).unwrap();

    let error = instance
        .call(
            "repeated",
            &[OwnedValue::String("ab".to_string()), OwnedValue::I32(129)],
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("repeat output exceeds 256 bytes"), "{error}");
}
