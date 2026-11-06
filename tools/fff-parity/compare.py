"""Check bundled search results on disposable workspaces, optionally comparing with rg."""
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


def rg(root, query, flags, env):
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
    proc = subprocess.run([*args, "--", query["pattern"], "."], cwd=root, env=env, capture_output=True)
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


# Each expected row is (relative path, zero-based line, UTF-8 byte ranges).
# None denotes an invalid regex. Dense results are compared as a bounded subset.
def text_cases():
    text = "src/utf8.rs"
    patterns = "src/patterns"
    unicode_words = [(text, 1, ((0, 6), (7, 13)))]
    return [
        ("needle", True, [(text, 0, ((5, 11), (12, 18)))]),
        ("Needle", True, [(text, 0, ((5, 11),))]),
        (r"\*", True, [(patterns, 0, ((0, 1), (33, 34)))]),
        ("[", True, None),
        (r"\b\w+cole", True, unicode_words),
        ("école", True, unicode_words),
        ("TARGET", True, [("src/long", 0, ((621, 627),))]),
        ("large-tail", True, [("large", 800_000, ((0, 10),))]),
        ("dense match", True, [("dense", i, ((0, 11),)) for i in range(502)]),
        ("absent", True, []),
        ("invalid-bytes", True, [("invalid", 0, ((1, 14),))]),
        ("binary-needle", True, []),
        (r"\n", False, [(patterns, 0, ((30, 32),))]),
        ("*.rs", False, [(patterns, 0, ((0, 4),))]),
        ("git:modified", False, [(patterns, 0, ((5, 17),))]),
        ("src/main.rs", False, [(patterns, 0, ((18, 29),))]),
        ("abc  abc", False, [(text, 2, ((0, 8),))]),
    ]


def visible_rows(kind, flags, has_link):
    paths = []
    if flags["hidden"]:
        paths.append(".hidden")
    if kind == "non-Git" or flags["ignored"]:
        paths.append("ignored")
    if flags["ignored"]:
        paths.extend(["nested/vendor", "rgignored"])
    if has_link and flags["follow"]:
        paths.append("link")
    return [(path, 0, ((0, 10),)) for path in sorted(paths)]


def compare(binary, kind, raw, against_rg):
    failures = []
    total = 0
    with tempfile.TemporaryDirectory() as temp:
        # Global Git excludes and rg options belong to the caller, not these fixtures.
        fixture_home = Path(temp, "home")
        fixture_home.mkdir()
        env = dict(os.environ, HOME=str(fixture_home), USERPROFILE=str(fixture_home),
                   XDG_CONFIG_HOME=str(fixture_home / ".config"), GIT_CONFIG_GLOBAL=os.devnull,
                   GIT_CONFIG_NOSYSTEM="1")
        env.pop("RIPGREP_CONFIG_PATH", None)
        root = Path(temp, "repo")
        if kind == "linked":
            base = Path(temp, "base")
            subprocess.run(["git", "init", "-q", base], env=env, check=True)
            subprocess.run(["git", "-C", base, "-c", "user.name=FFF fixture", "-c",
                            "user.email=fixture@example.invalid", "-c", "core.hooksPath="+temp,
                            "commit", "--allow-empty", "--no-gpg-sign", "-qm", "fixture"], env=env, check=True)
            subprocess.run(["git", "-C", base, "-c", "core.hooksPath="+temp, "worktree",
                            "add", "-q", "--detach", root], env=env, check=True)
        else:
            root.mkdir()
        if kind == "Git":
            subprocess.run(["git", "init", "-q", root], env=env, check=True)
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
            cases = [("visibility", False, visible_rows(kind, flags, (root / "link").is_symlink()))]
            if not any(flags.values()):
                cases += text_cases()
            inputs = [dict(pattern=s, regex=r, **flags) for s, r, _ in cases]
            args = [binary, str(root)] + (["--follow"] if follow else []) + ([] if raw else ["--red", "--wait-for-index"])
            proc = subprocess.run(args, input="".join(json.dumps(q) + "\n" for q in inputs),
                                  text=True, env=env, capture_output=True, timeout=150)
            if proc.returncode:
                raise RuntimeError(f"FFF driver exited {proc.returncode}: {proc.stderr}")
            _, *results = map(json.loads, proc.stdout.splitlines())
            for query, result, (_, _, expected) in zip(inputs, results, cases, strict=True):
                total += 1
                if against_rg and rg(root, query, flags, env) != expected:
                    raise ValueError(f"rg disagrees with fixture for {kind} root: {query}")
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
    parser.add_argument("--against-rg", action="store_true", help="also verify expected rows with installed rg")
    args = parser.parse_args()
    if os.name == "nt" and not args.driver.suffix:
        args.driver = args.driver.with_suffix(".exe")
    failed = 0
    for kind in ["non-Git", "Git", "linked"]:
        total, failures = compare(args.driver.resolve(), kind, args.raw, args.against_rg)
        print(f"{kind} root: {total-len(failures)}/{total} cases passed")
        for query, actual, expected in failures:
            print(f"  {query}: got {actual}, expected {expected}")
        failed += len(failures)
    return int(bool(failed))


if __name__ == "__main__":
    raise SystemExit(main())
