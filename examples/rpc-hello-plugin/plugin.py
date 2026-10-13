#!/usr/bin/env python3
import json
import sys


def send(message):
    print(json.dumps(message, separators=(",", ":")), flush=True)


def request_host(method, params):
    request_id = "plugin-1"
    send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
    response = json.loads(sys.stdin.readline())
    if response.get("id") != request_id:
        raise RuntimeError("unexpected host response")
    if "error" in response:
        raise RuntimeError(response["error"]["message"])
    return response["result"]


last_transaction_id = None
last_event = None

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
                        },
                        {
                            "name": "RpcDocument",
                            "metadata": {"title": "Show current RPC document"},
                        },
                        {
                            "name": "RpcApply",
                            "metadata": {"title": "Apply an RPC document edit"},
                        },
                        {
                            "name": "RpcLastEvent",
                            "metadata": {"title": "Show the last RPC editor event"},
                        },
                        {
                            "name": "RpcUndo",
                            "metadata": {"title": "Undo the last RPC edit"},
                        },
                    ],
                    "events": ["file:saved"],
                },
            }
        )
    elif method == "event/notify":
        last_event = message["params"]
    elif method == "command/execute":
        try:
            command = message["params"]["name"]
            if command == "RpcDocument":
                document = request_host("red/DocumentSnapshot", {})
                path = document["path"] or "[scratch]"
                result = {
                    "message": (
                        f"Current document: {path} @ revision {document['revision']} "
                        f"({len(document['text'])} chars)"
                    )
                }
            elif command == "RpcApply":
                document = request_host("red/DocumentSnapshot", {})
                applied = request_host(
                    "red/DocumentApply",
                    {
                        "expected_revision": document["revision"],
                        "label": "RPC example edit",
                        "edits": [
                            {
                                "range": {
                                    "start": {"line": 0, "character": 0},
                                    "end": {"line": 0, "character": 0},
                                },
                                "text": "# Edited by Python RPC\n",
                                "expected_text": "",
                            }
                        ],
                    },
                )
                last_transaction_id = applied["transaction_id"]
                result = {"message": f"RPC edit applied @ {applied['revision']}"}
            elif command == "RpcUndo":
                if last_transaction_id is None:
                    raise RuntimeError("RpcApply has no transaction to undo")
                undone = request_host(
                    "red/DocumentUndo", {"transaction_id": last_transaction_id}
                )
                last_transaction_id = None
                result = {"message": f"RPC edit undone @ {undone['revision']}"}
            elif command == "RpcLastEvent":
                if last_event is None:
                    raise RuntimeError("no RPC editor event has been received")
                event_file = last_event["payload"].get("file", "[scratch]")
                result = {
                    "message": f"Last RPC event: {last_event['name']} ({event_file})"
                }
            else:
                name = message["params"]["raw_args"] or "Red"
                result = {"message": f"Hello, {name}, from Python RPC!"}
            send({"jsonrpc": "2.0", "id": message["id"], "result": result})
        except RuntimeError as error:
            send(
                {
                    "jsonrpc": "2.0",
                    "id": message["id"],
                    "error": {"code": -32000, "message": str(error)},
                }
            )
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": message["id"], "result": None})
    elif method == "exit":
        break
