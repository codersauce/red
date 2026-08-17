#!/usr/bin/env python3
"""Exercise Red's real Crossterm reader in a Unix PTY, without starting an agent."""

import argparse
import errno
import os
from pathlib import Path
import pty
import select
import signal
import subprocess
import time


ROOT = Path(__file__).resolve().parent.parent


def run_case(binary, name, protocol, chunks, expected, reply=None):
    master, slave = pty.openpty()
    process = subprocess.Popen(
        [str(binary), "keys", "--protocol", protocol, "--count", "1"],
        stdin=slave, stdout=slave, stderr=slave,
        env={**os.environ, "TERM": "xterm-256color"},
        start_new_session=True,
    )
    os.close(slave)
    output = bytearray()
    sent_reply = False
    sent_keys = False
    deadline = time.monotonic() + 8
    try:
        while time.monotonic() < deadline:
            if reply and not sent_reply and b"\x1b[?u\x1b[c" in output:
                os.write(master, reply)
                sent_reply = True
            if not sent_keys and b"Text is not recorded." in output:
                for chunk in chunks:
                    os.write(master, chunk)
                    if len(chunks) > 1:
                        time.sleep(0.01)
                sent_keys = True
            if select.select([master], [], [], 0.05)[0]:
                try:
                    data = os.read(master, 65536)
                except OSError as error:
                    if error.errno == errno.EIO:
                        break
                    raise
                if not data:
                    break
                output.extend(data)
            elif process.poll() is not None:
                break
        if process.poll() is None:
            process.wait(timeout=1)
        text = output.decode("utf-8", errors="replace")
        assert process.returncode == 0, (name, process.returncode, text)
        assert expected in text, (name, expected, text)
        if protocol == "kitty" or (protocol == "auto" and reply and b"[?1u" in reply):
            assert b"\x1b[>1u" in output and b"\x1b[<1u" in output, (name, text)
        if protocol == "xterm":
            assert b"\x1b[>4;2m" in output and b"\x1b[>4m" in output, (name, text)
        print(f"PASS {name}")
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            process.wait(timeout=3)
        os.close(master)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/red")
    args = parser.parse_args()
    binary = args.binary.resolve()
    cases = [
        ("plain Enter", "legacy", [b"\r"], "code=Enter modifiers=KeyModifiers(0x0)", None),
        ("legacy Ctrl+J", "legacy", [b"\n"], "code=Ctrl+J modifiers=KeyModifiers(CONTROL)", None),
        ("legacy Alt+Enter", "legacy", [b"\x1b\r"], "code=Enter modifiers=KeyModifiers(ALT)", None),
        ("legacy Alt+/", "legacy", [b"\x1b/"], "code=other key (hidden) modifiers=KeyModifiers(ALT)", None),
        ("CSI-u Alt+/", "kitty", [b"\x1b[47;3u"], "code=other key (hidden) modifiers=KeyModifiers(ALT)", None),
        ("xterm Alt+/", "xterm", [b"\x1b[27;3;47~"], "code=other key (hidden) modifiers=KeyModifiers(ALT)", None),
        ("CSI-u Shift+Enter", "kitty", [b"\x1b[13;2u"], "code=Enter modifiers=KeyModifiers(SHIFT)", None),
        ("CSI-u Alt+Enter", "kitty", [b"\x1b[13;3u"], "code=Enter modifiers=KeyModifiers(ALT)", None),
        ("CSI-u Ctrl+Enter", "kitty", [b"\x1b[13;5u"], "code=Enter modifiers=KeyModifiers(CONTROL)", None),
        ("CSI-u Ctrl+J", "kitty", [b"\x1b[106;5u"], "code=Ctrl+J modifiers=KeyModifiers(CONTROL)", None),
        ("xterm Shift+Enter", "xterm", [b"\x1b[27;2;13~"], "code=Enter modifiers=KeyModifiers(SHIFT)", None),
        ("xterm Alt+Enter", "xterm", [b"\x1b[27;3;13~"], "code=Enter modifiers=KeyModifiers(ALT)", None),
        ("xterm Ctrl+Enter", "xterm", [b"\x1b[27;5;13~"], "code=Enter modifiers=KeyModifiers(CONTROL)", None),
        ("fragmented CSI-u", "legacy", [b"\x1b[13;", b"3u"], "code=Enter modifiers=KeyModifiers(ALT)", None),
        ("fragmented xterm", "legacy", [b"\x1b[27;", b"2;13~"], "code=Enter modifiers=KeyModifiers(SHIFT)", None),
        ("key repeat", "kitty", [b"\x1b[13;1:2u"], "kind=Repeat", None),
        ("key release", "kitty", [b"\x1b[13;1:3u"], "kind=Release", None),
        ("automatic Kitty negotiation", "auto", [b"\r"], "code=Enter", b"\x1b[?1u\x1b[?1;2c"),
        ("Kitty reply without device attributes", "auto", [b"\r"], "code=Enter", b"\x1b[?1u"),
        ("automatic xterm fallback", "auto", [b"\x1b[27;3;13~"], "code=Enter modifiers=KeyModifiers(ALT)", b"\x1b[?1;2c"),
    ]
    for case in cases:
        run_case(binary, *case)
    print(f"{len(cases)} keyboard protocol cases passed")


if __name__ == "__main__":
    main()
