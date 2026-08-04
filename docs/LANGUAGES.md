# Language extensions

Red uses one language definition for syntax highlighting, exact filename and
extension detection, comment templates, indentation, and language-server
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

[languages.buildspec.lsp]
command = "build-language-server"
args = ["--stdio"]
root_markers = ["Buildfile", ".git"]

[languages.buildspec.lsp.settings.build]
validate = true
```

Extensions are case-insensitive and may start with a dot. Exact filenames are
case-sensitive and take precedence over extensions. Aliases are accepted by
`:syntax build-script`, syntax completion, and Markdown code fences.

Every field is optional. A grammar-free language can still provide syntax
selection, comments, indentation, or an LSP. Use `lsp.server = "typescript"`
to attach another language to an existing named server instead of starting a
separate process. Explicit `[lsp.servers.<name>]` definitions and explicit
`[commenting.languages]` entries take precedence over generated defaults.

`initialization_options` is sent in the LSP `initialize` request. Nested
`settings` values are returned to `workspace/configuration` requests; dotted
sections select nested objects and unknown sections resolve to JSON `null`.

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

## Browse and install language packs

Open **Language packs** from the command palette to browse Red's catalog. The
picker labels official and curated packs separately from custom sources, shows
host and target compatibility, and reports missing language-server commands.
Installing a pack is always an explicit action.

The same catalog is available from the CLI:

```shell
red plugin catalog
red plugin install --catalog go-language
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
