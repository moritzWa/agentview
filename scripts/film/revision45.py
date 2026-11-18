"""Approved45s edit. Reuses captured UI, not new live-account interactions."""

import base64
import io
import json
from pathlib import Path

import numpy as np
import resvg_py
from PIL import Image, ImageFilter
from propose45 import morph_box
from soundtrack import decode
from visual import (
    BLUE,
    INK,
    MUTED,
    VOICE_CHOICE,
    Film,
    H,
    W,
    font,
    image,
    rect,
    txt,
)
from waveform import VoiceTracks, envelope

SECONDS = 45
PURPLE = "#6f38c5"
SPEAKER_COLORS = {"YOU": PURPLE, "AGENT VIEW": BLUE, "NARRATOR": INK}
KEY_EVENTS = {
    "Right": 5.8,
    "Left": 7.2,
    "CmdK": 9.2,
    "CtrlS": 22.2,
    "Work": 25.2,
}


def keycap(x, y, label, pressed=False, scale=1):
    width = 80 if label == "Ctrl" else 56
    color = "white" if pressed else INK
    face = BLUE if pressed else "white"
    out = (
        f'<g transform="translate({x} {y}) scale({scale})">'
        + rect(0, 3, width, 54, 10, "#cdd1db")
        + rect(0, 0, width, 52, 10, face, "#d9dde6")
        + rect(3, 3, width - 6, 46, 7, face)
    )
    if label in ["Right", "Left", "Down"]:
        transform = (
            "scale(-1 1)"
            if label == "Left"
            else "rotate(90)"
            if label == "Down"
            else ""
        )
        out += f'<g transform="translate({width / 2} 26) {transform}" fill="none" stroke="{color}" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round"><path d="M-14 0H14M4-10L14 0 4 10"/></g>'
    elif label == "Cmd":
        out += f'<g fill="none" stroke="{color}" stroke-width="2.6"><path d="M21 19h14v14H21z M21 19v-5a5 5 0 1 0-5 5h5 M35 19h5a5 5 0 1 0-5-5v5 M35 33v5a5 5 0 1 0 5-5h-5 M21 33h-5a5 5 0 1 0 5 5v-5"/></g>'
    elif label == "Shift":
        out += f'<path d="M28 12l-13 14h8v12h10V26h8z" fill="none" stroke="{color}" stroke-width="2.6" stroke-linejoin="round"/>'
    else:
        out += txt(
            width / 2, 36, label, 20 if label == "Ctrl" else 28, color, True, "middle"
        )
    return out + "</g>"


def shortcut_overlay(keys, label, pressed=False, x=None, y=728):
    """One glass pill. Keys and label share its vertical center."""
    scale = 1.25
    width = 720
    height = 92
    # The stage group is translate(-44) scale(1.055); local 800 stays frame-center.
    if x is None:
        x = 800 - width / 2
    key_width = (
        sum(80 if key == "Ctrl" else 56 for key in keys) * scale + (len(keys) - 1) * 12
    )
    total = key_width + 28 + font(26, True).getlength(label)
    start = x + (width - total) / 2
    key_body = 52 * scale
    key_y = y + (height - key_body) / 2
    out = (
        '<g data-shortcut-overlay="shared" data-glass="0.32">'
        + f'<g opacity=".32">{rect(x, y, width, height, 26, "#d8edff")}</g>'
    )
    for key in keys:
        out += keycap(start, key_y, key, pressed, scale)
        start += (80 if key == "Ctrl" else 56) * scale + 12
    out += txt(start + 16, y + height / 2 + 9, label, 26, BLUE, True)
    return out + "</g>"


def footer_window(im, hide_setup=False, display_width=1460, max_height=560):
    """Crop so the path/footer line is complete, never sliced mid-glyph."""
    gray = np.asarray(im.convert("L"))
    text = np.where((gray < 160).any(axis=1))[0]
    last = int(text[-1]) if len(text) else gray.shape[0] - 1
    bottom = min(gray.shape[0], last + 40)
    top = 636 if hide_setup and gray.shape[0] > 900 else 0
    # Walk off any glyph row the setup crop might land on.
    while top > 0 and (gray[top] < 160).any():
        top -= 1
    height = display_width * (bottom - top) / gray.shape[1]
    if height > max_height:
        keep = int(max_height / display_width * gray.shape[1])
        top = max(0, bottom - keep)
        while top > 0 and (gray[top] < 160).any():
            top -= 1
    if top >= last:
        top = 0
    assert bottom > last, "Footer crop still slices the path line"
    return top, bottom


def word_slots(lines, size=30):
    slots = []
    for row, line in enumerate(lines):
        prefix = ""
        for word in line.split():
            slots.append((word, font(size, True).getlength(prefix), row * 39))
            prefix += word + " "
    return slots


def reveal(lines, words, local, x, y, size=30, color=INK):
    slots = word_slots(lines, size)
    assert [s[0] for s in slots] == [w["text"] for w in words]
    return "".join(
        txt(x + dx, y + dy, word, size, color, True)
        for (word, dx, dy), timing in zip(slots, words)
        if local >= timing["start"]
    )


def line_wrap(text, size=23, max_width=1360):
    lines = [""]
    for word in text.split():
        proposed = (lines[-1] + " " + word).strip()
        if lines[-1] and font(size, True).getlength(proposed) > max_width:
            lines.append(word)
        else:
            lines[-1] = proposed
    return lines


class Film45(Film):
    def header(self, *args, **kwargs):
        return ""

    def terminal(self, shot, x=70, y=194, w=1460, h=790, region=None):
        # Actual software pixels only, no editorial window title or traffic lights.
        return image(self.crop(shot, region), x, y, w, h)

    def __init__(self, output):
        super().__init__(output)
        self.plan = json.loads((self.output / "edit45.json").read_text())
        self.cues = self.plan["cues"]
        self.alignments = {
            c["file"]: json.loads(
                (
                    self.output / Path(c["final_file"]).with_suffix(".words.json")
                ).read_text()
            )
            for c in self.cues
        }
        self.voice_tracks = VoiceTracks(
            [
                {
                    "at": c["at"],
                    "speaker": c["speaker"],
                    "levels": envelope(decode(self.output / c["final_file"])),
                }
                for c in self.cues
                if c["speaker"] != "NARRATOR"
            ]
        )
        self.mapping = json.loads(
            (self.output / "proposal45/morph-map.json").read_text()
        )["mapping"]
        assert len({a["id"] for a in self.mapping}) == 16
        self.morph_source = Image.open(
            self.output / "proposal45/captures/overview.png"
        ).convert("RGB")
        self.morph_height = max(a["cell_bounds"][1] + 28 for a in self.mapping)
        self.morph_source = self.morph_source.crop(
            (0, 0, self.morph_source.width, round(self.morph_height * 3))
        )
        self.morph_source_uri = self._image_uri(self.morph_source)
        self.morph_rows = {
            a["id"]: self._image_uri(
                self.morph_source.crop(
                    (
                        a["raster_bounds"][0],
                        a["raster_bounds"][1],
                        a["raster_bounds"][0] + a["raster_bounds"][2],
                        a["raster_bounds"][1] + a["raster_bounds"][3],
                    )
                )
            )
            for a in self.mapping
        }
        self.navigation_meta = json.loads(
            (self.output / "navigation/recording.json").read_text()
        )
        self.native_clip = json.loads(
            (self.output / "chatgpt-desktop/native-hires-clip.json").read_text()
        )
        assert (
            self.native_clip["source_dimensions"][0] >= 2560
            and self.native_clip["moving"]
        )
        self.native_frames_dir = self.output / "chatgpt-desktop/frames-hires"
        self.native_frames = sorted(self.native_frames_dir.glob("*.png"))
        assert len(self.native_frames) >= 186, "Moving native6.2s excerpt missing"
        self.still = None
        self.session_uris = {}
        live = self.output / "captures/voice-live.png"
        if live.exists():
            self.shots["voice-live"] = Image.open(live).convert("RGB")

    @staticmethod
    def _image_uri(im):
        out = io.BytesIO()
        im.save(out, format="PNG")
        return "data:image/png;base64," + base64.b64encode(out.getvalue()).decode()

    def status_image(self, selected=False):
        if not selected:
            return image(
                self.morph_source_uri, 70, 200, 1460, self.morph_height * 1460 / 1616
            )
        data = self.crop("selected", (0, 0, 1616, self.morph_height))
        return image(data, 70, 200, 1460, self.morph_height * 1460 / 1616)

    def intro(self, t):
        p = max(0, min(1, (t - 1.4) / 0.9))
        p = p * p * (3 - 2 * p)
        out = self.header(
            "Too many agents. Too many windows."
            if t < 2.0
            else "Agent View. One terminal.",
            "All your coding agents. One terminal. One voice.",
        )
        out += (
            f'<g opacity="{max(0, (p - 0.85) / 0.15)}">' + self.status_image() + "</g>"
        )
        logs = [
            "$ pnpm test checkout --watch",
            "  compiling src/routes/cart.tsx…",
            "@@ -38,7 +38,11 @@ checkout",
            "+ const total = applyCoupon(cart);",
            "  GET /api/checkout 200  182ms",
            "  retrying flaky suite (3 of20)",
            "  ███████░░░ 72% building",
            "  watching184 changed files",
            "  npm run typecheck -- --strict",
            "  ✓ migrated12 schema changes",
        ]
        for i, a in enumerate(self.mapping):
            x, y, w, h = morph_box(a["grid"], a["destination"], p)
            identifier = f"morph{i}"
            radius = 4 * (1 - p)
            out += f'<defs><clipPath id="{identifier}">{rect(x, y, w, h, radius)}</clipPath></defs><g clip-path="url(#{identifier})">'
            out += rect(x, y, w, h, radius, "white", "#b8bec9" if p < 0.98 else "none")
            row_mix = max(0, min(1, (p - 0.93) / 0.07))
            out += f'<g opacity="{1 - row_mix}">' + rect(x, y, w, 24, 0, "#e3e6ed")
            label = {
                "claude": "Claude",
                "codex": "Codex",
                "opencode": "OpenCode",
                "cursor": "Cursor",
                "devin": "Devin",
            }.get(a["provider"], a["provider"])
            scale = 1460 / 1616
            name_offset = a["actual_text"].index(a["name"]) * 12 * scale
            provider_offset = a["actual_text"].index(label) * 12 * scale
            baseline = y + 17 * (1 - p) + 19 * scale * p
            size = 11.5 * (1 - p) + 20 * scale * p
            out += txt(
                x + 8 * (1 - p) + 12 * scale * p,
                baseline,
                "✱" if a["state"] == "needs_input" else "✳",
                size,
                a["state_color"],
                mono=True,
            )
            out += txt(
                x + 26 * (1 - p) + name_offset * p,
                baseline,
                a["name"],
                size,
                INK,
                mono=True,
            )
            provider_start = 360 - 8 - font(10, mono=True).getlength(label)
            expand = max(0, (p - 0.5) * 2)
            out += txt(
                x + provider_start * (1 - expand) + provider_offset * expand,
                baseline,
                label,
                10.5 * (1 - expand) + 20 * scale * expand,
                MUTED,
                mono=True,
            )
            shift = (int(t * (3 + i % 3)) + i * 3) % len(logs)
            for j in range(8):
                line = logs[(shift + j) % len(logs)]
                if j == 5:
                    line = (
                        ("? " + a["summary"])[:48]
                        if a["state"] == "needs_input"
                        else a["summary"][:48]
                    )
                out += txt(
                    x + 8,
                    y + 37 + j * 13,
                    line,
                    10.1,
                    a["state_color"] if j == 5 else MUTED,
                    mono=True,
                )
            out += "</g>"
            out += (
                f'<g opacity="{row_mix}">'
                + image(self.morph_rows[a["id"]], x, y, w, h).replace(
                    "xMidYMid meet", "none"
                )
                + "</g></g>"
            )
        return out

    def navigation(self, t):
        right, left = KEY_EVENTS["Right"], KEY_EVENTS["Left"]
        out = self.header(
            "Zoom into or out of your session.",
            "Use the arrow keys.",
        )
        # Left returns to the same 20-row fixture. The dedicated-session
        # return frames showed one Done row and this film's own build command.
        # Whole dashboard, eased toward the selected row. No strip crop.
        out += self.gentle_dashboard(
            0 if t < right or t >= left else min(1, (t - right) / 0.45)
        )
        if t < left:
            out += shortcut_overlay(
                ["Right"], "Zoom in", right <= t < right + 0.22, y=760
            )
        else:
            out += shortcut_overlay(
                ["Left"], "Zoom out", left <= t < left + 0.22, y=760
            )
        return out

    def session_image(self, path, hide_setup):
        key = (str(path), hide_setup)
        if key not in self.session_uris:
            source = Image.open(path)
            top, bottom = footer_window(source, hide_setup)
            cropped = source.crop((0, top, source.width, bottom))
            height = 1460 * cropped.height / cropped.width
            self.session_uris[key] = (
                self._image_uri(cropped),
                height,
                bottom > top,
            )
        data, height, _complete = self.session_uris[key]
        return image(data, 70, 168, 1460, height).replace("xMidYMid meet", "none")

    def gentle_dashboard(self, amount):
        # Whole list, eased toward the selected row. No strip crop.
        scale = 1 + 0.12 * amount
        return (
            f'<g transform="translate(800 430) scale({scale}) translate(-800 -430)">'
            + image(self.crop("overview"), 70, 160, 1460, 680)
            + "</g>"
        )

    def clean_zoom(self, shot, region):
        base = self.shots[shot]
        key = ("blur", shot)
        if key not in self.session_uris:
            blurred = base.filter(ImageFilter.GaussianBlur(28))
            self.session_uris[key] = self._image_uri(blurred)
        out = image(self.session_uris[key], 70, 110, 1460, 600)
        out += '<g opacity=".45">' + rect(70, 110, 1460, 600, 16, "#f4f5f7") + "</g>"
        left, top, right, lower = region
        crop_w = max(1, right - left)
        crop_h = max(1, lower - top)
        card_w = 1320.0
        card_h = max(72.0, min(160.0, card_w * crop_h / crop_w))
        card_w = card_h * crop_w / crop_h
        return out + image(
            self.crop(shot, region), (1600 - card_w) / 2, 310, card_w, card_h
        )

    def native_transcript(self, path, x, y, w, h):
        key = ("native", str(path))
        if key not in self.session_uris:
            im = Image.open(path)
            # Drop the title bar only. Keep the window, including the orb.
            cropped = im.crop((0, 52, im.width, im.height))
            self.session_uris[key] = self._image_uri(cropped)
        data = self.session_uris[key]
        clip = f"nat{abs(hash(str(path)))}"
        return (
            f'<defs><clipPath id="{clip}">{rect(x, y, w, h, 16)}</clipPath></defs>'
            f'<g clip-path="url(#{clip})">' + image(data, x, y, w, h) + "</g>"
        )

    def voice_intro(self, t):
        active = t >= KEY_EVENTS["CmdK"]
        out = self.header("Agent View Voice", "Talk to your sessions.")
        # The stage group pulls content up. Push this beat down so the
        # recording fills the frame instead of sitting in a white band.
        out += (
            '<g transform="translate(0 150)">'
            + image(
                self.crop("voice-start" if active else "overview"),
                40,
                40,
                1520,
                760,
            )
            + "</g>"
        )
        return out + shortcut_overlay(
            ["Cmd", "K"],
            "Start Voice",
            KEY_EVENTS["CmdK"] <= t < KEY_EVENTS["CmdK"] + 0.3,
            y=900,
        )

    def dialogue(self, t):
        cue = next(
            c
            for c in self.cues
            if c["file"]
            == (
                "question.mp3"
                if t < 12.5
                else "answer.mp3"
                if t < 15.3
                else "checkout.mp3"
            )
        )
        who = cue["speaker"]
        color = SPEAKER_COLORS[who]
        out = self.header(
            "One terminal. One voice.", "Ask once. Unblock the right agent."
        ) + image(
            self.crop("voice-live" if "voice-live" in self.shots else "voice-start"),
            70,
            110,
            1460,
            600,
        )
        out += rect(300, 614, 1000, 192, 25, "white", "#d3def8") + txt(
            333, 645, who, 15, color, True
        )
        out += self.waves(1070, 641, t, 170, 36, color, speaker=who)
        lines = list(VOICE_CHOICE) if cue["file"] == "answer.mp3" else [cue["text"]]
        out += reveal(lines, self.alignments[cue["file"]], t - cue["at"], 333, 687, 30)
        return out

    def chatgpt(self, t):
        out = self.header(
            "ChatGPT MCP",
            "Your authenticated connection to Agent View, wherever you are.",
        )
        out += rect(70, 200, 1460, 610, 24)
        n = min(len(self.native_frames) - 1, max(0, int((t - 16.2) * 30)))
        # Crop past the window title and the system-prompt bubble.
        return out + self.native_transcript(self.native_frames[n], 90, 150, 1420, 620)

    def projects(self, t):
        out = self.header(
            "Status or project. Your view.",
            "Control S changes grouping, not which sessions are shown.",
        )
        mix = max(0, min(1, (t - KEY_EVENTS["CtrlS"]) / 0.55))
        out += image(self.crop("overview"), 70, 110, 1460, 600)
        if mix:
            out += (
                f'<g opacity="{mix}">'
                + image(self.crop("directories"), 70, 110, 1460, 600)
                + "</g>"
            )
        pressed = KEY_EVENTS["CtrlS"] <= t < KEY_EVENTS["CtrlS"] + 0.3
        out += shortcut_overlay(["Ctrl", "S"], "Switch view", pressed, y=800)
        return out

    def work_scope(self, t):
        out = self.header(
            "Keep work in view.",
            "Same moment. Six directories become three. Everything else keeps running.",
        )
        mix = max(0, min(1, (t - KEY_EVENTS["Work"]) / 0.6))
        out += image(self.crop("directories"), 70, 110, 1460, 600)
        if mix:
            out += (
                f'<g opacity="{mix}">'
                + image(self.crop("work-directories"), 70, 110, 1460, 600)
                + "</g>"
            )
        return out + shortcut_overlay(
            ["Cmd", "Shift", "B"],
            "Work only",
            KEY_EVENTS["Work"] <= t < KEY_EVENTS["Work"] + 0.3,
            y=760,
        )

    def zoom_region(self, shot, region, color):
        left, top, right, lower = region
        crop_w = max(1, right - left)
        crop_h = max(1, lower - top)
        card_w = 1320
        card_h = max(72, min(160, card_w * crop_h / crop_w))
        card_w = card_h * crop_w / crop_h
        card_x = (1600 - card_w) / 2
        card_y = 310
        pad = 12
        out = self.clean_zoom(shot, region)
        return out + rect(
            card_x - pad,
            card_y - pad,
            card_w + pad * 2,
            card_h + pad * 2,
            14,
            "none",
            color,
        )

    def scheduled(self, t):
        # Hold Scheduled through the explanation. Resume only after that hold.
        if t < 31.2:
            return self.zoom_region("scheduled", (0, 145, 1616, 250), BLUE)
        jumped = min(1, (t - 31.2) / 0.8)
        clock = shortcut_overlay([], "09:00 · time skip", t >= 31.6, y=800)
        out = self.clean_zoom(
            "continued" if jumped > 0.55 else "scheduled", (0, 145, 1616, 250)
        )
        return out + clock

    def bouncer(self, t):
        # Staged fixture progression: tests running, then the resumed capture.
        shot = "scheduled" if t < 36.2 else "continued"
        out = self.zoom_region(shot, (0, 145, 1616, 280), "#179299")
        label = "checkout tests running" if t < 35 else "Rust build running"
        return out + shortcut_overlay([], label, False, y=800)

    def aligned_narration(self, t):
        for cue in reversed(self.cues):
            if cue["speaker"] != "NARRATOR" or not cue["at"] <= t <= cue["end"] + 0.25:
                continue
            lines = line_wrap(cue["text"])
            width = max(font(23, True).getlength(line) for line in lines)
            return reveal(
                lines,
                self.alignments[cue["file"]],
                t - cue["at"],
                (1600 - width) / 2,
                856,
                23,
            )
        return ""

    def frame(self, t):
        if t >= 40 and self.still is not None:
            return self.still.copy()
        if t < 5.2:
            content = self.intro(t)
        elif t < 8.2:
            content = self.navigation(t)
        elif t < 11.1:
            content = self.voice_intro(t)
        elif t < 16.2:
            content = self.dialogue(t)
        elif t < 21.2:
            content = self.chatgpt(t)
        elif t < 24.3:
            content = self.projects(t)
        elif t < 27:
            content = self.work_scope(t)
        elif t < 34:
            content = self.scheduled(t)
        elif t < 40:
            content = self.bouncer(t)
        else:
            content = super().bento(t)
        if t < 40:
            content = (
                '<g transform="translate(-44 -150) scale(1.055)">' + content + "</g>"
            )
        background = "#e3e5e9" if t >= 40 else "white"
        svg = f'<svg xmlns="http://www.w3.org/2000/svg" width="3840" height="2160" viewBox="0 0 1600 900">{rect(0, 0, 1600, 900, 0, background)}{content}</svg>'
        raw = resvg_py.svg_to_bytes(
            svg_string=svg,
            width=W,
            height=H,
            skip_system_fonts=True,
            font_files=[
                "/System/Library/Fonts/Supplemental/Arial.ttf",
                "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
                "/System/Library/Fonts/Menlo.ttc",
            ],
        )
        im = Image.open(io.BytesIO(raw)).convert("RGB")
        if t >= 40:
            self.still = im.copy()
        return im
