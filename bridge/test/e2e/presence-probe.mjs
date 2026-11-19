/** Read-only presence probe against the real deployed Worker.
 * Mints a short-lived history:read credential in memory, lists the tools, calls mac_presence,
 * and removes the credential on every exit path. Prints no message text.
 *   node presence-probe.mjs
 */
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {randomBytes} from 'node:crypto';
import {openDatabase} from '../../private/database.mjs';
import {digest} from '../../sync/projection.mjs';

const base = 'https://agentvoice-private.moritzw.workers.dev', owner = 'wallawitsch@gmail.com';
const reader = randomBytes(32).toString('base64url');
let db, hash;

async function rpc(method, params) {
  const started = Date.now();
  const response = await fetch(base + '/mcp', {method: 'POST', headers: {authorization: `Bearer ${reader}`, 'Content-Type': 'application/json', Accept: 'application/json, text/event-stream'},
    body: JSON.stringify({jsonrpc: '2.0', id: 1, method, params}), signal: AbortSignal.timeout(60_000)});
  assert.equal(response.status, 200);
  const body = await response.json();
  assert(!body.result?.isError, body.result?.content?.[0]?.text);
  return {ms: Date.now() - started, result: body.result};
}

try {
  const url = execFileSync('neonctl', ['connection-string', '--project-id', 'frosty-pond-48075355', '--role-name', 'neondb_owner', '--pooled'], {encoding: 'utf8', timeout: 30_000}).trim();
  db = await openDatabase(url);
  const primary = (await db.query('SELECT device_id,database_epoch FROM av_owner_source WHERE owner_id=$1', [owner])).rows[0];
  assert(primary, 'no paired source');
  hash = await digest(reader);
  await db.query('INSERT INTO av_credentials(token_hash,owner_id,device_id,database_epoch,audience,expires_at,scopes) VALUES($1,$2,$3,$4,$5,$6,$7)',
    [hash, owner, primary.device_id, primary.database_epoch, 'read', Date.now() + 10 * 60_000, 'history:read']);
  const tools = (await rpc('tools/list', {})).result.tools.map(t => t.name);
  console.log(JSON.stringify({tools}));
  assert(tools.includes('mac_presence'), 'mac_presence is not exposed by the deployed Worker');
  const {ms, result} = await rpc('tools/call', {name: 'mac_presence', arguments: {}});
  console.log(JSON.stringify({http_ms: ms, presence: JSON.parse(result.content[0].text)}, null, 2));
} finally {
  if (hash && db) try { await db.query('DELETE FROM av_credentials WHERE token_hash=$1', [hash]); console.log('credential removed'); } catch { console.log('CREDENTIAL CLEANUP FAILED'); process.exitCode = 1; }
  await fetch(base + '/revoke', {method: 'POST', body: new URLSearchParams({token: reader}), signal: AbortSignal.timeout(30_000)}).catch(() => {});
  if (db) await db.close();
}
