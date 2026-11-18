import {test,before,after} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {PGlite} from '@electric-sql/pglite';
import {privateMcp,SERVER_INSTRUCTIONS} from '../private/mcp.mjs';
import {SyncStore} from '../sync/store.mjs';
import {Outbox} from '../private/outbox.mjs';
import {digest} from '../sync/projection.mjs';
import {finalAsk,matchesQuery,ageText,buildDigest} from '../private/attention.mjs';

// Synthetic data only: nothing here is read from real sessions.
let db,store,outbox,reader,writer;
let runtime={available:true,partial:false,checked_at:0,statuses:[]};
let seen=true,checkedAt=()=>Date.now();
const gate={activity:async()=>({source_checked_at:checkedAt(),mac_seen_recently:seen}),runtime:async()=>runtime,signal:async()=>{},signalReceipt:async()=>{},waitReceipt:async()=>({signalled:false})};
before(async()=>{
 db=new PGlite();await db.exec(await readFile(new URL('../sync/schema.sql',import.meta.url),'utf8'));await db.exec(await readFile(new URL('../private/schema.sql',import.meta.url),'utf8'));
 store=new SyncStore(db);outbox=new Outbox(db,gate);
 for(const [token,audience,scopes] of [['w'.repeat(43),'sync','history:sync messages:deliver'],['r'.repeat(43),'read','history:read messages:send']])await db.query('INSERT INTO av_credentials(token_hash,owner_id,device_id,database_epoch,audience,expires_at,scopes) VALUES($1,$2,$3,$4,$5,$6,$7)',[await digest(token),'test-owner','test-device','test-epoch',audience,Date.now()+3600_000,scopes]);
 writer=await store.authenticate('w'.repeat(43),'sync');reader=await store.authenticate('r'.repeat(43),'read');
});
after(async()=>db?.close());

const turn=(order,role,text)=>({id:`msg_${order}`,role,order,created_at:order,updated_at:order,text});
async function seed(id,title,messages){await store.apply(writer,{version:1,batch_id:id,session_id:id,kind:'baseline',from:0,through:100,upload_id:id,chunk:0,complete:true,session:{title,archived:false,deleted:false,visible_before:null},messages,delete_ids:[]});}
async function rpcRaw(name,args){
 const response=await privateMcp(new Request('https://test.invalid/mcp',{method:'POST',headers:{'Content-Type':'application/json',Accept:'application/json, text/event-stream'},body:JSON.stringify({jsonrpc:'2.0',id:1,method:'tools/call',params:{name,arguments:args}})}),{db,store,principal:reader,outbox,gate});
 return response.json();
}
async function rpc(name,args){const body=await rpcRaw(name,args);assert(!body.result?.isError,body.result?.content?.[0]?.text);return JSON.parse(body.result.content[0].text);}
async function method(name,params={}){
 const response=await privateMcp(new Request('https://test.invalid/mcp',{method:'POST',headers:{'Content-Type':'application/json',Accept:'application/json, text/event-stream'},body:JSON.stringify({jsonrpc:'2.0',id:1,method:name,params})}),{db,store,principal:reader,outbox,gate});
 return response.json();
}
const status=(session_id,extra={})=>({session_id,status:'done',reason:null,updated_at:Date.now()-60_000,last_user_ms:Date.now()-60_000,top_level:true,attention_assessed:true,can_message:true,last_user_message_id:'msg_1',directory:`/work/${session_id}`,...extra});

// Four pending sessions, one of which the desktop classifier called "done".
const DIAGRAM_REPLY='I built the diagram benchmark harness and the scoring script. '+'Extraction details, metrics and code layout. '.repeat(120)
 +'\n\nThe bench is blocked on ground truth: there are no reference labels, so accuracy cannot be computed. Options: (a) you label ten diagrams by hand, about 20 minutes; (b) I auto-label with a strong model and you spot-check three; (c) skip labels and compare two extractors against each other only. Please label ten diagrams, or tell me to use option b.';
async function seedOverview(){
 const needs=[['mobile-sync','Mobile sync fix','Should I ship the sync fix now or wait for the Tuesday release?'],
  ['billing-export','Billing export','Which currency should the export default to, USD or EUR?'],
  ['auth-refactor','Auth refactor','The refactor is ready. Do you want it as one PR or three?']];
 for(const [id,title,ask] of needs)await seed(id,title,[turn(1,'user','Please work on '+title),turn(2,'assistant','Progress is good. '+ask)]);
 await seed('diagram-bench','diagram-bench',[turn(1,'user','Build a benchmark for diagram extraction and report accuracy.'),turn(2,'assistant','Started on the harness.'),turn(3,'user','Keep it simple.'),turn(4,'assistant',DIAGRAM_REPLY)]);
 await seed('finished-clean','Docs typo fix',[turn(1,'user','Fix the typo in the docs.'),turn(2,'assistant','Fixed the typo and pushed. Nothing else needed.')]);
 await seed('still-working','Long refactor',[turn(1,'user','Refactor the parser.'),turn(2,'assistant','Working on it.')]);
 runtime={available:true,partial:false,checked_at:Date.now(),statuses:[
  ...needs.map(([id],i)=>status(id,{status:'needs_you',reason:'reply',updated_at:Date.now()-(i+1)*600_000})),
  status('diagram-bench',{status:'done',last_user_message_id:'msg_3'}),status('finished-clean',{status:'done'}),status('still-working',{status:'working'})]};
}

test('four pending sessions: the done-but-blocked one is named, and the answer is short',async()=>{
 await seedOverview();
 const result=await rpc('list_sessions',{status:'active'});
 assert.equal(result.counts.needs_you,3,'native count stays what the desktop reports');
 const d=result.digest;
 assert.equal(d.confirmed,3);assert.equal(d.top.length,3);assert(d.top.every(i=>i.confirmed));
 assert.deepEqual(d.may_also_need_you.map(i=>i.session_id),['diagram-bench'],'diagram-bench must not be omitted');
 assert(!JSON.stringify(d).includes('finished-clean')&&!JSON.stringify(d).includes('still-working'));
 assert(d.say.startsWith('3 sessions need you.'));assert(d.say.includes('diagram-bench may also be waiting'));
 assert(d.say.split(/\s+/).length<=60,'spoken summary must stay short: '+d.say);
 assert(d.presentation.includes('nothing else'));
 assert.equal(d.timing_ms.freshness_wait>=0,true);
 const body=JSON.stringify(result);assert(body.length<6000,'attention response is compact, got '+body.length);
 assert(result.sessions.every(s=>!('recent_messages' in s)),'no message text in the overview rows');
 assert.equal(result.sessions.find(s=>s.session_id==='ses_none'),undefined);
});

test('the possible decision is named, not argued, and only for a direct ask',()=>{
 const base={now:Date.now(),counts:{},statusFresh:true,freshness:{mac_seen_recently:true}};
 const row={session_id:'x',title:'bench',status:'done',attention_assessed:true,mirrored:true};
 const d=buildDigest({...base,rows:[row],tails:new Map([['x',{text:'Done with the harness. Please label ten diagrams so I can score it.',assistant_last:true}]])});
 assert.equal(d.confirmed,0);assert.equal(d.top.length,0);assert.deepEqual(d.may_also_need_you,[{session_id:'x',title:'bench'}]);
 assert.equal(d.say,'Nothing is confirmed waiting on you. bench may also be waiting.');
 for(const text of ['Merged and published. Want me to keep going on the follow-ups?','Shortcut restored. You only need to choose a free key if you want an extra shortcut.','All done. Let me know if you want more.','Results are published. Should I also write a summary?']) {
  const none=buildDigest({...base,rows:[row],tails:new Map([['x',{text,assistant_last:true}]])});
  assert.equal(none.may_also_need_you.length,0,'optional offer flagged: '+text);
 }
 const unassessed=buildDigest({...base,rows:[{...row,attention_assessed:false}],tails:new Map([['x',{text:'Which database should I use?',assistant_last:true}]])});
 assert.equal(unassessed.may_also_need_you.length,1);
});

test('the digest is the first key of the list_sessions result and a non-attention view has none',async()=>{
 const raw=await rpcRaw('list_sessions',{status:'active'}),text=raw.result.content[0].text;
 assert.equal(Object.keys(JSON.parse(text))[0],'digest');assert(JSON.parse(text).digest.say);assert(text.indexOf('"digest"')<text.indexOf('"sessions"'));
 assert.equal((await rpc('list_sessions',{status:'working'})).digest,undefined);
 assert.equal((await rpc('list_sessions',{query:'billing'})).digest,undefined,'a name search is not an overview');
});

test('overview to deep dive: naming a session returns task, full final reply, decision, data age and send parameters',async()=>{
 const brief=await rpc('session_brief',{query:'diagram bench'});
 assert.equal(brief.found,true);assert.equal(brief.session.session_id,'diagram-bench');assert.equal(brief.session.status,'done');assert(brief.session.done_note);
 assert(brief.task.text.startsWith('Build a benchmark'));
 assert.equal(brief.agent_replied_last,true);
 const final=brief.transcript.at(-1);
 assert.equal(final.role,'assistant');assert(final.text.includes('(a) you label ten diagrams'),'options must survive; the old overview kept only ~570 characters');assert(final.text.includes('option b.'));
 assert(brief.transcript_scope.includes('of 4 visible text messages'));
 assert(brief.decision_candidates.final_question_or_blocker.text.includes('label ten diagrams'));
 assert(brief.guide.includes('DEEP DIVE')&&brief.guide.includes('five short sentences')&&brief.guide.includes('recommendation'));assert(brief.timing_ms.freshness_wait>=0);
 assert(brief.send_guide.includes('exact text'));
 assert.equal(brief.data_age.status_fresh,true);assert.equal(brief.data_age.caveat,null);
 assert(brief.send_params.context&&Number.isInteger(brief.send_params.base_cursor));
 assert(!JSON.stringify(brief).includes('still-working'),'a deep dive carries no other sessions');
});

test('a deep dive writes nothing; the send path still queues only through send_session_message with brief parameters',async()=>{
 const before=(await db.query('SELECT count(*)::int AS n FROM av_outbox')).rows[0].n;
 const brief=await rpc('session_brief',{session_id:'diagram-bench'});
 assert.equal((await db.query('SELECT count(*)::int AS n FROM av_outbox')).rows[0].n,before);
 const sent=await rpc('send_session_message',{operation_id:'deepdive_send_1',session_id:'diagram-bench',text:'Use option b.',confirmed:true,...brief.send_params});
 assert.equal(sent.session_id,'diagram-bench');assert(['queued','leased','accepted'].includes(sent.status));
 assert.equal((await db.query('SELECT count(*)::int AS n FROM av_outbox')).rows[0].n,before+1);
});

test('ambiguous, missing and unknown names ask or fail instead of guessing',async()=>{
 await seed('bench-v2','diagram-bench v2',[turn(1,'user','Second bench'),turn(2,'assistant','Done.')]);
 runtime.statuses.push(status('bench-v2',{status:'done'}));
 const exact=await rpc('session_brief',{query:'diagram-bench'});assert.equal(exact.found,true);assert.equal(exact.session.session_id,'diagram-bench','an exact title wins over a longer one');
 const ambiguous=await rpc('session_brief',{query:'bench'});
 assert.equal(ambiguous.found,false);assert.equal(ambiguous.ambiguous,true);assert.equal(ambiguous.candidates.length,2);assert(!('transcript' in ambiguous));
 const none=await rpc('session_brief',{query:'nonexistent project'});assert.equal(none.found,false);assert(!none.ambiguous);
 const unknown=await rpcRaw('session_brief',{session_id:'does-not-exist'});assert(unknown.result.isError);assert.equal(unknown.result.content[0].text,'session_not_found');
 const empty=await rpcRaw('session_brief',{});assert(empty.result.isError);
});

test('stale sync is reported with its age, in the digest and the brief',async()=>{
 const old=Date.now()-12*60_000,previous=runtime.checked_at;
 runtime={...runtime,checked_at:old};checkedAt=()=>old;seen=false;
 try{
  const d=(await rpc('list_sessions',{status:'active'})).digest;
  assert.equal(d.data_age.status_fresh,false);assert(d.data_age.caveat.includes('not live'));assert(d.data_age.caveat.includes('12 minutes'));assert(d.data_age.caveat.includes('Mac has not been seen'));
  const b=await rpc('session_brief',{session_id:'diagram-bench'});assert.equal(b.data_age.status_fresh,false);assert(b.data_age.caveat.includes('12 minutes'));assert.equal(b.data_age.status,'12 minutes');
 }finally{runtime={...runtime,checked_at:previous};checkedAt=()=>Date.now();seen=true;}
});

test('a finished turn that ends on a user message, or an old question, is not flagged',async()=>{
 await seed('user-last','User spoke last',[turn(1,'user','Question?'),turn(2,'assistant','Should I proceed with plan A or plan B?'),turn(3,'user','Plan A.')]);
 await seed('old-question','Moved on',[turn(1,'user','Go'),turn(2,'assistant','Should we use X? '+'Then I went ahead with X and finished everything. '.repeat(40)+'All done.')]);
 await seed('unclassified','Not yet classified',[turn(1,'user','Go'),turn(2,'assistant','Which database should I use?')]);
 runtime={...runtime,statuses:[status('user-last',{status:'done'}),status('old-question',{status:'done'}),status('unclassified',{status:'done',attention_assessed:false})]};
 const d=(await rpc('list_sessions',{status:'active'})).digest;
 assert.deepEqual(d.may_also_need_you.map(i=>i.session_id),['unclassified']);assert.equal(d.top.length,0);
 const none=buildDigest({rows:[],tails:new Map(),now:Date.now(),counts:{working:2},statusFresh:true,freshness:{mac_seen_recently:true}});
 assert.equal(none.say,'Nothing needs you right now.');
});

test('native approval and question prompts are high urgency and rank before replies',()=>{
 const rows=[{session_id:'a',title:'A',status:'needs_you',reason:'reply',updated_at:Date.now()},{session_id:'b',title:'B',status:'needs_you',reason:'approval',updated_at:Date.now()-1e6},{session_id:'c',title:'C',status:'needs_you',reason:'failed',updated_at:Date.now()}];
 const d=buildDigest({rows,tails:new Map(),now:Date.now(),counts:{},statusFresh:true,freshness:{mac_seen_recently:true}});
 assert.deepEqual(d.top.map(i=>[i.session_id,i.urgency]),[['b','high'],['c','medium'],['a','medium']]);
 assert(d.top[0].decision.includes('permission'));assert(d.say.includes('B: approve a permission'));
});

test('server instructions put mode rules in the first 512 characters, and the new tool is advertised',async()=>{
 const init=await method('initialize',{protocolVersion:'2025-06-18',capabilities:{},clientInfo:{name:'test',version:'1'}});
 const head=init.result.instructions.slice(0,512);
 assert.equal(init.result.instructions,SERVER_INSTRUCTIONS);
 for(const word of ['list_sessions','digest.say','session_brief','send_session_message','brief'])assert(head.includes(word),word);
 const tools=(await method('tools/list')).result.tools,byName=Object.fromEntries(tools.map(t=>[t.name,t]));
 assert(byName.session_brief.annotations.readOnlyHint);assert.equal(byName.send_session_message.annotations.readOnlyHint,false);
 assert(byName.list_sessions.description.includes('digest.say'));assert(byName.read_session.description.includes('session_brief'));
});

test('finalAsk, matchesQuery and ageText',()=>{
 assert.equal(finalAsk('Done. Ship it now or wait for Tuesday?').text,'Ship it now or wait for Tuesday?');
 assert.equal(finalAsk('A?\nWhich option do you prefer?').text,'Which option do you prefer?');
 assert.equal(finalAsk('All finished. Let me know if you want more.'),null);
 assert.equal(finalAsk('Blocked on credentials for the staging server.').source,'blocker_phrase_in_last_reply');
 assert.equal(finalAsk('Should we do X? '+'filler. '.repeat(200)),null,'a question outside the window is old news');
 assert.equal(finalAsk(''),null);assert.equal(finalAsk(undefined),null);
 assert(matchesQuery({title:'diagram-bench'},'diagram bench'));assert(matchesQuery({title:'x',project_name:'project-alpha'},'Alpha'));assert(!matchesQuery({title:'x'},'diagram'));assert(!matchesQuery({title:'x'},''));
 assert.equal(ageText(30_000),'30 seconds');assert.equal(ageText(12*60_000),'12 minutes');assert.equal(ageText(5*3600_000),'5 hours');assert.equal(ageText(3*86400_000),'3 days');assert.equal(ageText(-1),null);
});

test('list_sessions query now matches names ignoring punctuation',async()=>{
 runtime={...runtime,statuses:[status('diagram-bench',{status:'done',last_user_message_id:'msg_3'})]};
 const result=await rpc('list_sessions',{query:'diagram bench',view:'all'});
 assert(result.sessions.some(s=>s.session_id==='diagram-bench'));
});

test('a runtime-only wait ignores stale history and a short cap bounds the wait',async()=>{
 const {WakeGate,gateClient}=await import('../private/gate.mjs');
 const values=new Map();const storage={get:async k=>Array.isArray(k)?new Map(k.map(x=>[x,values.get(x)])):values.get(k),put:async(k,v)=>{if(typeof k==='object')for(const [key,value]of Object.entries(k))values.set(key,value);else values.set(k,v);},transaction:async fn=>fn(storage)};
 const object=new WakeGate({storage}),namespace={idFromName:x=>x,get:()=>({fetch:(url,init)=>object.fetch(new Request(url,init))})};
 const g=gateClient(namespace,'owner'),hash='b'.repeat(64),source='device|epoch';
 await g.enroll({token_hash:hash,device_id:'device',database_epoch:'epoch',expires_at:Date.now()+3600_000});await g.poll(hash);
 values.set(`checked:${source}`,Date.now()-120_000);values.set(`runtime_checked:${source}`,Date.now()-1000);
 let t=Date.now();await g.activity(source,true,'runtime',{max_wait:4000});assert(Date.now()-t<500,'fresh runtime must not wait for stale history');
 values.set(`runtime_checked:${source}`,Date.now()-60_000);
 t=Date.now();await g.activity(source,true,'runtime',{max_wait:300});const took=Date.now()-t;assert(took>=250&&took<1500,'stale runtime waits only up to the cap, took '+took);
});
