# Voice Mode

Talk to your OpenCode sessions while your hands are busy. Press `Ctrl+K` or
`Cmd+K` in the dashboard (or type `/voice`), say what you want, and press the key
again to end. Inside a session's own view, `Ctrl+K` (OpenCode), `Cmd+K` and
`Ctrl+]` do the same, and the repaint that `Ctrl+K` used to do moves to
`Ctrl+\`. Saying "that's all, bye" ends the call too.

`Cmd+K` reaches agentview only where the terminal passes it on; in Ghostty add
`keybind = super+k=unbind`.

## What it knows about what you are looking at

The session selected in the dashboard, or open in its own view, is the one
"this" and "it" mean. The voice is told when you move to another, and
`list_sessions` marks it `on_screen` and always includes it. "Start one in the
same folder" uses its folder. A call keeps working while you are inside a
session: it asks for fresh session data itself, because the dashboard is not
refreshing then.

## What you can say

- "Which sessions need me?" The voice already knows the state of the sessions
  you are working in, so it answers at once.
- "What did the semantic search one last say?" It reads the last messages of
  that session and tells you the gist.
- "Tell it to go with option two." It sends your words to that session, as if
  you had typed them. A busy session queues the message.
- "Stop that one." Same as pressing Escape in OpenCode.
- "Tell it to continue." A session whose last turn failed takes a message to
  retry. The voice never starts a new session to recover another.
- "Actually, make it about laundry instead." It stops the session if it is
  running, rewinds to just before your last message the way `/undo` does
  (including that message's file changes), and sends the corrected text.
- "Start a session in the cronus folder to fix the menu." "This repository"
  means the folder of the session you were just talking about.
- "Tell me when the excel one is done." It speaks up when the session finishes
  or needs you.

There is no confirmation step: the voice decides from what you said.

## Which sessions it lists

The sessions you are working in, not every parked one. Recent means the last
**10** sessions by when you last wrote to them (an agent's own activity does not
count), plus any working or waiting session you touched in the last day. Paused
sessions and sessions another session started are left out. It ends with a
count of the parked ones and how many of them need you; ask for "all" or name a
session to find an older one.

Each session carries its state (working, needs you, done), its folder, when you
last wrote to it, what it last said (the closing sentences, only when they
changed since you heard them), and what keeps it busy: a background command your
plugin reported, or running sub-agents.

## How it works

```text
you ── mic/speaker ── voice page (a pinned browser tab, 127.0.0.1:47617)
                         │  WebRTC audio + events         │ local WebSocket
                         ▼                                ▼
                  OpenAI GPT-Live  ◀── curl, once ──  agentview ── your sessions
                  talker + backend model                 runs the tools
```

A browser holds the microphone and speaker, because it brings echo
cancellation and noise suppression with it. agentview starts the OpenAI session
with `curl`, so the key never reaches the page. The page only relays events; the
tools run in agentview, against the same sessions the dashboard shows.

By default agentview runs that browser itself, **hidden**: a Chromium-based
browser (Chrome, Chromium, Edge or Brave) in headless mode, with its own
profile, started when a call starts and stopped when it ends. No tab or window
appears and nothing needs pinning. It costs a browser's worth of memory (about
350 MB) only while a call is open. The first call, macOS asks once to let your
terminal (or the browser) use the microphone. If no such browser is found,
agentview falls back to opening a tab. The page listens on `127.0.0.1` only and
accepts a WebSocket only from its own origin.

The `browser` setting changes this: `"tab"` opens a tab in your default browser
(pin it and allow the microphone once; a connected tab is used at once), and
`"none"` leaves the page to you at `http://127.0.0.1:47617/`. `chrome_path`
points at a particular browser.

## Setup and cost

- Needs an OpenAI API key with access to GPT-Live. The first `Ctrl+K` asks for
  it (masked), checks it, and saves it owner-only under
  `~/.local/state/agentview/voice/`. `OPENAI_API_KEY` or
  `AGENTVIEW_OPENAI_API_KEY` in the environment win. `agentview voice --set-key`
  reads a key from stdin, so `op read ... | agentview voice --set-key` works.
- GPT-Live bills about 5 cents a minute while a call is open, silence included,
  plus the backend model's tokens (pennies). Starting a call bills 15 seconds up
  front, credited against the running time. A call ends itself after 10
  minutes of silence with nothing watched, and after 60 minutes in all.
- Nothing is stored at OpenAI: the session is created with `store: false`.
  OpenAI keeps abuse-monitoring logs for 30 days unless the organisation has
  Zero Data Retention. Organisation admins can see usage and cost per project
  and key.
- Only OpenCode sessions are in v1. Claude Code and the other harnesses are not
  listed.

## Settings

`~/.local/state/agentview/voice/settings.json`, every field optional:

```json
{
  "voice": "meridian",
  "backend_model": "gpt-6-luna",
  "reasoning_effort": "low",
  "service_tier": "priority",
  "idle_minutes": 10,
  "max_minutes": 60,
  "browser": "hidden",
  "chrome_path": "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
}
```

`voice` is any GPT-Live voice name (`meridian` is a male, North American
voice). On the backend, picking a tool takes about a second on every model
tried, so a bigger model buys quality, not speed.

## When something goes wrong

Every call leaves `~/.local/state/agentview/voice/last-call.log`: the events,
tool names and timings of the last call, with no words in it. If a call did not
connect, it says how far it got.

A voice tab left open from an earlier version is told to close (it shows "not
needed") and cannot take over a call. While a call is on inside a session's own
view, a badge (`● voice 2:13`) stays on screen: in the blank stretch of the
bottom row when there is one, else in the top right corner.

Set `AGENTVIEW_VOICE_LOG=/some/file` before starting agentview: every transcript
line, tool call (with its time and result) and event goes into that file as
JSON lines. `scripts/voice-spike/summarize.py FILE` prints it as a conversation.
A microphone that stays exactly silent for 20 seconds shows "No sound from the
microphone" in the footer: on a Mac that is the Privacy & Security setting for
your terminal or browser.

## Testing

`scripts/voice-spike/` holds the manual checks, which use real OpenAI calls and
cost a few cents. `spike.mjs` runs the page against a stand-in server.
`e2e.py` runs the real dashboard in a pseudo-terminal, lets agentview start its
hidden browser with a synthesized, muted microphone, and prints what the call
did; `scenario.sh` wraps it with the log summary. `native_view.py` presses the
voice key inside a session's own view. `onboarding.py` covers the first run. `cargo test --test voice_edit_real -- --ignored` checks the correction
against a running OpenCode server. Run the browser parts under
`~/.claude/scripts/heavy-job.py --wait --`.
