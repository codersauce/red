---
title: "Husk Decisions"
summary: "Reading-order hub for Husk decisions about native semantics, package execution, engine ownership, embedding, and extension tiers."
topics: [decisions, husk, navigation]
sources:
  - id: semantic-profiles
    type: file
    path: almanac/decisions/husk/semantic-profiles.md
  - id: value-semantics
    type: file
    path: almanac/decisions/husk/value-semantics.md
  - id: scripts-modules
    type: file
    path: almanac/decisions/husk/scripts-and-modules.md
  - id: engine-ownership
    type: file
    path: almanac/decisions/husk/engine-instance-ownership.md
  - id: extension-tiers
    type: file
    path: almanac/decisions/husk/extension-tiers.md
---

# Husk Decisions

The Husk decision cluster records how the language stays backend-neutral, reproducible, embeddable, and narrow at dynamic extension boundaries. Read it when changing the Husk parser, semantic analyzer, runtime, standalone CLI, package resolver, public embedding API, Red plugin runtime, or WebAssembly extension support. The individual decisions are separate because language semantics, script execution, engine ownership, and extension loading constrain different implementation layers [@semantic-profiles] [@value-semantics] [@scripts-modules] [@engine-ownership] [@extension-tiers].

## Language Semantics

Start with [Semantic Profiles](semantic-profiles) when a change could reintroduce JavaScript assumptions into native Husk. The accepted model makes `Native` the default profile and keeps `LegacyJavaScript` as an explicit compatibility mode for older frontend and Red plugin code [@semantic-profiles].

Then read [Value Semantics](value-semantics) for the observable runtime rules behind native Husk values. That decision fixes strict boolean conditions, checked integer behavior, Unicode-aware string slicing, nominal structs and enums, closure capture behavior, structural equality, and explicit JSON conversion [@value-semantics].

## Execution And Ownership

Read [Scripts And Modules](scripts-and-modules) when the work touches `husk run`, package manifests, module resolution, script arguments, or lock-file-backed extension inputs. The accepted standalone shape requires an explicit `main` entrypoint and keeps package resolution local and deterministic [@scripts-modules].

Read [Engine Instance Ownership](engine-instance-ownership) when changing embedding, plugin reload, compilation reuse, or mutable execution state. Husk separates immutable `Engine` and `CompiledModule` artifacts from per-run `Instance` state so Red can stage plugin reloads without mutating a live plugin generation during compilation [@engine-ownership].

## Extension Boundaries

Read [Extension Tiers](extension-tiers) before adding or changing external extension mechanisms. Husk accepts statically linked native modules for Rust embedders and portable WebAssembly Component bundles for dynamic standalone extension loading; arbitrary Rust dynamic loading remains outside the public contract [@extension-tiers].

For runtime flow after these choices are understood, continue to [Husk Architecture](../../architecture/husk). For exact command lookup, use [Husk Command](../../reference/cli/husk-command).
