#!/usr/bin/env python3
"""Isolated PTY layout smoke test. Requires pyte; run in the macOS test Space."""
import codecs
import fcntl
import json
import os
import pathlib
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

import pyte


def test(binary, width):
    with tempfile.TemporaryDirectory(prefix="agentview-scheduled-layout-") as temp:
        home = pathlib.Path(temp)
        state = home / "state/agentview"
        state.mkdir(parents=True, mode=0o700)
        (state / "pinned-sessions.json").write_text(json.dumps({
            "version": 1, "sessions": [{"id": "opencode:host:paused", "pinned_at_ms": 1}]}))
        def row(name, status="completed"):
            return dict(id=f"opencode:host:{name}", provider_session_id=name,
                        provider="opencode", runtime={"kind": "host"}, kind="managed",
                        name=name, cwd="/work/layout", state=status, summary="Shared preview text",
                        raw_state="managed_server", pid=None, started_at=None, updated_at=None,
                        pull_requests=None, capabilities=[])
        reminder = row("reminder-row")
        reminder["wake"] = dict(label="Mon 10:00", text="Check replies", due_ms=9999999999999, deferred_at_ms=None)
        blocked = row("blocked-row", "scheduled")
        blocked["raw_state"] = "background: wake-later Tue 10:00: Check blocking reply"
        fixture = home / "fixture.json"
        fixture.write_text(json.dumps({"sessions": [row("plain-row"), reminder, blocked, row("paused")]}))
        env = dict(os.environ, HOME=temp, XDG_STATE_HOME=str(home / "state"),
                   XDG_CONFIG_HOME=str(home / "config"), XDG_DATA_HOME=str(home / "data"),
                   TERM="xterm-256color", COLORTERM="truecolor")
        env.pop("OPENROUTER_API_KEY", None)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, width, 0, 0))
        process = subprocess.Popen([binary, "--fixture", str(fixture), "--no-host-providers", "--theme", "light"],
                                   env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        screen = pyte.Screen(width, 30)
        stream = pyte.Stream(screen)
        decoder = codecs.getincrementaldecoder("utf-8")("replace")
        def wait(check):
            deadline = time.monotonic() + 12
            while time.monotonic() < deadline:
                if check():
                    return
                if select.select([master], [], [], 0.1)[0]:
                    stream.feed(decoder.decode(os.read(master, 65536)))
            raise AssertionError("\n".join(screen.display))
        def has(text):
            return any(text in line for line in screen.display)
        try:
            wait(lambda: has("plain-row") and has("Scheduled 1") and has("Paused 1"))
            os.write(master, b"\x13")
            wait(lambda: has("/work/layout") and has("Done"))
            lines = screen.display
            find = lambda text: next(i for i, line in enumerate(lines) if text in line)
            assert find("Paused 1") < find("Scheduled 1") < find("/work/layout")
            assert not has("blocked-row"), "Scheduled starts collapsed"
            plain = lines[find("plain-row")]
            reminder_line = lines[find("reminder-row")]
            assert plain.index("Done") == reminder_line.index("Done"), (plain, reminder_line)
            assert plain.index("Shared") == reminder_line.index("Shared"), (plain, reminder_line)
            slot = reminder_line.index("↻")
            assert plain[slot:slot + 14].isspace(), (plain, reminder_line)
            assert reminder_line[slot - 1] == " ", "reminder has a clear gap"
            os.write(master, b"\x19")
            wait(lambda: has("blocked-row"))
        finally:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=5)
            os.close(master)


if __name__ == "__main__":
    for columns in (85, 120):
        test(str(pathlib.Path(sys.argv[1]).resolve()), columns)
    print("PASS: aligned status/preview columns and blank reminder slots at 85/120 columns; Paused then Scheduled then projects; Scheduled expands.")
