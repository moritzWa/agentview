use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(any(target_os = "macos", test))]
const IMAGE_EXTENSIONS: [&str; 6] = ["png", "jpg", "jpeg", "gif", "webp", "heic"];

pub fn default_image_dir() -> Result<PathBuf> {
    if let Some(state_home) = std::env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(state_home).join("agentview").join("images"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/state/agentview/images"))
}

/// Save the clipboard image under `dir` and return its path. A copied image
/// file is used in place. `Ok(None)` means the clipboard holds no image.
pub fn save_clipboard_image(dir: &Path) -> Result<Option<PathBuf>> {
    if let Some(path) = copied_image_file() {
        return Ok(Some(path));
    }
    std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let path = dir.join(format!("paste-{stamp}.png"));
    if !write_clipboard_png(&path)? {
        let _ = std::fs::remove_file(&path);
        return Ok(None);
    }
    if std::fs::metadata(&path).map_or(true, |metadata| metadata.len() == 0) {
        let _ = std::fs::remove_file(&path);
        return Ok(None);
    }
    Ok(Some(path))
}

#[cfg(target_os = "macos")]
fn write_clipboard_png(path: &Path) -> Result<bool> {
    // AppleScript coerces TIFF and other bitmap flavors to PNG, so browser
    // copies and screenshots both arrive here.
    let script = format!(
        "set f to open for access (POSIX file {}) with write permission\n\
         try\n\
         write (the clipboard as «class PNGf») to f\n\
         close access f\n\
         on error message\n\
         close access f\n\
         error message\n\
         end try",
        applescript_string(&path.display().to_string())
    );
    let status = Command::new("osascript")
        .args(["-e", &script])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to run osascript")?;
    Ok(status.success())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn write_clipboard_png(path: &Path) -> Result<bool> {
    let candidates: [(&str, &[&str]); 2] = [
        ("wl-paste", &["--no-newline", "--type", "image/png"]),
        (
            "xclip",
            &["-selection", "clipboard", "-target", "image/png", "-out"],
        ),
    ];
    let mut found_tool = false;
    for (program, args) in candidates {
        let Ok(output) = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        else {
            continue;
        };
        found_tool = true;
        if output.status.success() && output.stdout.starts_with(b"\x89PNG") {
            std::fs::write(path, &output.stdout)
                .with_context(|| format!("failed to write {}", path.display()))?;
            return Ok(true);
        }
    }
    if !found_tool {
        anyhow::bail!("pasting images needs wl-paste or xclip");
    }
    Ok(false)
}

#[cfg(not(unix))]
fn write_clipboard_png(_path: &Path) -> Result<bool> {
    anyhow::bail!("pasting images is not supported on this platform yet")
}

/// A file copied in Finder is a file URL, not image data.
#[cfg(target_os = "macos")]
fn copied_image_file() -> Option<PathBuf> {
    let output = Command::new("osascript")
        .args(["-e", "POSIX path of (the clipboard as «class furl»)"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = PathBuf::from(
        String::from_utf8(output.stdout)
            .ok()?
            .trim_end_matches('\n'),
    );
    (is_image_path(&path) && path.is_file()).then_some(path)
}

#[cfg(not(target_os = "macos"))]
fn copied_image_file() -> Option<PathBuf> {
    None
}

#[cfg(any(target_os = "macos", test))]
fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            IMAGE_EXTENSIONS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

#[cfg(target_os = "macos")]
fn applescript_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_paths_are_recognized_by_extension() {
        assert!(is_image_path(Path::new("/tmp/Shot 1.PNG")));
        assert!(is_image_path(Path::new("photo.jpeg")));
        assert!(!is_image_path(Path::new("notes.txt")));
        assert!(!is_image_path(Path::new("png")));
    }
}
