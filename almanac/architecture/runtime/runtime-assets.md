---
title: "Runtime Assets"
summary: "Red resolves plugins and themes from user files, RED_RUNTIME development files, and embedded assets, with listing and ejection paths for customization."
topics: [architecture, runtime, runtime-assets, plugins, themes]
sources:
  - id: asset-system
    type: file
    path: src/assets.rs
  - id: bundled-plugins
    type: file
    path: plugins/
  - id: bundled-themes
    type: file
    path: themes/
  - id: plugin-registry
    type: file
    path: src/plugin/registry.rs
  - id: config-loader
    type: file
    path: src/config.rs
---

# Runtime Assets

Runtime assets are Red's portable source for bundled Husk plugins and VS Code-compatible themes. The asset system resolves only two public categories, `plugins` and `themes`, and searches user config files first, a development `RED_RUNTIME` directory second, and files embedded in the binary last [@asset-system]. This gives users a way to override or eject a single asset while keeping Red able to start from a self-contained binary with bundled plugins and themes [@bundled-plugins] [@bundled-themes].

## Asset Kinds And Safe Names

`RuntimeAssetKind` owns the category boundary: plugin assets resolve under `plugins`, and theme assets resolve under `themes` [@asset-system]. Resolution normalizes names through `safe_relative_path`, which trims a leading slash, rejects empty paths, and rejects parent directories, roots, and platform prefixes before searching the selected category root [@asset-system]. Public listing and ejection use the stricter direct-file surface: listed plugins must be direct `.hk` files, listed themes must be direct `.json` files, and ejection accepts either `plugins/<file>` or `themes/<file>` category paths or bare supported plugin and theme file names [@asset-system].

Embedded asset listing deliberately hides plugin files that are not public runtime plugins. The public embedded plugin filter excludes `test.hk`, `unicode_demo.hk`, and files ending in `.test.hk`; themes do not have the same special exclusion [@asset-system]. The repository's bundled plugin corpus includes user-facing `.hk` shells such as `theme_browser.hk`, `git.hk`, `neotree.hk`, and `agent.hk`, plus core package directories used by some plugins [@bundled-plugins].

## Resolution Precedence

Resolution returns a `ResolvedRuntimeAsset` with its kind, file name, source layer, optional filesystem path, and optional embedded contents [@asset-system]. The search order is user config directory, `RED_RUNTIME`, then embedded assets; the first existing file wins [@asset-system]. `Config::resolve_plugin_path` uses this resolution for relative configured plugins, returning an absolute configured path unchanged, a filesystem path for user or development assets, or a private bundled specifier for embedded plugins [@config-loader].

The private bundled plugin scheme is `red-bundled:///plugins/<file>`. Embedded plugin assets cannot be handed to Husk as normal filesystem paths, so `ResolvedRuntimeAsset::plugin_specifier` returns that URI and `bundled_plugin_contents` maps it back to embedded source when the registry reads plugin code [@asset-system] [@plugin-registry]. `plugin_source` in the registry branches on the bundled specifier and reads embedded contents instead of using `fs::read_to_string` [@plugin-registry].

## Listing And Shadowing

`red --runtime-files` is backed by `format_runtime_files`, which prints plugins and themes separately, lists the winning source for each asset, reports lower-precedence layers shadowed by the winner, and ends with the resolution order and ejection hint [@asset-system]. `list_runtime_assets` builds that output by collecting names from the user category directory, the optional `RED_RUNTIME` category directory, and embedded files into a source set keyed by file name [@asset-system].

Theme list entries also attempt to parse the selected theme JSON's `name` field for display metadata [@asset-system]. Plugin list entries do not parse metadata there; plugin command and lifecycle metadata belongs to plugin discovery and activation, described in [Lifecycle And Reload](../plugins/lifecycle-and-reload).

## Ejection

Ejection copies a resolved non-user asset into the user's config directory. `eject_runtime_asset` parses the requested category and file, refuses to overwrite an existing user file unless `--eject-force` was supplied, resolves the source while excluding the user layer, creates the target directory, and writes the selected contents [@asset-system]. This means ejection can copy a `RED_RUNTIME` asset before an embedded asset when both exist, matching the non-user half of the normal precedence order [@asset-system].

The result is a customization boundary rather than a synchronization mechanism. Once copied under the user config directory, the ejected file becomes the highest-precedence source and shadows both `RED_RUNTIME` and embedded copies of the same file [@asset-system]. The decision behind this shape is documented in [Embedded Assets With User Ejection](../../decisions/runtime/embedded-assets-with-user-ejection).

## Runtime Consumers

Startup consumes theme assets through `load_theme`, while configuration and plugin discovery consume plugin assets through `Config::resolve_plugin_path` and the plugin registry [@config-loader] [@plugin-registry]. The runtime asset system does not activate plugins, validate host API compatibility, or interpret VS Code colors; it only locates and reads safe runtime files. The downstream consumers are [Theme Import](../themes/theme-import), which parses theme JSON, and plugin lifecycle pages such as [Lifecycle And Reload](../plugins/lifecycle-and-reload), which explain activation and reload behavior.

Operational checks for the embedded runtime appear in [Self Check](../../reference/runtime/self-check).
