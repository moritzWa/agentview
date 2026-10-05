# Changelog

All notable changes will be documented here. agentview uses a `0.x` release
line and may change before a stable release.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and released versions are intended to follow Semantic Versioning.

## [Unreleased]

### Added

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

### Fixed

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
