# Splash screen

Status: implemented (`src/splash.rs`, `render_splash` in
`src/editor/rendering.rs`); companion theme at `themes/red.json`
Date: 2026-07-19

The intro screen shown when red starts with no file arguments — the same slot
Neovim's `:intro` occupies, carrying red's own identity: a rounded lowercase
wordmark, a single point of color (the dot, always the theme's red), six
verified keystrokes, and the trust-model epigraph.

## Layout

A 60-column block, centered horizontally and vertically in the focused
window's viewport. Render-only overlay: the buffer stays empty, the cursor
stays at 1:1, line number 1 renders normally.

```
                                    ╷
                   ╭──╮   ╭──╮   ╭──┤
                   │      ├──╯   │  │
                   ╵      ╰──╴   ╰──╯  ●

                         red v0.1.1
              the modal editor for the agent era
                 github.com/codersauce/red

────────────────────────────────────────────────────────────
press  Space ?               to discover every command
press  Ctrl-p                to find a file
press  Space A               to ask the agent
type   :AgentReview<Enter>   to review the agent's proposals
press  Space t               to change the theme
type   :q<Enter>             to exit
────────────────────────────────────────────────────────────

              every agent edit is a proposal —
       nothing touches your files until you accept it
```

The version string comes from `CARGO_PKG_VERSION`. Every keystroke above is
verified against `default_config.toml` (`Space ?` → CommandPalette, `Ctrl-p` →
FilePicker, `Space A` → Agent, `Space t` → ThemeBrowser). red has no `:help`;
the command palette is the discovery surface, so it leads the list.

## Identity notes

- **Rounded where Neovim is angular.** The wordmark is single-stroke box
  drawing with rounded corners (`╭ ╮ ╰ ╯`), lowercase and small — the opposite
  temperament of nvim's thin, doubled, angular N.
- **The dot is the brand.** The wordmark reads "red●"; the dot is the only
  saturated element on screen.
- **The epigraph is the differentiator** (see `docs/DIFFERENTIATION.md`): the
  proposal-based agent trust model, stated in one sentence, in the slot where
  nvim puts its sponsor line.

## Colors — theme tokens only, never hardcoded

| Element                 | Token                       | Fallback                        |
| ----------------------- | --------------------------- | ------------------------------- |
| Wordmark strokes        | `editor.foreground`         | terminal default fg             |
| The dot `●`             | `terminal.ansiBrightRed`    | `errorForeground`, then ANSI red|
| Key column              | `terminal.ansiRed`          | same chain as the dot           |
| Version, tagline, verbs | `descriptionForeground`     | dimmed fg                       |
| Rules                   | `editorGroup.border`        | dim fg                          |
| Epigraph                | `editor.foreground` dimmed  | between fg and muted            |

Because everything maps to tokens red already reads from the active VSCode
theme, the splash re-skins live with the theme browser (`Space t`) like every
other surface.

## Behavior

- **Show when:** launched with no file arguments, a single unnamed blank
  buffer, and a window content area of at least 60×20 cells.
- **Dismiss when:** the buffer is first modified, a file is opened (picker,
  `:e`, session restore), or the window is split. Pure motions do not
  dismiss. Once its conditions fail after it has been shown, the splash is
  latched off for the rest of the session.
- **Config:** `splash = false` in `config.toml` (flat key, matching `wrap` /
  `scrolloff` style; default `true`).
- **Degrade:** below 60×20 content cells, a compact variant — wordmark,
  `red v0.1.1`, and `press Space ? for commands`. Below 26×7, nothing.

```
                 ╷
╭──╮   ╭──╮   ╭──┤
│      ├──╯   │  │
╵      ╰──╴   ╰──╯  ●

     red v0.1.1
press Space ? for commands
```

## Implementation

Belongs in the core empty-buffer render path (like Neovim's intro), not a Husk
plugin: it must appear on the very first frame, before plugin activation
completes, and it needs the theme-token fallback chain that core rendering
already owns. It is a paint-time overlay in the window's viewport, composed
after the buffer/line-number pass and skipped entirely once dismissed.
