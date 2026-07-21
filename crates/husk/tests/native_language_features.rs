use husk::{Engine, OwnedValue};

fn instance(source: &str) -> husk::Instance<()> {
    let engine = Engine::<()>::builder().build().unwrap();
    let compiled = engine
        .compile_source("features", "features.hk", source)
        .unwrap();
    engine.instantiate(compiled, ()).unwrap()
}

#[test]
fn executes_ranges_checked_casts_and_formatting_through_hir() {
    let mut instance = instance(
        r#"
            fn range_total() -> i32 {
                let total = 0;
                for value in 1..=4 {
                    total += value;
                }
                total
            }

            fn cast_value() -> i32 {
                let wide = 42 as i64;
                wide as i32
            }

            fn render() -> String {
                let answer = 42;
                format("answer={answer:04x}")
            }
        "#,
    );

    assert_eq!(
        instance.call("range_total", &[]).unwrap(),
        OwnedValue::I64(10)
    );
    assert_eq!(
        instance.call("cast_value", &[]).unwrap(),
        OwnedValue::I64(42)
    );
    assert_eq!(
        instance.call("render", &[]).unwrap(),
        OwnedValue::String("answer=002a".to_string())
    );
}

#[test]
fn ranges_are_nominal_and_slices_support_open_unicode_bounds() {
    let mut instance = instance(
        r#"
            fn slices() -> ([i32], [i32], String) {
                let values = [0, 1, 2, 3, 4];
                (values[..2], values[2..], "aé🦀z"[1..=2])
            }

            fn range_value() -> Range<i32> {
                1..=3
            }

            fn range_methods() -> (bool, bool, bool) {
                (
                    (1..=3).contains(3),
                    (3..3).is_empty(),
                    (3..=3).is_empty(),
                )
            }
        "#,
    );

    assert_eq!(
        instance.call("slices", &[]).unwrap(),
        OwnedValue::Tuple(vec![
            OwnedValue::List(vec![OwnedValue::I64(0), OwnedValue::I64(1)]),
            OwnedValue::List(vec![
                OwnedValue::I64(2),
                OwnedValue::I64(3),
                OwnedValue::I64(4),
            ]),
            OwnedValue::String("é🦀".to_string()),
        ])
    );
    assert_eq!(
        instance.call("range_value", &[]).unwrap(),
        OwnedValue::Range {
            start: 1,
            end: 3,
            inclusive: true,
        }
    );
    assert_eq!(
        instance.call("range_methods", &[]).unwrap(),
        OwnedValue::Tuple(vec![
            OwnedValue::Bool(true),
            OwnedValue::Bool(true),
            OwnedValue::Bool(false),
        ])
    );
}

#[test]
fn question_mark_propagates_result_and_option_variants() {
    let mut instance = instance(
        r#"
            fn add_one(value: Result<i32, String>) -> Result<i32, String> {
                let value = value?;
                Ok(value + 1)
            }

            fn present(value: Option<i32>) -> Option<i32> {
                let value = value?;
                Some(value + 1)
            }
        "#,
    );

    let ok = OwnedValue::Variant {
        type_name: "Result".to_string(),
        case: "Ok".to_string(),
        fields: vec![OwnedValue::I32(41)],
    };
    assert_eq!(
        instance.call("add_one", &[ok]).unwrap(),
        OwnedValue::Variant {
            type_name: "Result".to_string(),
            case: "Ok".to_string(),
            fields: vec![OwnedValue::I64(42)],
        }
    );

    let error = OwnedValue::Variant {
        type_name: "Result".to_string(),
        case: "Err".to_string(),
        fields: vec![OwnedValue::String("stop".to_string())],
    };
    assert_eq!(instance.call("add_one", &[error.clone()]).unwrap(), error);

    let none = OwnedValue::Variant {
        type_name: "Option".to_string(),
        case: "None".to_string(),
        fields: Vec::new(),
    };
    assert_eq!(instance.call("present", &[none.clone()]).unwrap(), none);
}

#[test]
fn checked_casts_reject_fractional_or_out_of_range_values() {
    let mut instance = instance("fn fail() -> i32 { 3.5 as i32 }");
    let error = instance.call("fail", &[]).unwrap_err().to_string();

    assert!(error.contains("not a finite whole number"), "{error}");
    assert!(error.contains("features.hk:1:"), "{error}");
}

#[test]
fn nominal_structs_tuples_enums_and_patterns_keep_their_identity() {
    let mut instance = instance(
        r#"
            struct Point {
                x: i32,
                y: i32,
            }

            enum Message {
                Quit,
                Number(i32),
                Named { value: i32 },
            }

            fn point() -> Point {
                Point { x: 19, y: 23 }
            }

            fn tuple() -> (i32, String) {
                (42, "answer")
            }

            fn make_number() -> Message {
                Message::Number(42)
            }

            fn classify(message: Message) -> i32 {
                match message {
                    Message::Quit => 0,
                    Message::Number(value) => value,
                    Message::Named { value } => value,
                }
            }

            fn option_or_zero(value: Option<i32>) -> i32 {
                if let Some(value) = value {
                    value
                } else {
                    0
                }
            }

            fn option_or_one(value: Option<i32>) -> i32 {
                let Some(value) = value else {
                    return 1;
                };
                value
            }
        "#,
    );

    assert_eq!(
        instance.call("point", &[]).unwrap(),
        OwnedValue::Struct {
            type_name: "Point".to_string(),
            fields: std::collections::BTreeMap::from([
                ("x".to_string(), OwnedValue::I64(19)),
                ("y".to_string(), OwnedValue::I64(23)),
            ]),
        }
    );
    assert_eq!(
        instance.call("tuple", &[]).unwrap(),
        OwnedValue::Tuple(vec![
            OwnedValue::I64(42),
            OwnedValue::String("answer".to_string()),
        ])
    );
    assert_eq!(
        instance.call("make_number", &[]).unwrap(),
        OwnedValue::Variant {
            type_name: "Message".to_string(),
            case: "Number".to_string(),
            fields: vec![OwnedValue::I64(42)],
        }
    );

    let named = OwnedValue::Variant {
        type_name: "Message".to_string(),
        case: "Named".to_string(),
        fields: vec![OwnedValue::Record(std::collections::BTreeMap::from([(
            "value".to_string(),
            OwnedValue::I32(9),
        )]))],
    };
    assert_eq!(
        instance.call("classify", &[named]).unwrap(),
        OwnedValue::I64(9)
    );

    let none = OwnedValue::Variant {
        type_name: "Option".to_string(),
        case: "None".to_string(),
        fields: Vec::new(),
    };
    assert_eq!(
        instance.call("option_or_zero", &[none.clone()]).unwrap(),
        OwnedValue::I64(0)
    );
    assert_eq!(
        instance.call("option_or_one", &[none]).unwrap(),
        OwnedValue::I64(1)
    );
}

#[test]
fn inherent_methods_and_associated_functions_dispatch_to_lowered_hir() {
    let mut instance = instance(
        r#"
            struct Point {
                x: i32,
                y: i32,
            }

            impl Point {
                fn origin() -> Point {
                    Point { x: 0, y: 0 }
                }

                fn sum(&self) -> i32 {
                    self.x + self.y
                }

                fn shifted(self, amount: i32) -> Point {
                    Point {
                        x: self.x + amount,
                        y: self.y + amount,
                    }
                }

                fn translate(&mut self, amount: i32) {
                    self.x += amount;
                    self.y += amount;
                }
            }

            fn method_sum() -> i32 {
                Point { x: 19, y: 23 }.sum()
            }

            fn associated_sum() -> i32 {
                Point::origin().shifted(21).sum()
            }

            fn mutable_sum() -> i32 {
                let mut point = Point { x: 19, y: 21 };
                point.translate(1);
                point.sum()
            }
        "#,
    );

    assert_eq!(
        instance.call("method_sum", &[]).unwrap(),
        OwnedValue::I64(42)
    );
    assert_eq!(
        instance.call("associated_sum", &[]).unwrap(),
        OwnedValue::I64(42)
    );
    assert_eq!(
        instance.call("mutable_sum", &[]).unwrap(),
        OwnedValue::I64(42)
    );
}

#[test]
fn mutable_array_methods_update_the_receiver_cell() {
    let mut instance = instance(
        r#"
            fn mutate() -> (i32, String) {
                let mut values = [3, 1, 2];
                values.push(4);
                values.sort();
                let last = values.pop();
                values.reverse();
                (last, values.join(","))
            }

            fn higher_order() -> (String, i32, bool, i32, i32) {
                let values = [1, 2, 3, 4];
                let rendered = values
                    .map(|value| value * 2)
                    .filter(|value| value > 4)
                    .join(",");
                let sum = values.reduce(|left, right| left + right);
                let has_three = values.some(|value| value == 3);
                let index = values.findLastIndex(|value| value % 2 == 0);
                let total = 0;
                values.forEach(|value| {
                    total += value;
                });
                (rendered, sum, has_three, index, total)
            }
        "#,
    );

    assert_eq!(
        instance.call("mutate", &[]).unwrap(),
        OwnedValue::Tuple(vec![
            OwnedValue::I64(4),
            OwnedValue::String("3,2,1".to_string()),
        ])
    );
    assert_eq!(
        instance.call("higher_order", &[]).unwrap(),
        OwnedValue::Tuple(vec![
            OwnedValue::String("6,8".to_string()),
            OwnedValue::I64(10),
            OwnedValue::Bool(true),
            OwnedValue::I64(3),
            OwnedValue::I64(10),
        ])
    );
}

#[test]
fn closures_capture_shared_cells_and_nest() {
    let mut instance = instance(
        r#"
            fn shared_capture() -> i32 {
                let total = 0;
                let add = |amount: i32| {
                    total += amount;
                    total
                };
                let read = || total;
                add(2);
                add(3);
                read()
            }

            fn nested_capture() -> i32 {
                let offset = 2;
                let factory = |base: i32| |value: i32| base + offset + value;
                factory(30)(10)
            }

            fn make_adder(base: i32) -> fn(i32) -> i32 {
                |value: i32| base + value
            }
        "#,
    );

    assert_eq!(
        instance.call("shared_capture", &[]).unwrap(),
        OwnedValue::I64(5)
    );
    assert_eq!(
        instance.call("nested_capture", &[]).unwrap(),
        OwnedValue::I64(42)
    );

    let handle = instance
        .capture_function("make_adder", &[OwnedValue::I32(40)])
        .unwrap();
    assert_eq!(handle.instance_generation(), instance.generation());
    assert_eq!(
        instance
            .invoke_function(handle, &[OwnedValue::I32(2)])
            .unwrap(),
        OwnedValue::I64(42)
    );
    assert!(instance.release_function(handle).unwrap());
    assert!(!instance.release_function(handle).unwrap());
    let error = instance
        .invoke_function(handle, &[OwnedValue::I32(2)])
        .unwrap_err()
        .to_string();
    assert!(error.contains("released"), "{error}");
}

#[test]
fn generic_functions_and_trait_impl_methods_use_static_source_types() {
    let mut instance = instance(
        r#"
            trait Describe {
                fn describe(&self) -> String;
            }

            trait Kind {
                fn kind(&self) -> String {
                    "number"
                }
            }

            struct Number {
                value: i32,
            }

            impl Describe for Number {
                fn describe(&self) -> String {
                    format("number={}", self.value)
                }
            }

            impl Kind for Number {}

            fn identity<T>(value: T) -> T {
                value
            }

            fn render() -> String {
                let number = identity(Number { value: identity(42) });
                format("{}:{}", number.kind(), number.describe())
            }
        "#,
    );

    assert_eq!(
        instance.call("render", &[]).unwrap(),
        OwnedValue::String("number:number=42".to_string())
    );
}
