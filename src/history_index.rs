//! Derived, text-only OpenCode history. OpenCode's database is always read-only.
//! A transaction on this database elects one writer across dashboard instances;
//! WAL readers keep searching the previous index while it catches up.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

pub const REFRESH_INTERVAL: Duration = Duration::from_secs(10 * 60);

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
            let index = match control.history_search_index() {
                Ok(Some(index)) => index,
                Ok(None) => return,
                Err(error) => {
                    crate::perf!("history-index", "initialization failed: {error:#}");
                    return;
                }
            };
            let _ = sender.send(index.clone());
            loop {
                if worker_cancelled.load(Ordering::Relaxed) {
                    break;
                }
                let refreshed = index.refresh(worker_cancelled.clone());
                if let Err(error) = &refreshed {
                    crate::perf!("history-index", "refresh failed: {error:#}");
                }
                if sender.send(index.clone()).is_err() {
                    break;
                }
                let delay = if refreshed.is_err() {
                    REFRESH_INTERVAL
                } else {
                    index.next_refresh_in()
                };
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
        Ok(Self { source, path })
    }

    /// Literal, case-insensitive substring matching, like the old picker.
    /// Trigrams keep partial words ("lead" in "leads") indexed too.
    pub fn search(&self, needle: &str) -> Result<BTreeMap<String, String>> {
        if needle.trim().chars().count() < 3 || !self.path.exists() {
            return Ok(BTreeMap::new());
        }
        let db = read_only(&self.path)?;
        db.busy_timeout(Duration::from_millis(50))?;
        let query = format!("\"{}\"", needle.replace('"', "\"\""));
        let mut statement = db.prepare(
            "SELECT d.root, substr(d.text, max(1, instr(lower(d.text), lower(?1)) - 100), length(?1) + 300)
             FROM messages_fts JOIN documents d ON d.id = messages_fts.rowid
             WHERE messages_fts MATCH ?2 GROUP BY d.root",
        )?;
        let rows = statement.query_map(params![needle, query], |row| {
            Ok((
                format!("opencode:host:{}", row.get::<_, String>(0)?),
                row.get(1)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// `false` means another instance refreshed recently (or owns the writer).
    /// The timestamp and source-session versions commit with the index, so a
    /// cancelled/interrupted refresh cannot incorrectly mark stale data fresh.
    pub fn refresh(&self, cancelled: Arc<AtomicBool>) -> Result<bool> {
        if cancelled.load(Ordering::Relaxed) {
            return Ok(false);
        }
        std::fs::create_dir_all(self.path.parent().context("history index has no parent")?)?;
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
             CREATE TABLE IF NOT EXISTS documents (id INTEGER PRIMARY KEY, source TEXT NOT NULL, root TEXT NOT NULL, text TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS documents_source ON documents(source);
             CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(text, content='documents', content_rowid='id', tokenize='trigram');
             CREATE TRIGGER IF NOT EXISTS documents_ai AFTER INSERT ON documents BEGIN
               INSERT INTO messages_fts(rowid, text) VALUES (new.id, new.text);
             END;
             CREATE TRIGGER IF NOT EXISTS documents_ad AFTER DELETE ON documents BEGIN
               INSERT INTO messages_fts(messages_fts, rowid, text) VALUES ('delete', old.id, old.text);
             END;",
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
        let source = read_only(&self.source)?;
        source.busy_timeout(Duration::from_millis(100))?;
        let stop = cancelled.clone();
        source.progress_handler(1000, Some(move || stop.load(Ordering::Relaxed)));
        let stop = cancelled.clone();
        tx.progress_handler(1000, Some(move || stop.load(Ordering::Relaxed)));
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
        let sessions = {
            // Session timestamps alone miss streaming edits. Current OpenCode
            // keeps one cheap event-sequence counter per session. Older builds
            // without it can compare part metadata without extracting JSON.
            let has_sequences: bool = source.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='event_sequence')",
                [],
                |r| r.get(0),
            )?;
            let query = if has_sequences {
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
            let mut q = source.prepare(query)?;
            let rows = q.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
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
            let mut q = source.prepare(
                "SELECT json_extract(data, '$.text') FROM part
                 WHERE session_id=?1 AND json_extract(data, '$.type')='text'
                   AND json_extract(data, '$.synthetic') IS NOT 1
                   AND length(json_extract(data, '$.text')) > 0 ORDER BY time_created, id",
            )?;
            let mut rows = q.query([&id])?;
            while let Some(row) = rows.next()? {
                if cancelled.load(Ordering::Relaxed) {
                    bail!("history indexing cancelled");
                }
                let text: String = row.get(0)?;
                tx.execute(
                    "INSERT INTO documents(source, root, text) VALUES (?1, ?2, ?3)",
                    params![id, root, text],
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
