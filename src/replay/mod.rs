//! Editor-owned pull request replay, source identity, and review exercises.
//!
//! Replay intentionally keeps GitHub access, worktree creation, patch parsing,
//! staged edits, and recovery under the Rust editor. The bundled Husk plugin
//! receives bounded presentation snapshots rather than process permissions.

mod demo;
mod patch;
mod session;
mod source;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub use demo::{replay_demo_plan, ReplayDemoPlan, ReplayDemoStep};
pub use patch::{
    parse_patch, ReplayChangeKind, ReplayFilePatch, ReplayHunk, ReplayHunkRange, ReplayPatch,
};
pub use session::{
    ReplayCompletion, ReplayController, ReplayMode, ReplayNote, ReplayNoteCategory,
    ReplayRecoverySnapshot, ReplaySession, ReplaySessionState, ReplayStage, ReplayStep,
    ReplayStepKind, ReplayStepStatus, ReplayValidation, ReplayWorkspace, ReplayWorkspacePreview,
};
pub use source::{
    fetch_pull_request_objects, finalize_pull_request, prepare_workspace,
    resolve_local_branch_source, resolve_local_source, resolve_pull_request, GitObjectId,
    PullRequestInput, ReplayCommitSummary, ReplayPullRequest, ReplayRepository,
    ReplayResolvedLocalBranch, ReplayResolvedPullRequest, ReplayReviewContext, ReplaySource,
    ReplaySourceKind,
};

/// Upper bounds enforced before a source becomes a replayable plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReplayLimits {
    /// Maximum complete canonical Git diff in bytes.
    pub max_patch_bytes: usize,
    /// Maximum complete canonical Git diff in lines.
    pub max_patch_lines: usize,
    /// Maximum number of changed files.
    pub max_changed_files: usize,
    /// Maximum number of compiled replay exercises.
    pub max_steps: usize,
    /// Maximum bounded GitHub metadata response in bytes.
    pub max_metadata_bytes: usize,
    /// Maximum original pull request description in bytes.
    pub max_description_bytes: usize,
    /// Maximum source commit descriptions retained in the review context.
    pub max_commit_summaries: usize,
    /// Maximum local reviewer observation in bytes.
    pub max_note_bytes: usize,
}

impl Default for ReplayLimits {
    fn default() -> Self {
        Self {
            max_patch_bytes: 8 * 1024 * 1024,
            max_patch_lines: 100_000,
            max_changed_files: 250,
            max_steps: 2_000,
            max_metadata_bytes: 1024 * 1024,
            max_description_bytes: 256 * 1024,
            max_commit_summaries: 250,
            max_note_bytes: 64 * 1024,
        }
    }
}

/// Structured, non-secret failures returned across the replay host boundary.
#[derive(Debug, Error)]
pub enum ReplayError {
    /// The requested pull request number or URL is malformed.
    #[error("invalid pull request: {0}")]
    InvalidPullRequest(String),
    /// The current directory does not belong to a usable Git repository.
    #[error("repository discovery failed: {0}")]
    RepositoryMissing(String),
    /// A returned pull request does not belong to the selected repository.
    #[error("pull request repository does not match the current repository")]
    RepositoryMismatch,
    /// A Git object is malformed or not immutable.
    #[error("invalid Git object identity: {0}")]
    InvalidObject(String),
    /// A required immutable Git object has not been explicitly fetched.
    #[error("source Git objects are not available; explicitly confirm the fetch first")]
    MissingObjects,
    /// The pinned remote reference moved between resolution and fetch.
    #[error("the source reference changed after the pull request was resolved")]
    SourceRefMoved,
    /// Git or the configured GitHub CLI could not perform a bounded operation.
    #[error("{program} failed: {message}")]
    CommandFailed {
        /// Trusted executable name.
        program: String,
        /// Bounded, sanitized command diagnostic.
        message: String,
    },
    /// A complete response would exceed its declared safe limit.
    #[error("{kind} exceeds the replay limit of {limit}")]
    LimitExceeded {
        /// Human-readable bounded resource.
        kind: &'static str,
        /// Maximum allowed bytes, lines, files, or steps.
        limit: usize,
    },
    /// A unified patch is incomplete, malformed, or inconsistent.
    #[error("invalid unified patch: {0}")]
    InvalidPatch(String),
    /// A source path would escape the selected replay workspace.
    #[error("unsafe replay path: {0}")]
    UnsafePath(String),
    /// A requested source, session, or replay step no longer exists.
    #[error("{kind} was not found: {id}")]
    NotFound {
        /// Missing entity type.
        kind: &'static str,
        /// Opaque entity identifier.
        id: String,
    },
    /// A planned step cannot be unambiguously applied to the visible text.
    #[error("replay anchor is ambiguous or no longer matches the visible buffer")]
    AnchorConflict,
    /// A single-use preview is stale, foreign, or already consumed.
    #[error("replay preview is stale or has already been consumed")]
    StalePreview,
    /// A source file operation cannot be represented by an editor transaction.
    #[error("unsupported replay file operation: {0}")]
    UnsupportedOperation(String),
    /// A worktree cannot be created without the explicit confirmation flag.
    #[error("scratch worktree creation requires explicit confirmation")]
    WorkspaceConfirmationRequired,
    /// A pre-existing scratch branch or worktree must never be overwritten.
    #[error("the replay workspace already exists: {0}")]
    WorkspaceExists(String),
    /// A required dependency has not been completed.
    #[error("the replay step is blocked by an unfinished prerequisite")]
    DependencyBlocked,
    /// JSON returned by the trusted GitHub provider was invalid.
    #[error("invalid GitHub pull request metadata: {0}")]
    InvalidMetadata(String),
    /// A reviewer note was empty or larger than its configured limit.
    #[error("invalid review observation: {0}")]
    InvalidReviewNote(String),
    /// A safe local filesystem operation could not be completed.
    #[error("replay filesystem operation failed: {0}")]
    Filesystem(String),
}

impl ReplayError {
    /// Returns the stable error code exposed to Husk callbacks.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidPullRequest(_) => "pull_request_invalid",
            Self::RepositoryMissing(_) => "repository_missing",
            Self::RepositoryMismatch => "repository_mismatch",
            Self::InvalidObject(_) => "source_identity_invalid",
            Self::MissingObjects => "source_objects_missing",
            Self::SourceRefMoved => "source_ref_moved",
            Self::CommandFailed { .. } => "source_command_failed",
            Self::LimitExceeded { .. } => "source_too_large",
            Self::InvalidPatch(_) => "source_patch_invalid",
            Self::UnsafePath(_) => "path_unsafe",
            Self::NotFound { .. } => "replay_not_found",
            Self::AnchorConflict => "anchor_ambiguous",
            Self::StalePreview => "preview_expired",
            Self::UnsupportedOperation(_) => "file_unsupported",
            Self::WorkspaceConfirmationRequired => "workspace_confirmation_required",
            Self::WorkspaceExists(_) => "workspace_exists",
            Self::DependencyBlocked => "dependency_blocked",
            Self::InvalidMetadata(_) => "pull_request_metadata_invalid",
            Self::InvalidReviewNote(_) => "review_note_invalid",
            Self::Filesystem(_) => "replay_filesystem_failed",
        }
    }

    /// Produces a bounded structured error for a plugin callback.
    #[must_use]
    pub fn payload(&self) -> serde_json::Value {
        serde_json::json!({
            "version": 1,
            "ok": false,
            "code": self.code(),
            "message": self.to_string(),
        })
    }
}

pub(crate) fn digest(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}
