# ADR 0006: Husk semantic profiles

- Status: accepted
- Date: 2026-07-19
- Scope: native language semantics and legacy frontend compatibility

## Decision

Compilation always selects a semantic profile:

- `Native` is the default for the embedding API and standalone CLI. Its
  prelude and runtime are backend-neutral.
- `LegacyJavaScript` exists only for compatibility with older frontend and
  semantic tests that depend on `JsValue`, `extern "js"`, JavaScript globals,
  or `js { ... }`.

Native Husk does not silently enable JavaScript semantics. A JavaScript-only
construct in the native profile produces a specific compile diagnostic. Native
and CLI execution use the same value and evaluation rules.

Red compatibility is provided by registered `red` declarations and an adapter,
not by treating Red as a third language backend. Temporary Red-only
compatibility switches must be explicit and removable.

See the
[native language profile section](../HUSK_LANGUAGE_EXTRACTION_PLAN.md#native-language-profile-and-standard-library)
of the extraction plan.

## Consequences

- The general runtime no longer carries a hidden JavaScript assumption.
- Existing JS-oriented compiler work can migrate incrementally.
- Tests state which language contract they exercise.
- New standard-library APIs are native modules or HIR primitives, not
  JavaScript globals.

## Revisit when

Revisit if the legacy frontend is removed completely, another backend requires
meaningfully different source semantics, or a compatibility corpus proves a
specific behavior cannot be isolated behind declarations and adapters.
