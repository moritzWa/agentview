import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { PGlite } from '@electric-sql/pglite';
import { projectMessage, digest, SyncError } from '../sync/projection.mjs';
import { SyncStore } from '../sync/store.mjs';
import { privateHandler } from '../sync/http.mjs';
import { sendPending } from '../sync/client.mjs';

let db, store, writer, reader;
const token = 'w'.repeat(43), readToken = 'r'.repeat(43);
const state = { title: 'Disposable test session', archived: false, visible_before: null, deleted: false };
const msg = (id, order, text = `Visible ${id}`) => ({ id, order, text, role: 'assistant', created_at: 1, updated_at: 2 });
const batch = (session, overrides = {}) => ({ version: 1, batch_id: crypto.randomUUID(), session_id: session,
  kind: 'baseline', from: 0, through: 10, upload_id: `initial-${session}`, chunk: 0, complete: true,
  session: { ...state }, messages: [msg('m1', 1)], delete_ids: [], ...overrides });
const delta = (session, from, through, overrides = {}) => {
  const b = batch(session, { kind: 'delta', from, through, ...overrides });
  delete b.upload_id; delete b.chunk; delete b.complete;
  return b;
};
before(async () => {
  db = new PGlite();
  await db.exec(await readFile(new URL('../sync/schema.sql', import.meta.url), 'utf8'));
  store = new SyncStore(db);
  for (const [t, audience] of [[token, 'sync'], [readToken, 'read']]) {
    await db.query('INSERT INTO av_credentials VALUES ($1,$2,$3,$4,$5,$6,false)', [await digest(t), 'owner', 'mac', 'epoch1', audience, Date.now() + 60_000]);
  }
  writer = await store.authenticate(token, 'sync'); reader = await store.authenticate(readToken, 'read');
});
after(async () => { await db?.close(); });

test('projection allows only visible text, never tools, reasoning, attachments or native payloads', () => {
  const base = { id: 'm', type: 'assistant', seq: 4, time_created: 1, time_updated: 2 };
  const row = { ...base, data: { native: 'SECRET', content: [
    { type: 'text', text: 'First', native: 'SECRET' },
    { type: 'reasoning', text: 'SECRET' }, { type: 'tool', state: { output: 'SECRET' } },
    { type: 'text', text: 'Second' },
  ] } };
  assert.deepEqual(projectMessage(row), { id: 'm', role: 'assistant', order: 4, created_at: 1, updated_at: 2, text: 'First\nSecond' });
  assert(!JSON.stringify(projectMessage(row)).includes('SECRET'));
  assert.equal(projectMessage({ ...base, type: 'synthetic' }), null);
  assert.equal(projectMessage({ ...base, data: { content: [{ type: 'tool' }] } }), null);
  assert.equal(projectMessage({ ...base, type: 'user', data: { text: 'Hello', files: ['SECRET'] } }).text, 'Hello');
  assert.throws(() => projectMessage({ ...base, data: {} }), /unsupported_source_assistant/);
  assert.throws(() => projectMessage({ ...base, data: { content: [{ type: 'future-secret', text: 'SECRET' }] } }), /unsupported_source_part/);
  assert.throws(() => projectMessage({ ...base, data: 'bad json' }), /unsupported_source_message/);
});

test('chunked baseline is invisible until committed; duplicate ACK is identical', async () => {
  const first = batch('chunked', { complete: false });
  const receipt = await store.apply(writer, first);
  assert.equal(receipt.committed, false);
  assert.deepEqual(await store.apply(writer, first), receipt);
  await assert.rejects(store.read(reader, 'chunked'), /session_not_found/);
  await assert.rejects(store.apply(writer, { ...first, batch_id: 'gap', chunk: 2 }), /baseline_chunk_conflict/);
  const last = { ...first, batch_id: 'finish', chunk: 1, complete: true, messages: [msg('m2', 2)] };
  assert.equal((await store.apply(writer, last)).through, 10);
  assert.deepEqual((await store.read(reader, 'chunked')).messages.map(m => m.message_id), ['m1', 'm2']);
  assert.deepEqual(await store.apply(writer, first), receipt); // Late ACK retry cannot regress active cursor.
  await assert.rejects(store.apply(writer, batch('chunked')), /baseline_already_committed/);
});

test('edits replace same ID, deletions and cursor advance atomically; stale and conflicting retries fail', async () => {
  const initial = batch('edits'); await store.apply(writer, initial);
  const edit = delta('edits', 10, 15, { messages: [msg('m1', 1, 'Edited'), msg('m2', 2)] });
  await store.apply(writer, edit);
  assert.deepEqual(await store.apply(writer, edit), { batch_id: edit.batch_id, session_id: 'edits', committed: true, through: 15, next_chunk: null });
  await assert.rejects(store.apply(writer, { ...edit, messages: [msg('m1', 1, 'Changed retry')] }), /batch_id_conflict/);
  await assert.rejects(store.apply(writer, delta('edits', 10, 20)), /cursor_gap/);
  await store.apply(writer, delta('edits', 15, 16, { messages: [], delete_ids: ['m2'] }));
  const read = await store.read(reader, 'edits');
  assert.equal(Number(read.cursor), 16); assert.equal(read.messages.length, 1); assert.equal(read.messages[0].text, 'Edited');
});

test('revert excludes undone messages from ALL read/search paths, unrevert exposes them again', async () => {
  await store.apply(writer, batch('revert', { messages: [msg('a', 1), msg('b', 2, 'unique-undone')] }));
  await store.apply(writer, delta('revert', 10, 11, { messages: [], session: { ...state, visible_before: 2 } }));
  assert.equal((await store.read(reader, 'revert')).messages.length, 1);
  assert.equal((await store.read(reader, 'revert', { query: 'unique-undone' })).messages.length, 0);
  await store.apply(writer, delta('revert', 11, 12, { messages: [] }));
  assert.equal((await store.read(reader, 'revert')).messages.length, 2);
});

test('failed DB write rolls back every row, cursor and receipt', async () => {
  await store.apply(writer, batch('atomic'));
  const invalid = delta('atomic', 10, 11, { messages: [msg('m1', 1, 'Must rollback'), msg('m2', 1)] });
  await assert.rejects(store.apply(writer, invalid)); // Unique message-order constraint.
  const result = await store.read(reader, 'atomic');
  assert.equal(Number(result.cursor), 10); assert.equal(result.messages[0].text, 'Visible m1');
  assert.equal((await db.query('SELECT count(*)::int AS n FROM av_receipts WHERE batch_id=$1', [invalid.batch_id])).rows[0].n, 0);
});

test('large delta chunks remain hidden and do not advance the cursor until final commit',async()=>{
  await store.apply(writer,batch('delta-chunks',{messages:[msg('old',1),msg('keep',2)]}));
  const first=delta('delta-chunks',10,20,{upload_id:'delta-upload',chunk:0,complete:false,messages:[msg('new',1)],delete_ids:['old']});
  // delta() intentionally strips chunk fields for simple test cases; add them explicitly.
  Object.assign(first,{upload_id:'delta-upload',chunk:0,complete:false});
  const ack=await store.apply(writer,first);assert.equal(ack.committed,false);
  const interim=await store.read(reader,'delta-chunks');assert.equal(Number(interim.cursor),10);assert.equal(interim.messages[0].message_id,'old');
  const last={...first,batch_id:'delta-last',chunk:1,complete:true,messages:[msg('keep',2,'Updated')],delete_ids:[]};
  await store.apply(writer,last);
  const final=await store.read(reader,'delta-chunks');assert.equal(Number(final.cursor),20);assert.deepEqual(final.messages.map(m=>m.message_id),['new','keep']);assert.equal(final.messages[1].text,'Updated');
  assert.deepEqual(await store.apply(writer,first),ack);
});

test('a source-visible paste larger than 500K remains intact within the HTTPS budget',async()=>{
  const text='visible text '.repeat(57_000)+'DISTINCT_LONG_TAIL';
  assert(text.length>500_000);
  const body=batch('long-text',{messages:[msg('long-message',1,text)]});
  assert(new TextEncoder().encode(JSON.stringify(body)).length<850_000);
  await store.apply(writer,body);
  const result=await store.read(reader,'long-text');
  assert.equal(result.messages[0].text,text);
});

test('owner/device/DB epoch scopes cannot read another source; forks stay separate', async () => {
  await store.apply(writer, batch('fork-parent')); await store.apply(writer, batch('fork-child'));
  for (const patch of [{ owner_id: 'stranger' }, { device_id: 'other' }, { database_epoch: 'old' }]) {
    await assert.rejects(store.read({ ...reader, ...patch }, 'fork-parent'), /session_not_found/);
    await assert.rejects(store.apply({ ...writer, ...patch }, batch('new')), /unauthorized/);
  }
  assert.equal((await store.read(reader, 'fork-child')).messages.length, 1);
  await store.apply(writer, delta('fork-parent', 10, 11, { session: { ...state, deleted: true }, messages: [] }));
  await assert.rejects(store.read(reader, 'fork-parent'), /session_not_found/);
  assert.equal((await store.read(reader, 'fork-child')).messages.length, 1);
});

test('wrong audience, missing, malformed, expired and revoked credentials fail closed', async () => {
  for (const t of ['', 'short', 'x'.repeat(43)]) await assert.rejects(store.authenticate(t, 'sync'), /unauthorized/);
  await assert.rejects(store.authenticate(readToken, 'sync'), /unauthorized/);
  await assert.rejects(store.authenticate(token, 'sync', Date.now() + 120_000), /unauthorized/);
  await assert.rejects(store.read(writer, 'edits'), /forbidden/);
  await db.query('UPDATE av_credentials SET revoked=true');
  await assert.rejects(store.apply(writer, batch('revoked')), /unauthorized/);
  await assert.rejects(store.read(reader, 'edits'), /session_not_found/);
  await assert.rejects(store.authenticate(token, 'sync'), /unauthorized/);
  await db.query('UPDATE av_credentials SET revoked=false');
});

test('HTTP rejects raw JSON extras, cross-owner overrides, oversized bodies and SQL patterns safely', async () => {
  const handler = privateHandler(store);
  const send = (body, t = token) => handler(new Request('https://private.invalid/v1/sync/batches', { method: 'POST', headers: { authorization: `Bearer ${t}`, 'content-type': 'application/json' }, body: JSON.stringify(body) }));
  assert.equal((await send(batch('http'), '')).status, 401);
  assert.equal((await send(batch('http'), readToken)).status, 401);
  const extra = await send({ ...batch('http'), owner_id: 'other', secret: 'CANARY_SECRET' });
  assert.equal(extra.status, 400); assert(!(await extra.text()).includes('CANARY_SECRET'));
  assert.equal((await send({ ...batch('huge'), padding: 'x'.repeat(1_000_000) })).status, 413);
  assert.equal((await send(batch('http'))).status, 200);
  const read = await handler(new Request('https://private.invalid/v1/sessions/http/messages?q=%25', { headers: { authorization: `Bearer ${readToken}` } }));
  assert.equal(read.status, 200); assert.equal(read.headers.get('cache-control'), 'no-store');
  assert.equal((await read.json()).messages.length, 0); // % is literal, not a wildcard.
  assert.equal((await privateHandler(null)(new Request('https://x/v1/sync/batches', { method: 'POST' }))).status, 503);
});

test('lost server ACK and failed local checkpoint replay the SAME batch without duplicating history', async () => {
  const pending = batch('lost-ack');
  let saved = pending, bookmark = null;
  const journal = { pending: async () => saved, ack: async (id, receipt) => { assert.equal(id, pending.batch_id); bookmark = receipt.through; saved = null; } };
  const handler = privateHandler(store);
  const options = { journal, endpoint: 'https://private.invalid', token };
  await assert.rejects(sendPending({ ...options, fetch: async (url, init) => {
    await handler(new Request(url, init)); throw new Error('network lost after commit');
  } }), /network lost/);
  assert.equal(bookmark, null); assert.equal(saved.batch_id, pending.batch_id);
  const fetch = (url, init) => handler(new Request(url, init));
  await assert.rejects(sendPending({ ...options, fetch, journal: { ...journal, ack: async () => { throw new Error('disk write failed'); } } }), /disk write failed/);
  assert.equal(bookmark, null); assert(saved);
  await sendPending({ ...options, fetch });
  assert.equal(bookmark, 10); assert.equal(saved, null);
  assert.equal((await store.read(reader, 'lost-ack')).messages.length, 1);
  let requests = 0;
  assert.equal(await sendPending({ ...options, fetch: async () => { requests++; } }), null);
  assert.equal(requests, 0); // Idle history sync does not keep Neon awake.
});

test('invalid ACK, redirects and cleartext endpoints never advance the local bookmark', async () => {
  let acks = 0;
  const journal = { pending: async () => batch('bad-ack'), ack: async () => { acks++; } };
  await assert.rejects(sendPending({ journal, endpoint: 'http://bad.invalid', token }), /https_endpoint_required/);
  await assert.rejects(sendPending({ journal, endpoint: 'https://x.invalid', token,
    fetch: async (_url, init) => { assert.equal(init.redirect, 'error'); return Response.json({ through: 999 }); },
  }), /invalid_sync_receipt/);
  assert.equal(acks, 0);
});

test('a rebuilt batch after an unseen commit fails cursor_gap, so unknown-outcome batches must be retried verbatim', async () => {
  const session = 'unseen-commit';
  const handler = privateHandler(store);
  const post = body => handler(new Request('https://private.invalid/v1/sync/batches', { method: 'POST', headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' }, body: JSON.stringify(body) }));
  assert.equal((await post(batch(session))).status, 200);
  const committed = delta(session, 10, 20, { messages: [msg('m2', 2)] });
  const first = await post(committed); // The cloud commits; pretend the daemon never saw the answer.
  assert.equal(first.status, 200);
  const receipt = await first.json();
  // Rebuilding from the old checkpoint is what a discard-and-restage would do.
  const rebuilt = delta(session, 10, 20, { messages: [msg('m2', 2)] });
  const refused = await post(rebuilt);
  assert.equal(refused.status, 409);
  assert.equal((await refused.json()).error, 'cursor_gap');
  // Retrying the original bytes is safe and returns the stored receipt.
  const replay = await post(committed);
  assert.equal(replay.status, 200);
  assert.deepEqual(await replay.json(), receipt);
  assert.equal((await store.read(reader, session)).messages.length, 2);
});
