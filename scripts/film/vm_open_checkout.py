"""Open one disposable checkout-redesign session in the VM. No secrets printed."""

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
HOME = Path("/Users/lume")
STATE = ROOT / "state-open"
WORK = HOME / "Code/shop"


def say(line):
    print(line, flush=True)


def main():
    assert Path.home().name == "lume"
    WORK.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env.update(
        HOME=str(HOME),
        XDG_CONFIG_HOME=str(ROOT / "config-open"),
        XDG_DATA_HOME=str(ROOT / "data-open"),
        XDG_STATE_HOME=str(STATE),
        XDG_CACHE_HOME=str(ROOT / "cache-open"),
        TERM="xterm-256color",
        OPENCODE_DISABLE_FFF="1",
    )
    log = ROOT / "open-serve.out"
    serve = subprocess.Popen(
        [str(ROOT / "opencode"), "serve", "--hostname", "127.0.0.1", "--port", "4098"],
        env=env,
        stdout=log.open("w"),
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    password = None
    for _ in range(40):
        text = log.read_text(errors="replace") if log.exists() else ""
        if "server password " in text:
            password = text.split("server password ", 1)[1].split()[0].strip()
            break
        time.sleep(0.4)
    if not password:
        say("BLOCKED serve did not start")
        serve.terminate()
        return
    token = __import__("base64").b64encode(("opencode:" + password).encode()).decode()
    headers = {"Authorization": "Basic " + token, "Content-Type": "application/json"}
    request = urllib.request.Request(
        "http://127.0.0.1:4098/api/session",
        data=json.dumps(
            {
                "title": "checkout-redesign",
                "location": {"directory": str(WORK)},
            }
        ).encode(),
        headers=headers,
    )
    urllib.request.urlopen(request, timeout=30).read()
    say("session created")
    pointer = ROOT / "open-service.json"
    pointer.write_text(
        json.dumps(
            {
                "id": "film-open",
                "password": password,
                "pid": serve.pid,
                "url": "http://127.0.0.1:4098/",
                "version": "v2",
            }
        )
    )
    os.chmod(pointer, 0o600)
    env["AGENTVIEW_OPENCODE_API"] = "v2"
    env["AGENTVIEW_OPENCODE_SERVICE_FILE"] = str(pointer)
    master, slave = pty.openpty()
    import fcntl

    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    agent = subprocess.Popen(
        [
            str(ROOT / "agentview"),
            "--theme",
            "light",
            "--cwd",
            str(WORK),
            "--harness",
            "opencode",
            "--opencode-bin",
            str(ROOT / "opencode"),
            "--no-host-claude",
            "--no-host-codex",
            "--no-host-pi",
            "--no-host-copilot",
        ],
        stdin=slave,
        stdout=slave,
        stderr=slave,
        env=env,
        cwd=str(WORK),
        start_new_session=True,
    )
    os.close(slave)
    blob = b""
    end = time.time() + 25
    opened = False
    while time.time() < end:
        if select.select([master], [], [], 0.3)[0]:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                break
            blob += chunk
            if b"\x1b[6n" in chunk:
                os.write(master, b"\x1b[1;1R")
        if not opened and time.time() > end - 16:
            os.write(master, b"\x1b[C")
            opened = True
            say("sent Right")
            time.sleep(2)
    (ROOT / "open-screen.raw").write_bytes(blob)
    text = blob.decode("utf-8", "replace")
    say("has checkout-redesign " + str("checkout-redesign" in text))
    say("has Code/shop " + str("Code/shop" in text))
    agent.terminate()
    serve.terminate()
    say("open probe finished")


if __name__ == "__main__":
    main()
