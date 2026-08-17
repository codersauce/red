# Manual checkpoint: current inline items and explicit approval

Use the rebuilt implementation binary in the existing Red-source smoke worktree.
Preserve the rename edits already made there:

```sh
cd /Users/felipe.coury/code/red.codex-inline-outcome-smoke
/Users/felipe.coury/code/red.codex-inline-assist-context/target/debug/red src/plugin/text_link.rs
```

1. Select `markdown_link_target` and ask with `Space i`:

   > Explain the URL and local-file branches with two or three inline comments.
   > Do not change code.

2. Open one comment with `Space v`, press `i`, and ask for a simpler explanation.
   The original and follow-up should now overlap. Click their count to see the
   local chooser. Browse it, cancel once, then select the other explanation.
   Browsing alone must not switch the current card.
3. Click a card. Use Left/Right or `h`/`l`, Enter to expand, and Esc to return.
   Try `[i` and `]i` in Normal mode. Check that the highlighted connector follows
   the visible comment. `i`, `A`, `r`, `x`, and `d` must address that comment or
   its named discussion. Dismissing with `x` should select the next overlap.
4. Open the same file in a vertical split. Choose different overlapping comments
   in each pane, resize the split, and switch between panes. Each should retain
   its own current item. Scroll a card close to the viewport bottom and open it:
   the focused and full popups must leave the clicked card visible. Restore
   dismissed/resolved discussions through Space H.
5. Select `markdown_link_target` again and ask:

   > Rename the local variable `lowercase` to `lowercase_destination` throughout
   > this function. Preserve behavior and all existing changes.

   Keep the dialog open. Completion must leave the source unchanged. Enter opens
   the colored diff; Enter again does not apply it. Press `a` to apply. Check the
   Applied/unsaved summary, then open it and use `u` to undo while it is safe.
   Repeat once and choose `d` to verify the declined outcome.

Automated integration and E2E execution remains deferred until this review is
complete. This checkpoint does not save or reset the smoke worktree.
