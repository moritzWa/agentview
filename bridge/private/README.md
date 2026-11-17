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

Tools: `list_sessions`, `search_history`, `read_session`, `read_message`,
`send_session_message`, `message_status`, and `cancel_message`.

The instruction queue is separate from mirrored messages. Stable operation IDs
deduplicate cloud writes. Stable OpenCode message IDs reconcile local admission
after a lost response. Rewritten context requires a fresh read and confirmation;
appended turns and streaming suffixes do not invalidate a queued instruction.
Native permissions are not bypassed. Queued does not mean executed.

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
- Each listed row includes bounded source-visible excerpts of the latest user
  request and assistant reply, with full-message IDs when more detail is needed.
  The assistant excerpt preserves the ending where questions usually appear.
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
history every 15 minutes while idle. Mobile use requests five minutes of faster
checks. Neon can suspend between idle syncs.

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
