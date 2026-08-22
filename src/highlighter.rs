//! Tree-sitter language selection and byte-span syntax highlighting.
//!
//! [`Highlighter`] maps file names to bundled grammars and queries, parses supplied
//! source text, and returns [`StyleInfo`] byte ranges resolved
//! against the current theme. Parsing is scoped to the text supplied by the caller;
//! viewport caching and conversion from slice-relative spans belong to the editor.

use std::{
    collections::HashMap,
    fs,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
    thread::{self, JoinHandle},
};

use anyhow::Context as _;
use husk_lexer::{Keyword, Lexer, TokenKind, Trivia};
use libloading::Library;
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator, Tree};
use tree_sitter_language::LanguageFn;

use crate::{
    config::{LanguageConfig, LanguageGrammarConfig},
    editor::StyleInfo,
    language::GrammarTrustStore,
    theme::{Style, Theme},
    utils::expand_user_path,
};

#[derive(Clone, Copy)]
struct BundledLanguageDefinition {
    id: &'static str,
    extensions: &'static [&'static str],
    filenames: &'static [&'static str],
    language: Option<fn() -> Language>,
    highlight_queries: &'static [&'static str],
    textobject_queries: &'static [&'static str],
    injection_query: Option<&'static str>,
    specialized: Option<SpecializedHighlighter>,
}

#[derive(Clone, Copy)]
enum SpecializedHighlighter {
    Husk,
    GitCommit,
}

#[derive(Clone)]
enum GrammarSource {
    Bundled(fn() -> Language),
    Dynamic(Arc<DynamicGrammar>),
}

struct DynamicGrammar {
    language: Language,
    // The grammar and every parser/query using it must be dropped before its library.
    _library: Arc<Library>,
}

#[derive(Clone)]
struct RuntimeLanguageDefinition {
    id: String,
    extensions: Vec<String>,
    filenames: Vec<String>,
    aliases: Vec<String>,
    grammar: Option<GrammarSource>,
    highlight_queries: Vec<String>,
    textobject_queries: Vec<String>,
    indent_queries: Vec<String>,
    injection_query: Option<String>,
    specialized: Option<SpecializedHighlighter>,
}

/// Immutable language routing and grammar definitions shared by all render surfaces.
#[derive(Clone)]
pub struct LanguageRegistry {
    languages: HashMap<String, RuntimeLanguageDefinition>,
    extensions: HashMap<String, String>,
    filenames: HashMap<String, String>,
    aliases: HashMap<String, String>,
}

impl LanguageRegistry {
    /// Builds Red's complete set of bundled language definitions.
    #[must_use]
    pub fn bundled() -> Self {
        let mut registry = Self {
            languages: HashMap::new(),
            extensions: HashMap::new(),
            filenames: HashMap::new(),
            aliases: HashMap::new(),
        };
        for definition in language_definitions() {
            registry.insert(RuntimeLanguageDefinition {
                id: definition.id.to_string(),
                extensions: definition
                    .extensions
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                filenames: definition
                    .filenames
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                aliases: LANGUAGE_NAMES
                    .iter()
                    .filter(|(_, language)| *language == definition.id)
                    .map(|(alias, _)| (*alias).to_string())
                    .collect(),
                grammar: definition.language.map(GrammarSource::Bundled),
                highlight_queries: definition
                    .highlight_queries
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                textobject_queries: definition
                    .textobject_queries
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                indent_queries: bundled_indent_query(definition.id)
                    .into_iter()
                    .map(ToString::to_string)
                    .collect(),
                injection_query: definition.injection_query.map(ToString::to_string),
                specialized: definition.specialized,
            });
        }
        registry
    }

    /// Validates and prepares a complete language snapshot before exposing it to rendering.
    pub fn from_config(
        configured: &HashMap<String, LanguageConfig>,
        config_dir: &Path,
    ) -> anyhow::Result<Self> {
        let mut registry = Self::bundled();
        let mut languages = configured.iter().collect::<Vec<_>>();
        languages.sort_unstable_by_key(|(language, _)| *language);
        for (id, config) in languages {
            registry.insert_configured(id, config, config_dir)?;
        }
        Ok(registry)
    }

    /// Validates one definition without rebuilding or reloading accepted grammars.
    pub(crate) fn insert_configured(
        &mut self,
        id: &str,
        config: &LanguageConfig,
        config_dir: &Path,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !id.trim().is_empty()
                && id.bytes().all(|byte| byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'_')),
            "invalid language identifier `{id}`"
        );

        let inherited = self.languages.get(id).cloned();
        let mut definition = inherited.unwrap_or_else(|| RuntimeLanguageDefinition {
            id: id.to_string(),
            extensions: Vec::new(),
            filenames: Vec::new(),
            aliases: Vec::new(),
            grammar: None,
            highlight_queries: Vec::new(),
            textobject_queries: Vec::new(),
            indent_queries: Vec::new(),
            injection_query: None,
            specialized: None,
        });
        if !config.extensions.is_empty() {
            definition.extensions = config
                .extensions
                .iter()
                .map(|extension| extension.trim_start_matches('.').to_ascii_lowercase())
                .collect();
        }
        if !config.filenames.is_empty() {
            definition.filenames.clone_from(&config.filenames);
        }
        definition.aliases.extend(config.aliases.iter().cloned());

        if let Some(grammar) = &config.grammar {
            if let Some(builtin) = &grammar.builtin {
                let bundled = self.languages.get(builtin).ok_or_else(|| {
                    anyhow::anyhow!("language `{id}` refers to unknown bundled grammar `{builtin}`")
                })?;
                definition.grammar.clone_from(&bundled.grammar);
                definition
                    .highlight_queries
                    .clone_from(&bundled.highlight_queries);
                definition
                    .textobject_queries
                    .clone_from(&bundled.textobject_queries);
                definition
                    .indent_queries
                    .clone_from(&bundled.indent_queries);
                definition
                    .injection_query
                    .clone_from(&bundled.injection_query);
                definition.specialized = None;
            }
            if let Some(path) = grammar_path(grammar, config_dir)? {
                let trust = GrammarTrustStore::new(config_dir);
                let symbol = grammar
                    .symbol
                    .clone()
                    .unwrap_or_else(|| format!("tree_sitter_{}", id.replace('-', "_")));
                definition.grammar = Some(load_dynamic_grammar(
                    &trust,
                    &path,
                    &symbol,
                    grammar.trusted,
                )?);
                definition.specialized = None;
            }
            if !grammar.highlights.is_empty() {
                definition.highlight_queries = grammar
                    .highlights
                    .iter()
                    .map(|path| read_query(path, config_dir, "highlight"))
                    .collect::<anyhow::Result<_>>()?;
            }
            if !grammar.textobjects.is_empty() {
                definition.textobject_queries = grammar
                    .textobjects
                    .iter()
                    .map(|path| read_query(path, config_dir, "text object"))
                    .collect::<anyhow::Result<_>>()?;
            }
            if !grammar.indents.is_empty() {
                definition.indent_queries = grammar
                    .indents
                    .iter()
                    .map(|path| read_query(path, config_dir, "indentation"))
                    .collect::<anyhow::Result<_>>()?;
            }
            if let Some(path) = &grammar.injections {
                definition.injection_query = Some(read_query(path, config_dir, "injection")?);
            }
        }

        anyhow::ensure!(
            definition.indent_queries.is_empty() || definition.grammar.is_some(),
            "language `{id}` declares indentation queries without a grammar"
        );
        if let Some(source) = &definition.grammar {
            let language = grammar_language(source);
            let mut parser = Parser::new();
            parser.set_language(&language).with_context(|| {
                format!("language `{id}` uses an incompatible Tree-sitter grammar")
            })?;
            if definition.textobject_queries.is_empty() {
                if let Some(fallback) = package_textobject_fallback(id) {
                    // Older language packs have no structural-query declaration. A stale or
                    // incompatible fallback must never quarantine their grammar, highlights,
                    // or optional language server.
                    if Query::new(&language, fallback).is_ok() {
                        definition.textobject_queries.push(fallback.to_string());
                    }
                }
            }
            if !definition.highlight_queries.is_empty() {
                Query::new(&language, &definition.highlight_queries.join("\n"))
                    .with_context(|| format!("language `{id}` has an invalid highlight query"))?;
            }
            if !definition.textobject_queries.is_empty() {
                let query = Query::new(&language, &definition.textobject_queries.join("\n"))
                    .with_context(|| format!("language `{id}` has an invalid text-object query"))?;
                for pattern in 0..query.pattern_count() {
                    if let Some(predicate) = query.general_predicates(pattern).first() {
                        anyhow::bail!(
                            "language `{id}` uses unsupported text-object predicate `#{}`",
                            predicate.operator
                        );
                    }
                }
            }
            if !definition.indent_queries.is_empty() {
                let query = Query::new(&language, &definition.indent_queries.join("\n"))
                    .with_context(|| format!("language `{id}` has an invalid indentation query"))?;
                crate::syntax_indent::validate_query(&query)?;
            }
            if let Some(query) = &definition.injection_query {
                Query::new(&language, query)
                    .with_context(|| format!("language `{id}` has an invalid injection query"))?;
            }
        }

        self.insert(definition);
        Ok(())
    }

    fn insert(&mut self, definition: RuntimeLanguageDefinition) {
        let id = definition.id.clone();
        if let Some(previous) = self.languages.remove(&id) {
            self.extensions
                .retain(|_, language| language != &previous.id);
            self.filenames
                .retain(|_, language| language != &previous.id);
            self.aliases.retain(|_, language| language != &previous.id);
        }
        for extension in &definition.extensions {
            self.extensions.insert(extension.clone(), id.clone());
        }
        for filename in &definition.filenames {
            self.filenames.insert(filename.clone(), id.clone());
        }
        self.aliases.insert(id.clone(), id.clone());
        for alias in &definition.aliases {
            self.aliases.insert(alias.to_ascii_lowercase(), id.clone());
        }
        self.languages.insert(id, definition);
    }

    pub(crate) fn indentation_language(&self, id: &str) -> Option<(Language, String)> {
        let definition = self.languages.get(id)?;
        let source = definition.grammar.as_ref()?;
        (!definition.indent_queries.is_empty()).then(|| {
            (
                grammar_language(source),
                definition.indent_queries.join("\n"),
            )
        })
    }

    /// Returns the grammar and normalized structural queries for one language.
    pub(crate) fn textobject_language(&self, id: &str) -> Option<(Language, String)> {
        let definition = self.languages.get(id)?;
        let source = definition.grammar.as_ref()?;
        (!definition.textobject_queries.is_empty()).then(|| {
            (
                grammar_language(source),
                definition.textobject_queries.join("\n"),
            )
        })
    }
}

fn bundled_indent_query(id: &str) -> Option<&'static str> {
    match id {
        "rust" => Some(include_str!("queries/indents/rust.scm")),
        "javascript" | "jsx" | "typescript" | "tsx" => {
            Some(include_str!("queries/indents/ecma.scm"))
        }
        "json" => Some(include_str!("queries/indents/json.scm")),
        "toml" => Some(include_str!("queries/indents/toml.scm")),
        "powershell" => Some(include_str!("queries/indents/powershell.scm")),
        "bash" => Some(include_str!("queries/indents/bash.scm")),
        "fish" => Some(include_str!("queries/indents/fish.scm")),
        "lua" => Some(include_str!("queries/indents/lua.scm")),
        "yaml" => Some(include_str!("queries/indents/yaml.scm")),

        _ => None,
    }
}

fn package_textobject_fallback(id: &str) -> Option<&'static str> {
    Some(match id {
        "c" => include_str!("queries/textobjects/c.scm"),
        "c-sharp" | "c_sharp" => include_str!("queries/textobjects/c-sharp.scm"),
        "cpp" => include_str!("queries/textobjects/cpp.scm"),
        "css" => include_str!("queries/textobjects/css.scm"),
        "go" => include_str!("queries/textobjects/go.scm"),
        "html" => include_str!("queries/textobjects/html.scm"),
        "java" => include_str!("queries/textobjects/java.scm"),
        "json" => include_str!("queries/textobjects/json.scm"),
        "kotlin" => include_str!("queries/textobjects/kotlin.scm"),
        "php" => include_str!("queries/textobjects/php.scm"),
        "powershell" => include_str!("queries/textobjects/powershell.scm"),
        "python" => include_str!("queries/textobjects/python.scm"),
        "svelte" => include_str!("queries/textobjects/svelte.scm"),
        "swift" => include_str!("queries/textobjects/swift.scm"),
        "vue" => include_str!("queries/textobjects/vue.scm"),
        _ => return None,
    })
}

fn grammar_path(
    grammar: &LanguageGrammarConfig,
    config_dir: &Path,
) -> anyhow::Result<Option<PathBuf>> {
    if let Some(path) = &grammar.path {
        return resolve_language_path(path, config_dir).map(Some);
    }
    let Some(artifact) = grammar.targets.get(crate::language::host_target()) else {
        return Ok(None);
    };
    artifact
        .path
        .as_ref()
        .map(|path| resolve_language_path(path, config_dir))
        .transpose()
}

fn resolve_language_path(path: &Path, config_dir: &Path) -> anyhow::Result<PathBuf> {
    let path = expand_user_path(&path.to_string_lossy())?;
    Ok(if path.is_absolute() {
        path
    } else {
        config_dir.join(path)
    })
}

fn read_query(path: &Path, config_dir: &Path, kind: &str) -> anyhow::Result<String> {
    let path = resolve_language_path(path, config_dir)?;
    fs::read_to_string(&path)
        .with_context(|| format!("failed to read {kind} query {}", path.display()))
}

fn load_dynamic_grammar(
    trust: &GrammarTrustStore,
    path: &Path,
    symbol: &str,
    explicitly_trusted: bool,
) -> anyhow::Result<GrammarSource> {
    let staged = trust.approved_grammar_path(path, explicitly_trusted)?;
    // SAFETY: Native code is opened only after explicit path-and-digest approval.
    // The immutable staged file prevents source replacement after that decision.
    let library =
        Arc::new(unsafe { Library::new(&staged) }.with_context(|| {
            format!("failed to open trusted native grammar {}", staged.display())
        })?);
    // SAFETY: Tree-sitter grammar exports have this documented C ABI. The symbol is
    // held only long enough to construct a Language; the Arc keeps its library loaded.
    let function = unsafe {
        *library
            .get::<unsafe extern "C" fn() -> *const ()>(symbol.as_bytes())
            .with_context(|| format!("native grammar does not export `{symbol}`"))?
    };
    // SAFETY: The approved symbol is a generated Tree-sitter grammar function.
    let language = Language::new(unsafe { LanguageFn::from_raw(function) });
    Ok(GrammarSource::Dynamic(Arc::new(DynamicGrammar {
        language,
        _library: library,
    })))
}

fn grammar_language(source: &GrammarSource) -> Language {
    match source {
        GrammarSource::Bundled(language) => language(),
        GrammarSource::Dynamic(grammar) => grammar.language.clone(),
    }
}

struct LanguageHighlighter {
    parser: Parser,
    query: Query,
    injection_query: Option<Query>,
    capture_styles: Vec<Option<Style>>,
    cached_tree: Option<CachedSyntaxTree>,
}

struct CachedSyntaxTree {
    source: String,
    tree: Tree,
}

struct CompiledLanguageQueries {
    language: Language,
    query: Query,
    injection_query: Option<Query>,
}

pub(crate) struct PendingLanguageHighlighter {
    language_id: String,
    registry: Arc<LanguageRegistry>,
    task: JoinHandle<anyhow::Result<CompiledLanguageQueries>>,
}

struct Injection {
    language_id: String,
    content_start: usize,
    content_end: usize,
}

struct RawInjection {
    language_name: String,
    content_start: usize,
    content_end: usize,
}

struct CachedHighlight {
    language_id: String,
    code: String,
    styles: Vec<StyleInfo>,
}

pub struct Highlighter {
    highlighters: HashMap<String, LanguageHighlighter>,
    cached_highlight: Option<CachedHighlight>,
    registry: Arc<LanguageRegistry>,
    theme: Theme,
    husk_styles: HuskStyles,
    git_commit_styles: GitCommitStyles,
}

struct HuskStyles {
    comment: Option<Style>,
    constant_builtin: Option<Style>,
    variable_builtin: Option<Style>,
    keyword: Option<Style>,
    numeric: Option<Style>,
    string: Option<Style>,
    type_builtin: Option<Style>,
    operator: Option<Style>,
}

impl HuskStyles {
    fn new(theme: &Theme) -> Self {
        Self {
            comment: theme.get_style("comment"),
            constant_builtin: theme.get_style("constant.builtin"),
            variable_builtin: theme.get_style("variable.builtin"),
            keyword: theme.get_style("keyword"),
            numeric: theme.get_style("constant.numeric"),
            string: theme.get_style("string"),
            type_builtin: theme.get_style("type.builtin"),
            operator: theme.get_style("operator"),
        }
    }
}

struct GitCommitStyles {
    comment: Option<Style>,
    heading: Option<Style>,
    reference: Option<Style>,
    status: Option<Style>,
    inserted: Option<Style>,
    deleted: Option<Style>,
    changed: Option<Style>,
}

impl GitCommitStyles {
    fn new(theme: &Theme) -> Self {
        Self {
            comment: theme.get_style("comment"),
            heading: theme
                .get_style("markup.heading")
                .or_else(|| theme.get_style("keyword")),
            reference: theme.get_style("string"),
            status: theme.get_style("keyword"),
            inserted: theme.get_style("markup.inserted.diff"),
            deleted: theme.get_style("markup.deleted.diff"),
            changed: theme
                .get_style("markup.changed.diff")
                .or_else(|| theme.get_style("keyword")),
        }
    }
}

const MAX_INJECTION_DEPTH: usize = 3;
const MAX_CACHED_HIGHLIGHT_BYTES: usize = 64 * 1024;
const MAX_CACHED_HIGHLIGHT_SPANS: usize = 4_096;

const LANGUAGE_NAMES: &[(&str, &str)] = &[
    ("rs", "rust"),
    ("rust", "rust"),
    ("js", "javascript"),
    ("javascript", "javascript"),
    ("mjs", "javascript"),
    ("cjs", "javascript"),
    ("jsx", "jsx"),
    ("ts", "typescript"),
    ("typescript", "typescript"),
    ("tsx", "tsx"),
    ("json", "json"),
    ("toml", "toml"),
    ("yaml", "yaml"),
    ("yml", "yaml"),
    ("md", "markdown"),
    ("markdown", "markdown"),
    ("bash", "bash"),
    ("sh", "bash"),
    ("shell", "bash"),
    ("zsh", "bash"),
    ("fish", "fish"),
    ("powershell", "powershell"),
    ("pwsh", "powershell"),
    ("ps1", "powershell"),
    ("lua", "lua"),
    ("hk", "husk"),
    ("husk", "husk"),
    ("gitcommit", "gitcommit"),
    ("git-commit", "gitcommit"),
    ("commit", "gitcommit"),
];

const YAML_ADDITIONAL_HIGHLIGHTS_QUERY: &str = r#"
(escape_sequence) @escape

[
  (yaml_directive)
  (tag_directive)
  (reserved_directive)
] @keyword.directive
"#;

const RUST_TEXTOBJECT_QUERIES: &[&str] = &[include_str!("queries/textobjects/rust.scm")];
const ECMA_TEXTOBJECT_QUERY: &str = include_str!("queries/textobjects/ecma.scm");
const JSX_TEXTOBJECT_QUERY: &str = include_str!("queries/textobjects/jsx.scm");
const JAVASCRIPT_TEXTOBJECT_QUERIES: &[&str] = &[
    ECMA_TEXTOBJECT_QUERY,
    JSX_TEXTOBJECT_QUERY,
    include_str!("queries/textobjects/javascript.scm"),
];
const JSX_TEXTOBJECT_QUERIES: &[&str] = &[ECMA_TEXTOBJECT_QUERY, JSX_TEXTOBJECT_QUERY];
const TYPESCRIPT_TEXTOBJECT_QUERY: &str = include_str!("queries/textobjects/typescript.scm");
const TYPESCRIPT_TEXTOBJECT_QUERIES: &[&str] =
    &[ECMA_TEXTOBJECT_QUERY, TYPESCRIPT_TEXTOBJECT_QUERY];
const TSX_TEXTOBJECT_QUERIES: &[&str] = &[
    ECMA_TEXTOBJECT_QUERY,
    TYPESCRIPT_TEXTOBJECT_QUERY,
    JSX_TEXTOBJECT_QUERY,
    include_str!("queries/textobjects/tsx.scm"),
];

impl Highlighter {
    pub fn new(theme: &Theme) -> anyhow::Result<Self> {
        Self::with_registry(theme, Arc::new(LanguageRegistry::bundled()))
    }

    /// Creates a highlighter sharing one immutable runtime language snapshot.
    pub fn with_registry(theme: &Theme, registry: Arc<LanguageRegistry>) -> anyhow::Result<Self> {
        Ok(Self {
            highlighters: HashMap::new(),
            cached_highlight: None,
            registry,
            theme: theme.clone(),
            husk_styles: HuskStyles::new(theme),
            git_commit_styles: GitCommitStyles::new(theme),
        })
    }

    /// Returns the immutable language snapshot used by this rendering surface.
    #[must_use]
    pub fn registry(&self) -> Arc<LanguageRegistry> {
        Arc::clone(&self.registry)
    }

    /// Identifies a bounded set of configured fenced languages before the
    /// Markdown parser is available. Unknown and duplicate aliases are ignored;
    /// false positives only prewarm an existing grammar and never change spans.
    pub(crate) fn startup_injected_language_ids(
        &self,
        language_id: &str,
        source: &str,
    ) -> Vec<String> {
        const MAX_STARTUP_INJECTION_LANGUAGES: usize = 3;
        if language_id != "markdown" {
            return Vec::new();
        }

        let mut languages = Vec::new();
        for line in source.lines() {
            let line = line.trim_start();
            let Some(info) = line
                .strip_prefix("```")
                .or_else(|| line.strip_prefix("~~~"))
            else {
                continue;
            };
            let Some(name) = info.split_whitespace().next() else {
                continue;
            };
            let Some(resolved) = self.language_id_for_name(name) else {
                continue;
            };
            if resolved == language_id || languages.iter().any(|language| language == resolved) {
                continue;
            }
            languages.push(resolved.to_string());
            if languages.len() == MAX_STARTUP_INJECTION_LANGUAGES {
                break;
            }
        }
        languages
    }

    /// Compile the first visible language's queries while independent startup
    /// work runs. The registry snapshot keeps dynamic grammar libraries alive.
    pub(crate) fn prepare_language_in_background(
        &self,
        language_id: &str,
    ) -> Option<PendingLanguageHighlighter> {
        if self.highlighters.contains_key(language_id) {
            return None;
        }
        let definition = self.registry.languages.get(language_id)?;
        if definition.specialized.is_some() || definition.highlight_queries.is_empty() {
            return None;
        }
        let grammar = definition.grammar.clone()?;
        let highlights = definition.highlight_queries.join("\n");
        let injections = definition.injection_query.clone();
        let task = thread::Builder::new()
            .name("red-highlight-startup".to_string())
            .spawn(move || {
                let language = grammar_language(&grammar);
                let query = Query::new(&language, &highlights)?;
                let injection_query = injections
                    .as_deref()
                    .map(|source| Query::new(&language, source))
                    .transpose()?;
                Ok(CompiledLanguageQueries {
                    language,
                    query,
                    injection_query,
                })
            })
            .ok()?;
        Some(PendingLanguageHighlighter {
            language_id: language_id.to_string(),
            registry: Arc::clone(&self.registry),
            task,
        })
    }

    /// Install a completed query only when its exact language snapshot is still
    /// current. Failure remains best-effort; ordinary lazy loading retries it.
    pub(crate) fn finish_prepared_language(&mut self, pending: PendingLanguageHighlighter) {
        let Ok(Ok(prepared)) = pending.task.join() else {
            return;
        };
        if !Arc::ptr_eq(&self.registry, &pending.registry)
            || self.highlighters.contains_key(&pending.language_id)
        {
            return;
        }

        let mut parser = Parser::new();
        if parser.set_language(&prepared.language).is_err() {
            return;
        }
        let capture_styles = prepared
            .query
            .capture_names()
            .iter()
            .map(|scope| self.theme.get_style(scope))
            .collect();
        self.highlighters.insert(
            pending.language_id,
            LanguageHighlighter {
                parser,
                query: prepared.query,
                injection_query: prepared.injection_query,
                capture_styles,
                cached_tree: None,
            },
        );
    }

    pub fn language_id_for_file(&self, file: Option<&str>) -> Option<&str> {
        let file = file?;
        if let Some(filename) = Path::new(file).file_name().and_then(|name| name.to_str()) {
            if let Some(language) = self.registry.filenames.get(filename) {
                return Some(language.as_str());
            }
        }
        let extension = file_extension(file)?;
        self.language_id_for_extension(&extension)
    }

    /// Whether highlighting a language requires all text before the visible slice.
    ///
    /// YAML structure is indentation-sensitive, so parsing an arbitrary indented
    /// viewport can lose the mapping and scalar context that determines its nodes.
    pub(crate) fn requires_document_prefix(&self, language_id: Option<&str>) -> bool {
        matches!(language_id, Some("yaml"))
    }

    pub fn language_id_for_extension(&self, extension: &str) -> Option<&str> {
        let extension = extension.trim_start_matches('.').to_ascii_lowercase();
        self.registry
            .extensions
            .get(extension.as_str())
            .map(String::as_str)
    }

    pub fn language_id_for_name(&self, name: &str) -> Option<&str> {
        let name = name.trim().to_ascii_lowercase();
        let name = name.split_whitespace().next().unwrap_or_default();
        self.registry
            .aliases
            .get(name)
            .map(String::as_str)
            .or_else(|| self.language_id_for_extension(name))
    }

    /// Returns the bundled canonical language identifiers in display order.
    pub fn language_ids(&self) -> Vec<&str> {
        let mut language_ids = self
            .registry
            .languages
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        language_ids.sort_unstable();
        language_ids
    }

    /// Returns canonical language identifiers matching a name or extension prefix.
    pub fn matching_language_ids(&self, prefix: &str) -> Vec<&str> {
        let prefix = prefix.trim_start_matches('.').to_ascii_lowercase();
        let mut language_ids = self
            .registry
            .languages
            .keys()
            .map(String::as_str)
            .filter(|language| language.starts_with(&prefix))
            .chain(
                self.registry
                    .extensions
                    .iter()
                    .filter(|(extension, _)| extension.starts_with(&prefix))
                    .map(|(_, language)| language.as_str()),
            )
            .chain(
                self.registry
                    .aliases
                    .iter()
                    .filter(|(name, _)| name.starts_with(&prefix))
                    .map(|(_, language)| language.as_str()),
            )
            .collect::<Vec<_>>();
        language_ids.sort_unstable();
        language_ids.dedup();
        language_ids
    }

    pub fn highlight_for_file(
        &mut self,
        file: Option<&str>,
        code: &str,
    ) -> anyhow::Result<Vec<StyleInfo>> {
        let Some(language_id) = self.language_id_for_file(file) else {
            return Ok(Vec::new());
        };
        let language_id = language_id.to_string();
        self.highlight(&language_id, code)
    }

    pub fn highlight(&mut self, language_id: &str, code: &str) -> anyhow::Result<Vec<StyleInfo>> {
        if let Some(cached) = &self.cached_highlight {
            if cached.language_id == language_id && cached.code == code {
                return Ok(cached.styles.clone());
            }
        }

        if let Some(styles) = self.reuse_specialized_token_highlight(language_id, code) {
            return Ok(styles);
        }

        if let Some(styles) = self.reuse_markdown_injected_token_highlight(language_id, code) {
            return Ok(styles);
        }

        if let Some(styles) = self.reuse_stable_token_highlight(language_id, code) {
            return Ok(styles);
        }

        let styles = self.highlight_with_depth(language_id, code, 0)?;
        self.cached_highlight = if code.len() <= MAX_CACHED_HIGHLIGHT_BYTES
            && styles.len() <= MAX_CACHED_HIGHLIGHT_SPANS
        {
            Some(CachedHighlight {
                language_id: language_id.to_string(),
                code: code.to_string(),
                styles: styles.clone(),
            })
        } else {
            None
        };
        Ok(styles)
    }

    /// Recompute one independent commit line or preserve one stable Husk token.
    fn reuse_specialized_token_highlight(
        &mut self,
        language_id: &str,
        code: &str,
    ) -> Option<Vec<StyleInfo>> {
        if code.len() > MAX_CACHED_HIGHLIGHT_BYTES {
            return None;
        }
        let definition = self.registry.languages.get(language_id)?;
        let specialized = definition.specialized?;
        if definition.grammar.is_some() || !definition.highlight_queries.is_empty() {
            return None;
        }
        let cached = self.cached_highlight.as_mut()?;
        if cached.language_id != language_id {
            return None;
        }
        let edit = crate::syntax_indent::replacement_edit(&cached.code, code);
        let inserted = &code[edit.start_byte..edit.new_end_byte];
        let removed = &cached.code[edit.start_byte..edit.old_end_byte];
        if inserted.contains(['\r', '\n']) || removed.contains(['\r', '\n']) {
            return None;
        }
        let delta = isize::try_from(edit.new_end_byte)
            .ok()?
            .checked_sub(isize::try_from(edit.old_end_byte).ok()?)?;

        let styles = match (language_id, specialized) {
            ("gitcommit", SpecializedHighlighter::GitCommit) => {
                let line_start = cached.code[..edit.start_byte]
                    .rfind('\n')
                    .map_or(0, |position| position + 1);
                let old_line_end = cached.code[edit.old_end_byte..]
                    .find('\n')
                    .map_or(cached.code.len(), |offset| edit.old_end_byte + offset);
                let new_line_end = old_line_end.checked_add_signed(delta)?;
                let mut updated = Vec::with_capacity(cached.styles.len() + 4);
                updated.extend(
                    cached
                        .styles
                        .iter()
                        .filter(|style| style.end <= line_start)
                        .cloned(),
                );
                for mut style in
                    highlight_git_commit(&code[line_start..new_line_end], &self.git_commit_styles)
                {
                    style.start += line_start;
                    style.end += line_start;
                    updated.push(style);
                }
                for style in cached
                    .styles
                    .iter()
                    .filter(|style| style.start >= old_line_end)
                {
                    let mut style = style.clone();
                    style.start = style.start.checked_add_signed(delta)?;
                    style.end = style.end.checked_add_signed(delta)?;
                    updated.push(style);
                }
                updated
            }
            ("husk", SpecializedHighlighter::Husk) => {
                let covering = cached
                    .styles
                    .iter()
                    .find(|style| style.start < edit.start_byte && style.end > edit.old_end_byte);
                let (comment, string) = if let Some(covering) = covering {
                    let token = cached.code.get(covering.start..covering.end)?;
                    let comment = self
                        .husk_styles
                        .comment
                        .as_ref()
                        .is_some_and(|style| *style == covering.style)
                        && token.starts_with("//")
                        && !token.contains('\n')
                        && edit.start_byte > covering.start.saturating_add(2);
                    let string = self
                        .husk_styles
                        .string
                        .as_ref()
                        .is_some_and(|style| *style == covering.style)
                        && token.starts_with('"')
                        && token.ends_with('"')
                        && !token.contains(['\r', '\n'])
                        && !inserted.chars().chain(removed.chars()).any(|character| {
                            character.is_control() || matches!(character, '"' | '\\')
                        });
                    (comment, string)
                } else {
                    (false, false)
                };
                let numeric = stable_husk_numeric_token(
                    &cached.code,
                    edit.start_byte,
                    edit.old_end_byte,
                    inserted,
                );
                let identifier = stable_husk_identifier_token(
                    &cached.code,
                    code,
                    edit.start_byte,
                    edit.old_end_byte,
                    inserted,
                );
                if !comment && !string && !numeric && !identifier {
                    return None;
                }
                let mut updated = cached.styles.clone();
                for style in &mut updated {
                    if style.end <= edit.start_byte {
                        continue;
                    }
                    if style.start >= edit.old_end_byte {
                        style.start = style.start.checked_add_signed(delta)?;
                        style.end = style.end.checked_add_signed(delta)?;
                    } else if style.start < edit.start_byte && style.end >= edit.old_end_byte {
                        style.end = style.end.checked_add_signed(delta)?;
                    } else {
                        return None;
                    }
                }
                updated
            }
            _ => return None,
        };
        if styles.len() > MAX_CACHED_HIGHLIGHT_SPANS {
            return None;
        }
        cached.code.clear();
        cached.code.push_str(code);
        cached.styles.clone_from(&styles);
        Some(styles)
    }

    /// Preserve Markdown and an optional injected tree for one stable fenced token.
    fn reuse_markdown_injected_token_highlight(
        &mut self,
        language_id: &str,
        code: &str,
    ) -> Option<Vec<StyleInfo>> {
        if language_id != "markdown" || code.len() > MAX_CACHED_HIGHLIGHT_BYTES {
            return None;
        }
        let markdown = self.registry.languages.get("markdown")?;
        if !bundled_highlight_definition(markdown, "markdown")
            || markdown.injection_query.as_deref() != Some(MARKDOWN_INJECTION_QUERY)
        {
            return None;
        }

        let cached = self.cached_highlight.as_ref()?;
        if cached.language_id != "markdown" {
            return None;
        }
        let outer = self.highlighters.get("markdown")?.cached_tree.as_ref()?;
        if outer.source != cached.code {
            return None;
        }
        let outer_edit = crate::syntax_indent::replacement_edit(&cached.code, code);
        let inserted = code.get(outer_edit.start_byte..outer_edit.new_end_byte)?;
        let removed = cached
            .code
            .get(outer_edit.start_byte..outer_edit.old_end_byte)?;
        if inserted.chars().chain(removed.chars()).any(|character| {
            character.is_control() || matches!(character, '`' | '~' | '\u{2028}' | '\u{2029}')
        }) {
            return None;
        }

        let content = outer
            .tree
            .root_node()
            .named_descendant_for_byte_range(outer_edit.start_byte, outer_edit.old_end_byte)?;
        if content.kind() != "code_fence_content"
            || content.start_byte() >= outer_edit.start_byte
            || content.end_byte() <= outer_edit.old_end_byte
        {
            return None;
        }
        let fence = content.parent()?;
        if fence.kind() != "fenced_code_block" {
            return None;
        }
        let injected_language = {
            let mut fence_cursor = fence.walk();
            let info = fence
                .named_children(&mut fence_cursor)
                .find(|node| node.kind() == "info_string")?;
            let mut info_cursor = info.walk();
            let language = info
                .named_children(&mut info_cursor)
                .find(|node| node.kind() == "language")?;
            let language_name = cached.code.get(language.byte_range())?;
            self.language_id_for_name(language_name)?.to_string()
        };
        let definition = self.registry.languages.get(&injected_language)?;
        let specialized_husk = injected_language == "husk"
            && matches!(definition.specialized, Some(SpecializedHighlighter::Husk))
            && definition.grammar.is_none()
            && definition.highlight_queries.is_empty();
        if (!specialized_husk && !bundled_highlight_definition(definition, &injected_language))
            || definition.injection_query.is_some()
        {
            return None;
        }

        let delta = isize::try_from(outer_edit.new_end_byte)
            .ok()?
            .checked_sub(isize::try_from(outer_edit.old_end_byte).ok()?)?;
        let content_start = content.start_byte();
        let old_content_end = content.end_byte();
        let new_content_end = old_content_end.checked_add_signed(delta)?;
        let old_contents = cached.code.get(content_start..old_content_end)?;
        let new_contents = code.get(content_start..new_content_end)?;
        let nested_edit = if specialized_husk {
            let covering = cached.styles.iter().find(|style| {
                style.start >= content_start
                    && style.end <= old_content_end
                    && style.start < outer_edit.start_byte
                    && style.end > outer_edit.old_end_byte
                    && (self
                        .husk_styles
                        .comment
                        .as_ref()
                        .is_some_and(|expected| *expected == style.style)
                        || self
                            .husk_styles
                            .string
                            .as_ref()
                            .is_some_and(|expected| *expected == style.style)
                        || self
                            .husk_styles
                            .numeric
                            .as_ref()
                            .is_some_and(|expected| *expected == style.style))
            });
            let (comment, string) = if let Some(covering) = covering {
                let token = cached.code.get(covering.start..covering.end)?;
                let comment = self
                    .husk_styles
                    .comment
                    .as_ref()
                    .is_some_and(|style| *style == covering.style)
                    && token.starts_with("//")
                    && !token.contains('\n')
                    && outer_edit.start_byte > covering.start.saturating_add(2);
                let string = self
                    .husk_styles
                    .string
                    .as_ref()
                    .is_some_and(|style| *style == covering.style)
                    && token.starts_with('"')
                    && token.ends_with('"')
                    && !token.contains(['\r', '\n'])
                    && !inserted
                        .chars()
                        .chain(removed.chars())
                        .any(|character| matches!(character, '"' | '\\'));
                (comment, string)
            } else {
                (false, false)
            };
            let numeric = stable_husk_numeric_token(
                old_contents,
                outer_edit.start_byte.checked_sub(content_start)?,
                outer_edit.old_end_byte.checked_sub(content_start)?,
                inserted,
            );
            let identifier = stable_husk_identifier_token(
                old_contents,
                new_contents,
                outer_edit.start_byte.checked_sub(content_start)?,
                outer_edit.old_end_byte.checked_sub(content_start)?,
                inserted,
            );
            if !comment && !string && !numeric && !identifier {
                return None;
            }
            None
        } else {
            let nested = self
                .highlighters
                .get(&injected_language)?
                .cached_tree
                .as_ref()?;
            if nested.source != old_contents {
                return None;
            }
            let nested_edit = crate::syntax_indent::replacement_edit(old_contents, new_contents);
            stable_bundled_token(
                &injected_language,
                old_contents,
                new_contents,
                nested,
                &nested_edit,
            )?;
            Some(nested_edit)
        };

        let mut styles = cached.styles.clone();
        for style in &mut styles {
            if style.end <= outer_edit.start_byte {
                continue;
            }
            if style.start >= outer_edit.old_end_byte {
                style.start = style.start.checked_add_signed(delta)?;
                style.end = style.end.checked_add_signed(delta)?;
            } else if style.start < outer_edit.start_byte && style.end >= outer_edit.old_end_byte {
                style.end = style.end.checked_add_signed(delta)?;
            } else {
                return None;
            }
        }

        let outer = self
            .highlighters
            .get_mut("markdown")?
            .cached_tree
            .as_mut()?;
        outer.tree.edit(&outer_edit);
        outer.source.clear();
        outer.source.push_str(code);
        if let Some(nested_edit) = nested_edit {
            let nested = self
                .highlighters
                .get_mut(&injected_language)?
                .cached_tree
                .as_mut()?;
            nested.tree.edit(&nested_edit);
            nested.source.clear();
            nested.source.push_str(new_contents);
        }
        let cached = self.cached_highlight.as_mut()?;
        cached.code.clear();
        cached.code.push_str(code);
        cached.styles.clone_from(&styles);
        Some(styles)
    }

    /// Preserve exact bundled captures when an edit cannot change one token's
    /// grammar, boundaries, or predicates. Each supported grammar and token
    /// has its own restrictive validation before sharing the existing tree.
    fn reuse_stable_token_highlight(
        &mut self,
        language_id: &str,
        code: &str,
    ) -> Option<Vec<StyleInfo>> {
        if code.len() > MAX_CACHED_HIGHLIGHT_BYTES {
            return None;
        }

        let definition = self.registry.languages.get(language_id)?;
        if !bundled_highlight_definition(definition, language_id)
            || (language_id == "markdown"
                && definition.injection_query.as_deref() != Some(MARKDOWN_INJECTION_QUERY))
        {
            return None;
        }

        let cached = self.cached_highlight.as_mut()?;
        if cached.language_id != language_id {
            return None;
        }
        let syntax = self
            .highlighters
            .get_mut(language_id)?
            .cached_tree
            .as_mut()?;
        if syntax.source != cached.code {
            return None;
        }

        let edit = crate::syntax_indent::replacement_edit(&cached.code, code);
        let delta = isize::try_from(edit.new_end_byte)
            .ok()?
            .checked_sub(isize::try_from(edit.old_end_byte).ok()?)?;
        stable_bundled_token(language_id, &cached.code, code, syntax, &edit)?;

        let mut styles = cached.styles.clone();
        for style in &mut styles {
            if style.end <= edit.start_byte {
                continue;
            }
            if style.start >= edit.old_end_byte {
                style.start = style.start.checked_add_signed(delta)?;
                style.end = style.end.checked_add_signed(delta)?;
            } else if style.start < edit.start_byte && style.end >= edit.old_end_byte {
                style.end = style.end.checked_add_signed(delta)?;
            } else {
                return None;
            }
        }

        syntax.tree.edit(&edit);
        syntax.source.clear();
        syntax.source.push_str(code);
        cached.code.clear();
        cached.code.push_str(code);
        cached.styles.clone_from(&styles);
        Some(styles)
    }

    fn highlight_with_depth(
        &mut self,
        language_id: &str,
        code: &str,
        depth: usize,
    ) -> anyhow::Result<Vec<StyleInfo>> {
        let registry = Arc::clone(&self.registry);
        let Some(definition) = registry.languages.get(language_id) else {
            return Ok(Vec::new());
        };
        if let Some(specialized) = definition.specialized {
            return Ok(match specialized {
                SpecializedHighlighter::Husk => highlight_husk(code, &self.husk_styles),
                SpecializedHighlighter::GitCommit => {
                    highlight_git_commit(code, &self.git_commit_styles)
                }
            });
        }
        let Some(grammar) = &definition.grammar else {
            return Ok(Vec::new());
        };
        if definition.highlight_queries.is_empty() {
            return Ok(Vec::new());
        }

        if !self.highlighters.contains_key(&definition.id) {
            let language = grammar_language(grammar);
            let mut parser = Parser::new();
            parser.set_language(&language)?;
            let highlight_query = definition.highlight_queries.join("\n");
            let query = Query::new(&language, &highlight_query)?;
            let capture_styles = query
                .capture_names()
                .iter()
                .map(|scope| self.theme.get_style(scope))
                .collect();
            let injection_query = definition
                .injection_query
                .as_deref()
                .map(|query| Query::new(&language, query))
                .transpose()?;
            self.highlighters.insert(
                definition.id.clone(),
                LanguageHighlighter {
                    parser,
                    query,
                    injection_query,
                    capture_styles,
                    cached_tree: None,
                },
            );
        }

        let mut colors = Vec::new();
        let mut raw_injections = Vec::new();

        {
            let Some(highlighter) = self.highlighters.get_mut(&definition.id) else {
                return Ok(Vec::new());
            };
            let previous_tree = highlighter.cached_tree.take().map(|mut cached| {
                cached.tree.edit(&crate::syntax_indent::replacement_edit(
                    &cached.source,
                    code,
                ));
                cached.tree
            });
            let parsed = highlighter.parser.parse(code, previous_tree.as_ref());
            let Some(tree) = parsed else {
                return Ok(Vec::new());
            };

            {
                let mut cursor = QueryCursor::new();
                let mut matches =
                    cursor.matches(&highlighter.query, tree.root_node(), code.as_bytes());
                let mut refinement_colors = Vec::new();

                while let Some(mat) = matches.next() {
                    for cap in mat.captures {
                        let node = cap.node;
                        let start = node.start_byte();
                        let end = node.end_byte();
                        let capture_name = highlighter.query.capture_names()[cap.index as usize];
                        if let Some(style) = highlighter.capture_styles[cap.index as usize].as_ref()
                        {
                            let captured = StyleInfo {
                                start,
                                end,
                                style: style.clone(),
                            };
                            if capture_refines_equal_range(capture_name) {
                                refinement_colors.push(captured);
                            } else {
                                colors.push(captured);
                            }
                        }
                    }
                }

                // Query cursors return captures in syntax-tree order, which can put a
                // broad scalar capture after a more specific capture over the same
                // bytes. Keep semantic refinements later so the renderer's stable
                // equal-range tie-break selects them.
                colors.extend(refinement_colors);
            }

            if depth < MAX_INJECTION_DEPTH {
                if let Some(injection_query) = &highlighter.injection_query {
                    raw_injections = collect_injections(injection_query, tree.root_node(), code);
                }
            }

            if code.len() <= MAX_CACHED_HIGHLIGHT_BYTES {
                highlighter.cached_tree = Some(CachedSyntaxTree {
                    source: code.to_owned(),
                    tree,
                });
            }
        }

        let injections = raw_injections
            .into_iter()
            .filter_map(|injection| {
                let language_id = self.language_id_for_name(&injection.language_name)?;
                Some(Injection {
                    language_id: language_id.to_string(),
                    content_start: injection.content_start,
                    content_end: injection.content_end,
                })
            })
            .collect::<Vec<_>>();

        for injection in injections {
            let Some(injected_code) = code.get(injection.content_start..injection.content_end)
            else {
                continue;
            };
            let mut injected_colors =
                self.highlight_with_depth(&injection.language_id, injected_code, depth + 1)?;
            for color in &mut injected_colors {
                color.start += injection.content_start;
                color.end += injection.content_start;
            }
            colors.extend(injected_colors);
        }

        Ok(colors)
    }
}

fn stable_husk_numeric_token(source: &str, start: usize, end: usize, inserted: &str) -> bool {
    if !inserted.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let line_start = source[..start].rfind('\n').map_or(0, |index| index + 1);
    let line_end = source[end..]
        .find('\n')
        .map_or(source.len(), |offset| end + offset);
    let line = &source[line_start..line_end];
    Lexer::new(line).any(|token| {
        let range = token.span.range;
        let token_start = line_start + range.start;
        let token_end = line_start + range.end;
        matches!(token.kind, TokenKind::IntLiteral(_))
            && token_start < start
            && token_end > end
            && line.get(range).is_some_and(|text| {
                !text.starts_with('0') && text.bytes().all(|byte| byte.is_ascii_digit())
            })
    })
}

fn stable_husk_identifier_token(
    previous_source: &str,
    source: &str,
    start: usize,
    end: usize,
    inserted: &str,
) -> bool {
    if !inserted.bytes().all(|byte| byte.is_ascii_lowercase()) {
        return false;
    }
    let line_start = previous_source[..start]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let line_end = previous_source[end..]
        .find('\n')
        .map_or(previous_source.len(), |offset| end + offset);
    let line = &previous_source[line_start..line_end];
    Lexer::new(line).any(|token| {
        let TokenKind::Ident(previous) = token.kind else {
            return false;
        };
        let token_start = line_start + token.span.range.start;
        let token_end = line_start + token.span.range.end;
        if token_start >= start
            || token_end <= end
            || !previous.bytes().all(|byte| byte.is_ascii_lowercase())
            || is_husk_builtin_type(&previous)
        {
            return false;
        }
        let updated_end = token_end - (end - start) + inserted.len();
        source.get(token_start..updated_end).is_some_and(|updated| {
            updated.bytes().all(|byte| byte.is_ascii_lowercase())
                && !husk_lexer::is_keyword(updated)
                && !is_husk_builtin_type(updated)
        })
    })
}

fn bundled_highlight_definition(definition: &RuntimeLanguageDefinition, language_id: &str) -> bool {
    let expected_queries = match language_id {
        "rust" => &[tree_sitter_rust::HIGHLIGHTS_QUERY][..],
        "markdown" => &[MARKDOWN_HIGHLIGHT_QUERY][..],
        "javascript" => JAVASCRIPT_HIGHLIGHT_QUERIES,
        "jsx" => JSX_HIGHLIGHT_QUERIES,
        "typescript" => TYPESCRIPT_HIGHLIGHT_QUERIES,
        "tsx" => TSX_HIGHLIGHT_QUERIES,
        "json" => &[tree_sitter_json::HIGHLIGHTS_QUERY][..],
        "toml" => &[tree_sitter_toml_ng::HIGHLIGHTS_QUERY][..],
        "yaml" => &[
            tree_sitter_yaml::HIGHLIGHTS_QUERY,
            YAML_ADDITIONAL_HIGHLIGHTS_QUERY,
        ][..],
        "bash" => &[tree_sitter_bash::HIGHLIGHT_QUERY][..],
        "fish" => &[tree_sitter_fish::HIGHLIGHTS_QUERY][..],
        "powershell" => &[tree_sitter_powershell::HIGHLIGHTS_QUERY][..],
        "lua" => &[tree_sitter_lua::HIGHLIGHTS_QUERY][..],
        _ => return false,
    };
    matches!(definition.grammar.as_ref(), Some(GrammarSource::Bundled(_)))
        && definition.highlight_queries.len() == expected_queries.len()
        && definition
            .highlight_queries
            .iter()
            .zip(expected_queries)
            .all(|(actual, expected)| actual == expected)
}

fn stable_bundled_token(
    language_id: &str,
    previous_source: &str,
    source: &str,
    syntax: &CachedSyntaxTree,
    edit: &tree_sitter::InputEdit,
) -> Option<()> {
    let token = syntax
        .tree
        .root_node()
        .named_descendant_for_byte_range(edit.start_byte, edit.old_end_byte)?;
    if token.start_byte() >= edit.start_byte || token.end_byte() <= edit.old_end_byte {
        return None;
    }

    let delta = isize::try_from(edit.new_end_byte)
        .ok()?
        .checked_sub(isize::try_from(edit.old_end_byte).ok()?)?;
    let inserted = source.get(edit.start_byte..edit.new_end_byte)?;
    match token.kind() {
        "integer_literal" | "decimal_integer_literal" | "integer" | "integer_scalar" | "number" => {
            let supported = match token.kind() {
                "integer_literal" => language_id == "rust",
                "decimal_integer_literal" => language_id == "powershell",
                "integer" => language_id == "toml",
                "integer_scalar" => language_id == "yaml",
                "number" => matches!(
                    language_id,
                    "javascript" | "jsx" | "typescript" | "tsx" | "json" | "lua"
                ),
                _ => false,
            };
            let previous = previous_source.get(token.byte_range())?;
            let end = token.end_byte().checked_add_signed(delta)?;
            let updated = source.get(token.start_byte()..end)?;
            if !supported
                || previous.starts_with('0')
                || !previous.bytes().all(|byte| byte.is_ascii_digit())
                || !inserted.bytes().all(|byte| byte.is_ascii_digit())
                || !updated.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
        }
        "identifier" | "type_identifier" | "field_identifier" => {
            let previous = previous_source.get(token.byte_range())?;
            let end = token.end_byte().checked_add_signed(delta)?;
            let updated = source.get(token.start_byte()..end)?;
            if language_id == "rust" {
                if !previous.starts_with(|character: char| character.is_ascii_lowercase())
                    || !previous.chars().all(|character| {
                        character == '_' || unicode_ident::is_xid_continue(character)
                    })
                    || !inserted.chars().all(|character| {
                        character == '_' || unicode_ident::is_xid_continue(character)
                    })
                    || rust_keyword(updated)
                {
                    return None;
                }
            } else {
                if token.kind() != "identifier"
                    || !matches!(
                        language_id,
                        "javascript" | "jsx" | "typescript" | "tsx" | "lua"
                    )
                    || !previous.bytes().all(|byte| byte.is_ascii_lowercase())
                    || !inserted.bytes().all(|byte| byte.is_ascii_lowercase())
                    || !updated.bytes().all(|byte| byte.is_ascii_lowercase())
                {
                    return None;
                }
                let sensitive = if language_id == "lua" {
                    lua_sensitive_identifier
                } else {
                    ecmascript_sensitive_identifier
                };
                if sensitive(previous) || sensitive(updated) {
                    return None;
                }
            }
        }
        "bare_key" | "string_scalar" | "variable_name" | "word" | "variable" => {
            let parent = token.parent()?;
            let supported = match (language_id, token.kind()) {
                ("toml", "bare_key") => parent.kind() == "pair",
                ("yaml", "string_scalar") => {
                    if parent.kind() != "plain_scalar" {
                        return None;
                    }
                    let flow = parent.parent()?;
                    let mapping = flow.parent()?;
                    flow.kind() == "flow_node"
                        && mapping.kind() == "block_mapping_pair"
                        && mapping.child_by_field_name("key") == Some(flow)
                }
                ("bash", "variable_name") => {
                    parent.kind() == "variable_assignment"
                        && parent.child_by_field_name("name") == Some(token)
                }
                ("fish", "word") => {
                    parent.kind() == "function_definition"
                        && parent.child_by_field_name("name") == Some(token)
                }
                ("powershell", "variable") => true,
                _ => false,
            };
            if !supported || !inserted.bytes().all(|byte| byte.is_ascii_lowercase()) {
                return None;
            }
            let previous = previous_source.get(token.byte_range())?;
            let end = token.end_byte().checked_add_signed(delta)?;
            let updated = source.get(token.start_byte()..end)?;
            let (previous, updated) = if language_id == "powershell" {
                (previous.strip_prefix('$')?, updated.strip_prefix('$')?)
            } else {
                (previous, updated)
            };
            if !previous.bytes().all(|byte| byte.is_ascii_lowercase())
                || !updated.bytes().all(|byte| byte.is_ascii_lowercase())
            {
                return None;
            }
            let sensitive = match language_id {
                "yaml" => yaml_sensitive_identifier(previous) || yaml_sensitive_identifier(updated),
                "fish" => fish_sensitive_identifier(previous) || fish_sensitive_identifier(updated),
                "powershell" => {
                    powershell_sensitive_identifier(previous)
                        || powershell_sensitive_identifier(updated)
                }
                _ => false,
            };
            if sensitive {
                return None;
            }
        }
        "inline" => {
            if language_id != "markdown" || token.parent()?.kind() != "atx_heading" {
                return None;
            }
            let removed = previous_source.get(edit.start_byte..edit.old_end_byte)?;
            if inserted.chars().chain(removed.chars()).any(|character| {
                character.is_control()
                    || matches!(
                        character,
                        '#' | '\\'
                            | '`'
                            | '*'
                            | '_'
                            | '['
                            | ']'
                            | '!'
                            | '>'
                            | '|'
                            | '~'
                            | '='
                            | ':'
                            | '\u{2028}'
                            | '\u{2029}'
                    )
            }) {
                return None;
            }
        }
        "line_comment" => {
            if language_id != "rust" {
                return None;
            }
            let previous = previous_source.get(token.byte_range())?;
            if !previous.starts_with("//")
                || previous.starts_with("///")
                || previous.starts_with("//!")
                || edit.start_byte <= token.start_byte().saturating_add(2)
                || inserted.contains(['\r', '\n'])
            {
                return None;
            }
        }
        "comment" => {
            let previous = previous_source.get(token.byte_range())?;
            let marker = match language_id {
                "javascript" | "jsx" | "typescript" | "tsx" => "//",
                "toml" | "yaml" | "bash" | "fish" | "powershell" => "#",
                _ => return None,
            };
            if !previous.starts_with(marker)
                || edit.start_byte <= token.start_byte().saturating_add(marker.len())
                || inserted.contains(['\r', '\n', '\u{2028}', '\u{2029}'])
            {
                return None;
            }
        }
        "comment_content" => {
            if language_id != "lua" {
                return None;
            }
            let parent = token.parent()?;
            let previous = previous_source.get(parent.byte_range())?;
            if parent.kind() != "comment"
                || !previous.starts_with("--")
                || previous.starts_with("--[")
                || inserted.contains(['\r', '\n'])
            {
                return None;
            }
        }
        "string_content" => {
            let expected_parent = match language_id {
                "rust" => "string_literal",
                "json" | "bash" | "lua" => "string",
                _ => return None,
            };
            if token.parent()?.kind() != expected_parent
                || inserted.chars().any(|character| {
                    character.is_control()
                        || matches!(character, '"' | '\\')
                        || (language_id == "bash" && matches!(character, '$' | '`'))
                })
            {
                return None;
            }
        }
        "string_fragment" => {
            if !matches!(language_id, "javascript" | "jsx" | "typescript" | "tsx")
                || token.parent()?.kind() != "string"
                || inserted.chars().any(|character| {
                    character.is_control()
                        || matches!(character, '"' | '\'' | '\\' | '\u{2028}' | '\u{2029}')
                })
            {
                return None;
            }
        }
        "string" | "double_quote_scalar" | "double_quote_string" | "expandable_string_literal" => {
            let expected_kind = match language_id {
                "toml" => "string",
                "yaml" => "double_quote_scalar",
                "fish" => "double_quote_string",
                "powershell" => "expandable_string_literal",
                _ => return None,
            };
            let previous = previous_source.get(token.byte_range())?;
            if token.kind() != expected_kind
                || !previous.starts_with('"')
                || !previous.ends_with('"')
                || previous.starts_with("\"\"\"")
                || inserted.chars().any(|character| {
                    character.is_control()
                        || matches!(character, '"' | '\\')
                        || (matches!(language_id, "fish" | "powershell")
                            && matches!(character, '$' | '`'))
                })
            {
                return None;
            }
        }
        _ => return None,
    }
    Some(())
}

fn rust_keyword(identifier: &str) -> bool {
    matches!(
        identifier,
        "abstract"
            | "as"
            | "async"
            | "await"
            | "become"
            | "box"
            | "break"
            | "const"
            | "continue"
            | "crate"
            | "default"
            | "do"
            | "dyn"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "final"
            | "fn"
            | "for"
            | "gen"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "macro"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "override"
            | "priv"
            | "pub"
            | "raw"
            | "ref"
            | "return"
            | "safe"
            | "self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "try"
            | "type"
            | "typeof"
            | "union"
            | "unsafe"
            | "unsized"
            | "use"
            | "virtual"
            | "where"
            | "while"
            | "yield"
    )
}

fn ecmascript_sensitive_identifier(identifier: &str) -> bool {
    matches!(
        identifier,
        "abstract"
            | "any"
            | "arguments"
            | "as"
            | "assert"
            | "asserts"
            | "async"
            | "await"
            | "bigint"
            | "boolean"
            | "break"
            | "case"
            | "catch"
            | "class"
            | "console"
            | "const"
            | "constructor"
            | "continue"
            | "debugger"
            | "declare"
            | "default"
            | "delete"
            | "do"
            | "document"
            | "else"
            | "enum"
            | "eval"
            | "export"
            | "extends"
            | "false"
            | "finally"
            | "for"
            | "from"
            | "function"
            | "get"
            | "global"
            | "if"
            | "implements"
            | "import"
            | "in"
            | "infer"
            | "instanceof"
            | "interface"
            | "intrinsic"
            | "is"
            | "keyof"
            | "let"
            | "module"
            | "namespace"
            | "never"
            | "new"
            | "null"
            | "number"
            | "object"
            | "of"
            | "out"
            | "override"
            | "package"
            | "private"
            | "protected"
            | "prototype"
            | "public"
            | "readonly"
            | "require"
            | "return"
            | "satisfies"
            | "set"
            | "static"
            | "string"
            | "super"
            | "switch"
            | "symbol"
            | "target"
            | "this"
            | "throw"
            | "true"
            | "try"
            | "type"
            | "typeof"
            | "undefined"
            | "unique"
            | "unknown"
            | "using"
            | "var"
            | "void"
            | "while"
            | "window"
            | "with"
            | "yield"
    )
}

fn lua_sensitive_identifier(identifier: &str) -> bool {
    matches!(
        identifier,
        "and"
            | "assert"
            | "break"
            | "collectgarbage"
            | "do"
            | "dofile"
            | "else"
            | "elseif"
            | "end"
            | "error"
            | "false"
            | "for"
            | "function"
            | "getfenv"
            | "getmetatable"
            | "global"
            | "goto"
            | "if"
            | "in"
            | "ipairs"
            | "load"
            | "loadfile"
            | "loadstring"
            | "local"
            | "module"
            | "next"
            | "nil"
            | "not"
            | "or"
            | "pairs"
            | "pcall"
            | "print"
            | "rawequal"
            | "rawget"
            | "rawset"
            | "repeat"
            | "require"
            | "return"
            | "select"
            | "self"
            | "setfenv"
            | "setmetatable"
            | "then"
            | "tonumber"
            | "tostring"
            | "true"
            | "type"
            | "until"
            | "unpack"
            | "while"
            | "xpcall"
    )
}

fn yaml_sensitive_identifier(identifier: &str) -> bool {
    matches!(
        identifier,
        "false" | "no" | "null" | "off" | "on" | "true" | "yes"
    )
}

fn fish_sensitive_identifier(identifier: &str) -> bool {
    matches!(
        identifier,
        "and"
            | "begin"
            | "break"
            | "case"
            | "continue"
            | "else"
            | "end"
            | "for"
            | "function"
            | "if"
            | "in"
            | "not"
            | "or"
            | "return"
            | "set"
            | "switch"
            | "test"
            | "while"
    )
}

fn powershell_sensitive_identifier(identifier: &str) -> bool {
    matches!(
        identifier,
        "args"
            | "error"
            | "event"
            | "eventargs"
            | "eventsubscriber"
            | "executioncontext"
            | "false"
            | "foreach"
            | "home"
            | "host"
            | "input"
            | "iscoreclr"
            | "islinux"
            | "ismacos"
            | "iswindows"
            | "lastsuccess"
            | "matches"
            | "myinvocation"
            | "nestedpromptlevel"
            | "null"
            | "pid"
            | "profile"
            | "psboundparameters"
            | "pscommandpath"
            | "psculture"
            | "psdebugcontext"
            | "pshome"
            | "psitem"
            | "psscriptroot"
            | "pssenderinfo"
            | "psuiculture"
            | "psversiontable"
            | "pwd"
            | "sender"
            | "shellid"
            | "stacktrace"
            | "switch"
            | "this"
            | "true"
    )
}

fn capture_refines_equal_range(scope: &str) -> bool {
    match scope {
        "property" | "escape" | "string.escape" => true,
        scope if scope.starts_with("keyword.directive") => true,
        _ => false,
    }
}

fn collect_injections(
    query: &Query,
    root_node: tree_sitter::Node<'_>,
    code: &str,
) -> Vec<RawInjection> {
    let mut injections = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root_node, code.as_bytes());

    while let Some(mat) = matches.next() {
        let mut language_name = query
            .property_settings(mat.pattern_index)
            .iter()
            .find(|property| property.key.as_ref() == "injection.language")
            .and_then(|property| property.value.as_deref());
        let mut content = None;

        for capture in mat.captures {
            let capture_name = query.capture_names()[capture.index as usize];
            match capture_name {
                "injection.language" => {
                    if language_name.is_none() {
                        language_name = capture.node.utf8_text(code.as_bytes()).ok();
                    }
                }
                "injection.content" => {
                    content = Some((capture.node.start_byte(), capture.node.end_byte()));
                }
                _ => {}
            }
        }

        let (Some(language_name), Some((content_start, content_end))) = (language_name, content)
        else {
            continue;
        };

        injections.push(RawInjection {
            language_name: language_name.to_string(),
            content_start,
            content_end,
        });
    }

    injections
}

fn file_extension(file: &str) -> Option<String> {
    Path::new(file)
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
}

fn language_definitions() -> Vec<BundledLanguageDefinition> {
    vec![
        BundledLanguageDefinition {
            id: "rust",
            extensions: &["rs"],
            filenames: &[],
            language: Some(|| tree_sitter_rust::LANGUAGE.into()),
            highlight_queries: &[tree_sitter_rust::HIGHLIGHTS_QUERY],
            textobject_queries: RUST_TEXTOBJECT_QUERIES,
            injection_query: None,
            specialized: None,
        },
        BundledLanguageDefinition {
            id: "markdown",
            extensions: &["md", "markdown"],
            filenames: &[],
            language: Some(|| tree_sitter_md::LANGUAGE.into()),
            highlight_queries: &[MARKDOWN_HIGHLIGHT_QUERY],
            textobject_queries: &[include_str!("queries/textobjects/markdown.scm")],
            injection_query: Some(MARKDOWN_INJECTION_QUERY),
            specialized: None,
        },
        BundledLanguageDefinition {
            id: "javascript",
            extensions: &["js", "mjs", "cjs"],
            filenames: &[],
            language: Some(|| tree_sitter_javascript::LANGUAGE.into()),
            highlight_queries: JAVASCRIPT_HIGHLIGHT_QUERIES,
            textobject_queries: JAVASCRIPT_TEXTOBJECT_QUERIES,
            injection_query: None,
            specialized: None,
        },
        BundledLanguageDefinition {
            id: "jsx",
            extensions: &["jsx"],
            filenames: &[],
            language: Some(|| tree_sitter_javascript::LANGUAGE.into()),
            highlight_queries: JSX_HIGHLIGHT_QUERIES,
            textobject_queries: JSX_TEXTOBJECT_QUERIES,
            injection_query: None,
            specialized: None,
        },
        BundledLanguageDefinition {
            id: "typescript",
            extensions: &["ts"],
            filenames: &[],
            language: Some(|| tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
            highlight_queries: TYPESCRIPT_HIGHLIGHT_QUERIES,
            textobject_queries: TYPESCRIPT_TEXTOBJECT_QUERIES,
            injection_query: None,
            specialized: None,
        },
        BundledLanguageDefinition {
            id: "tsx",
            extensions: &["tsx"],
            filenames: &[],
            language: Some(|| tree_sitter_typescript::LANGUAGE_TSX.into()),
            highlight_queries: TSX_HIGHLIGHT_QUERIES,
            textobject_queries: TSX_TEXTOBJECT_QUERIES,
            injection_query: None,
            specialized: None,
        },
        BundledLanguageDefinition {
            id: "json",
            extensions: &["json"],
            filenames: &[],
            language: Some(|| tree_sitter_json::LANGUAGE.into()),
            highlight_queries: &[tree_sitter_json::HIGHLIGHTS_QUERY],
            textobject_queries: &[include_str!("queries/textobjects/json.scm")],
            injection_query: None,
            specialized: None,
        },
        BundledLanguageDefinition {
            id: "toml",
            extensions: &["toml"],
            filenames: &[],
            language: Some(|| tree_sitter_toml_ng::LANGUAGE.into()),
            highlight_queries: &[tree_sitter_toml_ng::HIGHLIGHTS_QUERY],
            textobject_queries: &[include_str!("queries/textobjects/toml.scm")],
            injection_query: None,
            specialized: None,
        },
        BundledLanguageDefinition {
            id: "yaml",
            extensions: &["yml", "yaml"],
            filenames: &[],
            language: Some(|| tree_sitter_yaml::LANGUAGE.into()),
            highlight_queries: &[
                tree_sitter_yaml::HIGHLIGHTS_QUERY,
                YAML_ADDITIONAL_HIGHLIGHTS_QUERY,
            ],
            textobject_queries: &[include_str!("queries/textobjects/yaml.scm")],
            injection_query: None,
            specialized: None,
        },
        BundledLanguageDefinition {
            id: "bash",
            extensions: &["sh", "bash", "zsh"],
            filenames: &[],
            language: Some(|| tree_sitter_bash::LANGUAGE.into()),
            highlight_queries: &[tree_sitter_bash::HIGHLIGHT_QUERY],
            textobject_queries: &[include_str!("queries/textobjects/bash.scm")],
            injection_query: None,
            specialized: None,
        },
        BundledLanguageDefinition {
            id: "fish",
            extensions: &["fish"],
            filenames: &[],
            language: Some(tree_sitter_fish::language),
            highlight_queries: &[tree_sitter_fish::HIGHLIGHTS_QUERY],
            textobject_queries: &[include_str!("queries/textobjects/fish.scm")],
            injection_query: None,
            specialized: None,
        },
        BundledLanguageDefinition {
            id: "powershell",
            extensions: &["ps1", "psm1", "psd1"],
            filenames: &[],
            language: Some(|| tree_sitter_powershell::LANGUAGE.into()),
            highlight_queries: &[tree_sitter_powershell::HIGHLIGHTS_QUERY],
            textobject_queries: &[include_str!("queries/textobjects/powershell.scm")],
            injection_query: None,
            specialized: None,
        },
        BundledLanguageDefinition {
            id: "lua",
            extensions: &["lua"],
            filenames: &[],
            language: Some(|| tree_sitter_lua::LANGUAGE.into()),
            highlight_queries: &[tree_sitter_lua::HIGHLIGHTS_QUERY],
            textobject_queries: &[include_str!("queries/textobjects/lua.scm")],
            injection_query: None,
            specialized: None,
        },
        BundledLanguageDefinition {
            id: "husk",
            extensions: &["hk", "husk"],
            filenames: &[],
            language: None,
            highlight_queries: &[],
            textobject_queries: &[],
            injection_query: None,
            specialized: Some(SpecializedHighlighter::Husk),
        },
        BundledLanguageDefinition {
            id: "gitcommit",
            extensions: &["gitcommit"],
            filenames: &["COMMIT_EDITMSG", "MERGE_MSG", "SQUASH_MSG", "TAG_EDITMSG"],
            language: None,
            highlight_queries: &[],
            textobject_queries: &[],
            injection_query: None,
            specialized: Some(SpecializedHighlighter::GitCommit),
        },
    ]
}

fn highlight_git_commit(code: &str, theme: &GitCommitStyles) -> Vec<StyleInfo> {
    let mut styles = Vec::new();
    let mut offset = 0;

    for line_with_ending in code.split_inclusive('\n') {
        let line = line_with_ending.trim_end_matches(['\r', '\n']);
        if let Some(comment) = line.strip_prefix('#') {
            let line_end = offset + line.len();
            push_style(theme.comment.as_ref(), offset..line_end, &mut styles);

            let leading = comment.len() - comment.trim_start().len();
            let body = comment.trim_start();
            let body_start = offset + 1 + leading;
            highlight_git_commit_comment(body, body_start, theme, &mut styles);
        }
        offset += line_with_ending.len();
    }

    styles
}

fn highlight_git_commit_comment(
    body: &str,
    body_start: usize,
    theme: &GitCommitStyles,
    styles: &mut Vec<StyleInfo>,
) {
    if body.is_empty() {
        return;
    }

    if body == "--- Red commit context (not part of the commit message) ---"
        || body == "Commands:"
        || body == "Staged diff:"
        || body.ends_with("files:")
        || body.ends_with("paths:")
        || body.ends_with("committed:")
        || body.ends_with("commit:")
    {
        push_style(
            theme.heading.as_ref(),
            body_start..body_start + body.len(),
            styles,
        );
    }

    if let Some(branch) = body.strip_prefix("On branch ") {
        let prefix_len = "On branch ".len();
        push_style(
            theme.status.as_ref(),
            body_start..body_start + prefix_len,
            styles,
        );
        push_style(
            theme.reference.as_ref(),
            body_start + prefix_len..body_start + prefix_len + branch.len(),
            styles,
        );
    }

    for prefix in ["new file:", "modified:", "deleted:", "renamed:"] {
        if let Some(path) = body.strip_prefix(prefix) {
            let path_leading = path.len() - path.trim_start().len();
            push_style(
                theme.status.as_ref(),
                body_start..body_start + prefix.len(),
                styles,
            );
            push_style(
                theme.reference.as_ref(),
                body_start + prefix.len() + path_leading..body_start + body.len(),
                styles,
            );
        }
    }

    if let Some((status, path)) = body.split_once("  ") {
        if matches!(status, "A" | "M" | "D" | "R" | "C" | "U" | "??") && !path.is_empty() {
            push_style(
                theme.status.as_ref(),
                body_start..body_start + status.len(),
                styles,
            );
            push_style(
                theme.reference.as_ref(),
                body_start + status.len() + 2..body_start + body.len(),
                styles,
            );
        }
    }

    for quoted in body.match_indices('\'') {
        let quote_start = quoted.0;
        let rest = &body[quote_start + 1..];
        if let Some(relative_end) = rest.find('\'') {
            push_style(
                theme.reference.as_ref(),
                body_start + quote_start..body_start + quote_start + relative_end + 2,
                styles,
            );
            break;
        }
    }

    let diff_style = if body.starts_with('+') && !body.starts_with("+++") {
        theme.inserted.as_ref()
    } else if body.starts_with('-') && !body.starts_with("---") {
        theme.deleted.as_ref()
    } else if body.starts_with("diff --git ")
        || body.starts_with("index ")
        || body.starts_with("@@")
        || body.starts_with("--- ")
        || body.starts_with("+++ ")
    {
        theme.changed.as_ref()
    } else {
        None
    };
    push_style(diff_style, body_start..body_start + body.len(), styles);
}

fn highlight_husk(code: &str, theme: &HuskStyles) -> Vec<StyleInfo> {
    let mut styles = Vec::new();
    let mut cursor = 0;

    for token in Lexer::new(code) {
        highlight_trivia(&token.leading_trivia, cursor, theme, &mut styles);

        let token_start = token.span.range.start;
        let token_end = token.span.range.end;
        if !matches!(token.kind, TokenKind::Eof) {
            highlight_husk_token(
                &token.kind,
                token_start..token_end,
                code,
                theme,
                &mut styles,
            );
        }
        cursor = token_end;

        cursor = highlight_trivia(&token.trailing_trivia, cursor, theme, &mut styles);
    }

    styles
}

fn highlight_trivia(
    trivia: &[Trivia],
    mut cursor: usize,
    theme: &HuskStyles,
    styles: &mut Vec<StyleInfo>,
) -> usize {
    for item in trivia {
        let len = trivia_len(item);
        let start = cursor;
        cursor += len;
        if matches!(item, Trivia::LineComment(_)) {
            push_style(theme.comment.as_ref(), start..cursor, styles);
        }
    }
    cursor
}

fn trivia_len(trivia: &Trivia) -> usize {
    match trivia {
        Trivia::Whitespace(value) | Trivia::Newline(value) | Trivia::LineComment(value) => {
            value.len()
        }
    }
}

fn highlight_husk_token(
    kind: &TokenKind,
    range: Range<usize>,
    code: &str,
    theme: &HuskStyles,
    styles: &mut Vec<StyleInfo>,
) {
    match kind {
        TokenKind::Keyword(Keyword::True | Keyword::False) => {
            push_style(theme.constant_builtin.as_ref(), range, styles);
        }
        TokenKind::Keyword(Keyword::SelfType) => {
            push_style(theme.variable_builtin.as_ref(), range, styles);
        }
        TokenKind::Keyword(_) => {
            push_style(theme.keyword.as_ref(), range, styles);
        }
        TokenKind::IntLiteral(_) | TokenKind::FloatLiteral(_) => {
            push_style(theme.numeric.as_ref(), range, styles);
        }
        TokenKind::StringLiteral(_) => {
            push_style(theme.string.as_ref(), range, styles);
        }
        TokenKind::Ident(_) => {
            if let Some(text) = code.get(range.clone()) {
                if is_husk_builtin_type(text) {
                    push_style(theme.type_builtin.as_ref(), range, styles);
                }
            }
        }
        TokenKind::Plus
        | TokenKind::PlusEq
        | TokenKind::Minus
        | TokenKind::MinusEq
        | TokenKind::Star
        | TokenKind::Slash
        | TokenKind::Percent
        | TokenKind::PercentEq
        | TokenKind::Eq
        | TokenKind::EqEq
        | TokenKind::Bang
        | TokenKind::BangEq
        | TokenKind::Lt
        | TokenKind::Gt
        | TokenKind::Le
        | TokenKind::Ge
        | TokenKind::AndAnd
        | TokenKind::Amp
        | TokenKind::OrOr
        | TokenKind::Pipe
        | TokenKind::Arrow
        | TokenKind::FatArrow
        | TokenKind::Question
        | TokenKind::DotDot
        | TokenKind::DotDotEq => {
            push_style(theme.operator.as_ref(), range, styles);
        }
        _ => {}
    }
}

fn is_husk_builtin_type(text: &str) -> bool {
    matches!(
        text,
        "bool"
            | "char"
            | "f32"
            | "f64"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "isize"
            | "Json"
            | "str"
            | "String"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "usize"
    )
}

fn push_style(style: Option<&Style>, range: Range<usize>, styles: &mut Vec<StyleInfo>) {
    if range.start >= range.end {
        return;
    }

    if let Some(style) = style {
        styles.push(StyleInfo {
            start: range.start,
            end: range.end,
            style: style.clone(),
        });
    }
}

const JAVASCRIPT_PARAMETER_HIGHLIGHT_QUERY: &str = r#"
(formal_parameters
  (pattern/identifier) @variable.parameter)

(formal_parameters
  (pattern/array_pattern
    (identifier) @variable.parameter))

(formal_parameters
  (pattern/object_pattern
    [
      (pair_pattern value: (identifier) @variable.parameter)
      (shorthand_property_identifier_pattern) @variable.parameter
    ]))
"#;

const JAVASCRIPT_HIGHLIGHT_QUERIES: &[&str] = &[
    tree_sitter_javascript::HIGHLIGHT_QUERY,
    JAVASCRIPT_PARAMETER_HIGHLIGHT_QUERY,
];
const JSX_HIGHLIGHT_QUERIES: &[&str] = &[
    tree_sitter_javascript::HIGHLIGHT_QUERY,
    JAVASCRIPT_PARAMETER_HIGHLIGHT_QUERY,
    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
];
const TYPESCRIPT_HIGHLIGHT_QUERIES: &[&str] = &[
    tree_sitter_javascript::HIGHLIGHT_QUERY,
    tree_sitter_typescript::HIGHLIGHTS_QUERY,
];
const TSX_HIGHLIGHT_QUERIES: &[&str] = &[
    tree_sitter_javascript::HIGHLIGHT_QUERY,
    tree_sitter_typescript::HIGHLIGHTS_QUERY,
    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
];

const MARKDOWN_HIGHLIGHT_QUERY: &str = r#"
(atx_heading
  (atx_h1_marker) @punctuation.definition.heading.markdown
  (inline) @heading.1.markdown)

(atx_heading
  (atx_h2_marker) @punctuation.definition.heading.markdown
  (inline) @heading.2.markdown)

(atx_heading
  (atx_h3_marker) @punctuation.definition.heading.markdown
  (inline) @heading.3.markdown)

(atx_heading
  (atx_h4_marker) @punctuation.definition.heading.markdown
  (inline) @heading.4.markdown)

(atx_heading
  (atx_h5_marker) @punctuation.definition.heading.markdown
  (inline) @heading.5.markdown)

(atx_heading
  (atx_h6_marker) @punctuation.definition.heading.markdown
  (inline) @heading.6.markdown)

(setext_heading
  (paragraph) @markup.heading.setext.1.markdown
  (setext_h1_underline) @punctuation.definition.heading.markdown)

(setext_heading
  (paragraph) @markup.heading.setext.2.markdown
  (setext_h2_underline) @punctuation.definition.heading.markdown)

[
  (list_marker_plus)
  (list_marker_minus)
  (list_marker_star)
  (list_marker_dot)
  (list_marker_parenthesis)
] @punctuation.definition.list.begin.markdown

[
  (indented_code_block)
  (fenced_code_block)
] @markup.raw.block.markdown

(fenced_code_block_delimiter) @punctuation.definition.raw.markdown

(link_destination) @markup.underline.link.markdown
(link_label) @constant.other.reference.link.markdown
(thematic_break) @meta.separator.markdown

[
  (block_continuation)
  (block_quote_marker)
] @punctuation.definition.quote.begin.markdown

(backslash_escape) @escape
"#;

const MARKDOWN_INJECTION_QUERY: &str = r#"
(fenced_code_block
  (info_string
    (language) @injection.language)
  (code_fence_content) @injection.content)
"#;

pub fn normalized_extension(file: &str) -> Option<String> {
    Path::new(file)
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use crate::{
        color::Color,
        theme::{parse_vscode_theme, Style, Theme, TokenStyle},
    };

    use super::*;

    fn highlighter() -> Highlighter {
        let theme = parse_vscode_theme("themes/mocha.json").unwrap();
        Highlighter::new(&theme).unwrap()
    }

    fn theme_with_markdown_textmate_scopes() -> Theme {
        let markdown_heading = Style {
            fg: Some(Color::Rgb {
                r: 139,
                g: 164,
                b: 176,
            }),
            ..Default::default()
        };
        let markdown_plain = Style {
            fg: Some(Color::Rgb {
                r: 197,
                g: 201,
                b: 199,
            }),
            ..Default::default()
        };

        Theme {
            token_styles: vec![
                TokenStyle {
                    name: None,
                    scope: vec!["markup.heading.markdown".to_string()],
                    style: markdown_heading,
                },
                TokenStyle {
                    name: None,
                    scope: vec!["punctuation.definition.list_item.markdown".to_string()],
                    style: markdown_plain,
                },
            ],
            ..Theme::default()
        }
    }

    fn theme_with_scopes(scopes: &[&str]) -> Theme {
        let style = Style {
            fg: Some(Color::Rgb {
                r: 139,
                g: 164,
                b: 176,
            }),
            ..Default::default()
        };

        Theme {
            token_styles: scopes
                .iter()
                .map(|scope| TokenStyle {
                    name: None,
                    scope: vec![(*scope).to_string()],
                    style: style.clone(),
                })
                .collect(),
            ..Theme::default()
        }
    }

    fn assert_token_highlighted(styles: &[StyleInfo], code: &str, token: &str) {
        let start = code.find(token).unwrap();
        let end = start + token.len();

        assert!(
            styles
                .iter()
                .any(|style| style.start <= start && style.end >= end),
            "`{token}` should be highlighted"
        );
    }

    fn effective_style_at(styles: &[StyleInfo], byte: usize) -> Option<&Style> {
        styles
            .iter()
            .filter(|span| span.start <= byte && byte < span.end)
            .map(|span| &span.style)
            .next_back()
    }

    fn markdown_injections(query_source: &str, code: &str) -> Vec<RawInjection> {
        let language: Language = tree_sitter_md::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(code, None).unwrap();
        let query = Query::new(&language, query_source).unwrap();
        collect_injections(&query, tree.root_node(), code)
    }

    #[test]
    fn repeated_highlighting_reuses_matching_language_and_source() {
        let mut highlighter = highlighter();
        let source = "fn greeting() { let value = 42; }";

        let first = highlighter.highlight("rust", source).unwrap();
        assert!(!first.is_empty());
        let second = highlighter.highlight("rust", source).unwrap();

        assert_eq!(first.len(), second.len());
        for (original, repeated) in first.iter().zip(&second) {
            assert_eq!(original.start, repeated.start);
            assert_eq!(original.end, repeated.end);
            assert_eq!(original.style, repeated.style);
        }
        assert_eq!(
            highlighter
                .cached_highlight
                .as_ref()
                .map(|cached| cached.code.as_str()),
            Some(source)
        );

        let changed = "fn greeting() { return; }";
        let updated = highlighter.highlight("rust", changed).unwrap();
        assert_token_highlighted(&updated, changed, "return");
        assert_eq!(
            highlighter
                .cached_highlight
                .as_ref()
                .map(|cached| cached.code.as_str()),
            Some(changed)
        );

        let other_language = highlighter.highlight("javascript", "return true;").unwrap();
        assert_token_highlighted(&other_language, "return true;", "true");
        assert_eq!(
            highlighter
                .cached_highlight
                .as_ref()
                .map(|cached| cached.language_id.as_str()),
            Some("javascript")
        );
    }

    #[test]
    fn background_language_preparation_preserves_styles_and_injections() {
        for (language, source) in [
            ("rust", "fn greeting(value: usize) -> usize { value }\n"),
            ("markdown", "# Example\n\n```rust\nfn greet() {}\n```\n"),
            ("yaml", "root:\n  enabled: true\n"),
        ] {
            let mut prepared = highlighter();
            let pending = prepared
                .prepare_language_in_background(language)
                .expect("tree-sitter language should be prepared");
            prepared.finish_prepared_language(pending);
            assert!(prepared.highlighters.contains_key(language));

            let actual = prepared.highlight(language, source).unwrap();
            let expected = highlighter().highlight(language, source).unwrap();
            let shape = |styles: &[StyleInfo]| {
                styles
                    .iter()
                    .map(|style| (style.start, style.end, style.style.clone()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(shape(&actual), shape(&expected), "{language}");
            assert!(prepared.prepare_language_in_background(language).is_none());
        }
        assert!(highlighter()
            .prepare_language_in_background("husk")
            .is_none());
        assert!(highlighter()
            .prepare_language_in_background("unknown")
            .is_none());
    }

    #[test]
    fn startup_injected_languages_resolve_aliases_deduplicate_and_stay_bounded() {
        let highlighter = highlighter();
        let source = concat!(
            "```unknown\nignored\n```\n",
            "  ```shell options\nprintf hi\n```\n",
            "```sh\ntrue\n```\n",
            "~~~pwsh\nGet-Item .\n~~~\n",
            "```rust\nfn main() {}\n```\n",
            "```yaml\nignored: true\n```\n",
        );
        assert_eq!(
            highlighter.startup_injected_language_ids("markdown", source),
            vec!["bash", "powershell", "rust"]
        );
        assert!(highlighter
            .startup_injected_language_ids("rust", source)
            .is_empty());
    }

    #[test]
    fn background_language_preparation_rejects_stale_registries_and_invalid_queries() {
        let mut stale = highlighter();
        let pending = stale.prepare_language_in_background("rust").unwrap();
        stale.registry = Arc::new(LanguageRegistry::bundled());
        stale.finish_prepared_language(pending);
        assert!(!stale.highlighters.contains_key("rust"));
        assert!(!stale
            .highlight("rust", "fn current() {}\n")
            .unwrap()
            .is_empty());

        let mut registry = LanguageRegistry::bundled();
        registry
            .languages
            .get_mut("rust")
            .unwrap()
            .highlight_queries = vec!["(unknown_node) @function".to_string()];
        let theme = parse_vscode_theme("themes/mocha.json").unwrap();
        let mut invalid = Highlighter::with_registry(&theme, Arc::new(registry)).unwrap();
        let pending = invalid.prepare_language_in_background("rust").unwrap();
        invalid.finish_prepared_language(pending);
        assert!(!invalid.highlighters.contains_key("rust"));
        assert!(invalid
            .highlight("rust", "fn original_error() {}\n")
            .is_err());
    }

    #[test]
    fn repeated_markdown_highlighting_preserves_nested_language_offsets() {
        let mut highlighter = highlighter();
        let source = "# Example\n\n```rust\nfn greeting() {}\n```\n";

        let first = highlighter.highlight("markdown", source).unwrap();
        let second = highlighter.highlight("markdown", source).unwrap();
        assert_eq!(first.len(), second.len());
        assert_token_highlighted(&second, source, "fn");
        for (original, repeated) in first.iter().zip(&second) {
            assert_eq!(original.start, repeated.start);
            assert_eq!(original.end, repeated.end);
            assert_eq!(original.style, repeated.style);
        }
    }

    #[test]
    fn incremental_highlighting_matches_cold_parses_across_unicode_and_context_changes() {
        for (language, sources) in [
            (
                "rust",
                vec![
                    "fn greet() { let value = \"café\"; }\n",
                    "fn greet() { let value = \"café 🦀\"; }\n",
                    "fn greet() { /* comment\nvalue */ let value = 42; }\n",
                    "fn greet() { return; }\n",
                ],
            ),
            (
                "yaml",
                vec![
                    "root:\n  title: café\n",
                    "root:\n  title: \"café 世界\"\n  nested:\n    enabled: true\n",
                    "root:\n  title: value\n",
                ],
            ),
            (
                "markdown",
                vec![
                    "# Example\n\n```rust\nfn greet() {}\n```\n",
                    "# Example 🦀\n\n```rust\nfn greet() { let value = 1; }\n```\n",
                    "# Example\n\n```javascript\nconst value = true;\n```\n",
                ],
            ),
        ] {
            let mut incremental = highlighter();
            for source in sources {
                let actual = incremental.highlight(language, source).unwrap();
                let expected = highlighter().highlight(language, source).unwrap();
                let shape = |styles: &[StyleInfo]| {
                    styles
                        .iter()
                        .map(|style| (style.start, style.end, style.style.clone()))
                        .collect::<Vec<_>>()
                };
                assert_eq!(shape(&actual), shape(&expected), "{language}: {source}");
            }
        }
    }

    #[test]
    fn rust_identifier_edits_reuse_captures_without_reparsing() {
        let sources = [
            "fn greeting(value: usize) -> usize { value + 1 }\n",
            "fn gλreeting(value: usize) -> usize { value + 1 }\n",
            "fn gλreeting(va世界lue: usize) -> usize { value + 1 }\n",
            "fn gλreeting(valλue: usize) -> usize { value + 1 }\n",
            "fn gλreeting(value: usize) -> usize { value + 1 }\n",
        ];
        let mut incremental = highlighter();
        incremental.highlight("rust", sources[0]).unwrap();

        for source in &sources[1..] {
            let actual = incremental.highlight("rust", source).unwrap();
            let expected = highlighter().highlight("rust", source).unwrap();
            let shape = |styles: &[StyleInfo]| {
                styles
                    .iter()
                    .map(|style| (style.start, style.end, style.style.clone()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(shape(&actual), shape(&expected), "{source}");
            assert!(
                incremental.highlighters["rust"]
                    .cached_tree
                    .as_ref()
                    .unwrap()
                    .tree
                    .root_node()
                    .has_changes(),
                "interior identifier edits should reuse the edited syntax tree: {source}"
            );
        }
    }

    #[test]
    fn bundled_identifier_edits_preserve_direct_and_fenced_parser_captures() {
        for (language, contents) in [
            (
                "javascript",
                "function retainedvalue(value) { return value; }\n",
            ),
            ("jsx", "function retainedvalue(value) { return value; }\n"),
            (
                "typescript",
                "function retainedvalue(value: string) { return value; }\n",
            ),
            (
                "tsx",
                "function retainedvalue(value: string) { return value; }\n",
            ),
            ("lua", "local retainedvalue = 123456789\n"),
            ("husk", "fn retainedvalue() { let value = 123456789; }\n"),
            ("toml", "retainedvalue = 123456789\n"),
            ("yaml", "retainedvalue: 123456789\n"),
            ("bash", "retainedvalue=123456789\n"),
            ("fish", "function retainedvalue\nend\n"),
            ("powershell", "$retainedvalue = 123456789; \"retained\"\n"),
        ] {
            for fenced in [false, true] {
                let before = if fenced {
                    format!(
                        "## heading\n\n```{language}\n{contents}```\n\n```rust\nfn sibling() {{}}\n```\n"
                    )
                } else {
                    contents.to_string()
                };
                let outer = if fenced { "markdown" } else { language };
                let mut incremental = highlighter();
                incremental.highlight(outer, &before).unwrap();
                let inserted = before.replace("retainedvalue", "retainxyedvalue");
                let deleted = inserted.replace("retainxyedvalue", "retainyedvalue");
                for source in [inserted, deleted] {
                    let actual = incremental.highlight(outer, &source).unwrap();
                    let expected = highlighter().highlight(outer, &source).unwrap();
                    let shape = |styles: &[StyleInfo]| {
                        styles
                            .iter()
                            .map(|style| (style.start, style.end, style.style.clone()))
                            .collect::<Vec<_>>()
                    };
                    assert_eq!(shape(&actual), shape(&expected), "{language}: {source}");
                    if outer != "husk" {
                        assert!(
                            incremental.highlighters[outer]
                                .cached_tree
                                .as_ref()
                                .unwrap()
                                .tree
                                .root_node()
                                .has_changes(),
                            "{language} outer identifier tree was not reused (fenced={fenced})"
                        );
                    }
                    if fenced && language != "husk" {
                        assert!(
                            incremental.highlighters[language]
                                .cached_tree
                                .as_ref()
                                .unwrap()
                                .tree
                                .root_node()
                                .has_changes(),
                            "{language} fenced identifier tree was not reused"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn identifier_keywords_builtins_case_and_custom_queries_reparse() {
        for (language, before, after) in [
            (
                "javascript",
                "function reurn() { return 1; }\n",
                "function return() { return 1; }\n",
            ),
            (
                "javascript",
                "function consle() { return 1; }\n",
                "function console() { return 1; }\n",
            ),
            (
                "javascript",
                "function console() { return 1; }\n",
                "function consxole() { return 1; }\n",
            ),
            (
                "jsx",
                "function retained() { return 1; }\n",
                "function retaiNed() { return 1; }\n",
            ),
            (
                "typescript",
                "function intrface() { return 1; }\n",
                "function interface() { return 1; }\n",
            ),
            ("lua", "local prnt = 123\n", "local print = 123\n"),
            ("lua", "local slf = 123\n", "local self = 123\n"),
            ("husk", "let bol = 123;\n", "let bool = 123;\n"),
            ("husk", "let retrn = 123;\n", "let return = 123;\n"),
            ("toml", "retainedvalue = 123\n", "retained.value = 123\n"),
            ("yaml", "tue: 123\n", "true: 123\n"),
            ("yaml", "key: retainedvalue\n", "key: retainxyedvalue\n"),
            ("bash", "retainedvalue=123\n", "retained_value=123\n"),
            ("fish", "function tst\nend\n", "function test\nend\n"),
            (
                "fish",
                "set retainedvalue 123\n",
                "set retainxyedvalue 123\n",
            ),
            ("powershell", "$tue = 123\n", "$true = 123\n"),
            (
                "powershell",
                "$global:retainedvalue = 123\n",
                "$global:retainxyedvalue = 123\n",
            ),
        ] {
            for fenced in [false, true] {
                let before = if fenced {
                    format!("## heading\n\n```{language}\n{before}```\n")
                } else {
                    before.to_string()
                };
                let after = if fenced {
                    format!("## heading\n\n```{language}\n{after}```\n")
                } else {
                    after.to_string()
                };
                let outer = if fenced { "markdown" } else { language };
                let mut incremental = highlighter();
                incremental.highlight(outer, &before).unwrap();
                let actual = incremental.highlight(outer, &after).unwrap();
                let expected = highlighter().highlight(outer, &after).unwrap();
                let shape = |styles: &[StyleInfo]| {
                    styles
                        .iter()
                        .map(|style| (style.start, style.end, style.style.clone()))
                        .collect::<Vec<_>>()
                };
                assert_eq!(shape(&actual), shape(&expected), "{language}: {after}");
                if outer != "husk" {
                    assert!(
                        !incremental.highlighters[outer]
                            .cached_tree
                            .as_ref()
                            .unwrap()
                            .tree
                            .root_node()
                            .has_changes(),
                        "{language} sensitive identifier must reparse (fenced={fenced})"
                    );
                }
            }
        }

        let mut registry = LanguageRegistry::bundled();
        registry
            .languages
            .get_mut("javascript")
            .unwrap()
            .highlight_queries
            .push("((identifier) @function (#match? @function \"x\"))".to_string());
        let theme = parse_vscode_theme("themes/mocha.json").unwrap();
        let mut customized = Highlighter::with_registry(&theme, Arc::new(registry)).unwrap();
        customized
            .highlight("javascript", "function retained() {}\n")
            .unwrap();
        customized
            .highlight("javascript", "function retaxined() {}\n")
            .unwrap();
        assert!(!customized.highlighters["javascript"]
            .cached_tree
            .as_ref()
            .unwrap()
            .tree
            .root_node()
            .has_changes());
    }

    #[test]
    fn rust_comment_and_string_edits_reuse_exact_fresh_parser_captures() {
        for sources in [
            vec![
                "// retained comment text\nfn value() {}\n",
                "// retained.! comment text\nfn value() {}\n",
                "// retained.! λ界 comment text\nfn value() {}\n",
                "// retained λ界 comment text\nfn value() {}\n",
            ],
            vec![
                "// retained comment text\r\nfn value() {}\r\n",
                "// retained.! comment text\r\nfn value() {}\r\n",
            ],
            vec![
                "fn value() { let text = \"retained string text\"; }\n",
                "fn value() { let text = \"retained.! string text\"; }\n",
                "fn value() { let text = \"retained.! λ界 string text\"; }\n",
                "fn value() { let text = \"retained λ界 string text\"; }\n",
            ],
            vec![
                "fn value() { let text = \"retained string text\"; }\r\n",
                "fn value() { let text = \"retained.! string text\"; }\r\n",
            ],
        ] {
            let mut incremental = highlighter();
            incremental.highlight("rust", sources[0]).unwrap();

            for source in &sources[1..] {
                let actual = incremental.highlight("rust", source).unwrap();
                let expected = highlighter().highlight("rust", source).unwrap();
                let shape = |styles: &[StyleInfo]| {
                    styles
                        .iter()
                        .map(|style| (style.start, style.end, style.style.clone()))
                        .collect::<Vec<_>>()
                };
                assert_eq!(shape(&actual), shape(&expected), "{source}");
                assert!(
                    incremental.highlighters["rust"]
                        .cached_tree
                        .as_ref()
                        .unwrap()
                        .tree
                        .root_node()
                        .has_changes(),
                    "safe Rust token edit should retain its existing syntax tree: {source}"
                );
            }
        }
    }

    #[test]
    fn rust_token_edits_reparse_documentation_boundaries_and_string_escapes() {
        for (before, after) in [
            (
                "// retained comment\nfn value() {}\n",
                "/// retained comment\nfn value() {}\n",
            ),
            (
                "// retained comment\nfn value() {}\n",
                "// retained\ncomment\nfn value() {}\n",
            ),
            (
                "fn value() { let text = \"retained string\"; }\n",
                "fn value() { let text = \"retained\\ string\"; }\n",
            ),
            (
                "fn value() { let text = \"retained string\"; }\n",
                "fn value() { let text = \"retained\" string\"; }\n",
            ),
            (
                "/* retained comment */\nfn value() {}\n",
                "/* retained.! comment */\nfn value() {}\n",
            ),
        ] {
            let mut incremental = highlighter();
            incremental.highlight("rust", before).unwrap();
            let actual = incremental.highlight("rust", after).unwrap();
            let expected = highlighter().highlight("rust", after).unwrap();
            let shape = |styles: &[StyleInfo]| {
                styles
                    .iter()
                    .map(|style| (style.start, style.end, style.style.clone()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(shape(&actual), shape(&expected), "{after}");
            assert!(
                !incremental.highlighters["rust"]
                    .cached_tree
                    .as_ref()
                    .unwrap()
                    .tree
                    .root_node()
                    .has_changes(),
                "grammar-changing Rust token edit must reparse: {after}"
            );
        }
    }

    #[test]
    fn bundled_decimal_digit_edits_preserve_direct_and_fenced_parser_captures() {
        for (language, contents) in [
            ("rust", "fn value() -> usize { 123456789 }\n"),
            ("javascript", "const value = 123456789;\n"),
            ("jsx", "const value = 123456789;\n"),
            ("typescript", "const value: number = 123456789;\n"),
            ("tsx", "const value: number = 123456789;\n"),
            ("json", "{\"value\": 123456789}\n"),
            ("toml", "value = 123456789\n"),
            ("yaml", "value: 123456789\n"),
            ("lua", "local value = 123456789\n"),
            ("powershell", "$value = 123456789; \"retained\"\n"),
            ("husk", "let value = 123456789;\n"),
        ] {
            for fenced in [false, true] {
                let before = if fenced {
                    format!("## heading\n\n```{language}\n{contents}```\n")
                } else {
                    contents.to_string()
                };
                let outer = if fenced { "markdown" } else { language };
                let mut incremental = highlighter();
                incremental.highlight(outer, &before).unwrap();
                let inserted = before.replace("123456789", "12347856789");
                let deleted = inserted.replace("12347856789", "1234756789");
                for source in [inserted, deleted] {
                    let actual = incremental.highlight(outer, &source).unwrap();
                    let expected = highlighter().highlight(outer, &source).unwrap();
                    let shape = |styles: &[StyleInfo]| {
                        styles
                            .iter()
                            .map(|style| (style.start, style.end, style.style.clone()))
                            .collect::<Vec<_>>()
                    };
                    assert_eq!(shape(&actual), shape(&expected), "{language}: {source}");
                    if outer != "husk" {
                        assert!(
                            incremental.highlighters[outer]
                                .cached_tree
                                .as_ref()
                                .unwrap()
                                .tree
                                .root_node()
                                .has_changes(),
                            "{language} outer numeric tree was not reused (fenced={fenced})"
                        );
                    }
                    if fenced && language != "husk" {
                        assert!(incremental.highlighters[language]
                            .cached_tree
                            .as_ref()
                            .unwrap()
                            .tree
                            .root_node()
                            .has_changes());
                    }
                }
            }
        }
    }

    #[test]
    fn numeric_grammar_boundaries_and_custom_queries_reparse() {
        for (language, before, after) in [
            ("rust", "let value = 123456;", "let value = 123.456;"),
            ("rust", "let value = 123456;", "let value = 123_456;"),
            ("rust", "let value = 0x123456;", "let value = 0x1237456;"),
            (
                "javascript",
                "const value = 123456;",
                "const value = 123e456;",
            ),
            (
                "javascript",
                "const value = 123456n;",
                "const value = 1237456n;",
            ),
            ("json", "{\"value\": 123456}", "{\"value\": 123.456}"),
            ("toml", "value = 123456\n", "value = 123_456\n"),
            ("yaml", "value: 123456\n", "value: 123.456\n"),
            ("lua", "local value = 0x123456", "local value = 0x1237456"),
            (
                "powershell",
                "$value = 123456; \"retained\"",
                "$value = 123.456; \"retained\"",
            ),
        ] {
            let mut incremental = highlighter();
            incremental.highlight(language, before).unwrap();
            let actual = incremental.highlight(language, after).unwrap();
            let expected = highlighter().highlight(language, after).unwrap();
            let shape = |styles: &[StyleInfo]| {
                styles
                    .iter()
                    .map(|style| (style.start, style.end, style.style.clone()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(shape(&actual), shape(&expected), "{language}: {after}");
            assert!(!incremental.highlighters[language]
                .cached_tree
                .as_ref()
                .unwrap()
                .tree
                .root_node()
                .has_changes());
        }

        let mut registry = LanguageRegistry::bundled();
        registry
            .languages
            .get_mut("javascript")
            .unwrap()
            .highlight_queries
            .push("((number) @function (#match? @function \"7\"))".to_string());
        let theme = parse_vscode_theme("themes/mocha.json").unwrap();
        let mut customized = Highlighter::with_registry(&theme, Arc::new(registry)).unwrap();
        customized
            .highlight("javascript", "const value = 123456;")
            .unwrap();
        customized
            .highlight("javascript", "const value = 1237456;")
            .unwrap();
        assert!(!customized.highlighters["javascript"]
            .cached_tree
            .as_ref()
            .unwrap()
            .tree
            .root_node()
            .has_changes());
    }

    #[test]
    fn bundled_javascript_and_json_token_edits_match_fresh_parser_captures() {
        for (language, sources) in [
            (
                "javascript",
                vec![
                    "// retained comment\nconst value = \"retained string\";\n",
                    "// retained.! λ comment\nconst value = \"retained string\";\n",
                    "// retained.! λ comment\nconst value = \"retained.! λ string\";\n",
                ],
            ),
            (
                "jsx",
                vec![
                    "// retained comment\nconst value = \"retained string\";\n",
                    "// retained.! λ comment\nconst value = \"retained string\";\n",
                    "// retained.! λ comment\nconst value = \"retained.! λ string\";\n",
                ],
            ),
            (
                "typescript",
                vec![
                    "// retained comment\r\nconst value: string = \"retained string\";\r\n",
                    "// retained.! λ comment\r\nconst value: string = \"retained string\";\r\n",
                    "// retained.! λ comment\r\nconst value: string = \"retained.! λ string\";\r\n",
                ],
            ),
            (
                "tsx",
                vec![
                    "// retained comment\nconst value: string = 'retained string';\n",
                    "// retained.! λ comment\nconst value: string = 'retained string';\n",
                    "// retained.! λ comment\nconst value: string = 'retained.! λ string';\n",
                ],
            ),
            (
                "json",
                vec![
                    "{\"retained key\": \"retained value\"}\n",
                    "{\"retained.! λ key\": \"retained value\"}\n",
                    "{\"retained.! λ key\": \"retained.! λ value\"}\n",
                ],
            ),
        ] {
            let mut incremental = highlighter();
            incremental.highlight(language, sources[0]).unwrap();

            for source in &sources[1..] {
                let actual = incremental.highlight(language, source).unwrap();
                let expected = highlighter().highlight(language, source).unwrap();
                let shape = |styles: &[StyleInfo]| {
                    styles
                        .iter()
                        .map(|style| (style.start, style.end, style.style.clone()))
                        .collect::<Vec<_>>()
                };
                assert_eq!(shape(&actual), shape(&expected), "{language}: {source}");
                assert!(
                    incremental.highlighters[language]
                        .cached_tree
                        .as_ref()
                        .unwrap()
                        .tree
                        .root_node()
                        .has_changes(),
                    "safe {language} token edit should reuse its existing syntax tree"
                );
            }
        }
    }

    #[test]
    fn specialized_husk_token_edits_match_fresh_lexer_captures() {
        for sources in [
            vec![
                "pub fn activate() { let value = \"retained string\"; } // retained comment\n",
                "pub fn activate() { let value = \"retained string\"; } // retained. λ comment\n",
                "pub fn activate() { let value = \"retained. λ string\"; } // retained. λ comment\n",
            ],
            vec![
                "pub fn activate() { let value = \"retained 世界 string\"; } // retained 世界 comment\r\n",
                "pub fn activate() { let value = \"retained 世界 string\"; } // retained. λ 世界 comment\r\n",
            ],
        ] {
            let mut incremental = highlighter();
            incremental.highlight("husk", sources[0]).unwrap();
            for source in &sources[1..] {
                let actual = incremental.highlight("husk", source).unwrap();
                let expected = highlighter().highlight("husk", source).unwrap();
                let shape = |styles: &[StyleInfo]| {
                    styles
                        .iter()
                        .map(|style| (style.start, style.end, style.style.clone()))
                        .collect::<Vec<_>>()
                };
                assert_eq!(shape(&actual), shape(&expected), "{source}");
            }
        }
    }

    #[test]
    fn specialized_git_commit_line_edits_preserve_semantic_fresh_parser_captures() {
        let cases = [
            (
                "retained subject\n# existing comment\n",
                "retained. λ subject\n# existing comment\n",
            ),
            (
                "# retained comment\n# next comment\n",
                "# retained. λ comment\n# next comment\n",
            ),
            (
                "# On branch retained_branch\n# modified: retained/path.rs\n",
                "# On branch retained.λ_branch\n# modified: retained/path.rs\n",
            ),
            (
                "# modified: retained/path.rs\r\n# +retained diff\r\n",
                "# modified: retained.λ/path.rs\r\n# +retained diff\r\n",
            ),
            ("# ordinary body\n", "# On branch ordinary body\n"),
            ("# +retained diff\n", "# -retained diff\n"),
            ("# plain path\n", "# 'plain' path\n"),
        ];
        for (before, after) in cases {
            let mut incremental = highlighter();
            incremental.highlight("gitcommit", before).unwrap();
            let actual = incremental.highlight("gitcommit", after).unwrap();
            let expected = highlighter().highlight("gitcommit", after).unwrap();
            let shape = |styles: &[StyleInfo]| {
                styles
                    .iter()
                    .map(|style| (style.start, style.end, style.style.clone()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(shape(&actual), shape(&expected), "{after}");
        }
    }

    #[test]
    fn specialized_husk_and_commit_syntax_boundaries_fall_back_safely() {
        for (language, before, after) in [
            (
                "husk",
                "let value = \"retained string\";\n",
                "let value = \"retained\\ string\";\n",
            ),
            (
                "husk",
                "let value = \"retained string\";\n",
                "let value = \"retained\" string\";\n",
            ),
            ("husk", "// retained comment\n", "// retained\n comment\n"),
            (
                "gitcommit",
                "# retained comment\n# later comment\n",
                "# retained\n comment\n# later comment\n",
            ),
        ] {
            let mut incremental = highlighter();
            incremental.highlight(language, before).unwrap();
            let actual = incremental.highlight(language, after).unwrap();
            let expected = highlighter().highlight(language, after).unwrap();
            let shape = |styles: &[StyleInfo]| {
                styles
                    .iter()
                    .map(|style| (style.start, style.end, style.style.clone()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(shape(&actual), shape(&expected), "{language}: {after}");
        }
    }

    #[test]
    fn javascript_and_json_syntax_changes_reparse_before_reusing_captures() {
        for (language, before, after) in [
            (
                "javascript",
                "// retained comment\nconst value = 1;\n",
                "// retained\n comment\nconst value = 1;\n",
            ),
            (
                "javascript",
                "// retained comment\nconst value = 1;\n",
                "// retained\u{2028} comment\nconst value = 1;\n",
            ),
            (
                "javascript",
                "/* retained comment */\nconst value = 1;\n",
                "/* retained.! comment */\nconst value = 1;\n",
            ),
            (
                "javascript",
                "const value = \"retained string\";\n",
                "const value = \"retained\\ string\";\n",
            ),
            (
                "typescript",
                "const value = `retained string`;\n",
                "const value = `retained.! string`;\n",
            ),
            (
                "json",
                "{\"key\": \"retained value\"}\n",
                "{\"key\": \"retained\\ value\"}\n",
            ),
        ] {
            let mut incremental = highlighter();
            incremental.highlight(language, before).unwrap();
            let actual = incremental.highlight(language, after).unwrap();
            let expected = highlighter().highlight(language, after).unwrap();
            let shape = |styles: &[StyleInfo]| {
                styles
                    .iter()
                    .map(|style| (style.start, style.end, style.style.clone()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(shape(&actual), shape(&expected), "{language}: {after}");
            assert!(
                !incremental.highlighters[language]
                    .cached_tree
                    .as_ref()
                    .unwrap()
                    .tree
                    .root_node()
                    .has_changes(),
                "grammar-changing {language} token edit must reparse"
            );
        }
    }

    #[test]
    fn configuration_and_shell_token_edits_match_fresh_parser_captures() {
        for (language, sources) in [
            (
                "toml",
                vec![
                    "key = \"retained string\" # retained comment\n",
                    "key = \"retained string\" # retained.! λ comment\n",
                    "key = \"retained.! λ string\" # retained.! λ comment\n",
                ],
            ),
            (
                "yaml",
                vec![
                    "---\nkey: \"retained string\" # retained comment\r\n",
                    "---\nkey: \"retained string\" # retained.! λ comment\r\n",
                    "---\nkey: \"retained.! λ string\" # retained.! λ comment\r\n",
                ],
            ),
            (
                "bash",
                vec![
                    "echo \"retained string\" # retained comment\n",
                    "echo \"retained string\" # retained.! λ comment\n",
                    "echo \"retained.! λ string\" # retained.! λ comment\n",
                ],
            ),
            (
                "fish",
                vec![
                    "echo \"retained string\" # retained comment\n",
                    "echo \"retained string\" # retained.! λ comment\n",
                    "echo \"retained.! λ string\" # retained.! λ comment\n",
                ],
            ),
            (
                "powershell",
                vec![
                    "$value = \"retained string\" # retained comment\n",
                    "$value = \"retained string\" # retained.! λ comment\n",
                    "$value = \"retained.! λ string\" # retained.! λ comment\n",
                ],
            ),
            (
                "lua",
                vec![
                    "local value = \"retained string\" -- retained comment\n",
                    "local value = \"retained string\" -- retained.! λ comment\n",
                    "local value = \"retained.! λ string\" -- retained.! λ comment\n",
                ],
            ),
        ] {
            let mut incremental = highlighter();
            incremental.highlight(language, sources[0]).unwrap();

            for source in &sources[1..] {
                let actual = incremental.highlight(language, source).unwrap();
                let expected = highlighter().highlight(language, source).unwrap();
                let shape = |styles: &[StyleInfo]| {
                    styles
                        .iter()
                        .map(|style| (style.start, style.end, style.style.clone()))
                        .collect::<Vec<_>>()
                };
                assert_eq!(shape(&actual), shape(&expected), "{language}: {source}");
                assert!(
                    incremental.highlighters[language]
                        .cached_tree
                        .as_ref()
                        .unwrap()
                        .tree
                        .root_node()
                        .has_changes(),
                    "safe {language} token edit should reuse its existing syntax tree"
                );
            }
        }
    }

    #[test]
    fn markdown_fenced_husk_tokens_preserve_fresh_specialized_lexer_captures() {
        for sources in [
            vec![
                "## outer heading\n\n```husk\npub fn value() { let text = \"retained string\"; } // retained comment\n```\n\n```rust\nfn sibling() {}\n```\n",
                "## outer heading\n\n```husk\npub fn value() { let text = \"retained string\"; } // retained. λ comment\n```\n\n```rust\nfn sibling() {}\n```\n",
                "## outer heading\n\n```husk\npub fn value() { let text = \"retained. λ string\"; } // retained. λ comment\n```\n\n```rust\nfn sibling() {}\n```\n",
            ],
            vec![
                "## outer 世界\r\n\r\n```hk\r\npub fn value() { let text = \"retained 世界 string\"; } // retained 世界 comment\r\n```\r\n",
                "## outer 世界\r\n\r\n```hk\r\npub fn value() { let text = \"retained 世界 string\"; } // retained. λ 世界 comment\r\n```\r\n",
                "## outer 世界\r\n\r\n```hk\r\npub fn value() { let text = \"retained. λ 世界 string\"; } // retained. λ 世界 comment\r\n```\r\n",
            ],
            vec![
                "```husk\n// retained comment\n```\n\n```husk\n// later comment\n```\n",
                "```husk\n// retained. λ comment\n```\n\n```husk\n// later comment\n```\n",
            ],
        ] {
            let mut incremental = highlighter();
            incremental.highlight("markdown", sources[0]).unwrap();
            for source in &sources[1..] {
                let actual = incremental.highlight("markdown", source).unwrap();
                let expected = highlighter().highlight("markdown", source).unwrap();
                let shape = |styles: &[StyleInfo]| {
                    styles
                        .iter()
                        .map(|style| (style.start, style.end, style.style.clone()))
                        .collect::<Vec<_>>()
                };
                assert_eq!(shape(&actual), shape(&expected), "{source}");
                assert!(incremental.highlighters["markdown"]
                    .cached_tree
                    .as_ref()
                    .unwrap()
                    .tree
                    .root_node()
                    .has_changes());
                assert!(!incremental.highlighters.contains_key("husk"));
            }
        }
    }

    #[test]
    fn markdown_fenced_husk_boundaries_and_custom_definitions_reparse() {
        for (before, after) in [
            (
                "```husk\n// retained comment\n```\n",
                "```husk\n// retained\n comment\n```\n",
            ),
            (
                "```husk\n// retained comment\n```\n",
                "```husk\n// retained` comment\n```\n",
            ),
            (
                "```husk\nlet value = \"retained string\";\n```\n",
                "```husk\nlet value = \"retained\\ string\";\n```\n",
            ),
            (
                "```husk\nlet value = \"retained string\";\n```\n",
                "```husk\nlet value = \"retained\" string\";\n```\n",
            ),
        ] {
            let mut incremental = highlighter();
            incremental.highlight("markdown", before).unwrap();
            let actual = incremental.highlight("markdown", after).unwrap();
            let expected = highlighter().highlight("markdown", after).unwrap();
            let shape = |styles: &[StyleInfo]| {
                styles
                    .iter()
                    .map(|style| (style.start, style.end, style.style.clone()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(shape(&actual), shape(&expected), "{after}");
            assert!(!incremental.highlighters["markdown"]
                .cached_tree
                .as_ref()
                .unwrap()
                .tree
                .root_node()
                .has_changes());
        }

        let mut registry = LanguageRegistry::bundled();
        registry
            .languages
            .get_mut("husk")
            .unwrap()
            .highlight_queries
            .push("customized specialized definition".to_string());
        let theme = parse_vscode_theme("themes/mocha.json").unwrap();
        let mut customized = Highlighter::with_registry(&theme, Arc::new(registry)).unwrap();
        customized
            .highlight("markdown", "```husk\n// retained comment\n```\n")
            .unwrap();
        customized
            .highlight("markdown", "```husk\n// retained. λ comment\n```\n")
            .unwrap();
        assert!(!customized.highlighters["markdown"]
            .cached_tree
            .as_ref()
            .unwrap()
            .tree
            .root_node()
            .has_changes());
    }

    #[test]
    fn markdown_fenced_tokens_preserve_fresh_outer_and_injected_captures() {
        for (language, contents) in [
            (
                "rust",
                "fn retained_name() { let value = \"retained string\"; } // retained comment\n",
            ),
            (
                "javascript",
                "const value = \"retained string\"; // retained comment\n",
            ),
            (
                "jsx",
                "const value = \"retained string\"; // retained comment\n",
            ),
            (
                "typescript",
                "const value: string = \"retained string\"; // retained comment\n",
            ),
            (
                "tsx",
                "const value: string = \"retained string\"; // retained comment\n",
            ),
            ("json", "{\"key\": \"retained string\"}\n"),
            ("toml", "key = \"retained string\" # retained comment\n"),
            ("yaml", "key: \"retained string\" # retained comment\n"),
            ("bash", "echo \"retained string\" # retained comment\n"),
            ("fish", "echo \"retained string\" # retained comment\n"),
            (
                "powershell",
                "$value = \"retained string\" # retained comment\n",
            ),
            (
                "lua",
                "local value = \"retained string\" -- retained comment\n",
            ),
        ] {
            let sibling = if language == "rust" {
                "```javascript\nconst sibling = true;\n```\n"
            } else {
                "```rust\nfn sibling() {}\n```\n"
            };
            let original = format!("## outer heading\n\n```{language}\n{contents}```\n\n{sibling}");
            let mut sources = Vec::new();
            if contents.contains("retained comment") {
                sources.push(original.replace("retained comment", "retained. λ comment"));
            }
            let previous = sources.last().unwrap_or(&original);
            sources.push(previous.replace("retained string", "retained. λ string"));
            if language == "rust" {
                let previous = sources.last().unwrap();
                sources.push(previous.replace("retained_name", "retainλed_name"));
            }

            let mut incremental = highlighter();
            incremental.highlight("markdown", &original).unwrap();
            for source in sources {
                let actual = incremental.highlight("markdown", &source).unwrap();
                let expected = highlighter().highlight("markdown", &source).unwrap();
                let shape = |styles: &[StyleInfo]| {
                    styles
                        .iter()
                        .map(|style| (style.start, style.end, style.style.clone()))
                        .collect::<Vec<_>>()
                };
                assert_eq!(shape(&actual), shape(&expected), "{language}: {source}");
                for id in ["markdown", language] {
                    assert!(
                        incremental.highlighters[id]
                            .cached_tree
                            .as_ref()
                            .unwrap()
                            .tree
                            .root_node()
                            .has_changes(),
                        "safe fenced {language} edit should retain the {id} syntax tree"
                    );
                }
            }
        }

        let before = "## outer 世界\r\n\r\n```rust\r\n// retained 世界 comment\r\n```\r\n";
        let after = before.replace("retained 世界", "retained. λ 世界");
        let mut incremental = highlighter();
        incremental.highlight("markdown", before).unwrap();
        let actual = incremental.highlight("markdown", &after).unwrap();
        let expected = highlighter().highlight("markdown", &after).unwrap();
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(expected) {
            assert_eq!((actual.start, actual.end), (expected.start, expected.end));
            assert_eq!(actual.style, expected.style);
        }
    }

    #[test]
    fn markdown_fenced_structural_edits_and_duplicate_trees_reparse() {
        for (before, after) in [
            (
                "```rust\n// retained comment\n```\n",
                "```rust\n// retained\n comment\n```\n",
            ),
            (
                "```rust\n// retained comment\n```\n",
                "```rust\n// retained` comment\n```\n",
            ),
            (
                "```rust\n// retained comment\n```\n",
                "```rust\n// retained~ comment\n```\n",
            ),
            (
                "```rust\nlet value = \"retained string\";\n```\n",
                "```rust\nlet value = \"retained\\ string\";\n```\n",
            ),
            (
                "```bash\necho \"retained string\"\n```\n",
                "```bash\necho \"retained$ string\"\n```\n",
            ),
            (
                "```rust\n// retained comment\n```\n\n```rust\nfn later() {}\n```\n",
                "```rust\n// retained. λ comment\n```\n\n```rust\nfn later() {}\n```\n",
            ),
            (
                "```rust\n// retained comment\n```\n",
                "```javascript\n// retained comment\n```\n",
            ),
        ] {
            let mut incremental = highlighter();
            incremental.highlight("markdown", before).unwrap();
            let actual = incremental.highlight("markdown", after).unwrap();
            let expected = highlighter().highlight("markdown", after).unwrap();
            let shape = |styles: &[StyleInfo]| {
                styles
                    .iter()
                    .map(|style| (style.start, style.end, style.style.clone()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(shape(&actual), shape(&expected), "{after}");
            assert!(
                !incremental.highlighters["markdown"]
                    .cached_tree
                    .as_ref()
                    .unwrap()
                    .tree
                    .root_node()
                    .has_changes(),
                "fenced structural or ambiguous edit must reparse: {after}"
            );
        }
    }

    #[test]
    fn markdown_fenced_custom_injected_queries_reparse_before_styling() {
        let mut registry = LanguageRegistry::bundled();
        registry
            .languages
            .get_mut("javascript")
            .unwrap()
            .highlight_queries
            .push("((comment) @function (#match? @function \"λ\"))".to_string());
        let registry = Arc::new(registry);
        let theme = parse_vscode_theme("themes/mocha.json").unwrap();
        let mut customized = Highlighter::with_registry(&theme, Arc::clone(&registry)).unwrap();
        customized
            .highlight("markdown", "```javascript\n// retained comment\n```\n")
            .unwrap();

        let source = "```javascript\n// retained λ comment\n```\n";
        let actual = customized.highlight("markdown", source).unwrap();
        let expected = Highlighter::with_registry(&theme, registry)
            .unwrap()
            .highlight("markdown", source)
            .unwrap();
        let shape = |styles: &[StyleInfo]| {
            styles
                .iter()
                .map(|style| (style.start, style.end, style.style.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(shape(&actual), shape(&expected));
        assert!(!customized.highlighters["markdown"]
            .cached_tree
            .as_ref()
            .unwrap()
            .tree
            .root_node()
            .has_changes());
    }

    #[test]
    fn markdown_heading_edits_preserve_fresh_captures_and_fenced_injections() {
        for sources in [
            vec!["# retained heading\n", "# retained. λ heading\n"],
            vec!["## retained heading\n", "## retained. λ heading\n"],
            vec!["### retained heading\n", "### retained. λ heading\n"],
            vec![
                "## retained 世界😀 heading\r\n",
                "## retained. λ 世界😀 heading\r\n",
            ],
            vec![
                "## retained heading\n\n```rust\nfn greeting() {}\n```\n\n```javascript\nconst value = true;\n```\n",
                "## retained. λ heading\n\n```rust\nfn greeting() {}\n```\n\n```javascript\nconst value = true;\n```\n",
                "## retained. λ. λ heading\n\n```rust\nfn greeting() {}\n```\n\n```javascript\nconst value = true;\n```\n",
            ],
        ] {
            let mut incremental = highlighter();
            incremental.highlight("markdown", sources[0]).unwrap();

            for source in &sources[1..] {
                let actual = incremental.highlight("markdown", source).unwrap();
                let expected = highlighter().highlight("markdown", source).unwrap();
                let shape = |styles: &[StyleInfo]| {
                    styles
                        .iter()
                        .map(|style| (style.start, style.end, style.style.clone()))
                        .collect::<Vec<_>>()
                };
                assert_eq!(shape(&actual), shape(&expected), "{source}");
                assert!(incremental.highlighters["markdown"]
                    .cached_tree
                    .as_ref()
                    .unwrap()
                    .tree
                    .root_node()
                    .has_changes());
            }
        }
    }

    #[test]
    fn markdown_structural_and_fenced_edits_reparse_before_reusing_captures() {
        for (before, after) in [
            ("## retained heading\n", "## retained# heading\n"),
            ("## retained heading\n", "## retained` heading\n"),
            ("## retained heading\n", "## retained\\ heading\n"),
            ("## retained heading\n", "## retained\n heading\n"),
            ("## retained heading\n", "### retained heading\n"),
            (
                "## retained heading\n\n```rust\nfn greeting() {}\n```\n",
                "## retained heading\n\n```rust\nfn greet.ing() {}\n```\n",
            ),
        ] {
            let mut incremental = highlighter();
            incremental.highlight("markdown", before).unwrap();
            let actual = incremental.highlight("markdown", after).unwrap();
            let expected = highlighter().highlight("markdown", after).unwrap();
            let shape = |styles: &[StyleInfo]| {
                styles
                    .iter()
                    .map(|style| (style.start, style.end, style.style.clone()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(shape(&actual), shape(&expected), "{after}");
            assert!(!incremental.highlighters["markdown"]
                .cached_tree
                .as_ref()
                .unwrap()
                .tree
                .root_node()
                .has_changes());
        }

        let mut registry = LanguageRegistry::bundled();
        registry
            .languages
            .get_mut("markdown")
            .unwrap()
            .injection_query = Some(format!("{MARKDOWN_INJECTION_QUERY}\n"));
        let theme = parse_vscode_theme("themes/mocha.json").unwrap();
        let mut customized = Highlighter::with_registry(&theme, Arc::new(registry)).unwrap();
        customized
            .highlight("markdown", "## retained heading\n")
            .unwrap();
        customized
            .highlight("markdown", "## retained λ heading\n")
            .unwrap();
        assert!(!customized.highlighters["markdown"]
            .cached_tree
            .as_ref()
            .unwrap()
            .tree
            .root_node()
            .has_changes());
    }

    #[test]
    fn configuration_and_shell_syntax_changes_reparse_before_reusing_captures() {
        for (language, before, after) in [
            (
                "toml",
                "key = \"\"\"retained string\"\"\"\n",
                "key = \"\"\"retained.! string\"\"\"\n",
            ),
            (
                "toml",
                "key = \"retained string\" # retained comment\n",
                "key = \"retained string\" # retained\n comment\n",
            ),
            (
                "yaml",
                "---\nkey: \"retained string\"\n",
                "---\nkey: \"retained\\ string\"\n",
            ),
            (
                "bash",
                "echo \"retained string\"\n",
                "echo \"retained$ string\"\n",
            ),
            (
                "bash",
                "echo \"retained string\"\n",
                "echo \"retained` string\"\n",
            ),
            (
                "fish",
                "echo \"retained string\"\n",
                "echo \"retained$ string\"\n",
            ),
            (
                "powershell",
                "$value = \"retained string\"\n",
                "$value = \"retained$ string\"\n",
            ),
            (
                "powershell",
                "$value = \"retained string\"\n",
                "$value = \"retained` string\"\n",
            ),
            (
                "lua",
                "--[[ retained comment ]]\nlocal value = 1\n",
                "--[[ retained.! comment ]]\nlocal value = 1\n",
            ),
            (
                "lua",
                "local value = \"retained string\"\n",
                "local value = \"retained\\ string\"\n",
            ),
        ] {
            let mut incremental = highlighter();
            incremental.highlight(language, before).unwrap();
            let actual = incremental.highlight(language, after).unwrap();
            let expected = highlighter().highlight(language, after).unwrap();
            let shape = |styles: &[StyleInfo]| {
                styles
                    .iter()
                    .map(|style| (style.start, style.end, style.style.clone()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(shape(&actual), shape(&expected), "{language}: {after}");
            assert!(
                !incremental.highlighters[language]
                    .cached_tree
                    .as_ref()
                    .unwrap()
                    .tree
                    .root_node()
                    .has_changes(),
                "grammar-changing {language} token edit must reparse"
            );
        }
    }

    #[test]
    fn customized_javascript_queries_reparse_interior_token_edits() {
        let mut registry = LanguageRegistry::bundled();
        registry
            .languages
            .get_mut("javascript")
            .unwrap()
            .highlight_queries
            .push("((comment) @function (#match? @function \"λ\"))".to_string());
        let registry = Arc::new(registry);
        let theme = parse_vscode_theme("themes/mocha.json").unwrap();
        let mut customized = Highlighter::with_registry(&theme, Arc::clone(&registry)).unwrap();
        customized
            .highlight("javascript", "// retained comment\n")
            .unwrap();

        let source = "// retained λ comment\n";
        let actual = customized.highlight("javascript", source).unwrap();
        let expected = Highlighter::with_registry(&theme, registry)
            .unwrap()
            .highlight("javascript", source)
            .unwrap();
        let shape = |styles: &[StyleInfo]| {
            styles
                .iter()
                .map(|style| (style.start, style.end, style.style.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(shape(&actual), shape(&expected));
        assert!(!customized.highlighters["javascript"]
            .cached_tree
            .as_ref()
            .unwrap()
            .tree
            .root_node()
            .has_changes());
    }

    #[test]
    fn rust_identifier_edits_reuse_captures_inside_real_editor_viewports() {
        let mut source = include_str!("editor.rs")
            .lines()
            .skip(60)
            .take(80)
            .collect::<Vec<_>>()
            .join("\n");
        source.push('\n');
        let insertion = source.find("\nuse crate::{").unwrap() + 1;
        let mut incremental = highlighter();
        incremental.highlight("rust", &source).unwrap();
        source.insert(insertion, 'a');
        incremental.highlight("rust", &source).unwrap();
        for (offset, character) in ['λ', 'a', 'λ', 'a'].into_iter().enumerate() {
            let cursor = insertion
                + source[insertion..]
                    .char_indices()
                    .nth(offset + 1)
                    .unwrap()
                    .0;
            source.insert(cursor, character);
            incremental.highlight("rust", &source).unwrap();
            assert!(
                incremental.highlighters["rust"]
                    .cached_tree
                    .as_ref()
                    .unwrap()
                    .tree
                    .root_node()
                    .has_changes(),
                "real Rust viewport identifier edit should reuse existing captures"
            );
        }
    }

    #[test]
    fn rust_identifier_edits_reparse_keywords_boundaries_and_custom_queries() {
        let cases = [
            (
                "fn example() { let usae = 1; }\n",
                "fn example() { let use = 1; }\n",
            ),
            (
                "fn example() { let value = 1; }\n",
                "fn example() { let value! = 1; }\n",
            ),
            (
                "fn Example() { let value = 1; }\n",
                "fn Exλample() { let value = 1; }\n",
            ),
        ];

        for (before, after) in cases {
            let mut incremental = highlighter();
            incremental.highlight("rust", before).unwrap();
            let actual = incremental.highlight("rust", after).unwrap();
            let expected = highlighter().highlight("rust", after).unwrap();
            let shape = |styles: &[StyleInfo]| {
                styles
                    .iter()
                    .map(|style| (style.start, style.end, style.style.clone()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(shape(&actual), shape(&expected), "{after}");
            assert!(
                !incremental.highlighters["rust"]
                    .cached_tree
                    .as_ref()
                    .unwrap()
                    .tree
                    .root_node()
                    .has_changes(),
                "syntax-changing identifier edits must reparse: {after}"
            );
        }

        let mut registry = LanguageRegistry::bundled();
        registry
            .languages
            .get_mut("rust")
            .unwrap()
            .highlight_queries
            .push("((identifier) @function (#match? @function \"λ\"))".to_string());
        let registry = Arc::new(registry);
        let theme = parse_vscode_theme("themes/mocha.json").unwrap();
        let mut customized = Highlighter::with_registry(&theme, Arc::clone(&registry)).unwrap();
        customized.highlight("rust", "fn greeting() {}\n").unwrap();
        let actual = customized.highlight("rust", "fn gλreeting() {}\n").unwrap();
        let expected = Highlighter::with_registry(&theme, registry)
            .unwrap()
            .highlight("rust", "fn gλreeting() {}\n")
            .unwrap();
        assert_eq!(actual.len(), expected.len());
        assert!(!customized.highlighters["rust"]
            .cached_tree
            .as_ref()
            .unwrap()
            .tree
            .root_node()
            .has_changes());
    }

    #[test]
    fn oversized_highlight_requests_do_not_retain_source_or_styles() {
        let mut highlighter = highlighter();
        highlighter.highlight("rust", "fn cached() {}").unwrap();
        assert!(highlighter.cached_highlight.is_some());

        let source = format!("// {}", "a".repeat(MAX_CACHED_HIGHLIGHT_BYTES));
        highlighter.highlight("rust", &source).unwrap();
        assert!(highlighter.cached_highlight.is_none());
        assert!(highlighter.highlighters["rust"].cached_tree.is_none());
    }

    #[test]
    fn resolves_language_by_file_extension() {
        let highlighter = highlighter();

        assert_eq!(
            highlighter.language_id_for_file(Some("main.rs")),
            Some("rust")
        );
        assert_eq!(
            highlighter.language_id_for_file(Some("README.MD")),
            Some("markdown")
        );
        assert_eq!(
            highlighter.language_id_for_file(Some("component.tsx")),
            Some("tsx")
        );
        assert_eq!(
            highlighter.language_id_for_file(Some("component.jsx")),
            Some("jsx")
        );
        assert_eq!(
            highlighter.language_id_for_file(Some("config.yml")),
            Some("yaml")
        );
        assert_eq!(
            highlighter.language_id_for_file(Some("script.sh")),
            Some("bash")
        );
        assert_eq!(
            highlighter.language_id_for_file(Some("config.fish")),
            Some("fish")
        );
        assert_eq!(
            highlighter.language_id_for_file(Some("bootstrap.ps1")),
            Some("powershell")
        );
        assert_eq!(
            highlighter.language_id_for_file(Some("theme.lua")),
            Some("lua")
        );
        assert_eq!(
            highlighter.language_id_for_file(Some("plugin.hk")),
            Some("husk")
        );
        assert_eq!(
            highlighter.language_id_for_file(Some("[Git Commit].gitcommit")),
            Some("gitcommit")
        );
        assert_eq!(
            highlighter.language_id_for_file(Some(".git/COMMIT_EDITMSG")),
            Some("gitcommit")
        );
        assert_eq!(
            highlighter.language_id_for_file(Some(".git/MERGE_MSG")),
            Some("gitcommit")
        );
        assert_eq!(
            highlighter.language_id_for_name("commit"),
            Some("gitcommit")
        );
        assert_eq!(highlighter.language_id_for_file(Some("main.py")), None);
        assert_eq!(highlighter.language_id_for_file(Some("LICENSE")), None);
    }

    #[test]
    fn git_commit_highlighter_styles_comments_and_semantic_details() {
        let theme = theme_with_scopes(&[
            "comment",
            "markup.heading",
            "string",
            "keyword",
            "markup.inserted.diff",
            "markup.deleted.diff",
            "markup.changed.diff",
        ]);
        let mut highlighter = Highlighter::new(&theme).unwrap();
        let code = "feat(git): keep café readable\n\n\
# --- Red commit context (not part of the commit message) ---\n\
# On branch fëat/commit-style\n\
# Changes to be committed:\n\
#   A  src/café.rs\n\
# diff --git a/src/café.rs b/src/café.rs\n\
# +let ready = true;\n\
# -let ready = false;\n";

        let styles = highlighter.highlight("gitcommit", code).unwrap();

        assert!(effective_style_at(&styles, 0).is_none());
        for token in [
            "# --- Red commit context (not part of the commit message) ---",
            "fëat/commit-style",
            "Changes to be committed:",
            "A",
            "src/café.rs",
            "diff --git a/src/café.rs b/src/café.rs",
            "+let ready = true;",
            "-let ready = false;",
        ] {
            assert_token_highlighted(&styles, code, token);
        }

        for token in [
            "fëat/commit-style",
            "src/café.rs",
            "+let ready = true;",
            "-let ready = false;",
        ] {
            let start = code.find(token).unwrap();
            assert!(
                styles
                    .iter()
                    .any(|style| style.start == start && style.end == start + token.len()),
                "`{token}` should have a precise semantic highlight span"
            );
        }
    }

    #[test]
    fn configurable_languages_share_bundled_grammars_aliases_and_exact_filenames() {
        let directory = tempfile::tempdir().unwrap();
        let languages = HashMap::from([(
            "buildspec".to_string(),
            LanguageConfig {
                extensions: vec!["build".to_string()],
                filenames: vec!["Buildfile".to_string()],
                aliases: vec!["build-script".to_string()],
                grammar: Some(LanguageGrammarConfig {
                    builtin: Some("rust".to_string()),
                    ..LanguageGrammarConfig::default()
                }),
                ..LanguageConfig::default()
            },
        )]);
        let registry =
            Arc::new(LanguageRegistry::from_config(&languages, directory.path()).unwrap());
        let theme = theme_with_scopes(&["keyword", "function", "string"]);
        let mut highlighter = Highlighter::with_registry(&theme, registry).unwrap();

        assert_eq!(
            highlighter.language_id_for_file(Some("project/Buildfile")),
            Some("buildspec")
        );
        assert_eq!(
            highlighter.language_id_for_file(Some("project/buildfile")),
            None
        );
        assert_eq!(
            highlighter.language_id_for_file(Some("project/example.BUILD")),
            Some("buildspec")
        );
        assert_eq!(
            highlighter.language_id_for_name("BUILD-SCRIPT"),
            Some("buildspec")
        );
        assert!(!highlighter
            .highlight_for_file(Some("Buildfile"), "fn main() {}")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn configurable_languages_can_replace_bundled_structural_queries() {
        let directory = tempfile::tempdir().unwrap();
        let query_path = directory.path().join("textobjects.scm");
        let query_source = "(function_item) @function.outer";
        fs::write(&query_path, query_source).unwrap();
        let languages = HashMap::from([(
            "buildspec".to_string(),
            LanguageConfig {
                grammar: Some(LanguageGrammarConfig {
                    builtin: Some("rust".to_string()),
                    textobjects: vec![query_path],
                    ..LanguageGrammarConfig::default()
                }),
                ..LanguageConfig::default()
            },
        )]);

        let registry = LanguageRegistry::from_config(&languages, directory.path()).unwrap();
        let (_, loaded_query) = registry.textobject_language("buildspec").unwrap();

        assert_eq!(loaded_query, query_source);
    }

    #[test]
    fn configurable_indentation_queries_inherit_override_and_validate() {
        let directory = tempfile::tempdir().unwrap();
        let query_path = directory.path().join("indents.scm");
        let mut definition = LanguageConfig {
            grammar: Some(LanguageGrammarConfig {
                builtin: Some("rust".into()),
                ..LanguageGrammarConfig::default()
            }),
            ..LanguageConfig::default()
        };
        let mut languages = HashMap::from([("buildspec".to_string(), definition.clone())]);
        let inherited = LanguageRegistry::from_config(&languages, directory.path()).unwrap();
        assert!(inherited
            .indentation_language("buildspec")
            .unwrap()
            .1
            .contains("@indent.begin"));
        fs::write(&query_path, "(line_comment) @indent.ignore").unwrap();
        definition.grammar.as_mut().unwrap().indents = vec![query_path.clone()];
        languages.insert("buildspec".into(), definition);
        let registry = LanguageRegistry::from_config(&languages, directory.path()).unwrap();
        assert_eq!(
            registry.indentation_language("buildspec").unwrap().1,
            "(line_comment) @indent.ignore"
        );
        fs::write(&query_path, "(block) @indent.unsupported").unwrap();
        assert!(LanguageRegistry::from_config(&languages, directory.path()).is_err());
    }

    #[test]
    fn older_language_definitions_receive_compatible_structural_fallbacks() {
        let directory = tempfile::tempdir().unwrap();
        let mut registry = LanguageRegistry::bundled();
        registry
            .languages
            .get_mut("json")
            .unwrap()
            .textobject_queries
            .clear();

        registry
            .insert_configured("json", &LanguageConfig::default(), directory.path())
            .unwrap();

        let (_, query) = registry.textobject_language("json").unwrap();
        assert!(query.contains("@comment.outer"));
    }

    #[test]
    fn incompatible_structural_fallbacks_do_not_quarantine_a_language() {
        let directory = tempfile::tempdir().unwrap();
        let mut registry = LanguageRegistry::bundled();
        let mut definition = registry.languages.get("rust").unwrap().clone();
        definition.id = "c".to_string();
        definition.extensions = vec!["c".to_string()];
        definition.textobject_queries.clear();
        registry.insert(definition);

        registry
            .insert_configured("c", &LanguageConfig::default(), directory.path())
            .unwrap();

        assert!(registry.languages.contains_key("c"));
        assert!(registry.textobject_language("c").is_none());
        assert_eq!(registry.extensions.get("c").map(String::as_str), Some("c"));
    }

    #[test]
    fn configurable_languages_reject_invalid_structural_queries_and_predicates() {
        let directory = tempfile::tempdir().unwrap();
        let query_path = directory.path().join("textobjects.scm");
        let languages = HashMap::from([(
            "buildspec".to_string(),
            LanguageConfig {
                grammar: Some(LanguageGrammarConfig {
                    builtin: Some("rust".to_string()),
                    textobjects: vec![query_path.clone()],
                    ..LanguageGrammarConfig::default()
                }),
                ..LanguageConfig::default()
            },
        )]);

        fs::write(&query_path, "(missing_node) @function.outer").unwrap();
        let error = LanguageRegistry::from_config(&languages, directory.path())
            .err()
            .expect("invalid structural queries must be rejected");
        assert!(error.to_string().contains("invalid text-object query"));

        fs::write(
            &query_path,
            "((function_item) @function.outer (#offset! @function.outer 0 0 0 0))",
        )
        .unwrap();
        let error = LanguageRegistry::from_config(&languages, directory.path())
            .err()
            .expect("unsupported structural predicates must be rejected");
        assert!(error
            .to_string()
            .contains("unsupported text-object predicate"));
    }

    #[test]
    fn language_without_a_grammar_remains_available_without_highlighting() {
        let directory = tempfile::tempdir().unwrap();
        let languages = HashMap::from([(
            "plain-custom".to_string(),
            LanguageConfig {
                filenames: vec!["Customfile".to_string()],
                ..LanguageConfig::default()
            },
        )]);
        let registry =
            Arc::new(LanguageRegistry::from_config(&languages, directory.path()).unwrap());
        let mut highlighter = Highlighter::with_registry(&Theme::default(), registry).unwrap();

        assert_eq!(
            highlighter.language_id_for_file(Some("Customfile")),
            Some("plain-custom")
        );
        assert!(highlighter
            .highlight_for_file(Some("Customfile"), "hello")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn failed_incremental_language_insertion_preserves_accepted_definitions() {
        let directory = tempfile::tempdir().unwrap();
        let mut registry = LanguageRegistry::bundled();
        registry
            .insert_configured(
                "accepted",
                &LanguageConfig {
                    extensions: vec!["accepted".to_string()],
                    grammar: Some(LanguageGrammarConfig {
                        builtin: Some("rust".to_string()),
                        ..LanguageGrammarConfig::default()
                    }),
                    ..LanguageConfig::default()
                },
                directory.path(),
            )
            .unwrap();

        let error = registry
            .insert_configured(
                "rejected",
                &LanguageConfig {
                    grammar: Some(LanguageGrammarConfig {
                        builtin: Some("missing".to_string()),
                        ..LanguageGrammarConfig::default()
                    }),
                    ..LanguageConfig::default()
                },
                directory.path(),
            )
            .unwrap_err();

        assert!(error.to_string().contains("unknown bundled grammar"));
        assert!(registry.languages.contains_key("accepted"));
        assert!(!registry.languages.contains_key("rejected"));
        assert_eq!(
            registry.extensions.get("accepted").map(String::as_str),
            Some("accepted")
        );
    }

    #[test]
    fn unapproved_native_grammar_is_rejected_before_dynamic_loading() {
        let directory = tempfile::tempdir().unwrap();
        let grammar = directory.path().join("untrusted.so");
        fs::write(&grammar, b"not a dynamic library").unwrap();
        let languages = HashMap::from([(
            "unsafe-example".to_string(),
            LanguageConfig {
                extensions: vec!["unsafe".to_string()],
                grammar: Some(LanguageGrammarConfig {
                    path: Some(grammar),
                    ..LanguageGrammarConfig::default()
                }),
                ..LanguageConfig::default()
            },
        )]);

        let error = match LanguageRegistry::from_config(&languages, directory.path()) {
            Ok(_) => panic!("unapproved native grammar must not be loaded"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("not approved"));
    }

    #[test]
    fn yaml_requires_document_prefix_for_highlighting() {
        let highlighter = highlighter();

        assert!(highlighter.requires_document_prefix(Some("yaml")));
        assert!(!highlighter.requires_document_prefix(Some("rust")));
        assert!(!highlighter.requires_document_prefix(Some("markdown")));
        assert!(!highlighter.requires_document_prefix(None));
    }

    #[test]
    fn yaml_distinguishes_properties_values_escapes_and_directives() {
        let theme = parse_vscode_theme("themes/red.json").unwrap();
        let property = theme.get_style("property").unwrap();
        let string = theme.get_style("string").unwrap();
        let escape = theme.get_style("escape").unwrap();
        let directive = theme.get_style("keyword.directive").unwrap();
        assert_ne!(property.fg, string.fg);

        let mut highlighter = Highlighter::new(&theme).unwrap();
        let code = "%YAML 1.2\n---\nname: example\nescaped: \"line\\n\"\n";
        let styles = highlighter.highlight("yaml", code).unwrap();

        assert_eq!(
            effective_style_at(&styles, code.find("name").unwrap()),
            Some(&property)
        );
        assert_eq!(
            effective_style_at(&styles, code.find("example").unwrap()),
            Some(&string)
        );
        assert_eq!(
            effective_style_at(&styles, code.find("\\n").unwrap()),
            Some(&escape)
        );
        assert_eq!(effective_style_at(&styles, 0), Some(&directive));
    }

    #[test]
    fn highlights_supported_languages() {
        let samples = [
            ("rust", "fn main() { let value = true; }\n"),
            ("markdown", "# Heading\n\n```rust\nfn main() {}\n```\n"),
            ("javascript", "const value = true;\n"),
            ("jsx", "export const View = () => <div />;\n"),
            ("typescript", "const value: boolean = true;\n"),
            ("tsx", "export const View = () => <div />;\n"),
            ("json", r#"{"value": true}"#),
            ("toml", "value = true\n"),
            ("yaml", "value: true\n"),
            ("bash", "if [ -f Cargo.toml ]; then\n  echo yes\nfi\n"),
            (
                "fish",
                "function greet --argument-names name\n    echo \"hello, $name\"\nend\n",
            ),
            (
                "powershell",
                "function Invoke-Greeting { param([string]$Name) Write-Host \"Hello $Name\" }\n",
            ),
            (
                "lua",
                "local function greet(name) return 'hello ' .. name end\n",
            ),
            (
                "husk",
                "pub fn activate() { red::add_command(\"Hello\", hello); }\n",
            ),
        ];
        let mut highlighter = highlighter();

        for (language_id, code) in samples {
            let styles = highlighter.highlight(language_id, code).unwrap();
            assert!(
                !styles.is_empty(),
                "{language_id} should produce syntax highlight spans"
            );
        }
    }

    #[test]
    fn resolves_fenced_code_language_aliases() {
        let highlighter = highlighter();

        assert_eq!(highlighter.language_id_for_name("rs"), Some("rust"));
        assert_eq!(highlighter.language_id_for_name("py"), None);
        assert_eq!(highlighter.language_id_for_name("yml"), Some("yaml"));
        assert_eq!(highlighter.language_id_for_name("ts"), Some("typescript"));
        assert_eq!(highlighter.language_id_for_name("jsx"), Some("jsx"));
        assert_eq!(highlighter.language_id_for_name("sh"), Some("bash"));
        assert_eq!(highlighter.language_id_for_name("shell"), Some("bash"));
        assert_eq!(highlighter.language_id_for_name("fish"), Some("fish"));
        assert_eq!(highlighter.language_id_for_name("pwsh"), Some("powershell"));
        assert_eq!(highlighter.language_id_for_name("lua"), Some("lua"));
        assert_eq!(highlighter.language_id_for_name("hk"), Some("husk"));
        assert_eq!(highlighter.language_id_for_name("husk"), Some("husk"));
        assert_eq!(highlighter.language_id_for_name("unknown"), None);
    }

    #[test]
    fn static_tree_sitter_injection_properties_select_the_nested_language() {
        let code = "```ignored\nconst answer = true;\n```\n";
        let injections = markdown_injections(
            r#"((fenced_code_block
                  (code_fence_content) @injection.content)
                 (#set! injection.language "javascript"))"#,
            code,
        );

        assert_eq!(injections.len(), 1);
        assert_eq!(injections[0].language_name, "javascript");
        assert_eq!(
            &code[injections[0].content_start..injections[0].content_end],
            "const answer = true;\n"
        );
    }

    #[test]
    fn dynamic_tree_sitter_injection_captures_remain_supported() {
        let injections =
            markdown_injections(MARKDOWN_INJECTION_QUERY, "```rust\nfn main() {}\n```\n");

        assert_eq!(injections.len(), 1);
        assert_eq!(injections[0].language_name, "rust");
    }

    #[test]
    fn explicit_static_injection_language_takes_precedence_over_dynamic_capture() {
        let injections = markdown_injections(
            r#"((fenced_code_block
                  (info_string (language) @injection.language)
                  (code_fence_content) @injection.content)
                 (#set! injection.language "javascript"))"#,
            "```rust\nconst answer = true;\n```\n",
        );

        assert_eq!(injections.len(), 1);
        assert_eq!(injections[0].language_name, "javascript");
    }

    #[test]
    fn unavailable_static_injection_language_degrades_without_loading_another_grammar() {
        let directory = tempfile::tempdir().unwrap();
        let query = directory.path().join("injections.scm");
        fs::write(
            &query,
            r#"((fenced_code_block
                  (code_fence_content) @injection.content)
                 (#set! injection.language "not-installed"))"#,
        )
        .unwrap();
        let languages = HashMap::from([(
            "markdown".to_string(),
            LanguageConfig {
                grammar: Some(LanguageGrammarConfig {
                    injections: Some(query),
                    ..LanguageGrammarConfig::default()
                }),
                ..LanguageConfig::default()
            },
        )]);
        let registry =
            Arc::new(LanguageRegistry::from_config(&languages, directory.path()).unwrap());
        let mut highlighter = Highlighter::with_registry(&Theme::default(), registry).unwrap();

        assert!(highlighter
            .highlight("markdown", "```unknown\nconst answer = true;\n```\n")
            .is_ok());
    }

    #[test]
    fn static_injection_properties_highlight_available_nested_source() {
        let directory = tempfile::tempdir().unwrap();
        let query = directory.path().join("injections.scm");
        fs::write(
            &query,
            r#"((fenced_code_block
                  (code_fence_content) @injection.content)
                 (#set! injection.language "javascript"))"#,
        )
        .unwrap();
        let languages = HashMap::from([(
            "markdown".to_string(),
            LanguageConfig {
                grammar: Some(LanguageGrammarConfig {
                    injections: Some(query),
                    ..LanguageGrammarConfig::default()
                }),
                ..LanguageConfig::default()
            },
        )]);
        let registry =
            Arc::new(LanguageRegistry::from_config(&languages, directory.path()).unwrap());
        let mut highlighter =
            Highlighter::with_registry(&theme_with_scopes(&["keyword"]), registry).unwrap();
        let code = "```unknown\nconst answer = true;\n```\n";
        let styles = highlighter.highlight("markdown", code).unwrap();

        assert_token_highlighted(&styles, code, "const");
    }

    #[test]
    fn lists_bundled_language_ids_in_display_order() {
        let highlighter = highlighter();

        assert_eq!(
            highlighter.language_ids(),
            vec![
                "bash",
                "fish",
                "gitcommit",
                "husk",
                "javascript",
                "json",
                "jsx",
                "lua",
                "markdown",
                "powershell",
                "rust",
                "toml",
                "tsx",
                "typescript",
                "yaml",
            ]
        );
    }

    #[test]
    fn matches_language_ids_by_name_and_extension_prefix() {
        let highlighter = highlighter();

        assert_eq!(highlighter.matching_language_ids("fi"), vec!["fish"]);
        assert_eq!(highlighter.matching_language_ids("ru"), vec!["rust"]);
        assert_eq!(highlighter.matching_language_ids("ym"), vec!["yaml"]);
        assert_eq!(highlighter.matching_language_ids(".rs"), vec!["rust"]);
        assert_eq!(
            highlighter.matching_language_ids("ts"),
            vec!["tsx", "typescript"]
        );
        assert!(highlighter.matching_language_ids("unknown").is_empty());
    }

    #[test]
    fn fish_highlights_keywords_functions_variables_strings_and_comments() {
        let theme = theme_with_scopes(&["keyword", "function", "constant", "string", "comment"]);
        let mut highlighter = Highlighter::new(&theme).unwrap();
        let code = "# Greeting\nfunction greet --argument-names name\n    if test -n \"$name\"\n        echo \"hello, $name\"\n    end\nend\n";
        let styles = highlighter
            .highlight_for_file(Some("greet.fish"), code)
            .unwrap();

        for token in ["# Greeting", "function", "greet", "if", "$name", "echo"] {
            assert_token_highlighted(&styles, code, token);
        }
        assert_token_highlighted(&styles, code, "\"hello, $name\"");
    }

    #[test]
    fn husk_highlights_tokens_from_lexer() {
        let theme = theme_with_scopes(&[
            "comment",
            "constant.builtin",
            "constant.numeric",
            "keyword",
            "operator",
            "string",
            "type.builtin",
        ]);
        let mut highlighter = Highlighter::new(&theme).unwrap();
        let code = r#"// activate plugin
pub fn activate(event: Json) {
    let enabled = true;
    let count: i32 = 42;
    red::execute("Print", "hello");
}
"#;

        let styles = highlighter
            .highlight_for_file(Some("plugin.hk"), code)
            .unwrap();

        for token in [
            "// activate plugin",
            "pub",
            "fn",
            "Json",
            "let",
            "true",
            "i32",
            "42",
            "=",
            "\"Print\"",
        ] {
            assert_token_highlighted(&styles, code, token);
        }
    }

    #[test]
    fn typescript_inherits_javascript_highlights() {
        let theme = theme_with_scopes(&["keyword", "string", "function", "function.method"]);
        let mut highlighter = Highlighter::new(&theme).unwrap();
        let code = r#"import fs from "node:fs/promises";
describe("StateStore", async () => {
    const store = new StateStore();
    await store.initialize();
});
"#;

        for language_id in ["typescript", "tsx"] {
            let styles = highlighter.highlight(language_id, code).unwrap();

            for token in [
                "import",
                "\"node:fs/promises\"",
                "describe",
                "async",
                "const",
                "new",
                "await",
                "initialize",
            ] {
                assert_token_highlighted(&styles, code, token);
            }
        }
    }

    #[test]
    fn javascript_family_highlights_parameters() {
        let theme = theme_with_scopes(&["variable.parameter"]);
        let mut highlighter = Highlighter::new(&theme).unwrap();
        let code = "function greet(person) { return person; }";

        for language_id in ["javascript", "jsx", "typescript", "tsx"] {
            let styles = highlighter.highlight(language_id, code).unwrap();
            assert_token_highlighted(&styles, code, "person");
        }
    }

    #[test]
    fn jsx_languages_highlight_tags_and_attributes() {
        let theme = theme_with_scopes(&["tag", "attribute"]);
        let mut highlighter = Highlighter::new(&theme).unwrap();
        let code = r#"const view = <section data-id="value" />;"#;

        for language_id in ["jsx", "tsx"] {
            let styles = highlighter.highlight(language_id, code).unwrap();
            assert_token_highlighted(&styles, code, "section");
            assert_token_highlighted(&styles, code, "data-id");
        }
    }

    #[test]
    fn markdown_uses_theme_compatible_scopes() {
        let mut highlighter = highlighter();
        let styles = highlighter
            .highlight_for_file(Some("CLAUDE.md"), "### Debugging\n- `dh` - History\n")
            .unwrap();

        assert!(
            !styles.is_empty(),
            "markdown should produce themed highlight spans"
        );
        assert!(
            styles
                .iter()
                .any(|style| style.start <= 4 && style.end >= 13),
            "markdown heading text should be highlighted"
        );
        assert!(
            styles.iter().any(|style| style.start == 14),
            "markdown list marker should be highlighted"
        );
    }

    #[test]
    fn markdown_highlights_with_textmate_markdown_theme_scopes() {
        let theme = theme_with_markdown_textmate_scopes();
        let mut highlighter = Highlighter::new(&theme).unwrap();
        let code = "## Determining the PR(s)\n- Use `gh`\n";
        let styles = highlighter
            .highlight_for_file(Some("SKILL.md"), code)
            .unwrap();
        let list_marker_start = code.find("- ").unwrap();

        assert!(
            styles
                .iter()
                .any(|style| style.start <= 3 && style.end >= 21),
            "markdown heading should use TextMate-compatible theme scopes"
        );
        assert!(
            styles.iter().any(|style| style.start == list_marker_start),
            "markdown list marker should use TextMate-compatible theme scopes"
        );
    }

    #[test]
    fn markdown_highlights_rust_fenced_code() {
        let mut highlighter = highlighter();
        let code = "# Example\n\n```rust\nfn main() {\n    let value = true;\n}\n```\n";
        let styles = highlighter
            .highlight_for_file(Some("README.md"), code)
            .unwrap();
        let fn_start = code.find("fn").unwrap();
        let let_start = code.find("let").unwrap();

        assert!(
            styles
                .iter()
                .any(|style| style.start <= fn_start && style.end >= fn_start + 2),
            "fenced Rust `fn` keyword should be highlighted at Markdown byte offsets"
        );
        assert!(
            styles
                .iter()
                .any(|style| style.start <= let_start && style.end >= let_start + 3),
            "fenced Rust `let` keyword should be highlighted at Markdown byte offsets"
        );
    }

    #[test]
    fn markdown_highlights_json_fenced_code() {
        let mut highlighter = highlighter();
        let code = "```json\n{\"enabled\": true}\n```\n";
        let styles = highlighter
            .highlight_for_file(Some("README.md"), code)
            .unwrap();
        let bool_start = code.find("true").unwrap();

        assert!(
            styles
                .iter()
                .any(|style| style.start <= bool_start && style.end >= bool_start + 4),
            "fenced JSON boolean should be highlighted at Markdown byte offsets"
        );
    }

    #[test]
    fn markdown_highlights_bash_fenced_code() {
        let mut highlighter = highlighter();
        let code = "```sh\nif [ -f Cargo.toml ]; then\n  echo yes\nfi\n```\n";
        let styles = highlighter
            .highlight_for_file(Some("README.md"), code)
            .unwrap();
        let if_start = code.find("if").unwrap();
        let echo_start = code.find("echo").unwrap();

        assert!(
            styles
                .iter()
                .any(|style| style.start <= if_start && style.end >= if_start + 2),
            "fenced shell `if` keyword should be highlighted at Markdown byte offsets"
        );
        assert!(
            styles
                .iter()
                .any(|style| style.start <= echo_start && style.end >= echo_start + 4),
            "fenced shell command should be highlighted at Markdown byte offsets"
        );
    }

    #[test]
    fn markdown_highlights_fish_fenced_code() {
        let mut highlighter = highlighter();
        let code = "```fish\nfunction greet\n    echo hello\nend\n```\n";
        let styles = highlighter
            .highlight_for_file(Some("README.md"), code)
            .unwrap();
        let function_start = code.find("function").unwrap();
        let echo_start = code.find("echo").unwrap();

        assert!(
            styles
                .iter()
                .any(|style| style.start <= function_start && style.end >= function_start + 8),
            "fenced Fish `function` keyword should be highlighted at Markdown byte offsets"
        );
        assert!(
            styles
                .iter()
                .any(|style| style.start <= echo_start && style.end >= echo_start + 4),
            "fenced Fish command should be highlighted at Markdown byte offsets"
        );
    }

    #[test]
    fn markdown_highlights_husk_fenced_code() {
        let theme = theme_with_scopes(&["keyword", "string"]);
        let mut highlighter = Highlighter::new(&theme).unwrap();
        let code = "```husk\npub fn activate() { red::log(\"ready\"); }\n```\n";
        let styles = highlighter
            .highlight_for_file(Some("README.md"), code)
            .unwrap();
        let pub_start = code.find("pub").unwrap();
        let ready_start = code.find("\"ready\"").unwrap();

        assert!(
            styles
                .iter()
                .any(|style| style.start <= pub_start && style.end >= pub_start + 3),
            "fenced Husk `pub` keyword should be highlighted at Markdown byte offsets"
        );
        assert!(
            styles
                .iter()
                .any(|style| style.start <= ready_start && style.end >= ready_start + 7),
            "fenced Husk string should be highlighted at Markdown byte offsets"
        );
    }

    #[test]
    fn markdown_resolves_fenced_code_by_registered_extension() {
        let mut highlighter = highlighter();
        let code = "```pyw\nprint(True)\n```\n";
        let styles = highlighter
            .highlight_for_file(Some("README.md"), code)
            .unwrap();
        let true_start = code.find("True").unwrap();

        assert!(
            styles
                .iter()
                .any(|style| style.start <= true_start && style.end >= true_start + 4),
            "fenced language names should resolve through registered extensions"
        );
    }

    #[test]
    fn markdown_ignores_unknown_fenced_code_language() {
        let mut highlighter = highlighter();
        let code = "```madeup\nhello\n```\n";
        let styles = highlighter
            .highlight_for_file(Some("README.md"), code)
            .unwrap();
        let content_start = code.find("hello").unwrap();

        assert!(
            styles
                .iter()
                .any(|style| style.start <= content_start && style.end >= content_start + 5),
            "unknown fenced language should keep Markdown raw block styling"
        );
    }

    #[test]
    fn unknown_languages_do_not_error() {
        let mut highlighter = highlighter();

        assert!(highlighter
            .highlight("unknown", "plain text")
            .unwrap()
            .is_empty());
        assert!(highlighter
            .highlight_for_file(Some("notes.txt"), "plain text")
            .unwrap()
            .is_empty());
    }
}
