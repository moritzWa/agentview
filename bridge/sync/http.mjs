import { SyncError } from './projection.mjs';

const MAX_BYTES = 1_000_000;
const headers = { 'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff' };
function reply(body, status = 200) { return Response.json(body, { status, headers }); }

async function readJson(request) {
  if (!request.headers.get('content-type')?.toLowerCase().startsWith('application/json')) throw new SyncError('json_required', 415);
  if (Number(request.headers.get('content-length')) > MAX_BYTES) throw new SyncError('batch_too_large', 413);
  const reader = request.body?.getReader();
  if (!reader) throw new SyncError('invalid_json', 400);
  let length = 0;
  const chunks = [];
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      length += value.length;
      if (length > MAX_BYTES) { await reader.cancel(); throw new SyncError('batch_too_large', 413); }
      chunks.push(value);
    }
  } finally { reader.releaseLock(); }
  const bytes = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
  try { return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes)); }
  catch { throw new SyncError('invalid_json', 400); }
}

/** Authenticated private sync/reader routes, never part of the public MCP probe. */
export function privateHandler(store) {
  return async request => {
    const url = new URL(request.url);
    const write = url.pathname === '/v1/sync/batches' && request.method === 'POST';
    const match = url.pathname.match(/^\/v1\/sessions\/([^/]+)\/messages$/);
    const read = !!match && request.method === 'GET';
    if (!write && !read) return reply({ error: 'not_found' }, 404);
    if (!store) return reply({ error: 'not_configured' }, 503);
    try {
      const auth = request.headers.get('authorization') ?? '';
      const token = auth.startsWith('Bearer ') ? auth.slice(7) : '';
      const principal = await store.authenticate(token, write ? 'sync' : 'read');
      if (write) return reply(await store.apply(principal, await readJson(request)));
      const known = new Set(['q', 'after', 'limit']);
      if ([...url.searchParams.keys()].some(k => !known.has(k))) throw new SyncError('invalid_read', 400);
      let session;
      try { session = decodeURIComponent(match[1]); } catch { throw new SyncError('invalid_read', 400); }
      if (session.length > 200) throw new SyncError('invalid_read', 400);
      return reply(await store.read(principal, session, {
        query: url.searchParams.get('q') ?? '',
        after: url.searchParams.has('after') ? Number(url.searchParams.get('after')) : -1,
        limit: url.searchParams.has('limit') ? Number(url.searchParams.get('limit')) : 50,
      }));
    } catch (error) {
      // No raw errors, SQL, validation input, tokens, URLs or bodies in responses/logs.
      return error instanceof SyncError ? reply({ error: error.code }, error.status) : reply({ error: 'internal_error' }, 500);
    }
  };
}
