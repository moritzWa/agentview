# Agent instructions

- Work in your own worktree (`git worktree add`), not the shared checkout at
  `~/Code/agentview`: several sessions edit this repo at once.
- Small changes go straight to `main`; larger ones get a branch and PR. Either
  way, push as soon as it builds and the unit tests pass. If the push is rejected,
  rebase onto `origin/main` and push again.
- After a change that affects the app, reinstall from the pushed `main`
  (`cargo install --path . --locked --root "$HOME/.local" --force`), not from a
  feature branch: an install from a branch drops whatever `main` has that it
  lacks.
