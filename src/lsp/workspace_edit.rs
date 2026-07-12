use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

use path_absolutize::Absolutize;

#[cfg(unix)]
use {
    nix::{
        errno::Errno,
        fcntl::{open, openat, renameat, AtFlags, OFlag},
        sys::stat::{fstatat, Mode, SFlag},
        unistd::{unlinkat, UnlinkatFlags},
    },
    std::os::fd::{AsRawFd, FromRawFd},
};

use super::{apply_text_edits, file_path, file_uri, LspError, WorkspaceEditOperation};

const MAX_WORKSPACE_EDIT_OPERATIONS: usize = 1024;
const MAX_WORKSPACE_FILE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_WORKSPACE_EDIT_TOTAL_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct OpenWorkspaceDocument {
    pub index: usize,
    pub uri: String,
    pub contents: String,
    pub revision: u64,
    pub version: Option<i64>,
    pub dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedWorkspaceDocument {
    pub index: Option<usize>,
    pub original_uri: Option<String>,
    pub uri: String,
    pub original_contents: String,
    pub contents: String,
    pub text_changed: bool,
}

#[derive(Debug)]
pub struct PreparedWorkspaceEdit {
    pub documents: Vec<PreparedWorkspaceDocument>,
    pub resource_operations: Vec<WorkspaceEditOperation>,
    snapshots: Vec<FileSnapshot>,
    root: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct VirtualDocument {
    index: Option<usize>,
    original_uri: Option<String>,
    uri: String,
    original_contents: String,
    contents: String,
    revision: Option<u64>,
    version: Option<i64>,
    dirty: bool,
    exists: bool,
    text_changed: bool,
    resource_changed: bool,
}

#[derive(Debug, Clone)]
struct FileSnapshot {
    path: PathBuf,
    contents: Option<Vec<u8>>,
    permissions: Option<fs::Permissions>,
}

pub fn prepare_workspace_edit(
    operations: &[WorkspaceEditOperation],
    expected_revisions: &[(String, u64)],
    open_documents: Vec<OpenWorkspaceDocument>,
    workspace_root: Option<&Path>,
) -> Result<PreparedWorkspaceEdit, LspError> {
    if operations.len() > MAX_WORKSPACE_EDIT_OPERATIONS {
        return Err(protocol_error(format!(
            "LSP workspace edit exceeds {MAX_WORKSPACE_EDIT_OPERATIONS} operations"
        )));
    }

    let root = workspace_root.map(canonical_workspace_root).transpose()?;
    let mut documents = HashMap::new();
    for document in open_documents {
        let path = normalized_path(&document.uri)?;
        let uri = file_uri(&path)?;
        if documents
            .insert(
                path.clone(),
                VirtualDocument {
                    index: Some(document.index),
                    original_uri: Some(uri.clone()),
                    uri,
                    original_contents: document.contents.clone(),
                    contents: document.contents,
                    revision: Some(document.revision),
                    version: document.version,
                    dirty: document.dirty,
                    exists: true,
                    text_changed: false,
                    resource_changed: false,
                },
            )
            .is_some()
        {
            return Err(protocol_error(format!(
                "LSP workspace edit has duplicate open buffers for {}",
                path.display()
            )));
        }
    }
    ensure_total_budget(&documents, &HashMap::new())?;

    let mut snapshots = HashMap::new();
    let mut resource_operations = Vec::new();
    for operation in operations {
        match operation {
            WorkspaceEditOperation::Document { edit } => {
                let path = normalized_path(&edit.uri)?;
                if let Some(root) = root.as_deref() {
                    ensure_safe_path(root, &path)?;
                }
                if !documents.contains_key(&path) {
                    let root = require_workspace_root(root.as_deref(), &path)?;
                    ensure_safe_path(root, &path)?;
                    let snapshot = snapshot_file(root, &path)?;
                    let bytes = snapshot.contents.as_deref().ok_or_else(|| {
                        protocol_error(format!(
                            "LSP workspace edit targets missing file {}",
                            path.display()
                        ))
                    })?;
                    let contents = std::str::from_utf8(bytes).map_err(|_| {
                        protocol_error(format!(
                            "LSP workspace edit targets non-UTF-8 file {}",
                            path.display()
                        ))
                    })?;
                    documents.insert(
                        path.clone(),
                        VirtualDocument {
                            index: None,
                            original_uri: None,
                            uri: file_uri(&path)?,
                            original_contents: contents.to_string(),
                            contents: contents.to_string(),
                            revision: None,
                            version: None,
                            dirty: false,
                            exists: true,
                            text_changed: false,
                            resource_changed: false,
                        },
                    );
                    snapshots.entry(path.clone()).or_insert(snapshot);
                }

                let document = documents.get_mut(&path).expect("document was inserted");
                if !document.exists {
                    return Err(protocol_error(format!(
                        "LSP workspace edit targets deleted file {}",
                        path.display()
                    )));
                }
                if let Some(expected) = expected_revisions.iter().find_map(|(uri, revision)| {
                    (normalized_path(uri).ok().as_deref() == Some(path.as_path()))
                        .then_some(*revision)
                }) {
                    if document.revision != Some(expected) {
                        return Err(protocol_error(format!(
                            "LSP workspace edit is stale for {}; buffer changed",
                            path.display()
                        )));
                    }
                } else if document.index.is_some() && !expected_revisions.is_empty() {
                    return Err(protocol_error(format!(
                        "LSP workspace edit is missing a revision for open file {}",
                        path.display()
                    )));
                }
                if let Some(version) = edit.version {
                    if document.version != Some(version) {
                        return Err(protocol_error(format!(
                            "LSP workspace edit version is stale for {}",
                            path.display()
                        )));
                    }
                }

                let updated = apply_text_edits(&document.contents, &edit.edits)?;
                document.text_changed |= updated != document.contents;
                document.contents = updated;
            }
            WorkspaceEditOperation::Create {
                uri,
                overwrite,
                ignore_if_exists,
            } => {
                let path = normalized_path(uri)?;
                let root = require_workspace_root(root.as_deref(), &path)?;
                ensure_safe_path(root, &path)?;
                ensure_parent_directory(&path)?;
                let snapshot = snapshots
                    .entry(path.clone())
                    .or_insert(snapshot_file(root, &path)?);
                let exists = documents
                    .get(&path)
                    .map_or(snapshot.contents.is_some(), |document| document.exists);
                if exists && !*overwrite && *ignore_if_exists {
                    continue;
                }
                if exists && !overwrite {
                    return Err(protocol_error(format!(
                        "LSP create target already exists: {}",
                        path.display()
                    )));
                }
                if documents
                    .get(&path)
                    .is_some_and(|document| document.index.is_some())
                {
                    return Err(protocol_error(format!(
                        "LSP create would overwrite an open buffer: {}",
                        path.display()
                    )));
                }
                documents.insert(
                    path.clone(),
                    VirtualDocument {
                        index: None,
                        original_uri: None,
                        uri: file_uri(&path)?,
                        original_contents: String::new(),
                        contents: String::new(),
                        revision: None,
                        version: None,
                        dirty: false,
                        exists: true,
                        text_changed: false,
                        resource_changed: true,
                    },
                );
                resource_operations.push(operation.clone());
            }
            WorkspaceEditOperation::Rename {
                old_uri,
                new_uri,
                overwrite,
                ignore_if_exists,
            } => {
                let old_path = normalized_path(old_uri)?;
                let new_path = normalized_path(new_uri)?;
                let root = require_workspace_root(root.as_deref(), &old_path)?;
                ensure_safe_path(root, &old_path)?;
                ensure_safe_path(root, &new_path)?;
                ensure_parent_directory(&new_path)?;
                snapshots
                    .entry(old_path.clone())
                    .or_insert(snapshot_file(root, &old_path)?);
                if snapshots
                    .get(&old_path)
                    .is_some_and(|snapshot| snapshot.contents.is_none())
                    && !documents.get(&old_path).is_some_and(|document| {
                        document.index.is_none() && document.resource_changed && document.exists
                    })
                {
                    return Err(protocol_error(format!(
                        "LSP rename source does not exist on disk: {}",
                        old_path.display()
                    )));
                }
                let destination = snapshots
                    .entry(new_path.clone())
                    .or_insert(snapshot_file(root, &new_path)?);
                let destination_exists = documents
                    .get(&new_path)
                    .map_or(destination.contents.is_some(), |document| document.exists);
                if destination_exists && !*overwrite && *ignore_if_exists {
                    continue;
                }
                if destination_exists && !overwrite {
                    return Err(protocol_error(format!(
                        "LSP rename target already exists: {}",
                        new_path.display()
                    )));
                }
                if documents
                    .get(&new_path)
                    .is_some_and(|document| document.index.is_some())
                {
                    return Err(protocol_error(format!(
                        "LSP rename would overwrite an open buffer: {}",
                        new_path.display()
                    )));
                }

                let mut document = if let Some(document) = documents.remove(&old_path) {
                    document
                } else {
                    let source = snapshots.get(&old_path).expect("source was snapshotted");
                    let bytes = source.contents.as_deref().ok_or_else(|| {
                        protocol_error(format!(
                            "LSP rename source does not exist: {}",
                            old_path.display()
                        ))
                    })?;
                    VirtualDocument {
                        index: None,
                        original_uri: None,
                        uri: file_uri(&old_path)?,
                        original_contents: String::from_utf8_lossy(bytes).into_owned(),
                        contents: String::from_utf8_lossy(bytes).into_owned(),
                        revision: None,
                        version: None,
                        dirty: false,
                        exists: true,
                        text_changed: false,
                        resource_changed: true,
                    }
                };
                if !document.exists {
                    return Err(protocol_error(format!(
                        "LSP rename source does not exist: {}",
                        old_path.display()
                    )));
                }
                document.uri = file_uri(&new_path)?;
                document.resource_changed = true;
                documents.insert(new_path, document);
                resource_operations.push(operation.clone());
            }
            WorkspaceEditOperation::Delete {
                uri,
                recursive,
                ignore_if_not_exists,
            } => {
                let path = normalized_path(uri)?;
                let root = require_workspace_root(root.as_deref(), &path)?;
                ensure_safe_path(root, &path)?;
                if *recursive {
                    return Err(protocol_error(format!(
                        "LSP recursive delete is not supported: {}",
                        path.display()
                    )));
                }
                let snapshot = snapshots
                    .entry(path.clone())
                    .or_insert(snapshot_file(root, &path)?);
                let exists = documents
                    .get(&path)
                    .map_or(snapshot.contents.is_some(), |document| document.exists);
                if !exists && *ignore_if_not_exists {
                    continue;
                }
                if !exists {
                    return Err(protocol_error(format!(
                        "LSP delete target does not exist: {}",
                        path.display()
                    )));
                }
                if let Some(document) = documents
                    .get(&path)
                    .filter(|document| document.index.is_some())
                {
                    return Err(protocol_error(format!(
                        "LSP delete would remove an {}open buffer: {}",
                        if document.dirty { "unsaved " } else { "" },
                        path.display()
                    )));
                }
                documents.remove(&path);
                resource_operations.push(operation.clone());
            }
        }
        ensure_total_budget(&documents, &snapshots)?;
    }

    let mut prepared_documents = documents
        .into_values()
        .filter(|document| {
            document.exists
                && (document.text_changed
                    || (document.index.is_some() && document.resource_changed))
        })
        .map(|document| PreparedWorkspaceDocument {
            index: document.index,
            original_uri: document.original_uri,
            uri: document.uri,
            original_contents: document.original_contents,
            contents: document.contents,
            text_changed: document.text_changed,
        })
        .collect::<Vec<_>>();
    prepared_documents.sort_by(|left, right| left.uri.cmp(&right.uri));

    Ok(PreparedWorkspaceEdit {
        documents: prepared_documents,
        resource_operations,
        snapshots: snapshots.into_values().collect(),
        root,
    })
}

pub fn apply_workspace_resource_operations(edit: &PreparedWorkspaceEdit) -> Result<(), LspError> {
    apply_workspace_resource_operations_with_hook(edit, |_| Ok(()))
}

fn apply_workspace_resource_operations_with_hook(
    edit: &PreparedWorkspaceEdit,
    mut before_operation: impl FnMut(usize) -> Result<(), LspError>,
) -> Result<(), LspError> {
    if edit.resource_operations.is_empty() {
        return Ok(());
    }
    let root = edit.root.as_deref().ok_or_else(|| {
        protocol_error("LSP resource operation is missing a workspace root".to_string())
    })?;
    verify_snapshots(root, &edit.snapshots)?;
    let mut expected = edit.snapshots.clone();

    let result = edit.resource_operations.iter().enumerate().try_for_each(
        |(index, operation)| -> Result<(), LspError> {
            before_operation(index)?;
            verify_operation_snapshots(root, &expected, operation)?;
            apply_resource_operation(root, operation)?;
            refresh_operation_snapshots(root, &mut expected, operation)
        },
    );
    if let Err(error) = result {
        if let Err(race) = verify_snapshots(root, &expected) {
            return Err(protocol_error(format!(
                "LSP resource operation failed ({error}) and rollback was refused because a target changed concurrently ({race})"
            )));
        }
        if let Err(rollback_error) = restore_snapshots(root, &edit.snapshots) {
            return Err(protocol_error(format!(
                "LSP resource operation failed ({error}) and rollback failed ({rollback_error})"
            )));
        }
        return Err(error);
    }
    Ok(())
}

fn apply_resource_operation(
    root: &Path,
    operation: &WorkspaceEditOperation,
) -> Result<(), LspError> {
    match operation {
        WorkspaceEditOperation::Create { uri, overwrite, .. } => {
            let path = normalized_path(uri)?;
            let snapshot = secure_snapshot_file(root, &path)?;
            if *overwrite && snapshot.contents.is_some() {
                atomic_write_with_permissions(root, &path, b"", snapshot.permissions.as_ref())?;
            } else {
                secure_create(root, &path)?;
            }
        }
        WorkspaceEditOperation::Rename {
            old_uri,
            new_uri,
            overwrite,
            ..
        } => {
            let old_path = normalized_path(old_uri)?;
            let new_path = normalized_path(new_uri)?;
            secure_rename(root, &old_path, &new_path, *overwrite)?;
        }
        WorkspaceEditOperation::Delete { uri, .. } => {
            let path = normalized_path(uri)?;
            secure_remove(root, &path)?;
        }
        WorkspaceEditOperation::Document { .. } => {}
    }
    Ok(())
}

fn verify_snapshots(root: &Path, snapshots: &[FileSnapshot]) -> Result<(), LspError> {
    for snapshot in snapshots {
        let current = secure_snapshot_file(root, &snapshot.path)?;
        if current.contents != snapshot.contents {
            return Err(protocol_error(format!(
                "LSP resource target changed during preparation: {}",
                snapshot.path.display()
            )));
        }
    }
    Ok(())
}

fn operation_paths(operation: &WorkspaceEditOperation) -> Result<Vec<PathBuf>, LspError> {
    match operation {
        WorkspaceEditOperation::Create { uri, .. } | WorkspaceEditOperation::Delete { uri, .. } => {
            Ok(vec![normalized_path(uri)?])
        }
        WorkspaceEditOperation::Rename {
            old_uri, new_uri, ..
        } => Ok(vec![normalized_path(old_uri)?, normalized_path(new_uri)?]),
        WorkspaceEditOperation::Document { .. } => Ok(Vec::new()),
    }
}

fn verify_operation_snapshots(
    root: &Path,
    snapshots: &[FileSnapshot],
    operation: &WorkspaceEditOperation,
) -> Result<(), LspError> {
    let paths = operation_paths(operation)?;
    let relevant = snapshots
        .iter()
        .filter(|snapshot| paths.contains(&snapshot.path))
        .cloned()
        .collect::<Vec<_>>();
    verify_snapshots(root, &relevant)
}

fn refresh_operation_snapshots(
    root: &Path,
    snapshots: &mut Vec<FileSnapshot>,
    operation: &WorkspaceEditOperation,
) -> Result<(), LspError> {
    for path in operation_paths(operation)? {
        let current = secure_snapshot_file(root, &path)?;
        if let Some(snapshot) = snapshots.iter_mut().find(|snapshot| snapshot.path == path) {
            *snapshot = current;
        } else {
            snapshots.push(current);
        }
    }
    Ok(())
}

fn restore_snapshots(root: &Path, snapshots: &[FileSnapshot]) -> Result<(), LspError> {
    for snapshot in snapshots {
        match &snapshot.contents {
            Some(contents) => {
                atomic_write_with_permissions(
                    root,
                    &snapshot.path,
                    contents,
                    snapshot.permissions.as_ref(),
                )?;
            }
            None if secure_snapshot_file(root, &snapshot.path)?
                .contents
                .is_some() =>
            {
                secure_remove(root, &snapshot.path)?;
            }
            None => {}
        }
    }
    Ok(())
}

fn snapshot_file(root: &Path, path: &Path) -> Result<FileSnapshot, LspError> {
    secure_snapshot_file(root, path)
}

#[cfg(unix)]
fn secure_parent(root: &Path, path: &Path) -> Result<(fs::File, std::ffi::OsString), LspError> {
    let relative = path.strip_prefix(root).map_err(|_| {
        protocol_error(format!(
            "LSP workspace path is outside {}: {}",
            root.display(),
            path.display()
        ))
    })?;
    let leaf = relative.file_name().ok_or_else(|| {
        protocol_error(format!(
            "LSP workspace path has no filename: {}",
            path.display()
        ))
    })?;
    let raw = open(
        root,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?;
    // SAFETY: `open` returned a new, owned descriptor.
    let mut parent = unsafe { fs::File::from_raw_fd(raw) };
    if let Some(components) = relative.parent() {
        for component in components.components() {
            let Component::Normal(component) = component else {
                return Err(protocol_error(format!(
                    "LSP workspace path contains an invalid component: {}",
                    path.display()
                )));
            };
            let raw = openat(
                Some(parent.as_raw_fd()),
                component,
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )?;
            // SAFETY: `openat` returned a new, owned descriptor.
            parent = unsafe { fs::File::from_raw_fd(raw) };
        }
    }
    Ok((parent, leaf.to_os_string()))
}

#[cfg(unix)]
fn secure_snapshot_file(root: &Path, path: &Path) -> Result<FileSnapshot, LspError> {
    let (parent, leaf) = secure_parent(root, path)?;
    let raw = match openat(
        Some(parent.as_raw_fd()),
        leaf.as_os_str(),
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    ) {
        Ok(raw) => raw,
        Err(Errno::ENOENT) => {
            return Ok(FileSnapshot {
                path: path.to_path_buf(),
                contents: None,
                permissions: None,
            });
        }
        Err(error) => return Err(error.into()),
    };
    // SAFETY: `openat` returned a new, owned descriptor.
    let file = unsafe { fs::File::from_raw_fd(raw) };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(protocol_error(format!(
            "LSP workspace path is not a regular file: {}",
            path.display()
        )));
    }
    if metadata.len() > MAX_WORKSPACE_FILE_BYTES {
        return Err(protocol_error(format!(
            "LSP workspace file exceeds {MAX_WORKSPACE_FILE_BYTES} bytes: {}",
            path.display()
        )));
    }
    let mut contents = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_WORKSPACE_FILE_BYTES + 1)
        .read_to_end(&mut contents)?;
    if contents.len() as u64 > MAX_WORKSPACE_FILE_BYTES {
        return Err(protocol_error(format!(
            "LSP workspace file exceeds {MAX_WORKSPACE_FILE_BYTES} bytes: {}",
            path.display()
        )));
    }
    Ok(FileSnapshot {
        path: path.to_path_buf(),
        contents: Some(contents),
        permissions: Some(metadata.permissions()),
    })
}

#[cfg(unix)]
fn secure_create(root: &Path, path: &Path) -> Result<(), LspError> {
    let (parent, leaf) = secure_parent(root, path)?;
    let raw = openat(
        Some(parent.as_raw_fd()),
        leaf.as_os_str(),
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::from_bits_truncate(0o644),
    )?;
    // SAFETY: `openat` returned a new, owned descriptor.
    drop(unsafe { fs::File::from_raw_fd(raw) });
    Ok(())
}

#[cfg(unix)]
fn secure_remove(root: &Path, path: &Path) -> Result<(), LspError> {
    let (parent, leaf) = secure_parent(root, path)?;
    let stat = fstatat(
        Some(parent.as_raw_fd()),
        leaf.as_os_str(),
        AtFlags::AT_SYMLINK_NOFOLLOW,
    )?;
    if SFlag::from_bits_truncate(stat.st_mode).contains(SFlag::S_IFLNK)
        || !SFlag::from_bits_truncate(stat.st_mode).contains(SFlag::S_IFREG)
    {
        return Err(protocol_error(format!(
            "LSP workspace path is not a regular file: {}",
            path.display()
        )));
    }
    unlinkat(
        Some(parent.as_raw_fd()),
        leaf.as_os_str(),
        UnlinkatFlags::NoRemoveDir,
    )?;
    Ok(())
}

#[cfg(unix)]
fn secure_rename(root: &Path, old: &Path, new: &Path, overwrite: bool) -> Result<(), LspError> {
    let (old_parent, old_leaf) = secure_parent(root, old)?;
    let (new_parent, new_leaf) = secure_parent(root, new)?;
    let source = fstatat(
        Some(old_parent.as_raw_fd()),
        old_leaf.as_os_str(),
        AtFlags::AT_SYMLINK_NOFOLLOW,
    )?;
    if !SFlag::from_bits_truncate(source.st_mode).contains(SFlag::S_IFREG) {
        return Err(protocol_error(format!(
            "LSP workspace path is not a regular file: {}",
            old.display()
        )));
    }
    match fstatat(
        Some(new_parent.as_raw_fd()),
        new_leaf.as_os_str(),
        AtFlags::AT_SYMLINK_NOFOLLOW,
    ) {
        Ok(_destination) if !overwrite => {
            return Err(protocol_error(format!(
                "LSP rename target already exists: {}",
                new.display()
            )));
        }
        Ok(destination)
            if !SFlag::from_bits_truncate(destination.st_mode).contains(SFlag::S_IFREG) =>
        {
            return Err(protocol_error(format!(
                "LSP workspace path is not a regular file: {}",
                new.display()
            )));
        }
        Ok(_) | Err(Errno::ENOENT) => {}
        Err(error) => return Err(error.into()),
    }
    renameat(
        Some(old_parent.as_raw_fd()),
        old_leaf.as_os_str(),
        Some(new_parent.as_raw_fd()),
        new_leaf.as_os_str(),
    )?;
    Ok(())
}

#[cfg(unix)]
fn atomic_write_with_permissions(
    root: &Path,
    path: &Path,
    contents: &[u8],
    permissions: Option<&fs::Permissions>,
) -> Result<(), LspError> {
    let (parent, leaf) = secure_parent(root, path)?;
    let temp = std::ffi::OsString::from(format!(".red-lsp-{}.tmp", uuid::Uuid::new_v4()));
    let raw = openat(
        Some(parent.as_raw_fd()),
        temp.as_os_str(),
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )?;
    // SAFETY: `openat` returned a new, owned descriptor.
    let mut file = unsafe { fs::File::from_raw_fd(raw) };
    let result = (|| -> Result<(), LspError> {
        file.write_all(contents)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions.clone())?;
        }
        file.sync_all()?;
        renameat(
            Some(parent.as_raw_fd()),
            temp.as_os_str(),
            Some(parent.as_raw_fd()),
            leaf.as_os_str(),
        )?;
        Ok(())
    })();
    if result.is_err() {
        let _ = unlinkat(
            Some(parent.as_raw_fd()),
            temp.as_os_str(),
            UnlinkatFlags::NoRemoveDir,
        );
    }
    result
}

#[cfg(not(unix))]
fn secure_snapshot_file(_root: &Path, path: &Path) -> Result<FileSnapshot, LspError> {
    Err(protocol_error(format!(
        "LSP unopened/resource edits require no-follow filesystem support: {}",
        path.display()
    )))
}

#[cfg(not(unix))]
fn secure_create(_root: &Path, path: &Path) -> Result<(), LspError> {
    Err(protocol_error(format!(
        "LSP resource create requires no-follow filesystem support: {}",
        path.display()
    )))
}

#[cfg(not(unix))]
fn secure_remove(_root: &Path, path: &Path) -> Result<(), LspError> {
    Err(protocol_error(format!(
        "LSP resource delete requires no-follow filesystem support: {}",
        path.display()
    )))
}

#[cfg(not(unix))]
fn secure_rename(_root: &Path, old: &Path, _new: &Path, _overwrite: bool) -> Result<(), LspError> {
    Err(protocol_error(format!(
        "LSP resource rename requires no-follow filesystem support: {}",
        old.display()
    )))
}

#[cfg(not(unix))]
fn atomic_write_with_permissions(
    _root: &Path,
    path: &Path,
    _contents: &[u8],
    _permissions: Option<&fs::Permissions>,
) -> Result<(), LspError> {
    Err(protocol_error(format!(
        "LSP resource restore requires no-follow filesystem support: {}",
        path.display()
    )))
}

fn canonical_workspace_root(root: &Path) -> Result<PathBuf, LspError> {
    let root = root.absolutize()?.to_path_buf();
    #[cfg(windows)]
    let root = {
        let value = root.to_string_lossy();
        PathBuf::from(value.strip_prefix(r"\\?\").unwrap_or(&value))
    };
    let metadata = fs::symlink_metadata(&root)?;
    if metadata.file_type().is_symlink() {
        return Err(protocol_error(format!(
            "LSP workspace root must not be a symlink: {}",
            root.display()
        )));
    }
    if !metadata.is_dir() {
        return Err(protocol_error(format!(
            "LSP workspace root is not a directory: {}",
            root.display()
        )));
    }
    Ok(root)
}

fn require_workspace_root<'a>(root: Option<&'a Path>, path: &Path) -> Result<&'a Path, LspError> {
    root.ok_or_else(|| {
        protocol_error(format!(
            "LSP edit targets unopened/resource path without a workspace root: {}",
            path.display()
        ))
    })
}

fn ensure_parent_directory(path: &Path) -> Result<(), LspError> {
    let parent = path.parent().ok_or_else(|| {
        protocol_error(format!(
            "LSP path has no parent directory: {}",
            path.display()
        ))
    })?;
    if !parent.is_dir() {
        return Err(protocol_error(format!(
            "LSP resource parent directory does not exist: {}",
            parent.display()
        )));
    }
    Ok(())
}

fn normalized_path(uri: &str) -> Result<PathBuf, LspError> {
    let path = PathBuf::from(file_path(uri)?);
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(protocol_error(format!(
                        "LSP path escapes the filesystem root: {}",
                        path.display()
                    )));
                }
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    Ok(normalized)
}

pub fn normalized_file_path(uri: &str) -> Result<String, LspError> {
    Ok(normalized_path(uri)?.to_string_lossy().into_owned())
}

fn ensure_safe_path(root: &Path, path: &Path) -> Result<(), LspError> {
    if !path.starts_with(root) {
        return Err(protocol_error(format!(
            "LSP workspace path is outside {}: {}",
            root.display(),
            path.display()
        )));
    }
    let relative = path
        .strip_prefix(root)
        .map_err(|error| protocol_error(error.to_string()))?;
    let components = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let protected = components.iter().any(|component| {
        component == ".git"
            || component == ".ssh"
            || component == ".red"
            || component == "red.toml"
            || component.starts_with(".env")
    }) || components.windows(2).any(|parts| {
        parts[0] == ".vscode" && (parts[1] == "tasks.json" || parts[1] == "launch.json")
    });
    if protected {
        return Err(protocol_error(format!(
            "LSP workspace edit targets a protected control or secret path: {}",
            path.display()
        )));
    }
    let mut current = root.to_path_buf();
    let mut seen = HashSet::new();
    for component in path
        .strip_prefix(root)
        .map_err(|error| protocol_error(error.to_string()))?
        .components()
    {
        current.push(component.as_os_str());
        if !seen.insert(current.clone()) {
            return Err(protocol_error(format!(
                "LSP workspace path repeated a component: {}",
                current.display()
            )));
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(protocol_error(format!(
                    "LSP workspace path contains a symlink: {}",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn protocol_error(message: String) -> LspError {
    LspError::ProtocolError(message)
}

fn ensure_total_budget(
    documents: &HashMap<PathBuf, VirtualDocument>,
    snapshots: &HashMap<PathBuf, FileSnapshot>,
) -> Result<(), LspError> {
    let document_bytes = documents
        .values()
        .try_fold(0usize, |total, document| {
            total.checked_add(document.contents.len())
        })
        .ok_or_else(|| protocol_error("LSP workspace edit content size overflowed".to_string()))?;
    let snapshot_bytes = snapshots
        .values()
        .filter_map(|snapshot| snapshot.contents.as_ref())
        .try_fold(0usize, |total, contents| total.checked_add(contents.len()))
        .ok_or_else(|| protocol_error("LSP workspace edit snapshot size overflowed".to_string()))?;
    let total = document_bytes
        .checked_add(snapshot_bytes)
        .ok_or_else(|| protocol_error("LSP workspace edit size overflowed".to_string()))?;
    if total > MAX_WORKSPACE_EDIT_TOTAL_BYTES {
        return Err(protocol_error(format!(
            "LSP workspace edit exceeds {MAX_WORKSPACE_EDIT_TOTAL_BYTES} total bytes"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::lsp::workspace_edit_operations;

    fn uri(path: &Path) -> String {
        file_uri(path).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn prepares_unicode_edits_for_open_and_unopened_documents() {
        let root = tempfile::tempdir().unwrap();
        let open_path = root.path().join("open café.rs");
        let closed_path = root.path().join("closed.rs");
        fs::write(&open_path, "👋 open").unwrap();
        fs::write(&closed_path, "👋 closed\r\n").unwrap();
        let operations = workspace_edit_operations(&json!({
            "changes": {
                (uri(&open_path)): [{ "range": { "start": { "line": 0, "character": 3 }, "end": { "line": 0, "character": 7 } }, "newText": "visible" }],
                (uri(&closed_path)): [{ "range": { "start": { "line": 0, "character": 3 }, "end": { "line": 0, "character": 9 } }, "newText": "hidden" }]
            }
        }))
        .unwrap();

        let prepared = prepare_workspace_edit(
            &operations,
            &[(uri(&open_path), 4)],
            vec![OpenWorkspaceDocument {
                index: 2,
                uri: uri(&open_path),
                contents: "👋 open".to_string(),
                revision: 4,
                version: Some(9),
                dirty: true,
            }],
            Some(root.path()),
        )
        .unwrap();

        assert_eq!(prepared.documents.len(), 2);
        assert!(prepared
            .documents
            .iter()
            .any(|document| { document.index == Some(2) && document.contents == "👋 visible" }));
        assert!(prepared.documents.iter().any(|document| {
            document.index.is_none() && document.contents == "👋 hidden\r\n"
        }));
    }

    #[cfg(unix)]
    #[test]
    fn creates_edits_and_renames_a_workspace_file_in_order() {
        let root = tempfile::tempdir().unwrap();
        let created = root.path().join("new.rs");
        let renamed = root.path().join("renamed.rs");
        let operations = workspace_edit_operations(&json!({
            "documentChanges": [
                { "kind": "create", "uri": uri(&created) },
                { "textDocument": { "uri": uri(&created), "version": null }, "edits": [{ "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } }, "newText": "fn main() {}" }] },
                { "kind": "rename", "oldUri": uri(&created), "newUri": uri(&renamed) }
            ]
        }))
        .unwrap();

        let prepared =
            prepare_workspace_edit(&operations, &[], Vec::new(), Some(root.path())).unwrap();
        apply_workspace_resource_operations(&prepared).unwrap();

        assert!(!created.exists());
        assert!(renamed.exists());
        assert_eq!(fs::read(&renamed).unwrap(), b"");
        assert_eq!(prepared.documents[0].uri, uri(&renamed));
        assert_eq!(prepared.documents[0].contents, "fn main() {}");
    }

    #[cfg(unix)]
    #[test]
    fn failed_resource_sequence_rolls_back_prior_operations() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first.rs");
        let moved = root.path().join("moved.rs");
        let removed = root.path().join("removed.rs");
        fs::write(&first, "first").unwrap();
        fs::write(&removed, "remove me").unwrap();
        let operations = workspace_edit_operations(&json!({
            "documentChanges": [
                { "kind": "rename", "oldUri": uri(&first), "newUri": uri(&moved) },
                { "kind": "delete", "uri": uri(&removed) }
            ]
        }))
        .unwrap();
        let prepared =
            prepare_workspace_edit(&operations, &[], Vec::new(), Some(root.path())).unwrap();

        let error = apply_workspace_resource_operations_with_hook(&prepared, |index| {
            if index == 1 {
                return Err(protocol_error("injected resource failure".to_string()));
            }
            Ok(())
        })
        .unwrap_err();

        assert!(error.to_string().contains("injected resource failure"));
        assert_eq!(fs::read_to_string(&first).unwrap(), "first");
        assert!(!moved.exists());
        assert_eq!(fs::read_to_string(&removed).unwrap(), "remove me");
    }

    #[test]
    fn rejects_stale_versions_open_deletes_outside_paths_and_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("open.rs");
        fs::write(&path, "value").unwrap();
        let open = || OpenWorkspaceDocument {
            index: 0,
            uri: uri(&path),
            contents: "dirty value".to_string(),
            revision: 3,
            version: Some(4),
            dirty: true,
        };

        let versioned = workspace_edit_operations(&json!({
            "documentChanges": [{ "textDocument": { "uri": uri(&path), "version": 5 }, "edits": [] }]
        }))
        .unwrap();
        assert!(prepare_workspace_edit(
            &versioned,
            &[(uri(&path), 3)],
            vec![open()],
            Some(root.path())
        )
        .unwrap_err()
        .to_string()
        .contains("version is stale"));

        #[cfg(unix)]
        {
            let delete = workspace_edit_operations(&json!({
                "documentChanges": [{ "kind": "delete", "uri": uri(&path) }]
            }))
            .unwrap();
            assert!(
                prepare_workspace_edit(&delete, &[], vec![open()], Some(root.path()))
                    .unwrap_err()
                    .to_string()
                    .contains("open buffer")
            );
        }

        let outside = tempfile::tempdir().unwrap();
        let outside_path = outside.path().join("outside.rs");
        fs::write(&outside_path, "outside").unwrap();
        let outside_edit = workspace_edit_operations(&json!({
            "changes": { (uri(&outside_path)): [] }
        }))
        .unwrap();
        assert!(
            prepare_workspace_edit(&outside_edit, &[], Vec::new(), Some(root.path()))
                .unwrap_err()
                .to_string()
                .contains("outside")
        );
        assert!(prepare_workspace_edit(
            &outside_edit,
            &[(uri(&outside_path), 2)],
            vec![OpenWorkspaceDocument {
                index: 0,
                uri: uri(&outside_path),
                contents: "unsaved outside".to_string(),
                revision: 2,
                version: None,
                dirty: true,
            }],
            Some(root.path()),
        )
        .unwrap_err()
        .to_string()
        .contains("outside"));

        #[cfg(unix)]
        {
            let link = root.path().join("link.rs");
            std::os::unix::fs::symlink(&outside_path, &link).unwrap();
            let link_edit =
                workspace_edit_operations(&json!({ "changes": { (uri(&link)): [] } })).unwrap();
            assert!(
                prepare_workspace_edit(&link_edit, &[], Vec::new(), Some(root.path()))
                    .unwrap_err()
                    .to_string()
                    .contains("symlink")
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn detects_resource_changes_between_prepare_and_commit() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("delete.rs");
        fs::write(&path, "original").unwrap();
        let operations = workspace_edit_operations(&json!({
            "documentChanges": [{ "kind": "delete", "uri": uri(&path) }]
        }))
        .unwrap();
        let prepared =
            prepare_workspace_edit(&operations, &[], Vec::new(), Some(root.path())).unwrap();
        fs::write(&path, "changed").unwrap();

        let error = apply_workspace_resource_operations(&prepared).unwrap_err();

        assert!(error.to_string().contains("changed during preparation"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "changed");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_later_resource_operation_and_rollback_when_a_target_changes_mid_sequence() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first.rs");
        let moved = root.path().join("moved.rs");
        let second = root.path().join("second.rs");
        fs::write(&first, "first").unwrap();
        fs::write(&second, "original").unwrap();
        let operations = workspace_edit_operations(&json!({
            "documentChanges": [
                { "kind": "rename", "oldUri": uri(&first), "newUri": uri(&moved) },
                { "kind": "delete", "uri": uri(&second) }
            ]
        }))
        .unwrap();
        let prepared =
            prepare_workspace_edit(&operations, &[], Vec::new(), Some(root.path())).unwrap();

        let error = apply_workspace_resource_operations_with_hook(&prepared, |index| {
            if index == 1 {
                fs::write(&second, "new external data").unwrap();
            }
            Ok(())
        })
        .unwrap_err();

        assert!(error.to_string().contains("rollback was refused"));
        assert_eq!(fs::read_to_string(&second).unwrap(), "new external data");
        assert!(moved.exists());
        assert!(!first.exists());
    }

    #[cfg(unix)]
    #[test]
    fn overwrite_takes_precedence_over_ignore_if_exists() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.rs");
        let created = root.path().join("created.rs");
        let destination = root.path().join("destination.rs");
        fs::write(&source, "source").unwrap();
        fs::write(&created, "old create").unwrap();
        fs::set_permissions(&created, fs::Permissions::from_mode(0o751)).unwrap();
        fs::write(&destination, "old destination").unwrap();
        let operations = workspace_edit_operations(&json!({
            "documentChanges": [
                { "kind": "create", "uri": uri(&created), "options": { "overwrite": true, "ignoreIfExists": true } },
                { "kind": "rename", "oldUri": uri(&source), "newUri": uri(&destination), "options": { "overwrite": true, "ignoreIfExists": true } }
            ]
        }))
        .unwrap();
        let prepared =
            prepare_workspace_edit(&operations, &[], Vec::new(), Some(root.path())).unwrap();
        apply_workspace_resource_operations(&prepared).unwrap();

        assert_eq!(fs::read(&created).unwrap(), b"");
        assert_eq!(
            fs::metadata(&created).unwrap().permissions().mode() & 0o777,
            0o751
        );
        assert_eq!(fs::read_to_string(&destination).unwrap(), "source");
        assert!(!source.exists());
    }

    #[cfg(not(unix))]
    #[test]
    fn unopened_and_resource_workspace_edits_fail_closed_without_mutating_disk() {
        let root = tempfile::tempdir().unwrap();
        let open = root.path().join("open.rs");
        let closed = root.path().join("closed café.rs");
        let created = root.path().join("created.rs");
        let renamed = root.path().join("renamed.rs");
        fs::write(&open, "disk open").unwrap();
        fs::write(&closed, "👋 closed\r\n").unwrap();

        let open_edit = workspace_edit_operations(&json!({
            "changes": { (uri(&open)): [{
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 12 } },
                "newText": "visible"
            }] }
        }))
        .unwrap();
        let prepared = prepare_workspace_edit(
            &open_edit,
            &[(uri(&open), 4)],
            vec![OpenWorkspaceDocument {
                index: 2,
                uri: uri(&open),
                contents: "unsaved open".to_string(),
                revision: 4,
                version: Some(9),
                dirty: true,
            }],
            Some(root.path()),
        )
        .unwrap();
        assert_eq!(prepared.documents.len(), 1);
        assert_eq!(prepared.documents[0].index, Some(2));
        assert_eq!(prepared.documents[0].contents, "visible");

        for operation in [
            json!({ "changes": { (uri(&closed)): [] } }),
            json!({ "documentChanges": [{ "kind": "create", "uri": uri(&created) }] }),
            json!({ "documentChanges": [{ "kind": "rename", "oldUri": uri(&closed), "newUri": uri(&renamed) }] }),
            json!({ "documentChanges": [{ "kind": "delete", "uri": uri(&closed) }] }),
        ] {
            let operations = workspace_edit_operations(&operation).unwrap();
            let error = prepare_workspace_edit(&operations, &[], Vec::new(), Some(root.path()))
                .unwrap_err();
            assert!(
                error.to_string().contains("no-follow filesystem support"),
                "{error}"
            );
        }

        assert_eq!(fs::read_to_string(&open).unwrap(), "disk open");
        assert_eq!(fs::read_to_string(&closed).unwrap(), "👋 closed\r\n");
        assert!(!created.exists());
        assert!(!renamed.exists());
    }

    #[test]
    fn rejects_protected_control_and_secret_paths() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".git/hooks")).unwrap();
        fs::create_dir_all(root.path().join(".vscode")).unwrap();
        for path in [
            root.path().join(".git/hooks/pre-commit"),
            root.path().join(".env.local"),
            root.path().join(".vscode/tasks.json"),
        ] {
            let operations = workspace_edit_operations(&json!({
                "documentChanges": [{ "kind": "create", "uri": uri(&path) }]
            }))
            .unwrap();
            let error = prepare_workspace_edit(&operations, &[], Vec::new(), Some(root.path()))
                .unwrap_err();
            assert!(error.to_string().contains("protected"), "{error}");
            assert!(!path.exists());
        }
    }

    #[test]
    fn rejects_workspace_edits_that_exceed_the_aggregate_content_budget() {
        let path = PathBuf::from("/tmp/too-large.rs");
        let documents = HashMap::from([(
            path.clone(),
            VirtualDocument {
                index: Some(0),
                original_uri: Some("file:///tmp/too-large.rs".to_string()),
                uri: "file:///tmp/too-large.rs".to_string(),
                original_contents: String::new(),
                contents: "x".repeat(MAX_WORKSPACE_EDIT_TOTAL_BYTES + 1),
                revision: Some(1),
                version: Some(1),
                dirty: true,
                exists: true,
                text_changed: true,
                resource_changed: false,
            },
        )]);

        let error = ensure_total_budget(&documents, &HashMap::new()).unwrap_err();

        assert!(error.to_string().contains("total bytes"));
    }
}
