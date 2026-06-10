use std::{
    collections::HashSet,
    path::{Component, Path},
};

use include_dir::{include_dir, Dir};

pub const DEFAULT_CONFIG: &str = include_str!("../default_config.toml");

static THEMES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/themes");
static PLUGINS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/plugins");

const BUNDLED_PLUGIN_SCHEME: &str = "red-bundled";

pub fn starter_config() -> String {
    let mut out = String::new();
    out.push_str("# Red user config.\n");
    out.push_str("# Defaults are built into this version of red.\n");
    out.push_str("# Uncomment settings below to override them.\n\n");

    for line in DEFAULT_CONFIG.lines() {
        if line.trim().is_empty() {
            out.push('\n');
        } else if line.trim_start().starts_with('#') {
            out.push_str(line);
            out.push('\n');
        } else {
            out.push_str("# ");
            out.push_str(line);
            out.push('\n');
        }
    }

    out
}

pub fn bundled_theme(name: &str) -> Option<&'static str> {
    safe_relative_path(name)
        .and_then(|path| THEMES.get_file(path))
        .and_then(|file| file.contents_utf8())
}

pub fn bundled_theme_files() -> Vec<(&'static str, &'static str)> {
    THEMES
        .files()
        .filter_map(|file| {
            let path = file.path();
            if path.parent().is_some_and(|parent| parent != Path::new(""))
                || path.extension().and_then(|ext| ext.to_str()) != Some("json")
            {
                return None;
            }

            let name = path.file_name()?.to_str()?;
            let contents = file.contents_utf8()?;
            Some((name, contents))
        })
        .collect()
}

pub fn bundled_plugin_specifier(name: &str) -> Option<String> {
    let path = safe_relative_path(name)?;
    PLUGINS
        .get_file(path)
        .map(|_| format!("{BUNDLED_PLUGIN_SCHEME}:///plugins/{name}"))
}

pub fn bundled_plugin_contents(specifier: &str) -> Option<&'static str> {
    let path = specifier.strip_prefix(&format!("{BUNDLED_PLUGIN_SCHEME}:///plugins/"))?;
    safe_relative_path(path)
        .and_then(|path| PLUGINS.get_file(path))
        .and_then(|file| file.contents_utf8())
}

pub fn is_bundled_plugin_specifier(specifier: &str) -> bool {
    specifier.starts_with(&format!("{BUNDLED_PLUGIN_SCHEME}:///plugins/"))
}

pub fn dedupe_theme_entries(
    user_entries: impl IntoIterator<Item = (String, String)>,
) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();

    for (name, file) in user_entries {
        seen.insert(file.clone());
        entries.push((name, file));
    }

    for (file, contents) in bundled_theme_files() {
        if seen.contains(file) {
            continue;
        }
        let name = theme_name_from_json(contents).unwrap_or_else(|| file.to_string());
        entries.push((name, file.to_string()));
    }

    entries
}

fn theme_name_from_json(contents: &str) -> Option<String> {
    let metadata: serde_json::Value =
        serde_json::from_reader(json_comments::StripComments::new(contents.as_bytes())).ok()?;
    metadata
        .get("name")
        .and_then(|name| name.as_str())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn safe_relative_path(path: &str) -> Option<&str> {
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        return None;
    }

    let path_ref = Path::new(path);
    if path_ref.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return None;
    }

    Some(path)
}

#[cfg(test)]
mod tests {
    use crate::config::Config;

    use super::*;

    #[test]
    fn starter_config_is_loadable_as_user_overrides() {
        let config = Config::from_user_toml_with_overrides(&starter_config(), &[]).unwrap();

        assert_eq!(config.theme, "mocha.json");
        assert!(config.plugins.contains_key("theme_browser"));
        assert!(config.keys.normal.contains_key("Ctrl-t"));
    }

    #[test]
    fn bundled_assets_include_default_theme_and_plugins() {
        assert!(bundled_theme("mocha.json").is_some());
        assert!(bundled_plugin_specifier("theme_browser.js")
            .as_deref()
            .is_some_and(|specifier| specifier == "red-bundled:///plugins/theme_browser.js"));
        assert!(bundled_plugin_contents("red-bundled:///plugins/theme_browser.js").is_some());
    }

    #[test]
    fn bundled_asset_lookup_rejects_parent_paths() {
        assert!(bundled_theme("../default_config.toml").is_none());
        assert!(bundled_plugin_specifier("../plugins/theme_browser.js").is_none());
    }
}
