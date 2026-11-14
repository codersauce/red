# Agent pane model

`RedPane.tla` is a TLA+ model of the Agent pane lifecycle in `src/editor/agent_pane.rs`, together with the parts of Red and Codex it talks to:

- the plugin-to-editor request queue,
- the editor and `AgentManager`,
- the editor-to-worker command queue (`CodexBridge`),
- the worker and the Codex app-server JSONL stream (`src/codex/mod.rs`),
- the worker-to-editor event queue.

Transcript text, recap text, tool calls, and streaming are left out. The model describes `main` at f02478e, which merged #447.

The model checker found the four bugs that #447 fixed. Each one has a test in `src/codex/mod.rs` or `src/editor/agent_pane.rs` that drives the same ordering against the real code.

## Running it

This needs TLC from tla2tools 1.7.4 and a Java runtime. The results below used OpenJDK 27. Set `TLA2TOOLS` to the jar's path.

```sh
cd docs/verification/agent-pane
java -XX:+UseParallelGC -cp "$TLA2TOOLS" tlc2.TLC -workers auto -config MC.cfg MC.tla
java -XX:+UseParallelGC -cp "$TLA2TOOLS" tlc2.TLC -workers auto -config MCCrash.cfg MC.tla
```

| Config | Codex crashes | Invariants | Result on 24 workers |
| - | - | - | - |
| `MC.cfg` | none | all ten | no error, 6,454,733 distinct states, 26 s |
| `MCCrash.cfg` | one | all except `CutOffMeansSent` | no error, 51,645,121 distinct states, 3 min 35 s |

## What the invariants say

- `NoReplay`. A message that reached Codex is never sent to Codex again by itself.
- `NothingLost`. Every message the user submitted is still waiting, reached Codex, or was dropped by the user.
- `IdleMeansIdle`. The pane never shows its conversation idle while Codex runs a turn in it.
- `RunningTurnCanEdit`. While the pane shows a turn working, Red accepts that turn's edits.
- `NoOrphanTurn`. Codex does not keep running a turn of a conversation Red closed.
- `NotStuck`. When everything is quiet, the pane is not in a busy phase.
- `QueueExplained`. When everything is quiet, a queued follow-up shows why it waits.
- `NoHiddenQueue`. When everything is quiet, the editor holds no message the pane does not show.
- `CutOffMeansSent`. The pane says a reply was cut off only for a message Codex received.
- `StopMeansYouStopped`. The pane shows Stopping only for a message the user asked to stop.

## Known violation

With one crash, `CutOffMeansSent` fails. The shortest trace has 11 states. The user submits a message, Red resumes the saved conversation and hands the prompt to the worker, and Codex dies before the prompt reaches it. The pane then offers Retry and Continue for a message Codex never saw.

The pane cannot tell the two cases apart today. It treats a message as sent once the editor emits `agent:turn_started`, which happens before the worker writes `turn/start` to Codex. The fix is an acceptance event from the backend, sent when Codex answers `turn/start`, as planned in #448. `MCCrash.cfg` leaves this invariant out until that lands.

## Assumption

`MCCrash.cfg` uses the action constraint `CloseForgetAtomic`. When the pane forgets a conversation, `drain_agent_pane_effects` in `src/editor/agent_pane_host.rs` sends `AgentCloseSession` and `AgentForgetSession` back to back. The model assumes Codex does not die between the two requests. Without the constraint, TLC finds a `NotStuck` violation. Nobody has checked whether the real editor can observe a crash between those two requests.
