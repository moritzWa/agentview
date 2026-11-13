# Voice Mode: which session, and when it is done

## November 13 realtime replay

The session-reference prompt now follows the developer's topic, rather than
equating "it" with the latest message target or automatically preferring the
screen. A short example asks "Data leads or the website?" after discussing
data leads and starting a website edit. This is model guidance, not a routing
guard. There is no routine spoken title prefix.

The previous prompt had conflicting rules and repeated instructions. Adding
more disambiguation sentences to it did not help the real voice model. A
compact relay prompt did: in six new realtime conversations per case, the old
prompt answered from the website in all six ambiguous cases. The compact
prompt asked which session in five and still picked the website in one.
Explicit data-leads and website references, sending to data leads, stopping
the website session, and ending the call all chose the expected tool/target
in six of six runs each under both prompts. This is an improvement, not a
guarantee against another mix-up.

These runs use `grok-voice-latest` on xAI's realtime WebSocket, which identified
itself as `grok-voice-3.0`. They inject text conversation items and simulated
tool results and collect the generated transcript, discarding audio without
playing it. They test the actual voice model's reasoning, not microphone
transcription, the browser, or real session delivery. The conversation is
reconstructed from this note, not the lost original recording. The default
fixture assumes the ownership fix lets the earlier send succeed.

With the original failed-send result retained, a separate four-run replay
improved from zero clarification questions to two, with two wrong website
answers remaining. The clearer prompt helps but does not eliminate the bug,
especially when the earlier data-leads send failed. All explicit-reference
controls in that replay still chose the correct sessions.

Replay with `websocket-client` installed and `XAI_API_KEY` set (or the existing
voice key file). This makes paid API requests; tools never touch real sessions:

```sh
python3 scripts/voice_reference_replay.py --baseline a2d5a19 --runs 6 \
  --cases ambiguous,named,recent,relay,stop,end --output /tmp/voice-replay.json
```

`--send-failed` retains the original call's reply-authority error instead of
the successful send. Output includes the spoken answer and read targets so
the final answer, not just the first tool choice, can be reviewed.

Research note, November 12, 2026. No code changed. It comes from one real Grok
call (about 9 minutes, `last-call.log`) where the voice answered about the wrong
session and its picture of session state was stale. Direct observations from the
log and the code are separate from inference, which is marked.

## What happened in the call

| Time | Developer | Voice |
| --- | --- | --- |
| 0 | Call starts. `eu-biotech-data-leads` is on screen (`this`). Start notes list it as `needs_you`. | |
| 44 to 139 s | Talks about putting a line on his website, based on that session's notes. | `read_session this` seven times over the call, correct session. State now reads `working`. |
| 139 s | "Did it come back regarding the register check thing?" (a status question) | Reads, then calls `send_message this` with the question rewritten as a message to the session. Refused: `reply authority was not granted for this session`. Voice says the session "hasn't granted reply authority". |
| 304 s | Asks for a website edit. | `start_session` (folder `m`, handle s31), then `watch_session s31`. |
| 332 s | "Did the session respond finally?" He means data-leads. | Reads **s31**, the session it just started and is watching, and says it is done. |
| 354 s | "No, I meant the one we were just discussing, the data leads one." | Reads `this`, correct from then on. |

Two things differ from the first reading of the problem:

- The screen never changed during the call. `this` was right all along. The wrong
  guess came from the voice's own actions: starting and watching s31 made it the
  most recent session in the voice's context, and "the session" resolved to that.
- The failed send was not a stale state problem only. The question was a status
  check, and the voice turned it into a message to a working session (a message
  that would have queued and added noise). The relay prompt ("what the developer
  says goes to the session as they said it") pushes that way.

## Problem 1: knowing which session the developer means

### Today

- Three ways to name a session: a handle (`s31`) from a tool result, `this` (the
  session on screen), `it` (the focus).
- `this` comes from the dashboard selection or the open session, sampled every 2
  seconds. The voice is told once when it changes ("Current screen: ... overrides
  earlier screen notes"), with the title and two recent exchanges. Nothing
  restates it later.
- `it` is the `focus` field in `tools.rs`. It is overwritten by every `resolve()`,
  by `start_session`, and by a screen change. So it means "the session a tool
  touched last", not "the topic". Agent-initiated reads move it too.
- Nothing in code sees the developer's wording. The model decides what "the
  session" means, and it can pass an explicit handle that bypasses `this` and
  `it` entirely, which is what happened at 332 s.
- Tool results name the session (`title`) but the spoken answer did not. The
  developer could only tell it was the wrong session from the content.
- The prompt says `this` is on screen and `it` is the one just discussed. It has
  no rule for when two sessions are plausible.

### Options

**A. Ground every result and every spoken answer.** Each tool result gets a small
`context` object: the on-screen title, the session last started or sent to, and
what is being watched. The prompt gets one rule: when an answer comes from a
session other than the one you last answered about, say its title first ("On the
website one: it is done"). Costs about 50 tokens per result and a few lines in
`xai.md` and `talker.md`. It does not prevent the mix-up, but the developer hears
it in the first second and corrects it, instead of after the answer.

**B. Ask when the referent is open.** Prompt rule: if "the session", "it" or "that
one" could mean the on-screen session or one you started, sent to or watched in
this call, ask a one-line either/or ("the data leads one or the website one?").
This needs A's context to be reliable. A code guard (compare the transcript with
the resolved title and refuse) was considered and rejected: string matching on
speech is brittle, and short prompts have worked better than guards here.
Trade-off: one extra turn, only when two real candidates exist.

**C. Let him point.** Keep a short screen history (last three sessions on screen
with seconds ago) so "the one I was just looking at" resolves, and optionally show
the voice focus in the badge (`● voice 2:13 · data leads`) or as a row marker.
The history is cheap and invisible. The visible marker costs screen space and he
is listening, not looking; he has rejected clutter in the footer before. Do the
history, skip the marker for now.

**D. Make `it` smarter** (do not move focus on `start_session`, or only on user
requested reads). Rejected: starting a session and then asking about it is the
common case, and code cannot tell which tool calls the developer asked for.

### Recommendation

A + B + the history part of C. All prompt and `tools.rs`, no UI. Also add a
prompt line that a status question ("did it come back?", "is it done?") is a
read, never a `send_message`.

## Problem 2: knowing what state sessions are in, and when one finishes

### Today

- `list_sessions` is a snapshot of the dashboard's session list. `read_session`
  adds `state`, `answer_ready`, `still_working` and running shells, with the last
  completed turns.
- Freshness: the dashboard refreshes sessions every 15 seconds by default
  (`--refresh-ms`). While a call is live the voice asks for a refresh every 8
  seconds. So state is up to about 8 to 15 seconds old, and a busy period shorter
  than that can be missed entirely. Agent View polls OpenCode's session status;
  it does not subscribe to events.
- `watch_session(until: done | needs_you)` registers a one-shot watch. Every 2
  seconds `poll()` compares the snapshot with it. A `done` watch fires when the
  session was seen busy and is now not busy, or, for work just given, when it was
  never seen busy 45 seconds after registering. The result is spoken as
  commentary: Grok gets `response.create` with "Tell the developer, briefly...";
  OpenAI gets `session.commentary.append`.
- A watch also keeps the call open past the 10 minute idle limit (up to 60 minutes
  in all, billed).
- Other recent sessions: changes to `needs_you` or `done`, and a changed ending of
  a finished session, are sent as a quiet system note "do not bring it up
  unprompted". A session starting work is deliberately not reported.
- Reply authority is a capability on the session, `cannot_message` in
  `list_sessions` rows. It is computed in the OpenCode adapter from the state at
  that moment and withheld while the session needs input (except a failed turn).

### What the Nov 12 call shows

- **Stale state.** The start notes said data-leads was `needs_you`. Forty-four
  seconds later it read `working`. Nothing told the voice in between, because a
  session starting work is not announced, even quietly.
- **State and permission disagree.** The voice saw `working` and tried to send;
  the refusal came from the capability, not the state. A parallel note,
  [`voice-reply-authority.md`](voice-reply-authority.md), traced it: the session
  was not owned by the supervisor, so it never got `Reply`, and the voice host
  does not adopt such sessions before sending. (My first guess, that the turn
  classifier rewrites state after capabilities are granted, is ruled out there.)
  For this note the relevant part is the signal: `cannot_message` appears in list
  rows but not in `read_session`, the row says nothing about why, and the error
  text is raw ("reply authority") rather than something the voice can act on.
- **Polling.** Eight `read_session` calls in nine minutes; seven were on
  data-leads. Each followed a developer question, not a status poll, but while the
  session works each read returns the same last completed turns plus
  `still_working`. There is no cheap "what changed since I last looked".
- **The watch is unobservable.** Neither watch firings nor the commentary and
  quiet notes sent to the page are written to `last-call.log`, so the log cannot
  say whether the s31 watch fired. By the code it would not have: s31 finished
  inside one snapshot window, and the 45 second fallback (about 366 s) came after
  the developer had already asked at 332 s. Its only test coverage is with a fake
  host.
- **Gaps in the watch.** A session missing from the snapshot (just started) is
  silently retained. Nothing guards against speaking over the developer.
- **Push exists.** OpenCode V1 serves `/global/event` (SSE). The running V2 server
  has an authenticated `/api/event` route (it exists; its payload was not
  inspected). Agent View uses neither.

### Options

**A. Make the existing watch reliable.** Small, voice only.
- Watch automatically any session the voice starts or sends a message to, for
  `done` and `needs_you`, so it does not depend on the model remembering.
- For watched ids only, read OpenCode's session status every 1 to 2 seconds
  instead of waiting for the snapshot.
- Add a second fire condition: the session's last update moved past the time the
  voice gave it work and it is not busy. That catches work that finished between
  looks.
- Log registrations, firings and every commentary and quiet note.
- Hold a spoken notice until the developer stops talking.
- Adopt an unowned session before sending (the fix in
  [`voice-reply-authority.md`](voice-reply-authority.md)), so a `working` row is
  also a messageable one.

**B. Subscribe to the server's event stream.** Medium, touches the supervisor.
Agent View keeps one SSE connection per OpenCode server and turns events into
status transitions on an in-memory bus. Voice watches and the dashboard both read
it. Exact transitions, no snapshot lag, fewer polls, and it would speed up the
dashboard too. Costs: V1 and V2 payloads differ, reconnect and missed-event
handling, OpenCode only, and the voice change is blocked on the supervisor change.

**C. Tell the voice what changed, with ages.** Small, voice only.
- Put `state_as_of` seconds on rows and `can_message` plus a reason on
  `read_session`.
- Add a "what changed since I last looked" result (started working, finished,
  needs you, with times), so one call replaces repeated reads.
- Report "started working" quietly for sessions in the call's context.
- Interjection policy: speak up unprompted only for sessions the voice started,
  sent to, or watched in this call; everything else stays on request.

**D. Announce every finishing session.** Rejected. With 12 to 14 recent sessions it
is noise, and the voice is meant to be a quiet relay.

### Recommendation

Do A and C now, in one change. They fix every failure in the call without new
infrastructure, and the new logging shows whether snapshot lag still loses
completions. Do B only if the logs show missed or late completions after that,
since it is the largest change and mostly pays off for latency. The refused-send
fix is its own change (see the reply-authority note) and should land first,
because auto-watch depends on being able to message the sessions it watches.

## Open points

- Should an unprompted "it finished" wait for a pause in speech or interrupt?
  Assumed: wait.
- Should the auto-watch also cover sessions the developer messages by hand? Assumed
  no: only what the voice did.
- Voice behavior changes need a live check; sound runs in the muted macOS Space,
  not on the host.
