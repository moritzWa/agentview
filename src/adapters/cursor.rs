use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;

use super::cursor_history::CursorOwnership;
#[cfg(target_os = "linux")]
use super::cursor_managed::CursorSupervisor;
use crate::control::{
    run_native_authentication, ControlOutcome, LaunchMode, LaunchPresentation, LaunchRequest,
    ProviderController,
};
use crate::domain::{AgentSession, Provider, Runtime, SessionSnapshot};
use crate::process::{CommandRequest, CommandRunner, ProcessRunner};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CursorCommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub current_dir: PathBuf,
}

impl CursorCommandSpec {
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args).current_dir(&self.current_dir);
        command
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CursorInvocation {
    executable: String,
}

impl CursorInvocation {
    pub fn host(executable: impl Into<String>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    /// Build Cursor's documented native resume command without a shell.
    pub fn resume(&self, session_id: &str, cwd: &Path) -> Result<CursorCommandSpec> {
        require_session_id(session_id)?;
        require_absolute_cwd(cwd)?;
        Ok(CursorCommandSpec {
            program: self.executable.clone(),
            args: vec![
                "--resume".into(),
                session_id.into(),
                "--workspace".into(),
                cwd.display().to_string(),
            ],
            current_dir: cwd.to_owned(),
        })
    }

    /// Build an interactive first turn for a preallocated chat. Unlike the
    /// managed background transport this intentionally omits `--print` so the
    /// user sees Cursor's native interface immediately.
    pub fn resume_with_prompt(
        &self,
        session_id: &str,
        cwd: &Path,
        prompt: &str,
        model: Option<&str>,
    ) -> Result<CursorCommandSpec> {
        self.resume_with_prompt_security(session_id, cwd, prompt, model, false)
    }

    pub fn resume_with_prompt_yolo(
        &self,
        session_id: &str,
        cwd: &Path,
        prompt: &str,
        model: Option<&str>,
    ) -> Result<CursorCommandSpec> {
        self.resume_with_prompt_security(session_id, cwd, prompt, model, true)
    }

    fn resume_with_prompt_security(
        &self,
        session_id: &str,
        cwd: &Path,
        prompt: &str,
        model: Option<&str>,
        yolo: bool,
    ) -> Result<CursorCommandSpec> {
        require_session_id(session_id)?;
        require_absolute_cwd(cwd)?;
        require_prompt(prompt)?;
        let mut spec = self.resume(session_id, cwd)?;
        if yolo {
            spec.args.insert(0, "--force".into());
        }
        if let Some(model) = model {
            require_model(model)?;
            spec.args.extend(["--model".into(), model.into()]);
        }
        spec.args.push(prompt.trim().into());
        Ok(spec)
    }

    /// Build the documented empty-chat allocator used by managed integrations.
    pub fn create_chat(&self, cwd: &Path) -> Result<CursorCommandSpec> {
        self.create_chat_with_model(cwd, None)
    }

    /// Allocate the chat with its selected model already persisted.
    ///
    /// Cursor applies a model flag passed only to a later `--resume` process
    /// to that process, but a preallocated chat can retain the account's prior
    /// named model on its following turn. Supplying the documented global
    /// option to `create-chat` makes the session itself use the requested
    /// choice, including `auto` on plans that reject named models.
    pub fn create_chat_with_model(
        &self,
        cwd: &Path,
        model: Option<&str>,
    ) -> Result<CursorCommandSpec> {
        require_absolute_cwd(cwd)?;
        let mut args = vec!["create-chat".into()];
        if let Some(model) = model {
            require_model(model)?;
            args.extend(["--model".into(), model.into()]);
        }
        Ok(CursorCommandSpec {
            program: self.executable.clone(),
            args,
            current_dir: cwd.to_owned(),
        })
    }

    /// Build a single-turn managed run. The caller must retain the child and
    /// NDJSON stream; this function never adds `--force` or `--yolo`.
    pub fn print_turn(
        &self,
        session_id: &str,
        cwd: &Path,
        prompt: &str,
        model: Option<&str>,
    ) -> Result<CursorCommandSpec> {
        self.print_turn_with_security(session_id, cwd, prompt, model, false)
    }

    pub fn print_turn_yolo(
        &self,
        session_id: &str,
        cwd: &Path,
        prompt: &str,
        model: Option<&str>,
    ) -> Result<CursorCommandSpec> {
        self.print_turn_with_security(session_id, cwd, prompt, model, true)
    }

    fn print_turn_with_security(
        &self,
        session_id: &str,
        cwd: &Path,
        prompt: &str,
        model: Option<&str>,
        yolo: bool,
    ) -> Result<CursorCommandSpec> {
        require_session_id(session_id)?;
        require_absolute_cwd(cwd)?;
        require_prompt(prompt)?;
        let mut args = Vec::new();
        if yolo {
            args.push("--force".into());
        }
        args.extend([
            "--resume".into(),
            session_id.into(),
            "--print".into(),
            "--output-format".into(),
            "stream-json".into(),
            "--workspace".into(),
            cwd.display().to_string(),
        ]);
        if let Some(model) = model {
            require_model(model)?;
            args.extend(["--model".into(), model.into()]);
        }
        args.push(prompt.trim().into());
        Ok(CursorCommandSpec {
            program: self.executable.clone(),
            args,
            current_dir: cwd.to_owned(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CursorStreamEvent {
    Initialized {
        session_id: String,
        cwd: PathBuf,
        model: Option<String>,
    },
    AssistantText {
        session_id: String,
        text: String,
    },
    ToolStarted {
        session_id: String,
        call_id: String,
    },
    ToolCompleted {
        session_id: String,
        call_id: String,
    },
    Finished {
        session_id: String,
        result: String,
        is_error: bool,
    },
    Other(Value),
}

pub fn parse_cursor_stream_event(line: &str) -> Result<CursorStreamEvent> {
    let value: Value = serde_json::from_str(line).context("invalid Cursor stream-json event")?;
    let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    let subtype = value.get("subtype").and_then(Value::as_str).unwrap_or("");
    match (event_type, subtype) {
        ("system", "init") => Ok(CursorStreamEvent::Initialized {
            session_id: required_string(&value, "session_id")?,
            cwd: PathBuf::from(required_string(&value, "cwd")?),
            model: value
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }),
        ("assistant", _) => Ok(CursorStreamEvent::AssistantText {
            session_id: required_string(&value, "session_id")?,
            text: assistant_text(&value)?,
        }),
        ("tool_call", "started") => Ok(CursorStreamEvent::ToolStarted {
            session_id: required_string(&value, "session_id")?,
            call_id: required_string(&value, "call_id")?,
        }),
        ("tool_call", "completed") => Ok(CursorStreamEvent::ToolCompleted {
            session_id: required_string(&value, "session_id")?,
            call_id: required_string(&value, "call_id")?,
        }),
        ("result", _) => Ok(CursorStreamEvent::Finished {
            session_id: required_string(&value, "session_id")?,
            result: value
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            is_error: value
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(subtype != "success"),
        }),
        _ => Ok(CursorStreamEvent::Other(value)),
    }
}

pub fn parse_cursor_chat_id(output: &str) -> Result<String> {
    let id = output.trim();
    require_session_id(id)?;
    if id.split_whitespace().count() != 1 {
        bail!("Cursor create-chat returned more than one token");
    }
    Ok(id.to_owned())
}

/// Host controller for Cursor chats.
///
/// On every platform it can create a chat and open it in Cursor's interface.
/// Inline reply, interrupt, and the durable registry stay on the Linux
/// supervisor.
pub struct CursorController {
    invocation: CursorInvocation,
    chats_root: Option<PathBuf>,
    /// Chats this dashboard created off Linux, listed by the history source
    /// without `--include-external`.
    ownership: Option<Arc<CursorOwnership>>,
    #[cfg(target_os = "linux")]
    supervisor: Option<Arc<CursorSupervisor>>,
}

impl CursorController {
    pub fn host(executable: impl Into<String>) -> Self {
        Self {
            invocation: CursorInvocation::host(executable),
            chats_root: None,
            ownership: None,
            #[cfg(target_os = "linux")]
            supervisor: None,
        }
    }

    /// Control both native external sessions and the exact processes launched
    /// through this supervisor. Only the latter gain inline capabilities.
    #[cfg(target_os = "linux")]
    pub fn managed(supervisor: Arc<CursorSupervisor>) -> Self {
        Self {
            invocation: CursorInvocation::host(supervisor.executable()),
            chats_root: None,
            ownership: None,
            supervisor: Some(supervisor),
        }
    }

    /// Read on-disk chats from `chats_root` instead of `~/.cursor/chats`.
    pub fn with_chats_root(mut self, chats_root: PathBuf) -> Self {
        self.chats_root = Some(chats_root);
        self
    }

    /// Record the chats this controller creates in `ownership`.
    pub fn with_ownership(mut self, ownership: Arc<CursorOwnership>) -> Self {
        self.ownership = Some(ownership);
        self
    }
}

impl ProviderController for CursorController {
    fn provider(&self) -> Provider {
        Provider::Cursor
    }

    fn launch_mode(&self) -> LaunchMode {
        // Linux launches go through the supervisor's registry; without one a
        // new chat would have no owner and never appear as a row.
        #[cfg(target_os = "linux")]
        if self.supervisor.is_none() {
            return LaunchMode::Unavailable;
        }
        LaunchMode::SelectableModel
    }

    fn launch_presentation(&self) -> LaunchPresentation {
        // Linux managed launch records the chat first, then opens it once the
        // row appears. Everywhere else the composer opens Cursor directly.
        #[cfg(target_os = "linux")]
        if self.supervisor.is_some() {
            return LaunchPresentation::DeferredForeground;
        }
        LaunchPresentation::Foreground
    }

    fn supports_yolo(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.supervisor.is_some()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    fn available_models(&self) -> Result<Vec<String>> {
        #[cfg(target_os = "linux")]
        if self.supervisor.is_some() {
            return self.managed_supervisor()?.available_models();
        }
        host_available_models(&self.invocation.executable)
    }

    fn supports_authentication(&self) -> bool {
        true
    }

    fn authenticate(&self) -> Result<ControlOutcome> {
        run_native_authentication(&self.invocation.executable, &["login"], Provider::Cursor)
    }

    fn enrich(&self, snapshot: &mut SessionSnapshot) {
        #[cfg(target_os = "linux")]
        if let Some(supervisor) = &self.supervisor {
            supervisor.enrich(snapshot);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = snapshot;
    }

    fn launch(&self, request: &LaunchRequest) -> Result<ControlOutcome> {
        if request.provider != Provider::Cursor {
            bail!("the Cursor controller cannot launch another provider");
        }
        // Off Linux the composer only opens Cursor in the foreground; a
        // background launch would have nowhere to deliver the prompt.
        #[cfg(not(target_os = "linux"))]
        bail!(
            "background Cursor launch is unavailable on this platform; open it in the foreground"
        );
        #[cfg(target_os = "linux")]
        let supervisor = self.managed_supervisor()?;
        #[cfg(target_os = "linux")]
        let session_id = supervisor.allocate_chat_with_model(
            &request.prompt,
            &request.cwd,
            request.model.as_deref(),
        )?;
        #[cfg(target_os = "linux")]
        Ok(ControlOutcome {
            message: format!("launched managed Cursor session {session_id}"),
            provider_session_hint: Some(session_id),
        })
    }

    fn launch_yolo(&self, request: &LaunchRequest) -> Result<ControlOutcome> {
        if request.provider != Provider::Cursor {
            bail!("the Cursor controller cannot launch another provider");
        }
        #[cfg(target_os = "linux")]
        let session_id = self.managed_supervisor()?.allocate_chat_with_model_yolo(
            &request.prompt,
            &request.cwd,
            request.model.as_deref(),
        )?;
        #[cfg(not(target_os = "linux"))]
        bail!("managed Cursor YOLO launch is unavailable on this platform");
        #[cfg(target_os = "linux")]
        Ok(ControlOutcome {
            message: format!("launched managed Cursor YOLO session {session_id}"),
            provider_session_hint: Some(session_id),
        })
    }

    fn launch_foreground(&self, request: &LaunchRequest) -> Result<ControlOutcome> {
        if request.provider != Provider::Cursor {
            bail!("the Cursor controller cannot launch another provider");
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.open_new_chat(request)
        }
        #[cfg(target_os = "linux")]
        {
            let supervisor = self.managed_supervisor()?;
            let session_id = supervisor.allocate_chat_with_model(
                &request.prompt,
                &request.cwd,
                request.model.as_deref(),
            )?;
            let spec = self.invocation.resume_with_prompt(
                &session_id,
                &request.cwd,
                &request.prompt,
                request.model.as_deref(),
            )?;
            let key = format!("cursor:host:{session_id}");
            match crate::native_session::run(spec.command(), &key)? {
                crate::native_session::NativeSessionExit::Backgrounded => {
                    supervisor.mark_native_opened(&session_id)?;
                    Ok(ControlOutcome {
                        message: format!(
                            "backgrounded Cursor session {session_id}; Enter/Right resumes it"
                        ),
                        provider_session_hint: Some(session_id),
                    })
                }
                crate::native_session::NativeSessionExit::Exited(status) if status.success() => {
                    supervisor.mark_native_opened(&session_id)?;
                    Ok(ControlOutcome {
                        message: format!("returned from Cursor session {session_id}"),
                        provider_session_hint: Some(session_id),
                    })
                }
                crate::native_session::NativeSessionExit::Exited(status) => {
                    supervisor.mark_native_opened(&session_id)?;
                    bail!("Cursor session exited with status {status}")
                }
            }
        }
    }

    fn interrupt(&self, session: &AgentSession) -> Result<ControlOutcome> {
        #[cfg(target_os = "linux")]
        {
            self.managed_supervisor()?.interrupt(session)?;
            Ok(ControlOutcome {
                message: format!("interrupted {}", session.name),
                provider_session_hint: None,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = session;
            bail!("managed Cursor interrupt is unavailable on this platform")
        }
    }

    fn inspect(&self, session: &AgentSession) -> Result<String> {
        #[cfg(target_os = "linux")]
        if let Some(supervisor) = &self.supervisor {
            if supervisor.owns(session) {
                return supervisor.inspect(session);
            }
        }
        // Chats discovered from Cursor's on-disk store: show the prompts it
        // recorded. The chats directory can be overridden for tests.
        let chats_root = self
            .chats_root
            .clone()
            .map(Ok)
            .unwrap_or_else(super::cursor_history::default_cursor_chats_dir)?;
        super::cursor_history::inspect_cursor_history(&chats_root, session)
    }

    fn reply(&self, session: &AgentSession, prompt: &str) -> Result<ControlOutcome> {
        #[cfg(target_os = "linux")]
        {
            self.managed_supervisor()?.reply(session, prompt)?;
            Ok(ControlOutcome {
                message: format!("sent a new turn to {}", session.name),
                provider_session_hint: None,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (session, prompt);
            bail!("managed Cursor reply is unavailable on this platform")
        }
    }

    fn open(&self, session: &AgentSession) -> Result<ControlOutcome> {
        if session.provider != Provider::Cursor || session.runtime != Runtime::Host {
            bail!("the Cursor host controller cannot open this session");
        }
        #[cfg(target_os = "linux")]
        let owned = match &self.supervisor {
            Some(supervisor) => supervisor.owns(session),
            None => false,
        };
        #[cfg(not(target_os = "linux"))]
        let owned = false;
        #[cfg(target_os = "linux")]
        if let Some(supervisor) = &self.supervisor {
            if owned && supervisor.is_running(session)? {
                bail!("interrupt the active managed Cursor turn before opening it natively");
            }
        }
        // An external chat with a live holder is already open in another
        // cursor-agent; a second resume would write the same chat store. The
        // holder may be the terminal this dashboard keeps in the background,
        // which the resume below reattaches instead of starting a second one.
        let ours = crate::native_session::detached_session_keys()
            .iter()
            .any(|key| *key == session.id);
        if let (false, false, Some(pid)) = (owned, ours, session.pid) {
            bail!("chat is open in cursor-agent (pid {pid}); close it there first");
        }
        #[cfg(target_os = "linux")]
        let pending_native = self
            .supervisor
            .as_ref()
            .map(|supervisor| supervisor.pending_native_launch(session))
            .transpose()?
            .flatten();
        #[cfg(not(target_os = "linux"))]
        let pending_native: Option<(String, Option<String>, bool)> = None;
        let (spec, yolo) = if let Some((prompt, model, yolo)) = pending_native {
            let spec = if yolo {
                self.invocation.resume_with_prompt_yolo(
                    &session.provider_session_id,
                    &session.cwd,
                    &prompt,
                    model.as_deref(),
                )?
            } else {
                self.invocation.resume_with_prompt(
                    &session.provider_session_id,
                    &session.cwd,
                    &prompt,
                    model.as_deref(),
                )?
            };
            (spec, yolo)
        } else {
            #[cfg(target_os = "linux")]
            let yolo = self
                .supervisor
                .as_ref()
                .is_some_and(|supervisor| supervisor.yolo_if_owned(session));
            #[cfg(not(target_os = "linux"))]
            let yolo = false;
            let mut spec = self
                .invocation
                .resume(&session.provider_session_id, &session.cwd)?;
            if yolo {
                spec.args.insert(0, "--force".into());
            }
            (spec, yolo)
        };
        let exit = if yolo {
            crate::native_session::run_yolo(spec.command(), &session.id, "Cursor")?
        } else {
            crate::native_session::run(spec.command(), &session.id)?
        };
        match exit {
            crate::native_session::NativeSessionExit::Backgrounded => {
                #[cfg(target_os = "linux")]
                if let Some(supervisor) = &self.supervisor {
                    if supervisor.owns(session) {
                        supervisor.mark_native_opened(&session.provider_session_id)?;
                    }
                }
                Ok(ControlOutcome {
                    message: format!("backgrounded {}; Enter/Right resumes it", session.name),
                    provider_session_hint: Some(session.provider_session_id.clone()),
                })
            }
            crate::native_session::NativeSessionExit::Exited(status) if status.success() => {
                #[cfg(target_os = "linux")]
                if let Some(supervisor) = &self.supervisor {
                    if supervisor.owns(session) {
                        supervisor.mark_native_opened(&session.provider_session_id)?;
                    }
                }
                Ok(ControlOutcome {
                    message: format!("returned from {}", session.name),
                    provider_session_hint: None,
                })
            }
            crate::native_session::NativeSessionExit::Exited(status) => {
                bail!("Cursor session exited with status {status}")
            }
        }
    }
}

impl CursorController {
    #[cfg(target_os = "linux")]
    fn managed_supervisor(&self) -> Result<&CursorSupervisor> {
        self.supervisor
            .as_deref()
            .context("managed Cursor control is not configured")
    }
}

fn required_string(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .with_context(|| format!("Cursor event omitted {field}"))
}

fn assistant_text(value: &Value) -> Result<String> {
    #[derive(Deserialize)]
    struct Message {
        content: Vec<Content>,
    }
    #[derive(Deserialize)]
    struct Content {
        #[serde(rename = "type")]
        kind: String,
        text: Option<String>,
    }
    let message: Message = serde_json::from_value(
        value
            .get("message")
            .cloned()
            .context("Cursor assistant event omitted message")?,
    )?;
    Ok(message
        .content
        .into_iter()
        .filter(|content| content.kind == "text")
        .filter_map(|content| content.text)
        .collect())
}

fn require_session_id(session_id: &str) -> Result<()> {
    if session_id.is_empty()
        || session_id.chars().any(char::is_control)
        || session_id.chars().any(char::is_whitespace)
    {
        bail!("Cursor session ID is empty or contains whitespace/control characters");
    }
    Ok(())
}

/// The prompt is cursor-agent's trailing positional argument. Its help does
/// not document `--` as an end-of-options marker, so a prompt that looks like
/// an option (`-f`, `--yolo`) would be parsed as one and could turn on Run
/// Everything without agentview's warning. Refuse it instead.
fn require_prompt(prompt: &str) -> Result<()> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        bail!("Cursor prompt must not be empty");
    }
    if prompt.starts_with('-') {
        bail!("Cursor prompts cannot start with '-'; cursor-agent would read it as an option");
    }
    Ok(())
}

fn require_absolute_cwd(cwd: &Path) -> Result<()> {
    if !cwd.is_absolute() {
        bail!("Cursor workspace must be absolute");
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
impl CursorController {
    /// Create a chat and open it with the prompt in Cursor's interface.
    ///
    /// `create-chat` runs on the dashboard thread (bounded at 20 seconds)
    /// because this is a foreground launch. Moving it to the launch worker
    /// would mean a deferred open like Linux's, which needs the new chat's
    /// row to be discoverable before its first turn; the chat store is not
    /// known to list a chat that early, so the allocation stays here.
    fn open_new_chat(&self, request: &LaunchRequest) -> Result<ControlOutcome> {
        let session_id =
            create_host_chat(&self.invocation, &request.cwd, request.model.as_deref())?;
        let spec = self.invocation.resume_with_prompt(
            &session_id,
            &request.cwd,
            &request.prompt,
            request.model.as_deref(),
        )?;
        // Record the chat before opening it, so its row is listed on return
        // even when external discovery is off.
        if let Some(ownership) = &self.ownership {
            ownership.record(&session_id, &request.cwd, &request.prompt)?;
        }
        let key = format!("cursor:host:{session_id}");
        match crate::native_session::run(spec.command(), &key)? {
            crate::native_session::NativeSessionExit::Backgrounded => Ok(ControlOutcome {
                message: format!(
                    "backgrounded Cursor session {session_id}; Enter/Right resumes it"
                ),
                provider_session_hint: Some(session_id),
            }),
            crate::native_session::NativeSessionExit::Exited(status) if status.success() => {
                Ok(ControlOutcome {
                    message: format!("returned from Cursor session {session_id}"),
                    provider_session_hint: Some(session_id),
                })
            }
            crate::native_session::NativeSessionExit::Exited(status) => {
                bail!("Cursor session exited with status {status}")
            }
        }
    }
}

/// Model ids from `cursor-agent models`. The CLI colours its output when
/// `FORCE_COLOR` is set and redraws a progress line before the list, so escape
/// sequences are rendered away before the ids are read.
pub(super) fn parse_cursor_models(output: &str) -> Vec<String> {
    let rendered = if output.contains('\x1b') {
        let mut parser = vt100::Parser::new(200, 240, 0);
        parser.process(output.as_bytes());
        parser.screen().contents()
    } else {
        output.to_owned()
    };
    let mut models = std::collections::BTreeSet::new();
    for line in rendered.lines() {
        let line = line.trim().trim_start_matches(|character: char| {
            character.is_whitespace() || matches!(character, '-' | '*' | '•' | '›' | '>' | '✓')
        });
        let Some(candidate) = line.split_whitespace().next() else {
            continue;
        };
        let candidate = candidate.trim_matches(|character: char| matches!(character, ':' | ','));
        let lower = candidate.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "loading" | "models" | "model" | "available" | "no" | "name" | "id" | "tip"
        ) {
            continue;
        }
        if candidate.is_empty()
            || candidate.len() > 128
            || !candidate.chars().all(|character| {
                character.is_ascii_alphanumeric()
                    || matches!(character, '-' | '_' | '.' | ':' | '/' | '@')
            })
        {
            continue;
        }
        models.insert(if candidate.eq_ignore_ascii_case("auto") {
            "auto".into()
        } else {
            candidate.into()
        });
    }
    models.into_iter().collect()
}

fn host_available_models(executable: &str) -> Result<Vec<String>> {
    let mut request = CommandRequest::new(executable, vec!["models".into()]);
    request.timeout = Duration::from_secs(4);
    let output = ProcessRunner.run(&request).with_context(|| {
        format!("Cursor model catalog did not respond (configured executable: {executable})")
    })?;
    let stdout = output.stdout_text()?;
    let stderr = output.stderr_lossy();
    if output.status != 0 {
        let detail = format!("{stdout}\n{stderr}").to_ascii_lowercase();
        if detail.contains("auth") || detail.contains("login") || detail.contains("no models") {
            bail!(
                "Cursor is not authenticated or this account has no models; press Enter to sign in"
            );
        }
        bail!(
            "Cursor model catalog failed with status {}: {stderr}",
            output.status
        );
    }
    let models = parse_cursor_models(stdout);
    if models.is_empty() {
        bail!("Cursor returned no account models; press Enter to sign in or check plan access");
    }
    Ok(models)
}

#[cfg(not(target_os = "linux"))]
fn create_host_chat(
    invocation: &CursorInvocation,
    cwd: &Path,
    model: Option<&str>,
) -> Result<String> {
    let spec = invocation.create_chat_with_model(cwd, model)?;
    let mut request = CommandRequest::new(spec.program, spec.args);
    request.current_dir = Some(spec.current_dir);
    request.timeout = Duration::from_secs(20);
    let output = ProcessRunner
        .run(&request)
        .context("Cursor create-chat did not respond")?;
    let stdout = output.stdout_text()?;
    if output.status != 0 {
        bail!(
            "Cursor create-chat failed with status {}: {}",
            output.status,
            output.stderr_lossy()
        );
    }
    parse_cursor_chat_id(stdout)
}

fn require_model(model: &str) -> Result<()> {
    if model.is_empty()
        || model.len() > 128
        || model
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        bail!("Cursor model must contain 1 to 128 non-whitespace bytes");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::ProviderController;

    #[test]
    fn model_catalog_keeps_auto_and_named_ids() {
        let models = parse_cursor_models(
            "Available models\n\nauto - Auto (default)\ncomposer-2.5 - Composer\nTip: use --model\n",
        );
        assert_eq!(models, vec!["auto".to_owned(), "composer-2.5".to_owned()]);
    }

    #[test]
    fn model_catalog_reads_ids_through_forced_colour() {
        // `FORCE_COLOR=1 cursor-agent models` wraps each id in SGR codes.
        let models = parse_cursor_models(
            "\x1b[1mAvailable models\x1b[22m\n\n\x1b[36mauto\x1b[39m - Auto (default)\n\x1b[36mcomposer-2.5\x1b[39m - Composer\n\x1b[2mTip: use --model\x1b[22m\n",
        );
        assert_eq!(models, vec!["auto".to_owned(), "composer-2.5".to_owned()]);
    }

    #[test]
    fn host_cursor_is_a_selectable_harness() {
        let controller = CursorController::host("cursor-agent");
        // Linux launches need the supervisor's registry.
        #[cfg(target_os = "linux")]
        assert_eq!(controller.launch_mode(), LaunchMode::Unavailable);
        #[cfg(not(target_os = "linux"))]
        assert_eq!(controller.launch_mode(), LaunchMode::SelectableModel);
        #[cfg(not(target_os = "linux"))]
        assert_eq!(
            controller.launch_presentation(),
            LaunchPresentation::Foreground
        );
    }

    #[cfg(unix)]
    #[test]
    fn builds_shell_free_resume_and_safe_print_invocations() {
        let invocation = CursorInvocation::host("cursor-agent");
        assert_eq!(
            invocation
                .resume("chat-id", Path::new("/work/repo"))
                .unwrap(),
            CursorCommandSpec {
                program: "cursor-agent".into(),
                args: vec![
                    "--resume".into(),
                    "chat-id".into(),
                    "--workspace".into(),
                    "/work/repo".into(),
                ],
                current_dir: "/work/repo".into(),
            }
        );
        let print = invocation
            .print_turn(
                "chat-id",
                Path::new("/work/repo"),
                "check tests",
                Some("auto"),
            )
            .unwrap();
        assert!(print.args.contains(&"stream-json".into()));
        assert!(print
            .args
            .windows(2)
            .any(|args| args == ["--model", "auto"]));
        assert!(!print
            .args
            .iter()
            .any(|arg| arg == "--force" || arg == "--yolo"));
        let foreground = invocation
            .resume_with_prompt(
                "chat-id",
                Path::new("/work/repo"),
                "check interactively",
                Some("auto"),
            )
            .unwrap();
        assert_eq!(
            foreground.args,
            [
                "--resume",
                "chat-id",
                "--workspace",
                "/work/repo",
                "--model",
                "auto",
                "check interactively",
            ]
        );
        assert!(!foreground.args.iter().any(|arg| arg == "--print"));
        let yolo_foreground = invocation
            .resume_with_prompt_yolo(
                "chat-id",
                Path::new("/work/repo"),
                "check interactively",
                Some("auto"),
            )
            .unwrap();
        assert_eq!(yolo_foreground.args[0], "--force");
        assert!(!yolo_foreground.args.iter().any(|arg| arg == "--yolo"));
        let yolo_print = invocation
            .print_turn_yolo(
                "chat-id",
                Path::new("/work/repo"),
                "check tests",
                Some("auto"),
            )
            .unwrap();
        assert_eq!(yolo_print.args[0], "--force");
        assert!(yolo_print.args.iter().any(|arg| arg == "--print"));
        assert!(!yolo_print.args.iter().any(|arg| arg == "--yolo"));
        assert_eq!(
            invocation
                .create_chat_with_model(Path::new("/work/repo"), Some("auto"))
                .unwrap()
                .args,
            ["create-chat", "--model", "auto"]
        );
    }

    #[test]
    fn parses_documented_stream_events() {
        let init = parse_cursor_stream_event(
            r#"{"type":"system","subtype":"init","cwd":"/work","session_id":"abc","model":"GPT-5"}"#,
        )
        .unwrap();
        assert_eq!(
            init,
            CursorStreamEvent::Initialized {
                session_id: "abc".into(),
                cwd: "/work".into(),
                model: Some("GPT-5".into()),
            }
        );

        let assistant = parse_cursor_stream_event(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"done"}]},"session_id":"abc"}"#,
        )
        .unwrap();
        assert_eq!(
            assistant,
            CursorStreamEvent::AssistantText {
                session_id: "abc".into(),
                text: "done".into(),
            }
        );
    }

    #[test]
    fn refuses_to_open_an_external_chat_another_process_holds() {
        let session = AgentSession {
            id: "cursor:host:2a243dcb-b43b-47be-8dfc-73f656a3f5ea".into(),
            provider_session_id: "2a243dcb-b43b-47be-8dfc-73f656a3f5ea".into(),
            provider: Provider::Cursor,
            runtime: Runtime::Host,
            kind: crate::domain::SessionKind::Interactive,
            name: "Agent Comparison".into(),
            cwd: PathBuf::from("/work/repo"),
            state: crate::domain::SessionState::NeedsInput,
            summary: String::new(),
            raw_state: Some("waiting at prompt".into()),
            pid: Some(4242),
            started_at: None,
            updated_at: None,
            pull_requests: None,
            capabilities: std::collections::BTreeSet::new(),
        };
        // The executable does not exist: reaching the resume would fail
        // differently, so this error proves the guard ran first.
        let error = CursorController::host("/nonexistent/cursor-agent")
            .open(&session)
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "chat is open in cursor-agent (pid 4242); close it there first"
        );
    }

    #[test]
    fn refuses_prompts_cursor_agent_would_parse_as_options() {
        let invocation = CursorInvocation::host("cursor-agent");
        let cwd = &std::env::temp_dir();
        for prompt in ["--yolo", "-f", "  --force fix it", "-p"] {
            let error = invocation
                .resume_with_prompt("chat-id", cwd, prompt, None)
                .unwrap_err();
            assert!(
                error.to_string().contains("cannot start with '-'"),
                "{error}"
            );
            assert!(invocation.print_turn("chat-id", cwd, prompt, None).is_err());
        }
        // A dash later in the prompt is ordinary text.
        let spec = invocation
            .resume_with_prompt("chat-id", cwd, "fix the -f flag", None)
            .unwrap();
        assert_eq!(spec.args.last().unwrap(), "fix the -f flag");
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn background_launch_is_refused_off_linux_instead_of_dropping_the_prompt() {
        let error = CursorController::host("/nonexistent/cursor-agent")
            .launch(&LaunchRequest {
                provider: Provider::Cursor,
                prompt: "check the tests".into(),
                cwd: PathBuf::from("/work/repo"),
                model: None,
            })
            .unwrap_err();
        assert!(
            error.to_string().contains("unavailable on this platform"),
            "{error}"
        );
    }

    #[test]
    fn rejects_unsafe_ids_and_relative_workspaces() {
        let invocation = CursorInvocation::host("cursor-agent");
        assert!(invocation.resume("bad\nid", Path::new("/work")).is_err());
        assert!(invocation.resume("id", Path::new("relative")).is_err());
        assert!(parse_cursor_chat_id("one two").is_err());
    }
}
