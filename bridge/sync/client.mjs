import { canonical, parseBatch, SyncError } from './projection.mjs';

/**
 * transport POSTs ONE persisted batch. Journal is private local durable storage.
 * The caller must persist the projected batch before calling this function.
 * This layer never reads raw OpenCode JSON and never writes source SQLite.
 */
export async function sendPending({ journal, fetch: transport = fetch, endpoint, token }) {
  const url = new URL(endpoint);
  if (url.protocol !== 'https:' || url.username || url.password || url.search || url.hash) throw new SyncError('https_endpoint_required', 400);
  const pending = await journal.pending();
  if (!pending) return null; // No HTTP or DB traffic when idle.
  const batch = parseBatch(pending);
  const response = await transport(new URL('/v1/sync/batches', url), {
    method: 'POST', redirect: 'error',
    headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
    body: canonical(batch), signal: AbortSignal.timeout(30_000),
  });
  if (!response.ok) throw new SyncError(response.status === 409 ? 'sync_conflict' : 'sync_upload_failed', response.status);
  let receipt;
  try { receipt = await response.json(); } catch { throw new SyncError('invalid_sync_receipt', 502); }
  const committed = batch.upload_id !== undefined ? batch.complete : true;
  if (receipt.batch_id !== batch.batch_id || receipt.session_id !== batch.session_id || receipt.committed !== committed
    || receipt.through !== (committed ? batch.through : null)
    || receipt.next_chunk !== (committed ? null : batch.chunk + 1)) throw new SyncError('invalid_sync_receipt', 502);
  // Implementations atomically save the ACK/cursor and remove the pending batch.
  // If this write fails, leave the same batch queued; its server receipt is idempotent.
  await journal.ack(batch.batch_id, receipt);
  return receipt;
}
