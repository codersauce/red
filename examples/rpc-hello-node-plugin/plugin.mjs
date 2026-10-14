#!/usr/bin/env node
import process from "node:process";
import readline from "node:readline";

function send(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

function sendError(message, detail) {
  if (Object.prototype.hasOwnProperty.call(message, "id")) {
    send({
      jsonrpc: "2.0",
      id: message.id,
      error: { code: -32000, message: detail },
    });
  }
}

function scalarLength(text) {
  return Array.from(text).length;
}

let initialized = false;
let lastTransactionId = null;
let lastDocumentId = null;
let lastEvent = null;
let nextRequestId = 1;

async function* readMessages() {
  const lines = readline.createInterface({
    input: process.stdin,
    crlfDelay: Infinity,
  });
  for await (const line of lines) {
    if (!line) {
      continue;
    }
    const message = JSON.parse(line);
    const method = message.method;
    if (method === "initialized") {
      initialized = true;
    } else if (method === "event/notify") {
      lastEvent = message.params;
    } else if (method === "picker/event") {
      const picker = message.params;
      lastEvent = {
        name: `picker:${picker.event}`,
        payload: { file: picker.id },
      };
    } else if (method === "shutdown") {
      send({ jsonrpc: "2.0", id: message.id, result: null });
    } else if (method === "exit") {
      process.exit(0);
    } else {
      yield message;
    }
  }
}

const incoming = readMessages();

async function requestHost(method, params) {
  if (!initialized) {
    throw new Error("cannot call host methods before initialized");
  }
  const requestId = `plugin-${nextRequestId++}`;
  send({ jsonrpc: "2.0", id: requestId, method, params });
  while (true) {
    const { value: response, done } = await incoming.next();
    if (done) {
      throw new Error("host closed stdin while awaiting response");
    }
    if (Object.prototype.hasOwnProperty.call(response, "method")) {
      sendError(response, "plugin is busy waiting for a host response");
      continue;
    }
    if (response.id !== requestId) {
      throw new Error("unexpected host response");
    }
    if (Object.prototype.hasOwnProperty.call(response, "error")) {
      throw new Error(response.error.message);
    }
    return response.result;
  }
}

for await (const message of incoming) {
  const method = message.method;
  if (method === "initialize") {
    send({
      jsonrpc: "2.0",
      id: message.id,
      result: {
        protocol_version: 1,
        commands: [
          {
            name: "RpcHello",
            metadata: {
              title: "Hello from RPC",
              arguments: true,
            },
          },
          {
            name: "RpcDocument",
            metadata: { title: "Show current RPC document" },
          },
          {
            name: "RpcConfig",
            metadata: { title: "Show this RPC plugin's configuration" },
          },
          {
            name: "RpcApply",
            metadata: {
              title: "Apply an RPC document edit",
              arguments: true,
            },
          },
          {
            name: "RpcLastEvent",
            metadata: { title: "Show the last RPC editor event" },
          },
          {
            name: "RpcPicker",
            metadata: { title: "Open an RPC picker" },
          },
          {
            name: "RpcPanel",
            metadata: { title: "Open an RPC text panel" },
          },
          {
            name: "RpcPanelClose",
            metadata: { title: "Close the RPC text panel" },
          },
          {
            name: "RpcStatusline",
            metadata: { title: "Set the RPC statusline" },
          },
          {
            name: "RpcStatuslineClear",
            metadata: { title: "Clear the RPC statusline" },
          },
          {
            name: "RpcStorage",
            metadata: { title: "Increment persistent RPC storage" },
          },
          {
            name: "RpcStorageClear",
            metadata: { title: "Clear persistent RPC storage" },
          },
          {
            name: "RpcUndo",
            metadata: { title: "Undo the last RPC edit" },
          },
        ],
        events: ["file:saved"],
      },
    });
  } else if (method === "command/execute") {
    try {
      const command = message.params.name;
      let result = null;
      if (command === "RpcConfig") {
        const config = await requestHost("red/ConfigGet", {});
        result = { message: `RPC config: ${JSON.stringify(config)}` };
      } else if (command === "RpcDocument") {
        const document = await requestHost("red/DocumentSnapshot", {});
        const path = document.path || "[scratch]";
        const chars = scalarLength(document.text);
        result = {
          message: `Current document: ${path} @ revision ${document.revision} (${chars} chars)`,
        };
      } else if (command === "RpcApply") {
        const document = await requestHost("red/DocumentSnapshot", {
          include_document_id: true,
        });
        const suffix = message.params.raw_args || "";
        let edit;
        if (suffix) {
          const firstLine = (document.text || "")
            .split("\n", 1)[0]
            .replace(/\r$/, "");
          edit = {
            range: {
              start: { line: 0, character: 0 },
              end: { line: 0, character: scalarLength(firstLine) },
            },
            text: `${firstLine} ${suffix}`,
            expected_text: firstLine,
          };
        } else {
          edit = {
            range: {
              start: { line: 0, character: 0 },
              end: { line: 0, character: 0 },
            },
            text: "# Edited by Node.js RPC\n",
            expected_text: "",
          };
        }
        const applied = await requestHost("red/DocumentApply", {
          document_id: document.document_id,
          expected_revision: document.revision,
          label: "RPC example edit",
          edits: [edit],
        });
        lastTransactionId = applied.transaction_id;
        lastDocumentId = document.document_id;
        result = { message: `RPC edit applied @ ${applied.revision}` };
      } else if (command === "RpcUndo") {
        if (lastTransactionId === null) {
          throw new Error("RpcApply has no transaction to undo");
        }
        const undone = await requestHost("red/DocumentUndo", {
          document_id: lastDocumentId,
          transaction_id: lastTransactionId,
        });
        lastTransactionId = null;
        result = { message: `RPC edit undone @ ${undone.revision}` };
      } else if (command === "RpcLastEvent") {
        if (lastEvent === null) {
          throw new Error("no RPC editor event has been received");
        }
        const eventFile = lastEvent.payload?.file ?? "[scratch]";
        result = {
          message: `Last RPC event: ${lastEvent.name} (${eventFile})`,
        };
      } else if (command === "RpcPicker") {
        await requestHost("red/PickerOpen", {
          id: "node-choice",
          title: "Node.js RPC picker",
          items: [
            { id: "alpha", label: "Alpha" },
            { id: "beta", label: "Beta", detail: "second choice" },
            { id: "gamma", label: "Gamma", data: { rank: 3 } },
          ],
        });
      } else if (command === "RpcPanel") {
        await requestHost("red/TextPanelCreate", {
          id: "node-preview",
          side: "right",
          width: 36,
          title: "Node.js RPC",
        });
        await requestHost("red/TextPanelSetContent", {
          id: "node-preview",
          blocks: [
            {
              id: "welcome",
              kind: "agent",
              format: "markdown",
              text: "# Language-agnostic panel\n\nRendered by Red from a **Node.js** JSON-RPC plugin.",
            },
          ],
        });
        await requestHost("red/TextPanelFocus", { id: "node-preview" });
      } else if (command === "RpcPanelClose") {
        await requestHost("red/TextPanelDispose", { id: "node-preview" });
      } else if (command === "RpcStatusline") {
        await requestHost("red/StatuslineSet", {
          text: "Node.js RPC ready",
          side: "right",
        });
      } else if (command === "RpcStatuslineClear") {
        await requestHost("red/StatuslineClear", {});
      } else if (command === "RpcStorage") {
        const current = await requestHost("red/StorageGet", { key: "count" });
        const count = Number.isInteger(current) ? current + 1 : 1;
        await requestHost("red/StorageSet", { key: "count", value: count });
        result = { message: `Persistent RPC count: ${count}` };
      } else if (command === "RpcStorageClear") {
        await requestHost("red/StorageDelete", { key: "count" });
        result = { message: "Persistent RPC count cleared" };
      } else {
        const name = message.params.raw_args || "Red";
        result = { message: `Hello, ${name}, from Node.js RPC!` };
      }
      send({ jsonrpc: "2.0", id: message.id, result });
    } catch (error) {
      sendError(message, error instanceof Error ? error.message : String(error));
    }
  }
}
