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
    /// Resolved once with `opencode db path`, which follows OpenCode's own
    /// rules: release channels and `OPENCODE_DB` select different files.
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
        let mut request =
            CommandRequest::new(self.program.clone(), vec!["db".into(), "path".into()]);
        request.timeout = Duration::from_secs(8);
        let output = self.inner.run(&request).ok()?;
        if output.status != 0 {
            return None;
        }
        let path = PathBuf::from(output.stdout_text().ok()?.trim());
        (path.is_absolute() && path.is_file()).then_some(path)
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
        let mut statement = match db.prepare(sql) {
            Ok(statement) => statement,
            Err(error) => return Ok(Query::Unsupported(error.to_string())),
        };
        let deadline = Instant::now() + timeout;
        let cancelled = self.cancelled.clone();
        db.progress_handler(
            10_000,
            Some(move || cancelled.load(Ordering::SeqCst) || Instant::now() >= deadline),
        );
        Ok(Query::Rows(render_tsv(&mut statement)?))
    }
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
}
