use crate::error::{Error, Result};
use std::io::Read;
use std::time::Duration;

pub const DEFAULT_TIMEOUT_SECS: u64 = 30;
pub const MAX_REDIRECTS: u32 = 5;
pub const DEFAULT_MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;

pub fn max_download_bytes() -> u64 {
    std::env::var("TUISHOU_MAX_DOWNLOAD_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MAX_DOWNLOAD_BYTES)
}

pub fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .redirects(MAX_REDIRECTS)
        .build()
}

pub fn github_token() -> Option<String> {
    for key in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(v) = std::env::var(key) {
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

pub fn get_json(agent: &ureq::Agent, url: &str) -> Result<serde_json::Value> {
    ensure_https(url)?;
    let mut req = agent
        .get(url)
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", "tuishou");
    if let Some(token) = github_token() {
        req = req.set("Authorization", &format!("Bearer {token}"));
    }
    let resp = req.call().map_err(|e| map_ureq_err("GET", url, e))?;
    let mut body = String::new();
    resp.into_reader()
        .take(DEFAULT_MAX_DOWNLOAD_BYTES + 1)
        .read_to_string(&mut body)
        .map_err(|e| Error::Http(format!("failed to read {url}: {e}")))?;
    serde_json::from_str(&body).map_err(|e| Error::Http(format!("invalid JSON from {url}: {e}")))
}

pub fn download_to_writer(
    agent: &ureq::Agent,
    url: &str,
    mut writer: impl std::io::Write,
    max_bytes: u64,
) -> Result<u64> {
    ensure_https(url)?;
    let mut req = agent.get(url).set("User-Agent", "tuishou");
    if let Some(token) = github_token() {
        req = req.set("Authorization", &format!("Bearer {token}"));
    }
    let resp = req.call().map_err(|e| map_ureq_err("GET", url, e))?;
    let mut reader = resp.into_reader().take(max_bytes + 1);
    let mut buf = [0u8; 16384];
    let mut total: u64 = 0;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| Error::Http(format!("failed to read {url}: {e}")))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > max_bytes {
            return Err(Error::DownloadTooLarge { limit: max_bytes });
        }
        writer
            .write_all(&buf[..n])
            .map_err(|e| Error::Http(format!("failed to write download: {e}")))?;
    }
    Ok(total)
}

pub fn ensure_https(url: &str) -> Result<()> {
    if url.starts_with("https://") {
        return Ok(());
    }
    // Allow http only for loopback in tests via env override
    if allow_insecure_http()
        && (url.starts_with("http://127.0.0.1:")
            || url.starts_with("http://localhost:")
            || url.starts_with("http://[::1]:"))
    {
        return Ok(());
    }
    Err(Error::Http(format!("only https URLs are allowed: {url}")))
}

fn allow_insecure_http() -> bool {
    std::env::var("TUISHOU_ALLOW_INSECURE_HTTP").ok().as_deref() == Some("1")
}

fn map_ureq_err(method: &str, url: &str, err: ureq::Error) -> Error {
    match err {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let body = body.trim();
            if body.is_empty() {
                Error::Http(format!("{method} {url} failed with status {code}"))
            } else {
                Error::Http(format!("{method} {url} failed with status {code}: {body}"))
            }
        }
        other => Error::Http(format!("{method} {url} failed: {other}")),
    }
}
