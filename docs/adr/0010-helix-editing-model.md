# ADR 0010: Add an experimental Helix editing model beside Vim

- Status: Accepted for an experimental implementation
- Date: 2026-11-03

## Context

Red's Vim grammar consumes `d`, `c`, `y`, `f`, `t`, `r`, `m`, `q`, and `@`
inside `Editor::handle_normal_event` before the keymap is consulted. A keymap
preset cannot give those keys Helix meanings. The cursor (`cx` / `cy`) and the
visual selection are also separate pieces of state, while Helix treats every
cursor as a one-scalar selection and lets motions replace that selection.

Rebuilding the editor as selection-first and emulating Vim on top would put
the existing Vim suite back on the table. A second grammar can reuse Red's
actions, transactions, pickers, LSP, and Explore without that rewrite.

The headless selection kernel is intended to become its own crate, named
`felix`, after this editor integration has been validated. This change does
not extract that crate.

## Decision

Keep the Vim engine unchanged and add a Helix grammar behind
`editing_model = "vim" | "helix"`.

- The selection model lives in `src/felix/`. A `Range` is an inclusive pair of
  Unicode scalar indices (`anchor`, `head`). A `Selection` is a non-empty,
  ordered, non-overlapping set of ranges. The cursor is a one-scalar range.
- Each window stores that selection next to `cx` / `cy`. Under the Vim model
  the set is one one-scalar range, rewritten from the cursor whenever the
  window is synced. Rendering and commands ignore it, so Vim behavior does
  not change.
- Endpoints move through edits with the same left/right affinity rules as
  `transform_anchor_for_edit`. A cursor uses right affinity.
- `editing_model = "helix"` selects `Mode::Select` and the felix key path.
  That path does not call the Vim operator, find, replace, mark, or macro
  interceptors. It resolves `helix_keys`, then the built-in felix defaults.
- Helix commands call existing Red actions where the behavior already exists
  (undo, search entry, colon commands, pickers, window commands, LSP
  `goto_definition`). Selection-shaped motions and verbs are felix commands.
- The setting is read from configuration at startup. Agent and panel text
  areas keep their own Vim engine.
- Where a Helix `Space` binding clashes with a Red `Space` menu, Red's menu
  stays. Other Helix defaults are followed. In particular `Space e` remains
  Explore and `Space f` remains Red's format binding.

`src/felix/` must not depend on the terminal, the LSP client, or the plugin
runtime, so it can move into the `felix` crate later without dragging the
editor with it.

## Consequences

Vim users see no key or rendering change. The Vim suite, including
`tests/editing.rs`, remains the regression gate.

Helix mode is experimental. The compatibility matrix in
`docs/HELIX_COMPATIBILITY.md` is the list of keys that are actually wired.
Gaps stay documented there instead of being implied by a Helix keymap page.

Two grammars now have to be maintained. Mode-coupled features must check
`editing_model` before assuming a visual selection or a Vim operator.
