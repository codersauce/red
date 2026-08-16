---
title: "Copilot Inline Completion"
summary: "Enable, authenticate, and use optional GitHub Copilot ghost-text suggestions without replacing ordinary language-server completion."
topics: [guides, agent, completion]
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

Alternatively, use `:Copilot enable` to opt in for the current editor session.
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

## Use Suggestions

Type in Insert mode and pause briefly. `Tab` accepts a visible suggestion as
one undo step. `Esc` dismisses it and returns to Normal mode. Typing, moving the
cursor, or changing buffers invalidates stale suggestions. `Alt-\` requests a
suggestion explicitly; `:Copilot complete` enters Insert mode and requests one
at the current cursor [@editor].

With Copilot enabled, idle identifier typing prefers ghost text over the
automatic word-completion popup. `Ctrl-Space` still opens ordinary completion,
and language-server trigger characters still work. An open completion popup
takes priority over AI suggestions; `Alt-\` switches from that popup to an
explicit AI request [@editor].

This first integration supports single-line and multiline insertions at the
cursor. Suggestions that would rewrite existing code are not accepted. Copilot
next-edit suggestions require a separate review/preview interface and are not
part of this feature [@editor].
