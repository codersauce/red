//! Layered configuration loading with diagnostic recovery instead of startup-wide failure.
//!
//! Embedded defaults form the complete baseline, user TOML applies independent
//! overrides, and command-line fragments are strict final overrides. Invalid user
//! fields are diagnosed and replaced by their corresponding safe defaults when that can
//! be done independently; malformed whole-file input falls back to a deliberately
//! restricted profile. Loading never rewrites the user's configuration.
//!
//! [`LoadedConfig`] retains both the usable [`Config`] and the diagnostics explaining
//! every fallback. Runtime validation can append diagnostics through the same model so
//! the editor has one acknowledgement and display path.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt, fs, io,
    ops::Range,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{json, Value};

use crate::assets;
use crate::editor::Action;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// User-facing importance of a recovered configuration problem.
pub enum ConfigDiagnosticSeverity {
    /// Configuration remains usable with a non-critical fallback.
    Warning,
    /// The requested value could not be honored.
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Input layer that produced a configuration diagnostic.
pub enum ConfigDiagnosticSource {
    /// User configuration file at this path.
    UserFile(PathBuf),
    /// Zero-based command-line override index.
    CliOverride(usize),
}

impl fmt::Display for ConfigDiagnosticSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UserFile(path) => write!(formatter, "{}", path.display()),
            Self::CliOverride(index) => write!(formatter, "override #{index}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Source-located configuration problem and the fallback Red selected.
pub struct ConfigDiagnostic {
    /// Diagnostic importance.
    pub severity: ConfigDiagnosticSeverity,
    /// Stable machine-readable diagnostic code.
    pub code: String,
    /// Configuration layer that supplied the invalid value.
    pub source: ConfigDiagnosticSource,
    /// Byte range in the source text, when it can be recovered.
    pub span: Option<Range<usize>>,
    /// One-based source line.
    pub line: Option<usize>,
    /// One-based source column.
    pub column: Option<usize>,
    /// Dotted configuration key path.
    pub path: String,
    /// Human-readable explanation.
    pub message: String,
    /// Safe behavior Red selected instead.
    pub fallback: String,
}

impl ConfigDiagnostic {
    /// Formats a compact source-located diagnostic for terminal output.
    pub fn format(&self) -> String {
        let location = match (self.line, self.column) {
            (Some(line), Some(column)) => format!("{}:{line}:{column}", self.source),
            _ => self.source.to_string(),
        };
        format!(
            "{location}: {} {} at {}: {}; fallback: {}",
            self.code,
            match self.severity {
                ConfigDiagnosticSeverity::Warning => "warning",
                ConfigDiagnosticSeverity::Error => "error",
            },
            self.path,
            self.message,
            self.fallback
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Extent to which loading had to recover from invalid user input.
pub enum ConfigRecovery {
    /// Every supplied value was accepted.
    Clean,
    /// One or more fields fell back independently.
    Partial,
    /// The user file was unusable and a restricted fallback profile was used.
    WholeFileFallback,
}

#[derive(Debug)]
/// Usable configuration paired with all diagnostics from layered loading.
pub struct LoadedConfig {
    /// Effective configuration after defaults, recovery, and overrides.
    pub config: Config,
    /// Problems encountered while producing `config`.
    pub diagnostics: Vec<ConfigDiagnostic>,
    /// Coarse summary of the recovery path.
    pub recovery: ConfigRecovery,
    source_path: PathBuf,
    source_text: String,
    override_fragments: Vec<String>,
}

impl LoadedConfig {
    /// Returns whether loading required no fallback and produced no diagnostics.
    pub fn is_clean(&self) -> bool {
        self.recovery == ConfigRecovery::Clean && self.diagnostics.is_empty()
    }

    /// Returns the user configuration file whose contents produced this snapshot.
    #[must_use]
    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    /// Returns legacy server definitions explicitly supplied by the user or CLI.
    #[must_use]
    pub fn explicit_language_server_names(&self) -> HashSet<String> {
        self.explicit_names_at_path("lsp", "servers")
            .into_iter()
            .filter(|name| self.config.lsp.servers.contains_key(name))
            .collect()
    }

    /// Returns legacy comment templates explicitly supplied by the user or CLI.
    #[must_use]
    pub fn explicit_comment_language_names(&self) -> HashSet<String> {
        self.explicit_names_at_path("commenting", "languages")
    }

    fn explicit_names_at_path(&self, section: &str, entries: &str) -> HashSet<String> {
        let mut names = HashSet::new();
        for source in std::iter::once(self.source_text.as_str())
            .chain(self.override_fragments.iter().map(String::as_str))
        {
            let Ok(value) = source.parse::<toml::Value>() else {
                continue;
            };
            if let Some(table) = value
                .get(section)
                .and_then(|section| section.get(entries))
                .and_then(toml::Value::as_table)
            {
                names.extend(table.keys().cloned());
            }
        }
        names
    }

    /// Adds a post-load validation problem using the original user source.
    pub fn add_runtime_diagnostic(
        &mut self,
        code: &str,
        severity: ConfigDiagnosticSeverity,
        path: &[String],
        message: impl Into<String>,
        fallback: impl Into<String>,
    ) {
        self.diagnostics.push(diagnostic_for_path(
            &self.source_text,
            ConfigDiagnosticSource::UserFile(self.source_path.clone()),
            code,
            severity,
            path,
            message.into(),
            fallback.into(),
        ));
        if self.recovery == ConfigRecovery::Clean {
            self.recovery = ConfigRecovery::Partial;
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
/// Complete effective editor configuration.
///
/// Optional scalar fields preserve whether the user supplied a value while
/// access sites apply embedded defaults. Collections use Serde defaults so
/// older user files remain compatible.
pub struct Config {
    /// Per-mode key mappings.
    pub keys: Keys,
    /// Runtime theme name.
    pub theme: String,
    /// Cursor shape by editor mode.
    #[serde(default)]
    pub cursor: CursorConfig,
    /// User plugin name-to-path overrides.
    #[serde(default)]
    pub plugins: HashMap<String, String>,
    /// Plugin names excluded from discovery or activation.
    #[serde(default)]
    pub disabled_plugins: Vec<String>,
    /// Capabilities granted to individual plugins.
    #[serde(default)]
    pub plugin_permissions: HashMap<String, PluginPermissions>,
    /// Plugin-specific JSON-compatible settings.
    #[serde(default)]
    pub plugin_config: HashMap<String, Value>,
    /// Log destination override.
    pub log_file: Option<String>,
    /// Lines moved per terminal mouse-wheel event.
    pub mouse_scroll_lines: Option<usize>,
    /// Preferred visible lines retained above and below the cursor.
    pub scrolloff: Option<usize>,
    /// Whether long lines wrap to continuation rows.
    pub wrap: Option<bool>,
    /// Show the cursor line's absolute number and distances on other lines.
    /// Defaults to off.
    pub relative_line_numbers: Option<bool>,
    /// Indent wrapped continuation rows to the line's leading whitespace,
    /// like vim's 'breakindent'. Defaults to on.
    pub breakindent: Option<bool>,
    /// Horizontal columns moved when the cursor leaves an unwrapped viewport.
    pub sidescroll: Option<usize>,
    /// Preferred visible columns retained beside the cursor.
    pub sidescrolloff: Option<usize>,
    /// Show the startup splash when red opens without file arguments.
    /// Defaults to on.
    pub splash: Option<bool>,
    /// Announce each newly installed Red release once after interactive startup.
    /// Defaults to on.
    #[serde(default)]
    pub show_whats_new: Option<bool>,
    /// Refresh bundled release notes from the matching published GitHub release.
    /// Defaults to on.
    #[serde(default)]
    pub fetch_release_notes: Option<bool>,
    /// Retain inline conversations in local editor recovery snapshots. Defaults to on.
    #[serde(default)]
    pub persist_inline_history: Option<bool>,
    /// Interactive search behavior.
    #[serde(default)]
    pub search: SearchConfig,
    /// Insert-mode completion sources and automatic triggering.
    #[serde(default)]
    pub completion: CompletionConfig,
    /// Non-modal callable signatures shown near the Insert-mode cursor.
    #[serde(default)]
    pub signature_help: SignatureHelpConfig,
    /// Opt-in AI inline completion, independent of ordinary language servers.
    #[serde(default)]
    pub copilot: crate::copilot::CopilotConfig,
    /// Picker layout behavior.
    #[serde(default)]
    pub picker: PickerConfig,
    /// Ordered status-line sections and icon presentation.
    #[serde(default)]
    pub statusline: StatuslineConfig,
    /// Delayed key-prefix guide behavior.
    #[serde(default)]
    pub key_hints: KeyHintsConfig,
    /// System clipboard integration.
    #[serde(default)]
    pub clipboard: ClipboardConfig,
    /// Language-server routing and behavior.
    #[serde(default)]
    pub lsp: LspConfig,
    /// Document formatting behavior shared by language servers and external tools.
    #[serde(default)]
    pub formatting: FormattingConfig,
    /// User-defined syntax, grammar, formatting, and language-server definitions.
    #[serde(default)]
    pub languages: HashMap<String, LanguageConfig>,
    /// Language-specific templates used by Vim-style line commenting.
    #[serde(default)]
    pub commenting: CommentingConfig,
    /// Matching-token navigation.
    #[serde(default)]
    pub matchit: MatchitConfig,
    /// Disable every agent surface, adapter check, and process launch.
    #[serde(default = "default_false")]
    pub disable_ai: bool,
    /// Unsupported development escape hatch set by `--no-typecheck`.
    #[serde(default, skip_serializing)]
    pub disable_plugin_typecheck: bool,
    /// Codex adapter launch configuration.
    #[serde(default)]
    pub agent: AgentConfig,
    /// Diagnostic presentation in the editor gutter.
    #[serde(default)]
    pub diagnostics: DiagnosticsConfig,
    /// Whether diagnostics appear in editor UI.
    #[serde(default = "default_true")]
    pub show_diagnostics: bool,
    /// Whether split borders use only ASCII characters.
    #[serde(default = "default_false")]
    pub window_borders_ascii: bool,
    /// Number of files supplied at startup; runtime-only context.
    #[serde(default, skip_serializing)]
    pub startup_file_count: usize,
    /// Whether startup already restored a core-owned recovery snapshot.
    #[serde(default, skip_serializing)]
    pub startup_session_resumed: bool,
}

/// Direct Codex CLI launch configuration.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// Codex executable override. Red uses `codex` from PATH when absent.
    pub command: Option<String>,
    /// Additional arguments appended to the Codex command.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment additions supplied only to the Codex child process.
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// Diagnostic presentation layered on top of the master `show_diagnostics` switch.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsConfig {
    /// Show one severity-aware sign for each line that has diagnostics.
    #[serde(default = "default_true")]
    pub gutter_signs: bool,
    /// Glyph family used for diagnostic gutter signs.
    #[serde(default)]
    pub icon_style: PickerIconStyle,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self {
            gutter_signs: true,
            icon_style: PickerIconStyle::NerdFont,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
/// Picker input placement.
pub struct PickerConfig {
    /// Whether the query row appears above or below results.
    #[serde(default)]
    pub input_position: PickerInputPosition,
    #[serde(default)]
    pub icons: PickerIconsConfig,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PickerIconsConfig {
    #[serde(default)]
    pub style: PickerIconStyle,
    #[serde(default = "default_true")]
    pub color: bool,
}

impl Default for PickerIconsConfig {
    fn default() -> Self {
        Self {
            style: PickerIconStyle::NerdFont,
            color: true,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PickerIconStyle {
    Unicode,
    #[default]
    NerdFont,
    Ascii,
    None,
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
            icons: PickerIconsConfig::default(),
        }
    }
}

/// Configurable status-line layout.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StatuslineConfig {
    /// Sections rendered from the left edge toward the center.
    #[serde(default = "default_statusline_left")]
    pub left: Vec<StatuslineSection>,
    /// Sections rendered from the right edge toward the center.
    #[serde(default = "default_statusline_right")]
    pub right: Vec<StatuslineSection>,
    /// Icon style shared by Git and syntax sections.
    #[serde(default)]
    pub icons: PickerIconsConfig,
}

impl Default for StatuslineConfig {
    fn default() -> Self {
        Self {
            left: default_statusline_left(),
            right: default_statusline_right(),
            icons: PickerIconsConfig::default(),
        }
    }
}

fn default_statusline_left() -> Vec<StatuslineSection> {
    vec![
        StatuslineSection::Mode,
        StatuslineSection::Diagnostics,
        StatuslineSection::GitBranch,
        StatuslineSection::Filename,
    ]
}

fn default_statusline_right() -> Vec<StatuslineSection> {
    vec![StatuslineSection::Position, StatuslineSection::Syntax]
}

/// A piece of editor context that can be placed on either status-line side.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StatuslineSection {
    Mode,
    GitBranch,
    Filename,
    Syntax,
    Position,
    Diagnostics,
    GitChanges,
    LspStatus,
    CurrentSymbol,
    Selection,
    Recording,
    SearchMatches,
    Indentation,
    Encoding,
    LineEndings,
    ReadOnly,
    Modified,
    Workspace,
    RelativePath,
    BufferIndex,
    WindowIndex,
    FileSize,
    AgentActivity,
    Formatter,
    Clock,
}

impl StatuslineSection {
    pub const ALL: [Self; 25] = [
        Self::Mode,
        Self::GitBranch,
        Self::Filename,
        Self::Syntax,
        Self::Position,
        Self::Diagnostics,
        Self::GitChanges,
        Self::LspStatus,
        Self::CurrentSymbol,
        Self::Selection,
        Self::Recording,
        Self::SearchMatches,
        Self::Indentation,
        Self::Encoding,
        Self::LineEndings,
        Self::ReadOnly,
        Self::Modified,
        Self::Workspace,
        Self::RelativePath,
        Self::BufferIndex,
        Self::WindowIndex,
        Self::FileSize,
        Self::AgentActivity,
        Self::Formatter,
        Self::Clock,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mode => "mode",
            Self::GitBranch => "git_branch",
            Self::Filename => "filename",
            Self::Syntax => "syntax",
            Self::Position => "position",
            Self::Diagnostics => "diagnostics",
            Self::GitChanges => "git_changes",
            Self::LspStatus => "lsp_status",
            Self::CurrentSymbol => "current_symbol",
            Self::Selection => "selection",
            Self::Recording => "recording",
            Self::SearchMatches => "search_matches",
            Self::Indentation => "indentation",
            Self::Encoding => "encoding",
            Self::LineEndings => "line_endings",
            Self::ReadOnly => "read_only",
            Self::Modified => "modified",
            Self::Workspace => "workspace",
            Self::RelativePath => "relative_path",
            Self::BufferIndex => "buffer_index",
            Self::WindowIndex => "window_index",
            Self::FileSize => "file_size",
            Self::AgentActivity => "agent_activity",
            Self::Formatter => "formatter",
            Self::Clock => "clock",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Mode => "Mode",
            Self::GitBranch => "Git branch",
            Self::Filename => "Filename",
            Self::Syntax => "Syntax",
            Self::Position => "Cursor position",
            Self::Diagnostics => "Diagnostics",
            Self::GitChanges => "Git changes",
            Self::LspStatus => "LSP status",
            Self::CurrentSymbol => "Current symbol",
            Self::Selection => "Selection",
            Self::Recording => "Macro recording",
            Self::SearchMatches => "Search matches",
            Self::Indentation => "Indentation",
            Self::Encoding => "Encoding",
            Self::LineEndings => "Line endings",
            Self::ReadOnly => "Read-only",
            Self::Modified => "Modified",
            Self::Workspace => "Workspace",
            Self::RelativePath => "Relative path",
            Self::BufferIndex => "Buffer index",
            Self::WindowIndex => "Window index",
            Self::FileSize => "File size",
            Self::AgentActivity => "Agent activity",
            Self::Formatter => "Formatter",
            Self::Clock => "Clock",
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
/// Vertical placement of the picker query row.
pub enum PickerInputPosition {
    /// Place input before results.
    Top,
    /// Place input after results.
    #[default]
    Bottom,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
/// System clipboard synchronization policy.
pub struct ClipboardConfig {
    /// Master switch for system clipboard access.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Copy yanked editor text to the system clipboard.
    #[serde(default = "default_true")]
    pub sync_on_yank: bool,
    /// Prefer system clipboard text for paste operations.
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

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
/// Language-specific comment templates keyed by canonical language or extension.
pub struct CommentingConfig {
    /// Templates containing one placeholder for the original line contents.
    #[serde(default = "default_comment_templates")]
    pub languages: HashMap<String, String>,
}

impl Default for CommentingConfig {
    fn default() -> Self {
        Self {
            languages: default_comment_templates(),
        }
    }
}

fn default_comment_templates() -> HashMap<String, String> {
    [
        ("bash", "# %s"),
        ("c", "// %s"),
        ("cc", "// %s"),
        ("cpp", "// %s"),
        ("css", "/* %s */"),
        ("cxx", "// %s"),
        ("fish", "# %s"),
        ("go", "// %s"),
        ("h", "// %s"),
        ("hpp", "// %s"),
        ("html", "<!-- %s -->"),
        ("husk", "// %s"),
        ("java", "// %s"),
        ("javascript", "// %s"),
        ("jsonc", "// %s"),
        ("jsx", "// %s"),
        ("lua", "-- %s"),
        ("markdown", "<!-- %s -->"),
        ("powershell", "# %s"),
        ("rust", "// %s"),
        ("scss", "/* %s */"),
        ("sql", "-- %s"),
        ("toml", "# %s"),
        ("tsx", "// %s"),
        ("typescript", "// %s"),
        ("xml", "<!-- %s -->"),
        ("yaml", "# %s"),
    ]
    .into_iter()
    .map(|(language, template)| (language.to_string(), template.to_string()))
    .collect()
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginPermissions {
    /// Executables this plugin may launch through the process API.
    ///
    /// Entries are matched exactly against the requested command. Red does
    /// not invoke a shell when launching plugin processes.
    #[serde(default)]
    pub process: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
/// Matching-token navigation configuration.
pub struct MatchitConfig {
    /// Master switch for matching-token navigation.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Literal opener/closer pairs.
    #[serde(default = "default_matchit_pairs")]
    pub pairs: Vec<[String; 2]>,
    /// Language-specific token groups keyed by normalized extension.
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
/// Language-specific groups whose members cycle as matching tokens.
pub struct MatchitLanguageConfig {
    /// Ordered groups of equivalent matching constructs.
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
/// Interactive search policy.
pub struct SearchConfig {
    /// Update the current match while the query changes.
    #[serde(default = "default_true")]
    pub incsearch: bool,
    /// Keep matches highlighted after search completes.
    #[serde(default = "default_true")]
    pub hlsearch: bool,
    /// Continue searching from the opposite buffer end.
    #[serde(default = "default_true")]
    pub wrapscan: bool,
    /// Compare case-insensitively by default.
    #[serde(default = "default_false")]
    pub ignorecase: bool,
    /// Restore case sensitivity when the query contains uppercase characters.
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

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
/// Insert-mode completion behavior shared by buffer and language-server sources.
pub struct CompletionConfig {
    /// Request completion after an identifier prefix is typed.
    #[serde(default = "default_true")]
    pub auto_trigger: bool,
    /// Minimum identifier prefix length required for automatic completion.
    #[serde(default = "default_completion_min_prefix_length")]
    pub min_prefix_length: usize,
    /// Quiet period after typing before automatic completion is requested.
    #[serde(default = "default_completion_debounce_ms")]
    pub debounce_ms: u64,
    /// Include matching words from open buffers alongside LSP candidates.
    #[serde(default = "default_true")]
    pub buffer_words: bool,
    /// Maximum number of buffer-word candidates collected for one request.
    #[serde(default = "default_completion_max_buffer_words")]
    pub max_buffer_words: usize,
}

impl Default for CompletionConfig {
    fn default() -> Self {
        Self {
            auto_trigger: true,
            min_prefix_length: default_completion_min_prefix_length(),
            debounce_ms: default_completion_debounce_ms(),
            buffer_words: true,
            max_buffer_words: default_completion_max_buffer_words(),
        }
    }
}

fn default_completion_min_prefix_length() -> usize {
    1
}

/// Automatic signature-help presentation; explicit invocation remains available.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SignatureHelpConfig {
    pub auto_trigger: bool,
    pub debounce_ms: u64,
    pub show_documentation: bool,
}

impl Default for SignatureHelpConfig {
    fn default() -> Self {
        Self {
            auto_trigger: true,
            debounce_ms: 120,
            show_documentation: true,
        }
    }
}

fn default_completion_debounce_ms() -> u64 {
    0
}

fn default_completion_max_buffer_words() -> usize {
    100
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
/// Terminal cursor shape requested for an editor mode.
pub enum CursorShape {
    /// Leave cursor shape selection to the terminal.
    #[default]
    Default,
    /// Blinking full cell.
    BlinkingBlock,
    /// Steady full cell.
    SteadyBlock,
    /// Blinking underline.
    BlinkingUnderscore,
    /// Steady underline.
    SteadyUnderscore,
    /// Blinking vertical bar.
    BlinkingBar,
    /// Steady vertical bar.
    SteadyBar,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
/// Cursor shape selected for each editor mode.
pub struct CursorConfig {
    /// Normal mode.
    #[serde(default)]
    pub normal: CursorShape,
    /// Insert mode.
    #[serde(default = "cursor_shape_steady_bar")]
    pub insert: CursorShape,
    /// Command-line mode.
    #[serde(default)]
    pub command: CursorShape,
    /// Search prompt mode.
    #[serde(default)]
    pub search: CursorShape,
    /// Characterwise visual mode.
    #[serde(default)]
    pub visual: CursorShape,
    /// Linewise visual mode.
    #[serde(default)]
    pub visual_line: CursorShape,
    /// Blockwise visual mode.
    #[serde(default)]
    pub visual_block: CursorShape,
    /// Waiting or busy mode.
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
#[serde(deny_unknown_fields)]
/// Language-server subsystem configuration.
pub struct LspConfig {
    /// Master switch for all language-server activity.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Legacy alias for formatting.on_save, normalized by the config loaders.
    #[serde(default)]
    pub format_on_save: bool,
    /// Named language-server launch and routing definitions.
    #[serde(
        default = "default_language_servers",
        deserialize_with = "deserialize_language_servers"
    )]
    pub servers: HashMap<String, LanguageServerConfig>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Selects the formatting backend used for explicit and save-time formatting.
pub enum FormattingProvider {
    /// Prefer a configured external formatter and otherwise ask the language server.
    #[default]
    Auto,
    /// Use only the language pack's external formatter.
    External,
    /// Use only language-server formatting.
    Lsp,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
/// Global document formatting behavior.
pub struct FormattingConfig {
    /// Format supported documents immediately before saving them. Defaults to on.
    #[serde(default = "default_true")]
    pub on_save: bool,
    /// Backend selected for explicit and save-time formatting.
    #[serde(default)]
    pub provider: FormattingProvider,
}

impl Default for FormattingConfig {
    fn default() -> Self {
        Self {
            on_save: true,
            provider: FormattingProvider::default(),
        }
    }
}

/// One configurable language shared by highlighting, editing, and LSP routing.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LanguageConfig {
    /// Case-insensitive file extensions, with or without leading dots.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Case-sensitive exact file names, such as `Dockerfile` or `Makefile`.
    #[serde(default)]
    pub filenames: Vec<String>,
    /// Additional names accepted by syntax selection and injected fenced blocks.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Vim-style line-comment template containing a single `%s` placeholder.
    #[serde(default)]
    pub comment: Option<String>,
    /// Preferred indentation width for files recognized as this language.
    #[serde(default)]
    pub indent_width: Option<usize>,
    /// Bundled or explicitly trusted native Tree-sitter grammar.
    #[serde(default)]
    pub grammar: Option<LanguageGrammarConfig>,
    /// Language-server launch and settings associated with this language.
    #[serde(default)]
    pub lsp: Option<LanguageLspConfig>,
    /// External stdin-to-stdout formatter supplied by the language pack.
    #[serde(default)]
    pub formatter: Option<LanguageFormatterConfig>,
}

/// One external formatter launched directly with the document on stdin.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LanguageFormatterConfig {
    /// Human-readable formatter name shown in editor feedback.
    pub name: String,
    /// Formatter executable launched without a shell.
    pub command: String,
    /// Arguments with optional `{file}` and `{workspace}` placeholders.
    #[serde(default)]
    pub args: Vec<String>,
    /// Files or directories searched upward to select the working directory.
    #[serde(default)]
    pub root_markers: Vec<String>,
    /// Environment additions supplied only to the formatter process.
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// Source, highlighting queries, and platform artifacts for one grammar.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LanguageGrammarConfig {
    /// Existing bundled grammar to reuse rather than opening native code.
    #[serde(default)]
    pub builtin: Option<String>,
    /// Shared-library path; relative package paths remain inside their package.
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// Exported grammar symbol; defaults to `tree_sitter_<language>`.
    #[serde(default)]
    pub symbol: Option<String>,
    /// Ordered paths to Tree-sitter highlight query files.
    #[serde(default)]
    pub highlights: Vec<PathBuf>,
    /// Ordered paths to Tree-sitter structural text-object query files.
    #[serde(default)]
    pub textobjects: Vec<PathBuf>,
    /// Ordered paths to Red indentation query files.
    #[serde(default)]
    pub indents: Vec<PathBuf>,
    /// Optional Tree-sitter injection query file.
    #[serde(default)]
    pub injections: Option<PathBuf>,
    /// Explicit consent to loading this exact configuration-owned native grammar.
    #[serde(default)]
    pub trusted: bool,
    /// Package-bundled or downloadable native artifacts by Rust target triple.
    #[serde(default)]
    pub targets: BTreeMap<String, LanguageGrammarArtifact>,
}

/// One bundled or SHA-256-verified downloadable platform grammar.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LanguageGrammarArtifact {
    /// Package-relative bundled grammar shared library.
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// GitHub HTTPS release artifact URL.
    #[serde(default)]
    pub url: Option<String>,
    /// Expected lowercase or uppercase SHA-256 artifact digest.
    #[serde(default)]
    pub sha256: Option<String>,
}

/// Language-local launch and dynamic `workspace/configuration` settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LanguageLspConfig {
    /// Existing named server to reuse instead of declaring a new executable.
    #[serde(default)]
    pub server: Option<String>,
    /// Language-server executable launched directly, without a shell.
    #[serde(default)]
    pub command: Option<String>,
    /// Command-line arguments supplied to the language server.
    #[serde(default)]
    pub args: Vec<String>,
    /// Workspace root markers searched from the document's parent directory.
    #[serde(default)]
    pub root_markers: Vec<String>,
    /// Environment additions supplied only to the language-server process.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// JSON initialization options included in the LSP initialize request.
    #[serde(default)]
    pub initialization_options: Option<Value>,
    /// JSON settings returned to server `workspace/configuration` requests.
    #[serde(default)]
    pub settings: Option<Value>,
    /// Optional display name reported for the workspace folder.
    #[serde(default)]
    pub workspace_name: Option<String>,
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
#[serde(deny_unknown_fields)]
/// Launch, routing, and initialization settings for one language server.
pub struct LanguageServerConfig {
    /// Executable launched without a shell.
    pub command: String,
    /// Command arguments.
    #[serde(default)]
    pub args: Vec<String>,
    /// Legacy single-document-selector language identifier.
    #[serde(default)]
    pub language_id: String,
    /// Legacy single-document-selector extensions.
    #[serde(default)]
    pub file_extensions: Vec<String>,
    /// Legacy single-document-selector exact file names.
    #[serde(default)]
    pub filenames: Vec<String>,
    /// Preferred set of document selectors sharing this server.
    #[serde(default)]
    pub documents: Vec<LanguageDocumentConfig>,
    /// Files or directories searched upward to select a workspace root.
    #[serde(default)]
    pub root_markers: Vec<String>,
    /// Environment additions supplied only to the server process.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// JSON passed as LSP initialization options.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_toml_compatible_json"
    )]
    pub initialization_options: Option<Value>,
    /// JSON settings returned to server `workspace/configuration` requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<Value>,
    /// Optional display name reported for the workspace folder.
    pub workspace_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
/// Language identifier and extensions routed to a server.
pub struct LanguageDocumentConfig {
    /// LSP language identifier.
    pub language_id: String,
    /// File extensions, with or without leading dots.
    #[serde(default)]
    pub file_extensions: Vec<String>,
    /// Case-sensitive exact file names routed independently of extensions.
    #[serde(default)]
    pub filenames: Vec<String>,
}

fn serialize_toml_compatible_json<S>(
    value: &Option<Value>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut value = value.clone();
    if let Some(value) = &mut value {
        remove_json_nulls(value);
    }
    value.serialize(serializer)
}

fn remove_json_nulls(value: &mut Value) {
    match value {
        Value::Object(values) => {
            values.retain(|_, value| !value.is_null());
            for value in values.values_mut() {
                remove_json_nulls(value);
            }
        }
        Value::Array(values) => {
            values.retain(|value| !value.is_null());
            for value in values {
                remove_json_nulls(value);
            }
        }
        _ => {}
    }
}

impl LanguageServerConfig {
    /// Returns normalized selectors, adapting the legacy single-selector fields.
    pub fn documents(&self) -> Vec<LanguageDocumentConfig> {
        if !self.documents.is_empty() {
            return self.documents.clone();
        }

        if self.language_id.is_empty()
            || (self.file_extensions.is_empty() && self.filenames.is_empty())
        {
            return Vec::new();
        }

        vec![LanguageDocumentConfig {
            language_id: self.language_id.clone(),
            file_extensions: self.file_extensions.clone(),
            filenames: self.filenames.clone(),
        }]
    }
}

impl Config {
    /// Materializes language-local comment templates and LSP document selectors.
    pub fn apply_language_definitions(
        &mut self,
        explicit_servers: &HashSet<String>,
        explicit_comment_languages: &HashSet<String>,
    ) -> anyhow::Result<()> {
        let mut languages = self.languages.iter().collect::<Vec<_>>();
        languages.sort_unstable_by_key(|(id, _)| *id);
        for (id, language) in languages {
            if let Some(comment) = &language.comment {
                anyhow::ensure!(
                    comment.matches("%s").count() == 1,
                    "language `{id}` comment must contain exactly one `%s` placeholder"
                );
                if !explicit_comment_languages.contains(id) {
                    self.commenting
                        .languages
                        .insert(id.clone(), comment.clone());
                }
            }
            if let Some(width) = language.indent_width {
                anyhow::ensure!(width > 0, "language `{id}` indent_width must be positive");
            }

            let Some(lsp) = &language.lsp else {
                continue;
            };
            let server_name = lsp.server.as_deref().unwrap_or(id);
            anyhow::ensure!(
                lsp.command.is_some() || self.lsp.servers.contains_key(server_name),
                "language `{id}` references unknown language server `{server_name}`"
            );
            if let Some(command) = &lsp.command {
                anyhow::ensure!(
                    !command.trim().is_empty(),
                    "language `{id}` LSP command is empty"
                );
                if !explicit_servers.contains(server_name) {
                    self.lsp.servers.insert(
                        server_name.to_string(),
                        LanguageServerConfig {
                            command: command.clone(),
                            args: lsp.args.clone(),
                            language_id: String::new(),
                            file_extensions: Vec::new(),
                            filenames: Vec::new(),
                            documents: Vec::new(),
                            root_markers: lsp.root_markers.clone(),
                            env: lsp.env.clone(),
                            initialization_options: lsp.initialization_options.clone(),
                            settings: lsp.settings.clone(),
                            workspace_name: lsp.workspace_name.clone(),
                        },
                    );
                }
            }
            let server = self.lsp.servers.get_mut(server_name).ok_or_else(|| {
                anyhow::anyhow!(
                    "language `{id}` language server `{server_name}` was not configured"
                )
            })?;
            if server.documents.is_empty() {
                server.documents = server.documents();
            }
            let selector = LanguageDocumentConfig {
                language_id: id.clone(),
                file_extensions: language.extensions.clone(),
                filenames: language.filenames.clone(),
            };
            if let Some(existing) = server
                .documents
                .iter_mut()
                .find(|document| document.language_id == *id)
            {
                existing.file_extensions.extend(selector.file_extensions);
                existing.file_extensions.sort_unstable();
                existing.file_extensions.dedup();
                existing.filenames.extend(selector.filenames);
                existing.filenames.sort_unstable();
                existing.filenames.dedup();
            } else {
                server.documents.push(selector);
            }
        }
        Ok(())
    }
}

/// Returns Red's embedded language-server definitions.
pub fn default_language_servers() -> HashMap<String, LanguageServerConfig> {
    HashMap::from([
        (
            "rust".to_string(),
            LanguageServerConfig {
                command: "rust-analyzer".to_string(),
                args: vec!["-v".to_string()],
                language_id: "rust".to_string(),
                file_extensions: vec!["rs".to_string()],
                filenames: Vec::new(),
                documents: Vec::new(),
                root_markers: vec!["Cargo.toml".to_string(), ".git".to_string()],
                env: HashMap::new(),
                initialization_options: Some(rust_analyzer_initialization_options()),
                settings: None,
                workspace_name: Some("red".to_string()),
            },
        ),
        (
            "husk".to_string(),
            LanguageServerConfig {
                command: std::env::current_exe()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| "red".to_string()),
                args: vec!["husk".to_string(), "lsp".to_string(), "--stdio".to_string()],
                language_id: String::new(),
                file_extensions: Vec::new(),
                filenames: Vec::new(),
                documents: vec![document("husk", &["hk", "husk"])],
                root_markers: vec!["Husk.toml".to_string(), ".git".to_string()],
                env: HashMap::new(),
                initialization_options: Some(json!({
                    "looseSemanticProfile": "legacyJavaScript",
                    "declarations": [crate::plugin::husk_lsp_declarations()]
                })),
                settings: None,
                workspace_name: Some("husk".to_string()),
            },
        ),
        (
            "fish".to_string(),
            server(
                "fish-lsp",
                &["start"],
                &[document("fish", &["fish"])],
                &["config.fish", ".git"],
            ),
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
        filenames: Vec::new(),
        documents: documents.to_vec(),
        root_markers: root_markers
            .iter()
            .map(|marker| marker.to_string())
            .collect(),
        env: HashMap::new(),
        initialization_options: None,
        settings: None,
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
        filenames: Vec::new(),
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

/// Returns Red's default rust-analyzer initialization options.
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
    /// Returns Red's platform configuration directory.
    ///
    /// `XDG_CONFIG_HOME` takes precedence; otherwise Red uses
    /// `$HOME/.config/red`.
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

    /// Resolves a configuration-relative path.
    pub fn path(p: &str) -> PathBuf {
        Self::config_dir().join(p)
    }

    /// Strictly parses TOML, applies ordered override fragments, and deserializes it.
    ///
    /// Unlike [`Self::load_user_toml`], this constructor does not recover
    /// invalid individual fields.
    pub fn from_toml_with_overrides(contents: &str, overrides: &[String]) -> anyhow::Result<Self> {
        let mut value: toml::Value = toml::from_str(contents)
            .map_err(|err| anyhow::anyhow!("failed to parse config.toml: {err}"))?;
        normalize_format_on_save_alias(&mut value);

        for (index, override_toml) in overrides.iter().enumerate() {
            let mut override_value: toml::Value = toml::from_str(override_toml).map_err(|err| {
                anyhow::anyhow!("failed to parse config override #{}: {err}", index + 1)
            })?;
            normalize_format_on_save_alias(&mut override_value);
            merge_toml_values(&mut value, override_value);
        }

        let mut config: Self = value
            .try_into()
            .map_err(|err| anyhow::anyhow!("failed to deserialize merged config: {err}"))?;
        config.apply_disabled_plugins();
        Ok(config)
    }

    /// Loads recoverable user TOML and returns only its effective configuration.
    pub fn from_user_toml_with_overrides(
        contents: &str,
        overrides: &[String],
    ) -> anyhow::Result<Self> {
        Ok(Self::load_user_toml(contents, Path::new("<user config>"), overrides)?.config)
    }

    /// Loads a user file over embedded defaults with field-level recovery.
    ///
    /// A missing file is equivalent to an empty user layer. Unreadable or
    /// malformed whole files use the restricted fallback profile, while CLI
    /// overrides remain strict.
    pub fn load_user_file(path: &Path, overrides: &[String]) -> anyhow::Result<LoadedConfig> {
        match fs::read_to_string(path) {
            Ok(contents) => Self::load_user_toml(&contents, path, overrides),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Self::load_user_toml("", path, overrides)
            }
            Err(error) => {
                let mut loaded = safe_loaded_config(
                    path,
                    "CFG001",
                    format!("could not read the user configuration: {error}"),
                )?;
                apply_strict_overrides(&mut loaded.config, overrides)?;
                loaded.override_fragments = overrides.to_vec();
                Ok(loaded)
            }
        }
    }

    /// Applies recoverable user TOML and strict ordered CLI overrides.
    ///
    /// Unknown or independently invalid user fields become diagnostics and
    /// retain safe defaults. This function never rewrites `path`.
    pub fn load_user_toml(
        contents: &str,
        path: &Path,
        overrides: &[String],
    ) -> anyhow::Result<LoadedConfig> {
        let mut base_value = embedded_config_value()?;
        let source = ConfigDiagnosticSource::UserFile(path.to_path_buf());
        let mut diagnostics = Vec::new();
        let mut disabled_plugins = HashSet::new();
        let mut disabled_permissions = HashSet::new();
        let mut disabled_servers = HashSet::new();
        let mut disable_agent = false;
        let mut disable_lsp = false;

        if !contents.trim().is_empty() {
            let mut user_value = match toml::from_str::<toml::Value>(contents) {
                Ok(value) => value,
                Err(error) => {
                    let mut loaded = safe_loaded_config(
                        path,
                        "CFG002",
                        "the user configuration contains malformed TOML".to_string(),
                    )?;
                    if let Some(span) = error.span() {
                        let (line, column) = line_column(contents, span.start);
                        loaded.diagnostics[0].span = Some(span);
                        loaded.diagnostics[0].line = Some(line);
                        loaded.diagnostics[0].column = Some(column);
                    }
                    apply_strict_overrides(&mut loaded.config, overrides)?;
                    loaded.override_fragments = overrides.to_vec();
                    return Ok(loaded);
                }
            };

            normalize_format_on_save_alias(&mut user_value);
            let table = user_value.as_table().ok_or_else(|| {
                anyhow::anyhow!("user config must contain a top-level TOML table")
            })?;
            for (key, value) in sorted_table_entries(table) {
                let unit_path = vec![key.to_string()];
                if !known_top_level_field(key) {
                    diagnostics.push(diagnostic_for_path(
                        contents,
                        source.clone(),
                        "CFG101",
                        ConfigDiagnosticSeverity::Warning,
                        &unit_path,
                        "unknown configuration field; it was ignored".to_string(),
                        "no setting was applied".to_string(),
                    ));
                    continue;
                }

                apply_user_value(
                    &mut base_value,
                    value.clone(),
                    &unit_path,
                    contents,
                    &source,
                    &mut diagnostics,
                    &mut disabled_plugins,
                    &mut disabled_permissions,
                    &mut disabled_servers,
                    &mut disable_agent,
                    &mut disable_lsp,
                );
            }
        }

        let mut config = deserialize_config(base_value)?;
        for plugin in disabled_plugins {
            config.plugins.remove(&plugin);
        }
        for plugin in disabled_permissions {
            config.plugin_permissions.remove(&plugin);
        }
        for server in disabled_servers {
            config.lsp.servers.remove(&server);
        }
        if disable_agent {
            config.disable_ai = true;
            config.agent = AgentConfig::default();
            config.plugins.remove("agent");
        }
        if disable_lsp {
            config.lsp.enabled = false;
            config.lsp.servers.clear();
        }
        let config_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let missing_plugins = config.missing_plugins(config_dir);
        for plugin in missing_plugins {
            config.plugins.remove(&plugin);
            diagnostics.push(diagnostic_for_path(
                contents,
                source.clone(),
                "CFG301",
                ConfigDiagnosticSeverity::Error,
                &["plugins".to_string(), plugin],
                "configured plugin could not be found".to_string(),
                "quarantined the affected plugin".to_string(),
            ));
        }
        apply_strict_overrides(&mut config, overrides)?;
        config.apply_disabled_plugins();

        Ok(LoadedConfig {
            config,
            recovery: if diagnostics.is_empty() {
                ConfigRecovery::Clean
            } else {
                ConfigRecovery::Partial
            },
            diagnostics,
            source_path: path.to_path_buf(),
            source_text: contents.to_string(),
            override_fragments: overrides.to_vec(),
        })
    }

    /// Persists the selected theme without rewriting unrelated user settings.
    pub fn persist_theme(theme_name: &str) -> anyhow::Result<()> {
        let config_path = Self::path("config.toml");
        let contents = fs::read_to_string(&config_path).unwrap_or_default();
        fs::write(
            config_path,
            update_theme_config_contents(&contents, theme_name)?,
        )?;
        Ok(())
    }

    /// Persists the status-line table without rewriting unrelated user configuration.
    pub fn persist_statusline(statusline: &StatuslineConfig) -> anyhow::Result<()> {
        let config_path = Self::path("config.toml");
        let contents = match fs::read_to_string(&config_path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error.into()),
        };
        fs::write(
            config_path,
            update_statusline_config_contents(&contents, statusline)?,
        )?;
        Ok(())
    }

    /// Resolves a plugin path or bundled-plugin specifier for runtime loading.
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

    /// Returns enabled configured plugins that cannot be resolved.
    pub fn missing_plugins(&self, config_dir: &Path) -> Vec<String> {
        let mut missing = self
            .plugins
            .iter()
            .filter_map(|(name, configured_path)| {
                let configured = Path::new(configured_path);
                let available = if configured.is_absolute() {
                    configured.is_file()
                } else {
                    assets::resolve_plugin(configured_path, config_dir).is_some()
                };
                (!available).then(|| name.clone())
            })
            .collect::<Vec<_>>();
        missing.sort_unstable();
        missing
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

fn embedded_config_value() -> anyhow::Result<toml::Value> {
    let value: toml::Value = toml::from_str(assets::DEFAULT_CONFIG)
        .map_err(|error| anyhow::anyhow!("failed to parse bundled default_config.toml: {error}"))?;
    deserialize_config(value.clone())
        .map_err(|error| anyhow::anyhow!("invalid bundled default_config.toml: {error}"))?;
    Ok(value)
}

fn deserialize_config(value: toml::Value) -> anyhow::Result<Config> {
    let mut config: Config = value
        .try_into()
        .map_err(|error| anyhow::anyhow!("failed to deserialize merged config: {error}"))?;
    config.apply_disabled_plugins();
    Ok(config)
}

fn safe_loaded_config(path: &Path, code: &str, message: String) -> anyhow::Result<LoadedConfig> {
    let mut config = deserialize_config(embedded_config_value()?)?;
    config.theme = "red.json".to_string();
    config.log_file = None;
    config.plugins.clear();
    config.disabled_plugins.clear();
    config.plugin_permissions.clear();
    config.disable_ai = true;
    config.agent = AgentConfig::default();
    config.lsp.enabled = false;
    config.lsp.servers.clear();
    config.formatting.on_save = false;
    config.languages.clear();
    Ok(LoadedConfig {
        config,
        diagnostics: vec![ConfigDiagnostic {
            severity: ConfigDiagnosticSeverity::Error,
            code: code.to_string(),
            source: ConfigDiagnosticSource::UserFile(path.to_path_buf()),
            span: None,
            line: None,
            column: None,
            path: "<document>".to_string(),
            message,
            fallback: "started with the fail-closed embedded profile".to_string(),
        }],
        recovery: ConfigRecovery::WholeFileFallback,
        source_path: path.to_path_buf(),
        source_text: String::new(),
        override_fragments: Vec::new(),
    })
}

fn known_top_level_field(field: &str) -> bool {
    matches!(
        field,
        "keys"
            | "theme"
            | "cursor"
            | "plugins"
            | "disabled_plugins"
            | "plugin_permissions"
            | "plugin_config"
            | "log_file"
            | "mouse_scroll_lines"
            | "scrolloff"
            | "wrap"
            | "relative_line_numbers"
            | "breakindent"
            | "sidescroll"
            | "sidescrolloff"
            | "splash"
            | "show_whats_new"
            | "fetch_release_notes"
            | "persist_inline_history"
            | "search"
            | "completion"
            | "signature_help"
            | "copilot"
            | "picker"
            | "statusline"
            | "key_hints"
            | "clipboard"
            | "lsp"
            | "formatting"
            | "languages"
            | "commenting"
            | "matchit"
            | "disable_ai"
            | "agent"
            | "diagnostics"
            | "show_diagnostics"
            | "window_borders_ascii"
    )
}

#[allow(clippy::too_many_arguments)]
fn apply_user_value(
    base: &mut toml::Value,
    value: toml::Value,
    path: &[String],
    contents: &str,
    source: &ConfigDiagnosticSource,
    diagnostics: &mut Vec<ConfigDiagnostic>,
    disabled_plugins: &mut HashSet<String>,
    disabled_permissions: &mut HashSet<String>,
    disabled_servers: &mut HashSet<String>,
    disable_agent: &mut bool,
    disable_lsp: &mut bool,
) {
    if !known_schema_path(path) {
        diagnostics.push(diagnostic_for_path(
            contents,
            source.clone(),
            "CFG101",
            ConfigDiagnosticSeverity::Warning,
            path,
            "unknown configuration field; it was ignored".to_string(),
            "no setting was applied".to_string(),
        ));
        return;
    }

    if path.first().is_some_and(|part| part == "keys") {
        apply_keymap_value(base, value, path, contents, source, diagnostics);
        return;
    }

    if path.first().is_some_and(|part| part == "plugin_config") {
        let mut candidate = base.clone();
        merge_at_path(&mut candidate, path, value);
        if deserialize_config(candidate.clone()).is_ok() {
            *base = candidate;
        }
        return;
    }

    let atomic_dynamic_entry = matches!(
        path,
        [first, _]
            if first == "plugins" || first == "plugin_permissions" || first == "languages"
    ) || matches!(path, [first, second, _] if first == "lsp" && second == "servers")
        || matches!(path, [first, second, _] if first == "matchit" && second == "languages");
    let agent_unit = path.first().is_some_and(|part| part == "agent");

    if let toml::Value::Table(table) = &value {
        if !atomic_dynamic_entry && !agent_unit {
            for (key, child) in sorted_table_entries(table) {
                let mut child_path = path.to_vec();
                child_path.push(key.to_string());
                apply_user_value(
                    base,
                    child.clone(),
                    &child_path,
                    contents,
                    source,
                    diagnostics,
                    disabled_plugins,
                    disabled_permissions,
                    disabled_servers,
                    disable_agent,
                    disable_lsp,
                );
            }
            return;
        }
    }

    let mut candidate = base.clone();
    merge_at_path(&mut candidate, path, value);
    match deserialize_config(candidate.clone()) {
        Ok(_) => *base = candidate,
        Err(error) => {
            let fallback = if matches!(path, [first, _] if first == "plugins") {
                disabled_plugins.insert(path[1].clone());
                "disabled the affected plugin"
            } else if matches!(path, [first, _] if first == "plugin_permissions") {
                disabled_permissions.insert(path[1].clone());
                "removed the affected plugin permission"
            } else if matches!(path, [first, second, _] if first == "lsp" && second == "servers") {
                disabled_servers.insert(path[2].clone());
                "disabled the affected language server"
            } else if matches!(path, [first, _] if first == "languages") {
                "ignored the affected language definition"
            } else if path.first().is_some_and(|part| part == "agent") {
                *disable_agent = true;
                "disabled agent support"
            } else if path.first().is_some_and(|part| part == "lsp") {
                *disable_lsp = true;
                "disabled LSP support"
            } else {
                "kept the previous valid value"
            };
            diagnostics.push(diagnostic_for_path(
                contents,
                source.clone(),
                "CFG102",
                ConfigDiagnosticSeverity::Error,
                path,
                sanitize_deserialize_error(&error),
                fallback.to_string(),
            ));
        }
    }
}

fn known_schema_path(path: &[String]) -> bool {
    let parts = path.iter().map(String::as_str).collect::<Vec<_>>();
    match parts.as_slice() {
        [field] => known_top_level_field(field),
        ["keys", ..] | ["plugin_config", ..] => true,
        ["plugins", _] => true,
        ["plugin_permissions", _] | ["plugin_permissions", _, "process"] => true,
        ["agent", field] => matches!(*field, "adapter" | "command" | "args" | "env"),
        ["agent", "env", _] => true,
        ["diagnostics", field] => matches!(*field, "gutter_signs" | "icon_style"),
        ["cursor", field] => matches!(
            *field,
            "normal"
                | "insert"
                | "command"
                | "search"
                | "visual"
                | "visual_line"
                | "visual_block"
                | "waiting"
        ),
        ["search", field] => matches!(
            *field,
            "incsearch" | "hlsearch" | "wrapscan" | "ignorecase" | "smartcase"
        ),
        ["completion", field] => matches!(
            *field,
            "auto_trigger"
                | "min_prefix_length"
                | "debounce_ms"
                | "buffer_words"
                | "max_buffer_words"
        ),
        ["signature_help", field] => {
            matches!(
                *field,
                "auto_trigger" | "debounce_ms" | "show_documentation"
            )
        }
        ["copilot", field] => matches!(
            *field,
            "enabled" | "command" | "args" | "debounce_ms" | "max_file_bytes" | "excluded_patterns"
        ),
        ["picker", "input_position"] => true,
        ["picker", "icons", field] => matches!(*field, "style" | "color"),
        ["statusline", field] => matches!(*field, "left" | "right" | "icons"),
        ["statusline", "icons", field] => matches!(*field, "style" | "color"),
        ["key_hints", field] => matches!(*field, "enabled" | "delay_ms"),
        ["clipboard", field] => {
            matches!(*field, "enabled" | "sync_on_yank" | "sync_on_paste")
        }
        ["lsp", field] => matches!(*field, "enabled" | "format_on_save" | "servers"),
        ["formatting", field] => matches!(*field, "on_save" | "provider"),
        ["lsp", "servers", _] => true,
        ["lsp", "servers", _, field] => matches!(
            *field,
            "command"
                | "args"
                | "language_id"
                | "file_extensions"
                | "filenames"
                | "documents"
                | "root_markers"
                | "env"
                | "initialization_options"
                | "settings"
                | "workspace_name"
        ),
        ["lsp", "servers", _, "env", _]
        | ["lsp", "servers", _, "initialization_options", ..]
        | ["lsp", "servers", _, "settings", ..] => true,
        ["languages", _] => true,
        ["languages", _, field] => matches!(
            *field,
            "extensions"
                | "filenames"
                | "aliases"
                | "comment"
                | "indent_width"
                | "grammar"
                | "lsp"
                | "formatter"
        ),
        ["languages", _, "grammar", field] => matches!(
            *field,
            "builtin"
                | "path"
                | "symbol"
                | "highlights"
                | "textobjects"
                | "indents"
                | "injections"
                | "trusted"
                | "targets"
        ),
        ["languages", _, "grammar", "targets", _] => true,
        ["languages", _, "grammar", "targets", _, field] => {
            matches!(*field, "path" | "url" | "sha256")
        }
        ["languages", _, "lsp", field] => matches!(
            *field,
            "server"
                | "command"
                | "args"
                | "root_markers"
                | "env"
                | "initialization_options"
                | "settings"
                | "workspace_name"
        ),
        ["languages", _, "lsp", "env", _]
        | ["languages", _, "lsp", "initialization_options", ..]
        | ["languages", _, "lsp", "settings", ..] => true,
        ["languages", _, "formatter", field] => {
            matches!(*field, "name" | "command" | "args" | "root_markers" | "env")
        }
        ["languages", _, "formatter", "env", _] => true,
        ["commenting", "languages"] | ["commenting", "languages", _] => true,
        ["matchit", field] => matches!(*field, "enabled" | "pairs" | "languages"),
        ["matchit", "languages", _] | ["matchit", "languages", _, "groups"] => true,
        _ => false,
    }
}

fn apply_keymap_value(
    base: &mut toml::Value,
    value: toml::Value,
    path: &[String],
    contents: &str,
    source: &ConfigDiagnosticSource,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) {
    if path.len() < 3 {
        if let toml::Value::Table(table) = value {
            for (key, child) in sorted_table_entries(&table) {
                let mut child_path = path.to_vec();
                child_path.push(key.to_string());
                apply_keymap_value(
                    base,
                    child.clone(),
                    &child_path,
                    contents,
                    source,
                    diagnostics,
                );
            }
        } else {
            diagnostics.push(diagnostic_for_path(
                contents,
                source.clone(),
                "CFG201",
                ConfigDiagnosticSeverity::Error,
                path,
                "keymap groups must be TOML tables".to_string(),
                "kept the embedded keymap".to_string(),
            ));
        }
        return;
    }

    if let Ok(action) = value.clone().try_into::<KeyAction>() {
        let mut candidate = base.clone();
        let merged = merge_key_action_at_path(&mut candidate, path, value, action);
        if merged && deserialize_config(candidate.clone()).is_ok() {
            *base = candidate;
            return;
        }
    } else if let toml::Value::Table(table) = value {
        for (key, child) in sorted_table_entries(&table) {
            let mut child_path = path.to_vec();
            child_path.push(key.to_string());
            apply_keymap_value(
                base,
                child.clone(),
                &child_path,
                contents,
                source,
                diagnostics,
            );
        }
        return;
    }

    diagnostics.push(diagnostic_for_path(
        contents,
        source.clone(),
        "CFG201",
        ConfigDiagnosticSeverity::Error,
        path,
        "invalid key action".to_string(),
        "kept the previous valid binding".to_string(),
    ));
}

fn merge_key_action_at_path(
    base: &mut toml::Value,
    path: &[String],
    value: toml::Value,
    action: KeyAction,
) -> bool {
    let Some(existing) = value_at_path(base, path).cloned() else {
        merge_at_path(base, path, value);
        return true;
    };
    let existing_action = existing.clone().try_into::<KeyAction>().ok();
    match (existing_action, action) {
        (Some(KeyAction::Nested(_)), KeyAction::Nested(_)) => {
            let mut merged = existing;
            merge_key_action_values(&mut merged, value);
            merge_at_path(base, path, merged);
        }
        _ => merge_at_path(base, path, value),
    }
    true
}

fn merge_key_action_values(base: &mut toml::Value, value: toml::Value) {
    match (base, value) {
        (toml::Value::Table(base), toml::Value::Table(value)) => {
            for (key, child) in value {
                match base.get_mut(&key) {
                    Some(existing) => {
                        let old = existing.clone().try_into::<KeyAction>().ok();
                        let new = child.clone().try_into::<KeyAction>().ok();
                        if matches!(old, Some(KeyAction::Nested(_)))
                            && matches!(new, Some(KeyAction::Nested(_)))
                        {
                            merge_key_action_values(existing, child);
                        } else {
                            *existing = child;
                        }
                    }
                    None => {
                        base.insert(key, child);
                    }
                }
            }
        }
        (base, value) => *base = value,
    }
}

fn merge_at_path(base: &mut toml::Value, path: &[String], value: toml::Value) {
    let mut current = base;
    for part in &path[..path.len().saturating_sub(1)] {
        let table = current
            .as_table_mut()
            .expect("configuration paths always traverse tables");
        current = table
            .entry(part.clone())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    }
    if let Some(last) = path.last() {
        current
            .as_table_mut()
            .expect("configuration parent is always a table")
            .insert(last.clone(), value);
    }
}

fn value_at_path<'a>(value: &'a toml::Value, path: &[String]) -> Option<&'a toml::Value> {
    path.iter()
        .try_fold(value, |current, part| current.as_table()?.get(part))
}

/// Resolves the legacy spelling within one layer before higher-priority layers merge.
/// Keep the original field so invalid legacy values retain their source diagnostics.
fn normalize_format_on_save_alias(value: &mut toml::Value) {
    let Some(on_save) = value
        .get("lsp")
        .and_then(|lsp| lsp.get("format_on_save"))
        .and_then(toml::Value::as_bool)
    else {
        return;
    };
    let Some(table) = value.as_table_mut() else {
        return;
    };
    let formatting = table
        .entry("formatting")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    if let Some(formatting) = formatting.as_table_mut() {
        formatting
            .entry("on_save")
            .or_insert(toml::Value::Boolean(on_save));
    }
}

fn apply_strict_overrides(config: &mut Config, overrides: &[String]) -> anyhow::Result<()> {
    if overrides.is_empty() {
        return Ok(());
    }
    let mut value = toml::Value::try_from(&*config)?;
    let mut allowed_servers = config.lsp.servers.keys().cloned().collect::<HashSet<_>>();
    for (index, override_toml) in overrides.iter().enumerate() {
        let mut override_value: toml::Value = toml::from_str(override_toml)
            .map_err(|_| anyhow::anyhow!("failed to parse config override #{}", index + 1))?;
        if let Some(path) = first_unknown_path(&override_value, &[]) {
            anyhow::bail!(
                "invalid config override #{}: unknown field {}",
                index + 1,
                render_path(&path)
            );
        }
        if let Some(servers) = override_value
            .get("lsp")
            .and_then(|lsp| lsp.get("servers"))
            .and_then(toml::Value::as_table)
        {
            allowed_servers.extend(servers.keys().cloned());
        }
        normalize_format_on_save_alias(&mut override_value);
        merge_config_values(&mut value, override_value, &[]);
        *config = deserialize_config(value.clone()).map_err(|_| {
            anyhow::anyhow!(
                "invalid config override #{}: value does not match the expected configuration type",
                index + 1
            )
        })?;
        config
            .lsp
            .servers
            .retain(|server, _| allowed_servers.contains(server));
        value = toml::Value::try_from(&*config)?;
    }
    Ok(())
}

fn first_unknown_path(value: &toml::Value, path: &[String]) -> Option<Vec<String>> {
    if !path.is_empty() && !known_schema_path(path) {
        return Some(path.to_vec());
    }
    let opaque = matches!(
        path,
        [first, ..] if first == "keys" || first == "plugin_config"
    ) || matches!(path, [first, _] if first == "plugins" || first == "plugin_permissions")
        || matches!(path, [first, second, _] if first == "lsp" && second == "servers")
        || matches!(path, [first, second, _] if first == "matchit" && second == "languages")
        || matches!(path, [first] if first == "agent");
    if opaque {
        return None;
    }
    value.as_table().and_then(|table| {
        sorted_table_entries(table)
            .into_iter()
            .find_map(|(key, child)| {
                let mut child_path = path.to_vec();
                child_path.push(key.to_string());
                first_unknown_path(child, &child_path)
            })
    })
}

fn merge_config_values(base: &mut toml::Value, value: toml::Value, path: &[String]) {
    match (base, value) {
        (toml::Value::Table(base), toml::Value::Table(value)) => {
            for (key, child) in value {
                let mut child_path = path.to_vec();
                child_path.push(key.clone());
                match base.get_mut(&key) {
                    Some(existing) if child_path.first().is_some_and(|part| part == "keys") => {
                        let old = existing.clone().try_into::<KeyAction>().ok();
                        let new = child.clone().try_into::<KeyAction>().ok();
                        if matches!(old, Some(KeyAction::Nested(_)))
                            && matches!(new, Some(KeyAction::Nested(_)))
                        {
                            merge_key_action_values(existing, child);
                        } else if new.is_some() {
                            *existing = child;
                        } else {
                            merge_config_values(existing, child, &child_path);
                        }
                    }
                    Some(existing) => merge_config_values(existing, child, &child_path),
                    None => {
                        base.insert(key, child);
                    }
                }
            }
        }
        (base, value) => *base = value,
    }
}

fn sorted_table_entries(table: &toml::map::Map<String, toml::Value>) -> Vec<(&str, &toml::Value)> {
    let mut entries = table
        .iter()
        .map(|(key, value)| (key.as_str(), value))
        .collect::<Vec<_>>();
    entries.sort_unstable_by_key(|(key, _)| *key);
    entries
}

fn diagnostic_for_path(
    contents: &str,
    source: ConfigDiagnosticSource,
    code: &str,
    severity: ConfigDiagnosticSeverity,
    path: &[String],
    message: String,
    fallback: String,
) -> ConfigDiagnostic {
    let span = find_path_span(contents, path);
    let (line, column) = span
        .as_ref()
        .map(|span| line_column(contents, span.start))
        .unzip();
    ConfigDiagnostic {
        severity,
        code: code.to_string(),
        source,
        span,
        line,
        column,
        path: render_path(path),
        message,
        fallback,
    }
}

fn find_path_span(contents: &str, path: &[String]) -> Option<Range<usize>> {
    let mut table_path = Vec::new();
    let mut offset = 0;
    for line in contents.split_inclusive('\n') {
        let leading = line.len() - line.trim_start().len();
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            let brackets = if trimmed.starts_with("[[") { 2 } else { 1 };
            let closing = if brackets == 2 { "]]" } else { "]" };
            if let Some(inner) = trimmed
                .strip_prefix(&"[".repeat(brackets))
                .and_then(|value| value.strip_suffix(closing))
            {
                let segments = parse_dotted_key(inner);
                table_path = segments
                    .iter()
                    .map(|segment| segment.value.clone())
                    .collect();
                if table_path == path {
                    let segment = segments.last()?;
                    let start = offset + leading + brackets + segment.span.start;
                    return Some(start..start + segment.span.len());
                }
            }
            offset += line.len();
            continue;
        }

        if let Some(equals) = find_unquoted(trimmed, '=') {
            let key = &trimmed[..equals];
            let segments = parse_dotted_key(key.trim());
            let mut assignment_path = table_path.clone();
            assignment_path.extend(segments.iter().map(|segment| segment.value.clone()));
            if assignment_path == path {
                let segment = segments.last()?;
                let key_leading = key.len() - key.trim_start().len();
                let start = offset + leading + key_leading + segment.span.start;
                return Some(start..start + segment.span.len());
            }
            if path.starts_with(&assignment_path) {
                let remaining = &path[assignment_path.len()..];
                if let Some(target) = remaining.last() {
                    let value = &trimmed[equals + 1..];
                    if let Some(relative) = find_toml_key(value, target) {
                        let start = offset + leading + equals + 1 + relative.start;
                        return Some(start..offset + leading + equals + 1 + relative.end);
                    }
                }
            }
        }
        offset += line.len();
    }
    None
}

#[derive(Debug)]
struct SourceKey {
    value: String,
    span: Range<usize>,
}

fn parse_dotted_key(input: &str) -> Vec<SourceKey> {
    let mut keys = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote == Some('"') {
            escaped = true;
            continue;
        }
        if matches!(character, '"' | '\'') {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            }
            continue;
        }
        if character == '.' && quote.is_none() {
            push_source_key(&mut keys, input, start, index);
            start = index + 1;
        }
    }
    push_source_key(&mut keys, input, start, input.len());
    keys
}

fn push_source_key(keys: &mut Vec<SourceKey>, input: &str, start: usize, end: usize) {
    let raw = &input[start..end];
    let leading = raw.len() - raw.trim_start().len();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return;
    }
    let value = if (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
    {
        toml::from_str::<toml::Value>(&format!("key = {trimmed}"))
            .ok()
            .and_then(|value| value.get("key")?.as_str().map(str::to_string))
            .unwrap_or_else(|| trimmed[1..trimmed.len() - 1].to_string())
    } else {
        trimmed.to_string()
    };
    let span_start = start + leading;
    keys.push(SourceKey {
        value,
        span: span_start..span_start + trimmed.len(),
    });
}

fn find_unquoted(input: &str, needle: char) -> Option<usize> {
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in input.char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' && quote == Some('"') {
            escaped = true;
        } else if matches!(character, '"' | '\'') {
            quote = if quote == Some(character) {
                None
            } else if quote.is_none() {
                Some(character)
            } else {
                quote
            };
        } else if character == needle && quote.is_none() {
            return Some(index);
        }
    }
    None
}

fn find_toml_key(input: &str, target: &str) -> Option<Range<usize>> {
    parse_dotted_key(input)
        .into_iter()
        .find(|key| key.value == target)
        .map(|key| key.span)
        .or_else(|| {
            let quoted = format!("\"{}\"", target.replace('"', "\\\""));
            input.find(&quoted).map(|start| start..start + quoted.len())
        })
}

fn line_column(contents: &str, offset: usize) -> (usize, usize) {
    let prefix = &contents[..offset.min(contents.len())];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix.rsplit('\n').next().map(str::len).unwrap_or_default() + 1;
    (line, column)
}

fn render_path(path: &[String]) -> String {
    let mut rendered = String::new();
    for (index, part) in path.iter().enumerate() {
        let dynamic = match path.first().map(String::as_str) {
            Some("keys") => index >= 2,
            Some("plugins" | "plugin_permissions" | "plugin_config") => index >= 1,
            Some("lsp") if path.get(1).is_some_and(|part| part == "servers") => index >= 2,
            Some("commenting") if path.get(1).is_some_and(|part| part == "languages") => index >= 2,
            Some("matchit") if path.get(1).is_some_and(|part| part == "languages") => index >= 2,
            _ => false,
        };
        if index == 0 && is_identifier(part) {
            rendered.push_str(part);
        } else if is_identifier(part) && !dynamic {
            rendered.push('.');
            rendered.push_str(part);
        } else {
            rendered.push_str("[\"");
            rendered.push_str(&part.replace('\\', "\\\\").replace('"', "\\\""));
            rendered.push_str("\"]");
        }
    }
    rendered
}

fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn sanitize_deserialize_error(_error: &anyhow::Error) -> String {
    "value does not match the expected configuration type".to_string()
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

fn update_statusline_config_contents(
    contents: &str,
    statusline: &StatuslineConfig,
) -> anyhow::Result<String> {
    use toml_edit::{Array, DocumentMut, Item, Table, Value as EditValue};

    let mut document = if contents.trim().is_empty() {
        DocumentMut::new()
    } else {
        contents
            .parse::<DocumentMut>()
            .map_err(|error| anyhow::anyhow!("could not update config.toml: {error}"))?
    };
    let section_array = |sections: &[StatuslineSection]| {
        let mut array = Array::new();
        for section in sections {
            array.push(section.as_str());
        }
        array
    };

    let mut table = Table::new();
    table["left"] = Item::Value(EditValue::Array(section_array(&statusline.left)));
    table["right"] = Item::Value(EditValue::Array(section_array(&statusline.right)));
    let mut icons = Table::new();
    icons["style"] = toml_edit::value(match statusline.icons.style {
        PickerIconStyle::Unicode => "unicode",
        PickerIconStyle::NerdFont => "nerd_font",
        PickerIconStyle::Ascii => "ascii",
        PickerIconStyle::None => "none",
    });
    icons["color"] = toml_edit::value(statusline.icons.color);
    table["icons"] = Item::Table(icons);
    document["statusline"] = Item::Table(table);

    Ok(document.to_string())
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

/// Serde default helper for options that are enabled by default.
pub fn default_true() -> bool {
    true
}

/// Serde default helper for options that are disabled by default.
pub fn default_false() -> bool {
    false
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(untagged)]
/// One configured key mapping, including prefixes and repeat counts.
pub enum KeyAction {
    /// Explicitly consume the mapping without an editor action.
    None,
    /// Execute one editor action.
    Single(Action),
    /// Execute actions in order.
    Multiple(Vec<Action>),
    /// Continue resolving a key-prefix map.
    Nested(HashMap<String, KeyAction>),
    /// Execute a mapping a fixed number of times.
    Repeating(u16, Box<KeyAction>),
}

#[derive(Debug, Serialize, Deserialize, Default)]
/// Key mappings grouped by editor mode.
pub struct Keys {
    /// Normal-mode mappings.
    #[serde(default)]
    pub normal: HashMap<String, KeyAction>,
    /// Insert-mode mappings.
    #[serde(default)]
    pub insert: HashMap<String, KeyAction>,
    /// Command-line-mode mappings.
    #[serde(default)]
    pub command: HashMap<String, KeyAction>,
    /// Characterwise visual-mode mappings.
    #[serde(default)]
    pub visual: HashMap<String, KeyAction>,
    /// Linewise visual-mode mappings.
    #[serde(default)]
    pub visual_line: HashMap<String, KeyAction>,
    /// Blockwise visual-mode mappings.
    #[serde(default)]
    pub visual_block: HashMap<String, KeyAction>,
}

#[cfg(test)]
mod test {
    use crate::editor::{Action, Mode, SearchDirection};

    use super::*;

    const LEGACY_CONFIG: &str = include_str!("../tests/fixtures/legacy_config.toml");

    #[test]
    fn legacy_config_recovers_key_actions_and_reports_ignored_settings() {
        let loaded =
            Config::load_user_toml(LEGACY_CONFIG, Path::new("/tmp/config.toml"), &[]).unwrap();

        assert_eq!(
            loaded.config.keys.normal.get("/"),
            Some(&KeyAction::Single(Action::EnterMode(
                crate::editor::Mode::Search
            )))
        );
        let leader = loaded.config.keys.normal.get(" ").unwrap();
        let KeyAction::Nested(leader) = leader else {
            panic!("leader binding must remain a chord");
        };
        assert_eq!(
            leader.get("c"),
            Some(&KeyAction::Single(Action::PluginCommand(
                "codex.open".to_string()
            )))
        );
        assert!(loaded
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "CFG101" && diagnostic.path == "commands"));
        assert!(["buffer_picker", "fidget", "neotree", "codex"]
            .iter()
            .all(|plugin| !loaded.config.plugins.contains_key(*plugin)));
    }

    #[test]
    fn legacy_window_keymap_preserves_focus_and_inherits_edge_movement() {
        let loaded =
            Config::load_user_toml(LEGACY_CONFIG, Path::new("/tmp/config.toml"), &[]).unwrap();
        let Some(KeyAction::Nested(ctrl_w)) = loaded.config.keys.normal.get("Ctrl-w") else {
            panic!("legacy window bindings should remain a keymap prefix");
        };

        for (key, action) in [
            ("h", Action::MoveWindowLeft),
            ("j", Action::MoveWindowDown),
            ("k", Action::MoveWindowUp),
            ("l", Action::MoveWindowRight),
            ("H", Action::MoveWindowToLeft),
            ("J", Action::MoveWindowToBottom),
            ("K", Action::MoveWindowToTop),
            ("L", Action::MoveWindowToRight),
        ] {
            assert_eq!(ctrl_w.get(key), Some(&KeyAction::Single(action)));
        }
    }

    #[test]
    fn independent_invalid_values_do_not_hide_valid_siblings() {
        let loaded = Config::load_user_toml(
            r#"
mouse_scroll_lines = "many"
scrolloff = "near"
wrap = "yes"

[keys.normal]
"j" = "MoveScreenLineDown"
"x" = "NotAnAction"
"#,
            Path::new("/tmp/config.toml"),
            &[],
        )
        .unwrap();

        assert!(loaded.diagnostics.len() >= 4);
        assert_eq!(
            loaded.config.keys.normal.get("j"),
            Some(&KeyAction::Single(Action::MoveScreenLineDown))
        );
        assert_ne!(
            loaded.config.keys.normal.get("x"),
            Some(&KeyAction::Single(Action::MoveScreenLineDown))
        );
    }

    #[test]
    fn relative_line_numbers_are_an_optional_opt_in_setting() {
        let defaults = Config::load_user_toml("", Path::new("/tmp/config.toml"), &[]).unwrap();
        assert_eq!(defaults.config.relative_line_numbers, Some(false));

        let enabled = Config::load_user_toml(
            "relative_line_numbers = true",
            Path::new("/tmp/config.toml"),
            &[],
        )
        .unwrap();
        assert!(enabled.is_clean());
        assert_eq!(enabled.config.relative_line_numbers, Some(true));
    }

    #[test]
    fn invalid_relative_line_numbers_falls_back_independently() {
        let loaded = Config::load_user_toml(
            r#"relative_line_numbers = "yes""#,
            Path::new("/tmp/config.toml"),
            &[],
        )
        .unwrap();

        assert_eq!(loaded.config.relative_line_numbers, Some(false));
        assert!(loaded.diagnostics.iter().any(|diagnostic| {
            diagnostic.path == "relative_line_numbers"
                && diagnostic.fallback == "kept the previous valid value"
        }));
    }

    #[test]
    fn malformed_user_config_uses_fail_closed_profile() {
        let loaded =
            Config::load_user_toml("[keys.normal", Path::new("/tmp/config.toml"), &[]).unwrap();

        assert_eq!(loaded.recovery, ConfigRecovery::WholeFileFallback);
        assert!(loaded.config.disable_ai);
        assert!(loaded.config.plugins.is_empty());
        assert!(loaded.config.plugin_permissions.is_empty());
        assert!(!loaded.config.lsp.enabled);
        assert!(!loaded.config.formatting.on_save);
        assert!(loaded.config.lsp.servers.is_empty());
        assert!(loaded.config.log_file.is_none());
        assert_eq!(loaded.config.theme, "red.json");
    }

    #[test]
    fn unreadable_user_config_uses_fail_closed_profile() {
        let directory = tempfile::tempdir().unwrap();
        let loaded = Config::load_user_file(directory.path(), &[]).unwrap();

        assert_eq!(loaded.recovery, ConfigRecovery::WholeFileFallback);
        assert_eq!(loaded.diagnostics[0].code, "CFG001");
        assert!(loaded.config.disable_ai);
        assert!(!loaded.config.lsp.enabled);
        assert!(!loaded.config.formatting.on_save);
    }

    #[test]
    fn invalid_action_sequence_is_rejected_as_one_unit() {
        let loaded = Config::load_user_toml(
            r#"
[keys.normal]
"q" = [ "MoveDown", "NotAnAction", "MoveUp" ]
"#,
            Path::new("/tmp/config.toml"),
            &[],
        )
        .unwrap();

        assert_eq!(
            loaded.config.keys.normal.get("q"),
            Config::load_user_toml("", Path::new("/tmp/config.toml"), &[])
                .unwrap()
                .config
                .keys
                .normal
                .get("q")
        );
        assert_eq!(
            loaded
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.path == r#"keys.normal["q"]"#)
                .count(),
            1
        );
    }

    #[test]
    fn invalid_capability_entries_fail_closed() {
        let loaded = Config::load_user_toml(
            r#"
[plugin_permissions.project_search]
process = "rg"

[lsp.servers.rust]
command = ["rust-analyzer"]
"#,
            Path::new("/tmp/config.toml"),
            &[],
        )
        .unwrap();

        assert!(!loaded
            .config
            .plugin_permissions
            .contains_key("project_search"));
        assert!(!loaded.config.lsp.servers.contains_key("rust"));
    }

    #[test]
    fn diagnostics_never_include_rejected_values() {
        let secret = "credential-value-that-must-not-appear";
        let loaded = Config::load_user_toml(
            &format!(
                r#"
[agent]
args = "{secret}"
"#
            ),
            Path::new("/tmp/config.toml"),
            &[],
        )
        .unwrap();

        assert!(loaded
            .diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.format().contains(secret)));

        let malformed = Config::load_user_toml(
            &format!("agent = \"{secret}"),
            Path::new("/tmp/config.toml"),
            &[],
        )
        .unwrap();
        assert!(!malformed.diagnostics[0].format().contains(secret));
    }

    #[test]
    fn loading_never_rewrites_user_config() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let contents = "[commands]\nwrite = \"Save\"\n";
        fs::write(&path, contents).unwrap();

        Config::load_user_file(&path, &[]).unwrap();

        assert_eq!(fs::read_to_string(path).unwrap(), contents);
    }

    #[test]
    fn strict_override_does_not_restore_quarantined_default_servers() {
        let loaded = Config::load_user_toml(
            r#"
[lsp.servers.rust]
command = ["rust-analyzer"]
"#,
            Path::new("/tmp/config.toml"),
            &[r#"theme = "mocha.json""#.to_string()],
        )
        .unwrap();

        assert!(!loaded.config.lsp.servers.contains_key("rust"));
    }

    #[test]
    fn strict_override_identifies_its_index() {
        let error = Config::load_user_toml(
            "",
            Path::new("/tmp/config.toml"),
            &[
                r#"theme = "mocha.json""#.to_string(),
                "commands.foo = 1".to_string(),
            ],
        )
        .unwrap_err();

        assert!(error.to_string().contains("override #2"));
    }

    #[test]
    fn diagnostic_paths_quote_dynamic_keys() {
        assert_eq!(
            render_path(&["keys".to_string(), "normal".to_string(), "/".to_string()]),
            r#"keys.normal["/"]"#
        );
        assert_eq!(
            render_path(&[
                "lsp".to_string(),
                "servers".to_string(),
                "foo.bar".to_string()
            ]),
            r#"lsp.servers["foo.bar"]"#
        );
    }

    #[test]
    fn diagnostic_spans_use_the_full_table_path() {
        let contents = r#"
[keys.normal." "]
"c" = "NotAnAction"

[keys.normal."d"]
"c" = "DumpCapabilities"
"#;
        let loaded = Config::load_user_toml(contents, Path::new("/tmp/config.toml"), &[]).unwrap();
        let diagnostic = loaded
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.path == r#"keys.normal[" "]["c"]"#)
            .unwrap();

        assert_eq!(diagnostic.line, Some(3));
        assert_eq!(&contents[diagnostic.span.clone().unwrap()], r#""c""#);
    }

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
    fn update_statusline_config_preserves_unrelated_settings_and_comments() {
        let contents = r#"# keep this comment
theme = "mocha.json"

[statusline]
left = ["filename"]
right = ["position"]

[statusline.icons]
style = "ascii"
color = false

[keys.normal]
"x" = "DeleteChar"
"#;
        let statusline = StatuslineConfig {
            left: vec![StatuslineSection::Mode, StatuslineSection::Filename],
            right: vec![StatuslineSection::Syntax, StatuslineSection::Position],
            icons: PickerIconsConfig {
                style: PickerIconStyle::Unicode,
                color: true,
            },
        };

        let updated = update_statusline_config_contents(contents, &statusline).unwrap();
        let value = updated.parse::<toml::Value>().unwrap();

        assert!(updated.contains("# keep this comment"));
        assert!(updated.contains("[keys.normal]"));
        assert!(updated.contains(r#""x" = "DeleteChar""#));
        assert_eq!(
            value["statusline"]["left"].as_array().unwrap(),
            &vec!["mode".into(), "filename".into()]
        );
        assert_eq!(
            value["statusline"]["icons"]["style"].as_str(),
            Some("unicode")
        );
        assert_eq!(value["statusline"]["icons"]["color"].as_bool(), Some(true));
        assert_eq!(updated.matches("[statusline]").count(), 1);
    }

    #[test]
    fn update_statusline_config_refuses_to_overwrite_malformed_toml() {
        let error =
            update_statusline_config_contents("[statusline\n", &StatuslineConfig::default())
                .unwrap_err();

        assert!(error.to_string().contains("could not update config.toml"));
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
        let fish = config.lsp.servers.get("fish").unwrap();
        assert_eq!(fish.command, "fish-lsp");
        assert_eq!(fish.args, vec!["start"]);
        assert_eq!(fish.documents(), vec![document("fish", &["fish"])]);
        assert_eq!(fish.root_markers, vec!["config.fish", ".git"]);
        let husk = config.lsp.servers.get("husk").unwrap();
        assert_eq!(
            husk.command,
            std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        );
        assert_eq!(husk.args, vec!["husk", "lsp", "--stdio"]);
        assert_eq!(husk.documents(), vec![document("husk", &["hk", "husk"])]);
        assert_eq!(husk.root_markers, vec!["Husk.toml", ".git"]);
        assert_eq!(
            husk.initialization_options
                .as_ref()
                .and_then(|options| options.get("looseSemanticProfile")),
            Some(&json!("legacyJavaScript"))
        );
        assert!(husk
            .initialization_options
            .as_ref()
            .and_then(|options| options.get("declarations"))
            .and_then(Value::as_array)
            .is_some_and(
                |declarations| declarations.iter().any(|declaration| declaration
                    .as_str()
                    .is_some_and(|source| source.contains("mod global red")))
            ));
        assert!(config.lsp.servers.contains_key("markdown"));
        assert!(!config.lsp.servers.contains_key("python"));
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
    fn default_config_maps_shift_d_to_line_diagnostics() {
        let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();

        assert_eq!(
            config.keys.normal.get("D"),
            Some(&KeyAction::Single(Action::ShowLineDiagnostics))
        );
    }

    #[test]
    fn default_config_maps_diagnostic_navigation() {
        let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();

        let Some(KeyAction::Nested(previous)) = config.keys.normal.get("[") else {
            panic!("expected [ prefix");
        };
        assert_eq!(
            previous.get("d"),
            Some(&KeyAction::Single(Action::PreviousDiagnostic))
        );

        let Some(KeyAction::Nested(next)) = config.keys.normal.get("]") else {
            panic!("expected ] prefix");
        };
        assert_eq!(
            next.get("d"),
            Some(&KeyAction::Single(Action::NextDiagnostic))
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
        assert_eq!(config.picker.icons.style, PickerIconStyle::NerdFont);
        assert!(config.picker.icons.color);
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
    fn diagnostic_gutter_signs_default_to_nerd_font_icons() {
        let config: Config = toml::from_str(
            r#"
theme = "mocha.json"

[keys]
"#,
        )
        .unwrap();

        assert!(config.diagnostics.gutter_signs);
        assert_eq!(config.diagnostics.icon_style, PickerIconStyle::NerdFont);
    }

    #[test]
    fn diagnostic_gutter_sign_configuration_parses_all_icon_styles() {
        for (value, expected) in [
            ("unicode", PickerIconStyle::Unicode),
            ("nerd_font", PickerIconStyle::NerdFont),
            ("ascii", PickerIconStyle::Ascii),
            ("none", PickerIconStyle::None),
        ] {
            let config = Config::from_user_toml_with_overrides(
                &format!(
                    r#"
[diagnostics]
gutter_signs = false
icon_style = "{value}"
"#
                ),
                &[],
            )
            .unwrap();

            assert!(!config.diagnostics.gutter_signs);
            assert_eq!(config.diagnostics.icon_style, expected);
        }
    }

    #[test]
    fn statusline_defaults_match_the_bundled_neovim_inspired_layout() {
        let config = Config::from_user_toml_with_overrides("", &[]).unwrap();

        assert_eq!(
            config.statusline.left,
            [
                StatuslineSection::Mode,
                StatuslineSection::Diagnostics,
                StatuslineSection::GitBranch,
                StatuslineSection::Filename,
            ]
        );
        assert_eq!(
            config.statusline.right,
            [StatuslineSection::Position, StatuslineSection::Syntax]
        );
        assert_eq!(config.statusline.icons.style, PickerIconStyle::NerdFont);
        assert!(config.statusline.icons.color);
    }

    #[test]
    fn statusline_sections_can_move_between_sides_and_change_icon_style() {
        let config = Config::from_user_toml_with_overrides(
            r#"
[statusline]
left = ["syntax", "filename"]
right = ["mode", "git_branch", "position"]

[statusline.icons]
style = "ascii"
color = false
"#,
            &[],
        )
        .unwrap();

        assert_eq!(
            config.statusline.left,
            [StatuslineSection::Syntax, StatuslineSection::Filename]
        );
        assert_eq!(
            config.statusline.right,
            [
                StatuslineSection::Mode,
                StatuslineSection::GitBranch,
                StatuslineSection::Position,
            ]
        );
        assert_eq!(config.statusline.icons.style, PickerIconStyle::Ascii);
        assert!(!config.statusline.icons.color);
    }

    #[test]
    fn every_statusline_section_round_trips_through_toml() {
        let statusline = StatuslineConfig {
            left: StatuslineSection::ALL.to_vec(),
            right: Vec::new(),
            icons: PickerIconsConfig::default(),
        };

        let serialized = toml::to_string(&statusline).unwrap();
        let parsed = toml::from_str::<StatuslineConfig>(&serialized).unwrap();

        assert_eq!(parsed.left, StatuslineSection::ALL);
        assert!(parsed.right.is_empty());
    }

    #[test]
    fn invalid_statusline_section_keeps_the_default_side() {
        let loaded = Config::load_user_toml(
            r#"
[statusline]
left = ["mode", "weather"]
"#,
            Path::new("/tmp/config.toml"),
            &[],
        )
        .unwrap();

        assert_eq!(loaded.config.statusline.left, default_statusline_left());
        assert!(loaded
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "statusline.left"));
    }

    #[test]
    fn picker_icon_config_parses_all_styles_and_defaults_to_color() {
        for (value, expected) in [
            ("unicode", PickerIconStyle::Unicode),
            ("nerd_font", PickerIconStyle::NerdFont),
            ("ascii", PickerIconStyle::Ascii),
            ("none", PickerIconStyle::None),
        ] {
            let config: Config = toml::from_str(&format!(
                r#"
theme = "mocha.json"

[picker.icons]
style = "{value}"

[keys]
"#
            ))
            .unwrap();

            assert_eq!(config.picker.icons.style, expected);
            assert!(config.picker.icons.color);
        }
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
    fn default_config_maps_vim_word_character_and_screen_motions() {
        let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();

        assert_eq!(
            config.keys.normal.get("W"),
            Some(&KeyAction::Single(Action::MoveToNextBigWord))
        );
        assert_eq!(
            config.keys.normal.get(";"),
            Some(&KeyAction::Single(Action::RepeatCharSearch(1)))
        );
        assert_eq!(
            config.keys.normal.get("H"),
            Some(&KeyAction::Single(Action::MoveToViewportTop(1)))
        );
        assert_eq!(
            config.keys.normal.get("M"),
            Some(&KeyAction::Single(Action::MoveToViewportMiddle))
        );
        assert_eq!(
            config.keys.normal.get("L"),
            Some(&KeyAction::Single(Action::MoveToViewportBottom(1)))
        );
    }

    #[test]
    fn default_config_preserves_wrap_toggle_under_gw() {
        let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
        let Some(KeyAction::Nested(g)) = config.keys.normal.get("g") else {
            panic!("default config should map g to nested actions");
        };

        assert_eq!(g.get("W"), Some(&KeyAction::Single(Action::ToggleWrap)));
    }

    #[test]
    fn default_config_maps_last_visual_restore_and_visual_indent() {
        let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();
        let Some(KeyAction::Nested(normal_g)) = config.keys.normal.get("g") else {
            panic!("default config should map normal g to nested actions");
        };
        let Some(KeyAction::Nested(visual_g)) = config.keys.visual.get("g") else {
            panic!("default config should map visual g to nested actions");
        };

        let restore = Some(&KeyAction::Single(Action::RestoreLastVisualSelection));
        assert_eq!(normal_g.get("v"), restore);
        assert_eq!(visual_g.get("v"), restore);
        assert_eq!(
            config.keys.visual.get(":"),
            Some(&KeyAction::Single(Action::EnterMode(Mode::Command)))
        );
        assert_eq!(
            config.keys.visual.get(">"),
            Some(&KeyAction::Single(Action::IndentSelection(1)))
        );
        assert_eq!(
            config.keys.visual.get("<"),
            Some(&KeyAction::Single(Action::UnindentSelection(1)))
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
    fn default_config_maps_normal_and_visual_comment_operators() {
        let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();

        let Some(KeyAction::Nested(normal_g)) = config.keys.normal.get("g") else {
            panic!("default config should map normal g to nested actions");
        };
        assert_eq!(
            normal_g.get("c"),
            Some(&KeyAction::Single(Action::StartCommentOperator(1)))
        );

        let Some(KeyAction::Nested(visual_g)) = config.keys.visual.get("g") else {
            panic!("default config should map visual g to nested actions");
        };
        assert_eq!(
            visual_g.get("c"),
            Some(&KeyAction::Multiple(vec![
                Action::ToggleCommentSelection,
                Action::EnterMode(Mode::Normal),
            ]))
        );
    }

    #[test]
    fn comment_configuration_defaults_cover_line_and_wrapping_comments() {
        let config = Config::default();

        assert_eq!(config.commenting.languages["fish"], "# %s");
        assert_eq!(config.commenting.languages["rust"], "// %s");
        assert!(!config.commenting.languages.contains_key("python"));
        assert_eq!(config.commenting.languages["lua"], "-- %s");
        assert_eq!(config.commenting.languages["html"], "<!-- %s -->");
        assert_eq!(config.commenting.languages["css"], "/* %s */");
        assert!(!config.commenting.languages.contains_key("json"));
    }

    #[test]
    fn user_comment_templates_merge_without_discarding_language_defaults() {
        let loaded = Config::load_user_toml(
            "[commenting.languages]\nrust = \"/* %s */\"\ncustom = \"; %s\"\n",
            Path::new("/tmp/config.toml"),
            &[],
        )
        .unwrap();

        assert_eq!(loaded.config.commenting.languages["rust"], "/* %s */");
        assert_eq!(loaded.config.commenting.languages["custom"], "; %s");
        assert!(!loaded.config.commenting.languages.contains_key("python"));
    }

    #[test]
    fn invalid_comment_template_type_recovers_independently() {
        let loaded = Config::load_user_toml(
            "[commenting.languages]\nrust = 42\npython = \"## %s\"\n",
            Path::new("/tmp/config.toml"),
            &[],
        )
        .unwrap();

        assert_eq!(loaded.config.commenting.languages["rust"], "// %s");
        assert_eq!(loaded.config.commenting.languages["python"], "## %s");
        assert!(loaded.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "CFG102" && diagnostic.path == r#"commenting.languages["rust"]"#
        }));
    }

    #[test]
    fn strict_overrides_accept_comment_template_paths() {
        let loaded = Config::load_user_toml(
            "",
            Path::new("/tmp/config.toml"),
            &["commenting.languages.rust = \"/* %s */\"".to_string()],
        )
        .unwrap();

        assert_eq!(loaded.config.commenting.languages["rust"], "/* %s */");
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

        for (key, action) in [
            ("h", Action::MoveWindowLeft),
            ("j", Action::MoveWindowDown),
            ("k", Action::MoveWindowUp),
            ("l", Action::MoveWindowRight),
            ("H", Action::MoveWindowToLeft),
            ("J", Action::MoveWindowToBottom),
            ("K", Action::MoveWindowToTop),
            ("L", Action::MoveWindowToRight),
        ] {
            assert_eq!(ctrl_w.get(key), Some(&KeyAction::Single(action)));
        }

        assert_eq!(
            ctrl_w.get("s"),
            Some(&KeyAction::Single(Action::SplitHorizontal))
        );
        for key in ["v", "d"] {
            assert_eq!(
                ctrl_w.get(key),
                Some(&KeyAction::Single(Action::SplitVertical))
            );
        }
        assert_eq!(
            ctrl_w.get("r"),
            Some(&KeyAction::Single(Action::EnterPaneResizeMode))
        );
        for (key, action) in [
            ("+", Action::ResizeWindowDown(1)),
            ("-", Action::ResizeWindowUp(1)),
            ("<", Action::ResizeWindowLeft(1)),
            (">", Action::ResizeWindowRight(1)),
        ] {
            assert_eq!(ctrl_w.get(key), Some(&KeyAction::Single(action)));
        }
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
            ctrl_w.get("z"),
            Some(&KeyAction::Single(Action::TogglePaneZoom))
        );
        assert_eq!(
            ctrl_w.get("o"),
            Some(&KeyAction::Single(Action::OnlyWindow))
        );
    }

    #[test]
    fn default_config_maps_lsp_navigation_and_actions() {
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
            leader.get("d"),
            Some(&KeyAction::Single(Action::OpenDiagnosticsPicker))
        );
        assert_eq!(
            leader.get("e"),
            Some(&KeyAction::Single(Action::OpenErrorDiagnosticsPicker))
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

        assert_eq!(config.show_whats_new, Some(true));
        assert_eq!(config.fetch_release_notes, Some(true));
        assert_eq!(config.persist_inline_history, Some(true));
        assert!(known_top_level_field("persist_inline_history"));
        assert_eq!(
            config.keys.normal.get("F1"),
            Some(&KeyAction::Single(Action::KeyboardShortcuts))
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
        assert_eq!(
            leader.get("s"),
            Some(&KeyAction::Single(Action::OpenStatuslineManager))
        );
        assert_eq!(
            leader.get("P"),
            Some(&KeyAction::Single(Action::ListPlugins))
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
    fn user_config_controls_completion_sources_and_triggering() {
        let config = Config::from_user_toml_with_overrides(
            r#"
[completion]
auto_trigger = false
min_prefix_length = 3
debounce_ms = 250
buffer_words = false
max_buffer_words = 20
"#,
            &[],
        )
        .unwrap();

        assert!(!config.completion.auto_trigger);
        assert_eq!(config.completion.min_prefix_length, 3);
        assert_eq!(config.completion.debounce_ms, 250);
        assert!(!config.completion.buffer_words);
        assert_eq!(config.completion.max_buffer_words, 20);
    }

    #[test]
    fn signature_help_configuration_accepts_user_settings_and_overrides() {
        let defaults = Config::from_user_toml_with_overrides("", &[]).unwrap();
        assert_eq!(defaults.signature_help, SignatureHelpConfig::default());
        let config = Config::from_user_toml_with_overrides(
            "[signature_help]\nauto_trigger = false\ndebounce_ms = 350\nshow_documentation = false\n",
            &["signature_help.debounce_ms = 75".to_owned()],
        ).unwrap();
        assert!(!config.signature_help.auto_trigger);
        assert!(!config.signature_help.show_documentation);
        assert_eq!(config.signature_help.debounce_ms, 75);
        assert!(known_top_level_field("signature_help"));
    }

    #[test]
    fn copilot_configuration_is_opt_in_and_accepts_overrides() {
        let defaults = Config::from_user_toml_with_overrides("", &[]).unwrap();
        assert!(!defaults.copilot.enabled);
        let config = Config::from_user_toml_with_overrides(
            "[copilot]\nenabled = true\ncommand = 'custom-copilot'\ndebounce_ms = 250\n",
            &[],
        )
        .unwrap();
        assert!(config.copilot.enabled);
        assert_eq!(config.copilot.command, "custom-copilot");
        assert_eq!(config.copilot.debounce_ms, 250);
        assert!(known_top_level_field("copilot"));
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
    fn default_config_maps_alt_a_to_agent_toggle() {
        let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();

        assert_eq!(
            config.keys.normal.get("Alt-a"),
            Some(&KeyAction::Single(Action::PluginCommand(
                "AgentToggle".to_string()
            )))
        );
        assert!(!config.keys.insert.contains_key("Alt-a"));
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
        assert_eq!(config.log_file.as_deref(), Some("red.log"));
    }

    #[test]
    fn default_config_enables_session_restore() {
        let config: Config = toml::from_str(include_str!("../default_config.toml")).unwrap();

        assert_eq!(
            config.plugins.get("session_restore").map(String::as_str),
            Some("session_restore.hk")
        );
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
    fn formatting_defaults_to_on_for_missing_fields_and_files() {
        assert!(Config::default().formatting.on_save);
        assert!(toml::from_str::<FormattingConfig>("").unwrap().on_save);
        let config: Config = toml::from_str("theme = \"red.json\"\n[keys]").unwrap();
        assert!(config.formatting.on_save);
        let config: Config = toml::from_str(assets::DEFAULT_CONFIG).unwrap();
        assert!(config.formatting.on_save);
        assert_eq!(config.formatting.provider, FormattingProvider::Auto);

        let directory = tempfile::tempdir().unwrap();
        let loaded = Config::load_user_file(&directory.path().join("missing.toml"), &[]).unwrap();
        assert!(loaded.is_clean());
        assert!(loaded.config.formatting.on_save);
    }

    #[test]
    fn formatting_on_save_resolves_each_config_layer() {
        let cases = [
            ("", vec![], true),
            ("[formatting]\nprovider = \"lsp\"", vec![], true),
            ("[formatting]\non_save = false", vec![], false),
            ("[lsp]\nformat_on_save = false", vec![], false),
            ("[lsp]\nformat_on_save = true", vec![], true),
            (
                "[lsp]\nformat_on_save = true\n[formatting]\non_save = false",
                vec![],
                false,
            ),
            (
                "[lsp]\nformat_on_save = false\n[formatting]\non_save = true",
                vec![],
                true,
            ),
            (
                "[formatting]\non_save = false",
                vec!["lsp.format_on_save = true"],
                true,
            ),
            (
                "[lsp]\nformat_on_save = true",
                vec!["formatting.on_save = false"],
                false,
            ),
            (
                "",
                vec!["lsp.format_on_save = true\nformatting.on_save = false"],
                false,
            ),
            (
                "",
                vec!["lsp.format_on_save = false\nformatting.on_save = true"],
                true,
            ),
            (
                "",
                vec!["formatting.on_save = false", "lsp.format_on_save = true"],
                true,
            ),
            (
                "",
                vec!["formatting.on_save = true", "lsp.format_on_save = false"],
                false,
            ),
            (
                "[lsp]\nformat_on_save = false",
                vec!["formatting.provider = \"external\""],
                false,
            ),
        ];
        for (source, overrides, expected) in cases {
            let overrides = overrides.into_iter().map(str::to_owned).collect::<Vec<_>>();
            let loaded =
                Config::load_user_toml(source, Path::new("/tmp/config.toml"), &overrides).unwrap();
            assert!(loaded.is_clean(), "{source:?}: {:?}", loaded.diagnostics);
            assert_eq!(loaded.config.formatting.on_save, expected, "{source:?}");

            let source = format!("theme = \"red.json\"\n[keys]\n{source}");
            let strict = Config::from_toml_with_overrides(&source, &overrides).unwrap();
            assert_eq!(strict.formatting.on_save, expected, "{source:?}");
            let roundtrip: Config = toml::from_str(&toml::to_string(&strict).unwrap()).unwrap();
            assert_eq!(roundtrip.formatting.on_save, expected, "{source:?}");
        }
    }

    #[test]
    fn invalid_formatting_settings_keep_their_original_diagnostics() {
        for (source, expected_path) in [
            ("[formatting]\non_save = \"no\"", "formatting.on_save"),
            ("[lsp]\nformat_on_save = \"no\"", "lsp.format_on_save"),
            (
                "formatting = false\n[lsp]\nformat_on_save = false",
                "formatting",
            ),
        ] {
            let loaded =
                Config::load_user_toml(source, Path::new("/tmp/config.toml"), &[]).unwrap();
            assert_eq!(loaded.diagnostics.len(), 1, "{source:?}");
            assert_eq!(loaded.diagnostics[0].path, expected_path);
            assert!(loaded.diagnostics[0].line.is_some());
            assert!(Config::load_user_toml(
                "",
                Path::new("/tmp/config.toml"),
                &[source.to_string()],
            )
            .is_err());
        }
    }

    #[test]
    fn formatting_config_accepts_external_language_formatter() {
        let config: Config = toml::from_str(
            r#"
theme = "theme/nightfox.json"

[keys]

[formatting]
on_save = true
provider = "external"

[languages.python]
extensions = ["py"]

[languages.python.formatter]
name = "Black"
command = "black"
args = ["--quiet", "--stdin-filename", "{file}", "-"]
root_markers = ["pyproject.toml", ".git"]

[languages.python.formatter.env]
BLACK_CACHE_DIR = "{workspace}/.cache/black"
"#,
        )
        .unwrap();

        assert!(config.formatting.on_save);
        assert_eq!(config.formatting.provider, FormattingProvider::External);
        let formatter = config.languages["python"].formatter.as_ref().unwrap();
        assert_eq!(formatter.name, "Black");
        assert_eq!(formatter.command, "black");
        assert_eq!(
            formatter.args,
            ["--quiet", "--stdin-filename", "{file}", "-"]
        );
        assert_eq!(formatter.root_markers, ["pyproject.toml", ".git"]);
        assert_eq!(formatter.env["BLACK_CACHE_DIR"], "{workspace}/.cache/black");
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
                    filenames: Vec::new(),
                },
                LanguageDocumentConfig {
                    language_id: "javascript".to_string(),
                    file_extensions: vec!["js".to_string()],
                    filenames: Vec::new(),
                },
            ]
        );
    }

    #[test]
    fn unified_language_configuration_supports_filenames_aliases_grammar_and_lsp_settings() {
        let loaded = Config::load_user_toml(
            r##"
[languages.buildspec]
extensions = ["build"]
filenames = ["Buildfile"]
aliases = ["build-script"]
comment = "# %s"
indent_width = 2

[languages.buildspec.grammar]
builtin = "rust"
textobjects = ["queries/buildspec/textobjects.scm"]

[languages.buildspec.lsp]
command = "build-language-server"
args = ["--stdio"]
root_markers = ["Buildfile"]

[languages.buildspec.lsp.settings.build]
validate = true
"##,
            Path::new("/tmp/config.toml"),
            &[],
        )
        .unwrap();
        let definition = loaded.config.languages.get("buildspec").unwrap();

        assert_eq!(definition.extensions, ["build"]);
        assert_eq!(definition.filenames, ["Buildfile"]);
        assert_eq!(definition.aliases, ["build-script"]);
        assert_eq!(definition.comment.as_deref(), Some("# %s"));
        assert_eq!(definition.indent_width, Some(2));
        assert_eq!(
            definition
                .grammar
                .as_ref()
                .and_then(|grammar| grammar.builtin.as_deref()),
            Some("rust")
        );
        assert_eq!(
            definition.grammar.as_ref().unwrap().textobjects,
            [PathBuf::from("queries/buildspec/textobjects.scm")]
        );
        assert_eq!(
            definition
                .lsp
                .as_ref()
                .and_then(|lsp| lsp.settings.as_ref()),
            Some(&json!({ "build": { "validate": true } }))
        );
    }

    #[test]
    fn invalid_language_definition_does_not_quarantine_other_languages() {
        let loaded = Config::load_user_toml(
            r#"
[languages.valid]
extensions = ["ok"]

[languages.invalid]
indent_width = "not a number"
"#,
            Path::new("/tmp/config.toml"),
            &[],
        )
        .unwrap();

        assert!(loaded.config.languages.contains_key("valid"));
        assert!(!loaded.config.languages.contains_key("invalid"));
        assert!(loaded
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "languages.invalid"));
    }

    #[test]
    fn language_local_lsp_and_commenting_preserve_explicit_legacy_overrides() {
        let mut loaded = Config::load_user_toml(
            r##"
[commenting.languages]
custom = "// %s"

[lsp.servers.custom]
command = "explicit-server"
language_id = "custom"
file_extensions = ["old"]

[languages.custom]
extensions = ["new"]
filenames = ["Customfile"]
comment = "# %s"

[languages.custom.lsp]
command = "generated-server"
"##,
            Path::new("/tmp/config.toml"),
            &[],
        )
        .unwrap();
        let explicit_servers = loaded.explicit_language_server_names();
        let explicit_comments = loaded.explicit_comment_language_names();
        loaded
            .config
            .apply_language_definitions(&explicit_servers, &explicit_comments)
            .unwrap();

        assert_eq!(loaded.config.commenting.languages["custom"], "// %s");
        let server = loaded.config.lsp.servers.get("custom").unwrap();
        assert_eq!(server.command, "explicit-server");
        assert_eq!(server.documents().len(), 1);
        assert_eq!(server.documents()[0].file_extensions, ["new", "old"]);
        assert_eq!(server.documents()[0].filenames, ["Customfile"]);
    }

    #[test]
    fn language_local_lsp_recovers_from_a_quarantined_explicit_server() {
        let mut loaded = Config::load_user_toml(
            r#"
[lsp.servers.custom]
command = ["invalid"]

[languages.custom]
extensions = ["custom"]

[languages.custom.lsp]
command = "generated-server"
"#,
            Path::new("/tmp/config.toml"),
            &[],
        )
        .unwrap();

        assert!(!loaded.config.lsp.servers.contains_key("custom"));
        let explicit_servers = loaded.explicit_language_server_names();
        let explicit_comments = loaded.explicit_comment_language_names();
        assert!(!explicit_servers.contains("custom"));
        loaded
            .config
            .apply_language_definitions(&explicit_servers, &explicit_comments)
            .unwrap();

        assert_eq!(
            loaded.config.lsp.servers["custom"].command,
            "generated-server"
        );
        assert!(loaded
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == r#"lsp.servers["custom"]"#));
    }

    #[test]
    fn language_local_settings_preserve_explicit_command_line_overrides() {
        let mut loaded = Config::load_user_toml(
            r##"
[languages.custom]
extensions = ["new"]
comment = "# %s"

[languages.custom.lsp]
command = "generated-server"
"##,
            Path::new("/tmp/config.toml"),
            &[
                r#"lsp.servers.custom = { command = "override-server", language_id = "custom", file_extensions = ["old"] }"#.to_string(),
                r#"commenting.languages.custom = "// %s""#.to_string(),
            ],
        )
        .unwrap();
        let explicit_servers = loaded.explicit_language_server_names();
        let explicit_comments = loaded.explicit_comment_language_names();
        loaded
            .config
            .apply_language_definitions(&explicit_servers, &explicit_comments)
            .unwrap();

        assert_eq!(loaded.config.commenting.languages["custom"], "// %s");
        let server = loaded.config.lsp.servers.get("custom").unwrap();
        assert_eq!(server.command, "override-server");
        assert_eq!(server.documents()[0].file_extensions, ["new", "old"]);
    }

    #[test]
    fn lsp_initialization_options_and_settings_survive_configuration_round_trips() {
        let config = Config::from_user_toml_with_overrides(
            r#"
[lsp.servers.custom]
command = "custom-lsp"
language_id = "custom"
file_extensions = ["custom"]

[lsp.servers.custom.initialization_options]
mode = "strict"

[lsp.servers.custom.settings.custom]
validation = true
"#,
            &[],
        )
        .unwrap();
        let serialized = toml::to_string(&config).unwrap();
        let round_trip: Config = toml::from_str(&serialized).unwrap();
        let server = round_trip.lsp.servers.get("custom").unwrap();

        assert_eq!(
            server.initialization_options,
            Some(json!({ "mode": "strict" }))
        );
        assert_eq!(
            server.settings,
            Some(json!({ "custom": { "validation": true } }))
        );
    }
}
