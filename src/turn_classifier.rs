//! Separates finished turns from turns that wait on the user.
//!
//! Providers report an idle session either as completed or as waiting at its
//! prompt, without saying whether the agent's last message asked for anything.
//! When an OpenRouter key is configured, the transcript tail of each recently
//! idle session is labelled once by a small hosted model. The verdict is cached
//! until the session's summary changes, so a turn costs at most one request.
//! Verdicts are kept on disk so a restarted dashboard shows them at once.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
    store: Option<Arc<PathBuf>>,
}

#[derive(Default)]
struct Cache {
    /// Session ID to the turn it was labelled for and the label.
    verdicts: BTreeMap<String, Labelled>,
    in_flight: BTreeSet<String>,
    /// A turn whose request failed is not retried until the turn changes.
    failed: BTreeMap<String, u64>,
    last_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Labelled {
    turn: u64,
    verdict: Verdict,
    labelled_at_ms: u64,
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
            store: None,
        }
    }

    /// Keep verdicts in `path`, starting from the ones already there.
    pub fn with_store(mut self, path: PathBuf) -> Self {
        if let Ok(mut cache) = self.cache.lock() {
            cache.verdicts = read_store(&path);
        }
        self.store = Some(Arc::new(path));
        self
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
        let classifier = Self::new(
            move |session| control.inspect(session),
            move |tail| ask_openrouter(&key, &model, tail),
        );
        Some(match default_store_path() {
            Some(path) => classifier.with_store(path),
            None => classifier,
        })
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
                Some(labelled) if labelled.turn == turn => {
                    session.state = match labelled.verdict {
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
                let result = crate::last_message::get(&session.id)
                    .map_or_else(|| (classifier.inspect)(&session), Ok)
                    .and_then(|transcript| (classifier.ask)(tail(&transcript, TAIL_CHARS)))
                    .map(|verdict| Labelled {
                        turn,
                        verdict,
                        labelled_at_ms: now_ms(),
                    });
                if let (Ok(labelled), Some(path)) = (&result, &classifier.store) {
                    let _ = record_in_store(path, &session.id, *labelled);
                }
                let Ok(mut cache) = classifier.cache.lock() else {
                    return;
                };
                cache.in_flight.remove(&session.id);
                match result {
                    Ok(labelled) => {
                        cache.failed.remove(&session.id);
                        cache.last_error = None;
                        cache.verdicts.insert(session.id, labelled);
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
        && (session.capabilities.contains(&Capability::Inspect)
            || crate::last_message::get(&session.id).is_some())
        && !session.summary.trim().is_empty()
        && session.age(now).is_some_and(|age| age <= RECENT)
}

/// Some summaries stay the thread's first prompt across turns, so the latest
/// reply identifies the turn too. FNV-1a, because stored verdicts must match
/// across builds and `DefaultHasher` is free to change between Rust releases.
fn turn_hash(session: &AgentSession) -> u64 {
    let reply = crate::last_message::get(&session.id).unwrap_or_default();
    session
        .summary
        .bytes()
        .chain([0])
        .chain(reply.bytes())
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

fn default_store_path() -> Option<PathBuf> {
    let state_home = crate::fs_util::xdg_home("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    Some(state_home.join("agentview/turn-verdicts.json"))
}

/// Verdicts for turns young enough to still be classified; older ones would
/// never be looked up again.
fn read_store(path: &Path) -> BTreeMap<String, Labelled> {
    let cutoff = now_ms().saturating_sub(RECENT.as_millis() as u64);
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<BTreeMap<String, Labelled>>(&bytes).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, labelled)| labelled.labelled_at_ms >= cutoff)
        .collect()
}

/// Merge one verdict into the file, which other dashboards may be writing too.
fn record_in_store(path: &Path, session_id: &str, labelled: Labelled) -> Result<()> {
    static WRITING: Mutex<()> = Mutex::new(());
    let _writing = WRITING.lock();
    let mut verdicts = read_store(path);
    verdicts.insert(session_id.to_owned(), labelled);
    crate::fs_util::write_private_json(path, &verdicts)
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
    let config_home = crate::fs_util::xdg_home("XDG_CONFIG_HOME")
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
    fn a_restarted_dashboard_reuses_stored_verdicts() {
        let dir = std::env::temp_dir().join(format!("av-verdicts-{}", std::process::id()));
        let path = dir.join("turn-verdicts.json");
        let _ = std::fs::remove_dir_all(&dir);
        let asked = Arc::new(AtomicUsize::new(0));
        let classifier_asking = |asked: &Arc<AtomicUsize>| {
            let counter = Arc::clone(asked);
            TurnClassifier::new(
                |session| Ok(session.summary.clone()),
                move |_| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(Verdict::NeedsInput)
                },
            )
            .with_store(path.clone())
        };
        let session = || snapshot(vec![idle("one", SessionState::Completed, "", "Which?")]);

        let first = classifier_asking(&asked);
        first.apply(&mut session());
        first.wait_for_idle();

        let restarted = classifier_asking(&asked);
        let mut shown = session();
        restarted.apply(&mut shown);
        assert_eq!(shown.sessions[0].state, SessionState::NeedsInput);
        restarted.wait_for_idle();
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_recorded_last_message_is_labelled_without_transcript_access() {
        let classifier = TurnClassifier::new(
            |_| bail!("no transcript access"),
            |tail| {
                Ok(if tail.ends_with("Which branch?") {
                    Verdict::NeedsInput
                } else {
                    Verdict::Done
                })
            },
        );
        let mut session = idle(
            "codex:host:no-inspect",
            SessionState::Completed,
            "idle",
            "I looked",
        );
        session.capabilities.clear();
        crate::last_message::remember(&session.id, "I looked at both. Which branch?");
        let next = || snapshot(vec![session.clone()]);
        classifier.apply(&mut next());
        classifier.wait_for_idle();
        let mut shown = next();
        classifier.apply(&mut shown);
        assert_eq!(shown.sessions[0].state, SessionState::NeedsInput);
        assert!(shown.warnings.is_empty());
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
