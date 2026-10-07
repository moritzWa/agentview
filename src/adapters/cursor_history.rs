//! Cursor's CLI keeps every chat on disk. Each one lives under
//! `~/.cursor/chats/<md5 of the working directory>/<chat id>/` with a small
//! `meta.json` (title, working directory, created/updated timestamps), a
//! `prompt_history.json` (the user's prompts, newest first), and a SQLite
//! `store.db` holding the conversation blobs.
//!
//! This source lists those chats on every platform. It only reads the two
//! small JSON files; the conversation store is never opened. A running
//! `cursor-agent` process keeps its chat's `store.db` open, which is how a
//! chat is recognised as live without scraping the CLI's TTY picker.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::native_owned::{sanitize, NativeOwnership};
use super::{DiscoveryRequest, SessionSource};
use crate::domain::{AgentSession, Capability, Provider, Runtime, SessionKind, SessionState};
#[cfg(all(unix, not(target_os = "linux")))]
use crate::process::CommandRequest;
use crate::process::{CommandRunner, ProcessRunner};

const MAX_META_BYTES: u64 = 64 * 1024;
const MAX_PROMPT_HISTORY_BYTES: u64 = 4 * 1024 * 1024;
const MAX_HASH_DIRS: usize = 4_096;
const MAX_CHATS_PER_DIR: usize = 4_096;
/// A chat whose store changed this recently while its process is alive is
/// treated as mid-turn rather than idle at the prompt.
const ACTIVE_WINDOW: Duration = Duration::from_secs(20);
#[cfg(all(unix, not(target_os = "linux")))]
const PROCESS_PROBE_TIMEOUT: Duration = Duration::from_secs(4);

/// Chat IDs another source already reports, such as the managed registry on
/// Linux, read once per discovery so one chat does not appear twice. An error
/// skips this tick's listing rather than risk a duplicate row.
pub type OwnedChatIds = Arc<dyn Fn() -> Result<BTreeSet<String>> + Send + Sync>;

/// Chats this dashboard created itself where no managed supervisor records
/// them (every platform but Linux). The history source lists these even
/// without `--include-external`, like other native-only harnesses' own rows.
pub struct CursorOwnership {
    inner: NativeOwnership,
}

impl CursorOwnership {
    pub fn load_default() -> Result<Arc<Self>> {
        Self::load(default_cursor_ownership_path()?)
    }

    pub fn load(path: PathBuf) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            inner: NativeOwnership::load(path, "Cursor")?,
        }))
    }

    pub fn record(&self, chat_id: &str, cwd: &Path, prompt: &str) -> Result<()> {
        self.inner.record(chat_id, cwd, prompt, None, "Cursor")
    }

    pub fn chat_ids(&self) -> BTreeSet<String> {
        self.inner
            .records()
            .into_iter()
            .map(|record| record.session_id)
            .collect()
    }
}

pub fn default_cursor_ownership_path() -> Result<PathBuf> {
    if let Some(state_home) = crate::fs_util::xdg_home("XDG_STATE_HOME") {
        return Ok(PathBuf::from(state_home).join("agentview/cursor-owned.json"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/state/agentview/cursor-owned.json"))
}

pub struct CursorHistorySource {
    /// `None` resolves the default store at discovery time, so a missing home
    /// directory fails this source alone instead of startup.
    chats_root: Option<PathBuf>,
    versions_root: Option<PathBuf>,
    runner: Arc<dyn CommandRunner>,
    skip: Option<OwnedChatIds>,
    owned: Option<Arc<CursorOwnership>>,
}

impl CursorHistorySource {
    pub fn host(chats_root: PathBuf, versions_root: Option<PathBuf>) -> Self {
        Self {
            chats_root: Some(chats_root),
            versions_root,
            runner: Arc::new(ProcessRunner),
            skip: None,
            owned: None,
        }
    }

    /// The CLI's default store under the user's home directory.
    pub fn host_default() -> Self {
        Self {
            chats_root: None,
            versions_root: default_cursor_versions_dir().ok(),
            runner: Arc::new(ProcessRunner),
            skip: None,
            owned: None,
        }
    }

    /// Leave out the chat IDs `skip` returns.
    pub fn skipping(mut self, skip: OwnedChatIds) -> Self {
        self.skip = Some(skip);
        self
    }

    /// Also list the chats `owned` records when external chats are not
    /// requested.
    pub fn owned(mut self, owned: Arc<CursorOwnership>) -> Self {
        self.owned = Some(owned);
        self
    }

    #[cfg(test)]
    fn with_runner(mut self, runner: Arc<dyn CommandRunner>) -> Self {
        self.runner = runner;
        self
    }
}

impl SessionSource for CursorHistorySource {
    fn label(&self) -> &str {
        "Cursor (host)"
    }

    fn discover(&self, request: &DiscoveryRequest) -> Result<Vec<AgentSession>> {
        // Every chat in this store is a foreground TUI session, so external
        // ones need both flags. Chats this dashboard created are its own.
        let external = request.include_external && request.include_interactive;
        let owned = match (&self.owned, external) {
            (Some(owned), false) => Some(owned.chat_ids()),
            (None, false) => return Ok(Vec::new()),
            (_, true) => None,
        };
        if owned.as_ref().is_some_and(BTreeSet::is_empty) {
            return Ok(Vec::new());
        }
        let chats_root = match &self.chats_root {
            Some(chats_root) => chats_root.clone(),
            None => default_cursor_chats_dir()?,
        };
        let mut chats = read_cursor_chats(&chats_root)?;
        if let Some(owned) = &owned {
            chats.retain(|chat| owned.contains(&chat.id));
        }
        if let Some(skip) = &self.skip {
            let owned = skip().context("failed to read the managed Cursor registry")?;
            chats.retain(|chat| !owned.contains(&chat.id));
        }
        if chats.is_empty() {
            return Ok(Vec::new());
        }
        let live = live_chats(
            self.versions_root.as_deref(),
            &chats_root,
            self.runner.as_ref(),
        );
        let now = SystemTime::now();
        chats.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
        let mut sessions = Vec::new();
        let mut completed = 0usize;
        for chat in chats {
            let pid = live.get(&chat.dir).copied();
            let background = pid.and_then(|_| {
                crate::native_session::background_screen_contents(&format!(
                    "cursor:host:{}",
                    chat.id
                ))
            });
            let (state, raw_state) =
                background_state(pid, background).unwrap_or_else(|| chat_state(&chat, pid, now));
            if state == SessionState::Completed {
                if !request.include_completed || completed >= request.history_limit.max(1) {
                    continue;
                }
                completed += 1;
            }
            let summary = latest_prompt(&chat.dir)
                .map(|prompt| sanitize(&prompt, 180, &chat.title))
                .unwrap_or_else(|| format!("no prompts recorded · {}", chat.cwd.display()));
            sessions.push(AgentSession {
                id: format!("cursor:host:{}", chat.id),
                provider_session_id: chat.id.clone(),
                provider: Provider::Cursor,
                runtime: Runtime::Host,
                kind: SessionKind::Interactive,
                name: chat.title.clone(),
                cwd: chat.cwd.clone(),
                state,
                summary,
                raw_state: Some(raw_state.into()),
                pid,
                started_at: chat.created_at,
                updated_at: chat.updated_at,
                pull_requests: None,
                capabilities: BTreeSet::from([Capability::Inspect]),
            });
        }
        Ok(sessions)
    }
}

/// A bounded transcript of the user's own prompts, newest last. Cursor keeps
/// the assistant side in a SQLite blob store that this dashboard does not
/// parse, so the prompts are what an operator can review.
pub fn inspect_cursor_history(chats_root: &Path, session: &AgentSession) -> Result<String> {
    let chat = find_chat(chats_root, &session.provider_session_id)?
        .with_context(|| format!("Cursor chat {} was not found on disk", session.name))?;
    let prompts = read_prompt_history(&chat.dir)?;
    // The peek panel shows the tail, so the newest prompt goes last.
    let mut lines = vec![
        format!("{} · {}", chat.title, chat.cwd.display()),
        "Open the row to resume the chat in Cursor.".into(),
        String::new(),
    ];
    if prompts.is_empty() {
        lines.push("No prompts recorded for this chat yet.".into());
    } else {
        lines.push(format!("Last {} prompt(s):", prompts.len().min(20)));
        for prompt in prompts.iter().take(20).rev() {
            lines.push(format!("❯ {}", sanitize(prompt, 400, "(empty prompt)")));
        }
    }
    Ok(lines.join("\n"))
}

pub fn default_cursor_chats_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join(".cursor").join("chats"))
}

/// Where the CLI installer keeps versioned builds. Each build records live
/// process IDs under `.running/`, which bounds the process probe.
pub fn default_cursor_versions_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join(".local/share/cursor-agent/versions"))
}

/// `HOME`, or `USERPROFILE` where `HOME` is normally unset (Windows).
fn home_dir() -> Result<PathBuf> {
    home_from(|name| std::env::var_os(name))
}

fn home_from(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Result<PathBuf> {
    ["HOME", "USERPROFILE"]
        .into_iter()
        .filter_map(var)
        .find(|value| !value.is_empty())
        .map(PathBuf::from)
        .context("neither HOME nor USERPROFILE is set")
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CursorChat {
    id: String,
    dir: PathBuf,
    title: String,
    cwd: PathBuf,
    created_at: Option<SystemTime>,
    updated_at: Option<SystemTime>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CursorChatMeta {
    #[serde(default)]
    created_at_ms: Option<u64>,
    #[serde(default)]
    updated_at_ms: Option<u64>,
    #[serde(default)]
    has_conversation: Option<bool>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    cwd: Option<PathBuf>,
}

fn read_cursor_chats(chats_root: &Path) -> Result<Vec<CursorChat>> {
    let root_metadata = match fs::symlink_metadata(chats_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("failed to read the Cursor chats directory"),
    };
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        bail!("Cursor chats root must be a real directory");
    }
    let mut chats = Vec::new();
    for hash_dir in real_subdirectories(chats_root)?
        .into_iter()
        .take(MAX_HASH_DIRS)
    {
        // A workspace directory created, removed, or unreadable mid-scan must
        // not hide every other chat.
        let Ok(chat_dirs) = real_subdirectories(&hash_dir) else {
            continue;
        };
        for chat_dir in chat_dirs.into_iter().take(MAX_CHATS_PER_DIR) {
            let Some(id) = chat_dir.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !is_chat_id(id) {
                continue;
            }
            match read_chat(&chat_dir, id) {
                Ok(Some(chat)) => chats.push(chat),
                // One damaged chat must not hide the rest of the history.
                Ok(None) | Err(_) => continue,
            }
        }
    }
    Ok(chats)
}

fn real_subdirectories(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        paths.push(entry.path());
    }
    paths.sort();
    Ok(paths)
}

/// One chat by ID. Chats live under `<hash of cwd>/<id>`, so probing each
/// workspace directory for that ID reads a single `meta.json` instead of
/// every chat in the store.
fn find_chat(chats_root: &Path, id: &str) -> Result<Option<CursorChat>> {
    if !is_chat_id(id) {
        return Ok(None);
    }
    for hash_dir in real_subdirectories(chats_root)?
        .into_iter()
        .take(MAX_HASH_DIRS)
    {
        let chat_dir = hash_dir.join(id);
        match fs::symlink_metadata(&chat_dir) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            _ => continue,
        }
        if let Ok(Some(chat)) = read_chat(&chat_dir, id) {
            return Ok(Some(chat));
        }
    }
    Ok(None)
}

fn read_chat(chat_dir: &Path, id: &str) -> Result<Option<CursorChat>> {
    let meta: CursorChatMeta = read_bounded_json(&chat_dir.join("meta.json"), MAX_META_BYTES)?;
    if meta.has_conversation == Some(false) {
        return Ok(None);
    }
    let Some(cwd) = meta.cwd.filter(|path| path.is_absolute()) else {
        return Ok(None);
    };
    let title = meta
        .title
        .map(|title| sanitize(&title, 120, "Cursor chat"))
        .unwrap_or_else(|| "Cursor chat".into());
    Ok(Some(CursorChat {
        id: id.to_owned(),
        dir: chat_dir.to_owned(),
        title,
        cwd,
        created_at: meta.created_at_ms.map(millis),
        updated_at: meta.updated_at_ms.map(millis),
    }))
}

fn read_bounded_json<T: for<'de> Deserialize<'de>>(path: &Path, limit: u64) -> Result<T> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{} must be a real file", path.display());
    }
    if metadata.len() > limit {
        bail!("{} exceeded the {limit}-byte safety limit", path.display());
    }
    let file = File::open(path)?;
    serde_json::from_reader(BufReader::new(file))
        .with_context(|| format!("invalid JSON in {}", path.display()))
}

fn read_prompt_history(chat_dir: &Path) -> Result<Vec<String>> {
    let path = chat_dir.join("prompt_history.json");
    match fs::symlink_metadata(&path) {
        Ok(_) => read_bounded_json::<Vec<String>>(&path, MAX_PROMPT_HISTORY_BYTES),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn latest_prompt(chat_dir: &Path) -> Option<String> {
    read_prompt_history(chat_dir)
        .ok()?
        .into_iter()
        .find(|prompt| !prompt.trim().is_empty())
}

/// Cursor's composer shows this right-hand hint while a turn is being
/// processed (`isProcessing` in the CLI) and the composer input is empty,
/// including long thinking and tool calls that write nothing to the chat
/// store. Typing a queued follow-up mid-turn hides it.
const PROCESSING_HINT: &str = "ctrl+c to stop";
/// Placeholders of the empty composer (agent, plan, and shell modes). They
/// show while idle and while processing alike, so they only prove that the
/// composer is on screen and empty.
const COMPOSER_PLACEHOLDERS: &[&str] = &[
    "Add a follow-up",
    "Plan, search, build anything",
    "Run a command",
];
/// The composer and its status lines sit in the last few non-empty rows,
/// below the transcript.
const COMPOSER_ROWS: usize = 6;

/// State from the screen this dashboard holds in the background for a chat,
/// given the pid holding the chat's store. Only our own live child's screen
/// speaks for the chat: one resumed elsewhere after ours exited is held by
/// another pid.
fn background_state(
    holder: Option<u32>,
    background: Option<(u32, String)>,
) -> Option<(SessionState, &'static str)> {
    let (child, screen) = background?;
    if holder != Some(child) {
        return None;
    }
    screen_state(&screen)
}

/// The state the terminal this dashboard holds for a discovered chat shows
/// now, for the dashboard to apply between discoveries.
pub(super) fn background_screen_state(
    session: &AgentSession,
) -> Option<(SessionState, &'static str)> {
    background_state(
        session.pid,
        crate::native_session::background_screen_contents(&session.id),
    )
}

/// State of a chat whose terminal this dashboard holds in the background, or
/// `None` when the screen does not settle it. The hint's absence alone is not
/// evidence of waiting: typed input, a startup or login screen, or a renamed
/// hint all hide it, so waiting needs the empty idle composer itself.
fn screen_state(screen: &str) -> Option<(SessionState, &'static str)> {
    let composer = screen
        .lines()
        .rev()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .take(COMPOSER_ROWS)
        .collect::<Vec<_>>();
    if composer.iter().any(|line| line.ends_with(PROCESSING_HINT)) {
        return Some((SessionState::Working, "running turn"));
    }
    let idle = composer.iter().any(|line| {
        COMPOSER_PLACEHOLDERS
            .iter()
            .any(|placeholder| line.contains(placeholder))
    });
    idle.then_some((SessionState::NeedsInput, "waiting at prompt"))
}

/// Fallback for chats running in some other terminal: the store's write time.
fn chat_state(
    chat: &CursorChat,
    pid: Option<u32>,
    now: SystemTime,
) -> (SessionState, &'static str) {
    if pid.is_none() {
        return (SessionState::Completed, "closed");
    }
    let last_write = store_mtime(&chat.dir).max(chat.updated_at);
    let active = last_write
        .and_then(|written| now.duration_since(written).ok())
        .is_some_and(|age| age <= ACTIVE_WINDOW);
    if active {
        (SessionState::Working, "running turn")
    } else {
        (SessionState::NeedsInput, "waiting at prompt")
    }
}

/// The newest write to the conversation store. Cursor appends to the SQLite
/// WAL while a turn streams, so this moves during work and rests otherwise.
fn store_mtime(chat_dir: &Path) -> Option<SystemTime> {
    ["store.db-wal", "store.db", "meta.json"]
        .iter()
        .filter_map(|name| fs::metadata(chat_dir.join(name)).ok()?.modified().ok())
        .max()
}

fn is_chat_id(value: &str) -> bool {
    value.len() == 36
        && value.chars().enumerate().all(|(index, character)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                character == '-'
            } else {
                character.is_ascii_hexdigit()
            }
        })
}

fn millis(value: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(value)
}

/// Chat directories held open by a live `cursor-agent`, keyed by directory.
fn live_chats(
    versions_root: Option<&Path>,
    chats_root: &Path,
    runner: &dyn CommandRunner,
) -> BTreeMap<PathBuf, u32> {
    let pids = candidate_pids(versions_root)
        .into_iter()
        .filter(|pid| process_alive(*pid))
        .collect::<Vec<_>>();
    if pids.is_empty() {
        return BTreeMap::new();
    }
    // The probe may report the resolved path (macOS `/private/tmp`), so match
    // against the root as configured and as canonicalised.
    let mut roots = vec![chats_root.to_owned()];
    if let Ok(canonical) = fs::canonicalize(chats_root) {
        if canonical != chats_root {
            roots.push(canonical);
        }
    }
    let mut live = BTreeMap::new();
    for (pid, path) in open_store_files(&pids, runner) {
        let Some(relative) = roots.iter().find_map(|root| path.strip_prefix(root).ok()) else {
            continue;
        };
        let parts = relative.components().collect::<Vec<_>>();
        // `<hash>/<chat id>/store.db`
        if parts.len() != 3 {
            continue;
        }
        let chat_dir = chats_root.join(parts[0]).join(parts[1]);
        live.entry(chat_dir).or_insert(pid);
    }
    live
}

/// The CLI records each live process under `<version>/.running/<pid>`. Stale
/// markers are common, so every candidate is verified before use.
fn candidate_pids(versions_root: Option<&Path>) -> BTreeSet<u32> {
    let mut pids = BTreeSet::new();
    let Some(versions_root) = versions_root else {
        return pids;
    };
    let Ok(versions) = fs::read_dir(versions_root) else {
        return pids;
    };
    for version in versions.flatten() {
        let Ok(markers) = fs::read_dir(version.path().join(".running")) else {
            continue;
        };
        for marker in markers.flatten() {
            if let Some(pid) = marker
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            {
                pids.insert(pid);
            }
        }
    }
    pids
}

fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        // Signal 0 checks existence only. EPERM still means the process exists.
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// `store.db` paths each candidate process holds open.
fn open_store_files(pids: &[u32], runner: &dyn CommandRunner) -> Vec<(u32, PathBuf)> {
    #[cfg(target_os = "linux")]
    {
        let _ = runner;
        let mut files = Vec::new();
        for pid in pids {
            let Ok(entries) = fs::read_dir(format!("/proc/{pid}/fd")) else {
                continue;
            };
            for entry in entries.flatten() {
                if let Ok(target) = fs::read_link(entry.path()) {
                    if target.file_name().and_then(|name| name.to_str()) == Some("store.db") {
                        files.push((*pid, target));
                    }
                }
            }
        }
        files
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let mut files = Vec::new();
        let mut unread = Vec::new();
        for pid in pids {
            match crate::proc_info::open_files(*pid) {
                Some(paths) => files.extend(
                    paths
                        .into_iter()
                        .filter(|path| {
                            path.file_name().and_then(|name| name.to_str()) == Some("store.db")
                        })
                        .map(|path| (*pid, path)),
                ),
                None => unread.push(*pid),
            }
        }
        if unread.is_empty() {
            return files;
        }
        let list = unread
            .iter()
            .map(|pid| pid.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let mut request = CommandRequest::new(
            "lsof",
            vec![
                "-n".into(),
                "-P".into(),
                "-F".into(),
                "pn".into(),
                "-p".into(),
                list,
            ],
        );
        request.timeout = PROCESS_PROBE_TIMEOUT;
        // lsof exits non-zero when any listed PID has gone away; the output it
        // did produce is still valid, so only an unreadable stream is fatal.
        if let Ok(output) = runner.run(&request) {
            files.extend(
                output
                    .stdout_text()
                    .map(parse_lsof_store_files)
                    .unwrap_or_default(),
            );
        }
        files
    }
    #[cfg(not(unix))]
    {
        let _ = (pids, runner);
        Vec::new()
    }
}

/// Parse `lsof -F pn` output: a `p<pid>` line starts each process, and every
/// `n<path>` line after it belongs to that process.
pub fn parse_lsof_store_files(output: &str) -> Vec<(u32, PathBuf)> {
    let mut files = Vec::new();
    let mut current = None;
    for line in output.lines() {
        if let Some(pid) = line.strip_prefix('p') {
            current = pid.trim().parse::<u32>().ok();
        } else if let (Some(pid), Some(path)) = (current, line.strip_prefix('n')) {
            let path = PathBuf::from(path);
            if path.file_name().and_then(|name| name.to_str()) == Some("store.db") {
                files.push((pid, path));
            }
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::process::{CommandOutput, CommandRequest};

    const CHAT_A: &str = "2a243dcb-b43b-47be-8dfc-73f656a3f5ea";
    const CHAT_B: &str = "499e4a32-b12d-4fc1-b0a0-01ee8c58601d";
    const CHAT_EMPTY: &str = "7ceca4ee-479c-4a75-83b0-0ae564583cdb";

    struct StaticRunner {
        stdout: String,
        requests: Mutex<Vec<CommandRequest>>,
    }

    impl CommandRunner for StaticRunner {
        fn run(&self, request: &CommandRequest) -> Result<CommandOutput> {
            self.requests.lock().unwrap().push(request.clone());
            Ok(CommandOutput {
                status: 0,
                stdout: self.stdout.clone().into_bytes(),
                stderr: Vec::new(),
            })
        }
    }

    /// A Unix-style fixture path made absolute on this platform, since chats
    /// with a relative cwd are skipped. Forward slashes need no JSON escaping.
    fn absolute(path: &str) -> String {
        if cfg!(windows) {
            format!("C:{path}")
        } else {
            path.to_owned()
        }
    }

    fn write_chat(root: &Path, hash: &str, id: &str, title: &str, cwd: &str, updated: u64) {
        let cwd = absolute(cwd);
        let dir = root.join(hash).join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("meta.json"),
            format!(
                r#"{{"schemaVersion":1,"createdAtMs":{},"hasConversation":true,"title":"{title}","updatedAtMs":{updated},"cwd":"{cwd}"}}"#,
                updated - 60_000
            ),
        )
        .unwrap();
        fs::write(
            dir.join("prompt_history.json"),
            r#"["  newest   prompt\nhere ", "older prompt"]"#,
        )
        .unwrap();
        fs::write(dir.join("store.db"), b"sqlite").unwrap();
    }

    fn request() -> DiscoveryRequest {
        DiscoveryRequest {
            include_completed: true,
            include_interactive: true,
            include_external: true,
            cwd: None,
            history_limit: 100,
            history_oldest_first: false,
        }
    }

    #[test]
    fn background_screen_decides_between_working_and_waiting() {
        let thinking = "  ⬡ Thinking  1234 tokens\n\n → Add a follow-up    ctrl+c to stop \n";
        assert_eq!(
            screen_state(thinking),
            Some((SessionState::Working, "running turn"))
        );
        let idle = "  Done.\n\n → Add a follow-up\n  Auto · 12% context\n";
        assert_eq!(
            screen_state(idle),
            Some((SessionState::NeedsInput, "waiting at prompt"))
        );
        let plan = " → Plan, search, build anything\n\n\n";
        assert_eq!(
            screen_state(plan),
            Some((SessionState::NeedsInput, "waiting at prompt"))
        );
    }

    #[test]
    fn background_screen_without_hint_or_idle_composer_is_not_evidence() {
        // A queued follow-up typed mid-turn hides both the hint and the
        // placeholder; the turn is still running.
        let typed = "  ⬡ Thinking  1234 tokens\n\n → also check the tests\n  Auto · 12% context\n";
        assert_eq!(screen_state(typed), None);
        // Blank startup, a login prompt, or a renamed hint are not waiting.
        assert_eq!(screen_state(""), None);
        assert_eq!(screen_state("\n\n   \n"), None);
        assert_eq!(screen_state("Press any key to sign in...\n"), None);
        assert_eq!(screen_state(" → Nachfrage    Strg+C zum Stoppen\n"), None);
    }

    #[test]
    fn only_our_own_live_child_screen_decides_the_state() {
        let working = || Some((42, " → Add a follow-up    ctrl+c to stop\n".to_owned()));
        assert_eq!(
            background_state(Some(42), working()),
            Some((SessionState::Working, "running turn"))
        );
        // Our child crashed and the chat was resumed by another process.
        assert_eq!(background_state(Some(7), working()), None);
        assert_eq!(background_state(None, working()), None);
        // An exited child reports no screen at all.
        assert_eq!(background_state(Some(42), None), None);
    }

    #[test]
    fn processing_hint_counts_only_at_the_end_of_a_composer_row() {
        // The hint quoted in the transcript, above the composer, is not ours.
        let quoted = format!(
            "  Cursor shows ctrl+c to stop while busy\n  press ctrl+c to stop\n{}\n → Add a follow-up\n  Auto\n",
            "  transcript\n".repeat(COMPOSER_ROWS)
        );
        assert_eq!(
            screen_state(&quoted),
            Some((SessionState::NeedsInput, "waiting at prompt"))
        );
        // Inside the composer rows but mid-line, it is not the hint either.
        let mid_line = " → why does ctrl+c to stop not work here\n  Auto\n";
        assert_eq!(screen_state(mid_line), None);
    }

    #[test]
    fn chat_ids_are_uuid_shaped() {
        assert!(is_chat_id(CHAT_A));
        assert!(!is_chat_id("2a243dcb"));
        assert!(!is_chat_id("../../../../etc/passwd/xxxxxxxxxxxxxxxxxx"));
        assert!(!is_chat_id("2a243dcbXb43bX47beX8dfcX73f656a3f5ea"));
    }

    #[test]
    fn lists_chats_from_meta_json_and_skips_unusable_entries() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write_chat(
            root,
            "8550e5f0",
            CHAT_A,
            "Agent Comparison",
            "/Users/m",
            2_000_000,
        );
        write_chat(
            root,
            "d3643a72",
            CHAT_B,
            "Agent View Setup",
            "/Users/m/Code",
            1_000_000,
        );
        // No conversation yet: an empty composer, not a session.
        let empty = root.join("f8dbe6c9").join(CHAT_EMPTY);
        fs::create_dir_all(&empty).unwrap();
        fs::write(
            empty.join("meta.json"),
            r#"{"schemaVersion":1,"createdAtMs":1,"hasConversation":false,"title":"New","updatedAtMs":1,"cwd":"/tmp"}"#,
        )
        .unwrap();
        // Not a chat id.
        fs::create_dir_all(root.join("8550e5f0").join("notes")).unwrap();
        // Damaged meta must not hide the rest.
        let broken = root
            .join("8550e5f0")
            .join("11111111-2222-4333-8444-555555555555");
        fs::create_dir_all(&broken).unwrap();
        fs::write(broken.join("meta.json"), "{ not json").unwrap();

        let source = CursorHistorySource::host(root.to_owned(), None);
        let sessions = source.discover(&request()).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, format!("cursor:host:{CHAT_A}"));
        assert_eq!(sessions[0].name, "Agent Comparison");
        assert_eq!(sessions[0].cwd, PathBuf::from(absolute("/Users/m")));
        assert_eq!(sessions[0].summary, "newest prompt here");
        assert_eq!(sessions[0].state, SessionState::Completed);
        assert_eq!(sessions[0].kind, SessionKind::Interactive);
        assert_eq!(sessions[0].raw_state.as_deref(), Some("closed"));
        assert_eq!(sessions[0].updated_at, Some(millis(2_000_000)));
        assert_eq!(sessions[0].started_at, Some(millis(1_940_000)));
        assert_eq!(sessions[1].provider_session_id, CHAT_B);
        assert!(sessions
            .iter()
            .all(|session| session.capabilities == BTreeSet::from([Capability::Inspect])));
    }

    #[test]
    fn foreground_chats_are_hidden_without_include_interactive() {
        let temp = tempfile::tempdir().unwrap();
        write_chat(temp.path(), "8550e5f0", CHAT_A, "A", "/Users/m", 2_000_000);
        let source = CursorHistorySource::host(temp.path().to_owned(), None);
        let mut request = request();
        request.include_interactive = false;
        assert!(source.discover(&request).unwrap().is_empty());
    }

    #[test]
    fn completed_history_honours_the_limit_and_completed_toggle() {
        let temp = tempfile::tempdir().unwrap();
        write_chat(temp.path(), "8550e5f0", CHAT_A, "A", "/Users/m", 2_000_000);
        write_chat(temp.path(), "8550e5f0", CHAT_B, "B", "/Users/m", 1_000_000);
        let source = CursorHistorySource::host(temp.path().to_owned(), None);
        let mut limited = request();
        limited.history_limit = 1;
        let sessions = source.discover(&limited).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].provider_session_id, CHAT_A);
        let mut active_only = request();
        active_only.include_completed = false;
        assert!(source.discover(&active_only).unwrap().is_empty());
    }

    #[test]
    fn skip_filter_removes_chats_another_source_owns() {
        let temp = tempfile::tempdir().unwrap();
        write_chat(temp.path(), "8550e5f0", CHAT_A, "A", "/Users/m", 2_000_000);
        write_chat(temp.path(), "8550e5f0", CHAT_B, "B", "/Users/m", 1_000_000);
        let source = CursorHistorySource::host(temp.path().to_owned(), None)
            .skipping(Arc::new(|| Ok(BTreeSet::from([CHAT_A.to_owned()]))));
        let sessions = source.discover(&request()).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].provider_session_id, CHAT_B);
    }

    #[test]
    fn owned_chats_are_listed_without_external_discovery() {
        let temp = tempfile::tempdir().unwrap();
        write_chat(temp.path(), "8550e5f0", CHAT_A, "A", "/Users/m", 2_000_000);
        write_chat(temp.path(), "8550e5f0", CHAT_B, "B", "/Users/m", 1_000_000);
        let state = tempfile::tempdir().unwrap();
        let registry = state.path().join("agentview/cursor-owned.json");
        let ownership = CursorOwnership::load(registry.clone()).unwrap();
        let source =
            CursorHistorySource::host(temp.path().to_owned(), None).owned(ownership.clone());
        // The default dashboard asks for neither external nor interactive
        // sessions.
        let mut own_only = request();
        own_only.include_external = false;
        own_only.include_interactive = false;
        assert!(source.discover(&own_only).unwrap().is_empty());

        ownership
            .record(CHAT_B, Path::new("/Users/m"), "fix the tests")
            .unwrap();
        let sessions = source.discover(&own_only).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].provider_session_id, CHAT_B);
        // Reloading the registry from disk keeps the chat listed.
        let reloaded = CursorHistorySource::host(temp.path().to_owned(), None)
            .owned(CursorOwnership::load(registry).unwrap());
        assert_eq!(reloaded.discover(&own_only).unwrap().len(), 1);
        // With external discovery on, every chat is listed once.
        assert_eq!(source.discover(&request()).unwrap().len(), 2);
    }

    #[test]
    fn an_unreadable_owner_registry_skips_the_listing_instead_of_duplicating() {
        let temp = tempfile::tempdir().unwrap();
        write_chat(temp.path(), "8550e5f0", CHAT_A, "A", "/Users/m", 2_000_000);
        let source = CursorHistorySource::host(temp.path().to_owned(), None)
            .skipping(Arc::new(|| bail!("registry is locked")));
        let error = source.discover(&request()).unwrap_err();
        assert!(format!("{error:#}").contains("registry is locked"));
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_workspace_directory_does_not_hide_other_chats() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        write_chat(temp.path(), "8550e5f0", CHAT_A, "A", "/Users/m", 2_000_000);
        write_chat(temp.path(), "d3643a72", CHAT_B, "B", "/Users/m", 1_000_000);
        let locked = temp.path().join("d3643a72");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let readable = fs::read_dir(&locked).is_ok();
        let source = CursorHistorySource::host(temp.path().to_owned(), None);
        let result = source.discover(&request());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let sessions = result.unwrap();
        if readable {
            // Running as root: permissions are not enforced, both are listed.
            assert_eq!(sessions.len(), 2);
        } else {
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].provider_session_id, CHAT_A);
        }
    }

    #[test]
    fn find_chat_probes_workspace_directories_for_the_id() {
        let temp = tempfile::tempdir().unwrap();
        write_chat(temp.path(), "8550e5f0", CHAT_A, "A", "/Users/m", 2_000_000);
        write_chat(temp.path(), "d3643a72", CHAT_B, "B", "/Users/m", 1_000_000);
        // A damaged chat elsewhere is never read on the way to the target.
        let broken = temp.path().join("0000aaaa").join(CHAT_EMPTY);
        fs::create_dir_all(&broken).unwrap();
        fs::write(broken.join("meta.json"), "{ not json").unwrap();
        let chat = find_chat(temp.path(), CHAT_B).unwrap().unwrap();
        assert_eq!(chat.dir, temp.path().join("d3643a72").join(CHAT_B));
        assert!(find_chat(temp.path(), CHAT_EMPTY).unwrap().is_none());
        assert!(find_chat(temp.path(), "../d3643a72").unwrap().is_none());
    }

    #[test]
    fn home_falls_back_to_userprofile() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| std::ffi::OsString::from(value))
            }
        };
        assert_eq!(
            home_from(env(&[("HOME", "/home/a"), ("USERPROFILE", "C:\\Users\\a")])).unwrap(),
            PathBuf::from("/home/a")
        );
        assert_eq!(
            home_from(env(&[("HOME", ""), ("USERPROFILE", "C:\\Users\\a")])).unwrap(),
            PathBuf::from("C:\\Users\\a")
        );
        assert!(home_from(env(&[])).is_err());
    }

    #[test]
    fn a_live_process_holding_the_store_marks_the_chat_active_or_idle() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("chats");
        let versions = temp.path().join("versions");
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        write_chat(&root, "8550e5f0", CHAT_A, "Active", "/Users/m", now_ms);
        write_chat(
            &root,
            "8550e5f0",
            CHAT_B,
            "Idle",
            "/Users/m",
            now_ms - 3_600_000,
        );
        // Make the idle chat's files old so it is not mistaken for a running turn.
        let old = filetime_old();
        for name in ["store.db", "meta.json", "prompt_history.json"] {
            let path = root.join("8550e5f0").join(CHAT_B).join(name);
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        let running = versions.join("2026.09.28-64d2043").join(".running");
        fs::create_dir_all(&running).unwrap();
        let me = std::process::id();
        fs::write(running.join(me.to_string()), b"").unwrap();
        fs::write(running.join("999999"), b"").unwrap();

        let store_a = root.join("8550e5f0").join(CHAT_A).join("store.db");
        let store_b = root.join("8550e5f0").join(CHAT_B).join("store.db");
        let runner = Arc::new(StaticRunner {
            stdout: format!(
                "p{me}\nfcwd\nn/Users/m\nn{}\nn{}\nn{}-wal\n",
                store_a.display(),
                store_b.display(),
                store_a.display()
            ),
            requests: Mutex::new(Vec::new()),
        });
        // macOS reads open files in-process, so this process really holds them.
        #[cfg(target_os = "macos")]
        let _held = [File::open(&store_a).unwrap(), File::open(&store_b).unwrap()];
        let source =
            CursorHistorySource::host(root.clone(), Some(versions)).with_runner(runner.clone());
        let sessions = source.discover(&request()).unwrap();
        let by_id = sessions
            .iter()
            .map(|session| (session.provider_session_id.as_str(), session))
            .collect::<BTreeMap<_, _>>();

        #[cfg(target_os = "macos")]
        {
            assert!(runner.requests.lock().unwrap().is_empty(), "no lsof");
            assert_eq!(by_id[CHAT_A].state, SessionState::Working);
            assert_eq!(by_id[CHAT_A].pid, Some(me));
            assert_eq!(by_id[CHAT_B].state, SessionState::NeedsInput);
        }
        #[cfg(all(unix, not(target_os = "linux"), not(target_os = "macos")))]
        {
            let requests = runner.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].program, "lsof");
            assert_eq!(
                requests[0].args[..2],
                ["-n", "-P"],
                "no DNS or port lookups"
            );
            assert_eq!(requests[0].args[5], me.to_string(), "dead PIDs are pruned");
            assert_eq!(by_id[CHAT_A].state, SessionState::Working);
            assert_eq!(by_id[CHAT_A].pid, Some(me));
            assert_eq!(by_id[CHAT_B].state, SessionState::NeedsInput);
            assert_eq!(
                by_id[CHAT_B].raw_state.as_deref(),
                Some("waiting at prompt")
            );
        }
        #[cfg(target_os = "linux")]
        {
            // /proc shows this test process does not hold the stores open.
            assert_eq!(by_id[CHAT_A].state, SessionState::Completed);
            assert_eq!(by_id[CHAT_B].state, SessionState::Completed);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn processes_libproc_cannot_read_fall_back_to_lsof() {
        let runner = StaticRunner {
            stdout: "p1\nn/Users/m/.cursor/chats/h/id/store.db\n".into(),
            requests: Mutex::new(Vec::new()),
        };
        // launchd belongs to root, so libproc refuses it.
        let files = open_store_files(&[1, std::process::id()], &runner);
        assert_eq!(
            files,
            vec![(1, PathBuf::from("/Users/m/.cursor/chats/h/id/store.db"))]
        );
        let requests = runner.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].args[5], "1");
    }

    #[test]
    fn lsof_parser_keeps_only_store_paths_under_their_pid() {
        let parsed = parse_lsof_store_files(
            "p100\nfcwd\nn/Users/m\nf12\nn/Users/m/.cursor/chats/h/id/store.db\nn/Users/m/.cursor/chats/h/id/store.db-wal\np200\nn/tmp/other/store.db\n",
        );
        assert_eq!(
            parsed,
            vec![
                (100, PathBuf::from("/Users/m/.cursor/chats/h/id/store.db")),
                (200, PathBuf::from("/tmp/other/store.db")),
            ]
        );
    }

    #[test]
    fn inspect_lists_recent_prompts_oldest_first() {
        let temp = tempfile::tempdir().unwrap();
        write_chat(
            temp.path(),
            "8550e5f0",
            CHAT_A,
            "Agent Comparison",
            "/Users/m",
            2_000_000,
        );
        let source = CursorHistorySource::host(temp.path().to_owned(), None);
        let session = source.discover(&request()).unwrap().remove(0);
        let text = inspect_cursor_history(temp.path(), &session).unwrap();
        assert!(text.starts_with(&format!("Agent Comparison · {}", absolute("/Users/m"))));
        let older = text.find("older prompt").unwrap();
        let newest = text.find("newest prompt here").unwrap();
        assert!(older < newest);
        assert!(text.contains("Open the row to resume"));
        assert!(text.trim_end().ends_with("newest prompt here"));
    }

    fn filetime_old() -> SystemTime {
        SystemTime::now() - Duration::from_secs(3_600)
    }
}
