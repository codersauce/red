# Composer keyboard compatibility

The Agent dialog and conversation footer send with Enter (or Ctrl+Enter).
Alt+Enter, Shift+Enter, and Ctrl+J insert a newline. Ctrl+J takes priority over
conversation scrolling while the footer is focused. Escape enters Vim Normal
mode; Enter in an active Vim search confirms the search, not the prompt.
Pasting multiline text never sends it. File editing and single-line inputs keep
their own Enter behavior.

Run `red keys` in the affected terminal to see the selected protocol and decoded
Enter modifiers. It does not start Codex, open a document, log typed text, or
change terminal configuration files. Escape or Ctrl+C exits. Compare with
`red keys --protocol legacy`, `--protocol kitty`, or `--protocol xterm` when
diagnosing an emulator or multiplexer. The overrides affect that diagnostic only.

Red uses native console key records on Windows. On Unix it first queries support
for Kitty keyboard disambiguation, then requests xterm `modifyOtherKeys` on
xterm-compatible terminal names. Unknown terminals retain legacy input. The
xterm fallback is a request, not proof that the terminal supports it. Red
restores the selected mode on exit, suspend, and detach. Reattachment negotiates
with the new terminal. Crossterm is the only input reader.

## When a shortcut still does not arrive

- Use Ctrl+J as the portable newline fallback. A terminal may encode Shift+Enter
  exactly like Enter; Red cannot safely infer a modifier that was not transmitted.
- In Windows Terminal, remove or reassign the Alt+Enter fullscreen binding if
  you want the application to receive it. Native Windows and WSL use different
  input backends, so inspect both when relevant.
- In Terminal.app or iTerm2, check Option/Meta handling and the profile's keyboard
  mappings. A profile can map modified Enter to Ctrl+J (line-feed byte `0x0a`).
- In VS Code, check shortcuts intercepted by the editor. Its
  `workbench.action.terminal.sendSequence` command can map the desired shortcut
  to `\u000a` while terminal focus is active.
- Test outside tmux first. If that works, inspect the tmux version, its
  `extended-keys` setting, and the outer terminal's `extkeys` capability. Repeat
  inside SSH if that is part of the actual path.
- Legacy Alt uses an Escape prefix, which is inherently ambiguous with pressing
  Escape followed by another key. Prefer an enhanced protocol or Ctrl+J on
  connections that split that sequence.

See the [Kitty keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/),
[xterm control sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html),
[tmux modifier-key guide](https://github.com/tmux/tmux/wiki/Modifier-Keys),
[Windows Terminal bindings](https://learn.microsoft.com/en-us/windows/terminal/customize-settings/actions),
and [VS Code terminal shortcuts](https://code.visualstudio.com/docs/terminal/advanced).

## Validation

`cargo test --lib composer -- --test-threads=1` checks the surface policy.
On Linux/macOS, build Red and run `python3 scripts/test_keyboard_protocol.py` to
exercise legacy, CSI-u, xterm, fragmented input, negotiation, and cleanup through
the real Crossterm PTY reader. The vendored parser also has focused tests:
`cargo test --locked --manifest-path vendor/crossterm/Cargo.toml --lib red_`.

Real-emulator checks should record OS, terminal/version, direct versus tmux/SSH,
the `red keys` result, and whether a user mapping was required. Target
Terminal.app, iTerm2, Kitty, Ghostty, WezTerm, a VTE terminal, VS Code, and Windows
Terminal (native and WSL). Passing the PTY suite does not certify an emulator's
physical key bindings.
