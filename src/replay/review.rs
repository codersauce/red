//! Explicitly approved, atomic GitHub reviews of pinned original PR sources.

use std::{
    collections::HashSet,
    path::{Component, Path},
    process::Command,
};

use serde::{Deserialize, Serialize};
use url::Url;

use super::{
    digest, now_ms, refresh_pull_request_capabilities,
    source::{run_command, run_command_with_input, validate_relative_path, ReplayCommandFailure},
    GitObjectId, ReplayController, ReplayDiffSide, ReplayDraftOrigin, ReplayDraftState,
    ReplayError, ReplayLimits, ReplayPullRequest, ReplayReviewDraft, ReplayReviewDraftKind,
    ReplayReviewRole, ReplaySession, ReplaySource, ReplaySourceKind,
};

/// User-selected result of a single, explicitly submitted GitHub PR review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayReviewOutcome {
    /// Publish observations without approving or requesting changes.
    Comment,
    /// Approve another author's exact, pinned pull-request head.
    Approve,
    /// Request changes to another author's exact, pinned pull-request head.
    RequestChanges,
}

impl ReplayReviewOutcome {
    /// Returns the mandatory event for GitHub's atomic create-review endpoint.
    #[must_use]
    pub const fn github_event(self) -> &'static str {
        match self {
            Self::Comment => "COMMENT",
            Self::Approve => "APPROVE",
            Self::RequestChanges => "REQUEST_CHANGES",
        }
    }

    const fn github_state(self) -> &'static str {
        match self {
            Self::Comment => "COMMENTED",
            Self::Approve => "APPROVED",
            Self::RequestChanges => "CHANGES_REQUESTED",
        }
    }
}

/// Complete, local-only preview of one potential atomic GitHub review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayReviewSubmissionPreview {
    /// Exact editor-owned review workspace; never an original PR checkout.
    pub workspace_id: String,
    /// Verified original GitHub host, base owner, and repository.
    pub repository: String,
    /// Verified original pull-request number.
    pub pull_request: u64,
    /// Canonical, original pull-request URL.
    pub pull_request_url: String,
    /// Full immutable original head that will receive the submitted review.
    pub target_commit: GitObjectId,
    /// Authenticated GitHub viewer whose identity the worker rechecks.
    pub viewer: String,
    /// Explicitly chosen, non-pending GitHub review event.
    pub outcome: ReplayReviewOutcome,
    /// Exact local human and agent drafts included in this submission.
    pub drafts: Vec<ReplayReviewDraft>,
    /// Number of original-diff-anchored comments included in the request.
    pub inline_comment_count: usize,
    /// Number of user-composed PR-level summaries included in the request.
    pub summary_count: usize,
    /// Number of agent-proposed drafts still requiring human confirmation.
    pub agent_draft_count: usize,
    /// Author fix proposals which remain strictly local and are not submitted.
    pub local_fix_count: usize,
    /// Exact PR-level body that GitHub will receive, including any safe default.
    pub body: String,
    /// Editor-owned generation invalidating this preview after any local edit.
    pub generation: u64,
    /// Digest of the complete original identity, event, generation, and drafts.
    pub preview_digest: String,
}

/// Provenance of a submitted-review receipt retained in local Replay state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayReceiptVerification {
    /// The owning editor verified the exact GitHub review and original drafts.
    #[default]
    Verified,
    /// A portable file claimed this receipt; GitHub has not verified it here.
    Unverified,
}

impl ReplayReceiptVerification {
    const fn is_verified(&self) -> bool {
        matches!(self, Self::Verified)
    }
}

/// Crash-recoverable publication state for one exact, human-approved review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayReviewSubmissionState {
    /// The exact request was durably recorded before the provider worker ran.
    InFlight,
    /// The request may have reached GitHub and requires explicit reconciliation.
    Uncertain,
}

/// Exact local record that prevents blind retry after a crash or lost receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayPendingReviewSubmission {
    /// Original PR, viewer, outcome, body, and source-anchored draft identities.
    pub preview: ReplayReviewSubmissionPreview,
    /// SHA-256 of the exact event-bearing JSON request accepted by the user.
    pub payload_digest: String,
    /// Unix-millisecond time when the editor committed the local safety record.
    pub started_at_ms: u64,
    /// Whether the original worker is active or the provider result is unknown.
    pub state: ReplayReviewSubmissionState,
}

/// Verified, portable receipt returned only after GitHub submits a real review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayReviewReceipt {
    /// Positive, provider-assigned GitHub pull-request review ID.
    pub id: u64,
    /// Canonical, provider-confirmed URL for this submitted review.
    pub url: String,
    /// Exact non-pending outcome selected by the reviewer.
    pub outcome: ReplayReviewOutcome,
    /// Full original head verified again immediately before submission.
    pub target_commit: GitObjectId,
    /// Authenticated GitHub viewer that actually submitted this review.
    pub viewer: String,
    /// Stable local identities of every original-source draft included.
    pub draft_ids: Vec<String>,
    /// SHA-256 of the exact atomic GitHub JSON request body.
    pub payload_digest: String,
    /// Provider-confirmed submission time; pending reviews have no such time.
    pub submitted_at: String,
    /// Portable receipts lose trusted provenance until independently verified.
    #[serde(
        default,
        skip_serializing_if = "ReplayReceiptVerification::is_verified"
    )]
    pub verification: ReplayReceiptVerification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct GitHubReviewComment {
    path: String,
    line: usize,
    side: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_side: Option<&'static str>,
    body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct GitHubReviewRequest {
    commit_id: GitObjectId,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
    /// Required by construction; omitting this would create a PENDING review.
    event: &'static str,
    comments: Vec<GitHubReviewComment>,
}

/// A content-pinned review that can be submitted only after human confirmation.
#[derive(Debug)]
pub(crate) struct PreparedReplayReviewSubmission {
    source: ReplaySource,
    pub(crate) preview: ReplayReviewSubmissionPreview,
    request: GitHubReviewRequest,
}

/// Exact original-source data required for a bounded read-only GitHub lookup.
#[derive(Debug)]
pub(crate) struct PreparedReplayReviewReconciliation {
    source: ReplaySource,
    preview: ReplayReviewSubmissionPreview,
    request: GitHubReviewRequest,
    payload_digest: String,
    imported_receipt_id: Option<u64>,
}

/// Provider-confirmed result of one explicit, non-mutating review lookup.
#[derive(Debug)]
pub enum ReplayReviewReconciliation {
    /// Exactly one source-, viewer-, outcome-, body-, and comment-matched review.
    Verified {
        preview: Box<ReplayReviewSubmissionPreview>,
        receipt: Box<ReplayReviewReceipt>,
    },
    /// GitHub completed its bounded lookup and has no matching original review.
    NotFound {
        /// Imported receipt to release, if this was not an in-flight submission.
        imported_receipt_id: Option<u64>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GitHubSubmittedReview {
    id: u64,
    html_url: String,
    state: String,
    commit_id: String,
    #[serde(default)]
    body: Option<String>,
    user: GitHubReviewUser,
    #[serde(default)]
    submitted_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GitHubReviewUser {
    login: String,
}

#[derive(Debug, Deserialize)]
struct GitHubSubmittedReviewComment {
    path: String,
    body: String,
    #[serde(default)]
    line: Option<usize>,
    #[serde(default)]
    original_line: Option<usize>,
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    original_start_line: Option<usize>,
    #[serde(default)]
    side: Option<String>,
    #[serde(default)]
    start_side: Option<String>,
    #[serde(default)]
    commit_id: Option<String>,
    #[serde(default)]
    original_commit_id: Option<String>,
}

const MAX_RECONCILIATION_PAGES: usize = 10;
const RECONCILIATION_PAGE_SIZE: usize = 100;
type ReplayProviderRead<'a> =
    dyn FnMut(&ReplaySource, &str, &str, ReplayLimits) -> Result<Vec<u8>, ReplayError> + 'a;

impl ReplayController {
    /// Previews all eligible local outcomes without writing or contacting GitHub.
    pub fn preview_review_submission(
        &self,
        session_id: &str,
        outcome: ReplayReviewOutcome,
    ) -> Result<ReplayReviewSubmissionPreview, ReplayError> {
        let session = self.session(session_id)?;
        if session.review.pending_submission.is_some() {
            return Err(ReplayError::ReviewSubmissionUncertain(
                "reconcile the previous confirmed review with GitHub before creating another submission"
                    .to_string(),
            ));
        }
        build_review_submission(session, self.limits(), outcome).map(|(preview, _)| preview)
    }

    /// Freezes the exact human-confirmed preview for the bounded network worker.
    pub(crate) fn prepare_review_submission(
        &self,
        session_id: &str,
        outcome: ReplayReviewOutcome,
        expected_digest: &str,
        confirmed: bool,
    ) -> Result<PreparedReplayReviewSubmission, ReplayError> {
        if !confirmed {
            return Err(ReplayError::ReviewSubmissionConfirmationRequired);
        }
        let session = self.session(session_id)?;
        if session.review.pending_submission.is_some() {
            return Err(ReplayError::ReviewSubmissionUncertain(
                "the previous confirmed review must be reconciled before another request is sent"
                    .to_string(),
            ));
        }
        let (preview, request) = build_review_submission(session, self.limits(), outcome)?;
        if preview.preview_digest != expected_digest {
            return Err(ReplayError::StalePreview);
        }
        Ok(PreparedReplayReviewSubmission {
            source: session.source.clone(),
            preview,
            request,
        })
    }

    /// Pins the accepted review before the editor durably saves and starts POST.
    pub(crate) fn begin_review_submission(
        &mut self,
        session_id: &str,
        outcome: ReplayReviewOutcome,
        expected_digest: &str,
        confirmed: bool,
    ) -> Result<PreparedReplayReviewSubmission, ReplayError> {
        let submission =
            self.prepare_review_submission(session_id, outcome, expected_digest, confirmed)?;
        let encoded = serde_json::to_vec(&submission.request).map_err(|error| {
            ReplayError::InvalidReviewDraft(format!(
                "cannot pin the exact approved GitHub review request: {error}"
            ))
        })?;
        let pending = ReplayPendingReviewSubmission {
            preview: submission.preview.clone(),
            payload_digest: digest(&encoded),
            started_at_ms: now_ms(),
            state: ReplayReviewSubmissionState::InFlight,
        };
        self.session_mut(session_id)?.review.pending_submission = Some(pending);
        self.advance_generation();
        Ok(submission)
    }

    /// Preserves provider uncertainty until an explicit bounded lookup resolves it.
    pub(crate) fn mark_review_submission_uncertain(
        &mut self,
        session_id: &str,
    ) -> Result<(), ReplayError> {
        let pending = self
            .session_mut(session_id)?
            .review
            .pending_submission
            .as_mut()
            .ok_or_else(|| {
                ReplayError::ReviewSubmissionUncertain(
                    "the durable approved review record is missing".to_string(),
                )
            })?;
        pending.state = ReplayReviewSubmissionState::Uncertain;
        self.advance_generation();
        Ok(())
    }

    /// Removes a request only after proving it never ran or explicitly finding no review.
    pub(crate) fn clear_review_submission(&mut self, session_id: &str) -> Result<(), ReplayError> {
        if self
            .session_mut(session_id)?
            .review
            .pending_submission
            .take()
            .is_some()
        {
            self.advance_generation();
        }
        Ok(())
    }

    /// Freezes an uncertain submit or explicitly imported unverified receipt.
    pub(crate) fn prepare_review_reconciliation(
        &self,
        session_id: &str,
    ) -> Result<PreparedReplayReviewReconciliation, ReplayError> {
        let session = self.session(session_id)?;
        let Some(pending) = session.review.pending_submission.as_ref() else {
            return self.prepare_imported_review_reconciliation(session);
        };
        let (current, request) =
            build_review_submission(session, self.limits(), pending.preview.outcome)?;
        if current.workspace_id != pending.preview.workspace_id
            || current.repository != pending.preview.repository
            || current.pull_request != pending.preview.pull_request
            || current.pull_request_url != pending.preview.pull_request_url
            || current.target_commit != pending.preview.target_commit
            || current.viewer != pending.preview.viewer
            || current.outcome != pending.preview.outcome
            || current.body != pending.preview.body
            || current.drafts != pending.preview.drafts
        {
            return Err(ReplayError::ReviewSubmissionUncertain(
                "the durable confirmed review no longer matches its exact original drafts"
                    .to_string(),
            ));
        }
        let encoded = serde_json::to_vec(&request).map_err(|error| {
            ReplayError::ReviewSubmissionUncertain(format!(
                "cannot reconstruct the exact confirmed GitHub request: {error}"
            ))
        })?;
        if digest(&encoded) != pending.payload_digest {
            return Err(ReplayError::ReviewSubmissionUncertain(
                "the recovered review payload no longer matches its exact approved digest"
                    .to_string(),
            ));
        }
        Ok(PreparedReplayReviewReconciliation {
            source: session.source.clone(),
            preview: pending.preview.clone(),
            request,
            payload_digest: pending.payload_digest.clone(),
            imported_receipt_id: None,
        })
    }

    fn prepare_imported_review_reconciliation(
        &self,
        session: &ReplaySession,
    ) -> Result<PreparedReplayReviewReconciliation, ReplayError> {
        let receipt = session
            .review
            .receipts
            .iter()
            .find(|receipt| receipt.verification == ReplayReceiptVerification::Unverified)
            .ok_or_else(|| {
                ReplayError::InvalidReviewDraft(
                    "there is no uncertain review or imported receipt to verify".to_string(),
                )
            })?;
        let expected = receipt
            .draft_ids
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let mut isolated = session.clone();
        isolated
            .review
            .drafts
            .retain(|draft| expected.contains(draft.id.as_str()));
        for draft in &mut isolated.review.drafts {
            draft.state = ReplayDraftState::Local;
        }
        let (preview, request) =
            build_review_submission(&isolated, self.limits(), receipt.outcome)?;
        if preview.viewer != receipt.viewer
            || preview.target_commit != receipt.target_commit
            || preview
                .drafts
                .iter()
                .map(|draft| draft.id.as_str())
                .collect::<Vec<_>>()
                != receipt
                    .draft_ids
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
        {
            return Err(ReplayError::InvalidReviewDraft(
                "the imported receipt does not match the verified viewer or its original drafts"
                    .to_string(),
            ));
        }
        Ok(PreparedReplayReviewReconciliation {
            source: session.source.clone(),
            preview,
            request,
            payload_digest: receipt.payload_digest.clone(),
            imported_receipt_id: Some(receipt.id),
        })
    }

    /// Releases one imported claim only after GitHub proves no such review exists.
    pub(crate) fn clear_unverified_review_receipt(
        &mut self,
        session_id: &str,
        receipt_id: u64,
    ) -> Result<(), ReplayError> {
        let session = self.session_mut(session_id)?;
        let previous_len = session.review.receipts.len();
        session.review.receipts.retain(|receipt| {
            receipt.id != receipt_id
                || receipt.verification != ReplayReceiptVerification::Unverified
        });
        if session.review.receipts.len() != previous_len {
            session.generation = session.generation.saturating_add(1);
            self.advance_generation();
        }
        Ok(())
    }

    /// Marks exactly the provider-verified drafts submitted on the editor thread.
    pub(crate) fn record_review_submission(
        &mut self,
        session_id: &str,
        preview: &ReplayReviewSubmissionPreview,
        receipt: ReplayReviewReceipt,
    ) -> Result<ReplayReviewReceipt, ReplayError> {
        let session = self.session(session_id)?;
        let request = session.source.pull_request.as_ref().ok_or_else(|| {
            ReplayError::ReviewSubmissionUncertain(
                "the original pull request disappeared after GitHub accepted the review"
                    .to_string(),
            )
        })?;
        validate_refreshed_review_identity(request, preview)
            .map_err(|error| ReplayError::ReviewSubmissionUncertain(error.to_string()))?;
        if receipt.verification != ReplayReceiptVerification::Verified {
            return Err(ReplayError::ReviewSubmissionUncertain(
                "an imported review receipt must be verified directly with GitHub".to_string(),
            ));
        }
        if let Some(pending) = session.review.pending_submission.as_ref() {
            if pending.preview != *preview || pending.payload_digest != receipt.payload_digest {
                return Err(ReplayError::ReviewSubmissionUncertain(
                    "the submitted review does not match its durable approved request".to_string(),
                ));
            }
        }
        if let Some(existing) = session
            .review
            .receipts
            .iter()
            .find(|existing| {
                existing.id == receipt.id
                    && existing.verification == ReplayReceiptVerification::Verified
            })
            .cloned()
        {
            if existing == receipt {
                if session.review.pending_submission.is_some() {
                    self.session_mut(session_id)?.review.pending_submission = None;
                    self.advance_generation();
                }
                return Ok(existing);
            }
            return Err(ReplayError::ReviewSubmissionUncertain(
                "the submitted GitHub review ID belongs to a different verified receipt"
                    .to_string(),
            ));
        }
        if preview.workspace_id != session.id
            || receipt.id == 0
            || !review_receipt_matches_original_pull_request(request, &receipt)
            || receipt.outcome != preview.outcome
            || receipt.target_commit != preview.target_commit
            || !receipt.viewer.eq_ignore_ascii_case(&preview.viewer)
            || receipt.payload_digest.len() != 64
            || !receipt
                .payload_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || receipt.submitted_at.trim().is_empty()
            || receipt.draft_ids
                != preview
                    .drafts
                    .iter()
                    .map(|draft| draft.id.clone())
                    .collect::<Vec<_>>()
        {
            return Err(ReplayError::ReviewSubmissionUncertain(
                "the accepted GitHub review receipt does not match the approved original drafts"
                    .to_string(),
            ));
        }
        for expected in &preview.drafts {
            let actual = session
                .review
                .drafts
                .iter()
                .find(|draft| draft.id == expected.id);
            if actual != Some(expected) {
                return Err(ReplayError::ReviewSubmissionUncertain(
                    "an approved original draft changed after GitHub accepted the review"
                        .to_string(),
                ));
            }
        }

        let draft_ids = receipt
            .draft_ids
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let session = self.session_mut(session_id)?;
        session.review.receipts.retain(|existing| {
            existing.id != receipt.id
                || existing.verification != ReplayReceiptVerification::Unverified
        });
        for draft in &mut session.review.drafts {
            if draft_ids.contains(draft.id.as_str()) {
                draft.state = ReplayDraftState::Submitted;
            }
        }
        session.review.receipts.push(receipt.clone());
        session.review.pending_submission = None;
        session.generation = session.generation.saturating_add(1);
        self.advance_generation();
        Ok(receipt)
    }
}

fn build_review_submission(
    session: &ReplaySession,
    limits: ReplayLimits,
    outcome: ReplayReviewOutcome,
) -> Result<(ReplayReviewSubmissionPreview, GitHubReviewRequest), ReplayError> {
    if session.source.kind != ReplaySourceKind::GitHubPullRequest {
        return Err(ReplayError::InvalidReviewDraft(
            "only an original GitHub pull request can receive a GitHub review".to_string(),
        ));
    }
    let request = session.source.pull_request.as_ref().ok_or_else(|| {
        ReplayError::InvalidReviewDraft(
            "the original GitHub pull request identity is not available".to_string(),
        )
    })?;
    if request.head_commit != session.source.target_commit {
        return Err(ReplayError::SourceRefMoved);
    }
    let viewer = request.capabilities.viewer.as_ref().ok_or_else(|| {
        ReplayError::InvalidReviewDraft(
            "verify your authenticated GitHub identity before publishing a review".to_string(),
        )
    })?;
    let actual_role = ReplayReviewRole::from_pull_request(Some(request));
    if actual_role != session.review.role {
        return Err(ReplayError::InvalidReviewDraft(
            "the authenticated GitHub viewer no longer matches this review role".to_string(),
        ));
    }
    if actual_role == ReplayReviewRole::Author && outcome != ReplayReviewOutcome::Comment {
        return Err(ReplayError::InvalidReviewDraft(
            "the original PR author cannot approve or request changes to their own pull request"
                .to_string(),
        ));
    }

    let local_fix_count = session
        .review
        .drafts
        .iter()
        .filter(|draft| {
            draft.state == ReplayDraftState::Local && draft.kind == ReplayReviewDraftKind::CodeFix
        })
        .count();
    let mut drafts = Vec::new();
    let mut comments = Vec::new();
    let mut summaries = Vec::new();
    let mut agent_draft_count = 0;

    for draft in &session.review.drafts {
        if draft.state != ReplayDraftState::Local || draft.kind == ReplayReviewDraftKind::CodeFix {
            continue;
        }
        if draft.target_commit != session.source.target_commit
            || draft.text.trim().is_empty()
            || draft.text.len() > limits.max_note_bytes
        {
            return Err(ReplayError::InvalidReviewDraft(
                "a review draft does not match the exact original pull-request head".to_string(),
            ));
        }
        match draft.kind {
            ReplayReviewDraftKind::InlineComment => {
                comments.push(original_review_comment(session, draft, limits)?);
            }
            ReplayReviewDraftKind::ReviewSummary => {
                if draft.step_id.is_some() || draft.path.is_some() || draft.anchor.is_some() {
                    return Err(ReplayError::InvalidReviewDraft(
                        "a pull-request summary cannot impersonate an inline comment".to_string(),
                    ));
                }
                summaries.push(draft.text.as_str());
            }
            ReplayReviewDraftKind::CodeFix => continue,
        }
        agent_draft_count += usize::from(draft.origin == ReplayDraftOrigin::Agent);
        drafts.push(draft.clone());
    }

    if drafts.is_empty() {
        return Err(ReplayError::InvalidReviewDraft(
            "add a local inline comment or PR-level summary before publishing a GitHub review"
                .to_string(),
        ));
    }
    if outcome == ReplayReviewOutcome::RequestChanges && summaries.is_empty() {
        return Err(ReplayError::InvalidReviewDraft(
            "requesting changes requires a PR-level summary; use s to explain the requested changes"
                .to_string(),
        ));
    }

    let body = if summaries.is_empty() && outcome == ReplayReviewOutcome::Comment {
        "Inline review comments.".to_string()
    } else {
        summaries.join("\n\n")
    };
    let api_request = GitHubReviewRequest {
        commit_id: session.source.target_commit.clone(),
        body: (!body.is_empty()).then(|| body.clone()),
        event: outcome.github_event(),
        comments,
    };
    let encoded_request = serde_json::to_vec(&api_request).map_err(|error| {
        ReplayError::InvalidReviewDraft(format!(
            "cannot encode the original GitHub review: {error}"
        ))
    })?;
    if encoded_request.len() > limits.max_metadata_bytes {
        return Err(ReplayError::LimitExceeded {
            kind: "GitHub review submission",
            limit: limits.max_metadata_bytes,
        });
    }

    let mut preview = ReplayReviewSubmissionPreview {
        workspace_id: session.id.clone(),
        repository: format!(
            "{}/{}/{}",
            request.host, request.repository_owner, request.repository_name
        ),
        pull_request: request.number,
        pull_request_url: request.url.clone(),
        target_commit: session.source.target_commit.clone(),
        viewer: viewer.clone(),
        outcome,
        inline_comment_count: api_request.comments.len(),
        summary_count: summaries.len(),
        agent_draft_count,
        local_fix_count,
        body,
        generation: session.generation,
        drafts,
        preview_digest: String::new(),
    };
    let fingerprint = serde_json::to_vec(&preview).map_err(|error| {
        ReplayError::InvalidReviewDraft(format!("cannot pin the GitHub review preview: {error}"))
    })?;
    if fingerprint.len() > limits.max_metadata_bytes {
        return Err(ReplayError::LimitExceeded {
            kind: "GitHub review preview",
            limit: limits.max_metadata_bytes,
        });
    }
    preview.preview_digest = digest(&fingerprint);
    Ok((preview, api_request))
}

/// Rejects durable request records that no longer match their exact source or drafts.
pub(super) fn validate_recovered_pending_submission(
    session: &ReplaySession,
    pending: &ReplayPendingReviewSubmission,
    limits: ReplayLimits,
) -> Result<(), ReplayError> {
    if pending.started_at_ms == 0
        || pending.payload_digest.len() != 64
        || !pending
            .payload_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ReplayError::InvalidReviewDraft(
            "the recovered confirmed-review record is malformed".to_string(),
        ));
    }

    let mut fingerprint = pending.preview.clone();
    let expected_preview_digest = std::mem::take(&mut fingerprint.preview_digest);
    let encoded_fingerprint = serde_json::to_vec(&fingerprint).map_err(|error| {
        ReplayError::InvalidReviewDraft(format!(
            "cannot validate the recovered confirmed-review preview: {error}"
        ))
    })?;
    if digest(&encoded_fingerprint) != expected_preview_digest {
        return Err(ReplayError::InvalidReviewDraft(
            "the recovered confirmed-review preview no longer matches its digest".to_string(),
        ));
    }

    let (current, request) = build_review_submission(session, limits, pending.preview.outcome)?;
    if current.workspace_id != pending.preview.workspace_id
        || current.repository != pending.preview.repository
        || current.pull_request != pending.preview.pull_request
        || current.pull_request_url != pending.preview.pull_request_url
        || current.target_commit != pending.preview.target_commit
        || current.viewer != pending.preview.viewer
        || current.outcome != pending.preview.outcome
        || current.drafts != pending.preview.drafts
        || current.body != pending.preview.body
    {
        return Err(ReplayError::InvalidReviewDraft(
            "the recovered confirmed review no longer matches the original source and drafts"
                .to_string(),
        ));
    }
    let payload = serde_json::to_vec(&request).map_err(|error| {
        ReplayError::InvalidReviewDraft(format!(
            "cannot validate the recovered confirmed-review request: {error}"
        ))
    })?;
    if digest(&payload) != pending.payload_digest {
        return Err(ReplayError::InvalidReviewDraft(
            "the recovered confirmed-review request no longer matches its approved digest"
                .to_string(),
        ));
    }
    Ok(())
}

fn original_review_comment(
    session: &ReplaySession,
    draft: &ReplayReviewDraft,
    limits: ReplayLimits,
) -> Result<GitHubReviewComment, ReplayError> {
    let step_id = draft.step_id.as_deref().ok_or_else(|| {
        ReplayError::InvalidReviewDraft(
            "an inline review comment is missing its exact original source hunk".to_string(),
        )
    })?;
    let step = session
        .steps
        .iter()
        .find(|step| step.id == step_id)
        .ok_or_else(|| {
            ReplayError::InvalidReviewDraft(
                "an inline review comment names an unrelated original source hunk".to_string(),
            )
        })?;
    let anchor = session.original_review_anchor(step, limits)?;
    if draft.anchor.as_ref() != Some(&anchor)
        || draft.path.as_deref() != Some(anchor.path.as_path())
        || anchor.target_commit != session.source.target_commit
        || anchor.start_line == 0
        || anchor.end_line < anchor.start_line
    {
        return Err(ReplayError::InvalidReviewDraft(
            "an inline review comment no longer matches its exact original diff and line range"
                .to_string(),
        ));
    }
    validate_relative_path(&anchor.path)?;
    let path = github_relative_path(&anchor.path)?;
    let side = match anchor.side {
        ReplayDiffSide::Left => "LEFT",
        ReplayDiffSide::Right => "RIGHT",
    };
    let multiline = anchor.end_line > anchor.start_line;
    Ok(GitHubReviewComment {
        path,
        line: anchor.end_line,
        side,
        start_line: multiline.then_some(anchor.start_line),
        start_side: multiline.then_some(side),
        body: draft.text.clone(),
    })
}

fn github_relative_path(path: &Path) -> Result<String, ReplayError> {
    path.components()
        .map(|component| match component {
            Component::Normal(name) => name
                .to_str()
                .map(str::to_string)
                .ok_or_else(|| ReplayError::UnsafePath(path.display().to_string())),
            _ => Err(ReplayError::UnsafePath(path.display().to_string())),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|components| components.join("/"))
}

/// Submits exactly one confirmed, event-bearing GitHub review on a worker.
pub(crate) fn submit_prepared_review(
    mut submission: PreparedReplayReviewSubmission,
    limits: ReplayLimits,
) -> Result<(ReplayReviewSubmissionPreview, ReplayReviewReceipt), ReplayError> {
    refresh_pull_request_capabilities(&mut submission.source, limits)?;
    let request = submission.source.pull_request.as_ref().ok_or_else(|| {
        ReplayError::InvalidReviewDraft(
            "the original GitHub pull request disappeared before review submission".to_string(),
        )
    })?;
    validate_refreshed_review_identity(request, &submission.preview)?;

    let encoded = serde_json::to_vec(&submission.request).map_err(|error| {
        ReplayError::InvalidReviewDraft(format!(
            "cannot encode the approved GitHub review: {error}"
        ))
    })?;
    let endpoint = format!(
        "repos/{}/{}/pulls/{}/reviews",
        request.repository_owner, request.repository_name, request.number,
    );
    let mut command = Command::new("gh");
    command
        .current_dir(&submission.source.repository.root)
        .args(["api", "--hostname", &request.host, "--method", "POST"])
        .args(["--header", "Accept: application/vnd.github+json"])
        .args(["--input", "-", &endpoint])
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1");
    let output = run_command_with_input(&mut command, &encoded, limits.max_metadata_bytes)
        .map_err(|error| match error {
            ReplayCommandFailure::NotStarted(error) => error,
            ReplayCommandFailure::PossiblyExecuted(error) => {
                ReplayError::ReviewSubmissionUncertain(error.to_string())
            }
        })?;
    let receipt =
        validated_review_receipt(request, &submission.preview, &digest(&encoded), &output)
            .map_err(|error| ReplayError::ReviewSubmissionUncertain(error.to_string()))?;
    Ok((submission.preview, receipt))
}

/// Reconciles one exact approved review through bounded, read-only GitHub APIs.
pub(crate) fn reconcile_prepared_review(
    mut reconciliation: PreparedReplayReviewReconciliation,
    limits: ReplayLimits,
) -> Result<ReplayReviewReconciliation, ReplayError> {
    refresh_pull_request_capabilities(&mut reconciliation.source, limits)?;
    reconcile_verified_review_source(reconciliation, limits, &mut github_read_only)
}

fn reconcile_verified_review_source(
    reconciliation: PreparedReplayReviewReconciliation,
    limits: ReplayLimits,
    provider_read: &mut ReplayProviderRead<'_>,
) -> Result<ReplayReviewReconciliation, ReplayError> {
    let request = reconciliation.source.pull_request.as_ref().ok_or_else(|| {
        ReplayError::ReviewSubmissionUncertain(
            "the original GitHub pull request disappeared before review reconciliation".to_string(),
        )
    })?;
    validate_refreshed_review_identity(request, &reconciliation.preview)?;

    let payload = serde_json::to_vec(&reconciliation.request).map_err(|error| {
        ReplayError::ReviewSubmissionUncertain(format!(
            "cannot reconstruct the exact approved GitHub review request: {error}"
        ))
    })?;
    let actual_payload_digest = digest(&payload);
    if reconciliation.imported_receipt_id.is_none()
        && actual_payload_digest != reconciliation.payload_digest
    {
        return Err(ReplayError::ReviewSubmissionUncertain(
            "the recovered original review no longer matches its exact approved payload"
                .to_string(),
        ));
    }

    let reviews = list_submitted_reviews(
        &reconciliation.source,
        request,
        limits,
        reconciliation.imported_receipt_id,
        provider_read,
    )?;
    let mut matches = Vec::new();
    for review in reviews {
        if review.state != reconciliation.preview.outcome.github_state()
            || GitObjectId::parse(&review.commit_id).ok().as_ref()
                != Some(&reconciliation.preview.target_commit)
            || !review
                .user
                .login
                .eq_ignore_ascii_case(&reconciliation.preview.viewer)
            || review.body.as_deref().unwrap_or_default() != reconciliation.preview.body
            || review
                .submitted_at
                .as_deref()
                .is_none_or(|time| time.trim().is_empty())
        {
            continue;
        }

        let comments = submitted_review_comments(
            &reconciliation.source,
            request,
            review.id,
            reconciliation.request.comments.len(),
            limits,
            provider_read,
        )?;
        if !same_original_review_comments(
            &reconciliation.request.comments,
            &comments,
            &reconciliation.preview.target_commit,
        ) {
            continue;
        }

        let encoded_review = serde_json::to_vec(&review).map_err(|error| {
            ReplayError::ReviewSubmissionUncertain(format!(
                "cannot validate the original GitHub review response: {error}"
            ))
        })?;
        let receipt = validated_review_receipt(
            request,
            &reconciliation.preview,
            &actual_payload_digest,
            &encoded_review,
        )?;
        matches.push(receipt);
        if matches.len() > 1 {
            return Err(ReplayError::ReviewSubmissionUncertain(
                "more than one submitted GitHub review matches the approved original request"
                    .to_string(),
            ));
        }
    }

    Ok(match matches.pop() {
        Some(receipt) => ReplayReviewReconciliation::Verified {
            preview: Box::new(reconciliation.preview),
            receipt: Box::new(receipt),
        },
        None => ReplayReviewReconciliation::NotFound {
            imported_receipt_id: reconciliation.imported_receipt_id,
        },
    })
}

fn list_submitted_reviews(
    source: &ReplaySource,
    request: &ReplayPullRequest,
    limits: ReplayLimits,
    selected_id: Option<u64>,
    provider_read: &mut ReplayProviderRead<'_>,
) -> Result<Vec<GitHubSubmittedReview>, ReplayError> {
    let mut reviews = Vec::new();
    for page in 1..=MAX_RECONCILIATION_PAGES {
        let endpoint = format!(
            "repos/{}/{}/pulls/{}/reviews?per_page={RECONCILIATION_PAGE_SIZE}&page={page}",
            request.repository_owner, request.repository_name, request.number,
        );
        let output = provider_read(source, &request.host, &endpoint, limits)?;
        let batch: Vec<GitHubSubmittedReview> =
            serde_json::from_slice(&output).map_err(|error| {
                ReplayError::InvalidMetadata(format!(
                    "invalid original GitHub submitted-review list: {error}"
                ))
            })?;
        if batch.len() > RECONCILIATION_PAGE_SIZE {
            return Err(ReplayError::InvalidMetadata(
                "GitHub returned more submitted reviews than the bounded page allows".to_string(),
            ));
        }
        let batch_len = batch.len();
        reviews.extend(
            batch
                .into_iter()
                .filter(|review| selected_id.is_none_or(|selected| review.id == selected)),
        );
        if batch_len < RECONCILIATION_PAGE_SIZE {
            return Ok(reviews);
        }
    }
    Err(ReplayError::ReviewSubmissionUncertain(
        "the original pull request has too many submitted reviews to reconcile safely".to_string(),
    ))
}

fn submitted_review_comments(
    source: &ReplaySource,
    request: &ReplayPullRequest,
    review_id: u64,
    expected_count: usize,
    limits: ReplayLimits,
    provider_read: &mut ReplayProviderRead<'_>,
) -> Result<Vec<GitHubSubmittedReviewComment>, ReplayError> {
    let maximum_pages = expected_count
        .div_ceil(RECONCILIATION_PAGE_SIZE)
        .saturating_add(1)
        .min(MAX_RECONCILIATION_PAGES);
    let mut comments = Vec::new();
    for page in 1..=maximum_pages {
        let endpoint = format!(
            "repos/{}/{}/pulls/{}/reviews/{review_id}/comments?per_page={RECONCILIATION_PAGE_SIZE}&page={page}",
            request.repository_owner, request.repository_name, request.number,
        );
        let output = provider_read(source, &request.host, &endpoint, limits)?;
        let batch: Vec<GitHubSubmittedReviewComment> =
            serde_json::from_slice(&output).map_err(|error| {
                ReplayError::InvalidMetadata(format!(
                    "invalid original GitHub submitted-review comments: {error}"
                ))
            })?;
        if batch.len() > RECONCILIATION_PAGE_SIZE {
            return Err(ReplayError::InvalidMetadata(
                "GitHub returned more review comments than the bounded page allows".to_string(),
            ));
        }
        let batch_len = batch.len();
        comments.extend(batch);
        if comments.len() > expected_count {
            return Ok(comments);
        }
        if batch_len < RECONCILIATION_PAGE_SIZE {
            return Ok(comments);
        }
    }
    Err(ReplayError::ReviewSubmissionUncertain(
        "the submitted GitHub review has too many comments to reconcile safely".to_string(),
    ))
}

fn github_read_only(
    source: &ReplaySource,
    host: &str,
    endpoint: &str,
    limits: ReplayLimits,
) -> Result<Vec<u8>, ReplayError> {
    let mut command = Command::new("gh");
    command
        .current_dir(&source.repository.root)
        .args(["api", "--hostname", host])
        .args(["--header", "Accept: application/vnd.github+json"])
        .arg(endpoint)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1");
    run_command(&mut command, limits.max_metadata_bytes)
}

fn same_original_review_comments(
    expected: &[GitHubReviewComment],
    actual: &[GitHubSubmittedReviewComment],
    target_commit: &GitObjectId,
) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    let mut remaining = actual.iter().collect::<Vec<_>>();
    for comment in expected {
        let Some(index) = remaining.iter().position(|candidate| {
            candidate.path == comment.path
                && candidate.body == comment.body
                && candidate.original_line.or(candidate.line) == Some(comment.line)
                && candidate.side.as_deref() == Some(comment.side)
                && candidate.original_start_line.or(candidate.start_line) == comment.start_line
                && candidate.start_side.as_deref() == comment.start_side
                && candidate
                    .original_commit_id
                    .as_deref()
                    .or(candidate.commit_id.as_deref())
                    == Some(target_commit.as_str())
        }) else {
            return false;
        };
        remaining.swap_remove(index);
    }
    remaining.is_empty()
}

fn validate_refreshed_review_identity(
    request: &ReplayPullRequest,
    preview: &ReplayReviewSubmissionPreview,
) -> Result<(), ReplayError> {
    let repository = format!(
        "{}/{}/{}",
        request.host, request.repository_owner, request.repository_name
    );
    if !repository.eq_ignore_ascii_case(&preview.repository)
        || request.number != preview.pull_request
        || request.head_commit != preview.target_commit
        || request.url != preview.pull_request_url
    {
        return Err(ReplayError::SourceRefMoved);
    }
    let viewer = request.capabilities.viewer.as_deref().ok_or_else(|| {
        ReplayError::InvalidReviewDraft(
            "the authenticated GitHub reviewer could not be verified again".to_string(),
        )
    })?;
    if !viewer.eq_ignore_ascii_case(&preview.viewer) {
        return Err(ReplayError::InvalidReviewDraft(
            "the authenticated GitHub reviewer changed after the submission preview".to_string(),
        ));
    }
    if ReplayReviewRole::from_pull_request(Some(request)) == ReplayReviewRole::Author
        && preview.outcome != ReplayReviewOutcome::Comment
    {
        return Err(ReplayError::InvalidReviewDraft(
            "the original author cannot approve or request changes to their own PR".to_string(),
        ));
    }
    Ok(())
}

fn validated_review_receipt(
    request: &ReplayPullRequest,
    preview: &ReplayReviewSubmissionPreview,
    payload_digest: &str,
    output: &[u8],
) -> Result<ReplayReviewReceipt, ReplayError> {
    let response: GitHubSubmittedReview = serde_json::from_slice(output).map_err(|error| {
        ReplayError::InvalidMetadata(format!("invalid submitted GitHub review receipt: {error}"))
    })?;
    let submitted_at = response.submitted_at.filter(|time| !time.trim().is_empty());
    if response.id == 0
        || response.state != preview.outcome.github_state()
        || GitObjectId::parse(&response.commit_id)? != preview.target_commit
        || !response.user.login.eq_ignore_ascii_case(&preview.viewer)
        || response.body.as_deref().unwrap_or_default() != preview.body
        || submitted_at.is_none()
    {
        return Err(ReplayError::InvalidMetadata(
            "GitHub did not confirm the exact submitted, non-pending review".to_string(),
        ));
    }
    if !review_receipt_url_matches_original_pull_request(request, response.id, &response.html_url) {
        return Err(ReplayError::InvalidMetadata(
            "GitHub returned a review receipt for an unrelated pull request".to_string(),
        ));
    }
    Ok(ReplayReviewReceipt {
        id: response.id,
        url: response.html_url,
        outcome: preview.outcome,
        target_commit: preview.target_commit.clone(),
        viewer: response.user.login,
        draft_ids: preview
            .drafts
            .iter()
            .map(|draft| draft.id.clone())
            .collect(),
        payload_digest: payload_digest.to_string(),
        submitted_at: submitted_at.unwrap_or_default(),
        verification: ReplayReceiptVerification::Verified,
    })
}

pub(super) fn review_receipt_matches_original_pull_request(
    request: &ReplayPullRequest,
    receipt: &ReplayReviewReceipt,
) -> bool {
    review_receipt_url_matches_original_pull_request(request, receipt.id, &receipt.url)
}

fn review_receipt_url_matches_original_pull_request(
    request: &ReplayPullRequest,
    id: u64,
    url: &str,
) -> bool {
    let Ok(url) = Url::parse(url) else {
        return false;
    };
    let expected_path = format!(
        "/{}/{}/pull/{}",
        request.repository_owner, request.repository_name, request.number,
    );
    let expected_fragment = format!("pullrequestreview-{id}");
    url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case(&request.host))
        && url.path().eq_ignore_ascii_case(&expected_path)
        && url.fragment() == Some(expected_fragment.as_str())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::replay::{
        ReplayGitHubCapabilities, ReplayRepository, ReplayRepositoryPermission, ReplayWorkspace,
    };

    const PATCH: &str = concat!(
        "diff --git a/src/token.rs b/src/token.rs\n",
        "index 1111111..2222222 100644\n",
        "--- a/src/token.rs\n",
        "+++ b/src/token.rs\n",
        "@@ -1,3 +1,3 @@ fn refresh\n",
        " fn refresh() {\n",
        "-    old();\n",
        "+    new();\n",
        " }\n",
    );

    fn controller_with_pull_request(
        patch: &str,
        author: &str,
        viewer: Option<&str>,
    ) -> (ReplayController, String, String) {
        let base_commit = GitObjectId::parse(&"a".repeat(40)).unwrap();
        let target_commit = GitObjectId::parse(&"b".repeat(40)).unwrap();
        let repository = ReplayRepository {
            root: PathBuf::from("review-fixture"),
            common_directory: PathBuf::from("review-fixture/.git"),
            host: "github.com".to_string(),
            owner: "example".to_string(),
            name: "replay".to_string(),
        };
        let request = ReplayPullRequest {
            host: repository.host.clone(),
            repository_owner: repository.owner.clone(),
            repository_name: repository.name.clone(),
            number: 482,
            url: "https://github.com/example/replay/pull/482".to_string(),
            author: Some(author.to_string()),
            base_ref: "master".to_string(),
            base_ref_tip: base_commit.clone(),
            head_repository_owner: repository.owner.clone(),
            head_repository_name: repository.name.clone(),
            head_ref: "feature/replay".to_string(),
            head_commit: target_commit.clone(),
            cross_repository: false,
            capabilities: ReplayGitHubCapabilities {
                viewer: viewer.map(str::to_string),
                head_permission: ReplayRepositoryPermission::Write,
                warning: None,
            },
            captured_at_ms: 1,
        };
        let source = ReplaySource {
            id: "original-pr-482".to_string(),
            repository,
            kind: ReplaySourceKind::GitHubPullRequest,
            base_commit: base_commit.clone(),
            target_commit,
            patch: patch.to_string(),
            patch_digest: digest(patch.as_bytes()),
            pull_request: Some(request),
            review_context: None,
        };
        let workspace = ReplayWorkspace {
            root: PathBuf::from("review-fixture.replay-pr-482"),
            branch: "replay/pr-482-bbbbbbb".to_string(),
            base_commit,
            created_by_replay: true,
        };
        let session = ReplaySession::from_source(source, workspace, ReplayLimits::default())
            .expect("compile the exact immutable original pull-request fixture");
        let session_id = session.id.clone();
        let step_id = session.steps[0].id.clone();
        let mut controller = ReplayController::default();
        controller.adopt_session(session);
        (controller, session_id, step_id)
    }

    fn add_inline(controller: &mut ReplayController, session: &str, step: &str) {
        controller
            .add_review_draft(
                session,
                Some(step),
                ReplayReviewDraftKind::InlineComment,
                "Please cover the exact refresh boundary.",
            )
            .expect("anchor a local comment to the exact original PR diff");
    }

    fn add_summary(controller: &mut ReplayController, session: &str, text: &str) {
        controller
            .add_review_draft(
                session,
                /*step_id*/ None,
                ReplayReviewDraftKind::ReviewSummary,
                text,
            )
            .expect("retain an explicitly written pull-request-level summary");
    }

    fn review_receipt_json(preview: &ReplayReviewSubmissionPreview) -> serde_json::Value {
        json!({
            "id": 71,
            "html_url": "https://github.com/example/replay/pull/482#pullrequestreview-71",
            "state": preview.outcome.github_state(),
            "commit_id": preview.target_commit.as_str(),
            "body": preview.body,
            "user": { "login": preview.viewer },
            "submitted_at": "2026-07-27T20:00:00Z",
        })
    }

    fn provider_review_comments(
        request: &GitHubReviewRequest,
        target_commit: &GitObjectId,
    ) -> serde_json::Value {
        json!(request
            .comments
            .iter()
            .map(|comment| {
                json!({
                    "path": comment.path,
                    "body": comment.body,
                    "original_line": comment.line,
                    "original_start_line": comment.start_line,
                    "side": comment.side,
                    "start_side": comment.start_side,
                    "original_commit_id": target_commit.as_str(),
                })
            })
            .collect::<Vec<_>>())
    }

    #[test]
    fn confirmed_review_becomes_recoverable_before_a_provider_request_can_start() {
        let (mut controller, session_id, step_id) =
            controller_with_pull_request(PATCH, "original-author", Some("reviewer"));
        add_inline(&mut controller, &session_id, &step_id);
        let preview = controller
            .preview_review_submission(&session_id, ReplayReviewOutcome::Comment)
            .unwrap();

        controller
            .begin_review_submission(
                &session_id,
                ReplayReviewOutcome::Comment,
                &preview.preview_digest,
                /*confirmed*/ true,
            )
            .unwrap();
        let snapshot = controller.recovery_snapshot().unwrap();
        assert_eq!(
            snapshot.sessions[0]
                .review
                .pending_submission
                .as_ref()
                .unwrap()
                .state,
            ReplayReviewSubmissionState::InFlight,
        );

        let mut recovered = ReplayController::default();
        recovered.restore(&snapshot).unwrap();
        let pending = recovered
            .session(&session_id)
            .unwrap()
            .review
            .pending_submission
            .as_ref()
            .unwrap();
        assert_eq!(pending.state, ReplayReviewSubmissionState::Uncertain);
        assert!(matches!(
            recovered.preview_review_submission(&session_id, ReplayReviewOutcome::Comment),
            Err(ReplayError::ReviewSubmissionUncertain(_)),
        ));
        let draft_id = recovered.session(&session_id).unwrap().review.drafts[0]
            .id
            .clone();
        assert!(matches!(
            recovered.update_review_draft(&session_id, &draft_id, "Changed while pending"),
            Err(ReplayError::ReviewSubmissionUncertain(_)),
        ));
        assert!(recovered.prepare_review_reconciliation(&session_id).is_ok());
    }

    #[test]
    fn exact_provider_review_reconciliation_adopts_one_verified_receipt_without_reposting() {
        let (mut controller, session_id, step_id) =
            controller_with_pull_request(PATCH, "original-author", Some("reviewer"));
        add_inline(&mut controller, &session_id, &step_id);
        add_summary(
            &mut controller,
            &session_id,
            "Verify the original provider response.",
        );
        let preview = controller
            .preview_review_submission(&session_id, ReplayReviewOutcome::Comment)
            .unwrap();
        controller
            .begin_review_submission(
                &session_id,
                ReplayReviewOutcome::Comment,
                &preview.preview_digest,
                /*confirmed*/ true,
            )
            .unwrap();
        controller
            .mark_review_submission_uncertain(&session_id)
            .unwrap();
        let prepared = controller
            .prepare_review_reconciliation(&session_id)
            .unwrap();
        let comments = provider_review_comments(&prepared.request, &preview.target_commit);
        let review = review_receipt_json(&preview);
        let mut endpoints = Vec::new();
        let result = reconcile_verified_review_source(
            prepared,
            ReplayLimits::default(),
            &mut |_source, host, endpoint, _limits| {
                assert_eq!(host, "github.com");
                endpoints.push(endpoint.to_string());
                let response = if endpoint.contains("/reviews/71/comments?") {
                    comments.clone()
                } else {
                    json!([review.clone()])
                };
                Ok(serde_json::to_vec(&response).unwrap())
            },
        )
        .expect("verify the exact original review using read-only provider responses");

        let ReplayReviewReconciliation::Verified { preview, receipt } = result else {
            panic!("the exact original provider review must be verified");
        };
        assert_eq!(receipt.verification, ReplayReceiptVerification::Verified);
        let receipt = controller
            .record_review_submission(&session_id, &preview, *receipt)
            .unwrap();
        let session = controller.session(&session_id).unwrap();
        assert!(session.review.pending_submission.is_none());
        assert_eq!(session.review.receipts, vec![receipt.clone()]);
        assert!(session
            .review
            .drafts
            .iter()
            .all(|draft| draft.state == ReplayDraftState::Submitted));
        assert_eq!(
            controller
                .record_review_submission(&session_id, &preview, receipt.clone())
                .unwrap(),
            receipt,
        );
        assert_eq!(endpoints.len(), 2);
        assert!(endpoints
            .iter()
            .all(|endpoint| endpoint.starts_with("repos/example/replay/pulls/482/reviews")));
    }

    #[test]
    fn an_unrelated_provider_comment_never_resolves_the_confirmed_review() {
        let (mut controller, session_id, step_id) =
            controller_with_pull_request(PATCH, "original-author", Some("reviewer"));
        add_inline(&mut controller, &session_id, &step_id);
        let preview = controller
            .preview_review_submission(&session_id, ReplayReviewOutcome::Comment)
            .unwrap();
        controller
            .begin_review_submission(
                &session_id,
                ReplayReviewOutcome::Comment,
                &preview.preview_digest,
                /*confirmed*/ true,
            )
            .unwrap();
        let prepared = controller
            .prepare_review_reconciliation(&session_id)
            .unwrap();
        let mut comments = provider_review_comments(&prepared.request, &preview.target_commit);
        comments[0]["body"] = json!("An unrelated original-source review comment.");
        let review = review_receipt_json(&preview);

        let result = reconcile_verified_review_source(
            prepared,
            ReplayLimits::default(),
            &mut |_source, _host, endpoint, _limits| {
                let response = if endpoint.contains("/reviews/71/comments?") {
                    comments.clone()
                } else {
                    json!([review.clone()])
                };
                Ok(serde_json::to_vec(&response).unwrap())
            },
        )
        .unwrap();

        assert!(matches!(
            result,
            ReplayReviewReconciliation::NotFound {
                imported_receipt_id: None
            }
        ));
        assert!(controller
            .session(&session_id)
            .unwrap()
            .review
            .pending_submission
            .is_some());
        controller.clear_review_submission(&session_id).unwrap();
        assert!(controller
            .preview_review_submission(&session_id, ReplayReviewOutcome::Comment)
            .is_ok());
    }

    #[test]
    fn duplicate_matching_provider_reviews_remain_uncertain_instead_of_being_adopted() {
        let (mut controller, session_id, step_id) =
            controller_with_pull_request(PATCH, "original-author", Some("reviewer"));
        add_inline(&mut controller, &session_id, &step_id);
        let preview = controller
            .preview_review_submission(&session_id, ReplayReviewOutcome::Comment)
            .unwrap();
        controller
            .begin_review_submission(
                &session_id,
                ReplayReviewOutcome::Comment,
                &preview.preview_digest,
                /*confirmed*/ true,
            )
            .unwrap();
        let prepared = controller
            .prepare_review_reconciliation(&session_id)
            .unwrap();
        let comments = provider_review_comments(&prepared.request, &preview.target_commit);
        let first = review_receipt_json(&preview);
        let mut second = first.clone();
        second["id"] = json!(72);
        second["html_url"] =
            json!("https://github.com/example/replay/pull/482#pullrequestreview-72");

        let result = reconcile_verified_review_source(
            prepared,
            ReplayLimits::default(),
            &mut |_source, _host, endpoint, _limits| {
                let response = if endpoint.contains("/comments?") {
                    comments.clone()
                } else {
                    json!([first.clone(), second.clone()])
                };
                Ok(serde_json::to_vec(&response).unwrap())
            },
        );

        assert!(matches!(
            result,
            Err(ReplayError::ReviewSubmissionUncertain(message))
                if message.contains("more than one")
        ));
    }

    #[test]
    fn imported_review_receipt_becomes_trusted_only_after_exact_provider_reconciliation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("portable-verified-review.json");
        let (mut original, original_id, original_step) =
            controller_with_pull_request(PATCH, "original-author", Some("reviewer"));
        add_inline(&mut original, &original_id, &original_step);
        let preview = original
            .preview_review_submission(&original_id, ReplayReviewOutcome::Comment)
            .unwrap();
        let prepared = original
            .prepare_review_submission(
                &original_id,
                ReplayReviewOutcome::Comment,
                &preview.preview_digest,
                /*confirmed*/ true,
            )
            .unwrap();
        let encoded = serde_json::to_vec(&prepared.request).unwrap();
        let original_request = original
            .session(&original_id)
            .unwrap()
            .source
            .pull_request
            .as_ref()
            .unwrap();
        let receipt = validated_review_receipt(
            original_request,
            &preview,
            &digest(&encoded),
            &serde_json::to_vec(&review_receipt_json(&preview)).unwrap(),
        )
        .unwrap();
        original
            .record_review_submission(&original_id, &preview, receipt)
            .unwrap();
        original
            .save_review_bundle(&original_id, &path, /*overwrite*/ false)
            .unwrap();

        let (mut destination, destination_id, _) =
            controller_with_pull_request(PATCH, "original-author", Some("reviewer"));
        let import = destination
            .preview_review_bundle(&destination_id, &path)
            .unwrap();
        destination
            .import_review_bundle(
                &destination_id,
                &path,
                &import.bundle_digest,
                /*confirmed*/ true,
            )
            .unwrap();
        assert_eq!(
            destination
                .session(&destination_id)
                .unwrap()
                .review
                .receipts[0]
                .verification,
            ReplayReceiptVerification::Unverified,
        );
        assert_eq!(
            destination.session(&destination_id).unwrap().review.drafts[0].state,
            ReplayDraftState::Local,
        );

        let prepared = destination
            .prepare_review_reconciliation(&destination_id)
            .unwrap();
        let comments = provider_review_comments(&prepared.request, &preview.target_commit);
        let review = review_receipt_json(&prepared.preview);
        let result = reconcile_verified_review_source(
            prepared,
            ReplayLimits::default(),
            &mut |_source, _host, endpoint, _limits| {
                let response = if endpoint.contains("/reviews/71/comments?") {
                    comments.clone()
                } else {
                    json!([review.clone()])
                };
                Ok(serde_json::to_vec(&response).unwrap())
            },
        )
        .unwrap();
        let ReplayReviewReconciliation::Verified { preview, receipt } = result else {
            panic!("an imported provider receipt must be verified from GitHub");
        };
        destination
            .record_review_submission(&destination_id, &preview, *receipt)
            .unwrap();

        let recovered = destination.session(&destination_id).unwrap();
        assert_eq!(
            recovered.review.receipts[0].verification,
            ReplayReceiptVerification::Verified,
        );
        assert_eq!(
            recovered.review.drafts[0].state,
            ReplayDraftState::Submitted
        );
    }

    #[test]
    fn recovery_refuses_a_tampered_confirmed_review_request_digest() {
        let (mut original, session_id, step_id) =
            controller_with_pull_request(PATCH, "original-author", Some("reviewer"));
        add_inline(&mut original, &session_id, &step_id);
        let preview = original
            .preview_review_submission(&session_id, ReplayReviewOutcome::Comment)
            .unwrap();
        original
            .begin_review_submission(
                &session_id,
                ReplayReviewOutcome::Comment,
                &preview.preview_digest,
                /*confirmed*/ true,
            )
            .unwrap();
        let mut snapshot = original.recovery_snapshot().unwrap();
        snapshot.sessions[0]
            .review
            .pending_submission
            .as_mut()
            .unwrap()
            .payload_digest = "f".repeat(64);

        let mut recovered = ReplayController::default();
        assert!(matches!(
            recovered.restore(&snapshot),
            Err(ReplayError::InvalidReviewDraft(message))
                if message.contains("approved digest")
        ));
    }

    #[test]
    fn atomic_comment_payload_pins_the_head_and_never_creates_a_pending_review() {
        let (mut controller, session, step) =
            controller_with_pull_request(PATCH, "original-author", Some("reviewer"));
        add_inline(&mut controller, &session, &step);
        add_summary(
            &mut controller,
            &session,
            "Please add a refresh regression test.",
        );

        let preview = controller
            .preview_review_submission(&session, ReplayReviewOutcome::Comment)
            .unwrap();
        let prepared = controller
            .prepare_review_submission(
                &session,
                ReplayReviewOutcome::Comment,
                &preview.preview_digest,
                /*confirmed*/ true,
            )
            .unwrap();
        let payload = serde_json::to_value(&prepared.request).unwrap();

        assert_eq!(preview.repository, "github.com/example/replay");
        assert_eq!(preview.pull_request, 482);
        assert_eq!(preview.viewer, "reviewer");
        assert_eq!(preview.inline_comment_count, 1);
        assert_eq!(preview.summary_count, 1);
        assert_eq!(
            payload,
            json!({
                "commit_id": "b".repeat(40),
                "body": "Please add a refresh regression test.",
                "event": "COMMENT",
                "comments": [{
                    "path": "src/token.rs",
                    "line": 2,
                    "side": "RIGHT",
                    "body": "Please cover the exact refresh boundary.",
                }],
            }),
        );
    }

    #[test]
    fn inline_only_comment_previews_the_exact_required_github_review_body() {
        let (mut controller, session, step) =
            controller_with_pull_request(PATCH, "author", Some("reviewer"));
        add_inline(&mut controller, &session, &step);

        let preview = controller
            .preview_review_submission(&session, ReplayReviewOutcome::Comment)
            .unwrap();

        assert_eq!(preview.summary_count, 0);
        assert_eq!(preview.body, "Inline review comments.");
    }

    #[test]
    fn multiline_comments_preserve_original_side_and_both_changed_lines() {
        let patch = concat!(
            "diff --git a/src/token.rs b/src/token.rs\n",
            "--- a/src/token.rs\n",
            "+++ b/src/token.rs\n",
            "@@ -1,3 +1,4 @@ fn refresh\n",
            " fn refresh() {\n",
            "-    old();\n",
            "+    new();\n",
            "+    bounded();\n",
            " }\n",
        );
        let (mut controller, session, step) =
            controller_with_pull_request(patch, "author", Some("reviewer"));
        add_inline(&mut controller, &session, &step);
        let preview = controller
            .preview_review_submission(&session, ReplayReviewOutcome::Comment)
            .unwrap();
        let prepared = controller
            .prepare_review_submission(
                &session,
                ReplayReviewOutcome::Comment,
                &preview.preview_digest,
                /*confirmed*/ true,
            )
            .unwrap();
        let payload = serde_json::to_value(&prepared.request).unwrap();

        assert_eq!(payload["comments"][0]["path"], "src/token.rs");
        assert_eq!(payload["comments"][0]["side"], "RIGHT");
        assert_eq!(payload["comments"][0]["start_side"], "RIGHT");
        assert_eq!(payload["comments"][0]["start_line"], 2);
        assert_eq!(payload["comments"][0]["line"], 3);
    }

    #[test]
    fn deleted_original_lines_retain_the_github_left_side() {
        let patch = concat!(
            "diff --git a/src/token.rs b/src/token.rs\n",
            "--- a/src/token.rs\n",
            "+++ b/src/token.rs\n",
            "@@ -7,3 +7,2 @@ fn refresh\n",
            " before\n",
            "-removed\n",
            " after\n",
        );
        let (mut controller, session, step) =
            controller_with_pull_request(patch, "author", Some("reviewer"));
        add_inline(&mut controller, &session, &step);
        let preview = controller
            .preview_review_submission(&session, ReplayReviewOutcome::Comment)
            .unwrap();
        let prepared = controller
            .prepare_review_submission(
                &session,
                ReplayReviewOutcome::Comment,
                &preview.preview_digest,
                /*confirmed*/ true,
            )
            .unwrap();
        let payload = serde_json::to_value(&prepared.request).unwrap();

        assert_eq!(payload["comments"][0]["side"], "LEFT");
        assert_eq!(payload["comments"][0]["line"], 8);
        assert!(payload["comments"][0].get("start_line").is_none());
    }

    #[test]
    fn separate_summaries_are_combined_into_the_exact_previewed_review_body() {
        let (mut controller, session, _) =
            controller_with_pull_request(PATCH, "author", Some("reviewer"));
        add_summary(&mut controller, &session, "First original review finding.");
        add_summary(&mut controller, &session, "Second original review finding.");

        let preview = controller
            .preview_review_submission(&session, ReplayReviewOutcome::Comment)
            .unwrap();

        assert_eq!(preview.inline_comment_count, 0);
        assert_eq!(preview.summary_count, 2);
        assert_eq!(
            preview.body,
            "First original review finding.\n\nSecond original review finding.",
        );
    }

    #[test]
    fn requesting_changes_requires_an_explicit_pr_level_explanation() {
        let (mut controller, session, step) =
            controller_with_pull_request(PATCH, "author", Some("reviewer"));
        add_inline(&mut controller, &session, &step);

        assert!(matches!(
            controller.preview_review_submission(&session, ReplayReviewOutcome::RequestChanges),
            Err(ReplayError::InvalidReviewDraft(message))
                if message.contains("PR-level summary"),
        ));
        add_summary(
            &mut controller,
            &session,
            "The refresh needs a regression test.",
        );
        let preview = controller
            .preview_review_submission(&session, ReplayReviewOutcome::RequestChanges)
            .unwrap();
        assert_eq!(preview.outcome.github_event(), "REQUEST_CHANGES");
    }

    #[test]
    fn approval_can_publish_inline_feedback_without_inventing_a_summary() {
        let (mut controller, session, step) =
            controller_with_pull_request(PATCH, "author", Some("reviewer"));
        add_inline(&mut controller, &session, &step);
        let preview = controller
            .preview_review_submission(&session, ReplayReviewOutcome::Approve)
            .unwrap();
        let prepared = controller
            .prepare_review_submission(
                &session,
                ReplayReviewOutcome::Approve,
                &preview.preview_digest,
                /*confirmed*/ true,
            )
            .unwrap();
        let payload = serde_json::to_value(&prepared.request).unwrap();

        assert_eq!(payload["event"], "APPROVE");
        assert!(payload.get("body").is_none());
    }

    #[test]
    fn original_authors_cannot_approve_or_request_changes_to_their_own_pr() {
        let (mut controller, session, step) =
            controller_with_pull_request(PATCH, "original-author", Some("Original-Author"));
        add_inline(&mut controller, &session, &step);
        add_summary(&mut controller, &session, "My own PR summary.");

        for outcome in [
            ReplayReviewOutcome::Approve,
            ReplayReviewOutcome::RequestChanges,
        ] {
            assert!(matches!(
                controller.preview_review_submission(&session, outcome),
                Err(ReplayError::InvalidReviewDraft(message))
                    if message.contains("own pull request"),
            ));
        }
        assert!(controller
            .preview_review_submission(&session, ReplayReviewOutcome::Comment)
            .is_ok());
    }

    #[test]
    fn original_pr_fix_proposals_stay_local_and_outside_review_payloads() {
        let (mut controller, session, step) =
            controller_with_pull_request(PATCH, "original-author", Some("original-author"));
        add_inline(&mut controller, &session, &step);
        controller
            .add_review_draft(
                &session,
                Some(&step),
                ReplayReviewDraftKind::CodeFix,
                "Use the bounded original-branch refresh implementation.",
            )
            .unwrap();

        let preview = controller
            .preview_review_submission(&session, ReplayReviewOutcome::Comment)
            .unwrap();

        assert_eq!(preview.local_fix_count, 1);
        assert_eq!(preview.drafts.len(), 1);
        assert!(preview
            .drafts
            .iter()
            .all(|draft| draft.kind != ReplayReviewDraftKind::CodeFix));
    }

    #[test]
    fn agent_proposed_comments_remain_visible_and_require_human_confirmation() {
        let (mut controller, session, step) =
            controller_with_pull_request(PATCH, "author", Some("reviewer"));
        add_inline(&mut controller, &session, &step);
        controller.session_mut(&session).unwrap().review.drafts[0].origin =
            ReplayDraftOrigin::Agent;

        let preview = controller
            .preview_review_submission(&session, ReplayReviewOutcome::Comment)
            .unwrap();

        assert_eq!(preview.agent_draft_count, 1);
        assert_eq!(preview.drafts[0].origin, ReplayDraftOrigin::Agent);
        assert!(matches!(
            controller.prepare_review_submission(
                &session,
                ReplayReviewOutcome::Comment,
                &preview.preview_digest,
                /*confirmed*/ false,
            ),
            Err(ReplayError::ReviewSubmissionConfirmationRequired),
        ));
    }

    #[test]
    fn submission_is_impossible_without_an_authenticated_github_viewer() {
        let (mut controller, session, step) =
            controller_with_pull_request(PATCH, "author", /*viewer*/ None);
        add_inline(&mut controller, &session, &step);

        assert!(matches!(
            controller.preview_review_submission(&session, ReplayReviewOutcome::Comment),
            Err(ReplayError::InvalidReviewDraft(message))
                if message.contains("authenticated GitHub identity"),
        ));
    }

    #[test]
    fn local_branch_reviews_cannot_become_remote_github_submissions() {
        let (mut controller, session, step) =
            controller_with_pull_request(PATCH, "author", Some("reviewer"));
        add_inline(&mut controller, &session, &step);
        controller.session_mut(&session).unwrap().source.kind = ReplaySourceKind::LocalRevision;

        assert!(matches!(
            controller.preview_review_submission(&session, ReplayReviewOutcome::Comment),
            Err(ReplayError::InvalidReviewDraft(message))
                if message.contains("only an original GitHub pull request"),
        ));
    }

    #[test]
    fn any_edit_invalidates_the_exact_human_confirmed_submission_preview() {
        let (mut controller, session, step) =
            controller_with_pull_request(PATCH, "author", Some("reviewer"));
        add_inline(&mut controller, &session, &step);
        let preview = controller
            .preview_review_submission(&session, ReplayReviewOutcome::Comment)
            .unwrap();
        let draft_id = preview.drafts[0].id.clone();
        controller
            .update_review_draft(&session, &draft_id, "This text changed after the preview.")
            .unwrap();

        assert!(matches!(
            controller.prepare_review_submission(
                &session,
                ReplayReviewOutcome::Comment,
                &preview.preview_digest,
                /*confirmed*/ true,
            ),
            Err(ReplayError::StalePreview),
        ));
    }

    #[test]
    fn confirmation_cannot_switch_the_previewed_outcome() {
        let (mut controller, session, step) =
            controller_with_pull_request(PATCH, "author", Some("reviewer"));
        add_inline(&mut controller, &session, &step);
        let preview = controller
            .preview_review_submission(&session, ReplayReviewOutcome::Comment)
            .unwrap();

        assert!(matches!(
            controller.prepare_review_submission(
                &session,
                ReplayReviewOutcome::Approve,
                &preview.preview_digest,
                /*confirmed*/ true,
            ),
            Err(ReplayError::StalePreview),
        ));
    }

    #[test]
    fn submitted_reviews_keep_a_receipt_and_cannot_be_edited_or_reposted() {
        let (mut controller, session, step) =
            controller_with_pull_request(PATCH, "author", Some("reviewer"));
        add_inline(&mut controller, &session, &step);
        let preview = controller
            .preview_review_submission(&session, ReplayReviewOutcome::Comment)
            .unwrap();
        let request = controller
            .session(&session)
            .unwrap()
            .source
            .pull_request
            .as_ref()
            .unwrap();
        let response = serde_json::to_vec(&review_receipt_json(&preview)).unwrap();
        let receipt = validated_review_receipt(
            request,
            &preview,
            &digest(b"original submitted request"),
            &response,
        )
        .unwrap();
        let draft_id = preview.drafts[0].id.clone();

        controller
            .record_review_submission(&session, &preview, receipt.clone())
            .expect("record only a verified, non-pending original PR review");
        let reviewed = controller.session(&session).unwrap();
        assert_eq!(reviewed.review.drafts[0].state, ReplayDraftState::Submitted);
        assert_eq!(reviewed.review.receipts, vec![receipt]);
        assert!(matches!(
            controller.preview_review_submission(&session, ReplayReviewOutcome::Comment),
            Err(ReplayError::InvalidReviewDraft(_)),
        ));
        assert!(matches!(
            controller.update_review_draft(&session, &draft_id, "Rewrite published history"),
            Err(ReplayError::InvalidReviewDraft(_)),
        ));
        assert!(matches!(
            controller.remove_review_draft(&session, &draft_id),
            Err(ReplayError::InvalidReviewDraft(_)),
        ));
    }

    #[test]
    fn a_verified_receipt_survives_unrelated_drafts_added_while_the_worker_runs() {
        let (mut controller, session, step) =
            controller_with_pull_request(PATCH, "author", Some("reviewer"));
        add_inline(&mut controller, &session, &step);
        let preview = controller
            .preview_review_submission(&session, ReplayReviewOutcome::Comment)
            .unwrap();
        let response = serde_json::to_vec(&review_receipt_json(&preview)).unwrap();
        let receipt = validated_review_receipt(
            controller
                .session(&session)
                .unwrap()
                .source
                .pull_request
                .as_ref()
                .unwrap(),
            &preview,
            &digest(b"approved original review"),
            &response,
        )
        .unwrap();
        add_summary(
            &mut controller,
            &session,
            "An unrelated draft composed while the original request was in flight.",
        );

        controller
            .record_review_submission(&session, &preview, receipt)
            .expect("never discard a verified remote receipt because unrelated drafts were added");
        let review = &controller.session(&session).unwrap().review;
        assert_eq!(review.receipts.len(), 1);
        assert_eq!(review.drafts[0].state, ReplayDraftState::Submitted);
        assert_eq!(review.drafts[1].state, ReplayDraftState::Local);
    }

    #[test]
    fn changing_an_already_accepted_draft_is_reported_as_uncertain_not_unposted() {
        let (mut controller, session, step) =
            controller_with_pull_request(PATCH, "author", Some("reviewer"));
        add_inline(&mut controller, &session, &step);
        let preview = controller
            .preview_review_submission(&session, ReplayReviewOutcome::Comment)
            .unwrap();
        let response = serde_json::to_vec(&review_receipt_json(&preview)).unwrap();
        let receipt = validated_review_receipt(
            controller
                .session(&session)
                .unwrap()
                .source
                .pull_request
                .as_ref()
                .unwrap(),
            &preview,
            &digest(b"approved original review"),
            &response,
        )
        .unwrap();
        controller
            .update_review_draft(
                &session,
                &preview.drafts[0].id,
                "Changed after GitHub accepted the original review.",
            )
            .unwrap();

        assert!(matches!(
            controller.record_review_submission(&session, &preview, receipt),
            Err(ReplayError::ReviewSubmissionUncertain(message))
                if message.contains("changed after GitHub accepted"),
        ));
        assert_eq!(
            controller.session(&session).unwrap().review.drafts[0].state,
            ReplayDraftState::Local,
        );
    }

    #[test]
    fn github_pending_or_wrong_user_receipts_never_mark_a_draft_published() {
        let (mut controller, session, step) =
            controller_with_pull_request(PATCH, "author", Some("reviewer"));
        add_inline(&mut controller, &session, &step);
        let preview = controller
            .preview_review_submission(&session, ReplayReviewOutcome::Comment)
            .unwrap();
        let request = controller
            .session(&session)
            .unwrap()
            .source
            .pull_request
            .as_ref()
            .unwrap();
        let mut pending = review_receipt_json(&preview);
        pending["state"] = json!("PENDING");
        pending["submitted_at"] = serde_json::Value::Null;
        let output = serde_json::to_vec(&pending).unwrap();
        assert!(matches!(
            validated_review_receipt(request, &preview, &digest(b"payload"), &output),
            Err(ReplayError::InvalidMetadata(_)),
        ));

        let mut wrong_user = review_receipt_json(&preview);
        wrong_user["user"]["login"] = json!("another-account");
        let output = serde_json::to_vec(&wrong_user).unwrap();
        assert!(matches!(
            validated_review_receipt(request, &preview, &digest(b"payload"), &output),
            Err(ReplayError::InvalidMetadata(_)),
        ));
        assert_eq!(
            controller.session(&session).unwrap().review.drafts[0].state,
            ReplayDraftState::Local,
        );
    }

    #[test]
    fn receipt_for_an_unrelated_github_pull_request_is_rejected() {
        let (mut controller, session, step) =
            controller_with_pull_request(PATCH, "author", Some("reviewer"));
        add_inline(&mut controller, &session, &step);
        let preview = controller
            .preview_review_submission(&session, ReplayReviewOutcome::Comment)
            .unwrap();
        let request = controller
            .session(&session)
            .unwrap()
            .source
            .pull_request
            .as_ref()
            .unwrap();
        let mut receipt = review_receipt_json(&preview);
        receipt["html_url"] =
            json!("https://github.com/example/unrelated/pull/482#pullrequestreview-71");

        assert!(matches!(
            validated_review_receipt(
                request,
                &preview,
                &digest(b"payload"),
                &serde_json::to_vec(&receipt).unwrap(),
            ),
            Err(ReplayError::InvalidMetadata(_)),
        ));
    }

    #[test]
    fn refreshed_head_or_viewer_changes_abort_before_any_github_post() {
        let (mut controller, session, step) =
            controller_with_pull_request(PATCH, "author", Some("reviewer"));
        add_inline(&mut controller, &session, &step);
        let preview = controller
            .preview_review_submission(&session, ReplayReviewOutcome::Comment)
            .unwrap();
        let original = controller
            .session(&session)
            .unwrap()
            .source
            .pull_request
            .as_ref()
            .unwrap();
        let mut moved = original.clone();
        moved.head_commit = GitObjectId::parse(&"c".repeat(40)).unwrap();
        assert!(matches!(
            validate_refreshed_review_identity(&moved, &preview),
            Err(ReplayError::SourceRefMoved),
        ));

        let mut wrong_user = original.clone();
        wrong_user.capabilities.viewer = Some("another-reviewer".to_string());
        assert!(matches!(
            validate_refreshed_review_identity(&wrong_user, &preview),
            Err(ReplayError::InvalidReviewDraft(_)),
        ));
    }

    #[test]
    fn github_paths_are_normalized_without_platform_dependent_separators() {
        let relative = Path::new("src").join("nested").join("token.rs");
        assert_eq!(
            github_relative_path(&relative).unwrap(),
            "src/nested/token.rs"
        );
        assert!(github_relative_path(Path::new("../outside.rs")).is_err());
    }

    #[test]
    fn empty_outboxes_cannot_submit_approval_comment_or_requested_changes() {
        let (controller, session, _) =
            controller_with_pull_request(PATCH, "author", Some("reviewer"));
        for outcome in [
            ReplayReviewOutcome::Comment,
            ReplayReviewOutcome::Approve,
            ReplayReviewOutcome::RequestChanges,
        ] {
            assert!(matches!(
                controller.preview_review_submission(&session, outcome),
                Err(ReplayError::InvalidReviewDraft(_)),
            ));
        }
    }
}
