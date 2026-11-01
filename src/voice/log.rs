//! Two logs of a call. `AGENTVIEW_VOICE_LOG` names a file for a full one, with
//! what was said and what the tools returned. Always, the last call also leaves
//! `voice/last-call.log` in the state directory with the same events but none
//! of the words, so "it did not connect" can be looked into afterwards.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use serde_json::{json, Value};

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

/// A new call: the short log starts over.
pub fn start_call() {
    let Ok(dir) = super::config::state_dir() else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
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

/// Writes one line: seconds since the call began, a kind, and details.
pub fn log(kind: &str, details: Value) {
    write(full(), kind, &details);
    write(last_call(), kind, &without_words(&details));
}

fn write(sink: &Mutex<Option<Sink>>, kind: &str, details: &Value) {
    if let Ok(mut guard) = sink.lock() {
        if let Some(sink) = guard.as_mut() {
            let line = json!({"t": sink.started.elapsed().as_millis() as f64 / 1000.0, "kind": kind, "d": details});
            let _ = writeln!(sink.file, "{line}");
        }
    }
}

/// The same details with every word replaced by its length.
fn without_words(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    let hidden = matches!(
                        key.as_str(),
                        "text" | "result" | "content" | "args" | "message" | "delta"
                    );
                    let value = if hidden {
                        json!(format!("<{} chars>", value.to_string().chars().count()))
                    } else {
                        without_words(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        other => other.clone(),
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
    fn the_short_log_keeps_events_and_drops_words() {
        let cleaned = without_words(&json!({
            "type": "tool", "name": "read_session", "ms": 5,
            "args": {"id": "s1"}, "result": "a private message",
        }));
        assert_eq!(cleaned["name"], "read_session");
        assert_eq!(cleaned["ms"], 5);
        assert!(cleaned["args"].as_str().unwrap().starts_with('<'));
        assert!(!cleaned.to_string().contains("private"));
    }
}
