//! Where Voice Mode keeps its OpenAI key and settings.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// The port the voice page is served on. Fixed, so the browser remembers the
/// microphone permission for this address.
pub const DEFAULT_PORT: u16 = 47617;

/// The port to serve on; `AGENTVIEW_VOICE_PORT` lets a test run beside a real dashboard.
pub fn port() -> u16 {
    std::env::var("AGENTVIEW_VOICE_PORT")
        .ok()
        .and_then(|port| port.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

pub(super) fn state_dir() -> Result<PathBuf> {
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

/// Which company's voice API a call uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Provider {
    #[default]
    OpenAi,
    Xai,
}

impl Provider {
    /// "openai", "xai" or "grok" (any case).
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_lowercase().as_str() {
            "openai" | "gpt" | "gpt-live" => Some(Self::OpenAi),
            "xai" | "x.ai" | "grok" => Some(Self::Xai),
            _ => None,
        }
    }

    /// What is written in `settings.json`.
    pub fn id(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Xai => "xai",
        }
    }

    /// What the person sees.
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI",
            Self::Xai => "Grok",
        }
    }

    /// Whether the call can change how fast the voice talks.
    pub fn has_speed(self) -> bool {
        self == Self::Xai
    }

    pub fn other(self) -> Self {
        match self {
            Self::OpenAi => Self::Xai,
            Self::Xai => Self::OpenAi,
        }
    }
}

/// The range Grok accepts for `audio.output.speed`.
pub const SPEED_MIN: f64 = 0.7;
pub const SPEED_MAX: f64 = 1.5;
pub const SPEED_STEP: f64 = 0.1;

/// A speed inside what Grok accepts, in steps of a tenth.
pub fn clamp_speed(speed: f64) -> f64 {
    ((speed.clamp(SPEED_MIN, SPEED_MAX)) * 10.0).round() / 10.0
}

#[derive(Default, Serialize, Deserialize)]
struct KeyFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    openai_api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    xai_api_key: Option<String>,
}

fn read_key_file() -> KeyFile {
    state_dir()
        .ok()
        .and_then(|dir| std::fs::read_to_string(dir.join("key.json")).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn clean(key: Option<String>) -> Option<String> {
    key.map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty())
}

/// The key for a provider: the environment, then the file `agentview voice
/// --set-key` or the first-run prompt wrote.
pub fn key_for(provider: Provider) -> Option<String> {
    let names: &[&str] = match provider {
        Provider::OpenAi => &["AGENTVIEW_OPENAI_API_KEY", "OPENAI_API_KEY"],
        Provider::Xai => &["AGENTVIEW_XAI_API_KEY", "XAI_API_KEY"],
    };
    let from_env = names
        .iter()
        .find_map(|name| clean(std::env::var(name).ok()));
    if from_env.is_some() {
        return from_env;
    }
    let file = read_key_file();
    clean(match provider {
        Provider::OpenAi => file.openai_api_key,
        Provider::Xai => file.xai_api_key,
    })
}

/// An OpenAI key with access to GPT-Live.
pub fn api_key() -> Option<String> {
    key_for(Provider::OpenAi)
}

/// Saves an OpenAI key owner-only after checking that OpenAI accepts it.
pub fn save_key(key: &str) -> Result<()> {
    save_key_for(Provider::OpenAi, key)
}

/// Saves a provider's key owner-only after checking that the provider accepts it.
pub fn save_key_for(provider: Provider, key: &str) -> Result<()> {
    let key = key.trim();
    match provider {
        Provider::OpenAi => {
            if !key.starts_with("sk-") || key.len() < 20 || key.chars().any(char::is_whitespace) {
                bail!("that does not look like an OpenAI API key");
            }
            check_key(key)?;
        }
        Provider::Xai => {
            if !key.starts_with("xai-") || key.len() < 20 || key.chars().any(char::is_whitespace) {
                bail!("that does not look like an xAI API key (it starts with xai-)");
            }
            check_xai_key(key)?;
        }
    }
    let mut file = read_key_file();
    match provider {
        Provider::OpenAi => file.openai_api_key = Some(key.to_owned()),
        Provider::Xai => file.xai_api_key = Some(key.to_owned()),
    }
    crate::fs_util::write_private_json(&state_dir()?.join("key.json"), &file)
}

/// One cheap request: xAI must accept the key.
pub fn check_xai_key(key: &str) -> Result<()> {
    match http_status("https://api.x.ai/v1/models", key)?.as_str() {
        "200" => Ok(()),
        "401" | "403" => bail!("xAI rejected that key"),
        "" | "000" => bail!("could not reach xAI to check the key"),
        other => bail!("xAI answered {other} when checking the key"),
    }
}

/// The status code of a GET with the key in an `Authorization` header, sent
/// through curl's stdin so it never appears in the process list.
fn http_status(url: &str, key: &str) -> Result<String> {
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
        .arg(url)
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
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// One cheap request: the key must be accepted and see GPT-Live.
pub fn check_key(key: &str) -> Result<()> {
    match http_status("https://api.openai.com/v1/models/gpt-live-1", key)?.as_str() {
        "200" => Ok(()),
        "401" => bail!("OpenAI rejected that key"),
        "404" => bail!("that key cannot use GPT-Live"),
        "" | "000" => bail!("could not reach OpenAI to check the key"),
        other => bail!("OpenAI answered {other} when checking the key"),
    }
}

/// Optional `voice/settings.json`; every field has a default.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// `openai`, `xai` (Grok), or empty: whichever has a key, OpenAI first.
    pub provider: String,
    /// A Grok voice id (`rex` is male).
    pub xai_voice: String,
    /// `grok-voice-latest` follows xAI's newest voice model.
    pub xai_model: String,
    /// `none` keeps Grok fast; `high` lets it think first.
    pub xai_reasoning: String,
    /// How fast Grok talks, 0.7 to 1.5.
    pub speed: f64,
    pub voice: String,
    pub backend_model: String,
    pub reasoning_effort: String,
    /// Minutes of silence, with nothing watched, before the call ends itself.
    pub idle_minutes: u64,
    /// The longest a call may run.
    pub max_minutes: u64,
    /// `priority` asks OpenAI for its fast tier on the backend model.
    pub service_tier: Option<String>,
    /// Who holds the microphone: `hidden` (a browser agentview starts and stops),
    /// `tab` (a tab in your browser) or `none` (a page is opened some other way).
    pub browser: String,
    /// The Chromium-based browser `hidden` runs, when it is not where agentview looks.
    pub chrome_path: Option<String>,
    /// Seconds the hidden browser stays up after a call, so the next one starts
    /// at once. 0 stops it with the call.
    pub keep_warm_seconds: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            provider: String::new(),
            xai_voice: "rex".into(),
            xai_model: "grok-voice-latest".into(),
            xai_reasoning: "none".into(),
            speed: 1.0,
            voice: "meridian".into(),
            backend_model: "gpt-6-luna".into(),
            reasoning_effort: "low".into(),
            idle_minutes: 10,
            max_minutes: 60,
            service_tier: None,
            browser: "hidden".into(),
            chrome_path: None,
            keep_warm_seconds: 120,
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

    /// The provider a call uses now: the chosen one, else the only one with a
    /// key, else OpenAI.
    pub fn chosen_provider(&self) -> Provider {
        Provider::parse(&self.provider).unwrap_or_else(|| {
            if key_for(Provider::OpenAi).is_none() && key_for(Provider::Xai).is_some() {
                Provider::Xai
            } else {
                Provider::OpenAi
            }
        })
    }
}

/// Changes one field of `settings.json` and keeps the rest as the person wrote it.
pub fn save_setting(name: &str, value: serde_json::Value) -> Result<()> {
    let path = state_dir()?.join("settings.json");
    let mut map: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    map.insert(name.to_owned(), value);
    crate::fs_util::write_private_json(&path, &map)
}

/// Written once the person has started a call, so the footer hint can go.
pub fn hint_seen() -> bool {
    state_dir().is_ok_and(|dir| dir.join("hint-seen").exists())
}

/// Where the hidden browser keeps its profile.
pub fn browser_profile_dir() -> Option<PathBuf> {
    state_dir().ok().map(|dir| dir.join("browser-profile"))
}

pub fn mark_hint_seen() {
    if let Ok(dir) = state_dir() {
        if !dir.exists() && std::fs::create_dir_all(&dir).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            }
        }
        let _ = std::fs::write(dir.join("hint-seen"), b"");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_are_found_by_their_common_names() {
        assert_eq!(Provider::parse("Grok"), Some(Provider::Xai));
        assert_eq!(Provider::parse("xai"), Some(Provider::Xai));
        assert_eq!(Provider::parse(" OpenAI "), Some(Provider::OpenAi));
        assert_eq!(Provider::parse("gemini"), None);
        assert!(Provider::Xai.has_speed() && !Provider::OpenAi.has_speed());
    }

    #[test]
    fn speed_stays_inside_what_grok_accepts_in_tenths() {
        assert_eq!(clamp_speed(0.1), 0.7);
        assert_eq!(clamp_speed(9.0), 1.5);
        assert_eq!(clamp_speed(1.2499), 1.2);
        assert_eq!(clamp_speed(1.26), 1.3);
    }

    #[test]
    fn grok_defaults_are_a_male_voice_without_reasoning() {
        let settings = Settings::default();
        assert_eq!(settings.xai_voice, "rex");
        assert_eq!(settings.xai_reasoning, "none");
        assert_eq!(settings.speed, 1.0);
    }

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
        assert!(save_key_for(Provider::Xai, "sk-this-is-an-openai-key-not-xai").is_err());
        assert!(save_key_for(Provider::Xai, "xai-short").is_err());
    }
}
