import {test} from 'node:test';
import assert from 'node:assert/strict';
import {WakeGate,gateClient} from '../private/gate.mjs';
import {privateMcp} from '../private/mcp.mjs';

function setup() {
  const map=new Map();
  const storage={
    get:async k=>Array.isArray(k)?new Map(k.filter(x=>map.has(x)).map(x=>[x,map.get(x)])):map.get(k),
    put:async(k,v)=>{if(typeof k==='object')for(const [a,b] of Object.entries(k))map.set(a,b);else map.set(k,v);},
    delete:async k=>{map.delete(k);},
    transaction:async fn=>fn(storage),
  };
  const object=new WakeGate({storage});
  const namespace={idFromName:x=>x,get:()=>({fetch:(url,init)=>object.fetch(new Request(url,init))})};
  return {gate:gateClient(namespace,'owner'),map};
}
const row=(session_id,status,reason=null)=>({session_id,status,reason,directory:`/work/${session_id}`,top_level:true});
async function enroll(gate) {
  await gate.enroll({token_hash:'hash',owner_id:'owner',device_id:'device',database_epoch:'epoch',expires_at:Date.now()+60_000});
  return snapshot=>gate.setRuntime('hash',{available:true,partial:false,statuses:snapshot});
}

test('the first call reports sessions that need the owner, then only changes', async()=>{
  const {gate}=setup(),publish=await enroll(gate),source='device|epoch';
  await publish([row('a','needs_you','question'),row('b','working'),row('c','done')]);
  const first=await gate.changes(source);
  assert.equal(first.baseline,true);
  assert.deepEqual(first.changes.map(c=>[c.session_id,c.change]),[['a','needs_you']]);
  const again=await gate.changes(source);
  assert.equal(again.baseline,false);
  assert.deepEqual(again.changes,[]);
  await publish([row('a','needs_you','question'),row('b','done'),row('c','needs_you','approval')]);
  const next=await gate.changes(source);
  assert.deepEqual(next.changes.map(c=>[c.session_id,c.change]).sort(),[['b','finished'],['c','needs_you']]);
  assert.deepEqual((await gate.changes(source)).changes,[]);
});

test('a session that needs the owner for a new reason is reported again', async()=>{
  const {gate}=setup(),publish=await enroll(gate),source='device|epoch';
  await publish([row('a','needs_you','question')]);
  await gate.changes(source);
  await publish([row('a','needs_you','approval')]);
  assert.deepEqual((await gate.changes(source)).changes.map(c=>c.reason),['approval']);
});

test('unavailable or partial native states consume nothing', async()=>{
  const {gate,map}=setup(),source='device|epoch';
  await enroll(gate);
  assert.equal((await gate.changes(source)).available,false);
  await gate.setRuntime('hash',{available:true,partial:true,statuses:[row('a','needs_you','question')]});
  assert.equal((await gate.changes(source)).available,false);
  assert.equal(map.has('seen:device|epoch'),false);
});

test('new_since_last_check returns titles through the MCP tool', async()=>{
  const {gate}=setup(),publish=await enroll(gate);
  await publish([row('a','needs_you','question')]);
  const call=async()=>{
    const response=await privateMcp(new Request('https://test.invalid/mcp',{method:'POST',headers:{'Content-Type':'application/json',Accept:'application/json, text/event-stream'},
      body:JSON.stringify({jsonrpc:'2.0',id:1,method:'tools/call',params:{name:'new_since_last_check',arguments:{}}})}),{
      db:{query:async sql=>sql.includes('av_sessions s WHERE')&&sql.includes('ANY')?{rows:[{session_id:'a',title:'Billing review'}]}:{rows:[{scopes:'history:read',device_id:'device',database_epoch:'epoch'}]}},
      store:{},principal:{device_id:'device',database_epoch:'epoch',owner_id:'owner',token_hash:'hash'},outbox:{},gate,
    });
    const body=await response.json();
    assert.equal(body.result.isError,undefined);
    return JSON.parse(body.result.content[0].text);
  };
  const first=await call();
  assert.equal(first.changes[0].title,'Billing review');
  assert.equal(first.changes[0].project_name,'a');
  assert.deepEqual((await call()).changes,[]);
});
