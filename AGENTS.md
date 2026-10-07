# Agent instructions

- Work in your own worktree (`git worktree add`), not the shared checkout at
  `~/Code/agentview`: several sessions edit this repo at once.
- Small changes go straight to `main`; larger ones get a branch and PR. Either
  way, push as soon as it builds and the unit tests pass. If the push is rejected,
  rebase onto `origin/main` and push again.
- After a change that affects the app, push it, then reinstall with
  `~/.claude/scripts/install-latest.sh agentview`. It fetches and builds
  `origin/main` in a temporary worktree. Never `cargo install` from a checkout:
  one that hasn't pulled drops whatever `main` has that it lacks.
