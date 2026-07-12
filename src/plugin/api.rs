use husk_ast::Span;
use husk_diagnostics::{Diagnostic, Report, SourceFile};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct HostApiSchema {
    pub version: String,
    pub calls: Vec<HostCall>,
}

#[derive(Debug, Deserialize)]
pub struct HostCall {
    pub name: String,
    pub kind: String,
    pub signature: String,
    pub introduced: String,
}

pub static HOST_API: Lazy<HostApiSchema> = Lazy::new(|| {
    let schema: HostApiSchema = serde_json::from_str(include_str!("host_api.json"))
        .expect("the embedded plugin host API schema must be valid");
    assert!(
        schema
            .calls
            .iter()
            .all(|call| !call.signature.is_empty() && !call.introduced.is_empty()),
        "every host API call needs a signature and introduction version"
    );
    schema
});

static HOST_CALL: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"red::(execute|request)\s*\(\s*"([^"]+)""#).expect("host call regex must compile")
});

pub fn validate_source(name: &str, path: &str, source: &str) -> anyhow::Result<()> {
    let calls = HOST_API
        .calls
        .iter()
        .map(|call| ((call.kind.as_str(), call.name.as_str()), call))
        .collect::<std::collections::HashMap<_, _>>();
    let source_file = SourceFile::new(path, source);
    let mut diagnostics = Vec::new();
    for captures in HOST_CALL.captures_iter(source) {
        let kind = captures.get(1).expect("kind capture exists");
        let action = captures.get(2).expect("action capture exists");
        let Some(call) = calls.get(&(kind.as_str(), action.as_str())) else {
            diagnostics.push(
                Diagnostic::new(
                    "HUSK-A0001",
                    format!(
                        "unknown Red host API {} call `{}`",
                        kind.as_str(),
                        action.as_str()
                    ),
                    source_file.clone(),
                    Span {
                        range: action.start()..action.end(),
                        file: None,
                    },
                    "not present in the canonical host API schema",
                )
                .with_note(format!(
                    "plugin `{name}` targets host API {}; see docs/PLUGIN_API.md",
                    HOST_API.version
                )),
            );
            continue;
        };
        let Some(arguments) = call_arguments(source, captures.get(0).unwrap().end()) else {
            continue;
        };
        let parameters = signature_parameters(&call.signature);
        let required = parameters
            .iter()
            .filter(|(_, _, optional)| !optional)
            .count();
        if arguments.len() < required || arguments.len() > parameters.len() {
            diagnostics.push(
                Diagnostic::new(
                    "HUSK-A0002",
                    format!(
                        "Red host API {} call `{}` expects {}{} argument(s), got {}",
                        kind.as_str(),
                        action.as_str(),
                        required,
                        if required == parameters.len() {
                            String::new()
                        } else {
                            format!("..{}", parameters.len())
                        },
                        arguments.len()
                    ),
                    source_file.clone(),
                    Span {
                        range: action.start()..action.end(),
                        file: None,
                    },
                    format!("expected signature {}", call.signature),
                )
                .with_note(format!(
                    "plugin `{name}` targets host API {}; see docs/PLUGIN_API.md",
                    HOST_API.version
                )),
            );
            continue;
        }
        for (argument, (parameter, expected, optional)) in arguments.iter().zip(&parameters) {
            let Some(actual) = literal_type(argument) else {
                continue;
            };
            if (*optional && actual == "null") || literal_matches(expected, actual) {
                continue;
            }
            diagnostics.push(
                Diagnostic::new(
                    "HUSK-A0003",
                    format!(
                        "Red host API {} call `{}` received a {actual} literal for `{parameter}: {expected}`",
                        kind.as_str(),
                        action.as_str()
                    ),
                    source_file.clone(),
                    Span {
                        range: action.start()..action.end(),
                        file: None,
                    },
                    format!("expected signature {}", call.signature),
                )
                .with_note(format!(
                    "plugin `{name}` targets host API {}; see docs/PLUGIN_API.md",
                    HOST_API.version
                )),
            );
        }
    }
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(anyhow::Error::new(Report::from_diagnostics(diagnostics)))
    }
}

fn signature_parameters(signature: &str) -> Vec<(&str, &str, bool)> {
    let body = signature
        .strip_prefix('(')
        .and_then(|body| body.strip_suffix(')'))
        .unwrap_or(signature)
        .trim();
    if body.is_empty() {
        return Vec::new();
    }
    body.split(',')
        .filter_map(|parameter| {
            let (name, ty) = parameter.trim().split_once(':')?;
            let name = name.trim();
            let ty = ty.trim();
            Some((
                name.trim_end_matches('?'),
                ty.trim_end_matches('?'),
                name.ends_with('?') || ty.ends_with('?'),
            ))
        })
        .collect()
}

fn call_arguments(source: &str, mut cursor: usize) -> Option<Vec<&str>> {
    let bytes = source.as_bytes();
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    if bytes.get(cursor) == Some(&b')') {
        return Some(Vec::new());
    }
    if bytes.get(cursor) != Some(&b',') {
        return None;
    }
    cursor += 1;
    let mut start = cursor;
    let mut nesting = 0usize;
    let mut quote = None;
    let mut escaped = false;
    let mut arguments = Vec::new();
    while let Some(&byte) = bytes.get(cursor) {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' => quote = Some(byte),
            b'(' | b'[' | b'{' => nesting += 1,
            b')' if nesting == 0 => {
                let argument = source[start..cursor].trim();
                if !argument.is_empty() {
                    arguments.push(argument);
                }
                return Some(arguments);
            }
            b')' | b']' | b'}' => nesting = nesting.saturating_sub(1),
            b',' if nesting == 0 => {
                arguments.push(source[start..cursor].trim());
                start = cursor + 1;
            }
            _ => {}
        }
        cursor += 1;
    }
    None
}

fn literal_type(argument: &str) -> Option<&'static str> {
    let argument = argument.trim();
    if argument.starts_with('"') || argument.starts_with('\'') {
        Some("string")
    } else if matches!(argument, "true" | "false") {
        Some("boolean")
    } else if argument.starts_with('[') {
        Some("array")
    } else if argument.starts_with('{') || argument.contains(" {") {
        Some("object")
    } else if argument == "null" || argument == "red::null()" {
        Some("null")
    } else if argument
        .strip_prefix('-')
        .unwrap_or(argument)
        .chars()
        .all(|character| character.is_ascii_digit() || matches!(character, '_' | '.'))
        && argument.chars().any(|character| character.is_ascii_digit())
    {
        Some("number")
    } else {
        None
    }
}

fn literal_matches(expected: &str, actual: &str) -> bool {
    match expected {
        "String" => actual == "string",
        "bool" => actual == "boolean",
        "i32" | "u32" | "usize" => actual == "number",
        ty if ty.starts_with('[') => actual == "array",
        ty if ty.starts_with("fn(") => false,
        "Json" => true,
        _ => actual == "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_matches_runtime_contract() {
        assert_eq!(HOST_API.version, super::super::RED_HOST_API_VERSION);
        assert!(HOST_API
            .calls
            .iter()
            .all(|call| { !call.signature.is_empty() && !call.introduced.is_empty() }));
        let mut calls = std::collections::HashSet::new();
        for call in &HOST_API.calls {
            assert!(matches!(call.kind.as_str(), "execute" | "request"));
            assert!(
                calls.insert((call.kind.as_str(), call.name.as_str())),
                "duplicate host API call: {} {}",
                call.kind,
                call.name
            );
        }

        let composer = HOST_API
            .calls
            .iter()
            .find(|call| call.kind == "execute" && call.name == "OpenAgentComposer")
            .expect("agent composer must be present in the host API schema");
        assert_eq!(composer.introduced, "0.2.0");
    }

    #[test]
    fn unknown_literal_host_call_has_a_source_diagnostic() {
        let error = validate_source(
            "old",
            "plugins/old.hk",
            r#"fn activate() { red::execute("RemovedAction"); }"#,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("HUSK-A0001"));
        assert!(error.contains("RemovedAction"));
        assert!(error.contains("docs/PLUGIN_API.md"));
    }

    #[test]
    fn invalid_host_call_arity_has_a_source_diagnostic() {
        let error = validate_source(
            "old",
            "plugins/old.hk",
            r#"fn activate() { red::execute("OpenBuffer"); }"#,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("HUSK-A0002"));
        assert!(error.contains("OpenBuffer"));
        assert!(error.contains("expects 1 argument"));
    }

    #[test]
    fn invalid_host_call_literal_types_have_source_diagnostics() {
        let error = validate_source(
            "old",
            "plugins/old.hk",
            r#"
                fn loaded(result: Json) {}
                fn activate() {
                    red::execute("OpenBuffer", 42);
                    red::request("GetBufferText", loaded, "bad");
                }
            "#,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("HUSK-A0003"));
        assert!(error.contains("OpenBuffer"));
        assert!(error.contains("GetBufferText"));
        assert!(error.contains("number literal"));
        assert!(error.contains("string literal"));
    }

    #[test]
    fn host_call_validation_accepts_nested_arguments_and_optional_values() {
        validate_source(
            "valid",
            "plugins/valid.hk",
            r#"
                fn loaded(result: Json) {}
                fn activate() {
                    red::execute("OpenDynamicPicker", "Items", 1, [Json { label: "a,b" }], Json {});
                    red::execute("AgentPermissionResponse", "request", red::null());
                    red::request("GetBufferText", loaded, 0, 2);
                }
            "#,
        )
        .unwrap();
    }

    #[test]
    fn runtime_dispatch_is_covered_by_the_machine_readable_schema() {
        let dispatch = Regex::new(
            r#"(?m)^            "([A-Z][A-Za-z0-9_]*)"(?:\s*\|\s*"([A-Z][A-Za-z0-9_]*)")?\s*=>"#,
        )
        .unwrap();
        let runtime = include_str!("runtime.rs");
        let request_start = Regex::new(r"(?m)^    fn request\(\r?$")
            .unwrap()
            .find(runtime)
            .unwrap()
            .start();
        let request_end = runtime[request_start..].find("    fn query(").unwrap() + request_start;
        let documented = HOST_API
            .calls
            .iter()
            .map(|call| (call.kind.as_str(), call.name.as_str()))
            .collect::<std::collections::HashSet<_>>();
        let dispatched = dispatch
            .captures_iter(runtime)
            .flat_map(|captures| {
                let kind = if captures.get(0).unwrap().start() >= request_start
                    && captures.get(0).unwrap().start() < request_end
                {
                    "request"
                } else {
                    "execute"
                };
                [captures.get(1), captures.get(2)]
                    .into_iter()
                    .flatten()
                    .map(|capture| (kind, capture.as_str()))
                    .collect::<Vec<_>>()
            })
            .collect::<std::collections::HashSet<_>>();
        let missing = dispatched
            .difference(&documented)
            .copied()
            .collect::<Vec<_>>();
        assert!(
            missing.is_empty(),
            "runtime calls missing from schema: {missing:?}"
        );
    }

    #[test]
    fn frequently_used_host_signatures_match_runtime_arguments() {
        let signatures = HOST_API
            .calls
            .iter()
            .map(|call| (call.name.as_str(), call.signature.as_str()))
            .collect::<std::collections::HashMap<_, _>>();
        let expected = [
            ("ShowDialog", "()"),
            ("OpenBuffer", "(name: String)"),
            (
                "OpenAgentComposer",
                "(title: String, id: i32, query: String, history: [String])",
            ),
            (
                "UpdateWindowBar",
                "(id: String, window_id: i32, segments: [WindowBarSegment])",
            ),
            ("CloseWindowBar", "(id: String, window_id?: i32)"),
            ("RecordCursorMoved", "(event: Json)"),
            ("RecordModeChanged", "(event: Json)"),
            ("RecordSearchHighlighted", "(event: Json)"),
            ("RecordSearchCleared", "(event: Json)"),
            (
                "GetBufferText",
                "(callback: fn(Json), start_line?: i32, end_line?: i32)",
            ),
            ("DocumentSymbols", "(callback: fn(Json), buffer_id?: i32)"),
            (
                "References",
                "(callback: fn(Json), include_declaration?: bool)",
            ),
            (
                "CharIndexToDisplayColumn",
                "(callback: fn(Json), x: i32, y: i32)",
            ),
            (
                "DisplayColumnToCharIndex",
                "(callback: fn(Json), column: i32, y: i32)",
            ),
        ];
        for (name, signature) in expected {
            assert_eq!(signatures.get(name), Some(&signature), "{name}");
        }
    }
}
