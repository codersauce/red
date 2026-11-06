"""Measure wall time to completed results for repeated, uncapped queries on a checkout."""
import argparse
import json
import statistics
import shutil
import subprocess
import time
from pathlib import Path


def stats(samples):
    ordered = sorted(samples)
    return {"p50_ms": round(statistics.median(ordered), 2),
            "p95_ms": round(ordered[(95 * len(ordered) + 99) // 100 - 1], 2)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("driver", type=Path)
    parser.add_argument("root", type=Path)
    parser.add_argument("patterns", nargs="+")
    parser.add_argument("--runs", type=int, default=12)
    args = parser.parse_args()
    root = args.root.resolve()
    start = time.perf_counter()
    with subprocess.Popen([args.driver.resolve(), root, "--red"], text=True,
                          stdin=subprocess.PIPE, stdout=subprocess.PIPE) as native:
        json.loads(native.stdout.readline())
        for pattern in args.patterns:
            samples = {"fff": [], "rg": [], "rg_first_match": [], "fff_first_match_in_worker": []}
            for index in range(args.runs + 1):
                t = time.perf_counter()
                print(json.dumps({"pattern": pattern, "regex": True}), file=native.stdin, flush=True)
                result = json.loads(native.stdout.readline())
                native_ms = (time.perf_counter() - t) * 1000
                if index == 0 and pattern == args.patterns[0]:
                    print(json.dumps({"native_first_query_including_launch_ms": round((time.perf_counter()-start)*1000, 2)}))
                if index == 0:
                    deadline = time.monotonic() + 120
                    while not result["indexed"]:
                        if time.monotonic() >= deadline:
                            raise RuntimeError("index warmup timed out")
                        time.sleep(0.02)
                        print(json.dumps({"pattern": pattern, "regex": True}), file=native.stdin, flush=True)
                        result = json.loads(native.stdout.readline())
                t = time.perf_counter()
                with subprocess.Popen(["rg", "--json", "--smart-case", "--no-hidden", "--glob=!.bare",
                                       "--glob=!.git", "--max-columns=500", "--max-columns-preview",
                                       "--", pattern, "."], cwd=root, stdout=subprocess.PIPE) as rg:
                    count = 0
                    first_ms = None
                    for line in rg.stdout:
                        if json.loads(line)["type"] == "match":
                            count += 1
                            if first_ms is None:
                                first_ms = (time.perf_counter() - t) * 1000
                    if rg.wait() not in (0, 1):
                        raise RuntimeError("rg search failed")
                rg_ms = (time.perf_counter() - t) * 1000
                if result["regex_error"] or result["truncated"] or len(result["matches"]) != count:
                    raise ValueError(f"incomparable result set for {pattern!r}, rg returned {count} lines")
                if index:
                    samples["fff"].append(native_ms)
                    samples["rg"].append(rg_ms)
                    if result.get("first_us") is not None:
                        samples["fff_first_match_in_worker"].append(result["first_us"] / 1000)
                    if first_ms is not None:
                        samples["rg_first_match"].append(first_ms)
            print(json.dumps({"pattern":pattern, "lines":count, **{k:stats(v) for k,v in samples.items() if v}}))
        if shutil.which("ps"):
            try:
                rss = subprocess.run(["ps", "-o", "rss=", "-p", str(native.pid)],
                                     capture_output=True, text=True, check=True)
                print(json.dumps({"resident_after_warm_queries_kib": int(rss.stdout.strip())}))
            except (OSError, subprocess.CalledProcessError, ValueError):
                pass
        native.stdin.close()
        if native.wait():
            raise RuntimeError("search process failed")


if __name__ == "__main__":
    main()
