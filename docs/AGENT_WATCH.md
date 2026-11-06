# Agent watch

Agent watch follows a coding agent that is already working in your project and
shows that work in Red. As the agent reads or edits a file, Red opens it, moves
to the lines that changed, and tints them. The mode is read-only: Red does not
send input to the agent and does not write files.

This is separate from Red's own Codex agent (`Space A`, inline assist, and
`[agent] follow_tool_calls`). Those features drive Codex through Red. Agent
watch only reads the session logs that Claude Code, Codex CLI, and Gemini CLI
already write.

The session locations and the mapping from tool calls to file positions follow
the approach in [shoulder](https://github.com/bjarneo/shoulder) by Bjarne
Øverli (MIT). The implementation is Red's.

## Start and stop

From the project the agent is working in:

```shell
red --agent-watch
```

Watch another directory with an equals sign. A following path is a file to
open, not the watch root:

```shell
red --agent-watch=/path/to/project
red --agent-watch src/main.rs
```

`--agent-watch` without `=` uses the working directory, so the last example
opens `src/main.rs` and follows agents in the current project. `--root` still
sets the process directory before watch starts.

Inside Red:

```text
:agent-watch
:agent-watch /path/to/project
:AgentWatch
:agent-watch stop
```

`:agent-watch stop` and `q` both leave the mode and return to normal editing.
The buffer stays open.

Red watches the git top level of the directory you name. A session counts when
its working directory is that tree or a subdirectory, and when its log was
modified in the last hour.

## What you see

The status line shows `FOLLOW` while Red is tracking the agent, and `WATCH`
while you are scrolling on your own. The message line names the agent and the
current activity, for example:

```text
A1 claude · editing snake.js +1 -1:12 · Simplify snake.js
```

Edited lines use the theme's inserted-line color. Lines the agent is reading
use the diff header color. When several agents are active, the newest edit or
read wins. Red waits about 1.5 seconds before jumping to a different file so
two agents do not flicker the view.

## Keys

These keys belong to the watch mode. Other editing keys are refused.

| Key | Action |
| --- | --- |
| `q` or `esc` while following | Leave agent watch and return to normal editing |
| `f` | Toggle follow and free scroll |
| `esc` while scrolling | Follow the agent again |
| `j` `k` `h` `l`, arrows, and other motions | Scroll freely; follow pauses |
| `1`–`9` | Pin that agent and follow it |
| `a` | Follow whichever agent is working |
| `?` | Show the key reminder |
| `:` | Command line, including `:q` to quit Red |
| `/` `n` `N` | Search the open buffer; follow pauses |

Mouse clicks and scrolling also pause follow. Yank, wrap, and window-local
navigation stay available. Insert, delete, and other edits do not.

## Agents

Red reads the logs. It does not connect to a network or launch the agent.

| Agent | Session data | Other location |
| --- | --- | --- |
| Claude Code, including subagents | `~/.claude/projects/` | `CLAUDE_CONFIG_DIR` |
| Codex CLI | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` | `CODEX_HOME` |
| Gemini CLI | `~/.gemini/tmp/<project>/chats/` | `GEMINI_CLI_HOME` |

Gemini CLI's project directory is recognized from `.project_root` or, for older
logs, the SHA-256 of the project path. Tool names covered include read, write,
edit, apply-patch, and the usual shell, search, and web tools. A plain `cat`,
`head`, or `sed -n` file read is followed too.

## Limits

- opencode stores sessions in SQLite (`~/.local/share/opencode/opencode.db`).
  Red does not follow it.
- Only sessions touched in the last hour are considered.
- The view shows the file on disk. A clean buffer reloads when the agent
  writes; a buffer you edited before entering watch is left alone.
- Approval prompts are inferred. Gemini CLI records an approval wait. For
  Claude Code and Codex, a file tool with no result for 20 seconds is shown as
  waiting.
- Shell commands that chain with `&&`, pipes beyond a simple file read, or
  redirects stay on the activity line and do not move the cursor.
- The mode does not split the screen per agent or replay a terminal transcript.
- Windows session paths are discovered the same way, but the feature is tested
  on Unix.
