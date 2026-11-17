"""Small frame-synced proof strip from the actual isolated narration clips."""

import argparse
import io
import json

import resvg_py
from PIL import Image
from revision45 import SPEAKER_COLORS
from revision45 import Film45 as Film
from visual import txt


def main():
    from pathlib import Path

    ap = argparse.ArgumentParser()
    ap.add_argument("--output", type=Path, required=True)
    args = ap.parse_args()
    film = Film(args.output)
    sheet = Image.new("RGB", (1440, 320), "white")
    report = []
    for i, (t, speaker) in enumerate(
        [
            (14.4, "YOU"),
            (15.0, "YOU"),
            (16.1, "YOU"),
            (16.7, "AGENT VIEW"),
            (18.0, "AGENT VIEW"),
            (21.0, "YOU"),
        ]
    ):
        levels = film.voice_tracks.at(t, speaker)
        content = txt(20, 30, f"{t:.2f}s · {speaker}", 21, bold=True) + film.waves(
            20, 94, t, 440, 85, color=SPEAKER_COLORS[speaker], speaker=speaker
        )
        svg = f'<svg xmlns="http://www.w3.org/2000/svg" width="480" height="160"><rect width="480" height="160" fill="white"/>{content}</svg>'
        panel = Image.open(io.BytesIO(resvg_py.svg_to_bytes(svg_string=svg))).convert(
            "RGB"
        )
        sheet.paste(panel, ((i % 3) * 480, (i // 3) * 160))
        report.append(
            {
                "at": t,
                "speaker": speaker,
                "peak": float(levels.max()),
                "bands": levels.tolist(),
            }
        )
    assert report[2]["peak"] == 0, "Between-cue silence must be flat"
    assert report[0]["bands"] != report[1]["bands"], "Actual voice frames must move"
    sheet.save(args.output / "waveform-strip.jpg", quality=97)
    (args.output / "audio/waveform-check.json").write_text(
        json.dumps(
            {
                "source": "speaker-isolated narration, not live microphone or music",
                "frames": report,
            },
            indent=2,
        )
    )
    print(
        "Audio-reactive frame strip passed voice movement and between-cue silence checks."
    )


if __name__ == "__main__":
    main()
