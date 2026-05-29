#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="$(mktemp -d)"
trap 'rm -rf "$OUT_DIR"' EXIT

npx -y -p typescript tsc \
  --target ES2021 \
  --module commonjs \
  --lib ES2021 \
  --skipLibCheck \
  --outDir "$OUT_DIR" \
  "$ROOT/plugins/codex.ts"

node <<NODE
const codex = require("$OUT_DIR/codex.js");

const first = codex.__testRootScopedCodexIds("/tmp/project-one");
const firstWithSlash = codex.__testRootScopedCodexIds("/tmp/project-one/");
const second = codex.__testRootScopedCodexIds("/tmp/project-two");

if (first.windowId !== firstWithSlash.windowId) {
  throw new Error("workspace root window id must be stable after path normalization");
}
if (first.storageKey !== firstWithSlash.storageKey) {
  throw new Error("workspace root storage key must be stable after path normalization");
}
if (first.windowId === second.windowId) {
  throw new Error("distinct workspace roots must use distinct Codex window ids");
}
if (first.storageKey === second.storageKey) {
  throw new Error("distinct workspace roots must use distinct Codex storage keys");
}
if (!first.windowId.startsWith("chat-")) {
  throw new Error("root-scoped Codex window ids must keep the chat prefix");
}
if (!first.storageKey.startsWith("codex.chat.")) {
  throw new Error("root-scoped Codex storage keys must keep the codex.chat prefix");
}
NODE

echo "Codex plugin root id test passed."
