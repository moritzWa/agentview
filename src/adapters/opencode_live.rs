//! Live state for persisted OpenCode sessions.
//!
//! OpenCode keeps a session's busy/idle status and pending permission requests
//! in the memory of the process that runs it; the database only records
//! messages. A turn that was cut off by a crash or kill stays "in progress" in
//! the database forever. State is therefore derived from three sources, most
//! authoritative first:
//!
//! 1. The screen of an OpenCode frontend this dashboard holds in the
//!    background, which shows permission and question prompts.
//! 2. The latest persisted message, but only for a session a live `opencode`
//!    process holds, and only when that message was written after the process
//!    started.
//! 3. Otherwise the session is closed history.
//!
//! Windows has no process probe, so there every session is history.
#![cfg_attr(not(unix), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::domain::SessionState;
#[cfg(unix)]
use crate::process::{CommandRequest, CommandRunner, ProcessRunner};

#[cfg(unix)]
const PROBE_TIMEOUT: Duration = Duration::from_secs(4);
/// `ps` reports elapsed time in whole seconds, so a process start is known to
/// about a second. Records this close to the start still count as its own.
const START_SLACK_MS: u64 = 2_000;
/// A session with an unfinished turn updated this recently claims a process
/// before idle sessions do. A long tool call writes nothing while it runs, so
/// this is a window rather than "updated since the process started".
const ACTIVE_WINDOW_MS: u64 = 10 * 60 * 1_000;
/// The footer and prompt panels sit in the last rows, below the transcript.
const PROMPT_ROWS: usize = 8;

/// A live `opencode` process and the session it is known or presumed to run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Holder {
    pub pid: u32,
    pub started_ms: u64,
    pub target: Target,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Target {
    /// `--session ID` on the command line.
    Session(String),
    /// A new, continued, or forked session in this directory; the process's
    /// working directory when the command line names none.
    Directory(Option<PathBuf>),
}

/// The newest message of a session, as selected by the discovery query.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(super) struct LastMessage {
    pub role: Option<String>,
    pub created: u64,
    #[serde(default)]
    pub completed: Option<u64>,
    /// A `question` tool call in this message is still waiting for an answer.
    #[serde(default)]
    pub question: u8,
}

/// Identity of a discovered root session used to attribute holders.
pub(super) struct Candidate<'a> {
    pub id: &'a str,
    pub directory: &'a Path,
    pub created_ms: u64,
    pub updated_ms: u64,
    /// An unfinished turn written recently: some live process is running it.
    pub active: bool,
}

/// Every live `opencode` process that runs a TUI or a headless `run`.
pub(super) fn probe_host_holders() -> Vec<Holder> {
    #[cfg(unix)]
    {
        probe_with(&ProcessRunner, now_ms())
    }
    #[cfg(not(unix))]
    {
        Vec::new()
    }
}

#[cfg(unix)]
fn probe_with(runner: &dyn CommandRunner, now_ms: u64) -> Vec<Holder> {
    let mut request = CommandRequest::new(
        "ps",
        vec!["axww".into(), "-o".into(), "pid=,etime=,args=".into()],
    );
    request.timeout = PROBE_TIMEOUT;
    let Ok(output) = runner.run(&request) else {
        return Vec::new();
    };
    let Ok(text) = output.stdout_text() else {
        return Vec::new();
    };
    let own_pid = std::process::id();
    let mut holders = parse_ps(text, now_ms, &process_args)
        .into_iter()
        .filter(|holder| holder.pid != own_pid)
        .collect::<Vec<_>>();
    let unnamed = holders
        .iter()
        .filter(|holder| matches!(holder.target, Target::Directory(None)))
        .map(|holder| holder.pid)
        .collect::<Vec<_>>();
    if !unnamed.is_empty() {
        let cwds = process_cwds(&unnamed, runner);
        for holder in &mut holders {
            if matches!(holder.target, Target::Directory(None)) {
                holder.target = Target::Directory(cwds.get(&holder.pid).cloned());
            }
        }
        holders.retain(|holder| !matches!(holder.target, Target::Directory(None)));
    }
    holders
}

/// Parse `ps -o pid=,etime=,args=` rows into OpenCode session holders.
/// `ps` joins arguments with spaces, so a prompt passed as `--prompt=...`
/// would split into stray words; `exact_args` supplies the real argument
/// vector where the platform exposes it.
pub(super) fn parse_ps(
    output: &str,
    now_ms: u64,
    exact_args: &dyn Fn(u32) -> Option<Vec<String>>,
) -> Vec<Holder> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse::<u32>().ok()?;
            let elapsed = parse_etime(fields.next()?)?;
            let program = Path::new(fields.next()?).file_name()?.to_str()?;
            if program != "opencode" {
                return None;
            }
            let args = exact_args(pid)
                .map(|mut args| {
                    args.drain(..1.min(args.len()));
                    args
                })
                .unwrap_or_else(|| fields.map(str::to_owned).collect());
            let args = args.iter().map(String::as_str).collect::<Vec<_>>();
            Some(Holder {
                pid,
                started_ms: now_ms.saturating_sub(elapsed.saturating_mul(1_000)),
                target: classify(&args)?,
            })
        })
        .collect()
}

/// The exact argument vector of a process owned by this user.
#[cfg(target_os = "linux")]
fn process_args(pid: u32) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let args = raw
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect::<Vec<_>>();
    (!args.is_empty()).then_some(args)
}

/// The exact argument vector of a process owned by this user, from
/// `KERN_PROCARGS2`: argc, the executable path, padding, then argv.
#[cfg(target_os = "macos")]
fn process_args(pid: u32) -> Option<Vec<String>> {
    let pid = libc::c_int::try_from(pid).ok()?;
    let mut argmax: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    let status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            2,
            (&mut argmax as *mut libc::c_int).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 || argmax <= 0 {
        return None;
    }
    let mut buffer = vec![0_u8; argmax as usize];
    let mut size = buffer.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 || size < 4 {
        return None;
    }
    buffer.truncate(size);
    let argc = i32::from_ne_bytes(buffer[..4].try_into().ok()?);
    let rest = &buffer[4..];
    let mut position = rest.iter().position(|byte| *byte == 0)?;
    while rest.get(position) == Some(&0) {
        position += 1;
    }
    let mut args = Vec::new();
    for _ in 0..argc.max(0) {
        let end = position + rest.get(position..)?.iter().position(|byte| *byte == 0)?;
        args.push(String::from_utf8_lossy(&rest[position..end]).into_owned());
        position = end + 1;
    }
    (!args.is_empty()).then_some(args)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn process_args(_pid: u32) -> Option<Vec<String>> {
    None
}

/// `[[dd-]hh:]mm:ss` as printed by `ps -o etime`.
fn parse_etime(value: &str) -> Option<u64> {
    let (days, clock) = match value.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, value),
    };
    let parts = clock
        .split(':')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    let seconds = match parts.as_slice() {
        [minutes, seconds] => minutes * 60 + seconds,
        [hours, minutes, seconds] => hours * 3_600 + minutes * 60 + seconds,
        _ => return None,
    };
    Some(days * 86_400 + seconds)
}

/// The session an `opencode` command line runs, or `None` for subcommands
/// that do not run one in this process (servers, attach clients, `db`, ...).
fn classify(args: &[&str]) -> Option<Target> {
    // Options of the TUI and `run` that take a separate value.
    const VALUE_FLAGS: &[&str] = &[
        "--log-level",
        "--port",
        "--hostname",
        "--mdns-domain",
        "--cors",
        "-m",
        "--model",
        "-s",
        "--session",
        "--prompt",
        "--agent",
        "--replay-limit",
        "--dir",
        "--title",
        "--command",
        "--format",
        "--variant",
        "--attach",
        "-p",
        "--password",
        "-u",
        "--username",
    ];
    let mut session = None;
    let mut dir = None;
    let mut fork = false;
    let mut attach = false;
    let mut positional = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index];
        index += 1;
        if arg == "--" {
            break;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with('-') => (flag, Some(value)),
            _ => (arg, None),
        };
        if !flag.starts_with('-') {
            positional.push(arg);
            continue;
        }
        let value = if VALUE_FLAGS.contains(&flag) {
            inline.or_else(|| {
                let value = args.get(index).copied();
                index += 1;
                value
            })
        } else {
            inline
        };
        match flag {
            "-s" | "--session" => session = value.map(str::to_owned),
            "--dir" => dir = value.map(PathBuf::from),
            "--fork" => fork = true,
            "--attach" => attach = true,
            _ => {}
        }
    }
    let project = match positional.first().copied() {
        None => None,
        Some("run") => {
            if attach {
                return None;
            }
            dir
        }
        Some("pr") => None,
        Some(
            "completion" | "acp" | "mcp" | "attach" | "debug" | "providers" | "auth" | "agent"
            | "upgrade" | "uninstall" | "serve" | "web" | "models" | "stats" | "export" | "import"
            | "github" | "session" | "plugin" | "plug" | "db",
        ) => return None,
        Some(path) => Some(PathBuf::from(path)),
    };
    match session {
        Some(id) if !fork => Some(Target::Session(id)),
        _ => Some(Target::Directory(project)),
    }
}

#[cfg(unix)]
fn process_cwds(pids: &[u32], runner: &dyn CommandRunner) -> BTreeMap<u32, PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let _ = runner;
        pids.iter()
            .filter_map(|pid| Some((*pid, std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?)))
            .collect()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let list = pids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let mut request = CommandRequest::new(
            "lsof",
            vec![
                "-a".into(),
                "-d".into(),
                "cwd".into(),
                "-p".into(),
                list,
                "-Fpn".into(),
            ],
        );
        request.timeout = PROBE_TIMEOUT;
        // lsof exits non-zero when a listed PID has exited; what it printed for
        // the others is still valid.
        runner
            .run(&request)
            .ok()
            .and_then(|output| output.stdout_text().ok().map(parse_lsof_cwds))
            .unwrap_or_default()
    }
}

/// Parse `lsof -Fpn` output: `p<pid>` starts a process, `n<path>` is its cwd.
#[cfg(any(test, all(unix, not(target_os = "linux"))))]
pub(super) fn parse_lsof_cwds(output: &str) -> BTreeMap<u32, PathBuf> {
    let mut cwds = BTreeMap::new();
    let mut current = None;
    for line in output.lines() {
        if let Some(pid) = line.strip_prefix('p') {
            current = pid.parse::<u32>().ok();
        } else if let (Some(path), Some(pid)) = (line.strip_prefix('n'), current) {
            cwds.entry(pid).or_insert_with(|| PathBuf::from(path));
        }
    }
    cwds
}

/// Session IDs named on live command lines. Discovery loads these even when
/// they fall outside the recent-history window.
pub(super) fn named_sessions(holders: &[Holder]) -> BTreeSet<String> {
    holders
        .iter()
        .filter_map(|holder| match &holder.target {
            Target::Session(id) => Some(id.clone()),
            Target::Directory(_) => None,
        })
        .collect()
}

/// Map each held session to its holder. A process that names its session
/// holds exactly that one. Every other process holds at most one session in
/// its directory that changed after it started. Sessions with an active turn
/// claim first, then the most recently updated. Each goes to the process that
/// created it (the latest one started before the session was created) when
/// that process is still free, otherwise to the newest free process. A fresh
/// TUI on its home screen that touched nothing holds none.
pub(super) fn assign(holders: &[Holder], candidates: &[Candidate<'_>]) -> BTreeMap<String, Holder> {
    let known = candidates
        .iter()
        .map(|candidate| candidate.id)
        .collect::<BTreeSet<_>>();
    let mut assigned = BTreeMap::new();
    for holder in holders {
        if let Target::Session(id) = &holder.target {
            if known.contains(id.as_str()) {
                assigned
                    .entry(id.clone())
                    .and_modify(|existing: &mut Holder| {
                        if holder.started_ms > existing.started_ms {
                            *existing = holder.clone();
                        }
                    })
                    .or_insert_with(|| holder.clone());
            }
        }
    }
    let mut by_directory = holders
        .iter()
        .filter_map(|holder| match &holder.target {
            Target::Directory(Some(dir)) => Some((holder, canonical(dir))),
            _ => None,
        })
        .collect::<Vec<_>>();
    if by_directory.is_empty() {
        return assigned;
    }
    by_directory.sort_by_key(|(holder, _)| std::cmp::Reverse(holder.started_ms));
    let mut ordered = candidates
        .iter()
        .map(|candidate| (candidate, canonical(candidate.directory)))
        .collect::<Vec<_>>();
    ordered.sort_by_key(|(candidate, _)| {
        (
            std::cmp::Reverse(candidate.active),
            std::cmp::Reverse(candidate.updated_ms),
        )
    });
    let mut claimed = vec![false; by_directory.len()];
    for (candidate, dir) in &ordered {
        if assigned.contains_key(candidate.id) {
            continue;
        }
        let free = (0..by_directory.len())
            .filter(|&index| {
                let (holder, holder_dir) = &by_directory[index];
                !claimed[index]
                    && holder_dir == dir
                    && holder.started_ms <= candidate.updated_ms + START_SLACK_MS
            })
            .collect::<Vec<_>>();
        let pick = free
            .iter()
            .copied()
            .find(|&index| {
                by_directory[index].0.started_ms <= candidate.created_ms + START_SLACK_MS
            })
            .or_else(|| free.first().copied());
        if let Some(index) = pick {
            claimed[index] = true;
            assigned.insert(candidate.id.to_owned(), by_directory[index].0.clone());
        }
    }
    assigned
}

/// Whether a session has an unfinished turn, its own or a subagent's, that
/// was written within the active window.
pub(super) fn is_active(
    last: Option<&LastMessage>,
    child_turn_ms: Option<u64>,
    updated_ms: u64,
    now_ms: u64,
) -> bool {
    let unfinished = child_turn_ms.is_some()
        || last.is_some_and(|last| {
            matches!(
                (last.role.as_deref(), last.completed),
                (Some("assistant"), None) | (Some("user"), _)
            )
        });
    unfinished && now_ms.saturating_sub(updated_ms) <= ACTIVE_WINDOW_MS
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

/// The earliest start among live processes that could be running this
/// session: one naming it, or one working in its directory. A turn written
/// before all of them started was cut off with a process that has exited.
pub(super) fn earliest_start(holders: &[Holder], id: &str, directory: &Path) -> Option<u64> {
    let mut directory_canonical = None;
    holders
        .iter()
        .filter(|holder| match &holder.target {
            Target::Session(session) => session == id,
            Target::Directory(Some(dir)) => {
                canonical(dir) == *directory_canonical.get_or_insert_with(|| canonical(directory))
            }
            Target::Directory(None) => false,
        })
        .map(|holder| holder.started_ms)
        .min()
}

/// State from the database for a session held by a live process, counting
/// only turns written since `since_ms`. `child_turn_ms` is when the newest
/// unfinished turn of one of its subagent sessions began: a background task
/// (`task` with `background: true`) keeps working after the parent's own turn
/// has ended.
pub(super) fn held_state(
    last: Option<&LastMessage>,
    child_turn_ms: Option<u64>,
    since_ms: u64,
) -> (SessionState, &'static str) {
    let current = |created: u64| created + START_SLACK_MS >= since_ms;
    if let Some(last) = last.filter(|last| current(last.created)) {
        match (last.role.as_deref(), last.completed) {
            (Some("assistant"), None) if last.question != 0 => {
                return (SessionState::NeedsInput, "question asked");
            }
            (Some("assistant"), None) | (Some("user"), _) => {
                return (SessionState::Working, "running turn");
            }
            _ => {}
        }
    }
    if child_turn_ms.is_some_and(current) {
        return (SessionState::Working, "background task");
    }
    (SessionState::NeedsInput, "waiting at prompt")
}

/// Combine a background screen's reading with the database's. An idle prompt
/// on this dashboard's screen does not rule out a background subagent, or a
/// turn another process is running for the same session, so it yields to a
/// database that shows work. Permission, question, and interrupt hints are
/// shown only by the process running the turn and always win.
pub(super) fn combine(
    screen: Option<(SessionState, &'static str)>,
    database: (SessionState, &'static str),
) -> (SessionState, &'static str) {
    match screen {
        Some((SessionState::NeedsInput, "waiting at prompt"))
            if database.0 == SessionState::Working =>
        {
            database
        }
        Some(screen) => screen,
        None => database,
    }
}

/// The state a held screen gives a row between discoveries, or `None` to
/// keep the row's state. An idle prompt is left to discovery: a background
/// task or a plugin hold can keep a session working without showing it, and
/// the managed server reports an idle session as completed.
pub(super) fn settle_from_screen(
    screen: (SessionState, &'static str),
) -> Option<(SessionState, &'static str)> {
    (screen != (SessionState::NeedsInput, "waiting at prompt")).then_some(screen)
}

/// State of a session whose OpenCode terminal this dashboard holds in the
/// background, or `None` when the screen does not settle it (a login or
/// startup screen, or a remapped command-palette key).
pub(super) fn screen_state(screen: &str) -> Option<(SessionState, &'static str)> {
    let rows = screen
        .lines()
        .rev()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .take(PROMPT_ROWS)
        .collect::<Vec<_>>();
    let shows = |marker: &str| rows.iter().any(|row| row.contains(marker));
    if shows("esc interrupt") || shows("esc again to interrupt") {
        return Some((SessionState::Working, "running turn"));
    }
    // Permission and question panels replace the prompt, and with it the
    // footer's interrupt hint, while the turn waits on the user. The panel's
    // title can sit above these rows; its option row is always at the bottom.
    if rows
        .iter()
        .any(|row| row.contains("Allow once") && row.contains("Reject"))
    {
        return Some((SessionState::NeedsInput, "permission requested"));
    }
    if shows("esc dismiss") {
        return Some((SessionState::NeedsInput, "question asked"));
    }
    if shows("ctrl+p commands") {
        return Some((SessionState::NeedsInput, "waiting at prompt"));
    }
    None
}

pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<&str> {
        line.split_whitespace().collect()
    }

    #[test]
    fn etime_formats() {
        assert_eq!(parse_etime("08:31"), Some(511));
        assert_eq!(parse_etime("01:02:03"), Some(3_723));
        assert_eq!(parse_etime("2-01:00:00"), Some(2 * 86_400 + 3_600));
        assert_eq!(parse_etime("x"), None);
    }

    #[test]
    fn command_lines_name_their_session_or_directory() {
        assert_eq!(
            classify(&args("--session ses_1")),
            Some(Target::Session("ses_1".into()))
        );
        assert_eq!(
            classify(&args("-s=ses_1 --model a/b")),
            Some(Target::Session("ses_1".into()))
        );
        assert_eq!(
            classify(&args("--dangerously-skip-permissions")),
            Some(Target::Directory(None))
        );
        assert_eq!(
            classify(&args("--model a/b /work/app")),
            Some(Target::Directory(Some("/work/app".into())))
        );
        assert_eq!(
            classify(&args("run --dir /work --title t -f a.md -- hello")),
            Some(Target::Directory(Some("/work".into())))
        );
        assert_eq!(
            classify(&args("--session ses_1 --fork")),
            Some(Target::Directory(None))
        );
        assert_eq!(classify(&args("run --attach http://x -s ses_1")), None);
        for sub in ["serve", "db", "export ses_1", "attach http://x", "models"] {
            assert_eq!(classify(&args(sub)), None, "{sub}");
        }
    }

    #[test]
    fn ps_rows_keep_only_opencode_session_runners() {
        let rows = "\
91244       08:31 /Users/m/.opencode/bin/opencode --session ses_a
75818       12:52 opencode --dangerously-skip-permissions
 4242       00:01 opencode db SELECT 1
 5151       00:10 /usr/bin/vim opencode.rs
 6161       00:05 /bin/opencode-helper --session ses_b
";
        let holders = parse_ps(rows, 1_000_000, &|_| None);
        assert_eq!(
            holders,
            vec![
                Holder {
                    pid: 91244,
                    started_ms: 1_000_000 - 511_000,
                    target: Target::Session("ses_a".into()),
                },
                Holder {
                    pid: 75818,
                    started_ms: 1_000_000 - 772_000,
                    target: Target::Directory(None),
                },
            ]
        );
    }

    #[test]
    fn exact_arguments_keep_a_prompt_with_spaces_and_flags_whole() {
        let rows = "7070       00:03 opencode --model=a/b --prompt=explain the --session flag\n";
        let split = parse_ps(rows, 10_000, &|_| None);
        assert_eq!(split[0].target, Target::Session("flag".into()));
        let exact = parse_ps(rows, 10_000, &|_| {
            Some(vec![
                "opencode".into(),
                "--model=a/b".into(),
                "--prompt=explain the --session flag".into(),
            ])
        });
        assert_eq!(exact[0].target, Target::Directory(None));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn reads_this_process_exact_arguments() {
        let args = process_args(std::process::id()).unwrap();
        assert_eq!(args, std::env::args().collect::<Vec<_>>());
    }

    #[test]
    fn lsof_cwd_parser() {
        let cwds = parse_lsof_cwds("p75818\nfcwd\nn/Users/m/Code\np81942\nfcwd\nn/work\n");
        assert_eq!(cwds.get(&75818), Some(&PathBuf::from("/Users/m/Code")));
        assert_eq!(cwds.get(&81942), Some(&PathBuf::from("/work")));
    }

    fn candidate<'a>(
        id: &'a str,
        directory: &'a Path,
        created_ms: u64,
        updated_ms: u64,
    ) -> Candidate<'a> {
        Candidate {
            id,
            directory,
            created_ms,
            updated_ms,
            active: false,
        }
    }

    #[test]
    fn a_session_mid_turn_claims_the_process_before_one_it_created_and_left() {
        // Observed live: a TUI resumed an older session and is running a long
        // polling loop in it, while a session it created after starting sits
        // idle and was updated more recently.
        let work = PathBuf::from("/nonexistent-agentview/work");
        let mut running = candidate("ses_running", &work, 100, 20_000);
        running.active = true;
        let left = candidate("ses_left", &work, 12_000, 30_000);
        let tui = holder(1, 10_000, Target::Directory(Some(work.clone())));
        let assigned = assign(std::slice::from_ref(&tui), &[left, running]);
        assert_eq!(assigned.len(), 1);
        assert_eq!(assigned["ses_running"], tui);
    }

    #[test]
    fn only_recent_unfinished_turns_are_active() {
        let open = LastMessage {
            role: Some("assistant".into()),
            created: 1_000,
            completed: None,
            question: 0,
        };
        let done = LastMessage {
            completed: Some(2_000),
            ..open.clone()
        };
        let now = ACTIVE_WINDOW_MS + 100_000;
        assert!(is_active(Some(&open), None, now - 1_000, now));
        assert!(!is_active(Some(&open), None, 1_000, now));
        assert!(!is_active(Some(&done), None, now - 1_000, now));
        assert!(is_active(Some(&done), Some(now - 5_000), now - 1_000, now));
    }

    fn holder(pid: u32, started_ms: u64, target: Target) -> Holder {
        Holder {
            pid,
            started_ms,
            target,
        }
    }

    #[test]
    fn named_holders_claim_exactly_their_session() {
        let work = PathBuf::from("/nonexistent-agentview/work");
        let candidates = [
            candidate("ses_named", &work, 50, 9_000),
            candidate("ses_other", &work, 60, 9_500),
        ];
        let named = holder(1, 100, Target::Session("ses_named".into()));
        let assigned = assign(std::slice::from_ref(&named), &candidates);
        assert_eq!(assigned.len(), 1);
        assert_eq!(assigned["ses_named"], named);
    }

    #[test]
    fn a_session_belongs_to_the_process_that_created_it_not_a_newer_idle_one() {
        // Observed live: a TUI created the session at 10s and is still working
        // in it; a second TUI started in the same directory at 900s sits on its
        // home screen.
        let work = PathBuf::from("/nonexistent-agentview/work");
        let candidates = [candidate("ses_busy", &work, 10_000, 960_000)];
        let creator = holder(1, 8_000, Target::Directory(Some(work.clone())));
        let fresh = holder(2, 900_000, Target::Directory(Some(work.clone())));
        let assigned = assign(&[fresh, creator.clone()], &candidates);
        assert_eq!(assigned.len(), 1);
        assert_eq!(assigned["ses_busy"], creator);
    }

    #[test]
    fn a_process_holds_the_newest_session_it_created() {
        // `/new` inside one TUI leaves the earlier session behind.
        let work = PathBuf::from("/nonexistent-agentview/work");
        let candidates = [
            candidate("ses_first", &work, 2_000, 3_000),
            candidate("ses_second", &work, 4_000, 5_000),
        ];
        let tui = holder(1, 1_000, Target::Directory(Some(work.clone())));
        let assigned = assign(std::slice::from_ref(&tui), &candidates);
        assert_eq!(assigned.len(), 1);
        assert_eq!(assigned["ses_second"], tui);
    }

    #[test]
    fn a_process_that_resumed_a_session_holds_the_newest_one_it_touched() {
        let work = PathBuf::from("/nonexistent-agentview/work");
        let other = PathBuf::from("/nonexistent-agentview/other");
        let candidates = [
            candidate("ses_resumed", &work, 100, 8_000),
            candidate("ses_untouched", &work, 200, 1_000),
            candidate("ses_elsewhere", &other, 300, 9_500),
        ];
        let tui = holder(1, 5_000, Target::Directory(Some(work.clone())));
        let assigned = assign(std::slice::from_ref(&tui), &candidates);
        assert_eq!(assigned.len(), 1);
        assert_eq!(assigned["ses_resumed"], tui);
    }

    #[test]
    fn a_running_background_subagent_keeps_the_parent_working() {
        let done = LastMessage {
            role: Some("assistant".into()),
            created: 10_000,
            completed: Some(11_000),
            question: 0,
        };
        assert_eq!(
            held_state(Some(&done), Some(12_000), 5_000),
            (SessionState::Working, "background task")
        );
        // A subagent turn cut off before the holder started is not work.
        assert_eq!(
            held_state(Some(&done), Some(1_000), 5_000),
            (SessionState::NeedsInput, "waiting at prompt")
        );
    }

    #[test]
    fn an_idle_screen_yields_to_work_the_database_shows() {
        let idle = Some((SessionState::NeedsInput, "waiting at prompt"));
        let working = (SessionState::Working, "running turn");
        let waiting = (SessionState::NeedsInput, "waiting at prompt");
        assert_eq!(combine(idle, working), working);
        assert_eq!(combine(idle, waiting), waiting);
        let permission = (SessionState::NeedsInput, "permission requested");
        assert_eq!(combine(Some(permission), working), permission);
        assert_eq!(combine(None, working), working);
    }

    #[test]
    fn a_held_screen_starts_work_at_once_but_an_idle_one_waits_for_discovery() {
        let working = (SessionState::Working, "running turn");
        let permission = (SessionState::NeedsInput, "permission requested");
        assert_eq!(settle_from_screen(working), Some(working));
        assert_eq!(settle_from_screen(permission), Some(permission));
        assert_eq!(
            settle_from_screen((SessionState::NeedsInput, "waiting at prompt")),
            None
        );
    }

    #[test]
    fn the_earliest_possible_runner_bounds_what_counts_as_current() {
        let work = PathBuf::from("/nonexistent-agentview/work");
        let holders = [
            holder(1, 9_000, Target::Session("ses_a".into())),
            holder(2, 3_000, Target::Directory(Some(work.clone()))),
            holder(3, 1_000, Target::Session("ses_b".into())),
        ];
        assert_eq!(earliest_start(&holders, "ses_a", &work), Some(3_000));
        assert_eq!(
            earliest_start(&holders, "ses_c", Path::new("/nonexistent-agentview/else")),
            None
        );
    }

    #[test]
    fn database_state_ignores_turns_older_than_the_holder() {
        let turn = |role: &str, created, completed, question| LastMessage {
            role: Some(role.into()),
            created,
            completed,
            question,
        };
        assert_eq!(
            held_state(Some(&turn("assistant", 10_000, None, 0)), None, 5_000).0,
            SessionState::Working
        );
        assert_eq!(
            held_state(Some(&turn("user", 10_000, None, 0)), None, 5_000).0,
            SessionState::Working
        );
        assert_eq!(
            held_state(Some(&turn("assistant", 10_000, None, 1)), None, 5_000),
            (SessionState::NeedsInput, "question asked")
        );
        assert_eq!(
            held_state(
                Some(&turn("assistant", 10_000, Some(11_000), 0)),
                None,
                5_000
            ),
            (SessionState::NeedsInput, "waiting at prompt")
        );
        // Left unfinished by a process that was killed before this one started.
        assert_eq!(
            held_state(Some(&turn("assistant", 1_000, None, 0)), None, 60_000),
            (SessionState::NeedsInput, "waiting at prompt")
        );
        assert_eq!(
            held_state(None, None, 5_000),
            (SessionState::NeedsInput, "waiting at prompt")
        );
    }

    // Bottom rows captured from OpenCode 1.18.33 in a 150x45 terminal.
    const WORKING: &str = "\
  ┃  Build · Claude Opus 5.5 1M Cursor · Claude Opus 5.5 1M High Fast                                         /private/tmp/oc-probe:main
  ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀
   ■■■■⬝⬝⬝⬝  esc interrupt                                                     tab agents  ctrl+p commands    • OpenCode 1.18.33
";
    const PERMISSION: &str = "\
     ⠋ Read /etc/hosts [limit=1]
     ▣  Build · Claude Opus 5.5 1M
  ┃
  ┃  △ Permission required
  ┃    ← Access external directory /etc
  ┃
  ┃  Patterns
  ┃
  ┃  - /etc/*
  ┃
  ┃                                                                                                           /private/tmp/oc-probe:main
  ┃   Allow once   Allow always   Reject                     ctrl+f fullscreen  ⇆ select  enter confirm
  ┃                                                                                                           • OpenCode 1.18.33
";
    const IDLE: &str = "\
     ⠴ Read /etc/hosts [limit=1]
  ┃  Build · Claude Opus 5.5 1M Cursor · Claude Opus 5.5 1M High Fast                                         /private/tmp/oc-probe:main
  ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀
   /private/tmp/oc-probe                                                       14.8K (1%)  ctrl+p commands    • OpenCode 1.18.33
";

    #[test]
    fn background_screen_distinguishes_working_permission_question_and_idle() {
        assert_eq!(
            screen_state(WORKING),
            Some((SessionState::Working, "running turn"))
        );
        assert_eq!(
            screen_state(PERMISSION),
            Some((SessionState::NeedsInput, "permission requested"))
        );
        assert_eq!(
            screen_state("  ┃  Which one?\n  ┃  ⇆ tab  ↑↓ select  enter submit  esc dismiss\n"),
            Some((SessionState::NeedsInput, "question asked"))
        );
        assert_eq!(
            screen_state(IDLE),
            Some((SessionState::NeedsInput, "waiting at prompt"))
        );
        assert_eq!(screen_state(""), None);
        assert_eq!(screen_state("Loading...\n"), None);
    }

    #[test]
    fn transcript_text_above_the_prompt_is_not_a_marker() {
        let mut screen = String::from("     the footer says esc interrupt while busy\n");
        for _ in 0..PROMPT_ROWS {
            screen.push_str("  ┃  some transcript line\n");
        }
        screen.push_str(IDLE);
        assert_eq!(
            screen_state(&screen),
            Some((SessionState::NeedsInput, "waiting at prompt"))
        );
    }
}
