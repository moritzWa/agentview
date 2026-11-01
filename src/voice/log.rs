//! An optional JSON-lines log of a call, for finding out what went wrong:
//! set `AGENTVIEW_VOICE_LOG` to a file. Nothing is written otherwise.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use serde_json::{json, Value};

fn sink() -> &'static Mutex<Option<(std::fs::File, Instant)>> {
    static SINK: OnceLock<Mutex<Option<(std::fs::File, Instant)>>> = OnceLock::new();
    SINK.get_or_init(|| {
        let file = std::env::var_os("AGENTVIEW_VOICE_LOG")
            .and_then(|path| OpenOptions::new().create(true).append(true).open(path).ok());
        Mutex::new(file.map(|file| (file, Instant::now())))
    })
}

/// Writes one line: seconds since the first line, a kind, and details.
pub fn log(kind: &str, details: Value) {
    if let Ok(mut guard) = sink().lock() {
        if let Some((file, started)) = guard.as_mut() {
            let line = json!({"t": (started.elapsed().as_millis() as f64) / 1000.0, "kind": kind, "d": details});
            let _ = writeln!(file, "{line}");
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
