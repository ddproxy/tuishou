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

/// GitHub tokens are only sent to GitHub API endpoints. Asset hosts and hosts
/// added through the test-only `TUISHOU_ALLOWED_HOSTS` never receive credentials.
pub fn auth_headers_for(url: &str) -> Vec<(&'static str, String)> {
    if !is_github_api(url) {
        return Vec::new();
    }
    match github_token() {
        Some(token) => vec![("Authorization", format!("Bearer {token}"))],
        None => Vec::new(),
    }
}

fn is_github_api(url: &str) -> bool {
    let Ok((_, host, _, path)) = url_parts(url) else {
        return false;
    };
    host == "api.github.com" || (host == "github.com" && path.starts_with("/api/"))
}

/// Replace any known token with a placeholder before a message is logged or
/// returned as an error.
pub fn redact_secrets(text: &str) -> String {
    let mut out = text.to_string();
    if let Some(token) = github_token() {
        if !token.is_empty() {
            out = out.replace(&token, "[redacted]");
        }
    }
    out
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
    headers.extend(auth_headers_for(url));
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
    headers.extend(auth_headers_for(url));
    let resp = get_response(agent, url, &headers)?;
    if !(200..300).contains(&resp.status()) {
        return Err(status_error("GET", url, resp));
    }
    let body = read_body(resp, DEFAULT_MAX_DOWNLOAD_BYTES)?;
    serde_json::from_str(&body).map_err(|e| Error::Http(format!("invalid JSON from {url}: {e}")))
}

/// A response body read under an explicit byte cap, with its HTTP status kept
/// so callers can map specific statuses without inspecting error text.
pub struct LimitedResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// GET a response body with an explicit byte cap. Used for attestation API
/// responses, which must not be allowed to exhaust memory or disk.
pub fn get_limited_response(
    agent: &ureq::Agent,
    url: &str,
    headers: &[(&str, String)],
    max_bytes: u64,
) -> Result<LimitedResponse> {
    let mut all: Vec<(&str, String)> = headers.to_vec();
    all.extend(auth_headers_for(url));
    let resp = get_response(agent, url, &all)?;
    let status = resp.status();
    let mut reader = resp.into_reader().take(max_bytes.saturating_add(1));
    let mut body = Vec::new();
    reader
        .read_to_end(&mut body)
        .map_err(|e| Error::Http(format!("failed to read response body: {e}")))?;
    if body.len() as u64 > max_bytes {
        return Err(Error::DownloadTooLarge { limit: max_bytes });
    }
    Ok(LimitedResponse { status, body })
}

pub fn download_to_writer(
    agent: &ureq::Agent,
    url: &str,
    mut writer: impl std::io::Write,
    max_bytes: u64,
) -> Result<u64> {
    // Asset downloads never carry credentials, even when a token is present.
    let headers: Vec<(&str, String)> = vec![("User-Agent", "tuishou".to_string())];
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
    let initial_origin = origin(&current)?;
    for _ in 0..=MAX_REDIRECTS {
        ensure_allowed_url(&current)?;
        let hop_origin = origin(&current)?;
        let mut req = agent.get(&current).set("User-Agent", "tuishou");
        for (name, value) in headers {
            // Credentials never cross an origin boundary, and are never sent
            // to a host that is not a GitHub API endpoint.
            if name.eq_ignore_ascii_case("authorization") && hop_origin != initial_origin {
                continue;
            }
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

/// Split a URL into scheme, host, effective port and path. The port is
/// normalised so scheme changes count as an origin change.
fn url_parts(url: &str) -> Result<(String, String, u16, String)> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| Error::Http(format!("invalid URL {url}")))?;
    let authority = rest.split('/').next().unwrap_or(rest);
    let host_port = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    let (host, port) = match host_port.strip_prefix('[') {
        Some(rest) => {
            let (host, tail) = rest
                .split_once(']')
                .ok_or_else(|| Error::Http(format!("invalid URL {url}")))?;
            let port = tail
                .strip_prefix(':')
                .and_then(|p| p.parse::<u16>().ok())
                .unwrap_or(if scheme == "https" { 443 } else { 80 });
            (host.to_ascii_lowercase(), port)
        }
        None => {
            let (host, port) = host_port
                .split_once(':')
                .map(|(h, p)| (h, p.parse::<u16>().ok()))
                .unwrap_or((host_port, None));
            (
                host.to_ascii_lowercase(),
                port.unwrap_or(if scheme == "https" { 443 } else { 80 }),
            )
        }
    };
    let path = rest.strip_prefix(authority).unwrap_or("").to_string();
    Ok((scheme.to_ascii_lowercase(), host, port, path))
}

fn origin(url: &str) -> Result<(String, String, u16)> {
    let (scheme, host, port, _) = url_parts(url)?;
    Ok((scheme, host, port))
}

/// Host allowlist for redirects. Loopback and extra hosts are only available
/// in builds with the `test-hooks` feature; release binaries ignore the
/// corresponding environment variables.
fn ensure_allowed_url(url: &str) -> Result<()> {
    ensure_https(url)?;
    let (_, host, _, _) = url_parts(url)?;
    let extra = extra_allowed_hosts().iter().any(|s| s == &host);
    let github = matches!(
        host.as_str(),
        "github.com"
            | "api.github.com"
            | "objects.githubusercontent.com"
            | "release-assets.githubusercontent.com"
            | "github-releases.githubusercontent.com"
    );
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

#[cfg(feature = "test-hooks")]
fn extra_allowed_hosts() -> Vec<String> {
    std::env::var("TUISHOU_ALLOWED_HOSTS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(not(feature = "test-hooks"))]
fn extra_allowed_hosts() -> Vec<String> {
    Vec::new()
}

#[cfg(feature = "test-hooks")]
fn allow_insecure_http() -> bool {
    std::env::var("TUISHOU_ALLOW_INSECURE_HTTP").ok().as_deref() == Some("1")
}

#[cfg(not(feature = "test-hooks"))]
fn allow_insecure_http() -> bool {
    false
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
    status_error_body(method, url, code, body.trim().as_bytes())
}

/// Build the same redacted, one-line HTTP error from an already-read body.
pub(crate) fn status_error_body(method: &str, url: &str, status: u16, body: &[u8]) -> Error {
    let body = redact_secrets(&crate::logging::one_line(
        String::from_utf8_lossy(body).trim(),
    ));
    let message = if body.is_empty() {
        format!("{method} {url} failed with status {status}")
    } else {
        format!("{method} {url} failed with status {status}: {body}")
    };
    Error::Http(redact_secrets(&message))
}

fn map_ureq_err(method: &str, url: &str, err: ureq::Error) -> Error {
    match err {
        ureq::Error::Status(_, resp) => status_error(method, url, resp),
        other => Error::Http(redact_secrets(&format!("{method} {url} failed: {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "test-hooks")]
    use std::sync::{Arc, Mutex};

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
        // Other tests set TUISHOU_ALLOW_INSECURE_HTTP; hold the lock so the answer does not depend on timing.
        let _guard = crate::test_env_lock();
        assert!(ensure_allowed_url("https://github.com/ddproxy/tuishou").is_ok());
        assert!(ensure_allowed_url("https://evil.test/x").is_err());
        assert!(ensure_allowed_url("http://127.0.0.1:8080/x").is_err());
    }

    #[test]
    fn github_auth_is_only_for_api_endpoints() {
        assert!(is_github_api("https://api.github.com/repos/a/b/releases"));
        assert!(!is_github_api(
            "https://objects.githubusercontent.com/asset"
        ));
        assert!(!is_github_api("https://example.test/api/v3/releases"));
        assert_eq!(origin("http://localhost:1/a").unwrap().2, 1);
        assert_eq!(origin("http://localhost/a").unwrap().2, 80);
    }

    #[cfg(feature = "test-hooks")]
    fn spawn_capture_server(
        body: &'static [u8],
        redirect: Option<String>,
    ) -> (String, Arc<Mutex<Vec<String>>>, std::sync::mpsc::Sender<()>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let (stop, rx) = std::sync::mpsc::channel::<()>();
        let handle = thread::spawn(move || {
            while rx.try_recv().is_err() {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buf = [0u8; 4096];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        seen2
                            .lock()
                            .unwrap()
                            .push(String::from_utf8_lossy(&buf[..n]).to_string());
                        let response = match &redirect {
                            Some(location) => format!(
                                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            ),
                            None => format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            ),
                        };
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.write_all(body);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        // The server thread is leaked only for the duration of the test
        // process; the channel stops its loop when the test ends.
        let _ = handle;
        (base, seen, stop)
    }

    #[cfg(feature = "test-hooks")]
    #[test]
    fn redirect_to_another_origin_drops_credentials() {
        let _guard = crate::test_env_lock();
        std::env::set_var("TUISHOU_ALLOW_INSECURE_HTTP", "1");
        let (second, seen_second, stop_second) = spawn_capture_server(b"ok", None);
        let (first, seen_first, stop_first) = spawn_capture_server(b"", Some(second.clone()));
        let headers = [
            (
                "Authorization".to_string(),
                "Bearer secret-token".to_string(),
            ),
            ("X-Test".to_string(), "kept".to_string()),
        ];
        let refs: Vec<(&str, String)> = headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect();
        let agent = agent();
        let response = get_response(&agent, &format!("{first}/start"), &refs).unwrap();
        assert_eq!(response.status(), 200);
        let first_requests = seen_first.lock().unwrap();
        assert!(first_requests[0].contains("secret-token"));
        drop(first_requests);
        let second_requests = seen_second.lock().unwrap();
        assert!(!second_requests[0]
            .to_ascii_lowercase()
            .contains("authorization"));
        assert!(!second_requests[0].contains("secret-token"));
        assert!(second_requests[0].to_ascii_lowercase().contains("x-test"));
        drop(second_requests);
        let _ = stop_first.send(());
        let _ = stop_second.send(());
        std::env::remove_var("TUISHOU_ALLOW_INSECURE_HTTP");
    }

    #[test]
    fn only_known_github_asset_hosts_are_allowed() {
        let _guard = crate::test_env_lock();
        assert!(ensure_allowed_url("https://objects.githubusercontent.com/a").is_ok());
        assert!(ensure_allowed_url("https://release-assets.githubusercontent.com/a").is_ok());
        assert!(ensure_allowed_url("https://attacker.githubusercontent.com/a").is_err());
    }

    #[cfg(feature = "test-hooks")]
    #[test]
    fn test_hooks_enable_extra_hosts_and_loopback() {
        let _guard = crate::test_env_lock();
        std::env::set_var("TUISHOU_ALLOWED_HOSTS", "extra.test");
        std::env::set_var("TUISHOU_ALLOW_INSECURE_HTTP", "1");
        assert!(ensure_allowed_url("https://extra.test/x").is_ok());
        assert!(ensure_allowed_url("http://127.0.0.1:8080/x").is_ok());
        std::env::remove_var("TUISHOU_ALLOWED_HOSTS");
        std::env::remove_var("TUISHOU_ALLOW_INSECURE_HTTP");
    }

    #[cfg(not(feature = "test-hooks"))]
    #[test]
    fn release_build_ignores_network_test_hooks() {
        std::env::set_var("TUISHOU_ALLOWED_HOSTS", "extra.test");
        std::env::set_var("TUISHOU_ALLOW_INSECURE_HTTP", "1");
        assert!(ensure_allowed_url("https://extra.test/x").is_err());
        assert!(ensure_allowed_url("http://127.0.0.1:8080/x").is_err());
        std::env::remove_var("TUISHOU_ALLOWED_HOSTS");
        std::env::remove_var("TUISHOU_ALLOW_INSECURE_HTTP");
    }
}
