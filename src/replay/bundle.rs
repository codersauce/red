//! Explicit, portable, private review bundles pinned to immutable PR sources.

use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::{
    digest, now_ms, GitObjectId, ReplayController, ReplayError, ReplayLimits, ReplayNote,
    ReplayReviewDraft, ReplayReviewDraftKind, ReplayReviewRole, ReplaySession, ReplaySource,
    ReplaySourceKind,
};

/// Current, deliberately versioned portable review-bundle format.
pub const REPLAY_REVIEW_BUNDLE_VERSION: u32 = 1;

/// Upper bound applied before a local review file is read or decoded.
pub const MAX_REPLAY_REVIEW_BUNDLE_BYTES: u64 = 16 * 1024 * 1024;

/// Suggests a private Git-metadata path that never dirties the review worktree.
#[must_use]
pub fn suggested_review_bundle_path(source: &ReplaySource) -> PathBuf {
    let name = source.pull_request.as_ref().map_or_else(
        || format!("local-{}.red-review.json", source.target_commit.short()),
        |request| {
            format!(
                "pr-{}-{}.red-review.json",
                request.number,
                source.target_commit.short(),
            )
        },
    );
    source
        .repository
        .common_directory
        .join("red")
        .join("replay-reviews")
        .join(name)
}

/// Cross-computer identity of the exact original source being reviewed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayReviewBundleIdentity {
    /// Case-normalized host, owner, and repository; never a machine-local path.
    pub repository: String,
    /// Whether this source is an original GitHub PR or a pinned local range.
    pub source_kind: ReplaySourceKind,
    /// Original GitHub PR number, when the source came from a pull request.
    pub pull_request: Option<u64>,
    /// Exact original merge-base or selected local base.
    pub base_commit: GitObjectId,
    /// Exact original PR head or selected local target.
    pub target_commit: GitObjectId,
    /// SHA-256 of the complete original canonical diff.
    pub patch_digest: String,
}

impl ReplayReviewBundleIdentity {
    fn from_source(source: &ReplaySource) -> Self {
        Self {
            repository: source.repository.host_repository().to_ascii_lowercase(),
            source_kind: source.kind,
            pull_request: source.pull_request.as_ref().map(|request| request.number),
            base_commit: source.base_commit.clone(),
            target_commit: source.target_commit.clone(),
            patch_digest: source.patch_digest.clone(),
        }
    }
}

/// User-exported private findings and drafts; never a remote pending review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayReviewBundle {
    /// Explicit portable format version; unknown versions are rejected.
    pub version: u32,
    /// Original repository, PR, commits, and complete-patch identity.
    pub identity: ReplayReviewBundleIdentity,
    /// Original-source-linked private reviewer observations.
    pub notes: Vec<ReplayNote>,
    /// Original-hunk-anchored private comments, summaries, and fix proposals.
    pub drafts: Vec<ReplayReviewDraft>,
    /// Unix-millisecond time at which the user explicitly saved the file.
    pub exported_at_ms: u64,
}

/// Exact user-visible result of an explicitly saved local review bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplayReviewBundleSaved {
    /// Canonical parent plus the exact user-selected review filename.
    pub path: PathBuf,
    /// Number of private observations in the resulting file.
    pub note_count: usize,
    /// Number of original-source-anchored local drafts in the resulting file.
    pub draft_count: usize,
}

/// Verified import preview; committing requires its exact file-content digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplayReviewBundlePreview {
    /// Canonical parent plus the exact user-selected review filename.
    pub path: PathBuf,
    /// SHA-256 of the complete, bounded file inspected for confirmation.
    pub bundle_digest: String,
    /// Private findings that would be added without replacing existing notes.
    pub notes_to_add: usize,
    /// Identical findings already present in the current review.
    pub notes_already_present: usize,
    /// Local review drafts that would be added without replacing existing drafts.
    pub drafts_to_add: usize,
    /// Identical original-source drafts already present in the review.
    pub drafts_already_present: usize,
}

impl ReplayController {
    /// Freezes the exact editor-owned review before background file export.
    pub(crate) fn prepare_review_bundle(
        &self,
        session_id: &str,
    ) -> Result<ReplayReviewBundle, ReplayError> {
        ReplayReviewBundle::from_session(self.session(session_id)?, self.limits())
    }

    /// Saves an explicitly selected, private file without contacting GitHub.
    ///
    /// Existing regular files are replaced only after `overwrite` is explicitly
    /// confirmed. The original PR, scratch source, and Git refs remain untouched.
    pub fn save_review_bundle(
        &self,
        session_id: &str,
        path: &Path,
        overwrite: bool,
    ) -> Result<ReplayReviewBundleSaved, ReplayError> {
        let bundle = self.prepare_review_bundle(session_id)?;
        write_prepared_review_bundle(&bundle, path, overwrite)
    }

    /// Reads and validates a private review file without changing session state.
    pub fn preview_review_bundle(
        &self,
        session_id: &str,
        path: &Path,
    ) -> Result<ReplayReviewBundlePreview, ReplayError> {
        preview_review_bundle_snapshot(self.session(session_id)?, self.limits(), path)
    }

    /// Merges only the exact, user-confirmed and previously previewed file.
    ///
    /// Conflicts, changed file contents, foreign repositories, moved heads, and
    /// unauthorized fix proposals all fail before any local state is changed.
    pub fn import_review_bundle(
        &mut self,
        session_id: &str,
        path: &Path,
        expected_digest: &str,
        confirmed: bool,
    ) -> Result<ReplayReviewBundlePreview, ReplayError> {
        let limits = self.limits();
        let (bundle, preview) = prepare_review_bundle_import(
            self.session(session_id)?,
            limits,
            path,
            expected_digest,
            confirmed,
        )?;
        self.merge_review_bundle(session_id, bundle, preview)
    }

    /// Commits a background-validated bundle only if the live review still matches.
    pub(crate) fn merge_review_bundle(
        &mut self,
        session_id: &str,
        bundle: ReplayReviewBundle,
        expected: ReplayReviewBundlePreview,
    ) -> Result<ReplayReviewBundlePreview, ReplayError> {
        let preview = {
            let session = self.session(session_id)?;
            bundle.preview_for(
                session,
                self.limits(),
                expected.path.clone(),
                expected.bundle_digest.clone(),
            )?
        };
        if preview != expected {
            return Err(ReplayError::StalePreview);
        }
        if preview.notes_to_add == 0 && preview.drafts_to_add == 0 {
            return Ok(preview);
        }

        let session = self.session_mut(session_id)?;
        for note in bundle.notes {
            if !session.notes.iter().any(|existing| existing.id == note.id) {
                session.notes.push(note);
            }
        }
        for draft in bundle.drafts {
            if !session
                .review
                .drafts
                .iter()
                .any(|existing| existing.id == draft.id)
            {
                session.review.drafts.push(draft);
            }
        }
        session.generation = session.generation.saturating_add(1);
        self.advance_generation();
        Ok(preview)
    }
}

/// Saves an immutable review snapshot on the bounded Replay background worker.
pub(crate) fn write_prepared_review_bundle(
    bundle: &ReplayReviewBundle,
    path: &Path,
    overwrite: bool,
) -> Result<ReplayReviewBundleSaved, ReplayError> {
    write_review_bundle(bundle, path, overwrite)
}

/// Validates a bounded local review file against an immutable session snapshot.
pub(crate) fn preview_review_bundle_snapshot(
    session: &ReplaySession,
    limits: ReplayLimits,
    path: &Path,
) -> Result<ReplayReviewBundlePreview, ReplayError> {
    let (bundle, path, bundle_digest) = read_review_bundle(path)?;
    bundle.preview_for(session, limits, path, bundle_digest)
}

/// Prepares an explicitly confirmed import without mutating editor-owned state.
pub(crate) fn prepare_review_bundle_import(
    session: &ReplaySession,
    limits: ReplayLimits,
    path: &Path,
    expected_digest: &str,
    confirmed: bool,
) -> Result<(ReplayReviewBundle, ReplayReviewBundlePreview), ReplayError> {
    if !confirmed {
        return Err(ReplayError::ReviewBundleConfirmationRequired);
    }
    let (bundle, path, actual_digest) = read_review_bundle(path)?;
    if actual_digest != expected_digest {
        return Err(ReplayError::InvalidReviewBundle(
            "the review file changed after its import preview; preview it again".to_string(),
        ));
    }
    let preview = bundle.preview_for(session, limits, path, actual_digest)?;
    Ok((bundle, preview))
}

impl ReplayReviewBundle {
    fn from_session(session: &ReplaySession, limits: ReplayLimits) -> Result<Self, ReplayError> {
        if session.notes.is_empty() && session.review.drafts.is_empty() {
            return Err(ReplayError::InvalidReviewBundle(
                "there are no local review comments, summaries, proposals, or observations to save"
                    .to_string(),
            ));
        }
        let bundle = Self {
            version: REPLAY_REVIEW_BUNDLE_VERSION,
            identity: ReplayReviewBundleIdentity::from_source(&session.source),
            notes: session.notes.clone(),
            drafts: session.review.drafts.clone(),
            exported_at_ms: now_ms(),
        };
        bundle.validate_for(session, limits)?;
        Ok(bundle)
    }

    fn validate_for(
        &self,
        session: &ReplaySession,
        limits: ReplayLimits,
    ) -> Result<(), ReplayError> {
        if self.version != REPLAY_REVIEW_BUNDLE_VERSION {
            return Err(ReplayError::InvalidReviewBundle(format!(
                "unsupported review bundle version {}; this Red version supports version {}",
                self.version, REPLAY_REVIEW_BUNDLE_VERSION,
            )));
        }
        if digest(session.source.patch.as_bytes()) != session.source.patch_digest {
            return Err(ReplayError::InvalidReviewBundle(
                "the current review no longer matches its complete original source diff"
                    .to_string(),
            ));
        }
        if self.identity != ReplayReviewBundleIdentity::from_source(&session.source) {
            return Err(ReplayError::InvalidReviewBundle(
                "this review file belongs to a different repository, pull request, original head, base, or diff"
                    .to_string(),
            ));
        }
        if self.notes.is_empty() && self.drafts.is_empty() {
            return Err(ReplayError::InvalidReviewBundle(
                "the review file does not contain any local observations or drafts".to_string(),
            ));
        }
        if self.notes.len() > limits.max_steps {
            return Err(ReplayError::LimitExceeded {
                kind: "portable review observations",
                limit: limits.max_steps,
            });
        }
        if self.drafts.len() > limits.max_steps {
            return Err(ReplayError::LimitExceeded {
                kind: "portable review drafts",
                limit: limits.max_steps,
            });
        }

        let mut note_ids = HashSet::with_capacity(self.notes.len());
        for note in &self.notes {
            if note.id.trim().is_empty()
                || !note_ids.insert(note.id.as_str())
                || note.target_commit != session.source.target_commit
                || note.text.trim().is_empty()
                || note.text.len() > limits.max_note_bytes
            {
                return Err(ReplayError::InvalidReviewBundle(
                    "a portable observation is duplicated, unbounded, or belongs to a different original head"
                        .to_string(),
                ));
            }
            let expected_path = note.step_id.as_deref().map(|id| {
                session
                    .steps
                    .iter()
                    .find(|step| step.id == id)
                    .map(|step| step.path.as_path())
            });
            if matches!(expected_path, Some(None))
                || note.path.as_deref() != expected_path.flatten()
            {
                return Err(ReplayError::InvalidReviewBundle(
                    "a portable observation no longer matches its exact original source hunk"
                        .to_string(),
                ));
            }
        }

        let mut draft_ids = HashSet::with_capacity(self.drafts.len());
        for draft in &self.drafts {
            if draft.id.trim().is_empty()
                || !draft_ids.insert(draft.id.as_str())
                || draft.target_commit != session.source.target_commit
                || draft.text.trim().is_empty()
                || draft.text.len() > limits.max_note_bytes
                || draft.updated_at_ms < draft.created_at_ms
            {
                return Err(ReplayError::InvalidReviewBundle(
                    "a portable review draft is duplicated, unbounded, or belongs to a different original head"
                        .to_string(),
                ));
            }
            if draft.kind == ReplayReviewDraftKind::CodeFix
                && session.review.role != ReplayReviewRole::Author
            {
                return Err(ReplayError::InvalidReviewBundle(
                    "original-PR fix proposals can be loaded only by the verified original author"
                        .to_string(),
                ));
            }
            match draft.kind {
                ReplayReviewDraftKind::InlineComment | ReplayReviewDraftKind::CodeFix => {
                    let step_id = draft.step_id.as_deref().ok_or_else(|| {
                        ReplayError::InvalidReviewBundle(
                            "a portable inline draft does not identify its original source hunk"
                                .to_string(),
                        )
                    })?;
                    let step = session
                        .steps
                        .iter()
                        .find(|step| step.id == step_id)
                        .ok_or_else(|| {
                            ReplayError::InvalidReviewBundle(
                                "a portable inline draft names an unrelated original source hunk"
                                    .to_string(),
                            )
                        })?;
                    let expected = session.original_review_anchor(step, limits)?;
                    if draft.anchor.as_ref() != Some(&expected)
                        || draft.path.as_deref() != Some(expected.path.as_path())
                    {
                        return Err(ReplayError::InvalidReviewBundle(
                            "a portable inline draft no longer matches its exact original diff side, line range, and hunk"
                                .to_string(),
                        ));
                    }
                }
                ReplayReviewDraftKind::ReviewSummary => {
                    if draft.step_id.is_some() || draft.path.is_some() || draft.anchor.is_some() {
                        return Err(ReplayError::InvalidReviewBundle(
                            "a portable PR-level summary cannot claim an inline diff anchor"
                                .to_string(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn preview_for(
        &self,
        session: &ReplaySession,
        limits: ReplayLimits,
        path: PathBuf,
        bundle_digest: String,
    ) -> Result<ReplayReviewBundlePreview, ReplayError> {
        self.validate_for(session, limits)?;
        let mut preview = ReplayReviewBundlePreview {
            path,
            bundle_digest,
            notes_to_add: 0,
            notes_already_present: 0,
            drafts_to_add: 0,
            drafts_already_present: 0,
        };

        for note in &self.notes {
            match session.notes.iter().find(|existing| existing.id == note.id) {
                Some(existing) if existing == note => preview.notes_already_present += 1,
                Some(_) => {
                    return Err(ReplayError::ReviewBundleConflict(
                        "an imported observation has the same ID as a different existing local observation"
                            .to_string(),
                    ));
                }
                None => preview.notes_to_add += 1,
            }
        }
        for draft in &self.drafts {
            match session
                .review
                .drafts
                .iter()
                .find(|existing| existing.id == draft.id)
            {
                Some(existing) if existing == draft => preview.drafts_already_present += 1,
                Some(_) => {
                    return Err(ReplayError::ReviewBundleConflict(
                        "an imported review draft has the same ID as a differently edited existing local draft"
                            .to_string(),
                    ));
                }
                None => preview.drafts_to_add += 1,
            }
        }

        if session.notes.len().saturating_add(preview.notes_to_add) > limits.max_steps {
            return Err(ReplayError::LimitExceeded {
                kind: "merged review observations",
                limit: limits.max_steps,
            });
        }
        if session
            .review
            .drafts
            .len()
            .saturating_add(preview.drafts_to_add)
            > limits.max_steps
        {
            return Err(ReplayError::LimitExceeded {
                kind: "merged review drafts",
                limit: limits.max_steps,
            });
        }
        Ok(preview)
    }
}

fn canonical_review_path(path: &Path) -> Result<PathBuf, ReplayError> {
    let filename = path.file_name().ok_or_else(|| {
        ReplayError::InvalidReviewBundle(
            "choose a local review filename, not a directory".to_string(),
        )
    })?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let parent = fs::canonicalize(parent.unwrap_or_else(|| Path::new("."))).map_err(|error| {
        ReplayError::Filesystem(format!(
            "cannot open the selected review directory: {error}"
        ))
    })?;
    if !parent.is_dir() {
        return Err(ReplayError::InvalidReviewBundle(
            "the selected review parent is not a directory".to_string(),
        ));
    }
    Ok(parent.join(filename))
}

fn write_review_bundle(
    bundle: &ReplayReviewBundle,
    path: &Path,
    overwrite: bool,
) -> Result<ReplayReviewBundleSaved, ReplayError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        if !parent.exists() {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;

                builder.mode(0o700);
            }
            builder.create(parent).map_err(|error| {
                ReplayError::Filesystem(format!(
                    "cannot create the selected private review directory: {error}",
                ))
            })?;
        }
    }
    let path = canonical_review_path(path)?;
    match fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(ReplayError::InvalidReviewBundle(
                "the selected review destination must be a regular file, not a link or directory"
                    .to_string(),
            ));
        }
        Ok(_) if !overwrite => {
            return Err(ReplayError::ReviewBundleExists(path.display().to_string()));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(ReplayError::Filesystem(format!(
                "cannot inspect the selected review file: {error}",
            )));
        }
    }

    let mut encoded = serde_json::to_vec_pretty(bundle).map_err(|error| {
        ReplayError::InvalidReviewBundle(format!("cannot encode the local review file: {error}"))
    })?;
    encoded.push(b'\n');
    if encoded.len() as u64 > MAX_REPLAY_REVIEW_BUNDLE_BYTES {
        return Err(ReplayError::LimitExceeded {
            kind: "portable review bundle bytes",
            limit: MAX_REPLAY_REVIEW_BUNDLE_BYTES as usize,
        });
    }
    let parent = path.parent().ok_or_else(|| {
        ReplayError::InvalidReviewBundle(
            "the selected review file has no containing directory".to_string(),
        )
    })?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".red-replay-review-")
        .tempfile_in(parent)
        .map_err(|error| {
            ReplayError::Filesystem(format!(
                "cannot create a private local review file: {error}"
            ))
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                ReplayError::Filesystem(format!(
                    "cannot protect the private local review file: {error}",
                ))
            })?;
    }
    temporary.write_all(&encoded).map_err(|error| {
        ReplayError::Filesystem(format!(
            "cannot write the private local review file: {error}"
        ))
    })?;
    temporary.as_file().sync_all().map_err(|error| {
        ReplayError::Filesystem(format!(
            "cannot sync the private local review file: {error}"
        ))
    })?;

    let persisted = if overwrite {
        temporary.persist(&path)
    } else {
        temporary.persist_noclobber(&path)
    };
    if let Err(error) = persisted {
        if !overwrite && error.error.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(ReplayError::ReviewBundleExists(path.display().to_string()));
        }
        return Err(ReplayError::Filesystem(format!(
            "cannot safely finalize the private local review file: {}",
            error.error,
        )));
    }
    #[cfg(unix)]
    {
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                ReplayError::Filesystem(format!(
                    "cannot sync the private local review directory: {error}",
                ))
            })?;
    }

    Ok(ReplayReviewBundleSaved {
        path,
        note_count: bundle.notes.len(),
        draft_count: bundle.drafts.len(),
    })
}

fn read_review_bundle(path: &Path) -> Result<(ReplayReviewBundle, PathBuf, String), ReplayError> {
    let path = canonical_review_path(path)?;
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        ReplayError::Filesystem(format!("cannot inspect the selected review file: {error}"))
    })?;
    if !metadata.file_type().is_file() {
        return Err(ReplayError::InvalidReviewBundle(
            "the selected review file must be a regular file, not a link or directory".to_string(),
        ));
    }
    if metadata.len() > MAX_REPLAY_REVIEW_BUNDLE_BYTES {
        return Err(ReplayError::LimitExceeded {
            kind: "portable review bundle bytes",
            limit: MAX_REPLAY_REVIEW_BUNDLE_BYTES as usize,
        });
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;

        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(&path).map_err(|error| {
        ReplayError::Filesystem(format!(
            "cannot safely open the selected review file: {error}"
        ))
    })?;
    let opened = file.metadata().map_err(|error| {
        ReplayError::Filesystem(format!("cannot verify the selected review file: {error}"))
    })?;
    if !opened.file_type().is_file() || opened.len() > MAX_REPLAY_REVIEW_BUNDLE_BYTES {
        return Err(ReplayError::InvalidReviewBundle(
            "the selected review file changed, is unsafe, or exceeds the local review limit"
                .to_string(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        if metadata.dev() != opened.dev() || metadata.ino() != opened.ino() {
            return Err(ReplayError::InvalidReviewBundle(
                "the selected review file changed while it was being opened".to_string(),
            ));
        }
    }

    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.take(MAX_REPLAY_REVIEW_BUNDLE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| {
            ReplayError::Filesystem(format!("cannot read the selected review file: {error}"))
        })?;
    if bytes.len() as u64 > MAX_REPLAY_REVIEW_BUNDLE_BYTES {
        return Err(ReplayError::LimitExceeded {
            kind: "portable review bundle bytes",
            limit: MAX_REPLAY_REVIEW_BUNDLE_BYTES as usize,
        });
    }
    let bundle = serde_json::from_slice(&bytes).map_err(|error| {
        ReplayError::InvalidReviewBundle(format!(
            "the selected file is not a valid portable review bundle: {error}",
        ))
    })?;
    Ok((bundle, path, digest(&bytes)))
}
