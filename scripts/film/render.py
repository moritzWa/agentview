#!/usr/bin/env python3
"""Native 3840×2160 review master. Source UI captures are independently >4K.

uv run --with pillow --with resvg-py --with numpy scripts/film/render.py --output ~/Movies/agentview-film-draft
"""

import argparse
import subprocess
from pathlib import Path

from PIL import Image, ImageDraw
from soundtrack import audio
from visual import FPS, SECONDS, VOICE_CHOICE, Film, H, W, ease, font, provider

__all__ = [
    "FPS",
    "SECONDS",
    "VOICE_CHOICE",
    "Film",
    "H",
    "W",
    "ease",
    "font",
    "provider",
]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", type=Path, required=True)
    ap.add_argument("--stills", action="store_true")
    args = ap.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    film = Film(args.output)
    times = [0, 2.2, 6.7, 11.6, 13.7, 16.7, 20.0, 23.1, 27.5]
    sheet = Image.new("RGB", (1920, 1080), "white")
    for i, t in enumerate(times):
        im = film.frame(t)
        im.save(args.output / f"frame-{t:04.1f}.png")
        thumb = im.resize((640, 360), Image.Resampling.LANCZOS)
        ImageDraw.Draw(thumb).text(
            (12, 12), f"{t:.1f}s", font=font(20, True), fill="#1e66f5"
        )
        sheet.paste(thumb, ((i % 3) * 640, (i // 3) * 360))
    sheet.save(args.output / "contact-sheet.jpg", quality=96)
    if args.stills:
        return
    audio(args.output)
    dest = args.output / "agentview-30s-review.mp4"
    cmd = [
        "ffmpeg",
        "-v",
        "error",
        "-nostdin",
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
        str(args.output / "soundtrack.wav"),
        "-map",
        "0:v",
        "-map",
        "1:a",
        "-c:v",
        "libx264",
        "-preset",
        "fast",
        "-threads",
        "6",
        "-crf",
        "17",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-b:a",
        "256k",
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
            raise RuntimeError("4K encoder failed")
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()
    print(f"Rendered native 4K: {dest}; 30 seconds, 900 frames. No playback.")


if __name__ == "__main__":
    main()
