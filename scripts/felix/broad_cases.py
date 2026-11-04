"""Document editing scenarios for the broad Helix compatibility suite."""


def shell_cases():
    text = "cat dog\ncat elf\n"
    words = ["%", "s", {"paste": "[a-z]+"}, "<enter>"]

    def submit(key, command):
        return [key, {"paste": command}, "<enter>"]

    yield "shell-pipe-undo", text, words + submit("|", "tr a-z A-Z") + ["u", "U"]
    yield "shell-insert-append", text, words + submit("!", "printf '界\\n'") + submit("<a-!>", "printf Z") + ["u", "u"]
    yield "shell-keep", text, words + [")"] + submit("$", "grep -qx cat") + submit("$", "false")
    yield "shell-pipe-to", text, words + submit("<a-|>", "cat >> seen") + submit("!", "wc -c < seen | tr -d ' '")
    yield "shell-atomic-failure", text, words + submit("|", 'v=$(cat); test "$v" = cat && printf ok')
    yield "shell-reverse-crlf", "alpha\r\nbeta\r\n", list("x") + ["<a-;>"] + submit("|", "cat") + submit("<a-!>", "printf '\\r\\n'") + ["u"]
    yield "shell-edit-history", text, ["|", "c", "a", "t", "<enter>", "|", "<up>", "<home>", "<end>", "<backspace>", "t", "<enter>", "|", "<esc>"]
    yield "shell-register-and-word", text, ['"', 'a', 'y', '!', {"paste": "printf "}, "<c-r>", 'a', "<enter>", 'u', '!', {"paste": "printf "}, "<c-s>", "<enter>"]
    yield "shell-nonzero-stderr", text, submit("|", "printf err >&2; exit 1") + ["u"] + submit("$", "printf err >&2; exit 1")
    yield "shell-stderr", text, submit("!", "printf out; printf err >&2") + ["u"] + submit("!", "false")


def navigation_cases():
    paras = "one\n  space\n\n\nsecond\n\nlast\n"
    yield "navigation-paragraphs", paras, list("]p]p[p2[pv]p")
    yield "navigation-blank-lines", "solo\r\nthird\r\n", list("2] j[ ") + ["u", "u", "U"]
    words = ["%", "s", {"paste": "[a-z]+"}, "<enter>"]
    yield "navigation-rotate-group", "abc d\ne fgh\n", words + [")", "<a-)>", "2", "<a-(>", "u", "U"]
    yield "navigation-word-labels", "one two x three\nfour five six\n", list("gwaagwabvgwaa") + ["<esc>"] + list("gwz")
    yield "navigation-align", "a x\nbbb y\n", ["%", "s", {"paste": "[xy]"}, "<enter>", "&", "u", "U"]
    lines = "".join(f"line {i}\n" for i in range(48))
    yield "page-navigation-goto", lines, list("gbgt3gbgc")
    yield "page-navigation-view", lines, ["<c-d>", "<c-d>"] + list("ztzzzbZjjkk") + ["<esc>", "h"]
    yield "navigation-host-branch", "one\n", ["i", "a", "<esc>", "i", "b", "<esc>", "u", "i", "c", "<esc>", "<a-u>", "<a-U>", "2", "<a-u>", "2", "<a-U>"]
    yield "navigation-host-last-edit", "abc\ndef\n", ["j","i","X","Y","<esc>","g","g","g","."]


def comment_cases():
    yield "comments-line", "  alpha\r\n    βeta\r\n\r\n", list("% c cuU"), "rs"
    yield "comments-block", "alpha beta alpha\n", list("%salpha") + ["<enter>", ")"] + list(" C CuU"), "js"
    yield "comments-auto-unblock", "/** doc */\n/*! test */\n", list("% c cu"), "rs"
    yield "comments-line-only", "x = 2\ny = 3\n", list("vx C") + [" ", "<a-c>"], "py"
    yield "comments-block-only", "  hello\n    world\n", list("%") + [" ", "<a-c>"] + list(" cu"), "md"
    yield "comments-plain", "one two\n", list("vl C CuU"), "txt"
