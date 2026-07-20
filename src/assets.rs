//! Resolution and extraction of Red's embedded, development, and user runtime assets.
//!
//! Plugins and themes resolve in the order user configuration, `RED_RUNTIME`, then
//! embedded assets. Listing reports shadowed sources, while ejection copies a resolved
//! non-user asset into the user directory for customization. All public lookup paths are
//! relative asset specifiers; accepting parent traversal would let a runtime operation
//! escape the configured asset roots.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    env,
    fmt::{self, Display},
    fs,
    path::{Component, Path, PathBuf},
};

use include_dir::{include_dir, Dir};

/// Complete configuration defaults embedded in the Red binary.
pub const DEFAULT_CONFIG: &str = include_str!("../default_config.toml");

static THEMES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/themes");
static PLUGINS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/plugins");

const BUNDLED_PLUGIN_SCHEME: &str = "red-bundled";
const STARTER_CONFIG_HEADER: &str = "# Red user config.\n# Defaults are built into this version of red.\n# Uncomment settings below to override them.\n# Uncomment the matching table header, such as [keys.normal], with any setting inside it.\n\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
/// Category of runtime file resolved by the asset system.
pub enum RuntimeAssetKind {
    /// Husk plugin source.
    Plugin,
    /// JSON theme definition.
    Theme,
}

impl RuntimeAssetKind {
    /// Returns the directory component used for this asset category.
    pub fn dir_name(self) -> &'static str {
        match self {
            Self::Plugin => "plugins",
            Self::Theme => "themes",
        }
    }

    fn embedded_dir(self) -> &'static Dir<'static> {
        match self {
            Self::Plugin => &PLUGINS,
            Self::Theme => &THEMES,
        }
    }

    fn accepts_file(self, path: &Path) -> bool {
        path.is_file()
            && path
                .file_name()
                .is_some_and(|file| self.accepts_file_name(Path::new(file)))
    }

    fn accepts_file_name(self, path: &Path) -> bool {
        if path.parent().is_some_and(|parent| parent != Path::new("")) {
            return false;
        }

        match self {
            Self::Plugin => matches!(path.extension().and_then(|ext| ext.to_str()), Some("hk")),
            Self::Theme => path.extension().and_then(|ext| ext.to_str()) == Some("json"),
        }
    }

    fn accepts_public_embedded_file_name(self, path: &Path) -> bool {
        if !self.accepts_file_name(path) {
            return false;
        }

        let Some(file_name) = path.file_name().and_then(|file| file.to_str()) else {
            return false;
        };

        match self {
            Self::Plugin => {
                !matches!(file_name, "test.hk" | "unicode_demo.hk")
                    && !file_name.ends_with(".test.hk")
            }
            Self::Theme => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
/// Layer that supplied a resolved runtime asset.
pub enum RuntimeAssetSource {
    /// User configuration directory; highest precedence.
    User,
    /// Development directory selected by `RED_RUNTIME`.
    Runtime,
    /// File compiled into the Red binary; lowest precedence.
    Embedded,
}

impl RuntimeAssetSource {
    fn precedence(self) -> usize {
        match self {
            Self::User => 0,
            Self::Runtime => 1,
            Self::Embedded => 2,
        }
    }
}

impl Display for RuntimeAssetSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::User => "user",
            Self::Runtime => "runtime",
            Self::Embedded => "embedded",
        })
    }
}

#[derive(Debug, Clone)]
/// One runtime asset selected according to Red's precedence rules.
pub struct ResolvedRuntimeAsset {
    /// Asset category.
    pub kind: RuntimeAssetKind,
    /// Safe relative file name.
    pub file: String,
    /// Winning source layer.
    pub source: RuntimeAssetSource,
    path: Option<PathBuf>,
    embedded_contents: Option<&'static str>,
}

impl ResolvedRuntimeAsset {
    /// Returns the filesystem path, or `None` for an embedded asset.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Reads filesystem content or clones the embedded source.
    pub fn read_to_string(&self) -> anyhow::Result<String> {
        if let Some(contents) = self.embedded_contents {
            return Ok(contents.to_string());
        }

        let path = self
            .path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("runtime asset has no readable source"))?;
        Ok(fs::read_to_string(path)?)
    }

    /// Returns a filesystem path or private bundled URI suitable for the Husk loader.
    ///
    /// Returns an error when called for a theme.
    pub fn plugin_specifier(&self) -> anyhow::Result<String> {
        anyhow::ensure!(
            self.kind == RuntimeAssetKind::Plugin,
            "runtime asset is not a plugin: {}",
            self.file
        );

        if let Some(path) = &self.path {
            return Ok(path.to_string_lossy().into_owned());
        }

        bundled_plugin_specifier(&self.file)
            .ok_or_else(|| anyhow::anyhow!("bundled plugin `{}` was not found", self.file))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One asset-list row including shadowed lower-precedence sources.
pub struct RuntimeAssetListEntry {
    /// Asset category.
    pub kind: RuntimeAssetKind,
    /// Safe relative file name.
    pub file: String,
    /// Theme display name parsed from JSON, when available.
    pub name: Option<String>,
    /// Winning source layer.
    pub source: RuntimeAssetSource,
    /// Lower-precedence sources hidden by `source`.
    pub shadows: Vec<RuntimeAssetSource>,
}

/// Produces a commented copy of embedded defaults for a new user config.
pub fn starter_config() -> String {
    let mut out = String::from(STARTER_CONFIG_HEADER);

    for line in DEFAULT_CONFIG.lines() {
        if line.trim().is_empty() {
            out.push('\n');
        } else {
            out.push_str("# ");
            out.push_str(line);
            out.push('\n');
        }
    }

    out
}

/// Resolves a theme by display file name and precedence.
pub fn resolve_theme(name: &str, config_dir: &Path) -> Option<ResolvedRuntimeAsset> {
    resolve_runtime_asset(RuntimeAssetKind::Theme, name, config_dir)
}

/// Resolves a plugin by display file name and precedence.
pub fn resolve_plugin(name: &str, config_dir: &Path) -> Option<ResolvedRuntimeAsset> {
    resolve_runtime_asset(RuntimeAssetKind::Plugin, name, config_dir)
}

/// Resolves an asset from user, development runtime, then embedded layers.
///
/// Unsafe or category-incompatible relative paths return `None`.
pub fn resolve_runtime_asset(
    kind: RuntimeAssetKind,
    name: &str,
    config_dir: &Path,
) -> Option<ResolvedRuntimeAsset> {
    resolve_runtime_asset_with_user(kind, name, config_dir, true)
}

/// Resolves an asset while excluding the user layer.
///
/// This is the source selection used by ejection so an existing customized
/// file is never copied onto itself.
pub fn resolve_non_user_runtime_asset(
    kind: RuntimeAssetKind,
    name: &str,
    config_dir: &Path,
) -> Option<ResolvedRuntimeAsset> {
    resolve_runtime_asset_with_user(kind, name, config_dir, false)
}

/// Returns an embedded theme's JSON source.
pub fn bundled_theme(name: &str) -> Option<&'static str> {
    embedded_contents(RuntimeAssetKind::Theme, name)
}

/// Lists all public embedded theme files and their JSON source.
pub fn bundled_theme_files() -> Vec<(&'static str, &'static str)> {
    embedded_files(RuntimeAssetKind::Theme)
        .into_iter()
        .filter_map(|file| {
            embedded_contents(RuntimeAssetKind::Theme, file).map(|contents| (file, contents))
        })
        .collect()
}

/// Converts an embedded plugin file name into the private loader URI.
pub fn bundled_plugin_specifier(name: &str) -> Option<String> {
    let path = safe_relative_path(name)?;
    PLUGINS
        .get_file(path)
        .map(|_| format!("{BUNDLED_PLUGIN_SCHEME}:///plugins/{path}"))
}

/// Resolves a private bundled plugin URI back to embedded source.
pub fn bundled_plugin_contents(specifier: &str) -> Option<&'static str> {
    let path = specifier.strip_prefix(&format!("{BUNDLED_PLUGIN_SCHEME}:///plugins/"))?;
    embedded_contents(RuntimeAssetKind::Plugin, path)
}

/// Returns whether a specifier uses Red's private embedded-plugin scheme.
pub fn is_bundled_plugin_specifier(specifier: &str) -> bool {
    specifier.starts_with(&format!("{BUNDLED_PLUGIN_SCHEME}:///plugins/"))
}

/// Lists effective assets and all lower-precedence shadowed sources.
pub fn list_runtime_assets(
    kind: RuntimeAssetKind,
    config_dir: &Path,
) -> anyhow::Result<Vec<RuntimeAssetListEntry>> {
    let mut by_file: BTreeMap<String, BTreeSet<RuntimeAssetSource>> = BTreeMap::new();

    for file in list_files_in_dir(&config_dir.join(kind.dir_name()), kind)? {
        by_file
            .entry(file)
            .or_default()
            .insert(RuntimeAssetSource::User);
    }

    if let Some(runtime_dir) = runtime_dir() {
        for file in list_files_in_dir(&runtime_dir.join(kind.dir_name()), kind)? {
            by_file
                .entry(file)
                .or_default()
                .insert(RuntimeAssetSource::Runtime);
        }
    }

    for file in embedded_files(kind) {
        by_file
            .entry(file.to_string())
            .or_default()
            .insert(RuntimeAssetSource::Embedded);
    }

    let mut entries = Vec::new();
    for (file, sources) in by_file {
        let source = sources
            .iter()
            .copied()
            .min_by_key(|source| source.precedence())
            .expect("source set should not be empty");
        let shadows = sources
            .iter()
            .copied()
            .filter(|candidate| candidate.precedence() > source.precedence())
            .collect();
        let name = if kind == RuntimeAssetKind::Theme {
            resolve_runtime_asset(kind, &file, config_dir)
                .and_then(|asset| asset.read_to_string().ok())
                .and_then(|contents| theme_name_from_json(&contents))
        } else {
            None
        };
        entries.push(RuntimeAssetListEntry {
            kind,
            file,
            name,
            source,
            shadows,
        });
    }

    Ok(entries)
}

/// Formats the runtime asset inventory used by `red --runtime-files`.
pub fn format_runtime_files(config_dir: &Path) -> anyhow::Result<String> {
    let mut out = String::new();

    for (heading, kind) in [
        ("Runtime plugins", RuntimeAssetKind::Plugin),
        ("Runtime themes", RuntimeAssetKind::Theme),
    ] {
        out.push_str(heading);
        out.push_str(":\n");

        let entries = list_runtime_assets(kind, config_dir)?;
        if entries.is_empty() {
            out.push_str("  (none)\n");
        }

        for entry in entries {
            out.push_str("  ");
            out.push_str(&entry.file);
            out.push_str("  ");
            out.push_str(&entry.source.to_string());
            if !entry.shadows.is_empty() {
                out.push_str(" (shadows ");
                out.push_str(
                    &entry
                        .shadows
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                out.push(')');
            }
            out.push('\n');
        }
        out.push('\n');
    }

    out.push_str("Resolution order: user config, $RED_RUNTIME, embedded.\n");
    out.push_str(
        "Use `red --eject plugins/<file>` or `red --eject themes/<file>` to copy an asset into your config directory.\n",
    );
    Ok(out)
}

/// Copies a development or embedded asset into the user configuration layer.
///
/// The asset path must name `plugins/<file>` or `themes/<file>`. Existing
/// targets are preserved unless `force` is true.
pub fn eject_runtime_asset(asset: &str, config_dir: &Path, force: bool) -> anyhow::Result<PathBuf> {
    let (kind, file) = parse_asset_path(asset)?;
    let target = config_dir.join(kind.dir_name()).join(&file);
    if target.exists() && !force {
        anyhow::bail!(
            "{} already exists; use --eject-force to overwrite it",
            target.display()
        );
    }

    let source = resolve_non_user_runtime_asset(kind, &file, config_dir).ok_or_else(|| {
        anyhow::anyhow!(
            "runtime asset `{}/{}` was not found in $RED_RUNTIME or embedded assets",
            kind.dir_name(),
            file
        )
    })?;
    let contents = source.read_to_string()?;

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&target, contents)?;
    Ok(target)
}

/// Appends unshadowed bundled themes after user-provided theme entries.
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

fn resolve_runtime_asset_with_user(
    kind: RuntimeAssetKind,
    name: &str,
    config_dir: &Path,
    include_user: bool,
) -> Option<ResolvedRuntimeAsset> {
    let name = safe_relative_path(name)?.to_string();

    if include_user {
        let user_path = config_dir.join(kind.dir_name()).join(&name);
        if user_path.is_file() {
            return Some(ResolvedRuntimeAsset {
                kind,
                file: name,
                source: RuntimeAssetSource::User,
                path: Some(user_path),
                embedded_contents: None,
            });
        }
    }

    if let Some(runtime_dir) = runtime_dir() {
        let runtime_path = runtime_dir.join(kind.dir_name()).join(&name);
        if runtime_path.is_file() {
            return Some(ResolvedRuntimeAsset {
                kind,
                file: name,
                source: RuntimeAssetSource::Runtime,
                path: Some(runtime_path),
                embedded_contents: None,
            });
        }
    }

    embedded_contents(kind, &name).map(|contents| ResolvedRuntimeAsset {
        kind,
        file: name,
        source: RuntimeAssetSource::Embedded,
        path: None,
        embedded_contents: Some(contents),
    })
}

fn parse_asset_path(asset: &str) -> anyhow::Result<(RuntimeAssetKind, String)> {
    if let Some((dir, file)) = asset.split_once('/') {
        anyhow::ensure!(
            safe_relative_path(file).is_some(),
            "invalid runtime asset path: {asset}"
        );
        anyhow::ensure!(
            !file.contains('/'),
            "runtime asset must be directly inside `plugins/` or `themes/`: {asset}"
        );

        let kind = match dir {
            "plugins" => RuntimeAssetKind::Plugin,
            "themes" => RuntimeAssetKind::Theme,
            _ => anyhow::bail!("asset must start with `plugins/` or `themes/`: {asset}"),
        };

        let path = Path::new(file);
        anyhow::ensure!(
            kind.accepts_file_name(path),
            "unsupported runtime asset file: {asset}"
        );
        return Ok((kind, file.to_string()));
    }

    anyhow::ensure!(
        safe_relative_path(asset).is_some(),
        "invalid runtime asset path: {asset}"
    );

    let path = Path::new(asset);
    if RuntimeAssetKind::Plugin.accepts_file_name(path) {
        return Ok((RuntimeAssetKind::Plugin, asset.to_string()));
    }
    if RuntimeAssetKind::Theme.accepts_file_name(path) {
        return Ok((RuntimeAssetKind::Theme, asset.to_string()));
    }

    anyhow::bail!(
        "asset must look like `plugins/name.hk`, `themes/name.json`, or a bare plugin/theme file name"
    )
}

fn runtime_dir() -> Option<PathBuf> {
    env::var_os("RED_RUNTIME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn list_files_in_dir(dir: &Path, kind: RuntimeAssetKind) -> anyhow::Result<Vec<String>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !kind.accepts_file(&path) {
            continue;
        }
        if let Some(file) = path.file_name().and_then(|file| file.to_str()) {
            files.push(file.to_string());
        }
    }
    files.sort();
    Ok(files)
}

fn embedded_files(kind: RuntimeAssetKind) -> Vec<&'static str> {
    let mut files = kind
        .embedded_dir()
        .files()
        .filter_map(|file| {
            let path = file.path();
            if path.parent().is_some_and(|parent| parent != Path::new(""))
                || !kind.accepts_public_embedded_file_name(path)
            {
                return None;
            }
            path.file_name()?.to_str()
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn embedded_contents(kind: RuntimeAssetKind, name: &str) -> Option<&'static str> {
    safe_relative_path(name)
        .and_then(|path| kind.embedded_dir().get_file(path))
        .and_then(|file| file.contents_utf8())
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
    use std::sync::{Mutex, MutexGuard};

    use crate::config::Config;

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn unique_temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("red-assets-{name}-{}", uuid::Uuid::new_v4()))
    }

    struct RedRuntimeGuard<'a> {
        _guard: MutexGuard<'a, ()>,
        previous: Option<std::ffi::OsString>,
    }

    impl RedRuntimeGuard<'_> {
        fn set(path: &Path) -> Self {
            let guard = ENV_LOCK.lock().unwrap();
            let previous = env::var_os("RED_RUNTIME");
            env::set_var("RED_RUNTIME", path);
            Self {
                _guard: guard,
                previous,
            }
        }

        fn unset() -> Self {
            let guard = ENV_LOCK.lock().unwrap();
            let previous = env::var_os("RED_RUNTIME");
            env::remove_var("RED_RUNTIME");
            Self {
                _guard: guard,
                previous,
            }
        }
    }

    impl Drop for RedRuntimeGuard<'_> {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                env::set_var("RED_RUNTIME", previous);
            } else {
                env::remove_var("RED_RUNTIME");
            }
        }
    }

    #[test]
    fn starter_config_is_loadable_as_user_overrides() {
        let config = Config::from_user_toml_with_overrides(&starter_config(), &[]).unwrap();

        assert_eq!(config.theme, "red.json");
        assert!(config.plugins.contains_key("theme_browser"));
        assert!(config.keys.normal.contains_key("Ctrl-t"));
    }

    #[test]
    fn starter_config_uncomments_to_default_config() {
        let starter = starter_config();
        let body = starter
            .strip_prefix(STARTER_CONFIG_HEADER)
            .expect("starter should begin with its header");
        let mut uncommented = String::new();
        for line in body.split_inclusive('\n') {
            if line == "\n" {
                uncommented.push('\n');
            } else {
                uncommented.push_str(
                    line.strip_prefix("# ")
                        .expect("non-blank starter lines should be commented"),
                );
            }
        }

        assert_eq!(
            uncommented.replace("\r\n", "\n"),
            DEFAULT_CONFIG.replace("\r\n", "\n")
        );
    }

    #[test]
    fn bundled_assets_include_default_theme_and_plugins() {
        assert!(bundled_theme("red.json").is_some());
        assert!(bundled_plugin_specifier("theme_browser.hk")
            .as_deref()
            .is_some_and(|specifier| specifier == "red-bundled:///plugins/theme_browser.hk"));
        assert!(bundled_plugin_contents("red-bundled:///plugins/theme_browser.hk").is_some());
    }

    #[test]
    fn bundled_asset_lookup_rejects_parent_paths() {
        assert!(bundled_theme("../default_config.toml").is_none());
        assert!(bundled_plugin_specifier("../plugins/theme_browser.hk").is_none());
    }

    #[test]
    fn resolver_prefers_user_then_runtime_then_embedded() {
        let config_dir = unique_temp_dir("resolver-config");
        let runtime_dir = unique_temp_dir("resolver-runtime");
        fs::create_dir_all(config_dir.join("themes")).unwrap();
        fs::create_dir_all(runtime_dir.join("themes")).unwrap();
        fs::write(
            runtime_dir.join("themes/mocha.json"),
            r#"{ "runtime": true }"#,
        )
        .unwrap();
        let _runtime = RedRuntimeGuard::set(&runtime_dir);

        let runtime_asset = resolve_theme("mocha.json", &config_dir).unwrap();
        assert_eq!(runtime_asset.source, RuntimeAssetSource::Runtime);

        fs::write(config_dir.join("themes/mocha.json"), r#"{ "user": true }"#).unwrap();
        let user_asset = resolve_theme("mocha.json", &config_dir).unwrap();
        assert_eq!(user_asset.source, RuntimeAssetSource::User);

        fs::remove_dir_all(config_dir).ok();
        fs::remove_dir_all(runtime_dir).ok();
    }

    #[test]
    fn resolver_uses_embedded_when_user_and_runtime_are_missing() {
        let _runtime = RedRuntimeGuard::unset();
        let config_dir = unique_temp_dir("resolver-embedded");

        let asset = resolve_theme("mocha.json", &config_dir).unwrap();

        assert_eq!(asset.source, RuntimeAssetSource::Embedded);
        fs::remove_dir_all(config_dir).ok();
    }

    #[test]
    fn runtime_files_marks_shadowed_sources() {
        let config_dir = unique_temp_dir("runtime-files-config");
        let runtime_dir = unique_temp_dir("runtime-files-runtime");
        fs::create_dir_all(config_dir.join("plugins")).unwrap();
        fs::create_dir_all(runtime_dir.join("plugins")).unwrap();
        fs::write(
            config_dir.join("plugins/theme_browser.hk"),
            "pub fn activate() {}",
        )
        .unwrap();
        fs::write(
            runtime_dir.join("plugins/theme_browser.hk"),
            "pub fn activate() {}",
        )
        .unwrap();
        let _runtime = RedRuntimeGuard::set(&runtime_dir);

        let files = format_runtime_files(&config_dir).unwrap();

        assert!(files.contains("Runtime plugins:"));
        assert!(files.contains("Runtime themes:"));
        assert!(files.contains("Resolution order: user config, $RED_RUNTIME, embedded."));
        let line = files
            .lines()
            .find(|line| line.contains("theme_browser.hk") && line.contains("user"))
            .unwrap_or_else(|| panic!("missing shadowed theme_browser.hk entry: {files}"));
        assert!(
            line.contains("runtime"),
            "expected runtime shadow in {line}"
        );
        assert!(
            line.contains("embedded"),
            "expected embedded shadow in {line}"
        );
        fs::remove_dir_all(config_dir).ok();
        fs::remove_dir_all(runtime_dir).ok();
    }

    #[test]
    fn runtime_files_hides_bundled_dev_plugin_files() {
        let _runtime = RedRuntimeGuard::unset();
        let config_dir = unique_temp_dir("runtime-files-public-list");

        let files = format_runtime_files(&config_dir).unwrap();

        assert!(files.contains("theme_browser.hk"));
        assert!(!files.contains("barbecue.test.hk"));
        assert!(!files.contains("test.hk"));
        assert!(!files.contains("unicode_demo.hk"));
        fs::remove_dir_all(config_dir).ok();
    }

    #[test]
    fn eject_copies_non_user_asset_and_refuses_overwrite() {
        let _runtime = RedRuntimeGuard::unset();
        let config_dir = unique_temp_dir("eject");

        let target = eject_runtime_asset("plugins/theme_browser.hk", &config_dir, false).unwrap();
        assert_eq!(target, config_dir.join("plugins/theme_browser.hk"));
        assert!(target.exists());
        assert!(eject_runtime_asset("plugins/theme_browser.hk", &config_dir, false).is_err());
        eject_runtime_asset("plugins/theme_browser.hk", &config_dir, true).unwrap();

        fs::remove_dir_all(config_dir).ok();
    }

    #[test]
    fn eject_accepts_bare_plugin_and_theme_names() {
        let _runtime = RedRuntimeGuard::unset();
        let config_dir = unique_temp_dir("eject-bare");

        let plugin = eject_runtime_asset("theme_browser.hk", &config_dir, false).unwrap();
        let theme = eject_runtime_asset("mocha.json", &config_dir, false).unwrap();

        assert_eq!(plugin, config_dir.join("plugins/theme_browser.hk"));
        assert_eq!(theme, config_dir.join("themes/mocha.json"));
        assert!(plugin.exists());
        assert!(theme.exists());
        fs::remove_dir_all(config_dir).ok();
    }

    #[test]
    fn eject_uses_runtime_before_embedded() {
        let config_dir = unique_temp_dir("eject-config");
        let runtime_dir = unique_temp_dir("eject-runtime");
        fs::create_dir_all(runtime_dir.join("plugins")).unwrap();
        fs::write(
            runtime_dir.join("plugins/theme_browser.hk"),
            "pub fn activate() { red::log(\"runtime\"); }",
        )
        .unwrap();
        let _runtime = RedRuntimeGuard::set(&runtime_dir);

        let target = eject_runtime_asset("plugins/theme_browser.hk", &config_dir, false).unwrap();

        assert_eq!(
            fs::read_to_string(target).unwrap(),
            "pub fn activate() { red::log(\"runtime\"); }"
        );
        fs::remove_dir_all(config_dir).ok();
        fs::remove_dir_all(runtime_dir).ok();
    }
}
