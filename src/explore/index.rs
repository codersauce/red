//! File-scoped name index. Updates replace one file at a time.
//!
//! Matches are textual. Two different `template` bindings share a bucket.
//! A `.scip` file is noted when it exists and is not parsed: SCIP is protobuf,
//! and this index is the tree-sitter stand-in.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::analyze::{Analysis, Role, analyze};
use crate::highlighter::LanguageRegistry;

pub const MAX_SCAN_FILES: usize = 400;
const MAX_FILE_BYTES: u64 = 512 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefLoc {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub kind: String,
    pub signature: String,
    pub return_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefLoc {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub role: Role,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexedFile {
    hash: u64,
    definitions: Vec<DefLoc>,
    references: Vec<(String, RefLoc)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanStats {
    pub files: usize,
    pub truncated: bool,
    pub message: String,
}

#[derive(Debug, Default, Clone)]
pub struct Index {
    pub root: Option<PathBuf>,
    files: BTreeMap<String, IndexedFile>,
    by_name: HashMap<String, Vec<DefLoc>>,
    refs: HashMap<String, Vec<RefLoc>>,
    pub pr_files: Option<BTreeSet<String>>,
    pub pr_base: Option<String>,
    pub pr_summary: String,
    pub truncated: bool,
    pub scip_present: bool,
    pub message: String,
}

impl Index {
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn summary(&self) -> String {
        let cap = if self.truncated { " · capped" } else { "" };
        match &self.pr_files {
            Some(changed) => format!(
                "Review · {} changed · {} files{cap}",
                changed.len(),
                self.files.len()
            ),
            None => format!("{} files{cap}", self.files.len()),
        }
    }

    pub fn note(&self) -> String {
        let mut note = String::from("Same spelling. gd asks the language server.");
        if self.scip_present {
            note = format!("A SCIP file is here and not loaded. {note}");
        }
        if !self.message.is_empty() {
            note = format!("{} {note}", self.message);
        }
        note
    }

    pub fn upsert(&mut self, file: &str, hash: u64, analysis: &Analysis) {
        if analysis.language.is_none() {
            return;
        }
        if self
            .files
            .get(file)
            .is_some_and(|existing| existing.hash == hash)
        {
            return;
        }
        self.forget(file);
        let mut definitions = Vec::new();
        for def in &analysis.definitions {
            let loc = DefLoc {
                file: file.to_string(),
                line: def.name_span.start_line,
                column: def.name_span.start_col,
                kind: def.kind.clone(),
                signature: def.signature.clone(),
                return_type: def.return_type.clone(),
            };
            self.by_name
                .entry(def.name.clone())
                .or_default()
                .push(loc.clone());
            definitions.push(loc);
        }
        let mut references = Vec::new();
        for name in &analysis.names {
            if name.role == Role::Definition {
                continue;
            }
            let loc = RefLoc {
                file: file.to_string(),
                line: name.span.start_line,
                column: name.span.start_col,
                role: name.role,
            };
            references.push((name.name.clone(), loc.clone()));
            self.refs.entry(name.name.clone()).or_default().push(loc);
        }
        self.files.insert(
            file.to_string(),
            IndexedFile {
                hash,
                definitions,
                references,
            },
        );
    }

    fn forget(&mut self, file: &str) {
        let Some(old) = self.files.remove(file) else {
            return;
        };
        self.by_name.retain(|_, list| {
            list.retain(|item| item.file != file);
            !list.is_empty()
        });
        for (name, _) in &old.references {
            if let Some(list) = self.refs.get_mut(name) {
                list.retain(|item| item.file != file);
            }
        }
        self.refs.retain(|_, list| !list.is_empty());
    }

    fn clear_files(&mut self) {
        self.files.clear();
        self.by_name.clear();
        self.refs.clear();
    }

    pub fn definitions<'a>(&'a self, name: &str) -> impl Iterator<Item = &'a DefLoc> {
        self.by_name.get(name).into_iter().flatten()
    }

    pub fn best_definition(&self, name: &str, current_file: &str) -> Option<&DefLoc> {
        self.definitions(name).min_by_key(|def| {
            let same = usize::from(def.file != current_file);
            let pr = match &self.pr_files {
                Some(files) => usize::from(!files.contains(&def.file)),
                None => 0,
            };
            (same, pr, def.file.len(), def.line)
        })
    }

    pub fn reference_count(&self, name: &str) -> usize {
        self.refs.get(name).map(Vec::len).unwrap_or(0)
    }

    pub fn references<'a>(&'a self, name: &str) -> impl Iterator<Item = &'a RefLoc> {
        self.refs.get(name).into_iter().flatten()
    }

    pub fn defined_in_scope(&self, name: &str) -> bool {
        self.definitions(name)
            .any(|def| self.file_in_scope(&def.file))
    }

    pub fn clear_pr_scope(&mut self) {
        self.pr_files = None;
        self.pr_base = None;
        self.pr_summary.clear();
    }

    pub fn file_in_scope(&self, file: &str) -> bool {
        match &self.pr_files {
            Some(files) => files.contains(file),
            None => self.files.contains_key(file),
        }
    }

    pub fn scan_workspace(&mut self, root: &Path, registry: &LanguageRegistry) -> ScanStats {
        self.clear_files();
        self.pr_files = None;
        self.pr_base = None;
        self.pr_summary.clear();
        self.scan_paths(root, registry, None)
    }

    pub fn scan_pr(
        &mut self,
        root: &Path,
        registry: &LanguageRegistry,
        base: Option<&str>,
    ) -> Result<ScanStats, String> {
        let base = resolve_base(root, base);
        let changed = changed_files(root, &base)?;
        let summary = diff_summary(root, &base);
        self.pr_base = Some(base);
        self.pr_summary = summary;
        self.pr_files = Some(
            changed
                .iter()
                .map(|path| normalize_path(&path.to_string_lossy()))
                .collect(),
        );
        let stats = self.scan_paths(root, registry, Some(&changed));
        Ok(stats)
    }

    fn scan_paths(
        &mut self,
        root: &Path,
        registry: &LanguageRegistry,
        only: Option<&[PathBuf]>,
    ) -> ScanStats {
        self.root = Some(root.to_path_buf());
        self.scip_present = scip_present(root);
        self.truncated = false;
        let mut files = 0usize;
        let mut skipped = 0usize;
        let paths: Vec<PathBuf> = if let Some(only) = only {
            only.to_vec()
        } else {
            workspace_files(root)
        };
        if paths.len() > MAX_SCAN_FILES {
            self.truncated = true;
        }
        for path in paths.into_iter().take(MAX_SCAN_FILES) {
            if !path.is_file() {
                skipped += 1;
                continue;
            }
            let Ok(meta) = path.metadata() else {
                skipped += 1;
                continue;
            };
            if meta.len() > MAX_FILE_BYTES {
                skipped += 1;
                continue;
            }
            let Some(language) = language_id_from_path(&path) else {
                continue;
            };
            if registry.grammar(language).is_none() {
                skipped += 1;
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                skipped += 1;
                continue;
            };
            let key = normalize_path(&path.to_string_lossy());
            let hash = hash_text(&text);
            let analysis = analyze(registry, language, &text);
            self.upsert(&key, hash, &analysis);
            files += 1;
        }
        let message = if files == 0 {
            "no source files indexed".to_string()
        } else if skipped > 0 {
            format!("{skipped} paths skipped")
        } else {
            String::new()
        };
        self.message = message.clone();
        ScanStats {
            files,
            truncated: self.truncated,
            message,
        }
    }
}

pub fn hash_text(text: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

pub fn normalize_path(path: &str) -> String {
    if path.starts_with("buffer:") {
        return path.to_string();
    }
    match Path::new(path).absolutize() {
        Ok(absolute) => absolute.to_string_lossy().into_owned(),
        Err(_) => path.to_string(),
    }
}

use path_absolutize::Absolutize;

pub fn language_id_from_path(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?;
    Some(match extension {
        "rs" => "rust",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "jsx",
        "ts" => "typescript",
        "tsx" => "tsx",
        "lua" => "lua",
        "sh" | "bash" => "bash",
        "fish" => "fish",
        "ps1" => "powershell",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        _ => return None,
    })
}

pub fn repo_root(start: &Path) -> Option<PathBuf> {
    let dir = if start.is_file() {
        start.parent()?
    } else {
        start
    };
    git(dir, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

fn workspace_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .standard_filters(true)
        .hidden(true)
        .max_depth(Some(16))
        .build();
    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        if path.components().any(|component| {
            matches!(
                component.as_os_str().to_str(),
                Some("target" | "node_modules" | "vendor" | "dist" | ".git")
            )
        }) {
            continue;
        }
        if language_id_from_path(path).is_some() {
            files.push(path.to_path_buf());
        }
    }
    files.sort();
    files
}

fn scip_present(root: &Path) -> bool {
    if root.join("index.scip").is_file() {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "scip")
    })
}

fn resolve_base(root: &Path, requested: Option<&str>) -> String {
    if let Some(base) = requested.map(str::trim).filter(|base| !base.is_empty()) {
        return base.to_string();
    }
    for candidate in ["origin/main", "origin/master", "main", "master"] {
        if git(root, &["rev-parse", "--verify", "--quiet", candidate]).is_some() {
            return candidate.to_string();
        }
    }
    "HEAD".to_string()
}

fn changed_files(root: &Path, base: &str) -> Result<Vec<PathBuf>, String> {
    let mut names = BTreeSet::new();
    let spec = format!("{base}...HEAD");
    for args in [
        vec!["diff", "--name-only", "--diff-filter=ACMR", spec.as_str()],
        vec!["diff", "--name-only", "--diff-filter=ACMR"],
        vec!["diff", "--name-only", "--cached", "--diff-filter=ACMR"],
    ] {
        if let Some(output) = git(root, &args) {
            for line in output.lines() {
                if !line.trim().is_empty() {
                    names.insert(line.trim().to_string());
                }
            }
        }
    }
    if names.is_empty() && git(root, &["rev-parse", "--is-inside-work-tree"]).is_none() {
        return Err("not a git repository".to_string());
    }
    Ok(names
        .into_iter()
        .map(|name| root.join(name))
        .filter(|path| language_id_from_path(path).is_some())
        .collect())
}

fn diff_summary(root: &Path, base: &str) -> String {
    let spec = format!("{base}...HEAD");
    let Some(output) = git(root, &["diff", "--numstat", spec.as_str()]) else {
        return String::new();
    };
    let mut added = 0usize;
    let mut removed = 0usize;
    for line in output.lines() {
        let mut parts = line.split_whitespace();
        let Some(add) = parts.next().and_then(|value| value.parse::<usize>().ok()) else {
            continue;
        };
        let Some(del) = parts.next().and_then(|value| value.parse::<usize>().ok()) else {
            continue;
        };
        added += add;
        removed += del;
    }
    if added == 0 && removed == 0 {
        String::new()
    } else {
        format!("+{added} −{removed}")
    }
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_replaces_one_file_and_keeps_the_other() {
        let registry = LanguageRegistry::bundled();
        let mut index = Index::default();
        let alpha = "fn alpha() {}\nfn kept() { alpha(); }\n";
        let beta = "fn beta() { alpha(); }\n";
        let alpha_analysis = analyze(&registry, "rust", alpha);
        let beta_analysis = analyze(&registry, "rust", beta);
        index.upsert("a.rs", hash_text(alpha), &alpha_analysis);
        index.upsert("b.rs", hash_text(beta), &beta_analysis);
        assert!(index.best_definition("alpha", "b.rs").is_some());
        assert!(index.reference_count("alpha") >= 1);
        let rewritten = "fn kept() {}\n";
        let rewritten_analysis = analyze(&registry, "rust", rewritten);
        index.upsert("a.rs", hash_text(rewritten), &rewritten_analysis);
        assert!(index.best_definition("alpha", "b.rs").is_none());
        assert!(index.best_definition("beta", "a.rs").is_some());
        assert!(index.best_definition("kept", "b.rs").is_some());
    }

    #[test]
    fn pr_scope_only_counts_changed_files() {
        let mut index = Index::default();
        let registry = LanguageRegistry::bundled();
        let changed = "fn audit() {}\n";
        let other = "fn elsewhere() {}\n";
        index.upsert("/repo/changed.rs", 1, &analyze(&registry, "rust", changed));
        index.upsert("/repo/other.rs", 2, &analyze(&registry, "rust", other));
        index.pr_files = Some(BTreeSet::from(["/repo/changed.rs".to_string()]));
        assert!(index.defined_in_scope("audit"));
        assert!(!index.defined_in_scope("elsewhere"));
        assert!(
            index
                .best_definition("elsewhere", "/repo/changed.rs")
                .is_some()
        );
    }

    #[test]
    fn pr_scan_reads_only_files_git_reports_as_changed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .env("GIT_AUTHOR_NAME", "Explore")
                .env("GIT_AUTHOR_EMAIL", "explore@example.com")
                .env("GIT_COMMITTER_NAME", "Explore")
                .env("GIT_COMMITTER_EMAIL", "explore@example.com")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "{args:?}");
        };
        git(&["init"]);
        std::fs::write(root.join("kept.rs"), "fn kept() {}\n").unwrap();
        git(&["add", "kept.rs"]);
        git(&["commit", "-m", "base"]);
        std::fs::write(
            root.join("kept.rs"),
            "fn kept() {}\nfn extra() { kept(); }\n",
        )
        .unwrap();
        std::fs::write(root.join("ignored.txt"), "not code\n").unwrap();
        let registry = LanguageRegistry::bundled();
        let mut index = Index::default();
        let stats = index.scan_pr(root, &registry, Some("HEAD")).unwrap();
        assert!(stats.files >= 1, "{}", stats.message);
        assert!(index.best_definition("extra", "").is_some());
        assert!(
            index
                .pr_files
                .as_ref()
                .unwrap()
                .iter()
                .all(|path| path.ends_with("kept.rs"))
        );
        assert!(!index.pr_summary.is_empty() || index.pr_base.as_deref() == Some("HEAD"));
    }
}
