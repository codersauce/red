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
else:
    sys.stdout.buffer.write(b'{"ok":true}\n')
'''


class PluginProcess:
    def __init__(self, fake_jq):
        self.process = subprocess.Popen(
            [sys.executable, str(PLUGIN), sys.executable, str(fake_jq)],
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

    def expect_status(self, text=None):
        request = self.expect_host_request("red/StatuslineSet")
        if text is not None and request["params"]["text"] != text:
            raise AssertionError(request)
        self.reply(request)

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
        cls.fake_jq = Path(cls.tempdir.name) / "fake_jq.py"
        cls.fake_jq.write_text(FAKE_JQ, encoding="utf-8")

    @classmethod
    def tearDownClass(cls):
        cls.tempdir.cleanup()

    def complete_query(self, plugin, command_id, document, output, create_panel):
        snapshot = plugin.expect_host_request("red/DocumentSnapshot")
        plugin.reply(snapshot, document)
        plugin.expect_status("JSON: running")
        if create_panel:
            create = plugin.expect_host_request("red/TextPanelCreate")
            self.assertEqual(create["params"]["id"], "json-workbench")
            plugin.reply(create)
        content = plugin.expect_host_request("red/TextPanelSetContent")
        self.assertEqual(content["params"]["blocks"][1]["text"], output)
        plugin.reply(content)
        plugin.expect_status(f"JSON: ready ({len(output.encode('utf-8'))} B)")
        response = plugin.receive()
        self.assertEqual(response["id"], command_id)
        self.assertEqual(response["result"], {"message": "JSON preview ready"})
        return snapshot

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

            plugin.command("apply", "JsonApply")
            apply = plugin.expect_host_request("red/DocumentApply")
            edit = apply["params"]["edits"][0]
            self.assertEqual(apply["params"]["document_id"], "7")
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
                undo["params"], {"document_id": "7", "transaction_id": "tx-1"}
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

    def test_pinned_source_retry_and_invalidation_rules(self):
        first = {"path": "a.json", "document_id": "7", "revision": "1", "text": "{}"}
        second = {"path": "a.json", "revision": "2", "text": "{\"a\":1}"}
        with PluginProcess(self.fake_jq) as plugin:
            plugin.command("first", "JsonQuery", "stream")
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

    def test_jq_failures_are_bounded_and_reaped(self):
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

    def test_shutdown_interrupts_a_running_query(self):
        plugin = PluginProcess(self.fake_jq)
        pid_file = Path(self.tempdir.name) / "slow.pid"
        plugin.command("query", "JsonQuery", f"slow:{pid_file}")
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
        if os.name == "posix":
            with self.assertRaises(ProcessLookupError):
                os.kill(child_pid, 0)

    @unittest.skipUnless(shutil.which("jq"), "jq is not installed")
    def test_real_jq_smoke(self):
        runner = WORKBENCH.JsonWorkbench([shutil.which("jq")], jq_timeout=2)
        self.assertEqual(runner.run_jq(".answer", b'{"answer":42}'), "42\n")


if __name__ == "__main__":
    unittest.main()
