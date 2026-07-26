---
title: "Husk Extension Tiers"
summary: "Husk separates extension support into static native modules, portable WebAssembly Components, and a deferred trusted native ABI."
topics: [decisions, husk, extensions, wasm]
sources:
  - id: adr-0004
    type: file
    path: docs/adr/0004-husk-extension-tiers.md
  - id: adr-0009
    type: file
    path: docs/adr/0009-wasm-component-extension-go.md
  - id: extension
    type: file
    path: crates/husk-extension/src/lib.rs
  - id: wasm
    type: file
    path: crates/husk-wasm/src/lib.rs
---

Husk's accepted extension decision is a three-tier model: Rust embedders use statically linked native modules, the standalone tool loads portable WebAssembly Component bundles, and any native C ABI remains a future trusted-only tier [@adr-0004]. The choice rejects arbitrary Rust dynamic loading because Rust does not provide a stable general-purpose ABI for that contract, and it keeps every extension boundary narrow, versioned, and typed [@adr-0004]. The current implementation already enforces the portable tier through `.huskext` bundle validation, manifest schema checks, component digesting, import capability validation, Wasmtime component inspection, and per-instance Wasmtime stores [@extension] [@wasm].

## Status

This decision is accepted by ADR 0004, dated 2026-07-19 [@adr-0004]. ADR 0009 later accepts WebAssembly Components as the portable dynamic extension tier and sets three-platform CI as a release gate for that tier [@adr-0009].

## Context

Husk needs two different extension experiences. Embedded applications want to expose Rust crates without runtime loading overhead, while the standalone interpreter needs an extension format that can be loaded dynamically, inspected for types, and denied ambient authority by default [@adr-0004] [@adr-0009]. Rust `rlib` files, trait objects, and Rust-owned dynamic-library values are not suitable for that public boundary because they would depend on compiler, allocator, panic, layout, and dependency details outside Husk's control [@adr-0009].

The extension bundle crate reflects that boundary before Wasmtime is involved. It defines a versioned `extension.toml` manifest, validates package and module names, rejects absolute or non-normal artifact paths, rejects symlinks, bounds manifest and component sizes, canonicalizes the component path under the bundle root, and computes a SHA-256 digest of the component bytes [@extension]. It also models capabilities as normalized dotted names and enforces `actual imports <= requested capabilities <= granted capabilities` [@extension].

## Decision

Static native modules are the preferred Rust embedding API. An embedding application links ordinary Cargo dependencies and registers typed adapter functions with a Husk `Engine`; this fits the [public embedding API](../../architecture/husk/public-embedding-api) because the host application owns the trust boundary and compilation environment [@adr-0004].

WebAssembly Components are the preferred portable dynamic extension format for standalone Husk. Extension authors compile an adapter for the Component Model, the host derives a Husk `ModuleDescriptor` from component exports, calls use Component `Val`, and JSON is not an implicit transport [@adr-0009] [@wasm]. The implementation creates a Wasmtime component engine with the component model and fuel enabled, compiles component bytes, inspects imports, validates capabilities, and derives descriptors from exports before instantiation [@wasm].

A native C ABI may be added later only for trusted, platform-specific deployments. ADR 0004 and ADR 0009 both keep that tier explicit and opt-in, not a fallback that weakens the portable tier [@adr-0004] [@adr-0009].

## Consequences

The [extension architecture](../../architecture/husk/extensions) has a clear rule for new extension work: do not expose Rust implementation types or ad hoc JSON as the public extension contract. Static adapters and Wasm Components both describe Husk-facing modules through descriptors and typed values [@adr-0004] [@adr-0009].

Portable extensions are sandboxable, but only dependency graphs that compile to the Component target can use that tier [@adr-0009]. The Wasm loader intentionally links no WASI implementation, starts with an empty linker, gives components no ambient filesystem, network, environment, clock, random, or process access, and applies fuel plus store resource limits to each instantiated component [@wasm].

Native-only crates still have a supported path through static host adapters. They need a host application that chooses to link and trust them, or a future trusted ABI if that tier is later accepted [@adr-0004] [@adr-0009].
