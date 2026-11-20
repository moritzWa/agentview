import { digest, SyncError } from '../sync/projection.mjs';
import { requireGrant } from './grant.mjs';

const scope = p => [p.owner_id, p.device_id, p.database_epoch];
const where = 'owner_id=$1 AND device_id=$2 AND database_epoch=$3';
const DENY = /^\/(etc|private|System|usr|bin|sbin|var|dev|tmp|Library|Applications|opt)(\/|$)/;

/** Shape only. The Mac decides whether the directory exists and is allowed. */
export function directoryShape(path) {
  if (typeof path !== 'string' || path.length < 2 || path.length > 1024) throw new SyncError('invalid_directory', 400);
  const trimmed = path.length > 1 ? path.replace(/\/+$/, '') : path;
  if (!trimmed.startsWith('/') || /[\u0000-\u001f]/.test(trimmed)) throw new SyncError('invalid_directory', 400);
  const parts = trimmed.split('/');
  if (parts.includes('..') || parts.includes('.')) throw new SyncError('invalid_directory', 400);
  if (trimmed === '/' || DENY.test(trimmed)) throw new SyncError('directory_not_allowed', 400);
  return trimmed;
}

export class Starts {
  constructor(db, gate) { this.db = db; this.gate = gate; }
  async enqueue(principal, { operation_id, directory, text }) {
    if (!principal.scopes?.split(' ').includes('messages:send')) throw new SyncError('write_permission_required', 403);
    if (!/^[A-Za-z0-9_-]{8,160}$/.test(operation_id ?? '') || typeof text !== 'string' || !text.trim() || text.length > 50_000) throw new SyncError('invalid_start', 400);
    directory = directoryShape(directory);
    const hash = await digest({ operation_id, directory, text });
    await this.gate.signal(`${principal.device_id}|${principal.database_epoch}`);
    const receipt = await this.db.transaction(async tx => {
      await requireGrant(tx, principal, { scope: 'messages:send', lock: true });
      const s = scope(principal);
      const device = (await tx.query(`SELECT 1 FROM av_credentials WHERE ${where} AND audience='sync' AND revoked=false AND expires_at>$4 LIMIT 1`, [...s, Date.now()])).rows[0];
      if (!device) throw new SyncError('device_disconnected', 409);
      await tx.query('SELECT pg_advisory_xact_lock(hashtextextended($1, 0))', [JSON.stringify([...s, operation_id])]);
      const old = (await tx.query(`SELECT * FROM av_starts WHERE ${where} AND operation_id=$4`, [...s, operation_id])).rows[0];
      if (old) { if (old.payload_hash !== hash) throw new SyncError('operation_id_conflict'); return publicReceipt(old); }
      const row = (await tx.query(`INSERT INTO av_starts (owner_id,device_id,database_epoch,operation_id,directory,text,payload_hash,created_at,expires_at)
        VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING *`, [...s, operation_id, directory, text, hash, Date.now(), Date.now() + 86400_000])).rows[0];
      return publicReceipt(row);
    });
    await this.gate.signal(`${principal.device_id}|${principal.database_epoch}`);
    return receipt;
  }
  async lease(principal) {
    return this.db.transaction(async tx => {
      await requireGrant(tx, principal, { audience: 'sync', scope: 'messages:deliver', lock: true });
      const s = scope(principal), now = Date.now();
      await tx.query(`UPDATE av_starts SET state=CASE WHEN state='queued' THEN 'expired' ELSE 'uncertain' END WHERE ${where} AND state IN ('queued','leased') AND expires_at<=$4`, [...s, now]);
      const row = (await tx.query(`SELECT * FROM av_starts WHERE ${where} AND (state='queued' OR (state='leased' AND lease_until<$4)) ORDER BY sequence LIMIT 1 FOR UPDATE SKIP LOCKED`, [...s, now])).rows[0];
      if (!row) return null;
      await tx.query(`UPDATE av_starts SET state='leased', lease_until=$5 WHERE ${where} AND operation_id=$4`, [...s, row.operation_id, now + 60_000]);
      return { operation_id: row.operation_id, directory: row.directory, text: row.text, expires_at: Number(row.expires_at) };
    });
  }
  async ack(principal, { operation_id, status, session_id, reason }) {
    if (!['started', 'created_not_started', 'failed', 'uncertain'].includes(status)) throw new SyncError('invalid_receipt', 400);
    const needsId = status === 'started' || status === 'created_not_started';
    if (needsId ? typeof session_id !== 'string' || session_id.length < 1 || session_id.length > 200 : session_id) throw new SyncError('invalid_receipt', 400);
    if (reason != null && (typeof reason !== 'string' || !/^[a-z_]{1,40}$/.test(reason))) throw new SyncError('invalid_receipt', 400);
    const result = await this.db.transaction(async tx => {
      await requireGrant(tx, principal, { audience: 'sync', scope: 'messages:deliver', lock: true });
      return tx.query(`UPDATE av_starts SET state=$5, session_id=COALESCE($6, session_id), receipt=$7
        WHERE ${where} AND operation_id=$4 AND state IN ('leased',$5)
        AND (session_id IS NULL OR session_id IS NOT DISTINCT FROM $6) RETURNING *`,
        [...scope(principal), operation_id, status, needsId ? session_id : null, JSON.stringify({ reason: reason ?? null })]);
    });
    if (!result.rows.length) throw new SyncError('receipt_conflict');
    await this.gate.signalReceipt?.(`${principal.device_id}|${principal.database_epoch}`, operation_id);
    return publicReceipt(result.rows[0]);
  }
  async status(principal, operation_id) {
    await this.db.query(`UPDATE av_starts SET state=CASE WHEN state='queued' THEN 'expired' ELSE 'uncertain' END WHERE ${where} AND operation_id=$4 AND state IN ('queued','leased') AND expires_at<=$5`, [...scope(principal), operation_id, Date.now()]);
    const row = (await this.db.query(`SELECT * FROM av_starts WHERE ${where} AND operation_id=$4`, [...scope(principal), operation_id])).rows[0];
    if (!row) throw new SyncError('start_not_found', 404);
    return publicReceipt(row);
  }
  async cancel(principal, operation_id) {
    if (!principal.scopes?.split(' ').includes('messages:send')) throw new SyncError('write_permission_required', 403);
    const row = await this.db.transaction(async tx => {
      await requireGrant(tx, principal, { scope: 'messages:send', lock: true });
      return (await tx.query(`UPDATE av_starts SET state='cancelled' WHERE ${where} AND operation_id=$4 AND state IN ('queued','cancelled') RETURNING *`, [...scope(principal), operation_id])).rows[0];
    });
    if (!row) throw new SyncError('cannot_cancel_admitted_or_inflight_message', 409);
    await this.gate.signal(`${principal.device_id}|${principal.database_epoch}`);
    return publicReceipt(row);
  }
}

function publicReceipt(row) {
  return {
    operation_id: row.operation_id,
    status: row.state,
    directory: row.directory,
    session_id: row.session_id ?? null,
    started: row.state === 'started',
    prompt_accepted: row.state === 'started',
    reason: row.receipt?.reason ?? null,
    expires_at: Number(row.expires_at),
    next_action: ['queued', 'leased'].includes(row.state) ? 'check_start_status' : row.state === 'started' ? 'session_health' : null,
  };
}

export const START_RULE = 'Say say_to_owner. Never claim a session exists unless session_id is set. Queued is not started. started means OpenCode accepted the task, not that the work is finished.';

const SAYS = {
  not_started: 'Not started. No session was created.',
  queued: 'Queued. No session has been created yet.',
  queued_offline: 'Queued. Your Mac looks offline, so no session has been created.',
  started: 'OpenCode created the session and accepted the task. That does not mean the work is finished.',
  created_not_started: 'OpenCode created a session but did not accept the task. It is not working.',
  failed: 'Not started. The Mac could not create it.',
  uncertain: 'I cannot tell if a session was created. I will not start another one with this id.',
  cancelled: 'Cancelled. No session was created.',
};

export function startView(receipt, freshness = {}) {
  const status = receipt?.status;
  const session_id = typeof receipt?.session_id === 'string' && receipt.session_id ? receipt.session_id : null;
  const base = (delivery_state, claim_ceiling, say) => ({
    delivery_state, claim_ceiling, say_to_owner: SAYS[say], session_id: claim_ceiling === 'none' ? null : session_id,
    started: delivery_state === 'started', report_rule: START_RULE,
  });
  switch (status) {
    case 'queued':
    case 'leased':
      return base(freshness.mac_seen_recently === false ? 'queued_offline' : 'queued', 'none', freshness.mac_seen_recently === false ? 'queued_offline' : 'queued');
    case 'started':
      return session_id ? base('started', 'started', 'started') : base('uncertain', 'none', 'uncertain');
    case 'created_not_started':
      return session_id ? base('created_not_started', 'created', 'created_not_started') : base('uncertain', 'none', 'uncertain');
    case 'failed': return base('failed', 'none', 'failed');
    case 'cancelled': return base('cancelled', 'none', 'cancelled');
    case 'expired': return base('not_started', 'none', 'not_started');
    default: return base('uncertain', 'none', 'uncertain');
  }
}
