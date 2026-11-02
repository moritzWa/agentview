# Changelog

All notable changes will be documented here. agentview uses a `0.x` release
line and may change before a stable release.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and released versions are intended to follow Semantic Versioning.

## [Unreleased]

### Added

- Voice session reads include bounded running-shell progress: command excerpt,
  folder, elapsed time and recent output. Resumed calls and screen switches get
  fresh on-screen context with two recent exchanges; content questions prompt a
  read rather than guessing from titles. Routine actions get a short "done".
  Parked calls no longer show a badge inside OpenCode.
- Shared history search refreshes every 30 seconds instead of ten minutes.
  Voice search counts matches for each query word correctly.

- Voice Mode: press Ctrl+K (or type `/voice`) and talk to your OpenCode sessions
  while your hands are busy. A pinned browser tab holds the microphone and
  speaks with OpenAI's GPT-Live; it can list the sessions you are working in,
  read what one last said, send it a message, stop it, correct your last
  message, start a new session, and tell you when one finishes. It lists your
  most recent sessions by when you last wrote to them, not every parked one.
  The first call asks for an OpenAI API key with GPT-Live access
  (`agentview voice --set-key` takes one on stdin). See `docs/voice.md`.

- Voice Mode runs its browser hidden: agentview starts a headless Chromium-based
  browser for the call and stops it afterwards, so no tab opens. Cmd+K works as
  well as Ctrl+K, and inside a session's own view Ctrl+K (OpenCode), Cmd+K or
  Ctrl+] start and end a call. The voice knows which session is selected or
  open, and ends the call when you say goodbye.

- Voice Mode: ending a call pauses it for 30 minutes and the next `Cmd+K`
  continues it (`/voice new` starts fresh); `Cmd+S` in an OpenCode session
  pauses a live call and still starts your read-aloud plugin; pressing `Cmd+K`
  with a draft in the OpenCode input box lets the voice help you think it
  through and send the final message (`send_draft`), clearing the box; Grok
  hears you from the first moment of a call.

- The Voice Mode badge in a session's view stays at the bottom right, below the
  input (on the status row, or the empty row under it on a narrow terminal),
  and no longer jumps to the top right corner.

- Voice Mode goes quiet about 20 ms after the end key (it took over a second):
  the page cuts the audio first. The hidden browser stays up two minutes after a
  call, and Grok's token is fetched while it starts, so a Grok call is live in
  about 1 second cold and 0.4 warm.

- Voice Mode can run on xAI's Grok (`grok-voice-latest`, default voice `rex`,
  no reasoning so it stays fast) as well as OpenAI. Switch with `/voice grok` or
  `/voice openai`, or `agentview voice --provider grok`; each provider has its
  own key. Grok's speed (0.7x to 1.5x) has clickable `−` and `+` in the
  dashboard's call line and in the OpenCode footer badge, and `/voice speed 1.2`.

- Voice Mode in a session's own view: the badge is blue text without a
  background, and starting or ending a call no longer writes a status line on
  the bottom row (only failures and things to act on do).

- Voice Mode: a short answer ("go", "continue", "sure") now goes to the session
  that asked, word for word, instead of being turned into a plan the voice
  carries out itself (it once started four sessions for one "go"). Reading a
  session gives what you asked and how each turn ended, without tool calls.
  New: searching the whole history (`search_history`) and closing sessions
  (`close_session`: stops them, removes them from Agent View). Every call now
  leaves a full log with the words, the last ten kept.

- The voice key now works in a session's own view when the terminal reports it
  with lock keys on (Caps Lock, Num Lock) or as a kitty key event with extra
  fields, and Ctrl+K in the kitty encoding counts in OpenCode. Control keys that
  arrive in a session's view are traced in `perf.log` (`voicekey`, never typed
  text), so a key that does nothing can be told from one that never arrived.

- Voice Mode no longer lets a tab left open from an earlier version take over a
  call (it made calls hang on a microphone prompt nobody could see). A call
  that waits for the microphone says so and how to allow it, a call that does
  not connect says which step failed, every call leaves a words-free
  `last-call.log`, and a badge shows in a session's own view while a call is on.

- An OpenCode session's age now follows its latest real message. A model or
  agent switch used to bump it, so several sessions could all read "6 hours".

- When [agent-bouncer](https://github.com/moritzWa/agent-bouncer) is installed,
  a panel above the input lists its running and waiting heavy jobs and when the
  queue should clear, and a Working session whose tool call waits in line reads
  `#1 in line for <job>`.

- Ctrl+B keeps the model and switches who serves it, from the session list or
  the new-task composer: OpenCode's `anthropic/claude-opus-5-5` becomes
  `cursor/claude-opus-5-5` and back. The header shows what a new task starts
  with, and the model picked for each harness is remembered across restarts.

- Shift+Tab opens the model picker from the session list too, not only from an
  open composer. While a new-task draft is a bare `/word`, the footer lists the
  commands it could become.

- An OpenCode session whose Cursor model left a command running in the
  background, such as a watcher the agent reports back from, shows as working
  until that command exits, instead of as done or waiting for input.
  agentview finds these from the logs `cursor-opencode-provider` writes to the
  temp directory.

- Claude Code sessions stay on the dashboard as completed after their process
  exits, read from the transcripts under `~/.claude/projects`, and opening one
  runs `claude --resume` in its folder. A running interactive Claude session
  that is waiting for a prompt shows as completed instead of unknown.

- Codex, Claude Code, Devin, and the other CLIs agentview opens now show a
  reply as working as soon as their window does, and a permission prompt as
  needing input, read from the screen of the window agentview keeps behind
  the dashboard, as OpenCode and Cursor already did. When a window stops
  showing a running turn, the dashboard refreshes at once instead of at the
  next interval. Codex sessions that no Codex process has loaded show as
  completed after two quiet minutes instead of as unknown.

- A long paste into the new-task composer or a reply shows as
  `[Pasted ~N lines]`, as in OpenCode, and the full text is sent on submit.

- `agentview opencode restart` restarts agentview's OpenCode server on the same
  port, so open OpenCode windows reconnect, and sends `continue` to every
  session whose turn the restart cut off.

- After a reboot, the first OpenCode server the dashboard starts sends
  `continue` to every top-level session whose reply the shutdown cut off,
  found in the OpenCode database. `agentview opencode stop` stops the server
  and leaves the same resume for its next start.

- Opening OpenCode sessions reuses one OpenCode window per folder, switching it
  to the chosen session instead of starting a window per session (2 to 3
  seconds and about 300 MB each). After `agentview opencode restart`, the next
  open starts a fresh window, so OpenCode and plugin changes take effect. This
  needs an OpenCode server that can switch a single window; with older
  servers, sessions open in their own windows as before.

### Changed

- Ctrl+X no longer opens a confirmation dialog to delete or hide a session or
  group. The first press shows `ctrl+x again to delete` (or `to hide`) on the
  row and the second press acts; any other key cancels. A stop still counts as
  the first press, so stop-then-delete remains Ctrl+X twice.
- Enter right after a `\` in the composer replaces the `\` with a line break
  instead of submitting, wherever the cursor is, as Claude Code does. Terminals
  such as Hyper report Shift+Enter as a plain Enter, so `\` then Return is the
  line break they can send.
- Sessions started in a system temp directory such as `/tmp` are hidden, since
  agents run throwaway sessions there and leave them behind. Pinned sessions
  stay visible, and `--include-temp` shows the rest.

- The first load shows placeholder rows shaped like the list instead of the
  empty-state text, and keeps them under any rows that arrive early, until every
  provider has answered.

### Fixed

- The session list appears in a fraction of a second on a large OpenCode V2
  history, where it took about six. Startup no longer scans the whole message
  table for background shells, enriches every partial result, fetches the
  session list once per folder, or starts OpenCode to ask where its database is
  (the answer is remembered between launches).
- An idle Devin, Hermes Agent, MastraCode, or other native harness session
  held in the background shows as completed instead of working. Its screen
  decides: a turn in progress still shows as working.
- An empty or relative `XDG_STATE_HOME`, `XDG_DATA_HOME`, `XDG_CONFIG_HOME`,
  or `XDG_CACHE_HOME` is ignored as the XDG spec requires, instead of putting
  agentview's state under the directory it was started in.
- Codex sessions keep working after Codex updates itself. agentview restarts
  its background Codex App Server once the installed Codex version changes and
  no owned turn is running, and stops the outdated server on Linux and macOS,
  which otherwise kept threads loaded so Codex refused to open them.
- Pressing Left at an empty prompt in an opened Claude Code session returns
  to the dashboard instead of opening Claude's own agent view, including in
  background sessions agentview opens with `claude attach`.
- Devin sessions are found on macOS. Devin 3000.11 keeps its session database
  under `~/.local/share/devin/cli/` (or `$XDG_DATA_HOME`) on macOS as on
  Linux, while agentview looked only in `~/Library/Application Support`, so
  no Devin history ever appeared there.

- Pasting more than about a kilobyte into an opened session no longer drops
  you back to the dashboard with the rest of the paste in the composer. Input
  now waits until the session reads it, which also covers long tasks typed
  into a new session at launch.

- Replying in an opened OpenCode or Cursor session and returning to the
  dashboard shows it as working within a fraction of a second, instead of
  after the next full provider refresh. The dashboard now reads the screens
  of sessions it keeps in the background on every tick.

- OpenCode sessions running in a terminal outside agentview now show as
  working or waiting when `--opencode-bin` names a renamed build such as
  `opencode-dev`, instead of as completed. Processes were recognized only
  when the executable was called exactly `opencode`.

## [0.1.0] - 2026-10-01

### Added

- First release of agentview: one terminal dashboard for sessions across 18
  coding harnesses plus Terminal, with pinning, reordering, hiding, background
  launches, durable OpenCode sessions, and a composer for new tasks.
