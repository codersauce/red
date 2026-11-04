"""Alternate native Red builds on one machine and retain individual replay results."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess


def digest(path):
    with path.open("rb") as stream:
        checksum = hashlib.sha256()
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            checksum.update(chunk)
        return checksum.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for arg in ("driver", "base", "candidate", "file", "output"):
        parser.add_argument("--" + arg, type=Path, required=True)
    parser.add_argument("--candidate-runtime", type=Path, help="Use this Red runtime overlay for the candidate")
    parser.add_argument("--scenarios", nargs="+", choices=("inside", "scroll", "reverse", "window", "buffer"),
                        default=["inside", "scroll", "reverse", "window", "buffer"])
    parser.add_argument("--config-override", action="append", default=[])
    parser.add_argument("--pairs", type=int, default=7)
    parser.add_argument("--presses", type=int, default=200)
    parser.add_argument("--interval-ms", type=int, default=25)
    args = parser.parse_args()
    if min(args.pairs, args.presses) < 1 or args.interval_ms < 0:
        parser.error("pairs and presses must be positive and interval must be nonnegative")
    args.output.mkdir(parents=True, exist_ok=False)
    manifest = {"os": platform.platform(), "machine": platform.machine(), "args": vars(args),
                "sha256": {k: digest(getattr(args, k)) for k in ("driver", "base", "candidate", "file")}}
    if args.candidate_runtime:
        overlay = args.candidate_runtime.resolve()
        manifest["runtime_sha256"] = {str(p.relative_to(overlay)): digest(p)
                                      for p in sorted(overlay.rglob("*")) if p.is_file()}
    (args.output / "manifest.json").write_text(json.dumps(manifest, indent=2, default=str))
    env = os.environ.copy()
    env.pop("RED_RUNTIME", None)
    summary = {}
    for scenario in args.scenarios:
        samples = {"base": [], "candidate": []}
        for pair in range(args.pairs + 1):  # pair zero warms both executables
            order = ("base", "candidate") if pair % 2 else ("candidate", "base")
            for variant in order:
                command = [args.driver.resolve(), "--binary", getattr(args, variant).resolve(),
                           "--file", args.file.resolve(), "--scenario", scenario, "--presses",
                           str(args.presses), "--interval-ms", str(args.interval_ms)]
                for setting in args.config_override:
                    command.extend(["--config-override", setting])
                run_env = env.copy()
                if variant == "candidate" and args.candidate_runtime:
                    run_env["RED_RUNTIME"] = str(args.candidate_runtime.resolve())
                run = subprocess.run(command, text=True, capture_output=True, env=run_env, timeout=180 + args.presses * args.interval_ms / 1000)
                prefix = args.output / f"{scenario}-{pair}-{variant}"
                prefix.with_suffix(".stderr").write_text(run.stderr)
                run.check_returncode()
                result = json.loads(run.stdout)
                prefix.with_suffix(".json").write_text(json.dumps(result, indent=2))
                if not result["document_verified"] or result["input_count"] != args.presses:
                    raise RuntimeError(f"failed replay verification: {prefix}")
                if pair:
                    samples[variant].append(result["send_to_statusline_us"]["p95"])
        summary[scenario] = {variant: statistics.median(values) for variant, values in samples.items()}
        print(scenario, summary[scenario], flush=True)
        (args.output / "median-p95-us.json").write_text(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
