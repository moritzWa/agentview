# Voice `send_message`: "reply authority was not granted for this session"

Research note, 2026-11-12. No code changed.

## Summary

- The error means the session row lacks the `Reply` capability. It has
  nothing to do with the session's state.
- In the observed failures the cause was **ownership**, not `needs_you`.
  Agent View grants `Reply` only to OpenCode sessions in its supervisor's
  owned list. `ads-performance-research` was not in it, so every send failed,
  whatever its state.
- This is a gap, not intended behavior. The voice already adopts history-only
  sessions on demand, but skips sessions that are listed in the dashboard and
  not owned. Those are the ones that fail.
- A second, narrower denial is intended: a session blocked on OpenCode's own
  `question` or `permission` prompt.
- Recommended fix: in the voice host, adopt any unowned OpenCode session
  before sending. Keep the supervisor's ownership check as the gate. Details
  below.

## Evidence

From `~/.local/state/agentview/voice/last-call.log` (the call of 2026-11-12):

| t (s) | `send_message` target | Session state | Result |
| --- | --- | --- | --- |
| 140 | `this` (eu-biotech-data-leads) | **working**, row marked `cannot_message` | reply authority error |
| 582, 662 | `s34` | working | ok |
| 615, 721 | `s11` (ads-performance-research) | **needs_you** (the `read_session` just before reports it) | reply authority error |

- A **working** session failed the same way as the needs_you one, so state is
  not the discriminator. (`cannot_message` is `Reply` missing from the row, so
  that session had no grant either; I did not check its ownership record.)
- `s11`'s id (`ses_e384a2d3affeqJtwBpjQqGzFv6`) is in neither the
  supervisor's owned record (`~/.local/state/agentview/opencode/server.json`,
  697 sessions) nor `opencode-owned.json`. `turn-verdicts.json` does hold a
  `NeedsInput` verdict for it, so the classifier is what marked it needs_you.
- The voice digest already tells the model: `list_sessions` rows carry
  `cannot_message: true` (`voice/digest.rs`). Nothing tells the model what to
  do about it, and `send_message` still goes through the same path.

## 1. What "reply authority" is

`Capability::Reply` is one entry in the `capabilities` set on each
`AgentSession`. `ControlHub::reply` and `ControlHub::edit_last_message`
(`src/control.rs`) bail with this exact message when the entry is missing. The
dashboard's own Peek overlay shows a related notice for the same reason
("... is read-only; reply authority was not granted", `src/app.rs`).

Where it is granted for OpenCode:

- `normalize_record` (`adapters/opencode.rs`): every discovered host session
  starts with `Inspect` only.
- `grant_managed_capabilities` (same file), reached from `enrich` only for
  sessions in the supervisor's owned list:
  - Working: `Reply` + `Interrupt`
  - Completed / ReadyForReview: `Reply`
  - NeedsInput with raw state `turn failed`: `Reply` (a failed turn is
    continued by sending the next message)
  - other NeedsInput (`question asked`, `permission requested`) and Unknown:
    nothing

The controller then checks again: `OpenCodeController::reply` calls
`require_owned` ("refusing to control an OpenCode session not created by this
supervisor") and refuses NeedsInput other than `turn failed` with "needs
provider-native recovery".

`docs/control-model.md` states the intent: Launch/reply is "Disabled" for
external sessions. Agent View only writes to sessions it owns.

## 2. Why a needs_you session is denied

Two separate causes, easy to confuse:

**A. Not owned (the observed bug).** The session was started outside this
dashboard, for example a background agent from `opencode-bg.sh` on the shared
server, or a TUI the user opened by hand. It is listed (external sessions are
shown), but ownership was never recorded, so it has `Inspect` only.

How a session becomes owned: launched from the dashboard, restored from the
picker (`adopt`), or adopted by the voice host. The last one is the hole:

```text
DashboardHost::controllable(id)           src/voice/host.rs
  in_dashboard?  yes -> skip adopt        <- external rows land here
                 no  -> adopt(found)      <- history-only rows only
  session(id)    returns the dashboard row as is (Inspect only)
ControlHub::reply -> no Reply -> error
```

Compare `stop`: on "not created by this supervisor" it falls back to
`force_interrupt`, so the voice can stop an unowned session but not message
it.

**B. Blocked on an OpenCode prompt (intended).** If the server reports a
pending `question` or `permission` request, the raw state is `question asked`
or `permission requested`, and both layers refuse the reply. The intent is that
the answer is typed in OpenCode, because a free-text prompt would not answer
the structured request.

**Not a cause: the classifier.** `turn_classifier` flips an idle session to
NeedsInput after `enrich` has already granted capabilities, and it does not
touch them. A classifier-labelled, owned session keeps `Reply`. So "its last
message ends on a question" does not by itself cause a denial.

## 3. Bug or intended?

- Cause A is a **bug in the voice path**. The product promise in `voice.md` and
  the `send_message` tool text is "send your words to that session", "Sessions
  found by search_history can be messaged too", and "go/yes/continue" goes to
  whichever session asked. A session the dashboard shows can be read, stopped
  and closed by voice but not answered. That is inconsistent.
- The ownership rule itself is deliberate and worth keeping for the dashboard's
  own typing path. It is a guard against writing to a session another harness
  controls. The developer telling the voice to answer is explicit consent for
  that one session, which is what history-row adoption already assumes.
- Cause B is **intended**, but the voice handles it badly: the model sees
  "needs_you_because: waits for the answer there" and may still call
  `send_message`.
- Two UX flaws make it worse:
  - The error text is jargon. The model repeated it aloud ("it hasn't granted
    reply authority. You'll need to type send it yourself").
  - `needs_you_because` falls back to "its last message ends on a question for
    the developer" for every NeedsInput raw state, including `waiting at prompt`
    and `turn interrupted`. The label can be wrong.

## 4. Recommended fix

Do not special-case needs_you. The state is not what is blocking the send, and
a needs_you rule would leave working and done unowned sessions broken. Fix the
ownership gap.

1. **Adopt before sending** (core fix, `src/voice/host.rs`). In
   `controllable`, adopt any OpenCode host session that is not owned, not only
   ones missing from the dashboard. Build a `RestorableSession` from the
   dashboard row (`id`, `provider_session_id`, `name`, `cwd`).
   - The row in the dashboard list is stale (no `Reply`), so after a
     successful `adopt` return it with `Reply` (and `Interrupt` when working)
     inserted. This is what `session()` already does for the synthesized
     fallback row and what `stoppable` does for `Interrupt`.
   - Keep `OpenCodeController::reply`'s `require_owned` as the real gate. Do
     not widen `grant_managed_capabilities`.
   - Apply to `edit_last` too, since it shares `controllable`.
   - Skip the adopt for non-OpenCode rows; the host only lists OpenCode.
2. **Keep the question/permission refusal** but report it in words. Map the
   controller error to something like "it is waiting on a question prompt in
   OpenCode; answer it there", and return it as a structured tool result with
   a hint, so the model says it plainly.
3. **Fix the `needs_you_because` fallback.** Only say "ends on a question" when
   the cause is the classifier. Use "waiting at its prompt" or "stopped midway"
   for the other raw states.
4. **Prompt line** (one short sentence in `backend.md` / `xai.md`, per the
   preference for minimal prompts): a row with `cannot_message` is not
   messageable; say that it must be answered in OpenCode, never "reply
   authority". With fix 1 this should become rare.

### Tests to add with the fix

- Unit, `voice/host.rs`: an unowned dashboard row is adopted and then
  replied to; an adopt failure returns the controller's error.
- Unit, controller: a `question asked` owned session still refuses reply, with
  the readable message.
- E2E (cua VM, per the usual rule): start an OpenCode session from a plain
  `opencode` shell outside Agent View, let it end on a question, then say "send
  it" to the voice (or call the tool through a fake host) and confirm the text
  arrives.

### Risks

- Adoption writes the session into the supervisor's owned record, so Agent View
  will then show and manage it like its own (interrupt, move). This already
  happens for restored history rows, so no new category of authority.
- A session running in a TUI of another process: `prompt_async` goes to the
  shared server, so the message queues behind the running turn like any other.
  The dashboard cannot tell if another client is typing at the same moment.

## Effort

Fix 1 plus tests: about one agent-hour. Fixes 2 to 4: another half hour.
