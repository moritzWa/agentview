#!/usr/bin/env ~/.cache/voice-e2e-venv/bin/python
"""A draft typed into OpenCode's input box reaches the call. Opens a session's own view in a PTY, types a short
draft (nothing is submitted), presses the voice key, and reads the call log: its notes must carry the draft.
  draft.py "session title" [agentview binary]"""
import fcntl, os, pty, select, struct, subprocess, sys, tempfile, termios, time, json
import pyte

title = sys.argv[1]
binary = sys.argv[2] if len(sys.argv) > 2 else os.path.expanduser("~/Code/agentview/target/debug/agentview")
scratch = tempfile.mkdtemp(prefix="voice-draft-")
log = os.path.join(scratch, "log.jsonl")
# SAY="..." makes the fake microphone say it, in a real hidden browser, so the whole flow runs.
say = os.environ.get("SAY")
wav = None
if say:
    wav = os.path.join(scratch, "mic.wav")
    subprocess.run(["say", "-v", "Samantha", "-o", wav + ".aiff", say], check=True)
    subprocess.run(["python3", os.path.expanduser("~/.claude/scripts/heavy-job.py"), "--wait", "--", "ffmpeg", "-y", "-loglevel", "error", "-i", wav + ".aiff", "-ar", "48000", "-ac", "1", "-sample_fmt", "s16", wav], check=True)
env = dict(os.environ, TERM="xterm-256color", AGENTVIEW_VOICE_STATE_DIR=scratch, AGENTVIEW_PERF_LOG=os.path.join(scratch, "perf.log"),
           AGENTVIEW_VOICE_PORT="47696", AGENTVIEW_VOICE_LOG=log,
           AGENTVIEW_XAI_API_KEY=subprocess.run(["op", "item", "get", "x ai api key", "--fields", "credential", "--reveal"], capture_output=True, text=True).stdout.strip())
open(os.path.join(scratch, "settings.json"), "w").write('{"provider": "grok"}')
if wav:
    env["AGENTVIEW_VOICE_CHROME_ARGS"] = f"--no-sandbox --use-fake-device-for-media-stream --use-file-for-fake-audio-capture={wav}%noloop --mute-audio"
else:
    env["AGENTVIEW_VOICE_NO_BROWSER"] = "1"
pid, fd = pty.fork()
if pid == 0:
    os.execve(binary, [binary, "--opencode-bin", os.path.expanduser("~/.local/bin/opencode-dev"), "--include-external", "--include-interactive", "--no-host-claude"], env)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
vt = pyte.Screen(120, 40); stream = pyte.ByteStream(vt)
def pump(seconds):
    end = time.time() + seconds
    while time.time() < end:
        if select.select([fd], [], [], 0.2)[0]:
            try: stream.feed(os.read(fd, 65536))
            except OSError: return
def draft_notes():
    try:
        return [json.loads(l)["d"]["notes"] for l in open(log) if json.loads(l)["kind"] == "draft"]
    except FileNotFoundError:
        return []
pump(15)
os.write(fd, b"\x06"); pump(0.5); os.write(fd, title.encode()); pump(0.5)
os.write(fd, b"\r"); pump(1.5); os.write(fd, b"\r"); pump(8)
typed = os.environ.get("DRAFT", "let's rename the voice settings and keep the old names working")
wire = typed.replace("\\n", "\x1b\r")
os.write(fd, wire.encode()); pump(2)
os.write(fd, b"\x0b"); pump(3)
found = draft_notes()
ok = bool(found) and all(part in found[-1] for part in typed.split("\\n"))
print(("PASS" if ok else "FAIL"), "the call's notes carry the typed draft")
if found: print("   ", found[-1][-260:].replace("\n", " | "))
if wav:
    pump(25)
    tools = [json.loads(l) for l in open(log) if json.loads(l)["kind"] == "tool"]
    sent = [t for t in tools if t["d"]["name"] == "send_draft"]
    print(("PASS" if sent and '"ok":true' in sent[-1]["d"]["result"] else "FAIL"), "the agent sent the draft with send_draft")
    if sent: print("    text sent:", json.dumps(sent[-1]["d"]["args"])[:300])
    box = [vt.display[n].rstrip() for n in range(26, 38)]
    print("    input box rows now:", [r for r in box if r.strip().startswith("┃")][:5])
    subprocess.run("pkill -TERM -f " + scratch, shell=True)
os.kill(pid, 9)
