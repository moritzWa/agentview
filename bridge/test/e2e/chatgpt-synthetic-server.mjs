// Operator harness: serves the real MCP handlers over HTTP with PURE SYNTHETIC
// sessions and no authentication, so a throwaway ChatGPT custom connector can
// exercise overview/deep-dive/send behavior without touching the production
// Worker, its OAuth grants or any real history. Run on the host and expose with
// a temporary tunnel; stop it afterwards.
//
//   node test/e2e/chatgpt-synthetic-server.mjs [port]
//
// GET /__log prints every MCP call (method, tool, arguments) seen so far.
// GET /__outbox prints queued sends, to verify nothing was written implicitly.
import http from 'node:http';
import {readFile} from 'node:fs/promises';
import {PGlite} from '@electric-sql/pglite';
import {privateMcp} from '../../private/mcp.mjs';
import {SyncStore} from '../../sync/store.mjs';
import {Outbox} from '../../private/outbox.mjs';
import {digest} from '../../sync/projection.mjs';

const port=Number(process.argv[2]??8799);
const root=new URL('../../',import.meta.url);
const db=new PGlite();
await db.exec(await readFile(new URL('sync/schema.sql',root),'utf8'));await db.exec(await readFile(new URL('private/schema.sql',root),'utf8'));
const store=new SyncStore(db);
for(const [token,audience,scopes] of [['w'.repeat(43),'sync','history:sync messages:deliver'],['r'.repeat(43),'read','history:read messages:send']])
 await db.query('INSERT INTO av_credentials(token_hash,owner_id,device_id,database_epoch,audience,expires_at,scopes) VALUES($1,$2,$3,$4,$5,$6,$7)',[await digest(token),'synthetic-owner','synthetic-device','synthetic-epoch',audience,Date.now()+86400_000,scopes]);
const writer=await store.authenticate('w'.repeat(43),'sync'),reader=await store.authenticate('r'.repeat(43),'read');

const log=[];
const gate={activity:async()=>({source_checked_at:Date.now(),mac_seen_recently:true}),signal:async()=>{},signalReceipt:async()=>{},waitReceipt:async()=>({signalled:false}),
 runtime:async()=>({available:true,partial:false,truncated:false,checked_at:Date.now(),statuses:statuses()})};
const outbox=new Outbox(db,gate);

const turn=(order,role,text)=>({id:`msg_${order}`,role,order,created_at:order,updated_at:order,text});
async function seed(id,title,messages){await store.apply(writer,{version:1,batch_id:id,session_id:id,kind:'baseline',from:0,through:100,upload_id:id,chunk:0,complete:true,session:{title,archived:false,deleted:false,visible_before:null},messages,delete_ids:[]});}
const MIN=60_000;
const sessions=[
 {id:'ses_synth_pricing',title:'Pricing page redesign',dir:'/work/acme-site',status:'needs_you',reason:'reply',ageMin:12,messages:[
  turn(1,'user','Redesign the pricing page so the Team plan stands out.'),
  turn(2,'assistant','I built two variants. Variant A highlights Team with a bold border and a "Most popular" badge; it keeps the layout and ships today. Variant B reorders the plans so Team is first, which tested better in a similar product but changes the navigation links and needs a copy review. Do you want A now, or B after the copy review?')]},
 {id:'ses_synth_migration',title:'Database migration',dir:'/work/acme-api',status:'needs_you',reason:'approval',ageMin:3,messages:[
  turn(1,'user','Add the audit_log table and backfill it.'),
  turn(2,'assistant','The migration is written and tested on a copy. It now needs permission to run against the staging database.')]},
 {id:'ses_synth_onboarding',title:'Onboarding emails',dir:'/work/acme-growth',status:'needs_you',reason:'reply',ageMin:95,messages:[
  turn(1,'user','Draft the three onboarding emails.'),
  turn(2,'assistant','Drafts are ready. Should the sender be the founder or the support team?')]},
 {id:'ses_synth_diagram_bench',title:'diagram-bench',dir:'/work/diagram-bench',status:'done',assessed:true,ageMin:40,messages:[
  turn(1,'user','Build a benchmark that measures how accurately our extractor reads diagrams, and report the accuracy.'),
  turn(2,'assistant','Plan: 1) collect 40 diagrams, 2) run the extractor, 3) score against reference labels.'),
  turn(3,'user','Keep it simple, no fancy metrics.'),
  turn(4,'assistant','Collected 40 diagrams from the docs and wrote the harness plus the scoring script; the extractor runs end to end on all 40. '+'Details of the harness layout, the cache and the report format follow. '.repeat(40)
   +'\n\nThe benchmark is blocked on ground truth: there are no reference labels, so accuracy cannot be computed. The options are (a) you label ten diagrams by hand, about 20 minutes, which gives a rough accuracy with wide error bars; (b) I auto-label all 40 with a strong model and you spot-check three, which is faster but inherits the model\'s mistakes; or (c) skip labels and compare the old and new extractor against each other, which shows regressions but not absolute accuracy. Please label ten diagrams, or tell me to use option b.')]},
 {id:'ses_synth_docs',title:'Docs typo fix',dir:'/work/acme-docs',status:'done',assessed:true,ageMin:200,messages:[
  turn(1,'user','Fix the typo on the install page.'),turn(2,'assistant','Fixed the typo and pushed. Nothing else needed.')]},
 {id:'ses_synth_refactor',title:'Parser refactor',dir:'/work/acme-core',status:'working',ageMin:1,messages:[
  turn(1,'user','Refactor the parser.'),turn(2,'assistant','Working on it.')]},
];
for(const s of sessions)await seed(s.id,s.title,s.messages);
const statuses=()=>sessions.map(s=>({session_id:s.id,status:s.status,reason:s.reason??null,updated_at:Date.now()-s.ageMin*MIN,last_user_ms:Date.now()-s.ageMin*MIN,top_level:true,attention_assessed:s.assessed??s.status==='needs_you',can_message:s.reason!=='approval',
 last_user_message_id:`msg_${s.messages.findLast(m=>m.role==='user').order}`,directory:s.dir}));

http.createServer(async(req,res)=>{
 try{
  const url=new URL(req.url,'http://x');
  if(url.pathname==='/__log'){res.setHeader('content-type','application/json');return res.end(JSON.stringify(log,null,1));}
  if(url.pathname==='/__outbox'){res.setHeader('content-type','application/json');return res.end(JSON.stringify((await db.query('SELECT operation_id,session_id,text,state FROM av_outbox')).rows,null,1));}
  if(url.pathname!=='/mcp'||req.method!=='POST'){res.statusCode=url.pathname==='/mcp'?405:404;return res.end();}
  const chunks=[];for await(const c of req)chunks.push(c);const body=Buffer.concat(chunks);
  try{const m=JSON.parse(body.toString());log.push({at:new Date().toISOString(),method:m.method,tool:m.params?.name,arguments:m.params?.arguments});}catch{}
  const response=await privateMcp(new Request('http://x/mcp',{method:'POST',headers:{'content-type':'application/json',accept:'application/json, text/event-stream'},body}),{db,store,principal:reader,outbox,gate});
  res.statusCode=response.status;response.headers.forEach((v,k)=>res.setHeader(k,v));res.end(Buffer.from(await response.arrayBuffer()));
 }catch(e){res.statusCode=500;res.end(String(e));}
}).listen(port,'127.0.0.1',()=>console.log(`synthetic MCP on http://127.0.0.1:${port}/mcp`));
