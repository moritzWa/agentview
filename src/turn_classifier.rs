//! Separates finished turns from turns that wait on the user.
//!
//! Providers report an idle session either as completed or as waiting at its
//! prompt, without saying whether the agent's last message asked for anything.
//! When an OpenRouter key is configured, the transcript tail of each recently
//! idle session is labelled once by a small hosted model. The verdict is cached
//! until the session's summary changes, so a turn costs at most one request.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::control::ControlHub;
use crate::domain::{AgentSession, Capability, Provider, SessionSnapshot, SessionState};

const DEFAULT_MODEL: &str = "google/gemini-2.5-flash-lite";
const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";
/// Older idle sessions keep their provider state rather than spending requests.
const RECENT: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_IN_FLIGHT: usize = 4;
const TAIL_CHARS: usize = 2_000;

const SYSTEM_PROMPT: &str =
    "You label a coding agent's latest turn. Reply with exactly one word.\n\
INPUT: the agent's last message waits on the user: it asks a question, asks for a choice, \
approval, credentials, or clarification, or says it is blocked.\n\
DONE: the agent finished or reported results without needing anything; optional offers \
like 'let me know if you want more' are DONE.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Verdict {
    NeedsInput,
    Done,
}

type Inspect = dyn Fn(&AgentSession) -> Result<String> + Send + Sync;
type Ask = dyn Fn(&str) -> Result<Verdict> + Send + Sync;

#[derive(Clone)]
pub struct TurnClassifier {
    inspect: Arc<Inspect>,
    ask: Arc<Ask>,
    cache: Arc<Mutex<Cache>>,
}

#[derive(Default)]
struct Cache {
    /// Session ID to the turn it was labelled for and the label.
    verdicts: BTreeMap<String, (u64, Verdict)>,
    in_flight: BTreeSet<String>,
    /// A turn whose request failed is not retried until the turn changes.
    failed: BTreeMap<String, u64>,
    last_error: Option<String>,
}

impl TurnClassifier {
    pub fn new(
        inspect: impl Fn(&AgentSession) -> Result<String> + Send + Sync + 'static,
        ask: impl Fn(&str) -> Result<Verdict> + Send + Sync + 'static,
    ) -> Self {
        Self {
            inspect: Arc::new(inspect),
            ask: Arc::new(ask),
            cache: Arc::new(Mutex::new(Cache::default())),
        }
    }

    /// A classifier backed by OpenRouter, or `None` when no key is configured,
    /// provider I/O is disabled, or `AGENTVIEW_TURN_CLASSIFIER=off`.
    pub fn from_env(control: &ControlHub) -> Option<Self> {
        if !control.provider_io_enabled()
            || std::env::var("AGENTVIEW_TURN_CLASSIFIER").is_ok_and(|value| value == "off")
        {
            return None;
        }
        let key = openrouter_api_key()?;
        let model = std::env::var("AGENTVIEW_TURN_CLASSIFIER_MODEL")
            .ok()
            .filter(|model| !model.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.into());
        let control = control.clone();
        Some(Self::new(
            move |session| control.inspect(session),
            move |tail| ask_openrouter(&key, &model, tail),
        ))
    }

    /// Apply cached verdicts to idle sessions and start labelling new turns in
    /// the background; their verdicts land on a later refresh.
    pub fn apply(&self, snapshot: &mut SessionSnapshot) {
        let now = SystemTime::now();
        let Ok(mut cache) = self.cache.lock() else {
            return;
        };
        let mut pending = Vec::new();
        for session in &mut snapshot.sessions {
            if !eligible(session, now) {
                continue;
            }
            let turn = turn_hash(session);
            match cache.verdicts.get(&session.id) {
                Some((labelled, verdict)) if *labelled == turn => {
                    session.state = match verdict {
                        Verdict::NeedsInput => SessionState::NeedsInput,
                        Verdict::Done => SessionState::Completed,
                    };
                }
                _ if cache.in_flight.contains(&session.id)
                    || cache.failed.get(&session.id) == Some(&turn) => {}
                _ if cache.in_flight.len() + pending.len() < MAX_IN_FLIGHT => {
                    pending.push((session.clone(), turn));
                }
                _ => {}
            }
        }
        if let Some(error) = &cache.last_error {
            snapshot
                .warnings
                .push(format!("needs-input classifier: {error}"));
        }
        for (session, _) in &pending {
            cache.in_flight.insert(session.id.clone());
        }
        drop(cache);

        for (session, turn) in pending {
            let classifier = self.clone();
            thread::spawn(move || {
                let result = (classifier.inspect)(&session)
                    .and_then(|transcript| (classifier.ask)(tail(&transcript, TAIL_CHARS)));
                let Ok(mut cache) = classifier.cache.lock() else {
                    return;
                };
                cache.in_flight.remove(&session.id);
                match result {
                    Ok(verdict) => {
                        cache.failed.remove(&session.id);
                        cache.last_error = None;
                        cache.verdicts.insert(session.id, (turn, verdict));
                    }
                    Err(error) => {
                        cache.last_error = Some(format!("{error:#}"));
                        cache.failed.insert(session.id, turn);
                    }
                }
            });
        }
    }

    #[cfg(test)]
    fn wait_for_idle(&self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !self.cache.lock().unwrap().in_flight.is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "classifier never settled"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Idle sessions whose provider cannot tell a finished turn from a question.
/// Permission and question prompts are already known to need input.
fn eligible(session: &AgentSession, now: SystemTime) -> bool {
    let idle = match session.state {
        SessionState::Completed => true,
        SessionState::NeedsInput => session.raw_state.as_deref() == Some("waiting at prompt"),
        _ => false,
    };
    idle && session.provider != Provider::Terminal
        && session.capabilities.contains(&Capability::Inspect)
        && !session.summary.trim().is_empty()
        && session.age(now).is_some_and(|age| age <= RECENT)
}

fn turn_hash(session: &AgentSession) -> u64 {
    let mut hasher = DefaultHasher::new();
    session.summary.hash(&mut hasher);
    hasher.finish()
}

fn tail(text: &str, max_chars: usize) -> &str {
    let text = text.trim_end();
    match max_chars
        .checked_sub(1)
        .and_then(|last| text.char_indices().rev().nth(last))
    {
        Some((index, _)) => &text[index..],
        None => text,
    }
}

fn openrouter_api_key() -> Option<String> {
    if let Some(key) = std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
    {
        return Some(key.trim().to_owned());
    }
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    let key = std::fs::read_to_string(config_home.join("agentview/openrouter-api-key")).ok()?;
    let key = key.trim();
    (!key.is_empty()).then(|| key.to_owned())
}

fn ask_openrouter(key: &str, model: &str, transcript_tail: &str) -> Result<Verdict> {
    let body = json!({
        "model": model,
        "temperature": 0,
        "max_tokens": 16,
        "reasoning": { "enabled": false },
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            {
                "role": "user",
                "content": format!(
                    "Agent transcript tail:\n<<<\n{transcript_tail}\n>>>\nDoes the agent wait on the user? Answer INPUT or DONE."
                ),
            },
        ],
    });
    // The key travels on stdin so it never appears in the process list.
    let config = format!(
        "url = \"{ENDPOINT}\"\nheader = \"Authorization: Bearer {}\"\nheader = \"Content-Type: application/json\"\nheader = \"X-Title: agentview\"\ndata-binary = \"{}\"\n",
        curl_quote(key),
        curl_quote(&body.to_string()),
    );
    let mut child = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--max-time",
            "20",
            "--config",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start curl")?;
    child
        .stdin
        .take()
        .context("curl stdin was unavailable")?
        .write_all(config.as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "OpenRouter request failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    parse_verdict(&output.stdout)
}

fn curl_quote(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn parse_verdict(response: &[u8]) -> Result<Verdict> {
    let response: Value =
        serde_json::from_slice(response).context("OpenRouter returned invalid JSON")?;
    if let Some(error) = response.pointer("/error/message").and_then(Value::as_str) {
        bail!("OpenRouter: {error}");
    }
    let answer = response
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_uppercase();
    if answer.starts_with("INPUT") {
        Ok(Verdict::NeedsInput)
    } else if answer.starts_with("DONE") {
        Ok(Verdict::Done)
    } else {
        bail!("unexpected classifier answer {answer:?}")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::domain::{Runtime, SessionKind};

    fn idle(id: &str, state: SessionState, raw_state: &str, summary: &str) -> AgentSession {
        AgentSession {
            id: id.into(),
            provider_session_id: id.into(),
            provider: Provider::OpenCode,
            runtime: Runtime::Host,
            kind: SessionKind::Background,
            name: id.into(),
            cwd: "/work".into(),
            state,
            summary: summary.into(),
            raw_state: Some(raw_state.into()),
            pid: None,
            started_at: None,
            updated_at: Some(SystemTime::now()),
            pull_requests: None,
            capabilities: BTreeSet::from([Capability::Inspect]),
        }
    }

    fn snapshot(sessions: Vec<AgentSession>) -> SessionSnapshot {
        SessionSnapshot {
            sessions,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn idle_turns_are_split_by_the_model_and_asked_once() {
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&asked);
        let classifier = TurnClassifier::new(
            |session| Ok(session.summary.clone()),
            move |tail| {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(if tail.ends_with('?') {
                    Verdict::NeedsInput
                } else {
                    Verdict::Done
                })
            },
        );
        let sessions = vec![
            idle(
                "ask",
                SessionState::Completed,
                "managed_server",
                "Which one?",
            ),
            idle(
                "held",
                SessionState::NeedsInput,
                "waiting at prompt",
                "All tests pass.",
            ),
            idle(
                "perm",
                SessionState::NeedsInput,
                "permission requested",
                "Run rm?",
            ),
        ];

        let mut first = snapshot(sessions.clone());
        classifier.apply(&mut first);
        assert_eq!(first.sessions[0].state, SessionState::Completed);
        classifier.wait_for_idle();

        for _ in 0..2 {
            let mut next = snapshot(sessions.clone());
            classifier.apply(&mut next);
            let states = next.sessions.iter().map(|s| s.state).collect::<Vec<_>>();
            assert_eq!(
                states,
                [
                    SessionState::NeedsInput,
                    SessionState::Completed,
                    SessionState::NeedsInput,
                ]
            );
        }
        assert_eq!(asked.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_new_turn_is_labelled_again_and_failures_are_not_retried() {
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&asked);
        let classifier = TurnClassifier::new(
            |session| Ok(session.summary.clone()),
            move |tail| {
                counter.fetch_add(1, Ordering::SeqCst);
                if tail == "boom" {
                    bail!("no key")
                }
                Ok(Verdict::NeedsInput)
            },
        );
        for summary in ["boom", "boom", "Ready?", "Ready?"] {
            let mut next = snapshot(vec![idle("one", SessionState::Completed, "", summary)]);
            classifier.apply(&mut next);
            classifier.wait_for_idle();
        }
        let mut last = snapshot(vec![idle("one", SessionState::Completed, "", "Ready?")]);
        classifier.apply(&mut last);
        assert_eq!(last.sessions[0].state, SessionState::NeedsInput);
        assert!(last.warnings.is_empty());
        assert_eq!(asked.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn tail_keeps_the_end_on_a_character_boundary() {
        assert_eq!(tail("héllo wörld", 5), "wörld");
        assert_eq!(tail("short", 50), "short");
    }

    #[test]
    fn answers_and_errors_are_parsed() {
        let answer = |text: &str| {
            parse_verdict(
                json!({ "choices": [{ "message": { "content": text } }] })
                    .to_string()
                    .as_bytes(),
            )
            .ok()
        };
        assert_eq!(answer(" input\n"), Some(Verdict::NeedsInput));
        assert_eq!(answer("DONE."), Some(Verdict::Done));
        assert_eq!(answer("maybe"), None);
        let error = parse_verdict(br#"{"error":{"message":"bad key"}}"#).unwrap_err();
        assert!(error.to_string().contains("bad key"));
    }

    #[test]
    fn curl_config_values_are_escaped() {
        assert_eq!(curl_quote(r#"{"a":"b\nc"}"#), r#"{\"a\":\"b\\nc\"}"#);
    }
}
