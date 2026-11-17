use super::Config;
use crate::adapters::{DiscoveryRequest, OpenCodeController, OpenCodeSource, SessionSource};
use crate::control::ProviderController;
use crate::domain::{AgentSession, SessionSnapshot, SessionState};
use crate::opencode_supervisor::OpenCodeSupervisor;
use anyhow::Result;
use rusqlite::OptionalExtension;
use serde::Serialize;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

#[derive(Serialize)]
pub(super) struct RuntimeStatus {
    session_id: String,
    status: &'static str,
    reason: Option<&'static str>,
    updated_at: u64,
    can_message: bool,
    last_user_message_id: Option<String>,
    last_user_ms: u64,
    top_level: bool,
    attention_assessed: bool,
}
#[derive(Serialize)]
pub(super) struct Snapshot {
    available: bool,
    partial: bool,
    truncated: bool,
    statuses: Vec<RuntimeStatus>,
}

pub(super) fn project(session: &AgentSession) -> RuntimeStatus {
    let status = match session.state {
        SessionState::Working => "working",
        SessionState::NeedsInput => "needs_you",
        SessionState::Scheduled => "scheduled",
        SessionState::ReadyForReview | SessionState::Completed => "done",
        SessionState::Unknown => "unknown",
    };
    let reason = if session.state == SessionState::NeedsInput {
        Some(match session.raw_state.as_deref() {
            Some("permission requested") => "approval",
            Some("question asked") => "question",
            Some("turn failed") => "failed",
            _ => "reply",
        })
    } else {
        None
    };
    let can_message = session.state != SessionState::Unknown
        && !matches!(
            session.raw_state.as_deref(),
            Some("permission requested" | "question asked")
        );
    RuntimeStatus {
        session_id: session.provider_session_id.clone(),
        status,
        reason,
        updated_at: session
            .updated_at
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as u64),
        can_message,
        last_user_message_id: None,
        last_user_ms: crate::last_message::facts_for(&session.id)
            .map_or(0, |facts| facts.last_user_ms),
        top_level: true,
        attention_assessed: session.state == SessionState::NeedsInput,
    }
}

pub(super) fn snapshot(config: &Config) -> Result<Snapshot> {
    let supervisor = Arc::new(if let Some(state) = &config.supervisor_state {
        OpenCodeSupervisor::with_state_dir(config.opencode_bin.clone(), state.clone())?
    } else {
        OpenCodeSupervisor::host(config.opencode_bin.clone())?
    });
    if supervisor.live_server_pid()?.is_none() {
        return Ok(Snapshot {
            available: false,
            partial: false,
            truncated: false,
            statuses: Vec::new(),
        });
    }
    // Exactly the dashboard's source/classifier, including native permission,
    // question, failed-turn, child-agent and background-work states. Do not ask
    // ChatGPT to infer these from the conversation copy.
    let source = OpenCodeSource::managed(config.opencode_bin.clone(), supervisor.clone());
    let discovery = source.discover_with_warnings(&DiscoveryRequest {
        include_completed: true,
        include_interactive: true,
        include_external: true,
        history_limit: 5000,
        ..DiscoveryRequest::default()
    })?;
    let mut discovery = SessionSnapshot {
        sessions: discovery.sessions,
        warnings: discovery.warnings,
    };
    // Discovery alone omits the dashboard's holds/native-shell overlay and its
    // persisted judgement that a finished assistant turn is asking the user.
    OpenCodeController::managed(config.opencode_bin.clone(), supervisor).enrich(&mut discovery);
    let assessed = if config.supervisor_state.is_none() {
        crate::turn_classifier::apply_saved(&mut discovery)
    } else {
        Default::default()
    };
    let parents = if config.supervisor_state.is_none() {
        Some(crate::session_tree::ParentLinks::load_default()?)
    } else {
        None
    };
    let paused = if config.supervisor_state.is_none() {
        Some(crate::paused::PausedSessions::load_default()?.paused())
    } else {
        None
    };
    let hidden = if config.supervisor_state.is_none() {
        Some(crate::hidden::HiddenSessions::load_default()?)
    } else {
        None
    };
    let mut statuses = discovery
        .sessions
        .iter()
        .filter(|session| {
            !paused
                .as_ref()
                .is_some_and(|ids| ids.contains_key(&session.id))
        })
        .filter(|session| !hidden.as_ref().is_some_and(|ids| ids.contains(&session.id)))
        .map(|session| {
            let mut status = project(session);
            status.attention_assessed |= assessed.contains(&session.id);
            status.top_level = !parents
                .as_ref()
                .is_some_and(|p| p.links().contains_key(&session.provider_session_id));
            status
        })
        .collect::<Vec<_>>();
    statuses.sort_by_key(|s| {
        (
            if s.status == "needs_you" {
                0
            } else if s.status == "working" {
                1
            } else {
                2
            },
            std::cmp::Reverse(s.updated_at),
        )
    });
    let truncated = statuses.len() > 1000;
    let partial =
        !discovery.warnings.is_empty() || statuses.iter().skip(1000).any(|s| s.status != "done");
    // Keep the durable wake snapshot small; never drop attention states merely
    // to make room for old completed history. Search still covers all history.
    statuses.truncate(1000);
    let db = super::source::open(&config.database)?;
    let mut latest=db.prepare("SELECT m.id FROM session_message m JOIN session_v2 s ON s.id=m.session_id WHERE s.id=?1 AND m.type='user' AND m.seq<COALESCE((SELECT b.seq FROM session_message b WHERE b.session_id=s.id AND b.id=json_extract(s.revert,'$.messageID')),9007199254740991) ORDER BY m.seq DESC LIMIT 1")?;
    for status in &mut statuses {
        status.last_user_message_id = latest
            .query_row([&status.session_id], |row| row.get(0))
            .optional()?;
    }
    Ok(Snapshot {
        available: true,
        partial,
        truncated,
        statuses,
    })
}
