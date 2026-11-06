//! Derived, text-only provider history. Provider storage is always read-only.
//! A transaction on this database elects one writer across dashboard instances;
//! WAL readers keep searching the previous index while it catches up.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

pub const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// A picker text match. `distance` is the span of the tightest cluster of
/// the query words; zero means the query itself appears as a substring.
#[derive(Clone, Debug)]
pub struct SearchHit {
    pub snippet: String,
    pub distance: u32,
}

/// Byte span of a match inside the text `text_hit` was given.
#[derive(Clone, Debug)]
pub(crate) struct TextHit {
    pub start: usize,
    pub end: usize,
    pub distance: u32,
}

/// Find `needle` in `text`. ASCII case folding keeps the returned offsets
/// valid in the original string.
pub(crate) fn text_hit(text: &str, needle: &str) -> Option<TextHit> {
    let needle = needle.trim().to_ascii_lowercase();
    if needle.chars().count() < 3 {
        return None;
    }
    let text = text.to_ascii_lowercase();
    if let Some(start) = text.find(&needle) {
        return Some(TextHit {
            start,
            end: start + needle.len(),
            distance: 0,
        });
    }
    let tokens: Vec<&str> = needle
        .split_whitespace()
        .filter(|token| token.chars().count() >= 3)
        .collect();
    if tokens.len() < 2 {
        return None;
    }
    let hits: Vec<Vec<(usize, usize)>> =
        tokens.iter().map(|token| word_hits(&text, token)).collect();
    if hits.iter().any(|group| group.is_empty()) {
        return None;
    }
    tightest(&hits)
}

fn word_hits(text: &str, token: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut hits = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_alphanumeric() {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_alphanumeric() {
            index += 1;
        }
        if word_matches(token, &text[start..index]) {
            hits.push((start, index));
        }
    }
    hits
}

fn word_matches(query: &str, word: &str) -> bool {
    if word.contains(query) {
        return true;
    }
    let query_stem = stem(query);
    let word_stem = stem(word);
    if query_stem == word_stem {
        return true;
    }
    let shared = query_stem
        .bytes()
        .zip(word_stem.bytes())
        .take_while(|(left, right)| left == right)
        .count();
    shared >= 5 && is_inflection(&query_stem[shared..]) && is_inflection(&word_stem[shared..])
}

fn stem(word: &str) -> &str {
    for suffix in ["tion", "ing", "ion", "es", "ed", "se", "s", "e", "d"] {
        if let Some(stem) = word.strip_suffix(suffix) {
            if stem.len() >= 5 {
                return stem;
            }
        }
    }
    word
}

fn is_inflection(rest: &str) -> bool {
    matches!(
        rest,
        "" | "e" | "s" | "es" | "ed" | "d" | "ing" | "se" | "ion" | "tion"
    )
}

fn tightest(groups: &[Vec<(usize, usize)>]) -> Option<TextHit> {
    let mut best: Option<TextHit> = None;
    for &(origin_start, origin_end) in &groups[0] {
        let mut span_start = origin_start;
        let mut span_end = origin_end;
        let mut ok = true;
        let origin_mid = origin_start + (origin_end - origin_start) / 2;
        for group in &groups[1..] {
            let Some(&(start, end)) = group.iter().min_by_key(|(start, end)| {
                let mid = start + (end - start) / 2;
                mid.abs_diff(origin_mid)
            }) else {
                ok = false;
                break;
            };
            span_start = span_start.min(start);
            span_end = span_end.max(end);
        }
        if !ok {
            continue;
        }
        let distance = (span_end - span_start) as u32;
        if best.as_ref().is_none_or(|hit| distance < hit.distance) {
            best = Some(TextHit {
                start: span_start,
                end: span_end,
                distance,
            });
        }
    }
    best
}

fn fts_query(needle: &str) -> String {
    let tokens: Vec<&str> = needle
        .split_whitespace()
        .filter(|token| token.chars().count() >= 3)
        .collect();
    if tokens.len() < 2 {
        return quote_fts(needle);
    }
    tokens
        .iter()
        .map(|token| {
            let forms = query_forms(token);
            if forms.len() == 1 {
                quote_fts(&forms[0])
            } else {
                format!(
                    "({})",
                    forms
                        .iter()
                        .map(|form| quote_fts(form))
                        .collect::<Vec<_>>()
                        .join(" OR ")
                )
            }
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn query_forms(token: &str) -> Vec<String> {
    let mut forms = BTreeSet::new();
    forms.insert(token.to_string());
    let Some(stem) = inflection_stem(token) else {
        return vec![token.to_string()];
    };
    for suffix in ["d", "se", "s", "es", "ed", "ing", "e"] {
        let form = format!("{stem}{suffix}");
        if form.chars().count() < 5 {
            continue;
        }
        forms.insert(form.clone());
        if suffix == "d" {
            for extra in ["s", "ed", "ing"] {
                let verb = format!("{form}{extra}");
                if verb.chars().count() >= 5 {
                    forms.insert(verb);
                }
            }
        }
    }
    forms.into_iter().collect()
}

fn inflection_stem(token: &str) -> Option<&str> {
    for suffix in ["tion", "ing", "ion", "es", "ed", "se", "s", "e", "d"] {
        if let Some(stem) = token.strip_suffix(suffix) {
            if stem.len() >= 5 {
                return Some(stem);
            }
        }
    }
    None
}

fn quote_fts(token: &str) -> String {
    format!("\"{}\"", token.replace('"', "\"\""))
}

fn window(text: &str, start: usize, end: usize) -> String {
    let pad = 180;
    let mut from = start.saturating_sub(pad);
    while !text.is_char_boundary(from) {
        from -= 1;
    }
    let mut to = (end + pad).min(text.len());
    while to < text.len() && !text.is_char_boundary(to) {
        to += 1;
    }
    text[from..to].to_string()
}

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
pub struct MessageExcerpt {
    pub id: String,
    pub role: String,
    pub created_at_ms: Option<u64>,
    pub text: String,
}

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
pub struct MessageMatch {
    /// May be a subagent; the enclosing search result still restores its root.
    pub source_session_id: String,
    pub before: Vec<MessageExcerpt>,
    pub matched: MessageExcerpt,
    pub after: Vec<MessageExcerpt>,
}

/// Dropping the dashboard stops the timer and interrupts an in-flight scan,
/// including early error returns from terminal setup or event handling.
pub struct Worker {
    cancelled: Arc<AtomicBool>,
    stop: std::sync::mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Worker {
    pub fn start(
        control: crate::control::ControlHub,
    ) -> (Self, std::sync::mpsc::Receiver<HistoryIndex>) {
        let (sender, receiver) = std::sync::mpsc::channel();
        let (stop, stopped) = std::sync::mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        let thread = std::thread::spawn(move || {
            lower_worker_priority();
            let indices = match control.history_search_indices() {
                Ok(indices) if !indices.is_empty() => indices,
                Ok(_) => return,
                Err(error) => {
                    crate::perf!("history-index", "initialization failed: {error:#}");
                    return;
                }
            };
            for index in &indices {
                let _ = sender.send(index.clone());
            }
            loop {
                if worker_cancelled.load(Ordering::Relaxed) {
                    break;
                }
                let mut next_refresh = None;
                for index in &indices {
                    if worker_cancelled.load(Ordering::Relaxed) {
                        break;
                    }
                    let refreshed = index.refresh(worker_cancelled.clone());
                    if let Err(error) = &refreshed {
                        crate::perf!("history-index", "refresh failed: {error:#}");
                    } else {
                        let due = Instant::now() + index.next_refresh_in();
                        next_refresh =
                            Some(next_refresh.map_or(due, |known: Instant| known.min(due)));
                    }
                    if sender.send(index.clone()).is_err() {
                        return;
                    }
                }
                let delay = next_refresh
                    .map(|due| due.saturating_duration_since(Instant::now()))
                    .unwrap_or(REFRESH_INTERVAL)
                    .max(Duration::from_secs(5));
                if stopped.recv_timeout(delay) != Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
                    break;
                }
            }
        });
        (
            Self {
                cancelled,
                stop,
                thread: Some(thread),
            },
            receiver,
        )
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryIndex {
    source: PathBuf,
    path: PathBuf,
    provider: Option<crate::file_history::FileProvider>,
}

impl HistoryIndex {
    fn next_refresh_in(&self) -> Duration {
        let last = read_only(&self.path).ok().and_then(|db| {
            db.query_row("SELECT value FROM meta WHERE key='refreshed'", [], |r| {
                r.get::<_, u64>(0)
            })
            .ok()
        });
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        match last {
            Some(last) => REFRESH_INTERVAL
                .saturating_sub(Duration::from_secs(now.saturating_sub(last)))
                .max(Duration::from_secs(5)),
            // Another instance may still be doing the initial build.
            None => Duration::from_secs(30),
        }
    }

    pub fn is_ready(&self) -> bool {
        read_only(&self.path).ok().is_some_and(|db| {
            db.query_row("SELECT value FROM meta WHERE key='refreshed'", [], |r| {
                r.get::<_, i64>(0)
            })
            .is_ok()
        })
    }

    pub fn for_source(source: PathBuf) -> Result<Self> {
        // Stable across processes and Rust versions, unlike DefaultHasher.
        let hash = source
            .to_string_lossy()
            .bytes()
            .fold(0xcbf29ce484222325u64, |h, b| {
                (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
            });
        let state = crate::adapters::default_opencode_ownership_path()?;
        let path = state.with_file_name(format!("history-search-{hash:016x}.db"));
        Ok(Self {
            source,
            path,
            provider: None,
        })
    }

    pub fn for_files(provider: crate::file_history::FileProvider, home: PathBuf) -> Result<Self> {
        let mut index = Self::for_source(home)?;
        let name = index
            .path
            .file_name()
            .context("index has no file name")?
            .to_string_lossy();
        index.path = index
            .path
            .with_file_name(format!("{}-{name}", provider.key()));
        index.provider = Some(provider);
        Ok(index)
    }

    pub fn file_sessions(&self) -> Result<Vec<crate::control::RestorableSession>> {
        let Some(provider) = self.provider else {
            return Ok(Vec::new());
        };
        if !self.is_ready() {
            return Ok(Vec::new());
        }
        let db = read_only(&self.path)?;
        let mut q = db.prepare("SELECT id, name, cwd, max(updated_ms) FROM file_catalog GROUP BY id ORDER BY max(updated_ms) DESC")?;
        let rows = q.query_map([], |r| {
            let id: String = r.get(0)?;
            Ok(crate::control::RestorableSession {
                id: format!("{}:host:{id}", provider.key()),
                provider_session_id: id,
                provider: provider.provider(),
                name: r.get(1)?,
                cwd: PathBuf::from(r.get::<_, String>(2)?),
                updated_at_ms: r.get(3)?,
                transcript: String::new(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Case-insensitive text match. One word is a substring, so "lead" still
    /// finds "leads". Several words match when each word, or a close
    /// inflection of it, appears anywhere in the text. The hit is the
    /// tightest cluster, not the exact phrase: "archie response" finds
    /// "Never respond in a thread Archie".
    pub fn search(&self, needle: &str) -> Result<BTreeMap<String, SearchHit>> {
        if needle.trim().chars().count() < 3 || !self.path.exists() {
            return Ok(BTreeMap::new());
        }
        let db = read_only(&self.path)?;
        db.busy_timeout(Duration::from_millis(50))?;
        let mut statement = db.prepare(
            "SELECT d.root, substr(d.text, 1, 20000)
             FROM messages_fts JOIN documents d ON d.id = messages_fts.rowid
             WHERE messages_fts MATCH ?1
             LIMIT 400",
        )?;
        let rows = statement.query_map(params![fts_query(needle)], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut best = BTreeMap::new();
        for row in rows {
            let (root, text) = row?;
            let Some(hit) = text_hit(&text, needle) else {
                continue;
            };
            let id = if self.provider.is_some() {
                root
            } else {
                format!("opencode:host:{root}")
            };
            let replace = best
                .get(&id)
                .is_none_or(|previous: &SearchHit| hit.distance < previous.distance);
            if !replace {
                continue;
            }
            best.insert(
                id,
                SearchHit {
                    snippet: window(&text, hit.start, hit.end),
                    distance: hit.distance,
                },
            );
        }
        Ok(best)
    }

    /// One actual matching message and its adjacent messages, from the same
    /// read-only WAL snapshot as the match. No provider history is scanned here.
    pub fn matching_message(
        &self,
        session: &str,
        words: &[String],
    ) -> Result<Option<MessageMatch>> {
        if self.provider.is_some() || !self.path.exists() {
            return Ok(None);
        }
        let query = words
            .iter()
            .filter(|word| word.chars().count() >= 3)
            .map(|word| format!("\"{}\"", word.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ");
        if query.is_empty() {
            return Ok(None);
        }
        let mut db = read_only(&self.path)?;
        db.busy_timeout(Duration::from_millis(50))?;
        let tx = db.transaction()?;
        let root = session.strip_prefix("opencode:host:").unwrap_or(session);
        let found = tx
            .query_row(
                "SELECT d.id,d.source,d.message_id,d.role,d.created_ms,d.text
             FROM messages_fts JOIN documents d ON d.id=messages_fts.rowid
             WHERE messages_fts MATCH ?1 AND d.root=?2 AND d.message_id IS NOT NULL
             ORDER BY CASE WHEN instr(lower(d.text),?3)>0 THEN 0 ELSE 1 END,
                      bm25(messages_fts),d.created_ms DESC,d.id DESC LIMIT 1",
                params![query, root, words.join(" ")],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<u64>>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((anchor, source, message, role, created_at_ms, text)) = found else {
            return Ok(None);
        };
        let around = |before: bool| -> Result<Vec<MessageExcerpt>> {
            let (operator, order) = if before { ("<", "DESC") } else { (">", "ASC") };
            let sql = format!(
                "SELECT message_id,role,created_ms FROM documents
                WHERE source=?1 AND message_id!=?2 AND id {operator} ?3 AND message_id IS NOT NULL
                GROUP BY message_id ORDER BY min(id) {order} LIMIT 1"
            );
            let neighbor = tx
                .query_row(&sql, params![source, message, anchor], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<u64>>(2)?,
                    ))
                })
                .optional()?;
            let Some((id, role, created_at_ms)) = neighbor else {
                return Ok(Vec::new());
            };
            let mut statement = tx.prepare("SELECT substr(text,1,600) FROM documents WHERE source=?1 AND message_id=?2 ORDER BY id LIMIT 4")?;
            let parts = statement
                .query_map(params![source, id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(vec![MessageExcerpt {
                id,
                role,
                created_at_ms,
                text: parts.join("\n").chars().take(600).collect(),
            }])
        };
        let result = MessageMatch {
            source_session_id: source.clone(),
            before: around(true)?,
            matched: MessageExcerpt {
                id: message.clone(),
                role,
                created_at_ms,
                text: match_excerpt(&text, words),
            },
            after: around(false)?,
        };
        Ok(Some(result))
    }

    /// `false` means another instance refreshed recently (or owns the writer).
    /// The timestamp and source-session versions commit with the index, so a
    /// cancelled/interrupted refresh cannot incorrectly mark stale data fresh.
    pub fn refresh(&self, cancelled: Arc<AtomicBool>) -> Result<bool> {
        if cancelled.load(Ordering::Relaxed) {
            return Ok(false);
        }
        std::fs::create_dir_all(self.path.parent().context("history index has no parent")?)?;
        // All providers share this lease. Two dashboards must not start a
        // Claude and a Codex backfill at the same time on the same computer.
        let lease = self.path.with_file_name("history-refresh-lock.db");
        let mut lease_options = std::fs::OpenOptions::new();
        lease_options.create(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            lease_options.mode(0o600);
        }
        lease_options.open(&lease)?;
        let mut coordinator = Connection::open(lease)?;
        coordinator.busy_timeout(Duration::ZERO)?;
        let _writer = match coordinator
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        {
            Ok(tx) => tx,
            Err(error) if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy) => {
                return Ok(false)
            }
            Err(error) => return Err(error.into()),
        };
        let mut options = std::fs::OpenOptions::new();
        options.create(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(&self.path)?;
        let mut db = Connection::open(&self.path)?;
        db.busy_timeout(Duration::from_millis(50))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA cache_size=-2048;")?;
        let tx = match db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate) {
            Ok(tx) => tx,
            Err(error) if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy) => {
                return Ok(false);
            }
            Err(error) => return Err(error.into()),
        };
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY, updated TEXT NOT NULL, root TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS documents (id INTEGER PRIMARY KEY, source TEXT NOT NULL, root TEXT NOT NULL, text TEXT NOT NULL, message_id TEXT, created_ms INTEGER, role TEXT);
             CREATE INDEX IF NOT EXISTS documents_source ON documents(source);
             CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(text, content='documents', content_rowid='id', tokenize='trigram');
             CREATE TRIGGER IF NOT EXISTS documents_ai AFTER INSERT ON documents BEGIN
               INSERT INTO messages_fts(rowid, text) VALUES (new.id, new.text);
             END;
             CREATE TRIGGER IF NOT EXISTS documents_ad AFTER DELETE ON documents BEGIN
               INSERT INTO messages_fts(messages_fts, rowid, text) VALUES ('delete', old.id, old.text);
             END;",
        )?;
        let columns = tx
            .prepare("PRAGMA table_info(documents)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !columns.iter().any(|column| column == "message_id") {
            // Upgrade only this derived cache. Provider storage stays read-only.
            // WAL readers retain the old index until the complete rebuild commits.
            tx.execute_batch(
                "ALTER TABLE documents ADD COLUMN message_id TEXT;
                ALTER TABLE documents ADD COLUMN created_ms INTEGER;
                ALTER TABLE documents ADD COLUMN role TEXT;
                DELETE FROM documents; DELETE FROM sessions;
                DELETE FROM meta WHERE key='refreshed';",
            )?;
        }
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS documents_message ON documents(source,message_id,id);
            CREATE INDEX IF NOT EXISTS documents_root ON documents(root);",
        )?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        let last: i64 = tx
            .query_row("SELECT value FROM meta WHERE key='refreshed'", [], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or(0);
        if now >= last && now - last < REFRESH_INTERVAL.as_secs() as i64 {
            return Ok(false);
        }
        let stop = cancelled.clone();
        tx.progress_handler(1000, Some(move || stop.load(Ordering::Relaxed)));
        if let Some(provider) = self.provider {
            self.refresh_files(&tx, provider, &cancelled)?;
            let finished = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
            tx.execute(
                "INSERT OR REPLACE INTO meta VALUES ('refreshed', ?1)",
                [finished],
            )?;
            tx.commit()?;
            return Ok(true);
        }
        let source = read_only(&self.source)?;
        source.busy_timeout(Duration::from_millis(100))?;
        let stop = cancelled.clone();
        source.progress_handler(1000, Some(move || stop.load(Ordering::Relaxed)));
        let v2 = crate::adapters::opencode_db::install_compatibility_views(&source)?;
        let versions = {
            let mut q = tx.prepare("SELECT id, updated, root FROM sessions")?;
            let rows = q.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    (r.get::<_, String>(1)?, r.get::<_, String>(2)?),
                ))
            })?;
            rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()?
        };
        // Metadata is tiny. Parts use part_session_idx rather than scanning
        // every part in history each time a single session changes.
        let mut sessions = {
            // Session timestamps alone miss streaming edits. Current OpenCode
            // keeps one cheap event-sequence counter per session. Older builds
            // without it can compare part metadata without extracting JSON.
            let has_sequences: bool = source.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='event_sequence')",
                [],
                |r| r.get(0),
            )?;
            let legacy_query = if has_sequences {
                "SELECT s.id, CAST(s.time_updated AS TEXT)||':'||COALESCE(CAST(e.seq AS TEXT),'0'),
                        COALESCE(s.parent_id,s.id)
                 FROM session s LEFT JOIN event_sequence e ON e.aggregate_id=s.id
                 ORDER BY s.time_updated DESC"
            } else {
                "SELECT s.id, CAST(s.time_updated AS TEXT)||':'||
                        COALESCE((SELECT CAST(max(time_updated) AS TEXT) FROM part WHERE session_id=s.id),'0')||':'||
                        (SELECT count(*) FROM part WHERE session_id=s.id), COALESCE(s.parent_id,s.id)
                 FROM session s ORDER BY s.time_updated DESC"
            };
            let query = if v2 {
                // Streaming mutates an existing message without touching the
                // session timestamp or allocating a new message seq. Read raw
                // message metadata, not the JSON-expanded part view. Include
                // the durable event counter when available, plus a generation
                // tag so migration invalidates an already-indexed V1 session.
                let event_counter = if has_sequences {
                    "COALESCE((SELECT seq FROM event_sequence WHERE aggregate_id=s.id),0)"
                } else {
                    "0"
                };
                let mut query = format!(
                    "SELECT s.id, 'v2:'||s.time_updated||':'||{event_counter}||':'||
                            COALESCE((SELECT max(time_updated) FROM main.session_message WHERE session_id=s.id),0)||':'||
                            (SELECT count(*) FROM main.session_message WHERE session_id=s.id)||':'||
                            COALESCE((SELECT max(seq) FROM main.session_message WHERE session_id=s.id),0),
                            COALESCE(s.parent_id,s.id)
                     FROM main.session_v2 s"
                );
                let has_legacy: bool = source.query_row(
                    "SELECT EXISTS(SELECT 1 FROM main.sqlite_master WHERE type='table' AND name='session')",
                    [], |r| r.get(0),
                )?;
                if has_legacy {
                    let legacy_query = legacy_query
                        .trim_end_matches("ORDER BY s.time_updated DESC")
                        .replace("FROM session s", "FROM main.session s");
                    query.push_str(&format!(
                        " UNION ALL SELECT * FROM ({legacy_query}) legacy
                          WHERE NOT EXISTS(SELECT 1 FROM main.session_v2 WHERE id=legacy.id)"
                    ));
                }
                query
            } else {
                legacy_query.to_owned()
            };
            let mut q = source.prepare(&query)?;
            let rows = q.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        // A nested subagent must restore the actual top-level conversation,
        // not an intermediate child that has no row in the restore picker.
        let ancestry: BTreeMap<_, _> = sessions
            .iter()
            .map(|(id, _, parent)| (id.clone(), parent.clone()))
            .collect();
        for (id, _, root) in &mut sessions {
            let mut seen = BTreeSet::new();
            while let Some(parent) = ancestry.get(root) {
                if parent == root {
                    break;
                }
                if !seen.insert(root.clone()) {
                    *root = id.clone();
                    break;
                }
                *root = parent.clone();
            }
        }
        let live: BTreeSet<_> = sessions.iter().map(|s| s.0.as_str()).collect();
        for id in versions.keys().filter(|id| !live.contains(id.as_str())) {
            tx.execute("DELETE FROM documents WHERE source=?1", [id])?;
            tx.execute("DELETE FROM sessions WHERE id=?1", [id])?;
        }
        let mut changed = 0;
        for (id, updated, root) in sessions {
            if cancelled.load(Ordering::Relaxed) {
                bail!("history indexing cancelled");
            }
            if versions.get(&id) == Some(&(updated.clone(), root.clone())) {
                continue;
            }
            tx.execute("DELETE FROM documents WHERE source=?1", [&id])?;
            // Keep each read snapshot short, so a backfill does not pin the
            // OpenCode WAL for its entire duration. The earlier metadata
            // version is conservative if this session changes during a scan.
            let query = "SELECT json_extract(p.data,'$.text'),p.message_id,COALESCE(m.time_created,p.time_created),
                COALESCE(json_extract(m.data,'$.role'),'unknown')
                FROM part p LEFT JOIN message m ON m.id=p.message_id
                WHERE p.session_id=?1 AND json_extract(p.data,'$.type')='text'
                AND json_extract(p.data,'$.synthetic') IS NOT 1
                AND length(json_extract(p.data,'$.text'))>0 ORDER BY COALESCE(m.time_created,p.time_created),p.message_id,p.id";
            // Older minimal stores may not have message records. Their text is
            // still searchable; mark role unknown rather than inventing metadata.
            let mut q = source.prepare(query).or_else(|_| {
                source.prepare(
                    "SELECT json_extract(data,'$.text'),id,time_created,'unknown' FROM part
                 WHERE session_id=?1 AND json_extract(data,'$.type')='text'
                 AND json_extract(data,'$.synthetic') IS NOT 1
                 AND length(json_extract(data,'$.text'))>0 ORDER BY time_created,id",
                )
            })?;
            let mut rows = q.query([&id])?;
            while let Some(row) = rows.next()? {
                if cancelled.load(Ordering::Relaxed) {
                    bail!("history indexing cancelled");
                }
                let text: String = row.get(0)?;
                let message: Option<String> = row.get(1)?;
                let created: Option<u64> = row.get(2)?;
                let role: String = row.get(3)?;
                tx.execute(
                    "INSERT INTO documents(source,root,text,message_id,created_ms,role) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![id,root,text,message,created,role],
                )?;
            }
            drop(rows);
            drop(q);
            tx.execute(
                "INSERT OR REPLACE INTO sessions VALUES (?1, ?2, ?3)",
                params![id, updated, root],
            )?;
            changed += 1;
            // Yield between sessions rather than competing continuously with
            // interactive work, especially during the one-time backfill.
            std::thread::sleep(Duration::from_millis(5));
        }
        let finished = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        tx.execute(
            "INSERT OR REPLACE INTO meta VALUES ('refreshed', ?1)",
            [finished],
        )?;
        tx.commit()?;
        crate::perf!("history-index", "updated_sessions={changed}");
        Ok(true)
    }

    fn refresh_files(
        &self,
        tx: &rusqlite::Transaction<'_>,
        provider: crate::file_history::FileProvider,
        cancelled: &AtomicBool,
    ) -> Result<()> {
        tx.execute_batch("CREATE TABLE IF NOT EXISTS file_catalog (source TEXT PRIMARY KEY, id TEXT NOT NULL, name TEXT NOT NULL, cwd TEXT NOT NULL, updated_ms INTEGER NOT NULL);")?;
        let versions = {
            let mut q = tx.prepare("SELECT id, updated FROM sessions")?;
            let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()?
        };
        let files = provider.files(&self.source)?;
        let live: BTreeSet<_> = files
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        for source in versions.keys().filter(|source| !live.contains(*source)) {
            tx.execute("DELETE FROM documents WHERE source=?1", [source])?;
            tx.execute("DELETE FROM sessions WHERE id=?1", [source])?;
            tx.execute("DELETE FROM file_catalog WHERE source=?1", [source])?;
        }
        for path in files {
            if cancelled.load(Ordering::Relaxed) {
                bail!("history indexing cancelled");
            }
            let metadata = std::fs::metadata(&path)?;
            let modified = metadata.modified()?.duration_since(UNIX_EPOCH)?;
            let version = format!("{}:{}", metadata.len(), modified.as_nanos());
            let source = path.to_string_lossy().into_owned();
            if versions.get(&source) == Some(&version) {
                continue;
            }
            tx.execute("DELETE FROM documents WHERE source=?1", [&source])?;
            tx.execute("DELETE FROM file_catalog WHERE source=?1", [&source])?;
            let session = crate::file_history::read_session(
                &path,
                metadata.len(),
                provider,
                cancelled,
                |id, text| {
                    tx.execute(
                        "INSERT INTO documents(source, root, text) VALUES (?1, ?2, ?3)",
                        params![source, id, text],
                    )?;
                    Ok(())
                },
            )?;
            let root = match session {
                Some(session) => {
                    tx.execute(
                        "INSERT INTO file_catalog VALUES (?1,?2,?3,?4,?5)",
                        params![
                            source,
                            session.id,
                            session.name,
                            session.cwd.to_string_lossy(),
                            modified.as_millis() as u64
                        ],
                    )?;
                    format!("{}:host:{}", provider.key(), session.id)
                }
                None => {
                    tx.execute("DELETE FROM documents WHERE source=?1", [&source])?;
                    String::new()
                }
            };
            tx.execute(
                "INSERT OR REPLACE INTO sessions VALUES (?1,?2,?3)",
                params![source, version, root],
            )?;
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }
}

fn match_excerpt(text: &str, words: &[String]) -> String {
    let lower = text.to_lowercase();
    let at = words
        .iter()
        .filter_map(|word| lower.find(word))
        .min()
        .unwrap_or(0);
    let start = lower[..at].chars().count().saturating_sub(250);
    let excerpt: String = text.chars().skip(start).take(1200).collect();
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        excerpt,
        if text.chars().count() > start + 1200 {
            "…"
        } else {
            ""
        }
    )
}

fn read_only(path: &Path) -> Result<Connection> {
    Ok(Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?)
}

/// Thread-only scheduling priority; never lowers the dashboard/UI process.
pub fn lower_worker_priority() {
    #[cfg(target_os = "macos")]
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_BACKGROUND, 0);
    }
    #[cfg(target_os = "linux")]
    unsafe {
        libc::setpriority(
            libc::PRIO_PROCESS,
            libc::syscall(libc::SYS_gettid) as u32,
            10,
        );
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn fixture() -> (tempfile::TempDir, HistoryIndex) {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.db");
        let db = Connection::open(&source).unwrap();
        db.execute_batch(
            "CREATE TABLE session(id TEXT PRIMARY KEY, parent_id TEXT, time_updated INTEGER);
             CREATE TABLE part(id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT, time_updated INTEGER DEFAULT 1);
             CREATE TABLE event_sequence(aggregate_id TEXT PRIMARY KEY, seq INTEGER);
             INSERT INTO event_sequence VALUES ('ses_chat', 1), ('child', 1);
             CREATE INDEX part_session_idx ON part(session_id);
             INSERT INTO session VALUES ('ses_chat', NULL, 1), ('child', 'ses_chat', 1);
             INSERT INTO part(id, session_id, time_created, data) VALUES ('p1', 'ses_chat', 1, '{\"type\":\"text\",\"text\":\"retry the flaky Upload test, understand-leads and 100% literal\"}');
             INSERT INTO part(id, session_id, time_created, data) VALUES ('p2', 'ses_chat', 2, '{\"type\":\"tool\",\"text\":\"toolnoise\"}');
             INSERT INTO part(id, session_id, time_created, data) VALUES ('p3', 'ses_chat', 3, '{\"type\":\"text\",\"text\":\"syntheticnoise\",\"synthetic\":true}');
             INSERT INTO part(id, session_id, time_created, data) VALUES ('p4', 'child', 4, '{\"type\":\"text\",\"text\":\"child conversation\"}');",
        ).unwrap();
        let index = HistoryIndex {
            source,
            path: dir.path().join("index.db"),
            provider: None,
        };
        (dir, index)
    }

    fn refresh(index: &HistoryIndex) -> bool {
        index.refresh(Arc::new(AtomicBool::new(false))).unwrap()
    }

    fn make_due(index: &HistoryIndex) {
        Connection::open(&index.path)
            .unwrap()
            .execute("UPDATE meta SET value=0 WHERE key='refreshed'", [])
            .unwrap();
    }

    fn v2_fixture(mixed: bool, sequences: bool) -> (tempfile::TempDir, HistoryIndex) {
        let (dir, index) = fixture();
        let db = Connection::open(&index.source).unwrap();
        if mixed {
            // The minimal V1 test schema above omits columns used by discovery.
            db.execute_batch(
                "ALTER TABLE session ADD COLUMN title TEXT;
                ALTER TABLE session ADD COLUMN project_id TEXT;
                ALTER TABLE session ADD COLUMN directory TEXT;
                ALTER TABLE session ADD COLUMN time_created INTEGER;
                ALTER TABLE session ADD COLUMN time_archived INTEGER;
                ALTER TABLE part ADD COLUMN message_id TEXT;",
            )
            .unwrap();
        } else {
            db.execute_batch("DROP TABLE session; DROP TABLE part;")
                .unwrap();
        }
        if !sequences {
            db.execute_batch("DROP TABLE event_sequence;").unwrap();
        }
        db.execute_batch(r#"
            CREATE TABLE session_v2(id TEXT PRIMARY KEY, title TEXT, project_id TEXT,
                directory TEXT, parent_id TEXT, fork_session_id TEXT,
                time_created INTEGER, time_updated INTEGER, time_archived INTEGER);
            CREATE TABLE session_message(id TEXT PRIMARY KEY, session_id TEXT, type TEXT,
                seq INTEGER, time_created INTEGER, time_updated INTEGER, data TEXT);
            CREATE UNIQUE INDEX session_message_session_seq_idx ON session_message(session_id, seq);
            INSERT INTO session_v2 VALUES
                ('ses_v2', 'V2 title', 'p', '/work', NULL, NULL, 10, 20, NULL),
                ('child_v2', NULL, 'p', '/work', 'ses_v2', NULL, 11, 20, NULL),
                ('grandchild_v2', NULL, 'p', '/work', 'child_v2', NULL, 12, 20, NULL),
                ('fork_v2', NULL, 'p', '/work', NULL, 'ses_v2', 13, 20, NULL);
            INSERT INTO session_message VALUES
                ('u', 'ses_v2', 'user', 1, 10, 10, '{"text":"V2 lighthouse request: 100% literal"}'),
                ('a', 'ses_v2', 'assistant', 2, 11, 11,
                 '{"content":[{"type":"text","text":"streaming first draft"},{"type":"reasoning","text":"reasoningnoise"},{"type":"tool","name":"read","text":"toolnoise","state":{"status":"completed","content":[{"type":"text","text":"tooloutputnoise"}]}}]}'),
                ('synthetic', 'ses_v2', 'synthetic', 3, 12, 12, '{"text":"syntheticnoise"}'),
                ('system', 'ses_v2', 'system', 4, 13, 13, '{"text":"systemnoise"}'),
                ('compaction', 'ses_v2', 'compaction', 5, 14, 14, '{"summary":"compactionnoise","status":"completed"}'),
                ('nested', 'grandchild_v2', 'user', 1, 12, 12, '{"text":"nested child conversation"}'),
                ('forked', 'fork_v2', 'user', 1, 13, 13, '{"text":"independent fork conversation"}');
        "#).unwrap();
        if sequences {
            db.execute_batch(
                "INSERT INTO event_sequence VALUES ('ses_v2',5),('grandchild_v2',1),('fork_v2',1);",
            )
            .unwrap();
        }
        (dir, index)
    }

    #[test]
    fn matching_messages_keep_ids_timestamps_and_adjacent_exchanges() {
        let (_dir, index) = v2_fixture(false, false);
        let source = Connection::open(&index.source).unwrap();
        let text = format!(
            "{} Latent working memory is the shared KV cache. {}",
            "intro ".repeat(200),
            "ending ".repeat(400)
        );
        source
            .execute(
                "UPDATE session_message SET data=?1 WHERE id='a'",
                [serde_json::json!({"content":[{"type":"text","text":text}]}).to_string()],
            )
            .unwrap();
        source
            .execute(
                "INSERT INTO session_message VALUES('followup','ses_v2','user',6,30,30,?1)",
                [r#"{"text":"How does it compare with Interlat?"}"#],
            )
            .unwrap();
        source.execute("INSERT INTO session_message VALUES('speed','ses_v2','assistant',7,40,40,?1)", [r#"{"content":[{"type":"text","text":"Grok audio.output.speed accepts 0.7 to 1.5."}]}"#]).unwrap();
        assert!(refresh(&index));
        let found = index
            .matching_message("opencode:host:ses_v2", &["latent working memory".into()])
            .unwrap()
            .unwrap();
        assert_eq!(found.source_session_id, "ses_v2");
        assert_eq!(found.matched.id, "a");
        assert_eq!(found.matched.role, "assistant");
        assert_eq!(found.matched.created_at_ms, Some(11));
        assert!(found
            .matched
            .text
            .contains("Latent working memory is the shared KV cache"));
        assert!(found.matched.text.chars().count() <= 1202);
        assert_eq!(found.before[0].id, "u");
        assert_eq!(found.after[0].id, "followup");
        let speed = index
            .matching_message("opencode:host:ses_v2", &["0.7".into(), "1.5".into()])
            .unwrap()
            .unwrap();
        assert_eq!(speed.matched.id, "speed");
        assert_eq!(speed.matched.created_at_ms, Some(40));
        assert!(speed.matched.text.contains("0.7 to 1.5"));
        assert!(index
            .matching_message("opencode:host:fork_v2", &["latent working memory".into()])
            .unwrap()
            .is_none());
    }

    #[test]
    fn search_finds_nearby_word_forms_ahead_of_a_distant_mention() {
        let (_dir, index) = fixture();
        let db = Connection::open(&index.source).unwrap();
        db.execute_batch(
            "INSERT INTO session VALUES ('ses_gate', NULL, 2), ('ses_far', NULL, 3), ('ses_no', NULL, 4);
             INSERT INTO part(id, session_id, time_created, data) VALUES
               ('pg', 'ses_gate', 1, '{\"type\":\"text\",\"text\":\"Never respond in a thread Archie has not joined.\"}'),
               ('pf', 'ses_far', 1, '{\"type\":\"text\",\"text\":\"archie is mentioned up front. filler filler filler filler filler and only later a response header.\"}'),
               ('pn', 'ses_no', 1, '{\"type\":\"text\",\"text\":\"Archie is responsible for the nightly snapshot.\"}');",
        )
        .unwrap();
        assert!(refresh(&index));
        let found = index.search("archie response").unwrap();
        let near = found
            .get("opencode:host:ses_gate")
            .expect("nearby respond/Archie");
        let far = found.get("opencode:host:ses_far").expect("distant mention");
        assert!(
            near.distance < far.distance,
            "near {} far {}",
            near.distance,
            far.distance
        );
        let snippet = near.snippet.to_ascii_lowercase();
        assert!(snippet.contains("respond"), "{snippet}");
        assert!(snippet.contains("archie"), "{snippet}");
        assert!(
            !found.contains_key("opencode:host:ses_no"),
            "responsible is not a form of response"
        );
    }

    #[test]
    fn old_text_only_index_is_upgraded_even_when_it_was_just_refreshed() {
        let (_dir, index) = fixture();
        let db = Connection::open(&index.path).unwrap();
        db.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY,value INTEGER NOT NULL);
            CREATE TABLE sessions(id TEXT PRIMARY KEY,updated TEXT NOT NULL,root TEXT NOT NULL);
            CREATE TABLE documents(id INTEGER PRIMARY KEY,source TEXT NOT NULL,root TEXT NOT NULL,text TEXT NOT NULL);
            CREATE VIRTUAL TABLE messages_fts USING fts5(text,content='documents',content_rowid='id',tokenize='trigram');").unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        db.execute("INSERT INTO meta VALUES('refreshed',?1)", [now])
            .unwrap();
        drop(db);
        assert!(
            refresh(&index),
            "metadata upgrade must not wait for freshness timeout"
        );
        assert!(index
            .search("Upload")
            .unwrap()
            .contains_key("opencode:host:ses_chat"));
        let found = index
            .matching_message("opencode:host:ses_chat", &["upload".into()])
            .unwrap()
            .unwrap();
        assert_eq!(found.matched.created_at_ms, Some(1));
    }

    #[test]
    fn v2_only_restore_search_indexes_conversation_text_and_nested_children() {
        let (_dir, index) = v2_fixture(false, false);
        assert!(refresh(&index));
        for text in [
            "lighthouse",
            "100%",
            "first draft",
            "nested child conversation",
        ] {
            let found = index.search(text).unwrap();
            assert_eq!(found.len(), 1, "{text}");
            assert!(found.contains_key("opencode:host:ses_v2"), "{text}");
        }
        let found = index.search("independent fork conversation").unwrap();
        assert!(found.contains_key("opencode:host:fork_v2"));
        for text in [
            "toolnoise",
            "tooloutputnoise",
            "reasoningnoise",
            "syntheticnoise",
            "systemnoise",
            "compactionnoise",
        ] {
            assert!(index.search(text).unwrap().is_empty(), "{text}");
        }
        let source = Connection::open(&index.source).unwrap();
        let added: i64 = source.query_row("SELECT count(*) FROM sqlite_master WHERE name IN ('documents','messages_fts','meta','sessions','session','part') OR type IN ('trigger','view')", [], |r| r.get(0)).unwrap();
        assert_eq!(added, 0, "compatibility objects must stay connection-local");
    }

    #[test]
    fn mixed_v1_v2_streaming_refresh_is_incremental_even_with_a_static_event_counter() {
        for sequences in [false, true] {
            let (_dir, index) = v2_fixture(true, sequences);
            assert!(refresh(&index));
            assert!(index
                .search("upload")
                .unwrap()
                .contains_key("opencode:host:ses_chat"));
            let source = Connection::open(&index.source).unwrap();
            // Same message ID, seq and session timestamp: streaming only bumps
            // the raw message's update timestamp. Durable seq may stay put.
            source
                .execute(
                    "UPDATE session_message SET data=?1, time_updated=21 WHERE id='a'",
                    [r#"{"content":[{"type":"text","text":"streamed replacement"}]}"#],
                )
                .unwrap();
            // An unchanged session must not have its JSON parsed again.
            source
                .execute(
                    "UPDATE session_message SET data='not json' WHERE id='forked'",
                    [],
                )
                .unwrap();
            make_due(&index);
            assert!(refresh(&index));
            assert!(index.search("first draft").unwrap().is_empty());
            assert!(index
                .search("streamed replacement")
                .unwrap()
                .contains_key("opencode:host:ses_v2"));
            assert!(index
                .search("independent fork")
                .unwrap()
                .contains_key("opencode:host:fork_v2"));
            assert!(index
                .search("upload")
                .unwrap()
                .contains_key("opencode:host:ses_chat"));
            // New messages and reverted/deleted messages also invalidate.
            source.execute_batch("INSERT INTO session_message VALUES ('new','ses_v2','user',6,22,22,'{\"text\":\"appended request\"}');").unwrap();
            make_due(&index);
            assert!(refresh(&index));
            assert_eq!(index.search("appended request").unwrap().len(), 1);
            source
                .execute("DELETE FROM session_message WHERE id='new'", [])
                .unwrap();
            source
                .execute("DELETE FROM session_v2 WHERE id='grandchild_v2'", [])
                .unwrap();
            make_due(&index);
            assert!(refresh(&index));
            assert!(index.search("appended request").unwrap().is_empty());
            assert!(index.search("nested child").unwrap().is_empty());
        }
    }

    #[test]
    fn migrating_an_indexed_v1_id_to_v2_replaces_its_old_documents() {
        let (_dir, index) = v2_fixture(true, true);
        assert!(refresh(&index));
        let source = Connection::open(&index.source).unwrap();
        source.execute_batch("INSERT INTO session_v2 VALUES ('ses_chat','Migrated','p','/work',NULL,NULL,1,1,NULL);
            INSERT INTO session_message VALUES ('migrated','ses_chat','user',1,1,1,'{\"text\":\"migrated replacement\"}');").unwrap();
        make_due(&index);
        assert!(refresh(&index));
        assert!(index.search("upload").unwrap().is_empty());
        assert!(index
            .search("migrated replacement")
            .unwrap()
            .contains_key("opencode:host:ses_chat"));
        // Legacy child history continues to belong to the migrated root.
        assert!(index
            .search("child conversation")
            .unwrap()
            .contains_key("opencode:host:ses_chat"));
    }

    #[test]
    fn indexes_literal_substrings_and_children_but_not_tools_or_synthetic_text() {
        let (_dir, index) = fixture();
        assert!(!index.is_ready());
        assert!(refresh(&index));
        for query in [
            "upload",
            "leads",
            "understand-leads",
            "100%",
            "child conversation",
        ] {
            let found = index.search(query).unwrap();
            assert_eq!(found.len(), 1, "query={query}");
            assert!(found.contains_key("opencode:host:ses_chat"));
        }
        for query in ["toolnoise", "syntheticnoise", "ab", "\" OR *"] {
            assert!(index.search(query).unwrap().is_empty(), "query={query}");
        }
        assert!(index.is_ready());
    }

    #[test]
    fn refresh_is_incremental_persistent_and_shared_between_instances() {
        let (_dir, index) = fixture();
        assert!(refresh(&index));
        let another = index.clone();
        assert!(!refresh(&another));
        let db = Connection::open(&index.source).unwrap();
        db.execute(
            "UPDATE part SET data=?1 WHERE id='p1'",
            [r#"{"type":"text","text":"replacement content"}"#],
        )
        .unwrap();
        db.execute(
            "UPDATE event_sequence SET seq=2 WHERE aggregate_id='ses_chat'",
            [],
        )
        .unwrap();
        assert!(
            !refresh(&another),
            "ten minute interval applies across instances"
        );
        make_due(&index);
        assert!(refresh(&another));
        assert!(index.search("upload").unwrap().is_empty());
        assert_eq!(index.search("replacement").unwrap().len(), 1);
        // If an unchanged session were reparsed, malformed JSON would fail.
        db.execute("UPDATE part SET data='not json' WHERE id='p4'", [])
            .unwrap();
        make_due(&index);
        assert!(refresh(&index));
        assert_eq!(index.search("child conversation").unwrap().len(), 1);
        db.execute("DELETE FROM session WHERE id='child'", [])
            .unwrap();
        make_due(&index);
        assert!(refresh(&index));
        assert!(index.search("child conversation").unwrap().is_empty());
    }

    #[test]
    fn another_writer_does_not_block_search_or_start_duplicate_work() {
        let (_dir, index) = fixture();
        refresh(&index);
        make_due(&index);
        let mut db = Connection::open(&index.path).unwrap();
        let _tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        assert!(!refresh(&index));
        assert_eq!(index.search("upload").unwrap().len(), 1);
    }

    #[test]
    fn provider_backfills_share_one_cross_instance_writer_lease() {
        let (_dir, index) = fixture();
        refresh(&index);
        make_due(&index);
        let mut lease =
            Connection::open(index.path.with_file_name("history-refresh-lock.db")).unwrap();
        let _writer = lease
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        let other = HistoryIndex {
            source: index.source.clone(),
            path: index.path.with_file_name("claude-index.db"),
            provider: Some(crate::file_history::FileProvider::Claude),
        };
        assert!(!refresh(&index));
        assert!(!refresh(&other));
        assert!(
            !other.path.exists(),
            "another provider must not start a parallel backfill"
        );
        assert_eq!(index.search("upload").unwrap().len(), 1);
    }

    #[test]
    fn older_opencode_without_event_sequences_tracks_part_edits() {
        let (_dir, index) = fixture();
        let source = Connection::open(&index.source).unwrap();
        source.execute("DROP TABLE event_sequence", []).unwrap();
        refresh(&index);
        source
            .execute(
                "UPDATE part SET data=?1, time_updated=2 WHERE id='p1'",
                [r#"{"type":"text","text":"streamed replacement"}"#],
            )
            .unwrap();
        make_due(&index);
        refresh(&index);
        assert!(index.search("upload").unwrap().is_empty());
        assert_eq!(index.search("streamed replacement").unwrap().len(), 1);
    }

    #[test]
    fn cancellation_preserves_the_previous_index() {
        let (_dir, index) = fixture();
        refresh(&index);
        make_due(&index);
        assert!(!index.refresh(Arc::new(AtomicBool::new(true))).unwrap());
        assert_eq!(index.search("upload").unwrap().len(), 1);
    }

    #[test]
    fn failed_refresh_rolls_back_and_never_modifies_the_source_schema() {
        let (_dir, index) = fixture();
        refresh(&index);
        make_due(&index);
        let source = Connection::open(&index.source).unwrap();
        source
            .execute("UPDATE part SET data='broken json' WHERE id='p1'", [])
            .unwrap();
        source
            .execute("UPDATE session SET time_updated=2 WHERE id='ses_chat'", [])
            .unwrap();
        assert!(index.refresh(Arc::new(AtomicBool::new(false))).is_err());
        assert_eq!(index.search("upload").unwrap().len(), 1);
        let schema_count: i64 = source.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name IN ('documents','messages_fts','meta','sessions') OR type='trigger'",
            [], |r| r.get(0),
        ).unwrap();
        assert_eq!(schema_count, 0);
    }

    #[test]
    fn file_history_index_updates_appends_rewrites_deletes_and_resume_metadata() {
        use crate::file_history::FileProvider;
        use std::io::Write;
        for provider in [FileProvider::Claude, FileProvider::Codex] {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().join("provider");
            let file = home.join(if provider == FileProvider::Claude {
                "projects/project/session-a.jsonl"
            } else {
                "sessions/2026/10/15/rollout-a.jsonl"
            });
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            let initial = if provider == FileProvider::Claude {
                "{\"type\":\"user\",\"cwd\":\"/work/project\",\"message\":{\"content\":\"First lighthouse request\"}}\n"
            } else {
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"session-a\",\"cwd\":\"/work/project\"}}\n{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"First lighthouse request\"}}\n"
            };
            let cwd = dir.path().join("project");
            let json_cwd = serde_json::to_string(&cwd).unwrap();
            let initial = initial.replace("/work/project", &json_cwd[1..json_cwd.len() - 1]);
            std::fs::write(&file, &initial).unwrap();
            let index = HistoryIndex {
                source: home,
                path: dir.path().join("index.db"),
                provider: Some(provider),
            };
            refresh(&index);
            let id = format!("{}:host:session-a", provider.key());
            assert!(index.search("lighthouse").unwrap().contains_key(&id));
            let saved = index.file_sessions().unwrap();
            assert_eq!(saved.len(), 1);
            assert_eq!(saved[0].provider, provider.provider());
            assert_eq!(saved[0].provider_session_id, "session-a");
            assert_eq!(saved[0].cwd, cwd);
            let appended = if provider == FileProvider::Claude {
                "{\"type\":\"assistant\",\"message\":{\"content\":\"meridian response\"}}\n"
            } else {
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"meridian response\"}}\n"
            };
            std::fs::OpenOptions::new()
                .append(true)
                .open(&file)
                .unwrap()
                .write_all(appended.as_bytes())
                .unwrap();
            make_due(&index);
            refresh(&index);
            assert!(index.search("meridian").unwrap().contains_key(&id));
            std::fs::write(&file, initial.replace("lighthouse", "northstar")).unwrap();
            make_due(&index);
            refresh(&index);
            assert!(index.search("meridian").unwrap().is_empty());
            assert!(index.search("lighthouse").unwrap().is_empty());
            assert!(index.search("northstar").unwrap().contains_key(&id));
            // A second reader can immediately use the persisted results.
            assert_eq!(index.clone().file_sessions().unwrap().len(), 1);
            std::fs::remove_file(&file).unwrap();
            make_due(&index);
            refresh(&index);
            assert!(index.search("northstar").unwrap().is_empty());
            assert!(index.file_sessions().unwrap().is_empty());
        }
    }

    #[test]
    #[ignore = "read-only real history benchmark; set AGENTVIEW_INDEX_SOURCE"]
    fn real_history_index_benchmark() {
        lower_worker_priority();
        let source = PathBuf::from(std::env::var_os("AGENTVIEW_INDEX_SOURCE").unwrap());
        let index = HistoryIndex::for_source(source).unwrap();
        if index.is_ready() {
            make_due(&index);
        }
        let start = std::time::Instant::now();
        let changed = refresh(&index);
        println!(
            "refresh: {:?}, changed={changed}, index={}",
            start.elapsed(),
            index.path.display()
        );
        for query in ["lead", "understand", "flaky test", "upload", "opencode"] {
            let start = std::time::Instant::now();
            let matches = index.search(query).unwrap();
            println!(
                "{query:?}: {:?}, {} sessions",
                start.elapsed(),
                matches.len()
            );
        }
        let start = std::time::Instant::now();
        assert!(!refresh(&index));
        println!("unchanged refresh: {:?}", start.elapsed());
    }
}
