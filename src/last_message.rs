//! The end of each session's latest agent message, as discovery read it.
//!
//! Rows keep only the start of that message as their summary, while whether
//! the agent waits on the user is usually decided by its last sentence. Some
//! providers also grant no transcript access for sessions agentview did not
//! start. Discovery already holds the whole message, so it leaves the tail
//! here for the needs-input classifier.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Matches the transcript tail the classifier sends.
const TAIL_CHARS: usize = 2_000;

fn tails() -> &'static Mutex<HashMap<String, String>> {
    static TAILS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    TAILS.get_or_init(Default::default)
}

/// Record the latest agent message of the session whose row ID is `session_id`.
pub fn remember(session_id: &str, message: &str) {
    let message = message.trim();
    if message.is_empty() {
        return;
    }
    let start = message
        .char_indices()
        .rev()
        .nth(TAIL_CHARS - 1)
        .map_or(0, |(index, _)| index);
    if let Ok(mut tails) = tails().lock() {
        if tails.get(session_id).map(String::as_str) != Some(&message[start..]) {
            tails.insert(session_id.to_owned(), message[start..].to_owned());
        }
    }
}

pub fn get(session_id: &str) -> Option<String> {
    tails().lock().ok()?.get(session_id).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_end_of_long_messages() {
        let long = format!("{}Which one?", "a".repeat(5_000));
        remember("last-message-test", &long);
        let kept = get("last-message-test").unwrap();
        assert_eq!(kept.chars().count(), TAIL_CHARS);
        assert!(kept.ends_with("Which one?"));
        remember("last-message-test", "  ");
        assert!(get("last-message-test").unwrap().ends_with("Which one?"));
    }
}
