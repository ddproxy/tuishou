//! GitHub attestations API client.
//!
//! This module deliberately reuses tuishou's HTTP rules: manual redirects,
//! GitHub-only host allowlist, timeouts, and credentials that are only ever
//! attached to `api.github.com`. No subprocess is involved.

use crate::error::{Error, Result};
use crate::http;

pub const MAX_ATTESTATION_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_ATTESTATION_BUNDLE_BYTES: u64 = 4 * 1024 * 1024;
/// Cap candidate bundles so one response cannot trigger unbounded verification work.
pub const MAX_ATTESTATION_BUNDLES: usize = 16;
/// The attestation API uses 404 for both a missing attestation and an
/// inaccessible repository. Never surface the latter as an authentication hint.
pub const ATTESTATION_NOT_FOUND_MESSAGE: &str = "no attestation found for this digest";

/// Fetch every attestation bundle associated with a subject digest.
pub fn fetch_bundles(agent: &ureq::Agent, repo: &str, sha256_hex: &str) -> Result<Vec<Vec<u8>>> {
    validate_repo(repo)?;
    let (owner, name) = split_repo(repo)?;
    if !is_sha256_hex(sha256_hex) {
        return Err(Error::AttestationRefused(format!(
            "invalid subject digest '{sha256_hex}': expected 64 hex characters"
        )));
    }
    let url = format!(
        "{}/repos/{owner}/{name}/attestations/sha256:{sha256_hex}",
        crate::release::github_api_base()
    );
    let headers = vec![
        ("Accept", "application/vnd.github+json".to_string()),
        ("User-Agent", "tuishou".to_string()),
    ];
    let response =
        http::get_limited_response(agent, &url, &headers, MAX_ATTESTATION_RESPONSE_BYTES).map_err(
            |e| match e {
                Error::DownloadTooLarge { limit } => Error::AttestationRefused(format!(
                    "attestation API response exceeded {limit} bytes"
                )),
                other => other,
            },
        )?;
    if response.status == 404 {
        return Err(Error::AttestationUnavailable(
            ATTESTATION_NOT_FOUND_MESSAGE.to_string(),
        ));
    }
    if !(200..300).contains(&response.status) {
        return Err(http::status_error_body(
            "GET",
            &url,
            response.status,
            &response.body,
        ));
    }
    parse_attestation_response(&response.body)
}

/// Parse the REST response. Kept separate so malformed responses can be
/// exercised without a live server.
pub fn parse_attestation_response(body: &[u8]) -> Result<Vec<Vec<u8>>> {
    if body.is_empty() {
        return Err(Error::AttestationRefused(
            "attestation API returned an empty response".to_string(),
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(body).map_err(|e| {
        Error::AttestationRefused(format!("attestation API returned malformed JSON: {e}"))
    })?;
    let attestations = value
        .get("attestations")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            Error::AttestationRefused(
                "attestation API response missing 'attestations' array".to_string(),
            )
        })?;
    if attestations.is_empty() {
        return Err(Error::AttestationUnavailable(
            "attestation API returned no bundles for the subject digest".to_string(),
        ));
    }
    if attestations.len() > MAX_ATTESTATION_BUNDLES {
        return Err(Error::AttestationRefused(format!(
            "attestation API returned {} bundles; limit is {MAX_ATTESTATION_BUNDLES}",
            attestations.len()
        )));
    }
    let mut bundles = Vec::with_capacity(attestations.len());
    for (index, entry) in attestations.iter().enumerate() {
        let bundle = entry.get("bundle").ok_or_else(|| {
            Error::AttestationRefused(format!("attestation[{index}] has no bundle"))
        })?;
        let bytes = match bundle {
            serde_json::Value::Object(_) => serde_json::to_vec(bundle)
                .map_err(|e| Error::Attestation(format!("cannot encode bundle: {e}")))?,
            serde_json::Value::String(s) => s.as_bytes().to_vec(),
            _ => {
                return Err(Error::AttestationRefused(format!(
                    "attestation[{index}] bundle has an unsupported shape"
                )))
            }
        };
        if bytes.is_empty() {
            return Err(Error::AttestationRefused(format!(
                "attestation[{index}] bundle is empty"
            )));
        }
        if bytes.len() as u64 > MAX_ATTESTATION_BUNDLE_BYTES {
            return Err(Error::AttestationRefused(format!(
                "attestation[{index}] bundle exceeded {MAX_ATTESTATION_BUNDLE_BYTES} bytes"
            )));
        }
        bundles.push(bytes);
    }
    Ok(bundles)
}

/// Validate an `owner/name` repository before using it in a URL or SAN URI.
pub fn validate_repo(repo: &str) -> Result<()> {
    split_repo(repo).map(|_| ())
}

fn split_repo(repo: &str) -> Result<(&str, &str)> {
    let (owner, name) = repo.split_once('/').ok_or_else(|| {
        Error::AttestationRefused(format!("invalid repository '{repo}': expected owner/name"))
    })?;
    if !valid_component(owner) || !valid_component(name) || owner.is_empty() || name.is_empty() {
        return Err(Error::AttestationRefused(format!(
            "invalid repository '{repo}': expected owner/name"
        )));
    }
    Ok((owner, name))
}

fn valid_component(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_repo_and_digest() {
        assert!(split_repo("owner/repo").is_ok());
        assert!(split_repo("owner").is_err());
        assert!(split_repo("owner/../repo").is_err());
        assert!(is_sha256_hex(&"a".repeat(64)));
        assert!(!is_sha256_hex("short"));
    }

    #[test]
    fn parses_bundle_objects() {
        let body = br#"{"attestations":[{"bundle":{"mediaType":"x"}}]}"#;
        let bundles = parse_attestation_response(body).unwrap();
        assert_eq!(bundles.len(), 1);
        let value: serde_json::Value = serde_json::from_slice(&bundles[0]).unwrap();
        assert_eq!(value["mediaType"], "x");
    }

    #[test]
    fn rejects_too_many_bundles() {
        let entry = r#"{"bundle":{"mediaType":"x"}}"#;
        let body = format!(
            "{{\"attestations\":[{}]}}",
            vec![entry; MAX_ATTESTATION_BUNDLES].join(",")
        );
        assert_eq!(
            parse_attestation_response(body.as_bytes()).unwrap().len(),
            16
        );

        let body = format!(
            "{{\"attestations\":[{}]}}",
            vec![entry; MAX_ATTESTATION_BUNDLES + 1].join(",")
        );
        let err = parse_attestation_response(body.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("limit is 16"), "{err}");
    }

    #[test]
    fn rejects_oversized_bundle() {
        let padding = "a".repeat(MAX_ATTESTATION_BUNDLE_BYTES as usize + 1);
        let body = format!(r#"{{"attestations":[{{"bundle":{{"x":"{padding}"}}}}]}}"#);
        let err = parse_attestation_response(body.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("exceeded"), "{err}");
    }

    #[test]
    fn rejects_missing_truncated_and_empty_responses() {
        assert!(parse_attestation_response(b"").is_err());
        assert!(parse_attestation_response(br#"{"attestations":["#).is_err());
        assert!(parse_attestation_response(br#"{}"#).is_err());
        assert!(parse_attestation_response(br#"{"attestations":[]}"#).is_err());
        assert!(parse_attestation_response(br#"{"attestations":[{}]}"#).is_err());
    }

    #[cfg(feature = "test-hooks")]
    #[test]
    fn not_found_attestation_hides_authentication_hints() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;
        use std::time::{Duration, Instant};

        let _guard = crate::test_env_lock();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let body =
            br#"{"message":"Must have admin rights to the repository; authentication required"}"#;
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return;
                        }
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => return,
                }
            };
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(body);
        });

        let old_base = std::env::var_os("TUISHOU_GITHUB_API_BASE");
        let old_http = std::env::var_os("TUISHOU_ALLOW_INSECURE_HTTP");
        std::env::set_var("TUISHOU_GITHUB_API_BASE", &base);
        std::env::set_var("TUISHOU_ALLOW_INSECURE_HTTP", "1");
        let err = fetch_bundles(&crate::http::agent(), "owner/repo", &"a".repeat(64)).unwrap_err();
        server.join().unwrap();
        match old_base {
            Some(value) => std::env::set_var("TUISHOU_GITHUB_API_BASE", value),
            None => std::env::remove_var("TUISHOU_GITHUB_API_BASE"),
        }
        match old_http {
            Some(value) => std::env::set_var("TUISHOU_ALLOW_INSECURE_HTTP", value),
            None => std::env::remove_var("TUISHOU_ALLOW_INSECURE_HTTP"),
        }

        let message = err.to_string();
        assert!(message.contains(ATTESTATION_NOT_FOUND_MESSAGE), "{message}");
        assert!(
            !message.to_ascii_lowercase().contains("authentication"),
            "{message}"
        );
    }
}
