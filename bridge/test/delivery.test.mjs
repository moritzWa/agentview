import {test,before,after} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {PGlite} from '@electric-sql/pglite';
import {privateMcp} from '../private/mcp.mjs';
import {SyncStore} from '../sync/store.mjs';
import {Outbox} from '../private/outbox.mjs';
import {digest} from '../sync/projection.mjs';
import {deliveryView,errorView,REPORT_RULE} from '../private/delivery.mjs';

const ORDER={none:0,queued:1,accepted:2,completed:3};
const STATES=['not_sent','queued_mac_offline','queued_waiting_for_mac','accepted_by_session','completed_confirmed','needs_confirmation','rejected_stale_context','cancelled','failed','expired','uncertain'];

test('every receipt status maps to one state, one honest line and a claim ceiling',()=>{
  const cases=[
    [{status:'queued'},{mac_seen_recently:false},false,'queued_mac_offline','queued'],
    [{status:'leased'},{mac_seen_recently:false},false,'queued_mac_offline','queued'],
    [{status:'queued'},{mac_seen_recently:true},false,'queued_waiting_for_mac','queued'],
    [{status:'queued'},{},false,'queued_waiting_for_mac','queued'],
    [{status:'accepted'},{mac_seen_recently:true},false,'accepted_by_session','accepted'],
    [{status:'accepted'},{mac_seen_recently:true},true,'completed_confirmed','completed'],
    [{status:'needs_confirmation'},{},false,'needs_confirmation','none'],
    [{status:'cancelled'},{},false,'cancelled','none'],
    [{status:'failed'},{},false,'failed','none'],
    [{status:'expired'},{},false,'expired','none'],
    [{status:'uncertain'},{},false,'uncertain','none'],
    [{status:'something_new'},{},false,'uncertain','none'],
  ];
  for(const [receipt,freshness,done,state,ceiling] of cases) {
    const v=deliveryView(receipt,freshness,done);
    assert.equal(v.delivery_state,state,JSON.stringify(receipt));
    assert.equal(v.claim_ceiling,ceiling);
    assert(STATES.includes(v.delivery_state));
    assert.equal(typeof v.say_to_owner,'string');assert(v.say_to_owner.length>10);
    assert.equal(v.report_rule,REPORT_RULE);
  }
});

test('queued and accepted lines never claim delivery or completion',()=>{
  const queued=deliveryView({status:'queued'},{mac_seen_recently:false}).say_to_owner;
  assert(/not delivered/i.test(queued));assert(!/\b(delivered to|sent to|done|finished)\b/i.test(queued.replace(/not delivered/i,'')));
  const accepted=deliveryView({status:'accepted'},{}).say_to_owner;
  assert(/not finished/i.test(accepted));
  assert.equal(deliveryView({status:'accepted'},{}).claim_ceiling,'accepted');
  assert(ORDER[deliveryView({status:'queued'},{}).claim_ceiling]<ORDER[deliveryView({status:'accepted'},{}).claim_ceiling]);
});

test('a queued message behind an unresolved earlier one asks for confirmation, never reports delivery',()=>{
  for(const status of ['needs_confirmation','uncertain']) {
    const v=deliveryView({status:'queued',blocked_by:{operation_id:'earlier_op_1',status}},{mac_seen_recently:true});
    assert.equal(v.delivery_state,'needs_confirmation');assert.equal(v.claim_ceiling,'none');assert.equal(v.owner_action,'reconfirm');
  }
});

test('every known error code has a structured body that says nothing was sent, or that the outcome is unknown',()=>{
  const codes=['context_changed_re_read_and_confirm','history_changed_re_read_and_confirm','device_disconnected','session_not_found','write_permission_required','operation_id_conflict','invalid_message','invalid_read_context','unauthorized','message_not_found','cannot_cancel_admitted_or_inflight_message','receipt_conflict','operation_failed','totally_unknown_code'];
  for(const code of codes) {
    const e=errorView(code);
    assert.equal(e.error,code);assert(STATES.includes(e.delivery_state),code);
    assert.equal(e.claim_ceiling,'none');assert([false,null].includes(e.sent),code);
    assert(e.say_to_owner.length>10&&e.next_step.length>10);
  }
  assert.equal(errorView('context_changed_re_read_and_confirm').delivery_state,'rejected_stale_context');
  assert.equal(errorView('context_changed_re_read_and_confirm').retry_same_operation,false);
  assert.equal(errorView('operation_failed').delivery_state,'uncertain');
  assert.equal(errorView('operation_failed').sent,null);
  assert.equal(errorView('device_disconnected').sent,false);
});

test('a stale-context error tells the model to ask the owner again before any new send',()=>{
  const e=errorView('history_changed_re_read_and_confirm');
  assert(/ask/i.test(e.next_step)&&/NEW operation_id/.test(e.next_step)&&/clear yes/i.test(e.next_step));
});

let db,store,outbox,reader,writer,macSeen=true;
const gate={activity:async()=>({source_checked_at:Date.now(),mac_seen_recently:macSeen}),runtime:async()=>({available:true,partial:false,checked_at:0,statuses:[]}),signal:async()=>{},signalReceipt:async()=>{},waitReceipt:async()=>{}};
before(async()=>{
  db=new PGlite();await db.exec(await readFile(new URL('../sync/schema.sql',import.meta.url),'utf8'));await db.exec(await readFile(new URL('../private/schema.sql',import.meta.url),'utf8'));
  store=new SyncStore(db);outbox=new Outbox(db,gate);
  for(const [token,audience,scopes] of [['w'.repeat(43),'sync','history:sync messages:deliver'],['r'.repeat(43),'read','history:read messages:send']])await db.query('INSERT INTO av_credentials(token_hash,owner_id,device_id,database_epoch,audience,expires_at,scopes) VALUES($1,$2,$3,$4,$5,$6,$7)',[await digest(token),'test-owner','test-device','test-epoch',audience,Date.now()+3600_000,scopes]);
  writer=await store.authenticate('w'.repeat(43),'sync');reader=await store.authenticate('r'.repeat(43),'read');
  await store.apply(writer,{version:1,batch_id:'seed',session_id:'ses_delivery',kind:'baseline',from:0,through:100,upload_id:'seed',chunk:0,complete:true,session:{title:'Delivery',archived:false,deleted:false,visible_before:null},messages:[{id:'msg_1',role:'user',order:1,created_at:1,updated_at:1,text:'hello'}],delete_ids:[]});
});
after(async()=>db?.close());
async function rpcRaw(name,args){
  const response=await privateMcp(new Request('https://test.invalid/mcp',{method:'POST',headers:{'Content-Type':'application/json',Accept:'application/json, text/event-stream'},body:JSON.stringify({jsonrpc:'2.0',id:1,method:'tools/call',params:{name,arguments:args}})}),{db,store,principal:reader,outbox,gate});
  return response.json();
}
const body=raw=>JSON.parse(raw.result.content[0].text);
const input=id=>({operation_id:id,session_id:'ses_delivery',text:'Synthetic approved text',base_cursor:100,confirmed:true});

test('send with the Mac offline reports queued, not delivered, and claims at most queued',async()=>{
  macSeen=false;
  const raw=await rpcRaw('send_session_message',input('delivery_offline_1'));
  assert(!raw.result.isError);const out=body(raw);
  assert.equal(out.delivery_state,'queued_mac_offline');assert.equal(out.claim_ceiling,'queued');assert.equal(out.status,'queued');
  assert(/not delivered/i.test(out.say_to_owner));
  macSeen=true;
});

test('after the Mac accepts it, status says the session has it but is not finished',async()=>{
  const sent=await outbox.send(reader,{operation_id:'delivery_accepted_1',session_id:'ses_delivery',text:'Another approved text',base_cursor:100});
  for(const [operation_id,message_id] of [['delivery_offline_1','msg_agentvoice_delivery_offline_1'],[sent.operation_id,sent.message_id]]) {
    assert.equal((await outbox.lease(writer)).operation_id,operation_id);
    await outbox.ack(writer,{operation_id,message_id,status:'accepted'});
  }
  const out=body(await rpcRaw('message_status',{operation_id:'delivery_accepted_1'}));
  assert.equal(out.delivery_state,'accepted_by_session');assert.equal(out.claim_ceiling,'accepted');assert.equal(out.completion_confirmed,false);
});

test('a stale cursor is still rejected, now with a structured body that says nothing was sent',async()=>{
  const before=(await db.query('SELECT count(*)::int AS n FROM av_outbox')).rows[0].n;
  const raw=await rpcRaw('send_session_message',{...input('delivery_stale_1'),base_cursor:99});
  assert(raw.result.isError);const out=body(raw);
  assert.equal(out.error,'context_changed_re_read_and_confirm');assert.equal(out.delivery_state,'rejected_stale_context');
  assert.equal(out.sent,false);assert.equal(out.retry_same_operation,false);
  assert.equal((await db.query('SELECT count(*)::int AS n FROM av_outbox')).rows[0].n,before,'a rejected send must not create a row');
});

test('safety checks are unchanged: confirmed must be literal true, same id with other text conflicts, unknown sessions fail',async()=>{
  const noConfirm=await rpcRaw('send_session_message',{...input('delivery_noconfirm_1'),confirmed:false});
  assert(noConfirm.error||noConfirm.result?.isError);
  assert.equal((await db.query("SELECT count(*)::int AS n FROM av_outbox WHERE operation_id='delivery_noconfirm_1'")).rows[0].n,0);
  const conflict=await rpcRaw('send_session_message',{...input('delivery_accepted_1'),text:'Different text'});
  assert(conflict.result.isError);assert.equal(body(conflict).error,'operation_id_conflict');assert.equal(body(conflict).delivery_state,'not_sent');
  const missing=await rpcRaw('send_session_message',{...input('delivery_missing_1'),session_id:'ses_nope'});
  assert(missing.result.isError);assert.equal(body(missing).error,'session_not_found');
});

test('status of an operation that was never sent says so, which is how a skipped send is caught',async()=>{
  const raw=await rpcRaw('message_status',{operation_id:'never_sent_operation'});
  assert(raw.result.isError);const out=body(raw);
  assert.equal(out.error,'message_not_found');assert.equal(out.delivery_state,'not_sent');assert.equal(out.sent,false);
  assert(/do not claim a send/i.test(out.next_step));
});

test('cancel reports cancelled and the session never got it',async()=>{
  await outbox.send(reader,{operation_id:'delivery_cancel_1',session_id:'ses_delivery',text:'Cancel me',base_cursor:100});
  const raw=await rpcRaw('cancel_message',{operation_id:'delivery_cancel_1',confirmed:true});
  assert(!raw.result.isError,raw.result.content?.[0]?.text);const out=body(raw);
  assert.equal(out.delivery_state,'cancelled');assert.equal(out.claim_ceiling,'none');
});

test('read tools keep their bare error codes',async()=>{
  const raw=await rpcRaw('read_message',{session_id:'ses_delivery',message_id:'nope'});
  assert(raw.result.isError);assert.equal(raw.result.content[0].text,'message_not_found');
});
