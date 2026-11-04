"""Drive the pinned Helix observer to produce per-key Red integration fixtures."""

import argparse
import json
import os
from pathlib import Path
import random
import tomllib
import subprocess
import tempfile
import time

COMMIT = tomllib.loads(Path(__file__).with_name("coverage.toml").read_text())["reference"]


def scenarios(seed, steps):
    text = "éclair e\u0301!  👩‍💻\r\n\tlast line\r\n"
    yield "word-boundaries", text, list("wwb2EW3BveB") + ["<esc>"] + list("gggsglge")
    yield "reverse-at-eof", "A\r\n", ["%", "<a-;>", ";", "9", "l", "v", "h", "h", "l", "l", "<esc>"]
    yield "empty-rope", "", list("hlewBb3gg%") + ["<a-;>", "v", "h", "<esc>"]
    yield "keep-and-rotate", "ab ab abc\n", list("%sab") + ["<enter>", ")", "(", "2", ")", "<a-,>", ","]
    enter = ["<enter>"]
    yield "search-and-navigate", "foo xFOO foo\n", list("/foo") + enter + list("nnNvN") + ["<esc>"] + list("?x") + enter
    yield "regex-select-split-keep", "red blue red\n", list("%S ") + enter + list("Kred") + enter + list(")") + [",", "<a-;>"]
    yield "yank-paste-replace", "one two\n", list("eywpP;Rl") + ["<a-;>"] + list("rX")
    yield "character-arguments", "abc db e!\n", list("fb") + ["<a-.>"] + list("TbFr") + ["<esc>"] + list("ggvre") + ["<esc>"]
    yield "line-selections", "a\r\nbc\r\ndef", list("x2x") + ["<a-s>", ")", "<a-->", "<a-;>", "X", "<a-x>"]
    yield "typed-selection", "abcd\n", list("eliX") + ["<esc>"] + list("uU")
    yield "vertical-goal", "abcdef\nxy\nuvwxyz\n", list("4ljjkkvjj") + ["<esc>"]
    yield "append-and-undo", "ab cd\n", list("eaXY") + ["<esc>"] + list("uU") + ["%", "a", "!", "<esc>", "u"]
    yield "explicit-objects", "(ab cd) \"ef gh\"\n", list("lmiwmawmi(ma(ggmm") + ["<esc>"]
    yield "insert-controls", "  abc def\n", list("A") + ["<c-w>", "z", "<c-s>", "<c-u>", "q", "<esc>", "u", "u", "U"]
    yield "indent-join", "abc\n  de f\nxy\n", list("x>") + ["<", "J", "<a-J>", "u", "U"]
    yield "open-line", "abc\nxyz\n", list("oHi") + ["<esc>", "u", "O", "z", "<esc>", "u"]
    yield "selection-jumplist", "abc de\nxyz\n", list("e") + ["<c-s>"] + list("gegg") + ["<c-o>", "<c-o>", "<tab>"]
    yield "jump-follows-edit", "one two three\n", list("ww") + ["<c-s>"] + list("ggiéé") + ["<esc>", "<c-o>", "<c-o>", "<tab>"]
    yield "auto-pairs", "abc \n", list("A(x)") + ["<esc>", "u", "U"]
    yield "whole-line-change", "one\ntwo\n", list("xcX") + ["<esc>", "u", "%", "c", "Y", "<esc>", "u"]
    yield "paragraph-and-copies", "abc\ndef\n\nnext\n", list("lC") + ["<a-C>", ","] + list("mipmap")
    yield "page-movement", "abc\n" * 90, ["l", "<pagedown>", "2", "<c-f>", "<c-b>", "<c-d>", "v", "<c-u>", "<esc>", "<pageup>"]
    yield "wrapped-motion", "abcdefghijklmnopqrstuvwxyz" * 3 + "\nabc\n" + "ABCDEF" * 12 + "\n", list("4ljjkkvj") + ["<esc>"] + list("gjgkk")
    yield "page-through-eof", "abc\n" * 40, ["<c-f>", "<c-f>", "<c-f>", "<c-b>", "<c-b>"]
    yield "page-edges", "a\nbc\ndef", ["<c-f>", "<c-f>", "<c-u>", "<pageup>", "i", "<pagedown>", "<pageup>", "<esc>"]
    yield "page-multiple", "abcdef\n" * 75, ["l", "C", "<c-d>", "<c-f>", "<c-u>", "<pageup>"]
    # Every seed includes a reproducible mix of direction changes and pending prefixes.
    rng = random.Random(seed)
    commands = ["w", "B", "E", "e", "h", "l", "v", "%", ";", "3W", "2b", "gg", "ge", "gh", "gl", "gs", "<esc>", "<a-;>"]
    keys = []
    for _ in range(steps):
        cmd = rng.choice(commands)
        keys.extend([cmd] if cmd.startswith("<") else list(cmd))
    yield f"motion-seed-{seed}", text, keys


def capture(observer, runtime, root, name, text, keys):
    root.mkdir()
    path = root / "example.txt"
    path.write_bytes(text.encode())
    trace = root / "trace.jsonl"
    wrap_width = 20 if name == "wrapped-motion" else None
    if wrap_width:
        config = root / "config" / "helix"
        config.mkdir(parents=True)
        (config / "config.toml").write_text(
            f"[editor]\ntext-width = {wrap_width}\n[editor.soft-wrap]\nenable = true\n"
            'wrap-at-text-width = true\nwrap-indicator = ""\nmax-indent-retain = 0\nmax-wrap = 0\n'
        )
    env = {"PATH": os.environ["PATH"], "HOME": str(root), "SHELL": "/bin/sh",
           "LANG": "en_US.UTF-8", "HELIX_RUNTIME": str(runtime),
           "FELIX_REFERENCE_TRACE": str(trace), "XDG_CONFIG_HOME": str(root/"config")}
    def tmux(*args, check=True):
        return subprocess.run(["tmux", "-f", "/dev/null", "-S", str(root/"t.sock"), *args],
                              cwd=root, env=env, capture_output=True, text=True, check=check).stdout
    try:
        tmux("new-session", "-d", "-s", "h", "-x", "80", "-y", "24", str(observer), str(path))
        deadline = time.monotonic() + 20
        while "example.txt" not in tmux("capture-pane", "-pt", "h"):
            if time.monotonic() >= deadline:
                raise TimeoutError(f"{name}: Helix did not open example.txt")
            time.sleep(.02)
        events = []
        trace_offset = 0
        for index, key in enumerate(keys):
            named = {"<pageup>": "PageUp", "<pagedown>": "PageDown", "<esc>": "Escape", "<enter>": "Enter", "<tab>": "Tab", "<a-;>": "M-;", "<a-,>": "M-,"}
            if key.startswith("<a-"):
                raw = b"\x1b" + key[3:-1].encode()
                tmux("send-keys","-t","h","-H",*[format(byte,"02x") for byte in raw])
            elif key.startswith("<c-"):
                tmux("send-keys","-t","h","C-"+key[3:-1])
            elif key == ";":
                tmux("send-keys", "-t", "h", "-H", "3b")
            elif key in named:
                tmux("send-keys", "-t", "h", named[key])
            else:
                tmux("send-keys", "-t", "h", "-l", key)
            deadline = time.monotonic() + 5
            while True:
                if trace.exists():
                    with trace.open("rb") as stream:
                        stream.seek(trace_offset)
                        line = stream.readline()
                        if line.endswith(b"\n"):
                            trace_offset = stream.tell()
                            break
                if time.monotonic() >= deadline:
                    raise TimeoutError(f"{name}: no observation for key {index + 1} ({key})")
                time.sleep(.01)
            record = json.loads(line)
            raw_event = record["event"]
            special = {"<pageup>": "PageUp", "<pagedown>": "PageDown", "<esc>": "Esc", "<enter>": "Enter", "<tab>": "Tab", "<a-;>": "Char(';')", "<a-,>": "Char(',')"}
            ch = key[3:-1] if key.startswith(("<a-", "<c-")) else key
            code = special.get(key, f"Char('{ch}')")
            if f"code: {code}," not in raw_event or ("ALT" in raw_event) != key.startswith("<a-") or ("CONTROL" in raw_event) != key.startswith("<c-"):
                raise ValueError(f"{name}: input {index+1} expected {key}, observed {raw_event}")
            events.append({"key": key, **{k: record[k] for k in
                ("mode", "text", "ranges", "primary", "unnamed", "search", "pending", "count", "document")}})
            if name.startswith("page-"):
                events[-1]["viewport"] = record["viewport"]
        case = {"name": name, "text": text, "events": events}
        if wrap_width:
            case["wrap_width"] = wrap_width
        return case
    finally:
        tmux("kill-server", check=False)


def dump_suite(commit, seed, cases):
    """Keep each captured event on its own line so behavioral changes can be reviewed."""
    encode = lambda value: json.dumps(value, ensure_ascii=False)
    print('{"commit": ' + encode(commit) + ', "seed": ' + str(seed) + ', "cases": [')
    for index, case in enumerate(cases):
        header = {key: value for key, value in case.items() if key != "events"}
        print("  " + encode(header)[:-1] + ', "events": [')
        print(",\n".join("    " + encode(event) for event in case["events"]))
        print("  ]}" + ("," if index + 1 < len(cases) else ""))
    print("]}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--observer", required=True, type=Path)
    parser.add_argument("--runtime", required=True, type=Path)
    parser.add_argument("--seed", type=int, default=250701)
    parser.add_argument("--steps", type=int, default=48)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="fx-") as tmp:
        result = [capture(args.observer.resolve(strict=True), args.runtime.resolve(strict=True),
                          Path(tmp)/str(i), *scenario)
                  for i, scenario in enumerate(scenarios(args.seed, args.steps))]
    dump_suite(COMMIT, args.seed, result)


if __name__ == "__main__":
    main()
