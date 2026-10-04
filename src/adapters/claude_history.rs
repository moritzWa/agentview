//! Claude Code sessions that have ended, read from the transcripts Claude
//! keeps under `~/.claude/projects/<folder>/<session id>.jsonl`.
//! `claude agents` lists a terminal session only while its process runs, so
//! without these a session leaves the dashboard as soon as it exits.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde_json::Value;

use crate::domain::{AgentSession, Provider, Runtime, SessionKind, SessionState};

/// The folder, kind, and first prompt are written at the start of a
/// transcript; titles and the latest prompt are appended near its end.
/// Reading only these keeps a refresh fast with multi-megabyte transcripts.
const HEAD_BYTES: u64 = 64 * 1024;
const TAIL_BYTES: u64 = 256 * 1024;

/// Marks rows read from a transcript rather than from `claude agents`, which
/// open with `claude --resume` because no process runs them.
pub(crate) const HISTORY_RAW_STATE: &str = "history";

#[derive(Clone, Debug, PartialEq, Eq)]
struct Transcript {
    cwd: PathBuf,
    background: bool,
    title: Option<String>,
    first_prompt: Option<String>,
    last_prompt: Option<String>,
}

/// Parsed transcripts by path, kept while their size and write time are
/// unchanged so a refresh rereads only the ones that grew.
type Cache = BTreeMap<PathBuf, (u64, SystemTime, Option<Transcript>)>;

pub(super) struct ClaudeHistory {
    projects: PathBuf,
    cache: Mutex<Cache>,
}

impl ClaudeHistory {
    /// Claude's own transcripts: `$CLAUDE_CONFIG_DIR/projects`, else
    /// `~/.claude/projects`.
    pub(super) fn host() -> Option<Self> {
        let config = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))?;
        Some(Self::at(config.join("projects")))
    }

    pub(super) fn at(projects: PathBuf) -> Self {
        Self {
            projects,
            cache: Mutex::new(BTreeMap::new()),
        }
    }

    /// The `limit` most recently written sessions, newest first.
    pub(super) fn sessions(&self, limit: usize) -> Vec<AgentSession> {
        let mut files = transcript_files(&self.projects);
        files.sort_by_key(|a| std::cmp::Reverse(a.2));
        let Ok(mut cache) = self.cache.lock() else {
            return Vec::new();
        };
        let mut seen = BTreeSet::new();
        let mut sessions = Vec::new();
        for (path, len, modified, created) in files {
            if sessions.len() >= limit {
                break;
            }
            let transcript = match cache.get(&path) {
                Some((cached_len, cached_modified, transcript))
                    if *cached_len == len && *cached_modified == modified =>
                {
                    transcript.clone()
                }
                _ => {
                    let transcript = read_transcript(&path, len);
                    cache.insert(path.clone(), (len, modified, transcript.clone()));
                    transcript
                }
            };
            let (Some(transcript), Some(id)) =
                (transcript, path.file_stem().and_then(|stem| stem.to_str()))
            else {
                continue;
            };
            if seen.insert(id.to_owned()) {
                sessions.push(session(id, transcript, created, modified));
            }
        }
        sessions
    }
}

/// Top-level transcripts only: subagent transcripts live in a folder named
/// after their parent session.
fn transcript_files(projects: &Path) -> Vec<(PathBuf, u64, SystemTime, Option<SystemTime>)> {
    let Ok(folders) = fs::read_dir(projects) else {
        return Vec::new();
    };
    folders
        .flatten()
        .filter(|folder| folder.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|folder| fs::read_dir(folder.path()).ok())
        .flat_map(|entries| entries.flatten())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            metadata.is_file().then(|| {
                (
                    entry.path(),
                    metadata.len(),
                    metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    metadata.created().ok(),
                )
            })
        })
        .collect()
}

fn read_transcript(path: &Path, len: u64) -> Option<Transcript> {
    let mut file = File::open(path).ok()?;
    let head = read_lines(&mut file, 0, HEAD_BYTES.min(len), len)?;
    let tail_start = len.saturating_sub(TAIL_BYTES);
    let tail = if tail_start < HEAD_BYTES.min(len) {
        head.clone()
    } else {
        read_lines(&mut file, tail_start, len - tail_start, len)?
    };
    let mut cwd = None;
    let mut background = false;
    let mut first_prompt = None;
    for record in &head {
        if cwd.is_none() {
            cwd = record.get("cwd").and_then(Value::as_str).map(PathBuf::from);
        }
        background |= record.get("sessionKind").and_then(Value::as_str) == Some("bg");
        if first_prompt.is_none() {
            first_prompt = typed_prompt(record);
        }
    }
    let (mut custom, mut agent, mut ai, mut last_prompt) = (None, None, None, None);
    for record in &tail {
        let text = |key| record.get(key).and_then(Value::as_str).map(str::to_owned);
        match record.get("type").and_then(Value::as_str) {
            Some("custom-title") => custom = text("customTitle"),
            Some("agent-name") => agent = text("agentName"),
            Some("ai-title") => ai = text("aiTitle"),
            Some("last-prompt") => last_prompt = text("lastPrompt"),
            _ => {}
        }
    }
    // A transcript without a prompt is a session closed before its first
    // message.
    first_prompt.as_ref().or(last_prompt.as_ref())?;
    Some(Transcript {
        cwd: cwd?,
        background,
        title: custom
            .or(agent)
            .or(ai)
            .filter(|title| !title.trim().is_empty()),
        first_prompt,
        last_prompt,
    })
}

/// The JSON records among `length` bytes at `start`, minus a line cut off at
/// either end of the window.
fn read_lines(file: &mut File, start: u64, length: u64, len: u64) -> Option<Vec<Value>> {
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(length).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.split('\n').collect::<Vec<_>>();
    if start + length < len {
        lines.pop();
    }
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    Some(
        lines
            .into_iter()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect(),
    )
}

/// A prompt the user typed, skipping tool results, injected context, and
/// slash-command records, which Claude wraps in tags.
fn typed_prompt(record: &Value) -> Option<String> {
    if record.get("type").and_then(Value::as_str) != Some("user")
        || record.get("isMeta").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let content = record.pointer("/message/content")?;
    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => return None,
    };
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty() && !text.starts_with('<')).then_some(text)
}

fn session(
    id: &str,
    transcript: Transcript,
    created: Option<SystemTime>,
    modified: SystemTime,
) -> AgentSession {
    let name = transcript
        .title
        .clone()
        .or_else(|| {
            transcript
                .first_prompt
                .as_deref()
                .map(|prompt| truncate(prompt, 60))
        })
        .unwrap_or_else(|| format!("claude-{}", id.chars().take(8).collect::<String>()));
    let summary = transcript
        .last_prompt
        .as_deref()
        .or(transcript.first_prompt.as_deref())
        .map(|prompt| truncate(prompt, 160))
        .unwrap_or_default();
    AgentSession {
        id: format!("claude:host:{id}"),
        provider_session_id: id.to_owned(),
        provider: Provider::Claude,
        runtime: Runtime::Host,
        kind: if transcript.background {
            SessionKind::Background
        } else {
            SessionKind::Interactive
        },
        name,
        cwd: transcript.cwd,
        state: SessionState::Completed,
        summary,
        raw_state: Some(HISTORY_RAW_STATE.into()),
        pid: None,
        started_at: created,
        updated_at: Some(modified),
        pull_requests: None,
        capabilities: BTreeSet::new(),
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut cut = text.chars().take(max_chars - 1).collect::<String>();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, id: &str, lines: &[Value]) {
        let text = lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(dir.join(format!("{id}.jsonl")), text + "\n").unwrap();
    }

    fn user(text: &str) -> Value {
        serde_json::json!({
            "type": "user", "cwd": "/work/repo", "sessionId": "s",
            "message": {"role": "user", "content": text}
        })
    }

    #[test]
    fn ended_sessions_list_newest_first_with_titles_and_prompts() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("-work-repo");
        fs::create_dir(&folder).unwrap();
        write(
            &folder,
            "older",
            &[
                serde_json::json!({"type": "permission-mode", "permissionMode": "default"}),
                serde_json::json!({"type": "user", "isMeta": true, "cwd": "/work/repo",
                    "message": {"content": "<local-command-caveat>ignored</local-command-caveat>"}}),
                user("Fix the flaky login test"),
                serde_json::json!({"type": "ai-title", "aiTitle": "flaky login test"}),
                serde_json::json!({"type": "last-prompt", "lastPrompt": "now push it"}),
            ],
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(&folder, "newer", &[user("Explain the build")]);
        write(
            &folder,
            "empty",
            &[serde_json::json!({"type": "mode", "mode": "normal"})],
        );
        fs::create_dir(folder.join("newer")).unwrap();
        write(&folder.join("newer"), "subagent", &[user("sidechain")]);

        let sessions = ClaudeHistory::at(root.path().to_owned()).sessions(10);

        let rows = sessions
            .iter()
            .map(|s| {
                (
                    s.provider_session_id.as_str(),
                    s.name.as_str(),
                    s.summary.as_str(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            vec![
                ("newer", "Explain the build", "Explain the build"),
                ("older", "flaky login test", "now push it"),
            ]
        );
        assert_eq!(sessions[1].cwd, PathBuf::from("/work/repo"));
        assert_eq!(sessions[1].state, SessionState::Completed);
        assert_eq!(sessions[1].kind, SessionKind::Interactive);
        assert_eq!(sessions[1].raw_state.as_deref(), Some(HISTORY_RAW_STATE));
    }

    #[test]
    fn a_long_transcript_reads_its_title_from_the_tail() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("-work-repo");
        fs::create_dir(&folder).unwrap();
        let mut lines = vec![user("Start a big refactor")];
        let filler = "x".repeat(10_000);
        lines.extend((0..60).map(|_| serde_json::json!({"type": "assistant", "text": filler})));
        lines.push(serde_json::json!({"type": "agent-name", "agentName": "big refactor"}));
        write(&folder, "long", &lines);

        let history = ClaudeHistory::at(root.path().to_owned());
        let sessions = history.sessions(10);

        assert_eq!(sessions[0].name, "big refactor");
        assert_eq!(sessions[0].summary, "Start a big refactor");
        assert_eq!(history.sessions(10), sessions);
    }
}
