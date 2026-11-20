/** What the model may say about one session's health. No I/O.
 * A stale observation is unknown now. Silence and sync age are not a stall or a failure. */

const LIVE = new Set(['running', 'idle', 'potentially_stalled', 'failed']);
const finite = n => Number.isFinite(Number(n)) && Number(n) > 1e12 ? Number(n) : null;

const BASIS = {
  running: 'The Mac sees this session working, with a recent event or an open tool or shell.',
  idle: 'The agent is not in a working turn.',
  potentially_stalled: 'It was working, with no open tool or shell, and no progress for at least 10 minutes. That is a possible stall, not a confirmed failure.',
  failed: 'The Mac reported a confirmed failed turn. This is not inferred from silence.',
  unknown: 'There is not enough live evidence to classify it.',
};

export function liveHealth(value, statusFresh) {
  return statusFresh && LIVE.has(value) ? value : 'unknown';
}

export function sessionHealth(status, { now, statusFresh, observedAt } = {}) {
  const observed = LIVE.has(status?.health) || status?.health === 'unknown' ? status.health : 'unknown';
  const live = Boolean(statusFresh);
  const health = live ? observed : 'unknown';
  const turn = finite(status?.turn_started_at);
  const progress = finite(status?.last_progress_at);
  const working = health === 'running' || health === 'potentially_stalled';
  return {
    health,
    health_at_observation: observed,
    health_live: live,
    turn_started_at: turn,
    working_for_ms: live && working && turn ? Math.max(0, now - turn) : null,
    last_progress_at: progress,
    last_progress_kind: status?.last_progress_kind ?? null,
    progress_age_ms: live && progress ? Math.max(0, now - progress) : null,
    in_flight: status?.in_flight ?? null,
    observed_at: observedAt ?? null,
    basis: BASIS[health],
    note: live
      ? 'A long tool or shell call is running, not stalled. Do not call this failed unless health is failed.'
      : 'This is not a live reading. Do not call the session stalled, failed, or running from it.',
    claim_rule: 'failed is only a confirmed native turn failure. potentially_stalled is not a failure. Sync age and a bridge restart are not progress and are not a stall.',
  };
}
