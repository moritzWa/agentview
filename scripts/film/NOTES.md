# Agent View film

The revised review film is 30 seconds at native 3840 × 2160, 30 fps. SVG
typography and graphics are rasterized directly at 4K. Actual terminal-cell
captures are 4848 × 2028 with fresh 60-pixel Menlo glyphs, not enlarged images
from the first draft. The final bento holds for five seconds.

## Reproduce

Run from the checkout. Use a current Agent View binary with `view work` support.
The scripts need macOS Menlo and Arial, `uv`, FFmpeg and FFprobe. Dry voice clips
and the privately obtained artist stream must already be in the working audio
folder. New voice takes use `ELEVENLABS_API_KEY` or `ELEVENLABS_OP_ITEM`; secrets
are held in memory only. Cached clips need no API request.

```sh
python3 ~/.claude/scripts/heavy-job.py --wait -- uv run --with pillow --with pyte --with resvg-py --with numpy scripts/film/build.py --output "$HOME/Movies/agentview-film-draft"
```

`render.py --stills` creates 4K checkpoints and a contact sheet without encoding
video or playing audio. No test plays audio or opens a GUI on the host.

## What is real

- `capture.py` launches Agent View in a headless PTY with isolated HOME and XDG
  paths. It renders the terminal cells with pyte and Menlo. This is actual TUI
  output, not hand-drawn app controls. It does not probe private sessions.
- Work view is toggled using the app's `view work` command. Assertions verify
  that its title appears and the synthetic personal session disappears.
- Scheduling and Heavy Jobs are the actual integrated app widgets. A private
  read-only `agent-bouncer status` stub supplies synthetic jobs; no real queue
  is modified. Ctrl+Y expands the actual Scheduled section. A fixture update
  stages its later Working state, with an explicit time-jump caption.
- The built-in voice exchange and checkout state change are staged. They are
  not a live call, real send or resumed native session.
- The requested native ChatGPT desktop Voice recording is blocked in the
  recording VM. Codex and Work both failed with `NotReadableError: Microphone
  is already in use`, including a BlackHole input attempt. The reserved slot
  says so. No conceptual phone or fabricated call substitutes for that proof.
  Diagnostic evidence stays in the private working folder.
- All 18 coding harnesses come from `Provider::CODING_HARNESSES`. AgentVoice's
  demonstrated read/search/send surface is for OpenCode sessions, not all 18.
- Vector keycaps depict this user's configured global Cmd+Shift+B binding.
  It is not advertised as an out-of-the-box app shortcut.

## Music and visual references

Moritz selected Lorn's “TIMESINK” and reports personal permission from Marcos
Ortega for a short demo excerpt. The official artist Bandcamp stream supplied
the working source. `soundtrack.py` extracts only 0:00–0:30, ducks it under
speech and normalizes the mix. The separate `audio/music-excerpt.mp3` stem is
replaceable. Neither the full song nor the separate stem is committed.

The revised music-bearing master remains private pending creative/publication
review. This user-reported permission is not a general redistribution license.

The bento was studied from actual Apple slides, not generic web cards:

- [WWDC24 macOS, iOS, iPadOS and Apple Intelligence summaries](https://thatvirtualboy.com/wwdc24-bentos)
- [WWDC25 summaries](https://www.macstories.net/news/wwdc-2025-all-the-small-things-bento-box-version/)
- [Apple event slide collection](https://www.figma.com/community/file/1150817983915754582/all-apple-event-summary-slides-2019-2026)

Unequal tile sizes, focused screenshot crops, a colored central identity and
short labels carry the design. No Apple artwork is used in the film.

## Review and site integration

`review.html` is the standalone, muted-by-default player. Chapter buttons seek
without starting playback. Verification creates synchronized WebVTT captions,
checks all 900 decoded frames, duration, narration timing and loudness.

The older checked-in first-draft package is not overwritten. Neither are `site/demo.mp4`, `site/demo.gif`,
`site/demo-poster.png` and `docs/assets/demo.gif`. None is replaced or deployed.
After review, the approved master can replace `site/demo.mp4`, with a new poster
from its first frame. The existing site's muted autoplay player can consume it
without a backend change. Keep narration opt-in and provide a caption track or
the review player's controls when sound is offered.

Stage the verified revision privately without touching those public files:

```sh
python3 scripts/film/package.py --output "$HOME/Movies/agentview-film-draft"
```

This writes ignored `target/film-review/`. The durable requested player remains
`~/Movies/agentview-film-draft/review.html`, muted and without autoplay.

Before public release, resolve and record the real ChatGPT desktop call and
exact-target synthetic read/send. The current rough cut is not evidence that
those actions succeeded. Creative review remains ongoing.
