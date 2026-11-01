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

/// The end of `text`, at most `max` characters, starting at a sentence start
/// when one is close to the cut. A long message's opening is mostly preamble;
/// its result or question comes last.
pub fn end_excerpt(text: &str, max: usize) -> String {
    let text = text.trim();
    let total = text.chars().count();
    if total <= max {
        return text.to_owned();
    }
    let skip = total - max;
    let tail: String = text.chars().skip(skip).collect();
    // A sentence start in the first third of the excerpt spares a fragment.
    let window = tail.chars().take(max / 3).collect::<String>();
    let start = [". ", "? ", "! "]
        .iter()
        .filter_map(|mark| window.find(mark).map(|index| index + mark.len()))
        .min();
    match start {
        Some(start) => tail[start..].to_owned(),
        None => format!("…{}", tail.trim_start()),
    }
}

/// What Voice Mode reads about a session without asking its provider: when the
/// person last wrote to it, and how its last message ends.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Facts {
    pub last_user_ms: u64,
    pub tail: String,
}

fn facts() -> &'static Mutex<HashMap<String, Facts>> {
    static FACTS: OnceLock<Mutex<HashMap<String, Facts>>> = OnceLock::new();
    FACTS.get_or_init(Default::default)
}

pub fn remember_facts(session_id: &str, last_user_ms: u64, tail: &str) {
    if let Ok(mut facts) = facts().lock() {
        let entry = Facts {
            last_user_ms,
            tail: tail.to_owned(),
        };
        if facts.get(session_id) != Some(&entry) {
            facts.insert(session_id.to_owned(), entry);
        }
    }
}

pub fn facts_for(session_id: &str) -> Option<Facts> {
    facts().lock().ok()?.get(session_id).cloned()
}

pub fn get(session_id: &str) -> Option<String> {
    tails().lock().ok()?.get(session_id).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_excerpt_starts_at_a_sentence_when_one_is_close() {
        let text = format!(
            "{} It broke. The fix is merged. Want me to rerun?",
            "word ".repeat(80)
        );
        let excerpt = end_excerpt(&text, 45);
        assert_eq!(excerpt, "The fix is merged. Want me to rerun?");
        assert!(!excerpt.starts_with('…'));
        assert_eq!(end_excerpt("Short one.", 60), "Short one.");
        assert!(end_excerpt(&"x".repeat(200), 60).starts_with('…'));
    }

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
