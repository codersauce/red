#!/usr/bin/env python3
"""Compare Red's Helix mode with Helix 25.07.1 on helix-cm6's generated cases.

The cases come from a pinned helix-cm6 checkout at run time; nothing from it is
committed to Red. Helix's results are cached per Helix commit and case set.

    python3 scripts/felix/cm6/run.py hand fuzz:1
    python3 scripts/felix/cm6/run.py --update-baseline hand
"""
import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

HELIX_PIN = "a05c151bb6e8e9c65ec390b0ae2afe7a5efd619b"  # 25.07.1
CM6_PIN = "21dd29d9d9b57c9b728ff28df7f7d009534fdfcb"
HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]


def sh(*args, **kw):
    return subprocess.run(args, check=True, **kw)


def checkout(url, path, rev):
    if not (path / ".git").exists():
        sh("git", "clone", "-q", "--filter=blob:none", url, str(path))
    if subprocess.run(["git", "-C", str(path), "cat-file", "-e", f"{rev}^{{commit}}"]).returncode:
        sh("git", "-C", str(path), "fetch", "-q", url, rev)
    sh("git", "-C", str(path), "checkout", "-q", "--detach", rev)


def oracle_binary(helix):
    shutil.copy(HERE / "oracle.rs", helix / "helix-term/tests/felix_oracle.rs")
    env = {**os.environ, "HELIX_DISABLE_AUTO_GRAMMAR_BUILD": "1", "RUSTUP_TOOLCHAIN": "stable"}
    out = subprocess.run(
        ["cargo", "test", "--locked", "--release", "-p", "helix-term", "--features", "integration",
         "--test", "felix_oracle", "--no-run", "--message-format=json"],
        cwd=helix, env=env, stdout=subprocess.PIPE, check=True, text=True,
    ).stdout
    exes = re.findall(r'"executable":"([^"]*felix_oracle-[^"]*)"', out)
    if not exes:
        sys.exit("cannot build the Helix oracle")
    return exes[-1]


def run_oracle(helix, cases, jobs):
    exe = oracle_binary(helix)
    shards = [cases[i::jobs] for i in range(jobs)]
    tmp = Path(tempfile.mkdtemp(prefix="felix-oracle-"))

    def shard(n):
        src, dst = tmp / f"in{n}.json", tmp / f"out{n}.json"
        src.write_text(json.dumps(shards[n]))
        env = {**os.environ, "CARGO_MANIFEST_DIR": str(helix / "helix-term"),
               "FELIX_ORACLE_IN": str(src), "FELIX_ORACLE_OUT": str(dst)}
        subprocess.run([exe, "--test-threads=1", "-q"], cwd=helix / "helix-term", env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if not dst.exists():
            sys.exit(f"oracle shard {n} failed")
        return json.loads(dst.read_text())

    with ThreadPoolExecutor(jobs) as pool:
        results = [r for part in pool.map(shard, range(jobs)) for r in part]
    shutil.rmtree(tmp)
    return {r.pop("id"): r for r in results}


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("suites", nargs="+", help="hand, fuzz:<seed>[:<count>], md or code")
    parser.add_argument("--cache", type=Path, default=Path.home() / ".cache/red-felix/cm6")
    parser.add_argument("--jobs", type=int, default=os.cpu_count() or 4)
    parser.add_argument("--report", type=Path, help="write a JSON failure report")
    parser.add_argument("--update-baseline", action="store_true")
    parser.add_argument("--no-baseline", action="store_true", help="report only; never fail")
    args = parser.parse_args()

    cache = args.cache
    cache.mkdir(parents=True, exist_ok=True)
    helix, cm6 = cache / "helix", cache / "cm6"
    checkout("https://github.com/helix-editor/helix", helix, HELIX_PIN)
    checkout("https://github.com/Kosyakssss/helix-cm6", cm6, CM6_PIN)

    cases = []
    for suite in args.suites:
        out = subprocess.run(["bun", str(HERE / "export.ts"), str(cm6), suite],
                             stdout=subprocess.PIPE, check=True, text=True).stdout
        cases += json.loads(out)

    digest = hashlib.sha256(
        json.dumps(cases, sort_keys=True).encode() + (HERE / "oracle.rs").read_bytes()
    ).hexdigest()[:16]
    expected_path = cache / f"expected-{HELIX_PIN[:12]}-{digest}.json"
    if expected_path.exists():
        expected = json.loads(expected_path.read_text())
    else:
        print(f"running Helix {HELIX_PIN[:12]} on {len(cases)} cases", file=sys.stderr)
        unique = {(c["in"], c["keys"], c.get("lang")): c for c in cases}
        keyed = [{**c, "id": str(i)} for i, c in enumerate(unique.values())]
        results = run_oracle(helix, keyed, args.jobs)
        by_input = {(c["in"], c["keys"], c.get("lang")): results[c["id"]] for c in keyed}
        expected = {c["id"]: by_input[(c["in"], c["keys"], c.get("lang"))] for c in cases}
        expected_path.write_text(json.dumps(expected))

    # Language cases need Red's per-language syntax setup; run them later.
    plain = [{**c, "expected": expected[c["id"]]} for c in cases if not c.get("lang")]
    work = Path(tempfile.mkdtemp(prefix="felix-cm6-"))
    cases_path, report_path = work / "cases.json", args.report or work / "report.json"
    cases_path.write_text(json.dumps(plain))
    baseline = HERE / "baseline.txt"
    env = {**os.environ, "FELIX_CM6_CASES": str(cases_path), "FELIX_CM6_REPORT": str(report_path)}
    if baseline.exists() and not (args.update_baseline or args.no_baseline):
        env["FELIX_CM6_BASELINE"] = str(baseline)
    test = subprocess.run(
        ["cargo", "test", "--locked", "--features", "helix", "--lib",
         "editor::felix::cm6::cm6_cases_match_helix", "--", "--nocapture"],
        cwd=ROOT, env=env,
    )
    report = json.loads(report_path.read_text())
    if args.update_baseline:
        update_baseline(baseline, report, {c["id"] for c in plain})
    return 0 if args.update_baseline or args.no_baseline else test.returncode


def update_baseline(path, report, ids):
    """Keep entries for suites not run now; replace those for the ones that ran."""
    kept = []
    if path.exists():
        kept = [line for line in path.read_text().splitlines()
                if line.startswith("#") or (line.split() or [""])[0] not in ids]
    failed = sorted(report["failures"], key=lambda f: f["id"])
    lines = [f"{f['id']}  # {','.join(f['fields'])} {json.dumps(f['keys'])}" for f in failed]
    if not kept:
        kept = ["# Cases known to differ from Helix 25.07.1: `<id>  # <fields> <keys>`.",
                "# Regenerate with: python3 scripts/felix/cm6/run.py --update-baseline <suites>"]
    path.write_text("\n".join(kept + lines) + "\n")
    print(f"baseline: {len(lines)} expected failures written to {path}", file=sys.stderr)


if __name__ == "__main__":
    sys.exit(main())
