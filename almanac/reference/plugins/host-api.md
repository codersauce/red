---
title: "Plugin Host API"
summary: "Lookup reference for the Red plugin host API source files, schema shape, versioning, and compatibility rules."
topics: [plugins, host-api, reference]
sources:
  - id: schema
    type: file
    path: src/plugin/host_api.json
  - id: api
    type: file
    path: src/plugin/api.rs
  - id: registry
    type: file
    path: src/plugin/registry.rs
  - id: api-doc
    type: file
    path: docs/PLUGIN_API.md
  - id: changes
    type: file
    path: docs/plugin_api_changes.json
---

The Plugin Host API reference identifies the files that define Red's Husk plugin contract and the rules for changing it. The canonical schema is `src/plugin/host_api.json`; it declares version `0.17.0` and lists host calls by `name`, `kind`, `signature`, and `introduced` [@schema]. The implementation embeds that schema in `src/plugin/api.rs`, validates literal plugin calls against it, and tests that runtime dispatch remains covered by the schema [@api]. This page does not copy the full schema; open the schema when exact call names or signatures are needed.

## Source Of Truth

| Material | Role |
| --- | --- |
| `src/plugin/host_api.json` | Canonical machine-readable list of host `execute` and `request` calls, signatures, and introduction versions [@schema]. |
| `src/plugin/api.rs` | Embedded schema loader, static validator, diagnostic families, and schema coverage tests [@api]. |
| `src/plugin/registry.rs` | Runtime compatibility gate; `RED_HOST_API_VERSION` is `0.17.0`, and `0.4.0`, `0.6.0`, `0.7.0`, `0.8.0`, `0.9.0`, `0.10.0`, `0.11.0`, `0.12.0`, `0.14.0`, and `0.16.0` remain accepted compatibility targets for existing packages [@registry]. |
| `docs/PLUGIN_API.md` | Human compatibility guide, migration notes, and behavioral descriptions for plugin authors [@api-doc]. |
| `docs/plugin_api_changes.json` | Versioned change manifest that records introduced symbols and migration note anchors through `0.17.0` [@changes]. |

Use code as the authority for runtime behavior and the schema as the authority for the public host call inventory. The prose guide is useful for compatibility intent and migration guidance, but when its stated host version or target range conflicts with `src/plugin/host_api.json` or `src/plugin/registry.rs`, use the schema and registry until the guide is refreshed [@schema] [@registry] [@api-doc].

## Schema Shape

The schema top level has `version` and `calls` fields [@schema]. Each call entry has:

| Field | Meaning |
| --- | --- |
| `name` | Action or request name passed as the first literal argument to `red::execute` or `red::request` [@schema]. |
| `kind` | Either `execute` or `request`; tests reject other kinds [@api]. |
| `signature` | Human-readable parameter contract used by the validator for arity and obvious literal-type checks [@api]. |
| `introduced` | Host API version where the call or symbol was introduced [@schema]. |

The validator treats literal `red::execute("...")` and `red::request("...")` calls as statically checkable host call sites [@api]. It reports `HUSK-A0001` for unknown calls, `HUSK-A0002` for required/optional arity mismatches, and `HUSK-A0003` for obvious literal type mismatches [@api].

## Current Version And Notable Introductions

The current host API version is `0.17.0`; it adds `MonotonicTime` for process-local elapsed-time measurements [@schema] [@registry]. The schema marks `SetPanelStatus` and `CancelGitStatus` as `0.16.0` calls [@schema]. It marks `PrintWarning`, `OpenPanelSearch`, `UpdatePanelSearch`, `KeepPanelSearch`, `ClosePanelSearch`, `InvalidateWorkspacePaths`, `SetTextPanelComposerHistory`, and stable-identity `OpenBufferById` as `0.15.0` calls; the change manifest also records `GetEditorInfo.buffers.id` at that version [@schema] [@changes]. It marks `AgentReadDefaultModel`, `AgentListModels`, `AgentSetModel`, `SetTextPanelHeaderDetail`, and `UpdatePickerSelection` as `0.14.0` calls [@schema]. The change manifest also records `PickerOptions.item_layout`, `agent:model_changed`, and `agent:model_rerouted` as `0.14.0` introductions, `GetWindows.document_id`, `GetWindows.breadcrumb_components`, `DocumentSymbols.document_id`, `file:saved.document_id`, and `red::document_symbol_chain` as `0.13.0` introductions, language-pack indentation symbols as `0.12.0`, command argument metadata as `0.11.0`, and language-pack formatter settings as `0.10.0` [@changes].

The schema marks `AgentResumeSession`, `AgentForgetSession`, and `UpdateOverlayBusy` as `0.9.0` calls, and the `OpenConfirm` signature accepts an optional `options?: Json` argument [@schema]. The change manifest also records the agent conversation-restore events `agent:conversation_restore_pending`, `agent:session_restored`, and `agent:session_restore_failed`, plus `OpenConfirm.options`, as `0.9.0` introductions [@changes].

Earlier minor additions remain part of the supported surface. The schema marks `ShowLineDiagnostics` as introduced in `0.8.0`; the current `OpenScratchBuffer` signature includes optional `commands`, and the change manifest records `OpenScratchBuffer.commands` as the documented migration-note entry for that version [@schema] [@changes]. The change manifest records command metadata scope as introduced in `0.7.0`; `CompanionCall`, `DocumentSnapshot`, `DocumentApply`, `DocumentUndo`, and `UpdatePickerBusy` as introduced in `0.6.0`; `FileOperation`, `OpenInput`, and `OpenConfirm` as introduced in `0.4.0`; callback-scoped picker and composer symbols as introduced in `0.3.0`; legacy agent composer and text panel surfaces as introduced in `0.2.0`; and the original `red::execute`, `red::request`, `red::on`, and `red::state` surface as introduced in `0.1.0` [@changes].

The schema's call list is the exact lookup source for current signatures. Examples include `OpenPicker(title: String, items: [PickerItem], options: PickerOptions, handlers: PickerHandlers)`, `OpenComposer(title: String, query: String, history: [String], handlers: ComposerHandlers)`, `OpenInput(title: String, initial: String, handlers: ComposerHandlers)`, `OpenConfirm(title: String, message: String, handlers: PickerHandlers, options?: Json)`, `UpdateOverlayBusy(id: String, busy: bool)`, and `AgentSetModel(callback: fn(Json), session_id: String, selection: Json)` [@schema].

## Compatibility Rules

Plugin packages may declare a semver range in `red_api_version`; Red checks that range before activation and quarantines malformed or incompatible packages without stopping unrelated plugins [@api-doc]. The registry accepts `0.4.0`, `0.6.0`, `0.7.0`, `0.8.0`, `0.9.0`, `0.10.0`, `0.11.0`, `0.12.0`, `0.14.0`, `0.16.0`, and the current `0.17.0` host API version, so existing packages can remain on those supported minors while new packages should target `^0.17.0` unless they intentionally avoid newer host calls [@registry] [@api-doc]. Because pre-1.0 caret ranges do not cross minor versions, compatibility checks test the declared range against every supported host API version instead of only the current version [@registry]. The documented pre-1.0 policy is:

| Release kind | Compatibility rule |
| --- | --- |
| Patch | Fix behavior without intentionally changing signatures [@api-doc]. |
| Minor | Add calls and fields, and possibly deprecate calls [@api-doc]. |
| Removal or incompatible change | Requires a host-API minor bump, a change manifest entry, and a migration note [@api-doc]. |

When changing host calls, update the schema, the runtime dispatch, and the change manifest together. The validator tests are designed to catch schema/runtime drift and missing introduction metadata [@api].

## Related Pages

Read [Red host API](../../architecture/plugins/red-host-api) for the architecture of validation and dispatch. Read [Plugin lifecycle and reload](../../architecture/plugins/lifecycle-and-reload) for how incompatible or failing plugins become quarantined. Read [Write a Husk plugin](../../guides/plugins/write-a-husk-plugin) for author-facing workflow once that guide is the relevant entrypoint.
