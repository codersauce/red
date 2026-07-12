//! Crash-safe, core-owned editor session snapshots.

use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, Read as _, Write as _},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(not(unix))]
use std::fs::OpenOptions;
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
        #[cfg(not(unix))]
        match portable_session_directory(directory, /*create*/ false) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
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

    fn temporary_name() -> String {
        let id = NEXT_TEMPORARY_SNAPSHOT_ID.fetch_add(1, Ordering::Relaxed);
        format!("snapshot-{}-{id}.tmp", std::process::id())
    }

    pub fn load(&self) -> anyhow::Result<SessionSnapshot> {
        let validated = |path: &Path| {
            read_snapshot(path).and_then(|snapshot| {
                validate_snapshot(snapshot)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
            })
        };
        match validated(&self.latest_path()) {
            Ok(snapshot) => Ok(snapshot),
            Err(latest_error)
                if latest_error.kind() == io::ErrorKind::InvalidData
                    || latest_error.kind() == io::ErrorKind::NotFound =>
            {
                validated(&self.previous_path()).map_err(|previous_error| {
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
        self.write_with_fault_and_directory_hook(snapshot, fault, || {})
    }

    fn write_with_fault_and_directory_hook(
        &self,
        snapshot: &mut SessionSnapshot,
        fault: SnapshotFault,
        after_directory_open: impl FnOnce(),
    ) -> anyhow::Result<()> {
        let directory = SnapshotWriteDirectory::open(self)?;
        after_directory_open();
        let (previous_generation, rotate_latest) =
            self.previous_generation_for_write(&directory)?;
        snapshot.version = SESSION_SCHEMA_VERSION;
        snapshot.generation = previous_generation.saturating_add(1);
        let encoded = serde_json::to_vec(snapshot)?;
        anyhow::ensure!(
            encoded.len() as u64 <= MAX_SESSION_SNAPSHOT_BYTES,
            "session snapshot exceeds the {MAX_SESSION_SNAPSHOT_BYTES}-byte recovery limit"
        );
        let temporary_name = Self::temporary_name();
        let mut temporary_created = false;
        let result = (|| -> anyhow::Result<()> {
            let mut temporary = directory.create(&temporary_name)?;
            temporary_created = true;
            temporary.write_all(&encoded)?;
            temporary.sync_all()?;
            if fault == SnapshotFault::AfterTempSync {
                anyhow::bail!("injected snapshot failure after temporary-file sync");
            }

            if rotate_latest {
                directory.remove_if_exists("previous.json")?;
                directory.rename("latest.json", "previous.json")?;
            } else {
                directory.remove_if_exists("latest.json")?;
            }
            if fault == SnapshotFault::AfterRotate {
                anyhow::bail!("injected snapshot failure after generation rotation");
            }
            directory.rename(&temporary_name, "latest.json")?;
            directory.sync()?;
            Ok(())
        })();
        if result.is_err() && temporary_created {
            let _ = directory.remove_if_exists(&temporary_name);
        }
        result
    }

    fn previous_generation_for_write(
        &self,
        directory: &SnapshotWriteDirectory,
    ) -> anyhow::Result<(u64, bool)> {
        let validated = |name: &str| {
            directory.read(name).and_then(|snapshot| {
                validate_snapshot(snapshot)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
            })
        };
        let latest_error = match validated("latest.json") {
            Ok(snapshot) => return Ok((snapshot.generation, true)),
            Err(error)
                if error.kind() == io::ErrorKind::InvalidData
                    || error.kind() == io::ErrorKind::NotFound =>
            {
                error
            }
            Err(error) => return Err(error.into()),
        };

        match validated("previous.json") {
            Ok(snapshot) => Ok((snapshot.generation, false)),
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

#[cfg(unix)]
struct SnapshotWriteDirectory {
    directory: File,
}

#[cfg(unix)]
impl SnapshotWriteDirectory {
    fn open(store: &SessionStore) -> io::Result<Self> {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = if let Some(root) = store.namespace_root.as_deref() {
            let mut directory = open_or_create_session_directory(root)?;
            directory.set_permissions(fs::Permissions::from_mode(0o700))?;
            let relative = store.directory.strip_prefix(root).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "session snapshot directory is outside its namespace root",
                )
            })?;
            for component in relative.components() {
                let Component::Normal(name) = component else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "session snapshot namespace contains an invalid directory component",
                    ));
                };
                directory = open_or_create_session_child(&directory, name)?;
            }
            directory
        } else {
            open_or_create_session_directory(&store.directory)?
        };
        directory.set_permissions(fs::Permissions::from_mode(0o700))?;
        Ok(Self { directory })
    }

    fn read(&self, name: &str) -> io::Result<SessionSnapshot> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};

        use nix::{
            errno::Errno,
            fcntl::{openat, OFlag},
            sys::stat::Mode,
        };

        let descriptor = match openat(
            Some(self.directory.as_raw_fd()),
            name,
            OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
            Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(Errno::ENOENT) => return Err(io::Error::from(io::ErrorKind::NotFound)),
            Err(Errno::ELOOP | Errno::ENOTDIR) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "session snapshot must be a regular file and cannot be a symlink",
                ));
            }
            Err(error) => return Err(io::Error::from_raw_os_error(error as i32)),
        };
        // SAFETY: `openat` returned a new owned descriptor and `File` becomes its sole owner.
        read_snapshot_file(unsafe { File::from_raw_fd(descriptor) })
    }

    fn create(&self, name: &str) -> io::Result<File> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};

        use nix::{
            fcntl::{openat, OFlag},
            sys::stat::Mode,
        };

        let descriptor = openat(
            Some(self.directory.as_raw_fd()),
            name,
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )
        .map_err(|error| io::Error::from_raw_os_error(error as i32))?;
        // SAFETY: `openat` returned a new owned descriptor and `File` becomes its sole owner.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }

    fn remove_if_exists(&self, name: &str) -> io::Result<()> {
        use std::os::fd::AsRawFd as _;

        use nix::{
            errno::Errno,
            unistd::{unlinkat, UnlinkatFlags},
        };

        match unlinkat(
            Some(self.directory.as_raw_fd()),
            name,
            UnlinkatFlags::NoRemoveDir,
        ) {
            Ok(()) | Err(Errno::ENOENT) => Ok(()),
            Err(error) => Err(io::Error::from_raw_os_error(error as i32)),
        }
    }

    fn rename(&self, source: &str, destination: &str) -> io::Result<()> {
        use std::os::fd::AsRawFd as _;

        use nix::fcntl::renameat;

        renameat(
            Some(self.directory.as_raw_fd()),
            source,
            Some(self.directory.as_raw_fd()),
            destination,
        )
        .map_err(|error| io::Error::from_raw_os_error(error as i32))
    }

    fn sync(&self) -> io::Result<()> {
        self.directory.sync_all()
    }
}

#[cfg(not(unix))]
struct SnapshotWriteDirectory {
    directory: PathBuf,
}

#[cfg(not(unix))]
impl SnapshotWriteDirectory {
    fn open(store: &SessionStore) -> io::Result<Self> {
        if let Some(root) = store.namespace_root.as_deref() {
            portable_session_directory(root, /*create*/ true)?;
        }
        portable_session_directory(&store.directory, /*create*/ true)?;
        Ok(Self {
            directory: store.directory.clone(),
        })
    }

    fn read(&self, name: &str) -> io::Result<SessionSnapshot> {
        read_snapshot(&self.directory.join(name))
    }

    fn create(&self, name: &str) -> io::Result<File> {
        portable_session_directory(&self.directory, /*create*/ false)?;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.directory.join(name))
    }

    fn remove_if_exists(&self, name: &str) -> io::Result<()> {
        portable_session_directory(&self.directory, /*create*/ false)?;
        let path = self.directory.join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_dir() => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "session snapshot file cannot be a directory or directory reparse point: {}",
                    path.display()
                ),
            )),
            Ok(metadata)
                if metadata.file_type().is_file()
                    || metadata.file_type().is_symlink()
                    || portable_session_reparse_point(&metadata) =>
            {
                fs::remove_file(path)
            }
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "session snapshot file must be regular or an unlinkable file reparse point: {}",
                    path.display()
                ),
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn rename(&self, source: &str, destination: &str) -> io::Result<()> {
        portable_session_directory(&self.directory, /*create*/ false)?;
        let source = self.directory.join(source);
        let destination = self.directory.join(destination);
        portable_session_file_metadata(&source)?;
        match portable_session_file_metadata(&destination) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        fs::rename(source, destination)
    }

    fn sync(&self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(not(unix))]
fn portable_session_directory(path: &Path, create: bool) -> io::Result<()> {
    use std::path::Component;

    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                current.pop();
            }
            Component::Normal(name) => {
                current.push(name);
                let metadata = match fs::symlink_metadata(&current) {
                    Ok(metadata) => metadata,
                    Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                        match fs::create_dir(&current) {
                            Ok(()) => {}
                            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                            Err(error) => return Err(error),
                        }
                        fs::symlink_metadata(&current)?
                    }
                    Err(error) => return Err(error),
                };
                if metadata.file_type().is_symlink()
                    || portable_session_reparse_point(&metadata)
                    || !metadata.file_type().is_dir()
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "session snapshot directory cannot contain a symlink, reparse point, or non-directory component: {}",
                            current.display()
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn portable_session_file_metadata(path: &Path) -> io::Result<fs::Metadata> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "session snapshot file must be below a directory",
        )
    })?;
    portable_session_directory(parent, /*create*/ false)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || portable_session_reparse_point(&metadata)
        || !metadata.file_type().is_file()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "session snapshot file must be regular and cannot be a symlink or reparse point: {}",
                path.display()
            ),
        ));
    }
    Ok(metadata)
}

#[cfg(windows)]
fn portable_session_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(all(not(unix), not(windows)))]
fn portable_session_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn open_or_create_session_directory(path: &Path) -> io::Result<File> {
    open_or_create_session_directory_with_component_hook(path, |_, _| {})
}

#[cfg(unix)]
fn open_or_create_session_directory_with_component_hook(
    path: &Path,
    mut before_component: impl FnMut(bool, usize),
) -> io::Result<File> {
    let path = normalized_session_disk_path(path)?;
    let components = path
        .components()
        .filter(|component| !matches!(component, Component::RootDir | Component::CurDir))
        .collect::<Vec<_>>();
    let mut depth = 0usize;
    let component_depths = components
        .iter()
        .map(|component| {
            match component {
                Component::Normal(_) => depth = depth.saturating_add(1),
                Component::ParentDir => depth = depth.saturating_sub(1),
                Component::RootDir | Component::CurDir | Component::Prefix(_) => unreachable!(),
            }
            depth
        })
        .collect::<Vec<_>>();
    let mut future_minimum_depths = vec![depth; components.len() + 1];
    for index in (0..components.len()).rev() {
        future_minimum_depths[index] =
            future_minimum_depths[index + 1].min(component_depths[index]);
    }
    let mut directories = vec![File::open("/")?];
    for (index, component) in components.iter().enumerate() {
        match component {
            Component::ParentDir => {
                before_component(/*is_parent*/ true, directories.len());
                if directories.len() > 1 {
                    directories.pop();
                }
            }
            Component::Normal(name) => {
                before_component(/*is_parent*/ false, directories.len());
                let parent = directories.last().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "session snapshot directory stack is empty",
                    )
                })?;
                directories.push(open_or_create_session_child(parent, name)?);
            }
            Component::RootDir | Component::CurDir | Component::Prefix(_) => unreachable!(),
        }
        retain_required_session_directories(
            &mut directories,
            component_depths[index],
            future_minimum_depths[index + 1],
        );
    }
    directories.pop().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "session snapshot directory stack is empty",
        )
    })
}

#[cfg(unix)]
fn open_or_create_session_child(parent: &File, name: &std::ffi::OsStr) -> io::Result<File> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    use nix::{
        errno::Errno,
        fcntl::{openat, OFlag},
        sys::stat::{mkdirat, Mode},
    };

    let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW;
    let descriptor = match openat(Some(parent.as_raw_fd()), name, flags, Mode::empty()) {
        Ok(descriptor) => descriptor,
        Err(Errno::ENOENT) => {
            match mkdirat(
                Some(parent.as_raw_fd()),
                name,
                Mode::from_bits_truncate(0o700),
            ) {
                Ok(()) | Err(Errno::EEXIST) => {}
                Err(error) => return Err(io::Error::from_raw_os_error(error as i32)),
            }
            openat(Some(parent.as_raw_fd()), name, flags, Mode::empty())
                .map_err(|error| io::Error::from_raw_os_error(error as i32))?
        }
        Err(Errno::ELOOP | Errno::ENOTDIR) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "session snapshot directory must not be a symlink or contain a non-directory component",
            ));
        }
        Err(error) => return Err(io::Error::from_raw_os_error(error as i32)),
    };
    // SAFETY: `openat` returned a new owned descriptor and `File` becomes its sole owner.
    Ok(unsafe { File::from_raw_fd(descriptor) })
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

#[cfg(unix)]
pub(crate) fn capture_session_disk_fingerprint(
    path: &Path,
) -> io::Result<Option<SessionDiskFingerprint>> {
    let Some(file) = open_session_disk_file(path)? else {
        return Ok(None);
    };
    session_disk_fingerprint(&file.metadata()?).map(Some)
}

#[cfg(not(unix))]
pub(crate) fn capture_session_disk_fingerprint(
    path: &Path,
) -> io::Result<Option<SessionDiskFingerprint>> {
    match portable_session_file_metadata(path) {
        Ok(metadata) => session_disk_fingerprint(&metadata).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
pub(crate) fn read_session_disk_contents(
    path: &Path,
    expected: SessionDiskFingerprint,
) -> io::Result<String> {
    let file = open_session_disk_file(path)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "session disk base was removed before snapshot read",
        )
    })?;
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

#[cfg(not(unix))]
pub(crate) fn read_session_disk_contents(
    path: &Path,
    expected: SessionDiskFingerprint,
) -> io::Result<String> {
    portable_session_file_metadata(path)?;
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
    open_session_disk_file_with_component_hook(path, |_, _| {})
}

#[cfg(unix)]
fn open_session_disk_file_with_component_hook(
    path: &Path,
    mut before_component: impl FnMut(bool, usize),
) -> io::Result<Option<File>> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    use nix::{
        errno::Errno,
        fcntl::{openat, OFlag},
        sys::stat::Mode,
    };

    let path = normalized_session_disk_path(path)?;
    let components = path
        .components()
        .filter(|component| !matches!(component, Component::RootDir | Component::CurDir))
        .collect::<Vec<_>>();
    if components.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session disk base must be a regular file below the filesystem root",
        ));
    }
    let mut depth = 0usize;
    let component_depths = components
        .iter()
        .map(|component| {
            match component {
                Component::Normal(_) => depth = depth.saturating_add(1),
                Component::ParentDir => depth = depth.saturating_sub(1),
                Component::RootDir | Component::CurDir | Component::Prefix(_) => unreachable!(),
            }
            depth
        })
        .collect::<Vec<_>>();
    let mut future_minimum_depths = vec![depth; components.len() + 1];
    for index in (0..components.len()).rev() {
        future_minimum_depths[index] =
            future_minimum_depths[index + 1].min(component_depths[index]);
    }
    let mut directories = vec![File::open("/")?];
    for (index, component) in components.iter().enumerate() {
        let final_component = index + 1 == components.len();
        let name = match component {
            Component::Normal(name) => name,
            Component::ParentDir => {
                if final_component {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "session disk base must be a regular file",
                    ));
                }
                before_component(/*is_parent*/ true, directories.len());
                if directories.len() > 1 {
                    directories.pop();
                }
                retain_required_session_directories(
                    &mut directories,
                    component_depths[index],
                    future_minimum_depths[index + 1],
                );
                continue;
            }
            Component::RootDir | Component::CurDir | Component::Prefix(_) => unreachable!(),
        };
        before_component(/*is_parent*/ false, directories.len());
        let mut flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK;
        if !final_component {
            flags |= OFlag::O_DIRECTORY;
        }
        let directory = directories.last().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "session disk directory stack is empty",
            )
        })?;
        let descriptor = match openat(Some(directory.as_raw_fd()), *name, flags, Mode::empty()) {
            Ok(descriptor) => descriptor,
            Err(Errno::ENOENT) => return Ok(None),
            Err(error) => return Err(io::Error::from_raw_os_error(error as i32)),
        };
        // SAFETY: `openat` returned a new owned descriptor and `File` becomes its sole owner.
        let file = unsafe { File::from_raw_fd(descriptor) };
        if final_component {
            return Ok(Some(file));
        }
        directories.push(file);
        retain_required_session_directories(
            &mut directories,
            component_depths[index],
            future_minimum_depths[index + 1],
        );
    }
    Ok(None)
}

#[cfg(unix)]
fn retain_required_session_directories(
    directories: &mut Vec<File>,
    current_depth: usize,
    future_minimum_depth: usize,
) {
    let keep = current_depth
        .saturating_sub(future_minimum_depth.min(current_depth))
        .saturating_add(1);
    let discard = directories.len().saturating_sub(keep);
    directories.drain(..discard);
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
    for buffer in &snapshot.buffers {
        buffer.undo_history.validate().map_err(|error| {
            anyhow::anyhow!(
                "session snapshot buffer {} contains an invalid undo tree: {error}",
                buffer.index
            )
        })?;
    }
    // Versions in the supported range use serde defaults as their migration path.
    snapshot.version = SESSION_SCHEMA_VERSION;
    Ok(snapshot)
}

#[cfg(unix)]
fn read_snapshot(path: &Path) -> io::Result<SessionSnapshot> {
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
    read_snapshot_file(file)
}

#[cfg(not(unix))]
fn read_snapshot(path: &Path) -> io::Result<SessionSnapshot> {
    portable_session_file_metadata(path)?;
    read_snapshot_file(OpenOptions::new().read(true).open(path)?)
}

fn read_snapshot_file(file: File) -> io::Result<SessionSnapshot> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_workspace::ProposalWorkspace;
    use crate::window::{SplitSnapshot, WindowManagerSnapshot};

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
    fn corrupt_undo_tree_indices_fall_back_to_the_previous_snapshot() {
        use crate::undo::{CursorSnapshot, TextRange};

        for corruption in [
            "current",
            "parent",
            "child",
            "root",
            "branch",
            "duplicate",
            "line",
            "column",
            "revision",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let store = SessionStore::new(directory.path());
            store.write(&mut snapshot("known good")).unwrap();
            let mut latest = snapshot("corrupt latest");
            let history = &mut latest.buffers[0].undo_history;
            history.begin_transaction("insert", CursorSnapshot::default());
            history.record_replace(
                TextRange::insertion(TextPosition::default()),
                /*start_char*/ 0,
                String::new(),
                "x".to_string(),
            );
            history.commit_transaction(CursorSnapshot::default());
            history.begin_transaction("insert", CursorSnapshot::default());
            history.record_replace(
                TextRange::insertion(TextPosition::new(/*line*/ 0, /*character*/ 1)),
                /*start_char*/ 1,
                String::new(),
                "y".to_string(),
            );
            history.commit_transaction(CursorSnapshot::default());
            store.write(&mut latest).unwrap();
            let mut encoded: serde_json::Value =
                serde_json::from_slice(&fs::read(store.latest_path()).unwrap()).unwrap();
            let history = &mut encoded["buffers"][0]["undo_history"];
            match corruption {
                "current" => history["current"] = serde_json::json!(999),
                "parent" => history["nodes"][1]["parent"] = serde_json::json!(999),
                "child" => history["nodes"][0]["children"] = serde_json::json!([0]),
                "root" => history["root_children"] = serde_json::json!([999]),
                "branch" => {
                    history["branch_selection"] =
                        serde_json::json!({ (usize::MAX.to_string()): 999 });
                }
                "duplicate" => {
                    history["nodes"][1]["transaction"]["id"] =
                        history["nodes"][0]["transaction"]["id"].clone();
                }
                "line" => {
                    history["nodes"][0]["transaction"]["edits"][0]["range"] = serde_json::json!({
                        "start": { "line": usize::MAX, "character": 0 },
                        "end": { "line": usize::MAX, "character": 0 }
                    });
                    history["nodes"][0]["transaction"]["edits"][0]["new_text"] =
                        serde_json::json!("\n");
                }
                "column" => {
                    history["nodes"][0]["transaction"]["edits"][0]["range"] = serde_json::json!({
                        "start": { "line": 0, "character": usize::MAX },
                        "end": { "line": 0, "character": usize::MAX }
                    });
                }
                "revision" => history["next_revision"] = serde_json::json!(u64::MAX),
                _ => unreachable!(),
            }
            fs::write(store.latest_path(), serde_json::to_vec(&encoded).unwrap()).unwrap();

            let recovered = store.load().unwrap();
            let latest_recovered = SessionStore::load_latest(directory.path()).unwrap();

            assert_eq!(recovered.buffers[0].contents, "known good", "{corruption}");
            assert_eq!(
                latest_recovered.buffers[0].contents, "known good",
                "{corruption}"
            );

            store.write(&mut snapshot("replacement")).unwrap();
            assert_eq!(
                store.load().unwrap().buffers[0].contents,
                "replacement",
                "{corruption}"
            );
        }
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
    fn refuses_to_write_through_a_symlinked_snapshot_ancestor() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        fs::create_dir(&target).unwrap();
        let link = directory.path().join("linked");
        symlink(&target, &link).unwrap();
        let store = SessionStore::new(link.join("sessions/editor-one"));
        let mut value = snapshot("private");

        let error = store.write(&mut value).unwrap_err().to_string();

        assert!(error.contains("must not be a symlink"), "{error}");
        assert!(!target.join("sessions").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_replaced_snapshot_ancestor_cannot_redirect_rotation_or_temporary_cleanup() {
        use std::os::unix::fs::symlink;

        for fault in [
            SnapshotFault::None,
            SnapshotFault::AfterTempSync,
            SnapshotFault::AfterRotate,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().join("sessions");
            let moved_root = directory.path().join("original-sessions");
            let outside = directory.path().join("outside");
            let outside_owner = outside.join("editor-one");
            fs::create_dir_all(&outside_owner).unwrap();
            fs::write(outside_owner.join("latest.json"), b"outside latest").unwrap();
            fs::write(outside_owner.join("previous.json"), b"outside previous").unwrap();
            let store = SessionStore::for_owner(&root, "editor-one").unwrap();
            store.write(&mut snapshot("first")).unwrap();
            store.write(&mut snapshot("second")).unwrap();
            let mut third = snapshot("third");

            let result = store.write_with_fault_and_directory_hook(&mut third, fault, || {
                fs::rename(&root, &moved_root).unwrap();
                symlink(&outside, &root).unwrap();
            });

            assert_eq!(result.is_ok(), fault == SnapshotFault::None, "{fault:?}");
            assert_eq!(
                fs::read(outside_owner.join("latest.json")).unwrap(),
                b"outside latest",
                "{fault:?}"
            );
            assert_eq!(
                fs::read(outside_owner.join("previous.json")).unwrap(),
                b"outside previous",
                "{fault:?}"
            );
            let original_owner = moved_root.join("editor-one");
            let temporary_files = fs::read_dir(&original_owner)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .filter(|name| name.to_string_lossy().ends_with(".tmp"))
                .collect::<Vec<_>>();
            assert!(temporary_files.is_empty(), "{fault:?}");
            match fault {
                SnapshotFault::None => {
                    assert_eq!(
                        read_snapshot(&original_owner.join("latest.json"))
                            .unwrap()
                            .buffers[0]
                            .contents,
                        "third"
                    );
                    assert_eq!(
                        read_snapshot(&original_owner.join("previous.json"))
                            .unwrap()
                            .buffers[0]
                            .contents,
                        "second"
                    );
                }
                SnapshotFault::AfterTempSync => {
                    assert_eq!(
                        read_snapshot(&original_owner.join("latest.json"))
                            .unwrap()
                            .buffers[0]
                            .contents,
                        "second"
                    );
                    assert_eq!(
                        read_snapshot(&original_owner.join("previous.json"))
                            .unwrap()
                            .buffers[0]
                            .contents,
                        "first"
                    );
                }
                SnapshotFault::AfterRotate => {
                    assert!(!original_owner.join("latest.json").exists());
                    assert_eq!(
                        read_snapshot(&original_owner.join("previous.json"))
                            .unwrap()
                            .buffers[0]
                            .contents,
                        "second"
                    );
                }
            }
        }
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

    #[cfg(windows)]
    #[test]
    fn portable_snapshot_replaces_a_file_symlink_but_rejects_a_directory_symlink() {
        use std::os::windows::fs::{symlink_dir, symlink_file};

        for source in ["file", "directory"] {
            let directory = tempfile::tempdir().unwrap();
            let store = SessionStore::new(directory.path());
            let latest = store.latest_path();
            let outside = directory.path().join("outside");
            let link = match source {
                "file" => {
                    fs::write(&outside, b"outside secret").unwrap();
                    symlink_file(&outside, &latest)
                }
                "directory" => {
                    fs::create_dir(&outside).unwrap();
                    fs::write(outside.join("secret"), b"outside secret").unwrap();
                    symlink_dir(&outside, &latest)
                }
                _ => unreachable!(),
            };
            if let Err(error) = link {
                assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
                return;
            }
            fs::write(
                store.previous_path(),
                serde_json::to_vec(&snapshot("known good")).unwrap(),
            )
            .unwrap();
            assert_eq!(store.load().unwrap().buffers[0].contents, "known good");

            let mut replacement = snapshot("replacement");
            let write = store.write(&mut replacement);
            if source == "file" {
                write.unwrap();
                assert_eq!(store.load().unwrap().buffers[0].contents, "replacement");
                assert_eq!(fs::read(&outside).unwrap(), b"outside secret");
            } else {
                let error = write.unwrap_err().to_string();
                assert!(error.contains("directory reparse point"), "{error}");
                assert_eq!(fs::read(outside.join("secret")).unwrap(), b"outside secret");
            }
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
        let write_directory =
            SnapshotWriteDirectory::open(&SessionStore::new(directory.path())).unwrap();
        assert_eq!(
            write_directory.create("existing.tmp").unwrap_err().kind(),
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
        let mut renamed = false;
        let mut file = open_session_disk_file_with_component_hook(&safe_path, |is_parent, _| {
            if is_parent {
                fs::rename(workspace.join("child"), outside.join("moved-child")).unwrap();
                renamed = true;
            }
        })
        .unwrap()
        .unwrap();
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();
        assert!(renamed);
        assert_eq!(contents, "workspace base\n");

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

    #[cfg(unix)]
    #[test]
    fn deeply_nested_snapshot_paths_keep_one_directory_descriptor() {
        let directory = tempfile::tempdir().unwrap();
        let mut parent = directory.path().to_path_buf();
        for index in 0..96 {
            parent.push(format!("d{index}"));
            fs::create_dir(&parent).unwrap();
        }
        let path = parent.join("buffer.txt");
        fs::write(&path, "deep base\n").unwrap();
        let mut maximum_directories = 0;

        let mut file = open_session_disk_file_with_component_hook(&path, |is_parent, held| {
            assert!(!is_parent);
            maximum_directories = maximum_directories.max(held);
        })
        .unwrap()
        .unwrap();
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();

        assert_eq!(maximum_directories, 1);
        assert_eq!(contents, "deep base\n");

        fs::create_dir(parent.join("child")).unwrap();
        let parent_path = parent.join("child/../buffer.txt");
        maximum_directories = 0;
        let mut file = open_session_disk_file_with_component_hook(&parent_path, |_, held| {
            maximum_directories = maximum_directories.max(held);
        })
        .unwrap()
        .unwrap();
        contents.clear();
        file.read_to_string(&mut contents).unwrap();

        assert_eq!(maximum_directories, 2);
        assert_eq!(contents, "deep base\n");
    }

    #[cfg(unix)]
    #[test]
    fn deeply_nested_snapshot_writes_keep_one_directory_descriptor() {
        let directory = tempfile::tempdir().unwrap();
        let mut parent = directory.path().to_path_buf();
        for index in 0..96 {
            parent.push(format!("d{index}"));
        }
        let mut maximum_directories = 0;

        let opened =
            open_or_create_session_directory_with_component_hook(&parent, |is_parent, held| {
                assert!(!is_parent);
                maximum_directories = maximum_directories.max(held);
            })
            .unwrap();
        drop(opened);

        assert_eq!(maximum_directories, 1);
        let store = SessionStore::new(&parent);
        store.write(&mut snapshot("deep snapshot")).unwrap();
        assert_eq!(store.load().unwrap().buffers[0].contents, "deep snapshot");

        let parent_path = parent.join("child/..");
        maximum_directories = 0;
        let opened =
            open_or_create_session_directory_with_component_hook(&parent_path, |_, held| {
                maximum_directories = maximum_directories.max(held);
            })
            .unwrap();
        drop(opened);

        assert_eq!(maximum_directories, 2);
        let store = SessionStore::new(&parent_path);
        store.write(&mut snapshot("parent snapshot")).unwrap();
        assert_eq!(
            SessionStore::new(&parent).load().unwrap().buffers[0].contents,
            "parent snapshot"
        );
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

    #[cfg(not(unix))]
    #[test]
    fn portable_snapshot_paths_reject_non_directory_ancestors_and_non_regular_files() {
        let directory = tempfile::tempdir().unwrap();
        let blocked = directory.path().join("blocked");
        fs::write(&blocked, "not a directory").unwrap();
        let store = SessionStore::for_owner(&blocked, "editor-one").unwrap();
        let mut value = snapshot("private buffer");

        let write_error = store.write(&mut value).unwrap_err().to_string();

        assert!(
            write_error.contains("non-directory component"),
            "{write_error}"
        );
        assert_eq!(fs::read_to_string(&blocked).unwrap(), "not a directory");

        let root = directory.path().join("sessions");
        fs::create_dir_all(root.join("editor-one/latest.json")).unwrap();
        let store = SessionStore::for_owner(&root, "editor-one").unwrap();
        let load_error = store.load().unwrap_err().to_string();
        assert!(load_error.contains("must be regular"), "{load_error}");

        let backing = directory.path().join("backing-directory");
        fs::create_dir(&backing).unwrap();
        let read_error = capture_session_disk_fingerprint(&backing).unwrap_err();
        assert_eq!(read_error.kind(), io::ErrorKind::InvalidData);
        assert!(read_error.to_string().contains("must be regular"));

        value.buffers[0].path = Some(backing.to_string_lossy().into_owned());
        value.buffers[0].disk_contents = Some("trusted base\n".to_string());
        let divergences = detect_disk_divergence(&value);
        assert_eq!(divergences.len(), 1);
        assert!(divergences[0]
            .diff
            .contains("current disk could not be read safely"));
    }

    #[test]
    fn owner_namespaces_reject_traversal() {
        let directory = tempfile::tempdir().unwrap();

        assert!(SessionStore::for_owner(directory.path(), "../outside").is_err());
        assert!(SessionStore::for_owner(directory.path(), ".").is_err());
        assert!(SessionStore::for_owner(directory.path(), "..").is_err());
    }
}
