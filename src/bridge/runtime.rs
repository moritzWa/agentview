use super::Config;
use crate::adapters::{DiscoveryRequest, OpenCodeController, OpenCodeSource, SessionSource};
use crate::control::ProviderController;
use crate::domain::{AgentSession, SessionSnapshot, SessionState};
use crate::opencode_supervisor::OpenCodeSupervisor;
use anyhow::{Context, Result};
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
    directory: Option<String>,
    health: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    turn_started_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_progress_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_progress_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    in_flight: Option<&'static str>,
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
        directory: (!session.cwd.as_os_str().is_empty())
            .then(|| session.cwd.to_string_lossy().into_owned()),
        // Working stays unknown until the source read. A missing read is not a stall.
        health: if reason == Some("failed") {
            "failed"
        } else if status == "working" || status == "unknown" {
            "unknown"
        } else {
            "idle"
        },
        turn_started_at: None,
        last_progress_at: None,
        last_progress_kind: None,
        in_flight: None,
    }
}

pub(super) fn snapshot(config: &Config) -> Result<Snapshot> {
    let supervisor = Arc::new(if let Some(state) = &config.supervisor_state {
        OpenCodeSupervisor::with_state_dir(config.opencode_bin.clone(), state.clone())
            .context("runtime snapshot: supervisor")?
    } else {
        OpenCodeSupervisor::host(config.opencode_bin.clone())
            .context("runtime snapshot: supervisor")?
    });
    if supervisor
        .live_server_pid()
        .context("runtime snapshot: server_check")?
        .is_none()
    {
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
    let discovery = source
        .discover_with_warnings(&DiscoveryRequest {
            include_completed: true,
            include_interactive: true,
            include_external: true,
            history_limit: 5000,
            ..DiscoveryRequest::default()
        })
        .context("runtime snapshot: discover")?;
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
        Some(
            crate::session_tree::ParentLinks::load_default()
                .context("runtime snapshot: registries")?,
        )
    } else {
        None
    };
    let paused = if config.supervisor_state.is_none() {
        Some(
            crate::paused::PausedSessions::load_default()
                .context("runtime snapshot: registries")?
                .paused(),
        )
    } else {
        None
    };
    let hidden = if config.supervisor_state.is_none() {
        Some(
            crate::hidden::HiddenSessions::load_default()
                .context("runtime snapshot: registries")?,
        )
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
    let count_truncated = statuses.len() > 1000;
    let mut partial =
        !discovery.warnings.is_empty() || statuses.iter().skip(1000).any(|s| s.status != "done");
    // Keep the durable wake snapshot small; never drop attention states merely
    // to make room for old completed history. Search still covers all history.
    statuses.truncate(1000);
    let db = super::source::open(&config.database).context("runtime snapshot: source_open")?;
    let mut latest=db.prepare("SELECT m.id FROM session_message m JOIN session_v2 s ON s.id=m.session_id WHERE s.id=?1 AND m.type='user' AND m.seq<COALESCE((SELECT b.seq FROM session_message b WHERE b.session_id=s.id AND b.id=json_extract(s.revert,'$.messageID')),9007199254740991) ORDER BY m.seq DESC LIMIT 1").context("runtime snapshot: last_user")?;
    for status in &mut statuses {
        status.last_user_message_id = latest
            .query_row([&status.session_id], |row| row.get(0))
            .optional()
            .context("runtime snapshot: last_user")?;
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let working = statuses
        .iter()
        .filter(|status| status.status == "working")
        .map(|status| status.session_id.clone())
        .collect::<Vec<_>>();
    match super::health::progress(&db, &working, now_ms) {
        Ok(found) => {
            for status in &mut statuses {
                if let Some(progress) = found.get(&status.session_id) {
                    status.health = progress.health;
                    status.turn_started_at = progress.turn_started_at;
                    status.last_progress_at = progress.last_progress_at;
                    status.last_progress_kind = progress.last_progress_kind;
                    status.in_flight = progress.in_flight;
                }
            }
        }
        Err(_) => {
            // Leave working sessions unknown. Do not publish a stall from a failed read.
        }
    }
    // Full directory metadata can make a count-bounded snapshot too large.
    // Keep its serialized body safely inside the existing 850 KB transport cap.
    let mut bytes = 1024;
    let mut keep = 0;
    for status in &statuses {
        let size = serde_json::to_vec(status)?.len() + 1;
        if bytes + size > 750_000 {
            break;
        }
        bytes += size;
        keep += 1;
    }
    let truncated = count_truncated || keep < statuses.len();
    partial |= statuses.iter().skip(keep).any(|s| s.status != "done");
    statuses.truncate(keep);
    Ok(Snapshot {
        available: true,
        partial,
        truncated,
        statuses,
    })
}
