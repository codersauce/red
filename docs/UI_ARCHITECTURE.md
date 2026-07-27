# Terminal UI architecture

Red's terminal surfaces use a small set of concrete primitives. They do not form a
general-purpose widget framework: the editor, plugin panels, LSP completion, and agent
workspaces retain their own interaction, ownership, and security boundaries.

## Authoritative owners

- `src/editor/rendering.rs` owns complete-frame composition and window-aware,
  document-context-aware row rendering. Incremental text edits use the same cached
  highlighter and visible-row pipeline as a complete frame.
- `src/plugin/overlay.rs` composites every visible plugin overlay into every frame,
  ordered by its existing z-index. A clean overlay is still visible in the next newly
  allocated render buffer.
- `src/ui/dialog.rs` owns modal and popup border clipping, title and footer insets,
  optional rounded corners, and `Dialog` or `Popup` theme-role updates.
- `src/ui/prompt_buffer.rs` owns real, unnamed, rope-backed prompt buffers, grapheme
  cursors, Vim modes, actual undo history, bounded prompt history, and terminal-paste
  normalization. Floating and docked agent composers read their draft, cursor, and
  history directly from this buffer.
- `src/ui/selection.rs` owns selected-row viewport clamping and streaming
  `FollowTailViewport` state. Text panels preserve a manually interrupted tail rather
  than jumping to a newly streamed response.
- `src/ui/geometry.rs` owns half-open terminal-cell rectangles shared by pickers and
  plugin workspaces. Screen rectangles are not editor selections, buffer ranges, UTF-8
  byte offsets, or UTF-16 LSP positions.
- `src/ui/rich_text.rs` clips and paints the existing Markdown span model without
  splitting graphemes. Hover and text panels retain their own background, link,
  selection, and syntax-style policies.
- `src/ui/icons.rs` is the lookup boundary for file, symbol, and LSP-completion icons.
  Existing filename-before-extension priority, semantic file colors, ASCII, Unicode,
  Nerd Font, hidden-icon modes, and distinct LSP completion glyphs are preserved.
- `src/unicode_utils.rs` owns terminal-cell measurement and left- and right-clipped,
  marker-aware grapheme truncation.

Use `Action::Refresh` for a repaint that does not open a component. Preserve both
`Refresh` and `ShowDialog` in the public plugin host contract.

## Floating agent composer

The floating agent composer is deliberately **right-aligned**. Source code is usually
concentrated toward the left side of the editor; placing the prompt on the right keeps
that code visible while composing a request. Border repairs and shared geometry must not
center, move, or otherwise change this placement.

## Boundaries that remain separate

- LSP completion retains protocol-specific filtering, sorting, preselection, commit
  characters, cursor anchoring, and editor-event passthrough.
- `PanelSegment` and `WindowBarSegment` are public, separately serialized plugin wire
  contracts. Share painters, not their JSON representation.
- Row-panel selection, transcript following, and hover-link navigation are distinct
  interaction policies even when they share viewport primitives.
- Workspace access remains descriptor-relative, root-anchored, and symlink-safe.
  Geometry or preview reuse must not weaken filesystem confinement.
- Preferences, project configuration, agent approval, and staged file proposals retain
  their existing independent ownership and persistence lifecycles.

## Boundary-sensitive follow-up work

The reusable UI milestone intentionally does not merge unrelated architecture projects
into one change:

- Colon commands and interactive search share normalized first-line paste, but their
  command history, completion, and incremental-search state still have independent
  owners. A complete `PromptBuffer` migration needs dedicated cursor, history, and
  preview compatibility tests.
- File previews retain the existing bounded, cached, syntax-aware picker
  implementation. Extracting a separate preview service should first preserve FIFO
  rejection, UTF-8 boundaries, match overlays, cache invalidation, and source offsets.
- Husk plugins keep the existing versioned host contract. Exposing `IconCatalog` to
  external plugins requires an explicitly versioned schema, runtime dispatch, Husk
  declarations, and compatibility coverage.
- Strongly typed document coordinates, shared descriptor-safe workspace confinement,
  language-catalog consolidation, window and focus ownership, and larger native Husk
  migrations remain separate, reviewable projects. They must not be introduced through
  a terminal-UI refactor.

## Regression coverage

Run focused component and rendering checks while iterating:

```shell
CARGO_TARGET_DIR=/private/tmp/red-ui-unification-target cargo test --lib ui::
CARGO_TARGET_DIR=/private/tmp/red-ui-unification-target cargo test --lib plugin::
```

At the final integration boundary, run the workspace tests, formatting check, and the
repository-required all-target, all-feature Clippy command:

```shell
cargo fmt --check
cargo test --workspace --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
```
