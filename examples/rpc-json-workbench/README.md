# JSON Workbench

JSON Workbench is a standalone Python JSON-RPC plugin that previews, explores,
and applies [`jq`](https://jqlang.org/) filters to an open Red document. It keeps
the source document pinned until `JsonClose`, shows complete output in a
host-rendered panel, and uses Red's revision-checked transaction and undo APIs.

Requirements: Python 3.9 or later and `jq` on `PATH`. Install the development
package with:

```console
red plugin install --path ./examples/rpc-json-workbench
```

The package selects `python3` on macOS and Linux and `python` on Windows. Red
passes the package path and jq filter directly without a shell, so spaces in
either path are supported. If jq is elsewhere, set `jq_path` as shown below.
A missing executable reports how to install or configure it without disabling
the plugin.

The same package manager handles JSON-RPC and Husk plugins:

```console
red plugin list
red plugin disable json-workbench
red plugin update json-workbench
red plugin enable json-workbench
```

Restart Red after changing an installed plugin's enabled state from the CLI.
Local package manifests are watched while Red is running. After editing
`plugin.py`, save or touch `red-plugin.toml` to trigger reload; a valid reload
replaces the process, while a failed reload leaves the previous process active.
Updates preserve the package's enabled state.

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

The preview header keeps Run, Apply, and Close visible, replacing Run with
Cancel during panel-initiated or live-preview work. Focus the panel to use `r`
for Run, `Ctrl-c` for Cancel, `a` for Apply, `Enter` for Explore, `o` for Open,
`c` for Copy, `s` for Source, `u` for Undo, and `q` for Close. F1 opens Red's
searchable list of currently available actions, including Live preview and
Saved queries when their prerequisites are met. Search,
Visual selection, pending motions, and links under Enter keep precedence over
these panel actions.

Panel and live-preview runs use background work so Cancel and Close remain
responsive. `JsonRun` and `JsonQuery` keep their synchronous command responses;
use `JsonCancel` to interrupt a command-triggered query. Copy writes the complete
result to the system clipboard without giving the plugin read access, then shows
a transient success message while leaving the durable ready status intact.
`JsonCopy` provides the same explicit copy operation from the command line.

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

## Reload and coexistence

The `Json*` command prefix keeps this example separate from typical Husk
commands. JSON-RPC and Husk packages can run at the same time, and their
pickers, panels, status, storage, and watches remain owner-scoped. If two
plugins still declare the same command, the Husk command keeps dispatch and
discovery precedence.

Reload does not transfer a live workbench session. Query and result documents
are ordinary user-owned buffers and remain open, while the old preview panel,
status contribution, picker, watches, subprocess, pending requests, source ID,
and undo transaction are retired with the connection. Run `JsonWorkbench`
again from the intended source. Saved query history and the history preference
are the only plugin state restored from owner-scoped storage.

## Acceptance walkthrough

1. Install the package from a quoted path containing a space, then confirm
   `red plugin list` reports `enabled`, `compatible`, and `JSON-RPC`.
2. Open unsaved JSON, run `JsonWorkbench`, enter a multiline filter in the new
   query buffer, and run `JsonRun`.
3. Configure a preset, choose it with `JsonQueries`, save the query normally,
   and enable `JsonLive`.
4. Start an expensive filter from the panel and cancel it; editing and Husk UI
   should remain responsive.
5. Use `JsonExplore` on nested arrays, escaped keys, and a JSON stream. Copy the
   exact raw result, open a frozen result document, and confirm exploration did
   not rewrite either one.
6. Apply to a document and to a contiguous visual selection, undo each change,
   then edit the source after previewing and confirm stale Apply is rejected.
7. Save or touch the local manifest while its panel and a Husk picker or panel
   are open. The plugin-owned UI should retire, the ordinary query/result
   documents should remain, and both modalities should still work after
   reopening.
8. Temporarily set `jq_path` to a nonexistent executable, restart Red, and
   confirm the error suggests installing jq or correcting that setting. Restore
   the path before continuing.
9. Exercise `disable`, `update`, and `enable`; a disabled update must remain
   disabled until the explicit enable command.

For a package-local development override, an explicit jq executable may also be
appended to `[rpc].args`, after `plugin.py`:

```toml
args = ["plugin.py", "/opt/homebrew/bin/jq"]
```
