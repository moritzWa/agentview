//! Timing and diagnostics log for the open, preview and refresh paths. One
//! line per event: `<unix ms> <thread> <event> <details>`.
//!
//! `AGENTVIEW_PERF_LOG` names the file; `off` (or `0`) turns it off. Without
//! it, the dashboard writes to `perf.log` in agentview's state directory (see
//! [`use_default_file`]), so a hang that a restart clears still leaves a trace.
//! The file is rotated to `perf.log.1` once it passes [`ROTATE_BYTES`].

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const ROTATE_BYTES: u64 = 4 * 1024 * 1024;

struct Sink {
    path: PathBuf,
    file: File,
    written: u64,
}

static DEFAULT_PATH: OnceLock<PathBuf> = OnceLock::new();

fn configured_path() -> Option<PathBuf> {
    match std::env::var_os("AGENTVIEW_PERF_LOG") {
        Some(value) if value.is_empty() || value == "off" || value == "0" => None,
        Some(value) => Some(PathBuf::from(value)),
        None => DEFAULT_PATH.get().cloned(),
    }
}

fn open(path: &Path) -> Option<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).ok()
}

fn sink() -> Option<&'static Mutex<Sink>> {
    static SINK: OnceLock<Option<Mutex<Sink>>> = OnceLock::new();
    SINK.get_or_init(|| {
        let path = configured_path()?;
        let file = open(&path)?;
        let written = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        Some(Mutex::new(Sink {
            path,
            file,
            written,
        }))
    })
    .as_ref()
}

/// Log to `perf.log` in agentview's state directory unless
/// `AGENTVIEW_PERF_LOG` says otherwise. Call before the first event.
pub fn use_default_file() {
    let dir = match crate::fs_util::xdg_home("XDG_STATE_HOME") {
        Some(state) => PathBuf::from(state).join("agentview"),
        None => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(".local/state/agentview"),
            None => return,
        },
    };
    // The state directory's other stores refuse one that is not private, and
    // this can be the first thing to create it.
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    if builder.create(&dir).is_ok() {
        let _ = DEFAULT_PATH.set(dir.join("perf.log"));
    }
}

pub fn enabled() -> bool {
    sink().is_some()
}

pub fn event(name: &str, details: std::fmt::Arguments<'_>) {
    let Some(sink) = sink() else {
        return;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let thread = std::thread::current();
    let line = format!("{now} {} {name} {details}\n", thread.name().unwrap_or("-"));
    if let Ok(mut sink) = sink.lock() {
        if sink.written >= ROTATE_BYTES {
            let mut rotated = sink.path.clone().into_os_string();
            rotated.push(".1");
            let _ = std::fs::rename(&sink.path, &rotated);
            if let Some(file) = open(&sink.path) {
                sink.file = file;
            }
            sink.written = 0;
        }
        if sink.file.write_all(line.as_bytes()).is_ok() {
            sink.written += line.len() as u64;
        }
    }
}

/// Logs `span <name> took=…` when dropped.
pub struct Span {
    name: &'static str,
    started: std::time::Instant,
}

pub fn span(name: &'static str) -> Span {
    Span {
        name,
        started: std::time::Instant::now(),
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        event(
            "span",
            format_args!("{} took={}", self.name, ms(self.started.elapsed())),
        );
    }
}

pub fn ms(duration: Duration) -> String {
    format!("{:.1}ms", duration.as_secs_f64() * 1000.0)
}

#[macro_export]
macro_rules! perf {
    ($name:expr) => {
        $crate::perf_log::event($name, format_args!(""))
    };
    ($name:expr, $($arg:tt)+) => {
        $crate::perf_log::event($name, format_args!($($arg)+))
    };
}
