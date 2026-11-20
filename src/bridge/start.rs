//! Path policy and the local claim for a start. The claim is written before
//! OpenCode is called, so a crash cannot create a second session for the same id.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::path::{Path, PathBuf};

const SECRET: &[&str] = &[".ssh", ".gnupg", ".aws", ".kube", ".azure", "Keychains"];

/// Canonical directory under the owner's home, or a short reason code.
pub fn allowed_directory(path: &Path, home: &Path) -> std::result::Result<PathBuf, &'static str> {
    if !path.is_absolute() {
        return Err("not_a_directory");
    }
    let home = home.canonicalize().map_err(|_| "not_allowed")?;
    let canonical = path.canonicalize().map_err(|_| "not_a_directory")?;
    if !canonical.is_dir() {
        return Err("not_a_directory");
    }
    let rel = canonical.strip_prefix(&home).map_err(|_| "not_allowed")?;
    if rel.as_os_str().is_empty() {
        return Err("not_allowed");
    }
    let text = rel.to_string_lossy();
    if text == "Library" || text.starts_with("Library/") || text.starts_with(".config/") {
        return Err("not_allowed");
    }
    for component in rel.components() {
        let Some(name) = component.as_os_str().to_str() else {
            return Err("not_allowed");
        };
        if SECRET.contains(&name) {
            return Err("not_allowed");
        }
    }
    Ok(canonical)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Claim {
    Launch,
    /// A previous attempt did not record an id. Do not create another session.
    Uncertain,
    Resume {
        session_id: String,
        prompt_accepted: bool,
    },
}

pub fn claim(db: &Connection, operation: &str, body: &str) -> Result<Claim> {
    let tx = db.unchecked_transaction()?;
    let existing = tx
        .query_row(
            "SELECT session_id, prompt_accepted FROM start_op WHERE operation_id=?1",
            [operation],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                ))
            },
        )
        .optional()?;
    let claim = if let Some((session_id, accepted)) = existing {
        if let Some(session_id) = session_id {
            Claim::Resume {
                session_id,
                prompt_accepted: accepted == Some(1),
            }
        } else {
            Claim::Uncertain
        }
    } else {
        tx.execute(
            "INSERT INTO start_op(operation_id, body) VALUES (?1, ?2)",
            rusqlite::params![operation, body],
        )?;
        Claim::Launch
    };
    tx.commit()?;
    Ok(claim)
}

pub fn record(
    db: &Connection,
    operation: &str,
    session_id: &str,
    prompt_accepted: bool,
) -> Result<()> {
    db.execute(
        "UPDATE start_op SET session_id=?1, prompt_accepted=?2 WHERE operation_id=?3",
        rusqlite::params![session_id, i64::from(prompt_accepted), operation],
    )?;
    Ok(())
}

pub fn finish(db: &Connection, operation: &str) -> Result<()> {
    db.execute("DELETE FROM start_op WHERE operation_id=?1", [operation])?;
    Ok(())
}
