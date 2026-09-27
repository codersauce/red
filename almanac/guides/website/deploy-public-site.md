---
title: "Deploy Public Site"
summary: "Use this guide when publishing getred.dev through OpenAI Sites, including the built-archive boundary, Codex connector calls, domain checks, and rollback path."
topics: [guides, website, operations]
sources:
  - id: deployment-findings
    type: file
    path: ../red-website/SITES_DEPLOY_FINDINGS.md
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
  - id: direction-a-deploy-transcript
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
Sites connector [@deployment-findings]. For content, media, and shortcut
accuracy before publication, use [Refresh Public Site](refresh-public-site).

## Reopen The Live State

Do not infer production state from branch names, local worktrees, or an earlier
deployment note. A read-only Sites audit on 2026-09-26 found project
`appgprj_6a5d1afa50688191845035d9452f52b4`, active public URL
`https://getred.dev`, no preview URL, and live version 19 at commit
`97c8106ffc10d89e623c745e72003ebc283ee5fa` with deployment
`appgdep_6aaef6e922ac8191bf111ea7693fc748` [@deployment-findings]. Treat those
IDs as a rollback reference and historical snapshot, not as current truth.
Before each deploy, call the Sites read methods again and record the current
live version ID and deployment ID [@deployment-findings].

The same audit found four active custom domains: `getred.dev`,
`getrededitor.com`, `rededitor.dev`, and `rededitor.app`; the generated
`red-editor.fcoury.chatgpt.site` URL is not a custom-domain record
[@deployment-findings]. The website code also treats those five hosts as public
hosts for origin-aware install commands and metadata [@public-origin]. All five
hosts returned `200` without redirects during the 2026-09-26 check, and the
available Sites domain tools did not expose a redirect setting
[@deployment-findings]. If canonical-host redirects are added, implement and test
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
content and docs ancestry before saving a new Sites version [@direction-a-deploy-transcript].
It published version 20 for the redesigned homepage and later version 22 for the
primary installer-command correction [@direction-a-deploy-transcript]. Those
version numbers are historical waypoints only. They prove the Codex Sites
connector can perform the save-then-deploy sequence in an authorized Codex
session, but a later deploy still has to reopen the live version, deployment ID,
source-branch head, and host checks before changing production.

## Build The Archive

Deploy from an exact pushed commit in `red-website`. The site requires Node
`>=22.13.0`; `npm test` runs the installer drift check, `npm run build`, and
rendered HTML assertions [@website-package]. The README requires installers to
be synced only to an actually published Red release commit, because
`public/installers.json` feeds the release version shown in the site
[@website-readme].

The production artifact is the built `dist/` tree. `npm run build` runs
`vinext build`, and the Sites Vite plugin copies `.openai/hosting.json` into
`dist/.openai/` after compilation [@website-package] [@sites-build-plugin]. The
current hosting config contains project ID
`appgprj_6a5d1afa50688191845035d9452f52b4` with no D1 or R2 binding
[@hosting-config]. The deploy archive should therefore contain `dist/`, including
`dist/.openai/hosting.json`, `dist/server/index.js`, and static client assets;
do not package the source tree or omit the Worker output [@deployment-findings].

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
archive="/tmp/red-website-${sha}.tar.gz"
tar -czf "$archive" -C "$release_dir" dist
tar -tzf "$archive" | rg '^dist/(\.openai/hosting\.json|server/index\.js)$'
```

This command reproduces the observed archive root layout from the successful
Sites deployment audit [@deployment-findings]. Keep the temporary export and
archive until `save_site_version` succeeds, so a failed save can be inspected
without rebuilding.

## Save And Publish

Sites tools are available through the Codex connector rather than a standalone
repo CLI. A read-only `codex exec --ephemeral -s read-only -C
/Users/fcoury/code/red-website ...` call successfully invoked
`sites.get_deployment_status` on 2026-09-26 [@deployment-findings]. Later in the
same launch sequence, an explicitly authorized Codex session used the Sites
connector to save and publish production versions, then poll deployment status
until success [@direction-a-deploy-transcript]. Treat that as proof of the Codex
connector path, not as permission to deploy: every future `save_site_version`
and `deploy_site_version` call still needs explicit production authorization and
fresh rollback IDs.

After recording the current live version for rollback, ask Codex to call
`sites.save_site_version` with the project ID, full commit SHA, and archive path
for the built `dist/` tarball [@deployment-findings]. Record the returned
version ID, version number, commit SHA, and archive metadata. Then ask Codex to
publish that saved version with `sites.deploy_site_version`, because the site is
currently public; use `deploy_private_site_version` only if a fresh Sites read
shows the access mode has changed [@deployment-findings].

Poll `sites.get_deployment_status` with the new deployment ID until it reaches a
terminal status. A successful deploy is not complete until the generated Sites
URL and every custom host have been checked [@deployment-findings].

## Verify And Roll Back

Verification should cover the release-specific surface, not just the home page.
Check `https://getred.dev`, `getrededitor.com`, `rededitor.dev`,
`rededitor.app`, and the generated Sites host; verify `/install.sh`,
`/install.ps1`, `/installers.json`, `/docs`, representative docs routes,
`/releases`, `/og.png`, canonical and social-card metadata, and any new media
assets [@deployment-findings] [@website-readme]. Compare installer checksums
against the pinned `public/installers.json` values and confirm the page displays
the intended published Red release, not a local `main` version
[@website-readme].

The homepage installer UI has a separate product contract from the raw installer
files. The corrected live Direction A page uses `curl -fsS
https://getred.dev/install.sh | sh` as the primary macOS/Linux command, keeps
Homebrew as an alternative, and uses the PowerShell installer for Windows
[@direction-a-deploy-transcript]. Include that browser-visible command ordering
in the post-deploy check so a visual cleanup does not accidentally make
Homebrew look like the primary cross-platform path again.

Rollback redeploys an existing saved Sites version. List versions, choose the
exact previous version ID, call `sites.deploy_site_version` for that ID, poll the
new deployment ID, and rerun the same domain and installer checks
[@deployment-findings]. A rollback of a saved version does not need a rebuild or
`save_site_version`, but the current live IDs and access mode still need to be
reopened immediately before doing it [@deployment-findings].
