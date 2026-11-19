use super::source::{Capture, Message};
use super::{hash, random_id};
use anyhow::{bail, Result};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

pub struct Journal {
    pub db: Connection,
}
pub struct Checkpoint {
    pub cursor: i64,
    pub observed: i64,
    pub native: bool,
    pub state: Value,
}
impl Journal {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        let app: i64 = db.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        let count: i64 = db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table'",
            [],
            |r| r.get(0),
        )?;
        if (count > 0 || app != 0) && app != 0x41564252 {
            bail!("not an Agent View bridge journal");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        db.execute_batch("PRAGMA application_id=1096172114; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
          CREATE TABLE IF NOT EXISTS checkpoint(session_id TEXT PRIMARY KEY,cursor INTEGER NOT NULL,observed INTEGER NOT NULL,native INTEGER NOT NULL,state TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS cache(session_id TEXT NOT NULL,message_id TEXT NOT NULL,hash TEXT NOT NULL,PRIMARY KEY(session_id,message_id));
          CREATE TABLE IF NOT EXISTS pending(n INTEGER PRIMARY KEY AUTOINCREMENT,body TEXT NOT NULL,native INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS delivery(operation_id TEXT PRIMARY KEY,body TEXT NOT NULL,accepted INTEGER NOT NULL DEFAULT 0);")?;
        Ok(Self { db })
    }
    pub fn checkpoint(&self, id: &str) -> Result<Option<Checkpoint>> {
        let row = self
            .db
            .query_row(
                "SELECT cursor,observed,native,state FROM checkpoint WHERE session_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, String>(3)?)),
            )
            .optional()?;
        row.map(|(cursor, observed, native, state)| {
            Ok(Checkpoint {
                cursor,
                observed,
                native,
                state: serde_json::from_str(&state)?,
            })
        })
        .transpose()
    }
    pub fn cached(&self, id: &str) -> Result<BTreeMap<String, String>> {
        let mut q = self
            .db
            .prepare("SELECT message_id,hash FROM cache WHERE session_id=?1")?;
        let rows = q
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
        Ok(rows)
    }
    pub fn known(&self) -> Result<Vec<String>> {
        let mut q = self.db.prepare("SELECT session_id FROM checkpoint")?;
        let rows = q
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    pub fn pending(&self) -> Result<Option<Value>> {
        self.db
            .query_row("SELECT body FROM pending ORDER BY n LIMIT 1", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .map(|v| Ok(serde_json::from_str(&v)?))
            .transpose()
    }
    pub fn stage(&mut self, id: &str, capture: Capture, native: bool) -> Result<bool> {
        if self.pending()?.is_some() {
            bail!("drain pending history before staging");
        }
        let old = self.checkpoint(id)?;
        if old.as_ref().is_some_and(|p| p.native != native) {
            bail!("source format changed; re-pair required");
        }
        let cached = self.cached(id)?;
        let mut changed = Vec::new();
        for message in capture.messages {
            if cached.get(&message.id) != Some(&message_hash(&serde_json::to_value(&message)?)) {
                changed.push(message);
            }
        }
        let deletes = cached
            .keys()
            .filter(|mid| !capture.ids.contains(*mid))
            .cloned()
            .collect::<Vec<_>>();
        let baseline = old.is_none();
        if !baseline
            && changed.is_empty()
            && deletes.is_empty()
            && old.as_ref().is_some_and(|p| p.state == capture.state)
        {
            // Observed source events with no visible projection change. This is NOT a cloud ACK.
            self.db.execute(
                "UPDATE checkpoint SET observed=?2 WHERE session_id=?1",
                rusqlite::params![id, capture.through],
            )?;
            return Ok(false);
        }
        let from = old.map(|p| p.cursor).unwrap_or(0);
        if !baseline && capture.through <= from {
            bail!("visible source change without durable revision");
        }
        let upload = random_id()?;
        let mut chunks: Vec<(Vec<Message>, Vec<String>)> = vec![(Vec::new(), Vec::new())];
        let mut bytes = 0;
        for message in changed {
            let size = serde_json::to_vec(&message)?.len();
            if size > 850_000 {
                bail!("single message exceeds upload byte limit");
            }
            if !chunks.last().unwrap().0.is_empty()
                && (bytes + size > 850_000 || chunks.last().unwrap().0.len() >= 400)
            {
                chunks.push((Vec::new(), Vec::new()));
                bytes = 0;
            }
            bytes += size;
            chunks.last_mut().unwrap().0.push(message);
        }
        for removed in deletes {
            if chunks.last().unwrap().1.len() >= 400 {
                chunks.push((Vec::new(), Vec::new()));
            }
            chunks.last_mut().unwrap().1.push(removed);
        }
        let total = chunks.len();
        let tx = self.db.transaction()?;
        for (chunk, (messages, delete_ids)) in chunks.into_iter().enumerate() {
            let body = json!({"version":1,"batch_id":random_id()?,"session_id":id,"kind":if baseline{"baseline"}else{"delta"},"from":from,"through":capture.through,
                "upload_id":upload,"chunk":chunk,"complete":chunk+1==total,"session":capture.state,"messages":messages,"delete_ids":delete_ids});
            tx.execute(
                "INSERT INTO pending(body,native) VALUES (?1,?2)",
                rusqlite::params![body.to_string(), native],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }
    /// Drops batches the cloud permanently refused. The checkpoint is untouched,
    /// so nothing is marked as synced; the session is retried later.
    pub fn discard_pending(&mut self, session_id: &str) -> Result<()> {
        let mut q = self.db.prepare("SELECT n,body FROM pending ORDER BY n")?;
        let rows = q
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(q);
        for (n, body) in rows {
            let matches = serde_json::from_str::<Value>(&body)
                .map(|v| v["session_id"] == session_id)
                .unwrap_or(true);
            if matches {
                self.db.execute("DELETE FROM pending WHERE n=?1", [n])?;
            }
        }
        Ok(())
    }
    pub fn ack(&mut self, receipt: &Value) -> Result<()> {
        let tx = self.db.transaction()?;
        let (n, raw, native): (i64, String, bool) = tx.query_row(
            "SELECT n,body,native FROM pending ORDER BY n LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let body: Value = serde_json::from_str(&raw)?;
        let complete = body["complete"].as_bool().unwrap_or(true);
        if receipt["batch_id"] != body["batch_id"]
            || receipt["session_id"] != body["session_id"]
            || receipt["committed"] != json!(complete)
            || receipt["through"]
                != if complete {
                    body["through"].clone()
                } else {
                    Value::Null
                }
            || receipt["next_chunk"]
                != if complete {
                    Value::Null
                } else {
                    json!(body["chunk"].as_u64().unwrap() + 1)
                }
        {
            bail!("invalid sync acknowledgement");
        }
        let id = body["session_id"].as_str().unwrap();
        for value in body["messages"].as_array().unwrap() {
            let m: Message = serde_json::from_value(value.clone())?;
            tx.execute("INSERT INTO cache VALUES (?1,?2,?3) ON CONFLICT(session_id,message_id) DO UPDATE SET hash=excluded.hash",rusqlite::params![id,m.id,message_hash(value)])?;
        }
        for mid in body["delete_ids"].as_array().unwrap() {
            tx.execute(
                "DELETE FROM cache WHERE session_id=?1 AND message_id=?2",
                rusqlite::params![id, mid.as_str().unwrap()],
            )?;
        }
        if complete {
            tx.execute("INSERT INTO checkpoint VALUES (?1,?2,?2,?3,?4) ON CONFLICT(session_id) DO UPDATE SET cursor=excluded.cursor,observed=excluded.observed,native=excluded.native,state=excluded.state",rusqlite::params![id,body["through"].as_i64().unwrap(),native,body["session"].to_string()])?;
            if body["session"]["deleted"] == true {
                tx.execute("DELETE FROM cache WHERE session_id=?1", [id])?;
            }
        }
        tx.execute("DELETE FROM pending WHERE n=?1", [n])?;
        tx.commit()?;
        Ok(())
    }
    pub fn tombstone(&mut self, id: &str) -> Result<()> {
        let Some(old) = self.checkpoint(id)? else {
            return Ok(());
        };
        if old.state["deleted"] == true {
            return Ok(());
        }
        let mut state = old.state;
        state["deleted"] = json!(true);
        let through = old.observed.max(old.cursor) + 1;
        let body = json!({"version":1,"batch_id":random_id()?,"session_id":id,"kind":"delta","from":old.cursor,"through":through,"session":state,"messages":[],"delete_ids":[]});
        self.db.execute(
            "INSERT INTO pending(body,native) VALUES (?1,?2)",
            rusqlite::params![body.to_string(), old.native],
        )?;
        Ok(())
    }
    pub fn source_matches(&self, id: &str, capture: &Capture) -> Result<bool> {
        let Some(checkpoint) = self.checkpoint(id)? else {
            return Ok(false);
        };
        let current = capture
            .messages
            .iter()
            .map(|m| Ok((m.id.clone(), message_hash(&serde_json::to_value(m)?))))
            .collect::<Result<BTreeMap<_, _>>>()?;
        Ok(current == self.cached(id)? && checkpoint.state == capture.state)
    }
}

fn message_hash(value: &Value) -> String {
    let mut visible = value.clone();
    if let Some(object) = visible.as_object_mut() {
        object.remove("updated_at");
    }
    hash(&visible)
}
