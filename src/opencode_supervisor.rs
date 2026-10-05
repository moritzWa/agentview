//! Durable ownership and HTTP control for agentview-managed OpenCode sessions.
//!
//! OpenCode's documented server is reconnectable, so the dashboard persists an
//! authenticated loopback endpoint plus exact Linux process identity and the
//! canonical session IDs it created. Unrelated history never enters this
//! ownership record.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::fmt::Write as FmtWrite;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::net::TcpListener;
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::Stdio;
use std::sync::Mutex;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::thread;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::time::Instant;
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::domain::SessionState;

const RECORD_VERSION: u32 = 1;
#[cfg(any(target_os = "linux", target_os = "macos"))]
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// A restart must not be abandoned halfway, so a server that ignores SIGTERM
/// this long is killed.
const RESTART_GRACE: Duration = Duration::from_secs(15);
const KILL_GRACE: Duration = Duration::from_secs(5);
const RESTART_PENDING_FILE: &str = "restart-pending.json";
const RESUME_PROMPT: &str = "continue";
const RECENT_DIRECTORY_WINDOW: Duration = Duration::from_secs(3 * 24 * 60 * 60);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerRecord {
    version: u32,
    pid: u32,
    process_start_token: String,
    process_cmdline: Vec<u8>,
    executable: String,
    port: u16,
    username: String,
    password: String,
    created_at_ms: u64,
    #[serde(default)]
    sessions: BTreeMap<String, OwnedSession>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct OwnedSession {
    id: String,
    cwd: PathBuf,
    title: String,
    summary: String,
    created_at_ms: u64,
    updated_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedOpenCodeSession {
    pub id: String,
    pub cwd: PathBuf,
    pub title: String,
    pub summary: String,
    pub state: SessionState,
    pub server_pid: u32,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// Top-level and subagent sessions the live server has a turn in flight for.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ServerActivity {
    pub server_pid: Option<u32>,
    pub running: BTreeSet<String>,
    pub questions: BTreeSet<String>,
    pub permissions: BTreeSet<String>,
}

/// Reconnectable controller for one agentview-owned authenticated OpenCode server.
pub struct OpenCodeSupervisor {
    executable: String,
    state_dir: PathBuf,
    record_path: PathBuf,
    lock_path: PathBuf,
    /// Server pid and how far that server can move a single TUI.
    client_targeting: Mutex<Option<(u32, SharedClientReach)>>,
}

/// How far `/tui/select-session` can move one attached TUI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SharedClientReach {
    /// The server would switch every attached TUI, so none can be shared.
    None,
    /// One TUI can switch between the sessions of the directory it started in.
    Directory,
    /// One TUI can also move to another directory with `targetDirectory`.
    AnyDirectory,
}

impl OpenCodeSupervisor {
    pub fn host(executable: impl Into<String>) -> Result<Self> {
        Self::with_state_dir(executable, default_state_dir()?)
    }

    pub fn with_state_dir(executable: impl Into<String>, state_dir: PathBuf) -> Result<Self> {
        ensure_private_directory(&state_dir)?;
        Ok(Self {
            executable: executable.into(),
            record_path: state_dir.join("server.json"),
            lock_path: state_dir.join("server.lock"),
            state_dir,
            client_targeting: Mutex::new(None),
        })
    }

    pub fn launch(&self, prompt: &str, cwd: &Path) -> Result<ManagedOpenCodeSession> {
        self.launch_with_model(prompt, cwd, None)
    }

    pub fn launch_with_model(
        &self,
        prompt: &str,
        cwd: &Path,
        model: Option<&str>,
    ) -> Result<ManagedOpenCodeSession> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            bail!("the OpenCode launch prompt cannot be empty");
        }
        validate_cwd(cwd)?;
        // Validate the complete turn body before creating a provider session.
        // A malformed selector must never leave an empty owned session behind.
        let prompt_body = opencode_prompt_body(prompt, model)?;
        let _lock = StateLock::acquire(&self.lock_path)?;
        let mut record = self.ensure_server_locked()?;
        let title = prompt
            .split_whitespace()
            .take(8)
            .collect::<Vec<_>>()
            .join(" ");
        let path = with_directory_query("/session", cwd);
        // A caller-supplied title disables OpenCode's generated title, so the
        // prompt words stay a local placeholder until the provider names it.
        let response = self.request_json(&record, "POST", &path, Some(&json!({})))?;
        let id = response
            .get("id")
            .and_then(Value::as_str)
            .context("OpenCode session creation omitted its canonical ID")?
            .to_owned();
        if record.sessions.contains_key(&id) {
            bail!("OpenCode returned a duplicate managed session ID");
        }
        let now = now_millis();
        record.sessions.insert(
            id.clone(),
            OwnedSession {
                id: id.clone(),
                cwd: cwd.into(),
                title,
                summary: prompt.into(),
                created_at_ms: response
                    .pointer("/time/created")
                    .and_then(Value::as_u64)
                    .unwrap_or(now),
                updated_at_ms: now,
            },
        );
        // Persist ownership before starting the turn. A 2xx async acceptance
        // cannot prove that a provider/model will eventually succeed, but it
        // does prove which exact session this server created.
        save_record(&self.record_path, &record)?;
        let path = with_directory_query(&format!("/session/{id}/prompt_async"), cwd);
        if let Err(error) = self.request_empty(&record, "POST", &path, Some(&prompt_body)) {
            return Err(error).context(format!(
                "OpenCode created owned session {id}, but rejected its initial prompt"
            ));
        }
        self.session_snapshot(&record, record.sessions.get(&id).expect("inserted session"))
    }

    /// List only sessions backed by an already-running, exactly verified
    /// server. Read-only discovery never starts a server as a side effect.
    pub fn list(&self) -> Result<Vec<ManagedOpenCodeSession>> {
        let _lock = StateLock::acquire(&self.lock_path)?;
        let Some(mut record) = self.live_record_locked()? else {
            return Ok(Vec::new());
        };
        let mut statuses_by_directory = BTreeMap::new();
        let mut sessions_by_directory = BTreeMap::new();
        for owned in record.sessions.values() {
            if !statuses_by_directory.contains_key(&owned.cwd) {
                let path = with_directory_query("/session/status", &owned.cwd);
                let statuses = self.request_json(&record, "GET", &path, None)?;
                statuses_by_directory.insert(owned.cwd.clone(), statuses);

                let path = with_directory_query("/session", &owned.cwd);
                let sessions = self.request_json(&record, "GET", &path, None)?;
                sessions_by_directory.insert(owned.cwd.clone(), sessions);
            }
        }
        let mut refreshed = BTreeMap::new();
        for owned in record.sessions.values() {
            let Some((title, updated_at_ms)) = provider_session_metadata(
                sessions_by_directory
                    .get(&owned.cwd)
                    .expect("sessions were fetched for each owned directory"),
                &owned.id,
            )?
            else {
                continue;
            };
            let title = if is_default_title(&title) {
                owned.title.clone()
            } else {
                title
            };
            if updated_at_ms <= owned.updated_at_ms && title == owned.title {
                continue;
            }
            let path = with_directory_query(
                &format!("/session/{}/message", url_path_segment(&owned.id)),
                &owned.cwd,
            );
            let summary = self
                .request_json(&record, "GET", &format!("{path}&limit=20"), None)
                .ok()
                .and_then(|messages| latest_assistant_summary(&messages));
            refreshed.insert(owned.id.clone(), (title, summary, updated_at_ms));
        }
        if !refreshed.is_empty() {
            for (id, (title, summary, updated_at_ms)) in refreshed {
                let Some(owned) = record.sessions.get_mut(&id) else {
                    continue;
                };
                owned.title = title;
                if let Some(summary) = summary {
                    owned.summary = summary;
                }
                owned.updated_at_ms = updated_at_ms;
            }
            save_record(&self.record_path, &record)?;
        }
        Ok(record
            .sessions
            .values()
            .map(|owned| {
                let statuses = statuses_by_directory
                    .get(&owned.cwd)
                    .expect("status was fetched for each owned directory");
                self.snapshot_with_state(&record, owned, state_from_statuses(statuses, &owned.id))
            })
            .collect())
    }

    pub fn inspect(&self, session_id: &str) -> Result<String> {
        let _lock = StateLock::acquire(&self.lock_path)?;
        let record = self.required_live_record_locked()?;
        let owned = require_owned(&record, session_id)?;
        let path = with_directory_query(
            &format!("/session/{}/message", url_path_segment(session_id)),
            &owned.cwd,
        );
        self.render_latest_messages(&record, &path)
    }

    /// A session too long for one response keeps its latest messages.
    fn render_latest_messages(&self, record: &ServerRecord, path: &str) -> Result<String> {
        let messages = self
            .request_json(record, "GET", path, None)
            .or_else(|_| self.request_json(record, "GET", &format!("{path}&limit=50"), None))?;
        render_messages(&messages)
    }

    /// Read-only transcript of any session the live server can load, for
    /// sessions this dashboard opened without starting them. `opencode export`
    /// loses everything past its first 64 KiB when stdout is a pipe. Never
    /// starts a server.
    pub fn read_transcript(&self, session_id: &str, cwd: &Path) -> Result<String> {
        let _lock = StateLock::acquire(&self.lock_path)?;
        let record = self.required_live_record_locked()?;
        let path = with_directory_query(
            &format!("/session/{}/message", url_path_segment(session_id)),
            cwd,
        );
        self.render_latest_messages(&record, &path)
    }

    /// The title OpenCode holds for a session the live server can load. Never
    /// starts a server.
    pub fn session_title(&self, session_id: &str, cwd: &Path) -> Result<String> {
        let _lock = StateLock::acquire(&self.lock_path)?;
        let record = self.required_live_record_locked()?;
        let path = with_directory_query(&format!("/session/{}", url_path_segment(session_id)), cwd);
        let session = self.request_json(&record, "GET", &path, None)?;
        session
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .context("OpenCode returned a session without a title")
    }

    pub fn reply(&self, session_id: &str, prompt: &str) -> Result<()> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            bail!("the OpenCode reply cannot be empty");
        }
        let _lock = StateLock::acquire(&self.lock_path)?;
        let mut record = self.required_live_record_locked()?;
        let owned = require_owned(&record, session_id)?.clone();
        let path = with_directory_query(
            &format!("/session/{}/prompt_async", url_path_segment(session_id)),
            &owned.cwd,
        );
        self.request_empty(
            &record,
            "POST",
            &path,
            Some(&json!({"parts": [{"type": "text", "text": prompt}]})),
        )?;
        if let Some(session) = record.sessions.get_mut(session_id) {
            session.summary = prompt.into();
            session.updated_at_ms = now_millis();
        }
        save_record(&self.record_path, &record)
    }

    pub fn interrupt(&self, session_id: &str) -> Result<()> {
        let _lock = StateLock::acquire(&self.lock_path)?;
        let record = self.required_live_record_locked()?;
        let owned = require_owned(&record, session_id)?;
        let status = self.status_for(&record, owned)?;
        if status != SessionState::Working {
            bail!("the managed OpenCode session is not currently working");
        }
        let path = with_directory_query(
            &format!("/session/{}/abort", url_path_segment(session_id)),
            &owned.cwd,
        );
        self.request_empty(&record, "POST", &path, Some(&json!({})))
    }

    /// Build a native TUI client attached to the exact authenticated server
    /// that owns this session. The password stays in the child environment;
    /// it is never placed in argv, logs, or a dashboard notice.
    pub fn native_attach_command(&self, session_id: &str) -> Result<Command> {
        let _lock = StateLock::acquire(&self.lock_path)?;
        let record = self.required_live_record_locked()?;
        let owned = require_owned(&record, session_id)?;
        Ok(build_native_attach_command(
            &self.executable,
            &record,
            &owned.id,
            &owned.cwd,
        ))
    }

    /// How far the server, started if needed, can move one attached TUI.
    pub fn shared_client_reach(&self) -> Result<SharedClientReach> {
        let _lock = StateLock::acquire(&self.lock_path)?;
        let record = self.ensure_server_locked()?;
        Ok(self.client_reach(&record))
    }

    /// Attach a TUI that `select_in_shared_client` can later move between
    /// sessions, tagged with `client` so a switch reaches only this TUI.
    /// Without `session_id` it starts on OpenCode's home screen. Returns the
    /// command and the server's pid, or `None` when the server cannot target
    /// one client: it would accept the request but switch every attached TUI.
    pub fn shared_client_command(
        &self,
        session_id: Option<&str>,
        cwd: &Path,
        client: &str,
    ) -> Result<Option<(Command, u32)>> {
        let _lock = StateLock::acquire(&self.lock_path)?;
        let record = self.ensure_server_locked()?;
        if self.client_reach(&record) == SharedClientReach::None {
            return Ok(None);
        }
        let mut command = attach_command(&self.executable, &record, cwd);
        if let Some(session_id) = session_id {
            command.args(["--session", session_id]);
        }
        command.args(["--client", client]);
        Ok(Some((command, record.pid)))
    }

    /// Switch the shared TUI tagged `client` to `session_id`, moving it to
    /// `cwd` first when the server supports that. Returns the server's pid so
    /// the caller can tell a restarted server from the one the TUI was
    /// started against.
    pub fn select_in_shared_client(
        &self,
        session_id: &str,
        cwd: &Path,
        client: &str,
    ) -> Result<u32> {
        let _lock = StateLock::acquire(&self.lock_path)?;
        let record = self.required_live_record_locked()?;
        let mut body = json!({ "sessionID": session_id, "client": client });
        match self.client_reach(&record) {
            SharedClientReach::None => bail!("the OpenCode server cannot switch a single TUI"),
            SharedClientReach::Directory => {}
            SharedClientReach::AnyDirectory => {
                body["targetDirectory"] = json!(cwd.to_string_lossy());
            }
        }
        self.request_empty(
            &record,
            "POST",
            &with_directory_query("/tui/select-session", cwd),
            Some(&body),
        )?;
        Ok(record.pid)
    }

    /// The server pid and reach [`Self::shared_client_reach`] last found,
    /// without checking the server again.
    pub fn known_client_reach(&self) -> Option<(u32, SharedClientReach)> {
        *self.client_targeting.lock().ok()?
    }

    /// The directory recorded for an owned session, read from the state file
    /// without contacting the server.
    pub fn owned_session_cwd(&self, session_id: &str) -> Result<Option<PathBuf>> {
        let _lock = StateLock::acquire(&self.lock_path)?;
        Ok(
            load_record(&self.record_path, &self.state_dir)?.and_then(|record| {
                record
                    .sessions
                    .get(session_id)
                    .map(|owned| owned.cwd.clone())
            }),
        )
    }

    /// Run a TUI command in the TUIs attached to the live server for `cwd`.
    /// A TUI that does not know the command ignores it.
    pub fn run_tui_command(&self, cwd: &Path, command: &str) -> Result<()> {
        let _lock = StateLock::acquire(&self.lock_path)?;
        let Some(record) = self.live_record_locked()? else {
            return Ok(());
        };
        let body = json!({ "type": "tui.command.execute", "properties": { "command": command } });
        self.request_empty(
            &record,
            "POST",
            &with_directory_query("/tui/publish", cwd),
            Some(&body),
        )
    }

    /// Turns the live server is running in `directories`, for sessions this
    /// dashboard did not start but opened in its shared TUI: no `opencode`
    /// process of their own holds them, so only the server knows they run.
    /// Never starts a server.
    pub fn server_activity(&self, directories: &BTreeSet<PathBuf>) -> Result<ServerActivity> {
        let _lock = StateLock::acquire(&self.lock_path)?;
        let mut activity = ServerActivity::default();
        let Some(record) = self.live_record_locked()? else {
            return Ok(activity);
        };
        activity.server_pid = Some(record.pid);
        for directory in directories.iter().filter(|path| path.is_dir()) {
            let Ok(statuses) = self.request_json(
                &record,
                "GET",
                &with_directory_query("/session/status", directory),
                None,
            ) else {
                continue;
            };
            let running = running_session_ids(&statuses);
            if running.is_empty() {
                continue;
            }
            for (route, blocked) in [
                ("/question", &mut activity.questions),
                ("/permission", &mut activity.permissions),
            ] {
                if let Ok(requests) = self.request_json(
                    &record,
                    "GET",
                    &with_directory_query(route, directory),
                    None,
                ) {
                    blocked.extend(request_session_ids(&requests));
                }
            }
            activity.running.extend(running);
        }
        Ok(activity)
    }

    /// The live server's pid, which changes when the server restarts.
    pub fn live_server_pid(&self) -> Result<Option<u32>> {
        let _lock = StateLock::acquire(&self.lock_path)?;
        Ok(self.live_record_locked()?.map(|record| record.pid))
    }

    /// Which of `client` and `targetDirectory` the server's
    /// `/tui/select-session` accepts. Older servers silently drop unknown
    /// fields, so this reads the published schema instead of trying the
    /// request. Cached per server process.
    fn client_reach(&self, record: &ServerRecord) -> SharedClientReach {
        if let Ok(cache) = self.client_targeting.lock() {
            if let Some((pid, reach)) = *cache {
                if pid == record.pid {
                    return reach;
                }
            }
        }
        let reach = self
            .request_json(record, "GET", "/doc", None)
            .map(|doc| {
                let properties = doc.pointer(
                    "/paths/~1tui~1select-session/post/requestBody/content/application~1json/schema/properties",
                );
                let accepts = |field: &str| properties.and_then(|p| p.get(field)).is_some();
                match (accepts("client"), accepts("targetDirectory")) {
                    (true, true) => SharedClientReach::AnyDirectory,
                    (true, false) => SharedClientReach::Directory,
                    (false, _) => SharedClientReach::None,
                }
            })
            .unwrap_or(SharedClientReach::None);
        if let Ok(mut cache) = self.client_targeting.lock() {
            *cache = Some((record.pid, reach));
        }
        reach
    }

    /// Attach a native TUI to a session this supervisor did not create. Its
    /// turns then run in the durable server rather than in a TUI process the
    /// dashboard owns, so quitting the dashboard does not abort them.
    pub fn native_attach_command_for_external(
        &self,
        session_id: &str,
        cwd: &Path,
    ) -> Result<Command> {
        let _lock = StateLock::acquire(&self.lock_path)?;
        let record = self.ensure_server_locked()?;
        Ok(build_native_attach_command(
            &self.executable,
            &record,
            session_id,
            cwd,
        ))
    }

    /// Stop the exact verified test/development server. Normal dashboard exit
    /// deliberately leaves it running for reconnect.
    pub fn shutdown_server(&self) -> Result<()> {
        let _lock = StateLock::acquire(&self.lock_path)?;
        let Some(record) = self.live_record_locked()? else {
            return Ok(());
        };
        stop_server(&record, SHUTDOWN_GRACE, false)
    }

    /// Restart the owned server on the same port and credentials, then send
    /// `continue` to every top-level session whose turn the restart cut off.
    ///
    /// Attached native TUIs reconnect on their own because the endpoint does
    /// not change. Sessions blocked on a question or permission prompt are
    /// reported instead of resumed: their prompt dies with the old server and
    /// only the user can answer it. The interrupted set is persisted before
    /// the old server stops, so a failed restart is completed by running this
    /// again.
    pub fn restart_server(&self) -> Result<OpenCodeRestartReport> {
        let _lock = StateLock::acquire(&self.lock_path)?;
        let pending_path = self.state_dir.join(RESTART_PENDING_FILE);
        let mut pending = load_pending_turns(&pending_path)?;
        let previous = self.live_record_locked()?;
        let mut report = OpenCodeRestartReport::default();
        if let Some(record) = &previous {
            let (interrupted, awaiting_input) = self.interrupted_turns(record)?;
            for turn in interrupted {
                if !pending.iter().any(|known| known.id == turn.id) {
                    pending.push(turn);
                }
            }
            report.previous_pid = Some(record.pid);
            report.awaiting_input = awaiting_input;
            save_pending_turns(&pending_path, &pending)?;
            stop_server(record, RESTART_GRACE, true)?;
        }
        let sessions = load_record(&self.record_path, &self.state_dir)?
            .map(|record| record.sessions)
            .unwrap_or_default();
        let executable = previous
            .as_ref()
            .map_or(self.executable.as_str(), |record| {
                record.executable.as_str()
            });
        let record = match &previous {
            Some(endpoint) => self
                .start_server(executable, sessions.clone(), Some(endpoint))
                .or_else(|_| self.start_server(executable, sessions, None))?,
            None => self.start_server(executable, sessions, None)?,
        };
        report.pid = record.pid;
        report.port = record.port;
        report.same_port = previous
            .as_ref()
            .is_some_and(|endpoint| endpoint.port == record.port);
        self.resume_turns(&record, pending, &mut report)?;
        Ok(report)
    }

    /// Stop the owned server and remember which top-level turns it cut off,
    /// so the next server start sends them `continue`. Sessions blocked on a
    /// question or permission prompt are reported instead.
    pub fn stop_server_for_resume(&self) -> Result<OpenCodeStopReport> {
        let _lock = StateLock::acquire(&self.lock_path)?;
        let pending_path = self.state_dir.join(RESTART_PENDING_FILE);
        let mut pending = load_pending_turns(&pending_path)?;
        let mut report = OpenCodeStopReport::default();
        let Some(record) = self.live_record_locked()? else {
            report.saved = pending.into_iter().map(|turn| turn.id).collect();
            return Ok(report);
        };
        let (interrupted, awaiting_input) = self.interrupted_turns(&record)?;
        for turn in interrupted {
            if !pending.iter().any(|known| known.id == turn.id) {
                pending.push(turn);
            }
        }
        save_pending_turns(&pending_path, &pending)?;
        stop_server(&record, RESTART_GRACE, true)?;
        report.previous_pid = Some(record.pid);
        report.awaiting_input = awaiting_input;
        report.saved = pending.into_iter().map(|turn| turn.id).collect();
        Ok(report)
    }

    /// Send `continue` to each turn, keeping the ones that fail in the resume
    /// list so the next start or restart retries them.
    fn resume_turns(
        &self,
        record: &ServerRecord,
        pending: Vec<InterruptedTurn>,
        report: &mut OpenCodeRestartReport,
    ) -> Result<()> {
        let pending_path = self.state_dir.join(RESTART_PENDING_FILE);
        let mut unresumed = Vec::new();
        for turn in pending {
            let path = with_directory_query(
                &format!("/session/{}/prompt_async", url_path_segment(&turn.id)),
                &turn.cwd,
            );
            match self.request_empty(record, "POST", &path, Some(&resume_prompt_body(&turn))) {
                Ok(()) => report.resumed.push(turn.id),
                Err(error) => {
                    report.failed.push((turn.id.clone(), format!("{error:#}")));
                    unresumed.push(turn);
                }
            }
        }
        if unresumed.is_empty() {
            match fs::remove_file(&pending_path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error).context("failed to clear restart resume list"),
            }
        } else {
            save_pending_turns(&pending_path, &unresumed)
        }
    }

    /// Turns a server that is no longer running left behind: the saved resume
    /// list, plus, when the machine rebooted since that server started, every
    /// top-level turn whose reply never completed. A reboot leaves no process
    /// that could still be running such a turn, so resuming cannot run it
    /// twice; after a crash without a reboot, a standalone TUI might be.
    fn turns_left_by_stopped_server(
        &self,
        previous: Option<&ServerRecord>,
        report: &mut OpenCodeRestartReport,
    ) -> Result<Vec<InterruptedTurn>> {
        let mut pending = load_pending_turns(&self.state_dir.join(RESTART_PENDING_FILE))?;
        let Some(previous) = previous else {
            return Ok(pending);
        };
        if !boot_time_ms().is_some_and(|booted| booted > previous.created_at_ms) {
            return Ok(pending);
        }
        report.previous_pid = Some(previous.pid);
        for (id, cwd, awaiting_input) in
            unfinished_top_level_turns(&self.executable, previous.created_at_ms)
        {
            if awaiting_input {
                report.awaiting_input.push(id);
            } else if !pending.iter().any(|known| known.id == id) {
                pending.push(InterruptedTurn {
                    id,
                    cwd,
                    agent: None,
                    provider_id: None,
                    model_id: None,
                    variant: None,
                });
            }
        }
        Ok(pending)
    }

    /// Fill in the agent and model of turns found without a server, from the
    /// last user message the new server returns.
    fn with_turn_settings(
        &self,
        record: &ServerRecord,
        turns: Vec<InterruptedTurn>,
    ) -> Vec<InterruptedTurn> {
        turns
            .into_iter()
            .map(|turn| {
                if turn.agent.is_some() || turn.model_id.is_some() {
                    return turn;
                }
                let path = with_directory_query(
                    &format!("/session/{}/message", url_path_segment(&turn.id)),
                    &turn.cwd,
                );
                let messages = self
                    .request_json(record, "GET", &format!("{path}&limit=20"), None)
                    .unwrap_or(Value::Null);
                interrupted_turn(turn.id, turn.cwd, &messages)
            })
            .collect()
    }

    /// Top-level sessions with a turn in flight on this server, split into
    /// those a restart can resume and those waiting on the user.
    fn interrupted_turns(
        &self,
        record: &ServerRecord,
    ) -> Result<(Vec<InterruptedTurn>, Vec<String>)> {
        let mut directories = record
            .sessions
            .values()
            .map(|owned| owned.cwd.clone())
            .collect::<BTreeSet<_>>();
        directories.extend(recent_session_directories(&record.executable));
        let mut interrupted = Vec::new();
        let mut awaiting_input = Vec::new();
        for directory in directories.into_iter().filter(|path| path.is_dir()) {
            // A directory whose project fails to load has no running turns,
            // and must not block restarting everything else.
            let Ok(statuses) = self.request_json(
                record,
                "GET",
                &with_directory_query("/session/status", &directory),
                None,
            ) else {
                continue;
            };
            let running = running_session_ids(&statuses);
            if running.is_empty() {
                continue;
            }
            let mut blocked = BTreeSet::new();
            for route in ["/question", "/permission"] {
                let requests = self.request_json(
                    record,
                    "GET",
                    &with_directory_query(route, &directory),
                    None,
                )?;
                blocked.extend(request_session_ids(&requests));
            }
            for id in running {
                let session = self.request_json(
                    record,
                    "GET",
                    &with_directory_query(
                        &format!("/session/{}", url_path_segment(&id)),
                        &directory,
                    ),
                    None,
                )?;
                if session.get("parentID").and_then(Value::as_str).is_some() {
                    continue;
                }
                if blocked.contains(&id) {
                    awaiting_input.push(id);
                    continue;
                }
                let path = with_directory_query(
                    &format!("/session/{}/message", url_path_segment(&id)),
                    &directory,
                );
                let messages = self
                    .request_json(record, "GET", &format!("{path}&limit=20"), None)
                    .unwrap_or(Value::Null);
                interrupted.push(interrupted_turn(id, directory.clone(), &messages));
            }
        }
        Ok((interrupted, awaiting_input))
    }

    fn ensure_server_locked(&self) -> Result<ServerRecord> {
        let previous = load_record(&self.record_path, &self.state_dir)?;
        if let Some(record) = &previous {
            if record.version != RECORD_VERSION {
                bail!(
                    "unsupported OpenCode supervisor record version {}",
                    record.version
                );
            }
            if verify_server(record)? {
                if !record_uses_executable(record, &self.executable) {
                    bail!(
                        "a verified OpenCode server is already running with executable {}; configured executable is {}",
                        record.executable,
                        self.executable
                    );
                }
                self.verify_http_endpoint(record)?;
                return Ok(record.clone());
            }
        }
        let mut report = OpenCodeRestartReport::default();
        let pending = self.turns_left_by_stopped_server(previous.as_ref(), &mut report)?;
        let record = self.start_server(
            &self.executable,
            previous.map(|record| record.sessions).unwrap_or_default(),
            None,
        )?;
        if !pending.is_empty() {
            let pending = self.with_turn_settings(&record, pending);
            // Turns that fail to resume stay in the resume list; they must not
            // keep the dashboard from starting.
            let _ = self.resume_turns(&record, pending, &mut report);
        }
        Ok(record)
    }

    /// Start a server, on `endpoint`'s port and credentials when given so
    /// clients of a stopped predecessor reconnect without new arguments.
    fn start_server(
        &self,
        executable: &str,
        sessions: BTreeMap<String, OwnedSession>,
        endpoint: Option<&ServerRecord>,
    ) -> Result<ServerRecord> {
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (executable, sessions, endpoint);
            bail!("durable OpenCode supervision currently requires Linux or macOS process identity verification")
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let (port, username, password) = match endpoint {
                Some(record) => (
                    record.port,
                    record.username.clone(),
                    record.password.clone(),
                ),
                None => {
                    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))?;
                    let port = listener.local_addr()?.port();
                    drop(listener);
                    (port, "opencode".to_owned(), random_secret()?)
                }
            };
            use std::os::unix::process::CommandExt;

            let log = private_append_file(&self.state_dir.join("server.log"))?;
            let mut child = Command::new(executable)
                // The server outlives the dashboard, so it must not receive the
                // interrupt or hangup the terminal sends to agentview's process group.
                .process_group(0)
                .args([
                    "serve",
                    "--hostname",
                    "127.0.0.1",
                    "--port",
                    &port.to_string(),
                ])
                .env("OPENCODE_SERVER_USERNAME", &username)
                .env("OPENCODE_SERVER_PASSWORD", &password)
                .stdin(Stdio::null())
                .stdout(Stdio::from(log.try_clone()?))
                .stderr(Stdio::from(log))
                .spawn()
                .with_context(|| format!("failed to start OpenCode server via {executable}"))?;
            let pid = child.id();
            let mut record = ServerRecord {
                version: RECORD_VERSION,
                pid,
                process_start_token: String::new(),
                process_cmdline: Vec::new(),
                executable: executable.to_owned(),
                port,
                username,
                password,
                created_at_ms: now_millis(),
                sessions,
            };
            let deadline = Instant::now() + STARTUP_TIMEOUT;
            let result = loop {
                match self.verify_http_endpoint(&record) {
                    Ok(()) => break Ok(()),
                    Err(error) if Instant::now() < deadline => {
                        let _ = error;
                        thread::sleep(Duration::from_millis(40));
                    }
                    Err(error) => break Err(error),
                }
            };
            if let Err(error) = result {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error).context("OpenCode server failed readiness/ownership checks");
            }
            record.process_start_token = process_start_token(pid)?;
            record.process_cmdline = process_cmdline(pid)?;
            if record.process_cmdline.is_empty() {
                let _ = child.kill();
                let _ = child.wait();
                bail!("new OpenCode server exposed an empty process command line");
            }
            // Bind the now-stable process identity to the listener and secret
            // one final time before persisting authority.
            if let Err(error) = self.verify_http_endpoint(&record) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error).context("OpenCode server identity changed during startup");
            }
            save_record(&self.record_path, &record)?;
            thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(record)
        }
    }

    fn live_record_locked(&self) -> Result<Option<ServerRecord>> {
        let Some(record) = load_record(&self.record_path, &self.state_dir)? else {
            return Ok(None);
        };
        if !verify_server(&record)? {
            return Ok(None);
        }
        self.verify_http_endpoint(&record)?;
        Ok(Some(record))
    }

    fn required_live_record_locked(&self) -> Result<ServerRecord> {
        self.live_record_locked()?
            .context("OpenCode supervisor has no live owned server")
    }

    fn verify_http_endpoint(&self, record: &ServerRecord) -> Result<()> {
        verify_listener_owner(record.pid, record.port)?;
        let health = self.request_json(record, "GET", "/global/health", None)?;
        if health.get("healthy").and_then(Value::as_bool) != Some(true) {
            bail!("OpenCode health endpoint did not report healthy");
        }
        Ok(())
    }

    fn session_snapshot(
        &self,
        record: &ServerRecord,
        owned: &OwnedSession,
    ) -> Result<ManagedOpenCodeSession> {
        let state = self.status_for(record, owned)?;
        Ok(self.snapshot_with_state(record, owned, state))
    }

    fn snapshot_with_state(
        &self,
        record: &ServerRecord,
        owned: &OwnedSession,
        state: SessionState,
    ) -> ManagedOpenCodeSession {
        ManagedOpenCodeSession {
            id: owned.id.clone(),
            cwd: owned.cwd.clone(),
            title: owned.title.clone(),
            summary: owned.summary.clone(),
            state,
            server_pid: record.pid,
            created_at_ms: owned.created_at_ms,
            updated_at_ms: owned.updated_at_ms,
        }
    }

    fn status_for(&self, record: &ServerRecord, owned: &OwnedSession) -> Result<SessionState> {
        let path = with_directory_query("/session/status", &owned.cwd);
        let statuses = self.request_json(record, "GET", &path, None)?;
        Ok(state_from_statuses(&statuses, &owned.id))
    }

    fn request_json(
        &self,
        record: &ServerRecord,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value> {
        let response = request_http(record, method, path, body)?;
        if !(200..300).contains(&response.status) {
            return Err(HttpStatusError {
                status: response.status,
                body: String::from_utf8_lossy(&response.body).into_owned(),
            }
            .into());
        }
        if response.body.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&response.body).context("invalid OpenCode server JSON")
    }

    fn request_empty(
        &self,
        record: &ServerRecord,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<()> {
        self.request_json(record, method, path, body).map(|_| ())
    }
}

/// Signal the exact verified server and wait for it to exit. With `force`, a
/// server still running after `grace` is killed rather than reported.
fn stop_server(record: &ServerRecord, grace: Duration, force: bool) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::{AsRawFd, FromRawFd};

        let raw_fd =
            unsafe { libc::syscall(libc::SYS_pidfd_open, record.pid as libc::pid_t, 0_u32) };
        if raw_fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("failed to open exact OpenCode server pidfd");
        }
        let pidfd = unsafe { File::from_raw_fd(raw_fd as i32) };
        // Open the stable kernel reference first, then revalidate that the
        // PID still denotes the recorded process before signaling through
        // the pidfd. No persisted numeric PID is ever passed to kill(2).
        if !verify_server(record)? {
            bail!("OpenCode server identity changed before shutdown");
        }
        verify_listener_owner(record.pid, record.port)?;
        let signal = |signal: libc::c_int| -> Result<()> {
            let sent = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    pidfd.as_raw_fd(),
                    signal,
                    std::ptr::null::<libc::siginfo_t>(),
                    0_u32,
                )
            };
            if sent != 0 {
                return Err(std::io::Error::last_os_error())
                    .context("failed to stop exact OpenCode server through pidfd");
            }
            Ok(())
        };
        let exited = |timeout: Duration| -> Result<bool> {
            let mut descriptor = libc::pollfd {
                fd: pidfd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let polled = unsafe { libc::poll(&mut descriptor, 1, timeout.as_millis() as i32) };
            if polled < 0 {
                return Err(std::io::Error::last_os_error())
                    .context("failed while waiting for exact OpenCode server exit");
            }
            Ok(polled > 0)
        };
        signal(libc::SIGTERM)?;
        if exited(grace)? {
            return Ok(());
        }
        if force {
            signal(libc::SIGKILL)?;
            if exited(KILL_GRACE)? {
                return Ok(());
            }
        }
        bail!("timed out waiting for exact OpenCode server to exit")
    }
    #[cfg(target_os = "macos")]
    {
        // macOS has no pidfd, so the identity check and the signal are two
        // steps. The window is a PID reuse within microseconds of a check
        // that also matched the start time, command line, and listener.
        if !verify_server(record)? {
            bail!("OpenCode server identity changed before shutdown");
        }
        verify_listener_owner(record.pid, record.port)?;
        let exited = |timeout: Duration| -> Result<bool> {
            let deadline = Instant::now() + timeout;
            while verify_server(record)? {
                if Instant::now() >= deadline {
                    return Ok(false);
                }
                thread::sleep(Duration::from_millis(40));
            }
            Ok(true)
        };
        if unsafe { libc::kill(record.pid as libc::pid_t, libc::SIGTERM) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("failed to stop exact OpenCode server");
        }
        if exited(grace)? {
            return Ok(());
        }
        if force && verify_server(record)? {
            if unsafe { libc::kill(record.pid as libc::pid_t, libc::SIGKILL) } != 0 {
                return Err(std::io::Error::last_os_error())
                    .context("failed to kill exact OpenCode server");
            }
            if exited(KILL_GRACE)? {
                return Ok(());
            }
        }
        bail!("timed out waiting for exact OpenCode server to exit")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (record, grace, force);
        bail!("durable OpenCode supervision currently requires Linux or macOS")
    }
}

/// A turn a restart cut off, with the agent and model its last user message
/// used so the resume does not fall back to the default agent.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct InterruptedTurn {
    id: String,
    cwd: PathBuf,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    provider_id: Option<String>,
    #[serde(default)]
    model_id: Option<String>,
    #[serde(default)]
    variant: Option<String>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeRestartReport {
    pub previous_pid: Option<u32>,
    pub pid: u32,
    pub port: u16,
    /// False when the old port could not be reused; attached TUIs then need
    /// to be reopened from the dashboard.
    pub same_port: bool,
    pub resumed: Vec<String>,
    /// Sessions that were blocked on a question or permission prompt.
    pub awaiting_input: Vec<String>,
    pub failed: Vec<(String, String)>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeStopReport {
    pub previous_pid: Option<u32>,
    /// Sessions the next server start sends `continue`.
    pub saved: Vec<String>,
    /// Sessions that were blocked on a question or permission prompt.
    pub awaiting_input: Vec<String>,
}

fn running_session_ids(statuses: &Value) -> Vec<String> {
    statuses
        .as_object()
        .map(|statuses| {
            statuses
                .iter()
                .filter(|(_, status)| {
                    status
                        .get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|kind| kind != "idle")
                })
                .map(|(id, _)| id.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn request_session_ids(requests: &Value) -> Vec<String> {
    requests
        .as_array()
        .map(|requests| {
            requests
                .iter()
                .filter_map(|request| request.get("sessionID").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn interrupted_turn(id: String, cwd: PathBuf, messages: &Value) -> InterruptedTurn {
    let last_user = messages.as_array().and_then(|messages| {
        messages
            .iter()
            .filter_map(|message| message.get("info"))
            .filter(|info| info.get("role").and_then(Value::as_str) == Some("user"))
            .max_by_key(|info| {
                info.pointer("/time/created")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            })
    });
    let text = |pointer: &str| {
        last_user
            .and_then(|info| info.pointer(pointer))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    InterruptedTurn {
        agent: text("/agent"),
        provider_id: text("/model/providerID"),
        model_id: text("/model/modelID"),
        variant: text("/model/variant").or_else(|| text("/variant")),
        id,
        cwd,
    }
}

fn resume_prompt_body(turn: &InterruptedTurn) -> Value {
    let mut body = json!({"parts": [{"type": "text", "text": RESUME_PROMPT}]});
    if let Some(agent) = &turn.agent {
        body["agent"] = json!(agent);
    }
    if let (Some(provider), Some(model)) = (&turn.provider_id, &turn.model_id) {
        body["model"] = json!({"providerID": provider, "modelID": model});
    }
    if let Some(variant) = &turn.variant {
        body["variant"] = json!(variant);
    }
    body
}

/// Directories of recently active top-level sessions. Server status is
/// scoped per directory, so these are the places a running turn can be.
fn recent_session_directories(executable: &str) -> Vec<PathBuf> {
    let since = now_millis().saturating_sub(RECENT_DIRECTORY_WINDOW.as_millis() as u64);
    let query = format!(
        "select distinct directory from session where parent_id is null and time_updated > {since}"
    );
    let Ok(output) = Command::new(executable)
        .args(["db", &query, "--format", "tsv"])
        .stdin(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Top-level sessions whose latest message is a reply started at or after
/// `since` that never completed, with whether it stopped on a question. A
/// finished or aborted reply records `time.completed`; a killed server never
/// gets to.
fn unfinished_top_level_turns(executable: &str, since: u64) -> Vec<(String, PathBuf, bool)> {
    let query = format!(
        "select s.id, s.directory, exists (select 1 from part p where p.message_id = m.id \
         and json_extract(p.data, '$.type') = 'tool' and json_extract(p.data, '$.tool') = 'question' \
         and json_extract(p.data, '$.state.status') in ('pending', 'running')) as awaiting_input \
         from session s join message m on m.id = (select id from message where session_id = s.id \
         order by time_created desc, id desc limit 1) \
         where s.parent_id is null and s.time_archived is null and m.time_created >= {since} \
         and json_extract(m.data, '$.role') = 'assistant' \
         and json_extract(m.data, '$.time.completed') is null"
    );
    let Ok(output) = Command::new(executable)
        .args(["db", &query, "--format", "tsv"])
        .stdin(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let id = fields.next()?.trim();
            let directory = fields.next()?.trim();
            let awaiting_input = fields.next()?.trim() == "1";
            (!id.is_empty() && !directory.is_empty())
                .then(|| (id.to_owned(), PathBuf::from(directory), awaiting_input))
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn boot_time_ms() -> Option<u64> {
    let mut boot = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut size = std::mem::size_of::<libc::timeval>();
    let name = c"kern.boottime";
    let result = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&mut boot as *mut libc::timeval).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (result == 0 && boot.tv_sec > 0).then(|| boot.tv_sec as u64 * 1000 + boot.tv_usec as u64 / 1000)
}

#[cfg(target_os = "linux")]
fn boot_time_ms() -> Option<u64> {
    fs::read_to_string("/proc/stat")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("btime "))?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|seconds| seconds * 1000)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn boot_time_ms() -> Option<u64> {
    None
}

fn load_pending_turns(path: &Path) -> Result<Vec<InterruptedTurn>> {
    match fs::read(path) {
        Ok(input) => serde_json::from_slice(&input).context("invalid restart resume list"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error).context("failed to read restart resume list"),
    }
}

fn save_pending_turns(path: &Path, turns: &[InterruptedTurn]) -> Result<()> {
    let temporary = path.with_extension(format!("tmp-{}-{}", std::process::id(), now_millis()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let result = (|| -> Result<()> {
        let mut file = options.open(&temporary)?;
        serde_json::to_writer_pretty(&mut file, turns)?;
        file.sync_all()?;
        crate::fs_util::replace_file(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn require_owned<'a>(record: &'a ServerRecord, session_id: &str) -> Result<&'a OwnedSession> {
    record
        .sessions
        .get(session_id)
        .context("refusing to control an OpenCode session not created by this supervisor")
}

fn build_native_attach_command(
    executable: &str,
    record: &ServerRecord,
    session_id: &str,
    cwd: &Path,
) -> Command {
    let mut command = attach_command(executable, record, cwd);
    command.args(["--session", session_id]);
    command
}

fn attach_command(executable: &str, record: &ServerRecord, cwd: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .arg("attach")
        .arg(format!("http://127.0.0.1:{}", record.port))
        .arg("--dir")
        .arg(cwd)
        .env("OPENCODE_SERVER_USERNAME", &record.username)
        .env("OPENCODE_SERVER_PASSWORD", &record.password)
        .current_dir(cwd);
    command
}

#[derive(Debug)]
struct HttpStatusError {
    status: u16,
    body: String,
}

impl std::fmt::Display for HttpStatusError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "OpenCode HTTP status {}: {}",
            self.status,
            self.body.trim()
        )
    }
}

impl std::error::Error for HttpStatusError {}

fn state_from_statuses(statuses: &Value, session_id: &str) -> SessionState {
    let status = statuses
        .get(session_id)
        .and_then(|status| status.get("type").or(Some(status)))
        .and_then(Value::as_str);
    match status {
        Some("busy" | "active" | "running") => SessionState::Working,
        Some("retry" | "error") => SessionState::NeedsInput,
        Some("idle") | None => SessionState::Completed,
        Some(_) => SessionState::Unknown,
    }
}

struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

fn request_http(
    record: &ServerRecord,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<HttpResponse> {
    let body = body
        .map(serde_json::to_vec)
        .transpose()?
        .unwrap_or_default();
    let authorization =
        base64_encode(format!("{}:{}", record.username, record.password).as_bytes());
    let mut stream = TcpStream::connect_timeout(
        &SocketAddrV4::new(Ipv4Addr::LOCALHOST, record.port).into(),
        HTTP_TIMEOUT,
    )?;
    stream.set_read_timeout(Some(HTTP_TIMEOUT))?;
    stream.set_write_timeout(Some(HTTP_TIMEOUT))?;
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Basic {authorization}\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        record.port,
        body.len()
    )?;
    stream.write_all(&body)?;
    stream.flush()?;
    read_http_response(&mut stream)
}

fn read_http_response(stream: &mut TcpStream) -> Result<HttpResponse> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() > MAX_HEADER_BYTES + MAX_BODY_BYTES {
            bail!("OpenCode HTTP response exceeded size limit");
        }
        // OpenCode may keep an HTTP/1.1 connection alive even when the client
        // asks it to close. Stop as soon as framing proves the complete body
        // arrived instead of waiting for EOF and turning a healthy response
        // into an EAGAIN/read-timeout launch failure.
        if http_response_is_complete(&bytes)? {
            break;
        }
    }
    if bytes.len() > MAX_HEADER_BYTES + MAX_BODY_BYTES {
        bail!("OpenCode HTTP response exceeded size limit");
    }
    let split =
        find_bytes(&bytes, b"\r\n\r\n").context("OpenCode HTTP response omitted headers")?;
    if split > MAX_HEADER_BYTES {
        bail!("OpenCode HTTP headers exceeded size limit");
    }
    let headers =
        std::str::from_utf8(&bytes[..split]).context("OpenCode HTTP headers were not UTF-8")?;
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .context("OpenCode HTTP response omitted status")?
        .parse::<u16>()?;
    let mut body = bytes[split + 4..].to_vec();
    let chunked = headers.lines().any(|line| {
        line.split_once(':')
            .map(|(name, value)| {
                name.eq_ignore_ascii_case("transfer-encoding")
                    && value.to_ascii_lowercase().contains("chunked")
            })
            .unwrap_or(false)
    });
    if chunked {
        body = decode_chunked(&body)?;
    } else if let Some(content_length) = content_length(headers)? {
        if body.len() < content_length {
            bail!("truncated OpenCode HTTP response body");
        }
        body.truncate(content_length);
    }
    if body.len() > MAX_BODY_BYTES {
        bail!("OpenCode HTTP body exceeded size limit");
    }
    Ok(HttpResponse { status, body })
}

fn http_response_is_complete(bytes: &[u8]) -> Result<bool> {
    let Some(split) = find_bytes(bytes, b"\r\n\r\n") else {
        if bytes.len() > MAX_HEADER_BYTES {
            bail!("OpenCode HTTP headers exceeded size limit");
        }
        return Ok(false);
    };
    if split > MAX_HEADER_BYTES {
        bail!("OpenCode HTTP headers exceeded size limit");
    }
    let headers =
        std::str::from_utf8(&bytes[..split]).context("OpenCode HTTP headers were not UTF-8")?;
    let body = &bytes[split + 4..];
    if headers.lines().any(|line| {
        line.split_once(':')
            .map(|(name, value)| {
                name.eq_ignore_ascii_case("transfer-encoding")
                    && value.to_ascii_lowercase().contains("chunked")
            })
            .unwrap_or(false)
    }) {
        return Ok(decode_chunked(body).is_ok());
    }
    if let Some(expected) = content_length(headers)? {
        if expected > MAX_BODY_BYTES {
            bail!("OpenCode HTTP body exceeded size limit");
        }
        return Ok(body.len() >= expected);
    }
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok());
    Ok(matches!(status, Some(100..=199 | 204 | 304)))
}

fn content_length(headers: &str) -> Result<Option<usize>> {
    let mut result = None;
    for value in headers.lines().filter_map(|line| {
        line.split_once(':').and_then(|(name, value)| {
            name.eq_ignore_ascii_case("content-length")
                .then_some(value.trim())
        })
    }) {
        let parsed = value
            .parse::<usize>()
            .context("invalid OpenCode HTTP content length")?;
        if result.replace(parsed).is_some_and(|prior| prior != parsed) {
            bail!("conflicting OpenCode HTTP content lengths");
        }
    }
    Ok(result)
}

fn decode_chunked(input: &[u8]) -> Result<Vec<u8>> {
    let mut remaining = input;
    let mut output = Vec::new();
    loop {
        let line_end = find_bytes(remaining, b"\r\n").context("invalid chunked response")?;
        let size_text = std::str::from_utf8(&remaining[..line_end])?
            .split(';')
            .next()
            .unwrap_or_default();
        let size = usize::from_str_radix(size_text.trim(), 16)?;
        remaining = &remaining[line_end + 2..];
        if size == 0 {
            break;
        }
        if remaining.len() < size + 2 || &remaining[size..size + 2] != b"\r\n" {
            bail!("truncated chunked OpenCode response");
        }
        output.extend_from_slice(&remaining[..size]);
        if output.len() > MAX_BODY_BYTES {
            bail!("OpenCode HTTP body exceeded size limit");
        }
        remaining = &remaining[size + 2..];
    }
    Ok(output)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn render_messages(value: &Value) -> Result<String> {
    let messages = value
        .as_array()
        .context("OpenCode message endpoint did not return an array")?;
    let mut transcript = Vec::new();
    for message in messages {
        let role = message
            .pointer("/info/role")
            .and_then(Value::as_str)
            .unwrap_or("event");
        let text = message
            .get("parts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        if !text.trim().is_empty() {
            transcript.push(format!("{}: {}", capitalize(role), text.trim()));
        }
    }
    let transcript = if transcript.is_empty() {
        "No text messages are available in this managed OpenCode session.".into()
    } else {
        transcript.join("\n\n")
    };
    Ok(limit_chars(transcript, 32 * 1024))
}

fn provider_session_metadata(value: &Value, session_id: &str) -> Result<Option<(String, u64)>> {
    let sessions = value
        .as_array()
        .context("OpenCode session endpoint did not return an array")?;
    let Some(session) = sessions
        .iter()
        .find(|session| session.get("id").and_then(Value::as_str) == Some(session_id))
    else {
        return Ok(None);
    };
    let title = session
        .get("title")
        .and_then(Value::as_str)
        .context("OpenCode session omitted its title")?;
    let updated_at_ms = session
        .pointer("/time/updated")
        .and_then(Value::as_u64)
        .context("OpenCode session omitted its updated time")?;
    Ok(Some((title.to_owned(), updated_at_ms)))
}

/// Whether the terminal title an OpenCode TUI set is the one it shows for a
/// session titled `session_title`: `OpenCode` for a placeholder title, else
/// the title, cut short with `…` when long.
pub fn tui_shows_title(session_title: &str, terminal_title: &str) -> bool {
    if is_default_title(session_title) {
        return terminal_title == "OpenCode";
    }
    match terminal_title.strip_suffix('…') {
        Some(shortened) if !shortened.is_empty() => session_title.starts_with(shortened),
        _ => terminal_title == session_title,
    }
}

/// OpenCode's placeholder title (`New session - <ISO timestamp>`), which it
/// replaces with a generated one after the first turn.
fn is_default_title(title: &str) -> bool {
    let Some(stamp) = title
        .strip_prefix("New session - ")
        .or_else(|| title.strip_prefix("Child session - "))
    else {
        return false;
    };
    stamp.len() == 24
        && stamp.ends_with('Z')
        && stamp.bytes().enumerate().all(|(index, byte)| match index {
            4 | 7 => byte == b'-',
            10 => byte == b'T',
            13 | 16 => byte == b':',
            19 => byte == b'.',
            23 => byte == b'Z',
            _ => byte.is_ascii_digit(),
        })
}

fn latest_assistant_summary(value: &Value) -> Option<String> {
    let messages = value.as_array()?;
    messages
        .iter()
        .filter(|message| {
            message.pointer("/info/role").and_then(Value::as_str) == Some("assistant")
        })
        .filter_map(|message| {
            let text = message
                .get("parts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            let text = text.trim();
            (!text.is_empty()).then(|| {
                let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if normalized.chars().count() <= 160 {
                    normalized
                } else {
                    let mut summary = normalized.chars().take(159).collect::<String>();
                    summary.push('…');
                    summary
                }
            })
        })
        .last()
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

fn limit_chars(value: String, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value;
    }
    let tail = value
        .chars()
        .rev()
        .take(limit.saturating_sub(24))
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("[earlier output omitted]\n{tail}")
}

fn with_directory_query(path: &str, cwd: &Path) -> String {
    format!("{path}?directory={}", url_query_path(cwd))
}

fn url_query_path(path: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        percent_encode(path.as_os_str().as_bytes(), false)
    }
    #[cfg(not(unix))]
    percent_encode(path.to_string_lossy().as_bytes(), false)
}

fn url_path_segment(value: &str) -> String {
    percent_encode(value.as_bytes(), true)
}

fn percent_encode(bytes: &[u8], path_segment: bool) -> String {
    let mut output = String::new();
    for &byte in bytes {
        let safe = byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~')
            || (!path_segment && byte == b'/');
        if safe {
            output.push(byte as char);
        } else {
            output.push_str(&format!("%{byte:02X}"));
        }
    }
    output
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(TABLE[(first >> 2) as usize] as char);
        output.push(TABLE[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        output.push(if chunk.len() > 1 {
            TABLE[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[(third & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    output
}

fn validate_cwd(cwd: &Path) -> Result<()> {
    if !cwd.is_absolute() {
        bail!("the OpenCode working directory must be absolute");
    }
    let metadata = fs::metadata(cwd).context("failed to inspect OpenCode working directory")?;
    if !metadata.is_dir() {
        bail!("the OpenCode working directory is not a directory");
    }
    Ok(())
}

fn opencode_prompt_body(prompt: &str, model: Option<&str>) -> Result<Value> {
    let mut body = json!({"parts": [{"type": "text", "text": prompt}]});
    let Some(identifier) = model else {
        return Ok(body);
    };
    let identifier = identifier.trim();
    if identifier.is_empty()
        || identifier.len() > 128
        || identifier
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        bail!("the OpenCode model name must contain 1 to 128 non-whitespace bytes");
    }
    let (provider_id, model_id) = identifier
        .split_once('/')
        .context("the OpenCode model must use provider/model format")?;
    if provider_id.is_empty() || model_id.is_empty() {
        bail!("the OpenCode model must use provider/model format");
    }
    body.as_object_mut()
        .expect("the OpenCode prompt body is an object")
        .insert(
            "model".into(),
            json!({"providerID": provider_id, "modelID": model_id}),
        );
    Ok(body)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn random_secret() -> Result<String> {
    let mut bytes = [0_u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let mut secret = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        FmtWrite::write_fmt(&mut secret, format_args!("{byte:02x}"))?;
    }
    Ok(secret)
}

fn default_state_dir() -> Result<PathBuf> {
    if let Some(path) = crate::fs_util::xdg_home("XDG_STATE_HOME") {
        return Ok(PathBuf::from(path).join("agentview/opencode"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/state/agentview/opencode"))
}

fn ensure_private_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
                bail!("OpenCode supervisor state path is not a real directory");
            }
            verify_current_owner(&metadata, "OpenCode supervisor state directory")?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir_all(path)?,
        Err(error) => return Err(error).context("failed to inspect OpenCode supervisor state"),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn private_append_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        bail!("refusing to use a non-regular OpenCode supervisor log");
    }
    verify_current_owner(&metadata, "OpenCode supervisor log")?;
    verify_private_mode(&metadata, "OpenCode supervisor log")?;
    Ok(file)
}

fn load_record(path: &Path, state_dir: &Path) -> Result<Option<ServerRecord>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("failed to open OpenCode server record"),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        bail!("refusing to use a non-regular OpenCode server record");
    }
    verify_current_owner(&metadata, "OpenCode server record")?;
    verify_private_mode(&metadata, "OpenCode server record")?;
    let mut input = Vec::new();
    file.take((MAX_BODY_BYTES + 1) as u64)
        .read_to_end(&mut input)?;
    if input.len() > MAX_BODY_BYTES {
        bail!("OpenCode server record exceeded size limit");
    }
    let record: ServerRecord = serde_json::from_slice(&input)?;
    validate_record(&record)?;
    if state_dir != path.parent().unwrap_or(state_dir) {
        bail!("OpenCode server record escaped its state directory");
    }
    Ok(Some(record))
}

fn save_record(path: &Path, record: &ServerRecord) -> Result<()> {
    validate_record(record)?;
    let temporary = path.with_extension(format!("tmp-{}-{}", std::process::id(), now_millis()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let result = (|| -> Result<()> {
        let mut file = options.open(&temporary)?;
        serde_json::to_writer_pretty(&mut file, record)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        crate::fs_util::replace_file(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn validate_record(record: &ServerRecord) -> Result<()> {
    if record.version != RECORD_VERSION
        || record.port == 0
        || record.executable.is_empty()
        || record.username != "opencode"
        || record.password.len() != 64
        || !record.password.bytes().all(|byte| byte.is_ascii_hexdigit())
        || record.process_cmdline.len() > MAX_HEADER_BYTES
    {
        bail!("invalid OpenCode server authority record");
    }
    for (key, session) in &record.sessions {
        if key != &session.id
            || key.is_empty()
            || key.len() > 512
            || key.chars().any(char::is_control)
            || !session.cwd.is_absolute()
            || session.title.len() > MAX_HEADER_BYTES
            || session.summary.len() > MAX_BODY_BYTES
        {
            bail!("invalid owned OpenCode session record");
        }
    }
    Ok(())
}

fn verify_current_owner(metadata: &fs::Metadata, description: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } {
            bail!("{description} is not owned by the current user");
        }
    }
    #[cfg(not(unix))]
    let _ = (metadata, description);
    Ok(())
}

fn verify_private_mode(metadata: &fs::Metadata, description: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 {
            bail!("{description} is accessible by another user");
        }
    }
    #[cfg(not(unix))]
    let _ = (metadata, description);
    Ok(())
}

fn verify_server(record: &ServerRecord) -> Result<bool> {
    if record.process_start_token.is_empty() || record.process_cmdline.is_empty() {
        return Ok(false);
    }
    if process_state(record.pid)?.as_deref() == Some("Z") {
        return Ok(false);
    }
    let start = match process_start_token(record.pid) {
        Ok(value) => value,
        Err(error) if is_missing_process(&error) => return Ok(false),
        Err(error) => return Err(error),
    };
    let cmdline = match process_cmdline(record.pid) {
        Ok(value) => value,
        Err(error) if is_missing_process(&error) => return Ok(false),
        Err(error) => return Err(error),
    };
    Ok(start == record.process_start_token && cmdline == record.process_cmdline)
}

fn record_uses_executable(record: &ServerRecord, configured: &str) -> bool {
    if record.executable == configured {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        let actual = fs::read_link(format!("/proc/{}/exe", record.pid))
            .ok()
            .and_then(|path| fs::canonicalize(path).ok());
        actual.is_some() && actual == resolve_host_executable(configured)
    }
    #[cfg(target_os = "macos")]
    {
        let actual = process_executable(record.pid)
            .ok()
            .and_then(|path| fs::canonicalize(path).ok());
        actual.is_some() && actual == resolve_host_executable(configured)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    false
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn resolve_host_executable(executable: &str) -> Option<PathBuf> {
    let path = Path::new(executable);
    if path.components().count() > 1 {
        return fs::canonicalize(path).ok();
    }
    if let Some(found) = std::env::var_os("PATH").and_then(|search| {
        std::env::split_paths(&search)
            .map(|directory| directory.join(executable))
            .find(|candidate| candidate.is_file())
    }) {
        return fs::canonicalize(found).ok();
    }
    let home = PathBuf::from(std::env::var_os("HOME")?);
    [".local/bin", ".opencode/bin", ".bun/bin"]
        .iter()
        .map(|directory| home.join(directory).join(executable))
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| fs::canonicalize(candidate).ok())
}

#[cfg(target_os = "linux")]
fn process_state(pid: u32) -> Result<Option<String>> {
    let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(parse_process_stat(&stat)?.0))
}

#[cfg(target_os = "macos")]
fn process_state(pid: u32) -> Result<Option<String>> {
    match bsd_info(pid) {
        Ok(info) if info.pbi_status == libc::SZOMB => Ok(Some("Z".into())),
        Ok(_) => Ok(Some("R".into())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(target_os = "macos")]
fn bsd_info(pid: u32) -> std::io::Result<libc::proc_bsdinfo> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let read = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    if read == size {
        return Ok(info);
    }
    let error = std::io::Error::last_os_error();
    if read <= 0 && error.raw_os_error() == Some(libc::ESRCH) {
        return Err(std::io::ErrorKind::NotFound.into());
    }
    Err(error)
}

#[cfg(target_os = "macos")]
fn process_executable(pid: u32) -> std::io::Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;

    let mut buffer = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let length = unsafe {
        libc::proc_pidpath(
            pid as libc::c_int,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
        )
    };
    if length <= 0 {
        return Err(std::io::Error::last_os_error());
    }
    buffer.truncate(length as usize);
    Ok(PathBuf::from(std::ffi::OsString::from_vec(buffer)))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_state(_: u32) -> Result<Option<String>> {
    bail!("process-state verification is unavailable on this platform")
}

fn is_missing_process(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .map(|error| error.kind() == std::io::ErrorKind::NotFound)
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn process_start_token(pid: u32) -> Result<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    Ok(parse_process_stat(&stat)?.1)
}

#[cfg(target_os = "linux")]
fn parse_process_stat(stat: &str) -> Result<(String, String)> {
    let suffix = stat
        .rsplit_once(')')
        .map(|(_, suffix)| suffix)
        .context("invalid /proc process stat")?;
    let fields = suffix.split_whitespace().collect::<Vec<_>>();
    let state = fields
        .first()
        .map(|value| (*value).to_owned())
        .context("/proc process stat omitted state")?;
    let start = fields
        .get(19)
        .map(|value| (*value).to_owned())
        .context("/proc process stat omitted starttime")?;
    Ok((state, start))
}

#[cfg(target_os = "macos")]
fn process_start_token(pid: u32) -> Result<String> {
    let info = bsd_info(pid)?;
    Ok(format!(
        "{}.{:06}",
        info.pbi_start_tvsec, info.pbi_start_tvusec
    ))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_start_token(_: u32) -> Result<String> {
    bail!("process start-token verification is unavailable on this platform")
}

#[cfg(target_os = "linux")]
fn process_cmdline(pid: u32) -> Result<Vec<u8>> {
    fs::read(format!("/proc/{pid}/cmdline")).map_err(Into::into)
}

/// The argument vector in Linux `/proc/PID/cmdline` form: each argument
/// followed by a NUL. `KERN_PROCARGS2` returns argc, the executable path,
/// alignment NULs, the arguments, and then the environment, which is skipped.
#[cfg(target_os = "macos")]
fn process_cmdline(pid: u32) -> Result<Vec<u8>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut size: libc::size_t = 0;
    let sized = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if sized != 0 {
        return Err(missing_or_os_error(pid).into());
    }
    let mut buffer = vec![0_u8; size];
    let read = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if read != 0 {
        return Err(missing_or_os_error(pid).into());
    }
    buffer.truncate(size);
    parse_procargs2(&buffer)
}

#[cfg(target_os = "macos")]
fn missing_or_os_error(pid: u32) -> std::io::Error {
    let error = std::io::Error::last_os_error();
    // KERN_PROCARGS2 reports EINVAL rather than ESRCH for an exited process.
    match bsd_info(pid) {
        Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => missing,
        _ => error,
    }
}

#[cfg(any(target_os = "macos", test))]
fn parse_procargs2(buffer: &[u8]) -> Result<Vec<u8>> {
    let count = buffer.get(..4).context("process arguments omitted argc")?;
    let argc = i32::from_ne_bytes(count.try_into()?);
    let argc = usize::try_from(argc).context("process arguments reported a negative argc")?;
    let rest = &buffer[4..];
    let path_end = rest
        .iter()
        .position(|byte| *byte == 0)
        .context("process arguments omitted the executable path terminator")?;
    let mut position = path_end;
    while rest.get(position) == Some(&0) {
        position += 1;
    }
    let mut cmdline = Vec::new();
    for _ in 0..argc {
        let argument = rest
            .get(position..)
            .context("process arguments ended before argc arguments")?;
        let end = argument
            .iter()
            .position(|byte| *byte == 0)
            .context("process argument omitted its terminator")?;
        cmdline.extend_from_slice(&argument[..=end]);
        position += end + 1;
    }
    Ok(cmdline)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_cmdline(_: u32) -> Result<Vec<u8>> {
    bail!("process command-line verification is unavailable on this platform")
}

#[cfg(target_os = "linux")]
fn verify_listener_owner(pid: u32, port: u16) -> Result<()> {
    let mut socket_inodes = BTreeSet::new();
    for entry in fs::read_dir(format!("/proc/{pid}/fd"))? {
        let target = fs::read_link(entry?.path())?;
        let target = target.to_string_lossy();
        if let Some(inode) = target
            .strip_prefix("socket:[")
            .and_then(|value| value.strip_suffix(']'))
        {
            socket_inodes.insert(inode.to_owned());
        }
    }
    let expected_address = format!("0100007F:{port:04X}");
    let found = fs::read_to_string("/proc/net/tcp")?
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            (fields.len() > 9).then_some((fields[1], fields[3], fields[9]))
        })
        .any(|(address, state, inode)| {
            address == expected_address && state == "0A" && socket_inodes.contains(inode)
        });
    if !found {
        bail!("verified OpenCode process does not own the recorded loopback listener");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_listener_owner(pid: u32, port: u16) -> Result<()> {
    let output = Command::new("/usr/sbin/lsof")
        .args(["-nP", "-a", "-p", &pid.to_string()])
        .arg(format!("-iTCP@127.0.0.1:{port}"))
        .args(["-sTCP:LISTEN", "-Fp"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("failed to run lsof for OpenCode listener verification")?;
    let expected = format!("p{pid}");
    let found = output.status.success()
        && String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line == expected);
    if !found {
        bail!("verified OpenCode process does not own the recorded loopback listener");
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn verify_listener_owner(_: u32, _: u16) -> Result<()> {
    bail!("listener ownership verification is unavailable on this platform")
}

struct StateLock {
    file: File,
}

impl StateLock {
    fn acquire(path: &Path) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            bail!("refusing to use a non-regular OpenCode server lock");
        }
        verify_current_owner(&metadata, "OpenCode server lock")?;
        verify_private_mode(&metadata, "OpenCode server lock")?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
                return Err(std::io::Error::last_os_error())
                    .context("failed to lock OpenCode server state");
            }
        }
        Ok(Self { file })
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_basic_auth_and_url_components() {
        assert_eq!(base64_encode(b"opencode:secret"), "b3BlbmNvZGU6c2VjcmV0");
        assert_eq!(url_path_segment("ses_/ ?"), "ses_%2F%20%3F");
    }

    #[test]
    fn matches_the_terminal_title_the_tui_sets_for_a_session() {
        let long = "Optimizing Session Switching Performance in AV";
        assert!(tui_shows_title(
            long,
            "Optimizing Session Switching Performa…"
        ));
        assert!(tui_shows_title("Fix bug", "Fix bug"));
        assert!(!tui_shows_title("Fix bug", "Fix other bug"));
        assert!(!tui_shows_title(long, "Optimizing Session Status…"));
        assert!(!tui_shows_title("Fix bug", "…"));
        assert!(tui_shows_title(
            "New session - 2026-10-01T17:42:05.123Z",
            "OpenCode"
        ));
        assert!(!tui_shows_title("Fix bug", "OpenCode"));
    }

    #[test]
    fn recognizes_only_opencode_placeholder_titles() {
        assert!(is_default_title("New session - 2026-10-01T17:42:05.123Z"));
        assert!(is_default_title("Child session - 2026-10-01T17:42:05.123Z"));
        assert!(!is_default_title("New session - custom"));
        assert!(!is_default_title("Fix av session titles"));
    }

    #[test]
    fn renders_bounded_message_text() {
        let value = json!([
            {"info":{"role":"user"},"parts":[{"type":"text","text":"Build"}]},
            {"info":{"role":"assistant"},"parts":[{"type":"text","text":"Done"}]}
        ]);
        assert_eq!(
            render_messages(&value).unwrap(),
            "User: Build\n\nAssistant: Done"
        );
    }

    #[test]
    fn extracts_current_provider_metadata_and_latest_assistant_summary() {
        let sessions = json!([
            {"id":"other","title":"other","time":{"updated":2}},
            {"id":"ses_owned","title":"renamed task","time":{"updated":42}}
        ]);
        assert_eq!(
            provider_session_metadata(&sessions, "ses_owned").unwrap(),
            Some(("renamed task".into(), 42))
        );
        let messages = json!([
            {"info":{"role":"assistant"},"parts":[{"type":"text","text":"old answer"}]},
            {"info":{"role":"user"},"parts":[{"type":"text","text":"new question"}]},
            {"info":{"role":"assistant"},"parts":[
                {"type":"text","text":"  latest"},
                {"type":"tool"},
                {"type":"text","text":"answer  "}
            ]}
        ]);
        assert_eq!(
            latest_assistant_summary(&messages).as_deref(),
            Some("latest answer")
        );
    }

    #[test]
    fn builds_documented_model_selector_for_async_prompt() {
        assert_eq!(
            opencode_prompt_body("Build", Some("anthropic/claude-sonnet-4-5")).unwrap(),
            json!({
                "parts": [{"type": "text", "text": "Build"}],
                "model": {
                    "providerID": "anthropic",
                    "modelID": "claude-sonnet-4-5"
                }
            })
        );
        assert_eq!(
            opencode_prompt_body("Build", Some("openrouter/vendor/model")).unwrap()["model"],
            json!({"providerID": "openrouter", "modelID": "vendor/model"})
        );
        assert!(opencode_prompt_body("Build", Some("missing-provider")).is_err());
        assert!(opencode_prompt_body("Build", Some("openai/ ")).is_err());
    }

    #[test]
    fn reads_a_complete_keep_alive_response_without_waiting_for_eof() {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let body = br#"{"healthy":true}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
            stream.flush().unwrap();
            std::thread::sleep(Duration::from_millis(750));
        });

        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(250)))
            .unwrap();
        let started = std::time::Instant::now();
        let response = read_http_response(&mut stream).unwrap();
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(response.status, 200);
        assert_eq!(response.body, br#"{"healthy":true}"#);
        server.join().unwrap();
    }

    #[test]
    fn rejects_conflicting_content_lengths() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nx";
        assert!(http_response_is_complete(response).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_zombie_state_separately_from_start_identity() {
        let stat = "12 (provider worker) Z 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 4242";
        assert_eq!(
            parse_process_stat(stat).unwrap(),
            ("Z".into(), "4242".into())
        );
    }

    #[test]
    fn rejects_unbounded_or_relative_owned_session_records() {
        let record = ServerRecord {
            version: RECORD_VERSION,
            pid: 1,
            process_start_token: "start".into(),
            process_cmdline: b"command".to_vec(),
            executable: "opencode".into(),
            port: 1234,
            username: "opencode".into(),
            password: "a".repeat(64),
            created_at_ms: 1,
            sessions: BTreeMap::from([(
                "ses_owned".into(),
                OwnedSession {
                    id: "ses_owned".into(),
                    cwd: PathBuf::from("relative"),
                    title: "task".into(),
                    summary: String::new(),
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
            )]),
        };
        assert!(validate_record(&record).is_err());
    }

    #[test]
    fn native_attach_keeps_the_server_secret_out_of_argv() {
        let owned = OwnedSession {
            id: "ses_owned".into(),
            cwd: PathBuf::from("/work/project"),
            title: "task".into(),
            summary: String::new(),
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        let record = ServerRecord {
            version: RECORD_VERSION,
            pid: 1,
            process_start_token: "start".into(),
            process_cmdline: b"command".to_vec(),
            executable: "opencode".into(),
            port: 4242,
            username: "private-user".into(),
            password: "private-password".into(),
            created_at_ms: 1,
            sessions: BTreeMap::from([(owned.id.clone(), owned.clone())]),
        };

        let command = build_native_attach_command("/bin/opencode", &record, &owned.id, &owned.cwd);
        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            arguments,
            vec![
                "attach",
                "http://127.0.0.1:4242",
                "--dir",
                "/work/project",
                "--session",
                "ses_owned"
            ]
        );
        assert!(!arguments.iter().any(|value| value.contains("private")));
        assert_eq!(command.get_current_dir(), Some(Path::new("/work/project")));
        assert!(command.get_envs().any(|(name, value)| {
            name == "OPENCODE_SERVER_PASSWORD"
                && value == Some(std::ffi::OsStr::new("private-password"))
        }));
    }

    #[test]
    fn procargs2_yields_only_the_arguments_in_proc_cmdline_form() {
        let mut buffer = 2_i32.to_ne_bytes().to_vec();
        buffer.extend_from_slice(b"/bin/opencode\0\0\0\0opencode\0serve\0SECRET=x\0");
        assert_eq!(parse_procargs2(&buffer).unwrap(), b"opencode\0serve\0");
        let mut truncated = 3_i32.to_ne_bytes().to_vec();
        truncated.extend_from_slice(b"/bin/opencode\0opencode\0serve\0");
        assert!(parse_procargs2(&truncated).is_err());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn live_process_identity_is_stable_and_an_exited_process_is_missing() {
        let pid = std::process::id();
        assert_eq!(
            process_start_token(pid).unwrap(),
            process_start_token(pid).unwrap()
        );
        let expected = std::env::args_os()
            .flat_map(|argument| {
                use std::os::unix::ffi::OsStrExt;
                let mut bytes = argument.as_bytes().to_vec();
                bytes.push(0);
                bytes
            })
            .collect::<Vec<_>>();
        assert_eq!(process_cmdline(pid).unwrap(), expected);
        assert_ne!(process_state(pid).unwrap().as_deref(), Some("Z"));

        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let exited = child.id();
        child.wait().unwrap();
        assert_eq!(process_state(exited).unwrap(), None);
        assert!(is_missing_process(
            &process_start_token(exited).unwrap_err()
        ));
        assert!(is_missing_process(&process_cmdline(exited).unwrap_err()));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_bare_recorded_name_matches_the_same_canonical_running_executable() {
        let pid = std::process::id();
        let record = ServerRecord {
            version: RECORD_VERSION,
            pid,
            process_start_token: process_start_token(pid).unwrap(),
            process_cmdline: process_cmdline(pid).unwrap(),
            executable: "opencode".into(),
            port: 4242,
            username: "opencode".into(),
            password: "x".repeat(64),
            created_at_ms: 1,
            sessions: BTreeMap::new(),
        };

        assert!(record_uses_executable(
            &record,
            std::env::current_exe().unwrap().to_str().unwrap()
        ));
        assert!(!record_uses_executable(&record, "/bin/sh"));
    }
}
