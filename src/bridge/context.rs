use super::source::Capture;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Compare only already-read bytes. Streaming suffixes and appended turns do
/// not make queued input stale; rewriting/removing the read context does.
pub(super) fn matches(capture: &Capture, guard: &Value) -> bool {
    let Some(anchor) = guard["anchor_order"].as_i64().filter(|n| *n >= -1) else {
        return false;
    };
    let Some(anchor_bytes) = guard["anchor_bytes"].as_u64().filter(|n| *n <= 1_000_000) else {
        return false;
    };
    if guard["version"] != 1 || guard["visible_before"] != capture.state["visible_before"] {
        return false;
    }
    let cutoff = capture.state["visible_before"].as_i64();
    let mut messages = capture
        .messages
        .iter()
        .filter(|m| m.order <= anchor && cutoff.map_or(true, |n| m.order < n))
        .collect::<Vec<_>>();
    messages.sort_by(|a, b| (a.order, &a.id).cmp(&(b.order, &b.id)));
    let mut prefix = Sha256::new();
    for message in messages {
        let text = if message.order == anchor {
            let Some(prefix) = message.text.as_bytes().get(..anchor_bytes as usize) else {
                return false;
            };
            prefix
        } else {
            message.text.as_bytes()
        };
        let text_hash = format!("{:x}", Sha256::digest(text));
        let leaf = format!(
            "{}:{}:{}:{}:{}",
            message.id.len(),
            message.id,
            message.order,
            message.role,
            text_hash
        );
        prefix.update(format!("{:x}", Sha256::digest(leaf.as_bytes())).as_bytes());
    }
    guard["prefix_hash"].as_str() == Some(format!("{:x}", prefix.finalize()).as_str())
}
