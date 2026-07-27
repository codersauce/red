//! Editor-owned replay sessions, source-linked observations, and one-shot stages.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::undo::{TextPosition, TextRange};

use super::{
    digest, fetch_pull_request_objects, finalize_pull_request, now_ms, parse_patch,
    prepare_workspace, GitObjectId, ReplayChangeKind, ReplayError, ReplayLimits,
    ReplayResolvedPullRequest, ReplaySource,
};

/// Canonical learning modes shared with the existing code-replay skill.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayMode {
    /// Explain the change without revealing the original implementation.
    #[default]
    Challenge,
    /// Reveal the exact source snippet while retaining manual reconstruction.
    Snippet,
}

/// Lifecycle state of an editor-owned reviewer learning session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplaySessionState {
    /// Session and scratch branch are ready for the reviewer.
    #[default]
    Ready,
    /// A learning step is active.
    Active,
    /// The guide is hidden while source and progress remain recoverable.
    Paused,
    /// All replayable learning steps have been handled.
    Completed,
}

/// Operation represented by one safe, source-linked editor exercise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayStepKind {
    /// Add contextually anchored source text.
    Add,
    /// Replace an exact contextually anchored source image.
    Change,
    /// Remove an exact contextually anchored source image.
    Remove,
    /// Populate a safely opened new-file editor buffer.
    AddFile,
}

/// Reviewer-visible step progress.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayStepStatus {
    /// The step has not been activated.
    #[default]
    Pending,
    /// The reviewer is studying or implementing this step.
    Active,
    /// The exact source image has been manually or automatically reproduced.
    Done,
    /// The reviewer explicitly skipped the learning exercise.
    Skipped,
    /// An unfinished prerequisite blocks this exercise.
    Blocked,
    /// The visible buffer no longer matches an unambiguous source image.
    Conflict,
}

/// Evidence distinguishing reviewer-authored work from confirmed assistance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayCompletion {
    /// The reviewer reconstructed the source by hand.
    Manual,
    /// The reviewer explicitly confirmed one staged editor transaction.
    Automatic,
}

/// Exact, context-aware result of inspecting the current editor buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayValidation {
    /// The unique expected target image is present in the correct file.
    Exact,
    /// The unique original pre-image remains ready to be implemented.
    Incomplete,
    /// More than one safe source candidate exists.
    Ambiguous,
    /// Neither exact pre-image nor exact target image is present.
    Conflict,
    /// A selected prerequisite remains unfinished.
    Blocked,
    /// The source change is informational and cannot be a text transaction.
    Unsupported,
}

/// Source-linked learning exercise derived from one complete unified hunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayStep {
    /// Stable UUID derived from the immutable source digest and original hunk.
    pub id: String,
    /// One-based presentation order.
    pub ordinal: usize,
    /// Safe text operation.
    pub kind: ReplayStepKind,
    /// Validated repository-relative original source path.
    pub path: PathBuf,
    /// Exact pinned original author head.
    pub target_commit: GitObjectId,
    /// Stable source hunk identity.
    pub hunk_digest: String,
    /// Original source semantic heading.
    pub heading: String,
    /// Exact source pre-image, including unique contextual lines.
    pub before: String,
    /// Exact original author post-image, including contextual lines.
    pub after: String,
    /// One-based old-file line anchor.
    pub old_start: usize,
    /// Steps in the same file that must first complete.
    pub dependencies: Vec<String>,
    /// Current reviewer-visible exercise state.
    pub status: ReplayStepStatus,
    /// Whether the step was completed manually or by a confirmed stage.
    pub completion: Option<ReplayCompletion>,
}

/// Confirmed, durable local scratch review workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayWorkspace {
    /// Exact root of the durable sibling Git worktree.
    pub root: PathBuf,
    /// Named local reviewer branch; never the original author head branch.
    pub branch: String,
    /// Exact pinned replay merge-base object.
    pub base_commit: GitObjectId,
    /// Whether this replay created and owns the worktree.
    pub created_by_replay: bool,
}

/// Read-only scratch worktree preview shown before explicit confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayWorkspacePreview {
    /// Preserved original working tree.
    pub repository_root: PathBuf,
    /// Proposed durable sibling review workspace.
    pub root: PathBuf,
    /// Proposed normal local review branch.
    pub branch: String,
    /// Original PR merge base from which reconstruction begins.
    pub base_commit: GitObjectId,
}

/// Source-linked local reviewer observation categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayNoteCategory {
    /// Question to ask the original pull request author.
    Question,
    /// Design or implementation observation.
    Observation,
    /// Potential correctness or security concern.
    PotentialIssue,
    /// Missing test or insufficient regression coverage.
    TestGap,
    /// Follow-up item for the final independent GitHub review.
    FollowUp,
}

/// Reviewer-owned local note; never a submitted or pending GitHub review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayNote {
    /// Stable local note identity.
    pub id: String,
    /// Original pinned author head to which the observation belongs.
    pub target_commit: GitObjectId,
    /// Original replay step when a finding concerns a particular source hunk.
    pub step_id: Option<String>,
    /// Validated original repository-relative source path.
    pub path: Option<PathBuf>,
    /// Reviewer-selected observation type.
    pub category: ReplayNoteCategory,
    /// Bounded reviewer-authored local text.
    pub text: String,
    /// Unix-millisecond creation time.
    pub created_at_ms: u64,
}

/// Complete reviewer session and original author source context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplaySession {
    /// Opaque editor-owned session identity.
    pub id: String,
    /// Immutable author source, context, and complete bounded patch.
    pub source: ReplaySource,
    /// Explicitly confirmed scratch review workspace.
    pub workspace: ReplayWorkspace,
    /// Manual Challenge or source-visible Snippet mode.
    pub mode: ReplayMode,
    /// Current recoverable learning session state.
    pub state: ReplaySessionState,
    /// Stable currently selected exercise identity.
    pub active_step: Option<String>,
    /// Complete deterministic learning exercises.
    pub steps: Vec<ReplayStep>,
    /// Informational original changes excluded from automatic application.
    pub informational_changes: Vec<PathBuf>,
    /// Source-linked local reviewer observations.
    pub notes: Vec<ReplayNote>,
    /// Monotonic state generation used to invalidate stale previews.
    pub generation: u64,
}

/// Opaque, single-use automatic-application preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayStage {
    /// Unforgeable editor-owned token, consumed on commit.
    pub token: String,
    /// Session owning this preview.
    pub session_id: String,
    /// Source-linked step being applied.
    pub step_id: String,
    /// Exact validated repository-relative target path.
    pub path: PathBuf,
    /// Canonical zero-based Unicode-scalar replacement coordinates.
    pub range: TextRange,
    /// Exact visible pre-image checked at confirmation time.
    pub before: String,
    /// Exact original-author replacement.
    pub replacement: String,
    /// SHA-256 of the complete visible buffer at staging time.
    pub buffer_digest: String,
    /// Editor buffer revision at staging time.
    pub buffer_revision: u64,
    /// Session generation at staging time.
    pub generation: u64,
}

/// Optional backwards-compatible state embedded in the editor session snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayRecoverySnapshot {
    /// Durable replay recovery format.
    pub version: u32,
    /// Active reviewer session, when the guide was visible.
    pub active_session: Option<String>,
    /// All editor-owned source-linked reviewer sessions.
    pub sessions: Vec<ReplaySession>,
    /// Monotonic replay state generation.
    pub generation: u64,
}

/// Single-editor owner of source handles, review sessions, and staged edits.
#[derive(Debug)]
pub struct ReplayController {
    limits: ReplayLimits,
    pending_pull_requests: HashMap<String, ReplayResolvedPullRequest>,
    sources: HashMap<String, ReplaySource>,
    workspaces: HashMap<String, ReplayWorkspace>,
    sessions: HashMap<String, ReplaySession>,
    staged: HashMap<String, ReplayStage>,
    active_session: Option<String>,
    generation: u64,
}

impl Default for ReplayController {
    fn default() -> Self {
        Self::new(ReplayLimits::default())
    }
}

impl ReplayController {
    /// Creates one editor-owned replay state controller.
    #[must_use]
    pub fn new(limits: ReplayLimits) -> Self {
        Self {
            limits,
            pending_pull_requests: HashMap::new(),
            sources: HashMap::new(),
            workspaces: HashMap::new(),
            sessions: HashMap::new(),
            staged: HashMap::new(),
            active_session: None,
            generation: 0,
        }
    }

    /// Returns the source and reviewer-content bounds.
    #[must_use]
    pub const fn limits(&self) -> ReplayLimits {
        self.limits
    }

    /// Records validated read-only PR metadata before any fetch or worktree.
    pub fn register_pull_request(&mut self, request: ReplayResolvedPullRequest) {
        self.pending_pull_requests
            .insert(request.source_id.clone(), request);
        self.advance_generation();
    }

    /// Records an independently resolved, locally available revision.
    pub fn register_source(&mut self, source: ReplaySource) {
        self.sources.insert(source.id.clone(), source);
        self.advance_generation();
    }

    /// Returns the trusted metadata for a pending original pull request.
    pub fn pending_pull_request(
        &self,
        source_id: &str,
    ) -> Result<&ReplayResolvedPullRequest, ReplayError> {
        self.pending_pull_requests
            .get(source_id)
            .ok_or_else(|| missing("replay source", source_id))
    }

    /// Fetches only pinned source objects after explicit editor confirmation.
    pub fn fetch_source(
        &mut self,
        source_id: &str,
        confirmed: bool,
    ) -> Result<ReplaySource, ReplayError> {
        let pending = self
            .pending_pull_requests
            .get_mut(source_id)
            .ok_or_else(|| missing("replay source", source_id))?;
        fetch_pull_request_objects(pending, confirmed)?;
        let source = finalize_pull_request(pending, self.limits)?;
        self.sources.insert(source.id.clone(), source.clone());
        self.advance_generation();
        Ok(source)
    }

    /// Produces or verifies the complete immutable source for a source handle.
    pub fn source(&mut self, source_id: &str) -> Result<&ReplaySource, ReplayError> {
        if !self.sources.contains_key(source_id) {
            let pending = self
                .pending_pull_requests
                .get(source_id)
                .ok_or_else(|| missing("replay source", source_id))?;
            let source = finalize_pull_request(pending, self.limits)?;
            self.sources.insert(source_id.to_string(), source);
        }
        self.sources
            .get(source_id)
            .ok_or_else(|| missing("replay source", source_id))
    }

    /// Previews or explicitly creates a durable named scratch review worktree.
    pub fn prepare_workspace(
        &mut self,
        source_id: &str,
        confirmed: bool,
    ) -> Result<(ReplayWorkspacePreview, Option<ReplayWorkspace>), ReplayError> {
        let source = self.source(source_id)?.clone();
        let (preview, workspace) = prepare_workspace(&source, confirmed)?;
        if let Some(workspace) = workspace.as_ref() {
            self.workspaces
                .insert(source_id.to_string(), workspace.clone());
            self.advance_generation();
        }
        Ok((preview, workspace))
    }

    /// Registers a scratch worktree verified by the bounded background worker.
    ///
    /// The source handle, expected durable sibling path, branch, and immutable
    /// merge base are checked again before editor state adopts the worktree.
    pub(crate) fn adopt_workspace(
        &mut self,
        source_id: &str,
        workspace: ReplayWorkspace,
    ) -> Result<(), ReplayError> {
        let source = self.source(source_id)?.clone();
        let (preview, _) = prepare_workspace(&source, /*confirmed*/ false)?;
        if workspace.root != preview.root
            || workspace.branch != preview.branch
            || workspace.base_commit != preview.base_commit
        {
            return Err(ReplayError::WorkspaceExists(
                workspace.root.display().to_string(),
            ));
        }
        self.workspaces.insert(source_id.to_string(), workspace);
        self.advance_generation();
        Ok(())
    }

    /// Creates a deterministic reviewer session for a confirmed workspace.
    pub fn create_session(&mut self, source_id: &str) -> Result<&ReplaySession, ReplayError> {
        let source = self.source(source_id)?.clone();
        let workspace = self
            .workspaces
            .get(source_id)
            .cloned()
            .ok_or(ReplayError::WorkspaceConfirmationRequired)?;
        let session = ReplaySession::from_source(source, workspace, self.limits)?;
        let id = session.id.clone();
        self.sessions.insert(id.clone(), session);
        self.active_session = Some(id.clone());
        self.advance_generation();
        self.session(&id)
    }

    /// Attaches an already verified, editor-owned source session.
    ///
    /// Production sessions are normally installed by [`Self::create_session`].
    /// The editor may also receive a session that was independently constructed
    /// from a pinned source, such as an isolated integration fixture.
    pub(crate) fn adopt_session(&mut self, session: ReplaySession) {
        let id = session.id.clone();
        self.sources
            .entry(session.source.id.clone())
            .or_insert_with(|| session.source.clone());
        self.workspaces
            .entry(session.source.id.clone())
            .or_insert_with(|| session.workspace.clone());
        self.sessions.entry(id.clone()).or_insert(session);
        self.active_session = Some(id);
        self.advance_generation();
    }

    /// Returns a bounded editor-owned session without transferring authority.
    pub fn session(&self, id: &str) -> Result<&ReplaySession, ReplayError> {
        self.sessions
            .get(id)
            .ok_or_else(|| missing("replay session", id))
    }

    /// Lists all recoverable review sessions.
    #[must_use]
    pub fn sessions(&self) -> Vec<&ReplaySession> {
        let mut sessions = self.sessions.values().collect::<Vec<_>>();
        sessions.sort_by(|left, right| left.id.cmp(&right.id));
        sessions
    }

    /// Selects an existing eligible learning step.
    pub fn activate_step(
        &mut self,
        session_id: &str,
        step_id: &str,
    ) -> Result<&ReplayStep, ReplayError> {
        let session = self.session_mut(session_id)?;
        let index = session
            .steps
            .iter()
            .position(|step| step.id == step_id)
            .ok_or_else(|| missing("replay step", step_id))?;
        if !session.dependencies_complete(index) {
            return Err(ReplayError::DependencyBlocked);
        }
        session.steps[index].status = ReplayStepStatus::Active;
        session.active_step = Some(step_id.to_string());
        session.state = ReplaySessionState::Active;
        session.generation = session.generation.saturating_add(1);
        self.generation = self.generation.saturating_add(1);
        self.sessions
            .get(session_id)
            .and_then(|session| session.steps.iter().find(|step| step.id == step_id))
            .ok_or_else(|| missing("replay step", step_id))
    }

    /// Validates only the correct file and uniquely anchored source occurrence.
    pub fn validate_step(
        &self,
        session_id: &str,
        step_id: &str,
        path: &Path,
        text: &str,
    ) -> Result<ReplayValidation, ReplayError> {
        let session = self.session(session_id)?;
        let step = session
            .steps
            .iter()
            .find(|step| step.id == step_id)
            .ok_or_else(|| missing("replay step", step_id))?;
        if !session.is_step_eligible(step) {
            return Ok(ReplayValidation::Blocked);
        }
        if step.path != path {
            return Ok(ReplayValidation::Conflict);
        }
        Ok(validate_text(
            step,
            text,
            effective_old_start(session, step),
        ))
    }

    /// Issues one immutable, session-owned preview without changing an editor buffer.
    pub fn stage_step(
        &mut self,
        session_id: &str,
        step_id: &str,
        path: &Path,
        text: &str,
        buffer_revision: u64,
    ) -> Result<ReplayStage, ReplayError> {
        let session = self.session(session_id)?;
        let step = session
            .steps
            .iter()
            .find(|step| step.id == step_id)
            .ok_or_else(|| missing("replay step", step_id))?;
        if !session.is_step_eligible(step) {
            return Err(ReplayError::DependencyBlocked);
        }
        if step.path != path {
            return Err(ReplayError::AnchorConflict);
        }
        let range = anchored_range(step, text, effective_old_start(session, step))?;
        let stage = ReplayStage {
            token: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            step_id: step_id.to_string(),
            path: path.to_path_buf(),
            range,
            before: step.before.clone(),
            replacement: step.after.clone(),
            buffer_digest: digest(text.as_bytes()),
            buffer_revision,
            generation: session.generation,
        };
        self.staged.insert(stage.token.clone(), stage.clone());
        Ok(stage)
    }

    /// Consumes a stage exactly once and revalidates the complete visible buffer.
    pub fn consume_stage(
        &mut self,
        token: &str,
        path: &Path,
        text: &str,
        buffer_revision: u64,
    ) -> Result<ReplayStage, ReplayError> {
        let stage = self.staged.remove(token).ok_or(ReplayError::StalePreview)?;
        let session = self.session(&stage.session_id)?;
        if stage.path != path
            || stage.buffer_revision != buffer_revision
            || stage.buffer_digest != digest(text.as_bytes())
            || stage.generation != session.generation
        {
            return Err(ReplayError::StalePreview);
        }
        let step = session
            .steps
            .iter()
            .find(|step| step.id == stage.step_id)
            .ok_or(ReplayError::StalePreview)?;
        if anchored_range(step, text, effective_old_start(session, step))? != stage.range {
            return Err(ReplayError::StalePreview);
        }
        Ok(stage)
    }

    /// Marks a uniquely validated exercise manual or explicitly auto-applied.
    pub fn complete_step(
        &mut self,
        session_id: &str,
        step_id: &str,
        completion: ReplayCompletion,
    ) -> Result<(), ReplayError> {
        let session = self.session_mut(session_id)?;
        let step = session
            .steps
            .iter_mut()
            .find(|step| step.id == step_id)
            .ok_or_else(|| missing("replay step", step_id))?;
        step.status = ReplayStepStatus::Done;
        step.completion = Some(completion);
        session.generation = session.generation.saturating_add(1);
        if session.steps.iter().all(|step| {
            matches!(
                step.status,
                ReplayStepStatus::Done | ReplayStepStatus::Skipped
            )
        }) {
            session.state = ReplaySessionState::Completed;
        }
        self.advance_generation();
        Ok(())
    }

    /// Restores an automatically undone hunk to active, recoverable progress.
    ///
    /// A completed dependent step prevents reopening its prerequisite; the
    /// caller must undo or reopen the dependent original change first.
    pub fn reopen_step(&mut self, session_id: &str, step_id: &str) -> Result<(), ReplayError> {
        let session = self.session_mut(session_id)?;
        if session.steps.iter().any(|candidate| {
            candidate.status == ReplayStepStatus::Done
                && candidate
                    .dependencies
                    .iter()
                    .any(|dependency| dependency == step_id)
        }) {
            return Err(ReplayError::DependencyBlocked);
        }

        let index = session
            .steps
            .iter()
            .position(|step| step.id == step_id)
            .ok_or_else(|| missing("replay step", step_id))?;
        session.steps[index].status = ReplayStepStatus::Active;
        session.steps[index].completion = None;
        session.active_step = Some(step_id.to_string());
        session.state = ReplaySessionState::Active;
        session.generation = session.generation.saturating_add(1);
        self.advance_generation();
        Ok(())
    }

    /// Skips one exercise without silently applying its blocked dependencies.
    pub fn skip_step(&mut self, session_id: &str, step_id: &str) -> Result<(), ReplayError> {
        let session = self.session_mut(session_id)?;
        let index = session
            .steps
            .iter()
            .position(|step| step.id == step_id)
            .ok_or_else(|| missing("replay step", step_id))?;
        session.steps[index].status = ReplayStepStatus::Skipped;
        for step in &mut session.steps {
            if step
                .dependencies
                .iter()
                .any(|dependency| dependency == step_id)
                && step.status == ReplayStepStatus::Pending
            {
                step.status = ReplayStepStatus::Blocked;
            }
        }
        session.generation = session.generation.saturating_add(1);
        self.advance_generation();
        Ok(())
    }

    /// Sets the skill-compatible reviewer learning mode.
    pub fn set_mode(&mut self, session_id: &str, mode: ReplayMode) -> Result<(), ReplayError> {
        let session = self.session_mut(session_id)?;
        session.mode = mode;
        session.generation = session.generation.saturating_add(1);
        self.advance_generation();
        Ok(())
    }

    /// Records a bounded, source-linked finding without contacting GitHub.
    pub fn add_note(
        &mut self,
        session_id: &str,
        step_id: Option<&str>,
        category: ReplayNoteCategory,
        text: &str,
    ) -> Result<ReplayNote, ReplayError> {
        let text = text.trim();
        if text.is_empty() || text.len() > self.limits.max_note_bytes {
            return Err(ReplayError::InvalidReviewNote(
                "observation must be nonempty and within the configured limit".to_string(),
            ));
        }
        let session = self.session_mut(session_id)?;
        let path = if let Some(id) = step_id {
            Some(
                session
                    .steps
                    .iter()
                    .find(|step| step.id == id)
                    .ok_or_else(|| missing("replay step", id))?
                    .path
                    .clone(),
            )
        } else {
            None
        };
        let note = ReplayNote {
            id: uuid::Uuid::new_v4().to_string(),
            target_commit: session.source.target_commit.clone(),
            step_id: step_id.map(str::to_string),
            path,
            category,
            text: text.to_string(),
            created_at_ms: now_ms(),
        };
        session.notes.push(note.clone());
        session.generation = session.generation.saturating_add(1);
        self.advance_generation();
        Ok(note)
    }

    /// Hides a session without discarding its scratch branch or reviewer findings.
    pub fn pause(&mut self, session_id: &str) -> Result<(), ReplayError> {
        let session = self.session_mut(session_id)?;
        session.state = ReplaySessionState::Paused;
        session.generation = session.generation.saturating_add(1);
        self.staged
            .retain(|_, stage| stage.session_id != session_id);
        self.advance_generation();
        Ok(())
    }

    /// Returns recoverable source and reviewer state, excluding one-shot tokens.
    #[must_use]
    pub fn recovery_snapshot(&self) -> Option<ReplayRecoverySnapshot> {
        (!self.sessions.is_empty()).then(|| ReplayRecoverySnapshot {
            version: 1,
            active_session: self.active_session.clone(),
            sessions: self.sessions().into_iter().cloned().collect(),
            generation: self.generation,
        })
    }

    /// Restores verified editor-owned sessions without reusing staged tokens.
    pub fn restore(&mut self, snapshot: &ReplayRecoverySnapshot) -> Result<(), ReplayError> {
        if snapshot.version != 1 {
            return Err(ReplayError::InvalidMetadata(
                "unsupported replay recovery version".to_string(),
            ));
        }
        self.staged.clear();
        self.sessions.clear();
        self.sources.clear();
        self.workspaces.clear();
        for session in &snapshot.sessions {
            self.sources
                .insert(session.source.id.clone(), session.source.clone());
            self.workspaces
                .insert(session.source.id.clone(), session.workspace.clone());
            self.sessions.insert(session.id.clone(), session.clone());
        }
        self.active_session = snapshot
            .active_session
            .as_ref()
            .filter(|id| self.sessions.contains_key(id.as_str()))
            .cloned();
        self.generation = snapshot.generation;
        Ok(())
    }

    /// Returns the monotonic generation required by the session writer.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    fn session_mut(&mut self, id: &str) -> Result<&mut ReplaySession, ReplayError> {
        self.sessions
            .get_mut(id)
            .ok_or_else(|| missing("replay session", id))
    }

    fn advance_generation(&mut self) {
        self.generation = self.generation.saturating_add(1);
    }
}

impl ReplaySession {
    /// Compiles complete source-backed exercises without changing the worktree.
    pub fn from_source(
        source: ReplaySource,
        workspace: ReplayWorkspace,
        limits: ReplayLimits,
    ) -> Result<Self, ReplayError> {
        let patch = parse_patch(&source.patch, limits)?;
        let mut steps = Vec::new();
        let mut informational_changes = Vec::new();
        let mut prior_by_path: HashMap<PathBuf, String> = HashMap::new();

        for file in patch.files {
            let Some(path) = file.path().map(Path::to_path_buf) else {
                continue;
            };
            if !file.kind.supports_text_replay() {
                informational_changes.push(path);
                continue;
            }
            for hunk in file.hunks {
                if steps.len() >= limits.max_steps {
                    return Err(ReplayError::LimitExceeded {
                        kind: "replay steps",
                        limit: limits.max_steps,
                    });
                }
                let kind = if file.kind == ReplayChangeKind::AddFile {
                    ReplayStepKind::AddFile
                } else if hunk.removed_lines == 0 {
                    ReplayStepKind::Add
                } else if hunk.added_lines == 0 {
                    ReplayStepKind::Remove
                } else {
                    ReplayStepKind::Change
                };
                let original = format!(
                    "{}\0{}\0{}\0{}\0{}",
                    source.patch_digest,
                    path.display(),
                    hunk.header,
                    hunk.before,
                    hunk.after
                );
                let hunk_digest = digest(original.as_bytes());
                let id =
                    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, original.as_bytes()).to_string();
                let dependencies = prior_by_path.get(&path).cloned().into_iter().collect();
                prior_by_path.insert(path.clone(), id.clone());
                steps.push(ReplayStep {
                    id,
                    ordinal: steps.len() + 1,
                    kind,
                    path: path.clone(),
                    target_commit: source.target_commit.clone(),
                    hunk_digest,
                    heading: hunk.heading,
                    before: hunk.before,
                    after: hunk.after,
                    old_start: hunk.old_range.start,
                    dependencies,
                    status: ReplayStepStatus::Pending,
                    completion: None,
                });
            }
        }

        Ok(Self {
            id: uuid::Uuid::new_v4().to_string(),
            source,
            workspace,
            mode: ReplayMode::Challenge,
            state: ReplaySessionState::Ready,
            active_step: None,
            steps,
            informational_changes,
            notes: Vec::new(),
            generation: 0,
        })
    }

    fn dependencies_complete(&self, index: usize) -> bool {
        self.steps[index].dependencies.iter().all(|dependency| {
            self.steps.iter().any(|candidate| {
                candidate.id == *dependency && candidate.status == ReplayStepStatus::Done
            })
        })
    }

    fn is_step_eligible(&self, step: &ReplayStep) -> bool {
        step.dependencies.iter().all(|dependency| {
            self.steps.iter().any(|candidate| {
                candidate.id == *dependency && candidate.status == ReplayStepStatus::Done
            })
        })
    }
}

fn validate_text(step: &ReplayStep, text: &str, old_start: usize) -> ReplayValidation {
    if step.before.is_empty() {
        if text == step.after || (text.ends_with('\n') && text.trim_end_matches('\n') == step.after)
        {
            return ReplayValidation::Exact;
        }
        if text.is_empty() || text == "\n" {
            return ReplayValidation::Incomplete;
        }
        return ReplayValidation::Conflict;
    }
    if !step.after.is_empty() {
        match anchored_hunk_offset(text, &step.after, old_start) {
            Ok(_) => return ReplayValidation::Exact,
            Err(ReplayError::AnchorConflict)
                if has_equidistant_hunk_candidates(text, &step.after, old_start) =>
            {
                return ReplayValidation::Ambiguous;
            }
            Err(_) => {}
        }
    }

    match anchored_hunk_offset(text, &step.before, old_start) {
        Ok(_) => ReplayValidation::Incomplete,
        Err(ReplayError::AnchorConflict)
            if has_equidistant_hunk_candidates(text, &step.before, old_start) =>
        {
            ReplayValidation::Ambiguous
        }
        Err(_) => ReplayValidation::Conflict,
    }
}

fn effective_old_start(session: &ReplaySession, step: &ReplayStep) -> usize {
    let line_delta = session
        .steps
        .iter()
        .take_while(|candidate| candidate.id != step.id)
        .filter(|candidate| {
            candidate.path == step.path && candidate.status == ReplayStepStatus::Done
        })
        .fold(0_isize, |delta, candidate| {
            delta.saturating_add(replay_line_delta(&candidate.before, &candidate.after))
        });
    step.old_start.saturating_add_signed(line_delta).max(1)
}

pub(super) fn replay_line_delta(before: &str, after: &str) -> isize {
    let before_lines = before.bytes().filter(|byte| *byte == b'\n').count();
    let after_lines = after.bytes().filter(|byte| *byte == b'\n').count();
    let before_lines = isize::try_from(before_lines).unwrap_or(isize::MAX);
    let after_lines = isize::try_from(after_lines).unwrap_or(isize::MAX);
    after_lines.saturating_sub(before_lines)
}

fn has_equidistant_hunk_candidates(text: &str, pattern: &str, old_start: usize) -> bool {
    if pattern.is_empty() {
        return false;
    }

    let expected_line = old_start.max(1);
    let mut previous_offset = 0;
    let mut current_line = 1_usize;
    let mut minimum_distance = usize::MAX;
    let mut closest_count = 0_usize;

    for (offset, _) in text.match_indices(pattern) {
        current_line = current_line.saturating_add(
            text[previous_offset..offset]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count(),
        );
        previous_offset = offset;
        if offset != 0 && text.as_bytes()[offset - 1] != b'\n' {
            continue;
        }

        let end = offset + pattern.len();
        if !pattern.ends_with('\n') && end != text.len() && text.as_bytes().get(end) != Some(&b'\n')
        {
            continue;
        }

        let distance = current_line.abs_diff(expected_line);
        match distance.cmp(&minimum_distance) {
            std::cmp::Ordering::Less => {
                minimum_distance = distance;
                closest_count = 1;
            }
            std::cmp::Ordering::Equal => closest_count = closest_count.saturating_add(1),
            std::cmp::Ordering::Greater => {}
        }
    }

    closest_count > 1
}

pub(crate) fn anchored_hunk_offset(
    text: &str,
    pattern: &str,
    old_start: usize,
) -> Result<usize, ReplayError> {
    if pattern.is_empty() {
        return Err(ReplayError::AnchorConflict);
    }

    let expected_line = old_start.max(1);
    let mut previous_offset = 0;
    let mut current_line = 1_usize;
    let mut closest: Option<(usize, usize)> = None;
    let mut tied = false;

    for (offset, _) in text.match_indices(pattern) {
        current_line = current_line.saturating_add(
            text[previous_offset..offset]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count(),
        );
        previous_offset = offset;
        if offset != 0 && text.as_bytes()[offset - 1] != b'\n' {
            continue;
        }

        let end = offset + pattern.len();
        if !pattern.ends_with('\n') && end != text.len() && text.as_bytes().get(end) != Some(&b'\n')
        {
            continue;
        }

        let distance = current_line.abs_diff(expected_line);
        match closest {
            None => {
                closest = Some((distance, offset));
                tied = false;
            }
            Some((best_distance, _)) if distance < best_distance => {
                closest = Some((distance, offset));
                tied = false;
            }
            Some((best_distance, _)) if distance == best_distance => tied = true,
            Some(_) => {}
        }
    }

    if tied {
        return Err(ReplayError::AnchorConflict);
    }
    closest
        .map(|(_, offset)| offset)
        .ok_or(ReplayError::AnchorConflict)
}

fn anchored_range(
    step: &ReplayStep,
    text: &str,
    old_start: usize,
) -> Result<TextRange, ReplayError> {
    if step.before.is_empty() {
        if text.is_empty() || text == "\n" {
            return Ok(TextRange::insertion(TextPosition::new(0, 0)));
        }
        return Err(ReplayError::AnchorConflict);
    }
    let start = anchored_hunk_offset(text, &step.before, old_start)?;
    Ok(TextRange::new(
        text_position_at_byte(text, start),
        text_position_at_byte(text, start + step.before.len()),
    ))
}

fn text_position_at_byte(text: &str, offset: usize) -> TextPosition {
    let prefix = &text[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let line_start = prefix.rfind('\n').map_or(0, |position| position + 1);
    let character = prefix[line_start..].chars().count();
    TextPosition::new(line, character)
}

fn missing(kind: &'static str, id: &str) -> ReplayError {
    ReplayError::NotFound {
        kind,
        id: id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::{ReplayRepository, ReplaySourceKind};

    const CHANGE: &str = "diff --git a/src/token.rs b/src/token.rs\nindex 1111111..2222222 100644\n--- a/src/token.rs\n+++ b/src/token.rs\n@@ -1,3 +1,3 @@ fn refresh\n fn refresh() {\n-    old();\n+    new();\n }\n";

    fn sample_source(patch: &str) -> ReplaySource {
        let base = GitObjectId::parse(&"a".repeat(40)).unwrap();
        let target = GitObjectId::parse(&"b".repeat(40)).unwrap();
        ReplaySource {
            id: "source".to_string(),
            repository: ReplayRepository {
                root: PathBuf::from("/workspace/repository"),
                common_directory: PathBuf::from("/workspace/repository/.git"),
                host: "github.com".to_string(),
                owner: "owner".to_string(),
                name: "repository".to_string(),
            },
            kind: ReplaySourceKind::LocalRevision,
            base_commit: base,
            target_commit: target,
            patch: patch.to_string(),
            patch_digest: digest(patch.as_bytes()),
            pull_request: None,
            review_context: None,
        }
    }

    fn sample_session() -> ReplaySession {
        let source = sample_source(CHANGE);
        let workspace = ReplayWorkspace {
            root: PathBuf::from("/workspace/repository.replay"),
            branch: "replay/test".to_string(),
            base_commit: source.base_commit.clone(),
            created_by_replay: true,
        };
        ReplaySession::from_source(source, workspace, ReplayLimits::default()).unwrap()
    }

    fn controller_with_session() -> (ReplayController, String, String) {
        let session = sample_session();
        let session_id = session.id.clone();
        let step_id = session.steps[0].id.clone();
        let mut controller = ReplayController::default();
        controller.sessions.insert(session_id.clone(), session);
        (controller, session_id, step_id)
    }

    #[test]
    fn compiles_deterministic_source_linked_change_steps() {
        let first = sample_session();
        let second = sample_session();
        assert_eq!(first.steps[0].id, second.steps[0].id);
        assert_eq!(first.steps[0].kind, ReplayStepKind::Change);
        assert_eq!(first.mode, ReplayMode::Challenge);
    }

    #[test]
    fn validates_the_correct_unique_source_occurrence() {
        let (controller, session, step) = controller_with_session();
        let old = "fn refresh() {\n    old();\n}\n";
        let new = "fn refresh() {\n    new();\n}\n";
        assert_eq!(
            controller
                .validate_step(&session, &step, Path::new("src/token.rs"), old)
                .unwrap(),
            ReplayValidation::Incomplete
        );
        assert_eq!(
            controller
                .validate_step(&session, &step, Path::new("src/token.rs"), new)
                .unwrap(),
            ReplayValidation::Exact
        );
    }

    #[test]
    fn refuses_matching_text_in_the_wrong_source_file() {
        let (controller, session, step) = controller_with_session();
        let new = "fn refresh() {\n    new();\n}\n";
        assert_eq!(
            controller
                .validate_step(&session, &step, Path::new("src/other.rs"), new)
                .unwrap(),
            ReplayValidation::Conflict
        );
    }

    #[test]
    fn repeated_original_context_uses_the_pinned_hunk_line() {
        let (mut controller, session, step) = controller_with_session();
        let old = "fn refresh() {\n    old();\n}\n";
        let duplicate = format!("{old}{old}");
        let stage = controller
            .stage_step(
                &session,
                &step,
                Path::new("src/token.rs"),
                &duplicate,
                /*buffer_revision*/ 0,
            )
            .expect("the original hunk line selects the first repeated context");

        assert_eq!(
            stage.range.start,
            TextPosition::new(/*line*/ 0, /*character*/ 0)
        );
        assert_eq!(stage.before, old);
        assert_eq!(
            controller
                .validate_step(&session, &step, Path::new("src/token.rs"), &duplicate)
                .unwrap(),
            ReplayValidation::Incomplete,
        );
    }

    #[test]
    fn genuinely_equidistant_repeated_original_context_remains_ambiguous() {
        let (mut controller, session, step) = controller_with_session();
        controller
            .sessions
            .get_mut(&session)
            .expect("registered isolated replay session")
            .steps[0]
            .old_start = 3;
        let old = "fn refresh() {\n    old();\n}\n";
        let duplicate = format!("{old}\n{old}");

        assert!(matches!(
            controller.stage_step(
                &session,
                &step,
                Path::new("src/token.rs"),
                &duplicate,
                /*buffer_revision*/ 0,
            ),
            Err(ReplayError::AnchorConflict),
        ));
        assert_eq!(
            controller
                .validate_step(&session, &step, Path::new("src/token.rs"), &duplicate)
                .unwrap(),
            ReplayValidation::Ambiguous,
        );
    }

    #[test]
    fn preview_tokens_are_single_use_and_revision_checked() {
        let (mut controller, session, step) = controller_with_session();
        let old = "fn refresh() {\n    old();\n}\n";
        let stage = controller
            .stage_step(&session, &step, Path::new("src/token.rs"), old, 7)
            .unwrap();
        controller
            .consume_stage(&stage.token, Path::new("src/token.rs"), old, 7)
            .unwrap();
        assert!(matches!(
            controller.consume_stage(&stage.token, Path::new("src/token.rs"), old, 7),
            Err(ReplayError::StalePreview)
        ));
    }

    #[test]
    fn a_changed_visible_buffer_invalidates_a_preview() {
        let (mut controller, session, step) = controller_with_session();
        let old = "fn refresh() {\n    old();\n}\n";
        let stage = controller
            .stage_step(&session, &step, Path::new("src/token.rs"), old, 7)
            .unwrap();
        assert!(matches!(
            controller.consume_stage(
                &stage.token,
                Path::new("src/token.rs"),
                "changed elsewhere",
                7
            ),
            Err(ReplayError::StalePreview)
        ));
    }

    #[test]
    fn undone_original_hunk_reopens_recoverable_session_progress() {
        let (mut controller, session_id, step_id) = controller_with_session();
        controller
            .complete_step(&session_id, &step_id, ReplayCompletion::Automatic)
            .expect("complete the exact automatically applied original hunk");
        assert_eq!(
            controller.session(&session_id).unwrap().state,
            ReplaySessionState::Completed,
        );

        controller
            .reopen_step(&session_id, &step_id)
            .expect("return the undone original hunk to recoverable active progress");

        let session = controller.session(&session_id).unwrap();
        assert_eq!(session.state, ReplaySessionState::Active);
        assert_eq!(session.active_step.as_deref(), Some(step_id.as_str()));
        assert_eq!(session.steps[0].status, ReplayStepStatus::Active);
        assert_eq!(session.steps[0].completion, None);
        let recovered = controller
            .recovery_snapshot()
            .expect("preserve reopened replay progress in crash recovery");
        assert_eq!(
            recovered.sessions[0].steps[0].status,
            ReplayStepStatus::Active
        );
    }

    #[test]
    fn never_reopens_an_original_hunk_beneath_a_completed_dependent() {
        let (mut controller, session_id, first_id) = controller_with_session();
        let second_id = "dependent-original-hunk".to_string();
        let session = controller.sessions.get_mut(&session_id).unwrap();
        let mut dependent = session.steps[0].clone();
        dependent.id = second_id.clone();
        dependent.ordinal = 2;
        dependent.dependencies = vec![first_id.clone()];
        dependent.status = ReplayStepStatus::Pending;
        dependent.completion = None;
        session.steps.push(dependent);
        controller
            .complete_step(&session_id, &first_id, ReplayCompletion::Automatic)
            .expect("complete the original prerequisite");
        controller
            .complete_step(&session_id, &second_id, ReplayCompletion::Manual)
            .expect("retain the reviewer-authored dependent change");

        assert!(matches!(
            controller.reopen_step(&session_id, &first_id),
            Err(ReplayError::DependencyBlocked),
        ));
        let session = controller.session(&session_id).unwrap();
        assert_eq!(session.steps[0].status, ReplayStepStatus::Done);
        assert_eq!(session.steps[1].status, ReplayStepStatus::Done);
        assert_eq!(session.steps[1].completion, Some(ReplayCompletion::Manual));
    }

    #[test]
    fn adopts_only_the_background_worktree_for_its_pinned_source() {
        let original = sample_session();
        let source_id = original.source.id.clone();
        let mut controller = ReplayController::default();
        controller.register_source(original.source.clone());
        let (preview, _) = controller
            .prepare_workspace(&source_id, /*confirmed*/ false)
            .expect("derive the exact original scratch-worktree identity");
        let workspace = ReplayWorkspace {
            root: preview.root,
            branch: preview.branch,
            base_commit: preview.base_commit,
            created_by_replay: true,
        };

        controller
            .adopt_workspace(&source_id, workspace.clone())
            .expect("adopt only the verified original background worktree");

        let session = controller
            .create_session(&source_id)
            .expect("create a session from the editor-owned pinned source");
        assert_eq!(session.workspace, workspace);
    }

    #[test]
    fn rejects_a_background_worktree_for_an_unrelated_source_path() {
        let original = sample_session();
        let source_id = original.source.id.clone();
        let mut controller = ReplayController::default();
        controller.register_source(original.source.clone());
        let (preview, _) = controller
            .prepare_workspace(&source_id, /*confirmed*/ false)
            .expect("derive the exact pinned original worktree identity");
        let foreign = ReplayWorkspace {
            root: PathBuf::from("/workspace/unrelated-scratch"),
            branch: preview.branch,
            base_commit: preview.base_commit,
            created_by_replay: true,
        };

        assert!(matches!(
            controller.adopt_workspace(&source_id, foreign),
            Err(ReplayError::WorkspaceExists(_)),
        ));
        assert!(matches!(
            controller.create_session(&source_id),
            Err(ReplayError::WorkspaceConfirmationRequired),
        ));
    }

    #[test]
    fn review_observations_stay_linked_to_the_original_head() {
        let (mut controller, session, step) = controller_with_session();
        let note = controller
            .add_note(
                &session,
                Some(&step),
                ReplayNoteCategory::TestGap,
                "Add a refresh-token regression test.",
            )
            .unwrap();
        assert_eq!(note.path.as_deref(), Some(Path::new("src/token.rs")));
        assert_eq!(
            note.target_commit,
            GitObjectId::parse(&"b".repeat(40)).unwrap()
        );
    }

    #[test]
    fn recovery_preserves_notes_but_never_restores_stage_tokens() {
        let (mut controller, session, step) = controller_with_session();
        let old = "fn refresh() {\n    old();\n}\n";
        let stage = controller
            .stage_step(&session, &step, Path::new("src/token.rs"), old, 7)
            .unwrap();
        controller
            .add_note(
                &session,
                Some(&step),
                ReplayNoteCategory::Question,
                "Why rotate?",
            )
            .unwrap();
        let snapshot = controller.recovery_snapshot().unwrap();
        let mut restored = ReplayController::default();
        restored.restore(&snapshot).unwrap();
        assert_eq!(restored.session(&session).unwrap().notes.len(), 1);
        assert!(matches!(
            restored.consume_stage(&stage.token, Path::new("src/token.rs"), old, 7),
            Err(ReplayError::StalePreview)
        ));
    }

    #[test]
    fn unicode_anchor_positions_use_scalar_indices() {
        let text = "é👋\n漢token";
        let position = text_position_at_byte(text, text.find("token").unwrap());
        assert_eq!(position, TextPosition::new(1, 1));
    }

    #[test]
    fn rejects_empty_and_oversized_review_observations() {
        let (mut controller, session, step) = controller_with_session();
        assert!(controller
            .add_note(&session, Some(&step), ReplayNoteCategory::Question, " ")
            .is_err());
        assert!(controller
            .add_note(
                &session,
                Some(&step),
                ReplayNoteCategory::Question,
                &"x".repeat(ReplayLimits::default().max_note_bytes + 1)
            )
            .is_err());
    }
}
