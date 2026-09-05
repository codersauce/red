# Language extensions

Red uses one language definition for syntax highlighting, exact filename and
extension and shebang detection, comment templates, indentation, and language-server
routing. Definitions can live in your configuration or in an installable
external plugin package.

## Add a language in your configuration

Add a table to `~/.config/red/config.toml`:

```toml
[languages.buildspec]
extensions = ["build"]
filenames = ["Buildfile"]
aliases = ["build-script"]
comment = "# %s"
indent_width = 2

[languages.buildspec.grammar]
builtin = "rust"
# Optional: replace the bundled Rust structural queries.
# textobjects = ["queries/buildspec/textobjects.scm"]

[languages.buildspec.lsp]
command = "build-language-server"
args = ["--stdio"]
root_markers = ["Buildfile", ".git"]

[languages.buildspec.lsp.settings.build]
validate = true
```

Extensions are case-insensitive and may start with a dot. Exact filenames are
case-sensitive and take precedence over extensions. When no filename or extension matches, Red reads up to 512 characters from the
first line and checks its shebang. Bundled interpreters include `sh`, `bash`,
`dash`, `ash`, `zsh` (using Bash syntax), `fish`, `pwsh`, `powershell`, `node`,
`nodejs`, `lua`, `luajit`, and `husk`. No executable permission is required.

Add interpreter basenames to any language definition or language-pack manifest:

```toml
[languages.python]
shebangs = ["python", "python3", "pypy", "pypy3"]
```

This registers detection; Python highlighting still requires its language pack.
Direct paths and common `env` forms are supported, including `env -S bash -eu`,
`-i`, `-u NAME`, `-C DIR`, `--`, and environment assignments. Numeric interpreter
versions such as `python3.12` fall back to the registered base name. Quoted
commands, shell expansion, and arbitrary wrapper commands are not interpreted.
An unknown or overlong shebang leaves the language undetected.

Detection uses the current buffer, so editing the first line updates syntax even
when it is offscreen. `:syntax <language>` overrides detection and `:syntax off`
disables highlighting and structural operations. Shebang detection also applies
to full-source previews. LSP routing still requires a filename or extension
selector; this feature does not attach language servers to extensionless files.

Aliases are accepted by
`:syntax build-script`, syntax completion, and Markdown code fences.

Every field is optional. A grammar-free language can still provide syntax
selection, comments, indentation, or an LSP. Use `lsp.server = "typescript"`
to attach another language to an existing named server instead of starting a
separate process. Explicit `[lsp.servers.<name>]` definitions and explicit
`[commenting.languages]` entries take precedence over generated defaults.

`initialization_options` is sent in the LSP `initialize` request. Nested
`settings` values are returned to `workspace/configuration` requests; dotted
sections select nested objects and unknown sections resolve to JSON `null`.

For Rust, Red also reads `rust-analyzer.rustfmt.extraArgs` from the nearest
project-local `.vscode/settings.json`. Settings files may contain comments and
trailing commas. Red searches upward from the Cargo workspace but never beyond
the Git repository, and applies these arguments only to that project's language
server. Explicit `initialization_options` or `settings` in your Red configuration
take precedence. Other VS Code settings, including executable and environment
overrides, are not imported.

## Load a native Tree-sitter grammar

Reuse an installed Neovim parser and its queries:

```toml
[languages.css]
extensions = ["css"]
aliases = ["stylesheet"]
comment = "/* %s */"
indent_width = 2

[languages.css.grammar]
path = "~/.local/share/nvim/site/parser/css.so"
symbol = "tree_sitter_css"
highlights = [
  "~/.local/share/nvim/lazy/nvim-treesitter/runtime/queries/css/highlights.scm",
]
textobjects = ["~/.config/red/queries/css/textobjects.scm"]
```

Native grammar shared libraries execute arbitrary code inside the editor. Red
never loads one until you explicitly approve its canonical path and exact
SHA-256 digest:

```shell
red language trust css
```

You can also set `trusted = true` directly in the language's grammar table; that
configuration change is itself explicit consent. Approval is stored in
`trusted-grammars.json` with owner-only permissions. Replacing the library
invalidates its approval. Revoke approval with:

```shell
red language untrust css
```

Red copies approved bytes to an immutable digest-addressed grammar cache before
loading them. Missing symbols, incompatible Tree-sitter ABIs, invalid queries,
and unapproved or changed binaries quarantine only the affected language at
startup.

## Navigate syntax-aware text objects

Red bundles structural Tree-sitter queries for Rust, Markdown, JavaScript, JSX,
TypeScript, TSX, JSON, TOML, YAML, Bash, Fish, PowerShell, and Lua. Available
objects depend on the language's query: not every language defines calls,
functions, classes, comments, and parameters.

First-party C, C#, C++, CSS, Go, HTML, Java, Kotlin, PHP, Python, Svelte,
Swift, and Vue packs also receive compatible structural queries. Older installed
packs use Red's bundled fallbacks immediately; newer releases declare and ship
their own query files. Explicit package or user queries take precedence. These
operations use Tree-sitter and do not require the optional language server.

| Keys | Operation |
| --- | --- |
| `]m`, `[m` | Move to the next or previous call. |
| `]f`, `[f` | Move to the next or previous function. |
| `]c`, `[c` | Move to the next or previous class or equivalent declaration. |
| `am`, `im` | Select an outer or inner call. |
| `af`, `if` | Select an outer or inner function. |
| `ac`, `ic` | Select an outer or inner class. |
| `ak`, `ik` | Select an outer or inner comment. |
| `Space ] a`, `Space [ a` | Swap a parameter with its next or previous sibling. |
| `Space ] m`, `Space [ m` | Swap a function with its next or previous sibling. |

Structural motions accept counts, work in Visual mode and after operators, and
record jumps. Objects work with delete, change, yank, and case-change operators;
outer functions and classes use linewise selections and registers. Swaps stay
inside the same syntactic container, preserve separators, and form one undoable,
repeatable edit. Existing `Space n` and `Space p` plugin/navigation bindings are
unchanged.

`:syntax off` disables structural operations. Languages without structural
queries, including Husk and Git commit messages, remain editable without them.
The editor lazily parses documents up to 2 MiB, caches their syntax trees,
queries only the requested object kind, and progressively bounds directional
searches in larger documents. Edits, syntax changes, and language reloads
invalidate the affected cached state.

Configure `languages.<id>.grammar.textobjects` with an ordered list of query
files to replace a reused grammar's bundled structural queries or provide
queries for a native grammar. Supported capture names are `@call.outer`,
`@call.inner`, `@function.outer`, `@function.inner`, `@class.outer`,
`@class.inner`, `@comment.outer`, `@comment.inner`, `@parameter.outer`, and
`@parameter.inner`. Repeated captures in one match form a single range.

Query files must already include any inherited patterns. Standard Tree-sitter
predicates such as `#eq?` and `#match?` work; Neovim-specific `; inherits:`,
`#offset!`, and Lua predicates are not interpreted. Unsupported custom
predicates are rejected when the language configuration is loaded. Bundled
upstream queries are normalized for this runtime, with comment and Fish function
interiors supplied by Red when needed.

## Browse and install language packs

Open **Language packs** from the command palette to browse Red's catalog. The
picker labels official and curated packs separately from custom sources, shows
host and target compatibility, and reports missing language-server commands.
Installing a pack is always an explicit action.

The same catalog is available from the CLI:

```shell
red plugin catalog
red plugin install --catalog go-language
red plugin install --catalog python-language --trust-native-grammars
```

Catalog entries resolve to immutable, target-specific release archives. Red
checks the archive's declared byte length and SHA-256 digest before extracting
it, validates its manifest against the catalog entry, and verifies every
bundled grammar digest. The catalog establishes provenance and integrity; it
does not make native grammar code safe. Choose the separate approval action in
the picker or pass `--trust-native-grammars` only when you want Red to load
those exact verified grammar bytes:

```shell
red plugin install --catalog swift-language --trust-native-grammars
```

The default catalog is published from
[`codersauce/red-language-packs`](https://github.com/codersauce/red-language-packs).
Set `RED_PLUGIN_CATALOG_URL` or pass `--catalog-url` to use another catalog that
follows the same schema.

## Create or install a custom language package

A package needs `red-plugin.toml` but does not need a Husk entrypoint or native
companion:

```toml
schema_version = 1

[plugin]
id = "acme-languages"
name = "Acme language pack"
version = "1.0.0"
red_api = "^0.6.0"

[languages.acme]
extensions = ["acme"]
filenames = ["Acmefile"]
comment = "// %s"
indent_width = 4

[languages.acme.grammar]
symbol = "tree_sitter_acme"
highlights = ["queries/acme/highlights.scm"]
textobjects = ["queries/acme/textobjects.scm"]

[languages.acme.grammar.targets.aarch64-apple-darwin]
path = "grammars/aarch64-apple-darwin/acme.dylib"

[languages.acme.grammar.targets.x86_64-unknown-linux-gnu]
url = "https://github.com/acme/red-languages/releases/download/v1.0.0/acme.so"
sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"

[languages.acme.lsp]
command = "acme-lsp"
args = ["--stdio"]
root_markers = ["Acmefile", ".git"]
```

Bundled grammar and query paths are package-relative, cannot traverse outside
the package, and cannot escape through symlinks. Downloaded grammars must use
a GitHub HTTPS URL and a matching SHA-256 digest.

The editor's **Add custom source** action accepts either a local path or
`owner/repository[@git-ref]`. Custom sources are clearly marked as unreviewed.
On the CLI, install them explicitly:

```shell
red plugin install --path ./acme-languages
red plugin install acme/red-languages
```

Omit `--trust-native-grammars` to install any package without approving its
native grammar. Run `red language trust acme` after inspecting the installed
artifact, or use the manager's trust action. A package cannot self-approve
native code through its manifest.

If a package and user configuration define the same language ID, the explicit
user definition wins.

First-party language packs are maintained together in the language-pack
monorepo, while each pack keeps an independent version, release tag, artifact,
and catalog entry. For example, the Swift pack supplies its own Tree-sitter
grammar, highlight queries, SwiftPM root detection, and SourceKit-LSP
configuration without adding Swift-specific code to Red.

Language packs can also supply an injection query through
`languages.<id>.grammar.injections`. Both dynamic `@injection.language`
captures and static Tree-sitter query properties are supported:

```scheme
((script_element
  (raw_text) @injection.content)
 (#set! injection.language "javascript"))
```

Injected languages are optional: Red highlights them only when their language
is already available. Opening one pack never installs or approves another
pack's native grammar.

## Reload without restarting

After editing your configuration, installing a package, or approving a grammar,
run:

```vim
:languages reload
```

The command is also available as **Reload language definitions** in the command
palette. Red prepares and validates the entire new language registry before
replacing the existing one, refreshes editor highlighting and workspace
previews, preserves open and unsaved buffers, and restarts only language-server
processes whose definitions actually changed. Invalid reloads leave the current
language configuration active. There is no automatic filesystem watching or
implicit native-code approval.

## Indentation queries

Red owns newline edits, cursor movement, indentation width, and undo. A language
pack supplies declarative Tree-sitter rules, without a running language server:

```toml
[languages.example.grammar]
indents = ["queries/indents.scm"]
```

This field requires host API `^0.12.0`. Explicit query lists replace bundled
indentation rules. Query files use Red's version-1 capture contract:

- `@indent.begin`: opens one indentation level.
- `@indent.end`: closes the matching level and aligns with its opener.
- `@indent.branch`: aligns with an opener without closing its level.
- `@indent.ignore`: makes a comment or literal opaque, preserving multiline text.
- `@indent.zero`: aligns a leading token at column zero.
- `@indent.match`: supplies a dynamic matching key, such as a tag name.
- `@indent.continuation`: marks an explicit line continuation; consecutive
  continuations share one indentation level and the completed statement resets it.

Brackets match automatically. For keywords or tag nodes, use
`(#set! indent.match "group")` on both matching patterns. Unknown captures,
properties, or custom predicates are rejected. Multiple openers on one source
line contribute one level. New lines inherit nearby actual indentation plus
the structural difference; leading closers align with their matching opener.
Unfinished syntax is supported, and size/time limits fall back to ordinary
auto-indent. Python retains its established provider during migration.

Use `red language check-indent fixtures.json` to test the effective, installed
language configuration. Native grammars still require explicit trust. Fixtures
are a JSON array with `name`, `language`, `source`, zero-based `line`,
`expected` display columns, and optional `width` (default 4). Include a blank
target line in `source` to test opening a line; include a closing token on the
target to test dedenting.
