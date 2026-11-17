/** Real deployed Worker + Neon contract tests. Synthetic cloud-only source,
 * simulated status metadata, no native execution or personal-source switch.
 * Tokens exist only in memory and are revoked on every exit path.
 */
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {randomUUID,randomBytes} from 'node:crypto';
import {openDatabase} from '../../private/database.mjs';
import {digest} from '../../sync/projection.mjs';
const base='https://agentvoice-private.moritzw.workers.dev',owner='wallawitsch@gmail.com';
const device='contracts_'+randomUUID().replaceAll('-',''),epoch=randomUUID().replaceAll('-','');
const reader=randomBytes(32).toString('base64url');let writer,db,timer,refreshChain=Promise.resolve(),failedRefresh;
const source=[owner,device,epoch],ids=[0,1,2].map(i=>'ses_'+device+'_'+i);
let statuses=ids.map((session_id,i)=>({session_id,status:['needs_you','working','done'][i],reason:i===0?'reply':null,updated_at:100-i,can_message:true,last_user_message_id:null,attention_assessed:true,top_level:true,last_user_ms:Date.now()-i}));
async function post(path,token,body){
 const response=await fetch(base+path,{method:'POST',headers:{'Content-Type':'application/json',...(token?{authorization:`Bearer ${token}`}:{})},body:JSON.stringify(body),signal:AbortSignal.timeout(35_000)});
 if(!response.ok)throw new Error(`Fixture HTTP ${response.status} at ${path}`);return response.json();
}
const refresh=()=>refreshChain=refreshChain.then(async()=>{await post('/v1/connector/checked',writer,{});await post('/v1/connector/runtime',writer,{available:true,partial:false,truncated:false,statuses});});
async function rpc(name,args,allowError=false){
 if(failedRefresh)throw failedRefresh;
 const response=await fetch(base+'/mcp',{method:'POST',headers:{authorization:`Bearer ${reader}`,'Content-Type':'application/json',Accept:'application/json, text/event-stream'},body:JSON.stringify({jsonrpc:'2.0',id:1,method:'tools/call',params:{name,arguments:args}}),signal:AbortSignal.timeout(40_000)});
 assert.equal(response.status,200);const body=await response.json();
 if(allowError)return body.result;
 assert(!body.result?.isError,body.result?.content?.[0]?.text);return JSON.parse(body.result.content[0].text);
}
try{
 const url=execFileSync('neonctl',['connection-string','--project-id','frosty-pond-48075355','--role-name','neondb_owner','--pooled'],{encoding:'utf8',timeout:30_000}).trim();db=await openDatabase(url);
 const primary=(await db.query('SELECT device_id,database_epoch FROM av_owner_source WHERE owner_id=$1',[owner])).rows[0];assert(primary&&primary.device_id!==device);
 const pairing=await post('/pair/start',null,{device_id:device,database_epoch:epoch,label:'Disposable cloud tool contract fixture'});
 // Operator approves only this synthetic fixture. Do not call owner consent,
 // which would make this device the primary source for the real ChatGPT grant.
 await db.query('UPDATE av_pairings SET approved=true WHERE code_hash=$1 AND device_id=$2',[await digest(pairing.device_code),device]);
 writer=(await post('/pair/token',null,{device_code:pairing.device_code})).access_token;assert(writer);
 await db.query('INSERT INTO av_credentials(token_hash,owner_id,device_id,database_epoch,audience,expires_at,scopes) VALUES($1,$2,$3,$4,$5,$6,$7)',[await digest(reader),...source,'read',Date.now()+30*60_000,'history:read messages:send']);
 const special='😀 '.repeat(13000)+'A contractquartzneedle is here. '+'z'.repeat(5000)+' Should we ship option A or option B?';
 const messages=Array.from({length:8},(_,i)=>({id:'msg_'+device+'_'+i,role:i===0?'user':'assistant',order:i+1,created_at:i+1,updated_at:i+1,text:i===7?special:'x'.repeat(25000)}));
 for(let i=0;i<ids.length;i++)await post('/v1/sync/batches',writer,{version:1,batch_id:'batch_'+device+'_'+i,session_id:ids[i],kind:'baseline',from:0,through:100,upload_id:'gen_'+device+'_'+i,chunk:0,complete:true,session:{title:'Synthetic cloud contract '+i,archived:false,deleted:false,visible_before:null},messages:i===0?messages:[],delete_ids:[]});
 statuses[0].last_user_message_id=messages[0].id;
 await refresh();timer=setInterval(()=>{refresh().catch(error=>{failedRefresh=error;});},1500);
 const seen=[];let after=0;
 for(let i=0;i<10;i++){const page=await rpc('read_session',{session_id:ids[0],after,limit:5});if(page.no_newer_messages){assert.deepEqual(page.messages,[]);break;}seen.push(...page.messages.map(m=>m.message_order));after=page.next_after;}
 assert.deepEqual(seen,[1,2,3,4,5,6,7,8]);console.log('PASS deployed forward pagination has no skipped inputs and never substitutes old replies');
 const result=await rpc('search_history',{query:'contractquartzneedle'}),hit=result.matches.find(m=>m.session_id===ids[0]);
 assert(hit&&hit.excerpt.includes('contractquartzneedle'));assert(hit.excerpt_offset>0);assert([...hit.excerpt].length<=2000);
 const message=await rpc('read_message',{session_id:ids[0],message_id:hit.message_id,offset:hit.excerpt_offset});assert(message.text.startsWith(hit.excerpt));assert(message.mac_seen_recently);assert(message.source_checked_at);
 console.log('PASS deployed matching search excerpt and Unicode-compatible message offset');
 const overview=await rpc('list_sessions',{status:'active'});
 assert.equal(overview.sessions.length,2);assert.equal(overview.counts.total,3);assert.equal(overview.counts.working,1);assert.equal(overview.counts.needs_you,1);
 const decision=overview.sessions.find(s=>s.session_id===ids[0]);assert(decision.last_assistant_message.text.endsWith('option A or option B?'));assert(decision.last_user_message);assert.equal(decision.attention,'needs_you');
 console.log('PASS one deployed overview includes active counts and the actual pending question');
 const first=await rpc('list_sessions',{limit:1});const second=await rpc('list_sessions',{limit:1,after:first.next_after});assert.notEqual(first.sessions[0].session_id,second.sessions[0].session_id);assert(first.status_fresh);
 statuses[2].status='working';await refresh();const changed=await rpc('list_sessions',{limit:1,after:first.next_after},true);assert(changed.isError);assert.equal(changed.content[0].text,'session_list_changed_restart_pagination');
 console.log('PASS deployed session-list cursor detects a changed snapshot');
 const view=await rpc('read_session',{session_id:ids[0]});const operation='op_'+device;
 const queued=await rpc('send_session_message',{operation_id:operation,session_id:ids[0],text:'Cloud-only fixture. Never execute.',base_cursor:Number(view.cursor),context:view.context,confirmed:true});assert.equal(queued.status,'queued');assert.equal(queued.delivery_confirmed,false);
 assert.equal((await rpc('message_status',{operation_id:operation})).status,'queued');
 assert.equal((await rpc('cancel_message',{operation_id:operation,confirmed:true})).status,'cancelled');
 console.log('PASS cloud-only queued receipt and cancellation; no native admission was simulated');
 assert.deepEqual((await db.query('SELECT device_id,database_epoch FROM av_owner_source WHERE owner_id=$1',[owner])).rows[0],primary);
 console.log('CLOUD CONTRACTS PASS. Personal primary source and history unchanged.');
}catch(error){console.error(error.message);process.exitCode=1;}
finally{
 if(timer)clearInterval(timer);await refreshChain.catch(()=>{});
 for(const token of [reader,writer].filter(Boolean))await fetch(base+'/revoke',{method:'POST',body:new URLSearchParams({token}),signal:AbortSignal.timeout(30_000)}).catch(()=>{});
 if(db){
  let clean=true;
  for(const table of ['av_staged_deletes','av_messages','av_baselines','av_receipts','av_sessions','av_outbox','av_credentials'])try{await db.query(`DELETE FROM ${table} WHERE owner_id=$1 AND device_id=$2 AND database_epoch=$3`,source);}catch{clean=false;}
  try{await db.query('DELETE FROM av_pairings WHERE device_id=$1 AND database_epoch=$2',[device,epoch]);}catch{clean=false;}
  if(!clean){console.error('Synthetic fixture cleanup was incomplete');process.exitCode=1;}else console.log('Synthetic fixture grants and database rows removed.');
  await db.close();
 }
}
process.exit(process.exitCode??0);
