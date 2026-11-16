# Mobile OpenCode bridge

The opt-in bridge copies visible conversation text to a private MCP server.
ChatGPT can search the copy while the Mac is offline. An explicit send enters a
separate queue. OpenCode receives it when the connector reconnects.

Deploy [voizecode/bridge](https://github.com/moritzWa/voizecode/tree/main/bridge)
and configure owner login before pairing.

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

Credentials go in a dedicated Keychain item through Apple's stable `security`
helper. The local journal has owner-only permissions. Source connections are
read-only. The shared OpenCode supervisor handles admission through its API.
The source database and provider credentials never go to the cloud.

## Privacy

All dates and sessions are copied, including archived history and native forks.
Only user text and assistant text are allowed. Tool output, reasoning, files and
native provider payloads are excluded. Edits, removals and revert boundaries
replace the remote view after sync. Chunked changes stay hidden until committed.

The backend can read copied text. ChatGPT/OpenAI can read retrieved text. HTTPS
and encrypted disks do not hide it from those services. A local revert cannot
recall content already retrieved by ChatGPT.

## Freshness and queued work

Idle sync runs every 15 minutes so Neon can suspend. Mobile requests notify the
connector to check more often for five minutes. Reads report the last cloud sync
and successful source check. Offline reads are snapshots.

Sending requires the current user to ask, a write-enabled OAuth grant, client
approval, an operation ID and the cursor from a recent read. Retries reuse the
same OpenCode message ID. Accepted means admitted, not completed. Native
permission gates remain in force. Retrieved history is never authorization.

Queued messages expire after a day. A context change requires a fresh read and
new confirmed operation. Cancel the old queued operation first. An expired
lease stays uncertain until the connector reconciles its admission ID.

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
