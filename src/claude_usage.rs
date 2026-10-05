//! Claude subscription usage for the footer.
//!
//! Reads the Claude Code OAuth token (macOS Keychain, else
//! `~/.claude/.credentials.json`) and polls the endpoint behind Claude Code's
//! `/usage`. The endpoint is undocumented, so every failure just leaves the
//! footer without a usage line.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const POLL_INTERVAL: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub percent: f64,
    /// Unix seconds.
    pub resets_at: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
}

impl Usage {
    pub fn parse(body: &Value) -> Option<Self> {
        let window = |key: &str| {
            let window = body.get(key)?;
            Some(Window {
                percent: window.get("utilization")?.as_f64()?,
                resets_at: window
                    .get("resets_at")
                    .and_then(Value::as_str)
                    .and_then(parse_rfc3339),
            })
        };
        let usage = Self {
            five_hour: window("five_hour"),
            seven_day: window("seven_day"),
        };
        (usage.five_hour.is_some() || usage.seven_day.is_some()).then_some(usage)
    }

    /// The footer text and whether a limit is (nearly) exhausted.
    pub fn summary(&self, now: u64) -> (String, bool) {
        let blocked = [("5h", &self.five_hour), ("weekly", &self.seven_day)]
            .into_iter()
            .filter_map(|(label, window)| Some((label, window.as_ref()?)))
            .filter(|(_, window)| window.percent >= 100.0)
            .max_by_key(|(_, window)| window.resets_at.unwrap_or(0));
        if let Some((label, window)) = blocked {
            let resets = window
                .resets_at
                .map(|at| format!(" · resets in {}", countdown(at.saturating_sub(now))))
                .unwrap_or_default();
            return (format!("claude {label} limit hit{resets}"), true);
        }
        let parts = [("5h", &self.five_hour), ("week", &self.seven_day)]
            .into_iter()
            .filter_map(|(label, window)| Some(format!("{label} {:.0}%", window.as_ref()?.percent)))
            .collect::<Vec<_>>();
        let high = [&self.five_hour, &self.seven_day]
            .into_iter()
            .flatten()
            .any(|window| window.percent >= 90.0);
        (format!("claude {}", parts.join(" · ")), high)
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Polls usage in the background; `take_update` hands the latest result to
/// the event loop.
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
            .name("claude-usage".into())
            .spawn(move || loop {
                if let Some(usage) = fetch() {
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

    pub fn take_update(&self) -> Option<Usage> {
        let mut latest = None;
        while let Ok(usage) = self.updates.try_recv() {
            latest = Some(usage);
        }
        latest
    }
}

impl Drop for UsageWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn fetch() -> Option<Usage> {
    let token = access_token()?;
    let mut child = Command::new("curl")
        .args(["-sf", "--max-time", "10", "-H", "@-", "-H"])
        .arg("anthropic-beta: oauth-2025-04-20")
        .arg(USAGE_URL)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // The token goes through stdin so it never shows up in `ps`.
    {
        use std::io::Write;
        let mut stdin = child.stdin.take()?;
        writeln!(stdin, "Authorization: Bearer {token}").ok()?;
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    Usage::parse(&serde_json::from_slice(&output.stdout).ok()?)
}

fn access_token() -> Option<String> {
    let credentials = keychain_credentials().or_else(|| {
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
        std::fs::read_to_string(std::path::Path::new(&home).join(".claude/.credentials.json")).ok()
    })?;
    let credentials: Value = serde_json::from_str(&credentials).ok()?;
    credentials
        .pointer("/claudeAiOauth/accessToken")?
        .as_str()
        .map(str::to_owned)
}

fn keychain_credentials() -> Option<String> {
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
    fn exhausted_limit_shows_when_it_resets() {
        let usage = Usage::parse(&json!({
            "five_hour": {"utilization": 0.0, "resets_at": null},
            "seven_day": {"utilization": 100.0, "resets_at": "2026-10-06T08:59:59+00:00"},
        }))
        .unwrap();
        let now = parse_rfc3339("2026-10-05T13:09:59Z").unwrap();
        assert_eq!(
            usage.summary(now),
            ("claude weekly limit hit · resets in 19h 50m".into(), true)
        );
    }

    #[test]
    fn healthy_usage_lists_both_windows() {
        let usage = Usage::parse(&json!({
            "five_hour": {"utilization": 38.4, "resets_at": "2026-10-05T15:00:00Z"},
            "seven_day": {"utilization": 72.0, "resets_at": "2026-10-06T08:59:59Z"},
        }))
        .unwrap();
        assert_eq!(usage.summary(0), ("claude 5h 38% · week 72%".into(), false));
    }

    #[test]
    fn missing_windows_are_not_usage() {
        assert_eq!(Usage::parse(&json!({"error": "unauthorized"})), None);
    }
}
