#!/usr/bin/env python3
import itertools
import json
import sys


def send(message):
    print(json.dumps(message, separators=(",", ":")), flush=True)


def send_error(message, detail):
    if "id" in message:
        send({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32000, "message": detail}})


def read_messages():
    """Service notifications and lifecycle messages during every receive loop."""
    global last_event
    for line in sys.stdin:
        message = json.loads(line)
        method = message.get("method")
        if method == "event/notify":
            last_event = message["params"]
        elif method == "shutdown":
            send({"jsonrpc": "2.0", "id": message["id"], "result": None})
        elif method == "exit":
            sys.exit(0)
        else:
            yield message


def request_host(method, params):
    request_id = f"plugin-{next(request_ids)}"
    send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
    for response in incoming:
        if "method" in response:
            send_error(response, "plugin is busy waiting for a host response")
            continue
        if response.get("id") != request_id:
            raise RuntimeError("unexpected host response")
        if "error" in response:
            raise RuntimeError(response["error"]["message"])
        return response["result"]
    raise RuntimeError("host closed stdin while awaiting response")


last_transaction_id = None
last_document_id = None
last_event = None
request_ids = itertools.count(1)
incoming = read_messages()

for message in incoming:
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
                            "name": "RpcPanel",
                            "metadata": {"title": "Open an RPC text panel"},
                        },
                        {
                            "name": "RpcPanelClose",
                            "metadata": {"title": "Close the RPC text panel"},
                        },
                        {
                            "name": "RpcStatusline",
                            "metadata": {"title": "Set the RPC statusline"},
                        },
                        {
                            "name": "RpcStatuslineClear",
                            "metadata": {"title": "Clear the RPC statusline"},
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
                document = request_host("red/DocumentSnapshot", {"include_document_id": True})
                applied = request_host(
                    "red/DocumentApply",
                    {
                        "document_id": document["document_id"],
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
                last_document_id = document["document_id"]
                result = {"message": f"RPC edit applied @ {applied['revision']}"}
            elif command == "RpcUndo":
                if last_transaction_id is None:
                    raise RuntimeError("RpcApply has no transaction to undo")
                undone = request_host(
                    "red/DocumentUndo",
                    {"document_id": last_document_id, "transaction_id": last_transaction_id},
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
            elif command == "RpcPanel":
                request_host(
                    "red/TextPanelCreate",
                    {
                        "id": "python-preview",
                        "side": "right",
                        "width": 36,
                        "title": "Python RPC",
                    },
                )
                request_host(
                    "red/TextPanelSetContent",
                    {
                        "id": "python-preview",
                        "blocks": [
                            {
                                "id": "welcome",
                                "kind": "agent",
                                "format": "markdown",
                                "text": "# Language-agnostic panel\n\nRendered by Red from a **Python** JSON-RPC plugin.",
                            }
                        ],
                    },
                )
                request_host("red/TextPanelFocus", {"id": "python-preview"})
                result = None
            elif command == "RpcPanelClose":
                request_host("red/TextPanelDispose", {"id": "python-preview"})
                result = None
            elif command == "RpcStatusline":
                request_host(
                    "red/StatuslineSet",
                    {"text": "Python RPC ready", "side": "right"},
                )
                result = None
            elif command == "RpcStatuslineClear":
                request_host("red/StatuslineClear", {})
                result = None
            else:
                name = message["params"]["raw_args"] or "Red"
                result = {"message": f"Hello, {name}, from Python RPC!"}
            send({"jsonrpc": "2.0", "id": message["id"], "result": result})
        except RuntimeError as error:
            send_error(message, str(error))
