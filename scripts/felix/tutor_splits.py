"""Compare Helix tutor 13.4–13.6 using document text to identify each split."""

import re
import time


def run(tmux, observe, workdir):
    observe("open", "foo foo", "NOR", "1 sel")
    names = ("foo foo", "TUTOR-ONE", "TUTOR-TWO")

    def command(text):
        tmux("send-keys", ":")
        tmux("send-keys", "-l", text)
        tmux("send-keys", "Enter")

    def layout(label, expected_rows):
        # Check the relative positions of all three first lines, independent of
        # gutters, dividers and the number of cells allocated to each window.
        deadline = time.monotonic() + 5
        while True:
            lines = tmux("capture-pane", "-p").stdout.splitlines()
            positions = [{line.index(name): name for name in names if name in line}
                         for line in lines]
            columns = sorted({x for row in positions for x in row})
            rows = [[row.get(x) for x in columns] for row in positions if row]
            normal = any(re.search(r"\bNOR(?:MAL)?\b", line) for line in lines)
            if rows == expected_rows and normal and not any(re.search(r"\bINS(?:ERT)?\b", line) for line in lines):
                return {"step": label, "document_rows": rows}
            if time.monotonic() >= deadline:
                raise RuntimeError(f"{label}: expected {expected_rows!r}, saw {rows!r}\n" + "\n".join(lines))
            time.sleep(0.02)

    tutor, one, two = names
    # The first file exists; the second is created by writing the newly named split.
    (workdir / "hello1").write_text(one + "\n")
    command("vs hello1")
    command("hs hello2")
    tmux("send-keys", "i")
    tmux("send-keys", "-l", two)
    tmux("send-keys", "Escape")
    initial = [[tutor, one], [None, two]]
    frames = [layout("hs opens the selected new document below", initial)]
    command("w")
    saved = workdir / "hello2"
    deadline = time.monotonic() + 5
    while not saved.exists() or saved.read_text().splitlines() != [two]:
        if time.monotonic() >= deadline:
            frame = tmux("capture-pane", "-p").stdout
            raise RuntimeError(f":w did not save the edit to {saved}\n{frame}")
        time.sleep(0.02)
    tmux("send-keys", "C-w", "K")
    frames.append(layout("swap with upper window", [[tutor, two], [None, one]]))
    tmux("send-keys", "C-w", "H")
    frames.append(layout("swap with left window", [[two, tutor], [None, one]]))
    tmux("send-keys", "C-w", "l", "C-w", "o")
    frames.append(layout("keep tutor only", [[tutor]]))
    command("vs hello1")
    command("hsplit hello2")
    frames.append(layout("hsplit reopens the saved document below", initial))
    tmux("send-keys", "C-w", "h", "C-w", "t")
    frames.append(layout("transpose tutor's split", [[tutor], [one], [two]]))
    tmux("send-keys", "C-w", "t")
    frames.append(layout("restore tutor's split", initial))
    tmux("send-keys", "C-w", "l", "C-w", "t")
    frames.append(layout("transpose hello1's split", [[tutor, one, two]]))
    tmux("send-keys", "C-w", "t")
    frames.append(layout("restore hello1's split", initial))
    tmux("send-keys", "C-w", "h", "C-w", "o")
    command("hs")
    frames.append(layout("hs without arguments duplicates the document below", [[tutor], [tutor]]))
    tmux("send-keys", "C-w", "o")
    (workdir / "two words").write_text(two + "\n")
    command("hs hello1 'two words'")
    frames.append(layout("split multiple paths including a quoted filename", [[tutor], [one], [two]]))
    return frames
