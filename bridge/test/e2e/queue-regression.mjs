/** Real CF/Neon + native OpenCode/Agent View in the existing macOS Cua VM.
 * Reuses its isolated synthetic source. NEVER pairs it as the primary source.
 * A short-lived source-bound reader is operator-provisioned for these tests;
 * actual owner OAuth/client discovery is verified separately in ChatGPT.
 */
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {randomBytes} from 'node:crypto';
import {openDatabase} from '../../private/database.mjs';
import {digest} from '../../sync/projection.mjs';
const base='https://agentvoice-private.moritzw.workers.dev',owner='wallawitsch@gmail.com';
const root='/Users/lume/agentvoice-e2e-20261116';
const helper='/Users/lume/agentvoice-environment-repair-v8.py';
const binary='/Users/lume/agentview-mobile-repair-test-v2';
const reader=randomBytes(32).toString('base64url'),suffix=randomBytes(8).toString('hex');
const quote=s=>"'"+s.replaceAll("'","'\\''")+"'";
const pause=ms=>new Promise(r=>setTimeout(r,ms));
let db,config,fixture;const created=[];
const command=args=>args.map(quote).join(' ');
const vm=async(args)=>{
 const p=spawn('cua',['sandbox','exec','local:mac-for-desktop-testing','--',command(args)],{stdio:['ignore','pipe','pipe']});let out='',err='';p.stdout.on('data',x=>out+=x);p.stderr.on('data',x=>err+=x);
 const timer=setTimeout(()=>p.kill('SIGTERM'),90_000);const code=await new Promise(r=>p.on('exit',r));clearTimeout(timer);
 if(code!==0)throw new Error('VM regression command failed: '+args.slice(0,3).join(' ')+' '+(err+' '+out).slice(-1600));return out.trim();
};
const bridge=verb=>vm(['env','AGENTVIEW_OPENCODE_API=v2','AGENTVIEW_OPENCODE_SERVICE_FILE='+root+'/service.json',binary,'bridge','--state-dir',root+'/bridge',verb]);
const native=async(path,body=null,method)=>JSON.parse(await vm(['python3',helper,'api',path,JSON.stringify(body),...(method?[method]:[])]));
const wait=async(fn,label)=>{const end=Date.now()+90_000;while(Date.now()<end){if(await fn())return;await pause(1000);}throw new Error('Timed out: '+label);};
const rpc=async(name,args)=>{
 const response=await fetch(base+'/mcp',{method:'POST',headers:{authorization:`Bearer ${reader}`,'Content-Type':'application/json',Accept:'application/json, text/event-stream'},body:JSON.stringify({jsonrpc:'2.0',id:1,method:'tools/call',params:{name,arguments:args}}),signal:AbortSignal.timeout(45_000)});
 if(!response.ok)throw new Error('MCP fixture HTTP '+response.status);const value=await response.json();if(value.error||value.result?.isError)throw new Error('MCP fixture tool failed: '+(value.result?.content?.[0]?.text??'protocol_error'));return JSON.parse(value.result.content[0].text);
};
const count=async(table,id)=>{
 assert(['session_message','session_inbox'].includes(table));
 const code=`import sqlite3;d=sqlite3.connect(${JSON.stringify('file:'+root+'/data/opencode/history-v1.db?mode=ro')},uri=True);print(d.execute(${JSON.stringify('SELECT count(*) FROM '+table+' WHERE id=?')},(${JSON.stringify(id)},)).fetchone()[0])`;
 return Number(await vm(['python3','-c',code]));
};
try {
 config=JSON.parse(await vm(['cat',root+'/bridge/config.json']));
 const url=execFileSync('neonctl',['connection-string','--project-id','frosty-pond-48075355','--role-name','neondb_owner','--pooled'],{encoding:'utf8',timeout:30_000}).trim();db=await openDatabase(url);
 const primary=(await db.query('SELECT device_id,database_epoch FROM av_owner_source WHERE owner_id=$1',[owner])).rows[0];
 assert(primary&&primary.device_id!==config.device_id,'the fixture must not be the real primary source');
 await db.query('INSERT INTO av_credentials (token_hash,owner_id,device_id,database_epoch,audience,expires_at,scopes) VALUES ($1,$2,$3,$4,$5,$6,$7)',[await digest(reader),owner,config.device_id,config.database_epoch,'read',Date.now()+30*60_000,'history:read messages:send']);
 fixture=spawn('cua',['sandbox','exec','local:mac-for-desktop-testing','--',command(['python3',helper,'serve'])],{stdio:['ignore','ignore','ignore']});
 await wait(async()=>{try{return Boolean(await native('/api/info'));}catch{return false;}},'isolated native service');
 const newSession=async(title,permissions)=>{
  const result=await native('/api/session',{title,location:{directory:root+'/work'},model:{providerID:'fixture',id:'echo'},...(permissions?{permissions}:{})});
  const id=(result.data??result).id;created.push(id);return id;
 };
 const session=await newSession('Queue append regression '+suffix);
 const prompt=(id,text)=>native(`/api/session/${session}/prompt`,{id,text,delivery:'queue',resume:true});
 const replies=async()=>{const value=await native(`/api/session/${session}/message`);return (value.data??value).filter(m=>m.type==='assistant'&&m.content?.some(p=>p.type==='text'));};
 await prompt('msg_reg_initial_'+suffix,'Initial synthetic turn');await wait(async()=>(await replies()).length>=1,'initial reply');await bridge('sync-once');
 const view=await rpc('read_session',{session_id:session});assert(view.context);
 // Reproduce the real failure: the desktop changes while the connector is
 // offline, so the phone is reading a valid but older mirrored revision.
 await prompt('msg_reg_desktop_'+suffix,'An ordinary appended desktop turn');await wait(async()=>(await replies()).length>=2,'desktop reply');
 const input={operation_id:'offline_append_'+suffix,session_id:session,text:'Phone instruction after reconnect',base_cursor:Number(view.cursor),context:view.context,confirmed:true};
 const queued=await rpc('send_session_message',input);assert.equal(queued.status,'queued');assert.equal(queued.delivery_confirmed,false);
 await wait(async()=>{await bridge('deliver-once');return (await rpc('message_status',{operation_id:input.operation_id})).status==='accepted';},'native admission after reconnect');
 await wait(async()=>(await replies()).length>=3,'phone reply');
 await wait(async()=>{await bridge('sync-once');return (await rpc('message_status',{operation_id:input.operation_id})).completion_confirmed;},'correlated native completion');
 assert.equal(await count('session_message',queued.message_id),1);
 assert.equal((await rpc('send_session_message',input)).message_id,queued.message_id);await bridge('deliver-once');assert.equal(await count('session_message',queued.message_id),1);
 console.log('PASS offline append → actual native admission/reply → correlated completion; retry has one native input');
 const current=await rpc('read_session',{session_id:session});
 const guarded=await rpc('send_session_message',{operation_id:'revert_guard_'+suffix,session_id:session,text:'Must not pass changed history',base_cursor:Number(current.cursor),context:current.context,confirmed:true});
 await native(`/api/session/${session}/revert/stage`,{messageID:queued.message_id,files:false});
 await wait(async()=>{await bridge('deliver-once');return (await rpc('message_status',{operation_id:guarded.operation_id})).status==='needs_confirmation';},'revert guard');
 assert.equal(await count('session_message',guarded.message_id)+await count('session_inbox',guarded.message_id),0);
 await rpc('cancel_message',{operation_id:guarded.operation_id,confirmed:true});await native(`/api/session/${session}/revert`,null,'DELETE');
 console.log('PASS rewritten/reverted context still blocks native admission');
 const approval=await newSession('Native approval regression '+suffix,[{action:'read',resource:'*',effect:'ask'}]);
 await native(`/api/session/${approval}/prompt`,{id:'msg_reg_approval_'+suffix,text:'Please read canary.txt (read canary) from this workspace.',delivery:'queue',resume:true});
 await wait(async()=>{const value=await native('/api/permission/request');return (value.data??value).some(p=>p.sessionID===approval);},'actual native approval');
 await wait(async()=>{await bridge('sync-once');const result=await rpc('list_sessions',{status:'needs_you'});return result.status_fresh&&result.sessions.some(s=>s.session_id===approval&&s.reason==='approval');},'native needs-you snapshot');
 console.log('PASS one list_sessions(status=needs_you) call identifies a real native approval without reading chat history');
 assert.deepEqual((await db.query('SELECT device_id,database_epoch FROM av_owner_source WHERE owner_id=$1',[owner])).rows[0],primary);
 console.log('VM NATIVE REGRESSION PASS. Personal primary source unchanged; deterministic local model, no paid model.');
} catch(error) {console.error(error.message);process.exitCode=1;}
finally {
 // Retain the old synthetic source and its genuine phone-test audit. Delete
 // only this run's new native sessions, then sync their normal tombstones.
 for(const id of created)await native(`/api/session/${id}`,null,'DELETE').catch(()=>{});
 if(created.length)await bridge('sync-once').catch(()=>{});
 await fetch(base+'/revoke',{method:'POST',body:new URLSearchParams({token:reader})}).catch(()=>{});
 await vm(['python3',helper,'stop']).catch(()=>{});
 if(fixture){await Promise.race([new Promise(r=>fixture.once('exit',r)),pause(5000)]);if(fixture.exitCode===null)fixture.kill('SIGTERM');}
 if(db){await db.query('DELETE FROM av_credentials WHERE token_hash=$1',[await digest(reader)]).catch(()=>{});await db.close();}
}
process.exit(process.exitCode??0);
