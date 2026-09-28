---
title: "Refresh Public Site"
summary: "Use this guide when updating getred.dev docs, media, or interactive website concepts without turning prototypes or stale captures into Red runtime claims."
topics: [guides, website, red-editor]
sources:
  - id: readme
    type: file
    path: README.md
  - id: getting-started
    type: file
    path: docs/GETTING_STARTED.md
  - id: agent-workflow
    type: file
    path: docs/AGENT_WORKFLOW.md
  - id: website-positioning
    type: file
    path: almanac/concepts/website-positioning.md
  - id: docs-foundation-session
    type: conversation
    path: /Users/fcoury/.codex/sessions/2026/09/19/rollout-2026-09-19T16-29-45-01a0bb25-67be-7920-9517-c9924483caef.jsonl
  - id: default-config
    type: file
    path: default_config.toml
  - id: git-plugin
    type: file
    path: plugins/git.hk
  - id: keyboard-doc
    type: file
    path: docs/KEYBOARD.md
  - id: deploy-public-site
    type: file
    path: almanac/guides/website/deploy-public-site.md
  - id: website-readme
    type: file
    path: ../red-website/README.md
  - id: website-design-a
    type: file
    path: ../red-website/worker/design-a.html
  - id: website-captures
    type: file
    path: ../red-website/captures/README.md
  - id: direction-a-deploy-session
    type: conversation
    path: /Users/fcoury/.codex/sessions/2026/09/26/rollout-2026-09-26T18-58-30-01a0dfba-17a6-7ad2-9025-e1bf987aa2f2.jsonl
---

# Refresh Public Site

Use this guide when updating `getred.dev` documentation, captures, walkthroughs,
or interactive website concepts. The successful outcome is a public page or
media asset that matches current Red behavior, preserves installer URLs, and
keeps prototype language separate from shipped commands. Start with [Website
Positioning](../../concepts/website-positioning) for the product boundary, then
use this page as the operating checklist for evidence and production assets
[@website-positioning].

## Keep Public Docs Task-First

Public documentation is organized around what a reader is trying to accomplish,
not around Red's internal architecture. A website foundation pass rejected
copying internal architecture, CI notes, plans, and implementation history into
public docs; public pages should publish reader tasks, verified commands,
expected results, and only the caveats that affect using Red
[@docs-foundation-session]. Architecture explanations belong in this Almanac or
repository docs unless the site is intentionally redesigned.

Use current Red source or versioned user docs as the evidence gate for commands,
keybindings, defaults, product names, and platform support
[@docs-foundation-session]. The accepted route map covers installation, a first
session, editor navigation, search, windows, language support, Git, Agent,
configuration, keybindings, themes, plugins, Husk, sessions, CLI, Vim
compatibility, and troubleshooting [@docs-foundation-session].

Preserve the installer paths unless the release process and install docs are
changed deliberately. The README sends users to
`https://getred.dev/install.sh` and `https://getred.dev/install.ps1`, so a site
refresh must keep those routes working or update the repository documentation
and release workflow at the same time [@readme]. When a refresh is ready to go
live, use [Deploy Public Site](deploy-public-site) for the OpenAI Sites version,
domain, archive, verification, and rollback procedure [@deploy-public-site].

## Audit Before Recording

Before recording a workflow, verify the exact shortcut and command path against
the current source or maintained docs. This is especially important for
navigation, Git, Agent, and inline-assist scenes because they are visible in
marketing media and easy to make obsolete [@docs-foundation-session]
[@getting-started] [@agent-workflow].

Two shortcut traps from the media pass are durable checks. Current Red maps
normal-mode `D` to `ShowLineDiagnostics`, not delete-to-end-of-line, and
`Space c c` dispatches `GitSubmitMessage`, whose command title is "Submit Git
message", rather than opening the commit editor [@default-config]
[@git-plugin]. The manual Git commit opening path starts from the Git dashboard
commit menu and selects "Write message" [@git-plugin].

Agent prompt videos must match the current composer keyboard contract. The
Agent dialog and conversation footer send with Enter or Ctrl+Enter, while
Alt+Enter, Shift+Enter, and Ctrl+J insert a newline [@keyboard-doc].

## Capture Real Workflows

Screenshots and videos should show real Red project workflows rather than
placeholder editor mockups [@website-positioning]. Practical first captures are
a navigation pilot that opens Red on the Red repository and exercises `Ctrl-p`,
the file tree, `Space ?`, and `F1`; a search walkthrough from `ThemeBrowser` to
its implementation; a Git workflow that inspects hunks and stages only selected
changes; and a split-window workflow across related files
[@docs-foundation-session]. Agent and inline-assist walkthroughs should come
after these because authentication, model output, and interface changes create
more recording drift [@docs-foundation-session].

Documentation videos should remain subordinate to the written docs. Use short
inline motion clips for page-local interactions, 90-second to 3-minute
walkthroughs for realistic tasks, and rare longer deep dives only when a
workflow spans several Red areas [@docs-foundation-session]. Put videos on the
relevant documentation pages, with the written page as the source of truth
[@docs-foundation-session].

## Keep Homepage Media Lazy

Homepage media is part of the user experience, not only content. The Direction A
performance investigation for issue 366 could not reproduce the reported freeze
locally, but it did find that the page declared six 1600x900 videos and that a
browser had enough data to play all six after load, including five recordings
below the fold [@direction-a-deploy-session]. Treat `preload="metadata"` as an
insufficient guard for these homepage loops: before publishing media changes,
verify which recordings have fetched playable data, which videos are decoding,
and whether any offscreen ticker or loop continues running [@direction-a-deploy-session].

The safer homepage pattern is poster-first media. Keep posters visible at first
paint, load recordings when they enter view or when a visitor chooses playback,
and compare reload, largest-contentful-paint, and a real interaction under CPU
throttling before claiming the performance issue is fixed
[@direction-a-deploy-session]. This does not replace the recording-source
requirements below; it adds a runtime loading check so authentic media does not
make the public site feel broken on lower-end hardware.

## Package Media For Replacement

Each production video package should preserve the editable recording, MP4/WebM
exports, poster, captions, transcript, script or shot list, demo reset state,
Red commit, docs route, recording date, duration, and verification status. This
lets a changed scene be replaced without rerecording the whole walkthrough
[@docs-foundation-session].

AI tooling may help produce Red documentation media, but it is not the
authority for demonstrated behavior. Use it to verify shortcuts against pinned
Red source, prepare scripts and repeatable demo state, draft captions and page
copy, inspect rendered frames for readability and private information, and
compare the final transcript and visible actions against current docs and
implementation [@docs-foundation-session]. Published narrated videos should use
a human, conversational voice after the screen edit; synthetic narration is
useful only for timing drafts [@docs-foundation-session].

## Find The Production Assets

The production Direction A homepage lives in the sibling `red-website`
checkout, not in the exploratory `site/` prototype tree in this repository. The
website README names `worker/design-a.html` as the standalone Direction A
homepage and says the Worker serves that file at `/`, while `/docs`,
documentation routes, and `/releases` come from the app router
[@website-readme]. The page itself keeps the Docs and Releases links, points its
social metadata at `https://getred.dev/og.png`, and references six `/media/*.mp4`
clips with matching posters [@website-design-a].

Homepage recording sources are also in `red-website`. The capture README says
the six silent MP4 loops and posters live in `public/media/`, while the editable
VHS tapes, reset fixture, capture settings, postprocessor, and social-card
source live under `captures/` [@website-captures]. Those files were copied from
the Red repository's `site/captures/` prototype on 2026-09-26, and the exact
Red binary commit used for the original recordings was not recorded
[@website-captures]. Re-recording therefore needs a current Red binary plus a
fresh visual verification pass, not just a rebuild of the website.
