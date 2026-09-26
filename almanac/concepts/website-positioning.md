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
  - id: website-guide
    type: file
    path: almanac/guides/website/refresh-public-site.md
  - id: deploy-public-site
    type: file
    path: almanac/guides/website/deploy-public-site.md
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
editor mockups [@website-direction]. Existing media can become stale whenever
the visible Agent, inline assist, Git, or navigation surfaces change, so site
work should recapture current Red flows instead of reusing old proposal-era
assets [@website-direction] [@readme]. Useful capture subjects are the file
picker, command discovery, Git workspace, theme browser, full Agent workflow,
inline assist, InlineHistory receipts, language packs, plugins, and detachable
sessions [@website-direction] [@readme] [@agent-workflow].

The design system should borrow from Red itself without turning design tokens
into unsupported feature claims. The `red` theme defines the dark background
`#101014`, foreground `#D8D8DE`, cursor/accent `#E5484D`, muted line numbers,
selection color, semantic diagnostic colors, and widget colors that can anchor
site components [@red-theme]. A theme-gallery capture should use documented
runtime controls such as `red -c 'theme = "<name>"'`, `red --runtime-files`, and
the shipped `Space t` theme browser rather than claiming an Ex `:theme` command
unless that command is verified in current code [@getting-started]
[@website-direction].

Prototype artifacts from the design transcript are design direction, not
production evidence. They can suggest Vim-style navigation, command-line
interaction, live theme switching from bundled colors, and scripted Pair or
Delegate demos, but every implementation pass must reopen the actual website
repository and verify the current stack, install routes, browser behavior, and
shipped Red shortcuts before publishing [@website-direction].

Installer URLs are a site constraint, not just marketing copy. The README
installation commands fetch `https://getred.dev/install.sh` and
`https://getred.dev/install.ps1`, so a site rebuild must preserve those public
paths or intentionally migrate the install documentation and release process
with review [@readme]. This boundary keeps [Red Editor](red-editor) and
[Agent-Attributed Edits](agent-attributed-edits) focused on current runtime
facts while preserving the external-site audience, media plan, and prototype
constraints for future website work.

Use [Refresh Public Site](../guides/website/refresh-public-site) for the
operating procedure behind public docs, recording evidence, video packaging,
AI-assisted media preparation, and shortcut source audits [@website-guide]. Use
[Deploy Public Site](../guides/website/deploy-public-site) when that work is
ready for OpenAI Sites versioning, production domain verification, or rollback
planning [@deploy-public-site].
