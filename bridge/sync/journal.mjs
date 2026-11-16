import { DatabaseSync } from 'node:sqlite';
import { chmodSync } from 'node:fs';
import { canonical, parseBatch, SyncError } from './projection.mjs';

const APPLICATION_ID = 0x41565359; // AVSY, NOT the OpenCode source DB.

/** Local integration harness journal. Production Agent View will use its Rust SQLite adapter. */
export class LocalJournal {
  constructor(path, sourceKey) {
    if (typeof sourceKey !== 'string' || !sourceKey.length) throw new SyncError('source_key_required', 400);
    this.db = new DatabaseSync(path);
    try {
      const app = this.db.prepare('PRAGMA application_id').get().application_id;
      const tables = this.db.prepare("SELECT name FROM sqlite_master WHERE type='table'").all();
      if ((app !== 0 && app !== APPLICATION_ID) || (tables.length && app !== APPLICATION_ID)) throw new SyncError('not_a_sync_journal', 400);
      // SQLite derives WAL/SHM permissions from the main file. Restrict it BEFORE enabling WAL.
      if (path !== ':memory:') chmodSync(path, 0o600);
      this.db.exec(`PRAGMA application_id=${APPLICATION_ID}; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
        CREATE TABLE IF NOT EXISTS binding (id INTEGER PRIMARY KEY CHECK (id=1), source_key TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS pending (id INTEGER PRIMARY KEY CHECK (id=1), batch TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS acknowledged (session_id TEXT PRIMARY KEY, receipt TEXT NOT NULL);`);
      const binding = this.db.prepare('SELECT source_key FROM binding WHERE id=1').get();
      if (binding && binding.source_key !== sourceKey) throw new SyncError('source_key_mismatch', 409);
      this.db.prepare('INSERT OR IGNORE INTO binding VALUES (1,?)').run(sourceKey);
    } catch (e) { this.db.close(); throw e; }
  }
  async enqueue(input) {
    const batch = parseBatch(input), encoded = canonical(batch);
    const existing = this.db.prepare('SELECT batch FROM pending WHERE id=1').get();
    if (existing) {
      if (existing.batch === encoded) return;
      throw new SyncError('pending_batch_exists');
    }
    const prior = this.lastReceipt(batch.session_id);
    if (batch.kind === 'delta') {
      if (batch.chunk && !prior?.committed) {
        if (prior?.next_chunk!==batch.chunk) throw new SyncError('local_cursor_gap');
      } else if (!prior?.committed || prior.through !== batch.from) throw new SyncError('local_cursor_gap');
    } else if (prior?.committed || batch.chunk !== (prior?.next_chunk ?? 0)) {
      throw new SyncError('local_baseline_conflict');
    }
    this.db.prepare('INSERT INTO pending VALUES (1,?)').run(encoded);
  }
  async pending() {
    const row = this.db.prepare('SELECT batch FROM pending WHERE id=1').get();
    return row ? parseBatch(JSON.parse(row.batch)) : null;
  }
  async ack(id, receipt) {
    this.db.exec('BEGIN IMMEDIATE');
    try {
      const batch = await this.pending();
      const committed = batch?.upload_id !== undefined ? batch?.complete : true;
      if (!batch || batch.batch_id !== id || receipt.batch_id !== id || receipt.session_id !== batch.session_id
        || receipt.committed !== committed || receipt.through !== (committed ? batch.through : null)
        || receipt.next_chunk !== (committed ? null : batch.chunk + 1)) throw new SyncError('invalid_sync_receipt', 502);
      this.db.prepare('INSERT INTO acknowledged VALUES (?,?) ON CONFLICT(session_id) DO UPDATE SET receipt=excluded.receipt').run(batch.session_id, JSON.stringify(receipt));
      this.db.exec('DELETE FROM pending; COMMIT');
    } catch (e) { this.db.exec('ROLLBACK'); throw e; }
  }
  lastReceipt(sessionId) {
    const row = this.db.prepare('SELECT receipt FROM acknowledged WHERE session_id=?').get(sessionId);
    return row ? JSON.parse(row.receipt) : null;
  }
  close() { this.db.close(); }
}
