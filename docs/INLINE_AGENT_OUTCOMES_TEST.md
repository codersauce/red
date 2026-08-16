# Manual checkpoint: multi-file Agent outcomes

Automated end-to-end execution remains deferred. This is the human review plan.

## Safe workspace

A dedicated sibling worktree was created with `wt`:

```sh
cd /Users/felipe.coury/code/red.codex-inline-outcome-smoke
/Users/felipe.coury/code/red.codex-inline-assist-context/target/debug/red src/plugin/text_link.rs
```

The smoke branch starts at `8576339`; the executable comes from the implementation
worktree. Do not use `cargo run` in the smoke branch, since that would build the
older implementation. The rename below is disposable test work, not a requested
production change.

## Main exercise

1. Find `TextPanelFileLocation` near line 13 and select its declaration.
2. Press `Space i` and ask:

   > Rename the crate-internal Rust type `TextPanelFileLocation` to
   > `SourceFileLocation` everywhere in this project, including re-exports,
   > imports, construction sites, and tests. Preserve behavior and make no
   > unrelated changes. This requires multiple files; continue in Agent.

3. Choose **Continue in Agent**. Review the draft, retain its `Red inline history
   reference` line, and send it. If you already have an unsent Agent draft, first
   confirm that declining replacement preserves it; then retry the handoff.
4. While the Agent runs, navigate to another file. `Space H` should show the
   original request as **Agent working** and list editor writes as they arrive.
5. After completion, use the bottom notification or `Space N`. The grouped
   review should show the affected files, saved state, and exact historical
   diffs. Use `f` / `F` between files and `]` / `[` between changed locations.
6. Open `Space H` again. Click an affected file path, then cycle `v` to Changes.
   Confirm the retained diff matches what the Agent actually changed. Agent
   writes are saved automatically; this is an after-the-edit receipt, not a
   second approval step.
7. In an affected file, dismiss its marker with `Space x` or `Space X`. Restore
   it using History `p`. The source must not change a second time.
8. Make a small unsaved edit in an affected file, reopen History, and confirm
   that it reports **Changed since Agent · unsaved** while preserving the
   original diff.

At this baseline, the type appears in seven Rust files. In a separate terminal,
these read-only checks can confirm the scope:

```sh
git diff --stat
git diff --check
rg -n 'TextPanelFileLocation' src
rg -n 'SourceFileLocation' src
```

The old name should have no remaining matches. Do not commit or merge the smoke
rename unless you independently decide to keep it.

## History responsiveness

After the multi-file result is retained, open `Space H` and cycle to Changes.
Scroll the detail repeatedly with the mouse or Page Up/Page Down, then try
`j`/`k` and repeated `h`/`l` at the same item. Navigation should remain
responsive, and a no-op move must not reset the detail's scroll position.
Resize the pane and switch themes once: the diff should reflow/recolor without
losing syntax colors, changed-line backgrounds, or clickable locations.

## Optional interruption exercise

Start another explicit multi-file handoff, then cancel after the first file
changes. The outcome must say that the turn stopped, retain any writes already
applied, and let you review them. It must not claim the entire task succeeded or
silently undo completed writes.
