//! Exact, multi-file presentation snapshots compiled from pinned Replay sources.
//!
//! The editor retains complete scratch-source images and gives the coach only
//! one bounded, original Git hunk per learning step. Git transport headers are
//! preserved for validation even though the native panel presents only the
//! syntax-highlighted source.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use serde::{Deserialize, Serialize};

use super::session::{anchored_hunk_offset, replay_line_delta};
use super::{
    parse_patch, ReplayChangeKind, ReplayDemoPlan, ReplayDemoStep, ReplayError, ReplayLimits,
    ReplaySession, ReplayStep, ReplayStepKind,
};

/// Bounded reviewer-visible source context for one original-author hunk.
///
/// The editor retains the complete scratch-file images for validation and
/// application. The presentation boundary carries only this step's exact
/// independent unified diff and the original hunk-local source images.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayPresentationStep {
    /// Stable original source-hunk identity.
    pub id: String,
    /// One-based reconstruction order.
    pub ordinal: usize,
    /// Safe repository-relative scratch-source path.
    pub path: String,
    /// User-facing patch operation.
    pub kind: String,
    /// Concise description of the original change.
    pub title: String,
    /// Original-author rationale for this exercise.
    pub why: String,
    /// Concrete manual-reconstruction instruction.
    pub task: String,
    /// Optional graduated reconstruction hint.
    pub hint: String,
    /// Exact old hunk and its original surrounding context.
    pub before: String,
    /// Exact new hunk and its original surrounding context.
    pub after: String,
    /// Complete, independently parseable original Git unified hunk.
    pub diff: String,
}

/// Bounded metadata and original hunk snapshots sent to the Replay coach.
///
/// Complete file buffers never cross the plugin boundary or get repeated for
/// every change. They remain owned and validated by the editor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayPresentationPlan {
    /// Original GitHub pull request number, or zero for a local branch.
    pub pull_request: u64,
    /// Original pull request or local-review title.
    pub title: String,
    /// Original pull request author or local source label.
    pub author: String,
    /// Original reviewed feature branch.
    pub branch: String,
    /// Repository-relative initial scratch-source path.
    pub source_path: String,
    /// Ordered, independently parseable, hunk-local presentation steps.
    pub steps: Vec<ReplayPresentationStep>,
}

/// Projects an editor-owned replay into exact, bounded reviewer-visible hunks.
///
/// Full source-file images remain exclusively in the editor-owned plan, so a
/// small change in a megabyte-sized source file does not exhaust the panel's
/// bounded model or repeatedly serialize the complete source.
///
/// # Errors
///
/// Returns an error when a step does not contain exactly one original hunk,
/// its source path is inconsistent, or the complete serialized presentation
/// exceeds the configured review limit.
pub fn replay_presentation_plan(
    plan: &ReplayDemoPlan,
    limits: ReplayLimits,
) -> Result<ReplayPresentationPlan, ReplayError> {
    if plan.steps.len() > limits.max_steps {
        return Err(ReplayError::LimitExceeded {
            kind: "replay presentation steps",
            limit: limits.max_steps,
        });
    }

    let steps = plan
        .steps
        .iter()
        .map(|step| {
            let patch = parse_patch(&step.diff, limits)?;
            if patch.files.len() != 1 {
                return Err(ReplayError::InvalidPatch(
                    "presentation step is not one original source file".to_string(),
                ));
            }
            let file = &patch.files[0];
            if file.path() != Some(Path::new(&step.path)) || file.hunks.len() != 1 {
                return Err(ReplayError::InvalidPatch(
                    "presentation step is not one original source hunk".to_string(),
                ));
            }
            let hunk = &file.hunks[0];
            Ok(ReplayPresentationStep {
                id: step.id.clone(),
                ordinal: step.ordinal,
                path: step.path.clone(),
                kind: step.kind.clone(),
                title: step.title.clone(),
                why: step.why.clone(),
                task: step.task.clone(),
                hint: step.hint.clone(),
                before: hunk.before.clone(),
                after: hunk.after.clone(),
                diff: step.diff.clone(),
            })
        })
        .collect::<Result<Vec<_>, ReplayError>>()?;

    let presentation = ReplayPresentationPlan {
        pull_request: plan.pull_request,
        title: plan.title.clone(),
        author: plan.author.clone(),
        branch: plan.branch.clone(),
        source_path: plan.source_path.clone(),
        steps,
    };
    let bytes = serde_json::to_vec(&presentation)
        .map_err(|error| ReplayError::InvalidPatch(error.to_string()))?;
    if bytes.len() > limits.max_patch_bytes {
        return Err(ReplayError::LimitExceeded {
            kind: "replay presentation bytes",
            limit: limits.max_patch_bytes,
        });
    }

    Ok(presentation)
}

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
    let mut file_line_deltas: HashMap<String, isize> = HashMap::new();
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
            let old_start = source_step
                .old_start
                .saturating_add_signed(*file_line_deltas.get(&display_path).unwrap_or(&0));
            let after = apply_unique_original_hunk(&before, source_step, old_start)?;
            file_line_deltas
                .entry(display_path.clone())
                .and_modify(|delta| {
                    *delta = delta
                        .saturating_add(replay_line_delta(&source_step.before, &source_step.after));
                })
                .or_insert_with(|| replay_line_delta(&source_step.before, &source_step.after));
            file_images.insert(display_path.clone(), after.clone());
            steps.push(ReplayDemoStep {
                id: source_step.id.clone(),
                ordinal: source_step.ordinal,
                path: display_path.clone(),
                kind: replay_kind(source_step.kind).to_string(),
                title: replay_title(source_step, &display_path),
                why: replay_rationale(session, source_step, &display_path),
                task: replay_task(source_step, &display_path),
                hint: replay_hint(source_step, &display_path),
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

fn apply_unique_original_hunk(
    before: &str,
    step: &ReplayStep,
    old_start: usize,
) -> Result<String, ReplayError> {
    if step.before.is_empty() {
        if !before.is_empty() {
            return Err(ReplayError::AnchorConflict);
        }
        return Ok(step.after.clone());
    }
    let start = anchored_hunk_offset(before, &step.before, old_start)?;
    let end = start + step.before.len();
    let mut after = String::with_capacity(
        before
            .len()
            .saturating_sub(step.before.len())
            .saturating_add(step.after.len()),
    );
    after.push_str(&before[..start]);
    after.push_str(&step.after);
    after.push_str(&before[end..]);
    Ok(after)
}

const fn replay_kind(kind: ReplayStepKind) -> &'static str {
    match kind {
        ReplayStepKind::Add => "add",
        ReplayStepKind::Change => "change",
        ReplayStepKind::Remove => "remove",
        ReplayStepKind::AddFile => "add_file",
    }
}

const MAX_REPLAY_RATIONALE_CHARS: usize = 240;
const MAX_SEMANTIC_SOURCE_LINES: usize = 32;
const MAX_SEMANTIC_SOURCE_CHARS: usize = 8 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReviewSection {
    Motivation,
    Changes,
    Other,
}

fn replay_title(step: &ReplayStep, path: &str) -> String {
    if let Some((field, container)) = changed_field(step) {
        return named_change_title(step.kind, field, container);
    }

    if let Some((kind, symbol)) = source_symbol(&step.heading) {
        if matches!(kind, "fn" | "impl") {
            if let Some(function) = changed_function(step) {
                if function != symbol {
                    return named_change_title(step.kind, function, symbol);
                }
            }
            if let Some(binding) = changed_binding(step) {
                return named_change_title(step.kind, binding, symbol);
            }
        }
        return format!("Update {symbol}");
    }

    let source_path = Path::new(path);
    if source_path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "md" | "markdown" | "mdown" | "mdx" | "rst"
            )
        })
    {
        if let Some(subject) = changed_markdown_subject(step) {
            if let Some(endpoint) = markdown_endpoint(step) {
                return format!("Document {endpoint} {subject}");
            }
            return format!("Document {subject}");
        }
        if let Some(component) = source_path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
        {
            return format!("Update {component} documentation");
        }
        return "Update documentation".to_string();
    }

    if let Some(module) = changed_module(step) {
        return format!("{} {module} module", replay_action(step.kind));
    }

    let file = source_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path);
    match step.kind {
        ReplayStepKind::AddFile => format!("Add {file}"),
        ReplayStepKind::Add | ReplayStepKind::Change | ReplayStepKind::Remove => {
            format!("Update {file}")
        }
    }
}

fn replay_task(step: &ReplayStep, path: &str) -> String {
    if let Some((field, container)) = changed_field(step) {
        return named_change_task(step.kind, field, "field", container);
    }

    if let Some((kind, symbol)) = source_symbol(&step.heading) {
        if matches!(kind, "fn" | "impl") {
            if let Some(function) = changed_function(step) {
                if function != symbol {
                    return named_change_task(step.kind, function, "function", symbol);
                }
            }
            if let Some(binding) = changed_binding(step) {
                return named_change_task(step.kind, binding, "binding", symbol);
            }
        }
        return format!("Reconstruct the original change to {symbol} in {path}.");
    }

    format!("Reconstruct the exact original change in {path}.")
}

fn replay_hint(step: &ReplayStep, path: &str) -> String {
    let location = format!("{path}:{}", step.old_start.max(1));
    let action = replay_action(step.kind).to_ascii_lowercase();

    if let Some((field, container)) = changed_field(step) {
        return format!(
            "At {location}, {action} the `{field}` field {} `{container}`; use the neighboring original fields as your anchor.",
            replay_container_preposition(step.kind),
        );
    }

    if let Some((kind, container)) = source_symbol(&step.heading) {
        if matches!(kind, "fn" | "impl") {
            if let Some(function) = changed_function(step) {
                if function != container {
                    return format!(
                        "At {location}, {action} `{function}` {} `{container}`; preserve the surrounding original implementation.",
                        replay_container_preposition(step.kind),
                    );
                }
            }
            if let Some(binding) = changed_binding(step) {
                return format!(
                    "At {location}, {action} the `{binding}` binding {} `{container}`; use its unchanged neighboring statements as your anchor.",
                    replay_container_preposition(step.kind),
                );
            }
        }
        return format!(
            "At {location}, inspect `{container}` and reconstruct only the original {kind} change.",
        );
    }

    if let Some(subject) = changed_markdown_subject(step) {
        if let Some(endpoint) = markdown_endpoint(step) {
            return format!(
                "At {location}, update the `{endpoint}` documentation for `{subject}`; keep the surrounding original explanation intact.",
            );
        }
        return format!(
            "At {location}, update the original documentation for `{subject}` without changing unrelated examples.",
        );
    }

    if let Some(module) = changed_module(step) {
        return format!(
            "At {location}, {action} the `{module}` module; use its neighboring original declarations as your anchor.",
        );
    }

    format!(
        "At {location}, use the unchanged lines around the original {} hunk as your reconstruction anchor.",
        replay_kind(step.kind),
    )
}

const fn replay_action(kind: ReplayStepKind) -> &'static str {
    match kind {
        ReplayStepKind::Add | ReplayStepKind::AddFile => "Add",
        ReplayStepKind::Change => "Update",
        ReplayStepKind::Remove => "Remove",
    }
}

const fn replay_container_preposition(kind: ReplayStepKind) -> &'static str {
    match kind {
        ReplayStepKind::Add | ReplayStepKind::AddFile => "to",
        ReplayStepKind::Change => "in",
        ReplayStepKind::Remove => "from",
    }
}

fn named_change_title(kind: ReplayStepKind, name: &str, container: &str) -> String {
    format!(
        "{} {name} {} {container}",
        replay_action(kind),
        replay_container_preposition(kind),
    )
}

fn named_change_task(kind: ReplayStepKind, name: &str, noun: &str, container: &str) -> String {
    format!(
        "{} the {name} {noun} {} {container}.",
        replay_action(kind),
        replay_container_preposition(kind),
    )
}

fn replay_rationale(session: &ReplaySession, step: &ReplayStep, path: &str) -> String {
    if let Some(documentation) = added_documentation(step) {
        return bounded_rationale(&documentation);
    }

    if let Some(context) = session.source.review_context.as_ref() {
        if let Some(description) = review_body_rationale(&context.body, step, path) {
            return bounded_rationale(&description);
        }

        for commit in &context.commits {
            if let Some(description) = review_body_rationale(&commit.body, step, path) {
                return bounded_rationale(&description);
            }
            if !commit.headline.trim().is_empty() {
                return bounded_rationale(commit.headline.trim());
            }
        }
    }

    format!(
        "Study the original {} hunk in {path} before reconstructing it.",
        replay_kind(step.kind),
    )
}

fn bounded_rationale(text: &str) -> String {
    text.chars().take(MAX_REPLAY_RATIONALE_CHARS).collect()
}

fn changed_source_lines(step: &ReplayStep) -> Vec<&str> {
    let (original, changed) = if step.kind == ReplayStepKind::Remove {
        (&step.after, &step.before)
    } else {
        (&step.before, &step.after)
    };
    let mut remaining = HashMap::<&str, usize>::new();
    for line in original.lines() {
        *remaining.entry(line).or_default() += 1;
    }

    let mut result = Vec::new();
    for line in changed.lines() {
        match remaining.get_mut(line) {
            Some(count) if *count > 0 => *count -= 1,
            _ => result.push(line),
        }
    }
    result
}

fn source_symbol(heading: &str) -> Option<(&str, &str)> {
    let mut words =
        heading.split(|character: char| !character.is_ascii_alphanumeric() && character != '_');

    while let Some(word) = words.next() {
        if matches!(word, "struct" | "enum" | "trait" | "fn" | "impl") {
            let name = words.find(|candidate| is_source_identifier(candidate))?;
            return Some((word, name));
        }
    }

    None
}

fn is_source_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    matches!(characters.next(), Some(character) if character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn changed_field(step: &ReplayStep) -> Option<(&str, &str)> {
    let (kind, container) = source_symbol(&step.heading)?;
    if !matches!(kind, "struct" | "enum") {
        return None;
    }

    changed_source_lines(step).into_iter().find_map(|line| {
        let line = line.trim();
        if line.starts_with("//") || line.starts_with('#') || line.starts_with('*') {
            return None;
        }

        let (declaration, _) = line.split_once(':')?;
        if declaration.contains(':') {
            return None;
        }
        let field = declaration.split_whitespace().last()?;
        is_source_identifier(field).then_some((field, container))
    })
}

fn changed_function(step: &ReplayStep) -> Option<&str> {
    changed_source_lines(step).into_iter().find_map(|line| {
        let (kind, name) = source_symbol(line.trim())?;
        (kind == "fn").then_some(name)
    })
}

fn changed_binding(step: &ReplayStep) -> Option<&str> {
    changed_source_lines(step).into_iter().find_map(|line| {
        let declaration = line.trim().strip_prefix("let ")?.trim_start();
        let declaration = declaration.strip_prefix("mut ").unwrap_or(declaration);
        let (name, _) = declaration.split_once('=')?;
        let name = name.split_once(':').map_or(name, |(name, _)| name).trim();
        (name != "_" && is_source_identifier(name)).then_some(name)
    })
}

fn changed_module(step: &ReplayStep) -> Option<&str> {
    changed_source_lines(step).into_iter().find_map(|line| {
        let mut words = line
            .trim()
            .split(|character: char| !character.is_ascii_alphanumeric() && character != '_');
        while let Some(word) = words.next() {
            if word == "mod" {
                let name = words.find(|candidate| is_source_identifier(candidate))?;
                return Some(name);
            }
        }
        None
    })
}

fn inline_code_spans(text: &str) -> impl Iterator<Item = &str> {
    text.lines().flat_map(|line| {
        line.split('`')
            .enumerate()
            .filter_map(|(index, span)| (index % 2 == 1).then_some(span.trim()))
            .filter(|span| !span.is_empty())
    })
}

fn changed_markdown_subject(step: &ReplayStep) -> Option<&str> {
    let (original, changed) = if step.kind == ReplayStepKind::Remove {
        (&step.after, &step.before)
    } else {
        (&step.before, &step.after)
    };
    let mut remaining = HashMap::<&str, usize>::new();
    for span in inline_code_spans(original) {
        *remaining.entry(span).or_default() += 1;
    }

    inline_code_spans(changed).find_map(|span| {
        if let Some(count) = remaining.get_mut(span) {
            if *count > 0 {
                *count -= 1;
                return None;
            }
        }

        let name = span
            .split(|character: char| character == ':' || character.is_ascii_whitespace())
            .next()?;
        is_source_identifier(name).then_some(name)
    })
}

fn markdown_endpoint(step: &ReplayStep) -> Option<&str> {
    inline_code_spans(&step.heading)
        .chain(inline_code_spans(&step.after))
        .find(|span| span.starts_with("thread/") && !span.chars().any(char::is_whitespace))
}

fn added_documentation(step: &ReplayStep) -> Option<String> {
    let mut lines = Vec::new();

    for line in changed_source_lines(step) {
        let line = line.trim();
        if let Some(documentation) = line
            .strip_prefix("///")
            .or_else(|| line.strip_prefix("//!"))
        {
            let documentation = documentation.trim();
            if !documentation.is_empty() {
                lines.push(documentation);
            }
        } else if !lines.is_empty() {
            break;
        }
    }

    (!lines.is_empty()).then(|| lines.join(" "))
}

fn review_body_rationale(body: &str, step: &ReplayStep, path: &str) -> Option<String> {
    let keywords = replay_semantic_tokens(step, path);
    let mut section = ReviewSection::Other;
    let mut fenced = false;
    let mut motivation = String::new();
    let mut first_prose = String::new();
    let mut best_change: Option<(usize, String)> = None;

    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }

        if let Some(heading) = markdown_heading(trimmed) {
            section = review_section(heading);
            continue;
        }

        if trimmed.is_empty()
            || trimmed.starts_with("<!--")
            || trimmed.starts_with("-->")
            || trimmed.starts_with("![")
        {
            continue;
        }

        if let Some(bullet) = markdown_bullet(trimmed) {
            if section == ReviewSection::Changes {
                let score = semantic_tokens(bullet).intersection(&keywords).count();
                if score >= 2
                    && best_change
                        .as_ref()
                        .is_none_or(|(best_score, _)| score > *best_score)
                {
                    best_change = Some((score, bullet.trim_matches('`').trim().to_string()));
                }
            }
            continue;
        }

        if section == ReviewSection::Motivation && motivation.is_empty() {
            motivation = trimmed.to_string();
        }
        if first_prose.is_empty() {
            first_prose = trimmed.to_string();
        }
    }

    best_change
        .map(|(_, change)| change)
        .or_else(|| (!motivation.is_empty()).then_some(motivation))
        .or_else(|| (!first_prose.is_empty()).then_some(first_prose))
}

fn markdown_heading(line: &str) -> Option<&str> {
    let heading = line.strip_prefix('#')?;
    let heading = heading.trim_start_matches('#');
    if !heading.starts_with(char::is_whitespace) {
        return None;
    }
    let heading = heading.trim().trim_end_matches('#').trim();
    (!heading.is_empty()).then_some(heading)
}

fn review_section(heading: &str) -> ReviewSection {
    let heading = heading.to_ascii_lowercase();
    if matches!(
        heading.as_str(),
        "why" | "motivation" | "background" | "problem" | "context" | "overview"
    ) {
        ReviewSection::Motivation
    } else if matches!(
        heading.as_str(),
        "what changed" | "what changes" | "changes" | "implementation" | "solution"
    ) {
        ReviewSection::Changes
    } else {
        ReviewSection::Other
    }
}

fn markdown_bullet(line: &str) -> Option<&str> {
    line.strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.strip_prefix("+ "))
        .map(str::trim)
        .filter(|bullet| !bullet.is_empty())
}

fn replay_semantic_tokens(step: &ReplayStep, path: &str) -> HashSet<String> {
    let mut source = String::new();
    source.push_str(&step.heading);
    source.push(' ');
    source.push_str(path);

    for line in changed_source_lines(step)
        .into_iter()
        .take(MAX_SEMANTIC_SOURCE_LINES)
    {
        let remaining = MAX_SEMANTIC_SOURCE_CHARS.saturating_sub(source.len());
        if remaining == 0 {
            break;
        }
        source.push(' ');
        for character in line.chars() {
            if source.len().saturating_add(character.len_utf8()) > MAX_SEMANTIC_SOURCE_CHARS {
                break;
            }
            source.push(character);
        }
    }

    semantic_tokens(&source)
}

fn semantic_tokens(text: &str) -> HashSet<String> {
    let mut tokens = HashSet::new();
    let mut current = String::new();
    let mut previous_lowercase = false;

    for character in text.chars().take(MAX_SEMANTIC_SOURCE_CHARS) {
        if character.is_ascii_alphanumeric() {
            if character.is_ascii_uppercase() && previous_lowercase {
                push_semantic_token(&mut tokens, &mut current);
            }
            current.push(character.to_ascii_lowercase());
            previous_lowercase = character.is_ascii_lowercase();
        } else {
            push_semantic_token(&mut tokens, &mut current);
            previous_lowercase = false;
        }
    }
    push_semantic_token(&mut tokens, &mut current);

    tokens
}

fn push_semantic_token(tokens: &mut HashSet<String>, current: &mut String) {
    if current.len() >= 3
        && !matches!(
            current.as_str(),
            "add"
                | "and"
                | "are"
                | "change"
                | "changes"
                | "for"
                | "from"
                | "let"
                | "new"
                | "pub"
                | "src"
                | "the"
                | "this"
                | "use"
                | "with"
        )
    {
        tokens.insert(std::mem::take(current));
    } else {
        current.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::{
        digest, replay_demo_plan, GitObjectId, ReplayRepository, ReplayReviewContext, ReplaySource,
        ReplaySourceKind, ReplayWorkspace,
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

    const DOCUMENTED_FIELD_PATCH: &str = concat!(
        "diff --git a/src/token.rs b/src/token.rs\n",
        "index 1111111..2222222 100644\n",
        "--- a/src/token.rs\n",
        "+++ b/src/token.rs\n",
        "@@ -1,4 +1,8 @@ pub struct ThreadResumeParams {\n",
        " pub struct ThreadResumeParams {\n",
        "     pub exclude_turns: bool,\n",
        "+    /// Replay cached token usage after the response without loading all thread turns.\n",
        "+    #[experimental(\"thread/resume.restoreTokenUsage\")]\n",
        "+    #[serde(default, skip_serializing_if = \"std::ops::Not::not\")]\n",
        "+    pub restore_token_usage: bool,\n",
        "     pub initial_turns_page: Option<usize>,\n",
        " }\n",
    );

    const UNDOCUMENTED_FIELD_PATCH: &str = concat!(
        "diff --git a/src/token.rs b/src/token.rs\n",
        "index 1111111..2222222 100644\n",
        "--- a/src/token.rs\n",
        "+++ b/src/token.rs\n",
        "@@ -1,4 +1,5 @@ pub struct ThreadResumeParams {\n",
        " pub struct ThreadResumeParams {\n",
        "     pub exclude_turns: bool,\n",
        "+    pub restore_token_usage: bool,\n",
        "     pub initial_turns_page: Option<usize>,\n",
        " }\n",
    );

    fn codex_review_context(body: &str) -> ReplayReviewContext {
        ReplayReviewContext {
            title: "feat(tui): paginate session history by scrollback budget".to_string(),
            body: body.to_string(),
            author: Some("fcoury-oai".to_string()),
            commits: Vec::new(),
            changed_files: 21,
        }
    }

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
    fn real_plan_explains_the_actual_added_field_instead_of_its_raw_hunk_heading() {
        let (directory, mut session) = source_session(DOCUMENTED_FIELD_PATCH);
        std::fs::write(
            directory.path().join("src/token.rs"),
            concat!(
                "pub struct ThreadResumeParams {\n",
                "    pub exclude_turns: bool,\n",
                "    pub initial_turns_page: Option<usize>,\n",
                "}\n",
            ),
        )
        .expect("write the real original thread-resume source");
        session.source.review_context = Some(codex_review_context(concat!(
            "## Why\n\n",
            "Resuming a long session must not replay more history than the terminal can retain.\n\n",
            "## What changed\n\n",
            "- Add experimental restoreTokenUsage support so token totals remain available.\n",
        )));

        let plan = replay_plan_from_session(
            &session,
            "fcoury/tui-paginated-history",
            ReplayLimits::default(),
        )
        .expect("compile a source-grounded learning step");
        let step = &plan.steps[0];

        assert_eq!(step.title, "Add restore_token_usage to ThreadResumeParams");
        assert_eq!(
            step.why,
            "Replay cached token usage after the response without loading all thread turns.",
        );
        assert_eq!(
            step.task,
            "Add the restore_token_usage field to ThreadResumeParams.",
        );
        assert_eq!(
            step.hint,
            "At src/token.rs:1, add the `restore_token_usage` field to `ThreadResumeParams`; use the neighboring original fields as your anchor.",
        );
        assert!(!step.why.starts_with('#'));
    }

    #[test]
    fn real_plan_skips_markdown_headings_when_extracting_pull_request_rationale() {
        let (_directory, mut session) = source_session(MULTI_FILE_PATCH);
        let rationale =
            "Resuming a long session must not replay more history than the terminal can retain.";
        session.source.review_context = Some(codex_review_context(&format!(
            "## Why\n\n{rationale}\n\n## What changed\n\n- Update the implementation.\n",
        )));

        let plan = replay_plan_from_session(&session, "feature/replay", ReplayLimits::default())
            .expect("compile a Markdown-aware replay plan");

        assert_eq!(plan.steps[0].why, rationale);
        assert!(plan.steps.iter().all(|step| !step.why.starts_with('#')));
    }

    #[test]
    fn real_plan_prefers_the_relevant_author_written_change_description() {
        let (directory, mut session) = source_session(UNDOCUMENTED_FIELD_PATCH);
        std::fs::write(
            directory.path().join("src/token.rs"),
            concat!(
                "pub struct ThreadResumeParams {\n",
                "    pub exclude_turns: bool,\n",
                "    pub initial_turns_page: Option<usize>,\n",
                "}\n",
            ),
        )
        .expect("write the original thread-resume source");
        let relevant = "Add experimental restoreTokenUsage support so token totals remain available without materializing the entire transcript.";
        session.source.review_context = Some(codex_review_context(&format!(
            concat!(
                "## Why\n\n",
                "Long sessions should only load history that the terminal can display.\n\n",
                "## What changed\n\n",
                "- Preserve the visible transcript scroll anchor.\n",
                "- {}\n",
                "- Keep the terminal history budget bounded.\n",
            ),
            relevant,
        )));

        let plan = replay_plan_from_session(
            &session,
            "fcoury/tui-paginated-history",
            ReplayLimits::default(),
        )
        .expect("compile a change-specific author-written rationale");

        assert_eq!(plan.steps[0].why, relevant);
    }

    #[test]
    fn real_plan_distinguishes_the_actual_resume_and_fork_documentation_changes() {
        let (_directory, session) = source_session(MULTI_FILE_PATCH);
        let mut resume = session.steps[0].clone();
        resume.heading = "Valid `personality` values are friendly and pragmatic.".to_string();
        resume.before = concat!(
            "By default, `thread/resume` skips restored ",
            "`thread/tokenUsage/updated`.\n",
        )
        .to_string();
        resume.after = concat!(
            "By default, `thread/resume` skips restored ",
            "`thread/tokenUsage/updated` unless `restoreTokenUsage: true` is provided.\n",
        )
        .to_string();

        assert_eq!(
            replay_title(&resume, "codex-rs/app-server/README.md"),
            "Document thread/resume restoreTokenUsage",
        );
        assert_eq!(
            replay_hint(&resume, "codex-rs/app-server/README.md"),
            "At codex-rs/app-server/README.md:1, update the `thread/resume` documentation for `restoreTokenUsage`; keep the surrounding original explanation intact.",
        );

        let mut fork = resume.clone();
        fork.heading = "To branch from a stored session, call `thread/fork`.".to_string();
        fork.before = concat!(
            "Like `thread/resume`, call `thread/fork` ",
            "without replaying `thread/tokenUsage/updated`.\n",
        )
        .to_string();
        fork.after = concat!(
            "Like `thread/resume`, call `thread/fork` ",
            "without replaying `thread/tokenUsage/updated` ",
            "unless `restoreTokenUsage: true` is provided.\n",
        )
        .to_string();

        assert_eq!(
            replay_title(&fork, "codex-rs/app-server/README.md"),
            "Document thread/fork restoreTokenUsage",
        );
        assert_eq!(
            replay_hint(&fork, "codex-rs/app-server/README.md"),
            "At codex-rs/app-server/README.md:1, update the `thread/fork` documentation for `restoreTokenUsage`; keep the surrounding original explanation intact.",
        );
    }

    #[test]
    fn real_plan_distinguishes_relocated_bindings_within_the_same_function() {
        let (_directory, session) = source_session(MULTI_FILE_PATCH);
        let mut removal = session.steps[0].clone();
        removal.kind = ReplayStepKind::Remove;
        removal.heading = "pub(super) async fn handle_pending_thread_resume_request(".to_string();
        removal.before = concat!(
            "    let token_usage_turn_id = pending.include_turns;\n",
            "    continue_resume();\n",
        )
        .to_string();
        removal.after = "    continue_resume();\n".to_string();

        assert_eq!(
            replay_title(&removal, "src/thread_lifecycle.rs"),
            "Remove token_usage_turn_id from handle_pending_thread_resume_request",
        );
        assert_eq!(
            replay_task(&removal, "src/thread_lifecycle.rs"),
            "Remove the token_usage_turn_id binding from handle_pending_thread_resume_request.",
        );
        assert_eq!(
            replay_hint(&removal, "src/thread_lifecycle.rs"),
            "At src/thread_lifecycle.rs:1, remove the `token_usage_turn_id` binding from `handle_pending_thread_resume_request`; use its unchanged neighboring statements as your anchor.",
        );

        let mut addition = removal.clone();
        addition.kind = ReplayStepKind::Add;
        addition.before = "    continue_resume();\n".to_string();
        addition.after = concat!(
            "    let token_usage_turn_id = pending.restore_token_usage;\n",
            "    continue_resume();\n",
        )
        .to_string();

        assert_eq!(
            replay_title(&addition, "src/thread_lifecycle.rs"),
            "Add token_usage_turn_id to handle_pending_thread_resume_request",
        );
        let hint = replay_hint(&addition, "src/thread_lifecycle.rs");
        assert!(hint.contains("`token_usage_turn_id`"));
        assert!(hint.contains("`handle_pending_thread_resume_request`"));
        assert!(!hint.contains("pending.restore_token_usage"));
    }

    #[test]
    fn real_plan_identifies_functions_added_inside_existing_implementations() {
        let (_directory, session) = source_session(MULTI_FILE_PATCH);
        let mut step = session.steps[0].clone();
        step.kind = ReplayStepKind::Add;
        step.heading = "impl TranscriptOverlay {".to_string();
        step.before = "impl TranscriptOverlay {\n}\n".to_string();
        step.after = concat!(
            "impl TranscriptOverlay {\n",
            "    /// Return whether the transcript needs an older history page.\n",
            "    pub(crate) fn should_load_older(&self) -> bool {\n",
            "        true\n",
            "    }\n",
            "}\n",
        )
        .to_string();

        assert_eq!(
            replay_title(&step, "src/pager_overlay.rs"),
            "Add should_load_older to TranscriptOverlay",
        );
        assert_eq!(
            replay_task(&step, "src/pager_overlay.rs"),
            "Add the should_load_older function to TranscriptOverlay.",
        );
        assert_eq!(
            replay_hint(&step, "src/pager_overlay.rs"),
            "At src/pager_overlay.rs:1, add `should_load_older` to `TranscriptOverlay`; preserve the surrounding original implementation.",
        );
    }

    #[test]
    fn real_plan_keeps_new_file_names_visible_in_narrow_panels() {
        let (_directory, session) = source_session(MULTI_FILE_PATCH);
        let mut step = session.steps[0].clone();
        step.kind = ReplayStepKind::AddFile;
        step.heading.clear();
        step.before.clear();
        step.after =
            "//! Load older transcript pages without rewriting terminal scrollback.\n".to_string();

        assert_eq!(
            replay_title(&step, "codex-rs/tui/src/app/history_pagination.rs"),
            "Add history_pagination.rs",
        );
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
            assert!(step.hint.starts_with(&format!("At {}:", step.path)));
            assert!(!step.hint.contains("diff --git"));
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
    fn repeated_hunk_context_follows_original_lines_and_prior_line_changes() {
        const PATCH: &str = concat!(
            "diff --git a/src/token.rs b/src/token.rs\n",
            "index 1111111..2222222 100644\n",
            "--- a/src/token.rs\n",
            "+++ b/src/token.rs\n",
            "@@ -1,3 +1,4 @@ prepare first occurrence\n",
            "+// prepare the original first occurrence\n",
            " fn repeated() {\n",
            "     old();\n",
            " }\n",
            "@@ -5,3 +6,3 @@ update second occurrence\n",
            " fn repeated() {\n",
            "-    old();\n",
            "+    new();\n",
            " }\n",
        );

        let (directory, session) = source_session(PATCH);
        let original = "fn repeated() {\n    old();\n}\n\nfn repeated() {\n    old();\n}\n";
        std::fs::write(directory.path().join("src/token.rs"), original)
            .expect("identical original source contexts");

        let plan = replay_plan_from_session(&session, "feature/replay", ReplayLimits::default())
            .expect("the exact original hunk lines disambiguate repeated source");

        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].before, original);
        assert_eq!(
            plan.steps[0].after,
            concat!(
                "// prepare the original first occurrence\n",
                "fn repeated() {\n    old();\n}\n\n",
                "fn repeated() {\n    old();\n}\n",
            ),
        );
        assert_eq!(plan.steps[1].before, plan.steps[0].after);
        assert_eq!(
            plan.steps[1].after,
            concat!(
                "// prepare the original first occurrence\n",
                "fn repeated() {\n    old();\n}\n\n",
                "fn repeated() {\n    new();\n}\n",
            ),
        );
    }

    #[test]
    fn real_presentation_preserves_each_exact_original_independent_hunk() {
        let (_directory, session) = source_session(MULTI_FILE_PATCH);
        let limits = ReplayLimits::default();
        let plan = replay_plan_from_session(&session, "feature/replay", limits)
            .expect("compile the full editor-owned source plan");

        let presentation = replay_presentation_plan(&plan, limits)
            .expect("compile bounded reviewer-visible original hunks");

        assert_eq!(presentation.source_path, "src/token.rs");
        assert_eq!(presentation.steps.len(), plan.steps.len());
        for (presented, original) in presentation.steps.iter().zip(&plan.steps) {
            assert_eq!(presented.id, original.id);
            assert_eq!(presented.path, original.path);
            assert_eq!(presented.diff, original.diff);

            let patch = parse_patch(&presented.diff, limits)
                .expect("presentation retains the complete independent original hunk");
            let hunk = &patch.files[0].hunks[0];
            assert_eq!(presented.before, hunk.before);
            assert_eq!(presented.after, hunk.after);
        }
        assert!(presentation.steps[0].before.contains("old_first()"));
        assert!(!presentation.steps[0].before.contains("old_second()"));
        assert!(presentation.steps[1].after.contains("new_second()"));
        assert!(!presentation.steps[1].after.contains("new_first()"));
    }

    #[test]
    fn large_source_images_never_exhaust_the_bounded_replay_presentation() {
        let mut plan = replay_demo_plan().expect("original source-backed demo hunks");
        let unchanged_source = "// unrelated original source stays in the editor\n".repeat(30_000);
        plan.initial_source = format!("{unchanged_source}{}", plan.initial_source);
        for step in &mut plan.steps {
            step.before = format!("{unchanged_source}{}", step.before);
            step.after = format!("{unchanged_source}{}", step.after);
        }
        let limits = ReplayLimits::default();
        let complete_plan = serde_json::to_vec(&plan).expect("serialize full editor-owned images");
        assert!(complete_plan.len() > limits.max_patch_bytes);

        let presentation = replay_presentation_plan(&plan, limits)
            .expect("large source files must still produce a bounded real reviewer guide");
        let bytes = serde_json::to_vec(&presentation).expect("serialize bounded original hunks");

        assert!(bytes.len() < 64 * 1024);
        assert_eq!(presentation.steps.len(), plan.steps.len());
        for (presented, original) in presentation.steps.iter().zip(&plan.steps) {
            assert_eq!(presented.diff, original.diff);
            assert!(!presented.before.contains("unrelated original source"));
            assert!(!presented.after.contains("unrelated original source"));
        }
    }

    #[test]
    fn real_presentation_refuses_a_hunk_for_an_unrelated_source_file() {
        let mut plan = replay_demo_plan().expect("original source-backed demo hunks");
        plan.steps[0].path = "src/unrelated.rs".to_string();

        assert!(matches!(
            replay_presentation_plan(&plan, ReplayLimits::default()),
            Err(ReplayError::InvalidPatch(_)),
        ));
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
