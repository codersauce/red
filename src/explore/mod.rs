//! Deterministic code reading: tree-sitter structure, a name index, and a
//! keyboard-first review trail.
//!
//! Nothing here asks a model a question. Definitions are names found in
//! source. SCIP protobuf files are detected and left unread. Language-server
//! goto-definition stays on `gd`.

mod analyze;
mod index;

use std::path::{Path, PathBuf};

pub use analyze::{last_type_name, Analysis, Role, Span};
pub use index::{language_id_from_path, normalize_path, repo_root, Index, ScanStats};

use crate::highlighter::LanguageRegistry;
use analyze::{human_kind, is_syscall_wrapper};
use index::hash_text;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub x0: usize,
    pub x1: usize,
    pub y: usize,
    pub jump: Jump,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jump {
    pub label: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewRef {
    pub label: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub detail: String,
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewDef {
    pub label: String,
    pub kind: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub signature: String,
    pub refs: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DimZone {
    pub zone: Span,
    pub keep: Vec<Span>,
    pub holes: Vec<Span>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub language: String,
    pub note: String,
    pub in_function: Option<String>,
    pub in_file: String,
    pub cursor_name: Option<String>,
    pub cursor_role: String,
    pub cursor_syntax: String,
    pub in_function_count: usize,
    pub definition: Option<ViewDef>,
    pub references: Vec<ViewRef>,
    pub following: Option<String>,
    pub follow_spans: Vec<Span>,
    pub trail: Vec<Jump>,
    pub trail_at: usize,
    pub skeleton: bool,
    pub pr_mode: bool,
    pub pr_summary: String,
    pub index_summary: String,
    pub index_note: String,
    pub badges: Vec<(usize, usize)>,
    pub dim: Option<DimZone>,
    pub file_key: String,
    pub line_starts: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FollowLock {
    name: String,
    function: String,
    function_start: usize,
    file_wide: bool,
}

#[derive(Debug, Clone)]
struct OpenFile {
    key: String,
    revision: u64,
    analysis: Analysis,
}

#[derive(Debug, Clone, Default)]
pub struct Session {
    pub enabled: bool,
    pub skeleton: bool,
    pub pr_mode: bool,
    follow: Option<FollowLock>,
    trail: Vec<Jump>,
    trail_at: usize,
    xref_name: Option<String>,
    xref_at: usize,
    pub index: Index,
    open: Option<OpenFile>,
    hits: Vec<Hit>,
}

pub struct SyncInput<'a> {
    pub key: &'a str,
    pub revision: u64,
    pub language: Option<&'a str>,
    pub source: &'a str,
    pub registry: &'a LanguageRegistry,
}

impl Session {
    pub fn sync(&mut self, input: SyncInput<'_>) {
        if !self.enabled {
            return;
        }
        let unchanged = self
            .open
            .as_ref()
            .is_some_and(|open| open.key == input.key && open.revision == input.revision);
        if unchanged {
            return;
        }
        let analysis = match input.language {
            Some(language) => analyze_source(input.registry, language, input.source),
            None => Analysis::empty("no language detected for this buffer"),
        };
        if analysis.language.is_some() {
            self.index
                .upsert(input.key, hash_text(input.source), &analysis);
        }
        self.open = Some(OpenFile {
            key: input.key.to_string(),
            revision: input.revision,
            analysis,
        });
    }

    pub fn ingest_source(
        &mut self,
        key: &str,
        source: &str,
        language: &str,
        registry: &LanguageRegistry,
    ) {
        let analysis = analyze_source(registry, language, source);
        self.index.upsert(key, hash_text(source), &analysis);
    }

    pub fn view(&self, line: usize, column: usize) -> View {
        let open = self.open.as_ref();
        let analysis = open.map(|open| &open.analysis);
        let file_key = open.map(|open| open.key.clone()).unwrap_or_default();
        let shown_file = file_label(&file_key);
        let function = analysis.and_then(|analysis| analysis.function_at(line, column));
        let name = analysis.and_then(|analysis| analysis.name_at(line, column));
        let syntax = analysis.and_then(|analysis| analysis.syntax_at(line, column));
        let cursor_name = name.map(|name| name.name.clone());
        let cursor_role = name
            .map(|name| name.role.label().to_string())
            .unwrap_or_default();
        let cursor_syntax = syntax
            .map(|syntax| human_kind(&syntax.kind))
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| {
                name.map(|name| human_kind(&name.parent_kind))
                    .unwrap_or_default()
            });
        let in_function_names: Vec<&analyze::NameOcc> =
            match (analysis, function, cursor_name.as_deref()) {
                (Some(analysis), Some(function), Some(symbol)) => analysis
                    .names
                    .iter()
                    .filter(|name| name.name == symbol && function.span.contains_span(name.span))
                    .collect(),
                (Some(analysis), None, Some(symbol)) => analysis
                    .names
                    .iter()
                    .filter(|name| name.name == symbol)
                    .take(80)
                    .collect(),
                _ => Vec::new(),
            };
        let definition = resolve_definition(analysis, &self.index, &file_key, function, name);
        let mut references = Vec::new();
        for occ in &in_function_names {
            references.push(ViewRef {
                label: occ.name.clone(),
                file: file_key.clone(),
                line: occ.span.start_line,
                column: occ.span.start_col,
                detail: analysis
                    .map(|parsed| parsed.snippet(occ.span.start_line))
                    .unwrap_or_default(),
                current: occ.span.contains_pos(line, column),
            });
        }
        if let Some(symbol) = cursor_name.as_deref() {
            for loc in self.index.references(symbol) {
                let duplicate = references.iter().any(|existing| {
                    existing.file == loc.file
                        && existing.line == loc.line
                        && existing.column == loc.column
                });
                if duplicate {
                    continue;
                }
                references.push(ViewRef {
                    label: symbol.to_string(),
                    file: loc.file.clone(),
                    line: loc.line,
                    column: loc.column,
                    detail: if loc.file == file_key {
                        analysis
                            .map(|parsed| parsed.snippet(loc.line))
                            .unwrap_or_else(|| "this file".to_string())
                    } else {
                        file_label(&loc.file)
                    },
                    current: false,
                });
                if references.len() >= 40 {
                    break;
                }
            }
        }
        let (following, follow_spans) = follow_spans(self.follow.as_ref(), analysis, &file_key);
        let dim = skeleton_zone(self.skeleton, analysis, function, &self.index, &file_key);
        let badges = badges(analysis, &self.index, &file_key);
        View {
            language: analysis
                .and_then(|analysis| analysis.language.clone())
                .unwrap_or_else(|| "text".to_string()),
            note: analysis
                .map(|analysis| analysis.note.clone())
                .filter(|note| !note.is_empty())
                .unwrap_or_default(),
            in_function: function.map(|function| function.name.clone()),
            in_file: shown_file,
            cursor_name,
            cursor_role,
            cursor_syntax,
            in_function_count: in_function_names.len(),
            definition,
            references,
            following,
            follow_spans,
            trail: self.trail.clone(),
            trail_at: self.trail_at,
            skeleton: self.skeleton,
            pr_mode: self.pr_mode,
            pr_summary: self.index.pr_summary.clone(),
            index_summary: self.index.summary(),
            index_note: self.index.note(),
            badges,
            dim,
            file_key,
            line_starts: analysis
                .map(|analysis| analysis.line_starts.clone())
                .unwrap_or_else(|| vec![0]),
        }
    }

    pub fn is_current(&self, key: &str, revision: u64) -> bool {
        self.open
            .as_ref()
            .is_some_and(|open| open.key == key && open.revision == revision)
    }

    pub fn invalidate(&mut self) {
        self.open = None;
    }

    pub fn set_hits(&mut self, hits: Vec<Hit>) {
        self.hits = hits;
    }

    pub fn hit_at(&self, x: usize, y: usize) -> Option<&Jump> {
        self.hits
            .iter()
            .find(|hit| hit.y == y && x >= hit.x0 && x < hit.x1)
            .map(|hit| &hit.jump)
    }

    pub fn clear_pr(&mut self) {
        self.pr_mode = false;
        self.index.clear_pr_scope();
    }

    pub fn clear_follow(&mut self) {
        self.follow = None;
    }

    pub fn toggle_follow(&mut self, line: usize, column: usize) -> Result<String, String> {
        let next = {
            let Some(open) = self.open.as_ref() else {
                return Err("explore has not read this buffer yet".to_string());
            };
            let Some(name) = open.analysis.name_at(line, column) else {
                return Err("point at a name, then follow it".to_string());
            };
            let function = open.analysis.function_at(line, column);
            FollowLock {
                name: name.name.clone(),
                function: function
                    .map(|function| function.name.clone())
                    .unwrap_or_default(),
                function_start: function
                    .map(|function| function.span.start_line)
                    .unwrap_or(0),
                file_wide: function.is_none(),
            }
        };
        if self.follow.as_ref() == Some(&next) {
            self.follow = None;
            return Ok(format!("cleared follow on {}", next.name));
        }
        let label = next.name.clone();
        self.follow = Some(next);
        Ok(format!("following {label}"))
    }

    pub fn record_jump(&mut self, from: Jump, to: Jump) {
        if self.trail_at + 1 < self.trail.len() {
            self.trail.truncate(self.trail_at + 1);
        }
        if self
            .trail
            .last()
            .is_none_or(|crumb| !same_jump(crumb, &from))
        {
            self.trail.push(from);
        }
        self.trail.push(to);
        if self.trail.len() > 32 {
            let extra = self.trail.len() - 32;
            self.trail.drain(0..extra);
        }
        self.trail_at = self.trail.len().saturating_sub(1);
    }

    pub fn back(&mut self) -> Option<Jump> {
        if self.trail.is_empty() || self.trail_at == 0 {
            return None;
        }
        self.trail_at -= 1;
        self.trail.get(self.trail_at).cloned()
    }

    pub fn forward(&mut self) -> Option<Jump> {
        if self.trail_at + 1 >= self.trail.len() {
            return None;
        }
        self.trail_at += 1;
        self.trail.get(self.trail_at).cloned()
    }

    pub fn step_reference(
        &mut self,
        line: usize,
        column: usize,
        forward: bool,
    ) -> Result<Jump, String> {
        let view = self.view(line, column);
        if view.references.is_empty() {
            return Err("no references for this name".to_string());
        }
        let symbol = view.cursor_name.clone().unwrap_or_default();
        if self.xref_name.as_deref() != Some(symbol.as_str()) {
            self.xref_name = Some(symbol);
        }
        // The sidebar lists the current function first. Stepping uses file
        // order so entering another function does not renumber the matches.
        let order = navigation_order(&view.references);
        let current = order
            .iter()
            .enumerate()
            .filter(|(_, &index)| {
                let reference = &view.references[index];
                reference.file == view.file_key
                    && reference.line == line
                    && reference.column <= column
            })
            .max_by_key(|(_, &index)| view.references[index].column)
            .map(|(position, _)| position);
        let next = match current {
            Some(position) if forward => (position + 1) % order.len(),
            Some(position) => (position + order.len() - 1) % order.len(),
            None if forward => 0,
            None => order.len() - 1,
        };
        self.xref_at = next;
        let reference = &view.references[order[next]];
        Ok(Jump {
            label: reference.label.clone(),
            file: reference.file.clone(),
            line: reference.line,
            column: reference.column,
        })
    }

    pub fn definition_jump(&self, line: usize, column: usize) -> Result<Jump, String> {
        let view = self.view(line, column);
        let Some(name) = view.cursor_name.as_deref() else {
            return Err("point at a name to jump to its definition".to_string());
        };
        let Some(def) = view.definition else {
            return Err(format!("{name} is not in the local index"));
        };
        Ok(Jump {
            label: def.label,
            file: def.file,
            line: def.line,
            column: def.column,
        })
    }

    pub fn type_jump(&self, line: usize, column: usize) -> Result<Jump, String> {
        let Some(open) = self.open.as_ref() else {
            return Err("explore has not read this buffer yet".to_string());
        };
        let analysis = &open.analysis;
        let type_name = type_under_cursor(analysis, &self.index, &open.key, line, column).ok_or_else(|| {
            "no explicit type here. Annotations and signatures are visible; inferred types are not."
                .to_string()
        })?;
        let Some(def) = self.index.best_definition(&type_name, &open.key) else {
            return Err(format!("{type_name} has no definition in the index"));
        };
        Ok(Jump {
            label: type_name,
            file: def.file.clone(),
            line: def.line,
            column: def.column,
        })
    }

    pub fn scan_workspace(&mut self, root: &Path, registry: &LanguageRegistry) -> ScanStats {
        self.pr_mode = false;
        self.index.scan_workspace(root, registry)
    }

    pub fn scan_pr(
        &mut self,
        root: &Path,
        registry: &LanguageRegistry,
        base: Option<&str>,
    ) -> Result<ScanStats, String> {
        self.pr_mode = true;
        self.index.scan_pr(root, registry, base)
    }

    pub fn root_for(&self, file: &str) -> Option<PathBuf> {
        let path = Path::new(file);
        repo_root(path).or_else(|| path.parent().map(Path::to_path_buf))
    }
}

fn analyze_source(registry: &LanguageRegistry, language: &str, source: &str) -> Analysis {
    analyze::analyze(registry, language, source)
}

fn file_label(path: &str) -> String {
    if path.is_empty() {
        return "buffer".to_string();
    }
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_string()
}

fn same_jump(left: &Jump, right: &Jump) -> bool {
    left.file == right.file && left.line == right.line && left.column == right.column
}

fn resolve_definition(
    analysis: Option<&Analysis>,
    index: &Index,
    file_key: &str,
    function: Option<&analyze::FunctionScope>,
    name: Option<&analyze::NameOcc>,
) -> Option<ViewDef> {
    let symbol = name?;
    if symbol.role == Role::Definition {
        if let Some(def) = analysis.and_then(|analysis| {
            analysis
                .definitions
                .iter()
                .find(|def| def.name_span == symbol.span)
        }) {
            return Some(view_def(
                &def.name,
                &def.kind,
                file_key,
                def.name_span.start_line,
                def.name_span.start_col,
                &def.signature,
                index.reference_count(&def.name),
            ));
        }
    }
    if let Some(binding) = analysis.and_then(|analysis| {
        analysis.binding_named(symbol.name.as_str(), function.map(|function| function.span))
    }) {
        let signature = match binding.type_name.as_deref() {
            Some(type_name) => format!("type {type_name}"),
            None => "local name".to_string(),
        };
        return Some(view_def(
            &binding.name,
            "let",
            file_key,
            binding.span.start_line,
            binding.span.start_col,
            &signature,
            index.reference_count(&binding.name),
        ));
    }
    index.best_definition(&symbol.name, file_key).map(|def| {
        view_def(
            &symbol.name,
            &def.kind,
            &def.file,
            def.line,
            def.column,
            &def.signature,
            index.reference_count(&symbol.name),
        )
    })
}

fn view_def(
    name: &str,
    kind: &str,
    file: &str,
    line: usize,
    column: usize,
    signature: &str,
    refs: usize,
) -> ViewDef {
    ViewDef {
        label: name.to_string(),
        kind: kind.to_string(),
        file: file.to_string(),
        line,
        column,
        signature: signature.to_string(),
        refs,
    }
}

fn follow_spans(
    follow: Option<&FollowLock>,
    analysis: Option<&Analysis>,
    file_key: &str,
) -> (Option<String>, Vec<Span>) {
    let Some(follow) = follow else {
        return (None, Vec::new());
    };
    let Some(analysis) = analysis else {
        return (Some(follow.name.clone()), Vec::new());
    };
    let spans = if follow.file_wide {
        analysis
            .names
            .iter()
            .filter(|name| name.name == follow.name)
            .take(80)
            .map(|name| name.span)
            .collect()
    } else {
        analysis
            .functions
            .iter()
            .filter(|function| {
                function.name == follow.function
                    && function.span.start_line == follow.function_start
            })
            .flat_map(|function| {
                analysis
                    .names
                    .iter()
                    .filter(move |name| {
                        name.name == follow.name && function.span.contains_span(name.span)
                    })
                    .map(|name| name.span)
            })
            .collect()
    };
    let _ = file_key;
    (Some(follow.name.clone()), spans)
}

fn navigation_order(references: &[ViewRef]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..references.len()).collect();
    order.sort_by(|&left, &right| {
        let left = &references[left];
        let right = &references[right];
        (&left.file, left.line, left.column).cmp(&(&right.file, right.line, right.column))
    });
    order.dedup_by(|&mut left, &mut right| {
        let left = &references[left];
        let right = &references[right];
        left.file == right.file && left.line == right.line && left.column == right.column
    });
    order
}

fn skeleton_zone(
    enabled: bool,
    analysis: Option<&Analysis>,
    function: Option<&analyze::FunctionScope>,
    index: &Index,
    file_key: &str,
) -> Option<DimZone> {
    if !enabled {
        return None;
    }
    let analysis = analysis?;
    let function = function?;
    let mut keep = vec![function.signature];
    let file_is_in_scope = index.file_in_scope(file_key);
    for call in &function.calls {
        let local = file_is_in_scope
            && analysis
                .definitions
                .iter()
                .any(|def| def.name == call.name && def.kind == "fn");
        let repo = index.defined_in_scope(&call.name) || local;
        if call.syscall || repo || is_syscall_wrapper(&call.path, &call.name, call.is_macro) {
            keep.push(call.call);
        }
    }
    let holes = analysis
        .functions
        .iter()
        .filter(|other| {
            other.span.start_byte != function.span.start_byte
                && function.span.contains_span(other.span)
        })
        .map(|other| other.span)
        .collect();
    Some(DimZone {
        zone: function.span,
        keep,
        holes,
    })
}

fn badges(analysis: Option<&Analysis>, index: &Index, file_key: &str) -> Vec<(usize, usize)> {
    let Some(analysis) = analysis else {
        return Vec::new();
    };
    let mut badges: Vec<(usize, usize)> = Vec::new();
    for def in &analysis.definitions {
        if !matches!(
            def.kind.as_str(),
            "fn" | "struct" | "enum" | "trait" | "type"
        ) {
            continue;
        }
        let count = index.reference_count(&def.name);
        if let Some(existing) = badges
            .iter_mut()
            .find(|(line, _)| *line == def.name_span.start_line)
        {
            existing.1 = existing.1.max(count);
        } else {
            badges.push((def.name_span.start_line, count));
        }
    }
    let _ = file_key;
    badges
}

fn type_under_cursor(
    analysis: &Analysis,
    index: &Index,
    file_key: &str,
    line: usize,
    column: usize,
) -> Option<String> {
    let name = analysis.name_at(line, column)?;
    if name.role == Role::Type {
        return Some(name.name.clone());
    }
    if let Some(function) = analysis.function_at(line, column) {
        if let Some(binding) = analysis.binding_named(&name.name, Some(function.span)) {
            if let Some(type_name) = binding.type_name.clone() {
                return Some(type_name);
            }
        }
        if let Some(call) = function
            .calls
            .iter()
            .filter(|call| call.call.contains_pos(line, column))
            .min_by_key(|call| call.call.len())
        {
            if let Some(def) = index.best_definition(&call.name, file_key) {
                if let Some(type_name) = def
                    .return_type
                    .clone()
                    .or_else(|| last_type_name(def.signature.split("->").nth(1).unwrap_or("")))
                {
                    return Some(type_name);
                }
            }
            if let Some(local) = analysis
                .definitions
                .iter()
                .find(|def| def.name == call.name)
            {
                if let Some(type_name) = local.return_type.clone() {
                    return Some(type_name);
                }
            }
        }
    }
    if name.role == Role::Definition {
        let def = analysis
            .definitions
            .iter()
            .find(|def| def.name_span == name.span)?;
        return def.return_type.clone();
    }
    None
}

pub fn sidebar_columns(inner_width: usize) -> usize {
    if inner_width >= 110 {
        36
    } else if inner_width >= 78 {
        28
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_with(source: &str) -> (Session, LanguageRegistry) {
        let registry = LanguageRegistry::bundled();
        let mut session = Session {
            enabled: true,
            ..Session::default()
        };
        session.sync(SyncInput {
            key: "/repo/review.rs",
            revision: 1,
            language: Some("rust"),
            source,
            registry: &registry,
        });
        (session, registry)
    }

    const SOURCE: &str = r#"
struct LaunchSpec;
fn helper() -> LaunchSpec { LaunchSpec }
fn review(owned: bool) -> bool {
    let template: LaunchSpec = helper();
    let worker = template;
    std::fs::read_to_string("log").ok();
    if worker.starts_with("a") {
        helper();
    }
    template
}
fn other() {
    let template = 1;
    template
}
"#;

    #[test]
    fn follow_stays_inside_the_function() {
        let (mut session, _) = session_with(SOURCE);
        let (line, column) = {
            let analysis = &session.open.as_ref().unwrap().analysis;
            let template = analysis
                .names
                .iter()
                .find(|name| {
                    name.name == "template"
                        && analysis
                            .function_at(name.span.start_line, name.span.start_col)
                            .is_some_and(|function| function.name == "review")
                })
                .unwrap();
            (template.span.start_line, template.span.start_col)
        };
        session.toggle_follow(line, column).unwrap();
        let view = session.view(line, column);
        assert_eq!(view.following.as_deref(), Some("template"));
        assert!(view.follow_spans.len() >= 2);
        assert!(view.follow_spans.len() < 5);
        assert_eq!(view.in_function.as_deref(), Some("review"));
        assert!(
            view.cursor_syntax.contains("variable")
                || view.cursor_role == "name"
                || !view.cursor_syntax.is_empty()
        );
    }

    #[test]
    fn skeleton_keeps_repo_calls_and_syscalls() {
        let (mut session, _) = session_with(SOURCE);
        session.skeleton = true;
        let analysis = &session.open.as_ref().unwrap().analysis;
        let inside = analysis
            .functions
            .iter()
            .find(|function| function.name == "review")
            .unwrap()
            .span;
        let view = session.view(inside.start_line + 1, 4);
        let dim = view.dim.expect("dim zone");
        let review = analysis
            .functions
            .iter()
            .find(|function| function.name == "review")
            .unwrap();
        let helper = review
            .calls
            .iter()
            .find(|call| call.name == "helper")
            .unwrap();
        let read = review
            .calls
            .iter()
            .find(|call| call.name == "read_to_string")
            .unwrap();
        let starts = review
            .calls
            .iter()
            .find(|call| call.name == "starts_with")
            .unwrap();
        assert!(dim.keep.iter().any(|span| span.contains_span(helper.call)));
        assert!(dim.keep.iter().any(|span| span.contains_span(read.call)));
        assert!(!dim.keep.iter().any(|span| span.contains_span(starts.call)));
    }

    #[test]
    fn type_jump_uses_the_annotation() {
        let (session, _) = session_with(SOURCE);
        let analysis = &session.open.as_ref().unwrap().analysis;
        let template = analysis.binding_named("template", None).unwrap();
        let jump = session
            .type_jump(template.span.start_line, template.span.start_col)
            .unwrap();
        assert_eq!(jump.label, "LaunchSpec");
        assert_eq!(
            jump.line,
            analysis
                .definitions
                .iter()
                .find(|def| def.name == "LaunchSpec")
                .unwrap()
                .name_span
                .start_line
        );
    }

    #[test]
    fn trail_moves_back_and_forward() {
        let (mut session, _) = session_with(SOURCE);
        session.record_jump(
            Jump {
                label: "review".into(),
                file: "/repo/review.rs".into(),
                line: 3,
                column: 0,
            },
            Jump {
                label: "helper".into(),
                file: "/repo/review.rs".into(),
                line: 2,
                column: 3,
            },
        );
        let back = session.back().unwrap();
        assert_eq!(back.label, "review");
        let forward = session.forward().unwrap();
        assert_eq!(forward.label, "helper");
        assert!(session.forward().is_none());
    }

    #[test]
    fn reference_badges_count_name_matches() {
        let (session, _) = session_with(SOURCE);
        let view = session.view(0, 0);
        let helper_line = session
            .open
            .as_ref()
            .unwrap()
            .analysis
            .definitions
            .iter()
            .find(|def| def.name == "helper")
            .unwrap()
            .name_span
            .start_line;
        let badge = view
            .badges
            .iter()
            .find(|(line, _)| *line == helper_line)
            .unwrap();
        assert!(badge.1 >= 2, "helper refs: {}", badge.1);
    }

    #[test]
    fn next_match_visits_every_occurrence_across_functions() {
        let source = r#"
fn alpha() {
    let marker = 1;
    marker
}
fn beta() {
    let marker = 2;
    marker
}
"#;
        let (mut session, _) = session_with(source);
        let mut positions = session
            .open
            .as_ref()
            .unwrap()
            .analysis
            .names
            .iter()
            .filter(|name| name.name == "marker")
            .map(|name| (name.span.start_line, name.span.start_col))
            .collect::<Vec<_>>();
        positions.sort();
        positions.dedup();
        assert!(positions.len() >= 4, "{positions:?}");
        let (mut line, mut column) = positions[0];
        let mut seen = Vec::new();
        for _ in 0..positions.len() {
            let jump = session.step_reference(line, column, true).unwrap();
            seen.push((jump.line, jump.column));
            line = jump.line;
            column = jump.column;
        }
        let mut sorted = seen.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted, positions, "visited {seen:?}");
        assert_eq!(seen.last().copied(), Some(positions[0]));
    }

    #[test]
    fn review_scope_dims_helpers_outside_the_changed_files() {
        let (mut session, _) = session_with(SOURCE);
        session.skeleton = true;
        session.index.pr_files = Some(std::collections::BTreeSet::from(["/repo/other.rs".into()]));
        let (helper, read, line) = call_spans(&session);
        let dim = session.view(line, 4).dim.expect("dim zone inside review");
        assert!(
            !dim.keep.iter().any(|span| span.contains_span(helper)),
            "an unchanged file's helper must dim under review scope"
        );
        assert!(dim.keep.iter().any(|span| span.contains_span(read)));
    }

    #[test]
    fn review_scope_keeps_helpers_in_a_changed_file() {
        let (mut session, _) = session_with(SOURCE);
        session.skeleton = true;
        session.index.pr_files = Some(std::collections::BTreeSet::from(["/repo/review.rs".into()]));
        let (helper, _, line) = call_spans(&session);
        let dim = session.view(line, 4).dim.expect("dim zone inside review");
        assert!(dim.keep.iter().any(|span| span.contains_span(helper)));
    }

    fn call_spans(session: &Session) -> (Span, Span, usize) {
        let analysis = &session.open.as_ref().unwrap().analysis;
        let review = analysis
            .functions
            .iter()
            .find(|function| function.name == "review")
            .unwrap();
        let helper = review
            .calls
            .iter()
            .find(|call| call.name == "helper")
            .unwrap()
            .call;
        let read = review
            .calls
            .iter()
            .find(|call| call.name == "read_to_string")
            .unwrap()
            .call;
        (helper, read, review.span.start_line + 1)
    }
}
