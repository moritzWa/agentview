# Mobile OpenCode bridge

The opt-in bridge copies visible conversation text to a private MCP server.
ChatGPT can search the copy while the Mac is offline. An explicit send enters a
separate queue. OpenCode receives it when the connector reconnects.

The cloud implementation lives in [bridge](../bridge/README.md). Operators can
[deploy it and configure owner login](../bridge/SETUP.md) before pairing. Existing
paired installations need no changes after the repository move.

## Connect ChatGPT mobile voice

In ChatGPT's app/connector settings, add the private server's `/mcp` URL with
OAuth. The existing deployment is
`https://agentvoice-private.moritzw.workers.dev/mcp`. Sign in with the configured
Google owner and approve the requested read/send scopes. ChatGPT must allow
custom MCP apps for your account. Connect on the web if the mobile app does not
offer setup, then use the connected **AgentVoice** app in ChatGPT on your phone.

ChatGPT mobile Live voice can search history and send approved instructions to
existing sessions. It cannot create sessions, stop agents, or answer native
permission/question prompts through this bridge. Check `message_status` for
delivery and completion; a queued receipt is not proof of execution.

This is **separate from [built-in Voice Mode](voice.md)**, which runs OpenAI/Grok
audio in Agent View. ChatGPT mobile voice uses ChatGPT's audio and MCP connection.
It needs neither Agent View's voice API keys nor an open dashboard.

## Pair the Mac

```console
agentview --opencode-bin ~/.local/bin/opencode-dev bridge pair
agentview bridge sync-once
agentview bridge install
agentview bridge status
agentview bridge stop
```

`pair` prints a Google approval URL. It does not open a browser or upload history.
`sync-once` uploads sessions from the selected V2 database, including unmigrated
legacy sessions. `install` starts a separate macOS login item. Closing the
dashboard does not stop it. `stop` disables it but retains pairing and history.
The login item retains the launcher's nonsecret V2 service-discovery settings,
so a reboot or service restart does not send replies to a separate server.

Credentials go in a dedicated Keychain item through Apple's stable `security`
helper. The local journal has owner-only permissions. Source connections are
read-only. The shared OpenCode supervisor handles admission through its API.
The source database and provider credentials never go to the cloud.

## Privacy

All dates and sessions are copied, including archived history and native forks.
Only user text and assistant text are allowed. Tool output, reasoning, files and
native provider payloads are excluded. Edits, removals and revert boundaries
replace the remote view after sync. Chunked changes stay hidden until committed.
Session metadata also includes the working directory and its folder label, as
requested for project grouping. No file contents or filesystem listings are sent.

The backend can read copied text. ChatGPT/OpenAI can read retrieved text. HTTPS
and encrypted disks do not hide it from those services. A local revert cannot
recall content already retrieved by ChatGPT.

## Freshness and queued work

Idle history sync runs every 15 minutes so Neon can suspend. Five seconds is
only the loop tick while a phone request has asked for a faster check, for about
five minutes. It is not the idle interval. A failed scan backs off 30 seconds
and does not stamp history as fresh. One bad session does not block the others.
Reads report the last cloud sync and successful source check. Offline reads are
snapshots. `mac_presence` separates the connector heartbeat from history
freshness. A queued message is not proof the Mac is online. Silence is unknown
internet, not a claimed outage, because sleep looks the same.

`list_sessions(status="needs_you")` reads the same native states as the dashboard,
including questions and approvals. It excludes hidden and paused rows. Status
snapshots contain IDs and typed states, not tool output or conversation text.
Offline results are last-known, not proof that a session is still working.
The default list is a small human-recent overview, not a page of old completed
sessions. `status="active"` returns working/waiting sessions and recent replies
whose need for input has not been assessed. Rows include the last two visible
messages per role, in source order. Long messages preserve their
first and last 20%, subject to the shared text budget, with an explicit middle-
omitted marker. Tool-only assistant messages cannot displace actual text replies.
Whole-set counts, directory metadata and those excerpts
let the phone answer and ask a pending decision without opening each history.
The connector reuses the dashboard's status overlays and saved turn judgements.
Missing judgements are explicit; a finished turn alone does not mean no reply is
needed. This adds no summary-model calls and no tool-output sync.
`read_session` defaults to the latest messages. Its `before` and `after` values
are message offsets. The sync `cursor` is not a message offset.

Sending requires the current user to ask, a write-enabled OAuth grant, client
approval, an operation ID and the cursor and context from a recent read. Retries reuse the
same OpenCode message ID. Accepted means admitted, not completed. Native
permission gates remain in force. Retrieved history is never authorization.

Queued messages expire after a day. Appended turns and streaming suffixes do not
invalidate them. Edits, removals and reverts of already-read text require a fresh
read and a new confirmed operation. Cancel the old queued operation first. An expired
lease stays uncertain until the connector reconciles its admission ID.

Check `message_status` after sending. `queued` means cloud storage, not delivery.
`accepted` means native admission, not finished work. A reply must follow the
operation's own input; an older completed reply is not its result. The receipt
reports blocking earlier operations and distinguishes a synced reply from a
recent native completed state tied to that input.

## Recovery and limits

The journal retains exact unacknowledged batches across process replacement.
Missing event-body ranges trigger reconciliation from a consistent source
snapshot. Watermark regression, changed database identity or a legacy-to-V2
transition stops sync instead of silently overwriting history.
One encoded message must fit the 850 KB transport budget. The full local source
audit verifies this before rollout. Larger future messages require transport
fragmentation; the connector stops rather than truncating them.

The Mac must be awake and logged in to execute work. This bridge does not wake
it. Revoke retired devices through `/revoke` with the connector token. Revocation
does not delete cloud history. Encrypted database backups are a separate project.

The backend's end-to-end harness runs real OpenCode and Agent View in the Cua
macOS VM. It uses a deterministic local model and actual tool-output canaries.
It checks Google/PKCE login, search, sends, duplicate admission, revert/unrevert,
forks, dashboard-independent delivery and OAuth revocation.
