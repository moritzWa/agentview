# Changelog

All notable changes will be documented here. agentview uses a `0.x` release
line and may change before a stable release.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and released versions are intended to follow Semantic Versioning.

## [Unreleased]

### Fixed

- A managed OpenCode session waiting on a permission or question prompt, its
  own or a subagent's, now shows as needing input instead of working. OpenCode
  reports such a turn as busy, so it used to sit at "Working" indefinitely.

### Changed

- Ctrl+X no longer opens a confirmation dialog to delete or hide a session or
  group. The first press shows `ctrl+x again to delete` (or `to hide`) on the
  row and the second press acts; any other key cancels. A stop still counts as
  the first press, so stop-then-delete remains Ctrl+X twice.

## [0.1.0] - 2026-10-01

### Added

- First release of agentview: one terminal dashboard for sessions across 18
  coding harnesses plus Terminal, with pinning, reordering, hiding, background
  launches, durable OpenCode sessions, and a composer for new tasks.
