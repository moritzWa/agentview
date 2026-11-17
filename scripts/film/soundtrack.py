"""Dry speech and replaceable, narration-ducked TIMESINK excerpt. No playback."""

import json
import subprocess
import wave

import numpy as np

RATE = 48000


def decode(path, seconds=None, tempo=1):
    cmd = ["ffmpeg", "-v", "error", "-nostdin", "-i", str(path)]
    if seconds is not None:
        cmd += ["-t", str(seconds)]
    if tempo > 1:
        cmd += ["-af", f"atempo={tempo}"]
    raw = subprocess.check_output(
        cmd + ["-ac", "2", "-ar", str(RATE), "-f", "f32le", "pipe:1"]
    )
    return np.frombuffer(raw, dtype="<f4").reshape(-1, 2).copy()


def write(path, samples):
    with wave.open(str(path), "wb") as wav:
        wav.setnchannels(2)
        wav.setsampwidth(2)
        wav.setframerate(RATE)
        wav.writeframes((np.clip(samples, -0.99, 0.99) * 32767).astype("<i2").tobytes())


def normalize(source, dest):
    measure = subprocess.run(
        [
            "ffmpeg",
            "-hide_banner",
            "-nostdin",
            "-i",
            str(source),
            "-af",
            "loudnorm=I=-16:TP=-1.5:LRA=7:print_format=json",
            "-f",
            "null",
            "-",
        ],
        capture_output=True,
        text=True,
        check=True,
    )
    m, _ = json.JSONDecoder().raw_decode(measure.stderr[measure.stderr.rfind("{") :])
    filt = f"loudnorm=I=-16:TP=-1.5:LRA=7:measured_I={m['input_i']}:measured_TP={m['input_tp']}:measured_LRA={m['input_lra']}:measured_thresh={m['input_thresh']}:offset={m['target_offset']}:linear=true"
    subprocess.run(
        [
            "ffmpeg",
            "-v",
            "error",
            "-nostdin",
            "-y",
            "-i",
            str(source),
            "-af",
            filt,
            "-ar",
            str(RATE),
            str(dest),
        ],
        check=True,
    )


def audio(output):
    plan = json.loads((output / "edit45.json").read_text())
    assert plan["duration_approved"] and plan["seconds"] == 45
    clips = plan["cues"]
    seconds = 45
    samples = np.zeros((RATE * seconds, 2), dtype=np.float32)
    cues = []
    for i, c in enumerate(clips):
        pcm = decode(output / "proposal45/audio" / c["file"])
        budget = (clips[i + 1]["at"] - 0.08 if i + 1 < len(clips) else 40) - c["at"]
        assert len(pcm) / RATE <= budget, (
            "Approved speech must fit without time compression"
        )
        start = round(c["at"] * RATE)
        samples[start : start + len(pcm)] = pcm
        cues.append(
            dict(c, duration=len(pcm) / RATE, end=c["at"] + len(pcm) / RATE, speed=1)
        )
    write(output / "voice-mix.wav", samples)
    normalize(output / "voice-mix.wav", output / "narration.wav")
    music_path = output / "audio/timesink-source.mp3"
    assert music_path.exists(), (
        "TIMESINK source must be fetched from the official artist stream first"
    )
    excerpt = decode(music_path, 30)
    excerpt = np.pad(excerpt, ((0, max(0, RATE * 30 - len(excerpt))), (0, 0)))[
        : RATE * 30
    ]
    # Separate excerpt stem. Full source is never staged or redistributed.
    write(output / "audio/music-excerpt.wav", excerpt)
    subprocess.run(
        [
            "ffmpeg",
            "-v",
            "error",
            "-nostdin",
            "-y",
            "-i",
            str(output / "audio/music-excerpt.wav"),
            "-c:a",
            "libmp3lame",
            "-b:a",
            "256k",
            str(output / "audio/music-excerpt.mp3"),
        ],
        check=True,
    )
    # Same30s source excerpt, not an extended song extract. Quiet connection/
    # view explanation in the middle, then return for the schedule payoff/close.
    music = np.zeros_like(samples)
    split = round(22.5 * RATE)
    return_at = round(37.5 * RATE)
    music[:split] = excerpt[:split]
    music[return_at:] = excerpt[split:]
    t = np.arange(RATE * seconds) / RATE
    gain = np.full(len(t), 0.22, dtype=np.float32)
    for cue in cues:
        enter = np.clip((t - (cue["at"] - 0.12)) / 0.12, 0, 1)
        leave = np.clip((cue["end"] + 0.35 - t) / 0.35, 0, 1)
        gain *= 1 - 0.70 * np.minimum(enter, leave)
    gain *= np.minimum(np.clip(t / 0.35, 0, 1), np.clip((seconds - t) / 1.0, 0, 1))
    gain[t < 22.5] *= np.clip((22.5 - t[t < 22.5]) / 0.4, 0, 1)
    gain[t >= 37.5] *= np.clip((t[t >= 37.5] - 37.5) / 0.35, 0, 1)
    voice = decode(output / "narration.wav")[: RATE * seconds]
    write(output / "music-mix.wav", voice + music * gain[:, None])
    normalize(output / "music-mix.wav", output / "soundtrack.wav")
    (output / "audio/timing.json").write_text(json.dumps(cues, indent=2))
    (output / "audio/music.json").write_text(
        json.dumps(
            {
                "title": "TIMESINK",
                "artist": "Lorn",
                "source": "https://lorn.bandcamp.com/track/timesink",
                "source_excerpt_seconds": [0, 30],
                "edit_placements": [
                    {"video": [0, 22.5], "source": [0, 22.5]},
                    {"video": [37.5, 45], "source": [22.5, 30]},
                ],
                "permission": "User reports personal permission from artist Marcos Ortega for a short demo excerpt. Not a general redistribution license.",
                "replaceable_stem": "music-excerpt.mp3",
                "speech_duck_db": -10.46,
                "public_release": "Creative review still required; full song is not in the package.",
            },
            indent=2,
        )
    )
