"""Native 4K SVG composition. Every glyph and shape is rasterized at delivery size."""

import base64
import io
import json
from functools import lru_cache
from html import escape
from pathlib import Path

import resvg_py
from PIL import Image, ImageFont
from waveform import VoiceTracks

ROOT = Path(__file__).resolve().parents[2]
W, H, FPS, SECONDS = 3840, 2160, 30, 30
BLUE, INK, MUTED = "#1e66f5", "#11121b", "#6c6f85"
VOICE_CHOICE = (
    "Your checkout agent asked whether coupons",
    "belong in the cart or at checkout.",
)
HARNESSES = [
    ("Claude Code", "claude.svg"),
    ("OpenAI Codex", "openai.png"),
    ("Pi", "pi.svg"),
    ("OpenCode", "opencode.svg"),
    ("Cursor", "cursor.svg"),
    ("GitHub Copilot", "copilot.svg"),
    ("Antigravity", "antigravity.svg"),
    ("Mistral Vibe", "mistral-vibe.svg"),
    ("Muse Code", "muse.svg"),
    ("Qwen Code", "qwen.svg"),
    ("Kimi Code", "kimi.png"),
    ("Oh My Pi", "oh-my-pi.svg"),
    ("Grok", "grok.svg"),
    ("Kilo Code", "kilo-code.svg"),
    ("OpenHands", "openhands.svg"),
    ("Hermes Agent", "hermes.png"),
    ("MastraCode", "mastra.png"),
    ("Devin", "devin.png"),
]


def ease(t):
    return 1 - (1 - max(0, min(1, t))) ** 3


def mix(a, b, t):
    return a + (b - a) * t


@lru_cache(maxsize=32)
def font(size, bold=False, mono=False):
    p = (
        "/System/Library/Fonts/Menlo.ttc"
        if mono
        else "/System/Library/Fonts/Supplemental/Arial"
        + (" Bold" if bold else "")
        + ".ttf"
    )
    return ImageFont.truetype(p, size)


@lru_cache(maxsize=24)
def uri(path):
    path = Path(path)
    mime = "image/svg+xml" if path.suffix == ".svg" else "image/png"
    return f"data:{mime};base64," + base64.b64encode(path.read_bytes()).decode()


@lru_cache(maxsize=24)
def navigation_uri(path, focus_question):
    if not focus_question:
        return uri(path)
    with Image.open(path) as source:
        # Framing only: omit the long synthetic setup above the real answer.
        source = source.crop((0, 636, source.width, source.height))
        out = io.BytesIO()
        source.save(out, format="PNG")
    return "data:image/png;base64," + base64.b64encode(out.getvalue()).decode()


@lru_cache(maxsize=24)
def provider(name, size):
    p = ROOT / "site/providers" / name
    if p.suffix == ".svg":
        return Image.open(
            io.BytesIO(resvg_py.svg_to_bytes(svg_path=str(p), width=size, height=size))
        ).convert("RGBA")
    im = Image.open(p).convert("RGBA")
    im.thumbnail((size, size), Image.Resampling.LANCZOS)
    return im


def rect(x, y, w, h, r=24, fill="white", stroke="none"):
    return f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}" fill="{fill}" stroke="{stroke}"/>'


def txt(x, y, s, size=26, color=INK, bold=False, anchor="start", mono=False):
    return f'<text x="{x}" y="{y}" font-family="{("Menlo" if mono else "Arial")}" font-size="{size}" font-weight="{700 if bold else 400}" text-anchor="{anchor}" fill="{color}">{escape(s)}</text>'


def image(data, x, y, w, h):
    return f'<image href="{data}" x="{x}" y="{y}" width="{w}" height="{h}" preserveAspectRatio="xMidYMid meet"/>'


def rounded_image(path, x, y, w, h, radius=15):
    with Image.open(path) as source:
        scale = min(w / source.width, h / source.height)
        iw, ih = source.width * scale, source.height * scale
    ix, iy = x + (w - iw) / 2, y + (h - ih) / 2
    key = f"photo{abs(hash((str(path), x, y, w, h)))}"
    return (
        f'<defs><clipPath id="{key}">{rect(ix, iy, iw, ih, radius)}</clipPath></defs><g clip-path="url(#{key})">'
        + image(uri(path), ix, iy, iw, ih)
        + "</g>"
    )


def mark(x, y, s=1, color=BLUE):
    # Embed the source, without rebuilding circles, changing aspect or colors.
    return image(
        uri(ROOT / "docs/assets/logo.svg"), x - 32 * s, y - 40 * s, 64 * s, 80 * s
    )


def keycaps(x, y, scale=1):
    # Vector symbols avoid missing-glyph fallbacks. Each key has a concentric
    # inset face: outer radius 10, inset 3, inner radius 7.
    out = f'<g transform="translate({x} {y}) scale({scale})">'
    for i in range(3):
        a = i * 67
        out += (
            rect(a, 3, 56, 54, 10, "#cdd1db")
            + rect(a, 0, 56, 52, 10, "#f9fafc", "#d9dde6")
            + rect(a + 3, 3, 50, 46, 7, "white")
        )
    # Command loop, Shift arrow and the B glyph.
    out += '<g fill="none" stroke="#30343f" stroke-width="2.6"><path d="M21 19h14v14H21z M21 19v-5a5 5 0 1 0-5 5h5 M35 19h5a5 5 0 1 0-5-5v5 M35 33v5a5 5 0 1 0 5-5h-5 M21 33h-5a5 5 0 1 0 5 5v-5"/><path d="M95 13l-13 14h8v12h10V27h8z" stroke-linejoin="round"/></g>'
    out += txt(162, 36, "B", 28, "#30343f", True, "middle") + "</g>"
    return out


class Film:
    def __init__(self, output):
        self.output = Path(output)
        self.shots = {
            n: Image.open(self.output / "captures" / f"{n}.png").convert("RGB")
            for n in [
                "overview",
                "resumed",
                "work",
                "scheduled",
                "continued",
                "directories",
                "work-directories",
                "selected",
            ]
        }
        self.rows = json.loads((self.output / "captures/layout.json").read_text())
        self.crops = {}
        self.still = None
        self.voice_tracks = VoiceTracks.load(self.output / "audio")

    def crop(self, shot, region=None):
        key = (shot, region)
        if key not in self.crops:
            im = self.shots[shot]
            if region:
                im = im.crop(tuple(round(v * 3) for v in region))
            buf = io.BytesIO()
            im.save(buf, format="PNG")
            self.crops[key] = (
                "data:image/png;base64," + base64.b64encode(buf.getvalue()).decode()
            )
        return self.crops[key]

    def terminal(self, shot, x=70, y=194, w=1460, h=620, region=None):
        data = self.crop(shot, region)
        native = self.shots[shot]
        logical = (
            (region[2] - region[0], region[3] - region[1])
            if region
            else (native.width / 3, native.height / 3)
        )
        img_h = (w - 20) * logical[1] / logical[0]
        clip = f"c{abs(hash((shot, x, y, w, h, region)))}"
        return (
            f'<defs><clipPath id="{clip}">{rect(x, y, w, h, 20)}</clipPath></defs>'
            + rect(x, y + 7, w, h, 20, "#e3e6ee")
            + f'<g clip-path="url(#{clip})">'
            + rect(x, y, w, h, 20)
            + rect(x, y, w, 32, 0, "#f0f1f5")
            + "".join(
                f'<circle cx="{x + 19 + i * 15}" cy="{y + 16}" r="4" fill="{c}"/>'
                for i, c in enumerate(["#eba49b", "#e8cc8a", "#a6cca3"])
            )
            + txt(x + w / 2, y + 21, "Agent View", 12, MUTED, anchor="middle")
            + image(data, x + 10, y + 36, w - 20, img_h)
            + "</g>"
        )

    def header(self, title, sub=""):
        return (
            mark(78, 53, 0.6)
            + txt(110, 62, "Agent View", 23, bold=True)
            + txt(70, 135, title, 48, bold=True)
            + txt(72, 174, sub, 23, MUTED)
        )

    def waves(self, x, y, t, w=180, h=35, color=BLUE, speaker="AGENT VIEW"):
        levels = self.voice_tracks.at(t, speaker)
        return "".join(
            rect(x + i * w / 31, y - (a := 2 + float(level) * h) / 2, 3, a, 1.5, color)
            for i, level in enumerate(levels)
        )

    def opening(self, t):
        out = self.header("All your coding agents.", "One terminal. One voice.")
        p = ease((t - 1.7) / 0.8)
        out += f'<g opacity="{p}">' + self.terminal("overview") + "</g>"
        logs = [
            "$ pnpm test checkout --watch",
            "  PASS  payment.spec.ts (23 tests)",
            "  compiling src/routes/cart.tsx…",
            "@@ -38,7 +38,11 @@ checkout",
            "+ const total = applyCoupon(cart);",
            "  tool: inspect failing request",
            "  GET /api/checkout 200  182ms",
            "  retrying flaky suite (3 of 20)",
            "  ███████░░░ 72% building",
            "  watching 184 changed files",
            "  npm run typecheck -- --strict",
            "  → tracing webhook retries",
            "  ✓ migrated 12 schema changes",
            '  import { Checkout } from "./ui";',
        ]
        for i in range(16):
            name, asset = HARNESSES[i]
            x, y = 70 + (i % 4) * 366, 200 + (i // 4) * 153
            xx, yy = mix(x, 80, p), mix(y, 240 + (i % 16) * 19, p)
            pane = rect(0, 0, 360, 147, 3, "#fff", "#aeb4c1") + rect(
                0, 0, 360, 23, 3, "#e1e4eb"
            )
            pane += image(uri(ROOT / "site/providers" / asset), 8, 5, 13, 13) + txt(
                27,
                16,
                name + "  / " + ["shop", "payments", "docs", "home-lab"][i % 4],
                11,
                bold=True,
            )
            pane += txt(
                344, 16, "●", 11, "#179299" if i != 10 else "#ba7600", True, "end"
            )
            shift = (int(t * (3 + i % 3)) + i * 3) % len(logs)
            for j in range(8):
                line = logs[(shift + j) % len(logs)][:49]
                if i == 10 and j == 5:
                    line = "? NEEDS INPUT: where should coupons go?"
                pane += txt(
                    8,
                    36 + j * 13,
                    line,
                    10.2,
                    "#a56800"
                    if i == 10 and j == 5
                    else ["#343744", "#6d7280", "#176d89"][j % 3],
                    mono=True,
                )
            out += f'<g opacity="{1 - p}" transform="translate({xx} {yy}) scale({mix(1, 0.22, p)} {mix(1, 0.10, p)})">{pane}</g>'
        out += txt(
            800,
            860,
            "16 busy terminals. One buried decision."
            if t < 1.7
            else "Everything organized by who needs you.",
            22,
            MUTED,
            anchor="middle",
        )
        return out

    def navigation(self, t):
        out = self.header(
            "Group agents by status.", "Use the arrows to open the one that needs you."
        )
        if t < 3.6 or t >= 6.05:
            return (
                out
                + self.terminal("selected" if 3.1 <= t < 3.6 else "overview")
                + txt(
                    1450,
                    840,
                    "↓ select" if t < 3.6 else "← ← back",
                    26,
                    BLUE,
                    True,
                    "end",
                )
            )
        meta = json.loads((self.output / "navigation/recording.json").read_text())
        phase = "open" if t < 5.85 else "return"
        frames = [f for f in meta["frames"] if f["phase"] == phase]
        progress = (
            0.25 + 0.75 * (t - 3.6) / 2.25
            if phase == "open"
            else 0.8 + 0.2 * (t - 5.85) / 0.2
        )
        n = min(len(frames) - 1, max(0, int(progress * len(frames))))
        out += image(
            navigation_uri(
                self.output / "navigation" / frames[n]["file"], phase == "open"
            ),
            70,
            190,
            1460,
            620,
        )
        out += txt(
            1450,
            850,
            "→ open session" if phase == "open" else "← ← back to overview",
            28,
            BLUE,
            True,
            "end",
        )
        return out + txt(
            70,
            850,
            "Actual keyboard recording · dedicated synthetic session",
            16,
            MUTED,
        )

    def voice(self, t):
        resumed = t >= 14.65
        out = self.header(
            "One terminal. One voice.",
            "Ask once. Unblock the right agent.",
        ) + self.terminal("resumed" if resumed else "overview")
        if t < 9:
            who, lines = "YOU", ["Which sessions need my attention?"]
        elif t < 13.4:
            who, lines = "AGENT VIEW", VOICE_CHOICE
        else:
            who, lines = "YOU", ["Tell it checkout."]
        out += (
            rect(300, 614, 1000, 192, 25, "white", "#d3def8")
            + txt(333, 645, who, 15, BLUE, True)
            + self.waves(1070, 641, t, 170, 23, speaker=who)
        )
        for i, line in enumerate(lines):
            out += txt(333, 687 + i * 37, line, 30, bold=True)
        out += txt(
            333,
            782,
            "Staged state change · not a live call or real send"
            if resumed
            else "Illustrated desktop voice exchange",
            15,
            MUTED,
        )
        return out

    def chatgpt(self, t):
        out = self.header(
            "ChatGPT Voice, in the desktop app.",
            "Actual VM recording · typed prompts in Voice · dedicated synthetic OpenCode target",
        )
        clip = self.output / "chatgpt-desktop" / "frames"
        frames = sorted(clip.glob("*.png")) if clip.exists() else []
        if frames:
            n = min(len(frames) - 1, max(0, int((t - 15) / 3 * len(frames))))
            out += rect(70, 200, 1460, 610, 24) + rounded_image(
                frames[n], 82, 212, 1436, 586, 12
            )
            out += txt(
                800,
                850,
                "Real AgentVoice integration. No private sessions shown.",
                20,
                MUTED,
                anchor="middle",
            )
        else:
            # Never silently substitute a concept phone for requested real proof.
            out += self.terminal("resumed", 70, 200, 1460, 610)
            out += (
                rect(450, 490, 700, 150, 24, "#fff8ed")
                + txt(
                    800,
                    548,
                    "Actual desktop recording is blocked",
                    29,
                    INK,
                    True,
                    "middle",
                )
                + txt(
                    800,
                    589,
                    "VM microphone: “already in use”. No mockup substituted.",
                    22,
                    MUTED,
                    anchor="middle",
                )
            )
        return out

    def work(self, t):
        changing = t < 19.4
        out = self.header(
            "Two views. One busy dashboard." if changing else "Work stays in view.",
            "Ctrl+S: status ↔ directory. This does not filter sessions."
            if changing
            else "Same moment. Six directories become three. Personal work keeps running.",
        ) + self.terminal(
            "overview"
            if t < 18.4
            else "directories"
            if t < 19.7
            else "work-directories"
        )
        out += (
            '<g opacity=".72">'
            + rect(480, 728, 640, 110, 28, "#d8edff")
            + "</g>"
            + (
                txt(800, 797, "Ctrl + S   status ↔ directory", 30, BLUE, True, "middle")
                if changing
                else keycaps(510, 746, 1.35)
                + txt(1085, 797, "Work only", 30, BLUE, True, "end")
            )
        )
        return (
            out
            if changing
            else out
            + txt(
                800,
                860,
                "Global shortcut configured for this setup",
                15,
                MUTED,
                anchor="middle",
            )
        )

    def continuation(self, t):
        jumped = t >= 23
        out = self.header(
            "Review checkout tomorrow at 9.",
            "The same session wakes up and continues."
            if jumped
            else "A follow-up you don’t have to remember.",
        )
        out += self.terminal("continued" if jumped else "scheduled")
        # Editorial annotation, not a fabricated scheduling or queue control.
        out += txt(1500, 63, "agent-bouncer", 22, "#179299", True, "end")
        out += txt(
            800,
            851,
            "Tomorrow, 09:00 · staged time jump"
            if jumped
            else "Scheduled follow-up + Heavy jobs, inside Agent View",
            23,
            BLUE,
            anchor="middle",
        )
        return out

    def crop_tile(
        self, shot, region, x, y, w, h, label, fill="white", cover=True, label_color=INK
    ):
        identifier = f"t{abs(hash((x, y, w, h, label)))}"
        ix, iy, iw, ih = x + 12, y + 12, w - 24, h - 62
        if not cover:
            # Round the fitted image itself, not the larger letterbox. This is
            # why the earlier Heavy Jobs source still showed square corners.
            ratio = min(iw / (region[2] - region[0]), ih / (region[3] - region[1]))
            iw, ih = (region[2] - region[0]) * ratio, (region[3] - region[1]) * ratio
            ix = x + (w - iw) / 2
            iy = y + 12 + (h - 62 - ih) / 2
        radius = max(3, 27 - min(ix - x, iy - y))
        return (
            rect(x, y, w, h, 27, fill)
            + f'<defs><clipPath id="{identifier}">{rect(ix, iy, iw, ih, radius)}</clipPath></defs>'
            + f'<g clip-path="url(#{identifier})">'
            + image(self.crop(shot, region), ix, iy, iw, ih).replace(
                "xMidYMid meet", "xMinYMin slice" if cover else "xMidYMid meet"
            )
            + "</g>"
            + txt(x + w / 2, y + h - 20, label, 22, label_color, True, "middle")
        )

    def bento(self, t):
        # Studied actual WWDC24 macOS/iOS/iPadOS + WWDC25 slides. Image-first,
        # unequal tiles, central colored identity, short labels; no bullet cards.
        out = txt(
            800,
            65,
            "All your coding agents. One terminal. One voice.",
            35,
            INK,
            True,
            "middle",
        )
        out += self.crop_tile(
            "directories",
            (0, 155, 950, 700),
            35,
            105,
            565,
            250,
            "All: six directories, 20 agents",
        )
        out += rect(615, 105, 535, 170, 27)
        for i, (_, asset) in enumerate(HARNESSES):
            out += image(
                uri(ROOT / "site/providers" / asset),
                640 + (i % 9) * 56,
                122 + (i // 9) * 48,
                36,
                36,
            )
        out += txt(882, 253, "18 coding harnesses", 23, INK, True, "middle")
        frames = sorted((self.output / "chatgpt-desktop/frames").glob("*.png"))
        out += rect(1165, 105, 400, 275, 27)
        if frames:
            out += rounded_image(frames[-1], 1177, 117, 376, 213, 15)
        else:
            out += txt(1365, 235, "Capture pending", 24, MUTED, anchor="middle")
        out += txt(1365, 358, "ChatGPT Voice · OpenCode", 22, INK, True, "middle")
        out += self.crop_tile(
            "work-directories",
            (0, 155, 950, 700),
            35,
            370,
            565,
            365,
            "Work: three directories",
        )
        out += (
            rect(615, 290, 535, 330, 28, "white")
            + mark(882, 387, 1.7)
            + txt(882, 493, "Agent View", 62, INK, True, "middle")
            + txt(882, 553, "Open source · Built in Rust", 22, BLUE, anchor="middle")
        )
        out += self.crop_tile(
            "continued",
            (0, 454, 480, 548),
            1165,
            395,
            400,
            225,
            "Heavy jobs · agent-bouncer",
            "#007f86",
            cover=False,
            label_color="white",
        )
        out += self.crop_tile(
            "scheduled",
            (0, 145, 1050, 245),
            615,
            635,
            535,
            210,
            "Scheduled follow-ups",
        )
        out += self.crop_tile(
            "scheduled",
            (84, 36, 980, 122),
            35,
            750,
            565,
            95,
            "Claude · Claude2 · Codex",
            cover=False,
        )
        out += (
            rect(1165, 635, 400, 210, 27, "#1459df")
            + keycaps(1245, 678, 1.0)
            + txt(1365, 807, "Work only", 26, "white", True, "middle")
        )
        out += txt(
            800,
            885,
            "agent-view.app  ·  github.com/moritzWa/agentview",
            22,
            BLUE,
            anchor="middle",
        )
        out += txt(800, 858, "Music: Lorn · TIMESINK", 13, MUTED, anchor="middle")
        return out

    def frame(self, t):
        if t >= 25.35 and self.still is not None:
            return self.still.copy()
        if t < 2.5:
            content = self.opening(t)
        elif t < 7:
            content = self.navigation(t)
        elif t < 15:
            content = self.voice(t)
        elif t < 18:
            content = self.chatgpt(t)
        elif t < 20.5:
            content = self.work(t)
        elif t < 25:
            content = self.continuation(t)
        else:
            content = self.bento(t)
        if t < 25:
            content += txt(
                70,
                893,
                "REVIEW · Real app UI, synthetic sessions · voice exchange staged",
                12,
                MUTED,
            )
        background = "#e3e5e9" if t >= 25 else "#f0f1f5"
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
        if t >= 25.35:
            self.still = im.copy()
        return im
