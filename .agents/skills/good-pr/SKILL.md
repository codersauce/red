---
name: good-pr
description: Create or update pull requests with accurate titles, context, and test instructions.
---

# Good Pull Requests

## Read the change first

Inspect the complete diff and any existing pull request body before changing PR metadata. Preserve useful existing context and assets. Describe the net change, not abandoned implementation attempts.

## Readiness

Open pull requests ready for review. Do not create a draft unless the user explicitly requests one. If an existing PR is a draft, mark it ready before finalizing unless the user asks to keep it as a draft.

## Title

Use Conventional Commit format when it accurately describes the change:

```text
<type>(<scope>): <lowercase imperative subject>
```

Keep the title concise and focused on the user-visible or reviewer-relevant outcome. Do not add product-name prefixes unless requested.

## Body

Explain why the change is needed before describing what changed. Keep the body limited to the PR's net effect and reference related issues or PRs when relevant. Use repository-relative paths rather than absolute local paths.

Include a `## How to Test` section with concrete steps and expected results. Cover the main success path and one focused regression or non-happy path when applicable. Name targeted automated tests when useful, but do not fill the section with generic CI commands.

## Automated release PRs

Release-preparation PRs must remain reviewable and must not publish on merge. State the requested version, summarize the generated version and changelog changes, and explain the separate tag action that starts publication.
