# Agent pane look-and-feel plan

The agent pane should make three things obvious at a glance: who said what, what
the agent is doing now, and what the user can do next. Improvements should stay
useful in narrow terminals and should not turn transient UI state into durable
conversation history.

## 1. Make turn state visible

- Show an agent-side waiting row immediately after prompt submission.
- Replace the generic waiting label with concrete tool activity when available.
- Mirror active, stopping, and ready states in the footer with distinct markers.
- Replace it with a stopping state on cancellation, then remove it on response,
  completion, or failure.

This phase is implemented. The waiting row is semantic, muted, excluded from
copy-all, and never written to transcript storage.

## 2. Strengthen conversation hierarchy

- Keep role labels visually distinct while reducing repeated chrome in long turns.
- Give errors, interruptions, and proposal-ready notices consistent semantic styles.
- Group tool activity under the turn that caused it instead of mixing it with
  durable assistant prose.

## 3. Clarify actions and outcomes

- Surface proposal counts as a compact action near the relevant answer.
- Make queued follow-ups and cancellation state visible without relying on print
  messages outside the pane.
- Keep the primary next action first in narrow footer layouts.

## 4. Polish responsive behavior

- Test the pane at narrow, default, and wide terminal sizes.
- Preserve readable wrapping and stable scroll position as status rows appear.
- Use text and shape, not color alone, for every state.
- Add render-level coverage for each state and interaction-level coverage for
  submission, activity, streaming, clearing, cancellation, and recovery.
