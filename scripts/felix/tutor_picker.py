"""Tutor 13.7: choose a file in a split, then keep only the focused view."""

import re
import time


def run_picker_split_trace(tmux, workdir):
    (workdir / "tutor").write_text("VERTICAL TARGET\n")
    (workdir / "lesson").write_text("HORIZONTAL TARGET\n")
    checkpoints = []

    def wait_for(label, matches):
        deadline = time.monotonic() + 15
        while True:
            frame = tmux("capture-pane", "-p", "-t", "oracle:0.0").stdout
            rows = frame.splitlines()
            if matches(rows):
                checkpoints.append({"step": label, "frame": rows})
                return
            if time.monotonic() >= deadline:
                raise RuntimeError(f"{label}: unexpected picker or document layout\n{frame}")
            time.sleep(.02)

    def positions(rows, text):
        # The document gutter prints line one before the marker in each window.
        return [(row, match.start()) for row, line in enumerate(rows)
                for match in re.finditer(r"\b1 +" + re.escape(text), line)]

    def split_visible(rows, source, dest, key):
        before, after = positions(rows, source), positions(rows, dest)
        if len(before) != 1 or len(after) != 1:
            return False
        old_row, old_col = before[0]
        new_row, new_col = after[0]
        if key == "C-v":
            return old_col < 40 <= new_col
        return old_row < new_row and max(old_col, new_col) < 40

    source = "foo foo"
    wait_for("open original", lambda rows: positions(rows, source))
    for key, filename, dest in [
        ("C-v", "tutor", "VERTICAL TARGET"),
        ("C-s", "lesson", "HORIZONTAL TARGET"),
    ]:
        tmux("send-keys", "Space", "f")
        tmux("send-keys", "-l", filename)
        def ready(rows):
            dialog = [row for row in rows if "│" in row]
            other = "lesson" if filename == "tutor" else "tutor"
            return (sum(filename in row for row in dialog) >= 2
                    and not any(other in row or "Searching..." in row for row in dialog))

        # The filename must appear in both the input and the filtered results.
        wait_for(f"pick {filename}", ready)
        tmux("send-keys", key)
        wait_for(f"{key} opens {filename}", lambda rows: split_visible(rows, source, dest, key))
        tmux("send-keys", "C-w", "o")
        wait_for(f"keep focused {filename}", lambda rows: not positions(rows, source)
                 and len(positions(rows, dest)) == 1 and positions(rows, dest)[0][1] < 40)
        source = dest
    return checkpoints
