//! The dashboard view (by status or by directory) and the selected session a
//! new start opens on.
//!
//! Every ctrl+s toggle is recorded, so the next start shows the view the
//! dashboard was last left in. A missing or unreadable record falls back to
//! the status view. The row the cursor rests on is recorded the same way, so
//! a restart returns to it instead of the top of the list.

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<String>,
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

    fn read(&self) -> Option<LastViewDocument> {
        let input = fs::read_to_string(&self.path).ok()?;
        let document: LastViewDocument = serde_json::from_str(&input).ok()?;
        (document.version == DOCUMENT_VERSION).then_some(document)
    }

    pub fn view_mode(&self) -> Option<ViewMode> {
        self.read().map(|document| document.view)
    }

    pub fn session_id(&self) -> Option<String> {
        self.read()?.session
    }

    pub fn save(&self, view: ViewMode) -> Result<()> {
        let session = self.session_id();
        if self.view_mode() == Some(view) {
            return Ok(());
        }
        self.write(view, session)
    }

    pub fn save_session(&self, session: &str) -> Result<()> {
        if self.session_id().as_deref() == Some(session) {
            return Ok(());
        }
        let view = self.view_mode().unwrap_or(ViewMode::Status);
        self.write(view, Some(session.to_owned()))
    }

    fn write(&self, view: ViewMode, session: Option<String>) -> Result<()> {
        crate::fs_util::write_private_json(
            &self.path,
            &LastViewDocument {
                version: DOCUMENT_VERSION,
                view,
                session,
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
    fn saved_session_survives_a_restart_and_a_view_change() {
        let directory = crate::test_support::tempfile::tempdir().unwrap();
        let path = directory.path().join("last-view.json");
        assert_eq!(LastView::at(path.clone()).session_id(), None);
        LastView::at(path.clone()).save_session("a").unwrap();
        LastView::at(path.clone())
            .save(ViewMode::Directory)
            .unwrap();
        let last_view = LastView::at(path);
        assert_eq!(last_view.session_id().as_deref(), Some("a"));
        assert_eq!(last_view.view_mode(), Some(ViewMode::Directory));
        last_view.save_session("b").unwrap();
        assert_eq!(last_view.view_mode(), Some(ViewMode::Directory));
        assert_eq!(last_view.session_id().as_deref(), Some("b"));
    }

    #[test]
    fn unreadable_record_falls_back_to_the_default() {
        let directory = crate::test_support::tempfile::tempdir().unwrap();
        let path = directory.path().join("last-view.json");
        fs::write(&path, "not json").unwrap();
        assert_eq!(LastView::at(path).view_mode(), None);
    }
}
