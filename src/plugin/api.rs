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
        if !calls.contains_key(&(kind.as_str(), action.as_str())) {
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
        }
    }
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(anyhow::Error::new(Report::from_diagnostics(diagnostics)))
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
    fn runtime_dispatch_is_covered_by_the_machine_readable_schema() {
        let dispatch = Regex::new(
            r#"(?m)^            "([A-Z][A-Za-z0-9_]*)"(?:\s*\|\s*"([A-Z][A-Za-z0-9_]*)")?\s*=>"#,
        )
        .unwrap();
        let runtime = include_str!("runtime.rs");
        let documented = HOST_API
            .calls
            .iter()
            .map(|call| call.name.as_str())
            .collect::<std::collections::HashSet<_>>();
        let dispatched = dispatch
            .captures_iter(runtime)
            .flat_map(|captures| {
                [captures.get(1), captures.get(2)]
                    .into_iter()
                    .flatten()
                    .map(|capture| capture.as_str())
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
}
