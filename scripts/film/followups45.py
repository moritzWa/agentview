"""Conform user-approved followups within45s. No playback or account changes."""

import argparse
import json
import subprocess
from pathlib import Path

from propose45 import NARRATOR, credential, generate_take

SCENES = [
    (0, 5.6, "Problem and fast morph"),
    (5.6, 10.3, "Arrow-key zoom"),
    (10.3, 15, "Agent View Voice / Command K"),
    (15, 23, "Human / agent exchange"),
    (23, 29.2, "Actual moving ChatGPT MCP video"),
    (29.2, 32.8, "Control S views"),
    (32.8, 35.6, "Work scope"),
    (35.6, 40, "Schedule / agent-bouncer"),
    (40, 45, "Bento hold"),
]
REPLACEMENTS = {
    "navigation": (
        "Use the arrow keys to zoom into or zoom out of the session.",
        "navigation-followup.mp3",
        5.7,
        10.3,
    ),
    "voice_intro": (
        "Talk to your sessions with Agent View Voice. Press Command K to start.",
        "voice-intro.mp3",
        10.35,
        15,
    ),
    "chatgpt": (
        "Use the ChatGPT MCP to talk to Agent View, wherever you are.",
        "chatgpt-followup.mp3",
        23.1,
        29.2,
    ),
    "schedule": (
        "Schedule the follow-up. Agent bouncer queues your heavy jobs.",
        "schedule-followup.mp3",
        35.6,
        40,
    ),
}
OFFSETS = {
    "intro.mp3": (0.15, 5.6),
    "question.mp3": (15.1, 23),
    "answer.mp3": (17.05, 23),
    "checkout.mp3": (21.4, 23),
    "projects.mp3": (29.21, 32.8),
    "work.mp3": (32.81, 35.6),
}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", type=Path, required=True)
    args = ap.parse_args()
    root = args.output
    plan = json.loads((root / "proposal45/script-timeline.json").read_text())
    audio = root / "proposal45/audio"
    key = None
    cues = [c.copy() for c in plan["cues"] if c["file"] in OFFSETS]
    for cue in cues:
        cue["at"], cue["scene_end"] = OFFSETS[cue["file"]]
    for text, name, at, end in REPLACEMENTS.values():
        dest = audio / name
        alignment = audio / name.replace(".mp3", ".alignment.json")
        if not dest.exists() or not alignment.exists():
            key = key or credential()
            generate_take(dest, alignment, text, NARRATOR, key)
        duration = float(
            subprocess.check_output(
                [
                    "ffprobe",
                    "-v",
                    "error",
                    "-show_entries",
                    "format=duration",
                    "-of",
                    "default=nw=1:nk=1",
                    str(dest),
                ],
                text=True,
            )
        )
        cues.append(
            {
                "at": at,
                "scene_end": end,
                "text": text,
                "file": name,
                "speaker": "NARRATOR",
                "duration": duration,
            }
        )
    cues.sort(key=lambda c: c["at"])
    for cue in cues:
        cue["end"] = cue["at"] + cue["duration"]
        cue["fits_scene"] = cue["end"] <= cue["scene_end"]
    plan.update(
        status="Approved followup edit within45seconds",
        scenes=[{"start": s, "end": e, "title": label} for s, e, label in SCENES],
        cues=cues,
        narrator_captions=False,
        music="Continuously loop/edit the same approved30s TIMESINK excerpt; no extra song source.",
    )
    folder = root / "followup45"
    folder.mkdir(exist_ok=True)
    (folder / "script-timeline.json").write_text(json.dumps(plan, indent=2))
    print(
        json.dumps(
            [
                {
                    "file": c["file"],
                    "at": c["at"],
                    "end": round(c["end"], 3),
                    "fits": c["fits_scene"],
                }
                for c in cues
            ]
        )
    )
    assert all(c["fits_scene"] for c in cues), (
        "One measured cue needs timing adjustment, not speedup"
    )


if __name__ == "__main__":
    main()
