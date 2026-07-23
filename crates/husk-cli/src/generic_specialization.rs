use std::fmt;

use anyhow::{Context, bail};

use crate::{
    AdapterCallableInspection, AdapterResourceInspection, ApiItemInspection, PublicApiInspection,
    WitCallableInspection,
};

const MAX_SPECIALIZATION_BYTES: usize = 4096;
const MAX_TYPE_DEPTH: usize = 16;
const SERDE_JSON_VALUE_PROFILE: &str = "serde_json/value@1";
const SERDE_JSON_VALUE_SPECIALIZATIONS: [&str; 3] = [
    "serde_json::from_str<serde_json::Value>",
    "serde_json::to_string<serde_json::Value>",
    "serde_json::to_string_pretty<serde_json::Value>",
];

/// One finite, ahead-of-time instantiation of a public Rust function.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct GenericSpecialization {
    function: String,
    type_arguments: Vec<SpecializationType>,
}

/// A validated Rust type that may appear in an adapter specialization.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SpecializationType {
    path: String,
    arguments: Vec<Self>,
}

impl GenericSpecialization {
    pub(crate) fn parse(source: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !source.is_empty() && source.len() <= MAX_SPECIALIZATION_BYTES,
            "generic specialization must contain between 1 and {MAX_SPECIALIZATION_BYTES} bytes"
        );
        let mut parser = SpecializationParser::new(source);
        let function = parser.parse_path().context("parse generic function path")?;
        if parser.consume_str("::") {
            anyhow::ensure!(
                parser.peek() == Some('<'),
                "expected `<` after the specialization turbofish"
            );
        }
        let type_arguments = parser
            .parse_type_arguments(0)
            .context("parse generic type arguments")?;
        parser.skip_whitespace();
        anyhow::ensure!(
            parser.is_finished(),
            "unexpected trailing specialization input"
        );
        anyhow::ensure!(
            !type_arguments.is_empty(),
            "generic specialization must include at least one concrete type"
        );
        Ok(Self {
            function,
            type_arguments,
        })
    }

    pub(crate) fn function(&self) -> &str {
        &self.function
    }

    pub(crate) fn type_arguments(&self) -> &[SpecializationType] {
        &self.type_arguments
    }

    pub(crate) fn canonical(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for GenericSpecialization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}<", self.function)?;
        for (index, argument) in self.type_arguments.iter().enumerate() {
            if index > 0 {
                formatter.write_str(", ")?;
            }
            write!(formatter, "{argument}")?;
        }
        formatter.write_str(">")
    }
}

impl SpecializationType {
    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    pub(crate) fn arguments(&self) -> &[Self] {
        &self.arguments
    }
}

impl fmt::Display for SpecializationType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.path)?;
        if !self.arguments.is_empty() {
            formatter.write_str("<")?;
            for (index, argument) in self.arguments.iter().enumerate() {
                if index > 0 {
                    formatter.write_str(", ")?;
                }
                write!(formatter, "{argument}")?;
            }
            formatter.write_str(">")?;
        }
        Ok(())
    }
}

/// Parse and canonicalize user input before it reaches generated Rust source.
pub(crate) fn parse_specializations(
    sources: &[String],
) -> anyhow::Result<Vec<GenericSpecialization>> {
    let mut specializations = sources
        .iter()
        .map(|source| {
            GenericSpecialization::parse(source)
                .with_context(|| format!("invalid generic specialization `{source}`"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    specializations.sort();
    for pair in specializations.windows(2) {
        anyhow::ensure!(
            pair[0] != pair[1],
            "duplicate generic specialization `{}`",
            pair[0]
        );
    }
    Ok(specializations)
}

/// Resolve user selections and provide a versioned, reproducible default.
pub(crate) fn select_specializations(
    crate_name: &str,
    crate_version: &str,
    requested: &[String],
) -> anyhow::Result<Vec<GenericSpecialization>> {
    let version = semver::Version::parse(crate_version)
        .with_context(|| format!("invalid inspected crate version `{crate_version}`"))?;
    let defaults = if requested.is_empty() && crate_name == "serde_json" && version.major == 1 {
        SERDE_JSON_VALUE_SPECIALIZATIONS
            .iter()
            .map(|specialization| (*specialization).to_string())
            .collect::<Vec<_>>()
    } else {
        requested.to_vec()
    };
    let specializations = parse_specializations(&defaults)?;
    let crate_path = crate_name.replace('-', "_");
    for specialization in &specializations {
        anyhow::ensure!(
            specialization
                .function()
                .strip_prefix(&crate_path)
                .is_some_and(|suffix| suffix.starts_with("::")),
            "generic specialization `{specialization}` does not belong to crate `{crate_name}`"
        );
    }
    Ok(specializations)
}

/// Materialize validated Rust instantiations as portable, concrete WIT exports.
pub(crate) fn apply_specializations(
    public_api: &mut PublicApiInspection,
    crate_name: &str,
    crate_version: &str,
    specializations: &[GenericSpecialization],
) -> anyhow::Result<Option<String>> {
    if specializations.is_empty() {
        return Ok(None);
    }
    let version = semver::Version::parse(crate_version)
        .with_context(|| format!("invalid inspected crate version `{crate_version}`"))?;
    anyhow::ensure!(
        crate_name == "serde_json" && version.major == 1,
        "crate `{crate_name}` does not yet provide a portable lowering profile for generic specializations"
    );

    let mut items = Vec::with_capacity(specializations.len());
    for specialization in specializations {
        anyhow::ensure!(
            specialization.type_arguments().len() == 1
                && specialization.type_arguments()[0].path() == "serde_json::Value"
                && specialization.type_arguments()[0].arguments().is_empty(),
            "the `{SERDE_JSON_VALUE_PROFILE}` profile requires the concrete type `serde_json::Value`; got `{specialization}`"
        );
        let (kind, owner_resource, declaration, implementation, signature) = match specialization
            .function()
        {
            "serde_json::from_str" => (
                "function",
                None,
                "from-str-value: func(input: string) -> result<value, string>;",
                "fn from_str_value(input: String) -> Result<Value, String> {\n    inspected::from_str::<inspected::Value>(&input)\n        .map(|value| Value::new(AdapterValue(value)))\n        .map_err(|error| error.to_string())\n}",
                "fn serde_json::from_str<serde_json::Value>(input: &str) -> Result<serde_json::Value, serde_json::Error>",
            ),
            "serde_json::to_string" => (
                "method",
                Some("value"),
                "to-string: func() -> result<string, string>;",
                "fn to_string(&self) -> Result<String, String> {\n    inspected::to_string::<inspected::Value>(&self.0)\n        .map_err(|error| error.to_string())\n}",
                "fn serde_json::to_string<serde_json::Value>(value: &serde_json::Value) -> Result<String, serde_json::Error>",
            ),
            "serde_json::to_string_pretty" => (
                "method",
                Some("value"),
                "to-string-pretty: func() -> result<string, string>;",
                "fn to_string_pretty(&self) -> Result<String, String> {\n    inspected::to_string_pretty::<inspected::Value>(&self.0)\n        .map_err(|error| error.to_string())\n}",
                "fn serde_json::to_string_pretty<serde_json::Value>(value: &serde_json::Value) -> Result<String, serde_json::Error>",
            ),
            function => anyhow::bail!(
                "the `{SERDE_JSON_VALUE_PROFILE}` profile has no portable lowering for `{function}`"
            ),
        };
        let canonical = specialization.canonical();
        items.push(ApiItemInspection {
            path: canonical.clone(),
            kind,
            signature: signature.to_string(),
            compatibility: "compatible",
            reason: None,
            specialization: Some(canonical),
            wit: Some(WitCallableInspection {
                owner_resource: owner_resource.map(str::to_string),
                declaration: declaration.to_string(),
                resources: vec!["value".to_string()],
                resource_types: vec![AdapterResourceInspection {
                    wit_name: "value".to_string(),
                    rust_path: "serde_json::Value".to_string(),
                }],
                adapter: Some(AdapterCallableInspection {
                    implementation: implementation.to_string(),
                }),
            }),
        });
    }

    if public_api.status != "available" {
        public_api.status = "available";
        public_api.source = Some(format!("built-in {SERDE_JSON_VALUE_PROFILE} profile"));
        public_api.unavailable_reason = None;
    }
    if !public_api
        .resources
        .iter()
        .any(|resource| resource == "serde_json::Value")
    {
        public_api.resources.push("serde_json::Value".to_string());
        public_api.resources.sort();
    }
    public_api.items.extend(items);
    public_api
        .items
        .sort_unstable_by(|left, right| left.path.cmp(&right.path));
    public_api.compatible_items = public_api
        .items
        .iter()
        .filter(|item| item.compatibility == "compatible")
        .count();
    public_api.specializable_items = public_api
        .items
        .iter()
        .filter(|item| item.compatibility == "specializable")
        .count();
    public_api.incompatible_items =
        public_api.items.len() - public_api.compatible_items - public_api.specializable_items;
    Ok(Some(SERDE_JSON_VALUE_PROFILE.to_string()))
}

struct SpecializationParser<'a> {
    source: &'a str,
    position: usize,
}

impl<'a> SpecializationParser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            position: 0,
        }
    }

    fn is_finished(&self) -> bool {
        self.position == self.source.len()
    }

    fn peek(&self) -> Option<char> {
        self.source[self.position..].chars().next()
    }

    fn skip_whitespace(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.position += self.peek().map_or(0, char::len_utf8);
        }
    }

    fn consume_str(&mut self, expected: &str) -> bool {
        self.skip_whitespace();
        if self.source[self.position..].starts_with(expected) {
            self.position += expected.len();
            true
        } else {
            false
        }
    }

    fn parse_identifier(&mut self) -> anyhow::Result<String> {
        self.skip_whitespace();
        let start = self.position;
        let Some(first) = self.peek() else {
            bail!("expected a Rust identifier");
        };
        anyhow::ensure!(
            first == '_' || first.is_ascii_alphabetic(),
            "expected a Rust identifier"
        );
        self.position += first.len_utf8();
        while self
            .peek()
            .is_some_and(|character| character == '_' || character.is_ascii_alphanumeric())
        {
            self.position += 1;
        }
        Ok(self.source[start..self.position].to_string())
    }

    fn parse_path(&mut self) -> anyhow::Result<String> {
        let mut path = self.parse_identifier()?;
        loop {
            let saved = self.position;
            if !self.consume_str("::") {
                break;
            }
            self.skip_whitespace();
            if self.peek() == Some('<') {
                self.position = saved;
                break;
            }
            let next = self.parse_identifier()?;
            path.push_str("::");
            path.push_str(&next);
        }
        Ok(path)
    }

    fn parse_type_arguments(&mut self, depth: usize) -> anyhow::Result<Vec<SpecializationType>> {
        anyhow::ensure!(
            depth < MAX_TYPE_DEPTH,
            "generic specialization exceeds the maximum type depth of {MAX_TYPE_DEPTH}"
        );
        anyhow::ensure!(self.consume_str("<"), "expected concrete type arguments");
        let mut arguments = Vec::new();
        loop {
            self.skip_whitespace();
            anyhow::ensure!(self.peek() != Some('>'), "empty type argument");
            let path = self.parse_path()?;
            let nested = if self.peek_after_whitespace() == Some('<') {
                self.parse_type_arguments(depth + 1)?
            } else {
                Vec::new()
            };
            arguments.push(SpecializationType {
                path,
                arguments: nested,
            });
            if self.consume_str(">") {
                break;
            }
            anyhow::ensure!(
                self.consume_str(","),
                "expected `,` or `>` in type arguments"
            );
        }
        Ok(arguments)
    }

    fn peek_after_whitespace(&mut self) -> Option<char> {
        self.skip_whitespace();
        self.peek()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GenericSpecialization, SERDE_JSON_VALUE_PROFILE, apply_specializations,
        parse_specializations, select_specializations,
    };

    #[test]
    fn parses_and_canonicalizes_qualified_generic_specializations() {
        let specialization =
            GenericSpecialization::parse("serde_json::from_str::<serde_json::Value>").unwrap();

        assert_eq!(specialization.function(), "serde_json::from_str");
        assert_eq!(
            specialization.type_arguments()[0].path(),
            "serde_json::Value"
        );
        assert_eq!(
            specialization.canonical(),
            "serde_json::from_str<serde_json::Value>"
        );
    }

    #[test]
    fn preserves_nested_concrete_type_arguments() {
        let specialization =
            GenericSpecialization::parse("example::decode<Result<Vec<String>, example::Error>>")
                .unwrap();

        assert_eq!(
            specialization.canonical(),
            "example::decode<Result<Vec<String>, example::Error>>"
        );
        assert_eq!(specialization.type_arguments()[0].arguments().len(), 2);
    }

    #[test]
    fn rejects_source_injection_and_incomplete_type_arguments() {
        for source in [
            "serde_json::from_str",
            "serde_json::from_str<>",
            "serde_json::from_str<Value>; panic!()",
            "serde_json::from_str<&Value>",
            "serde_json::from_str<Value,>",
        ] {
            assert!(GenericSpecialization::parse(source).is_err(), "{source}");
        }
    }

    #[test]
    fn rejects_duplicate_canonical_specializations() {
        let error = parse_specializations(&[
            "serde_json::from_str<serde_json::Value>".to_string(),
            "serde_json::from_str::<serde_json::Value>".to_string(),
        ])
        .unwrap_err();

        assert!(error.to_string().contains("duplicate"), "{error:#}");
    }

    #[test]
    fn automatically_selects_the_versioned_serde_json_value_profile() {
        let specializations = select_specializations("serde_json", "1.0.151", &[]).unwrap();

        assert_eq!(
            specializations
                .iter()
                .map(GenericSpecialization::canonical)
                .collect::<Vec<_>>(),
            [
                "serde_json::from_str<serde_json::Value>",
                "serde_json::to_string<serde_json::Value>",
                "serde_json::to_string_pretty<serde_json::Value>",
            ]
        );
    }

    #[test]
    fn rejects_specializations_from_a_different_crate() {
        let error = select_specializations(
            "serde_json",
            "1.0.151",
            &["another_crate::decode<serde_json::Value>".to_string()],
        )
        .unwrap_err();

        assert!(error.to_string().contains("does not belong"), "{error:#}");
    }

    #[test]
    fn rejects_unversioned_or_unsupported_serde_json_lowerings() {
        let specializations =
            parse_specializations(&["serde_json::from_str<serde_json::Map>".to_string()]).unwrap();
        let mut public_api = crate::unavailable_public_api("fixture has no Rustdoc cache");
        let error =
            apply_specializations(&mut public_api, "serde_json", "1.0.151", &specializations)
                .unwrap_err();

        assert!(error.to_string().contains("serde_json::Value"), "{error:#}");
        assert_eq!(public_api.status, "unavailable");
    }

    #[test]
    fn serde_json_profile_generates_valid_wit_and_concrete_guest_code() {
        let specializations = select_specializations("serde_json", "1.0.151", &[]).unwrap();
        let mut public_api = crate::unavailable_public_api("fixture has no Rustdoc cache");
        let profile =
            apply_specializations(&mut public_api, "serde_json", "1.0.151", &specializations)
                .unwrap();

        assert_eq!(profile.as_deref(), Some(SERDE_JSON_VALUE_PROFILE));
        assert_eq!(public_api.status, "available");
        assert_eq!(public_api.compatible_items, 3);
        assert_eq!(public_api.resources, ["serde_json::Value"]);

        let inspection = crate::CrateInspection {
            name: "serde_json".to_string(),
            version: "1.0.151".to_string(),
            source: "registry+https://github.com/rust-lang/crates.io-index".to_string(),
            rust_version: None,
            license: None,
            repository: None,
            enabled_features: vec!["std".to_string(), "preserve_order".to_string()],
            available_features: vec!["std".to_string(), "preserve_order".to_string()],
            targets: Vec::new(),
            has_library: true,
            has_build_script: false,
            native_links: None,
            readiness: "ready-for-adapter-design",
            blockers: Vec::new(),
            warnings: Vec::new(),
            next_step: "generate adapter",
            specialization_profile: profile,
            specializations: specializations
                .iter()
                .map(GenericSpecialization::canonical)
                .collect(),
            public_api,
        };
        let generated = crate::render_adapter_package(&inspection, &[]).unwrap();

        wit_parser::Resolve::default()
            .push_str("serde-json.wit", &generated.wit)
            .unwrap();
        assert!(generated.wit.contains("resource value {"));
        assert!(
            generated
                .wit
                .contains("from-str-value: func(input: string)")
        );
        assert!(generated.wit.contains("to-string-pretty: func()"));
        assert!(
            generated
                .source
                .contains("inspected::from_str::<inspected::Value>(&input)")
        );
        assert!(
            generated
                .source
                .contains("inspected::to_string_pretty::<inspected::Value>(&self.0)")
        );
        assert!(generated.manifest.contains("\"preserve_order\""));
        let report: serde_json::Value = serde_json::from_str(&generated.report).unwrap();
        assert_eq!(report["specialization_profile"], SERDE_JSON_VALUE_PROFILE);
        assert_eq!(report["specializations"].as_array().unwrap().len(), 3);
        assert_eq!(report["items"].as_array().unwrap().len(), 3);
    }
}
