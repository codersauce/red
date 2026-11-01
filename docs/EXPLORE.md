# Code explore

The sidebar follows `docs/DESIGN.md`: theme colors, a footer that sits under the content, an ellipsis when a line does not fit, and keycaps with the key on the section-header color.

Code explore is a reading mode. It keeps the editor you already use and adds a
sidebar, dimming, and a review trail so a function is easier to follow. It does
not send the buffer to a model and it does not ask you a question.

Turn it on with `Space E`, `\e`, or `:explore`. The sidebar opens on the right
when the window is wide enough, and a one-line trail sits above the status
line. `?` inside the `\` chord (`\?`) lists the keys in the sidebar.

`Ctrl-\` still detaches a session. The explore chord is a bare `\`.

## What you can see

The sidebar follows the cursor.

- **In** is the function that encloses the cursor.
- **Here** is the name under the cursor, what kind of name it is, and the
  syntax node around it (a call, a variable, a match, and so on).
- **Definition** is the matching name in the local index, or the `let` / parameter
  in this function.
- **Matches** lists occurrences of that name in the current function first,
  then other indexed files. These are shared spellings, not type-checked cross
  references.
- A `×N` count sits just after a function, struct, enum, trait, or type
  definition when the line is short enough to leave room. `N` is the number of
  name matches in the index.

## Keys

| Key | Action |
| --- | --- |
| `Space E`, `\e`, `:explore` | Toggle explore |
| `\q`, `:explore off` | Close it |
| `\f`, `\*` | Follow the name under the cursor through this function |
| double-click | Same follow, only while explore is on |
| `\s` | Dim other lines in this function |
| `\d` | Jump to the definition in the index |
| `\t` | Jump to the explicit type, when one is written down |
| `\j` / `\k` | Next / previous reference |
| `\b` / `\n` | Review trail back / forward |
| `\r` | Re-read the current file |
| `\R` | Index source files in the repo (capped) |
| `\p` | Index only this PR's changed files, or turn that scope off |
| `\?` | Show or hide the key list |
| `gd` | Unchanged. This still asks the language server |

`:explore` accepts the same verbs: `on`, `off`, `skeleton`, `follow`, `def`,
`type`, `back`, `forward`, `next`, `prev`, `index`, `refresh`, `pr`, `pr origin/main`,
and `help`.

Click a reference, definition, or trail crumb to jump there. Jumps are recorded
on the trail so you can walk back.

While explore is off, double-click still selects a word.

## Skeleton

`\s` fades the current function except:

- the signature
- calls whose name is defined in the indexed repo
- calls into the standard library that wrap a syscall or process (`std::fs`,
  `std::process`, `std::net`, `File::open`, `Command::new`, `println!`, and
  similar)

Nested functions stay readable. With PR scope on (`\p`), "defined in the repo"
narrows to definitions in the changed files. Syscall wrappers stay bright
either way.

## Index

Opening explore reads the current buffer immediately. It does not walk the
repository until you ask.

- `\r` reparses the file you are editing. That is the fast path while you move
  through a change.
- `\R` walks the git repo (respecting `.gitignore`) and parses up to 400 source
  files with Red's tree-sitter grammars. A later edit reparses only the open
  buffer.
- `\p` asks git for the changed files against `origin/main`, `main`, `master`,
  or a base you pass (`:explore pr origin/main`), then indexes those files
  only. The sidebar shows the PR file count and a `+added −removed` summary
  when `git diff --numstat` can provide one.

The index is a map from name to file and line. It does not know which `template`
you meant when two exist. The sidebar says so.

### SCIP and LSP

If `index.scip` (or another `*.scip` file) sits in the repository root, the
sidebar says it is present and unread. SCIP indexes are protobuf; this mode
does not decode them.

`gd` is still the language-server jump. The explore index does not call the
language server, so it works when no server is running and it stays
deterministic.

### Grammars

Tree-sitter reads the languages Red already ships grammars for, including Rust,
TypeScript, TSX, JavaScript, JSX, Lua, Bash, Fish, PowerShell, JSON, TOML, and
YAML. A buffer with no grammar still gets the trail and the key list. Husk uses
Red's own highlighter rather than a tree-sitter grammar, so explore does not
pretend to parse it.

## Limits

- Types are only the ones written in the source: `let` annotations, parameters,
  and `->` return types. Inferred types are not recovered.
- Reference counts and jumps are name matches.
- The repository scan stops at 400 files and skips files larger than 512 KiB.
- Skeleton detection for syscalls is a list of paths and names, not a model of
  the standard library.
