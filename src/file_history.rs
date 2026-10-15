//! Read-only conversation text and resume metadata from Claude/Codex JSONL.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Result};
use serde_json::Value;

use crate::domain::Provider;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileProvider {
    Claude,
    Codex,
}

impl FileProvider {
    pub fn provider(self) -> Provider {
        match self {
            Self::Claude => Provider::Claude,
            Self::Codex => Provider::Codex,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    pub fn home(self) -> Option<PathBuf> {
        let (variable, fallback) = match self {
            Self::Claude => ("CLAUDE_CONFIG_DIR", ".claude"),
            Self::Codex => ("CODEX_HOME", ".codex"),
        };
        std::env::var_os(variable).map(PathBuf::from).or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|home| PathBuf::from(home).join(fallback))
        })
    }

    pub fn files(self, home: &Path) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        match self {
            // Exclude nested subagent logs, which are not resumable sessions.
            Self::Claude => walk(&home.join("projects"), 1, &mut files)?,
            Self::Codex => {
                walk(&home.join("sessions"), 4, &mut files)?;
                walk(&home.join("archived_sessions"), 4, &mut files)?;
            }
        }
        files.sort();
        Ok(files)
    }
}

fn walk(root: &Path, depth: usize, files: &mut Vec<PathBuf>) -> Result<()> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() && depth > 0 {
            walk(&entry.path(), depth - 1, files)?;
        } else if kind.is_file() && entry.path().extension().is_some_and(|ext| ext == "jsonl") {
            files.push(entry.path());
        }
    }
    Ok(())
}

#[derive(Debug)]
pub struct FileSession {
    pub id: String,
    pub name: String,
    pub cwd: PathBuf,
}

/// Stream a bounded snapshot of a file. An unfinished final JSON line is
/// ignored; the size/mtime check picks it up once the writer finishes it.
pub fn read_session(
    path: &Path,
    len: u64,
    provider: FileProvider,
    cancelled: &AtomicBool,
    mut on_text: impl FnMut(&str, &str) -> Result<()>,
) -> Result<Option<FileSession>> {
    let file = fs::File::open(path)?;
    let mut reader = BufReader::new(file.take(len));
    let mut line = Vec::new();
    let mut id = match provider {
        FileProvider::Claude => path.file_stem().and_then(|s| s.to_str()).map(str::to_owned),
        FileProvider::Codex => None,
    };
    let mut cwd = None;
    let (mut custom, mut agent, mut ai, mut prompt) = (None, None, None, None);
    loop {
        if cancelled.load(Ordering::Relaxed) {
            bail!("history indexing cancelled");
        }
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        let Ok(record) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let kind = record
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let payload = record.get("payload").unwrap_or(&Value::Null);
        if provider == FileProvider::Codex && kind == "session_meta" {
            id = payload.get("id").and_then(Value::as_str).map(str::to_owned);
            cwd = payload
                .get("cwd")
                .and_then(Value::as_str)
                .map(PathBuf::from);
            custom = payload
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        if provider == FileProvider::Claude {
            if cwd.is_none() {
                cwd = record.get("cwd").and_then(Value::as_str).map(PathBuf::from);
            }
            match kind {
                "custom-title" => {
                    custom = record
                        .get("customTitle")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                }
                "agent-name" => {
                    agent = record
                        .get("agentName")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                }
                "ai-title" => {
                    ai = record
                        .get("aiTitle")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                }
                _ => {}
            }
        }
        let (role, text) = match provider {
            FileProvider::Claude
                if matches!(kind, "user" | "assistant")
                    && record.get("isMeta").and_then(Value::as_bool) != Some(true) =>
            {
                (kind, content_text(record.pointer("/message/content")))
            }
            FileProvider::Codex
                if kind == "response_item"
                    && payload.get("type").and_then(Value::as_str) == Some("message") =>
            {
                (
                    payload
                        .get("role")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    content_text(payload.get("content")),
                )
            }
            FileProvider::Codex if kind == "event_msg" => {
                match payload.get("type").and_then(Value::as_str) {
                    Some("user_message") => (
                        "user",
                        payload
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    ),
                    Some("agent_message") => (
                        "assistant",
                        payload
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    ),
                    _ => continue,
                }
            }
            _ => continue,
        };
        if !matches!(role, "user" | "assistant") || text.trim().is_empty() {
            continue;
        }
        let Some(id) = id.as_deref().filter(|id| valid_id(id)) else {
            continue;
        };
        if role == "user" && prompt.is_none() {
            prompt = Some(
                text.split_whitespace()
                    .take(20)
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        on_text(&format!("{}:host:{id}", provider.key()), &text)?;
    }
    let (Some(id), Some(cwd)) = (
        id.filter(|id| valid_id(id)),
        cwd.filter(|cwd| cwd.is_absolute()),
    ) else {
        return Ok(None);
    };
    let name = custom.or(agent).or(ai).or(prompt).unwrap_or_else(|| {
        format!(
            "{}-{}",
            provider.key(),
            id.chars().take(8).collect::<String>()
        )
    });
    Ok(Some(FileSession {
        id,
        name: name.chars().take(240).collect(),
        cwd,
    }))
}

fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|part| {
                matches!(
                    part.get("type").and_then(Value::as_str),
                    Some("text" | "input_text" | "output_text")
                )
            })
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_history_reads_conversation_and_title_without_tool_noise() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("claude-session.jsonl");
        let cwd = dir.path().join("project");
        let json_cwd = serde_json::to_string(&cwd).unwrap();
        fs::write(&file, concat!(
            "{\"type\":\"user\",\"cwd\":\"/work/project\",\"message\":{\"content\":\"find the launch thread\"}}\n",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"the lighthouse migration is complete\"},{\"type\":\"tool_use\",\"input\":{\"secret\":\"toolnoise\"}}]}}\n",
            "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"content\":\"resultnoise\"}]}}\n",
            "{\"type\":\"user\",\"isMeta\":true,\"message\":{\"content\":\"metanoise\"}}\n",
            "{\"type\":\"custom-title\",\"customTitle\":\"Launch research\"}\n",
            "{\"type\":\"assistant\",\"message\":{\"content\":\"unfinished"
        ).replace("/work/project", &json_cwd[1..json_cwd.len()-1])).unwrap();
        // A streaming writer can stop partway through a UTF-8 character too.
        fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap()
            .write_all(&[0xf0, 0x9f])
            .unwrap();
        let mut texts = Vec::new();
        let session = read_session(
            &file,
            fs::metadata(&file).unwrap().len(),
            FileProvider::Claude,
            &AtomicBool::new(false),
            |id, text| {
                texts.push((id.to_owned(), text.to_owned()));
                Ok(())
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(session.id, "claude-session");
        assert_eq!(session.name, "Launch research");
        assert_eq!(session.cwd, cwd);
        assert_eq!(texts.len(), 2);
        assert!(texts
            .iter()
            .all(|(id, _)| id == "claude:host:claude-session"));
        assert!(texts[1].1.contains("lighthouse"));
    }

    #[test]
    fn codex_history_reads_response_items_and_legacy_message_events() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rollout.jsonl");
        let cwd = dir.path().join("project");
        let json_cwd = serde_json::to_string(&cwd).unwrap();
        fs::write(&file, concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"codex-session\",\"cwd\":\"/work/project\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"Investigate meridian launches\"}]}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"the meridian trace passed\"}]}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call_output\",\"output\":\"toolnoise\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"developer\",\"content\":[{\"type\":\"input_text\",\"text\":\"systemnoise\"}]}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_reasoning\",\"text\":\"reasoningnoise\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"legacy final answer\"}}\n"
        ).replace("/work/project", &json_cwd[1..json_cwd.len()-1])).unwrap();
        let mut texts = Vec::new();
        let session = read_session(
            &file,
            fs::metadata(&file).unwrap().len(),
            FileProvider::Codex,
            &AtomicBool::new(false),
            |id, text| {
                texts.push((id.to_owned(), text.to_owned()));
                Ok(())
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(session.id, "codex-session");
        assert_eq!(session.name, "Investigate meridian launches");
        assert_eq!(texts.len(), 3);
        assert!(texts.iter().all(|(id, _)| id == "codex:host:codex-session"));
    }

    #[test]
    fn history_enumeration_excludes_claude_subagents_and_includes_codex_archives() {
        let dir = tempfile::tempdir().unwrap();
        for path in [
            "projects/project/root.jsonl",
            "projects/project/root/subagents/agent.jsonl",
            "sessions/2026/10/15/rollout.jsonl",
            "archived_sessions/archive.jsonl",
        ] {
            let path = dir.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        assert_eq!(FileProvider::Claude.files(dir.path()).unwrap().len(), 1);
        assert_eq!(FileProvider::Codex.files(dir.path()).unwrap().len(), 2);
    }
}
