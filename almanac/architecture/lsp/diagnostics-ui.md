---
title: "Diagnostics UI"
summary: "Red presents URI-keyed LSP diagnostics through editor-owned gutter signs, statusline counts, diagnostic pickers, and a cursor-anchored line popup."
topics: [architecture, lsp, diagnostics, ui]
sources:
  - id: editor
    type: file
    path: src/editor.rs
  - id: picker
    type: file
    path: src/editor/diagnostics_picker.rs
  - id: popup
    type: file
    path: src/ui/diagnostic_info.rs
  - id: geometry
    type: file
    path: src/ui/geometry.rs
  - id: hover
    type: file
    path: src/ui/hover_info.rs
  - id: rendering
    type: file
    path: src/editor/rendering.rs
  - id: defaults
    type: file
    path: default_config.toml
  - id: host-api
    type: file
    path: src/plugin/host_api.json
---

# Diagnostics UI

Red's diagnostic UI is editor-owned display state built from the latest URI-keyed LSP diagnostic snapshot. The editor accepts diagnostics only when `show_diagnostics` is enabled, normalizes incoming URIs, stores diagnostics by URI, and then feeds four user-facing surfaces: gutter signs, statusline counts, diagnostic pickers, and a line-scoped popup [@editor]. This keeps diagnostics tied to editor buffer identity and render state instead of making bundled plugins interpret LSP snapshots.

## Defaults And Actions

Diagnostics are enabled in the shipped default configuration with `show_diagnostics = true`, `[diagnostics].gutter_signs = true`, and `[diagnostics].icon_style = "nerd_font"` [@defaults]. The default statusline puts `diagnostics` between `mode` and `git_branch`, so counts appear without user configuration when diagnostics exist [@defaults] [@rendering].

Normal-mode `D` maps to `ShowLineDiagnostics`, while Space `d` opens all diagnostics and Space `e` opens error diagnostics [@defaults]. `ShowLineDiagnostics` is also a plugin host `execute` call introduced in the `0.8.0` schema, so plugins can request the same editor-owned popup without owning its rendering or data model [@host-api].

## Gutter And Statusline

The diagnostic gutter namespace is `diagnostics`. When diagnostics change, the editor picks at most one sign per diagnosed line and chooses the highest-priority severity, with errors and diagnostics without a severity outranking warnings, information, and hints [@editor]. Gutter signs use the configured icon style: Nerd Font, Unicode, ASCII, or none [@editor] [@defaults]. Turning off `show_diagnostics` or `[diagnostics].gutter_signs` clears the diagnostic gutter namespace instead of leaving stale signs behind [@editor].

The statusline diagnostics segment is absent when both error and warning counts are zero [@rendering]. When visible, it hides empty severities, uses the configured statusline icon style, and applies theme-derived error and warning colors with contrast adjustment against the active statusline slot [@rendering]. Because hidden sections are skipped, adding or removing the diagnostics segment changes the visible slot styling of neighboring sections [@rendering].

## Pickers

The diagnostics picker builds its items from the current `Editor::diagnostics` map and has two filters: all diagnostics and errors only [@picker]. Entries are sorted by severity, display path, start line, start character, and message, then selection opens the location in the current window using UTF-16 column encoding so LSP offsets land on the intended grapheme [@picker].

Picker search is not limited to the visible message. Each item's search text includes the message, display path, source/code origin, and severity, so queries such as a linter name or diagnostic code can match even when those values are rendered in the annotation [@picker]. Preview highlighting converts UTF-16 diagnostic offsets to UTF-8 byte spans for the rendered source line [@picker].

Unsaved previews are deliberately bounded. Before the picker model is built, the editor snapshots only buffers that have diagnostics included by the active filter, and snapshots are `Rope` values rather than full string copies [@picker]. A preview line larger than `MAX_UNFOCUSED_PREVIEW_BYTES` is omitted, preserving the picker preview budget even when an open unsaved buffer is very large [@picker].

## Line Popup

`ShowLineDiagnostics` checks only diagnostics that cover the active buffer line; when none match, it is a safe no-op [@editor]. When diagnostics match, `DiagnosticInfo` renders a rounded, cursor-anchored dialog using the same anchored popup geometry helper that `HoverInfo` uses [@popup] [@geometry] [@hover]. The popup numbers diagnostics in publication order, colors only the message body by severity, appends diagnostic codes, preserves multiline indentation, wraps by display width, and includes related information as `file:line:column` rows [@popup].

The popup is a normal `Component`, so it handles close, scroll, resize, and theme updates through the shared modal UI contract [@popup]. `Esc` and `q` close it; `j`, `k`, arrow keys, Page Up/Page Down, Home/End, `g`, `G`, and mouse wheel events update bounded scroll state [@popup]. Resize and theme changes reflow the diagnostic lines and keep the popup within the current viewport [@popup].

## Related Pages

Diagnostic transport and publication routing are covered by [Transport](transport) and [LSP Document Sync](../editor/lsp-document-sync). The shared modal component contract is in [UI Components](../../reference/editor/ui-components), and the exact user defaults are in [Default Config](../../reference/configuration/default-config).
