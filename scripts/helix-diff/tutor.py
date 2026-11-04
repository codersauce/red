#!/usr/bin/env python3
"""Walk the Helix 25.07.1 tutor exercises against Red's helix mode.

Each supported section is the exercise text plus the keystrokes that lesson
asks for. ``probe_hx`` drives real Helix; the Red dump drives the editor.
Sections that need a feature Red does not implement are listed and not probed,
so a match means both editors finished the exercise the same way.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT / "scripts" / "helix-diff") not in sys.path:
    sys.path.insert(0, str(ROOT / "scripts" / "helix-diff"))

from cases import case  # noqa: E402
from probe_hx import run_cases  # noqa: E402
from run import compare, dump_red  # noqa: E402

# Features the 25.07.1 tutor teaches that Red's helix mode does not implement.
# Kept here so a re-run prints the same list instead of pretending they match.
UNSUPPORTED = [
    {
        "section": "4.2",
        "title": "System clipboard",
        "keys": "Space y / Space p",
        "needs": "Helix yanks and pastes the primary selection with y/p. Space y and Space p talk to the system clipboard. Red keeps Space for its own menu, so those two chords are not Helix clipboard commands.",
    },
    {
        "section": "6.3",
        "title": "Repetition",
        "keys": ". and Alt-.",
        "needs": "A replay of the last insert (.) and of the last f/t motion (Alt-.). Red does not record those for the helix grammar.",
    },
    {
        "section": "8.1",
        "title": "Registers",
        "keys": "\" <char>",
        "needs": "Named registers. Yank, paste, and R currently use one default register. Selecting \"b before a yank needs a register map keyed by character.",
    },
    {
        "section": "8.2",
        "title": "Macros",
        "keys": "Q and q",
        "needs": "Macro recording into a register and replay. That is a keystroke log plus a player, separate from the command grammar.",
    },
    {
        "section": "9.4",
        "title": "Two-character labels",
        "keys": "gw",
        "needs": "An overlay that replaces visible word starts with two-character labels and jumps when those characters are typed. It is a popup, not a buffer edit.",
    },
    {
        "section": "13.1",
        "title": "Create a new split",
        "keys": "Ctrl-w n v / Ctrl-w n s",
        "needs": "Helix's window menu. Red keeps Ctrl-w as its own window menu, so the tutor's nv/ns chords are not implemented.",
    },
    {
        "section": "13.2",
        "title": "Move between splits",
        "keys": "Ctrl-w hjkl / Ctrl-w w / Ctrl-w q / Ctrl-w o",
        "needs": "Helix focus and close commands on Ctrl-w. Red's Ctrl-w menu uses different keys.",
    },
    {
        "section": "13.3",
        "title": "Split the current buffer",
        "keys": "Ctrl-w s / Ctrl-w v",
        "needs": "The same Helix window menu. Red can split, but not with these Helix chords.",
    },
    {
        "section": "13.4",
        "title": "Split from the command line",
        "keys": ":vs / :hs",
        "needs": ":vsplit and :hsplit that open a named buffer. Red's :split / :vsplit exist for the current buffer; the tutor's :vs FILENAME form is not the same command.",
    },
    {
        "section": "13.5",
        "title": "Swap splits",
        "keys": "Ctrl-w HJKL",
        "needs": "Swapping windows in a direction. Red does not have that Helix command.",
    },
    {
        "section": "13.6",
        "title": "Transpose splits",
        "keys": "Ctrl-w t",
        "needs": "Transposing a split's layout. Red does not have that command.",
    },
    {
        "section": "13.7",
        "title": "Open a split from the file picker",
        "keys": "Space f, Ctrl-v / Ctrl-s",
        "needs": "Helix's file picker. Space f in Red is format, not the picker, and the picker does not open splits with Ctrl-v / Ctrl-s.",
    },
]


def tutor_cases() -> list[dict]:
    cases: list[dict] = []

    def add(section: str, text: str, keys: str) -> None:
        cases.append(case(f"tutor-{section}", text, keys))

    add("1.1", "abcde\nfghij\nklmno\n", "lljhhk")
    add("1.2", "stay here\n", ":{esc}")
    add(
        "1.3",
        " --> Thhiss senttencee haass exxtra charracterss.\n",
        "llllllldlldllllldlllldllldldllldlllllllldlllllld",
    )
    add(
        "1.4",
        " --> Th stce misg so.\n",
        "llllllliis{esc}llien{esc}lien{esc}llliis {esc}lllisin{esc}llllime text{esc}",
    )
    add("1.5", "notes\n", ":w{enter}")
    add(
        "2.1",
        " --> This sentence is miss\n     This sentence is missing some text.\n",
        "Aing some text{esc}",
    )
    add("2.2", " --> What is the best editor?\n", "oHelix{esc}")
    add(
        "3.1",
        " --> This sentence pencil has vacuum extra words in the it.\n",
        "wwwwdwdwwdwwd",
    )
    add("3.2", "--> The Middle Kingdom.\n", "fdeb")
    add("3.3-words", '--> Helix is a one-of-a-kind "modal" text editor\n', "wwwwwwwwww")
    add("3.3-WORDS", '--> Helix is a one-of-a-kind "modal" text editor\n', "ghWWWWWW")
    add(
        "3.4",
        " --> This paper has heavy words behind it.\n",
        "wwecesentence{esc}wecincorrect{esc}wecin{esc}",
    )
    add(
        "3.5",
        " --> This is just a line with words you can move around in.\n",
        "2w3e2b",
    )
    add(
        "3.6",
        " --> Remove the FOO BAR distracting words BAZ BIZ from this line.\n",
        "fFv2wdfBv2wd",
    )
    add(
        "3.7",
        " --> 1) Roses are red,\n"
        " --> 2) Mud is fun,\n"
        " --> 3) Violets are blue,\n"
        " --> 4) I have a car,\n"
        " --> 5) Clocks tell time,\n"
        " --> 6) Sugar is sweet,\n"
        " --> 7) And so are you.\n",
        "jxdj2xd",
    )
    add("3.8", " --> This is an error-free line with words to move around in.\n", "w;")
    add(
        "4.1",
        " --> Fiix the errors on thhis line and reeplace them witth undo.\n",
        "lllllduU",
    )
    add("4.2", " --> 1 banana 2 3 4\n", "fbwyp2fp")
    add(
        "4.3",
        " --> banana splits\n --> another banana\n",
        "/banana{enter}nN",
    )
    add(
        "5.1",
        " --> Fix th two nes at same ime.\n"
        " -->\n"
        " --> Fix th two nes at same ime.\n",
        "fFlliis{esc}fneeeli{esc}fmee{esc},",
    )
    add(
        "5.2",
        " --> I like to eat apples since my favorite fruit is apples.\n",
        "xsapples{enter}coranges{esc},",
    )
    add(
        "5.3",
        " --> This  sentence has   some      extra spaces.\n",
        "xs  +{enter}c {esc}",
    )
    add(
        "5.4",
        " --> 97) lorem\n --> 98) ipsum\n --> 99) dolor\n --> 100) sit\n --> 101) amet\n",
        "f 4CW&",
    )
    add(
        "5.5",
        "    | FRUIT   | AMOUNT |\n"
        "    |---------|--------|\n"
        " | Apples  | 8      |\n"
        "    | Bananas | 6      |\n"
        "  | Oranges | 3      |\n"
        "     | Donuts  | 4      |\n",
        "6x{alt-s}&",
    )
    add(
        "6.1",
        " --> -----[Free this sentence of its brackets!]-----\n"
        " --> ------Free this sentence of its dashes!------\n",
        "f-f[dglF]djf-t-dglT-d",
    )
    add(
        "6.2",
        " | Month | Days |\n |=======|------|\n | Jan   | 31   |\n",
        "jf=t|r-",
    )
    add(
        "7.1",
        " --> I like watermelons because oranges are refreshing.\n",
        "wwy2wR",
    )
    add(
        "7.2",
        " --> This sentence\nis spilling over\nonto other\nlines.\n",
        "4xJ",
    )
    add(
        "7.3",
        " --> These lines\n    are indented\n         very poorly.\n",
        "j>j<",
    )
    add(
        "7.4",
        " --> 1) First point.\n"
        " --> 2) Added point.\n"
        " --> 2) Next point.\n"
        " --> 3) Another point.\n"
        " --> 6) Last point.\n",
        "jjf2{ctrl-a}jf3{ctrl-a}jf6{ctrl-x}",
    )
    add(
        "9.1",
        " --> A horse is a horse, of course, of course,\n"
        " --> And no one can talk to a horse of course.\n",
        "fhe*nN",
    )
    add(
        "9.2",
        " --> Everybody wants to be a bat,\n"
        " --> because a cat's the only bat\n"
        " --> who knows where it's at.\n",
        "fbe*vnccat{esc}",
    )
    lines = "".join(f"line {index:02d}\n" for index in range(1, 40))
    add("9.3", lines, "{ctrl-s}ge{ctrl-o}")
    add(
        "10.1",
        " --> How much would would a wouldchuck chuck\n"
        " --> if a wouldchuck could chuck would?\n",
        "2xswould{enter}){alt-,}cwood{esc}",
    )
    add(
        "10.2",
        " --> Jumping through the water,\n --> daring to know.\n",
        "2xsthrough|water|know{enter}{alt-(}",
    )
    add(
        "10.3",
        " --> thIs sENtencE hAs MIS-cApitalIsed leTTerS.\n"
        " --> this SENTENCE SHOULD all be in LOWERCASE.\n"
        " --> THIS sentence should ALL BE IN uppercase!\n",
        "x`jx{alt-`}",
    )
    add(
        "10.4",
        "---\n"
        "these are sentences. some sentences don't start with uppercase\n"
        "letters! that is not good grammar. you can fix this.\n",
        "j2xS\\. |! {enter}{alt-;};{alt-`}",
    )
    add("11.1", " --> Comment me please\n", "{ctrl-c}{ctrl-c}")
    add(
        "11.2",
        " --> How many are you going to comment?\n"
        " --> Is this enough for a comment?\n"
        " --> What are you doing?!\n"
        " --> Stop commenting me!\n"
        " --> AAAAaargh!!!\n",
        "5x{ctrl-c}",
    )
    add("12.1", " --> you can (jump between matching parentheses)\n", "f(mm")
    add(
        "12.2",
        " --> outside and (inside x parentheses) - and outside again\n",
        "fxi(",
    )
    add(
        "12.3",
        " --> you ( select x around ) to include delimiters in the select\n",
        "fxma(d",
    )
    add(
        "12.4",
        " --> so, select all of this, and surround it with ()\n",
        "fsev4ems(",
    )
    add(
        "12.5",
        " --> delete (the x pair of parentheses) from within!\n",
        "fxmd(",
    )
    add(
        "12.6",
        " --> replace the (pair from x within), with something else\n",
        "fxmr([",
    )
    return cases


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ids", default="")
    parser.add_argument("--skip-red", action="store_true")
    parser.add_argument("--out", type=Path, default=Path("/tmp/helix-diff/tutor.json"))
    args = parser.parse_args()
    cases = tutor_cases()
    if args.ids:
        wanted = set(args.ids.split(","))
        cases = [item for item in cases if item["id"] in wanted or item["id"].removeprefix("tutor-") in wanted]
    print(f"unsupported sections: {len(UNSUPPORTED)}")
    for item in UNSUPPORTED:
        print(f"  {item['section']} {item['title']}: {item['keys']}")
    out_dir = args.out.parent
    out_dir.mkdir(parents=True, exist_ok=True)
    hx = run_cases(cases)
    (out_dir / "tutor-hx.json").write_text(json.dumps(hx, indent=2) + "\n", encoding="utf-8")
    if args.skip_red:
        return
    red = dump_red(cases, out_dir / "tutor-red.json", out_dir / "tutor-cases.tsv")
    rows = compare(hx, red)
    payload = {"unsupported": UNSUPPORTED, "results": rows}
    args.out.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    counts: dict[str, int] = {}
    for row in rows:
        counts[row["status"]] = counts.get(row["status"], 0) + 1
        if row["status"] != "match":
            print(f"\n{row['status']} {row['id']} keys={row['keys']!r}")
            if detail := row.get("detail"):
                print(detail)
    print("tutor summary", counts, "of", len(rows))
    if counts.get("differ") or counts.get("helix-error") or counts.get("missing-red"):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
