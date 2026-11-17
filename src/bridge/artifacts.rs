//! Explicit file publication only. History sync never discovers or uploads files.
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::Read;
use std::path::Path;

const MAX_BYTES: u64 = 512_000;

fn encode(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        out.push(TABLE[(a >> 2) as usize] as char);
        out.push(TABLE[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(((b & 15) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn payload(session_id: &str, path: &Path, title: Option<&str>) -> Result<Value> {
    if session_id.is_empty() || session_id.len() > 200 {
        bail!("an exact session id is required");
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .context("file needs a UTF-8 name")?;
    if name.chars().count() > 160 || name.chars().any(|c| c.is_control() || c == '\\') {
        bail!("artifact filename is invalid");
    }
    let title = title.unwrap_or(name).trim();
    if title.is_empty() || title.chars().count() > 160 {
        bail!("artifact title must contain 1 to 160 characters");
    }
    let mime = match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html",
        "txt" | "md" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        _ => bail!("supported artifact files: HTML, text, PNG, JPEG, PDF and ZIP"),
    };
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        bail!("publish an explicit regular file, not a directory or symlink");
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        bail!("artifact is not a regular file");
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_BYTES {
        bail!("artifact must contain 1 to 512000 bytes; nothing uploaded");
    }
    if mime.starts_with("text/") {
        std::str::from_utf8(&bytes).context("text artifacts must be UTF-8; nothing uploaded")?;
    }
    let mut value = json!({"session_id":session_id,"title":title,"name":name,"mime_type":mime,"blob":encode(&bytes),"confirmed":true});
    // The same exact approved revision is retryable without duplicate uploads.
    value["artifact_id"] = Value::String(super::hash(&value)[..32].to_owned());
    Ok(value)
}

pub fn publish(
    dir: &Path,
    session_id: &str,
    path: &Path,
    title: Option<&str>,
    confirmed: bool,
) -> Result<Value> {
    if !confirmed {
        bail!("publishing uploads this file to your private bridge and ChatGPT; approve this exact file, then pass --confirmed");
    }
    let value = payload(session_id, path, title)?;
    let config = super::load(dir)?;
    let credential = super::token(&config)?;
    super::http::request(
        &config.endpoint,
        "/v1/artifacts",
        Some(&credential),
        Some(&value),
        60,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_file_is_stable_and_requires_approval() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("index.html");
        std::fs::write(&file, b"<h1>fixture</h1>").unwrap();
        let a = payload("ses_fixture", &file, None).unwrap();
        assert_eq!(a, payload("ses_fixture", &file, None).unwrap());
        assert_eq!(a["blob"], "PGgxPmZpeHR1cmU8L2gxPg==");
        assert_eq!(a["mime_type"], "text/html");
        assert!(publish(dir.path(), "ses_fixture", &file, None, false)
            .unwrap_err()
            .to_string()
            .contains("--confirmed"));
        std::fs::write(&file, b"changed").unwrap();
        assert_ne!(
            a["artifact_id"],
            payload("ses_fixture", &file, None).unwrap()["artifact_id"]
        );
    }
    #[test]
    fn refuses_directories_empty_oversize_and_symlink_files() {
        let dir = tempfile::tempdir().unwrap();
        assert!(payload("ses_fixture", dir.path(), None).is_err());
        let file = dir.path().join("fixture.html");
        std::fs::write(&file, b"").unwrap();
        assert!(payload("ses_fixture", &file, None).is_err());
        std::fs::write(&file, vec![0; MAX_BYTES as usize + 1]).unwrap();
        assert!(payload("ses_fixture", &file, None).is_err());
        #[cfg(unix)]
        {
            let link = dir.path().join("link.html");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            assert!(payload("ses_fixture", &link, None).is_err());
        }
    }
    #[test]
    fn base64_padding_is_canonical() {
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
    }
}
