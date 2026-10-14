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
HISTORY_LIMIT = 20
PRESET_LIMIT = 128
PICKER_LABEL_LIMIT = 256
PANEL_ID = "json-workbench"
PICKER_ID = "json-queries"
HISTORY_KEY = "query-history"
PREFERENCES_KEY = "query-preferences"
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
        "name": "JsonQueries",
        "metadata": {
            "title": "Choose a saved JSON query",
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
            overflow.put(OperationError(f"jq {name} exceeds {limit} bytes"))
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
        return overflow.get_nowait()
    except queue.Empty:
        return None


def bounded_text(text, limit):
    return text.encode("utf-8")[:limit].decode("utf-8", errors="ignore")


def query_label(query_text):
    label = (
        query_text.replace("\r\n", " ").replace("\r", " ").replace("\n", " ")
    ).strip()
    return bounded_text(label, PICKER_LABEL_LIMIT) or "[empty query]"


def normalize_history(value):
    if not isinstance(value, list):
        return []
    history = []
    for query_text in value:
        if not isinstance(query_text, str) or not query_text.strip():
            continue
        if len(query_text.encode("utf-8")) > QUERY_LIMIT:
            continue
        if query_text not in history:
            history.append(query_text)
        if len(history) == HISTORY_LIMIT:
            break
    return history


def normalize_preferences(value):
    if not isinstance(value, dict):
        return True
    enabled = value.get("history_enabled")
    return enabled if isinstance(enabled, bool) else True


def configured_jq_command(jq_path, default_command):
    if jq_path is None:
        return list(default_command)
    if not isinstance(jq_path, str) or not jq_path.strip():
        raise OperationError("jq_path must be a non-empty string")
    if len(jq_path.encode("utf-8")) > 4096:
        raise OperationError("jq_path may use at most 4096 bytes")
    return [jq_path]


def configured_timeout(timeout, default_timeout):
    if timeout is None:
        timeout = default_timeout
    if isinstance(timeout, bool) or not isinstance(timeout, (int, float)):
        raise OperationError("timeout_seconds must be a number")
    if not 0 < timeout <= JQ_TIMEOUT_SECONDS:
        raise OperationError(
            f"timeout_seconds must be between 0 and {JQ_TIMEOUT_SECONDS}"
        )
    return float(timeout)


def configured_result_limit(result_limit):
    if result_limit is None:
        result_limit = RESULT_LIMIT
    if isinstance(result_limit, bool) or not isinstance(result_limit, int):
        raise OperationError("result_limit_bytes must be an integer")
    if not 0 < result_limit <= RESULT_LIMIT:
        raise OperationError(f"result_limit_bytes must be between 1 and {RESULT_LIMIT}")
    return result_limit


def configured_preset(preset):
    if not isinstance(preset, dict):
        raise OperationError("each preset must be a table")
    label = preset.get("label")
    query_text = preset.get("query")
    if not isinstance(label, str) or not label.strip():
        raise OperationError("each preset requires a non-empty label")
    if len(label.encode("utf-8")) > PICKER_LABEL_LIMIT:
        raise OperationError(
            f"preset labels may use at most {PICKER_LABEL_LIMIT} bytes"
        )
    validate_query(query_text)
    return label, query_text


def configured_presets(raw_presets):
    if raw_presets is None:
        return []
    if not isinstance(raw_presets, list) or len(raw_presets) > PRESET_LIMIT:
        raise OperationError(
            f"presets must be a list of at most {PRESET_LIMIT} entries"
        )
    return [configured_preset(preset) for preset in raw_presets]


def parse_settings(value, default_command, default_timeout):
    if value is None:
        value = {}
    if not isinstance(value, dict):
        raise OperationError("JSON Workbench configuration must be a table")
    return (
        configured_jq_command(value.get("jq_path"), default_command),
        configured_timeout(value.get("timeout_seconds"), default_timeout),
        configured_result_limit(value.get("result_limit_bytes")),
        configured_presets(value.get("presets")),
    )


def validate_query(query_text):
    if not isinstance(query_text, str) or not query_text.strip():
        raise OperationError("JsonQuery requires a jq filter")
    if len(query_text.encode("utf-8")) > QUERY_LIMIT:
        raise OperationError(f"jq filter exceeds {QUERY_LIMIT} bytes")


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
        self.default_jq_command = list(jq_command)
        self.default_jq_timeout = jq_timeout
        self.jq_command = list(jq_command)
        self.jq_timeout = jq_timeout
        self.result_limit = RESULT_LIMIT
        self.presets = []
        self.settings_loaded = False
        self.history = []
        self.history_enabled = True
        self.storage_loaded = False
        self.storage_available = False
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
        self.active_picker_id = None
        self.picker_actions = {}
        self.active_run = None

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
        self.wake_pending(error)
        self.incoming.put(EOF)

    def wake_pending(self, error):
        with self.pending_lock:
            pending = list(self.pending.values())
        for response in pending:
            response.put(error)

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
            self.wake_pending(CommandCancelled())
        elif method == "exit":
            self.close_protocol(CommandCancelled())
            return False
        elif method == "picker/event":
            self.incoming.put(message)
        elif method == "command/execute" and self.command_active.is_set():
            self.send_error(message, "JSON Workbench is busy")
        elif "id" in message:
            self.incoming.put(message)
        return True

    def background_busy(self):
        if self.active_run is None:
            return False
        thread = self.active_run[0]
        return thread.ident is None or thread.is_alive()

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

    def ensure_panel(self):
        if self.panel_open:
            return
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

    def retire_preview(self, error, create=False):
        try:
            if create:
                self.ensure_panel()
            if not self.panel_open:
                return
            prefix = "Error: "
            detail = bounded_text(str(error), DIAGNOSTIC_LIMIT - len(prefix))
            self.request_host(
                "red/TextPanelSetContent",
                {
                    "id": PANEL_ID,
                    "blocks": [
                        {
                            "id": "error",
                            "kind": "text",
                            "format": "plain",
                            "text": prefix + detail,
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

    def run_cancelled(self, cancel):
        return (
            self.cancelling.is_set()
            or self.closed.is_set()
            or (cancel is not None and cancel.is_set())
        )

    def wait_for_jq(self, process, overflow, cancel=None):
        deadline = time.monotonic() + self.jq_timeout
        while process.poll() is None:
            if self.run_cancelled(cancel):
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

    def run_jq(self, query_text, source, cancel=None):
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
                args=(
                    process.stdout,
                    self.result_limit,
                    "output",
                    collected,
                    overflow,
                ),
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

        failure = self.wait_for_jq(process, overflow, cancel)
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
        self.ensure_panel()
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

    def load_settings(self):
        if self.settings_loaded:
            return
        config = self.request_host("red/ConfigGet", {})
        command, timeout, result_limit, presets = parse_settings(
            config, self.default_jq_command, self.default_jq_timeout
        )
        self.jq_command = command
        self.jq_timeout = timeout
        self.result_limit = result_limit
        self.presets = presets
        self.settings_loaded = True

    def load_storage(self):
        if self.storage_loaded:
            return self.storage_available
        try:
            preferences = self.request_host(
                "red/StorageGet", {"key": PREFERENCES_KEY}
            )
        except OperationError:
            self.storage_loaded = True
            return False
        self.history_enabled = normalize_preferences(preferences)
        try:
            history = self.request_host("red/StorageGet", {"key": HISTORY_KEY})
        except OperationError:
            self.storage_loaded = True
            return False
        self.history = normalize_history(history)
        self.storage_loaded = True
        self.storage_available = True
        return True

    def ensure_run_active(self, cancel):
        if self.run_cancelled(cancel):
            raise CommandCancelled()

    def remember_query(self, query_text):
        if not self.history_enabled:
            return True
        self.history = normalize_history([query_text, *self.history])
        if not self.storage_available:
            return False
        try:
            self.request_host(
                "red/StorageSet", {"key": HISTORY_KEY, "value": self.history}
            )
        except OperationError:
            self.storage_available = False
            return False
        return True

    def run_query(self, query_text, cancel=None, background=False):
        validate_query(query_text)
        self.candidate = None
        try:
            self.ensure_run_active(cancel)
            document = self.snapshot_source()
            self.ensure_run_active(cancel)
            source = document["text"]
            source_bytes = source.encode("utf-8")
            if len(source_bytes) > SOURCE_LIMIT:
                raise OperationError(f"JSON source exceeds {SOURCE_LIMIT} bytes")
            self.set_status("JSON: running")
            output = self.run_jq(query_text, source_bytes, cancel)
            self.ensure_run_active(cancel)
            self.show_result(document, query_text, output)
            self.ensure_run_active(cancel)
            self.candidate = {
                "document_id": self.source_id,
                "revision": document["revision"],
                "source": source,
                "output": output,
            }
            saved = self.remember_query(query_text)
            self.ensure_run_active(cancel)
            note = "" if saved else "; history not saved"
            self.set_status(
                f"JSON: ready ({len(output.encode('utf-8'))} B{note})"
            )
        except OperationError as error:
            self.retire_preview(error, create=background)
            raise
        return {"message": "JSON preview ready"}

    def query(self, params):
        query_text = params.get("raw_args", "")
        validate_query(query_text)
        self.clear_picker()
        self.load_settings()
        self.load_storage()
        return self.run_query(query_text)

    def clear_picker(self):
        self.active_picker_id = None
        self.picker_actions = {}

    def query_picker_items(self):
        items = []
        actions = {}
        for index, (label, query_text) in enumerate(self.presets):
            item_id = f"preset-{index}"
            items.append(
                {
                    "id": item_id,
                    "label": label,
                    "detail": query_label(query_text),
                }
            )
            actions[item_id] = ("query", query_text)
        for index, query_text in enumerate(self.history):
            item_id = f"history-{index}"
            items.append(
                {"id": item_id, "label": query_label(query_text), "detail": "Recent"}
            )
            actions[item_id] = ("query", query_text)
        if self.history:
            items.append({"id": "history-clear", "label": "Clear query history"})
            actions["history-clear"] = ("clear", None)
        toggle_label = (
            "Pause query history" if self.history_enabled else "Resume query history"
        )
        items.append({"id": "history-toggle", "label": toggle_label})
        actions["history-toggle"] = ("toggle", None)
        return items, actions

    def queries(self, _params):
        if self.source_id is None:
            self.snapshot_source()
        self.load_settings()
        self.load_storage()
        items, actions = self.query_picker_items()
        self.clear_picker()
        self.active_picker_id = PICKER_ID
        self.picker_actions = actions
        try:
            self.request_host(
                "red/PickerOpen",
                {"id": PICKER_ID, "title": "JSON Queries", "items": items},
            )
        except OperationError:
            self.clear_picker()
            raise
        return None

    def clear_history(self):
        self.history = []
        note = ""
        try:
            self.request_host("red/StorageDelete", {"key": HISTORY_KEY})
        except OperationError:
            self.storage_available = False
            note = " (not saved)"
        self.best_effort_status(f"JSON: history cleared{note}")

    def toggle_history(self):
        self.history_enabled = not self.history_enabled
        note = ""
        try:
            self.request_host(
                "red/StorageSet",
                {
                    "key": PREFERENCES_KEY,
                    "value": {"history_enabled": self.history_enabled},
                },
            )
        except OperationError:
            self.storage_available = False
            note = " (not saved)"
        state = "resumed" if self.history_enabled else "paused"
        self.best_effort_status(f"JSON: history {state}{note}")

    def background_query(self, query_text, cancel):
        try:
            self.run_query(query_text, cancel=cancel, background=True)
        except CommandCancelled:
            pass
        except OperationError:
            self.best_effort_status("JSON: error")

    def start_background_query(self, query_text):
        if self.background_busy():
            self.best_effort_status("JSON: busy")
            return
        self.candidate = None
        cancel = threading.Event()
        thread = threading.Thread(
            target=self.background_query,
            args=(query_text, cancel),
            name="json-workbench-query",
        )
        self.active_run = (thread, cancel)
        thread.start()

    def stop_background_query(self):
        if self.active_run is None:
            return
        thread, cancel = self.active_run
        cancel.set()
        if thread.ident is not None:
            thread.join()
        self.active_run = None

    def handle_picker_event(self, message):
        params = message.get("params", {})
        if params.get("id") != self.active_picker_id:
            return
        actions = self.picker_actions
        self.clear_picker()
        if params.get("event") != "selected":
            return
        item = params.get("item")
        if not isinstance(item, dict):
            return
        action = actions.get(item.get("id"))
        if action is None:
            return
        kind, query_text = action
        if kind == "query":
            self.start_background_query(query_text)
        elif kind == "clear":
            self.clear_history()
        else:
            self.toggle_history()

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
        self.clear_picker()
        self.stop_background_query()
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
        name = params.get("name")
        if self.background_busy() and name != "JsonClose":
            self.send_error(message, "JSON Workbench is busy")
            return
        handlers = {
            "JsonQuery": self.query,
            "JsonQueries": self.queries,
            "JsonApply": self.apply,
            "JsonUndo": self.undo,
            "JsonClose": self.close,
        }
        handler = handlers.get(name)
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
        try:
            while not self.closed.is_set():
                message = self.incoming.get()
                if message is EOF:
                    return
                method = message.get("method")
                if method == "initialize":
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
                elif method == "command/execute":
                    self.execute(message)
                elif method == "picker/event":
                    self.handle_picker_event(message)
                else:
                    self.send_error(message, "method not found")
        finally:
            self.clear_picker()
            self.stop_background_query()


def main():
    jq_command = sys.argv[1:] or ["jq"]
    try:
        JsonWorkbench(jq_command).serve()
    except (BrokenPipeError, CommandCancelled):
        pass


if __name__ == "__main__":
    main()
