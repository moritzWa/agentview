import {test} from 'node:test';
import assert from 'node:assert/strict';
import {liveHealth, sessionHealth} from '../private/health.mjs';

const now = 1_700_000_000_000;
const working = {health:'running', turn_started_at:now-120_000, last_progress_at:now-5_000, last_progress_kind:'tool', in_flight:'tool'};

test('a live open tool is running, with duration, and is not called stalled', () => {
  const report = sessionHealth(working, {now, statusFresh:true, observedAt:now-1_000});
  assert.equal(report.health, 'running');
  assert.equal(report.health_live, true);
  assert.equal(report.in_flight, 'tool');
  assert.equal(report.working_for_ms, 120_000);
  assert.equal(report.progress_age_ms, 5_000);
  assert.match(report.note, /not stalled/);
  assert.doesNotMatch(report.basis, /fail/);
});

test('a stale snapshot is unknown now, even if the last word was stalled or failed', () => {
  for (const health of ['potentially_stalled', 'failed', 'running']) {
    const report = sessionHealth({health, turn_started_at:now-3_600_000}, {now, statusFresh:false, observedAt:now-120_000});
    assert.equal(report.health, 'unknown', health);
    assert.equal(report.health_at_observation, health);
    assert.equal(report.health_live, false);
    assert.equal(report.working_for_ms, null);
    assert.match(report.note, /not a live reading/);
  }
});

test('overview keeps only a live health word', () => {
  assert.equal(liveHealth('potentially_stalled', true), 'potentially_stalled');
  assert.equal(liveHealth('failed', false), 'unknown');
  assert.equal(liveHealth('invented', true), 'unknown');
  assert.equal(liveHealth(undefined, true), 'unknown');
});

test('confirmed failure is named only when the observation is live', () => {
  const report = sessionHealth({health:'failed'}, {now, statusFresh:true, observedAt:now});
  assert.equal(report.health, 'failed');
  assert.match(report.basis, /confirmed/);
  assert.match(report.claim_rule, /not a failure/);
});
