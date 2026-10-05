//! The dashboard view (by status or by directory) a new start opens on.
//!
//! Every ctrl+s toggle is recorded, so the next start shows the view the
//! dashboard was last left in. A missing or unreadable record falls back to
//! the status view.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::app::ViewMode;

const DOCUMENT_VERSION: u32 = 1;

#[derive(Debug, Deserialize, Serialize)]
struct LastViewDocument {
    version: u32,
    view: ViewMode,
}

#[derive(Clone, Debug)]
pub struct LastView {
    path: PathBuf,
}

impl LastView {
    pub fn load_default() -> Result<Self> {
        Ok(Self::at(default_last_view_path()?))
    }

    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn view_mode(&self) -> Option<ViewMode> {
        let input = fs::read_to_string(&self.path).ok()?;
        let document: LastViewDocument = serde_json::from_str(&input).ok()?;
        (document.version == DOCUMENT_VERSION).then_some(document.view)
    }

    pub fn save(&self, view: ViewMode) -> Result<()> {
        if self.view_mode() == Some(view) {
            return Ok(());
        }
        crate::fs_util::write_private_json(
            &self.path,
            &LastViewDocument {
                version: DOCUMENT_VERSION,
                view,
            },
        )
    }
}

pub fn default_last_view_path() -> Result<PathBuf> {
    if let Some(state_home) = crate::fs_util::xdg_home("XDG_STATE_HOME") {
        return Ok(PathBuf::from(state_home)
            .join("agentview")
            .join("last-view.json"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/state/agentview/last-view.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_view_survives_a_restart() {
        let directory = crate::test_support::tempfile::tempdir().unwrap();
        let path = directory.path().join("state").join("last-view.json");
        assert_eq!(LastView::at(path.clone()).view_mode(), None);
        LastView::at(path.clone())
            .save(ViewMode::Directory)
            .unwrap();
        assert_eq!(
            LastView::at(path.clone()).view_mode(),
            Some(ViewMode::Directory)
        );
        LastView::at(path.clone()).save(ViewMode::Status).unwrap();
        assert_eq!(LastView::at(path).view_mode(), Some(ViewMode::Status));
    }

    #[test]
    fn unreadable_record_falls_back_to_the_default() {
        let directory = crate::test_support::tempfile::tempdir().unwrap();
        let path = directory.path().join("last-view.json");
        fs::write(&path, "not json").unwrap();
        assert_eq!(LastView::at(path).view_mode(), None);
    }
}
