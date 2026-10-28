//! A read-only view of an `agent-bouncer` queue.
//!
//! agent-bouncer is a separate tool that lets only as many heavy jobs (builds,
//! test suites, evals) run at once as the machine can take. Agentview does not
//! manage it. It asks `agent-bouncer status --json` every couple of seconds and
//! shows what comes back, so a session that is stuck behind a full queue says so.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const BINARY: &str = "agent-bouncer";

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Queue {
    #[serde(default)]
    pub slots_total: usize,
    #[serde(default)]
    pub slots_in_use: usize,
    pub avail_gb: Option<f64>,
    pub swap_gb: Option<f64>,
    #[serde(default)]
    pub running: Vec<RunningJob>,
    #[serde(default)]
    pub waiting: Vec<WaitingJob>,
    /// Guess at how long until the queue is empty, from how long the same jobs took before.
    pub est_free_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct RunningJob {
    pub label: Option<String>,
    #[serde(default)]
    pub cmd: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub started_ms: u64,
    #[serde(default)]
    pub weight: usize,
    #[serde(default)]
    pub rss_mb: u64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct WaitingJob {
    pub label: Option<String>,
    #[serde(default)]
    pub cmd: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub since_ms: u64,
    #[serde(default)]
    pub reason: String,
}

impl RunningJob {
    pub fn name(&self) -> String {
        friendly_name(self.label.as_deref(), &self.cmd)
    }
}

impl WaitingJob {
    pub fn name(&self) -> String {
        friendly_name(self.label.as_deref(), &self.cmd)
    }

    /// What the job's status column says: Waiting, unless the machine is the holdup.
    pub fn status_text(&self) -> &'static str {
        match self.reason.as_str() {
            "ram" | "swap" => "Low RAM",
            "cpu" | "cores" | "load" => "CPU busy",
            _ => "Waiting",
        }
    }

    /// Why the job is still in line, in words.
    pub fn reason_text(&self) -> &'static str {
        match self.reason.as_str() {
            "queue" => "others are ahead",
            "slots" => "all slots in use",
            "ram" => "low free RAM",
            "cpu" => "CPU saturated",
            "cores" => "all CPU cores spoken for",
            "load" => "machine overloaded",
            "swap" => "machine is swapping to disk",
            "settle" => "another job is starting up",
            _ => "busy",
        }
    }
}

impl Queue {
    pub fn parse(json: &[u8]) -> Option<Self> {
        serde_json::from_slice(json).ok()
    }

    pub fn is_idle(&self) -> bool {
        self.running.is_empty() && self.waiting.is_empty()
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// `agent-bouncer` on PATH, or in `~/.local/bin` where it installs itself.
pub fn find_binary() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut directories: Vec<PathBuf> = std::env::split_paths(&path).collect();
    if let Some(home) = std::env::var_os("HOME") {
        directories.push(PathBuf::from(home).join(".local/bin"));
    }
    directories
        .into_iter()
        .map(|directory| directory.join(BINARY))
        .find(|candidate| candidate.is_file())
}

fn fetch(binary: &PathBuf) -> Option<Queue> {
    let output = Command::new(binary)
        .args(["status", "--json"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| Queue::parse(&output.stdout))
        .flatten()
}

/// Polls the queue in the background; `take_latest` hands the newest reading
/// to the event loop.
pub struct QueueWatcher {
    updates: mpsc::Receiver<Queue>,
    stop: Arc<AtomicBool>,
}

impl QueueWatcher {
    /// None when agent-bouncer is not installed: the dashboard then shows nothing.
    pub fn spawn() -> Option<Self> {
        let binary = find_binary()?;
        let (tx, updates) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        thread::Builder::new()
            .name("agent-bouncer-queue".into())
            .spawn(move || {
                while !stop_flag.load(Ordering::Relaxed) {
                    if let Some(queue) = fetch(&binary) {
                        if tx.send(queue).is_err() {
                            return;
                        }
                    }
                    thread::sleep(POLL_INTERVAL);
                }
            })
            .ok()?;
        Some(Self { updates, stop })
    }

    pub fn take_latest(&self) -> Option<Queue> {
        self.updates.try_iter().last()
    }
}

impl Drop for QueueWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A job's label, or else the command with the wrapping noise taken off: `env`
/// settings, `fnm exec --`, `bash -c` and setup lines like `cd` and `set -e`,
/// leaving what the job actually runs, such as `bun run test:web:backend`.
pub fn friendly_name(label: Option<&str>, cmd: &str) -> String {
    if let Some(label) = label.filter(|label| !label.trim().is_empty()) {
        return label.to_string();
    }
    let cleaned: String = cmd
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let name = core_command(cmd);
    if name.is_empty() {
        cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        name
    }
}

const SETUP_COMMANDS: [&str; 6] = ["set", "cd", "export", "echo", "git", "true"];

fn core_command(cmd: &str) -> String {
    let normalized = cmd.replace("&&", "\n").replace(';', "\n");
    let lines: Vec<&str> = normalized.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        let words: Vec<&str> = line.split_whitespace().collect();
        let (words, script) = unwrap_command(&words);
        if let Some(script) = script {
            let mut rest = vec![script.as_str()];
            rest.extend(&lines[index + 1..]);
            return core_command(&rest.join("\n"));
        }
        let Some(program) = words.first() else {
            continue;
        };
        if !SETUP_COMMANDS.contains(program) {
            return render_words(&words);
        }
    }
    String::new()
}

/// Peels `env`, `fnm exec --`, `timeout` and leading `VAR=value` off a command.
/// A `bash -c` returns its script instead, to be read the same way.
fn unwrap_command<'a>(words: &[&'a str]) -> (Vec<&'a str>, Option<String>) {
    let mut words = words.to_vec();
    loop {
        let Some(&program) = words.first() else {
            return (words, None);
        };
        let base = program.rsplit('/').next().unwrap_or(program);
        let skip = match base {
            "env" => {
                let mut index = 1;
                while let Some(word) = words.get(index) {
                    if *word == "-u" || *word == "--unset" {
                        index += 2;
                    } else if word.starts_with('-') || is_assignment(word) {
                        index += 1;
                    } else {
                        break;
                    }
                }
                index
            }
            "fnm" if words.get(1) == Some(&"exec") => words
                .iter()
                .position(|word| *word == "--")
                .map_or(words.len(), |at| at + 1),
            "timeout" => {
                if words.get(1).is_some_and(|word| word.starts_with('-')) {
                    3
                } else {
                    2
                }
            }
            "bash" | "sh" | "zsh" if words.get(1) == Some(&"-c") => {
                return (Vec::new(), Some(words[2..].join(" ")));
            }
            _ if is_assignment(program) => 1,
            _ => return (words, None),
        };
        words = words.get(skip..).unwrap_or_default().to_vec();
    }
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

fn render_words(words: &[&str]) -> String {
    let mut out = Vec::new();
    for (index, word) in words.iter().enumerate() {
        if word.starts_with('>') || word.starts_with("2>") || *word == "|" || *word == "&" {
            break;
        }
        if index == 0 {
            out.push(word.rsplit('/').next().unwrap_or(word));
        } else {
            out.push(word);
        }
    }
    out.join(" ")
}

/// `4m12s`, `48s`, `1h05m`.
pub fn duration_text(milliseconds: u64) -> String {
    let seconds = milliseconds / 1000;
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m{:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h{:02}m", seconds / 3600, seconds % 3600 / 60),
    }
}

/// `~30s`, `~3m`, `~1h05m`: how long until the queue is expected to be empty.
pub fn clear_in_text(milliseconds: u64) -> String {
    let seconds = milliseconds / 1000;
    if seconds < 90 {
        return format!("~{}s", (seconds + 2) / 5 * 5).replace("~0s", "~5s");
    }
    let minutes = (seconds + 30) / 60;
    match minutes {
        0..=59 => format!("~{minutes}m"),
        _ => format!("~{}h{:02}m", minutes / 60, minutes % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = r#"{
        "slots_total": 2, "slots_in_use": 2, "avail_gb": 9.4, "swap_gb": 3.7, "cpu_busy": null,
        "running": [
            {"job": "a", "pid": 1, "label": null, "cmd": "cargo test", "cwd": "/work/api",
             "started_ms": 1000, "weight": 1, "rss_mb": 2100}
        ],
        "waiting": [
            {"job": "b", "pid": 2, "label": "e2e", "cmd": "pnpm playwright test", "cwd": "/work/web/apps/site",
             "since_ms": 5000, "weight": 1, "reason": "slots"},
            {"job": "c", "pid": 3, "label": null, "cmd": "cargo clippy", "cwd": "/work/api",
             "since_ms": 6000, "weight": 1, "reason": "queue"}
        ]
    }"#;

    #[test]
    fn core_and_load_waits_read_as_cpu_busy() {
        let json = r#"{"slots_total": 32, "slots_in_use": 1, "running": [],
            "waiting": [{"cmd": "cargo build", "cwd": "/w", "since_ms": 1, "weight": 1, "reason": "cores"}]}"#;
        let queue = Queue::parse(json.as_bytes()).unwrap();
        assert_eq!(queue.waiting[0].status_text(), "CPU busy");
        assert_eq!(queue.waiting[0].reason_text(), "all CPU cores spoken for");
    }

    #[test]
    fn parses_the_status_json_and_ignores_unknown_fields() {
        let queue = Queue::parse(STATUS.as_bytes()).unwrap();
        assert_eq!((queue.slots_in_use, queue.slots_total), (2, 2));
        assert_eq!(queue.running[0].name(), "cargo test");
        assert_eq!(friendly_name(None, "bash -c cd apps/web && node scripts/test.mjs a.tsx > /tmp/x.log 2>&1; echo EXIT"), "node scripts/test.mjs a.tsx");
        assert_eq!(queue.waiting[0].name(), "e2e");
        assert_eq!(queue.waiting[1].reason_text(), "others are ahead");
        assert!(!queue.is_idle());
    }

    #[test]
    fn commands_lose_their_wrapping_noise() {
        let name = |cmd: &str| friendly_name(None, cmd);
        assert_eq!(name("env -u FORCE_COLOR NO_COLOR=1 PATH=/a:/b bash -c git commit --no-edit && bun run test:web && git push"), "bun run test:web");
        assert_eq!(name("bash -c set -e\ngo test -count=1 ./...\nbun run ship"), "go test -count=1 ./...");
        assert_eq!(name("fnm exec --using 24.22.1 -- bun run test:web:backend -- tests/a.ts"), "bun run test:web:backend -- tests/a.ts");
        assert_eq!(name("env POSTGRES_URL=postgresql://x NODE_ENV=dev /usr/local/bin/bun run test"), "bun run test");
        assert_eq!(name("cargo test --lib"), "cargo test --lib");
        assert_eq!(name("set -e"), "set -e");
        assert_eq!(friendly_name(Some("evals"), "x"), "evals");
    }

    #[test]
    fn formats_durations_and_memory() {
        assert_eq!(duration_text(48_000), "48s");
        assert_eq!(duration_text(252_000), "4m12s");
        assert_eq!(duration_text(3_900_000), "1h05m");
        assert_eq!(clear_in_text(500), "~5s");
        assert_eq!(clear_in_text(28_000), "~30s");
        assert_eq!(clear_in_text(95_000), "~2m");
        assert_eq!(clear_in_text(3_900_000), "~1h05m");
    }

    #[test]
    fn garbage_is_not_a_queue() {
        assert!(Queue::parse(b"not json").is_none());
    }
}
