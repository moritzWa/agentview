ALTER TABLE av_credentials ADD COLUMN IF NOT EXISTS scopes text NOT NULL DEFAULT 'history:read';
ALTER TABLE av_credentials ADD COLUMN IF NOT EXISTS family text;
ALTER TABLE av_baselines ADD COLUMN IF NOT EXISTS from_seq bigint NOT NULL DEFAULT 0;
ALTER TABLE av_baselines ADD COLUMN IF NOT EXISTS kind text NOT NULL DEFAULT 'baseline';
CREATE INDEX IF NOT EXISTS av_messages_search ON av_messages USING gin(to_tsvector('simple',text));
CREATE TABLE IF NOT EXISTS av_owner_source (
  owner_id text PRIMARY KEY, device_id text NOT NULL, database_epoch text NOT NULL
);
-- This private app is owner-wide history access, not per-device read consent.
-- Existing owner links should follow a newly paired primary Mac without re-login.
UPDATE av_credentials SET device_id='*',database_epoch='*' WHERE audience='read' AND owner_id='wallawitsch@gmail.com';
CREATE TABLE IF NOT EXISTS av_oauth_clients (
  client_id text PRIMARY KEY, redirect_uris jsonb NOT NULL
);
CREATE TABLE IF NOT EXISTS av_auth_flows (
  state_hash text PRIMARY KEY, browser_hash text NOT NULL,
  nonce text NOT NULL, google_verifier text NOT NULL,
  payload jsonb NOT NULL, expires_at bigint NOT NULL
);
CREATE TABLE IF NOT EXISTS av_consents (
  code_hash text PRIMARY KEY, browser_hash text NOT NULL,
  payload jsonb NOT NULL, expires_at bigint NOT NULL
);
CREATE TABLE IF NOT EXISTS av_oauth_codes (
  code_hash text PRIMARY KEY, client_id text NOT NULL, redirect_uri text NOT NULL,
  challenge text NOT NULL, scopes text NOT NULL, resource text NOT NULL,
  owner_id text NOT NULL, device_id text NOT NULL, database_epoch text NOT NULL,
  expires_at bigint NOT NULL
);
CREATE TABLE IF NOT EXISTS av_refresh (
  token_hash text PRIMARY KEY, family text NOT NULL, client_id text NOT NULL,
  owner_id text NOT NULL, device_id text NOT NULL, database_epoch text NOT NULL,
  scopes text NOT NULL, resource text NOT NULL, expires_at bigint NOT NULL,
  consumed boolean NOT NULL DEFAULT false, revoked boolean NOT NULL DEFAULT false
);
CREATE INDEX IF NOT EXISTS av_refresh_family ON av_refresh(family);
CREATE TABLE IF NOT EXISTS av_pairings (
  code_hash text PRIMARY KEY, user_code text UNIQUE NOT NULL,
  device_id text NOT NULL, database_epoch text NOT NULL, label text NOT NULL,
  approved boolean NOT NULL DEFAULT false, expires_at bigint NOT NULL
);
CREATE TABLE IF NOT EXISTS av_outbox (
  owner_id text NOT NULL, device_id text NOT NULL, database_epoch text NOT NULL,
  operation_id text NOT NULL, session_id text NOT NULL, message_id text NOT NULL,
  text text NOT NULL, payload_hash text NOT NULL, base_cursor bigint NOT NULL,
  sequence bigint GENERATED ALWAYS AS IDENTITY,
  state text NOT NULL DEFAULT 'queued', created_at bigint NOT NULL, expires_at bigint NOT NULL,
  lease_until bigint, receipt jsonb,
  PRIMARY KEY (owner_id,device_id,database_epoch,operation_id)
);
CREATE INDEX IF NOT EXISTS av_outbox_pending ON av_outbox(owner_id,device_id,database_epoch,state,sequence);
ALTER TABLE av_outbox ADD COLUMN IF NOT EXISTS context_guard jsonb;
-- Explicitly approved artifact bytes only, separate from text-history sync.
CREATE TABLE IF NOT EXISTS av_artifacts (
  owner_id text NOT NULL, device_id text NOT NULL, database_epoch text NOT NULL,
  artifact_id text NOT NULL, session_id text NOT NULL, title text NOT NULL,
  name text NOT NULL, mime_type text NOT NULL, blob text NOT NULL,
  sha256 text NOT NULL, byte_length integer NOT NULL CHECK (byte_length BETWEEN 1 AND 512000),
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY(owner_id,device_id,database_epoch,artifact_id)
);
CREATE INDEX IF NOT EXISTS av_artifacts_session ON av_artifacts(owner_id,device_id,database_epoch,session_id);
-- A start has no session until the Mac creates one. session_id stays null until then.
CREATE TABLE IF NOT EXISTS av_starts (
  owner_id text NOT NULL, device_id text NOT NULL, database_epoch text NOT NULL,
  operation_id text NOT NULL, directory text NOT NULL, text text NOT NULL,
  payload_hash text NOT NULL, sequence bigint GENERATED ALWAYS AS IDENTITY,
  state text NOT NULL DEFAULT 'queued', session_id text,
  created_at bigint NOT NULL, expires_at bigint NOT NULL,
  lease_until bigint, receipt jsonb,
  PRIMARY KEY (owner_id,device_id,database_epoch,operation_id)
);
CREATE INDEX IF NOT EXISTS av_starts_pending ON av_starts(owner_id,device_id,database_epoch,state,sequence);
