# Verification, November 16, 2026

## Deployed cloud and macOS VM

`cloud-vm.mjs` passed against the private Cloudflare Worker and Neon Free.
The VM ran the real OpenCode V2 binary and the new Agent View connector.
The model was a deterministic local HTTP fixture, not a paid LLM.

Verified: Google owner login and consent, code/PKCE token exchange, source history
upload, full-text search, actual tool-output canary exclusion, remote sends,
same-ID retry deduplication, agent reply sync, native revert/unrevert, native
forks, dashboard-independent login-item sync, stopped-connector queueing,
reconnect delivery, refresh rotation and immediate revocation.

A separate payload fixture uploaded one 741,020-byte visible user message
through the Free Worker without a request failure. This is a payload test, not a
claim about sustained Free-tier CPU or monthly usage.

Neon's control API reported 1,077 compute seconds (about 0.30 CU-hours), 252,391
egress bytes and a 31,768,576-byte branch logical size during this test run.
The endpoint's `suspend_timeout_seconds: 0` means the plan default, not disabled
autosuspend. Neon Free fixes that default at five minutes.

## Actual ChatGPT text client

The owner connected the private **AgentVoice** plugin through real OAuth.
ChatGPT searched for `BRIDGE_E2E_AGENT_REPLY` and quoted a matching result.
It then read a disposable session and requested approval for a mutating send.
After **Allow once**, it queued `CHATGPT_SEND_PROOF_1794805408039`, observed
`accepted`, and read the exact user input and subsequent fixture-model reply.

Test conversation:
`https://chatgpt.com/c/6afa8e75-8894-83ea-b85a-6c89fe0cedde`

This check caught a browser-only issue that the same-origin integration callback
did not: the consent page's form-action CSP blocked its redirect to ChatGPT.
The policy now permits the already-allowlisted ChatGPT callback origin.

## iPhone confirmation and personal-history rollout

The owner confirmed the private iPhone Live check on November 16. The backend
independently recorded its `PhoneTest_20261116_0939` operation queued against the
offline disposable VM. This verifies private write-tool eligibility, not execution
of that particular queued test. The earlier VM harness already proved admission
and reply sync using the same transport.

After confirmation, the personal Mac paired through Google owner consent. Its
initial upload committed 2,608 sessions and 72,252 visible messages (38,307,859
text bytes). Local and cloud counts matched with no unacknowledged batches.
Database size was 116,178,944 bytes at that point. An authenticated read-only
MCP grant listed real history and returned an exact source-message hash match.
No personal chat bodies were logged by the verifier.

The native login item is running with shared V2 service-discovery settings.
Closing the dashboard does not stop it. A first-day Free-tier usage review is
scheduled; this is not a claim of sustained monthly quota compliance.

## Not claimed

- Live voice audio in this VM. A fake-microphone run opened voice mode but did
  not produce a new transcribed turn, so it is not evidence of a voice tool call.
- Sustained monthly quota compliance or wireless remote wake.
- Native permission bypass. The connector does not alter permission rules.

## Full local source audit, without upload

The Rust source reader projected all 2,609 local sessions: 72,221 visible
messages and 38,285,538 UTF-8 text bytes. The largest encoded message was
761,406 bytes, within the 850 KB connector batch budget. No personal text left
the Mac during that pre-rollout audit. It caught an overly small 500K-character
message limit; the
protocol now accepts up to one million characters, still subject to its 1 MB
request-byte limit. The production reader and backend have a matching large-paste
regression test. Long-message pagination also preserves Unicode character offsets.

Local coverage includes inline/split text, legacy-only sessions, source cursor
gap reconciliation, revert resolution, hidden reasoning/tool metadata, crash-safe
batch replay, chunked deltas, rollback, stale-context sends, cancellation, expiry,
uncertain admission and owner/device isolation.
# Real phone workflow repair, November 16, 2026

The initial rollout checks did not prove the user's workflow. The real ChatGPT
conversation exposed missing native status metadata, cursor/offset confusion,
and an instruction that reached `needs_confirmation` instead of OpenCode. The
Mac was asleep; an appended desktop turn later invalidated the old strict cursor
check. A cloud `queued` receipt was not proof of delivery.

Candidate Worker version `756a6d27-a784-4611-b623-600567980706` passed a new
deployed-cloud/macOS-Cua regression with real OpenCode and a local fixture model:

- A phone send based on an older mirrored read survived an appended desktop turn.
  Native admission, a later reply and a completed native state matched its input ID.
- Retrying the operation left exactly one native user input.
- A native revert still stopped admission with `needs_confirmation`.
- One `list_sessions(status="needs_you")` call found an actual native approval.
- The personal primary source stayed unchanged throughout the fixture run.

The reconnect test also exposed stale registration handling after a VM reboot.
The candidate connector now verifies and adopts the live replacement service.
Backend tests: 30 passed. Agent View library tests: 948 passed, 7 ignored.

The authenticated ChatGPT text client then made one latest-message read with
`limit: 3` and no offset scanning. It sent one new instruction to a separate
synthetic personal-Mac session, checked the actual `accepted` receipt, and read
`MOBILE_REPAIR_NATIVE_OK_20261116` after that operation's input. It did not reuse
the old fixture reply or claim completion from admission alone. The old personal
daemon had no native-state snapshot, so `completion_confirmed` correctly stayed
false even after the new reply appeared. A follow-up adds an explicit `page_kind`
field because the client over-explained the former `no_newer_messages: false`.

These are transport and native-state checks, not a new iPhone Live verification.
Agent View PR #61 (repair mobile queue context and expose native session states)
then passed all CI checks, merged as `9ac9c22`, and was installed. The independent
personal login daemon was restarted and its executable path and inode matched
the installed binary. The authenticated ChatGPT client then made exactly two
status calls, without reading histories: `needs_you` and `all`. Both returned
`status_fresh: true`, `partial: false`, `mac_seen_recently: true`. Its receipt
check returned `accepted`, `reply_available: true`, `completion_confirmed: true`
and `recipient_status: done` for the new synthetic instruction.

The phone's original implementation operation was never delivered or resent.
After its requested changes shipped, ChatGPT cancelled only that obsolete blocked
operation and checked its final `cancelled` state. An independent backend read
confirmed it. The original receipt and body remain available for audit; the old
operation no longer blocks later instructions to that session.

The temporary personal-Mac test session was deleted through the native API and
its normal history deletion synced. VM services, the VM and test browsing were
stopped; only the intended production connector remains running.
# Tool audit follow-up and second review, November 16, 2026

All 42 backend tests passed. Deployed Worker version
`d1150c68-a74f-4460-870c-51d6b49ee6a0` then passed `tool-contracts.mjs` against
real Cloudflare/Neon with a disposable, source-bound synthetic projection:

- Forward pages returned every message exactly once under the response budget.
  An exhausted forward read returned no old replies.
- Search returned the matching passage deep inside Unicode text. Its bounded
  excerpt and character offset matched a subsequent `read_message` result.
- Session-list paging detected a changed status snapshot instead of skipping rows.
- A cloud-only instruction stayed queued, then cancelled. No native execution
  or admission was simulated. The personal primary source was unchanged.

The second code review also fixed unadmitted confirmation requests that never
expired, missing/stale native records labelled fresh, inconsistent input/reply
read snapshots, and access revoked while a tool waited or mutated its queue.
Regression tests exercise the real Worker router as well as individual handlers.

Correction to the first audit: the outer authenticated MCP router already renewed
mobile activity on every request. Direct-handler tests bypassed that router, so
the claim that receipt polling falls back to 15-minute sync was wrong. Receipt
checks now additionally wait for both history and native-state refresh and return
freshness metadata. This is not a fix for a nonexistent idle-sync fallback.

Known scope limits remain explicit: search returns at most 30 matches without a
continuation cursor; mobile does not expose create-session, stop-agent or native
approval/question-answer tools. The older renewal/retention/source-replacement
maintenance items remain tracked separately in issue #18. These tests are not a
new ChatGPT client, iPhone Live, or native-agent execution verification.
# Actionable mobile overview, November 16, 2026

The new screenshots exposed two omissions that transport tests did not cover:
the independent runtime reader skipped controller enrichment and saved desktop
turn judgements, and list results had no final-message context. A completed
provider turn was therefore easy to mistake for a task needing nothing.

The candidate now applies desktop control enrichment and exact saved turn labels
without starting new classifier-model calls. Its read-only real-Mac audit found
1 needs-you, 5 working, 7 scheduled and 388 completed sessions, with no partial
snapshot. 957 Rust library tests passed (7 ignored), Clippy passed, and 40 backend
tests passed. The backend regressions include 401 sessions, decision excerpts,
new sessions still syncing, child filtering and reverted/staged-text exclusion.

Deployed overview contracts passed with synthetic status fixtures and preserved
the personal primary source. A fresh authenticated ChatGPT text conversation was
then asked only: "Which of my Agent View sessions are active or need my attention?"
It made one `list_sessions(limit=50)` call using its existing tool schema. The
server returned a complete recent selection of 10 rows, global counts across 402
sessions and latest-turn context for all 10. ChatGPT identified a pending question
without reading individual histories, asking to scan the remaining sessions, or
calling the real data synthetic demo sessions. No messages were sent to OpenCode.

This first client check used the previously installed daemon, so its native
needs-you count was still zero and idle attention assessments were unknown. The
included reply excerpts allowed the client to identify a decision anyway. Native
installation and post-install classification verification are separate rollout
checks; this is not a new iPhone Live test.

Post-install verification then passed on merged commit `23835cf` (PR #65,
actionable mobile overview). Every CI check passed. The independent connector
was restarted and its running executable inode matched the installed binary.
The natural follow-up "Which sessions need my input, and what are they asking me
to decide?" again made exactly one `list_sessions(limit=50)` call, with no history
reads or mutating tools. It received 12 relevant rows, complete counts across 391
visible top-level sessions, fresh runtime and fresh context, and saved attention
assessments on six returned rows. The formal needs-you count was zero at this
later snapshot; two recent unassessed replies still supplied decision context.
ChatGPT identified the actual decisions and prioritized them without claiming
that a zero formal counter meant no attention was needed. It did not call the
real sessions synthetic or ask to scan the remaining inventory.

The final backend deployment is Worker version
`981e7373-0a4f-42ab-95f0-31b68ab0ae4a`. Its deployed synthetic contracts also
passed. This remains text-client and data-path verification, not a fresh phone
voice/audio test. No personal conversation excerpts are copied into this record.
# Directory and two-message context, November 16-17, 2026

PR #67 (directories and two-turn mobile context) merged as `7f6ba18` with every
CI check passing. Local checks passed: 958 Rust library tests (7 ignored), Clippy,
and 46 backend tests. The previously observed Copilot login timing failure passed
on retry. A Mac ARM native-background notification test stalled and passed on a
fresh runner; its evidence is recorded under Agent View issue #32. No assertions
were weakened. The automatic daemon restart failed once, the direct retry
succeeded, and the running executable inode matched the installed binary.

Worker version `8e24cf3c-4beb-420d-a784-f3d7dccecb4e` passed deployed synthetic
contracts for working-directory/project metadata, two visible messages per role,
source ordering, tool/reasoning exclusion and head/tail omission. Fixture grants
and rows were removed; the personal primary source stayed unchanged.

The real authenticated ChatGPT text client was asked to group active sessions by
their actual directory and explain what needed input. Its first restart-time read
was correctly flagged stale and still lacked new directory metadata. After the
connector published a fresh snapshot, one refreshed `list_sessions(limit=50)`
call returned 11 sessions across six actual directories. All 11 had two user and
two assistant text messages, in source order, with no other message roles. Both
runtime and context freshness were true. Context plus compatibility aliases used
16,472 Unicode characters, below the shared 36K budget; long excerpts included
both ends and an explicit middle-omitted marker. ChatGPT grouped by actual returned
paths and identified current decisions without reading extra histories or sending
instructions. No personal directory paths or conversation text are recorded here.

This verifies the text client and data path, not a new iPhone Live/audio test.
