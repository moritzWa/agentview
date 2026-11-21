use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: String,
    pub role: String,
    pub order: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub text: String,
}

/// The bytes both sides hash. Field order is fixed so a Rust hash equals a JS hash.
/// The sync cursor is left out on purpose: it moves on events that change no visible
/// text, so two identical copies can sit at different cursors. Pointers are compared
/// separately.
pub fn projection_text(state: &Value, messages: &[Message]) -> String {
    let visible = state
        .get("visible_before")
        .and_then(|v| v.as_i64())
        .map(|n| n.to_string())
        .unwrap_or_default();
    let mut lines = vec![
        format!("archived={}", state["archived"].as_bool().unwrap_or(false)),
        format!("deleted={}", state["deleted"].as_bool().unwrap_or(false)),
        format!("title={}", state["title"].as_str().unwrap_or("")),
        format!("visible_before={visible}"),
    ];
    let mut ordered = messages.to_vec();
    ordered.sort_by(|a, b| a.order.cmp(&b.order).then(a.id.cmp(&b.id)));
    for message in ordered {
        lines.push(format!(
            "{}\t{}\t{}\t{}",
            message.id, message.order, message.role, message.text
        ));
    }
    lines.join("\n")
}

pub struct Capture {
    pub through: i64,
    pub state: Value,
    pub messages: Vec<Message>,
    pub ids: BTreeSet<String>,
    pub cwd: String,
}

pub fn open(path: &Path) -> Result<Connection> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(Duration::from_millis(200))?;
    check_schema(&db)?;
    Ok(db)
}

fn has_table(db: &Connection, name: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [name],
        |r| r.get(0),
    )?)
}

fn check_schema(db: &Connection) -> Result<()> {
    for table in ["session_v2", "session_message", "event_sequence", "event"] {
        if !has_table(db, table)? {
            bail!("unsupported OpenCode source schema");
        }
    }
    Ok(())
}

pub fn sessions(db: &Connection) -> Result<Vec<String>> {
    let deadline = Instant::now() + Duration::from_secs(60);
    db.progress_handler(10_000, Some(move || Instant::now() > deadline));
    let mut s = db.prepare("SELECT id FROM session_v2 ORDER BY id")?;
    let mut ids = s
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    if has_table(db, "session")? {
        let mut s = db.prepare(
            "SELECT id FROM main.session WHERE id NOT IN (SELECT id FROM session_v2) ORDER BY id",
        )?;
        ids.extend(
            s.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?,
        );
    }
    Ok(ids)
}

pub fn native(db: &Connection, id: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM session_v2 WHERE id=?1)",
        [id],
        |r| r.get(0),
    )?)
}

pub fn watermark(db: &Connection, id: &str) -> Result<i64> {
    if native(db, id)? {
        return Ok(db
            .query_row(
                "SELECT seq FROM event_sequence WHERE aggregate_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0));
    }
    // Unmigrated V1 history has no durable V2 event cursor. Use its monotonic
    // timestamp watermark and independently compare IDs. A format transition
    // to V2 must require re-pairing rather than reinterpret the old bookmark.
    if has_table(db, "session")? {
        return Ok(db.query_row("SELECT max(COALESCE((SELECT max(time_updated) FROM main.part WHERE session_id=?1),0),COALESCE((SELECT time_updated FROM main.session WHERE id=?1),0))",[id],|r|r.get(0))?);
    }
    Ok(0)
}

pub fn capture(db: &mut Connection, id: &str, after: Option<i64>) -> Result<Option<Capture>> {
    let deadline = Instant::now() + Duration::from_secs(30);
    db.progress_handler(10_000, Some(move || Instant::now() > deadline));
    let tx = db.transaction()?;
    let v2 = native(&tx, id)?;
    let table = if v2 {
        "session_v2"
    } else if has_table(&tx, "session")? {
        "main.session"
    } else {
        return Ok(None);
    };
    let meta=tx.query_row(&format!("SELECT COALESCE(title,'New session'),directory,time_archived,revert FROM {table} WHERE id=?1"),[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<i64>>(2)?,r.get::<_,Option<String>>(3)?))).optional()?;
    let Some((title, cwd, archived, revert)) = meta else {
        return Ok(None);
    };
    let through = watermark(&tx, id)?;
    if after.is_some_and(|old| through < old) {
        bail!("source watermark regressed; re-pair after restore");
    }
    let mut dirty = BTreeSet::new();
    let mut reconcile = after.is_none();
    if v2 {
        if let Some(after) = after {
            let mut q=tx.prepare("SELECT seq,type,COALESCE(json_extract(data,'$.messageID'),json_extract(data,'$.assistantMessageID'),json_extract(data,'$.inboxID'),json_extract(data,'$.compactionMessageID')) FROM event WHERE aggregate_id=?1 AND seq>?2 AND seq<=?3 ORDER BY seq")?;
            let mut rows = q.query(rusqlite::params![id, after, through])?;
            let mut expected = after + 1;
            while let Some(row) = rows.next()? {
                let seq: i64 = row.get(0)?;
                let kind: String = row.get(1)?;
                if seq != expected {
                    reconcile = true;
                }
                expected = seq + 1;
                // A new OpenCode event name must not freeze this session forever.
                // Reproject the current messages instead of trusting this event.
                if validate_event(&kind).is_ok() {
                    if let Some(mid) = row.get::<_, Option<String>>(2)? {
                        dirty.insert(mid);
                    }
                } else {
                    reconcile = true;
                }
            }
            // V2 persists sequence watermarks even when event-body persistence
            // is disabled. Missing replay ranges require a current projection
            // reconciliation, not a guessed cursor or permanent re-pair loop.
            if expected != through + 1 {
                reconcile = true;
            }
        }
    }
    let mut ids = BTreeSet::new();
    let mut messages = Vec::new();
    let mut legacy_order = BTreeMap::new();
    if v2 {
        let mut q=tx.prepare("SELECT id FROM session_message WHERE session_id=?1 AND type IN ('user','assistant') ORDER BY seq")?;
        ids = q
            .query_map([id], |r| r.get(0))?
            .collect::<rusqlite::Result<BTreeSet<String>>>()?;
        let targets = if reconcile { ids.clone() } else { dirty };
        for mid in targets {
            let row=tx.query_row("SELECT id,type,seq,time_created,time_updated,data FROM session_message WHERE id=?1 AND session_id=?2",rusqlite::params![mid,id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?,r.get::<_,String>(5)?))).optional()?;
            if let Some((mid, kind, order, created, updated, raw)) = row {
                if kind == "assistant" {
                    let value: Value = parse_json(&raw).with_context(|| {
                        format!("invalid assistant message {mid} in session {id}")
                    })?;
                    if !value["content"].is_array() && !has_table(&tx, "session_part")? {
                        bail!("unknown assistant content schema");
                    }
                }
                if let Some(mut message) = project(&mid, &kind, order, created, updated, &raw)? {
                    if kind == "assistant"
                        && message.text.is_empty()
                        && has_table(&tx, "session_part")?
                    {
                        message.text = split_text(&tx, &mid)?;
                    }
                    if !message.text.trim().is_empty() {
                        messages.push(message);
                    } else {
                        ids.remove(&mid);
                    }
                }
            }
        }
    } else {
        let mut q=tx.prepare("SELECT id,json_extract(data,'$.role'),time_created,time_updated FROM main.message WHERE session_id=?1 ORDER BY time_created,id")?;
        let rows = q
            .query_map([id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (order, (mid, role, created, updated)) in rows.into_iter().enumerate() {
            legacy_order.insert(mid.clone(), order as i64);
            if !matches!(role.as_str(), "user" | "assistant") {
                continue;
            }
            let mut p=tx.prepare("SELECT json_extract(data,'$.text') FROM main.part WHERE message_id=?1 AND json_extract(data,'$.type')='text' AND COALESCE(json_extract(data,'$.synthetic'),0)=0 AND COALESCE(json_extract(data,'$.ignored'),0)=0 ORDER BY id")?;
            let text = p
                .query_map([&mid], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .join("\n");
            if !text.trim().is_empty() {
                ids.insert(mid.clone());
                messages.push(Message {
                    id: mid,
                    role,
                    order: order as i64,
                    created_at: created,
                    updated_at: updated,
                    text,
                });
            }
        }
    }
    messages.sort_by_key(|m| m.order);
    let visible_before = if let Some(raw) = revert {
        let value: Value = serde_json::from_str(&raw).context("invalid revert metadata")?;
        let mid = value
            .get("messageID")
            .and_then(Value::as_str)
            .context("unsupported revert metadata")?;
        if v2 {
            Some(tx.query_row(
                "SELECT seq FROM session_message WHERE session_id=?1 AND id=?2",
                rusqlite::params![id, mid],
                |r| r.get::<_, i64>(0),
            )?)
        } else {
            let order = legacy_order
                .get(mid)
                .copied()
                .context("legacy revert boundary missing")?;
            Some(order)
        }
    } else {
        None
    };
    tx.commit()?;
    Ok(Some(Capture {
        through,
        state: json!({"title":fit_title(title),"archived":archived.is_some(),"visible_before":visible_before,"deleted":false}),
        messages,
        ids,
        cwd,
    }))
}

fn split_text(db: &Connection, id: &str) -> Result<String> {
    let mut q =
        db.prepare("SELECT type,data FROM session_part WHERE message_id=?1 ORDER BY ordinal")?;
    let mut parts = Vec::new();
    for row in q.query_map([id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })? {
        let (kind, raw) = row?;
        if kind == "text" {
            let p: Value =
                parse_json(&raw).with_context(|| format!("invalid text part in message {id}"))?;
            parts.push(p["text"].as_str().context("invalid split text")?.to_owned());
        } else if !matches!(kind.as_str(), "tool" | "reasoning") {
            bail!("unknown source part type");
        }
    }
    Ok(parts.join("\n"))
}

/// OpenCode can store text cut in the middle of an emoji, leaving a lone UTF-16
/// surrogate escape that serde_json rejects. One such message must not block
/// syncing every later session, so retry with those escapes replaced by U+FFFD.
pub fn parse_json(raw: &str) -> Result<Value> {
    match serde_json::from_str(raw) {
        Ok(value) => Ok(value),
        Err(first) => serde_json::from_str(&replace_lone_surrogates(raw)).map_err(|_| first.into()),
    }
}

fn replace_lone_surrogates(raw: &str) -> String {
    fn unit(chars: &[char], at: usize) -> Option<u32> {
        if chars.get(at) != Some(&'\\') || chars.get(at + 1) != Some(&'u') {
            return None;
        }
        let hex: String = chars.get(at + 2..at + 6)?.iter().collect();
        u32::from_str_radix(&hex, 16).ok()
    }
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '\\' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        match unit(&chars, i) {
            Some(0xD800..=0xDBFF) => {
                if matches!(unit(&chars, i + 6), Some(0xDC00..=0xDFFF)) {
                    out.extend(&chars[i..i + 12]);
                    i += 12;
                } else {
                    out.push_str("\\ufffd");
                    i += 6;
                }
            }
            Some(0xDC00..=0xDFFF) => {
                out.push_str("\\ufffd");
                i += 6;
            }
            _ => {
                out.push('\\');
                if let Some(next) = chars.get(i + 1) {
                    out.push(*next);
                }
                i += 2;
            }
        }
    }
    out
}

pub fn project(
    id: &str,
    kind: &str,
    order: i64,
    created: i64,
    updated: i64,
    raw: &str,
) -> Result<Option<Message>> {
    if !matches!(kind, "user" | "assistant") {
        return Ok(None);
    }
    let value: Value = parse_json(raw).context("invalid source message")?;
    let text = if kind == "user" {
        value["text"]
            .as_str()
            .context("unknown user text schema")?
            .to_owned()
    } else if let Some(parts) = value["content"].as_array() {
        let mut texts = Vec::new();
        for p in parts {
            match p["type"].as_str() {
                Some("text") => texts.push(
                    p["text"]
                        .as_str()
                        .context("invalid source text")?
                        .to_owned(),
                ),
                Some("tool" | "reasoning") => {}
                _ => bail!("unknown source part type"),
            }
        }
        texts.join("\n")
    } else {
        String::new()
    };
    Ok(Some(Message {
        id: id.to_owned(),
        role: kind.to_owned(),
        order,
        created_at: created,
        updated_at: updated,
        text: fit_text(text),
    }))
}

const TEXT_LIMIT: usize = 800_000;
const TRUNCATED: &str = "\n[BetterAgentView omitted the rest of this message: too large to sync]";

/// The cloud rejects NUL characters and oversized text. A single such message
/// must not stall its session forever, so make it fit and say so visibly.
fn fit_text(text: String) -> String {
    let mut text = if text.contains('\0') {
        text.replace('\0', "\u{fffd}")
    } else {
        text
    };
    if serde_json::to_vec(&text).map_or(0, |v| v.len()) <= TEXT_LIMIT {
        return text;
    }
    while serde_json::to_vec(&text).map_or(0, |v| v.len()) > TEXT_LIMIT - TRUNCATED.len() {
        let mut end = text.len() * 9 / 10;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text.push_str(TRUNCATED);
    text
}

fn fit_title(title: String) -> String {
    let title = title.replace('\0', "\u{fffd}");
    let mut units = 0;
    let mut end = title.len();
    for (index, ch) in title.char_indices() {
        units += ch.len_utf16();
        if units > 1000 {
            end = index;
            break;
        }
    }
    title[..end].to_owned()
}

fn validate_event(kind: &str) -> Result<()> {
    let (name, version) = kind.rsplit_once('.').context("unversioned source event")?;
    let version: u32 = version.parse().context("unknown source event version")?;
    let known = [
        "created",
        "agent.selected",
        "model.selected",
        "moved",
        "renamed",
        "metadata.updated",
        "permissions",
        "viewed",
        "archived",
        "unarchived",
        "message.content.updated",
        "usage.recorded",
        "deleted",
        "forked",
        "inbox.delivered",
        "inbox.enqueued",
        "inbox.cancelled",
        "inbox.withdrawn",
        "inbox.delivery.changed",
        "execution.started",
        "execution.succeeded",
        "execution.failed",
        "execution.interrupted",
        "instructions.updated",
        "synthetic",
        "skill.activated",
        "shell.started",
        "shell.ended",
        "step.started",
        "step.first-token",
        "step.streamed",
        "step.ended",
        "step.failed",
        "text.started",
        "text.ended",
        "reasoning.started",
        "reasoning.ended",
        "tool.input.started",
        "tool.input.ended",
        "tool.called",
        "tool.progress",
        "tool.success",
        "tool.failed",
        "retry.scheduled",
        "compaction.started",
        "compaction.ended",
        "compaction.failed",
        "revert.staged",
        "revert.cleared",
        "revert.committed",
    ];
    let short = name
        .strip_prefix("session.")
        .context("unknown event aggregate")?;
    if version == 0 || version > 2 || !known.contains(&short) {
        bail!("unsupported source event; update BetterAgentView");
    }
    Ok(())
}
