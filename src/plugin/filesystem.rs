//! Workspace-confined filesystem operations exposed to trusted Red plugins.
//!
//! Paths are always interpreted relative to the editor workspace. Existing parent
//! components are canonicalized before mutation to reject paths already redirected
//! outside the workspace by a symlink.

use std::{
    fs,
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
};

use anyhow::{anyhow, bail, Context};
use serde_json::{json, Value};

use crate::utils::get_workspace_path;

const MAX_BRACE_EXPANSIONS: usize = 256;

#[derive(Debug, Default)]
pub struct FileOperationOutcome {
    pub payload: Value,
    pub renames: Vec<(PathBuf, PathBuf)>,
    pub removals: Vec<PathBuf>,
}

pub fn apply_file_operation(operation: &Value) -> FileOperationOutcome {
    match apply_file_operation_inner(operation) {
        Ok(outcome) => outcome,
        Err(error) => FileOperationOutcome {
            payload: json!({
                "ok": false,
                "error": error.to_string(),
            }),
            ..FileOperationOutcome::default()
        },
    }
}

fn apply_file_operation_inner(operation: &Value) -> anyhow::Result<FileOperationOutcome> {
    let root = get_workspace_path()
        .canonicalize()
        .context("workspace root is unavailable")?;
    let kind = required_string(operation, "kind")?;
    match kind {
        "create" | "create_file" | "create_directory" => create(&root, kind, operation),
        "rename" | "move" => rename_or_move(&root, operation),
        "copy" => copy(&root, operation),
        "delete" => delete(&root, operation),
        "trash" => trash_paths(&root, operation),
        "restore" | "undo_trash" => restore_paths(&root, operation),
        "stat" => stat_path(&root, operation),
        _ => bail!("unsupported file operation `{kind}`"),
    }
}

fn create(root: &Path, kind: &str, operation: &Value) -> anyhow::Result<FileOperationOutcome> {
    let pattern = required_string(operation, "path")?;
    let expanded = expand_braces(pattern)?;
    if expanded.is_empty() {
        bail!("file name is empty");
    }

    let mut destinations = Vec::with_capacity(expanded.len());
    for path in expanded {
        let trailing_separator = path.ends_with('/') || path.ends_with('\\');
        let path = path.trim_end_matches(['/', '\\']);
        let destination = resolve_workspace_path(root, path)?;
        refuse_root(root, &destination)?;
        if destinations
            .iter()
            .any(|(existing, _)| existing == &destination)
        {
            bail!(
                "{} appears more than once in the expanded path",
                display_relative(root, &destination)
            );
        }
        if fs::symlink_metadata(&destination).is_ok() {
            bail!("{} already exists", display_relative(root, &destination));
        }
        let is_directory = kind == "create_directory" || (kind == "create" && trailing_separator);
        destinations.push((destination, is_directory));
    }

    let mut created = Vec::with_capacity(destinations.len());
    for (destination, is_directory) in destinations {
        if is_directory {
            fs::create_dir_all(&destination)
                .with_context(|| format!("could not create {}", destination.display()))?;
        } else {
            let parent = destination
                .parent()
                .ok_or_else(|| anyhow!("file destination has no parent"))?;
            fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)
                .with_context(|| format!("could not create {}", destination.display()))?;
        }
        created.push(display_relative(root, &destination));
    }

    Ok(FileOperationOutcome {
        payload: json!({
            "ok": true,
            "error": null,
            "created": created,
        }),
        ..FileOperationOutcome::default()
    })
}

fn rename_or_move(root: &Path, operation: &Value) -> anyhow::Result<FileOperationOutcome> {
    let source = resolve_workspace_path(root, required_string(operation, "source")?)?;
    let destination = resolve_workspace_path(root, required_string(operation, "destination")?)?;
    refuse_root(root, &source)?;
    refuse_root(root, &destination)?;
    require_existing(&source)?;
    require_unused(&destination)?;
    refuse_descendant_destination(&source, &destination)?;

    let parent = destination
        .parent()
        .ok_or_else(|| anyhow!("destination has no parent"))?;
    fs::create_dir_all(parent).with_context(|| format!("could not create {}", parent.display()))?;
    fs::rename(&source, &destination).with_context(|| {
        format!(
            "could not move {} to {}",
            display_relative(root, &source),
            display_relative(root, &destination)
        )
    })?;

    Ok(FileOperationOutcome {
        payload: json!({
            "ok": true,
            "error": null,
            "source": display_relative(root, &source),
            "destination": display_relative(root, &destination),
        }),
        renames: vec![(source, destination)],
        removals: Vec::new(),
    })
}

fn copy(root: &Path, operation: &Value) -> anyhow::Result<FileOperationOutcome> {
    let source = resolve_workspace_path(root, required_string(operation, "source")?)?;
    let destination = resolve_workspace_path(root, required_string(operation, "destination")?)?;
    refuse_root(root, &destination)?;
    require_existing(&source)?;
    require_unused(&destination)?;
    refuse_descendant_destination(&source, &destination)?;
    if let Err(error) = copy_path(&source, &destination) {
        if fs::symlink_metadata(&destination).is_ok() {
            let _ = remove_path(&destination);
        }
        return Err(error).with_context(|| {
            format!(
                "could not copy {} to {}",
                display_relative(root, &source),
                display_relative(root, &destination)
            )
        });
    }

    Ok(FileOperationOutcome {
        payload: json!({
            "ok": true,
            "error": null,
            "source": display_relative(root, &source),
            "destination": display_relative(root, &destination),
        }),
        ..FileOperationOutcome::default()
    })
}

fn delete(root: &Path, operation: &Value) -> anyhow::Result<FileOperationOutcome> {
    let paths = resolve_targets(root, operation)?;
    for path in &paths {
        refuse_root(root, path)?;
        require_existing(path)?;
    }
    for path in &paths {
        remove_path(path)?;
    }

    Ok(FileOperationOutcome {
        payload: json!({
            "ok": true,
            "error": null,
            "removed": relative_paths(root, &paths),
        }),
        removals: paths,
        renames: Vec::new(),
    })
}

fn trash_paths(root: &Path, operation: &Value) -> anyhow::Result<FileOperationOutcome> {
    let paths = resolve_targets(root, operation)?;
    for path in &paths {
        refuse_root(root, path)?;
        require_existing(path)?;
    }
    trash::delete_all(&paths).map_err(|error| anyhow!("could not move item to trash: {error}"))?;

    Ok(FileOperationOutcome {
        payload: json!({
            "ok": true,
            "error": null,
            "removed": relative_paths(root, &paths),
            "undo_supported": cfg!(any(
                windows,
                all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))
            )),
        }),
        removals: paths,
        renames: Vec::new(),
    })
}

#[cfg(any(
    windows,
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
))]
fn restore_paths(root: &Path, operation: &Value) -> anyhow::Result<FileOperationOutcome> {
    let wanted = resolve_targets(root, operation)?;
    let wanted = wanted.into_iter().collect::<std::collections::HashSet<_>>();
    let mut candidates = trash::os_limited::list()
        .map_err(|error| anyhow!("could not inspect trash: {error}"))?
        .into_iter()
        .filter(|item| wanted.contains(&item.original_path()))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|item| item.time_deleted);

    let mut selected = Vec::new();
    for path in &wanted {
        let item = candidates
            .iter()
            .rev()
            .find(|item| item.original_path() == *path)
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "no matching trash item for {}",
                    display_relative(root, path)
                )
            })?;
        selected.push(item);
    }
    trash::os_limited::restore_all(selected)
        .map_err(|error| anyhow!("could not restore trash item: {error}"))?;

    Ok(FileOperationOutcome {
        payload: json!({
            "ok": true,
            "error": null,
            "restored": relative_paths(root, &wanted.into_iter().collect::<Vec<_>>()),
        }),
        ..FileOperationOutcome::default()
    })
}

#[cfg(not(any(
    windows,
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
)))]
fn restore_paths(_root: &Path, _operation: &Value) -> anyhow::Result<FileOperationOutcome> {
    bail!("restoring items from the system trash is not supported on this platform")
}

fn stat_path(root: &Path, operation: &Value) -> anyhow::Result<FileOperationOutcome> {
    let path = resolve_workspace_path(root, required_string(operation, "path")?)?;
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("could not inspect {}", display_relative(root, &path)))?;
    let kind = if metadata.file_type().is_symlink() {
        "symlink"
    } else if metadata.is_dir() {
        "directory"
    } else {
        "file"
    };
    let modified_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis());
    let created_ms = metadata
        .created()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis());

    Ok(FileOperationOutcome {
        payload: json!({
            "ok": true,
            "error": null,
            "path": display_relative(root, &path),
            "kind": kind,
            "size": metadata.len(),
            "readonly": metadata.permissions().readonly(),
            "modified_ms": modified_ms,
            "created_ms": created_ms,
        }),
        ..FileOperationOutcome::default()
    })
}

fn required_string<'a>(value: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("file operation requires `{key}`"))
}

fn resolve_targets(root: &Path, operation: &Value) -> anyhow::Result<Vec<PathBuf>> {
    let raw = operation
        .get("paths")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("file operation requires `paths`"))?;
    if raw.is_empty() {
        bail!("file operation requires at least one path");
    }
    let mut paths = raw
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| anyhow!("file operation paths must be strings"))
                .and_then(|path| resolve_workspace_path(root, path))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    paths.sort();
    paths.dedup();
    let selected = paths.clone();
    paths.retain(|path| {
        !selected
            .iter()
            .any(|other| other != path && path.starts_with(other))
    });
    Ok(paths)
}

fn resolve_workspace_path(root: &Path, raw: &str) -> anyhow::Result<PathBuf> {
    let raw_path = Path::new(raw);
    if raw_path.is_absolute() {
        bail!("absolute paths are not allowed");
    }
    let mut relative = PathBuf::new();
    for component in raw_path.components() {
        match component {
            Component::Normal(component) => relative.push(component),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("path escapes the workspace")
            }
        }
    }
    if relative.as_os_str().is_empty() {
        return Ok(root.to_path_buf());
    }

    let candidate = root.join(&relative);
    let parent = candidate.parent().unwrap_or(root);
    let mut existing = parent;
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or_else(|| anyhow!("path has no existing workspace ancestor"))?;
    }
    let canonical_parent = existing
        .canonicalize()
        .with_context(|| format!("could not resolve {}", existing.display()))?;
    if !canonical_parent.starts_with(root) {
        bail!("path resolves outside the workspace");
    }
    Ok(candidate)
}

fn refuse_root(root: &Path, path: &Path) -> anyhow::Result<()> {
    if path == root {
        bail!("the workspace root cannot be modified");
    }
    Ok(())
}

fn require_existing(path: &Path) -> anyhow::Result<()> {
    fs::symlink_metadata(path)
        .map(|_| ())
        .with_context(|| format!("{} does not exist", path.display()))
}

fn require_unused(path: &Path) -> anyhow::Result<()> {
    if fs::symlink_metadata(path).is_ok() {
        bail!("{} already exists", path.display());
    }
    Ok(())
}

fn refuse_descendant_destination(source: &Path, destination: &Path) -> anyhow::Result<()> {
    if destination == source || destination.starts_with(source) {
        bail!("cannot copy or move an item into itself");
    }
    Ok(())
}

fn remove_path(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn copy_path(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(source)?;
        create_symlink(&target, destination, source.is_dir())?;
        return Ok(());
    }
    if metadata.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, destination)?;
        fs::set_permissions(destination, metadata.permissions())?;
        return Ok(());
    }

    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        copy_path(&entry.path(), &destination.join(entry.file_name()))?;
    }
    fs::set_permissions(destination, metadata.permissions())?;
    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: &Path, destination: &Path, _directory: bool) -> anyhow::Result<()> {
    std::os::unix::fs::symlink(target, destination)?;
    Ok(())
}

#[cfg(windows)]
fn create_symlink(target: &Path, destination: &Path, directory: bool) -> anyhow::Result<()> {
    if directory {
        std::os::windows::fs::symlink_dir(target, destination)?;
    } else {
        std::os::windows::fs::symlink_file(target, destination)?;
    }
    Ok(())
}

fn display_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn relative_paths(root: &Path, paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| display_relative(root, path))
        .collect()
}

fn expand_braces(pattern: &str) -> anyhow::Result<Vec<String>> {
    let mut expanded = vec![pattern.to_string()];
    loop {
        let mut changed = false;
        let mut next = Vec::new();
        for value in expanded {
            let Some((start, end)) = first_brace_pair(&value)? else {
                next.push(value);
                continue;
            };
            changed = true;
            let choices = brace_choices(&value[start + 1..end])?;
            for choice in choices {
                let mut item = String::new();
                item.push_str(&value[..start]);
                item.push_str(&choice);
                item.push_str(&value[end + 1..]);
                next.push(item);
                if next.len() > MAX_BRACE_EXPANSIONS {
                    bail!("brace expansion exceeds {MAX_BRACE_EXPANSIONS} paths");
                }
            }
        }
        if !changed {
            return Ok(next);
        }
        expanded = next;
    }
}

fn first_brace_pair(value: &str) -> anyhow::Result<Option<(usize, usize)>> {
    let mut start = None;
    let mut depth = 0usize;
    for (index, character) in value.char_indices() {
        match character {
            '{' => {
                if start.is_none() {
                    start = Some(index);
                }
                depth += 1;
            }
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    return Ok(start.map(|start| (start, index)));
                }
            }
            '}' => bail!("brace expansion has an unmatched closing brace"),
            _ => {}
        }
    }
    if depth != 0 {
        bail!("brace expansion has an unmatched opening brace");
    }
    Ok(None)
}

fn brace_choices(contents: &str) -> anyhow::Result<Vec<String>> {
    let parts = split_top_level(contents, ',');
    if parts.len() > 1 {
        return Ok(parts);
    }
    let range = split_top_level(contents, '.')
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if range.len() == 2 || range.len() == 3 {
        if let (Ok(start), Ok(end)) = (range[0].parse::<i64>(), range[1].parse::<i64>()) {
            let step = range
                .get(2)
                .map(|value| value.parse::<i64>())
                .transpose()?
                .unwrap_or(if start <= end { 1 } else { -1 });
            if step == 0 {
                bail!("brace expansion step cannot be zero");
            }
            let width = range[0].len().max(range[1].len());
            let mut values = Vec::new();
            let mut current = start;
            while (step > 0 && current <= end) || (step < 0 && current >= end) {
                values.push(format!("{current:0width$}"));
                current += step;
                if values.len() > MAX_BRACE_EXPANSIONS {
                    bail!("brace expansion exceeds {MAX_BRACE_EXPANSIONS} paths");
                }
            }
            return Ok(values);
        }
        let start = range[0].chars().collect::<Vec<_>>();
        let end = range[1].chars().collect::<Vec<_>>();
        if start.len() == 1 && end.len() == 1 {
            let start = start[0] as i64;
            let end = end[0] as i64;
            let step = range
                .get(2)
                .map(|value| value.parse::<i64>())
                .transpose()?
                .unwrap_or(if start <= end { 1 } else { -1 });
            if step == 0 {
                bail!("brace expansion step cannot be zero");
            }
            let mut values = Vec::new();
            let mut current = start;
            while (step > 0 && current <= end) || (step < 0 && current >= end) {
                let character = char::from_u32(current as u32)
                    .ok_or_else(|| anyhow!("invalid character range"))?;
                values.push(character.to_string());
                current += step;
            }
            return Ok(values);
        }
    }
    Ok(vec![contents.to_string()])
}

fn split_top_level(value: &str, separator: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    for (index, character) in value.char_indices() {
        match character {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            character if character == separator && depth == 0 => {
                parts.push(value[start..index].to_string());
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(value[start..].to_string());
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_lists_ranges_steps_and_nested_braces() {
        assert_eq!(expand_braces("file{,.bak}").unwrap(), ["file", "file.bak"]);
        assert_eq!(
            expand_braces("{a,b}/{00..02}.rs").unwrap(),
            ["a/00.rs", "a/01.rs", "a/02.rs", "b/00.rs", "b/01.rs", "b/02.rs"]
        );
        assert_eq!(expand_braces("x{a..e..2}").unwrap(), ["xa", "xc", "xe"]);
    }

    #[test]
    fn workspace_paths_reject_absolute_and_parent_components() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        assert!(resolve_workspace_path(&root, "../outside").is_err());
        assert!(resolve_workspace_path(&root, "/outside").is_err());
        assert_eq!(
            resolve_workspace_path(&root, "./src/main.rs").unwrap(),
            root.join("src/main.rs")
        );
    }

    #[test]
    fn recursive_copy_preserves_files_and_rejects_existing_destinations() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("nested/file.txt"), "contents").unwrap();

        copy_path(&source, &destination).unwrap();
        assert_eq!(
            fs::read_to_string(destination.join("nested/file.txt")).unwrap(),
            "contents"
        );
    }

    #[test]
    fn create_move_copy_stat_and_delete_stay_within_the_workspace() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();

        let created = create(&root, "create", &json!({ "path": "src/{one,two}.rs" })).unwrap();
        assert_eq!(
            created.payload["created"],
            json!(["src/one.rs", "src/two.rs"])
        );
        create(
            &root,
            "create_directory",
            &json!({ "path": "assets/icons" }),
        )
        .unwrap();
        fs::write(root.join("src/one.rs"), "fn one() {}").unwrap();

        copy(
            &root,
            &json!({
                "source": "src",
                "destination": "src-copy",
            }),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("src-copy/one.rs")).unwrap(),
            "fn one() {}"
        );

        let moved = rename_or_move(
            &root,
            &json!({
                "source": "src/two.rs",
                "destination": "src/renamed.rs",
            }),
        )
        .unwrap();
        assert_eq!(
            moved.payload["destination"],
            serde_json::Value::String("src/renamed.rs".to_string())
        );
        assert_eq!(moved.renames.len(), 1);

        let stat = stat_path(&root, &json!({ "path": "src/one.rs" })).unwrap();
        assert_eq!(stat.payload["kind"], "file");
        assert_eq!(stat.payload["size"], 11);

        let removed = delete(
            &root,
            &json!({
                "paths": ["src-copy/one.rs", "src-copy"],
            }),
        )
        .unwrap();
        assert_eq!(removed.payload["removed"], json!(["src-copy"]));
        assert!(!root.join("src-copy").exists());
        assert!(root.join("src/one.rs").exists());
    }

    #[cfg(unix)]
    #[test]
    fn workspace_paths_reject_children_of_symlinks_that_escape_the_root() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("outside-link")).unwrap();

        assert!(resolve_workspace_path(&root, "outside-link/escaped.txt").is_err());
        assert_eq!(
            resolve_workspace_path(&root, "outside-link").unwrap(),
            root.join("outside-link")
        );
    }
}
