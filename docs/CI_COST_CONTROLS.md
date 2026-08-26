# CI Cost Controls

Red keeps cross-platform pull request coverage while avoiding a second paid
three-platform test matrix after every merge.

## Validation modes

- Pull requests run Ubuntu, macOS, and Windows tests. Documentation-only pull
  requests skip the paid test matrix.
- Pushes to `main` or `develop` run one Ubuntu smoke suite.
- Manual CI runs can select either the full matrix or the smoke suite.
- Clippy runs once on Ubuntu for code pull requests and manual runs. It is
  skipped for documentation-only pull requests and post-merge pushes.
- Newer runs cancel older work in the same workflow and branch or pull request.
- `CI Gate` is the stable terminal check for branch rules. It fails unless all
  jobs required by the selected mode reach their expected conclusions.

The normal test runners are `warp-ubuntu-latest-x64-8x` and
`warp-macos-latest-arm64-6x`. Windows remains on 32x until a paired benchmark
demonstrates that its smaller candidate reduces billed cost without hurting
reliability or memory headroom.

## Runner sizing benchmark

Run **Runner Sizing** manually and explicitly confirm the paid benchmark. It
launches three clean replicas on each runner in the selected pair:

- Ubuntu: 32x versus 8x
- macOS: 12x versus 6x
- Windows: 32x versus 16x

Compare billed cost, p90 duration, CPU, memory, and failures in WarpBuild
Reports. Keep the smaller runner only when its cost per successful run falls.
Windows must also show safe memory headroom; the initial 32x sample was already
near its reported memory ceiling.

The August 24, 2026 macOS benchmark completed all three replicas on both runner
sizes. The 12x replicas took 1:26-1:37 and cost $0.96 in total; the 6x replicas
took 1:45-3:04 and cost $0.64 in total. The slow 6x replica spent 78 seconds
restoring the cache rather than running tests, while the other two replicas
finished in 1:45 with roughly 53% memory use. Based on that result, normal
macOS validation uses 6x. Re-run the paired benchmark and revert to 12x if the
rolling p90 reaches four billed minutes, platform-only failures increase, or
cost per successful macOS job is no longer below the 12x baseline.

## Main branch ruleset

After a pull request containing `CI Gate` has run successfully, import
`.github/rulesets/main.json` at **Settings -> Rules -> Rulesets -> New ruleset
-> Import a ruleset**. Review it before creation. The ruleset:

- requires changes to reach `main` through a pull request;
- requires the `CI Gate` check;
- requires the checked commit to include the latest `main`;
- blocks deletion and force pushes.

Do not activate the ruleset before `CI Gate` exists on the default branch, or
future pull requests will wait for a check that GitHub has not seen yet.

## WarpBuild budget

In WarpBuild billing settings, set alerts at 50%, 75%, and 90% of the agreed
monthly budget. The initial target is $250 per month. Enable a $250 hard limit
only after confirming who can raise it and how WarpBuild handles jobs that are
already running when the limit is reached.

Review the first seven days after rollout using cost per merged pull request,
job count, billed minutes, p90 duration, cancellations, and platform-only
failures. Roll back an individual runner change when reliability falls or cost
per successful run rises.
