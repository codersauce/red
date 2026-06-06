use std::{collections::HashMap, fs, path::PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};

use crate::editor::Action;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Config {
    pub keys: Keys,
    pub theme: String,
    #[serde(default)]
    pub plugins: HashMap<String, String>,
    pub log_file: Option<String>,
    pub mouse_scroll_lines: Option<usize>,
    pub scrolloff: Option<usize>,
    #[serde(default)]
    pub lsp: LspConfig,
    #[serde(default = "default_true")]
    pub show_diagnostics: bool,
    #[serde(default = "default_false")]
    pub window_borders_ascii: bool,
    #[serde(default, skip_serializing)]
    pub startup_file_count: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LspConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
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
    pub fn path(p: &str) -> PathBuf {
        #[allow(deprecated)]
        std::env::home_dir()
            .unwrap()
            .join(".config")
            .join("red")
            .join(p)
    }

    pub fn persist_theme(theme_name: &str) -> anyhow::Result<()> {
        let config_path = Self::path("config.toml");
        let contents = fs::read_to_string(&config_path)?;
        fs::write(
            config_path,
            update_theme_config_contents(&contents, theme_name)?,
        )?;
        Ok(())
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
    use crate::editor::Mode;

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
    }

    #[test]
    fn test_lsp_config_accepts_additional_servers() {
        let config: Config = toml::from_str(
            r#"
theme = "theme/nightfox.json"

[keys]

[lsp]
enabled = true

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
