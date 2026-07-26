---
title: "Husk Packages And Locks"
summary: "Husk packages are deterministic local source graphs described by Husk.toml and stabilized by Husk.lock when extensions are involved."
topics: [husk, packages, locks, architecture]
sources:
  - id: package-code
    type: file
    path: crates/husk-package/src/lib.rs
  - id: package-tests
    type: file
    path: crates/husk-package/tests/package.rs
  - id: git-core-manifest
    type: file
    path: plugins/git_core/Husk.toml
  - id: neotree-manifest
    type: file
    path: plugins/neotree_core/Husk.toml
---

Husk package resolution is a deterministic, filesystem-only system. A package is rooted at a `Husk.toml` manifest, its source graph starts at the declared entry file, and its lock file records the exact extension inputs that package commands must reproduce [@package-code]. This architecture keeps package loading local: there is no registry resolver in the package crate, embedded source packages cannot declare extensions, and extension bundles are validated through paths and digests rather than by executing generated state [@package-code]. It is the package layer that connects the public [Husk embedding API](public-embedding-api), [Husk extensions](extensions), the [Husk Scripts And Modules](../../decisions/husk/scripts-and-modules) decision, and the `husk` command reference.

## Manifest Shape

`PackageManifest` has schema version `1`, a `[package]` section, and an optional `extensions` map [@package-code]. The package section contains a local package name, a semantic version, and an entry path [@package-code]. The bundled Red Husk plugin packages use this minimal shape: `plugins/git_core/Husk.toml` declares `red-git-core` with `entry = "src/main.hk"`, and `plugins/neotree_core/Husk.toml` declares `red-neotree-core` with the same entry convention [@git-core-manifest] [@neotree-manifest].

Manifest validation is deliberately narrow. Package and extension names may contain ASCII alphanumeric characters, `_`, `-`, and `.`, while entry and path extension values must be normalized relative paths with no absolute components or parent traversal [@package-code]. Crate extension declarations also validate the crate package name, semantic version requirement, include list, default feature setting, feature list, and explicit generic specializations [@package-code].

## Module Resolution

`ResolvedPackage::open` canonicalizes the manifest, rejects symlinked control paths, verifies the manifest filename is `Husk.toml`, reads the bounded manifest, validates the entry path, and resolves source modules from the entry's parent directory [@package-code]. For a `mod child;` item, the resolver accepts exactly one of `child.hk` or `child/mod.hk`, depending on whether the parent module is the root or a `mod.hk` file [@package-code]. It rejects missing modules, ambiguous flat-versus-nested modules, module cycles, duplicate canonical files, paths escaping the source root, symlinked inputs, oversize sources, parse failures, and module counts above the configured limit [@package-code].

The tests lock in the important shape of this graph. They assert stable ordering for flat and nested modules, manifest discovery from a nested module file, rejection of missing and ambiguous module files, source-size limit enforcement, and fail-closed behavior for symlinked manifests or module cycles through symlinks [@package-tests].

## Lock Semantics

`Husk.lock` uses schema version `1` and records the locked package name, package version, and a deterministic map of locked extensions [@package-code]. For each extension, the lock stores the module name, extension version, local source path, component SHA-256, optional crate provenance, optional vendored artifact path, and optional adapter report SHA-256 [@package-code]. `PackageLock::validate_manifest` requires the package name, package version, extension count, declared extension names, paths, crate requirements, features, default-feature mode, include selections, and specializations to match the manifest [@package-code].

Lock enforcement compares the current resolved lock with the bytes parsed from `Husk.lock`; a difference returns `LockChanged` and tells the caller to regenerate the lock file [@package-code]. Tests cover deterministic local extension locks, changed component bytes becoming lock changes, package-specific lock byte limits, rejected changed generic specializations, and backward-compatible parsing of older locks without specialization or adapter-report provenance [@package-tests].

## Embedded Source Packages

`ResolvedPackage::from_sources` gives embedders a pure package path for source bundled in the host binary [@package-code]. It applies the same manifest parsing, source byte limit, module count limit, module layout rules, ambiguity checks, duplicate-source checks, and path normalization rules as filesystem packages, but it creates an empty lock and rejects any extension declarations [@package-code]. The tests cover stable embedded module order, package-relative display paths, and rejection of missing, ambiguous, duplicate, escaping, and extension-bearing embedded inputs [@package-tests].

The split matters for embedders. A host can ship a multi-file Husk program as static strings and still get normal module semantics, but it cannot smuggle unverifiable extension provenance into an embedded package. Extension-bearing packages require a filesystem root and a lock that can be checked.

## Extension Sources

Manifest extension values are either local path bundles or crate-backed extension requests [@package-code]. A path extension is opened as a `.huskext` bundle, its manifest name must equal the `extensions.<name>` key, and the package lock records the bundle digest [@package-code]. A crate-backed extension requires an existing lock entry; the locked source path is derived from `.husk/extensions/<digest>.huskext`, and the lock must also carry crate provenance and an artifact path [@package-code].

The package crate exposes helper paths for installed and vendored bundles: `.husk/extensions/<digest>.huskext` for local install state and `vendor/husk/<digest>.huskext` for committed vendor state [@package-code]. The standalone command behavior around these paths is listed in [Husk Command](../../reference/cli/husk-command), while the extension trust boundary is explained in [Extension Tiers](../../decisions/husk/extension-tiers) and [Husk Extensions](extensions).
