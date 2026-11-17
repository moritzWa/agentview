#!/usr/bin/env python3
"""Stage only reviewed-safe deliverables in a separate, non-public asset folder.

Does NOT publish, deploy or replace any site/README asset.
"""

import argparse
import json
import shutil
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", type=Path, required=True)
    args = ap.parse_args()
    verification = json.loads((args.output / "verification.json").read_text())
    assert (
        verification["frames"] == 1350
        and verification["decode"] == "all frames and audio packets passed"
    )
    # The revised music-bearing master stays private until creative/publication
    # review. The older checked-in review package is not overwritten here.
    dest = ROOT / "target/film-review"
    # Explicit allowlist. Never copy private runtime state or credentials.
    files = [
        "agentview-review.mp4",
        "edit45.json",
        "review.html",
        "captions.vtt",
        "contact-sheet.jpg",
        "frame-00.0.png",
        "verification.json",
        "audio/manifest.json",
        "audio/timing.json",
        "audio/music.json",
        "captures/overview.png",
        "captures/work.png",
        "captures/resumed.png",
        "captures/layout.json",
        "captures/scheduled.png",
        "captures/continued.png",
        "captures/directories.png",
        "captures/work-directories.png",
        "captures/selected.png",
        "audio/waveform-check.json",
        "waveform-strip.jpg",
        "captures/provenance.json",
    ]
    files += [
        "proposal45/script-timeline.json",
        "proposal45/morph-map.json",
        "proposal45/captures/session-anchors.json",
        "proposal45/captures/overview.png",
    ]
    files += [
        str(p.relative_to(args.output))
        for p in (args.output / "proposal45/audio").glob("*")
        if p.suffix in [".mp3", ".json"]
    ]
    files += [
        str(p.relative_to(args.output))
        for p in (args.output / "chatgpt-desktop/frames45").glob("*.png")
    ]
    files += [
        f"agentview-film-evidence/{name}"
        for name in [
            "native-voice-proof.json",
            "native-voice-send-proof.png",
            "native-voice-receiver.json",
            "native-edge-trim.json",
            "edit-key-events.json",
        ]
        if (args.output / "agentview-film-evidence" / name).exists()
    ]
    for name in files:
        target = dest / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(args.output / name, target)
    shutil.copyfile(Path(__file__).with_name("shotlist.json"), dest / "shotlist.json")
    print(f"Staged private review package: {dest}. Existing public assets unchanged.")


if __name__ == "__main__":
    main()
