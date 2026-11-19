"""Approved45s edit. Reuses captured UI, not new live-account interactions."""

import base64
import io
import json
from pathlib import Path

import numpy as np
import resvg_py
from PIL import Image, ImageFilter
from soundtrack import decode
from visual import (
    BLUE,
    INK,
    MUTED,
    ROOT,
    VOICE_CHOICE,
    Film,
    H,
    W,
    font,
    image,
    rect,
    txt,
    uri,
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
    "CtrlS": 24.0,
    "Work": 26.69,
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


HARNESS_LOOK = {
    "claude": {
        "label": "Claude",
        "title": "Claude Code",
        "glyph": "✳",
        "icon": "claude.svg",
        "sub": "Opus · ~/Code/shop",
        "footer": "▸▸ bypass permissions on (shift+tab to cycle)",
        "accent": "#d20f39",
    },
    "codex": {
        "label": "Codex",
        "title": "OpenAI Codex",
        "glyph": ">_",
        "icon": "openai.png",
        "sub": "gpt-6 · ~/Code/payments",
        "footer": "⏎ send   ⇧⏎ newline   esc interrupt",
        "accent": "#6c6f85",
    },
    "opencode": {
        "label": "OpenCode",
        "title": "OpenCode",
        "glyph": "◧",
        "icon": "opencode.svg",
        "sub": "Build · GPT-6.1",
        "footer": "Build · GPT-6.1 Sol   ctrl+p commands",
        "accent": "#fe640b",
    },
    "cursor": {
        "label": "Cursor",
        "title": "Cursor Agent",
        "glyph": "⬡",
        "icon": "cursor.svg",
        "sub": "auto · ~/Code/docs",
        "footer": "Auto-run · ctrl+c to stop",
        "accent": "#6c6f85",
    },
    "devin": {
        "label": "Devin",
        "title": "Devin",
        "glyph": "◆",
        "icon": "devin.png",
        "sub": "~/Code/shop",
        "footer": "devin › working",
        "accent": "#179299",
    },
}


def chat_script(a):
    name = a["name"]
    area = name.split("-")[0]
    lines = [
        f"> fix {name.replace('-', ' ')} and run the tests",
        f"● Reading src/{area}/index.ts",
        "  └ 214 lines",
        f"● Update(src/{area}/{name}.ts)",
        "  └ +18 −6",
        f"● Bash(pnpm test {area})",
        "  └ 41 passed · 1 failed",
        f"● Update(src/{area}/{name}.test.ts)",
        "  └ +4 −1",
        f"● Bash(pnpm test {area})",
        "  └ 42 passed",
    ]
    if a["state"] == "needs_input":
        lines.append("? " + a["summary"])
    else:
        lines.append("• " + a["summary"] + "…")
    return lines


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
        self.native_sequence = list(range(166, 186))
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

    def pane(self, i, a, x, y, w, h, t):
        """One agent window: title bar, a chat streaming in, prompt, footer."""
        harness = HARNESS_LOOK[a["provider"]]
        out = rect(x, y, w, h, 10, "white", "#c9ced8")
        out += rect(x, y, w, 22, 10, "#f3f4f7") + rect(x, y + 12, w, 10, 0, "#f3f4f7")
        out += f'<line x1="{x}" y1="{y + 22}" x2="{x + w}" y2="{y + 22}" stroke="#e1e4ea"/>'
        lights = ["#ff5f57", "#febc2e", "#28c840"] if i == 5 else ["#d5d8de"] * 3
        for k, color in enumerate(lights):
            out += (
                f'<circle cx="{x + 12 + k * 13}" cy="{y + 11}" r="4" fill="{color}"/>'
            )
        out += txt(
            x + 56,
            y + 15,
            f"{harness['title']} · {a['name']}",
            10,
            MUTED,
            True,
        )
        out += image(
            uri(ROOT / "site/providers" / harness["icon"]), x + 10, y + 30, 22, 22
        )
        out += txt(x + 40, y + 41, harness["title"], 10, INK, True, mono=True)
        out += txt(x + 40, y + 53, harness["sub"], 8.5, MUTED, mono=True)
        # The conversation types in, each window at its own pace, and scrolls.
        script = chat_script(a)
        typed = 36 + max(0.0, t + 0.3 * (i % 5)) * (58 + 14 * (i % 4))
        shown, budget = [], typed
        for line in script:
            if budget <= 0:
                break
            shown.append(line[: int(budget)])
            budget -= len(line) + 6
        body_top, line_h = y + 70, 12
        rows = int((h - 70 - 46) / line_h)
        for k, line in enumerate(shown[-rows:]):
            asking = a["state"] == "needs_input" and line.startswith("? ")
            color = (
                "#df8e1d"
                if asking
                else INK
                if line.startswith(("> ", "●", "•"))
                else MUTED
            )
            out += txt(x + 10, body_top + k * line_h, line[:58], 8.6, color, mono=True)
        base = y + h - 38
        out += f'<line x1="{x + 8}" y1="{base}" x2="{x + w - 8}" y2="{base}" stroke="#d5d8de"/>'
        out += txt(x + 10, base + 14, "❯", 9, INK, mono=True)
        if int(t * 2.5) % 2 and a["state"] == "needs_input":
            out += rect(x + 22, base + 5, 6, 11, 0, "#df8e1d")
        out += f'<line x1="{x + 8}" y1="{base + 21}" x2="{x + w - 8}" y2="{base + 21}" stroke="#d5d8de"/>'
        out += txt(
            x + 10, base + 33, harness["footer"], 8.2, harness["accent"], mono=True
        )
        return out

    def intro(self, t):
        """Sixteen agent windows, then each one shrinks into its row."""
        p = ease_in_out((t - 4.0) / 0.9)
        gap = 14
        gw, gh = (1600 - 5 * gap) / 4, (900 - 5 * gap) / 4
        out = ""
        if p > 0:
            out += faded(self.locked("overview"), ease_in_out((p - 0.35) / 0.5))
        for i, a in enumerate(self.mapping):
            col, row = i % 4, i // 4
            gx, gy = gap + col * (gw + gap), gap + row * (gh + gap)
            if p <= 0:
                out += self.pane(i, a, gx, gy, gw, gh, t)
                continue
            dx, dy, dw, dh = to_canvas(*a["cell_bounds"])
            # Uniform scale only: the window keeps its shape while it flies
            # to its row's centre and shrinks to the row's height.
            scale = 1 + (dh * 2.2 / gh - 1) * p
            cx = gx + gw / 2 + (dx + dw * 0.18 - gx - gw / 2) * p
            cy = gy + gh / 2 + (dy + dh / 2 - gy - gh / 2) * p
            out += faded(
                f'<g transform="translate({cx} {cy}) scale({scale}) translate({-(gx + gw / 2)} {-(gy + gh / 2)})">'
                + self.pane(i, a, gx, gy, gw, gh, t)
                + "</g>",
                1 - ease_in_out((p - 0.3) / 0.5),
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
            return source.crop((30 * 3, row_y(7) * 3, 1590 * 3, row_y(10) * 3))

        def composer():
            box = source.crop((30 * 3, row_y(21) * 3, 1590 * 3, row_y(25) * 3)).copy()
            # The session title sits on the composer's last text row, right of
            # the model name. Clear it with the composer's own fill, inside the
            # composer only, so its right edge and border stay intact.
            title_top = (row_y(24) - row_y(21)) * 3
            fill = box.getpixel((1400 * 3, title_top + 14 * 3))
            box.paste(fill, (870 * 3, title_top, 1545 * 3, title_top + 28 * 3))
            return box

        top = self.cached(("session", "message2"), message)
        bottom = self.cached(("session", "composer4"), composer)
        z = 1500 / 1560
        width = 1560 * z
        x = (1600 - width) / 2
        size, line = 20 * z, 28 * z
        # Earlier turns of the same checkout conversation, in OpenCode's layout.
        history = [
            ("prompt", "Move checkout to one page and keep the tests green."),
            ("gap", ""),
            ("tool", "→ Read src/checkout/Cart.tsx"),
            ("tool", "→ Read src/checkout/Checkout.tsx"),
            ("tool", "← Edit src/checkout/Checkout.tsx   +42 −11"),
            ("tool", "$ pnpm test checkout                 38 passed"),
            ("gap", ""),
            ("text", "Checkout is now one page. The coupon field still lives on"),
            ("text", "the cart page, so the totals can disagree."),
        ]
        y0 = 160
        tx = x + 36 * z + 9
        out = ""
        for k, (kind, text) in enumerate(history):
            ly = y0 + k * line
            if kind == "prompt":
                out += rect(x + 14, ly - 8, width - 14, line + 16, 0, "#f0f0f0")
                out += rect(x + 14, ly - 8, 4, line + 16, 0, "#e0784a")
                out += txt(tx, ly + size * 0.95, text, size, INK, mono=True)
            elif kind == "tool":
                out += txt(tx, ly + size * 0.95, text, size, MUTED, mono=True)
            elif kind == "text":
                out += txt(tx, ly + size * 0.95, text, size, INK, mono=True)
        my = y0 + len(history) * line + line * 0.6
        message_h = (row_y(10) - row_y(7)) * z
        composer_h = (row_y(25) - row_y(21)) * z
        out += image(top, x, my, width, message_h).replace("xMidYMid meet", "none")
        out += image(bottom, x, my + message_h + line, width, composer_h).replace(
            "xMidYMid meet", "none"
        )
        return out

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
        push = 1 + 0.05 * ease_in_out((t - 17.8) / 2.4)
        out = f'<g transform="translate(800 450) scale({push}) translate(-800 -450)">'
        out += txt(800, 250, "ChatGPT MCP", 44, INK, True, "middle")
        out += txt(
            800, 292, "Connect Agent View to ChatGPT", 24, MUTED, anchor="middle"
        )
        out += rect(x - 24, y - 24, w + 48, h + 48, 28, "#fcfcfc", "#e1e3e8")
        return out + image(card, x, y, w, h) + "</g>"

    def chatgpt_window(self, t):
        # Real recording, real speed: the answer arrives, then the reply is
        # typed and sent. Cut between the two takes, never retimed.
        n = min(len(self.native_sequence) - 1, max(0, int((t - 20.2) * 30)))
        path = self.native_frames[self.native_sequence[n]]
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

    def bridge_diagram(self, t):
        """How a reply reaches an offline Mac: phone, cloud queue, Mac."""
        appear = ease_in_out((t - 20.85) / 0.3)
        sent = ease_in_out((t - 21.2) / 0.5)  # reply: phone -> queue
        online = ease_in_out((t - 22.75) / 0.3)  # the Mac comes back
        delivered = ease_in_out((t - 22.95) / 0.5)  # queue -> Mac
        teal = "#179299"
        grey = "#9ca0b0"
        out = ""
        boxes = [
            (110, "ChatGPT voice", "your phone", BLUE),
            (600, "AgentVoice MCP", "cloud", teal),
            (1090, "Agent View", "your Mac", BLUE if online >= 0.5 else grey),
        ]
        bw, bh, by = 400, 250, 300
        for x, title, sub, color in boxes:
            out += rect(x, by, bw, bh, 26, "white", color)
            out += txt(x + bw / 2, by + 62, title, 34, INK, True, "middle")
            out += txt(x + bw / 2, by + 98, sub, 22, MUTED, anchor="middle")
        # Mac status
        status = "online" if online >= 0.5 else "offline"
        out += txt(
            1290,
            by + 136,
            "● " + status,
            22,
            BLUE if online >= 0.5 else grey,
            True,
            "middle",
        )
        # Queue inside the cloud box
        out += rect(640, by + 140, 320, 76, 16, "#eef7f7", teal)
        out += txt(800, by + 125, "queued messages", 18, teal, True, "middle")
        # Arrows
        for x1, x2 in [(510, 600), (1000, 1090)]:
            out += (
                f'<line x1="{x1 + 8}" y1="{by + bh / 2}" x2="{x2 - 14}" y2="{by + bh / 2}" '
                f'stroke="{MUTED}" stroke-width="3"/>'
                f'<path d="M{x2 - 16} {by + bh / 2 - 9} L{x2 - 4} {by + bh / 2} L{x2 - 16} {by + bh / 2 + 9}z" fill="{MUTED}"/>'
            )
        # The reply chip travels phone -> queue (waits) -> Mac.
        chip_w, chip_h = 230, 44
        start_x, queue_x, mac_x = 195, 685, 1175
        cx = start_x + (queue_x - start_x) * sent + (mac_x - queue_x) * delivered
        cy = by + 156 if delivered <= 0 else by + 156 + 0 * delivered
        out += rect(cx, cy, chip_w, chip_h, 22, "#11121b")
        out += txt(
            cx + chip_w / 2, cy + 29, "Tell it checkout.", 20, "white", True, "middle"
        )
        out += txt(
            800,
            690,
            "Your Mac picks up queued replies when it comes back online.",
            24,
            MUTED,
            anchor="middle",
        )
        return faded(out, appear)

    def chatgpt(self, t):
        if t < 20.2:
            return self.plugin_page(t)
        if t < 20.85:
            return self.chatgpt_window(t)
        return faded(
            self.chatgpt_window(t), 1 - ease_in_out((t - 20.85) / 0.3)
        ) + self.bridge_diagram(t)

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
        elif t < 23.9:
            content = self.chatgpt(t)
        elif t < 26.3:
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
