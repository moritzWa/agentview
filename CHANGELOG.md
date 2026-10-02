# Changelog

All notable changes will be documented here. agentview uses a `0.x` release
line and may change before a stable release.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and released versions are intended to follow Semantic Versioning.

## [Unreleased]

### Changed

- Ctrl+X no longer opens a confirmation dialog to delete or hide a session or
  group. The first press shows `ctrl+x again to delete` (or `to hide`) on the
  row and the second press acts; any other key cancels. A stop still counts as
  the first press, so stop-then-delete remains Ctrl+X twice.
- Enter right after a `\` in the composer replaces the `\` with a line break
  instead of submitting, wherever the cursor is, as Claude Code does. Terminals
  such as Hyper report Shift+Enter as a plain Enter, so `\` then Return is the
  line break they can send.

### Fixed

- Pasting more than about a kilobyte into an opened session no longer drops
  you back to the dashboard with the rest of the paste in the composer. Input
  now waits until the session reads it, which also covers long tasks typed
  into a new session at launch.

## [0.1.0] - 2026-10-01

### Added

- First release of agentview: one terminal dashboard for sessions across 18
  coding harnesses plus Terminal, with pinning, reordering, hiding, background
  launches, durable OpenCode sessions, and a composer for new tasks.
