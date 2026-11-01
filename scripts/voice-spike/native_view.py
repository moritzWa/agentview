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
env = dict(os.environ, AGENTVIEW_OPENAI_API_KEY=key, AGENTVIEW_VOICE_PORT="47698", TERM=os.environ.get("NATIVE_TERM", "xterm-256color"), TERM_PROGRAM=os.environ.get("NATIVE_TERM_PROGRAM", "xterm"))
# Keep the call logs and perf trace of a test out of the real ones.
_scratch = os.path.join(os.environ.get("TMPDIR", "/tmp"), "voice-e2e-state")
os.makedirs(_scratch, exist_ok=True)
env["AGENTVIEW_VOICE_STATE_DIR"] = _scratch
env["AGENTVIEW_PERF_LOG"] = os.path.join(_scratch, "perf.log")
voice_key = os.environ.get("NATIVE_KEY", "\\x0b").encode().decode("unicode_escape").encode("latin-1")
live = bool(os.environ.get("NATIVE_LIVE"))
if live:
    # A real hidden browser with a silent fake microphone: the call really connects.
    env["AGENTVIEW_VOICE_CHROME_ARGS"] = "--no-sandbox --use-fake-device-for-media-stream --mute-audio"
else:
    env["AGENTVIEW_VOICE_NO_BROWSER"] = "1"
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
os.write(fd, voice_key); pump(1.5)           # the voice key
def badge_cells():
    out = []
    for n, line in enumerate(vt.display):
        c = line.find("voice")
        if c >= 0 and ("●" in line or "◌" in line):
            out.append((n, vt.buffer[n][max(c - 2, 0)]))
    return out
pump(1.5)
cells = badge_cells()
check("the key starts a call and a badge shows", bool(cells), str(cells[:1]))
check("the badge is blue text with no background", bool(cells) and cells[0][1].fg == "blue" and cells[0][1].bg == "default", f"{cells[0][1].fg}/{cells[0][1].bg}" if cells else "")
check("nothing is written on the bottom row", "voice" not in row() and "ended" not in row(), row())
pump(8 if live else 3)
where = [(n, line.strip()[:100]) for n, line in enumerate(vt.display) if "voice" in line]
check("a badge stays on screen while the call is on", bool(where), str(where))
if live:
    check("the call is live, not stuck connecting", any("voice 0:" in line or "voice 1:" in line for line in vt.display), str(where))
print("   bottom:", repr(row()[:140]))
print("   last rows:", [vt.display[n].rstrip()[-110:] for n in (-3, -2, -1)])
os.write(fd, voice_key); pump(1.5)
pump(1.5)
check("the key again ends it quietly", not badge_cells() and "ended" not in row(), f"{badge_cells()[:1]} {row()}")
os.write(fd, b"\x1b[1;2D"); pump(3)        # shift+left: back to the dashboard
pump(2.5)
check("the badge is gone after the call", "voice" not in row(), row())
check("shift+left returns to the dashboard", "agentview" in vt.display[0] or "agentview" in "\n".join(vt.display[:4]), vt.display[0][:80])
os.write(fd, b"\x1b"); pump(1)
try:
    os.kill(pid, 9)
except ProcessLookupError:
    pass
