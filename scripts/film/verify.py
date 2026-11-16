#!/usr/bin/env python3
"""Validate the encoded deliverable, write captions and its safe review page."""

import argparse
import json
import subprocess
from pathlib import Path

from PIL import Image, ImageStat


def stamp(seconds):
    ms = round(seconds * 1000)
    return f"{ms // 3600000:02d}:{ms // 60000 % 60:02d}:{ms // 1000 % 60:02d}.{ms % 1000:03d}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", type=Path, required=True)
    args = ap.parse_args()
    movie = args.output / "agentview-30s-review.mp4"
    probe = json.loads(
        subprocess.check_output(
            [
                "ffprobe",
                "-v",
                "error",
                "-show_format",
                "-show_streams",
                "-of",
                "json",
                str(movie),
            ]
        )
    )
    video = next(s for s in probe["streams"] if s["codec_type"] == "video")
    audio = next(s for s in probe["streams"] if s["codec_type"] == "audio")
    assert (video["width"], video["height"]) == (3840, 2160)
    assert video["r_frame_rate"] == "30/1"
    assert int(video["nb_frames"]) == 900
    assert video["pix_fmt"] == "yuv420p"
    assert abs(float(probe["format"]["duration"]) - 30) < 0.05
    assert audio["codec_name"] == "aac"
    assert int(audio["sample_rate"]) == 48000
    assert float(audio["duration"]) >= 29.9, "Encoded narration is truncated"
    provenance = json.loads((args.output / "captures/provenance.json").read_text())
    assert provenance["font_pixels"] >= 60
    assert provenance["raster_size"][0] >= 3840, (
        "UI sources must be genuinely high resolution"
    )
    clips = json.loads((args.output / "audio/timing.json").read_text())
    for i, c in enumerate(clips):
        assert c["end"] <= 30 and c["speed"] <= 1.35
        if i:
            assert clips[i - 1]["end"] < c["at"]
    # Decode every frame/audio packet, not just the container header.
    subprocess.run(
        ["ffmpeg", "-v", "error", "-i", str(movie), "-f", "null", "-"], check=True
    )
    subprocess.run(
        [
            "ffmpeg",
            "-v",
            "error",
            "-y",
            "-i",
            str(movie),
            "-vf",
            "select=eq(n\\,0)+eq(n\\,450)+eq(n\\,899)",
            "-fps_mode",
            "vfr",
            str(args.output / "verified-%02d.png"),
        ],
        check=True,
    )
    for i in range(1, 4):
        with Image.open(args.output / f"verified-{i:02d}.png") as frame:
            assert max(ImageStat.Stat(frame.convert("RGB")).stddev) > 10, (
                "Blank encoded checkpoint"
            )
    result = subprocess.run(
        [
            "ffmpeg",
            "-hide_banner",
            "-i",
            str(movie),
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
    start = result.stderr.rfind("{")
    loudness, _ = json.JSONDecoder().raw_decode(result.stderr[start:])
    assert -18 < float(loudness["input_i"]) < -14
    assert float(loudness["input_tp"]) <= 0
    vtt = (
        "WEBVTT\n\n"
        + "\n\n".join(
            f"{stamp(c['at'])} --> {stamp(c['end'])}\n{c['text']}" for c in clips
        )
        + "\n"
    )
    (args.output / "captions.vtt").write_text(vtt)
    template = Path(__file__).with_name("review.html").read_text()
    # Inline cues also work when opened with file://, without a caption CORS error.
    template = template.replace(
        'id="film-captions">[]',
        'id="film-captions">' + json.dumps(clips).replace("</", "<\\/"),
    )
    (args.output / "review.html").write_text(template)
    report = {
        "duration": float(probe["format"]["duration"]),
        "frames": int(video["nb_frames"]),
        "resolution": [video["width"], video["height"]],
        "video": video["codec_name"],
        "audio": audio["codec_name"],
        "sample_rate": audio["sample_rate"],
        "integrated_lufs": float(loudness["input_i"]),
        "true_peak_dbtp": float(loudness["input_tp"]),
        "decode": "all frames and audio packets passed",
        "voice_overlap": False,
        "live_call_recording": False,
        "public_assets_replaced": False,
        "source_ui_resolution": provenance["raster_size"],
        "source_glyph_pixels": provenance["font_pixels"],
        "music": "Lorn · TIMESINK, 30-second excerpt, user-reported artist permission",
        "native_chatgpt_voice": "blocked in recording VM: NotReadableError, Microphone is already in use",
        "creative_review_complete": False,
    }
    (args.output / "verification.json").write_text(json.dumps(report, indent=2))
    print(json.dumps(report))


if __name__ == "__main__":
    main()
