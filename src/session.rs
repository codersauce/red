//! Crash-safe, core-owned editor session snapshots.

use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Read as _, Write as _},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(unix)]
use std::path::Component;

use serde::{Deserialize, Serialize};

use crate::{
    agent_workspace::ProposalWorkspaceSnapshot,
    editor::Content,
    undo::{TextPosition, UndoHistory},
    window::WindowManagerSnapshot,
};

pub const SESSION_SCHEMA_VERSION: u32 = 2;
const MAX_SESSION_DISK_CONTENT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SESSION_SNAPSHOT_BYTES: u64 = 256 * 1024 * 1024;
static NEXT_TEMPORARY_SNAPSHOT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionDiskFingerprint {
    len: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    modified_seconds: i64,
    #[cfg(unix)]
    modified_nanoseconds: i64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
    #[cfg(not(unix))]
    modified: Option<std::time::SystemTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub version: u32,
    #[serde(default)]
    pub generation: u64,
    pub cwd: String,
    pub saved_at_ms: u64,
    pub buffers: Vec<SessionBufferSnapshot>,
    pub current_buffer_index: usize,
    pub window_layout: WindowManagerSnapshot,
    #[serde(default)]
    pub registers: HashMap<char, Content>,
    #[serde(default)]
    pub jumps: Vec<SessionJump>,
    #[serde(default)]
    pub jump_index: usize,
    #[serde(default)]
    pub local_marks: Vec<SessionMark>,
    #[serde(default)]
    pub global_marks: Vec<SessionMark>,
    #[serde(default)]
    pub special_marks: Vec<SessionMark>,
    #[serde(default)]
    pub agent_transcript: Option<String>,
    #[serde(default)]
    pub agent_workspace: Option<ProposalWorkspaceSnapshot>,
    /// False means the transcript is archived context after recovery. Red never
    /// invents ACP resume support that the adapter did not negotiate.
    #[serde(default)]
    pub agent_session_resumable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionBufferSnapshot {
    pub index: usize,
    pub path: Option<String>,
    pub contents: String,
    pub dirty: bool,
    pub revision: u64,
    pub cursor_x: usize,
    pub cursor_y: usize,
    pub viewport_top: usize,
    pub undo_history: UndoHistory,
    #[serde(default)]
    pub disk_contents: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionJump {
    pub file: Option<String>,
    pub x: usize,
    pub y: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionAnchorAffinity {
    Left,
    Right,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionMark {
    pub name: char,
    pub buffer_index: usize,
    pub file: Option<String>,
    pub char_index: usize,
    pub fallback: TextPosition,
    pub affinity: SessionAnchorAffinity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryDivergence {
    pub path: String,
    pub diff: String,
}

#[derive(Debug, Clone)]
pub struct SessionStore {
    directory: PathBuf,
    namespace_root: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum SnapshotFault {
    None,
    AfterTempSync,
    AfterRotate,
}

impl SessionStore {
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            namespace_root: None,
        }
    }

    pub fn for_owner(directory: impl AsRef<Path>, owner: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !owner.is_empty()
                && owner != "."
                && owner != ".."
                && owner
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character)),
            "session snapshot owner may contain only letters, numbers, dash, underscore, and dot"
        );
        let directory = directory.as_ref();
        if let Ok(metadata) = fs::symlink_metadata(directory) {
            anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "session snapshot root must not be a symlink"
            );
        }
        Ok(Self {
            directory: directory.join(owner),
            namespace_root: Some(directory.to_path_buf()),
        })
    }

    pub fn load_latest(directory: impl AsRef<Path>) -> anyhow::Result<SessionSnapshot> {
        Self::load_latest_with_store(directory).map(|(_, snapshot)| snapshot)
    }

    pub fn load_latest_with_store(
        directory: impl AsRef<Path>,
    ) -> anyhow::Result<(Self, SessionSnapshot)> {
        let directory = directory.as_ref();
        if let Ok(metadata) = fs::symlink_metadata(directory) {
            anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "session snapshot root must not be a symlink"
            );
        }

        let mut stores = vec![Self::new(directory)];
        match fs::read_dir(directory) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    if entry.file_type()?.is_dir() {
                        stores.push(Self {
                            directory: entry.path(),
                            namespace_root: Some(directory.to_path_buf()),
                        });
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        let mut latest = None;
        let mut last_error = None;
        for store in stores {
            match store.load() {
                Ok(snapshot) => {
                    let recoverable = snapshot.buffers.iter().any(|buffer| buffer.dirty)
                        || snapshot
                            .agent_workspace
                            .as_ref()
                            .is_some_and(ProposalWorkspaceSnapshot::has_pending_files);
                    let newer = latest.as_ref().is_none_or(
                        |(_, current, current_recoverable): &(Self, SessionSnapshot, bool)| {
                            (recoverable, snapshot.saved_at_ms, snapshot.generation)
                                > (
                                    *current_recoverable,
                                    current.saved_at_ms,
                                    current.generation,
                                )
                        },
                    );
                    if newer {
                        latest = Some((store, snapshot, recoverable));
                    }
                }
                Err(error) => last_error = Some(error),
            }
        }
        latest
            .map(|(store, snapshot, _)| (store, snapshot))
            .ok_or_else(|| {
                last_error
                    .unwrap_or_else(|| anyhow::anyhow!("no recoverable session snapshots found"))
            })
    }

    #[must_use]
    pub fn latest_path(&self) -> PathBuf {
        self.directory.join("latest.json")
    }

    fn previous_path(&self) -> PathBuf {
        self.directory.join("previous.json")
    }

    fn temporary_path(&self) -> PathBuf {
        let id = NEXT_TEMPORARY_SNAPSHOT_ID.fetch_add(1, Ordering::Relaxed);
        self.directory
            .join(format!("snapshot-{}-{id}.tmp", std::process::id()))
    }

    pub fn load(&self) -> anyhow::Result<SessionSnapshot> {
        match read_snapshot(&self.latest_path()) {
            Ok(snapshot) => validate_snapshot(snapshot),
            Err(latest_error)
                if latest_error.kind() == io::ErrorKind::InvalidData
                    || latest_error.kind() == io::ErrorKind::NotFound =>
            {
                read_snapshot(&self.previous_path())
                    .and_then(|snapshot| {
                        validate_snapshot(snapshot)
                            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
                    })
                    .map_err(|previous_error| {
                        anyhow::anyhow!(
                            "latest session snapshot is invalid ({latest_error}); last known-good snapshot is unavailable ({previous_error})"
                        )
                    })
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn write(&self, snapshot: &mut SessionSnapshot) -> anyhow::Result<()> {
        self.write_with_fault(snapshot, SnapshotFault::None)
    }

    #[doc(hidden)]
    pub fn write_with_fault(
        &self,
        snapshot: &mut SessionSnapshot,
        fault: SnapshotFault,
    ) -> anyhow::Result<()> {
        if let Some(root) = self.namespace_root.as_deref() {
            fs::create_dir_all(root)?;
            anyhow::ensure!(
                !fs::symlink_metadata(root)?.file_type().is_symlink(),
                "session snapshot root must not be a symlink"
            );
            set_directory_permissions(root)?;
        }
        fs::create_dir_all(&self.directory)?;
        anyhow::ensure!(
            !fs::symlink_metadata(&self.directory)?
                .file_type()
                .is_symlink(),
            "session snapshot directory must not be a symlink"
        );
        set_directory_permissions(&self.directory)?;

        let (previous_generation, rotate_latest) = self.previous_generation_for_write()?;
        snapshot.version = SESSION_SCHEMA_VERSION;
        snapshot.generation = previous_generation.saturating_add(1);
        let encoded = serde_json::to_vec(snapshot)?;
        anyhow::ensure!(
            encoded.len() as u64 <= MAX_SESSION_SNAPSHOT_BYTES,
            "session snapshot exceeds the {MAX_SESSION_SNAPSHOT_BYTES}-byte recovery limit"
        );
        let temporary_path = self.temporary_path();
        let mut temporary_created = false;
        let result = (|| -> anyhow::Result<()> {
            let mut temporary = restrictive_file(&temporary_path)?;
            temporary_created = true;
            temporary.write_all(&encoded)?;
            temporary.sync_all()?;
            if fault == SnapshotFault::AfterTempSync {
                anyhow::bail!("injected snapshot failure after temporary-file sync");
            }

            let latest_path = self.latest_path();
            let previous_path = self.previous_path();
            if rotate_latest {
                if previous_path.try_exists()? {
                    fs::remove_file(&previous_path)?;
                }
                fs::rename(&latest_path, &previous_path)?;
            } else if latest_path.try_exists()? {
                fs::remove_file(&latest_path)?;
            }
            if fault == SnapshotFault::AfterRotate {
                anyhow::bail!("injected snapshot failure after generation rotation");
            }
            fs::rename(&temporary_path, &latest_path)?;
            sync_directory(&self.directory)?;
            Ok(())
        })();
        if result.is_err() && temporary_created {
            let _ = fs::remove_file(&temporary_path);
        }
        result
    }

    fn previous_generation_for_write(&self) -> anyhow::Result<(u64, bool)> {
        let latest_error = match read_snapshot(&self.latest_path()) {
            Ok(snapshot) => return Ok((validate_snapshot(snapshot)?.generation, true)),
            Err(error)
                if error.kind() == io::ErrorKind::InvalidData
                    || error.kind() == io::ErrorKind::NotFound =>
            {
                error
            }
            Err(error) => return Err(error.into()),
        };

        match read_snapshot(&self.previous_path()) {
            Ok(snapshot) => Ok((validate_snapshot(snapshot)?.generation, false)),
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    && latest_error.kind() == io::ErrorKind::NotFound =>
            {
                Ok((0, false))
            }
            Err(error) => anyhow::bail!(
                "cannot replace the latest session snapshot ({latest_error}); last known-good snapshot is unavailable ({error})"
            ),
        }
    }
}

pub fn detect_disk_divergence(snapshot: &SessionSnapshot) -> Vec<RecoveryDivergence> {
    snapshot
        .buffers
        .iter()
        .filter_map(|buffer| {
            let path = buffer.path.as_deref()?;
            let expected = buffer.disk_contents.as_deref();
            let actual_result = read_current_session_disk_contents(Path::new(path));
            let unreadable = actual_result.is_err();
            let actual = actual_result.ok().flatten();
            (unreadable || actual.as_deref() != expected).then(|| RecoveryDivergence {
                path: path.to_string(),
                diff: similar::TextDiff::from_lines(
                    expected.unwrap_or_default(),
                    if unreadable {
                        "[current disk could not be read safely]\n"
                    } else {
                        actual.as_deref().unwrap_or_default()
                    },
                )
                .unified_diff()
                .header("snapshot disk base", "current disk")
                .to_string(),
            })
        })
        .collect()
}

pub(crate) fn capture_session_disk_fingerprint(
    path: &Path,
) -> io::Result<Option<SessionDiskFingerprint>> {
    #[cfg(unix)]
    {
        let Some(file) = open_session_disk_file(path)? else {
            return Ok(None);
        };
        session_disk_fingerprint(&file.metadata()?).map(Some)
    }
    #[cfg(not(unix))]
    {
        match fs::symlink_metadata(path) {
            Ok(metadata) => session_disk_fingerprint(&metadata).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

pub(crate) fn read_session_disk_contents(
    path: &Path,
    expected: SessionDiskFingerprint,
) -> io::Result<String> {
    #[cfg(unix)]
    let file = open_session_disk_file(path)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "session disk base was removed before snapshot read",
        )
    })?;
    #[cfg(not(unix))]
    let file = OpenOptions::new().read(true).open(path)?;
    let before = session_disk_fingerprint(&file.metadata()?)?;
    if before != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "session disk base changed before snapshot read",
        ));
    }

    let mut contents = String::new();
    (&file)
        .take(MAX_SESSION_DISK_CONTENT_BYTES + 1)
        .read_to_string(&mut contents)?;
    let after = session_disk_fingerprint(&file.metadata()?)?;
    let at_path = capture_session_disk_fingerprint(path)?;
    if contents.len() as u64 != expected.len || after != expected || at_path != Some(expected) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "session disk base changed during snapshot read",
        ));
    }
    Ok(contents)
}

#[cfg(unix)]
fn open_session_disk_file(path: &Path) -> io::Result<Option<File>> {
    use std::{
        ffi::OsStr,
        os::fd::{AsRawFd as _, FromRawFd as _},
    };

    use nix::{
        errno::Errno,
        fcntl::{openat, OFlag},
        sys::stat::Mode,
    };

    let path = normalized_session_disk_path(path)?;
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name),
            Component::ParentDir => Some(OsStr::new("..")),
            Component::RootDir => None,
            Component::CurDir | Component::Prefix(_) => None,
        })
        .collect::<Vec<_>>();
    if components.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session disk base must be a regular file below the filesystem root",
        ));
    }
    let mut directory = File::open("/")?;
    for (index, component) in components.iter().enumerate() {
        let final_component = index + 1 == components.len();
        let mut flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK;
        if !final_component {
            flags |= OFlag::O_DIRECTORY;
        }
        let descriptor = match openat(
            Some(directory.as_raw_fd()),
            *component,
            flags,
            Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(Errno::ENOENT) => return Ok(None),
            Err(error) => return Err(io::Error::from_raw_os_error(error as i32)),
        };
        // SAFETY: `openat` returned a new owned descriptor and `File` becomes its sole owner.
        let file = unsafe { File::from_raw_fd(descriptor) };
        if final_component {
            return Ok(Some(file));
        }
        directory = file;
    }
    Ok(None)
}

#[cfg(unix)]
fn normalized_session_disk_path(path: &Path) -> io::Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    #[cfg(target_os = "macos")]
    let path = {
        let mut physical = path.clone();
        for (alias, target) in [
            (Path::new("/var"), Path::new("/private/var")),
            (Path::new("/tmp"), Path::new("/private/tmp")),
            (Path::new("/etc"), Path::new("/private/etc")),
        ] {
            if let Ok(remainder) = path.strip_prefix(alias) {
                physical = target.join(remainder);
                break;
            }
        }
        physical
    };
    if path
        .components()
        .any(|component| matches!(component, Component::Prefix(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session disk base contains an unsupported path prefix",
        ));
    }
    Ok(path)
}

fn read_current_session_disk_contents(path: &Path) -> io::Result<Option<String>> {
    let Some(fingerprint) = capture_session_disk_fingerprint(path)? else {
        return Ok(None);
    };
    read_session_disk_contents(path, fingerprint).map(Some)
}

fn session_disk_fingerprint(metadata: &fs::Metadata) -> io::Result<SessionDiskFingerprint> {
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "session disk base is not a regular file",
        ));
    }
    if metadata.len() > MAX_SESSION_DISK_CONTENT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "session disk base exceeds the snapshot read limit",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        Ok(SessionDiskFingerprint {
            len: metadata.len(),
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }
    #[cfg(not(unix))]
    {
        Ok(SessionDiskFingerprint {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
}

fn validate_snapshot(mut snapshot: SessionSnapshot) -> anyhow::Result<SessionSnapshot> {
    anyhow::ensure!(
        snapshot.version <= SESSION_SCHEMA_VERSION,
        "session snapshot version {} is newer than supported version {}",
        snapshot.version,
        SESSION_SCHEMA_VERSION
    );
    anyhow::ensure!(
        snapshot.version > 0,
        "session snapshot version must be positive"
    );
    // Versions in the supported range use serde defaults as their migration path.
    snapshot.version = SESSION_SCHEMA_VERSION;
    Ok(snapshot)
}

fn read_snapshot(path: &Path) -> io::Result<SessionSnapshot> {
    #[cfg(unix)]
    let file = match open_session_disk_file(path) {
        Ok(Some(file)) => file,
        Ok(None) => return Err(io::Error::from(io::ErrorKind::NotFound)),
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(code) if code == nix::errno::Errno::ELOOP as i32
                    || code == nix::errno::Errno::ENOTDIR as i32
            ) =>
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("session snapshot path cannot contain a symlink or non-directory component: {error}"),
            ));
        }
        Err(error) => return Err(error),
    };
    #[cfg(not(unix))]
    let file = {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "session snapshot must be a regular file and cannot be a symlink",
            ));
        }
        OpenOptions::new().read(true).open(path)?
    };

    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "session snapshot is not a regular file",
        ));
    }
    if metadata.len() > MAX_SESSION_SNAPSHOT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "session snapshot exceeds the {MAX_SESSION_SNAPSHOT_BYTES}-byte recovery limit"
            ),
        ));
    }
    let mut contents = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_SESSION_SNAPSHOT_BYTES + 1)
        .read_to_end(&mut contents)?;
    if contents.len() as u64 > MAX_SESSION_SNAPSHOT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "session snapshot exceeds the {MAX_SESSION_SNAPSHOT_BYTES}-byte recovery limit"
            ),
        ));
    }
    serde_json::from_slice(&contents)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn restrictive_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

fn set_directory_permissions(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agent_workspace::ProposalWorkspace,
        window::{SplitSnapshot, WindowManagerSnapshot},
    };

    fn snapshot(contents: &str) -> SessionSnapshot {
        SessionSnapshot {
            version: SESSION_SCHEMA_VERSION,
            generation: 0,
            cwd: "/workspace".to_string(),
            saved_at_ms: 1,
            buffers: vec![SessionBufferSnapshot {
                index: 0,
                path: None,
                contents: contents.to_string(),
                dirty: true,
                revision: 1,
                cursor_x: 0,
                cursor_y: 0,
                viewport_top: 0,
                undo_history: UndoHistory::default(),
                disk_contents: None,
            }],
            current_buffer_index: 0,
            window_layout: WindowManagerSnapshot {
                active_window_id: 0,
                root: SplitSnapshot::Window {
                    buffer_index: 0,
                    vtop: 0,
                    vleft: 0,
                    skipcol: 0,
                    wrap: true,
                    cx: 0,
                    cy: 0,
                    vx: 0,
                },
            },
            registers: HashMap::new(),
            jumps: Vec::new(),
            jump_index: 0,
            local_marks: Vec::new(),
            global_marks: Vec::new(),
            special_marks: Vec::new(),
            agent_transcript: None,
            agent_workspace: None,
            agent_session_resumable: false,
        }
    }

    #[test]
    fn crash_during_snapshot_keeps_a_loadable_generation() {
        let directory = tempfile::tempdir().unwrap();
        let store = SessionStore::new(directory.path());
        let mut first = snapshot("first");
        store.write(&mut first).unwrap();

        let mut second = snapshot("second");
        assert!(store
            .write_with_fault(&mut second, SnapshotFault::AfterRotate)
            .is_err());
        assert_eq!(store.load().unwrap().buffers[0].contents, "first");

        store.write(&mut second).unwrap();
        assert_eq!(store.load().unwrap().buffers[0].contents, "second");
    }

    #[test]
    fn future_snapshot_versions_fail_without_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let store = SessionStore::new(directory.path());
        let mut future = snapshot("future");
        future.version = SESSION_SCHEMA_VERSION + 1;
        fs::create_dir_all(directory.path()).unwrap();
        fs::write(store.latest_path(), serde_json::to_vec(&future).unwrap()).unwrap();

        let error = store.load().unwrap_err().to_string();
        assert!(error.contains("newer than supported"));

        let encoded = fs::read(store.latest_path()).unwrap();
        let mut replacement = snapshot("replacement");
        let error = store.write(&mut replacement).unwrap_err().to_string();
        assert!(error.contains("newer than supported"));
        assert_eq!(fs::read(store.latest_path()).unwrap(), encoded);
    }

    #[test]
    fn corrupt_latest_never_replaces_the_last_known_good_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let store = SessionStore::new(directory.path());
        let mut first = snapshot("first");
        let mut second = snapshot("second");
        store.write(&mut first).unwrap();
        store.write(&mut second).unwrap();
        fs::write(store.latest_path(), b"not a snapshot").unwrap();

        let mut third = snapshot("third");
        store.write(&mut third).unwrap();

        assert_eq!(store.load().unwrap().buffers[0].contents, "third");
        let previous = read_snapshot(&store.previous_path()).unwrap();
        assert_eq!(previous.buffers[0].contents, "first");
    }

    #[test]
    fn failed_temporary_write_is_removed() {
        let directory = tempfile::tempdir().unwrap();
        let store = SessionStore::new(directory.path());
        let mut value = snapshot("temporary");

        assert!(store
            .write_with_fault(&mut value, SnapshotFault::AfterTempSync)
            .is_err());
        let temporary_files = fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().ends_with(".tmp"))
            .collect::<Vec<_>>();
        assert!(temporary_files.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_write_through_a_symlinked_snapshot_directory() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        fs::create_dir(&target).unwrap();
        let link = directory.path().join("sessions");
        symlink(&target, &link).unwrap();
        let store = SessionStore::new(&link);
        let mut value = snapshot("private");

        let error = store.write(&mut value).unwrap_err().to_string();

        assert!(error.contains("must not be a symlink"));
        assert!(!target.join("latest.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_snapshot_files_fail_closed_for_load_write_and_resume() {
        use nix::{sys::stat::Mode, unistd::mkfifo};
        use std::os::unix::fs::symlink;

        for source in ["symlink", "fifo", "oversized"] {
            let directory = tempfile::tempdir().unwrap();
            let store = SessionStore::new(directory.path());
            let latest = store.latest_path();
            let outside = directory.path().join("outside.json");
            fs::write(&outside, b"outside secret").unwrap();
            match source {
                "symlink" => symlink(&outside, &latest).unwrap(),
                "fifo" => mkfifo(&latest, Mode::S_IRUSR | Mode::S_IWUSR).unwrap(),
                "oversized" => File::create(&latest)
                    .unwrap()
                    .set_len(MAX_SESSION_SNAPSHOT_BYTES + 1)
                    .unwrap(),
                _ => unreachable!(),
            }

            let read_error = read_snapshot(&latest).unwrap_err();
            assert_eq!(read_error.kind(), io::ErrorKind::InvalidData, "{source}");
            let message = read_error.to_string();
            assert!(
                message.contains("symlink")
                    || message.contains("regular file")
                    || message.contains("recovery limit"),
                "{source}: {message}"
            );
            assert!(store.load().is_err(), "{source}");
            assert!(
                SessionStore::load_latest(directory.path()).is_err(),
                "{source}"
            );

            let mut replacement = snapshot("replacement");
            let write_error = store.write(&mut replacement).unwrap_err().to_string();
            assert!(
                write_error.contains("cannot replace the latest session snapshot"),
                "{source}: {write_error}"
            );
            assert_eq!(fs::read(&outside).unwrap(), b"outside secret", "{source}");

            fs::write(
                store.previous_path(),
                serde_json::to_vec(&snapshot("known good")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                store.load().unwrap().buffers[0].contents,
                "known good",
                "{source}"
            );
            assert_eq!(
                SessionStore::load_latest_with_store(directory.path())
                    .unwrap()
                    .1
                    .buffers[0]
                    .contents,
                "known good",
                "{source}"
            );

            store.write(&mut replacement).unwrap();
            assert_eq!(
                store.load().unwrap().buffers[0].contents,
                "replacement",
                "{source}"
            );
            assert_eq!(fs::read(&outside).unwrap(), b"outside secret", "{source}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn snapshots_are_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let store = SessionStore::new(directory.path().join("sessions"));
        let mut value = snapshot("private");
        store.write(&mut value).unwrap();

        assert_eq!(
            fs::metadata(store.latest_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        let temporary = directory.path().join("existing.tmp");
        fs::write(&temporary, b"existing").unwrap();
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o666)).unwrap();
        assert_eq!(
            restrictive_file(&temporary).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&temporary).unwrap(), b"existing");
    }

    #[cfg(unix)]
    #[test]
    fn namespaced_snapshot_roots_are_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("sessions");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        let store = SessionStore::for_owner(&root, "editor-one").unwrap();
        let mut value = snapshot("private");

        store.write(&mut value).unwrap();

        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(root.join("editor-one"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(store.latest_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn an_external_edit_after_snapshot_freeze_cannot_hide_disk_divergence() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("buffer.txt");
        fs::write(&path, "base\n").unwrap();
        let fingerprint = capture_session_disk_fingerprint(&path).unwrap().unwrap();
        let replacement = directory.path().join("replacement.txt");
        fs::write(&replacement, "edit\n").unwrap();
        fs::rename(replacement, &path).unwrap();
        let mut value = snapshot("unsaved buffer\n");
        value.buffers[0].path = Some(path.to_string_lossy().into_owned());
        value.buffers[0].disk_contents = read_session_disk_contents(&path, fingerprint).ok();

        assert!(value.buffers[0].disk_contents.is_none());
        let divergences = detect_disk_divergence(&value);
        assert_eq!(divergences.len(), 1);
        assert!(divergences[0].diff.contains("edit"));
    }

    #[test]
    fn unchanged_regular_disk_bases_are_read_within_the_snapshot_bound() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("buffer.txt");
        fs::write(&path, "stable\n").unwrap();
        let fingerprint = capture_session_disk_fingerprint(&path).unwrap().unwrap();

        assert_eq!(
            read_session_disk_contents(&path, fingerprint).unwrap(),
            "stable\n"
        );

        fs::write(
            &path,
            vec![b'x'; (MAX_SESSION_DISK_CONTENT_BYTES + 1) as usize],
        )
        .unwrap();
        assert!(capture_session_disk_fingerprint(&path).is_err());
        let mut value = snapshot("unsaved buffer\n");
        value.buffers[0].path = Some(path.to_string_lossy().into_owned());
        value.buffers[0].disk_contents = None;
        let divergences = detect_disk_divergence(&value);
        assert_eq!(divergences.len(), 1);
        assert!(divergences[0]
            .diff
            .contains("current disk could not be read safely"));
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_backing_files_fail_closed_during_resume_divergence_checks() {
        use nix::{sys::stat::Mode, unistd::mkfifo};
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.txt");
        fs::write(&target, "outside\n").unwrap();
        let symlink_path = directory.path().join("link.txt");
        symlink(&target, &symlink_path).unwrap();
        let fifo_path = directory.path().join("blocked.fifo");
        mkfifo(&fifo_path, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();

        for path in [symlink_path, fifo_path] {
            assert!(capture_session_disk_fingerprint(&path).is_err());
            let mut value = snapshot("unsaved buffer\n");
            value.buffers[0].path = Some(path.to_string_lossy().into_owned());
            value.buffers[0].disk_contents = None;

            let divergences = detect_disk_divergence(&value);
            assert_eq!(divergences.len(), 1);
            assert!(divergences[0]
                .diff
                .contains("current disk could not be read safely"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_replaced_ancestor_cannot_escape_snapshot_reads_or_resume_divergence() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let source = workspace.join("source");
        let moved_source = workspace.join("original-source");
        let outside = directory.path().join("outside");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir(&outside).unwrap();
        let path = source.join("buffer.txt");
        fs::write(&path, "trusted base\n").unwrap();
        fs::write(outside.join("buffer.txt"), "outside secret\n").unwrap();
        let fingerprint = capture_session_disk_fingerprint(&path).unwrap().unwrap();

        fs::rename(&source, moved_source).unwrap();
        symlink(&outside, &source).unwrap();

        assert!(capture_session_disk_fingerprint(&path).is_err());
        assert!(read_session_disk_contents(&path, fingerprint).is_err());
        let mut value = snapshot("unsaved buffer\n");
        value.buffers[0].path = Some(path.to_string_lossy().into_owned());
        value.buffers[0].disk_contents = None;
        let divergences = detect_disk_divergence(&value);
        assert_eq!(divergences.len(), 1);
        assert!(divergences[0]
            .diff
            .contains("current disk could not be read safely"));
        assert!(!divergences[0].diff.contains("outside secret"));
    }

    #[cfg(unix)]
    #[test]
    fn a_parent_component_cannot_hide_a_symlinked_snapshot_ancestor() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let outside = directory.path().join("outside");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(workspace.join("child")).unwrap();
        fs::create_dir_all(outside.join("child")).unwrap();
        fs::write(workspace.join("buffer.txt"), "workspace base\n").unwrap();
        fs::write(outside.join("buffer.txt"), "outside secret\n").unwrap();
        symlink(outside.join("child"), workspace.join("linked")).unwrap();

        let safe_path = workspace.join("child/../buffer.txt");
        let fingerprint = capture_session_disk_fingerprint(&safe_path)
            .unwrap()
            .unwrap();
        assert_eq!(
            read_session_disk_contents(&safe_path, fingerprint).unwrap(),
            "workspace base\n"
        );

        let path = workspace.join("linked/../buffer.txt");
        assert_eq!(fs::read_to_string(&path).unwrap(), "outside secret\n");
        let error = capture_session_disk_fingerprint(&path).unwrap_err();
        assert!(matches!(
            error.raw_os_error(),
            Some(code) if code == nix::errno::Errno::ELOOP as i32
                || code == nix::errno::Errno::ENOTDIR as i32
        ));

        let mut value = snapshot("unsaved buffer\n");
        value.buffers[0].path = Some(path.to_string_lossy().into_owned());
        value.buffers[0].disk_contents = None;
        let divergences = detect_disk_divergence(&value);
        assert_eq!(divergences.len(), 1);
        assert!(divergences[0]
            .diff
            .contains("current disk could not be read safely"));
        assert!(!divergences[0].diff.contains("outside secret"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn snapshot_reads_preserve_the_macos_var_alias() {
        let directory = tempfile::tempdir().unwrap();
        let physical = fs::canonicalize(directory.path()).unwrap();
        let remainder = physical
            .strip_prefix("/private/var")
            .expect("macOS temporary directory should be under /private/var");
        let alias = Path::new("/var").join(remainder).join("buffer.txt");
        fs::write(physical.join("buffer.txt"), "trusted base\n").unwrap();

        let fingerprint = capture_session_disk_fingerprint(&alias).unwrap().unwrap();

        assert_eq!(
            read_session_disk_contents(&alias, fingerprint).unwrap(),
            "trusted base\n"
        );
    }

    #[test]
    fn owner_namespaces_are_independent_and_resume_loads_the_latest() {
        let directory = tempfile::tempdir().unwrap();
        let first = SessionStore::for_owner(directory.path(), "editor-one").unwrap();
        let second = SessionStore::for_owner(directory.path(), "detached-work").unwrap();
        let mut older = snapshot("older");
        older.saved_at_ms = 10;
        let mut newer = snapshot("newer");
        newer.saved_at_ms = 20;

        first.write(&mut older).unwrap();
        second.write(&mut newer).unwrap();

        assert_eq!(first.load().unwrap().buffers[0].contents, "older");
        assert_eq!(second.load().unwrap().buffers[0].contents, "newer");
        assert_eq!(
            SessionStore::load_latest(directory.path()).unwrap().buffers[0].contents,
            "newer"
        );
        assert_ne!(first.latest_path(), second.latest_path());
    }

    #[test]
    fn resume_prefers_older_dirty_work_and_reuses_its_owner_until_clean() {
        let directory = tempfile::tempdir().unwrap();
        let crashed = SessionStore::for_owner(directory.path(), "editor-crashed").unwrap();
        let newer = SessionStore::for_owner(directory.path(), "editor-newer").unwrap();
        let mut dirty = snapshot("unsaved work");
        dirty.saved_at_ms = 10;
        let mut clean = snapshot("newer clean");
        clean.saved_at_ms = 20;
        clean.buffers[0].dirty = false;

        crashed.write(&mut dirty).unwrap();
        newer.write(&mut clean).unwrap();

        let (resumed_store, mut resumed) =
            SessionStore::load_latest_with_store(directory.path()).unwrap();
        assert_eq!(resumed_store.latest_path(), crashed.latest_path());
        assert_eq!(resumed.buffers[0].contents, "unsaved work");

        resumed.buffers[0].contents = "saved work".to_string();
        resumed.buffers[0].dirty = false;
        resumed.saved_at_ms = 30;
        resumed_store.write(&mut resumed).unwrap();

        let repeated = SessionStore::load_latest(directory.path()).unwrap();
        assert_eq!(repeated.buffers[0].contents, "saved work");
        assert!(!repeated.buffers[0].dirty);
    }

    #[test]
    fn resume_prefers_older_pending_proposals_over_a_newer_clean_session() {
        let directory = tempfile::tempdir().unwrap();
        let crashed = SessionStore::for_owner(directory.path(), "editor-crashed").unwrap();
        let newer = SessionStore::for_owner(directory.path(), "editor-newer").unwrap();
        let path = directory.path().join("proposal.txt");
        fs::write(&path, "base\n").unwrap();
        let mut workspace = ProposalWorkspace::new(directory.path()).unwrap();
        workspace.begin_turn("archived", "turn-1".to_string());
        workspace
            .write("archived", &path, "proposed\n".to_string())
            .unwrap();
        let mut pending = snapshot("pending proposal");
        pending.saved_at_ms = 10;
        pending.buffers[0].dirty = false;
        pending.agent_workspace = Some(workspace.snapshot());
        let mut clean = snapshot("newer clean");
        clean.saved_at_ms = 20;
        clean.buffers[0].dirty = false;

        crashed.write(&mut pending).unwrap();
        newer.write(&mut clean).unwrap();

        assert_eq!(
            SessionStore::load_latest(directory.path()).unwrap().buffers[0].contents,
            "pending proposal"
        );
    }

    #[test]
    fn resume_still_loads_a_legacy_root_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let legacy = SessionStore::new(directory.path());
        let mut value = snapshot("legacy");
        legacy.write(&mut value).unwrap();

        assert_eq!(
            SessionStore::load_latest(directory.path()).unwrap().buffers[0].contents,
            "legacy"
        );
    }

    #[test]
    fn owner_namespaces_reject_traversal() {
        let directory = tempfile::tempdir().unwrap();

        assert!(SessionStore::for_owner(directory.path(), "../outside").is_err());
        assert!(SessionStore::for_owner(directory.path(), ".").is_err());
        assert!(SessionStore::for_owner(directory.path(), "..").is_err());
    }
}
