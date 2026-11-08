//! Codex-style `-c`/`--config key=value` overrides.
//!
//! A value that parses as TOML is kept as that TOML value. Anything else is a
//! plain string, so `theme=onedark` does not need quotes. Full TOML documents
//! are still accepted, which preserves `--config-override` fragments.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use super::{embedded_config_value, find_unquoted, known_schema_path, render_path};

const EXTRA_SCHEMA_PATHS: &[&[&str]] = &[
    &["theme"],
    &["log_file"],
    &["mouse_scroll_lines"],
    &["scrolloff"],
    &["wrap"],
    &["relative_line_numbers"],
    &["splash"],
    &["show_whats_new"],
    &["fetch_release_notes"],
    &["disable_ai"],
    &["show_diagnostics"],
    &["window_borders_ascii"],
    &["markdown_tables"],
    &["markdown_conceal"],
    &["markdown_mermaid"],
    &["markdown_frontmatter"],
    &["lsp", "enabled"],
    &["lsp", "format_on_save"],
    &["formatting", "on_save"],
    &["formatting", "on_paste"],
    &["languages", "rust", "indent_width"],
    &["languages", "rs", "indent_width"],
    &["search", "ignorecase"],
    &["search", "smartcase"],
    &["search", "incsearch"],
    &["updates", "mode"],
    &["cursor", "normal"],
    &["cursor", "insert"],
    &["agent", "command"],
    &["diagnostics", "gutter_signs"],
    &["diagnostics", "icon_style"],
    &["statusline", "left"],
    &["statusline", "right"],
    &["clipboard", "enabled"],
    &["completion", "enabled"],
    &["signature_help", "auto_trigger"],
    &["key_hints", "enabled"],
    &["key_hints", "delay_ms"],
];

pub(super) fn unknown_override_path(value: &toml::Value) -> Option<Vec<String>> {
    let mut path = super::first_unknown_path(value, &[])?;
    loop {
        let current = super::value_at_path(value, &path)?;
        let Some(table) = current.as_table() else {
            break;
        };
        let mut keys = table.keys();
        let Some(key) = keys.next() else {
            break;
        };
        if keys.next().is_some() {
            break;
        }
        path.push(key.clone());
    }
    Some(path)
}

pub(super) fn parse_config_override(raw: &str) -> Result<toml::Value, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(expected_pair());
    }
    if let Ok(value) = toml::from_str::<toml::Value>(trimmed) {
        if value.is_table() {
            return Ok(value);
        }
    }
    parse_key_value(trimmed)
}

pub(super) fn suggest_config_key(rendered: &str) -> Option<String> {
    let mut best: Option<(usize, &str)> = None;
    for candidate in suggestion_keys() {
        let distance = levenshtein(rendered, candidate);
        if distance == 0 || distance > 3 {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|(best_distance, _)| distance < *best_distance)
        {
            best = Some((distance, candidate.as_str()));
        }
    }
    best.map(|(_, key)| key.to_string())
}

pub(super) fn override_key_label(value: &toml::Value) -> String {
    let mut leaves = Vec::new();
    collect_leaves(value, &[], &mut leaves);
    if leaves.is_empty() {
        return "the override".to_string();
    }
    leaves
        .into_iter()
        .map(|path| render_path(&path))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn quote_flag(raw: &str) -> String {
    if !raw.contains('\'') {
        format!("'{raw}'")
    } else if !raw.contains('"') {
        format!("\"{raw}\"")
    } else {
        format!("{raw:?}")
    }
}

pub(super) fn type_error_detail(error: &anyhow::Error) -> String {
    let text = error.to_string();
    let line = text
        .lines()
        .next()
        .unwrap_or("value does not match the expected configuration type");
    let line = line.trim_start_matches("failed to deserialize merged config: ");
    if line.chars().count() > 200 {
        let truncated = line.chars().take(200).collect::<String>();
        format!("{truncated}…")
    } else {
        line.to_string()
    }
}

fn parse_key_value(raw: &str) -> Result<toml::Value, String> {
    let Some(equals) = find_unquoted(raw, '=') else {
        return Err(expected_pair());
    };
    let key = raw[..equals].trim();
    let value_raw = raw[equals + 1..].trim();
    if key.is_empty() {
        return Err("missing key before '='".to_string());
    }
    if value_raw.is_empty() {
        return Err("missing value; for example -c theme=onedark.json".to_string());
    }
    let segments = key_segments(key)?;
    Ok(nest(&segments, parse_toml_value_or_string(value_raw)))
}

fn expected_pair() -> String {
    "expected key=value, for example -c theme=onedark.json".to_string()
}

fn key_segments(input: &str) -> Result<Vec<String>, String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut quote: Option<char> = None;
    let invalid = || format!("invalid key '{input}'");

    while let Some(character) = chars.next() {
        if let Some(active) = quote {
            if character == '\\' && active == '"' {
                match chars.next() {
                    Some(escaped) => current.push(escaped),
                    None => return Err(invalid()),
                }
                continue;
            }
            if character == active {
                quote = None;
                continue;
            }
            current.push(character);
            continue;
        }

        match character {
            '"' | '\'' => {
                if !current.trim().is_empty() {
                    return Err(invalid());
                }
                current.clear();
                quote = Some(character);
            }
            '.' => {
                push_segment(&mut segments, &mut current, input)?;
            }
            other => current.push(other),
        }
    }
    if quote.is_some() {
        return Err(invalid());
    }
    push_segment(&mut segments, &mut current, input)?;
    if segments.is_empty() {
        return Err(invalid());
    }
    Ok(segments)
}

fn push_segment(
    segments: &mut Vec<String>,
    current: &mut String,
    input: &str,
) -> Result<(), String> {
    let segment = std::mem::take(current);
    let trimmed = segment.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_whitespace) {
        return Err(format!("invalid key '{input}'"));
    }
    segments.push(trimmed.to_string());
    Ok(())
}

fn nest(segments: &[String], value: toml::Value) -> toml::Value {
    let mut current = value;
    for segment in segments.iter().rev() {
        let mut table = toml::map::Map::new();
        table.insert(segment.clone(), current);
        current = toml::Value::Table(table);
    }
    current
}

fn parse_toml_value_or_string(raw: &str) -> toml::Value {
    let wrapped = format!("_x_={raw}\n");
    if let Ok(value) = toml::from_str::<toml::Value>(&wrapped) {
        if let Some(parsed) = value.get("_x_").cloned() {
            return parsed;
        }
    }
    toml::Value::String(
        raw.trim_matches(|character| character == '"' || character == '\'')
            .to_string(),
    )
}

fn suggestion_keys() -> &'static [String] {
    static KEYS: OnceLock<Vec<String>> = OnceLock::new();
    KEYS.get_or_init(|| {
        let mut seen = BTreeSet::new();
        let mut keys = Vec::new();
        let mut push = |segments: Vec<String>| {
            if segments.is_empty() || !known_schema_path(&segments) {
                return;
            }
            let rendered = render_path(&segments);
            if seen.insert(rendered.clone()) {
                keys.push(rendered);
            }
        };

        if let Ok(defaults) = embedded_config_value() {
            if let Ok(config) = super::deserialize_config(defaults) {
                if let Ok(value) = toml::Value::try_from(config) {
                    collect_suggestion_paths(&value, &[], &mut push);
                }
            }
        }
        for path in EXTRA_SCHEMA_PATHS {
            push(path.iter().map(|segment| (*segment).to_string()).collect());
        }
        keys
    })
}

fn collect_suggestion_paths(
    value: &toml::Value,
    path: &[String],
    push: &mut impl FnMut(Vec<String>),
) {
    if !path.is_empty() {
        push(path.to_vec());
    }
    let Some(table) = value.as_table() else {
        return;
    };
    for (key, child) in table {
        let mut child_path = path.to_vec();
        child_path.push(key.clone());
        collect_suggestion_paths(child, &child_path, push);
    }
}

fn collect_leaves(value: &toml::Value, path: &[String], leaves: &mut Vec<Vec<String>>) {
    let Some(table) = value.as_table() else {
        if !path.is_empty() {
            leaves.push(path.to_vec());
        }
        return;
    };
    if table.is_empty() {
        if !path.is_empty() {
            leaves.push(path.to_vec());
        }
        return;
    }
    for (key, child) in table {
        let mut child_path = path.to_vec();
        child_path.push(key.clone());
        collect_leaves(child, &child_path, leaves);
    }
}

fn levenshtein(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    if left.is_empty() {
        return right.len();
    }
    if right.is_empty() {
        return left.len();
    }
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (row, left_char) in left.iter().enumerate() {
        current[0] = row + 1;
        for (column, right_char) in right.iter().enumerate() {
            let cost = usize::from(left_char != right_char);
            current[column + 1] = (previous[column + 1] + 1)
                .min(current[column] + 1)
                .min(previous[column] + cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::path::Path;

    fn value_at<'a>(value: &'a toml::Value, path: &[&str]) -> &'a toml::Value {
        path.iter()
            .fold(value, |current, segment| current.get(*segment).unwrap())
    }

    #[test]
    fn parses_toml_values_string_fallback_and_nested_paths() {
        let list = parse_config_override("list=[1,2]").unwrap();
        assert_eq!(
            value_at(&list, &["list"]).as_array().unwrap(),
            &vec![toml::Value::Integer(1), toml::Value::Integer(2)]
        );

        let theme = parse_config_override("theme=onedark").unwrap();
        assert_eq!(value_at(&theme, &["theme"]).as_str(), Some("onedark"));

        let quoted = parse_config_override("theme=\"onedark.json\"").unwrap();
        assert_eq!(value_at(&quoted, &["theme"]).as_str(), Some("onedark.json"));

        let dotted_string = parse_config_override("theme=onedark.json").unwrap();
        assert_eq!(
            value_at(&dotted_string, &["theme"]).as_str(),
            Some("onedark.json")
        );

        let boolean = parse_config_override("wrap=false").unwrap();
        assert_eq!(value_at(&boolean, &["wrap"]).as_bool(), Some(false));

        let nested = parse_config_override("languages.rust.indent_width=2").unwrap();
        assert_eq!(
            value_at(&nested, &["languages", "rust", "indent_width"]).as_integer(),
            Some(2)
        );

        let inline = parse_config_override("search={ignorecase=true,smartcase=false}").unwrap();
        assert_eq!(
            value_at(&inline, &["search", "ignorecase"]).as_bool(),
            Some(true)
        );
        assert_eq!(
            value_at(&inline, &["search", "smartcase"]).as_bool(),
            Some(false)
        );

        let quoted_key = parse_config_override(
            "keys.normal.\"Ctrl-t\"={ PluginCommand = \"LspDocumentSymbols\" }",
        )
        .unwrap();
        assert_eq!(
            value_at(&quoted_key, &["keys", "normal", "Ctrl-t", "PluginCommand"]).as_str(),
            Some("LspDocumentSymbols")
        );

        let document = parse_config_override("theme = \"red.json\"\nwrap = true\n").unwrap();
        assert_eq!(value_at(&document, &["theme"]).as_str(), Some("red.json"));
        assert_eq!(value_at(&document, &["wrap"]).as_bool(), Some(true));
    }

    #[test]
    fn rejects_malformed_key_value_overrides() {
        assert!(parse_config_override("nope")
            .unwrap_err()
            .contains("key=value"));
        assert!(parse_config_override("=true")
            .unwrap_err()
            .contains("missing key"));
        assert!(parse_config_override("theme=")
            .unwrap_err()
            .contains("missing value"));
        assert!(parse_config_override("a..b=1")
            .unwrap_err()
            .contains("invalid key"));
        assert!(parse_config_override("foo bar=1")
            .unwrap_err()
            .contains("invalid key"));
    }

    #[test]
    fn repeated_key_value_overrides_merge_in_order_and_last_wins() {
        let loaded = Config::load_user_toml(
            "theme = \"red.json\"\nscrolloff = 9\n",
            Path::new("/tmp/config.toml"),
            &[
                "theme=onedark.json".to_string(),
                "scrolloff=8".to_string(),
                "scrolloff=4".to_string(),
                "languages.rust.indent_width=2".to_string(),
                "search={ignorecase=true}".to_string(),
                "disabled_plugins=[\"fidget\"]".to_string(),
            ],
        )
        .unwrap();

        assert_eq!(loaded.config.theme, "onedark.json");
        assert_eq!(loaded.config.scrolloff, Some(4));
        assert_eq!(
            loaded
                .config
                .languages
                .get("rust")
                .and_then(|language| language.indent_width),
            Some(2)
        );
        assert!(loaded.config.search.ignorecase);
        assert_eq!(loaded.config.disabled_plugins, vec!["fidget".to_string()]);
        assert!(!loaded.config.plugins.contains_key("fidget"));
        let rendered = loaded.render_effective_toml();
        assert!(rendered.contains("precedence: embedded defaults < config.toml < -c/--config"));
        assert!(rendered.contains("-c theme=onedark.json"));
        assert!(rendered.contains("theme = \"onedark.json\""));
    }

    #[test]
    fn unknown_keys_type_mismatches_and_malformed_flags_name_the_override() {
        let unknown = Config::load_user_toml(
            "",
            Path::new("/tmp/config.toml"),
            &["themee=onedark".to_string()],
        )
        .unwrap_err()
        .to_string();
        assert!(unknown.contains("invalid config override #1"));
        assert!(unknown.contains("-c/--config 'themee=onedark'"));
        assert!(unknown.contains("unknown key 'themee'"));
        assert!(unknown.contains("did you mean 'theme'"));

        let nested = Config::load_user_toml(
            "",
            Path::new("/tmp/config.toml"),
            &["lsp.enabld=false".to_string()],
        )
        .unwrap_err()
        .to_string();
        assert!(nested.contains("unknown key 'lsp.enabld'"));
        assert!(nested.contains("did you mean 'lsp.enabled'"));

        let distant = Config::load_user_toml(
            "",
            Path::new("/tmp/config.toml"),
            &["editor.tab_width=2".to_string()],
        )
        .unwrap_err()
        .to_string();
        assert!(
            distant.contains("-c/--config 'editor.tab_width=2'"),
            "{distant}"
        );
        assert!(
            distant.contains("unknown key 'editor.tab_width'"),
            "{distant}"
        );
        assert!(!distant.contains("did you mean"), "{distant}");

        let mismatch = Config::load_user_toml(
            "",
            Path::new("/tmp/config.toml"),
            &["scrolloff=true".to_string()],
        )
        .unwrap_err()
        .to_string();
        assert!(mismatch.contains("invalid config override #1"));
        assert!(mismatch.contains("-c/--config 'scrolloff=true'"));
        assert!(mismatch.contains("scrolloff"));
        assert!(mismatch.contains("expected"));

        let malformed = Config::load_user_toml(
            "",
            Path::new("/tmp/config.toml"),
            &["theme = \"red.json\"".to_string(), "nope".to_string()],
        )
        .unwrap_err()
        .to_string();
        assert!(malformed.contains("invalid config override #2"));
        assert!(malformed.contains("-c/--config 'nope'"));
        assert!(malformed.contains("key=value"));
    }

    #[test]
    fn string_fallback_does_not_override_real_toml_types() {
        let loaded = Config::load_user_toml(
            "",
            Path::new("/tmp/config.toml"),
            &[
                "relative_line_numbers=true".to_string(),
                "mouse_scroll_lines=5".to_string(),
            ],
        )
        .unwrap();
        assert_eq!(loaded.config.relative_line_numbers, Some(true));
        assert_eq!(loaded.config.mouse_scroll_lines, Some(5));
    }
}
