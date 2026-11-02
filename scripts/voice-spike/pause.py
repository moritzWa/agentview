#!/usr/bin/env ~/.cache/voice-e2e-venv/bin/python
"""Cmd+S (Ctrl+S) inside a session's view pauses the call, and the next voice key continues it.
The provider runs through a wrapper with OPENCODE_PURE=1 so the read-aloud plugin cannot start talking on the real Mac.
  pause.py "session title" [agentview binary]"""
import fcntl, os, pty, select, struct, subprocess, sys, tempfile, termios, time, json, stat
import pyte

title = sys.argv[1]
binary = sys.argv[2] if len(sys.argv) > 2 else os.path.expanduser("~/Code/agentview/target/debug/agentview")
scratch = tempfile.mkdtemp(prefix="voice-pause-")
log = os.path.join(scratch, "log.jsonl")
wrapper = os.path.join(scratch, "opencode-pure")
open(wrapper, "w").write('#!/bin/sh\nOPENCODE_PURE=1 exec "$HOME/.local/bin/opencode-dev" "$@"\n')
os.chmod(wrapper, 0o755)
env = dict(os.environ, TERM="xterm-256color", AGENTVIEW_VOICE_STATE_DIR=scratch, AGENTVIEW_PERF_LOG=os.path.join(scratch, "perf.log"),
           AGENTVIEW_VOICE_PORT="47695", AGENTVIEW_VOICE_LOG=log,
           AGENTVIEW_VOICE_CHROME_ARGS="--no-sandbox --use-fake-device-for-media-stream --mute-audio",
           AGENTVIEW_XAI_API_KEY=subprocess.run(["op", "item", "get", "x ai api key", "--fields", "credential", "--reveal"], capture_output=True, text=True).stdout.strip())
open(os.path.join(scratch, "settings.json"), "w").write('{"provider": "grok"}')
pid, fd = pty.fork()
if pid == 0:
    os.execve(binary, [binary, "--opencode-bin", wrapper, "--include-external", "--include-interactive", "--no-host-claude"], env)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
vt = pyte.Screen(120, 40); stream = pyte.ByteStream(vt)
def pump(seconds):
    end = time.time() + seconds
    while time.time() < end:
        if select.select([fd], [], [], 0.1)[0]:
            try: stream.feed(os.read(fd, 65536))
            except OSError: return
def badge():
    for line in vt.display:
        for mark in ("●", "◌", "⏸"):
            i = line.find(mark)
            if i >= 0 and "voice" in line[i:i + 20]:
                return line[i:i + 60].strip()
    return ""
def check(label, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + label + f"  [{detail[:90]}]")
def wait_for(mark, seconds):
    end = time.time() + seconds
    while time.time() < end:
        pump(0.3)
        if mark in badge(): return True
    return False
pump(15)
os.write(fd, b"\x06"); pump(0.5); os.write(fd, title.encode()); pump(0.5)
os.write(fd, b"\r"); pump(1.5); os.write(fd, b"\r"); pump(8)
os.write(fd, b"\x0b")
check("the call goes live in the session's view", wait_for("●", 12), badge())
pump(2)
os.write(fd, b"\x13")                      # Ctrl+S: Cmd+S in this terminal setup
pump(2)
check("Ctrl+S parks without an OpenCode paused badge", not badge(), badge())
pump(3)
os.write(fd, b"\x0b")                      # the voice key continues it
check("the voice key continues the call", wait_for("●", 12), badge())
starts = [json.loads(l) for l in open(log) if json.loads(l)["kind"] == "start"]
check("the second start is a resumed call", len(starts) == 2 and starts[-1]["d"].get("resumed") is True, str([s["d"] for s in starts]))
nowplaying = subprocess.run("ps -A -o command= | grep -c '[n]owplaying-'", shell=True, capture_output=True, text=True).stdout.strip()
print("   read-aloud helper processes running:", nowplaying)
os.write(fd, b"\x0b"); pump(2)
for line in subprocess.run(["ps", "-A", "-o", "pid=,command="], capture_output=True, text=True).stdout.splitlines():
    if scratch in line and "--headless" in line: os.kill(int(line.split()[0]), 15)
os.kill(pid, 9)
