//! Static validation of Husk calls against the machine-readable Red host API.
//!
//! [`HOST_API`] loads the embedded `host_api.json` schema, and the validator walks a
//! parsed Husk AST to check action name, call kind, arity, and argument compatibility.
//! This pass runs before activation so an invalid bundled or user plugin can be
//! quarantined without executing host effects.
//!
//! Validation is deliberately conservative: a call that cannot be resolved safely is
//! rejected rather than deferred to a dynamic runtime failure.

use std::{collections::HashMap, ops::Range};

use husk_ast::{
    Expr, ExprKind, File, ImplItemKind, ItemKind, LiteralKind, Span, Stmt, StmtKind, UnaryOp,
};
use husk_diagnostics::{Diagnostic, Report, SourceFile};
use once_cell::sync::Lazy;
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

static HOST_CALLS: Lazy<HashMap<(&'static str, &'static str), &'static HostCall>> =
    Lazy::new(|| {
        Lazy::force(&HOST_API)
            .calls
            .iter()
            .map(|call| ((call.kind.as_str(), call.name.as_str()), call))
            .collect()
    });

struct HostCallSite<'a> {
    kind: &'static str,
    action: &'a str,
    action_span: Range<usize>,
    arguments: &'a [Expr],
}

fn host_call_sites(file: &File) -> Vec<HostCallSite<'_>> {
    let mut calls = Vec::new();
    for item in &file.items {
        match &item.kind {
            ItemKind::Fn { body, .. } => visit_statements(body, &mut calls),
            ItemKind::Trait(definition) => {
                for item in &definition.items {
                    let husk_ast::TraitItemKind::Method(method) = &item.kind;
                    if let Some(body) = &method.default_body {
                        visit_statements(body, &mut calls);
                    }
                }
            }
            ItemKind::Impl(block) => {
                for item in &block.items {
                    if let ImplItemKind::Method(method) = &item.kind {
                        visit_statements(&method.body, &mut calls);
                    }
                }
            }
            ItemKind::Struct { .. }
            | ItemKind::Enum { .. }
            | ItemKind::TypeAlias { .. }
            | ItemKind::ExternBlock { .. }
            | ItemKind::Mod { .. }
            | ItemKind::Use { .. } => {}
        }
    }
    calls
}

fn visit_statements<'a>(statements: &'a [Stmt], calls: &mut Vec<HostCallSite<'a>>) {
    for statement in statements {
        visit_statement(statement, calls);
    }
}

fn visit_statement<'a>(statement: &'a Stmt, calls: &mut Vec<HostCallSite<'a>>) {
    match &statement.kind {
        StmtKind::Let {
            value, else_block, ..
        } => {
            if let Some(value) = value {
                visit_expression(value, calls);
            }
            if let Some(block) = else_block {
                visit_statements(&block.stmts, calls);
            }
        }
        StmtKind::Assign { target, value, .. } => {
            visit_expression(target, calls);
            visit_expression(value, calls);
        }
        StmtKind::Expr(expression) | StmtKind::Semi(expression) => {
            visit_expression(expression, calls);
        }
        StmtKind::Return { value } => {
            if let Some(value) = value {
                visit_expression(value, calls);
            }
        }
        StmtKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            visit_expression(cond, calls);
            visit_statements(&then_branch.stmts, calls);
            if let Some(branch) = else_branch {
                visit_statement(branch, calls);
            }
        }
        StmtKind::While { cond, body } => {
            visit_expression(cond, calls);
            visit_statements(&body.stmts, calls);
        }
        StmtKind::Loop { body } | StmtKind::Block(body) => visit_statements(&body.stmts, calls),
        StmtKind::ForIn { iterable, body, .. } => {
            visit_expression(iterable, calls);
            visit_statements(&body.stmts, calls);
        }
        StmtKind::IfLet {
            scrutinee,
            then_branch,
            else_branch,
            ..
        } => {
            visit_expression(scrutinee, calls);
            visit_statements(&then_branch.stmts, calls);
            if let Some(branch) = else_branch {
                visit_statement(branch, calls);
            }
        }
        StmtKind::Break | StmtKind::Continue => {}
    }
}

fn visit_expression<'a>(expression: &'a Expr, calls: &mut Vec<HostCallSite<'a>>) {
    match &expression.kind {
        ExprKind::Call { callee, args, .. } => {
            if let ExprKind::Path { segments } = &callee.kind {
                let kind = match segments.as_slice() {
                    [module, method] if module.name == "red" && method.name == "execute" => {
                        Some("execute")
                    }
                    [module, method] if module.name == "red" && method.name == "request" => {
                        Some("request")
                    }
                    _ => None,
                };
                if let (Some(kind), Some(action)) = (kind, args.first()) {
                    if let ExprKind::Literal(literal) = &action.kind {
                        if let LiteralKind::String(action) = &literal.kind {
                            calls.push(HostCallSite {
                                kind,
                                action,
                                action_span: literal.span.range.start + 1
                                    ..literal.span.range.end.saturating_sub(1),
                                arguments: &args[1..],
                            });
                        }
                    }
                }
            }
            visit_expression(callee, calls);
            for argument in args {
                visit_expression(argument, calls);
            }
        }
        ExprKind::Field { base, .. } | ExprKind::TupleField { base, .. } => {
            visit_expression(base, calls);
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            visit_expression(receiver, calls);
            for argument in args {
                visit_expression(argument, calls);
            }
        }
        ExprKind::Unary { expr, .. } | ExprKind::Cast { expr, .. } | ExprKind::Try { expr } => {
            visit_expression(expr, calls);
        }
        ExprKind::Binary { left, right, .. } => {
            visit_expression(left, calls);
            visit_expression(right, calls);
        }
        ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            visit_expression(cond, calls);
            visit_expression(then_branch, calls);
            visit_expression(else_branch, calls);
        }
        ExprKind::Match { scrutinee, arms } => {
            visit_expression(scrutinee, calls);
            for arm in arms {
                visit_expression(&arm.expr, calls);
            }
        }
        ExprKind::Block(block) => visit_statements(&block.stmts, calls),
        ExprKind::Struct { fields, .. } => {
            for field in fields {
                visit_expression(&field.value, calls);
            }
        }
        ExprKind::FormatPrint { args, .. }
        | ExprKind::Format { args, .. }
        | ExprKind::Array { elements: args }
        | ExprKind::Tuple { elements: args } => {
            for argument in args {
                visit_expression(argument, calls);
            }
        }
        ExprKind::Closure { body, .. } => visit_expression(body, calls),
        ExprKind::Index { base, index } => {
            visit_expression(base, calls);
            visit_expression(index, calls);
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(start) = start {
                visit_expression(start, calls);
            }
            if let Some(end) = end {
                visit_expression(end, calls);
            }
        }
        ExprKind::Assign { target, value, .. } => {
            visit_expression(target, calls);
            visit_expression(value, calls);
        }
        ExprKind::Literal(_)
        | ExprKind::Ident(_)
        | ExprKind::Path { .. }
        | ExprKind::JsLiteral { .. } => {}
    }
}

#[cfg(test)]
fn validate_source(name: &str, path: &str, source: &str) -> anyhow::Result<()> {
    let parsed = husk_parser::parse_str(source);
    let Some(file) = parsed.file.as_ref() else {
        return Ok(());
    };
    validate_parsed_source(name, path, source, file)
}

pub(crate) fn validate_parsed_source(
    name: &str,
    path: &str,
    source: &str,
    file: &File,
) -> anyhow::Result<()> {
    let mut source_file = None;
    let mut diagnostics = Vec::new();
    for site in host_call_sites(file) {
        let Some(call) = HOST_CALLS.get(&(site.kind, site.action)).copied() else {
            diagnostics.push(
                Diagnostic::new(
                    "HUSK-A0001",
                    format!("unknown Red host API {} call `{}`", site.kind, site.action),
                    source_file
                        .get_or_insert_with(|| SourceFile::new(path, source))
                        .clone(),
                    Span {
                        range: site.action_span.clone(),
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
        let parameters = signature_parameters(&call.signature);
        let required = parameters
            .iter()
            .filter(|(_, _, optional)| !optional)
            .count();
        if site.arguments.len() < required || site.arguments.len() > parameters.len() {
            diagnostics.push(
                Diagnostic::new(
                    "HUSK-A0002",
                    format!(
                        "Red host API {} call `{}` expects {}{} argument(s), got {}",
                        site.kind,
                        site.action,
                        required,
                        if required == parameters.len() {
                            String::new()
                        } else {
                            format!("..{}", parameters.len())
                        },
                        site.arguments.len()
                    ),
                    source_file
                        .get_or_insert_with(|| SourceFile::new(path, source))
                        .clone(),
                    Span {
                        range: site.action_span.clone(),
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
        for (argument, (parameter, expected, optional)) in site.arguments.iter().zip(&parameters) {
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
                        site.kind, site.action
                    ),
                    source_file
                        .get_or_insert_with(|| SourceFile::new(path, source))
                        .clone(),
                    Span {
                        range: site.action_span.clone(),
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

fn literal_type(argument: &Expr) -> Option<&'static str> {
    match &argument.kind {
        ExprKind::Literal(literal) => Some(match &literal.kind {
            LiteralKind::String(_) => "string",
            LiteralKind::Bool(_) => "boolean",
            LiteralKind::Int(_) | LiteralKind::Float(_) => "number",
        }),
        ExprKind::Array { .. } => Some("array"),
        ExprKind::Struct { .. } => Some("object"),
        ExprKind::Unary {
            op: UnaryOp::Neg,
            expr,
        } if matches!(
            &expr.kind,
            ExprKind::Literal(husk_ast::Literal {
                kind: LiteralKind::Int(_) | LiteralKind::Float(_),
                ..
            })
        ) =>
        {
            Some("number")
        }
        ExprKind::Call { callee, args, .. }
            if args.is_empty()
                && matches!(
                    &callee.kind,
                    ExprKind::Path { segments }
                        if matches!(segments.as_slice(), [module, method] if module.name == "red" && method.name == "null")
                ) =>
        {
            Some("null")
        }
        ExprKind::Ident(ident) if ident.name == "null" => Some("null"),
        _ => None,
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
    use regex::Regex;

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

        let picker = HOST_API
            .calls
            .iter()
            .find(|call| call.kind == "execute" && call.name == "OpenPicker")
            .expect("callback-scoped picker must be present in the host API schema");
        assert_eq!(picker.introduced, "0.3.0");
        assert!(picker.signature.contains("PickerHandlers"));

        let archive = HOST_API
            .calls
            .iter()
            .find(|call| call.kind == "execute" && call.name == "AgentArchiveSession")
            .expect("agent archive must be present in the host API schema");
        assert_eq!(archive.signature, "(session_id: String)");
        assert_eq!(archive.introduced, "0.2.0");
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
                    red::execute("SetCursorPosition", -1, 2);
                    red::request("GetBufferText", loaded, 0, 2);
                }
            "#,
        )
        .unwrap();
    }

    #[test]
    fn host_call_validation_ignores_comments_strings_and_embedded_javascript() {
        validate_source(
            "valid",
            "plugins/valid.hk",
            r#"
                // red::execute("RemovedAction");
                /// red::execute("OpenBuffer");
                fn activate() {
                    let example = "red::request(\"RemovedRequest\", callback)";
                    let javascript = js {
                        const example = 'red::execute("RemovedAction")';
                        // red::execute("OpenBuffer");
                    };
                    red::execute("Print", "ready");
                }
            "#,
        )
        .unwrap();
    }

    #[test]
    fn host_call_validation_still_reports_real_calls_next_to_ignored_text() {
        let source = r#"
            // red::execute("RemovedFromComment");
            fn activate() {
                let javascript = js { const example = 'red::execute("RemovedFromJs")'; };
                red::execute("RemovedAction");
                red::execute("OpenBuffer");
                red::execute("OpenBuffer", 42);
            }
        "#;
        let error = validate_source("old", "plugins/old.hk", source)
            .unwrap_err()
            .to_string();

        assert!(error.contains("HUSK-A0001"));
        assert!(error.contains("RemovedAction"));
        assert!(error.contains("HUSK-A0002"));
        assert!(error.contains("expects 1 argument"));
        assert!(error.contains("HUSK-A0003"));
        assert!(error.contains("number literal"));
        assert!(!error.contains("RemovedFromComment"));
        assert!(!error.contains("RemovedFromJs"));
        assert_eq!(
            host_call_sites(husk_parser::parse_str(source).file.as_ref().unwrap())
                .into_iter()
                .map(|site| (&source[site.action_span], site.action))
                .collect::<Vec<_>>(),
            vec![
                ("RemovedAction", "RemovedAction"),
                ("OpenBuffer", "OpenBuffer"),
                ("OpenBuffer", "OpenBuffer"),
            ]
        );
    }

    #[test]
    fn host_call_validation_handles_comments_and_javascript_inside_real_calls() {
        let source = r#"
            fn activate() {
                red::execute(
                    "Print",
                    // commas, brackets ], and parens ) in trivia are not arguments
                    "ready"
                );
                red::execute("UpdatePickerStatus", 1, js {
                    const value = `comma, paren ), bracket ], ${ { nested: [1, 2] } }`;
                    /* }, ), ], */
                    // }, ), ],
                    value;
                });
                red::execute("OpenBuffer");
                red::execute("OpenBuffer", 42);
            }
        "#;
        let error = validate_source("old", "plugins/old.hk", source)
            .unwrap_err()
            .to_string();

        assert_eq!(error.matches("HUSK-A0002").count(), 1);
        assert_eq!(error.matches("HUSK-A0003").count(), 1);
        assert!(error.contains("OpenBuffer"));
        assert!(error.contains("expects 1 argument"));
        assert!(error.contains("number literal"));
        assert!(!error.contains("Print"));
        assert!(!error.contains("UpdatePickerStatus"));
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
