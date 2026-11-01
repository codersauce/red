# Code explore

The sidebar follows `docs/DESIGN.md`: a `sideBar` column from the top row, one `│` on the editor edge, and three levels. `Explore` is the title, bold, with the Neo-tree root icon. `Inside`, `Here`, `Definition`, and `Matches` are bold section headers in the content color, with one blank row above each. Names are regular weight. Notes and the definition count stay muted and inline. `DIM` and `REVIEW` stay on the trail row.

Code explore is a reading mode. It keeps the editor you already use and adds a
sidebar, dimming, and a review trail so a function is easier to follow. It does
not send the buffer to a model and it does not ask you a question.

Turn it on with `Space e e`, `Space E`, or `:explore`. The sidebar opens on the
right when the window is wide enough. The row above the status line is
where the cursor is now: the enclosing function when there is one, the kind,
then the name, joined by ` › `. On `let template` inside `review` that is
`review › variable › template`. On `struct LaunchSpec;` it is
`struct › LaunchSpec`. A repeated word is dropped, and the word `explore` is
not used as a stand-in. After a jump the same row stays on the cursor, and
the right end shows the trail position, such as `2/2`. `Space e ?` opens Keyboard shortcuts scoped to the explore
bindings. Which-key, after `Space e`, shows only the next key.

`Space` is the same leader as the rest of Red. After `Space`, `e` is the
explore group, and the which-key popup names it "Explore". `Ctrl-\` still
detaches a session. `Space e` used to open the error list; that list is now
`Space D`.

## What you can see

The sidebar follows the cursor. `Explore` shares the top row with the file name.

- **In** is the function that encloses the cursor.
- **Here** is the name under the cursor, what kind of name it is, and the
  syntax node around it (a call, a variable, a match, and so on).
- **Definition** is the matching name in the local index, or the `let` / parameter
  in this function.
- **Matches** lists occurrences of that name in the current function first,
  then other indexed files. These are shared spellings, not type-checked cross
  references.
- A plain count sits just after a function, struct, enum, trait, or type
  definition when the line is short enough to leave room. The number is the
  name matches in the index, in the status-line color. Dim and review show as
  `DIM` and `REVIEW` on the location row, in the same prominent style as `NORMAL`.

## Keys

| Key | Action |
| --- | --- |
| `Space e e`, `Space E`, `:explore` | Toggle explore |
| `Space e q`, `:explore off` | Close it |
| `Space e f`, `Space e *` | Follow the name under the cursor through this function |
| double-click | Same follow, only while explore is on |
| `Space e s` | Dim everything in this function except project calls and system-touching library calls |
| `Space e d` | Jump to the definition in the index |
| `Space e t` | Jump to the explicit type, when one is written down |
| `Space e j` / `Space e k` | Next / previous match |
| `Space e b` / `Space e n` | Review trail back / forward |
| `Space e r` | Re-read the current file |
| `Space e R` | Index source files in the repo (capped) |
| `Space e p` | Index only this PR's changed files, or turn that scope off |
| `Space e ?` | Open Keyboard shortcuts for these bindings |
| `gd` | Unchanged. This still asks the language server |

`:explore` accepts the same verbs: `on`, `off`, `skeleton`, `follow`, `def`,
`type`, `back`, `forward`, `next`, `prev`, `index`, `refresh`, `pr`, `pr origin/main`,
and `help`.

Click a reference or definition to jump there. Jumps are recorded on the trail
so `Space e b` and `Space e n` can walk back. The bottom row does not list
those jumps. It keeps showing the cursor, and adds `1/2` style counter while
the trail has entries.

While explore is off, double-click still selects a word.

## Skeleton

`Space e s` fades tokens in the current function except:

- the signature
- calls whose name is defined in the indexed repo
- calls into the standard library that wrap a syscall or process (`std::fs`,
  `std::process`, `std::net`, `File::open`, `Command::new`, `println!`, and
  similar)

Nested functions stay readable. With PR scope on (`Space e p`), "defined in the repo"
narrows to definitions in the changed files. Syscall wrappers stay bright
either way.

## Index

Opening explore reads the current buffer immediately. It does not walk the
repository until you ask.

- `Space e r` reparses the file you are editing. That is the fast path while you move
  through a change.
- `Space e R` walks the git repo (respecting `.gitignore`) and parses up to 400 source
  files with Red's tree-sitter grammars. A later edit reparses only the open
  buffer.
- `Space e p` asks git for the changed files against `origin/main`, `main`, `master`,
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
YAML. A buffer with no grammar still gets the trail. Husk uses
Red's own highlighter rather than a tree-sitter grammar, so explore does not
pretend to parse it.

## Limits

- Types are only the ones written in the source: `let` annotations, parameters,
  and `->` return types. Inferred types are not recovered.
- Reference counts and jumps are name matches.
- The repository scan stops at 400 files and skips files larger than 512 KiB.
- Skeleton detection for syscalls is a list of paths and names, not a model of
  the standard library.
