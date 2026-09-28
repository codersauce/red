//! Directory rows shared by path buffers and the NeoTree plugin listing.
use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug)]
pub(crate) struct Directory {
    pub path: String,
    pub entries: Vec<DirectoryEntry>,
}

impl Directory {
    /// Leave space for both navigation rows even in a very short split.
    pub(crate) fn header_height(window_height: usize) -> usize {
        4.min(window_height.saturating_sub(2))
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct DirectoryEntry {
    pub name: String,
    pub path: PathBuf,
    pub kind: &'static str,
    #[serde(skip)]
    sort_name: String,
}

/// NeoTree filters workspace ignores; a directory buffer shows the actual contents.
pub(crate) fn scan(
    path: &Path,
    scan_path: &Path,
    filtered: bool,
) -> (Vec<DirectoryEntry>, Option<String>) {
    match std::fs::metadata(scan_path) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return (Vec::new(), Some("path is not a directory".into())),
        Err(error) => return (Vec::new(), Some(error.to_string())),
    }

    let mut builder = ignore::WalkBuilder::new(scan_path);
    builder
        .max_depth(Some(1))
        .hidden(false)
        .ignore(filtered)
        .git_ignore(filtered)
        .git_global(filtered)
        .git_exclude(filtered)
        .follow_links(false)
        .filter_entry(move |entry| {
            !filtered
                || entry.depth() == 0
                || !matches!(entry.file_name().to_str(), Some(".git" | ".bare"))
        });

    let mut entries = Vec::new();
    let mut scan_error = None;
    for result in builder.build() {
        let entry = match result {
            Ok(entry) if entry.depth() > 0 => entry,
            Ok(_) => continue,
            Err(error) => {
                scan_error.get_or_insert_with(|| error.to_string());
                continue;
            }
        };
        let kind = match entry.file_type() {
            Some(file_type) if file_type.is_dir() => "directory",
            Some(file_type) if file_type.is_file() => "file",
            Some(file_type) if file_type.is_symlink() => {
                // Present links as their targets without replacing the lexical path.
                // Unresolved links remain visible as files; only directory expansion
                // follows a link, so a listing never recursively walks a cycle.
                match std::fs::metadata(entry.path()) {
                    Ok(metadata) if metadata.is_dir() => "directory",
                    Ok(metadata) if metadata.is_file() => "file",
                    Ok(_) => "other",
                    Err(_) => "file",
                }
            }
            _ => "other",
        };
        if kind == "other" {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        entries.push(DirectoryEntry {
            sort_name: name.to_lowercase(),
            name,
            path: path.join(entry.path().strip_prefix(scan_path).unwrap_or(entry.path())),
            kind,
        });
    }

    entries.sort_by(|a, b| {
        (a.kind != "directory")
            .cmp(&(b.kind != "directory"))
            .then_with(|| a.sort_name.cmp(&b.sort_name))
    });

    (entries, scan_error)
}
