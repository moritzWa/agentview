#!/usr/bin/env python3
"""Capture the actual TUI in an isolated PTY. No desktop, network or user sessions.

uv run --with pillow --with pyte scripts/film/capture.py --output TARGET
"""

import argparse
import copy
import fcntl
import hashlib
import json
import os
import pty
import select
import shutil
import signal
import struct
import subprocess
import tempfile
import termios
import time
from pathlib import Path

import pyte
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[2]
COLS, ROWS = 132, 23
RASTER_SCALE = 3
PALETTE = [
    "5c5f77",
    "d20f39",
    "40a02b",
    "df8e1d",
    "1e66f5",
    "ea76cb",
    "179299",
    "acb0be",
]


def color(value, default):
    names = dict(
        zip(
            ["black", "red", "green", "brown", "blue", "magenta", "cyan", "white"],
            PALETTE,
        )
    )
    value = names.get(value, value)
    return "#" + value if len(value) == 6 else default


def picture(screen, path):
    font = ImageFont.truetype("/System/Library/Fonts/Menlo.ttc", 20 * RASTER_SCALE)
    im = Image.new(
        "RGB",
        ((COLS * 12 + 32) * RASTER_SCALE, (ROWS * 28 + 32) * RASTER_SCALE),
        "white",
    )
    draw = ImageDraw.Draw(im)
    for y in range(ROWS):
        for x in range(COLS):
            c = screen.buffer[y][x]
            fg, bg = color(c.fg, "#000000"), color(c.bg, "#ffffff")
            if c.reverse:
                fg, bg = bg, fg
            box = tuple(
                v * RASTER_SCALE
                for v in (16 + x * 12, 16 + y * 28, 28 + x * 12, 44 + y * 28)
            )
            draw.rectangle(box, fill=bg)
            if c.data.strip():
                draw.text((box[0], box[1]), c.data, font=font, fill=fg)
    im.save(path)
    path.with_suffix(".txt").write_text("\n".join(screen.display))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", type=Path, required=True)
    native = Path.home() / ".local/share/opencode-v2/bin/native/agentview"
    ap.add_argument(
        "--binary", default=str(native) if native.is_file() else "agentview"
    )
    args = ap.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    # Agent View deliberately hides sessions rooted under the OS temp folder.
    with tempfile.TemporaryDirectory(prefix="isolated-home-", dir=args.output) as tmp:
        home = Path(tmp)
        work = home / "Code/shop"
        personal = home / "Code/weekend"
        work.mkdir(parents=True)
        personal.mkdir(parents=True)
        conf = home / ".config/agentview"
        conf.mkdir(parents=True)
        (conf / "work-roots.json").write_text(json.dumps({"roots": [str(work)]}))
        data = json.loads((ROOT / "fixtures/demo-sessions.json").read_text())
        # Keep the composition legible. These are real app rows, not drawn imitations.
        data["sessions"] = [
            s
            for s in data["sessions"]
            if s["name"] in ["checkout-redesign", "fix-flaky-tests", "db-migration"]
        ]
        for s in data["sessions"]:
            s["cwd"] = str(work)
        p = copy.deepcopy(data["sessions"][2])
        p.update(
            id="opencode:host:weekend",
            name="plan-weekend",
            cwd=str(personal),
            summary="Compare two hiking routes",
            state="working",
        )
        data["sessions"].append(p)
        devin = copy.deepcopy(data["sessions"][2])
        devin.update(
            id="devin:host:storefront",
            provider="devin",
            provider_session_id="synthetic-storefront",
            name="storefront-build",
            summary="Building the storefront preview",
            pid=None,
        )
        data["sessions"].append(devin)
        fixture = home / "film.json"
        fixture.write_text(json.dumps(data))
        queue_dir = home / "queue"
        queue_dir.mkdir()
        queue_file = queue_dir / "synthetic-status.json"
        queue_file.write_text("{}")
        bins = home / "bin"
        bins.mkdir()
        queue_stub = bins / "agent-bouncer"
        queue_stub.write_text(
            '#!/usr/bin/env python3\nimport os,pathlib\nprint((pathlib.Path(os.environ["AGENT_BOUNCER_DIR"])/"synthetic-status.json").read_text())\n'
        )
        queue_stub.chmod(0o700)
        env = {
            "PATH": str(bins) + os.pathsep + os.environ["PATH"],
            "AGENT_BOUNCER_DIR": str(queue_dir),
            "LANG": "en_US.UTF-8",
            "HOME": str(home),
            "XDG_STATE_HOME": str(home / "state"),
            "XDG_CONFIG_HOME": str(home / ".config"),
            "XDG_DATA_HOME": str(home / "data"),
            "TERM": "xterm-256color",
            "COLORTERM": "truecolor",
            "NO_COLOR": "",
        }
        # Hide the real queue too. Fixture mode disables the queue watcher.
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        proc = subprocess.Popen(
            [
                args.binary,
                "--theme",
                "light",
                "--harness",
                "opencode",
                "--refresh-ms",
                "250",
                "--fixture",
                str(fixture),
            ],
            stdin=slave,
            stdout=slave,
            stderr=slave,
            env=env,
            cwd=work,
            start_new_session=True,
        )
        os.close(slave)
        screen = pyte.Screen(COLS, ROWS)
        stream = pyte.ByteStream(screen)

        def drain(seconds):
            until = time.monotonic() + seconds
            while time.monotonic() < until:
                if select.select([master], [], [], 0.05)[0]:
                    try:
                        chunk = os.read(master, 65536)
                    except OSError:
                        break
                    stream.feed(chunk)
                    # Ratatui's terminal initialization requests cursor position.
                    if b"\x1b[6n" in chunk:
                        os.write(master, b"\x1b[1;1R")

        try:
            drain(8)
            assert "checkout-redesign" in "\n".join(screen.display), "\n".join(
                screen.display
            )
            assert "plan-weekend" in "\n".join(screen.display)
            picture(screen, args.output / "overview.png")
            rows = {}
            for name in [
                "checkout-redesign",
                "db-migration",
                "fix-flaky-tests",
                "storefront-build",
            ]:
                rows[name] = next(
                    i for i, line in enumerate(screen.display) if name in line
                )
            (args.output / "layout.json").write_text(json.dumps(rows, indent=2))
            # This is a staged fixture update, not a voice send. Capture the real
            # app moving the same synthetic checkout row into Working.
            checkout = next(
                s for s in data["sessions"] if s["name"] == "checkout-redesign"
            )
            checkout.update(
                state="working",
                raw_state="running",
                summary="Moving coupon field to checkout; updating tests",
            )
            fixture.write_text(json.dumps(data))
            drain(1)
            display = "\n".join(screen.display)
            assert "checkout-redesign" not in display.split("Working")[0]
            assert "checkout-redesign" in display.split("Working")[1]
            picture(screen, args.output / "resumed.png")
            # Global scope uses the same isolated state as this capture.
            r = subprocess.run(
                [args.binary, "view", "work"], env=env, capture_output=True, check=False
            )
            if r.returncode:
                raise RuntimeError(
                    "Capture binary lacks work view. Build latest origin/main."
                )
            drain(1)
            picture(screen, args.output / "work.png")
            assert "Work only" in "\n".join(screen.display)
            assert "plan-weekend" not in "\n".join(screen.display)
            scheduled = copy.deepcopy(data["sessions"][2])
            scheduled.update(
                id="opencode:host:review-checkout",
                name="review-checkout",
                provider_session_id="synthetic-review-checkout",
                state="scheduled",
                raw_state="background: wake-later Tomorrow 09:00: Review checkout results",
                summary="Review checkout results",
                pid=None,
            )
            data["sessions"] = [checkout, scheduled]
            fixture.write_text(json.dumps(data))
            queue_file.write_text(
                json.dumps(
                    {
                        "running": [
                            {
                                "label": "checkout tests",
                                "cwd": str(work),
                                "started_ms": int(time.time() * 1000) - 42000,
                            }
                        ],
                        "waiting": [
                            {
                                "label": "Rust build",
                                "cwd": str(work),
                                "since_ms": int(time.time() * 1000) - 12000,
                                "reason": "slots",
                            }
                        ],
                        "est_free_ms": 180000,
                    }
                )
            )
            drain(3)
            os.write(
                master, b"\x19"
            )  # Actual Ctrl+Y expands the integrated Scheduled section.
            drain(1)
            display = "\n".join(screen.display)
            assert "review-checkout" in display, display
            assert "Heavy jobs" in display, display
            picture(screen, args.output / "scheduled.png")
            scheduled.update(
                state="working",
                raw_state="background: shell: checking checkout results",
                summary="Reviewing checkout tests and preview",
            )
            fixture.write_text(json.dumps(data))
            drain(1)
            picture(screen, args.output / "continued.png")
            (args.output / "provenance.json").write_text(
                json.dumps(
                    {
                        "source_reference": subprocess.check_output(
                            ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
                        ).strip(),
                        "binary": shutil.which(args.binary) or args.binary,
                        "binary_sha256": hashlib.sha256(
                            Path(shutil.which(args.binary) or args.binary).read_bytes()
                        ).hexdigest(),
                        "capture": "actual Agent View PTY, rasterized via pyte",
                        "raster_size": [
                            (COLS * 12 + 32) * RASTER_SCALE,
                            (ROWS * 28 + 32) * RASTER_SCALE,
                        ],
                        "font_pixels": 20 * RASTER_SCALE,
                        "queue_source": "isolated read-only synthetic agent-bouncer status; actual app widget",
                        "data": "isolated synthetic fixture; no live calls or sends",
                        "checks": [
                            "work-only heading visible",
                            "personal session hidden",
                            "staged checkout row moved from Needs input to Working",
                            "real Scheduled and Heavy jobs widgets visible together",
                        ],
                    },
                    indent=2,
                )
            )
        finally:
            if proc.poll() is None:
                try:
                    os.write(master, b"\x03")
                except OSError:
                    pass  # The terminal may already have closed on an app error.
            drain(0.3)
            # Close the PTY before waiting, including its backpressure on exit.
            os.close(master)
            try:
                proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(proc.pid, signal.SIGTERM)
                except (ProcessLookupError, PermissionError):
                    proc.terminate()
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    try:
                        os.killpg(proc.pid, signal.SIGKILL)
                    except (ProcessLookupError, PermissionError):
                        proc.kill()
                    proc.wait(timeout=5)


if __name__ == "__main__":
    main()
