#!/usr/bin/env python3
"""Record the landing page video: a Voice Mode call and agent-bouncer's queue.

Run from the repo root after `cargo build --release`:

    uv run --with pillow scripts/demo_site.py

Needs vhs, ffmpeg and a real `agent-bouncer`. The dashboard runs on
fixtures/demo-sessions.json. The queue is real: four jobs go through
agent-bouncer with two slots. The voice call is the fixture's scripted `voice`
section (nothing is heard, nothing leaves the machine). While the call runs, the
script rewrites the fixture so the session the call answers starts working and
the session the call starts appears. Writes target/demo/site.mp4 (full size),
site/demo.mp4 (web size) and site/demo-poster.png.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import demo  # noqa: E402
import demo_queue  # noqa: E402

REPO = demo.REPO
OUTPUT_DIR = REPO / "target/demo"
SITE = REPO / "site"

WIDTH, HEIGHT, FONT = 1280, 720, 15
# Seconds the hidden start-up takes before the first recorded frame.
HEAD = 2.0

# (VHS command, badge keys, badge caption, seconds to wait afterwards).
STEPS = [
    ("Sleep", None, None, 2.5),
    ("Ctrl+K", ["ctrl", "K"], "talk to your sessions", 20.6),
    ("Ctrl+K", ["ctrl", "K"], "end the call", 6.5),
]
CALL_AT = 2.5
CONNECT = 0.8

JOBS = [
    ("cargo test", "shop-api", 14, 0.0),
    ("playwright e2e", "shop-api", 26, 0.4),
    ("lint + typecheck", "shop-web", 8, 0.8),
    ("docs build", "shop-api/docs", 8, 1.2),
]

VOICE = [
    (500, "you", "Which sessions need me?"),
    (2300, "agent", "Two. db-migration wants approval to run a staging migration."),
    (5400, "agent", "checkout-redesign asks if the coupon field moves to checkout."),
    (8800, "you", "Tell it yes, move it to checkout."),
    (10600, "agent", "Sent to checkout-redesign."),
    (13000, "you", "Start a session in shop-web to upgrade the dependencies."),
    (15200, "agent", "Started. I'll tell you when it's done."),
    (17800, "you", "That's all, bye."),
    (19000, "agent", "Bye."),
]
ANSWER_AT = CALL_AT + CONNECT + 10.6
START_AT = CALL_AT + CONNECT + 15.2


def build_fixture(work: Path, stage: int) -> dict:
    document = json.loads(demo.FIXTURE.read_text())
    now_ms = int(time.time() * 1000)
    for index, session in enumerate(document["sessions"]):
        session["updated_at"] = now_ms - (2 + index * 4) * 60_000
        session["cwd"] = str(work / session["cwd"].removeprefix("/work/"))
        if session["name"] == "api-docs":
            session["cwd"] = str(work / "shop-api/docs")
        if stage >= 1 and session["name"] == "checkout-redesign":
            session["state"] = "working"
            session["raw_state"] = "running"
            session["summary"] = "Moving the coupon field to the checkout page"
            session["updated_at"] = now_ms
    if stage >= 2:
        document["sessions"].insert(2, {
            "id": "opencode:host:upgrade-deps",
            "provider_session_id": "ses_demo_upgrade",
            "provider": "opencode",
            "runtime": {"kind": "host"},
            "kind": "managed",
            "name": "upgrade-dependencies",
            "cwd": str(work / "shop-web"),
            "state": "working",
            "summary": "Reading the changelogs of the packages that are out of date",
            "raw_state": "busy",
            "pid": 4190,
            "started_at": None,
            "updated_at": now_ms,
            "pull_requests": None,
            "capabilities": ["inspect"],
        })
    document["demo"]["restorable"] = []
    document["demo"]["voice"] = [
        {"at_ms": at, "who": who, "text": text} for at, who, text in VOICE
    ]
    return document


def drive_fixture(path: Path, work: Path, go: Path) -> threading.Thread:
    def run() -> None:
        while not go.exists():
            time.sleep(0.05)
        started = time.time()
        for stage, at in ((1, ANSWER_AT), (2, START_AT)):
            time.sleep(max(0.0, started + HEAD + at - time.time()))
            tmp = path.with_suffix(".tmp")
            tmp.write_text(json.dumps(build_fixture(work, stage)))
            tmp.replace(path)

    thread = threading.Thread(target=run, daemon=True)
    thread.start()
    return thread


def main() -> None:
    binary = REPO / "target/release/agentview"
    if not binary.exists():
        sys.exit("build first: cargo build --release")
    for tool in ("vhs", "ffmpeg"):
        if shutil.which(tool) is None:
            sys.exit(f"{tool} is required")
    bouncer = demo_queue.find_bouncer()

    demo.WIDTH, demo.HEIGHT = WIDTH * demo.SCALE, HEIGHT * demo.SCALE
    demo.FONT_SIZE = FONT * demo.SCALE
    demo_queue.STEPS = STEPS
    demo_queue.JOBS = JOBS

    with tempfile.TemporaryDirectory(prefix="agentview-site-demo-") as temp:
        temp = Path(temp).resolve()
        home = OUTPUT_DIR / "site-demo-home"
        shutil.rmtree(home, ignore_errors=True)
        work = home / "work"
        for _, folder, _, _ in JOBS:
            (work / folder).mkdir(parents=True, exist_ok=True)
        (work / "shop-web").mkdir(parents=True, exist_ok=True)
        demo.seed_state(temp / "env")
        env = {
            "HOME": str(home),
            "XDG_STATE_HOME": str(temp / "env/state"),
            "PATH": f"{Path(bouncer).parent}:{os.environ['PATH']}",
            "AGENT_BOUNCER_DIR": str(temp / "bouncer"),
            "AGENT_BOUNCER_SLOTS": str(demo_queue.SLOTS),
            "AGENT_BOUNCER_MIN_FREE_GB": "0",
            "AGENT_BOUNCER_MAX_CPU": "100",
        }
        fixture = temp / "fixture.json"
        fixture.write_text(json.dumps(build_fixture(work, 0)))
        demo_queue.seed_history(temp / "bouncer")
        go = temp / "go"
        raw = temp / "raw.mp4"
        tape_path = temp / "site.tape"
        tape_text = demo_queue.tape(env, fixture, work, go, raw)
        tape_path.write_text(tape_text.replace("--fixture", "--refresh-ms 500 --fixture"))
        jobs = demo_queue.start_jobs(bouncer, env, work, go)
        drive_fixture(fixture, work, go)
        demo.run("vhs", tape_path)
        jobs.join(timeout=60)

        inputs: list[str | Path] = []
        filters = []
        label = "0:v"
        for index, (start, end, keys, caption) in enumerate(demo_queue.badge_events(), start=1):
            badge = temp / f"badge-{index}.png"
            demo.draw_badge(keys, caption, badge)
            inputs += ["-i", badge]
            filters.append(
                f"[{label}][{index}:v]overlay=x=W-w-{28 * demo.SCALE}:y=H-h-{40 * demo.SCALE}"
                f":enable='between(t,{start:.3f},{end:.3f})'[v{index}]"
            )
            label = f"v{index}"
        OUTPUT_DIR.mkdir(parents=True, exist_ok=True)
        full = OUTPUT_DIR / "site.mp4"
        demo.run(
            "ffmpeg", "-y", "-loglevel", "error", "-i", raw, *inputs,
            "-filter_complex", ";".join(filters), "-map", f"[{label}]",
            "-c:v", "libx264", "-crf", "16", "-pix_fmt", "yuv420p", full,
        )
        web = SITE / "demo.mp4"
        demo.run(
            "ffmpeg", "-y", "-loglevel", "error", "-i", full,
            "-vf", f"scale={WIDTH}:{HEIGHT}:flags=lanczos,fps=24",
            "-c:v", "libx264", "-crf", "27", "-preset", "slow", "-pix_fmt", "yuv420p",
            "-movflags", "+faststart", "-an", web,
        )
    shutil.rmtree(home, ignore_errors=True)
    print(f"wrote {full} and {web}")


if __name__ == "__main__":
    main()
