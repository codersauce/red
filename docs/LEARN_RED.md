# Learn Red

Run `:tutorial` to choose a track. Each lesson uses the real editor and checks
what happened, not just which keys you pressed. The coach shows your effective
key binding, including configuration overrides.

| Track | Command | What you practice |
| --- | --- | --- |
| Essentials | `:tutorial essentials` | Modes, undo, commands, and saving |
| Build with AI | `:tutorial ai` | Inline explanations and edits, review, Agent, and diffs |
| Fix & ship | `:tutorial ship` | Diagnostics, symbols, a quick fix, staging, and a local commit |
| Find your way | `:tutorial navigation` | Files, project search, symbols, splits, and zoom |
| Edit with precision | `:tutorial editing` | Motions, text objects, repeat, and substitution |
| Make Red yours | `:tutorial custom` | Themes, keymaps, language support, and recovery |

Add a lesson number to jump directly, for example `:tutorial ship 3`. Tracks
are not locked. Vim users can start with command discovery
(`:tutorial essentials 3`) or go straight to an unfamiliar workflow.

## Continue at your own pace

| Command | Effect |
| --- | --- |
| `:tutorial help` | Show the current task and all learning controls |
| `:tutorial next` | Continue after completing the current lesson |
| `:tutorial skip` | Move on without marking the lesson complete |
| `:tutorial restart` | Recreate the current lesson and its practice files |
| `:tutorial quit` | Restore your original workspace |
| `:tutorial resume` | Reopen your last lesson with fresh practice fixtures |

Completed lessons and a bookmark for each track survive restarting Red. The
practice files and unfinished requests do not. You can replay or revisit any
lesson. The old quick/guided tour's current topic is migrated where possible;
its simulated Git and Agent demos do not count as completing the new exercises.

## What leaves the practice workspace?

The five required AI lessons use labeled, recorded responses and work offline.
For a real request, choose **Try live AI** in the AI track or run
`:tutorial ai live`. Run `:tutorial ai-check` there to check the local Codex executable and
version without signing in or sending a prompt. Submitting in the live inline
popup sends your prompt and the owned practice code to your configured Codex
service. Authentication is checked by that request, and normal account usage
may apply. The live exercise is optional and does not affect offline completion.

File, Git, LSP, and recovery exercises use disposable editor-owned storage.
Git commits are local and have no remote. The LSP lessons use bundled Husk.
Recovery practice uses a separate real snapshot, not your normal recovery store.
Leaving a lesson restores the original buffers, layout, and Agent state. The
keymap exercise is temporary; saving a theme is an explicit persistent choice.
