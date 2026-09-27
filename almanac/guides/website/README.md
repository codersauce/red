---
title: "Website Operations"
summary: "Guide hub for Red public-site work, from positioning and media refreshes to OpenAI Sites deployment and rollback."
topics: [guides, website, navigation]
sources:
  - id: topics
    type: file
    path: almanac/topics.yaml
  - id: positioning
    type: file
    path: almanac/concepts/website-positioning.md
  - id: refresh
    type: file
    path: almanac/guides/website/refresh-public-site.md
  - id: deploy
    type: file
    path: almanac/guides/website/deploy-public-site.md
---

# Website Operations

Website operations are the task guides for `getred.dev` work. The topic graph
groups them under the `website` topic so future maintainers can find the
external-site product boundary, content refresh workflow, and deployment
procedure together [@topics]. Use this hub when the work touches public docs,
homepage media, installer routes, OpenAI Sites, custom domains, or rollback.

Start with [Website positioning](../../concepts/website-positioning) before
editing copy or media. That concept page keeps external product language,
prototype ideas, real Red shortcuts, media evidence, and installer URL
constraints separate from shipped editor runtime claims [@positioning].

Use [Refresh public site](refresh-public-site) when changing website content,
captures, walkthroughs, interactive homepage concepts, or production media. It
is the guide for checking public claims against current Red docs and source,
keeping prototype material out of runtime claims, finding the production
`red-website` assets, and preparing replaceable media packages [@refresh].

Use [Deploy public site](deploy-public-site) only when refreshed content is
ready to publish or roll back. It covers the sibling `red-website` checkout,
the built `dist/` archive boundary, Sites connector calls, custom-domain
checks, installer-route verification, and rollback to a saved Sites version
[@deploy].

For editor release work that merely needs a reminder to coordinate the public
site, stay with [Release Red](../releases/release-red). Move into this hub when
the actual public-site content, media, deployment, or domain state becomes part
of the task.
