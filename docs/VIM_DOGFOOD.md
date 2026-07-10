# Vim compatibility dogfood protocol

This is the durable evidence log for the Phase 1 external-user gate. Automated tests do
not substitute for the two required one-week real-work trials.

## Tester protocol

Each tester uses the same Red build for five working days on their normal source tree.
They should use their normal keyboard rather than a prescribed demo and record every
surprise against the versioned compatibility matrix.

At minimum, the week must exercise:

- counts, operators, motions, and at least three supported text objects;
- named macros, `@@`, counted playback, and dot-repeat across distinct locations;
- insert, visual, visual-line, and visual-block changes;
- search, substitution with confirmation, command cancellation, undo, and redo;
- local/global/special marks and backward/forward jumplist traversal;
- Unicode text, an empty buffer, both final-line forms, wrapped lines, and two windows.

## Issue classification

Use exactly one result for every observed difference:

- **implementation bug** — behavior contradicts a supported matrix row;
- **matrix correction** — behavior is stable but was described incorrectly;
- **intentional difference** — accepted and documented before launch;
- **not yet supported** — explicitly outside the supported claim;
- **release-blocking compatibility** — prevents normal work and must be resolved or the
  relevant matrix row must be removed from the launch claim.

Absence of a report is not a pass. The tester signs off on each required area and links
all issues below.

## Trial records

| Tester | Build/commit | Dates | Repository/work | Required areas completed | Issues | Sign-off |
|---|---|---|---|---|---|---|
| _unassigned_ | | | | no | | pending |
| _unassigned_ | | | | no | | pending |

## Launch decision

- [ ] Two external Vim-native testers completed five working days each.
- [ ] Every required area has explicit evidence from both testers.
- [ ] Every implementation bug is fixed or its row is no longer marked supported.
- [ ] No unresolved issue is labeled **release-blocking compatibility**.
- [ ] `cargo test --all-targets --all-features` passes on the release candidate.
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes on the release
      candidate.

**Current decision:** pending external trials. This status must not be rewritten as
“zero reports.”
