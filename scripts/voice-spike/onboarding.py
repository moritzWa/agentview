#!/usr/bin/env ~/.cache/voice-e2e-venv/bin/python  (needs pyte)
"""First run in a PTY with no key saved: the footer hint, the masked key prompt, a wrong key,
then the real one, which starts a call. Real dashboard, real OpenAI key check, no browser."""
import fcntl, os, pty, re, select, shutil, stat, struct, subprocess, sys, tempfile, termios, time
import pyte

binary = sys.argv[1] if len(sys.argv) > 1 else os.path.expanduser("~/Code/agentview-wt-voice/target/debug/agentview")
key = os.environ.get("VOICE_E2E_KEY") or subprocess.run(
    ["op", "item", "get", os.environ.get("VOICE_E2E_OP_ITEM", "arca agentview voice agent"), "--fields", "api key", "--reveal"],
    capture_output=True, text=True).stdout.strip()
assert key.startswith("sk-")
state = tempfile.mkdtemp(prefix="voice-onboarding-")
env = {k: v for k, v in os.environ.items() if "OPENAI" not in k}
env.update(AGENTVIEW_VOICE_STATE_DIR=state, AGENTVIEW_VOICE_NO_BROWSER="1", TERM="xterm-256color")
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
        r, _, _ = select.select([fd], [], [], 0.2)
        if r:
            stream.feed(os.read(fd, 65536))
def text():
    return "\n".join(vt.display)
def check(label, ok):
    print(("PASS " if ok else "FAIL ") + label)
    return ok

pump(12)
print("FOOTER:", vt.display[-1].strip())
check("footer hints at ctrl+k on a fresh install", "ctrl+k talk to your agents" in text())
os.write(fd, b"\x0b"); pump(1.5)
check("ctrl+k without a key opens the key prompt", "OpenAI API key" in text())
os.write(fd, b"hello\r"); pump(1.5)
check("a wrong key is refused without calling OpenAI", "does not look like an OpenAI API key" in text())
os.write(fd, key.encode()); pump(1)
shown = text()
check("the key is not shown on screen", key[:12] not in shown and key[-8:] not in shown and "****" in shown)
os.write(fd, b"\r"); pump(6)
body = text()
check("the saved key starts a call", "● voice" in body)
saved = os.path.join(state, "key.json")
check("the key is saved, owner-only", os.path.exists(saved) and stat.S_IMODE(os.stat(saved).st_mode) == 0o600)
check("the hint is marked as seen", os.path.exists(os.path.join(state, "hint-seen")))
os.write(fd, b"\x0b"); pump(2)
check("ctrl+k ends the call", "ended the call" in text().lower() or "voice call ended" in text().lower())
os.write(fd, b"\x1b"); pump(1)
try:
    os.kill(pid, 9)
except ProcessLookupError:
    pass
shutil.rmtree(state, ignore_errors=True)
