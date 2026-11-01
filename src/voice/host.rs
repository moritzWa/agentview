//! The real `VoiceHost`: the dashboard's latest sessions plus the control hub.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use anyhow::{bail, Result};

use super::digest::Info;
use super::tools::{HistoryHit, Turn, VoiceHost};
use crate::adapters::DiscoveryRequest;
use crate::control::{ControlHub, RestorableSession};
use crate::domain::{AgentSession, Capability, Provider, Runtime, SessionKind, SessionState};

pub struct DashboardHost {
    control: ControlHub,
    sessions: Mutex<Vec<AgentSession>>,
    paused: crate::paused::PausedSessions,
    hidden: crate::hidden::HiddenSessions,
    /// Sessions closed during this call, gone from the list before the next refresh.
    closed: Mutex<HashSet<String>>,
    /// Sessions found by searching the history, by row id.
    found: Mutex<HashMap<String, RestorableSession>>,
    parents: Mutex<crate::session_tree::ParentLinks>,
    refresher: Mutex<Option<(SyncSender<DiscoveryRequest>, DiscoveryRequest)>>,
}

impl DashboardHost {
    pub fn new(
        control: ControlHub,
        paused: crate::paused::PausedSessions,
        hidden: crate::hidden::HiddenSessions,
    ) -> Arc<Self> {
        Arc::new(Self {
            control,
            sessions: Mutex::default(),
            paused,
            hidden,
            closed: Mutex::default(),
            found: Mutex::default(),
            parents: Mutex::new(
                crate::session_tree::ParentLinks::load_default().unwrap_or_default(),
            ),
            refresher: Mutex::default(),
        })
    }

    /// How a call asks the refresh worker for fresh sessions when the dashboard
    /// itself is busy in a session's own view.
    pub fn set_refresher(&self, sender: SyncSender<DiscoveryRequest>, request: DiscoveryRequest) {
        *self.refresher.lock().unwrap() = Some((sender, request));
    }

    /// The refresh worker and the dashboard call this whenever sessions refresh.
    pub fn publish(&self, sessions: &[AgentSession]) {
        if let Ok(mut view) = self.sessions.lock() {
            *view = sessions.to_vec();
        }
    }

    /// The session behind a row id. One started a moment ago is not in the
    /// dashboard's list yet, but its supervisor already knows it.
    fn session(&self, id: &str) -> Result<AgentSession> {
        if let Some(session) = self
            .sessions
            .lock()
            .ok()
            .and_then(|sessions| sessions.iter().find(|s| s.id == id).cloned())
        {
            return Ok(session);
        }
        let Some(provider_id) = id.strip_prefix("opencode:host:") else {
            bail!("that session is no longer in the dashboard");
        };
        let found = self
            .found
            .lock()
            .ok()
            .and_then(|found| found.get(id).cloned());
        Ok(AgentSession {
            id: id.to_owned(),
            provider_session_id: provider_id.to_owned(),
            provider: Provider::OpenCode,
            runtime: Runtime::Host,
            kind: SessionKind::Managed,
            name: found
                .as_ref()
                .map_or_else(|| provider_id.to_owned(), |hit| hit.name.clone()),
            cwd: found.map(|hit| hit.cwd).unwrap_or_default(),
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

    /// Stops the running turn, also of a session another session started.
    fn stop(&self, session: &AgentSession) -> Result<()> {
        match self.control.interrupt(session) {
            Ok(_) => Ok(()),
            Err(error) if format!("{error:#}").contains("not created by this supervisor") => {
                self.control.force_interrupt(session)
            }
            Err(error) => Err(error),
        }
    }

    /// The session to stop. Whether it can be is for the supervisor to say:
    /// it checks that the session is its own and that a turn is running.
    fn stoppable(&self, id: &str) -> Result<AgentSession> {
        let mut session = self.controllable(id)?;
        session.capabilities.insert(Capability::Interrupt);
        Ok(session)
    }

    /// The session to act on. One that is only in the history is brought back
    /// under Agent View's control first, as restoring it from the picker does.
    fn controllable(&self, id: &str) -> Result<AgentSession> {
        let in_dashboard = self
            .sessions
            .lock()
            .ok()
            .is_some_and(|sessions| sessions.iter().any(|s| s.id == id));
        if !in_dashboard {
            let found = self
                .found
                .lock()
                .ok()
                .and_then(|found| found.get(id).cloned());
            if let Some(found) = found {
                self.control.adopt(&found)?;
            }
        }
        self.session(id)
    }
}

/// The provider's reason, in words the voice can say.
fn needs_you_because(session: &AgentSession) -> Option<String> {
    if session.state != SessionState::NeedsInput {
        return None;
    }
    Some(
        match session.raw_state.as_deref() {
            Some("turn failed") => {
                "its last turn failed; send it a message such as \"continue\" to retry it"
            }
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
        let Ok(sessions) = self.sessions.lock() else {
            return Vec::new();
        };
        let paused = self.paused.paused();
        let parents = {
            let mut links = self.parents.lock().unwrap();
            links.reload_if_changed();
            links.links().clone()
        };
        let holds = crate::holds::default_holds_dir();
        let closed = self
            .closed
            .lock()
            .map(|closed| closed.clone())
            .unwrap_or_default();
        sessions
            .iter()
            .filter(|session| {
                session.provider == Provider::OpenCode
                    && session.runtime == Runtime::Host
                    && !closed.contains(&session.id)
            })
            .map(|session| {
                let facts = crate::last_message::facts_for(&session.id).unwrap_or_default();
                let subagents = sessions
                    .iter()
                    .filter(|child| {
                        child.state == SessionState::Working
                            && parents.get(&child.provider_session_id)
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
                    paused: paused.contains_key(&session.id),
                    top_level: !parents.contains_key(&session.provider_session_id),
                    can_message: session.capabilities.contains(&Capability::Reply),
                    why: needs_you_because(session),
                }
            })
            .collect()
    }

    fn recent_turns(&self, id: &str, turns: usize) -> Result<Vec<Turn>> {
        let session = self.session(id)?;
        Ok(self
            .control
            .recent_turns(&session, turns)?
            .into_iter()
            .map(|turn| Turn {
                you: turn.you,
                it: turn.it,
            })
            .collect())
    }

    fn search_history(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<(Vec<HistoryHit>, Option<String>)> {
        let words = search_words(query);
        if words.is_empty() {
            bail!("query needs words to look for");
        }
        let (all, errors) = self.control.restorable_sessions();
        let sessions: Vec<RestorableSession> = all
            .into_iter()
            .filter(|session| session.provider == Provider::OpenCode)
            .collect();
        if sessions.is_empty() && !errors.is_empty() {
            bail!("the history could not be read: {}", errors.join("; "));
        }
        let indices = self.control.history_search_indices().unwrap_or_default();
        let mut note = None;
        if indices.is_empty() || indices.iter().any(|index| !index.is_ready()) {
            note = Some(
                "the index of conversations is still being built, so only titles and folders were searched"
                    .to_owned(),
            );
        }
        // Whole phrase first, then each word: the index matches literal text.
        let mut needles: Vec<(String, bool)> = Vec::new();
        if words.len() > 1 {
            needles.push((words.join(" "), true));
        }
        needles.extend(
            words
                .iter()
                .filter(|word| word.chars().count() >= 3)
                .map(|w| (w.clone(), false)),
        );
        let mut content: Vec<(bool, HashMap<String, String>)> = Vec::new();
        for (needle, phrase) in &needles {
            let mut found = HashMap::new();
            for index in &indices {
                if let Ok(rows) = index.search(needle) {
                    found.extend(rows);
                }
            }
            content.push((*phrase, found));
        }
        let mut scored: Vec<(usize, usize, &RestorableSession, String)> = Vec::new();
        for session in &sessions {
            let title = session.name.to_lowercase();
            let folder = session.cwd.to_string_lossy().to_lowercase();
            let mut words_hit = 0;
            let mut score = 0;
            for word in &words {
                let in_title = title.contains(word.as_str());
                let in_folder = folder.contains(word.as_str());
                let in_text = content.iter().any(|(phrase, rows)| {
                    !phrase
                        && needles.iter().any(|(n, p)| !p && n == word)
                        && rows.contains_key(&session.id)
                });
                if in_title || in_folder || in_text {
                    words_hit += 1;
                }
                score +=
                    usize::from(in_title) * 3 + usize::from(in_folder) + usize::from(in_text) * 2;
            }
            if content
                .iter()
                .any(|(phrase, rows)| *phrase && rows.contains_key(&session.id))
            {
                score += 4;
            }
            let needed = words.len().div_ceil(2);
            if words_hit == 0 || (words.len() > 1 && words_hit < needed) {
                continue;
            }
            let snippet = content
                .iter()
                .filter_map(|(_, rows)| rows.get(&session.id))
                .next()
                .cloned()
                .unwrap_or_default();
            scored.push((words_hit, score, session, snippet));
        }
        scored.sort_by(|a, b| (b.0, b.1, b.2.updated_at_ms).cmp(&(a.0, a.1, a.2.updated_at_ms)));
        scored.truncate(limit);
        let mut cache = self.found.lock().unwrap();
        let hits = scored
            .into_iter()
            .map(|(_, _, session, snippet)| {
                cache.insert(session.id.clone(), session.clone());
                HistoryHit {
                    id: session.id.clone(),
                    title: session.name.clone(),
                    cwd: session.cwd.clone(),
                    updated_ms: session.updated_at_ms,
                    snippet,
                }
            })
            .collect();
        Ok((hits, note))
    }

    fn close(&self, id: &str) -> Result<String> {
        let session = self.stoppable(id)?;
        let working = self
            .sessions
            .lock()
            .ok()
            .and_then(|sessions| sessions.iter().find(|s| s.id == id).map(|s| s.state))
            == Some(SessionState::Working);
        let mut done = Vec::new();
        // Whatever the dashboard's list says, ask the server: a session that
        // was hidden before may still be running.
        match self.stop(&session) {
            Ok(()) => done.push("stopped it"),
            // Busy with a background wait or its own helpers, not with a turn.
            Err(error) if format!("{error:#}").contains("not currently working") => {
                done.push("it was between turns, so there was nothing to stop")
            }
            Err(error) if working => bail!(
                "it is still working and could not be stopped, so it was left in Agent View: {error:#}"
            ),
            Err(_) => {}
        }
        self.hidden.hide_session(&session)?;
        if let Ok(mut closed) = self.closed.lock() {
            closed.insert(id.to_owned());
        }
        self.refresh();
        done.push(
            "removed it from Agent View (its history is kept and search_history still finds it)",
        );
        Ok(done.join(" and "))
    }

    fn send(&self, id: &str, text: &str) -> Result<()> {
        let session = self.controllable(id)?;
        self.control.reply(&session, text).map(|_| ())
    }

    fn interrupt(&self, id: &str) -> Result<()> {
        let session = self.stoppable(id)?;
        self.stop(&session)
    }

    fn edit_last(&self, id: &str, text: &str) -> Result<()> {
        let session = self.controllable(id)?;
        self.control.edit_last_message(&session, text).map(|_| ())
    }

    fn refresh(&self) {
        if let Some((sender, request)) = self.refresher.lock().unwrap().as_ref() {
            let _ = sender.try_send(request.clone());
        }
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

/// The words of a spoken search worth looking for.
fn search_words(query: &str) -> Vec<String> {
    const SKIP: [&str; 14] = [
        "the",
        "a",
        "an",
        "of",
        "to",
        "in",
        "on",
        "for",
        "about",
        "that",
        "session",
        "chat",
        "conversation",
        "find",
    ];
    let mut words: Vec<String> = Vec::new();
    for word in query.split_whitespace() {
        let word: String = word
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase();
        if word.chars().count() >= 2 && !SKIP.contains(&word.as_str()) && !words.contains(&word) {
            words.push(word);
        }
        if words.len() == 6 {
            break;
        }
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spoken_searches_keep_the_words_that_name_something() {
        assert_eq!(
            search_words("Find the session about library digitization pilot"),
            ["library", "digitization", "pilot"]
        );
        assert_eq!(search_words("the a of"), Vec::<String>::new());
        assert_eq!(search_words("pilot, pilot!"), ["pilot"]);
    }
}
