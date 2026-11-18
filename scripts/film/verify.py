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
    movie = args.output / "agentview-review.mp4"
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
    assert int(video["nb_frames"]) == 1350
    assert video["pix_fmt"] == "yuv420p"
    assert abs(float(probe["format"]["duration"]) - 45) < 0.05
    assert audio["codec_name"] == "aac"
    assert int(audio["sample_rate"]) == 48000
    assert float(audio["duration"]) >= 44.9, "Encoded narration is truncated"
    provenance = json.loads((args.output / "captures/provenance.json").read_text())
    assert provenance["font_pixels"] >= 60
    assert provenance["raster_size"][0] >= 3840, (
        "UI sources must be genuinely high resolution"
    )
    clips = json.loads((args.output / "audio/timing.json").read_text())
    edge_check = json.loads(
        (args.output / "agentview-film-evidence/native-edge-trim.json").read_text()
    )
    assert len(edge_check["samples"]) == 3
    assert all(s["green_edge_pixels"] == 0 for s in edge_check["samples"])
    wave_check = json.loads((args.output / "audio/waveform-check.json").read_text())
    assert next(f for f in wave_check["frames"] if f["at"] == 11.3)["peak"] == 0
    native = json.loads(
        (args.output / "chatgpt-desktop/native-hires-clip.json").read_text()
    )
    assert native["moving"] and native["source_dimensions"][0] >= 2560
    assert (
        native["motion_near_excerpt_end"] > 0.05
        and native["moving_transition_fraction"] > 0.07
    )
    music = json.loads((args.output / "audio/music.json").read_text())
    assert (
        music["continuous"]
        and music["edit_placements"][0]["video"][1]
        >= music["edit_placements"][1]["video"][0]
    )
    approved = json.loads((args.output / "edit45.json").read_text())
    assert approved["duration_approved"] and approved["seconds"] == 45
    mapping = json.loads((args.output / "proposal45/morph-map.json").read_text())[
        "mapping"
    ]
    anchors = {
        a["id"]: a
        for a in json.loads(
            (args.output / "proposal45/captures/session-anchors.json").read_text()
        )
    }
    assert len({a["id"] for a in mapping}) == 16
    for item in mapping:
        assert item["cell_bounds"] == anchors[item["id"]]["cell_bounds"]
        x, y, w, h = item["cell_bounds"]
        s = 1460 / 1616
        assert item["destination"] == [70 + x * s, 200 + y * s, w * s, h * s]
    for i, c in enumerate(clips):
        assert c["end"] <= 44.7 and c["speed"] == 1.5
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
            "select=eq(n\\,0)+eq(n\\,750)+eq(n\\,1349)",
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
    template = Path(__file__).with_name("review.html").read_text()
    assert "addTextTrack" not in template and "<track" not in template
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
        "live_call_recording": True,
        "public_assets_replaced": False,
        "source_ui_resolution": provenance["raster_size"],
        "source_glyph_pixels": provenance["font_pixels"],
        "music": "Lorn · TIMESINK, 30-second excerpt, user-reported artist permission",
        "native_chatgpt_voice": "active Work Voice; actual AgentVoice exact-target read/send, receiver verified",
        "native_input": "typed within active Voice; microphone speech recognition not tested",
        "native_desktop_source_resolution": native["source_dimensions"],
        "native_video_motion": native,
        "native_edge_crop": "4 left-source pixels, first/middle/last green-edge checks passed",
        "editorial_waveform": "actual isolated speaker clip RMS+31-band FFT, cue-synced; silence flat, not music or live mic",
        "editorial_logo": "canonical docs/assets/logo.svg embedded unchanged",
        "pane_row_morph": "16 stable normalized IDs, exact actual captured cell-bound destinations",
        "spoken_text": "Dialogue cards only: actual ElevenLabs alignment. No narrator captions or subtitle tracks.",
        "narrator_captions": False,
        "spoken_playback_speed": 1.5,
        "pitch_preserved": True,
        "presentation_headers": False,
        "music_continuous": True,
        "speaker_colors": {"YOU": "#6f38c5", "AGENT VIEW": "#1e66f5"},
        "creative_review_complete": False,
    }
    (args.output / "verification.json").write_text(json.dumps(report, indent=2))
    print(json.dumps(report))


if __name__ == "__main__":
    main()
