---
title: "Learning And Tutorial"
summary: "Red keeps first-run teaching inside editor-owned, side-effect-free practice buffers, with Learn Red and tutorial commands sharing contextual routing but different progress stores."
topics: [architecture, editor, onboarding]
sources:
  - id: editor
    type: file
    path: src/editor.rs
  - id: learning
    type: file
    path: src/editor/learning.rs
  - id: learn-core
    type: file
    path: src/learn.rs
  - id: tutorial-core
    type: file
    path: src/tutorial.rs
  - id: learn-ui
    type: file
    path: src/ui/learn.rs
  - id: welcome-ui
    type: file
    path: src/ui/welcome.rs
  - id: command-palette
    type: file
    path: src/command_palette.rs
  - id: preferences
    type: file
    path: src/preferences.rs
---

Red has two editor-native teaching paths. `:learn` opens the Learn Red hub, whose first implemented lesson is the Essentials practice checkpoint, while `:tutorial` starts or resumes the guided or quick tour [@editor] [@learn-ui] [@tutorial-core]. Both paths use temporary practice buffers and semantic editor actions, so custom keymaps still work and the lessons do not run real Git, Codex, filesystem writes, or project-buffer writes [@learn-core] [@tutorial-core] [@editor]. Use this page when changing onboarding, tutorial commands, practice-buffer safety, or progress persistence.

## Command Routing

The command parser routes `:learn` to the Learn hub and routes bare `:tutorial` to the guided tutorial track [@editor]. The tutorial command also accepts `quick`, `resume`, `restart`, `essentials`, `next`, and `quit`; `essentials` starts the Learn Red practice lesson rather than the guided tour [@editor]. While a Learn lesson is active, `:tutorial quit`, `:tutorial restart`, and `:tutorial next` are contextual controls for that lesson; outside Learn, those same words control the guided tutorial [@editor].

The command palette exposes both entries. `editor.tutorial` is the guided tour entry with `:tutorial`, and `editor.learn` is the Learn Red entry with `:learn` and onboarding search aliases [@command-palette]. The first-run welcome card points users toward the guided tour and presents it as practice for editing, Git, and safe agent changes [@welcome-ui].

## Learn Red

Learn Red is a hub plus one implemented practice lesson. `src/learn.rs` defines six tracks, but `LearnHub::open_selected` starts a lesson only for the `essentials` track; selecting another track refreshes the hub instead of starting work [@learn-core] [@learn-ui]. The stable completed lesson id is `essentials.find-your-footing.v1`, and completion is stored as a durable preference without changing project files [@learn-core] [@preferences].

Starting the lesson is guarded. Red refuses to open Learn while an inline assist, workspace manager, or callback-owned composer is active, and it refuses to start the lesson while an agent turn, inline assist, or workspace is active [@learning]. The terminal must be at least 32 columns by 12 rows because the coach reserves screen space beside or below the practice buffer [@learning] [@learn-ui].

The lesson checkpoints the real workspace before opening its scratch buffer, saves the original buffer, window layout, zoom, panel focus, repeat state, and registers, then replaces the visible layout with a single unnamed practice buffer [@learning]. Its allowed actions are deliberately narrow: basic editing, movement, undo/redo, command entry, refresh, and Learn controls are accepted, while save, save-as, file opening, buffer switching, plugin commands, and inline assist are refused [@learn-core]. Exiting removes the practice buffer, drops its LSP state, clears marks and visual selections tied to it, and restores the original editor state [@learning].

## Guided Tutorial

The guided tutorial is a versioned curriculum stored as `TutorialProgress`. The guided track has editing, discovery, navigation, completion, Git, agent, and theme lessons; the quick track has discovery, navigation, and agent lessons [@tutorial-core]. Progress records the curriculum version, selected track, lesson index, phase, and completion state, and stale or invalid progress normalizes back to the start of the selected track [@tutorial-core].

Tutorial progress observes editor actions instead of raw keys. The editing lesson advances on real mode changes, insertion, and undo; discovery and navigation advance through command palette, file picker, and project search actions; Git and agent lessons use simulated editor-owned panels or proposals instead of touching the repository or launching Codex [@tutorial-core] [@editor]. `:tutorial resume` reloads saved progress when present and otherwise starts a guided tour from the beginning [@editor] [@preferences].

The tutorial practice buffer is also protected. It is unnamed, cannot be saved, and `SaveAs` while the tutorial buffer is active reports that the practice buffer cannot be saved [@editor] [@tutorial-core]. Finishing or quitting removes the practice buffer and restores the original buffer and window layout [@editor] [@tutorial-core].

## Recovery Boundary

Learning state is not crash-recovery state. Preferences persist Learn lesson completion and tutorial progress, while [Crash Recovery Snapshots](../sessions/crash-recovery-snapshots) keep the user's real workspace recoverable [@preferences]. Learn Red explicitly snapshots the workspace before opening its scratch buffer, and later lesson edits do not replace that recovery snapshot; exiting writes a fresh snapshot for the restored real workspace [@learning] [@editor].

Do not route new teaching actions around the normal editor action pipeline. Both systems depend on semantic actions so configured shortcuts, command discovery, undo, rendering, and practice-buffer guards remain the same paths users exercise outside onboarding [@tutorial-core] [@learn-core] [@editor]. For command lookup behavior, read [Command Discovery](../commands/command-discovery); for stored onboarding state, read [Preferences Store](../preferences/preferences-store).
