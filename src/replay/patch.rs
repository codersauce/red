//! Complete, bounded unified-diff parsing for source-linked replay steps.

use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};

use super::{source::validate_relative_path, ReplayError, ReplayLimits};

static HUNK_HEADER: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@(?: (.*))?$")
        .expect("replay unified hunk expression is valid")
});

/// Complete source file classification, including changes unsafe to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayChangeKind {
    /// Ordinary modifications to an existing UTF-8 source file.
    Modify,
    /// New regular source file opened as an unsaved editor buffer.
    AddFile,
    /// Whole-file deletion reported but not automatically applied.
    DeleteFile,
    /// Source rename reported but not automatically applied.
    Rename,
    /// Binary patch reported but not automatically applied.
    Binary,
    /// Executable or other file-mode-only change.
    ModeChange,
}

impl ReplayChangeKind {
    /// Reports whether this file can produce ordinary editor text steps.
    #[must_use]
    pub const fn supports_text_replay(self) -> bool {
        matches!(self, Self::Modify | Self::AddFile)
    }
}

/// One original Git hunk range using one-based unified-diff coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayHunkRange {
    /// One-based Git hunk start; zero is permitted for an empty file.
    pub start: usize,
    /// Number of old or new source lines consumed.
    pub count: usize,
}

/// Complete original hunk and exact before and after text images.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayHunk {
    /// Original unified-diff hunk header.
    pub header: String,
    /// Original old-file Git line range.
    pub old_range: ReplayHunkRange,
    /// Original target-file Git line range.
    pub new_range: ReplayHunkRange,
    /// Original semantic heading supplied by Git.
    pub heading: String,
    /// Exact context and removed-text image.
    pub before: String,
    /// Exact context and added-text image.
    pub after: String,
    /// Number of changed old lines.
    pub removed_lines: usize,
    /// Number of changed new lines.
    pub added_lines: usize,
    /// Exact first-to-last removed-line range in the original base image.
    #[serde(default)]
    pub removed_range: Option<ReplayHunkRange>,
    /// Exact first-to-last added-line range in the pinned original head image.
    #[serde(default)]
    pub added_range: Option<ReplayHunkRange>,
}

/// A single source-backed file and all of its complete unified hunks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayFilePatch {
    /// Original old-file repository-relative path.
    pub old_path: Option<PathBuf>,
    /// Original target-file repository-relative path.
    pub new_path: Option<PathBuf>,
    /// Classification determining whether editor application is safe.
    pub kind: ReplayChangeKind,
    /// Complete original file metadata and headers.
    pub headers: Vec<String>,
    /// Complete original unified hunks without silent truncation.
    pub hunks: Vec<ReplayHunk>,
}

impl ReplayFilePatch {
    /// Returns the target path, falling back to the original deletion path.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.new_path.as_deref().or(self.old_path.as_deref())
    }
}

/// Complete, bounded, and honestly classified original source patch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayPatch {
    /// Complete changed-file records.
    pub files: Vec<ReplayFilePatch>,
    /// Complete source size before any replay compilation.
    pub bytes: usize,
    /// Complete source line count before any replay compilation.
    pub lines: usize,
}

#[derive(Debug, Default)]
struct FileBuilder {
    old_path: Option<PathBuf>,
    new_path: Option<PathBuf>,
    headers: Vec<String>,
    hunks: Vec<ReplayHunk>,
    binary: bool,
    rename: bool,
    mode_change: bool,
    new_file: bool,
    deleted_file: bool,
}

#[derive(Debug)]
struct HunkBuilder {
    header: String,
    old_range: ReplayHunkRange,
    new_range: ReplayHunkRange,
    heading: String,
    before: String,
    after: String,
    old_seen: usize,
    new_seen: usize,
    removed_lines: usize,
    added_lines: usize,
    first_removed_line: Option<usize>,
    last_removed_line: Option<usize>,
    first_added_line: Option<usize>,
    last_added_line: Option<usize>,
    last_line: Option<HunkLineKind>,
}

#[derive(Debug, Clone, Copy)]
enum HunkLineKind {
    Context,
    Removed,
    Added,
}

/// Parses an entire canonical patch or fails without exposing partial hunks.
pub fn parse_patch(text: &str, limits: ReplayLimits) -> Result<ReplayPatch, ReplayError> {
    if text.len() > limits.max_patch_bytes {
        return Err(ReplayError::LimitExceeded {
            kind: "canonical patch bytes",
            limit: limits.max_patch_bytes,
        });
    }
    let line_count = text.lines().count();
    if line_count > limits.max_patch_lines {
        return Err(ReplayError::LimitExceeded {
            kind: "canonical patch lines",
            limit: limits.max_patch_lines,
        });
    }

    let mut files = Vec::new();
    let mut file: Option<FileBuilder> = None;
    let mut hunk: Option<HunkBuilder> = None;

    for raw in text.split_inclusive('\n') {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line = line.strip_suffix('\r').unwrap_or(line);

        if let Some(header) = line.strip_prefix("diff --git ") {
            finish_hunk(&mut file, &mut hunk)?;
            finish_file(&mut files, &mut file, limits)?;
            let mut next = FileBuilder::default();
            next.headers.push(line.to_string());
            if let Some((old, new)) = header.split_once(" b/") {
                if let Some(old) = old.strip_prefix("a/") {
                    next.old_path = Some(parse_relative_path(old)?);
                    next.new_path = Some(parse_relative_path(new)?);
                }
            }
            file = Some(next);
            continue;
        }

        let Some(current) = file.as_mut() else {
            if !line.trim().is_empty() {
                return Err(ReplayError::InvalidPatch(
                    "content appeared before the first file header".to_string(),
                ));
            }
            continue;
        };

        if line.starts_with("@@ ") {
            finish_hunk(&mut file, &mut hunk)?;
            hunk = Some(parse_hunk_header(line)?);
            continue;
        }

        if let Some(current_hunk) = hunk.as_mut() {
            if line.starts_with("\\ No newline at end of file") {
                remove_missing_final_newline(current_hunk);
                continue;
            }
            let Some(prefix) = raw.chars().next() else {
                return Err(ReplayError::InvalidPatch(
                    "empty line inside a source hunk".to_string(),
                ));
            };
            let payload = &raw[prefix.len_utf8()..];
            match prefix {
                ' ' => {
                    current_hunk.before.push_str(payload);
                    current_hunk.after.push_str(payload);
                    current_hunk.old_seen += 1;
                    current_hunk.new_seen += 1;
                    current_hunk.last_line = Some(HunkLineKind::Context);
                }
                '-' => {
                    let original_line = current_hunk
                        .old_range
                        .start
                        .saturating_add(current_hunk.old_seen);
                    current_hunk.first_removed_line.get_or_insert(original_line);
                    current_hunk.last_removed_line = Some(original_line);
                    current_hunk.before.push_str(payload);
                    current_hunk.old_seen += 1;
                    current_hunk.removed_lines += 1;
                    current_hunk.last_line = Some(HunkLineKind::Removed);
                }
                '+' => {
                    let original_line = current_hunk
                        .new_range
                        .start
                        .saturating_add(current_hunk.new_seen);
                    current_hunk.first_added_line.get_or_insert(original_line);
                    current_hunk.last_added_line = Some(original_line);
                    current_hunk.after.push_str(payload);
                    current_hunk.new_seen += 1;
                    current_hunk.added_lines += 1;
                    current_hunk.last_line = Some(HunkLineKind::Added);
                }
                _ => {
                    return Err(ReplayError::InvalidPatch(format!(
                        "unexpected source hunk line: {line}"
                    )));
                }
            }
            continue;
        }

        current.headers.push(line.to_string());
        if let Some(path) = line.strip_prefix("--- ") {
            current.old_path = parse_side_path(path, "a/")?;
        } else if let Some(path) = line.strip_prefix("+++ ") {
            current.new_path = parse_side_path(path, "b/")?;
        } else if let Some(path) = line.strip_prefix("rename from ") {
            current.old_path = Some(parse_relative_path(path)?);
            current.rename = true;
        } else if let Some(path) = line.strip_prefix("rename to ") {
            current.new_path = Some(parse_relative_path(path)?);
            current.rename = true;
        } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
            current.binary = true;
        } else if line.starts_with("new file mode ") {
            current.new_file = true;
        } else if line.starts_with("deleted file mode ") {
            current.deleted_file = true;
        } else if line.starts_with("old mode ") || line.starts_with("new mode ") {
            current.mode_change = true;
        }
    }

    finish_hunk(&mut file, &mut hunk)?;
    finish_file(&mut files, &mut file, limits)?;
    Ok(ReplayPatch {
        files,
        bytes: text.len(),
        lines: line_count,
    })
}

fn parse_hunk_header(line: &str) -> Result<HunkBuilder, ReplayError> {
    let captures = HUNK_HEADER
        .captures(line)
        .ok_or_else(|| ReplayError::InvalidPatch(format!("malformed hunk header: {line}")))?;
    let number = |index: usize, default: usize| -> Result<usize, ReplayError> {
        captures
            .get(index)
            .map(|value| value.as_str().parse::<usize>())
            .transpose()
            .map(|value| value.unwrap_or(default))
            .map_err(|_| ReplayError::InvalidPatch(format!("invalid hunk range: {line}")))
    };
    Ok(HunkBuilder {
        header: line.to_string(),
        old_range: ReplayHunkRange {
            start: number(1, 0)?,
            count: number(2, 1)?,
        },
        new_range: ReplayHunkRange {
            start: number(3, 0)?,
            count: number(4, 1)?,
        },
        heading: captures
            .get(5)
            .map(|value| value.as_str().to_string())
            .unwrap_or_default(),
        before: String::new(),
        after: String::new(),
        old_seen: 0,
        new_seen: 0,
        removed_lines: 0,
        added_lines: 0,
        first_removed_line: None,
        last_removed_line: None,
        first_added_line: None,
        last_added_line: None,
        last_line: None,
    })
}

fn remove_missing_final_newline(hunk: &mut HunkBuilder) {
    match hunk.last_line {
        Some(HunkLineKind::Context) => {
            strip_one_newline(&mut hunk.before);
            strip_one_newline(&mut hunk.after);
        }
        Some(HunkLineKind::Removed) => strip_one_newline(&mut hunk.before),
        Some(HunkLineKind::Added) => strip_one_newline(&mut hunk.after),
        None => {}
    }
}

fn strip_one_newline(text: &mut String) {
    if text.ends_with('\n') {
        text.pop();
    }
}

fn finish_hunk(
    file: &mut Option<FileBuilder>,
    hunk: &mut Option<HunkBuilder>,
) -> Result<(), ReplayError> {
    let Some(hunk) = hunk.take() else {
        return Ok(());
    };
    if hunk.old_seen != hunk.old_range.count || hunk.new_seen != hunk.new_range.count {
        return Err(ReplayError::InvalidPatch(format!(
            "incomplete source hunk: {}",
            hunk.header
        )));
    }
    let file = file
        .as_mut()
        .ok_or_else(|| ReplayError::InvalidPatch("source hunk has no owning file".to_string()))?;
    file.hunks.push(ReplayHunk {
        header: hunk.header,
        old_range: hunk.old_range,
        new_range: hunk.new_range,
        heading: hunk.heading,
        before: hunk.before,
        after: hunk.after,
        removed_lines: hunk.removed_lines,
        added_lines: hunk.added_lines,
        removed_range: changed_hunk_range(hunk.first_removed_line, hunk.last_removed_line),
        added_range: changed_hunk_range(hunk.first_added_line, hunk.last_added_line),
    });
    Ok(())
}

fn changed_hunk_range(first: Option<usize>, last: Option<usize>) -> Option<ReplayHunkRange> {
    first.zip(last).map(|(start, end)| ReplayHunkRange {
        start,
        count: end.saturating_sub(start).saturating_add(1),
    })
}

fn finish_file(
    files: &mut Vec<ReplayFilePatch>,
    file: &mut Option<FileBuilder>,
    limits: ReplayLimits,
) -> Result<(), ReplayError> {
    let Some(file) = file.take() else {
        return Ok(());
    };
    if files.len() >= limits.max_changed_files {
        return Err(ReplayError::LimitExceeded {
            kind: "changed files",
            limit: limits.max_changed_files,
        });
    }
    let kind = if file.binary {
        ReplayChangeKind::Binary
    } else if file.rename {
        ReplayChangeKind::Rename
    } else if file.deleted_file || file.new_path.is_none() {
        ReplayChangeKind::DeleteFile
    } else if file.new_file || file.old_path.is_none() {
        ReplayChangeKind::AddFile
    } else if file.mode_change && file.hunks.is_empty() {
        ReplayChangeKind::ModeChange
    } else {
        ReplayChangeKind::Modify
    };
    if file.old_path.is_none() && file.new_path.is_none() {
        return Err(ReplayError::InvalidPatch(
            "source file has no safe repository-relative path".to_string(),
        ));
    }
    files.push(ReplayFilePatch {
        old_path: file.old_path,
        new_path: file.new_path,
        kind,
        headers: file.headers,
        hunks: file.hunks,
    });
    Ok(())
}

fn parse_side_path(value: &str, prefix: &str) -> Result<Option<PathBuf>, ReplayError> {
    if value == "/dev/null" {
        return Ok(None);
    }
    let path = value
        .strip_prefix(prefix)
        .ok_or_else(|| ReplayError::InvalidPatch(format!("unexpected source path: {value}")))?;
    parse_relative_path(path).map(Some)
}

fn parse_relative_path(path: &str) -> Result<PathBuf, ReplayError> {
    let path = if path.starts_with('"') {
        serde_json::from_str::<String>(path).map_err(|_| {
            ReplayError::InvalidPatch("unsupported quoted Git source path".to_string())
        })?
    } else {
        path.to_string()
    };
    let path = PathBuf::from(path);
    validate_relative_path(&path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANGE: &str = "diff --git a/src/token.rs b/src/token.rs\nindex 1111111..2222222 100644\n--- a/src/token.rs\n+++ b/src/token.rs\n@@ -1,3 +1,3 @@ fn refresh\n fn refresh() {\n-    old();\n+    new();\n }\n";

    #[test]
    fn parses_complete_source_backed_change_hunks() {
        let patch = parse_patch(CHANGE, ReplayLimits::default()).unwrap();
        assert_eq!(patch.files.len(), 1);
        let file = &patch.files[0];
        assert_eq!(file.kind, ReplayChangeKind::Modify);
        assert_eq!(file.path(), Some(Path::new("src/token.rs")));
        let hunk = &file.hunks[0];
        assert_eq!(hunk.heading, "fn refresh");
        assert_eq!(hunk.before, "fn refresh() {\n    old();\n}\n");
        assert_eq!(hunk.after, "fn refresh() {\n    new();\n}\n");
        assert_eq!(
            hunk.removed_range,
            Some(ReplayHunkRange { start: 2, count: 1 }),
        );
        assert_eq!(
            hunk.added_range,
            Some(ReplayHunkRange { start: 2, count: 1 }),
        );
    }

    #[test]
    fn preserves_exact_changed_line_coordinates_around_original_context() {
        let text = concat!(
            "diff --git a/src/token.rs b/src/token.rs\n",
            "--- a/src/token.rs\n",
            "+++ b/src/token.rs\n",
            "@@ -10,5 +20,5 @@ fn refresh\n",
            " before\n",
            "-old_first\n",
            "+new_first\n",
            " middle\n",
            "-old_last\n",
            "+new_last\n",
            " after\n",
        );
        let patch = parse_patch(text, ReplayLimits::default())
            .expect("parse every original changed and contextual source line");
        let hunk = &patch.files[0].hunks[0];

        assert_eq!(
            hunk.removed_range,
            Some(ReplayHunkRange {
                start: 11,
                count: 3,
            }),
        );
        assert_eq!(
            hunk.added_range,
            Some(ReplayHunkRange {
                start: 21,
                count: 3,
            }),
        );
    }

    #[test]
    fn deletion_only_hunk_retains_base_side_comment_coordinates() {
        let text = concat!(
            "diff --git a/src/token.rs b/src/token.rs\n",
            "--- a/src/token.rs\n",
            "+++ b/src/token.rs\n",
            "@@ -7,3 +7,2 @@ fn refresh\n",
            " before\n",
            "-removed\n",
            " after\n",
        );
        let patch = parse_patch(text, ReplayLimits::default())
            .expect("preserve an original deletion without inventing head-side lines");
        let hunk = &patch.files[0].hunks[0];

        assert_eq!(
            hunk.removed_range,
            Some(ReplayHunkRange { start: 8, count: 1 }),
        );
        assert_eq!(hunk.added_range, None);
    }

    #[test]
    fn classifies_new_regular_source_files() {
        let text = "diff --git a/src/new.rs b/src/new.rs\nnew file mode 100644\n--- /dev/null\n+++ b/src/new.rs\n@@ -0,0 +1,1 @@\n+fn new() {}\n";
        let patch = parse_patch(text, ReplayLimits::default()).unwrap();
        assert_eq!(patch.files[0].kind, ReplayChangeKind::AddFile);
        assert_eq!(patch.files[0].hunks[0].before, "");
        assert_eq!(patch.files[0].hunks[0].removed_range, None);
        assert_eq!(
            patch.files[0].hunks[0].added_range,
            Some(ReplayHunkRange { start: 1, count: 1 }),
        );
    }

    #[test]
    fn reports_renames_without_presenting_them_as_text_transactions() {
        let text = "diff --git a/src/old.rs b/src/new.rs\nsimilarity index 100%\nrename from src/old.rs\nrename to src/new.rs\n";
        let patch = parse_patch(text, ReplayLimits::default()).unwrap();
        assert_eq!(patch.files[0].kind, ReplayChangeKind::Rename);
        assert!(!patch.files[0].kind.supports_text_replay());
    }

    #[test]
    fn rejects_incomplete_unified_hunks() {
        let text =
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n";
        assert!(matches!(
            parse_patch(text, ReplayLimits::default()),
            Err(ReplayError::InvalidPatch(_))
        ));
    }

    #[test]
    fn rejects_repository_escape_in_diff_headers() {
        let text = "diff --git a/../escape b/../escape\n--- a/../escape\n+++ b/../escape\n";
        assert!(matches!(
            parse_patch(text, ReplayLimits::default()),
            Err(ReplayError::UnsafePath(_))
        ));
    }

    #[test]
    fn complete_patch_is_not_silently_truncated_after_twelve_thousand_lines() {
        let lines = 12_001;
        let mut patch = format!(
            "diff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,{lines} @@\n"
        );
        for index in 0..lines {
            patch.push_str(&format!("+line {index}\n"));
        }
        let parsed = parse_patch(&patch, ReplayLimits::default()).unwrap();
        assert_eq!(parsed.files[0].hunks[0].added_lines, lines);
    }

    #[test]
    fn rejects_patch_larger_than_its_explicit_limit() {
        let limits = ReplayLimits {
            max_patch_bytes: CHANGE.len() - 1,
            ..ReplayLimits::default()
        };
        assert!(matches!(
            parse_patch(CHANGE, limits),
            Err(ReplayError::LimitExceeded { .. })
        ));
    }

    #[test]
    fn retains_source_files_with_spaces() {
        let text = "diff --git a/src/my file.rs b/src/my file.rs\n--- a/src/my file.rs\n+++ b/src/my file.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let patch = parse_patch(text, ReplayLimits::default()).unwrap();
        assert_eq!(patch.files[0].path(), Some(Path::new("src/my file.rs")));
    }
}
