---
title: "Husk Semantic Profiles"
summary: "Husk uses a native semantic profile by default and keeps JavaScript compatibility isolated in an explicit legacy profile."
topics: [decisions, husk, semantics, plugins]
sources:
  - id: adr-0006
    type: file
    path: docs/adr/0006-husk-semantic-profiles.md
  - id: semantic
    type: file
    path: crates/husk-semantic/src/lib.rs
  - id: plugin-runtime
    type: file
    path: src/plugin/runtime.rs
  - id: compile-tests
    type: file
    path: crates/husk/tests/compile_pipeline.rs
---

Husk's accepted semantic-profile decision makes backend-neutral `Native` semantics the default for the embedding API and standalone CLI, while keeping `LegacyJavaScript` as an explicit compatibility profile for older frontend and Red plugin code [@adr-0006]. Native code does not silently receive JavaScript globals, dynamic `JsValue` behavior, or `js { ... }`; those constructs belong to the legacy profile and produce native diagnostics when used in native compilation [@adr-0006] [@semantic] [@compile-tests]. Red compatibility is handled with host declarations and adapters rather than a third source-language backend [@adr-0006] [@plugin-runtime].

## Status

This decision is accepted by ADR 0006, dated 2026-07-19 [@adr-0006]. The semantic analyzer defines `SemanticProfile::Native` and `SemanticProfile::LegacyJavaScript`, and its native options select the native profile with the prelude enabled [@semantic].

## Context

Husk is being extracted from a system that historically used JavaScript-oriented constructs. ADR 0006 names the compatibility surface directly: old tests and frontend code may depend on `JsValue`, `extern "js"`, JavaScript globals, or raw `js { ... }` blocks [@adr-0006]. Keeping those behaviors implicit in general Husk would make the standalone language and embedding API carry a hidden JavaScript assumption.

The semantic analyzer enforces the distinction. In legacy mode, `JsValue` is treated as a dynamic type, the JavaScript globals file is added with the prelude, and raw JavaScript literals type as `JsValue` [@semantic]. In native mode, JavaScript globals are not loaded and `js { ... }` emits the specific error message that the construct is only available in the legacy JavaScript profile [@semantic].

## Decision

Every compilation selects a semantic profile. Native is the backend-neutral language profile for embedded and standalone Husk, while `LegacyJavaScript` is a compatibility profile rather than the default language contract [@adr-0006]. The compile pipeline test confirms that `CompileOptions::default()` produces a program with `SemanticProfile::Native` and that native compilation rejects `js { window.location }` with the legacy-profile diagnostic [@compile-tests].

Red plugins compile under the explicit compatibility profile. `compile_plugin_source` builds `CompileOptions::legacy_runtime_compatibility()`, enables typechecking, selects `SemanticProfile::LegacyJavaScript`, and loads parsed `RED_HOST_DECLARATIONS` as trusted declarations before compiling plugin source [@plugin-runtime]. Those declarations describe the Red host API with `extern "red"` functions and host-facing structs, which links this decision to the [Red host API](../../architecture/plugins/red-host-api) instead of treating Red as another backend [@plugin-runtime].

## Consequences

Native Husk can evolve as the language described in [Husk language](../../concepts/husk-language) without inheriting JavaScript semantics by accident. New standard-library APIs should be native modules or HIR primitives, not JavaScript globals made available to every compilation [@adr-0006].

Red keeps compatibility, but the compatibility is visible. Plugin compilation includes host declarations and the legacy profile in one place, so future removal or narrowing of JavaScript compatibility has a clear boundary to inspect [@plugin-runtime]. The same split informs the [Husk language server](../../architecture/husk/language-server), because diagnostics and semantic services must know which profile a file is being checked under.

Tests now document which contract they exercise. The compile pipeline tests use native defaults for compiled artifacts and JavaScript rejection, while Red runtime tests can assert native compilation for embedded core packages separately from legacy plugin compatibility [@compile-tests] [@plugin-runtime].
