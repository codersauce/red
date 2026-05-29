#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SESSION="red-codex-turn-smoke-$$"
VISUAL_HOME="$(mktemp -d)"
PORT_FILE="$(mktemp)"
SERVER_LOG="$(mktemp)"
WIDTH="${RED_SMOKE_WIDTH:-100}"
HEIGHT="${RED_SMOKE_HEIGHT:-30}"
READY_TIMEOUT="${RED_SMOKE_READY_TIMEOUT:-30}"
TURN_TIMEOUT="${RED_SMOKE_TURN_TIMEOUT:-30}"

cleanup() {
  tmux kill-session -t "$SESSION" 2>/dev/null || true
  if [[ -n "${SERVER_PID:-}" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf "$VISUAL_HOME"
  rm -f "$PORT_FILE" "$SERVER_LOG"
}
trap cleanup EXIT

python3 "$ROOT/scripts/fake-codex-app-server.py" --port-file "$PORT_FILE" >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!

deadline=$((SECONDS + READY_TIMEOUT))
until [[ -s "$PORT_FILE" ]]; do
  if (( SECONDS >= deadline )); then
    cat "$SERVER_LOG" >&2 || true
    echo "Codex turn smoke test failed: fake app-server did not start." >&2
    exit 1
  fi
  sleep 0.1
done

endpoint="ws://127.0.0.1:$(cat "$PORT_FILE")"
tmux new-session -d -s "$SESSION" "RED_VISUAL_HOME='$VISUAL_HOME' RED_CODEX_APP_SERVER_ENDPOINT='$endpoint' '$ROOT/scripts/visual-test-codex.sh'"
tmux resize-window -t "$SESSION" -x "$WIDTH" -y "$HEIGHT"

deadline=$((SECONDS + READY_TIMEOUT))
until tmux capture-pane -t "$SESSION" -p | rg -q 'NORMAL .*\[No Name\]|NORMAL .*Codex'; do
  if (( SECONDS >= deadline )); then
    tmux capture-pane -t "$SESSION" -p
    cat "$SERVER_LOG" >&2 || true
    echo "Codex turn smoke test failed: Red did not finish starting." >&2
    exit 1
  fi
  sleep 0.5
done

tmux send-keys -t "$SESSION" Space c
sleep 1
tmux send-keys -t "$SESSION" "hello from smoke" Enter

deadline=$((SECONDS + TURN_TIMEOUT))
until tmux capture-pane -t "$SESSION" -p | rg -q 'fake streamed response'; do
  if (( SECONDS >= deadline )); then
    tmux capture-pane -t "$SESSION" -p
    cat "$SERVER_LOG" >&2 || true
    echo "Codex turn smoke test failed: streamed response was not rendered." >&2
    exit 1
  fi
  sleep 0.5
done

echo "Codex turn smoke test passed."
