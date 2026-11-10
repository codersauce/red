---------------------------- MODULE RedPane ----------------------------
(***************************************************************************)
(* Red's Agent pane lifecycle (src/editor/agent_pane.rs) inside its real   *)
(* environment, at commit f02478e (#447):                                 *)
(*   plugin -> editor request FIFO        (runtime.send_request)          *)
(*   editor + AgentManager                (src/editor.rs, agent_manager.rs)*)
(*   editor -> worker command FIFO        (CodexBridge)                    *)
(*   worker <-> Codex app-server JSONL    (src/codex/mod.rs)               *)
(*   worker -> editor event FIFO                                           *)
(* Transcript text, recap text, tool calls and streaming are left out.    *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS UserPrompts,   \* messages the user types
          ExtraPrompts,  \* messages made by Retry / Continue
          NewThreads,    \* sequence of thread ids Codex hands out
          Saved,         \* thread saved by an earlier Red session
          MaxCrashes, MaxResets, MaxSwitches,
          MaxRedTurns, MaxCodexTurns, MaxChan

NONE == "none"
Threads == {Saved} \cup {NewThreads[i] : i \in 1..Len(NewThreads)}
Prompts == UserPrompts \cup ExtraPrompts
Range(s) == {s[i] : i \in DOMAIN s}

VARIABLES
  pane,                       \* AgentPaneLifecycle
  reqs,                       \* plugin request FIFO (pane intents + pane effects)
  bridge,                     \* "none" | "live" | "dead" (task finished)
  mPhase, mTurn, mQueue,      \* AgentManager turn_phases / active_turn_ids / queued_prompts
  conv, forg,                 \* AgentManager conversation thread / forgotten_conversations
  cmds, evts,                 \* CodexBridge command and event channels
  redCtr,                     \* editor turn ids minted
  wSessions, wActive, wCancelled, wPendInt,   \* worker session map
  cin, cout,                  \* app-server stdin / stdout
  cxThreads, cxLoaded, cxRun, cxIntr, cxCtr, nextThread,  \* Codex
  crashes, resets, switches,
  started, submitted, consented   \* ghosts

edVars == <<pane, reqs, bridge, mPhase, mTurn, mQueue, conv, forg, cmds, evts, redCtr>>
wVars == <<wSessions, wActive, wCancelled, wPendInt, cin, cout>>
cxVars == <<cxThreads, cxLoaded, cxRun, cxIntr, cxCtr, nextThread>>
ghostVars == <<crashes, resets, switches, started, submitted, consented>>
vars == <<edVars, wVars, cxVars, ghostVars>>

\* One record shape for every message so TLC can compare them.
Msg(k, t, p, n, s) == [k |-> k, t |-> t, p |-> p, n |-> n, s |-> s]
Req(k, t, p) == Msg(k, t, p, 0, "")

---------------------------------------------------------------------------
(* AgentPaneLifecycle. Each operator returns [pn |-> pane, fx |-> effects]. *)

Default == [phase |-> "Idle", session |-> NONE, turn |-> 0, inFlight |-> NONE,
            queue |-> <<>>, paused |-> FALSE, stopReq |-> FALSE,
            stopFailed |-> FALSE, recap |-> FALSE, interrupted |-> NONE,
            resumeCwd |-> FALSE, switchTarget |-> NONE, switchPrev |-> NONE,
            forgotten |-> NONE]

Out(pn, fx) == [pn |-> pn, fx |-> fx]
Then(o, F(_)) == LET r == F(o.pn) IN Out(r.pn, o.fx \o r.fx)
Busy(pn) == pn.phase \in {"Working", "Stopping"}
Paused(pn) == pn.queue # <<>>

ResumeNowOk(pn) == pn.session # NONE /\ pn.phase = "Idle" /\ pn.resumeCwd

Advance(pn) ==
  IF pn.paused \/ pn.queue = <<>> \/ pn.interrupted # NONE THEN Out(pn, <<>>)
  ELSE IF ResumeNowOk(pn)
    THEN Out([pn EXCEPT !.phase = "Restoring", !.resumeCwd = FALSE],
             <<Req("resume", pn.session, NONE)>>)
  ELSE IF pn.phase = "Idle" /\ pn.session = NONE
    THEN Out([pn EXCEPT !.phase = "Starting"], <<Req("new", NONE, NONE)>>)
  ELSE IF pn.phase = "Idle"
    THEN Out([pn EXCEPT !.queue = Tail(pn.queue), !.recap = FALSE,
                        !.phase = "Working", !.turn = 0,
                        !.inFlight = Head(pn.queue)],
             <<Req("prompt", pn.session, Head(pn.queue))>>)
  ELSE Out(pn, <<>>)

Submit(pn, p) ==
  LET pn1 == [pn EXCEPT !.interrupted = NONE]
  IN IF Busy(pn) \/ (pn.paused /\ pn.queue # <<>>)
       THEN Out([pn1 EXCEPT !.queue = Append(pn.queue, p)], <<>>)
       ELSE Advance([pn1 EXCEPT !.paused = FALSE, !.queue = Append(pn.queue, p)])

Stop(pn) ==
  IF Busy(pn) /\ pn.session # NONE
    THEN Out([pn EXCEPT !.phase = "Stopping", !.stopReq = TRUE, !.stopFailed = FALSE],
             <<Req("cancel", pn.session, NONE)>>)
  ELSE IF pn.phase = "Starting" THEN Out([pn EXCEPT !.paused = Paused(pn)], <<>>)
  ELSE Out(pn, <<>>)

SendQueued(pn) == Advance([pn EXCEPT !.interrupted = NONE, !.paused = FALSE])

DiscardQueued(pn) == Out([pn EXCEPT !.queue = <<>>, !.paused = FALSE], <<>>)

StartIntent(pn) ==
  IF pn.session # NONE \/ pn.phase # "Idle" THEN Out(pn, <<>>)
  ELSE Out([pn EXCEPT !.phase = "Starting"], <<Req("new", NONE, NONE)>>)

\* ForgetSession effect = AgentCloseSession then AgentForgetSession.
Forget(t) == <<Req("close", t, NONE), Req("forget", t, NONE)>>

ResetStart(pn) ==
  LET fx == IF pn.session # NONE THEN Forget(pn.session) ELSE <<>>
      pn1 == [Default EXCEPT
                !.phase = IF pn.phase = "Starting" THEN "Starting" ELSE "Idle",
                !.forgotten = IF pn.phase = "Restoring" THEN pn.session ELSE pn.forgotten]
  IN Then(Out(pn1, fx), StartIntent)

Prepare(pn) ==
  IF pn.phase = "Idle" /\ ResumeNowOk(pn)
    THEN Out([pn EXCEPT !.phase = "Restoring", !.resumeCwd = FALSE],
             <<Req("resume", pn.session, NONE)>>)
    ELSE Out(pn, <<>>)

SessionCreated(pn, t) ==
  LET fx == IF pn.session # NONE /\ pn.session # t THEN <<Req("close", pn.session, NONE)>> ELSE <<>>
      pn1 == [pn EXCEPT !.forgotten = NONE, !.session = t,
                        !.phase = IF pn.phase = "Starting" THEN "Idle" ELSE pn.phase]
  IN Then(Out(pn1, fx), Advance)

StartFailed(pn) ==
  IF pn.phase # "Starting" THEN Out(pn, <<>>)
  ELSE Out([pn EXCEPT !.phase = "Idle", !.paused = Paused(pn)], <<>>)

TurnStarted(pn, t, rt) ==
  IF pn.session = t THEN Out([pn EXCEPT !.turn = rt], <<>>) ELSE Out(pn, <<>>)

TurnEnded(pn, t, rt, stopped) ==
  IF pn.session # t \/ (rt # 0 /\ pn.turn # 0 /\ rt # pn.turn) \/ ~Busy(pn)
    THEN Out(pn, <<>>)
  ELSE LET pn1 == [pn EXCEPT !.inFlight = NONE,
                     !.interrupted = IF stopped /\ ~pn.stopReq THEN pn.inFlight ELSE pn.interrupted,
                     !.phase = "Idle", !.turn = 0, !.stopReq = FALSE, !.stopFailed = FALSE]
       IN IF pn.stopReq \/ stopped
            THEN Out([pn1 EXCEPT !.paused = Paused(pn1)], <<>>)
            ELSE Advance(pn1)

Stopping(pn, t) ==
  IF pn.session = t /\ pn.phase = "Working"
    THEN Out([pn EXCEPT !.phase = "Stopping", !.stopReq = TRUE, !.stopFailed = FALSE], <<>>)
    ELSE Out(pn, <<>>)

StopFailed(pn, t) ==
  IF pn.session = t /\ pn.phase = "Stopping"
    THEN Out([pn EXCEPT !.phase = "Working", !.stopFailed = TRUE], <<>>)
    ELSE Out(pn, <<>>)

SessionLost(pn, unsent) ==
  IF unsent /\ pn.phase = "Restoring" THEN Out(pn, <<>>) ELSE
  LET retry == IF unsent THEN pn.inFlight ELSE NONE
      ran == IF ~unsent /\ Busy(pn) THEN pn.inFlight ELSE NONE
      pn1 == [pn EXCEPT !.inFlight = NONE,
                        !.interrupted = IF ran # NONE THEN ran ELSE pn.interrupted,
                        !.resumeCwd = FALSE, !.session = NONE,
                        !.recap = pn.recap \/ pn.session # NONE \/ retry # NONE,
                        !.phase = "Idle", !.turn = 0, !.stopReq = FALSE, !.stopFailed = FALSE]
  IN IF retry # NONE
       THEN Advance([pn1 EXCEPT !.queue = <<retry>> \o pn1.queue, !.paused = FALSE])
       ELSE Out([pn1 EXCEPT !.paused = Paused(pn1)], <<>>)

RestorePending(pn, t) ==
  LET run == Busy(pn)
      sent == pn.turn # 0
      back == run /\ pn.inFlight # NONE /\ ~sent
      q == IF back THEN <<pn.inFlight>> \o pn.queue ELSE pn.queue
  IN Out([pn EXCEPT !.switchTarget = NONE, !.switchPrev = NONE,
            !.interrupted = IF run /\ pn.inFlight # NONE /\ sent THEN pn.inFlight ELSE pn.interrupted,
            !.queue = q,
            !.paused = IF run THEN q # <<>> /\ (sent \/ pn.stopReq) ELSE pn.paused,
            !.session = t, !.phase = "Restoring", !.turn = 0, !.inFlight = NONE,
            !.stopReq = FALSE, !.stopFailed = FALSE, !.resumeCwd = FALSE],
         <<Req("resume", t, NONE)>>)

SwitchOk(pn, t) ==
  /\ pn.phase = "Idle" /\ pn.session # t /\ pn.inFlight = NONE
  /\ pn.queue = <<>> /\ pn.interrupted = NONE

SwitchPending(pn, t) ==
  [pn EXCEPT !.switchPrev = pn.session, !.switchTarget = t,
             !.phase = "Restoring", !.turn = 0]

Resolve(pn, choice, x) ==
  IF pn.interrupted = NONE THEN Out(pn, <<>>)
  ELSE IF choice = "dismiss"
    THEN Out([pn EXCEPT !.interrupted = NONE, !.paused = Paused(pn)], <<>>)
  ELSE Advance([pn EXCEPT !.interrupted = NONE, !.queue = <<x>> \o pn.queue,
                          !.paused = FALSE])

SessionClosed(pn, t) ==
  IF pn.session # t THEN Out(pn, <<>>)
  ELSE Out([pn EXCEPT !.session = NONE, !.resumeCwd = FALSE, !.phase = "Idle",
                      !.turn = 0, !.inFlight = NONE, !.stopReq = FALSE,
                      !.stopFailed = FALSE, !.paused = Paused(pn)], <<>>)

Restored(pn, t) ==
  IF pn.switchTarget # NONE THEN
    LET prev == pn.switchPrev
        fx == IF prev # NONE /\ prev # t THEN <<Req("close", prev, NONE)>> ELSE <<>>
    IN Then(Out([pn EXCEPT !.switchTarget = NONE, !.switchPrev = NONE,
                           !.forgotten = NONE, !.session = t, !.phase = "Idle",
                           !.recap = FALSE, !.resumeCwd = FALSE], fx), Advance)
  ELSE IF pn.forgotten = t THEN Out(pn, <<>>)
  ELSE IF pn.session # NONE /\ pn.session # t THEN Out(pn, <<>>)
  ELSE Advance([pn EXCEPT !.session = t, !.phase = "Idle", !.recap = FALSE,
                          !.resumeCwd = FALSE])

RestoreFailed(pn, t) ==
  IF pn.switchTarget = t
    THEN Out([pn EXCEPT !.switchTarget = NONE, !.session = pn.switchPrev,
                        !.switchPrev = NONE, !.phase = "Idle"], <<>>)
  ELSE IF pn.session # t THEN Out(pn, <<>>)
  ELSE Then(Out([pn EXCEPT !.session = NONE, !.resumeCwd = FALSE, !.phase = "Idle",
                           !.recap = TRUE], Forget(t)), Advance)

---------------------------------------------------------------------------
(* Editor: one record E over the editor-owned variables.                   *)

CurE == [pane |-> pane, rq |-> reqs, bridge |-> bridge, mPhase |-> mPhase,
         mTurn |-> mTurn, mQueue |-> mQueue, conv |-> conv, forg |-> forg,
         cmds |-> cmds, evts |-> evts, redCtr |-> redCtr]

SetE(e) ==
  /\ pane' = e.pane /\ reqs' = e.rq /\ bridge' = e.bridge
  /\ mPhase' = e.mPhase /\ mTurn' = e.mTurn /\ mQueue' = e.mQueue
  /\ conv' = e.conv /\ forg' = e.forg /\ cmds' = e.cmds /\ evts' = e.evts
  /\ redCtr' = e.redCtr

\* notify_agent: the pane observes, its effects join the request FIFO.
PaneDo(e, o) == [e EXCEPT !.pane = o.pn, !.rq = e.rq \o o.fx]

\* finish_agent_bridge: drops the bridge with its unread events, clears busy
\* state; agent:prompts_returned goes to plugins, and the pane ignores it.
Finish(e) == [e EXCEPT !.bridge = "none", !.mPhase = [x \in Threads |-> "none"],
                       !.mTurn = [x \in Threads |-> 0],
                       !.mQueue = [x \in Threads |-> <<>>],
                       !.cmds = <<>>, !.evts = <<>>]

Fresh(e) == [e EXCEPT !.bridge = "live"]

\* mark_session_inactive: a Stopping turn stays busy.
Inactive(e, t) ==
  IF e.mPhase[t] = "Stopping" THEN e
  ELSE [e EXCEPT !.mPhase[t] = "none", !.mTurn[t] = 0]

\* dispatch_agent_prompt
Dispatch(e, t, p) ==
  IF e.bridge = "dead"
    THEN LET f == Finish(e) IN PaneDo(f, SessionLost(f.pane, TRUE))
  ELSE IF e.bridge = "none" THEN PaneDo(e, SessionLost(e.pane, TRUE))
  ELSE IF e.mPhase[t] # "none" THEN [e EXCEPT !.mQueue[t] = Append(@, p)]
  ELSE LET rt == e.redCtr + 1
           e1 == [e EXCEPT !.redCtr = rt, !.mTurn[t] = rt]
           e2 == PaneDo(e1, TurnStarted(e1.pane, t, rt))
       IN [e2 EXCEPT !.mPhase[t] = "Running",
                     !.cmds = Append(@, Msg("prompt", t, p, rt, ""))]

\* Running the head request with a pane transition only.
PaneReq(o) ==
  /\ SetE(PaneDo([CurE EXCEPT !.rq = Tail(reqs)], o))
  /\ UNCHANGED <<wVars, cxVars>>

Hd == Head(reqs)
\* The request loop stops while the app-server task is finished but not yet
\* torn down (editor.rs service_background), so the tail runs first.
ReqReady == reqs # <<>> /\ bridge # "dead"
Rest == [CurE EXCEPT !.rq = Tail(reqs)]

---------------------------------------------------------------------------
(* The user, through the plugin. Intents queue behind earlier requests.    *)

Enqueue(r) == reqs' = Append(reqs, r)

\* The composer is disabled while a session switch is pending.
UserSubmit(p) ==
  /\ p \notin submitted
  /\ pane.switchTarget = NONE /\ ~\E r \in Range(reqs) : r.k = "u_switch"
  /\ Enqueue(Req("u_submit", NONE, p))
  /\ submitted' = submitted \cup {p}
  /\ UNCHANGED <<pane, bridge, mPhase, mTurn, mQueue, conv, forg, cmds, evts, redCtr,
                 wVars, cxVars, crashes, resets, switches, started, consented>>

UserIntent(k) ==
  /\ Enqueue(Req(k, NONE, NONE))
  /\ UNCHANGED <<pane, bridge, mPhase, mTurn, mQueue, conv, forg, cmds, evts, redCtr,
                 wVars, cxVars, ghostVars>>

UserStop == (Busy(pane) \/ pane.phase = "Starting") /\ UserIntent("u_stop")
UserSendQueued == (pane.paused \/ pane.interrupted # NONE) /\ UserIntent("u_send")
UserDiscard == pane.queue # <<>> /\ UserIntent("u_discard")
UserPrepare == pane.resumeCwd /\ UserIntent("u_prepare")
\* Retry and Continue need a fresh message id; only one exists in the model.
UserResolve(c) ==
  /\ pane.interrupted # NONE
  /\ c # "dismiss" => /\ ExtraPrompts \ submitted # {}
                     /\ ~\E r \in Range(reqs) : r.k \in {"retry", "continue"}
  /\ UserIntent(c)

UserReset ==
  /\ resets < MaxResets
  /\ Enqueue(Req("u_reset", NONE, NONE))
  /\ resets' = resets + 1
  /\ UNCHANGED <<pane, bridge, mPhase, mTurn, mQueue, conv, forg, cmds, evts, redCtr,
                 wVars, cxVars, crashes, switches, started, submitted, consented>>

\* session_selected: the plugin checks that the pane is idle, then asks.
UserSwitch(t) ==
  /\ switches < MaxSwitches
  /\ pane.phase = "Idle" /\ t \in cxThreads /\ t # pane.session
  /\ Enqueue(Req("u_switch", t, NONE))
  /\ switches' = switches + 1
  /\ UNCHANGED <<pane, bridge, mPhase, mTurn, mQueue, conv, forg, cmds, evts, redCtr,
                 wVars, cxVars, crashes, resets, started, submitted, consented>>

---------------------------------------------------------------------------
(* Editor handles pane intents (handle_agent_pane_request).                *)

Held(pn) == {pn.inFlight, pn.interrupted} \cup Range(pn.queue)

EdSubmit ==
  /\ ReqReady /\ Hd.k = "u_submit"
  /\ consented' = consented \cup ({pane.interrupted} \ {NONE})
  /\ PaneReq(Submit(pane, Hd.p))
  /\ UNCHANGED <<crashes, resets, switches, started, submitted>>

EdStop ==
  /\ ReqReady /\ Hd.k = "u_stop"
  /\ consented' = consented \cup (IF Busy(pane) THEN {pane.inFlight} \ {NONE} ELSE {})
  /\ PaneReq(Stop(pane))
  /\ UNCHANGED <<crashes, resets, switches, started, submitted>>

EdSendQueued ==
  /\ ReqReady /\ Hd.k = "u_send"
  /\ consented' = consented \cup ({pane.interrupted} \ {NONE})
  /\ PaneReq(SendQueued(pane))
  /\ UNCHANGED <<crashes, resets, switches, started, submitted>>

EdDiscard ==
  /\ ReqReady /\ Hd.k = "u_discard"
  /\ consented' = consented \cup Range(pane.queue)
  /\ PaneReq(DiscardQueued(pane))
  /\ UNCHANGED <<crashes, resets, switches, started, submitted>>

EdReset ==
  /\ ReqReady /\ Hd.k = "u_reset"
  /\ consented' = consented \cup (Held(pane) \ {NONE})
  /\ PaneReq(ResetStart(pane))
  /\ UNCHANGED <<crashes, resets, switches, started, submitted>>

EdPrepare ==
  /\ ReqReady /\ Hd.k = "u_prepare"
  /\ PaneReq(Prepare(pane))
  /\ UNCHANGED ghostVars

\* Retry and Continue send a new message; the original is answered.
EdResolve ==
  /\ ReqReady /\ Hd.k \in {"retry", "continue", "dismiss"}
  /\ \/ /\ Hd.k = "dismiss" \/ pane.interrupted = NONE
        /\ consented' = consented \cup ({pane.interrupted} \ {NONE})
        /\ PaneReq(Resolve(pane, "dismiss", NONE))
        /\ UNCHANGED <<crashes, resets, switches, started, submitted>>
     \/ /\ Hd.k # "dismiss" /\ pane.interrupted # NONE
        /\ \E x \in ExtraPrompts \ submitted :
             /\ submitted' = submitted \cup {x}
             /\ consented' = consented \cup {pane.interrupted}
             /\ PaneReq(Resolve(pane, Hd.k, x))
        /\ UNCHANGED <<crashes, resets, switches, started>>

(* Editor handles agent host requests.                                    *)

EdNewSession ==
  /\ ReqReady /\ Hd.k = "new"
  /\ LET e1 == IF bridge = "dead"
                 THEN LET f == Finish(Rest) IN PaneDo(f, SessionLost(f.pane, FALSE))
                 ELSE Rest
         e2 == Fresh(e1)
     IN SetE([e2 EXCEPT !.cmds = Append(@, Msg("new", NONE, NONE, 0, ""))])
  /\ UNCHANGED <<wVars, cxVars, ghostVars>>

EdResumeSession ==
  /\ ReqReady /\ Hd.k = "resume"
  /\ LET e1 == IF bridge = "dead" THEN Finish(Rest) ELSE Rest
         e2 == Fresh(e1)
     IN SetE([e2 EXCEPT !.cmds = Append(@, Msg("resume", Hd.t, NONE, 0, ""))])
  /\ UNCHANGED <<wVars, cxVars, ghostVars>>

EdSwitchSession ==
  /\ ReqReady /\ Hd.k = "u_switch"
  /\ IF SwitchOk(pane, Hd.t)
       THEN LET e0 == [Rest EXCEPT !.pane = SwitchPending(pane, Hd.t)]
                e1 == IF bridge = "dead" THEN Finish(e0) ELSE e0
                e2 == Fresh(e1)
                \* session_switch_command: Resume only for the saved conversation
                \* whose tool contract matches; any other thread is continued in
                \* a new thread.
                k == IF conv = Hd.t THEN "resume" ELSE "continue"
            IN SetE([e2 EXCEPT !.cmds = Append(@, Msg(k, Hd.t, NONE, 0, ""))])
       ELSE SetE(PaneDo(Rest, RestoreFailed(pane, Hd.t)))
  /\ UNCHANGED <<wVars, cxVars, ghostVars>>

EdPrompt ==
  /\ ReqReady /\ Hd.k = "prompt"
  /\ SetE(Dispatch(Rest, Hd.t, Hd.p))
  /\ UNCHANGED <<wVars, cxVars, ghostVars>>

EdCancel ==
  /\ ReqReady /\ Hd.k = "cancel"
  /\ LET t == Hd.t
         e1 == IF mPhase[t] = "Running" THEN [Rest EXCEPT !.mPhase[t] = "Stopping"] ELSE Rest
     IN SetE(CASE bridge = "none" -> e1
               [] bridge = "dead" -> PaneDo(e1, StartFailed(e1.pane))
               [] OTHER -> [e1 EXCEPT !.cmds = Append(@, Msg("cancel", t, NONE, 0, ""))])
  /\ UNCHANGED <<wVars, cxVars, ghostVars>>

EdClose ==
  /\ ReqReady /\ Hd.k = "close"
  /\ LET t == Hd.t
         e1 == Inactive(Rest, t)
         e2 == [e1 EXCEPT !.mPhase[t] = "none", !.mQueue[t] = <<>>]
         e3 == PaneDo(e2, SessionClosed(e2.pane, t))
     IN SetE(CASE bridge = "none" -> e3
               [] bridge = "dead" -> PaneDo(e3, StartFailed(e3.pane))
               [] OTHER -> [e3 EXCEPT !.cmds = Append(@, Msg("close", t, NONE, 0, ""))])
  /\ UNCHANGED <<wVars, cxVars, ghostVars>>

EdForget ==
  /\ ReqReady /\ Hd.k = "forget"
  /\ LET t == Hd.t
         e1 == Inactive(Rest, t)
     IN SetE([e1 EXCEPT !.forg = @ \cup {t},
                        !.conv = IF conv = t THEN NONE ELSE conv])
  /\ UNCHANGED <<wVars, cxVars, ghostVars>>

(* Editor handles worker events (service_background).                     *)

Ev == Head(evts)
EvRest == [CurE EXCEPT !.evts = Tail(evts)]
EdEvent(k) == bridge # "none" /\ evts # <<>> /\ Ev.k = k

EdSessionCreated ==
  /\ EdEvent("created")
  /\ LET t == Ev.t
         e1 == [EvRest EXCEPT !.conv = t, !.forg = @ \ {t}]
     IN SetE(IF pane.phase = "Starting" \/ (pane.phase = "Idle" /\ pane.session = NONE)
               THEN PaneDo(e1, SessionCreated(pane, t)) ELSE e1)
  /\ UNCHANGED <<wVars, cxVars, ghostVars>>

EdSessionRestored ==
  /\ EdEvent("restored")
  /\ LET t == Ev.t
     IN SetE(IF t \in forg
               THEN [EvRest EXCEPT !.forg = @ \ {t},
                       !.cmds = IF bridge = "live" THEN Append(@, Msg("close", t, NONE, 0, "")) ELSE @]
               ELSE PaneDo([EvRest EXCEPT !.conv = t], Restored(pane, t)))
  /\ UNCHANGED <<wVars, cxVars, ghostVars>>

EdRestoreFailed ==
  /\ EdEvent("restore_failed")
  /\ SetE(PaneDo(EvRest, RestoreFailed(pane, Ev.t)))
  /\ UNCHANGED <<wVars, cxVars, ghostVars>>

\* Completed and Failed{Some}: finish_turn, notify with the ended turn id,
\* then dispatch the prompt the manager held behind this turn.
TurnEnd(stopped) ==
  LET t == Ev.t
      ended == mTurn[t]
      held == mQueue[t]
      e1 == [EvRest EXCEPT !.mPhase[t] = "none", !.mTurn[t] = 0,
                           !.mQueue[t] = IF held = <<>> THEN <<>> ELSE Tail(held)]
      e2 == PaneDo(e1, TurnEnded(pane, t, ended, stopped))
  IN IF held = <<>> THEN e2 ELSE Dispatch(e2, t, Head(held))

EdCompleted ==
  /\ EdEvent("completed")
  /\ SetE(TurnEnd(Ev.s # "completed"))
  /\ UNCHANGED <<wVars, cxVars, ghostVars>>

EdFailed ==
  /\ EdEvent("failed")
  /\ SetE(IF Ev.t = NONE THEN PaneDo(EvRest, StartFailed(pane)) ELSE TurnEnd(TRUE))
  /\ UNCHANGED <<wVars, cxVars, ghostVars>>

EdCancelled ==
  /\ EdEvent("cancelled")
  /\ SetE(PaneDo(Inactive(EvRest, Ev.t), Stopping(pane, Ev.t)))
  /\ UNCHANGED <<wVars, cxVars, ghostVars>>

EdCancelFailed ==
  /\ EdEvent("cancel_failed")
  /\ SetE(PaneDo(EvRest, StopFailed(pane, Ev.t)))
  /\ UNCHANGED <<wVars, cxVars, ghostVars>>

\* The app-server task finished and every event was read.
EdBridgeLost ==
  /\ bridge = "dead" /\ evts = <<>>
  /\ LET f == Finish(CurE)
     IN SetE(IF conv # NONE THEN PaneDo(f, RestorePending(f.pane, conv))
                            ELSE PaneDo(f, SessionLost(f.pane, FALSE)))
  /\ UNCHANGED <<wVars, cxVars, ghostVars>>

---------------------------------------------------------------------------
(* Worker (src/codex/mod.rs run loop).                                    *)

Cmd == Head(cmds)
WCmd(k) == bridge = "live" /\ cmds # <<>> /\ Cmd.k = k

WNewSession ==
  /\ WCmd("new")
  /\ cmds' = Tail(cmds) /\ cin' = Append(cin, Msg("start", NONE, NONE, 0, ""))
  /\ UNCHANGED <<pane, reqs, bridge, mPhase, mTurn, mQueue, conv, forg, evts, redCtr,
                 wSessions, wActive, wCancelled, wPendInt, cout, cxVars, ghostVars>>

WResume ==
  /\ bridge = "live" /\ cmds # <<>> /\ Cmd.k \in {"resume", "continue"}
  /\ cmds' = Tail(cmds) /\ cin' = Append(cin, Msg(Cmd.k, Cmd.t, NONE, 0, ""))
  /\ UNCHANGED <<pane, reqs, bridge, mPhase, mTurn, mQueue, conv, forg, evts, redCtr,
                 wSessions, wActive, wCancelled, wPendInt, cout, cxVars, ghostVars>>

\* start_turn
WPrompt ==
  /\ WCmd("prompt")
  /\ cmds' = Tail(cmds)
  /\ LET t == Cmd.t IN
     IF t \notin wSessions \/ wActive[t] # 0
       THEN /\ evts' = Append(evts, Msg("failed", t, NONE, 0, ""))
            /\ UNCHANGED <<wCancelled, wPendInt, cin>>
       ELSE /\ wCancelled' = [wCancelled EXCEPT ![t] = FALSE]
            /\ wPendInt' = [wPendInt EXCEPT ![t] = 0]
            /\ cin' = Append(cin, Msg("turn", t, Cmd.p, 0, ""))
            /\ UNCHANGED evts
  /\ UNCHANGED <<pane, reqs, bridge, mPhase, mTurn, mQueue, conv, forg, redCtr,
                 wSessions, wActive, cout, cxVars, ghostVars>>

\* stop_session(close)
PendingTurn(t) ==
  \/ \E i \in 1..Len(cin) : cin[i].k = "turn" /\ cin[i].t = t
  \/ \E i \in 1..Len(cout) : cout[i].k \in {"turn_ok", "turn_err"} /\ cout[i].t = t

StopSession(close) ==
  LET t == Cmd.t
      known == t \in wSessions
      hasTurn == (known /\ wActive[t] # 0) \/ PendingTurn(t)
      newly == hasTurn /\ (~known \/ ~wCancelled[t])
      interrupt == known /\ wActive[t] # 0 /\ wPendInt[t] # wActive[t]
  IN /\ cmds' = Tail(cmds)
     /\ wCancelled' = IF known THEN [wCancelled EXCEPT ![t] = TRUE] ELSE wCancelled
     /\ wPendInt' = IF interrupt THEN [wPendInt EXCEPT ![t] = wActive[t]] ELSE wPendInt
     /\ cin' = IF interrupt THEN Append(cin, Msg("interrupt", t, NONE, wActive[t], "")) ELSE cin
     /\ evts' = IF newly THEN Append(evts, Msg("cancelled", t, NONE, 0, "")) ELSE evts
     /\ wSessions' = IF close THEN wSessions \ {t} ELSE wSessions
     /\ wActive' = IF close THEN [wActive EXCEPT ![t] = 0] ELSE wActive

WCancel ==
  /\ WCmd("cancel") /\ StopSession(FALSE)
  /\ UNCHANGED <<pane, reqs, bridge, mPhase, mTurn, mQueue, conv, forg, redCtr,
                 cout, cxVars, ghostVars>>

WClose ==
  /\ WCmd("close") /\ StopSession(TRUE)
  /\ UNCHANGED <<pane, reqs, bridge, mPhase, mTurn, mQueue, conv, forg, redCtr,
                 cout, cxVars, ghostVars>>

Out1 == Head(cout)
WOut(k) == bridge = "live" /\ cout # <<>> /\ Out1.k = k
WUnchangedBut == <<pane, reqs, bridge, mPhase, mTurn, mQueue, conv, forg, cmds, redCtr,
                   cxVars, ghostVars>>

WStarted ==
  /\ WOut("start_ok")
  /\ cout' = Tail(cout)
  /\ wSessions' = wSessions \cup {Out1.t}
  /\ wActive' = [wActive EXCEPT ![Out1.t] = 0]
  /\ wCancelled' = [wCancelled EXCEPT ![Out1.t] = FALSE]
  /\ evts' = Append(evts, Msg("created", Out1.t, NONE, 0, ""))
  /\ UNCHANGED <<WUnchangedBut, wPendInt, cin>>

WStartFailed ==
  /\ WOut("start_err")
  /\ cout' = Tail(cout)
  /\ evts' = Append(evts, Msg("failed", NONE, NONE, 0, ""))
  /\ UNCHANGED <<WUnchangedBut, wSessions, wActive, wCancelled, wPendInt, cin>>

WResumed ==
  /\ WOut("resume_ok")
  /\ cout' = Tail(cout)
  /\ wSessions' = wSessions \cup {Out1.t}
  /\ wActive' = [wActive EXCEPT ![Out1.t] = 0]
  /\ wCancelled' = [wCancelled EXCEPT ![Out1.t] = FALSE]
  /\ evts' = Append(evts, Msg("restored", Out1.t, NONE, 0, ""))
  /\ UNCHANGED <<WUnchangedBut, wPendInt, cin>>

WResumeFailed ==
  /\ WOut("resume_err")
  /\ cout' = Tail(cout)
  /\ evts' = Append(evts, Msg("restore_failed", Out1.t, NONE, 0, ""))
  /\ UNCHANGED <<WUnchangedBut, wSessions, wActive, wCancelled, wPendInt, cin>>

\* Pending::Turn response: record the turn, interrupt it if already cancelled.
WTurnStarted ==
  /\ WOut("turn_ok")
  /\ cout' = Tail(cout)
  /\ LET t == Out1.t
         known == t \in wSessions
         interrupt == known /\ wCancelled[t] /\ wPendInt[t] # Out1.n
     IN /\ wActive' = IF known THEN [wActive EXCEPT ![t] = Out1.n] ELSE wActive
        /\ wPendInt' = IF interrupt THEN [wPendInt EXCEPT ![t] = Out1.n] ELSE wPendInt
        /\ cin' = IF interrupt \/ ~known THEN Append(cin, Msg("interrupt", t, NONE, Out1.n, "")) ELSE cin
  /\ UNCHANGED <<WUnchangedBut, wSessions, wCancelled, evts>>

WTurnRejected ==
  /\ WOut("turn_err")
  /\ cout' = Tail(cout)
  /\ evts' = Append(evts, Msg("failed", Out1.t, NONE, 0, ""))
  /\ UNCHANGED <<WUnchangedBut, wSessions, wActive, wCancelled, wPendInt, cin>>

WInterruptAccepted ==
  /\ WOut("int_ok")
  /\ cout' = Tail(cout)
  /\ UNCHANGED <<WUnchangedBut, wSessions, wActive, wCancelled, wPendInt, cin, evts>>

\* send_pending_failure for Pending::Interrupt
WInterruptRejected ==
  /\ WOut("int_err")
  /\ cout' = Tail(cout)
  /\ LET t == Out1.t
         live == t \in wSessions /\ wActive[t] = Out1.n
     IN /\ wPendInt' = IF live THEN [wPendInt EXCEPT ![t] = 0] ELSE wPendInt
        /\ evts' = IF live THEN Append(evts, Msg("cancel_failed", t, NONE, 0, "")) ELSE evts
  /\ UNCHANGED <<WUnchangedBut, wSessions, wActive, wCancelled, cin>>

\* turn/completed for the active turn
WTurnCompleted ==
  /\ WOut("completed")
  /\ cout' = Tail(cout)
  /\ LET t == Out1.t
         match == t \in wSessions /\ wActive[t] = Out1.n
     IN /\ wActive' = IF match THEN [wActive EXCEPT ![t] = 0] ELSE wActive
        /\ wPendInt' = IF match THEN [wPendInt EXCEPT ![t] = 0] ELSE wPendInt
        /\ evts' = IF match THEN Append(evts, Msg("completed", t, NONE, 0, Out1.s)) ELSE evts
  /\ UNCHANGED <<WUnchangedBut, wSessions, wCancelled, cin>>

---------------------------------------------------------------------------
(* Codex app-server. It reads requests in order and writes one stream.    *)

In1 == Head(cin)
CxIn(k) == bridge = "live" /\ cin # <<>> /\ In1.k = k
CxUnchangedBut == <<edVars, wSessions, wActive, wCancelled, wPendInt>>

CxStartThread ==
  /\ CxIn("start")
  /\ cin' = Tail(cin)
  /\ \/ /\ nextThread <= Len(NewThreads)
        /\ LET t == NewThreads[nextThread] IN
           /\ cxThreads' = cxThreads \cup {t}
           /\ cxLoaded' = cxLoaded \cup {t}
           /\ cout' = Append(cout, Msg("start_ok", t, NONE, 0, ""))
        /\ nextThread' = nextThread + 1
     \/ /\ cout' = Append(cout, Msg("start_err", NONE, NONE, 0, ""))
        /\ UNCHANGED <<cxThreads, cxLoaded, nextThread>>
  /\ UNCHANGED <<CxUnchangedBut, cxRun, cxIntr, cxCtr, ghostVars>>

CxResumeThread ==
  /\ CxIn("resume")
  /\ cin' = Tail(cin)
  /\ \/ /\ In1.t \in cxThreads
        /\ cxLoaded' = cxLoaded \cup {In1.t}
        /\ cout' = Append(cout, Msg("resume_ok", In1.t, NONE, 0, ""))
     \/ /\ cout' = Append(cout, Msg("resume_err", In1.t, NONE, 0, ""))
        /\ UNCHANGED cxLoaded
  /\ UNCHANGED <<CxUnchangedBut, cxThreads, cxRun, cxIntr, cxCtr, nextThread, ghostVars>>

\* ContinueSession: read the source thread, start a new thread, inject history.
\* Success reports the new thread as restored; failure names the source.
CxContinueThread ==
  /\ CxIn("continue")
  /\ cin' = Tail(cin)
  /\ \/ /\ In1.t \in cxThreads /\ nextThread <= Len(NewThreads)
        /\ LET n == NewThreads[nextThread] IN
           /\ cxThreads' = cxThreads \cup {n}
           /\ cxLoaded' = cxLoaded \cup {n}
           /\ cout' = Append(cout, Msg("resume_ok", n, NONE, 0, ""))
        /\ nextThread' = nextThread + 1
     \/ /\ cout' = Append(cout, Msg("resume_err", In1.t, NONE, 0, ""))
        /\ UNCHANGED <<cxThreads, cxLoaded, nextThread>>
  /\ UNCHANGED <<CxUnchangedBut, cxRun, cxIntr, cxCtr, ghostVars>>

\* turn/start runs the message, unless the thread is not loaded or busy.
CxStartTurn ==
  /\ CxIn("turn")
  /\ cin' = Tail(cin)
  /\ LET t == In1.t IN
     IF t \in cxLoaded /\ cxRun[t] = 0
       THEN /\ cxCtr' = cxCtr + 1
            /\ cxRun' = [cxRun EXCEPT ![t] = cxCtr + 1]
            /\ cxIntr' = [cxIntr EXCEPT ![t] = FALSE]
            /\ started' = [started EXCEPT ![In1.p] = @ + 1]
            /\ cout' = Append(cout, Msg("turn_ok", t, In1.p, cxCtr + 1, ""))
       ELSE /\ cout' = Append(cout, Msg("turn_err", t, NONE, 0, ""))
            /\ UNCHANGED <<cxRun, cxIntr, cxCtr, started>>
  /\ UNCHANGED <<CxUnchangedBut, cxThreads, cxLoaded, nextThread,
                 crashes, resets, switches, submitted, consented>>

CxInterrupt ==
  /\ CxIn("interrupt")
  /\ cin' = Tail(cin)
  /\ LET t == In1.t IN
     IF cxRun[t] = In1.n
       THEN /\ cxIntr' = [cxIntr EXCEPT ![t] = TRUE]
            /\ cout' = Append(cout, Msg("int_ok", t, NONE, 0, ""))
       ELSE /\ cout' = Append(cout, Msg("int_err", t, NONE, In1.n, ""))
            /\ UNCHANGED cxIntr
  /\ UNCHANGED <<CxUnchangedBut, cxThreads, cxLoaded, cxRun, cxCtr, nextThread, ghostVars>>

CxFinishTurn(t) ==
  /\ bridge = "live" /\ cxRun[t] # 0
  /\ \E st \in (IF cxIntr[t] THEN {"interrupted"} ELSE {"completed", "failed"}) :
       cout' = Append(cout, Msg("completed", t, NONE, cxRun[t], st))
  /\ cxRun' = [cxRun EXCEPT ![t] = 0]
  /\ UNCHANGED <<CxUnchangedBut, cin, cxThreads, cxLoaded, cxIntr, cxCtr, nextThread, ghostVars>>

\* The app-server process exits. Unread events stay in the bridge channel.
Crash ==
  /\ bridge = "live" /\ crashes < MaxCrashes
  /\ bridge' = "dead" /\ crashes' = crashes + 1
  /\ cmds' = <<>> /\ cin' = <<>> /\ cout' = <<>>
  /\ wSessions' = {} /\ wActive' = [t \in Threads |-> 0]
  /\ wCancelled' = [t \in Threads |-> FALSE] /\ wPendInt' = [t \in Threads |-> 0]
  /\ cxLoaded' = {} /\ cxRun' = [t \in Threads |-> 0] /\ cxIntr' = [t \in Threads |-> FALSE]
  /\ UNCHANGED <<pane, reqs, mPhase, mTurn, mQueue, conv, forg, evts, redCtr,
                 cxThreads, cxCtr, nextThread, resets, switches, started, submitted, consented>>

---------------------------------------------------------------------------

Quiet ==
  /\ reqs = <<>> /\ cmds = <<>> /\ evts = <<>> /\ cin = <<>> /\ cout = <<>>
  /\ bridge # "dead" /\ \A t \in Threads : cxRun[t] = 0

Done == Quiet /\ UNCHANGED vars

Init ==
  /\ \/ /\ pane = Default /\ conv = NONE /\ cxThreads = {}
     \/ /\ pane = [Default EXCEPT !.session = Saved, !.resumeCwd = TRUE]
        /\ conv = Saved /\ cxThreads = {Saved}
  /\ reqs = <<>> /\ bridge = "none"
  /\ mPhase = [t \in Threads |-> "none"] /\ mTurn = [t \in Threads |-> 0]
  /\ mQueue = [t \in Threads |-> <<>>] /\ forg = {}
  /\ cmds = <<>> /\ evts = <<>> /\ redCtr = 0
  /\ wSessions = {} /\ wActive = [t \in Threads |-> 0]
  /\ wCancelled = [t \in Threads |-> FALSE] /\ wPendInt = [t \in Threads |-> 0]
  /\ cin = <<>> /\ cout = <<>>
  /\ cxLoaded = {} /\ cxRun = [t \in Threads |-> 0] /\ cxIntr = [t \in Threads |-> FALSE]
  /\ cxCtr = 0 /\ nextThread = 1
  /\ crashes = 0 /\ resets = 0 /\ switches = 0
  /\ started = [p \in Prompts |-> 0] /\ submitted = {} /\ consented = {}

Next ==
  \/ \E p \in UserPrompts : UserSubmit(p)
  \/ UserStop \/ UserSendQueued \/ UserDiscard \/ UserPrepare \/ UserReset
  \/ \E c \in {"retry", "continue", "dismiss"} : UserResolve(c)
  \/ \E t \in Threads : UserSwitch(t)
  \/ EdSubmit \/ EdStop \/ EdSendQueued \/ EdDiscard \/ EdReset \/ EdPrepare \/ EdResolve
  \/ EdNewSession \/ EdResumeSession \/ EdSwitchSession \/ EdPrompt
  \/ EdCancel \/ EdClose \/ EdForget
  \/ EdSessionCreated \/ EdSessionRestored \/ EdRestoreFailed
  \/ EdCompleted \/ EdFailed \/ EdCancelled \/ EdCancelFailed \/ EdBridgeLost
  \/ WNewSession \/ WResume \/ WPrompt \/ WCancel \/ WClose
  \/ WStarted \/ WStartFailed \/ WResumed \/ WResumeFailed
  \/ WTurnStarted \/ WTurnRejected \/ WInterruptAccepted \/ WInterruptRejected
  \/ WTurnCompleted
  \/ CxStartThread \/ CxResumeThread \/ CxContinueThread \/ CxStartTurn \/ CxInterrupt
  \/ \E t \in Threads : CxFinishTurn(t)
  \/ Crash
  \/ Done

Spec == Init /\ [][Next]_vars

Bounds ==
  /\ redCtr <= MaxRedTurns /\ cxCtr <= MaxCodexTurns
  /\ Len(reqs) <= MaxChan /\ Len(cmds) <= MaxChan /\ Len(evts) <= MaxChan
  /\ Len(cin) <= MaxChan /\ Len(cout) <= MaxChan

---------------------------------------------------------------------------
(* Rules.                                                                  *)

\* A message that reached Codex is never sent to Codex again by itself.
NoReplay == \A p \in Prompts : started[p] <= 1

\* Every message the user wrote ran, still waits somewhere, or the user let it go.
Waiting ==
  Held(pane) \cup {r.p : r \in Range(reqs)} \cup {r.p : r \in Range(cmds)}
  \cup {r.p : r \in Range(cin)} \cup UNION {Range(mQueue[t]) : t \in Threads}
NothingLost ==
  \A p \in submitted : p \in Waiting \/ started[p] > 0 \/ p \in consented

\* The pane never shows its conversation idle while Codex runs a turn in it.
IdleMeansIdle ==
  \A t \in Threads :
    ~(bridge = "live" /\ pane.session = t /\ pane.phase = "Idle"
      /\ t \in wSessions /\ cxRun[t] # 0)

\* While the pane shows a turn working, Red accepts that turn's edits.
RunningTurnCanEdit ==
  \A t \in Threads :
    (bridge = "live" /\ pane.session = t /\ pane.phase = "Working"
     /\ cxRun[t] # 0 /\ wActive[t] = cxRun[t] /\ ~cxIntr[t]) => mPhase[t] = "Running"

\* Codex does not keep running a turn of a conversation Red closed.
NoOrphanTurn ==
  \A t \in Threads :
    ~(bridge = "live" /\ cxRun[t] # 0 /\ ~cxIntr[t] /\ t \notin wSessions
      /\ ~(\E m \in Range(cin) : m.k = "interrupt" /\ m.t = t)
      /\ ~(\E m \in Range(cout) : m.k = "turn_ok" /\ m.t = t))

\* When everything is quiet the pane is not stuck in a busy phase.
NotStuck == Quiet => pane.phase = "Idle"

\* When everything is quiet a queued follow-up shows why it waits.
QueueExplained ==
  Quiet /\ pane.queue # <<>> => pane.paused \/ pane.interrupted # NONE

\* When everything is quiet the editor holds no hidden message.
NoHiddenQueue == Quiet => \A t \in Threads : mQueue[t] = <<>>

\* The pane says a reply was cut off only for a message Codex received.
CutOffMeansSent == pane.interrupted # NONE => started[pane.interrupted] > 0

\* The pane shows Stopping only for a message you asked to stop.
StopMeansYouStopped == pane.phase = "Stopping" /\ pane.inFlight # NONE => pane.inFlight \in consented

\* Probes, not rules.
MgrQueueEmpty == \A t \in Threads : mQueue[t] = <<>>

\* The app-server does not die between the close and forget requests of one
\* ForgetSession effect (consecutive requests in one editor tick).
CloseForgetAtomic == ~(Crash /\ reqs # <<>> /\ Head(reqs).k = "forget")

\* Probe: the app-server never dies while a prompt sits in the worker's
\* command queue or on Codex's stdin.
NoCrashInWorkerTransit ==
  ~(Crash /\ ((\E i \in 1..Len(cmds) : cmds[i].k = "prompt")
              \/ (\E i \in 1..Len(cin) : cin[i].k = "turn")))

\* Exclude the four roots already found, to look for others.
KnownRootsExcluded ==
  /\ ~(WInterruptRejected /\ ~(Out1.t \in wSessions /\ wActive[Out1.t] # 0))
  /\ ~(EdBridgeLost /\ conv # pane.session)
  /\ ~((EdSwitchSession \/ EdResumeSession) /\ bridge = "dead")
  /\ ~(WClose /\ \E m \in Range(cin) : m.k = "turn" /\ m.t = Cmd.t)

=============================================================================
