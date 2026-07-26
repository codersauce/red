---
title: "Husk Extensions"
summary: "Husk extensions are split between trusted static native modules and portable WebAssembly Component bundles with manifest, capability, digest, and adapter workflows."
topics: [husk, extensions, wasm, architecture]
sources:
  - id: extension-adr
    type: file
    path: docs/adr/0004-husk-extension-tiers.md
  - id: wasm-adr
    type: file
    path: docs/adr/0009-wasm-component-extension-go.md
  - id: bundle-code
    type: file
    path: crates/husk-extension/src/lib.rs
  - id: wasm-code
    type: file
    path: crates/husk-wasm/src/lib.rs
  - id: cli-code
    type: file
    path: crates/husk-cli/src/lib.rs
---

Husk extensions have two implemented tiers: static native modules for Rust embedders and portable `.huskext` bundles backed by WebAssembly Components for dynamic loading. The accepted extension-tier decision keeps those paths separate because Rust does not provide a stable general-purpose ABI for loading arbitrary crate artifacts, while Wasm Components give the standalone interpreter a typed and inspectable dynamic boundary [@extension-adr] [@wasm-adr]. This page describes the runtime architecture behind that decision; the CLI operations that package and inspect extensions are listed in [Husk Command](../../reference/cli/husk-command).

## Static Native Tier

Static native modules are the preferred extension path when a Rust application owns its Cargo graph. The tier links ordinary Cargo dependencies into the embedding application and registers typed adapter functions with a Husk `Engine`, avoiding runtime crate loading and letting the embedding application carry the trust decision [@extension-adr]. That contract is implemented through the public [Husk embedding API](public-embedding-api), where a `NativeModule<T>` exposes descriptor-backed functions and receives mutable host state through `CallContext`.

The consequence is direct but explicit integration. A crate does not become callable merely because it is present in Cargo metadata; the host chooses the module name, callable functions, parameter mapping, return mapping, and error boundary in Rust code [@extension-adr]. Native-only crates that need operating-system APIs or cannot compile to a Wasm Component remain compatible with Husk through this static adapter tier.

## Portable Bundle Boundary

The dynamic tier is a directory bundle validated before component compilation. A `.huskext` bundle contains an `extension.toml` manifest and the component artifact named by that manifest [@bundle-code]. The manifest has schema version `1`, package name, version, Husk module name, artifact path, WIT world, minimum Husk version, and requested capabilities [@bundle-code]. Bundle opening rejects symlinked control files, non-directory roots, unsupported schemas, invalid package names, invalid module names, absolute or non-normal artifact paths, artifacts escaping the bundle, missing artifact files, oversize manifests, and oversize components [@bundle-code].

The bundle digest is the SHA-256 identity of the exact component bytes [@bundle-code]. Package locks use that digest to connect manifest declarations to installed or vendored extension bundles, which is why [Husk Packages And Locks](packages-and-locks) treats extensions as verified local inputs instead of as runtime downloads.

## Capability Policy

Capability validation enforces `actual imports ⊆ requested capabilities ⊆ granted capabilities` [@bundle-code]. Capability names are dot-separated lowercase segments that may contain digits, `_`, and `-`; duplicate requested capabilities are rejected during manifest validation [@bundle-code]. The Wasm extension loader inspects actual component imports, maps them to Husk capability categories, and validates them before instantiation [@wasm-code].

The current standalone runtime grants no capabilities by default and links no WASI or custom capability provider during component instantiation [@wasm-code] [@cli-code]. As a result, portable runtime extensions are pure unless a host explicitly provides and grants imports in a future capability provider path. `husk extension inspect` can validate and display an import set by granting the requested capabilities for inspection, but ordinary CLI execution still compiles bundles with default grants [@cli-code].

## Component Inspection And Instances

`WasmComponent::from_bundle` checks `minimum_husk`, derives requested capabilities from the bundle manifest, compiles the component, inspects imports, validates capability policy, and derives the Husk `ModuleDescriptor` from component exports [@wasm-code]. Export inspection supports root functions and interface functions, normalizes WIT names to Husk identifiers, rejects normalization collisions, rejects unsupported export kinds, and maps supported WIT types into Husk type descriptors [@wasm-code]. Component calls use Wasmtime `Val`; the Wasm decision explicitly rejects JSON as an implicit transport [@wasm-adr].

The compiled component is shared, but each Husk instance receives an isolated Wasmtime store and component instance [@wasm-adr] [@wasm-code]. Per-instance limits cover fuel, memory, tables, core instances, table count, memory count, and boundary value bytes [@wasm-code]. Guest failures that make an instance unsafe to resume poison that extension instance, while other Husk instances remain separate [@wasm-code].

## Adapter And CLI Workflow

The CLI exposes the portable tier through `extension inspect`, `extension componentize`, and `extension pack` [@cli-code]. `pack` assembles a bundle from an existing manifest and component without invoking Cargo, then reopens the bundle through normal validation before reporting its digest [@bundle-code] [@cli-code]. `componentize` turns a WIT-aware core module into a component, verifies that its exports and imports fit the Husk boundary, and refuses unexpected imports [@cli-code].

Crate adapter workflows sit above that lower-level bundle path. `husk add` generates, sandbox-builds, verifies, vendors, installs, and locks a Rust crate adapter, while `husk crate inspect`, `interface`, `adapter`, and `build-adapter` expose the intermediate analysis and build steps [@cli-code]. The CLI verifies adapter components against generated reports before publishing build artifacts, including expected interface names and selected exports [@cli-code]. This workflow keeps dynamic crate loading out of the runtime and makes adapter selection reviewable, which is the core constraint recorded in [Extension Tiers](../../decisions/husk/extension-tiers).
