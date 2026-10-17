# OpenCode V1 and V2 parity

Agentview keeps its V1 adapter and selects V2 explicitly. The migration does
not install a binary or restart the shared server.

## Baselines

- Agentview: `origin/main` at `9e9390f`.
- Stock V2 contracts inspected: OpenCode `origin/dev` at
  `594af423e8676b8458d6ef125ddde6766afad392`.
- Targeted TUI extensions come from the migration core stream. Stock V2 does
  not advertise these capabilities, so agentview uses separate frontends.

## Isolated configuration

Use a separate empty profile for tests. Keep every XDG directory and the
database separate from V1. The profile must stay the same for agentview, the
server, and attached TUIs.

```sh
export PROFILE="$HOME/.local/share/agentview-v2-test"
export XDG_CONFIG_HOME="$PROFILE/config"
export XDG_DATA_HOME="$PROFILE/data"
export XDG_CACHE_HOME="$PROFILE/cache"
export XDG_STATE_HOME="$PROFILE/state"
export OPENCODE_CONFIG_DIR="$PROFILE/config/opencode"
export OPENCODE_DB="$PROFILE/data/opencode/opencode.db"
export AGENTVIEW_OPENCODE_API=v2

# Use the candidate's absolute path. Do not replace the installed app.
/path/to/agentview-test --opencode-bin /path/to/opencode-v2
```

Without `AGENTVIEW_OPENCODE_API=v2`, new supervisor records use V1. Existing
records retain their protocol across reconnection and restart. Changing the
protocol requires a separate supervisor state directory, not reuse of a live
V1 record.

Agentview can discover an existing V2 service through its private registration:

```sh
export AGENTVIEW_OPENCODE_SERVICE_FILE="$XDG_STATE_HOME/opencode/service.json"
```

The filename depends on the CLI's channel; local builds commonly use
`service-local.json`. Use the registration the candidate actually writes.
Agentview reads `url`, `pid`, and `password`; checks private ownership and file
permissions; requires a loopback HTTP URL; verifies the listener PID; and checks
the authenticated `/api/info` PID. Invalid registration fails instead of starting
a competing server. Keep the candidate's service config and XDG paths when
restarting a discovered service, so its CLI can renew the registration and reuse
its saved password.

Without a registration, agentview starts its own authenticated loopback server.
Its private record is under `$XDG_STATE_HOME/agentview/opencode`. Secrets travel
in child environment variables and HTTP Basic authentication, not attach argv.

For source-only tests, use a small executable launcher that runs
`bun run src/index.ts "$@"` from OpenCode's `packages/cli` directory. The CLI's
local JSX configuration is required. A compiled candidate needs no launcher.

## Contracts

| Behavior | V1 | V2 |
| --- | --- | --- |
| Readiness | `/global/health` | `/api/info`, exact server PID |
| List / status | `/session`, `/session/status?directory=` | `/api/session`, global `/api/session/active` |
| Create | Directory query | `location.directory` in create body |
| Prompt | `prompt_async`, text parts and model fields | `/prompt`, `text`; durable `/model` and `/agent` controls first |
| Interrupt | `/abort` | `/interrupt` |
| Transcript | `/message`, `{info,parts}` | `/message`, `{data,cursor}` and typed messages |
| Model picker | `/config/providers` | `/api/model` plus `/api/provider` display names |
| Waiting | `/question`, `/permission` | `/api/form`, `/api/permission/request` |
| Native attach | `attach URL --dir DIR --session ID` | `--server URL --session ID DIR` |
| Store | `session`, `message`, `part` | `session_v2`, `session_message`; read-only temporary compatibility views |
| Database path | `db path` | `debug paths db` |

List and full transcript reads follow V2 cursors. Recent-message reads retain
their requested limit. Transcripts render oldest-first. Disabled and deprecated
models stay out of the picker.

### Exactly one TUI

Agentview requires `/api/info` to advertise
`capabilities.tuiSessionSelect=true` before sharing a V2 frontend across
directories. Each dashboard sets a stable `OPENCODE_TUI_CLIENT_ID` on its child.
Agentview sends authenticated:

```json
POST /api/tui/select-session
{"sessionID":"ses_example","client":"agentview-12345","targetDirectory":"/work/project"}
```

The server emits global `tui.session.select` with the same fields. The TUI
matches its client identity and changes location before navigation. Agentview
also checks the server PID to reject switches sent during server replacement.
Missing capability never triggers a broadcast fallback.

Speech pause on detach requires `capabilities.tuiExecuteCommand=true`:

```json
POST /api/tui/execute-command
{"command":"speech.pause","client":"agentview-12345"}
```

Stock V2 without that extension cannot pause one frontend remotely. Agentview
does not broadcast a command to unrelated TUIs.

### Restart, state, and history

- Restart captures globally active top-level V2 sessions, their directories,
  agent, model, and variant. It resumes interrupted turns with `continue`.
- Pending forms and permissions remain waiting states instead of receiving an
  automatic continuation. Child turns are not resumed independently.
- Crash recovery reads V2 message sequence and explicit idle markers. It does
  not mistake a finished turn for an unfinished assistant message.
- Parent IDs preserve the session hierarchy. Fork provenance does not create
  a parent-child relationship. Existing monitor/background-shell hold files
  continue to overlay live server state.
- Failed idle V2 sessions and pending forms/permissions need input. Global
  active sessions remain working even when their directory has no frontend.
- FTS restore search includes user and assistant text, including nested child
  sessions. It excludes reasoning, tool payloads, and synthetic messages.
- Mixed stores prefer migrated V2 IDs while retaining unmigrated V1 history.
  Streaming edits, message removal, and V1-to-V2 migration refresh the index.
- Hide and close retain the existing agentview lifecycle. Read-only discovery
  and FTS queries never migrate or modify the provider database.

## Verification

Run builds and tests through the shared resource guard:

```sh
python3 ~/.claude/scripts/heavy-job.py --wait -- cargo test --all-targets
python3 ~/.claude/scripts/heavy-job.py --wait -- cargo build
```

The opt-in real CLI contract uses the isolated environment above:

```sh
export AGENTVIEW_REAL_OPENCODE_BIN=/absolute/path/to/opencode-v2
python3 ~/.claude/scripts/heavy-job.py --wait -- \
  cargo test --test opencode_v2 -- --ignored --nocapture
```

Recorded checks:

- [x] Existing V1 supervisor tests: 16 passed before the V2 additions.
- [x] Frontend adapter tests: 27 passed, including V1/V2 model arguments,
  cross-directory client identity, and server-replacement races.
- [x] DB/live/background tests: 39 passed before the FTS additions.
- [x] V2 HTTP fixture: authentication, model-before-prompt, cursor traversal,
  projected transcript, and explicit capability checks.
- [x] Real stock V2 source lifecycle: create, prompt, reconnect, list,
  transcript, model/provider catalog, attach argv, reply, interrupt, restart,
  retained session, and verified shutdown. Passed in 5.09 seconds on macOS.
- [x] Final library run: 708 passed, 2 ignored. The existing usage-header test
  was excluded after its failure was reproduced on untouched `9e9390f`.
- [x] Debug build.
- [x] Full `--all-targets --no-fail-fast` run. README inventory and five PTY
  tests failed; the same failures were reproduced on untouched `9e9390f`
  (which had nine PTY failures). Existing tracking:
  [Restore the failing baseline CI checks](https://github.com/moritzWa/agentview/issues/32).
- [x] Compiled stock V2: actual database discovery and FTS search; private
  service registration discovery; restart with renewed registration PID;
  authenticated reconnect; and shutdown. The real test uses a fresh temporary
  database and private XDG directories on every run.
- [ ] Integrated core candidate: two live TUIs, exact-client cross-directory
  navigation, and targeted speech pause. Stock V2 cannot verify those extensions.

## Remaining integration boundaries

- Docker V2 discovery bypasses the host direct SQLite adapter. This migration
  validates the host workflow; remote V2 database transport needs its own path.
- An unmanaged stock V2 root TUI has no model flag. Agentview offers its default
  model there and rejects an explicit model before spawn. The normal managed
  launch preserves selected models through the HTTP controls.
- A live UI test still needs the core and interface patches in the same
  candidate. Manager owns cutover, installation, and shared-server restart.
