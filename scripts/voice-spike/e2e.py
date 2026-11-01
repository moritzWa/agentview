#!/usr/bin/env python3
"""End to end: the real agentview dashboard in a PTY, a headless Chromium standing in for
the pinned voice tab (synthesized microphone, muted), and the real GPT-Live.

  python3 scripts/voice-spike/e2e.py "question to ask" [agentview binary]

Read-only against real sessions unless the question asks otherwise. Costs a few cents."""
import fcntl, os, pty, re, select, struct, subprocess, sys, tempfile, termios, time

# Each question is "text" or "text@seconds" (when it is spoken, 3 by default).
questions = [q for q in sys.argv[1:] if not q.startswith("/")] or ["Which of my sessions need my attention right now?"]
binary = next((q for q in sys.argv[1:] if q.startswith("/")), os.path.expanduser("~/Code/agentview-wt-voice/target/debug/agentview"))
RUN = int(os.environ.get("E2E_SECONDS", "85"))
here = os.path.dirname(os.path.abspath(__file__))
key = os.environ.get("VOICE_E2E_KEY") or subprocess.run(
    ["op", "item", "get", os.environ.get("VOICE_E2E_OP_ITEM", "arca agentview voice agent"), "--fields", "api key", "--reveal"],
    capture_output=True, text=True).stdout.strip()
assert key.startswith("sk-"), "no key from 1Password"

tmp = tempfile.mkdtemp(prefix="voice-e2e-")
inputs, filters, labels = [], [], []
for n, question in enumerate(questions):
    text, _, at = question.partition("@")
    ms = int(float(at or 3) * 1000)
    subprocess.run(["say", "-v", "Samantha", "-o", f"{tmp}/q{n}.aiff", text], check=True)
    inputs += ["-i", f"{tmp}/q{n}.aiff"]
    filters.append(f"[{n}]adelay={ms}|{ms}[a{n}]")
    labels.append(f"[a{n}]")
graph = ";".join(filters) + ";" + "".join(labels) + f"amix=inputs={len(questions)}:normalize=0,apad=pad_dur=90"
subprocess.run(["ffmpeg", "-y", "-loglevel", "error", *inputs, "-filter_complex", graph,
                "-ar", "48000", "-ac", "1", "-sample_fmt", "s16", f"{tmp}/mic.wav"], check=True)

env = dict(os.environ, AGENTVIEW_OPENAI_API_KEY=key, AGENTVIEW_VOICE_NO_BROWSER="1", TERM="xterm-256color")
pid, fd = pty.fork()
if pid == 0:
    os.execve(binary, [binary, "--opencode-bin", os.path.expanduser("~/.local/bin/opencode-dev"),
                       "--include-external", "--include-interactive", "--no-host-claude"], env)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 150, 0, 0))

screen = b""
def pump(seconds):
    global screen
    end = time.time() + seconds
    while time.time() < end:
        ready, _, _ = select.select([fd], [], [], 0.2)
        if ready:
            try:
                screen += os.read(fd, 65536)
            except OSError:
                return False
    return True

def footer_lines():
    text = re.sub(rb"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07", b"", screen).decode("utf8", "replace")
    return [l.strip() for l in text.splitlines() if "voice" in l and ("●" in l or "voice" in l.lower())][-3:]

pump(10)
print("dashboard up, bytes:", len(screen))
os.write(fd, b"\x0b")  # ctrl+k
pump(2)
driver = subprocess.Popen(
    ["python3", os.path.expanduser("~/.claude/scripts/heavy-job.py"), "--wait", "--", "node", f"{here}/page-driver.mjs", "47617", f"{tmp}/mic.wav", str(RUN - 8)],
    stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
start = time.time()
seen = set()
answered = False
while time.time() - start < RUN:
    pump(1)
    for line in footer_lines():
        if line not in seen:
            seen.add(line)
            print("FOOTER:", line[-150:])
    ready, _, _ = select.select([driver.stdout], [], [], 0)
    if ready:
        line = driver.stdout.readline()
        if line:
            print("PAGE:", line.rstrip()[:300])
            if "Agent View" in line and ("|" in line) and line.count("Agent View") >= 1 and time.time() - start > 20:
                answered = True
os.write(fd, b"\x0b")  # end the call
pump(3)
for line in footer_lines():
    if line not in seen:
        print("FOOTER:", line[-150:])
os.write(fd, b"\x1b")  # esc quits
pump(2)
driver.terminate()
try:
    os.kill(pid, 9)
except ProcessLookupError:
    pass
print("done; mic file", tmp)
