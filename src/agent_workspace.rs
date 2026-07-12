//! Session-scoped, review-before-apply filesystem for ACP agents.
//!
//! Agent writes update proposed contents only. Visible buffers and disk are never
//! mutated by this module; callers must explicitly accept a proposal and route the
//! returned contents through the editor's transaction boundary.

use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

#[cfg(unix)]
use std::io::Read as _;

use agent_client_protocol_schema::v1::{
    ReadTextFileRequest, ReadTextFileResponse, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SessionNotification, WriteTextFileRequest, WriteTextFileResponse,
};
use async_trait::async_trait;
use path_absolutize::Absolutize as _;
use serde::{Deserialize, Serialize};
use similar::{DiffTag, TextDiff};
use uuid::Uuid;

use crate::acp::{AcpHost, MAX_MESSAGE_BYTES};

const MAX_PROPOSAL_CONTENT_BYTES: usize = MAX_MESSAGE_BYTES - 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct VisibleFile {
    revision: u64,
    contents: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ProposedFile {
    base_revision: u64,
    base_contents: String,
    proposed_contents: String,
    created: bool,
    turn_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AgentSession {
    files: HashMap<PathBuf, ProposedFile>,
    current_turn: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TextChange {
    base_start: usize,
    base_end: usize,
    replacement: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProposalHunk {
    pub id: String,
    pub path: PathBuf,
    pub old_start: usize,
    pub old_end: usize,
    pub old_text: String,
    pub new_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProposalDisposition {
    Applied {
        path: PathBuf,
        contents: String,
        current_contents: String,
        base_revision: u64,
        session_id: String,
        turn_id: String,
        created: bool,
    },
    Conflict {
        path: PathBuf,
        base: String,
        current: String,
        proposed: String,
    },
    NoChanges,
}

#[derive(Debug, Clone)]
pub struct StagedProposalAcceptance {
    disposition: ProposalDisposition,
    session_id: String,
    path: PathBuf,
    expected: ProposedFile,
    updated: ProposedFile,
}

impl StagedProposalAcceptance {
    #[must_use]
    pub fn disposition(&self) -> &ProposalDisposition {
        &self.disposition
    }
}

/// Shared proposal state for all live ACP sessions.
#[derive(Debug)]
pub struct ProposalWorkspace {
    root: PathBuf,
    #[cfg(unix)]
    root_directory: Option<std::fs::File>,
    visible: HashMap<PathBuf, VisibleFile>,
    sessions: HashMap<String, AgentSession>,
    recovered_sessions: HashSet<String>,
    generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalWorkspaceSnapshot {
    root: PathBuf,
    visible: HashMap<PathBuf, VisibleFile>,
    sessions: HashMap<String, AgentSession>,
}

impl ProposalWorkspaceSnapshot {
    #[must_use]
    pub(crate) fn has_pending_files(&self) -> bool {
        self.sessions.values().any(|session| {
            session
                .files
                .values()
                .any(|proposal| proposal.base_contents != proposal.proposed_contents)
        })
    }
}

impl ProposalWorkspace {
    /// Create a workspace rooted at an existing or prospective directory.
    pub fn new(root: impl AsRef<Path>) -> anyhow::Result<Self> {
        let root = root.as_ref().absolutize()?.to_path_buf();
        Ok(Self {
            #[cfg(unix)]
            root_directory: open_workspace_root(&root),
            root,
            visible: HashMap::new(),
            sessions: HashMap::new(),
            recovered_sessions: HashSet::new(),
            generation: 0,
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn snapshot(&self) -> ProposalWorkspaceSnapshot {
        ProposalWorkspaceSnapshot {
            root: self.root.clone(),
            visible: self.visible.clone(),
            sessions: self.sessions.clone(),
        }
    }

    #[must_use]
    pub fn from_snapshot(snapshot: ProposalWorkspaceSnapshot) -> Self {
        let recovered_sessions = snapshot.sessions.keys().cloned().collect();
        Self {
            #[cfg(unix)]
            root_directory: open_workspace_root(&snapshot.root),
            root: snapshot.root,
            visible: snapshot.visible,
            sessions: snapshot.sessions,
            recovered_sessions,
            generation: 0,
        }
    }

    /// Publish the latest user-visible buffer contents. Existing proposal bases remain
    /// stable; a later review observes this value as user divergence.
    pub fn sync_visible_file(
        &mut self,
        path: impl AsRef<Path>,
        revision: u64,
        contents: String,
    ) -> anyhow::Result<()> {
        self.ensure_root_is_current()?;
        let path = self.normalize_path(path.as_ref())?;
        let visible = VisibleFile { revision, contents };
        if self.visible.get(&path) != Some(&visible) {
            self.visible.insert(path, visible);
            self.bump_generation();
        }
        Ok(())
    }

    /// Replace the complete set of editor-visible files, dropping closed buffers and
    /// skipping paths that cannot be shared safely with the agent.
    pub fn replace_visible_files(
        &mut self,
        files: impl IntoIterator<Item = (PathBuf, u64, String)>,
    ) -> anyhow::Result<usize> {
        self.ensure_root_is_current()?;
        let mut visible = HashMap::new();
        let mut skipped = 0;
        for (path, revision, contents) in files {
            let Ok(path) = self.normalize_path(&path) else {
                skipped += 1;
                continue;
            };
            visible.insert(path, VisibleFile { revision, contents });
        }
        if self.visible != visible {
            self.visible = visible;
            self.bump_generation();
        }
        Ok(skipped)
    }

    /// Verify that the lexical workspace root still names the pinned directory before
    /// exposing any path that could later be saved through the editor.
    pub fn ensure_root_is_current(&self) -> anyhow::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            let pinned = self.root_directory.as_ref().ok_or_else(|| {
                anyhow::anyhow!("ACP proposal workspace root cannot be opened safely")
            })?;
            let current = open_workspace_root(&self.root).ok_or_else(|| {
                anyhow::anyhow!("ACP proposal workspace root cannot be opened safely")
            })?;
            let pinned = pinned.metadata()?;
            let current = current.metadata()?;
            anyhow::ensure!(
                pinned.dev() == current.dev() && pinned.ino() == current.ino(),
                "ACP proposal workspace root changed after the session was created"
            );
        }
        #[cfg(not(unix))]
        {
            let metadata = std::fs::symlink_metadata(&self.root)?;
            anyhow::ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "ACP proposal workspace root is not a safe directory"
            );
        }
        Ok(())
    }

    pub fn read(
        &mut self,
        session_id: &str,
        path: &Path,
        line: Option<u32>,
        limit: Option<u32>,
    ) -> anyhow::Result<String> {
        let path = self.normalize_path(path)?;
        let proposal = self.ensure_proposal(session_id, &path)?;
        Ok(slice_lines(&proposal.proposed_contents, line, limit))
    }

    pub fn write(&mut self, session_id: &str, path: &Path, contents: String) -> anyhow::Result<()> {
        let path = self.normalize_path(path)?;
        anyhow::ensure!(
            contents.len() <= MAX_PROPOSAL_CONTENT_BYTES,
            "ACP proposal contents exceed {MAX_PROPOSAL_CONTENT_BYTES} bytes"
        );
        let turn_id = self
            .sessions
            .get(session_id)
            .and_then(|session| session.current_turn.clone())
            .unwrap_or_else(|| "unattributed".to_string());
        let changed = {
            let proposal = self.ensure_proposal(session_id, &path)?;
            let changed = proposal.proposed_contents != contents
                || proposal.turn_id.as_deref() != Some(turn_id.as_str());
            if changed {
                proposal.proposed_contents = contents;
                proposal.turn_id = Some(turn_id);
            }
            changed
        };
        if changed {
            self.bump_generation();
        }
        Ok(())
    }

    pub fn begin_turn(&mut self, session_id: &str, turn_id: String) {
        self.adopt_recovered_sessions(session_id);
        let session = self.sessions.entry(session_id.to_string()).or_default();
        if session.current_turn.as_deref() != Some(turn_id.as_str()) {
            session.current_turn = Some(turn_id);
            self.bump_generation();
        }
    }

    pub fn close_session(&mut self, session_id: &str) {
        let changed = self.sessions.remove(session_id).is_some();
        self.recovered_sessions.remove(session_id);
        if changed {
            self.bump_generation();
        }
    }

    /// Retain reviewable proposals when their ACP session closes; discard empty sessions.
    pub fn archive_session(&mut self, session_id: &str) {
        let Some(session) = self.sessions.get_mut(session_id) else {
            self.recovered_sessions.remove(session_id);
            return;
        };
        if session
            .files
            .values()
            .all(|proposal| proposal.base_contents == proposal.proposed_contents)
        {
            self.close_session(session_id);
            return;
        }
        let changed = session.current_turn.take().is_some();
        self.recovered_sessions.insert(session_id.to_string());
        if changed {
            self.bump_generation();
        }
    }

    /// Return the active session and every archived session with reviewable changes.
    #[must_use]
    pub fn review_sessions(&self, session_id: &str) -> Vec<String> {
        let mut sessions = self
            .recovered_sessions
            .iter()
            .filter(|recovered| recovered.as_str() != session_id)
            .filter(|recovered| !self.pending_files(recovered).is_empty())
            .cloned()
            .collect::<Vec<_>>();
        sessions.sort();
        if !session_id.is_empty() {
            sessions.insert(0, session_id.to_string());
        }
        sessions
    }

    /// Attach non-conflicting archived proposals to a replacement ACP session.
    ///
    /// Archived sessions that overlap an existing proposal remain independently reviewable.
    pub fn adopt_recovered_sessions(&mut self, session_id: &str) -> usize {
        if session_id.is_empty() {
            return 0;
        }
        let mut recovered = self.recovered_sessions.iter().cloned().collect::<Vec<_>>();
        recovered.sort();
        let mut adopted = 0;
        for recovered_id in recovered {
            if recovered_id == session_id {
                self.recovered_sessions.remove(&recovered_id);
                continue;
            }
            let overlaps = self
                .sessions
                .get(session_id)
                .zip(self.sessions.get(&recovered_id))
                .is_some_and(|(target, source)| {
                    source
                        .files
                        .keys()
                        .any(|path| target.files.contains_key(path))
                });
            if overlaps {
                continue;
            }
            let Some(source) = self.sessions.remove(&recovered_id) else {
                self.recovered_sessions.remove(&recovered_id);
                continue;
            };
            self.sessions
                .entry(session_id.to_string())
                .or_default()
                .files
                .extend(source.files);
            self.recovered_sessions.remove(&recovered_id);
            adopted += 1;
        }
        if adopted > 0 {
            self.bump_generation();
        }
        adopted
    }

    /// Read the current on-disk contents of an unopened proposal file without following
    /// symlinks, blocking on special files, or exceeding the ACP content bound.
    pub fn read_current_file(&self, path: &Path) -> anyhow::Result<Option<String>> {
        let path = self.normalize_path(path)?;
        read_bounded_file(self, &path)
    }

    pub fn pending_files(&self, session_id: &str) -> Vec<PathBuf> {
        let mut files = self
            .sessions
            .get(session_id)
            .into_iter()
            .flat_map(|session| &session.files)
            .filter(|(_, proposal)| proposal.base_contents != proposal.proposed_contents)
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        files.sort();
        files
    }

    pub fn hunks(
        &self,
        session_id: &str,
        path: &Path,
        current_contents: &str,
    ) -> anyhow::Result<Vec<ProposalHunk>> {
        let path = self.normalize_path(path)?;
        let proposal = self.proposal(session_id, &path)?;
        let rebased = rebase_contents(
            &proposal.base_contents,
            current_contents,
            &proposal.proposed_contents,
        )
        .ok_or_else(|| anyhow::anyhow!("proposal conflicts with current buffer"))?;
        Ok(changes_between(current_contents, &rebased)
            .into_iter()
            .map(|change| ProposalHunk {
                id: hunk_id(&path, &change),
                path: path.clone(),
                old_start: change.base_start,
                old_end: change.base_end,
                old_text: char_slice(current_contents, change.base_start, change.base_end),
                new_text: change.replacement,
            })
            .collect())
    }

    pub fn accept_all(
        &mut self,
        session_id: &str,
        path: &Path,
        current_revision: u64,
        current_contents: &str,
    ) -> anyhow::Result<ProposalDisposition> {
        let acceptance =
            self.stage_accept_all(session_id, path, current_revision, current_contents)?;
        let disposition = acceptance.disposition.clone();
        self.commit_acceptance(acceptance)?;
        Ok(disposition)
    }

    pub fn stage_accept_all(
        &self,
        session_id: &str,
        path: &Path,
        current_revision: u64,
        current_contents: &str,
    ) -> anyhow::Result<StagedProposalAcceptance> {
        let path = self.normalize_path(path)?;
        let proposal = self.proposal(session_id, &path)?.clone();
        let Some(contents) = rebase_contents(
            &proposal.base_contents,
            current_contents,
            &proposal.proposed_contents,
        ) else {
            return Ok(StagedProposalAcceptance {
                disposition: ProposalDisposition::Conflict {
                    path: path.clone(),
                    base: proposal.base_contents.clone(),
                    current: current_contents.to_string(),
                    proposed: proposal.proposed_contents.clone(),
                },
                session_id: session_id.to_string(),
                path,
                expected: proposal.clone(),
                updated: proposal,
            });
        };
        let mut updated = proposal.clone();
        updated.base_revision = current_revision;
        updated.base_contents.clone_from(&contents);
        updated.proposed_contents.clone_from(&contents);
        let disposition = if contents == current_contents {
            ProposalDisposition::NoChanges
        } else {
            ProposalDisposition::Applied {
                path: path.clone(),
                contents,
                current_contents: current_contents.to_string(),
                base_revision: proposal.base_revision,
                session_id: session_id.to_string(),
                turn_id: proposal
                    .turn_id
                    .clone()
                    .unwrap_or_else(|| "unattributed".to_string()),
                created: proposal.created,
            }
        };
        Ok(StagedProposalAcceptance {
            disposition,
            session_id: session_id.to_string(),
            path,
            expected: proposal,
            updated,
        })
    }

    pub fn accept_hunk(
        &mut self,
        session_id: &str,
        path: &Path,
        selected_hunk_id: &str,
        current_revision: u64,
        current_contents: &str,
    ) -> anyhow::Result<ProposalDisposition> {
        let acceptance = self.stage_accept_hunk(
            session_id,
            path,
            selected_hunk_id,
            current_revision,
            current_contents,
        )?;
        let disposition = acceptance.disposition.clone();
        self.commit_acceptance(acceptance)?;
        Ok(disposition)
    }

    pub fn stage_accept_hunk(
        &self,
        session_id: &str,
        path: &Path,
        selected_hunk_id: &str,
        current_revision: u64,
        current_contents: &str,
    ) -> anyhow::Result<StagedProposalAcceptance> {
        let path = self.normalize_path(path)?;
        let proposal = self.proposal(session_id, &path)?.clone();
        let Some(rebased) = rebase_contents(
            &proposal.base_contents,
            current_contents,
            &proposal.proposed_contents,
        ) else {
            return Ok(StagedProposalAcceptance {
                disposition: ProposalDisposition::Conflict {
                    path: path.clone(),
                    base: proposal.base_contents.clone(),
                    current: current_contents.to_string(),
                    proposed: proposal.proposed_contents.clone(),
                },
                session_id: session_id.to_string(),
                path,
                expected: proposal.clone(),
                updated: proposal,
            });
        };
        let changes = changes_between(current_contents, &rebased);
        let selected = changes
            .iter()
            .find(|change| hunk_id(&path, change) == selected_hunk_id)
            .ok_or_else(|| anyhow::anyhow!("proposal hunk is stale"))?;
        let contents = apply_changes(current_contents, std::slice::from_ref(selected));
        let mut updated = proposal.clone();
        updated.base_revision = current_revision;
        updated.base_contents.clone_from(&contents);
        updated.proposed_contents = rebased;
        Ok(StagedProposalAcceptance {
            disposition: ProposalDisposition::Applied {
                path: path.clone(),
                contents,
                current_contents: current_contents.to_string(),
                base_revision: proposal.base_revision,
                session_id: session_id.to_string(),
                turn_id: proposal
                    .turn_id
                    .clone()
                    .unwrap_or_else(|| "unattributed".to_string()),
                created: proposal.created,
            },
            session_id: session_id.to_string(),
            path,
            expected: proposal,
            updated,
        })
    }

    pub fn commit_acceptance(
        &mut self,
        acceptance: StagedProposalAcceptance,
    ) -> anyhow::Result<()> {
        let current = self.proposal_mut(&acceptance.session_id, &acceptance.path)?;
        anyhow::ensure!(
            *current == acceptance.expected,
            "agent proposal changed while it was being accepted"
        );
        if *current != acceptance.updated {
            *current = acceptance.updated;
            self.bump_generation();
        }
        Ok(())
    }

    pub fn reject_all(
        &mut self,
        session_id: &str,
        path: &Path,
        current_revision: u64,
        current_contents: &str,
    ) -> anyhow::Result<()> {
        let path = self.normalize_path(path)?;
        self.reset_file(session_id, &path, current_revision, current_contents)
    }

    pub fn reject_hunk(
        &mut self,
        session_id: &str,
        path: &Path,
        selected_hunk_id: &str,
        current_revision: u64,
        current_contents: &str,
    ) -> anyhow::Result<()> {
        let path = self.normalize_path(path)?;
        let proposal = self.proposal(session_id, &path)?.clone();
        let rebased = rebase_contents(
            &proposal.base_contents,
            current_contents,
            &proposal.proposed_contents,
        )
        .ok_or_else(|| anyhow::anyhow!("proposal conflicts with current buffer"))?;
        let mut remaining = changes_between(current_contents, &rebased);
        let original_len = remaining.len();
        remaining.retain(|change| hunk_id(&path, change) != selected_hunk_id);
        anyhow::ensure!(remaining.len() != original_len, "proposal hunk is stale");
        let proposed_contents = apply_changes(current_contents, &remaining);
        self.reset_file(session_id, &path, current_revision, current_contents)?;
        let changed = {
            let proposal = self.proposal_mut(session_id, &path)?;
            let changed = proposal.proposed_contents != proposed_contents;
            if changed {
                proposal.proposed_contents = proposed_contents;
            }
            changed
        };
        if changed {
            self.bump_generation();
        }
        Ok(())
    }

    fn ensure_proposal(
        &mut self,
        session_id: &str,
        path: &Path,
    ) -> anyhow::Result<&mut ProposedFile> {
        if self
            .sessions
            .get(session_id)
            .is_some_and(|session| session.files.contains_key(path))
        {
            return self.proposal_mut(session_id, path);
        }
        let (base_revision, contents, created) = if let Some(visible) = self.visible.get(path) {
            anyhow::ensure!(
                visible.contents.len() <= MAX_PROPOSAL_CONTENT_BYTES,
                "ACP proposal source exceeds {MAX_PROPOSAL_CONTENT_BYTES} bytes"
            );
            (visible.revision, visible.contents.clone(), false)
        } else {
            match read_bounded_file(self, path)? {
                Some(contents) => (0, contents, false),
                None => (0, String::new(), true),
            }
        };
        self.sessions
            .entry(session_id.to_string())
            .or_default()
            .files
            .insert(
                path.to_path_buf(),
                ProposedFile {
                    base_revision,
                    base_contents: contents.clone(),
                    proposed_contents: contents,
                    created,
                    turn_id: None,
                },
            );
        self.bump_generation();
        self.proposal_mut(session_id, path)
    }

    fn proposal(&self, session_id: &str, path: &Path) -> anyhow::Result<&ProposedFile> {
        self.sessions
            .get(session_id)
            .and_then(|session| session.files.get(path))
            .ok_or_else(|| anyhow::anyhow!("agent has not read or written {}", path.display()))
    }

    fn proposal_mut(&mut self, session_id: &str, path: &Path) -> anyhow::Result<&mut ProposedFile> {
        self.sessions
            .get_mut(session_id)
            .and_then(|session| session.files.get_mut(path))
            .ok_or_else(|| anyhow::anyhow!("agent has not read or written {}", path.display()))
    }

    fn reset_file(
        &mut self,
        session_id: &str,
        path: &Path,
        revision: u64,
        contents: &str,
    ) -> anyhow::Result<()> {
        let changed = {
            let proposal = self.proposal_mut(session_id, path)?;
            let changed = proposal.base_revision != revision
                || proposal.base_contents != contents
                || proposal.proposed_contents != contents;
            if changed {
                proposal.base_revision = revision;
                proposal.base_contents = contents.to_string();
                proposal.proposed_contents = contents.to_string();
            }
            changed
        };
        if changed {
            self.bump_generation();
        }
        Ok(())
    }

    fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    fn normalize_path(&self, path: &Path) -> anyhow::Result<PathBuf> {
        anyhow::ensure!(path.is_absolute(), "ACP filesystem path must be absolute");
        let path = lexical_normalize(path)?;
        anyhow::ensure!(
            path.starts_with(&self.root),
            "agent path {} is outside workspace {}",
            path.display(),
            self.root.display()
        );
        reject_symlink_components(&self.root, &path)?;
        Ok(path)
    }
}

#[cfg(unix)]
fn open_workspace_root(root: &Path) -> Option<std::fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};

    use nix::{
        fcntl::{openat, OFlag},
        sys::stat::Mode,
    };

    #[cfg(target_os = "macos")]
    let physical = {
        let mut physical = root.to_path_buf();
        for (alias, target) in [
            (Path::new("/var"), Path::new("/private/var")),
            (Path::new("/tmp"), Path::new("/private/tmp")),
            (Path::new("/etc"), Path::new("/private/etc")),
        ] {
            if let Ok(remainder) = root.strip_prefix(alias) {
                physical = target.join(remainder);
                break;
            }
        }
        physical
    };
    #[cfg(not(target_os = "macos"))]
    let physical = root.to_path_buf();

    let mut directory = std::fs::File::open("/").ok()?;
    for component in physical.components() {
        let name = match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => name,
            Component::ParentDir | Component::Prefix(_) => return None,
        };
        let descriptor = openat(
            Some(directory.as_raw_fd()),
            name,
            OFlag::O_RDONLY
                | OFlag::O_DIRECTORY
                | OFlag::O_CLOEXEC
                | OFlag::O_NOFOLLOW
                | OFlag::O_NONBLOCK,
            Mode::empty(),
        )
        .ok()?;
        // SAFETY: `openat` returned a new owned descriptor and `File` becomes its sole owner.
        directory = unsafe { std::fs::File::from_raw_fd(descriptor) };
    }
    directory.metadata().ok()?.is_dir().then_some(directory)
}

#[cfg(unix)]
fn read_bounded_file(workspace: &ProposalWorkspace, path: &Path) -> anyhow::Result<Option<String>> {
    use std::os::fd::{AsRawFd, FromRawFd};

    use nix::{
        errno::Errno,
        fcntl::{openat, OFlag},
        sys::stat::Mode,
    };

    let Some(root_directory) = workspace.root_directory.as_ref() else {
        return match std::fs::symlink_metadata(path) {
            Ok(_) => anyhow::bail!(
                "ACP proposal source cannot be opened safely because the workspace root is unavailable"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        };
    };
    let mut directory = root_directory.try_clone()?;
    let components = path
        .strip_prefix(&workspace.root)?
        .components()
        .collect::<Vec<_>>();
    anyhow::ensure!(
        !components.is_empty(),
        "ACP proposal source must be a regular file below the workspace root"
    );
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            anyhow::bail!("ACP proposal source contains a non-normal path component");
        };
        let final_component = index + 1 == components.len();
        let mut flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK;
        if !final_component {
            flags |= OFlag::O_DIRECTORY;
        }
        let descriptor = match openat(Some(directory.as_raw_fd()), *name, flags, Mode::empty()) {
            Ok(descriptor) => descriptor,
            Err(Errno::ENOENT) => return Ok(None),
            Err(error) => anyhow::bail!("ACP proposal source cannot be opened safely: {error}"),
        };
        // SAFETY: `openat` returned a new owned descriptor and `File` becomes its sole owner.
        let file = unsafe { std::fs::File::from_raw_fd(descriptor) };
        if final_component {
            return read_open_file(file, path).map(Some);
        }
        directory = file;
    }
    Ok(None)
}

#[cfg(not(unix))]
fn read_bounded_file(
    _workspace: &ProposalWorkspace,
    path: &Path,
) -> anyhow::Result<Option<String>> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => anyhow::bail!(
            "ACP proposal source cannot be read safely on this platform; open the file in Red first"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn read_open_file(file: std::fs::File, path: &Path) -> anyhow::Result<String> {
    anyhow::ensure!(
        file.metadata()?.is_file(),
        "ACP proposal source {} is not a regular file",
        path.display()
    );
    let mut contents = String::new();
    file.take((MAX_PROPOSAL_CONTENT_BYTES + 1) as u64)
        .read_to_string(&mut contents)?;
    anyhow::ensure!(
        contents.len() <= MAX_PROPOSAL_CONTENT_BYTES,
        "ACP proposal source {} exceeds {MAX_PROPOSAL_CONTENT_BYTES} bytes",
        path.display()
    );
    Ok(contents)
}

/// ACP host that exposes the proposal filesystem and denies permissions until a UI
/// explicitly supplies an option-selection policy.
#[derive(Debug, Clone)]
pub struct ProposalAcpHost {
    workspace: Arc<Mutex<ProposalWorkspace>>,
}

impl ProposalAcpHost {
    #[must_use]
    pub fn new(workspace: Arc<Mutex<ProposalWorkspace>>) -> Self {
        Self { workspace }
    }

    #[must_use]
    pub fn workspace(&self) -> Arc<Mutex<ProposalWorkspace>> {
        Arc::clone(&self.workspace)
    }
}

#[async_trait]
impl AcpHost for ProposalAcpHost {
    async fn read_text_file(
        &mut self,
        request: ReadTextFileRequest,
    ) -> anyhow::Result<ReadTextFileResponse> {
        let contents = self
            .workspace
            .lock()
            .map_err(|_| anyhow::anyhow!("proposal workspace lock is poisoned"))?
            .read(
                &request.session_id.to_string(),
                &request.path,
                request.line,
                request.limit,
            )?;
        Ok(ReadTextFileResponse::new(contents))
    }

    async fn write_text_file(
        &mut self,
        request: WriteTextFileRequest,
    ) -> anyhow::Result<WriteTextFileResponse> {
        self.workspace
            .lock()
            .map_err(|_| anyhow::anyhow!("proposal workspace lock is poisoned"))?
            .write(
                &request.session_id.to_string(),
                &request.path,
                request.content,
            )?;
        Ok(WriteTextFileResponse::new())
    }

    async fn request_permission(
        &mut self,
        _request: RequestPermissionRequest,
    ) -> anyhow::Result<RequestPermissionResponse> {
        Ok(RequestPermissionResponse::new(
            RequestPermissionOutcome::Cancelled,
        ))
    }

    async fn session_update(&mut self, _notification: SessionNotification) -> anyhow::Result<()> {
        Ok(())
    }
}

fn slice_lines(contents: &str, line: Option<u32>, limit: Option<u32>) -> String {
    if line.is_none() && limit.is_none() {
        return contents.to_string();
    }
    let start = line.unwrap_or(1).saturating_sub(1) as usize;
    let limit = limit.map_or(usize::MAX, |value| value as usize);
    contents
        .split_inclusive('\n')
        .skip(start)
        .take(limit)
        .collect()
}

fn lexical_normalize(path: &Path) -> anyhow::Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                anyhow::ensure!(normalized.pop(), "agent path escapes filesystem root");
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    Ok(normalized)
}

fn reject_symlink_components(root: &Path, path: &Path) -> anyhow::Result<()> {
    let mut current = root.to_path_buf();
    for component in path.strip_prefix(root)?.components() {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!(
                    "agent path contains symlink component: {}",
                    current.display()
                )
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn changes_between(base: &str, target: &str) -> Vec<TextChange> {
    let target_chars = target.chars().collect::<Vec<_>>();
    TextDiff::from_chars(base, target)
        .ops()
        .iter()
        .filter(|operation| operation.tag() != DiffTag::Equal)
        .map(|operation| TextChange {
            base_start: operation.old_range().start,
            base_end: operation.old_range().end,
            replacement: target_chars[operation.new_range()].iter().collect(),
        })
        .filter(|change| change.base_start != change.base_end || !change.replacement.is_empty())
        .collect()
}

fn changes_overlap(left: &TextChange, right: &TextChange) -> bool {
    match (
        left.base_start == left.base_end,
        right.base_start == right.base_end,
    ) {
        (true, true) => left.base_start == right.base_start,
        (true, false) => right.base_start < left.base_start && left.base_start < right.base_end,
        (false, true) => left.base_start < right.base_start && right.base_start < left.base_end,
        (false, false) => left.base_start < right.base_end && right.base_start < left.base_end,
    }
}

fn rebase_contents(base: &str, current: &str, proposed: &str) -> Option<String> {
    if current == base {
        return Some(proposed.to_string());
    }
    if proposed == base || current == proposed {
        return Some(current.to_string());
    }
    let user_changes = changes_between(base, current);
    let agent_changes = changes_between(base, proposed);
    if user_changes.iter().any(|user| {
        agent_changes
            .iter()
            .any(|agent| changes_overlap(user, agent))
    }) {
        return None;
    }

    let mapped = agent_changes
        .into_iter()
        .map(|agent| {
            let delta = user_changes
                .iter()
                .filter(|user| user.base_end <= agent.base_start)
                .map(|user| {
                    user.replacement.chars().count() as isize
                        - (user.base_end - user.base_start) as isize
                })
                .sum::<isize>();
            TextChange {
                base_start: agent.base_start.saturating_add_signed(delta),
                base_end: agent.base_end.saturating_add_signed(delta),
                replacement: agent.replacement,
            }
        })
        .collect::<Vec<_>>();
    Some(apply_changes(current, &mapped))
}

fn apply_changes(contents: &str, changes: &[TextChange]) -> String {
    let mut characters = contents.chars().collect::<Vec<_>>();
    let mut changes = changes.to_vec();
    changes.sort_by_key(|change| change.base_start);
    for change in changes.into_iter().rev() {
        characters.splice(
            change.base_start..change.base_end,
            change.replacement.chars(),
        );
    }
    characters.into_iter().collect()
}

fn char_slice(contents: &str, start: usize, end: usize) -> String {
    contents.chars().skip(start).take(end - start).collect()
}

fn hunk_id(path: &Path, change: &TextChange) -> String {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!(
            "red-proposal:{}:{}:{}:{}",
            path.display(),
            change.base_start,
            change.base_end,
            change.replacement
        )
        .as_bytes(),
    )
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> (tempfile::TempDir, ProposalWorkspace, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("src.rs");
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let mut workspace = ProposalWorkspace::new(temp.path()).unwrap();
        workspace
            .sync_visible_file(&path, 7, "one\nunsaved\nthree\n".to_string())
            .unwrap();
        (temp, workspace, path)
    }

    #[test]
    fn read_after_write_uses_unsaved_base_without_touching_disk() {
        let (_temp, mut workspace, path) = workspace();
        assert_eq!(
            workspace.read("s1", &path, None, None).unwrap(),
            "one\nunsaved\nthree\n"
        );
        workspace
            .write("s1", &path, "one\nagent\nthree\n".to_string())
            .unwrap();
        assert_eq!(
            workspace.read("s1", &path, None, None).unwrap(),
            "one\nagent\nthree\n"
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "one\ntwo\nthree\n");
    }

    #[test]
    fn generation_changes_only_for_snapshot_relevant_mutations() {
        let (_temp, mut workspace, path) = workspace();
        let initial = workspace.generation();

        workspace
            .sync_visible_file(&path, 7, "one\nunsaved\nthree\n".to_string())
            .unwrap();
        assert_eq!(workspace.generation(), initial);

        workspace.read("s1", &path, None, None).unwrap();
        let after_read = workspace.generation();
        assert_ne!(after_read, initial);
        workspace.read("s1", &path, None, None).unwrap();
        assert_eq!(workspace.generation(), after_read);

        workspace.begin_turn("s1", "turn-1".to_string());
        let after_turn = workspace.generation();
        assert_ne!(after_turn, after_read);
        workspace.begin_turn("s1", "turn-1".to_string());
        assert_eq!(workspace.generation(), after_turn);

        workspace
            .write("s1", &path, "one\nagent\nthree\n".to_string())
            .unwrap();
        let after_write = workspace.generation();
        assert_ne!(after_write, after_turn);
        workspace
            .write("s1", &path, "one\nagent\nthree\n".to_string())
            .unwrap();
        assert_eq!(workspace.generation(), after_write);

        workspace.close_session("missing");
        assert_eq!(workspace.generation(), after_write);
        workspace.close_session("s1");
        assert_ne!(workspace.generation(), after_write);
    }

    #[test]
    fn clean_and_conflicting_user_divergence_are_distinguished() {
        let (_temp, mut workspace, path) = workspace();
        workspace
            .write("s1", &path, "ONE\nunsaved\nthree\n".to_string())
            .unwrap();
        assert_eq!(
            workspace
                .accept_all("s1", &path, 8, "one\nunsaved\nTHREE\n")
                .unwrap(),
            ProposalDisposition::Applied {
                path: path.clone(),
                contents: "ONE\nunsaved\nTHREE\n".to_string(),
                current_contents: "one\nunsaved\nTHREE\n".to_string(),
                base_revision: 7,
                session_id: "s1".to_string(),
                turn_id: "unattributed".to_string(),
                created: false,
            }
        );

        workspace
            .write("s2", &path, "agent\nunsaved\nthree\n".to_string())
            .unwrap();
        assert!(matches!(
            workspace
                .accept_all("s2", &path, 9, "user\nunsaved\nthree\n")
                .unwrap(),
            ProposalDisposition::Conflict { .. }
        ));
    }

    #[test]
    fn adjacent_user_insertion_and_agent_replacement_rebase_cleanly() {
        assert_eq!(
            rebase_contents("abcdef", "ab!cdef", "aBcdef"),
            Some("aB!cdef".to_string())
        );
    }

    #[test]
    fn adjacent_user_replacement_and_agent_insertion_rebase_cleanly() {
        assert_eq!(
            rebase_contents("abcdef", "aBcdef", "ab!cdef"),
            Some("aB!cdef".to_string())
        );
    }

    #[test]
    fn closing_a_session_releases_only_its_proposals() {
        let (_temp, mut workspace, path) = workspace();
        workspace
            .write("s1", &path, "one\nfirst agent\nthree\n".to_string())
            .unwrap();
        workspace
            .write("s2", &path, "one\nsecond agent\nthree\n".to_string())
            .unwrap();

        workspace.close_session("s1");

        assert!(workspace.pending_files("s1").is_empty());
        assert_eq!(workspace.pending_files("s2"), [path]);
    }

    #[test]
    fn archiving_a_session_preserves_pending_proposals_and_discards_clean_sessions() {
        let (_temp, mut workspace, path) = workspace();
        workspace.begin_turn("pending", "turn-1".to_string());
        workspace
            .write("pending", &path, "one\nagent\nthree\n".to_string())
            .unwrap();
        workspace.begin_turn("clean", "turn-2".to_string());
        workspace.read("clean", &path, None, None).unwrap();
        let before_archive = workspace.generation();

        workspace.archive_session("pending");

        assert_ne!(workspace.generation(), before_archive);
        assert_eq!(workspace.review_sessions(""), ["pending"]);
        assert_eq!(
            workspace.pending_files("pending"),
            std::slice::from_ref(&path)
        );
        let after_archive = workspace.generation();
        workspace.archive_session("pending");
        assert_eq!(workspace.generation(), after_archive);

        let before_clean_archive = workspace.generation();
        workspace.archive_session("clean");
        assert_ne!(workspace.generation(), before_clean_archive);
        assert_eq!(workspace.review_sessions(""), ["pending"]);
        assert!(workspace.pending_files("clean").is_empty());
    }

    #[test]
    fn archived_proposals_are_reviewable_before_and_after_session_replacement() {
        let (_temp, mut workspace, path) = workspace();
        workspace.begin_turn("archived", "turn-1".to_string());
        workspace
            .write("archived", &path, "one\nagent\nthree\n".to_string())
            .unwrap();
        let snapshot = workspace.snapshot();
        let mut restored = ProposalWorkspace::from_snapshot(snapshot);

        assert_eq!(restored.review_sessions(""), ["archived"]);
        assert_eq!(
            restored.pending_files("archived"),
            std::slice::from_ref(&path)
        );

        restored.begin_turn("replacement", "turn-2".to_string());

        assert!(restored.pending_files("archived").is_empty());
        assert_eq!(restored.review_sessions("replacement"), ["replacement"]);
        assert_eq!(restored.pending_files("replacement"), [path]);
    }

    #[test]
    fn overlapping_archived_proposals_remain_independently_reviewable() {
        let (_temp, mut workspace, path) = workspace();
        workspace
            .write("archived", &path, "one\narchived\nthree\n".to_string())
            .unwrap();
        let snapshot = workspace.snapshot();
        let mut restored = ProposalWorkspace::from_snapshot(snapshot);
        restored
            .write(
                "replacement",
                &path,
                "one\nreplacement\nthree\n".to_string(),
            )
            .unwrap();

        assert_eq!(restored.adopt_recovered_sessions("replacement"), 0);
        assert_eq!(
            restored.review_sessions("replacement"),
            ["replacement", "archived"]
        );
        assert_eq!(
            restored.pending_files("archived"),
            std::slice::from_ref(&path)
        );
        assert_eq!(restored.pending_files("replacement"), [path]);
    }

    #[test]
    fn partial_accept_and_reject_rebase_remaining_hunks() {
        let (_temp, mut workspace, path) = workspace();
        workspace
            .write("s1", &path, "ONE\nunsaved\nTHREE\n".to_string())
            .unwrap();
        let hunks = workspace
            .hunks("s1", &path, "one\nunsaved\nthree\n")
            .unwrap();
        assert_eq!(hunks.len(), 2);
        let accepted = workspace
            .accept_hunk("s1", &path, &hunks[0].id, 8, "one\nunsaved\nthree\n")
            .unwrap();
        let ProposalDisposition::Applied { contents, .. } = accepted else {
            panic!("hunk should apply");
        };
        assert_eq!(contents, "ONE\nunsaved\nthree\n");
        let remaining = workspace.hunks("s1", &path, &contents).unwrap();
        assert_eq!(remaining.len(), 1);
        workspace
            .reject_hunk("s1", &path, &remaining[0].id, 9, &contents)
            .unwrap();
        assert!(workspace.pending_files("s1").is_empty());
    }

    #[test]
    fn staged_acceptance_keeps_proposals_pending_until_commit_and_rejects_concurrent_changes() {
        let (_temp, mut workspace, path) = workspace();
        workspace
            .write("s1", &path, "ONE\nunsaved\nTHREE\n".to_string())
            .unwrap();
        let hunks = workspace
            .hunks("s1", &path, "one\nunsaved\nthree\n")
            .unwrap();
        let acceptance = workspace
            .stage_accept_hunk("s1", &path, &hunks[0].id, 8, "one\nunsaved\nthree\n")
            .unwrap();

        assert_eq!(workspace.pending_files("s1"), std::slice::from_ref(&path));
        assert_eq!(
            workspace
                .hunks("s1", &path, "one\nunsaved\nthree\n")
                .unwrap()
                .len(),
            2
        );

        workspace.commit_acceptance(acceptance).unwrap();
        assert_eq!(
            workspace
                .hunks("s1", &path, "ONE\nunsaved\nthree\n")
                .unwrap()
                .len(),
            1
        );

        let acceptance = workspace
            .stage_accept_all("s1", &path, 9, "ONE\nunsaved\nthree\n")
            .unwrap();
        workspace
            .write("s1", &path, "ONE\nnew agent change\nTHREE\n".to_string())
            .unwrap();

        assert!(workspace.commit_acceptance(acceptance).is_err());
        assert_eq!(workspace.pending_files("s1"), [path]);
    }

    #[test]
    fn outside_workspace_and_symlink_paths_are_rejected() {
        let (temp, mut workspace, _path) = workspace();
        assert!(workspace
            .read("s1", Path::new("/outside"), None, None)
            .is_err());
        assert!(workspace.read("s1", temp.path(), None, None).is_err());

        #[cfg(unix)]
        {
            let link = temp.path().join("link");
            std::os::unix::fs::symlink("/outside", &link).unwrap();
            assert!(workspace.read("s1", &link, None, None).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn disk_reads_stay_anchored_when_the_workspace_root_is_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let moved_root = temp.path().join("original-workspace");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(root.join("source.rs"), "workspace contents\n").unwrap();
        std::fs::write(outside.join("source.rs"), "outside secret\n").unwrap();
        let mut workspace = ProposalWorkspace::new(&root).unwrap();

        std::fs::rename(&root, &moved_root).unwrap();
        std::os::unix::fs::symlink(&outside, &root).unwrap();

        assert_eq!(
            workspace
                .read("s1", &root.join("source.rs"), None, None)
                .unwrap(),
            "workspace contents\n"
        );
        assert_eq!(
            workspace
                .read_current_file(&root.join("source.rs"))
                .unwrap()
                .as_deref(),
            Some("workspace contents\n")
        );
    }

    #[cfg(unix)]
    #[test]
    fn restored_workspace_refuses_a_symlinked_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let moved_root = temp.path().join("original-workspace");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(root.join("source.rs"), "workspace contents\n").unwrap();
        std::fs::write(outside.join("source.rs"), "outside secret\n").unwrap();
        let workspace = ProposalWorkspace::new(&root).unwrap();
        let snapshot = workspace.snapshot();

        std::fs::rename(&root, &moved_root).unwrap();
        std::os::unix::fs::symlink(&outside, &root).unwrap();
        let restored = ProposalWorkspace::from_snapshot(snapshot);

        let error = restored
            .read_current_file(&root.join("source.rs"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("workspace root is unavailable"));
    }

    #[cfg(unix)]
    #[test]
    fn disk_reads_refuse_a_component_replaced_with_a_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source.rs");
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(&path, "workspace contents\n").unwrap();
        std::fs::write(outside.path(), "outside secret\n").unwrap();
        let workspace = ProposalWorkspace::new(temp.path()).unwrap();
        let normalized = workspace.normalize_path(&path).unwrap();

        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(outside.path(), &path).unwrap();

        assert!(read_bounded_file(&workspace, &normalized).is_err());
        assert!(workspace.read_current_file(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn current_disk_reads_reject_fifos_without_blocking() {
        use nix::{sys::stat::Mode, unistd::mkfifo};

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source.pipe");
        mkfifo(&path, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        let workspace = ProposalWorkspace::new(temp.path()).unwrap();

        let error = workspace.read_current_file(&path).unwrap_err().to_string();

        assert!(error.contains("not a regular file"));
    }

    #[cfg(not(unix))]
    #[test]
    fn disk_reads_fail_closed_without_portable_no_follow_opens() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source.rs");
        std::fs::write(&path, "disk contents\n").unwrap();
        let mut workspace = ProposalWorkspace::new(temp.path()).unwrap();

        assert!(workspace
            .read("s1", &path, None, None)
            .unwrap_err()
            .to_string()
            .contains("open the file in Red first"));

        workspace
            .sync_visible_file(&path, 7, "unsaved contents\n".to_string())
            .unwrap();
        assert_eq!(
            workspace.read("s2", &path, None, None).unwrap(),
            "unsaved contents\n"
        );
    }

    #[test]
    fn oversized_disk_visible_and_proposed_contents_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let disk_path = temp.path().join("large-disk.txt");
        let visible_path = temp.path().join("large-visible.txt");
        let proposed_path = temp.path().join("large-proposed.txt");
        let oversized = "x".repeat(MAX_PROPOSAL_CONTENT_BYTES + 1);
        std::fs::write(&disk_path, &oversized).unwrap();
        let mut workspace = ProposalWorkspace::new(temp.path()).unwrap();

        let disk_error = workspace
            .read("s1", &disk_path, None, None)
            .unwrap_err()
            .to_string();
        #[cfg(unix)]
        assert!(disk_error.contains("exceed"));
        #[cfg(not(unix))]
        assert!(disk_error.contains("open the file in Red first"));
        let current_error = workspace
            .read_current_file(&disk_path)
            .unwrap_err()
            .to_string();
        #[cfg(unix)]
        assert!(current_error.contains("exceed"));
        #[cfg(not(unix))]
        assert!(current_error.contains("open the file in Red first"));
        workspace
            .sync_visible_file(&visible_path, 1, oversized.clone())
            .unwrap();
        assert!(workspace
            .read("s1", &visible_path, None, None)
            .unwrap_err()
            .to_string()
            .contains("exceed"));
        assert!(workspace
            .write("s1", &proposed_path, oversized)
            .unwrap_err()
            .to_string()
            .contains("exceed"));
    }

    #[test]
    fn unicode_hunks_use_character_coordinates() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("unicode.txt");
        let mut workspace = ProposalWorkspace::new(temp.path()).unwrap();
        workspace
            .sync_visible_file(&path, 1, "a👋b\n".to_string())
            .unwrap();
        workspace.write("s1", &path, "a🌍b\n".to_string()).unwrap();
        let hunks = workspace.hunks("s1", &path, "a👋b\n").unwrap();
        assert_eq!(hunks[0].old_start, 1);
        assert_eq!(hunks[0].old_end, 2);
    }
}
