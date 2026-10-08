# CLI and keyboard reference

This reference describes the current checkout. `agentview --help` and
`agentview <subcommand> --help` remain authoritative for the installed
binary. `av` is the installed shorthand. Both commands execute the same canonical
binary and report `agentview VERSION`.

## Dashboard and JSON options

```text
agentview [OPTIONS]
```

Version and self-update commands:

```console
agentview --version   # -v and -V are accepted
av update
av upgrade                # alias of update
```

`update` downloads the repository's public installer over HTTPS and runs it for
the current install directory. Neither installation nor updating requires the
GitHub CLI. The installer still resolves a
published release asset and verifies its SHA-256 checksum before replacing the
binary. `AGENTVIEW_REPO` and `AGENTVIEW_INSTALL_DIR` retain their documented installer
overrides; `GH_TOKEN` is optional and only needed for API rate limits or private
forks. The final line verifies the binary that was installed and reports
`Updated agentview from X to Y.`; if both versions match, it reports that
agentview is already up to date.

| Option | Meaning |
| --- | --- |
| `--json` | Print a normalized snapshot and do not enter the TUI. |
| `--theme auto\|dark\|light` | Dashboard colors. `auto` (the default) reads the terminal background with OSC 11, then `COLORFGBG`, then the operating system appearance (macOS `AppleInterfaceStyle`, Windows `AppsUseLightTheme`, GNOME `color-scheme` when a desktop session is present). If none of these answer, the dark palette is used. While the dashboard runs, `auto` polls the operating system appearance every 2 seconds and switches palettes when it changes; it stops polling after 5 failed lookups in a row. `dark` and `light` force a palette. |
| `--yolo` | **Dangerous, explicit opt-in.** Launch new sessions with a verified provider-native permission-bypass mode. Unsupported harnesses fail closed. Existing sessions are not changed. |
| `--all` | Compatibility flag that explicitly includes completed sessions; completed is already the default. |
| `--hide-completed` / `--active-only` | Hide completed sessions at startup. `/completed show` restores them without restarting. |
| `--include-interactive` | Include provider sessions reported as foreground/interactive. |
| `--include-temp` | Include sessions whose working directory is a system temp directory (`/tmp`, `/var/tmp`, `/var/folders`, `$TMPDIR`). They are hidden by default because agents start throwaway sessions there; pinned sessions and `--cwd` inside a temp directory still show them. |
| `--include-external` | Include provider sessions not created or managed by agentview. External history is excluded by default. |
| `--history-limit N` | Read at most `N` persisted-history records per provider per refresh; default 100, range 1–10,000. Live/owned inventories are separate. |
| `--cwd PATH` | Keep sessions whose working directory starts with `PATH`. |
| `--fixture FILE` | Read a normalized snapshot/session array instead of probing providers; all provider operations are fenced. |
| `--no-host-providers` | Disable every host provider while retaining explicit Docker targets. |
| `--claude-bin PATH` | Use a particular Claude executable; default `claude`. |
| `--no-host-claude` / `--no-claude` | Disable host Claude discovery and control. Every `--no-host-<provider>` flag also accepts the shorter `--no-<provider>` form. |
| `--codex-bin PATH` | Use a particular Codex executable; default `codex`. |
| `--no-host-codex` | Disable host Codex discovery and supervision. |
| `--pi-bin PATH` / `--pi-session-dir PATH` | Select Pi and optionally override its documented history store. |
| `--no-host-pi` | Disable host Pi history and managed supervision. |
| `--opencode-bin PATH` / `--no-host-opencode` | Select or disable OpenCode history and launch: durable managed supervision on Linux, OpenCode's own interface elsewhere. |
| `--copilot-bin PATH` / `--no-host-copilot` | Select or disable persisted Copilot discovery and process-local managed ACP control. |
| `--cursor-bin PATH` / `--no-host-cursor` | Select or disable Cursor support on the host. External chats are read from Cursor's own chat store with `--include-external --include-interactive`. On Linux, launch and inline reply go through agentview's managed supervisor. Elsewhere, choosing Cursor in the harness list runs `cursor-agent create-chat` and opens that chat with the prompt in Cursor's interface; agentview records the chat id in `cursor-owned.json`, so its row is listed without `--include-external`. Inline reply stays Linux-only. |
| `--cursor-chats-dir PATH` | Override the chat store Cursor's CLI writes to (default `~/.cursor/chats`). |
| `--antigravity-bin PATH` / `--no-host-antigravity` | Select or disable host Antigravity discovery. |
| `--mistral-vibe-bin PATH` / `--mistral-vibe-app-server-bin PATH` / `--no-host-mistral-vibe` | Select or disable Mistral Vibe native control and app-server discovery. |
| `--muse-bin PATH` / `--no-host-muse` | Select or disable Muse Code native control and owned local-history discovery. |
| `--qwen-bin PATH` / `--no-host-qwen` | Select or disable Qwen Code native control plus bounded session/live discovery. |
| `--kimi-bin PATH` / `--no-host-kimi` | Select or disable Kimi Code native control and owned local-index discovery. |
| `--omp-bin PATH` / `--no-host-omp` | Select or disable Oh My Pi native control plus bounded JSONL discovery. |
| `--grok-bin PATH` / `--no-host-grok` | Select or disable Grok native control plus bounded summary/update discovery. |
| `--kilo-bin PATH` / `--no-host-kilo` | Select or disable Kilo Code native control plus bounded, read-only metadata discovery through `kilo db`. |
| `--openhands-bin PATH` / `--no-host-openhands` | Select or disable OpenHands native control plus bounded event-store discovery. |
| `--docker-container NAME_OR_ID` | Observe Claude and Codex in one explicitly selected running container; repeatable. |
| `--docker-bin PATH` | Use a particular Docker executable; default `docker`. |
| `--session-migrate-bin PATH` | Use a particular session-migrate executable for `ctrl+m` moves and `/migrate`; default `session-migrate`. |
| `--harness` / `--launch-provider claude\|codex\|pi\|omp\|opencode\|cursor\|copilot\|antigravity\|mistral-vibe\|muse\|qwen\|kimi\|grok\|kilo\|openhands\|terminal` | Initial harness for new-session prompts; default Claude. Each configured coding harness opens its native full-screen UI; Terminal opens the user's shell. `oh-my-pi` and `kilo-code` are accepted aliases. |
| `--launch-cwd PATH` | Working directory for newly launched host sessions; default current directory. |
| `--refresh-ms N` | Refresh interval, at least 250 ms; default 15000 ms. Refresh runs off the input thread, and first-launch results appear provider by provider. Use `ctrl+l` for an immediate refresh. |

The `--managed-docker-registry PATH` global option applies to the managed
Docker subcommands described below. Provider discovery warnings appear in the
snapshot rather than hiding healthy sessions from another adapter.

### Explicit YOLO mode

`av --yolo` is deliberately off by default. While it is active, the dashboard,
new-task composer, and every native session launched in that mode display a
persistent warning. Returning to or resuming the native screen does not clear
the warning. Stop agentview and restart without `--yolo` to return new launches to
their normal security settings.

agentview enables the mode only where the installed harness exposes a verified native
equivalent:

| Harness | Native setting used for a new session |
| --- | --- |
| Claude Code | `--dangerously-skip-permissions` |
| OpenAI Codex | `--dangerously-bypass-approvals-and-sandbox` for the native CLI; App Server uses `approvalPolicy=never` and `sandbox=danger-full-access` |
| Cursor | `--force` (Linux managed sessions) |
| Antigravity | `--dangerously-skip-permissions` |
| Mistral Vibe | `--auto-approve` (the native `--yolo` alias) |
| Muse Code | `--yolo` |
| Qwen Code | `--yolo` |
| Kimi Code | `--yolo` |
| Oh My Pi | `--yolo` |
| Grok | `--yolo` |
| Kilo Code | `--yolo` |
| OpenHands | `--always-approve` |

Pi, OpenCode, GitHub Copilot, and Terminal are intentionally unsupported.
Pi's `--approve` only trusts a project; it is not a permission bypass. agentview's
managed OpenCode path starts a server and attaches a client, and the attach
command does not accept OpenCode's top-level YOLO flag. No equivalent is claimed
for Copilot or a shell. Selecting any unsupported harness while `--yolo` is
active returns an error before a provider process is launched. agentview never guesses
at a similar-looking flag.

The setting applies only to new sessions created by this agentview process. It does
not weaken discovery, adopt external sessions, change credentials, or grant agentview
additional control over an existing session.

Bare provider command defaults are resolved from `PATH` first, then from the
provider's conventional user-local install directories. This includes
`~/.local/bin`, `~/.npm-global/bin` for Codex/Copilot,
`~/.opencode/bin`/`~/.bun/bin` for OpenCode, `~/.cursor/bin`, and
`~/.antigravity/bin`. An explicit path is never replaced by a guessed one.

Fixture mode is intentionally non-operational even when the JSON advertises
synthetic capabilities: launch, inspect, open, reply, approve/decline,
structured response, interrupt, archive, and delete all refuse before provider
I/O. This makes committed fixtures safe for real-TTY interaction tests.

Useful read-only invocations:

```console
agentview --json
agentview --hide-completed
agentview --include-external --history-limit 500
agentview --json --no-host-claude --no-host-codex
agentview --json --cwd /absolute/project
agentview doctor
agentview doctor --json
agentview doctor --docker-container exact-name-or-id
```

Install one missing provider CLI through its official user-local installer:

```console
agentview setup HARNESS
agentview setup HARNESS --yes
```

`HARNESS` accepts the eighteen coding-harness values plus `terminal` (which is built
in and needs no installation). Without `--yes`, setup requires
an interactive terminal confirmation naming the exact download/package source.
In non-interactive use it refuses before network or installer execution. Shell
installers are staged in a private temporary directory, download with a visible
curl progress bar, and are removed afterward. npm installs retain npm's native
progress. Restart the dashboard after setup so executable discovery runs again.

`--harness` chooses the initial composer harness; `--launch-provider` remains
an alias. In the new-task composer, `tab` opens a palette containing only
configured launch-capable harnesses. Arrow keys or `tab` preview, `1`–`9`
select directly, `enter` confirms, and `esc` returns without changing the
harness or losing the draft. `/harness NAME` selects explicitly;
`/provider NAME` remains a compatibility alias.

For every installed launch-capable provider, `shift+tab` from the new-task composer
opens an asynchronous searchable picker while preserving the current draft;
`/model` opens the same picker as a command. When Terminal is selected,
`/shell` is an equivalent, clearer alias and the picker lists shells rather
than models. Type to filter, use arrows or `tab`
to move, `page up`/`page down` to move ten choices, `enter` to select, and
`esc` to keep the previous selection and draft. After a successful catalog
load, **Default** is available alongside the exact account models. An
authentication/catalog failure does not offer a blind default launch; it
offers the native sign-in handoff instead. If catalog retrieval itself is
unavailable but the provider supports an exact identifier, type that ID in the
error-state picker and press Enter. Antigravity is deliberately excluded from
this fallback because its CLI validates `--model` against the same unavailable
catalog. Press Enter/`l` for native recovery and Ctrl+R to retry instead.
`/model NAME` accepts an exact custom
identifier without loading the catalog, and `/model default` resets the
selection. `/login` hands the terminal to the selected provider's native
authentication/setup UI. When the catalog reports an authentication failure,
the picker offers `enter`/`l` for that handoff and reloads the same account
catalog automatically. The selected harness/model (or Terminal shell) is always displayed in the composer
border before submission. Catalog sources are provider-native: Claude parses
aliases advertised beside `--model` in `claude --help`; Codex requests all
visible pages of App Server `model/list`; Pi parses `pi --offline
--list-models`; OpenCode parses `opencode models`; Cursor parses `cursor-agent
models`; Copilot queries its headless SDK `models.list` without creating a
session; Antigravity parses `agy models`; Oh My Pi parses
`omp models list --no-extensions --json`; Grok parses `grok models`; Kilo Code
parses `kilo models`; and OpenHands reads
model IDs from its saved configuration plus `LLM_MODEL`. An exact OpenHands ID
can also be entered directly. A catalog is informative,
not proof that the current account can successfully invoke every listed model.

A launch-time authentication failure follows the same route: agentview preserves the
task draft and selected model, opens the provider's setup picker, and makes
Enter/`l` run native login. This avoids leaving a Cursor/Copilot error as a
passive footer where Enter would open an unrelated selected row.

The native setup surfaces are `claude auth login`, `codex login`, Pi's
no-session TUI (`/login` inside Pi), `opencode auth login`, `cursor-agent login`,
`copilot login`, Antigravity's first-run `agy` flow, Oh My Pi's no-session TUI,
`grok login`, `kilo auth login`, and `openhands login`. agentview suspends its
alternate screen before these commands and never reads or copies credentials.
The setup/login UI always gets its own private terminal; Left/Right twice at a
cursor boundary or Shift+Left/Right anywhere returns to the dashboard and
leaves that process running. Enter/Right shows its latest screen. The
frontend is stopped only when the dashboard itself exits. `/setup
HARNESS` uses the same terminal for an installation check, confirmed official
installer, and native login. It never attaches setup to the last agent session.

Select `Terminal` (or `/harness terminal`) to create a plain interactive shell.
The task text becomes the terminal's display name, not a command. A boundary
double-arrow or Shift+Left/Right returns to agentview, Enter/Right resumes, the first
Ctrl+X stops it, and the second Ctrl+X
deletes its completed row. These terminal frontends are process-local and are
stopped when the dashboard itself exits. `/shell` and `/model` both open a
searchable shell picker. It shows the configured default and installed Bash,
Zsh, Fish, Nushell, Xonsh, and Elvish executables. A missing supported shell is
shown as an explicit `install` row; selecting that row suspends agentview and opens
the host package manager (`brew`, `apt-get`, `dnf`, `pacman`, or `zypper`) in
the native terminal. agentview never evaluates the session name or shell choice as a
command string.

New Copilot dashboard tasks reserve an exact UUID and open Copilot's native
interactive UI in the foreground. Their exact IDs and latest bounded message
summary are retained privately, so a later dashboard restores the row and
refreshes its text and age from ACP history. Copilot also retains one
process-local ACP control connection for rows explicitly created or loaded
through its managed control path. A later dashboard does not silently inherit
that connection's control authority.

`doctor` checks executable availability and explicitly named Docker targets. It
does not launch, stop, or modify a provider session or container. A missing
optional host provider is a warning; failure to verify an explicitly requested
container is an error and produces a nonzero exit status.

## Completed history and bulk archive

The default dashboard and JSON snapshot include completed exact agentview-managed
sessions. Ownership and lifecycle visibility remain separate: `/completed hide`
or `--hide-completed` provides an active-only managed view, while `/completed
show` restores completed sessions. These controls do not read unrelated
provider history. Add
`--include-external` when provider-wide history is actually wanted. For
example, `agentview --include-external --history-limit 500` opts into
a bounded completed-history review.

Completed filtering is applied by adapters and again at the central discovery
boundary. Claude is queried without `--all` when completed is hidden, and
OpenCode's global persisted-history query is never started without both
`--include-external` and completed visibility. Rows returned in violation of a
provider's completed/cwd/interactive contract are removed before a partial
snapshot reaches the UI. The header shows `completed hidden` rather than a
misleading zero. `/completed show`, `/completed hide`, and `/completed` update
the running dashboard; `--hide-completed` selects the active-only initial state
and `--all` remains accepted for compatibility. External history is
limited to 100 records per provider by default, with a warning when more exist.
The Show-more row pages only the already discovered window.

### Needs input versus done

Most providers report an idle session as completed or as waiting at its prompt
without saying whether the agent asked something. When an OpenRouter key is set
in `OPENROUTER_API_KEY` or `~/.config/agentview/openrouter-api-key`, the
dashboard sends the transcript tail of each session that went idle in the last
24 hours to a small model once per turn. A turn that waits on the user moves to
Needs input; a finished one stays Completed. Permission and question prompts are
already known and are never sent. `AGENTVIEW_TURN_CLASSIFIER_MODEL` overrides
the default `google/gemini-2.5-flash-lite`, and `AGENTVIEW_TURN_CLASSIFIER=off`
disables it. The terminal tab title shows the same counts, for example
`2 need input · 1 working · 3 done · agentview`.

### Timing log

`AGENTVIEW_PERF_LOG=/tmp/av.log agentview` appends one line per event on the
open and refresh paths: each provider's discovery time, how long an open waited
for the hidden OpenCode TUI and why it went ahead, the preview's switch and
draw, the first keystrokes' echo latency, and any wait on or long hold of the
OpenCode state lock with its call site. Nothing is written without it.

OpenCode history queries read OpenCode's database file directly, read-only,
instead of starting `opencode db` for each one. `opencode-db` lines show each
query's time, or `cli` with the reason when a query went through OpenCode's
CLI instead.

### Local hide, provider delete, and provider archive

Ctrl+X follows the selected row's current lifecycle. On an active row with
exact Interrupt authority, the first press stops that exact session. Discovery
refreshes immediately; when the same row becomes idle, the next press deletes
it if the provider grants exact Delete authority. Providers without a safe
delete surface instead remove the idle row locally and reversibly, retaining
provider history. The same rule applies from Peek.

Delete and local hide always take two presses on the same row. The first press
replaces the row's summary with `ctrl+x again to delete` (or `to hide`) and
changes nothing; the second press acts. Any other key, or a different
selection, cancels. A stop counts as the first press, so stopping and then
removing a session is still Ctrl+X twice. An active row without Interrupt
authority is hidden this way too; its live process continues.

Completed-group deletion uses the same two presses on the group heading. It
deletes only when every row grants Delete; otherwise it hides only the
undeletable rows locally. Bulk stop for an active group remains unavailable.

The local hidden-ID registry can also be managed without opening the TUI:

```console
# Obtain the stable normalized ID from Peek or JSON output. Add
# --include-external when the target is not agentview-managed.
agentview --json --include-external --all

agentview sessions hide 'pi:host:EXACT_ID'
agentview sessions hidden
agentview sessions unhide 'pi:host:EXACT_ID'

# Each maintenance command also supports machine-readable output.
agentview --json sessions hidden
```

`sessions hide` is idempotent and accepts an exact normalized ID even if its
provider row is not currently discoverable. `sessions unhide` only removes the
local suppression; the row returns on the next discovery only if its provider
still reports it. Neither command opens, stops, deletes, archives, or edits a
provider session.

Private display names use a separate registry and never call a provider rename
surface:

```console
agentview sessions rename 'pi:host:EXACT_ID' 'release captain'
agentview sessions aliases
agentview --json sessions aliases
agentview sessions reset-name 'pi:host:EXACT_ID'
```

`rename` and `reset-name` are idempotent. If a native harness renames the same
conversation, the local agentview name continues to win until reset. After reset, the
next refresh displays the provider's latest title. In the TUI, `ctrl+r` edits
the same local name and an empty submission resets it.

Provider-native bulk archive is currently available for exact agentview-owned,
completed host Codex threads. The first command is always a read-only preview:

```console
agentview sessions archive
agentview sessions archive --cwd /absolute/project --older-than-days 30 --limit 100
agentview --json sessions archive --older-than-days 30
```

The report distinguishes all completed threads seen, those matching the
directory/age scope, those with exact Archive authority, and the bounded batch
selected. It lists skipped matched threads that are visible but unowned. To
apply the reviewed batch, repeat the exact command with `--yes`:

```console
agentview sessions archive --cwd /absolute/project --older-than-days 30 --limit 100 --yes
```

The default batch limit is 100 and the maximum is 1,100. Every archive is independently revalidated against
the live owning App Server; one refusal is reported without granting authority
to or silently skipping the remaining selected records. Fixture mode, disabled
host Codex, missing Codex, active threads, external threads, Docker threads,
and providers without a documented archive operation are refused or reported
as ineligible. agentview does not call deletion an archive.

## Restarting the OpenCode server

agentview runs one OpenCode server that its attached OpenCode sessions use.
Restarting it, for example to pick up a new OpenCode build, normally cuts off
every turn in flight. This command restarts it and resumes those turns:

```console
agentview opencode restart
agentview --json opencode restart
```

Before stopping the server it records every top-level session with a running
turn, in any directory used in the last three days. The new server listens on
the same port with the same credentials, so open OpenCode windows reconnect by
themselves. Each recorded session then gets a `continue` prompt using the agent
and model of its last message. Subagent sessions are left alone because
resuming their parent restarts them. Sessions that were waiting on a question
or permission prompt are listed rather than resumed, since that prompt does not
survive the restart and only you can answer it. If the restart fails partway,
running the command again resumes the sessions it recorded.

Rebooting needs no command. When the dashboard next starts the server and the
machine has booted since the previous server started, agentview reads the
OpenCode database for top-level sessions whose last reply began on that server
and never completed, and resumes them the same way. A finished or interrupted
reply always records its completion, and a server killed by shutdown never
does. agentview only does this after a reboot: if the server merely crashed, a
standalone OpenCode window could still be running one of those turns.

To stop the server without restarting it:

```console
agentview opencode stop
```

It records the running turns as `restart` does, and the next server start
resumes them.

Open windows reconnect but keep running the OpenCode build and TUI plugins they
started with. The dashboard keeps one OpenCode window per folder and switches
it between that folder's sessions with `/tui/select-session`, addressed to that
window's `--client` ID. The first time you open a session after a restart, it
replaces that window with a fresh one. A window running a session you opened
before this feature existed keeps running until you close it. Shared windows
need a server whose `/tui/select-session` accepts `client`; agentview reads the
server's `/doc` to check, because older servers accept the field and then
switch every attached window.

## Managed Docker lifecycle

Managed Docker is distinct from `--docker-container`. The latter enrolls one
already-running container for observation only. Lifecycle authority exists
only when agentview created the container and its exact immutable ID,
random instance label, and protected external owner record still agree.

Create the mount sources first. Do not make the state home a parent or child of
the workspace:

```console
install -d /absolute/project /absolute/dedicated-agent-home
agentview docker create \
  --name agentview-agent \
  --image registry.example/agents/runtime@sha256:FULL_64_HEX_DIGEST \
  --workspace /absolute/project \
  --state-home /absolute/dedicated-agent-home \
  --network bridge
```

Creation validates and canonicalizes both directories, requires a digest-pinned
image, creates a stopped container, re-inspects its labels and full ID, and only
then writes the owner record. It does not copy credentials. The workspace is
mounted at `/workspace`; the dedicated state home becomes `/home/agent` and
the container's `HOME`. Both mounts are persistent bind mounts.

The default container identity is the invoking effective UID/GID. Use
`--uid N --gid N` together only when the image and host-directory permissions
require another non-root identity. The default network is Docker's `bridge`.
`--network none` and an existing named Docker network are accepted; host and
`container:...` network sharing are deliberately refused. Creation also uses
an init, drops all capabilities, enables `no-new-privileges`, sets a PID limit,
and runs `sleep infinity`. It does not make the image root filesystem read-only.

Every later command accepts the registered name or immutable ID and revalidates
the immutable identity before acting:

```console
agentview docker list
agentview docker status agentview-agent
agentview docker start agentview-agent
agentview docker stop agentview-agent --yes
agentview docker status agentview-agent --json
agentview docker remove agentview-agent --yes
```

`start` refuses an already-running container. `stop` refuses a stopped
container and gives Docker ten seconds before its ordinary stop behavior.
`remove` refuses a running container and does not use force or volume-removal
flags. It retains both host directories and forgets the owner record only after
Docker confirms removal. `stop` and `remove` require the literal `--yes`; there
is no interactive CLI prompt.

The default owner registry is:

```text
$XDG_STATE_HOME/agentview/managed-docker/owners.json
```

or, when `XDG_STATE_HOME` is unset:

```text
~/.local/state/agentview/managed-docker/owners.json
```

Use the same `--managed-docker-registry /absolute/path/owners.json` on every
managed-Docker invocation when overriding this location. The registry's parent
must be a real current-user-owned `0700` directory and the existing file must
be a real current-user-owned `0600` regular file. Do not hand-edit it to adopt
an existing container; labels or a record alone are intentionally insufficient.

All Docker lifecycle/status commands support `--json`. JSON status contains
the immutable container ID, random instance ID, optional name/image, normalized
state, and a redacted detail string. It excludes labels, environment values,
and mount details.

## TUI keys and mode behavior

Every session row spells out its provider name, including Oh My Pi, Grok, Kilo
Code, and OpenHands. Provider identity takes priority over task summary width on narrow
terminals. Peek expands the selected row with the full host or container runtime
label.

| Context | Key | Result |
| --- | --- | --- |
| Session list | `↑` / `↓` | Move cyclically through group headings and rows. |
| Show more row | `enter` | Reveal the next terminal-sized page (at most 25) in that group. |
| Group heading | `enter` | Collapse or expand the group. |
| Session row | `enter` or `→` | Suspend the dashboard and open the provider's full native interface. The physical screen is cleared before the provider draws. |
| Provider-native interface | `←` / `→` twice at a cursor boundary | The first arrow is forwarded. If the cursor does not move, a 1.6-second bottom-line hint appears; repeat the same arrow to retain the frontend and return to agentview. In Claude Code, `←` at an empty prompt (where Claude shows "← for agents") returns at once and never opens Claude's agents view; OpenCode also returns on one `←`. |
| Provider-native interface | `shift+←` / `shift+→` | Return immediately from anywhere. Plain arrows otherwise remain available for line editing. `enter` or `→` on the same dashboard row reattaches and restores its terminal screen. |
| Inline Peek | `←` | Return to the session list without opening the native provider interface. |
| Session row | `space` | Open the inline Peek panel and inspect transcript/request details when capability is advertised. |
| Inspect peek | type, `enter` | Send an owned provider reply/steer or the current structured answer. |
| Inspect peek | `y` / `n` | Allow once / deny only when the exact capability is advertised. |
| Inspect peek | `enter` with no text | Open the provider-native interface when that managed/live boundary allows a second client. |
| Session list | `ctrl+s` | Toggle status and working-directory grouping. |
| Session list | `ctrl+f` | Edit the case-insensitive name/summary/path/provider filter. |
| Session list | `ctrl+l` | Request an immediate provider refresh. |
| Session list or new-task composer | `ctrl+g` | Search the dashboard's sessions (marked `open`), sessions hidden with `ctrl+x`, and older OpenCode sessions the dashboard does not list, by name, harness, folder, ID, or recent OpenCode message text, which shows under each match with the match in bold; `page up` / `page down` move by the rows that fit. Older sessions arrive in the background while the others are already searchable. `enter` opens a dashboard session directly, or unhides the chosen one, or adopts an older session so it stays listed without `--include-external`, then selects and opens its row once discovery lists it, unless you have moved the cursor or opened another panel meanwhile; a row the current filter excludes is reported rather than the filter being cleared. `esc` or `ctrl+g` closes the picker, returning to the draft when it was opened from the composer. Zellij binds `ctrl+g` to its lock mode by default, so under Zellij use `/hidden` instead or unbind that key. |
| Session list | `ctrl+t` or `ctrl+p` | Pause or unpause the selected session, for one you are waiting on. Paused rows move into a Paused group at the top, which starts collapsed; `enter` on its header expands or collapses it. Pausing moves the cursor to the next row and leaves the group as it was. `ctrl+t` matches Claude Code's agent-view pin key; `cmd+p` also works when the terminal delivers it as the super modifier. Both also work in Peek. A pause is kept while the session is hidden with `ctrl+x` and applies again once it is restored. |
| Session list | `tab`, `/`, or printable text | Compose a new host task. `/` begins a dashboard command rather than a filter. |
| New-task composer | `shift+enter`, `ctrl+enter`, `alt+enter`, `ctrl+j`, or `\` then `enter` | Add a line without submitting; pasted text keeps its lines. A `\` right before the cursor is replaced by the line break, as in Claude Code, for terminals that report `shift+enter` as plain `enter`. `enter` submits the whole draft. |
| New-task composer | `←` / `→`, `↑` / `↓`, `home` / `end`, `alt+←` / `alt+→`, `ctrl+a` / `ctrl+e` | Move the cursor through the draft by character, line, or word; typing, `backspace`, and `delete` edit at the cursor. |
| New-task composer | `shift` with any of those, e.g. `shift+←` / `shift+→`, `option+shift+←` / `option+shift+→`, `cmd+shift+←` / `cmd+shift+→` | Select by character, word, or to the line edge, growing the selection on each press, as in OpenCode. Typing, pasting, `backspace`, or `delete` replaces the selection; a plain arrow collapses it. |
| New-task composer | `tab` | Open the visible harness picker. |
| New-task composer | `shift+tab` | Open the selected harness's model picker—or Terminal shell picker—without changing the task draft. From the session list it opens an empty composer first. |
| Harness picker | `↑` / `↓`, `←` / `→`, or `tab` / `shift+tab` | Preview configured launch-capable harnesses with wraparound. |
| Harness picker | `enter` or `1`–`9` | Select the highlighted or numbered harness and return to the unchanged draft; changing harness resets the model to its default. |
| Harness picker | `esc` | Return to the unchanged draft without switching harnesses. |
| New-task composer | `/harness` / `/harness NAME` | Open the picker or directly select any configured harness when its launch controller is available; `/provider` is an alias. |
| New-task composer | `/model` | Asynchronously load the selected harness's account/catalog model list and open a searchable picker. |
| Terminal composer | `/shell` / `/shell NAME` | Open the shell picker or select an installed shell; `/model` remains an exact alias while Terminal is selected. Missing supported shells appear as explicit native package-manager install actions. |
| Model picker | type, `backspace`, `↑` / `↓`, `tab` / `shift+tab`, `page up` / `page down` | Filter and navigate catalog results; provider discovery stays off the input thread. |
| Model picker | `enter` / `esc` | Select the highlighted model, or return with the previous model and draft unchanged. |
| New-task composer | `/model NAME` / `/model default` | Select an exact custom model identifier or reset to the provider default. The provider revalidates it at launch. |
| New-task composer | `/completed [show\|hide]` | Toggle completed discovery, or set it explicitly. `show` refreshes providers; `hide` immediately removes completed rows and keeps later refreshes active-only. |
| New-task composer | `/filter TEXT` / `/help` | Apply a session filter or list dashboard slash commands without contacting a provider. |
| New-task composer | `/hidden` | Open the hidden-session restore picker, the same as `ctrl+g`. |
| New-task composer | `/setup [HARNESS]` | Open the selected or named harness's isolated install/login terminal. |
| Writable composer | `ctrl+j` | Insert a newline rather than submit. |
| Any view | paste | Pasted text is inserted as text, never submitted. Line breaks in the clipboard become newlines in one draft; a paste on the dashboard opens the new-task composer. agentview enables bracketed paste. On terminals without it, keys arriving within milliseconds of each other count as one paste: an Enter with more input right behind it is a pasted line break, pasted characters never act as shortcuts, and only an Enter after a known dashboard command still runs it. Control characters other than tab and newline are dropped; the model picker, filter, rename, and migration-name fields turn line breaks and tabs into spaces. In the new-task composer and a reply, a bracketed paste of three or more lines or over 150 characters shows as `[Pasted ~N lines]`, as in OpenCode, and the full text is sent on submit. Backspace removes the token and its paste; a second paste that would read the same gets a `#2` suffix. |
| New-task composer or reply | `ctrl+v` | Save the clipboard image under `~/.local/state/agentview/images/` (or `$XDG_STATE_HOME/agentview/images/`) and insert `[Image #N]`. A copied image file is used where it is. On submit, each token becomes the image's absolute path, which the harness reads as an image file. `cmd+v` with only an image on the clipboard does the same. On the dashboard it opens the new-task composer first. Linux needs `wl-paste` or `xclip`; images do not reach harnesses running inside a container. |
| Writable composer | `backspace` | Remove the last character, or a whole `[Image #N]` token. |
| Writable composer/model filter | `option+backspace` or `ctrl+w` | Remove the previous word. |
| Writable composer/model filter | `cmd+backspace` or `ctrl+u` | Remove to the beginning of the current line. |
| Session row | `ctrl+r` | Open the accented `rename session` composer. The `name ❯` mode label is separate from the editable display name; empty submission clears it and follows the latest provider title again. |
| Host coding-harness session row | `ctrl+m` (`cmd+m` on macOS) | Open the folder picker and move the selected session there with the same harness and name. session-migrate creates the copy in the new folder; agentview then deletes the original, or hides it when the harness cannot delete it. Working sessions must stop first. Files in either folder are not touched. |
| New-task composer | `/migrate` | Choose a different supported harness, edit the prefilled local name, and migrate the exact selected session through `session-migrate`. The default is `CURRENT NAME (TARGET HARNESS)`. Escape returns from the name editor to the target picker before closing the workflow. |
| Idle owned Codex row | `ctrl+a`, then `enter` | Confirm archive. |
| Session row or Peek | `ctrl+x`, then `ctrl+x` | Stop an exact active owned session; after refresh reports it idle, press again to delete it or remove it reversibly from agentview's view. Idle rows and active rows without stop authority show `ctrl+x again to delete` (or `to hide`) on the first press and act on the second; any other key cancels. |
| Completed group | `ctrl+x`, then `ctrl+x` | Delete only when every member grants Delete; otherwise hide the undeletable rows locally. |
| Any ordinary view | `?` | Open contextual help; `?`, `enter`, or `esc` closes it. |
| Any overlay/composer | `esc` | Cancel that mode and discard its unsubmitted input. |
| Session list | `esc` | Quit immediately and restore the terminal. |
| Empty session list | `q` | Quit; when a row is selected, printable `q` starts a task like other text. |

Controls are capability-driven. A key listed here can safely do nothing or
show an authority notice for an observe-only, mismatched, expired, or otherwise
unsupported target. Approval `y` is never offered for a file change lacking a
correlated diff, expanded permissions, or unknown request form. See the
[control model](control-model.md) for the exact boundary.

### Session migration

The `/migrate` workflow supports all 18 coding harnesses in the agentview picker as
sources and destinations. It is intentionally limited to host sessions because
session-migrate reads each harness's local native state. Terminal jobs and
Docker-observed sessions are not offered as sources, and the source harness is
removed from the destination menu.

agentview runs `session-migrate transfer` off the input thread, passes the exact
provider session ID and working directory without a shell, and validates the
returned JSON and target before indexing the result. A failed transfer leaves
no agentview migration record; a migration never edits the source session, and a
move removes it only after the copy has been recorded. Successful
imports are stored in the private agentview state directory so they remain visible
without enabling all external provider history. The name selected in agentview is a
local display alias; it does not claim to rewrite the destination harness's
native title. OpenCode and Kilo virtual exports reuse the executable selected
by `--opencode-bin` or `--kilo-bin`.

Install session-migrate with:

```sh
curl -LsSf https://session-migrate.github.io/install.sh | sh
```

Use `--session-migrate-bin PATH` to select another executable.

Paging affects only the interactive list. Counts, filtering, JSON output, and
group-level safety checks always use the complete discovered session set. Each
status or directory group initially shows a terminal-sized page of at most 25
session rows, followed by a selectable `Show N more · M hidden` row when more
match. Each Enter reveals at most one more page and moves selection to the first
newly visible row. The
revealed count is remembered across ordinary provider refreshes and reset when
switching views or applying a filter, keeping a newly narrowed queue bounded.

After a successful managed launch, the dashboard refreshes immediately and
uses the exact provider/session hint to select the new row. If the provider
persists its record after the launch response, agentview retries discovery
every 250 ms for up to five seconds. The UI remains interactive during those
retries and reports a manual `ctrl+l` recovery only if the exact row still has
not appeared.

Only host Claude background rows recorded in agentview's ownership registry receive
Interrupt. Immediately before `claude stop`, agentview reruns `claude
agents --json` and requires the exact full UUID to remain a host background
session in an active state. Interactive, completed, Docker, external, missing,
or changed rows are refused. Ctrl+X dispatches the stop directly for the exact
owned row; its next use can remove the row only after refresh reports it idle.

Managed Cursor rows on Linux expose Inspect and either Interrupt while the
verified owned process is active or Reply after it becomes idle. Managed
Cursor native open is likewise refused until the active process exits. Managed
Copilot rows expose Inspect, Reply while idle, Cancel while a prompt is active,
and only the exact `allow_once`/`reject_once` choices offered by a pending ACP
permission request. Persisted Copilot rows from `session/list` do not inherit
those controls.

Managed OpenCode rows on Linux expose Inspect and Reply; while the owned server
reports active work they also expose Interrupt. Native open attaches to the
same exact authenticated loopback server/session rather than starting a second
server. They do not yet expose provider permission or structured-input
requests. External OpenCode history remains inspect/native-open only.

Outside Linux, a new OpenCode task opens OpenCode's interface and pastes the
task into its editor once it is ready. The session it creates is recorded as agentview's own and listed without
`--include-external`; it has Inspect and native open but no inline control.

OpenCode rows that a live `opencode` process runs (a TUI, or a headless
`opencode run`) are shown as working or needs input rather than completed. For a
session held in the background by this dashboard, OpenCode's screen decides:
the footer's interrupt hint means working, a permission or question panel or an
idle prompt means needs input. Otherwise the newest persisted message decides,
counting only turns written after that process started, so a turn cut off by a
killed process does not read as working. A background subagent (OpenCode's
`task` tool with `background: true`, its closest equivalent of a monitor) keeps
its parent working until the subagent's turn ends. An idle prompt on the
dashboard's own screen yields to that, and to a turn another process is running
for the same session. A plugin can keep a session working after its turn
ends, for example while a monitor waits on CI to start the next turn, by
writing `{"pid": PID, "reason": "…", "expires_ms": MS}` to
`~/.local/state/agentview/holds/opencode/SESSION_ID/NAME.json`
(`$XDG_STATE_HOME/agentview/holds/…` when set) and deleting it when the work
ends. The row shows `background: REASON` while that process lives and the
expiry has not passed, unless a permission or question prompt needs the user.
A permission prompt in another
terminal is not recorded by OpenCode and reads as working there. Sessions no
live process runs are completed history. A process started with `--session ID`
holds exactly that session. Any other holds one session in its working
directory that changed since it started: sessions with an unfinished turn
written in the last ten minutes are matched first, each to the process that
created it if that one is free, otherwise to the newest free process.
Live state needs `ps` and, on macOS, `lsof`; on Windows every row is history.

New Pi dashboard tasks open its full native interface first and save into agentview's
managed session directory. Managed Pi RPC rows expose Ctrl+X stop while their exact RPC process is alive,
including an idle process after a completed turn. Stop closes the selected
supervisor-owned stdin without waiting on a model response. After refresh
observes exit, the same row exposes exact Delete; the next Ctrl+X removes its
validated JSONL file. Enter/Right performs that handoff automatically only for
a completed row, then opens Pi's full native interface. Active work and pending
questions must be stopped explicitly first.

## Runtime state paths

Under `$XDG_STATE_HOME/agentview/`, or `~/.local/state/agentview/`
when `XDG_STATE_HOME` is unset, the current implementation stores:

| Path | Purpose |
| --- | --- |
| `ownership.json` | Exact host Claude session prefixes launched here. |
| `codex-supervisor/` | Detached App Server record, socket, locks, log, and owned Codex thread/turn IDs. |
| `pi/` | Detached Linux RPC supervisor record, socket, locks/logs, and agentview-owned Pi session history. |
| `opencode/` | Private authenticated-loopback server record, lock, log, and exact agentview-owned OpenCode session IDs. |
| `opencode-owned.json` | OpenCode sessions launched in OpenCode's own interface outside Linux: exact IDs, workspaces, and names. |
| `cursor/` | Linux ownership registry, process identities, locks, and bounded logs for agentview-owned Cursor runs. |
| `copilot/` | Exact agentview-created Copilot IDs, workspaces, titles, latest bounded summaries, provider timestamps, and registry lock. No credentials or full transcripts. |
| `cursor-owned.json` | Exact Cursor chat IDs this dashboard created outside Linux, so their rows are listed without `--include-external`. |
| `hidden-sessions.json` | Reversible local suppression records; provider history and live processes are not changed. |
| `pinned-sessions.json` | Local pin records (session ID and pin time) for the dashboard's Pinned group. Pins never change the provider session. |
| `managed-docker/owners.json` | Exact external proof for managed-container lifecycle. |

These files contain authority metadata and should not be shared between users.
They do not contain collected Codex structured answers. Copilot's registry does
not preserve ACP connection authority or permission requests. Removal or repair
has safety consequences; follow [troubleshooting and recovery](troubleshooting.md)
instead of deleting state speculatively.
