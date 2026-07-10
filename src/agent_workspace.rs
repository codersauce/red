//! Session-scoped, review-before-apply filesystem for ACP agents.
//!
//! Agent writes update proposed contents only. Visible buffers and disk are never
//! mutated by this module; callers must explicitly accept a proposal and route the
//! returned contents through the editor's transaction boundary.

use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

use agent_client_protocol_schema::v1::{
    ReadTextFileRequest, ReadTextFileResponse, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SessionNotification, WriteTextFileRequest, WriteTextFileResponse,
};
use async_trait::async_trait;
use path_absolutize::Absolutize as _;
use serde::{Deserialize, Serialize};
use similar::{DiffTag, TextDiff};
use uuid::Uuid;

use crate::acp::AcpHost;

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

/// Shared proposal state for all live ACP sessions.
#[derive(Debug)]
pub struct ProposalWorkspace {
    root: PathBuf,
    visible: HashMap<PathBuf, VisibleFile>,
    sessions: HashMap<String, AgentSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalWorkspaceSnapshot {
    root: PathBuf,
    visible: HashMap<PathBuf, VisibleFile>,
    sessions: HashMap<String, AgentSession>,
}

impl ProposalWorkspace {
    /// Create a workspace rooted at an existing or prospective directory.
    pub fn new(root: impl AsRef<Path>) -> anyhow::Result<Self> {
        let root = root.as_ref().absolutize()?.to_path_buf();
        Ok(Self {
            root,
            visible: HashMap::new(),
            sessions: HashMap::new(),
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
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
        Self {
            root: snapshot.root,
            visible: snapshot.visible,
            sessions: snapshot.sessions,
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
        let path = self.normalize_path(path.as_ref())?;
        self.visible
            .insert(path, VisibleFile { revision, contents });
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
        let turn_id = self
            .sessions
            .get(session_id)
            .and_then(|session| session.current_turn.clone())
            .unwrap_or_else(|| "unattributed".to_string());
        let proposal = self.ensure_proposal(session_id, &path)?;
        proposal.proposed_contents = contents;
        proposal.turn_id = Some(turn_id);
        Ok(())
    }

    pub fn begin_turn(&mut self, session_id: &str, turn_id: String) {
        self.sessions
            .entry(session_id.to_string())
            .or_default()
            .current_turn = Some(turn_id);
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
        let path = self.normalize_path(path)?;
        let proposal = self.proposal(session_id, &path)?.clone();
        let Some(contents) = rebase_contents(
            &proposal.base_contents,
            current_contents,
            &proposal.proposed_contents,
        ) else {
            return Ok(ProposalDisposition::Conflict {
                path,
                base: proposal.base_contents,
                current: current_contents.to_string(),
                proposed: proposal.proposed_contents,
            });
        };
        if contents == current_contents {
            self.reset_file(session_id, &path, current_revision, current_contents)?;
            return Ok(ProposalDisposition::NoChanges);
        }
        self.reset_file(session_id, &path, current_revision, &contents)?;
        Ok(ProposalDisposition::Applied {
            path,
            contents,
            base_revision: proposal.base_revision,
            session_id: session_id.to_string(),
            turn_id: proposal
                .turn_id
                .unwrap_or_else(|| "unattributed".to_string()),
            created: proposal.created,
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
        let path = self.normalize_path(path)?;
        let proposal = self.proposal(session_id, &path)?.clone();
        let Some(rebased) = rebase_contents(
            &proposal.base_contents,
            current_contents,
            &proposal.proposed_contents,
        ) else {
            return Ok(ProposalDisposition::Conflict {
                path,
                base: proposal.base_contents,
                current: current_contents.to_string(),
                proposed: proposal.proposed_contents,
            });
        };
        let changes = changes_between(current_contents, &rebased);
        let selected = changes
            .iter()
            .find(|change| hunk_id(&path, change) == selected_hunk_id)
            .ok_or_else(|| anyhow::anyhow!("proposal hunk is stale"))?;
        let contents = apply_changes(current_contents, std::slice::from_ref(selected));
        self.reset_file(session_id, &path, current_revision, &contents)?;
        if contents != rebased {
            let proposal = self.proposal_mut(session_id, &path)?;
            proposal.proposed_contents = rebased;
        }
        Ok(ProposalDisposition::Applied {
            path,
            contents,
            base_revision: proposal.base_revision,
            session_id: session_id.to_string(),
            turn_id: proposal
                .turn_id
                .unwrap_or_else(|| "unattributed".to_string()),
            created: proposal.created,
        })
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
        self.proposal_mut(session_id, &path)?.proposed_contents = proposed_contents;
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
            (visible.revision, visible.contents.clone(), false)
        } else if path.exists() {
            (0, std::fs::read_to_string(path)?, false)
        } else {
            (0, String::new(), true)
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
        let proposal = self.proposal_mut(session_id, path)?;
        proposal.base_revision = revision;
        proposal.base_contents = contents.to_string();
        proposal.proposed_contents = contents.to_string();
        Ok(())
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
    if left.base_start == left.base_end && right.base_start == right.base_end {
        return left.base_start == right.base_start;
    }
    left.base_start < right.base_end && right.base_start < left.base_end
        || left.base_start == right.base_start
        || left.base_end == right.base_end
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
    fn outside_workspace_and_symlink_paths_are_rejected() {
        let (temp, mut workspace, _path) = workspace();
        assert!(workspace
            .read("s1", Path::new("/outside"), None, None)
            .is_err());

        #[cfg(unix)]
        {
            let link = temp.path().join("link");
            std::os::unix::fs::symlink("/outside", &link).unwrap();
            assert!(workspace.read("s1", &link, None, None).is_err());
        }
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
