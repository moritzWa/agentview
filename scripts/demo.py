#!/usr/bin/env python3
"""Record docs/assets/demo.gif from fixtures/demo-sessions.json.

Run from the repo root after `cargo build --release`:

    uv run --with pillow scripts/demo.py

Needs vhs and ffmpeg. The script writes a VHS tape, records it at twice the
README display size in light mode (Catppuccin Latte with a white background),
draws a badge for every shortcut pressed, and encodes the GIF. It also
generates 500 past sessions with message text for the Ctrl+G search, links
two OpenCode sub-sessions to the session that spawned them, and seeds
OpenCode's recent models so the model picker shows its "Recent · Provider"
groups. Badge timing is derived from the same step list that produces the tape,
so edit `STEPS` rather than the generated tape.
"""

import json
import random
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
WIDTH, HEIGHT, PADDING, FONT_SIZE = 1300 * SCALE, 760 * SCALE, 20 * SCALE, 16 * SCALE
FRAMERATE = 20
GIF_WIDTH, GIF_FPS = 1600, 20
TYPING_MS = 60

FIXTURE = REPO / "fixtures/demo-sessions.json"
PAST_SESSION_COUNT = 500

# The one past session the demo searches for, by a phrase from its messages.
TARGET_PAST_SESSION = {
    "id": "claude:host:webhook-retries",
    "provider_session_id": "44444444-4444-4444-8444-444444444444",
    "provider": "claude",
    "name": "webhook-retries",
    "cwd": "/work/shop-api",
    "age_secs": 23 * 86400,
    "transcript": "Stripe retries a webhook for up to three days, so the handler stores "
    "each event id and skips duplicates. I added an idempotency key table and "
    "a test that replays the same charge.succeeded event twice.",
}

# OpenCode sessions started by another OpenCode session: child -> parent, by
# the id after `opencode:host:` in the fixture.
SESSION_PARENTS = {
    "ses_demo_flaky_payments": "flaky-tests",
    "ses_demo_flaky_clock": "flaky-tests",
}

# OpenCode's own recent models, most recent first, as its model.json keeps them.
RECENT_MODELS = [
    "openai/gpt-6.1-sol",
    "cursor/claude-sonnet-5-5",
    "anthropic/claude-opus-5-5",
    "openai/gpt-6-luna",
    "anthropic/claude-sonnet-5-5",
    "cursor/claude-opus-5-5-1m",
    "openai/gpt-6-astra",
]

# Catppuccin Latte with a white background and black text.
THEME = {
    "name": "Catppuccin Latte White",
    "black": "#5c5f77", "red": "#d20f39", "green": "#40a02b", "yellow": "#df8e1d",
    "blue": "#1e66f5", "magenta": "#ea76cb", "cyan": "#179299", "white": "#acb0be",
    "brightBlack": "#6c6f85", "brightRed": "#de293e", "brightGreen": "#49af3d",
    "brightYellow": "#eea02d", "brightBlue": "#456eff", "brightMagenta": "#fe85d8",
    "brightCyan": "#2d9fa8", "brightWhite": "#bcc0cc",
    "background": "#ffffff", "foreground": "#000000",
    "cursor": "#dc8a78", "selection": "#acb0be",
}

# (VHS command, badge keys, badge caption, seconds to wait afterwards).
# `keys` is None for steps that get no badge.
STEPS = [
    ("Sleep", None, None, 2.5),
    ("Down", None, None, 0.25),
    ("Down", None, None, 0.25),
    ("Down", None, None, 0.25),
    ("Down", None, None, 0.25),
    ("Down", None, None, 0.25),
    ("Down", ["↓"], "select", 0.6),
    ("Down", ["↓"], "sub-session", 0.9),
    ("Space", ["space"], "peek", 2.6),
    ("Space", ["space"], "close peek", 0.8),
    ("Ctrl+G", ["ctrl", "G"], f"search {PAST_SESSION_COUNT} past sessions", 1.6),
    ('Type "idempotency"', None, None, 2.6),
    ("Escape", ["esc"], "clear", 0.6),
    ("Escape", ["esc"], "close", 0.8),
    ('Type "add rate limiting to the checkout API"', None, None, 1.0),
    ("Shift+Tab", ["shift", "tab"], "choose model", 3.2),
    ("Down", ["↓"], "select", 0.5),
    ("Down", ["↓"], "select", 0.5),
    ("Down", ["↓"], "select", 0.9),
    ("Enter", ["enter"], "use this model", 2.5),
]
BADGE_SECONDS = 1.4


def step_duration(command: str) -> float:
    if command == "Sleep":
        return 0.0
    if command.startswith("Type "):
        return len(json.loads(command[5:])) * TYPING_MS / 1000
    return TYPING_MS / 1000


def tape(state_dir: Path, fixture: Path, mp4: Path) -> str:
    lines = [
        f'Output "{mp4}"',
        'Set Shell "bash"',
        f"Set FontSize {FONT_SIZE}",
        f"Set Width {WIDTH}",
        f"Set Height {HEIGHT}",
        f"Set Padding {PADDING}",
        f"Set Framerate {FRAMERATE}",
        f"Set Theme {json.dumps(THEME)}",
        'Set FontFamily "Menlo"',
        "Set LineHeight 1.3",
        f"Set TypingSpeed {TYPING_MS}ms",
        "",
        "Hide",
        f"Type \"export REPO={REPO} HOME={state_dir}/home XDG_STATE_HOME={state_dir}/state PS1='$ '; "
        f'mkdir -p $HOME/Code/shop; cd $HOME/Code/shop; clear" Enter',
        f'Type "$REPO/target/release/agentview --theme light --harness opencode --fixture {fixture}" Enter',
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
        radius=17 * SCALE,  # key radius (5) + padding (12), so the corners are concentric
        fill=(255, 255, 255, 245),
        outline=(30, 102, 245, 255),
        width=2 * SCALE,
    )
    x = pad
    for key, key_width in zip(keys, key_widths):
        draw.rounded_rectangle(
            (x, pad, x + key_width, pad + key_height),
            radius=5 * SCALE,
            fill=(239, 241, 245, 255),
            outline=(172, 176, 190, 255),
            width=SCALE,
        )
        draw.text(
            (x + key_width / 2, pad + key_height / 2),
            key,
            font=key_font,
            fill=(0, 0, 0, 255),
            anchor="mm",
        )
        x += key_width + gap
    draw.text(
        (x + gap, pad + key_height / 2),
        caption,
        font=caption_font,
        fill=(76, 79, 105, 255),
        anchor="lm",
    )
    image.save(path)


def past_sessions() -> list[dict]:
    """Plausible finished sessions across a few repos, with message text."""
    rng = random.Random(7)
    repos = ["shop-api", "shop-web", "infra", "mobile-app", "analytics"]
    areas = ["checkout", "cart", "search", "auth", "orders", "invoices", "emails",
             "inventory", "reviews", "shipping", "coupons", "admin", "onboarding"]
    tasks = ["fix", "refactor", "add-tests", "speed-up", "migrate", "clean-up", "debug"]
    providers = ["claude", "codex", "opencode", "cursor"]
    sentences = [
        "The {area} tests pass locally and in CI now.",
        "I moved the {area} queries behind a repository interface.",
        "Profiling showed the {area} page spent most time in JSON parsing.",
        "Renamed the {area} feature flag and removed the dead branch.",
        "The flaky {area} test depended on wall-clock time; it now uses a fake clock.",
        "Added pagination to the {area} endpoint and updated the client.",
        "Logged the {area} error with the request id so support can find it.",
    ]
    sessions = []
    for index in range(PAST_SESSION_COUNT - 1):
        area = rng.choice(areas)
        provider = rng.choice(providers)
        sessions.append({
            "id": f"{provider}:host:past-{index}",
            "provider_session_id": f"past-{index}",
            "provider": provider,
            "name": f"{rng.choice(tasks)}-{area}-{index}",
            "cwd": f"/work/{rng.choice(repos)}",
            "age_secs": rng.randint(2 * 86400, 120 * 86400),
            "transcript": " ".join(
                rng.choice(sentences).format(area=area) for _ in range(rng.randint(3, 8))
            ),
        })
    sessions.append(TARGET_PAST_SESSION)
    return sessions


def write_fixture(path: Path) -> None:
    document = json.loads(FIXTURE.read_text())
    # Open sessions were active in the last hour, newest first in file order.
    now_ms = int(time.time() * 1000)
    for index, session in enumerate(document["sessions"]):
        session["updated_at"] = now_ms - (2 + index * 4) * 60_000
    document["demo"]["restorable"] = past_sessions()
    path.write_text(json.dumps(document))


def seed_state(state_dir: Path) -> None:
    state = state_dir / "state"
    registry = state / "agentview"
    (registry / "opencode").mkdir(parents=True, mode=0o700)
    registry.chmod(0o700)
    parents = registry / "opencode/session-parents.jsonl"
    parents.write_text(
        "".join(json.dumps({"child": child, "parent": parent}) + "\n"
                for child, parent in SESSION_PARENTS.items())
    )
    opencode = state / "opencode"
    opencode.mkdir(parents=True, mode=0o700)
    recent = [dict(zip(("providerID", "modelID"), model.split("/", 1))) for model in RECENT_MODELS]
    (opencode / "model.json").write_text(json.dumps({"recent": recent, "favorite": []}))


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
        seed_state(state_dir)
        fixture = temp / "fixture.json"
        write_fixture(fixture)
        raw = temp / "raw.mp4"
        tape_path = temp / "demo.tape"
        tape_path.write_text(tape(state_dir, fixture, raw))
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
                f"[{label}][{index}:v]overlay=x=W-w-{28 * SCALE}:y=H-h-{6 * SCALE}"
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
