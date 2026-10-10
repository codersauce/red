---
title: "Release Operations"
summary: "Release operations route Red publication work across the editor release runbook, release campaign manifest, CI and archive mechanics, installer smoke checks, website coordination, and language-pack releases."
topics: [guides, release, navigation]
sources:
  - id: release-red
    type: file
    path: almanac/guides/releases/release-red.md
  - id: release-campaign
    type: file
    path: almanac/reference/releases/release-campaign.md
  - id: validation
    type: file
    path: almanac/reference/validation/ci-and-validation.md
  - id: installers
    type: file
    path: almanac/guides/installers/release-installers.md
  - id: language-pack
    type: file
    path: almanac/guides/plugins/release-language-pack.md
  - id: website
    type: file
    path: almanac/guides/website/README.md
---

# Release Operations

Release operations are the reading hub for Red publication work. Start here
when a task might touch the editor release, reviewed release copy, CI release
jobs, archive smoke tests, installers, the public website, or first-party
language-pack publication. The main editor release runbook is [Release Red](release-red),
but the surrounding pages split exact workflow mechanics, installer checks,
website coordination, and language-pack releases so one release task does not
silently borrow assumptions from another [@release-red] [@validation]
[@installers] [@language-pack] [@website].

## Editor Release Path

Use [Release Red](release-red) for the end-to-end editor release sequence:
prepare-release workflow, release PR review, annotated tag, archive smoke
tests, draft GitHub release review, Homebrew publication, installer checks,
website coordination, and announcement [@release-red]. It is the task guide
for publishing a Red version.

Use [Release Campaign](../../reference/releases/release-campaign) when the work
is the reviewed message source: exact version resolution, story order, GitHub
release introduction, Discord and in-app highlights, and preview-only X and
Bluesky copy [@release-campaign]. Campaign editing belongs before publication,
not as an after-the-fact announcement rewrite.

Use [CI and validation](../../reference/validation/ci-and-validation) when the
question is what a workflow or job proves. That reference owns the exact main
CI, release workflow, Homebrew, installer-workflow, security-audit, and
performance mechanics [@validation]. The release guide should decide task
order; the validation reference should answer job-shape and trigger questions.

## Adjacent Release Surfaces

Use [Release Installers](../installers/release-installers) when the change is
inside `install/install.sh`, `install/install.ps1`, installer tests, checksum
handling, supported platforms, or latest-release smoke coverage [@installers].
Installer verification depends on the editor release assets, but the installer
guide owns script-specific fixture and platform behavior.

Use [Website Operations](../website) when the public site, release pages,
installer routes, media, OpenAI Sites deployment, custom domains, or rollback
become part of the release closeout [@website]. The website is a separate
publication surface and needs fresh source, Sites, dependency-audit, and host
evidence before it can be called current.

Use [Release A Language Pack](../plugins/release-language-pack) for first-party
language-pack publication. That path validates a shared language-pack source,
tags one pack version, waits for catalog publication, and verifies the catalog
entry Red consumes [@language-pack]. It is related to editor releases but does
not replace the editor release runbook.
