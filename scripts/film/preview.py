#!/usr/bin/env python3
"""Dense review sheets of the composited frames, without encoding.

uv run --with pillow --with resvg-py --with numpy scripts/film/preview.py --output ~/Movies/agentview-film-draft [--times 5,5.5] [--step 0.5]
"""

import argparse
from pathlib import Path

from PIL import Image, ImageDraw
from revision45 import Film45
from visual import font


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", type=Path, required=True)
    ap.add_argument("--times", default="")
    ap.add_argument("--step", type=float, default=0.5)
    ap.add_argument("--end", type=float, default=45)
    ap.add_argument("--full", action="store_true", help="also save 1920px frames")
    args = ap.parse_args()
    film = Film45(args.output)
    if args.times:
        times = [float(t) for t in args.times.split(",")]
    else:
        times = [round(i * args.step, 2) for i in range(int(args.end / args.step))]
    folder = args.output / "preview"
    folder.mkdir(exist_ok=True)
    thumbs = []
    for t in times:
        im = film.frame(t)
        if args.full:
            im.resize((1920, 1080), Image.Resampling.LANCZOS).save(
                folder / f"f{t:05.2f}.jpg", quality=90
            )
        thumb = im.resize((640, 360), Image.Resampling.LANCZOS)
        d = ImageDraw.Draw(thumb)
        d.rectangle((0, 0, 639, 359), outline="#999999")
        d.text((8, 6), f"{t:.2f}s", font=font(22, True), fill="#d20f39")
        thumbs.append(thumb)
    per = 12
    for page in range(0, len(thumbs), per):
        chunk = thumbs[page : page + per]
        sheet = Image.new("RGB", (640 * 3, 360 * ((len(chunk) + 2) // 3)), "white")
        for i, th in enumerate(chunk):
            sheet.paste(th, ((i % 3) * 640, (i // 3) * 360))
        sheet.save(folder / f"sheet-{page // per:02d}.jpg", quality=88)
    print(f"{len(times)} frames -> {folder}")


if __name__ == "__main__":
    main()
