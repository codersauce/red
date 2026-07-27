//! Exact, multi-file presentation snapshots compiled from pinned Replay sources.
//!
//! The coach receives one complete original Git hunk per learning step together
//! with the full before- and after-images of its scratch-worktree source file.
//! Git transport headers are preserved for validation even though the native
//! panel presents only the syntax-highlighted source.

use std::{collections::HashMap, path::Path};

use super::{
    parse_patch, ReplayChangeKind, ReplayDemoPlan, ReplayDemoStep, ReplayError, ReplayLimits,
    ReplaySession, ReplayStep, ReplayStepKind,
};

/// Compiles exact, source-backed presentation steps for a confirmed worktree.
///
/// Each generated step preserves its own original Git headers and unified hunk;
/// its complete source images are advanced independently for every changed
/// file. No original repository file is edited, and no change is applied to the
/// durable scratch worktree.
///
/// # Errors
///
/// Returns an error when the original patch is malformed, a scratch source is
/// missing, a path escapes the workspace, or a hunk cannot be applied at one
/// unambiguous position in its original merge-base image.
pub fn replay_plan_from_session(
    session: &ReplaySession,
    branch: &str,
    limits: ReplayLimits,
) -> Result<ReplayDemoPlan, ReplayError> {
    let patch = parse_patch(&session.source.patch, limits)?;
    let hunk_groups = original_hunk_groups(&session.source.patch);
    if patch.files.len() != hunk_groups.len() {
        return Err(ReplayError::InvalidPatch(
            "original source file and hunk groups do not match".to_string(),
        ));
    }

    let workspace_root = std::fs::canonicalize(&session.workspace.root)
        .map_err(|error| ReplayError::Filesystem(error.to_string()))?;
    let mut file_images: HashMap<String, String> = HashMap::new();
    let mut steps = Vec::with_capacity(session.steps.len());
    let mut initial_source = None;

    for (file, original_hunks) in patch.files.into_iter().zip(hunk_groups) {
        if !file.kind.supports_text_replay() {
            continue;
        }
        if file.hunks.len() != original_hunks.len() {
            return Err(ReplayError::InvalidPatch(
                "original file hunk boundaries do not match".to_string(),
            ));
        }
        let path = file
            .path()
            .map(Path::to_path_buf)
            .ok_or_else(|| ReplayError::InvalidPatch("source file has no path".to_string()))?;
        let display_path = path.to_string_lossy().into_owned();
        let initial = scratch_file_image(&workspace_root, &path, file.kind)?;
        if initial_source.is_none() {
            initial_source = Some((display_path.clone(), initial.clone()));
        }
        file_images.insert(display_path.clone(), initial);

        for (hunk, diff) in file.hunks.into_iter().zip(original_hunks) {
            let source_step = session.steps.get(steps.len()).ok_or_else(|| {
                ReplayError::InvalidPatch("original source has an unexpected hunk".to_string())
            })?;
            if source_step.path != path
                || source_step.before != hunk.before
                || source_step.after != hunk.after
            {
                return Err(ReplayError::InvalidPatch(
                    "original session hunk no longer matches its source file".to_string(),
                ));
            }
            let standalone = parse_patch(&diff, limits)?;
            if standalone.files.len() != 1
                || standalone.files[0].hunks.len() != 1
                || standalone.files[0].path() != Some(path.as_path())
            {
                return Err(ReplayError::InvalidPatch(
                    "original step is not one complete independent unified hunk".to_string(),
                ));
            }

            let before = file_images
                .get(&display_path)
                .cloned()
                .ok_or_else(|| ReplayError::UnsafePath(display_path.clone()))?;
            let after = apply_unique_original_hunk(&before, source_step)?;
            file_images.insert(display_path.clone(), after.clone());
            steps.push(ReplayDemoStep {
                id: source_step.id.clone(),
                ordinal: source_step.ordinal,
                path: display_path.clone(),
                kind: replay_kind(source_step.kind).to_string(),
                title: replay_title(source_step, &display_path),
                why: replay_rationale(session, source_step, &display_path),
                task: format!("Reconstruct the exact original change in {display_path}."),
                hint: String::new(),
                before,
                after,
                diff,
            });
        }
    }

    if steps.len() != session.steps.len() || steps.is_empty() {
        return Err(ReplayError::UnsupportedOperation(
            "the selected source contains no replayable text hunks".to_string(),
        ));
    }
    let (source_path, initial_source) = initial_source.ok_or_else(|| {
        ReplayError::UnsupportedOperation("no replayable scratch source exists".to_string())
    })?;
    let context = session.source.review_context.as_ref();
    let pull_request = session.source.pull_request.as_ref();

    Ok(ReplayDemoPlan {
        pull_request: pull_request.map_or(0, |request| request.number),
        title: context
            .map(|context| context.title.clone())
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| format!("Replay {branch}")),
        author: context
            .and_then(|context| context.author.clone())
            .or_else(|| pull_request.and_then(|request| request.author.clone()))
            .unwrap_or_else(|| "local".to_string()),
        branch: branch.to_string(),
        source_path,
        initial_source,
        steps,
    })
}

fn original_hunk_groups(patch: &str) -> Vec<Vec<String>> {
    let mut files = Vec::new();
    let mut headers = String::new();
    let mut hunk = String::new();
    let mut group = Vec::new();
    let mut in_file = false;

    for raw in patch.split_inclusive('\n') {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.starts_with("diff --git ") {
            if in_file {
                if !hunk.is_empty() {
                    group.push(format!("{headers}{hunk}"));
                    hunk.clear();
                }
                files.push(std::mem::take(&mut group));
            }
            headers.clear();
            headers.push_str(raw);
            in_file = true;
        } else if line.starts_with("@@ ") && in_file {
            if !hunk.is_empty() {
                group.push(format!("{headers}{hunk}"));
                hunk.clear();
            }
            hunk.push_str(raw);
        } else if in_file && hunk.is_empty() {
            headers.push_str(raw);
        } else if in_file {
            hunk.push_str(raw);
        }
    }

    if in_file {
        if !hunk.is_empty() {
            group.push(format!("{headers}{hunk}"));
        }
        files.push(group);
    }
    files
}

fn scratch_file_image(
    workspace_root: &Path,
    path: &Path,
    kind: ReplayChangeKind,
) -> Result<String, ReplayError> {
    let scratch_path = workspace_root.join(path);
    if !scratch_path.exists() {
        if kind == ReplayChangeKind::AddFile && std::fs::symlink_metadata(&scratch_path).is_err() {
            return Ok(String::new());
        }
        return Err(ReplayError::Filesystem(format!(
            "original scratch source does not exist: {}",
            path.display(),
        )));
    }
    let canonical = std::fs::canonicalize(&scratch_path)
        .map_err(|error| ReplayError::Filesystem(error.to_string()))?;
    if !canonical.starts_with(workspace_root) {
        return Err(ReplayError::UnsafePath(path.display().to_string()));
    }
    std::fs::read_to_string(canonical).map_err(|error| ReplayError::Filesystem(error.to_string()))
}

fn apply_unique_original_hunk(before: &str, step: &ReplayStep) -> Result<String, ReplayError> {
    if step.before.is_empty() {
        if !before.is_empty() {
            return Err(ReplayError::AnchorConflict);
        }
        return Ok(step.after.clone());
    }
    if before.matches(&step.before).count() != 1 {
        return Err(ReplayError::AnchorConflict);
    }
    Ok(before.replacen(&step.before, &step.after, /*count*/ 1))
}

const fn replay_kind(kind: ReplayStepKind) -> &'static str {
    match kind {
        ReplayStepKind::Add => "add",
        ReplayStepKind::Change => "change",
        ReplayStepKind::Remove => "remove",
        ReplayStepKind::AddFile => "add_file",
    }
}

fn replay_title(step: &ReplayStep, path: &str) -> String {
    let heading = step.heading.trim();
    if heading.is_empty() {
        format!("Update {path}")
    } else {
        heading.to_string()
    }
}

fn replay_rationale(session: &ReplaySession, step: &ReplayStep, path: &str) -> String {
    if let Some(description) = session
        .source
        .review_context
        .as_ref()
        .and_then(|context| context.body.lines().find(|line| !line.trim().is_empty()))
    {
        return description.trim().chars().take(240).collect();
    }
    format!(
        "Study the original {} hunk in {path} before reconstructing it.",
        replay_kind(step.kind),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::{
        digest, GitObjectId, ReplayRepository, ReplaySource, ReplaySourceKind, ReplayWorkspace,
    };

    const MULTI_FILE_PATCH: &str = concat!(
        "diff --git a/src/token.rs b/src/token.rs\n",
        "index 1111111..2222222 100644\n",
        "--- a/src/token.rs\n",
        "+++ b/src/token.rs\n",
        "@@ -1,3 +1,3 @@ fn first\n",
        " fn first() {\n",
        "-    old_first();\n",
        "+    new_first();\n",
        " }\n",
        "@@ -5,3 +5,3 @@ fn second\n",
        " fn second() {\n",
        "-    old_second();\n",
        "+    new_second();\n",
        " }\n",
        "diff --git a/tests/token.rs b/tests/token.rs\n",
        "index 3333333..4444444 100644\n",
        "--- a/tests/token.rs\n",
        "+++ b/tests/token.rs\n",
        "@@ -1 +1 @@ token test\n",
        "-assert_eq!(token(), 1);\n",
        "+assert_eq!(token(), 2);\n",
    );

    fn source_session(patch: &str) -> (tempfile::TempDir, ReplaySession) {
        let directory = tempfile::tempdir().expect("isolated replay source fixture");
        let root = directory.path();
        std::fs::create_dir_all(root.join("src")).expect("source fixture directory");
        std::fs::create_dir_all(root.join("tests")).expect("test fixture directory");
        std::fs::write(
            root.join("src/token.rs"),
            "fn first() {\n    old_first();\n}\n\nfn second() {\n    old_second();\n}\n",
        )
        .expect("merge-base source image");
        std::fs::write(root.join("tests/token.rs"), "assert_eq!(token(), 1);\n")
            .expect("merge-base test image");
        let base = GitObjectId::parse(&"a".repeat(40)).unwrap();
        let source = ReplaySource {
            id: "real-source".to_string(),
            repository: ReplayRepository {
                root: root.to_path_buf(),
                common_directory: root.join(".git"),
                host: "github.com".to_string(),
                owner: "owner".to_string(),
                name: "repository".to_string(),
            },
            kind: ReplaySourceKind::LocalRange,
            base_commit: base.clone(),
            target_commit: GitObjectId::parse(&"b".repeat(40)).unwrap(),
            patch: patch.to_string(),
            patch_digest: digest(patch.as_bytes()),
            pull_request: None,
            review_context: None,
        };
        let workspace = ReplayWorkspace {
            root: root.to_path_buf(),
            branch: "replay/real-source".to_string(),
            base_commit: base,
            created_by_replay: true,
        };
        let session = ReplaySession::from_source(source, workspace, ReplayLimits::default())
            .expect("source-linked replay session");
        (directory, session)
    }

    #[test]
    fn real_plan_retains_the_exact_original_independent_git_hunks() {
        let (_directory, session) = source_session(MULTI_FILE_PATCH);
        let plan = replay_plan_from_session(&session, "feature/replay", ReplayLimits::default())
            .expect("compile the original author hunks");

        assert_eq!(plan.pull_request, 0);
        assert_eq!(plan.branch, "feature/replay");
        assert_eq!(plan.steps.len(), 3);
        assert_eq!(plan.source_path, "src/token.rs");
        assert!(plan.steps[0].diff.contains("@@ -1,3 +1,3 @@ fn first"));
        assert!(!plan.steps[0].diff.contains("@@ -5,3"));
        assert!(plan.steps[1].diff.contains("@@ -5,3 +5,3 @@ fn second"));
        assert!(!plan.steps[1].diff.contains("@@ -1,3"));
        assert_eq!(plan.steps[2].path, "tests/token.rs");
        for step in &plan.steps {
            let parsed = parse_patch(&step.diff, ReplayLimits::default())
                .expect("one original independently parseable unified diff");
            assert_eq!(parsed.files.len(), 1);
            assert_eq!(parsed.files[0].hunks.len(), 1);
            assert_eq!(parsed.files[0].path(), Some(Path::new(&step.path)));
        }
    }

    #[test]
    fn real_multi_file_plan_advances_each_full_file_image_independently() {
        let (_directory, session) = source_session(MULTI_FILE_PATCH);
        let plan = replay_plan_from_session(&session, "feature/replay", ReplayLimits::default())
            .expect("compile source-backed multi-file review");

        assert!(plan.steps[0].before.contains("old_first()"));
        assert!(plan.steps[0].before.contains("old_second()"));
        assert!(plan.steps[0].after.contains("new_first()"));
        assert!(plan.steps[0].after.contains("old_second()"));
        assert_eq!(plan.steps[1].before, plan.steps[0].after);
        assert!(plan.steps[1].after.contains("new_first()"));
        assert!(plan.steps[1].after.contains("new_second()"));
        assert_eq!(plan.steps[2].before, "assert_eq!(token(), 1);\n");
        assert_eq!(plan.steps[2].after, "assert_eq!(token(), 2);\n");
    }

    #[test]
    fn real_plan_refuses_missing_or_modified_scratch_preimages() {
        let (directory, session) = source_session(MULTI_FILE_PATCH);
        std::fs::write(
            directory.path().join("src/token.rs"),
            "fn first() {\n    reviewer_changed_it();\n}\n",
        )
        .expect("simulate a modified scratch pre-image");

        assert!(matches!(
            replay_plan_from_session(&session, "feature/replay", ReplayLimits::default()),
            Err(ReplayError::AnchorConflict),
        ));
    }

    #[cfg(unix)]
    #[test]
    fn real_plan_refuses_scratch_source_symlinks_outside_the_worktree() {
        use std::os::unix::fs::symlink;

        let (directory, session) = source_session(MULTI_FILE_PATCH);
        let external = tempfile::NamedTempFile::new().expect("external source fixture");
        let path = directory.path().join("src/token.rs");
        std::fs::remove_file(&path).expect("replace fixture source with a symlink");
        symlink(external.path(), &path).expect("external scratch symlink");

        assert!(matches!(
            replay_plan_from_session(&session, "feature/replay", ReplayLimits::default()),
            Err(ReplayError::UnsafePath(_)),
        ));
    }
}
