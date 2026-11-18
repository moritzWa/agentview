"""Measured1.5x speech conform and separate schedule/queue explanation."""

import argparse
import json
from itertools import pairwise
from pathlib import Path

from propose45 import NARRATOR, credential, generate_take, timed_words
from soundtrack import RATE, decode, write

SCENES = [
    (0, 5.2, "Overload to dashboard"),
    (5.2, 8.2, "Arrow zoom"),
    (8.2, 11.1, "Start Voice"),
    (11.1, 16.2, "Dialogue"),
    (16.2, 21.2, "ChatGPT MCP"),
    (21.2, 24.3, "Project grouping"),
    (24.3, 27, "Work scope"),
    (27, 33, "Scheduled followup"),
    (33, 40, "Heavy build/test queue"),
    (40, 45, "Bento"),
]
OFFSETS = {
    "question.mp3": 11.2,
    "answer.mp3": 12.5,
    "checkout.mp3": 15.3,
}
TAKES = [
    (
        "intro-sixteen.mp3",
        "Sixteen agents. Sixteen windows. Which one needs you? Agent View sorts them for you.",
        0.15,
        5.2,
    ),
    (
        "nav-jumps.mp3",
        "Right arrow jumps in. Left jumps back.",
        5.3,
        8.2,
    ),
    (
        "voice-cmdk.mp3",
        "Or just ask. Command K starts a voice call.",
        8.3,
        11.1,
    ),
    (
        "chatgpt-away.mp3",
        "Away from your desk? Ask through ChatGPT.",
        16.3,
        20.5,
    ),
    (
        "projects-groups.mp3",
        "Control S groups them by project.",
        21.2,
        24.2,
    ),
    (
        "work-shiftb.mp3",
        "Press Command Shift B to show only work directories.",
        24.4,
        27,
    ),
    (
        "self-schedule.mp3",
        "Agents can schedule their own follow-ups. They wait here, then resume when it’s time.",
        27.1,
        32.5,
    ),
    (
        "bouncer-freeze.mp3",
        "Agent-bouncer queues heavy builds and tests, so they don’t freeze your computer.",
        33.2,
        39.2,
    ),
    (
        "close-opensource.mp3",
        "Agent View. Open source at agent hyphen view dot app.",
        40.2,
        44.6,
    ),
]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", type=Path, required=True)
    args = ap.parse_args()
    root = args.output
    plan = json.loads((root / "followup45/script-timeline.json").read_text())
    folder = root / "proposal45/audio"
    key = None
    cues = [
        dict(c, at=OFFSETS[c["file"]]) for c in plan["cues"] if c["file"] in OFFSETS
    ]
    for filename, text, at, end in TAKES:
        path = folder / filename
        alignment = folder / filename.replace(".mp3", ".alignment.json")
        if not path.exists() or not alignment.exists():
            key = key or credential()
            generate_take(path, alignment, text, NARRATOR, key)
        cues.append(
            {
                "file": filename,
                "text": text,
                "at": at,
                "speaker": "NARRATOR",
                "scene_end": end,
            }
        )
    final = root / "audio/final45"
    final.mkdir(exist_ok=True)
    for c in cues:
        source = folder / c["file"]
        pcm = decode(source, tempo=1.5)
        c.update(
            speed=1.5,
            final_file="audio/final45/" + source.stem + ".wav",
            duration=len(pcm) / RATE,
        )
        c["end"] = c["at"] + c["duration"]
        c["scene_end"] = next(end for start, end, _ in SCENES if start <= c["at"] < end)
        c["fits_scene"] = c["end"] <= c["scene_end"]
        assert c["fits_scene"], c
        write(root / c["final_file"], pcm)
        original = json.loads(
            (folder / c["file"].replace(".mp3", ".alignment.json")).read_text()
        )
        words = timed_words(original, c["text"])
        for word in words:
            word["start"] /= 1.5
            if "end" in word:
                word["end"] = min(c["duration"], word["end"] / 1.5)
        (final / (source.stem + ".words.json")).write_text(json.dumps(words, indent=2))
    cues.sort(key=lambda c: c["at"])
    for left, right in pairwise(cues):
        assert left["end"] + 0.08 < right["at"], (left["file"], right["file"])
    plan.update(
        status="Approved headerless demo,1.5x spoken playback",
        scenes=[{"start": s, "end": e, "title": t} for s, e, t in SCENES],
        cues=cues,
        speech_tempo=1.5,
        pitch_preserved=True,
        presentation_headers=False,
        public_release_approved=False,
        duration_approved=True,
    )
    (root / "edit45.json").write_text(json.dumps(plan, indent=2))
    print(
        json.dumps(
            [
                {
                    "file": c["file"],
                    "at": c["at"],
                    "end": round(c["end"], 3),
                    "speed": c["speed"],
                }
                for c in cues
            ]
        )
    )


if __name__ == "__main__":
    main()
