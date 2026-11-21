//! How often each session is opened from the find-session picker.
//!
//! The picker's default view (no query typed) ranks sessions by this count
//! together with how recently the session saw activity. The count decays, so
//! a session opened often long ago fades as new ones take its place.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const DOCUMENT_VERSION: u32 = 1;
const DAY_MS: f64 = 86_400_000.0;
/// Days for an open's weight to halve.
const OPEN_HALF_LIFE_DAYS: f64 = 30.0;
/// Days of inactivity for a session's recency to halve.
const ACTIVITY_HALF_LIFE_DAYS: f64 = 7.0;
/// A record this faint no longer changes the ranking.
const FORGET_BELOW: f64 = 0.01;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
struct OpenRecord {
    /// Opens, each weighted by how long ago it happened, as of `at_ms`.
    weight: f64,
    at_ms: u64,
}

impl OpenRecord {
    fn weight_at(&self, now_ms: u64) -> f64 {
        let age_days = now_ms.saturating_sub(self.at_ms) as f64 / DAY_MS;
        self.weight * 0.5_f64.powf(age_days / OPEN_HALF_LIFE_DAYS)
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct SearchOpensDocument {
    version: u32,
    sessions: BTreeMap<String, OpenRecord>,
}

#[derive(Clone, Debug)]
pub struct SearchOpens {
    path: PathBuf,
    records: BTreeMap<String, OpenRecord>,
}

impl SearchOpens {
    pub fn load_default() -> Result<Self> {
        Ok(Self::load(default_search_opens_path()?))
    }

    /// A missing or unreadable file starts an empty count.
    pub fn load(path: PathBuf) -> Self {
        let records = fs::read_to_string(&path)
            .ok()
            .and_then(|input| serde_json::from_str::<SearchOpensDocument>(&input).ok())
            .filter(|document| document.version == DOCUMENT_VERSION)
            .map(|document| document.sessions)
            .unwrap_or_default();
        Self { path, records }
    }

    /// Count one open of `id` from the picker and save.
    pub fn record(&mut self, id: &str, now_ms: u64) -> Result<()> {
        let weight = self
            .records
            .get(id)
            .map_or(0.0, |record| record.weight_at(now_ms));
        self.records.insert(
            id.to_owned(),
            OpenRecord {
                weight: weight + 1.0,
                at_ms: now_ms,
            },
        );
        self.records
            .retain(|_, record| record.weight_at(now_ms) >= FORGET_BELOW);
        crate::fs_util::write_private_json(
            &self.path,
            &SearchOpensDocument {
                version: DOCUMENT_VERSION,
                sessions: self.records.clone(),
            },
        )
        .with_context(|| format!("failed to save {}", self.path.display()))
    }

    /// Ranking score for the default view; higher ranks first.
    ///
    /// Recent activity (a reply, or the last open from search) scales the
    /// decayed open count, so a session never opened is ordered by recency
    /// alone, and one opened a lot but idle for weeks sinks. The score is a
    /// logarithm, so very old sessions stay ordered instead of rounding to 0.
    pub fn score(&self, id: &str, activity_ms: u64, now_ms: u64) -> f64 {
        let (opens, last_open_ms) = self
            .records
            .get(id)
            .map_or((0.0, 0), |record| (record.weight_at(now_ms), record.at_ms));
        let last_touch_ms = activity_ms.max(last_open_ms);
        let idle_days = now_ms.saturating_sub(last_touch_ms) as f64 / DAY_MS;
        (1.0 + opens).ln() - idle_days / ACTIVITY_HALF_LIFE_DAYS * std::f64::consts::LN_2
    }
}

pub fn default_search_opens_path() -> Result<PathBuf> {
    if let Some(state_home) = crate::fs_util::xdg_home("XDG_STATE_HOME") {
        return Ok(PathBuf::from(state_home)
            .join("agentview")
            .join("search-opens.json"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/state/agentview/search-opens.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400_000;
    const NOW: u64 = 1_000 * DAY;

    fn store() -> (::tempfile::TempDir, SearchOpens) {
        let directory = crate::test_support::tempfile::tempdir().unwrap();
        let path = directory.path().join("state").join("search-opens.json");
        (directory, SearchOpens::load(path))
    }

    #[test]
    fn opens_survive_a_restart() {
        let (directory, mut opens) = store();
        opens.record("a", NOW).unwrap();
        opens.record("a", NOW).unwrap();
        let reloaded = SearchOpens::load(directory.path().join("state/search-opens.json"));
        assert_eq!(reloaded.records, opens.records);
        assert!(reloaded.score("a", NOW, NOW) > reloaded.score("b", NOW, NOW));
    }

    #[test]
    fn unopened_sessions_rank_by_recency_alone() {
        let (_directory, opens) = store();
        assert!(opens.score("a", NOW - DAY, NOW) > opens.score("b", NOW - 2 * DAY, NOW));
    }

    #[test]
    fn more_opens_rank_higher_at_equal_recency() {
        let (_directory, mut opens) = store();
        opens.record("often", NOW).unwrap();
        opens.record("often", NOW).unwrap();
        opens.record("once", NOW).unwrap();
        assert!(opens.score("often", NOW, NOW) > opens.score("once", NOW, NOW));
    }

    #[test]
    fn opened_a_lot_but_long_idle_ranks_below_a_fresh_session() {
        let (_directory, mut opens) = store();
        for _ in 0..10 {
            opens.record("old-favorite", NOW - 30 * DAY).unwrap();
        }
        let favorite = opens.score("old-favorite", NOW - 30 * DAY, NOW);
        let fresh = opens.score("fresh", NOW, NOW);
        assert!(favorite < fresh, "{favorite} should be below {fresh}");
    }

    #[test]
    fn opened_a_lot_and_active_ranks_first() {
        let (_directory, mut opens) = store();
        for _ in 0..5 {
            opens.record("favorite", NOW - DAY).unwrap();
        }
        assert!(opens.score("favorite", NOW - DAY, NOW) > opens.score("fresh", NOW, NOW));
    }

    #[test]
    fn faint_records_are_forgotten_on_save() {
        let (_directory, mut opens) = store();
        opens.record("ancient", NOW - 400 * DAY).unwrap();
        opens.record("new", NOW).unwrap();
        assert!(!opens.records.contains_key("ancient"));
        assert!(opens.records.contains_key("new"));
    }

    #[test]
    fn unreadable_file_starts_empty() {
        let directory = crate::test_support::tempfile::tempdir().unwrap();
        let path = directory.path().join("search-opens.json");
        fs::write(&path, "not json").unwrap();
        assert!(SearchOpens::load(path).records.is_empty());
    }
}
