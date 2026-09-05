---
title: "Release A Language Pack"
summary: "Release a first-party Red language pack by validating the shared source repo, tagging one pack version, and verifying the catalog entry Red consumes."
topics: [guides, plugins, release, syntax, validation]
sources:
  - id: red-catalog
    type: file
    path: src/plugin/catalog.rs
  - id: red-package
    type: file
    path: src/plugin/package.rs
  - id: packs-readme
    type: web
    url: https://github.com/codersauce/red-language-packs/blob/742394dacf2c6d8bd448c62e6e1006bf4aab48ea/README.md
  - id: packs-contributing
    type: web
    url: https://github.com/codersauce/red-language-packs/blob/742394dacf2c6d8bd448c62e6e1006bf4aab48ea/CONTRIBUTING.md
  - id: validate-workflow
    type: web
    url: https://github.com/codersauce/red-language-packs/blob/742394dacf2c6d8bd448c62e6e1006bf4aab48ea/.github/workflows/validate.yml
  - id: release-workflow
    type: web
    url: https://github.com/codersauce/red-language-packs/blob/742394dacf2c6d8bd448c62e6e1006bf4aab48ea/.github/workflows/release-pack.yml
  - id: package-release
    type: web
    url: https://github.com/codersauce/red-language-packs/blob/742394dacf2c6d8bd448c62e6e1006bf4aab48ea/scripts/package_release.py
  - id: merge-catalog
    type: web
    url: https://github.com/codersauce/red-language-packs/blob/742394dacf2c6d8bd448c62e6e1006bf4aab48ea/scripts/merge_catalog.py
---

Use this guide when publishing one first-party Red language pack from the
`codersauce/red-language-packs` source repository. That repository is shared
authoring infrastructure, but every directory under `packs/` remains its own
Red package with an independent manifest, version, release artifact, install
record, and native-grammar approval boundary [@packs-readme]
[@packs-contributing]. The Red editor consumes the published catalog at
`https://github.com/codersauce/red-language-packs/releases/download/catalog-v1/v1.json`
unless `RED_PLUGIN_CATALOG_URL` overrides it [@red-catalog].

## Validate The Source Change

Before tagging, run the language-pack repository checks that match the change.
The validation workflow first checks Arborium policy with unit tests,
`scripts/arborium.py inventory --check`, and
`scripts/arborium.py sync --check`; then it validates each reviewed pack from
the generated pack list and builds all grammars on Linux, macOS Apple Silicon,
macOS Intel, and Windows [@validate-workflow].

For a focused local pass in the language-pack repository, use the commands from
the contributor guide:

```shell
python3 scripts/arborium.py inventory --check
python3 scripts/arborium.py sync --check
PYTHONPATH=scripts python3 -m unittest discover -s scripts/tests
python3.13 scripts/validate_pack.py packs/<pack>
python3.13 scripts/build_grammar.py <pack>
python3.13 scripts/validate_pack.py packs/<pack>
```

Use the actual Red binary that will support the pack when checking indentation.
The indentation helper runs with disposable configuration and grammar-trust
stores, so it does not alter the developer's installed packs or native grammar
trust decisions [@packs-contributing]. If shell resolution is ambiguous, use
[Build, Test, And Validate](../development/build-test-and-validate) before
changing manifests.

```shell
python3 scripts/check_indents.py packs/<pack> --red /path/to/red
```

## Tag One Pack

After the version change is merged to the language-pack repository's main
branch, push exactly one annotated tag for the pack being released:

```shell
git checkout main
git pull --ff-only origin main
git tag -a <pack>/v<manifest-version> -m "Release <pack> v<manifest-version>"
git push origin <pack>/v<manifest-version>
```

The release workflow extracts the pack slug from the tag prefix, verifies
`arborium/languages/<pack>.toml` exists, and requires the tag to match the
manifest version before packaging [@release-workflow] [@package-release].
Pushing unrelated pack tags in the same operation makes the catalog queue harder
to audit; release one pack at a time unless the release plan explicitly needs a
batch.

## Watch Packaging And Catalog Publication

The release workflow builds one bundle per supported host target:
`x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`,
`x86_64-apple-darwin`, and `x86_64-pc-windows-msvc` [@release-workflow].
Each runner installs Tree-sitter CLI `0.25.10`, builds the pinned grammar,
and runs `scripts/package_release.py` with the tag, target, and exact commit
SHA [@release-workflow].

`package_release.py` writes deterministic tarballs and per-target catalog
metadata. The metadata records package id, version, Red API range, repository,
source path, resolved commit, license, tier, language ids, optional external
requirements, artifact URL, artifact SHA-256, size, and native grammar digests
[@package-release]. The workflow then creates the immutable pack release and
updates the stable `catalog-v1` asset by merging the new target metadata into
the previous catalog or the checked-in base catalog [@release-workflow]
[@merge-catalog].

The workflow serializes catalog updates with the `language-pack-catalog`
concurrency group and `cancel-in-progress: false`, so wait for the catalog job
from one pack release before treating the next catalog state as final
[@release-workflow].

## Verify Red Can Consume It

After the workflow completes, verify both the language-pack release and the
catalog Red will fetch:

```shell
gh release view <pack>/v<manifest-version> --repo codersauce/red-language-packs
gh release download catalog-v1 --repo codersauce/red-language-packs --pattern v1.json --dir /tmp/red-language-catalog
cargo run --locked -- plugin catalog
cargo run --locked -- plugin install --catalog <package-id>
```

Red parses catalog schema version 1, limits the catalog payload to 1 MiB,
requires HTTPS catalog and artifact URLs, validates host-target artifacts, and
checks the pack's Red API range against the supported host API set before
installing [@red-catalog] [@red-package]. A successful catalog install still
does not grant native grammar trust by itself: catalog checks prove release
artifact integrity, while loading native grammar bytes remains an explicit
digest-bound user decision [@red-catalog] [@red-package].

For the public command surface behind catalog listing and installation, read
[Red Command](../../reference/cli/red-command). For the architecture and policy
behind these boundaries, read
[Official Language Pack Distribution](../../decisions/plugins/language-pack-distribution).
