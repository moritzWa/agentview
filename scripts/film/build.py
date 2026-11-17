#!/usr/bin/env python3
"""Run capture, render, verification and review staging under one queue admission.

python3 ~/.claude/scripts/heavy-job.py --wait -- uv run --with pillow --with pyte --with resvg-py --with numpy scripts/film/build.py --output ~/Movies/agentview-film-draft

Requires existing audio clips and private native recording frames. No API call,
browser, playback or public deploy.
"""

import argparse
import subprocess
import sys
from pathlib import Path


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", type=Path, required=True)
    args = ap.parse_args()
    directory = Path(__file__).parent
    assert (args.output / "audio/manifest.json").exists(), (
        "Generate or copy the dry voice clips first"
    )
    subprocess.run(
        [
            sys.executable,
            str(directory / "capture.py"),
            "--output",
            str(args.output / "captures"),
        ],
        check=True,
    )
    for script in [
        "prepare45.py",
        "trim_native_frames.py",
        "preview_waveform.py",
        "render.py",
        "verify.py",
        "package.py",
    ]:
        subprocess.run(
            [sys.executable, str(directory / script), "--output", str(args.output)],
            check=True,
        )


if __name__ == "__main__":
    main()
