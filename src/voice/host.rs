//! The real `VoiceHost`: the dashboard's latest sessions plus the control hub.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use anyhow::{bail, Result};

use super::digest::Info;
use super::tools::{Message, VoiceHost};
use crate::control::ControlHub;
use crate::domain::{AgentSession, Capability, Provider, Runtime, SessionKind, SessionState};

#[derive(Default)]
struct View {
    sessions: Vec<AgentSession>,
    paused: BTreeSet<String>,
    /// Child to parent, by provider session id.
    parents: HashMap<String, String>,
}

pub struct DashboardHost {
    control: ControlHub,
    view: Mutex<View>,
}

impl DashboardHost {
    pub fn new(control: ControlHub) -> Arc<Self> {
        Arc::new(Self {
            control,
            view: Mutex::default(),
        })
    }

    /// The dashboard calls this whenever its sessions refresh.
    pub fn publish(
        &self,
        sessions: &[AgentSession],
        paused: &BTreeMap<String, u64>,
        parents: &HashMap<String, String>,
    ) {
        if let Ok(mut view) = self.view.lock() {
            view.sessions = sessions.to_vec();
            view.paused = paused.keys().cloned().collect();
            view.parents = parents.clone();
        }
    }

    /// The session behind a row id. One started a moment ago is not in the
    /// dashboard's list yet, but its supervisor already knows it.
    fn session(&self, id: &str) -> Result<AgentSession> {
        if let Some(session) = self
            .view
            .lock()
            .ok()
            .and_then(|view| view.sessions.iter().find(|s| s.id == id).cloned())
        {
            return Ok(session);
        }
        let Some(provider_id) = id.strip_prefix("opencode:host:") else {
            bail!("that session is no longer in the dashboard");
        };
        Ok(AgentSession {
            id: id.to_owned(),
            provider_session_id: provider_id.to_owned(),
            provider: Provider::OpenCode,
            runtime: Runtime::Host,
            kind: SessionKind::Managed,
            name: provider_id.to_owned(),
            cwd: Default::default(),
            state: SessionState::Working,
            summary: String::new(),
            raw_state: None,
            pid: None,
            started_at: None,
            updated_at: None,
            pull_requests: None,
            capabilities: BTreeSet::from([
                Capability::Inspect,
                Capability::Reply,
                Capability::Interrupt,
            ]),
        })
    }
}

/// The provider's reason, in words the voice can say.
fn needs_you_because(session: &AgentSession) -> Option<String> {
    if session.state != SessionState::NeedsInput {
        return None;
    }
    Some(
        match session.raw_state.as_deref() {
            Some("turn failed") => "its last turn failed; it needs recovery in OpenCode itself",
            Some("question asked") => {
                "it asked a question in OpenCode and waits for the answer there"
            }
            Some("permission requested") => "it waits for a permission to be approved in OpenCode",
            _ => "its last message ends on a question for the developer",
        }
        .to_owned(),
    )
}

fn millis(time: Option<std::time::SystemTime>) -> u64 {
    time.and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

impl VoiceHost for DashboardHost {
    fn infos(&self) -> Vec<Info> {
        let Ok(view) = self.view.lock() else {
            return Vec::new();
        };
        let holds = crate::holds::default_holds_dir();
        view.sessions
            .iter()
            .filter(|session| {
                session.provider == Provider::OpenCode && session.runtime == Runtime::Host
            })
            .map(|session| {
                let facts = crate::last_message::facts_for(&session.id).unwrap_or_default();
                let subagents = view
                    .sessions
                    .iter()
                    .filter(|child| {
                        child.state == SessionState::Working
                            && view.parents.get(&child.provider_session_id)
                                == Some(&session.provider_session_id)
                    })
                    .count();
                Info {
                    id: session.id.clone(),
                    title: session.name.clone(),
                    cwd: session.cwd.clone(),
                    state: session.state,
                    updated_ms: millis(session.updated_at),
                    last_user_ms: facts.last_user_ms,
                    tail: facts.tail,
                    doing: holds.as_deref().and_then(|root| {
                        crate::holds::live_hold(root, "opencode", &session.provider_session_id)
                    }),
                    subagents,
                    paused: view.paused.contains(&session.id),
                    top_level: !view.parents.contains_key(&session.provider_session_id),
                    can_message: session.capabilities.contains(&Capability::Reply),
                    why: needs_you_because(session),
                }
            })
            .collect()
    }

    fn recent_messages(&self, id: &str, count: usize) -> Result<Vec<Message>> {
        let session = self.session(id)?;
        Ok(self
            .control
            .recent_messages(&session, count)?
            .into_iter()
            .map(|message| Message {
                role: message.role,
                text: message.text,
            })
            .collect())
    }

    fn send(&self, id: &str, text: &str) -> Result<()> {
        let session = self.session(id)?;
        self.control.reply(&session, text).map(|_| ())
    }

    fn interrupt(&self, id: &str) -> Result<()> {
        let session = self.session(id)?;
        self.control.interrupt(&session).map(|_| ())
    }

    fn edit_last(&self, id: &str, text: &str) -> Result<()> {
        let session = self.session(id)?;
        self.control.edit_last_message(&session, text).map(|_| ())
    }

    fn start(&self, cwd: &Path, task: &str) -> Result<String> {
        let outcome = self.control.launch_with(
            Provider::OpenCode,
            None,
            task.to_owned(),
            Some(cwd.to_owned()),
        )?;
        match outcome.provider_session_hint {
            Some(id) => Ok(format!("opencode:host:{id}")),
            None => bail!("OpenCode started the session but did not say which one it is"),
        }
    }
}
