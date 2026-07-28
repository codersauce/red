//! Bounded, editor-owned GitHub metadata and immutable Git source resolution.

use std::{
    ffi::OsStr,
    io::Write as _,
    path::{Component, Path, PathBuf},
    process::{Command, Output, Stdio},
};

use serde::{Deserialize, Serialize};
use url::Url;

use super::{digest, now_ms, ReplayError, ReplayLimits, ReplayWorkspace, ReplayWorkspacePreview};

const GITHUB_METADATA_FIELDS: &str = "number,url,title,body,author,baseRefName,baseRefOid,headRefName,headRefOid,headRepository,headRepositoryOwner,isCrossRepository,commits,changedFiles";
const GITHUB_CAPABILITIES_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!) { viewer { login } repository(owner: $owner, name: $name) { nameWithOwner pullRequest(number: $number) { number author { login } headRefName headRefOid headRepository { nameWithOwner viewerPermission } } } }";
const MAX_COMMAND_DIAGNOSTIC_BYTES: usize = 4 * 1024;

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
        Err(ReplayError::CommandFailed { .. }) => {}
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
    let branch_exists = replay_git_command(&source.repository.root)
        .args(["show-ref", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .stdin(Stdio::null())
        .status()
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
    replay_git_command(root)
        .args(["cat-file", "-e"])
        .arg(format!("{}^{{commit}}", object.as_str()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .map_err(|error| ReplayError::CommandFailed {
            program: "git".to_string(),
            message: error.to_string(),
        })
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

fn run_command(command: &mut Command, limit: usize) -> Result<Vec<u8>, ReplayError> {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = command
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|error| ReplayError::CommandFailed {
            program: program.clone(),
            message: error.to_string(),
        })?;
    bounded_command_output(program, output, limit)
}

/// Runs one bounded command with an explicitly supplied, noninteractive body.
pub(super) fn run_command_with_input(
    command: &mut Command,
    input: &[u8],
    limit: usize,
) -> Result<Vec<u8>, ReplayError> {
    if input.len() > limit {
        return Err(ReplayError::LimitExceeded {
            kind: "GitHub review submission",
            limit,
        });
    }

    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        .spawn()
        .map_err(|error| ReplayError::CommandFailed {
            program: program.clone(),
            message: error.to_string(),
        })?;
    let write_result = child
        .stdin
        .take()
        .ok_or_else(|| ReplayError::CommandFailed {
            program: program.clone(),
            message: "could not open the GitHub review request body".to_string(),
        })?
        .write_all(input);
    if let Err(error) = write_result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(ReplayError::CommandFailed {
            program,
            message: format!("could not send the GitHub review request body: {error}"),
        });
    }
    let output = child
        .wait_with_output()
        .map_err(|error| ReplayError::CommandFailed {
            program: program.clone(),
            message: error.to_string(),
        })?;
    bounded_command_output(program, output, limit)
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
            message: diagnostic.trim().to_string(),
        });
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

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
