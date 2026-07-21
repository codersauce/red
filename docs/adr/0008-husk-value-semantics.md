# ADR 0008: Husk value and evaluation semantics

- Status: accepted
- Date: 2026-07-19
- Scope: primitive operations, containers, closures, JSON, indexing, and equality

## Decision

Native Husk uses these rules:

- calls and ordinary binary operands evaluate left-to-right;
- conditions and `!` require `bool`, and `&&`/`||` short-circuit;
- blocks create lexical scopes, and mutation or rebinding requires `let mut`;
- resolved `i32` and `i64` arithmetic is checked without an `f64` round trip;
  division/remainder by zero, overflow, and invalid casts are source-aware
  runtime errors;
- floating-point values use IEEE `f64`, with NaN comparisons diagnosed rather
  than inherited accidentally from a Rust helper;
- strings iterate and index by Unicode scalar value initially;
- arrays, tuples, structs, and enums retain the production interpreter's
  observable value/copy-on-write behavior;
- closures capture bindings through shared cells so a mutable captured binding
  is observed consistently by all closures from that scope;
- equality is structural for ordinary data and must terminate for cyclic
  values; host resources and functions use identity equality or are rejected
  statically as non-comparable;
- ordinary invalid indexing is source-aware and fails. Compatibility helpers
  may explicitly offer fallback behavior;
- JSON is an explicit boundary type. JSON null and a missing JSON field may
  retain Red's null-like compatibility behavior, but nominal struct fields do
  not inherit it.

String concatenation with a non-string is resolved through a backend-neutral
display contract. A compatibility difference required by a production Red
plugin belongs in the Red adapter or an explicit profile; embedded and CLI
native Husk do not diverge.

The current exceptions are inventoried in the
[runtime support matrix](../HUSK_RUNTIME_SUPPORT_MATRIX.md), and the broader
rationale is in the
[semantic rules section](../HUSK_LANGUAGE_EXTRACTION_PLAN.md#semantic-rules-to-freeze-before-hir).

## Consequences

- HIR and interpreter work has a testable semantic target.
- Internal `Arc`, cell, or arena choices cannot silently redefine assignment
  behavior.
- Native integer correctness does not depend on floating-point precision.
- Red can preserve missing/null and fallback-index compatibility while new
  scripts receive stricter diagnostics.

## Revisit when

Revisit only through a versioned language change backed by conformance tests,
or when the production plugin corpus proves an existing observable behavior
cannot be isolated in the Red compatibility layer.
