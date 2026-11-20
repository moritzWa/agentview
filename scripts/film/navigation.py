"""Record real right-arrow open / double-left return on one safe OpenCode session.

Requires a pre-created tools-denied synthetic session in live-demo-session.json.
No prompts are sent and no account/connector settings are changed here.
"""

import argparse
import fcntl
import json
import os
import pty
import select
import signal
import struct
import subprocess
import termios
import time
from pathlib import Path

import pyte
from capture import COLS, picture


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", type=Path, required=True)
    args = ap.parse_args()
    target = json.loads((args.output / "live-demo-session.json").read_text())
    root = args.output / "navigation"
    root.mkdir(exist_ok=True)
    master, slave = pty.openpty()
    rows = 28
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, COLS, 0, 0))
    env = dict(
        os.environ,
        TERM="xterm-256color",
        COLORTERM="truecolor",
        OPENCODE_DISABLE_FFF="1",
    )
    binary = Path.home() / ".local/share/opencode-v2/bin/native/agentview"
    proc = subprocess.Popen(
        [
            str(binary),
            "--theme",
            "light",
            "--cwd",
            target["workspace"],
            "--include-external",
            "--harness",
            "opencode",
            "--opencode-bin",
            str(Path.home() / ".local/bin/opencode-dev"),
            "--no-host-claude",
            "--no-host-codex",
            "--no-host-pi",
            "--no-host-copilot",
        ],
        stdin=slave,
        stdout=slave,
        stderr=slave,
        env=env,
        cwd=target["workspace"],
        start_new_session=True,
    )
    os.close(slave)
    screen = pyte.Screen(COLS, rows)
    stream = pyte.ByteStream(screen)
    frames = []

    def record(seconds, label):
        end = time.monotonic() + seconds
        next_frame = 0
        while time.monotonic() < end:
            if select.select([master], [], [], 0.03)[0]:
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    break
                stream.feed(chunk)
                if b"\x1b[6n" in chunk:
                    os.write(master, b"\x1b[1;1R")
            if time.monotonic() >= next_frame:
                next_frame = time.monotonic() + 0.1
                path = root / f"{len(frames):04d}.png"
                picture(screen, path)
                frames.append({"file": path.name, "phase": label})

    try:
        record(8, "warmup")
        display = "\n".join(screen.display)
        assert "synthetic checkout demo" in display, display
        picture(screen, root / "dashboard.png")
        os.write(master, b"\x1b[C")
        record(5, "open")
        display = "\n".join(screen.display)
        assert "coupon" in display.lower(), display
        picture(screen, root / "opened.png")
        os.write(master, b"\x1b[D")
        record(3, "return")
        picture(screen, root / "returned.png")
        assert "backgrounded OpenCode session" in "\n".join(
            screen.display
        ) or "enter to open" in "\n".join(screen.display), "Did not return to dashboard"
        (root / "recording.json").write_text(
            json.dumps(
                {
                    "target": target["id"],
                    "actual_app": True,
                    "actual_native_open": True,
                    "input": ["Right", "Left"],
                    "fps_approx": 10,
                    "frames": frames,
                },
                indent=2,
            )
        )
    finally:
        os.close(master)
        if proc.poll() is None:
            os.killpg(proc.pid, signal.SIGTERM)
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait()


if __name__ == "__main__":
    main()
