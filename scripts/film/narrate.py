#!/usr/bin/env python3
"""Generate the edit's dry narration with ElevenLabs. Never plays audio.

Credential is read in memory from ELEVENLABS_OP_ITEM, or ELEVENLABS_API_KEY. Audio is
written outside the checkout. No key is saved or passed in a process argument.
"""

import argparse
import json
import os
import subprocess
import urllib.request
from pathlib import Path

LINES = [
    (0.15, "All your coding agents. One terminal. One voice.", "JBFqnCBsd6RMkjVDRZzb"),
    (3.25, "Which sessions need my attention?", "nPczCjzI2devNBz1zQrb"),
    (
        5.25,
        "Your checkout agent asked whether coupons belong in the cart or at checkout.",
        "JBFqnCBsd6RMkjVDRZzb",
    ),
    (10.1, "Tell it checkout.", "nPczCjzI2devNBz1zQrb"),
    (11.35, "Sent.", "JBFqnCBsd6RMkjVDRZzb"),
    (12.6, "Take your agents with you.", "JBFqnCBsd6RMkjVDRZzb"),
    (
        16.05,
        "Keep work in view. Everything else keeps running.",
        "JBFqnCBsd6RMkjVDRZzb",
    ),
    (
        19.25,
        "Tomorrow at nine, review checkout. Agent bouncer queues the heavy jobs.",
        "JBFqnCBsd6RMkjVDRZzb",
    ),
    (26.0, "Open source. Built in Rust.", "JBFqnCBsd6RMkjVDRZzb"),
]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", type=Path, required=True)
    args = ap.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    key = os.environ.get("ELEVENLABS_API_KEY")
    missing = any(
        not (args.output / f"{i:02d}.mp3").exists() for i in range(len(LINES))
    )
    if missing and not key:
        item_ref = os.environ.get("ELEVENLABS_OP_ITEM")
        if not item_ref:
            raise RuntimeError(
                "Set ELEVENLABS_API_KEY or ELEVENLABS_OP_ITEM for new voice takes. Cached clips need no credential."
            )
        item = json.loads(
            subprocess.check_output(
                ["op", "item", "get", item_ref, "--vault", "Main", "--format", "json"]
            )
        )
        key = next(f["value"] for f in item["fields"] if f["id"] == "credential")
    manifest = []
    for i, (at, text, voice) in enumerate(LINES):
        dest = args.output / f"{i:02d}.mp3"
        if not dest.exists():
            request = urllib.request.Request(
                f"https://api.elevenlabs.io/v1/text-to-speech/{voice}?output_format=mp3_44100_128",
                data=json.dumps(
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
                headers={"xi-api-key": key, "Content-Type": "application/json"},
            )
            with urllib.request.urlopen(request, timeout=90) as response:
                dest.write_bytes(response.read())
        manifest.append({"at": at, "text": text, "file": dest.name, "voice": voice})
    (args.output / "manifest.json").write_text(json.dumps(manifest, indent=2))
    print(f"Generated {len(manifest)} ElevenLabs voice clips. No playback.")


if __name__ == "__main__":
    main()
