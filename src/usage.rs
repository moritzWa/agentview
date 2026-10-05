//! Subscription usage for the header, one compact entry per provider.
//!
//! Each provider reuses its own CLI's login and polls the undocumented
//! endpoint behind that CLI's usage screen (Claude Code's `/usage`, Codex's
//! `/status`). Any failure just leaves that provider out of the header.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

const POLL_INTERVAL: Duration = Duration::from_secs(300);
const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub label: &'static str,
    pub percent: f64,
    /// Unix seconds.
    pub resets_at: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Usage {
    pub provider: &'static str,
    pub windows: Vec<Window>,
}

impl Usage {
    pub fn parse_claude(body: &Value) -> Option<Self> {
        let window = |key: &str, label| {
            let window = body.get(key)?;
            Some(Window {
                label,
                percent: window.get("utilization")?.as_f64()?,
                resets_at: window
                    .get("resets_at")
                    .and_then(Value::as_str)
                    .and_then(parse_rfc3339),
            })
        };
        Self::new(
            "claude",
            [window("five_hour", "5h"), window("seven_day", "week")],
        )
    }

    pub fn parse_codex(body: &Value) -> Option<Self> {
        let window = |key: &str, label| {
            let window = body.pointer(&format!("/rate_limit/{key}"))?;
            Some(Window {
                label,
                percent: window.get("used_percent")?.as_f64()?,
                resets_at: window.get("reset_at").and_then(Value::as_u64),
            })
        };
        Self::new(
            "codex",
            [
                window("primary_window", "5h"),
                window("secondary_window", "week"),
            ],
        )
    }

    fn new(provider: &'static str, windows: [Option<Window>; 2]) -> Option<Self> {
        let windows = windows.into_iter().flatten().collect::<Vec<_>>();
        (!windows.is_empty()).then_some(Self { provider, windows })
    }

    /// One short entry and whether it needs attention. Only the binding
    /// window is shown so several providers fit on one line.
    pub fn summary(&self, now: u64) -> (String, bool) {
        let blocked = self
            .windows
            .iter()
            .filter(|window| window.percent >= 100.0)
            .max_by_key(|window| window.resets_at.unwrap_or(0));
        if let Some(window) = blocked {
            let resets = window
                .resets_at
                .map(|at| format!(" · resets in {}", countdown(at.saturating_sub(now))))
                .unwrap_or_default();
            return (
                format!("{} {} limit hit{resets}", self.provider, window.label),
                true,
            );
        }
        let Some(top) = self
            .windows
            .iter()
            .max_by(|left, right| left.percent.total_cmp(&right.percent))
        else {
            return (self.provider.to_owned(), false);
        };
        (
            format!("{} {} {:.0}%", self.provider, top.label, top.percent),
            top.percent >= 90.0,
        )
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Polls every provider in the background; `take_updates` hands the latest
/// results to the event loop.
pub struct UsageWatcher {
    updates: mpsc::Receiver<Usage>,
    stop: Arc<AtomicBool>,
}

impl UsageWatcher {
    pub fn spawn() -> Option<Self> {
        let (tx, updates) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        thread::Builder::new()
            .name("subscription-usage".into())
            .spawn(move || loop {
                for usage in [fetch_claude(), fetch_codex()].into_iter().flatten() {
                    if tx.send(usage).is_err() {
                        return;
                    }
                }
                let deadline = SystemTime::now() + POLL_INTERVAL;
                while SystemTime::now() < deadline {
                    if stop_flag.load(Ordering::Relaxed) {
                        return;
                    }
                    thread::sleep(Duration::from_secs(1));
                }
            })
            .ok()?;
        Some(Self { updates, stop })
    }

    pub fn take_updates(&self) -> Vec<Usage> {
        self.updates.try_iter().collect()
    }
}

impl Drop for UsageWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn fetch_claude() -> Option<Usage> {
    let credentials = keychain_claude_credentials()
        .or_else(|| std::fs::read_to_string(home()?.join(".claude/.credentials.json")).ok())?;
    let credentials: Value = serde_json::from_str(&credentials).ok()?;
    let token = credentials
        .pointer("/claudeAiOauth/accessToken")?
        .as_str()?;
    let body = get_json(
        CLAUDE_USAGE_URL,
        &[
            format!("Authorization: Bearer {token}"),
            "anthropic-beta: oauth-2025-04-20".into(),
        ],
    )?;
    Usage::parse_claude(&body)
}

fn fetch_codex() -> Option<Usage> {
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(home()?.join(".codex")))?;
    let auth: Value =
        serde_json::from_str(&std::fs::read_to_string(codex_home.join("auth.json")).ok()?).ok()?;
    let token = auth.pointer("/tokens/access_token")?.as_str()?;
    let account = auth.pointer("/tokens/account_id")?.as_str()?;
    let body = get_json(
        CODEX_USAGE_URL,
        &[
            format!("Authorization: Bearer {token}"),
            format!("ChatGPT-Account-Id: {account}"),
        ],
    )?;
    Usage::parse_codex(&body)
}

/// Headers go through stdin so tokens never show up in `ps`.
fn get_json(url: &str, headers: &[String]) -> Option<Value> {
    use std::io::Write;
    let mut child = Command::new("curl")
        .args(["-sf", "--max-time", "10", "-H", "@-", url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    {
        let mut stdin = child.stdin.take()?;
        for header in headers {
            writeln!(stdin, "{header}").ok()?;
        }
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn keychain_claude_credentials() -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let output = Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "Claude Code-credentials",
            "-w",
        ])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
}

fn countdown(seconds: u64) -> String {
    let (days, hours, minutes) = (seconds / 86_400, seconds / 3600 % 24, seconds / 60 % 60);
    match (days, hours) {
        (0, 0) => format!("{minutes}m"),
        (0, _) => format!("{hours}h {minutes}m"),
        _ => format!("{days}d {hours}h"),
    }
}

/// `2026-10-06T08:59:59.741585+00:00` to Unix seconds.
fn parse_rfc3339(text: &str) -> Option<u64> {
    let number = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    let rest = text.get(19..)?;
    let rest = rest.strip_prefix('.').map_or(rest, |fraction| {
        fraction.trim_start_matches(|c: char| c.is_ascii_digit())
    });
    let offset = match rest {
        "Z" | "z" => 0,
        _ => {
            let sign = match rest.get(..1)? {
                "+" => 1,
                "-" => -1,
                _ => return None,
            };
            let hours = rest.get(1..3)?.parse::<i64>().ok()?;
            let minutes = rest.get(4..6)?.parse::<i64>().ok()?;
            sign * (hours * 3600 + minutes * 60)
        }
    };
    // Days from civil, Howard Hinnant's algorithm.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hour * 3600 + minute * 60 + second - offset).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_timestamps_with_fractions_and_offsets() {
        assert_eq!(
            parse_rfc3339("2026-10-06T08:59:59.741585+00:00"),
            Some(1_791_277_199)
        );
        assert_eq!(
            parse_rfc3339("2026-10-06T04:59:59-04:00"),
            Some(1_791_277_199)
        );
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("garbage"), None);
    }

    #[test]
    fn exhausted_claude_limit_shows_when_it_resets() {
        let usage = Usage::parse_claude(&json!({
            "five_hour": {"utilization": 0.0, "resets_at": null},
            "seven_day": {"utilization": 100.0, "resets_at": "2026-10-06T08:59:59+00:00"},
        }))
        .unwrap();
        let now = parse_rfc3339("2026-10-05T13:09:59Z").unwrap();
        assert_eq!(
            usage.summary(now),
            ("claude week limit hit · resets in 19h 50m".into(), true)
        );
    }

    #[test]
    fn healthy_usage_shows_only_the_binding_window() {
        let usage = Usage::parse_claude(&json!({
            "five_hour": {"utilization": 38.4, "resets_at": "2026-10-05T15:00:00Z"},
            "seven_day": {"utilization": 72.0, "resets_at": "2026-10-06T08:59:59Z"},
        }))
        .unwrap();
        assert_eq!(usage.summary(0), ("claude week 72%".into(), false));
    }

    #[test]
    fn parses_codex_rate_limit_windows() {
        let usage = Usage::parse_codex(&json!({
            "plan_type": "plus",
            "rate_limit": {
                "primary_window": {"used_percent": 0, "reset_at": 1_791_227_364},
                "secondary_window": {"used_percent": 1, "reset_at": 1_791_636_247},
            },
        }))
        .unwrap();
        assert_eq!(usage.windows[1].resets_at, Some(1_791_636_247));
        assert_eq!(usage.summary(0), ("codex week 1%".into(), false));
    }

    #[test]
    fn responses_without_windows_are_not_usage() {
        assert_eq!(Usage::parse_claude(&json!({"error": "unauthorized"})), None);
        assert_eq!(Usage::parse_codex(&json!({"rate_limit": null})), None);
    }
}
