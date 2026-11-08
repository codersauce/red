---
title: "Theme Import"
summary: "Red imports VS Code color-theme JSON into its renderer theme model, preserving workbench colors, token styles, UI styles, and contrast repair rules."
topics: [architecture, themes, runtime-assets, configuration]
sources:
  - id: theme-model
    type: file
    path: src/theme/mod.rs
  - id: vscode-import
    type: file
    path: src/theme/vscode.rs
  - id: bundled-themes
    type: file
    path: themes/
  - id: theme-licenses
    type: file
    path: themes/THIRD_PARTY.md
  - id: startup-finalize
    type: file
    path: src/main.rs
---

# Theme Import

Theme import is the boundary that turns user, development, or embedded VS Code color-theme JSON into Red's concrete renderer theme. Startup resolves the configured theme through the runtime asset system, parses filesystem themes with `parse_vscode_theme`, parses embedded themes from their contents, and falls back to embedded `red.json` with a configuration diagnostic when the selected theme cannot load [@startup-finalize]. The importer keeps Red compatible with VS Code-style workbench colors and TextMate token rules while producing a single `Theme` value used by rendering, UI surfaces, cursor styling, selections, and plugin style lookup [@theme-model] [@vscode-import].

## Source Format And Bundled Corpus

Red's bundled theme directory contains many `.json` color themes and a third-party license manifest, so theme import must accept the format used by upstream VS Code and Neovim theme conversions rather than a Red-only schema [@bundled-themes] [@theme-licenses]. `parse_vscode_theme_contents` wraps the input in `json_comments::StripComments`, deserializes `name`, `colors`, and `tokenColors`, and ignores unknown source fields through the narrow `VsCodeTheme` structure [@vscode-import]. Invalid colors or required structural values still fail parsing, which lets startup report a broken selected theme instead of producing a partially initialized renderer [@vscode-import] [@startup-finalize].

Themes are selected by the configuration field `theme`, but import happens after configuration recovery. If the configured name cannot be resolved or parsed, startup appends `CFG302`, sets `loaded.config.theme` to `red.json`, and parses the embedded fallback contents [@startup-finalize]. That makes [Layered Config Recovery](../configuration/layered-config-recovery) responsible for reporting the bad setting and [Runtime Assets](../runtime/runtime-assets) responsible for locating candidate files.

## Internal Theme Model

The internal `Theme` model contains the editor base style, gutter style, statusline style, UI style set, token styles, raw workbench colors, and optional styles for line highlight, bracket match, find matches, selection, cursor, and errors [@theme-model]. Keeping both raw workbench colors and token styles matters because plugin-facing `ThemeStyleSpec` can resolve ordered foreground and background references against either workbench color keys or `scope:` token scopes [@theme-model].

Semantic scope lookup is intentionally compatible rather than exact-only. `Theme::get_style` tries a requested scope, walks parent scopes by trimming dot-separated suffixes, and adds Markdown aliases for common markdown highlighting names such as headings, list markers, fenced raw blocks, quote prefixes, and link titles [@theme-model]. The VS Code adapter also translates common TextMate scopes to Red-friendly semantic categories such as `function`, `function.method`, `type`, `property`, `punctuation.delimiter`, and `constant.builtin` [@vscode-import].

## Workbench Colors And UI Surfaces

The importer maps VS Code workbench colors into Red UI surfaces instead of using only token colors. Editor foreground and background come from `editor.foreground` and `editor.background`, with legacy scope-less token colors used only as fallback [@vscode-import]. Gutter, line highlight, selection, bracket match, find match, error, cursor, statusline, popup, dialog, picker, prompt, muted, and deprecated styles are derived from specific workbench color keys with internal fallback colors where VS Code lacks a direct equivalent [@vscode-import].

Statusline import uses `statusBar.*` colors, prominent or remote item colors, or selection-derived accents before falling back to Red defaults [@vscode-import]. Picker and dialog surfaces use quick input, editor widget, hover widget, list selection, input, placeholder, warning, and error color keys, while transparent values are ignored for backgrounds and borders that need real terminal colors [@vscode-import].

## Contrast Repair

Red repairs contrast at the theme layer for UI states that must remain readable in a terminal. `compose_selection_style` blends requested selection colors against the actual surface and enforces minimum contrast for both selection state and selected text [@theme-model]. `compose_synthetic_cursor_style` derives a cursor block from editor, content, and optional cursor colors, then enforces separate minimum contrast between cursor and cell background and between cursor foreground and cursor background [@theme-model].

The VS Code adapter uses the same contrast helpers when deriving picker selection and selection-derived statusline colors [@vscode-import]. `Theme::ensure_text_contrast` is another runtime helper that repairs arbitrary text style foregrounds against their resolved background using the selection text contrast threshold [@theme-model]. These repairs mean imported themes can preserve their intended palette while still giving Red accessible cursor, selection, and picker states.

## Runtime Boundary

Theme import produces a `Theme`; it does not persist config, list available files, or decide asset precedence. `Config::persist_theme` changes the user's selected theme name, [Runtime Assets](../runtime/runtime-assets) resolves theme files from user, `RED_RUNTIME`, and embedded layers, and startup owns fallback diagnostics when import fails [@startup-finalize]. Exact default field names and theme-related configuration values are covered in [Default Config](../../reference/configuration/default-config).
