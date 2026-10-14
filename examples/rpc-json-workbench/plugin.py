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
LIVE_DEBOUNCE_SECONDS = 0.12
HISTORY_LIMIT = 20
PRESET_LIMIT = 128
PICKER_LABEL_LIMIT = 256
EXPLORE_ROOT_LIMIT = 1024
EXPLORE_CHILD_LIMIT = 1000
EXPLORE_NODE_LIMIT = 10000
EXPLORE_DEPTH_LIMIT = 64
PANEL_ID = "json-workbench"
PICKER_ID = "json-queries"
HISTORY_KEY = "query-history"
PREFERENCES_KEY = "query-preferences"
EOF = object()


def panel_action(
    action_id,
    key,
    label,
    description,
    priority="primary",
    enabled=True,
    show_in_header=True,
):
    return {
        "id": action_id,
        "key": key,
        "label": label,
        "group": "JSON Workbench",
        "description": description,
        "priority": priority,
        "enabled": enabled,
        "show_in_header": show_in_header,
    }


COMMANDS = [
    {
        "name": "JsonWorkbench",
        "metadata": {
            "title": "Open an editable JSON query",
            "category": "JSON Workbench",
        },
    },
    {
        "name": "JsonSelection",
        "metadata": {
            "title": "Open a JSON workbench for the last selection",
            "category": "JSON Workbench",
        },
    },
    {
        "name": "JsonRun",
        "metadata": {
            "title": "Run the editable JSON query",
            "category": "JSON Workbench",
        },
    },
    {
        "name": "JsonLive",
        "metadata": {
            "title": "Toggle live JSON previews",
            "category": "JSON Workbench",
        },
    },
    {
        "name": "JsonCancel",
        "metadata": {
            "title": "Cancel the current JSON preview",
            "category": "JSON Workbench",
        },
    },
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
        "name": "JsonOpenResult",
        "metadata": {
            "title": "Open the JSON result in a document",
            "category": "JSON Workbench",
        },
    },
    {
        "name": "JsonExplore",
        "metadata": {
            "title": "Explore the JSON result",
            "category": "JSON Workbench",
        },
    },
    {
        "name": "JsonCopy",
        "metadata": {
            "title": "Copy the JSON result",
            "category": "JSON Workbench",
        },
    },
    {
        "name": "JsonSource",
        "metadata": {
            "title": "Show the JSON source document",
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


class JsonObject:
    def __init__(self, pairs):
        self.pairs = pairs


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


def parse_json_stream(text):
    def reject_constant(value):
        raise ValueError(f"non-standard JSON value: {value}")

    decoder = json.JSONDecoder(
        object_pairs_hook=JsonObject,
        parse_constant=reject_constant,
    )
    roots = []
    offset = 0
    while True:
        while offset < len(text) and text[offset].isspace():
            offset += 1
        if offset == len(text):
            break
        if len(roots) >= EXPLORE_ROOT_LIMIT:
            raise OperationError(
                f"JSON result contains more than {EXPLORE_ROOT_LIMIT} roots"
            )
        try:
            value, offset = decoder.raw_decode(text, offset)
        except (json.JSONDecodeError, RecursionError, ValueError) as error:
            raise OperationError(f"result is not valid bounded JSON: {error}") from error
        roots.append(value)
    validate_json_tree(roots)
    return roots


def validate_json_tree(roots):
    pending = [(root, 1) for root in roots]
    nodes = 0
    while pending:
        node, depth = pending.pop()
        nodes += 1
        if nodes > EXPLORE_NODE_LIMIT:
            raise OperationError(
                f"JSON result contains more than {EXPLORE_NODE_LIMIT} values"
            )
        if depth > EXPLORE_DEPTH_LIMIT:
            raise OperationError(
                f"JSON result exceeds {EXPLORE_DEPTH_LIMIT} levels"
            )
        if isinstance(node, JsonObject):
            pending.extend((value, depth + 1) for _key, value in node.pairs)
        elif isinstance(node, list):
            pending.extend((value, depth + 1) for value in node)


def json_value_detail(value):
    if isinstance(value, JsonObject):
        return f"object · {len(value.pairs)} keys"
    if isinstance(value, list):
        return f"array · {len(value)} items"
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "boolean"
    if isinstance(value, str):
        return f"string · {len(value)} characters"
    return "number"


def json_node(roots, path):
    node = roots[path[0][1]]
    for kind, value in path[1:]:
        if kind == "index":
            node = node[value]
        else:
            node = next(child for key, child in node.pairs if key == value)
    return node


def json_breadcrumb(root_count, path):
    if not path:
        return f"stream · {root_count} roots"
    root = path[0][1]
    breadcrumb = "$" if root_count == 1 else f"$<{root + 1}>"
    for kind, value in path[1:]:
        token = value if kind == "index" else json.dumps(value, ensure_ascii=False)
        breadcrumb += f"[{token}]"
    return breadcrumb


def json_path_query(base_query, root_count, path):
    root = path[0][1]
    query = base_query if root_count == 1 else f"[({base_query})][{root}]"
    for kind, value in path[1:]:
        token = str(value) if kind == "index" else json.dumps(value)
        query = f"({query}) | .[{token}]"
    validate_query(query)
    return query


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
        self.explore_picker_ids = itertools.count(1)
        self.source_id = None
        self.source_selection = None
        self.query_document_id = None
        self.last_query = None
        self.result = None
        self.result_context = None
        self.result_generation = 0
        self.candidate = None
        self.transaction = None
        self.panel_open = False
        self.sent_panel_actions = None
        self.active_picker_id = None
        self.picker_actions = {}
        self.explore_roots = None
        self.explore_path = None
        self.inspect_path = None
        self.active_run = None
        self.run_generation = 0
        self.live_enabled = False
        self.watched_ids = set()

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
        elif method in ("picker/event", "document/event", "panel/event"):
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
        actions = self.panel_actions()
        self.request_host(
            "red/TextPanelCreate",
            {
                "id": PANEL_ID,
                "side": "right",
                "width": 48,
                "title": "JSON Workbench",
                "actions": actions,
            },
        )
        self.panel_open = True
        self.sent_panel_actions = actions

    @staticmethod
    def run_panel_action(busy):
        if busy:
            return panel_action(
                "cancel",
                "Ctrl-c",
                "Cancel",
                "Cancel the running query",
                priority="essential",
            )
        return panel_action(
            "run",
            "r",
            "Run",
            "Run the current jq query",
            priority="essential",
        )

    def panel_actions(self, busy=None):
        if busy is None:
            busy = self.background_busy()
        has_result = self.result is not None and not busy
        has_source = self.source_id is not None and not busy
        return [
            self.run_panel_action(busy),
            panel_action(
                "apply",
                "a",
                "Apply",
                "Replace the source with the current result",
                enabled=self.candidate is not None and not busy,
            ),
            panel_action(
                "close",
                "q",
                "×",
                "Close the JSON Workbench",
                priority="essential",
            ),
            panel_action(
                "explore",
                "Enter",
                "Explore",
                "Explore the result as a JSON tree",
                priority="essential",
                enabled=has_result,
                show_in_header=False,
            ),
            panel_action(
                "open",
                "o",
                "Open",
                "Open the result in an editor buffer",
                enabled=has_result,
                show_in_header=False,
            ),
            panel_action(
                "copy",
                "c",
                "Copy",
                "Copy the complete result",
                enabled=has_result,
                show_in_header=False,
            ),
            panel_action(
                "source",
                "s",
                "Source",
                "Return to the source document",
                priority="secondary",
                enabled=has_source,
                show_in_header=False,
            ),
            panel_action(
                "undo",
                "u",
                "Undo",
                "Undo the last workbench Apply",
                priority="secondary",
                enabled=self.transaction is not None and not busy,
                show_in_header=False,
            ),
            panel_action(
                "live",
                "",
                "Pause live preview"
                if self.live_enabled
                else "Start live preview",
                "Toggle automatic previews when query or source changes",
                priority="reference",
                enabled=self.source_id is not None
                and self.query_document_id is not None,
                show_in_header=False,
            ),
            panel_action(
                "queries",
                "",
                "Saved queries",
                "Choose a preset or recent jq query",
                priority="reference",
                enabled=not busy,
                show_in_header=False,
            ),
        ]

    def update_panel_actions(self, busy=None):
        if not self.panel_open:
            return
        actions = self.panel_actions(busy)
        if actions == self.sent_panel_actions:
            return
        self.request_host(
            "red/TextPanelSetActions", {"id": PANEL_ID, "actions": actions}
        )
        self.sent_panel_actions = actions

    def best_effort_panel_actions(self, busy=None):
        try:
            self.update_panel_actions(busy)
        except (OperationError, CommandCancelled):
            pass

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
        self.best_effort_panel_actions()

    def snapshot_source(self, include_selection=False):
        if self.source_id is None:
            params = {"include_document_id": True}
            if include_selection:
                params["include_selection"] = True
            document = self.request_host("red/DocumentSnapshot", params)
            self.source_id = document["document_id"]
            selection = document.get("selection")
            if selection is not None:
                self.source_selection = {
                    "range": selection["range"],
                    "text": selection["text"],
                    "revision": document["revision"],
                }
            return document
        return self.request_host(
            "red/DocumentSnapshot", {"document_id": self.source_id}
        )

    def query_source(self, document):
        selection = self.source_selection
        if selection is None:
            source = document["text"]
            return source, {
                "start": {"line": 0, "character": 0},
                "end": end_position(source),
            }
        if document["revision"] != selection["revision"]:
            raise OperationError(
                "selected JSON source changed; close and reopen the workbench"
            )
        return selection["text"], selection["range"]

    def run_cancelled(self, cancel, generation=None):
        return (
            self.cancelling.is_set()
            or self.closed.is_set()
            or (cancel is not None and cancel.is_set())
            or (generation is not None and generation != self.run_generation)
        )

    def wait_for_jq(self, process, overflow, cancel=None, generation=None):
        deadline = time.monotonic() + self.jq_timeout
        while process.poll() is None:
            if self.run_cancelled(cancel, generation):
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

    def run_jq(self, query_text, source, cancel=None, generation=None):
        try:
            process = subprocess.Popen(
                [*self.jq_command, "--", query_text],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
        except FileNotFoundError as error:
            executable = self.jq_command[0]
            raise OperationError(
                f"jq executable `{executable}` was not found; "
                "install jq or set plugin_config.json-workbench.jq_path"
            ) from error
        except OSError as error:
            raise OperationError(f"jq could not start: {error}") from error

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

        failure = self.wait_for_jq(process, overflow, cancel, generation)
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

    def show_result(self, document, query_text, output, inspect_path=None):
        self.ensure_panel()
        path = document["path"] or "[scratch]"
        inspection = "" if inspect_path is None else f"\nInspect: {inspect_path}"
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
                            f"Filter: {query_text}{inspection}"
                        ),
                    },
                    {
                        "id": "result",
                        "kind": "text",
                        "format": "code",
                        "language": "json",
                        "text": output,
                    },
                ],
            },
        )

    def create_document(self, text, syntax=None):
        params = {"text": text}
        if syntax is not None:
            params["syntax"] = syntax
        created = self.request_host("red/DocumentCreate", params)
        if not isinstance(created, dict) or not isinstance(
            created.get("document_id"), str
        ):
            raise OperationError("red/DocumentCreate returned an invalid document ID")
        return created["document_id"]

    def show_document(self, document_id):
        self.request_host("red/DocumentShow", {"document_id": document_id})

    def initial_query(self):
        if self.last_query is not None:
            return self.last_query
        if self.history:
            return self.history[0]
        if self.presets:
            return self.presets[0][1]
        return "."

    def show_workbench_help(self, document):
        if self.result is not None:
            return
        self.ensure_panel()
        path = document["path"] or "[scratch]"
        self.request_host(
            "red/TextPanelSetContent",
            {
                "id": PANEL_ID,
                "blocks": [
                    {
                        "id": "source",
                        "kind": "activity",
                        "format": "plain",
                        "text": f"Source: {path} @ {document['revision']}",
                    },
                    {
                        "id": "instructions",
                        "kind": "text",
                        "format": "plain",
                        "text": "Edit the query document, then run :JsonRun.",
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

    def ensure_run_active(self, cancel, generation=None):
        if self.run_cancelled(cancel, generation):
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

    def run_query(
        self, query_text, cancel=None, background=False, generation=None
    ):
        validate_query(query_text)
        self.last_query = query_text
        self.result_generation += 1
        self.clear_explorer()
        self.result = None
        self.result_context = None
        self.candidate = None
        try:
            self.ensure_run_active(cancel, generation)
            document = self.snapshot_source()
            self.ensure_run_active(cancel, generation)
            source, source_range = self.query_source(document)
            source_bytes = source.encode("utf-8")
            if len(source_bytes) > SOURCE_LIMIT:
                raise OperationError(f"JSON source exceeds {SOURCE_LIMIT} bytes")
            self.set_status("JSON: running")
            output = self.run_jq(query_text, source_bytes, cancel, generation)
            self.ensure_run_active(cancel, generation)
            self.show_result(document, query_text, output)
            self.ensure_run_active(cancel, generation)
            self.result = output
            self.result_context = {
                "path": document["path"],
                "revision": document["revision"],
                "query": query_text,
            }
            self.candidate = {
                "document_id": self.source_id,
                "revision": document["revision"],
                "range": source_range,
                "source": source,
                "output": output,
            }
            saved = self.remember_query(query_text)
            self.ensure_run_active(cancel, generation)
            note = "" if saved else "; history not saved"
            self.set_status(
                f"JSON: ready ({len(output.encode('utf-8'))} B{note})"
            )
            self.update_panel_actions(busy=False)
        except OperationError as error:
            self.retire_preview(error, create=background)
            raise
        return {"message": "JSON preview ready"}

    def show_workbench(self, document):
        self.load_settings()
        self.load_storage()
        self.show_workbench_help(document)
        if self.query_document_id is not None:
            try:
                self.show_document(self.query_document_id)
            except HostError as error:
                if error.code != -32001:
                    raise
                self.query_document_id = None
        if self.query_document_id is None:
            self.query_document_id = self.create_document(self.initial_query())
            self.show_document(self.query_document_id)
        self.best_effort_status("JSON: query")
        self.update_panel_actions()
        return {"message": "JSON query document ready"}

    def workbench(self, _params):
        self.clear_picker()
        return self.show_workbench(self.snapshot_source())

    def selection(self, _params):
        if self.source_id is not None:
            raise OperationError("close the current JSON Workbench first")
        self.clear_picker()
        document = self.snapshot_source(include_selection=True)
        if self.source_selection is None:
            self.source_id = None
            raise OperationError("JsonSelection has no contiguous visual selection")
        return self.show_workbench(document)

    def snapshot_query(self):
        if self.query_document_id is None:
            raise OperationError("JsonWorkbench has no query document")
        try:
            return self.request_host(
                "red/DocumentSnapshot", {"document_id": self.query_document_id}
            )
        except HostError as error:
            if error.code == -32001:
                self.query_document_id = None
            raise

    def run(self, _params):
        self.stop_background_query()
        query = self.snapshot_query()
        self.load_settings()
        self.load_storage()
        return self.run_query(query["text"])

    def watch_document(self, document_id):
        watched = self.request_host(
            "red/DocumentWatch", {"document_id": document_id}
        )
        if not isinstance(watched, dict) or not isinstance(
            watched.get("revision"), str
        ) or not watched["revision"].isdigit():
            raise OperationError("red/DocumentWatch returned an invalid revision")

    def disable_live(self):
        self.live_enabled = False
        self.stop_background_query()
        watched_ids = tuple(sorted(self.watched_ids))
        self.watched_ids.clear()
        for document_id in watched_ids:
            self.request_host(
                "red/DocumentUnwatch", {"document_id": document_id}
            )

    def watch_workbench_documents(self):
        watched = []
        try:
            for document_id in (self.source_id, self.query_document_id):
                self.watch_document(document_id)
                watched.append(document_id)
        except OperationError:
            for document_id in watched:
                try:
                    self.request_host(
                        "red/DocumentUnwatch", {"document_id": document_id}
                    )
                except OperationError:
                    pass
            raise
        return set(watched)

    def live(self, _params):
        if self.live_enabled:
            self.disable_live()
            self.best_effort_status("JSON: live paused")
            self.update_panel_actions()
            return {"message": "JSON live preview paused"}
        self.stop_background_query()
        if self.source_id is None or self.query_document_id is None:
            raise OperationError("JsonWorkbench must be open before live preview")
        self.watched_ids = self.watch_workbench_documents()
        self.live_enabled = True
        self.start_background_query(None, replace=True, debounce=True)
        self.best_effort_status("JSON: live")
        self.update_panel_actions()
        return {"message": "JSON live preview scheduled"}

    def cancel(self, _params):
        self.stop_background_query()
        self.clear_explorer()
        self.candidate = None
        self.result = None
        self.result_context = None
        self.best_effort_status("JSON: cancelled")
        self.update_panel_actions()
        return {"message": "JSON preview cancelled"}

    def open_result(self, _params):
        if self.result is None:
            raise OperationError("JsonRun has no result to open")
        self.clear_picker()
        document_id = self.create_document(self.result, syntax="json")
        self.show_document(document_id)
        self.best_effort_status("JSON: result")
        return {"message": "JSON result document ready"}

    def copy_result(self, _params):
        if self.result is None:
            raise OperationError("JsonRun has no result to copy")
        self.request_host("red/ClipboardWrite", {"text": self.result})
        self.best_effort_status("JSON: copied")
        return {"message": "JSON result copied"}

    def clear_explorer(self):
        if self.active_picker_id is not None and self.active_picker_id.startswith(
            "json-explore-"
        ):
            self.clear_picker()
        self.explore_roots = None
        self.explore_path = None
        self.inspect_path = None

    def refresh_result_panel(self):
        context = self.result_context
        if context is None or self.result is None:
            raise OperationError("JsonRun has no result to explore")
        self.show_result(
            context,
            context["query"],
            self.result,
            inspect_path=self.inspect_path,
        )

    def explorer_root_items(self, generation, roots):
        if not roots:
            return [{"id": "empty", "label": "No JSON roots"}], {}
        items = []
        actions = {}
        for index, value in enumerate(roots):
            item_id = f"root-{index}"
            items.append(
                {
                    "id": item_id,
                    "label": f"Root {index + 1}",
                    "detail": json_value_detail(value),
                }
            )
            actions[item_id] = ("explore", (generation, (("root", index),)))
        return items, actions

    def explorer_object_children(self, generation, path, node):
        counts = {}
        for key, _value in node.pairs:
            counts[key] = counts.get(key, 0) + 1
        items = []
        actions = {}
        for index, (key, value) in enumerate(node.pairs[:EXPLORE_CHILD_LIMIT]):
            item_id = f"key-{index}"
            detail = json_value_detail(value)
            duplicate = counts[key] > 1
            if duplicate:
                detail += " · duplicate key is not addressable"
            items.append(
                {
                    "id": item_id,
                    "label": bounded_text(
                        json.dumps(key, ensure_ascii=False), PICKER_LABEL_LIMIT
                    ),
                    "detail": detail,
                }
            )
            if not duplicate:
                actions[item_id] = (
                    "explore",
                    (generation, (*path, ("key", key))),
                )
        return items, actions, len(node.pairs)

    def explorer_array_children(self, generation, path, node):
        items = []
        actions = {}
        for index, value in enumerate(node[:EXPLORE_CHILD_LIMIT]):
            item_id = f"index-{index}"
            items.append(
                {
                    "id": item_id,
                    "label": f"[{index}]",
                    "detail": json_value_detail(value),
                }
            )
            actions[item_id] = (
                "explore",
                (generation, (*path, ("index", index))),
            )
        return items, actions, len(node)

    def explorer_children(self, generation, path, node):
        if isinstance(node, JsonObject):
            return self.explorer_object_children(generation, path, node)
        if isinstance(node, list):
            return self.explorer_array_children(generation, path, node)
        return [], {}, 0

    def explorer_items(self, generation, path):
        roots = self.explore_roots
        if roots is None:
            return [], {}
        if not path:
            return self.explorer_root_items(generation, roots)

        context = self.result_context
        node = json_node(roots, path)
        query = json_path_query(context["query"], len(roots), path)
        items = [
            {
                "id": "run-path",
                "label": "Run this path",
                "detail": query_label(query),
            },
            {"id": "back", "label": "Back"},
        ]
        actions = {
            "run-path": ("explore-query", (generation, query)),
            "back": ("explore", (generation, path[:-1])),
        }
        children, child_actions, child_count = self.explorer_children(
            generation, path, node
        )
        items.extend(children)
        actions.update(child_actions)
        if child_count > EXPLORE_CHILD_LIMIT:
            items.append(
                {
                    "id": "truncated",
                    "label": f"… {child_count - EXPLORE_CHILD_LIMIT} more values",
                }
            )
        return items, actions

    def open_explorer(self, generation, path):
        if generation != self.result_generation or self.explore_roots is None:
            return
        breadcrumb = json_breadcrumb(len(self.explore_roots), path)
        self.explore_path = path
        self.inspect_path = breadcrumb
        self.refresh_result_panel()
        items, actions = self.explorer_items(generation, path)
        self.clear_picker()
        self.active_picker_id = (
            f"json-explore-{generation}-{next(self.explore_picker_ids)}"
        )
        self.picker_actions = actions
        try:
            self.request_host(
                "red/PickerOpen",
                {
                    "id": self.active_picker_id,
                    "title": bounded_text(f"JSON {breadcrumb}", PICKER_LABEL_LIMIT),
                    "items": items,
                },
            )
        except OperationError:
            self.clear_picker()
            raise

    def explore(self, _params):
        if self.result is None or self.result_context is None:
            raise OperationError("JsonRun has no result to explore")
        generation = self.result_generation
        if self.explore_roots is None:
            self.explore_roots = parse_json_stream(self.result)
            self.explore_path = (
                (("root", 0),) if len(self.explore_roots) == 1 else ()
            )
        path = self.explore_path
        self.open_explorer(generation, path)
        return None

    def source(self, _params):
        if self.source_id is None:
            raise OperationError("JSON Workbench has no source document")
        self.clear_picker()
        try:
            self.show_document(self.source_id)
        except HostError as error:
            if error.code == -32001:
                self.source_id = None
                self.source_selection = None
                self.candidate = None
                self.transaction = None
                self.result = None
                self.result_context = None
                self.clear_explorer()
                self.best_effort_panel_actions()
            raise
        self.best_effort_status("JSON: source")
        return {"message": "JSON source document ready"}

    def query(self, params):
        query_text = params.get("raw_args", "")
        validate_query(query_text)
        self.stop_background_query()
        self.clear_picker()
        self.load_settings()
        self.load_storage()
        return self.run_query(query_text)

    def clear_picker(self):
        self.active_picker_id = None
        self.picker_actions = {}

    def handle_picker_action(self, kind, value):
        if kind == "query":
            self.start_background_query(value)
        elif kind == "explore":
            generation, path = value
            self.open_explorer(generation, path)
        elif kind == "explore-query":
            generation, query_text = value
            if generation == self.result_generation:
                self.start_background_query(query_text)
        elif kind == "clear":
            self.clear_history()
        elif kind == "toggle":
            self.toggle_history()

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

    def background_query(self, query_text, cancel, generation, debounce):
        try:
            if debounce and cancel.wait(LIVE_DEBOUNCE_SECONDS):
                raise CommandCancelled()
            if query_text is None:
                query_text = self.snapshot_query()["text"]
            self.run_query(
                query_text,
                cancel=cancel,
                background=True,
                generation=generation,
            )
        except CommandCancelled:
            pass
        except OperationError:
            self.best_effort_status("JSON: error")
        finally:
            self.best_effort_panel_actions(busy=False)

    def start_background_query(self, query_text, replace=False, debounce=False):
        if self.background_busy():
            if not replace:
                self.best_effort_status("JSON: busy")
                return False
            self.stop_background_query()
        self.run_generation += 1
        generation = self.run_generation
        self.clear_explorer()
        self.candidate = None
        self.result = None
        self.result_context = None
        cancel = threading.Event()
        thread = threading.Thread(
            target=self.background_query,
            args=(query_text, cancel, generation, debounce),
            name="json-workbench-query",
        )
        self.active_run = (thread, cancel, generation)
        self.update_panel_actions()
        thread.start()
        return True

    def stop_background_query(self):
        if self.active_run is None:
            return
        thread, cancel, _generation = self.active_run
        self.run_generation += 1
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
        try:
            self.handle_picker_action(*action)
        except CommandCancelled:
            pass
        except OperationError as error:
            self.retire_preview(error)
            self.best_effort_status("JSON: error")

    def handle_panel_event(self, message):
        params = message.get("params", {})
        if params.get("id") != PANEL_ID:
            return
        action = params.get("action")
        handlers = {
            "cancel": self.cancel,
            "apply": self.apply,
            "explore": self.explore,
            "open": self.open_result,
            "copy": self.copy_result,
            "source": self.source,
            "undo": self.undo,
            "live": self.live,
            "queries": self.queries,
            "close": self.close,
        }
        if action == "run":
            self.start_background_query(None)
            return
        handler = handlers.get(action)
        if handler is None:
            return
        if self.background_busy() and action not in ("cancel", "close", "live"):
            self.best_effort_status("JSON: busy")
            return
        try:
            handler({})
        except CommandCancelled:
            pass
        except OperationError as error:
            self.retire_preview(error)
            self.best_effort_status("JSON: error")

    def handle_document_event(self, message):
        params = message.get("params", {})
        document_id = params.get("document_id")
        if document_id not in self.watched_ids:
            return
        event = params.get("event")
        if event == "changed":
            revision = params.get("revision")
            if self.live_enabled and isinstance(revision, str) and revision.isdigit():
                self.start_background_query(None, replace=True, debounce=True)
            return
        if event == "closed":
            self.retire_closed_document(document_id)

    def retire_closed_document(self, document_id):
        if document_id == self.source_id:
            self.source_id = None
            self.source_selection = None
        if document_id == self.query_document_id:
            self.query_document_id = None
        self.disable_live()
        self.clear_explorer()
        self.candidate = None
        self.result = None
        self.result_context = None
        error = OperationError("JSON Workbench document was closed")
        self.retire_preview(error, create=True)
        self.best_effort_status("JSON: closed")

    def apply(self, _params):
        candidate = self.candidate
        if candidate is None:
            raise OperationError("JsonQuery has no applicable preview")
        if candidate["output"] == candidate["source"]:
            self.candidate = None
            self.best_effort_status("JSON: unchanged")
            self.update_panel_actions()
            return {"message": "JSON preview already matches the source"}
        try:
            applied = self.request_host(
                "red/DocumentApply",
                {
                    "document_id": candidate["document_id"],
                    "allow_inactive": True,
                    "expected_revision": candidate["revision"],
                    "label": "JSON Workbench apply",
                    "edits": [
                        {
                            "range": candidate["range"],
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
            "allow_inactive": True,
            "transaction_id": applied["transaction_id"],
        }
        self.candidate = None
        self.best_effort_status("JSON: applied")
        self.update_panel_actions()
        return {"message": f"JSON result applied @ {applied['revision']}"}

    def undo(self, _params):
        if self.transaction is None:
            raise OperationError("JsonApply has no transaction to undo")
        try:
            undone = self.request_host("red/DocumentUndo", self.transaction)
        except HostError as error:
            if error.code == -32001:
                self.transaction = None
                self.best_effort_panel_actions()
            raise
        self.transaction = None
        self.candidate = None
        self.best_effort_status("JSON: undone")
        self.update_panel_actions()
        return {"message": f"JSON result undone @ {undone['revision']}"}

    def close(self, _params):
        self.clear_picker()
        if self.live_enabled or self.watched_ids:
            self.disable_live()
        else:
            self.stop_background_query()
        if self.panel_open:
            self.request_host("red/TextPanelDispose", {"id": PANEL_ID})
        self.request_host("red/StatuslineClear", {})
        self.source_id = None
        self.source_selection = None
        self.query_document_id = None
        self.result = None
        self.result_context = None
        self.explore_roots = None
        self.explore_path = None
        self.inspect_path = None
        self.candidate = None
        self.transaction = None
        self.panel_open = False
        self.sent_panel_actions = None
        return None

    def execute(self, message):
        params = message.get("params", {})
        name = params.get("name")
        background_commands = {
            "JsonCancel",
            "JsonClose",
            "JsonLive",
            "JsonQuery",
            "JsonRun",
        }
        if self.background_busy() and name not in background_commands:
            self.send_error(message, "JSON Workbench is busy")
            return
        handlers = {
            "JsonWorkbench": self.workbench,
            "JsonSelection": self.selection,
            "JsonRun": self.run,
            "JsonLive": self.live,
            "JsonCancel": self.cancel,
            "JsonQuery": self.query,
            "JsonQueries": self.queries,
            "JsonOpenResult": self.open_result,
            "JsonExplore": self.explore,
            "JsonCopy": self.copy_result,
            "JsonSource": self.source,
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
            self.command_active.clear()
            self.send_error(message, error)
        else:
            self.command_active.clear()
            self.send({"jsonrpc": "2.0", "id": message["id"], "result": result})
        finally:
            self.command_active.clear()

    def serve(self):
        threading.Thread(target=self.read_messages, daemon=True).start()
        notification_handlers = {
            "picker/event": self.handle_picker_event,
            "document/event": self.handle_document_event,
            "panel/event": self.handle_panel_event,
        }
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
                elif method in notification_handlers:
                    notification_handlers[method](message)
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
