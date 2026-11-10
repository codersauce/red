//! Workspace-root validation and race-resistant file reads shared by agent backends and inline context.

use std::{
    fs::File,
    io::Read as _,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result};

pub(crate) const TOOL_CONTENT_BYTES: usize = 960 * 1024;

pub(crate) fn validate_workspace_root(root: &Path) -> Result<()> {
    anyhow::ensure!(root.is_absolute(), "workspace root must be absolute");
    let inspected = physical_workspace_root(root);
    for ancestor in inspected.ancestors() {
        let metadata =
            std::fs::symlink_metadata(ancestor).context("failed to inspect workspace root")?;
        anyhow::ensure!(
            !metadata.file_type().is_symlink(),
            "workspace root cannot contain a symlink"
        );
    }
    anyhow::ensure!(
        std::fs::symlink_metadata(inspected)?.is_dir(),
        "workspace root must be a directory"
    );
    Ok(())
}

pub(crate) fn physical_workspace_root(root: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        for (alias, target) in [
            (Path::new("/var"), Path::new("/private/var")),
            (Path::new("/tmp"), Path::new("/private/tmp")),
            (Path::new("/etc"), Path::new("/private/etc")),
        ] {
            if let Ok(suffix) = root.strip_prefix(alias) {
                return target.join(suffix);
            }
        }
    }
    root.to_path_buf()
}

#[cfg(unix)]
fn open_workspace_directory(root: &Path) -> Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};

    use nix::{
        fcntl::{openat, OFlag},
        sys::stat::Mode,
    };

    let inspected = physical_workspace_root(root);
    let descriptor = openat(
        None,
        Path::new("/"),
        OFlag::O_RDONLY
            | OFlag::O_CLOEXEC
            | OFlag::O_DIRECTORY
            | OFlag::O_NOFOLLOW
            | OFlag::O_NONBLOCK,
        Mode::empty(),
    )
    .context("failed to safely open filesystem root")?;
    // SAFETY: `openat` returned a new descriptor and `File` becomes its sole owner.
    let mut directory = unsafe { File::from_raw_fd(descriptor) };
    for component in inspected.components() {
        let name = match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => name,
            Component::ParentDir | Component::Prefix(_) => {
                anyhow::bail!("workspace root contains a non-normal path component");
            }
        };
        let descriptor = openat(
            Some(directory.as_raw_fd()),
            name,
            OFlag::O_RDONLY
                | OFlag::O_CLOEXEC
                | OFlag::O_DIRECTORY
                | OFlag::O_NOFOLLOW
                | OFlag::O_NONBLOCK,
            Mode::empty(),
        )
        .context("failed to safely open workspace root component")?;
        // SAFETY: `openat` returned a new descriptor and `File` becomes its sole owner.
        directory = unsafe { File::from_raw_fd(descriptor) };
    }
    Ok(directory)
}

#[cfg(unix)]
fn open_workspace_file_at(directory: &File, relative: &Path) -> Result<Option<File>> {
    use std::os::fd::{AsRawFd, FromRawFd};

    use nix::{
        fcntl::{openat, OFlag},
        sys::stat::Mode,
    };

    let mut components = relative.components().peekable();
    let mut opened_directory: Option<File> = None;
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            anyhow::bail!("workspace walker returned a non-normal path");
        };
        let final_component = components.peek().is_none();
        let mut flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK;
        if !final_component {
            flags |= OFlag::O_DIRECTORY;
        }
        let parent = opened_directory.as_ref().unwrap_or(directory);
        let descriptor = match openat(Some(parent.as_raw_fd()), name, flags, Mode::empty()) {
            Ok(descriptor) => descriptor,
            Err(_) => return Ok(None),
        };
        // SAFETY: `openat` returned a new descriptor and `File` becomes its sole owner.
        let file = unsafe { File::from_raw_fd(descriptor) };
        if final_component {
            return Ok(Some(file));
        }
        opened_directory = Some(file);
    }
    Ok(None)
}

#[cfg(unix)]
fn open_workspace_file(root: &Path, relative: &Path) -> Result<Option<File>> {
    open_workspace_file_at(&open_workspace_directory(root)?, relative)
}

#[cfg(unix)]
pub(crate) fn read_workspace_file(root: &Path, relative: &str) -> Result<Option<(String, u64)>> {
    let Some(file) = open_workspace_file(root, Path::new(relative))? else {
        return Ok(None);
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > TOOL_CONTENT_BYTES as u64 {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(TOOL_CONTENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > TOOL_CONTENT_BYTES {
        return Ok(None);
    }
    let byte_count = bytes.len() as u64;
    let Ok(content) = String::from_utf8(bytes) else {
        return Ok(None);
    };
    Ok(Some((content, byte_count)))
}

/// Read through the same no-symlink descriptor walk used by workspace search.
pub(crate) fn read_inline_workspace_file(
    root: &Path,
    relative: &str,
    limit: usize,
) -> Result<Option<String>> {
    #[cfg(unix)]
    {
        validate_workspace_root(root)?;
        let Some(file) = open_workspace_file(root, Path::new(relative))? else {
            return Ok(None);
        };
        read_inline_workspace_handle(file, limit)
    }
    #[cfg(not(unix))]
    {
        let _ = (root, relative, limit);
        anyhow::bail!("safe on-disk inline context reads are unavailable on this platform")
    }
}

/// A validated directory handle that confines every search read with `openat`.
#[cfg(unix)]
pub(crate) struct InlineWorkspaceReader {
    directory: File,
}

#[cfg(unix)]
impl InlineWorkspaceReader {
    pub(crate) fn new(root: &Path) -> Result<Self> {
        validate_workspace_root(root)?;
        Ok(Self {
            directory: open_workspace_directory(root)?,
        })
    }

    pub(crate) fn read(&self, relative: &str, limit: usize) -> Result<Option<String>> {
        let Some(file) = open_workspace_file_at(&self.directory, Path::new(relative))? else {
            return Ok(None);
        };
        read_inline_workspace_handle(file, limit)
    }
}

#[cfg(unix)]
fn read_inline_workspace_handle(file: File, limit: usize) -> Result<Option<String>> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit || bytes.contains(&0) {
        return Ok(None);
    }
    Ok(String::from_utf8(bytes).ok())
}
