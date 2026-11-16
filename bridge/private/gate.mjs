/** Durable wake metadata only, never chat text. Idle polls do not connect to Neon. */
function runtimeBucket(id){let hash=0;for(let i=0;i<id.length;i++)hash=(hash*31+id.charCodeAt(i))>>>0;return hash%64;}
export class WakeGate {
  constructor(state) { this.state=state;this.waiters=new Set(); }
  async fetch(request) {
    const p=await request.json(),path=new URL(request.url).pathname,s=this.state.storage;
    if(path==='/enroll') { await s.put(`token:${p.token_hash}`,p);await s.put(`pending:${p.device_id}|${p.database_epoch}`,true);return Response.json({ok:true}); }
    if(path==='/revoke') { await s.delete(`token:${p.token_hash}`);for(const wake of this.waiters)wake();return Response.json({ok:true}); }
    if(path==='/signal') {
      const generation=await s.transaction(async tx=>{const n=(await tx.get(`generation:${p.source}`)??0)+1;await tx.put({[`generation:${p.source}`]:n,[`pending:${p.source}`]:true});return n;});
      for(const wake of this.waiters)wake();
      return Response.json({generation});
    }
    if(path==='/clear') {
      await s.transaction(async tx=>{if((await tx.get(`generation:${p.source}`)??0)===p.generation)await tx.put(`pending:${p.source}`,false);});
      return Response.json({ok:true});
    }
    if(path==='/rate') {
      const minute=Math.floor(Date.now()/60_000),key=`rate:${p.key}`;
      const allowed=await s.transaction(async tx=>{const old=await tx.get(key),n=old?.minute===minute?old.n+1:1;await tx.put(key,{minute,n});return n<=30;});
      return Response.json({allowed});
    }
    if(path==='/activity') {
      const now=Date.now();await s.put(`active_until:${p.source}`,now+5*60_000);
      for(const wake of this.waiters)wake();
      const seen=await s.get(`last_seen:${p.source}`)??0;
      const checks=p.kind==='both'?[['checked',5000],['runtime_checked',20_000]]:[[p.kind==='runtime'?'runtime_checked':'checked',p.kind==='runtime'?20_000:5000]];
      const fresh=async()=>{for(const [key,age] of checks)if((await s.get(`${key}:${p.source}`)??0)<now-age)return false;return true;};
      if(p.wait && seen>now-60_000 && !await fresh()) {
        const deadline=Date.now()+8000;
        while(Date.now()<deadline && !await fresh()) {
          await new Promise(resolve=>{let timer;const wake=()=>{clearTimeout(timer);this.waiters.delete(wake);resolve();};this.waiters.add(wake);timer=setTimeout(wake,Math.max(1,deadline-Date.now()));});
        }
      }
      return Response.json({source_checked_at:await s.get(`checked:${p.source}`)??null,mac_seen_recently:seen>now-60_000});
    }
    if(path==='/runtime-read') {
      const keys=Array.from({length:64},(_,i)=>`runtime:${p.source}:${i}`),stored=await s.get(keys);
      const meta=await s.get(`runtime_meta:${p.source}`)??{available:false,partial:false,checked_at:null};
      return Response.json({...meta,statuses:keys.flatMap(k=>JSON.parse(stored.get(k)??'[]'))});
    }
    if(path==='/receipt-signal') {
      const key=`receipts:${p.source}`,recent=await s.get(key)??[];
      await s.put(key,[...recent.filter(r=>r!==p.operation_id),p.operation_id].slice(-32));
      for(const wake of this.waiters)wake();return Response.json({ok:true});
    }
    if(path==='/receipt-wait') {
      const deadline=Date.now()+8000,key=`receipts:${p.source}`;
      while(Date.now()<deadline && !(await s.get(key)??[]).includes(p.operation_id)) {
        await new Promise(resolve=>{let timer;const wake=()=>{clearTimeout(timer);this.waiters.delete(wake);resolve();};this.waiters.add(wake);timer=setTimeout(wake,Math.max(1,deadline-Date.now()));});
      }
      return Response.json({signalled:(await s.get(key)??[]).includes(p.operation_id)});
    }
    const credential=await s.get(`token:${p.token_hash}`);
    if(!credential || credential.expires_at<=Date.now())return Response.json({error:'unauthorized'},{status:401});
    const source=`${credential.device_id}|${credential.database_epoch}`;
    if(path==='/runtime-write') {
      const {statuses,...meta}=p.snapshot,now=Date.now(),buckets=Array.from({length:64},()=>[]);
      for(const status of statuses)buckets[runtimeBucket(status.session_id)].push(status);
      const keys=buckets.map((_,i)=>`runtime:${source}:${i}`),old=await s.get(keys);
      const changed={[`runtime_meta:${source}`]:{...meta,checked_at:now},[`runtime_checked:${source}`]:now,[`last_seen:${source}`]:now};
      for(let i=0;i<buckets.length;i++) {
        const value=JSON.stringify(buckets[i].sort((a,b)=>a.session_id.localeCompare(b.session_id)));
        if(value!==(old.get(keys[i])??'[]'))changed[keys[i]]=value;
      }
      await s.put(changed);
      for(const wake of this.waiters)wake();
      return Response.json({ok:true});
    }
    if(path==='/checked') {const now=Date.now();await s.put({[`checked:${source}`]:now,[`last_seen:${source}`]:now});for(const wake of this.waiters)wake();}
    if(path==='/poll') {
      await s.put(`last_seen:${source}`,Date.now());
      if(!(await s.get(`pending:${source}`))) {
        await new Promise(resolve=>{
          let timer;
          const wake=()=>{clearTimeout(timer);this.waiters.delete(wake);resolve();};
          this.waiters.add(wake);timer=setTimeout(wake,20_000);
        });
      }
      if(!(await s.get(`token:${p.token_hash}`)))return Response.json({error:'unauthorized'},{status:401});
    }
    return Response.json({principal:credential,pending:await s.get(`pending:${source}`)??false,generation:await s.get(`generation:${source}`)??0,last_seen:await s.get(`last_seen:${source}`)??null,active_until:await s.get(`active_until:${source}`)??0});
  }
}

export function gateClient(namespace,key) {
  const stub=namespace.get(namespace.idFromName(key));
  const call=async(path,p)=>{
    const response=await stub.fetch(`https://internal${path}`,{method:'POST',body:JSON.stringify(p)});
    const body=await response.json();if(!response.ok)throw Object.assign(new Error('unauthorized'),{status:response.status});return body;
  };
  return {enroll:p=>call('/enroll',p),revoke:token_hash=>call('/revoke',{token_hash}),signal:source=>call('/signal',{source}),
    clear:(source,generation)=>call('/clear',{source,generation}),poll:token_hash=>call('/poll',{token_hash}),
    inspect:token_hash=>call('/inspect',{token_hash}),checked:token_hash=>call('/checked',{token_hash}),
    activity:(source,wait=false,kind='history')=>call('/activity',{source,wait,kind}),
    runtime:source=>call('/runtime-read',{source}),setRuntime:(token_hash,snapshot)=>call('/runtime-write',{token_hash,snapshot}),
    signalReceipt:(source,operation_id)=>call('/receipt-signal',{source,operation_id}),waitReceipt:(source,operation_id)=>call('/receipt-wait',{source,operation_id}),rate:key=>call('/rate',{key})};
}
