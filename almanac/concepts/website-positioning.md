---
title: "Website Positioning"
summary: "The external Red website should use evidence-first product positioning while keeping prototype ideas separate from runtime support claims."
topics: [concepts, red-editor, website]
sources:
  - id: readme
    type: file
    path: README.md
  - id: website-direction
    type: conversation
    path: /Users/fcoury/.claude/projects/-Users-fcoury-code-red/acaa6873-03ae-4f92-a325-63c9b68fef5b.jsonl
---

# Website Positioning

Website positioning is durable product context for `getred.dev`, not a runtime
specification for Red. The current external-site direction targets developers
who already know code editors, especially Neovim users who need Vim muscle
memory to transfer [@website-direction]. Production copy can lead with Red's
AI-native work model and batteries-included runtime, but it must verify exact
shipped shortcuts and feature names against repository docs: the README
currently documents `Space A` for the full Agent, `Space i` for inline assist,
`Space t` for theme browsing, bundled plugins and themes, language packs, and
detachable sessions [@readme].

Visual work for `getred.dev` is evidence-first. The selected direction calls for
screenshots and videos of real Red project workflows rather than placeholder
editor mockups [@website-direction]. One prototype intentionally made the page
behave like Red with Normal-mode navigation, live theme switching, and scripted
Pair and Delegate demos [@website-direction]. Treat that prototype as design
direction, not runtime truth: features or keys that are not supported by the
current code or docs, such as a dedicated `Space D` Delegate entry point, must
stay marked as proposed until implementation catches up [@website-direction]
[@readme].

This boundary keeps [Red Editor](red-editor) focused on current runtime product
facts while preserving the external-site audience and media direction for future
website work.
