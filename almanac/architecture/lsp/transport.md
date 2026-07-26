---
title: "LSP Transport"
summary: "Red's LSP transport owns one server process, bounded JSON-RPC framing, request correlation, initialization queues, diagnostics debounce, and shutdown."
topics: [architecture, lsp]
sources:
  - id: client
    type: file
    path: src/lsp/client.rs
  - id: lsp-mod
    type: file
    path: src/lsp/mod.rs
---

The LSP transport is the process and JSON-RPC boundary for one language server. `RealLspClient` owns the child process, stdin writer, stdout reader, stderr monitor, request id table, initialized state, pending message queue, document versions, server capabilities, and diagnostics debounce map [@client]. The editor receives typed `InboundMessage` values from this boundary and remains responsible for applying state changes, so transport failures and protocol errors do not directly mutate buffers [@lsp-mod].

## Process IO And Framing

`RealLspClient::start` launches the configured command without a shell, passes configured arguments and environment, and splits stdin, stdout, and stderr into asynchronous tasks [@client]. The writer task serializes outbound requests, notifications, and responses to stdin. The stdout task reads LSP frames, parses JSON, and sends typed inbound messages through a bounded channel [@client].

Frame parsing is deliberately bounded. Headers are capped, message bodies are limited to 16 MiB, duplicate or invalid `Content-Length` headers fail, and message bodies must be valid UTF-8 JSON [@client]. Stderr is also bounded by line length and retained tail length; routine lines are logged, while fatal-looking or panic-looking lines are surfaced as server errors [@client].

## Request Correlation And Initialization

Requests use process-wide numeric ids created by `Request::new`, and each sent request is stored in `pending_responses` with its method and timestamp [@lsp-mod]. Incoming responses with ids are paired back to the original request so editor handlers can dispatch by method, while server-to-client requests are kept separate because they contain both `id` and `method` [@client].

Initialization is a special lifecycle phase. `initialize` is sent with force before the client is marked initialized, while non-forced requests and notifications are queued until the server returns a valid initialize result [@client]. The pending queue has message-count and byte budgets; exceeding either budget fails the server and converts queued requests into request errors instead of allowing unbounded memory growth [@client]. After a successful initialize response, Red stores server capabilities, sends `initialized`, marks the client ready, and drains queued messages into the writer channel with refreshed timestamps [@client].

## Diagnostics, Timeouts, And Server Failure

Document changes schedule diagnostics instead of requesting them immediately. `did_change` updates the document version, computes full or incremental content changes from server sync capabilities, and records a due time; `recv_response` later sends `textDocument/diagnostic` only after the document has been quiet for the debounce interval and only when the server advertises a diagnostic provider [@client].

`recv_response` also enforces a 30-second request timeout. Initialize timeout fails the whole server, while other request timeouts return request-specific errors [@client]. If stdout closes unexpectedly, a child exits, or a processing error arrives, `fail_server` marks the client unavailable, drains pending responses and queued requests into failed request records, clears pending diagnostics, and preserves a failure reason for later errors [@client].

## Shutdown And Server Requests

Unsupported server-initiated requests are answered with JSON-RPC method-not-found errors, except `workspace/applyEdit`, which is allowed through to the editor for validated handling by [workspace edits](workspace-edits) [@client]. Shutdown sends `shutdown`, waits briefly for the response, sends `exit`, and then waits for the child to exit before killing it if needed [@client]. This keeps the transport boundary orderly without requiring the editor to know about process details.

