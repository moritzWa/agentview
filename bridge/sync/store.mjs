import { digest, parseBatch, projectionText, SyncError } from './projection.mjs';

const key = p => [p.owner_id, p.device_id, p.database_epoch];
const predicate = 'owner_id=$1 AND device_id=$2 AND database_epoch=$3';
const sessionPredicate = `${predicate} AND session_id=$4`;

/** db.query(text, params) + db.transaction(callback), with an exclusive PG client per transaction. */
export class SyncStore {
  constructor(db) { this.db = db; }

  async authenticate(token, audience, now = Date.now()) {
    if (typeof token !== 'string' || !/^[A-Za-z0-9_-]{43,128}$/.test(token)) throw new SyncError('unauthorized', 401);
    const tokenHash = await digest(token);
    const { rows } = await this.db.query('SELECT * FROM av_credentials WHERE token_hash=$1 AND audience=$2 AND revoked=false AND expires_at>$3', [tokenHash, audience, now]);
    if (!rows.length) throw new SyncError('unauthorized', 401);
    const principal=rows[0];
    // Owner OAuth links follow the currently paired primary source. Connector
    // credentials remain bound to one exact device/epoch and never get widened.
    if(audience==='read' && principal.device_id==='*' && principal.database_epoch==='*') {
      const source=(await this.db.query('SELECT device_id,database_epoch FROM av_owner_source WHERE owner_id=$1',[principal.owner_id])).rows[0];
      if(!source)throw new SyncError('source_not_paired',409);
      return {...principal,...source};
    }
    return principal;
  }

  async apply(principal, input) {
    const batch = parseBatch(input), hash = await digest(batch);
    return this.db.transaction(async tx => {
      // Check live credential INSIDE the transaction, not only in the HTTP middleware.
      // FOR SHARE serializes a revocation UPDATE with an already-admitted batch.
      const credential = await tx.query('SELECT token_hash FROM av_credentials WHERE token_hash=$1 AND owner_id=$2 AND device_id=$3 AND database_epoch=$4 AND audience=$5 AND revoked=false AND expires_at>$6 FOR SHARE',
        [principal.token_hash, ...key(principal), 'sync', Date.now()]);
      if (!credential.rows.length) throw new SyncError('unauthorized', 401);
      const scope = key(principal), sk = [...scope, batch.session_id];
      // Consistent lock order also serializes repeated batch IDs across sessions.
      await tx.query('SELECT pg_advisory_xact_lock(hashtextextended($1, 0))', [JSON.stringify([...scope, batch.batch_id])]);
      const old = await tx.query(`SELECT payload_hash,receipt FROM av_receipts WHERE ${predicate} AND batch_id=$4`, [...scope, batch.batch_id]);
      if (old.rows.length) {
        if (old.rows[0].payload_hash !== hash) throw new SyncError('batch_id_conflict');
        return old.rows[0].receipt;
      }
      await tx.query(`INSERT INTO av_sessions (owner_id,device_id,database_epoch,session_id,state) VALUES ($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING`, [...sk, JSON.stringify(batch.session)]);
      const current = (await tx.query(`SELECT * FROM av_sessions WHERE ${sessionPredicate} FOR UPDATE`, sk)).rows[0];
      let generation = current.generation;
      const chunked = batch.upload_id !== undefined;
      let staged = (await tx.query(`SELECT * FROM av_baselines WHERE ${sessionPredicate}`, sk)).rows[0];
      const replacing = batch.kind === 'replace';
      if (batch.kind === 'baseline') {
        if (current.cursor !== null) throw new SyncError('baseline_already_committed');
      } else if (replacing) {
        // A full replacement is how a restored journal heals. It does not require
        // the cursors to match, and it must not hide the copy people can read.
        if (current.state.deleted) throw new SyncError('session_deleted');
      } else {
        if (current.cursor === null || Number(current.cursor) !== batch.from) throw new SyncError('cursor_gap');
        if (current.state.deleted) throw new SyncError('session_deleted');
      }
      if (replacing && batch.chunk === 0 && staged && staged.upload_id !== batch.upload_id) {
        await tx.query(`DELETE FROM av_messages WHERE ${sessionPredicate} AND generation=$5`, [...sk, staged.upload_id]);
        await tx.query(`DELETE FROM av_staged_deletes WHERE ${sessionPredicate} AND generation=$5`, [...sk, staged.upload_id]);
        await tx.query(`DELETE FROM av_baselines WHERE ${sessionPredicate}`, sk);
        staged = undefined;
      }
      if (chunked) {
        const stateHash = await digest(batch.session);
        if (batch.upload_id === current.generation) throw new SyncError('generation_conflict');
        if (!staged) {
          if (batch.chunk !== 0) throw new SyncError('baseline_chunk_gap');
          await tx.query('INSERT INTO av_baselines (owner_id,device_id,database_epoch,session_id,upload_id,next_chunk,through_seq,state_hash,from_seq,kind) VALUES ($1,$2,$3,$4,$5,0,$6,$7,$8,$9)', [...sk, batch.upload_id, batch.through, stateHash,batch.from,batch.kind]);
        } else if (staged.upload_id !== batch.upload_id || Number(staged.next_chunk) !== batch.chunk || Number(staged.through_seq) !== batch.through || staged.state_hash !== stateHash || Number(staged.from_seq)!==batch.from || staged.kind!==batch.kind) {
          throw new SyncError('baseline_chunk_conflict');
        }
        generation = batch.upload_id;
      } else if (staged) throw new SyncError('upload_in_progress');
      if (batch.messages.length) {
        await tx.query(`INSERT INTO av_messages (owner_id,device_id,database_epoch,session_id,generation,message_id,message_order,role,text,created_at,updated_at)
          SELECT $1,$2,$3,$4,$5,x.id,x."order",x.role,x.text,x.created_at,x.updated_at
          FROM jsonb_to_recordset($6::jsonb) AS x(id text,"order" bigint,role text,text text,created_at bigint,updated_at bigint)
          ON CONFLICT (owner_id,device_id,database_epoch,session_id,generation,message_id)
          DO UPDATE SET message_order=EXCLUDED.message_order,role=EXCLUDED.role,text=EXCLUDED.text,created_at=EXCLUDED.created_at,updated_at=EXCLUDED.updated_at`,
        [...sk, generation, JSON.stringify(batch.messages)]);
      }
      for (const id of batch.delete_ids) {
        if (chunked) await tx.query('INSERT INTO av_staged_deletes VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING',[...sk,generation,id]);
        else await tx.query(`DELETE FROM av_messages WHERE ${sessionPredicate} AND generation=$5 AND message_id=$6`, [...sk, generation, id]);
      }
      const committed = chunked ? batch.complete : true;
      if (committed) {
        if (replacing) {
          const previous = current.generation;
          generation = batch.upload_id;
          if (previous && previous !== generation) {
            await tx.query(`DELETE FROM av_messages WHERE ${sessionPredicate} AND generation=$5`, [...sk, previous]);
            await tx.query(`DELETE FROM av_staged_deletes WHERE ${sessionPredicate} AND generation=$5`, [...sk, previous]);
          }
        } else if (chunked && batch.kind==='delta') {
          const deletes=(await tx.query(`SELECT message_id FROM av_staged_deletes WHERE ${sessionPredicate} AND generation=$5`,[...sk,generation])).rows;
          for (const {message_id} of deletes) await tx.query(`DELETE FROM av_messages WHERE ${sessionPredicate} AND generation=$5 AND message_id=$6`,[...sk,current.generation,message_id]);
          await tx.query(`INSERT INTO av_messages SELECT owner_id,device_id,database_epoch,session_id,$6,message_id,message_order,role,text,created_at,updated_at FROM av_messages
            WHERE ${sessionPredicate} AND generation=$5
            ON CONFLICT (owner_id,device_id,database_epoch,session_id,generation,message_id) DO UPDATE SET message_order=EXCLUDED.message_order,role=EXCLUDED.role,text=EXCLUDED.text,created_at=EXCLUDED.created_at,updated_at=EXCLUDED.updated_at`,[...sk,generation,current.generation]);
          await tx.query(`DELETE FROM av_messages WHERE ${sessionPredicate} AND generation=$5`,[...sk,generation]);
          await tx.query(`DELETE FROM av_staged_deletes WHERE ${sessionPredicate} AND generation=$5`,[...sk,generation]);
          generation=current.generation;
        }
        if (batch.session.deleted) await tx.query(`DELETE FROM av_messages WHERE ${sessionPredicate}`,sk);
        await tx.query(`UPDATE av_sessions SET generation=$5,cursor=$6,state=$7,synced_at=now() WHERE ${sessionPredicate}`, [...sk, generation, batch.through, JSON.stringify(batch.session)]);
        await tx.query(`DELETE FROM av_baselines WHERE ${sessionPredicate}`, sk);
      } else {
        await tx.query(`UPDATE av_baselines SET next_chunk=$5 WHERE ${sessionPredicate}`, [...sk, batch.chunk + 1]);
      }
      const receipt = { batch_id: batch.batch_id, session_id: batch.session_id, committed, through: committed ? batch.through : null, next_chunk: committed ? null : batch.chunk + 1 };
      await tx.query('INSERT INTO av_receipts VALUES ($1,$2,$3,$4,$5,$6)', [...scope, batch.batch_id, hash, JSON.stringify(receipt)]);
      return receipt;
    });
  }

  /** Cursor compare only. A mismatch is a session whose cloud pointer is not the Mac's. */
  async cursors(principal, input) {
    if (principal.audience !== 'sync') throw new SyncError('forbidden', 403);
    const sessions = input?.sessions;
    if (!Array.isArray(sessions) || sessions.length > 4000) throw new SyncError('invalid_cursors', 400);
    const ids = [...new Set(sessions.filter(s => typeof s?.session_id === 'string' && s.session_id.length <= 200).map(s => s.session_id))];
    const rows = ids.length ? (await this.db.query(`SELECT session_id, cursor FROM av_sessions WHERE ${predicate} AND session_id = ANY($4::text[])`, [...key(principal), ids])).rows : [];
    const cloud = new Map(rows.map(r => [r.session_id, r.cursor === null || r.cursor === undefined ? null : Number(r.cursor)]));
    const mismatch = [];
    for (const s of sessions) {
      if (typeof s?.session_id !== 'string' || !Number.isSafeInteger(s.cursor)) continue;
      if (cloud.get(s.session_id) !== s.cursor) mismatch.push(s.session_id);
    }
    return {mismatch};
  }

  /** One session, hashes only. The old copy stays the one reads see until a replace commits. */
  async verify(principal, input) {
    if (principal.audience !== 'sync') throw new SyncError('forbidden', 403);
    const sessionId = input?.session_id;
    if (typeof sessionId !== 'string' || sessionId.length > 200 || typeof input?.hash !== 'string' || !/^[a-f0-9]{64}$/.test(input.hash)) throw new SyncError('invalid_verify', 400);
    const rows = (await this.db.query(`SELECT s.cursor, s.state, m.message_id, m.message_order, m.role, m.text
      FROM av_sessions s LEFT JOIN av_messages m ON m.owner_id=s.owner_id AND m.device_id=s.device_id AND m.database_epoch=s.database_epoch
      AND m.session_id=s.session_id AND m.generation=s.generation
      WHERE ${sessionPredicate} AND s.cursor IS NOT NULL AND (s.state->>'deleted')::boolean=false
      ORDER BY m.message_order, m.message_id`, [...key(principal), sessionId])).rows;
    if (!rows.length) return {session_id: sessionId, match: false, reason: 'missing'};
    const state = rows[0].state, messages = rows.filter(r => r.message_id).map(r => ({id: r.message_id, order: Number(r.message_order), role: r.role, text: r.text}));
    const cloud_hash = await digest(projectionText(Number(rows[0].cursor), state, messages));
    return {session_id: sessionId, match: cloud_hash === input.hash, cloud_cursor: Number(rows[0].cursor), cloud_count: messages.length, reason: cloud_hash === input.hash ? 'match' : 'differs'};
  }

  /** No search/reader API can see incomplete baselines or undone messages. */
  async read(principal, sessionId, { query = '', after = -1, before = Number.MAX_SAFE_INTEGER, tail = false, limit = 50 } = {}) {
    if (principal.audience !== 'read') throw new SyncError('forbidden', 403);
    if (typeof query !== 'string' || query.length > 200 || !Number.isSafeInteger(after) || after < -1 || !Number.isSafeInteger(limit) || limit < 1 || limit > 100) throw new SyncError('invalid_read', 400);
    if(!Number.isSafeInteger(before)||before<0||typeof tail!=='boolean')throw new SyncError('invalid_read',400);
    const sk = [...key(principal), sessionId];
    // Literal substring match, NOT SQL LIKE patterns. Returned rows remain source ordered.
    // One statement means the revert boundary and message rows share one MVCC snapshot.
    const result = await this.db.query(`SELECT s.cursor,s.state,s.synced_at,
      COALESCE((SELECT jsonb_agg(x ORDER BY x.message_order) FROM (
        SELECT message_id,role,message_order,text,created_at,updated_at FROM av_messages m
        WHERE m.owner_id=s.owner_id AND m.device_id=s.device_id AND m.database_epoch=s.database_epoch
        AND m.session_id=s.session_id AND m.generation=s.generation AND m.message_order>$5 AND m.message_order<$10
        AND ((s.state->>'visible_before') IS NULL OR m.message_order<(s.state->>'visible_before')::bigint)
        AND ($6='' OR strpos(lower(m.text),lower($6))>0)
        ORDER BY m.message_order ${tail?'DESC':'ASC'} LIMIT $7
      ) x),'[]'::jsonb) AS messages FROM av_sessions s
      WHERE ${sessionPredicate} AND cursor IS NOT NULL AND (state->>'deleted')::boolean=false
      AND EXISTS (SELECT 1 FROM av_credentials c WHERE c.token_hash=$8 AND c.owner_id=s.owner_id
        AND (c.device_id=s.device_id OR c.device_id='*') AND (c.database_epoch=s.database_epoch OR c.database_epoch='*') AND c.audience='read'
        AND c.revoked=false AND c.expires_at>$9)`, [...sk, after, query, limit, principal.token_hash, Date.now(),before]);
    if (!result.rows.length) throw new SyncError('session_not_found', 404);
    return result.rows[0];
  }
}
