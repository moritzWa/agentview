#!/usr/bin/env python3
"""Headless PTY smoke test. Pass a test binary and the wake-later helper.

Run in the macOS test Space. Uses a disposable home, never real session state.
"""
import fcntl
import json
import os
import pathlib
import pty
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time


def main():
    binary, helper = map(lambda value: pathlib.Path(value).resolve(), sys.argv[1:3])
    with tempfile.TemporaryDirectory(prefix="agentview-reminder-ui-") as temp:
        home = pathlib.Path(temp)
        local_helper = home / ".claude/scripts/wake-later.py"
        local_helper.parent.mkdir(parents=True)
        shutil.copyfile(helper, local_helper)
        env = dict(os.environ, HOME=temp, XDG_STATE_HOME=str(home / "state"),
                   XDG_CONFIG_HOME=str(home / "config"), XDG_DATA_HOME=str(home / "data"),
                   TERM="xterm-256color", COLORTERM="truecolor")
        env.pop("OPENROUTER_API_KEY", None)
        session_id = "ses_outreach_ui_test"
        subprocess.run([sys.executable, str(helper), "add", session_id, temp, "+2h", "Check receipt outcomes"],
                       env=env, check=True, capture_output=True)

        def row(sid, name, state):
            return dict(id=f"opencode:host:{sid}", provider_session_id=sid,
                        provider="opencode", runtime={"kind": "host"}, kind="managed",
                        name=name, cwd="/work/data-leads", state=state, summary="VC and lawyer introductions",
                        raw_state="managed_server", pid=None, started_at=None, updated_at=None,
                        pull_requests=None, capabilities=[])

        active = row(session_id, "eu-biotech-outreach", "completed")
        active["wake"] = dict(label="Mon 10:00", text="Check receipts", due_ms=9999999999999, deferred_at_ms=None)
        blocked = row("ses_blocked_ui_test", "fully-blocked-outreach", "scheduled")
        blocked["raw_state"] = "background: wake-later Tue 10:00: Check blocking reply"
        fixture = home / "fixture.json"
        fixture.write_text(json.dumps({"sessions": [active, blocked]}))
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 35, 140, 0, 0))
        process = subprocess.Popen([str(binary), "--fixture", str(fixture), "--no-host-providers",
                                    "--theme", "light", "--refresh-ms", "500"],
                                   env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        captured = bytearray()

        def read_until(check, label, timeout=12):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                if check():
                    return
                readable, _, _ = select.select([master], [], [], 0.1)
                if readable:
                    try:
                        data = os.read(master, 65536)
                    except OSError:
                        break
                    if not data:
                        break
                    captured.extend(data)
            raise AssertionError(f"timed out: {label}; output: {captured.decode(errors='replace')[-4000:]}")

        def reminders():
            return json.loads((home / ".local/state/wake-later/reminders.json").read_text())

        def command(text):
            os.write(master, text.encode() + b"\r")

        try:
            read_until(lambda: b"eu-biotech-outreach" in captured and b"Mon 10:00" in captured, "normal reminder row")
            assert b"fully-blocked-outreach" not in captured, "deferred row starts collapsed"
            os.write(master, b"\x13")  # Directory view shows the per-row status.
            read_until(lambda: b"Done" in captured, "follow-up retains Done")
            assert b"38;2;0;120;40" in captured, "new light-mode Done green"
            command("/defer")
            read_until(lambda: reminders()[0].get("deferred_at_ms") is not None, "defer selected session")
            # Wait for the helper's completion notice before sending another command.
            read_until(lambda: b"reminder." in captured, "defer completion")
            command("/keep")
            read_until(lambda: reminders()[0].get("deferred_at_ms") is None, "keep without cancelling")
            read_until(lambda: b"unchanged." in captured, "keep completion")
            assert reminders()[0]["status"] == "pending"
            command("/defer +2h Check the blocking reply")
            read_until(lambda: len(reminders()) == 2, "create a deferred wake")
            assert reminders()[1]["text"] == "Check the blocking reply"
            assert reminders()[1]["deferred_at_ms"] is not None
            os.write(master, b"\x19")
            read_until(lambda: b"fully-blocked-outreach" in captured, "expand Scheduled")
            print("PASS: visible reminder, collapsed defer, both defer commands, keep without cancellation, brighter Done green.")
        finally:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=5)
            os.close(master)


if __name__ == "__main__":
    main()
