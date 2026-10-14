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
