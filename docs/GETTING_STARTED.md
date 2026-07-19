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
- `Ctrl-o` and `Tab` move backward and forward through the jump list.
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

Built-in server defaults cover Rust, TypeScript/JavaScript, Python, Markdown,
JSON, TOML, YAML, and Lua. Each language server must be installed separately
and available on `PATH`; servers start only after a matching file is opened.

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
| `Space ?`, `F1`, `Alt-x`, `Ctrl-Shift-p` | Command palette |
| `Ctrl-p` | File picker |
| `Ctrl-p`, then `>` | Switch from files to commands |
| `Ctrl-e` | Toggle hidden files in the picker; open the tree otherwise |
| `Ctrl-j` or `Space b` | Buffer picker |
| `Space g` | Project search using `rg` |
| `Space t` | Theme browser |

The command palette includes descriptions, effective keymaps, and accepted
`:Command` invocations. Pause after a configured prefix such as `Space`,
`Ctrl-w`, or `g` to display available continuations.

## Windows and buffers

- `Ctrl-w s` and `Ctrl-w v` split horizontally and vertically.
- `Ctrl-w h/j/k/l` move between windows.
- `Ctrl-w w` selects the next window.
- `Ctrl-w c` closes a window.
- `Ctrl-w =`, `Ctrl-w _`, and `Ctrl-w o` balance, maximize, or keep only the
  current window.
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
| `:join [count]` / `:join! [count]` | Join with normalized or preserved spacing |
| `:commands` | Open the command palette |

## Git workspace

The bundled Git plugin provides gutter signs and a full-screen status
workspace. Open it with `Space G`.

- `[h` and `]h` move between hunks.
- `Space h s`, `Space h u`, and `Space h r` stage, unstage, or reset a hunk.
- `Space c c` submits a commit message; `Space c q` cancels it.

The workspace covers staged, unstaged, untracked, and conflicted files with an
adaptive diff pane. It also exposes synchronization, branch, remote, tag,
stash, worktree, log, reset, and interactive-rebase actions. Authentication
uses your existing SSH agent or Git credential helper.

## Agent workflow

Install and authenticate Codex separately, then press `Space A` from Normal or
Visual mode. Red sends a bounded source excerpt, unsaved contents, and relevant
diagnostics. Suggested writes remain isolated until you review them with
`:AgentReview`.

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

Red logs to `/tmp/red.log` by default. Override `log_file` in your config when
another location is preferable.

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
