"""Measured1.5x speech conform and separate schedule/queue explanation."""

import argparse
import json
from itertools import pairwise
from pathlib import Path

from propose45 import NARRATOR, credential, generate_take, timed_words
from soundtrack import RATE, decode, write

SCENES = [
    (0, 4.0, "Grid of agents"),
    (4.0, 6.9, "One list"),
    (6.9, 8.8, "Arrow zoom"),
    (8.8, 11.4, "Start Voice"),
    (11.4, 17.8, "Dialogue and payoff"),
    (17.8, 23.1, "ChatGPT MCP"),
    (23.1, 26.0, "Project grouping"),
    (26.0, 29.0, "Work scope"),
    (29.0, 34.6, "Scheduled followup"),
    (34.6, 40, "Heavy build/test queue"),
    (40, 45, "Bento"),
]
OFFSETS = {
    "question.mp3": 11.5,
    "answer.mp3": 12.85,
    "checkout.mp3": 15.75,
}
TAKES = [
    (
        "intro-scan.mp3",
        "Still scanning a grid of agents for the one that needs you? Or clicking through chats to switch between them?",
        0.2,
        4.0,
    ),
    (
        "intro-onelist.mp3",
        "Agent View puts every agent in one list, sorted by what needs you.",
        4.1,
        6.9,
    ),
    (
        "nav-jumps.mp3",
        "Right arrow jumps in. Left jumps back.",
        7.0,
        8.8,
    ),
    # Two takes, so "Or just ask." lands on its own instead of running on.
    (
        "voice-ask.mp3",
        "Or just ask.",
        8.9,
        9.9,
    ),
    (
        "voice-cmdk2.mp3",
        "Command K starts a voice call.",
        9.95,
        11.4,
    ),
    (
        "chatgpt-away.mp3",
        "Away from your desk? Ask through ChatGPT.",
        17.9,
        23.1,
    ),
    (
        "projects-groups.mp3",
        "Control S groups them by project.",
        23.2,
        26.0,
    ),
    (
        "work-shiftb.mp3",
        "Press Command Shift B to show only work directories.",
        26.1,
        29.0,
    ),
    (
        "self-schedule.mp3",
        "Agents can schedule their own follow-ups. They wait here, then resume when it’s time.",
        29.2,
        34.6,
    ),
    (
        "bouncer-freeze.mp3",
        "Agent-bouncer queues heavy builds and tests, so they don’t freeze your computer.",
        34.8,
        40,
    ),
    (
        "close-opensource.mp3",
        "Agent View. Open source at agent hyphen view dot app.",
        40.3,
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
