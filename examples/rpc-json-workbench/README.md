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
:JsonQuery .users | map(select(.active))
:JsonApply
:JsonUndo
:JsonClose
```

`JsonQuery` pins the current document on its first successful snapshot. Later
queries keep using that document even after switching buffers. `JsonApply` and
`JsonUndo` currently require the pinned source to be active; select it before
retrying if Red reports that it is not current. Close the workbench before
starting on another source.

The plugin bounds source and complete output to 1 MiB, jq diagnostics to 64 KiB,
query text to 16 KiB, and each jq run to five seconds. Empty output and JSON streams
are preserved exactly. Failed, timed-out, or oversized runs cannot be applied.

To use an explicit jq executable, append it to `[rpc].args`, after `plugin.py`:

```toml
args = ["plugin.py", "/opt/homebrew/bin/jq"]
```
