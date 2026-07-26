---
title: "Husk Value Semantics"
summary: "Native Husk fixes value, evaluation, equality, closure, indexing, and JSON-boundary semantics for the interpreter and host API."
topics: [decisions, husk, semantics, runtime]
sources:
  - id: adr-0008
    type: file
    path: docs/adr/0008-husk-value-semantics.md
  - id: runtime
    type: file
    path: crates/husk-runtime/src/lib.rs
  - id: native-features
    type: file
    path: crates/husk/tests/native_language_features.rs
  - id: native-stdlib
    type: file
    path: crates/husk/tests/native_stdlib.rs
---

Husk's accepted value-semantics decision fixes the native language target for primitive operations, containers, closures, equality, indexing, and JSON boundaries [@adr-0008]. Native execution uses checked integer behavior, strict boolean conditions, Unicode-aware string slicing, nominal structs and enums, shared closure capture cells, structural equality for ordinary values, and explicit JSON conversion rather than accidental JavaScript compatibility [@adr-0008] [@runtime] [@native-features] [@native-stdlib]. The decision gives HIR, interpreter, standard-library, and [public embedding API](../../architecture/husk/public-embedding-api) work a testable semantic target.

## Status

This decision is accepted by ADR 0008, dated 2026-07-19 [@adr-0008]. Native language feature and standard-library tests exercise many of the rules in the current interpreter [@native-features] [@native-stdlib].

## Context

Husk had to freeze observable semantics before deeper HIR and runtime extraction work could proceed. ADR 0008 names the risky areas directly: operand order, mutability, integer precision, floating-point behavior, strings, containers, closure capture, equality, indexing, and the boundary between JSON-like host data and nominal Husk values [@adr-0008]. Without a decision, low-level storage choices such as `Arc`, shared cells, or arena allocation could silently redefine assignment or copy behavior.

The runtime value type shows why the boundary is explicit. `Value` has native variants for unit, null, booleans, integers, floats, strings, arrays, tuples, ranges, objects, structs, enum variants, resources, callbacks, and closures, plus a `Json` variant for legacy host paths and a `Missing` value that keeps missing-field diagnostic context [@runtime]. JSON conversion enters through `Value::from_json` and leaves through `Value::to_json`, rather than making JSON the internal representation for all compound data [@runtime].

## Decision

Native Husk uses strict evaluation and mutation rules. ADR 0008 states that ordinary calls and binary operands evaluate left-to-right, boolean conditions require `bool`, logical operators short-circuit, blocks create lexical scopes, and mutation or rebinding requires `let mut` [@adr-0008]. The native feature tests exercise mutable receivers, array mutation, higher-order array methods, range iteration, methods, associated functions, pattern matching, and trait impl methods through lowered HIR [@native-features].

Integer and floating-point behavior is explicit. ADR 0008 requires resolved `i32` and `i64` arithmetic to be checked without an `f64` round trip, treats division by zero, overflow, and invalid casts as source-aware runtime errors, and requires floating-point values to use finite `f64` behavior without accidental NaN semantics [@adr-0008]. Tests cover checked casts, exact `i64` to `f64` conversion, rejection of inexact infallible conversion, finite number parsing, and source-aware invalid cast diagnostics [@native-features] [@native-stdlib].

Compound values keep native identity. Tests assert nominal structs, tuples, enums, range values, `Option` and `Result` variants, Unicode string slicing, shared closure captures, nested closures, captured function handles, and static source types for generic functions and trait methods [@native-features]. The runtime `PartialEq` implementation compares ordinary data structurally, compares resources by type and handle, treats callbacks and closures by identity, and preserves legacy null-like comparisons for missing values and `Unit`/`Null` compatibility [@runtime].

## Consequences

Native Husk behavior can be tested independently from Red compatibility. The semantic profile decision keeps native execution and legacy JavaScript compatibility separate, so production Red differences such as missing/null compatibility or fallback indexing belong in the Red adapter or explicit compatibility profile, not in the general native semantics target [@adr-0008]. See [semantic profiles](semantic-profiles) for that boundary.

Host-facing values must not redefine language behavior. The runtime stores arrays, tuples, objects, structs, and variants as native value shapes, and the embedding API converts host data through typed values rather than requiring ad hoc JSON transport [@runtime]. That protects assignments, equality, and closure capture from changing when the host representation changes.

Future semantic changes need versioned language work and tests. ADR 0008 allows revisiting the decision only through a versioned language change backed by conformance tests, or when the production plugin corpus proves that an observable behavior cannot be isolated in the Red compatibility layer [@adr-0008].
