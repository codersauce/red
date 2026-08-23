---
title: "Copilot Inline Completion"
summary: "Enable, authenticate, and use optional GitHub Copilot ghost-text suggestions without replacing ordinary language-server completion."
topics: [guides, ai-completion, configuration]
sources:
  - id: defaults
    type: file
    path: default_config.toml
  - id: transport
    type: file
    path: src/copilot.rs
  - id: editor
    type: file
    path: src/editor/inline_completion.rs
  - id: preferences
    type: file
    path: src/preferences.rs
---

# Copilot Inline Completion

Copilot is optional and disabled by default. Enabling it permits the official
GitHub Copilot language server to process eligible source code. It runs beside
the normal language server; it does not replace rust-analyzer, TypeScript's
server, or other language intelligence [@transport] [@editor].

## Set Up

Install GitHub's official `@github/copilot-language-server` package or a native
binary from its [release repository](https://github.com/github/copilot-language-server-release).
Make `copilot-language-server` available on `PATH`, or configure its absolute
path. Red does not download or update this executable automatically.

```toml
[copilot]
enabled = true
command = "copilot-language-server"
args = ["--stdio"]
debounce_ms = 150
max_file_bytes = 262144
excluded_patterns = [".env", ".env.*", "*.pem", "*.key", "**/.git/**"]
```

Alternatively, use `:Copilot enable` to opt in. Red saves `enabled = true` in
your user configuration so the choice survives restarts. Confirming “Enable
and sign in” through `:Copilot signin` saves the same choice before starting
authentication. `:Copilot disable` saves `enabled = false`; `:Copilot signout`
signs out without changing the enablement setting. If the configuration cannot
be saved, Red applies the choice only to the current session and shows a warning.
Type `:Copilot ` and press `Tab` to cycle through subcommands, or complete a
prefix such as `:Copilot en`. `Shift-Tab` cycles backward. Completion does not
enable Copilot or start authentication.
Then run `:Copilot signin`, copy the displayed device code, and approve opening
GitHub's sign-in page. Use `:Copilot status` to inspect the latest provider
status, `:Copilot restart` after changing the executable, and `:Copilot signout`
or `:Copilot disable` when finished [@defaults] [@editor].

`disable_ai = true` takes precedence over both configuration and session-level
enablement. Excluded, oversized, and outside-workspace files are not submitted
by Red. Exclusion patterns use gitignore syntax; replacing the list also
replaces its defaults. Provider-side account, content-exclusion, and data-use
settings still apply [@transport] [@editor].

These checks govern documents Red sends directly. The language server is a
separate process with normal filesystem access, not a sandbox [@transport].

## Setup Hint And Sign-In State

When Copilot is disabled in config but the configured executable exists, Red can
show a one-time setup hint instead of starting the Copilot bridge. The hint is
shown only when global AI is not disabled, preferences are filesystem-backed,
the hint has not already been recorded, there is no active dialog or message,
the current file passes the workspace, size, and exclusion checks, and the
configured `copilot-language-server` can be found [@editor] [@preferences].

Displaying the hint records `copilot_setup_hint_seen` in preferences before the
bridge starts. Running `:Copilot signin`, `:Copilot enable`, or
`:Copilot disable` also records the hint as seen [@editor] [@preferences].
The hint is onboarding state, not consent or authentication state. Detecting
an installed server never enables it. Explicit enablement is remembered in
`[copilot].enabled`, independently of whether authentication succeeds. Red
leaves credential management to the language server [@defaults] [@editor].

The sign-in flow keeps the device-code dialog open after Red sends
`github.copilot.finishDeviceFlow` to the language server. Red copies the code to
the clipboard when possible, leaves the code visible if copying fails, updates
the dialog on sign-in failure, and closes it only after the server reports a
successful sign-in [@editor]. Sign-in success, sign-in failure, provider stop
messages, and `:Copilot status` must be visible through Red's notification
path; writing them only to the legacy `last_error` field can hide them from the
user [@editor].

## Use Suggestions

Type in Insert mode and pause briefly. `Tab` accepts a visible suggestion as
one undo step. `Esc` dismisses it and returns to Normal mode. Typing, moving the
cursor, or changing buffers invalidates stale suggestions. `Alt-\` requests a
suggestion explicitly; `:Copilot complete` enters Insert mode and requests one
at the current cursor [@editor].

Copilot and ordinary autocomplete remain enabled together. A visible completion
popup takes priority and hides ghost text; `Tab` and `Enter` keep accepting the
selected popup item. When the popup closes or has no matching items, a current
Copilot suggestion can reappear and deferred suggestions can run. `Ctrl-Space`
opens ordinary completion, while `Alt-\` switches from that popup to an
explicit AI request. `Ctrl-e` closes the completion popup without leaving Insert
mode [@editor].

This first integration supports single-line and multiline insertions at the
cursor. Suggestions that would rewrite existing code are not accepted. Copilot
next-edit suggestions require a separate review/preview interface and are not
part of this feature [@editor].
