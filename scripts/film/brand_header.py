#!/usr/bin/env python3
"""Replace the header row of every terminal raster with one rendered by the current binary.

The approved captures keep every other cell. A full recapture would change rows,
glyphs and hints, so only the first terminal row (3x raster, 132 px) is swapped.

    uv run --with pillow --with pyte python scripts/film/capture.py \
        --output RECAPTURE --binary target/debug/betteragentview
    uv run --with pillow python scripts/film/brand_header.py \
        --draft ~/Movies/agentview-film-draft --recapture RECAPTURE

Patches captures/*.png (the matching header on the Work-only ones), the Right/Left
navigation frames and captures/voice-live.png. Every band is checked to be
pixel-identical to the old header before it is replaced.
"""

import argparse
from pathlib import Path

from PIL import Image, ImageChops

BAND = 132
WIDTH = 4848


def header_text(path):
    return path.read_text().split("\n")[0]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--draft", type=Path, required=True)
    ap.add_argument("--recapture", type=Path, required=True)
    args = ap.parse_args()
    caps = args.draft / "captures"
    old_plain = Image.open(caps / "overview.png").convert("RGB").crop((0, 0, WIDTH, BAND))
    new_plain = Image.open(args.recapture / "overview.png").convert("RGB").crop((0, 0, WIDTH, BAND))
    plain_text = header_text(args.recapture / "overview.txt")

    patched = 0
    for txt in sorted(caps.glob("*.txt")) + sorted((args.draft / "navigation").glob("*.txt")):
        lines = txt.read_text().split("\n")
        if "agentview v0" not in lines[0]:
            continue
        work = "Work only" in lines[0]
        source = args.recapture / txt.name
        band = (
            Image.open(source.with_suffix(".png")).convert("RGB").crop((0, 0, WIDTH, BAND))
            if work
            else new_plain
        )
        png = txt.with_suffix(".png")
        image = Image.open(png).convert("RGB")
        if not work:
            same = ImageChops.difference(image.crop((0, 0, WIDTH, BAND)), old_plain).getbbox()
            assert same is None, f"{png} header differs from the plain header"
        image.paste(band, (0, 0))
        image.save(png)
        lines[0] = header_text(source) if work else plain_text
        txt.write_text("\n".join(lines))
        patched += 1
    live = caps / "voice-live.png"
    image = Image.open(live).convert("RGB")
    image.paste(new_plain, (0, 0))
    image.save(live)
    print(f"patched {patched} rasters and voice-live.png")


if __name__ == "__main__":
    main()
