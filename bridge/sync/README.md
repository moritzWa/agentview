# Text-only sync core

The core is deployed through the separate [private Worker](../private/README.md).
Agent View provides the production Rust source reader, journal and login daemon.
The public MCP probe remains in voizecode, synthetic and separate from this code.

## Scope

All sessions, all dates: user text and visible assistant text only. Tool calls,
tool results, hidden reasoning, attachments, paths, model-native payloads and
arbitrary source metadata are never serialized by the projection adapter. Visible
text itself may still contain secrets or quoted tool output; it is not a redactor.

Local sizing pass: about 38 MB visible text, 49 MB with compact
message metadata for 72,056 messages. This measured the installed inline-content
V2 projection only, not inherited fork rendering, legacy-only sessions or actual
Postgres index/storage size. It is an estimate, not a proof of full-history coverage.

The OpenCode DB is authoritative. Postgres receives only connector-projected
history. Phone instructions belong in the separate private outbox, never optimistic
inserts into the mirrored conversation. Full-DB recovery backups are deferred
and out of scope. Prefer Workers Free + Neon Free, subject to CPU/quota tests.

## Files and guarantees

- `projection.mjs`: explicit field allowlist. User `text` and assistant content
  tagged `text` only. Known tool/reasoning parts are ignored. Unknown source part
  types, malformed data or a missing inline-content array fail closed instead of
  guessing. Empty projections return null: the eventual change adapter must emit
  a delete if a previously visible source message becomes empty/non-text.
- `schema.sql` + `store.mjs`: owner/device/database-epoch scoped Postgres rows,
  opaque hashed scoped credentials, staged initial baseline, delta application,
  duplicate receipts, cursor validation and transactional rollback. Separate
  read/sync credentials are required, checked for expiry and revocation.
- `http.mjs`: authenticated, no-store request handler with a 1 MB body limit.
  No public provisioning, signup, pairing or login endpoint. No insecure local
  auth bypass. No SQL/body/token error logging. The handler requires an injected
  store; absent configuration fails closed. Never attach it to the public probe.
- `client.mjs`: sends exactly the persisted pending batch over HTTPS, disallows
  redirects, verifies the receipt, and advances local state only after a matching
  ACK. No pending batch means no request and no Neon wake-up from history sync.
- `journal.mjs`: Node 24 local integration harness, not the production Mac daemon.
  Durable pending batch and receipt in a separate user-only SQLite file, bound to
  one source identity. Refuses unrelated SQLite files. The real Agent View daemon
  must use its shared Rust adapters rather than a second JavaScript source reader.

## Protocol

Every batch is versioned and belongs to ONE session. The authenticated credential,
not request JSON, supplies owner/device/database epoch. Source IDs are preserved.
Message `order` is source message order, not the durable event cursor.

Initial baseline: capture a consistent source snapshot and event high-watermark,
then upload chunks with one `upload_id`, identical state/high-watermark and
monotonically increasing `chunk`. Every chunk has a distinct stable `batch_id`.
Retry a chunk with the same body/ID. Partial baselines remain invisible. The final
chunk atomically exposes all staged messages and the captured cursor. Later source
events replay after that cursor. No rebaseline-over-live-generation is implemented:
cursor loss or source replacement must stop until explicit generation recovery is
built, not overwrite live history opportunistically. Interrupted baselines need
their retained local snapshot to continue; abandon/restart cleanup is not built.

Deltas: send projected changed rows, deleted IDs and current session visibility
state with `from` equal to the acknowledged cursor. `through` must advance it.
The server applies rows/state/cursor and stores the receipt in ONE transaction.
An identical repeated batch ID returns the original receipt; different contents
under that ID conflict. Gaps and stale/out-of-order batches fail. The local
journal retains the exact batch until the server ACK is saved durably.

`session.visible_before` is an EXCLUSIVE source message-order boundary. The local
adapter must resolve OpenCode's staged revert message ID using the installed
source's actual semantics. It must not treat message IDs as ordinal numbers.
Reads and literal per-session searches both filter that boundary. Clearing it
restores visibility of retained rows. Committed source deletion emits tombstones.
Already-returned ChatGPT text cannot be recalled by reverting locally.

Core session reads return at most 100 messages. The private MCP layer adds global
Postgres full-text search, smaller response budgets and paginated message text.

## Testing

Run `npm test` from `bridge` through the normal heavy-job queue. All test data is
synthetic. PGlite runs actual PostgreSQL SQL locally; it does not replace Neon
deployment, connection/concurrency, quota or security validation.

Tests exercise mixed text/tool projection, unknown schemas, chunk visibility,
same-ID edits, deletion, staged revert/unrevert, fork/session isolation, wrong
owner/device/epoch, expiry/revocation/audience, SQL rollback, lost server ACK,
failed local checkpoint, journal reopen, file permissions, unsafe source-file
refusal, HTTP size/validation limits, and no idle history requests. Existing probe
tests in voizecode prove the public tool surface still exposes synthetic data only.

## Integration status and remaining limits

Owner Google login, MCP code/PKCE OAuth, revocation, device pairing, a private
Workers/Neon deployment and a separate instruction outbox are implemented.
The Rust daemon reuses Agent View's profile resolver and shared supervisor.
Its source reader supports inline/split V2 text and unmigrated legacy sessions.
Missing event-body ranges reconcile a current source snapshot. Database identity
changes and regressed watermarks fail closed. Native fork, revert/unrevert and
background/offline delivery passed the deployed Cua VM harness.

ChatGPT's actual authenticated text client has searched synthetic history and
sent a message into the VM's real OpenCode service after write approval.
The owner separately confirmed authenticated iPhone Live. Its synthetic phone
operation reached the outbox. Personal-history rollout followed that gate; an
authenticated cloud read matched the source text exactly. The login daemon is
enabled independently of the dashboard. The original synthetic read-only probe
alone was not evidence of private write eligibility.

Unattended credential renewal beyond the connector's 90-day lifetime, automatic
abandoned-staging/receipt cleanup and explicit source-generation replacement are
not implemented. Real storage/compute usage must be measured after initial sync.
The Node journal is an integration fixture, not the production source reader.

Remote wake is a separate best-effort enhancement: this Mac reports wireless
wake support and enables network wake on AC, but no tested LAN relay exists.
Neither launchd nor cloud polling can wake a disconnected Mac by itself.
