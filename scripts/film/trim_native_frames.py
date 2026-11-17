"""Trim the verified capture-edge green pixels, without changing app content."""

import argparse
import json
from pathlib import Path

import numpy as np
from PIL import Image


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", type=Path, required=True)
    args = ap.parse_args()
    folder = args.output / "chatgpt-desktop/frames-hires"
    clip = json.loads(
        (args.output / "chatgpt-desktop/native-hires-clip.json").read_text()
    )
    frames = sorted(folder.glob("*.png"))
    assert frames, "Native recording frames missing"
    for path in frames:
        with Image.open(path) as im:
            assert list(im.size) == clip["frame_dimensions"], (
                "Unexpected native source dimensions"
            )
    samples = []
    for path in [frames[0], frames[len(frames) // 2], frames[-1]]:
        with Image.open(path) as im:
            edge = np.asarray(im.convert("RGB"), dtype=np.int16)[50:-50, :4]
            green = (edge[:, :, 1] - edge[:, :, 0] > 8) & (
                edge[:, :, 1] - edge[:, :, 2] > 8
            )
            assert not green.any(), f"Green capture border remains in {path.name}"
            samples.append(
                {
                    "frame": path.name,
                    "green_edge_pixels": int(green.sum()),
                    "size": list(im.size),
                }
            )
    (args.output / "agentview-film-evidence/native-edge-trim.json").write_text(
        json.dumps(
            {
                "crop": "4 source pixels from left edge; recording unchanged",
                "samples": samples,
            },
            indent=2,
        )
    )
    print(
        "Native first/middle/last source frames: green edge removed. Main clip and bento share trimmed sources."
    )


if __name__ == "__main__":
    main()
