use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde::Serialize;

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// An XDG base directory variable such as `XDG_STATE_HOME`. The spec says an
/// empty or relative value is invalid and must be ignored; using it would put
/// state under whatever directory agentview happened to start in.
pub(crate) fn xdg_home(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name).filter(|value| Path::new(value).is_absolute())
}

/// Write a small owner-only JSON state file through a temporary sibling so a
/// crash never leaves it truncated. A missing parent is created owner-only.
pub(crate) fn write_private_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent", path.display()))?;
    if !parent.exists() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
    }
    let name = path
        .file_name()
        .with_context(|| format!("{} has no file name", path.display()))?
        .to_string_lossy();
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_file_name(format!(".{name}.tmp-{}-{sequence}", std::process::id()));
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
        serde_json::to_writer_pretty(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        replace_file(&temporary, path)
            .with_context(|| format!("failed to replace {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Replace a persisted state file without exposing a missing-destination
/// window. Unix `rename` already replaces atomically. Windows requires the
/// explicit Win32 replacement flag when the destination exists.
#[cfg(not(windows))]
pub(crate) fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(windows)]
pub(crate) fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::iter;
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{replace_file, xdg_home};

    #[test]
    fn xdg_home_ignores_empty_and_relative_values() {
        std::env::set_var("AGENTVIEW_TEST_XDG_EMPTY", "");
        std::env::set_var("AGENTVIEW_TEST_XDG_RELATIVE", "state");
        let absolute = std::env::temp_dir().join("state");
        std::env::set_var("AGENTVIEW_TEST_XDG_ABSOLUTE", &absolute);

        assert_eq!(xdg_home("AGENTVIEW_TEST_XDG_EMPTY"), None);
        assert_eq!(xdg_home("AGENTVIEW_TEST_XDG_RELATIVE"), None);
        assert_eq!(xdg_home("AGENTVIEW_TEST_XDG_UNSET"), None);
        assert_eq!(
            xdg_home("AGENTVIEW_TEST_XDG_ABSOLUTE"),
            Some(absolute.into_os_string())
        );
    }

    #[test]
    fn replaces_an_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("state.new");
        let destination = directory.path().join("state.json");
        fs::write(&source, b"new state").unwrap();
        fs::write(&destination, b"old state").unwrap();

        replace_file(&source, &destination).unwrap();

        assert_eq!(fs::read(&destination).unwrap(), b"new state");
        assert!(!source.exists());
    }
}
