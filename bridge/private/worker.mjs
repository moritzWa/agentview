import { OwnerAuth, OWNER, json, html } from './auth.mjs';
import { openDatabase } from './database.mjs';
import { gateClient } from './gate.mjs';
import { Outbox } from './outbox.mjs';
import { Starts } from './starts.mjs';
import { privateMcp } from './mcp.mjs';
import { SyncStore } from '../sync/store.mjs';
import { privateHandler } from '../sync/http.mjs';
import { digest, SyncError } from '../sync/projection.mjs';
import { RuntimeSnapshot } from './runtime.mjs';
import {publishArtifact} from './artifacts.mjs';
export { WakeGate } from './gate.mjs';

const AUTH_PATHS=new Set(['/register','/authorize','/token','/revoke','/pair/start','/pair/token','/pair','/consent','/auth/google/callback']);
async function bounded(request,limit) {
  if(!request.body)return request;
  const reader=request.body.getReader(),chunks=[];let n=0;
  try {
    for(;;) { const {done,value}=await reader.read();if(done)break;n+=value.length;
      if(n>limit){await reader.cancel();throw new SyncError('request_too_large',413);}chunks.push(value); }
  } finally {reader.releaseLock();}
  const bytes=new Uint8Array(n);let offset=0;for(const chunk of chunks){bytes.set(chunk,offset);offset+=chunk.length;}
  return new Request(request.url,{method:request.method,headers:request.headers,body:bytes});
}
export function application({database=openDatabase}={}) {
  return {async fetch(request,env) {
    const url=new URL(request.url),path=url.pathname;
    if(path==='/health')return json({ok:true,private:true});
    if(path==='/oauth/test-callback')return html('<h1>Authorization callback received</h1><p>The integration test can finish exchanging its one-use code. You can close this tab.</p>');
    if(!env.ISSUER || url.origin!==env.ISSUER || !env.GATE) return json({error:'not_configured'},503);
    const gate=gateClient(env.GATE,OWNER);
    const authHeader=request.headers.get('authorization')??'',token=authHeader.startsWith('Bearer ')?authHeader.slice(7):'';
    let db;
    try {
      if(Number(request.headers.get('content-length')??0)>1_000_000)throw new SyncError('request_too_large',413);
      if(path.startsWith('/.well-known/'))return await new OwnerAuth(null,env).handle(request)??json({error:'not_found'},404);
      if(path==='/')return new Response('AgentVoice private bridge. Pair a device to get started.',{headers:{'Cache-Control':'no-store'}});
      if(path==='/v1/connector/checked' && request.method==='POST') {
        if(!/^[A-Za-z0-9_-]{43}$/.test(token))throw new SyncError('unauthorized',401);
        const body=await request.json().catch(()=>({}));
        await gate.checked(await digest(token),{complete:body?.complete!==false,skipped:Number(body?.skipped)||0,behind:Number(body?.behind)||0,behind_since:Number(body?.behind_since)||null});
        return json({ok:true}); // No Neon query. A partial scan does not stamp history fresh.
      }
      if(path==='/v1/connector/poll' && request.method==='POST') {
        if(!/^[A-Za-z0-9_-]{43}$/.test(token))throw new SyncError('unauthorized',401);
        const value=await gate.poll(await digest(token));
        return json({pending:value.pending,generation:value.generation,active_until:value.active_until}); // No Neon query.
      }
      if(path==='/v1/connector/runtime' && request.method==='POST') {
        if(!/^[A-Za-z0-9_-]{43}$/.test(token))throw new SyncError('unauthorized',401);
        request=await bounded(request,850_000);
        const parsed=RuntimeSnapshot.safeParse(await request.json());
        if(!parsed.success)throw new SyncError('invalid_runtime_snapshot',400);
        await gate.setRuntime(await digest(token),parsed.data);return json({ok:true});
      }
      if(AUTH_PATHS.has(path)) {
        const ip=request.headers.get('cf-connecting-ip')??'local';
        const rate=await gate.rate(await digest(`${ip}:${path}`));
        if(!rate.allowed)throw new SyncError('rate_limited',429);
      } else if(!token) {
        return new Response(null,{status:401,headers:{'Cache-Control':'no-store','WWW-Authenticate':`Bearer resource_metadata="${env.ISSUER}/.well-known/oauth-protected-resource/mcp"`}});
      }
      request=await bounded(request,AUTH_PATHS.has(path)?32_000:1_000_000);
      if(!env.DATABASE_URL)throw new SyncError('not_configured',503);
      db=await database(env.DATABASE_URL);
      const auth=await new OwnerAuth(db,env,{gate}).handle(request);if(auth)return auth;
      const store=new SyncStore(db),outbox=new Outbox(db,gate),starts=new Starts(db,gate);
      if(path==='/mcp') {
        const principal=await store.authenticate(token,'read');
        await gate.activity(`${principal.device_id}|${principal.database_epoch}`);
        return await privateMcp(request,{db,store,principal,outbox,gate,starts});
      }
      if(path==='/v1/connector/lease' && request.method==='POST') {
        const principal=await store.authenticate(token,'sync'),value=await gate.inspect(await digest(token));
        const message=await outbox.lease(principal);
        if(!message)await gate.clear(`${principal.device_id}|${principal.database_epoch}`,value.generation);
        return json({message});
      }
      if(path==='/v1/connector/ack' && request.method==='POST') {
        const principal=await store.authenticate(token,'sync');return json(await outbox.ack(principal,await request.json()));
      }
      if(path==='/v1/connector/start-lease' && request.method==='POST') {
        const principal=await store.authenticate(token,'sync');
        return json({start:await starts.lease(principal)});
      }
      if(path==='/v1/connector/start-ack' && request.method==='POST') {
        const principal=await store.authenticate(token,'sync');return json(await starts.ack(principal,await request.json()));
      }
      if((path==='/v1/sync/cursors' || path==='/v1/sync/verify') && request.method==='POST') {
        const principal=await store.authenticate(token,'sync');
        const body=await request.json();
        return json(path.endsWith('/cursors')?await store.cursors(principal,body):await store.verify(principal,body));
      }
      if(path==='/v1/artifacts' && request.method==='POST') {
        const principal=await store.authenticate(token,'sync');
        return json(await publishArtifact(db,principal,await request.json()));
      }
      return await privateHandler(store)(request);
    } catch(e) {
      if(e instanceof SyncError){const response=json({error:e.code},e.status);if(e.status===401)response.headers.set('WWW-Authenticate',`Bearer resource_metadata="${env.ISSUER}/.well-known/oauth-protected-resource/mcp"`);return response;}
      if(e?.status===401)return json({error:'unauthorized'},401);
      // Static class/code only. Never log the message, stack, URL, token or SQL.
      const name=/^[A-Za-z]{1,60}$/.test(e?.name??'')?e.name:'Error';
      const code=/^[A-Z0-9_]{1,60}$/.test(String(e?.code??''))?String(e.code):'UNKNOWN';
      console.error('bridge_request_failed',name,code);
      return json({error:'request_failed'},500); // Deliberately no SQL, token or body logging.
    } finally { try{await db?.close();}catch{console.error('bridge_db_close_failed');} }
  }};
}
export default application();
