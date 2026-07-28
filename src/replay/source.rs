//! Bounded, editor-owned GitHub metadata and immutable Git source resolution.

use std::{
    ffi::OsStr,
    io::Write as _,
    path::{Component, Path, PathBuf},
    process::{Command, ExitStatus, Output, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use url::Url;

use super::{
    digest, now_ms, ReplayError, ReplayLimits, ReplayReviewRole, ReplayWorkspace,
    ReplayWorkspacePreview,
};

const GITHUB_METADATA_FIELDS: &str = "number,url,title,body,author,baseRefName,baseRefOid,headRefName,headRefOid,headRepository,headRepositoryOwner,isCrossRepository,commits,changedFiles";
const GITHUB_CAPABILITIES_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!) { viewer { login } repository(owner: $owner, name: $name) { nameWithOwner pullRequest(number: $number) { number author { login } headRefName headRefOid headRepository { nameWithOwner viewerPermission } } } }";
const MAX_COMMAND_DIAGNOSTIC_BYTES: usize = 4 * 1024;
const MAX_COMMAND_DURATION: Duration = Duration::from_secs(45);
const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Validated, immutable SHA-1 or SHA-256 Git object identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GitObjectId(String);

impl GitObjectId {
    /// Parses an unambiguous complete Git object ID.
    pub fn parse(value: &str) -> Result<Self, ReplayError> {
        if !matches!(value.len(), 40 | 64) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ReplayError::InvalidObject(value.to_string()));
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    /// Borrows the complete pinned object identity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the short object prefix used only in a display-safe branch name.
    #[must_use]
    pub fn short(&self) -> &str {
        &self.0[..7]
    }
}

impl std::fmt::Display for GitObjectId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Validated Git repository used to confine every replay Git operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayRepository {
    /// Canonical current worktree root.
    pub root: PathBuf,
    /// Canonical shared Git directory identity.
    pub common_directory: PathBuf,
    /// Configured origin host.
    pub host: String,
    /// Validated origin repository owner.
    pub owner: String,
    /// Validated origin repository name.
    pub name: String,
}

impl ReplayRepository {
    /// Resolves the current repository without checking out or fetching a ref.
    pub fn discover(cwd: &Path) -> Result<Self, ReplayError> {
        let root = git_text(cwd, &["rev-parse", "--show-toplevel"], 16 * 1024)
            .map_err(|error| ReplayError::RepositoryMissing(error.to_string()))?;
        let root = std::fs::canonicalize(root.trim())
            .map_err(|error| ReplayError::RepositoryMissing(error.to_string()))?;
        let common = git_text(&root, &["rev-parse", "--git-common-dir"], 16 * 1024)?;
        let common_path = PathBuf::from(common.trim());
        let common_directory = if common_path.is_absolute() {
            common_path
        } else {
            root.join(common_path)
        };
        let common_directory = std::fs::canonicalize(&common_directory)
            .map_err(|error| ReplayError::RepositoryMissing(error.to_string()))?;
        let origin = git_text(&root, &["remote", "get-url", "origin"], 16 * 1024)
            .map_err(|error| ReplayError::RepositoryMissing(error.to_string()))?;
        let (host, owner, name) = parse_remote(origin.trim())?;
        Ok(Self {
            root,
            common_directory,
            host,
            owner,
            name,
        })
    }

    /// Returns the validated host-qualified GitHub repository selector.
    #[must_use]
    pub fn host_repository(&self) -> String {
        format!("{}/{}/{}", self.host, self.owner, self.name)
    }

    /// Returns a stable digest of the canonical shared repository identity.
    #[must_use]
    pub fn identity(&self) -> String {
        digest(self.common_directory.to_string_lossy().as_bytes())
    }
}

/// Supported first-release GitHub PR input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullRequestInput {
    /// PR number scoped to the verified current origin repository.
    Number(u64),
    /// Validated canonical PR URL.
    Url {
        /// Validated URL host.
        host: String,
        /// Validated base-repository owner.
        owner: String,
        /// Validated base-repository name.
        repository: String,
        /// Positive pull request number.
        number: u64,
        /// Canonical HTTPS pull request URL.
        url: String,
    },
}

impl PullRequestInput {
    /// Parses a positive current-repository PR number or canonical HTTPS PR URL.
    pub fn parse(input: &str) -> Result<Self, ReplayError> {
        let input = input.trim();
        if !input.is_empty() && input.bytes().all(|byte| byte.is_ascii_digit()) {
            let number = input
                .parse::<u64>()
                .map_err(|_| ReplayError::InvalidPullRequest(input.to_string()))?;
            if number == 0 {
                return Err(ReplayError::InvalidPullRequest(input.to_string()));
            }
            return Ok(Self::Number(number));
        }

        let url =
            Url::parse(input).map_err(|_| ReplayError::InvalidPullRequest(input.to_string()))?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(ReplayError::InvalidPullRequest(input.to_string()));
        }
        let host = url
            .host_str()
            .ok_or_else(|| ReplayError::InvalidPullRequest(input.to_string()))?;
        let segments = url
            .path_segments()
            .map(Iterator::collect::<Vec<_>>)
            .ok_or_else(|| ReplayError::InvalidPullRequest(input.to_string()))?;
        let [owner, repository, pull, raw_number] = segments.as_slice() else {
            return Err(ReplayError::InvalidPullRequest(input.to_string()));
        };
        if *pull != "pull"
            || !safe_repository_component(owner)
            || !safe_repository_component(repository)
        {
            return Err(ReplayError::InvalidPullRequest(input.to_string()));
        }
        let number = raw_number
            .parse::<u64>()
            .ok()
            .filter(|number| *number > 0)
            .ok_or_else(|| ReplayError::InvalidPullRequest(input.to_string()))?;
        Ok(Self::Url {
            host: host.to_ascii_lowercase(),
            owner: (*owner).to_string(),
            repository: (*repository).to_string(),
            number,
            url: format!("https://{host}/{owner}/{repository}/pull/{number}"),
        })
    }

    fn validate_repository(&self, repository: &ReplayRepository) -> Result<(), ReplayError> {
        match self {
            Self::Number(_) => Ok(()),
            Self::Url {
                host,
                owner,
                repository: name,
                ..
            } if host.eq_ignore_ascii_case(&repository.host)
                && owner.eq_ignore_ascii_case(&repository.owner)
                && name.eq_ignore_ascii_case(&repository.name) =>
            {
                Ok(())
            }
            Self::Url { .. } => Err(ReplayError::RepositoryMismatch),
        }
    }

    fn argument(&self) -> String {
        match self {
            Self::Number(number) => number.to_string(),
            Self::Url { url, .. } => url.clone(),
        }
    }

    fn expected_number(&self) -> u64 {
        match self {
            Self::Number(number) | Self::Url { number, .. } => *number,
        }
    }
}

/// Bounded original-author commit information shown in the replay guide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayCommitSummary {
    /// Pinned source commit when the provider supplies one.
    pub oid: Option<GitObjectId>,
    /// Untrusted, bounded author-written commit subject.
    pub headline: String,
    /// Untrusted, bounded original commit explanation.
    pub body: String,
}

/// Authenticated viewer access to the exact original pull-request head.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayRepositoryPermission {
    /// GitHub did not establish access for the authenticated viewer.
    #[default]
    Unknown,
    /// The authenticated viewer has no access to the head repository.
    None,
    /// The authenticated viewer can read the head repository.
    Read,
    /// The authenticated viewer has triage access to the head repository.
    Triage,
    /// The authenticated viewer can write to the head repository.
    Write,
    /// The authenticated viewer can maintain the head repository.
    Maintain,
    /// The authenticated viewer can administer the head repository.
    Admin,
}

impl ReplayRepositoryPermission {
    fn from_github(value: Option<&str>) -> Self {
        match value {
            Some("NONE") => Self::None,
            Some("READ") => Self::Read,
            Some("TRIAGE") => Self::Triage,
            Some("WRITE") => Self::Write,
            Some("MAINTAIN") => Self::Maintain,
            Some("ADMIN") => Self::Admin,
            _ => Self::Unknown,
        }
    }
}

/// Read-only, identity-verified capabilities for one pinned GitHub PR head.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReplayGitHubCapabilities {
    /// Authenticated GitHub login verified against the original PR identity.
    pub viewer: Option<String>,
    /// Viewer permission on the exact original head repository, not the base.
    pub head_permission: ReplayRepositoryPermission,
    /// Read-only capability lookup failure; privileged actions remain blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// Immutable identity of the author's original pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayPullRequest {
    /// Configured, validated GitHub host.
    pub host: String,
    /// Base repository owner.
    pub repository_owner: String,
    /// Base repository name.
    pub repository_name: String,
    /// Positive original pull request number.
    pub number: u64,
    /// Validated canonical original pull request URL.
    pub url: String,
    /// Original author's GitHub login, when visible.
    pub author: Option<String>,
    /// Validated original base reference name.
    pub base_ref: String,
    /// Pinned tip of the original base reference.
    pub base_ref_tip: GitObjectId,
    /// Original head repository owner, including fork identity.
    pub head_repository_owner: String,
    /// Original head repository name.
    pub head_repository_name: String,
    /// Validated original head reference.
    pub head_ref: String,
    /// Exact pinned original author head.
    pub head_commit: GitObjectId,
    /// Whether the original head lives in another GitHub repository.
    pub cross_repository: bool,
    /// Read-only viewer identity and access for the exact original PR head.
    #[serde(default)]
    pub capabilities: ReplayGitHubCapabilities,
    /// Unix-millisecond metadata capture time.
    pub captured_at_ms: u64,
}

/// Original author context retained for understanding, never as instructions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayReviewContext {
    /// Bounded original pull request title.
    pub title: String,
    /// Bounded, untrusted original pull request description.
    pub body: String,
    /// Original author identity when visible.
    pub author: Option<String>,
    /// Bounded original-author commit messages.
    pub commits: Vec<ReplayCommitSummary>,
    /// Provider-reported changed-file count.
    pub changed_files: usize,
}

/// Resolved metadata before the user authorizes any missing-object fetch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayResolvedPullRequest {
    /// Stable editor-owned source handle.
    pub source_id: String,
    /// Validated originating repository.
    pub repository: ReplayRepository,
    /// Pinned original author and pull request identity.
    pub pull_request: ReplayPullRequest,
    /// Bounded read-only review context.
    pub context: ReplayReviewContext,
    /// Exact endpoint objects not yet present in the local object store.
    pub missing_objects: Vec<GitObjectId>,
}

/// First-release local or GitHub replay source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplaySourceKind {
    /// Original GitHub pull request resolved through the trusted editor.
    #[serde(rename = "github_pull_request", alias = "git_hub_pull_request")]
    GitHubPullRequest,
    /// Immutable locally selected commit or reference.
    LocalRevision,
    /// Explicitly pinned local commit endpoints.
    LocalRange,
}

/// Complete, immutable source from which learning exercises are compiled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplaySource {
    /// Stable editor-owned source handle.
    pub id: String,
    /// Verified originating repository and shared Git identity.
    pub repository: ReplayRepository,
    /// Whether the source came from GitHub or pinned local objects.
    pub kind: ReplaySourceKind,
    /// Exact replay base; GitHub sources use the computed merge base.
    pub base_commit: GitObjectId,
    /// Exact original target tree.
    pub target_commit: GitObjectId,
    /// Complete bounded source diff.
    pub patch: String,
    /// SHA-256 digest of the complete canonical diff.
    pub patch_digest: String,
    /// Original pull request identity when the source is GitHub.
    pub pull_request: Option<ReplayPullRequest>,
    /// Original author and commit context.
    pub review_context: Option<ReplayReviewContext>,
}

/// Read-only identity of the original author head, never the learning scratch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayAuthorWorkspacePreview {
    /// The original checkout remains unchanged when this worktree is opened.
    pub repository_root: PathBuf,
    /// Exact durable sibling reserved for this original pull-request head.
    pub root: PathBuf,
    /// Separate Replay-owned branch, not the actual pull-request branch.
    pub branch: String,
    /// Complete, immutable original author-head commit.
    pub head_commit: GitObjectId,
    /// Host-qualified original head repository, including a fork when present.
    pub head_repository: String,
    /// Original pull-request branch, retained for a later explicit push.
    pub head_ref: String,
    /// Authenticated original pull-request author.
    pub viewer: String,
    /// Whether a sibling already exists and must be independently verified.
    pub existing: bool,
}

impl ReplayAuthorWorkspacePreview {
    /// Binds confirmation to the exact original head, fork, branch, and path.
    #[must_use]
    pub fn digest(&self) -> String {
        let identity = serde_json::json!({
            "repository_root": self.repository_root,
            "root": self.root,
            "branch": self.branch,
            "head_commit": self.head_commit,
            "head_repository": self.head_repository,
            "head_ref": self.head_ref,
            "viewer": self.viewer,
            "existing": self.existing,
        });
        digest(identity.to_string().as_bytes())
    }
}

/// Independently verified real author worktree, separate from replay scratch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayAuthorWorkspace {
    /// Canonical durable sibling containing the real original PR source.
    pub root: PathBuf,
    /// Separate Replay-owned local author branch.
    pub branch: String,
    /// Original PR head from which author edits must descend.
    pub head_commit: GitObjectId,
    /// Host-qualified exact original head repository, including forks.
    pub head_repository: String,
    /// Original remote PR branch, never implicitly checked out or pushed.
    pub head_ref: String,
    /// Whether this confirmation created, rather than reopened, the worktree.
    pub created_by_replay: bool,
}

impl ReplayAuthorWorkspace {
    /// Resolves a real, regular PR file without following out-of-root symlinks.
    ///
    /// # Errors
    ///
    /// Returns an error for missing, non-regular, absolute, traversing, or
    /// symbolic-link source paths.
    pub fn source_path(&self, path: &Path) -> Result<PathBuf, ReplayError> {
        validate_relative_path(path)?;
        let joined = self.root.join(path);
        let metadata = match std::fs::symlink_metadata(&joined) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(ReplayError::NotFound {
                    kind: "original PR source file",
                    id: path.display().to_string(),
                });
            }
            Err(error) => return Err(ReplayError::Filesystem(error.to_string())),
        };
        if !metadata.file_type().is_file() {
            return Err(ReplayError::UnsafePath(path.display().to_string()));
        }
        let canonical = std::fs::canonicalize(&joined)
            .map_err(|error| ReplayError::Filesystem(error.to_string()))?;
        if !canonical.starts_with(&self.root) {
            return Err(ReplayError::UnsafePath(path.display().to_string()));
        }
        Ok(canonical)
    }
}

/// Pinned local feature branch and its explicitly selected or detected base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayResolvedLocalBranch {
    /// Validated reviewer-selected feature reference.
    pub head_ref: String,
    /// Validated explicit base or detected origin default branch.
    pub base_ref: String,
    /// Immutable merge-base-to-feature source and its complete original patch.
    pub source: ReplaySource,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitHubMetadata {
    number: u64,
    url: String,
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    author: Option<GitHubActor>,
    base_ref_name: String,
    base_ref_oid: String,
    head_ref_name: String,
    head_ref_oid: String,
    #[serde(default)]
    head_repository: Option<GitHubRepository>,
    #[serde(default)]
    head_repository_owner: Option<GitHubActor>,
    #[serde(default)]
    is_cross_repository: bool,
    #[serde(default)]
    commits: Vec<GitHubCommit>,
    #[serde(default)]
    changed_files: usize,
}

#[derive(Debug, Deserialize)]
struct GitHubActor {
    #[serde(default)]
    login: String,
}

#[derive(Debug, Deserialize)]
struct GitHubRepository {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitHubCommit {
    #[serde(default)]
    oid: String,
    #[serde(default)]
    message_headline: String,
    #[serde(default)]
    message_body: String,
}

#[derive(Debug, Deserialize)]
struct GitHubCapabilityEnvelope {
    data: Option<GitHubCapabilityData>,
}

#[derive(Debug, Deserialize)]
struct GitHubCapabilityData {
    viewer: GitHubActor,
    repository: Option<GitHubCapabilityRepository>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitHubCapabilityRepository {
    name_with_owner: String,
    pull_request: Option<GitHubCapabilityPullRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitHubCapabilityPullRequest {
    number: u64,
    author: Option<GitHubActor>,
    head_ref_name: String,
    head_ref_oid: String,
    head_repository: Option<GitHubCapabilityHeadRepository>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitHubCapabilityHeadRepository {
    name_with_owner: String,
    viewer_permission: Option<String>,
}

/// Resolves read-only GitHub metadata without fetching or changing a branch.
pub fn resolve_pull_request(
    cwd: &Path,
    input: &str,
    limits: ReplayLimits,
) -> Result<ReplayResolvedPullRequest, ReplayError> {
    let repository = ReplayRepository::discover(cwd)?;
    let input = PullRequestInput::parse(input)?;
    input.validate_repository(&repository)?;

    let mut command = Command::new("gh");
    command
        .current_dir(&repository.root)
        .args(["pr", "view"])
        .arg(input.argument())
        .args(["--repo", &repository.host_repository()])
        .args(["--json", GITHUB_METADATA_FIELDS])
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1");
    let metadata = run_command(&mut command, limits.max_metadata_bytes)?;
    let metadata: GitHubMetadata = serde_json::from_slice(&metadata)
        .map_err(|error| ReplayError::InvalidMetadata(error.to_string()))?;
    let mut resolved = parse_pull_request_metadata(repository, input, metadata, limits)?;
    match resolve_pull_request_capabilities(&resolved.pull_request, &resolved.repository, limits) {
        Ok(capabilities) => resolved.pull_request.capabilities = capabilities,
        Err(error @ ReplayError::CommandFailed { .. }) => {
            resolved.pull_request.capabilities.warning = Some(format!(
                "GitHub viewer could not be verified: {error}. Review publication and PR-head editing remain unavailable."
            ));
        }
        Err(error) => return Err(error),
    }
    Ok(resolved)
}

fn resolve_pull_request_capabilities(
    request: &ReplayPullRequest,
    repository: &ReplayRepository,
    limits: ReplayLimits,
) -> Result<ReplayGitHubCapabilities, ReplayError> {
    let mut command = Command::new("gh");
    command
        .current_dir(&repository.root)
        .args(["api", "graphql", "--hostname", &repository.host])
        .args(["--raw-field", &format!("query={GITHUB_CAPABILITIES_QUERY}")])
        .args(["--raw-field", &format!("owner={}", repository.owner)])
        .args(["--raw-field", &format!("name={}", repository.name)])
        .args(["--field", &format!("number={}", request.number)])
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1");
    let output = run_command(&mut command, limits.max_metadata_bytes)?;
    parse_pull_request_capabilities(request, &output)
}

/// Refreshes authenticated access without changing a recovered PR identity.
///
/// This reads GitHub metadata for the exact pinned original repository, pull
/// request, head branch, and commit. It never fetches, checks out a branch,
/// writes a review, or replaces the recovered original source.
pub fn refresh_pull_request_capabilities(
    source: &mut ReplaySource,
    limits: ReplayLimits,
) -> Result<(), ReplayError> {
    let Some(request) = source.pull_request.as_mut() else {
        return Ok(());
    };
    request.capabilities = resolve_pull_request_capabilities(request, &source.repository, limits)?;
    Ok(())
}

fn parse_pull_request_capabilities(
    request: &ReplayPullRequest,
    output: &[u8],
) -> Result<ReplayGitHubCapabilities, ReplayError> {
    let envelope: GitHubCapabilityEnvelope = serde_json::from_slice(output)
        .map_err(|error| ReplayError::InvalidMetadata(error.to_string()))?;
    let data = envelope.data.ok_or_else(|| {
        ReplayError::InvalidMetadata("GitHub did not return authenticated PR capabilities".into())
    })?;
    if !safe_repository_component(&data.viewer.login) {
        return Err(ReplayError::InvalidMetadata(
            "unsafe authenticated GitHub viewer".into(),
        ));
    }
    let repository = data.repository.ok_or(ReplayError::RepositoryMismatch)?;
    let expected_repository = format!("{}/{}", request.repository_owner, request.repository_name);
    if !repository
        .name_with_owner
        .eq_ignore_ascii_case(&expected_repository)
    {
        return Err(ReplayError::RepositoryMismatch);
    }
    let pull_request = repository
        .pull_request
        .ok_or(ReplayError::RepositoryMismatch)?;
    if pull_request.number != request.number {
        return Err(ReplayError::RepositoryMismatch);
    }
    if pull_request.head_ref_name != request.head_ref
        || GitObjectId::parse(&pull_request.head_ref_oid)? != request.head_commit
    {
        return Err(ReplayError::SourceRefMoved);
    }
    let observed_author = pull_request.author.map(|author| author.login);
    if !same_optional_github_login(request.author.as_deref(), observed_author.as_deref()) {
        return Err(ReplayError::InvalidMetadata(
            "the original pull request author changed during capability resolution".into(),
        ));
    }

    let head_permission = if let Some(head) = pull_request.head_repository {
        let expected_head = format!(
            "{}/{}",
            request.head_repository_owner, request.head_repository_name
        );
        if !head.name_with_owner.eq_ignore_ascii_case(&expected_head) {
            return Err(ReplayError::RepositoryMismatch);
        }
        ReplayRepositoryPermission::from_github(head.viewer_permission.as_deref())
    } else {
        ReplayRepositoryPermission::Unknown
    };

    Ok(ReplayGitHubCapabilities {
        viewer: Some(data.viewer.login),
        head_permission,
        warning: None,
    })
}

fn same_optional_github_login(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
        (None, None) => true,
        _ => false,
    }
}

fn parse_pull_request_metadata(
    repository: ReplayRepository,
    input: PullRequestInput,
    metadata: GitHubMetadata,
    limits: ReplayLimits,
) -> Result<ReplayResolvedPullRequest, ReplayError> {
    if metadata.number != input.expected_number() {
        return Err(ReplayError::RepositoryMismatch);
    }
    let canonical = PullRequestInput::parse(&metadata.url)?;
    canonical.validate_repository(&repository)?;
    if canonical.expected_number() != metadata.number {
        return Err(ReplayError::RepositoryMismatch);
    }
    if metadata.body.len() > limits.max_description_bytes {
        return Err(ReplayError::LimitExceeded {
            kind: "pull request description",
            limit: limits.max_description_bytes,
        });
    }
    if metadata.commits.len() > limits.max_commit_summaries {
        return Err(ReplayError::LimitExceeded {
            kind: "pull request commit summaries",
            limit: limits.max_commit_summaries,
        });
    }
    if metadata.changed_files > limits.max_changed_files {
        return Err(ReplayError::LimitExceeded {
            kind: "changed files",
            limit: limits.max_changed_files,
        });
    }
    validate_git_reference(&metadata.base_ref_name)?;
    validate_git_reference(&metadata.head_ref_name)?;
    let base_ref_tip = GitObjectId::parse(&metadata.base_ref_oid)?;
    let head_commit = GitObjectId::parse(&metadata.head_ref_oid)?;
    let author = metadata.author.map(|author| author.login);
    let head_repository_owner = metadata
        .head_repository_owner
        .map(|owner| owner.login)
        .filter(|owner| !owner.is_empty())
        .unwrap_or_else(|| repository.owner.clone());
    let head_repository_name = metadata
        .head_repository
        .map(|head| head.name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| repository.name.clone());
    if !safe_repository_component(&head_repository_owner)
        || !safe_repository_component(&head_repository_name)
    {
        return Err(ReplayError::InvalidMetadata(
            "unsafe pull request head repository".to_string(),
        ));
    }
    let commits = metadata
        .commits
        .into_iter()
        .map(|commit| {
            let oid = if commit.oid.is_empty() {
                None
            } else {
                Some(GitObjectId::parse(&commit.oid)?)
            };
            Ok(ReplayCommitSummary {
                oid,
                headline: commit.message_headline,
                body: commit.message_body,
            })
        })
        .collect::<Result<Vec<_>, ReplayError>>()?;
    let pull_request = ReplayPullRequest {
        host: repository.host.clone(),
        repository_owner: repository.owner.clone(),
        repository_name: repository.name.clone(),
        number: metadata.number,
        url: metadata.url,
        author: author.clone(),
        base_ref: metadata.base_ref_name,
        base_ref_tip,
        head_repository_owner,
        head_repository_name,
        head_ref: metadata.head_ref_name,
        head_commit,
        cross_repository: metadata.is_cross_repository,
        capabilities: ReplayGitHubCapabilities::default(),
        captured_at_ms: now_ms(),
    };
    let mut missing_objects = Vec::with_capacity(2);
    for object in [&pull_request.base_ref_tip, &pull_request.head_commit] {
        if !has_git_commit(&repository.root, object)? {
            missing_objects.push(object.clone());
        }
    }
    Ok(ReplayResolvedPullRequest {
        source_id: uuid::Uuid::new_v4().to_string(),
        repository,
        pull_request,
        context: ReplayReviewContext {
            title: metadata.title,
            body: metadata.body,
            author,
            commits,
            changed_files: metadata.changed_files,
        },
        missing_objects,
    })
}

/// Fetches only exact verified PR refs after the editor has obtained confirmation.
pub fn fetch_pull_request_objects(
    resolved: &mut ReplayResolvedPullRequest,
    confirmed: bool,
) -> Result<(), ReplayError> {
    if resolved.missing_objects.is_empty() {
        return Ok(());
    }
    if !confirmed {
        return Err(ReplayError::WorkspaceConfirmationRequired);
    }
    let request = &resolved.pull_request;
    validate_git_reference(&request.base_ref)?;
    let namespace = format!(
        "refs/replay/pr-{}-{}",
        request.number,
        request.head_commit.short()
    );
    let base = format!("refs/heads/{}:{namespace}/base", request.base_ref);
    let head = format!("refs/pull/{}/head:{namespace}/head", request.number);
    let mut command = replay_git_command(&resolved.repository.root);
    command
        .args(["-c", "core.hooksPath=/dev/null", "fetch"])
        .args([
            "--no-tags",
            "--no-recurse-submodules",
            "--no-write-fetch-head",
        ])
        .arg("origin")
        .arg(base)
        .arg(head);
    run_command(&mut command, MAX_COMMAND_DIAGNOSTIC_BYTES)?;
    for object in [&request.base_ref_tip, &request.head_commit] {
        if !has_git_commit(&resolved.repository.root, object)? {
            return Err(ReplayError::SourceRefMoved);
        }
    }
    let fetched_base = git_object(&resolved.repository.root, &format!("{namespace}/base"))?;
    let fetched_head = git_object(&resolved.repository.root, &format!("{namespace}/head"))?;
    if fetched_base != request.base_ref_tip || fetched_head != request.head_commit {
        return Err(ReplayError::SourceRefMoved);
    }
    resolved.missing_objects.clear();
    Ok(())
}

/// Captures the complete original PR diff from immutable, locally verified objects.
pub fn finalize_pull_request(
    resolved: &ReplayResolvedPullRequest,
    limits: ReplayLimits,
) -> Result<ReplaySource, ReplayError> {
    if !resolved.missing_objects.is_empty() {
        return Err(ReplayError::MissingObjects);
    }
    let request = &resolved.pull_request;
    let merge_base = git_text(
        &resolved.repository.root,
        &[
            "merge-base",
            request.base_ref_tip.as_str(),
            request.head_commit.as_str(),
        ],
        1024,
    )?;
    let base_commit = GitObjectId::parse(merge_base.trim())?;
    build_source(
        ReplaySourceSeed {
            id: resolved.source_id.clone(),
            repository: resolved.repository.clone(),
            kind: ReplaySourceKind::GitHubPullRequest,
            base_commit,
            target_commit: request.head_commit.clone(),
            pull_request: Some(request.clone()),
            review_context: Some(resolved.context.clone()),
        },
        limits,
    )
}

/// Resolves a locally present commit or explicit commit range without network access.
pub fn resolve_local_source(
    cwd: &Path,
    input: &str,
    limits: ReplayLimits,
) -> Result<ReplaySource, ReplayError> {
    let repository = ReplayRepository::discover(cwd)?;
    let input = input.trim();
    if input.is_empty() || input.starts_with('-') {
        return Err(ReplayError::InvalidObject(input.to_string()));
    }
    let (kind, base, target) = if let Some((base, target)) = input.split_once("..") {
        if base.is_empty() || target.is_empty() || target.contains("..") {
            return Err(ReplayError::InvalidObject(input.to_string()));
        }
        (
            ReplaySourceKind::LocalRange,
            git_object(&repository.root, base)?,
            git_object(&repository.root, target)?,
        )
    } else {
        let target = git_object(&repository.root, input)?;
        let parent = git_text(
            &repository.root,
            &[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{target}^"),
            ],
            1024,
        )?;
        (
            ReplaySourceKind::LocalRevision,
            GitObjectId::parse(parent.trim())?,
            target,
        )
    };
    build_source(
        ReplaySourceSeed {
            id: uuid::Uuid::new_v4().to_string(),
            repository,
            kind,
            base_commit: base,
            target_commit: target,
            pull_request: None,
            review_context: None,
        },
        limits,
    )
}

/// Resolves a local feature branch against its real merge base without a checkout.
///
/// When no base is supplied, the verified `origin/HEAD` target is preferred,
/// followed by locally present `origin/main`, `origin/master`, `main`, and
/// `master` references. Both endpoints are pinned before computing the merge
/// base, so newer changes on the base branch never enter the reviewer patch.
///
/// # Errors
///
/// Returns an error when the repository, references, merge base, or complete
/// bounded canonical patch cannot be resolved entirely from local Git objects.
pub fn resolve_local_branch_source(
    cwd: &Path,
    head: &str,
    base: Option<&str>,
    limits: ReplayLimits,
) -> Result<ReplayResolvedLocalBranch, ReplayError> {
    let repository = ReplayRepository::discover(cwd)?;
    let head_ref = head.trim();
    validate_git_reference(head_ref)?;
    let target_commit = git_object(&repository.root, head_ref)?;

    let base_ref = match base
        .map(str::trim)
        .filter(|reference| !reference.is_empty())
    {
        Some(reference) => {
            validate_git_reference(reference)?;
            reference.to_string()
        }
        None => default_local_base(&repository.root)?,
    };
    let base_tip = git_object(&repository.root, &base_ref)?;
    let merge_base = git_text(
        &repository.root,
        &["merge-base", base_tip.as_str(), target_commit.as_str()],
        /*limit*/ 1024,
    )?;
    let base_commit = GitObjectId::parse(merge_base.trim())?;
    let source = build_source(
        ReplaySourceSeed {
            id: uuid::Uuid::new_v4().to_string(),
            repository,
            kind: ReplaySourceKind::LocalRange,
            base_commit,
            target_commit,
            pull_request: None,
            review_context: None,
        },
        limits,
    )?;

    Ok(ReplayResolvedLocalBranch {
        head_ref: head_ref.to_string(),
        base_ref,
        source,
    })
}

fn default_local_base(root: &Path) -> Result<String, ReplayError> {
    if let Ok(reference) = git_text(
        root,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        /*limit*/ 1024,
    ) {
        let reference = reference.trim();
        if validate_git_reference(reference).is_ok() && git_object(root, reference).is_ok() {
            return Ok(reference
                .strip_prefix("refs/remotes/")
                .unwrap_or(reference)
                .to_string());
        }
    }

    for reference in ["origin/main", "origin/master", "main", "master"] {
        if git_object(root, reference).is_ok() {
            return Ok(reference.to_string());
        }
    }

    Err(ReplayError::RepositoryMissing(
        "no local default base branch is available; specify an explicit base".to_string(),
    ))
}

struct ReplaySourceSeed {
    id: String,
    repository: ReplayRepository,
    kind: ReplaySourceKind,
    base_commit: GitObjectId,
    target_commit: GitObjectId,
    pull_request: Option<ReplayPullRequest>,
    review_context: Option<ReplayReviewContext>,
}

fn build_source(seed: ReplaySourceSeed, limits: ReplayLimits) -> Result<ReplaySource, ReplayError> {
    let ReplaySourceSeed {
        id,
        repository,
        kind,
        base_commit,
        target_commit,
        pull_request,
        review_context,
    } = seed;
    let mut command = replay_git_command(&repository.root);
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--find-renames",
            "--full-index",
            "--no-color",
        ])
        .arg(base_commit.as_str())
        .arg(target_commit.as_str())
        .arg("--");
    let output = run_command(&mut command, limits.max_patch_bytes)?;
    let line_count = output.iter().filter(|byte| **byte == b'\n').count();
    if line_count > limits.max_patch_lines {
        return Err(ReplayError::LimitExceeded {
            kind: "canonical patch lines",
            limit: limits.max_patch_lines,
        });
    }
    let patch_digest = digest(&output);
    let patch = String::from_utf8(output)
        .map_err(|_| ReplayError::InvalidPatch("canonical patch is not UTF-8".to_string()))?;
    Ok(ReplaySource {
        id,
        repository,
        kind,
        base_commit,
        target_commit,
        patch,
        patch_digest,
        pull_request,
        review_context,
    })
}

/// Previews or explicitly opens the verified author's real original PR source.
///
/// The proposed branch and sibling are distinct from both the original PR
/// branch and the merge-base learning worktree. Previewing does not create a
/// directory, branch, commit, or remote request. Existing author edits are
/// deliberately preserved when the exact verified worktree is reopened.
///
/// # Errors
///
/// Returns an error for local sources, unverified or read-only authors, moved
/// source identities, unrelated existing branches or worktrees, and bounded
/// Git or filesystem failures.
pub fn prepare_author_workspace(
    source: &ReplaySource,
    confirmed: bool,
) -> Result<(ReplayAuthorWorkspacePreview, Option<ReplayAuthorWorkspace>), ReplayError> {
    if source.kind != ReplaySourceKind::GitHubPullRequest {
        return Err(ReplayError::AuthorWorkspaceUnavailable(
            "only an original GitHub pull request has an author worktree".to_string(),
        ));
    }
    let request = source.pull_request.as_ref().ok_or_else(|| {
        ReplayError::AuthorWorkspaceUnavailable(
            "the original GitHub pull request could not be verified".to_string(),
        )
    })?;
    if source.target_commit != request.head_commit {
        return Err(ReplayError::SourceRefMoved);
    }
    if ReplayReviewRole::from_pull_request(Some(request)) != ReplayReviewRole::Author {
        return Err(ReplayError::AuthorWorkspaceUnavailable(
            "only the verified original PR author can open its real source".to_string(),
        ));
    }
    if !matches!(
        request.capabilities.head_permission,
        ReplayRepositoryPermission::Write
            | ReplayRepositoryPermission::Maintain
            | ReplayRepositoryPermission::Admin
    ) {
        return Err(ReplayError::AuthorWorkspaceUnavailable(
            "the verified author does not have write access to the original head repository"
                .to_string(),
        ));
    }
    let viewer = request.capabilities.viewer.clone().ok_or_else(|| {
        ReplayError::AuthorWorkspaceUnavailable(
            "the original author's authenticated identity could not be confirmed".to_string(),
        )
    })?;

    let parent =
        source.repository.root.parent().ok_or_else(|| {
            ReplayError::UnsafePath("repository has no durable parent".to_string())
        })?;
    let slug = format!("pr-{}-{}", request.number, request.head_commit.short());
    let branch = format!("replay/author/{slug}");
    let directory_name = format!("{}.replay-author-{slug}", source.repository.name);
    let root = parent.join(&directory_name);
    let preview = ReplayAuthorWorkspacePreview {
        repository_root: source.repository.root.clone(),
        existing: root.exists() || std::fs::symlink_metadata(&root).is_ok(),
        root,
        branch,
        head_commit: request.head_commit.clone(),
        head_repository: format!(
            "{}/{}/{}",
            request.host, request.head_repository_owner, request.head_repository_name
        ),
        head_ref: request.head_ref.clone(),
        viewer,
    };
    if !confirmed {
        return Ok((preview, None));
    }
    if !has_git_commit(&source.repository.root, &request.head_commit)? {
        return Err(ReplayError::MissingObjects);
    }
    if preview.existing {
        let workspace = existing_author_workspace(source, &preview)?;
        return Ok((preview, Some(workspace)));
    }

    let branch_reference = format!("refs/heads/{}", preview.branch);
    let mut branch_command = replay_git_command(&source.repository.root);
    branch_command
        .args(["show-ref", "--verify", "--quiet"])
        .arg(&branch_reference);
    let branch_exists = run_command_status(&mut branch_command)
        .map_err(|error| ReplayError::Filesystem(error.to_string()))?
        .success();
    if branch_exists
        && !author_head_is_ancestor(
            &source.repository.root,
            &request.head_commit,
            &branch_reference,
        )?
    {
        return Err(ReplayError::WorkspaceExists(preview.branch.clone()));
    }

    let relative_root = Path::new("..").join(&directory_name);
    let mut command = replay_git_command(&source.repository.root);
    command.args(["worktree", "add"]);
    if branch_exists {
        command.arg(&relative_root).arg(&preview.branch);
    } else {
        command
            .arg("-b")
            .arg(&preview.branch)
            .arg(&relative_root)
            .arg(request.head_commit.as_str());
    }
    if let Err(error) = run_command(&mut command, MAX_COMMAND_DIAGNOSTIC_BYTES) {
        if preview.root.exists() || std::fs::symlink_metadata(&preview.root).is_ok() {
            if let Ok(workspace) = existing_author_workspace(source, &preview) {
                return Ok((preview, Some(workspace)));
            }
        }
        return Err(error);
    }
    let mut workspace = existing_author_workspace(source, &preview)?;
    workspace.created_by_replay = true;
    Ok((preview, Some(workspace)))
}

fn existing_author_workspace(
    source: &ReplaySource,
    preview: &ReplayAuthorWorkspacePreview,
) -> Result<ReplayAuthorWorkspace, ReplayError> {
    let conflict = || ReplayError::WorkspaceExists(preview.root.display().to_string());
    let metadata = std::fs::symlink_metadata(&preview.root).map_err(|_| conflict())?;
    if !metadata.file_type().is_dir() {
        return Err(conflict());
    }
    let canonical_root = std::fs::canonicalize(&preview.root).map_err(|_| conflict())?;
    if canonical_root != preview.root {
        return Err(conflict());
    }
    let actual_root = git_text(&preview.root, &["rev-parse", "--show-toplevel"], 16 * 1024)
        .map_err(|_| conflict())?;
    let actual_root = std::fs::canonicalize(actual_root.trim()).map_err(|_| conflict())?;
    if actual_root != canonical_root {
        return Err(conflict());
    }
    let actual_branch = git_text(
        &preview.root,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        1024,
    )
    .map_err(|_| conflict())?;
    if actual_branch.trim() != preview.branch {
        return Err(conflict());
    }
    let actual_head =
        git_text(&preview.root, &["rev-parse", "HEAD"], 1024).map_err(|_| conflict())?;
    let actual_head = GitObjectId::parse(actual_head.trim()).map_err(|_| conflict())?;
    if !author_head_is_ancestor(&preview.root, &preview.head_commit, actual_head.as_str())
        .map_err(|_| conflict())?
    {
        return Err(conflict());
    }
    let common = git_text(&preview.root, &["rev-parse", "--git-common-dir"], 16 * 1024)
        .map_err(|_| conflict())?;
    let common = PathBuf::from(common.trim());
    let common = if common.is_absolute() {
        common
    } else {
        preview.root.join(common)
    };
    let common = std::fs::canonicalize(common).map_err(|_| conflict())?;
    if common != source.repository.common_directory {
        return Err(conflict());
    }

    Ok(ReplayAuthorWorkspace {
        root: canonical_root,
        branch: preview.branch.clone(),
        head_commit: preview.head_commit.clone(),
        head_repository: preview.head_repository.clone(),
        head_ref: preview.head_ref.clone(),
        created_by_replay: false,
    })
}

fn author_head_is_ancestor(
    root: &Path,
    head: &GitObjectId,
    descendant: &str,
) -> Result<bool, ReplayError> {
    let mut command = replay_git_command(root);
    command.args(["merge-base", "--is-ancestor", head.as_str(), descendant]);
    let status = run_command_status(&mut command)?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(ReplayError::CommandFailed {
            program: "git".to_string(),
            message: "could not verify the original PR head ancestry".to_string(),
        }),
    }
}

/// Produces the preview or creates the explicitly confirmed scratch worktree.
pub fn prepare_workspace(
    source: &ReplaySource,
    confirmed: bool,
) -> Result<(ReplayWorkspacePreview, Option<ReplayWorkspace>), ReplayError> {
    let repository_name = &source.repository.name;
    let (branch, slug) = if let Some(request) = &source.pull_request {
        let slug = format!("pr-{}-{}", request.number, request.head_commit.short());
        (format!("replay/{slug}"), slug)
    } else {
        let slug = format!("revision-{}", source.target_commit.short());
        (format!("replay/{slug}"), slug)
    };
    let parent =
        source.repository.root.parent().ok_or_else(|| {
            ReplayError::UnsafePath("repository has no durable parent".to_string())
        })?;
    let directory_name = format!("{repository_name}.replay-{slug}");
    let root = parent.join(&directory_name);
    let relative_root = Path::new("..").join(&directory_name);
    let preview = ReplayWorkspacePreview {
        repository_root: source.repository.root.clone(),
        root: root.clone(),
        branch: branch.clone(),
        base_commit: source.base_commit.clone(),
    };
    if !confirmed {
        return Ok((preview, None));
    }
    if root.exists() || std::fs::symlink_metadata(&root).is_ok() {
        let workspace = existing_workspace(source, &root, &branch)?;
        return Ok((preview, Some(workspace)));
    }
    let mut branch_command = replay_git_command(&source.repository.root);
    branch_command
        .args(["show-ref", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"));
    let branch_exists = run_command_status(&mut branch_command)
        .map_err(|error| ReplayError::Filesystem(error.to_string()))?
        .success();
    if branch_exists
        && git_object(&source.repository.root, &format!("refs/heads/{branch}"))?
            != source.base_commit
    {
        return Err(ReplayError::WorkspaceExists(branch));
    }
    let mut command = replay_git_command(&source.repository.root);
    command.args(["-c", "core.hooksPath=/dev/null", "worktree", "add"]);
    if branch_exists {
        command.arg(&relative_root).arg(&branch);
    } else {
        command
            .arg("-b")
            .arg(&branch)
            .arg(&relative_root)
            .arg(source.base_commit.as_str());
    }
    run_command(&mut command, MAX_COMMAND_DIAGNOSTIC_BYTES)?;
    Ok((
        preview,
        Some(ReplayWorkspace {
            root,
            branch,
            base_commit: source.base_commit.clone(),
            created_by_replay: true,
        }),
    ))
}

/// Reopens an existing, independently verified original review worktree.
///
/// Unlike [`prepare_workspace`], this operation never creates a worktree or
/// branch. It verifies the canonical scratch path, shared repository, original
/// branch, merge base, and clean working tree.
///
/// # Errors
///
/// Returns an error if the worktree is absent, unsafe, associated with another
/// repository or branch, no longer at its original merge base, or contains
/// saved or untracked reviewer changes.
pub fn reopen_existing_workspace(source: &ReplaySource) -> Result<ReplayWorkspace, ReplayError> {
    let (preview, _) = prepare_workspace(source, /*confirmed*/ false)?;
    existing_workspace(source, &preview.root, &preview.branch)
}

fn existing_workspace(
    source: &ReplaySource,
    root: &Path,
    branch: &str,
) -> Result<ReplayWorkspace, ReplayError> {
    let conflict = || ReplayError::WorkspaceExists(root.display().to_string());
    let metadata = std::fs::symlink_metadata(root).map_err(|_| conflict())?;
    if !metadata.file_type().is_dir() {
        return Err(conflict());
    }

    let canonical_root = std::fs::canonicalize(root).map_err(|_| conflict())?;
    if canonical_root != root {
        return Err(conflict());
    }

    let actual_root =
        git_text(root, &["rev-parse", "--show-toplevel"], 16 * 1024).map_err(|_| conflict())?;
    let actual_root = std::fs::canonicalize(actual_root.trim()).map_err(|_| conflict())?;
    if actual_root != canonical_root {
        return Err(conflict());
    }

    let actual_branch = git_text(root, &["symbolic-ref", "--quiet", "--short", "HEAD"], 1024)
        .map_err(|_| conflict())?;
    if actual_branch.trim() != branch {
        return Err(conflict());
    }

    let actual_head = git_text(root, &["rev-parse", "HEAD"], 1024).map_err(|_| conflict())?;
    if GitObjectId::parse(actual_head.trim()).map_err(|_| conflict())? != source.base_commit {
        return Err(conflict());
    }

    let common =
        git_text(root, &["rev-parse", "--git-common-dir"], 16 * 1024).map_err(|_| conflict())?;
    let common = PathBuf::from(common.trim());
    let common = if common.is_absolute() {
        common
    } else {
        root.join(common)
    };
    let common = std::fs::canonicalize(common).map_err(|_| conflict())?;
    if common != source.repository.common_directory {
        return Err(conflict());
    }

    let status = git_text(
        root,
        &["status", "--porcelain=v1", "--untracked-files=all"],
        16 * 1024,
    )
    .map_err(|_| conflict())?;
    if !status.trim().is_empty() {
        return Err(ReplayError::WorkspaceExists(format!(
            "{} contains saved or untracked review changes",
            root.display(),
        )));
    }

    Ok(ReplayWorkspace {
        root: canonical_root,
        branch: branch.to_string(),
        base_commit: source.base_commit.clone(),
        created_by_replay: false,
    })
}

pub(super) fn validate_relative_path(path: &Path) -> Result<(), ReplayError> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || path.components().any(
            |component| matches!(component, Component::Normal(name) if name == OsStr::new(".git")),
        )
    {
        return Err(ReplayError::UnsafePath(path.display().to_string()));
    }
    Ok(())
}

fn parse_remote(remote: &str) -> Result<(String, String, String), ReplayError> {
    let (host, path) = if let Ok(url) = Url::parse(remote) {
        let host = url
            .host_str()
            .ok_or_else(|| ReplayError::RepositoryMissing("origin has no host".to_string()))?;
        (
            host.to_ascii_lowercase(),
            url.path().trim_start_matches('/').to_string(),
        )
    } else if let Some((authority, path)) = remote.split_once(':') {
        let host = authority.rsplit('@').next().unwrap_or(authority);
        (host.to_ascii_lowercase(), path.to_string())
    } else {
        return Err(ReplayError::RepositoryMissing(
            "origin is not a supported GitHub remote".to_string(),
        ));
    };
    let path = path.strip_suffix(".git").unwrap_or(&path);
    let mut segments = path.split('/');
    let owner = segments.next().unwrap_or_default();
    let name = segments.next().unwrap_or_default();
    if segments.next().is_some()
        || !safe_repository_component(owner)
        || !safe_repository_component(name)
    {
        return Err(ReplayError::RepositoryMissing(
            "origin does not identify one GitHub repository".to_string(),
        ));
    }
    Ok((host, owner.to_string(), name.to_string()))
}

fn safe_repository_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn validate_git_reference(reference: &str) -> Result<(), ReplayError> {
    if reference.is_empty()
        || reference.starts_with('-')
        || reference.starts_with('/')
        || reference.ends_with('/')
        || reference.ends_with(".lock")
        || reference.contains("..")
        || reference.contains("@{")
        || reference.contains("//")
        || !reference
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.'))
    {
        return Err(ReplayError::InvalidMetadata(
            "unsafe Git reference".to_string(),
        ));
    }
    Ok(())
}

fn git_object(root: &Path, reference: &str) -> Result<GitObjectId, ReplayError> {
    if reference.is_empty() || reference.starts_with('-') {
        return Err(ReplayError::InvalidObject(reference.to_string()));
    }
    let object = git_text(
        root,
        &["rev-parse", "--verify", "--end-of-options", reference],
        1024,
    )?;
    GitObjectId::parse(object.trim())
}

fn has_git_commit(root: &Path, object: &GitObjectId) -> Result<bool, ReplayError> {
    let mut command = replay_git_command(root);
    command
        .args(["cat-file", "-e"])
        .arg(format!("{}^{{commit}}", object.as_str()));
    run_command_status(&mut command).map(|status| status.success())
}

/// Build an isolated Git command without depending on a repository fsmonitor daemon.
///
/// Git propagates command-line configuration to its child processes, including
/// the `git reset` used by `git worktree add`. Keeping this override local to
/// Replay avoids hanging on an unavailable monitor without changing Git config.
fn replay_git_command(root: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .args(["-c", "core.fsmonitor=false"]);
    command
}

fn git_text(root: &Path, args: &[&str], limit: usize) -> Result<String, ReplayError> {
    let mut command = replay_git_command(root);
    command.args(args);
    let output = run_command(&mut command, limit)?;
    String::from_utf8(output).map_err(|_| ReplayError::CommandFailed {
        program: "git".to_string(),
        message: "command output was not UTF-8".to_string(),
    })
}

pub(super) fn run_command(command: &mut Command, limit: usize) -> Result<Vec<u8>, ReplayError> {
    run_command_with_deadline(command, limit, MAX_COMMAND_DURATION)
}

fn run_command_with_deadline(
    command: &mut Command,
    limit: usize,
    timeout: Duration,
) -> Result<Vec<u8>, ReplayError> {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = run_bounded_command(command, None, limit, timeout)
        .map_err(ReplayCommandFailure::into_error)?;
    bounded_command_output(program, output, limit)
}

fn run_command_status(command: &mut Command) -> Result<ExitStatus, ReplayError> {
    run_bounded_command(
        command,
        None,
        MAX_COMMAND_DIAGNOSTIC_BYTES,
        MAX_COMMAND_DURATION,
    )
    .map(|output| output.status)
    .map_err(ReplayCommandFailure::into_error)
}

/// Whether a provider command failed before or after its request could be sent.
#[derive(Debug)]
pub(super) enum ReplayCommandFailure {
    /// The executable never received a complete request body.
    NotStarted(ReplayError),
    /// A complete request was provided and GitHub may have observed it.
    PossiblyExecuted(ReplayError),
}

impl ReplayCommandFailure {
    fn into_error(self) -> ReplayError {
        match self {
            Self::NotStarted(error) | Self::PossiblyExecuted(error) => error,
        }
    }
}

/// Runs one bounded command with an explicitly supplied, noninteractive body.
pub(super) fn run_command_with_input(
    command: &mut Command,
    input: &[u8],
    limit: usize,
) -> Result<Vec<u8>, ReplayCommandFailure> {
    if input.len() > limit {
        return Err(ReplayCommandFailure::NotStarted(
            ReplayError::LimitExceeded {
                kind: "GitHub review submission",
                limit,
            },
        ));
    }

    let program = command.get_program().to_string_lossy().into_owned();
    let output = run_bounded_command(command, Some(input), limit, MAX_COMMAND_DURATION)?;
    bounded_command_output(program, output, limit).map_err(ReplayCommandFailure::PossiblyExecuted)
}

fn run_bounded_command(
    command: &mut Command,
    input: Option<&[u8]>,
    limit: usize,
    timeout: Duration,
) -> Result<Output, ReplayCommandFailure> {
    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        .spawn()
        .map_err(|error| {
            ReplayCommandFailure::NotStarted(ReplayError::CommandFailed {
                program: program.clone(),
                message: error.to_string(),
            })
        })?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(ReplayCommandFailure::NotStarted(
            ReplayError::CommandFailed {
                program,
                message: "could not capture the bounded command output".to_string(),
            },
        ));
    };
    let Some(stderr) = child.stderr.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(ReplayCommandFailure::NotStarted(
            ReplayError::CommandFailed {
                program,
                message: "could not capture the bounded command diagnostic".to_string(),
            },
        ));
    };

    let overflowed = Arc::new(AtomicBool::new(false));
    let stdout_overflowed = Arc::clone(&overflowed);
    let stdout_reader =
        std::thread::spawn(move || capture_command_output(stdout, limit, Some(stdout_overflowed)));
    let stderr_reader = std::thread::spawn(move || {
        capture_command_output(stderr, MAX_COMMAND_DIAGNOSTIC_BYTES, None)
    });

    let request_written = Arc::new(AtomicBool::new(input.is_none()));
    let input_writer = if let Some(input) = input {
        let Some(mut stdin) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ReplayCommandFailure::NotStarted(
                ReplayError::CommandFailed {
                    program,
                    message: "could not open the GitHub review request body".to_string(),
                },
            ));
        };
        let contents = input.to_vec();
        let written = Arc::clone(&request_written);
        Some(std::thread::spawn(move || {
            let result = stdin.write_all(&contents);
            if result.is_ok() {
                written.store(true, Ordering::Release);
            }
            result
        }))
    } else {
        None
    };

    let started_at = Instant::now();
    let status = loop {
        if overflowed.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ReplayCommandFailure::PossiblyExecuted(
                ReplayError::LimitExceeded {
                    kind: "source command output",
                    limit,
                },
            ));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started_at.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let error = ReplayError::CommandFailed {
                    program,
                    message: format!(
                        "command timed out after {} ms and was cancelled",
                        timeout.as_millis()
                    ),
                };
                return Err(if request_written.load(Ordering::Acquire) {
                    ReplayCommandFailure::PossiblyExecuted(error)
                } else {
                    ReplayCommandFailure::NotStarted(error)
                });
            }
            Ok(None) => std::thread::sleep(COMMAND_POLL_INTERVAL),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ReplayCommandFailure::PossiblyExecuted(
                    ReplayError::CommandFailed {
                        program,
                        message: error.to_string(),
                    },
                ));
            }
        }
    };

    if let Some(writer) = input_writer {
        match writer.join() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                return Err(ReplayCommandFailure::NotStarted(
                    ReplayError::CommandFailed {
                        program,
                        message: format!("could not send the GitHub review request body: {error}"),
                    },
                ));
            }
            Err(_) => {
                return Err(ReplayCommandFailure::NotStarted(
                    ReplayError::CommandFailed {
                        program,
                        message: "the GitHub review request writer stopped unexpectedly"
                            .to_string(),
                    },
                ));
            }
        }
    }

    let stdout = match stdout_reader.join() {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            return Err(ReplayCommandFailure::PossiblyExecuted(
                ReplayError::CommandFailed {
                    program,
                    message: format!("could not read the bounded command output: {error}"),
                },
            ));
        }
        Err(_) => {
            return Err(ReplayCommandFailure::PossiblyExecuted(
                ReplayError::CommandFailed {
                    program,
                    message: "the bounded command output reader stopped unexpectedly".to_string(),
                },
            ));
        }
    };
    let stderr = match stderr_reader.join() {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            return Err(ReplayCommandFailure::PossiblyExecuted(
                ReplayError::CommandFailed {
                    program,
                    message: format!("could not read the bounded command diagnostic: {error}"),
                },
            ));
        }
        Err(_) => {
            return Err(ReplayCommandFailure::PossiblyExecuted(
                ReplayError::CommandFailed {
                    program,
                    message: "the bounded command diagnostic reader stopped unexpectedly"
                        .to_string(),
                },
            ));
        }
    };

    if overflowed.load(Ordering::Acquire) {
        return Err(ReplayCommandFailure::PossiblyExecuted(
            ReplayError::LimitExceeded {
                kind: "source command output",
                limit,
            },
        ));
    }

    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn capture_command_output(
    mut reader: impl std::io::Read,
    limit: usize,
    overflowed: Option<Arc<AtomicBool>>,
) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let count = reader.read(&mut chunk)?;
        if count == 0 {
            return Ok(output);
        }
        let retained = count.min(limit.saturating_sub(output.len()));
        output.extend_from_slice(&chunk[..retained]);
        if retained < count {
            if let Some(overflowed) = overflowed.as_ref() {
                overflowed.store(true, Ordering::Release);
                return Ok(output);
            }
        }
    }
}

fn bounded_command_output(
    program: String,
    output: Output,
    limit: usize,
) -> Result<Vec<u8>, ReplayError> {
    if output.stdout.len() > limit {
        return Err(ReplayError::LimitExceeded {
            kind: "source command output",
            limit,
        });
    }
    if !output.status.success() {
        let diagnostic_length = output.stderr.len().min(MAX_COMMAND_DIAGNOSTIC_BYTES);
        let diagnostic = String::from_utf8_lossy(&output.stderr[..diagnostic_length]);
        return Err(ReplayError::CommandFailed {
            program,
            message: redact_command_diagnostic(diagnostic.trim()),
        });
    }
    Ok(output.stdout)
}

fn redact_command_diagnostic(diagnostic: &str) -> String {
    let mut redact_next = false;
    diagnostic
        .split_whitespace()
        .map(|word| {
            if std::mem::take(&mut redact_next) {
                return "[REDACTED]".to_string();
            }
            if word.eq_ignore_ascii_case("bearer") {
                redact_next = true;
                return word.to_string();
            }
            if ["ghp_", "github_pat_", "gho_", "ghu_", "ghs_", "ghr_"]
                .iter()
                .any(|prefix| word.starts_with(prefix))
            {
                return "[REDACTED]".to_string();
            }
            if let Some((scheme, rest)) = word.split_once("://") {
                if matches!(scheme, "http" | "https") {
                    if let Some((credentials, host)) = rest.split_once('@') {
                        if !credentials.contains('/') {
                            return format!("{scheme}://[REDACTED]@{host}");
                        }
                    }
                }
            }
            word.to_string()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn hung_source_command_is_cancelled_at_its_bounded_deadline() {
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 5"]);
        let started = Instant::now();

        let error =
            run_command_with_deadline(&mut command, /*limit*/ 1024, Duration::from_millis(30))
                .expect_err("a hung source command must be cancelled");

        assert!(matches!(
            error,
            ReplayError::CommandFailed { message, .. }
                if message.contains("timed out") && message.contains("cancelled")
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn source_command_output_is_stopped_before_exceeding_its_memory_limit() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf 123456789"]);

        let error =
            run_command_with_deadline(&mut command, /*limit*/ 4, Duration::from_secs(1))
                .expect_err("source output larger than its configured bound must be rejected");

        assert!(matches!(
            error,
            ReplayError::LimitExceeded {
                kind: "source command output",
                limit: 4,
            }
        ));
    }

    #[test]
    fn missing_provider_executable_is_reported_before_any_request_can_be_sent() {
        let mut command = Command::new("red-replay-provider-command-does-not-exist");

        let error = run_command_with_input(&mut command, br#"{"event":"COMMENT"}"#, 1024)
            .expect_err("an unavailable executable cannot send a GitHub request");

        assert!(matches!(
            error,
            ReplayCommandFailure::NotStarted(ReplayError::CommandFailed { .. })
        ));
    }

    #[test]
    fn command_diagnostics_redact_github_tokens_bearer_headers_and_url_credentials() {
        let diagnostic = redact_command_diagnostic(
            "fatal ghp_sensitive Bearer another-secret https://token@example.com/private",
        );

        assert!(!diagnostic.contains("ghp_sensitive"));
        assert!(!diagnostic.contains("another-secret"));
        assert!(!diagnostic.contains("token@example.com"));
        assert!(diagnostic.contains("https://[REDACTED]@example.com/private"));
    }

    fn sample_capability_request() -> ReplayPullRequest {
        ReplayPullRequest {
            host: "github.com".to_string(),
            repository_owner: "example".to_string(),
            repository_name: "replay-fixture".to_string(),
            number: 482,
            url: "https://github.com/example/replay-fixture/pull/482".to_string(),
            author: Some("original-author".to_string()),
            base_ref: "master".to_string(),
            base_ref_tip: GitObjectId::parse(&"a".repeat(40)).unwrap(),
            head_repository_owner: "example".to_string(),
            head_repository_name: "replay-fixture".to_string(),
            head_ref: "feature/replay".to_string(),
            head_commit: GitObjectId::parse(&"b".repeat(40)).unwrap(),
            cross_repository: false,
            capabilities: ReplayGitHubCapabilities::default(),
            captured_at_ms: 0,
        }
    }

    fn sample_capability_response() -> serde_json::Value {
        serde_json::json!({
            "data": {
                "viewer": { "login": "original-author" },
                "repository": {
                    "nameWithOwner": "example/replay-fixture",
                    "pullRequest": {
                        "number": 482,
                        "author": { "login": "original-author" },
                        "headRefName": "feature/replay",
                        "headRefOid": "b".repeat(40),
                        "headRepository": {
                            "nameWithOwner": "example/replay-fixture",
                            "viewerPermission": "WRITE",
                        },
                    },
                },
            },
        })
    }

    #[test]
    fn authenticates_capabilities_only_for_the_exact_original_pull_request_head() {
        let request = sample_capability_request();
        let response = serde_json::to_vec(&sample_capability_response()).unwrap();

        let capabilities = parse_pull_request_capabilities(&request, &response)
            .expect("verify the viewer against the immutable original PR head");

        assert_eq!(capabilities.viewer.as_deref(), Some("original-author"));
        assert_eq!(
            capabilities.head_permission,
            ReplayRepositoryPermission::Write
        );
    }

    #[test]
    fn refuses_capabilities_after_the_original_pr_head_moves() {
        let request = sample_capability_request();
        let mut response = sample_capability_response();
        response["data"]["repository"]["pullRequest"]["headRefOid"] =
            serde_json::Value::String("c".repeat(40));

        assert!(matches!(
            parse_pull_request_capabilities(&request, &serde_json::to_vec(&response).unwrap()),
            Err(ReplayError::SourceRefMoved),
        ));
    }

    #[test]
    fn refuses_capabilities_from_a_different_original_head_repository() {
        let request = sample_capability_request();
        let mut response = sample_capability_response();
        response["data"]["repository"]["pullRequest"]["headRepository"]["nameWithOwner"] =
            serde_json::Value::String("another-owner/replay-fixture".to_string());

        assert!(matches!(
            parse_pull_request_capabilities(&request, &serde_json::to_vec(&response).unwrap()),
            Err(ReplayError::RepositoryMismatch),
        ));
    }

    #[test]
    fn refuses_capabilities_when_the_original_pr_author_changes() {
        let request = sample_capability_request();
        let mut response = sample_capability_response();
        response["data"]["repository"]["pullRequest"]["author"]["login"] =
            serde_json::Value::String("another-author".to_string());

        assert!(matches!(
            parse_pull_request_capabilities(&request, &serde_json::to_vec(&response).unwrap()),
            Err(ReplayError::InvalidMetadata(_)),
        ));
    }

    #[test]
    fn existing_pull_request_snapshots_default_to_read_only_capabilities() {
        let request = sample_capability_request();
        let mut snapshot = serde_json::to_value(&request).unwrap();
        snapshot
            .as_object_mut()
            .expect("pull request snapshots are structured")
            .remove("capabilities");

        let recovered: ReplayPullRequest = serde_json::from_value(snapshot)
            .expect("preserve pull requests captured before role detection");

        assert_eq!(recovered.capabilities, ReplayGitHubCapabilities::default());
    }

    #[test]
    fn github_source_kind_serializes_canonically_and_accepts_the_legacy_spelling() {
        assert_eq!(
            serde_json::to_value(ReplaySourceKind::GitHubPullRequest).unwrap(),
            serde_json::json!("github_pull_request"),
        );
        assert_eq!(
            serde_json::from_value::<ReplaySourceKind>(serde_json::json!("git_hub_pull_request"))
                .unwrap(),
            ReplaySourceKind::GitHubPullRequest,
        );
    }

    fn fixture_git(root: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(root)
            .args(arguments)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("Git is available for the local replay repository fixture");
        assert!(
            output.status.success(),
            "fixture Git command {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr),
        );
        String::from_utf8(output.stdout)
            .expect("fixture Git output is UTF-8")
            .trim()
            .to_string()
    }

    fn diverged_local_repository() -> (tempfile::TempDir, GitObjectId, GitObjectId) {
        let directory = tempfile::tempdir().expect("isolated local replay Git fixture");
        let root = directory.path();
        fixture_git(root, &["init", "--initial-branch=master"]);
        fixture_git(root, &["config", "core.autocrlf", "false"]);
        fixture_git(root, &["config", "user.name", "Replay Fixture"]);
        fixture_git(root, &["config", "user.email", "replay@example.test"]);
        fixture_git(
            root,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/example/replay-fixture.git",
            ],
        );
        std::fs::create_dir(root.join("src")).expect("fixture source directory");
        std::fs::write(root.join("src/token.rs"), "pub fn token() -> usize { 1 }\n")
            .expect("fixture merge-base source");
        fixture_git(root, &["add", "src/token.rs"]);
        fixture_git(
            root,
            &["commit", "--quiet", "-m", "create replay merge base"],
        );
        let merge_base = GitObjectId::parse(&fixture_git(root, &["rev-parse", "HEAD"]))
            .expect("immutable fixture merge base");

        fixture_git(root, &["checkout", "--quiet", "-b", "feature/replay"]);
        std::fs::write(root.join("src/token.rs"), "pub fn token() -> usize { 2 }\n")
            .expect("fixture original feature change");
        fixture_git(root, &["add", "src/token.rs"]);
        fixture_git(root, &["commit", "--quiet", "-m", "change feature token"]);
        let feature_head = GitObjectId::parse(&fixture_git(root, &["rev-parse", "HEAD"]))
            .expect("immutable fixture feature head");

        fixture_git(root, &["checkout", "--quiet", "master"]);
        std::fs::write(root.join("unrelated.txt"), "new work on master\n")
            .expect("fixture divergent default branch");
        fixture_git(root, &["add", "unrelated.txt"]);
        fixture_git(root, &["commit", "--quiet", "-m", "advance default branch"]);

        (directory, merge_base, feature_head)
    }

    fn reusable_workspace_source() -> (tempfile::TempDir, ReplaySource) {
        let directory = tempfile::tempdir().expect("isolated Replay worktree fixture");
        let root = directory.path().join("replay-fixture");
        std::fs::create_dir(&root).expect("isolated repository directory");
        fixture_git(&root, &["init", "--initial-branch=master"]);
        fixture_git(&root, &["config", "core.autocrlf", "false"]);
        fixture_git(&root, &["config", "user.name", "Replay Fixture"]);
        fixture_git(&root, &["config", "user.email", "replay@example.test"]);
        fixture_git(
            &root,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/example/replay-fixture.git",
            ],
        );
        std::fs::create_dir(root.join("src")).expect("fixture source directory");
        std::fs::write(root.join("src/token.rs"), "pub fn token() -> usize { 1 }\n")
            .expect("fixture merge-base source");
        fixture_git(&root, &["add", "src/token.rs"]);
        fixture_git(&root, &["commit", "--quiet", "-m", "create replay base"]);
        fixture_git(&root, &["checkout", "--quiet", "-b", "feature/replay"]);
        std::fs::write(root.join("src/token.rs"), "pub fn token() -> usize { 2 }\n")
            .expect("fixture original feature change");
        fixture_git(&root, &["add", "src/token.rs"]);
        fixture_git(&root, &["commit", "--quiet", "-m", "change replay token"]);
        fixture_git(&root, &["checkout", "--quiet", "master"]);

        let source = resolve_local_branch_source(
            &root,
            "feature/replay",
            Some("master"),
            ReplayLimits::default(),
        )
        .expect("resolve the isolated original feature");
        (directory, source.source)
    }

    fn reusable_author_workspace_source() -> (tempfile::TempDir, ReplaySource) {
        let (directory, mut source) = reusable_workspace_source();
        source.kind = ReplaySourceKind::GitHubPullRequest;
        source.pull_request = Some(ReplayPullRequest {
            host: "github.com".to_string(),
            repository_owner: "example".to_string(),
            repository_name: "replay-fixture".to_string(),
            number: 482,
            url: "https://github.com/example/replay-fixture/pull/482".to_string(),
            author: Some("original-author".to_string()),
            base_ref: "master".to_string(),
            base_ref_tip: source.base_commit.clone(),
            head_repository_owner: "example".to_string(),
            head_repository_name: "replay-fixture".to_string(),
            head_ref: "feature/replay".to_string(),
            head_commit: source.target_commit.clone(),
            cross_repository: false,
            capabilities: ReplayGitHubCapabilities {
                viewer: Some("original-author".to_string()),
                head_permission: ReplayRepositoryPermission::Write,
                warning: None,
            },
            captured_at_ms: 0,
        });
        (directory, source)
    }

    #[test]
    fn previews_original_author_head_without_creating_a_branch_or_worktree() {
        let (_directory, source) = reusable_author_workspace_source();

        let (preview, workspace) = prepare_author_workspace(&source, /*confirmed*/ false)
            .expect("preview the separately verified original PR head");

        assert!(workspace.is_none());
        assert!(!preview.existing);
        assert!(!preview.root.exists());
        assert_eq!(preview.head_commit, source.target_commit);
        assert_ne!(preview.head_commit, source.base_commit);
        assert_eq!(preview.viewer, "original-author");
        assert_eq!(preview.head_repository, "github.com/example/replay-fixture");
        assert_eq!(preview.head_ref, "feature/replay");
        assert!(preview.branch.starts_with("replay/author/pr-482-"));
        assert!(fixture_git(
            &source.repository.root,
            &["branch", "--list", &preview.branch],
        )
        .is_empty());
    }

    #[test]
    fn original_author_confirmation_digest_binds_head_fork_branch_and_path() {
        let (_directory, source) = reusable_author_workspace_source();
        let (preview, _) = prepare_author_workspace(&source, /*confirmed*/ false)
            .expect("preview the exact original author worktree");
        let expected = preview.digest();

        let mut different_head = preview.clone();
        different_head.head_commit = source.base_commit.clone();
        assert_ne!(different_head.digest(), expected);

        let mut different_fork = preview.clone();
        different_fork.head_repository = "github.com/another-author/fork".to_string();
        assert_ne!(different_fork.digest(), expected);

        let mut different_branch = preview.clone();
        different_branch.head_ref = "feature/different".to_string();
        assert_ne!(different_branch.digest(), expected);

        let mut different_path = preview;
        different_path.root = different_path.repository_root.join("unexpected-worktree");
        assert_ne!(different_path.digest(), expected);
    }

    #[test]
    fn opens_original_author_head_without_changing_the_learning_scratch() {
        let (_directory, source) = reusable_author_workspace_source();
        let (_, scratch) = prepare_workspace(&source, /*confirmed*/ true)
            .expect("create the independently confirmed merge-base learning scratch");
        let scratch = scratch.expect("the confirmed learning worktree exists");

        let (preview, workspace) = prepare_author_workspace(&source, /*confirmed*/ true)
            .expect("open the independently confirmed original author head");
        let workspace = workspace.expect("the real original-author worktree exists");

        assert!(workspace.created_by_replay);
        assert_eq!(workspace.root, preview.root);
        assert_ne!(workspace.root, scratch.root);
        assert_ne!(workspace.branch, scratch.branch);
        assert_eq!(
            fixture_git(&workspace.root, &["rev-parse", "HEAD"]),
            source.target_commit.as_str(),
        );
        assert_eq!(
            fixture_git(&scratch.root, &["rev-parse", "HEAD"]),
            source.base_commit.as_str(),
        );
        assert_eq!(
            std::fs::read_to_string(workspace.root.join("src/token.rs"))
                .expect("read the real original PR source"),
            "pub fn token() -> usize { 2 }\n",
        );
        assert_eq!(
            std::fs::read_to_string(scratch.root.join("src/token.rs"))
                .expect("read the unchanged merge-base learning source"),
            "pub fn token() -> usize { 1 }\n",
        );
        assert_eq!(
            fixture_git(&source.repository.root, &["branch", "--show-current"]),
            "master",
        );
    }

    #[test]
    fn original_author_branch_can_already_be_checked_out_elsewhere() {
        let (_directory, source) = reusable_author_workspace_source();
        fixture_git(
            &source.repository.root,
            &["checkout", "--quiet", "feature/replay"],
        );

        let (_, workspace) = prepare_author_workspace(&source, /*confirmed*/ true)
            .expect("create a separate author branch while the real PR branch stays checked out");
        let workspace = workspace.expect("the separately named author worktree exists");

        assert_eq!(
            fixture_git(&source.repository.root, &["branch", "--show-current"]),
            "feature/replay",
        );
        assert_eq!(
            fixture_git(&workspace.root, &["branch", "--show-current"]),
            workspace.branch,
        );
        assert_ne!(workspace.branch, "feature/replay");
    }

    #[test]
    fn resolves_only_regular_original_author_files_inside_the_exact_worktree() {
        let (_directory, source) = reusable_author_workspace_source();
        let (_, workspace) = prepare_author_workspace(&source, /*confirmed*/ true)
            .expect("create the exact original author worktree");
        let workspace = workspace.expect("the confirmed author worktree exists");

        let path = workspace
            .source_path(Path::new("src/token.rs"))
            .expect("resolve a genuine regular original PR source file");

        assert_eq!(path, workspace.root.join("src/token.rs"));
        assert!(matches!(
            workspace.source_path(Path::new("../src/token.rs")),
            Err(ReplayError::UnsafePath(_)),
        ));
        assert!(matches!(
            workspace.source_path(Path::new("src/missing.rs")),
            Err(ReplayError::NotFound {
                kind: "original PR source file",
                ..
            }),
        ));
    }

    #[test]
    fn reopening_original_author_worktree_preserves_dirty_and_untracked_files() {
        let (_directory, source) = reusable_author_workspace_source();
        let (_, created) = prepare_author_workspace(&source, /*confirmed*/ true)
            .expect("create the exact original author worktree");
        let created = created.expect("the confirmed author worktree exists");
        let changed = "pub fn token() -> usize { 3 }\n";
        std::fs::write(created.root.join("src/token.rs"), changed)
            .expect("preserve the author's real in-progress code");
        std::fs::write(
            created.root.join("private-follow-up.txt"),
            "keep this author edit\n",
        )
        .expect("preserve the author's real untracked work");

        let (preview, reopened) = prepare_author_workspace(&source, /*confirmed*/ true)
            .expect("reopen the independently verified dirty author worktree");
        let reopened = reopened.expect("reopen rather than replace the original author worktree");

        assert!(preview.existing);
        assert!(!reopened.created_by_replay);
        assert_eq!(reopened.root, created.root);
        assert_eq!(
            std::fs::read_to_string(reopened.root.join("src/token.rs")).unwrap(),
            changed,
        );
        assert_eq!(
            std::fs::read_to_string(reopened.root.join("private-follow-up.txt")).unwrap(),
            "keep this author edit\n",
        );
    }

    #[test]
    fn reopening_original_author_worktree_accepts_committed_head_descendants() {
        let (_directory, source) = reusable_author_workspace_source();
        let (_, created) = prepare_author_workspace(&source, /*confirmed*/ true)
            .expect("create the exact original author worktree");
        let created = created.expect("the confirmed author worktree exists");
        std::fs::write(
            created.root.join("src/token.rs"),
            "pub fn token() -> usize { 3 }\n",
        )
        .expect("prepare an explicitly authored local follow-up");
        fixture_git(&created.root, &["add", "src/token.rs"]);
        fixture_git(
            &created.root,
            &["commit", "--quiet", "-m", "add original author follow-up"],
        );
        let descendant = fixture_git(&created.root, &["rev-parse", "HEAD"]);

        let (_, reopened) = prepare_author_workspace(&source, /*confirmed*/ true)
            .expect("preserve committed descendants of the original pinned PR head");
        let reopened = reopened.expect("reopen the author's existing follow-up");

        assert!(!reopened.created_by_replay);
        assert_eq!(
            fixture_git(&reopened.root, &["rev-parse", "HEAD"]),
            descendant
        );
        assert_eq!(reopened.head_commit, source.target_commit);
    }

    #[test]
    fn original_author_preview_retains_the_exact_fork_repository() {
        let (_directory, mut source) = reusable_author_workspace_source();
        let request = source.pull_request.as_mut().unwrap();
        request.head_repository_owner = "original-author".to_string();
        request.head_repository_name = "forked-replay".to_string();
        request.cross_repository = true;

        let (preview, _) = prepare_author_workspace(&source, /*confirmed*/ false)
            .expect("retain the original fork rather than infer the base origin");

        assert_eq!(
            preview.head_repository,
            "github.com/original-author/forked-replay",
        );
        assert_eq!(preview.head_ref, "feature/replay");
    }

    #[test]
    fn refuses_original_author_worktree_for_unverified_or_read_only_viewers() {
        let (_directory, source) = reusable_author_workspace_source();

        for (viewer, permission) in [
            (Some("another-reviewer"), ReplayRepositoryPermission::Write),
            (None, ReplayRepositoryPermission::Admin),
            (Some("original-author"), ReplayRepositoryPermission::Unknown),
            (Some("original-author"), ReplayRepositoryPermission::None),
            (Some("original-author"), ReplayRepositoryPermission::Read),
            (Some("original-author"), ReplayRepositoryPermission::Triage),
        ] {
            let mut unverified = source.clone();
            let capabilities = &mut unverified.pull_request.as_mut().unwrap().capabilities;
            capabilities.viewer = viewer.map(str::to_string);
            capabilities.head_permission = permission;

            assert!(matches!(
                prepare_author_workspace(&unverified, /*confirmed*/ false),
                Err(ReplayError::AuthorWorkspaceUnavailable(_)),
            ));
        }
    }

    #[test]
    fn refuses_original_author_worktree_for_a_local_or_moved_source() {
        let (_directory, mut source) = reusable_author_workspace_source();
        source.kind = ReplaySourceKind::LocalRevision;

        assert!(matches!(
            prepare_author_workspace(&source, /*confirmed*/ false),
            Err(ReplayError::AuthorWorkspaceUnavailable(_)),
        ));

        source.kind = ReplaySourceKind::GitHubPullRequest;
        source.pull_request.as_mut().unwrap().head_commit = source.base_commit.clone();

        assert!(matches!(
            prepare_author_workspace(&source, /*confirmed*/ false),
            Err(ReplayError::SourceRefMoved),
        ));
    }

    #[test]
    fn refuses_unavailable_original_head_without_creating_an_author_branch() {
        let (_directory, mut source) = reusable_author_workspace_source();
        let missing = GitObjectId::parse(&"c".repeat(40)).unwrap();
        source.target_commit = missing.clone();
        source.pull_request.as_mut().unwrap().head_commit = missing;
        let (preview, _) = prepare_author_workspace(&source, /*confirmed*/ false)
            .expect("preview the exact unavailable head without changing Git");

        assert!(matches!(
            prepare_author_workspace(&source, /*confirmed*/ true),
            Err(ReplayError::MissingObjects),
        ));
        assert!(!preview.root.exists());
        assert!(fixture_git(
            &source.repository.root,
            &["branch", "--list", &preview.branch],
        )
        .is_empty());
    }

    #[test]
    fn refuses_unrelated_existing_original_author_branch_without_resetting_it() {
        let (_directory, source) = reusable_author_workspace_source();
        let (preview, _) = prepare_author_workspace(&source, /*confirmed*/ false)
            .expect("preview the exact original PR author branch");
        fixture_git(
            &source.repository.root,
            &["branch", &preview.branch, source.base_commit.as_str()],
        );

        assert!(matches!(
            prepare_author_workspace(&source, /*confirmed*/ true),
            Err(ReplayError::WorkspaceExists(_)),
        ));
        assert!(!preview.root.exists());
        assert_eq!(
            fixture_git(
                &source.repository.root,
                &["rev-parse", &format!("refs/heads/{}", preview.branch)],
            ),
            source.base_commit.as_str(),
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinked_original_author_workspace_without_following_it() {
        let (directory, source) = reusable_author_workspace_source();
        let (preview, _) = prepare_author_workspace(&source, /*confirmed*/ false)
            .expect("preview the independently confined original author workspace");
        let unrelated = directory.path().join("unrelated-author-source");
        std::fs::create_dir(&unrelated).unwrap();
        std::fs::write(unrelated.join("keep.txt"), "leave this untouched\n").unwrap();
        std::os::unix::fs::symlink(&unrelated, &preview.root).unwrap();

        assert!(matches!(
            prepare_author_workspace(&source, /*confirmed*/ true),
            Err(ReplayError::WorkspaceExists(_)),
        ));
        assert_eq!(
            std::fs::read_to_string(unrelated.join("keep.txt")).unwrap(),
            "leave this untouched\n",
        );
    }

    #[test]
    fn creates_the_exact_canonical_sibling_workspace_on_every_platform() {
        let (_directory, source) = reusable_workspace_source();
        let expected_root = source
            .repository
            .root
            .parent()
            .expect("the original repository has a durable parent")
            .join(format!(
                "{}.replay-revision-{}",
                source.repository.name,
                source.target_commit.short(),
            ));

        let (preview, workspace) = prepare_workspace(&source, /*confirmed*/ true)
            .expect("create the confirmed sibling workspace on every platform");
        let workspace = workspace.expect("a confirmed workspace is created");
        let git_root = fixture_git(&workspace.root, &["rev-parse", "--show-toplevel"]);

        assert_eq!(preview.root, expected_root);
        assert_eq!(workspace.root, expected_root);
        assert_eq!(
            std::fs::canonicalize(git_root).expect("canonicalize the Git worktree root"),
            expected_root,
        );
    }

    #[test]
    fn resumes_only_the_exact_clean_original_replay_worktree() {
        let (_directory, source) = reusable_workspace_source();
        let (preview, created) = prepare_workspace(&source, /*confirmed*/ true)
            .expect("create the explicitly confirmed original scratch worktree");
        let created = created.expect("confirmed Replay creates its scratch worktree");
        assert!(created.created_by_replay);

        let (second_preview, resumed) = prepare_workspace(&source, /*confirmed*/ true)
            .expect("resume the exact pinned original scratch worktree");
        let resumed = resumed.expect("confirmed Replay reopens its verified worktree");

        assert_eq!(second_preview, preview);
        assert_eq!(resumed.root, created.root);
        assert_eq!(resumed.branch, created.branch);
        assert_eq!(resumed.base_commit, source.base_commit);
        assert!(!resumed.created_by_replay);
        assert_eq!(
            fixture_git(&resumed.root, &["branch", "--show-current"]),
            resumed.branch,
        );
    }

    #[test]
    fn reopens_the_verified_original_workspace_without_creating_a_worktree() {
        let (_directory, source) = reusable_workspace_source();
        let (_, created) = prepare_workspace(&source, /*confirmed*/ true)
            .expect("create the explicitly confirmed original scratch worktree");
        let created = created.expect("confirmed Replay creates its original worktree");

        let reopened = reopen_existing_workspace(&source)
            .expect("reopen the same independently verified original worktree");

        assert_eq!(reopened.root, created.root);
        assert_eq!(reopened.branch, created.branch);
        assert_eq!(reopened.base_commit, source.base_commit);
        assert!(!reopened.created_by_replay);
    }

    #[test]
    fn reopening_a_missing_workspace_never_creates_a_branch_or_directory() {
        let (_directory, source) = reusable_workspace_source();
        let (preview, _) = prepare_workspace(&source, /*confirmed*/ false)
            .expect("compute the original scratch preview without side effects");

        assert!(reopen_existing_workspace(&source).is_err());
        assert!(!preview.root.exists());
        assert!(fixture_git(
            &source.repository.root,
            &["branch", "--list", &preview.branch],
        )
        .is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn replay_worktree_creation_and_resume_do_not_invoke_repository_fsmonitor() {
        use std::os::unix::fs::PermissionsExt;

        let (directory, source) = reusable_workspace_source();
        let hook = directory.path().join("replay-test-fsmonitor");
        let marker = directory.path().join("replay-fsmonitor-invoked");
        std::fs::write(
            &hook,
            format!(
                "#!/bin/sh\nprintf invoked > '{}'\nprintf 'replay-test\\0'\n",
                marker.display(),
            ),
        )
        .expect("write the isolated repository filesystem-monitor fixture");
        let mut permissions = std::fs::metadata(&hook)
            .expect("read the filesystem-monitor fixture permissions")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&hook, permissions)
            .expect("make the isolated filesystem-monitor fixture executable");
        fixture_git(
            &source.repository.root,
            &[
                "config",
                "core.fsmonitor",
                hook.to_str().expect("UTF-8 filesystem-monitor fixture"),
            ],
        );

        let (_, created) = prepare_workspace(&source, /*confirmed*/ true)
            .expect("create a Replay worktree without calling repository fsmonitor");
        let created = created.expect("confirmed Replay creates its scratch worktree");

        assert!(
            !marker.exists(),
            "Replay worktree checkout must not wait on the repository filesystem monitor",
        );
        assert_eq!(
            std::fs::read_to_string(created.root.join("src/token.rs"))
                .expect("read the fully checked-out merge-base source"),
            "pub fn token() -> usize { 1 }\n",
        );

        let (_, resumed) = prepare_workspace(&source, /*confirmed*/ true)
            .expect("resume a Replay worktree without calling repository fsmonitor");

        assert_eq!(
            resumed
                .expect("resume the exact clean scratch worktree")
                .root,
            created.root,
        );
        assert!(
            !marker.exists(),
            "Replay worktree verification must not wait on repository fsmonitor",
        );
        assert_eq!(
            fixture_git(
                &source.repository.root,
                &["config", "--get", "core.fsmonitor"]
            ),
            hook.to_string_lossy(),
            "Replay must never modify repository filesystem-monitor configuration",
        );
    }

    #[test]
    fn resumes_pinned_replay_branch_after_an_interrupted_worktree_checkout() {
        let (_directory, source) = reusable_workspace_source();
        let (preview, _) = prepare_workspace(&source, /*confirmed*/ false)
            .expect("preview the original durable scratch worktree");
        fixture_git(
            &source.repository.root,
            &["branch", &preview.branch, source.base_commit.as_str()],
        );
        assert!(
            !preview.root.exists(),
            "an interrupted checkout leaves its pinned branch without a worktree",
        );

        let (resumed_preview, workspace) = prepare_workspace(&source, /*confirmed*/ true)
            .expect("resume the exact original branch after an interrupted checkout");
        let workspace = workspace.expect("restore the confirmed Replay worktree");

        assert_eq!(resumed_preview, preview);
        assert_eq!(workspace.root, preview.root);
        assert_eq!(workspace.branch, preview.branch);
        assert_eq!(workspace.base_commit, source.base_commit);
        assert_eq!(
            fixture_git(&workspace.root, &["rev-parse", "HEAD"]),
            source.base_commit.as_str(),
        );
        assert_eq!(
            std::fs::read_to_string(workspace.root.join("src/token.rs"))
                .expect("read the completely restored merge-base source"),
            "pub fn token() -> usize { 1 }\n",
        );
    }

    #[test]
    fn refuses_an_existing_replay_branch_at_a_different_commit() {
        let (_directory, source) = reusable_workspace_source();
        let (preview, _) = prepare_workspace(&source, /*confirmed*/ false)
            .expect("preview the expected Replay branch");
        fixture_git(
            &source.repository.root,
            &["branch", &preview.branch, source.target_commit.as_str()],
        );

        let error = prepare_workspace(&source, /*confirmed*/ true)
            .expect_err("a differently pinned branch must never be reused or reset");

        assert!(matches!(error, ReplayError::WorkspaceExists(_)));
        assert!(!preview.root.exists());
        assert_eq!(
            fixture_git(
                &source.repository.root,
                &["rev-parse", &format!("refs/heads/{}", preview.branch)],
            ),
            source.target_commit.as_str(),
            "Replay must leave an unrelated existing branch completely untouched",
        );
    }

    #[test]
    fn never_resumes_over_saved_or_untracked_reviewer_changes() {
        let (_directory, source) = reusable_workspace_source();
        let (_, workspace) = prepare_workspace(&source, /*confirmed*/ true)
            .expect("create the explicitly confirmed original scratch worktree");
        let workspace = workspace.unwrap();
        let reviewer_source = "pub fn token() -> usize { 42 }\n";
        std::fs::write(workspace.root.join("src/token.rs"), reviewer_source)
            .expect("retain the reviewer's saved reconstruction");

        let error = prepare_workspace(&source, /*confirmed*/ true)
            .expect_err("saved reviewer changes must never be reset or overwritten");

        assert!(matches!(error, ReplayError::WorkspaceExists(_)));
        assert_eq!(
            std::fs::read_to_string(workspace.root.join("src/token.rs")).unwrap(),
            reviewer_source,
        );
        assert_eq!(
            fixture_git(&workspace.root, &["branch", "--show-current"]),
            workspace.branch,
        );
    }

    #[test]
    fn refuses_an_unrelated_repository_at_the_expected_replay_path() {
        let (_directory, source) = reusable_workspace_source();
        let (preview, _) = prepare_workspace(&source, /*confirmed*/ false)
            .expect("preview without creating a scratch worktree");
        std::fs::create_dir(&preview.root).expect("unrelated existing directory");
        fixture_git(&preview.root, &["init", "--initial-branch=master"]);

        assert!(matches!(
            prepare_workspace(&source, /*confirmed*/ true),
            Err(ReplayError::WorkspaceExists(_)),
        ));
        assert_eq!(
            fixture_git(&preview.root, &["branch", "--show-current"]),
            "master",
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symbolic_link_at_the_expected_replay_path() {
        let (directory, source) = reusable_workspace_source();
        let (preview, _) = prepare_workspace(&source, /*confirmed*/ false)
            .expect("preview without creating a scratch worktree");
        let unrelated = directory.path().join("unrelated-review");
        std::fs::create_dir(&unrelated).expect("preserved symbolic-link destination");
        std::os::unix::fs::symlink(&unrelated, &preview.root)
            .expect("symbolic-link collision fixture");

        assert!(matches!(
            prepare_workspace(&source, /*confirmed*/ true),
            Err(ReplayError::WorkspaceExists(_)),
        ));
        assert!(std::fs::symlink_metadata(&preview.root)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn pull_request_input_accepts_positive_numbers() {
        assert_eq!(
            PullRequestInput::parse("482").unwrap(),
            PullRequestInput::Number(482)
        );
        assert!(PullRequestInput::parse("0").is_err());
    }

    #[test]
    fn pull_request_input_accepts_canonical_github_urls() {
        let parsed = PullRequestInput::parse("https://github.com/codersauce/red/pull/482").unwrap();
        assert!(matches!(parsed, PullRequestInput::Url { number: 482, .. }));
    }

    #[test]
    fn pull_request_input_rejects_credentials_query_and_fragments() {
        for input in [
            "https://token@github.com/owner/repo/pull/1",
            "https://github.com/owner/repo/pull/1?token=secret",
            "https://github.com/owner/repo/pull/1#review",
            "http://github.com/owner/repo/pull/1",
            "https://github.com/owner/repo/issues/1",
        ] {
            assert!(PullRequestInput::parse(input).is_err(), "{input}");
        }
    }

    #[test]
    fn parses_https_and_ssh_github_origins() {
        assert_eq!(
            parse_remote("https://github.com/codersauce/red.git").unwrap(),
            (
                "github.com".to_string(),
                "codersauce".to_string(),
                "red".to_string()
            )
        );
        assert_eq!(
            parse_remote("git@github.com:codersauce/red.git").unwrap(),
            (
                "github.com".to_string(),
                "codersauce".to_string(),
                "red".to_string()
            )
        );
    }

    #[test]
    fn rejects_unsafe_repository_and_git_reference_components() {
        for reference in ["", "../main", "-main", "main.lock", "a//b", "a@{b"] {
            assert!(validate_git_reference(reference).is_err(), "{reference}");
        }
        assert!(validate_git_reference("feature/pr-replay").is_ok());
    }

    #[test]
    fn rejects_absolute_parent_and_git_administrative_paths() {
        for path in [
            Path::new("/tmp/escape"),
            Path::new("../escape"),
            Path::new(".git/config"),
            Path::new("src/../escape"),
        ] {
            assert!(validate_relative_path(path).is_err(), "{}", path.display());
        }
        assert!(validate_relative_path(Path::new("src/replay/mod.rs")).is_ok());
    }

    #[test]
    fn git_object_ids_require_complete_immutable_hex() {
        assert!(GitObjectId::parse("1234567").is_err());
        assert!(GitObjectId::parse(&"g".repeat(40)).is_err());
        let object = GitObjectId::parse(&"a".repeat(40)).unwrap();
        assert_eq!(object.short(), "aaaaaaa");
    }

    #[test]
    fn local_feature_replay_uses_merge_base_and_preserves_the_checked_out_branch() {
        let (repository, merge_base, feature_head) = diverged_local_repository();
        let resolved = resolve_local_branch_source(
            repository.path(),
            "feature/replay",
            Some("master"),
            ReplayLimits::default(),
        )
        .expect("resolve a diverged feature against its actual merge base");

        assert_eq!(resolved.head_ref, "feature/replay");
        assert_eq!(resolved.base_ref, "master");
        assert_eq!(resolved.source.kind, ReplaySourceKind::LocalRange);
        assert_eq!(resolved.source.base_commit, merge_base);
        assert_eq!(resolved.source.target_commit, feature_head);
        assert!(resolved.source.patch.contains("diff --git a/src/token.rs"));
        assert!(resolved
            .source
            .patch
            .contains("+pub fn token() -> usize { 2 }"));
        assert!(!resolved.source.patch.contains("unrelated.txt"));
        assert_eq!(
            fixture_git(repository.path(), &["branch", "--show-current"]),
            "master",
        );
    }

    #[test]
    fn local_feature_replay_prefers_the_pinned_origin_default_branch() {
        let (repository, merge_base, feature_head) = diverged_local_repository();
        let master = fixture_git(repository.path(), &["rev-parse", "master"]);
        fixture_git(
            repository.path(),
            &["update-ref", "refs/remotes/origin/master", &master],
        );
        fixture_git(
            repository.path(),
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/master",
            ],
        );

        let resolved = resolve_local_branch_source(
            repository.path(),
            "feature/replay",
            /*base*/ None,
            ReplayLimits::default(),
        )
        .expect("detect the locally pinned origin default branch");

        assert_eq!(resolved.base_ref, "origin/master");
        assert_eq!(resolved.source.base_commit, merge_base);
        assert_eq!(resolved.source.target_commit, feature_head);
        assert!(!resolved.source.patch.contains("unrelated.txt"));
    }

    #[test]
    fn local_feature_replay_rejects_unsafe_or_missing_branch_references() {
        let (repository, _, _) = diverged_local_repository();

        for reference in ["", "-feature", "../feature", "feature@{1}"] {
            assert!(
                resolve_local_branch_source(
                    repository.path(),
                    reference,
                    Some("master"),
                    ReplayLimits::default(),
                )
                .is_err(),
                "unsafe feature reference was accepted: {reference}",
            );
        }
        assert!(resolve_local_branch_source(
            repository.path(),
            "feature/replay",
            Some("../master"),
            ReplayLimits::default(),
        )
        .is_err());
        assert_eq!(
            fixture_git(repository.path(), &["branch", "--show-current"]),
            "master",
        );
    }
}
