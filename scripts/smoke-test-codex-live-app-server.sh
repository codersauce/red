#!/usr/bin/env bash
set -euo pipefail

if [[ "${RED_CODEX_LIVE_SMOKE:-}" != "1" ]]; then
  echo "Codex live app-server smoke skipped. Set RED_CODEX_LIVE_SMOKE=1 to run it."
  exit 0
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CWD="${1:-$ROOT}"
TIMEOUT_SECONDS="${RED_CODEX_LIVE_TIMEOUT:-20}"

python3 - "$CWD" "$TIMEOUT_SECONDS" <<'PY'
import json
import selectors
import subprocess
import sys
import time

cwd = sys.argv[1]
timeout_seconds = float(sys.argv[2])

process = subprocess.Popen(
    ["codex", "app-server", "--listen", "stdio://"],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
    cwd=cwd,
)


def fail(message):
    process.kill()
    _, stderr = process.communicate(timeout=5)
    raise SystemExit(f"{message}\n{stderr}".rstrip())


def send(value):
    if process.stdin is None:
        fail("codex app-server stdin was not available")
    process.stdin.write(json.dumps(value) + "\n")
    process.stdin.flush()


def read_response(request_id):
    if process.stdout is None:
        fail("codex app-server stdout was not available")
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    deadline = time.monotonic() + timeout_seconds
    try:
        while time.monotonic() < deadline:
            if process.poll() is not None:
                fail(f"codex app-server exited with status {process.returncode}")
            events = selector.select(timeout=0.1)
            if not events:
                continue
            line = process.stdout.readline()
            if not line:
                fail("codex app-server closed stdout")
            value = json.loads(line)
            if value.get("id") == request_id:
                if "error" in value:
                    fail(f"codex app-server returned error for id {request_id}: {value['error']}")
                return value.get("result")
    finally:
        selector.close()
    fail(f"timed out waiting for codex app-server response id {request_id}")


try:
    send({
        "method": "initialize",
        "id": 0,
        "params": {
            "clientInfo": {
                "name": "red_live_smoke",
                "title": "Red Live Smoke",
                "version": "0",
            },
        },
    })
    initialize_result = read_response(0)
    if not isinstance(initialize_result, dict):
        fail("codex app-server initialize response was not an object")

    send({"method": "initialized", "params": {}})
    send({
        "method": "thread/list",
        "id": 1,
        "params": {
            "limit": 1,
            "cwd": cwd,
            "sortKey": "updated_at",
            "sortDirection": "desc",
        },
    })
    list_result = read_response(1)
    if not isinstance(list_result, dict):
        fail("codex app-server thread/list response was not an object")
    if "data" in list_result and not isinstance(list_result["data"], list):
        fail("codex app-server thread/list `data` field was not a list")
    print("Codex live app-server smoke passed.")
finally:
    process.kill()
PY
