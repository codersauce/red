# Getting started with Red

This guide covers the day-to-day editor workflow. For installation, see the
[README](../README.md#install).

## First launch

Open one or more files:

```shell
red src/main.rs
red src/main.rs src/lib.rs
```

Set an explicit workspace root with `-r`:

```shell
red -r path/to/project src/main.rs
```

On the first interactive run, Red offers to create a starter configuration at
`~/.config/red/config.toml`. The file is optional; Red starts with its embedded
configuration, themes, and plugins when it is absent.

## Editor model

Red uses Vim-inspired modes. `Esc` returns to Normal mode.

| Mode | Enter with | Purpose |
| --- | --- | --- |
| Normal | `Esc` | Navigate and issue editing commands |
| Insert | `i`, `a`, `o`, and variants | Enter text |
| Visual | `v` | Select by character |
| Visual Line | `V` | Select whole lines |
| Visual Block | `Ctrl-v` | Select a rectangle |
| Command | `:` or `;` | Run named commands |

The [Vim compatibility matrix](VIM_COMPATIBILITY.md) records supported behavior
and intentional differences precisely.

## Moving around

- `h/j/k/l` or the arrow keys move left, down, up, and right.
- `w/b/e/ge` move by word; `B/E/gE` use whitespace-delimited WORDs.
- `f{char}`/`t{char}` and `F{char}`/`T{char}` find or move until a character.
  `,` repeats in the opposite direction.
- `0`, `^`, and `$` move to the beginning, first non-blank, and end of a line.
- `gg` and `G` move to the first and last line.
- `Ctrl-b`/`Ctrl-f` page up and down; `Ctrl-u`/`Ctrl-d` move half a page.
- `zz` centers the current line.
- `%` jumps to a matching bracket. `g%`, `[%`, and `]%` provide related
  matching-bracket motions.
- `Ctrl-o` and `Ctrl-i` (or `Tab`) move backward and forward through the current window's jump list.
- `gj` and `gk` move by screen line when wrapping is enabled.

## Editing

- `i`/`a`, `I`/`A`, and `o`/`O` enter Insert mode at common positions.
- `x`/`X` delete a character; `dd` deletes a line; `dw` deletes a word.
- `D`, `C`, and `Y` operate from the cursor to the end of the line.
- `s` and `S` substitute characters or the current line.
- `J` and `gJ` join lines with normalized or preserved whitespace.
- `~`, `gu{motion}`, `gU{motion}`, and `g~{motion}` change case.
- `u` undoes; `Ctrl-r` or `U` redoes.
- `p` and `P` paste after or before the cursor.
- `>>` and `<<` indent or unindent the current line.

Counts work with supported actions and motions.

## Selecting

Use `v`, `V`, or `Ctrl-v` for character, line, or block selections. In a
selection:

- `y` copies, `x` deletes, and `p` replaces with pasted text.
- `r{char}` replaces selected characters.
- `u`, `U`, and `~` change case.
- `I` in Visual Block mode inserts on every selected line.

Text objects include `iw` for a word and `i(`/`a(`, `i[`/`a[`, `i{`/`a{`,
`i<`/`a<`, and quoted equivalents for delimited text. `a%` selects a matchit
pair.

## Searching

- `/` and `?` search forward and backward with live preview.
- In either search prompt, Up/Down or Ctrl-p/Ctrl-n recall submitted searches,
  filtered by what you have typed. Down past the newest match restores your
  draft. Both directions share a history that persists across restarts.
- `n` and `N` repeat in the same or opposite direction.
- `*` searches for the word under the cursor.
- `:noh` clears highlights.

Patterns use Rust regular-expression syntax. `incsearch`, `hlsearch`,
`wrapscan`, `ignorecase`, and `smartcase` are configurable. The bundled
`cool_search` plugin clears stale highlights as you continue editing.

## Language intelligence

| Key | Action |
| --- | --- |
| `K` | Hover documentation |
| `gd` | Go to definition |
| `Ctrl-Space` | Trigger completion in Insert mode |
| `Ctrl-k` | Show signature help in Insert mode |
| `Ctrl-t` | Find document symbols |
| `Space w` | Find workspace symbols |
| `Space k` | Find references |
| `Space f` | Format the current document |
| `Space .` | Show code actions and quick fixes |
| `Space r` | Rename the current symbol |

Husk's first-party server is included and starts for `.hk` and `.husk` files.
Built-in defaults also cover Rust, TypeScript/JavaScript, Markdown, JSON, TOML,
YAML, and Lua; those external servers must be installed separately and
available on `PATH`. Servers start only after a matching file is opened.
See the [Husk language-server guide](HUSK_LSP.md) for its complete feature and
external-crate contract.

Python highlighting, commenting, and Pyright configuration are available from
the official language-pack catalog:

```shell
red plugin install --catalog python-language --trust-native-grammars
```

Completion combines two sources. Matching words from all open buffers provide
fast text completion even when no language server is installed. When a server
is available, its type-aware candidates are merged in and ranked ahead of
buffer words. `Ctrl-Space` requests both sources explicitly; typing an
identifier prefix requests them automatically. Language-server trigger
characters such as `.` also request completion immediately. While the menu is
open, use `Ctrl-n`/`Ctrl-p` or the arrow keys to select a candidate, `Tab` to
accept it, and `Ctrl-e` to dismiss the menu. `Enter` continues to insert a
newline.

Tune or disable either behavior in `config.toml`:

```toml
[completion]
auto_trigger = true
min_prefix_length = 1
debounce_ms = 0
buffer_words = true
max_buffer_words = 100
```

For example, after installing the Python pack, its type-aware names,
attributes, signatures, and import suggestions require the configured
`pyright-langserver` executable to be on `PATH`. Buffer-word completion remains
available if that executable is absent.

Add or override a server in `config.toml`:

```toml
[lsp.servers.go]
command = "gopls"
language_id = "go"
file_extensions = ["go"]
root_markers = ["go.mod", ".git"]
```

## Finding files, buffers, and commands

| Key | Action |
| --- | --- |
| `F1`, `:keys` | Contextual keyboard-shortcut explorer |
| `Space ?`, `Alt-x`, `Ctrl-Shift-p` | Command palette |
| `Ctrl-p` | File picker |
| `Ctrl-p`, then `>` | Switch from files to commands |
| `Ctrl-e` | Toggle hidden files in the picker; open the tree otherwise |
| `Ctrl-j` or `Space b` | Buffer picker |
| `Space g` | Project search using `rg` |
| `Space t` | Theme browser |

Press `F1` or click **F1 shortcuts** in an action strip to open keyboard help without closing the current pane or dialog. `Tab` switches between the current context and all Red keys; `/` searches by action or binding. `Esc` returns to exactly where you were. The **Keyboard shortcuts** command and `:keys` open the same explorer. User keymap overrides are reflected in the list.

The command palette includes descriptions, effective keymaps, and accepted
`:Command` invocations. Pause after a configured prefix such as `Space`,
`Ctrl-w`, or `g` to display available continuations.

## Windows and buffers

- `Ctrl-w s` splits horizontally (top/bottom); `Ctrl-w v` or `Ctrl-w d` splits
  vertically (side by side).
- `Ctrl-w h/j/k/l` move focus between editor windows and docked panes.
- `Ctrl-w H/J/K/L` move the focused editor window or pane to the left, bottom,
  top, or right outer edge.
- `Ctrl-w >` and `Ctrl-w <` grow or shrink a vertical pane or editor split.
- `Ctrl-w +` and `Ctrl-w -` grow or shrink a horizontal pane or editor split.
- Prefix a resize with a count, such as `5 Ctrl-w >`, to move five cells.
- Drag a pane or editor-split divider with the mouse to resize it directly. The
  captured divider highlights immediately, follows the mouse, and returns to its
  normal appearance when released. Press `Esc` to cancel the drag.
- `Ctrl-w w` selects the next window.
- `Ctrl-w c` closes a window.
- `Ctrl-w =` balances editor splits or restores a focused pane's original size.
- `Ctrl-w _` and `Ctrl-w o` maximize or keep only the current window.
- `Ctrl-w z` maximizes the focused window or docked pane; press it again to
  restore the layout. Moving focus or changing the layout restores it first.
  The same chord zooms the focused file-list or diff pane in Git workspaces.
- `Space Space`, `Space n`, and `Space p` move through buffers.

## Command mode

Enter Command mode with `:` or `;`.

| Command | Action |
| --- | --- |
| `:w [file]` | Save, optionally under another name |
| `:wq` | Save and quit |
| `:q` / `:q!` | Quit, or quit while discarding changes |
| `:e <file>` / `:e!` | Open or reload a file |
| `:<number>` / `:$` | Jump to a line or the last line |
| `:bn` / `:bd` | Select the next buffer or delete a buffer |
| `:sp [file]` / `:vs [file]` | Open a horizontal or vertical split |
| `:close` / `:only` | Close the window or keep only the current window |
| `:wrap` / `:nowrap` | Enable or disable wrapping |
| `:syntax [language]` / `:syn [language]` / `:ft [language]` | Choose buffer-local syntax; use `auto` to reset or `off` to disable |
| `:languages reload` | Reload custom language definitions, trusted grammars, and changed language servers |
| `:join [count]` / `:join! [count]` | Join with normalized or preserved spacing |
| `:commands` | Open the command palette |
| `:messages` | Browse active notifications and recent messages |

The bottom line shows the current notification. Routine feedback such as saves
and copies fades away without leaving a badge. The badge counts warnings or
errors needing acknowledgment, meaningful messages you may have missed, and
concurrent running operations. Informational messages count as seen after about
one second on the focused message line; every message remains in history. Open
the history with `Space m`, `:messages`, or a click on the message line. Use
`j`/`k` to browse, `/` to search full message text, `f` to switch between all,
active, needs-attention, and warning/error filters,
`Enter` to acknowledge, `y` to copy, and `Esc` to return. `Ctrl-d`/`Ctrl-u`
scroll long details; `D` clears inactive history. Messages are retained for
the current Red session, subject to the history limit.

## Git workspace

The bundled Git plugin provides gutter signs and a full-screen status
workspace. Open it with `Space G`.

- `[h` and `]h` move between hunks.
- `Space h s`, `Space h u`, and `Space h r` stage, unstage, or reset a hunk.
- `Space c c` submits a commit message; `Space c q` cancels it.
- The commit editor shows branch and working-tree status followed by the staged diff.
  `:w` or `:wq` submits the message and returns to the workspace; `:q` cancels.

The workspace covers staged, unstaged, untracked, and conflicted files with an
adaptive diff pane. It also exposes synchronization, branch, remote, tag,
stash, worktree, log, reset, and interactive-rebase actions. Authentication
uses your existing SSH agent or Git credential helper.

The diff pane wraps long lines by default. Press `Tab` or `Ctrl-w w` to move
between the file list and diff; `Ctrl-w h` and `Ctrl-w l` focus a pane
directly. Wide terminals show the panes side by side; narrower terminals stack
the file list above the full-width diff, falling back to one focused pane only
when there is not enough height for both. In the diff, `j`/`k`,
`Ctrl-u`/`Ctrl-d`, `Ctrl-b`/`Ctrl-f`, and
`[h`/`]h` provide line, page, and hunk navigation. `W` toggles wrapping; when
wrapping is off, `h`/`l`, the arrow keys, and `0`/`$` scroll horizontally.

The bottom action strip follows the focused pane and selection. Press `?` or
`F1` for the complete, actionable list. In the file pane, `/` filters paths;
`Enter` applies the filter and `Esc` clears it. `C` or `Enter` on a section
heading collapses that section. Drag the vertical divider, or use `Ctrl-w <`
and `Ctrl-w >`, to resize the file pane; `Ctrl-w o` hides or restores it.
The chosen width is retained while the editor remains open.

The diff header keeps the file, staged state, change counts, and current hunk
visible. `M` toggles raw patch metadata, and `L` opens the complete patch in a
scratch buffer when the bounded preview is not enough. Line colors and exact
changed-word highlights use the active theme without replacing syntax colors.

Use `v` to select changed lines. Lowercase `s`, `u`, and `x` stage, unstage,
or discard the current changed line or selection; uppercase `S`, `U`, and `X`
apply the same operation to the current hunk. Destructive actions remain
confirmation-gated.

## Agent workflow

For a bounded one-range edit, put the cursor on a line or make a character or
linewise visual selection and press `Space i`. Enter a request such as
`extract the condition into a named boolean`. Red anchors a small, auto-growing
prompt inside the initiating editor split, beside but never over the target,
and applies the completed replacement as one unsaved transaction.
Press Enter to keep it, `u` to undo, `r` to refine, or `A` to continue in the
full Agent panel. Visual-block selections are rejected in this first version.

Install and authenticate Codex separately, then press `Space A` from Normal or
Visual mode. Red sends a bounded source excerpt, unsaved contents, and relevant
diagnostics. Red reveals each file operation as it happens, applies
revision-checked edits through the editor, and saves them with agent attribution.

Run `red --agent-check` for an offline prerequisite report or
`red --agent-check --strict` for a non-zero exit when setup is incomplete.
See the [agent workflow and safety contract](AGENT_WORKFLOW.md) for the complete
interaction model and command list.

## Configuration

Red layers your configuration over embedded defaults:

```toml
# ~/.config/red/config.toml
theme = "red.json"
scrolloff = 8

[search]
ignorecase = true
smartcase = true

[keys.normal]
"Ctrl-s" = "Save"
```

Every mode has its own key table. A binding can name an action, list a sequence
of actions, define a nested chord, or invoke a plugin command:

```toml
[keys.normal]
"u" = "Undo"
"a" = [{ EnterMode = "Insert" }, "MoveRight"]
"g" = { "d" = "GoToDefinition" }
"Ctrl-j" = { PluginCommand = "BufferPicker" }
```

The prefix guide can be configured independently:

```toml
[key_hints]
enabled = true
delay_ms = 250
```

See [`default_config.toml`](../default_config.toml) for every supported setting.

## Plugins and themes

Bundled plugins are enabled by default. Disable or configure them by ID:

```toml
disabled_plugins = ["barbecue"]

[plugin_config.lsp_symbols.icons]
enabled = false
```

Plugins that spawn processes need an explicit allowlist. For example,
`project_search` uses:

```toml
[plugin_permissions.project_search]
process = ["rg"]
```

Run `red --runtime-files` to list every visible plugin and theme. Eject a
bundled asset to customize it:

```shell
red --eject plugins/fidget.hk
red --eject themes/red.json
```

Files in `~/.config/red/plugins/` and `~/.config/red/themes/` override embedded
assets with the same filename. An ejected copy continues to shadow future
bundled updates until it is removed. Read the
[plugin system guide](PLUGIN_SYSTEM.md) for runtime details.

## Command-line reference

```text
red [files...]              # open one or more files
red -r <path>               # set the working directory root
red -c 'wrap = false'       # inline TOML override; repeatable
red --version               # print the installed version
red --runtime-files         # list visible plugins/themes and their sources
red --eject <asset>         # copy a bundled plugin/theme into your config dir
red --agent-check           # report Codex integration prerequisites
```

Use `red --help` for the complete generated command-line reference.

## Troubleshooting

Red logs to `red.log` in its configuration directory by default. Relative
`log_file` values are resolved from that directory; absolute paths and `~/...`
paths are also supported.

- **LSP is not working:** confirm the language server is installed and on
  `PATH`.
- **A plugin is missing:** run `red --runtime-files` and check its source and
  activation status.
- **A theme is not found:** check its filename with `red --runtime-files` and
  validate custom theme JSON.
- **A bundled asset behaves like an old version:** an ejected file may be
  shadowing it. Delete the custom copy or replace it with
  `red --eject-force <asset>`.
- **Agent setup fails:** run `red --agent-check`, install or update Codex, and
  complete `codex login`.
- **A session needs recovery:** follow
  [Session recovery](SESSION_RECOVERY.md).

Report reproducible problems in
[GitHub Issues](https://github.com/codersauce/red/issues).
