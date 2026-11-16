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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Wake {
    pub label: String,
    pub text: String,
    pub due_ms: u64,
    /// Only an explicit defer may put a session away. A later user message
    /// clears its effect without cancelling the reminder.
    pub deferred_at_ms: Option<u64>,
}

#[derive(Deserialize)]
struct Hold {
    pid: u32,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    expires_ms: Option<u64>,
    #[serde(default)]
    wake: Option<Wake>,
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
    let live = entries
        .into_iter()
        .filter_map(|path| {
            let hold = serde_json::from_slice::<Hold>(&std::fs::read(path).ok()?).ok()?;
            let current = hold.expires_ms.map_or(true, |expires| now_ms < expires);
            (current
                && process_alive(hold.pid)
                && hold.wake.is_none()
                && !hold.reason.starts_with("wake-later "))
            .then_some(hold.reason)
        })
        .collect::<Vec<_>>();
    live.into_iter().next()
}

/// Earliest pending wake, whether it defers the session or merely reminds it.
pub fn next_wake(root: &Path, harness: &str, session_id: &str) -> Option<Wake> {
    if session_id.is_empty() || session_id.contains(['/', '\\']) || session_id.starts_with('.') {
        return None;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    let wakes = std::fs::read_dir(root.join(harness).join(session_id))
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| serde_json::from_slice::<Hold>(&std::fs::read(entry.path()).ok()?).ok())
        .filter(|hold| {
            hold.expires_ms.map_or(true, |expiry| now < expiry) && process_alive(hold.pid)
        })
        .filter_map(|hold| hold.wake)
        .collect::<Vec<_>>();
    let deferred_at = wakes.iter().filter_map(|wake| wake.deferred_at_ms).max();
    let mut next = wakes.into_iter().min_by_key(|wake| wake.due_ms)?;
    next.deferred_at_ms = deferred_at;
    Some(next)
}

/// Every session under `harness` that has a live hold, with that hold's reason.
pub fn live_holds(root: &Path, harness: &str) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(root.join(harness)) else {
        return found;
    };
    for entry in entries.filter_map(Result::ok) {
        let Some(session) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if let Some(reason) = live_hold(root, harness, &session) {
            found.insert(session, reason);
        }
    }
    found
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
        assert_eq!(
            live_holds(root.path(), "opencode")
                .get("ses_1")
                .map(String::as_str),
            Some("deploy")
        );
        assert_eq!(live_hold(root.path(), "opencode", "ses_2"), None);
        assert_eq!(live_hold(root.path(), "opencode", "../opencode"), None);
    }

    #[cfg(unix)]
    #[test]
    fn live_work_takes_precedence_over_a_reminder() {
        let root = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        write_hold(
            root.path(),
            "ses_both",
            "a.json",
            &format!(r#"{{"pid":{pid},"reason":"wake-later Sat 07:00"}}"#),
        );
        write_hold(
            root.path(),
            "ses_both",
            "z.json",
            &format!(r#"{{"pid":{pid},"reason":"CI running"}}"#),
        );
        assert_eq!(
            live_hold(root.path(), "opencode", "ses_both").as_deref(),
            Some("CI running")
        );
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

    #[cfg(unix)]
    #[test]
    fn reminders_are_metadata_not_work_and_only_explicit_defers_put_sessions_away() {
        let root = tempfile::tempdir().unwrap();
        let write = |name: &str, due, deferred_at| {
            let wake = Wake {
                label: "Mon 10:00".into(),
                text: "check replies".into(),
                due_ms: due,
                deferred_at_ms: deferred_at,
            };
            write_hold(
                root.path(),
                "ses_wake",
                name,
                &serde_json::json!({
                    "pid": std::process::id(), "reason": "wake-later Mon 10:00", "wake": wake
                })
                .to_string(),
            );
        };
        write("a.json", 200, None);
        assert_eq!(live_hold(root.path(), "opencode", "ses_wake"), None);
        assert_eq!(
            next_wake(root.path(), "opencode", "ses_wake")
                .unwrap()
                .deferred_at_ms,
            None
        );
        write("b.json", 300, Some(100));
        let wake = next_wake(root.path(), "opencode", "ses_wake").unwrap();
        assert_eq!((wake.due_ms, wake.deferred_at_ms), (200, Some(100)));
        assert_eq!(live_hold(root.path(), "opencode", "ses_wake"), None);
        assert_eq!(next_wake(root.path(), "opencode", "../ses_wake"), None);
    }
}
