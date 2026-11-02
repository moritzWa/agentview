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
const DAY: u64 = 86_400;
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

    /// Codex names its windows by position rather than duration, and which
    /// duration lands in which slot depends on the plan: on Pro the primary
    /// window is the seven-day one and the secondary is absent, so trusting
    /// the position reports a weekly figure as "5h".
    pub fn parse_codex(body: &Value) -> Option<Self> {
        let window = |key: &str, positional| {
            let window = body.pointer(&format!("/rate_limit/{key}"))?;
            let label = window
                .get("limit_window_seconds")
                .and_then(Value::as_u64)
                .and_then(window_label)
                .unwrap_or(positional);
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
                .map(|at| format!(" ({} reset)", countdown(at.saturating_sub(now))))
                .unwrap_or_default();
            return (
                format!(
                    "{} {} {:.0}%{resets}",
                    self.provider, window.label, window.percent
                ),
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

#[derive(Clone, Debug, PartialEq)]
pub enum Pace {
    /// Too little of the window has passed for the average to mean anything.
    TooEarly,
    /// Projected use when the window resets.
    AtReset(f64),
    /// Unix seconds when the current average rate reaches 100%.
    HitsLimit(u64),
}

impl Window {
    fn length(&self) -> Option<u64> {
        match self.label {
            "5h" => Some(5 * 3600),
            "week" => Some(7 * 86_400),
            _ => None,
        }
    }

    /// Window length and the span the average rate is taken over, or `None`
    /// while too little of the window has passed.
    fn rate_span(&self, now: u64) -> Option<Option<(u64, u64)>> {
        let length = self.length()?;
        let elapsed = length - self.resets_at?.saturating_sub(now).min(length);
        // A tenth of the window, but never more than 6h: a week would otherwise
        // show nothing for its first 17 hours, right when you want a forecast.
        if elapsed < (length / 10).min(6 * 3600) {
            return Some(None);
        }
        // Use comes in bursts while you are awake, so a busy first evening
        // spread over the clock reads as 10x the week (39% in 6h showed
        // `→>999%`). Until a full day has passed, count the window's use so
        // far as that day's use instead.
        let span = if length > DAY {
            elapsed.max(DAY)
        } else {
            elapsed
        };
        Some(Some((length, span)))
    }

    /// Linear projection from the average rate since the window started.
    pub fn pace(&self, now: u64) -> Option<Pace> {
        if self.percent >= 100.0 {
            return None;
        }
        let Some((length, span)) = self.rate_span(now)? else {
            return Some(Pace::TooEarly);
        };
        let projected = self.percent * length as f64 / span as f64;
        if projected < 100.0 {
            return Some(Pace::AtReset(projected));
        }
        let per_second = self.percent / span as f64;
        Some(Pace::HitsLimit(
            now + ((100.0 - self.percent) / per_second) as u64,
        ))
    }

    /// Use at the reset if the average rate holds, past 100% included (the
    /// header shows `→307%` rather than a separate "runs out" form).
    pub fn projected_percent(&self, now: u64) -> Option<f64> {
        if self.percent >= 100.0 {
            return None;
        }
        let (length, span) = self.rate_span(now)??;
        Some(self.percent * length as f64 / span as f64)
    }

    /// `Sat 3:12 PM`, or a countdown where local time is unavailable.
    pub fn when(at: u64, now: u64) -> String {
        local_clock(at, now).unwrap_or_else(|| format!("in {}", countdown(at.saturating_sub(now))))
    }

    /// `2h 14m` and `6:51 PM` (`Mon 4:59 AM` past today); the clock is
    /// missing where local time is unavailable.
    pub fn reset_parts(&self, now: u64) -> Option<(String, Option<String>)> {
        let at = self.resets_at?;
        Some((countdown(at.saturating_sub(now)), local_clock(at, now)))
    }
}

#[cfg(unix)]
fn local_clock(at: u64, now: u64) -> Option<String> {
    fn local(seconds: u64) -> Option<libc::tm> {
        let time = libc::time_t::try_from(seconds).ok()?;
        let mut tm = unsafe { std::mem::zeroed::<libc::tm>() };
        (!unsafe { libc::localtime_r(&time, &mut tm) }.is_null()).then_some(tm)
    }
    let (then, today) = (local(at)?, local(now)?);
    let hour = match then.tm_hour % 12 {
        0 => 12,
        hour => hour,
    };
    let meridiem = if then.tm_hour < 12 { "AM" } else { "PM" };
    let clock = format!("{hour}:{:02} {meridiem}", then.tm_min);
    if (then.tm_year, then.tm_yday) == (today.tm_year, today.tm_yday) {
        return Some(clock);
    }
    let weekday = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
        .get(usize::try_from(then.tm_wday).ok()?)?;
    Some(format!("{weekday} {clock}"))
}

#[cfg(not(unix))]
fn local_clock(_at: u64, _now: u64) -> Option<String> {
    None
}

/// A label is `&'static str`, so only the durations Codex actually bills in
/// are named; anything else falls back to the caller's positional guess.
fn window_label(seconds: u64) -> Option<&'static str> {
    Some(match seconds {
        18_000 => "5h",
        86_400 => "day",
        604_800 => "week",
        2_592_000 => "month",
        _ => return None,
    })
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
    // Several dashboards may be open. Read credentials only after acquiring
    // the lock so they cannot simultaneously rotate the same refresh token.
    #[cfg(unix)]
    let _lock = claude_usage_lock()?;
    let keychain = keychain_claude_credentials();
    let path = home()?.join(".claude/.credentials.json");
    let credentials = keychain
        .clone()
        .or_else(|| std::fs::read_to_string(&path).ok())?;
    let mut credentials: Value = serde_json::from_str(&credentials).ok()?;
    if claude_token_expiring(&credentials, now()) {
        let refresh = credentials
            .pointer("/claudeAiOauth/refreshToken")?
            .as_str()?;
        let response = post_json(
            "https://platform.claude.com/v1/oauth/token",
            &serde_json::json!({
                "grant_type": "refresh_token",
                "refresh_token": refresh,
                "client_id": "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
            }),
        )?;
        apply_claude_refresh(&mut credentials, &response, now())?;
        // Persist rotated refresh tokens in the same store Claude Code uses.
        // Never put credentials in subprocess arguments or diagnostic output.
        let encoded = serde_json::to_string(&credentials).ok()?;
        if keychain.is_some() {
            save_keychain_claude_credentials(&encoded)?;
        } else {
            std::fs::write(path, encoded).ok()?;
        }
    }
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

#[cfg(unix)]
fn claude_usage_lock() -> Option<std::fs::File> {
    use std::os::fd::AsRawFd;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(home()?.join(".claude/.agentview-usage.lock"))
        .ok()?;
    (unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0).then_some(file)
}

fn claude_token_expiring(credentials: &Value, now: u64) -> bool {
    credentials
        .pointer("/claudeAiOauth/expiresAt")
        .and_then(Value::as_u64)
        .is_some_and(|expires| expires / 1000 <= now + 60)
}

fn apply_claude_refresh(credentials: &mut Value, response: &Value, now: u64) -> Option<()> {
    let access = response.get("access_token")?.as_str()?;
    let expires = response.get("expires_in")?.as_u64()?;
    let oauth = credentials.get_mut("claudeAiOauth")?.as_object_mut()?;
    oauth.insert("accessToken".into(), access.into());
    oauth.insert("expiresAt".into(), ((now + expires) * 1000).into());
    if let Some(refresh) = response.get("refresh_token").and_then(Value::as_str) {
        oauth.insert("refreshToken".into(), refresh.into());
    }
    Some(())
}

#[cfg(target_os = "macos")]
fn save_keychain_claude_credentials(credentials: &str) -> Option<()> {
    security_framework::passwords::set_generic_password(
        "Claude Code-credentials",
        &std::env::var("USER").ok()?,
        credentials.as_bytes(),
    )
    .ok()
}

#[cfg(not(target_os = "macos"))]
fn save_keychain_claude_credentials(_credentials: &str) -> Option<()> {
    None
}

fn post_json(url: &str, body: &Value) -> Option<Value> {
    use std::io::Write;
    let mut child = Command::new("curl")
        .args([
            "-sf",
            "--max-time",
            "10",
            "-H",
            "Content-Type: application/json",
            "--data-binary",
            "@-",
            url,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child
        .stdin
        .take()?
        .write_all(body.to_string().as_bytes())
        .ok()?;
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
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
    #[ignore = "uses the local Claude login and the live usage API"]
    fn live_claude_usage() {
        let usage = fetch_claude().expect("Claude usage must be available");
        assert_eq!(usage.provider, "claude");
        assert!(!usage.windows.is_empty());
        println!("{}", usage.summary(now()).0);
    }

    #[test]
    fn expired_claude_credentials_are_refreshed_without_losing_account_metadata() {
        let mut credentials = json!({"claudeAiOauth": {
            "accessToken": "expired", "refreshToken": "old-refresh", "expiresAt": 1000000,
            "subscriptionType": "max", "scopes": ["user:profile"]
        }, "other": true});
        assert!(!claude_token_expiring(&credentials, 939));
        assert!(claude_token_expiring(&credentials, 940));
        apply_claude_refresh(
            &mut credentials,
            &json!({
                "access_token": "new", "refresh_token": "rotated", "expires_in": 3600
            }),
            1000,
        )
        .unwrap();
        assert_eq!(credentials["claudeAiOauth"]["accessToken"], "new");
        assert_eq!(credentials["claudeAiOauth"]["refreshToken"], "rotated");
        assert_eq!(credentials["claudeAiOauth"]["expiresAt"], 4600000_u64);
        assert_eq!(credentials["claudeAiOauth"]["subscriptionType"], "max");
        assert_eq!(credentials["other"], true);
        assert!(!claude_token_expiring(&credentials, 1000));
        apply_claude_refresh(
            &mut credentials,
            &json!({
                "access_token": "newer", "expires_in": 3600
            }),
            2000,
        )
        .unwrap();
        assert_eq!(credentials["claudeAiOauth"]["refreshToken"], "rotated");
        let before = credentials.clone();
        assert_eq!(
            apply_claude_refresh(&mut credentials, &json!({"error": "invalid_grant"}), 2000),
            None
        );
        assert_eq!(credentials, before);
    }

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
            ("claude week 100% (19h 50m reset)".into(), true)
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
    fn reset_parts_count_down_to_the_reset() {
        let window = Window {
            label: "week",
            percent: 4.0,
            resets_at: Some(10_000 + 3 * 86_400 + 2 * 3600),
        };
        assert_eq!(window.reset_parts(10_000).unwrap().0, "3d 2h");
        let unknown = Window {
            resets_at: None,
            ..window
        };
        assert_eq!(unknown.reset_parts(0), None);
    }

    #[test]
    fn pace_projects_the_average_rate_to_the_reset() {
        let week = |percent, left| Window {
            label: "week",
            percent,
            resets_at: Some(1_000_000 + left),
        };
        let day = 86_400;
        // 2 of 7 days gone at 20%: 70% by the reset.
        let Some(Pace::AtReset(projected)) = week(20.0, 5 * day).pace(1_000_000) else {
            panic!("expected a projection");
        };
        assert!((projected - 70.0).abs() < 0.01, "{projected}");
        // 1 of 7 days gone at 25%: 100% three days in, i.e. 3 more days.
        assert_eq!(
            week(25.0, 6 * day).pace(1_000_000),
            Some(Pace::HitsLimit(1_000_000 + 3 * day))
        );
        assert_eq!(
            week(5.0, 7 * day - 3600).pace(1_000_000),
            Some(Pace::TooEarly)
        );
        // 8h into the week at 4%: projected without waiting for 17h to pass,
        // with the 4% counted as the first day's use (28%, not 84%).
        let Some(Pace::AtReset(early)) = week(4.0, 7 * day - 8 * 3600).pace(1_000_000) else {
            panic!("expected a projection 8h in");
        };
        assert!((early - 28.0).abs() < 0.01, "{early}");
        // The live case: 39% after a busy 6.4h is 273% by the reset, not >999%.
        let busy = week(39.0, 7 * day - 23_000)
            .projected_percent(1_000_000)
            .unwrap();
        assert!((busy - 273.0).abs() < 0.01, "{busy}");
        // At 39% a day, the remaining 61% lasts about 1.56 more days.
        let Some(Pace::HitsLimit(out)) = week(39.0, 7 * day - 23_000).pace(1_000_000) else {
            panic!("expected a limit hit");
        };
        assert!((out - 1_000_000).abs_diff(135_138) < 2, "{out}");
        // The 5h window keeps its tenth (30 minutes).
        let five_hour = |left: u64| Window {
            label: "5h",
            percent: 10.0,
            resets_at: Some(1_000_000 + left),
        };
        assert_eq!(
            five_hour(5 * 3600 - 600).pace(1_000_000),
            Some(Pace::TooEarly)
        );
        assert!(matches!(
            five_hour(5 * 3600 - 1800).pace(1_000_000),
            Some(Pace::HitsLimit(_))
        ));
        assert_eq!(week(100.0, day).pace(1_000_000), None);
        // The percent form keeps going past 100: 25% one day in is 175%.
        let over = week(25.0, 6 * day).projected_percent(1_000_000).unwrap();
        assert!((over - 175.0).abs() < 0.01, "{over}");
        assert_eq!(week(5.0, 7 * day - 3600).projected_percent(1_000_000), None);
    }

    #[test]
    fn codex_window_is_labelled_by_its_duration_not_its_slot() {
        // A Pro account: the seven-day window arrives in the primary slot,
        // which the positional labels would have reported as "5h".
        let usage = Usage::parse_codex(&json!({
            "plan_type": "prolite",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 4,
                    "limit_window_seconds": 604_800,
                    "reset_at": 1_792_363_618,
                },
                "secondary_window": null,
            },
        }))
        .unwrap();
        assert_eq!(usage.windows.len(), 1);
        assert_eq!(usage.summary(0), ("codex week 4%".into(), false));
    }

    #[test]
    fn responses_without_windows_are_not_usage() {
        assert_eq!(Usage::parse_claude(&json!({"error": "unauthorized"})), None);
        assert_eq!(Usage::parse_codex(&json!({"rate_limit": null})), None);
    }
}
