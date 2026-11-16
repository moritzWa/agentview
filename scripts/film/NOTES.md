# Agent View film

The review film is 30 seconds at 1600 × 900, 30 fps. It uses real app renders,
purposeful motion and a five-second closing bento. ElevenLabs supplies the dry
narration. There is no music, autoplay or audible test playback.

## Reproduce

Run from the checkout. Use a current Agent View binary with `view work` support.
The scripts need macOS Menlo and Arial, `uv`, FFmpeg and FFprobe.

```sh
python3 ~/.claude/scripts/heavy-job.py --wait -- uv run --with pillow --with pyte scripts/film/capture.py --output "$HOME/Movies/agentview-film-draft/captures"
python3 scripts/film/narrate.py --output "$HOME/Movies/agentview-film-draft/audio"
python3 ~/.claude/scripts/heavy-job.py --wait -- uv run --with pillow --with resvg-py scripts/film/render.py --output "$HOME/Movies/agentview-film-draft"
python3 ~/.claude/scripts/heavy-job.py --wait -- python3 scripts/film/verify.py --output "$HOME/Movies/agentview-film-draft"
```

Narration reuses existing clips. For a new voice take, move that numbered clip
aside first. The key comes from `ELEVENLABS_API_KEY` or the 1Password item named
by `ELEVENLABS_OP_ITEM`, in memory only. It is never saved to disk or passed in argv.
`render.py --stills` creates a contact sheet without encoding video or audio.

With the dry clips already present, run the whole capture/render/verify/stage
pipeline under one queue admission:

```sh
python3 ~/.claude/scripts/heavy-job.py --wait -- uv run --with pillow --with pyte --with resvg-py scripts/film/build.py --output "$HOME/Movies/agentview-film-draft"
```

## What is real

- `capture.py` launches Agent View in a headless PTY with isolated HOME and XDG
  paths. It renders the terminal cells with pyte and Menlo. This is actual TUI
  output, not hand-drawn app controls. It does not probe private sessions.
- Work view is toggled using the app's `view work` command. Assertions verify
  that its title appears and the synthetic personal session disappears.
- The fixture disables provider I/O. It never records a live call or sends a
  real message. The voice, phone, scheduling and queue overlays are explicitly
  labeled editorial illustrations. A fixture update moves checkout from Needs
  input to Working in the actual app. This payoff is labeled staged. The native
  checkout session is not resumed and no agent runs because of the illustration.
- The phone describes only read, search and send to existing OpenCode sessions.
  It does not imply mobile session creation, stopping or native approvals.
- The Cmd+Shift+B caption describes this user's externally configured global
  binding. It is not advertised as an out-of-the-box app shortcut.

## Review and site integration

`review.html` is the standalone, muted-by-default player. Chapter buttons seek
without starting playback. Verification creates synchronized WebVTT captions,
checks all 900 decoded frames, duration, narration timing and loudness.

The checked-in review package is separate from `site/demo.mp4`, `site/demo.gif`,
`site/demo-poster.png` and `docs/assets/demo.gif`. None is replaced or deployed.
After review, the approved master can replace `site/demo.mp4`, with a new poster
from its first frame. The existing site's muted autoplay player can consume it
without a backend change. Keep narration opt-in and provide a caption track or
the review player's controls when sound is offered.

Stage the verified draft without touching those public files:

```sh
python3 scripts/film/package.py --output "$HOME/Movies/agentview-film-draft"
```

This writes `docs/assets/film-review/`, including the dry voice clips and capture
provenance so the draft can be reproduced without spending more voice credits.

Before public release, capture the actual desktop voice/send/resume path and
the phone's real ChatGPT Voice interaction. Replace their illustration beats,
not the app controls. The current draft establishes pacing and art direction;
it is not evidence that those end-to-end actions were exercised.
