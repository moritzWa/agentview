"""Muted label cut. Burns short labels with Pillow, no drawtext, no playback."""

import argparse
import subprocess
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

LABELS = [
    (0, 4.0, "Too many agent windows"),
    (4.0, 6.9, "One list, sorted by who needs you"),
    (6.9, 8.8, "Right / Left to open a session"),
    (8.8, 11.4, "Cmd+K: voice"),
    (11.4, 17.8, "Ask once, unblock the agent"),
    (17.8, 23.1, "ChatGPT MCP"),
    (23.1, 26.0, "Group by project"),
    (26.0, 29.0, "Work only"),
    (29.0, 34.6, "Agents schedule follow-ups"),
    (34.6, 40, "Heavy jobs queued"),
    (40, 45, "agent-view.app"),
]


def label_png(folder, index, text):
    path = folder / f"label-{index}.png"
    image = Image.new("RGBA", (3840, 2160), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    font = ImageFont.truetype("/System/Library/Fonts/Supplemental/Arial Bold.ttf", 64)
    box = draw.textbbox((0, 0), text, font=font)
    width, height = box[2] - box[0], box[3] - box[1]
    x, y = (3840 - width) / 2, 1960
    draw.rounded_rectangle(
        (x - 28, y - 16, x + width + 28, y + height + 18), 18, fill=(255, 255, 255, 210)
    )
    draw.text((x, y), text, font=font, fill=(30, 102, 245, 255))
    image.save(path)
    return path


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", type=Path, required=True)
    args = ap.parse_args()
    master = args.output / "agentview-review.mp4"
    bed = args.output / "audio/music-sfx.wav"
    dest = args.output / "agentview-review-silent.mp4"
    folder = args.output / "silent-labels"
    folder.mkdir(exist_ok=True)
    cards = [label_png(folder, i, text) for i, (_, _, text) in enumerate(LABELS)]
    inputs = ["-i", str(master), "-i", str(bed)]
    filters = []
    last = "0:v"
    for i, (start, end, _) in enumerate(LABELS):
        inputs += ["-i", str(cards[i])]
        nxt = f"v{i}"
        filters.append(
            f"[{last}][{i + 2}:v]overlay=enable='between(t,{start},{end})'[{nxt}]"
        )
        last = nxt
    subprocess.run(
        [
            "ffmpeg",
            "-v",
            "error",
            "-nostdin",
            "-y",
            *inputs,
            "-filter_complex",
            ";".join(filters),
            "-map",
            f"[{last}]",
            "-map",
            "1:a",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
            str(dest),
        ],
        check=True,
    )
    print(f"Muted label cut: {dest}")


if __name__ == "__main__":
    main()
