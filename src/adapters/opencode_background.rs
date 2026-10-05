//! Shells that `cursor-opencode-provider` detaches from OpenCode's bash tool.
//!
//! The plugin runs a background or timed-out command under `nohup`, logging to
//! `$TMPDIR/cursor-opencode-bg.XXXXXX` or `cursor-opencode-shell.XXXXXX`, and
//! the tool call returns with only the pid in its output. OpenCode then shows
//! the turn as over while the command, often a watcher that the agent reports
//! back from, keeps running.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::Result;
use serde::Deserialize;

use super::opencode_live::{now_ms, parse_etime};
use crate::process::{CommandRequest, CommandRunner};

const LOG_PREFIXES: &[&str] = &["cursor-opencode-bg.", "cursor-opencode-shell."];
const PID_MARKERS: &[&str] = &[
    "__CURSOR_BACKGROUND_SHELL__",
    "__CURSOR_SHELL_BACKGROUND__",
    "in the background (pid ",
];
/// The shell starts right after `mktemp` creates its log; `ps` reports
/// elapsed time in whole seconds.
const START_BEFORE_LOG_MS: u64 = 2_000;
const START_AFTER_LOG_MS: u64 = 15_000;
/// OpenCode writes the tool part when the model starts the call, before the
/// command runs and creates its log.
const PART_BEFORE_LOG_MS: u64 = 5 * 60_000;
const PART_AFTER_LOG_MS: u64 = 5_000;
/// Until its call returns, a shell's pid is not in the tool part yet.
const UNRESOLVED_RETRY_MS: u64 = 10 * 60_000;
const MAX_LOOKUPS: usize = 32;

#[derive(Clone, Debug, Eq, PartialEq)]
struct Shell {
    session: String,
    pid: u32,
}

#[derive(Debug, Eq, PartialEq)]
struct Log {
    path: PathBuf,
    created_ms: u64,
}

pub(super) struct BackgroundShells {
    dirs: Vec<PathBuf>,
    /// `None` once a log has no live shell of a known session.
    resolved: Mutex<BTreeMap<(PathBuf, u64), Option<Shell>>>,
    last: Mutex<BTreeSet<String>>,
}

impl BackgroundShells {
    pub fn host() -> Self {
        let mut dirs = vec![std::env::temp_dir()];
        if !dirs.iter().any(|dir| dir == Path::new("/tmp")) {
            dirs.push(PathBuf::from("/tmp"));
        }
        Self::in_dirs(dirs)
    }

    fn in_dirs(dirs: Vec<PathBuf>) -> Self {
        Self {
            dirs,
            resolved: Mutex::default(),
            last: Mutex::default(),
        }
    }

    /// Root sessions with a live background shell. `query` runs SQL through
    /// `opencode db` and returns its TSV output.
    pub fn running(
        &self,
        runner: &dyn CommandRunner,
        query: &dyn Fn(String) -> Result<String>,
    ) -> BTreeSet<String> {
        let logs = self
            .dirs
            .iter()
            .flat_map(|dir| list_logs(dir))
            .collect::<Vec<_>>();
        let now = now_ms();
        let mut resolved = self
            .resolved
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        resolved.retain(|(path, created), _| {
            logs.iter()
                .any(|log| &log.path == path && log.created_ms == *created)
        });
        let unseen = logs
            .iter()
            .filter(|log| !resolved.contains_key(&(log.path.clone(), log.created_ms)))
            .collect::<Vec<_>>();
        if !unseen.is_empty() {
            let processes = process_starts(runner, now);
            let mut lookups = Vec::new();
            for log in unseen {
                let started = started_with(&processes, log.created_ms);
                if !started.is_empty() && lookups.len() < MAX_LOOKUPS {
                    lookups.push((log, started));
                } else if started.is_empty() && settled(log, now) {
                    resolved.insert((log.path.clone(), log.created_ms), None);
                }
            }
            if !lookups.is_empty() {
                let windows = lookups
                    .iter()
                    .map(|(log, _)| part_window(log.created_ms))
                    .collect::<Vec<_>>();
                if let Ok(output) = query(parts_query(&windows)) {
                    let parts = parse_parts(&output);
                    for (log, started) in lookups {
                        let shell = match_shell(&parts, log.created_ms, &started);
                        if shell.is_some() || settled(log, now) {
                            resolved.insert((log.path.clone(), log.created_ms), shell);
                        }
                    }
                }
            }
        }
        let mut running = BTreeSet::new();
        for shell in resolved.values_mut() {
            match shell {
                Some(live) if crate::holds::process_alive(live.pid) => {
                    running.insert(live.session.clone());
                }
                Some(_) => *shell = None,
                None => {}
            }
        }
        *self.last.lock().unwrap_or_else(|error| error.into_inner()) = running.clone();
        running
    }

    /// The sessions the latest `running` call found.
    pub fn last(&self) -> BTreeSet<String> {
        self.last
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

fn settled(log: &Log, now: u64) -> bool {
    now.saturating_sub(log.created_ms) > UNRESOLVED_RETRY_MS
}

fn list_logs(dir: &Path) -> Vec<Log> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            LOG_PREFIXES.iter().any(|prefix| {
                name.strip_prefix(prefix)
                    .is_some_and(|rest| !rest.is_empty() && !rest.contains('.'))
            })
        })
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            let created = metadata.created().or_else(|_| metadata.modified()).ok()?;
            Some(Log {
                path: entry.path(),
                created_ms: created.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64,
            })
        })
        .collect()
}

/// Every live process with its approximate start time.
fn process_starts(runner: &dyn CommandRunner, now: u64) -> Vec<(u32, u64)> {
    let mut request = CommandRequest::new("ps", vec!["axo".into(), "pid=,etime=".into()]);
    request.timeout = Duration::from_secs(4);
    let Ok(output) = runner.run(&request) else {
        return Vec::new();
    };
    let Ok(text) = output.stdout_text() else {
        return Vec::new();
    };
    parse_process_starts(text, now)
}

fn parse_process_starts(text: &str, now: u64) -> Vec<(u32, u64)> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let elapsed = parse_etime(fields.next()?)?;
            Some((pid, now.saturating_sub(elapsed.saturating_mul(1_000))))
        })
        .collect()
}

fn started_with(processes: &[(u32, u64)], created_ms: u64) -> Vec<u32> {
    processes
        .iter()
        .filter(|(_, started)| {
            *started + START_BEFORE_LOG_MS >= created_ms
                && *started <= created_ms + START_AFTER_LOG_MS
        })
        .map(|(pid, _)| *pid)
        .collect()
}

fn part_window(created_ms: u64) -> (u64, u64) {
    (
        created_ms.saturating_sub(PART_BEFORE_LOG_MS),
        created_ms + PART_AFTER_LOG_MS,
    )
}

/// The `part.id` prefix for a time: OpenCode IDs start with the creation time
/// in milliseconds times 4096 plus a counter, as 48 bits of hex.
fn part_id_at(ms: u64) -> String {
    format!("prt_{:012x}", (u128::from(ms) * 4096) % (1 << 48))
}

/// Tool parts written around these times that report a backgrounded pid.
/// Selecting by ID range keeps SQLite on the primary-key index instead of
/// reading every part in a database that can be many gigabytes.
fn parts_query(windows: &[(u64, u64)]) -> String {
    let ranges = windows
        .iter()
        .map(|(from, to)| {
            let (low, high) = (part_id_at(*from), part_id_at(to + 1));
            let joiner = if low <= high { "AND" } else { "OR" };
            format!("(p.id >= '{low}' {joiner} p.id < '{high}')")
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    let markers = PID_MARKERS
        .iter()
        .map(|marker| format!("instr(p.data, '{marker}') > 0"))
        .collect::<Vec<_>>()
        .join(" OR ");
    format!(
        "SELECT json_object('session', COALESCE(s.parent_id, s.id), 'created', p.time_created, 'text', COALESCE(substr(json_extract(p.data, '$.state.metadata.output'), -400), '') || char(10) || COALESCE(substr(json_extract(p.data, '$.state.output'), -400), '')) AS record FROM part p JOIN session s ON s.id = p.session_id WHERE ({ranges}) AND ({markers})"
    )
}

#[derive(Debug, Deserialize)]
struct Part {
    session: String,
    created: u64,
    text: String,
}

fn parse_parts(output: &str) -> Vec<Part> {
    output
        .lines()
        .skip(1)
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn match_shell(parts: &[Part], created_ms: u64, started: &[u32]) -> Option<Shell> {
    let (from, to) = part_window(created_ms);
    parts
        .iter()
        .filter(|part| (from..=to).contains(&part.created))
        .find_map(|part| {
            reported_pids(&part.text)
                .into_iter()
                .find(|pid| started.contains(pid))
                .map(|pid| Shell {
                    session: part.session.clone(),
                    pid,
                })
        })
}

fn reported_pids(text: &str) -> Vec<u32> {
    PID_MARKERS
        .iter()
        .flat_map(|marker| text.match_indices(marker))
        .filter_map(|(index, marker)| {
            let rest = &text[index + marker.len()..];
            let digits = rest
                .find(|character: char| !character.is_ascii_digit())
                .map_or(rest, |end| &rest[..end]);
            digits.parse().ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_ids_follow_opencode_encoding() {
        assert_eq!(part_id_at(1_791_210_735_391), "prt_10c7ac71f000");
    }

    #[test]
    fn query_ranges_wrap_with_the_id_counter() {
        let wrap_ms = (1u64 << 48) / 4096;
        let query = parts_query(&[(wrap_ms - 10, wrap_ms + 10)]);
        assert!(query.contains("(p.id >= 'prt_ffffffff6000' OR p.id < 'prt_00000000b000')"));
        let query = parts_query(&[(1_000, 2_000)]);
        assert!(query.contains("(p.id >= 'prt_0000003e8000' AND p.id < 'prt_0000007d1000')"));
    }

    #[test]
    fn reads_pids_from_raw_and_sanitized_output() {
        assert_eq!(
            reported_pids("__CURSOR_BACKGROUND_SHELL__65622:/tmp/cursor-opencode-bg.dWzTc5\n"),
            vec![65622]
        );
        assert_eq!(
            reported_pids("log\n__CURSOR_SHELL_BACKGROUND__812:/tmp/x\nTerminated: 15"),
            vec![812]
        );
        assert_eq!(
            reported_pids("Still running in the background (pid 4242) after 30000ms."),
            vec![4242]
        );
        assert!(reported_pids("exit 0").is_empty());
    }

    #[test]
    fn matches_the_part_that_reports_the_shell_started_with_the_log() {
        let parts = vec![
            Part {
                session: "ses_other".into(),
                created: 100_000,
                text: "Started in the background (pid 7).".into(),
            },
            Part {
                session: "ses_watch".into(),
                created: 1_000_000,
                text: "__CURSOR_BACKGROUND_SHELL__42:/tmp/cursor-opencode-bg.a\n".into(),
            },
        ];
        assert_eq!(
            match_shell(&parts, 1_000_400, &[42]),
            Some(Shell {
                session: "ses_watch".into(),
                pid: 42
            })
        );
        assert_eq!(match_shell(&parts, 1_000_400, &[43]), None);
        assert_eq!(match_shell(&parts, 1_000_400, &[7]), None);
    }

    #[test]
    fn processes_started_with_a_log_are_candidates() {
        let processes = parse_process_starts("  10 01:40\n  11 00:05\n  12 1-00:00:00\n", 200_000);
        assert_eq!(processes, vec![(10, 100_000), (11, 195_000), (12, 0)]);
        assert_eq!(started_with(&processes, 101_000), vec![10]);
        assert!(started_with(&processes, 150_000).is_empty());
    }

    #[test]
    fn lists_only_plugin_logs() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "cursor-opencode-bg.dWzTc5",
            "cursor-opencode-shell.Ab12Cd",
            "cursor-opencode-shell-status.Ab12Cd",
            "cursor-opencode-bg.",
            "unrelated.log",
        ] {
            std::fs::write(dir.path().join(name), "").unwrap();
        }
        let mut names = list_logs(dir.path())
            .into_iter()
            .map(|log| log.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(
            names,
            ["cursor-opencode-bg.dWzTc5", "cursor-opencode-shell.Ab12Cd"]
        );
    }

    #[test]
    fn a_log_without_a_live_shell_settles_without_a_lookup() {
        struct NoProcesses;
        impl CommandRunner for NoProcesses {
            fn run(&self, _: &CommandRequest) -> Result<crate::process::CommandOutput> {
                Ok(crate::process::CommandOutput {
                    status: 0,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                })
            }
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cursor-opencode-bg.old"), "").unwrap();
        let shells = BackgroundShells::in_dirs(vec![dir.path().to_owned()]);
        let queried = std::cell::Cell::new(false);
        let query = |_: String| {
            queried.set(true);
            Ok(String::new())
        };
        assert!(shells.running(&NoProcesses, &query).is_empty());
        assert!(!queried.get());
    }
}
