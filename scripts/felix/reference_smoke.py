"""Exercise the first multi-selection trace against an official Helix release."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
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


def run_trace(bundle, root, executable=None, trace=None, red=False, scenario="edit"):
    # Helix abbreviates paths inside its working directory before folding HOME.
    # Keep documents in HOME and launch from its child so snapshots always use ~/.
    root = root.resolve()
    root.mkdir()
    workdir = root / "work"
    workdir.mkdir()
    fixture = root / ("example.rs" if scenario == "completion" else "example.txt")
    split_search = scenario in ("split-search", "wrapped-split-search")
    wrapped = scenario == "wrapped-split-search"
    if wrapped:
        # Each tag starts one 36-cell display row on the same logical line.
        fixture.write_text("".join(f"ROW-{n:03}" + "x" * 29 for n in range(1, 101)) + "\n")
    elif split_search or scenario == "paging":
        row_count = 100 if split_search else 40
        fixture.write_text("".join(f"ROW-{n:03}\n" for n in range(1, row_count + 1)))
    else:
        fixture.write_text("foo foo\n")
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

    state = root / "lsp-state.json"
    if scenario == "completion":
        server = Path(__file__).resolve().parents[1] / "edit_replay_lsp.py"
        args = [str(server), "--mode", "incremental", "--state", str(state), "--completion"]
        command = f"command = {json.dumps(sys.executable)}\nargs = {json.dumps(args)}\n"
    if scenario == "completion" and not red:
        config = root / "config/helix"
        config.mkdir(parents=True)
        (config / "languages.toml").write_text(
            '[[language]]\nname = "rust"\nlanguage-servers = ["felix-test"]\n[language-server.felix-test]\n' + command
        )
        (config / "config.toml").write_text('[editor]\nauto-completion = false\n')
    if wrapped and not red:
        config = root / "config/helix"
        config.mkdir(parents=True)
        (config / "config.toml").write_text(
            '[editor]\ngutters = []\ntext-width = 36\n[editor.soft-wrap]\nenable = true\n'
            'wrap-at-text-width = true\nwrap-indicator = ""\nmax-wrap = 0\nmax-indent-retain = 0\n'
        )
    if red:
        config = root / "config/red"
        config.mkdir(parents=True)
        # Compare equal document heights. Red's optional breadcrumb plugin
        # can reserve a screen row asynchronously after the document opens.
        window_config = 'disabled_plugins = ["barbecue"]\n' if split_search or scenario in ("paging", "repeat") else ""
        if wrapped:
            window_config += "wrap = true\n"
        (config / "config.toml").write_text(
            window_config + 'editing_model = "helix"\ndisable_ai = true\nsplash = false\n'
            'show_whats_new = false\nfetch_release_notes = false\n[lsp]\n'
            + ('enabled = true\n[completion]\nauto_trigger = false\n[lsp.servers.rust]\n'
               'language_id = "rust"\nfile_extensions = ["rs"]\nroot_markers = []\n' + command
               if scenario == "completion" else 'enabled = false\n')
        )

    if trace is not None:
        env["FELIX_REFERENCE_TRACE"] = str(trace)

    def tmux(*args, check=True):
        return subprocess.run(
            ["tmux", "-f", "/dev/null", "-S", str(root / "tmux.sock"), *args],
            cwd=workdir, env=env, capture_output=True, text=True, check=check,
        )

    checkpoints = []

    def observe(label, line, mode, sels="1/2 sels", name=None, copies=1, menu=None, cursor=None):
        name = name or fixture.name
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
            if cursor is not None and not red:
                matches &= bool(status) and status[-1] == cursor and not rows[-1].startswith("select:")
            if copies > 1:
                matches &= sum(row.strip() == (f"1 {line}" if red else f"1  {line}") for row in rows) == copies
            if menu is not None:
                menu_visible = any(("FelixCandidate" in row or "SnippetCandidate" in row) and not re.match(r"^\s*1\s", row) for row in rows)
                matches &= menu_visible == menu
            if matches:
                checkpoints.append({"step": label, "frame": rows})
                return
            if time.monotonic() >= deadline:
                raise RuntimeError(f"{label}: expected {line!r} in {mode} mode\n{frame}")
            time.sleep(0.02)

    def observe_server(text):
        expected = hashlib.sha256(text.encode()).hexdigest()
        deadline = time.monotonic() + 15
        while True:
            data = json.loads(state.read_text()) if state.exists() else {}
            if data.get("sha256") == expected:
                return data
            if time.monotonic() >= deadline:
                raise RuntimeError(f"completion server expected {text!r}, last state {data}")
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
        tmux("new-session", "-d", "-x", "81" if wrapped else "80", "-y", "24", "-s", "oracle",
             str(executable or bundle / "hx"), str(fixture), str(other))
        if split_search:
            def check_views(label, left, right, prompt=""):
                deadline = time.monotonic() + 5
                expected = [list(range(top, min(top + 22, 101))) for top in (left, right)]
                while True:
                    rows = tmux("capture-pane", "-p", "-t", "oracle:0.0").stdout.splitlines()
                    panes = [[], []]
                    for row in rows[:-2]:
                        for match in re.finditer(r"ROW-(\d{3})", row):
                            side = int(match.start() >= 40)
                            panes[side].append(int(match[1]))
                    prompt_matches = rows and (rows[-1] == "search:" + prompt if prompt
                                               else not rows[-1].startswith("search:"))
                    if panes == expected and prompt_matches:
                        checkpoints.append({"step": label, "visible": panes, "prompt": prompt})
                        return
                    if time.monotonic() >= deadline:
                        raise RuntimeError(f"{label}: expected {expected} and prompt {prompt!r}, saw\n" + "\n".join(rows))
                    time.sleep(.02)
            if wrapped:
                # Helix wraps at 36 before splitting too. Red initially uses
                # the full terminal width, so both tags can share its first row.
                deadline = time.monotonic() + 30
                while "ROW-001" not in tmux("capture-pane", "-p").stdout:
                    if time.monotonic() >= deadline:
                        raise RuntimeError("wrapped document did not open")
                    time.sleep(0.02)
            else:
                observe("open", "ROW-001", "NOR", "1 sel")
            tmux("send-keys", "C-w", "v", "C-w", "h")
            check_views("open two views", 1, 1)
            tmux("send-keys", "-l", "/ROW-060")
            check_views("preview in left view", 50, 1, "ROW-060")
            tmux("send-keys", "Escape")
            check_views("cancel left preview", 1, 1)
            tmux("send-keys", "-l", "/ROW-060")
            tmux("send-keys", "Enter")
            check_views("accept left search", 50, 1)
            tmux("send-keys", "C-w", "l")
            tmux("send-keys", "-l", "/ROW-090")
            check_views("preview in right view", 50, 80, "ROW-090")
            tmux("send-keys", "Enter")
            check_views("accept right search", 50, 80)
            tmux("send-keys", "C-w", "h")
            tmux("send-keys", "-l", "?ROW-012")
            check_views("reverse search on return", 7, 80, "ROW-012")
            tmux("send-keys", "C-c")
            check_views("restore both viewports", 50, 80)
            return checkpoints
        if scenario == "prompt":
            observe("open", "foo foo", "NOR", "1 sel")
            def check_prompt(label, value, column, menu_rows=()):
                deadline = time.monotonic() + 5
                while True:
                    rows = tmux("capture-pane", "-p", "-t", "oracle:0.0").stdout.splitlines()
                    caret = tmux("display-message", "-p", "-t", "oracle:0.0", "#{cursor_flag} #{cursor_x} #{cursor_y}").stdout.strip()
                    menu = rows[-1-len(menu_rows):-1]
                    if rows[-1] == "search:" + value and caret == f"1 {column} 23" and menu == list(menu_rows):
                        checkpoints.append({"step": label, "prompt": rows[-1], "caret": caret, "menu": menu})
                        return
                    if time.monotonic() >= deadline:
                        raise RuntimeError(f"{label}: saw {rows[-1]!r}, caret {caret}, menu {menu!r}")
                    time.sleep(.02)
            tmux("send-keys", "-l", "/foo")
            check_prompt("type query", "foo", 10)
            tmux("send-keys", "Enter", "?")
            check_prompt("suggest previous query", "foo", 7)
            tmux("send-keys", "Up", "Left")
            check_prompt("move within recalled query", "foo", 9)
            tmux("send-keys", "-l", "界")
            check_prompt("insert wide character", "fo界o", 11)
            tmux("send-keys", "BSpace")
            check_prompt("delete before caret", "foo", 9)
            tmux("send-keys", "C-c")
            observe("cancel search", "foo foo", "NOR", "1 sel")
            tmux("send-keys", "/", "f", "o", "x", "Enter", "/", "f", "a", "r", "Enter", "/", "f")
            menu = ("far" + " " * 37 + "fox", "foo")
            check_prompt("filter search history", "f", 8, menu)
            tmux("send-keys", "BTab")
            check_prompt("backward completion wraps", "fox", 10, menu)
            tmux("send-keys", "Tab")
            check_prompt("forward completion wraps", "far", 10, menu)
            tmux("send-keys", "BSpace")
            check_prompt("editing refilters history", "fa", 9, ("far",))
            tmux("send-keys", "Escape")
            observe("dismiss history menu", "foo foo", "NOR", "1 sel")
            tmux("resize-window", "-x", "30", "-y", "24", "-t", "oracle:0")
            tmux("send-keys", "-l", "/abcdefghijklmnopqrstuv界xyz")
            check_prompt("clip long query", "abcdefghijklmnopqrstu", 29)
            tmux("send-keys", "Home")
            check_prompt("move to start of long query", "abcdefghijklmnopqrstu", 7)
            tmux("send-keys", "End", "C-u")
            tmux("send-keys", "-l", "12345 12345 12345 12345 12界xy")
            check_prompt("wrap long query at a word", "12345 12345 12345", 29)
            tmux("resize-window", "-x", "80", "-y", "24", "-t", "oracle:0")
            check_prompt("reveal query after resize", "12345 12345 12345 12345 12界xy", 37)
            return checkpoints
        if scenario == "completion":
            observe("open", "foo foo", "NOR", "1 sel")
            observe_server("foo foo\n")
            tmux("send-keys", "-l", "%sfoo")
            tmux("send-keys", "Enter", "c")
            observe("change two selections", "", "INS")
            tmux("send-keys", "C-x")
            observe("completion before Ctrl-C", "", "INS", menu=True)
            tmux("send-keys", "F")
            observe("filter before dismissal", "F F", "INS", menu=True)
            tmux("send-keys", "C-c")
            observe("dismiss with Ctrl-C and keep filter", "F F", "INS", menu=False)
            observe_server("F F\n")
            tmux("send-keys", "C-x")
            observe("unselected menu", "F F", "INS", menu=True)
            before_preview = observe_server("F F\n")["notifications"]
            tmux("send-keys", "Up")
            observe("previous starts at last candidate", "FelixOther FelixOther", "INS", menu=True)
            tmux("send-keys", "Tab")
            observe("Tab wraps to first candidate", "FelixCandidate FelixCandidate", "INS", menu=True)
            tmux("send-keys", "C-p")
            observe("previous wraps to last candidate", "FelixOther FelixOther", "INS", menu=True)
            observe_server("F F\n")
            tmux("send-keys", "C-c")
            observe("abort preview", "F F", "INS", menu=False)
            observe_server("F F\n")
            tmux("send-keys", "C-x")
            observe("completion before Escape", "F F", "INS", menu=True)
            # The next completion response is a barrier on the same LSP stream.
            # Transient edits followed by a restore would have changed this count.
            if observe_server("F F\n")["notifications"] != before_preview:
                raise RuntimeError("preview sent a document change to the language server")
            tmux("send-keys", "Escape")
            observe("escape from completion", "F F", "NOR", menu=False)
            tmux("send-keys", "u")
            observe("undo typing and change", "foo foo", "NOR")
            observe_server("foo foo\n")
            tmux("send-keys", "c", "F", "C-x")
            observe("request before implicit acceptance", "F F", "INS", menu=True)
            tmux("send-keys", "C-p", "!")
            observe("type after preview", "FelixOther! FelixOther!", "INS", menu=False)
            observe_server("FelixOther! FelixOther!\n")
            tmux("send-keys", "Escape")
            observe("finish implicit acceptance", "FelixOther! FelixOther!", "NOR")
            tmux("send-keys", "u")
            observe("undo implicit acceptance", "F F", "NOR")
            tmux("send-keys", "u")
            observe("undo prefix", "foo foo", "NOR")
            tmux("send-keys", "c", "F", "C-x")
            observe("request before preview Escape", "F F", "INS", menu=True)
            tmux("send-keys", "C-p", "Escape")
            observe("Escape accepts preview", "FelixOther FelixOther", "NOR", menu=False)
            tmux("send-keys", "u")
            observe("undo preview accepted by Escape", "F F", "NOR")
            tmux("send-keys", "u")
            observe("undo next prefix", "foo foo", "NOR")
            tmux("send-keys", "c", "F", "C-x")
            observe("request completion at two selections", "F F", "INS", menu=True)
            tmux("send-keys", "e", "l")
            observe("extend filter at both cursors", "Fel Fel", "INS", menu=True)
            tmux("send-keys", "BSpace")
            observe("filter at both cursors", "Fe Fe", "INS", menu=True)
            observe_server("Fe Fe\n")
            tmux("send-keys", "C-n")
            observe("preview after filter", "FelixCandidate FelixCandidate", "INS", menu=True)
            tmux("send-keys", "Enter")
            observe("accept at both selections", "FelixCandidate FelixCandidate", "INS", menu=False)
            observe_server("FelixCandidate FelixCandidate\n")
            tmux("send-keys", "!", "Escape")
            observe("type after accepting", "FelixCandidate! FelixCandidate!", "NOR", menu=False)
            tmux("send-keys", "u")
            observe("undo completion and typing", "F F", "NOR")
            tmux("send-keys", "u")
            observe("undo initial change", "foo foo", "NOR")
            requests = observe_server("foo foo\n")["completion_requests"]
            tmux("send-keys", ".")
            observe("repeat completion", "FelixCandidate! F!", "NOR", menu=False)
            if observe_server("FelixCandidate! F!\n")["completion_requests"] != requests:
                raise RuntimeError("dot requested a new completion")
            tmux("send-keys", "u")
            observe("undo completion repeat", "foo foo", "NOR")
            tmux("send-keys", "U")
            observe("redo completion repeat", "FelixCandidate! F!", "NOR")
            tmux("send-keys", "u")
            observe("restore before completion repeat", "foo foo", "NOR")
            tmux("send-keys", "c", "S", "C-x")
            observe("request snippet", "S S", "INS", menu=True)
            tmux("send-keys", "C-n", "Enter")
            observe("expand snippet twice", "one=two one=two", "INS", menu=False)
            tmux("send-keys", "X")
            observe("replace first defaults", "X=two X=two", "INS")
            tmux("send-keys", "Tab", "Y")
            observe("replace second defaults", "X=Y X=Y", "INS")
            observe_server("X=Y X=Y\n")
            # Helix 25.07.1 panics on another Tab here. Red covers the final
            # stop in snippet_final_stop_preserves_both_insert_cursors_and_undo.
            tmux("send-keys", "Escape")
            observe("leave snippet", "X=Y X=Y", "NOR")
            tmux("send-keys", "u")
            observe("undo snippet edits", "S S", "NOR")
            tmux("send-keys", "u")
            observe("undo snippet prefix", "foo foo", "NOR")
            requests = observe_server("foo foo\n")["completion_requests"]
            tmux("send-keys", ".")
            observe("repeat snippet", "one=twoX SXY", "NOR", menu=False)
            if observe_server("one=twoX SXY\nY")["completion_requests"] != requests:
                raise RuntimeError("dot requested a new completion")
            tmux("send-keys", "u")
            observe("undo snippet repeat", "foo foo", "NOR")
            tmux("send-keys", "U")
            observe("redo snippet repeat", "one=twoX SXY", "NOR")
            tmux("send-keys", "u")
            observe("restore before snippet repeat", "foo foo", "NOR")
            return checkpoints
        if scenario == "paging":
            observe_page(1)
            for key, first in [("PageDown", 23), ("PageDown", 36), ("PageUp", 14), ("PageUp", 1)]:
                tmux("send-keys", key)
                observe_page(first)
            return checkpoints
        if scenario == "repeat":
            observe("open", "foo foo", "NOR", "1 sel")
            tmux("send-keys", "i", "X", "Escape")
            observe("record insert", "Xfoo foo", "NOR", "1 sel")
            tmux("send-keys", "g", "n")
            observe("switch document", "other buffer", "NOR", "1 sel", "other.txt")
            tmux("send-keys", ".")
            observe("repeat in second document", "Xother buffer", "NOR", "1 sel", "other.txt")
            tmux("send-keys", "C-w", "s")
            observe("split document", "Xother buffer", "NOR", "1 sel", "other.txt", copies=2)
            tmux("send-keys", "2", ".")
            observe("repeat twice in split", "XXXother buffer", "NOR", "1 sel", "other.txt", copies=2)
            tmux("send-keys", "u")
            observe("undo counted repeat", "Xother buffer", "NOR", "1 sel", "other.txt", copies=2)
            tmux("send-keys", "U")
            observe("redo counted repeat", "XXXother buffer", "NOR", "1 sel", "other.txt", copies=2)
            return checkpoints
        observe("open", "foo foo", "NOR", "1 sel")
        tmux("send-keys", "-l", "%sfoo")
        tmux("send-keys", "Enter")
        observe("select matches", "foo foo", "NOR", cursor="1:3")
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
            pages = run_trace(bundle, root / "release-pages", scenario="paging")
            red_pages = (run_trace(bundle, root / "red-pages", args.candidate.resolve(strict=True), red=True,
                                   scenario="paging") if args.candidate else None)
            repeats = run_trace(bundle, root / "release-repeat", scenario="repeat")
            red_repeats = (run_trace(bundle, root / "red-repeat", args.candidate.resolve(strict=True),
                                     red=True, scenario="repeat") if args.candidate else None)
            completion = run_trace(bundle, root / "release-completion", scenario="completion")
            red_completion = (run_trace(bundle, root / "red-completion", args.candidate.resolve(strict=True),
                                        red=True, scenario="completion") if args.candidate else None)
            prompts = run_trace(bundle, root / "release-prompt", scenario="prompt")
            red_prompts = (run_trace(bundle, root / "red-prompt", args.candidate.resolve(strict=True),
                                     red=True, scenario="prompt") if args.candidate else None)
            split_traces = {}
            for scenario, directory in (("split-search", "split"), ("wrapped-split-search", "wrap")):
                key = scenario.replace("-", "_")
                split_traces[key] = run_trace(bundle, root / f"release-{directory}", scenario=scenario)
                split_traces[f"red_{key}"] = (run_trace(bundle, root / f"red-{directory}",
                                                       args.candidate.resolve(strict=True), red=True,
                                                       scenario=scenario) if args.candidate else None)
            records = None
            if args.observer:
                observer = args.observer.resolve(strict=True)
                trace = root / "snapshot.jsonl"
                for name, output in [("disabled", None), ("enabled", trace)]:
                    observed = run_trace(bundle, root / name, observer, output)
                    for actual, expected in zip(observed, checkpoints, strict=True):
                        # The transient message row can vary between renders.
                        if actual["frame"][:-1] != expected["frame"][:-1]:
                            raise ValueError(
                                f"{name}: release frame mismatch at {expected['step']}\n"
                                + "release:\n" + "\n".join(expected["frame"])
                                + "\nobserver:\n" + "\n".join(actual["frame"])
                            )
                records = [json.loads(line) for line in trace.read_text().splitlines()]
                verify_records(records)
    print(json.dumps({"release": VERSION, "commit": COMMIT, "archive_sha256": digest,
                      "target": target, "checkpoints": checkpoints, "events": records, "red_checkpoints": red_frames,
                      "paging": pages, "red_paging": red_pages, "repeat": repeats, "red_repeat": red_repeats,
                      "completion": completion, "red_completion": red_completion,
                      "prompt": prompts, "red_prompt": red_prompts,
                      **split_traces}, indent=2))


if __name__ == "__main__":
    main()
