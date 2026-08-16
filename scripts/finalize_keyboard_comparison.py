#!/usr/bin/env python3
"""Summarize, freeze, and verify a paired keyboard-scroll evidence bundle."""

import argparse
import hashlib
import json
from pathlib import Path
import platform
import re
import shutil
import statistics
import subprocess

ROOT = Path(__file__).resolve().parent.parent
GROUPS = (("release", "main"), ("release", "controls"), ("debug", "debug"))
ESCAPE = re.compile(rb"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b\[[0-?]*[ -/]*[@-~]")
SYNC = re.compile(rb"\x1b\[\?2026([hl])")


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def command(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def read_runs(output):
    return [(profile, group, run)
            for profile, group in GROUPS
            for run in json.loads((output / profile / (group + "-summary.json")).read_text())]


def summarize(output):
    runs = read_runs(output)
    medians = {}
    controls = {}
    for profile, group, run in runs:
        if group == "controls":
            controls.setdefault(run["name"], {})[run["variant"]] = run["results"]
            continue
        key = "/".join((profile, run["variant"], run["layout"], run["perf"]))
        for phase, value in run["results"].items():
            bucket = medians.setdefault(key, {}).setdefault(phase, {})
            for field in ("process_cpu_seconds", "output_bytes", "nonempty_terminal_frames"):
                bucket.setdefault(field, []).append(value[field])
            for span in ("event key", "navigation:publish", "render:motion_frame", "render:motion_delta", "notify cursor:moved"):
                if value["spans"][span]:
                    bucket.setdefault(span + " p95_us", []).append(value["spans"][span]["p95_us"])
    medians = {key: {phase: {field: statistics.median(values)
                            for field, values in fields.items()}
                     for phase, fields in phases.items()}
               for key, phases in medians.items()}
    escapes = {}
    for variant in ("before", "after"):
        names = sorted(run["name"] for profile, group, run in runs
                       if profile == "release" and group == "main"
                       and run["variant"] == variant and run["layout"] == "workspace"
                       and run["perf"] == "off")
        representative = names[min(1, len(names) - 1)]
        escapes[variant] = {}
        for phase in ("inside", "down", "up"):
            raw = (output / "release" / variant / representative / (phase + ".ansi")).read_bytes()
            sequences = ESCAPE.findall(raw)
            escapes[variant][phase] = {
                "bytes": len(raw), "escape_bytes": sum(map(len, sequences)),
                "sgr_sequences": sum(seq.startswith(b"\x1b[") and seq.endswith(b"m") for seq in sequences),
                "cursor_moves": sum(seq.startswith(b"\x1b[") and seq.endswith(b"H") for seq in sequences),
            }
    return {"medians": medians, "controls": controls, "escape_analysis": escapes}


def validate_runs(output):
    after_keys = 0
    pairs = {}
    runs = read_runs(output)
    for profile, group, run in runs:
        directory = output / profile / run["variant"] / run["name"]
        metadata = json.loads((directory / "metadata.json").read_text())
        assert metadata["binary_sha256"] == digest(output / (run["variant"] + "-" + profile))
        pairs.setdefault((profile, group, run["name"]), set()).add(
            (metadata["file_sha256"], metadata["split_file_sha256"]))
        for phase, value in run["results"].items():
            if run["variant"] != "after":
                continue
            if run["perf"] == "trace":
                assert value["handled_key_events"] == value["input_events"], (profile, run["name"], phase)
                after_keys += value["handled_key_events"]
            assert value["synchronized_output_begin"] == value["nonempty_terminal_frames"]
            assert value["synchronized_output_end"] == value["synchronized_output_begin"]
            active = False
            for marker in SYNC.finditer((directory / (phase + ".ansi")).read_bytes()):
                begins = marker.group(1) == b"h"
                assert begins != active, (profile, run["name"], phase, "unbalanced frame")
                active = begins
            assert not active
    assert all(len(hashes) == 1 for hashes in pairs.values()), "paired fixture mismatch"
    assert len(runs) == len(pairs) * 2, "missing paired run"
    return {"runs": len(runs), "after_traced_keys": after_keys, "paired_fixtures": len(pairs)}


def verify(output):
    count = 0
    for line in (output / "SHA256SUMS").read_text().splitlines():
        expected, name = line.split("  ", 1)
        assert digest(output / name) == expected, name
        count += 1
    result = validate_runs(output)
    metadata = json.loads((output / "metadata.json").read_text())
    command("tmux", "has-session", "-t", metadata["tmux"])
    print(json.dumps({"checksums": count, **result, "tmux": metadata["tmux"]}, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True)
    parser.add_argument("--base")
    parser.add_argument("--session")
    parser.add_argument("--summarize", action="store_true")
    parser.add_argument("--verify", action="store_true")
    args = parser.parse_args()
    output = Path(args.output).resolve()
    if args.verify:
        verify(output)
        return
    comparison = summarize(output)
    (output / "comparison.json").write_text(json.dumps(comparison, indent=2) + "\n")
    validation = validate_runs(output)
    if args.summarize:
        print(json.dumps({"validation": validation, "medians": comparison["medians"]}, indent=2))
        return
    if not args.base or not args.session:
        parser.error("--base and --session are required to freeze evidence")
    assert not command("git", "diff", "HEAD", "--", "src"), "commit the tested source first"
    metadata = {
        "head": command("git", "rev-parse", "HEAD"),
        "tree": command("git", "rev-parse", "HEAD^{tree}"),
        "base": command("git", "rev-parse", args.base),
        "branch": command("git", "branch", "--show-current"),
        "worktree": str(ROOT), "os": platform.platform(),
        "rustc": command("rustc", "--version"),
        "tmux": args.session, "validation": validation,
        "binaries": {name: digest(output / name) for name in
                     ("before-release", "after-release", "before-debug", "after-debug")},
    }
    (output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    shutil.copy2(ROOT / "docs/performance-keyboard-scroll-2026-08-15.md", output / "report.md")
    harness = output / "harness"
    harness.mkdir(exist_ok=True)
    for name in ("workspace_scroll_bench.py", "keyboard_scroll_bench.py", "compare_keyboard_scroll.py",
                 "keyboard_scroll_tmux.py", "finalize_keyboard_comparison.py"):
        shutil.copy2(ROOT / "scripts" / name, harness / name)
    (output / "implementation.patch").write_text(command("git", "diff", "--binary", args.base, "HEAD", "--", "src") + "\n")
    paths = sorted(path for path in output.rglob("*") if path.is_file()
                   and not {"live", "fixture"}.intersection(path.relative_to(output).parts)
                   and path.name != "SHA256SUMS")
    (output / "SHA256SUMS").write_text("".join(f"{digest(path)}  {path.relative_to(output)}\n" for path in paths))
    verify(output)


if __name__ == "__main__":
    main()
