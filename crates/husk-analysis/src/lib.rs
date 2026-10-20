//! Incremental, editor-oriented analysis for Husk source.
//!
//! This crate deliberately stops before HIR lowering and execution. It accepts
//! unsaved source revisions, retains recovered syntax, and exposes stable
//! navigation data for protocol adapters such as `husk-lsp`.

mod formatter;
mod line_index;
mod symbols;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use husk_ast::{File, Span};
use husk_package::{PackageLimits, ResolvedPackage, discover_manifest};
use husk_runtime::{module_declaration_ast, source_module_descriptors};
use husk_semantic::{
    HoverInfo, SemanticOptions, SemanticProfile, SemanticResult,
    analyze_file_with_declarations_and_options,
};
use husk_types::ModuleDescriptor;

pub use formatter::format_source;
pub use line_index::{LineIndex, Position, PositionRange};
pub use symbols::{Symbol, SymbolId, SymbolKind, SymbolOccurrence};

const MAX_WORKSPACE_DOCUMENTS: usize = 256;
const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;

/// Diagnostic severity independent of a transport protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
}

/// One parser, semantic, package, or extension diagnostic.
#[derive(Debug, Clone)]
pub struct AnalysisDiagnostic {
    pub code: String,
    pub message: String,
    pub span: std::ops::Range<usize>,
    pub severity: DiagnosticSeverity,
    pub source: &'static str,
}

/// Immutable source and analysis products for one document revision.
pub struct Document {
    path: PathBuf,
    module_path: Vec<String>,
    version: i32,
    text: Arc<str>,
    syntax: File,
    line_index: LineIndex,
    semantic: SemanticResult,
    diagnostics: Vec<AnalysisDiagnostic>,
    symbols: Vec<Symbol>,
    occurrences: Vec<SymbolOccurrence>,
}

impl Document {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn module_path(&self) -> &[String] {
        &self.module_path
    }

    #[must_use]
    pub fn version(&self) -> i32 {
        self.version
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn syntax(&self) -> &File {
        &self.syntax
    }

    #[must_use]
    pub fn line_index(&self) -> &LineIndex {
        &self.line_index
    }

    #[must_use]
    pub fn semantic(&self) -> &SemanticResult {
        &self.semantic
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[AnalysisDiagnostic] {
        &self.diagnostics
    }

    #[must_use]
    pub fn symbols(&self) -> &[Symbol] {
        &self.symbols
    }

    #[must_use]
    pub fn occurrences(&self) -> &[SymbolOccurrence] {
        &self.occurrences
    }

    #[must_use]
    pub fn byte_offset(&self, position: Position) -> Option<usize> {
        self.line_index.byte_offset(&self.text, position)
    }

    #[must_use]
    pub fn position_range(&self, range: &std::ops::Range<usize>) -> PositionRange {
        self.line_index.range(&self.text, range)
    }

    #[must_use]
    pub fn hover(&self, byte_offset: usize) -> Option<&HoverInfo> {
        self.semantic
            .hover_info
            .iter()
            .filter(|((start, end), _)| *start <= byte_offset && byte_offset <= *end)
            .min_by_key(|((start, end), _)| end.saturating_sub(*start))
            .map(|(_, hover)| hover)
    }
}

/// Mutable analysis state for one LSP workspace.
pub struct Workspace {
    root: PathBuf,
    profile: SemanticProfile,
    cfg_flags: HashSet<String>,
    declarations: Vec<File>,
    trusted_declarations: Vec<File>,
    external_modules: Vec<ModuleDescriptor>,
    documents: BTreeMap<PathBuf, Document>,
    package_diagnostics: Vec<AnalysisDiagnostic>,
}

impl Workspace {
    /// Open a workspace and index its bounded Husk source set.
    pub fn open(root: impl AsRef<Path>, profile: SemanticProfile) -> anyhow::Result<Self> {
        let root = root
            .as_ref()
            .canonicalize()
            .with_context(|| format!("resolve Husk workspace `{}`", root.as_ref().display()))?;
        let mut workspace = Self {
            root,
            profile,
            cfg_flags: HashSet::new(),
            declarations: Vec::new(),
            trusted_declarations: Vec::new(),
            external_modules: Vec::new(),
            documents: BTreeMap::new(),
            package_diagnostics: Vec::new(),
        };
        workspace.refresh_disk()?;
        Ok(workspace)
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn profile(&self) -> SemanticProfile {
        self.profile
    }

    pub fn set_profile(&mut self, profile: SemanticProfile) {
        if self.profile != profile {
            self.profile = profile;
            self.reanalyze();
        }
    }

    pub fn set_cfg_flags(&mut self, flags: impl IntoIterator<Item = String>) {
        let flags = flags.into_iter().collect();
        if self.cfg_flags != flags {
            self.cfg_flags = flags;
            self.reanalyze();
        }
    }

    /// Replace the external typed module surface visible to every document.
    pub fn set_external_modules(&mut self, modules: Vec<ModuleDescriptor>) -> anyhow::Result<()> {
        let mut declarations = self.package_declarations().unwrap_or_default();
        declarations.extend(self.trusted_declarations.iter().cloned());
        for module in &modules {
            module.validate()?;
            declarations.push(module_declaration_ast(module)?);
        }
        self.external_modules = modules;
        self.declarations = declarations;
        self.reanalyze();
        Ok(())
    }

    /// Replace trusted host declaration sources, such as Red's plugin API.
    pub fn set_trusted_declaration_sources(
        &mut self,
        sources: impl IntoIterator<Item = String>,
    ) -> anyhow::Result<()> {
        let mut trusted = Vec::new();
        for source in sources {
            let parsed = husk_parser::parse_str(&source);
            if !parsed.errors.is_empty() {
                let messages = parsed
                    .errors
                    .iter()
                    .map(|error| error.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ");
                anyhow::bail!("trusted Husk declaration did not parse: {messages}");
            }
            trusted.push(
                parsed
                    .file
                    .expect("the recovery parser always returns a Husk file"),
            );
        }
        self.trusted_declarations = trusted;
        let modules = self.external_modules.clone();
        self.set_external_modules(modules)
    }

    #[must_use]
    pub fn external_modules(&self) -> &[ModuleDescriptor] {
        &self.external_modules
    }

    #[must_use]
    pub fn package_diagnostics(&self) -> &[AnalysisDiagnostic] {
        &self.package_diagnostics
    }

    #[must_use]
    pub fn document(&self, path: impl AsRef<Path>) -> Option<&Document> {
        self.documents.get(&normalize_path(path.as_ref()))
    }

    pub fn documents(&self) -> impl Iterator<Item = &Document> {
        self.documents.values()
    }

    /// Insert or replace an unsaved source revision.
    pub fn update(
        &mut self,
        path: impl AsRef<Path>,
        version: i32,
        text: impl Into<Arc<str>>,
    ) -> anyhow::Result<&Document> {
        let path = normalize_path(path.as_ref());
        anyhow::ensure!(
            path.starts_with(&self.root),
            "Husk document `{}` is outside workspace `{}`",
            path.display(),
            self.root.display()
        );
        let text = text.into();
        anyhow::ensure!(
            text.len() <= MAX_DOCUMENT_BYTES,
            "Husk document `{}` is {} bytes; maximum is {}",
            path.display(),
            text.len(),
            MAX_DOCUMENT_BYTES
        );
        if self
            .documents
            .get(&path)
            .is_some_and(|document| document.text.as_ref() == text.as_ref())
        {
            let document = self
                .documents
                .get_mut(&path)
                .expect("unchanged document was present immediately above");
            document.version = version;
            return Ok(document);
        }
        let module_path = self
            .documents
            .get(&path)
            .map(|document| document.module_path.clone())
            .unwrap_or_else(|| infer_module_path(&self.root, &path));
        let document = analyze_document(
            path.clone(),
            module_path,
            version,
            text,
            self.profile,
            &self.cfg_flags,
            &self.declarations,
        );
        self.documents.insert(path.clone(), document);
        Ok(self
            .documents
            .get(&path)
            .expect("document was inserted immediately above"))
    }

    /// Drop an in-memory revision and reload the on-disk source when present.
    pub fn close(&mut self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let path = normalize_path(path.as_ref());
        if path.is_file() {
            let bytes = fs::read(&path)
                .with_context(|| format!("read Husk document `{}`", path.display()))?;
            let text = String::from_utf8(bytes)
                .with_context(|| format!("Husk document `{}` is not UTF-8", path.display()))?;
            self.update(&path, 0, Arc::<str>::from(text))?;
        } else {
            self.documents.remove(&path);
        }
        Ok(())
    }

    /// Reload source and package declarations without changing open overlays.
    pub fn refresh_disk(&mut self) -> anyhow::Result<()> {
        self.package_diagnostics.clear();
        let existing = self
            .documents
            .iter()
            .filter(|(_, document)| document.version > 0)
            .map(|(path, document)| (path.clone(), document.version, Arc::clone(&document.text)))
            .collect::<Vec<_>>();
        self.documents.clear();

        match discover_manifest(&self.root) {
            Ok(manifest) => match ResolvedPackage::open(&manifest, PackageLimits::default()) {
                Ok(package) => {
                    let source_descriptors = source_module_descriptors(&package)?;
                    self.declarations = source_descriptors
                        .iter()
                        .map(module_declaration_ast)
                        .collect::<anyhow::Result<Vec<_>>>()?;
                    self.declarations
                        .extend(self.trusted_declarations.iter().cloned());
                    self.declarations.extend(
                        self.external_modules
                            .iter()
                            .map(module_declaration_ast)
                            .collect::<anyhow::Result<Vec<_>>>()?,
                    );
                    for module in package.modules {
                        self.insert_disk_document(
                            module.canonical_path,
                            module.module_path,
                            module.source,
                        );
                    }
                }
                Err(error) => {
                    self.package_diagnostics.push(AnalysisDiagnostic {
                        code: "HUSK-PKG0001".to_string(),
                        message: error.to_string(),
                        span: 0..0,
                        severity: DiagnosticSeverity::Error,
                        source: "husk-package",
                    });
                    self.load_loose_sources()?;
                }
            },
            Err(_) => self.load_loose_sources()?,
        }

        for (path, version, text) in existing {
            self.update(path, version, text)?;
        }
        Ok(())
    }

    #[must_use]
    pub fn symbol_at(&self, path: &Path, byte_offset: usize) -> Option<&Symbol> {
        let document = self.document(path)?;
        if let Some(symbol) = document
            .symbols
            .iter()
            .filter(|symbol| symbol.span.start <= byte_offset && byte_offset <= symbol.span.end)
            .min_by_key(|symbol| symbol.span.end.saturating_sub(symbol.span.start))
        {
            return Some(symbol);
        }
        let occurrence = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.span.start <= byte_offset && byte_offset <= occurrence.span.end
            })
            .min_by_key(|occurrence| occurrence.span.end.saturating_sub(occurrence.span.start))?;
        self.symbol_by_id(&occurrence.id)
    }

    #[must_use]
    pub fn symbol_by_id(&self, id: &SymbolId) -> Option<&Symbol> {
        self.documents()
            .flat_map(Document::symbols)
            .find(|symbol| &symbol.id == id)
    }

    #[must_use]
    pub fn definition(&self, id: &SymbolId) -> Option<(&Document, &Symbol)> {
        self.documents().find_map(|document| {
            document
                .symbols
                .iter()
                .find(|symbol| &symbol.id == id)
                .map(|symbol| (document, symbol))
        })
    }

    #[must_use]
    pub fn symbol_named(&self, name: &str) -> Option<(&Document, &Symbol)> {
        self.documents().find_map(|document| {
            document
                .symbols
                .iter()
                .find(|symbol| symbol.name == name || symbol.qualified_name == name)
                .map(|symbol| (document, symbol))
        })
    }

    #[must_use]
    pub fn references(&self, id: &SymbolId) -> Vec<&SymbolOccurrence> {
        self.documents()
            .flat_map(Document::occurrences)
            .filter(|occurrence| &occurrence.id == id)
            .collect()
    }

    #[must_use]
    pub fn workspace_symbols(&self, query: &str) -> Vec<(&Path, &Symbol)> {
        let query = query.to_ascii_lowercase();
        let mut symbols = self
            .documents()
            .flat_map(|document| {
                document.symbols.iter().filter_map(|symbol| {
                    (query.is_empty()
                        || symbol.name.to_ascii_lowercase().contains(&query)
                        || symbol.qualified_name.to_ascii_lowercase().contains(&query))
                    .then_some((document.path(), symbol))
                })
            })
            .collect::<Vec<_>>();
        symbols.sort_by(|left, right| {
            left.1
                .name
                .cmp(&right.1.name)
                .then_with(|| left.0.cmp(right.0))
                .then_with(|| left.1.span.start.cmp(&right.1.span.start))
        });
        symbols
    }

    #[must_use]
    pub fn completions(&self, path: &Path, prefix: &str) -> Vec<&Symbol> {
        let prefix = prefix.to_ascii_lowercase();
        let mut seen = HashSet::new();
        let mut symbols = self
            .documents()
            .flat_map(Document::symbols)
            .filter(|symbol| {
                (prefix.is_empty() || symbol.name.to_ascii_lowercase().starts_with(&prefix))
                    && seen.insert((symbol.name.clone(), symbol.kind, symbol.container.clone()))
            })
            .collect::<Vec<_>>();
        let local_symbols = self
            .document(path)
            .map(|document| {
                document
                    .symbols
                    .iter()
                    .map(|symbol| &symbol.id)
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        symbols.sort_by(|left, right| {
            let left_local = local_symbols.contains(&left.id);
            let right_local = local_symbols.contains(&right.id);
            right_local
                .cmp(&left_local)
                .then_with(|| left.name.cmp(&right.name))
        });
        symbols
    }

    fn insert_disk_document(&mut self, path: PathBuf, module_path: Vec<String>, text: Arc<str>) {
        let document = analyze_document(
            path.clone(),
            module_path,
            0,
            text,
            self.profile,
            &self.cfg_flags,
            &self.declarations,
        );
        self.documents.insert(path, document);
    }

    fn load_loose_sources(&mut self) -> anyhow::Result<()> {
        self.declarations.clear();
        self.declarations
            .extend(self.trusted_declarations.iter().cloned());
        self.declarations.extend(
            self.external_modules
                .iter()
                .map(module_declaration_ast)
                .collect::<anyhow::Result<Vec<_>>>()?,
        );
        let mut paths = Vec::new();
        collect_husk_files(&self.root, &mut paths)?;
        paths.sort();
        anyhow::ensure!(
            paths.len() <= MAX_WORKSPACE_DOCUMENTS,
            "Husk workspace contains {} source files; maximum is {}",
            paths.len(),
            MAX_WORKSPACE_DOCUMENTS
        );
        for path in paths {
            let bytes = fs::read(&path)
                .with_context(|| format!("read Husk document `{}`", path.display()))?;
            anyhow::ensure!(
                bytes.len() <= MAX_DOCUMENT_BYTES,
                "Husk document `{}` is {} bytes; maximum is {}",
                path.display(),
                bytes.len(),
                MAX_DOCUMENT_BYTES
            );
            let text = String::from_utf8(bytes)
                .with_context(|| format!("Husk document `{}` is not UTF-8", path.display()))?;
            self.insert_disk_document(
                path.clone(),
                infer_module_path(&self.root, &path),
                Arc::from(text),
            );
        }
        Ok(())
    }

    fn package_declarations(&self) -> anyhow::Result<Vec<File>> {
        let Ok(manifest) = discover_manifest(&self.root) else {
            return Ok(Vec::new());
        };
        let package = ResolvedPackage::open(manifest, PackageLimits::default())?;
        source_module_descriptors(&package)?
            .iter()
            .map(module_declaration_ast)
            .collect()
    }

    fn reanalyze(&mut self) {
        let documents = std::mem::take(&mut self.documents);
        self.documents = documents
            .into_iter()
            .map(|(path, document)| {
                let analyzed = analyze_document(
                    path.clone(),
                    document.module_path,
                    document.version,
                    document.text,
                    self.profile,
                    &self.cfg_flags,
                    &self.declarations,
                );
                (path, analyzed)
            })
            .collect();
    }
}

fn analyze_document(
    path: PathBuf,
    module_path: Vec<String>,
    version: i32,
    text: Arc<str>,
    profile: SemanticProfile,
    cfg_flags: &HashSet<String>,
    declarations: &[File],
) -> Document {
    let parsed = husk_parser::parse_str(&text);
    let syntax = parsed
        .file
        .expect("the recovery parser always returns a Husk file");
    let mut diagnostics = parsed
        .errors
        .into_iter()
        .map(|error| AnalysisDiagnostic {
            code: "HUSK-P0001".to_string(),
            message: error.message,
            span: error.span.range,
            severity: DiagnosticSeverity::Error,
            source: "husk-parser",
        })
        .collect::<Vec<_>>();
    let semantic = analyze_file_with_declarations_and_options(
        &syntax,
        declarations,
        SemanticOptions {
            prelude: true,
            cfg_flags: cfg_flags.clone(),
            profile,
            module_path: module_path.clone(),
        },
    );
    diagnostics.extend(
        semantic
            .symbols
            .errors
            .iter()
            .chain(&semantic.type_errors)
            .map(|error| AnalysisDiagnostic {
                code: "HUSK-T0001".to_string(),
                message: error.message.clone(),
                span: error.span.range.clone(),
                severity: DiagnosticSeverity::Error,
                source: "husk-semantic",
            }),
    );
    diagnostics.sort_by(|left, right| {
        left.span
            .start
            .cmp(&right.span.start)
            .then_with(|| left.span.end.cmp(&right.span.end))
            .then_with(|| left.message.cmp(&right.message))
    });
    diagnostics.dedup_by(|left, right| {
        left.code == right.code && left.message == right.message && left.span == right.span
    });
    let (symbols, occurrences) =
        symbols::extract_symbols(&path, &module_path, &text, &syntax, &semantic);
    let line_index = LineIndex::new(&text);
    Document {
        path,
        module_path,
        version,
        text,
        syntax,
        line_index,
        semantic,
        diagnostics,
        symbols,
        occurrences,
    }
}

fn collect_husk_files(directory: &Path, output: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    let mut entries = fs::read_dir(directory)
        .with_context(|| format!("read Husk workspace directory `{}`", directory.display()))?
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("inspect `{}`", path.display()))?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            let name = entry.file_name();
            if matches!(
                name.to_str(),
                Some(".git" | ".husk" | "target" | "vendor" | "node_modules")
            ) {
                continue;
            }
            collect_husk_files(&path, output)?;
        } else if file_type.is_file()
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| matches!(extension, "hk" | "husk"))
        {
            output.push(path);
        }
    }
    Ok(())
}

fn normalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn infer_module_path(root: &Path, path: &Path) -> Vec<String> {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let mut components = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if components
        .first()
        .is_some_and(|component| component == "src")
    {
        components.remove(0);
    }
    let Some(last) = components.last_mut() else {
        return Vec::new();
    };
    if let Some((stem, _)) = last.rsplit_once('.') {
        *last = stem.to_string();
    }
    if matches!(last.as_str(), "main" | "lib" | "mod") {
        components.pop();
    }
    components
}

/// Create a span constrained to a source revision.
#[must_use]
pub fn bounded_span(span: &Span, source_len: usize) -> std::ops::Range<usize> {
    span.range.start.min(source_len)..span.range.end.min(source_len)
}

#[cfg(test)]
mod tests {
    use husk_types::{FunctionDescriptor, TypeDescriptor, Version};

    use super::*;

    #[test]
    fn recovered_source_keeps_diagnostics_symbols_and_utf16_positions() {
        let root = tempfile::tempdir().expect("create fixture directory");
        let path = root.path().join("main.hk");
        fs::write(
            &path,
            "fn greet(name: String) -> String { name }\nfn broken(",
        )
        .expect("write source fixture");
        let workspace =
            Workspace::open(root.path(), SemanticProfile::Native).expect("open workspace");
        let document = workspace.document(&path).expect("fixture is indexed");

        assert!(!document.diagnostics().is_empty());
        assert!(
            document
                .symbols()
                .iter()
                .any(|symbol| symbol.name == "greet" && symbol.kind == SymbolKind::Function)
        );
    }

    #[test]
    fn unsaved_revisions_replace_disk_without_writing_it() {
        let root = tempfile::tempdir().expect("create fixture directory");
        let path = root.path().join("main.hk");
        fs::write(&path, "fn before() {}\n").expect("write source fixture");
        let mut workspace =
            Workspace::open(root.path(), SemanticProfile::Native).expect("open workspace");
        workspace
            .update(&path, 7, Arc::<str>::from("fn after() {}\n"))
            .expect("update overlay");

        let document = workspace.document(&path).expect("fixture is indexed");
        assert_eq!(document.version(), 7);
        assert!(
            document
                .symbols()
                .iter()
                .any(|symbol| symbol.name == "after")
        );
        assert_eq!(
            fs::read_to_string(&path).expect("read disk source"),
            "fn before() {}\n"
        );
    }

    #[test]
    fn unchanged_source_revision_reuses_existing_analysis_and_updates_version() {
        let root = tempfile::tempdir().expect("create fixture directory");
        let path = root.path().join("main.hk");
        let source = "fn unchanged() {}\n";
        fs::write(&path, source).expect("write source fixture");
        let mut workspace =
            Workspace::open(root.path(), SemanticProfile::Native).expect("open workspace");
        let original_symbols = workspace.document(&path).unwrap().symbols().as_ptr();

        let document = workspace
            .update(&path, 12, Arc::<str>::from(source))
            .expect("advance unchanged revision");

        assert_eq!(document.version(), 12);
        assert_eq!(document.symbols().as_ptr(), original_symbols);
    }

    #[test]
    fn unchanged_configuration_flags_do_not_reanalyze_documents() {
        let root = tempfile::tempdir().expect("create fixture directory");
        let path = root.path().join("main.hk");
        fs::write(&path, "fn unchanged() {}\n").expect("write source fixture");
        let mut workspace =
            Workspace::open(root.path(), SemanticProfile::Native).expect("open workspace");
        workspace.set_cfg_flags(["first".to_string(), "second".to_string()]);
        let original_symbols = workspace.document(&path).unwrap().symbols().as_ptr();

        workspace.set_cfg_flags(["second".to_string(), "first".to_string()]);

        assert_eq!(
            workspace.document(&path).unwrap().symbols().as_ptr(),
            original_symbols
        );
    }

    #[test]
    fn completions_prioritize_local_symbols_and_preserve_name_order() {
        let root = tempfile::tempdir().expect("create fixture directory");
        let local = root.path().join("main.hk");
        let other = root.path().join("other.hk");
        fs::write(&local, "fn z_local() {}\nfn b_local() {}\n")
            .expect("write local source fixture");
        fs::write(&other, "fn a_workspace() {}\nfn y_workspace() {}\n")
            .expect("write workspace source fixture");
        let workspace =
            Workspace::open(root.path(), SemanticProfile::Native).expect("open workspace");

        let names = workspace
            .completions(&local, "")
            .into_iter()
            .map(|symbol| symbol.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, ["b_local", "z_local", "a_workspace", "y_workspace"]);
        assert_eq!(workspace.completions(&local, "z_")[0].name, "z_local");
    }

    #[test]
    fn disk_refresh_preserves_external_module_declarations() {
        let root = tempfile::tempdir().expect("create fixture directory");
        let path = root.path().join("main.hk");
        fs::write(&path, "fn main() { demo::answer(); }\n").expect("write source fixture");
        let mut workspace =
            Workspace::open(root.path(), SemanticProfile::Native).expect("open workspace");
        let module = ModuleDescriptor::new(
            "demo",
            Version::new(1, 0, 0),
            vec![
                FunctionDescriptor::new("answer", Vec::new(), TypeDescriptor::Unit)
                    .expect("create function descriptor"),
            ],
            Vec::new(),
        )
        .expect("create module descriptor");
        workspace
            .set_external_modules(vec![module])
            .expect("register external module");
        workspace.refresh_disk().expect("refresh disk state");

        let document = workspace.document(&path).expect("fixture is indexed");
        assert!(
            document
                .diagnostics()
                .iter()
                .all(|diagnostic| !diagnostic.message.contains("demo")),
            "{:?}",
            document
                .diagnostics()
                .iter()
                .map(|diagnostic| &diagnostic.message)
                .collect::<Vec<_>>()
        );
    }
}
