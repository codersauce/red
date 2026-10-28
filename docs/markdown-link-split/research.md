# Reusable Markdown link destination: research

Source review at `d3a3c958`, 2026-10-28. These findings informed the [accepted implementation plan](../MARKDOWN_LINK_SPLIT_PLAN.md).

## UX precedents

- [Vim preview windows](https://github.com/vim/vim/blob/master/runtime/doc/windows.txt#L847-L938) keep a reusable destination and retain the source cursor. This is a close precedent for reading a report while inspecting references, although Vim scopes its preview to a tab rather than to each Markdown pane.
- [VS Code preview mode](https://code.visualstudio.com/docs/editing/getting-started/userinterface#_preview-mode) reuses a visibly marked editor until the user commits to it, such as by editing it. It supports the value of reuse but does not establish that Red needs a new disposable buffer type.
- [VS Code locked editor groups](https://code.visualstudio.com/docs/configure/custom-layout#_locked-editor-groups) make persistent opening rules visible and reversible. Red should likewise expose which source has a link destination and how to clear it.

**Agreed focus behavior:** retain source focus by default for repeated reference browsing, with configuration to focus the destination instead. A normal editable Red pane can serve as the destination; a special preview-buffer lifecycle is not required to preserve unsaved contents.

## Fit with Red

- [Markdown preparation](../../src/editor/markdown_links.rs) already validates a destination before changing buffers. It produces three local action forms: same-document movement, file opening, and opening a source location. A private prepared destination should normalize all three before routing to a split.
- [Window state](../../src/window.rs) has stable `WindowId`, independent cursors, viewports, and jump lists. A source-to-destination map should use stable window identities. Two panes showing the same Markdown buffer can then remember independent destinations.
- [Editor window activation](../../src/editor.rs) already saves and restores pane state. Restoring the source by its window identity avoids accidentally restoring a buffer-global remembered cursor instead.
- Existing split creation focuses the new pane and clones the source's history. Keeping source focus therefore requires an explicit target-history policy; ordinary FollowLink tracking will not record a move after focus returns to the source.
- Layout changes already prune window-associated state. Closure and layout restoration must invalidate remembered targets. Restored windows receive fresh identities.
- [OpenLocationTarget](../../src/plugin/location.rs) is a serialized plugin boundary. Editor-private routing can support this feature without adding a host API version or changing that enum.
- [Configuration](../../src/config.rs) uses typed snake_case enums for similar choices. A setting must be added to `Config`, `default_config.toml`, and the recoverable loader's known-field list. Configuration loads at startup; the existing `:languages reload` path only applies language-related settings. A focus setting does not imply general configuration hot reload.
- The smallest useful scope creates a destination only after successful local-link resolution and reuses it thereafter. An existing-pane chooser is additional interaction and assignment policy, not a prerequisite. The focus preference should apply to both creation and reuse; external URLs and failed resolutions must not create or retarget a split.
- If routing is session-only, existing restoration may still restore ordinary panes and buffers. It must leave them without remembered link associations because restored window identities are newly allocated.
- The simplest proposed lifecycle stores `source WindowId → {source BufferId, destination WindowId}`. Reconcile after synchronized state changes and validate on use. Closing either pane or changing the source buffer clears the route; retaining routes keyed by both source window and old buffer would revive them unexpectedly when switching back.
- Keeping the target assigned through manual buffer changes also preserves assignment through `Ctrl-O`/`Ctrl-I`. Automatically distinguishing manual opens from history would add command-specific exceptions. Explicit clear avoids that complexity.
- Start a link destination with fresh history, since normal split creation clones source history. Record the destination's previous location once after each successful reuse, and suppress outer FollowLink tracking in both focus modes. Otherwise destination-focus mode can add the source location or duplicate a jump. Active-window plugin notifications cannot reliably detect target updates made before restoring source focus.

## Agreed boundaries

- A first split changes the document's width and may rewrap text. Preserve its source cursor and context; exact screen geometry cannot stay unchanged.
- Following from a Markdown destination should use that pane's own association. Routing should take one hop, without recursively following through another destination's routing rule.
- Reusing a pane can hide an edited buffer while preserving it in Red. Editing or manually changing the destination does not clear its role; explicit clearing or pane closure does.
- Same-document headings use the destination, external URLs use the existing opener, source-buffer changes clear the association, and Ctrl-O/Ctrl-I use the destination's independent history.
