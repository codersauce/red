---
title: "LSP Navigation And Document Highlight"
summary: "Red advertises LSP location-navigation and document-highlight capabilities while the editor consumes only definition jumps; this page records the current boundary and the implementation constraints for closing that gap."
topics: [architecture, lsp, editor, ui, rendering, vim]
sources:
  - id: capability-builder
    type: file
    path: src/lsp/capabilities.rs
  - id: lsp-types
    type: file
    path: src/lsp/types.rs
  - id: lsp-trait
    type: file
    path: src/lsp/mod.rs
  - id: lsp-client
    type: file
    path: src/lsp/client.rs
  - id: lsp-manager
    type: file
    path: src/lsp/manager.rs
  - id: editor
    type: file
    path: src/editor.rs
  - id: signature-help
    type: file
    path: src/editor/signature_help.rs
  - id: diagnostics-picker
    type: file
    path: src/editor/diagnostics_picker.rs
  - id: rendering
    type: file
    path: src/editor/rendering.rs
  - id: lsp-plan
    type: conversation
    path: /Users/fcoury/.claude/projects/-Users-fcoury-code-red/c760e17a-d2b3-4f4e-b196-60f4261c3605.jsonl
---

# LSP Navigation And Document Highlight

LSP navigation and document highlight are one capability-advertisement gap in
Red. The client capability builder advertises definition, document-highlight,
type-definition, implementation, and declaration support; the location-link
variants for definition, type-definition, implementation, and declaration use
`link_support(false)` [@capability-builder]. The editor path, however, only
exposes a definition jump consumer, while `document_highlight` exists on the LSP
trait, client, and manager without an editor request, state, or rendering path
[@lsp-trait] [@lsp-client] [@lsp-manager] [@editor]. A prior implementation
plan established the durable ordering constraint for closing the gap: generalize
location navigation first, then add visible document highlights on top of the
same capability-gating and stale-response pattern [@lsp-plan].

## Current Definition Path

`Action::GoToDefinition` is the only built-in location-navigation action. It
requires the current buffer to be file-backed, converts the cursor to an LSP
position, opens the current buffer for LSP if needed, calls
`LspClient::goto_definition`, and inserts the returned request id into
`pending_definition_requests` [@editor] [@lsp-trait]. The manager routes that
request to the client associated with the file, and the concrete client sends
`textDocument/definition` [@lsp-manager] [@lsp-client].

The response path is hard-coded to definition. The editor treats an inbound
message as a completed definition request only when the method string is
`textDocument/definition`, removes the id from `pending_definition_requests`,
and runs deferred jump-list traversal after the set is empty [@editor].
`JumpBack` and `JumpForward` queue themselves while that set is non-empty, so a
definition destination lands before a user-typed `Ctrl-o` or `Ctrl-i`
navigation [@editor].

The actual jump parser accepts only the first result. If the server returns an
array, Red reads `arr.first()`; if it returns an object, Red treats that object
as the destination. The destination parser expects a `Location` shape with
`uri` and `range.start`, converts the URI back to a file path, and produces
`Action::MoveToFilePos(file, character, line + 1)` [@editor]. That means extra
definition results are silently ignored, and `LocationLink` response shapes do
not have a full visible selection path yet even though link support is
explicitly not advertised [@editor] [@capability-builder].

## Location Navigation Constraints

Future location-navigation work should not add separate copies of the
definition path. The planned implementation shape is one parameterized location
kind for definition, declaration, type definition, and implementation, with the
method string, server-capability predicate, and user-facing warning text
attached to the kind [@lsp-plan]. That kind belongs at the trait boundary as a
generalized `goto_location` method implemented by the client, manager, and test
mock, while the existing `goto_definition` call can remain as a compatibility
wrapper [@lsp-plan] [@lsp-trait] [@lsp-client] [@lsp-manager].

Capability gating should happen before sending an editor-initiated location
request. Red already stores server capability fields such as
`declaration_provider`, `definition_provider`, `type_definition_provider`,
`implementation_provider`, and `document_highlight_provider` in
`ServerCapabilities`, and the manager exposes `server_capabilities_for_file` for
the client associated with a file [@lsp-types] [@lsp-manager]. Signature help is
the nearby model: it checks `server_capabilities_for_file` before requesting
help, clears local state when the provider is absent, and avoids turning a
missing provider into a server error [@signature-help].

The pending request state also needs to become location-aware. The current
`HashSet<i64>` is enough to block jump-list traversal while a definition request
is in flight, but it cannot distinguish location kinds, origins, or stale
responses [@editor]. The planned replacement is a request-id map that records
the location kind plus the originating buffer and cursor. A response whose
origin no longer matches should be dropped so a slow language server cannot
move the user after they have changed context [@lsp-plan].

Multi-result handling is part of the navigation feature, not an optional polish
step. Implementation requests can return many destinations, and the
existing first-result-only behavior is already a latent definition limitation
[@lsp-plan] [@editor]. The implementation plan requires zero results to show a
kind-specific warning, one result to jump directly, and several results to open
a picker with file, line, and preview [@lsp-plan]. The diagnostics picker is the
local model for location-preview items: it builds `PickerItem` values with
`PickerPreview::Location`, bounded preview bytes, labels, and action payloads
owned by the editor rather than a plugin [@diagnostics-picker].

## Document Highlight Constraints

Document highlight should be implemented after location navigation because it
needs the same capability-gating and stale-response discipline [@lsp-plan].
The LSP side already has `document_highlight(file, x, y)` on the trait, manager,
and client, and the client sends `textDocument/documentHighlight`; the missing
pieces are editor scheduling, visible state, stale-response checks, theme
colors, dirty-row invalidation, and paint [@lsp-trait] [@lsp-manager]
[@lsp-client] [@editor].

The planned trigger is Normal-mode cursor idle, using the signature-help
deadline pattern rather than immediate requests on every motion [@lsp-plan]
[@signature-help]. The editor should skip requests for non-file buffers, insert
mode, active multi-cursor state, missing `document_highlight_provider`, and
cursor movement that remains inside an already highlighted range [@lsp-plan].
State should keep the request id, buffer id, buffer revision, cursor position,
and normalized ranges so edits, buffer switches, or cursor movement outside the
range can clear stale highlights [@lsp-plan].

Rendering needs to join the existing highlight pipeline rather than paint a
separate overlay. Window rendering already calls search highlights and matching
bracket highlights as part of the active window paint path [@rendering]. The
planned document-highlight pass belongs beside those passes, with precedence
below selections and search matches, and it must participate in the same
partial-row invalidation discipline as matching brackets so highlights do not
linger after motion-frame redraws [@lsp-plan] [@rendering].

Theme support should read VS Code word-highlight colors when available. The
plan names `editor.wordHighlightBackground` for read occurrences and
`editor.wordHighlightStrongBackground` for write occurrences, with a fallback
derived from Red's selection color for themes that omit them [@lsp-plan].
Configuration should add an on-by-default document-highlight switch and a
debounce setting in `LspConfig` and `default_config.toml`, so users can disable
the request stream without disabling all LSP features [@lsp-plan].

## Public Surfaces To Keep In Sync

Location navigation has several public surfaces beyond the `gd` key. New
location actions have to stay consistent across command-palette labels, the
plugin host API, agent tools, keyboard documentation, Vim compatibility
documentation, and the default config [@lsp-plan]. The exact new default keys
and Vim-style aliases were not settled by the plan, so they are not a shipped
contract and should be rechecked against current command and keymap code before
they are documented [@lsp-plan].

The prior plan recommended a core multi-result picker so `gd` and related
built-in keys do not depend on a bundled plugin being enabled [@lsp-plan].
Until implementation lands, do not document new navigation keys or
document-highlight configuration as shipped runtime behavior. Treat this page as
the architecture map for the missing editor consumer paths, and treat current
code as the source of truth for what users can invoke today [@editor]
[@lsp-plan].
