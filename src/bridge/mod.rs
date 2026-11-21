//! Opt-in private mobile history bridge. The dashboard does not own its lifecycle.
mod artifacts;
mod context;
mod health;
mod http;
mod journal;
mod runtime;
mod source;
mod start;
#[cfg(test)]
mod tests;
pub use artifacts::publish as publish_artifact;

use anyhow::{bail, Context, Result};
use journal::Journal;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub endpoint: String,
    pub device_id: String,
    pub database_epoch: String,
    pub database: PathBuf,
    pub opencode_bin: String,
    pub keychain_service: String,
    pub inode: u64,
    #[serde(default)]
    pub supervisor_state: Option<PathBuf>,
}
pub fn state_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is missing")?;
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(home).join(".local/state"));
    Ok(root.join("agentview/bridge"))
}
fn private_dir(path: &Path) -> Result<()> {
    if path.exists() && std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        bail!("bridge directory cannot be a symlink");
    }
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
pub fn random_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|_| anyhow::anyhow!("secure random source unavailable"))?;
    use std::fmt::Write;
    let mut encoded = String::with_capacity(32);
    for byte in bytes {
        write!(encoded, "{byte:02x}")?;
    }
    Ok(encoded)
}
pub fn hash(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}
fn inode(path: &Path) -> Result<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(std::fs::metadata(path)?.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(0)
    }
}
fn load(dir: &Path) -> Result<Config> {
    let config: Config = serde_json::from_slice(&std::fs::read(dir.join("config.json"))?)?;
    if config.version != 1 || config.inode != inode(&config.database)? {
        bail!("source database replaced; re-pair required");
    }
    Ok(config)
}
fn save_token(service: &str, token: &str) -> Result<()> {
    crate::usage::save_bridge_credential(service, token)
        .context("could not save dedicated bridge Keychain item")
}
fn token(config: &Config) -> Result<String> {
    crate::usage::read_bridge_credential(&config.keychain_service)
        .context("bridge Keychain credential unavailable")
}

pub fn pair(
    dir: &Path,
    endpoint: &str,
    bin: &str,
    database: Option<&Path>,
    supervisor_state: Option<&Path>,
) -> Result<()> {
    private_dir(dir)?;
    if dir.join("config.json").exists() {
        bail!("bridge is already paired; disconnect it before replacing the source");
    }
    let db_path = if let Some(path) = database {
        path.canonicalize()?
    } else {
        // Reuse the dashboard's executable/profile-aware, read-only source resolver.
        let (db, v2) = crate::adapters::opencode_db::open_read_only(bin)?;
        if !v2 {
            bail!("OpenCode V2 is required");
        }
        PathBuf::from(db.path().context("OpenCode database path unavailable")?)
    };
    let source = source::open(&db_path)?;
    drop(source);
    let device_id = random_id()?;
    let epoch = random_id()?;
    let started = http::request(
        endpoint,
        "/pair/start",
        None,
        Some(
            &json!({"device_id":device_id,"database_epoch":epoch,"label":std::env::var("USER").unwrap_or_else(|_|"Mac".into())+" Agent View"}),
        ),
        30,
    )?;
    let code = started["device_code"]
        .as_str()
        .context("missing pairing code")?;
    println!(
        "Approve this device in your browser:\n{}",
        started["verification_uri"]
            .as_str()
            .context("missing approval URL")?
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(600);
    loop {
        match http::request(
            endpoint,
            "/pair/token",
            None,
            Some(&json!({"device_code":code})),
            30,
        ) {
            Ok(response) => {
                let credential = response["access_token"]
                    .as_str()
                    .context("missing connector credential")?;
                let service = format!("agentview-bridge-{device_id}");
                save_token(&service, credential)?;
                let config = Config {
                    version: 1,
                    endpoint: endpoint.to_owned(),
                    device_id,
                    database_epoch: epoch,
                    database: db_path.clone(),
                    opencode_bin: bin.to_owned(),
                    keychain_service: service,
                    inode: inode(&db_path)?,
                    supervisor_state: supervisor_state.map(Path::to_path_buf),
                };
                crate::fs_util::write_private_json(&dir.join("config.json"), &config)?;
                println!("Paired. Credential stored in a dedicated Keychain item; no history uploaded yet.");
                return Ok(());
            }
            Err(e)
                if e.to_string() == "bridge HTTP 428" && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_secs(5))
            }
            Err(_) => bail!("pairing failed or expired; retry pairing"),
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Health {
    last_ok: u64,
    failing: Vec<Failure>,
    #[serde(default)]
    last_pass_at: u64,
    #[serde(default)]
    last_pass_ms: u64,
    #[serde(default)]
    scanned: usize,
    #[serde(default)]
    uploaded_batches: usize,
}

#[derive(Clone, Serialize, Deserialize)]
struct Failure {
    session_id: String,
    stage: String,
    reason: String,
    since: u64,
    last_try: u64,
    count: u32,
}

/// Seconds to wait before retrying a session that failed `count` times in a row.
/// Short at first so a transient blip on a session in active use heals within a
/// pass or two; long later so a permanently bad session costs almost nothing.
fn retry_after(count: u32) -> u64 {
    match count {
        0 | 1 => 5,
        2 => 15,
        3 => 30,
        4 => 60,
        5 => 120,
        _ => 300,
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn read_health(dir: &Path) -> Health {
    std::fs::read(dir.join("sync-health.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// What an upload failure tells us about whether the cloud applied the batch.
#[derive(Debug, PartialEq)]
enum Fault {
    /// The cloud refused the batch inside its transaction, so nothing was
    /// committed. Retrying the same bytes can never succeed.
    Definite,
    /// The cloud may have committed before it failed or before we saw the receipt.
    /// The same batch must be retried verbatim: the cloud replays its stored
    /// receipt, whereas a rebuilt batch would fail `cursor_gap` forever.
    Ambiguous,
}

fn http_status(message: &str) -> Option<u16> {
    message
        .strip_prefix("bridge HTTP ")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// A refused delta or baseline means the pointers disagree. A full replace heals it.
fn needs_replace(error: &anyhow::Error) -> bool {
    let text = error.to_string();
    text.contains("cursor_gap") || text.contains("baseline_already_committed")
}

fn classify(error: &anyhow::Error) -> Option<Fault> {
    let message = error.to_string();
    if let Some(status) = http_status(&message) {
        return match status {
            400 | 409 | 413 | 422 => Some(Fault::Definite),
            500..=599 => Some(Fault::Ambiguous),
            _ => None,
        };
    }
    if message == "invalid sync acknowledgement" {
        return Some(Fault::Ambiguous);
    }
    None
}

/// Short, content-free description for the log and health file.
fn safe_reason(error: &anyhow::Error) -> String {
    let text = format!("{error:#}");
    if text.contains('"') || text.contains('`') || text.contains('/') {
        return "unreadable source data".into();
    }
    text.chars().take(160).collect()
}

type Faults = Vec<(String, &'static str, anyhow::Error)>;

/// Uploads queued batches. A failure that belongs to one session is set aside
/// (held or dropped, per `classify`) so it cannot block other sessions. Anything
/// else (network, auth, rate limit, local disk) is a pass-level error.
fn flush_isolating(journal: &mut Journal, config: &Config, token: &str) -> Result<(usize, Faults)> {
    let mut count = 0;
    let mut faults: Faults = Vec::new();
    while let Some(body) = journal.pending()? {
        let session = body["session_id"].as_str().unwrap_or_default().to_owned();
        let sent = http::request(
            &config.endpoint,
            "/v1/sync/batches",
            Some(token),
            Some(&body),
            60,
        )
        .and_then(|receipt| journal.ack(&receipt));
        match sent {
            Ok(()) => count += 1,
            Err(error) => match classify(&error) {
                Some(Fault::Definite) => {
                    journal.discard_pending(&session)?;
                    faults.push((session, "upload", error));
                }
                Some(Fault::Ambiguous) => {
                    journal.hold_pending(&session)?;
                    faults.push((session, "upload", error));
                }
                None => return Err(error),
            },
        }
    }
    Ok((count, faults))
}

/// Uploads the whole current projection beside the copy the cloud already serves.
/// Reads keep returning the old copy until the last chunk is confirmed.
fn replace_session(
    db: &mut rusqlite::Connection,
    journal: &mut Journal,
    config: &Config,
    credential: &str,
    id: &str,
) -> std::result::Result<usize, (&'static str, anyhow::Error)> {
    let native = source::native(db, id).map_err(|e| ("source", e))?;
    let Some(capture) = source::capture(db, id, None).map_err(|e| ("capture", e))? else {
        return Ok(0);
    };
    journal
        .stage_replace(id, capture, native)
        .map_err(|e| ("stage", e))?;
    let (n, faults) = flush_isolating(journal, config, credential).map_err(|e| ("transport", e))?;
    if let Some((_, stage, error)) = faults.into_iter().next() {
        return Err((stage, error));
    }
    Ok(n)
}

fn sync_session(
    db: &mut rusqlite::Connection,
    journal: &mut Journal,
    config: &Config,
    credential: &str,
    id: &str,
    uploads: &mut usize,
) -> std::result::Result<(), (&'static str, anyhow::Error)> {
    // A batch with an unknown outcome goes first, byte for byte.
    if journal.has_held(id).map_err(|e| ("checkpoint", e))? {
        journal.restore_held(id).map_err(|e| ("checkpoint", e))?;
        let (n, faults) =
            flush_isolating(journal, config, credential).map_err(|e| ("transport", e))?;
        *uploads += n;
        if let Some((_, stage, error)) = faults.into_iter().next() {
            if needs_replace(&error) {
                *uploads += replace_session(db, journal, config, credential, id)?;
                return Ok(());
            }
            return Err((stage, error));
        }
    }
    let old = journal.checkpoint(id).map_err(|e| ("checkpoint", e))?;
    let native = source::native(db, id).map_err(|e| ("source", e))?;
    if let Some(old) = &old {
        if old.native != native {
            return Err((
                "source",
                anyhow::anyhow!("source format changed; re-pair required"),
            ));
        }
        if source::watermark(db, id).map_err(|e| ("source", e))? == old.observed {
            return Ok(());
        }
    }
    let capture =
        source::capture(db, id, old.as_ref().map(|p| p.observed)).map_err(|e| ("capture", e))?;
    if let Some(capture) = capture {
        journal
            .stage(id, capture, native)
            .map_err(|e| ("stage", e))?;
        let (n, faults) =
            flush_isolating(journal, config, credential).map_err(|e| ("transport", e))?;
        *uploads += n;
        if let Some((_, stage, error)) = faults.into_iter().next() {
            if needs_replace(&error) {
                *uploads += replace_session(db, journal, config, credential, id)?;
                return Ok(());
            }
            return Err((stage, error));
        }
    }
    Ok(())
}

pub fn sync_once(dir: &Path) -> Result<usize> {
    let _sync_lock = local_lock(&dir.join("sync.lock"))?;
    let started = std::time::Instant::now();
    let config = load(dir)?;
    let credential = token(&config)?;
    let mut journal = Journal::open(&dir.join("journal.sqlite"))?;
    let previous = read_health(dir);
    let now = now_secs();
    let mut failing: Vec<Failure> = Vec::new();
    let note = |failing: &mut Vec<Failure>, id: &str, stage: &str, error: &anyhow::Error| {
        let before = previous.failing.iter().find(|f| f.session_id == id);
        let count = before.map_or(1, |f| f.count + 1);
        let reason = safe_reason(error);
        if count == 1 || count % 20 == 0 {
            eprintln!(
                "bridge session stuck session={id} stage={stage} attempts={count} reason={reason}"
            );
        }
        failing.retain(|f| f.session_id != id);
        failing.push(Failure {
            session_id: id.to_owned(),
            stage: stage.into(),
            reason,
            since: before.map_or(now, |f| f.since),
            last_try: now,
            count,
        });
    };
    let (mut uploads, queued_faults) = flush_isolating(&mut journal, &config, &credential)?;
    let mut db = source::open(&config.database)?;
    let ids = source::sessions(&db)?.into_iter().collect::<BTreeSet<_>>();
    for (id, stage, error) in queued_faults {
        if needs_replace(&error) && ids.contains(&id) {
            match replace_session(&mut db, &mut journal, &config, &credential, &id) {
                Ok(n) => uploads += n,
                Err(("transport", error)) => return Err(error),
                Err((stage, error)) => note(&mut failing, &id, stage, &error),
            }
        } else {
            note(&mut failing, &id, stage, &error);
        }
    }
    let mut server_errors = 0;
    for id in &ids {
        if failing.iter().any(|f| &f.session_id == id) {
            continue;
        }
        let before = previous.failing.iter().find(|f| &f.session_id == id);
        if let Some(f) = before {
            if now.saturating_sub(f.last_try) < retry_after(f.count) {
                failing.push(f.clone());
                continue;
            }
        }
        match sync_session(
            &mut db,
            &mut journal,
            &config,
            &credential,
            id,
            &mut uploads,
        ) {
            Ok(()) => server_errors = 0,
            Err(("transport", error)) => return Err(error),
            Err((stage, error)) => {
                // One session's batch can make the cloud answer 5xx every time,
                // but three in a row across sessions looks like an outage.
                if classify(&error) == Some(Fault::Ambiguous) {
                    server_errors += 1;
                    if server_errors >= 3 {
                        return Err(error);
                    }
                }
                note(&mut failing, id, stage, &error);
            }
        }
    }
    // A restored journal can look caught up locally while the cloud pointer differs.
    // Compare pointers and replace those sessions. A network failure here is not a stall.
    if let Ok(local) = journal.checkpoints() {
        if !local.is_empty() {
            let sessions: Vec<Value> = local
                .iter()
                .map(|(id, cursor)| json!({"session_id": id, "cursor": cursor}))
                .collect();
            match http::request(
                &config.endpoint,
                "/v1/sync/cursors",
                Some(&credential),
                Some(&json!({"sessions": sessions})),
                60,
            ) {
                Ok(report) => {
                    for id in report["mismatch"].as_array().cloned().unwrap_or_default() {
                        let Some(id) = id.as_str() else { continue };
                        if !ids.contains(id) || failing.iter().any(|f| f.session_id == id) {
                            continue;
                        }
                        match replace_session(&mut db, &mut journal, &config, &credential, id) {
                            Ok(n) => uploads += n,
                            Err(("transport", error)) => return Err(error),
                            Err((stage, error)) => note(&mut failing, id, stage, &error),
                        }
                    }
                }
                Err(error) => log_unavailable("cursors", &error),
            }
        }
    }
    for id in journal.known()? {
        if !ids.contains(&id) {
            if journal.has_held(&id)? {
                journal.restore_held(&id)?;
                flush_isolating(&mut journal, &config, &credential)?;
            }
            journal.tombstone(&id)?;
            let (n, faults) = flush_isolating(&mut journal, &config, &credential)?;
            uploads += n;
            for (id, stage, error) in &faults {
                note(&mut failing, id, stage, error);
            }
        }
    }
    let health = Health {
        last_ok: if failing.is_empty() {
            now
        } else {
            previous.last_ok
        },
        failing,
        last_pass_at: now,
        last_pass_ms: started.elapsed().as_millis() as u64,
        scanned: ids.len(),
        uploaded_batches: uploads,
    };
    let _ = crate::fs_util::write_private_json(&dir.join("sync-health.json"), &health);
    // A scan with stuck sessions must not stamp history as fresh.
    http::request(
        &config.endpoint,
        "/v1/connector/checked",
        Some(&credential),
        Some(
            &json!({"complete":health.failing.is_empty(),"skipped":health.failing.len(),"behind":health.failing.len(),"behind_since":health.failing.iter().map(|f| f.since).min().map(|s| s * 1000)}),
        ),
        30,
    )?;
    if let Err(error) = publish_runtime(dir, &config, &credential) {
        log_unavailable("runtime", &error);
    }
    Ok(uploads)
}

fn publish_runtime(dir: &Path, config: &Config, credential: &str) -> Result<()> {
    // The poll thread and the sync loop both publish. If the other one is mid-publish,
    // its result is the one the phone needs, so this call has nothing to add.
    let Some(_lock) = try_local_lock(&dir.join("runtime.lock"))? else {
        return Ok(());
    };
    let stamp = dir.join("runtime-checked.json");
    if std::fs::metadata(&stamp)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < Duration::from_secs(15))
    {
        return Ok(());
    }
    let started = std::time::Instant::now();
    // One quick retry: a native read can fail for a moment while OpenCode writes.
    let snapshot = match runtime::snapshot(config) {
        Ok(snapshot) => snapshot,
        Err(first) => {
            log_unavailable("runtime", &first);
            std::thread::sleep(Duration::from_secs(2));
            runtime::snapshot(config)?
        }
    };
    http::request(
        &config.endpoint,
        "/v1/connector/runtime",
        Some(credential),
        Some(&serde_json::to_value(snapshot)?),
        35,
    )?;
    crate::fs_util::write_private_json(&stamp, &json!({"checked":true}))?;
    if started.elapsed() > Duration::from_secs(3) {
        eprintln!(
            "bridge runtime slow ms={} time={}",
            started.elapsed().as_millis(),
            now_secs()
        );
    }
    Ok(())
}

fn already_admitted(
    db: &rusqlite::Connection,
    id: &str,
    session: &str,
    text: &str,
) -> Result<bool> {
    let known=db.query_row("SELECT type,json_extract(data,'$.text') FROM session_message WHERE id=?1 AND session_id=?2",rusqlite::params![id,session],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?))).optional()?;
    if let Some((kind, body)) = known {
        if kind != "user" || body.as_deref() != Some(text) {
            bail!("local input ID payload conflict");
        }
        return Ok(true);
    }
    let inbox=db.query_row("SELECT type,json_extract(payload,'$.text') FROM session_inbox WHERE id=?1 AND session_id=?2",rusqlite::params![id,session],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?))).optional()?;
    if let Some((kind, body)) = inbox {
        if kind != "user" || body.as_deref() != Some(text) {
            bail!("local input ID payload conflict");
        }
        return Ok(true);
    }
    Ok(false)
}

pub fn deliver_once(dir: &Path) -> Result<bool> {
    let _delivery_lock = local_lock(&dir.join("delivery.lock"))?;
    let config = load(dir)?;
    let credential = token(&config)?;
    let journal = Journal::open(&dir.join("journal.sqlite"))?;
    // A delivery interrupted after local admission must ACK the original lease,
    // not ask the cloud for a different operation and lose its durable ID.
    let pending = journal
        .db
        .query_row(
            "SELECT body FROM delivery ORDER BY rowid LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    let message = if let Some(body) = pending {
        serde_json::from_str::<Value>(&body)?
    } else {
        let lease = http::request(
            &config.endpoint,
            "/v1/connector/lease",
            Some(&credential),
            Some(&json!({})),
            30,
        )?;
        if lease["message"].is_null() || lease["message"].get("needs_confirmation").is_some() {
            return Ok(false);
        }
        let message = lease["message"].clone();
        journal.db.execute(
            "INSERT INTO delivery(operation_id,body) VALUES (?1,?2)",
            rusqlite::params![
                message["operation_id"].as_str().context("invalid lease")?,
                message.to_string()
            ],
        )?;
        message
    };
    let id = message["session_id"].as_str().context("invalid target")?;
    let mid = message["message_id"].as_str().context("invalid input ID")?;
    let text = message["text"].as_str().context("invalid input")?;
    let operation = message["operation_id"]
        .as_str()
        .context("invalid operation")?;
    let mut db = source::open(&config.database)?;
    let mut status = "accepted";
    if !already_admitted(&db, mid, id, text)? {
        if message["expires_at"].as_i64().unwrap_or(0)
            <= std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis() as i64
        {
            status = "failed";
        } else if let Some(capture) = source::capture(&mut db, id, None).unwrap_or_else(|error| {
            eprintln!(
                "bridge delivery cannot read session={id} reason={}",
                safe_reason(&error)
            );
            None
        }) {
            let consistent = if message["context"].is_object() {
                context::matches(&capture, &message["context"])
            } else {
                journal
                    .checkpoint(id)?
                    .is_some_and(|p| Some(p.cursor) == message["base_cursor"].as_i64())
                    && journal.source_matches(id, &capture)?
            };
            if !consistent {
                status = "needs_confirmation";
            } else {
                let supervisor = if let Some(state) = &config.supervisor_state {
                    crate::opencode_supervisor::OpenCodeSupervisor::with_state_dir(
                        config.opencode_bin.clone(),
                        state.clone(),
                    )?
                } else {
                    crate::opencode_supervisor::OpenCodeSupervisor::host(
                        config.opencode_bin.clone(),
                    )?
                };
                supervisor.queue_remote_reply(
                    id,
                    Path::new(&capture.cwd),
                    capture.state["title"].as_str().unwrap_or("Session"),
                    mid,
                    text,
                )?;
            }
        } else {
            status = "failed";
        }
    }
    http::request(
        &config.endpoint,
        "/v1/connector/ack",
        Some(&credential),
        Some(&json!({"operation_id":operation,"message_id":mid,"status":status})),
        30,
    )?;
    journal
        .db
        .execute("DELETE FROM delivery WHERE operation_id=?1", [operation])?;
    Ok(true)
}

fn ack_start(
    endpoint: &str,
    credential: &str,
    operation: &str,
    status: &str,
    session_id: Option<&str>,
    reason: &str,
) -> Result<()> {
    http::request(
        endpoint,
        "/v1/connector/start-ack",
        Some(credential),
        Some(
            &json!({"operation_id": operation, "status": status, "session_id": session_id, "reason": if reason.is_empty() { Value::Null } else { json!(reason) }}),
        ),
        30,
    )?;
    Ok(())
}

/// Creates at most one queued session. A local claim without an id is never launched again.
pub fn start_once(dir: &Path) -> Result<bool> {
    let _lock = local_lock(&dir.join("start.lock"))?;
    let config = load(dir)?;
    let credential = token(&config)?;
    let journal = Journal::open(&dir.join("journal.sqlite"))?;
    let pending = journal
        .db
        .query_row(
            "SELECT operation_id, session_id, prompt_accepted FROM start_op LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            },
        )
        .optional()?;
    if let Some((operation, session_id, accepted)) = pending {
        let (status, reason) = if let Some(session_id) = session_id.as_deref() {
            let status = if accepted == Some(1) {
                "started"
            } else {
                "created_not_started"
            };
            ack_start(
                &config.endpoint,
                &credential,
                &operation,
                status,
                Some(session_id),
                "",
            )?;
            start::finish(&journal.db, &operation)?;
            return Ok(true);
        } else {
            ("uncertain", "unknown")
        };
        ack_start(
            &config.endpoint,
            &credential,
            &operation,
            status,
            None,
            reason,
        )?;
        start::finish(&journal.db, &operation)?;
        return Ok(true);
    }
    let lease = http::request(
        &config.endpoint,
        "/v1/connector/start-lease",
        Some(&credential),
        Some(&json!({})),
        30,
    )?;
    if lease["start"].is_null() {
        return Ok(false);
    }
    let start = lease["start"].clone();
    let operation = start["operation_id"].as_str().context("invalid start")?;
    let directory = start["directory"]
        .as_str()
        .context("invalid start directory")?;
    let text = start["text"].as_str().context("invalid start prompt")?;
    let home = std::env::var_os("HOME").context("HOME is not set");
    let allowed = home
        .as_ref()
        .ok()
        .and_then(|home| start::allowed_directory(Path::new(directory), Path::new(home)).ok());
    let deny = if home.is_err() {
        Some("server")
    } else if allowed.is_none() {
        Some(
            start::allowed_directory(Path::new(directory), Path::new(home.as_ref().unwrap()))
                .unwrap_err(),
        )
    } else {
        None
    };
    if let Some(reason) = deny {
        ack_start(
            &config.endpoint,
            &credential,
            operation,
            "failed",
            None,
            reason,
        )?;
        return Ok(true);
    }
    let canonical = allowed.context("start directory")?;
    match start::claim(&journal.db, operation, &start.to_string())? {
        start::Claim::Uncertain => {
            ack_start(
                &config.endpoint,
                &credential,
                operation,
                "uncertain",
                None,
                "unknown",
            )?;
            start::finish(&journal.db, operation)?;
            return Ok(true);
        }
        start::Claim::Resume {
            session_id,
            prompt_accepted,
        } => {
            let status = if prompt_accepted {
                "started"
            } else {
                "created_not_started"
            };
            ack_start(
                &config.endpoint,
                &credential,
                operation,
                status,
                Some(&session_id),
                "",
            )?;
            start::finish(&journal.db, operation)?;
            return Ok(true);
        }
        start::Claim::Launch => {}
    }
    let supervisor = if let Some(state) = &config.supervisor_state {
        crate::opencode_supervisor::OpenCodeSupervisor::with_state_dir(
            config.opencode_bin.clone(),
            state.clone(),
        )?
    } else {
        crate::opencode_supervisor::OpenCodeSupervisor::host(config.opencode_bin.clone())?
    };
    match supervisor.launch_reporting(text, &canonical, None) {
        Ok(report) => {
            start::record(
                &journal.db,
                operation,
                &report.session.id,
                report.prompt_accepted,
            )?;
            let status = if report.prompt_accepted {
                "started"
            } else {
                "created_not_started"
            };
            ack_start(
                &config.endpoint,
                &credential,
                operation,
                status,
                Some(&report.session.id),
                "",
            )?;
            start::finish(&journal.db, operation)?;
        }
        Err(error) => {
            eprintln!("bridge start failed reason={}", safe_reason(&error));
            ack_start(
                &config.endpoint,
                &credential,
                operation,
                "failed",
                None,
                "server",
            )?;
            start::finish(&journal.db, operation)?;
        }
    }
    Ok(true)
}

fn is_busy(error: &anyhow::Error) -> bool {
    error.to_string().contains("already running")
}

/// `None` when another thread or process holds the lock. That is the job being
/// done already, not a failure, so callers skip quietly instead of logging an
/// outage and backing off.
fn try_local_lock(path: &Path) -> Result<Option<std::fs::File>> {
    match local_lock(path) {
        Ok(lock) => Ok(Some(lock)),
        Err(error) if is_busy(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn deliver_unless_busy(dir: &Path) -> Result<bool> {
    match deliver_once(dir) {
        Err(error) if is_busy(&error) => Ok(false),
        other => other,
    }
}

fn start_unless_busy(dir: &Path) -> Result<bool> {
    match start_once(dir) {
        Err(error) if is_busy(&error) => Ok(false),
        other => other,
    }
}

fn local_lock(path: &Path) -> Result<std::fs::File> {
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            bail!("bridge operation already running");
        }
    }
    Ok(lock)
}

pub fn run(dir: &Path) -> Result<()> {
    let _ = load(dir)?;
    let _lock = local_lock(&dir.join("daemon.lock"))?;
    let queue_dir = dir.to_owned();
    let active_until = Arc::new(AtomicU64::new(0));
    let activity = active_until.clone();
    // The cloud wakes the poll thread the moment someone reads history. Waking
    // the sync loop from it saves up to a full tick on the first read after idle.
    let (wake, woken) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || loop {
        let result = (|| -> Result<()> {
            let config = load(&queue_dir)?;
            let credential = token(&config)?;
            let poll = http::request(
                &config.endpoint,
                "/v1/connector/poll",
                Some(&credential),
                Some(&json!({})),
                35,
            )?;
            activity.store(
                poll["active_until"].as_u64().unwrap_or(0),
                Ordering::Relaxed,
            );
            let _ = wake.send(());
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis() as u64;
            // Delivery and the session-state publish are independent. A failed
            // delivery must not leave the phone reading a stale state.
            let delivery = (|| -> Result<()> {
                if poll["pending"] == true {
                    while deliver_unless_busy(&queue_dir)? {}
                    while start_unless_busy(&queue_dir)? {}
                }
                Ok(())
            })();
            if poll["active_until"].as_u64().unwrap_or(0) > now {
                if let Err(error) = publish_runtime(&queue_dir, &config, &credential) {
                    log_unavailable("runtime", &error);
                }
            }
            delivery
        })();
        if let Err(error) = result {
            log_unavailable("delivery", &error);
            std::thread::sleep(Duration::from_secs(10));
        }
    });
    let mut last_sync = std::time::Instant::now() - Duration::from_secs(900);
    let mut unhealthy_since: Option<std::time::Instant> = None;
    let mut last_alert: Option<std::time::Instant> = None;
    loop {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as u64;
        // Idle backfill every 15 minutes lets Neon's five-minute autosuspend
        // actually take effect. Active mobile use requests near-live checks.
        if active_until.load(Ordering::Relaxed) <= now
            && last_sync.elapsed() < Duration::from_secs(900)
        {
            let _ = woken.recv_timeout(Duration::from_secs(5));
            continue;
        }
        while woken.try_recv().is_ok() {}
        let outcome = sync_once(dir);
        let healthy = outcome.is_ok() && read_health(dir).failing.is_empty();
        let offline = outcome
            .as_ref()
            .err()
            .is_some_and(|e| matches!(error_kind(&e.to_string()), "network" | "busy"));
        if healthy || offline {
            unhealthy_since = None;
        } else {
            let since = *unhealthy_since.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() >= STUCK_ALERT_AFTER
                && !last_alert.is_some_and(|t| t.elapsed() < STUCK_ALERT_EVERY)
            {
                last_alert = Some(std::time::Instant::now());
                notify_stuck();
            }
        }
        match outcome {
            Ok(n) => {
                last_sync = std::time::Instant::now();
                if n > 0 {
                    eprintln!("bridge synced {n} history batches");
                }
                // Reconcile Postgres at the same cadence that already wakes it
                // for history. A crash between commit and wake signaling must
                // not strand a queued instruction indefinitely.
                if deliver_unless_busy(dir).is_err() {
                    eprintln!("bridge queue reconciliation deferred");
                }
                if start_unless_busy(dir).is_err() {
                    eprintln!("bridge start reconciliation deferred");
                }
            }
            Err(error) => log_unavailable("sync", &error),
        };
        // Sleep between passes, but not past a fresh wake from the cloud.
        let _ = woken.recv_timeout(Duration::from_secs(5));
    }
}

const STUCK_ALERT_AFTER: Duration = Duration::from_secs(30 * 60);
const STUCK_ALERT_EVERY: Duration = Duration::from_secs(6 * 3600);

fn notify_stuck() {
    eprintln!("bridge sync stuck for 30+ minutes; notifying");
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("osascript")
        .args([
            "-e",
            "display notification \"Phone sync has not completed for 30+ minutes. Run: agentview bridge status\" with title \"Agent View bridge\"",
        ])
        .status();
}

fn error_kind(message: &str) -> &'static str {
    if message.contains("HTTP 401") {
        "authentication"
    } else if message.contains("HTTP 429") {
        "rate_limit"
    } else if message.contains("HTTP 5") {
        "cloud_service"
    } else if message.contains("already running") {
        "busy"
    } else if message.contains("Keychain") {
        "credential"
    } else if message.contains("network") {
        "network"
    } else if message.contains("timed out") {
        "timeout"
    } else if message.contains("database is locked") || message.contains("table is locked") {
        "source_locked"
    } else if message.contains("unsupported OpenCode source schema") {
        "source_schema"
    } else {
        "local_or_native"
    }
}

/// Which step of the session-state snapshot failed. Only these fixed names are printed.
fn runtime_stage(chain: &str) -> Option<&'static str> {
    [
        "supervisor",
        "server_check",
        "discover",
        "registries",
        "source_open",
        "last_user",
    ]
    .into_iter()
    .find(|stage| chain.contains(&format!("runtime snapshot: {stage}")))
}

fn log_unavailable(channel: &str, error: &anyhow::Error) {
    // The whole cause chain decides the kind. Only the kind and a fixed stage name
    // are printed: the text can contain paths, SQL, headers or prompt content.
    let chain = format!("{error:#}");
    let kind = error_kind(&chain);
    let stage = runtime_stage(&chain).map_or(String::new(), |stage| format!(" stage={stage}"));
    eprintln!(
        "bridge {channel} unavailable kind={kind}{stage} time={}; retrying",
        now_secs()
    );
}

/// Compares each local projection with the cloud copy. Prints no message text.
pub fn verify(dir: &Path) -> Result<Value> {
    let config = load(dir)?;
    let credential = token(&config)?;
    let journal = Journal::open(&dir.join("journal.sqlite"))?;
    let mut db = source::open(&config.database)?;
    let ids = source::sessions(&db)?;
    let mut checked = 0;
    let mut matched = 0;
    let mut moved = 0;
    let mut mismatches = Vec::new();
    let mut failed = Vec::new();
    for id in &ids {
        let Some(capture) = source::capture(&mut db, id, None)? else {
            continue;
        };
        let checkpoint = journal.checkpoint(id)?;
        // A session that changed after the last sync pass is waiting for the next
        // pass. It is not a mismatch, and comparing it would only add noise.
        if checkpoint
            .as_ref()
            .is_some_and(|p| p.observed != capture.through)
        {
            moved += 1;
            continue;
        }
        let text = source::projection_text(&capture.state, &capture.messages);
        let local_hash = format!("{:x}", md5::Md5::digest(text.as_bytes()));
        let report = match http::request(
            &config.endpoint,
            "/v1/sync/verify",
            Some(&credential),
            Some(
                &json!({"session_id": id, "hash": local_hash, "cursor": capture.through, "count": capture.messages.len()}),
            ),
            60,
        ) {
            Ok(report) => report,
            Err(error) => {
                failed.push(json!({"session_id": id, "reason": safe_reason(&error)}));
                continue;
            }
        };
        checked += 1;
        // Same text but a different pointer is still a problem: the next delta would
        // be refused. A moved watermark with no upload is not.
        let pointer_differs = checkpoint
            .as_ref()
            .is_some_and(|p| report["cloud_cursor"].as_i64() != Some(p.cursor));
        if report["match"] == true && !pointer_differs {
            matched += 1;
        } else {
            mismatches.push(json!({
                "session_id": id,
                "reason": if report["match"] == true { json!("pointer") } else { report["reason"].clone() },
                "local_checkpoint": checkpoint.as_ref().map(|p| p.cursor),
                "cloud_cursor": report["cloud_cursor"],
                "local_count": capture.messages.len(),
                "cloud_count": report["cloud_count"],
            }));
        }
        if checked % 200 == 0 {
            eprintln!("bridge verify checked {checked}");
        }
    }
    Ok(json!({
        "checked": checked,
        "matched": matched,
        "moved_since_last_sync": moved,
        "mismatches": mismatches,
        "failed": failed,
    }))
}

pub fn status(dir: &Path) -> Result<Value> {
    let config = load(dir)?;
    let journal = Journal::open(&dir.join("journal.sqlite"))?;
    let health = read_health(dir);
    let (sessions, messages, pending): (i64, i64, i64) = (
        journal
            .db
            .query_row("SELECT count(*) FROM checkpoint", [], |r| r.get(0))?,
        journal
            .db
            .query_row("SELECT count(*) FROM cache", [], |r| r.get(0))?,
        journal
            .db
            .query_row("SELECT count(*) FROM pending", [], |r| r.get(0))?,
    );
    Ok(
        json!({"paired":true,"endpoint":config.endpoint,"sessions":sessions,"messages":messages,"pending_batches":pending,"device_id":config.device_id,"database_epoch":config.database_epoch,"last_fully_synced":health.last_ok,"last_pass_at":health.last_pass_at,"last_pass_ms":health.last_pass_ms,"scanned_sessions":health.scanned,"stuck_sessions":health.failing,"held_batches_for":journal.held_sessions()?}),
    )
}

#[cfg(any(target_os = "macos", test))]
fn login_environment(
    isolated: bool,
    service_file: Option<&Path>,
    xdg_state: Option<&Path>,
) -> std::collections::BTreeMap<String, String> {
    let mut values =
        std::collections::BTreeMap::from([("AGENTVIEW_OPENCODE_API".into(), "v2".into())]);
    if !isolated {
        for (key, path) in [
            ("AGENTVIEW_OPENCODE_SERVICE_FILE", service_file),
            ("XDG_STATE_HOME", xdg_state),
        ] {
            if let Some(path) = path.filter(|p| p.is_absolute()) {
                values.insert(key.into(), path.to_string_lossy().into_owned());
            }
        }
    }
    values
}

pub fn install(dir: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let config = load(dir)?;
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME missing")?);
        let label = format!("com.agentview.bridge.{}", config.device_id);
        let plist = home
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist"));
        let xml_escape = |s: &str| {
            s.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
        };
        let args = [
            std::env::current_exe()?.to_string_lossy().to_string(),
            "bridge".into(),
            "--state-dir".into(),
            dir.to_string_lossy().to_string(),
            "run".into(),
        ];
        use std::fmt::Write;
        let mut strings = String::new();
        for arg in args {
            write!(strings, "<string>{}</string>", xml_escape(&arg))?;
        }
        let service_file = std::env::var_os("AGENTVIEW_OPENCODE_SERVICE_FILE").map(PathBuf::from);
        let xdg_state = crate::fs_util::xdg_home("XDG_STATE_HOME").map(PathBuf::from);
        let mut environment = String::new();
        for (key, value) in login_environment(
            config.supervisor_state.is_some(),
            service_file.as_deref(),
            xdg_state.as_deref(),
        ) {
            write!(
                environment,
                "<key>{}</key><string>{}</string>",
                xml_escape(&key),
                xml_escape(&value)
            )?;
        }
        let contents=format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>{label}</string><key>ProgramArguments</key><array>{strings}</array><key>EnvironmentVariables</key><dict>{environment}</dict><key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>ThrottleInterval</key><integer>15</integer><key>StandardErrorPath</key><string>{}</string></dict></plist>",xml_escape(&dir.join("daemon.log").to_string_lossy()));
        std::fs::create_dir_all(plist.parent().unwrap())?;
        crate::fs_util::write_private_bytes(&plist, contents.as_bytes())?;
        let domain = format!("gui/{}", unsafe { libc::getuid() });
        let _ = std::process::Command::new("launchctl")
            .args(["enable", &format!("{domain}/{label}")])
            .output();
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", &format!("{domain}/{label}")])
            .output();
        if !std::process::Command::new("launchctl")
            .arg("bootstrap")
            .arg(&domain)
            .arg(&plist)
            .status()?
            .success()
        {
            bail!("could not install bridge login item");
        }
        println!(
            "Installed bridge login item. Runs while logged in and awake; does not wake the Mac."
        );
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = dir;
        bail!("login-item installation currently requires macOS")
    }
}

pub fn stop(dir: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let config = load(dir)?;
        let target = format!(
            "gui/{}/com.agentview.bridge.{}",
            unsafe { libc::getuid() },
            config.device_id
        );
        if !std::process::Command::new("launchctl")
            .args(["disable", &target])
            .status()?
            .success()
        {
            bail!("could not disable bridge login item");
        }
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", &target])
            .output();
        println!("Bridge stopped and disabled at login. Pairing and cloud history are retained.");
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = dir;
        bail!("login-item control currently requires macOS")
    }
}
