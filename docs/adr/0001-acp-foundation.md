# ADR 0001: Native ACP foundation and adapter boundary

- Status: accepted for the Phase 0 spike
- Date: 2026-07-10
- Scope: protocol versioning, transport ownership, adapter launch, authentication,
  filesystem control, offline behavior, and the Phase 2 scheduling consequence

## Decision

Red owns its Agent Client Protocol client in Rust. The core uses the official
`agent-client-protocol-schema` crate pinned exactly to artifact version `1.4.0` and
negotiates stable wire protocol version `1`. Artifact and wire versions are separate:
upgrading the crate does not imply a wire-protocol change, and accepting a wire version
does not imply support for every optional capability.

The adapter is a child process using newline-delimited JSON-RPC 2.0 over piped stdin and
stdout. One actor owns the process and correlation table. Editor/plugin commands and
agent events use bounded queues. Request timeouts, malformed messages, adapter exits,
version mismatch, and queue closure become recoverable session errors rather than editor
startup failures. Shutdown closes stdin, waits for a bounded grace period, and kills an
unresponsive child.

The initial host surface contains only:

- `AgentNewSession`, `AgentPrompt`, `AgentCancel`, and `AgentCloseSession` actions from Husk to core;
- `agent:session_created`, `agent:update`, `agent:completed`, `agent:cancelled`, and
  `agent:error` events from core to Husk;
- typed client callbacks for `fs/read_text_file`, `fs/write_text_file`,
  `session/request_permission`, and `session/update`.

The bundled `agent.hk` plugin proves this boundary without making it the long-term UI.
The Phase 2 plugin will replace the status-line-only spike with a prompt composer,
conversation model, permissions, and proposal review.

## Conformance evidence

`tests/acp_conformance.rs` launches a real child fixture and verifies:

1. initialization and wire-version negotiation;
2. `session/new` and `session/prompt`;
3. an agent read observing unsaved client contents;
4. an agent write captured by the client rather than written to disk;
5. a permission request with an exact option ID response;
6. a streaming `session/update` notification;
7. cancellation while the prompt request is pending; and
8. orderly process shutdown.

The fixture is deliberately deterministic and runs on every supported Rust test target.
It validates Red's protocol implementation, not the behavior of an external vendor
adapter.

## Adapter audit and review guarantee

The first candidate was the official-registry Codex adapter,
`@agentclientprotocol/codex-acp` 1.1.2, audited at commit
`75893cd87741b2fe127f6698a8741a2e625c3787` on 2026-07-10. That adapter does not call
ACP client `fs/read_text_file` or `fs/write_text_file` for Codex edits. Its
`CodexToolCallMapper.ts` reconstructs diff display by reading the process filesystem,
while Codex/App Server owns the actual file-change path.

Therefore Red must not advertise reviewable, isolated edits with this adapter. Approval
events and post-hoc diff events are not equivalent to Red owning proposed contents. The
candidate remains usable later for conversation-only or explicitly direct-write modes,
but it is rejected for the Phase 2 review demo.

Phase 2 must do one of the following before scheduling the review workflow:

- select and pin an adapter whose edit tools demonstrably use ACP client filesystem
  methods; or
- build a provider-specific integration that redirects every read and write into Red's
  proposal filesystem and prevents shell/direct-process bypass.

The live-adapter conformance suite must assert read-after-write against Red's in-memory
proposal state and verify that disk is unchanged. This adds an explicit 1–2 engineer-week
adapter qualification allowance to Phase 2.2; it is not hidden inside UI work.

## Discovery, installation, authentication, and offline behavior

Phase 0 supports an explicit custom adapter only:

```toml
[agent]
command = "codex-acp"
args = []
```

`command` is executed directly, never through a shell. `args` and `env` are passed as
separate process values. Red does not search a remote registry, install packages, run
`npx -y`, update an adapter, or open a browser. A missing executable is reported through
`agent:error`.

Authentication remains owned by the installed adapter. Red preserves the user's process
environment plus configured overrides, but the Phase 0 client does not yet drive ACP
`authenticate`. Phase 2 must present advertised authentication methods without logging or
persisting secrets and must make browserless/SSH behavior explicit.

Offline, Red performs no adapter discovery or download. Starting a locally installed,
already-authenticated adapter is allowed; any network request made by that adapter is
subject to its own behavior and is not yet sandboxed by Red. With `disable_ai = true`,
the bundled agent plugin is removed before activation and the editor rejects agent
process startup. Phase 2's `red --agent-check` will report all of these prerequisites
without changing the machine.

## Consequences

- The protocol and process lifecycle are reusable independently of the Husk UI.
- Backpressure and cancellation work without blocking the terminal input loop.
- Client filesystem ownership is an explicit qualification gate, not inferred from
  general ACP compatibility.
- The Phase 0 spike is not a product-ready agent feature: authentication UI, proposal
  state, diff review, terminal methods, session close/load, and adapter discovery remain
  Phase 2 work.
