---
title: "LSP Configuration"
summary: "LSP configuration defines the global language-server switches, named server launch settings, document selectors, default servers, and Red's embedded Husk server setup."
topics: [reference, lsp, configuration, husk]
sources:
  - id: config
    type: file
    path: src/config.rs
  - id: defaults
    type: file
    path: default_config.toml
  - id: husk-doc
    type: file
    path: docs/HUSK_LSP.md
---

LSP configuration in Red is the TOML-backed contract for enabling language-server support, formatting on save, and routing file extensions to named server definitions. The `Config` type contains `lsp: LspConfig`, and `LspConfig` has three fields: `enabled`, `format_on_save`, and `servers` [@config]. Server definitions describe process launch, document selectors, workspace-root discovery, environment additions, initialization options, and optional workspace names [@config]. Runtime routing for these fields is described by [LSP Client Lifecycle And Routing](../../architecture/lsp/client-lifecycle-and-routing), while capability expectations are summarized by [LSP Capabilities](../../concepts/lsp/capabilities).

## Top-Level Fields

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `lsp.enabled` | boolean | `true` | Master switch for language-server activity [@config]. |
| `formatting.on_save` | boolean | `true` | Formats supported documents before saving; set to `false` to disable [@config]. |
| `formatting.trim_trailing_whitespace` | boolean | `true` | Removes trailing spaces and tabs before save-time formatting [@config]. |
| `formatting.trim_trailing_whitespace_exclude` | array of strings | `["gitcommit", "markdown"]` | Language ids that preserve trailing whitespace during save-time formatting [@config]. |
| `formatting.provider` | string | `"auto"` | Selects `auto`, `external`, or `lsp` formatting [@config]. |
| `lsp.format_on_save` | boolean | unset | Legacy alias for `formatting.on_save`; the modern key wins within the same config layer [@config]. |
| `lsp.servers` | table of named server configs | embedded defaults | Launch and routing definitions keyed by server name [@config]. |

The default config comments show the public TOML shape and note that language servers are selected by file extension [@defaults]. Existing single-language configs remain supported through `language_id` with `file_extensions` or `filenames`, while servers that handle more than one language can use repeated `documents` entries [@defaults] [@config].

## Server Fields

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `command` | string | yes | Executable launched directly, without a shell [@config]. |
| `args` | array of strings | no | Arguments passed to the command [@config]. |
| `language_id` | string | legacy selector | LSP language id for single-selector configs [@config]. |
| `file_extensions` | array of strings | legacy selector | Extensions for single-selector configs [@config]. |
| `filenames` | array of strings | legacy selector | Exact file names for single-selector configs [@config]. |
| `documents` | array of tables | no | Preferred selector list for one server handling one or more language ids [@config]. |
| `root_markers` | array of strings | no | Files or directories searched upward to choose a workspace root [@config]. |
| `env` | table | no | Environment additions supplied only to the server process [@config]. |
| `initialization_options` | JSON value | no | Options passed during LSP initialization; this field is runtime-only in serialization [@config]. |
| `settings` | JSON value | no | Settings returned to server `workspace/configuration` requests [@config]. |
| `workspace_name` | string | no | Display name reported for the workspace folder [@config]. |

`LanguageServerConfig::documents()` normalizes the two selector styles. If explicit `documents` exist, they are returned as-is; otherwise a non-empty `language_id` with non-empty `file_extensions` or `filenames` becomes a single `LanguageDocumentConfig` [@config]. Tests cover additional server definitions, `format_on_save`, legacy selector adaptation, `workspace_name`, settings round-tripping, and multi-document selector parsing [@config].

## Default Servers

Red embeds default definitions for these server keys:

| Key | Command | Documents or legacy selector | Root markers |
| --- | --- | --- | --- |
| `rust` | `rust-analyzer -v` | `rust` for `rs` | `Cargo.toml`, `.git` [@config] |
| `husk` | current Red executable with `husk lsp --stdio` | `husk` for `hk`, `husk` | `Husk.toml`, `.git` [@config] |
| `fish` | `fish-lsp start` | `fish` for `fish` | `config.fish`, `.git` [@config] |
| `typescript` | `typescript-language-server --stdio` | TypeScript, TSX, JavaScript, and JSX selectors | `package.json`, `tsconfig.json`, `jsconfig.json`, `.git` [@config] |
| `python` | `pyright-langserver --stdio` | `python` for `py`, `pyw` | `pyproject.toml`, `setup.py`, `requirements.txt`, `.git` [@config] |
| `markdown` | `marksman server` | `markdown` for `md`, `markdown` | `.marksman.toml`, `.git` [@config] |
| `json` | `vscode-json-language-server --stdio` | `json` for `json` | `package.json`, `.git` [@config] |
| `toml` | `taplo lsp stdio` | `toml` for `toml` | `taplo.toml`, `Cargo.toml`, `.git` [@config] |
| `yaml` | `yaml-language-server --stdio` | `yaml` for `yaml`, `yml` | `.git` [@config] |
| `lua` | `lua-language-server` | `lua` for `lua` | Lua config markers and `.git` [@config] |

The default TOML comments describe the same public set as covering Rust, Fish, Markdown, JavaScript/TypeScript, JSON, TOML, YAML, Python, and Lua [@defaults]. Husk is also embedded by code and covered by the dedicated Husk language-server documentation [@config] [@husk-doc].

Fish syntax highlighting works without a language server. For completions, diagnostics, formatting, and navigation, install `fish-lsp`; Red automatically launches `fish-lsp start` for `.fish` files and discovers the workspace using `config.fish` or `.git` [@config] [@defaults].

## Husk Integration

The embedded Husk definition launches the current Red executable with `husk lsp --stdio`, routes `.hk` and `.husk` to language id `husk`, and searches for `Husk.toml` before `.git` as workspace roots [@config]. Its initialization options set `looseSemanticProfile` to `legacyJavaScript` and include trusted Red plugin host declarations, so loose plugin files receive Red host types even when no Husk package exists [@config].

The Husk LSP documentation states that Red enables the first-party server by default and that the same server can also be launched as `husk lsp --stdio`, `red husk lsp --stdio`, or `husk-lsp` [@husk-doc]. It also documents the corresponding custom initialization options: `semanticProfile`, `looseSemanticProfile`, and optional declaration sources [@husk-doc].

## Example Shapes

Legacy single-selector config:

```toml
[lsp]
format_on_save = true

[lsp.servers.rust]
command = "rust-analyzer"
args = ["-v"]
language_id = "rust"
file_extensions = ["rs"]
root_markers = ["Cargo.toml", ".git"]
```

Multi-document selector config:

```toml
[lsp.servers.typescript]
command = "typescript-language-server"
args = ["--stdio"]
root_markers = ["package.json", "tsconfig.json", "jsconfig.json", ".git"]

[[lsp.servers.typescript.documents]]
language_id = "typescript"
file_extensions = ["ts"]

[[lsp.servers.typescript.documents]]
language_id = "javascript"
file_extensions = ["js", "mjs", "cjs"]
```

Both shapes are shown in `default_config.toml`, and the Rust config tests verify that both deserialize into usable server selectors [@defaults] [@config].
