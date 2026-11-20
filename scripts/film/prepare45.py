"""Conform approved45s cached inputs. No account calls, playback or live sends."""

import argparse
import json
import subprocess
import sys
from itertools import pairwise
from pathlib import Path

from revision45 import KEY_EVENTS


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", type=Path, required=True)
    args = ap.parse_args()
    root = args.output
    plan = json.loads((root / "followup45/script-timeline.json").read_text())
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
    if not (root / "chatgpt-desktop/native-hires-clip.json").exists():
        subprocess.run(
            [
                sys.executable,
                str(Path(__file__).with_name("native_hires.py")),
                "--output",
                str(root),
            ],
            check=True,
        )
    frames = root / "chatgpt-desktop/frames-hires"
    assert len(list(frames.glob("*.png"))) == 186
    navigation = json.loads((root / "navigation/recording.json").read_text())
    assert navigation["input"] == ["Right", "Left"], (
        "Require verified single-Left OpenCode return"
    )
    assert "Ready." in (root / "captures/voice-start.txt").read_text()
    (root / "agentview-film-evidence/edit-key-events.json").write_text(
        json.dumps(
            {
                "events": KEY_EVENTS,
                "source": "Actual recorded Right-open/single-Left-return in OpenCode; actual CmdK fixtureVoice live indicator and CtrlS app captures.",
                "retiming": "Idle/warmup trimmed; overlays conformed to actual input/state changes. No double-tap claim or shortcut change.",
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
