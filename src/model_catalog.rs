//! What the model picker shows before the harness has answered.
//!
//! Listing a harness's models can take seconds (`opencode models` boots a
//! whole process), so the last answer per harness is kept on disk and shown
//! at once while a fresh one loads in the background.
//!
//! OpenCode's own recent and favorite models live in its `model.json`. The
//! picker reads them to pin the same "Recent" groups OpenCode shows, and a
//! pick made here is written back so both pickers stay in step.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::domain::Provider;

const CACHE_VERSION: u32 = 1;
/// OpenCode keeps this many recent models (`recentModels` in its TUI).
const OPENCODE_RECENT_LIMIT: usize = 10;

#[derive(Debug, Default, Deserialize, Serialize)]
struct CacheDocument {
    version: u32,
    #[serde(default)]
    models: BTreeMap<Provider, Vec<String>>,
}

#[derive(Clone, Debug)]
pub struct ModelCache {
    path: PathBuf,
}

impl ModelCache {
    pub fn load_default() -> Result<Self> {
        Ok(Self::at(state_path("agentview", "model-cache.json")?))
    }

    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    fn document(&self) -> CacheDocument {
        fs::read_to_string(&self.path)
            .ok()
            .and_then(|input| serde_json::from_str::<CacheDocument>(&input).ok())
            .filter(|document| document.version == CACHE_VERSION)
            .unwrap_or_default()
    }

    pub fn all(&self) -> BTreeMap<Provider, Vec<String>> {
        self.document().models
    }

    pub fn save(&self, provider: &Provider, models: &[String]) -> Result<()> {
        let mut document = self.document();
        if document.models.get(provider).map(Vec::as_slice) == Some(models) {
            return Ok(());
        }
        document.version = CACHE_VERSION;
        document.models.insert(provider.clone(), models.to_vec());
        crate::fs_util::write_private_json(&self.path, &document)
    }
}

/// OpenCode's pinned models, as `provider/model` ids.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenCodePins {
    pub recent: Vec<String>,
    pub favorite: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct OpenCodeModelState {
    path: PathBuf,
}

impl OpenCodeModelState {
    pub fn load_default() -> Result<Self> {
        Ok(Self::at(state_path("opencode", "model.json")?))
    }

    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    fn raw(&self) -> Option<Value> {
        let input = fs::read_to_string(&self.path).ok()?;
        serde_json::from_str::<Value>(&input)
            .ok()
            .filter(Value::is_object)
    }

    pub fn pins(&self) -> OpenCodePins {
        let Some(raw) = self.raw() else {
            return OpenCodePins::default();
        };
        OpenCodePins {
            recent: model_ids(raw.get("recent")),
            favorite: model_ids(raw.get("favorite")),
        }
    }

    /// Move `model` to the front of OpenCode's recent list, the way its own
    /// picker does. Every other key in the file is kept as it was.
    pub fn record_recent(&self, model: &str) -> Result<()> {
        let Some((provider, model_id)) = model.split_once('/') else {
            return Ok(());
        };
        let mut raw = self.raw().unwrap_or_else(|| json!({}));
        let entry = json!({ "providerID": provider, "modelID": model_id });
        let mut recent = vec![entry.clone()];
        if let Some(existing) = raw.get("recent").and_then(Value::as_array) {
            recent.extend(existing.iter().filter(|item| **item != entry).cloned());
        }
        recent.truncate(OPENCODE_RECENT_LIMIT);
        if raw.get("recent").and_then(Value::as_array) == Some(&recent) {
            return Ok(());
        }
        raw.as_object_mut()
            .context("OpenCode model.json is not an object")?
            .insert("recent".into(), Value::Array(recent));
        crate::fs_util::write_private_json(&self.path, &raw)
    }
}

fn model_ids(list: Option<&Value>) -> Vec<String> {
    list.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let provider = item.get("providerID")?.as_str()?;
            let model = item.get("modelID")?.as_str()?;
            Some(format!("{provider}/{model}"))
        })
        .collect()
}

fn state_path(app: &str, file: &str) -> Result<PathBuf> {
    if let Some(state_home) = crate::fs_util::xdg_home("XDG_STATE_HOME") {
        return Ok(PathBuf::from(state_home).join(app).join(file));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join(".local/state")
        .join(app)
        .join(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_models_survive_a_restart_per_harness() {
        let directory = crate::test_support::tempfile::tempdir().unwrap();
        let cache = ModelCache::at(directory.path().join("state").join("model-cache.json"));
        assert!(cache.all().is_empty());
        cache
            .save(&Provider::OpenCode, &["anthropic/claude-opus-5-5".into()])
            .unwrap();
        cache.save(&Provider::Claude, &["opus".into()]).unwrap();
        let all = ModelCache::at(cache.path.clone()).all();
        assert_eq!(all[&Provider::OpenCode], vec!["anthropic/claude-opus-5-5"]);
        assert_eq!(all[&Provider::Claude], vec!["opus"]);
    }

    #[test]
    fn unreadable_cache_is_empty() {
        let directory = crate::test_support::tempfile::tempdir().unwrap();
        let path = directory.path().join("model-cache.json");
        fs::write(&path, "not json").unwrap();
        assert!(ModelCache::at(path).all().is_empty());
    }

    #[test]
    fn reads_opencode_pins_and_moves_a_pick_to_the_front_keeping_other_keys() {
        let directory = crate::test_support::tempfile::tempdir().unwrap();
        let path = directory.path().join("model.json");
        fs::write(
            &path,
            r#"{"recent":[{"providerID":"openai","modelID":"gpt-6-astra"},{"providerID":"anthropic","modelID":"claude-opus-5-5"}],"favorite":[{"providerID":"cursor","modelID":"auto"}],"variant":{"openai/gpt-6-astra":"default"}}"#,
        )
        .unwrap();
        let state = OpenCodeModelState::at(path.clone());
        assert_eq!(
            state.pins(),
            OpenCodePins {
                recent: vec![
                    "openai/gpt-6-astra".into(),
                    "anthropic/claude-opus-5-5".into()
                ],
                favorite: vec!["cursor/auto".into()],
            }
        );
        state.record_recent("anthropic/claude-opus-5-5").unwrap();
        assert_eq!(
            state.pins().recent,
            vec!["anthropic/claude-opus-5-5", "openai/gpt-6-astra"]
        );
        let raw: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw["variant"]["openai/gpt-6-astra"], "default");
    }

    #[test]
    fn recent_list_keeps_opencodes_limit() {
        let directory = crate::test_support::tempfile::tempdir().unwrap();
        let state = OpenCodeModelState::at(directory.path().join("model.json"));
        for index in 0..12 {
            state.record_recent(&format!("groq/model-{index}")).unwrap();
        }
        let recent = state.pins().recent;
        assert_eq!(recent.len(), OPENCODE_RECENT_LIMIT);
        assert_eq!(recent[0], "groq/model-11");
    }
}
