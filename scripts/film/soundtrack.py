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


def loop_excerpt(excerpt, seconds=45, rate=RATE, crossfade=0.8):
    length = round(seconds * rate)
    overlap = round(crossfade * rate)
    start = len(excerpt) - overlap
    assert 0 < overlap < len(excerpt) and length <= start + len(excerpt)
    music = np.zeros((length, 2), dtype=np.float32)
    music[: len(excerpt)] = excerpt
    angle = np.linspace(0, np.pi / 2, overlap)
    music[start : len(excerpt)] *= np.cos(angle)[:, None]
    repeat = excerpt[: length - start].copy()
    repeat[:overlap] *= np.sin(angle)[:, None]
    music[start:] += repeat
    return music


def tone(freq, seconds, rate=RATE, decay=12):
    n = int(rate * seconds)
    t = np.arange(n) / rate
    wave = np.sin(2 * np.pi * freq * t) * np.exp(-decay * t)
    return np.stack([wave, wave], axis=1).astype(np.float32)


def synthesize_effects(rate=RATE):
    """Quiet editorial effects. The sound-generation API rejected the TTS key."""
    noise = np.random.default_rng(7).standard_normal(int(rate * 0.45))
    window = np.hanning(len(noise))
    whoosh = np.stack([noise * window * 0.35, noise * window * 0.35], axis=1)
    low = tone(90, 0.12, rate, 28) * 0.4
    thock = tone(180, 0.08, rate, 40)
    thock = np.pad(thock, ((0, len(low) - len(thock)), (0, 0))) + low
    ping = tone(880, 0.18, rate, 10) * 0.5
    chime = tone(660, 0.28, rate, 6) * 0.4
    top = tone(880, 0.22, rate, 8) * 0.5
    chime[: len(top)] += top
    tick = tone(1400, 0.05, rate, 50) * 0.35
    pop = tone(520, 0.07, rate, 30) * 0.4
    clock = tone(1200, 0.04, rate, 60) * 0.3
    swell = tone(220, 1.1, rate, 2) * 0.15 + tone(330, 1.1, rate, 2) * 0.1
    return {
        "ping": ping.astype(np.float32),
        "whoosh": whoosh.astype(np.float32),
        "thock": thock.astype(np.float32),
        "chime": chime.astype(np.float32),
        "tick": tick.astype(np.float32),
        "pop": pop.astype(np.float32),
        "clock": clock.astype(np.float32),
        "swell": swell.astype(np.float32),
    }


def audio(output):
    plan = json.loads((output / "edit45.json").read_text())
    assert plan["duration_approved"] and plan["seconds"] == 45
    clips = plan["cues"]
    seconds = 45
    samples = np.zeros((RATE * seconds, 2), dtype=np.float32)
    cues = []
    for i, c in enumerate(clips):
        pcm = decode(output / c["final_file"])
        budget = (clips[i + 1]["at"] - 0.08 if i + 1 < len(clips) else 44.7) - c["at"]
        assert len(pcm) / RATE <= budget, (
            "Final1.5x pitch-preserved speech must fit measured timeline"
        )
        start = round(c["at"] * RATE)
        samples[start : start + len(pcm)] = pcm
        cues.append(
            dict(c, duration=len(pcm) / RATE, end=c["at"] + len(pcm) / RATE, speed=1.5)
        )
    write(output / "voice-mix.wav", samples)
    normalize(output / "voice-mix.wav", output / "narration.wav")
    music_path = output / "audio/timesink-source.mp3"
    assert music_path.exists(), (
        "TIMESINK source must be fetched from the official artist stream first"
    )
    # One contiguous span of the supplied track. No loop, no seam.
    excerpt = decode(music_path, 45)
    fade_in = np.linspace(0, 1, round(0.4 * RATE))
    fade_out = np.linspace(1, 0, round(1.0 * RATE))
    excerpt[: len(fade_in)] *= fade_in[:, None]
    excerpt[-len(fade_out) :] *= fade_out[:, None]
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
    music = excerpt[: RATE * seconds]
    write(output / "audio/music-bed.wav", music)
    t = np.arange(RATE * seconds) / RATE
    gain = np.full(len(t), 0.22, dtype=np.float32)
    for cue in cues:
        enter = np.clip((t - (cue["at"] - 0.12)) / 0.12, 0, 1)
        leave = np.clip((cue["end"] + 0.35 - t) / 0.35, 0, 1)
        gain *= 1 - 0.70 * np.minimum(enter, leave)
    gain *= np.minimum(np.clip(t / 0.35, 0, 1), np.clip((seconds - t) / 1.0, 0, 1))
    voice = decode(output / "narration.wav")[: RATE * seconds]
    effects = synthesize_effects()
    sfx = np.zeros_like(samples)
    for at, name, level in [
        (4.0, "whoosh", 0.07),
        (7.0, "thock", 0.09),
        (7.91, "thock", 0.09),
        (9.95, "thock", 0.09),
        (10.25, "chime", 0.07),
        (16.6, "tick", 0.08),
        (23.2, "thock", 0.09),
        (26.31, "thock", 0.09),
        (32.4, "clock", 0.08),
        (40.1, "swell", 0.06),
    ]:
        clip = effects[name] * level
        start = round(at * RATE)
        sfx[start : start + len(clip)] += clip[: max(0, len(sfx) - start)]
    write(output / "audio/music-sfx.wav", music * gain[:, None] + sfx)
    write(output / "music-mix.wav", voice + music * gain[:, None] + sfx)
    normalize(output / "music-mix.wav", output / "soundtrack.wav")
    (output / "audio/timing.json").write_text(json.dumps(cues, indent=2))
    (output / "audio/music.json").write_text(
        json.dumps(
            {
                "title": "TIMESINK",
                "artist": "Lorn",
                "source": "https://lorn.bandcamp.com/track/timesink",
                "source_excerpt_seconds": [0, 30],
                "continuous": True,
                "crossfade_seconds": 0.8,
                "edit_placements": [
                    {"video": [0, 30], "source": [0, 30]},
                    {"video": [29.2, 45], "source": [0, 15.8]},
                ],
                "permission": "User reports personal permission from artist Marcos Ortega for a short demo excerpt. Not a general redistribution license.",
                "replaceable_stem": "music-excerpt.mp3",
                "speech_duck_db": -10.46,
                "public_release": "Creative review still required; full song is not in the package.",
            },
            indent=2,
        )
    )
