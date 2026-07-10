//! Crash-safe, core-owned editor session snapshots.

use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Write as _},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    agent_workspace::ProposalWorkspaceSnapshot,
    editor::Content,
    undo::{TextPosition, UndoHistory},
    window::WindowManagerSnapshot,
};

pub const SESSION_SCHEMA_VERSION: u32 = 2;

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

    #[must_use]
    pub fn latest_path(&self) -> PathBuf {
        self.directory.join("latest.json")
    }

    fn previous_path(&self) -> PathBuf {
        self.directory.join("previous.json")
    }

    fn temporary_path(&self) -> PathBuf {
        self.directory.join("snapshot.tmp")
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
        set_directory_permissions(&self.directory)?;

        let previous_generation = self
            .load()
            .map(|saved| saved.generation)
            .unwrap_or_default();
        snapshot.version = SESSION_SCHEMA_VERSION;
        snapshot.generation = previous_generation.saturating_add(1);
        let encoded = serde_json::to_vec(snapshot)?;
        let temporary_path = self.temporary_path();
        let mut temporary = restrictive_file(&temporary_path)?;
        temporary.write_all(&encoded)?;
        temporary.sync_all()?;
        if fault == SnapshotFault::AfterTempSync {
            anyhow::bail!("injected snapshot failure after temporary-file sync");
        }

        let latest_path = self.latest_path();
        let previous_path = self.previous_path();
        if latest_path.exists() {
            if previous_path.exists() {
                fs::remove_file(&previous_path)?;
            }
            fs::rename(&latest_path, &previous_path)?;
        }
        if fault == SnapshotFault::AfterRotate {
            anyhow::bail!("injected snapshot failure after generation rotation");
        }
        fs::rename(&temporary_path, &latest_path)?;
        sync_directory(&self.directory)?;
        Ok(())
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
    options.write(true).create(true).truncate(true);
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
    }
}
