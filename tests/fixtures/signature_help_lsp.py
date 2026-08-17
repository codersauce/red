"""Deterministic LSP peer for signature-help transport and UI tests."""
import json
import pathlib
import re
import sys

events = pathlib.Path(sys.argv[1])
documents = {}


def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        key, value = line.decode().split(":", 1)
        headers[key.lower()] = value.strip()
    return json.loads(sys.stdin.buffer.read(int(headers["content-length"])))


def send(message):
    body = json.dumps(message).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    sys.stdout.buffer.flush()


def signature(params):
    text = documents.get(params["textDocument"]["uri"], "")
    position = params["position"]
    lines = text.splitlines(keepends=True)
    line = lines[position["line"]] if position["line"] < len(lines) else ""
    prefix = "".join(lines[:position["line"]]) + line.encode("utf-16-le")[:position["character"] * 2].decode("utf-16-le")
    stack = []
    for match in re.finditer(r"([\w:]+)\s*\(|[(),]", prefix):
        token = match.group()
        if token.endswith("("):
            stack.append([match.group(1) or "", 0])
        elif token == ")" and stack:
            stack.pop()
        elif token == "," and stack:
            stack[-1][1] += 1
    if not stack:
        return None
    name, active = stack[-1]
    names = ["value: f32"] if name == "inner" else ["x: f32", "y: f32"]
    label = "fn " + name + "(" + ", ".join(names) + ") -> f32"
    parameters = []
    for parameter in names:
        start = len(label[:label.index(parameter)].encode("utf-16-le")) // 2
        parameters.append({"label": [start, start + len(parameter)], "documentation": "Current " + parameter})
    result = {"label": label, "parameters": parameters, "activeParameter": min(active, len(names) - 1)}
    return {"activeSignature": 0, "signatures": [result]}


while True:
    message = read_message()
    if message is None:
        break
    with events.open("a") as output:
        output.write(json.dumps(message) + "\n")
    method = message.get("method")
    params = message.get("params", {})
    if method == "initialize":
        result = {"capabilities": {"textDocumentSync": 1, "signatureHelpProvider": {"triggerCharacters": ["(", ","], "retriggerCharacters": [")"]}}}
    elif method == "textDocument/didOpen":
        documents[params["textDocument"]["uri"]] = params["textDocument"]["text"]
        continue
    elif method == "textDocument/didChange":
        documents[params["textDocument"]["uri"]] = params["contentChanges"][-1]["text"]
        continue
    elif method == "textDocument/signatureHelp":
        result = signature(params)
    elif method == "textDocument/diagnostic":
        result = {"kind": "full", "items": []}
    elif method == "shutdown":
        result = None
    elif method == "exit":
        break
    elif "id" in message:
        result = None
    else:
        continue
    send({"jsonrpc": "2.0", "id": message["id"], "result": result})
