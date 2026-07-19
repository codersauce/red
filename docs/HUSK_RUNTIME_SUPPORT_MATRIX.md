# Husk production runtime support matrix

This document freezes the behavior of the production Husk interpreter before
the language is extracted from Red. It describes the runtime at commit
`b6442b6e`; it is an inventory, not a promise that every current behavior is
desirable.

The implementation roadmap and intended replacements live in the
[Husk language extraction plan](HUSK_LANGUAGE_EXTRACTION_PLAN.md). Changes to
the interpreter must update this matrix and its conformance tests intentionally.

## Status vocabulary

- **Executed** means the production VM evaluates the construct.
- **Partial** means the parser accepts it but the VM erases or supports only a
  restricted form.
- **Runtime gap** means parsing succeeds and execution produces an explicit
  unsupported-construct error.
- **Compile-only** means another Husk crate may understand the construct, but
  the production VM does not retain or execute it.

## Top-level items

| Construct | Status | Current production behavior |
| --- | --- | --- |
| Function | Executed | Stored by unqualified name. Parameter and return types, type parameters, visibility, and attributes do not affect execution. A later duplicate name replaces an earlier one. |
| Struct | Compile-only | Parsed, then discarded while constructing `Program`. Struct *expressions* separately become anonymous objects. |
| Enum | Compile-only | Parsed, then discarded. |
| Type alias | Compile-only | Parsed, then discarded. |
| `use` | Compile-only | Parsed, then discarded. Paths still evaluate as strings when used as expressions. |
| Trait and `impl` | Compile-only | Parsed, then discarded. |
| `extern` block | Compile-only | Parsed, then discarded. |

Only functions survive `Program::parse_at`. There is no executable module,
global, constant, or top-level-statement model in the production VM.

## Statements

| Construct | Status | Current production behavior |
| --- | --- | --- |
| `let name = value` | Executed | Adds or replaces a function-local binding. An omitted initializer stores `Unit`. |
| `let mut`, annotation | Partial | `mut` and the type annotation are ignored at runtime. Assignment is allowed regardless of `mut`. |
| Destructuring `let` | Partial | A non-binding pattern is silently ignored. It does not bind fields and does not fail. |
| `let ... else` | Partial | The `else` block is ignored by the VM. |
| Assignment statement | Executed | Supports `=`, `+=`, `-=`, and `%=` through a local, one field on a local, or one index on a local. Missing direct locals can be created by `=`. |
| Expression / semicolon | Executed | Evaluates the expression; semicolon does not change the internal value tracked for the block. |
| `return` | Executed | Returns a value or `Unit` from the current named function. |
| `if` / `else if` / `else` | Executed | Condition must be `bool`; no truthiness conversion occurs. |
| `while` | Executed | Supports `break`, `continue`, and early return. |
| `loop` | Executed | Supports `break`, `continue`, early return, and the instruction budget. |
| `for name in value` | Executed | Iterates arrays, JSON arrays, or Unicode scalar values of strings. |
| `break`, `continue` | Executed | Control the nearest evaluated loop. Escaping a loop produces `HUSK-R0006` or `HUSK-R0007`. |
| Block statement | Executed | Uses the same local map as its parent; current blocks do **not** create lexical scopes. |
| `if let` | Runtime gap | Produces `HUSK-R0002` when reached. |

## Expressions

| Construct | Status | Current production behavior |
| --- | --- | --- |
| Bool, integer, float, string literal | Executed | Maps to the corresponding dynamic `Value`. |
| Identifier | Executed | Resolves a local, then a function callback; otherwise becomes a string containing the identifier. |
| Path | Executed | Becomes a `::`-joined string. This is how `red::...` calls reach built-in dispatch. |
| Function call | Executed | Evaluates callee first and arguments left-to-right. Calls a named built-in/function or a callback value. |
| Field read | Executed | Reads dynamic objects and JSON objects. A missing field stays null-like until it is used, preserving the first missing-field span. |
| Array literal | Executed | Produces a copy-on-write `Arc<Vec<Value>>`. |
| Struct literal | Partial | Produces an anonymous object and discards the nominal struct path. |
| Index read | Executed | Arrays and strings use integer indices; objects use string indices. A missing or negative index currently returns `Unit`. Strings are indexed by Unicode scalar value. |
| Unary `!`, `-` | Executed | `!` requires bool. `-` accepts integer or float. |
| Binary arithmetic | Executed | `+`, `-`, `*`, `/`, `%`. String involvement makes `+` concatenate display strings. Integer division is checked; other integer arithmetic currently passes through `f64`. |
| Comparison / equality | Executed | Numeric and string ordering is supported. Equality is structural for ordinary values; `Unit`, `Null`, and missing fields compare equal. |
| `&&`, `||` | Executed | Require bool and short-circuit. |
| Block expression | Executed | Uses the current frame without a lexical child scope. |
| If expression | Executed | Requires an `else` branch at parse time and a bool condition at runtime. |
| Assignment expression | Executed | Has the same restrictions as assignment statements and returns the assigned value. |
| Method call | Runtime gap | Produces an unsupported-expression runtime error. |
| `match` | Runtime gap | Produces an unsupported-expression runtime error. |
| Formatted print / formatted string | Runtime gap | Produces an unsupported-expression runtime error. |
| Closure | Runtime gap | Current callback values are named plugin/function pairs, not closures. |
| Range | Runtime gap | Parsed but not materialized as an iterable value. |
| Embedded JavaScript literal | Runtime gap | Not executed by the production native VM. |
| Cast | Runtime gap | Parsed but not executed. |
| Tuple / tuple field | Runtime gap | Parsed but not represented in `Value`. |
| Try (`?`) | Runtime gap | Parsed but not executed. |

## Dynamic values and evaluation rules

The current boundary values are `Unit`, `Null`, `Bool`, `Int(i64)`,
`Float(f64)`, `String`, copy-on-write `Array`, copy-on-write `Object`, legacy
opaque `Json`, named `Callback`, and an internal `Missing` field sentinel.

Observable rules frozen by tests:

- ordinary operands and function arguments are evaluated left-to-right;
- `&&` and `||` short-circuit;
- conditions require bool;
- arrays and objects use copy-on-write cloning for state and assignment;
- array/object/JSON equality is structural;
- `Unit`, `Null`, and missing fields compare equal and all serialize to JSON
  `null`;
- out-of-range reads return `Unit`, while out-of-range writes fail;
- function locals currently have function scope rather than lexical block
  scope;
- each top-level callback gets a fresh local frame and a shared per-callback
  instruction counter;
- named calls are limited to 512 nested frames.

The intended native-language rules that deliberately improve checked
arithmetic, lexical scope, indexing failures, closure cells, and nominal values
are recorded in
[ADR 0008](adr/0008-husk-value-semantics.md). Compatibility differences must
be isolated in the Red adapter or an explicit semantic profile.

## Red-owned behavior currently inside `husk`

These behaviors are production requirements, but they are not language
semantics and must move behind the Red adapter:

- `activate`, `deactivate`, `before_exit`, `state_export`, and `state_import`
  lifecycle hooks;
- command registration and cross-plugin collision handling;
- event-listener ownership and isolated notification;
- one-shot request IDs and callback routing;
- per-plugin state;
- transactional reload cloning and replacement;
- all `red::...` built-ins, including editor actions, snapshots, conversions,
  collections, strings, colors, and JSON helpers.

The Red runtime additionally owns host-API type declarations, semantic
validation, dependency order, quarantine, capabilities, effect staging, timers,
processes, and editor integration.

## Diagnostic baseline

Parser failures are grouped into a source-aware `Report` with `HUSK-P0001`.
Many evaluator errors are enriched into source-aware `HUSK-R0001` reports, and
missing fields plus escaped loop control have dedicated codes.

Two current gaps are intentional baseline observations:

- a non-bool condition can escape as the plain message
  `Husk condition must evaluate to a bool`;
- some wildcard unsupported-expression paths can escape as a plain
  `unsupported Husk expression in embedded runtime` message.

The extraction should fix those paths without losing source spans, but tests
that describe the old production baseline must not falsely claim the richer
diagnostic already exists.

## Conformance evidence

The focused public-API suite is
`crates/husk/tests/runtime_conformance.rs`. It covers:

- conditionals, all loop forms, loop control, early return, and compound
  assignment;
- arithmetic, comparisons, strict booleans, and short-circuit evaluation;
- current plain-error behavior for non-bool conditions;
- explicit statement and expression runtime gaps.

The existing unit and Red integration suites cover:

- command registration, metadata, and collision ownership;
- event notification and one-shot request callbacks;
- per-plugin state and copy-on-write nested assignment;
- activation, deactivation, state migration, and effect boundaries;
- failed export, teardown, activation, and reload isolation;
- callback/resource cleanup and instruction exhaustion;
- parser and missing-field source diagnostics;
- bundled-plugin typechecking and activation.

Run the focused baseline with:

```shell
cargo test -p husk --test runtime_conformance
```
