//! Timing log for the open and preview paths, written only when
//! `AGENTVIEW_PERF_LOG` names a file. One line per event:
//! `<unix ms> <thread> <event> <details>`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn sink() -> Option<&'static Mutex<File>> {
    static SINK: OnceLock<Option<Mutex<File>>> = OnceLock::new();
    SINK.get_or_init(|| {
        let path = std::env::var_os("AGENTVIEW_PERF_LOG")?;
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
            .map(Mutex::new)
    })
    .as_ref()
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
    if let Ok(mut file) = sink.lock() {
        let _ = file.write_all(line.as_bytes());
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
