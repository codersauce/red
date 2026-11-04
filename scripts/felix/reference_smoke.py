"""Exercise the first multi-selection trace against an official Helix release."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import tarfile
import tempfile
import time
import tomllib


VERSION = "25.07.1"
COMMIT = tomllib.loads(Path(__file__).with_name("coverage.toml").read_text())["reference"]
# SHA-256 digests published with the official release archives.
ARCHIVES = {
    "aarch64-macos": "00b1651b4fdbbe0a2ae981c8e76b858bd26a7c33f5b3583f3b6bb9137d54f1ff",
    "x86_64-macos": "84dc32d617d28d32f4aa21e3aafac47bd715d1154aeb977697d4d60b887b7103",
    "aarch64-linux": "ce23fa8d395e633e3e54c052012f11965d91d8d5c2bfa659685f50430b4f8175",
    "x86_64-linux": "3f08e63ecd388fff657ad39722f88bb03dcf326f1f2da2700d99e1dc40ab2e8b",
}


def run_trace(bundle, root, executable=None, trace=None, red=False, paging=False):
    root.mkdir()
    fixture = root / "example.txt"
    fixture.write_text("".join(f"ROW-{number:03}\n" for number in range(1, 41)) if paging else "foo foo\n")
    other = root / "other.txt"
    other.write_text("other buffer\n")
    env = {
        "PATH": os.environ["PATH"],
        "HOME": str(root),
        "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_DATA_HOME": str(root / "data"),
        "XDG_CACHE_HOME": str(root / "cache"),
        "XDG_STATE_HOME": str(root / "state"),
        "HELIX_RUNTIME": str(bundle / "runtime"),
        "LANG": "en_US.UTF-8",
        "SHELL": "/bin/sh",
    }

    if red:
        config = root / "config/red"
        config.mkdir(parents=True)
        # Compare equal document heights. Red's optional breadcrumb plugin
        # can reserve a screen row asynchronously after the document opens.
        window_config = 'disabled_plugins = ["barbecue"]\n' if paging else ""
        (config / "config.toml").write_text(
            window_config + 'editing_model = "helix"\ndisable_ai = true\nsplash = false\n'
            'show_whats_new = false\nfetch_release_notes = false\n[lsp]\nenabled = false\n'
        )

    if trace is not None:
        env["FELIX_REFERENCE_TRACE"] = str(trace)

    def tmux(*args, check=True):
        return subprocess.run(
            ["tmux", "-f", "/dev/null", "-S", str(root / "tmux.sock"), *args],
            cwd=root, env=env, capture_output=True, text=True, check=check,
        )

    checkpoints = []

    def observe(label, line, mode, sels="1/2 sels", name="example.txt"):
        # Reading the tmux screen sends no input to the editor.
        deadline = time.monotonic() + (30 if label == "open" else 5)
        expected_line = f"    1  {line}".rstrip()
        while True:
            frame = tmux("capture-pane", "-p", "-t", "oracle:0.0").stdout
            rows = frame.splitlines()
            status = rows[-2].split() if len(rows) >= 2 else []
            if red:
                count = "1/1" if sels == "1 sel" else sels.split()[0]
                want_mode = {"NOR": "NORMAL", "INS": "INSERT"}[mode]
                matches = (any(row.strip() == f"1 {line}".rstrip() for row in rows)
                           and status[:2] == [want_mode, count] and any(name in part for part in status))
            else:
                matches = (expected_line in rows and status[:2] == [mode, f"~/{name}"]
                           and status[-3:-1] == sels.split())
            if matches:
                checkpoints.append({"step": label, "frame": rows})
                return
            if time.monotonic() >= deadline:
                raise RuntimeError(f"{label}: expected {line!r} in {mode} mode\n{frame}")
            time.sleep(0.02)

    def observe_page(first):
        expected = list(range(first, min(41, first + 22)))
        deadline = time.monotonic() + 20
        while True:
            frame = tmux("capture-pane", "-p", "-t", "oracle:0.0").stdout
            visible = [int(match) for match in re.findall(r"ROW-(\d{3})", frame)]
            if visible == expected:
                checkpoints.append({"first": first, "visible": visible, "frame": frame.splitlines()})
                return
            if time.monotonic() >= deadline:
                raise RuntimeError(f"paging: expected rows {expected}, saw {visible}\n{frame}")
            time.sleep(0.02)

    try:
        tmux("new-session", "-d", "-x", "80", "-y", "24", "-s", "oracle",
             str(executable or bundle / "hx"), str(fixture), str(other))
        if paging:
            observe_page(1)
            for key, first in [("PageDown", 23), ("PageDown", 36), ("PageUp", 14), ("PageUp", 1)]:
                tmux("send-keys", key)
                observe_page(first)
            return checkpoints
        observe("open", "foo foo", "NOR", "1 sel")
        tmux("send-keys", "-l", "%sfoo")
        tmux("send-keys", "Enter")
        observe("select matches", "foo foo", "NOR")
        tmux("send-keys", "c")
        observe("change", "", "INS")
        for prefix in ("b", "ba", "bar"):
            tmux("send-keys", "-l", prefix[-1])
            observe(f"type {prefix[-1]}", f"{prefix} {prefix}", "INS")
        tmux("send-keys", "Escape")
        observe("escape", "bar bar", "NOR")
        tmux("send-keys", "g", "n")
        observe("next buffer", "other buffer", "NOR", "1 sel", "other.txt")
        tmux("send-keys", "g", "p")
        observe("return to edited buffer", "bar bar", "NOR")
        tmux("send-keys", "u")
        observe("undo", "foo foo", "NOR")
        tmux("send-keys", "U")
        observe("redo", "bar bar", "NOR")
        return checkpoints
    finally:
        tmux("kill-server", check=False)


def verify_records(records):
    """Assert facts that plain terminal frames alone cannot distinguish."""
    # % s f o o Enter c b a r Esc g n g p u U
    if len(records) != 17:
        raise ValueError(f"expected one snapshot per input, got {len(records)}")
    def expect(index, **fields):
        for key, expected in fields.items():
            actual = records[index][key]
            if actual != expected:
                raise ValueError(f"event {index + 1} {key}: expected {expected!r}, got {actual!r}")
    fixture = json.loads((Path(__file__).parent / "fixtures/change-two-matches.json").read_text())
    for index, expected in enumerate(fixture):
        expect(index, **{key: value for key, value in expected.items() if key != "key"})
    expect(5, ranges=[[0, 3], [4, 7]], primary=0, text="foo foo\n")
    for i, word in enumerate(["", "b", "ba", "bar"]):
        expect(6 + i, mode="Insert", text=f"{word} {word}\n", unnamed=["foo", "foo"])
    expect(11, pending=["g"])
    expect(12, ranges=[[0, 1]], text="other buffer\n")
    expect(14, ranges=records[10]["ranges"], primary=records[10]["primary"])
    expect(15, ranges=records[5]["ranges"], primary=0, text="foo foo\n",
           history_revision=records[5]["history_revision"])
    expect(16, text="bar bar\n", history_revision=records[10]["history_revision"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path, help="official archive for this machine")
    parser.add_argument("--observer", type=Path, help="instrumented binary built from the pinned source")
    parser.add_argument("--candidate", type=Path, help="feature-enabled Red binary")
    args = parser.parse_args()
    os_name = {"Darwin": "macos", "Linux": "linux"}.get(platform.system())
    arch = {"arm64": "aarch64"}.get(platform.machine(), platform.machine())
    target = f"{arch}-{os_name}"
    if target not in ARCHIVES:
        parser.error(f"unsupported platform {target}")
    with args.archive.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
        if digest != ARCHIVES[target]:
            parser.error(f"archive digest {digest} differs from pin for {target}")
        source.seek(0)
        with tempfile.TemporaryDirectory(prefix="felix-") as tmp:
            root = Path(tmp)
            with tarfile.open(fileobj=source, mode="r:xz") as archive:
                archive.extractall(root, filter="data")
            bundle = root / f"helix-{VERSION}-{target}"
            checkpoints = run_trace(bundle, root / "release")
            red_frames = run_trace(bundle, root / "red", args.candidate.resolve(strict=True), red=True) if args.candidate else None
            pages = run_trace(bundle, root / "release-pages", paging=True)
            red_pages = (run_trace(bundle, root / "red-pages", args.candidate.resolve(strict=True), red=True,
                                   paging=True) if args.candidate else None)
            records = None
            if args.observer:
                observer = args.observer.resolve(strict=True)
                trace = root / "snapshot.jsonl"
                for name, output in [("disabled", None), ("enabled", trace)]:
                    observed = run_trace(bundle, root / name, observer, output)
                    for actual, expected in zip(observed, checkpoints, strict=True):
                        # The transient message row can vary between renders.
                        if actual["frame"][:-1] != expected["frame"][:-1]:
                            raise ValueError(f"{name}: release frame mismatch at {expected['step']}")
                records = [json.loads(line) for line in trace.read_text().splitlines()]
                verify_records(records)
    print(json.dumps({"release": VERSION, "commit": COMMIT, "archive_sha256": digest,
                      "target": target, "checkpoints": checkpoints, "events": records, "red_checkpoints": red_frames,
                      "paging": pages, "red_paging": red_pages}, indent=2))


if __name__ == "__main__":
    main()
