---
title: "Callback-Scoped Dialogs"
summary: "Callback-scoped dialogs bind picker and composer results to host-owned handles instead of global event names."
topics: [concepts, plugins, host-api, ui]
sources:
  - id: runtime
    type: file
    path: src/plugin/runtime.rs
  - id: picker-ui
    type: file
    path: src/ui/picker.rs
  - id: composer-ui
    type: file
    path: src/ui/agent_composer.rs
  - id: confirmation-ui
    type: file
    path: src/ui/confirmation.rs
  - id: api-doc
    type: file
    path: docs/PLUGIN_API.md
  - id: git-plugin
    type: file
    path: plugins/git.hk
---

Callback-scoped dialogs are Red's newer picker and composer contract for Husk plugins. Instead of asking plugins to invent numeric IDs and subscribe to synthetic global events, the host allocates an opaque picker or composer handle, stores the callback functions under the plugin that opened the dialog, and routes terminal results back only to that owner [@runtime]. The public API names are `OpenPicker`, `OpenComposer`, `OpenInput`, and `OpenConfirm`; legacy `OpenDynamicPicker`, `OpenAgentComposer`, and `composer:*:<id>` events remain for compatibility, but new plugin code is expected to use handler records [@api-doc].

## Handles Replace Global Event Names

`OpenPicker` accepts `PickerHandlers` and returns a host-generated integer handle that can be passed to update calls such as `UpdatePickerItems`, `UpdatePickerQuery`, `UpdatePickerStatus`, and `ClosePicker` [@api-doc]. Runtime dispatch allocates that handle, records the owning plugin and handlers, and sends an `OpenCallbackPicker` request to the editor [@runtime]. `OpenComposer` and `OpenInput` follow the same pattern with `ComposerHandlers` and an `OpenCallbackComposer` or `OpenCallbackInput` editor request [@runtime].

The handle is opaque. The host checks picker ownership before allowing mutation calls such as `UpdatePickerItems`, `UpdatePickerQuery`, `UpdatePickerStatus`, and `ClosePicker`, so one plugin cannot update another plugin's callback-scoped picker by guessing an integer [@runtime]. The API document states the same rule from the plugin side: plugins must not assign or interpret the returned handle [@api-doc].

## Terminal Cleanup

Picker handlers may include repeated callbacks such as `changed`, `query`, and `action`, plus terminal callbacks such as `selected` and `cancelled` [@api-doc]. Runtime delivery keeps repeated picker callbacks registered, but removes the picker registration before delivering a terminal callback [@runtime]. Composer delivery is always one-shot: `notify_composer` removes the composer registration before calling `submitted` or `cancelled` [@runtime].

This cleanup rule is deliberate. Runtime tests assert that a picker terminal handler is consumed even when the callback fails, that stale picker handles from an old plugin generation do not resolve, and that composers cannot be submitted or cancelled twice [@runtime]. Unloading a plugin removes its picker and composer registrations with the rest of that plugin's commands, listeners, pending requests, and state [@runtime].

## Typed Payloads At The Boundary

Callback-scoped dialogs are also a typed migration point in the Red host API. The runtime converts selected and changed picker events into `PickerItem`, cancelled picker events into `PickerCancelled`, picker actions into `PickerActionEvent`, submitted composer events into `String`, and cancelled composer events into `ComposerCancelled` [@runtime]. The API document calls this out as the first migrated slice away from broad `Json` event payloads while still leaving plugin-owned `PickerItem.data` dynamic [@api-doc].

The UI components are responsible for producing those results, not for running plugin code. The picker module describes a structured fuzzy picker whose selection actions are returned to the editor or plugin owner and not applied directly by the picker itself [@picker-ui]. The agent composer has separate legacy and callback targets; the callback target emits `NotifyComposer(handle, Submitted(...))` or `NotifyComposer(handle, Cancelled)` actions, while the legacy target still emits `composer:submitted:<id>` or `composer:cancelled:<id>` plugin events [@composer-ui].

## Rich Confirmations

`OpenConfirm` is callback-scoped like the picker, but it uses a smaller Accept/Cancel surface whose default selection is Cancel [@confirmation-ui]. The current host API accepts an optional options object with `accept_label`, `cancel_label`, and structured `rows`, so plugins can render bounded, theme-aware details without owning dialog layout or input behavior [@api-doc] [@confirmation-ui]. Accept returns a selected `PickerItem` whose id is `accept`; Escape, `Ctrl-c`, the default Enter path, or explicit Cancel all use the cancellation callback [@confirmation-ui].

The Git plugin uses this richer path for pushes. It first spawns bounded preview commands to count and list outgoing commits, then opens `OpenConfirm` with branch-to-target rows, warning text when the upstream is also ahead, and up to eight commit rows; accepting the dialog starts the actual `git push` [@git-plugin]. This keeps the risky operation behind a host-owned safe default while still letting plugin code build the operation-specific preview [@git-plugin] [@api-doc].

## Relationship To The Host API

Callback-scoped dialogs sit inside the broader [Red host API](../../architecture/plugins/red-host-api). The host API schema records `OpenPicker` and `OpenComposer` as host API `0.3.0` calls, `OpenInput` and the original `OpenConfirm` as `0.4.0` calls, and `OpenConfirm.options` as a `0.9.0` addition [@api-doc]. The lifecycle registry adds failure isolation around callback delivery: if a picker, composer, or request callback fails, the registry logs the failure, quarantines the owning plugin, and treats the consumed terminal callback or request as resolved so it is not replayed [@runtime].

Use [UI Components](../../reference/editor/ui-components) for the shared modal component methods, cursor placement, and sensitive-input behavior that render these dialogs. Use [Plugin Resource Ownership](../../architecture/plugins/resource-ownership) for the lifecycle of the editor-owned handles and resources that callback-scoped dialogs join.
