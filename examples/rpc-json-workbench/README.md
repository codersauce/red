# JSON Workbench

JSON Workbench is a standalone Python JSON-RPC plugin that previews and applies
[`jq`](https://jqlang.org/) filters to an open Red document. It keeps the source
document pinned until `JsonClose`, shows complete output in a host-rendered
panel, and uses Red's revision-checked transaction and undo APIs.

Requirements: Python 3.9 or later and `jq` on `PATH`. Install the development
package with:

```console
red plugin install --path ./examples/rpc-json-workbench
```

Open a JSON document and run:

```text
:JsonWorkbench
:JsonSelection
:JsonRun
:JsonLive
:JsonCancel
:JsonQuery .users | map(select(.active))
:JsonQueries
:JsonOpenResult
:JsonCopy
:JsonExplore
:JsonSource
:JsonApply
:JsonUndo
:JsonClose
```

`JsonQuery` pins the current document on its first successful snapshot. Later
queries keep using that document even after switching buffers. `JsonApply` and
`JsonUndo` update the pinned source without leaving the query or result
document. Close the workbench before starting on another source.

`JsonWorkbench` pins the source, opens the preview panel, and creates one
ordinary unnamed query document. Edit that document with normal Red commands
and run it with `JsonRun`; its undo history and dirty state are independent of
the source. The query can be saved normally as a `.jq` file. If the query
document is closed, the next `JsonWorkbench` invocation creates a replacement.

`JsonSelection` opens the same workbench against Red's most recent contiguous
character or line selection. Leave Visual mode with `Esc`, then run the command;
block and multiple selections are rejected. Filters receive only the exact
selected text, and `JsonApply` replaces only its normalized range with an exact
preimage check against the captured whole-document revision. Any source change,
including one outside the range, requires closing and reopening the workbench
before another selected run can become applicable.

`JsonLive` watches the pinned source and query documents, then refreshes the
preview after 120 ms of quiet. Rapid edits retain one latest desired run and
replace any active `jq` child. Run `JsonLive` again to pause or `JsonCancel` to
stop current work while keeping live mode available for the next edit. Closing
either watched document pauses live mode and retires the preview; `JsonClose`
stops work and removes both watches before disposing the panel.

`JsonOpenResult` copies the latest successful output into a new editable,
unnamed document with JSON syntax. Each copy is frozen and can be saved or
closed like any other buffer. Query and result documents remain open if the
plugin exits or reloads, while the preview panel remains connection-owned.
`JsonSource` explicitly returns to the pinned source when you want to inspect
it directly.

The preview header exposes Run, Cancel, Apply, Explore, Open, Copy, and Close actions.
Run schedules work in the background so Cancel and Close remain responsive;
Copy writes the complete result to the system clipboard without giving the
plugin read access. `JsonCopy` provides the same explicit copy operation from
the command line.

`JsonExplore` and the Explore header action open a native picker over the
latest result. Object keys and array indexes form a breadcrumb in the panel;
Run this path builds a correctly escaped jq filter and previews that subtree as
the next result. JSON streams appear as separate roots, while empty output is
shown explicitly. Duplicate object keys remain visible but cannot become a jq
path because their lookup is ambiguous. Exploration never rewrites the exact
raw output used by Copy, Open, or Apply. Cancelling and reopening the explorer
returns to the same node until a new result arrives.

`JsonQueries` opens a native picker containing configured presets and up to 20
recent successful queries. Choosing one runs it against the pinned source after
the command finishes, so `JsonClose` remains available to cancel slow work.
The picker also provides explicit actions to clear, pause, or resume history.
Pause keeps existing entries until they are cleared. Query history is stored in
the plugin's owner-scoped Red storage; source and result text are never stored.

Optional configuration lives under the plugin's local registration name:

```toml
[plugin_config.json-workbench]
jq_path = "/opt/homebrew/bin/jq"
timeout_seconds = 3
result_limit_bytes = 262144

[[plugin_config.json-workbench.presets]]
label = "Active users"
query = ".users | map(select(.active))"
```

Recognized configuration values are validated when the workbench first runs.
Timeout and result limits may lower the built-in ceilings of five seconds and
1 MiB. Preset labels use at most 256 UTF-8 bytes and filters use at most 16 KiB.

The plugin bounds source and complete output to 1 MiB, jq diagnostics to 64 KiB,
query text to 16 KiB, and each jq run to five seconds. Empty output and JSON
streams are preserved exactly. Failed, timed-out, or oversized runs cannot be
applied. Picker-driven failures appear in the result panel because they do not
have a waiting command response.

For a package-local development override, an explicit jq executable may also be
appended to `[rpc].args`, after `plugin.py`:

```toml
args = ["plugin.py", "/opt/homebrew/bin/jq"]
```
