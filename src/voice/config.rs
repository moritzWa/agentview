//! Where Voice Mode keeps its OpenAI key and settings.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// The port the voice page is served on. Fixed, so the browser remembers the
/// microphone permission for this address.
pub const DEFAULT_PORT: u16 = 47617;

fn state_dir() -> Result<PathBuf> {
    // Tests and demos keep the key and the hint flag out of the real state.
    if let Some(dir) = crate::fs_util::xdg_home("AGENTVIEW_VOICE_STATE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Some(state_home) = crate::fs_util::xdg_home("XDG_STATE_HOME") {
        return Ok(PathBuf::from(state_home).join("agentview/voice"));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/state/agentview/voice"))
}

#[derive(Serialize, Deserialize)]
struct KeyFile {
    openai_api_key: String,
}

/// An OpenAI key with access to GPT-Live: the environment, then the file
/// `agentview voice --set-key` or the first-run prompt wrote.
pub fn api_key() -> Option<String> {
    let from_env = std::env::var("AGENTVIEW_OPENAI_API_KEY")
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
        .ok()
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty());
    if from_env.is_some() {
        return from_env;
    }
    let text = std::fs::read_to_string(state_dir().ok()?.join("key.json")).ok()?;
    let file: KeyFile = serde_json::from_str(&text).ok()?;
    Some(file.openai_api_key.trim().to_owned()).filter(|key| !key.is_empty())
}

/// Saves the key owner-only after checking that OpenAI accepts it.
pub fn save_key(key: &str) -> Result<()> {
    let key = key.trim();
    if !key.starts_with("sk-") || key.len() < 20 || key.chars().any(char::is_whitespace) {
        bail!("that does not look like an OpenAI API key");
    }
    check_key(key)?;
    crate::fs_util::write_private_json(
        &state_dir()?.join("key.json"),
        &KeyFile {
            openai_api_key: key.to_owned(),
        },
    )
}

/// One cheap request: the key must be accepted and see GPT-Live.
pub fn check_key(key: &str) -> Result<()> {
    use std::io::Write;
    let mut child = Command::new("curl")
        .args([
            "-s",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            "--max-time",
            "15",
            "-H",
            "@-",
        ])
        .arg("https://api.openai.com/v1/models/gpt-live-1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("curl is needed to check the key")?;
    writeln!(
        child.stdin.take().context("no stdin")?,
        "Authorization: Bearer {key}"
    )?;
    let output = child.wait_with_output()?;
    match String::from_utf8_lossy(&output.stdout).trim() {
        "200" => Ok(()),
        "401" => bail!("OpenAI rejected that key"),
        "404" => bail!("that key cannot use GPT-Live"),
        "" | "000" => bail!("could not reach OpenAI to check the key"),
        other => bail!("OpenAI answered {other} when checking the key"),
    }
}

/// Optional `voice/settings.json`; every field has a default.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Settings {
    pub voice: String,
    pub backend_model: String,
    pub reasoning_effort: String,
    /// Minutes of silence, with nothing watched, before the call ends itself.
    pub idle_minutes: u64,
    /// The longest a call may run.
    pub max_minutes: u64,
    /// `priority` asks OpenAI for its fast tier on the backend model.
    pub service_tier: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            voice: "meridian".into(),
            backend_model: "gpt-6-luna".into(),
            reasoning_effort: "low".into(),
            idle_minutes: 10,
            max_minutes: 60,
            service_tier: None,
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        state_dir()
            .ok()
            .and_then(|dir| std::fs::read_to_string(dir.join("settings.json")).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }
}

/// Written once the person has started a call, so the footer hint can go.
pub fn hint_seen() -> bool {
    state_dir().is_ok_and(|dir| dir.join("hint-seen").exists())
}

pub fn mark_hint_seen() {
    if let Ok(dir) = state_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("hint-seen"), b"");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_fill_in_what_the_file_leaves_out() {
        let settings: Settings =
            serde_json::from_str(r#"{"voice":"vesper","idle_minutes":3}"#).unwrap();
        assert_eq!(settings.voice, "vesper");
        assert_eq!(settings.idle_minutes, 3);
        assert_eq!(settings.backend_model, "gpt-6-luna");
        assert_eq!(settings.max_minutes, 60);
        assert_eq!(Settings::default().voice, "meridian");
    }

    #[test]
    fn a_key_that_is_not_one_is_refused_before_any_request() {
        assert!(save_key("hello").is_err());
        assert!(save_key("sk-short").is_err());
        assert!(save_key("sk-aaaaaaaaaaaaaaaaaaaa bbbb").is_err());
    }
}
