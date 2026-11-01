//! The log of a call. Every call writes `voice/last-call.log` in the state
//! directory (owner-only): when the page and OpenAI connected, what was said
//! on both sides, each tool call with its arguments and result. The calls
//! before it are kept in `voice/calls/`, the last ten, so a bad call can be
//! looked into afterwards. `AGENTVIEW_VOICE_LOG` names one more file that
//! collects everything.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use serde_json::{json, Value};

/// Earlier calls kept next to the last one.
const CALLS_KEPT: usize = 10;

struct Sink {
    file: File,
    started: Instant,
}

fn full() -> &'static Mutex<Option<Sink>> {
    static SINK: OnceLock<Mutex<Option<Sink>>> = OnceLock::new();
    SINK.get_or_init(|| {
        let file = std::env::var_os("AGENTVIEW_VOICE_LOG")
            .and_then(|path| OpenOptions::new().create(true).append(true).open(path).ok());
        Mutex::new(file.map(|file| Sink {
            file,
            started: Instant::now(),
        }))
    })
}

fn last_call() -> &'static Mutex<Option<Sink>> {
    static SINK: OnceLock<Mutex<Option<Sink>>> = OnceLock::new();
    SINK.get_or_init(|| Mutex::new(None))
}

/// A new call: the previous one moves to `calls/` and the log starts over.
pub fn start_call() {
    let Ok(dir) = super::config::state_dir() else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    rotate(&dir);
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    if let Ok(file) = options.open(dir.join("last-call.log")) {
        if let Ok(mut guard) = last_call().lock() {
            *guard = Some(Sink {
                file,
                started: Instant::now(),
            });
        }
    }
}

/// Moves `last-call.log` into `calls/`, named by when it was written, and
/// drops all but the newest few.
fn rotate(dir: &Path) {
    let last = dir.join("last-call.log");
    let Ok(metadata) = std::fs::metadata(&last) else {
        return;
    };
    if metadata.len() == 0 {
        return;
    }
    let calls = dir.join("calls");
    if std::fs::create_dir_all(&calls).is_err() {
        return;
    }
    let written = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |elapsed| elapsed.as_secs());
    let _ = std::fs::rename(&last, calls.join(format!("call-{written}.log")));
    let mut kept: Vec<_> = std::fs::read_dir(&calls)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    // Names carry the time, so the newest sort last.
    kept.sort();
    let excess = kept.len().saturating_sub(CALLS_KEPT);
    for old in kept.into_iter().take(excess) {
        let _ = std::fs::remove_file(old);
    }
}

/// Writes one line: seconds since the call began, a kind, and details.
pub fn log(kind: &str, details: Value) {
    write(full(), kind, &details);
    write(last_call(), kind, &details);
}

fn write(sink: &Mutex<Option<Sink>>, kind: &str, details: &Value) {
    if let Ok(mut guard) = sink.lock() {
        if let Some(sink) = guard.as_mut() {
            let line = json!({"t": sink.started.elapsed().as_millis() as f64 / 1000.0, "kind": kind, "d": details});
            let _ = writeln!(sink.file, "{line}");
        }
    }
}

/// Text cut to `max` characters for the log.
pub fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        format!("{}…", text.chars().take(max).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_call_moves_aside_and_only_ten_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let calls = dir.path().join("calls");
        std::fs::create_dir_all(&calls).unwrap();
        for stamp in 100..115 {
            std::fs::write(calls.join(format!("call-{stamp}.log")), "old").unwrap();
        }
        std::fs::write(dir.path().join("last-call.log"), "{\"kind\":\"start\"}\n").unwrap();
        rotate(dir.path());
        assert!(!dir.path().join("last-call.log").exists());
        let mut names: Vec<String> = std::fs::read_dir(&calls)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names.len(), CALLS_KEPT);
        assert!(names.last().unwrap().len() > "call-114.log".len() - 1);
        // An empty log is not worth keeping.
        std::fs::write(dir.path().join("last-call.log"), "").unwrap();
        rotate(dir.path());
        assert_eq!(std::fs::read_dir(&calls).unwrap().count(), CALLS_KEPT);
    }
}
