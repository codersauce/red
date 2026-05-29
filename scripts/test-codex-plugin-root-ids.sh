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

(async () => {
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

const userInputActions = codex.__testInteractiveRequestActionLines("item/tool/requestUserInput");
if (!userInputActions.some((line) => line.includes("composer") && line.includes("Enter"))) {
  throw new Error("user-input requests must advertise composer submission");
}

const commandActions = codex.__testInteractiveRequestActionLines(
  "item/commandExecution/requestApproval",
  { availableDecisions: ["accept", "acceptForSession", "decline"] },
);
if (!commandActions.some((line) => line.includes("codex.approveRequestForSession"))) {
  throw new Error("session-capable requests must advertise session approval");
}
if (!commandActions.some((line) => line.includes("codex.declineRequest"))) {
  throw new Error("approval requests must advertise decline");
}

if (codex.__testPromptSubmitBlockedReason("disconnected") !== "disconnected") {
  throw new Error("disconnected chats must block prompt submission until reconnect");
}
if (codex.__testPromptSubmitBlockedReason("ready") !== undefined) {
  throw new Error("ready chats must not block prompt submission");
}
if (!codex.__testDisconnectedActionHint().includes("codex.reconnect")) {
  throw new Error("disconnected chats must advertise the reconnect command");
}

const wordMotion = codex.__testComposerWordMotion(
  ["ask codex", "about vim_motions"],
  { line: 0, column: 0 },
);
if (wordMotion.next.line !== 0 || wordMotion.next.column !== 4) {
  throw new Error("composer next-word motion should jump to the next word start");
}

const previousWordMotion = codex.__testComposerWordMotion(
  ["ask codex", "about vim_motions"],
  { line: 1, column: 7 },
);
if (previousWordMotion.previous.line !== 1 || previousWordMotion.previous.column !== 6) {
  throw new Error("composer previous-word motion should jump within the current line");
}

const crossLineMotion = codex.__testComposerWordMotion(
  ["ask codex", "about vim_motions"],
  { line: 1, column: 0 },
);
if (crossLineMotion.previous.line !== 0 || crossLineMotion.previous.column !== 4) {
  throw new Error("composer previous-word motion should cross line boundaries");
}

const registeredCommands = [];
await codex.activate({
  addCommand: (name) => registeredCommands.push(name),
  on: () => {},
  onPluginWindowEvent: () => {},
});
for (const name of [
  "codex.attachSelection",
  "codex.context.selection",
]) {
  if (!registeredCommands.includes(name)) {
    throw new Error(\`Codex command \${name} was not registered\`);
  }
}
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
NODE

echo "Codex plugin helper test passed."
