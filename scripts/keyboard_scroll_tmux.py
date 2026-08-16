#!/usr/bin/env python3
"""Retain a populated interactive workspace for the keyboard-scroll walkthrough."""
import argparse
import json
from pathlib import Path
import shlex
import subprocess
import time

ROOT = Path(__file__).resolve().parent.parent


def tmux(*args, capture=False):
    return subprocess.run(["tmux", *args], check=True, text=True,
                          stdout=subprocess.PIPE if capture else None).stdout


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--session", required=True)
    parser.add_argument("--root", default=str(ROOT))
    args = parser.parse_args()
    root = Path(args.root).resolve()
    session = args.session
    output = Path(args.output).resolve()
    live = output / "live"
    config = live / "config/red"
    config.mkdir(parents=True, exist_ok=True)
    log = live / "red.log"
    (config / "config.toml").write_text(
        f"log_file = {json.dumps(str(log))}\nshow_whats_new=false\nfetch_release_notes=false\n"
        f"[lsp]\nenabled=false\n[agent]\ncommand={json.dumps(str(ROOT / 'scripts/workspace_scroll_bench.py'))}\n")
    command = shlex.join(["env", "RED_PERF=trace", "XDG_CONFIG_HOME=" + str(live / "config"),
                          str(Path(args.binary).resolve()), "--root", str(root), str(root / "src/editor.rs")])
    tmux("new-session", "-d", "-s", session, "-n", "workspace", "-x", "200", "-y", "60", "-c", str(root), command)
    target = session + ":workspace"

    def wait_for(text):
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            if log.exists() and text in log.read_text(errors="replace"):
                return
            time.sleep(0.1)
        raise TimeoutError(text)

    def send(text, pause=0.3):
        tmux("send-keys", "-t", target, "-l", text)
        time.sleep(pause)

    def cmd(text):
        send(":" + text)
        tmux("send-keys", "-t", target, "Enter")
        time.sleep(0.4)

    def capture(name):
        (output / name).write_text(tmux("capture-pane", "-p", "-e", "-t", target, capture=True))

    wait_for("[PERF] startup:interactive:")
    cmd("sp " + str(root / "src/editor/rendering.rs"))
    tmux("send-keys", "-t", target, "C-w", "k")
    cmd("Agent")
    send("Show the deterministic performance fixture")
    tmux("send-keys", "-t", target, "Escape")
    time.sleep(0.1)
    tmux("send-keys", "-t", target, "Enter")
    wait_for("notify agent:completed")
    time.sleep(0.3)
    tmux("send-keys", "-t", target, "Escape")
    send("\x1b[<0;0015;0008M\x1b[<0;0015;0008m")
    cmd("NeoTree")
    send("\x1b[<0;0065;0008M\x1b[<0;0065;0008m")
    cmd("1500")
    send("120j", 0.7)
    capture("tmux-before.txt")
    for _ in range(40):
        send("j", 0.025)
    capture("tmux-down.txt")
    send("120k", 0.7)
    for _ in range(40):
        send("k", 0.025)
    capture("tmux-up.txt")
    tmux("resize-window", "-t", target, "-x", "180", "-y", "50")
    time.sleep(0.5)
    tmux("resize-window", "-t", target, "-x", "200", "-y", "60")
    time.sleep(0.5)
    capture("tmux-restored.txt")
    tmux("has-session", "-t", session)
    print(json.dumps({"session": session, "target": target, "captures": 4}), flush=True)


if __name__ == "__main__":
    main()
