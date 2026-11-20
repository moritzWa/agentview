import {test} from 'node:test';
import assert from 'node:assert/strict';
import {macPresence,HEARTBEAT_ONLINE_MS,HISTORY_FRESH_MS} from '../private/presence.mjs';
import {WakeGate,gateClient} from '../private/gate.mjs';
import {privateMcp} from '../private/mcp.mjs';

const now=1_700_000_000_000;
test('a recent heartbeat is online and reachable, without reading any queue',()=>{
  const report=macPresence(now,{last_seen:now-20_000,checked:now-60_000,sync_skipped:0});
  assert.equal(report.bridge,'online');
  assert.equal(report.internet,'reachable');
  assert.equal(report.history_sync,'fresh');
  assert.equal(report.stale,false);
  assert.equal(report.queue_acceptance_means_mac_online,false);
  assert.equal(report.cadence.idle_history_sync_seconds,900);
  assert.equal(report.cadence.active_tick_seconds,5);
});
test('silence is offline bridge and unknown internet, not a claimed outage',()=>{
  const report=macPresence(now,{last_seen:now-HEARTBEAT_ONLINE_MS-1,checked:now-HISTORY_FRESH_MS-1});
  assert.equal(report.bridge,'offline');
  assert.equal(report.internet,'unknown');
  assert.equal(report.history_sync,'stale');
  assert.equal(report.stale,true);
});
test('never seen is unknown, and a partial scan does not count as fresh history',()=>{
  assert.equal(macPresence(now,{}).bridge,'unknown');
  assert.equal(macPresence(now,{}).internet,'unknown');
  assert.equal(macPresence(now,{}).history_sync,'unknown');
  const partial=macPresence(now,{last_seen:now-1_000,checked:now-60_000,sync_attempted:now-1_000,sync_skipped:2});
  assert.equal(partial.bridge,'online');
  assert.equal(partial.history_sync,'partial');
  assert.equal(partial.stale,true);
  assert.equal(partial.sessions_behind,0);
  assert.equal(partial.behind_note,null);
});
test('sessions behind the cloud are named with how long, and zero is not a warning',()=>{
  const behind=macPresence(now,{last_seen:now-1_000,checked:now-60_000,behind:3,behind_since:now-20*60_000});
  assert.equal(behind.sessions_behind,3);
  assert.equal(behind.behind_for_ms,20*60_000);
  assert.match(behind.behind_note,/3 sessions have not reached the cloud for 20 minutes/);
  assert.equal(macPresence(now,{last_seen:now-1_000,behind:0}).behind_note,null);
});
test('a partial connector check does not stamp history fresh, and presence ignores the queue',async()=>{
  const map=new Map();
  const storage={get:async k=>Array.isArray(k)?new Map():map.get(k),put:async(k,v)=>{if(typeof k==='object')for(const [a,b] of Object.entries(k))map.set(a,b);else map.set(k,v);},delete:async()=>{},transaction:async fn=>fn(storage)};
  const object=new WakeGate({storage});
  const namespace={idFromName:x=>x,get:()=>({fetch:(url,init)=>object.fetch(new Request(url,init))})};
  const gate=gateClient(namespace,'owner');
  const token='t'.repeat(43);
  await gate.enroll({token_hash:'hash',owner_id:'owner',device_id:'device',database_epoch:'epoch',expires_at:now+60_000});
  map.set('token:hash',{device_id:'device',database_epoch:'epoch',expires_at:Date.now()+60_000});
  await gate.checked('hash',{complete:false,skipped:1});
  assert.equal(map.get('checked:device|epoch'),undefined);
  assert.equal(map.get('sync_skipped:device|epoch'),1);
  assert.equal(typeof map.get('sync_attempted:device|epoch'),'number');
  await gate.checked('hash',{});
  assert.equal(typeof map.get('checked:device|epoch'),'number');
  const stamps=await gate.presence('device|epoch');
  const outbox=new Proxy({},{get(){throw new Error('queue_must_not_define_presence');}});
  const response=await privateMcp(new Request('https://test.invalid/mcp',{method:'POST',headers:{'Content-Type':'application/json',Accept:'application/json, text/event-stream'},body:JSON.stringify({jsonrpc:'2.0',id:1,method:'tools/call',params:{name:'mac_presence',arguments:{}}})}),{
    db:{query:async()=>({rows:[{scopes:'history:read',device_id:'device',database_epoch:'epoch'}]})},store:{},principal:{device_id:'device',database_epoch:'epoch',owner_id:'owner',token_hash:'hash'},outbox,gate,
  });
  const body=await response.json();
  assert.equal(body.result.isError,undefined);
  const report=JSON.parse(body.result.content[0].text);
  assert.equal(report.queue_acceptance_means_mac_online,false);
  assert.equal(report.bridge,'online');
  assert.equal(stamps.sync_skipped,0);
  assert.notEqual(token,'');
});

test('history reads say outright when the page may be stale, and clear the flag once the Mac has checked in',async()=>{
  const map=new Map();
  const put=(k,v)=>{if(typeof k==='object')for(const [a,b] of Object.entries(k))map.set(a,b);else map.set(k,v);};
  const storage={get:async k=>Array.isArray(k)?new Map():map.get(k),put:async(k,v)=>put(k,v),delete:async()=>{},transaction:async f=>f({get:async k=>map.get(k),put:async(k,v)=>put(k,v)})};
  const object=new WakeGate({storage});
  const gate=gateClient({idFromName:x=>x,get:()=>({fetch:(url,init)=>object.fetch(new Request(url,init))})},'owner');
  const source='device|epoch',t=Date.now();
  map.set(`last_seen:${source}`,t-1000);map.set(`checked:${source}`,t-600_000);
  const stale=await gate.activity(source,false,'history');
  assert.equal(stale.history_fresh,false);
  assert.equal(stale.retry_after_ms,5000);
  assert.match(stale.history_note,/Repeat the same call/);
  map.set(`last_seen:${source}`,t-600_000);
  const offline=await gate.activity(source,false,'history');
  assert.equal(offline.history_fresh,false);
  assert.equal(offline.retry_after_ms,undefined);
  map.set(`last_seen:${source}`,t);map.set(`checked:${source}`,t);
  const fresh=await gate.activity(source,false,'history');
  assert.equal(fresh.history_fresh,true);
  assert.equal(fresh.history_note,undefined);
  assert.equal(fresh.retry_after_ms,undefined);
  const runtime=await gate.activity(source,false,'runtime');
  assert.equal(runtime.history_fresh,undefined);
});
