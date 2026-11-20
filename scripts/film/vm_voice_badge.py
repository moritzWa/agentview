"""Isolated VM probe for the real session voice badge. Prints no secrets."""

import json
import os
import pty
import select
import struct
import subprocess
import termios
import time
import urllib.request
from pathlib import Path

ROOT = Path("/Users/lume/film-voice")
HOME = ROOT / "home"
STATE = ROOT / "state"


def say(line):
    print(line, flush=True)


def main():
    assert Path.home().name == "lume"
    HOME.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env.update(
        HOME=str(HOME),
        XDG_CONFIG_HOME=str(ROOT / "config"),
        XDG_DATA_HOME=str(ROOT / "data"),
        XDG_STATE_HOME=str(STATE),
        XDG_CACHE_HOME=str(ROOT / "cache"),
        TERM="xterm-256color",
        COLORTERM="truecolor",
        OPENCODE_DISABLE_FFF="1",
        PATH="/Users/lume/film-voice/bin:" + os.environ.get("PATH", ""),
    )
    for key in [
        "AGENTVIEW_OPENCODE_SERVICE_FILE",
        "AGENTVIEW_OPENCODE_API",
        "OPENCODE_CONFIG_DIR",
        "OPENCODE_DB",
    ]:
        env.pop(key, None)
    serve_log = ROOT / "serve.out"
    serve = subprocess.Popen(
        [str(ROOT / "opencode"), "serve", "--hostname", "127.0.0.1", "--port", "4097"],
        env=env,
        stdout=serve_log.open("w"),
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    password = None
    for _ in range(40):
        text = serve_log.read_text(errors="replace") if serve_log.exists() else ""
        if "server password " in text and "listening" in text:
            password = text.split("server password ", 1)[1].split()[0].strip()
            break
        time.sleep(0.4)
    if not password:
        say("BLOCKED opencode serve did not advertise a loopback password")
        serve.terminate()
        return
    pointer = ROOT / "service.json"
    pointer.write_text(
        json.dumps(
            {
                "id": "film-voice",
                "password": password,
                "pid": serve.pid,
                "url": "http://127.0.0.1:4097/",
                "version": "v2",
            }
        )
    )
    os.chmod(pointer, 0o600)
    url = "http://127.0.0.1:4097"
    token = __import__("base64").b64encode(("opencode:" + password).encode()).decode()
    headers = {"Authorization": "Basic " + token, "Content-Type": "application/json"}
    workspace = HOME / "synthetic-workspace"
    workspace.mkdir(exist_ok=True)
    request = urllib.request.Request(
        url + "/api/session",
        data=json.dumps(
            {
                "title": "Agent View film voice badge",
                "location": {"directory": str(workspace)},
            }
        ).encode(),
        headers=headers,
    )
    created = json.loads(urllib.request.urlopen(request, timeout=30).read())
    session = created.get("data", created)
    say("synthetic session created " + str(bool(session.get("id"))))
    settings = STATE / "agentview/voice/settings.json"
    settings.write_text(
        json.dumps(
            {
                "browser": "hidden",
                "chrome_path": "/Users/lume/film-voice/bin/google-chrome",
            }
        )
    )
    os.chmod(settings, 0o600)
    env["AGENTVIEW_OPENCODE_API"] = "v2"
    env["AGENTVIEW_OPENCODE_SERVICE_FILE"] = str(pointer)
    master, slave = pty.openpty()
    fcntl_winsize = struct.pack("HHHH", 40, 120, 0, 0)
    import fcntl

    fcntl.ioctl(slave, termios.TIOCSWINSZ, fcntl_winsize)
    agent = subprocess.Popen(
        [
            str(ROOT / "agentview"),
            "--theme",
            "light",
            "--cwd",
            str(workspace),
            "--harness",
            "opencode",
            "--opencode-bin",
            str(ROOT / "opencode"),
            "--no-host-claude",
            "--no-host-codex",
            "--no-host-pi",
            "--no-host-copilot",
            "--refresh-ms",
            "250",
        ],
        stdin=slave,
        stdout=slave,
        stderr=slave,
        env=env,
        cwd=str(workspace),
        start_new_session=True,
    )
    os.close(slave)
    blob = b""
    launched = False
    started = False
    tab_opened = False
    end = time.time() + 90
    launch_at = time.time() + 6
    voice_at = time.time() + 12
    while time.time() < end:
        if select.select([master], [], [], 0.3)[0]:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                break
            blob += chunk
            if b"\x1b[6n" in chunk:
                os.write(master, b"\x1b[1;1R")
        now = time.time()
        if not launched and now >= launch_at:
            os.write(master, b"Say hello and stop.\r")
            launched = True
            say("sent a launch prompt")
        if launched and not started and now >= voice_at:
            os.write(master, b"\x1b[107;9u")
            started = True
            say("sent Command K")
        if started and not tab_opened and now >= voice_at + 8:
            os.write(master, b"\x1b[C")
            tab_opened = True
            say("sent Right into the session")
        text = blob.decode("utf-8", "replace")
        if "\u25cf voice 0:" in text:
            say("BADGE session indicator observed")
            time.sleep(1.2)
            if select.select([master], [], [], 0.4)[0]:
                try:
                    blob += os.read(master, 65536)
                except OSError:
                    pass
            break
    (ROOT / "badge-screen.raw").write_bytes(blob)
    tail = blob.decode("utf-8", "replace")
    if "\u25cf voice " not in tail:
        say("BLOCKED session badge absent")
    elif "connecting" in tail and "\u25cf voice 0:" not in tail:
        say("BLOCKED call stayed connecting")
    agent.terminate()
    serve.terminate()
    say("probe finished")


if __name__ == "__main__":
    main()
