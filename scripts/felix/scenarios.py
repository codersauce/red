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
    yield "cancel-invalid-search", "foo 😀 bar foo\n", list("/foo") + enter + list("?ba") + ["<esc>"] + list("/[") + enter + ["<esc>", "n"]
    yield "search-viewport", "home\n" + "a\n" * 40 + "target\n" + "a\n" * 25, list("/ta") + ["<esc>"] + list("/ta[") + ["<esc>"] + list("/ta") + enter + list("?ho") + ["<c-c>"]
    yield "search-wrapped-viewport", "abcd" * 150 + "goal" + "efgh" * 100 + "end\n", (
        list("/goal") + ["<esc>"] + list("/goal") + enter + list("/end") + ["<c-c>"]
        + list("?abcd[") + ["<esc>"])
    yield "empty-and-keep", "ab abc abc\n", list("%sab") + enter + list("Kx") + enter + ["<a-K>", "a", "<enter>", "<esc>"]
    yield "search-jump-history", "a1 ab a2 ab a3\n", list("/ab") + enter + ["<c-o>", "<tab>", "/", "<enter>"] + list("?a") + ["<backspace>", "<c-c>"]
    yield "prompt-history", "red green blue red\n", (list("/green") + enter + list("/blue") + enter
        + ["?", "<up>", "<c-p>", "<up>", "<down>", "<c-n>", "<enter>", "/", "<down>", "<esc>"]
        + list("%s") + ["<up>", "<up>", "<enter>"])
    yield "prompt-completions", "foo far fox far/ zoo\n", (
        list("/fox") + enter + list("/foo") + enter + list("/far") + enter + list("/foo") + enter
        + list("?f") + ["<backtab>", "<tab>", "<tab>", "<c-q>", "<tab>", "<backspace>", "<tab>", "<esc>"]
        + list("/far/") + enter + ["/", "<tab>", "<tab>", "<enter>", "<enter>"]
        + list("%sf") + ["<tab>", "<backtab>", "<esc>"])
    yield "prompt-edit", "red café e\u0301 😀 cats dog cat/dogs\n", (list("/cats") + ["<left>", "<backspace>", "r",
        "<home>", "<delete>", "<end>", "<c-b>", "<c-k>", "<c-a>", "<c-f>", "<c-d>", "<c-e>", "<c-u>"]
        + list("e\u0301😀") + ["<left>", "<right>", "<backspace>", "<c-h>", "<c-c>"])
    yield "prompt-words-and-paste", "word red fox cat/dogs\n", (["/", {"paste": "red fox cat/dogs"}]
        + ["<a-b>", "<a-d>", "<c-w>", "<home>", "<a-f>", "<c-k>", "<c-u>", "<c-s>", "<enter>"]
        + ["?", {"paste": "fox"}, "<enter>"])
    yield "search-identical-selections", "foo foo BAR foo\n", list("%sfoo") + enter + ["*", "n", "N", "<a-*>"]
    yield "quoted-objects", '(one (two)) "foo" bar\n', list('5lmi(2ma(gg13lmi"ma"ggmm')
    yield "nested-delimiters", "A {(b[c]d)e} and (f(g)h) unmatched {\n", (
        list("5lmi[ma]") + ["<a-;>"] + list("mi{gg3l2ma(gg20l2mi)gg34lmi{"))
    yield "quote-boundaries", 'a "one" mid "two" end\n', list('2lmi"lmi"ma"gg8lmi"gg8l2ma"gg12lmm')
    yield "bracket-matches", "[x(y)] 'aa' ()\n", (
        list("mmvmm") + ["<esc>"] + list("gg2lmmgg7lmmgg12lmm"))
    yield "unicode-delimiters", "「a（b）c」 «z» |hi| |ok|\n", list("mmlmi」gg3lma（gg13lmi|")

    yield "multiple-pair-objects", "[x] no x (x) [éx] x\n", (
        list("%sx") + enter + ["<a-;>"] + list("mi[duUma[u"))
    yield "word-object-boundaries", "aa  !! bb_cc. Z\r\nend", (
        list("2miwgg2lmiwmaWgg5l3maw") + ["<a-;>"] + list("miWge2miw"))
    yield "word-object-spaces", "ab\u200bcd\u1680ef\u3000gh\ufeffij\u180ekl\x0bmn", (
        list("mawgg3lmawgg6lmawgg9lmawgg12lmawgg15lmawge2miw"))
    yield "paragraph-boundaries", "aa\r\nbb\r\n\r\ncc\r\n\r\n\r\ndd\r\n", (
        list("2mapgg2jmipgg2j2mipgg3jmapge2mipgg4jmap"))
    yield "empty-paragraphs", "\n\n", list("mipmapge2map")
    yield "regex-select-split-keep", "red blue red\n", list("%S ") + enter + list("Kred") + enter + list(")") + [",", "<a-;>"]
    yield "yank-paste-replace", "one two\n", list("eywpP;Rl") + ["<a-;>"] + list("rX")
    yield "counted-register-edits", "a bb ccc\r\nend\r\n", list("%s[a-z]+") + enter + list("y,2R3pPuU")
    yield "empty-register", "ab\n", list("vlRvplvP")
    yield "reverse-register-edits", "abc def\n", list("wywe") + ["<a-;>", "2", "R", "u", "U"]
    yield "character-arguments", "abc db e!\n", list("fb") + ["<a-.>"] + list("TbFr") + ["<esc>"] + list("ggvre") + ["<esc>"]
    yield "line-selections", "a\r\nbc\r\ndef", list("x2x") + ["<a-s>", ")", "<a-->", "<a-;>", "X", "<a-x>"]
    for mode, initial in (("normal", []), ("select", ["v"])):
        keys = initial.copy()
        for prefix in ("g", "m", "mi", "ma", "f", "F", "t", "T", "r"):
            for cancel in ("<esc>", "<left>"):
                keys.extend(["2", *prefix, cancel])
        keys += list("2g9l3m4ll") + ["<esc>"] + list("123456789012345") + ["<esc>"]
        keys += list("g2gm2i(ggf2r3") + ["<esc>"]
        yield f"cancel-arguments-{mode}", "abc 123 abc\nline two\n", keys
    yield "typed-selection", "abcd\n", list("eliX") + ["<esc>"] + list("uU")
    yield "unicode-vertical-goal", "ab界cd\n\tx\nabcdefg\n👩‍💻x\n", (
        list("3ljjjkkkvjj") + ["<esc>"] + list("ggllgjgjgk"))
    yield "logical-separators", "ab\vcd\u2028ef\rgh\r\n\t世界ok\u0085qr\nlast", (
        list("4lghglgg2gggsglgjgjgkgkgeghgg2j2k"))
    yield "vertical-goal", "abcdef\nxy\nuvwxyz\n", list("4ljjkkvjj") + ["<esc>"]
    yield "repeat-bracketed-paste", "ab cd\n", ["i", "X", {"paste":"é😀"}, "Y", "<esc>", ".", "u", "u", "u", "u", "U"]
    yield "bracketed-paste-modes", "one two\r\n", ["2", {"paste":"x\ny"}, "u", "v", "l", {"paste":"Z"}, "u"]
    yield "repeat-count", "ab cd ef\n", list("ia") + ["<esc>"] + list("w.2.uU")
    yield "repeat-change", "foo foo baz\n", list("%sfoo") + enter + list("cZ") + ["<esc>", ",", "w", ".", "u", "U"]
    yield "repeat-checkpoint", "abc def\n", ["A", "x", "<c-s>", "y", "<esc>", "2", ".", "u", "u", "u", "U", "U"]
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
    yield "wrapped-edges", "abcdefghijklmnopqrst\nxy\n" + "界" * 10 + "\n", list("ljjjkkkgg18ljjkkgjgk")
    yield "wrapped-mixed-goals", "abcdefghij" * 5 + "\n" + "12" * 15 + "\n" + "界" * 18 + "\n", (
        list("4ljgjgkjgjgjgkgg24lgjgjgkgk"))
    yield "wrapped-unicode-motion", "abcdefghijklmnopqr界\tbcde界fg👩‍💻hijklmnopqrstuv\nabc界de\tz\n", (
        list("4ljjkk11ljjkkvjj") + ["<esc>"] + list("gg17ljkgj"))
    yield "wrapped-zero-width", "ab\u200bcd" + "x" * 15 + "A\u200bB\u200bC" + "y" * 15 + "\nend\n", (
        list("2ljjkklljkj") + ["v", "j", "k", "<esc>"] + list("gg20ljkrZ"))
    yield "zero-width-goal", "a\u200b\u200bob\n123456\na\u200b\u200bb\n", (
        list("2ljjkkvj") + ["<esc>"] + list("gg3ljkrX"))
    yield "wrapped-zero-edge", "a" * 20 + "\u200b\u200bcd" + "b" * 20 + "\u200bZ\n", list("jjkk2ljjkkgg20ljkhjrZ")
    yield "wrapped-motion", "abcdefghijklmnopqrstuvwxyz" * 3 + "\nabc\n" + "ABCDEF" * 12 + "\n", list("4ljjkkvj") + ["<esc>"] + list("gjgkk")
    yield "page-wrapped-continuations", "abcd" * 290 + "\nxy\n" + "EFGH" * 170 + "\n", (
        ["l", "<c-f>", "<c-d>", "v", "<c-u>", "<esc>", "2", "<pagedown>", "<pageup>", "<c-b>"])
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


ARROW_AND_EDIT_KEYS = {f"<{name}>": name.title()
                       for name in ("left", "right", "up", "down", "home", "end", "delete")}


def capture(observer, runtime, root, name, text, keys):
    # Helix abbreviates paths inside its working directory before folding HOME.
    # Keep documents in HOME and launch from its child so snapshots always use ~/.
    root = root.resolve()
    root.mkdir()
    workdir = root / "work"
    workdir.mkdir()
    path = root / "example.txt"
    path.write_bytes(text.encode())
    trace = root / "trace.jsonl"
    wrap_width = 20 if name.startswith("wrapped-") or name in ("search-wrapped-viewport", "page-wrapped-continuations") else None
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
                              cwd=workdir, env=env, capture_output=True, text=True, check=check).stdout
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
            named = {"<backspace>": "BSpace", "<pageup>": "PageUp", "<pagedown>": "PageDown", "<esc>": "Escape", "<enter>": "Enter", "<tab>": "Tab", "<backtab>": "BTab", "<a-;>": "M-;", "<a-,>": "M-,"}
            named.update(ARROW_AND_EDIT_KEYS)
            paste = key["paste"] if isinstance(key, dict) else None
            if paste is not None:
                key = "<paste>"
                raw = b"\x1b[200~" + paste.encode() + b"\x1b[201~"
                tmux("send-keys", "-t", "h", "-H", *(format(byte, "02x") for byte in raw))
            elif key.startswith("<a-"):
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
            special = {"<backspace>": "Backspace", "<pageup>": "PageUp", "<pagedown>": "PageDown", "<esc>": "Esc", "<enter>": "Enter", "<tab>": "Tab", "<backtab>": "BackTab", "<a-;>": "Char(';')", "<a-,>": "Char(',')"}
            special.update(ARROW_AND_EDIT_KEYS)
            ch = key[3:-1] if key.startswith(("<a-", "<c-")) else key
            code = special.get(key, f"Char('{ch}')")
            if paste is not None:
                if record["paste"] != paste:
                    raise ValueError(f"{name}: input {index+1} expected paste {paste!r}, observed {raw_event}")
            elif (f"code: {code}," not in raw_event and
                  not (len(ch) == 1 and f"code: Char('\\u{{{ord(ch):x}}}')," in raw_event)) or ("ALT" in raw_event) != key.startswith("<a-") or ("CONTROL" in raw_event) != key.startswith("<c-"):
                raise ValueError(f"{name}: input {index+1} expected {key}, observed {raw_event}")
            events.append({"key": key, **{k: record[k] for k in
                ("mode", "text", "ranges", "primary", "unnamed", "search", "pending", "count", "document")}})
            if name.startswith("prompt-"):
                events[-1]["prompt"] = record["prompt"]
            if paste is not None:
                events[-1]["paste"] = paste
            if name.startswith("repeat-"):
                recipe = record["insert_recipe"]
                commands = {"normal_mode": "", "insert_mode": "i", "append_mode": "a", "change_selection": "c",
                            "insert_at_line_start": "I", "insert_at_line_end": "A", "open_below": "o", "open_above": "O"}
                keys = [commands[recipe["command"]]] if recipe["command"] != "normal_mode" else []
                for item in recipe["events"]:
                    token = item["key"]
                    keys.append(token if len(token) == 1 else "<" + token.lower() + ">")
                events[-1]["last_insert"] = keys
            if name.startswith("page-") or name in ("search-viewport", "search-wrapped-viewport"):
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
