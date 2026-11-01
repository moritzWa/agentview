//! The browser that holds the microphone and speaker. By default agentview runs
//! one itself, hidden: a Chromium-based browser in headless mode, started when a
//! call starts and stopped when it ends. No tab or window appears; the browser
//! brings echo cancellation and noise suppression with it.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// How the voice page gets a browser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A hidden browser agentview starts and stops.
    Hidden,
    /// A tab in the default browser, which the person pins.
    Tab,
    /// Nothing: a page is opened some other way.
    None,
}

impl Mode {
    pub fn parse(text: &str) -> Self {
        match text.trim().to_ascii_lowercase().as_str() {
            "tab" => Self::Tab,
            "none" => Self::None,
            _ => Self::Hidden,
        }
    }
}

/// The browser to run, from the setting or the usual places.
pub fn find_chrome(configured: Option<&str>) -> Option<PathBuf> {
    if let Some(path) = configured.map(PathBuf::from).filter(|path| path.is_file()) {
        return Some(path);
    }
    let fixed: &[&str] = if cfg!(target_os = "macos") {
        &[
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ]
    } else if cfg!(windows) {
        &[
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe",
        ]
    } else {
        &[]
    };
    if let Some(found) = fixed.iter().map(PathBuf::from).find(|path| path.is_file()) {
        return Some(found);
    }
    let names = [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "microsoft-edge",
        "brave-browser",
    ];
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
}

/// Starts the hidden browser on `url`. `extra` is whitespace-separated flags,
/// which tests use to bring a synthesized microphone.
pub fn spawn_hidden(chrome: &Path, url: &str, profile: &Path, extra: &str) -> io::Result<Child> {
    std::fs::create_dir_all(profile)?;
    let mut command = Command::new(chrome);
    command
        .arg("--headless=new")
        .arg(format!("--user-data-dir={}", profile.display()))
        // The page asks for the microphone; the operating system still asks the
        // person once, for the browser or the terminal.
        .arg("--use-fake-ui-for-media-stream")
        .arg("--autoplay-policy=no-user-gesture-required")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-extensions")
        .arg("--disable-sync")
        .arg("--disable-background-networking")
        .arg("--disable-component-update")
        .arg("--noerrdialogs")
        .args(extra.split_whitespace())
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own group, so the whole browser can be stopped at once.
        command.process_group(0);
    }
    command.spawn()
}

/// Stops hidden browsers an earlier agentview left running (it was killed, or
/// crashed, before it could stop its own): they keep the profile locked and the
/// microphone open. Only browsers whose parent is gone are touched.
pub fn stop_orphans(profile: &Path) {
    #[cfg(unix)]
    {
        let marker = format!("--user-data-dir={}", profile.display());
        let Ok(output) = Command::new("ps")
            .args(["-A", "-o", "pid=,ppid=,command="])
            .output()
        else {
            return;
        };
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let mut parts = line.split_whitespace();
            let (Some(pid), Some(parent)) = (parts.next(), parts.next()) else {
                continue;
            };
            if parent == "1" && line.contains(&marker) && line.contains("--headless") {
                if let Ok(pid) = pid.parse::<libc::pid_t>() {
                    unsafe {
                        libc::kill(pid, libc::SIGTERM);
                    }
                }
            }
        }
    }
    #[cfg(not(unix))]
    let _ = profile;
}

/// Stops the browser and everything it started.
pub fn stop(mut child: Child) {
    #[cfg(unix)]
    {
        let group = child.id() as libc::pid_t;
        if group > 1 {
            unsafe {
                libc::kill(-group, libc::SIGTERM);
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mode_setting_reads_loosely_and_defaults_to_hidden() {
        assert_eq!(Mode::parse("tab"), Mode::Tab);
        assert_eq!(Mode::parse(" NONE "), Mode::None);
        assert_eq!(Mode::parse("hidden"), Mode::Hidden);
        assert_eq!(Mode::parse("anything else"), Mode::Hidden);
    }

    #[test]
    fn a_configured_browser_must_exist() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("browser");
        std::fs::write(&fake, b"").unwrap();
        assert_eq!(find_chrome(fake.to_str()), Some(fake));
    }

    #[cfg(unix)]
    #[test]
    fn stopping_ends_the_process() {
        let child = Command::new("sleep")
            .arg("30")
            .process_group_for_test()
            .spawn()
            .unwrap();
        let pid = child.id();
        stop(child);
        let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
        assert!(!alive);
    }

    #[cfg(unix)]
    trait GroupForTest {
        fn process_group_for_test(&mut self) -> &mut Self;
    }

    #[cfg(unix)]
    impl GroupForTest for Command {
        fn process_group_for_test(&mut self) -> &mut Self {
            use std::os::unix::process::CommandExt;
            self.process_group(0)
        }
    }
}
