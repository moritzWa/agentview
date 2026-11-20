import { test,before,after } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { PGlite } from '@electric-sql/pglite';
import { generateKeyPair,SignJWT } from 'jose';
import { OwnerAuth,OWNER,challenge,verifyGoogleIdToken } from '../private/auth.mjs';
import { WakeGate,gateClient } from '../private/gate.mjs';
import { application } from '../private/worker.mjs';
import { Outbox } from '../private/outbox.mjs';
import { readContext } from '../private/context.mjs';
import { SyncStore } from '../sync/store.mjs';
import { digest } from '../sync/projection.mjs';

let db,auth,gate,env,namespace;
const writerToken='s'.repeat(43),readerToken='r'.repeat(43);
const state={title:'Synthetic penguin project',archived:false,deleted:false,visible_before:null};
function storage() {
  const map=new Map();
  const s={writes:0,get:async k=>Array.isArray(k)?new Map(k.filter(x=>map.has(x)).map(x=>[x,structuredClone(map.get(x))])):structuredClone(map.get(k)),put:async(k,v)=>{if(typeof k==='object')for(const [a,b] of Object.entries(k)){map.set(a,structuredClone(b));s.writes++;}else{map.set(k,structuredClone(v));s.writes++;}},delete:async k=>map.delete(k),transaction:async fn=>fn(s)};
  return s;
}
before(async()=>{
  db=new PGlite();
  await db.exec(await readFile(new URL('../sync/schema.sql',import.meta.url),'utf8'));
  await db.exec(await readFile(new URL('../private/schema.sql',import.meta.url),'utf8'));
  const object=new WakeGate({storage:storage()});
  namespace={idFromName:x=>x,get:()=>({fetch:(url,init)=>object.fetch(new Request(url,init))})};
  gate=gateClient(namespace,OWNER);
  env={ISSUER:'https://bridge.invalid',GOOGLE_CLIENT_ID:'test-google-client',GOOGLE_CLIENT_SECRET:'synthetic-secret',GATE:namespace,DATABASE_URL:'synthetic'};
  auth=new OwnerAuth(db,env,{gate,googleFetch:async()=>Response.json({id_token:'fixture'}),verify:async()=> 'google-owner-sub'});
  await db.query('INSERT INTO av_owner_source VALUES ($1,$2,$3)',[OWNER,'device123','epoch123']);
  for(const [token,audience,scopes] of [[writerToken,'sync','history:sync messages:deliver'],[readerToken,'read','history:read messages:send']])await db.query('INSERT INTO av_credentials (token_hash,owner_id,device_id,database_epoch,audience,expires_at,scopes) VALUES ($1,$2,$3,$4,$5,$6,$7)',[await digest(token),OWNER,'device123','epoch123',audience,Date.now()+3600_000,scopes]);
  await gate.enroll({token_hash:await digest(writerToken),owner_id:OWNER,device_id:'device123',database_epoch:'epoch123',expires_at:Date.now()+3600_000});
  const store=new SyncStore(db),writer=await store.authenticate(writerToken,'sync');
  await store.apply(writer,{version:1,batch_id:'baseline',session_id:'ses_fixture',kind:'baseline',from:0,through:10,upload_id:'initial',chunk:0,complete:true,session:state,messages:[{id:'msg_one',role:'user',order:1,created_at:1,updated_at:1,text:'Penguin canary visible text'},{id:'msg_two',role:'assistant',order:2,created_at:2,updated_at:2,text:'An assistant response'}],delete_ids:[]});
});
after(async()=>db?.close());
const req=(path,options={})=>new Request(`https://bridge.invalid${path}`,options);
async function googleFlow(response) {
  const cookie=response.headers.get('set-cookie').split(';')[0];
  const state=new URL(response.headers.get('location')).searchParams.get('state');
  const page=await auth.handle(req(`/auth/google/callback?state=${state}&code=fake-google-code`,{headers:{cookie}}));
  const html=await page.text(),code=html.match(/name="code" value="([^"]+)"/)[1];
  return {cookie,code,page};
}
test('Google token validation pins owner, audience, issuer, nonce and verified email',async()=>{
  const {privateKey,publicKey}=await generateKeyPair('RS256');
  const sign=claims=>new SignJWT({email:OWNER,email_verified:true,nonce:'n',...claims}).setProtectedHeader({alg:'RS256'}).setIssuer('https://accounts.google.com').setAudience(env.GOOGLE_CLIENT_ID).setSubject('google-sub').setIssuedAt().setExpirationTime('5m').sign(privateKey);
  assert.equal(await verifyGoogleIdToken(await sign({}),env,'n',publicKey),'google-sub');
  for(const claims of [{email:'another@example.com'},{email_verified:false},{nonce:'wrong'},{azp:'other-client'}])await assert.rejects(verifyGoogleIdToken(await sign(claims),env,'n',publicKey));
  await assert.rejects(verifyGoogleIdToken(await sign({}),{GOOGLE_CLIENT_ID:'other-client'},'n',publicKey));
});
test('OAuth code is browser-bound, one-use, PKCE/resource checked; refresh replay revokes the family',async()=>{
  await assert.rejects(auth.handle(req('/register',{method:'POST',body:JSON.stringify({redirect_uris:['https://evil.invalid/callback']})})),/invalid_client_metadata/);
  const client=await (await auth.handle(req('/register',{method:'POST',body:JSON.stringify({redirect_uris:['https://bridge.invalid/oauth/test-callback']})}))).json();
  const verifier='v'.repeat(43),url=`/authorize?${new URLSearchParams({client_id:client.client_id,redirect_uri:'https://bridge.invalid/oauth/test-callback',response_type:'code',code_challenge_method:'S256',code_challenge:await challenge(verifier),scope:'history:read messages:send',state:'client-state',resource:'https://bridge.invalid/mcp'})}`;
  const started=await auth.handle(req(url));
  const flowState=new URL(started.headers.get('location')).searchParams.get('state');
  await assert.rejects(auth.handle(req(`/auth/google/callback?state=${flowState}&code=bad`,{headers:{cookie:'av_browser=wrong'}})),/invalid_login_state/);
  const {cookie,code}=await googleFlow(started);
  const rendered=await googleFlow(await auth.handle(req(url)));
  assert.match(rendered.page.headers.get('content-security-policy'),/form-action 'self' https:\/\/chatgpt\.com/);
  const consent=await auth.handle(req('/consent',{method:'POST',headers:{cookie,origin:env.ISSUER},body:new URLSearchParams({code,decision:'approve'})}));
  const callback=new URL(consent.headers.get('location'));assert.equal(callback.searchParams.get('state'),'client-state');
  const data={grant_type:'authorization_code',client_id:client.client_id,redirect_uri:'https://bridge.invalid/oauth/test-callback',code:callback.searchParams.get('code'),code_verifier:verifier,resource:'https://bridge.invalid/mcp'};
  await assert.rejects(auth.token(new Map(Object.entries({...data,code_verifier:'x'.repeat(43)}))),/invalid_grant/);
  await assert.rejects(auth.token(new Map(Object.entries({...data,resource:'https://evil.invalid'}))),/invalid_target/);
  const access=await (await auth.token(new Map(Object.entries(data)))).json();
  await assert.rejects(auth.token(new Map(Object.entries(data))),/invalid_grant/);
  const next=await (await auth.token(new Map(Object.entries({grant_type:'refresh_token',client_id:client.client_id,refresh_token:access.refresh_token})))).json();
  assert.notEqual(next.refresh_token,access.refresh_token);
  await assert.rejects(auth.token(new Map(Object.entries({grant_type:'refresh_token',client_id:client.client_id,refresh_token:access.refresh_token}))),/invalid_grant/);
  await assert.rejects(new SyncStore(db).authenticate(next.access_token,'read'),/unauthorized/);
});
test('pairing requires Google owner consent and is consumed once',async()=>{
  const start=await (await auth.handle(req('/pair/start',{method:'POST',body:JSON.stringify({device_id:'device456',database_epoch:'epoch456',label:'Synthetic Mac'})}))).json();
  await assert.rejects(auth.handle(req('/pair/token',{method:'POST',body:JSON.stringify({device_code:start.device_code})})),/authorization_pending/);
  const consent=await googleFlow(await auth.handle(new Request(start.verification_uri)));
  await assert.rejects(auth.handle(req('/consent',{method:'POST',headers:{cookie:consent.cookie,origin:'https://evil.invalid'},body:new URLSearchParams({code:consent.code,decision:'approve'})})),/invalid_origin/);
  await auth.handle(req('/consent',{method:'POST',headers:{cookie:consent.cookie,origin:env.ISSUER},body:new URLSearchParams({code:consent.code,decision:'approve'})}));
  const result=await (await auth.handle(req('/pair/token',{method:'POST',body:JSON.stringify({device_code:start.device_code})}))).json();
  assert.equal(result.device_id,'device456');assert.equal((await gate.inspect(await digest(result.access_token))).principal.device_id,'device456');
  await assert.rejects(auth.handle(req('/pair/token',{method:'POST',body:JSON.stringify({device_code:start.device_code})})),/pairing_expired/);
});
test('MCP exposes real read/search tools but rejects ungranted sends and bypass attempts',async()=>{
  const app=application({database:async()=>({query:(...args)=>db.query(...args),transaction:fn=>db.transaction(fn),close:async()=>{}})});
  const rpc=async(method,params={})=>{
    await gate.checked(await digest(writerToken));
    const response=await app.fetch(req('/mcp',{method:'POST',headers:{authorization:`Bearer ${readerToken}`,'Content-Type':'application/json',Accept:'application/json, text/event-stream'},body:JSON.stringify({jsonrpc:'2.0',id:1,method,params})}),env);
    assert.equal(response.status,200);return response.json();
  };
  const tools=(await rpc('tools/list')).result.tools;
  assert.equal(tools.find(t=>t.name==='send_session_message').annotations.readOnlyHint,false);
  await gate.poll(await digest(writerToken));
  await gate.setRuntime(await digest(writerToken),{available:true,partial:false,statuses:[{session_id:'ses_fixture',status:'needs_you',reason:'question',updated_at:123,can_message:false}]});
  const waiting=JSON.parse((await rpc('tools/call',{name:'list_sessions',arguments:{status:'needs_you'}})).result.content[0].text);
  assert.equal(waiting.sessions.length,1);assert.equal(waiting.sessions[0].reason,'question');assert.equal(waiting.status_fresh,true);
  const search=await rpc('tools/call',{name:'search_history',arguments:{query:'penguin'}});
  assert.match(search.result.content[0].text,/canary visible/);
  const forbidden=await rpc('tools/call',{name:'send_session_message',arguments:{operation_id:'op-short',session_id:'ses_fixture',text:'Do not run',base_cursor:10,confirmed:false}});
  assert(forbidden.error || forbidden.result?.isError);
  assert.equal((await app.fetch(req('/mcp'),env)).status,401);
  const store=new SyncStore(db),writer=await store.authenticate(writerToken,'sync');
  const pages=Array.from({length:30},(_,i)=>({id:`page_${i}`,role:i===0?'user':'assistant',order:(i+1)*100,created_at:i,updated_at:i,text:`Visible message ${i}`}));
  await store.apply(writer,{version:1,batch_id:'pages-baseline',session_id:'ses_pages',kind:'baseline',from:0,through:5000,upload_id:'pages-gen',chunk:0,complete:true,session:state,messages:pages,delete_ids:[]});
  const latest=JSON.parse((await rpc('tools/call',{name:'read_session',arguments:{session_id:'ses_pages'}})).result.content[0].text);
  assert.equal(latest.messages[0].message_order,2100);assert.equal(latest.messages.at(-1).message_order,3000);
  assert.equal(latest.page_kind,'latest');assert.equal(latest.no_newer_messages,undefined);
  const older=JSON.parse((await rpc('tools/call',{name:'read_session',arguments:{session_id:'ses_pages',before:latest.next_before}})).result.content[0].text);
  assert.equal(older.messages.at(-1).message_order,2000);
  const confused=JSON.parse((await rpc('tools/call',{name:'read_session',arguments:{session_id:'ses_pages',after:4980}})).result.content[0].text);
  assert.equal(confused.no_newer_messages,true);assert.equal(confused.messages.length,0);assert.equal(confused.page_kind,'newer');assert.equal(confused.context.anchor_order,3000);
  const big=Array.from({length:5},(_,i)=>({id:`big_${i}`,role:'assistant',order:i+1,created_at:i,updated_at:i,text:`Message ${i} `+'x'.repeat(25_000)}));
  await store.apply(writer,{version:1,batch_id:'big-tail',session_id:'ses_big_tail',kind:'baseline',from:0,through:20,upload_id:'big-tail-gen',chunk:0,complete:true,session:state,messages:big,delete_ids:[]});
  const bigTail=JSON.parse((await rpc('tools/call',{name:'read_session',arguments:{session_id:'ses_big_tail'}})).result.content[0].text);
  assert.equal(bigTail.messages.at(-1).message_id,'big_4');assert.equal(bigTail.messages[0].message_id,'big_2');assert.equal(bigTail.page_budget_limited,true);
  const unicode='😀'.repeat(25_000)+'TAIL';
  await store.apply(writer,{version:1,batch_id:'unicode-baseline',session_id:'ses_unicode',kind:'baseline',from:0,through:1,upload_id:'unicode-gen',chunk:0,complete:true,session:state,messages:[{id:'msg_unicode',role:'assistant',order:1,created_at:1,updated_at:1,text:unicode}],delete_ids:[]});
  const first=JSON.parse((await rpc('tools/call',{name:'read_message',arguments:{session_id:'ses_unicode',message_id:'msg_unicode'}})).result.content[0].text);
  assert.equal(first.next_offset,20_000);
  const next=JSON.parse((await rpc('tools/call',{name:'read_message',arguments:{session_id:'ses_unicode',message_id:'msg_unicode',offset:first.next_offset}})).result.content[0].text);
  assert.equal(first.text+next.text,unicode);assert.equal(next.next_offset,null);
  await store.apply(writer,{version:1,batch_id:'receipt-baseline',session_id:'ses_receipt',kind:'baseline',from:0,through:10,upload_id:'receipt-gen',chunk:0,complete:true,session:state,messages:[{id:'receipt_old_user',role:'user',order:1,created_at:1,updated_at:1,text:'Earlier turn'},{id:'receipt_old_answer',role:'assistant',order:2,created_at:2,updated_at:2,text:'Earlier completed reply'}],delete_ids:[]});
  const outbox=new Outbox(db,gate),reader=await store.authenticate(readerToken,'read');
  const sent=await outbox.send(reader,{operation_id:'correlated_receipt',session_id:'ses_receipt',text:'A new instruction',base_cursor:10});
  await outbox.lease(writer);await outbox.ack(writer,{operation_id:sent.operation_id,message_id:sent.message_id,status:'accepted'});
  const receiptCall=async()=>JSON.parse((await rpc('tools/call',{name:'message_status',arguments:{operation_id:sent.operation_id}})).result.content[0].text);
  const admitted=await receiptCall();assert.equal(admitted.input_in_history,false);assert.equal(admitted.completion_confirmed,false);
  await store.apply(writer,{version:1,batch_id:'receipt-reply',session_id:'ses_receipt',kind:'delta',from:10,through:20,session:state,messages:[{id:sent.message_id,role:'user',order:3,created_at:3,updated_at:3,text:'A new instruction'},{id:'receipt_new_answer',role:'assistant',order:4,created_at:4,updated_at:4,text:'The actual new reply'}],delete_ids:[]});
  const native={session_id:'ses_receipt',status:'done',reason:null,updated_at:4,can_message:true,last_user_message_id:'receipt_old_user'};
  await gate.setRuntime(await digest(writerToken),{available:true,partial:false,statuses:[native]});
  assert.equal((await receiptCall()).completion_confirmed,false,'an old completed snapshot is not completion of the new input');
  await gate.setRuntime(await digest(writerToken),{available:true,partial:false,statuses:[{...native,last_user_message_id:sent.message_id}]});
  const completed=await receiptCall();assert.equal(completed.reply_available,true);assert.equal(completed.completion_confirmed,true);assert.equal(completed.reply_after,3);
});
test('outbox is idempotent, source-scoped, ordered and checks stale context',async()=>{
  const store=new SyncStore(db),reader=await store.authenticate(readerToken,'read'),writer=await store.authenticate(writerToken,'sync'),outbox=new Outbox(db,gate);
  const input={operation_id:'operation123',session_id:'ses_fixture',text:'Synthetic no-op instruction',base_cursor:10};
  await assert.rejects(outbox.send({...reader,scopes:'history:read'},input),/write_permission_required/);
  const first=await outbox.send(reader,input);assert.deepEqual(await outbox.send(reader,input),first);
  await assert.rejects(outbox.send(reader,{...input,text:'changed'}),/operation_id_conflict/);
  await assert.rejects(outbox.send(reader,{...input,operation_id:'operationbad',base_cursor:9}),/context_changed/);
  await outbox.send(reader,{...input,operation_id:'operation456'});
  const leased=await outbox.lease(writer);assert.equal(leased.operation_id,'operation123');assert(leased.message_id.startsWith('msg_'));
  assert.equal(await outbox.lease(writer),null); // Same-session successor waits for admission.
  await outbox.ack(writer,{operation_id:leased.operation_id,message_id:leased.message_id,status:'accepted'});
  assert.equal((await outbox.lease(writer)).operation_id,'operation456');
  assert.equal((await outbox.status(reader,'operation123')).status,'accepted');
});

test('a queued offline message survives later appended turns but not edits or reverts',async()=>{
  const store=new SyncStore(db),writer=await store.authenticate(writerToken,'sync'),reader=await store.authenticate(readerToken,'read'),outbox=new Outbox(db,gate);
  await store.apply(writer,{version:1,batch_id:'append-initial',session_id:'ses_append',kind:'baseline',from:0,through:10,upload_id:'append-gen',chunk:0,complete:true,session:state,messages:[{id:'msg_anchor',role:'user',order:1,created_at:1,updated_at:1,text:'Read anchor'},{id:'msg_progress',role:'assistant',order:2,created_at:2,updated_at:2,text:'Still working'}],delete_ids:[]});
  const context=await readContext(db,reader,'ses_append');
  const input={operation_id:'append_offline1',session_id:'ses_append',text:'New instruction from the phone',base_cursor:10,context};
  await outbox.send(reader,input);
  await store.apply(writer,{version:1,batch_id:'append-changes',session_id:'ses_append',kind:'delta',from:10,through:20,session:state,messages:[{id:'msg_progress',role:'assistant',order:2,created_at:2,updated_at:3,text:'Still working, now finished'},{id:'msg_other_input',role:'user',order:3,created_at:3,updated_at:3,text:'An appended desktop turn'}],delete_ids:[]});
  const leased=await outbox.lease(writer);assert.equal(leased.operation_id,input.operation_id);
  assert.equal(leased.context.prefix_hash,context.prefix_hash);
  await outbox.ack(writer,{operation_id:leased.operation_id,message_id:leased.message_id,status:'accepted'});
  // The original read context still permits an append even after another turn.
  await outbox.send(reader,{...input,operation_id:'append_offline2'});
  await db.query("UPDATE av_messages SET text='Edited old input' WHERE session_id='ses_append' AND message_id='msg_anchor'");
  assert.equal((await outbox.lease(writer)).needs_confirmation,'append_offline2');
  await assert.rejects(outbox.send(reader,{...input,operation_id:'append_offline3'}),/history_changed/);
});

test('runtime snapshot is scoped, rejects chat payloads, and does not query Neon',async()=>{
  const previous=(await gate.runtime('device123|epoch123')).statuses;
  const token='t'.repeat(43),hash=await digest(token);
  await gate.enroll({token_hash:hash,owner_id:OWNER,device_id:'runtime-device',database_epoch:'runtime-epoch',expires_at:Date.now()+3600_000});
  let opened=0;const app=application({database:async()=>{opened++;throw new Error('not needed');}});
  const snapshot={available:true,partial:false,statuses:[{session_id:'ses_fixture',status:'needs_you',reason:'approval',updated_at:1,can_message:false}]};
  const post=body=>app.fetch(req('/v1/connector/runtime',{method:'POST',headers:{authorization:`Bearer ${token}`},body:JSON.stringify(body)}),env);
  assert.equal((await post(snapshot)).status,200);assert.equal(opened,0);
  assert.equal((await gate.runtime('runtime-device|runtime-epoch')).statuses[0].reason,'approval');
  assert.equal((await post({...snapshot,statuses:[{...snapshot.statuses[0],directory:'/projects/example'}]})).status,200);
  assert.equal((await gate.runtime('runtime-device|runtime-epoch')).statuses[0].directory,'/projects/example');
  assert.deepEqual((await gate.runtime('device123|epoch123')).statuses,previous);
  assert.equal((await post({...snapshot,statuses:[{...snapshot.statuses[0],tool_output:'PRIVATE'}]})).status,400);
});

test('unchanged runtime history is not rewritten on every heartbeat; receipt wake is durable',async()=>{
  const memory=storage(),object=new WakeGate({storage:memory});
  const ns={idFromName:x=>x,get:()=>({fetch:(url,init)=>object.fetch(new Request(url,init))})};
  const gate=gateClient(ns,OWNER),hash=await digest('d'.repeat(43)),source='device|epoch';
  await gate.enroll({token_hash:hash,device_id:'device',database_epoch:'epoch',expires_at:Date.now()+60_000});
  const statuses=Array.from({length:1000},(_,i)=>({session_id:`ses_${i}`,status:'done',reason:null,updated_at:1,can_message:true,last_user_message_id:null}));
  const snapshot={available:true,partial:false,truncated:false,statuses};
  await gate.setRuntime(hash,snapshot);const initial=memory.writes;
  await gate.setRuntime(hash,snapshot);assert.equal(memory.writes-initial,3);
  statuses[0].status='needs_you';const before=memory.writes;
  await gate.setRuntime(hash,snapshot);assert.equal(memory.writes-before,4);
  assert.equal((await gate.runtime(source)).statuses.length,1000);
  await gate.signalReceipt(source,'operation_before_wait');
  assert.equal((await gate.waitReceipt(source,'operation_before_wait')).signalled,true);
});
test('idle connector poll does not open Postgres; wake generations cannot be cleared by an older ACK',async()=>{
  const hash=await digest(writerToken),source='device123|epoch123';
  const a=await gate.signal(source),b=await gate.signal(source);await gate.clear(source,a.generation);
  assert.equal((await gate.inspect(hash)).pending,true);
  let opened=0;
  const app=application({database:async()=>{opened++;throw new Error('Must not open');}});
  const response=await app.fetch(req('/v1/connector/poll',{method:'POST',headers:{authorization:`Bearer ${writerToken}`}}),env);
  assert.equal(response.status,200);assert.equal((await response.json()).generation,b.generation);assert.equal(opened,0);
  await gate.revoke(hash);
  assert.equal((await app.fetch(req('/v1/connector/poll',{method:'POST',headers:{authorization:`Bearer ${writerToken}`}}),env)).status,401);
  assert.equal(opened,0);
});

test('owner-wide OAuth follows primary source without widening connector credentials',async()=>{
  const token='o'.repeat(43);
  await db.query('INSERT INTO av_credentials (token_hash,owner_id,device_id,database_epoch,audience,expires_at,scopes) VALUES ($1,$2,$3,$4,$5,$6,$7)',[await digest(token),OWNER,'*','*','read',Date.now()+3600_000,'history:read']);
  const store=new SyncStore(db);
  await db.query('UPDATE av_owner_source SET device_id=$2,database_epoch=$3 WHERE owner_id=$1',[OWNER,'device123','epoch123']);
  assert.equal((await store.authenticate(token,'read')).device_id,'device123');
  await db.query('UPDATE av_owner_source SET device_id=$2,database_epoch=$3 WHERE owner_id=$1',[OWNER,'new-device','new-epoch']);
  assert.equal((await store.authenticate(token,'read')).device_id,'new-device');
  assert.equal((await store.authenticate(writerToken,'sync')).device_id,'device123');
});

test('stale sends require confirmation, cancellation unblocks the queue and expiry distinguishes uncertain admission',async()=>{
  const store=new SyncStore(db),writer=await store.authenticate(writerToken,'sync'),reader=await store.authenticate(readerToken,'read'),outbox=new Outbox(db,gate);
  await store.apply(writer,{version:1,batch_id:'ttl-baseline',session_id:'ses_ttl',kind:'baseline',from:0,through:1,upload_id:'ttl-gen',chunk:0,complete:true,session:state,messages:[],delete_ids:[]});
  await outbox.send(reader,{operation_id:'ttl_operation1',session_id:'ses_ttl',text:'Fixture',base_cursor:1});
  await db.query("UPDATE av_sessions SET cursor=2,state=jsonb_set(state,'{visible_before}','0') WHERE session_id=$1",['ses_ttl']);
  assert.equal((await outbox.lease(writer)).needs_confirmation,'ttl_operation1');
  assert.equal((await outbox.cancel(reader,'ttl_operation1')).status,'cancelled');
  await db.query("UPDATE av_sessions SET state=jsonb_set(state,'{visible_before}','null') WHERE session_id=$1",['ses_ttl']);
  await outbox.send(reader,{operation_id:'ttl_operation2',session_id:'ses_ttl',text:'Fixture',base_cursor:2});
  await outbox.send(reader,{operation_id:'ttl_operation3',session_id:'ses_ttl',text:'Fixture',base_cursor:2});
  const leased=await outbox.lease(writer);assert.equal(leased.operation_id,'ttl_operation2');
  await db.query('UPDATE av_outbox SET expires_at=0 WHERE operation_id IN ($1,$2)',['ttl_operation2','ttl_operation3']);
  assert.equal((await outbox.status(reader,'ttl_operation2')).status,'uncertain');
  assert.equal((await outbox.status(reader,'ttl_operation3')).status,'expired');
  await assert.rejects(outbox.cancel(reader,'ttl_operation2'),/cannot_cancel/);
  assert.equal((await outbox.ack(writer,{operation_id:leased.operation_id,message_id:leased.message_id,status:'accepted'})).status,'accepted');
});
