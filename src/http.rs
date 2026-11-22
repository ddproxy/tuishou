use crate::error::{Error, Result};
use std::io::Read;
use std::thread;
use std::time::Duration;

pub const DEFAULT_TIMEOUT_SECS: u64 = 30;
pub const MAX_REDIRECTS: u32 = 5;
pub const DEFAULT_MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;
pub const MAX_RATE_LIMIT_SLEEP_SECS: u64 = 30;

pub fn max_download_bytes() -> u64 {
    std::env::var("TUISHOU_MAX_DOWNLOAD_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MAX_DOWNLOAD_BYTES)
}

/// Redirects are followed manually in `get_response` so that only GitHub and
/// its release-asset hosts are ever contacted.
pub fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .redirects(0)
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

fn auth_headers() -> Vec<(&'static str, String)> {
    match github_token() {
        Some(token) => vec![("Authorization", format!("Bearer {token}"))],
        None => Vec::new(),
    }
}

#[derive(Debug)]
pub struct ConditionalJson {
    pub not_modified: bool,
    pub etag: Option<String>,
    pub value: Option<serde_json::Value>,
}

/// GET a JSON document, optionally sending `If-None-Match`. A 304 is reported
/// as `not_modified`. HTTP 403/429 responses that carry `Retry-After` are
/// retried (once by default via `attempts`) after the requested delay.
pub fn get_json_conditional(
    agent: &ureq::Agent,
    url: &str,
    etag: Option<&str>,
    attempts: usize,
) -> Result<ConditionalJson> {
    let mut headers: Vec<(&str, String)> = vec![
        ("Accept", "application/vnd.github+json".to_string()),
        ("User-Agent", "tuishou".to_string()),
    ];
    headers.extend(auth_headers());
    if let Some(tag) = etag {
        headers.push(("If-None-Match", tag.to_string()));
    }
    for attempt in 0..attempts.max(1) {
        let resp = get_response(agent, url, &headers)?;
        let status = resp.status();
        if status == 304 {
            return Ok(ConditionalJson {
                not_modified: true,
                etag: etag.map(str::to_string),
                value: None,
            });
        }
        if status == 403 || status == 429 {
            if let Some(delay) = retry_after(resp.header("Retry-After")) {
                if attempt + 1 < attempts.max(1) {
                    journal_rate_limit(url, delay);
                    thread::sleep(Duration::from_secs(delay));
                    continue;
                }
                return Err(Error::Http(format!(
                    "rate limited by {url}; retry after {delay}s"
                )));
            }
            return Err(status_error("GET", url, resp));
        }
        if !(200..300).contains(&status) {
            return Err(status_error("GET", url, resp));
        }
        let response_etag = resp
            .header("ETag")
            .or_else(|| resp.header("etag"))
            .map(str::to_string);
        let body = read_body(resp, DEFAULT_MAX_DOWNLOAD_BYTES)?;
        let value = serde_json::from_str(&body)
            .map_err(|e| Error::Http(format!("invalid JSON from {url}: {e}")))?;
        return Ok(ConditionalJson {
            not_modified: false,
            etag: response_etag.or_else(|| etag.map(str::to_string)),
            value: Some(value),
        });
    }
    Err(Error::Http(format!("rate limited by {url}")))
}

fn journal_rate_limit(url: &str, delay: u64) {
    crate::logging::journal(
        "rate_limited",
        &[
            ("url", &crate::logging::one_line(url)),
            ("retry_after", &delay.to_string()),
        ],
    );
}

pub fn get_json(agent: &ureq::Agent, url: &str) -> Result<serde_json::Value> {
    let mut headers: Vec<(&str, String)> = vec![
        ("Accept", "application/vnd.github+json".to_string()),
        ("User-Agent", "tuishou".to_string()),
    ];
    headers.extend(auth_headers());
    let resp = get_response(agent, url, &headers)?;
    if !(200..300).contains(&resp.status()) {
        return Err(status_error("GET", url, resp));
    }
    let body = read_body(resp, DEFAULT_MAX_DOWNLOAD_BYTES)?;
    serde_json::from_str(&body).map_err(|e| Error::Http(format!("invalid JSON from {url}: {e}")))
}

pub fn download_to_writer(
    agent: &ureq::Agent,
    url: &str,
    mut writer: impl std::io::Write,
    max_bytes: u64,
) -> Result<u64> {
    let mut headers: Vec<(&str, String)> = vec![("User-Agent", "tuishou".to_string())];
    headers.extend(auth_headers());
    let resp = get_response(agent, url, &headers)?;
    if !(200..300).contains(&resp.status()) {
        return Err(status_error("GET", url, resp));
    }
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

/// Issue a GET, following redirects only to GitHub or its release asset hosts.
/// Any HTTP status is returned to the caller as a successful `Result`; only
/// transport errors and forbidden redirects produce an `Err`.
pub fn get_response(
    agent: &ureq::Agent,
    url: &str,
    headers: &[(&str, String)],
) -> Result<ureq::Response> {
    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        ensure_allowed_url(&current)?;
        let mut req = agent.get(&current).set("User-Agent", "tuishou");
        for (name, value) in headers {
            req = req.set(name, value.as_str());
        }
        match req.call() {
            Ok(resp) if is_redirect(resp.status()) => {
                current = follow_location(&current, &resp)?;
            }
            Ok(resp) => return Ok(resp),
            Err(ureq::Error::Status(code, resp)) if is_redirect(code) => {
                current = follow_location(&current, &resp)?;
            }
            Err(ureq::Error::Status(_, resp)) => return Ok(resp),
            Err(other) => return Err(map_ureq_err("GET", &current, other)),
        }
    }
    Err(Error::Http(format!(
        "too many redirects while fetching {url}"
    )))
}

fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn follow_location(base: &str, resp: &ureq::Response) -> Result<String> {
    let location = resp
        .header("Location")
        .or_else(|| resp.header("location"))
        .ok_or_else(|| Error::Http(format!("redirect from {base} has no Location header")))?;
    resolve_location(base, location)
}

/// Resolve an absolute or relative `Location` against `base` without a URL
/// crate (only the http(s) scheme is supported here).
fn resolve_location(base: &str, location: &str) -> Result<String> {
    let (scheme, rest) = base
        .split_once("://")
        .ok_or_else(|| Error::Http(format!("unsupported URL {base}")))?;
    let authority = rest.split('/').next().unwrap_or(rest);
    let prefix = format!("{scheme}://{authority}");
    if location.starts_with("http://") || location.starts_with("https://") {
        return Ok(location.to_string());
    }
    if let Some(path) = location.strip_prefix('/') {
        return Ok(format!("{prefix}/{path}"));
    }
    let base_path = rest.strip_prefix(authority).unwrap_or("");
    let directory = base_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    Ok(format!("{prefix}{directory}/{location}"))
}

pub fn ensure_https(url: &str) -> Result<()> {
    if url.starts_with("https://") {
        return Ok(());
    }
    if allow_insecure_http()
        && (url.starts_with("http://127.0.0.1:")
            || url.starts_with("http://localhost:")
            || url.starts_with("http://[::1]:"))
    {
        return Ok(());
    }
    Err(Error::Http(format!("only https URLs are allowed: {url}")))
}

/// Host allowlist for redirects. Loopback is permitted only for local tests
/// via `TUISHOU_ALLOW_INSECURE_HTTP=1`.
fn ensure_allowed_url(url: &str) -> Result<()> {
    ensure_https(url)?;
    let rest = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .ok_or_else(|| Error::Http(format!("invalid URL {url}")))?;
    let host_with_port = rest.split('/').next().unwrap_or(rest);
    let host = host_with_port
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(host_with_port)
        .split(':')
        .next()
        .unwrap_or(host_with_port)
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    let extra = std::env::var("TUISHOU_ALLOWED_HOSTS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .any(|s| s == host);
    let github = matches!(
        host.as_str(),
        "github.com"
            | "api.github.com"
            | "objects.githubusercontent.com"
            | "release-assets.githubusercontent.com"
            | "github-releases.githubusercontent.com"
    ) || host.ends_with(".githubusercontent.com");
    let loopback =
        allow_insecure_http() && matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1");
    if github || extra || loopback {
        Ok(())
    } else {
        Err(Error::Http(format!(
            "refusing redirect to host outside github.com and its release asset hosts: {host}"
        )))
    }
}

fn allow_insecure_http() -> bool {
    std::env::var("TUISHOU_ALLOW_INSECURE_HTTP").ok().as_deref() == Some("1")
}

pub fn retry_after(value: Option<&str>) -> Option<u64> {
    value
        .map(str::trim)
        .and_then(|v| v.parse::<u64>().ok())
        .map(|n| n.min(MAX_RATE_LIMIT_SLEEP_SECS))
}

fn read_body(resp: ureq::Response, max_bytes: u64) -> Result<String> {
    let mut body = String::new();
    resp.into_reader()
        .take(max_bytes + 1)
        .read_to_string(&mut body)
        .map_err(|e| Error::Http(format!("failed to read response body: {e}")))?;
    if body.len() as u64 > max_bytes {
        return Err(Error::DownloadTooLarge { limit: max_bytes });
    }
    Ok(body)
}

fn status_error(method: &str, url: &str, resp: ureq::Response) -> Error {
    let code = resp.status();
    let body = resp.into_string().unwrap_or_default();
    let body = crate::logging::one_line(body.trim());
    if body.is_empty() {
        Error::Http(format!("{method} {url} failed with status {code}"))
    } else {
        Error::Http(format!("{method} {url} failed with status {code}: {body}"))
    }
}

fn map_ureq_err(method: &str, url: &str, err: ureq::Error) -> Error {
    match err {
        ureq::Error::Status(_, resp) => status_error(method, url, resp),
        other => Error::Http(format!("{method} {url} failed: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_header_parsing() {
        assert_eq!(retry_after(Some("7")), Some(7));
        assert_eq!(retry_after(Some("Wed, 21 Oct 2015 07:28:00 GMT")), None);
        assert_eq!(retry_after(Some("999")), Some(MAX_RATE_LIMIT_SLEEP_SECS));
    }

    #[test]
    fn redirect_location_resolution() {
        assert_eq!(
            resolve_location("https://github.com/a/b", "/c").unwrap(),
            "https://github.com/c"
        );
        assert_eq!(
            resolve_location("https://github.com/a/b", "c").unwrap(),
            "https://github.com/a/c"
        );
        assert_eq!(
            resolve_location("https://github.com/a/b", "https://evil.test/x").unwrap(),
            "https://evil.test/x"
        );
    }

    #[test]
    fn non_github_hosts_are_refused() {
        assert!(ensure_allowed_url("https://github.com/ddproxy/tuishou").is_ok());
        assert!(ensure_allowed_url("https://evil.test/x").is_err());
        assert!(ensure_allowed_url("http://127.0.0.1:8080/x").is_err());
    }
}
