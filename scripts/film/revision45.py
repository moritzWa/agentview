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
# Pressed on the spoken word: "Right", "Left", "Command" K, "Control" S,
# "Command" Shift B. Measured from the final 1.5x takes.
KEY_EVENTS = {
    "Right": 7.0,
    "Left": 7.91,
    "CmdK": 9.95,
    "CtrlS": 23.2,
    "Work": 26.31,
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


# One camera for every Agent View shot: the whole terminal (1616 logical px
# wide, 28 px rows from y=16), same scale and top-left in every beat.
S = 0.75
LX, LY = (1600 - 1616 * S) / 2, 14
LW = 1616 * S
# One position for every shortcut pill, in the clear band under the terminal.
PILL_Y = 806
# Camera scale for the 23-row Scheduled and Heavy jobs captures.
COMPACT_S = 0.965
PILL_H = 80


def row_y(row):
    return 16 + 28 * row


def to_canvas(x, y, w, h):
    return LX + x * S, LY + y * S, w * S, h * S


def ease_in_out(p):
    p = max(0.0, min(1.0, p))
    return p * p * (3 - 2 * p)


def zoomed(content, cx, cy, scale):
    return (
        f'<g transform="translate({cx} {cy}) scale({scale}) translate({-cx} {-cy})">'
        + content
        + "</g>"
    )


def faded(content, opacity):
    if opacity <= 0:
        return ""
    if opacity >= 1:
        return content
    return f'<g opacity="{opacity:.3f}">{content}</g>'


def shortcut_overlay(keys, label, pressed=False, y=PILL_Y):
    """One glass pill, one place. Keys and label share its vertical center."""
    scale = 1.15
    height = PILL_H
    key_width = (
        sum(80 if key == "Ctrl" else 56 for key in keys) * scale
        + max(0, len(keys) - 1) * 12
    )
    gap = 24 if keys else 0
    total = key_width + gap + font(26, True).getlength(label)
    width = max(420, total + 96)
    x = 800 - width / 2
    start = 800 - total / 2
    key_y = y + (height - 52 * scale) / 2
    out = (
        '<g data-shortcut-overlay="shared" data-glass="0.32">'
        + f'<g opacity=".32">{rect(x, y, width, height, 26, "#d8edff")}</g>'
    )
    for key in keys:
        out += keycap(start, key_y, key, pressed, scale)
        start += (80 if key == "Ctrl" else 56) * scale + 12
    label_x = start - 12 + gap if keys else start
    out += txt(label_x, y + height / 2 + 9, label, 26, BLUE, True)
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
        overview = self.shots["overview"]
        self.morph_rows = {
            a["id"]: self._image_uri(
                overview.crop(
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
        # Two real-time runs of the recording. The cut skips the frames where
        # the scrolled setup prompt passes the top of the window.
        self.native_sequence = list(range(66, 120)) + list(range(144, 186))
        self.navigation = json.loads(
            (self.output / "navigation/recording.json").read_text()
        )
        self.still = None
        self.uris = {}
        live = Image.open(self.output / "captures/voice-live.png").convert("RGB")
        # Same fixture state; the call capture had the first row selected.
        # Keep the selection where Right/Left left it (checkout-redesign).
        band = (0, row_y(6) * 3, live.width, row_y(8) * 3)
        live.paste(self.shots["selected"].crop(band), band[:2])
        self.shots["voice-live"] = live

    @staticmethod
    def _image_uri(im):
        out = io.BytesIO()
        im.save(out, format="PNG")
        return "data:image/png;base64," + base64.b64encode(out.getvalue()).decode()

    def cached(self, key, build):
        if key not in self.uris:
            self.uris[key] = self._image_uri(build())
        return self.uris[key]

    # Camera ---------------------------------------------------------------

    def camera(self, shot):
        """(x, y, scale) for a shot. One fixed camera for the full terminal.

        The Scheduled and Heavy jobs captures come from a 23-row terminal; the
        camera eases in on them once, so they fill the frame instead of leaving
        its lower half empty.
        """
        if self.shots[shot].height > 2500:
            return LX, LY, S
        z = ease_in_out((self.t - 29.0) / 0.6)
        scale = S + (COMPACT_S - S) * z
        x = LX + ((1600 - 1616 * COMPACT_S) / 2 - LX) * z
        y = LY + ((900 - 676 * COMPACT_S) / 2 - LY) * z
        return x, y, scale

    def locked(self, shot):
        """The whole terminal at the camera's scale and position."""
        x, y, scale = self.camera(shot)
        source = self.shots[shot]
        return image(
            self.crop(shot), x, y, 1616 * scale, source.height / 3 * scale
        ).replace("xMidYMid meet", "none")

    def blend(self, a, b, mix):
        if mix <= 0:
            return self.locked(a)
        if mix >= 1:
            return self.locked(b)
        return self.locked(a) + faded(self.locked(b), mix)

    def row_box(self, row, color, opacity=1.0, pad=5):
        x, y, w, h = to_canvas(0, row_y(row), 1616, 28)
        return faded(
            f'<rect x="{x - pad}" y="{y - pad}" width="{w + 2 * pad}" height="{h + 2 * pad}" rx="8" fill="none" stroke="{color}" stroke-width="2.5"/>',
            opacity,
        )

    def section_focus(self, shot, rows, color, amount):
        """Whole screen first; then everything outside `rows` blurs and dims."""
        out = self.locked(shot)
        if amount <= 0:
            return out
        source = self.shots[shot]
        cx, cy, scale = self.camera(shot)
        width, height = 1616 * scale, source.height / 3 * scale
        blurred = self.cached(
            ("blur", shot),
            lambda: source.filter(ImageFilter.GaussianBlur(24)),
        )
        out += faded(
            image(blurred, cx, cy, width, height).replace("xMidYMid meet", "none")
            + f'<g opacity=".45">{rect(cx, cy, width, height, 0, "white")}</g>',
            amount,
        )
        top, bottom = row_y(rows[0]) - 6, row_y(rows[1] + 1) + 6
        region = (0, top, 1616, bottom)
        x, y, w, h = cx, cy + top * scale, width, (bottom - top) * scale
        out += image(self.crop(shot, region), x, y, w, h).replace(
            "xMidYMid meet", "none"
        )
        return out + faded(
            f'<rect x="{x - 4}" y="{y - 2}" width="{w + 8}" height="{h + 4}" rx="10" fill="none" stroke="{color}" stroke-width="2.5"/>',
            amount,
        )

    # Beats ----------------------------------------------------------------

    def intro(self, t):
        p = ease_in_out((t - 4.0) / 0.9)
        out = faded(self.locked("overview"), (p - 0.85) / 0.15)
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
            destination = list(to_canvas(*a["cell_bounds"]))
            x, y, w, h = morph_box(a["grid"], destination, p)
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
            name_offset = a["actual_text"].index(a["name"]) * 12 * S
            provider_offset = a["actual_text"].index(label) * 12 * S
            baseline = y + 17 * (1 - p) + 19 * S * p
            size = 11.5 * (1 - p) + 20 * S * p
            out += txt(
                x + 8 * (1 - p) + 12 * S * p,
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
                10.5 * (1 - expand) + 20 * S * expand,
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

    def one_list(self, t):
        # The selection moves to the agent that asked a question.
        return self.blend("overview", "selected", ease_in_out((t - 6.2) / 0.15))

    def session_view(self):
        """The real opened OpenCode session, laid out on the same terminal.

        Its message and its input box are cut from the recorded frame at their
        own rows; the setup prompt, the session title and the workspace path
        are left out.
        """
        frame = next(
            f
            for f in self.navigation["frames"]
            if f["phase"] == "open"
            and "Should the coupon"
            in (self.output / "navigation" / f["file"]).with_suffix(".txt").read_text()
        )
        source = Image.open(self.output / "navigation" / frame["file"]).convert("RGB")

        def message():
            return source.crop((0, row_y(7) * 3, source.width, row_y(10) * 3))

        def composer():
            box = source.crop((0, row_y(21) * 3, source.width, row_y(26) * 3)).copy()
            # Title text on the composer's last line, right of the model name.
            fill = box.getpixel((900 * 3, 10 * 3))
            title_row = (row_y(24) - row_y(21)) * 3
            box.paste(fill, (980 * 3, title_row, box.width - 60, title_row + 28 * 3))
            return box

        top = self.cached(("session", "message"), message)
        bottom = self.cached(("session", "composer"), composer)
        mx, my, mw, mh = to_canvas(0, row_y(1), 1616, row_y(10) - row_y(7))
        # The dashboard's own prompt sits on rows 32-35; the composer lands there.
        bx, by, bw, bh = to_canvas(0, row_y(31), 1616, row_y(26) - row_y(21))
        return image(top, mx, my, mw, mh).replace("xMidYMid meet", "none") + image(
            bottom, bx, by, bw, bh
        ).replace("xMidYMid meet", "none")

    def navigation_beat(self, t):
        right, left = KEY_EVENTS["Right"], KEY_EVENTS["Left"]
        _, cy, _, ch = to_canvas(0, row_y(2), 1616, 28)
        focus_y = cy + ch / 2
        if t < right:
            out = self.locked("selected")
        elif t < left:
            u = ease_in_out((t - right) / 0.3)
            out = faded(
                zoomed(self.locked("selected"), 800, focus_y, 1 + 0.35 * u), 1 - u
            )
            out += faded(zoomed(self.session_view(), 800, focus_y, 0.94 + 0.06 * u), u)
        else:
            u = ease_in_out((t - left) / 0.3)
            out = faded(zoomed(self.session_view(), 800, focus_y, 1 + 0.06 * u), 1 - u)
            out += faded(
                zoomed(self.locked("selected"), 800, focus_y, 1.2 - 0.2 * u), u
            )
        return out + shortcut_overlay(
            ["Right"] if t < left else ["Left"],
            "Open session" if t < left else "Back to the list",
            right <= t < right + 0.22 or left <= t < left + 0.22,
        )

    def voice_intro(self, t):
        key = KEY_EVENTS["CmdK"]
        out = self.blend("selected", "voice-live", ease_in_out((t - key) / 0.2))
        return out + shortcut_overlay(["Cmd", "K"], "Start voice", key <= t < key + 0.3)

    def dialogue(self, t):
        if t >= 16.5:
            return self.payoff(t)
        cue = next(
            c
            for c in self.cues
            if c["file"]
            == (
                "question.mp3"
                if t < 12.75
                else "answer.mp3"
                if t < 15.65
                else "checkout.mp3"
            )
        )
        who = cue["speaker"]
        color = SPEAKER_COLORS[who]
        out = self.locked("voice-live")
        x, y, w, h = 300, 575, 1000, 170
        out += rect(x, y, w, h, 25, "white", "#d3def8") + txt(
            x + 33, y + 33, who, 15, color, True
        )
        out += self.waves(x + 770, y + 29, t, 170, 36, color, speaker=who)
        lines = list(VOICE_CHOICE) if cue["file"] == "answer.mp3" else [cue["text"]]
        out += reveal(
            lines, self.alignments[cue["file"]], t - cue["at"], x + 33, y + 75, 30
        )
        return out

    def payoff(self, t):
        # Staged fixture update: the checkout agent got its answer and works on.
        mix = ease_in_out((t - 16.55) / 0.35)
        out = self.blend("voice-live", "resumed", mix)
        return out + self.row_box(13, BLUE, ease_in_out((t - 16.95) / 0.25))

    def plugin_page(self, t):
        card = self.cached(
            ("plugin",),
            lambda: Image.open(self.output / "captures/agentvoice-plugin-card.png"),
        )
        w = 1240
        h = w * 409 / 1498
        x, y = (1600 - w) / 2, 330
        out = txt(800, 250, "ChatGPT MCP", 44, INK, True, "middle")
        out += txt(
            800, 292, "Connect Agent View to ChatGPT", 24, MUTED, anchor="middle"
        )
        out += rect(x - 24, y - 24, w + 48, h + 48, 28, "#fcfcfc", "#e1e3e8")
        return out + image(card, x, y, w, h)

    def chatgpt_window(self, t):
        n = min(len(self.native_sequence) - 1, max(0, int((t - 19.2) * 30)))
        path = self.native_frames[self.native_sequence[n]]
        # Whole window from its left edge; title bar off; the empty right of
        # the conversation column trimmed so the window can fill the frame.
        data = self.cached(
            ("native", path.name),
            lambda: Image.open(path).convert("RGB").crop((0, 88, 2400, 1262)),
        )
        w = 1560
        h = w * 1174 / 2400
        x, y = (1600 - w) / 2, (900 - h) / 2
        clip = "natclip"
        return (
            f'<defs><clipPath id="{clip}">{rect(x, y, w, h, 18)}</clipPath></defs>'
            + rect(x - 1, y - 1, w + 2, h + 2, 19, "none", "#d9dce3")
            + f'<g clip-path="url(#{clip})">'
            + image(data, x, y, w, h).replace("xMidYMid meet", "none")
            + "</g>"
        )

    def chatgpt(self, t):
        if t < 19.2:
            return self.plugin_page(t)
        return self.chatgpt_window(t)

    def projects(self, t):
        key = KEY_EVENTS["CtrlS"]
        out = self.blend("overview", "directories", ease_in_out((t - key) / 0.25))
        return out + shortcut_overlay(
            ["Ctrl", "S"], "Group by project", key <= t < key + 0.3
        )

    def work_scope(self, t):
        key = KEY_EVENTS["Work"]
        out = self.blend(
            "directories", "work-directories", ease_in_out((t - key) / 0.25)
        )
        return out + shortcut_overlay(
            ["Cmd", "Shift", "B"], "Work only", key <= t < key + 0.3
        )

    def scheduled(self, t):
        # Full screen first, so you see where Scheduled lives; then focus it.
        amount = ease_in_out((t - 29.9) / 0.6)
        jump = ease_in_out((t - 33.0) / 0.35)
        if jump <= 0:
            out = self.section_focus("scheduled", (5, 7), BLUE, amount)
        else:
            out = self.section_focus("scheduled", (5, 7), BLUE, amount) + faded(
                self.section_focus("continued", (5, 7), BLUE, amount), jump
            )
        if 32.4 <= t < 33.6:
            out += shortcut_overlay([], "Tomorrow, 09:00 · time skip", t < 32.55)
        return out

    def bouncer(self, t):
        amount = ease_in_out((t - 35.4) / 0.6)
        return self.section_focus("continued", (16, 18), "#179299", amount)

    def frame(self, t):
        self.t = t
        if t >= 40 and self.still is not None:
            return self.still.copy()
        if t < 4.9:
            content = self.intro(t)
        elif t < 6.9:
            content = self.one_list(t)
        elif t < 8.8:
            content = self.navigation_beat(t)
        elif t < 11.4:
            content = self.voice_intro(t)
        elif t < 17.8:
            content = self.dialogue(t)
        elif t < 23.1:
            content = self.chatgpt(t)
        elif t < 26.0:
            content = self.projects(t)
        elif t < 29.0:
            content = self.work_scope(t)
        elif t < 34.6:
            content = self.scheduled(t)
        elif t < 40:
            content = self.bouncer(t)
        else:
            content = super().bento(t)
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
