import {test, before, after} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {PGlite} from '@electric-sql/pglite';
import {privateMcp} from '../private/mcp.mjs';
import {SyncStore} from '../sync/store.mjs';
import {Outbox} from '../private/outbox.mjs';
import {Starts, directoryShape, startView, START_RULE} from '../private/starts.mjs';
import {digest} from '../sync/projection.mjs';

let db, store, starts, reader, writer;
const gate = {activity:async()=>({mac_seen_recently:true, source_checked_at:Date.now()}), runtime:async()=>({available:true, partial:false, checked_at:Date.now(), statuses:[]}), signal:async()=>{}, signalReceipt:async()=>{}, waitReceipt:async()=>{}};
before(async()=>{
  db = new PGlite();
  await db.exec(await readFile(new URL('../sync/schema.sql', import.meta.url), 'utf8'));
  await db.exec(await readFile(new URL('../private/schema.sql', import.meta.url), 'utf8'));
  store = new SyncStore(db);
  starts = new Starts(db, gate);
  for (const [token, audience, scopes] of [['w'.repeat(43), 'sync', 'history:sync messages:deliver'], ['r'.repeat(43), 'read', 'history:read messages:send']]) {
    await db.query('INSERT INTO av_credentials(token_hash,owner_id,device_id,database_epoch,audience,expires_at,scopes) VALUES($1,$2,$3,$4,$5,$6,$7)', [await digest(token), 'test-owner', 'test-device', 'test-epoch', audience, Date.now()+3600_000, scopes]);
  }
  writer = await store.authenticate('w'.repeat(43), 'sync');
  reader = await store.authenticate('r'.repeat(43), 'read');
});
after(async()=>db?.close());

async function rpc(name, args) {
  const response = await privateMcp(new Request('https://test.invalid/mcp', {method:'POST', headers:{'Content-Type':'application/json', Accept:'application/json, text/event-stream'}, body:JSON.stringify({jsonrpc:'2.0', id:1, method:'tools/call', params:{name, arguments:args}})}), {db, store, principal:reader, outbox:new Outbox(db, gate), gate, starts});
  return response.json();
}
const body = raw => JSON.parse(raw.result.content[0].text);

test('a queued start has no session id and cannot be described as started', () => {
  const view = startView({status:'queued', session_id:null}, {mac_seen_recently:true});
  assert.equal(view.delivery_state, 'queued');
  assert.equal(view.claim_ceiling, 'none');
  assert.equal(view.session_id, null);
  assert.equal(view.started, false);
  assert.match(view.say_to_owner, /No session has been created/);
  assert.equal(view.report_rule, START_RULE);
  assert.equal(startView({status:'started', session_id:'ses_real'}).session_id, 'ses_real');
  assert.equal(startView({status:'started', session_id:null}).delivery_state, 'uncertain');
  assert.equal(startView({status:'created_not_started', session_id:'ses_real'}).claim_ceiling, 'created');
  assert.equal(startView({status:'created_not_started', session_id:'ses_real'}).started, false);
});

test('cloud path checks reject traversal and system roots before a row exists', () => {
  assert.equal(directoryShape('/Users/m/Code/agentview/'), '/Users/m/Code/agentview');
  for (const path of ['relative/dir', '/Users/m/../etc', '/etc', '/etc/passwd', '/', '/System/Library', '/tmp/work', 'C:\\Users\\m']) {
    assert.throws(() => directoryShape(path), /invalid_directory|directory_not_allowed/, path);
  }
});

test('queueing a start does not invent a session, and the id appears only after the Mac ack', async () => {
  const queued = body(await rpc('start_session', {operation_id:'start_queue_1', directory:'/Users/m/Code/agentview', text:'Add a health tool', confirmed:true}));
  assert.equal(queued.status, 'queued');
  assert.equal(queued.session_id, null);
  assert.equal(queued.started, false);
  assert.match(queued.say_to_owner, /No session has been created/);
  const again = body(await rpc('start_session', {operation_id:'start_queue_1', directory:'/Users/m/Code/agentview', text:'Add a health tool', confirmed:true}));
  assert.equal(again.operation_id, queued.operation_id);
  const conflict = await rpc('start_session', {operation_id:'start_queue_1', directory:'/Users/m/Code/other', text:'Add a health tool', confirmed:true});
  assert.equal(conflict.result.isError, true);
  assert.match(conflict.result.content[0].text, /operation_id_conflict/);
  const leased = await starts.lease(writer);
  assert.equal(leased.operation_id, 'start_queue_1');
  assert.equal(leased.text, 'Add a health tool');
  await starts.ack(writer, {operation_id:'start_queue_1', status:'started', session_id:'ses_created_1'});
  const done = body(await rpc('start_status', {operation_id:'start_queue_1'}));
  assert.equal(done.delivery_state, 'started');
  assert.equal(done.session_id, 'ses_created_1');
  assert.equal(done.claim_ceiling, 'started');
  assert.match(done.say_to_owner, /does not mean the work is finished/);
});

test('a rejected path and a missing approval do not create a start', async () => {
  const before = (await db.query('SELECT count(*)::int AS n FROM av_starts')).rows[0].n;
  const bad = await rpc('start_session', {operation_id:'start_bad_path', directory:'/etc', text:'nope', confirmed:true});
  assert.equal(bad.result.isError, true);
  assert.match(bad.result.content[0].text, /directory_not_allowed/);
  const unconfirmed = await rpc('start_session', {operation_id:'start_no_confirm', directory:'/Users/m/Code/agentview', text:'nope', confirmed:false});
  assert(unconfirmed.error || unconfirmed.result?.isError);
  assert.equal((await db.query('SELECT count(*)::int AS n FROM av_starts')).rows[0].n, before);
});

test('a queued start can be cancelled, and a created one cannot', async () => {
  const queued = await starts.enqueue(reader, {operation_id:'start_cancel_1', directory:'/Users/m/Code/agentview', text:'cancel me'});
  assert.equal(queued.session_id, null);
  const cancelled = body(await rpc('cancel_start', {operation_id:'start_cancel_1', confirmed:true}));
  assert.equal(cancelled.status, 'cancelled');
  assert.equal(cancelled.session_id, null);
  assert.match(cancelled.say_to_owner, /No session was created/);
  await starts.enqueue(reader, {operation_id:'start_kept_1', directory:'/Users/m/Code/agentview', text:'keep'});
  await starts.lease(writer);
  await starts.ack(writer, {operation_id:'start_kept_1', status:'created_not_started', session_id:'ses_empty'});
  const late = await rpc('cancel_start', {operation_id:'start_kept_1', confirmed:true});
  assert.equal(late.result.isError, true);
  const kept = await starts.status(reader, 'start_kept_1');
  assert.equal(kept.session_id, 'ses_empty');
  assert.equal(kept.started, false);
});
