#!/usr/bin/env python3
"""Compare Red's Helix mode with the installed Helix binary.

Writes a TSV of cases, probes ``hx`` in tmux, dumps Red through
``cargo test --test helix``, and prints a per-key report.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

from cases import all_cases
from probe_hx import run_cases

ROOT = Path(__file__).resolve().parents[2]


def write_tsv(path: Path, cases: list[dict]) -> None:
    lines = []
    for item in cases:
        text = base64.b64encode(item["text"].encode("utf-8")).decode("ascii")
        lines.append("\t".join([item["id"], text, item["keys"], item.get("path") or ""]))
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def dump_red(cases: list[dict], out: Path, tsv: Path) -> dict:
    write_tsv(tsv, cases)
    env = os.environ.copy()
    env["HELIX_DUMP_CASES"] = str(tsv)
    env["HELIX_DUMP_OUT"] = str(out)
    subprocess.run(
        [
            "cargo",
            "test",
            "--offline",
            "--test",
            "helix",
            "dump_helix_state_for_the_differential_harness",
            "--",
            "--exact",
        ],
        cwd=ROOT,
        env=env,
        check=True,
    )
    return json.loads(out.read_text(encoding="utf-8"))


def ranges_equal(left, right) -> bool:
    return left == right


def compare(hx: dict, red: dict) -> list[dict]:
    red_by_id = {item["id"]: item for item in red["results"]}
    rows = []
    for item in hx["results"]:
        other = red_by_id.get(item["id"])
        row = {"id": item["id"], "keys": item.get("keys", "")}
        if "error" in item:
            row["status"] = "helix-error"
            row["detail"] = item["error"].splitlines()[0]
        elif other is None:
            row["status"] = "missing-red"
        elif item.get("ranges") is None:
            same = item.get("mode") == other.get("mode") and item.get("text") == other.get("text")
            row["status"] = "match" if same else "differ"
            if not same:
                row["detail"] = f"mode {item.get('mode')} vs {other.get('mode')}"
        else:
            same = (
                item.get("mode") == other.get("mode")
                and item.get("text") == other.get("text")
                and item.get("primary") == other.get("primary")
                and ranges_equal(item.get("ranges"), other.get("ranges"))
            )
            row["status"] = "match" if same else "differ"
            if not same:
                row["detail"] = (
                    f"hx mode={item.get('mode')} primary={item.get('primary')} "
                    f"ranges={item.get('ranges')} text={item.get('text')!r}\n"
                    f"red mode={other.get('mode')} primary={other.get('primary')} "
                    f"ranges={other.get('ranges')} text={other.get('text')!r}"
                )
        rows.append(row)
    return rows


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--hx-json", type=Path)
    parser.add_argument("--limit", type=int, default=0)
    parser.add_argument("--ids", default="")
    parser.add_argument("--skip-red", action="store_true")
    args = parser.parse_args()
    cases = all_cases()
    if args.ids:
        wanted = set(args.ids.split(","))
        cases = [item for item in cases if item["id"] in wanted]
    if args.limit:
        cases = cases[: args.limit]
    out_dir = Path("/tmp/helix-diff")
    out_dir.mkdir(parents=True, exist_ok=True)
    hx_path = args.hx_json or (out_dir / "hx.json")
    if args.hx_json and hx_path.exists() and not args.ids and not args.limit:
        hx = json.loads(hx_path.read_text(encoding="utf-8"))
    else:
        hx = run_cases(cases)
        hx_path.write_text(json.dumps(hx, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        print(f"helix {hx['helix']} -> {hx_path}")
    if args.skip_red:
        return
    red = dump_red(cases, out_dir / "red.json", out_dir / "cases.tsv")
    rows = compare(hx, red)
    counts: dict[str, int] = defaultdict(int)
    for row in rows:
        counts[row["status"]] += 1
        if row["status"] != "match":
            print(f"\n{row['status']} {row['id']} keys={row['keys']!r}")
            if detail := row.get("detail"):
                print(detail)
    print("summary", dict(counts), "of", len(rows))
    if counts["differ"] or counts["helix-error"] or counts["missing-red"]:
        raise SystemExit(1)


if __name__ == "__main__":
    sys.exit(main())
