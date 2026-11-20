//! Local notification clicks hand an exact session to a running dashboard.
use anyhow::{bail, Result};
#[cfg(unix)]
use std::path::PathBuf;

#[cfg(unix)]
fn directory() -> Result<PathBuf> {
    Ok(crate::last_view::default_last_view_path()?.with_file_name("focus"))
}

pub fn validate(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 512 || id.chars().any(char::is_control) {
        bail!("expected a normalized session ID from agentview --json");
    }
    Ok(())
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::fs;
    use std::os::unix::{fs::PermissionsExt, net::UnixDatagram};
    use std::sync::Mutex;

    static INBOX: Mutex<Option<Inbox>> = Mutex::new(None);

    struct Inbox {
        socket: UnixDatagram,
        pending: Option<String>,
    }

    impl Inbox {
        fn read(&mut self) {
            let mut bytes = [0; 513];
            while let Ok(count) = self.socket.recv(&mut bytes) {
                if let Ok(id) = std::str::from_utf8(&bytes[..count]) {
                    if validate(id).is_ok() {
                        self.pending = Some(id.to_owned());
                    }
                }
            }
        }
    }

    pub struct Registration(PathBuf);
    impl Drop for Registration {
        fn drop(&mut self) {
            *INBOX.lock().unwrap() = None;
            let _ = fs::remove_file(&self.0);
        }
    }

    pub fn register() -> Result<Registration> {
        let dir = directory()?;
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        let path = dir.join(format!("{}.sock", std::process::id()));
        let socket = UnixDatagram::bind(&path)?;
        socket.set_nonblocking(true)?;
        *INBOX.lock().unwrap() = Some(Inbox {
            socket,
            pending: None,
        });
        Ok(Registration(path))
    }

    pub fn pending() -> bool {
        let mut inbox = INBOX.lock().unwrap();
        if let Some(inbox) = inbox.as_mut() {
            inbox.read();
            return inbox.pending.is_some();
        }
        false
    }

    pub fn take() -> Option<String> {
        let mut inbox = INBOX.lock().unwrap();
        let inbox = inbox.as_mut()?;
        inbox.read();
        inbox.pending.take()
    }

    pub fn send(id: &str) -> Result<()> {
        validate(id)?;
        let entries = match fs::read_dir(directory()?) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                bail!("no running BetterAgentView dashboard; start av first")
            }
            Err(error) => return Err(error.into()),
        };
        let mut paths: Vec<_> = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "sock"))
            .collect();
        paths.sort_by_key(|entry| {
            std::cmp::Reverse(entry.metadata().and_then(|m| m.modified()).ok())
        });
        let socket = UnixDatagram::unbound()?;
        for entry in paths {
            if socket.send_to(id.as_bytes(), entry.path()).is_ok() {
                #[cfg(target_os = "macos")]
                activate_terminal(&entry.path());
                return Ok(());
            }
        }
        bail!("no running BetterAgentView dashboard; start av first")
    }

    #[cfg(target_os = "macos")]
    fn activate_terminal(path: &std::path::Path) {
        // The server's TERM_PROGRAM may name a different terminal. Walk the
        // actual dashboard's parents instead, without opening a new window.
        let Some(mut pid) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u32>().ok())
        else {
            return;
        };
        for _ in 0..12 {
            let Ok(output) = std::process::Command::new("/bin/ps")
                .args(["-p", &pid.to_string(), "-o", "ppid=,comm="])
                .output()
            else {
                return;
            };
            let text = String::from_utf8_lossy(&output.stdout);
            let bundle = if text.contains("/Ghostty.app/") {
                Some("com.mitchellh.ghostty")
            } else if text.contains("/Terminal.app/") {
                Some("com.apple.Terminal")
            } else if text.contains("/iTerm.app/") {
                Some("com.googlecode.iterm2")
            } else {
                None
            };
            if let Some(bundle) = bundle {
                let _ = std::process::Command::new("/usr/bin/open")
                    .args(["-b", bundle])
                    .status();
                return;
            }
            let Some(parent) = text
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<u32>().ok())
            else {
                return;
            };
            if parent <= 1 || parent == pid {
                return;
            }
            pid = parent;
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn click_survives_native_detach_and_is_consumed_once() {
            let dir = crate::test_support::tempfile::tempdir().unwrap();
            let path = dir.path().join("focus.sock");
            let socket = UnixDatagram::bind(&path).unwrap();
            socket.set_nonblocking(true).unwrap();
            let sender = UnixDatagram::unbound().unwrap();
            let mut inbox = Inbox {
                socket,
                pending: None,
            };
            sender.send_to(b"opencode:host:ses_test", &path).unwrap();
            inbox.read();
            inbox.read();
            assert_eq!(
                inbox.pending.take().as_deref(),
                Some("opencode:host:ses_test")
            );
            inbox.read();
            assert!(inbox.pending.is_none());
            sender.send_to(b"bad\nrequest", &path).unwrap();
            inbox.read();
            assert!(inbox.pending.is_none());
        }
    }
}

#[cfg(unix)]
pub use platform::*;

#[cfg(not(unix))]
pub fn register() -> Result<()> {
    Ok(())
}
#[cfg(not(unix))]
pub fn pending() -> bool {
    false
}
#[cfg(not(unix))]
pub fn take() -> Option<String> {
    None
}
#[cfg(not(unix))]
pub fn send(_id: &str) -> Result<()> {
    bail!("notification navigation is not supported on this platform")
}
