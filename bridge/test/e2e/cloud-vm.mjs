/** Real deployed Worker + Neon + real Agent View/OpenCode in the Cua macOS VM.
 * Google approval happens in an already-authenticated Cua browser, never host Chrome.
 * Access tokens remain in process memory and are revoked in finally.
 */
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { challenge,randomToken } from '../../private/auth.mjs';
const require=createRequire('/Users/m/Code/understand/package.json');
const {chromium}=require('playwright');
const base='https://agentvoice-private.moritzw.workers.dev';
const root='/Users/lume/agentvoice-e2e-20261116';
const binary='/Users/lume/agentview-bridge-test-v1';
const helper='/Users/lume/agentvoice-environment-v5.py';
const quote=s=>"'"+s.replaceAll("'","'\\''")+"'";
const fetch=async(url,init)=>{
 for(let attempt=0;;attempt++)try{return await globalThis.fetch(url,{...init,signal:AbortSignal.timeout(40_000)});}
 catch(error){if(attempt>=2)throw error;await new Promise(r=>setTimeout(r,250*(attempt+1)));}
};
function vm(command) {
 const r=spawnSync('cua',['sandbox','exec','local:mac-for-desktop-testing','--',command],{encoding:'utf8',timeout:90_000,maxBuffer:4_000_000});
 if(r.status!==0)throw new Error(`VM step failed: ${r.stderr.slice(0,300)} ${r.stdout.slice(0,300)}`);
 return r.stdout.trim();
}
function local(path,body=null,method) {
 const args=['python3',helper,'api',path,JSON.stringify(body),...(method?[method]:[])];
 return JSON.parse(vm(args.map(quote).join(' ')));
}
const bridge=command=>vm(`${quote(binary)} bridge --state-dir ${quote(root+'/bridge')} ${command}`);
const wait=async(fn,description)=>{const deadline=Date.now()+75_000;for(;;){if(await fn())return;if(Date.now()>deadline)throw new Error('Timed out: '+description);await new Promise(r=>setTimeout(r,1000));}};
let access,refresh,client,page,browser;
const rpc=async(name,args)=>{
 const response=await fetch(base+'/mcp',{method:'POST',headers:{authorization:`Bearer ${access}`,'Content-Type':'application/json',Accept:'application/json, text/event-stream'},body:JSON.stringify({jsonrpc:'2.0',id:1,method:'tools/call',params:{name,arguments:args}})});
 assert.equal(response.status,200);const body=await response.json();
 if(body.error || body.result?.isError)throw new Error('MCP failed: '+JSON.stringify(body.error??body.result));
 return JSON.parse(body.result.content[0].text);
};
try {
 vm(`python3 ${quote(helper)} create`);vm(`python3 ${quote(helper)} prompt`);
 const fixtureSession=JSON.parse(vm(`cat ${quote(root+'/session.json')}`)).id;
 await wait(async()=>{
   const value=local(`/api/session/${fixtureSession}/message`),messages=value.data??value;
   return Array.isArray(messages)&&messages.some(m=>m.type==='assistant'&&m.content?.some(p=>p.type==='text'&&p.text?.includes('BRIDGE_E2E_AGENT_REPLY')));
 },'initial real OpenCode mock-model reply');
 bridge('sync-once');
 const registration=await fetch(base+'/register',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({redirect_uris:[base+'/oauth/test-callback'],token_endpoint_auth_method:'none'})});
 assert.equal(registration.status,201);client=(await registration.json()).client_id;
 const verifier=randomToken(),state=randomToken();
 const authorize=base+'/authorize?'+new URLSearchParams({client_id:client,redirect_uri:base+'/oauth/test-callback',response_type:'code',code_challenge:await challenge(verifier),code_challenge_method:'S256',resource:base+'/mcp',scope:'history:read messages:send',state});
 browser=await chromium.connectOverCDP('http://127.0.0.1:19949');page=await browser.contexts()[0].newPage();
 await page.goto(authorize);
 if(page.url().includes('accounts.google.com'))await page.getByText('wallawitsch@gmail.com',{exact:true}).click();
 await page.waitForURL(u=>u.origin===base,{timeout:30_000});
 await page.getByRole('button',{name:'Approve connection',exact:true}).click();
 await page.waitForURL(u=>u.pathname==='/oauth/test-callback',{timeout:30_000});
 const callback=new URL(page.url());assert.equal(callback.searchParams.get('state'),state);assert.equal(callback.searchParams.get('iss'),base);
 const token=await fetch(base+'/token',{method:'POST',body:new URLSearchParams({grant_type:'authorization_code',client_id:client,redirect_uri:base+'/oauth/test-callback',code:callback.searchParams.get('code'),code_verifier:verifier,resource:base+'/mcp'})});
 assert.equal(token.status,200);({access_token:access,refresh_token:refresh}=await token.json());
 console.log('PASS real Google owner login, consent and MCP PKCE token exchange');
 const session=JSON.parse(vm(`cat ${quote(root+'/session.json')}`)).id;
 let history=await rpc('read_session',{session_id:session,limit:20});
 const initialAssistantCount=history.messages.filter(m=>m.role==='assistant').length;
 assert(history.messages.some(m=>m.role==='user'));assert(history.messages.some(m=>m.role==='assistant'));
 assert(!JSON.stringify(history).includes('PRIVATE_TOOL_ONLY_CANARY_739104'));
 assert.equal((await rpc('search_history',{query:'PRIVATE_TOOL_ONLY_CANARY_739104'})).matches.length,0);
 assert((await rpc('search_history',{query:'BRIDGE_E2E_AGENT_REPLY'})).matches.length>0);
 console.log('PASS real source history uploaded; visible text searchable; actual tool canary excluded');
 const operation=randomToken(12),text='E2E_REMOTE_MESSAGE_'+operation;
 const queued=await rpc('send_session_message',{operation_id:operation,session_id:session,text,base_cursor:Number(history.cursor),confirmed:true});
 assert.equal(queued.status,'queued');
 const dup=await rpc('send_session_message',{operation_id:operation,session_id:session,text,base_cursor:Number(history.cursor),confirmed:true});assert.equal(dup.message_id,queued.message_id);
 const delivery=vm(`AGENTVIEW_OPENCODE_SERVICE_FILE=${quote(root+'/service.json')} ${quote(binary)} bridge --state-dir ${quote(root+'/bridge')} deliver-once`);
 assert(delivery.includes('true'));
 assert.equal((await rpc('message_status',{operation_id:operation})).status,'accepted');
 await wait(async()=>{
   bridge('sync-once');history=await rpc('read_session',{session_id:session,limit:20});
   return history.messages.some(m=>m.text===text) && history.messages.filter(m=>m.role==='assistant').length>initialAssistantCount;
 },'remote message and agent reply sync back');
 assert.equal(history.messages.filter(m=>m.message_id===queued.message_id).length,1);
 console.log('PASS remote MCP send → real OpenCode admission → mock-model agent reply → cloud history; no duplicate input');
 local(`/api/session/${session}/revert/stage`,{messageID:queued.message_id,files:false});bridge('sync-once');
 const reverted=await rpc('read_session',{session_id:session});assert(!reverted.messages.some(m=>m.text===text));assert.equal((await rpc('search_history',{query:text})).matches.length,0);
 local(`/api/session/${session}/revert`,null,'DELETE');bridge('sync-once');
 assert((await rpc('read_session',{session_id:session})).messages.some(m=>m.text===text));
 console.log('PASS native revert/unrevert changes visibility in reads and search');
 const fork=local(`/api/session/${session}/fork`,{});const forkId=(fork.data??fork).id;bridge('sync-once');
 assert((await rpc('read_session',{session_id:forkId})).messages.length>0);
 console.log('PASS native fork receives an independent searchable cloud session');
 bridge('install');
 const offlineOperation=randomToken(12);
 vm(`python3 ${quote(helper)} stop-bridge`);
 history=await rpc('read_session',{session_id:session});
 const offlineText='E2E_OFFLINE_'+offlineOperation;
 await rpc('send_session_message',{operation_id:offlineOperation,session_id:session,text:offlineText,base_cursor:Number(history.cursor),confirmed:true});
 assert.equal((await rpc('message_status',{operation_id:offlineOperation})).status,'queued');
 assert(!(await rpc('read_session',{session_id:session})).messages.some(m=>m.text===offlineText));
 bridge('install');
 await wait(async()=> (await rpc('message_status',{operation_id:offlineOperation})).status==='accepted','login daemon reconnect delivery');
 await wait(async()=> (await rpc('read_session',{session_id:session})).messages.some(m=>m.text===offlineText),'background daemon sync');
 vm(`python3 ${quote(helper)} stop-bridge`);
 console.log('PASS dashboard closed: login daemon syncs; offline read works; queued input arrives after reconnect');
 const rotated=await fetch(base+'/token',{method:'POST',body:new URLSearchParams({grant_type:'refresh_token',client_id:client,refresh_token:refresh,resource:base+'/mcp'})});assert.equal(rotated.status,200);({access_token:access,refresh_token:refresh}=await rotated.json());
 await fetch(base+'/revoke',{method:'POST',body:new URLSearchParams({token:refresh})});
 const denied=await fetch(base+'/mcp',{headers:{authorization:`Bearer ${access}`}});assert.equal(denied.status,401);
 console.log('PASS deployed refresh rotation and immediate credential revocation');
 console.log('E2E PASS: all cloud/VM assertions passed. Model replies are deterministic local fixtures, not a paid LLM.');
} finally {
 try{vm(`python3 ${quote(helper)} stop-bridge`);}catch{}
 if(refresh)await fetch(base+'/revoke',{method:'POST',body:new URLSearchParams({token:refresh})}).catch(()=>{});
 if(access)await fetch(base+'/revoke',{method:'POST',body:new URLSearchParams({token:access})}).catch(()=>{});
 if(page)await page.close().catch(()=>{});
}
process.exit(0);
