---
title: "Bundled Husk Plugins"
summary: "Bundled Husk plugins are the editor features shipped as embedded Husk source, with some plugins delegating pure logic to native Husk packages."
topics: [concepts, plugins, husk, runtime-assets]
sources:
  - id: plugins-dir
    type: file
    path: plugins/
  - id: default-config
    type: file
    path: default_config.toml
  - id: assets
    type: file
    path: src/assets.rs
  - id: runtime
    type: file
    path: src/plugin/runtime.rs
  - id: tree-model
    type: file
    path: src/plugin/tree.rs
---

Bundled Husk plugins are the Husk feature layer that ships with Red itself. The repository embeds the `plugins/` tree into the binary as runtime assets, while `default_config.toml` selects the default configured plugin set by mapping plugin names to `.hk` files [@assets] [@default-config]. Most bundled behavior lives in single `.hk` compatibility shells, but Git and Neo-tree also call embedded pure Husk packages for typed parsing, path handling, row construction, and command argument construction [@plugins-dir] [@runtime]. This distinction matters when changing plugin behavior: the shell owns editor events and host calls, while the core package owns deterministic logic that can be compiled and tested as Husk code.

## Bundled Versus Enabled

The bundled corpus is the set of plugin files under `plugins/`, including editor-facing `.hk` files and pure package directories such as `plugins/git_core/` and `plugins/neotree_core/` [@plugins-dir]. Runtime asset resolution embeds public `.hk` plugin files from that directory, filters out test and demo plugin names from public embedded listings, and uses a private `red-bundled:///plugins/<file>` specifier when an embedded plugin has no filesystem path [@assets].

Default enablement is a separate configuration decision. The `[plugins]` table in `default_config.toml` maps names such as `agent`, `git`, `neotree`, `project_search`, and `theme_browser` to bundled `.hk` files, while `disabled_plugins` and `disable_ai` let configuration remove defaults without editing bundled sources [@default-config]. The editor resolves configured plugin paths through the runtime asset system before the plugin registry loads them, so a user copy or `RED_RUNTIME` copy can shadow an embedded plugin with the same file name [@assets].

## Shells And Core Packages

Most bundled plugins are `.hk` shells that register commands, listen to events, request editor state, and update editor-owned UI resources through the Red host API [@plugins-dir]. Examples include buffer picking, theme browsing, search decoration, breadcrumbs, inlay hints, project search, LSP symbol pickers, agent UI, and Git or Neo-tree panels [@plugins-dir].

Git and Neo-tree have an extra split. `plugins/git.hk` remains the editor-facing plugin, but `plugins/git_core/` contains a Husk package with separate modules for status parsing, patch modeling, and Git command arguments [@plugins-dir]. `plugins/neotree.hk` stays responsible for panel events and filesystem actions, while `plugins/neotree_core/` contains pure path, status, and compatibility tree-row modules [@plugins-dir]. Large Neo-tree panels use a Rust-owned virtual model that shares Husk directory-entry arrays, indexes every expanded entry, and decorates only visible terminal rows [@tree-model]. The runtime embeds both package source sets with `ResolvedPackage::from_sources`, compiles them under the native semantic profile, caches their compiled programs, and exposes them only through internal `red::git_core` and `red::neotree_core` operations [@runtime].

That split keeps public plugin compatibility small. A shell can continue to use Red events and UI calls, while pure package code can use typed data models without becoming a public host API. The user-facing host contract for plugin authors belongs in [Plugin host API](../../reference/plugins/host-api), not in the internal bridge operation names [@runtime].

The Git shell owns the user-facing operation workflow around those pure helpers. User-triggered mutating operations such as commit, pull, merge, rebase, cherry-pick, revert, and safe-sync branches display transient progress through a plugin overlay, use host-managed busy animation, and then show bounded success or failure text [@plugins-dir]. Background refresh and hunk application stay quiet, while the safe-sync ahead branch routes into the normal push menu instead of bypassing its existing confirmation and progress flow [@plugins-dir].

The Git commit flow opens an editor-owned scratch buffer named `[Git Commit].gitcommit` with `GitSubmitMessage` as its submit command and `GitCancelMessage` as its cancel command [@plugins-dir]. The scratch text contains a marker line, and only text above that marker becomes the commit message; the generated context below it is ignored before `git commit` receives stdin [@plugins-dir]. The scratch buffer text tells users that `:w` or `:wq` submits and `:q` cancels, while the default Space command neighborhood also binds `Space c c` to `GitSubmitMessage` and `Space c q` to `GitCancelMessage` [@plugins-dir] [@default-config].

Neo-tree's shell also owns the user-facing create/reveal flow. A successful file create records that the result should be selected, waits for the `FileOperation` result's canonical `created` path, resets the tree through its existing reveal machinery, expands parent directories as needed, and finally calls `SelectPanelRow` for the created file [@plugins-dir] [@runtime]. Directory creation, cancelled prompts, and failed file operations clear the pending selection intent so a later refresh cannot highlight a stale row [@plugins-dir] [@runtime].

## Relationship To Runtime Assets

Bundled plugins are one layer in Red's runtime asset precedence. `src/assets.rs` resolves assets from the user config directory first, then `$RED_RUNTIME`, then embedded assets; its listing code reports the winning source and lower-precedence shadows [@assets]. This means a bundled plugin is stable enough to ship in the binary, but it is not necessarily the bytes running in a development or customized profile.

The plugin registry treats embedded plugin specifiers specially: bundled specs are not polled for hot reload and are read back through embedded contents rather than `fs::read_to_string` [@runtime] [@assets]. Development-time and user plugins follow the same [plugin lifecycle and reload](../../architecture/plugins/lifecycle-and-reload) machinery once their source has been resolved.

## Where To Read Next

Read [Husk language](../../concepts/husk-language) for the scripting language and package model that bundled plugins use. Read [Plugin lifecycle and reload](../../architecture/plugins/lifecycle-and-reload) for activation, quarantine, and reload behavior. Read [Packages and locks](../../architecture/husk/packages-and-locks) when the change involves `plugins/git_core/` or `plugins/neotree_core/`.
