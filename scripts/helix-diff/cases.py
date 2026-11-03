"""Key sequences compared against a real Helix binary.

The strings are original fixtures. Expected ranges are captured at runtime
from Helix, not copied from Helix's tests.
"""

from __future__ import annotations


def case(case_id: str, text: str, keys: str, path: str = "") -> dict:
    return {"id": case_id, "text": text, "keys": keys, "path": path}


WORDS = "add title prefixes su\n"
HELLO = "hello world\n"
PUNCT = "foo,bar  baz\n"
SPACES = "alpha   beta\n"
LINES = "one\ntwo\nthree\n"
EMPTY = "one\n\ntwo\n"
UNICODE = "héllo wörld\n"
CJK = "日本語 テスト\n"
INDENT = "    alpha\nbeta\n"
DUP = "red green red\n"
RUST = "fn main() {\n    let value = 1;\n}\n"
RUST_PATH = "fixture.rs"
PAGE = "".join(f"line {index:02d} ----\n" for index in range(1, 31))


def all_cases() -> list[dict]:
    cases: list[dict] = []

    def add(case_id: str, text: str, keys: str, path: str = "") -> None:
        cases.append(case(case_id, text, keys, path))

    add("cursor-initial", "hello\n", "")
    add("empty-doc", "", "")
    add("newline-only", "\n", "")
    add("no-final-newline", "tail", "")

    for keys in ("h", "l", "ll", "lll", "3l", "j", "k", "jj", "3j", "{left}", "{right}", "{down}", "{up}"):
        add(f"move-{keys.strip('{}') or 'h'}", LINES, keys)
    add("move-h-at-start", "ab\n", "h")
    add("move-l-at-eol", "ab\n", "lll")
    add("move-j-last", LINES, "jjj")
    add("move-k-first", LINES, "k")

    word_keys = (
        "w",
        "ww",
        "www",
        "wwww",
        "3w",
        "b",
        "e",
        "ee",
        "W",
        "WW",
        "B",
        "E",
        "llw",
        "wb",
        "be",
        "vw",
        "ve",
    )
    for name, text in (
        ("words", WORDS),
        ("hello", HELLO),
        ("punct", PUNCT),
        ("spaces", SPACES),
        ("lines", LINES),
        ("empty", EMPTY),
        ("unicode", UNICODE),
        ("cjk", CJK),
        ("end", "tail"),
    ):
        for keys in word_keys:
            add(f"word-{name}-{keys}", text, keys)

    add("combo-wd", HELLO, "wd")
    add("combo-wd-words", WORDS, "wd")
    add("combo-3wd", "one two three four\n", "3wd")
    add("combo-ed", HELLO, "ed")
    add("combo-Wd", PUNCT, "Wd")
    add("delete-cursor", "hello\n", "d")
    add("delete-noyank", "hello\n", "{alt-d}")
    add("change-word", HELLO, "wcX{esc}")
    add("change-noyank", "hello\n", "{alt-c}Z{esc}")

    add("find-x", "ab\ncdx\n", "fx")
    add("find-count", "axaxax\n", "2fx")
    add("till-x", "ab\ncdx\n", "tx")
    add("find-prev", "ab\ncdx\n", "fxFa")
    add("till-prev", "ab\ncdx\n", "fxTa")
    add("find-miss", "ab\ncdx\n", "fz")
    add("find-from-middle", "ab\ncdx\n", "jfx")

    add("line-x", LINES, "x")
    add("line-xx", LINES, "xx")
    add("line-xxx", LINES, "xxx")
    add("line-3x", LINES, "3x")
    add("line-X", LINES, "X")
    add("line-alt-x", "  one\n", "{alt-x}")
    add("line-x-then-shrink", LINES, "xx{alt-x}")
    add("select-all", "ab\ncd\n", "%")
    add("select-all-empty", "", "%")
    add("select-all-no-nl", "tail", "%")
    add("combo-xx", LINES, "xx")
    add("combo-xd", "keep me\n", "xd")

    add("yank-paste-after", "ab\n", "yp")
    add("yank-paste-before", "ab\n", "yP")
    add("yank-word-paste", HELLO, "ywp")
    add("replace-yanked", "ab\n", "ylprR")
    add("replace-char", "hello\n", "rZ")
    add("replace-keeps-newline", "ab\n", "llrZ")
    add("toggle-case", "AbC\n", "~")
    add("lower", "AbC\n", "`")
    add("upper", "AbC\n", "{alt-`}")
    add("toggle-count", "AbCd\n", "3~")

    add("insert-before", "hello\n", "iX{esc}")
    add("insert-stay", "hello\n", "iX")
    add("append", "hello\n", "aX{esc}")
    add("insert-line-start", INDENT, "IZ{esc}")
    add("insert-line-end", "hello\n", "AZ{esc}")
    add("open-below", "ab\n", "oZ{esc}")
    add("open-above", "ab\n", "OZ{esc}")
    add("multi-insert", "foo bar foo\n", "%sfoo{enter}cZ{esc}")

    add("goto-gg", LINES, "gg")
    add("goto-3gg", LINES, "3gg")
    add("goto-ge", LINES, "ge")
    add("goto-G", LINES, "G")
    add("goto-2G", LINES, "2G")
    add("goto-gh", INDENT, "gh")
    add("goto-gl", INDENT, "gl")
    add("goto-gs", INDENT, "gs")
    add("goto-home", INDENT, "{home}")
    add("goto-end", INDENT, "{end}")
    add("goto-gt", PAGE, "gt")
    add("goto-gc", PAGE, "gc")
    add("goto-gb", PAGE, "gb")
    add("goto-last-mod", "hello\n", "iX{esc}g.")
    add("goto-next-buffer", LINES, "gn")
    add("goto-prev-buffer", LINES, "gp")
    add("goto-alt-buffer", LINES, "ga")
    add("goto-def", RUST, "gd", RUST_PATH)
    add("goto-decl", RUST, "gD", RUST_PATH)
    add("goto-type", RUST, "gy", RUST_PATH)
    add("goto-impl", RUST, "gi", RUST_PATH)

    add("select-mode", "abcdef\n", "v")
    add("select-extend", "abcdef\n", "vl")
    add("select-collapse", "abcdef\n", "vl;")
    add("flip-word", HELLO, "w{alt-;}")
    add("flip-cursor", "abcdef\n", "{alt-;}")

    add("search-next-hit", DUP, "/red{enter}")
    add("search-repeat", DUP, "/red{enter}n")
    add("search-back", DUP, "?red{enter}")
    add("search-prev", DUP, "/red{enter}N")
    add("search-selection", DUP, "*")
    add("search-prompt", DUP, "/red")
    add("command-prompt", LINES, ":")

    add("match-bracket", "(hello)\n", "mm")
    add("match-inner", "(hello)\n", "f(mm")
    add("object-inner-word", HELLO, "miw")
    add("object-around-word", HELLO, "maw")
    add("object-inner-paren", "(hello)\n", "lmi(")
    add("object-around-paren", "(hello)\n", "lma(")
    add("object-paragraph", "alpha beta\n\ngamma\n", "mip")
    add("surround-add", "word\n", "ms(")
    add("surround-replace", "word\n", "ms(mr[")
    add("surround-delete", "(word)\n", "md(")

    add("regex-select", "foo bar foo\n", "%sfoo{enter}")
    add("regex-then-change", "foo bar foo\n", "%sfoo{enter}cZ{esc}")
    add("split-whitespace", "a  b  c\n", "S\\s+{enter}")
    add("split-lines", "a\nb\n", "%{alt-s}")
    add("keep-primary", "foo bar foo\n", "%sfoo{enter},")
    add("drop-primary", "foo bar foo\n", "%sfoo{enter}{alt-,}")
    add("rotate-forward", "foo bar foo\n", "%sfoo{enter})")
    add("rotate-back", "foo bar foo\n", "%sfoo{enter})(")
    add("copy-next-line", "abc\ndef\n", "C")
    add("copy-prev-line", "abc\ndef\n", "j{alt-C}")
    add("keep-regex", "foo\nbar\nfoo\n", "%sfoo|bar{enter}Kfoo{enter}")
    add("drop-regex", "foo\nbar\nfoo\n", "%sfoo|bar{enter}{alt-K}foo{enter}")
    add("align", "a\nbb\n", "%s[ab]+{enter}&")
    add("trim", "  hi  \n", "X_")

    add("indent", "alpha\n", ">")
    add("unindent", "    alpha\n", "<")
    add("join", "a\nb\n", "%J")
    add("comment", RUST, "j{ctrl-c}", RUST_PATH)
    add("format", RUST, "=", RUST_PATH)
    add("syntax-expand", RUST, "{alt-o}", RUST_PATH)
    add("syntax-shrink", RUST, "{alt-o}{alt-i}", RUST_PATH)
    add("syntax-next", RUST, "{alt-n}", RUST_PATH)
    add("syntax-prev", RUST, "{alt-p}", RUST_PATH)

    add("undo-line", "keep me\n", "xdu")
    add("redo", "keep me\n", "xduU")
    add("page-down", PAGE, "{ctrl-f}")
    add("page-up", PAGE, "{ctrl-f}{ctrl-b}")
    add("half-down", PAGE, "{ctrl-d}")
    add("half-up", PAGE, "{ctrl-d}{ctrl-u}")
    add("page-key", PAGE, "{pagedown}")
    add("jump-back", HELLO, "w{ctrl-o}")
    add("jump-forward", HELLO, "w{ctrl-o}{ctrl-i}")
    add("view-center", PAGE, "zz")
    add("view-top", PAGE, "zt")
    add("view-bottom", PAGE, "zb")
    add("scroll-down", PAGE, "zj")
    add("scroll-up", PAGE, "zk")

    return cases
