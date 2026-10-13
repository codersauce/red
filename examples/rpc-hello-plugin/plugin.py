#!/usr/bin/env python3
import json
import sys


def send(message):
    print(json.dumps(message, separators=(",", ":")), flush=True)


for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    if method == "initialize":
        send(
            {
                "jsonrpc": "2.0",
                "id": message["id"],
                "result": {
                    "protocol_version": 1,
                    "commands": [
                        {
                            "name": "RpcHello",
                            "metadata": {
                                "title": "Hello from RPC",
                                "arguments": True,
                            },
                        }
                    ],
                },
            }
        )
    elif method == "command/execute":
        name = message["params"]["raw_args"] or "Red"
        send(
            {
                "jsonrpc": "2.0",
                "id": message["id"],
                "result": {"message": f"Hello, {name}, from Python RPC!"},
            }
        )
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": message["id"], "result": None})
    elif method == "exit":
        break
