//! The harness the new-session composer starts on.
//!
//! The dashboard records the provider of every launch it dispatches, and the
//! next start opens the composer on it unless `--harness` says otherwise. A
//! missing or unreadable record falls back to the built-in default.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::domain::Provider;

const DOCUMENT_VERSION: u32 = 1;

#[derive(Debug, Deserialize, Serialize)]
struct LastHarnessDocument {
    version: u32,
    provider: Provider,
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

    pub fn provider(&self) -> Option<Provider> {
        let input = fs::read_to_string(&self.path).ok()?;
        let document: LastHarnessDocument = serde_json::from_str(&input).ok()?;
        (document.version == DOCUMENT_VERSION).then_some(document.provider)
    }

    pub fn save(&self, provider: &Provider) -> Result<()> {
        if self.provider().as_ref() == Some(provider) {
            return Ok(());
        }
        crate::fs_util::write_private_json(
            &self.path,
            &LastHarnessDocument {
                version: DOCUMENT_VERSION,
                provider: provider.clone(),
            },
        )
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
    fn unreadable_record_falls_back_to_the_default() {
        let directory = crate::test_support::tempfile::tempdir().unwrap();
        let path = directory.path().join("last-harness.json");
        fs::write(&path, "not json").unwrap();
        assert_eq!(LastHarness::at(path).provider(), None);
    }
}
