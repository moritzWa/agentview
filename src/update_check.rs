//! "A newer release is out" notice for the dashboard header.
//!
//! Only published release builds check; local and source builds never do. The
//! latest tag is cached for a day so the dashboard asks GitHub at most once a
//! day, from a background thread, and never blocks drawing.

use std::path::PathBuf;
use std::sync::{Mutex, Once};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
const LATEST_RELEASE: &str =
    "https://api.github.com/repos/moritzWa/betteragentview/releases/latest";

static LATEST: Mutex<Option<String>> = Mutex::new(None);
static START: Once = Once::new();

/// The newer version to offer, such as `0.3.4`, or `None` when up to date.
pub fn newer_version() -> Option<String> {
    if option_env!("AGENTVIEW_RELEASE_BUILD").is_none()
        || std::env::var_os("AGENTVIEW_NO_UPDATE_CHECK").is_some()
    {
        return None;
    }
    START.call_once(start);
    let latest = LATEST.lock().ok()?.clone()?;
    is_newer(&latest, env!("CARGO_PKG_VERSION")).then_some(latest)
}

fn start() {
    let cached = cache_path().and_then(|path| std::fs::read_to_string(path).ok());
    let (checked, tag) = cached
        .as_deref()
        .and_then(parse_cache)
        .unwrap_or((0, String::new()));
    if !tag.is_empty() {
        if let Ok(mut latest) = LATEST.lock() {
            *latest = Some(tag);
        }
    }
    let now = now_secs();
    if now.saturating_sub(checked) < CHECK_EVERY.as_secs() {
        return;
    }
    std::thread::spawn(move || {
        let Some(tag) = fetch_latest() else { return };
        if let Some(path) = cache_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, format!("{now} {tag}\n"));
        }
        if let Ok(mut latest) = LATEST.lock() {
            *latest = Some(tag);
        }
    });
}

fn fetch_latest() -> Option<String> {
    let output = std::process::Command::new("curl")
        .args(["-fsSL", "--max-time", "8", LATEST_RELEASE])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let tag = body.get("tag_name")?.as_str()?;
    Some(tag.trim_start_matches('v').to_string())
}

fn parse_cache(text: &str) -> Option<(u64, String)> {
    let (checked, tag) = text.trim().split_once(' ')?;
    Some((checked.parse().ok()?, tag.to_string()))
}

fn version_parts(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim_start_matches('v').split('.');
    Some((
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ))
}

fn is_newer(candidate: &str, current: &str) -> bool {
    match (version_parts(candidate), version_parts(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn cache_path() -> Option<PathBuf> {
    if let Some(state_home) = crate::fs_util::xdg_home("XDG_STATE_HOME") {
        return Some(
            PathBuf::from(state_home)
                .join("agentview")
                .join("latest-release"),
        );
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".local/state/agentview/latest-release"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("0.3.10", "0.3.9"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("0.3.0", "0.3.0"));
        assert!(!is_newer("0.2.9", "0.3.0"));
        assert!(!is_newer("garbage", "0.3.0"));
    }

    #[test]
    fn reads_the_cache_line() {
        assert_eq!(
            parse_cache("1700000000 0.3.4\n"),
            Some((1_700_000_000, "0.3.4".into()))
        );
        assert_eq!(parse_cache(""), None);
    }
}
