//! Progress facts for a working session, read from the OpenCode database.
//! A bridge restart recomputes these from the source. It does not invent a stall
//! from its own uptime, and a missing read is unknown, never stalled or failed.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::collections::BTreeMap;

/// Silence this long, with no open tool or shell, is only a possible stall.
pub const STALL_AFTER_MS: u64 = 10 * 60 * 1000;
const REAL_MS: u64 = 1_000_000_000_000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    pub turn_started_at: Option<u64>,
    pub last_progress_at: Option<u64>,
    pub in_flight: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    pub health: &'static str,
    pub turn_started_at: Option<u64>,
    pub last_progress_at: Option<u64>,
    pub last_progress_kind: Option<&'static str>,
    pub in_flight: Option<&'static str>,
}

/// Confirmed failure comes only from the native classifier. Silence is not failure.
pub fn classify(
    status: &str,
    reason: Option<&str>,
    facts: Option<&Facts>,
    now_ms: u64,
) -> &'static str {
    if reason == Some("failed") {
        return "failed";
    }
    if status == "unknown" || status != "working" {
        return if status == "unknown" {
            "unknown"
        } else {
            "idle"
        };
    }
    let Some(facts) = facts else {
        return "unknown";
    };
    if facts.in_flight.is_some() {
        return "running";
    }
    let Some(mark) = facts.last_progress_at.or(facts.turn_started_at) else {
        return "unknown";
    };
    if now_ms.saturating_sub(mark) < STALL_AFTER_MS {
        "running"
    } else {
        "potentially_stalled"
    }
}

pub fn progress(
    db: &Connection,
    ids: &[String],
    now_ms: u64,
) -> Result<BTreeMap<String, Progress>> {
    let mut out = BTreeMap::new();
    for id in ids {
        let mut parts = vec![facts_for(db, id)?];
        for child in child_ids(db, id)? {
            parts.push(facts_for(db, &child)?);
        }
        let facts = merge(&parts);
        let kind = if facts.in_flight == Some("tool") {
            Some("tool")
        } else if facts.in_flight == Some("shell") {
            Some("shell")
        } else if facts.last_progress_at.is_some() {
            Some("text")
        } else {
            None
        };
        out.insert(
            id.clone(),
            Progress {
                health: classify("working", None, Some(&facts), now_ms),
                turn_started_at: facts.turn_started_at,
                last_progress_at: facts.last_progress_at,
                last_progress_kind: kind,
                in_flight: facts.in_flight,
            },
        );
    }
    Ok(out)
}

fn merge(parts: &[Facts]) -> Facts {
    let mut out = Facts::default();
    for facts in parts {
        out.turn_started_at = max_ms(out.turn_started_at, facts.turn_started_at);
        out.last_progress_at = max_ms(out.last_progress_at, facts.last_progress_at);
        if (facts.in_flight == Some("tool") || out.in_flight.is_none()) && facts.in_flight.is_some()
        {
            out.in_flight = facts.in_flight;
        }
    }
    out
}

fn max_ms(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn ms(value: Option<i64>) -> Option<u64> {
    let value = u64::try_from(value?).ok()?;
    (value > REAL_MS).then_some(value)
}

fn facts_for(db: &Connection, id: &str) -> Result<Facts> {
    let turn_started_at = ms(db
        .query_row(
            "SELECT time_created FROM session_message WHERE session_id=?1 AND type='user' AND COALESCE(json_extract(data,'$.synthetic'),0)=0 ORDER BY seq DESC LIMIT 1",
            [id],
            |row| row.get(0),
        )
        .optional()?);
    let last_progress_at = ms(db.query_row(
        "SELECT MAX(time_updated) FROM session_message WHERE session_id=?1 AND type IN ('assistant','shell','compaction')",
        [id],
        |row| row.get::<_, Option<i64>>(0),
    )?);
    let tool_open: Option<i64> = db
        .query_row(
            "SELECT 1 FROM session_message m, json_each(m.data, '$.content') c WHERE m.session_id=?1 AND m.type='assistant' AND json_extract(c.value,'$.type')='tool' AND COALESCE(json_extract(c.value,'$.name'),'') NOT IN ('question','') AND (json_extract(c.value,'$.state.status') IN ('pending','running') OR (json_extract(c.value,'$.state.status') IS NULL AND json_extract(m.data,'$.time.completed') IS NULL)) LIMIT 1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    let shell_open: Option<i64> = db
        .query_row(
            "SELECT 1 FROM session_message WHERE session_id=?1 AND type='shell' AND json_extract(data,'$.status') IN ('running','pending') LIMIT 1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(Facts {
        turn_started_at,
        last_progress_at,
        in_flight: if tool_open.is_some() {
            Some("tool")
        } else if shell_open.is_some() {
            Some("shell")
        } else {
            None
        },
    })
}

fn child_ids(db: &Connection, parent: &str) -> Result<Vec<String>> {
    let mut statement = db.prepare("SELECT id FROM session_v2 WHERE parent_id=?1")?;
    let rows = statement
        .query_map([parent], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(rows)
}
