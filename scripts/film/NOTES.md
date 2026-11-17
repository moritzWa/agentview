# Agent View film

The approved runtime is45 seconds at native 3840 × 2160,30fps. SVG
typography and graphics are rasterized directly at 4K. Actual terminal-cell
captures are 4848 pixels wide (3120 tall for busy views, 2028 for compact views),
with fresh 60-pixel Menlo glyphs, not enlarged images
from the first draft. The final bento holds for five seconds.

## Reproduce

Run from the checkout. Use a current Agent View binary with `view work` support.
The scripts need macOS Menlo and Arial, `uv`, FFmpeg and FFprobe. Dry voice clips
and the privately obtained artist stream must already be in the working audio
folder. New voice takes use `ELEVENLABS_API_KEY` or `ELEVENLABS_OP_ITEM`; secrets
are held in memory only. Cached clips need no API request. The approved measured
timeline and aligned takes live in `proposal45/`; `prepare45.py` conforms these
cached inputs and native recording excerpts without new account interactions.

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
- A real Right-arrow/double-Left recording opens a dedicated synthetic native
  OpenCode session and returns to Agent View. No fake successful open.
- The ChatGPT desktop recording is actual Work Voice + AgentVoice read/send.
  Prompts were typed inside active Voice; speech recognition is untested.
  The initial microphone error was resolved by a temporary VM app TCC grant,
  restored to its prior absent state after capture. Receiver-side message
  metadata confirms the actual send. The dedicated tools-denied synthetic
  session was deleted afterward. No pairing or account-auth settings changed.
- Native desktop footage is a 1024 × 738 window inset, unlike the independently
  high-resolution Agent View sources. Do not claim that source is native 4K.
- The busy fixture has20 synthetic agents across6 work/personal directories.
  All/work comparison preserves identical state. Ctrl+S actually changes
  status/directory grouping, separately from scope. Supported usage account
  keys are `claude`, `claude2`, `codex`; `claude1` is ignored by the parser.
- All 18 coding harnesses come from `Provider::CODING_HARNESSES`. AgentVoice's
  demonstrated read/search/send surface is for OpenCode sessions, not all 18.
- Vector keycaps depict this user's configured global Cmd+Shift+B binding.
  It is not advertised as an out-of-the-box app shortcut.

## Music and visual references

The opening uses a dense animated4×4 synthetic coding-terminal grid. Stable
normalized IDs tie every pane title/provider/state to its captured session row.
Each pane rectangle and text anchor moves into the exact measured cell bounds;
matching captured row pixels complete the handoff. The7–14s Right/double-Left
beat is separate from the14–22.5s editorial voice exchange. Native footage omits test
setup instructions above the actual coupon question. After a genuine return,
the edit cuts back to the busy20-session fixture instead of lingering on capture
commands or the sparse dedicated-session setup.

`waveform.py` precomputes RMS and31 FFT bands from each active speaker's dry
clip at30fps. Cue offsets select the correct speaker; gaps return flat2-pixel
bars. Music never drives these bars. `preview_waveform.py` writes a frame strip
and numerical check from the real clips. Tests cover silence, relative level,
speaker switches, offsets and determinism. This is voiceover visualization,
not original microphone evidence. Recorded ChatGPT UI is never redrawn.

The spoken intro names the problem and Agent View. Navigation says Right and
double-tap Left explicitly. Large vector keycaps flash at recorded input-phase
boundaries. Actual ElevenLabs character timings reveal words at fixed positions,
without reflow. YOU is purple, AGENT VIEW blue, both labeled. No narration is
time-compressed. The final five seconds remain a readable music-only hold.

Moritz selected Lorn's “TIMESINK” and reports personal permission from Marcos
Ortega for a short demo excerpt. The official artist Bandcamp stream supplied
the working source. `soundtrack.py` extracts only 0:00–0:30, ducks it under
speech and normalizes the mix. The same30s excerpt plays at video0–22.5 and
37.5–45; the connection/view explanation has a quiet middle. No extra part of the
song is extracted. The separate `audio/music-excerpt.mp3` stem is
replaceable. Neither the full song nor the separate stem is committed.

The revised music-bearing master remains private pending creative/publication
review. This user-reported permission is not a general redistribution license.

The bento was studied from actual Apple slides, not generic web cards:

- [WWDC24 macOS, iOS, iPadOS and Apple Intelligence summaries](https://thatvirtualboy.com/wwdc24-bentos)
- [WWDC25 summaries](https://www.macstories.net/news/wwdc-2025-all-the-small-things-bento-box-version/)
- [Primary Apple iPadOS26 design reference](https://www.apple.com/newsroom/2025/06/ipados-26-introduces-powerful-new-features-that-push-ipad-even-further/)
- [Apple event slide collection](https://www.figma.com/community/file/1150817983915754582/all-apple-event-summary-slides-2019-2026)

Unequal aligned tiles, focused screenshot crops and short labels carry the design.
White/neutral tiles separate from a gray canvas; selective saturated teal/blue
accents replace barely distinguishable pastels. The canonical
`docs/assets/logo.svg` is embedded verbatim everywhere: original elongated hexagon,
gray outlined rings and blue center, never white or re-spaced on a gradient.
No Apple artwork is used in the film.

## Review and site integration

Moritz approved45 seconds with “45 is fine”. `propose45.py` retains the measured
script and proof inputs in private `proposal45/`. The active edit is
`revision45.py`; it reuses the canonical layout helpers from the earlier edit.
The inputs include measured narration,16 stable pane/session IDs, exact captured row
destinations, grid/midpoint/list keyframes, explicit vector keycaps and fixed-line
subtitle reveals. Voice generation/alignment stays beside the matching takes.

The forced-alignment endpoint refused the key. Separate proposal takes use the
supported ElevenLabs `with-timestamps` endpoint instead. Actual returned character
timings drive their subtitles, never old recordings with guessed new timings.
Human speech is purple and labeled YOU; Agent View is blue and labeled AGENT VIEW.
Runtime approval is not public publishing approval. The music excerpt stays30s.

`review.html` is the standalone, muted-by-default player. Chapter buttons seek
without starting playback. Verification creates synchronized WebVTT captions,
checks all1350 decoded frames, duration, narration timing and loudness. The active
movie is `agentview-review.mp4`; the earlier30s movie is retained separately.
Captions are opt-in because word-timed text is already burned into the picture.

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

Native Voice read/send is now evidenced independently from the staged opening
exchange. The short movie shows an excerpt, not the full wall-clock tool wait.
Creative review remains ongoing; microphone speech recognition is not verified.
