---
title: CodeAlmanac Wiki
topics: [wiki]
sources: []
---

# CodeAlmanac Wiki

This is the living wiki for this repository. It records the durable knowledge
the code cannot say: decisions, flows, invariants, incidents, gotchas, and
project context that future agents should not rediscover from scratch.

Start with [Getting Started](getting-started) when you need the main reading
paths through the Red editor, agent, Husk, sessions, and development areas.
Use [Concepts](concepts), [Architecture](architecture), [Guides](guides),
[Decisions](decisions), and [Reference](reference) when you want to browse by
page type instead of by subsystem.

## Notability Bar

Write a page when it preserves non-obvious knowledge that will help a future
agent work safely in this codebase.

Good pages explain:

- a decision that took research or trial-and-error
- a cross-file flow
- an invariant or gotcha not visible from one file
- an external dependency as this repo uses it
- a product or operational constraint that shapes future work

Do not write pages that restate nearby code.

## Topic Taxonomy

Topics live in `topics.yaml`. Pages are Markdown files directly under
`almanac/`, including nested folders.

## Links

Use normal Markdown links between pages. Put file evidence in `sources:`.
