//! Answers `opencode db <sql> --format tsv` by reading OpenCode's SQLite file
//! in-process. Starting the OpenCode CLI costs about half a second per query,
//! while the queries themselves take milliseconds, so every refresh paid
//! mostly for process startup. Any command this cannot answer, or any query
//! that fails to open or prepare, still runs the real CLI.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};

use crate::perf;
use crate::process::{CommandOutput, CommandRequest, CommandRunner};

pub(super) struct DirectDbRunner {
    inner: Arc<dyn CommandRunner>,
    program: String,
    /// Resolved by the CLI (`db path` on V1, `debug paths db` on V2), so
    /// release channels and `OPENCODE_DB` follow the selected executable.
    path: OnceLock<Option<PathBuf>>,
    cancelled: Arc<AtomicBool>,
}

impl DirectDbRunner {
    pub(super) fn new(inner: Arc<dyn CommandRunner>, program: String) -> Self {
        Self {
            inner,
            program,
            path: OnceLock::new(),
            cancelled: Arc::default(),
        }
    }

    fn path(&self) -> Option<&PathBuf> {
        self.path
            .get_or_init(|| {
                let started = Instant::now();
                let path = self.resolve_path();
                perf!(
                    "opencode-db",
                    "path took={} path={path:?}",
                    crate::perf_log::ms(started.elapsed())
                );
                path
            })
            .as_ref()
    }

    fn resolve_path(&self) -> Option<PathBuf> {
        for args in [vec!["db", "path"], vec!["debug", "paths", "db"]] {
            let mut request = CommandRequest::new(
                self.program.clone(),
                args.into_iter().map(str::to_owned).collect(),
            );
            request.timeout = Duration::from_secs(8);
            let Ok(output) = self.inner.run(&request) else {
                continue;
            };
            if output.status == 0 {
                if let Ok(text) = output.stdout_text() {
                    let path = PathBuf::from(text.trim());
                    if path.is_absolute() && path.is_file() {
                        return Some(path);
                    }
                }
            }
        }
        None
    }

    fn query(&self, sql: &str, timeout: Duration) -> Result<Query> {
        let path = self
            .path()
            .ok_or_else(|| anyhow!("OpenCode database path is unknown"))?;
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| anyhow!("open {}: {error}", path.display()))?;
        db.busy_timeout(Duration::from_secs(2))?;
        self.bound_query(&db, timeout);
        let v2 = install_compatibility_views(&db)?;
        // V2's chronological order is seq, not the message ID or wall clock.
        let sql = if v2 {
            sql.replace(
                "ORDER BY m.time_created DESC, m.id DESC",
                "ORDER BY m.sort_seq DESC, m.id DESC",
            )
            .replace(
                "ORDER BY m2.time_created DESC, m2.id DESC",
                "ORDER BY m2.sort_seq DESC, m2.id DESC",
            )
        } else {
            sql.to_owned()
        };
        let mut statement = match db.prepare(&sql) {
            Ok(statement) => statement,
            Err(error) => return Ok(Query::Unsupported(error.to_string())),
        };
        Ok(Query::Rows(render_tsv(&mut statement)?))
    }

    fn bound_query(&self, db: &Connection, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let cancelled = self.cancelled.clone();
        db.progress_handler(
            10_000,
            Some(move || cancelled.load(Ordering::SeqCst) || Instant::now() >= deadline),
        );
    }

    /// Keep the existing transcript renderer's V1-shaped input while reading
    /// V2 locally. No server is started by `session export` as a side effect.
    fn export_v2(&self, id: &str, timeout: Duration) -> Result<Option<String>> {
        let Some(path) = self.path() else {
            return Ok(None);
        };
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.busy_timeout(Duration::from_secs(2))?;
        self.bound_query(&db, timeout);
        if !has_table(&db, "session_v2")? {
            return Ok(None);
        }
        if !db.query_row(
            "SELECT EXISTS(SELECT 1 FROM session_v2 WHERE id=?1)",
            [id],
            |r| r.get::<_, bool>(0),
        )? {
            // A V2 executable has no V1 `export` command. Unmigrated history
            // alongside V2 must therefore also be inspectable locally.
            if !has_table(&db, "session")?
                || !db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM main.session WHERE id=?1)",
                    [id],
                    |r| r.get::<_, bool>(0),
                )?
            {
                return Ok(None);
            }
            let mut statement = db.prepare("SELECT json_object('info', json(m.data), 'parts', json((SELECT json_group_array(json(p.data)) FROM main.part p WHERE p.message_id=m.id))) FROM main.message m WHERE m.session_id=?1 ORDER BY m.time_created, m.id")?;
            let records = statement.query_map([id], |row| row.get::<_, String>(0))?;
            let mut messages = Vec::new();
            for record in records {
                messages.push(serde_json::from_str::<serde_json::Value>(&record?)?);
            }
            return Ok(Some(serde_json::json!({"messages":messages}).to_string()));
        }
        let mut statement =
            db.prepare("SELECT type, data FROM session_message WHERE session_id=?1 ORDER BY seq")?;
        let mut rows = statement.query([id])?;
        let mut messages = Vec::new();
        while let Some(row) = rows.next()? {
            let kind: String = row.get(0)?;
            let data: serde_json::Value = serde_json::from_str(&row.get::<_, String>(1)?)?;
            let parts = match kind.as_str() {
                "user" | "synthetic" | "system" => {
                    vec![serde_json::json!({"type":"text", "text":data["text"]})]
                }
                "assistant" => data["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|part| part["type"] == "text")
                    .cloned()
                    .collect(),
                _ => continue,
            };
            messages.push(serde_json::json!({"info":{"role":kind}, "parts":parts}));
        }
        Ok(Some(serde_json::json!({"messages":messages}).to_string()))
    }
}

fn has_table(db: &Connection, name: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_master WHERE type='table' AND name=?1)",
        [name],
        |row| row.get(0),
    )?)
}

/// Shared local read path for callers that previously spawned `opencode db`.
/// The boolean identifies V2-capable stores; the returned connection already
/// has the V1/V2 projections installed. No server is started and the connection
/// cannot write to OpenCode's store. Bound work like the other discovery probes.
pub(crate) fn open_read_only(executable: &str) -> Result<(Connection, bool)> {
    let runner = DirectDbRunner::new(
        Arc::new(crate::process::ProcessRunner),
        executable.to_owned(),
    );
    let path = runner
        .path()
        .ok_or_else(|| anyhow!("OpenCode database path is unknown"))?;
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(Duration::from_millis(100))?;
    runner.bound_query(&db, Duration::from_secs(4));
    let v2 = install_compatibility_views(&db)?;
    Ok((db, v2))
}

/// Connection-local read projections, never migrations of OpenCode's store.
/// Migrated IDs prefer V2; untouched V1 sessions remain discoverable in the
/// same database. Fork provenance is deliberately not a parent relationship.
pub(crate) fn install_compatibility_views(db: &Connection) -> Result<bool> {
    if !has_table(db, "session_v2")? {
        return Ok(false);
    }
    let legacy_session = if has_table(db, "session")? {
        "UNION ALL SELECT id, title, project_id, directory, parent_id, time_created, time_updated, time_archived FROM main.session WHERE id NOT IN (SELECT id FROM main.session_v2)"
    } else {
        ""
    };
    let legacy_message = if has_table(db, "message")? {
        "UNION ALL SELECT id, session_id, time_created, time_updated, data, time_created AS sort_seq FROM main.message WHERE session_id NOT IN (SELECT id FROM main.session_v2)"
    } else {
        ""
    };
    let legacy_part = if has_table(db, "part")? {
        "UNION ALL SELECT id, message_id, session_id, time_created, time_updated, data FROM main.part WHERE session_id NOT IN (SELECT id FROM main.session_v2)"
    } else {
        ""
    };
    db.execute_batch(&format!(
        r#"
        CREATE TEMP VIEW session AS
        SELECT id, COALESCE(title, 'New session') AS title, project_id, directory,
               parent_id, time_created, time_updated, time_archived FROM main.session_v2
        {legacy_session};
        CREATE TEMP VIEW message AS
        SELECT id, session_id, time_created, time_updated,
               json_set(data, '$.role', CASE type WHEN 'idle' THEN 'assistant'
                        WHEN 'shell' THEN 'assistant' WHEN 'compaction' THEN 'assistant'
                        WHEN 'synthetic' THEN 'user' ELSE type END,
                        '$.time.completed', CASE WHEN type='idle' THEN time_created
                        WHEN type IN ('shell','compaction') AND json_extract(data,'$.status') NOT IN ('running','pending') THEN time_updated
                        ELSE json_extract(data, '$.time.completed') END) AS data,
               seq AS sort_seq
        FROM main.session_message
        WHERE type IN ('user', 'synthetic', 'assistant', 'idle', 'shell', 'compaction')
        {legacy_message};
        CREATE TEMP VIEW part AS
        SELECT m.id || ':' || c.key AS id, m.id AS message_id, m.session_id,
               m.time_created, m.time_updated,
               CASE WHEN json_extract(c.value, '$.type')='tool'
                    THEN json_set(c.value, '$.tool', json_extract(c.value, '$.name'))
                    ELSE c.value END AS data
        FROM main.session_message m, json_each(m.data, '$.content') c
        WHERE m.type='assistant'
        UNION ALL
        SELECT id || ':text', id, session_id, time_created, time_updated,
               json_object('type', 'text', 'text', json_extract(data, '$.text'),
                           'synthetic', json(CASE WHEN type IN ('synthetic','system') THEN 'true' ELSE 'false' END))
        FROM main.session_message WHERE type IN ('user', 'synthetic', 'system')
        {legacy_part};
    "#
    ))?;
    Ok(true)
}

enum Query {
    Rows(String),
    /// SQL this SQLite build rejects; the OpenCode CLI may still accept it.
    Unsupported(String),
}

/// Rows the way `opencode db --format tsv` prints them: a header of column
/// names, tab-separated values, NULL as empty, and nothing at all for no rows.
fn render_tsv(statement: &mut rusqlite::Statement<'_>) -> Result<String> {
    let columns = statement.column_count();
    let header = statement.column_names().join("\t");
    let mut rows = statement.query([])?;
    let mut out = String::new();
    while let Some(row) = rows.next()? {
        if out.is_empty() {
            out.push_str(&header);
            out.push('\n');
        }
        for index in 0..columns {
            if index > 0 {
                out.push('\t');
            }
            match row.get_ref(index)? {
                ValueRef::Null => {}
                ValueRef::Integer(value) => out.push_str(&value.to_string()),
                ValueRef::Real(value) => out.push_str(&value.to_string()),
                ValueRef::Text(text) | ValueRef::Blob(text) => {
                    out.push_str(&String::from_utf8_lossy(text))
                }
            }
        }
        out.push('\n');
    }
    Ok(out)
}

/// The SQL of a `db <sql> --format tsv` request for this OpenCode program.
fn tsv_query<'a>(program: &str, request: &'a CommandRequest) -> Option<&'a str> {
    match request.args.as_slice() {
        [db, sql, format, tsv]
            if request.program == program && db == "db" && format == "--format" && tsv == "tsv" =>
        {
            Some(sql)
        }
        _ => None,
    }
}

impl CommandRunner for DirectDbRunner {
    fn run(&self, request: &CommandRequest) -> Result<CommandOutput> {
        if request.program == self.program {
            if request.args == ["db", "path"] {
                if self.cancelled.load(Ordering::SeqCst) {
                    bail!("command cancelled because the dashboard exited");
                }
                if let Some(path) = self.path() {
                    return Ok(CommandOutput {
                        status: 0,
                        stdout: format!("{}\n", path.display()).into_bytes(),
                        stderr: Vec::new(),
                    });
                }
            }
            if let [command, id] = request.args.as_slice() {
                if command == "export" {
                    if self.cancelled.load(Ordering::SeqCst) {
                        bail!("command cancelled because the dashboard exited");
                    }
                    if let Some(stdout) = self.export_v2(id, request.timeout)? {
                        return Ok(CommandOutput {
                            status: 0,
                            stdout: stdout.into_bytes(),
                            stderr: Vec::new(),
                        });
                    }
                }
            }
        }
        let Some(sql) = tsv_query(&self.program, request) else {
            return self.inner.run(request);
        };
        if self.cancelled.load(Ordering::SeqCst) {
            bail!("command cancelled because the dashboard exited");
        }
        let started = Instant::now();
        let fallback = match self.query(sql, request.timeout) {
            Ok(Query::Rows(stdout)) => {
                perf!(
                    "opencode-db",
                    "direct took={} bytes={}",
                    crate::perf_log::ms(started.elapsed()),
                    stdout.len()
                );
                return Ok(CommandOutput {
                    status: 0,
                    stdout: stdout.into_bytes(),
                    stderr: Vec::new(),
                });
            }
            Ok(Query::Unsupported(reason)) => reason,
            Err(error)
                if error
                    .downcast_ref::<rusqlite::Error>()
                    .is_some_and(interrupted) =>
            {
                return Err(error.context("OpenCode database query timed out"));
            }
            Err(error) => format!("{error:#}"),
        };
        let output = self.inner.run(request);
        perf!(
            "opencode-db",
            "cli took={} reason={fallback}",
            crate::perf_log::ms(started.elapsed())
        );
        output
    }

    fn run_until_stdout_line(&self, request: &CommandRequest) -> Result<CommandOutput> {
        if tsv_query(&self.program, request).is_some() {
            return self.run(request);
        }
        self.inner.run_until_stdout_line(request)
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.inner.cancel();
    }
}

fn interrupted(error: &rusqlite::Error) -> bool {
    error.sqlite_error_code() == Some(rusqlite::ErrorCode::OperationInterrupted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recording {
        db: PathBuf,
        requests: Mutex<Vec<Vec<String>>>,
    }

    impl CommandRunner for Recording {
        fn run(&self, request: &CommandRequest) -> Result<CommandOutput> {
            self.requests.lock().unwrap().push(request.args.clone());
            let stdout = match request.args.first().map(String::as_str) {
                Some("db") if request.args.get(1).is_some_and(|arg| arg == "path") => {
                    format!("{}\n", self.db.display())
                }
                _ => "from-cli\n".into(),
            };
            Ok(CommandOutput {
                status: 0,
                stdout: stdout.into_bytes(),
                stderr: Vec::new(),
            })
        }
    }

    fn db_request(sql: &str) -> CommandRequest {
        CommandRequest::new(
            "opencode",
            vec!["db".into(), sql.into(), "--format".into(), "tsv".into()],
        )
    }

    fn fixture() -> (tempfile::TempDir, Arc<Recording>, DirectDbRunner) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        let connection = Connection::open(&db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE session (id TEXT, title TEXT, n INTEGER, r REAL);
                 INSERT INTO session VALUES ('a', 'tab\there', 1, 1.5), ('b', NULL, 2, NULL);",
            )
            .unwrap();
        let inner = Arc::new(Recording {
            db,
            ..Default::default()
        });
        let runner = DirectDbRunner::new(inner.clone(), "opencode".into());
        (dir, inner, runner)
    }

    #[test]
    fn answers_tsv_queries_like_the_cli() {
        let (_dir, inner, runner) = fixture();
        let output = runner
            .run(&db_request(
                "SELECT id, title, n, r FROM session ORDER BY id",
            ))
            .unwrap();
        assert_eq!(
            output.stdout_text().unwrap(),
            "id\ttitle\tn\tr\na\ttab\there\t1\t1.5\nb\t\t2\t\n"
        );
        let empty = runner
            .run(&db_request("SELECT id FROM session WHERE 0"))
            .unwrap();
        assert_eq!(empty.stdout_text().unwrap(), "");
        assert_eq!(
            *inner.requests.lock().unwrap(),
            vec![vec!["db".to_owned(), "path".to_owned()]]
        );
    }

    #[test]
    fn reads_without_writing() {
        let (_dir, inner, runner) = fixture();
        let output = runner.run(&db_request("DELETE FROM session")).unwrap();
        assert_eq!(output.stdout_text().unwrap(), "from-cli\n");
        let left = runner
            .run(&db_request("SELECT count(*) AS n FROM session"))
            .unwrap();
        assert_eq!(left.stdout_text().unwrap(), "n\n2\n");
        assert_eq!(inner.requests.lock().unwrap().len(), 2);
    }

    #[test]
    fn falls_back_to_the_cli() {
        let (_dir, _inner, runner) = fixture();
        let unknown_table = runner.run(&db_request("SELECT * FROM nope")).unwrap();
        assert_eq!(unknown_table.stdout_text().unwrap(), "from-cli\n");
        let other = CommandRequest::new("opencode", vec!["models".into()]);
        assert_eq!(
            runner.run(&other).unwrap().stdout_text().unwrap(),
            "from-cli\n"
        );
        let json = CommandRequest::new(
            "opencode",
            vec![
                "db".into(),
                "SELECT 1".into(),
                "--format".into(),
                "json".into(),
            ],
        );
        assert_eq!(
            runner.run(&json).unwrap().stdout_text().unwrap(),
            "from-cli\n"
        );
    }

    #[test]
    fn unknown_path_uses_the_cli() {
        let inner = Arc::new(Recording {
            db: PathBuf::from("/nonexistent/opencode.db"),
            ..Default::default()
        });
        let runner = DirectDbRunner::new(inner.clone(), "opencode".into());
        for _ in 0..2 {
            let output = runner.run(&db_request("SELECT 1")).unwrap();
            assert_eq!(output.stdout_text().unwrap(), "from-cli\n");
        }
        let requests = inner.requests.lock().unwrap();
        assert_eq!(requests.iter().filter(|args| args[1] == "path").count(), 1);
    }

    fn v2_fixture(mixed: bool) -> (tempfile::TempDir, Arc<Recording>, DirectDbRunner) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("channel-override.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(r#"
            CREATE TABLE session_v2(id TEXT PRIMARY KEY, title TEXT, project_id TEXT,
                directory TEXT, parent_id TEXT, fork_session_id TEXT,
                time_created INTEGER, time_updated INTEGER, time_archived INTEGER);
            CREATE TABLE session_message(id TEXT PRIMARY KEY, session_id TEXT, type TEXT,
                seq INTEGER, time_created INTEGER, time_updated INTEGER, data TEXT);
            INSERT INTO session_v2 VALUES
                ('root', 'Tabs\tand title', 'p', '/work', NULL, NULL, 10000, 15000, NULL),
                ('child', NULL, 'p', '/work', 'root', NULL, 11000, 15000, NULL),
                ('fork', 'Fork', 'p', '/work', NULL, 'root', 12000, 15000, NULL);
            INSERT INTO session_message VALUES
                ('u', 'root', 'user', 1, 10000, 10000, '{"text":"first\nquestion","time":{"created":10000}}'),
                ('a', 'root', 'assistant', 2, 11000, 11000,
                 '{"content":[{"type":"reasoning","text":"private reasoning"},{"type":"text","text":"answer"},{"type":"tool","name":"question","state":{"status":"running"}}],"time":{"created":11000}}'),
                ('cu', 'child', 'user', 1, 12000, 12000, '{"text":"child prompt"}');
        "#).unwrap();
        if mixed {
            conn.execute_batch(r#"
                CREATE TABLE session(id TEXT PRIMARY KEY, title TEXT, project_id TEXT, directory TEXT,
                    parent_id TEXT, time_created INTEGER, time_updated INTEGER, time_archived INTEGER);
                CREATE TABLE message(id TEXT, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
                CREATE TABLE part(id TEXT, message_id TEXT, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
                INSERT INTO session VALUES ('legacy', 'V1 history', 'p', '/old', NULL, 100, 200, NULL),
                    ('root', 'Stale migrated title', 'p', '/old', NULL, 1, 2, NULL);
                INSERT INTO message VALUES ('lm', 'legacy', 100, 200, '{"role":"assistant","time":{"completed":200}}'),
                    ('stale', 'root', 999999, 999999, '{"role":"user"}');
                INSERT INTO part VALUES ('lp', 'lm', 'legacy', 100, 200, '{"type":"text","text":"legacy answer"}');
            "#).unwrap();
        }
        let inner = Arc::new(Recording {
            db,
            ..Default::default()
        });
        let runner = DirectDbRunner::new(inner.clone(), "opencode".into());
        (dir, inner, runner)
    }

    #[test]
    fn v2_discovery_preserves_hierarchy_titles_and_unmigrated_v1_history() {
        for mixed in [false, true] {
            let (_dir, inner, runner) = v2_fixture(mixed);
            let out = runner.run(&db_request("SELECT json_object('id',id,'title',title,'parent',parent_id,'directory',directory) AS record FROM session ORDER BY id")).unwrap();
            let records = out
                .stdout_text()
                .unwrap()
                .lines()
                .skip(1)
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(records.len(), if mixed { 4 } else { 3 });
            assert_eq!(records[0]["parent"], "root");
            assert_eq!(records[0]["title"], "New session");
            assert!(
                records[1]["parent"].is_null(),
                "fork source must not become a parent"
            );
            assert_eq!(records.last().unwrap()["directory"], "/work");
            assert_eq!(records.last().unwrap()["title"], "Tabs\\tand title");
            if mixed {
                let old = runner
                    .run(&db_request(
                        "SELECT data FROM part WHERE session_id='legacy'",
                    ))
                    .unwrap();
                assert!(old.stdout_text().unwrap().contains("legacy answer"));
            }
            assert_eq!(
                inner.requests.lock().unwrap().len(),
                1,
                "all queries answered locally"
            );
            // Views must never be persisted in OpenCode's own store.
            let db = Connection::open(&inner.db).unwrap();
            assert!(!has_table(&db, "part").unwrap() || mixed);
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='view'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                0
            );
        }
    }

    fn last(runner: &DirectDbRunner, id: &str) -> super::super::opencode_live::LastMessage {
        let query = format!("SELECT json_object('role',json_extract(m.data,'$.role'),'created',m.time_created,'completed',json_extract(m.data,'$.time.completed'),'question',EXISTS(SELECT 1 FROM part p WHERE p.message_id=m.id AND json_extract(p.data,'$.tool')='question' AND json_extract(p.data,'$.state.status') IN ('pending','running'))) AS record FROM message m WHERE m.session_id='{id}' ORDER BY m.time_created DESC, m.id DESC LIMIT 1");
        let out = runner.run(&db_request(&query)).unwrap();
        serde_json::from_str(out.stdout_text().unwrap().lines().nth(1).unwrap()).unwrap()
    }

    #[test]
    fn v2_embedded_question_idle_and_sequence_order_drive_live_state() {
        use super::super::opencode_live::held_state;
        use crate::domain::SessionState;
        let (_dir, inner, runner) = v2_fixture(true);
        let current = last(&runner, "root");
        assert_eq!(
            held_state(Some(&current), None, 10000),
            (SessionState::NeedsInput, "question asked")
        );
        let db = Connection::open(&inner.db).unwrap();
        // An idle marker wins by seq even if its clock or ID sorts earlier.
        db.execute_batch("INSERT INTO session_message VALUES ('0idle','root','idle',3,10500,15000,'{\"outcome\":\"failed\"}');").unwrap();
        let idle = last(&runner, "root");
        assert_eq!(idle.completed, Some(10500));
        assert_eq!(
            held_state(Some(&idle), None, 10000).0,
            SessionState::NeedsInput
        );
        let child = runner.run(&db_request("SELECT MAX(m.time_created) AS child FROM session c JOIN message m ON m.id=(SELECT m2.id FROM message m2 WHERE m2.session_id=c.id ORDER BY m2.time_created DESC, m2.id DESC LIMIT 1) WHERE c.parent_id='root' AND (json_extract(m.data,'$.role')='user' OR json_extract(m.data,'$.time.completed') IS NULL)")).unwrap();
        assert_eq!(child.stdout_text().unwrap(), "child\n12000\n");
        assert_eq!(
            held_state(Some(&idle), Some(12000), 10000).0,
            SessionState::Working
        );
        let legacy = last(&runner, "legacy");
        assert_eq!(legacy.completed, Some(200));
    }

    #[test]
    fn v2_transcript_uses_text_without_reasoning_or_tool_output() {
        let (_dir, inner, runner) = v2_fixture(false);
        let out = runner
            .run(&CommandRequest::new(
                "opencode",
                vec!["export".into(), "root".into()],
            ))
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["messages"][0]["parts"][0]["text"], "first\nquestion");
        assert_eq!(value["messages"][1]["info"]["role"], "assistant");
        assert_eq!(value["messages"][1]["parts"][0]["text"], "answer");
        assert_eq!(value["messages"][1]["parts"].as_array().unwrap().len(), 1);
        assert_eq!(inner.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn v1_transcript_export_still_uses_its_cli() {
        let (_dir, inner, runner) = fixture();
        let request = CommandRequest::new("opencode", vec!["export".into(), "a".into()]);
        assert_eq!(
            runner.run(&request).unwrap().stdout_text().unwrap(),
            "from-cli\n"
        );
        assert_eq!(
            inner.requests.lock().unwrap().last().unwrap(),
            &request.args
        );
    }

    #[test]
    fn unmigrated_v1_transcript_in_a_v2_store_is_inspectable_locally() {
        let (_dir, inner, runner) = v2_fixture(true);
        let out = runner
            .run(&CommandRequest::new(
                "opencode",
                vec!["export".into(), "legacy".into()],
            ))
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["messages"][0]["info"]["role"], "assistant");
        assert_eq!(value["messages"][0]["parts"][0]["text"], "legacy answer");
        assert_eq!(inner.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn v2_shells_and_compaction_preserve_running_and_terminal_status() {
        use super::super::opencode_live::held_state;
        use crate::domain::SessionState;
        let (_dir, inner, runner) = v2_fixture(false);
        let db = Connection::open(&inner.db).unwrap();
        for kind in ["shell", "compaction"] {
            db.execute("INSERT OR REPLACE INTO session_message VALUES ('activity','fork',?1,1,15000,16000,'{\"status\":\"running\"}')", [kind]).unwrap();
            assert_eq!(
                held_state(Some(&last(&runner, "fork")), None, 10000).0,
                SessionState::Working
            );
            db.execute(
                "UPDATE session_message SET data=?1 WHERE id='activity'",
                [if kind == "shell" {
                    "{\"status\":\"exited\"}"
                } else {
                    "{\"status\":\"completed\"}"
                }],
            )
            .unwrap();
            assert_eq!(
                held_state(Some(&last(&runner, "fork")), None, 10000).0,
                SessionState::NeedsInput
            );
        }
    }

    #[test]
    fn v2_path_contract_is_tried_when_v1_db_command_is_absent() {
        struct V2Path {
            path: PathBuf,
            calls: Mutex<Vec<Vec<String>>>,
        }
        impl CommandRunner for V2Path {
            fn run(&self, request: &CommandRequest) -> Result<CommandOutput> {
                self.calls.lock().unwrap().push(request.args.clone());
                let ok = request.args == ["debug", "paths", "db"];
                Ok(CommandOutput {
                    status: if ok { 0 } else { 1 },
                    stdout: if ok {
                        format!("{}\n", self.path.display()).into_bytes()
                    } else {
                        Vec::new()
                    },
                    stderr: Vec::new(),
                })
            }
        }
        let (_dir, fixture, _) = v2_fixture(false);
        let inner = Arc::new(V2Path {
            path: fixture.db.clone(),
            calls: Mutex::default(),
        });
        let runner = DirectDbRunner::new(inner.clone(), "opencode-v2".into());
        let path = runner
            .run(&CommandRequest::new(
                "opencode-v2",
                vec!["db".into(), "path".into()],
            ))
            .unwrap();
        assert_eq!(
            path.stdout_text().unwrap().trim(),
            fixture.db.to_str().unwrap()
        );
        assert_eq!(
            *inner.calls.lock().unwrap(),
            vec![vec!["db", "path"], vec!["debug", "paths", "db"]]
        );
        assert!(runner
            .query("SELECT id FROM session", Duration::from_secs(1))
            .is_ok());
        assert_eq!(inner.calls.lock().unwrap().len(), 2);
    }
}
