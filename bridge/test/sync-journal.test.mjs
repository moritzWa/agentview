import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { LocalJournal } from '../sync/journal.mjs';

const batch = { version: 1, batch_id: 'b1', session_id: 's1', kind: 'baseline', from: 0, through: 10,
  upload_id: 'u1', chunk: 0, complete: true, session: { title: 'Fixture', archived: false, deleted: false, visible_before: null }, messages: [], delete_ids: [] };
const receipt = { batch_id: 'b1', session_id: 's1', committed: true, through: 10, next_chunk: null };

test('journal persists exact batch across process replacement and atomically checkpoints ACK', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'av-sync-fixture-')), path = join(dir, 'journal.sqlite');
  let journal;
  try {
    journal = new LocalJournal(path, 'owner/device/epoch');
    await journal.enqueue(batch); await journal.enqueue(batch);
    assert.equal((await stat(`${path}-wal`)).mode & 0o777, 0o600);
    assert.equal((await stat(`${path}-shm`)).mode & 0o777, 0o600);
    await assert.rejects(journal.enqueue({ ...batch, batch_id: 'b2' }), /pending_batch_exists/);
    journal.close(); journal = new LocalJournal(path, 'owner/device/epoch');
    assert.deepEqual(await journal.pending(), batch);
    assert.equal(journal.lastReceipt('s1'), null);
    await assert.rejects(journal.ack('b1', { ...receipt, through: 11 }), /invalid_sync_receipt/);
    assert.deepEqual(await journal.pending(), batch);
    await journal.ack('b1', receipt);
    journal.close(); journal = new LocalJournal(path, 'owner/device/epoch');
    assert.equal(await journal.pending(), null); assert.deepEqual(journal.lastReceipt('s1'), receipt);
    await assert.rejects(journal.enqueue(batch), /local_baseline_conflict/);
    const { upload_id, chunk, complete, ...base } = batch;
    await assert.rejects(journal.enqueue({ ...base, kind: 'delta', from: 9, through: 11 }), /local_cursor_gap/);
    await journal.enqueue({ ...base, batch_id: 'b2', kind: 'delta', from: 10, through: 11 });
    assert.equal((await stat(path)).mode & 0o777, 0o600);
    assert.throws(() => new LocalJournal(path, 'wrong-source'), /source_key_mismatch/);
  } finally { journal?.close(); await rm(dir, { recursive: true, force: true }); }
});

test('journal refuses to modify a different SQLite database', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'av-source-fixture-')), path = join(dir, 'source.sqlite');
  try {
    const source = new DatabaseSync(path); source.exec('CREATE TABLE session_v2 (id TEXT)'); source.close();
    assert.throws(() => new LocalJournal(path, 'device'), /not_a_sync_journal/);
    const sourceRead = new DatabaseSync(path, { readOnly: true });
    assert.deepEqual(sourceRead.prepare("SELECT name FROM sqlite_master WHERE type='table'").all().map(r => r.name), ['session_v2']);
    sourceRead.close();
  } finally { await rm(dir, { recursive: true, force: true }); }
});
