use std::{collections::HashMap, fs, path::PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};

use crate::assets;
use crate::editor::Action;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Config {
    pub keys: Keys,
    pub theme: String,
    #[serde(default)]
    pub cursor: CursorConfig,
    #[serde(default)]
    pub plugins: HashMap<String, String>,
    #[serde(default)]
    pub disabled_plugins: Vec<String>,
    #[serde(default)]
    pub plugin_permissions: HashMap<String, PluginPermissions>,
    #[serde(default)]
    pub plugin_config: HashMap<String, Value>,
    pub log_file: Option<String>,
    pub mouse_scroll_lines: Option<usize>,
    pub scrolloff: Option<usize>,
    pub wrap: Option<bool>,
    /// Indent wrapped continuation rows to the line's leading whitespace,
    /// like vim's 'breakindent'. Defaults to on.
    pub breakindent: Option<bool>,
    pub sidescroll: Option<usize>,
    pub sidescrolloff: Option<usize>,
    #[serde(default)]
    pub search: SearchConfig,
    #[serde(default)]
    pub picker: PickerConfig,
    #[serde(default)]
    pub key_hints: KeyHintsConfig,
    #[serde(default)]
    pub clipboard: ClipboardConfig,
    #[serde(default)]
    pub lsp: LspConfig,
    #[serde(default)]
    pub matchit: MatchitConfig,
    /// Disable every agent surface, adapter check, and process launch.
    #[serde(default = "default_false")]
    pub disable_ai: bool,
    /// Unsupported development escape hatch set by `--no-typecheck`.
    #[serde(default, skip_serializing)]
    pub disable_plugin_typecheck: bool,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default = "default_true")]
    pub show_diagnostics: bool,
    #[serde(default = "default_false")]
    pub window_borders_ascii: bool,
    #[serde(default, skip_serializing)]
    pub startup_file_count: usize,
}

/// Direct Codex CLI launch configuration.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct AgentConfig {
    /// Codex executable override. Red uses `codex` from PATH when absent.
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct PickerConfig {
    #[serde(default)]
    pub input_position: PickerInputPosition,
}

/// Configuration for the delayed keymap-prefix guide.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct KeyHintsConfig {
    /// Show available key continuations after entering a configured prefix.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Delay before the prefix guide is shown.
    #[serde(default = "default_key_hint_delay_ms")]
    pub delay_ms: u64,
}

impl Default for KeyHintsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            delay_ms: default_key_hint_delay_ms(),
        }
    }
}

fn default_key_hint_delay_ms() -> u64 {
    250
}

impl Default for PickerConfig {
    fn default() -> Self {
        Self {
            input_position: PickerInputPosition::Bottom,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PickerInputPosition {
    Top,
    #[default]
    Bottom,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct ClipboardConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub sync_on_yank: bool,
    #[serde(default = "default_true")]
    pub sync_on_paste: bool,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sync_on_yank: true,
            sync_on_paste: true,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct PluginPermissions {
    /// Executables this plugin may launch through the process API.
    ///
    /// Entries are matched exactly against the requested command. Red does
    /// not invoke a shell when launching plugin processes.
    #[serde(default)]
    pub process: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct MatchitConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_matchit_pairs")]
    pub pairs: Vec<[String; 2]>,
    #[serde(default)]
    pub languages: HashMap<String, MatchitLanguageConfig>,
}

impl Default for MatchitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            pairs: default_matchit_pairs(),
            languages: HashMap::new(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct MatchitLanguageConfig {
    #[serde(default)]
    pub groups: Vec<Vec<String>>,
}

fn default_matchit_pairs() -> Vec<[String; 2]> {
    vec![
        ["(".to_string(), ")".to_string()],
        ["{".to_string(), "}".to_string()],
        ["[".to_string(), "]".to_string()],
    ]
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct SearchConfig {
    #[serde(default = "default_true")]
    pub incsearch: bool,
    #[serde(default = "default_true")]
    pub hlsearch: bool,
    #[serde(default = "default_true")]
    pub wrapscan: bool,
    #[serde(default = "default_false")]
    pub ignorecase: bool,
    #[serde(default = "default_false")]
    pub smartcase: bool,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            incsearch: true,
            hlsearch: true,
            wrapscan: true,
            ignorecase: false,
            smartcase: false,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CursorShape {
    #[default]
    Default,
    BlinkingBlock,
    SteadyBlock,
    BlinkingUnderscore,
    SteadyUnderscore,
    BlinkingBar,
    SteadyBar,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct CursorConfig {
    #[serde(default)]
    pub normal: CursorShape,
    #[serde(default = "cursor_shape_steady_bar")]
    pub insert: CursorShape,
    #[serde(default)]
    pub command: CursorShape,
    #[serde(default)]
    pub search: CursorShape,
    #[serde(default)]
    pub visual: CursorShape,
    #[serde(default)]
    pub visual_line: CursorShape,
    #[serde(default)]
    pub visual_block: CursorShape,
    #[serde(default = "cursor_shape_steady_underscore")]
    pub waiting: CursorShape,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            normal: CursorShape::Default,
            insert: CursorShape::SteadyBar,
            command: CursorShape::Default,
            search: CursorShape::Default,
            visual: CursorShape::Default,
            visual_line: CursorShape::Default,
            visual_block: CursorShape::Default,
            waiting: CursorShape::SteadyUnderscore,
        }
    }
}

fn cursor_shape_steady_bar() -> CursorShape {
    CursorShape::SteadyBar
}

fn cursor_shape_steady_underscore() -> CursorShape {
    CursorShape::SteadyUnderscore
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LspConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub format_on_save: bool,
    #[serde(
        default = "default_language_servers",
        deserialize_with = "deserialize_language_servers"
    )]
    pub servers: HashMap<String, LanguageServerConfig>,
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            format_on_save: false,
            servers: default_language_servers(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct LanguageServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub language_id: String,
    #[serde(default)]
    pub file_extensions: Vec<String>,
    #[serde(default)]
    pub documents: Vec<LanguageDocumentConfig>,
    #[serde(default)]
    pub root_markers: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default, skip_serializing)]
    pub initialization_options: Option<Value>,
    pub workspace_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct LanguageDocumentConfig {
    pub language_id: String,
    #[serde(default)]
    pub file_extensions: Vec<String>,
}

impl LanguageServerConfig {
    pub fn documents(&self) -> Vec<LanguageDocumentConfig> {
        if !self.documents.is_empty() {
            return self.documents.clone();
        }

        if self.language_id.is_empty() || self.file_extensions.is_empty() {
            return Vec::new();
        }

        vec![LanguageDocumentConfig {
            language_id: self.language_id.clone(),
            file_extensions: self.file_extensions.clone(),
        }]
    }
}

pub fn default_language_servers() -> HashMap<String, LanguageServerConfig> {
    HashMap::from([
        (
            "rust".to_string(),
            LanguageServerConfig {
                command: "rust-analyzer".to_string(),
                args: vec!["-v".to_string()],
                language_id: "rust".to_string(),
                file_extensions: vec!["rs".to_string()],
                documents: Vec::new(),
                root_markers: vec!["Cargo.toml".to_string(), ".git".to_string()],
                env: HashMap::new(),
                initialization_options: Some(rust_analyzer_initialization_options()),
                workspace_name: Some("red".to_string()),
            },
        ),
        (
            "typescript".to_string(),
            server(
                "typescript-language-server",
                &["--stdio"],
                &[
                    document("typescript", &["ts"]),
                    document("typescriptreact", &["tsx"]),
                    document("javascript", &["js", "mjs", "cjs"]),
                    document("javascriptreact", &["jsx"]),
                ],
                &["package.json", "tsconfig.json", "jsconfig.json", ".git"],
            ),
        ),
        (
            "python".to_string(),
            server(
                "pyright-langserver",
                &["--stdio"],
                &[document("python", &["py", "pyw"])],
                &["pyproject.toml", "setup.py", "requirements.txt", ".git"],
            ),
        ),
        (
            "markdown".to_string(),
            server(
                "marksman",
                &["server"],
                &[document("markdown", &["md", "markdown"])],
                &[".marksman.toml", ".git"],
            ),
        ),
        (
            "json".to_string(),
            server(
                "vscode-json-language-server",
                &["--stdio"],
                &[document("json", &["json"])],
                &["package.json", ".git"],
            ),
        ),
        (
            "toml".to_string(),
            server(
                "taplo",
                &["lsp", "stdio"],
                &[document("toml", &["toml"])],
                &["taplo.toml", "Cargo.toml", ".git"],
            ),
        ),
        (
            "yaml".to_string(),
            server(
                "yaml-language-server",
                &["--stdio"],
                &[document("yaml", &["yaml", "yml"])],
                &[".git"],
            ),
        ),
        (
            "lua".to_string(),
            server(
                "lua-language-server",
                &[],
                &[document("lua", &["lua"])],
                &[
                    ".luarc.json",
                    ".luarc.jsonc",
                    ".luacheckrc",
                    ".stylua.toml",
                    ".git",
                ],
            ),
        ),
    ])
}

fn server(
    command: &str,
    args: &[&str],
    documents: &[LanguageDocumentConfig],
    root_markers: &[&str],
) -> LanguageServerConfig {
    LanguageServerConfig {
        command: command.to_string(),
        args: args.iter().map(|arg| arg.to_string()).collect(),
        language_id: String::new(),
        file_extensions: Vec::new(),
        documents: documents.to_vec(),
        root_markers: root_markers
            .iter()
            .map(|marker| marker.to_string())
            .collect(),
        env: HashMap::new(),
        initialization_options: None,
        workspace_name: None,
    }
}

fn document(language_id: &str, file_extensions: &[&str]) -> LanguageDocumentConfig {
    LanguageDocumentConfig {
        language_id: language_id.to_string(),
        file_extensions: file_extensions
            .iter()
            .map(|extension| extension.to_string())
            .collect(),
    }
}

fn deserialize_language_servers<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, LanguageServerConfig>, D::Error>
where
    D: Deserializer<'de>,
{
    let user_servers = HashMap::<String, LanguageServerConfig>::deserialize(deserializer)?;
    let mut servers = default_language_servers();
    servers.extend(user_servers);
    Ok(servers)
}

pub fn rust_analyzer_initialization_options() -> Value {
    json!({
      "restartServerOnConfigChange": false,
      "showUnlinkedFileNotification": true,
      "showRequestFailedErrorNotification": true,
      "showDependenciesExplorer": true,
      "testExplorer": false,
      "initializeStopped": false,
      "runnables": {
        "extraEnv": null,
        "problemMatcher": [
          "$rustc"
        ],
        "askBeforeUpdateTest": true,
        "command": null,
        "extraArgs": [],
        "extraTestBinaryArgs": [
          "--show-output"
        ]
      },
      "statusBar": {
        "clickAction": "openLogs",
        "showStatusBar": {
          "documentSelector": [
            {
              "language": "rust"
            },
            {
              "pattern": "**/Cargo.toml"
            },
            {
              "pattern": "**/Cargo.lock"
            }
          ]
        }
      },
      "server": {
        "path": null,
        "extraEnv": null
      },
      "trace": {
        "server": "verbose",
        "extension": false
      },
      "debug": {
        "engine": "auto",
        "sourceFileMap": {
          "/rustc/<id>": "${env:USERPROFILE}/.rustup/toolchains/<toolchain-id>/lib/rustlib/src/rust"
        },
        "openDebugPane": false,
        "buildBeforeRestart": false,
        "engineSettings": {}
      },
      "typing": {
        "continueCommentsOnNewline": true,
        "excludeChars": "|<"
      },
      "diagnostics": {
        "previewRustcOutput": false,
        "useRustcErrorCode": false,
        "disabled": [],
        "enable": true,
        "experimental": {
          "enable": false
        },
        "remapPrefix": {},
      }
    })
}

impl Config {
    pub fn config_dir() -> PathBuf {
        if let Some(config_home) =
            std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty())
        {
            return PathBuf::from(config_home).join("red");
        }

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .or_else(|| {
                #[allow(deprecated)]
                std::env::home_dir()
            })
            .expect("home directory must be available to locate red config");

        home.join(".config").join("red")
    }

    pub fn path(p: &str) -> PathBuf {
        Self::config_dir().join(p)
    }

    pub fn from_toml_with_overrides(contents: &str, overrides: &[String]) -> anyhow::Result<Self> {
        let mut value: toml::Value = toml::from_str(contents)
            .map_err(|err| anyhow::anyhow!("failed to parse config.toml: {err}"))?;

        for (index, override_toml) in overrides.iter().enumerate() {
            let override_value: toml::Value = toml::from_str(override_toml).map_err(|err| {
                anyhow::anyhow!("failed to parse config override #{}: {err}", index + 1)
            })?;
            merge_toml_values(&mut value, override_value);
        }

        let mut config: Self = value
            .try_into()
            .map_err(|err| anyhow::anyhow!("failed to deserialize merged config: {err}"))?;
        config.apply_disabled_plugins();
        Ok(config)
    }

    pub fn from_user_toml_with_overrides(
        contents: &str,
        overrides: &[String],
    ) -> anyhow::Result<Self> {
        let mut value: toml::Value = toml::from_str(assets::DEFAULT_CONFIG)
            .map_err(|err| anyhow::anyhow!("failed to parse bundled default_config.toml: {err}"))?;

        if !contents.trim().is_empty() {
            let user_value: toml::Value = toml::from_str(contents)
                .map_err(|err| anyhow::anyhow!("failed to parse config.toml: {err}"))?;
            merge_toml_values(&mut value, user_value);
        }

        for (index, override_toml) in overrides.iter().enumerate() {
            let override_value: toml::Value = toml::from_str(override_toml).map_err(|err| {
                anyhow::anyhow!("failed to parse config override #{}: {err}", index + 1)
            })?;
            merge_toml_values(&mut value, override_value);
        }

        let mut config: Self = value
            .try_into()
            .map_err(|err| anyhow::anyhow!("failed to deserialize merged config: {err}"))?;
        config.apply_disabled_plugins();
        Ok(config)
    }

    pub fn persist_theme(theme_name: &str) -> anyhow::Result<()> {
        let config_path = Self::path("config.toml");
        let contents = fs::read_to_string(&config_path).unwrap_or_default();
        fs::write(
            config_path,
            update_theme_config_contents(&contents, theme_name)?,
        )?;
        Ok(())
    }

    pub fn resolve_plugin_path(configured_path: &str) -> String {
        let configured = PathBuf::from(configured_path);
        if configured.is_absolute() {
            return configured.to_string_lossy().into_owned();
        }

        if let Some(asset) = assets::resolve_plugin(configured_path, &Self::config_dir()) {
            return asset.plugin_specifier().unwrap_or_else(|_| {
                Self::path("plugins")
                    .join(configured_path)
                    .to_string_lossy()
                    .into_owned()
            });
        }

        Self::path("plugins")
            .join(configured_path)
            .to_string_lossy()
            .into_owned()
    }

    fn apply_disabled_plugins(&mut self) {
        if self.disable_ai {
            self.plugins.remove("agent");
        }
        for plugin in &self.disabled_plugins {
            self.plugins.remove(plugin);
        }
    }
}

fn merge_toml_values(base: &mut toml::Value, override_value: toml::Value) {
    match (base, override_value) {
        (toml::Value::Table(base), toml::Value::Table(override_table)) => {
            for (key, value) in override_table {
                match base.get_mut(&key) {
                    Some(base_value) => merge_toml_values(base_value, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, override_value) => {
            *base = override_value;
        }
    }
}

fn update_theme_config_contents(contents: &str, theme_name: &str) -> anyhow::Result<String> {
    #[derive(Serialize)]
    struct ThemeConfig<'a> {
        theme: &'a str,
    }

    let replacement = toml::to_string(&ThemeConfig { theme: theme_name })?;
    let mut updated = String::with_capacity(contents.len().max(replacement.len()));
    let mut replaced = false;

    let mut in_top_level = true;
    for line in contents.split_inclusive('\n') {
        if !replaced && in_top_level && is_theme_assignment(line) {
            updated.push_str(&replacement);
            replaced = true;
        } else {
            updated.push_str(line);
        }

        if starts_table_header(line) {
            in_top_level = false;
        }
    }

    if !replaced {
        updated = format!("{replacement}{contents}");
    }

    Ok(updated)
}

fn is_theme_assignment(line: &str) -> bool {
    let line = line.trim_start();
    if line.starts_with('#') {
        return false;
    }

    line.strip_prefix("theme")
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

fn starts_table_header(line: &str) -> bool {
    let line = line.trim_start();
    !line.starts_with('#') && line.starts_with('[')
}

pub fn default_true() -> bool {
    true
}

pub fn default_false() -> bool {
    false
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(untagged)]
pub enum KeyAction {
    None,
    Single(Action),
    Multiple(Vec<Action>),
    Nested(HashMap<String, KeyAction>),
    Repeating(u16, Box<KeyAction>),
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Keys {
    #[serde(default)]
    pub normal: HashMap<String, KeyAction>,
    #[serde(default)]
    pub insert: HashMap<String, KeyAction>,
    #[serde(default)]
    pub command: HashMap<String, KeyAction>,
    #[serde(default)]
    pub visual: HashMap<String, KeyAction>,
    #[serde(default)]
    pub visual_line: HashMap<String, KeyAction>,
    #[serde(default)]
    pub visual_block: HashMap<String, KeyAction>,
}

#[cfg(test)]
mod test {
    use crate::editor::{Action, Mode, SearchDirection};

    use super::*;

    #[test]
    fn test_persist_config() {
        let config = Config {
            theme: "theme/nightfox.json".to_string(),
            keys: Keys {
                normal: HashMap::from([
                    (
                        "o".to_string(),
                        KeyAction::Single(Action::InsertLineBelowCursor),
                    ),
                    (
                        "i".to_string(),
                        KeyAction::Single(Action::EnterMode(Mode::Normal)),
                    ),
                ]),
                insert: HashMap::new(),
                command: HashMap::new(),
                visual: HashMap::new(),
                visual_line: HashMap::new(),
                visual_block: HashMap::new(),
            },
            ..Default::default()
        };

        let toml = toml::to_string(&config).unwrap();
        println!("{toml}");
    }

    #[test]
    fn update_theme_config_replaces_existing_theme_line() {
        let contents = r#"# sample
# theme = "old-commented.json"
theme = "mocha.json"

[keys.normal]
"t" = { PluginCommand = "ThemeBrowser" }
"#;

        let updated = update_theme_config_contents(contents, "kanso-zen.json").unwrap();

        assert_eq!(
            updated,
            r#"# sample
# theme = "old-commented.json"
theme = "kanso-zen.json"

[keys.normal]
"t" = { PluginCommand = "ThemeBrowser" }
"#
        );
    }

    #[test]
    fn update_theme_config_appends_theme_when_missing() {
        let updated = update_theme_config_contents("[keys.normal]\n", "kanso-pearl.json").unwrap();

        assert_eq!(updated, "theme = \"kanso-pearl.json\"\n[keys.normal]\n");
    }

    #[test]
    fn test_lsp_config_defaults_to_rust() {
        let config: Config = toml::from_str(
            r#"
theme = "theme/nightfox.json"

[keys]
"#,
        )
        .unwrap();

        let rust = config.lsp.servers.get("rust").unwrap();
        let typescript = config.lsp.servers.get("typescript").unwrap();
        assert!(config.lsp.enabled);
        assert_eq!(rust.command, "rust-analyzer");
        assert_eq!(rust.args, vec!["-v"]);
        assert_eq!(rust.language_id, "rust");
        assert_eq!(rust.file_extensions, vec!["rs"]);
        assert_eq!(typescript.command, "typescript-language-server");
        assert!(config.lsp.servers.contains_key("markdown"));
        assert!(config.lsp.servers.contains_key("python"));
        assert!(config.lsp.servers.contains_key("json"));
        assert!(config.lsp.servers.contains_key("toml"));
        assert!(config.lsp.servers.contains_key("yaml"));
        let lua = config.lsp.servers.get("lua").unwrap();
        assert_eq!(lua.command, "lua-language-server");
        assert_eq!(lua.documents(), vec![document("lua", &["lua"])]);
    }

    #[test]
    fn config_overrides_replace_scalars_and_merge_nested_tables() {
        let config = Config::from_toml_with_overrides(
            r#"
theme = "mocha.json"
mouse_scroll_lines = 3

[keys.normal]
"Ctrl-p" = "FilePicker"

[plugins]
buffer_picker = "buffer_picker.hk"
"#,
            &[
                r#"theme = "nightfox.json""#.to_string(),
                r#"keys.normal."Ctrl-t" = { PluginCommand = "LspDocumentSymbols" }"#.to_string(),
                r#"plugins.lsp_symbols = "/tmp/lsp_symbols.hk""#.to_string(),
            ],
        )
        .unwrap();

        assert_eq!(config.theme, "nightfox.json");
        assert_eq!(config.mouse_scroll_lines, Some(3));
        assert_eq!(
            config.keys.normal.get("Ctrl-p"),
            Some(&KeyAction::Single(Action::FilePicker))
        );
        assert_eq!(
            config.keys.normal.get("Ctrl-t"),
            Some(&KeyAction::Single(Action::PluginCommand(
                "LspDocumentSymbols".to_string()
            )))
        );
        assert_eq!(
            config.plugins.get("buffer_picker").map(String::as_str),
            Some("buffer_picker.hk")
        );
        assert_eq!(
            config.plugins.get("lsp_symbols").map(String::as_str),
            Some("/tmp/lsp_symbols.hk")
        );
    }

    #[test]
    fn later_config_overrides_win() {
        let config = Config::from_toml_with_overrides(
            r#"
theme = "mocha.json"

[keys]
"#,
            &[
                r#"theme = "nightfox.json""#.to_string(),
                r#"theme = "latte.json""#.to_string(),
            ],
        )
        .unwrap();

        assert_eq!(config.theme, "latte.json");
    }

    #[test]
    fn config_override_errors_include_override_index() {
        let err = Config::from_toml_with_overrides(
            r#"
theme = "mocha.json"

[keys]
"#,
            &[
                r#"theme = "nightfox.json""#.to_string(),
                "theme =".to_string(),
            ],
        )
        .unwrap_err();

        assert!(err.to_string().contains("config override #2"));
    }

    #[test]
    fn user_config_is_layered_over_bundled_defaults() {
        let config = Config::from_user_toml_with_overrides(
            r#"
theme = "latte.json"
disabled_plugins = ["fidget"]

[keys.normal]
"Ctrl-x" = "FilePicker"
"#,
            &[],
        )
        .unwrap();

        assert_eq!(config.theme, "latte.json");
        assert_eq!(
            config.keys.normal.get("Ctrl-t"),
            Some(&KeyAction::Single(Action::PluginCommand(
                "LspDocumentSymbols".to_string()
            )))
        );
        assert_eq!(
            config.keys.normal.get("Ctrl-x"),
            Some(&KeyAction::Single(Action::FilePicker))
        );
        assert!(!config.plugins.contains_key("fidget"));
        assert!(config.plugins.contains_key("theme_browser"));
    }

    #[test]
    fn disable_ai_removes_the_bundled_agent_surface() {
        let config = Config::from_user_toml_with_overrides("disable_ai = true", &[]).unwrap();

        assert!(config.disable_ai);
        assert!(!config.plugins.contains_key("agent"));
    }

    #[test]
    fn custom_codex_command_is_parsed_without_shell_expansion() {
        let config = Config::from_user_toml_with_overrides(
            r#"
[agent]
command = "/opt/codex"
args = ["--strict-config"]
env = { NO_BROWSER = "1" }
"#,
            &[],
        )
        .unwrap();

        assert_eq!(config.agent.command.as_deref(), Some("/opt/codex"));
        assert_eq!(config.agent.args, ["--strict-config"]);
        assert_eq!(
            config.agent.env.get("NO_BROWSER").map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn default_config_maps_star_to_search_word_under_cursor() {
        let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();

        assert_eq!(
            config.keys.normal.get("*"),
            Some(&KeyAction::Single(Action::SearchWordUnderCursor))
        );
    }

    #[test]
    fn default_config_maps_neovim_style_search_keys() {
        let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();

        assert_eq!(config.search, SearchConfig::default());
        assert_eq!(
            config.keys.normal.get("/"),
            Some(&KeyAction::Single(Action::EnterSearch(
                SearchDirection::Forward
            )))
        );
        assert_eq!(
            config.keys.normal.get("?"),
            Some(&KeyAction::Single(Action::EnterSearch(
                SearchDirection::Backward
            )))
        );
        assert_eq!(
            config.keys.normal.get("n"),
            Some(&KeyAction::Single(Action::RepeatSearch))
        );
        assert_eq!(
            config.keys.normal.get("N"),
            Some(&KeyAction::Single(Action::RepeatSearchOpposite))
        );
    }

    #[test]
    fn picker_config_defaults_to_bottom_input() {
        let config: Config = toml::from_str(
            r#"
theme = "mocha.json"

[keys]
"#,
        )
        .unwrap();

        assert_eq!(config.picker.input_position, PickerInputPosition::Bottom);
    }

    #[test]
    fn picker_config_parses_top_input() {
        let config: Config = toml::from_str(
            r#"
theme = "mocha.json"

[picker]
input_position = "top"

[keys]
"#,
        )
        .unwrap();

        assert_eq!(config.picker.input_position, PickerInputPosition::Top);
    }

    #[test]
    fn picker_config_rejects_invalid_input_position() {
        let err = toml::from_str::<Config>(
            r#"
theme = "mocha.json"

[picker]
input_position = "left"

[keys]
"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("input_position"));
    }

    #[test]
    fn default_config_maps_wrap_toggle_key() {
        let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();

        assert_eq!(
            config.keys.normal.get("W"),
            Some(&KeyAction::Single(Action::ToggleWrap))
        );
    }

    #[test]
    fn default_config_maps_matchit_keys() {
        let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();

        assert_eq!(
            config.keys.normal.get("%"),
            Some(&KeyAction::Single(Action::MatchitForward))
        );
        let Some(KeyAction::Nested(g)) = config.keys.normal.get("g") else {
            panic!("default config should map g to nested actions");
        };
        assert_eq!(
            g.get("%"),
            Some(&KeyAction::Single(Action::MatchitBackward))
        );
    }

    #[test]
    fn matchit_config_defaults_and_language_groups() {
        let config = Config::from_toml_with_overrides(
            r#"
theme = "mocha.json"

[keys]

[matchit.languages.vim]
groups = [["\\bif\\b", "\\belse\\b", "\\bendif\\b"]]
"#,
            &[],
        )
        .unwrap();

        assert!(config.matchit.enabled);
        assert_eq!(
            config.matchit.pairs,
            vec![
                ["(".to_string(), ")".to_string()],
                ["{".to_string(), "}".to_string()],
                ["[".to_string(), "]".to_string()],
            ]
        );
        assert_eq!(
            config.matchit.languages["vim"].groups,
            vec![vec![
                "\\bif\\b".to_string(),
                "\\belse\\b".to_string(),
                "\\bendif\\b".to_string()
            ]]
        );
    }

    #[test]
    fn default_config_enables_window_management_prefix() {
        let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
        let Some(KeyAction::Nested(ctrl_w)) = config.keys.normal.get("Ctrl-w") else {
            panic!("default config should map Ctrl-w to window management actions");
        };

        assert_eq!(
            ctrl_w.get("s"),
            Some(&KeyAction::Single(Action::SplitHorizontal))
        );
        assert_eq!(
            ctrl_w.get("v"),
            Some(&KeyAction::Single(Action::SplitVertical))
        );
        assert_eq!(
            ctrl_w.get("w"),
            Some(&KeyAction::Single(Action::NextWindow))
        );
        assert_eq!(
            ctrl_w.get("W"),
            Some(&KeyAction::Single(Action::PreviousWindow))
        );
        assert_eq!(
            ctrl_w.get("c"),
            Some(&KeyAction::Single(Action::CloseWindow))
        );
        assert_eq!(
            ctrl_w.get("="),
            Some(&KeyAction::Single(Action::BalanceWindows))
        );
        assert_eq!(
            ctrl_w.get("_"),
            Some(&KeyAction::Single(Action::MaximizeWindow))
        );
        assert_eq!(
            ctrl_w.get("o"),
            Some(&KeyAction::Single(Action::OnlyWindow))
        );
    }

    #[test]
    fn default_config_maps_ctrl_t_to_lsp_document_symbols() {
        let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();

        assert_eq!(
            config.keys.normal.get("Ctrl-t"),
            Some(&KeyAction::Single(Action::PluginCommand(
                "LspDocumentSymbols".to_string()
            )))
        );
        assert_eq!(
            config.plugins.get("lsp_symbols").map(String::as_str),
            Some("lsp_symbols.hk")
        );
        assert_eq!(
            config.plugins.get("cool_search").map(String::as_str),
            Some("cool_search.hk")
        );
        assert_eq!(
            config.plugins.get("inlay_hints").map(String::as_str),
            Some("inlay_hints.hk")
        );

        let Some(KeyAction::Nested(leader)) = config.keys.normal.get(" ") else {
            panic!("expected a Space leader mapping");
        };
        assert_eq!(
            leader.get("w"),
            Some(&KeyAction::Single(Action::PluginCommand(
                "LspWorkspaceSymbols".to_string()
            )))
        );
        assert_eq!(
            leader.get("k"),
            Some(&KeyAction::Single(Action::PluginCommand(
                "LspReferences".to_string()
            )))
        );
        assert_eq!(
            leader.get("f"),
            Some(&KeyAction::Single(Action::FormatDocument))
        );
        assert_eq!(
            leader.get("."),
            Some(&KeyAction::Single(Action::CodeAction))
        );
        assert_eq!(
            leader.get("r"),
            Some(&KeyAction::Single(Action::StartRename))
        );
        assert_eq!(
            config.keys.insert.get("Ctrl-k"),
            Some(&KeyAction::Single(Action::SignatureHelp))
        );
    }

    #[test]
    fn default_config_maps_command_palette_entrypoints_and_enables_key_hints() {
        let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();

        assert_eq!(
            config.keys.normal.get("F1"),
            Some(&KeyAction::Single(Action::CommandPalette))
        );
        assert_eq!(
            config.keys.normal.get("Ctrl-Shift-p"),
            Some(&KeyAction::Single(Action::CommandPalette))
        );
        assert_eq!(
            config.keys.normal.get("Alt-x"),
            Some(&KeyAction::Single(Action::CommandPalette))
        );
        let Some(KeyAction::Nested(leader)) = config.keys.normal.get(" ") else {
            panic!("expected a Space leader mapping");
        };
        assert_eq!(
            leader.get("?"),
            Some(&KeyAction::Single(Action::CommandPalette))
        );
        assert_eq!(config.key_hints, KeyHintsConfig::default());
        assert!(config.key_hints.enabled);
        assert_eq!(config.key_hints.delay_ms, 250);
    }

    #[test]
    fn user_config_can_disable_or_delay_key_hints() {
        let config = Config::from_user_toml_with_overrides(
            "[key_hints]\nenabled = false\ndelay_ms = 750\n",
            &[],
        )
        .unwrap();

        assert!(!config.key_hints.enabled);
        assert_eq!(config.key_hints.delay_ms, 750);
    }

    #[test]
    fn default_config_maps_leader_a_to_select_all() {
        let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
        let Some(KeyAction::Nested(leader)) = config.keys.normal.get(" ") else {
            panic!("expected a Space leader mapping");
        };

        assert_eq!(
            leader.get("a"),
            Some(&KeyAction::Multiple(vec![
                Action::MoveToTop,
                Action::EnterMode(Mode::VisualLine),
                Action::MoveToBottom,
            ]))
        );
        let Some(KeyAction::Nested(visual_leader)) = config.keys.visual.get(" ") else {
            panic!("expected a visual Space leader mapping");
        };
        assert_eq!(
            visual_leader.get("A"),
            Some(&KeyAction::Single(Action::PluginCommand(
                "Agent".to_string()
            )))
        );
        assert_eq!(
            leader.get("A"),
            Some(&KeyAction::Single(Action::PluginCommand(
                "Agent".to_string()
            )))
        );
    }

    #[test]
    fn default_config_maps_ctrl_w_a_to_agent_open() {
        let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
        let Some(KeyAction::Nested(window_commands)) = config.keys.normal.get("Ctrl-w") else {
            panic!("expected a Ctrl-w keymap");
        };

        assert_eq!(
            window_commands.get("a"),
            Some(&KeyAction::Single(Action::PluginCommand(
                "AgentOpen".to_string()
            )))
        );
    }

    #[test]
    fn default_config_enables_project_search() {
        let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
        let Some(KeyAction::Nested(leader)) = config.keys.normal.get(" ") else {
            panic!("space should be a keymap");
        };

        assert_eq!(
            leader.get("g"),
            Some(&KeyAction::Single(Action::PluginCommand(
                "ProjectSearch".to_string()
            )))
        );
        assert_eq!(
            config.plugins.get("project_search").map(String::as_str),
            Some("project_search.hk")
        );
        let permissions = config.plugin_permissions.get("project_search").unwrap();
        assert_eq!(permissions.process, vec!["rg".to_string()]);
        assert_eq!(config.log_file.as_deref(), Some("/tmp/red.log"));
    }

    #[test]
    fn cursor_config_defaults_match_current_behavior() {
        let config: Config = toml::from_str(
            r#"
theme = "theme/nightfox.json"

[keys]
"#,
        )
        .unwrap();

        assert_eq!(config.cursor.normal, CursorShape::Default);
        assert_eq!(config.cursor.insert, CursorShape::SteadyBar);
        assert_eq!(config.cursor.command, CursorShape::Default);
        assert_eq!(config.cursor.search, CursorShape::Default);
        assert_eq!(config.cursor.visual, CursorShape::Default);
        assert_eq!(config.cursor.visual_line, CursorShape::Default);
        assert_eq!(config.cursor.visual_block, CursorShape::Default);
        assert_eq!(config.cursor.waiting, CursorShape::SteadyUnderscore);
    }

    #[test]
    fn cursor_config_accepts_supported_shapes() {
        let config: Config = toml::from_str(
            r#"
theme = "theme/nightfox.json"

[cursor]
normal = "default"
insert = "blinking_block"
command = "steady_block"
search = "blinking_underscore"
visual = "steady_underscore"
visual_line = "blinking_bar"
visual_block = "steady_bar"
waiting = "steady_underscore"

[keys]
"#,
        )
        .unwrap();

        assert_eq!(config.cursor.normal, CursorShape::Default);
        assert_eq!(config.cursor.insert, CursorShape::BlinkingBlock);
        assert_eq!(config.cursor.command, CursorShape::SteadyBlock);
        assert_eq!(config.cursor.search, CursorShape::BlinkingUnderscore);
        assert_eq!(config.cursor.visual, CursorShape::SteadyUnderscore);
        assert_eq!(config.cursor.visual_line, CursorShape::BlinkingBar);
        assert_eq!(config.cursor.visual_block, CursorShape::SteadyBar);
        assert_eq!(config.cursor.waiting, CursorShape::SteadyUnderscore);
    }

    #[test]
    fn cursor_config_rejects_unknown_shapes() {
        let config = toml::from_str::<Config>(
            r#"
theme = "theme/nightfox.json"

[cursor]
waiting = "tiny_triangle"

[keys]
"#,
        );

        assert!(config.is_err());
    }

    #[test]
    fn default_config_documents_cursor_defaults() {
        let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();

        assert_eq!(config.cursor.normal, CursorShape::Default);
        assert_eq!(config.cursor.insert, CursorShape::SteadyBar);
        assert_eq!(config.cursor.waiting, CursorShape::SteadyUnderscore);
    }

    #[test]
    fn plugin_process_permissions_default_to_empty() {
        let config: Config = toml::from_str(
            r#"
theme = "theme/nightfox.json"

[keys]
"#,
        )
        .unwrap();

        assert!(config.plugin_permissions.is_empty());
    }

    #[test]
    fn plugin_process_permissions_accept_executable_allowlists() {
        let config: Config = toml::from_str(
            r#"
theme = "theme/nightfox.json"

[keys]

[plugin_permissions.project_search]
process = ["rg", "/usr/bin/git"]
"#,
        )
        .unwrap();

        assert_eq!(
            config.plugin_permissions.get("project_search"),
            Some(&PluginPermissions {
                process: vec!["rg".to_string(), "/usr/bin/git".to_string()],
            })
        );
    }

    #[test]
    fn plugin_config_accepts_nested_settings_and_cli_overrides() {
        let config = Config::from_toml_with_overrides(
            r#"
theme = "theme/nightfox.json"

[keys]

[plugin_config.lsp_symbols.icons]
enabled = true

[plugin_config.lsp_symbols.icons.overrides]
struct = "S"
enum = "E"
"#,
            &[
                r#"plugin_config.lsp_symbols.icons.enabled = false"#.to_string(),
                r#"plugin_config.lsp_symbols.icons.overrides.enum = "enum-icon""#.to_string(),
            ],
        )
        .unwrap();

        let icons = &config.plugin_config["lsp_symbols"]["icons"];
        assert_eq!(icons["enabled"], json!(false));
        assert_eq!(icons["overrides"]["struct"], json!("S"));
        assert_eq!(icons["overrides"]["enum"], json!("enum-icon"));
    }

    #[test]
    fn test_lsp_config_accepts_additional_servers() {
        let config: Config = toml::from_str(
            r#"
theme = "theme/nightfox.json"

[keys]

[lsp]
enabled = true
format_on_save = true

[lsp.servers.typescript]
command = "typescript-language-server"
args = ["--stdio"]
language_id = "typescript"
file_extensions = ["ts", "tsx"]
root_markers = ["package.json", ".git"]
workspace_name = "frontend"
"#,
        )
        .unwrap();

        let server = config.lsp.servers.get("typescript").unwrap();
        assert!(config.lsp.format_on_save);
        assert!(config.lsp.servers.contains_key("rust"));
        assert_eq!(server.command, "typescript-language-server");
        assert_eq!(server.args, vec!["--stdio"]);
        assert_eq!(server.language_id, "typescript");
        assert_eq!(server.file_extensions, vec!["ts", "tsx"]);
        assert_eq!(server.documents()[0].language_id, "typescript");
        assert_eq!(server.documents()[0].file_extensions, vec!["ts", "tsx"]);
        assert_eq!(server.root_markers, vec!["package.json", ".git"]);
        assert_eq!(server.workspace_name.as_deref(), Some("frontend"));
    }

    #[test]
    fn test_lsp_config_accepts_document_selectors() {
        let config: Config = toml::from_str(
            r#"
theme = "theme/nightfox.json"

[keys]

[lsp.servers.web]
command = "typescript-language-server"
args = ["--stdio"]
root_markers = ["package.json", ".git"]

[[lsp.servers.web.documents]]
language_id = "typescript"
file_extensions = ["ts"]

[[lsp.servers.web.documents]]
language_id = "javascript"
file_extensions = ["js"]
"#,
        )
        .unwrap();

        let server = config.lsp.servers.get("web").unwrap();
        assert_eq!(server.language_id, "");
        assert_eq!(server.file_extensions, Vec::<String>::new());
        assert_eq!(
            server.documents(),
            vec![
                LanguageDocumentConfig {
                    language_id: "typescript".to_string(),
                    file_extensions: vec!["ts".to_string()],
                },
                LanguageDocumentConfig {
                    language_id: "javascript".to_string(),
                    file_extensions: vec!["js".to_string()],
                },
            ]
        );
    }
}
