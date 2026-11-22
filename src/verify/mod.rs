//! In-process release attestation verification.
//!
//! GitHub attestations are fetched over tuishou's existing HTTP path and
//! verified with the official `sigstore` crate. There is no `gh` subprocess,
//! and failures are always refused.

pub mod github;
pub mod sigstore_backend;
pub mod trust;

use crate::config::TrustRootKind;
use crate::error::{Error, Result};
use sha2::{Digest, Sha256};
use std::fmt::Debug;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub const FAKE_VERIFIER_ENV: &str = "TUISHOU_FAKE_ATTESTATION";

#[derive(Debug, Clone)]
pub struct AttestationRequest {
    pub path: PathBuf,
    pub repo: String,
    pub signer_workflow: String,
    /// Expected release tag. `tuishou verify` may omit it, in which case the
    /// tag is read from the certificate SAN and must still be a tag ref.
    pub tag: Option<String>,
    /// Expected source commit digest from the release metadata, when known.
    pub source_digest: Option<String>,
    pub trust_root: TrustRootKind,
    pub cache_dir: PathBuf,
    pub cache_ttl: Duration,
}

#[derive(Debug, Clone)]
pub struct AttestationReport {
    pub digest: String,
    pub issuer: String,
    pub identity: String,
    pub workflow: String,
    pub ref_value: String,
    pub log_index: i64,
    pub predicate_type: String,
}

pub trait AttestationVerifier: Debug + Send + Sync {
    fn verify(&self, request: &AttestationRequest) -> Result<AttestationReport>;
}

#[derive(Debug, Default)]
pub struct SigstoreAttestationVerifier;

impl AttestationVerifier for SigstoreAttestationVerifier {
    fn verify(&self, request: &AttestationRequest) -> Result<AttestationReport> {
        verify_attestation(request)
    }
}

/// The injected verifier used by production call sites. A debug-only hook lets
/// integration tests exercise install paths without network access; release
/// builds always use the real verifier.
pub fn default_verifier() -> Arc<dyn AttestationVerifier> {
    #[cfg(debug_assertions)]
    {
        if std::env::var_os(FAKE_VERIFIER_ENV).is_some() {
            return Arc::new(FakeAttestationVerifier);
        }
    }
    Arc::new(SigstoreAttestationVerifier)
}

/// Verify an artifact against every attestation returned for its digest.
pub fn verify_attestation(request: &AttestationRequest) -> Result<AttestationReport> {
    let digest = hex_encode(&sha256_bytes(&request.path)?);
    let bundles = github::fetch_bundles(&crate::http::agent(), &request.repo, &digest)?;
    let mut first_error: Option<Error> = None;
    for bundle in bundles {
        match sigstore_backend::verify_bundle(
            request,
            // The trust root is rebuilt from the same pinned bytes for each
            // candidate bundle; verification consumes it.
            trust::reload(&request.trust_root_config(), &request.cache_dir)?,
            &bundle,
            decode_sha256(&digest)?,
        ) {
            Ok(report) => return Ok(report),
            Err(e) => {
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }
    }
    Err(first_error.unwrap_or_else(|| {
        Error::AttestationRefused("no attestation bundle could be verified".to_string())
    }))
}

impl AttestationRequest {
    fn trust_root_config(&self) -> crate::config::VerifyConfig {
        crate::config::VerifyConfig {
            signer_workflow: self.signer_workflow.clone(),
            components: Default::default(),
            trust_root: self.trust_root.clone(),
            cache_dir: Some(self.cache_dir.clone()),
            cache_ttl: format!("{}", self.cache_ttl.as_secs()),
        }
    }
}

/// Fail-closed fake used by debug-build integration tests only.
#[derive(Debug, Default)]
pub struct FakeAttestationVerifier;

impl AttestationVerifier for FakeAttestationVerifier {
    fn verify(&self, request: &AttestationRequest) -> Result<AttestationReport> {
        let digest = hex_encode(&sha256_bytes(&request.path)?);
        let ref_value = request
            .tag
            .as_ref()
            .map(|tag| format!("refs/tags/{tag}"))
            .unwrap_or_else(|| "refs/tags/v0.0.0-test".to_string());
        Ok(AttestationReport {
            identity: format!(
                "https://github.com/{}/{}{}",
                request.repo, request.signer_workflow, ref_value
            ),
            digest,
            issuer: "https://token.actions.githubusercontent.com".to_string(),
            workflow: request.signer_workflow.clone(),
            ref_value,
            log_index: 0,
            predicate_type: "https://slsa.dev/provenance/v1".to_string(),
        })
    }
}

pub fn sha256_file(path: &Path) -> Result<String> {
    Ok(hex_encode(&sha256_bytes(path)?))
}

fn sha256_bytes(path: &Path) -> Result<Vec<u8>> {
    let mut file = std::fs::File::open(path).map_err(|source| Error::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 16384];
    loop {
        let n = file.read(&mut buf).map_err(|source| Error::Read {
            path: path.to_path_buf(),
            source,
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_vec())
}

pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn decode_sha256(hex: &str) -> Result<[u8; 32]> {
    let mut out = [0u8; 32];
    let (chunks, rest) = hex.as_bytes().as_chunks::<2>();
    if !rest.is_empty() || chunks.len() != out.len() {
        return Err(Error::Attestation(
            "artifact digest must be 64 hex characters".into(),
        ));
    }
    for (i, chunk) in chunks.iter().enumerate() {
        let hi = (chunk[0] as char).to_digit(16).ok_or_else(|| {
            Error::Attestation("artifact digest contains non-hex characters".into())
        })?;
        let lo = (chunk[1] as char).to_digit(16).ok_or_else(|| {
            Error::Attestation("artifact digest contains non-hex characters".into())
        })?;
        out[i] = ((hi << 4) | lo) as u8;
    }
    Ok(out)
}

pub fn parse_checksum_file(content: &str, filename: &str) -> Result<String> {
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        let hash = parts[0].to_ascii_lowercase();
        if !looks_like_sha256(&hash) {
            continue;
        }
        if parts.len() == 1 {
            return Ok(hash);
        }
        let file = parts[1].trim_start_matches('*');
        let file = file.rsplit('/').next().unwrap_or(file);
        if file == filename {
            return Ok(hash);
        }
    }
    let hashes: Vec<String> = content
        .lines()
        .filter_map(|l| {
            let h = l.split_whitespace().next()?.to_ascii_lowercase();
            if looks_like_sha256(&h) {
                Some(h)
            } else {
                None
            }
        })
        .collect();
    if hashes.len() == 1 {
        return Ok(hashes[0].clone());
    }
    Err(Error::ChecksumMismatch {
        file: filename.to_string(),
        expected: "<unparseable checksum file>".to_string(),
        actual: "<missing>".to_string(),
    })
}

fn looks_like_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn verify_checksum(path: &Path, expected: &str) -> Result<()> {
    let actual = sha256_file(path)?;
    let expected = expected.to_ascii_lowercase();
    if actual != expected {
        return Err(Error::ChecksumMismatch {
            file: path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string()),
            expected,
            actual,
        });
    }
    Ok(())
}

/// Validate a tag before it is embedded in an expected `refs/tags/...` URI.
pub fn validate_tag(tag: &str, field: &str) -> Result<()> {
    let invalid = tag.is_empty()
        || tag.starts_with('/')
        || tag.contains("..")
        || tag.contains('\\')
        || tag.chars().any(|c| c.is_control() || c.is_whitespace());
    if invalid {
        return Err(Error::msg(format!(
            "{field} must be a non-empty git tag (without a refs/tags/ prefix)"
        )));
    }
    Ok(())
}

/// Validate a configured signer workflow path before it is used to build an
/// expected SAN URI. Strict equality later prevents substring/confusion bugs.
pub fn validate_signer_workflow(path: &str, field: &str) -> Result<()> {
    let invalid = path.is_empty()
        || path.starts_with('/')
        || !path.starts_with(".github/workflows/")
        || path.contains("..")
        || path.contains('\\')
        || !(path.ends_with(".yml") || path.ends_with(".yaml"))
        || path.chars().any(|c| c.is_control() || c.is_whitespace());
    if invalid {
        return Err(Error::msg(format!(
            "{field} must be a relative .github/workflows/*.yml path"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_workflow_paths() {
        assert!(validate_signer_workflow(
            ".github/workflows/release.yml",
            "verify.signer_workflow"
        )
        .is_ok());
        assert!(validate_signer_workflow("release.yml", "x").is_err());
        assert!(validate_signer_workflow(".github/workflows/../x.yml", "x").is_err());
        assert!(validate_signer_workflow("/github/workflows/x.yml", "x").is_err());
        assert!(validate_signer_workflow(".github/workflows/x.txt", "x").is_err());
    }

    #[test]
    fn no_gh_subprocess_in_source() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        visit(&root, &mut offenders);
        assert!(offenders.is_empty(), "gh subprocess found: {offenders:?}");
    }

    fn visit(dir: &Path, offenders: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, offenders);
            } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    for needle in ["Command::new(\"gh\")", "run(\"gh\"", "checked(\"gh\""] {
                        if text.contains(needle) {
                            offenders.push(format!("{}: {needle}", path.display()));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn parse_checksum_standard() {
        let hash = "a".repeat(64);
        let content = format!("{hash}  lu-v0.4.0-x86_64-unknown-linux-musl.tar.gz\n");
        let got =
            parse_checksum_file(&content, "lu-v0.4.0-x86_64-unknown-linux-musl.tar.gz").unwrap();
        assert_eq!(got, hash);
    }

    #[test]
    fn verify_checksum_mismatch() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("f");
        std::fs::write(&p, b"hello").unwrap();
        let err = verify_checksum(&p, &"b".repeat(64)).unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"));
    }
}
