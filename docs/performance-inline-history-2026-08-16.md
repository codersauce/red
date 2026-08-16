# Inline History freeze investigation

The multi-file Agent outcome checkpoint exposed synchronous work on the UI
thread: switching to Changes highlighted both complete versions of every file,
and each navigation or scroll action rebuilt the detail. The highlight-to-line
projection scanned all captures once per source line and again per output span.
Repeated keys therefore queued several seconds of work apiece.

The fix in `fdc528a` uses a line-local capture sweep, projects only lines present
in diff hunks, caches the latest exact detail by content/width/theme/language
registry, updates scrolling directly, and skips unchanged navigation. Capture
precedence remains most-specific first, with later captures winning ties.

## Measured production action path

Debug builds on the same machine, with the `red.json` theme and the retained
six-file `TextPanelFileLocation` rename. The fixture contained 16 changed
locations; the largest source image was about 70 KB. A standalone harness loaded
only that receipt into an in-memory editor and called the production History
dispatcher. It attached no session store, started no Agent or terminal editor,
and verified that every source file remained unchanged.

| Action | Before (`7e250c1`) | After (`fdc528a`) |
| --- | ---: | ---: |
| Open Changes | 6.324 s | 0.285 s |
| Expand | 6.331 s | 0.006 s |
| Collapse | 6.282 s | 0.005 s |
| Next | 6.331 s | 0.003 s |
| Previous | 6.336 s | 0.003 s |
| Scroll down | 6.311 s | 0.003 s |
| Scroll up | 6.351 s | 0.004 s |

These timings establish the blocking mechanism; they are not CI thresholds.
The original live-process sample was taken after recovery, so it cannot prove
that rendering accounted for every moment of the reported pause.

## Repeatable regression

The permanent test embeds the same six Red source files, creates independent
temporary postimages, and opens a real multi-file History receipt. It checks
that twelve expand/collapse/navigation/scroll actions retain the exact rendered
allocation and leave source files unchanged. It also prints cold and warm
timings without imposing machine-dependent limits.

```sh
cargo test --lib inline_history_multifile_navigation_reuses_rendered_receipt -- --test-threads=1 --nocapture
```

The first isolated run measured 300 ms for the cold Changes view and 48 ms for
all twelve warm actions. Existing capture-parity, sparse-line, and cache-key
tests cover syntax correctness and invalidation. Automated end-to-end execution
remains deferred to the final inline-assist phase.
