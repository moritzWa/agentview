# Voice Mode

Talk to your OpenCode sessions while your hands are busy. Press `Ctrl+K` or
`Cmd+K` in the dashboard (or type `/voice`), say what you want, and press the key
again to end. Inside a session's own view, `Ctrl+K` (OpenCode), `Cmd+K` and
`Ctrl+]` do the same, and the repaint that `Ctrl+K` used to do moves to
`Ctrl+\`. Saying "that's all, bye" ends the call too.

A terminal sends nothing for `Cmd+K` unless it is told to. In Ghostty add
`keybind = super+k=text:\x0b`: it sends the `Ctrl+K` byte, which agentview reads in
the dashboard and inside a session's view alike (and which also stops Ghostty
from clearing the screen behind a TUI).

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
- "What did the semantic search one last say?" It reads the last few exchanges
  of that session: for each, what you asked and how the agent ended its turn
  (its final reply). Tool calls, scripts and the narration between them are
  left out. Two or three exchanges by default, up to ten when you ask for more.
- "Find the session where we talked about library digitization." It searches
  the whole history, old sessions and what was said inside them, like Cmd+G in
  OpenCode, and can then read, message or close what it finds.
- "Tell it to go with option two." It sends your words to that session, as if
  you had typed them. A busy session queues the message.
- "Go", "continue", "sure", "yes". A short answer goes, word for word, to the
  session that last asked or proposed something. It never starts new sessions to
  carry out what a session proposed: that session does the work once it hears
  "go".
- "Close the naming session." It stops the session if it is running and removes
  it from Agent View, your list of sessions in focus. The history is kept, and
  Ctrl+G restores it.
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

## OpenAI or Grok

Two voices can run a call: OpenAI's GPT-Live and xAI's Grok Voice. Switch with
`/voice grok` or `/voice openai` in the dashboard's composer (or
`agentview voice --provider grok`). The choice is saved, applies from the next
call, and the first call with a provider asks for its key. With no choice
saved, agentview uses OpenAI, or Grok when only an xAI key exists.

| | OpenAI GPT-Live | Grok (`grok-voice-latest`) |
| --- | --- | --- |
| Built as | a fast talker plus a slower text model that picks the tools | one speech model that calls the tools itself |
| Price | about 5 cents a minute | about 8 cents a minute |
| Speed control | none | 0.7x to 1.5x |
| Default voice | `meridian` | `rex` (male) |
| Thinking | `low` effort on the backend | off (`xai_reasoning`), so it answers fast |

**Speed.** While a Grok call is live, the footer line shows `−  1.0x  +` and so
does the badge in an OpenCode session's footer (`● voice 2:13  −  1.2x  +`).
Click the `−` or `+` to change it in steps of 0.1x, like the controls of the
read-aloud plugin. It changes while the voice is talking and is remembered.
`/voice speed 1.2` sets it by typing. The dashboard only asks your terminal for
clicks while such a call is live, so selecting text works as usual otherwise
(Shift-drag still selects while it is). OpenAI has no speed setting, so its
badge has no buttons.

## Reading sessions and searching

`read_session` returns recent requests and final replies, plus up to six running
shells. Each has a command excerpt (not the full script), working folder, elapsed
seconds and the last three output lines (at most 400 characters). This is a clue
to progress, not an exact execution position. If live status cannot be read, the
result says so rather than claiming nothing is running. Shell inspection uses
OpenCode V2's native shell registry.

Voice history search uses the same SQLite full-text index as the global restore
picker. It refreshes every 30 seconds at low priority and only reindexes changed
sessions. Routine voice actions get a short "done", not a narrated recap.

Search results include an excerpt of the actual matching message, its ID, role
and creation timestamp (Unix milliseconds), plus the preceding and following
message when present. Matches come from the shared index, not a second history
scan. An older index is rebuilt once to add message metadata. Session reads keep
both the beginning and end of long replies; search excerpts center on the match.
After asking a session for an answer, the voice watches until it is done, then
reads and relays the final reply. Reads mark stale/incomplete answers explicitly.

On connection (including resume) and screen switches, the voice gets the current
session's ID, title and two recent exchanges. For Grok, the current-screen note
is inserted after conversation replay and before buffered microphone audio.
Both providers are prompted to read three exchanges before answering about the
chat on screen, rather than infer its subject from its title.

## Pausing, continuing, and drafts

**Ending pauses.** A call you end on purpose (the key, `Cmd+S`, "bye") is kept
for 30 minutes (`resume_minutes`). The next `Cmd+K` continues it: Grok replays
the conversation itself (spoken turns only), OpenAI is told the end of it, and
either way the voice has the same session in mind and tells you what changed
while you were away. Nothing is billed while it is paused, because the
connection is closed. A paused call shows no indicator, in Agent View or inside
OpenCode; only a live call shows its badge. `Cmd+K` still continues it. `/voice
new` makes the next call start fresh.

**`Cmd+S` in a session's own view** (Ctrl+S, the key your read-aloud plugin
answers) pauses a live call, and the key still goes through to OpenCode, so the
plugin starts reading the latest answer. The call is paused, not ended: press
`Cmd+K` when you want to talk again. Outside a call it does only what it did.

The read-aloud plugin also notifies Agent View from its central playback-start
path. Double-click, Option+click, playback controls and media-key starts pause a
call too, without depending on a keyboard shortcut. The notification travels
over the existing native terminal connection and needs no account or server.

The reverse also holds: starting a call (`Cmd+K`) pauses the read-aloud plugin in
the session on screen, so the two voices never overlap. Agent View sends the
plugin's own `speech.pause` command through the OpenCode server, the same one it
sends when you leave a session's view; the plugin keeps its place.

**A draft comes along.** Press `Cmd+K` with text already in an OpenCode session's
input box and the voice knows that text and which session it belongs to. Say
"help me think this through", talk for a while, then "sounds good": it sends the
whole message (your draft plus what you agreed) with `send_draft` and empties the
box. agentview reads the box off the screen, so it sees what is visible, and it
clears it with OpenCode's own clear key (`ctrl+c`, the default).

## Starting and stopping fast

Pressing the key to end a call silences the voice at once (about 20 ms): the
page cuts the audio before anything else happens, and the call is torn down
behind it. After a call, the hidden browser stays up for two minutes
(`keep_warm_seconds`, 0 turns it off), so the next call skips starting it. A
Grok call is live in about 1 second cold and 0.4 seconds warm; an OpenAI call in
3 to 4 seconds cold and 1.5 warm, because OpenAI needs the browser's offer
before it can create the session. Grok also hears you from the first moment: the
page records while the call connects and sends that audio first, so you can
start talking as you press the key (a test that spoke at once lost "Which" without
this, and heard the whole sentence with it). `scripts/voice-spike/latency.py`
measures it.

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
with `curl`, so the key never reaches the page. For Grok the page opens a
websocket to xAI with a token agentview fetched with your key (good for five
minutes); the key stays in agentview. The page only relays events; the
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

- Needs an OpenAI API key with access to GPT-Live, or an xAI API key for Grok.
  The first `Ctrl+K` asks for the one the chosen provider needs (masked), checks
  it, and saves it owner-only under `~/.local/state/agentview/voice/`.
  `OPENAI_API_KEY` or `XAI_API_KEY` (or the `AGENTVIEW_` versions) in the
  environment win. `agentview voice --set-key` reads a key from stdin, so
  `op read ... | agentview voice --set-key --provider grok` works.
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
  "provider": "grok",
  "xai_voice": "rex",
  "xai_model": "grok-voice-latest",
  "xai_reasoning": "none",
  "speed": 1.0,
  "voice": "meridian",
  "backend_model": "gpt-6-luna",
  "reasoning_effort": "low",
  "service_tier": "priority",
  "idle_minutes": 10,
  "max_minutes": 60,
  "browser": "hidden",
  "keep_warm_seconds": 120,
  "resume_minutes": 30,
  "chrome_path": "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
}
```

`provider` is `openai` or `grok` (what `/voice grok` writes); `xai_voice` is any
Grok voice (`rex`, `leo`, `sal`, `orion` and `atlas` are male; `GET
https://api.x.ai/v1/tts/voices` lists them all); `xai_reasoning` is `none` or
`high`; `speed` is Grok's 0.7 to 1.5. `voice` is any GPT-Live voice name (`meridian` is a male, North American
voice). On the backend, picking a tool takes about a second on every model
tried, so a bigger model buys quality, not speed.

## When something goes wrong

Every call leaves `~/.local/state/agentview/voice/last-call.log` (owner-only):
how far it connected, every line either side said, and each tool call with its
arguments, result and time. The ten calls before it are kept in `calls/`. It is
the first place to look when the voice misunderstood you: it shows what was
transcribed and which tool it picked.

A voice tab left open from an earlier version is told to close (it shows "not
needed") and cannot take over a call. While a call is on inside a session's own
view, a badge (`● voice 2:13`, blue text, no background) stays at the bottom
right, below the input: left of the token count on OpenCode's status row, or on
the empty row under it when the terminal is too narrow. Never at the top.
Starting and ending a call write nothing else; only something to act on (no
key, a failure, a microphone that is not allowed) shows as a line on the
bottom row.

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
