# Private mobile bridge

This Worker is separate from the public synthetic probe. It uses Google owner
login, MCP OAuth with PKCE, a private Neon database and a revocable Mac connector.
The owner is pinned to `wallawitsch@gmail.com`.

## Deployment

Run from `bridge`. Read [deployment setup](../SETUP.md) first. Existing service
relocation requires neither of the following operator commands.

```console
npm ci
NEON_PROJECT_ID=<dedicated-project> node private/provision.mjs
npx wrangler deploy --config wrangler.private.jsonc
```

Provisioning applies both SQL schemas. It creates a runtime role without schema
ownership and saves its credential in 1Password. Worker secrets receive the
runtime database URL and the dedicated Google web client from 1Password.
The Google callback is `<issuer>/auth/google/callback`.

The deployed test setup uses the **AgentVoice Free** Neon organization
(`org-purple-unit-21830200`), project `frosty-pond-48075355`, fixed at 0.25 CU.
It is not in the paid Moritz organization. Cloudflare Worker:
`https://agentvoice-private.moritzw.workers.dev`.

## MCP

Add `<issuer>/mcp` to ChatGPT with OAuth. Discovery and dynamic registration are
supported. Registered redirects are limited to ChatGPT's documented callback
paths and this server's integration-test callback. Google login and browser-bound
consent precede every new grant. Scopes are `history:read` and `messages:send`.
Refresh tokens rotate. Replay revokes the token family.

Tools: `list_sessions`, `session_brief`, `search_history`, `read_session`, `read_message`,
`send_session_message`, `message_status`, `cancel_message`, `list_artifacts`,
and `preview_artifact`.

### Delivery contract

Every `send_session_message`, `message_status` and `cancel_message` result starts with
`delivery_state`, `say_to_owner`, `claim_ceiling` and `owner_action`. The model says
`say_to_owner` and claims no more than `claim_ceiling` (`none`, `queued`, `accepted`,
`completed`). States: `not_sent`, `queued_mac_offline`, `queued_waiting_for_mac`,
`accepted_by_session`, `completed_confirmed`, `needs_confirmation`,
`rejected_stale_context`, `cancelled`, `failed`, `expired`, `uncertain`. Failed sends
return a JSON body (`error` keeps the old code, plus `sent`, `say_to_owner`, `next_step`).
`sent: null` means unknown, so check `message_status` before claiming anything.
`message_status` for an operation that was never sent returns `not_sent`, which is how a
skipped send is caught. The logic is pure and lives in `delivery.mjs`. No safety check
changed. Read-tool errors stay bare codes.

### Artifact previews

`preview_artifact` always returns an original MCP file attachment and asks
ChatGPT to present an **Open preview** link. There are no alternative widget,
embedded-view or legacy-renderer tools to choose from. The tool does not claim
that a file is visible before the user opens it. UTF-8 HTML uses the ordinary
embedded text resource that worked in the native iPhone file viewer; binary
files use base64 resource contents. It never returns an MCP Apps template.

History sync does **not** discover files or upload paths mentioned in chats.
Publish an exact file only after the owner approves its transfer to the private
bridge and ChatGPT:

```console
agentview bridge publish-artifact --session-id ses_example --file ./output/index.html --confirmed
```

This uploads that one file unchanged, associated with the exact session. It
does not collect sibling assets, resolve imports or publish a public website.
This file transport does not yet implement the planned multi-file artifact
registry. Do not call a relative-asset HTML page fully previewable merely
because its entrypoint was uploaded. Folder registration and asset delivery
remain separate work; authors are not required to flatten their source files.
An explicit ZIP can carry a bundle for download, but is not a live HTML preview.

Only files up to 512,000 bytes are accepted. Text must be UTF-8. HTML, text,
PNG, JPEG, PDF and ZIP are accepted attachment types; only HTML has been
visually verified on iPhone so far. There are at most 128 immutable published
files per paired source. Repeating an identical command returns the same ID;
changed bytes create a different revision. `list_artifacts(session_id)` returns
metadata, and `preview_artifact(artifact_id)` returns bytes only after a live
owner/source grant check. Deleted sessions hide their attachments. Revoking
the bridge grant stops new retrievals, not copies already delivered to ChatGPT.

Deployment needs the additive `av_artifacts` table from `private/schema.sql`
and runtime SELECT/INSERT permission on that table. Do not re-provision or
replace existing credentials. Apply this migration only with operator approval,
then deploy the existing Worker and refresh its tools in ChatGPT. Nothing
uploads automatically after installing this change.

The instruction queue is separate from mirrored messages. Stable operation IDs
deduplicate cloud writes. Stable OpenCode message IDs reconcile local admission
after a lost response. Rewritten context requires a fresh read and confirmation;
appended turns and streaming suffixes do not invalidate a queued instruction.
Native permissions are not bypassed. Queued does not mean executed.

### Voice behavior: overview, deep dive, send

Behavior is steered by three layers, in order of reliability. Server-side logic
and result shape decide what the model sees; tool descriptions and server
instructions only advise how to use it. OpenAI documents that ChatGPT uses MCP
server `instructions` and recommends keeping the first 512 characters
self-contained, so the mode rules sit at the start. Whether Live voice follows
them has not been observed here; do not rely on text alone.

- **Overview.** `list_sessions` returns a `digest` first: a headline, at most 3
  `top` items (session, concrete decision, urgency, whether it is confirmed),
  names-only `more`, and `data_age`. Native `needs_you` rows are confirmed. A
  recently finished session whose last assistant message looks like a question
  or blocker (a `?` or a phrase such as "please label") appears as a less certain
  `possible_decision`, or `unconfirmed_ask` if the desktop never classified it.
  The native `counts.needs_you` is unchanged; the digest total can be larger.
- **Why "done" can still need the owner.** The classifier labels only the last
  ~2,000 characters once per turn with a small model, only for sessions under
  24 hours old, and a held (background-job) session reads as working unless its
  message contains `?`. Misses, failed requests and missing keys all leave a turn
  as done. The digest check is a backstop, not a replacement for the classifier.
- **Deep dive.** `session_brief(session_id | query)` returns the original task,
  the latest eight text messages with the final reply up to 8,000 characters,
  decision candidates, `data_age`, and `send_params`. A spoken name such as
  "diagram bench" matches "diagram-bench". Several matches return candidates
  without conversation text. It is read-only.
- **Send.** Only `send_session_message` writes, after the owner has decided, with
  `session_brief.send_params`. ChatGPT's on-screen approval still applies.
- **Staleness.** Digest and brief say how old native status and history are, and
  warn when the Mac has not been seen recently.

- `read_session` defaults to recent messages. Forward and backward pages preserve
  source order under the response budget. No newer messages means an empty page,
  never a substituted older reply. Read truncated messages with `read_message`.
- `search_history` returns a bounded matching passage and its Unicode character
  offset for `read_message`. Search returns at most 30 matches; it does not yet
  have a continuation cursor.
- Pass `list_sessions.next_after` verbatim. A changed list rejects its old cursor
  rather than silently skipping sessions. Start again without `after`.
- `list_sessions` now gives a Cmd+K-style recent overview by default. For active
  or needing-attention questions, `status="active"` selects working/waiting rows
  and a small recent set whose final replies still need assessment. Whole-set
  counts remove the need to page through completed rows looking for a blocker.
  `view="all"` retains exhaustive inventory access.
- Each listed row includes its exact working `directory` and a `project_name`
  folder label, so ChatGPT can group sessions without guessing their project from
  titles. The folder label is not a canonical Git repository identifier.
- `recent_messages` contains the last two nonempty visible messages per role
  (two user messages and two assistant text replies), in original source order.
  Tool calls, results and reasoning are not conversation context. Latest-message
  aliases remain for older clients, with full-message IDs for deeper reads.
- Short messages remain complete. Long messages preserve their first 20% and
  last 20%, capped by the shared 36K-character overview budget including aliases.
  An explicit middle-omitted marker and character counts describe what was cut.
  This preserves opening TLDRs and closing questions without copying huge replies.
  No extra summary-model call or copy of tool output is made. `done` means the
  latest turn ended; it is not proof that the task is finished or needs no reply.
- The connector applies the desktop control/status overlay and reuses the exact
  saved desktop turn classifications without additional model requests. If no
  classification exists for that turn, its reply is marked for review rather
  than presented as proof that nothing needs attention. Context freshness and
  runtime freshness are reported separately. New sessions without mirrored
  history still appear in active counts; their context is explicitly unavailable.
- All authenticated MCP requests already renew mobile activity at the router.
  Status checks additionally wait for history and native-state refresh, then
  report freshness. A missing or stale native status cannot confirm completion.
- Revocation is rechecked after tool waits and inside queue mutations. Expired,
  never-admitted confirmation requests no longer block later operations forever.

## Free-tier behavior

Idle connector polls use a SQLite Durable Object, not Postgres. Polls wait up to
20 seconds, about 4,320 requests per day for one Mac. The daemon checks source
history every 15 minutes while idle. Five seconds is the loop tick during the
five-minute window after a phone request, not the idle interval. A failed scan
backs off 30 seconds and does not mark history fresh. Mobile use requests five
minutes of faster checks. Neon can suspend between idle syncs. `mac_presence`
reports the connector heartbeat separately from history freshness. Accepting a
queued message does not mean the Mac is online.

Reads wake the connector at once, but the Worker waits at most 8 seconds. If the
Mac has not finished a pass by then, the response carries `history_fresh: false`,
`history_note` and `retry_after_ms`, and the caller should repeat the call.
Each session fails alone: a bad session is retried with backoff (5, 15, 30, 60,
120, then 300 seconds) while the rest keep syncing. A batch the cloud refused
inside its transaction (HTTP 400, 409, 413, 422) is dropped. A batch with an
unknown outcome (HTTP 5xx, unreadable receipt) is held and retried byte for
byte, because the cloud may have committed it and a rebuilt batch would fail
`cursor_gap`. A restored journal, or a baseline the cloud already has, is healed by a
`replace` upload: the new copy is written beside the old one, and reads keep
returning the old copy until the last chunk is confirmed. Then the pointers swap.
`agentview bridge verify` compares each local projection with the cloud copy and
prints mismatches only, never message text. Each check-in reports `behind` and
`behind_since`, so `mac_presence` can say how many sessions have not reached the
cloud and for how long. `agentview bridge status` shows `stuck_sessions`,
`held_batches_for` and the last pass duration.

At 0.25 CU, 100 CU-hours covers 400 active hours per month. Five minutes awake
after each 15-minute idle sync is about 240 active hours per 30 days. Continuous
active use can exceed Free compute. The 1 GB database limit includes indexes and
receipts, not just text. No always-free claim is made without measuring usage.

Expired OAuth rows, receipts and abandoned staging rows are not automatically
purged yet. Monitor storage before sustained use. Do not delete unacknowledged
receipts or staging while a connector can retry them.

## Tests

```console
npm test
node test/e2e/cloud-vm.mjs
node test/e2e/tool-contracts.mjs
```

The second command is an operator harness for the prepared Cua macOS VM. It
requires the disposable `vm_environment.py` service, the test Agent View binary
and the signed-in browser forwarded on port 19949. It uses synthetic sessions
and a deterministic local model. No paid model credential is needed.
The third command tests the deployed cloud tool contracts with a disposable,
source-bound synthetic projection and simulated status metadata. It does not
execute an agent or change the personal primary source. Fixture grants and rows
are removed on exit.

Verified against the deployed Worker and Neon: owner Google login, code/PKCE
exchange, actual OpenCode history projection, tool-output canary exclusion,
remote admission and reply sync, retry deduplication, revert/unrevert, forks,
dashboard-independent login-item delivery, offline queueing, refresh rotation and
revocation. Actual authenticated ChatGPT text search and approved send/reply have
also passed. The owner confirmed private iPhone Live, and its synthetic phone
test reached the outbox. Personal-history sync is now enabled: the initial
snapshot committed 2,608 sessions and 72,252 messages. An authenticated MCP read
matched source-visible text exactly without logging it. The login daemon runs
independently of the dashboard and retains shared V2 service discovery.

Backend queries can read copied text. OpenAI sees retrieved content. Logs exclude
request bodies, SQL, credentials and URLs with codes. The source database and
provider credentials remain local. Full encrypted backups and remote wake are
not part of this implementation.
