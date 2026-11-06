"""Compare persistent FFF results with Red's rg invocation on disposable files."""
import argparse
import itertools
import json
import os
from pathlib import Path
import subprocess
import tempfile


def corpus(root):
    for path, content in {
        "src/utf8.rs": "😀 Needle needle\r\nÉcole école\r\nabc  abc\n",
        "src/patterns": r"*.rs git:modified src/main.rs \n * [" + "\n",
        "src/long": "a" * 620 + " TARGET\n",
        ".hidden": "visibility\n",
        ".bare/config": "visibility\n",
        "ignored": "visibility\n",
        "rgignored": "visibility\n",
        "nested/.ignore": "vendor\n",
        "nested/vendor": "visibility\n",
        ".gitignore": "/ignored\n",
        ".rgignore": "/rgignored\n",
        "dense": "dense match\n" * 502,
        "large": "ordinary text\n" * 800_000 + "large-tail\n",
    }.items():
        dest = root / path
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_bytes(content.encode())
    (root / "binary").write_bytes(b"\0binary-needle\n")
    (root / "invalid").write_bytes(b"\xffinvalid-bytes\n")


def rg(root, query, flags):
    args = ["rg", "--json", "--color=never", "--no-heading", "--with-filename",
            "--line-number", "--column", "--smart-case", "--max-columns=500",
            "--max-columns-preview", "--glob=!.bare", "--glob=!.git",
            "--hidden" if flags["hidden"] else "--no-hidden"]
    if flags["ignored"]:
        args.append("--no-ignore")
    if flags["follow"]:
        args.append("--follow")
    if not query["regex"]:
        args.append("--fixed-strings")
    proc = subprocess.run([*args, "--", query["pattern"], "."], cwd=root, capture_output=True)
    if proc.returncode not in (0, 1):
        return None
    rows = []
    for row in map(json.loads, proc.stdout.splitlines()):
        if row["type"] == "match":
            data = row["data"]
            path = Path(data["path"]["text"]).as_posix()
            spans = [(s["start"], s["end"]) for s in data["submatches"]]
            rows.append((path, data["line_number"] - 1, tuple(spans)))
    return sorted(rows)


def compare(binary, kind, raw):
    failures = []
    total = 0
    queries = [(s, True) for s in ["needle", "Needle", r"\*", "[", r"\b\w+cole",
                                    "école", "TARGET", "large-tail", "dense match",
                                    "absent", "invalid-bytes", "binary-needle"]]
    queries += [(s, False) for s in [r"\n", "*.rs", "git:modified", "src/main.rs", "abc  abc"]]
    with tempfile.TemporaryDirectory() as temp:
        root = Path(temp, "repo")
        if kind == "linked":
            base = Path(temp, "base")
            subprocess.run(["git", "init", "-q", base], check=True)
            subprocess.run(["git", "-C", base, "-c", "user.name=FFF fixture", "-c",
                            "user.email=fixture@example.invalid", "-c", "core.hooksPath="+temp,
                            "commit", "--allow-empty", "--no-gpg-sign", "-qm", "fixture"], check=True)
            subprocess.run(["git", "-C", base, "-c", "core.hooksPath="+temp, "worktree",
                            "add", "-q", "--detach", root], check=True)
        else:
            root.mkdir()
        if kind == "Git":
            subprocess.run(["git", "init", "-q", root], check=True)
        corpus(root)
        try:
            outside = Path(temp, "outside")
            outside.write_text("visibility\n")
            (root / "link").symlink_to(outside)
        except OSError:
            if os.name != "nt":
                raise
        for hidden, ignored, follow in itertools.product((False, True), repeat=3):
            flags = dict(hidden=hidden, ignored=ignored, follow=follow)
            cases = [("visibility", False)]
            if not any(flags.values()):
                cases += queries
            inputs = [dict(pattern=s, regex=r, **flags) for s, r in cases]
            args = [binary, str(root)] + (["--follow"] if follow else []) + ([] if raw else ["--red", "--wait-for-index"])
            proc = subprocess.run(args, input="".join(json.dumps(q) + "\n" for q in inputs),
                                  text=True, capture_output=True, timeout=150)
            if proc.returncode:
                raise RuntimeError(f"FFF driver exited {proc.returncode}: {proc.stderr}")
            ready, *results = map(json.loads, proc.stdout.splitlines())
            for query, result in zip(inputs, results, strict=True):
                total += 1
                expected = rg(root, query, flags)
                actual = sorted((Path(m["path"]).as_posix(), m["line"], tuple(map(tuple, m["spans"])))
                                for m in result["matches"])
                if expected is None:
                    equal = bool(result["regex_error"]) and not actual
                elif len(expected) > 500:
                    equal = (not result["regex_error"] and result["truncated"]
                             and len(set(actual)) == len(actual) == 500 and set(actual) <= set(expected))
                else:
                    equal = not result["regex_error"] and not result["truncated"] and actual == expected
                if not equal:
                    failures.append((query, actual[:2], expected[:2] if expected else expected))
    return total, failures


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("driver", type=Path)
    parser.add_argument("--raw", action="store_true", help="test upstream FilePicker directly")
    args = parser.parse_args()
    if os.name == "nt" and not args.driver.suffix:
        args.driver = args.driver.with_suffix(".exe")
    failed = 0
    for kind in ["non-Git", "Git", "linked"]:
        total, failures = compare(args.driver.resolve(), kind, args.raw)
        print(f"{kind} root: {total-len(failures)}/{total} cases matched rg")
        for query, actual, expected in failures:
            print(f"  {query}: got {actual}, expected {expected}")
        failed += len(failures)
    return int(bool(failed))


if __name__ == "__main__":
    raise SystemExit(main())
