//! Provider-neutral native-TUI handoff with a dashboard detach key.
//!
//! Interactive provider clients run behind a private pseudo-terminal.
//! Plain Left and Right remain available to edit the provider's input line. At
//! a cursor boundary, the first arrow is still forwarded and opens a short,
//! visible return window; pressing the same arrow again returns to the
//! dashboard. OpenCode sessions return on that first Left instead.
//! Shift+Left and Shift+Right are immediate equivalents. In OpenCode, a Ctrl+X
//! that no other key follows returns too and asks the dashboard to treat it as
//! its own Ctrl+X on that row; a quick follow-up key keeps it OpenCode's
//! leader. The
//! provider process keeps running on its own pseudo-terminal, and a drain
//! thread holds the screen it produces. Selecting the same row attaches that
//! live screen again.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};
#[cfg(unix)]
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

const ESCAPE_FLUSH_DELAY: Duration = Duration::from_millis(30);
const ARROW_SETTLE_DELAY: Duration = Duration::from_millis(75);
const ARROW_RETURN_WINDOW: Duration = Duration::from_millis(1600);
/// How long a lone Ctrl+X waits for the rest of an OpenCode leader sequence
/// before it counts as the dashboard's removal key.
const LEADER_HOLD: Duration = Duration::from_millis(350);
const CTRL_X: u8 = 0x18;
const CTRL_K: u8 = 0x0b;
const RETURN_HINT_REFRESH: Duration = Duration::from_millis(100);
const EMPTY_PROMPT_MAX_COLUMN: u16 = 4;
const MAX_INITIAL_INPUT_BYTES: usize = 256 * 1024;
#[cfg(unix)]
const MAX_PENDING_INPUT_BYTES: usize = 64 * 1024;
#[cfg(unix)]
const FALLBACK_TERMINAL_ROWS: u16 = 24;
#[cfg(unix)]
const FALLBACK_TERMINAL_COLUMNS: u16 = 80;
#[cfg(unix)]
const SCREEN_PUBLISH_INTERVAL: Duration = Duration::from_millis(250);
/// How long a resumed OpenCode frontend sees a one-row-shorter terminal before
/// the real size returns. OpenCode ignores SIGWINCH when the size is unchanged
/// and debounces resizes for 100ms, so the hold must outlast that for both
/// sizes to be processed and a full repaint to happen.
#[cfg(unix)]
const RESUME_REDRAW_HOLD: Duration = Duration::from_millis(120);

#[derive(Debug)]
pub enum NativeSessionExit {
    Backgrounded,
    Exited(ExitStatus),
}

/// Generate a provider-neutral UUIDv4 for native CLIs that accept a caller-
/// supplied session identity. Keeping this here ensures a foreground launch
/// and its later dashboard row use the same unambiguous key.
pub fn new_session_id() -> Result<String> {
    let mut bytes = [0_u8; 16];
    getrandom::getrandom(&mut bytes).context("failed to generate a provider session ID")?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    ))
}

#[cfg(unix)]
struct DetachedSession {
    child: Option<std::process::Child>,
    warning: Option<String>,
    drain: Option<PtyDrain>,
}

/// Reads the provider's pseudo-terminal while the dashboard is in front, so a
/// long turn is not frozen and does not stall once the kernel buffer fills.
#[cfg(unix)]
struct PtyDrain {
    stop: Arc<AtomicBool>,
    /// Wakes the drain's poll so a resume does not wait out its timeout.
    wake: std::fs::File,
    done: thread::JoinHandle<(std::fs::File, vt100::Parser)>,
    contents: Arc<Mutex<String>>,
    /// The terminal title and when output last arrived, kept current on every
    /// read rather than every [`SCREEN_PUBLISH_INTERVAL`].
    title: Arc<Mutex<(String, Instant)>>,
}

#[cfg(unix)]
impl PtyDrain {
    fn halt(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = (&self.wake).write(&[0]);
    }
}

#[cfg(unix)]
static DETACHED: OnceLock<Mutex<BTreeMap<String, DetachedSession>>> = OnceLock::new();

/// Provisional launch keys renamed by [`rename_key`], mapped to their stable
/// key. A dashboard row can still carry the provisional key until its next
/// refresh, so lookups by that key must reach the renamed frontend.
#[cfg(unix)]
static RENAMED: OnceLock<Mutex<BTreeMap<String, String>>> = OnceLock::new();

/// The registry key that currently holds `session_key`'s frontend, following
/// renames. Returns `session_key` itself when it is held directly or unknown.
#[cfg(unix)]
fn current_key(session_key: &str) -> String {
    let Some(renamed) = RENAMED.get() else {
        return session_key.to_owned();
    };
    let Ok(renamed) = renamed.lock() else {
        return session_key.to_owned();
    };
    renamed
        .get(session_key)
        .cloned()
        .unwrap_or_else(|| session_key.to_owned())
}

static REMOVAL_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the last foreground session returned through the removal key,
/// clearing the request so it applies once.
pub fn take_removal_request() -> bool {
    REMOVAL_REQUESTED.swap(false, std::sync::atomic::Ordering::SeqCst)
}

/// Run or reattach one provider-native client. Non-TTY callers retain the
/// ordinary inherited-stdio behavior used by scripts and unit-test fixtures.
pub fn run(command: Command, session_key: &str) -> Result<NativeSessionExit> {
    run_with_warning(command, session_key, None)
}

/// Run a provider-native client while keeping agentview's dangerous-mode warning
/// visible in the terminal title for the full foreground handoff.
pub fn run_yolo(command: Command, session_key: &str, provider: &str) -> Result<NativeSessionExit> {
    run_with_warning(command, session_key, Some(yolo_warning(provider)))
}

fn run_with_warning(
    mut command: Command,
    session_key: &str,
    warning: Option<String>,
) -> Result<NativeSessionExit> {
    validate_session_key(session_key)?;
    #[cfg(unix)]
    if terminal_is_interactive() {
        return run_pty(command, session_key, None, warning);
    }
    if let Some(warning) = warning.as_deref() {
        eprintln!("{warning}");
    }
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    #[cfg(unix)]
    let status =
        status_retrying_text_busy(&mut command).context("failed to open provider session")?;
    #[cfg(not(unix))]
    let status = command
        .status()
        .context("failed to open provider session")?;
    Ok(NativeSessionExit::Exited(status))
}

fn yolo_warning(provider: &str) -> String {
    format!("⚠ YOLO MODE · {provider} permission safeguards are relaxed")
}

#[cfg(unix)]
fn status_retrying_text_busy(command: &mut Command) -> io::Result<ExitStatus> {
    const RETRIES: usize = 8;
    const RETRY_DELAY: Duration = Duration::from_millis(25);

    for attempt in 0..=RETRIES {
        match command.status() {
            Ok(status) => return Ok(status),
            Err(error) if error.raw_os_error() == Some(libc::ETXTBSY) && attempt < RETRIES => {
                thread::sleep(RETRY_DELAY);
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("the bounded provider spawn loop always returns")
}

/// Run a fresh provider-native client and submit input only after its parsed
/// screen contains an exact readiness marker. This is for native CLIs that do
/// not accept an initial interactive prompt argument. Login, workspace-trust,
/// and setup screens cannot receive the queued task because they do not render
/// the authenticated editor marker.
pub fn run_with_initial_input_after_screen(
    command: Command,
    session_key: &str,
    initial_input: &[u8],
    ready_marker: &str,
) -> Result<NativeSessionExit> {
    run_with_initial_input_after_screen_security(
        command,
        session_key,
        initial_input,
        ready_marker,
        None,
    )
}

/// Run a fresh provider-native client in explicit YOLO mode, while delaying
/// the initial input until the provider's authenticated editor is visible.
pub fn run_with_initial_input_after_screen_yolo(
    command: Command,
    session_key: &str,
    initial_input: &[u8],
    ready_marker: &str,
    provider: &str,
) -> Result<NativeSessionExit> {
    run_with_initial_input_after_screen_security(
        command,
        session_key,
        initial_input,
        ready_marker,
        Some(yolo_warning(provider)),
    )
}

fn run_with_initial_input_after_screen_security(
    command: Command,
    session_key: &str,
    initial_input: &[u8],
    ready_marker: &str,
    warning: Option<String>,
) -> Result<NativeSessionExit> {
    validate_session_key(session_key)?;
    if initial_input.is_empty() || initial_input.len() > MAX_INITIAL_INPUT_BYTES {
        bail!("provider-native initial input must contain 1 to {MAX_INITIAL_INPUT_BYTES} bytes");
    }
    if ready_marker.is_empty()
        || ready_marker.len() > 512
        || ready_marker.chars().any(char::is_control)
    {
        bail!("provider-native readiness marker is invalid");
    }
    #[cfg(unix)]
    {
        if !terminal_is_interactive() {
            bail!("screen-gated native input requires an interactive terminal");
        }
        run_pty(
            command,
            session_key,
            Some(ScreenTriggeredInput {
                bytes: initial_input.to_vec(),
                ready_marker: ready_marker.to_owned(),
                next: Vec::new(),
            }),
            warning,
        )
    }
    #[cfg(not(unix))]
    {
        let _ = command;
        bail!("screen-gated native input is unavailable on this platform")
    }
}

/// Resume an exact frontend previously backgrounded with a return gesture. Unlike
/// [`run`], this never starts a replacement command when the key is stale.
pub fn resume(session_key: &str) -> Result<NativeSessionExit> {
    validate_session_key(session_key)?;
    #[cfg(unix)]
    {
        if !terminal_is_interactive() {
            bail!("resuming a native session requires an interactive terminal");
        }
        let session_key = &current_key(session_key);
        let started = Instant::now();
        let detached =
            take_detached(session_key)?.context("the background terminal is no longer running")?;
        let (child, mut master, screen, warning) = detached.into_frontend()?;
        report_focus(&mut master, session_key, true);
        crate::perf!(
            "resume",
            "key={session_key} handoff={}",
            crate::perf_log::ms(started.elapsed())
        );
        bridge_session(child, master, screen, session_key, false, None, warning)
    }
    #[cfg(not(unix))]
    bail!("background terminal resume is unavailable on this platform")
}

/// Start a provider-native client straight into the background, as if it had
/// been opened and returned from, so a later [`resume`] shows it at once.
pub fn start_in_background(mut command: Command, session_key: &str) -> Result<()> {
    validate_session_key(session_key)?;
    #[cfg(unix)]
    {
        if !terminal_is_interactive() {
            bail!("a background native session requires an interactive terminal");
        }
        let mut registry = detached_registry()
            .lock()
            .map_err(|_| anyhow!("provider-native background registry lock was poisoned"))?;
        if registry.contains_key(session_key) {
            bail!("a provider-native frontend already uses this session key");
        }
        let (child, master) = spawn_pty(&mut command)?;
        let size = terminal_size(libc::STDIN_FILENO).unwrap_or(libc::winsize {
            ws_row: FALLBACK_TERMINAL_ROWS,
            ws_col: FALLBACK_TERMINAL_COLUMNS,
            ws_xpixel: 0,
            ws_ypixel: 0,
        });
        let drain = start_output_drain(
            master,
            vt100::Parser::new(size.ws_row, size.ws_col, 0),
            Some(session_key.to_owned()),
        )?;
        registry.insert(
            session_key.to_owned(),
            DetachedSession {
                child: Some(child),
                warning: None,
                drain: Some(drain),
            },
        );
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = command;
        bail!("background native sessions are unavailable on this platform")
    }
}

/// List process-local frontend keys for dashboard discovery. Keys never grant
/// authority over arbitrary processes: every entry was spawned by this agentview
/// process and is still held by its private PTY registry.
pub fn detached_session_keys() -> Vec<String> {
    #[cfg(unix)]
    {
        let Some(registry) = DETACHED.get() else {
            return Vec::new();
        };
        return registry
            .lock()
            .map(|registry| registry.keys().cloned().collect())
            .unwrap_or_default();
    }
    #[cfg(not(unix))]
    Vec::new()
}

/// The provider process ID and latest visible text of a frontend running
/// behind the dashboard, the text at most [`SCREEN_PUBLISH_INTERVAL`] old.
/// `None` when this process does not hold that frontend in the background or
/// its process has exited; an exited entry is dropped so its last screen is
/// never reported again.
pub fn background_screen_contents(session_key: &str) -> Option<(u32, String)> {
    #[cfg(unix)]
    {
        let session_key = &current_key(session_key);
        let mut registry = DETACHED.get()?.lock().ok()?;
        let session = registry.get_mut(session_key)?;
        let pid = session
            .child
            .as_mut()
            .and_then(|child| matches!(child.try_wait(), Ok(None)).then(|| child.id()));
        let Some(pid) = pid else {
            let dead = registry.remove(session_key);
            // Dropping a session joins its drain thread; never do that while
            // other dashboard calls wait on the registry.
            drop(registry);
            drop(dead);
            return None;
        };
        let contents = session.drain.as_ref()?.contents.lock().ok()?.clone();
        Some((pid, contents))
    }
    #[cfg(not(unix))]
    {
        let _ = session_key;
        None
    }
}

/// Wait until the frontend behind the dashboard sets a terminal title that
/// `matches` and then writes nothing for `settle`, so a frontend switched while
/// hidden is not shown on the screen it painted before. `false` when `timeout`
/// passes first, `still_wanted` turns false, or the frontend is not held.
pub fn wait_for_background_title(
    session_key: &str,
    matches: &dyn Fn(&str) -> bool,
    settle: Duration,
    timeout: Duration,
    still_wanted: &dyn Fn() -> bool,
) -> bool {
    #[cfg(unix)]
    {
        let title = {
            let session_key = &current_key(session_key);
            let Some(registry) = DETACHED.get().and_then(|registry| registry.lock().ok()) else {
                return false;
            };
            match registry
                .get(session_key)
                .and_then(|session| session.drain.as_ref())
            {
                Some(drain) => Arc::clone(&drain.title),
                None => return false,
            }
        };
        let started = Instant::now();
        let deadline = started + timeout;
        let mut first_match: Option<Duration> = None;
        let mut longest_quiet = Duration::ZERO;
        loop {
            let (matched, quiet, shown) = title
                .lock()
                .map(|slot| (matches(&slot.0), slot.1.elapsed(), slot.0.clone()))
                .unwrap_or((false, Duration::ZERO, String::new()));
            if matched {
                first_match.get_or_insert(started.elapsed());
                longest_quiet = longest_quiet.max(quiet);
            }
            let outcome = if matched && quiet >= settle {
                Some("drawn")
            } else if !still_wanted() {
                Some("superseded")
            } else if Instant::now() >= deadline {
                Some("timeout")
            } else {
                None
            };
            if let Some(outcome) = outcome {
                if crate::perf_log::enabled() {
                    crate::perf!(
                        "title-wait",
                        "key={session_key} outcome={outcome} elapsed={} first_match={} longest_quiet={} settle={} shown={shown:?}",
                        crate::perf_log::ms(started.elapsed()),
                        first_match.map_or_else(|| "never".into(), crate::perf_log::ms),
                        crate::perf_log::ms(longest_quiet),
                        crate::perf_log::ms(settle),
                    );
                }
                return outcome == "drawn";
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (session_key, matches, settle, timeout, still_wanted);
        false
    }
}

/// Whether this process currently retains the exact provider frontend.
pub fn is_backgrounded(session_key: &str) -> bool {
    #[cfg(unix)]
    {
        let session_key = &current_key(session_key);
        let Some(registry) = DETACHED.get() else {
            return false;
        };
        let Ok(mut registry) = registry.lock() else {
            return false;
        };
        let alive = registry
            .get_mut(session_key)
            .and_then(|session| session.child.as_mut())
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)));
        let dead = if alive {
            None
        } else {
            registry.remove(session_key)
        };
        // Dropping a session joins its drain thread; never do that while
        // other dashboard calls wait on the registry.
        drop(registry);
        drop(dead);
        alive
    }
    #[cfg(not(unix))]
    false
}

/// Stop one exact background frontend owned by this process.
pub fn terminate(session_key: &str) -> Result<()> {
    validate_session_key(session_key)?;
    #[cfg(unix)]
    {
        let mut detached = take_detached(&current_key(session_key))?
            .context("the background terminal is no longer running")?;
        terminate_detached(&mut detached);
        Ok(())
    }
    #[cfg(not(unix))]
    bail!("background terminal control is unavailable on this platform")
}

fn validate_session_key(session_key: &str) -> Result<()> {
    if session_key.is_empty()
        || session_key.len() > 512
        || session_key.chars().any(char::is_control)
    {
        bail!("invalid native session key");
    }
    Ok(())
}

#[cfg(test)]
mod id_tests {
    use super::*;

    #[test]
    fn generated_native_ids_are_distinct_uuid_v4_values() {
        let first = new_session_id().unwrap();
        let second = new_session_id().unwrap();
        assert_ne!(first, second);
        assert_eq!(first.len(), 36);
        assert_eq!(&first[14..15], "4");
        assert!(matches!(&first[19..20], "8" | "9" | "a" | "b"));
    }
}

/// Terminate detached native frontends during a normal dashboard shutdown.
/// Managed provider backends have separate verified ownership and are not
/// targeted here.
pub fn shutdown_all() {
    #[cfg(unix)]
    {
        let Some(registry) = DETACHED.get() else {
            return;
        };
        let sessions = match registry.lock() {
            Ok(mut registry) => std::mem::take(&mut *registry),
            Err(_) => return,
        };
        for (_, mut session) in sessions {
            terminate_detached(&mut session);
        }
    }
}

/// Move a detached provider frontend from a provisional launch key to the
/// stable normalized session key learned after the provider creates it.
pub fn rename_key(from: &str, to: &str) -> Result<()> {
    if from == to {
        return Ok(());
    }
    #[cfg(unix)]
    {
        let Some(registry) = DETACHED.get() else {
            return Ok(());
        };
        let mut registry = registry.lock().map_err(|_| {
            anyhow!("provider-native background session registry lock was poisoned")
        })?;
        if registry.contains_key(to) {
            bail!("a provider-native frontend already uses the stable session key");
        }
        if let Some(session) = registry.remove(from) {
            registry.insert(to.to_owned(), session);
            let mut renamed = RENAMED
                .get_or_init(|| Mutex::new(BTreeMap::new()))
                .lock()
                .map_err(|_| anyhow!("provider-native session rename lock was poisoned"))?;
            for target in renamed.values_mut().filter(|target| *target == from) {
                *target = to.to_owned();
            }
            renamed.insert(from.to_owned(), to.to_owned());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn terminal_is_interactive() -> bool {
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 && libc::isatty(libc::STDOUT_FILENO) == 1 }
}

#[cfg(unix)]
fn run_pty(
    mut command: Command,
    session_key: &str,
    initial_input: Option<ScreenTriggeredInput>,
    warning: Option<String>,
) -> Result<NativeSessionExit> {
    let session_key = &current_key(session_key);
    let detached = take_detached(session_key)?;
    let (child, master, screen, fresh, warning) = match detached {
        Some(detached) => {
            let (child, master, screen, warning) = detached.into_frontend()?;
            (child, master, screen, false, warning)
        }
        None => {
            clear_physical_screen()?;
            let (child, master) = spawn_pty(&mut command)?;
            let size = terminal_size(libc::STDIN_FILENO).unwrap_or(libc::winsize {
                ws_row: 24,
                ws_col: 80,
                ws_xpixel: 0,
                ws_ypixel: 0,
            });
            (
                child,
                master,
                vt100::Parser::new(size.ws_row, size.ws_col, 0),
                true,
                warning,
            )
        }
    };
    bridge_session(
        child,
        master,
        screen,
        session_key,
        fresh,
        fresh.then_some(initial_input).flatten(),
        warning,
    )
}

#[cfg(unix)]
struct ScreenTriggeredInput {
    bytes: Vec<u8>,
    ready_marker: String,
    next: Vec<(String, Vec<u8>)>,
}

/// Drive native slash commands only as each corresponding screen becomes ready.
/// Never use fixed delays: login and trust prompts must remain interactive.
/// Newline-separated marker fragments must all be present on the same screen.
pub fn run_with_screen_steps(
    command: Command,
    session_key: &str,
    steps: Vec<(String, Vec<u8>)>,
) -> Result<NativeSessionExit> {
    validate_session_key(session_key)?;
    if steps.is_empty()
        || steps.len() > 8
        || steps.iter().any(|(marker, bytes)| {
            marker.is_empty()
                || marker.len() > 512
                || marker.lines().any(str::is_empty)
                || marker.chars().any(|ch| ch.is_control() && ch != '\n')
                || bytes.is_empty()
                || bytes.len() > MAX_INITIAL_INPUT_BYTES
        })
    {
        bail!("invalid provider-native screen steps");
    }
    #[cfg(unix)]
    {
        if !terminal_is_interactive() {
            bail!("screen-gated native input requires an interactive terminal");
        }
        let mut steps = steps;
        let (ready_marker, bytes) = steps.remove(0);
        run_pty(
            command,
            session_key,
            Some(ScreenTriggeredInput {
                bytes,
                ready_marker,
                next: steps,
            }),
            None,
        )
    }
    #[cfg(not(unix))]
    {
        let _ = command;
        bail!("screen-gated native input is unavailable on this platform");
    }
}

#[cfg(unix)]
fn take_detached(session_key: &str) -> Result<Option<DetachedSession>> {
    let detached = detached_registry()
        .lock()
        .map_err(|_| anyhow!("provider-native background session registry lock was poisoned"))?
        .remove(session_key);
    match detached {
        Some(mut detached) => {
            let alive = detached
                .child
                .as_mut()
                .is_some_and(|child| matches!(child.try_wait(), Ok(None)));
            if alive {
                Ok(Some(detached))
            } else {
                Ok(None)
            }
        }
        None => Ok(None),
    }
}

#[cfg(unix)]
fn terminate_detached(session: &mut DetachedSession) {
    let Some(child) = session.child.as_mut() else {
        return;
    };
    signal_group(child.id(), libc::SIGCONT);
    signal_group(child.id(), libc::SIGTERM);
    let deadline = Instant::now() + Duration::from_millis(300);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Ok(None) | Err(_) => {
                signal_group(child.id(), libc::SIGKILL);
                // Never turn dashboard shutdown into an unbounded wait. The
                // exact child has received an uncatchable signal above; reap
                // it opportunistically while keeping cleanup latency bounded.
                let reap_deadline = Instant::now() + Duration::from_secs(1);
                while Instant::now() < reap_deadline {
                    if matches!(child.try_wait(), Ok(Some(_))) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                break;
            }
        }
    }
}

#[cfg(unix)]
fn detached_registry() -> &'static Mutex<BTreeMap<String, DetachedSession>> {
    DETACHED.get_or_init(|| Mutex::new(BTreeMap::new()))
}

#[cfg(unix)]
fn spawn_pty(command: &mut Command) -> Result<(std::process::Child, std::fs::File)> {
    let program = command.get_program().to_string_lossy().into_owned();
    let working_directory = command
        .get_current_dir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    if command.get_current_dir().is_some() && !working_directory.is_dir() {
        bail!(
            "provider-native working directory does not exist: {}",
            working_directory.display()
        );
    }
    if Path::new(&program).components().count() > 1 && !Path::new(&program).is_file() {
        bail!("provider-native executable does not exist: {program}");
    }
    let mut master_fd = -1;
    let mut slave_fd = -1;
    let mut size = terminal_size(libc::STDIN_FILENO).unwrap_or(libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    });
    let size_ptr = &mut size as *mut libc::winsize;
    let opened = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            size_ptr,
        )
    };
    if opened != 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to open provider pseudo-terminal");
    }
    set_close_on_exec(master_fd)?;
    set_close_on_exec(slave_fd)?;
    let master = unsafe { std::fs::File::from_raw_fd(master_fd) };
    let slave = unsafe { std::fs::File::from_raw_fd(slave_fd) };
    command
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            #[cfg(target_os = "linux")]
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGHUP) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().with_context(|| {
        format!(
            "failed to start provider-native client {program} in {}",
            working_directory.display()
        )
    })?;
    Ok((child, master))
}

#[cfg(unix)]
fn bridge_session(
    mut child: std::process::Child,
    mut master: std::fs::File,
    mut screen: vt100::Parser,
    session_key: &str,
    fresh: bool,
    mut initial_input: Option<ScreenTriggeredInput>,
    warning: Option<String>,
) -> Result<NativeSessionExit> {
    let _raw = RawModeGuard::enter()?;
    let _title = NativeTitleGuard::enter(warning.as_deref())?;
    let mut stdout = io::stdout().lock();
    let mut current_size = terminal_size(libc::STDIN_FILENO).ok();
    if let Some(size) = current_size {
        // The saved screen keeps the size it last had in the foreground. If
        // the window changed since, it must match before it is shown, and
        // before the provider's next frame is parsed into it.
        screen.set_size(size.ws_row, size.ws_col);
        set_pty_size(master.as_raw_fd(), size)?;
    }
    if !fresh {
        stdout.write_all(b"\x1b[2J\x1b[H")?;
        stdout.write_all(&screen.screen().state_formatted())?;
        stdout.flush()?;
        signal_group(child.id(), libc::SIGCONT);
    }
    if fresh {
        if let Some(warning) = warning.as_deref() {
            writeln!(stdout, "\x1b[1;33m{warning}\x1b[0m\r")?;
            stdout.flush()?;
        }
    }
    REMOVAL_REQUESTED.store(false, std::sync::atomic::Ordering::SeqCst);
    let mut parser = DetachParser::for_session(session_key);
    let mut return_gesture = ReturnGesture::for_session(session_key);
    let mut redraw_restore_at = None;
    let mut hidden_queries = TerminalQueryScanner::default();
    let mut color_queries = answers_color_queries(session_key).then(TerminalQueryScanner::default);
    let mut pending_input = PendingInput::default();
    // Resuming shows the saved screen as is: forcing OpenCode to repaint
    // costs about a quarter second on every open. If the terminal ever
    // disagrees with OpenCode's diff renderer, Ctrl+K forces the repaint.
    if !fresh {
        signal_group(child.id(), libc::SIGWINCH);
    }
    let bridged_at = Instant::now();
    crate::perf!("bridge-start", "key={session_key} fresh={fresh}");
    let mut echo_wait: Option<Instant> = None;
    let mut echo_samples = 0_u8;
    loop {
        let mut descriptors = [
            libc::pollfd {
                fd: master.as_raw_fd(),
                events: if pending_input.is_empty() {
                    libc::POLLIN
                } else {
                    libc::POLLIN | libc::POLLOUT
                },
                revents: 0,
            },
            libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: if pending_input.is_full() {
                    0
                } else {
                    libc::POLLIN
                },
                revents: 0,
            },
        ];
        let polled = unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, 25) };
        if polled < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error).context("provider-native terminal poll failed");
            }
        }
        if descriptors[0].revents & libc::POLLIN != 0 {
            if let Some(sent) = echo_wait.take() {
                crate::perf!(
                    "echo",
                    "key={session_key} latency={} since_bridge={}",
                    crate::perf_log::ms(sent.elapsed()),
                    crate::perf_log::ms(bridged_at.elapsed()),
                );
            }
            if redraw_restore_at.is_some() {
                absorb_available(&mut master, &mut screen, &mut hidden_queries)?;
            } else {
                copy_available(
                    &mut master,
                    &mut stdout,
                    &mut screen,
                    color_queries.as_mut(),
                )?;
            }
            forward_ready_initial_input(&mut initial_input, &screen, &mut pending_input)?;
        }
        let mut detach = false;
        if descriptors[1].revents & libc::POLLIN != 0 {
            let mut input = [0_u8; 256];
            let read =
                unsafe { libc::read(libc::STDIN_FILENO, input.as_mut_ptr().cast(), input.len()) };
            match read.cmp(&0) {
                std::cmp::Ordering::Less => {
                    let error = std::io::Error::last_os_error();
                    if error.kind() != std::io::ErrorKind::Interrupted {
                        return Err(error).context("failed to read provider-native keyboard input");
                    }
                }
                std::cmp::Ordering::Equal => {}
                std::cmp::Ordering::Greater => {
                    for action in parser.push(&input[..read as usize]) {
                        match action {
                            InputAction::Forward(bytes) => {
                                if echo_wait.is_none() && echo_samples < 5 {
                                    echo_samples += 1;
                                    echo_wait = Some(Instant::now());
                                    crate::perf!(
                                        "input",
                                        "key={session_key} since_bridge={}",
                                        crate::perf_log::ms(bridged_at.elapsed()),
                                    );
                                }
                                return_gesture.clear(&mut stdout, &screen)?;
                                pending_input.write_all(&bytes)?;
                            }
                            InputAction::Arrow(direction, bytes) => {
                                if return_gesture.should_detach(direction, &screen)
                                    || return_gesture.takes_left_over(direction, &screen)
                                {
                                    detach = true;
                                    break;
                                }
                                return_gesture.clear(&mut stdout, &screen)?;
                                let cursor = screen.screen().cursor_position();
                                pending_input.write_all(bytes)?;
                                // Claude and a few other TUIs use Left at an
                                // empty, left-margin prompt to change their own
                                // view. Preserve the agentview second-press window in
                                // that one boundary case even if they redraw.
                                return_gesture.begin_probe(
                                    direction,
                                    cursor,
                                    direction == ArrowDirection::Left
                                        && cursor.1 <= EMPTY_PROMPT_MAX_COLUMN,
                                );
                            }
                            InputAction::Detach => {
                                detach = true;
                                break;
                            }
                            InputAction::Redraw => {
                                // OpenCode ignores a resize to its current
                                // size, so show it one row shorter first.
                                // Output at that size stays hidden; only the
                                // repaint at the real size is shown.
                                if let Some(size) = current_size.filter(|size| size.ws_row > 1) {
                                    set_pty_size(
                                        master.as_raw_fd(),
                                        libc::winsize {
                                            ws_row: size.ws_row - 1,
                                            ..size
                                        },
                                    )?;
                                    signal_group(child.id(), libc::SIGWINCH);
                                    redraw_restore_at = Some(Instant::now() + RESUME_REDRAW_HOLD);
                                }
                            }
                        }
                    }
                }
            }
        }
        if !detach {
            if let Some(bytes) = parser.flush_expired() {
                return_gesture.clear(&mut stdout, &screen)?;
                pending_input.write_all(&bytes)?;
            }
            detach = return_gesture.update(&mut stdout, &screen)?;
        }
        if !detach && parser.take_expired_leader() {
            REMOVAL_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
            detach = true;
        }
        pending_input.write_to(&mut master)?;
        if detach {
            // The provider keeps its controlling terminal, which is the
            // private pseudo-terminal, not the dashboard's. Stopping it here
            // aborts an in-flight model request.
            restore_dashboard_terminal_modes(&mut stdout)?;
            report_focus(&mut master, session_key, false);
            let drain = start_output_drain(master, screen, None)?;
            detached_registry()
                .lock()
                .map_err(|_| anyhow!("provider-native background registry lock was poisoned"))?
                .insert(
                    session_key.to_owned(),
                    DetachedSession {
                        child: Some(child),
                        warning,
                        drain: Some(drain),
                    },
                );
            return Ok(NativeSessionExit::Backgrounded);
        }
        if redraw_restore_at.is_some_and(|at| Instant::now() >= at) {
            redraw_restore_at = None;
            // A busy provider can read both size changes at once, see no net
            // change, and skip the repaint, so show the hidden output first.
            stdout.write_all(b"\x1b[2J\x1b[H")?;
            stdout.write_all(&screen.screen().state_formatted())?;
            stdout.flush()?;
            if let Some(size) = current_size {
                set_pty_size(master.as_raw_fd(), size)?;
                signal_group(child.id(), libc::SIGWINCH);
            }
        }
        if let Ok(size) = terminal_size(libc::STDIN_FILENO) {
            if current_size
                .map(|current| !same_terminal_size(current, size))
                .unwrap_or(true)
            {
                set_pty_size(master.as_raw_fd(), size)?;
                screen.set_size(size.ws_row, size.ws_col);
                current_size = Some(size);
            }
        }
        if let Some(status) = child.try_wait()? {
            copy_available(&mut master, &mut stdout, &mut screen, None)?;
            return Ok(NativeSessionExit::Exited(status));
        }
    }
}

#[cfg(unix)]
struct NativeTitleGuard;

#[cfg(unix)]
impl NativeTitleGuard {
    fn enter(warning: Option<&str>) -> Result<Self> {
        if let Some(warning) = warning {
            let mut output = io::stdout().lock();
            let warning = warning.replace(['\x07', '\x1b'], "");
            write!(output, "\x1b]0;{warning}\x07")?;
            output.flush()?;
        }
        Ok(Self)
    }
}

#[cfg(unix)]
impl Drop for NativeTitleGuard {
    fn drop(&mut self) {
        let _ = io::stdout().write_all(b"\x1b]0;agentview\x07");
        let _ = io::stdout().flush();
    }
}

#[cfg(unix)]
fn forward_ready_initial_input(
    pending: &mut Option<ScreenTriggeredInput>,
    screen: &vt100::Parser,
    output: &mut impl Write,
) -> Result<bool> {
    let Some(input) = pending.as_ref() else {
        return Ok(false);
    };
    let contents = screen.screen().contents();
    if !input
        .ready_marker
        .lines()
        .all(|marker| contents.contains(marker))
    {
        return Ok(false);
    }
    output.write_all(&input.bytes)?;
    output.flush()?;
    let mut remaining = pending.take().expect("pending input exists").next;
    if !remaining.is_empty() {
        let (ready_marker, bytes) = remaining.remove(0);
        *pending = Some(ScreenTriggeredInput {
            bytes,
            ready_marker,
            next: remaining,
        });
    }
    Ok(true)
}

#[cfg(unix)]
fn restore_dashboard_terminal_modes(stdout: &mut impl Write) -> Result<()> {
    stdout.write_all(b"\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[<u\x1b[?25h")?;
    stdout.flush()?;
    Ok(())
}

/// Pause after an unexpected poll or read error before trying again. Giving
/// up would leave nobody reading the pseudo-terminal, and the provider would
/// block once the kernel buffer fills.
#[cfg(unix)]
const DRAIN_ERROR_BACKOFF: Duration = Duration::from_millis(50);

#[cfg(unix)]
/// `blur_once_started` sends an OpenCode frontend started behind the dashboard
/// its focus-out with its first output: before then its terminal may still
/// echo input instead of reading it.
fn start_output_drain(
    mut master: std::fs::File,
    mut screen: vt100::Parser,
    mut blur_once_started: Option<String>,
) -> Result<PtyDrain> {
    set_nonblocking(master.as_raw_fd(), true)?;
    let (woken, wake) = wake_pipe()?;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let contents = Arc::new(Mutex::new(screen.screen().contents()));
    let published = Arc::clone(&contents);
    let title = Arc::new(Mutex::new((
        screen.screen().title().to_owned(),
        Instant::now(),
    )));
    let latest_title = Arc::clone(&title);
    let done = thread::Builder::new()
        .name("native-pty-drain".into())
        .spawn(move || {
            let publish = |screen: &vt100::Parser| {
                if let Ok(mut slot) = published.lock() {
                    *slot = screen.screen().contents();
                }
            };
            let mut bytes = [0_u8; 8192];
            let mut queries = TerminalQueryScanner::default();
            let mut changed = false;
            let mut last_publish = Instant::now();
            'drain: loop {
                if flag.load(Ordering::Relaxed) {
                    break;
                }
                // Publish the settled screen for dashboard status without
                // re-rendering it on every byte of a streaming reply.
                if changed && last_publish.elapsed() >= SCREEN_PUBLISH_INTERVAL {
                    publish(&screen);
                    changed = false;
                    last_publish = Instant::now();
                }
                let mut descriptors = [
                    libc::pollfd {
                        fd: master.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: woken.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];
                let polled = unsafe { libc::poll(descriptors.as_mut_ptr(), 2, 50) };
                let descriptor = descriptors[0];
                if polled < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() != std::io::ErrorKind::Interrupted {
                        thread::sleep(DRAIN_ERROR_BACKOFF);
                    }
                    continue;
                }
                if descriptor.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) == 0 {
                    continue;
                }
                changed = true;
                if let Some(session_key) = blur_once_started.take() {
                    report_focus(&mut master, &session_key, false);
                }
                loop {
                    match master.read(&mut bytes) {
                        Ok(0) => break 'drain,
                        Ok(count) => {
                            process_detached_output(
                                &bytes[..count],
                                &mut screen,
                                &mut queries,
                                &mut master,
                            );
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        // EIO: every process holding the terminal has exited.
                        Err(error) if error.raw_os_error() == Some(libc::EIO) => {
                            break 'drain;
                        }
                        Err(_) => {
                            thread::sleep(DRAIN_ERROR_BACKOFF);
                            break;
                        }
                    }
                }
                if let Ok(mut slot) = latest_title.lock() {
                    let current = screen.screen().title();
                    if slot.0 != current {
                        current.clone_into(&mut slot.0);
                    }
                    slot.1 = Instant::now();
                }
            }
            loop {
                match master.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(count) => screen.process(&bytes[..count]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => break,
                }
            }
            publish(&screen);
            (master, screen)
        })
        .context("failed to keep the provider terminal running")?;
    Ok(PtyDrain {
        stop,
        wake,
        done,
        contents,
        title,
    })
}

/// A close-on-exec pipe, read end first, so provider children never hold it.
#[cfg(unix)]
fn wake_pipe() -> Result<(std::fs::File, std::fs::File)> {
    let mut fds = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to create a wake pipe");
    }
    let (read, write) = unsafe {
        (
            std::fs::File::from_raw_fd(fds[0]),
            std::fs::File::from_raw_fd(fds[1]),
        )
    };
    for fd in fds {
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(std::io::Error::last_os_error()).context("failed to configure a wake pipe");
        }
    }
    Ok((read, write))
}

/// Feed detached output to the screen and answer the terminal queries a
/// visible terminal would, so a provider that asks for its cursor position
/// does not stall until its own timeout while the dashboard is in front.
#[cfg(unix)]
fn process_detached_output(
    bytes: &[u8],
    screen: &mut vt100::Parser,
    queries: &mut TerminalQueryScanner,
    reply: &mut impl Write,
) {
    let mut processed = 0;
    for (index, byte) in bytes.iter().enumerate() {
        let Some(query) = queries.feed(*byte) else {
            continue;
        };
        // Answer from the screen exactly as it stood when the query arrived.
        screen.process(&bytes[processed..=index]);
        processed = index + 1;
        let answer = match query {
            TerminalQuery::CursorPosition => {
                let (row, column) = screen.screen().cursor_position();
                Some(format!(
                    "\x1b[{};{}R",
                    u32::from(row) + 1,
                    u32::from(column) + 1
                ))
            }
            TerminalQuery::PrimaryAttributes => Some("\x1b[?1;2c".to_owned()),
            TerminalQuery::Color(code) => crate::theme::osc_color_reply(code),
        };
        if let Some(answer) = answer {
            let _ = reply.write_all(answer.as_bytes());
        }
    }
    screen.process(&bytes[processed..]);
}

/// Answer OpenCode's foreground and background color queries from the
/// dashboard's scheme. OpenCode falls back to its dark palette when the
/// terminal's replies are late, and duplicate replies from the terminal are
/// ignored once it has decided.
#[cfg(unix)]
fn answer_color_queries(bytes: &[u8], queries: &mut TerminalQueryScanner, reply: &mut impl Write) {
    for byte in bytes {
        if let Some(TerminalQuery::Color(code)) = queries.feed(*byte) {
            if let Some(answer) = crate::theme::osc_color_reply(code) {
                let _ = reply.write_all(answer.as_bytes());
            }
        }
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalQuery {
    /// `CSI 6 n`
    CursorPosition,
    /// `CSI c` or `CSI 0 c`
    PrimaryAttributes,
    /// `OSC 10 ; ?` or `OSC 11 ; ?`
    Color(u8),
}

/// Incremental CSI and OSC scanner, so a query split across two reads is
/// still seen.
#[cfg(unix)]
#[derive(Default)]
struct TerminalQueryScanner {
    state: QueryScanState,
    parameters: Vec<u8>,
}

#[cfg(unix)]
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum QueryScanState {
    #[default]
    Ground,
    Escape,
    Csi,
    Osc,
    OscEscape,
}

#[cfg(unix)]
impl TerminalQueryScanner {
    const MAX_PARAMETERS: usize = 16;

    fn osc_query(&self) -> Option<TerminalQuery> {
        match self.parameters.as_slice() {
            b"10;?" => Some(TerminalQuery::Color(10)),
            b"11;?" => Some(TerminalQuery::Color(11)),
            _ => None,
        }
    }

    fn feed(&mut self, byte: u8) -> Option<TerminalQuery> {
        match self.state {
            QueryScanState::Ground => {
                if byte == 0x1b {
                    self.state = QueryScanState::Escape;
                }
                None
            }
            QueryScanState::Escape => {
                self.state = match byte {
                    b'[' => {
                        self.parameters.clear();
                        QueryScanState::Csi
                    }
                    b']' => {
                        self.parameters.clear();
                        QueryScanState::Osc
                    }
                    0x1b => QueryScanState::Escape,
                    _ => QueryScanState::Ground,
                };
                None
            }
            QueryScanState::Osc => match byte {
                0x07 => {
                    self.state = QueryScanState::Ground;
                    self.osc_query()
                }
                0x1b => {
                    self.state = QueryScanState::OscEscape;
                    None
                }
                _ => {
                    // Long OSC payloads (titles, hyperlinks) are never queries.
                    if self.parameters.len() < Self::MAX_PARAMETERS {
                        self.parameters.push(byte);
                    } else {
                        self.parameters.clear();
                        self.parameters.push(b'!');
                    }
                    None
                }
            },
            QueryScanState::OscEscape => {
                if byte == b'\\' {
                    self.state = QueryScanState::Ground;
                    return self.osc_query();
                }
                self.state = QueryScanState::Escape;
                self.feed(byte)
            }
            QueryScanState::Csi => match byte {
                0x30..=0x3f if self.parameters.len() < Self::MAX_PARAMETERS => {
                    self.parameters.push(byte);
                    None
                }
                0x40..=0x7e => {
                    self.state = QueryScanState::Ground;
                    match (byte, self.parameters.as_slice()) {
                        (b'n', b"6") => Some(TerminalQuery::CursorPosition),
                        (b'c', b"" | b"0") => Some(TerminalQuery::PrimaryAttributes),
                        _ => None,
                    }
                }
                0x1b => {
                    self.state = QueryScanState::Escape;
                    None
                }
                _ => {
                    self.state = QueryScanState::Ground;
                    None
                }
            },
        }
    }
}

#[cfg(unix)]
impl DetachedSession {
    fn into_frontend(
        mut self,
    ) -> Result<(
        std::process::Child,
        std::fs::File,
        vt100::Parser,
        Option<String>,
    )> {
        let child = self
            .child
            .take()
            .context("detached provider terminal has no process")?;
        let drain = self
            .drain
            .take()
            .context("detached provider terminal has no output drain")?;
        drain.halt();
        let (master, screen) = drain
            .done
            .join()
            .map_err(|_| anyhow!("provider terminal drain stopped unexpectedly"))?;
        Ok((child, master, screen, self.warning.take()))
    }
}

#[cfg(unix)]
impl Drop for DetachedSession {
    fn drop(&mut self) {
        if let Some(drain) = self.drain.take() {
            drain.halt();
            let _ = drain.done.join();
        }
    }
}

#[cfg(unix)]
fn copy_available(
    master: &mut std::fs::File,
    output: &mut impl Write,
    screen: &mut vt100::Parser,
    mut color_queries: Option<&mut TerminalQueryScanner>,
) -> Result<()> {
    set_nonblocking(master.as_raw_fd(), true)?;
    let mut bytes = [0_u8; 8192];
    loop {
        match master.read(&mut bytes) {
            Ok(0) => break,
            Ok(count) => {
                screen.process(&bytes[..count]);
                output.write_all(&bytes[..count])?;
                if let Some(queries) = color_queries.as_deref_mut() {
                    answer_color_queries(&bytes[..count], queries, master);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
            Err(error) => return Err(error.into()),
        }
    }
    output.flush()?;
    Ok(())
}

/// Keyboard bytes waiting for the provider to read them. Reading output leaves
/// the pseudo-terminal non-blocking and its input queue holds only about a
/// kilobyte on macOS, so a large paste or initial task has to wait here until
/// the provider catches up instead of failing the write.
#[cfg(unix)]
#[derive(Default)]
struct PendingInput {
    bytes: std::collections::VecDeque<u8>,
}

#[cfg(unix)]
impl PendingInput {
    fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Stop reading the keyboard past this, so a huge paste waits in the
    /// outer terminal rather than in memory.
    fn is_full(&self) -> bool {
        self.bytes.len() >= MAX_PENDING_INPUT_BYTES
    }

    fn write_to(&mut self, master: &mut impl Write) -> Result<()> {
        while !self.bytes.is_empty() {
            let (front, _) = self.bytes.as_slices();
            match master.write(front) {
                Ok(0) => break,
                Ok(written) => {
                    self.bytes.drain(..written);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                // The provider is gone; the exit check reports that.
                Err(error) if error.raw_os_error() == Some(libc::EIO) => {
                    self.bytes.clear();
                }
                Err(error) => {
                    return Err(error).context("failed to forward keyboard input to the provider")
                }
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
impl Write for PendingInput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.extend(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Read provider output into the screen model without drawing it, answering
/// terminal queries the way the background drain does.
#[cfg(unix)]
fn absorb_available(
    master: &mut std::fs::File,
    screen: &mut vt100::Parser,
    queries: &mut TerminalQueryScanner,
) -> Result<()> {
    set_nonblocking(master.as_raw_fd(), true)?;
    let mut bytes = [0_u8; 8192];
    loop {
        match master.read(&mut bytes) {
            Ok(0) => break,
            Ok(count) => process_detached_output(&bytes[..count], screen, queries, master),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Only OpenCode is known to repaint its whole screen on a real resize, which
/// hiding its output at the temporary size depends on. It leaves Ctrl+K
/// unbound, so agentview can take it.
fn redraws_on_ctrl_k(session_key: &str) -> bool {
    session_key.starts_with("opencode:")
}

/// Tell a frontend whether it is the one on screen, as a terminal reporting
/// focus would, so OpenCode defers work it only needs in front while it runs
/// behind the dashboard. Only OpenCode: its terminal library takes these as
/// focus changes, where another provider could read them as typed keys.
#[cfg(unix)]
fn report_focus(master: &mut std::fs::File, session_key: &str, focused: bool) {
    if session_key.starts_with("opencode:") {
        let _ = master.write_all(if focused { b"\x1b[I" } else { b"\x1b[O" });
    }
}

/// Other providers would read a second reply, the terminal's own, as typed
/// input, so only OpenCode gets local color answers while it is in front.
#[cfg(unix)]
fn answers_color_queries(session_key: &str) -> bool {
    session_key.starts_with("opencode:")
}

#[cfg(unix)]
fn signal_group(pid: u32, signal: libc::c_int) {
    let pid = pid as libc::pid_t;
    // A provider spawned through `setsid` should be its process-group leader,
    // but macOS can report ESRCH for the group during a rapid stop/continue
    // transition while the child itself is still waitable. The direct fallback
    // remains scoped to the exact child agentview spawned and prevents shutdown from
    // waiting forever on a frontend that never received the signal.
    if unsafe { libc::kill(-pid, signal) } < 0 {
        let _ = unsafe { libc::kill(pid, signal) };
    }
}

#[cfg(unix)]
fn clear_physical_screen() -> Result<()> {
    let mut output = io::stdout().lock();
    output.write_all(b"\x1b[2J\x1b[H")?;
    output.flush()?;
    Ok(())
}

#[cfg(unix)]
fn terminal_size(fd: libc::c_int) -> Result<libc::winsize> {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ as _, &mut size) } < 0 {
        return Err(std::io::Error::last_os_error()).context("failed to read terminal size");
    }
    // Some PTY allocators (notably `script` in a fresh container) report a
    // successful 0x0 size until their parent performs an explicit resize.
    // Passing that through makes provider TUIs unusable and a one-column
    // vt100 parser can underflow while processing a double-width glyph.
    // Treat a missing dimension as unknown, while preserving genuine small
    // non-zero terminals for the dashboard's compact-layout handling.
    Ok(normalize_terminal_size(size))
}

#[cfg(unix)]
fn normalize_terminal_size(mut size: libc::winsize) -> libc::winsize {
    if size.ws_row == 0 {
        size.ws_row = FALLBACK_TERMINAL_ROWS;
    }
    if size.ws_col < 2 {
        size.ws_col = FALLBACK_TERMINAL_COLUMNS;
    }
    size
}

#[cfg(unix)]
fn set_pty_size(fd: libc::c_int, size: libc::winsize) -> Result<()> {
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ as _, &size) } < 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to resize provider pseudo-terminal");
    }
    Ok(())
}

#[cfg(unix)]
fn same_terminal_size(left: libc::winsize, right: libc::winsize) -> bool {
    left.ws_row == right.ws_row
        && left.ws_col == right.ws_col
        && left.ws_xpixel == right.ws_xpixel
        && left.ws_ypixel == right.ws_ypixel
}

#[cfg(unix)]
fn set_close_on_exec(fd: libc::c_int) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to secure pseudo-terminal descriptor");
    }
    Ok(())
}

#[cfg(unix)]
fn set_nonblocking(fd: libc::c_int, enabled: bool) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to inspect pseudo-terminal flags");
    }
    let updated = if enabled {
        flags | libc::O_NONBLOCK
    } else {
        flags & !libc::O_NONBLOCK
    };
    if unsafe { libc::fcntl(fd, libc::F_SETFL, updated) } < 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to update pseudo-terminal flags");
    }
    Ok(())
}

#[cfg(unix)]
struct RawModeGuard;

#[cfg(unix)]
impl RawModeGuard {
    fn enter() -> Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

#[cfg(unix)]
impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArrowDirection {
    Left,
    Right,
}

impl ArrowDirection {
    fn symbol(self) -> &'static str {
        match self {
            Self::Left => "←",
            Self::Right => "→",
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum InputAction {
    Forward(Vec<u8>),
    Arrow(ArrowDirection, &'static [u8]),
    Detach,
    Redraw,
}

const SHIFT_LEFT: &[u8] = b"\x1b[1;2D";
const SHIFT_RIGHT: &[u8] = b"\x1b[1;2C";
const LEFT: &[u8] = b"\x1b[D";
const RIGHT: &[u8] = b"\x1b[C";
const APPLICATION_LEFT: &[u8] = b"\x1bOD";
const APPLICATION_RIGHT: &[u8] = b"\x1bOC";
const RECOGNIZED_ARROWS: [&[u8]; 6] = [
    SHIFT_LEFT,
    SHIFT_RIGHT,
    LEFT,
    RIGHT,
    APPLICATION_LEFT,
    APPLICATION_RIGHT,
];

#[derive(Default)]
struct DetachParser {
    pending: Vec<u8>,
    pending_since: Option<Instant>,
    holds_leader: bool,
    held_leader_since: Option<Instant>,
    redraws: bool,
}

impl DetachParser {
    fn for_session(session_key: &str) -> Self {
        Self {
            holds_leader: session_key.starts_with("opencode:"),
            redraws: redraws_on_ctrl_k(session_key),
            ..Self::default()
        }
    }

    fn push(&mut self, input: &[u8]) -> Vec<InputAction> {
        let mut bytes = Vec::new();
        if self.held_leader_since.take().is_some() {
            bytes.push(CTRL_X);
        }
        bytes.append(&mut self.pending);
        self.pending_since = None;
        bytes.extend_from_slice(input);
        let mut actions = Vec::new();
        let mut forward = Vec::new();
        let mut index = 0;
        while index < bytes.len() {
            let remaining = &bytes[index..];
            let recognized =
                if remaining.starts_with(SHIFT_LEFT) || remaining.starts_with(SHIFT_RIGHT) {
                    Some((InputAction::Detach, SHIFT_LEFT.len()))
                } else if remaining.starts_with(LEFT) {
                    Some((InputAction::Arrow(ArrowDirection::Left, LEFT), LEFT.len()))
                } else if remaining.starts_with(APPLICATION_LEFT) {
                    Some((
                        InputAction::Arrow(ArrowDirection::Left, APPLICATION_LEFT),
                        APPLICATION_LEFT.len(),
                    ))
                } else if remaining.starts_with(RIGHT) {
                    Some((
                        InputAction::Arrow(ArrowDirection::Right, RIGHT),
                        RIGHT.len(),
                    ))
                } else if remaining.starts_with(APPLICATION_RIGHT) {
                    Some((
                        InputAction::Arrow(ArrowDirection::Right, APPLICATION_RIGHT),
                        APPLICATION_RIGHT.len(),
                    ))
                } else if self.redraws && remaining[0] == CTRL_K {
                    Some((InputAction::Redraw, 1))
                } else {
                    None
                };
            if let Some((action, consumed)) = recognized {
                if !forward.is_empty() {
                    actions.push(InputAction::Forward(std::mem::take(&mut forward)));
                }
                actions.push(action);
                index += consumed;
                continue;
            }

            if remaining[0] == 0x1b
                && RECOGNIZED_ARROWS
                    .iter()
                    .any(|sequence| sequence.starts_with(remaining))
            {
                self.pending.extend_from_slice(&bytes[index..]);
                self.pending_since = Some(Instant::now());
                break;
            }
            if self.holds_leader && remaining == [CTRL_X] {
                self.held_leader_since = Some(Instant::now());
                break;
            }
            forward.push(bytes[index]);
            index += 1;
        }
        if !forward.is_empty() {
            actions.push(InputAction::Forward(forward));
        }
        actions
    }

    fn flush_expired(&mut self) -> Option<Vec<u8>> {
        if self.pending.is_empty()
            || self
                .pending_since
                .is_some_and(|since| since.elapsed() < ESCAPE_FLUSH_DELAY)
        {
            return None;
        }
        self.pending_since = None;
        Some(std::mem::take(&mut self.pending))
    }

    fn take_expired_leader(&mut self) -> bool {
        if self
            .held_leader_since
            .is_some_and(|since| since.elapsed() >= LEADER_HOLD)
        {
            self.held_leader_since = None;
            return true;
        }
        false
    }
}

#[derive(Debug)]
struct ArrowProbe {
    direction: ArrowDirection,
    cursor: (u16, u16),
    allow_cursor_change: bool,
    started: Instant,
}

#[derive(Debug)]
struct ArmedReturn {
    direction: ArrowDirection,
    cursor_guard: Option<(u16, u16)>,
    expires: Instant,
    last_bucket: Option<u64>,
}

#[derive(Default)]
struct ReturnGesture {
    probe: Option<ArrowProbe>,
    armed: Option<ArmedReturn>,
    hint_visible: bool,
    immediate_left: bool,
    replaces_agents_view: bool,
}

/// Claude shows one of these footers exactly when Left would leave for its
/// own agents view: a foreground session opens it, an attached background
/// session goes back to it.
const CLAUDE_AGENTS_HINTS: &[&str] = &["← for agents", "← to go back", "← again to go back"];

impl ReturnGesture {
    /// OpenCode has no view of its own behind Left at the input boundary, so
    /// one Left that reaches it returns to the dashboard without a second press.
    /// Claude's Left at an empty prompt opens its agents view, which agentview
    /// stands in for, so that Left never reaches Claude.
    fn for_session(session_key: &str) -> Self {
        Self {
            immediate_left: session_key.starts_with("opencode:"),
            replaces_agents_view: session_key.starts_with("claude:"),
            ..Self::default()
        }
    }

    fn takes_left_over(&self, direction: ArrowDirection, screen: &vt100::Parser) -> bool {
        if !self.replaces_agents_view || direction != ArrowDirection::Left {
            return false;
        }
        let (rows, cols) = screen.screen().size();
        screen
            .screen()
            .rows(0, cols)
            .skip(usize::from(rows.saturating_sub(4)))
            .any(|row| CLAUDE_AGENTS_HINTS.iter().any(|hint| row.contains(hint)))
    }

    fn begin_probe(
        &mut self,
        direction: ArrowDirection,
        cursor: (u16, u16),
        allow_cursor_change: bool,
    ) {
        self.probe = Some(ArrowProbe {
            direction,
            cursor,
            allow_cursor_change,
            started: Instant::now(),
        });
        self.armed = None;
    }

    fn should_detach(&mut self, direction: ArrowDirection, screen: &vt100::Parser) -> bool {
        let Some(armed) = self.armed.as_ref() else {
            return false;
        };
        armed.direction == direction
            && Instant::now() < armed.expires
            && armed
                .cursor_guard
                .map(|cursor| {
                    !screen.screen().hide_cursor() && screen.screen().cursor_position() == cursor
                })
                .unwrap_or(true)
    }

    /// Advance the return window; `true` means the settled arrow itself returns.
    fn update(&mut self, output: &mut impl Write, screen: &vt100::Parser) -> Result<bool> {
        let now = Instant::now();
        if screen.screen().hide_cursor()
            && self
                .probe
                .as_ref()
                .map_or(true, |probe| !probe.allow_cursor_change)
            && self
                .armed
                .as_ref()
                .map_or(true, |armed| armed.cursor_guard.is_some())
        {
            self.clear(output, screen)?;
            return Ok(false);
        }
        if let Some(probe) = self.probe.as_ref() {
            if !probe.allow_cursor_change && screen.screen().cursor_position() != probe.cursor {
                self.clear(output, screen)?;
                return Ok(false);
            }
            if now.duration_since(probe.started) >= ARROW_SETTLE_DELAY {
                if self.immediate_left && probe.direction == ArrowDirection::Left {
                    self.clear(output, screen)?;
                    return Ok(true);
                }
                self.armed = Some(ArmedReturn {
                    direction: probe.direction,
                    cursor_guard: (!probe.allow_cursor_change).then_some(probe.cursor),
                    expires: now + ARROW_RETURN_WINDOW,
                    last_bucket: None,
                });
                self.probe = None;
            }
        }

        let Some(armed) = self.armed.as_mut() else {
            return Ok(false);
        };
        if now >= armed.expires
            || armed
                .cursor_guard
                .is_some_and(|cursor| screen.screen().cursor_position() != cursor)
        {
            self.clear(output, screen)?;
            return Ok(false);
        }
        let remaining = armed.expires.saturating_duration_since(now);
        let bucket = remaining.as_millis() as u64 / RETURN_HINT_REFRESH.as_millis() as u64;
        if armed.last_bucket != Some(bucket) {
            write_return_hint(output, screen, armed.direction, remaining)?;
            armed.last_bucket = Some(bucket);
            self.hint_visible = true;
        }
        Ok(false)
    }

    fn clear(&mut self, output: &mut impl Write, screen: &vt100::Parser) -> Result<()> {
        self.probe = None;
        self.armed = None;
        if self.hint_visible {
            restore_bottom_row(output, screen)?;
            self.hint_visible = false;
        }
        Ok(())
    }
}

fn write_return_hint(
    output: &mut impl Write,
    screen: &vt100::Parser,
    direction: ArrowDirection,
    remaining: Duration,
) -> Result<()> {
    let (rows, cols) = screen.screen().size();
    let tenths = remaining.as_millis().div_ceil(100);
    let message = format!(
        " Press {} again to return to agentview · {:.1}s · Shift+←/→ anytime",
        direction.symbol(),
        tenths as f64 / 10.0
    );
    let message = truncate_to_columns(&message, usize::from(cols));
    write!(
        output,
        "\x1b7\x1b[{};1H\x1b[2K\x1b[30;46m{}\x1b[0m\x1b8",
        rows.max(1),
        message
    )?;
    output.flush()?;
    Ok(())
}

fn restore_bottom_row(output: &mut impl Write, screen: &vt100::Parser) -> Result<()> {
    let (rows, cols) = screen.screen().size();
    let last_row = screen
        .screen()
        .rows_formatted(0, cols)
        .nth(usize::from(rows.saturating_sub(1)))
        .unwrap_or_default();
    write!(output, "\x1b7\x1b[{};1H\x1b[2K", rows.max(1))?;
    output.write_all(&last_row)?;
    output.write_all(b"\x1b8")?;
    output.flush()?;
    Ok(())
}

fn truncate_to_columns(value: &str, width: usize) -> String {
    value.chars().take(width).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yolo_warning_names_the_provider_without_overclaiming_sandbox_behavior() {
        assert_eq!(
            yolo_warning("OpenHands"),
            "⚠ YOLO MODE · OpenHands permission safeguards are relaxed"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn native_launch_retries_a_transient_text_busy_executable() {
        use std::fs::OpenOptions;
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("provider");
        std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let writer = OpenOptions::new().write(true).open(&executable).unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            drop(writer);
        });

        let mut command = Command::new(&executable);
        let status = status_retrying_text_busy(&mut command).unwrap();
        release.join().unwrap();
        assert!(status.success());
    }

    #[cfg(unix)]
    #[test]
    fn a_paste_larger_than_the_pty_input_queue_waits_for_the_provider_to_read() {
        let (mut master_fd, mut slave_fd) = (-1, -1);
        let opened = unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(opened, 0);
        let mut master = unsafe { std::fs::File::from_raw_fd(master_fd) };
        let mut slave = unsafe { std::fs::File::from_raw_fd(slave_fd) };
        let mut attributes: libc::termios = unsafe { std::mem::zeroed() };
        unsafe {
            assert_eq!(libc::tcgetattr(slave_fd, &mut attributes), 0);
            libc::cfmakeraw(&mut attributes);
            assert_eq!(libc::tcsetattr(slave_fd, libc::TCSANOW, &attributes), 0);
        }
        set_nonblocking(master_fd, true).unwrap();
        set_nonblocking(slave_fd, true).unwrap();

        let paste: Vec<u8> = (0..32 * 1024)
            .map(|index| b'a' + (index % 26) as u8)
            .collect();
        let mut pending = PendingInput::default();
        pending.write_all(b"\x1b[200~").unwrap();
        pending.write_all(&paste).unwrap();
        pending.write_all(b"\x1b[201~").unwrap();

        pending.write_to(&mut master).unwrap();
        assert!(
            !pending.is_empty(),
            "the stalled provider left room for the whole paste"
        );

        let mut received = Vec::new();
        let mut chunk = [0_u8; 4096];
        let deadline = Instant::now() + Duration::from_secs(5);
        while !pending.is_empty() || received.len() < paste.len() + 12 {
            assert!(Instant::now() < deadline, "paste never drained");
            pending.write_to(&mut master).unwrap();
            match slave.read(&mut chunk) {
                Ok(read) => received.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("{error}"),
            }
        }
        let mut expected = b"\x1b[200~".to_vec();
        expected.extend_from_slice(&paste);
        expected.extend_from_slice(b"\x1b[201~");
        assert_eq!(received, expected);
    }

    #[cfg(unix)]
    #[test]
    fn zero_sized_pty_uses_safe_native_terminal_dimensions() {
        let normalized = normalize_terminal_size(libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        });
        assert_eq!(normalized.ws_row, FALLBACK_TERMINAL_ROWS);
        assert_eq!(normalized.ws_col, FALLBACK_TERMINAL_COLUMNS);

        let tiny = normalize_terminal_size(libc::winsize {
            ws_row: 1,
            ws_col: 1,
            ws_xpixel: 0,
            ws_ypixel: 0,
        });
        assert_eq!(tiny.ws_row, 1);
        assert_eq!(tiny.ws_col, FALLBACK_TERMINAL_COLUMNS);
    }

    #[test]
    fn detach_parser_handles_fragmented_shift_left_sequences() {
        let mut parser = DetachParser::default();
        let first = parser.push(b"hello\x1b");
        assert_eq!(first, vec![InputAction::Forward(b"hello".to_vec())]);
        let second = parser.push(b"[1;2Dignored");
        assert_eq!(
            second,
            vec![
                InputAction::Detach,
                InputAction::Forward(b"ignored".to_vec())
            ]
        );
    }

    #[test]
    fn detach_parser_classifies_plain_arrows_and_preserves_other_input_exactly() {
        let mut parser = DetachParser::default();
        let parsed = parser.push(b"abc\x1b[A\x1b[D\x1bOD\x1b[C\x1bOC\x1b[1;2C");
        assert_eq!(
            parsed,
            vec![
                InputAction::Forward(b"abc\x1b[A".to_vec()),
                InputAction::Arrow(ArrowDirection::Left, LEFT),
                InputAction::Arrow(ArrowDirection::Left, APPLICATION_LEFT),
                InputAction::Arrow(ArrowDirection::Right, RIGHT),
                InputAction::Arrow(ArrowDirection::Right, APPLICATION_RIGHT),
                InputAction::Detach,
            ]
        );
    }

    #[test]
    fn opencode_holds_a_lone_ctrl_x_until_it_expires_as_the_removal_key() {
        let mut parser = DetachParser::for_session("opencode:ses_1");
        assert_eq!(
            parser.push(b"hi\x18"),
            vec![InputAction::Forward(b"hi".to_vec())]
        );
        assert!(!parser.take_expired_leader());
        parser.held_leader_since = Some(Instant::now() - LEADER_HOLD);
        assert!(parser.take_expired_leader());
        assert!(!parser.take_expired_leader());
        assert_eq!(parser.push(b"a"), vec![InputAction::Forward(b"a".to_vec())]);
    }

    #[test]
    fn opencode_leader_sequences_reach_the_provider_intact() {
        let mut parser = DetachParser::for_session("opencode:ses_1");
        assert_eq!(parser.push(b"\x18"), vec![]);
        assert_eq!(
            parser.push(b"n"),
            vec![InputAction::Forward(b"\x18n".to_vec())]
        );
        assert_eq!(
            parser.push(b"\x18m"),
            vec![InputAction::Forward(b"\x18m".to_vec())]
        );
        assert!(!parser.take_expired_leader());
    }

    #[test]
    fn opencode_takes_ctrl_k_as_a_redraw_and_other_providers_receive_it() {
        let mut parser = DetachParser::for_session("opencode:shared");
        assert_eq!(
            parser.push(b"a\x0bb"),
            vec![
                InputAction::Forward(b"a".to_vec()),
                InputAction::Redraw,
                InputAction::Forward(b"b".to_vec()),
            ]
        );
        let mut parser = DetachParser::for_session("claude:worker");
        assert_eq!(
            parser.push(b"\x0b"),
            vec![InputAction::Forward(b"\x0b".to_vec())]
        );
    }

    #[test]
    fn other_providers_receive_ctrl_x_immediately() {
        let mut parser = DetachParser::for_session("claude:worker");
        assert_eq!(
            parser.push(b"\x18"),
            vec![InputAction::Forward(b"\x18".to_vec())]
        );
        assert!(!parser.take_expired_leader());
    }

    #[test]
    fn return_hint_is_bounded_to_the_terminal_width() {
        assert_eq!(truncate_to_columns("Press ← again", 7), "Press ←");
    }

    #[cfg(unix)]
    #[test]
    fn initial_input_waits_for_the_exact_authenticated_screen_marker_and_sends_once() {
        let mut screen = vt100::Parser::new(12, 80, 0);
        let mut pending = Some(ScreenTriggeredInput {
            bytes: b"fix the parser\r".to_vec(),
            ready_marker: "Send /help for help information.".into(),
            next: Vec::new(),
        });
        let mut forwarded = Vec::new();

        screen.process(b"Run /login or /provider to get started.");
        assert!(!forward_ready_initial_input(&mut pending, &screen, &mut forwarded).unwrap());
        assert!(forwarded.is_empty());

        screen.process(b"\r\nSend /help for help information.");
        assert!(forward_ready_initial_input(&mut pending, &screen, &mut forwarded).unwrap());
        assert_eq!(forwarded, b"fix the parser\r");
        assert!(pending.is_none());

        assert!(!forward_ready_initial_input(&mut pending, &screen, &mut forwarded).unwrap());
        assert_eq!(forwarded, b"fix the parser\r");
    }

    #[test]
    #[cfg(unix)]
    fn screen_steps_require_all_markers_and_wait_for_each_acknowledgement() {
        let mut screen = vt100::Parser::new(12, 80, 0);
        let mut pending = Some(ScreenTriggeredInput {
            ready_marker: "welcome\neditor ready".into(),
            bytes: b"/new\r".to_vec(),
            next: vec![("new conversation ready".into(), b"hello\r".to_vec())],
        });
        let mut forwarded = Vec::new();
        screen.process(b"welcome");
        assert!(!forward_ready_initial_input(&mut pending, &screen, &mut forwarded).unwrap());
        screen.process(b"\r\neditor ready");
        assert!(forward_ready_initial_input(&mut pending, &screen, &mut forwarded).unwrap());
        assert_eq!(forwarded, b"/new\r");
        assert!(!forward_ready_initial_input(&mut pending, &screen, &mut forwarded).unwrap());
        screen.process(b"\r\nnew conversation ready");
        assert!(forward_ready_initial_input(&mut pending, &screen, &mut forwarded).unwrap());
        assert_eq!(forwarded, b"/new\rhello\r");
        assert!(pending.is_none());
    }

    #[test]
    fn screen_gated_input_rejects_empty_or_oversized_payloads_before_spawning() {
        let command = || Command::new("provider-that-must-not-run");
        assert!(
            run_with_initial_input_after_screen(command(), "test:empty", b"", "ready")
                .unwrap_err()
                .to_string()
                .contains("initial input")
        );
        assert!(run_with_initial_input_after_screen(
            command(),
            "test:oversized",
            &vec![b'x'; MAX_INITIAL_INPUT_BYTES + 1],
            "ready",
        )
        .unwrap_err()
        .to_string()
        .contains("initial input"));
    }

    #[test]
    #[cfg(unix)]
    fn provider_output_keeps_arriving_while_the_dashboard_is_detached() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "i=0; while [ \"$i\" -lt 20 ]; do echo tick-$i; i=$((i+1)); sleep 0.05; done",
        ]);
        let (mut child, master) = spawn_pty(&mut command).unwrap();
        let drain = start_output_drain(master, vt100::Parser::new(24, 80, 0), None).unwrap();
        thread::sleep(Duration::from_millis(400));
        let state = Command::new("ps")
            .args(["-o", "state=", "-p", &child.id().to_string()])
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&state.stdout);
        assert!(
            !state.trim().starts_with('T'),
            "provider process was stopped: {state:?}"
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let published = drain.contents.lock().unwrap().clone();
            if published.contains("tick-19") {
                break;
            }
            assert!(Instant::now() < deadline, "{published}");
            thread::sleep(Duration::from_millis(20));
        }
        drain.halt();
        let (_master, screen) = drain.done.join().unwrap();
        let contents = screen.screen().contents();
        assert!(contents.contains("tick-0"), "{contents}");
        assert!(contents.contains("tick-5"), "{contents}");
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    #[cfg(unix)]
    fn detached_output_answers_cursor_and_attribute_queries_across_reads() {
        let mut screen = vt100::Parser::new(24, 80, 0);
        let mut queries = TerminalQueryScanner::default();
        let mut replies = Vec::new();
        // The cursor query is split across two reads, and output after it in
        // the same read must not move the reported position.
        process_detached_output(
            b"\x1b[3;5Habc\x1b[",
            &mut screen,
            &mut queries,
            &mut replies,
        );
        assert!(replies.is_empty());
        process_detached_output(b"6nlater", &mut screen, &mut queries, &mut replies);
        assert_eq!(replies, b"\x1b[3;8R");
        replies.clear();
        process_detached_output(b"\x1b[c\x1b", &mut screen, &mut queries, &mut replies);
        process_detached_output(b"[0c", &mut screen, &mut queries, &mut replies);
        assert_eq!(replies, b"\x1b[?1;2c\x1b[?1;2c");
        replies.clear();
        // Private and secondary variants, and ordinary sequences, are not ours.
        process_detached_output(
            b"\x1b[?6n\x1b[>c\x1b[16n\x1b[2J",
            &mut screen,
            &mut queries,
            &mut replies,
        );
        assert!(replies.is_empty());
        assert!(screen.screen().contents().is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn color_queries_are_answered_from_the_dashboard_scheme_across_reads() {
        crate::theme::set_terminal_scheme(crate::theme::ColorScheme::Light);
        let mut queries = TerminalQueryScanner::default();
        let mut replies = Vec::new();
        answer_color_queries(b"\x1b]10;?\x07\x1b]11", &mut queries, &mut replies);
        answer_color_queries(b";?\x1b", &mut queries, &mut replies);
        answer_color_queries(b"\\", &mut queries, &mut replies);
        assert_eq!(
            String::from_utf8(replies).unwrap(),
            "\x1b]10;rgb:3b3b/3b3b/3b3b\x1b\\\x1b]11;rgb:ffff/ffff/ffff\x1b\\"
        );
        // Setting colors, titles, palette queries, and cursor queries are not
        // color queries, and an unterminated OSC never answers.
        let mut replies = Vec::new();
        answer_color_queries(
            b"\x1b]11;rgb:0/0/0\x07\x1b]0;title 11;?\x07\x1b]4;1;?\x07\x1b[6n\x1b]10;?\x1b[c",
            &mut queries,
            &mut replies,
        );
        assert!(replies.is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn only_opencode_sessions_get_color_answers_in_front() {
        assert!(answers_color_queries("opencode:host:ses_1"));
        assert!(answers_color_queries("opencode:host:launch-abc"));
        assert!(!answers_color_queries("codex:portable:new"));
        assert!(!answers_color_queries("setup:OpenCode"));
    }

    #[test]
    #[cfg(unix)]
    fn output_absorbed_during_the_resume_redraw_still_updates_the_screen() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf hidden-frame; sleep 2"]);
        let (mut child, mut master) = spawn_pty(&mut command).unwrap();
        let mut screen = vt100::Parser::new(24, 80, 0);
        let mut queries = TerminalQueryScanner::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !screen.screen().contents().contains("hidden-frame") {
            assert!(Instant::now() < deadline, "{}", screen.screen().contents());
            absorb_available(&mut master, &mut screen, &mut queries).unwrap();
            thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn only_opencode_sessions_redraw_on_ctrl_k() {
        assert!(redraws_on_ctrl_k("opencode:host:ses_1"));
        assert!(redraws_on_ctrl_k("opencode:host:launch-abc"));
        assert!(!redraws_on_ctrl_k("claude:host:abc"));
        assert!(!redraws_on_ctrl_k("codex:portable:new"));
        assert!(!redraws_on_ctrl_k("setup:OpenCode"));
    }

    #[cfg(unix)]
    fn detach_for_test(session_key: &str, script: &str) -> u32 {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        let (child, master) = spawn_pty(&mut command).unwrap();
        let pid = child.id();
        let drain = start_output_drain(master, vt100::Parser::new(24, 80, 0), None).unwrap();
        detached_registry().lock().unwrap().insert(
            session_key.to_owned(),
            DetachedSession {
                child: Some(child),
                warning: None,
                drain: Some(drain),
            },
        );
        pid
    }

    #[test]
    #[cfg(unix)]
    fn waits_for_a_hidden_frontend_to_set_the_title_and_settle() {
        let key = "provider:host:background-title";
        detach_for_test(
            key,
            "printf '\\033]0;Old\\007'; sleep 0.3; printf '\\033]0;New\\007frame'; sleep 5",
        );
        let started = Instant::now();
        let is_new = |title: &str| title == "New";
        assert!(wait_for_background_title(
            key,
            &is_new,
            Duration::from_millis(40),
            Duration::from_secs(5),
            &|| true,
        ));
        assert!(started.elapsed() >= Duration::from_millis(300));
        let is_other = |title: &str| title == "Other";
        assert!(!wait_for_background_title(
            key,
            &is_other,
            Duration::from_millis(40),
            Duration::from_millis(100),
            &|| true,
        ));
        let cancelled = Instant::now();
        assert!(!wait_for_background_title(
            key,
            &is_other,
            Duration::from_millis(40),
            Duration::from_secs(5),
            &|| false,
        ));
        assert!(cancelled.elapsed() < Duration::from_secs(1));
        assert!(!wait_for_background_title(
            "provider:host:not-held",
            &is_new,
            Duration::ZERO,
            Duration::from_secs(5),
            &|| true,
        ));
        terminate(key).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn background_screen_names_the_live_child_and_forgets_it_once_it_exits() {
        let marker = std::env::temp_dir().join(format!(
            "agentview-background-screen-{}-{}",
            std::process::id(),
            new_session_id().unwrap()
        ));
        let key = "provider:host:background-screen";
        let script = format!(
            "printf 'working  ctrl+c to stop'; while [ ! -e '{}' ]; do sleep 0.02; done",
            marker.display()
        );
        let pid = detach_for_test(key, &script);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let screen = background_screen_contents(key);
            if let Some((child, contents)) = &screen {
                assert_eq!(*child, pid);
                if contents.contains("ctrl+c to stop") {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "{screen:?}");
            thread::sleep(Duration::from_millis(20));
        }
        std::fs::write(&marker, b"").unwrap();
        // The last screen of an exited provider is never reported as live.
        let deadline = Instant::now() + Duration::from_secs(10);
        while background_screen_contents(key).is_some() {
            assert!(Instant::now() < deadline, "exited provider still reported");
            thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::remove_file(&marker);
        assert!(!detached_session_keys().iter().any(|item| item == key));
    }

    #[test]
    #[cfg(unix)]
    fn detached_provider_keeps_writing_past_the_terminal_buffer_and_resumes() {
        let marker = std::env::temp_dir().join(format!(
            "agentview-detached-output-{}-{}",
            std::process::id(),
            new_session_id().unwrap()
        ));
        let key = "provider:host:detached-large-output";
        // ~200 KB is far past any kernel pseudo-terminal buffer: without a
        // reader the provider would block before touching the marker.
        let script = format!(
            "i=0; while [ \"$i\" -lt 2000 ]; do printf '%0100d\\n' \"$i\"; i=$((i+1)); done; \
             printf 'LARGE_OUTPUT_DONE'; : > '{}'; exec sleep 30",
            marker.display()
        );
        detach_for_test(key, &script);
        let deadline = Instant::now() + Duration::from_secs(20);
        while !marker.exists() {
            assert!(
                Instant::now() < deadline,
                "detached provider blocked on output"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let _ = std::fs::remove_file(&marker);
        assert!(is_backgrounded(key));
        let detached = take_detached(key)
            .unwrap()
            .expect("provider is still running");
        let (mut child, _master, screen, _warning) = detached.into_frontend().unwrap();
        let contents = screen.screen().contents();
        assert!(contents.contains("LARGE_OUTPUT_DONE"), "{contents}");
        signal_group(child.id(), libc::SIGKILL);
        let _ = child.wait();
    }

    #[test]
    #[cfg(unix)]
    fn provider_that_exits_while_detached_is_not_resumed() {
        let key = "provider:host:detached-exit";
        detach_for_test(key, "printf bye; exit 0");
        let deadline = Instant::now() + Duration::from_secs(10);
        while is_backgrounded(key) {
            assert!(Instant::now() < deadline, "exited provider still listed");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(take_detached(key).unwrap().is_none());
        assert!(!detached_session_keys().iter().any(|item| item == key));
    }

    #[test]
    fn empty_left_margin_prompt_keeps_return_window_across_provider_redraw() {
        let mut screen = vt100::Parser::new(6, 40, 0);
        screen.process(b"\x1b[1;3H> \x1b[?25h");
        let cursor = screen.screen().cursor_position();
        assert!(cursor.1 <= EMPTY_PROMPT_MAX_COLUMN);

        let mut gesture = ReturnGesture::default();
        gesture.begin_probe(ArrowDirection::Left, cursor, true);
        screen.process(b"\x1b[2J\x1b[6;20Hprovider subview\x1b[?25l");
        std::thread::sleep(ARROW_SETTLE_DELAY + Duration::from_millis(10));
        let mut output = Vec::new();
        gesture.update(&mut output, &screen).unwrap();

        assert!(gesture.should_detach(ArrowDirection::Left, &screen));
        assert!(String::from_utf8_lossy(&output).contains("Press ← again"));
    }

    fn settled_left(session_key: &str, screen: &mut vt100::Parser, moved: &[u8]) -> bool {
        let mut gesture = ReturnGesture::for_session(session_key);
        gesture.begin_probe(
            ArrowDirection::Left,
            screen.screen().cursor_position(),
            false,
        );
        screen.process(moved);
        std::thread::sleep(ARROW_SETTLE_DELAY + Duration::from_millis(10));
        gesture.update(&mut Vec::new(), screen).unwrap()
    }

    #[test]
    fn opencode_returns_on_one_left_at_the_input_boundary() {
        let mut screen = vt100::Parser::new(40, 120, 0);
        screen.process(b"\x1b[21;27H\x1b[?25h");
        assert!(settled_left("opencode:host:abc", &mut screen, b""));
    }

    #[test]
    fn opencode_left_that_moves_the_cursor_stays_in_the_session() {
        let mut screen = vt100::Parser::new(40, 120, 0);
        screen.process(b"\x1b[21;29H\x1b[?25h");
        assert!(!settled_left(
            "opencode:host:abc",
            &mut screen,
            b"\x1b[21;28H"
        ));
    }

    #[test]
    fn other_providers_still_need_a_second_left() {
        let mut screen = vt100::Parser::new(40, 120, 0);
        screen.process(b"\x1b[21;27H\x1b[?25h");
        assert!(!settled_left("claude:host:abc", &mut screen, b""));
    }

    fn claude_prompt(footer: &str) -> vt100::Parser {
        let mut screen = vt100::Parser::new(10, 120, 0);
        screen.process(
            format!("\x1b[8;1H❯ \x1b[2mTry \"fix lint\"\x1b[0m\x1b[10;1H  {footer}\x1b[8;3H")
                .as_bytes(),
        );
        screen
    }

    #[test]
    fn claude_left_at_an_empty_prompt_returns_instead_of_opening_its_agents_view() {
        let screen = claude_prompt("⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents");
        let gesture = ReturnGesture::for_session("claude:host:abc");
        assert!(gesture.takes_left_over(ArrowDirection::Left, &screen));
        assert!(!gesture.takes_left_over(ArrowDirection::Right, &screen));
        assert!(!ReturnGesture::for_session("codex:host:abc")
            .takes_left_over(ArrowDirection::Left, &screen));
    }

    #[test]
    fn claude_left_in_an_attached_background_session_returns_to_the_dashboard() {
        let gesture = ReturnGesture::for_session("claude:host:abc");
        for footer in [
            "? for shortcuts · ← to go back",
            "Press ← again to go back to agents",
        ] {
            assert!(gesture.takes_left_over(ArrowDirection::Left, &claude_prompt(footer)));
        }
    }

    #[test]
    fn claude_left_with_a_draft_still_moves_the_cursor() {
        let screen = claude_prompt("⏵⏵ bypass permissions on (shift+tab to cycle)");
        assert!(!ReturnGesture::for_session("claude:host:abc")
            .takes_left_over(ArrowDirection::Left, &screen));
    }
}
