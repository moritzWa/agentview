//! Background work that keeps a session busy after its turn ended, reported
//! by a harness plugin rather than the harness's own records.
//!
//! A plugin writes one JSON file per piece of work to
//! `$XDG_STATE_HOME/agentview/holds/<harness>/<session id>/<name>.json`
//! (`~/.local/state/...` without `XDG_STATE_HOME`) and deletes it when the
//! work ends:
//!
//! ```json
//! {"pid": 4242, "reason": "CI on PR 8", "expires_ms": 1790000000000}
//! ```
//!
//! A hold counts only while `pid` is alive and before `expires_ms`, so a file
//! left behind by a crashed plugin never pins a session as working.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

#[derive(Deserialize)]
struct Hold {
    pid: u32,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    expires_ms: Option<u64>,
}

pub fn default_holds_dir() -> Option<PathBuf> {
    if let Some(state_home) = crate::fs_util::xdg_home("XDG_STATE_HOME") {
        return Some(PathBuf::from(state_home).join("agentview").join("holds"));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".local/state/agentview/holds"))
}

/// The reason of the first live hold on a session, if any.
pub fn live_hold(root: &Path, harness: &str, session_id: &str) -> Option<String> {
    if session_id.is_empty() || session_id.contains(['/', '\\']) || session_id.starts_with('.') {
        return None;
    }
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let mut entries = std::fs::read_dir(root.join(harness).join(session_id))
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    entries.sort();
    entries.into_iter().find_map(|path| {
        let hold = serde_json::from_slice::<Hold>(&std::fs::read(path).ok()?).ok()?;
        let current = hold.expires_ms.map_or(true, |expires| now_ms < expires);
        (current && process_alive(hold.pid)).then_some(hold.reason)
    })
}

#[cfg(unix)]
pub(crate) fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    let signalled = unsafe { libc::kill(pid, 0) } == 0;
    signalled || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
pub(crate) fn process_alive(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_hold(root: &Path, session: &str, name: &str, body: &str) {
        let dir = root.join("opencode").join(session);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_hold_counts_only_while_its_process_lives_and_before_it_expires() {
        let root = tempfile::tempdir().unwrap();
        let me = std::process::id();
        assert_eq!(live_hold(root.path(), "opencode", "ses_1"), None);

        write_hold(
            root.path(),
            "ses_1",
            "m1.json",
            &format!(r#"{{"pid": {me}, "reason": "CI on PR 8", "expires_ms": 1}}"#),
        );
        assert_eq!(live_hold(root.path(), "opencode", "ses_1"), None);

        write_hold(
            root.path(),
            "ses_1",
            "m2.json",
            &format!(r#"{{"pid": {me}, "reason": "deploy"}}"#),
        );
        assert_eq!(
            live_hold(root.path(), "opencode", "ses_1").as_deref(),
            Some("deploy")
        );
        assert_eq!(live_hold(root.path(), "opencode", "ses_2"), None);
        assert_eq!(live_hold(root.path(), "opencode", "../opencode"), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_hold_from_an_exited_process_is_ignored() {
        let root = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        write_hold(
            root.path(),
            "ses_1",
            "m1.json",
            &format!(r#"{{"pid": {pid}, "reason": "gone"}}"#),
        );
        assert_eq!(live_hold(root.path(), "opencode", "ses_1"), None);
    }
}
