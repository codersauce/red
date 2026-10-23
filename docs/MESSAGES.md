# Messages and jobs

`:messages` is an inbox for unresolved problems that need your attention.
Examples include a failed save, a file changed on disk, configuration problems,
a disabled plugin, or an interrupted agent turn requiring a decision. Each item
includes the next step in its details. Repeated reports update the same active
issue instead of adding another row.

Reading an issue does not fix it. Save issues disappear when the affected buffer
is saved or deliberately discarded; plugin quarantine clears after recovery or
disabling the plugin. Producers resolve their own issues when their condition
clears. Enter explicitly dismisses an item you have decided to leave alone.

Search misses, invalid commands, successful saves, copy confirmations and other
routine feedback appear briefly in the status line and never enter the inbox.
Successful clipboard transport is silent. Background LSP diagnostics go to logs;
a stopped language server can produce a repair issue.

`:jobs` opens shell and update output separately. Shell commands open this view
when launched. Reopen it to inspect output, copy details or cancel a running
shell command. Successful, failed and cancelled task output remains bounded and
inspectable there without creating an inbox obligation.

Plugin authors should use `Print`/`PrintWarning` or `red/MessageShow` for transient
feedback. Use the explicit issue publication and resolution APIs only for
conditions that require action; see [the host API](PLUGIN_API.md#actionable-issues)
and [RPC protocol](RPC_PLUGIN_PROTOCOL.md#actionable-issues-host-api-0210).
