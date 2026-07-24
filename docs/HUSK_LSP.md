# Husk language server

Husk ships a first-party language server for `.hk` and `.husk` files. Red
enables it by default; other editors can launch the same server with either:

```shell
husk lsp --stdio
red husk lsp --stdio
husk-lsp
```

All three entry points speak Language Server Protocol over standard input and
output. Protocol logs and diagnostics never share standard output with framed
messages.

## Editor features

The server analyzes the current unsaved document revision rather than requiring
files to be saved or compiled. Positions and incremental edits use LSP UTF-16
code units, including correct handling of non-BMP Unicode characters.

| Area | Supported features |
| --- | --- |
| Feedback | Parser, type, package, and dependency diagnostics through push and pull diagnostics |
| Authoring | Contextual completion, signature help, hover signatures and documentation, type inlay hints |
| Navigation | Definition, declaration, type-definition and implementation lookup, references, document highlights |
| Refactoring | Prepare rename, workspace rename, missing-semicolon quick fixes, organize imports |
| Structure | Document and workspace symbols, folding ranges, selection ranges, incoming and outgoing call hierarchy |
| Presentation | Full semantic tokens for declarations, references, keywords, literals, and operators |
| Formatting | Whole-document and range formatting with client-selected indentation width |

Recovered parser syntax remains available while a document is incomplete, so
features that do not depend on the broken construct continue to work.
Dependency stubs are intentionally read-only: navigation and hover can enter
them, but rename never edits them.

## Packages and external crates

For a package with `Husk.toml`, the server uses native Husk semantics and the
same public-module descriptors as package compilation. It indexes every source
module in deterministic package order.

Crate-backed extensions come exclusively from the verified package state:

1. `Husk.toml` declares the crate adapter.
2. `Husk.lock` pins its version, component digest, adapter selection, features,
   and specializations.
3. The installed `.husk/extensions/` bundle is preferred; the exact vendored
   bundle is a read-only fallback.
4. The component export surface becomes a typed Husk declaration under
   `.husk/lsp/<component-digest>/<module>.hk`.
5. Adapter report documentation is attached to matching functions for hover
   and completion.

Indexing validates the manifest/lock relationship, bundle metadata, component
SHA-256, optional adapter-report SHA-256, component exports, type mappings, and
deny-by-default capability policy. It does not invoke Cargo, resolve a crate,
download anything, instantiate guest code, or execute an adapter. After a
clone, prepare the exact offline state once:

```shell
red husk install --locked --offline
```

A missing, stale, or invalid adapter produces `HUSK-DEP0001` diagnostics while
local source analysis continues. Generated stubs are content-addressed and are
only rewritten when their content changes.

## Red integration

Red's embedded server definition launches `red husk lsp --stdio`, selects
`Husk.toml` and then `.git` as workspace roots, and routes both supported file
extensions to language ID `husk`.

Real packages always use `SemanticProfile::Native`. Loose Husk files opened as
Red plugins use the legacy JavaScript compatibility profile and receive Red's
trusted host declarations, so `red::*`, `Json`, callback types, and editor host
signatures participate in completion and type checking.

The equivalent custom initialization options are:

```json
{
  "semanticProfile": "native",
  "looseSemanticProfile": "legacyJavaScript",
  "declarations": ["optional trusted Husk declaration source"]
}
```

`semanticProfile` explicitly selects a profile for every workspace.
`looseSemanticProfile` is only used when no `Husk.toml` is discoverable.
Clients can update `husk.semanticProfile` and `husk.cfgFlags` through
`workspace/didChangeConfiguration`.

## Boundaries

- One process owns one workspace folder and at most 256 indexed Husk files.
- Source files and overlays are bounded to 1 MiB each.
- JSON-RPC headers are bounded to 16 KiB and messages to 16 MiB.
- Only local `file:` URIs inside the selected workspace are accepted.
- Symlinked package inputs, bundles, artifacts, and generated-stub targets fail
  closed.
- External dependency names are derived from verified component exports, not
  from generated Rust source or Cargo metadata at edit time.

The implementation is split between `husk-analysis`, which owns recovered
syntax, semantics, overlays, UTF-16 indexing, symbols, and formatting, and
`husk-lsp`, which owns bounded JSON-RPC transport, protocol capabilities,
locked dependency indexing, and editor request handling.
