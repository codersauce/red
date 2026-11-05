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
    yield "comments-tutor-single", " --> Comment me please\n", ["<c-c>", "<c-c>", "u", "U"], "txt"
    lines = " --> first\n --> second\n leave this alone\n # --> third\n --> fourth\n --> fifth\n"
    markers = ["%", "s", {"paste": "-->"}, "<enter>"]
    yield "comments-tutor-multiple", lines, markers + ["v", "<c-c>", "<c-c>", "<esc>", "u", "U"], "txt"
    yield "comments-line", "  alpha\r\n    βeta\r\n\r\n", list("% c cuU"), "rs"
    yield "comments-block", "alpha beta alpha\n", list("%salpha") + ["<enter>", ")"] + list(" C CuU"), "js"
    yield "comments-auto-unblock", "/** doc */\n/*! test */\n", list("% c cu"), "rs"
    yield "comments-line-only", "x = 2\ny = 3\n", list("vx C") + [" ", "<a-c>"], "py"
    yield "comments-block-only", "  hello\n    world\n", list("%") + [" ", "<a-c>"] + list(" cu"), "md"
    yield "comments-plain", "one two\n", list("vl C CuU"), "txt"


def increment_cases():
    up, down = "<c-a>", "<c-x>"
    enter = "<enter>"
    # Tutor 7.4 selects a single digit on each successive line.
    yield "increment-tutor", " --> 2) Next point.\n --> 3) Another point.\n --> 6) Last point.\n", [
        "5", "l", up, "j", up, "j", down, "u", "U",
    ]
    yield "increment-digit-under-cursor", "123 apples\r\n", ["l", up, "3", down, "v", "h", up, "u", "U"]
    select_words = list("%s") + [{"paste": r"\S+"}, enter]
    yield "increment-mixed-multiple", "9 é 99\n-1 0\n", select_words + [
        ")", "<a-;>", '"', "#", "2", up, "u", "U", '"', "#", down,
    ]
    yield "increment-integer-formats", "0b0011 0o007 0x00FF 0x00ff 999_999 -1 010 +2\n", select_words + [
        up, "2", down, "u", "U",
    ]
    yield "increment-boundaries", "0x00 0o0 0b0 -001 000_009 0x_FF 0x000a_FF 170141183460469231731687303715884105727\n", select_words + [up, down, "2", down, up]
    yield "increment-invalid-select", "words", ["v", "l", up, down, "<esc>"]
    select_lines = list("%s") + [{"paste": r"[^\n]+"}, enter]
    yield "increment-calendar", "2024-02-28\n2023/02/28\n2024-02-28 23:59:59\n2024/02/29 00:00\n", select_lines + [
        up, "2", down, "u", "U",
    ]
    yield "increment-date-spellings", "Thu Feb 29 2024\n29-Feb-2024\n2024 Feb 29\nFeb 29, 2024\n", select_lines + [up, down]
    yield "increment-times", "23:59:59\n00:00\n11:59 pm\n12:00:12 AM\n7:21 AM\n7:21:53 am\n", select_lines + [up, "2", down]
    yield "increment-invalid-formats", "2021-02-29\n1980-2-21\n12:34:60\n12:30 Pm\n0xZ\n_1\n", select_lines + ["v", up, "<esc>"]
