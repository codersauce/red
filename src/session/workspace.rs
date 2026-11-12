//! Ordinary quit and startup use the snapshot codec in a separate workspace store.

use std::{
    fs::{File, OpenOptions, TryLockError},
    io::Read as _,
    path::{Path, PathBuf},
};

use anyhow::Context as _;
use sha2::{Digest as _, Sha256};

use super::{SessionSnapshot, SessionStore, SnapshotWriteDirectory, MAX_SESSION_SNAPSHOT_BYTES};

/// A working directory's last completed editor session, independent of crash recovery.
pub struct WorkspaceSession {
    root: PathBuf,
    workspace: PathBuf,
    owner: String,
    lock: Option<WorkspaceLock>,
}

struct WorkspaceLock(File);

impl Drop for WorkspaceLock {
    fn drop(&mut self) {
        // A child process can still have the descriptor open between fork and exec.
        // Release the lock without waiting for every copy of the descriptor to close.
        if let Err(error) = self.0.unlock() {
            crate::log!("Could not release workspace session lock: {error}");
        }
    }
}

impl WorkspaceSession {
    /// Creates a handle for a canonical working directory without reading its saved session.
    pub fn new(config_dir: &Path, workspace: PathBuf) -> Self {
        let owner = format!(
            "{:x}",
            Sha256::digest(workspace.as_os_str().as_encoded_bytes())
        );
        let root = config_dir
            .canonicalize()
            .unwrap_or_else(|_| config_dir.to_path_buf())
            .join("workspace-sessions");
        Self {
            root,
            workspace,
            owner,
            lock: None,
        }
    }

    /// Claims the workspace for an argument-free launch and loads its last completed session.
    /// A second editor starts independently while the workspace is claimed.
    pub fn claim_and_load(&mut self) -> anyhow::Result<Option<SessionSnapshot>> {
        if !self.claim()? {
            return Ok(None);
        }
        let mut snapshot = match self.store().and_then(|store| self.load(&store)) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                // A legacy fallback may still open the editor, but it cannot
                // replace a workspace snapshot it could not load.
                self.lock = None;
                return Err(error);
            }
        };
        if let Some(snapshot) = &mut snapshot {
            refresh_workspace_files(snapshot);
        }
        Ok(snapshot)
    }

    /// Claims the workspace without loading its saved view, for an explicit recovery.
    pub fn claim(&mut self) -> anyhow::Result<bool> {
        if self.lock.is_none() {
            self.lock = Self::try_lock(&self.store()?)?;
        }
        Ok(self.owns_workspace())
    }

    pub(crate) fn owns_workspace(&self) -> bool {
        self.lock.is_some()
    }

    pub(crate) fn workspace(&self) -> &Path {
        &self.workspace
    }

    fn store(&self) -> anyhow::Result<SessionStore> {
        SessionStore::for_owner(&self.root, &self.owner)
    }

    fn load(&self, store: &SessionStore) -> anyhow::Result<Option<SessionSnapshot>> {
        let snapshot = store.load_optional()?;
        if let Some(snapshot) = &snapshot {
            anyhow::ensure!(
                snapshot.cwd == self.workspace.to_string_lossy(),
                "workspace snapshot belongs to a different directory"
            );
        }
        Ok(snapshot)
    }

    fn try_lock(store: &SessionStore) -> anyhow::Result<Option<WorkspaceLock>> {
        let _directory = SnapshotWriteDirectory::open(store)?;
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options
                .mode(0o600)
                .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let lock = options.open(store.directory.join("workspace.lock"))?;
        let metadata = lock.metadata()?;
        anyhow::ensure!(metadata.is_file(), "workspace lock must be a regular file");
        #[cfg(windows)]
        anyhow::ensure!(
            !super::portable_session_reparse_point(&metadata),
            "workspace lock must not be a reparse point"
        );
        match lock.try_lock() {
            Ok(()) => Ok(Some(WorkspaceLock(lock))),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error.into()),
        }
    }

    pub(crate) fn save(
        &self,
        mut snapshot: SessionSnapshot,
        legacy_snapshot: impl FnOnce() -> Option<SessionSnapshot>,
    ) -> anyhow::Result<()> {
        let store = self.store()?;
        let _lock;
        if !self.owns_workspace() {
            // File-argument launches and parallel editors may contribute new
            // scratchpads, but must leave the saved workspace's layout intact.
            _lock =
                Self::try_lock(&store)?.context("another Red window is using this workspace")?;
            if let Some(mut existing) = self.load(&store)?.or_else(legacy_snapshot) {
                let mut last_index = existing.buffers.iter().map(|buffer| buffer.index).max();
                for mut buffer in snapshot.buffers.into_iter().filter(|buffer| {
                    buffer.path.is_none()
                        && buffer.directory.is_none()
                        && (buffer.dirty || !matches!(buffer.contents.as_str(), "" | "\n"))
                }) {
                    buffer.index = match last_index {
                        Some(index) => index.checked_add(1).context("too many saved buffers")?,
                        None => 0,
                    };
                    last_index = Some(buffer.index);
                    existing.buffers.push(buffer);
                }
                snapshot = existing;
            }
        }
        store.write(&mut snapshot)
    }
}

/// Reopens named files from disk and retains their undo trees only when the
/// saved text still matches. Unavailable files are skipped, as in legacy restore.
fn refresh_workspace_files(snapshot: &mut SessionSnapshot) {
    snapshot.buffers.retain_mut(|buffer| {
        let Some(path) = &buffer.path else {
            return true;
        };
        match read_workspace_file(Path::new(path)) {
            Ok(contents) => {
                if buffer.dirty || buffer.contents != contents {
                    buffer.undo_history = Default::default();
                    buffer.revision = 0;
                }
                buffer.contents = contents;
                buffer.saved_contents = None;
                buffer.dirty = false;
                true
            }
            Err(error) => {
                crate::log!("Workspace restore skipped {path}: {error}");
                false
            }
        }
    });
}

fn read_workspace_file(path: &Path) -> anyhow::Result<String> {
    #[cfg(unix)]
    let file = super::open_session_disk_file(path)?.context("file does not exist")?;
    #[cfg(windows)]
    let file = super::open_windows_session_disk_file(path)?.context("file does not exist")?;
    #[cfg(all(not(unix), not(windows)))]
    let file = {
        super::portable_session_file_metadata(path)?;
        File::open(path)?
    };

    let mut contents = String::new();
    anyhow::ensure!(
        file.metadata()?.is_file(),
        "workspace file must be a regular file"
    );
    file.take(MAX_SESSION_SNAPSHOT_BYTES + 1)
        .read_to_string(&mut contents)?;
    anyhow::ensure!(
        contents.len() as u64 <= MAX_SESSION_SNAPSHOT_BYTES,
        "file exceeds the session restore size limit"
    );
    Ok(contents)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn releasing_workspace_lock_does_not_wait_for_duplicate_descriptor() {
        let config = tempfile::tempdir().unwrap();
        let workspace = config.path().canonicalize().unwrap();
        let mut primary = WorkspaceSession::new(config.path(), workspace.clone());
        assert!(primary.claim().unwrap());
        let duplicate = primary.lock.as_ref().unwrap().0.try_clone().unwrap();

        let mut secondary = WorkspaceSession::new(config.path(), workspace);
        assert!(!secondary.claim().unwrap());
        drop(primary);
        assert!(secondary.claim().unwrap());
        drop(duplicate);
    }
}
