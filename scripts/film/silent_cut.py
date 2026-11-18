"""Muted label cut. Burns short labels with Pillow, no drawtext, no playback."""

import argparse
import subprocess
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

LABELS = [
    (0, 5.2, "Sorted by who needs you"),
    (5.2, 8.2, "Right / Left to jump in"),
    (8.2, 11.1, "Cmd+K: voice"),
    (11.1, 16.2, "Ask once"),
    (16.2, 21.2, "Control from ChatGPT"),
    (21.2, 24.3, "Group by project"),
    (24.3, 27, "Work only"),
    (27, 33, "Agents schedule follow-ups"),
    (33, 40, "Heavy jobs queued"),
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
