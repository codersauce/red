#!/usr/bin/env python3
"""Minimal local LSP server for edit_replay_bench.py; validates ordered edits."""

import argparse
import hashlib
import json
from pathlib import Path
import sys


def digest(text):
    return hashlib.sha256(text.encode()).hexdigest()


def offset(text, position):
    lines = text.split("\n")
    line, units = position["line"], position["character"]
    prefix = lines[line].encode("utf-16-le")[:units * 2].decode("utf-16-le")
    assert len(prefix.encode("utf-16-le")) == units * 2
    return sum(len(value) + 1 for value in lines[:line]) + len(prefix)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=("full", "incremental"), required=True)
    parser.add_argument("--state", type=Path, required=True)
    args = parser.parse_args()
    state = dict(opened=False, notifications=0, full=0, incremental=0, text_bytes=0)
    document = ""
    version = 0

    def save():
        state["sha256"] = digest(document)
        state["version"] = version
        temporary = args.state.with_suffix(".tmp")
        temporary.write_text(json.dumps(state))
        temporary.replace(args.state)

    def reply(request, result):
        data = json.dumps(dict(jsonrpc="2.0", id=request["id"], result=result)).encode()
        sys.stdout.buffer.write(f"Content-Length: {len(data)}\r\n\r\n".encode() + data)
        sys.stdout.buffer.flush()

    try:
        while True:
            headers = {}
            while line := sys.stdin.buffer.readline():
                if line in (b"\r\n", b"\n"):
                    break
                key, value = line.decode().split(":", 1)
                headers[key.lower()] = value.strip()
            if not line:
                break
            message = json.loads(sys.stdin.buffer.read(int(headers["content-length"])))
            method = message.get("method")
            params = message.get("params", {})
            if method == "initialize":
                reply(message, {"capabilities": {"textDocumentSync": 1 if args.mode == "full" else 2}})
            elif method == "textDocument/didOpen":
                document = params["textDocument"]["text"]
                version = params["textDocument"]["version"]
                state["opened"] = True
                save()
            elif method == "textDocument/didChange":
                assert params["textDocument"]["version"] > version
                version = params["textDocument"]["version"]
                state["notifications"] += 1
                for change in params["contentChanges"]:
                    text = change["text"]
                    state["text_bytes"] += len(text.encode())
                    if change.get("range") is None:
                        document = text
                        state["full"] += 1
                    else:
                        assert args.mode == "incremental"
                        start = offset(document, change["range"]["start"])
                        end = offset(document, change["range"]["end"])
                        assert start <= end
                        document = document[:start] + text + document[end:]
                        state["incremental"] += 1
                save()
            elif method == "exit":
                break
            elif "id" in message:
                reply(message, None)
    except Exception as error:
        state["error"] = repr(error)
        save()
        raise


if __name__ == "__main__":
    main()
