#!/usr/bin/env ~/.cache/voice-e2e-venv/bin/python
"""How fast a call starts and goes quiet. Presses the voice key on the real dashboard in a PTY, ends the call,
starts another, and reads the timings the call log keeps: `live` (ms from the key to a live voice) and
`silenced` (ms from the end key until the page cut the voice). Quiet fake microphone, muted browser.
  latency.py [agentview binary]    E2E_PROVIDER=grok runs it on xAI"""
import fcntl, os, pty, select, struct, subprocess, sys, tempfile, termios, time, json

binary = sys.argv[1] if len(sys.argv) > 1 else os.path.expanduser("~/Code/agentview/target/debug/agentview")
grok = os.environ.get("E2E_PROVIDER", "").lower() in ("grok", "xai")
scratch = tempfile.mkdtemp(prefix="voice-latency-")
log = os.path.join(scratch, "log.jsonl")
env = dict(os.environ, TERM="xterm-256color", AGENTVIEW_VOICE_STATE_DIR=scratch, AGENTVIEW_PERF_LOG=os.path.join(scratch, "perf.log"),
           AGENTVIEW_VOICE_PORT="47697", AGENTVIEW_VOICE_LOG=log,
           AGENTVIEW_VOICE_CHROME_ARGS="--no-sandbox --use-fake-device-for-media-stream --mute-audio")
if grok:
    env["AGENTVIEW_XAI_API_KEY"] = subprocess.run(["op", "item", "get", "x ai api key", "--fields", "credential", "--reveal"], capture_output=True, text=True).stdout.strip()
    open(os.path.join(scratch, "settings.json"), "w").write('{"provider": "grok"}')
else:
    env["AGENTVIEW_OPENAI_API_KEY"] = subprocess.run(["op", "item", "get", "arca agentview voice agent", "--fields", "api key", "--reveal"], capture_output=True, text=True).stdout.strip()
pid, fd = pty.fork()
if pid == 0:
    os.execve(binary, [binary, "--opencode-bin", os.path.expanduser("~/.local/bin/opencode-dev"), "--include-external", "--include-interactive", "--no-host-claude"], env)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 150, 0, 0))

def pump(seconds):
    end = time.time() + seconds
    while time.time() < end:
        if select.select([fd], [], [], 0.1)[0]:
            try: os.read(fd, 65536)
            except OSError: return

def events(kind):
    try:
        return [json.loads(l) for l in open(log) if json.loads(l)["kind"] == kind]
    except FileNotFoundError:
        return []

pump(int(os.environ.get("E2E_WAIT", "12")))
for call in ("cold start", "warm start"):
    os.write(fd, b"\x0b"); pump(10)
    os.write(fd, b"\x0b"); pump(3)
    live, silenced = events("live"), events("silenced")
    print(f"{call}: live after {live[-1]['d']['ms']} ms" if live else f"{call}: never live", end="")
    print(f", silenced {silenced[-1]['d']['ms_after_the_key']} ms after the end key" if silenced else ", no silence reported")
os.kill(pid, 9)
# A browser kept for the next call outlives a killed agentview: stop this run's.
for line in subprocess.run(["ps", "-A", "-o", "pid=,command="], capture_output=True, text=True).stdout.splitlines():
    if scratch in line and "--headless" in line:
        os.kill(int(line.split()[0]), 15)
