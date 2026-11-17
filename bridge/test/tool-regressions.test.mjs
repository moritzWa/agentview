import {test,before,after} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {PGlite} from '@electric-sql/pglite';
import {privateMcp} from '../private/mcp.mjs';
import {SyncStore} from '../sync/store.mjs';
import {Outbox} from '../private/outbox.mjs';
import {digest} from '../sync/projection.mjs';
import {WakeGate,gateClient} from '../private/gate.mjs';
import {application} from '../private/worker.mjs';

let db,store,outbox,reader,writer;const activity=[];let onActivity;
let runtime={available:true,partial:false,checked_at:0,statuses:[]};
const gate={activity:async(source,wait,kind)=>{activity.push({source,wait,kind});if(onActivity)await onActivity();return {source_checked_at:Date.now(),mac_seen_recently:true};},runtime:async()=>runtime,signal:async()=>{},signalReceipt:async()=>{}};
before(async()=>{
 db=new PGlite();await db.exec(await readFile(new URL('../sync/schema.sql',import.meta.url),'utf8'));await db.exec(await readFile(new URL('../private/schema.sql',import.meta.url),'utf8'));
 store=new SyncStore(db);outbox=new Outbox(db,gate);
 for(const [token,audience,scopes] of [['w'.repeat(43),'sync','history:sync messages:deliver'],['r'.repeat(43),'read','history:read messages:send']])await db.query('INSERT INTO av_credentials(token_hash,owner_id,device_id,database_epoch,audience,expires_at,scopes) VALUES($1,$2,$3,$4,$5,$6,$7)',[await digest(token),'test-owner','test-device','test-epoch',audience,Date.now()+3600_000,scopes]);
 writer=await store.authenticate('w'.repeat(43),'sync');reader=await store.authenticate('r'.repeat(43),'read');
});
after(async()=>db?.close());
const state={title:'Tool regression',archived:false,deleted:false,visible_before:null};
const message=(order,text)=>({id:`msg_${order}`,role:order===1?'user':'assistant',order,created_at:order,updated_at:order,text});
async function seed(id,messages){await store.apply(writer,{version:1,batch_id:id,session_id:id,kind:'baseline',from:0,through:100,upload_id:id,chunk:0,complete:true,session:state,messages,delete_ids:[]});}
async function rpcRaw(name,args){
 const response=await privateMcp(new Request('https://test.invalid/mcp',{method:'POST',headers:{'Content-Type':'application/json',Accept:'application/json, text/event-stream'},body:JSON.stringify({jsonrpc:'2.0',id:1,method:'tools/call',params:{name,arguments:args}})}),{db,store,principal:reader,outbox,gate});
 return response.json();
}
async function rpc(name,args){const body=await rpcRaw(name,args);assert(!body.result?.isError,body.result?.content?.[0]?.text);return JSON.parse(body.result.content[0].text);}
test('large forward and backward pages never skip a message under the response budget',async()=>{
 await seed('large-pages',Array.from({length:8},(_,i)=>message(i+1,'x'.repeat(25000))));
 const forward=[];let after=0;
 for(let n=0;n<10;n++){
  const page=await rpc('read_session',{session_id:'large-pages',after,limit:5});if(page.no_newer_messages)break;
  forward.push(...page.messages.map(m=>m.message_order));assert(page.next_after>after);after=page.next_after;
 }
 assert.deepEqual(forward,[1,2,3,4,5,6,7,8]);
 const backward=[];let before;
 for(let n=0;n<10;n++){
  const page=await rpc('read_session',{session_id:'large-pages',...(before===undefined?{}:{before}),limit:5});if(!page.messages.length)break;
  backward.unshift(...page.messages.map(m=>m.message_order));before=page.next_before;
 }
 assert.deepEqual(backward,[1,2,3,4,5,6,7,8]);
 const exhausted=await rpc('read_session',{session_id:'large-pages',after:8});
 assert.deepEqual(exhausted.messages,[]);assert.equal(exhausted.no_newer_messages,true);assert.equal(exhausted.next_after,8);
});
test('truncating a session preview never splits a Unicode surrogate pair',async()=>{
 await seed('unicode-preview',[message(1,'x'.repeat(19999)+'😀TAIL')]);
 const page=await rpc('read_session',{session_id:'unicode-preview'});
 assert.equal(page.messages[0].text,'x'.repeat(19999));assert(page.messages[0].truncated);
});
test('search returns bounded matching text with a Unicode-compatible read_message offset',async()=>{
 const text='😀 '.repeat(13000)+'A quartzneedle appears here. '+ 'z'.repeat(25000);
 await seed('late-search',[message(1,text)]);
 const result=await rpc('search_history',{query:'quartzneedle'}),hit=result.matches.find(m=>m.session_id==='late-search');
 assert(hit);assert(hit.excerpt.includes('quartzneedle'));assert(hit.excerpt_offset>0);assert([...hit.excerpt].length<=2000);
 const full=await rpc('read_message',{session_id:'late-search',message_id:hit.message_id,offset:hit.excerpt_offset});
 assert(full.text.startsWith(hit.excerpt));assert.equal(full.total_characters,[...text].length);
});
test('matching search offsets do not land on an earlier substring inside another word',async()=>{
 const text='anything '+ 'x '.repeat(3000)+'A thing appears with version v1.2 here.';
 await seed('word-boundary-search',[message(1,text)]);
 for(const query of ['thing','v1.2','"thing appears"']){
  const result=await rpc('search_history',{query}),hit=result.matches.find(m=>m.session_id==='word-boundary-search');
  assert(hit);assert(hit.excerpt_offset>1000);assert(hit.excerpt.includes('thing appears'));assert([...hit.excerpt].length<=2000);
 }
});
test('receipt polling refreshes both history and native state even before admission',async()=>{
 await seed('receipt-freshness',[]);
 const input={operation_id:'freshness_operation',session_id:'receipt-freshness',text:'Synthetic input',base_cursor:100};
 const sent=await outbox.send(reader,input);let previous=activity.length;
 const queued=await rpc('message_status',{operation_id:input.operation_id});
 assert.equal(queued.status,'queued');assert.equal(activity.length,previous+1);assert.equal(activity.at(-1).kind,'both');
 await outbox.lease(writer);await outbox.ack(writer,{operation_id:sent.operation_id,message_id:sent.message_id,status:'accepted'});
 previous=activity.length;const accepted=await rpc('message_status',{operation_id:input.operation_id});
 assert.equal(activity.length,previous+1);assert.equal(activity.at(-1).kind,'both');assert.equal(accepted.status_fresh,false);assert.equal(accepted.completion_confirmed,false);
});
test('read_message refreshes mobile activity and reports source freshness',async()=>{
 const previous=activity.length,result=await rpc('read_message',{session_id:'late-search',message_id:'msg_1'});
 assert.equal(activity.length,previous+1);assert.equal(activity.at(-1).wait,true);assert.equal(result.mac_seen_recently,true);assert(result.source_checked_at);assert(result.synced_at);
});
test('combined freshness waits for native runtime as well as history; polling renews the active window',async()=>{
 const values=new Map();const storage={get:async k=>values.get(k),put:async(k,v)=>{if(typeof k==='object')for(const [key,value]of Object.entries(k))values.set(key,value);else values.set(k,v);},transaction:async fn=>fn(storage)};
 const object=new WakeGate({storage}),namespace={idFromName:x=>x,get:()=>({fetch:(url,init)=>object.fetch(new Request(url,init))})};
 const g=gateClient(namespace,'owner'),hash='a'.repeat(64),source='device|epoch';
 await g.enroll({token_hash:hash,device_id:'device',database_epoch:'epoch',expires_at:Date.now()+3600_000});await g.poll(hash);
 values.set(`active_until:${source}`,Date.now()-1);
 let settled=false;const waiting=g.activity(source,true,'both').then(v=>{settled=true;return v;});
 await new Promise(r=>setTimeout(r,10));await g.checked(hash);await new Promise(r=>setTimeout(r,10));
 assert.equal(settled,false,'history alone cannot satisfy a combined status check');
 values.set(`runtime_checked:${source}`,Date.now());await g.checked(hash);await waiting;
 assert(values.get(`active_until:${source}`)>Date.now()+4*60_000);
});

test('the outer authenticated MCP router refreshes activity even without a tool call',async()=>{
 const values=new Map();const storage={get:async k=>values.get(k),put:async(k,v)=>{if(typeof k==='object')for(const [key,value]of Object.entries(k))values.set(key,value);else values.set(k,v);},transaction:async fn=>fn(storage)};
 const object=new WakeGate({storage}),namespace={idFromName:x=>x,get:()=>({fetch:(url,init)=>object.fetch(new Request(url,init))})};
 const app=application({database:async()=>({query:(...args)=>db.query(...args),transaction:fn=>db.transaction(fn),close:async()=>{}})});
 const response=await app.fetch(new Request('https://test.invalid/mcp',{method:'POST',headers:{authorization:`Bearer ${'r'.repeat(43)}`,'Content-Type':'application/json',Accept:'application/json, text/event-stream'},body:JSON.stringify({jsonrpc:'2.0',id:1,method:'tools/list'})}),{ISSUER:'https://test.invalid',GATE:namespace,DATABASE_URL:'synthetic'});
 assert.equal(response.status,200);assert(values.get('active_until:test-device|test-epoch')>Date.now()+4*60_000);
});

test('session-list pagination rejects changed snapshots rather than skipping or restarting silently',async()=>{
 for(const id of ['list-a','list-b','list-c'])await seed(id,[]);
 runtime={available:true,partial:false,checked_at:Date.now(),statuses:['list-a','list-b','list-c'].map((session_id,i)=>({session_id,status:'done',reason:null,updated_at:30-i,can_message:true}))};
 const first=await rpc('list_sessions',{limit:1});assert.equal(first.sessions[0].session_id,'list-a');assert(first.next_after);
 const second=await rpc('list_sessions',{limit:1,after:first.next_after});assert.equal(second.sessions[0].session_id,'list-b');
 runtime.statuses[2].status='working';
 const changed=await rpcRaw('list_sessions',{limit:1,after:first.next_after});assert(changed.result.isError);assert.equal(changed.result.content[0].text,'session_list_changed_restart_pagination');
});

test('expired unadmitted confirmation requests no longer block later instructions forever',async()=>{
 await seed('expiry-confirmation',[]);
 const sent=await outbox.send(reader,{operation_id:'expire_confirm_first',session_id:'expiry-confirmation',text:'Synthetic',base_cursor:100});
 await outbox.lease(writer);await outbox.ack(writer,{operation_id:sent.operation_id,message_id:sent.message_id,status:'needs_confirmation'});
 await db.query('UPDATE av_outbox SET expires_at=0 WHERE operation_id=$1',[sent.operation_id]);
 const next=await outbox.send(reader,{operation_id:'expire_confirm_next',session_id:'expiry-confirmation',text:'Synthetic successor',base_cursor:100});
 assert.equal((await outbox.lease(writer)).operation_id,next.operation_id);assert.equal((await outbox.status(reader,sent.operation_id)).status,'expired');
});

test('revocation during a read wait prevents returning conversation text',async()=>{
 onActivity=()=>db.query('UPDATE av_credentials SET revoked=true WHERE token_hash=$1',[reader.token_hash]);
 try {
  const result=await rpcRaw('read_message',{session_id:'late-search',message_id:'msg_1'});
  assert(result.result.isError);assert.equal(result.result.content[0].text,'unauthorized');
 }finally{onActivity=undefined;await db.query('UPDATE av_credentials SET revoked=false WHERE token_hash=$1',[reader.token_hash]);}
});

test('cached principals cannot mutate the queue or lease text after revocation',async()=>{
 await seed('revoked-write',[]);
 try{
  await db.query('UPDATE av_credentials SET revoked=true WHERE token_hash=$1',[reader.token_hash]);
  await assert.rejects(outbox.send(reader,{operation_id:'revoked_operation',session_id:'revoked-write',text:'Must not enqueue',base_cursor:100}),/unauthorized/);
  await assert.rejects(outbox.cancel(reader,'expire_confirm_next'),/unauthorized/);
  await db.query('UPDATE av_credentials SET revoked=true WHERE token_hash=$1',[writer.token_hash]);
  await assert.rejects(outbox.lease(writer),/unauthorized/);
  assert.equal((await db.query("SELECT count(*)::int AS n FROM av_outbox WHERE session_id='revoked-write'")).rows[0].n,0);
 }finally{await db.query('UPDATE av_credentials SET revoked=false');}
});

test('one active overview finds actionable sessions beyond hundreds of done rows and includes their decision context',async()=>{
 const now=Date.now();
 await db.query(`INSERT INTO av_sessions(owner_id,device_id,database_epoch,session_id,generation,cursor,state,synced_at)
  SELECT 'test-owner','test-device','test-epoch','overview-done-'||n,'overview',1,
   jsonb_build_object('title','Completed fixture '||n,'archived',false,'deleted',false,'visible_before',null),now() FROM generate_series(1,398) n`);
 for(const id of ['overview-working-a','overview-working-b','overview-decision'])await seed(id,[message(1,'Implement the mobile overview, keeping the existing design.'),message(2,'Already checked the options. '.repeat(140)+'Should I use option A or option B?')]);
 runtime={available:true,partial:false,checked_at:now,statuses:[
  ...Array.from({length:398},(_,i)=>({session_id:'overview-done-'+(i+1),status:'done',reason:null,updated_at:now-i,attention_assessed:true,last_user_ms:now-i,top_level:true})),
  ...['overview-working-a','overview-working-b'].map(session_id=>({session_id,status:'working',reason:null,updated_at:now,last_user_ms:now-1000,top_level:true,last_user_message_id:'msg_1'})),
  {session_id:'overview-decision',status:'needs_you',reason:'reply',updated_at:now,last_user_ms:now-3600_000,top_level:true,attention_assessed:true,last_user_message_id:'msg_1'},
 ]};
 const page=await rpc('list_sessions',{status:'active'});
 assert.equal(page.counts.total,401);assert.equal(page.counts.working,2);assert.equal(page.counts.needs_you,1);assert.equal(page.counts.done,398);
 assert.equal(page.sessions.length,3);assert.equal(page.next_after,null);assert.equal(page.status_fresh,true);assert.equal(page.context_fresh,true);
 const decision=page.sessions[0];assert.equal(decision.session_id,'overview-decision');assert.equal(decision.attention,'needs_you');
 assert(decision.last_user_message.text.includes('existing design'));assert(decision.last_assistant_message.text.endsWith('option A or option B?'));assert(decision.last_assistant_message.truncated);
 assert(page.note.includes('Counts cover the entire'));assert(page.note.includes('not that the task is finished'));
 const recent=await rpc('list_sessions',{});assert(recent.sessions.some(s=>s.session_id==='overview-decision'));assert(recent.sessions.length<=14);assert.equal(recent.counts.total,401);
 assert.equal(recent.total_matching,recent.sessions.length);assert.equal(recent.remaining_in_selection,0);assert.equal(recent.next_after,null);assert.equal(recent.outside_selection.needs_you,0);
});

test('recent unclassified final questions are provided for review, not equated with no attention',async()=>{
 await seed('overview-unclassified',[message(1,'Review this choice.'),message(2,'Which approach do you prefer?')]);
 runtime={available:true,partial:false,checked_at:Date.now(),statuses:[{session_id:'overview-unclassified',status:'done',reason:null,updated_at:Date.now(),last_user_ms:Date.now(),last_user_message_id:'msg_1',attention_assessed:false}]};
 const result=await rpc('list_sessions',{status:'active'});
 assert.equal(result.counts.unassessed_idle,1);assert.equal(result.sessions.length,1);assert.equal(result.sessions[0].attention,'review_last_reply');
 assert.equal(result.sessions[0].last_assistant_message.text,'Which approach do you prefer?');
});

test('overview context excludes reverted/staged text and child sessions, and flags a mirror behind native input',async()=>{
 await seed('overview-visible',[message(1,'Visible request'),message(2,'Visible answer'),message(3,'REVERTED_PRIVATE_CANARY')]);
 await db.query("UPDATE av_sessions SET state=jsonb_set(state,'{visible_before}','3') WHERE session_id='overview-visible'");
 await db.query("INSERT INTO av_messages VALUES('test-owner','test-device','test-epoch','overview-visible','uncommitted','staged_msg',4,'assistant','STAGED_PRIVATE_CANARY',4,4)");
 runtime={available:true,partial:false,checked_at:Date.now(),statuses:[
  {session_id:'overview-visible',status:'working',reason:null,updated_at:Date.now(),last_user_message_id:'new_native_input',top_level:true},
  {session_id:'overview-working-a',status:'working',reason:null,updated_at:Date.now(),top_level:false},
 ]};
 const result=await rpc('list_sessions',{view:'all'});
 assert.equal(result.counts.total,1);assert.equal(result.sessions.length,1);assert.equal(result.context_fresh,false);
 assert.equal(result.sessions[0].last_user_message.text,'Visible request');assert.equal(result.sessions[0].last_assistant_message.text,'Visible answer');
 assert(!JSON.stringify(result).includes('PRIVATE_CANARY'));
 runtime.statuses.push({session_id:'overview-not-mirrored-yet',status:'working',reason:null,updated_at:Date.now(),top_level:true,last_user_message_id:'new_input'});
 const pending=await rpc('list_sessions',{status:'active'}),fresh=pending.sessions.find(s=>s.session_id==='overview-not-mirrored-yet');
 assert.equal(pending.counts.working,2);assert(fresh);assert.equal(fresh.mirrored,false);assert.equal(fresh.context_fresh,false);assert.equal(fresh.last_assistant_message,null);
 runtime.truncated=true;
 const truncated=await rpc('list_sessions',{});assert.equal(truncated.counts_complete,false);assert.equal(truncated.active_counts_complete,true);
});
