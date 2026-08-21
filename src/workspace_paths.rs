//! Shared, ignore-aware workspace discovery and bounded fuzzy tree projections.
//!
//! Blocking directory traversal remains on background workers. Neo-tree reuses a
//! bounded path index across queries and receives only ranked matches plus ancestors,
//! keeping filesystem breadth outside the Husk instruction budget.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::Context;
use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use ignore::{DirEntry, WalkBuilder};
use once_cell::sync::Lazy;
use serde::Serialize;

const MAX_INDEXED_PATHS: usize = 100_000;
const MAX_TREE_SEARCH_MATCHES: usize = 48;
const FILENAME_MATCH_BONUS: i64 = 120;
const EXACT_NAME_BONUS: i64 = 800;
const PREFIX_NAME_BONUS: i64 = 240;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct WorkspacePath {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) kind: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct WorkspacePathOptions {
    pub(crate) hidden: bool,
    pub(crate) ignored: bool,
    pub(crate) directories: bool,
    pub(crate) max_entries: Option<usize>,
}

#[derive(Clone, Debug)]
struct WorkspacePathIndex {
    entries: Vec<WorkspacePath>,
    truncated: bool,
}

#[derive(Clone, Debug, Serialize)]
struct SearchChildren {
    path: String,
    entries: Vec<WorkspacePath>,
    truncated: bool,
}

#[derive(Clone, Debug, Serialize)]
struct SearchMatch {
    path: String,
    ranges: Vec<[usize; 2]>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct WorkspacePathSearch {
    query: String,
    directories_only: bool,
    children: Vec<SearchChildren>,
    expanded: Vec<String>,
    matches: Vec<SearchMatch>,
    total: usize,
    truncated: bool,
    error: Option<String>,
}

static TREE_PATH_INDEX: Lazy<Mutex<HashMap<PathBuf, Arc<WorkspacePathIndex>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub(crate) fn discover_workspace_paths(
    root: &Path,
    options: WorkspacePathOptions,
) -> anyhow::Result<(Vec<WorkspacePath>, bool)> {
    let honor_ignores = !options.ignored;
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(!options.hidden)
        .ignore(honor_ignores)
        .git_ignore(honor_ignores)
        .git_global(honor_ignores)
        .git_exclude(honor_ignores)
        .follow_links(false)
        .filter_entry(not_vcs_metadata);

    let mut entries = Vec::new();
    let mut truncated = false;
    for result in builder.build() {
        let entry = result.with_context(|| format!("failed to walk {}", root.display()))?;
        if entry.depth() == 0 {
            continue;
        }
        let Some(kind) = entry_kind(&entry) else {
            continue;
        };
        if kind == "directory" && !options.directories {
            continue;
        }
        if options
            .max_entries
            .is_some_and(|limit| entries.len() >= limit)
        {
            truncated = true;
            break;
        }
        let relative = entry.path().strip_prefix(root).with_context(|| {
            format!(
                "failed to make {} relative to {}",
                entry.path().display(),
                root.display()
            )
        })?;
        entries.push(WorkspacePath {
            name: entry.file_name().to_string_lossy().into_owned(),
            path: relative.to_string_lossy().replace('\\', "/"),
            kind,
        });
    }
    entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    Ok((entries, truncated))
}

fn entry_kind(entry: &DirEntry) -> Option<&'static str> {
    let kind = entry.file_type()?;
    if kind.is_file() {
        Some("file")
    } else if kind.is_dir() {
        Some("directory")
    } else if kind.is_symlink() {
        let target = std::fs::metadata(entry.path()).ok()?;
        if target.is_dir() {
            Some("directory")
        } else {
            target.is_file().then_some("file")
        }
    } else {
        None
    }
}

fn not_vcs_metadata(entry: &DirEntry) -> bool {
    entry.depth() == 0 || !matches!(entry.file_name().to_str(), Some(".git" | ".bare"))
}

pub(crate) fn invalidate_workspace_path_index(root: &Path) {
    if let Ok(mut indexes) = TREE_PATH_INDEX.lock() {
        indexes.remove(root);
    }
}

fn workspace_path_index(root: &Path) -> anyhow::Result<Arc<WorkspacePathIndex>> {
    {
        let indexes = TREE_PATH_INDEX
            .lock()
            .map_err(|_| anyhow::anyhow!("workspace path index lock was poisoned"))?;
        if let Some(index) = indexes.get(root) {
            return Ok(Arc::clone(index));
        }
    }
    let (entries, truncated) = discover_workspace_paths(
        root,
        WorkspacePathOptions {
            hidden: true,
            ignored: false,
            directories: true,
            max_entries: Some(MAX_INDEXED_PATHS),
        },
    )?;
    let index = Arc::new(WorkspacePathIndex { entries, truncated });
    let mut indexes = TREE_PATH_INDEX
        .lock()
        .map_err(|_| anyhow::anyhow!("workspace path index lock was poisoned"))?;
    Ok(Arc::clone(
        indexes.entry(root.to_path_buf()).or_insert(index),
    ))
}

pub(crate) fn search_workspace_paths(
    root: &Path,
    query: &str,
    directories_only: bool,
) -> WorkspacePathSearch {
    let mut response = WorkspacePathSearch {
        query: query.to_string(),
        directories_only,
        children: Vec::new(),
        expanded: vec![".".to_string()],
        matches: Vec::new(),
        total: 0,
        truncated: false,
        error: None,
    };
    if query.trim().is_empty() {
        return response;
    }
    let index = match workspace_path_index(root) {
        Ok(index) => index,
        Err(error) => {
            response.error = Some(error.to_string());
            return response;
        }
    };
    let matcher = SkimMatcherV2::default().smart_case();
    let tokens = query.split_whitespace().collect::<Vec<_>>();
    let mut ranked = index
        .entries
        .iter()
        .filter(|entry| !directories_only || entry.kind == "directory")
        .filter_map(|entry| search_score(&matcher, entry, &tokens).map(|score| (entry, score)))
        .collect::<Vec<_>>();
    ranked.sort_unstable_by(|(left, left_score), (right, right_score)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.path.len().cmp(&right.path.len()))
            .then_with(|| left.path.cmp(&right.path))
    });
    response.total = ranked.len();
    response.truncated = index.truncated || ranked.len() > MAX_TREE_SEARCH_MATCHES;
    ranked.truncate(MAX_TREE_SEARCH_MATCHES);

    let mut children: BTreeMap<String, HashMap<String, (WorkspacePath, i64)>> = BTreeMap::new();
    for (entry, score) in ranked {
        let full_path = format!("./{}", entry.path);
        response.matches.push(SearchMatch {
            path: full_path.clone(),
            ranges: filename_match_ranges(&matcher, &entry.name, &tokens),
        });
        let parts = entry.path.split('/').collect::<Vec<_>>();
        let mut parent = ".".to_string();
        for (index, name) in parts.iter().enumerate() {
            let path = if parent == "." {
                format!("./{name}")
            } else {
                format!("{parent}/{name}")
            };
            let kind = if index + 1 == parts.len() {
                entry.kind
            } else {
                "directory"
            };
            let candidate = WorkspacePath {
                name: (*name).to_string(),
                path: path.clone(),
                kind,
            };
            children
                .entry(parent.clone())
                .or_default()
                .entry(path.clone())
                .and_modify(|(_, existing_score)| *existing_score = (*existing_score).max(score))
                .or_insert((candidate, score));
            if index + 1 < parts.len() && !response.expanded.contains(&path) {
                response.expanded.push(path.clone());
            }
            parent = path;
        }
    }
    response.children = children
        .into_iter()
        .map(|(path, entries)| {
            let mut entries = entries.into_values().collect::<Vec<_>>();
            entries.sort_unstable_by(|(left, left_score), (right, right_score)| {
                right_score
                    .cmp(left_score)
                    .then_with(|| match (left.kind, right.kind) {
                        ("directory", "file") => Ordering::Less,
                        ("file", "directory") => Ordering::Greater,
                        _ => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
                    })
            });
            SearchChildren {
                path,
                entries: entries.into_iter().map(|(entry, _)| entry).collect(),
                truncated: false,
            }
        })
        .collect();
    response
}

fn search_score(matcher: &SkimMatcherV2, entry: &WorkspacePath, tokens: &[&str]) -> Option<i64> {
    let mut score = 0i64;
    for token in tokens {
        let path_score = matcher.fuzzy_match(&entry.path, token)?;
        score = score.saturating_add(path_score);
        if let Some(name_score) = matcher.fuzzy_match(&entry.name, token) {
            score = score
                .saturating_add(name_score)
                .saturating_add(FILENAME_MATCH_BONUS);
            if entry.name.eq_ignore_ascii_case(token) {
                score = score.saturating_add(EXACT_NAME_BONUS);
            } else if entry.name.to_lowercase().starts_with(&token.to_lowercase()) {
                score = score.saturating_add(PREFIX_NAME_BONUS);
            }
        }
    }
    Some(score)
}

fn filename_match_ranges(matcher: &SkimMatcherV2, name: &str, tokens: &[&str]) -> Vec<[usize; 2]> {
    let mut indices = tokens
        .iter()
        .filter_map(|token| matcher.fuzzy_indices(name, token))
        .flat_map(|(_, indices)| indices)
        .collect::<Vec<_>>();
    indices.sort_unstable();
    indices.dedup();
    let mut ranges: Vec<[usize; 2]> = Vec::new();
    for index in indices {
        if let Some(previous) = ranges.last_mut() {
            if previous[1] == index {
                previous[1] += 1;
                continue;
            }
        }
        ranges.push([index, index + 1]);
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_hidden_paths_but_excludes_git_metadata_and_ignored_entries() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".github/workflows")).unwrap();
        std::fs::create_dir_all(root.path().join(".git/objects")).unwrap();
        std::fs::create_dir_all(root.path().join("target/debug")).unwrap();
        std::fs::write(root.path().join(".gitignore"), "target/\n").unwrap();
        std::fs::write(root.path().join(".github/workflows/build.yml"), "").unwrap();
        std::fs::write(root.path().join(".git/objects/secret"), "").unwrap();
        std::fs::write(root.path().join("target/debug/generated"), "").unwrap();

        let (entries, truncated) = discover_workspace_paths(
            root.path(),
            WorkspacePathOptions {
                hidden: true,
                ignored: false,
                directories: true,
                max_entries: None,
            },
        )
        .unwrap();

        assert!(!truncated);
        assert!(entries
            .iter()
            .any(|entry| entry.path == ".github/workflows/build.yml"));
        assert!(!entries.iter().any(|entry| entry.path.starts_with(".git/")));
        assert!(!entries
            .iter()
            .any(|entry| entry.path.starts_with("target/")));
    }

    #[test]
    fn searches_multiple_full_path_words_and_preserves_ancestors() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src/ui")).unwrap();
        std::fs::write(root.path().join("src/ui/file_picker.rs"), "").unwrap();

        let result = search_workspace_paths(root.path(), "ui pick", false);

        assert_eq!(result.total, 1);
        assert_eq!(result.matches[0].path, "./src/ui/file_picker.rs");
        assert_eq!(result.expanded, [".", "./src", "./src/ui"]);
        assert_eq!(result.children.len(), 3);
        assert!(!result.matches[0].ranges.is_empty());
    }

    #[test]
    fn directory_only_search_excludes_matching_files() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("picker/components")).unwrap();
        std::fs::write(root.path().join("picker.rs"), "").unwrap();

        let result = search_workspace_paths(root.path(), "picker", true);

        assert!(result
            .matches
            .iter()
            .all(|entry| entry.path != "./picker.rs"));
        assert!(result.matches.iter().any(|entry| entry.path == "./picker"));
    }

    #[test]
    fn search_reaches_entries_beyond_directory_listing_limit() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..220 {
            std::fs::write(root.path().join(format!("entry-{index:03}.rs")), "").unwrap();
        }
        std::fs::write(root.path().join("needle-file.rs"), "").unwrap();

        let result = search_workspace_paths(root.path(), "needle", false);

        assert_eq!(result.matches[0].path, "./needle-file.rs");
        assert!(result.children[0]
            .entries
            .iter()
            .any(|entry| entry.path == "./needle-file.rs"));
    }

    #[test]
    fn exact_filename_matches_outrank_matches_in_parent_directories() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("needle/nested")).unwrap();
        std::fs::write(root.path().join("needle/nested/other.rs"), "").unwrap();
        std::fs::write(root.path().join("needle.rs"), "").unwrap();

        let result = search_workspace_paths(root.path(), "needle.rs", false);

        assert_eq!(result.matches[0].path, "./needle.rs");
    }

    #[test]
    fn bounded_search_projection_reports_the_complete_match_count() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..80 {
            std::fs::write(root.path().join(format!("match-{index:03}.rs")), "").unwrap();
        }

        let result = search_workspace_paths(root.path(), "match", false);

        assert_eq!(result.total, 80);
        assert_eq!(result.matches.len(), MAX_TREE_SEARCH_MATCHES);
        assert!(result.truncated);
    }

    #[test]
    fn unicode_filename_highlights_use_character_offsets() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("世picker.rs"), "").unwrap();

        let result = search_workspace_paths(root.path(), "pick", false);

        assert_eq!(result.matches[0].ranges, [[1, 5]]);
    }

    #[test]
    fn invalidation_refreshes_cached_workspace_entries() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("old.rs"), "").unwrap();
        assert_eq!(search_workspace_paths(root.path(), "new", false).total, 0);

        std::fs::write(root.path().join("new.rs"), "").unwrap();
        invalidate_workspace_path_index(root.path());

        assert_eq!(search_workspace_paths(root.path(), "new", false).total, 1);
    }
}
