#!/usr/bin/env python3
"""Record docs/assets/demo.gif from fixtures/demo-sessions.json.

Run from the repo root after `cargo build --release`:

    uv run --with pillow scripts/demo.py

Needs vhs and ffmpeg. The script writes a VHS tape, records it at twice the
README display size, draws a badge for every shortcut pressed, and encodes the
GIF. Badge timing is derived from the same step list that produces the tape,
so edit `STEPS` rather than the generated tape.
"""

import json
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

REPO = Path(__file__).resolve().parent.parent
OUTPUT_GIF = REPO / "docs/assets/demo.gif"
OUTPUT_MP4 = REPO / "target/demo/demo.mp4"

SCALE = 2
WIDTH, HEIGHT, PADDING, FONT_SIZE = 1300 * SCALE, 560 * SCALE, 20 * SCALE, 16 * SCALE
FRAMERATE = 20
GIF_WIDTH, GIF_FPS = 1600, 20
TYPING_MS = 60

# Sessions the demo user finished earlier and that no longer clutter the list.
PAST_SESSIONS = [
    ("claude:host:stripe-webhooks", "claude", "stripe-webhooks", 3 * 86400),
    ("codex:host:cart-a11y", "codex", "cart-accessibility", 5 * 86400),
    ("opencode:host:legacy-checkout", "opencode", "remove-legacy-checkout", 9 * 86400),
]

# (VHS command, badge keys, badge caption, seconds to wait afterwards).
# `keys` is None for steps that get no badge.
STEPS = [
    ("Sleep", None, None, 1.5),
    ("Down", ["↓"], "select", 0.7),
    ("Down", ["↓"], "select", 0.7),
    ("Space", ["space"], "peek", 3.0),
    ("Space", ["space"], "close peek", 1.0),
    ("Ctrl+G", ["ctrl", "G"], "past sessions", 1.6),
    ('Type "stripe"', None, None, 1.0),
    ("Enter", ["enter"], "restore to the list", 2.2),
    ("Space", ["space"], "peek", 2.6),
    ("Space", ["space"], "close peek", 1.0),
    ('Type "add rate limiting to the checkout API"', None, None, 3.0),
]
BADGE_SECONDS = 1.4


def step_duration(command: str) -> float:
    if command == "Sleep":
        return 0.0
    if command.startswith("Type "):
        return len(json.loads(command[5:])) * TYPING_MS / 1000
    return TYPING_MS / 1000


def tape(state_dir: Path, mp4: Path) -> str:
    lines = [
        f'Output "{mp4}"',
        'Set Shell "bash"',
        f"Set FontSize {FONT_SIZE}",
        f"Set Width {WIDTH}",
        f"Set Height {HEIGHT}",
        f"Set Padding {PADDING}",
        f"Set Framerate {FRAMERATE}",
        'Set Theme "Catppuccin Mocha"',
        f"Set TypingSpeed {TYPING_MS}ms",
        "",
        "Hide",
        f"Type \"export REPO={REPO} HOME={state_dir}/home XDG_STATE_HOME={state_dir}/state PS1='$ '; "
        f'mkdir -p $HOME/Code/shop; cd $HOME/Code/shop; clear" Enter',
        'Type "$REPO/target/release/agentview --fixture $REPO/fixtures/demo-sessions.json" Enter',
        "Sleep 2s",
        "Show",
    ]
    for command, _, _, wait in STEPS:
        if command != "Sleep":
            lines.append(command)
        lines.append(f"Sleep {int(wait * 1000)}ms")
    lines += ["Hide", "Ctrl+C", ""]
    return "\n".join(lines)


def badge_events() -> list[tuple[float, float, list[str], str]]:
    events = []
    clock = 0.0
    for command, keys, caption, wait in STEPS:
        start = clock
        clock += step_duration(command) + wait
        if keys:
            events.append((start, keys, caption))
    timed = []
    for index, (start, keys, caption) in enumerate(events):
        end = start + BADGE_SECONDS
        if index + 1 < len(events):
            end = min(end, events[index + 1][0])
        timed.append((start, end, keys, caption))
    return timed


def font(size: int) -> ImageFont.FreeTypeFont:
    return ImageFont.truetype("/System/Library/Fonts/Menlo.ttc", size)


def draw_badge(keys: list[str], caption: str, path: Path) -> None:
    key_font, caption_font = font(15 * SCALE), font(14 * SCALE)
    pad, gap, cap_pad_x, cap_pad_y = 12 * SCALE, 6 * SCALE, 9 * SCALE, 5 * SCALE
    measure = ImageDraw.Draw(Image.new("RGBA", (1, 1)))

    def size(text: str, typeface: ImageFont.FreeTypeFont) -> tuple[int, int]:
        left, top, right, bottom = measure.textbbox((0, 0), text, font=typeface)
        return right - left, bottom - top

    key_height = size("Mg", key_font)[1] + 2 * cap_pad_y
    key_widths = [max(size(key, key_font)[0] + 2 * cap_pad_x, key_height) for key in keys]
    caption_width = size(caption, caption_font)[0]
    width = pad * 2 + sum(key_widths) + gap * (len(keys) - 1) + gap * 2 + caption_width
    height = key_height + pad * 2

    image = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    draw.rounded_rectangle(
        (0, 0, width - 1, height - 1),
        radius=10 * SCALE,
        fill=(17, 17, 27, 235),
        outline=(124, 92, 255, 255),
        width=2 * SCALE,
    )
    x = pad
    for key, key_width in zip(keys, key_widths):
        draw.rounded_rectangle(
            (x, pad, x + key_width, pad + key_height),
            radius=5 * SCALE,
            fill=(49, 50, 68, 255),
            outline=(88, 91, 112, 255),
            width=SCALE,
        )
        draw.text(
            (x + key_width / 2, pad + key_height / 2),
            key,
            font=key_font,
            fill=(205, 214, 244, 255),
            anchor="mm",
        )
        x += key_width + gap
    draw.text(
        (x + gap, pad + key_height / 2),
        caption,
        font=caption_font,
        fill=(166, 173, 200, 255),
        anchor="lm",
    )
    image.save(path)


def seed_past_sessions(state_dir: Path) -> None:
    registry = state_dir / "state/agentview"
    registry.mkdir(parents=True, mode=0o700)
    registry.chmod(0o700)
    now_ms = int(time.time() * 1000)
    document = {
        "version": 1,
        "sessions": [
            {"id": id, "provider": provider, "name": name, "hidden_at_ms": now_ms - age * 1000}
            for id, provider, name, age in PAST_SESSIONS
        ],
    }
    path = registry / "hidden-sessions.json"
    path.write_text(json.dumps(document, indent=2) + "\n")
    path.chmod(0o600)


def run(*command: str | Path) -> None:
    subprocess.run([str(part) for part in command], check=True)


def main() -> None:
    binary = REPO / "target/release/agentview"
    if not binary.exists():
        sys.exit("build first: cargo build --release")
    for tool in ("vhs", "ffmpeg"):
        if shutil.which(tool) is None:
            sys.exit(f"{tool} is required")

    with tempfile.TemporaryDirectory(prefix="agentview-demo-") as temp:
        temp = Path(temp).resolve()
        state_dir = temp / "env"
        seed_past_sessions(state_dir)
        raw = temp / "raw.mp4"
        tape_path = temp / "demo.tape"
        tape_path.write_text(tape(state_dir, raw))
        run("vhs", tape_path)

        events = badge_events()
        inputs: list[str | Path] = []
        filters = []
        label = "0:v"
        for index, (start, end, keys, caption) in enumerate(events, start=1):
            badge = temp / f"badge-{index}.png"
            draw_badge(keys, caption, badge)
            inputs += ["-i", badge]
            filters.append(
                f"[{label}][{index}:v]overlay=x=W-w-{28 * SCALE}:y={22 * SCALE}"
                f":enable='between(t,{start:.3f},{end:.3f})'[v{index}]"
            )
            label = f"v{index}"
        overlaid = temp / "overlaid.mp4"
        run(
            "ffmpeg", "-y", "-loglevel", "error", "-i", raw, *inputs,
            "-filter_complex", ";".join(filters), "-map", f"[{label}]",
            "-c:v", "libx264", "-crf", "16", "-pix_fmt", "yuv420p", overlaid,
        )
        OUTPUT_MP4.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(overlaid, OUTPUT_MP4)

        palette = temp / "palette.png"
        scale = f"fps={GIF_FPS},scale={GIF_WIDTH}:-1:flags=lanczos"
        run(
            "ffmpeg", "-y", "-loglevel", "error", "-i", overlaid,
            "-vf", f"{scale},palettegen=max_colors=128:stats_mode=diff", palette,
        )
        run(
            "ffmpeg", "-y", "-loglevel", "error", "-i", overlaid, "-i", palette,
            "-lavfi", f"{scale}[x];[x][1:v]paletteuse=dither=none:diff_mode=rectangle",
            OUTPUT_GIF,
        )
    print(f"wrote {OUTPUT_GIF} and {OUTPUT_MP4}")


if __name__ == "__main__":
    main()
