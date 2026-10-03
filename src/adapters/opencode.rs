use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::native_owned::{poll_unique, NativeOwnership};
use super::opencode_live::{self, Candidate, Holder, LastMessage};
use super::{DiscoveryRequest, SessionSource, SourceDiscovery};
use crate::control::{
    run_native_authentication, ControlOutcome, LaunchMode, LaunchPresentation, LaunchRequest,
    ProviderController, RestorableSession,
};
use crate::domain::{
    AgentSession, Capability, Provider, Runtime, SessionKind, SessionSnapshot, SessionState,
};
use crate::opencode_supervisor::{ManagedOpenCodeSession, OpenCodeSupervisor};
use crate::process::{CancellableProcessRunner, CommandRequest, CommandRunner};

// `opencode session list` is workspace-scoped in OpenCode 1.18.18 despite its
// generic help text. The official read-only `db` command is the only current
// CLI surface that projects every session across workspaces.
// Ask SQLite to encode each row separately. OpenCode 1.17 truncates a large
// JSON-array result when stdout is a pipe, while TSV rows stream completely.
// json_object also preserves tabs/newlines in user titles and paths safely.
// Subagent sessions (those with a parent) are listed under their parent's
// running turn rather than as rows of their own. `last` is the newest message,
// read through the (session_id, time_created) index, which live state needs;
// `child` is when the newest unfinished turn of a subagent session began.
const GLOBAL_SESSION_ROWS: &str = "SELECT json_object('id', s.id, 'title', s.title, 'created', s.time_created, 'updated', s.time_updated, 'projectId', s.project_id, 'directory', s.directory, 'last', json((SELECT json_object('role', json_extract(m.data, '$.role'), 'created', m.time_created, 'completed', json_extract(m.data, '$.time.completed'), 'question', EXISTS (SELECT 1 FROM part p WHERE p.message_id = m.id AND json_extract(p.data, '$.tool') = 'question' AND json_extract(p.data, '$.state.status') IN ('pending', 'running'))) FROM message m WHERE m.session_id = s.id ORDER BY m.time_created DESC, m.id DESC LIMIT 1)), 'child', (SELECT MAX(m.time_created) FROM session c JOIN message m ON m.id = (SELECT m2.id FROM message m2 WHERE m2.session_id = c.id ORDER BY m2.time_created DESC, m2.id DESC LIMIT 1) WHERE c.parent_id = s.id AND (json_extract(m.data, '$.role') = 'user' OR json_extract(m.data, '$.time.completed') IS NULL))) AS record FROM session s WHERE s.parent_id IS NULL";
const MAX_MODEL_CATALOG_BYTES: usize = 4 * 1024 * 1024;
const LAUNCH_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_RESTORABLE_SESSIONS: usize = 2_000;
/// How far back the restore picker searches message text. Older sessions are
/// still listed and found by name, folder, or ID.
const RESTORE_TRANSCRIPT_WINDOW: Duration = Duration::from_secs(14 * 24 * 60 * 60);
const OPENCODE_READY_MARKER: &str = "Ask anything";

type HolderProbe = Arc<dyn Fn() -> Vec<Holder> + Send + Sync>;

/// OpenCode sessions this dashboard started where no managed server records
/// them (every platform but Linux). The history source lists these even
/// without `--include-external`.
pub struct OpenCodeOwnership {
    inner: NativeOwnership,
}

impl OpenCodeOwnership {
    pub fn load_default() -> Result<Arc<Self>> {
        Self::load(default_opencode_ownership_path()?)
    }

    pub fn load(path: PathBuf) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            inner: NativeOwnership::load(path, "OpenCode")?,
        }))
    }

    fn session_ids(&self) -> BTreeSet<String> {
        self.inner
            .records()
            .into_iter()
            .map(|record| record.session_id)
            .collect()
    }
}

pub fn default_opencode_ownership_path() -> Result<PathBuf> {
    if let Some(state_home) = std::env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(state_home).join("agentview/opencode-owned.json"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/state/agentview/opencode-owned.json"))
}

/// A command prefix for an OpenCode installation on the host or in Docker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenCodeInvocation {
    pub program: String,
    pub prefix_args: Vec<String>,
}

impl OpenCodeInvocation {
    pub fn host(executable: impl Into<String>) -> Self {
        Self {
            program: executable.into(),
            prefix_args: Vec::new(),
        }
    }

    pub fn docker(container: impl Into<String>) -> Self {
        Self {
            program: "docker".into(),
            prefix_args: vec!["exec".into(), container.into(), "opencode".into()],
        }
    }
}

/// Read-only discovery of sessions persisted by OpenCode.
///
/// OpenCode's session-list command intentionally does not report live state.
/// Consequently, this source reports persisted sessions as completed history.
/// A controller that owns an OpenCode server may enrich those records with live
/// state and additional capabilities, but discovery never infers authority.
pub struct OpenCodeSource {
    label: String,
    invocation: OpenCodeInvocation,
    runtime: Runtime,
    runner: Arc<dyn CommandRunner>,
    supervisor: Option<Arc<OpenCodeSupervisor>>,
    discover_external_history: bool,
    probe: HolderProbe,
    ownership: Option<Arc<OpenCodeOwnership>>,
}

/// Read-only history control plus optional exact owned-server lifecycle.
///
/// Managed HTTP authority comes only from `OpenCodeSupervisor`; it is never
/// inferred from the history commands used by `OpenCodeSource`.
pub struct OpenCodeController {
    executable: String,
    source: OpenCodeSource,
    supervisor: Option<Arc<OpenCodeSupervisor>>,
    /// Sessions this controller starts in OpenCode's own interface where no
    /// managed server exists, listed without `--include-external`.
    ownership: Option<Arc<OpenCodeOwnership>>,
}

impl OpenCodeController {
    pub fn host(executable: impl Into<String>) -> Self {
        let executable = executable.into();
        Self {
            source: OpenCodeSource::host(executable.clone()),
            executable,
            supervisor: None,
            ownership: None,
        }
    }

    pub fn managed(executable: impl Into<String>, supervisor: Arc<OpenCodeSupervisor>) -> Self {
        let executable = executable.into();
        Self {
            source: OpenCodeSource::managed(executable.clone(), supervisor.clone()),
            executable,
            supervisor: Some(supervisor),
            ownership: None,
        }
    }

    /// Start new sessions in OpenCode's interface and record them in
    /// `ownership`. Without a managed server this is how the dashboard
    /// launches OpenCode.
    pub fn with_ownership(mut self, ownership: Arc<OpenCodeOwnership>) -> Self {
        self.ownership = Some(ownership);
        self
    }
}

impl ProviderController for OpenCodeController {
    fn provider(&self) -> Provider {
        Provider::OpenCode
    }

    fn launch_mode(&self) -> LaunchMode {
        if self.supervisor.is_some() || self.ownership.is_some() {
            LaunchMode::SelectableModel
        } else {
            LaunchMode::Unavailable
        }
    }

    fn launch_presentation(&self) -> LaunchPresentation {
        if self.supervisor.is_some() {
            LaunchPresentation::DeferredForeground
        } else {
            LaunchPresentation::Foreground
        }
    }

    fn available_models(&self) -> Result<Vec<String>> {
        self.source.available_models()
    }

    fn supports_authentication(&self) -> bool {
        true
    }

    fn authenticate(&self) -> Result<ControlOutcome> {
        run_native_authentication(&self.executable, &["auth", "login"], Provider::OpenCode)
    }

    fn enrich(&self, snapshot: &mut SessionSnapshot) {
        let Some(supervisor) = &self.supervisor else {
            return;
        };
        let managed = match supervisor.list() {
            Ok(managed) => managed,
            Err(error) => {
                snapshot
                    .warnings
                    .push(format!("OpenCode managed control: {error:#}"));
                return;
            }
        };
        let managed: BTreeMap<_, _> = managed
            .into_iter()
            .map(|session| (session.id.clone(), session))
            .collect();
        for session in snapshot.sessions.iter_mut().filter(|session| {
            session.provider == Provider::OpenCode && session.runtime == Runtime::Host
        }) {
            let Some(owned) = managed.get(&session.provider_session_id) else {
                continue;
            };
            overlay_managed(session, owned);
            grant_managed_capabilities(session, owned);
        }
        apply_holds(snapshot.sessions.iter_mut().filter(|session| {
            session.provider == Provider::OpenCode && session.runtime == Runtime::Host
        }));
    }

    fn launch(&self, request: &LaunchRequest) -> Result<ControlOutcome> {
        if request.provider != Provider::OpenCode {
            bail!("the OpenCode controller cannot launch another provider");
        }
        let session = self
            .supervisor
            .as_ref()
            .context("managed OpenCode launch is not configured")?
            .launch_with_model(&request.prompt, &request.cwd, request.model.as_deref())?;
        Ok(ControlOutcome {
            message: format!("started managed OpenCode session {}", session.title),
            provider_session_hint: Some(session.id),
        })
    }

    fn launch_foreground(&self, request: &LaunchRequest) -> Result<ControlOutcome> {
        if self.supervisor.is_some() {
            return self.launch(request);
        }
        self.open_new_session(request)
    }

    fn restorable_sessions(&self) -> Result<Vec<RestorableSession>> {
        if self.ownership.is_none() {
            return Ok(Vec::new());
        }
        self.source.restorable_sessions()
    }

    fn adopt(&self, session: &RestorableSession) -> Result<()> {
        if session.provider != Provider::OpenCode {
            bail!("the OpenCode controller cannot adopt another provider's session");
        }
        self.ownership
            .as_ref()
            .context("bringing OpenCode sessions back is not configured")?
            .inner
            .record(
                &session.provider_session_id,
                &session.cwd,
                &session.name,
                None,
                "OpenCode",
            )
    }

    fn inspect(&self, session: &AgentSession) -> Result<String> {
        if self.owned_session(session)?.is_some() {
            return self
                .supervisor
                .as_ref()
                .context("managed OpenCode control is not configured")?
                .inspect(&session.provider_session_id);
        }
        self.source.inspect(session)
    }

    fn reply(&self, session: &AgentSession, prompt: &str) -> Result<ControlOutcome> {
        let owned = self.require_owned(session)?;
        if owned.state == SessionState::NeedsInput {
            bail!("the managed OpenCode session needs provider-native recovery");
        }
        self.supervisor
            .as_ref()
            .context("managed OpenCode control is not configured")?
            .reply(&owned.id, prompt)?;
        Ok(ControlOutcome {
            message: format!("sent a reply to OpenCode session {}", session.name),
            provider_session_hint: Some(owned.id),
        })
    }

    fn interrupt(&self, session: &AgentSession) -> Result<ControlOutcome> {
        let owned = self.require_owned(session)?;
        if owned.state != SessionState::Working {
            bail!("the managed OpenCode session is not currently working");
        }
        self.supervisor
            .as_ref()
            .context("managed OpenCode control is not configured")?
            .interrupt(&owned.id)?;
        Ok(ControlOutcome {
            message: format!("interrupted OpenCode session {}", session.name),
            provider_session_hint: Some(owned.id),
        })
    }

    fn open(&self, session: &AgentSession) -> Result<ControlOutcome> {
        if session.provider != Provider::OpenCode || session.runtime != Runtime::Host {
            bail!("the host OpenCode controller does not own this runtime");
        }
        if let Some(outcome) = self.open_in_shared_client(session)? {
            return Ok(outcome);
        }
        let command = if self.owned_session(session)?.is_some() {
            self.supervisor
                .as_ref()
                .context("managed OpenCode control is not configured")?
                .native_attach_command(&session.provider_session_id)?
        } else if let Some(supervisor) = self.supervisor.as_ref().filter(|_| session.cwd.is_dir()) {
            supervisor
                .native_attach_command_for_external(&session.provider_session_id, &session.cwd)?
        } else {
            let mut command = Command::new(&self.executable);
            command
                .args(["--session", &session.provider_session_id])
                .current_dir(&session.cwd);
            command
        };
        native_outcome(
            crate::native_session::run(command, &session.id)?,
            &session.provider_session_id,
            &session.name,
        )
    }
}

fn native_outcome(
    exit: crate::native_session::NativeSessionExit,
    session_id: &str,
    name: &str,
) -> Result<ControlOutcome> {
    match exit {
        crate::native_session::NativeSessionExit::Backgrounded => Ok(ControlOutcome {
            message: format!("backgrounded OpenCode session {name}; Enter/Right resumes it"),
            provider_session_hint: Some(session_id.to_owned()),
        }),
        crate::native_session::NativeSessionExit::Exited(status) if status.success() => {
            Ok(ControlOutcome {
                message: format!("returned from OpenCode session {name}"),
                provider_session_hint: Some(session_id.to_owned()),
            })
        }
        crate::native_session::NativeSessionExit::Exited(status) => {
            bail!("OpenCode session exited with status {status}")
        }
    }
}

/// A per-directory TUI that switches between sessions, by its native key.
struct SharedClient {
    /// The server it was started against; a new pid means a restart.
    server_pid: u32,
    /// The dashboard row ID of the session it shows.
    showing: String,
}

fn shared_clients() -> &'static Mutex<BTreeMap<String, SharedClient>> {
    static CLIENTS: OnceLock<Mutex<BTreeMap<String, SharedClient>>> = OnceLock::new();
    CLIENTS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn remember_shared_client(key: &str, server_pid: u32, showing: &str) {
    if let Ok(mut clients) = shared_clients().lock() {
        clients.insert(
            key.to_owned(),
            SharedClient {
                server_pid,
                showing: showing.to_owned(),
            },
        );
    }
}

/// The native key of the shared TUI showing this dashboard row, if any.
fn shared_client_showing(row_id: &str) -> Option<String> {
    shared_clients()
        .lock()
        .ok()?
        .iter()
        .find(|(_, shared)| shared.showing == row_id)
        .map(|(key, _)| key.clone())
}

/// Starts with `opencode:` so the native session layer gives it OpenCode's
/// resume redraw and color answers; `shared` cannot be a runtime ID.
fn shared_client_key(cwd: &Path) -> String {
    format!("opencode:shared:{}", cwd.display())
}

/// The `--client` tag a switch is addressed to. The dashboard's pid keeps two
/// dashboards on one server from switching each other's TUIs.
fn shared_client_id(cwd: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    cwd.hash(&mut hasher);
    format!("agentview-{}-{:016x}", std::process::id(), hasher.finish())
}

impl OpenCodeController {
    /// Show `session` in the one TUI this dashboard keeps per directory,
    /// switching it over from whatever session it showed last instead of
    /// starting a TUI per session (2-3 s and about 500 MB each). `None`
    /// leaves the open to the per-session path: no managed server, a server
    /// that cannot switch one TUI alone, or a TUI already parked for this
    /// exact session.
    fn open_in_shared_client(&self, session: &AgentSession) -> Result<Option<ControlOutcome>> {
        let Some(supervisor) = self.supervisor.as_ref() else {
            return Ok(None);
        };
        if crate::native_session::is_backgrounded(&session.id) {
            return Ok(None);
        }
        let cwd = match self.owned_session(session)? {
            Some(owned) => owned.cwd,
            None => session.cwd.clone(),
        };
        if !cwd.is_dir() {
            return Ok(None);
        }
        let key = shared_client_key(&cwd);
        let client = shared_client_id(&cwd);
        let started_against = shared_clients()
            .lock()
            .ok()
            .and_then(|clients| clients.get(&key).map(|shared| shared.server_pid));
        let parked = crate::native_session::is_backgrounded(&key);
        // A TUI from before a server restart still runs the old binary and
        // plugins; replace it so a restart picks up OpenCode changes.
        let reuse = parked
            && started_against.is_some()
            && started_against == supervisor.live_server_pid()?;
        if parked && !reuse {
            let _ = crate::native_session::terminate(&key);
        }
        let exit = if reuse {
            supervisor.select_in_shared_client(&session.provider_session_id, &cwd, &client)?;
            remember_shared_client(&key, started_against.unwrap_or_default(), &session.id);
            crate::native_session::resume(&key)?
        } else {
            let Some((command, server_pid)) =
                supervisor.shared_client_command(&session.provider_session_id, &cwd, &client)?
            else {
                return Ok(None);
            };
            remember_shared_client(&key, server_pid, &session.id);
            crate::native_session::run(command, &key)?
        };
        if !matches!(exit, crate::native_session::NativeSessionExit::Backgrounded) {
            if let Ok(mut clients) = shared_clients().lock() {
                clients.remove(&key);
            }
        }
        native_outcome(exit, &session.provider_session_id, &session.name).map(Some)
    }

    fn owned_session(&self, session: &AgentSession) -> Result<Option<ManagedOpenCodeSession>> {
        if session.provider != Provider::OpenCode || session.runtime != Runtime::Host {
            bail!("the host OpenCode controller does not own this runtime");
        }
        let Some(supervisor) = &self.supervisor else {
            return Ok(None);
        };
        Ok(supervisor
            .list()?
            .into_iter()
            .find(|owned| owned.id == session.provider_session_id))
    }

    fn require_owned(&self, session: &AgentSession) -> Result<ManagedOpenCodeSession> {
        self.owned_session(session)?
            .context("refusing to control an OpenCode session not created by this supervisor")
    }

    /// Open OpenCode's interface with the task already submitted, then record
    /// the session it created. OpenCode chooses session IDs itself, so the new
    /// one is found afterwards as the only root session created in the
    /// requested directory since the launch.
    fn open_new_session(&self, request: &LaunchRequest) -> Result<ControlOutcome> {
        if request.provider != Provider::OpenCode {
            bail!("the OpenCode controller cannot launch another provider");
        }
        let ownership = self
            .ownership
            .as_ref()
            .context("OpenCode launch is not configured")?;
        let prompt = request.prompt.trim();
        if prompt.is_empty() {
            bail!("the OpenCode launch prompt cannot be empty");
        }
        if !request.cwd.is_absolute() {
            bail!("the OpenCode workspace must be absolute");
        }
        let mut command = Command::new(&self.executable);
        command.current_dir(&request.cwd);
        if let Some(model) = request.model.as_deref() {
            validate_model(model)?;
            command.arg(format!("--model={model}"));
        }
        // `--prompt` only prefills OpenCode's editor without submitting it, so
        // the task is pasted and entered once the empty editor is on screen.
        // Bracketed paste keeps multiline and slash-prefixed tasks as text.
        let initial_input = format!("\x1b[200~{prompt}\x1b[201~\r").into_bytes();
        let launched_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let launch_key = format!(
            "opencode:host:launch-{}",
            crate::native_session::new_session_id()?
        );
        let exit = crate::native_session::run_with_initial_input_after_screen(
            command,
            &launch_key,
            &initial_input,
            OPENCODE_READY_MARKER,
        )?;
        let session_id = poll_unique(
            "one new OpenCode session in the requested workspace",
            LAUNCH_DISCOVERY_TIMEOUT,
            || {
                self.source
                    .sessions_created_since(&request.cwd, launched_ms)
            },
        )?;
        ownership
            .inner
            .record(&session_id, &request.cwd, prompt, None, "OpenCode")?;
        if matches!(exit, crate::native_session::NativeSessionExit::Backgrounded) {
            crate::native_session::rename_key(&launch_key, &format!("opencode:host:{session_id}"))?;
        }
        native_outcome(exit, &session_id, &session_id)
    }
}

fn validate_model(model: &str) -> Result<()> {
    let valid = model
        .split_once('/')
        .is_some_and(|(provider, name)| !provider.is_empty() && !name.trim().is_empty())
        && model.len() <= 256
        && !model
            .chars()
            .any(|character| character.is_control() || character.is_whitespace());
    if !valid {
        bail!("OpenCode models are provider/model identifiers without spaces");
    }
    Ok(())
}

impl OpenCodeSource {
    pub fn host(executable: impl Into<String>) -> Self {
        Self {
            label: "OpenCode (host)".into(),
            invocation: OpenCodeInvocation::host(executable),
            runtime: Runtime::Host,
            runner: Arc::new(CancellableProcessRunner::default()),
            supervisor: None,
            discover_external_history: true,
            probe: Arc::new(opencode_live::probe_host_holders),
            ownership: None,
        }
    }

    pub fn managed(executable: impl Into<String>, supervisor: Arc<OpenCodeSupervisor>) -> Self {
        Self {
            label: "OpenCode (host)".into(),
            invocation: OpenCodeInvocation::host(executable),
            runtime: Runtime::Host,
            runner: Arc::new(CancellableProcessRunner::default()),
            supervisor: Some(supervisor),
            discover_external_history: true,
            probe: Arc::new(opencode_live::probe_host_holders),
            ownership: None,
        }
    }

    /// Discover only sessions recorded by the exact agentview-owned server.
    pub fn managed_owned(
        executable: impl Into<String>,
        supervisor: Arc<OpenCodeSupervisor>,
    ) -> Self {
        Self {
            label: "OpenCode (managed host)".into(),
            invocation: OpenCodeInvocation::host(executable),
            runtime: Runtime::Host,
            runner: Arc::new(CancellableProcessRunner::default()),
            supervisor: Some(supervisor),
            discover_external_history: false,
            probe: Arc::new(opencode_live::probe_host_holders),
            ownership: None,
        }
    }

    pub fn docker(
        container_name: impl Into<String>,
        container_id: impl Into<String>,
        image: impl Into<String>,
    ) -> Self {
        let container_name = container_name.into();
        let container_id = container_id.into();
        Self {
            label: format!("OpenCode ({container_name})"),
            invocation: OpenCodeInvocation::docker(container_id.clone()),
            runtime: Runtime::Docker {
                container_id,
                container_name,
                image: image.into(),
            },
            runner: Arc::new(CancellableProcessRunner::default()),
            supervisor: None,
            discover_external_history: true,
            probe: Arc::new(Vec::new),
            ownership: None,
        }
    }

    /// Also list the sessions `ownership` records when external history is
    /// not requested.
    pub fn owned(mut self, ownership: Arc<OpenCodeOwnership>) -> Self {
        self.ownership = Some(ownership);
        self
    }

    fn available_models(&self) -> Result<Vec<String>> {
        let mut args = self.invocation.prefix_args.clone();
        args.push("models".into());
        let mut command = CommandRequest::new(self.invocation.program.clone(), args);
        command.timeout = Duration::from_secs(8);
        let output = self.runner.run(&command)?;
        if output.status != 0 {
            bail!(
                "OpenCode model discovery exited with status {}: {}",
                output.status,
                output.stderr_lossy()
            );
        }
        if output.stdout.len() > MAX_MODEL_CATALOG_BYTES {
            bail!("OpenCode model catalog exceeded the 4 MiB safety limit");
        }
        parse_opencode_models(output.stdout_text()?)
    }

    /// Render a persisted session transcript using OpenCode's read-only export
    /// command. This does not attach to, steer, or otherwise mutate a session.
    pub fn inspect(&self, session: &AgentSession) -> Result<String> {
        if session.provider != Provider::OpenCode || session.runtime != self.runtime {
            bail!("the OpenCode source does not own this provider runtime");
        }
        let mut args = self.invocation.prefix_args.clone();
        args.extend(["export".into(), session.provider_session_id.clone()]);
        let mut command = CommandRequest::new(self.invocation.program.clone(), args);
        command.timeout = Duration::from_secs(8);
        let output = self.runner.run(&command)?;
        if output.status != 0 {
            bail!(
                "opencode export exited with status {}: {}",
                output.status,
                output.stderr_lossy()
            );
        }
        render_opencode_export(output.stdout_text()?)
    }

    #[cfg(test)]
    fn with_runner(
        label: impl Into<String>,
        invocation: OpenCodeInvocation,
        runtime: Runtime,
        runner: Arc<dyn CommandRunner>,
    ) -> Self {
        Self {
            label: label.into(),
            invocation,
            runtime,
            runner,
            supervisor: None,
            discover_external_history: true,
            probe: Arc::new(Vec::new),
            ownership: None,
        }
    }

    #[cfg(test)]
    fn with_probe(mut self, probe: impl Fn() -> Vec<Holder> + Send + Sync + 'static) -> Self {
        self.probe = Arc::new(probe);
        self
    }
}

impl SessionSource for OpenCodeSource {
    fn label(&self) -> &str {
        &self.label
    }

    fn discover(&self, request: &DiscoveryRequest) -> Result<Vec<AgentSession>> {
        Ok(self.discover_with_warnings(request)?.sessions)
    }

    fn discover_with_warnings(&self, request: &DiscoveryRequest) -> Result<SourceDiscovery> {
        let mut sessions = BTreeMap::new();
        let mut warnings = Vec::new();
        let external = self.discover_external_history && request.include_external;
        // Sessions the dashboard started or brought back are always listed,
        // and are loaded even when they fall outside the history window.
        let owned = self
            .ownership
            .as_ref()
            .map(|ownership| ownership.session_ids())
            .unwrap_or_default();
        if external || !owned.is_empty() {
            let holders = if self.runtime == Runtime::Host {
                (self.probe)()
            } else {
                Vec::new()
            };
            // Persisted history that no live process holds is completed.
            // Avoid starting the potentially enormous global database query
            // when completed sessions are hidden and nothing runs OpenCode.
            if request.include_completed || !holders.is_empty() {
                let history_limit = request.history_limit.max(1);
                let scope = if external {
                    Scope::Recent {
                        limit: history_limit.saturating_add(1),
                        oldest_first: request.history_oldest_first,
                        pinned: opencode_live::named_sessions(&holders)
                            .into_iter()
                            .chain(owned.iter().cloned())
                            .collect(),
                    }
                } else {
                    Scope::Only(owned.clone())
                };
                let mut history = self.normalize_with_live_state(self.query(&scope)?, &holders);
                if request.history_oldest_first {
                    history.sort_by_key(|session| session.updated_at);
                } else {
                    history.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
                }
                let mut completed = 0usize;
                let mut truncated = false;
                for session in history {
                    if session.state == SessionState::Completed {
                        if !request.include_completed {
                            continue;
                        }
                        if owned.contains(&session.provider_session_id) {
                            // Listed on purpose; not part of the history window.
                        } else if completed >= history_limit {
                            truncated = true;
                            continue;
                        }
                        completed += 1;
                    }
                    if request
                        .cwd
                        .as_ref()
                        .map(|cwd| session.cwd.starts_with(cwd))
                        .unwrap_or(true)
                    {
                        sessions.insert(session.provider_session_id.clone(), session);
                    }
                }
                if truncated {
                    warnings.push(format!(
                        "OpenCode history is limited to {} records for this refresh; increase --history-limit to load more",
                        history_limit
                    ));
                }
            }
        }
        if let Some(supervisor) = &self.supervisor {
            for managed in supervisor.list()? {
                let session = agent_session_from_managed(&managed);
                if (request.include_completed || session.state != SessionState::Completed)
                    && request
                        .cwd
                        .as_ref()
                        .map(|cwd| session.cwd.starts_with(cwd))
                        .unwrap_or(true)
                {
                    sessions.insert(session.provider_session_id.clone(), session);
                }
            }
        }
        if self.runtime == Runtime::Host {
            apply_holds(sessions.values_mut());
        }
        Ok(SourceDiscovery {
            sessions: sessions.into_values().collect(),
            warnings,
        })
    }

    fn cancel(&self) {
        self.runner.cancel();
    }
}

/// Which persisted sessions one discovery reads.
enum Scope {
    /// The most recently updated sessions, plus sessions a live process names
    /// on its command line even when they fall outside that window.
    Recent {
        limit: usize,
        oldest_first: bool,
        pinned: BTreeSet<String>,
    },
    /// Exactly these sessions.
    Only(BTreeSet<String>),
}

impl OpenCodeSource {
    fn query(&self, scope: &Scope) -> Result<Vec<OpenCodeRecord>> {
        let mut args = self.invocation.prefix_args.clone();
        args.extend([
            "db".into(),
            session_query(scope),
            "--format".into(),
            "tsv".into(),
        ]);
        let mut command = CommandRequest::new(self.invocation.program.clone(), args);
        command.timeout = Duration::from_secs(8);
        let output = self.runner.run(&command)?;
        if output.status == 0 {
            return parse_opencode_db_records(output.stdout_text()?);
        }
        // Older OpenCode builds do not have `db`; retain their supported,
        // though potentially workspace-scoped, session-list behavior.
        let mut args = self.invocation.prefix_args.clone();
        args.extend([
            "session".into(),
            "list".into(),
            "--format".into(),
            "json".into(),
        ]);
        let mut fallback = CommandRequest::new(self.invocation.program.clone(), args);
        fallback.timeout = Duration::from_secs(8);
        let output = self.runner.run(&fallback)?;
        if output.status != 0 {
            bail!(
                "OpenCode global discovery and session-list fallback failed with status {}: {}",
                output.status,
                output.stderr_lossy()
            );
        }
        let mut records = parse_opencode_session_records(output.stdout_text()?)?;
        if let Scope::Only(ids) = scope {
            records.retain(|record| ids.contains(&record.id));
        }
        Ok(records)
    }

    fn normalize_with_live_state(
        &self,
        records: Vec<OpenCodeRecord>,
        holders: &[Holder],
    ) -> Vec<AgentSession> {
        let assigned = if holders.is_empty() {
            BTreeMap::new()
        } else {
            let now = opencode_live::now_ms();
            let candidates = records
                .iter()
                .map(|record| Candidate {
                    id: &record.id,
                    directory: &record.directory,
                    created_ms: record.created,
                    updated_ms: record.updated,
                    active: opencode_live::is_active(
                        record.last.as_ref(),
                        record.child,
                        record.updated,
                        now,
                    ),
                })
                .collect::<Vec<_>>();
            opencode_live::assign(holders, &candidates)
        };
        records
            .into_iter()
            .map(|mut record| {
                let last = record.last.take();
                let child = record.child.take();
                let since = opencode_live::earliest_start(holders, &record.id, &record.directory);
                let mut session = normalize_record(record, self.runtime.clone());
                if self.runtime == Runtime::Host {
                    let holder = assigned.get(&session.provider_session_id);
                    let since = since
                        .or(holder.map(|holder| holder.started_ms))
                        .unwrap_or(0);
                    apply_live_state(&mut session, last.as_ref(), child, holder, since);
                }
                session
            })
            .collect()
    }
}

impl OpenCodeSource {
    /// The newest root sessions in OpenCode's whole history, for the restore
    /// picker, minus sessions run in a temp directory. Sessions updated in the
    /// last `RESTORE_TRANSCRIPT_WINDOW` also carry their message text so the
    /// picker can search it.
    fn restorable_sessions(&self) -> Result<Vec<RestorableSession>> {
        let now_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as u64)
            .unwrap_or_default();
        self.restorable_sessions_at(now_ms)
    }

    fn restorable_sessions_at(&self, now_ms: u64) -> Result<Vec<RestorableSession>> {
        #[derive(Deserialize)]
        struct Row {
            id: String,
            title: String,
            directory: PathBuf,
            updated: u64,
        }
        let since_ms = now_ms.saturating_sub(RESTORE_TRANSCRIPT_WINDOW.as_millis() as u64);
        let transcripts_command = self.db_command(
            format!(
                "SELECT json_object('id', root, 'text', group_concat(text, char(10))) AS record FROM (SELECT COALESCE(s.parent_id, s.id) AS root, json_extract(p.data, '$.text') AS text FROM part p JOIN session s ON s.id = p.session_id WHERE p.time_created >= {since_ms} AND json_extract(p.data, '$.type') = 'text' AND json_extract(p.data, '$.synthetic') IS NOT 1 ORDER BY p.time_created) GROUP BY root"
            ),
            Duration::from_secs(20),
        );
        let command = self.db_command(
            format!(
                "SELECT json_object('id', id, 'title', title, 'directory', directory, 'updated', time_updated) AS record FROM session WHERE parent_id IS NULL ORDER BY time_updated DESC LIMIT {MAX_RESTORABLE_SESSIONS}"
            ),
            Duration::from_secs(8),
        );
        // The text query is the slow one (a scan of every part), so it runs
        // beside the metadata query instead of after it. Text is best effort:
        // without it the picker still lists and searches names.
        let runner = self.runner.clone();
        let (output, transcripts) = std::thread::scope(|scope| {
            let transcripts =
                scope.spawn(move || transcripts_from(runner.run(&transcripts_command)));
            let output = self.runner.run(&command);
            (output, transcripts.join().unwrap_or_default())
        });
        let mut transcripts = transcripts;
        let output = output?;
        if output.status != 0 {
            bail!(
                "OpenCode history lookup exited with status {}: {}",
                output.status,
                output.stderr_lossy()
            );
        }
        let runtime_id = match &self.runtime {
            Runtime::Host => "host",
            Runtime::Docker { container_id, .. } => container_id,
        };
        Ok(output
            .stdout_text()?
            .lines()
            .skip(1)
            .filter_map(|line| serde_json::from_str::<Row>(line).ok())
            .filter(|row| !is_throwaway_directory(&row.directory))
            .map(|row| RestorableSession {
                id: format!("opencode:{runtime_id}:{}", row.id),
                transcript: transcripts.remove(&row.id).unwrap_or_default(),
                provider_session_id: row.id,
                provider: Provider::OpenCode,
                name: row.title,
                cwd: row.directory,
                updated_at_ms: row.updated,
            })
            .collect())
    }

    fn db_command(&self, query: String, timeout: Duration) -> CommandRequest {
        let mut args = self.invocation.prefix_args.clone();
        args.extend(["db".into(), query, "--format".into(), "tsv".into()]);
        let mut command = CommandRequest::new(self.invocation.program.clone(), args);
        command.timeout = timeout;
        command
    }

    /// Root sessions created in `cwd` at or after `since_ms`.
    fn sessions_created_since(&self, cwd: &Path, since_ms: u64) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct Created {
            id: String,
            directory: PathBuf,
        }
        let mut args = self.invocation.prefix_args.clone();
        args.extend([
            "db".into(),
            format!(
                "SELECT json_object('id', id, 'directory', directory) AS record FROM session WHERE parent_id IS NULL AND time_created >= {}",
                since_ms.saturating_sub(2_000)
            ),
            "--format".into(),
            "tsv".into(),
        ]);
        let mut command = CommandRequest::new(self.invocation.program.clone(), args);
        command.timeout = Duration::from_secs(8);
        let output = self.runner.run(&command)?;
        if output.status != 0 {
            bail!(
                "OpenCode session lookup exited with status {}: {}",
                output.status,
                output.stderr_lossy()
            );
        }
        let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_owned());
        Ok(output
            .stdout_text()?
            .lines()
            .skip(1)
            .filter_map(|line| serde_json::from_str::<Created>(line).ok())
            .filter(|created| {
                std::fs::canonicalize(&created.directory)
                    .unwrap_or_else(|_| created.directory.clone())
                    == cwd
            })
            .map(|created| created.id)
            .collect())
    }
}

/// Replace the persisted-history state of a session that a live OpenCode
/// process runs.
fn apply_live_state(
    session: &mut AgentSession,
    last: Option<&LastMessage>,
    child: Option<u64>,
    holder: Option<&Holder>,
    since: u64,
) {
    let background = crate::native_session::background_screen_contents(&session.id).or_else(|| {
        shared_client_showing(&session.id)
            .and_then(|key| crate::native_session::background_screen_contents(&key))
    });
    let Some(pid) = background
        .as_ref()
        .map(|(pid, _)| *pid)
        .or(holder.map(|holder| holder.pid))
    else {
        return;
    };
    let (state, raw_state) = opencode_live::combine(
        background
            .as_ref()
            .and_then(|(_, screen)| opencode_live::screen_state(screen)),
        opencode_live::held_state(last, child, since),
    );
    session.state = state;
    session.raw_state = Some(raw_state.into());
    session.pid = Some(pid);
}

/// The state the TUI this dashboard holds for `session` shows now, for the
/// dashboard to apply between discoveries.
pub(super) fn background_screen_state(
    session: &AgentSession,
) -> Option<(SessionState, &'static str)> {
    let (_, screen) =
        crate::native_session::background_screen_contents(&session.id).or_else(|| {
            shared_client_showing(&session.id)
                .and_then(|key| crate::native_session::background_screen_contents(&key))
        })?;
    opencode_live::settle_from_screen(opencode_live::screen_state(&screen)?, session.state)
}

fn apply_holds<'a>(sessions: impl Iterator<Item = &'a mut AgentSession>) {
    let Some(root) = crate::holds::default_holds_dir() else {
        return;
    };
    for session in sessions {
        if let Some(reason) =
            crate::holds::live_hold(&root, "opencode", &session.provider_session_id)
        {
            apply_hold(session, &reason);
        }
    }
}

/// A plugin's background work, such as a CI monitor that will start the next
/// turn, keeps an idle or closed session working. A question or permission
/// prompt still needs the user first.
fn apply_hold(session: &mut AgentSession, reason: &str) {
    if session.state == SessionState::NeedsInput
        && session.raw_state.as_deref() != Some("waiting at prompt")
    {
        return;
    }
    session.state = SessionState::Working;
    session.raw_state = Some(if reason.is_empty() {
        "background work".into()
    } else {
        format!("background: {reason}")
    });
}

fn session_query(scope: &Scope) -> String {
    match scope {
        Scope::Recent {
            limit,
            oldest_first,
            pinned,
        } => {
            let recent = global_session_query(*limit, *oldest_first);
            match sql_id_list(pinned) {
                None => recent,
                Some(ids) => format!(
                    "SELECT record FROM ({recent}) UNION ALL SELECT record FROM ({GLOBAL_SESSION_ROWS} AND s.id IN ({ids}))"
                ),
            }
        }
        Scope::Only(ids) => format!(
            "{GLOBAL_SESSION_ROWS} AND s.id IN ({})",
            sql_id_list(ids).unwrap_or_else(|| "NULL".into())
        ),
    }
}

fn global_session_query(limit: usize, oldest_first: bool) -> String {
    format!(
        "{GLOBAL_SESSION_ROWS} ORDER BY s.time_updated {} LIMIT {}",
        if oldest_first { "ASC" } else { "DESC" },
        limit.max(1)
    )
}

/// A quoted SQL list of the IDs that are plain OpenCode identifiers. Anything
/// else comes from an arbitrary command line or file and is left out.
fn sql_id_list(ids: &BTreeSet<String>) -> Option<String> {
    let quoted = ids
        .iter()
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 128
                && id.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
                })
        })
        .map(|id| format!("'{id}'"))
        .collect::<Vec<_>>();
    (!quoted.is_empty()).then(|| quoted.join(", "))
}

fn parse_opencode_db_records(input: &str) -> Result<Vec<OpenCodeRecord>> {
    let mut lines = input.lines();
    let Some(header) = lines.next() else {
        return Ok(Vec::new());
    };
    if header.trim_end_matches('\r') != "record" {
        bail!("invalid OpenCode db TSV header");
    }
    lines
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line)
                .with_context(|| format!("invalid OpenCode db record on row {}", index + 2))
        })
        .collect()
}

fn agent_session_from_managed(managed: &ManagedOpenCodeSession) -> AgentSession {
    AgentSession {
        id: format!("opencode:host:{}", managed.id),
        provider_session_id: managed.id.clone(),
        provider: Provider::OpenCode,
        runtime: Runtime::Host,
        kind: SessionKind::Managed,
        name: managed.title.clone(),
        cwd: managed.cwd.clone(),
        state: managed.state,
        summary: managed.summary.clone(),
        raw_state: Some("managed_server".into()),
        pid: Some(managed.server_pid),
        started_at: Some(SystemTime::UNIX_EPOCH + Duration::from_millis(managed.created_at_ms)),
        updated_at: Some(SystemTime::UNIX_EPOCH + Duration::from_millis(managed.updated_at_ms)),
        pull_requests: None,
        capabilities: BTreeSet::from([Capability::Inspect]),
    }
}

fn overlay_managed(session: &mut AgentSession, managed: &ManagedOpenCodeSession) {
    session.kind = SessionKind::Managed;
    session.name = managed.title.clone();
    session.cwd = managed.cwd.clone();
    session.state = managed.state;
    session.summary = managed.summary.clone();
    session.raw_state = Some("managed_server".into());
    session.pid = Some(managed.server_pid);
    session.started_at =
        Some(SystemTime::UNIX_EPOCH + Duration::from_millis(managed.created_at_ms));
    session.updated_at =
        Some(SystemTime::UNIX_EPOCH + Duration::from_millis(managed.updated_at_ms));
}

fn grant_managed_capabilities(session: &mut AgentSession, managed: &ManagedOpenCodeSession) {
    session.capabilities.clear();
    session.capabilities.insert(Capability::Inspect);
    match managed.state {
        SessionState::Working => {
            session.capabilities.insert(Capability::Reply);
            session.capabilities.insert(Capability::Interrupt);
        }
        SessionState::Completed | SessionState::ReadyForReview => {
            session.capabilities.insert(Capability::Reply);
        }
        SessionState::NeedsInput | SessionState::Unknown => {}
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenCodeRecord {
    id: String,
    title: String,
    updated: u64,
    created: u64,
    #[allow(dead_code)]
    project_id: String,
    directory: PathBuf,
    #[serde(default)]
    last: Option<LastMessage>,
    #[serde(default)]
    child: Option<u64>,
}

pub fn parse_opencode_session_list(input: &str, runtime: Runtime) -> Result<Vec<AgentSession>> {
    Ok(parse_opencode_session_records(input)?
        .into_iter()
        .map(|record| normalize_record(record, runtime.clone()))
        .collect())
}

fn parse_opencode_session_records(input: &str) -> Result<Vec<OpenCodeRecord>> {
    // OpenCode 1.18 emits no bytes, rather than `[]`, when its store is empty.
    if input.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(input).context("invalid OpenCode session-list JSON")
}

fn render_opencode_export(input: &str) -> Result<String> {
    let value: serde_json::Value =
        serde_json::from_str(input).context("invalid `opencode export` output")?;
    let messages = value
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .context("opencode export omitted messages")?;
    let mut transcript = Vec::new();
    for message in messages {
        let role = message
            .pointer("/info/role")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("event");
        let text = message
            .get("parts")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|part| {
                (part.get("type").and_then(serde_json::Value::as_str) == Some("text"))
                    .then(|| part.get("text").and_then(serde_json::Value::as_str))
                    .flatten()
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !text.trim().is_empty() {
            transcript.push(format!("{}: {}", capitalize(role), text.trim()));
        }
    }
    Ok(limit_transcript(if transcript.is_empty() {
        "No text messages are available in this OpenCode session.".into()
    } else {
        transcript.join("\n\n")
    }))
}

fn capitalize(value: &str) -> String {
    let mut characters = value.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

fn limit_transcript(mut value: String) -> String {
    const MAX_CHARS: usize = 32 * 1024;
    if value.chars().count() <= MAX_CHARS {
        return value;
    }
    value = value
        .chars()
        .rev()
        .take(MAX_CHARS.saturating_sub(24))
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("[earlier output omitted]\n{value}")
}

fn parse_opencode_models(input: &str) -> Result<Vec<String>> {
    let mut models = BTreeSet::new();
    for (index, line) in input.lines().enumerate() {
        let identifier = line.trim();
        if identifier.is_empty() {
            continue;
        }
        let Some((provider, model)) = identifier.split_once('/') else {
            bail!("OpenCode model catalog row {} is malformed", index + 1);
        };
        if provider.is_empty()
            || model.is_empty()
            || identifier.len() > 128
            || identifier
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
        {
            bail!("OpenCode model catalog row {} is invalid", index + 1);
        }
        models.insert(identifier.to_owned());
        if models.len() > 20_000 {
            bail!("OpenCode model catalog exceeded the 20,000-model safety limit");
        }
    }
    Ok(models.into_iter().collect())
}

fn normalize_record(record: OpenCodeRecord, runtime: Runtime) -> AgentSession {
    let runtime_id = match &runtime {
        Runtime::Host => "host",
        Runtime::Docker { container_id, .. } => container_id,
    };
    let capabilities = if runtime == Runtime::Host {
        BTreeSet::from([Capability::Inspect])
    } else {
        // A host controller cannot safely route inspection into an arbitrary
        // container. Explicit Docker control needs its own enrolled controller.
        BTreeSet::new()
    };
    AgentSession {
        id: format!("opencode:{runtime_id}:{}", record.id),
        provider_session_id: record.id,
        provider: Provider::OpenCode,
        runtime,
        kind: SessionKind::Unknown,
        name: record.title.clone(),
        cwd: record.directory,
        // The list command is a history API and exposes no live status.
        state: SessionState::Completed,
        summary: record.title,
        raw_state: Some("persisted".into()),
        pid: None,
        started_at: Some(SystemTime::UNIX_EPOCH + Duration::from_millis(record.created)),
        updated_at: Some(SystemTime::UNIX_EPOCH + Duration::from_millis(record.updated)),
        pull_requests: None,
        capabilities,
    }
}

/// Root session ID to its message text, from the restore transcript query.
fn transcripts_from(output: Result<crate::process::CommandOutput>) -> BTreeMap<String, String> {
    #[derive(Deserialize)]
    struct Transcript {
        id: String,
        text: Option<String>,
    }
    let Ok(output) = output else {
        return BTreeMap::new();
    };
    if output.status != 0 {
        return BTreeMap::new();
    }
    output
        .stdout_text()
        .unwrap_or_default()
        .lines()
        .skip(1)
        .filter_map(|line| serde_json::from_str::<Transcript>(line).ok())
        .filter_map(|row| Some((row.id, row.text?)))
        .collect()
}

/// Sessions started in a temp directory are scratch work (scripts, probes,
/// test repos) that nobody restores.
fn is_throwaway_directory(directory: &Path) -> bool {
    let temp = std::env::temp_dir();
    let temp = temp.canonicalize().unwrap_or(temp);
    directory.starts_with(temp)
        || [
            "/tmp",
            "/private/tmp",
            "/var/tmp",
            "/private/var/folders",
            "/var/folders",
        ]
        .iter()
        .any(|root| directory.starts_with(root))
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    use super::*;
    use crate::process::CommandOutput;

    struct FakeRunner {
        expected: CommandRequest,
        output: Mutex<Option<CommandOutput>>,
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, request: &CommandRequest) -> Result<CommandOutput> {
            assert_eq!(request, &self.expected);
            Ok(self.output.lock().unwrap().take().unwrap())
        }
    }

    #[test]
    fn a_background_hold_keeps_a_closed_or_idle_session_working_but_not_a_prompt() {
        let input = r#"[{"id": "ses_1", "title": "t", "updated": 2, "created": 1,
          "projectId": "global", "directory": "/work"}]"#;
        let mut session = parse_opencode_session_list(input, Runtime::Host)
            .unwrap()
            .remove(0);
        assert_eq!(session.state, SessionState::Completed);
        apply_hold(&mut session, "CI on PR 8");
        assert_eq!(session.state, SessionState::Working);
        assert_eq!(session.raw_state.as_deref(), Some("background: CI on PR 8"));

        session.state = SessionState::NeedsInput;
        session.raw_state = Some("waiting at prompt".into());
        apply_hold(&mut session, "");
        assert_eq!(session.state, SessionState::Working);
        assert_eq!(session.raw_state.as_deref(), Some("background work"));

        session.state = SessionState::NeedsInput;
        session.raw_state = Some("permission requested".into());
        apply_hold(&mut session, "CI on PR 8");
        assert_eq!(session.state, SessionState::NeedsInput);
    }

    #[test]
    fn parses_current_opencode_json_shape() {
        let input = r#"[{
          "id": "ses_123",
          "title": "Implement the dashboard",
          "updated": 1787089210008,
          "created": 1787089195916,
          "projectId": "global",
          "directory": "/work/project"
        }]"#;

        let sessions = parse_opencode_session_list(input, Runtime::Host).unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].provider, Provider::OpenCode);
        assert_eq!(sessions[0].provider_session_id, "ses_123");
        assert_eq!(sessions[0].state, SessionState::Completed);
        assert_eq!(sessions[0].cwd, PathBuf::from("/work/project"));
        assert_eq!(
            sessions[0].capabilities,
            BTreeSet::from([Capability::Inspect])
        );
    }

    #[test]
    fn parses_exact_opencode_model_identifiers() {
        assert_eq!(
            parse_opencode_models("openai/gpt-5.4\nanthropic/claude-sonnet-4-5\nopenai/gpt-5.4\n")
                .unwrap(),
            vec!["anthropic/claude-sonnet-4-5", "openai/gpt-5.4"]
        );
        assert!(parse_opencode_models("gpt-5.4\n").is_err());
        assert!(parse_opencode_models("openai/\n").is_err());
    }

    #[test]
    fn controller_uses_the_documented_opencode_models_command() {
        let mut expected = CommandRequest::new("opencode", vec!["models".into()]);
        expected.timeout = Duration::from_secs(8);
        let runner = Arc::new(FakeRunner {
            expected,
            output: Mutex::new(Some(CommandOutput {
                status: 0,
                stdout: b"openai/gpt-5.4\nanthropic/claude-sonnet-4-5\n".to_vec(),
                stderr: Vec::new(),
            })),
        });
        let source = OpenCodeSource::with_runner(
            "test",
            OpenCodeInvocation::host("opencode"),
            Runtime::Host,
            runner,
        );
        let controller = OpenCodeController {
            executable: "opencode".into(),
            source,
            supervisor: None,
            ownership: None,
        };

        assert_eq!(
            controller.available_models().unwrap(),
            vec!["anthropic/claude-sonnet-4-5", "openai/gpt-5.4"]
        );
    }

    #[test]
    fn accepts_the_empty_store_output_from_opencode() {
        assert!(parse_opencode_session_list("\n", Runtime::Host)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn docker_history_does_not_claim_host_inspection_authority() {
        let runtime = Runtime::Docker {
            container_id: "sha256:exact".into(),
            container_name: "isolated".into(),
            image: "opencode@test".into(),
        };
        let session = parse_opencode_session_list(
            r#"[{"id":"ses_1","title":"one","updated":2,"created":1,"projectId":"global","directory":"/work"}]"#,
            runtime,
        )
        .unwrap()
        .remove(0);

        assert!(session.capabilities.is_empty());
    }

    #[test]
    fn source_uses_bounded_streaming_db_rows_and_filters_cwd() {
        let mut expected = CommandRequest::new(
            "opencode",
            vec![
                "db".into(),
                global_session_query(101, false),
                "--format".into(),
                "tsv".into(),
            ],
        );
        expected.timeout = Duration::from_secs(8);
        let runner = Arc::new(FakeRunner {
            expected,
            output: Mutex::new(Some(CommandOutput {
                status: 0,
                stdout: b"record\n{\"id\":\"ses_1\",\"title\":\"one\",\"updated\":2,\"created\":1,\"projectId\":\"global\",\"directory\":\"/work/one\"}\n{\"id\":\"ses_2\",\"title\":\"two\",\"updated\":2,\"created\":1,\"projectId\":\"global\",\"directory\":\"/else\"}\n".to_vec(),
                stderr: vec![],
            })),
        });
        let source = OpenCodeSource::with_runner(
            "test",
            OpenCodeInvocation::host("opencode"),
            Runtime::Host,
            runner,
        );

        let sessions = source
            .discover(&DiscoveryRequest {
                include_completed: true,
                include_interactive: false,
                include_external: true,
                cwd: Some(PathBuf::from("/work")),
                ..DiscoveryRequest::default()
            })
            .unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].provider_session_id, "ses_1");
    }

    #[test]
    fn source_returns_a_nonfatal_warning_when_more_history_exists() {
        let mut expected = CommandRequest::new(
            "opencode",
            vec![
                "db".into(),
                global_session_query(3, false),
                "--format".into(),
                "tsv".into(),
            ],
        );
        expected.timeout = Duration::from_secs(8);
        let rows = (1..=3)
            .map(|id| {
                format!(
                    "{{\"id\":\"ses_{id}\",\"title\":\"row {id}\",\"updated\":{id},\"created\":1,\"projectId\":\"global\",\"directory\":\"/work\"}}"
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let runner = Arc::new(FakeRunner {
            expected,
            output: Mutex::new(Some(CommandOutput {
                status: 0,
                stdout: format!("record\n{rows}\n").into_bytes(),
                stderr: vec![],
            })),
        });
        let source = OpenCodeSource::with_runner(
            "test",
            OpenCodeInvocation::host("opencode"),
            Runtime::Host,
            runner,
        );

        let result = source
            .discover_with_warnings(&DiscoveryRequest {
                include_completed: true,
                include_external: true,
                history_limit: 2,
                ..DiscoveryRequest::default()
            })
            .unwrap();

        assert_eq!(result.sessions.len(), 2);
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("limited to 2 records"));
    }

    #[test]
    fn a_live_process_turns_its_session_into_an_active_row() {
        let mut expected = CommandRequest::new(
            "opencode",
            vec![
                "db".into(),
                session_query(&Scope::Recent {
                    limit: 101,
                    oldest_first: false,
                    pinned: BTreeSet::from(["ses_1".to_owned()]),
                }),
                "--format".into(),
                "tsv".into(),
            ],
        );
        expected.timeout = Duration::from_secs(8);
        let runner = Arc::new(FakeRunner {
            expected,
            output: Mutex::new(Some(CommandOutput {
                status: 0,
                stdout: b"record\n{\"id\":\"ses_1\",\"title\":\"one\",\"updated\":12000,\"created\":9000,\"projectId\":\"global\",\"directory\":\"/work\",\"last\":{\"role\":\"assistant\",\"created\":10000,\"completed\":null,\"question\":0}}\n{\"id\":\"ses_2\",\"title\":\"two\",\"updated\":2,\"created\":1,\"projectId\":\"global\",\"directory\":\"/work\",\"last\":{\"role\":\"assistant\",\"created\":1,\"completed\":null,\"question\":0}}\n".to_vec(),
                stderr: vec![],
            })),
        });
        let source = OpenCodeSource::with_runner(
            "test",
            OpenCodeInvocation::host("opencode"),
            Runtime::Host,
            runner,
        )
        .with_probe(|| {
            vec![Holder {
                pid: 42,
                started_ms: 5_000,
                target: opencode_live::Target::Session("ses_1".into()),
            }]
        });

        // Completed history is hidden, but a held session is not history.
        let sessions = source
            .discover(&DiscoveryRequest {
                include_external: true,
                ..DiscoveryRequest::default()
            })
            .unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].provider_session_id, "ses_1");
        assert_eq!(sessions[0].state, SessionState::Working);
        assert_eq!(sessions[0].pid, Some(42));
        assert_eq!(sessions[0].raw_state.as_deref(), Some("running turn"));
    }

    /// Answers each `opencode db` query by a substring of its SQL, since the
    /// restore queries run concurrently in no fixed order.
    struct QueryRunner {
        answers: Vec<(&'static str, Result<&'static str, ()>)>,
        seen: Mutex<Vec<String>>,
    }

    impl CommandRunner for QueryRunner {
        fn run(&self, request: &CommandRequest) -> Result<CommandOutput> {
            let query = request.args[1].clone();
            self.seen.lock().unwrap().push(query.clone());
            let (_, answer) = self
                .answers
                .iter()
                .find(|(needle, _)| query.contains(needle))
                .unwrap_or_else(|| panic!("unexpected query {query}"));
            match answer {
                Ok(stdout) => Ok(CommandOutput {
                    status: 0,
                    stdout: stdout.as_bytes().to_vec(),
                    stderr: vec![],
                }),
                Err(()) => bail!("db unavailable"),
            }
        }
    }

    fn restorable_source(runner: Arc<QueryRunner>) -> OpenCodeSource {
        OpenCodeSource::with_runner(
            "test",
            OpenCodeInvocation::host("opencode"),
            Runtime::Host,
            runner,
        )
    }

    const SESSION_ROWS: &str = "record\n{\"id\":\"ses_old\",\"title\":\"arca memory\",\"directory\":\"/work/arca\",\"updated\":7}\n{\"id\":\"ses_tmp\",\"title\":\"probe\",\"directory\":\"/tmp/probe\",\"updated\":6}\n{\"id\":\"ses_scratch\",\"title\":\"scratch\",\"directory\":\"/private/var/folders/x/T/repo\",\"updated\":5}\nnot json\n";

    #[test]
    fn restorable_sessions_list_root_history_as_dashboard_rows_with_recent_text() {
        let runner = Arc::new(QueryRunner {
            answers: vec![
                ("FROM session WHERE parent_id IS NULL ORDER BY", Ok(SESSION_ROWS)),
                (
                    "group_concat",
                    Ok("record\n{\"id\":\"ses_old\",\"text\":\"fix the\\nflaky test\"}\n{\"id\":\"ses_tmp\",\"text\":\"scratch\"}\n"),
                ),
            ],
            seen: Mutex::new(Vec::new()),
        });
        let now_ms = 30 * 24 * 60 * 60 * 1000;

        let sessions = restorable_source(runner.clone())
            .restorable_sessions_at(now_ms)
            .unwrap();

        assert_eq!(
            sessions,
            vec![RestorableSession {
                id: "opencode:host:ses_old".into(),
                provider_session_id: "ses_old".into(),
                provider: Provider::OpenCode,
                name: "arca memory".into(),
                cwd: PathBuf::from("/work/arca"),
                updated_at_ms: 7,
                transcript: "fix the\nflaky test".into(),
            }]
        );
        let seen = runner.seen.lock().unwrap();
        let text_query = seen
            .iter()
            .find(|query| query.contains("group_concat"))
            .unwrap();
        assert!(text_query.contains(&format!("p.time_created >= {}", 16 * 24 * 60 * 60 * 1000)));
    }

    #[test]
    fn restorable_sessions_still_list_when_the_text_query_fails() {
        let runner = Arc::new(QueryRunner {
            answers: vec![
                (
                    "FROM session WHERE parent_id IS NULL ORDER BY",
                    Ok(SESSION_ROWS),
                ),
                ("group_concat", Err(())),
            ],
            seen: Mutex::new(Vec::new()),
        });

        let sessions = restorable_source(runner).restorable_sessions_at(0).unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].transcript, "");
    }

    #[cfg(unix)]
    #[test]
    fn adopted_sessions_are_listed_beyond_the_history_window() {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("state");
        std::fs::create_dir(&state).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
        let ownership = OpenCodeOwnership::load(state.join("owned.json")).unwrap();
        ownership
            .inner
            .record("ses_old", Path::new("/work"), "old", None, "OpenCode")
            .unwrap();
        let mut expected = CommandRequest::new(
            "opencode",
            vec![
                "db".into(),
                session_query(&Scope::Recent {
                    limit: 2,
                    oldest_first: false,
                    pinned: BTreeSet::from(["ses_old".to_owned()]),
                }),
                "--format".into(),
                "tsv".into(),
            ],
        );
        expected.timeout = Duration::from_secs(8);
        let row = |id: &str, updated: u64| {
            format!("{{\"id\":\"{id}\",\"title\":\"{id}\",\"updated\":{updated},\"created\":1,\"projectId\":\"global\",\"directory\":\"/work\"}}")
        };
        let runner = Arc::new(FakeRunner {
            expected,
            output: Mutex::new(Some(CommandOutput {
                status: 0,
                stdout: format!(
                    "record\n{}\n{}\n{}\n",
                    row("ses_new", 30),
                    row("ses_mid", 20),
                    row("ses_old", 1)
                )
                .into_bytes(),
                stderr: vec![],
            })),
        });
        let source = OpenCodeSource::with_runner(
            "test",
            OpenCodeInvocation::host("opencode"),
            Runtime::Host,
            runner,
        )
        .owned(ownership)
        .with_probe(Vec::new);

        let result = source
            .discover_with_warnings(&DiscoveryRequest {
                include_completed: true,
                include_external: true,
                history_limit: 1,
                ..DiscoveryRequest::default()
            })
            .unwrap();

        let ids = result
            .sessions
            .iter()
            .map(|session| session.provider_session_id.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(ids, BTreeSet::from(["ses_new", "ses_old"]));
    }

    #[test]
    fn pinned_and_owned_ids_are_quoted_only_when_they_are_plain_identifiers() {
        let ids = BTreeSet::from([
            "ses_ok-1".to_owned(),
            "ses_x') OR 1=1 --".to_owned(),
            String::new(),
        ]);
        assert_eq!(sql_id_list(&ids).as_deref(), Some("'ses_ok-1'"));
        assert!(session_query(&Scope::Only(BTreeSet::new())).ends_with("AND s.id IN (NULL)"));
    }

    #[test]
    fn inspect_uses_export_and_formats_text_messages() {
        let mut expected = CommandRequest::new("opencode", vec!["export".into(), "ses_1".into()]);
        expected.timeout = Duration::from_secs(8);
        let runner = Arc::new(FakeRunner {
            expected,
            output: Mutex::new(Some(CommandOutput {
                status: 0,
                stdout: br#"{"info":{"id":"ses_1"},"messages":[{"info":{"role":"user"},"parts":[{"type":"text","text":"Build it"}]},{"info":{"role":"assistant"},"parts":[{"type":"text","text":"Done"}]}]}"#.to_vec(),
                stderr: b"Exporting session: ses_1".to_vec(),
            })),
        });
        let source = OpenCodeSource::with_runner(
            "test",
            OpenCodeInvocation::host("opencode"),
            Runtime::Host,
            runner,
        );
        let session = parse_opencode_session_list(
            r#"[{"id":"ses_1","title":"one","updated":2,"created":1,"projectId":"global","directory":"/work"}]"#,
            Runtime::Host,
        )
        .unwrap()
        .remove(0);

        assert_eq!(
            source.inspect(&session).unwrap(),
            "User: Build it\n\nAssistant: Done"
        );
    }

    #[cfg(unix)]
    #[test]
    fn controller_opens_the_exact_native_session() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("opencode-test");
        std::fs::write(
            &executable,
            "#!/bin/sh\n[ \"$1\" = --session ] && [ \"$2\" = ses_1 ]\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions).unwrap();
        let mut session = parse_opencode_session_list(
            r#"[{"id":"ses_1","title":"one","updated":2,"created":1,"projectId":"global","directory":"/work"}]"#,
            Runtime::Host,
        )
        .unwrap()
        .remove(0);
        session.cwd = directory.path().to_path_buf();
        let controller = OpenCodeController::host(executable.display().to_string());

        let outcome = controller.open(&session).unwrap();

        assert_eq!(outcome.provider_session_hint, Some("ses_1".into()));
    }

    #[test]
    fn completed_history_respects_include_completed() {
        let mut expected = CommandRequest::new(
            "opencode",
            vec![
                "db".into(),
                global_session_query(101, false),
                "--format".into(),
                "tsv".into(),
            ],
        );
        expected.timeout = Duration::from_secs(8);
        let runner = Arc::new(FakeRunner {
            expected,
            output: Mutex::new(Some(CommandOutput {
                status: 0,
                stdout: br#"[{"id":"ses_1","title":"one","updated":2,"created":1,"projectId":"global","directory":"/work"}]"#.to_vec(),
                stderr: vec![],
            })),
        });
        let source = OpenCodeSource::with_runner(
            "test",
            OpenCodeInvocation::host("opencode"),
            Runtime::Host,
            runner.clone(),
        );

        assert!(source
            .discover(&DiscoveryRequest::default())
            .unwrap()
            .is_empty());
        assert!(
            runner.output.lock().unwrap().is_some(),
            "completed-history discovery should not run at all when it is hidden"
        );
    }
}
