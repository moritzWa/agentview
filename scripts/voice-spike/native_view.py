#!/usr/bin/env ~/.cache/voice-e2e-venv/bin/python
"""The voice key inside a session's own view: open a session from the real dashboard in a PTY,
press ctrl+k, and look for the message on the bottom row. No browser, no audio.
  native_view.py "text of the session title to open" [agentview binary]"""
import fcntl, os, pty, select, struct, subprocess, sys, termios, time
import pyte

title = sys.argv[1]
binary = sys.argv[2] if len(sys.argv) > 2 else os.path.expanduser("~/Code/agentview/target/debug/agentview")
key = os.environ.get("VOICE_E2E_KEY") or subprocess.run(
    ["op", "item", "get", os.environ.get("VOICE_E2E_OP_ITEM", "arca agentview voice agent"), "--fields", "api key", "--reveal"],
    capture_output=True, text=True).stdout.strip()
env = dict(os.environ, AGENTVIEW_OPENAI_API_KEY=key, AGENTVIEW_VOICE_NO_BROWSER="1", AGENTVIEW_VOICE_PORT="47698", TERM="xterm-256color")
pid, fd = pty.fork()
if pid == 0:
    os.execve(binary, [binary, "--opencode-bin", os.path.expanduser("~/.local/bin/opencode-dev"),
                       "--include-external", "--include-interactive", "--no-host-claude"], env)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 150, 0, 0))
vt = pyte.Screen(150, 40)
stream = pyte.ByteStream(vt)

def pump(seconds):
    end = time.time() + seconds
    while time.time() < end:
        if select.select([fd], [], [], 0.2)[0]:
            try:
                stream.feed(os.read(fd, 65536))
            except OSError:
                return

def row(n=-1):
    return vt.display[n].strip()

def check(label, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + label + f"  [{(detail or row())[:100]}]")

pump(12)
os.write(fd, b"\x06"); pump(0.5)           # ctrl+f: filter
os.write(fd, title.encode()); pump(0.5)
os.write(fd, b"\r"); pump(1.5)
os.write(fd, b"\r"); pump(8)               # enter: open the session's own view
screen = "\n".join(vt.display)
check("the session's own view is showing", "agentview v0.2.0" not in screen.split("\n")[0], screen.split("\n")[0][:80])
os.write(fd, b"\x0b"); pump(1.5)           # ctrl+k
check("ctrl+k in the view starts a call", "voice" in row(), row())
pump(4)
os.write(fd, b"\x0b"); pump(1.5)
check("ctrl+k again ends it", "ended the call" in row() or "voice ended" in row(), row())
os.write(fd, b"\x1b[1;2D"); pump(3)        # shift+left: back to the dashboard
check("shift+left returns to the dashboard", "agentview" in vt.display[0] or "agentview" in "\n".join(vt.display[:4]), vt.display[0][:80])
os.write(fd, b"\x1b"); pump(1)
try:
    os.kill(pid, 9)
except ProcessLookupError:
    pass
