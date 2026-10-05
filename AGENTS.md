# Agent instructions

- Commit and push every change you make as soon as it builds. Never leave
  finished work only in the local checkout.
- Commit only your own change. Other uncommitted files may be someone else's
  in-progress work: stage just your hunks and leave the rest untouched.
- If the push is rejected, rebase onto `origin/main` and push again.
- After a change that affects the installed app, reinstall from the pushed
  `main` (`cargo install --path . --locked --root "$HOME/.local" --force`).
