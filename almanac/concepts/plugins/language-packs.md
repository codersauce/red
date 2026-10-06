---
title: "Language Packs"
summary: "Language packs are Red plugin packages whose durable boundary crosses runtime syntax behavior, catalog provenance, CLI installation, native grammar trust, validation, and release work."
topics: [concepts, plugins, language-packs, syntax, safety]
sources:
  - id: catalog
    type: file
    path: src/plugin/catalog.rs
  - id: package
    type: file
    path: src/plugin/package.rs
  - id: language
    type: file
    path: src/language.rs
  - id: syntax-services
    type: file
    path: almanac/architecture/editor/syntax-services.md
  - id: distribution
    type: file
    path: almanac/decisions/plugins/language-pack-distribution.md
  - id: release-guide
    type: file
    path: almanac/guides/plugins/release-language-pack.md
  - id: validation-guide
    type: file
    path: almanac/guides/development/build-test-and-validate.md
  - id: red-command
    type: file
    path: almanac/reference/cli/red-command.md
---

# Language Packs

Language packs are Red plugin packages whose main effect is to add language
definitions rather than editor UI commands. The package manifest accepts a
language-only package, validates the Red host API requirement, checks package
paths, and enforces version floors for features such as indentation queries
[@package]. Catalog entries are discovery metadata for independently versioned
packages and per-target release artifacts; the catalog records provenance and
checksums, but it does not approve native grammar execution [@catalog].

Runtime language configuration remains local and explicit. Enabled compatible
package languages merge into the effective configuration only when the user has
not already defined the same language id, package grammar paths are rooted under
the installed package, and package-provided grammar trust is cleared before the
language enters the registry [@language]. Native grammar approval is a separate
digest-bound trust store: Red approves the current canonical grammar bytes,
stages approved bytes in an immutable cache, and rejects replaced bytes until
the user approves them again [@language].

## Reading Order

Start with [Syntax Services](../../architecture/editor/syntax-services) when the
question is what an installed language can change at runtime: language
selection, highlighting, injections, structural text objects, indentation
queries, or package-provided query fallback [@syntax-services]. Use
[Official Language Pack Distribution](../../decisions/plugins/language-pack-distribution)
when the work touches package identity, catalog provenance, per-pack lifecycle,
native grammar consent, or why one shared language-pack repository is not one
runtime package [@distribution].

Use [Red Command](../../reference/cli/red-command) for the exact command surface:
`red plugin catalog`, `red plugin install --catalog`, native grammar trust and
untrust, and `red language check-indent` [@red-command]. Use
[Build, Test, And Validate](../../guides/development/build-test-and-validate)
when an installed pack appears incompatible with a freshly built Red binary; the
binary-skew section explains why an `unknown field` error can point at the
wrong `red` executable instead of a bad package manifest [@validation-guide].

Use [Release A Language Pack](../../guides/plugins/release-language-pack) when
the source repository change is ready to become one published pack tag, target
bundle, catalog update, and Red-side install verification [@release-guide].

## Boundaries To Preserve

Do not treat catalog trust, package checksums, and native grammar trust as the
same decision. Catalog metadata can prove which artifact Red downloaded and
which digest it expected, while `GrammarTrustStore` controls whether executable
grammar bytes may be loaded into the editor process [@catalog] [@language].

Do not assume a language pack owns the language server lifecycle. Package
language definitions can contribute LSP settings that Red folds into the
effective configuration, and catalog requirements can describe external
commands, but server binaries, installation, licensing, and updates remain
outside the grammar package artifact boundary [@language] [@distribution].

Do not debug package compatibility before proving the binary under test.
`red --version` can be identical across stale and current local builds, so
language-pack failures should first verify the exact executable path and then
use the host API compatibility reference only after the running binary is known
[@validation-guide].
