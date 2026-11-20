use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::io::{Read, Write};
use std::process::{Command, Stdio};

pub fn request(
    endpoint: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&Value>,
    timeout: u64,
) -> Result<Value> {
    let base = url::Url::parse(endpoint)?;
    if base.scheme() != "https"
        || base.host_str().is_none()
        || !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
    {
        bail!("bridge endpoint must be HTTPS without credentials");
    }
    if !path.starts_with('/') || path.starts_with("//") {
        bail!("invalid bridge route");
    }
    let url = base.join(path)?;
    let mut config=format!("url = {}\nsilent\nshow-error\nmax-time = {timeout}\nconnect-timeout = 10\nproto = \"=https\"\nwrite-out = \"\\n%{{http_code}}\"\n",serde_json::to_string(url.as_str())?);
    if let Some(token) = token {
        if token.len() != 43
            || !token
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            bail!("invalid bridge credential");
        }
        config.push_str(&format!("header = \"Authorization: Bearer {token}\"\n"));
    }
    if let Some(body) = body {
        config.push_str("request = \"POST\"\nheader = \"Content-Type: application/json\"\n");
        config.push_str(&format!(
            "data-binary = {}\n",
            serde_json::to_string(&body.to_string())?
        ));
    }
    let mut child = Command::new("curl")
        .args(["--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("could not launch bridge HTTPS transport")?;
    let mut stdout = child.stdout.take().unwrap();
    let output = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    child.stdin.take().unwrap().write_all(config.as_bytes())?;
    let status = child.wait()?;
    let bytes = output
        .join()
        .map_err(|_| anyhow::anyhow!("bridge response reader failed"))??;
    if !status.success() {
        bail!("bridge network request failed");
    }
    let text = String::from_utf8(bytes).context("invalid bridge response encoding")?;
    let (json, status) = text
        .rsplit_once('\n')
        .context("missing bridge response status")?;
    let status: u16 = status.parse()?;
    if !(200..300).contains(&status) {
        // The code is a short token such as cursor_gap. Anything else stays out of the error.
        let code = serde_json::from_str::<Value>(json).ok().and_then(|v| {
            v.get("error").and_then(|e| e.as_str()).and_then(|c| {
                (c.len() <= 80 && c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_'))
                    .then(|| c.to_owned())
            })
        });
        if let Some(code) = code {
            bail!("bridge HTTP {status} {code}");
        }
        bail!("bridge HTTP {status}");
    }
    serde_json::from_str(json).context("invalid bridge JSON response")
}
