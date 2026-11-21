# OpenCode V1 and V2 parity

Agentview keeps its V1 adapter and selects V2 explicitly. The migration does
not install a binary or restart the shared server.

## Baselines

- Agentview: `origin/main` at `9e9390f`.
- Stock V2 contracts inspected: OpenCode `origin/dev` at
  `594af423e8676b8458d6ef125ddde6766afad392`.
- Targeted TUI extensions come from the migration core stream. Stock V2 does
  not advertise these capabilities, so betteragentview uses separate frontends.

## Isolated configuration

Use a separate empty profile for tests. Keep every XDG directory and the
database separate from V1. The profile must stay the same for betteragentview, the
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

Without a registration, betteragentview starts its own authenticated loopback server.
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
  This is a storage-format preference, not arbitration between live servers.
  Keep duplicate V1/V2 IDs under V1 authority until a quiescent final handoff.
- Hide and close retain the existing betteragentview lifecycle. Read-only discovery
  and FTS queries never migrate or modify the provider database.

## Frozen-snapshot ownership handoff

Use this only after the coordinator has fenced V1 writes and verified terminal
tool outcomes. The coordinator must run outside the V1 server being stopped.
Take a fresh SQLite-consistent snapshot after quiescing V1, migrate it into a
clean V2 store, and validate the resulting messages. An old rehearsal clone is
not an incremental migration target: the V1 migrator records completion and V2
public import rejects existing IDs.

The target must already be running. Point betteragentview at its verified private
service registration and a new supervisor state directory. Handoff import does
not start, stop, restart, or prompt the server. The first manifest requires an
empty supervisor ownership map. Keep the target fenced from other writers
through import and continuation admission; HTTP validation is not a transaction
with an unrelated TUI or API client.

Write a private, current-user-owned regular file (`chmod 600 handoff.json`):

```json
{
  "version": 1,
  "handoffID": "frozen-v1-cutover-2026-10-17",
  "endpoint": {
    "url": "http://127.0.0.1:49152",
    "pid": 12345
  },
  "sessions": [
    {
      "id": "ses_example",
      "title": "Finish the frozen task",
      "directory": "/work/project",
      "agent": "build",
      "model": {"providerID": "openai", "id": "model-name", "variant": "high"},
      "lastMessageID": "msg_final_migrated_projection",
      "createdAtMs": 1792270000000,
      "updatedAtMs": 1792270100000,
      "sourceState": "interrupted",
      "resume": true,
      "toolsSettled": true,
      "externalActionsSettled": true
    }
  ]
}
```

Read metadata from target `GET /api/session/{id}` after migration. Use the exact
target agent/model/variant, timestamps, title, and location. `agent` and `model`
may be null. `lastMessageID` is the newest projected message returned by target
`GET /api/session/{id}/message?limit=1`, including a migration notice or control
message. It may be null only for an empty transcript. The endpoint uses the
verified loopback URL and current `/api/info` process ID.

`sourceState` is `interrupted`, `completed`, `awaitingInput`, or `uncertain`.
Only `interrupted` entries may set `resume: true`, and both settled flags must
then be true. Those flags attest the coordinator's checks; they do not prove
external actions are exactly-once. Completed, waiting, and uncertain entries
can be adopted with `resume: false` for deliberate native recovery later.

```sh
/path/to/agentview-test --opencode-bin /path/to/opencode-v2 \
  opencode handoff-import /private/path/handoff.json

# Separate explicit operation; import itself never resumes a session.
/path/to/agentview-test --opencode-bin /path/to/opencode-v2 \
  opencode handoff-resume frozen-v1-cutover-2026-10-17
```

Import validates the entire batch before granting ownership. It checks the
authenticated endpoint, exact session metadata and transcript watermark, and
absence of active execution. Selected roots must be top-level interrupted
assistant turns. Pending session forms or permissions, successful/failed terminal
outcomes, completed assistant replies, pending/running tool calls, and migrated
`tool.interrupted` outcomes block automatic continuation. Unknown manifest
fields and duplicate session IDs are rejected.

The private `$XDG_STATE_HOME/agentview/opencode/handoffs.json` journal stores the
exact manifest and a random `msg_...` input ID for each selected root. Agentview
fsyncs those IDs before adopting ownership or sending any continuation. Each
resume checks the target inbox and projected transcript for that same ID before
posting. A lost HTTP ACK or process restart therefore reuses the durable ID or
records it as already accepted. Re-importing the identical manifest is
idempotent; changing a recorded manifest cannot expand its authority.

Adopted sessions gain the ordinary managed Reply/Interrupt controls and exact
session attach. Source creation timestamps and titles remain intact. Existing
V1 records and behavior remain separate.

Ordinary server restart does not inject an anonymous second `continue` into an
unfinished handoff continuation. Run `handoff-resume` again to reconcile its
admission receipt. If V2 execution itself was interrupted after admission, use
native recovery to inspect the new tool outcomes. A genuinely new user input
ends that initial handoff protection and returns to ordinary restart behavior.
The journal deduplicates input admission, not tool execution.

## Verification

Run builds and tests through the shared resource guard:

```sh
python3 ~/.claude/scripts/heavy-job.py --wait -- cargo test --all-targets
python3 ~/.claude/scripts/heavy-job.py --wait -- cargo build

# Canonical CI quality and MSRV gates:
python3 ~/.claude/scripts/heavy-job.py --wait -- cargo +1.75.0 fmt --all -- --check
python3 ~/.claude/scripts/heavy-job.py --wait -- cargo +1.75.0 clippy --all-targets --locked -- -D warnings
python3 ~/.claude/scripts/heavy-job.py --wait -- cargo +1.75.0 test --locked
```

The opt-in real CLI contract uses the isolated environment above:

```sh
export AGENTVIEW_REAL_OPENCODE_BIN=/absolute/path/to/opencode-v2
python3 ~/.claude/scripts/heavy-job.py --wait -- \
  cargo test --test opencode_v2 -- --ignored --nocapture

# Actual imported-session adoption, managed controls, and lost-ACK replay:
python3 ~/.claude/scripts/heavy-job.py --wait -- \
  cargo test --test opencode_v2_handoff -- --ignored --nocapture
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
- [x] Complete local macOS `cargo test --locked` runs on both Rust 1.75.0 and
  stable passed without name filters or exclusions. Each run includes 712
  library tests, 21 CLI tests, 5 README tests, and 24 real-PTY tests. The
  repository's pre-existing ignored annotations retain their original status; no
  additional tests were ignored or skipped to make these runs pass.
- [x] CLI unit tests: 21 passed. The real handoff test also invokes both new
  CLI subcommands against the actual target server.
- [x] Debug build.
- [x] Rust 1.75 formatting and all-target Clippy with warnings denied.
- [x] Isolated installer/package fixture tests.
- [x] Five restart fixtures normally ignored on macOS were explicitly enabled
  and passed on Rust 1.75. Reboot recovery now reads real persisted SQLite rows;
  behavioral checks exclude question-blocked, completed, child, pre-server, and
  post-boot replies. The fixture no longer simulates production SQL output.
- [x] Windows common supervisor/CLI imports type-check across library, binary,
  and test targets for `x86_64-pc-windows-msvc`.
  The local cross-check uses host SQLite pkg-config metadata only; native Windows
  C compilation, linking, tests, and startup remain the CI runner's checks.
- [x] Baseline blockers repaired on this branch: MSRV process-probe compilation,
  exact visible README inventory, percentage-forecast usage assertions, and
  count-free PTY row/visibility/pagination checks. A Claude fixture now handles
  asynchronous model-help probes without overwriting background-launch state.
  Responsiveness, exact inventory/row checks, and terminal-restoration assertions
  remain enforced. Original tracking:
  [Restore the failing baseline CI checks](https://github.com/moritzWa/betteragentview/issues/32).
- [x] Compiled stock V2: actual database discovery and FTS search; private
  service registration discovery; restart with renewed registration PID;
  authenticated reconnect; and shutdown. The real test uses a fresh temporary
  database and private XDG directories on every run.
- [x] Actual unified V2 handoff test: imported existing IDs, private manifest
  and whole-batch validation, managed Reply and successful Interrupt, pending
  forms/permissions rejected at import, a form arriving after adoption shown as
  Needs input and rejected at resume, completed/unselected roots never continued, and changed manifests
  rejected. A local token-free provider keeps execution active for Interrupt.
- [x] Lost-ACK/restart proof: restore the exact private pre-POST journal after
  the real continuation was admitted; restart while its provider stream is
  active; reconcile the identical manifest/input ID. The selected session has
  exactly one `continue` user message and restart adds no anonymous second input.
  The final guarded real handoff test passed in 2.55 seconds on macOS.
- [x] Integrated core + UI candidate: two actual live PTYs with distinct
  `OPENCODE_TUI_CLIENT_ID` values and different starting directories. Authenticated
  `/api/tui/select-session` switched each exact target across directories; the
  actual UI route/session, UI location, session title, and OSC terminal title
  converged. The other TUI's route/location/title and terminal-title stream stayed
  unchanged during a one-second observation window after each switch.
- [x] Targeted `/api/tui/execute-command` dispatch: a temporary harmless TUI
  fixture registered `speech.pause`; each target executed it exactly once while
  the other TUI did not. The fixture only observes state and registers this
  command; real selection and command filtering run through the candidate UI.
  **This proves dispatch, not read-aloud/audio pause.**
- [ ] Actual read-aloud pause remains manager-owned VM acceptance.

Targeting proof (2026-10-17): source launcher from `packages/cli` passed in
8.21 seconds; unified compiled `0.0.0-migration-ui` passed in 7.40 seconds.
Both runs used fresh temporary HOME/XDG/config/database directories, no provider
credentials or production plugins, and verified all server/TUI process groups
absent after cleanup. The compiled run additionally asserted the non-target OSC
terminal-title stream stayed unchanged. Reproduce either candidate with:

```sh
python3 ~/.claude/scripts/heavy-job.py --wait -- \
  python3 scripts/opencode_v2_targeting.py --bin /absolute/path/to/opencode-v2
# Source alternative; script sets cwd to packages/cli for JSX configuration:
python3 ~/.claude/scripts/heavy-job.py --wait -- \
  python3 scripts/opencode_v2_targeting.py --source /absolute/path/to/opencode-checkout
```

The proof defaults to a 95-second execution deadline (`--timeout`, maximum 110).
Its `finally` terminates each private process group, escalates to SIGKILL if
needed, verifies group disappearance, closes PTYs, and removes the temp profile.

## Remaining integration boundaries

- Docker V2 discovery bypasses the host direct SQLite adapter. This migration
  validates the host workflow; remote V2 database transport needs its own path.
- An unmanaged stock V2 root TUI has no model flag. Agentview offers its default
  model there and rejects an explicit model before spawn. The normal managed
  launch preserves selected models through the HTTP controls.
- Actual read-aloud/audio integration remains manager VM acceptance. Manager
  owns quiescence, cutover, installation, and shared-server restart.
