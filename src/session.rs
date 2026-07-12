//! Crash-safe, core-owned editor session snapshots.

use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};

use crate::{
    agent_workspace::ProposalWorkspaceSnapshot,
    editor::Content,
    undo::{TextPosition, UndoHistory},
    window::WindowManagerSnapshot,
};

pub const SESSION_SCHEMA_VERSION: u32 = 2;
static NEXT_TEMPORARY_SNAPSHOT_ID: AtomicU64 = AtomicU64::new(1);

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
        Ok(Self::new(directory.join(owner)))
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
                        stores.push(Self::new(entry.path()));
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
            let actual = fs::read_to_string(path).ok();
            (actual.as_deref() != expected).then(|| RecoveryDivergence {
                path: path.to_string(),
                diff: similar::TextDiff::from_lines(
                    expected.unwrap_or_default(),
                    actual.as_deref().unwrap_or_default(),
                )
                .unified_diff()
                .header("snapshot disk base", "current disk")
                .to_string(),
            })
        })
        .collect()
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
    let contents = fs::read(path)?;
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
