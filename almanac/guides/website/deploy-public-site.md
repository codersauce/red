---
title: "Deploy Public Site"
summary: "Use this guide when publishing getred.dev through OpenAI Sites, including the built-archive boundary, Codex connector calls, domain checks, and rollback path."
topics: [guides, website, operations]
sources:
  - id: website-readme
    type: file
    path: ../red-website/README.md
  - id: website-package
    type: file
    path: ../red-website/package.json
  - id: hosting-config
    type: file
    path: ../red-website/.openai/hosting.json
  - id: sites-build-plugin
    type: file
    path: ../red-website/build/sites-vite-plugin.ts
  - id: public-origin
    type: file
    path: ../red-website/app/public-origin.ts
  - id: deployment-transcript
    type: conversation
    path: /Users/fcoury/.claude/projects/-Users-fcoury-code-red/bb9593f8-adce-4010-bc45-ad18a2bcf438.jsonl
  - id: sites-session
    type: conversation
    path: /Users/fcoury/.codex/sessions/2026/09/26/rollout-2026-09-26T18-58-30-01a0dfba-17a6-7ad2-9025-e1bf987aa2f2.jsonl
---

# Deploy Public Site

Use this guide when publishing the Red marketing site to `getred.dev` and its
companion domains. The website is maintained in the sibling `red-website`
checkout, not in this repository, and it builds a vinext app into a Cloudflare
Worker rather than using a repo-owned Wrangler deployment flow [@website-readme].
OpenAI Sites owns production deployment for that Worker, so the release boundary
is a built `dist/` archive saved as a Sites version and then published with the
Sites connector [@sites-session]. For content, media, and shortcut
accuracy before publication, use [Refresh Public Site](refresh-public-site).

## Reopen The Live State

Do not infer production state from branch names, local worktrees, or an earlier
deployment note. A read-only Sites audit on 2026-09-26 found project
`appgprj_6a5d1afa50688191845035d9452f52b4`, active public URL
`https://getred.dev`, no preview URL, and live version 19 at commit
`97c8106ffc10d89e623c745e72003ebc283ee5fa` with deployment
`appgdep_6aaef6e922ac8191bf111ea7693fc748` [@sites-session]. Treat those
IDs as a rollback reference and historical snapshot, not as current truth.
Before each deploy, call the Sites read methods again and record the current
live version ID and deployment ID [@sites-session].

The same audit found four active custom domains: `getred.dev`,
`getrededitor.com`, `rededitor.dev`, and `rededitor.app`; the generated
`red-editor.fcoury.chatgpt.site` URL is not a custom-domain record
[@sites-session]. The website code also treats those five hosts as public
hosts for origin-aware install commands and metadata [@public-origin]. All five
hosts returned `200` without redirects during the 2026-09-26 check, and the
available Sites domain tools did not expose a redirect setting
[@sites-session]. If canonical-host redirects are added, implement and test
them in the Worker or another routing layer, then recheck installer behavior
through each host.

The September 2026 deployment audit found a real drift failure: the live site was
serving the `codex/red-docs-foundation` branch content that showed Red `v0.2.4`,
while the newer `origin/main` launch content showed Red `v0.7.0` and included
`/releases` [@deployment-transcript]. A homepage redesign should preserve both
the full docs branch content and the current release surface before deployment,
or the site can regress by publishing one side of that split over the other
[@deployment-transcript].

The follow-up Direction A deployment fixed that split by merging the launch
content and docs ancestry before saving a new Sites version [@sites-session].
It published version 20 for the redesigned homepage, version 22 for the primary
installer-command correction, and version 23 for the v0.8.0 release page and
docs update across all five public hosts [@sites-session]. Those
version numbers are historical waypoints only. They prove the Codex Sites
connector can perform the save-then-deploy sequence in an authorized Codex
session, but a later deploy still has to reopen the live version, deployment ID,
source-branch head, and host checks before changing production.

## Build The Archive

Deploy from an exact pushed commit in `red-website`. The Sites save operation
expects `commit_sha` to be the full SHA at the head of the site's configured
remote source branch, and the uploaded archive must be built from that same
source state [@sites-session]. This is a stricter requirement
than "the commit exists somewhere on GitHub": a save attempt can fail when the
public branch has the desired commit but the Sites source branch still points at
an older head [@sites-session]. The site requires Node
`>=22.13.0`; `npm test` runs the installer drift check, `npm run build`, and
rendered HTML assertions [@website-package]. Also run `npm audit --omit=dev`
against the same export before saving a production version: the v0.8.0 website
release found production dependency advisories in an otherwise unchanged
lockfile after the release page and docs were already live, and that count is
audit-time state rather than a durable release fact [@sites-session]. The README
requires installers to be synced only to an actually published Red release
commit, because `public/installers.json` feeds the release version shown in the
site [@website-readme].

The production artifact is the built `dist/` tree. `npm run build` runs
`vinext build`, and the Sites Vite plugin copies `.openai/hosting.json` into
`dist/.openai/` after compilation [@website-package] [@sites-build-plugin]. The
current hosting config contains project ID
`appgprj_6a5d1afa50688191845035d9452f52b4` with no D1 or R2 binding
[@hosting-config]. The deploy archive should therefore contain `dist/`, including
`dist/.openai/hosting.json`, `dist/server/index.js`, and static client assets;
do not package the source tree or omit the Worker output [@sites-session].

Use an isolated export for the commit being deployed so dirty local state cannot
enter the archive:

```shell
repo="$HOME/code/red-website"
sha="<full-sha>"
release_dir="$(mktemp -d /tmp/red-site-release.XXXXXX)"
git -C "$repo" archive --format=tar "$sha" | tar -xf - -C "$release_dir"
cd "$release_dir"
npm ci
npm test
npm audit --omit=dev
archive="/tmp/red-website-${sha}.tar.gz"
tar -czf "$archive" -C "$release_dir" dist
tar -tzf "$archive" | rg '^dist/(\.openai/hosting\.json|server/index\.js)$'
```

This command reproduces the observed archive root layout from the successful
Sites deployment audit [@sites-session]. Keep the temporary export and
archive until `save_site_version` succeeds, so a failed save can be inspected
without rebuilding.

If the dependency audit reports advisories, fix them before deployment or carry
an explicit applicability note for the vinext Worker. The v0.8.0 release left a
direct critical Next.js advisory unresolved because its applicability to the
Worker deployment had not been verified [@sites-session].

## Save And Publish

Sites tools are available through the Codex connector rather than a standalone
repo CLI. A read-only `codex exec --ephemeral -s read-only -C
/Users/fcoury/code/red-website ...` call successfully invoked
`sites.get_deployment_status` on 2026-09-26 [@sites-session]. Later in the
same launch sequence, an explicitly authorized Codex session used the Sites
connector to save and publish production versions, then poll deployment status
until success [@sites-session]. Treat that as proof of the Codex
connector path, not as permission to deploy: every future `save_site_version`
and `deploy_site_version` call still needs explicit production authorization and
fresh rollback IDs.

After recording the current live version for rollback, verify that the configured
Sites source branch points at the exact commit that produced the archive. Then
ask Codex to call `sites.save_site_version` with the project ID, that full
commit SHA, and the archive path for the built `dist/` tarball
[@sites-session]. Record the returned
version ID, version number, commit SHA, and archive metadata. Then ask Codex to
publish that saved version with `sites.deploy_site_version`, because the site is
currently public; use `deploy_private_site_version` only if a fresh Sites read
shows the access mode has changed [@sites-session].

Poll `sites.get_deployment_status` with the new deployment ID until it reaches a
terminal status. A successful deploy is not complete until the generated Sites
URL and every custom host have been checked [@sites-session].

## Verify And Roll Back

Verification should cover the release-specific surface, not just the home page.
Check `https://getred.dev`, `getrededitor.com`, `rededitor.dev`,
`rededitor.app`, and the generated Sites host; verify `/install.sh`,
`/install.ps1`, `/installers.json`, `/docs`, representative docs routes,
`/releases`, `/og.png`, canonical and social-card metadata, and any new media
assets [@sites-session] [@website-readme]. Compare installer checksums
against the pinned `public/installers.json` values and confirm the page displays
the intended published Red release, not a local `main` version
[@website-readme].

The homepage installer UI has a separate product contract from the raw installer
files. The corrected live Direction A page uses `curl -fsS
https://getred.dev/install.sh | sh` as the primary macOS/Linux command, keeps
Homebrew as an alternative, and uses the PowerShell installer for Windows
[@sites-session]. Include that browser-visible command ordering
in the post-deploy check so a visual cleanup does not accidentally make
Homebrew look like the primary cross-platform path again.

Rollback redeploys an existing saved Sites version. List versions, choose the
exact previous version ID, call `sites.deploy_site_version` for that ID, poll the
new deployment ID, and rerun the same domain and installer checks
[@sites-session]. A rollback of a saved version does not need a rebuild or
`save_site_version`, but the current live IDs and access mode still need to be
reopened immediately before doing it [@sites-session].
