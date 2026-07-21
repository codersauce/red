#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
temporary_root=$(mktemp -d "${TMPDIR:-/tmp}/husk-standalone.XXXXXX")
trap 'rm -rf "$temporary_root"' EXIT HUP INT TERM

mkdir -p "$temporary_root/crates"
cp "$repository_root/tools/husk-standalone/Cargo.toml" "$temporary_root/Cargo.toml"
cp -R "$repository_root/tools/husk-standalone/smoke" "$temporary_root/smoke"

for crate in \
    husk \
    husk-ast \
    husk-cli \
    husk-diagnostics \
    husk-extension \
    husk-hir \
    husk-lexer \
    husk-package \
    husk-parser \
    husk-runtime \
    husk-semantic \
    husk-types \
    husk-value \
    husk-wasm
do
    cp -R "$repository_root/crates/$crate" "$temporary_root/crates/$crate"
done

CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repository_root/target/husk-standalone}" \
    cargo check --manifest-path "$temporary_root/Cargo.toml" --workspace --all-features

CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repository_root/target/husk-standalone}" \
    cargo run --quiet --manifest-path "$temporary_root/Cargo.toml" \
    --package husk-standalone-smoke
