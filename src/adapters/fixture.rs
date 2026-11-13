use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Deserialize;

use super::{DiscoveryRequest, SessionSource};
use crate::control::RestorableSession;
use crate::domain::{AgentSession, LaunchTarget, Provider, SessionSnapshot};
use crate::model_catalog::ModelLabels;

/// What a fixture's optional `demo` section stands in for, so a recording
/// can show the composer, model picker, usage header and restore search
/// without touching a real harness, login or network.
#[derive(Debug, Default, Deserialize)]
pub struct FixtureDemo {
    /// Harnesses the composer offers, with the models their picker lists.
    #[serde(default)]
    pub launch: Vec<FixtureLaunch>,
    #[serde(default)]
    pub usage: Vec<FixtureUsage>,
    /// Past sessions the restore picker (Ctrl+G) can search and bring back.
    #[serde(default)]
    pub restorable: Vec<FixtureRestorable>,
    /// A scripted Voice Mode call. Ctrl+K starts it, and each line shows in the
    /// footer once its time has passed. Nothing is heard or sent anywhere.
    #[serde(default)]
    pub voice: Vec<FixtureVoiceLine>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FixtureVoiceLine {
    /// Milliseconds after the call starts.
    pub at_ms: u64,
    /// `you` or `agent`.
    pub who: String,
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct FixtureLaunch {
    pub provider: Provider,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub labels: ModelLabels,
}

#[derive(Debug, Deserialize)]
pub struct FixtureUsage {
    pub provider: String,
    pub windows: Vec<FixtureUsageWindow>,
}

#[derive(Debug, Deserialize)]
pub struct FixtureUsageWindow {
    pub label: String,
    pub percent: f64,
    /// Seconds from start-up until the window resets.
    pub resets_in_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct FixtureRestorable {
    pub id: String,
    pub provider_session_id: String,
    pub provider: Provider,
    pub name: String,
    pub cwd: PathBuf,
    /// Seconds since the session was last updated.
    pub age_secs: u64,
    #[serde(default)]
    pub transcript: String,
}

#[derive(Deserialize)]
struct DemoDocument {
    #[serde(default)]
    demo: Option<FixtureDemo>,
}

impl FixtureDemo {
    /// The `demo` section of the fixture at `path`, if it has one.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let input = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Ok(serde_json::from_str::<DemoDocument>(&input)
            .ok()
            .and_then(|document| document.demo))
    }

    pub fn launch_targets(&self) -> Vec<LaunchTarget> {
        self.launch
            .iter()
            .map(|launch| LaunchTarget {
                provider: launch.provider.clone(),
                supports_model: !launch.models.is_empty(),
            })
            .collect()
    }

    pub fn models(&self, provider: &Provider) -> Option<(Vec<String>, ModelLabels)> {
        self.launch
            .iter()
            .find(|launch| &launch.provider == provider && !launch.models.is_empty())
            .map(|launch| (launch.models.clone(), launch.labels.clone()))
    }

    pub fn usage(&self) -> Vec<crate::usage::Usage> {
        let now = crate::usage::now();
        let mut usage = self
            .usage
            .iter()
            .filter_map(|entry| {
                let provider = match entry.provider.as_str() {
                    "claude" => "claude",
                    "codex" => "codex",
                    _ => return None,
                };
                let windows = entry
                    .windows
                    .iter()
                    .filter_map(|window| {
                        Some(crate::usage::Window {
                            label: match window.label.as_str() {
                                "5h" => "5h",
                                "day" => "day",
                                "week" => "week",
                                "month" => "month",
                                _ => return None,
                            },
                            percent: window.percent,
                            resets_at: window.resets_in_secs.map(|seconds| now + seconds),
                        })
                    })
                    .collect::<Vec<_>>();
                (!windows.is_empty()).then_some(crate::usage::Usage { provider, windows })
            })
            .collect::<Vec<_>>();
        usage.sort_by(|left, right| left.provider.cmp(right.provider));
        usage
    }

    pub fn restorable_sessions(&self) -> Vec<RestorableSession> {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as u64);
        let mut sessions = self
            .restorable
            .iter()
            .map(|session| RestorableSession {
                id: session.id.clone(),
                provider_session_id: session.provider_session_id.clone(),
                provider: session.provider.clone(),
                name: session.name.clone(),
                cwd: session.cwd.clone(),
                updated_at_ms: now_ms.saturating_sub(session.age_secs * 1000),
                transcript: session.transcript.clone(),
            })
            .collect::<Vec<_>>();
        sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at_ms));
        sessions
    }
}

pub struct FixtureSource {
    path: PathBuf,
}

impl FixtureSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl SessionSource for FixtureSource {
    fn label(&self) -> &str {
        "fixture"
    }

    fn discover(&self, request: &DiscoveryRequest) -> Result<Vec<AgentSession>> {
        let input = fs::read_to_string(&self.path)
            .with_context(|| format!("failed to read {}", self.path.display()))?;
        let mut sessions = match serde_json::from_str::<SessionSnapshot>(&input) {
            Ok(snapshot) => snapshot.sessions,
            Err(_) => serde_json::from_str::<Vec<AgentSession>>(&input)
                .context("fixture must be a SessionSnapshot or session array")?,
        };
        sessions.retain(|session| {
            (request.include_completed || session.state != crate::domain::SessionState::Completed)
                && (request.include_interactive
                    || session.kind != crate::domain::SessionKind::Interactive)
                && request
                    .cwd
                    .as_ref()
                    .map(|cwd| session.cwd.starts_with(cwd))
                    .unwrap_or(true)
        });
        Ok(sessions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_section_serves_launch_models_usage_and_past_sessions() {
        let directory = crate::test_support::tempfile::tempdir().unwrap();
        let path = directory.path().join("fixture.json");
        fs::write(
            &path,
            r#"{"sessions": [], "demo": {
                "launch": [
                    {"provider": "opencode", "models": ["anthropic/claude-opus-5-5"],
                     "labels": {"anthropic/claude-opus-5-5": {"name": "Claude Opus 5.5", "provider": "Anthropic"}}},
                    {"provider": "claude"}
                ],
                "usage": [
                    {"provider": "codex", "windows": [{"label": "week", "percent": 44, "resets_in_secs": 60}]},
                    {"provider": "claude", "windows": [{"label": "5h", "percent": 38}, {"label": "fortnight", "percent": 1}]},
                    {"provider": "unknown", "windows": [{"label": "5h", "percent": 1}]}
                ],
                "restorable": [
                    {"id": "claude:host:old", "provider_session_id": "old", "provider": "claude",
                     "name": "old", "cwd": "/work", "age_secs": 900, "transcript": "idempotency key"},
                    {"id": "codex:host:new", "provider_session_id": "new", "provider": "codex",
                     "name": "new", "cwd": "/work", "age_secs": 60}
                ]
            }}"#,
        )
        .unwrap();
        let demo = FixtureDemo::load(&path).unwrap().unwrap();
        assert_eq!(
            demo.launch_targets(),
            vec![
                LaunchTarget {
                    provider: Provider::OpenCode,
                    supports_model: true
                },
                LaunchTarget {
                    provider: Provider::Claude,
                    supports_model: false
                },
            ]
        );
        let (models, labels) = demo.models(&Provider::OpenCode).unwrap();
        assert_eq!(models, vec!["anthropic/claude-opus-5-5"]);
        assert_eq!(labels["anthropic/claude-opus-5-5"].name, "Claude Opus 5.5");
        assert!(demo.models(&Provider::Claude).is_none());

        let usage = demo.usage();
        assert_eq!(
            usage
                .iter()
                .map(|entry| (entry.provider, entry.windows.len()))
                .collect::<Vec<_>>(),
            vec![("claude", 1), ("codex", 1)]
        );
        assert!(usage[1].windows[0].resets_at.unwrap() >= crate::usage::now() + 59);

        let past = demo.restorable_sessions();
        assert_eq!(
            past.iter()
                .map(|session| session.name.as_str())
                .collect::<Vec<_>>(),
            vec!["new", "old"]
        );
        assert_eq!(past[1].transcript, "idempotency key");
    }

    #[test]
    fn a_fixture_without_a_demo_section_has_none() {
        let directory = crate::test_support::tempfile::tempdir().unwrap();
        let path = directory.path().join("fixture.json");
        fs::write(&path, r#"{"sessions": []}"#).unwrap();
        assert!(FixtureDemo::load(&path).unwrap().is_none());
    }
}
