import json
import subprocess
import sys
import unittest
from pathlib import Path


PLUGIN = Path(__file__).resolve().parents[1] / "examples/rpc-hello-plugin/plugin.py"


class RpcExampleTest(unittest.TestCase):
    def run_plugin(self, *messages):
        messages = (
            {"id": "initialize", "method": "initialize", "params": {}},
            {"method": "initialized"},
            *messages,
        )
        result = subprocess.run(
            [sys.executable, str(PLUGIN)],
            input="".join(json.dumps({"jsonrpc": "2.0", **message}) + "\n" for message in messages),
            text=True,
            capture_output=True,
            timeout=5,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        return [json.loads(line) for line in result.stdout.splitlines()]

    def test_interleaved_events_and_requests_preserve_the_host_response(self):
        output = self.run_plugin(
            {"id": "apply", "method": "command/execute", "params": {"name": "RpcApply"}},
            {"method": "event/notify", "params": {"name": "file:saved", "payload": {"file": "notes.txt"}}},
            {"method": "unknown/notification"},
            {"id": "nested", "method": "command/execute", "params": {"name": "RpcHello", "raw_args": "nested"}},
            {"id": "plugin-1", "result": {"revision": "0", "document_id": "7"}},
            {"id": "plugin-2", "result": {"transaction_id": "edit-1", "revision": "1"}},
            {"id": "undo", "method": "command/execute", "params": {"name": "RpcUndo"}},
            {"id": "plugin-3", "result": {"revision": "2"}},
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
        self.assertEqual(replies["plugin-2"]["params"]["document_id"], "7")
        self.assertEqual(replies["plugin-3"]["params"], {"document_id": "7", "transaction_id": "edit-1"})
        self.assertEqual(replies["apply"]["result"]["message"], "RPC edit applied @ 1")
        self.assertEqual(replies["undo"]["result"]["message"], "RPC edit undone @ 2")
        self.assertEqual(replies["event"]["result"]["message"], "Last RPC event: file:saved (notes.txt)")
        self.assertEqual(replies["nested"]["error"]["code"], -32000)
        self.assertIsNone(replies["shutdown"]["result"])

    def test_host_errors_unmatched_responses_and_eof_are_reported(self):
        for responses, expected in [
            ([{"id": "plugin-1", "error": {"code": -32001, "message": "unavailable"}}], "unavailable"),
            ([{"id": "wrong", "result": None}], "unexpected host response"),
            ([], "host closed stdin while awaiting response"),
        ]:
            with self.subTest(expected=expected):
                output = self.run_plugin(
                    {"id": "document", "method": "command/execute", "params": {"name": "RpcDocument"}},
                    *responses,
                )
                self.assertEqual(output[-1]["id"], "document")
                self.assertEqual(output[-1]["error"], {"code": -32000, "message": expected})

    def test_config_and_storage_methods_are_exercised(self):
        output = self.run_plugin(
            {"id": "config", "method": "command/execute", "params": {"name": "RpcConfig"}},
            {"id": "plugin-1", "result": {"greeting": "hello"}},
            {"id": "storage", "method": "command/execute", "params": {"name": "RpcStorage"}},
            {"id": "plugin-2", "result": 2},
            {"id": "plugin-3", "result": None},
            {"id": "clear", "method": "command/execute", "params": {"name": "RpcStorageClear"}},
            {"id": "plugin-4", "result": None},
        )
        replies = {message["id"]: message for message in output}
        self.assertEqual(replies["plugin-1"]["method"], "red/ConfigGet")
        self.assertEqual(replies["plugin-2"]["method"], "red/StorageGet")
        self.assertEqual(replies["plugin-3"]["params"], {"key": "count", "value": 3})
        self.assertEqual(replies["plugin-4"]["method"], "red/StorageDelete")
        self.assertEqual(replies["storage"]["result"]["message"], "Persistent RPC count: 3")

    def test_picker_events_are_serviced_while_opening_the_host_picker(self):
        output = self.run_plugin(
            {"id": "picker", "method": "command/execute", "params": {"name": "RpcPicker"}},
            {
                "method": "picker/event",
                "params": {
                    "id": "python-choice",
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
            "Last RPC event: picker:selected (python-choice)",
        )

    def test_shutdown_and_exit_interrupt_a_host_response_wait(self):
        output = self.run_plugin(
            {"id": "document", "method": "command/execute", "params": {"name": "RpcDocument"}},
            {"id": "shutdown", "method": "shutdown"},
            {"method": "exit"},
        )
        self.assertEqual([message["id"] for message in output], ["initialize", "plugin-1", "shutdown"])
        self.assertIsNone(output[-1]["result"])


if __name__ == "__main__":
    unittest.main()
