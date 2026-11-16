//! A shared display scope, controllable even when a native TUI owns stdin.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

static WATCH_PATH: OnceLock<PathBuf> = OnceLock::new();
static APPLIED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug)]
pub struct WorkViewStore(pub PathBuf);

#[derive(Default, Deserialize, Serialize)]
struct State {
    work_only: bool,
}

impl WorkViewStore {
    pub fn load_default() -> Result<Self> {
        let base = crate::fs_util::xdg_home("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
            .context("HOME is not set")?;
        Ok(Self(base.join("agentview/work-view.json")))
    }

    pub fn read(&self) -> Result<bool> {
        match fs::read(&self.0) {
            Ok(bytes) => Ok(serde_json::from_slice::<State>(&bytes)?.work_only),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    pub fn set(&self, work_only: bool) -> Result<()> {
        crate::fs_util::write_private_json(&self.0, &State { work_only })
    }

    /// Serialize concurrent global-hotkey invocations on Unix. The lock is
    /// released by the kernel even if a caller crashes.
    pub fn toggle(&self) -> Result<bool> {
        let parent = self.0.parent().context("state path has no parent")?;
        if !parent.exists() {
            fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
            }
        }
        let mut options = fs::OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(self.0.with_extension("lock"))?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        let enabled = !self.read()?;
        self.set(enabled)?;
        drop(lock);
        Ok(enabled)
    }

    pub fn watch_native(&self, enabled: bool) {
        let _ = WATCH_PATH.set(self.0.clone());
        APPLIED.store(enabled, Ordering::SeqCst);
    }
}

pub(crate) fn native_scope_changed() -> bool {
    WATCH_PATH
        .get()
        .and_then(|p| WorkViewStore(p.clone()).read().ok())
        .is_some_and(|enabled| enabled != APPLIED.load(Ordering::SeqCst))
}

/// Exact directory roots, not substring matches. A linked worktree belongs to
/// the root that owns its Git common directory, even outside that root.
#[derive(Clone, Debug, Default)]
pub struct WorkRoots {
    roots: Vec<PathBuf>,
    cache: RefCell<BTreeMap<PathBuf, bool>>,
}

#[derive(Deserialize)]
struct RootsConfig {
    roots: Vec<PathBuf>,
}

impl WorkRoots {
    pub fn load_default() -> Result<Self> {
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
        let config = crate::fs_util::xdg_home("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("agentview/work-roots.json");
        let roots = match fs::read(config) {
            Ok(bytes) => serde_json::from_slice::<RootsConfig>(&bytes)?.roots,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        Ok(Self::new(roots))
    }

    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            roots: roots
                .into_iter()
                .filter(|r| r.is_absolute())
                .map(|r| fs::canonicalize(&r).unwrap_or(r))
                .collect(),
            cache: RefCell::default(),
        }
    }

    pub fn contains(&self, cwd: &Path) -> bool {
        if let Some(found) = self.cache.borrow().get(cwd) {
            return *found;
        }
        let found = self.resolve(cwd);
        self.cache.borrow_mut().insert(cwd.to_owned(), found);
        found
    }

    fn resolve(&self, cwd: &Path) -> bool {
        let Ok(cwd) = fs::canonicalize(cwd) else {
            return false;
        };
        if self.roots.iter().any(|root| cwd.starts_with(root)) {
            return true;
        }
        for ancestor in cwd.ancestors() {
            let git = ancestor.join(".git");
            if git.is_dir() {
                return false;
            }
            if let Ok(pointer) = fs::read_to_string(&git) {
                let Some(target) = pointer.trim().strip_prefix("gitdir: ") else {
                    return false;
                };
                let gitdir = ancestor.join(target);
                let Ok(common) = fs::read_to_string(gitdir.join("commondir")) else {
                    return false;
                };
                let Ok(common) = fs::canonicalize(gitdir.join(common.trim())) else {
                    return false;
                };
                return self.roots.iter().any(|root| {
                    fs::canonicalize(root.join(".git")).ok().as_ref() == Some(&common)
                });
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_toggle_and_corrupt_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkViewStore(dir.path().join("state/work.json"));
        assert!(!store.read().unwrap());
        assert!(store.toggle().unwrap());
        assert!(WorkViewStore(store.0.clone()).read().unwrap());
        assert!(!store.toggle().unwrap());
        fs::write(&store.0, "broken").unwrap();
        assert!(store.read().is_err());
        assert!(store.toggle().is_err());
    }

    #[test]
    fn exact_roots_subfolders_and_external_worktrees() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("arca");
        let other = dir.path().join("personal-arca-notes");
        let wt = dir.path().join("external-worktree");
        fs::create_dir_all(root.join(".git/worktrees/wt")).unwrap();
        fs::create_dir_all(root.join("apps/api")).unwrap();
        fs::create_dir_all(&other).unwrap();
        fs::create_dir_all(wt.join("apps")).unwrap();
        fs::write(
            wt.join(".git"),
            format!("gitdir: {}", root.join(".git/worktrees/wt").display()),
        )
        .unwrap();
        fs::write(root.join(".git/worktrees/wt/commondir"), "../..\n").unwrap();
        let roots = WorkRoots::new(vec![root.clone()]);
        assert!(roots.contains(&root.join("apps/api")));
        assert!(roots.contains(&wt.join("apps")));
        assert!(!roots.contains(&other));
        assert!(!roots.contains(&dir.path().join("missing")));
    }
}
