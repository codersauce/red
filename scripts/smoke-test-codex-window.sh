#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SESSION="red-codex-smoke-$$"
WIDTH="${RED_SMOKE_WIDTH:-100}"
HEIGHT="${RED_SMOKE_HEIGHT:-30}"
STARTUP_WAIT="${RED_SMOKE_STARTUP_WAIT:-10}"
OPEN_WAIT="${RED_SMOKE_OPEN_WAIT:-1}"
READY_TIMEOUT="${RED_SMOKE_READY_TIMEOUT:-30}"

cleanup() {
  tmux kill-session -t "$SESSION" 2>/dev/null || true
}
trap cleanup EXIT

tmux new-session -d -s "$SESSION" "$ROOT/scripts/visual-test-codex.sh"
tmux resize-window -t "$SESSION" -x "$WIDTH" -y "$HEIGHT"

sleep "$STARTUP_WAIT"
deadline=$((SECONDS + READY_TIMEOUT))
until tmux capture-pane -t "$SESSION" -p | rg -q 'NORMAL .*\[No Name\]|NORMAL .*Codex'; do
  if (( SECONDS >= deadline )); then
    tmux capture-pane -t "$SESSION" -p
    echo "Codex window smoke test failed: Red did not finish starting." >&2
    exit 1
  fi
  sleep 0.5
done

tmux send-keys -t "$SESSION" Space c
sleep "$OPEN_WAIT"

output="$(tmux capture-pane -t "$SESSION" -p)"

if ! printf '%s\n' "$output" | rg -q 'Codex Chat Window|Enter send|Ask Codex'; then
  printf '%s\n' "$output"
  echo "Codex window smoke test failed: expected chat window text was not rendered." >&2
  exit 1
fi

echo "Codex window smoke test passed."
