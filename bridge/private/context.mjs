import { z } from 'zod';
import { SyncError } from '../sync/projection.mjs';

// A consistency check, not authorization. Native permissions and the write grant
// remain independent. Preserve the already-read bytes of the latest visible
// message: streaming suffixes and appended turns do not invalidate a reply.
export const ReadContext=z.object({version:z.literal(1),anchor_order:z.number().int().min(-1),anchor_bytes:z.number().int().min(0).max(1_000_000),prefix_hash:z.string().regex(/^[a-f0-9]{64}$/),visible_before:z.number().int().min(0).nullable()}).strict();
export async function readContext(db,principal,sessionId,anchor=null) {
  const row=(await db.query(`WITH target AS (
      SELECT * FROM av_sessions WHERE owner_id=$1 AND device_id=$2 AND database_epoch=$3 AND session_id=$4 AND cursor IS NOT NULL AND (state->>'deleted')::boolean=false
    ), history AS (
      SELECT m.* FROM av_messages m JOIN target s USING(owner_id,device_id,database_epoch,session_id)
      WHERE m.generation=s.generation AND ((s.state->>'visible_before') IS NULL OR m.message_order<(s.state->>'visible_before')::bigint)
    ), anchor_id AS (SELECT COALESCE($5::bigint,(SELECT max(message_order) FROM history),-1) AS n),
    anchor AS (SELECT n,COALESCE($6::bigint,(SELECT octet_length(text) FROM history WHERE message_order=n LIMIT 1),0) AS bytes FROM anchor_id)
    SELECT anchor.n AS anchor_order,anchor.bytes AS anchor_bytes,target.state->'visible_before' AS visible_before,
      encode(sha256(convert_to(COALESCE((SELECT string_agg(
        encode(sha256(convert_to(octet_length(message_id)::text||':'||message_id||':'||message_order::text||':'||role||':'||encode(sha256(CASE WHEN message_order=anchor.n THEN substring(convert_to(text,'UTF8') FROM 1 FOR anchor.bytes::int) ELSE convert_to(text,'UTF8') END),'hex'),'UTF8')),'hex'),'' ORDER BY message_order,message_id)
        FROM history WHERE message_order<=anchor.n),''),'UTF8')),'hex') AS prefix_hash
    FROM target CROSS JOIN anchor`,[principal.owner_id,principal.device_id,principal.database_epoch,sessionId,anchor?.anchor_order??null,anchor?.anchor_bytes??null])).rows[0];
  if(!row)throw new SyncError('session_not_found',404);
  return {version:1,anchor_order:Number(row.anchor_order),anchor_bytes:Number(row.anchor_bytes),prefix_hash:row.prefix_hash,visible_before:row.visible_before??null};
}
export function sameContext(a,b) {
  return a?.version===1 && a.anchor_order===b.anchor_order && a.anchor_bytes===b.anchor_bytes && a.prefix_hash===b.prefix_hash && a.visible_before===b.visible_before;
}
