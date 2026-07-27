//! Source-backed, in-memory replay exercises used by the interactive UI preview.
//!
//! The demonstration deliberately never reads a repository or writes a file. Each
//! exercise still contains a complete before-image, after-image, and canonical
//! unified diff so the reviewer can genuinely reconstruct or apply one change.

use serde::{Deserialize, Serialize};
use similar::TextDiff;

use super::{digest, parse_patch, ReplayError, ReplayLimits};

const DEMO_SOURCE_PATH: &str = "src/editor/rendering.rs";
const DEMO_BASE: &str = "use std::collections::HashMap;\n\n#[derive(Clone, Debug, PartialEq, Eq)]\npub struct Diagnostic {\n    pub line: usize,\n    pub message: String,\n}\n\npub fn diagnostics_by_visible_line(\n    diagnostics: &[Diagnostic],\n) -> HashMap<usize, Vec<Diagnostic>> {\n    diagnostics\n        .iter()\n        .cloned()\n        .fold(HashMap::new(), |mut by_line, diagnostic| {\n            by_line\n                .entry(diagnostic.line)\n                .or_default()\n                .push(diagnostic);\n            by_line\n        })\n}\n";

/// A complete, locally generated original-author change for one demo exercise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayDemoStep {
    /// Stable original source-hunk identity.
    pub id: String,
    /// One-based reconstruction order.
    pub ordinal: usize,
    /// Safe relative path represented by the scratch source buffer.
    pub path: String,
    /// User-facing patch operation.
    pub kind: String,
    /// Concise description of the individual change.
    pub title: String,
    /// Original-author rationale for this reconstruction exercise.
    pub why: String,
    /// Concrete manual-reconstruction instruction.
    pub task: String,
    /// Optional graduated hint.
    pub hint: String,
    /// Exact complete source buffer before this step.
    pub before: String,
    /// Exact complete source buffer after this step.
    pub after: String,
    /// Complete, independently parseable Git-style unified diff.
    pub diff: String,
}

/// Original-author context and contiguous mock pull-request exercises.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayDemoPlan {
    /// Display-only original pull-request identity.
    pub pull_request: u64,
    /// Original pull-request title.
    pub title: String,
    /// Original pull-request author; never a review-submission identity.
    pub author: String,
    /// Original feature branch displayed by the coach.
    pub branch: String,
    /// Repository-relative target represented by the in-memory editor buffer.
    pub source_path: String,
    /// Source image at the mock original merge base.
    pub initial_source: String,
    /// Ordered, contiguous original-author unified hunks.
    pub steps: Vec<ReplayDemoStep>,
}

struct DemoSpec {
    kind: &'static str,
    title: &'static str,
    why: &'static str,
    task: &'static str,
    hint: &'static str,
    needle: &'static str,
    replacement: &'static str,
}

/// Constructs the deterministic, complete, parse-checked replay demonstration.
///
/// # Errors
///
/// Returns an error if a demo transformation loses its unique source anchor or a
/// generated unified hunk fails the same bounded parser used for real PRs.
pub fn replay_demo_plan() -> Result<ReplayDemoPlan, ReplayError> {
    let specs = [
        DemoSpec {
            kind: "add",
            title: "Capture the visible viewport",
            why: "Diagnostics must be evaluated against the actual visible editor rows.",
            task: "Add the inclusive visible-start and visible-end parameters to the grouping helper.",
            hint: "Keep the two bounds immediately after the diagnostic slice.",
            needle: "    diagnostics: &[Diagnostic],\n) ->",
            replacement: "    diagnostics: &[Diagnostic],\n    visible_start: usize,\n    visible_end: usize,\n) ->",
        },
        DemoSpec {
            kind: "add",
            title: "Filter diagnostics to the visible viewport",
            why: "Off-screen diagnostics should not occupy visible gutter or rendering space.",
            task: "Filter the iterator to diagnostic lines within the inclusive viewport.",
            hint: "An inclusive Rust range exposes a contains method.",
            needle: "        .iter()\n        .cloned()",
            replacement: "        .iter()\n        .filter(|diagnostic| {\n            (visible_start..=visible_end).contains(&diagnostic.line)\n        })\n        .cloned()",
        },
        DemoSpec {
            kind: "add",
            title: "Discard empty diagnostic messages",
            why: "Blank messages should not create misleading or empty visible indicators.",
            task: "Exclude diagnostics whose message becomes empty after trimming.",
            hint: "Filter on diagnostic.message.trim().is_empty().",
            needle: "        .filter(|diagnostic| {\n            (visible_start..=visible_end).contains(&diagnostic.line)\n        })\n        .cloned()",
            replacement: "        .filter(|diagnostic| {\n            (visible_start..=visible_end).contains(&diagnostic.line)\n        })\n        .filter(|diagnostic| !diagnostic.message.trim().is_empty())\n        .cloned()",
        },
        DemoSpec {
            kind: "add",
            title: "Declare the visible diagnostic limit",
            why: "A named limit documents the rendering contract and keeps future changes auditable.",
            task: "Declare MAX_VISIBLE_DIAGNOSTICS immediately above the grouping helper.",
            hint: "Use a usize constant with the value 250.",
            needle: "pub fn diagnostics_by_visible_line(\n",
            replacement: "const MAX_VISIBLE_DIAGNOSTICS: usize = 250;\n\npub fn diagnostics_by_visible_line(\n",
        },
        DemoSpec {
            kind: "add",
            title: "Bound visible rendering work",
            why: "A large diagnostic response must not monopolize an interactive render frame.",
            task: "Apply MAX_VISIBLE_DIAGNOSTICS to the filtered iterator before cloning.",
            hint: "Place take(MAX_VISIBLE_DIAGNOSTICS) after the message filter.",
            needle: "        .filter(|diagnostic| !diagnostic.message.trim().is_empty())\n        .cloned()",
            replacement: "        .filter(|diagnostic| !diagnostic.message.trim().is_empty())\n        .take(MAX_VISIBLE_DIAGNOSTICS)\n        .cloned()",
        },
    ];

    let mut source = DEMO_BASE.to_string();
    let mut steps = Vec::with_capacity(specs.len());
    for (index, spec) in specs.into_iter().enumerate() {
        if source.matches(spec.needle).count() != 1 {
            return Err(ReplayError::InvalidPatch(format!(
                "demo step {} has no unique source anchor",
                index + 1
            )));
        }
        let after = source.replacen(spec.needle, spec.replacement, 1);

        let diff = format!(
            "diff --git a/{DEMO_SOURCE_PATH} b/{DEMO_SOURCE_PATH}\n{}",
            TextDiff::from_lines(&source, &after)
                .unified_diff()
                .context_radius(3)
                .header(
                    &format!("a/{DEMO_SOURCE_PATH}"),
                    &format!("b/{DEMO_SOURCE_PATH}"),
                )
        );
        let parsed = parse_patch(&diff, ReplayLimits::default())?;
        if parsed.files.len() != 1 || parsed.files[0].hunks.is_empty() {
            return Err(ReplayError::InvalidPatch(
                "demo step did not produce a complete source hunk".to_string(),
            ));
        }
        let id = digest(diff.as_bytes());
        steps.push(ReplayDemoStep {
            id,
            ordinal: index + 1,
            path: DEMO_SOURCE_PATH.to_string(),
            kind: spec.kind.to_string(),
            title: spec.title.to_string(),
            why: spec.why.to_string(),
            task: spec.task.to_string(),
            hint: spec.hint.to_string(),
            before: source,
            after: after.clone(),
            diff,
        });
        source = after;
    }

    Ok(ReplayDemoPlan {
        pull_request: 482,
        title: "Render diagnostics only inside the visible viewport".to_string(),
        author: "original-author".to_string(),
        branch: "feat/viewport-diagnostics".to_string(),
        source_path: DEMO_SOURCE_PATH.to_string(),
        initial_source: DEMO_BASE.to_string(),
        steps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_steps_are_complete_independently_parseable_unified_diffs() {
        let plan = replay_demo_plan().unwrap();
        assert_eq!(plan.steps.len(), 5);
        for step in &plan.steps {
            let patch = parse_patch(&step.diff, ReplayLimits::default()).unwrap();
            assert_eq!(patch.files.len(), 1);
            assert_eq!(patch.files[0].hunks.len(), 1);
            assert!(step
                .diff
                .starts_with("diff --git a/src/editor/rendering.rs"));
            assert!(step.diff.contains("\n@@ "));
            assert_ne!(step.before, step.after);
        }
    }

    #[test]
    fn demo_steps_form_one_contiguous_manual_reconstruction() {
        let plan = replay_demo_plan().unwrap();
        assert_eq!(plan.steps[0].before, plan.initial_source);
        for adjacent in plan.steps.windows(2) {
            assert_eq!(adjacent[0].after, adjacent[1].before);
        }
        let last = plan.steps.last().unwrap();
        assert!(last.after.contains("MAX_VISIBLE_DIAGNOSTICS"));
        assert!(last.after.contains(".take(MAX_VISIBLE_DIAGNOSTICS)"));
    }

    #[test]
    fn demo_never_requires_a_repository_provider_or_local_file() {
        let plan = replay_demo_plan().unwrap();
        assert_eq!(plan.source_path, "src/editor/rendering.rs");
        assert_eq!(plan.pull_request, 482);
        assert!(plan
            .initial_source
            .contains("pub fn diagnostics_by_visible_line"));
        assert!(plan.steps.iter().all(|step| step.path == plan.source_path));
    }
}
