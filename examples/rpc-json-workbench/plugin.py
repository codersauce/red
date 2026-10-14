#!/usr/bin/env python3
"""Bounded jq workbench for Red's JSON-RPC plugin host."""

import itertools
import json
import queue
import subprocess
import sys
import threading
import time


FRAME_LIMIT = 8 * 1024 * 1024
SOURCE_LIMIT = 1024 * 1024
RESULT_LIMIT = 1024 * 1024
DIAGNOSTIC_LIMIT = 64 * 1024
QUERY_LIMIT = 16 * 1024
JQ_TIMEOUT_SECONDS = 5
PANEL_ID = "json-workbench"
EOF = object()

COMMANDS = [
    {
        "name": "JsonQuery",
        "metadata": {
            "title": "Preview a jq query",
            "category": "JSON Workbench",
            "arguments": True,
        },
    },
    {
        "name": "JsonApply",
        "metadata": {
            "title": "Apply the JSON preview",
            "category": "JSON Workbench",
        },
    },
    {
        "name": "JsonUndo",
        "metadata": {
            "title": "Undo the JSON workbench edit",
            "category": "JSON Workbench",
        },
    },
    {
        "name": "JsonClose",
        "metadata": {
            "title": "Close the JSON workbench",
            "category": "JSON Workbench",
        },
    },
]


class OperationError(RuntimeError):
    pass


class HostError(OperationError):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


class CommandCancelled(Exception):
    pass


def end_position(text):
    """Return Red's zero-based Unicode-scalar position at the end of text."""
    line = text.count("\n")
    return {"line": line, "character": len(text.rsplit("\n", 1)[-1])}


def collect_stream(stream, limit, name, collected, overflow):
    data = bytearray()
    try:
        while len(data) <= limit:
            chunk = stream.read(min(64 * 1024, limit + 1 - len(data)))
            if not chunk:
                break
            data.extend(chunk)
        if len(data) > limit:
            overflow.put(name)
        collected[name] = bytes(data[:limit])
    finally:
        stream.close()


def write_input(stream, data):
    try:
        stream.write(data)
    except BrokenPipeError:
        pass
    finally:
        stream.close()


def stop_process(process):
    if process.poll() is not None:
        return
    try:
        process.terminate()
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=0.2)
    except subprocess.TimeoutExpired:
        try:
            process.kill()
        except ProcessLookupError:
            pass
        process.wait()


def overflow_failure(overflow):
    try:
        name = overflow.get_nowait()
    except queue.Empty:
        return None
    limit = RESULT_LIMIT if name == "output" else DIAGNOSTIC_LIMIT
    return OperationError(f"jq {name} exceeds {limit} bytes")


def decode_result(process, collected):
    output = collected["output"]
    diagnostics = collected["diagnostics"]
    if process.returncode:
        detail = diagnostics.decode("utf-8", errors="replace").strip()
        raise OperationError(detail or f"jq exited with status {process.returncode}")
    try:
        return output.decode("utf-8")
    except UnicodeDecodeError as error:
        raise OperationError("jq returned non-UTF-8 output") from error


class JsonWorkbench:
    def __init__(self, jq_command, jq_timeout=JQ_TIMEOUT_SECONDS):
        self.jq_command = jq_command
        self.jq_timeout = jq_timeout
        self.incoming = queue.Queue()
        self.pending = {}
        self.pending_lock = threading.Lock()
        self.write_lock = threading.Lock()
        self.command_active = threading.Event()
        self.cancelling = threading.Event()
        self.closed = threading.Event()
        self.request_ids = itertools.count(1)
        self.source_id = None
        self.candidate = None
        self.transaction = None
        self.panel_open = False

    def send(self, message):
        frame = json.dumps(
            message, ensure_ascii=False, separators=(",", ":")
        ).encode("utf-8")
        frame += b"\n"
        if len(frame) > FRAME_LIMIT:
            raise OperationError("outbound JSON-RPC frame exceeds 8 MiB")
        with self.write_lock:
            sys.stdout.buffer.write(frame)
            sys.stdout.buffer.flush()

    def send_error(self, message, detail):
        if "id" in message:
            self.send(
                {
                    "jsonrpc": "2.0",
                    "id": message["id"],
                    "error": {"code": -32000, "message": str(detail)},
                }
            )

    def close_protocol(self, error):
        self.closed.set()
        with self.pending_lock:
            pending = list(self.pending.values())
        for response in pending:
            response.put(error)
        self.incoming.put(EOF)

    def read_messages(self):
        try:
            for frame in sys.stdin.buffer:
                if len(frame) > FRAME_LIMIT:
                    raise OperationError("inbound JSON-RPC frame exceeds 8 MiB")
                if not self.route_message(json.loads(frame)):
                    return
        except (json.JSONDecodeError, OperationError) as error:
            self.close_protocol(error)
            return
        self.close_protocol(CommandCancelled())

    def route_message(self, message):
        method = message.get("method")
        if method is None:
            with self.pending_lock:
                response = self.pending.get(str(message.get("id")))
            if response is None:
                raise OperationError("received an unexpected host response")
            response.put(message)
        elif method == "shutdown":
            self.send({"jsonrpc": "2.0", "id": message["id"], "result": None})
            self.cancelling.set()
            with self.pending_lock:
                pending = list(self.pending.values())
            for response in pending:
                response.put(CommandCancelled())
        elif method == "exit":
            self.close_protocol(CommandCancelled())
            return False
        elif method == "command/execute" and self.command_active.is_set():
            self.send_error(message, "JSON Workbench is busy")
        elif "id" in message:
            self.incoming.put(message)
        return True

    def request_host(self, method, params):
        if self.cancelling.is_set() or self.closed.is_set():
            raise CommandCancelled()
        request_id = f"plugin-{next(self.request_ids)}"
        response = queue.Queue()
        with self.pending_lock:
            self.pending[request_id] = response
        try:
            self.send(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "method": method,
                    "params": params,
                }
            )
            message = response.get()
            if isinstance(message, Exception):
                raise message
            if "error" in message:
                error = message["error"]
                raise HostError(error["code"], error["message"])
            return message["result"]
        finally:
            with self.pending_lock:
                self.pending.pop(request_id, None)

    def set_status(self, text):
        self.request_host("red/StatuslineSet", {"text": text, "side": "right"})

    def best_effort_status(self, text):
        if self.cancelling.is_set() or self.closed.is_set():
            return
        try:
            self.set_status(text)
        except (OperationError, CommandCancelled):
            pass

    def retire_preview(self, error):
        if not self.panel_open:
            return
        detail = str(error).encode("utf-8")[:DIAGNOSTIC_LIMIT]
        try:
            self.request_host(
                "red/TextPanelSetContent",
                {
                    "id": PANEL_ID,
                    "blocks": [
                        {
                            "id": "error",
                            "kind": "text",
                            "format": "plain",
                            "text": "Error: " + detail.decode("utf-8", errors="ignore"),
                        }
                    ],
                },
            )
        except (OperationError, CommandCancelled):
            pass

    def snapshot_source(self):
        if self.source_id is None:
            document = self.request_host(
                "red/DocumentSnapshot", {"include_document_id": True}
            )
            self.source_id = document["document_id"]
            return document
        return self.request_host(
            "red/DocumentSnapshot", {"document_id": self.source_id}
        )

    def wait_for_jq(self, process, overflow):
        deadline = time.monotonic() + self.jq_timeout
        while process.poll() is None:
            if self.cancelling.is_set() or self.closed.is_set():
                return CommandCancelled()
            failure = overflow_failure(overflow)
            if failure is not None:
                return failure
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return OperationError(
                    f"jq timed out after {self.jq_timeout:g} seconds"
                )
            try:
                process.wait(timeout=min(0.02, remaining))
            except subprocess.TimeoutExpired:
                pass
        return None

    def run_jq(self, query_text, source):
        try:
            process = subprocess.Popen(
                [*self.jq_command, "--", query_text],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
        except OSError as error:
            raise OperationError(f"jq is unavailable: {error}") from error

        collected = {}
        overflow = queue.Queue()
        workers = [
            threading.Thread(
                target=write_input, args=(process.stdin, source), daemon=True
            ),
            threading.Thread(
                target=collect_stream,
                args=(process.stdout, RESULT_LIMIT, "output", collected, overflow),
                daemon=True,
            ),
            threading.Thread(
                target=collect_stream,
                args=(
                    process.stderr,
                    DIAGNOSTIC_LIMIT,
                    "diagnostics",
                    collected,
                    overflow,
                ),
                daemon=True,
            ),
        ]
        for worker in workers:
            worker.start()

        failure = self.wait_for_jq(process, overflow)
        if failure is not None:
            stop_process(process)
        else:
            process.wait()
        for worker in workers:
            worker.join()
        if failure is None:
            failure = overflow_failure(overflow)
        if failure is not None:
            raise failure

        return decode_result(process, collected)

    def show_result(self, document, query_text, output):
        if not self.panel_open:
            self.request_host(
                "red/TextPanelCreate",
                {
                    "id": PANEL_ID,
                    "side": "right",
                    "width": 48,
                    "title": "JSON Workbench",
                },
            )
            self.panel_open = True
        path = document["path"] or "[scratch]"
        self.request_host(
            "red/TextPanelSetContent",
            {
                "id": PANEL_ID,
                "blocks": [
                    {
                        "id": "query",
                        "kind": "activity",
                        "format": "plain",
                        "text": (
                            f"Source: {path} @ {document['revision']}\n"
                            f"Filter: {query_text}"
                        ),
                    },
                    {
                        "id": "result",
                        "kind": "text",
                        "format": "plain",
                        "text": output,
                    },
                ],
            },
        )

    def query(self, params):
        query_text = params.get("raw_args", "")
        if not query_text.strip():
            raise OperationError("JsonQuery requires a jq filter")
        if len(query_text.encode("utf-8")) > QUERY_LIMIT:
            raise OperationError(f"jq filter exceeds {QUERY_LIMIT} bytes")
        self.candidate = None
        try:
            document = self.snapshot_source()
            source = document["text"]
            source_bytes = source.encode("utf-8")
            if len(source_bytes) > SOURCE_LIMIT:
                raise OperationError(f"JSON source exceeds {SOURCE_LIMIT} bytes")
            self.set_status("JSON: running")
            output = self.run_jq(query_text, source_bytes)
            self.show_result(document, query_text, output)
            self.set_status(f"JSON: ready ({len(output.encode('utf-8'))} B)")
        except OperationError as error:
            self.retire_preview(error)
            raise
        self.candidate = {
            "document_id": self.source_id,
            "revision": document["revision"],
            "source": source,
            "output": output,
        }
        return {"message": "JSON preview ready"}

    def apply(self, _params):
        candidate = self.candidate
        if candidate is None:
            raise OperationError("JsonQuery has no applicable preview")
        if candidate["output"] == candidate["source"]:
            self.candidate = None
            self.best_effort_status("JSON: unchanged")
            return {"message": "JSON preview already matches the source"}
        try:
            applied = self.request_host(
                "red/DocumentApply",
                {
                    "document_id": candidate["document_id"],
                    "expected_revision": candidate["revision"],
                    "label": "JSON Workbench apply",
                    "edits": [
                        {
                            "range": {
                                "start": {"line": 0, "character": 0},
                                "end": end_position(candidate["source"]),
                            },
                            "text": candidate["output"],
                            "expected_text": candidate["source"],
                        }
                    ],
                },
            )
        except HostError as error:
            if error.code in (-32001, -32002):
                self.candidate = None
                self.retire_preview(error)
            raise
        self.transaction = {
            "document_id": candidate["document_id"],
            "transaction_id": applied["transaction_id"],
        }
        self.candidate = None
        self.best_effort_status("JSON: applied")
        return {"message": f"JSON result applied @ {applied['revision']}"}

    def undo(self, _params):
        if self.transaction is None:
            raise OperationError("JsonApply has no transaction to undo")
        try:
            undone = self.request_host("red/DocumentUndo", self.transaction)
        except HostError as error:
            if error.code == -32001:
                self.transaction = None
            raise
        self.transaction = None
        self.candidate = None
        self.best_effort_status("JSON: undone")
        return {"message": f"JSON result undone @ {undone['revision']}"}

    def close(self, _params):
        if self.panel_open:
            self.request_host("red/TextPanelDispose", {"id": PANEL_ID})
        self.request_host("red/StatuslineClear", {})
        self.source_id = None
        self.candidate = None
        self.transaction = None
        self.panel_open = False
        return None

    def execute(self, message):
        params = message.get("params", {})
        handlers = {
            "JsonQuery": self.query,
            "JsonApply": self.apply,
            "JsonUndo": self.undo,
            "JsonClose": self.close,
        }
        handler = handlers.get(params.get("name"))
        if handler is None:
            self.send_error(message, "unknown JSON Workbench command")
            return
        self.command_active.set()
        try:
            result = handler(params)
        except CommandCancelled:
            return
        except OperationError as error:
            self.best_effort_status("JSON: error")
            self.send_error(message, error)
        else:
            self.send({"jsonrpc": "2.0", "id": message["id"], "result": result})
        finally:
            self.command_active.clear()

    def serve(self):
        threading.Thread(target=self.read_messages, daemon=True).start()
        while not self.closed.is_set():
            message = self.incoming.get()
            if message is EOF:
                return
            if message.get("method") == "initialize":
                self.send(
                    {
                        "jsonrpc": "2.0",
                        "id": message["id"],
                        "result": {
                            "protocol_version": 1,
                            "commands": COMMANDS,
                            "events": [],
                        },
                    }
                )
            elif message.get("method") == "command/execute":
                self.execute(message)
            else:
                self.send_error(message, "method not found")


def main():
    jq_command = sys.argv[1:] or ["jq"]
    try:
        JsonWorkbench(jq_command).serve()
    except (BrokenPipeError, CommandCancelled):
        pass


if __name__ == "__main__":
    main()
