#!/usr/bin/env ~/.cache/voice-e2e-venv/bin/python
"""End to end: the real agentview dashboard in a PTY and the real GPT-Live, with a synthesized,
muted microphone. Needs pyte (a venv at ~/.cache/voice-e2e-venv) and, for the page driver,
playwright (VOICE_E2E_MODULES_FROM).

  e2e.py [--driver] "question@seconds" ["another@seconds" ...] [/path/to/agentview]

By default agentview starts its own hidden Chrome, given the fake microphone through
AGENTVIEW_VOICE_CHROME_ARGS. With --driver a Playwright page stands in for a pinned tab.
Read-only against real sessions unless a question asks otherwise. Costs a few cents."""
import fcntl, os, pty, select, struct, subprocess, sys, tempfile, termios, time
import pyte

args = sys.argv[1:]
use_driver = "--driver" in args
# E2E_STALE_TAB=1: a visible tab left open from an earlier version is connected while agentview runs its own browser.
stale_tab = bool(os.environ.get("E2E_STALE_TAB"))
questions = [a for a in args if not a.startswith("/") and not a.startswith("--")] or ["Which of my sessions need my attention right now?"]
binary = next((a for a in args if a.startswith("/")), os.path.expanduser("~/Code/agentview/target/debug/agentview"))
RUN = int(os.environ.get("E2E_SECONDS", "85"))
here = os.path.dirname(os.path.abspath(__file__))
key = os.environ.get("VOICE_E2E_KEY") or subprocess.run(
    ["op", "item", "get", os.environ.get("VOICE_E2E_OP_ITEM", "arca agentview voice agent"), "--fields", "api key", "--reveal"],
    capture_output=True, text=True).stdout.strip()
assert key.startswith("sk-"), "no key"

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

env = dict(os.environ, AGENTVIEW_OPENAI_API_KEY=key, TERM="xterm-256color")
if use_driver:
    env["AGENTVIEW_VOICE_NO_BROWSER"] = "1"
else:
    env.pop("AGENTVIEW_VOICE_NO_BROWSER", None)
    # E2E_REAL_MIC=1: no fake microphone, as on a real machine.
    if not os.environ.get("E2E_REAL_MIC"):
      env["AGENTVIEW_VOICE_CHROME_ARGS"] = (
        f"--no-sandbox --use-fake-device-for-media-stream --use-file-for-fake-audio-capture={tmp}/mic.wav%noloop --mute-audio")
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
        ready, _, _ = select.select([fd], [], [], 0.2)
        if ready:
            try:
                stream.feed(os.read(fd, 65536))
            except OSError:
                return

def footer():
    return vt.display[-1].strip()

def chrome_processes():
    out = subprocess.run(["ps", "-A", "-o", "pid=,command="], capture_output=True, text=True).stdout
    return [l for l in out.splitlines() if "voice/browser-profile" in l or "browser-profile" in l and "headless" in l]

pump(10)
for _ in range(int(os.environ.get("E2E_SELECT_DOWN", "0"))):
    os.write(fd, b"\x1b[B")
    pump(0.3)
print("dashboard up; footer:", footer()[:120])
before = len(chrome_processes())
os.write(fd, b"\x0b")  # ctrl+k
driver = None
if use_driver or stale_tab:
    pump(2)
    driver = subprocess.Popen(
        ["python3", os.path.expanduser("~/.claude/scripts/heavy-job.py"), "--wait", "--", "node", f"{here}/page-driver.mjs", "47617", f"{tmp}/mic.wav", str(RUN - 8)],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
start = time.time()
last = ""
peak = 0
while time.time() - start < RUN:
    pump(1)
    peak = max(peak, len(chrome_processes()))
    line = footer()
    if line != last and ("voice" in line or "you:" in line or "agent:" in line):
        print(f"{time.time() - start:5.1f}s FOOTER:", line[:170])
        last = line
    if driver and select.select([driver.stdout], [], [], 0)[0]:
        out = driver.stdout.readline()
        if out:
            print("PAGE:", out.rstrip()[:200])
print("hidden browser processes while the call ran:", peak, "(before:", before, ")")
os.write(fd, b"\x0b")  # end the call
pump(3)
print("after ctrl+k:", footer()[:150])
time.sleep(2.5)
print("hidden browser processes after the call:", len(chrome_processes()))
os.write(fd, b"\x1b")
pump(1)
if driver:
    driver.terminate()
try:
    os.kill(pid, 9)
except ProcessLookupError:
    pass
print("done; mic file", tmp)
