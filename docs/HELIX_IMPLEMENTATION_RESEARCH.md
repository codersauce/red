# Helix implementation research and validation design

This document supports the [implementation brief](HELIX_IMPLEMENTATION_PLAN.md). Findings come from static inspection of Red main at `c42adfff` and the reviewed implementation at `71ab60a6`; no implementation work or tests were performed during planning. Recommendations remain proposals until incorporated as agreed decisions in the brief.

## Reusable Red boundaries

- Red already uses half-open text ranges and scalar text coordinates. A directed selection model can use scalar gap offsets with explicit grapheme, display-column, and UTF-16 conversions.
- `src/editing/transaction.rs` returns `AppliedTextEdit`. `Editor::replace_range` records undo/LSP changes and invokes anchor transformation. These are the integration points for mapping every affected view's selections through edits.
- `execute_with_tracking` and the shared action path own publication, batching, and clipboard work. Helix input should resolve to actions; it should not mutate text while resolving a key.
- Existing multi-cursor sessions check stable buffer and window identities and apply multiple edits using signed shifts. Their occurrence-oriented selection type is not a ready-made general Helix selection model.
- Existing helpers still need semantic audits. In particular, existing multi-cursor Backspace removes one scalar, so reusing the path does not by itself establish grapheme-safe editing.
- Undo transactions currently store a single cursor snapshot. Full selection restoration needs recorded selection-set metadata. Pinned Helix restores that recorded selection into the view invoking undo, while other saved view selections follow the edit mappings; do not restrict restoration to the view that originally edited.
- Rope snapshots are cheap and available. Avoid flattening the complete document or rebuilding its grapheme index for every cursor movement.
- Vim's normal navigation excludes the empty line after a trailing newline. Helix's EOF policy must be applied consistently to movement, bounds, layout, pointer mapping, and restored selections without changing Vim's policy accidentally.

## Adjacent Vimlet work and extraction boundary

A local source refresh found separate Vimlet integration work; it is not part of the inspected Red baseline. `/Users/felipe.coury/code/red.fcoury-vimlet-integration`, branch `fcoury/vimlet-integration`, HEAD `4995f40`, contains both file-buffer and embedded-text adapters and locks Vimlet at `0e3a860`. `/Users/felipe.coury/code/vimlet`, branch `main`, HEAD `d91a187`, provides a revision-checked host-operation precedent. Both checkouts had existing changes and were left untouched. These are local observations, not claims about merged or released integration.

The inspected Vimlet API represents one cursor and an optional Vim Visual selection; its session state is Vim-shaped. Treat it as a source of host-boundary lessons, not a ready Helix multi-selection engine. The user accepted a separate private, optional workspace crate inside Red with later extraction in mind, without a publication lifecycle or migration of Vimlet consumers in Milestone 1. The agreed engine name is Felix and the Cargo package name is `felix-core`; the brief records the collision with the existing `felix` registry package.

Host acknowledgement is essential: requests identify buffer/view/revision, Red validates and applies them once, and only an accepted result finalizes dependent register, repeat, and session effects in the required order. Existing Red transactional replacement and notification paths remain the host integration seam. The crate owns Helix behavior; Red owns the authoritative document, committed view/session storage, history, and host effects. The user accepted this ownership split; the detailed protocol remains to be designed.

The protocol must distinguish pre-mutation rejection from downstream failure after commit. After text commits, Red must acknowledge the actual revision and committed selections even if notification or rendering fails, and surface that later failure separately. Tests should show that engine state remains synchronized and retrying a downstream effect does not replay the edit or duplicate undo history. This follows from the agreed single authoritative document and acknowledgement boundary.

For later extraction, keep modes, commands, directed selections, coordinate types, position mapping, edit plans, repeat state, and the acknowledgement protocol inside the engine. Its boundary can accept opaque identities, normalized keys, borrowed text access, register fragments, history-group instructions, and typed host requests/results. Red's concrete types and external clients stay in adapters; core tests and conformance fixtures must run independently of the Red application. A Ropey adapter can be optional if needed.

The concrete `foo` to `bar` trace adds three constraints: `c` must successfully preserve all yanked fragments before deleting; acknowledgements for deletion and each typed character must preserve one undo group; and saved selections are associated with a document/view pair. Other views are mapped through edits rather than assigned the invoking view's restored undo selection. Resumed operations also need an originating session/selection generation check because a document revision alone cannot detect changed focus or selections. Sources: [change-selection ordering](https://github.com/helix-editor/helix/blob/a05c151bb6e8e9c65ec390b0ae2afe7a5efd619b/helix-term/src/commands.rs#L2862), [history grouping](https://github.com/helix-editor/helix/blob/a05c151bb6e8e9c65ec390b0ae2afe7a5efd619b/helix-view/src/document.rs#L1546), and [per-view selections](https://github.com/helix-editor/helix/blob/a05c151bb6e8e9c65ec390b0ae2afe7a5efd619b/helix-view/src/document.rs#L1375).

Full per-document/per-view selection retention follows from the accepted exact reference contract, so the brief no longer treats it as a separate product choice. Document switching enters Normal mode; closing a view removes its saved selections; an unseen document/view pair starts at the first grapheme (or the empty position); an ordinary split of the current document copies its selections and scroll offset. These limits need explicit integration cases.

## Compatibility reference

The user agreed to Helix 25.07.1's default behavior for the agreed command set as the reference across both milestones, with explicitly approved Red bindings, native interfaces, and save-policy differences. Its tag resolves to `a05c151bb6e8e9c65ec390b0ae2afe7a5efd619b`. The reviewed branch used this version, but its captures do not establish correctness of the new implementation.

Pin the release/commit, platform artifact checksum, runtime, language settings, user configuration, terminal geometry, keyboard protocol, and capture schema. Do not change the reference configuration simply to eliminate a mismatch. Declare approved compatibility differences and test the actual default profile separately where relevant.

Isolate both user and project configuration: an empty user configuration alone does not prevent `.helix/config.toml` or language settings from affecting results. Defaults such as auto-pairs, smart-case search, formatting on save, and final-newline insertion must be considered when they affect an agreed workflow. The user explicitly chose Red's configured formatting and whitespace policy for saving in both modes. Save cases must declare that approved difference, verify Red's configured result, and check selection/history mapping; they must not alter the reference defaults to hide the exception.

The versioned source identifies consequential behaviors to encode in the command manifest and fixtures: visual-line `j/k` versus logical-line `gj/gk`; the distinction between yank registers and explicit system clipboard commands; auto-pairs; indentation; smart Tab; and scrolloff. Tests must verify their pinned semantics, not just dispatch from the expected key.

Basic inner/around word, WORD, paragraph, and explicitly named quote/bracket objects are in the agreed first milestone. Explicit delimiter selection uses a text-only resolver; closest-pair objects, syntax objects, and surround editing belong to the broader follow-up. Preserve the pinned semantics, including word-object counts being ignored in this release and the matching rules for symmetric delimiters. Sources: [text-object dispatch](https://github.com/helix-editor/helix/blob/a05c151bb6e8e9c65ec390b0ae2afe7a5efd619b/helix-term/src/commands.rs#L5942-L5973), [word objects](https://github.com/helix-editor/helix/blob/a05c151bb6e8e9c65ec390b0ae2afe7a5efd619b/helix-core/src/textobject.rs#L70-L110), and [explicit delimiter objects](https://github.com/helix-editor/helix/blob/a05c151bb6e8e9c65ec390b0ae2afe7a5efd619b/helix-core/src/textobject.rs#L200-L251).

Sources:

- [Helix 25.07.1 release](https://github.com/helix-editor/helix/releases/tag/25.07.1)
- [Default keymap implementation](https://raw.githubusercontent.com/helix-editor/helix/25.07.1/helix-term/src/keymap/default.rs)
- [Versioned keymap guide](https://raw.githubusercontent.com/helix-editor/helix/25.07.1/book/src/keymap.md)
- [Versioned register semantics](https://raw.githubusercontent.com/helix-editor/helix/25.07.1/book/src/registers.md)
- [Versioned editor configuration](https://raw.githubusercontent.com/helix-editor/helix/25.07.1/book/src/editor.md)
- [Versioned configuration loading](https://github.com/helix-editor/helix/blob/a05c151bb6e8e9c65ec390b0ae2afe7a5efd619b/book/src/configuration.md)
- [Versioned language configuration](https://github.com/helix-editor/helix/blob/a05c151bb6e8e9c65ec390b0ae2afe7a5efd619b/book/src/languages.md)

## Validation layers

The agreed staged rollout excludes Helix from initial ordinary releases, then includes it in the next planned ordinary release after M1 passes its full acceptance gate, with Vim still selected by default. M2 begins immediately after acceptance and does not wait for that release. The required validation matrix covers feature disabled with Vim; feature enabled with Vim; feature enabled with Helix. Test explicit Helix selection in a feature-disabled binary as a startup capability error before editing begins, and keep the disabled build covered after regular releases begin including Helix. Baseline `origin/main` has no Cargo features table or editing-mode selector; the plan retains the reviewed branch's `editing_model` spelling for the new implementation. Configuration recovery can default invalid fields, so merely compiling out an enum variant would risk silently falling back to Vim. An all-features check alone does not cover this disabled path.

One editing model applies to all document windows and is selected at startup; changing it requires restarting Red. Both milestones cover document windows only. Embedded composers and panel inputs keep their existing editing behavior, with Helix support separately scoped after M2. Live switching and mixed Vim/Helix document windows are likewise deferred to a separate proposal after M2.

| Layer | Observable result | Coverage |
| --- | --- | --- |
| Kernel invariants | Text, directed ranges, primary identity, edit mappings, and transaction boundaries. | Seeded generated sequences and bounded exhaustive cases over empty documents, EOF, Unicode graphemes, CRLF, tabs, and adjacent/overlapping selections. |
| Pinned Helix differential | State after each input, including intermediate Insert/prefix states, relevant registers, and undo results. | Prefer a test-only state snapshot adapter that does not save, flip selections, or otherwise mutate the state it observes. Cross-check representative cases with the released binary. |
| Production Red events | Text, each view's selections, actual cursor, mode, viewport, undo, and clipboard. | Composed edits, buffer/split switches, pointer plus keyboard, cancellation, search, and simultaneous insertion/deletion/paste. |
| Rendered frames and terminal input | Selected cells, all cursors, visible rows, focus changes, and decoded terminal events. | Frame assertions for Unicode, final lines, newline/EOF cells, wrapping, Markdown, and splits; a smaller PTY suite for modifiers, drag/autoscroll, bracketed paste, and Explore. |
| Live edit producers | Real protocol messages, coordinates, ownership, document versions, and selection rebasing. | A deterministic fake LSP server, delayed/cross-file responses, formatting/reference edits, file watching, and plugin/RPC edit paths. |
| Regression and performance | Existing Vim behavior, shared Red workflows, and scaling with document/selection size. | Run the existing regression suite; measure motion, rendering, batch edits, undo, and scrolling against explicit budgets to be agreed. |

## Preventing false parity

- The reviewed capture approach is unsuitable for intermediate Insert-state parity: its F12 write mapping leaves Insert mode, combines a previously read mode with later text, and substitutes a collapsed selection. Its full-selection probe also flips and rotates selections. The new observer must inspect state without these mutations, with a short multi-selection `c` → `b` → `a` → `r` → Escape → switch → undo trace proving observability early in implementation.
- Require matching case inventories; fail on missing, duplicate, stale, or unexpectedly skipped cases.
- Every supported command needs positive coverage where it changes observable state. Serverless jumps, a single open buffer, and an empty jumplist are edge cases, not sufficient navigation evidence.
- Compare actual cursor placement and rendered selection cells as well as internal ranges. A correct stored endpoint can still be displayed or edited at the wrong location.
- Record oracle and implementation failures separately. A capture failure is not a passing case or an editor mismatch.
- Retain failing seeds and traces as regressions, with implementation commit and oracle/config hashes.

## Mandatory milestone gate

The user accepted the brief's eight-part acceptance table as the mandatory canonical gate and explicitly requested a permanent side-by-side behavioral regression suite. Freeze the core command/workflow inventory, prove the observer early, then require no unexplained differences within the agreed contract, passing production-event and rendered integration cases, no unresolved correctness defects or Vim regressions, and no performance-budget failures. Record each approved binding exception explicitly rather than normalizing unexpected differences away.

Establish performance budgets from a clean baseline on a recorded machine/toolchain, using fixed corpus/workload hashes and repeated runs that expose variability. Check new multi-selection workload scaling early, before the architecture hardens. Final evidence must identify the exact M1 commit and link the command manifest, reference/configuration identity, feature matrix, generated-case seeds, integration/frame/terminal results, and baseline/candidate performance distributions.

The proposed shared-case runners compare pinned Helix with Felix's minimal test host and, where applicable, Red's production integration. Fixtures declare required observations and supported hosts; native UI differences stay explicit. Reports identify the first divergent event and retain both original and minimized repros. The user agreed to required deterministic live-reference comparisons on every PR and longer seeded nightly runs; the exact handoff commit must pass both suites. The existing path-sensitive CI planner must not suppress the required conformance result.

Milestone 2 begins immediately after the gate passes and expands command coverage under the same validation layers and agreed Helix 25.07.1 reference. Exact inventories, numerical performance budgets, and detailed report schema remain to be finalized.
