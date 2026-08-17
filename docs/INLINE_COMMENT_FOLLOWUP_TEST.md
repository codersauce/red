# Manual checkpoint: ask about one inline comment

Automated end-to-end execution remains deferred. Use the current implementation
binary in the existing disposable Red-source worktree; preserve any rename edits
already made there:

```sh
cd /Users/felipe.coury/code/red.codex-inline-outcome-smoke
/Users/felipe.coury/code/red.codex-inline-assist-context/target/debug/red src/plugin/text_link.rs
```

1. Select `markdown_link_target` and use `Space i`:

   > Explain how this function distinguishes local file references from external
   > URLs. Leave two or three inline comments on the relevant branches. Do not
   > change the code.

2. Open one specific annotation with `Space v`, or click its text and press Enter. If
   comments overlap, click the `‹ 2/2 ›` arrows on the card, use `[ i` / `] i`
   in Normal mode, click the counter for a chooser, or use `h` / `l` in the full view. Cycling and reopening the
   same comment should place its viewer consistently, inside the source pane,
   with an intact border.
3. Press `i` (**ask inline**). The new dialog should identify the selected
   comment above an empty question. Ask:

   > Which edge case could make this particular branch misclassify a link?
   > Give one concrete example; do not edit the code.

4. The original annotation should remain. `Space H` should contain a separate
   discussion whose detail identifies the parent comment. Reopen it and confirm
   that “this particular branch” still has the correct context.
5. Open the original comment again and press `A` (**ask Agent**). The Agent
   composer should contain the exact comment, its location, historical source,
   and earlier discussion. Add a question after `My question:`. Nothing should
   be sent until you explicitly send it. Keep the `Red inline history reference`
   line if you want the resulting Agent changes linked to this new discussion.
6. Repeat with an existing unsent Agent draft. Decline replacement first and
   verify the draft survives; retry and accept. Merely staging the new draft
   must not add an empty History record.
7. Open another inline follow-up and immediately press Esc. It should close
   without a confirmation or saved draft. Type a question and press Esc again
   to verify the existing Delete / Edit / Save draft choices still work.

Optional: edit a commented source line, then ask about its outdated annotation.
The dialog should disclose that the source changed and use the current code as
its edit target. If you delete the entire target, it should offer Agent instead
of guessing which remaining lines to edit.
