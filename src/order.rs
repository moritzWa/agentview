//! Manual row order inside directory groups.
//!
//! Directory groups sort newest-first by start time. Moving a row stores a
//! sort key that replaces its start time, so moved rows keep their place
//! while new sessions still arrive at the top. Like pauses, this is a local
//! display preference and keys for ids discovery no longer returns are unused.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::paused::{
    ensure_private_directory, ensure_private_regular_file, temporary_path, validate_session_id,
    RegistryLock,
};

const REGISTRY_VERSION: u32 = 1;

#[derive(Debug, Deserialize, Serialize)]
struct OrderDocument {
    version: u32,
    sessions: Vec<OrderRecord>,
}

#[derive(Debug, Deserialize, Serialize)]
struct OrderRecord {
    id: String,
    sort_key_ms: u64,
}

/// Cloneable sort-key registry shared by the dashboard.
#[derive(Clone, Debug)]
pub struct SessionOrder {
    path: PathBuf,
    records: Arc<Mutex<BTreeMap<String, u64>>>,
}

impl SessionOrder {
    pub fn load_default() -> Result<Self> {
        Self::load(default_session_order_path()?)
    }

    pub fn load(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            ensure_private_directory(parent)?;
        }
        let records = read_registry(&path)?;
        Ok(Self {
            path,
            records: Arc::new(Mutex::new(records)),
        })
    }

    pub fn sort_keys(&self) -> BTreeMap<String, u64> {
        self.records
            .lock()
            .expect("session-order registry mutex poisoned")
            .clone()
    }

    pub fn set(&self, updates: &[(String, u64)]) -> Result<()> {
        for (session_id, _) in updates {
            validate_session_id(session_id)?;
        }
        let parent = self
            .path
            .parent()
            .context("session-order registry path has no parent")?;
        let _lock = RegistryLock::acquire(&parent.join("session-order.lock"))?;
        let mut records = read_registry(&self.path)?;
        for (session_id, sort_key_ms) in updates {
            records.insert(session_id.clone(), *sort_key_ms);
        }
        write_registry(&self.path, &records)?;
        *self
            .records
            .lock()
            .expect("session-order registry mutex poisoned") = records;
        Ok(())
    }
}

pub fn default_session_order_path() -> Result<PathBuf> {
    if let Some(state_home) = crate::fs_util::xdg_home("XDG_STATE_HOME") {
        return Ok(PathBuf::from(state_home)
            .join("agentview")
            .join("session-order.json"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/state/agentview/session-order.json"))
}

fn read_registry(path: &Path) -> Result<BTreeMap<String, u64>> {
    match fs::symlink_metadata(path) {
        Ok(_) => ensure_private_regular_file(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect session order {}", path.display()))
        }
    }
    let input = fs::read_to_string(path)
        .with_context(|| format!("failed to read session order {}", path.display()))?;
    let document: OrderDocument = serde_json::from_str(&input)
        .with_context(|| format!("invalid session order registry {}", path.display()))?;
    if document.version != REGISTRY_VERSION {
        bail!(
            "unsupported session order registry version {} in {}",
            document.version,
            path.display()
        );
    }
    let mut records = BTreeMap::new();
    for record in document.sessions {
        validate_session_id(&record.id)
            .with_context(|| format!("invalid session in {}", path.display()))?;
        if records.insert(record.id, record.sort_key_ms).is_some() {
            bail!("duplicate session ID in {}", path.display());
        }
    }
    Ok(records)
}

fn write_registry(path: &Path, records: &BTreeMap<String, u64>) -> Result<()> {
    let document = OrderDocument {
        version: REGISTRY_VERSION,
        sessions: records
            .iter()
            .map(|(id, sort_key_ms)| OrderRecord {
                id: id.clone(),
                sort_key_ms: *sort_key_ms,
            })
            .collect(),
    };
    let temporary = temporary_path(path)?;
    let result = (|| {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("failed to create {}", temporary.display()))?;
        serde_json::to_writer_pretty(&mut file, &document)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        crate::fs_util::replace_file(&temporary, path)
            .with_context(|| format!("failed to replace {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_keys_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = directory.path().join("session-order.json");
        let registry = SessionOrder::load(path.clone()).unwrap();
        assert!(registry.sort_keys().is_empty());
        registry.set(&[("a".into(), 20), ("b".into(), 10)]).unwrap();
        registry.set(&[("a".into(), 5)]).unwrap();
        let reloaded = SessionOrder::load(path).unwrap().sort_keys();
        assert_eq!(reloaded.get("a"), Some(&5));
        assert_eq!(reloaded.get("b"), Some(&10));
    }
}
