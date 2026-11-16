#!/usr/bin/env python3
"""Deterministic, headless 30-second review film. Pillow compositor + FFmpeg.

uv run --with pillow --with resvg-py scripts/film/render.py --output ~/Movies/agentview-film-draft
Use --stills to inspect the edit without rendering video. Never plays audio.
"""

import argparse
import io
import json
import math
import subprocess
import wave
from array import array
from functools import lru_cache
from pathlib import Path

import resvg_py
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[2]
W, H, FPS, SECONDS = 1600, 900, 30, 30
INK, MUTED, BLUE = "#10121b", "#626879", "#1e66f5"
TEAL, GREEN, LAVENDER = "#179299", "#40a02b", "#7287fd"
VOICE_CHOICE = (
    "Your checkout agent asked whether coupons",
    "belong in the cart or at checkout.",
)


def ease(t):
    t = max(0, min(1, t))
    return 1 - (1 - t) ** 3


def mix(a, b, v):
    return a + (b - a) * v


@lru_cache(maxsize=32)
def font(size, bold=False, mono=False):
    path = (
        "/System/Library/Fonts/Menlo.ttc"
        if mono
        else "/System/Library/Fonts/Supplemental/Arial"
        + (" Bold" if bold else "")
        + ".ttf"
    )
    return ImageFont.truetype(path, size)


def text(im, xy, words, size=26, fill=INK, bold=False, anchor=None, mono=False):
    ImageDraw.Draw(im).text(
        xy, words, font=font(size, bold, mono), fill=fill, anchor=anchor
    )


def box(im, xy, fill="white", radius=24, outline="#e0e4ee", shadow=True):
    d = ImageDraw.Draw(im)
    x, y, r, b = xy
    if shadow:
        for offset, col in [(10, "#e8ebf3"), (5, "#edf0f7")]:
            d.rounded_rectangle((x, y + offset, r, b + offset), radius, fill=col)
    d.rounded_rectangle(xy, radius, fill=fill, outline=outline, width=1)


def logo(im, x, y, scale=1):
    d = ImageDraw.Draw(im)
    for dx, dy in [(-15, -25), (15, -25), (-27, 0), (27, 0), (-15, 25), (15, 25)]:
        a, b = x + dx * scale, y + dy * scale
        rr = 4.3 * scale
        d.ellipse(
            (a - rr, b - rr, a + rr, b + rr),
            outline="#6c6f85",
            width=max(1, int(scale)),
        )
    r = 5.4 * scale
    d.ellipse((x - r, y - r, x + r, y + r), fill=BLUE)


@lru_cache(maxsize=20)
def provider(name, size):
    p = ROOT / "site/providers" / name
    if p.suffix == ".svg":
        raw = resvg_py.svg_to_bytes(svg_path=str(p), width=size, height=size)
        return Image.open(io.BytesIO(raw)).convert("RGBA")
    image = Image.open(p).convert("RGBA")
    image.thumbnail((size, size), Image.Resampling.LANCZOS)
    return image


def icon(im, name, x, y, size=38):
    p = provider(name, size)
    im.paste(p, (int(x), int(y)), p)


class Film:
    def __init__(self, output):
        self.output = output
        self.shots = {
            n: Image.open(output / "captures" / f"{n}.png").convert("RGB")
            for n in ["overview", "work", "resumed"]
        }
        self.rows = json.loads((output / "captures/layout.json").read_text())
        self.opening_cards = {}
        # Static background avoids noisy texture and keeps small terminal text sharp.
        self.bg = Image.new("RGB", (W, H), "#f7f8fc")
        d = ImageDraw.Draw(self.bg)
        for y in range(H):
            a = y / H
            d.line(
                (0, y, W, y),
                fill=(int(250 - 6 * a), int(251 - 5 * a), int(254 - 4 * a)),
            )
        self.panels = {}

    def header(self, im, title, sub):
        logo(im, 87, 65, 0.7)
        text(im, (121, 48), "Agent View", 25, bold=True)
        text(im, (W - 72, 61), "THE 30-SECOND TOUR", 16, MUTED, anchor="rm")
        text(im, (80, 114), title, 52, bold=True)
        text(im, (83, 178), sub, 23, MUTED)

    def terminal(self, im, rect, shot="overview"):
        x, y, w, h = [int(v) for v in rect]
        if w < 5 or h < 5:
            return
        key = (w, h, shot)
        if key not in self.panels:
            if len(self.panels) > 48:
                self.panels.clear()
            layer = Image.new("RGB", (w, h), "white")
            # Header is editorial window chrome. The content is unmodified app output.
            d = ImageDraw.Draw(layer)
            d.rectangle((0, 0, w, 39), fill="#f0f2f7")
            for i, c in enumerate(["#f4a69d", "#eecf87", "#a9d5a2"]):
                d.ellipse((18 + i * 18, 15, 27 + i * 18, 24), fill=c)
            text(
                layer,
                (w // 2, 20),
                "Agent View  /  shop",
                15,
                MUTED,
                anchor="mm",
                mono=True,
            )
            shot_im = self.shots[shot]
            # Crop only unused terminal rows; never move or redraw app controls.
            cropped = shot_im.crop((0, 0, shot_im.width, min(shot_im.height, 570)))
            crop = cropped.resize(
                (w - 24, round(cropped.height * (w - 24) / cropped.width)),
                Image.Resampling.LANCZOS,
            )
            layer.paste(crop, (12, 48))
            self.panels[key] = layer
        box(im, (x, y, x + w, y + h), radius=26)
        layer = self.panels[key]
        mask = Image.new("L", (w, h), 0)
        ImageDraw.Draw(mask).rounded_rectangle((0, 0, w - 1, h - 1), 26, fill=255)
        im.paste(layer, (x, y), mask)

    def waveform(self, im, x, y, t, width=180, height=45, color=BLUE):
        d = ImageDraw.Draw(im)
        for i in range(31):
            a = (
                (0.3 + 0.7 * abs(math.sin(i * 0.64 + t * 4)))
                * height
                * math.sin(math.pi * (i + 1) / 32)
            )
            xx = x + i * width / 31
            d.rounded_rectangle((xx, y - a / 2, xx + 3, y + a / 2), 2, fill=color)

    def opening(self, im, t):
        self.header(im, "All your coding agents.", "One terminal. One voice.")
        p = ease((t - 1.65) / 1.25)
        if p >= 1:
            self.terminal(im, (80, 235, 1440, 520))
            return
        beneath = im.copy()
        if p > 0:
            self.terminal(beneath, (80, 235, 1440, 520))
        providers = [
            (
                "openai.png",
                "Codex",
                "Review the checkout changes.",
                "db-migration",
                90,
                250,
                -3,
            ),
            (
                "claude.svg",
                "Claude Code",
                "Where should coupons go?",
                "checkout-redesign",
                690,
                320,
                3,
            ),
            (
                "devin.png",
                "Devin",
                "Build the storefront.",
                "storefront-build",
                175,
                510,
                2,
            ),
            (
                "opencode.svg",
                "OpenCode",
                "Find the flaky test.",
                "fix-flaky-tests",
                765,
                550,
                -3,
            ),
        ]
        for asset, name, line, session, x, y, tilt in providers:
            if asset not in self.opening_cards:
                card = Image.new("RGBA", (750, 250), (0, 0, 0, 0))
                box(card, (0, 0, 749, 234), radius=26)
                icon(card, asset, 30, 28, 44)
                text(card, (92, 33), name, 27, bold=True)
                text(card, (30, 115), "› " + line, 24, mono=True)
                d = ImageDraw.Draw(card)
                for j, length in enumerate([0.70, 0.86, 0.56]):
                    d.rounded_rectangle(
                        (32, 168 + j * 13, 32 + int(700 * length), 172 + j * 13),
                        2,
                        fill="#e8ebf3",
                    )
                self.opening_cards[asset] = card
            # Destinations come from captured terminal cells, not guessed UI rows.
            row_y = (
                235
                + 48
                + (16 + self.rows[session] * 28) * (1416 / self.shots["overview"].width)
            )
            xx = mix(x, 100, p)
            yy = mix(y, row_y, p)
            ww = max(1, round(mix(750, 330, p)))
            hh = max(1, round(mix(250, 34, p)))
            card = self.opening_cards[asset].resize((ww, hh), Image.Resampling.LANCZOS)
            card = card.rotate(tilt * (1 - p), Image.Resampling.BICUBIC, expand=True)
            im.paste(card, (round(xx), round(yy)), card)
        if p > 0:
            im.paste(Image.blend(im, beneath, ease((p - 0.3) / 0.7)))

    def voice(self, im, t):
        self.header(
            im, "One terminal. One voice.", "Ask once. Unblock the right agent."
        )
        resumed = t >= 11.35
        self.terminal(im, (80, 235, 1440, 520), "resumed" if resumed else "overview")
        local = t - 3
        lines = [
            (0, "YOU", "Which sessions need my attention?"),
            (2.25, "AGENT VIEW", "\n".join(VOICE_CHOICE)),
            (7.1, "YOU", "Tell it checkout."),
            (8.35, "AGENT VIEW", "Sent."),
        ]
        who, words = "YOU", lines[0][2]
        for at, a, b in lines:
            if local >= at:
                who, words = a, b
        box(im, (300, 600, 1300, 800), radius=28, fill="#ffffff", outline="#ccd9fc")
        text(im, (332, 620), who, 16, BLUE, bold=True)
        for i, line in enumerate(words.splitlines()):
            text(im, (332, 652 + i * 39), line, 30, bold=True)
        self.waveform(im, 1060, 630, t, 180, 26)
        text(
            im,
            (332, 758),
            "Staged app state change · not a live call or real send"
            if resumed
            else "Illustrated voice exchange · not a live call recording",
            16,
            MUTED,
        )
        if resumed:
            text(
                im,
                (800, 814),
                "checkout-redesign  →  Working  (staged)",
                22,
                BLUE,
                anchor="mm",
                mono=True,
            )

    def phone(self, im, t):
        self.header(im, "Take your agents with you.", "ChatGPT Voice + AgentVoice MCP")
        p = ease((t - 12) / 0.8)
        self.terminal(im, (80, 260, mix(1440, 940, p), 490), "resumed")
        x = int(mix(1680, 1115, p))
        y = 225
        box(im, (x, y, x + 320, y + 560), radius=48, fill="#121725", outline="#121725")
        # 12px inset: outer 48, inner 36. The phone is explicitly editorial.
        box(
            im,
            (x + 12, y + 12, x + 308, y + 548),
            radius=36,
            fill="white",
            outline="white",
            shadow=False,
        )
        text(im, (x + 160, y + 61), "ChatGPT Voice", 22, bold=True, anchor="mm")
        self.waveform(im, x + 57, y + 230, t, 210, 110)
        text(im, (x + 160, y + 349), "Your agents,", 27, bold=True, anchor="mm")
        text(im, (x + 160, y + 385), "in your pocket.", 27, bold=True, anchor="mm")
        text(im, (x + 160, y + 447), "Read · Search · Send", 18, BLUE, anchor="mm")
        text(im, (x + 160, y + 507), "Illustration", 16, MUTED, anchor="mm")
        text(im, (100, 790), "Same OpenCode sessions. Synced history.", 25, MUTED)

    def work(self, im, t):
        self.header(
            im,
            "Work stays in view.",
            "Hide personal sessions. Keep their work running.",
        )
        self.terminal(im, (80, 235, 1440, 520), "resumed" if t < 17.7 else "work")
        box(im, (582, 720, 1018, 804), radius=24, fill="#edf3ff", outline="#d1defa")
        text(im, (800, 761), "Cmd + Shift + B   Work only", 24, BLUE, anchor="mm")
        text(
            im,
            (800, 832),
            "Global shortcut configured for this setup",
            16,
            MUTED,
            anchor="mm",
        )

    def continuation(self, im, t):
        self.header(
            im,
            "Let the next step run.",
            "Schedule the follow-up. Leave heavy jobs in line.",
        )
        p = ease((t - 20.5) / 0.7)
        self.terminal(im, (80, 245, mix(1440, 850, p), 495), "work")
        x = int(mix(1660, 970, p))
        box(im, (x, 245, x + 550, 466), radius=26)
        text(im, (x + 28, 273), "SCHEDULED", 16, BLUE, bold=True)
        text(im, (x + 28, 316), "Review checkout results", 29, bold=True)
        text(im, (x + 28, 369), "Tomorrow, 9:00 AM", 24, MUTED)
        text(im, (x + 28, 417), "Continue in the same session", 18, MUTED)
        box(im, (x, 490, x + 550, 740), radius=26)
        text(im, (x + 28, 516), "agent-bouncer", 22, TEAL, bold=True)
        text(im, (x + 28, 564), "Heavy-job queue", 30, bold=True)
        text(im, (x + 28, 619), "Running   checkout tests", 22, GREEN, mono=True)
        text(im, (x + 28, 660), "Queued    Rust build", 22, MUTED, mono=True)
        text(im, (x + 28, 705), "Keeps your machine responsive", 18, MUTED)
        text(
            im,
            (800, 801),
            "Editorial summary · scheduled work and queue states illustrated",
            18,
            MUTED,
            anchor="mm",
        )

    def bento(self, im, t):
        text(im, (800, 86), "All your coding agents.", 48, bold=True, anchor="mm")
        text(im, (800, 143), "One terminal. One voice.", 31, MUTED, anchor="mm")
        # The last five seconds are held, not spent introducing six tiles.
        tiles = [
            (
                80,
                222,
                465,
                440,
                "Multi-harness",
                "Codex · Claude · Devin · OpenCode",
                BLUE,
            ),
            (80, 464, 465, 700, "Desktop voice", "Ask. Decide. Keep building.", BLUE),
            (
                1135,
                222,
                1520,
                440,
                "ChatGPT mobile voice",
                "AgentVoice MCP · OpenCode",
                TEAL,
            ),
            (
                1135,
                464,
                1520,
                700,
                "Work view",
                "Personal sessions stay out of sight.",
                LAVENDER,
            ),
            (
                489,
                568,
                1111,
                700,
                "Keeps moving",
                "Scheduled work · Background jobs",
                TEAL,
            ),
        ]
        for i, (x, y, r, b, title, sub, col) in enumerate(tiles):
            drift = round(12 * (1 - ease((t - 25 + i * 0.025) / 0.35)))
            box(im, (x, y + drift, r, b + drift), radius=26)
            text(im, (x + 26, y + 31 + drift), title, 27, col, bold=True)
            # Short, deliberate line breaks preserve the readable end hold.
            if title == "Multi-harness":
                for j, asset in enumerate(
                    ["openai.png", "claude.svg", "devin.png", "opencode.svg"]
                ):
                    icon(im, asset, x + 26 + j * 78, y + 83 + drift, 38)
                text(
                    im,
                    (x + 26, y + 155 + drift),
                    "Four harnesses. One home.",
                    20,
                    MUTED,
                )
            elif title == "Desktop voice":
                self.waveform(im, x + 28, y + 105, t, 220, 38, col)
                text(im, (x + 26, y + 178 + drift), sub, 20, MUTED)
            elif title == "Work view":
                text(im, (x + 26, y + 95 + drift), "Work only", 28, bold=True)
                text(
                    im,
                    (x + 26, y + 151 + drift),
                    "Personal stays out of sight.",
                    20,
                    MUTED,
                )
            elif title == "Keeps moving":
                text(im, (x + 26, y + 83 + drift), sub, 23, MUTED)
            else:
                text(
                    im, (x + 26, y + 96 + drift), "Read · Search · Send", 24, bold=True
                )
                text(im, (x + 26, y + 155 + drift), sub, 20, MUTED)
        box(im, (489, 222, 1111, 544), radius=28, fill="#edf3ff", outline="#dce5fb")
        logo(im, 800, 325, 2.15)
        text(im, (800, 425), "Agent View", 47, bold=True, anchor="mm")
        text(im, (800, 490), "Open source · Built in Rust", 23, MUTED, anchor="mm")
        text(
            im,
            (800, 752),
            "agent-bouncer queue  ·  Subscription usage",
            23,
            MUTED,
            anchor="mm",
        )
        text(
            im,
            (800, 806),
            "agent-view.app    /    github.com/moritzWa/agentview",
            25,
            BLUE,
            anchor="mm",
        )

    def frame(self, t):
        im = self.bg.copy()
        if t < 3:
            self.opening(im, t)
        elif t < 12:
            self.voice(im, t)
        elif t < 17:
            self.phone(im, t)
        elif t < 20.5:
            self.work(im, t)
        elif t < 25:
            self.continuation(im, t)
        else:
            self.bento(im, t)
        text(
            im,
            (80, 872),
            "REVIEW DRAFT  •  Synthetic sessions  •  Voice / phone / continuation illustrated",
            15,
            MUTED,
        )
        return im


def audio(output):
    clips = json.loads((output / "audio/manifest.json").read_text())
    rate = 48000
    samples = array("h", [0]) * (rate * SECONDS)
    metadata = []
    for i, clip in enumerate(clips):
        duration = float(
            subprocess.check_output(
                [
                    "ffprobe",
                    "-v",
                    "error",
                    "-show_entries",
                    "format=duration",
                    "-of",
                    "csv=p=0",
                    str(output / "audio" / clip["file"]),
                ]
            )
        )
        # Fit each line to its reserved window, preserving the full text.
        end = clips[i + 1]["at"] - 0.14 if i + 1 < len(clips) else SECONDS - 0.4
        budget = end - clip["at"]
        speed = max(1, duration / budget)
        if speed > 1.35:
            raise ValueError(
                f"Voice line {i} too rushed ({speed:.2f}x). Revise the edit."
            )
        cmd = [
            "ffmpeg",
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-i",
            str(output / "audio" / clip["file"]),
        ]
        if speed > 1:
            cmd += ["-af", f"atempo={speed:.5f}"]
        cmd += ["-ac", "1", "-ar", str(rate), "-f", "s16le", "pipe:1"]
        pcm = array("h")
        pcm.frombytes(subprocess.check_output(cmd))
        start = round(clip["at"] * rate)
        assert len(pcm) > rate / 4, f"Voice clip {i} decoded empty"
        assert start + len(pcm) <= len(samples)
        samples[start : start + len(pcm)] = pcm
        metadata.append(
            dict(
                clip,
                duration=len(pcm) / rate,
                speed=speed,
                end=clip["at"] + len(pcm) / rate,
            )
        )
    mixed = output / "voice-mix.wav"
    with wave.open(str(mixed), "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(rate)
        wav.writeframes(samples.tobytes())
    # Offline two-pass loudness normalization. Finite PCM mixing avoids filter
    # EOF/adelay edge cases and guarantees a real 30-second audio timeline.
    measure = subprocess.run(
        [
            "ffmpeg",
            "-hide_banner",
            "-nostdin",
            "-i",
            str(mixed),
            "-af",
            "loudnorm=I=-16:TP=-1.5:LRA=7:print_format=json",
            "-f",
            "null",
            "-",
        ],
        capture_output=True,
        text=True,
        check=True,
    )
    m, _ = json.JSONDecoder().raw_decode(measure.stderr[measure.stderr.rfind("{") :])
    norm = (
        f"loudnorm=I=-16:TP=-1.5:LRA=7:measured_I={m['input_i']}"
        f":measured_TP={m['input_tp']}:measured_LRA={m['input_lra']}"
        f":measured_thresh={m['input_thresh']}:offset={m['target_offset']}:linear=true"
    )
    subprocess.run(
        [
            "ffmpeg",
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-y",
            "-i",
            str(mixed),
            "-af",
            norm,
            "-ar",
            str(rate),
            str(output / "narration.wav"),
        ],
        check=True,
    )
    with wave.open(str(output / "narration.wav")) as wav:
        assert wav.getnframes() == rate * SECONDS, (
            "Normalized audio timeline is incomplete"
        )
    (output / "audio/timing.json").write_text(json.dumps(metadata, indent=2))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", type=Path, required=True)
    ap.add_argument("--stills", action="store_true")
    args = ap.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    film = Film(args.output)
    times = [0, 2.2, 3.6, 6.7, 11.6, 13.7, 18.5, 22.7, 27.5]
    thumbs = []
    for i, t in enumerate(times):
        im = film.frame(t)
        im.save(args.output / f"frame-{t:04.1f}.png")
        thumb = im.resize((480, 270), Image.Resampling.LANCZOS)
        text(thumb, (12, 12), f"{t:.1f}s", 18, BLUE, bold=True)
        thumbs.append(thumb)
    sheet = Image.new("RGB", (1440, 810), "white")
    for i, thumb in enumerate(thumbs):
        sheet.paste(thumb, ((i % 3) * 480, (i // 3) * 270))
    sheet.save(args.output / "contact-sheet.jpg", quality=95)
    if args.stills:
        return
    audio(args.output)
    dest = args.output / "agentview-30s-review.mp4"
    cmd = [
        "ffmpeg",
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgb24",
        "-s",
        f"{W}x{H}",
        "-r",
        str(FPS),
        "-i",
        "pipe:0",
        "-i",
        str(args.output / "narration.wav"),
        "-map",
        "0:v",
        "-map",
        "1:a",
        "-c:v",
        "libx264",
        "-preset",
        "fast",
        "-crf",
        "18",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-b:a",
        "192k",
        "-t",
        "30",
        "-movflags",
        "+faststart",
        str(dest),
    ]
    proc = subprocess.Popen(cmd, stdin=subprocess.PIPE)
    try:
        for n in range(FPS * SECONDS):
            proc.stdin.write(film.frame(n / FPS).tobytes())
        proc.stdin.close()
        if proc.wait():
            raise RuntimeError("Video encoder failed")
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()
    print(f"Rendered {dest}: 30 seconds, {W}×{H}, {FPS} fps. No playback.")


if __name__ == "__main__":
    main()
