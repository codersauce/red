---
title: "Agent Check"
summary: "`red --agent-check` reports whether Red can use the installed Codex CLI app-server integration without starting a live agent session."
topics: [reference, agent, cli, codex]
sources:
  - id: agent-check
    type: file
    path: src/agent_check.rs
  - id: main
    type: file
    path: src/main.rs
  - id: cli
    type: file
    path: src/cli.rs
  - id: docs
    type: file
    path: docs/AGENT_WORKFLOW.md
  - id: tests
    type: file
    path: tests/agent_check.rs
---

# Agent Check

`red --agent-check` is the offline readiness report for Red's direct Codex app-server integration. It loads a clean effective configuration, resolves the configured Codex executable, reads `codex --version`, compares that version with Red's minimum tested CLI version, and prints whether reviewable-edit support is ready [@main] [@agent-check]. It does not install Codex, start an app-server session, or verify authentication; the first live session performs the account check [@agent-check] [@docs].

## Command Forms

| Command | Behavior |
| --- | --- |
| `red --agent-check` | Prints the readiness report and exits successfully if the report itself ran [@cli] [@main]. |
| `red --agent-check --strict` | Exits non-zero when the report is not production-ready [@cli] [@main]. |
| `red --agent-check -c 'agent.command="/path/to/codex"'` | Checks a configured executable override supplied through the normal config override path [@main] [@tests]. |

The `--strict` flag is valid only with `--agent-check`; the Clap definition marks it as requiring `agent_check` [@cli]. Startup also rejects dirty configuration before running the report, so configuration diagnostics must be fixed separately from Codex readiness [@main].

## Report Fields

`AgentCheckReport::format` prints these lines in stable order [@agent-check]:

| Field | Meaning |
| --- | --- |
| `agent support` | `enabled` unless `disable_ai = true` [@agent-check]. |
| `backend` | Always `Codex app-server` for this integration [@agent-check]. |
| `command` | The configured executable name or path, defaulting to `codex` [@agent-check]. |
| `minimum Codex version` | The minimum accepted semantic version, currently `0.144.1` [@agent-check]. |
| `authentication` | The authentication expectation; it says `installed Codex CLI (`codex login`)` when agent support is enabled [@agent-check]. |
| `reviewable-edit readiness` | `ready` when executable discovery and version checks pass, otherwise `not ready` [@agent-check]. |
| `executable` | Printed only when the command resolves to a path [@agent-check]. |
| `installed version` | Printed only when `<executable> --version` succeeds [@agent-check]. |
| message lines | Actionable findings prefixed with `- ` [@agent-check]. |

When `disable_ai = true`, the report is disabled, skips executable and version discovery, marks production readiness false, and prints the message `Red will not launch Codex.` [@agent-check].

## Executable And Version Rules

The executable is resolved through Red's Codex executable lookup helper [@agent-check]. If no executable is found, the report tells the user to install Codex, run `codex login`, and try again [@agent-check]. If an executable is found, Red runs it with `--version`, takes the last whitespace-separated token from stdout, parses it as a semantic version, and requires it to be at least `0.144.1` [@agent-check].

The tests exercise compatible and incompatible fake Codex binaries. A fake `codex-cli 0.144.5` passes `--agent-check --strict`, while `codex-cli 0.100.0` fails strict mode and reports that `0.144.1 or newer` is required [@tests]. Another test confirms agent check can succeed even when unrelated runtime resources such as the configured theme are missing, as long as configuration loading remains clean for the check's own needs [@tests].

## Authentication Boundary

The report is intentionally offline. It prints that authentication is verified when the first session starts after executable and version checks pass [@agent-check]. The workflow documentation states the same boundary: `red --agent-check --strict` locates `codex`, reads `codex --version`, and reports app-server contract support, while authentication is verified by `account/read` when the first app-server session starts [@docs].

Use [Red Command](../cli/red-command) for the surrounding CLI rules and [Reviewable Agent Edits](../../concepts/reviewable-agent-edits) for the proposal-first editing model that the Codex readiness check protects.
