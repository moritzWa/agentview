"""Approved45s edit. Reuses captured UI, not new live-account interactions."""

import base64
import io
import json

import resvg_py
from PIL import Image
from propose45 import morph_box, timed_words
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
    keycaps,
    mark,
    navigation_uri,
    rect,
    rounded_image,
    txt,
)
from waveform import VoiceTracks, envelope

SECONDS = 45
PURPLE = "#6f38c5"
SPEAKER_COLORS = {"YOU": PURPLE, "AGENT VIEW": BLUE, "NARRATOR": INK}
KEY_EVENTS = {
    "Right": 9.286,
    "Left1": 11.9,
    "Left2": 12.05,
    "CtrlS": 29.5,
    "Work": 32.8,
}


def keycap(x, y, label, pressed=False, scale=1):
    width = 96 if label == "Ctrl" else 72
    color = "white" if pressed else INK
    face = BLUE if pressed else "white"
    out = (
        f'<g transform="translate({x} {y}) scale({scale})">'
        + rect(0, 5, width, 66, 12, "#b8c5df")
        + rect(0, 0, width, 66, 12, face)
        + rect(3, 3, width - 6, 60, 9, face)
    )
    if label in ["Right", "Left", "Down"]:
        transform = (
            "scale(-1 1)"
            if label == "Left"
            else "rotate(90)"
            if label == "Down"
            else ""
        )
        out += f'<g transform="translate({width / 2} 33) {transform}" fill="none" stroke="{color}" stroke-width="3.5" stroke-linecap="round" stroke-linejoin="round"><path d="M-18 0H18M6-12L18 0 6 12"/></g>'
    else:
        out += txt(width / 2, 43, label, 27, color, True, "middle")
    return out + "</g>"


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
    def __init__(self, output):
        super().__init__(output)
        self.plan = json.loads(
            (self.output / "proposal45/script-timeline.json").read_text()
        )
        self.cues = self.plan["cues"]
        self.alignments = {
            c["file"]: timed_words(
                json.loads(
                    (
                        self.output
                        / "proposal45/audio"
                        / c["file"].replace(".mp3", ".alignment.json")
                    ).read_text()
                ),
                c["text"],
            )
            for c in self.cues
        }
        self.voice_tracks = VoiceTracks(
            [
                {
                    "at": c["at"],
                    "speaker": c["speaker"],
                    "levels": envelope(
                        decode(self.output / "proposal45/audio" / c["file"])
                    ),
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
        self.native_frames = sorted(
            (self.output / "chatgpt-desktop/frames45").glob("*.png")
        )
        assert len(self.native_frames) >= 180, "Prepared native6s excerpt missing"
        self.still = None

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
        p = max(0, min(1, (t - 2.5) / 3.5))
        p = p * p * (3 - 2 * p)
        out = self.header(
            "Too many agents. Too many windows."
            if t < 2.5
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
        right, left1, left2 = (
            KEY_EVENTS["Right"],
            KEY_EVENTS["Left1"],
            KEY_EVENTS["Left2"],
        )
        out = self.header(
            "Zoom into the session that needs you.",
            "Right opens. Double-tap Left to return.",
        )
        if t < right or t >= 12.5:
            out += self.status_image(selected=8.6 <= t < right)
        else:
            phase = "open" if t < left1 else "return"
            frames = [f for f in self.navigation_meta["frames"] if f["phase"] == phase]
            progress = (
                (t - right) / (left1 - right) if phase == "open" else (t - left1) / 0.6
            )
            n = min(len(frames) - 1, max(0, int(progress * len(frames))))
            out += image(
                navigation_uri(
                    self.output / "navigation" / frames[n]["file"], phase == "open"
                ),
                70,
                200,
                1460,
                620,
            )
        if t < left1:
            out += keycap(1120, 744, "Right", right <= t < right + 0.22) + txt(
                1215, 789, "Open session", 26, BLUE, True
            )
        else:
            out += (
                keycap(1000, 744, "Left", left1 <= t < left1 + 0.12)
                + keycap(1086, 744, "Left", left2 <= t < left2 + 0.18)
                + txt(1183, 789, "Return", 26, BLUE, True)
            )
        return out

    def dialogue(self, t):
        cue = next(
            c
            for c in self.cues
            if c["file"]
            == (
                "question.mp3"
                if t < 16.2
                else "answer.mp3"
                if t < 20.6
                else "checkout.mp3"
            )
        )
        who = cue["speaker"]
        color = SPEAKER_COLORS[who]
        out = self.header(
            "One terminal. One voice.", "Ask once. Unblock the right agent."
        ) + self.terminal("resumed" if t >= 21.7 else "overview")
        out += rect(300, 614, 1000, 192, 25, "white", "#d3def8") + txt(
            333, 645, who, 15, color, True
        )
        out += self.waves(1070, 641, t, 170, 36, color, speaker=who)
        lines = list(VOICE_CHOICE) if cue["file"] == "answer.mp3" else [cue["text"]]
        out += reveal(lines, self.alignments[cue["file"]], t - cue["at"], 333, 687, 30)
        return out + txt(
            333,
            782,
            "Editorial voiceover · synthetic app state, not live microphone input",
            14,
            MUTED,
        )

    def chatgpt(self, t):
        out = self.header(
            "Your agents in ChatGPT.",
            "Connect through an authenticated AgentVoice MCP connection.",
        )
        out += rect(70, 200, 1460, 610, 24)
        n = min(len(self.native_frames) - 1, max(0, int((t - 22.5) * 30)))
        out += rounded_image(self.native_frames[n], 535, 212, 980, 586, 12)
        out += txt(110, 270, "CONNECTION CONCEPT · EDITORIAL", 13, MUTED, True)
        out += mark(260, 355, 1.4) + txt(
            260, 455, "Agent View", 34, INK, True, "middle"
        )
        out += txt(260, 500, "ChatGPT + AgentVoice", 25, BLUE, True, "middle")
        out += txt(260, 541, "Authorized MCP connection", 20, INK, anchor="middle")
        out += txt(260, 581, "Read and reply from anywhere", 19, MUTED, anchor="middle")
        out += txt(260, 617, "Existing OpenCode sessions", 17, MUTED, anchor="middle")
        out += txt(
            260, 691, "Actual native Voice recording →", 16, BLUE, True, "middle"
        )
        return out + txt(
            260,
            729,
            "Typed prompts. Not a new sign-in flow.",
            14,
            MUTED,
            anchor="middle",
        )

    def projects(self, t):
        out = self.header(
            "Status or project. Your view.",
            "Control S changes grouping, not which sessions are shown.",
        )
        out += image(
            self.crop("overview" if t < KEY_EVENTS["CtrlS"] else "directories"),
            70,
            200,
            1460,
            620,
        )
        pressed = KEY_EVENTS["CtrlS"] <= t < KEY_EVENTS["CtrlS"] + 0.3
        out += (
            keycap(1040, 739, "Ctrl", pressed)
            + keycap(1149, 739, "S", pressed)
            + txt(1245, 783, "Switch view", 25, BLUE, True)
        )
        return out

    def work_scope(self, t):
        out = self.header(
            "Keep work in view.",
            "Same moment. Six directories become three. Everything else keeps running.",
        )
        out += image(
            self.crop("directories" if t < KEY_EVENTS["Work"] else "work-directories"),
            70,
            200,
            1460,
            620,
        )
        out += (
            '<g opacity=".72">'
            + rect(478, 733, 644, 100, 26, "#d8edff")
            + "</g>"
            + keycaps(510, 747, 1.25)
            + txt(1083, 793, "Work only", 30, BLUE, True, "end")
        )
        return out + txt(
            800,
            825,
            "Configured global shortcut for this setup",
            13,
            MUTED,
            anchor="middle",
        )

    def scheduled(self, t):
        jumped = t >= 37.8
        out = self.header(
            "Review checkout tomorrow at 9.",
            "Tomorrow 09:00 · staged time jump to background review."
            if jumped
            else "A follow-up you don’t have to remember. Synthetic schedule and queue.",
        )
        return (
            out
            + self.terminal("continued" if jumped else "scheduled")
            + txt(1500, 63, "agent-bouncer", 22, "#179299", True, "end")
        )

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
        if t < 7:
            content = self.intro(t)
        elif t < 14:
            content = self.navigation(t)
        elif t < 22.5:
            content = self.dialogue(t)
        elif t < 28.5:
            content = self.chatgpt(t)
        elif t < 32:
            content = self.projects(t)
        elif t < 35:
            content = self.work_scope(t)
        elif t < 40:
            content = self.scheduled(t)
        else:
            content = super().bento(t)
        if t < 40:
            content += self.aligned_narration(t)
            content += txt(
                70,
                897,
                "REVIEW · Real app UI, synthetic sessions · editorial narration · native Voice uses typed prompts",
                10,
                MUTED,
            )
        background = "#e3e5e9" if t >= 40 else "#f0f1f5"
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
