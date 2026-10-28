# Reusable split for Markdown links

Keep a Markdown document visible while opening the link under the cursor in another editor split. Reuse that destination split for later link openings so the user can read a report and inspect its references without repeatedly replacing the report or creating more splits.

**In scope**

- Investigate the UX of a reusable link destination in Red's main editor.
- Open a Markdown link under the cursor in a split and remember that split as a destination for later link openings.
- Create a dedicated destination split on first use and reuse it for later local links from that source. The first version does not select an arbitrary existing pane.
- Each Markdown pane remembers its own destination. Opening links from another Markdown pane does not redirect the first pane's links.
- Remember destination associations only for the current editor session.
- Focus after opening a link in its destination is configurable. By default, keep focus in the source Markdown pane; users can choose to focus the destination instead.
- Develop a design and implementation plan that builds on the existing link-following and split-navigation behavior.

**Focus behavior**

**Keep source focus by default.** This supports reading a report and opening several references in sequence. The configurable alternative moves focus into the destination for immediate inspection or editing.

The setting is `markdown_link_split_focus = "source"`, with `"target"` as the alternative. It follows Red's existing TOML configuration, which is loaded at startup. Read the setting when opening a link rather than storing the focus preference in each remembered association.

For example, `gx` on a reference in `REPORT.md` updates its destination pane. With focus retained in the report, `j` can select the next reference immediately; moving focus to the opened file makes `j` navigate that file instead.

[Compare the two configuration options](markdown-link-split/focus-preview-standalone.html). The interactive mockup illustrates both behaviors with a destination already present; the first option is the agreed default. Its layout and status text are illustrative, not agreed UI. Keyboard and button behavior were checked; browser rendering could not be inspected because the preview host was unavailable.

[Research and implementation constraints](markdown-link-split/research.md) cover related editor behavior and Red's existing window, buffer, and history contracts. Creating the first split may rewrap the report; preserving its source cursor and context does not mean preserving identical screen geometry.

**First-version boundary**

**Create and reuse a dedicated split.** An explicit open-link-in-split action creates a destination on its first use and reuses that pane thereafter. Later local link openings from the source use the same destination.

This reuses Red's existing split creation and records the source and destination window identities. Both approaches need routing, focus, history, and closure handling; choosing an existing pane would add a selection or assignment command and rules for replacing associations.

**Associations last for the current session.** Existing session restoration may still restore ordinary panes and buffers, but it will not restore their link-routing association.

**Deferred**

- Selecting an existing pane as a destination. Revisit if users need to reuse a layout they have already arranged.
- Restoring link destinations across editor restarts. Revisit if repeatedly reestablishing destinations becomes a common interruption.

**Navigation**

- Add an explicit Open link in split action, available in the command palette and through the configurable `Ctrl-w x` default binding.
- The action creates a right-hand split after successful local-link resolution. Repeating it and using ordinary `gx` from that source reuse the remembered destination.
- Without a remembered destination, ordinary `gx` keeps its current behavior. External URLs continue to use the existing external opener.
- Apply the configured focus behavior to both initial creation and later reuse. Invalid links leave layout, focus, buffers, and history unchanged.
- Provide a Clear link destination command that removes the association while leaving the destination pane open.

**Lifecycle**

**Closing the destination ends routing.** Remove the association when either pane closes. Ordinary `gx` then resumes its existing behavior; Open link in split can establish a new destination. This avoids recreating a pane the user deliberately closed.

**Changing the source document ends routing.** A different document shown in that source pane does not inherit its destination. Switching back does not revive the cleared association.

**The destination keeps its role through normal use.** Editing, navigating history, or manually opening another file there does not release it. Clear link destination explicitly releases the pane. This avoids distinguishing manual buffer changes from history navigation across many editor commands.

**Preserve unsaved buffers and separate navigation histories.** Replacing the displayed target file uses normal buffer switching and retains edits. A new destination starts with fresh history. Subsequent successful link opens record its previous location once, so `Ctrl-O` and `Ctrl-I` walk references in that pane. The source's history stays unchanged with either focus setting.

**Implementation approach**

1. Extend [Markdown link preparation](../src/editor/markdown_links.rs) with an editor-private local destination representation that covers files, located files, and same-document anchors. Resolve and validate before changing buffers, layout, focus, or history. Keep the external opener path separate.
2. Add the split-opening action, palette entries, key binding, and typed focus configuration. Include the setting in the default configuration and known-field registry. Read it for every routed open.
3. Store a source window identity mapped to its source buffer identity and destination window identity. Reconcile identities after synchronized buffer/layout changes and validate before use. Keep this state out of session snapshots and plugin API schemas.
4. Route once into the destination using existing buffer and window operations. Record destination history explicitly and suppress generic source jump recording for routed opens. Restore the source by window identity when configured to retain focus. A Markdown destination can have its own route, but following from A to B does not automatically continue from B to C.
5. Add a small indication of an active association to the source statusline and expose the clear action in the palette. Document the command, focus setting, lifecycle, and restart behavior.

**Validation planned for implementation**

- Cover first creation and repeated reuse under both focus settings, independent sources, and two source panes showing the same buffer.
- Check local files, line/column links, same-document headings, and wrapped/table/Unicode link positions. Invalid links and external URLs must not create or retarget a split.
- Verify target `Ctrl-O`/`Ctrl-I`, unchanged source history, and preservation of unsaved target contents.
- Exercise source-document changes, target buffer switches, pane closure/reordering, nested routes, and restored layouts without associations.
- Check omitted/invalid configuration and unchanged ordinary `gx` without a route. Run focused tests, formatting, and strict Clippy; manually exercise the report workflow in tmux using the resulting binary.

**Open questions**

None. The user accepted the navigation and lifecycle and requested implementation on 2026-10-28.
