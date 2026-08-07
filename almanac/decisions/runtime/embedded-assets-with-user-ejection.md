---
title: "Embedded Assets With User Ejection"
summary: "Red embeds default runtime assets in the binary while allowing user and development overrides and explicit ejection into the user config directory."
topics: [decisions, runtime, runtime-assets, configuration]
sources:
  - id: assets
    type: file
    path: src/assets.rs
  - id: onboarding
    type: file
    path: src/onboarding.rs
  - id: main
    type: file
    path: src/main.rs
  - id: defaults
    type: file
    path: default_config.toml
---

Red chooses to embed its default configuration, bundled Husk plugins, and bundled themes in the binary, while still resolving user overrides and offering explicit ejection into the user's config directory. Runtime asset resolution prefers user config files, then `$RED_RUNTIME`, then embedded assets, and `red --eject` copies a non-user asset into the user layer for customization [@assets] [@main]. The consequence is that a fresh installed binary can start without checkout-relative runtime files, but users retain a concrete file-based escape hatch when they want to inspect or override a bundled plugin or theme.

## Context

Red needs default runtime files before user configuration can be trusted. `assets::DEFAULT_CONFIG` embeds `default_config.toml`, and the asset module embeds the `themes` and `plugins` directories with `include_dir` [@assets]. The default config enables bundled plugins by relative `.hk` names and selects the default `red.json` theme [@defaults]. If those names depended only on files beside the executable or on a source checkout, a packaged install would have to manage a separate runtime tree before the editor could load its defaults.

First-run onboarding reinforces the same constraint. When `config.toml` is absent, non-interactive sessions launch from embedded defaults, while interactive sessions may write only a starter config template [@onboarding]. The onboarding tests assert that writing default assets creates `config.toml` but does not create a `themes` directory, because plugins and themes remain embedded until the user chooses to override them [@onboarding].

## Decision

Runtime assets are resolved from three ordered layers: the user config directory, the development runtime selected by `RED_RUNTIME`, and embedded assets [@assets]. `resolve_runtime_asset` accepts only safe relative names, rejects parent traversal, and returns either a filesystem-backed asset or embedded contents [@assets]. Embedded plugin assets can also become private `red-bundled:///plugins/<file>` specifiers for the Husk loader [@assets].

Red exposes the chosen asset set with `red --runtime-files`, which prints plugins, themes, their winning source layer, shadowed lower-precedence layers, and the resolution order [@assets] [@main]. Red also exposes `red --eject` and `red --eject-force`; those commands copy a development or embedded asset into the user config directory, preserving existing user files unless force is supplied [@assets] [@main]. Ejection deliberately resolves from non-user layers so an existing customized file is never copied onto itself [@assets].

## Status

This decision is active. `src/main.rs` dispatches both runtime-file listing and ejection before onboarding and editor startup, and normal theme loading uses `assets::resolve_theme` with embedded fallback for the default theme [@main]. Plugin path resolution in configuration also goes through the asset resolver for relative plugin names, which lets bundled plugins load from embedded assets while user files shadow them [@assets].

## Consequences

Fresh installs are self-contained. Missing user configuration is recoverable because embedded defaults provide the baseline, and onboarding can write a commented starter config without copying all bundled runtime files [@assets] [@onboarding]. The runtime architecture page [Runtime assets](../../architecture/runtime/runtime-assets) explains the full resolution model, and [Layered Config Recovery](../../architecture/configuration/layered-config-recovery) explains how embedded defaults become the configuration baseline.

User customization stays explicit. A user file in the config directory shadows `$RED_RUNTIME` and embedded assets, and `red --eject` creates the file only when the user asks for a local copy [@assets] [@main]. This avoids silently materializing stale copies of bundled plugins or themes during onboarding.

The asset boundary must reject unsafe specifiers. `safe_relative_path` rejects empty, absolute, parent, root, and platform-prefix paths before asset lookup, and `parse_asset_path` only accepts direct `plugins/<file>` or `themes/<file>` asset names with supported extensions [@assets]. Any new runtime asset kind should preserve that public-specifier rule.

Development still has an override lane. `$RED_RUNTIME` can supply plugins and themes between user and embedded layers, which is useful for source-tree development and testing without changing user config or rebuilding the binary [@assets]. Because the user layer has higher precedence, a developer must account for user-shadowed assets when debugging runtime-file behavior.
