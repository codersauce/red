---
title: Coverage Map
summary: Frozen page inventory for this first wiki build.
topics: [build, wiki, reference]
sources: []
---

# Coverage Map

## Page Inventory

### Top Level

- path: `almanac/getting-started.md`
  - slug: `getting-started`
  - purpose: Front door to the finished wiki, routing readers to the main system areas and common work paths.
  - planned links: `concepts/red-editor`, `architecture/startup/runtime-lifecycle`, `architecture/editor/event-loop`, `architecture/plugins/lifecycle-and-reload`, `architecture/husk/public-embedding-api`, `architecture/agent/codex-app-server-workflow`, `guides/development/build-test-and-validate`, `reference/cli/red-command`
  - key evidence files: `README.md`, `Cargo.toml`, `docs/GETTING_STARTED.md`

### Concepts

#### `concepts/`

- path: `almanac/concepts/red-editor.md`
  - slug: `concepts/red-editor`
  - purpose: Define Red as a modal terminal editor with bundled runtime assets, language services, and reviewable agent edits.
  - planned links: `../architecture/startup/runtime-lifecycle`, `../architecture/editor/event-loop`, `../concepts/reviewable-agent-edits`, `../concepts/husk-language`, `../reference/cli/red-command`
  - key evidence files: `README.md`, `src/main.rs`, `src/editor.rs`
- path: `almanac/concepts/reviewable-agent-edits.md`
  - slug: `concepts/reviewable-agent-edits`
  - purpose: Explain the proposal-first model that keeps Codex writes out of visible buffers and disk until review.
  - planned links: `../architecture/agent/codex-app-server-workflow`, `../architecture/agent/proposal-workspace`, `../guides/agent/review-agent-proposals`, `../decisions/agent/direct-codex-app-server`
  - key evidence files: `docs/AGENT_WORKFLOW.md`, `src/agent_workspace.rs`, `src/codex/mod.rs`
- path: `almanac/concepts/husk-language.md`
  - slug: `concepts/husk-language`
  - purpose: Define Husk as Red's embedded scripting language and standalone package/runtime workspace.
  - planned links: `../architecture/husk/public-embedding-api`, `../architecture/plugins/lifecycle-and-reload`, `../reference/cli/husk-command`, `../concepts/plugins/bundled-husk-plugins`
  - key evidence files: `docs/HUSK_LANGUAGE_GUIDE.md`, `Cargo.toml`, `crates/husk/src/lib.rs`, `src/plugin/runtime.rs`

#### `concepts/editor/`

- path: `almanac/concepts/editor/coordinate-systems.md`
  - slug: `concepts/editor/coordinate-systems`
  - purpose: Explain Red's byte, scalar, grapheme, terminal-column, and UTF-16 coordinate boundaries.
  - planned links: `../../architecture/editor/text-mutation-boundary`, `../../architecture/editor/rendering-pipeline`, `../../architecture/lsp/workspace-edits`, `display-layout`
  - key evidence files: `src/editor.rs`, `src/unicode_utils.rs`, `src/lsp/edit.rs`, `tests/unicode.rs`
- path: `almanac/concepts/editor/display-layout.md`
  - slug: `concepts/editor/display-layout`
  - purpose: Explain logical-line to screen-row layout, wrapping, horizontal scrolling, and screen-line motion.
  - planned links: `coordinate-systems`, `../../architecture/editor/rendering-pipeline`, `../../architecture/editor/buffers-and-windows`
  - key evidence files: `src/editor/display_layout.rs`, `src/editor.rs`, `tests/movement.rs`
- path: `almanac/concepts/editor/undo-tree.md`
  - slug: `concepts/editor/undo-tree`
  - purpose: Explain buffer-local branching undo history, saved revisions, dirty state, and applied edit replay.
  - planned links: `coordinate-systems`, `../../architecture/editor/text-mutation-boundary`, `../../reference/editor/registers-clipboard-and-macros`
  - key evidence files: `src/undo.rs`, `src/buffer.rs`, `src/editor.rs`, `tests/editing.rs`

#### `concepts/lsp/`

- path: `almanac/concepts/lsp/capabilities.md`
  - slug: `concepts/lsp/capabilities`
  - purpose: Explain the exact LSP capability model Red advertises and the deliberate omissions.
  - planned links: `../../architecture/lsp/transport`, `../../architecture/lsp/workspace-edits`, `../../reference/lsp/configuration`
  - key evidence files: `src/lsp/capabilities.rs`, `src/lsp/fixtures/vscode-capabilities.json`

#### `concepts/plugins/`

- path: `almanac/concepts/plugins/bundled-husk-plugins.md`
  - slug: `concepts/plugins/bundled-husk-plugins`
  - purpose: Explain the bundled plugin corpus and the split between `.hk` shells and pure Husk core packages.
  - planned links: `../../concepts/husk-language`, `../../architecture/plugins/lifecycle-and-reload`, `../../architecture/husk/packages-and-locks`, `../../reference/plugins/host-api`
  - key evidence files: `plugins/`, `default_config.toml`, `src/assets.rs`, `src/plugin/runtime.rs`
- path: `almanac/concepts/plugins/callback-scoped-dialogs.md`
  - slug: `concepts/plugins/callback-scoped-dialogs`
  - purpose: Explain picker and composer handles, callback ownership, cleanup, and legacy event compatibility.
  - planned links: `../../architecture/plugins/red-host-api`, `../../architecture/plugins/resource-ownership`, `../../reference/editor/ui-components`
  - key evidence files: `src/plugin/runtime.rs`, `src/ui/picker.rs`, `src/ui/agent_composer.rs`, `docs/PLUGIN_API.md`

#### `concepts/sessions/`

- path: `almanac/concepts/sessions/detach-vs-recovery.md`
  - slug: `concepts/sessions/detach-vs-recovery`
  - purpose: Clarify the difference between live detachable sessions and persisted crash recovery.
  - planned links: `../../architecture/sessions/detachable-editor-core`, `../../architecture/sessions/crash-recovery-snapshots`, `../../guides/sessions/detach-reattach`, `../../guides/sessions/resume-after-crash`
  - key evidence files: `docs/DETACH.md`, `docs/SESSION_RECOVERY.md`, `src/headless/mod.rs`, `src/session.rs`

### Architecture

#### `architecture/startup/`

- path: `almanac/architecture/startup/runtime-lifecycle.md`
  - slug: `architecture/startup/runtime-lifecycle`
  - purpose: Explain how the binary selects `red husk`, utility, detach, resume, onboarding, and interactive editor lifecycles.
  - planned links: `../../reference/cli/red-command`, `../configuration/layered-config-recovery`, `../runtime/runtime-assets`, `../sessions/detachable-editor-core`
  - key evidence files: `src/main.rs`, `src/cli.rs`, `src/onboarding.rs`, `src/session.rs`, `src/headless/mod.rs`

#### `architecture/configuration/`

- path: `almanac/architecture/configuration/layered-config-recovery.md`
  - slug: `architecture/configuration/layered-config-recovery`
  - purpose: Explain embedded defaults, user TOML recovery, strict CLI overrides, diagnostics, and runtime validation.
  - planned links: `../../reference/configuration/default-config`, `../../decisions/configuration/fail-closed-recovery`, `../runtime/runtime-assets`, `../themes/theme-import`
  - key evidence files: `src/config.rs`, `default_config.toml`, `src/main.rs`

#### `architecture/runtime/`

- path: `almanac/architecture/runtime/runtime-assets.md`
  - slug: `architecture/runtime/runtime-assets`
  - purpose: Explain user, `RED_RUNTIME`, and embedded runtime asset precedence, listing, bundled URIs, and ejection.
  - planned links: `../../decisions/runtime/embedded-assets-with-user-ejection`, `../themes/theme-import`, `../plugins/lifecycle-and-reload`, `../../reference/runtime/self-check`
  - key evidence files: `src/assets.rs`, `plugins/`, `themes/`, `src/plugin/registry.rs`

#### `architecture/themes/`

- path: `almanac/architecture/themes/theme-import.md`
  - slug: `architecture/themes/theme-import`
  - purpose: Explain VS Code theme parsing, Red's theme model, workbench colors, and contrast repair.
  - planned links: `../runtime/runtime-assets`, `../configuration/layered-config-recovery`, `../../reference/configuration/default-config`
  - key evidence files: `src/theme/mod.rs`, `src/theme/vscode.rs`, `themes/`, `themes/THIRD_PARTY.md`

#### `architecture/commands/`

- path: `almanac/architecture/commands/command-discovery.md`
  - slug: `architecture/commands/command-discovery`
  - purpose: Explain colon command parsing, effective keymaps, command palette entries, and plugin command metadata.
  - planned links: `../../reference/cli/red-command`, `../plugins/red-host-api`, `../../reference/configuration/default-config`
  - key evidence files: `src/command.rs`, `src/command_palette.rs`, `src/editor.rs`, `default_config.toml`

#### `architecture/preferences/`

- path: `almanac/architecture/preferences/preferences-store.md`
  - slug: `architecture/preferences/preferences-store`
  - purpose: Explain persisted command history, picker history, plugin storage, legacy imports, and filesystem safety.
  - planned links: `../plugins/resource-ownership`, `../startup/runtime-lifecycle`, `../sessions/crash-recovery-snapshots`
  - key evidence files: `src/preferences.rs`

#### `architecture/editor/`

- path: `almanac/architecture/editor/event-loop.md`
  - slug: `architecture/editor/event-loop`
  - purpose: Explain the editor-owned input, background service, render, plugin, LSP, agent, and shutdown loop.
  - planned links: `text-mutation-boundary`, `plugin-host-requests`, `lsp-document-sync`, `../agent/codex-app-server-workflow`, `../sessions/detachable-editor-core`
  - key evidence files: `src/editor.rs`, `src/editor/agent_manager.rs`, `src/editor/lsp_coordinator.rs`, `src/editor/session_manager.rs`
- path: `almanac/architecture/editor/text-mutation-boundary.md`
  - slug: `architecture/editor/text-mutation-boundary`
  - purpose: Explain the canonical transaction path for text changes and the subsystems updated by it.
  - planned links: `event-loop`, `../../concepts/editor/coordinate-systems`, `../../concepts/editor/undo-tree`, `lsp-document-sync`, `../agent/proposal-workspace`
  - key evidence files: `src/editor.rs`, `src/buffer.rs`, `src/undo.rs`, `tests/editing.rs`
- path: `almanac/architecture/editor/buffers-and-windows.md`
  - slug: `architecture/editor/buffers-and-windows`
  - purpose: Explain stable buffer and window identities, split tree state, and synchronized cursor/view state.
  - planned links: `event-loop`, `rendering-pipeline`, `../../concepts/editor/display-layout`, `../../architecture/sessions/crash-recovery-snapshots`
  - key evidence files: `src/buffer.rs`, `src/window.rs`, `src/editor/buffer_manager.rs`, `src/editor.rs`
- path: `almanac/architecture/editor/rendering-pipeline.md`
  - slug: `architecture/editor/rendering-pipeline`
  - purpose: Explain render buffer construction, window drawing, overlays, dialogs, wide cells, diffing, and detached render deltas.
  - planned links: `buffers-and-windows`, `../../concepts/editor/display-layout`, `../../concepts/editor/coordinate-systems`, `../sessions/detachable-editor-core`
  - key evidence files: `src/editor/rendering.rs`, `src/editor/render_buffer.rs`, `src/editor/display_layout.rs`, `src/splash.rs`
- path: `almanac/architecture/editor/plugin-host-requests.md`
  - slug: `architecture/editor/plugin-host-requests`
  - purpose: Explain how plugin requests enter the editor loop and why the editor remains the only owner of state mutations.
  - planned links: `event-loop`, `../plugins/resource-ownership`, `../plugins/red-host-api`, `../../concepts/plugins/callback-scoped-dialogs`
  - key evidence files: `src/dispatcher.rs`, `src/editor.rs`, `src/plugin/runtime.rs`
- path: `almanac/architecture/editor/syntax-services.md`
  - slug: `architecture/editor/syntax-services`
  - purpose: Explain syntax selection, tree-sitter and Husk highlighting, markdown injections, viewport caching, and matchit.
  - planned links: `rendering-pipeline`, `buffers-and-windows`, `../../concepts/editor/coordinate-systems`
  - key evidence files: `src/highlighter.rs`, `src/matchit.rs`, `src/buffer.rs`, `src/editor.rs`
- path: `almanac/architecture/editor/lsp-document-sync.md`
  - slug: `architecture/editor/lsp-document-sync`
  - purpose: Explain lazy document open, change synchronization, diagnostics, identity changes, and editor-side LSP coordination.
  - planned links: `event-loop`, `text-mutation-boundary`, `../lsp/client-lifecycle-and-routing`, `../lsp/workspace-edits`
  - key evidence files: `src/editor/lsp_coordinator.rs`, `src/editor.rs`, `tests/lsp_lazy.rs`

#### `architecture/lsp/`

- path: `almanac/architecture/lsp/client-lifecycle-and-routing.md`
  - slug: `architecture/lsp/client-lifecycle-and-routing`
  - purpose: Explain document selector routing, workspace-root discovery, lazy process startup, failed-client handling, and round-robin polling.
  - planned links: `transport`, `../editor/lsp-document-sync`, `../../reference/lsp/configuration`, `../../concepts/lsp/capabilities`
  - key evidence files: `src/lsp/manager.rs`, `tests/lsp_lazy.rs`, `src/config.rs`
- path: `almanac/architecture/lsp/transport.md`
  - slug: `architecture/lsp/transport`
  - purpose: Explain LSP JSON-RPC framing, process IO, request correlation, pending queues, diagnostics debounce, and shutdown.
  - planned links: `client-lifecycle-and-routing`, `workspace-edits`, `../../concepts/lsp/capabilities`
  - key evidence files: `src/lsp/client.rs`, `src/lsp/mod.rs`
- path: `almanac/architecture/lsp/workspace-edits.md`
  - slug: `architecture/lsp/workspace-edits`
  - purpose: Explain fail-closed workspace edit parsing, UTF-16 conversion, version checks, protected paths, resource operations, rollback, and editor-owned application.
  - planned links: `transport`, `../editor/lsp-document-sync`, `../editor/text-mutation-boundary`, `../../guides/lsp/debugging-lsp-failures`
  - key evidence files: `src/lsp/edit.rs`, `src/lsp/workspace_edit.rs`, `src/editor.rs`, `tests/lsp_lazy.rs`
- path: `almanac/architecture/lsp/completion.md`
  - slug: `architecture/lsp/completion`
  - purpose: Explain completion request context, stale-response guards, UI filtering, snippet handling, atomic edit application, and follow-up commands.
  - planned links: `client-lifecycle-and-routing`, `workspace-edits`, `../editor/lsp-document-sync`, `../../reference/editor/ui-components`
  - key evidence files: `src/ui/completion.rs`, `src/editor.rs`, `tests/completion.rs`, `src/fixtures/lsp-completion-response.json`

#### `architecture/plugins/`

- path: `almanac/architecture/plugins/lifecycle-and-reload.md`
  - slug: `architecture/plugins/lifecycle-and-reload`
  - purpose: Explain plugin discovery, dependency ordering, activation states, quarantine, callback failure isolation, and transactional hot reload.
  - planned links: `red-host-api`, `resource-ownership`, `process-and-filesystem-boundaries`, `../../concepts/plugins/bundled-husk-plugins`, `../runtime/runtime-assets`
  - key evidence files: `src/plugin/registry.rs`, `src/plugin/runtime.rs`, `docs/PLUGIN_SYSTEM.md`, `docs/PLUGIN_API.md`
- path: `almanac/architecture/plugins/red-host-api.md`
  - slug: `architecture/plugins/red-host-api`
  - purpose: Explain the versioned Red host API, `host_api.json` as the canonical contract, static call validation, and dispatch.
  - planned links: `lifecycle-and-reload`, `resource-ownership`, `../../reference/plugins/host-api`, `../../guides/plugins/write-a-husk-plugin`
  - key evidence files: `src/plugin/host_api.json`, `src/plugin/api.rs`, `src/plugin/runtime.rs`, `docs/PLUGIN_API.md`
- path: `almanac/architecture/plugins/resource-ownership.md`
  - slug: `architecture/plugins/resource-ownership`
  - purpose: Explain plugin-owned requests for panels, workspaces, window bars, overlays, decorations, gutter signs, and dialogs under editor ownership.
  - planned links: `red-host-api`, `../../architecture/editor/plugin-host-requests`, `../../concepts/plugins/callback-scoped-dialogs`, `../../reference/editor/ui-components`
  - key evidence files: `src/plugin/panel.rs`, `src/plugin/workspace.rs`, `src/plugin/window_bar.rs`, `src/plugin/overlay.rs`, `src/plugin/decoration.rs`, `src/plugin/gutter.rs`
- path: `almanac/architecture/plugins/process-and-filesystem-boundaries.md`
  - slug: `architecture/plugins/process-and-filesystem-boundaries`
  - purpose: Explain plugin process permissions, bounded child IO, workspace-confined file operations, and fail-closed path checks.
  - planned links: `lifecycle-and-reload`, `red-host-api`, `../../reference/configuration/default-config`, `../../guides/plugins/write-a-husk-plugin`
  - key evidence files: `src/plugin/process.rs`, `src/plugin/filesystem.rs`, `src/config.rs`, `docs/PLUGIN_API.md`

#### `architecture/husk/`

- path: `almanac/architecture/husk/public-embedding-api.md`
  - slug: `architecture/husk/public-embedding-api`
  - purpose: Explain the public `husk` facade, engine, compiled modules, instances, native modules, REPL, and limits.
  - planned links: `packages-and-locks`, `extensions`, `../../concepts/husk-language`, `../../decisions/husk/engine-instance-ownership`
  - key evidence files: `crates/husk/src/lib.rs`, `crates/husk-runtime/src/embedding.rs`, `crates/husk/tests/embedding_api.rs`
- path: `almanac/architecture/husk/packages-and-locks.md`
  - slug: `architecture/husk/packages-and-locks`
  - purpose: Explain `Husk.toml`, module resolution, lock validation, embedded source packages, and local-only package rules.
  - planned links: `public-embedding-api`, `extensions`, `../../decisions/husk/scripts-and-modules`, `../../reference/cli/husk-command`
  - key evidence files: `crates/husk-package/src/lib.rs`, `crates/husk-package/tests/package.rs`, `plugins/git_core/Husk.toml`, `plugins/neotree_core/Husk.toml`
- path: `almanac/architecture/husk/extensions.md`
  - slug: `architecture/husk/extensions`
  - purpose: Explain static native modules, portable WebAssembly Components, `.huskext` bundle validation, capabilities, and crate adapter workflows.
  - planned links: `public-embedding-api`, `packages-and-locks`, `../../decisions/husk/extension-tiers`, `../../reference/cli/husk-command`
  - key evidence files: `docs/adr/0004-husk-extension-tiers.md`, `docs/adr/0009-wasm-component-extension-go.md`, `crates/husk-extension/src/lib.rs`, `crates/husk-wasm/src/lib.rs`, `crates/husk-cli/src/lib.rs`
- path: `almanac/architecture/husk/language-server.md`
  - slug: `architecture/husk/language-server`
  - purpose: Explain the first-party Husk LSP server, package and loose-file analysis, dependency stubs, bounds, and Red integration.
  - planned links: `public-embedding-api`, `packages-and-locks`, `../../architecture/lsp/client-lifecycle-and-routing`, `../../reference/lsp/configuration`
  - key evidence files: `docs/HUSK_LSP.md`, `crates/husk-lsp/src/`, `crates/husk-analysis/src/`, `src/config.rs`

#### `architecture/agent/`

- path: `almanac/architecture/agent/codex-app-server-workflow.md`
  - slug: `architecture/agent/codex-app-server-workflow`
  - purpose: Explain Red's direct Codex app-server process lifecycle, read-only safety boundary, thread flow, and event handling.
  - planned links: `dynamic-tools-and-editor-tools`, `proposal-workspace`, `../../concepts/reviewable-agent-edits`, `../../decisions/agent/direct-codex-app-server`, `../../reference/agent/agent-check`
  - key evidence files: `src/codex/mod.rs`, `src/editor/agent_manager.rs`, `src/agent_check.rs`, `docs/AGENT_WORKFLOW.md`
- path: `almanac/architecture/agent/dynamic-tools-and-editor-tools.md`
  - slug: `architecture/agent/dynamic-tools-and-editor-tools`
  - purpose: Explain Codex dynamic filesystem tools, editor tools, strict schemas, UTF-16 positions, and proposal staging.
  - planned links: `codex-app-server-workflow`, `proposal-workspace`, `../editor/text-mutation-boundary`, `../../concepts/editor/coordinate-systems`
  - key evidence files: `src/codex/mod.rs`, `src/agent_tools.rs`, `src/editor.rs`, `docs/AGENT_WORKFLOW.md`
- path: `almanac/architecture/agent/proposal-workspace.md`
  - slug: `architecture/agent/proposal-workspace`
  - purpose: Explain the session-scoped proposal filesystem, visible-file bases, conflict detection, staged acceptance, and recovery snapshot state.
  - planned links: `codex-app-server-workflow`, `dynamic-tools-and-editor-tools`, `../editor/text-mutation-boundary`, `../../guides/agent/review-agent-proposals`, `../../architecture/sessions/crash-recovery-snapshots`
  - key evidence files: `src/agent_workspace.rs`, `src/editor.rs`, `docs/AGENT_WORKFLOW.md`

#### `architecture/sessions/`

- path: `almanac/architecture/sessions/crash-recovery-snapshots.md`
  - slug: `architecture/sessions/crash-recovery-snapshots`
  - purpose: Explain schema-v2 session snapshots, owner namespaces, atomic rotation, selection rules, and disk divergence detection.
  - planned links: `../../concepts/sessions/detach-vs-recovery`, `detachable-editor-core`, `../../guides/sessions/resume-after-crash`, `../agent/proposal-workspace`
  - key evidence files: `src/session.rs`, `src/editor/session_manager.rs`, `src/editor.rs`, `docs/SESSION_RECOVERY.md`
- path: `almanac/architecture/sessions/detachable-editor-core.md`
  - slug: `architecture/sessions/detachable-editor-core`
  - purpose: Explain the Unix detach owner/client split, live editor ownership, background ticks, and reconnect behavior.
  - planned links: `crash-recovery-snapshots`, `../../concepts/sessions/detach-vs-recovery`, `../../reference/sessions/detach-ipc-protocol`, `../../guides/sessions/detach-reattach`, `../../decisions/sessions/detachable-core-boundary`
  - key evidence files: `src/headless/mod.rs`, `src/main.rs`, `src/editor.rs`, `docs/DETACH.md`, `tests/detach.rs`

### Guides

#### `guides/development/`

- path: `almanac/guides/development/build-test-and-validate.md`
  - slug: `guides/development/build-test-and-validate`
  - purpose: Guide maintainers through the local build, test, clippy, plugin, and self-check validation workflow.
  - planned links: `../../reference/validation/ci-and-validation`, `../../reference/runtime/self-check`, `../../architecture/startup/runtime-lifecycle`
  - key evidence files: `README.md`, `AGENTS.md`, `.github/workflows/ci.yml`, `.github/workflows/plugin-check.yml`

#### `guides/plugins/`

- path: `almanac/guides/plugins/write-a-husk-plugin.md`
  - slug: `guides/plugins/write-a-husk-plugin`
  - purpose: Guide maintainers through writing and validating a Red Husk plugin with metadata, commands, requests, resources, and permissions.
  - planned links: `../../architecture/plugins/lifecycle-and-reload`, `../../architecture/plugins/red-host-api`, `../../architecture/plugins/process-and-filesystem-boundaries`, `../../reference/plugins/host-api`
  - key evidence files: `examples/example-plugin/index.hk`, `examples/example-plugin/package.json`, `docs/PLUGIN_SYSTEM.md`, `docs/PLUGIN_API.md`

#### `guides/lsp/`

- path: `almanac/guides/lsp/debugging-lsp-failures.md`
  - slug: `guides/lsp/debugging-lsp-failures`
  - purpose: Guide maintainers through diagnosing LSP startup, transport, completion, diagnostics, and workspace edit failures.
  - planned links: `../../architecture/lsp/transport`, `../../architecture/lsp/workspace-edits`, `../../architecture/lsp/completion`, `../../reference/lsp/configuration`
  - key evidence files: `docs/DEBUGGING.md`, `src/lsp/client.rs`, `src/lsp/edit.rs`, `src/lsp/workspace_edit.rs`

#### `guides/agent/`

- path: `almanac/guides/agent/review-agent-proposals.md`
  - slug: `guides/agent/review-agent-proposals`
  - purpose: Guide maintainers through reviewing, accepting, rejecting, and recovering Codex proposal changes.
  - planned links: `../../architecture/agent/proposal-workspace`, `../../architecture/agent/codex-app-server-workflow`, `../../concepts/reviewable-agent-edits`
  - key evidence files: `src/editor.rs`, `src/agent_workspace.rs`, `docs/AGENT_WORKFLOW.md`, `docs/PLUGIN_API.md`

#### `guides/sessions/`

- path: `almanac/guides/sessions/resume-after-crash.md`
  - slug: `guides/sessions/resume-after-crash`
  - purpose: Guide maintainers through using `red --resume` and interpreting recovery warnings and divergence diffs.
  - planned links: `../../architecture/sessions/crash-recovery-snapshots`, `../../concepts/sessions/detach-vs-recovery`, `detach-reattach`
  - key evidence files: `docs/SESSION_RECOVERY.md`, `src/main.rs`, `src/session.rs`, `src/editor.rs`
- path: `almanac/guides/sessions/detach-reattach.md`
  - slug: `guides/sessions/detach-reattach`
  - purpose: Guide maintainers through starting, detaching, reattaching, and stopping detachable editor sessions.
  - planned links: `../../architecture/sessions/detachable-editor-core`, `../../reference/sessions/detach-ipc-protocol`, `../../concepts/sessions/detach-vs-recovery`, `resume-after-crash`
  - key evidence files: `docs/DETACH.md`, `src/cli.rs`, `src/main.rs`, `tests/detach.rs`

#### `guides/releases/`

- path: `almanac/guides/releases/release-red.md`
  - slug: `guides/releases/release-red`
  - purpose: Guide maintainers through preparing and publishing a Red release without confusing release prep, tags, smoke tests, and announcements.
  - planned links: `../../reference/validation/ci-and-validation`, `../installers/release-installers`, `../development/build-test-and-validate`
  - key evidence files: `docs/RELEASING.md`, `.github/workflows/prepare-release.yml`, `.github/workflows/release.yml`, `.github/workflows/announce-discord.yml`, `scripts/readme_release.py`, `scripts/discord_release.py`

#### `guides/performance/`

- path: `almanac/guides/performance/performance-checks.md`
  - slug: `guides/performance/performance-checks`
  - purpose: Guide maintainers through deterministic and workstation performance checks for editor and Husk work.
  - planned links: `../development/build-test-and-validate`, `../../architecture/editor/rendering-pipeline`, `../../architecture/husk/public-embedding-api`
  - key evidence files: `docs/performance.md`, `scripts/scroll_bench.py`, `scripts/detach_bench.py`, `scripts/interaction_bench.py`, `scripts/git_workspace_bench.py`

#### `guides/installers/`

- path: `almanac/guides/installers/release-installers.md`
  - slug: `guides/installers/release-installers`
  - purpose: Guide maintainers through installer verification for macOS, Linux, and Windows release artifacts.
  - planned links: `../releases/release-red`, `../../reference/runtime/self-check`, `../../architecture/runtime/runtime-assets`
  - key evidence files: `install/install.sh`, `install/install.ps1`, `tests/installers/install-sh.sh`, `tests/installers/install-ps1.ps1`, `.github/workflows/installers.yml`

### Decisions

#### `decisions/agent/`

- path: `almanac/decisions/agent/direct-codex-app-server.md`
  - slug: `decisions/agent/direct-codex-app-server`
  - purpose: Record the accepted decision to integrate directly with Codex app-server and supersede ACP.
  - planned links: `../../architecture/agent/codex-app-server-workflow`, `../../concepts/reviewable-agent-edits`
  - key evidence files: `docs/adr/0003-direct-codex-app-server.md`, `docs/adr/0001-acp-foundation.md`, `src/codex/mod.rs`

#### `decisions/sessions/`

- path: `almanac/decisions/sessions/detachable-core-boundary.md`
  - slug: `decisions/sessions/detachable-core-boundary`
  - purpose: Record the accepted owner/client boundary for detachable sessions.
  - planned links: `../../architecture/sessions/detachable-editor-core`, `../../concepts/sessions/detach-vs-recovery`, `../../reference/sessions/detach-ipc-protocol`
  - key evidence files: `docs/adr/0002-detachable-core.md`, `src/headless/mod.rs`, `src/main.rs`

#### `decisions/husk/`

- path: `almanac/decisions/husk/extension-tiers.md`
  - slug: `decisions/husk/extension-tiers`
  - purpose: Record the choice of static native modules, WebAssembly Components, and a deferred trusted native ABI for Husk extensions.
  - planned links: `../../architecture/husk/extensions`, `../../architecture/husk/public-embedding-api`
  - key evidence files: `docs/adr/0004-husk-extension-tiers.md`, `docs/adr/0009-wasm-component-extension-go.md`, `crates/husk-extension/src/lib.rs`, `crates/husk-wasm/src/lib.rs`
- path: `almanac/decisions/husk/engine-instance-ownership.md`
  - slug: `decisions/husk/engine-instance-ownership`
  - purpose: Record Husk's engine, compiled module, and instance ownership boundary.
  - planned links: `../../architecture/husk/public-embedding-api`, `../../architecture/plugins/lifecycle-and-reload`
  - key evidence files: `docs/adr/0005-husk-engine-ownership.md`, `crates/husk-runtime/src/embedding.rs`, `src/plugin/runtime.rs`
- path: `almanac/decisions/husk/semantic-profiles.md`
  - slug: `decisions/husk/semantic-profiles`
  - purpose: Record the native versus legacy JavaScript semantic profile split and Red's compatibility use of host declarations.
  - planned links: `../../concepts/husk-language`, `../../architecture/plugins/red-host-api`, `../../architecture/husk/language-server`
  - key evidence files: `docs/adr/0006-husk-semantic-profiles.md`, `src/plugin/runtime.rs`, `crates/husk/tests/compile_pipeline.rs`
- path: `almanac/decisions/husk/scripts-and-modules.md`
  - slug: `decisions/husk/scripts-and-modules`
  - purpose: Record the standalone `main` entrypoint and explicit module/package resolution rules.
  - planned links: `../../architecture/husk/packages-and-locks`, `../../reference/cli/husk-command`
  - key evidence files: `docs/adr/0007-husk-scripts-and-modules.md`, `crates/husk-cli/src/lib.rs`, `crates/husk-package/src/lib.rs`
- path: `almanac/decisions/husk/value-semantics.md`
  - slug: `decisions/husk/value-semantics`
  - purpose: Record the native Husk value and evaluation semantics target.
  - planned links: `../../architecture/husk/public-embedding-api`, `semantic-profiles`
  - key evidence files: `docs/adr/0008-husk-value-semantics.md`, `crates/husk-runtime/src/lib.rs`, `crates/husk/tests/native_language_features.rs`, `crates/husk/tests/native_stdlib.rs`

#### `decisions/configuration/`

- path: `almanac/decisions/configuration/fail-closed-recovery.md`
  - slug: `decisions/configuration/fail-closed-recovery`
  - purpose: Record the decision to keep editing possible while disabling high-risk surfaces on whole-file config failure.
  - planned links: `../../architecture/configuration/layered-config-recovery`, `../../reference/configuration/default-config`
  - key evidence files: `src/config.rs`, `src/main.rs`, `default_config.toml`

#### `decisions/runtime/`

- path: `almanac/decisions/runtime/embedded-assets-with-user-ejection.md`
  - slug: `decisions/runtime/embedded-assets-with-user-ejection`
  - purpose: Record why Red embeds default runtime files while supporting user overrides and explicit ejection.
  - planned links: `../../architecture/runtime/runtime-assets`, `../../architecture/configuration/layered-config-recovery`, `../../guides/installers/release-installers`
  - key evidence files: `src/assets.rs`, `src/onboarding.rs`, `README.md`, `default_config.toml`

### Reference

#### `reference/cli/`

- path: `almanac/reference/cli/red-command.md`
  - slug: `reference/cli/red-command`
  - purpose: Reference the public `red` command flags, utility modes, conflicts, and internal hidden boundaries.
  - planned links: `../../architecture/startup/runtime-lifecycle`, `husk-command`, `../../reference/agent/agent-check`, `../../reference/runtime/self-check`
  - key evidence files: `src/cli.rs`, `src/main.rs`, `README.md`, `docs/GETTING_STARTED.md`
- path: `almanac/reference/cli/husk-command.md`
  - slug: `reference/cli/husk-command`
  - purpose: Reference the forwarded `red husk` and standalone Husk CLI subcommands.
  - planned links: `../../architecture/husk/public-embedding-api`, `../../architecture/husk/packages-and-locks`, `../../architecture/husk/extensions`, `red-command`
  - key evidence files: `crates/husk-cli/src/lib.rs`, `crates/husk-cli/tests/cli.rs`, `docs/HUSK_LANGUAGE_GUIDE.md`

#### `reference/configuration/`

- path: `almanac/reference/configuration/default-config.md`
  - slug: `reference/configuration/default-config`
  - purpose: Reference the effective default configuration surface for keys, plugins, LSP, search, picker, cursor, AI, and permissions.
  - planned links: `../../architecture/configuration/layered-config-recovery`, `../../architecture/runtime/runtime-assets`, `../../reference/lsp/configuration`, `../../reference/plugins/host-api`
  - key evidence files: `default_config.toml`, `src/config.rs`

#### `reference/lsp/`

- path: `almanac/reference/lsp/configuration.md`
  - slug: `reference/lsp/configuration`
  - purpose: Reference LSP configuration fields, default servers, document selectors, and Husk integration.
  - planned links: `../../architecture/lsp/client-lifecycle-and-routing`, `../../architecture/husk/language-server`, `../../concepts/lsp/capabilities`
  - key evidence files: `src/config.rs`, `default_config.toml`, `docs/HUSK_LSP.md`

#### `reference/plugins/`

- path: `almanac/reference/plugins/host-api.md`
  - slug: `reference/plugins/host-api`
  - purpose: Reference Red host API versioning, source-of-truth files, schema shape, and compatibility policy without copying the full schema.
  - planned links: `../../architecture/plugins/red-host-api`, `../../architecture/plugins/resource-ownership`, `../../guides/plugins/write-a-husk-plugin`
  - key evidence files: `src/plugin/host_api.json`, `src/plugin/api.rs`, `docs/PLUGIN_API.md`, `docs/plugin_api_changes.json`

#### `reference/agent/`

- path: `almanac/reference/agent/agent-check.md`
  - slug: `reference/agent/agent-check`
  - purpose: Reference `red --agent-check`, `--strict`, Codex executable resolution, version gating, and readiness output.
  - planned links: `../../architecture/agent/codex-app-server-workflow`, `../../reference/cli/red-command`
  - key evidence files: `src/agent_check.rs`, `src/main.rs`, `src/cli.rs`, `docs/AGENT_WORKFLOW.md`

#### `reference/runtime/`

- path: `almanac/reference/runtime/self-check.md`
  - slug: `reference/runtime/self-check`
  - purpose: Reference packaged runtime validation through hidden `red --self-check`.
  - planned links: `../../architecture/runtime/runtime-assets`, `../../architecture/plugins/lifecycle-and-reload`, `../../reference/cli/red-command`
  - key evidence files: `src/self_check.rs`, `tests/self_check.rs`, `src/main.rs`, `docs/DEBUGGING.md`

#### `reference/sessions/`

- path: `almanac/reference/sessions/detach-ipc-protocol.md`
  - slug: `reference/sessions/detach-ipc-protocol`
  - purpose: Reference detachable-session IPC messages, versioning, authentication, frame limits, and timeouts.
  - planned links: `../../architecture/sessions/detachable-editor-core`, `../../decisions/sessions/detachable-core-boundary`, `../../guides/sessions/detach-reattach`
  - key evidence files: `src/headless/mod.rs`, `src/main.rs`, `docs/DETACH.md`

#### `reference/editor/`

- path: `almanac/reference/editor/ui-components.md`
  - slug: `reference/editor/ui-components`
  - purpose: Reference modal UI component contracts for drawing, key handling, resize/theme updates, handles, and sensitive input.
  - planned links: `../../architecture/editor/rendering-pipeline`, `../../architecture/plugins/resource-ownership`, `../../concepts/plugins/callback-scoped-dialogs`
  - key evidence files: `src/ui/mod.rs`, `src/ui/picker.rs`, `src/ui/completion.rs`, `src/ui/agent_composer.rs`, `src/ui/input_prompt.rs`, `src/ui/file_picker.rs`
- path: `almanac/reference/editor/registers-clipboard-and-macros.md`
  - slug: `reference/editor/registers-clipboard-and-macros`
  - purpose: Reference registers, clipboard provider behavior, macro replay limits, and dot-repeat boundaries.
  - planned links: `../../concepts/editor/undo-tree`, `../../architecture/editor/text-mutation-boundary`, `../../concepts/editor/coordinate-systems`
  - key evidence files: `src/clipboard.rs`, `src/editor.rs`, `tests/editing.rs`

#### `reference/validation/`

- path: `almanac/reference/validation/ci-and-validation.md`
  - slug: `reference/validation/ci-and-validation`
  - purpose: Reference CI jobs, plugin checks, clippy policy, release checks, and validation commands.
  - planned links: `../../guides/development/build-test-and-validate`, `../../guides/releases/release-red`, `../../reference/runtime/self-check`
  - key evidence files: `.github/workflows/ci.yml`, `.github/workflows/plugin-check.yml`, `.github/workflows/nightly.yml`, `.github/workflows/release.yml`, `AGENTS.md`

#### `reference/vim/`

- path: `almanac/reference/vim/vim-compatibility.md`
  - slug: `reference/vim/vim-compatibility`
  - purpose: Reference Red's supported Vim behavior, intentional differences, and tests that enforce editing compatibility.
  - planned links: `../../concepts/red-editor`, `../../architecture/editor/text-mutation-boundary`, `../../reference/editor/registers-clipboard-and-macros`
  - key evidence files: `docs/VIM_COMPATIBILITY.md`, `tests/editing.rs`, `tests/movement.rs`, `src/editor.rs`
