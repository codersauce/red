import importlib.util
import json
import os
import queue
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PLUGIN = ROOT / "examples/rpc-json-workbench/plugin.py"
SPEC = importlib.util.spec_from_file_location("rpc_json_workbench", PLUGIN)
WORKBENCH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(WORKBENCH)
EOF = object()


FAKE_JQ = r'''import os
import sys
import time

query = sys.argv[-1]
source = sys.stdin.buffer.read()
if query == "stream":
    sys.stdout.buffer.write(b"1\n2\n")
elif query == "empty":
    pass
elif query == "invalid":
    sys.stderr.write("jq: compile error\n")
    raise SystemExit(3)
elif query == "overflow-output":
    sys.stdout.buffer.write(b"x" * (1024 * 1024 + 1))
elif query == "overflow-diagnostics":
    sys.stderr.buffer.write(b"x" * (64 * 1024 + 1))
    raise SystemExit(4)
elif query == "slow":
    time.sleep(10)
elif query.startswith("slow:"):
    marker_path = query[5:]
    with open(marker_path + ".tmp", "w", encoding="utf-8") as marker:
        marker.write(str(os.getpid()))
    os.replace(marker_path + ".tmp", marker_path)
    time.sleep(10)
elif query == "echo":
    sys.stdout.buffer.write(source)
elif query == "wrap":
    sys.stdout.buffer.write(b"<" + source + b">")
elif query == "nested":
    sys.stdout.buffer.write(b'{"a.b":{"quote\\"key":[1,2]},"dup":1,"dup":2,"big":9007199254740993}\n{"second":[]}\n')
else:
    sys.stdout.buffer.write(b'{"ok":true}\n')
'''


def process_is_running(pid):
    if os.name == "nt":
        result = subprocess.run(
            ["tasklist", "/FI", f"PID eq {pid}", "/FO", "CSV", "/NH"],
            capture_output=True,
            text=True,
            check=False,
        )
        return f'"{pid}"' in result.stdout
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


class PluginProcess:
    plugin_path = PLUGIN

    def __init__(self, fake_jq):
        self.process = subprocess.Popen(
            [
                sys.executable,
                self.plugin_path.name,
                sys.executable,
                str(fake_jq),
            ],
            cwd=self.plugin_path.parent,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            bufsize=1,
        )
        self.output = queue.Queue()
        self.reader = threading.Thread(target=self._read_output, daemon=True)
        self.reader.start()
        self.send("initialize", "initialize", {})
        initialized = self.receive()
        if initialized.get("id") != "initialize":
            raise AssertionError(initialized)
        self.send_notification("initialized", {})

    def _read_output(self):
        try:
            for line in self.process.stdout:
                self.output.put(json.loads(line))
        except Exception as error:
            self.output.put(error)
        finally:
            self.output.put(EOF)

    def send(self, request_id, method, params):
        self._write(
            {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}
        )

    def send_notification(self, method, params):
        self._write({"jsonrpc": "2.0", "method": method, "params": params})

    def _write(self, message):
        self.process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
        self.process.stdin.flush()

    def receive(self, timeout=2):
        message = self.output.get(timeout=timeout)
        if message is EOF:
            raise AssertionError(f"plugin exited early: {self.process.stderr.read()}")
        if isinstance(message, Exception):
            raise message
        return message

    def command(self, request_id, name, raw_args=""):
        self.send(
            request_id,
            "command/execute",
            {"name": name, "args": [], "raw_args": raw_args},
        )

    def expect_host_request(self, method):
        request = self.receive()
        if request.get("method") != method:
            raise AssertionError(request)
        return request

    def reply(self, request, result=None, error=None):
        response = {"jsonrpc": "2.0", "id": request["id"]}
        if error is None:
            response["result"] = result
        else:
            response["error"] = {"code": error[0], "message": error[1]}
        self._write(response)

    def send_picker_event(self, picker, item_id=None):
        params = {"id": picker["params"]["id"]}
        if item_id is None:
            params["event"] = "cancelled"
        else:
            params["event"] = "selected"
            params["item"] = next(
                item for item in picker["params"]["items"] if item["id"] == item_id
            )
        self.send_notification("picker/event", params)

    def expect_storage(self, method, key):
        request = self.expect_host_request(method)
        if request["params"]["key"] != key:
            raise AssertionError(request)
        return request

    def expect_status(self, text=None):
        request = self.expect_host_request("red/StatuslineSet")
        if text is not None and request["params"]["text"] != text:
            raise AssertionError(request)
        self.reply(request)

    def load_runtime(self, config=None, preferences=None, history=None):
        request = self.expect_host_request("red/ConfigGet")
        self.reply(request, config)
        request = self.expect_host_request("red/StorageGet")
        if request["params"] != {"key": "query-preferences"}:
            raise AssertionError(request)
        self.reply(
            request,
            {"history_enabled": False} if preferences is None else preferences,
        )
        request = self.expect_host_request("red/StorageGet")
        if request["params"] != {"key": "query-history"}:
            raise AssertionError(request)
        self.reply(request, history)

    def stop(self):
        if self.process.poll() is None:
            self.send("shutdown", "shutdown", None)
            response = self.receive()
            if response != {"jsonrpc": "2.0", "id": "shutdown", "result": None}:
                raise AssertionError(response)
            self.send_notification("exit", None)
            self.process.wait(timeout=2)
        self.reader.join(timeout=1)
        stderr = self.process.stderr.read()
        self.close_pipes()
        if stderr:
            raise AssertionError(stderr)

    def close_pipes(self):
        self.process.stdin.close()
        self.process.stdout.close()
        self.process.stderr.close()

    def __enter__(self):
        return self

    def __exit__(self, exc_type, _exc, _traceback):
        if exc_type is None:
            self.stop()
        elif self.process.poll() is None:
            self.process.kill()
            self.process.wait()
            self.reader.join(timeout=1)
            self.close_pipes()


class JsonWorkbenchTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tempdir = tempfile.TemporaryDirectory()
        root = Path(cls.tempdir.name)
        package = root / "JSON Workbench package"
        package.mkdir()
        PluginProcess.plugin_path = package / "plugin.py"
        shutil.copy2(PLUGIN, PluginProcess.plugin_path)
        tools = root / "tools with spaces"
        tools.mkdir()
        cls.fake_jq = tools / "fake jq.py"
        cls.fake_jq.write_text(FAKE_JQ, encoding="utf-8")

    @classmethod
    def tearDownClass(cls):
        PluginProcess.plugin_path = PLUGIN
        cls.tempdir.cleanup()

    def assert_process_reaped(self, pid):
        deadline = time.monotonic() + 2
        while process_is_running(pid) and time.monotonic() < deadline:
            time.sleep(0.01)
        self.assertFalse(process_is_running(pid), f"process {pid} is still running")

    def complete_query(self, plugin, command_id, document, output, create_panel):
        snapshot = plugin.expect_host_request("red/DocumentSnapshot")
        plugin.reply(snapshot, document)
        plugin.expect_status("JSON: running")
        if create_panel:
            create = plugin.expect_host_request("red/TextPanelCreate")
            self.assertEqual(create["params"]["id"], "json-workbench")
            self.assertEqual(create["params"]["actions"], WORKBENCH.PANEL_ACTIONS)
            plugin.reply(create)
        content = plugin.expect_host_request("red/TextPanelSetContent")
        self.assertEqual(content["params"]["blocks"][1]["text"], output)
        plugin.reply(content)
        plugin.expect_status(f"JSON: ready ({len(output.encode('utf-8'))} B)")
        response = plugin.receive()
        self.assertEqual(response["id"], command_id)
        self.assertEqual(response["result"], {"message": "JSON preview ready"})
        return snapshot

    def open_workbench(self, plugin, source, query_id="9"):
        plugin.command("workbench", "JsonWorkbench")
        snapshot = plugin.expect_host_request("red/DocumentSnapshot")
        self.assertEqual(snapshot["params"], {"include_document_id": True})
        plugin.reply(snapshot, source)
        plugin.load_runtime()
        panel = plugin.expect_host_request("red/TextPanelCreate")
        plugin.reply(panel)
        help_content = plugin.expect_host_request("red/TextPanelSetContent")
        self.assertIn(":JsonRun", help_content["params"]["blocks"][1]["text"])
        plugin.reply(help_content)
        created = plugin.expect_host_request("red/DocumentCreate")
        self.assertEqual(created["params"], {"text": "."})
        plugin.reply(created, {"document_id": query_id})
        shown = plugin.expect_host_request("red/DocumentShow")
        self.assertEqual(shown["params"], {"document_id": query_id})
        plugin.reply(shown)
        plugin.expect_status("JSON: query")
        self.assertEqual(
            plugin.receive()["result"],
            {"message": "JSON query document ready"},
        )

    def enable_live(self, plugin, source_id="7", query_id="9"):
        plugin.command("live", "JsonLive")
        for document_id in (source_id, query_id):
            watch = plugin.expect_host_request("red/DocumentWatch")
            self.assertEqual(watch["params"], {"document_id": document_id})
            plugin.reply(watch, {"revision": "1"})
        plugin.expect_status("JSON: live")
        self.assertEqual(
            plugin.receive()["result"],
            {"message": "JSON live preview scheduled"},
        )

    def complete_background_query(
        self, plugin, query, source, output, query_snapshot=None
    ):
        if query_snapshot is None:
            query_snapshot = plugin.expect_host_request("red/DocumentSnapshot")
        self.assertEqual(query_snapshot["params"], {"document_id": "9"})
        plugin.reply(query_snapshot, query)
        source_snapshot = plugin.expect_host_request("red/DocumentSnapshot")
        self.assertEqual(source_snapshot["params"], {"document_id": "7"})
        plugin.reply(source_snapshot, source)
        plugin.expect_status("JSON: running")
        content = plugin.expect_host_request("red/TextPanelSetContent")
        self.assertEqual(content["params"]["blocks"][1]["text"], output)
        plugin.reply(content)
        plugin.expect_status(f"JSON: ready ({len(output.encode('utf-8'))} B)")

    def test_end_position_uses_red_unicode_scalar_coordinates(self):
        for text, expected in [
            ("", {"line": 0, "character": 0}),
            ("plain", {"line": 0, "character": 5}),
            ("😀e\u0301", {"line": 0, "character": 3}),
            ("one\r\ntwo", {"line": 1, "character": 3}),
            ("one\n", {"line": 1, "character": 0}),
        ]:
            with self.subTest(text=text):
                self.assertEqual(WORKBENCH.end_position(text), expected)

    def test_json_explorer_preserves_stream_structure_and_bounds_children(self):
        roots = WORKBENCH.parse_json_stream(
            '{"dup":1,"dup":2,"big":9007199254740993}\n[true]\n'
        )
        self.assertEqual(len(roots), 2)
        self.assertIsInstance(roots[0], WORKBENCH.JsonObject)
        self.assertEqual(
            roots[0].pairs,
            [("dup", 1), ("dup", 2), ("big", 9007199254740993)],
        )
        self.assertEqual(WORKBENCH.parse_json_stream(" \r\n\t"), [])
        with self.assertRaisesRegex(WORKBENCH.OperationError, "64 levels"):
            WORKBENCH.parse_json_stream("[" * 65 + "0" + "]" * 65)

        runner = WORKBENCH.JsonWorkbench(["jq"])
        runner.result_context = {"query": ".", "path": None, "revision": "1"}
        runner.result_generation = 3
        runner.explore_roots = roots
        items, actions = runner.explorer_items(3, (("root", 0),))
        duplicates = [item for item in items if item["label"] == '"dup"']
        self.assertEqual(len(duplicates), 2)
        self.assertTrue(
            all("not addressable" in item["detail"] for item in duplicates)
        )
        self.assertTrue(all(item["id"] not in actions for item in duplicates))

        runner.explore_roots = [
            list(range(WORKBENCH.EXPLORE_CHILD_LIMIT + 2))
        ]
        items, actions = runner.explorer_items(3, (("root", 0),))
        self.assertEqual(len(items), WORKBENCH.EXPLORE_CHILD_LIMIT + 3)
        self.assertEqual(items[-1]["label"], "… 2 more values")
        self.assertNotIn("index-1000", actions)

        runner.explore_roots = []
        self.assertEqual(
            runner.explorer_items(3, ()),
            ([{"id": "empty", "label": "No JSON roots"}], {}),
        )

    def test_json_explorer_builds_escaped_stream_paths(self):
        path = (("root", 1), ("key", 'a"bé\n'), ("index", 3))
        self.assertEqual(
            WORKBENCH.json_breadcrumb(2, path), '$<2>["a\\"bé\\n"][3]'
        )
        self.assertEqual(
            WORKBENCH.json_path_query(".items | map(.)", 2, path),
            '(([(.items | map(.))][1]) | .["a\\"b\\u00e9\\n"]) | .[3]',
        )

    def test_query_apply_undo_and_close_preserve_exact_text(self):
        source = "😀e\u0301\r\nlast"
        document = {
            "path": "data.json",
            "document_id": "7",
            "revision": "11",
            "text": source,
        }
        with PluginProcess(self.fake_jq) as plugin:
            plugin.command("query", "JsonQuery", "stream")
            plugin.load_runtime()
            snapshot = plugin.expect_host_request("red/DocumentSnapshot")
            self.assertEqual(snapshot["params"], {"include_document_id": True})
            plugin.send_notification(
                "event/notify", {"name": "file:saved", "payload": {}}
            )
            plugin.reply(snapshot, document)
            plugin.expect_status("JSON: running")
            create = plugin.expect_host_request("red/TextPanelCreate")
            plugin.reply(create)
            content = plugin.expect_host_request("red/TextPanelSetContent")
            self.assertEqual(content["params"]["blocks"][1]["text"], "1\n2\n")
            plugin.reply(content)
            plugin.expect_status("JSON: ready (4 B)")
            self.assertEqual(plugin.receive()["id"], "query")

            plugin.command("copy", "JsonCopy")
            copied = plugin.expect_host_request("red/ClipboardWrite")
            self.assertEqual(copied["params"], {"text": "1\n2\n"})
            plugin.reply(copied)
            plugin.expect_status("JSON: copied")
            self.assertEqual(
                plugin.receive()["result"], {"message": "JSON result copied"}
            )

            plugin.command("apply", "JsonApply")
            apply = plugin.expect_host_request("red/DocumentApply")
            edit = apply["params"]["edits"][0]
            self.assertEqual(apply["params"]["document_id"], "7")
            self.assertIs(apply["params"]["allow_inactive"], True)
            self.assertEqual(apply["params"]["expected_revision"], "11")
            self.assertEqual(edit["range"]["end"], {"line": 1, "character": 4})
            self.assertEqual(edit["expected_text"], source)
            self.assertEqual(edit["text"], "1\n2\n")
            plugin.reply(apply, {"transaction_id": "tx-1", "revision": "12"})
            plugin.expect_status("JSON: applied")
            self.assertEqual(
                plugin.receive()["result"], {"message": "JSON result applied @ 12"}
            )

            plugin.command("undo", "JsonUndo")
            undo = plugin.expect_host_request("red/DocumentUndo")
            self.assertEqual(
                undo["params"],
                {
                    "document_id": "7",
                    "allow_inactive": True,
                    "transaction_id": "tx-1",
                },
            )
            plugin.reply(undo, {"revision": "13"})
            plugin.expect_status("JSON: undone")
            self.assertEqual(
                plugin.receive()["result"], {"message": "JSON result undone @ 13"}
            )

            plugin.command("close", "JsonClose")
            dispose = plugin.expect_host_request("red/TextPanelDispose")
            plugin.reply(dispose)
            clear = plugin.expect_host_request("red/StatuslineClear")
            plugin.reply(clear)
            self.assertIsNone(plugin.receive()["result"])

    def test_panel_copy_and_close_actions_are_notifications(self):
        document = {
            "path": "data.json",
            "document_id": "7",
            "revision": "1",
            "text": "{}",
        }
        with PluginProcess(self.fake_jq) as plugin:
            plugin.command("query", "JsonQuery", "stream")
            plugin.load_runtime()
            self.complete_query(plugin, "query", document, "1\n2\n", True)

            plugin.send_notification(
                "panel/event", {"id": "json-workbench", "action": "copy"}
            )
            copied = plugin.expect_host_request("red/ClipboardWrite")
            self.assertEqual(copied["params"], {"text": "1\n2\n"})
            plugin.reply(copied)
            plugin.expect_status("JSON: copied")

            plugin.send_notification(
                "panel/event", {"id": "json-workbench", "action": "close"}
            )
            dispose = plugin.expect_host_request("red/TextPanelDispose")
            self.assertEqual(dispose["params"], {"id": "json-workbench"})
            plugin.reply(dispose)
            clear = plugin.expect_host_request("red/StatuslineClear")
            plugin.reply(clear)

    def test_explorer_navigates_nested_streams_and_rejects_stale_actions(self):
        output = (
            '{"a.b":{"quote\\"key":[1,2]},"dup":1,"dup":2,'
            '"big":9007199254740993}\n{"second":[]}\n'
        )
        document = {
            "path": "data.json",
            "document_id": "7",
            "revision": "1",
            "text": "{}",
        }
        with PluginProcess(self.fake_jq) as plugin:
            plugin.command("query", "JsonQuery", "nested")
            plugin.load_runtime()
            self.complete_query(plugin, "query", document, output, True)

            plugin.command("explore", "JsonExplore")
            panel = plugin.expect_host_request("red/TextPanelSetContent")
            self.assertIn(
                "Inspect: stream · 2 roots", panel["params"]["blocks"][0]["text"]
            )
            self.assertEqual(panel["params"]["blocks"][1]["text"], output)
            plugin.reply(panel)
            roots = plugin.expect_host_request("red/PickerOpen")
            self.assertEqual(
                [item["label"] for item in roots["params"]["items"]],
                ["Root 1", "Root 2"],
            )
            plugin.reply(roots)
            self.assertIsNone(plugin.receive()["result"])

            plugin.send_picker_event(roots, "root-0")
            panel = plugin.expect_host_request("red/TextPanelSetContent")
            self.assertIn("Inspect: $<1>", panel["params"]["blocks"][0]["text"])
            plugin.reply(panel)
            root = plugin.expect_host_request("red/PickerOpen")
            duplicates = [
                item for item in root["params"]["items"] if item["label"] == '"dup"'
            ]
            self.assertEqual(len(duplicates), 2)
            self.assertTrue(
                all("not addressable" in item["detail"] for item in duplicates)
            )
            plugin.reply(root)

            plugin.send_picker_event(root, "key-0")
            panel = plugin.expect_host_request("red/TextPanelSetContent")
            self.assertIn(
                'Inspect: $<1>["a.b"]', panel["params"]["blocks"][0]["text"]
            )
            plugin.reply(panel)
            nested = plugin.expect_host_request("red/PickerOpen")
            key = next(
                item for item in nested["params"]["items"] if item["id"] == "key-0"
            )
            self.assertEqual(key["label"], '"quote\\"key"')
            plugin.reply(nested)

            plugin.send_picker_event(nested, "key-0")
            panel = plugin.expect_host_request("red/TextPanelSetContent")
            self.assertIn(
                'Inspect: $<1>["a.b"]["quote\\"key"]',
                panel["params"]["blocks"][0]["text"],
            )
            plugin.reply(panel)
            array = plugin.expect_host_request("red/PickerOpen")
            self.assertEqual(
                [item["label"] for item in array["params"]["items"][-2:]],
                ["[0]", "[1]"],
            )
            run = next(
                item for item in array["params"]["items"] if item["id"] == "run-path"
            )
            self.assertEqual(
                run["detail"],
                '(([(nested)][0]) | .["a.b"]) | .["quote\\"key"]',
            )
            plugin.reply(array)

            plugin.send_picker_event(array)
            plugin.command("reopen-explorer", "JsonExplore")
            panel = plugin.expect_host_request("red/TextPanelSetContent")
            self.assertIn(
                'Inspect: $<1>["a.b"]["quote\\"key"]',
                panel["params"]["blocks"][0]["text"],
            )
            plugin.reply(panel)
            reopened = plugin.expect_host_request("red/PickerOpen")
            self.assertEqual(
                [item["label"] for item in reopened["params"]["items"][-2:]],
                ["[0]", "[1]"],
            )
            plugin.reply(reopened)
            self.assertIsNone(plugin.receive()["result"])

            plugin.send_picker_event(reopened, "run-path")
            selected = plugin.expect_host_request("red/DocumentSnapshot")
            self.assertEqual(selected["params"], {"document_id": "7"})
            plugin.reply(selected, document)
            plugin.expect_status("JSON: running")
            content = plugin.expect_host_request("red/TextPanelSetContent")
            self.assertEqual(
                content["params"]["blocks"][1]["text"], '{"ok":true}\n'
            )
            plugin.reply(content)
            plugin.expect_status("JSON: ready (12 B)")
            time.sleep(0.01)

            plugin.send_picker_event(reopened, "run-path")
            plugin.command("copy", "JsonCopy")
            copied = plugin.expect_host_request("red/ClipboardWrite")
            self.assertEqual(copied["params"], {"text": '{"ok":true}\n'})
            plugin.reply(copied)
            plugin.expect_status("JSON: copied")
            self.assertEqual(
                plugin.receive()["result"], {"message": "JSON result copied"}
            )

    def test_selected_source_uses_captured_range_and_revision(self):
        text = "before\r\n😀e\u0301\r\nafter"
        selected = "😀e\u0301\r\n"
        selection_range = {
            "start": {"line": 1, "character": 0},
            "end": {"line": 2, "character": 0},
        }
        document = {
            "path": "data.json",
            "document_id": "7",
            "revision": "11",
            "text": text,
            "selection": {"range": selection_range, "text": selected},
        }
        output = f"<{selected}>"
        with PluginProcess(self.fake_jq) as plugin:
            plugin.command("selection", "JsonSelection")
            snapshot = plugin.expect_host_request("red/DocumentSnapshot")
            self.assertEqual(
                snapshot["params"],
                {"include_document_id": True, "include_selection": True},
            )
            plugin.reply(snapshot, document)
            plugin.load_runtime()
            create = plugin.expect_host_request("red/TextPanelCreate")
            plugin.reply(create)
            help_content = plugin.expect_host_request("red/TextPanelSetContent")
            plugin.reply(help_content)
            created = plugin.expect_host_request("red/DocumentCreate")
            plugin.reply(created, {"document_id": "9"})
            shown = plugin.expect_host_request("red/DocumentShow")
            plugin.reply(shown)
            plugin.expect_status("JSON: query")
            self.assertEqual(plugin.receive()["id"], "selection")

            plugin.command("query", "JsonQuery", "wrap")
            source = plugin.expect_host_request("red/DocumentSnapshot")
            self.assertEqual(source["params"], {"document_id": "7"})
            plugin.reply(source, document)
            plugin.expect_status("JSON: running")
            content = plugin.expect_host_request("red/TextPanelSetContent")
            self.assertEqual(content["params"]["blocks"][1]["text"], output)
            plugin.reply(content)
            plugin.expect_status(f"JSON: ready ({len(output.encode('utf-8'))} B)")
            self.assertEqual(plugin.receive()["id"], "query")

            plugin.command("stale", "JsonApply")
            apply = plugin.expect_host_request("red/DocumentApply")
            self.assertEqual(apply["params"]["expected_revision"], "11")
            self.assertEqual(
                apply["params"]["edits"],
                [
                    {
                        "range": selection_range,
                        "text": output,
                        "expected_text": selected,
                    }
                ],
            )
            plugin.reply(apply, error=(-32002, "document revision changed"))
            retired = plugin.expect_host_request("red/TextPanelSetContent")
            plugin.reply(retired)
            plugin.expect_status("JSON: error")
            self.assertIn("revision changed", plugin.receive()["error"]["message"])

            plugin.command("changed", "JsonQuery", "wrap")
            changed = plugin.expect_host_request("red/DocumentSnapshot")
            self.assertEqual(changed["params"], {"document_id": "7"})
            plugin.reply(
                changed,
                {
                    **document,
                    "revision": "12",
                    "text": f"changed\r\n{selected}after",
                },
            )
            retired = plugin.expect_host_request("red/TextPanelSetContent")
            self.assertIn(
                "close and reopen", retired["params"]["blocks"][0]["text"]
            )
            plugin.reply(retired)
            plugin.expect_status("JSON: error")
            self.assertIn("close and reopen", plugin.receive()["error"]["message"])

    def test_json_selection_requires_context_without_pinning_source(self):
        document = {
            "path": "data.json",
            "document_id": "7",
            "revision": "1",
            "text": "{}",
            "selection": None,
        }
        with PluginProcess(self.fake_jq) as plugin:
            plugin.command("selection", "JsonSelection")
            snapshot = plugin.expect_host_request("red/DocumentSnapshot")
            plugin.reply(snapshot, document)
            plugin.expect_status("JSON: error")
            self.assertIn(
                "no contiguous visual selection",
                plugin.receive()["error"]["message"],
            )

            plugin.command("query", "JsonQuery", "stream")
            plugin.load_runtime()
            source = self.complete_query(
                plugin, "query", document, "1\n2\n", create_panel=True
            )
            self.assertEqual(source["params"], {"include_document_id": True})

    def test_pinned_source_retry_and_invalidation_rules(self):
        first = {"path": "a.json", "document_id": "7", "revision": "1", "text": "{}"}
        second = {"path": "a.json", "revision": "2", "text": "{\"a\":1}"}
        with PluginProcess(self.fake_jq) as plugin:
            plugin.command("first", "JsonQuery", "stream")
            plugin.load_runtime()
            self.complete_query(plugin, "first", first, "1\n2\n", True)

            plugin.command("switched", "JsonApply")
            apply = plugin.expect_host_request("red/DocumentApply")
            plugin.reply(apply, error=(-32003, "document is not current"))
            plugin.expect_status("JSON: error")
            self.assertIn("not current", plugin.receive()["error"]["message"])

            plugin.command("retry", "JsonApply")
            retry = plugin.expect_host_request("red/DocumentApply")
            plugin.reply(retry, {"transaction_id": "tx-2", "revision": "2"})
            plugin.expect_status("JSON: applied")
            self.assertEqual(plugin.receive()["id"], "retry")

            plugin.command("second", "JsonQuery", "stream")
            snapshot = self.complete_query(plugin, "second", second, "1\n2\n", False)
            self.assertEqual(snapshot["params"], {"document_id": "7"})

            plugin.command("stale", "JsonApply")
            stale = plugin.expect_host_request("red/DocumentApply")
            plugin.reply(stale, error=(-32002, "document revision changed"))
            retired = plugin.expect_host_request("red/TextPanelSetContent")
            self.assertIn("revision changed", retired["params"]["blocks"][0]["text"])
            plugin.reply(retired)
            plugin.expect_status("JSON: error")
            self.assertIn("revision changed", plugin.receive()["error"]["message"])

            plugin.command("invalidated", "JsonApply")
            plugin.expect_status("JSON: error")
            self.assertIn("no applicable preview", plugin.receive()["error"]["message"])

            third = {**second, "revision": "3"}
            plugin.command("third", "JsonQuery", "stream")
            self.complete_query(plugin, "third", third, "1\n2\n", False)

            plugin.command("closed-apply", "JsonApply")
            closed_apply = plugin.expect_host_request("red/DocumentApply")
            plugin.reply(closed_apply, error=(-32001, "document is no longer open"))
            retired = plugin.expect_host_request("red/TextPanelSetContent")
            self.assertIn("no longer open", retired["params"]["blocks"][0]["text"])
            plugin.reply(retired)
            plugin.expect_status("JSON: error")
            self.assertIn("no longer open", plugin.receive()["error"]["message"])

            plugin.command("closed-query", "JsonQuery", "stream")
            closed = plugin.expect_host_request("red/DocumentSnapshot")
            self.assertEqual(closed["params"], {"document_id": "7"})
            plugin.reply(closed, error=(-32001, "document is no longer open"))
            retired = plugin.expect_host_request("red/TextPanelSetContent")
            self.assertIn("no longer open", retired["params"]["blocks"][0]["text"])
            plugin.reply(retired)
            plugin.expect_status("JSON: error")
            self.assertIn("no longer open", plugin.receive()["error"]["message"])

            plugin.command("close", "JsonClose")
            dispose = plugin.expect_host_request("red/TextPanelDispose")
            plugin.reply(dispose)
            clear = plugin.expect_host_request("red/StatuslineClear")
            plugin.reply(clear)
            self.assertIsNone(plugin.receive()["result"])

            plugin.command("repin", "JsonQuery", "stream")
            repin = plugin.expect_host_request("red/DocumentSnapshot")
            self.assertEqual(repin["params"], {"include_document_id": True})
            plugin.reply(repin, error=(-32001, "no current document"))
            plugin.expect_status("JSON: error")
            self.assertIn("no current document", plugin.receive()["error"]["message"])

    def test_failed_query_retires_preview_and_recovers(self):
        document = {
            "path": "a.json",
            "document_id": "7",
            "revision": "1",
            "text": "{}",
        }
        with PluginProcess(self.fake_jq) as plugin:
            plugin.command("valid", "JsonQuery", "stream")
            plugin.load_runtime()
            self.complete_query(plugin, "valid", document, "1\n2\n", True)

            plugin.command("invalid", "JsonQuery", "invalid")
            self.complete_failed_query(plugin, document, "jq: compile error")
            plugin.expect_status("JSON: error")
            self.assertIn("compile error", plugin.receive()["error"]["message"])

            plugin.command("not-applicable", "JsonApply")
            plugin.expect_status("JSON: error")
            self.assertIn("no applicable preview", plugin.receive()["error"]["message"])

            plugin.command("recovered", "JsonQuery", "echo")
            self.complete_query(plugin, "recovered", document, "{}", False)
            plugin.command("unchanged", "JsonApply")
            plugin.expect_status("JSON: unchanged")
            self.assertEqual(
                plugin.receive()["result"],
                {"message": "JSON preview already matches the source"},
            )

    def complete_failed_query(self, plugin, document, error):
        snapshot = plugin.expect_host_request("red/DocumentSnapshot")
        plugin.reply(snapshot, document)
        plugin.expect_status("JSON: running")
        retired = plugin.expect_host_request("red/TextPanelSetContent")
        self.assertEqual(retired["params"]["blocks"][0]["id"], "error")
        self.assertIn(error, retired["params"]["blocks"][0]["text"])
        plugin.reply(retired)

    def test_phase_two_settings_and_history_normalization(self):
        command, timeout, result_limit, presets = WORKBENCH.parse_settings(
            {
                "jq_path": "/opt/bin/jq",
                "timeout_seconds": 2.5,
                "result_limit_bytes": 4096,
                "presets": [{"label": "Active users", "query": ".active"}],
            },
            ["jq"],
            5,
        )
        self.assertEqual(command, ["/opt/bin/jq"])
        self.assertEqual(timeout, 2.5)
        self.assertEqual(result_limit, 4096)
        self.assertEqual(presets, [("Active users", ".active")])
        with self.assertRaisesRegex(WORKBENCH.OperationError, "timeout_seconds"):
            WORKBENCH.parse_settings({"timeout_seconds": 6}, ["jq"], 5)

        history = [".a", "", 7, ".a", ".b", *(f".{i}" for i in range(30))]
        normalized = WORKBENCH.normalize_history(history)
        self.assertEqual(normalized[:2], [".a", ".b"])
        self.assertEqual(len(normalized), WORKBENCH.HISTORY_LIMIT)
        self.assertEqual(WORKBENCH.normalize_history({"query": ".a"}), [])
        self.assertEqual(
            WORKBENCH.query_label('.message == "two  spaces"\n| .value'),
            '.message == "two  spaces" | .value',
        )

    def test_editable_query_result_and_source_workflow(self):
        source = {
            "path": "data.json",
            "document_id": "7",
            "revision": "1",
            "text": "{}",
        }
        output = '{"ok":true}\n'
        with PluginProcess(self.fake_jq) as plugin:
            self.open_workbench(plugin, source)

            plugin.command("workbench-again", "JsonWorkbench")
            snapshot = plugin.expect_host_request("red/DocumentSnapshot")
            self.assertEqual(snapshot["params"], {"document_id": "7"})
            plugin.reply(snapshot, source)
            help_content = plugin.expect_host_request("red/TextPanelSetContent")
            plugin.reply(help_content)
            shown = plugin.expect_host_request("red/DocumentShow")
            self.assertEqual(shown["params"], {"document_id": "9"})
            plugin.reply(shown)
            plugin.expect_status("JSON: query")
            self.assertEqual(
                plugin.receive()["result"],
                {"message": "JSON query document ready"},
            )

            plugin.command("run", "JsonRun")
            query = plugin.expect_host_request("red/DocumentSnapshot")
            self.assertEqual(query["params"], {"document_id": "9"})
            plugin.reply(
                query,
                {
                    "path": None,
                    "revision": "3",
                    "text": ".users | map(select(.active))",
                },
            )
            self.complete_query(plugin, "run", source, output, create_panel=False)

            plugin.command("result", "JsonOpenResult")
            result = plugin.expect_host_request("red/DocumentCreate")
            self.assertEqual(
                result["params"], {"text": output, "syntax": "json"}
            )
            plugin.reply(result, {"document_id": "10"})
            shown = plugin.expect_host_request("red/DocumentShow")
            self.assertEqual(shown["params"], {"document_id": "10"})
            plugin.reply(shown)
            plugin.expect_status("JSON: result")
            self.assertEqual(
                plugin.receive()["result"],
                {"message": "JSON result document ready"},
            )

            plugin.command("source", "JsonSource")
            shown = plugin.expect_host_request("red/DocumentShow")
            self.assertEqual(shown["params"], {"document_id": "7"})
            plugin.reply(shown)
            plugin.expect_status("JSON: source")
            self.assertEqual(
                plugin.receive()["result"],
                {"message": "JSON source document ready"},
            )

            plugin.command("apply", "JsonApply")
            apply = plugin.expect_host_request("red/DocumentApply")
            self.assertEqual(apply["params"]["document_id"], "7")
            plugin.reply(apply, {"transaction_id": "tx-1", "revision": "2"})
            plugin.expect_status("JSON: applied")
            self.assertEqual(plugin.receive()["id"], "apply")

    def test_live_preview_reconciles_setup_and_coalesces_latest_changes(self):
        source = {
            "path": "data.json",
            "document_id": "7",
            "revision": "1",
            "text": "{}",
        }
        query = {"path": None, "revision": "2", "text": "stream"}
        with PluginProcess(self.fake_jq) as plugin:
            self.open_workbench(plugin, source)
            self.enable_live(plugin)

            stale = plugin.expect_host_request("red/DocumentSnapshot")
            self.assertEqual(stale["params"], {"document_id": "9"})
            plugin.send_notification(
                "document/event",
                {"document_id": "9", "event": "changed", "revision": "2"},
            )
            plugin.reply(stale, {"path": None, "revision": "1", "text": "."})
            next_snapshot = plugin.expect_host_request("red/DocumentSnapshot")
            if next_snapshot["params"] == {"document_id": "7"}:
                plugin.reply(next_snapshot, source)
                next_snapshot = plugin.expect_host_request("red/DocumentSnapshot")
            self.complete_background_query(
                plugin, query, source, "1\n2\n", query_snapshot=next_snapshot
            )

            plugin.send_notification(
                "document/event",
                {"document_id": "9", "event": "changed", "revision": "3"},
            )
            invalid = plugin.expect_host_request("red/DocumentSnapshot")
            plugin.reply(
                invalid, {"path": None, "revision": "3", "text": "invalid"}
            )
            self.complete_failed_query(plugin, source, "jq: compile error")
            plugin.expect_status("JSON: error")

            plugin.send_notification(
                "document/event",
                {"document_id": "7", "event": "changed", "revision": "4"},
            )
            plugin.send_notification(
                "document/event",
                {"document_id": "9", "event": "changed", "revision": "5"},
            )
            plugin.send_notification(
                "document/event",
                {"document_id": "7", "event": "changed", "revision": "6"},
            )
            latest_query = {"path": None, "revision": "5", "text": "echo"}
            latest_source = {
                **source,
                "revision": "6",
                "text": '{"n":6}',
            }
            self.complete_background_query(
                plugin, latest_query, latest_source, latest_source["text"]
            )
            with self.assertRaises(queue.Empty):
                plugin.receive(timeout=0.25)

    def test_live_cancel_and_document_close_reap_work(self):
        pid_file = Path(self.tempdir.name) / "live-slow.pid"
        source = {
            "path": "data.json",
            "document_id": "7",
            "revision": "1",
            "text": "{}",
        }
        slow_query = {
            "path": None,
            "revision": "1",
            "text": f"slow:{pid_file}",
        }
        with PluginProcess(self.fake_jq) as plugin:
            self.open_workbench(plugin, source)
            self.enable_live(plugin)
            query = plugin.expect_host_request("red/DocumentSnapshot")
            plugin.reply(query, slow_query)
            snapshot = plugin.expect_host_request("red/DocumentSnapshot")
            plugin.reply(snapshot, source)
            plugin.expect_status("JSON: running")
            deadline = time.monotonic() + 2
            while not pid_file.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertTrue(pid_file.exists(), "controlled jq child did not start")
            child_pid = int(pid_file.read_text(encoding="utf-8"))

            plugin.command("cancel", "JsonCancel")
            plugin.expect_status("JSON: cancelled")
            self.assertEqual(
                plugin.receive()["result"], {"message": "JSON preview cancelled"}
            )
            self.assert_process_reaped(child_pid)

            plugin.send_notification(
                "document/event", {"document_id": "7", "event": "closed"}
            )
            unwatched = set()
            for _ in range(2):
                unwatch = plugin.expect_host_request("red/DocumentUnwatch")
                unwatched.add(unwatch["params"]["document_id"])
                plugin.reply(unwatch)
            self.assertEqual(unwatched, {"7", "9"})
            retired = plugin.expect_host_request("red/TextPanelSetContent")
            self.assertIn("was closed", retired["params"]["blocks"][0]["text"])
            plugin.reply(retired)
            plugin.expect_status("JSON: closed")

            plugin.command("close", "JsonClose")
            dispose = plugin.expect_host_request("red/TextPanelDispose")
            plugin.reply(dispose)
            clear = plugin.expect_host_request("red/StatuslineClear")
            plugin.reply(clear)
            self.assertIsNone(plugin.receive()["result"])

    def test_query_picker_selects_a_preset_and_records_history(self):
        document = {
            "path": "data.json",
            "document_id": "7",
            "revision": "1",
            "text": "{}",
        }
        config = {"presets": [{"label": "Stream", "query": "stream"}]}
        with PluginProcess(self.fake_jq) as plugin:
            plugin.command("queries", "JsonQueries")
            snapshot = plugin.expect_host_request("red/DocumentSnapshot")
            self.assertEqual(snapshot["params"], {"include_document_id": True})
            plugin.reply(snapshot, document)
            plugin.load_runtime(
                config=config,
                preferences={"history_enabled": True},
                history=[],
            )
            picker = plugin.expect_host_request("red/PickerOpen")
            plugin.reply(picker)
            self.assertIsNone(plugin.receive()["result"])

            plugin.send_notification(
                "event/notify", {"name": "file:saved", "payload": {}}
            )
            plugin.send_picker_event(picker, "preset-0")
            selected = plugin.expect_host_request("red/DocumentSnapshot")
            self.assertEqual(selected["params"], {"document_id": "7"})
            plugin.reply(selected, document)
            plugin.expect_status("JSON: running")
            create = plugin.expect_host_request("red/TextPanelCreate")
            plugin.reply(create)
            content = plugin.expect_host_request("red/TextPanelSetContent")
            self.assertEqual(content["params"]["blocks"][1]["text"], "1\n2\n")
            plugin.reply(content)
            saved = plugin.expect_storage("red/StorageSet", "query-history")
            self.assertEqual(saved["params"]["value"], ["stream"])
            plugin.reply(saved)
            plugin.expect_status("JSON: ready (4 B)")

    def test_query_history_survives_restart_and_picker_actions(self):
        document = {
            "path": "data.json",
            "document_id": "7",
            "revision": "1",
            "text": "{}",
        }
        persisted = ["stream", ".old"]
        config = {"presets": [{"label": "Active", "query": ".active"}]}
        with PluginProcess(self.fake_jq) as plugin:
            plugin.command("queries", "JsonQueries")
            snapshot = plugin.expect_host_request("red/DocumentSnapshot")
            plugin.reply(snapshot, document)
            plugin.load_runtime(
                config=config,
                preferences={"history_enabled": True},
                history=persisted,
            )
            picker = plugin.expect_host_request("red/PickerOpen")
            ids = [item["id"] for item in picker["params"]["items"]]
            self.assertEqual(
                ids,
                [
                    "preset-0",
                    "history-0",
                    "history-1",
                    "history-clear",
                    "history-toggle",
                ],
            )
            plugin.reply(picker)
            self.assertIsNone(plugin.receive()["result"])
            plugin.send_picker_event(picker)

            plugin.command("clear-picker", "JsonQueries")
            picker = plugin.expect_host_request("red/PickerOpen")
            plugin.reply(picker)
            self.assertIsNone(plugin.receive()["result"])
            plugin.send_picker_event(picker, "history-clear")
            deleted = plugin.expect_storage("red/StorageDelete", "query-history")
            plugin.reply(deleted)
            plugin.expect_status("JSON: history cleared")

            plugin.command("toggle-picker", "JsonQueries")
            picker = plugin.expect_host_request("red/PickerOpen")
            ids = [item["id"] for item in picker["params"]["items"]]
            self.assertNotIn("history-0", ids)
            self.assertNotIn("history-clear", ids)
            plugin.reply(picker)
            self.assertIsNone(plugin.receive()["result"])
            plugin.send_picker_event(picker, "history-toggle")
            preference = plugin.expect_storage(
                "red/StorageSet", "query-preferences"
            )
            self.assertEqual(
                preference["params"]["value"], {"history_enabled": False}
            )
            plugin.reply(preference)
            plugin.expect_status("JSON: history paused")

            plugin.command("paused-picker", "JsonQueries")
            paused = plugin.expect_host_request("red/PickerOpen")
            toggle = next(
                item for item in paused["params"]["items"]
                if item["id"] == "history-toggle"
            )
            self.assertEqual(toggle["label"], "Resume query history")
            plugin.reply(paused)
            self.assertIsNone(plugin.receive()["result"])
            plugin.send_picker_event(paused)

    def test_picker_failure_is_visible_without_a_command_response(self):
        document = {
            "path": "data.json",
            "document_id": "7",
            "revision": "1",
            "text": "{}",
        }
        config = {"presets": [{"label": "Invalid", "query": "invalid"}]}
        with PluginProcess(self.fake_jq) as plugin:
            plugin.command("queries", "JsonQueries")
            snapshot = plugin.expect_host_request("red/DocumentSnapshot")
            plugin.reply(snapshot, document)
            plugin.load_runtime(config=config)
            picker = plugin.expect_host_request("red/PickerOpen")
            plugin.reply(picker)
            self.assertIsNone(plugin.receive()["result"])
            plugin.send_picker_event(picker, "preset-0")
            selected = plugin.expect_host_request("red/DocumentSnapshot")
            plugin.reply(selected, document)
            plugin.expect_status("JSON: running")
            create = plugin.expect_host_request("red/TextPanelCreate")
            plugin.reply(create)
            error = plugin.expect_host_request("red/TextPanelSetContent")
            self.assertIn("compile error", error["params"]["blocks"][0]["text"])
            plugin.reply(error)
            plugin.expect_status("JSON: error")

    def test_storage_failure_leaves_query_applicable(self):
        document = {
            "path": "data.json",
            "document_id": "7",
            "revision": "1",
            "text": "{}",
        }
        with PluginProcess(self.fake_jq) as plugin:
            plugin.command("query", "JsonQuery", "stream")
            plugin.load_runtime(preferences={"history_enabled": True}, history=[])
            snapshot = plugin.expect_host_request("red/DocumentSnapshot")
            plugin.reply(snapshot, document)
            plugin.expect_status("JSON: running")
            create = plugin.expect_host_request("red/TextPanelCreate")
            plugin.reply(create)
            content = plugin.expect_host_request("red/TextPanelSetContent")
            plugin.reply(content)
            saved = plugin.expect_storage("red/StorageSet", "query-history")
            plugin.reply(saved, error=(-32603, "preferences unavailable"))
            plugin.expect_status("JSON: ready (4 B; history not saved)")
            self.assertEqual(plugin.receive()["id"], "query")

            plugin.command("apply", "JsonApply")
            apply = plugin.expect_host_request("red/DocumentApply")
            plugin.reply(apply, {"transaction_id": "tx-1", "revision": "2"})
            plugin.expect_status("JSON: applied")
            self.assertEqual(plugin.receive()["id"], "apply")

    def test_history_opt_out_survives_partial_storage_failure(self):
        document = {
            "path": "data.json",
            "document_id": "7",
            "revision": "1",
            "text": "{}",
        }
        output = '{"ok":true}\n'
        with PluginProcess(self.fake_jq) as plugin:
            plugin.command("query", "JsonQuery", ".private")
            config = plugin.expect_host_request("red/ConfigGet")
            plugin.reply(config)
            preferences = plugin.expect_storage(
                "red/StorageGet", "query-preferences"
            )
            plugin.reply(preferences, {"history_enabled": False})
            history = plugin.expect_storage("red/StorageGet", "query-history")
            plugin.reply(history, error=(-32603, "history unavailable"))
            self.complete_query(plugin, "query", document, output, create_panel=True)

            plugin.command("queries", "JsonQueries")
            picker = plugin.expect_host_request("red/PickerOpen")
            self.assertEqual(
                picker["params"]["items"],
                [{"id": "history-toggle", "label": "Resume query history"}],
            )
            plugin.reply(picker)
            self.assertIsNone(plugin.receive()["result"])
            plugin.send_picker_event(picker)

    def test_close_cancels_picker_work_and_ignores_stale_selection(self):
        pid_file = Path(self.tempdir.name) / "picker-slow.pid"
        document = {
            "path": "data.json",
            "document_id": "7",
            "revision": "1",
            "text": "{}",
        }
        config = {
            "presets": [{"label": "Slow", "query": f"slow:{pid_file}"}]
        }
        with PluginProcess(self.fake_jq) as plugin:
            plugin.command("queries", "JsonQueries")
            snapshot = plugin.expect_host_request("red/DocumentSnapshot")
            plugin.reply(snapshot, document)
            plugin.load_runtime(config=config)
            picker = plugin.expect_host_request("red/PickerOpen")
            plugin.reply(picker)
            self.assertIsNone(plugin.receive()["result"])
            plugin.send_picker_event(picker, "preset-0")
            plugin.command("busy", "JsonApply")
            messages = [plugin.receive(), plugin.receive()]
            selected = next(
                message
                for message in messages
                if message.get("method") == "red/DocumentSnapshot"
            )
            busy = next(message for message in messages if message.get("id") == "busy")
            self.assertIn("busy", busy["error"]["message"])
            plugin.reply(selected, document)
            plugin.expect_status("JSON: running")
            deadline = time.monotonic() + 2
            while not pid_file.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertTrue(pid_file.exists(), "controlled jq child did not start")
            child_pid = int(pid_file.read_text(encoding="utf-8"))

            plugin.command("close", "JsonClose")
            clear = plugin.expect_host_request("red/StatuslineClear")
            plugin.reply(clear)
            self.assertIsNone(plugin.receive()["result"])
            self.assert_process_reaped(child_pid)

            plugin.send_picker_event(picker, "preset-0")
            plugin.command("after-close", "JsonApply")
            plugin.expect_status("JSON: error")
            self.assertIn(
                "no applicable preview", plugin.receive()["error"]["message"]
            )

    def test_jq_failures_are_bounded_and_reaped(self):
        runner = WORKBENCH.JsonWorkbench(["red-json-workbench-missing-jq"])
        with self.assertRaisesRegex(
            WORKBENCH.OperationError,
            "install jq or set plugin_config.json-workbench.jq_path",
        ):
            runner.run_jq(".", b"{}")

        command = [sys.executable, str(self.fake_jq)]
        for query_text, expected in [
            ("invalid", "jq: compile error"),
            ("overflow-output", "jq output exceeds 1048576 bytes"),
            ("overflow-diagnostics", "jq diagnostics exceeds 65536 bytes"),
        ]:
            with self.subTest(query=query_text):
                runner = WORKBENCH.JsonWorkbench(command, jq_timeout=1)
                with self.assertRaisesRegex(WORKBENCH.OperationError, expected):
                    runner.run_jq(query_text, b"{}")

        runner = WORKBENCH.JsonWorkbench(command, jq_timeout=0.05)
        started = time.monotonic()
        with self.assertRaisesRegex(WORKBENCH.OperationError, "timed out"):
            runner.run_jq("slow", b"{}")
        self.assertLess(time.monotonic() - started, 1)

        runner = WORKBENCH.JsonWorkbench(command, jq_timeout=1)
        self.assertEqual(runner.run_jq("empty", b"{}"), "")

        runner.result_limit = 3
        with self.assertRaisesRegex(WORKBENCH.OperationError, "exceeds 3 bytes"):
            runner.run_jq("default", b"{}")

    def test_shutdown_interrupts_a_running_query(self):
        plugin = PluginProcess(self.fake_jq)
        pid_file = Path(self.tempdir.name) / "slow.pid"
        plugin.command("query", "JsonQuery", f"slow:{pid_file}")
        plugin.load_runtime()
        snapshot = plugin.expect_host_request("red/DocumentSnapshot")
        plugin.reply(
            snapshot,
            {"path": "data.json", "document_id": "7", "revision": "1", "text": "{}"},
        )
        plugin.expect_status("JSON: running")
        deadline = time.monotonic() + 2
        while not pid_file.exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        self.assertTrue(pid_file.exists(), "controlled jq child did not start")
        child_pid = int(pid_file.read_text(encoding="utf-8"))
        plugin.send("shutdown", "shutdown", None)
        self.assertEqual(
            plugin.receive(), {"jsonrpc": "2.0", "id": "shutdown", "result": None}
        )
        plugin.send_notification("exit", None)
        plugin.process.wait(timeout=2)
        plugin.reader.join(timeout=1)
        self.assertEqual(plugin.process.stderr.read(), "")
        plugin.close_pipes()
        self.assert_process_reaped(child_pid)

    @unittest.skipUnless(shutil.which("jq"), "jq is not installed")
    def test_real_jq_smoke(self):
        runner = WORKBENCH.JsonWorkbench([shutil.which("jq")], jq_timeout=2)
        self.assertEqual(runner.run_jq(".answer", b'{"answer":42}'), "42\n")


if __name__ == "__main__":
    unittest.main()
