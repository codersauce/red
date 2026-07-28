//! Explicitly approved, atomic GitHub reviews of pinned original PR sources.

use std::{
    collections::HashSet,
    path::{Component, Path},
    process::Command,
};

use serde::{Deserialize, Serialize};
use url::Url;

use super::{
    digest, refresh_pull_request_capabilities,
    source::{run_command_with_input, validate_relative_path, ReplayCommandFailure},
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

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
struct GitHubReviewUser {
    login: String,
}

impl ReplayController {
    /// Previews all eligible local outcomes without writing or contacting GitHub.
    pub fn preview_review_submission(
        &self,
        session_id: &str,
        outcome: ReplayReviewOutcome,
    ) -> Result<ReplayReviewSubmissionPreview, ReplayError> {
        build_review_submission(self.session(session_id)?, self.limits(), outcome)
            .map(|(preview, _)| preview)
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
        if preview.workspace_id != session.id
            || receipt.id == 0
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
        if session
            .review
            .receipts
            .iter()
            .any(|existing| existing.id == receipt.id)
        {
            return Err(ReplayError::ReviewSubmissionUncertain(
                "the submitted GitHub review receipt was already recorded".to_string(),
            ));
        }
        for draft in &mut session.review.drafts {
            if draft_ids.contains(draft.id.as_str()) {
                draft.state = ReplayDraftState::Submitted;
            }
        }
        session.review.receipts.push(receipt.clone());
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
    let url = Url::parse(&response.html_url).map_err(|_| {
        ReplayError::InvalidMetadata("GitHub returned an invalid review receipt URL".to_string())
    })?;
    let expected_path = format!(
        "/{}/{}/pull/{}",
        request.repository_owner, request.repository_name, request.number
    );
    let expected_fragment = format!("pullrequestreview-{}", response.id);
    if url.scheme() != "https"
        || !url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case(&request.host))
        || !url.path().eq_ignore_ascii_case(&expected_path)
        || url.fragment() != Some(expected_fragment.as_str())
    {
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
    })
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
