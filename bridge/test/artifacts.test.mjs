import {test,before,after} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {PGlite} from '@electric-sql/pglite';
import {SyncStore} from '../sync/store.mjs';
import {digest} from '../sync/projection.mjs';
import {privateMcp} from '../private/mcp.mjs';
import {application} from '../private/worker.mjs';
import {publishArtifact,listArtifacts,previewArtifact,parseArtifact,PREVIEW_INSTRUCTIONS,MAX_ARTIFACT_BYTES} from '../private/artifacts.mjs';

let db,store,reader,writer,other;
const html='<html><body><h1>Original 😀 fixture</h1></body></html>';
const blob=Buffer.from(html).toString('base64');
const artifact={artifact_id:'a'.repeat(32),session_id:'ses_artifact',title:'Synthetic preview',name:'index.html',mime_type:'text/html',blob,confirmed:true};
before(async()=>{
  db=new PGlite();
  for(const file of ['../sync/schema.sql','../private/schema.sql'])await db.exec(await readFile(new URL(file,import.meta.url),'utf8'));
  store=new SyncStore(db);
  for(const [token,audience,owner,device,scopes] of [['w','sync','owner','device','history:sync'],['r','read','owner','device','history:read'],['o','read','other-owner','device','history:read'],['d','read','owner','other-device','history:read']]){
    await db.query('INSERT INTO av_credentials(token_hash,owner_id,device_id,database_epoch,audience,expires_at,scopes) VALUES($1,$2,$3,$4,$5,$6,$7)',[await digest(token.repeat(43)),owner,device,'epoch',audience,Date.now()+3600_000,scopes]);
  }
  writer=await store.authenticate('w'.repeat(43),'sync');reader=await store.authenticate('r'.repeat(43),'read');other=await store.authenticate('o'.repeat(43),'read');
  await store.apply(writer,{version:1,batch_id:'artifact-baseline',session_id:'ses_artifact',kind:'baseline',from:0,through:1,upload_id:'artifact-baseline',chunk:0,complete:true,session:{title:'Fixture',archived:false,deleted:false,visible_before:null},messages:[],delete_ids:[]});
});
after(async()=>db?.close());
test('publishes only explicit approved bytes; retry is immutable and list is metadata-only',async()=>{
  const first=await publishArtifact(db,writer,artifact);
  assert.equal(first.bytes,Buffer.byteLength(html));assert.equal(first.sha256,await digest(html));
  assert.deepEqual(await publishArtifact(db,writer,artifact),first);
  await assert.rejects(publishArtifact(db,writer,{...artifact,blob:Buffer.from('changed').toString('base64')}),{code:'artifact_id_conflict'});
  const {artifacts}=await listArtifacts(db,reader,{session_id:'ses_artifact'});
  assert.equal(artifacts.length,1);assert.equal(artifacts[0].name,'index.html');assert(!('blob' in artifacts[0]));
});
test('default preview returns unchanged original bytes as an attachment, never a widget',async()=>{
  const response=await previewArtifact(db,reader,artifact);
  assert.equal(response._meta,undefined);
  const file=response.content.find(c=>c.type==='resource');
  assert.equal(file.resource.mimeType,'text/html');assert.equal(file.resource.text,html);
  assert.equal(Buffer.from(file.resource.text).toString('base64'),blob);
  assert.equal(file.resource.uri,`artifact://${artifact.artifact_id}/index.html`);
  assert.match(response.content[0].text,/Open preview/);
  assert.match(response.content[0].text,/do not claim it is already visible/);
});
test('preview tools expose one self-explanatory file path, not renderer choices',async()=>{
  const request=method=>new Request('https://test.invalid/mcp',{method:'POST',headers:{'Content-Type':'application/json',Accept:'application/json, text/event-stream'},body:JSON.stringify({jsonrpc:'2.0',id:1,...method})});
  const env={db,store,principal:reader,outbox:{},gate:{}};
  const listed=await (await privateMcp(request({method:'tools/list'}),env)).json();
  const tools=listed.result.tools;
  const preview=tools.find(t=>t.name==='preview_artifact');
  assert(preview);assert.equal(preview.description,PREVIEW_INSTRUCTIONS);assert.equal(preview._meta,undefined);
  assert.deepEqual(tools.filter(t=>/preview|legacy|embedded/.test(t.name)).map(t=>t.name),['preview_artifact']);
  const result=await (await privateMcp(request({method:'tools/call',params:{name:'preview_artifact',arguments:{artifact_id:artifact.artifact_id}}}),env)).json();
  assert.equal(result.result.content[1].resource.text,html);assert(!result.result.isError);
});
test('owner, source and live grant boundaries prevent file reads',async()=>{
  for(const p of [other,await store.authenticate('d'.repeat(43),'read'),{...reader,database_epoch:'other-epoch'}]){
    await assert.rejects(previewArtifact(db,p,artifact));
  }
  assert.deepEqual((await listArtifacts(db,other,{session_id:'ses_artifact'})).artifacts,[]);
  await db.query('UPDATE av_credentials SET revoked=true WHERE token_hash=$1',[reader.token_hash]);
  try{await assert.rejects(previewArtifact(db,reader,artifact),{code:'unauthorized'});}
  finally{await db.query('UPDATE av_credentials SET revoked=false WHERE token_hash=$1',[reader.token_hash]);}
});
test('read grants cannot publish, and hidden sessions cannot publish or preview',async()=>{
  await assert.rejects(publishArtifact(db,reader,{...artifact,artifact_id:'b'.repeat(32)}),{code:'unauthorized'});
  await assert.rejects(publishArtifact(db,writer,{...artifact,session_id:'unknown'}),{code:'session_not_found'});
  await db.query('UPDATE av_sessions SET state=jsonb_set(state,\'{deleted}\',\'true\') WHERE session_id=$1',['ses_artifact']);
  try{
    await assert.rejects(previewArtifact(db,reader,artifact),{code:'artifact_not_found'});
    await assert.rejects(publishArtifact(db,writer,{...artifact,artifact_id:'b'.repeat(32)}),{code:'session_not_found'});
    assert.deepEqual((await listArtifacts(db,reader,{session_id:'ses_artifact'})).artifacts,[]);
  }finally{await db.query('UPDATE av_sessions SET state=jsonb_set(state,\'{deleted}\',\'false\') WHERE session_id=$1',['ses_artifact']);}
});
test('rejects missing consent, arbitrary paths, noncanonical base64 and oversized files',()=>{
  for(const update of [{confirmed:false},{name:'../secret.html'},{name:'..'},{name:'x\\file.html'},{name:'x\n.html'},{blob:'Zh=='},{blob:'not base64'},{mime_type:'application/javascript'},{local_path:'/private/secret'}]){
    assert.throws(()=>parseArtifact({...artifact,...update}),{code:'invalid_artifact'});
  }
  assert.throws(()=>parseArtifact({...artifact,blob:Buffer.alloc(MAX_ARTIFACT_BYTES+1).toString('base64')}));
  assert.throws(()=>parseArtifact({...artifact,blob:'/w=='}),{code:'artifact_requires_utf8'});
});
test('binary attachments and UTF-8 BOMs retain their original bytes',async()=>{
  for(const [artifact_id,name,mime_type,bytes] of [
    ['c'.repeat(32),'fixture.png','image/png',Buffer.from([137,80,78,71,0,255])],
    ['d'.repeat(32),'bom.html','text/html',Buffer.concat([Buffer.from([239,187,191]),Buffer.from(html)])],
  ]){
    const value={...artifact,artifact_id,name,mime_type,blob:bytes.toString('base64')};
    await publishArtifact(db,writer,value);
    const file=(await previewArtifact(db,reader,value)).content[1].resource;
    assert.deepEqual(file.text===undefined?Buffer.from(file.blob,'base64'):Buffer.from(file.text),bytes);
  }
});
test('HTTP publication uses connector authentication, never the ChatGPT read grant',async()=>{
  const app=application({database:async()=>({query:(...args)=>db.query(...args),transaction:fn=>db.transaction(fn),close:async()=>{}})});
  const env={ISSUER:'https://test.invalid',GATE:{idFromName:x=>x,get:()=>({fetch:()=>{throw new Error('Unexpected gate call');}})},DATABASE_URL:'fixture'};
  const request=token=>new Request('https://test.invalid/v1/artifacts',{method:'POST',headers:{'Content-Type':'application/json',...(token?{authorization:`Bearer ${token.repeat(43)}`}:{})},body:JSON.stringify({...artifact,artifact_id:'e'.repeat(32)})});
  assert.equal((await app.fetch(request(),env)).status,401);
  assert.equal((await app.fetch(request('r'),env)).status,401);
  const response=await app.fetch(request('w'),env);
  assert.equal(response.status,200);assert.equal((await response.json()).artifact_id,'e'.repeat(32));
});
test('revocation while a file is fetched blocks releasing its contents',async()=>{
  let revoked=false;
  const racingDb={query:async(...args)=>{
    const result=await db.query(...args);
    if(!revoked&&args[0].startsWith('SELECT a.artifact_id,a.name')){
      revoked=true;await db.query('UPDATE av_credentials SET revoked=true WHERE token_hash=$1',[reader.token_hash]);
    }
    return result;
  }};
  try{await assert.rejects(previewArtifact(racingDb,reader,artifact),{code:'unauthorized'});}
  finally{await db.query('UPDATE av_credentials SET revoked=false WHERE token_hash=$1',[reader.token_hash]);}
});
test('bounded source storage still permits retries of an approved revision',async()=>{
  const count=Number((await db.query('SELECT count(*) AS n FROM av_artifacts')).rows[0].n);
  await db.query(`INSERT INTO av_artifacts(owner_id,device_id,database_epoch,artifact_id,session_id,title,name,mime_type,blob,sha256,byte_length)
    SELECT 'owner','device','epoch','quota_'||i,'ses_artifact','Quota fixture','fixture.txt','text/plain','eA==','fixture',1 FROM generate_series(1,$1::integer) AS i`,[128-count]);
  try{
    await assert.rejects(publishArtifact(db,writer,{...artifact,artifact_id:'f'.repeat(32)}),{code:'artifact_quota_reached'});
    assert.equal((await publishArtifact(db,writer,artifact)).artifact_id,artifact.artifact_id);
  }finally{await db.query("DELETE FROM av_artifacts WHERE artifact_id LIKE 'quota_%'");}
});
