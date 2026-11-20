// Operator-only provisioning. Secrets stay in process memory, 1Password and Worker secrets.
import { spawnSync } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { openDatabase } from './database.mjs';
import { randomToken } from './auth.mjs';

const project=process.env.NEON_PROJECT_ID;
if(!project)throw new Error('Set NEON_PROJECT_ID explicitly');
const run=(cmd,args,input,env)=>{
  const r=spawnSync(cmd,args,{input,encoding:'utf8',env:{...process.env,...env}});
  if(r.status!==0)throw new Error(`${cmd} operation failed (secret output suppressed)`);
  return r.stdout;
};
const root=run('neonctl',['connection-string','--project-id',project,'--role-name','neondb_owner','--pooled']).trim();
const db=await openDatabase(root);
let password;
try {
  await db.query(await readFile(new URL('../sync/schema.sql',import.meta.url),'utf8'));
  await db.query(await readFile(new URL('./schema.sql',import.meta.url),'utf8'));
  const role=(await db.query("SELECT rolname FROM pg_roles WHERE rolname='agentvoice_runtime'")).rows[0];
  if(role) {
    const item=JSON.parse(run('op',['item','get','AgentVoice Neon database','--vault','Main','--format','json']));
    password=item.fields.find(f=>f.id==='password').value;
  } else {
    password=randomToken();
    const sql=(await db.query("SELECT format('CREATE ROLE agentvoice_runtime LOGIN PASSWORD %L',$1::text) AS sql",[password])).rows[0].sql;
    await db.query(sql);
    const template=JSON.parse(run('op',['item','template','get','Login']));
    template.title='AgentVoice Neon database';template.fields.find(f=>f.id==='username').value='agentvoice_runtime';template.fields.find(f=>f.id==='password').value=password;
    template.fields.find(f=>f.id==='notesPlain').value=`Runtime database role for AgentVoice. Free organization org-purple-unit-21830200. Project ${project}. Host ${new URL(root).hostname}. Database neondb. No schema ownership or DDL grants.`;
    const service=run('security',['find-generic-password','-s','op-service-account','-w']).trim();
    run('/opt/homebrew/bin/op',['item','create','--template=/dev/stdin','--vault','Main','--format','json'],JSON.stringify(template),{OP_SERVICE_ACCOUNT_TOKEN:service});
  }
  await db.query('GRANT USAGE ON SCHEMA public TO agentvoice_runtime');
  const tables=(await db.query("SELECT tablename FROM pg_tables WHERE schemaname='public' AND tablename LIKE 'av_%'")).rows;
  for(const {tablename} of tables) {
    if(!/^av_[a-z_]+$/.test(tablename))throw new Error('Unexpected table name');
    await db.query(`GRANT SELECT,INSERT,UPDATE,DELETE ON TABLE "${tablename}" TO agentvoice_runtime`);
  }
  await db.query('GRANT USAGE,SELECT ON SEQUENCE av_outbox_sequence_seq TO agentvoice_runtime');
  await db.query('GRANT USAGE,SELECT ON SEQUENCE av_starts_sequence_seq TO agentvoice_runtime');
  const runtime=new URL(root);runtime.username='agentvoice_runtime';runtime.password=password;
  const google=JSON.parse(run('op',['item','get','AgentVoice Google OAuth','--vault','Main','--format','json']));
  const secrets={DATABASE_URL:runtime.href,GOOGLE_CLIENT_ID:google.fields.find(f=>f.id==='username').value,GOOGLE_CLIENT_SECRET:google.fields.find(f=>f.id==='password').value};
  for(const [name,value] of Object.entries(secrets)) {
    run('npx',['wrangler','secret','put',name,'--config','wrangler.private.jsonc'],value);
    console.log(`Uploaded ${name} securely`);
  }
  const check=await openDatabase(runtime.href);
  try {console.log('Runtime credential verified; source rows:',(await check.query('SELECT count(*)::int AS n FROM av_sessions')).rows[0].n);}
  finally {await check.close();}
} catch(e) { console.error('Provisioning failed; sensitive details suppressed. '+(e?.message?.includes('operation failed')?e.message:''));process.exitCode=1; }
finally {await db.close();}
