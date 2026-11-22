use crate::error::{Error, Result};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use std::process::Command;

pub fn sha256_file(path: &Path) -> Result<String> {
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
    Ok(hex_encode(&hasher.finalize()))
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

pub fn parse_checksum_file(content: &str, filename: &str) -> Result<String> {
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // formats: "<hash>  <file>" or "<hash> <file>" or just "<hash>"
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
    // fallback: single hash in file
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

/// Verify a release with `gh attestation verify`. In auto mode the caller
/// passes a signer workflow identity which is forwarded to
/// `--signer-workflow` and never interpreted as a shell string.
pub fn verify_attestation_with_signer(
    archive: &Path,
    repo: &str,
    allow_unattested: bool,
    signer_workflow: Option<&str>,
) -> Result<()> {
    let gh = match which_gh() {
        Some(p) => p,
        None => {
            if allow_unattested {
                eprintln!(
                    "tuishou: gh not found; skipping attestation verification (--allow-unattested)"
                );
                return Ok(());
            }
            return Err(Error::AttestationUnavailable);
        }
    };
    let mut cmd = Command::new(gh);
    cmd.arg("attestation")
        .arg("verify")
        .arg(archive)
        .arg("--repo")
        .arg(repo);
    if let Some(signer) = signer_workflow {
        if !signer.is_empty() {
            cmd.arg("--signer-workflow").arg(signer);
        }
    }
    let output = cmd
        .output()
        .map_err(|e| Error::Attestation(format!("failed to run gh: {e}")))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let msg = format!(
        "{}{}",
        stdout.trim(),
        if stderr.trim().is_empty() {
            String::new()
        } else {
            format!("\n{}", stderr.trim())
        }
    );
    Err(Error::Attestation(msg.trim().to_string()))
}

pub fn gh_available() -> bool {
    which_gh().is_some()
}

fn which_gh() -> Option<String> {
    // simple PATH lookup
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let cand = dir.join("gh");
        if cand.is_file() {
            return Some(cand.to_string_lossy().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(b"hello").unwrap();
        let err = verify_checksum(&p, &"b".repeat(64)).unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"));
    }
}
