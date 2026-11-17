"""Conform approved45s cached inputs. No account calls, playback or live sends."""

import argparse
import json
import shutil
import subprocess
from itertools import pairwise
from pathlib import Path

from revision45 import KEY_EVENTS


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", type=Path, required=True)
    args = ap.parse_args()
    root = args.output
    plan = json.loads((root / "proposal45/script-timeline.json").read_text())
    assert plan["seconds"] == 45
    assert all(c["fits_scene"] for c in plan["cues"])
    for left, right in pairwise(plan["cues"]):
        assert left["end"] < right["at"]
    approved = dict(
        plan,
        status="Approved45-second edit",
        duration_approved=True,
        approval="User: 45 is fine",
        public_release_approved=False,
    )
    (root / "edit45.json").write_text(json.dumps(approved, indent=2))
    frames = root / "chatgpt-desktop/frames45"
    frames.mkdir(exist_ok=True)
    if len(list(frames.glob("*.png"))) < 180:
        existing = sorted((root / "chatgpt-desktop/frames").glob("*.png"))
        assert len(existing) >= 54
        for i, path in enumerate(existing[:54], 1):
            shutil.copy2(path, frames / f"{i:04d}.png")
        subprocess.run(
            [
                "ffmpeg",
                "-v",
                "error",
                "-nostdin",
                "-y",
                "-ss",
                "55",
                "-i",
                str(root / "chatgpt-desktop/agentview-film-native-send-v4.mov"),
                "-t",
                "4.2",
                "-vf",
                "crop=1020:738:38:26,fps=30",
                "-start_number",
                "55",
                str(frames / "%04d.png"),
            ],
            check=True,
        )
    assert len(list(frames.glob("*.png"))) >= 180
    (root / "agentview-film-evidence/edit-key-events.json").write_text(
        json.dumps(
            {
                "events": KEY_EVENTS,
                "source": "Actual recorded Right-open/double-Left-return phases; actual CtrlS before/after app captures.",
                "retiming": "Idle/warmup trimmed and phases conformed to edit; doubleLeft gap retained at150ms.",
                "work_binding": "Configured global binding illustrated; actual scope change captured through view work.",
            },
            indent=2,
        )
    )
    print(
        "Approved45s cached inputs conformed. Native source read/send retained; no new live interactions."
    )


if __name__ == "__main__":
    main()
