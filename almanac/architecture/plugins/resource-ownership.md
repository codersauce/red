---
title: "Plugin Resource Ownership"
summary: "Plugin resource ownership is Red's pattern for letting Husk plugins request UI resources while editor managers own state, rendering, validation, and teardown."
topics: [architecture, plugins, host-api]
sources:
  - id: panel
    type: file
    path: src/plugin/panel.rs
  - id: workspace
    type: file
    path: src/plugin/workspace.rs
  - id: window-bar
    type: file
    path: src/plugin/window_bar.rs
  - id: overlay
    type: file
    path: src/plugin/overlay.rs
  - id: decoration
    type: file
    path: src/plugin/decoration.rs
  - id: gutter
    type: file
    path: src/plugin/gutter.rs
  - id: runtime
    type: file
    path: src/plugin/runtime.rs
  - id: schema
    type: file
    path: src/plugin/host_api.json
---

Plugin resource ownership is the editor-side contract behind Red's plugin UI surfaces. Husk plugins call the [Red host API](red-host-api) with stable IDs, models, namespaces, callbacks, or segments, and the runtime turns those calls into [plugin host requests](../editor/plugin-host-requests) [@runtime] [@schema]. The editor-side managers then own focus, layout, hit testing, rendering, replacement semantics, and stale-resource cleanup. This shape lets plugins describe UI intent without letting plugin VMs mutate editor buffers, windows, render buffers, or long-lived UI state directly.

## Stable IDs Select Resources

Panels, text panels, workspaces, overlays, and window bars are all selected by plugin-provided string IDs. The runtime requires an ID for `CreateOverlay`, `CreateWindowBar`, `OpenWorkspace`, `CreatePanel`, and `CreateTextPanel`, deserializes the configuration payload, and sends a typed request for the editor to apply [@runtime]. The public schema records the same calls and their signatures, so plugin source can be checked before activation instead of relying only on runtime dispatch errors [@schema].

The managers treat an existing ID as replacement or update, not as a new anonymous resource. `PanelManager` owns panels by stable ID and keeps panel-side UI state such as focus, scrolling, composer drafts, follow-tail state, and header hit regions inside the manager [@panel]. `OverlayManager::create_overlay` inserts a new `PluginOverlay` under the supplied ID and keeps a z-order list keyed by those IDs [@overlay]. `WindowBarManager::create` updates the configuration and sequence for an existing bar ID or inserts a new bar when the ID is new [@window-bar].

## Editor Managers Own Runtime State

Plugin panel content is intentionally not just a flat text blob. Row panels contain structured rows, while text panels retain source `TextPanelBlock` values and derive rendered rows for the current width [@panel]. The manager owns composer validation, local history, Unicode-safe cursor movement, focused state, and tail-follow behavior, so streamed plugin updates do not destroy the authoritative source blocks [@panel].

Workspaces follow the same pattern at full-screen scale. A plugin supplies a `WorkspaceModel` with header segments, rows, detail lines, an optional focusable detail document, and footer segments [@workspace]. `WorkspaceManager` owns focus and selected row state, and selection restoration is based on row IDs so reordering rows does not silently move focus to unrelated content [@workspace]. That makes workspace updates model replacement operations instead of plugin-owned screen mutations.

## Namespaces Replace Decorations And Signs

Inline decorations and gutter signs use namespaces rather than individual resource handles. `DecorationManager::set` replaces one namespace atomically, rebuilds a buffer-line index, and sorts accepted decorations by priority for rendering [@decoration]. This prevents stale decorations from surviving a plugin refresh just because an old line was not explicitly cleared [@decoration].

Gutter signs use the same namespace replacement model with stricter display validation. A `GutterSign` must contain printable text occupying one or two display cells, and `GutterSignManager` resolves collisions by highest priority, then namespace order for equal priorities [@gutter]. Because a refresh replaces a complete namespace, plugins should compute the whole visible sign set for their owner rather than incrementally patching global gutter state.

## Bars And Overlays Reserve Screen Space Differently

Window bars are semantic chrome attached to editor windows. `WindowBarManager` selects at most one bar per window; higher priority wins, and the most recently created bar breaks ties [@window-bar]. Segment clipping is display-width aware, action hit regions are retained only for visible text, and styles can resolve through theme semantics before concrete overrides are applied [@window-bar]. A selected bar also reserves one top row for that window [@window-bar].

Overlays are floating resources positioned against terminal bounds. `PluginOverlay` stores full content, dimensions, alignment, dirty state, and the last computed position, while `OverlayManager` updates each overlay position and renders visible content in z-order [@overlay]. The current implementation positions each overlay independently and avoids the status line when rendering [@overlay]. That makes overlays useful for progress or transient status without changing the layout reservations used by panels and window bars.

## Callback Resources Stay Owner-Scoped

Dialog resources are related but stricter because they carry callback handles. The runtime allocates picker and composer handles for callback-scoped pickers, confirmations, composers, and inputs, stores the owning plugin with the handlers, and sends editor requests containing both owner and handle [@runtime]. Later picker updates check owner before changing or closing a callback-scoped picker [@runtime]. The lifecycle of those dialog callbacks is explained in [Callback-scoped dialogs](../../concepts/plugins/callback-scoped-dialogs).

## Consequences For Plugin Authors

A plugin should treat resource IDs and namespaces as capabilities it owns and can replace, not as direct pointers into editor memory. Reuse the same ID when the resource should keep editor-owned state, use a new ID when it should start fresh, and clear or close resources during teardown when they should disappear. The exact UI structs are documented in [Editor UI components](../../reference/editor/ui-components), while this page explains the ownership rule that makes those structs safe to expose through the plugin host boundary.
