# Helix differential harness

Compares Red's `editing_model = "helix"` with the real `hx` binary. The
expected values in `tests/helix.rs` were captured from Helix 25.07.1
(`a05c151b`) and then encoded by hand in that file. Re-run this harness when
Helix or the keymap changes.

## Requirements

- `hx` on `PATH`. This repo was checked against `helix 25.07.1 (a05c151b)`.
- `tmux`
- Python 3

Install the latest Helix release if `hx` is missing, and record `hx --version`
in `docs/HELIX_PARITY.md` when it changes.

## What it compares

For every case in `cases.py` the harness records:

- buffer text
- each selection as an inclusive `(anchor, head)` pair, in scalar indexes
- the primary index
- the mode (`normal`, `select`, `insert`, `search`, or `command`)

Red is driven in-process through `cargo test --test helix
dump_helix_state_for_the_differential_harness`. Helix is driven in a 100 by 24
truecolor tmux pane (`TERM=tmux-256color`, `COLORTERM=truecolor`). The probe
sets Helix `scrolloff` to 3 so it matches Red's default. The text area is 22
rows.

Helix's status column is a character column, with tabs expanded to the tab
stop. The other end of a selection is read by flipping it (`F1` in the probe
config, bound to `flip_selections`).

## Run

From the repository root:

```bash
python3 scripts/helix-diff/run.py
```

That probes `hx`, dumps Red, and exits non-zero when a case differs. To probe
only Helix:

```bash
python3 scripts/helix-diff/probe_hx.py --out /tmp/helix-diff/hx.json
```

`--ids id1,id2` and `--limit N` narrow the run. A full probe of the 300 cases
takes a few minutes because each case opens `hx`.

Key tokens in `cases.py` are literal characters plus `{enter}`, `{esc}`,
`{alt-x}`, `{ctrl-f}`, `{home}`, and the other names listed in `probe_hx.py`.
