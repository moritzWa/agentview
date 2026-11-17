"""Select real active video from new3K native recordings. No UI redraw/holds."""

import argparse
import hashlib
import json
import subprocess
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw


def info(path):
    data = json.loads(
        subprocess.check_output(
            [
                "ffprobe",
                "-v",
                "error",
                "-show_streams",
                "-show_format",
                "-of",
                "json",
                str(path),
            ]
        )
    )
    stream = next(s for s in data["streams"] if s["codec_type"] == "video")
    return stream, float(data["format"]["duration"])


def activity(path, crop):
    raw = subprocess.check_output(
        [
            "ffmpeg",
            "-v",
            "error",
            "-nostdin",
            "-threads",
            "4",
            "-i",
            str(path),
            "-vf",
            crop + ",fps=2,scale=384:164",
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "pipe:1",
        ]
    )
    frames = np.frombuffer(raw, dtype=np.uint8).reshape(-1, 164, 384, 3)
    gray = frames[:, 12:150, 40:370].mean(axis=3).astype(np.float32)
    score = np.abs(np.diff(gray, axis=0)).mean(axis=(1, 2))
    return frames, score


def choose_peak(score, low, high):
    candidates = [i for i in range(len(score)) if low <= (i + 1) / 2 <= high]
    assert candidates, "No eligible actual video activity"
    # Favor sustained real UI changes instead of a single capture-edge artifact.
    rank = lambda i: float(score[max(0, i - 2) : min(len(score), i + 3)].sum())
    best = max(candidates, key=rank)
    assert rank(best) > 0.05, "Native video has no visible activity"
    return (best + 1) / 2


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", type=Path, required=True)
    args = ap.parse_args()
    root = args.output
    folder = root / "chatgpt-desktop"
    first = folder / "native-chatgpt-v6-3k.mov"
    second = folder / "native-chatgpt-v6-send-3k.mov"
    stream, duration = info(first)
    other, other_duration = info(second)
    assert (stream["width"], stream["height"]) == (3068, 1338)
    assert (other["width"], other["height"]) == (3068, 1338)
    # 34px native shadow, then the4px capture-edge trim already approved.
    crop = "crop=2996:1270:38:34"
    _, scores1 = activity(first, crop)
    _, scores2 = activity(second, crop)
    peak1 = choose_peak(scores1, 10, min(80, duration - 4))
    peak2 = choose_peak(scores2, 2.1, other_duration - 4)
    start1 = max(0, min(duration - 3, peak1 - 1.3))
    start2 = max(0, min(other_duration - 3.2, peak2 - 2.0))
    frames = folder / "frames-hires"
    frames.mkdir(exist_ok=True)
    for old in frames.glob("*.png"):
        old.unlink()
    for source, start, length, number in [
        (first, start1, 3.0, 1),
        (second, start2, 3.2, 91),
    ]:
        subprocess.run(
            [
                "ffmpeg",
                "-v",
                "error",
                "-nostdin",
                "-y",
                "-ss",
                str(start),
                "-i",
                str(source),
                "-frames:v",
                str(round(length * 30)),
                "-vf",
                crop + ",fps=30",
                "-start_number",
                str(number),
                str(frames / "%04d.png"),
            ],
            check=True,
        )
    paths = sorted(frames.glob("*.png"))
    assert len(paths) == 186
    hashes = []
    gray = []
    sheet = Image.new("RGB", (1500, 600), "white")
    for i, path in enumerate(paths):
        with Image.open(path) as im:
            assert im.size == (2996, 1270)
            hashes.append(hashlib.sha256(im.tobytes()).hexdigest())
            gray.append(
                np.asarray(im.resize((384, 164)).convert("L"), dtype=np.float32)[
                    12:150, 40:370
                ]
            )
            if i in [0, 30, 60, 90, 120, 144, 150, 156, 165, 185]:
                slot = [0, 30, 60, 90, 120, 144, 150, 156, 165, 185].index(i)
                thumb = im.copy()
                thumb.thumbnail((300, 260))
                sheet.paste(thumb, ((slot % 5) * 300, (slot // 5) * 300))
                ImageDraw.Draw(sheet).text(
                    ((slot % 5) * 300 + 8, (slot // 5) * 300 + 270),
                    f"{23 + i / 30:.2f}s movie",
                    fill="black",
                )
    gray = np.asarray(gray)
    changes = np.abs(np.diff(gray, axis=0)).mean(axis=(1, 2))
    unique = len(set(hashes)) / len(hashes)
    moving_fraction = float((changes > 0.01).mean())
    late = float(np.abs(gray[165] - gray[135]).mean())
    assert late > 0.05, "Native app must visibly move around movie28s, not hold a still"
    assert unique > 0.3 and (changes > 0.1).sum() >= 12, (
        "Selected native excerpt is too static"
    )
    metadata = {
        "source_dimensions": [3000, 1270],
        "raw_capture_dimensions": [3068, 1338],
        "frame_dimensions": [2996, 1270],
        "moving": True,
        "frames": 186,
        "fps": 30,
        "unique_frame_fraction": unique,
        "moving_transition_fraction": moving_fraction,
        "motion_near_movie28s": late,
        "segments": [
            {"source": first.name, "start": start1, "duration": 3.0},
            {"source": second.name, "start": start2, "duration": 3.2},
        ],
        "retimed": False,
        "editorial_freeze": False,
        "provenance": "Actual native ChatGPT Work Voice, typed prompts, exact-target AgentVoice read/send; idle footage trimmed. No screenshot animation or artificial UI motion.",
        "border_trim": "34px native shadow plus4 left-edge pixels; app content retained.",
    }
    (folder / "native-hires-clip.json").write_text(json.dumps(metadata, indent=2))
    sheet.save(root / "native-hires-motion-sheet.jpg", quality=96)
    print(
        json.dumps(
            {k: v for k, v in metadata.items() if k not in ["segments", "provenance"]}
        )
    )


if __name__ == "__main__":
    main()
