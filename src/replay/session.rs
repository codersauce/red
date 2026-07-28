//! Editor-owned replay sessions, source-linked observations, and one-shot stages.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::undo::{TextPosition, TextRange};

use super::{
    digest, fetch_pull_request_objects, finalize_pull_request, now_ms, parse_patch,
    prepare_author_workspace, prepare_workspace, GitObjectId, ReplayAuthorWorkspace,
    ReplayAuthorWorkspacePreview, ReplayChangeKind, ReplayError, ReplayLimits, ReplayPullRequest,
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

/// Authenticated relationship between the current viewer and the original PR.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayReviewRole {
    /// Treat unknown and nonauthor identities as read-only PR reviewers.
    #[default]
    Reviewer,
    /// The verified GitHub viewer is the original pull-request author.
    Author,
}

impl ReplayReviewRole {
    /// Classifies only an authenticated viewer of the exact pinned PR.
    #[must_use]
    pub fn from_pull_request(request: Option<&ReplayPullRequest>) -> Self {
        request
            .and_then(|request| {
                request
                    .author
                    .as_deref()
                    .zip(request.capabilities.viewer.as_deref())
            })
            .filter(|(author, viewer)| author.eq_ignore_ascii_case(viewer))
            .map_or(Self::Reviewer, |_| Self::Author)
    }
}

/// Outcome a reviewer is preparing without contacting GitHub.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayReviewDraftKind {
    /// An inline review comment anchored to the original GitHub diff.
    InlineComment,
    /// A proposed original-PR fix retained as text until a later approval.
    CodeFix,
    /// A pull-request-level review observation or summary.
    ReviewSummary,
}

/// Whether a local draft was composed by the reviewer or an approved agent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayDraftOrigin {
    /// The reviewer composed this draft directly.
    #[default]
    Human,
    /// An agent proposed this draft for later reviewer inspection.
    Agent,
}

/// Honest publication state of an editor-owned review draft.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayDraftState {
    /// The draft exists only in the local, recoverable Replay session.
    #[default]
    Local,
    /// GitHub accepted this draft in a verified, explicitly submitted review.
    Submitted,
}

/// Original GitHub diff image to which an inline review draft belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayDiffSide {
    /// A deleted original line from the PR merge-base image.
    Left,
    /// An added or replacement line from the exact original PR head.
    Right,
}

/// Immutable original-source coordinates retained for a future inline comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayReviewAnchor {
    /// Exact original PR head; never the mutable learning scratch branch.
    pub target_commit: GitObjectId,
    /// Safe repository-relative file for the selected original diff side.
    pub path: PathBuf,
    /// Original base-side path when the source exposes one.
    pub old_path: Option<PathBuf>,
    /// Original GitHub diff side, not the current scratch-buffer position.
    pub side: ReplayDiffSide,
    /// One-based first original changed line on the selected diff side.
    pub start_line: usize,
    /// One-based last original changed line on the selected diff side.
    pub end_line: usize,
    /// Stable exact original source-hunk identity.
    pub hunk_digest: String,
}

/// Reviewer-owned intended outcome, never a remote GitHub pending review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayReviewDraft {
    /// Opaque, stable local draft identity.
    pub id: String,
    /// Exact original PR head to which the intended outcome belongs.
    pub target_commit: GitObjectId,
    /// Original replay step for an inline comment or proposed code fix.
    pub step_id: Option<String>,
    /// Safe original repository-relative path for a source-anchored draft.
    pub path: Option<PathBuf>,
    /// Whether the draft is an inline comment, fix, or PR-level summary.
    pub kind: ReplayReviewDraftKind,
    /// Whether the human reviewer or an agent produced the draft.
    #[serde(default)]
    pub origin: ReplayDraftOrigin,
    /// Local-only publication state; never implies a remote GitHub review.
    #[serde(default)]
    pub state: ReplayDraftState,
    /// Verified original diff coordinates for source-anchored outcomes.
    pub anchor: Option<ReplayReviewAnchor>,
    /// Bounded reviewer-visible draft text.
    pub text: String,
    /// Unix-millisecond original creation time.
    pub created_at_ms: u64,
    /// Unix-millisecond time of the latest local edit.
    pub updated_at_ms: u64,
}

/// Verified reviewer capabilities and durable, strictly local review outbox.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReplayReviewState {
    /// Role derived from the authenticated viewer and immutable PR author.
    pub role: ReplayReviewRole,
    /// Locally persisted comments, proposed fixes, and PR-level summaries.
    pub drafts: Vec<ReplayReviewDraft>,
    /// Verified receipts for reviews explicitly submitted to the original PR.
    #[serde(default)]
    pub receipts: Vec<super::ReplayReviewReceipt>,
    /// Exact durably approved request whose provider result remains unresolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_submission: Option<super::ReplayPendingReviewSubmission>,
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
    /// Authenticated role and local-only, original-source-anchored outbox.
    #[serde(default)]
    pub review: ReplayReviewState,
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
    author_workspaces: HashMap<String, ReplayAuthorWorkspace>,
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
            author_workspaces: HashMap::new(),
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

    /// Previews the verified author's real PR head without changing Git state.
    pub fn preview_author_workspace(
        &self,
        workspace_id: &str,
    ) -> Result<ReplayAuthorWorkspacePreview, ReplayError> {
        let session = self.session(workspace_id)?;
        prepare_author_workspace(&session.source, /*confirmed*/ false).map(|(preview, _)| preview)
    }

    /// Adopts only the bounded worker's exact, independently verified author head.
    pub(crate) fn adopt_author_workspace(
        &mut self,
        workspace_id: &str,
        workspace: ReplayAuthorWorkspace,
    ) -> Result<(), ReplayError> {
        let session = self.session(workspace_id)?;
        let source_id = session.source.id.clone();
        let (preview, _) = prepare_author_workspace(&session.source, /*confirmed*/ false)?;
        if workspace.root != preview.root
            || workspace.branch != preview.branch
            || workspace.head_commit != preview.head_commit
            || workspace.head_repository != preview.head_repository
            || workspace.head_ref != preview.head_ref
        {
            return Err(ReplayError::WorkspaceExists(
                workspace.root.display().to_string(),
            ));
        }
        self.author_workspaces.insert(source_id, workspace);
        self.advance_generation();
        Ok(())
    }

    /// Returns the independently confirmed original-head author worktree.
    pub fn author_workspace(
        &self,
        workspace_id: &str,
    ) -> Result<&ReplayAuthorWorkspace, ReplayError> {
        let session = self.session(workspace_id)?;
        self.author_workspaces
            .get(&session.source.id)
            .ok_or(ReplayError::AuthorWorkspaceConfirmationRequired)
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

    /// Returns the selected original-source review without transferring ownership.
    #[must_use]
    pub fn active_session(&self) -> Option<&ReplaySession> {
        self.active_session
            .as_deref()
            .and_then(|id| self.sessions.get(id))
    }

    /// Remembers a browsed original hunk without applying its prerequisites.
    ///
    /// Reviewers may inspect an independent or currently blocked step without
    /// claiming that its patch is eligible for automatic application.
    pub fn select_step(&mut self, session_id: &str, step_id: &str) -> Result<(), ReplayError> {
        let session = self.session_mut(session_id)?;
        if !session.steps.iter().any(|step| step.id == step_id) {
            return Err(missing("replay step", step_id));
        }
        if session.active_step.as_deref() == Some(step_id) {
            return Ok(());
        }

        session.active_step = Some(step_id.to_string());
        if session.state != ReplaySessionState::Completed {
            session.state = ReplaySessionState::Active;
        }
        session.generation = session.generation.saturating_add(1);
        self.advance_generation();
        Ok(())
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

    /// Creates a durable local review outcome from exact original-source data.
    ///
    /// This never changes a Git ref, starts an agent, or contacts GitHub.
    pub fn add_review_draft(
        &mut self,
        session_id: &str,
        step_id: Option<&str>,
        kind: ReplayReviewDraftKind,
        text: &str,
    ) -> Result<ReplayReviewDraft, ReplayError> {
        let text = text.trim();
        if text.is_empty() || text.len() > self.limits.max_note_bytes {
            return Err(ReplayError::InvalidReviewDraft(
                "draft must be nonempty and within the configured review limit".to_string(),
            ));
        }
        let limits = self.limits;
        let session = self.session_mut(session_id)?;
        if session.review.pending_submission.is_some() {
            return Err(ReplayError::ReviewSubmissionUncertain(
                "reconcile the confirmed GitHub review before changing its approved drafts"
                    .to_string(),
            ));
        }
        if session.review.drafts.len() >= limits.max_steps {
            return Err(ReplayError::LimitExceeded {
                kind: "local review drafts",
                limit: limits.max_steps,
            });
        }
        if kind == ReplayReviewDraftKind::CodeFix && session.review.role != ReplayReviewRole::Author
        {
            return Err(ReplayError::InvalidReviewDraft(
                "proposed PR fixes require a verified original pull-request author".to_string(),
            ));
        }

        let anchor = match kind {
            ReplayReviewDraftKind::InlineComment | ReplayReviewDraftKind::CodeFix => {
                let id = step_id.ok_or_else(|| {
                    ReplayError::InvalidReviewDraft(
                        "source-linked drafts require an exact original replay step".to_string(),
                    )
                })?;
                let step = session
                    .steps
                    .iter()
                    .find(|step| step.id == id)
                    .ok_or_else(|| missing("replay step", id))?;
                Some(session.original_review_anchor(step, limits)?)
            }
            ReplayReviewDraftKind::ReviewSummary => {
                if step_id.is_some() {
                    return Err(ReplayError::InvalidReviewDraft(
                        "a pull-request-level summary cannot claim an inline source anchor"
                            .to_string(),
                    ));
                }
                None
            }
        };
        let created_at_ms = now_ms();
        let draft = ReplayReviewDraft {
            id: uuid::Uuid::new_v4().to_string(),
            target_commit: session.source.target_commit.clone(),
            step_id: step_id.map(str::to_string),
            path: anchor.as_ref().map(|anchor| anchor.path.clone()),
            kind,
            origin: ReplayDraftOrigin::Human,
            state: ReplayDraftState::Local,
            anchor,
            text: text.to_string(),
            created_at_ms,
            updated_at_ms: created_at_ms,
        };
        session.review.drafts.push(draft.clone());
        session.generation = session.generation.saturating_add(1);
        self.advance_generation();
        Ok(draft)
    }

    /// Updates only the bounded text of an existing local review draft.
    pub fn update_review_draft(
        &mut self,
        session_id: &str,
        draft_id: &str,
        text: &str,
    ) -> Result<ReplayReviewDraft, ReplayError> {
        let text = text.trim();
        if text.is_empty() || text.len() > self.limits.max_note_bytes {
            return Err(ReplayError::InvalidReviewDraft(
                "draft must be nonempty and within the configured review limit".to_string(),
            ));
        }
        let session = self.session_mut(session_id)?;
        if session.review.pending_submission.is_some() {
            return Err(ReplayError::ReviewSubmissionUncertain(
                "reconcile the confirmed GitHub review before changing its approved drafts"
                    .to_string(),
            ));
        }
        let draft = session
            .review
            .drafts
            .iter_mut()
            .find(|draft| draft.id == draft_id)
            .ok_or_else(|| missing("local replay review draft", draft_id))?;
        if draft.state != ReplayDraftState::Local {
            return Err(ReplayError::InvalidReviewDraft(
                "a published review comment cannot be edited locally".to_string(),
            ));
        }
        draft.text = text.to_string();
        draft.updated_at_ms = now_ms().max(draft.created_at_ms);
        let draft = draft.clone();
        session.generation = session.generation.saturating_add(1);
        self.advance_generation();
        Ok(draft)
    }

    /// Removes one local outcome without discarding replay or scratch progress.
    pub fn remove_review_draft(
        &mut self,
        session_id: &str,
        draft_id: &str,
    ) -> Result<ReplayReviewDraft, ReplayError> {
        let session = self.session_mut(session_id)?;
        if session.review.pending_submission.is_some() {
            return Err(ReplayError::ReviewSubmissionUncertain(
                "reconcile the confirmed GitHub review before changing its approved drafts"
                    .to_string(),
            ));
        }
        let index = session
            .review
            .drafts
            .iter()
            .position(|draft| draft.id == draft_id)
            .ok_or_else(|| missing("local replay review draft", draft_id))?;
        if session.review.drafts[index].state != ReplayDraftState::Local {
            return Err(ReplayError::InvalidReviewDraft(
                "a published review comment cannot be discarded locally".to_string(),
            ));
        }
        let draft = session.review.drafts.remove(index);
        session.generation = session.generation.saturating_add(1);
        self.advance_generation();
        Ok(draft)
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

        let mut session_ids = HashSet::with_capacity(snapshot.sessions.len());
        for session in &snapshot.sessions {
            if !session_ids.insert(session.id.as_str()) {
                return Err(ReplayError::InvalidMetadata(
                    "replay recovery contains a duplicate session".to_string(),
                ));
            }
            validate_recovered_session(session, self.limits)?;
        }
        if snapshot
            .active_session
            .as_deref()
            .is_some_and(|id| !session_ids.contains(id))
        {
            return Err(ReplayError::InvalidMetadata(
                "replay recovery selects an unknown session".to_string(),
            ));
        }

        self.staged.clear();
        self.sessions.clear();
        self.sources.clear();
        self.workspaces.clear();
        let mut recovered_in_flight = false;
        for session in &snapshot.sessions {
            self.sources
                .insert(session.source.id.clone(), session.source.clone());
            self.workspaces
                .insert(session.source.id.clone(), session.workspace.clone());
            let mut restored = session.clone();
            if let Some(pending) = restored.review.pending_submission.as_mut() {
                if pending.state == super::ReplayReviewSubmissionState::InFlight {
                    pending.state = super::ReplayReviewSubmissionState::Uncertain;
                    recovered_in_flight = true;
                }
            }
            self.sessions.insert(restored.id.clone(), restored);
        }
        self.active_session = snapshot
            .active_session
            .as_ref()
            .filter(|id| self.sessions.contains_key(id.as_str()))
            .cloned();
        self.generation = snapshot
            .generation
            .saturating_add(u64::from(recovered_in_flight));
        Ok(())
    }

    /// Returns the monotonic generation required by the session writer.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub(super) fn session_mut(&mut self, id: &str) -> Result<&mut ReplaySession, ReplayError> {
        self.sessions
            .get_mut(id)
            .ok_or_else(|| missing("replay session", id))
    }

    pub(super) fn advance_generation(&mut self) {
        self.generation = self.generation.saturating_add(1);
    }
}

fn validate_recovered_session(
    session: &ReplaySession,
    limits: ReplayLimits,
) -> Result<(), ReplayError> {
    if session.source.patch_digest != digest(session.source.patch.as_bytes()) {
        return Err(ReplayError::InvalidPatch(
            "the recovered original patch no longer matches its pinned digest".to_string(),
        ));
    }
    if session.workspace.base_commit != session.source.base_commit {
        return Err(ReplayError::InvalidMetadata(
            "the recovered scratch worktree does not match the original merge base".to_string(),
        ));
    }

    let expected =
        ReplaySession::from_source(session.source.clone(), session.workspace.clone(), limits)?;
    if session.steps.len() != expected.steps.len()
        || session.informational_changes != expected.informational_changes
        || session
            .steps
            .iter()
            .zip(&expected.steps)
            .any(|(recovered, original)| {
                recovered.id != original.id
                    || recovered.ordinal != original.ordinal
                    || recovered.kind != original.kind
                    || recovered.path != original.path
                    || recovered.target_commit != original.target_commit
                    || recovered.hunk_digest != original.hunk_digest
                    || recovered.heading != original.heading
                    || recovered.before != original.before
                    || recovered.after != original.after
                    || recovered.old_start != original.old_start
                    || recovered.dependencies != original.dependencies
                    || (recovered.status == ReplayStepStatus::Done)
                        != recovered.completion.is_some()
            })
    {
        return Err(ReplayError::InvalidPatch(
            "recovered learning steps no longer match the pinned original patch".to_string(),
        ));
    }
    if session
        .active_step
        .as_deref()
        .is_some_and(|id| !session.steps.iter().any(|step| step.id == id))
    {
        return Err(ReplayError::InvalidMetadata(
            "replay recovery selects an unknown original hunk".to_string(),
        ));
    }

    for note in &session.notes {
        if note.target_commit != session.source.target_commit
            || note.text.trim().is_empty()
            || note.text.len() > limits.max_note_bytes
        {
            return Err(ReplayError::InvalidReviewNote(
                "the recovered observation is not linked to the pinned original source".to_string(),
            ));
        }
        let expected_path = note.step_id.as_deref().map(|id| {
            session
                .steps
                .iter()
                .find(|step| step.id == id)
                .map(|step| step.path.as_path())
        });
        if matches!(expected_path, Some(None)) || note.path.as_deref() != expected_path.flatten() {
            return Err(ReplayError::InvalidReviewNote(
                "the recovered observation does not match its original source hunk".to_string(),
            ));
        }
    }

    if session.review.role != expected.review.role {
        return Err(ReplayError::InvalidReviewDraft(
            "the recovered review role does not match the authenticated original PR author"
                .to_string(),
        ));
    }
    if session.review.drafts.len() > limits.max_steps {
        return Err(ReplayError::LimitExceeded {
            kind: "local review drafts",
            limit: limits.max_steps,
        });
    }
    if session.review.receipts.len() > limits.max_steps {
        return Err(ReplayError::LimitExceeded {
            kind: "submitted review receipts",
            limit: limits.max_steps,
        });
    }
    let mut draft_ids = HashSet::with_capacity(session.review.drafts.len());
    for draft in &session.review.drafts {
        if !draft_ids.insert(draft.id.as_str())
            || draft.target_commit != session.source.target_commit
            || draft.text.trim().is_empty()
            || draft.text.len() > limits.max_note_bytes
            || draft.updated_at_ms < draft.created_at_ms
        {
            return Err(ReplayError::InvalidReviewDraft(
                "a recovered local draft does not match the pinned original PR head".to_string(),
            ));
        }
        if draft.kind == ReplayReviewDraftKind::CodeFix
            && session.review.role != ReplayReviewRole::Author
        {
            return Err(ReplayError::InvalidReviewDraft(
                "a recovered PR fix does not belong to the verified original author".to_string(),
            ));
        }

        match draft.kind {
            ReplayReviewDraftKind::InlineComment | ReplayReviewDraftKind::CodeFix => {
                let step_id = draft.step_id.as_deref().ok_or_else(|| {
                    ReplayError::InvalidReviewDraft(
                        "a recovered source-linked draft has no original replay step".to_string(),
                    )
                })?;
                let step = session
                    .steps
                    .iter()
                    .find(|step| step.id == step_id)
                    .ok_or_else(|| {
                        ReplayError::InvalidReviewDraft(
                            "a recovered draft names an unrelated original source hunk".to_string(),
                        )
                    })?;
                let anchor = session.original_review_anchor(step, limits)?;
                if draft.anchor.as_ref() != Some(&anchor)
                    || draft.path.as_deref() != Some(anchor.path.as_path())
                {
                    return Err(ReplayError::InvalidReviewDraft(
                        "a recovered draft no longer matches its exact original diff coordinates"
                            .to_string(),
                    ));
                }
            }
            ReplayReviewDraftKind::ReviewSummary => {
                if draft.step_id.is_some() || draft.path.is_some() || draft.anchor.is_some() {
                    return Err(ReplayError::InvalidReviewDraft(
                        "a recovered PR-level summary cannot claim an inline source anchor"
                            .to_string(),
                    ));
                }
            }
        }
    }

    let mut receipt_ids = HashSet::with_capacity(session.review.receipts.len());
    let mut receipt_draft_ids = HashSet::new();
    let mut submitted_draft_ids = HashSet::new();
    for receipt in &session.review.receipts {
        if receipt.id == 0
            || !receipt_ids.insert(receipt.id)
            || !session.source.pull_request.as_ref().is_some_and(|request| {
                super::review::review_receipt_matches_original_pull_request(request, receipt)
            })
            || receipt.target_commit != session.source.target_commit
            || receipt.viewer.trim().is_empty()
            || receipt.submitted_at.trim().is_empty()
            || receipt.payload_digest.len() != 64
            || !receipt
                .payload_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || receipt.draft_ids.is_empty()
        {
            return Err(ReplayError::InvalidReviewDraft(
                "a recovered submitted-review receipt does not match the original PR head"
                    .to_string(),
            ));
        }
        for draft_id in &receipt.draft_ids {
            if !draft_ids.contains(draft_id.as_str())
                || !receipt_draft_ids.insert(draft_id.as_str())
            {
                return Err(ReplayError::InvalidReviewDraft(
                    "a recovered submitted review contains an unrelated or duplicate draft"
                        .to_string(),
                ));
            }
            if receipt.verification == super::ReplayReceiptVerification::Verified {
                submitted_draft_ids.insert(draft_id.as_str());
            }
        }
    }
    if session.review.drafts.iter().any(|draft| {
        (draft.state == ReplayDraftState::Submitted)
            != submitted_draft_ids.contains(draft.id.as_str())
            || (draft.state == ReplayDraftState::Submitted
                && draft.kind == ReplayReviewDraftKind::CodeFix)
    }) {
        return Err(ReplayError::InvalidReviewDraft(
            "a published review draft does not have a matching submitted GitHub review receipt"
                .to_string(),
        ));
    }

    if let Some(pending) = session.review.pending_submission.as_ref() {
        super::review::validate_recovered_pending_submission(session, pending, limits)?;
    }

    Ok(())
}

impl ReplaySession {
    /// Compiles complete source-backed exercises without changing the worktree.
    pub fn from_source(
        source: ReplaySource,
        workspace: ReplayWorkspace,
        limits: ReplayLimits,
    ) -> Result<Self, ReplayError> {
        let role = ReplayReviewRole::from_pull_request(source.pull_request.as_ref());
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
            review: ReplayReviewState {
                role,
                drafts: Vec::new(),
                receipts: Vec::new(),
                pending_submission: None,
            },
            generation: 0,
        })
    }

    pub(super) fn original_review_anchor(
        &self,
        step: &ReplayStep,
        limits: ReplayLimits,
    ) -> Result<ReplayReviewAnchor, ReplayError> {
        let patch = parse_patch(&self.source.patch, limits)?;
        for file in patch.files {
            if file.path() != Some(step.path.as_path()) {
                continue;
            }
            let old_path = file.old_path.clone();
            let new_path = file.new_path.clone();
            for hunk in file.hunks {
                let original = format!(
                    "{}\0{}\0{}\0{}\0{}",
                    self.source.patch_digest,
                    step.path.display(),
                    hunk.header,
                    hunk.before,
                    hunk.after,
                );
                if digest(original.as_bytes()) != step.hunk_digest {
                    continue;
                }
                let (side, path, range) = if let Some(range) = hunk.added_range {
                    (
                        ReplayDiffSide::Right,
                        new_path.clone().ok_or_else(|| {
                            ReplayError::InvalidReviewDraft(
                                "an original added-line comment has no head-side file".to_string(),
                            )
                        })?,
                        range,
                    )
                } else if let Some(range) = hunk.removed_range {
                    (
                        ReplayDiffSide::Left,
                        old_path.clone().ok_or_else(|| {
                            ReplayError::InvalidReviewDraft(
                                "an original deleted-line comment has no base-side file"
                                    .to_string(),
                            )
                        })?,
                        range,
                    )
                } else {
                    return Err(ReplayError::InvalidReviewDraft(
                        "the original hunk has no commentable changed lines".to_string(),
                    ));
                };
                if range.start == 0 || range.count == 0 {
                    return Err(ReplayError::InvalidReviewDraft(
                        "the original diff has invalid one-based comment coordinates".to_string(),
                    ));
                }
                return Ok(ReplayReviewAnchor {
                    target_commit: self.source.target_commit.clone(),
                    path,
                    old_path,
                    side,
                    start_line: range.start,
                    end_line: range.start.saturating_add(range.count.saturating_sub(1)),
                    hunk_digest: step.hunk_digest.clone(),
                });
            }
        }
        Err(ReplayError::InvalidReviewDraft(
            "the selected comment does not match an exact pinned original source hunk".to_string(),
        ))
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
    use crate::replay::{
        ReplayRepository, ReplayReviewBundle, ReplayReviewOutcome, ReplayReviewReceipt,
        ReplayReviewSubmissionPreview, ReplaySourceKind, MAX_REPLAY_REVIEW_BUNDLE_BYTES,
        REPLAY_REVIEW_BUNDLE_VERSION,
    };

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

    fn controller_with_pull_request(
        author: &str,
        viewer: Option<&str>,
    ) -> (ReplayController, String, String) {
        let mut source = sample_source(CHANGE);
        source.kind = ReplaySourceKind::GitHubPullRequest;
        source.pull_request = Some(ReplayPullRequest {
            host: "github.com".to_string(),
            repository_owner: "owner".to_string(),
            repository_name: "repository".to_string(),
            number: 482,
            url: "https://github.com/owner/repository/pull/482".to_string(),
            author: Some(author.to_string()),
            base_ref: "master".to_string(),
            base_ref_tip: source.base_commit.clone(),
            head_repository_owner: "owner".to_string(),
            head_repository_name: "repository".to_string(),
            head_ref: "feature/replay".to_string(),
            head_commit: source.target_commit.clone(),
            cross_repository: false,
            capabilities: super::super::ReplayGitHubCapabilities {
                viewer: viewer.map(str::to_string),
                head_permission: super::super::ReplayRepositoryPermission::Write,
                warning: None,
            },
            captured_at_ms: 0,
        });
        let workspace = ReplayWorkspace {
            root: PathBuf::from("/workspace/repository.replay"),
            branch: "replay/test".to_string(),
            base_commit: source.base_commit.clone(),
            created_by_replay: true,
        };
        let session = ReplaySession::from_source(source, workspace, ReplayLimits::default())
            .expect("compile the authenticated immutable pull request");
        let session_id = session.id.clone();
        let step_id = session.steps[0].id.clone();
        let mut controller = ReplayController::default();
        controller.sessions.insert(session_id.clone(), session);
        (controller, session_id, step_id)
    }

    fn read_portable_review(path: &Path) -> ReplayReviewBundle {
        let bytes = std::fs::read(path).expect("read the explicitly saved private review");
        serde_json::from_slice(&bytes).expect("decode the versioned portable private review")
    }

    fn replace_portable_review(path: &Path, bundle: &ReplayReviewBundle) {
        let encoded = serde_json::to_vec_pretty(bundle)
            .expect("encode an intentionally modified private review fixture");
        std::fs::write(path, encoded)
            .expect("replace only the isolated temporary private review fixture");
    }

    fn submitted_review_receipt(preview: &ReplayReviewSubmissionPreview) -> ReplayReviewReceipt {
        ReplayReviewReceipt {
            id: 71,
            url: "https://github.com/owner/repository/pull/482#pullrequestreview-71".to_string(),
            outcome: preview.outcome,
            target_commit: preview.target_commit.clone(),
            viewer: preview.viewer.clone(),
            draft_ids: preview
                .drafts
                .iter()
                .map(|draft| draft.id.clone())
                .collect(),
            payload_digest: digest(b"explicitly approved original PR review"),
            submitted_at: "2026-07-27T20:00:00Z".to_string(),
            verification: super::super::ReplayReceiptVerification::Verified,
        }
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
    fn author_workspace_is_adopted_without_replacing_the_learning_scratch() {
        let (mut controller, session_id, _) =
            controller_with_pull_request("original-author", Some("original-author"));
        let original_scratch = controller.session(&session_id).unwrap().workspace.clone();
        let preview = controller
            .preview_author_workspace(&session_id)
            .expect("preview only the authenticated author's exact PR head");
        let author = ReplayAuthorWorkspace {
            root: preview.root,
            branch: preview.branch,
            head_commit: preview.head_commit,
            head_repository: preview.head_repository,
            head_ref: preview.head_ref,
            created_by_replay: true,
        };

        assert!(matches!(
            controller.author_workspace(&session_id),
            Err(ReplayError::AuthorWorkspaceConfirmationRequired),
        ));
        controller
            .adopt_author_workspace(&session_id, author.clone())
            .expect("adopt only the bounded worker's exact original author worktree");

        assert_eq!(controller.author_workspace(&session_id).unwrap(), &author);
        assert_eq!(
            controller.session(&session_id).unwrap().workspace,
            original_scratch
        );
        assert_ne!(author.root, original_scratch.root);
        assert_ne!(author.head_commit, original_scratch.base_commit);
    }

    #[test]
    fn refuses_a_background_author_worktree_from_another_fork_or_original_head() {
        let (mut controller, session_id, _) =
            controller_with_pull_request("original-author", Some("original-author"));
        let preview = controller
            .preview_author_workspace(&session_id)
            .expect("preview the independently pinned original author head");
        let expected = ReplayAuthorWorkspace {
            root: preview.root,
            branch: preview.branch,
            head_commit: preview.head_commit,
            head_repository: preview.head_repository,
            head_ref: preview.head_ref,
            created_by_replay: true,
        };
        let mut foreign_fork = expected.clone();
        foreign_fork.head_repository = "github.com/another-author/unrelated-fork".to_string();

        assert!(matches!(
            controller.adopt_author_workspace(&session_id, foreign_fork),
            Err(ReplayError::WorkspaceExists(_)),
        ));

        let mut foreign_head = expected;
        foreign_head.head_commit = controller
            .session(&session_id)
            .unwrap()
            .source
            .base_commit
            .clone();
        assert!(matches!(
            controller.adopt_author_workspace(&session_id, foreign_head),
            Err(ReplayError::WorkspaceExists(_)),
        ));
        assert!(matches!(
            controller.author_workspace(&session_id),
            Err(ReplayError::AuthorWorkspaceConfirmationRequired),
        ));
    }

    #[test]
    fn shared_repository_write_access_does_not_authorize_a_reviewers_pr_worktree() {
        let (controller, session_id, _) =
            controller_with_pull_request("original-author", Some("another-reviewer"));

        assert!(matches!(
            controller.preview_author_workspace(&session_id),
            Err(ReplayError::AuthorWorkspaceUnavailable(_)),
        ));
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
    fn identifies_the_original_author_only_from_an_authenticated_matching_viewer() {
        let (author, session_id, _) =
            controller_with_pull_request("Original-Author", Some("original-author"));
        assert_eq!(
            author.session(&session_id).unwrap().review.role,
            ReplayReviewRole::Author,
        );

        let (reviewer, session_id, _) =
            controller_with_pull_request("original-author", Some("another-reviewer"));
        assert_eq!(
            reviewer.session(&session_id).unwrap().review.role,
            ReplayReviewRole::Reviewer,
        );

        let (unverified, session_id, _) =
            controller_with_pull_request("original-author", /*viewer*/ None);
        assert_eq!(
            unverified.session(&session_id).unwrap().review.role,
            ReplayReviewRole::Reviewer,
        );
    }

    #[test]
    fn inline_review_drafts_pin_the_original_head_and_actual_changed_line() {
        let (mut controller, session_id, step_id) = controller_with_session();
        let draft = controller
            .add_review_draft(
                &session_id,
                Some(&step_id),
                ReplayReviewDraftKind::InlineComment,
                "Should the replacement refresh be bounded?",
            )
            .expect("anchor a private inline comment to the original Git diff");
        let anchor = draft
            .anchor
            .expect("inline drafts require an exact diff anchor");

        assert_eq!(draft.kind, ReplayReviewDraftKind::InlineComment);
        assert_eq!(draft.state, ReplayDraftState::Local);
        assert_eq!(draft.origin, ReplayDraftOrigin::Human);
        assert_eq!(anchor.target_commit.as_str(), "b".repeat(40));
        assert_eq!(anchor.path, PathBuf::from("src/token.rs"));
        assert_eq!(anchor.old_path, Some(PathBuf::from("src/token.rs")));
        assert_eq!(anchor.side, ReplayDiffSide::Right);
        assert_eq!(anchor.start_line, 2);
        assert_eq!(anchor.end_line, 2);
        assert_eq!(
            anchor.hunk_digest,
            controller.session(&session_id).unwrap().steps[0].hunk_digest,
        );
    }

    #[test]
    fn deletion_only_review_drafts_anchor_the_original_base_side() {
        let patch = concat!(
            "diff --git a/src/token.rs b/src/token.rs\n",
            "--- a/src/token.rs\n",
            "+++ b/src/token.rs\n",
            "@@ -7,3 +7,2 @@ fn refresh\n",
            " before\n",
            "-removed\n",
            " after\n",
        );
        let source = sample_source(patch);
        let workspace = ReplayWorkspace {
            root: PathBuf::from("/workspace/repository.replay"),
            branch: "replay/test".to_string(),
            base_commit: source.base_commit.clone(),
            created_by_replay: true,
        };
        let session = ReplaySession::from_source(source, workspace, ReplayLimits::default())
            .expect("compile the exact original deletion");
        let session_id = session.id.clone();
        let step_id = session.steps[0].id.clone();
        let mut controller = ReplayController::default();
        controller.sessions.insert(session_id.clone(), session);

        let draft = controller
            .add_review_draft(
                &session_id,
                Some(&step_id),
                ReplayReviewDraftKind::InlineComment,
                "Why can this original line be safely removed?",
            )
            .expect("retain the original base-side deletion coordinates");
        let anchor = draft.anchor.expect("deleted lines use a left-side anchor");

        assert_eq!(anchor.side, ReplayDiffSide::Left);
        assert_eq!(anchor.start_line, 8);
        assert_eq!(anchor.end_line, 8);
    }

    #[test]
    fn only_verified_original_authors_can_draft_pr_code_fixes() {
        let (mut author, author_session, author_step) =
            controller_with_pull_request("original-author", Some("original-author"));
        let fix = author
            .add_review_draft(
                &author_session,
                Some(&author_step),
                ReplayReviewDraftKind::CodeFix,
                "Replace this with the bounded refresh helper.",
            )
            .expect("retain the author's proposed fix locally");
        assert_eq!(fix.kind, ReplayReviewDraftKind::CodeFix);
        assert_eq!(fix.state, ReplayDraftState::Local);

        let (mut reviewer, reviewer_session, reviewer_step) =
            controller_with_pull_request("original-author", Some("another-reviewer"));
        assert!(matches!(
            reviewer.add_review_draft(
                &reviewer_session,
                Some(&reviewer_step),
                ReplayReviewDraftKind::CodeFix,
                "Change someone else's PR.",
            ),
            Err(ReplayError::InvalidReviewDraft(_)),
        ));
        assert!(reviewer
            .session(&reviewer_session)
            .unwrap()
            .review
            .drafts
            .is_empty());
    }

    #[test]
    fn review_summaries_never_claim_an_original_inline_anchor() {
        let (mut controller, session_id, step_id) = controller_with_session();
        let draft = controller
            .add_review_draft(
                &session_id,
                /*step_id*/ None,
                ReplayReviewDraftKind::ReviewSummary,
                "The refresh flow needs a regression test.",
            )
            .expect("retain a genuinely pull-request-level local draft");

        assert_eq!(draft.kind, ReplayReviewDraftKind::ReviewSummary);
        assert!(draft.step_id.is_none());
        assert!(draft.path.is_none());
        assert!(draft.anchor.is_none());
        assert!(matches!(
            controller.add_review_draft(
                &session_id,
                Some(&step_id),
                ReplayReviewDraftKind::ReviewSummary,
                "This must not impersonate an inline comment.",
            ),
            Err(ReplayError::InvalidReviewDraft(_)),
        ));
    }

    #[test]
    fn local_review_drafts_can_be_edited_and_removed_without_changing_their_anchor() {
        let (mut controller, session_id, step_id) = controller_with_session();
        let original = controller
            .add_review_draft(
                &session_id,
                Some(&step_id),
                ReplayReviewDraftKind::InlineComment,
                "Initial private review comment.",
            )
            .unwrap();
        let generation = controller.generation();

        let updated = controller
            .update_review_draft(&session_id, &original.id, "Updated private review comment.")
            .expect("edit only a local comment");
        assert_eq!(updated.anchor, original.anchor);
        assert_eq!(updated.target_commit, original.target_commit);
        assert_eq!(updated.text, "Updated private review comment.");
        assert!(controller.generation() > generation);

        let removed = controller
            .remove_review_draft(&session_id, &original.id)
            .expect("discard only the selected local comment");
        assert_eq!(removed.id, original.id);
        assert!(controller
            .session(&session_id)
            .unwrap()
            .review
            .drafts
            .is_empty());
    }

    #[test]
    fn recovery_preserves_original_source_anchored_local_review_drafts() {
        let (mut controller, session_id, step_id) =
            controller_with_pull_request("original-author", Some("original-author"));
        let original = controller
            .add_review_draft(
                &session_id,
                Some(&step_id),
                ReplayReviewDraftKind::CodeFix,
                "Bound the refresh before updating the original PR.",
            )
            .unwrap();
        let snapshot = controller.recovery_snapshot().unwrap();
        let mut restored = ReplayController::default();

        restored
            .restore(&snapshot)
            .expect("restore only the exact original author and local diff anchor");

        let review = &restored.session(&session_id).unwrap().review;
        assert_eq!(review.role, ReplayReviewRole::Author);
        assert_eq!(review.drafts, vec![original]);
    }

    #[test]
    fn recovery_rejects_a_local_draft_reassigned_to_a_different_original_line() {
        let (mut controller, session_id, step_id) = controller_with_session();
        controller
            .add_review_draft(
                &session_id,
                Some(&step_id),
                ReplayReviewDraftKind::InlineComment,
                "Keep this exact original line bounded.",
            )
            .unwrap();
        let mut snapshot = controller.recovery_snapshot().unwrap();
        snapshot.sessions[0].review.drafts[0]
            .anchor
            .as_mut()
            .unwrap()
            .start_line = 99;
        let mut restored = ReplayController::default();

        assert!(matches!(
            restored.restore(&snapshot),
            Err(ReplayError::InvalidReviewDraft(_)),
        ));
        assert!(restored.sessions().is_empty());
    }

    #[test]
    fn existing_replay_snapshots_restore_without_a_local_outbox() {
        let (controller, session_id, _) = controller_with_session();
        let mut snapshot = serde_json::to_value(controller.recovery_snapshot().unwrap()).unwrap();
        snapshot["sessions"][0]
            .as_object_mut()
            .expect("recovered sessions are structured")
            .remove("review");
        let snapshot: ReplayRecoverySnapshot = serde_json::from_value(snapshot)
            .expect("recover snapshots created before the review outbox existed");
        let mut restored = ReplayController::default();

        restored.restore(&snapshot).unwrap();

        let review = &restored.session(&session_id).unwrap().review;
        assert_eq!(review.role, ReplayReviewRole::Reviewer);
        assert!(review.drafts.is_empty());
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
    fn browsing_an_original_hunk_does_not_claim_its_prerequisite_is_complete() {
        let (mut controller, session_id, first_id) = controller_with_session();
        let second_id = "dependent-original-hunk".to_string();
        let session = controller.sessions.get_mut(&session_id).unwrap();
        let mut dependent = session.steps[0].clone();
        dependent.id = second_id.clone();
        dependent.ordinal = 2;
        dependent.dependencies = vec![first_id];
        dependent.status = ReplayStepStatus::Pending;
        dependent.completion = None;
        session.steps.push(dependent);

        controller
            .select_step(&session_id, &second_id)
            .expect("reviewers may inspect a blocked original hunk");

        let session = controller.session(&session_id).unwrap();
        assert_eq!(session.active_step.as_deref(), Some(second_id.as_str()));
        assert_eq!(session.steps[1].status, ReplayStepStatus::Pending);
        assert_eq!(session.steps[1].completion, None);
        assert_eq!(
            controller
                .validate_step(
                    &session_id,
                    &second_id,
                    Path::new("src/token.rs"),
                    "fn refresh() {\n    old();\n}\n",
                )
                .unwrap(),
            ReplayValidation::Blocked,
        );
    }

    #[test]
    fn recovery_rejects_a_modified_original_patch_before_adopting_session_state() {
        let (controller, _, _) = controller_with_session();
        let mut snapshot = controller
            .recovery_snapshot()
            .expect("capture the pinned original reviewer session");
        snapshot.sessions[0]
            .source
            .patch
            .push_str("untrusted extra source\n");

        let mut restored = ReplayController::default();
        assert!(matches!(
            restored.restore(&snapshot),
            Err(ReplayError::InvalidPatch(_)),
        ));
        assert!(restored.sessions().is_empty());
        assert!(restored.active_session().is_none());
    }

    #[test]
    fn recovery_rejects_an_observation_reassigned_to_another_original_file() {
        let (mut controller, session_id, step_id) = controller_with_session();
        controller
            .add_note(
                &session_id,
                Some(&step_id),
                ReplayNoteCategory::Observation,
                "Keep the refresh bounded.",
            )
            .expect("record one original-source observation");
        let mut snapshot = controller
            .recovery_snapshot()
            .expect("capture the source-linked observation");
        snapshot.sessions[0].notes[0].path = Some(PathBuf::from("src/unrelated.rs"));

        let mut restored = ReplayController::default();
        assert!(matches!(
            restored.restore(&snapshot),
            Err(ReplayError::InvalidReviewNote(_)),
        ));
        assert!(restored.sessions().is_empty());
    }

    #[test]
    fn portable_review_saves_only_versioned_private_original_source_outcomes() {
        let directory = tempfile::tempdir().expect("create isolated private review storage");
        let path = directory
            .path()
            .join("private")
            .join("pr-482-bbbbbbb.red-review.json");
        let (mut controller, session_id, step_id) =
            controller_with_pull_request("alice", Some("reviewer"));
        controller
            .add_note(
                &session_id,
                Some(&step_id),
                ReplayNoteCategory::TestGap,
                "Cover the original refresh boundary.",
            )
            .unwrap();
        controller
            .add_review_draft(
                &session_id,
                Some(&step_id),
                ReplayReviewDraftKind::InlineComment,
                "Please add a boundary regression test.",
            )
            .unwrap();

        let saved = controller
            .save_review_bundle(&session_id, &path, /*overwrite*/ false)
            .expect("explicitly save only the original-source review outcomes");
        let bundle = read_portable_review(&saved.path);

        assert_eq!(bundle.version, REPLAY_REVIEW_BUNDLE_VERSION);
        assert_eq!(bundle.identity.repository, "github.com/owner/repository");
        assert_eq!(bundle.identity.pull_request, Some(482));
        assert_eq!(
            bundle.identity.source_kind,
            ReplaySourceKind::GitHubPullRequest
        );
        assert_eq!(saved.note_count, 1);
        assert_eq!(saved.draft_count, 1);
        assert_eq!(bundle.notes.len(), 1);
        assert_eq!(bundle.drafts.len(), 1);
        assert_eq!(
            bundle.drafts[0].anchor.as_ref().unwrap().path,
            Path::new("src/token.rs")
        );
        let encoded = std::fs::read_to_string(saved.path).unwrap();
        assert!(!encoded.contains("/workspace/repository"));
        assert!(!encoded.contains("pending_review"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600,
            );
            assert_eq!(
                std::fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700,
            );
        }
    }

    #[test]
    fn portable_reviews_keep_imported_github_receipts_unverified_until_provider_confirmation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("submitted-pr-482.red-review.json");
        let (mut original, original_id, original_step) =
            controller_with_pull_request("alice", Some("reviewer"));
        original
            .add_review_draft(
                &original_id,
                Some(&original_step),
                ReplayReviewDraftKind::InlineComment,
                "Please cover the original refresh boundary.",
            )
            .unwrap();
        let preview = original
            .preview_review_submission(&original_id, ReplayReviewOutcome::Comment)
            .unwrap();
        let receipt = submitted_review_receipt(&preview);
        original
            .record_review_submission(&original_id, &preview, receipt.clone())
            .unwrap();
        let saved = original
            .save_review_bundle(&original_id, &path, /*overwrite*/ false)
            .unwrap();
        let bundle = read_portable_review(&path);

        assert_eq!(bundle.version, REPLAY_REVIEW_BUNDLE_VERSION);
        assert_eq!(saved.receipt_count, 1);
        assert_eq!(bundle.receipts, vec![receipt.clone()]);
        assert_eq!(bundle.drafts[0].state, ReplayDraftState::Submitted);

        let (mut destination, destination_id, _) =
            controller_with_pull_request("alice", Some("reviewer"));
        let import = destination
            .preview_review_bundle(&destination_id, &path)
            .unwrap();
        assert_eq!(import.drafts_to_add, 1);
        assert_eq!(import.receipts_to_add, 1);
        destination
            .import_review_bundle(
                &destination_id,
                &path,
                &import.bundle_digest,
                /*confirmed*/ true,
            )
            .unwrap();
        let recovered = destination.session(&destination_id).unwrap();
        let mut imported = receipt;
        imported.verification = super::super::ReplayReceiptVerification::Unverified;
        assert_eq!(recovered.review.receipts, vec![imported]);
        assert_eq!(recovered.review.drafts[0].state, ReplayDraftState::Local);
        assert!(destination
            .preview_review_submission(&destination_id, ReplayReviewOutcome::Comment)
            .is_ok());
    }

    #[test]
    fn forged_portable_review_receipt_cannot_mark_a_local_draft_as_provider_verified() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("forged-pr-482.red-review.json");
        let (mut original, original_id, original_step) =
            controller_with_pull_request("alice", Some("reviewer"));
        original
            .add_review_draft(
                &original_id,
                Some(&original_step),
                ReplayReviewDraftKind::InlineComment,
                "A copied local JSON receipt is not GitHub evidence.",
            )
            .unwrap();
        let preview = original
            .preview_review_submission(&original_id, ReplayReviewOutcome::Comment)
            .unwrap();
        original
            .record_review_submission(&original_id, &preview, submitted_review_receipt(&preview))
            .unwrap();
        original
            .save_review_bundle(&original_id, &path, /*overwrite*/ false)
            .unwrap();
        let mut forged = read_portable_review(&path);
        forged.receipts[0].id = 991;
        forged.receipts[0].url =
            "https://github.com/owner/repository/pull/482#pullrequestreview-991".to_string();
        forged.receipts[0].payload_digest = "f".repeat(64);
        forged.receipts[0].verification = super::super::ReplayReceiptVerification::Verified;
        replace_portable_review(&path, &forged);

        let (mut destination, destination_id, _) =
            controller_with_pull_request("alice", Some("reviewer"));
        let preview = destination
            .preview_review_bundle(&destination_id, &path)
            .unwrap();
        destination
            .import_review_bundle(
                &destination_id,
                &path,
                &preview.bundle_digest,
                /*confirmed*/ true,
            )
            .unwrap();
        let recovered = destination.session(&destination_id).unwrap();

        assert_eq!(
            recovered.review.receipts[0].verification,
            super::super::ReplayReceiptVerification::Unverified,
        );
        assert_eq!(recovered.review.drafts[0].state, ReplayDraftState::Local);
        assert!(destination
            .preview_review_submission(&destination_id, ReplayReviewOutcome::Comment)
            .is_ok());
    }

    #[test]
    fn original_version_one_private_review_files_remain_loadable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy-pr-482.red-review.json");
        let (mut original, original_id, original_step) =
            controller_with_pull_request("alice", Some("reviewer"));
        original
            .add_review_draft(
                &original_id,
                Some(&original_step),
                ReplayReviewDraftKind::InlineComment,
                "Keep the original portable review backward compatible.",
            )
            .unwrap();
        original
            .save_review_bundle(&original_id, &path, /*overwrite*/ false)
            .unwrap();
        let mut legacy = read_portable_review(&path);
        legacy.version = 1;
        replace_portable_review(&path, &legacy);

        let (mut destination, destination_id, _) =
            controller_with_pull_request("alice", Some("reviewer"));
        let preview = destination
            .preview_review_bundle(&destination_id, &path)
            .expect("continue to recognize an existing version-one private review file");
        destination
            .import_review_bundle(
                &destination_id,
                &path,
                &preview.bundle_digest,
                /*confirmed*/ true,
            )
            .unwrap();

        assert_eq!(
            destination
                .session(&destination_id)
                .unwrap()
                .review
                .drafts
                .len(),
            1
        );
        assert!(destination
            .session(&destination_id)
            .unwrap()
            .review
            .receipts
            .is_empty());
    }

    #[test]
    fn submitted_drafts_cannot_be_imported_without_their_verified_github_receipt() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tampered-pr-482.red-review.json");
        let (mut original, original_id, original_step) =
            controller_with_pull_request("alice", Some("reviewer"));
        original
            .add_review_draft(
                &original_id,
                Some(&original_step),
                ReplayReviewDraftKind::InlineComment,
                "The original published comment needs its exact receipt.",
            )
            .unwrap();
        let preview = original
            .preview_review_submission(&original_id, ReplayReviewOutcome::Comment)
            .unwrap();
        original
            .record_review_submission(&original_id, &preview, submitted_review_receipt(&preview))
            .unwrap();
        original
            .save_review_bundle(&original_id, &path, /*overwrite*/ false)
            .unwrap();
        let mut tampered = read_portable_review(&path);
        tampered.receipts.clear();
        replace_portable_review(&path, &tampered);

        let (destination, destination_id, _) =
            controller_with_pull_request("alice", Some("reviewer"));
        assert!(matches!(
            destination.preview_review_bundle(&destination_id, &path),
            Err(ReplayError::InvalidReviewBundle(_)),
        ));
    }

    #[test]
    fn crash_recovery_restores_verified_submitted_reviews_without_reposting() {
        let (mut original, session_id, step_id) =
            controller_with_pull_request("alice", Some("reviewer"));
        original
            .add_review_draft(
                &session_id,
                Some(&step_id),
                ReplayReviewDraftKind::InlineComment,
                "Preserve the exact original published review across recovery.",
            )
            .unwrap();
        let preview = original
            .preview_review_submission(&session_id, ReplayReviewOutcome::Comment)
            .unwrap();
        let receipt = submitted_review_receipt(&preview);
        original
            .record_review_submission(&session_id, &preview, receipt.clone())
            .unwrap();
        let snapshot = original.recovery_snapshot().unwrap();

        let mut recovered = ReplayController::default();
        recovered.restore(&snapshot).unwrap();
        let session = recovered.session(&session_id).unwrap();
        assert_eq!(session.review.receipts, vec![receipt]);
        assert_eq!(session.review.drafts[0].state, ReplayDraftState::Submitted);
        assert!(matches!(
            recovered.preview_review_submission(&session_id, ReplayReviewOutcome::Comment),
            Err(ReplayError::InvalidReviewDraft(_)),
        ));
    }

    #[test]
    fn portable_review_merges_across_machine_specific_repository_paths() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pr-482.red-review.json");
        let (mut original, original_id, original_step) =
            controller_with_pull_request("alice", Some("reviewer"));
        original
            .add_note(
                &original_id,
                Some(&original_step),
                ReplayNoteCategory::Observation,
                "The original branch rotates its refresh token.",
            )
            .unwrap();
        let imported_draft = original
            .add_review_draft(
                &original_id,
                Some(&original_step),
                ReplayReviewDraftKind::InlineComment,
                "What prevents repeated rotation?",
            )
            .unwrap();
        original
            .save_review_bundle(&original_id, &path, /*overwrite*/ false)
            .unwrap();

        let (mut other_machine, other_id, _) =
            controller_with_pull_request("alice", Some("reviewer"));
        {
            let session = other_machine.session_mut(&other_id).unwrap();
            session.source.repository.root = PathBuf::from("/another/computer/repository");
            session.source.repository.common_directory =
                PathBuf::from("/another/computer/repository/.git");
        }
        let existing = other_machine
            .add_review_draft(
                &other_id,
                None,
                ReplayReviewDraftKind::ReviewSummary,
                "Keep my existing independent review.",
            )
            .unwrap();
        let preview = other_machine
            .preview_review_bundle(&other_id, &path)
            .expect("match host, repository, PR, commits, and original diff across computers");
        assert_eq!(preview.notes_to_add, 1);
        assert_eq!(preview.drafts_to_add, 1);

        let merged = other_machine
            .import_review_bundle(
                &other_id,
                &path,
                &preview.bundle_digest,
                /*confirmed*/ true,
            )
            .unwrap();
        let session = other_machine.session(&other_id).unwrap();
        assert_eq!(merged.notes_to_add, 1);
        assert_eq!(merged.drafts_to_add, 1);
        assert_eq!(session.notes.len(), 1);
        assert_eq!(session.review.drafts.len(), 2);
        assert_eq!(session.review.drafts[0], existing);
        assert_eq!(session.review.drafts[1], imported_draft);
        assert_eq!(session.review.role, ReplayReviewRole::Reviewer);
    }

    #[test]
    fn reloading_an_identical_review_never_duplicates_or_rewrites_outcomes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("review.json");
        let (mut controller, session_id, step_id) = controller_with_session();
        controller
            .add_note(
                &session_id,
                Some(&step_id),
                ReplayNoteCategory::Observation,
                "Preserve the original local observation.",
            )
            .unwrap();
        controller
            .add_review_draft(
                &session_id,
                Some(&step_id),
                ReplayReviewDraftKind::InlineComment,
                "Preserve the original local comment.",
            )
            .unwrap();
        controller
            .save_review_bundle(&session_id, &path, /*overwrite*/ false)
            .unwrap();
        let generation = controller.generation();
        let preview = controller
            .preview_review_bundle(&session_id, &path)
            .unwrap();

        assert_eq!(preview.notes_to_add, 0);
        assert_eq!(preview.notes_already_present, 1);
        assert_eq!(preview.drafts_to_add, 0);
        assert_eq!(preview.drafts_already_present, 1);
        controller
            .import_review_bundle(
                &session_id,
                &path,
                &preview.bundle_digest,
                /*confirmed*/ true,
            )
            .unwrap();
        assert_eq!(controller.generation(), generation);
        assert_eq!(controller.session(&session_id).unwrap().notes.len(), 1);
        assert_eq!(
            controller.session(&session_id).unwrap().review.drafts.len(),
            1
        );
    }

    #[test]
    fn portable_review_rejects_a_moved_original_pr_head_before_merging() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("review.json");
        let (mut controller, session_id, step_id) =
            controller_with_pull_request("alice", Some("reviewer"));
        controller
            .add_review_draft(
                &session_id,
                Some(&step_id),
                ReplayReviewDraftKind::InlineComment,
                "This comment belongs to the original head.",
            )
            .unwrap();
        controller
            .save_review_bundle(&session_id, &path, /*overwrite*/ false)
            .unwrap();
        let mut bundle = read_portable_review(&path);
        bundle.identity.target_commit = GitObjectId::parse(&"c".repeat(40)).unwrap();
        replace_portable_review(&path, &bundle);

        assert!(matches!(
            controller.preview_review_bundle(&session_id, &path),
            Err(ReplayError::InvalidReviewBundle(_)),
        ));
        assert_eq!(
            controller.session(&session_id).unwrap().review.drafts.len(),
            1
        );
    }

    #[test]
    fn portable_review_rejects_a_foreign_repository_and_unknown_format() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("review.json");
        let (mut controller, session_id, step_id) = controller_with_session();
        controller
            .add_review_draft(
                &session_id,
                Some(&step_id),
                ReplayReviewDraftKind::InlineComment,
                "Keep the original source identity.",
            )
            .unwrap();
        controller
            .save_review_bundle(&session_id, &path, /*overwrite*/ false)
            .unwrap();
        let original = read_portable_review(&path);
        let mut foreign = original.clone();
        foreign.identity.repository = "github.com/another/repository".to_string();
        replace_portable_review(&path, &foreign);
        assert!(matches!(
            controller.preview_review_bundle(&session_id, &path),
            Err(ReplayError::InvalidReviewBundle(_)),
        ));

        let mut future = original;
        future.version = REPLAY_REVIEW_BUNDLE_VERSION.saturating_add(1);
        replace_portable_review(&path, &future);
        assert!(matches!(
            controller.preview_review_bundle(&session_id, &path),
            Err(ReplayError::InvalidReviewBundle(_)),
        ));
    }

    #[test]
    fn portable_review_rejects_changed_original_diff_coordinates() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("review.json");
        let (mut controller, session_id, step_id) = controller_with_session();
        controller
            .add_review_draft(
                &session_id,
                Some(&step_id),
                ReplayReviewDraftKind::InlineComment,
                "Keep this exact original changed line.",
            )
            .unwrap();
        controller
            .save_review_bundle(&session_id, &path, /*overwrite*/ false)
            .unwrap();
        let mut bundle = read_portable_review(&path);
        bundle.drafts[0].anchor.as_mut().unwrap().start_line += 1;
        replace_portable_review(&path, &bundle);

        assert!(matches!(
            controller.preview_review_bundle(&session_id, &path),
            Err(ReplayError::InvalidReviewBundle(_)),
        ));
    }

    #[test]
    fn portable_review_rejects_duplicate_source_draft_identities() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("review.json");
        let (mut controller, session_id, step_id) = controller_with_session();
        controller
            .add_review_draft(
                &session_id,
                Some(&step_id),
                ReplayReviewDraftKind::InlineComment,
                "Each imported review has a stable independent identity.",
            )
            .unwrap();
        controller
            .save_review_bundle(&session_id, &path, /*overwrite*/ false)
            .unwrap();
        let mut bundle = read_portable_review(&path);
        bundle.drafts.push(bundle.drafts[0].clone());
        replace_portable_review(&path, &bundle);

        assert!(matches!(
            controller.preview_review_bundle(&session_id, &path),
            Err(ReplayError::InvalidReviewBundle(_)),
        ));
    }

    #[test]
    fn portable_review_conflicts_preserve_every_existing_local_outcome() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("review.json");
        let (mut original, original_id, step_id) = controller_with_session();
        let imported = original
            .add_review_draft(
                &original_id,
                Some(&step_id),
                ReplayReviewDraftKind::InlineComment,
                "Original review text.",
            )
            .unwrap();
        original
            .add_note(
                &original_id,
                Some(&step_id),
                ReplayNoteCategory::Question,
                "This observation must not be partially merged.",
            )
            .unwrap();
        original
            .save_review_bundle(&original_id, &path, /*overwrite*/ false)
            .unwrap();

        let (mut destination, destination_id, _) = controller_with_session();
        let mut conflicting = imported;
        conflicting.text = "Independently edited local review text.".to_string();
        destination
            .session_mut(&destination_id)
            .unwrap()
            .review
            .drafts
            .push(conflicting.clone());
        let before = destination.session(&destination_id).unwrap().clone();

        assert!(matches!(
            destination.preview_review_bundle(&destination_id, &path),
            Err(ReplayError::ReviewBundleConflict(_)),
        ));
        assert_eq!(destination.session(&destination_id).unwrap(), &before);
        assert!(destination
            .session(&destination_id)
            .unwrap()
            .notes
            .is_empty());
    }

    #[test]
    fn portable_fix_proposals_require_the_locally_verified_original_author() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("author-review.json");
        let (mut author, author_id, author_step) =
            controller_with_pull_request("alice", Some("alice"));
        author
            .add_review_draft(
                &author_id,
                Some(&author_step),
                ReplayReviewDraftKind::CodeFix,
                "Bound the original PR refresh.",
            )
            .unwrap();
        author
            .save_review_bundle(&author_id, &path, /*overwrite*/ false)
            .unwrap();

        let (reviewer, reviewer_id, _) = controller_with_pull_request("alice", Some("bob"));
        assert!(matches!(
            reviewer.preview_review_bundle(&reviewer_id, &path),
            Err(ReplayError::InvalidReviewBundle(_)),
        ));
        assert!(reviewer
            .session(&reviewer_id)
            .unwrap()
            .review
            .drafts
            .is_empty());
    }

    #[test]
    fn loading_a_portable_review_requires_explicit_preview_confirmation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("review.json");
        let (mut original, original_id, step_id) = controller_with_session();
        original
            .add_review_draft(
                &original_id,
                Some(&step_id),
                ReplayReviewDraftKind::InlineComment,
                "Import only after the reviewer accepts the exact preview.",
            )
            .unwrap();
        original
            .save_review_bundle(&original_id, &path, /*overwrite*/ false)
            .unwrap();
        let (mut destination, destination_id, _) = controller_with_session();
        let preview = destination
            .preview_review_bundle(&destination_id, &path)
            .unwrap();

        assert!(matches!(
            destination.import_review_bundle(
                &destination_id,
                &path,
                &preview.bundle_digest,
                /*confirmed*/ false,
            ),
            Err(ReplayError::ReviewBundleConfirmationRequired),
        ));
        assert!(destination
            .session(&destination_id)
            .unwrap()
            .review
            .drafts
            .is_empty());
    }

    #[test]
    fn portable_review_rejects_a_file_changed_after_the_import_preview() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("review.json");
        let (mut original, original_id, step_id) = controller_with_session();
        original
            .add_review_draft(
                &original_id,
                Some(&step_id),
                ReplayReviewDraftKind::InlineComment,
                "Pin the exact original reviewed file contents.",
            )
            .unwrap();
        original
            .save_review_bundle(&original_id, &path, /*overwrite*/ false)
            .unwrap();
        let (mut destination, destination_id, _) = controller_with_session();
        let preview = destination
            .preview_review_bundle(&destination_id, &path)
            .unwrap();
        let mut changed = read_portable_review(&path);
        changed.exported_at_ms = changed.exported_at_ms.saturating_add(1);
        replace_portable_review(&path, &changed);

        assert!(matches!(
            destination.import_review_bundle(
                &destination_id,
                &path,
                &preview.bundle_digest,
                /*confirmed*/ true,
            ),
            Err(ReplayError::InvalidReviewBundle(_)),
        ));
        assert!(destination
            .session(&destination_id)
            .unwrap()
            .review
            .drafts
            .is_empty());
    }

    #[test]
    fn replacing_a_saved_review_file_requires_explicit_overwrite_confirmation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("review.json");
        let (mut controller, session_id, step_id) = controller_with_session();
        controller
            .add_review_draft(
                &session_id,
                Some(&step_id),
                ReplayReviewDraftKind::InlineComment,
                "Keep the original review until replacement is accepted.",
            )
            .unwrap();
        controller
            .save_review_bundle(&session_id, &path, /*overwrite*/ false)
            .unwrap();
        let original = std::fs::read(&path).unwrap();
        controller
            .add_review_draft(
                &session_id,
                None,
                ReplayReviewDraftKind::ReviewSummary,
                "Include a separately reviewed PR summary.",
            )
            .unwrap();

        assert!(matches!(
            controller.save_review_bundle(&session_id, &path, /*overwrite*/ false),
            Err(ReplayError::ReviewBundleExists(_)),
        ));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let saved = controller
            .save_review_bundle(&session_id, &path, /*overwrite*/ true)
            .unwrap();
        assert_eq!(saved.draft_count, 2);
        assert_eq!(read_portable_review(&path).drafts.len(), 2);
    }

    #[test]
    fn empty_portable_reviews_never_create_a_file_or_directory() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("missing").join("review.json");
        let (controller, session_id, _) = controller_with_session();

        assert!(matches!(
            controller.save_review_bundle(&session_id, &path, /*overwrite*/ false),
            Err(ReplayError::InvalidReviewBundle(_)),
        ));
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn portable_review_rejects_oversized_files_before_reading_them() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("oversized.json");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_REPLAY_REVIEW_BUNDLE_BYTES.saturating_add(1))
            .unwrap();
        let (controller, session_id, _) = controller_with_session();

        assert!(matches!(
            controller.preview_review_bundle(&session_id, &path),
            Err(ReplayError::LimitExceeded { .. }),
        ));
    }

    #[cfg(unix)]
    #[test]
    fn portable_review_never_follows_symbolic_link_files() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let original_path = directory.path().join("original.json");
        let linked_path = directory.path().join("linked.json");
        let (mut controller, session_id, step_id) = controller_with_session();
        controller
            .add_review_draft(
                &session_id,
                Some(&step_id),
                ReplayReviewDraftKind::InlineComment,
                "Do not open or replace a symbolic link.",
            )
            .unwrap();
        controller
            .save_review_bundle(&session_id, &original_path, /*overwrite*/ false)
            .unwrap();
        symlink(&original_path, &linked_path).unwrap();

        assert!(matches!(
            controller.preview_review_bundle(&session_id, &linked_path),
            Err(ReplayError::InvalidReviewBundle(_)),
        ));
        assert!(matches!(
            controller.save_review_bundle(&session_id, &linked_path, /*overwrite*/ true),
            Err(ReplayError::InvalidReviewBundle(_)),
        ));
        assert_eq!(read_portable_review(&original_path).drafts.len(), 1);
    }

    #[test]
    fn suggested_portable_review_stays_inside_shared_git_metadata() {
        let (controller, session_id, _) = controller_with_pull_request("alice", Some("reviewer"));
        let session = controller.session(&session_id).unwrap();
        let suggested = super::super::suggested_review_bundle_path(&session.source);

        assert!(suggested.starts_with(&session.source.repository.common_directory));
        assert!(!suggested.starts_with(&session.workspace.root));
        assert_eq!(
            suggested.file_name().unwrap(),
            "pr-482-bbbbbbb.red-review.json",
        );
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
