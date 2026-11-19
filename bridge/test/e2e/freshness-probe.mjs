/** Read-only freshness probe against the real deployed Worker for one real session.
 * Mints a short-lived history:read credential in memory, calls the real /mcp read_session,
 * and removes the credential on every exit path. Prints message IDs/orders/lengths only,
 * never message text.
 *   SESSION=ses_... WANT_ORDER=<message_order that must become visible> node freshness-probe.mjs
 */
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {randomBytes} from 'node:crypto';
import {openDatabase} from '../../private/database.mjs';
import {digest} from '../../sync/projection.mjs';

const base = 'https://agentvoice-private.moritzw.workers.dev', owner = 'wallawitsch@gmail.com';
const session = process.env.SESSION, want = Number(process.env.WANT_ORDER ?? 0), budgetMs = Number(process.env.BUDGET_MS ?? 90_000);
assert(session, 'SESSION required');
const reader = randomBytes(32).toString('base64url');
let db, hash;

async function rpc(name, args) {
  const started = Date.now();
  const response = await fetch(base + '/mcp', {method: 'POST', headers: {authorization: `Bearer ${reader}`, 'Content-Type': 'application/json', Accept: 'application/json, text/event-stream'},
    body: JSON.stringify({jsonrpc: '2.0', id: 1, method: 'tools/call', params: {name, arguments: args}}), signal: AbortSignal.timeout(60_000)});
  assert.equal(response.status, 200);
  const body = await response.json();
  assert(!body.result?.isError, body.result?.content?.[0]?.text);
  return {ms: Date.now() - started, value: JSON.parse(body.result.content[0].text)};
}
const summarize = ({ms, value}) => ({
  http_ms: ms, cursor: value.cursor, synced_at: value.synced_at, source_checked_at: value.source_checked_at ? new Date(value.source_checked_at).toISOString() : null,
  mac_seen_recently: value.mac_seen_recently, history_fresh: value.history_fresh, history_age_ms: value.history_age_ms, retry_after_ms: value.retry_after_ms, page_kind: value.page_kind,
  messages: value.messages.map(m => ({id: m.message_id.slice(0, 18), order: Number(m.message_order), role: m.role, chars: m.text.length})),
});

try {
  const url = execFileSync('neonctl', ['connection-string', '--project-id', 'frosty-pond-48075355', '--role-name', 'neondb_owner', '--pooled'], {encoding: 'utf8', timeout: 30_000}).trim();
  db = await openDatabase(url);
  const primary = (await db.query('SELECT device_id,database_epoch FROM av_owner_source WHERE owner_id=$1', [owner])).rows[0];
  assert(primary, 'no paired source');
  hash = await digest(reader);
  await db.query('INSERT INTO av_credentials(token_hash,owner_id,device_id,database_epoch,audience,expires_at,scopes) VALUES($1,$2,$3,$4,$5,$6,$7)',
    [hash, owner, primary.device_id, primary.database_epoch, 'read', Date.now() + 10 * 60_000, 'history:read']);
  const t0 = Date.now();
  let attempt = 0, seen = 0;
  while (true) {
    attempt++;
    const result = await rpc('read_session', {session_id: session, limit: 6});
    const s = summarize(result);
    seen = Math.max(0, ...s.messages.map(m => m.order));
    console.log(JSON.stringify({t_s: ((Date.now() - t0) / 1000).toFixed(1), attempt, newest_order: seen, ...s}));
    if (seen >= want || Date.now() - t0 > budgetMs) break;
    await new Promise(r => setTimeout(r, 3000));
  }
  console.log(JSON.stringify({verdict: seen >= want ? 'VISIBLE' : 'NOT_VISIBLE_WITHIN_BUDGET', want_order: want, newest_order: seen, elapsed_s: ((Date.now() - t0) / 1000).toFixed(1)}));
} finally {
  if (hash && db) try { await db.query('DELETE FROM av_credentials WHERE token_hash=$1', [hash]); console.log('credential removed'); } catch { console.log('CREDENTIAL CLEANUP FAILED'); process.exitCode = 1; }
  await fetch(base + '/revoke', {method: 'POST', body: new URLSearchParams({token: reader}), signal: AbortSignal.timeout(30_000)}).catch(() => {});
  if (db) await db.close();
}
