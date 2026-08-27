# Getting started with Red

This guide covers the day-to-day editor workflow on the current development
branch. The latest published release is v0.6.0; features labeled **coming in
the next release** are available on `main` but not in that published binary.
For installation, see the [README](../README.md#install).

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

**Coming in the next release:** The first interactive launch opens a
keyboard-first welcome screen inside the editor. Start its guided tour, press
`i` for a shorter tour, view release
highlights, or press `Esc` to begin editing immediately. Use `:welcome` to
reopen it later.

The guided tour uses an unnamed practice buffer and simulated Git and agent
previews; it never saves the practice buffer, changes your repository, or
starts Codex. The full tour covers modal editing, command discovery, fuzzy
navigation, completion, Git, a safe local Agent demonstration, and themes.
Start or control it at any time:

```text
:tutorial          # start the full guided tour
:tutorial quick    # start the shorter experienced-user tour
:tutorial resume   # continue an unfinished tour
:tutorial next     # skip the current lesson
:tutorial quit     # exit and restore your original editor layout
```

A starter configuration at `~/.config/red/config.toml` remains optional. Press
`c` on the welcome screen to create one without overwriting existing settings.
Embedded configuration, themes, and plugins work when the file is absent.

## Editor model

Red uses Vim-inspired modes. `Esc` returns to Normal mode.

| Mode | Enter with | Purpose |
| --- | --- | --- |
| Normal | `Esc` | Navigate and issue editing commands |
| Insert | `i`, `a`, `o`, and variants | Enter text |
| Visual | `v` | Select by character |
| Visual Line | `V` | Select whole lines |
| Visual Block | `Ctrl-v` | Select a rectangle |
| Command | `:` | Run named commands |

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

## Multi-cursor editing

**Coming in the next release:** Red supports built-in, Vim-style multi-cursor
editing without an extra plugin. In Normal mode:

| Key | Action |
| --- | --- |
| `Ctrl-n` | Select the word under the cursor, then add the next occurrence |
| `Ctrl-Up` / `Ctrl-Down` | Add a cursor on the previous or next suitable line |
| `n` / `N` | Move forward or backward through matching selections |
| `q` / `Q` | Skip the current occurrence or remove its selection |
| `Tab` | Enter extend mode; press again to collapse to each selection head |
| `Shift-Left` / `Shift-Right` | Extend each selection by a grapheme |
| `h`, `l`, `w`, `e`, `0`, `$` | Extend with Vim motions while extend mode is active |
| `o` | Swap the anchor and head of every extended selection |
| `c`, `i`, `a` | Replace, insert before, or append after every selection |
| `d`, `x`, `y`, `p`, `P` | Delete, yank, or paste across the selected ranges |
| `Esc` | Finish an insertion or clear the active multi-cursor session |

For example, press `Ctrl-n` twice over `foo`, type `cbar`, and press `Esc` to
replace both selected occurrences with `bar`. One `u` undoes the entire edit.
Selections respect complete Unicode graphemes, and vertical cursors preserve
display columns across tabs and spaces.

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
| `Ctrl-k` | Show signature help in Insert mode; cycle available overloads |
| `Ctrl-t` | Find document symbols |
| `Space w` | Find workspace symbols |
| `Space k` | Find references |
| `Space f` | Format the current document |
| `Space .` | Show code actions and quick fixes |
| `Space r` | Rename the current symbol |

Ordinary completion and Copilot can be enabled independently. By default, the
completion menu takes priority over Copilot ghost text. **Coming in the next
release:** Try coordinated previews instead by adding:

```toml
[completion]
inline_mode = "coordinated"
```

When Copilot can extend the selected plain-text completion, both stay visible.
Up/Down changes the selected item, Tab or Enter accepts that item first, and a
second Tab accepts the remaining AI text. Ctrl-l accepts the whole AI suggestion
at once. Snippets and completions with additional edits keep the default
popup-first behavior. Ctrl-e closes the menu; Alt-\ requests Copilot on its own.
Set `inline_mode = "popup_first"` to restore the default.

For Copilot only, set `enabled = false` under `[completion]`. This also disables
Ctrl-Space and language-server trigger characters. To stop only identifier-prefix
popups, use `auto_trigger = false` instead; Ctrl-Space and language-server trigger
characters remain available. Neither setting enables or disables Copilot.

**Coming in the next release:** Supported documents are formatted on save by
default, and pasted ranges are formatted when the active language server
supports range formatting. Red prefers an installed language-pack formatter
for whole-document formatting and otherwise uses LSP. Disable either behavior
in `~/.config/red/config.toml`:

```toml
[formatting]
on_save = false
on_paste = false
```

`Space f` still formats explicitly. The same section accepts
`provider = "auto"`, `"external"`, or `"lsp"`. The legacy
`lsp.format_on_save` setting remains supported; `formatting.on_save` wins if
both are present in the same config layer.

When a language server supports signature help, Red shows a small popup while
you enter call arguments. The current parameter is highlighted, and typing,
completion, and cursor movement continue normally. `Ctrl-k` reopens the popup
or cycles through overloads. Leaving Insert mode closes it.

Use `[signature_help]` in `config.toml` to set `auto_trigger = false`, adjust
`debounce_ms` (120 by default), or hide the extra documentation line with
`show_documentation = false`. Manual `Ctrl-k` remains available when automatic
help is disabled.

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
open, use `Ctrl-n`/`Ctrl-p` or the arrow keys to select a candidate, `Tab` or
`Enter` to accept the selected item, and `Ctrl-e` to dismiss the menu. `Enter`
inserts a newline only when no completion is selected.

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

In the file picker, append `:<line>` to a fuzzy filename query to open the
selected file at that line. For example, `sona:123` opens `source_name.rs` at line 123.

Press `F1` or click **F1 shortcuts** in an action strip to open keyboard help without closing the current pane or dialog. `Tab` switches between the current context and all Red keys; `/` searches by action or binding. `Esc` returns to exactly where you were. The **Keyboard shortcuts** command and `:keys` open the same explorer. User keymap overrides are reflected in the list.

The command palette includes descriptions, effective keymaps, and accepted
`:Command` invocations. Pause after a configured prefix such as `Space`,
`Ctrl-w`, or `g` to display available continuations.

### Searching the file tree

Press `Ctrl-e` to open the Neo-tree sidebar, then `/` to search files and
directories recursively. Results appear directly in the tree, including entries
inside collapsed folders. Queries match complete workspace-relative paths, so
`ui pick` can find `src/ui/file_picker.rs`; matching filename characters are
highlighted.

Use Up/Down or `Ctrl-p`/`Ctrl-n` to move between results and Enter to open a
file or reveal a directory. `Ctrl-Enter` reveals the selected result without
opening it, and Escape restores the tree as it appeared before the search.
Press `D` instead of `/` to search directories only. Press `f` to apply a
persistent filter, or `Shift-Enter` to keep an ordinary search visible; use
`Ctrl-x` to clear either filter.

Search respects the tree's Git ignore settings, includes its visible dotfiles,
and never traverses `.git` metadata. It runs in the background without requiring
`fd`, `find`, or additional plugin process permissions.

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
- `Space Space` toggles between your two most recently used buffers.
- `Space n` and `Space p` move through the buffer list in order.

## Command mode

Enter Command mode with `:`. By default, `;` repeats the previous character
search, as in Vim; it can be remapped explicitly in your configuration.

| Command | Action |
| --- | --- |
| `:w [file]` | Save, optionally under another name |
| `:w! [file]` | Explicitly overwrite a changed file or existing destination |
| `:wa` / `:wall` | Save every modified file buffer |
| `:wq` | Save and quit |
| `:q` / `:q!` | Quit, or quit while discarding changes |
| `:e <file>` / `:e!` | Open a file or discard local edits and reload from disk |
| `:diffdisk` | Compare a locally edited file with its changed disk version |
| `:<number>` / `:$` | Jump to a line or the last line |
| `:bn` / `:bd` | Select the next buffer or delete a buffer |
| `:bufdo {command}` | Run a non-interactive Ex command in every open buffer |
| `:!{command}` / `:!!` | Run a shell command or repeat the previous one |
| `:%!{command}` / `:2,5!{command}` | Filter the whole buffer or selected lines through a command |
| `:'<,'>!{command}` | Filter the lines from the last Visual selection |
| `:sp [file]` / `:vs [file]` | Open a horizontal or vertical split |
| `:close` / `:only` | Close the window or keep only the current window |
| `:wrap` / `:nowrap` | Enable or disable wrapping |
| `:syntax [language]` / `:syn [language]` / `:ft [language]` | Choose buffer-local syntax; use `auto` to reset or `off` to disable |
| `:languages reload` | Reload custom language definitions, trusted grammars, and changed language servers |
| `:join [count]` / `:join! [count]` | Join with normalized or preserved spacing |
| `:commands` | Open the command palette |
| `:messages` | Browse active notifications and recent messages |

Shell commands run asynchronously in Red's working directory using `$SHELL -c`
on Unix or `COMSPEC /C` on Windows. Output streams into the Messages view while
the editor, language servers, and detached sessions remain responsive. Press
`Ctrl-c` in the selected running command to cancel it, or `Esc` to return to
editing while it continues. `%` expands to the current file, `#` to the
alternate file, and `!` to the previous shell command; prefix those characters
with `\` when they should remain literal. Ordinary `:!` jobs do not provide
interactive stdin. Range filters such as `:%!sort` pipe the selected text into
the command and replace it with stdout as one undoable edit; Visual `:` already
prefills the `'<,'>` range. Failed, cancelled, or outdated filters leave the
buffer unchanged, and stderr remains visible in Messages.

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

## Files changed outside Red

**Coming in the next release:** Red watches open files and protects your work
when another editor, formatter, Git operation, or agent changes them on disk.
A clean buffer reloads automatically. A dirty buffer remains unchanged, and Red
marks the conflict instead of overwriting either version. Deleted files stay
open and receive their own conflict indicator.

Run `:diffdisk` to compare the disk and buffer versions in a unified diff.
The default choice keeps your local edits and leaves the disk
untouched. Use `:e!` to discard your edits and reload, `:w <file>` to save
them elsewhere, or `:w!` to overwrite the changed disk version deliberately.
Ordinary `:w` and `:wall` never silently overwrite an external change.

## Git workspace

The bundled Git plugin provides gutter signs and a full-screen status
workspace. Open it with `Space G`.

- `[h` and `]h` move between hunks.
- `Space h s`, `Space h u`, and `Space h r` stage, unstage, or reset a hunk.
- `Space c c` submits a commit message; `Space c q` cancels it.
- The commit editor shows branch and working-tree status followed by the staged diff.
  `:w` or `:wq` submits the message and returns to the workspace; `:q` cancels.

The commit menu also offers **Generate message**. With Codex installed and
authenticated, Red drafts a message from the staged diff while using recent
commit messages only as style examples; you can edit the draft before committing.

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

Agent features require Codex CLI 0.144.1 or newer and a completed `codex login`.
Run `red --agent-check` for an offline prerequisite report, or use
`red --agent-check --strict` to return a non-zero status when setup is incomplete.

### Ask inline

Press `Space i` to review, explain, or refactor code beside the current source.
In Normal mode, Red targets the enclosing function when syntax information is
available, falling back to the current line. In Visual or Visual Line mode, it
targets exactly your selection. Visual-block targets are rejected. Enter a
request such as `extract the condition into a named boolean` or `review this
function for edge cases`.

Inline code changes are one **unsaved, undoable editor transaction**. Comments
and explanations never modify source. The published v0.6.0 release requires
explicit review for every code change. **Coming in the next release:** Exact
foreground results apply immediately by default; set
`[agent] auto_apply_inline_edits = false` to review them first. Background
results and wider same-file edits always require explicit approval. In a review
diff, `a` approves, `d` declines, and `Enter` does not apply the change.

Use `u` to undo an applied result, `r` to refine it, `Space H` to inspect retained
inline history, or `A` to prepare a full Agent follow-up. The follow-up remains
an unsent draft until you submit it yourself.

### Open the full Agent workspace

Press `Space A` from Normal or Visual mode. Red sends a bounded source excerpt,
selection, relevant diagnostics, and authoritative unsaved buffer contents.
Codex reads and changes files through Red's workspace-confined editor tools;
revision-checked Agent writes are attributed to the conversation and **saved
to disk**. This differs intentionally from unsaved inline edits.

The published v0.6.0 release follows every file tool visually. **Coming in the
next release:** Tool calls run without forced playback pauses by default; set
`[agent] follow_tool_calls = true` to reveal each target and pause before it runs.

**Also coming in the next release:** Ask the Agent to explain a subsystem and
follow links in its answer directly to source-anchored annotations. Click the
model in the Agent header, press `Alt+m`, or run `:AgentModel` to choose the
model and reasoning effort for this conversation without modifying global Codex
configuration.

See the [agent workflow and safety contract](AGENT_WORKFLOW.md) for the full
interaction model, commands, path boundaries, and failure behavior.

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

Bindings to `EnterMode = "Command"` also work in panel navigation and modal
workspaces. Normal-mode command bindings are inherited by visual modes; an
explicit visual-mode binding takes precedence. Text input, searches, and
unfinished Vim commands keep their characters. For example, this makes `;`
an alternative to `:` without changing what it types in insert mode:

```toml
[keys.normal]
";" = { EnterMode = "Command" }
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
