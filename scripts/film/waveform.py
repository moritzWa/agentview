"""Deterministic speaker-isolated narration envelopes, not simulated mic input."""

import json
import math
from itertools import pairwise

import numpy as np
from soundtrack import RATE, decode

SPEAKERS = {"01.mp3": "YOU", "03.mp3": "YOU", "02.mp3": "AGENT VIEW"}


def envelope(pcm, rate=RATE, fps=30, bands=31):
    mono = np.asarray(pcm, dtype=np.float64)
    if mono.ndim == 2:
        mono = mono.mean(axis=1)
    frames = math.ceil(len(mono) * fps / rate)
    if not frames:
        return np.zeros((0, bands))
    rms = []
    spectra = []
    window = 2048
    freq = np.fft.rfftfreq(window, 1 / rate)
    edges = np.geomspace(80, 6500, bands + 1)
    for i in range(frames):
        start = round(i * rate / fps)
        end = min(len(mono), round((i + 1) * rate / fps))
        block = mono[start:end]
        rms.append(float(np.sqrt(np.mean(block * block))) if len(block) else 0)
        chunk = np.zeros(window)
        part = mono[start : start + window]
        chunk[: len(part)] = part
        power = np.abs(np.fft.rfft(chunk * np.hanning(window)))
        spectra.append(
            [
                float(np.mean(power[(freq >= lo) & (freq < hi)]))
                if np.any((freq >= lo) & (freq < hi))
                else 0
                for lo, hi in pairwise(edges)
            ]
        )
    rms = np.asarray(rms)
    spectra = np.asarray(spectra)
    reference = max(float(np.percentile(rms, 95)), 1e-5)
    spectral_reference = max(float(np.percentile(spectra, 97)), 1e-8)
    levels = np.clip((rms / reference)[:, None], 0, 1) * np.sqrt(
        np.clip(spectra / spectral_reference, 0, 1)
    )
    levels[rms < 1e-5] = 0
    # Fast attack, 67ms release. Clip boundaries never leak into another speaker.
    for i in range(1, len(levels)):
        if rms[i] < 1e-5:
            continue
        levels[i] = np.maximum(levels[i], levels[i - 1] * 0.62)
    return levels


class VoiceTracks:
    def __init__(self, cues):
        self.cues = cues

    @classmethod
    def load(cls, root):
        manifest = json.loads((root / "manifest.json").read_text())
        return cls(
            [
                {
                    "at": c["at"],
                    "speaker": SPEAKERS[c["file"]],
                    "levels": envelope(decode(root / c["file"])),
                }
                for c in manifest
                if c["file"] in SPEAKERS
            ]
        )

    def at(self, t, speaker):
        for cue in self.cues:
            local = t - cue["at"]
            frame = math.floor(local * 30 + 1e-7)
            if cue["speaker"] == speaker and local >= 0 and frame < len(cue["levels"]):
                return cue["levels"][frame]
        return np.zeros(31)
