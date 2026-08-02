---
title: "Syntax Services"
summary: "Syntax services choose a buffer language, produce byte-range highlight spans, cache viewport parses, and feed matching-token motions without making syntax choice a text mutation."
topics: [architecture, editor, syntax, rendering, vim]
sources:
  - id: highlighter
    type: file
    path: src/highlighter.rs
  - id: buffer
    type: file
    path: src/buffer.rs
  - id: editor
    type: file
    path: src/editor.rs
  - id: matchit
    type: file
    path: src/matchit.rs
---

Syntax services in Red are editor-owned helpers for language selection, highlighting, and matching-token navigation. `Highlighter` maps filenames, extensions, and language names to bundled syntax definitions, then returns `StyleInfo` spans over UTF-8 byte ranges for the text slice it was given [@highlighter]. The editor decides which language applies to a buffer, caches parsed viewport slices by buffer revision and syntax selection, and clears those caches when the user changes syntax mode [@editor]. Match navigation uses the same current language identity, but it returns editor text positions instead of byte spans, keeping syntax services connected to [Editor Coordinate Systems](../../concepts/editor/coordinate-systems) without owning text mutation [@matchit].

## Language Selection Boundary

Each buffer stores a `SyntaxSelection` value: `Auto` detects from the file name, `Off` disables highlighting, and `Language(String)` forces a canonical language id [@buffer]. The selection is buffer-local and does not change the content revision or dirty state, so choosing `:syntax rust` affects presentation and syntax-aware helpers without becoming an undoable edit [@buffer].

The editor resolves that buffer setting before rendering. `Auto` asks the highlighter for a language id from the file extension, `Off` yields no language, and `Language` is normalized through `Highlighter::language_id_for_name` so aliases such as extensions and language names can resolve to the canonical bundled id [@editor]. The `SetSyntax` action accepts `auto`, `off`, or a known language, removes the active buffer's highlight cache entry, clears the bracket-match cache, forces a redraw, and reports the applied label [@editor].

## Highlight Production

The highlighter owns the parser and query cache for tree-sitter languages. It builds `LanguageHighlighter` entries lazily, combining bundled highlight queries, compiling an optional injection query, and mapping capture names to theme styles [@highlighter]. Supported tree-sitter-backed languages include Rust, Markdown, JavaScript, JSX, TypeScript, TSX, JSON, TOML, YAML, Python, Bash, Fish, PowerShell, and Lua [@highlighter].

Husk is a special path. The `husk` language id maps `.hk` and `.husk` files, but `highlight_with_depth` bypasses tree-sitter and calls the Husk lexer, assigning theme scopes to comments, keywords, numeric and string literals, builtin types, builtin constants, builtin variables, and operators [@highlighter]. This keeps plugin-language highlighting available even though Husk syntax is not represented by a tree-sitter grammar in this module [@highlighter].

Markdown supports language injections. The markdown definition has an injection query, `collect_injections` extracts `injection.language` and `injection.content`, and nested calls to `highlight_with_depth` add injected spans back into the parent byte range until `MAX_INJECTION_DEPTH` is reached [@highlighter]. YAML takes the opposite special case: `requires_document_prefix` returns true because parsing an arbitrary indented viewport can lose mapping and scalar context [@highlighter].

## Viewport Cache And Rendering

Viewport highlighting belongs to the editor because render-time slices depend on the active buffer, viewport, wrap layout, file name, syntax selection, and current buffer revision. `viewport_highlight_spans` caches `HighlightSpan` values per buffer index with the revision, file, language id, parse start line, line offsets, and spans for a parsed slice [@editor]. Cache hits slice spans back to the requested viewport, which lets line-by-line scrolling reuse work instead of reparsing every visible row [@editor].

The cache deliberately parses more than the exact viewport when it can. Same-document scrolling gets a margin of a screenful, while a new document or syntax context gets a smaller margin; YAML and other prefix-sensitive languages force `parse_start` to zero [@editor]. The cache refuses oversized parse slices and is capped at 32 entries before being cleared, so highlighting can fail soft rather than blocking rendering on very large spans [@editor]. This page covers the syntax-specific part of the [rendering pipeline](rendering-pipeline); the [buffers and windows](buffers-and-windows) page covers the window state that chooses which buffer region is visible.

## Matchit Integration

Matchit is separate from color highlighting but shares syntax identity. The editor calls `matchit::find_motion`, `find_unmatched_group`, and `select_around` with the current buffer contents, cursor text position, current language id, and `config.matchit` [@editor]. If a syntax language is forced, that language is used; otherwise the editor asks the highlighter to infer a language from the file and falls back to the buffer file type [@editor].

`matchit.rs` combines always-available delimiter pairs with language-aware groups when matchit is enabled. It indexes single-character bracket pairs lazily by buffer id, revision, and configured pairs, skips string and comment ranges for normal token matching, includes builtin Bash `if`/`elif`/`else`/`fi` groups, accepts configured regex groups per language, and recognizes XML-like tags [@matchit]. Bracket matching remains available even when advanced matchit navigation is disabled, which lets ordinary delimiter feedback stay cheaper and more predictable than full token navigation [@matchit].
