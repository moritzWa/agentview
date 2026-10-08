//! The harness the new-session composer starts on.
//!
//! The dashboard records the provider of every launch it dispatches, and the
//! next start opens the composer on it unless `--harness` says otherwise. A
//! missing or unreadable record falls back to the built-in default.
//!
//! It also keeps the model last chosen for each harness, so a choice such as
//! `anthropic/claude-opus-5-5` outlives a restart. No entry means the
//! harness's own default.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::domain::Provider;

const DOCUMENT_VERSION: u32 = 1;

#[derive(Debug, Default, Deserialize, Serialize)]
struct LastHarnessDocument {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider: Option<Provider>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    models: Vec<RememberedModel>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RememberedModel {
    provider: Provider,
    model: String,
}

#[derive(Clone, Debug)]
pub struct LastHarness {
    path: PathBuf,
}

impl LastHarness {
    pub fn load_default() -> Result<Self> {
        Ok(Self::at(default_last_harness_path()?))
    }

    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    fn document(&self) -> Option<LastHarnessDocument> {
        let input = fs::read_to_string(&self.path).ok()?;
        let document: LastHarnessDocument = serde_json::from_str(&input).ok()?;
        (document.version == DOCUMENT_VERSION).then_some(document)
    }

    pub fn provider(&self) -> Option<Provider> {
        self.document()?.provider
    }

    /// Every remembered model, by harness.
    pub fn models(&self) -> Vec<(Provider, String)> {
        self.document()
            .map(|document| {
                document
                    .models
                    .into_iter()
                    .map(|entry| (entry.provider, entry.model))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The model last chosen for `provider`, or `None` for its default.
    pub fn model(&self, provider: &Provider) -> Option<String> {
        self.document()?
            .models
            .into_iter()
            .find(|entry| &entry.provider == provider)
            .map(|entry| entry.model)
    }

    pub fn save(&self, provider: &Provider) -> Result<()> {
        let mut document = self.document().unwrap_or_default();
        if document.provider.as_ref() == Some(provider) {
            return Ok(());
        }
        document.version = DOCUMENT_VERSION;
        document.provider = Some(provider.clone());
        crate::fs_util::write_private_json(&self.path, &document)
    }

    /// Remember `model` for `provider`; `None` goes back to its default.
    pub fn save_model(&self, provider: &Provider, model: Option<&str>) -> Result<()> {
        if self.model(provider).as_deref() == model {
            return Ok(());
        }
        let mut document = self.document().unwrap_or_default();
        document.version = DOCUMENT_VERSION;
        document.models.retain(|entry| &entry.provider != provider);
        if let Some(model) = model {
            document.models.push(RememberedModel {
                provider: provider.clone(),
                model: model.to_owned(),
            });
        }
        crate::fs_util::write_private_json(&self.path, &document)
    }
}

pub fn default_last_harness_path() -> Result<PathBuf> {
    if let Some(state_home) = crate::fs_util::xdg_home("XDG_STATE_HOME") {
        return Ok(PathBuf::from(state_home)
            .join("agentview")
            .join("last-harness.json"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/state/agentview/last-harness.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_harness_survives_a_restart() {
        let directory = crate::test_support::tempfile::tempdir().unwrap();
        let path = directory.path().join("state").join("last-harness.json");
        assert_eq!(LastHarness::at(path.clone()).provider(), None);
        LastHarness::at(path.clone())
            .save(&Provider::OpenCode)
            .unwrap();
        assert_eq!(
            LastHarness::at(path.clone()).provider(),
            Some(Provider::OpenCode)
        );
        LastHarness::at(path.clone())
            .save(&Provider::Codex)
            .unwrap();
        assert_eq!(LastHarness::at(path).provider(), Some(Provider::Codex));
    }

    #[test]
    fn chosen_models_survive_a_restart_per_harness_and_keep_the_harness() {
        let directory = crate::test_support::tempfile::tempdir().unwrap();
        let path = directory.path().join("last-harness.json");
        // A record written before models were kept still reads.
        fs::write(&path, r#"{"version":1,"provider":"opencode"}"#).unwrap();
        let record = LastHarness::at(path.clone());
        assert_eq!(record.model(&Provider::OpenCode), None);
        record
            .save_model(&Provider::OpenCode, Some("cursor/claude-opus-5-5"))
            .unwrap();
        record.save_model(&Provider::Claude, Some("opus")).unwrap();
        record
            .save_model(&Provider::OpenCode, Some("anthropic/claude-opus-5-5"))
            .unwrap();
        let reloaded = LastHarness::at(path.clone());
        assert_eq!(reloaded.provider(), Some(Provider::OpenCode));
        assert_eq!(
            reloaded.model(&Provider::OpenCode).as_deref(),
            Some("anthropic/claude-opus-5-5")
        );
        assert_eq!(reloaded.model(&Provider::Claude).as_deref(), Some("opus"));
        reloaded.save(&Provider::Codex).unwrap();
        reloaded.save_model(&Provider::Claude, None).unwrap();
        let reloaded = LastHarness::at(path);
        assert_eq!(reloaded.provider(), Some(Provider::Codex));
        assert_eq!(reloaded.model(&Provider::Claude), None);
        assert!(reloaded.model(&Provider::OpenCode).is_some());
    }

    #[test]
    fn unreadable_record_falls_back_to_the_default() {
        let directory = crate::test_support::tempfile::tempdir().unwrap();
        let path = directory.path().join("last-harness.json");
        fs::write(&path, "not json").unwrap();
        assert_eq!(LastHarness::at(path).provider(), None);
    }
}
