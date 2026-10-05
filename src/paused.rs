//! Sessions paused to the top of the dashboard.
//!
//! Pausing is a local display preference. It does not change the provider
//! session, and a pause for an id discovery no longer returns is simply unused.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const REGISTRY_VERSION: u32 = 1;
const MAX_SESSION_ID_BYTES: usize = 4096;
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Deserialize, Serialize)]
struct PauseDocument {
    version: u32,
    sessions: Vec<PauseRecord>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PauseRecord {
    id: String,
    #[serde(rename = "pinned_at_ms")]
    paused_at_ms: u64,
}

impl Default for PauseDocument {
    fn default() -> Self {
        Self {
            version: REGISTRY_VERSION,
            sessions: Vec::new(),
        }
    }
}

/// Cloneable pause registry shared by the dashboard.
#[derive(Clone, Debug)]
pub struct PausedSessions {
    path: PathBuf,
    records: Arc<Mutex<BTreeMap<String, u64>>>,
}

impl PausedSessions {
    pub fn load_default() -> Result<Self> {
        Self::load(default_paused_sessions_path()?)
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

    pub fn paused(&self) -> BTreeMap<String, u64> {
        self.records
            .lock()
            .expect("paused-session registry mutex poisoned")
            .clone()
    }

    /// Pause or unpause one id. `paused_at_ms` is stored when pausing.
    pub fn set(&self, session_id: &str, paused: bool) -> Result<()> {
        validate_session_id(session_id)?;
        let session_id = session_id.to_owned();
        let parent = self
            .path
            .parent()
            .context("paused-session registry path has no parent")?;
        let _lock = RegistryLock::acquire(&parent.join("pinned-sessions.lock"))?;
        let mut records = read_registry(&self.path)?;
        if paused {
            records.insert(session_id, now_millis());
        } else {
            records.remove(&session_id);
        }
        write_registry(&self.path, &records)?;
        *self
            .records
            .lock()
            .expect("paused-session registry mutex poisoned") = records;
        Ok(())
    }

    /// Overwrite `paused_at_ms` for ids that are still paused, which is what
    /// orders the Paused group. Ids paused nowhere are ignored.
    pub fn set_paused_at(&self, updates: &[(String, u64)]) -> Result<()> {
        for (session_id, _) in updates {
            validate_session_id(session_id)?;
        }
        let parent = self
            .path
            .parent()
            .context("paused-session registry path has no parent")?;
        let _lock = RegistryLock::acquire(&parent.join("pinned-sessions.lock"))?;
        let mut records = read_registry(&self.path)?;
        for (session_id, paused_at_ms) in updates {
            if let Some(record) = records.get_mut(session_id) {
                *record = *paused_at_ms;
            }
        }
        write_registry(&self.path, &records)?;
        *self
            .records
            .lock()
            .expect("paused-session registry mutex poisoned") = records;
        Ok(())
    }
}

/// Paused sessions were once called pinned; the file keeps that name so
/// existing pauses survive.
pub fn default_paused_sessions_path() -> Result<PathBuf> {
    if let Some(state_home) = crate::fs_util::xdg_home("XDG_STATE_HOME") {
        return Ok(PathBuf::from(state_home)
            .join("agentview")
            .join("pinned-sessions.json"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/state/agentview/pinned-sessions.json"))
}

pub(crate) fn validate_session_id(session_id: &str) -> Result<()> {
    if session_id.is_empty() || session_id.len() > MAX_SESSION_ID_BYTES {
        bail!("session ID must contain between 1 and {MAX_SESSION_ID_BYTES} bytes");
    }
    if session_id.chars().any(char::is_control) {
        bail!("session ID cannot contain control characters");
    }
    Ok(())
}

fn read_registry(path: &Path) -> Result<BTreeMap<String, u64>> {
    match fs::symlink_metadata(path) {
        Ok(_) => ensure_private_regular_file(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect paused sessions {}", path.display()))
        }
    }
    let input = fs::read_to_string(path)
        .with_context(|| format!("failed to read paused sessions {}", path.display()))?;
    let document: PauseDocument = serde_json::from_str(&input)
        .with_context(|| format!("invalid paused sessions registry {}", path.display()))?;
    if document.version != REGISTRY_VERSION {
        bail!(
            "unsupported paused sessions registry version {} in {}",
            document.version,
            path.display()
        );
    }
    let mut records = BTreeMap::new();
    for record in document.sessions {
        validate_session_id(&record.id)
            .with_context(|| format!("invalid paused session in {}", path.display()))?;
        if records.insert(record.id, record.paused_at_ms).is_some() {
            bail!("duplicate paused session ID in {}", path.display());
        }
    }
    Ok(records)
}

fn write_registry(path: &Path, records: &BTreeMap<String, u64>) -> Result<()> {
    let document = PauseDocument {
        version: REGISTRY_VERSION,
        sessions: records
            .iter()
            .map(|(id, paused_at_ms)| PauseRecord {
                id: id.clone(),
                paused_at_ms: *paused_at_ms,
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

pub(crate) fn ensure_private_directory(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::create_dir_all(path)
            .with_context(|| format!("failed to create private directory {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
    }
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("{} must be a real directory", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() } {
            bail!("{} is not owned by the current user", path.display());
        }
        if metadata.permissions().mode() & 0o777 != 0o700 {
            bail!("{} must have mode 0700", path.display());
        }
    }
    Ok(())
}

pub(crate) fn ensure_private_regular_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{} must be a regular file", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() } {
            bail!("{} is not owned by the current user", path.display());
        }
        if metadata.permissions().mode() & 0o777 != 0o600 {
            bail!("{} must have mode 0600", path.display());
        }
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct RegistryLock {
    file: File,
}

impl RegistryLock {
    pub(crate) fn acquire(path: &Path) -> Result<Self> {
        match fs::symlink_metadata(path) {
            Ok(_) => ensure_private_regular_file(path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("failed to inspect {}", path.display()))
            }
        }
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("failed to lock {}", path.display()));
            }
        }
        Ok(Self { file })
    }
}

impl Drop for RegistryLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

pub(crate) fn temporary_path(path: &Path) -> Result<PathBuf> {
    let name = path
        .file_name()
        .with_context(|| format!("{} has no file name", path.display()))?
        .to_string_lossy();
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok(path.with_file_name(format!(".{name}.tmp-{}-{sequence}", std::process::id())))
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private_tempdir() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        }
        directory
    }

    #[test]
    fn pause_and_unpause_round_trip() {
        let directory = private_tempdir();
        let path = directory.path().join("pinned-sessions.json");
        let registry = PausedSessions::load(path.clone()).unwrap();
        assert!(registry.paused().is_empty());
        registry.set("cursor:host:abc", true).unwrap();
        registry.set("claude:host:def", true).unwrap();
        registry.set("cursor:host:abc", false).unwrap();
        let reloaded = PausedSessions::load(path).unwrap();
        let paused = reloaded.paused();
        assert!(!paused.contains_key("cursor:host:abc"));
        assert!(paused.contains_key("claude:host:def"));
    }
}
