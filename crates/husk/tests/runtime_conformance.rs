use husk_runtime::{Host, Value, Vm};

#[derive(Debug, Default)]
struct RecordingHost {
    actions: Vec<(String, Vec<Value>)>,
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
                .ok_or_else(|| anyhow::anyhow!("test::record expects an action name"))?;
            self.actions.push((action.to_string(), args[1..].to_vec()));
            Ok(Value::Unit)
        })
    }
}

fn activate(source: &str) -> anyhow::Result<RecordingHost> {
    let mut host = RecordingHost::default();
    let mut vm = Vm::new();
    vm.load_plugin("conformance", source, &mut host)?;
    Ok(host)
}

#[test]
fn executes_conditionals_while_continue_and_compound_assignment() {
    let host = activate(
        r#"
            pub fn activate() {
                let index = 0;
                let total = 0;
                while index < 5 {
                    index += 1;
                    if index == 3 {
                        continue;
                    }
                    total += index;
                }

                if total == 12 {
                    test::record("Result", index, total);
                } else {
                    test::record("WrongBranch");
                }
            }
        "#,
    )
    .unwrap();

    assert_eq!(
        host.actions,
        [("Result".to_string(), vec![Value::Int(5), Value::Int(12)])]
    );
}

#[test]
fn executes_loop_break_for_in_and_early_return() {
    let host = activate(
        r#"
            fn choose(value: i32) -> String {
                if value == 3 {
                    return "three";
                }
                return "other";
            }

            pub fn activate() {
                let count = 0;
                loop {
                    count += 1;
                    if count >= 3 {
                        break;
                    }
                }

                let joined = "";
                for character in "ab" {
                    joined += character;
                }

                test::record("Result", count, joined, choose(count));
            }
        "#,
    )
    .unwrap();

    assert_eq!(
        host.actions,
        [(
            "Result".to_string(),
            vec![
                Value::Int(3),
                Value::String("ab".to_string()),
                Value::String("three".to_string()),
            ]
        )]
    );
}

#[test]
fn evaluates_supported_operators_and_short_circuits_boolean_calls() {
    let host = activate(
        r#"
            fn touched() -> bool {
                test::record("Touched");
                return true;
            }

            pub fn activate() {
                test::record(
                    "Result",
                    !false,
                    -3,
                    1 + 2 * 3,
                    7 % 4,
                    2 < 3,
                    3 >= 3,
                    false && touched(),
                    true || touched()
                );
            }
        "#,
    )
    .unwrap();

    assert_eq!(
        host.actions,
        [(
            "Result".to_string(),
            vec![
                Value::Bool(true),
                Value::Int(-3),
                Value::Int(7),
                Value::Int(3),
                Value::Bool(true),
                Value::Bool(true),
                Value::Bool(false),
                Value::Bool(true),
            ]
        )]
    );
}

#[test]
fn rejects_non_boolean_conditions() {
    let error = activate(
        r#"
            pub fn activate() {
                if 1 {
                    test::record("Unreachable");
                }
            }
        "#,
    )
    .unwrap_err();
    let rendered = error.to_string();

    assert!(rendered.contains("Husk condition must evaluate to a bool"));
}

#[test]
fn rejects_loop_control_that_escapes_a_loop() {
    for (statement, code, message) in [
        ("break;", "HUSK-R0006", "`break` escaped a loop"),
        ("continue;", "HUSK-R0007", "`continue` escaped a loop"),
    ] {
        let source = format!("pub fn activate() {{ {statement} }}");
        let error = activate(&source).unwrap_err();
        let rendered = error.to_string();

        assert!(rendered.contains(code), "{rendered}");
        assert!(rendered.contains(message), "{rendered}");
    }
}

#[test]
fn executes_builtin_methods_and_if_let() {
    let host = activate(
        r#"
            pub fn activate() {
                let values = [1, 2];
                if let value = values.len() {
                    test::record(
                        "Result",
                        value,
                        "  Husk  ".trim().to_lower_case(),
                        values.includes(2)
                    );
                }
            }
        "#,
    )
    .unwrap();

    assert_eq!(
        host.actions,
        [(
            "Result".to_string(),
            vec![
                Value::Int(2),
                Value::String("husk".to_string()),
                Value::Bool(true),
            ],
        )]
    );
}

#[test]
fn closures_execute_and_share_captured_mutation() {
    let host = activate(
        r#"
            pub fn activate() {
                let count = 0;
                let increment = |amount| {
                    count += amount;
                    count
                };
                increment(2);
                test::record("Result", increment(3), count);
            }
        "#,
    )
    .unwrap();

    assert_eq!(
        host.actions,
        [("Result".to_string(), vec![Value::Int(5), Value::Int(5)])]
    );
}
