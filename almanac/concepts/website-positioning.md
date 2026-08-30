---
title: "Website Positioning"
summary: "The external Red website should use evidence-first product positioning while keeping prototype ideas separate from runtime support claims."
topics: [concepts, red-editor, website]
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
  - id: red-theme
    type: file
    path: themes/red.json
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
shipped shortcuts, command names, and feature names against repository docs
before presenting them as current product behavior [@readme].

The site may use "Pair Mode" and "Delegate Mode" as external product language,
but those names must stay separate from shipped editor surfaces until the
product adds matching commands or keys [@website-direction]. Current docs expose
the interactive agent path as `Space A` or `:Agent` for the full Agent panel and
`Space i` for bounded inline assist [@readme] [@getting-started]
[@agent-workflow]. Background inline work is documented as hidden jobs that can
finish without moving focus, notify the bottom line, reopen through `Space N` or
`:InlineLast`, and remain inspectable in `Space H` / `:InlineHistory`
[@agent-workflow]. A website section can explain that workflow as delegated
work, but a dedicated `Space D` Delegate entry point is still a proposed site
demo detail, not a current runtime shortcut [@website-direction] [@readme].

Visual work for `getred.dev` is evidence-first. The selected direction calls for
screenshots and videos of real Red project workflows rather than placeholder
editor mockups [@website-direction]. The transcript's design pass treated Red
`0.6.0` as the media baseline and rejected older captures that still showed the
old proposal-era agent UI, so future media work should recapture current Red
flows instead of reusing stale assets [@website-direction] [@readme]. Useful
capture subjects are the file picker, command discovery, Git workspace, theme
browser, full Agent workflow, inline assist, InlineHistory receipts, language
packs, plugins, and detachable sessions [@website-direction] [@readme]
[@agent-workflow].

The design system should borrow from Red itself without turning design tokens
into unsupported feature claims. The `red` theme defines the dark background
`#101014`, foreground `#D8D8DE`, cursor/accent `#E5484D`, muted line numbers,
selection color, semantic diagnostic colors, and widget colors that can anchor
site components [@red-theme]. A theme-gallery capture should use documented
runtime controls such as `red -c 'theme = "<name>"'`, `red --runtime-files`, and
the shipped `Space t` theme browser rather than claiming an Ex `:theme` command
unless that command is verified in current code [@getting-started]
[@website-direction].

Two design artifacts came out of the transcript and are useful references, but
neither is production evidence. The blueprint artifact records the content,
design-system, and media plan at
`https://claude.ai/code/artifact/3be93017-a5ae-4b19-bf36-b4f1b7af66e0`; the
second prototype makes the website behave like Red in Normal mode at
`https://claude.ai/code/artifact/9416ad22-5810-4e9d-81d4-40d03d189baf`
[@website-direction]. The second concept includes Vim-style navigation,
search, visual selection, a command line, live theme switching from bundled
theme colors, and scripted Pair and Delegate demos [@website-direction]. Keep
those artifacts as design direction. Before implementing them, reopen the
website repository and verify the current stack, worktree, browser console,
install routes, and shipped Red shortcuts [@website-direction].

Installer URLs are a site constraint, not just marketing copy. The README
installation commands fetch `https://getred.dev/install.sh` and
`https://getred.dev/install.ps1`, so a site rebuild must preserve those public
paths or intentionally migrate the install documentation and release process
with review [@readme]. This boundary keeps [Red Editor](red-editor) and
[Agent-Attributed Edits](agent-attributed-edits) focused on current runtime
facts while preserving the external-site audience, media plan, and prototype
constraints for future website work.
