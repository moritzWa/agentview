-- Separate private database. Never apply to OpenCode's SQLite or the public probe.
CREATE TABLE IF NOT EXISTS av_credentials (
  token_hash text PRIMARY KEY,
  owner_id text NOT NULL,
  device_id text NOT NULL,
  database_epoch text NOT NULL,
  audience text NOT NULL CHECK (audience IN ('sync', 'read')),
  expires_at bigint NOT NULL,
  revoked boolean NOT NULL DEFAULT false
);
CREATE TABLE IF NOT EXISTS av_sessions (
  owner_id text NOT NULL, device_id text NOT NULL, database_epoch text NOT NULL,
  session_id text NOT NULL,
  generation text, cursor bigint,
  state jsonb NOT NULL,
  synced_at timestamptz,
  PRIMARY KEY (owner_id, device_id, database_epoch, session_id)
);
CREATE TABLE IF NOT EXISTS av_baselines (
  owner_id text NOT NULL, device_id text NOT NULL, database_epoch text NOT NULL,
  session_id text NOT NULL, upload_id text NOT NULL,
  next_chunk bigint NOT NULL, through_seq bigint NOT NULL, state_hash text NOT NULL,
  from_seq bigint NOT NULL DEFAULT 0,
  kind text NOT NULL DEFAULT 'baseline',
  PRIMARY KEY (owner_id, device_id, database_epoch, session_id)
);
CREATE TABLE IF NOT EXISTS av_messages (
  owner_id text NOT NULL, device_id text NOT NULL, database_epoch text NOT NULL,
  session_id text NOT NULL, generation text NOT NULL,
  message_id text NOT NULL, message_order bigint NOT NULL,
  role text NOT NULL CHECK (role IN ('user', 'assistant')),
  text text NOT NULL, created_at bigint NOT NULL, updated_at bigint NOT NULL,
  PRIMARY KEY (owner_id, device_id, database_epoch, session_id, generation, message_id),
  UNIQUE (owner_id, device_id, database_epoch, session_id, generation, message_order)
);
CREATE TABLE IF NOT EXISTS av_receipts (
  owner_id text NOT NULL, device_id text NOT NULL, database_epoch text NOT NULL,
  batch_id text NOT NULL, payload_hash text NOT NULL, receipt jsonb NOT NULL,
  PRIMARY KEY (owner_id, device_id, database_epoch, batch_id)
);
CREATE TABLE IF NOT EXISTS av_staged_deletes (
  owner_id text NOT NULL, device_id text NOT NULL, database_epoch text NOT NULL,
  session_id text NOT NULL, generation text NOT NULL, message_id text NOT NULL,
  PRIMARY KEY(owner_id,device_id,database_epoch,session_id,generation,message_id)
);
-- Baseline staging is never searchable. Only the active generation is exposed.
-- Search is deliberately bounded and literal in the first slice; FTS comes later.
