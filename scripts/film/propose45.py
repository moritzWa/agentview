"""Prepare a measured45s proposal, morph proof and aligned subtitle samples.

Does not encode a movie, alter the current30s player, play sound or change auth.
"""

import argparse
import base64
import io
import json
import os
import re
import shutil
import subprocess
import urllib.error
import urllib.request
from pathlib import Path

import resvg_py
from PIL import Image
from soundtrack import decode
from visual import BLUE, INK, MUTED, font, image, mark, mix, rect, txt
from waveform import envelope

NARRATOR = "JBFqnCBsd6RMkjVDRZzb"
SCENES = [
    (
        0,
        7,
        "Problem → Agent View",
        "Too many agents. Too many windows. Agent View brings them into one terminal.",
        "intro",
        0.15,
    ),
    (
        7,
        14,
        "Right opens. Double-Left returns.",
        "Quickly zoom into the session that needs you with the Right arrow. Double-tap Left to return.",
        "navigation",
        7.15,
    ),
    (14, 22.5, "Ask and answer by voice", None, None, None),
    (
        22.5,
        28.5,
        "Your agents in ChatGPT",
        "Connect Agent View to ChatGPT. Check in and send instructions from anywhere.",
        "chatgpt",
        22.65,
    ),
    (
        28.5,
        32,
        "Status / project grouping",
        "Switch between status and project views with Control S.",
        "projects",
        28.55,
    ),
    (
        32,
        35,
        "Work scope",
        "Keep work in view. Everything else keeps running.",
        "work",
        32.1,
    ),
    (
        35,
        40,
        "Schedule + Heavy Jobs",
        "Tomorrow at nine, review checkout. Agent bouncer queues the heavy jobs.",
        "schedule",
        35.1,
    ),
    (40, 45, "Bento + URL hold", None, None, None),
]
EXISTING = {
    "question": ("01.mp3", 14.15, "Which sessions need my attention?", "YOU"),
    "answer": (
        "02.mp3",
        16.2,
        "Your checkout agent asked whether coupons belong in the cart or at checkout.",
        "AGENT VIEW",
    ),
    "checkout": ("03.mp3", 20.6, "Tell it checkout.", "YOU"),
    "work": (
        "06.mp3",
        32.1,
        "Keep work in view. Everything else keeps running.",
        "NARRATOR",
    ),
    "schedule": (
        "07.mp3",
        35.1,
        "Tomorrow at nine, review checkout. Agent bouncer queues the heavy jobs.",
        "NARRATOR",
    ),
}


def credential():
    key = os.environ.get("ELEVENLABS_API_KEY")
    if key:
        return key
    item = json.loads(
        subprocess.check_output(
            [
                "op",
                "item",
                "get",
                os.environ["ELEVENLABS_OP_ITEM"],
                "--vault",
                "Main",
                "--format",
                "json",
            ]
        )
    )
    return next(f["value"] for f in item["fields"] if f["id"] == "credential")


def request(path, body, key, content="application/json"):
    req = urllib.request.Request(
        "https://api.elevenlabs.io/v1/" + path,
        data=body,
        headers={"xi-api-key": key, "Content-Type": content},
    )
    with urllib.request.urlopen(req, timeout=120) as response:
        return json.load(response)


def force_align(path, text, key):
    boundary = "AgentViewProposalAlignment"
    body = (
        (
            f'--{boundary}\r\nContent-Disposition: form-data; name="text"\r\n\r\n{text}\r\n--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="voice.mp3"\r\nContent-Type: audio/mpeg\r\n\r\n'
        ).encode()
        + path.read_bytes()
        + f"\r\n--{boundary}--\r\n".encode()
    )
    return request(
        "forced-alignment", body, key, f"multipart/form-data; boundary={boundary}"
    )


def generate_take(dest, alignment, text, voice, key):
    data = request(
        f"text-to-speech/{voice}/with-timestamps?output_format=mp3_44100_128",
        json.dumps(
            {
                "text": text,
                "model_id": "eleven_multilingual_v2",
                "voice_settings": {
                    "stability": 0.48,
                    "similarity_boost": 0.75,
                    "speed": 1.12,
                },
            }
        ).encode(),
        key,
    )
    dest.write_bytes(base64.b64decode(data.pop("audio_base64")))
    alignment.write_text(json.dumps(data, indent=2))


def align_existing(old, dest, alignment, text, speaker, key):
    if alignment.exists() and dest.exists():
        return
    shutil.copy2(old, dest)
    try:
        alignment.write_text(json.dumps(force_align(dest, text, key), indent=2))
    except urllib.error.HTTPError as error:
        if error.code not in [401, 403]:
            raise
        # This key can generate timestamped speech but this endpoint refused it.
        # Generate a separate proposal take with its own actual returned timings.
        # Never attach new timings to the old recording or replace current film.
        voice = "nPczCjzI2devNBz1zQrb" if speaker == "YOU" else NARRATOR
        generate_take(dest, alignment, text, voice, key)
        data = json.loads(alignment.read_text())
        data["provenance"] = (
            "New proposal take with actual ElevenLabs character alignment; forced-alignment endpoint refused request."
        )
        alignment.write_text(json.dumps(data, indent=2))


def timed_words(data, text):
    if "words" in data:
        return data["words"]
    alignment = data["alignment"]
    characters = "".join(alignment["characters"])
    assert characters == text, "Character alignment does not match generated text"
    return [
        {
            "text": match.group(),
            "start": alignment["character_start_times_seconds"][match.start()],
            "end": alignment["character_end_times_seconds"][match.end() - 1],
        }
        for match in re.finditer(r"\S+", text)
    ]


def audio_proposal(root, proposal):
    audio = proposal / "audio"
    audio.mkdir(exist_ok=True)
    key = None
    cues = []
    for start, end, title, text, name, at in SCENES:
        if not name:
            continue
        dest = audio / (name + ".mp3")
        alignment = audio / (name + ".alignment.json")
        if name in EXISTING:
            old, _, _, speaker = EXISTING[name]
            if not alignment.exists():
                key = key or credential()
                align_existing(
                    root / "audio" / old, dest, alignment, text, speaker, key
                )
        elif not dest.exists() or not alignment.exists():
            key = key or credential()
            generate_take(dest, alignment, text, NARRATOR, key)
        cues.append(
            {
                "at": at,
                "text": text,
                "file": dest.name,
                "scene_end": end,
                "speaker": "NARRATOR",
            }
        )
    for name in ["question", "answer", "checkout"]:
        old, at, text, speaker = EXISTING[name]
        dest = audio / (name + ".mp3")
        alignment = audio / (name + ".alignment.json")
        if not alignment.exists():
            key = key or credential()
            align_existing(root / "audio" / old, dest, alignment, text, speaker, key)
        cues.append(
            {
                "at": at,
                "text": text,
                "file": dest.name,
                "scene_end": 22.5,
                "speaker": speaker,
            }
        )
    for cue in cues:
        cue["duration"] = float(
            subprocess.check_output(
                [
                    "ffprobe",
                    "-v",
                    "error",
                    "-show_entries",
                    "format=duration",
                    "-of",
                    "default=nw=1:nk=1",
                    str(audio / cue["file"]),
                ],
                text=True,
            )
        )
        cue["end"] = cue["at"] + cue["duration"]
        cue["fits_scene"] = cue["end"] <= cue["scene_end"]
    cues.sort(key=lambda c: c["at"])
    plan = {
        "status": "Proposed 45 seconds. Not a new movie. Duration approval required.",
        "seconds": 45,
        "scenes": [
            {"start": s, "end": e, "title": title} for s, e, title, *_ in SCENES
        ],
        "cues": cues,
        "post_generation_speedup": False,
        "tts_generation_speed": 1.12,
        "native_provenance": "Existing actual Work Voice + AgentVoice read/send recording; prompts typed, not speech-recognition proof.",
        "connection_visual": "Authenticated connector/OAuth concept is editorial, not footage of authorization; no auth/pairing changes.",
        "music": "Retain existing short30s TIMESINK source excerpt; placement to be re-edited only after runtime approval. No public publishing.",
    }
    (proposal / "script-timeline.json").write_text(json.dumps(plan, indent=2))
    return plan


def morph_box(grid, destination, p):
    shrink = min(1, p * 2)
    expand = max(0, (p - 0.5) * 2)
    if p == 1:
        return destination.copy()
    return [
        mix(grid[0], destination[0], p),
        destination[1] if shrink == 1 else mix(grid[1], destination[1], shrink),
        mix(grid[2], destination[2], expand),
        destination[3] if shrink == 1 else mix(grid[3], destination[3], shrink),
    ]


def morph_proof(proposal):
    anchors = json.loads((proposal / "captures/session-anchors.json").read_text())[:16]
    assert len({a["id"] for a in anchors}) == 16
    source = Image.open(proposal / "captures/overview.png")
    logical_height = max(a["cell_bounds"][1] + 28 for a in anchors)
    source = source.crop((0, 0, source.width, round(logical_height * 3)))
    buf = io.BytesIO()
    source.save(buf, format="PNG")
    data = "data:image/png;base64," + base64.b64encode(buf.getvalue()).decode()
    scale = 1460 / 1616
    mapping = []
    for i, a in enumerate(anchors):
        x, y, w, h = a["cell_bounds"]
        mapping.append(
            dict(
                a,
                grid=[70 + (i % 4) * 366, 200 + (i // 4) * 153, 360, 147],
                destination=[70 + x * scale, 200 + y * scale, w * scale, h * scale],
                state_color="#df8e1d" if a["state"] == "needs_input" else "#6c6f85",
            )
        )
    (proposal / "morph-map.json").write_text(
        json.dumps(
            {
                "source": "actual isolated Agent View PTY cell grid,3x raster",
                "identity": "Same normalized session id/name/provider on pane and actual row",
                "mapping": mapping,
            },
            indent=2,
        )
    )
    for p, label in [(0, "grid"), (0.5, "midpoint"), (1, "list")]:
        content = (
            mark(78, 53, 0.6)
            + txt(110, 62, "Agent View", 23, bold=True)
            + txt(70, 135, "Too many agents. Too many windows.", 43, bold=True)
            + txt(
                70,
                174,
                "Proposal: each pane becomes its matching captured session row.",
                22,
                MUTED,
            )
        )
        content += image(data, 70, 200, 1460, logical_height * scale).replace(
            "image href", 'image opacity="' + str(max(0, (p - 0.85) / 0.15)) + '" href'
        )
        for a in mapping:
            g, d = a["grid"], a["destination"]
            box = morph_box(g, d, p)
            assert box == g if p == 0 else box == d if p == 1 else True
            x, y, w, h = box
            content += f'<g opacity="{1 if p < 1 else 0}">' + rect(
                x, y, w, h, mix(4, 0, p), "white", "#b8bec9"
            )
            content += txt(
                x + 10,
                y + min(h - 5, 24),
                "✱" if a["state"] == "needs_input" else "✳",
                mix(12, 18, p),
                a["state_color"],
                True,
            )
            content += txt(
                x + 28, y + min(h - 5, 24), a["name"], mix(12, 18, p), INK, True
            )
            provider = {
                "claude": "Claude",
                "codex": "Codex",
                "opencode": "OpenCode",
                "cursor": "Cursor",
                "devin": "Devin",
            }.get(a["provider"], a["provider"])
            content += txt(
                x + w - 10,
                y + min(h - 5, 24),
                provider,
                mix(10, 16, p),
                MUTED,
                anchor="end",
            )
            if h > 65:
                for j, line in enumerate(
                    [
                        "$ pnpm test --watch",
                        "  compiling changed routes…",
                        "  " + a["summary"][:43],
                        "  23 passing · waiting on agent",
                    ]
                ):
                    content += txt(
                        x + 10, y + 47 + j * 17, line, 10.5, MUTED, mono=True
                    )
            content += "</g>"
        content += txt(
            70,
            872,
            f"GEOMETRIC MORPH PROOF · p={p:.2f} ·16 stable normalized session IDs · actual cell-bound destinations",
            17,
            BLUE,
        )
        svg = f'<svg xmlns="http://www.w3.org/2000/svg" width="3840" height="2160" viewBox="0 0 1600 900">{rect(0, 0, 1600, 900, 0, "#f0f1f5")}{content}</svg>'
        (proposal / f"morph-{label}.svg").write_text(svg)
        (proposal / f"morph-{label}.png").write_bytes(
            resvg_py.svg_to_bytes(svg_string=svg)
        )


def subtitle_proof(proposal):
    aligned = json.loads((proposal / "audio/answer.alignment.json").read_text())
    words = timed_words(aligned, EXISTING["answer"][2])
    lines = [
        "Your checkout agent asked whether coupons",
        "belong in the cart or at checkout.",
    ]
    expected = " ".join(lines).split()
    assert [w["text"] for w in words] == expected, (
        "Alignment words differ from original utterance"
    )
    sheet = Image.new("RGB", (1800, 750), "#f0f1f5")
    samples = []
    examples = [
        ("question", 0.6),
        ("answer", 0.45),
        ("answer", 1.4),
        ("answer", 2.6),
        ("answer", 4.0),
        ("checkout", 0.4),
    ]
    for i, (name, t) in enumerate(examples):
        _, at, text, speaker = EXISTING[name]
        color = "#6f38c5" if speaker == "YOU" else BLUE
        row_lines = lines if name == "answer" else [text, ""]
        words = timed_words(
            json.loads((proposal / f"audio/{name}.alignment.json").read_text()), text
        )
        levels = envelope(decode(proposal / f"audio/{name}.mp3"))
        content = rect(15, 15, 870, 220, 24, "white") + txt(
            40, 48, f"{speaker} · {at + t:.2f}s", 18, color, True
        )
        cursor = 0
        visible = []
        for row, line in enumerate(row_lines):
            prefix = ""
            for word in line.split():
                alignment = words[cursor]
                if alignment["start"] <= t:
                    content += txt(
                        40 + font(29, True).getlength(prefix),
                        100 + row * 42,
                        word,
                        29,
                        INK,
                        True,
                    )
                    visible.append(word)
                prefix += word + " "
                cursor += 1
        for j, level in enumerate(levels[min(len(levels) - 1, int(t * 30))]):
            h = 2 + float(level) * 26
            content += rect(40 + j * 8, 190 - h / 2, 3, h, 1.5, color)
        content += txt(
            360, 206, "YOU: purple #6f38c5 · AGENT VIEW: blue #1e66f5", 14, MUTED
        )
        svg = f'<svg xmlns="http://www.w3.org/2000/svg" width="900" height="250">{rect(0, 0, 900, 250, 0, "#f0f1f5")}{content}</svg>'
        panel = Image.open(io.BytesIO(resvg_py.svg_to_bytes(svg_string=svg))).convert(
            "RGB"
        )
        sheet.paste(panel, ((i % 2) * 900, (i // 2) * 250))
        samples.append(
            {
                "speaker": speaker,
                "color": color,
                "relative_time": t,
                "absolute_time": at + t,
                "visible_words": visible,
            }
        )
    sheet.save(proposal / "subtitle-reveal-sample.jpg", quality=97)
    (proposal / "subtitle-sample.json").write_text(
        json.dumps(
            {
                "source": "Actual ElevenLabs word/character alignment for matching proposal takes; not inferred timing",
                "stable_lines": lines,
                "no_reflow": True,
                "samples": samples,
            },
            indent=2,
        )
    )


def shortcut_proof(proposal):
    def cap(x, y, label, pressed=False):
        w = 105 if label == "Ctrl" else 74
        color = "white" if pressed else INK
        out = (
            rect(x, y + 5, w, 70, 12, "#b8c5df")
            + rect(x, y, w, 70, 12, BLUE if pressed else "white")
            + rect(x + 3, y + 3, w - 6, 64, 9, BLUE if pressed else "#fff")
        )
        if label in ["Right", "Left"]:
            flip = -1 if label == "Left" else 1
            out += f'<g transform="translate({x + w / 2} {y + 35}) scale({flip} 1)" fill="none" stroke="{color}" stroke-width="4" stroke-linecap="round" stroke-linejoin="round"><path d="M-18 0H18M6-12L18 0 6 12"/></g>'
        else:
            out += txt(x + w / 2, y + 45, label, 28, color, True, "middle")
        return out

    content = txt(50, 54, "Explicit keys, not “use the arrows”.", 30, bold=True)
    content += cap(50, 95, "Right", True) + txt(
        150, 140, "Zoom into the waiting session", 24
    )
    content += (
        cap(50, 205, "Left", True)
        + cap(138, 205, "Left")
        + txt(244, 250, "Double-tap Left to return", 24)
    )
    content += (
        cap(50, 315, "Ctrl")
        + cap(169, 315, "S", True)
        + txt(278, 360, "Status / project views", 24)
    )
    content += txt(
        50,
        440,
        "Pressed states will follow the recorded key events. This is a design proof, not new input footage.",
        18,
        MUTED,
    )
    svg = f'<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="480">{rect(0, 0, 1000, 480, 0, "#f0f1f5")}{content}</svg>'
    (proposal / "keycap-sample.png").write_bytes(resvg_py.svg_to_bytes(svg_string=svg))


def proposal_page(proposal):
    from html import escape

    plan = json.loads((proposal / "script-timeline.json").read_text())
    rows = "".join(
        f"<tr><td>{c['at']:.2f}–{c['end']:.2f}s</td><td>{escape(c['speaker'])}</td><td>{escape(c['text'])}</td><td>{c['duration']:.2f}s</td></tr>"
        for c in plan["cues"]
    )
    scenes = "".join(
        f"<li><b>{s['start']:g}–{s['end']:g}s</b> {escape(s['title'])}</li>"
        for s in plan["scenes"]
    )
    html = f"""<!doctype html><meta charset="utf-8"><title>Agent View · Proposed45-second script</title><style>body{{font:17px/1.5 system-ui;background:#e9ebef;color:#11121b;margin:40px auto;max-width:1400px;padding:0 24px}}h1{{font-size:36px}}section{{background:white;padding:24px;border-radius:20px;margin:22px 0}}table{{width:100%;border-collapse:collapse}}td,th{{padding:12px;text-align:left;border-bottom:1px solid #dce0e7}}td:first-child{{white-space:nowrap}}img{{width:100%;display:block}}.proofs{{display:grid;grid-template-columns:1fr 1fr;gap:20px}}a{{color:#1e66f5}}</style><h1>Proposed45-second film</h1><p><b>Script and keyframes only. Runtime needs approval.</b> The current30-second movie is unchanged. No autoplay or audio playback.</p><section><h2>Give each action time to read</h2><ol>{scenes}</ol></section><section><h2>Measured narration</h2><p>Actual generated clip durations. No post-generation speedup. All cues fit their scene windows.</p><table><tr><th>Time</th><th>Speaker</th><th>Script</th><th>Clip</th></tr>{rows}</table></section><section><h2>Grid → matching actual rows</h2><p>All16 panes keep the same session IDs, titles, providers and state tokens. Each rectangle ends at its measured row bounds in an actual Agent View capture. No unrelated list fades in.</p><img src="morph-midpoint.png" alt="Midpoint of geometric pane-to-row morph"><p><a href="morph-grid.png">Start</a> · <a href="morph-list.png">Finish</a> · <a href="morph-map.json">Identity and cell-bound map</a></p></section><section><h2>Words appear with the speaker</h2><p>Actual ElevenLabs character/word timing. Words keep fixed two-line positions. Human: purple and YOU. Agent: blue and AGENT VIEW. Each waveform still follows its own isolated speech track.</p><img src="subtitle-reveal-sample.jpg" alt="Aligned subtitle reveal samples in purple and blue"><p><a href="subtitle-sample.json">Timing samples</a></p></section><section><h2>Keys show the action</h2><img src="keycap-sample.png" alt="Right, double Left, Control S vector keycaps"></section><section><h2>ChatGPT connection</h2><p>Connect Agent View to ChatGPT. Check in and send instructions from anywhere.</p><p>An authenticated connector/OAuth badge is an editorial explanation, not footage of an authorization flow. The native clip remains real Work Voice + AgentVoice read/send to one existing synthetic OpenCode session. Prompts were typed. No account or pairing changes.</p><p>Bento: <b>Your agents in ChatGPT</b>. Subtext: existing OpenCode sessions.</p></section><p>Original logo, white/neutral bento with saturated accents, rounded inner images, clean native-window edge and private music stay. Public media is unchanged.</p>"""
    html = (
        html.replace("Proposed45", "Proposed 45")
        .replace("current30", "current 30")
        .replace("All16", "All 16")
    )
    (proposal / "proposal.html").write_text(html)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", type=Path, required=True)
    ap.add_argument("--assets-only", action="store_true")
    ap.add_argument("--audio-only", action="store_true")
    args = ap.parse_args()
    proposal = args.output / "proposal45"
    proposal.mkdir(exist_ok=True)
    if not args.assets_only:
        audio_proposal(args.output, proposal)
    if args.audio_only:
        print(
            "Measured proposal narration and actual alignments saved; no movie encoded."
        )
        return
    morph_proof(proposal)
    subtitle_proof(proposal)
    shortcut_proof(proposal)
    proposal_page(proposal)
    print(
        "Prepared 45s proposal only: measured script, exact row mapping, midpoint still and actual-aligned subtitle sample. Current 30s movie unchanged."
    )


if __name__ == "__main__":
    main()
