import json
import shutil
import subprocess
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PYTHON_PLUGIN = ROOT / "examples/rpc-hello-plugin/plugin.py"
NODE_PLUGIN = ROOT / "examples/rpc-hello-node-plugin/plugin.mjs"


class RpcExampleTestMixin:
    runtime_command = None
    plugin_path = None
    manifest_path = None
    runtime_label = None
    picker_id = None
    panel_id = None

    def test_manifest_activation_matches_initialize_handshake(self):
        manifest_lines = {}
        for raw_line in self.manifest_path.read_text(encoding="utf-8").splitlines():
            line = raw_line.strip()
            if line.startswith(("commands =", "events =", "red_api =")):
                key, value = [part.strip() for part in line.split("=", 1)]
                manifest_lines[key] = json.loads(value)
        self.assertEqual(manifest_lines["red_api"], "^0.19.0")
        output = self.run_raw_messages(
            {"id": "initialize", "method": "initialize", "params": {}},
            {"id": "shutdown", "method": "shutdown"},
            {"method": "exit"},
        )
        init_result = output[0]["result"]
        self.assertEqual(init_result["protocol_version"], 1)
        declared_commands = [entry["name"] for entry in init_result["commands"]]
        self.assertEqual(sorted(declared_commands), sorted(manifest_lines["commands"]))
        self.assertEqual(len(declared_commands), len(set(declared_commands)))
        self.assertEqual(init_result["events"], manifest_lines["events"])

    def run_raw_messages(self, *messages):
        result = subprocess.run(
            [self.runtime_command, str(self.plugin_path)],
            input="".join(
                json.dumps({"jsonrpc": "2.0", **message}) + "\n"
                for message in messages
            ),
            text=True,
            encoding="utf-8",
            capture_output=True,
            timeout=5,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        return [json.loads(line) for line in result.stdout.splitlines()]

    def run_plugin(self, *messages):
        return self.run_raw_messages(
            {"id": "initialize", "method": "initialize", "params": {}},
            {"method": "initialized"},
            *messages,
        )

    def test_interleaved_events_and_requests_preserve_the_host_response(self):
        output = self.run_plugin(
            {"id": "apply", "method": "command/execute", "params": {"name": "RpcApply"}},
            {"method": "event/notify", "params": {"name": "file:saved", "payload": {"file": "notes.txt"}}},
            {"method": "unknown/notification"},
            {"id": "nested", "method": "command/execute", "params": {"name": "RpcHello", "raw_args": "nested"}},
            {"id": "plugin-1", "result": {"revision": "9007199254740993", "document_id": "9007199254740995"}},
            {"id": "plugin-2", "result": {"transaction_id": "9007199254740997", "revision": "9007199254740994"}},
            {"id": "undo", "method": "command/execute", "params": {"name": "RpcUndo"}},
            {"id": "plugin-3", "result": {"revision": "9007199254740996"}},
            {"id": "event", "method": "command/execute", "params": {"name": "RpcLastEvent"}},
            {"id": "shutdown", "method": "shutdown"},
            {"method": "exit"},
        )
        replies = {message["id"]: message for message in output}
        self.assertEqual(
            [message["id"] for message in output],
            ["initialize", "plugin-1", "nested", "plugin-2", "apply", "plugin-3", "undo", "event", "shutdown"],
        )
        self.assertEqual(replies["plugin-1"]["params"], {"include_document_id": True})
        self.assertEqual(replies["plugin-2"]["params"]["document_id"], "9007199254740995")
        self.assertEqual(replies["plugin-2"]["params"]["expected_revision"], "9007199254740993")
        self.assertEqual(
            replies["plugin-3"]["params"],
            {"document_id": "9007199254740995", "transaction_id": "9007199254740997"},
        )
        self.assertEqual(replies["apply"]["result"]["message"], "RPC edit applied @ 9007199254740994")
        self.assertEqual(replies["undo"]["result"]["message"], "RPC edit undone @ 9007199254740996")
        self.assertEqual(replies["event"]["result"]["message"], "Last RPC event: file:saved (notes.txt)")
        self.assertEqual(replies["nested"]["error"]["code"], -32000)
        self.assertIsNone(replies["shutdown"]["result"])

    def test_unicode_scalar_positions_and_counts_match_across_languages(self):
        first_line = 'const icon = "🦀";'
        text = f"{first_line}\nsecond line\n"
        output = self.run_plugin(
            {"id": "doc", "method": "command/execute", "params": {"name": "RpcDocument"}},
            {"id": "plugin-1", "result": {"path": "src/main.ts", "revision": "4", "text": text}},
            {
                "id": "apply-suffix",
                "method": "command/execute",
                "params": {"name": "RpcApply", "raw_args": "// crab"},
            },
            {
                "id": "plugin-2",
                "result": {"document_id": "12", "revision": "4", "text": text},
            },
            {
                "id": "plugin-3",
                "result": {"transaction_id": "tx-1", "revision": "5"},
            },
        )
        replies = {message["id"]: message for message in output}
        self.assertEqual(
            replies["doc"]["result"]["message"],
            "Current document: src/main.ts @ revision 4 (30 chars)",
        )
        self.assertEqual(
            replies["plugin-3"]["params"]["edits"],
            [
                {
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 17},
                    },
                    "text": 'const icon = "🦀"; // crab',
                    "expected_text": first_line,
                }
            ],
        )
        self.assertEqual(replies["apply-suffix"]["result"]["message"], "RPC edit applied @ 5")

    def test_rejects_host_calls_before_initialized(self):
        output = self.run_raw_messages(
            {"id": "initialize", "method": "initialize", "params": {}},
            {"id": "early", "method": "command/execute", "params": {"name": "RpcDocument"}},
        )
        replies = {message["id"]: message for message in output}
        self.assertEqual(
            replies["early"]["error"],
            {"code": -32000, "message": "cannot call host methods before initialized"},
        )

    def test_host_errors_stale_revisions_unmatched_responses_and_eof_are_reported(self):
        for responses, expected in [
            ([{"id": "plugin-1", "error": {"code": -32001, "message": "unavailable"}}], "unavailable"),
            ([{"id": "plugin-1", "error": {"code": -32002, "message": "stale revision"}}], "stale revision"),
            ([{"id": "wrong", "result": None}], "unexpected host response"),
            ([], "host closed stdin while awaiting response"),
        ]:
            with self.subTest(expected=expected):
                follow_up = (
                    ({"id": "hello", "method": "command/execute", "params": {"name": "RpcHello", "raw_args": "world"}},)
                    if responses
                    else ()
                )
                output = self.run_plugin(
                    {"id": "document", "method": "command/execute", "params": {"name": "RpcDocument"}},
                    *responses,
                    *follow_up,
                )
                replies = {message["id"]: message for message in output}
                self.assertEqual(replies["document"]["error"], {"code": -32000, "message": expected})
                if responses:
                    self.assertEqual(
                        replies["hello"]["result"]["message"],
                        f"Hello, world, from {self.runtime_label} RPC!",
                    )

    def test_config_storage_panel_and_statusline_methods_are_exercised(self):
        output = self.run_plugin(
            {"id": "config", "method": "command/execute", "params": {"name": "RpcConfig"}},
            {"id": "plugin-1", "result": {"greeting": "hello"}},
            {"id": "storage", "method": "command/execute", "params": {"name": "RpcStorage"}},
            {"id": "plugin-2", "result": 2},
            {"id": "plugin-3", "result": None},
            {"id": "clear", "method": "command/execute", "params": {"name": "RpcStorageClear"}},
            {"id": "plugin-4", "result": None},
            {"id": "panel", "method": "command/execute", "params": {"name": "RpcPanel"}},
            {"id": "plugin-5", "result": None},
            {"id": "plugin-6", "result": None},
            {"id": "plugin-7", "result": None},
            {"id": "panel-close", "method": "command/execute", "params": {"name": "RpcPanelClose"}},
            {"id": "plugin-8", "result": None},
            {"id": "status", "method": "command/execute", "params": {"name": "RpcStatusline"}},
            {"id": "plugin-9", "result": None},
            {"id": "status-clear", "method": "command/execute", "params": {"name": "RpcStatuslineClear"}},
            {"id": "plugin-10", "result": None},
        )
        replies = {message["id"]: message for message in output}
        self.assertEqual(replies["plugin-1"]["method"], "red/ConfigGet")
        self.assertEqual(replies["plugin-2"]["method"], "red/StorageGet")
        self.assertEqual(replies["plugin-3"]["params"], {"key": "count", "value": 3})
        self.assertEqual(replies["plugin-4"]["method"], "red/StorageDelete")
        self.assertEqual(replies["storage"]["result"]["message"], "Persistent RPC count: 3")
        self.assertEqual(replies["plugin-5"]["method"], "red/TextPanelCreate")
        self.assertEqual(replies["plugin-5"]["params"]["id"], self.panel_id)
        self.assertEqual(replies["plugin-6"]["method"], "red/TextPanelSetContent")
        self.assertEqual(replies["plugin-7"]["method"], "red/TextPanelFocus")
        self.assertEqual(replies["plugin-8"]["method"], "red/TextPanelDispose")
        self.assertEqual(replies["plugin-9"]["method"], "red/StatuslineSet")
        self.assertEqual(replies["plugin-10"]["method"], "red/StatuslineClear")

    def test_picker_events_are_serviced_while_opening_the_host_picker(self):
        output = self.run_plugin(
            {"id": "picker", "method": "command/execute", "params": {"name": "RpcPicker"}},
            {
                "method": "picker/event",
                "params": {
                    "id": self.picker_id,
                    "event": "selected",
                    "item": {"id": "beta", "label": "Beta", "detail": "second choice", "data": None},
                },
            },
            {"id": "plugin-1", "result": None},
            {"id": "last", "method": "command/execute", "params": {"name": "RpcLastEvent"}},
        )
        replies = {message["id"]: message for message in output}
        self.assertEqual(replies["plugin-1"]["method"], "red/PickerOpen")
        self.assertEqual(replies["plugin-1"]["params"]["items"][1]["id"], "beta")
        self.assertIsNone(replies["picker"]["result"])
        self.assertEqual(
            replies["last"]["result"]["message"],
            f"Last RPC event: picker:selected ({self.picker_id})",
        )

    def test_shutdown_and_exit_interrupt_a_host_response_wait(self):
        output = self.run_plugin(
            {"id": "document", "method": "command/execute", "params": {"name": "RpcDocument"}},
            {"id": "shutdown", "method": "shutdown"},
            {"method": "exit"},
        )
        self.assertEqual([message["id"] for message in output], ["initialize", "plugin-1", "shutdown"])
        self.assertIsNone(output[-1]["result"])


class PythonRpcExampleTest(RpcExampleTestMixin, unittest.TestCase):
    runtime_command = sys.executable
    plugin_path = PYTHON_PLUGIN
    manifest_path = PYTHON_PLUGIN.parent / "red-plugin.toml"
    runtime_label = "Python"
    picker_id = "python-choice"
    panel_id = "python-preview"


@unittest.skipUnless(shutil.which("node"), "node is required for Node.js RPC example tests")
class NodeRpcExampleTest(RpcExampleTestMixin, unittest.TestCase):
    runtime_command = "node"
    plugin_path = NODE_PLUGIN
    manifest_path = NODE_PLUGIN.parent / "red-plugin.toml"
    runtime_label = "Node.js"
    picker_id = "node-choice"
    panel_id = "node-preview"


if __name__ == "__main__":
    unittest.main()
