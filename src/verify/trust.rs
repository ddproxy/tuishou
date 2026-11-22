//! Sigstore public-good trust root loading: embedded/pinned root, local cache,
//! and TUF refresh with a fail-closed fallback to the pinned root.

use crate::config::{TrustRootKind, VerifyConfig};
use crate::error::{Error, Result};
use sha2::{Digest, Sha256};
use sigstore::trust::sigstore::SigstoreTrustRoot;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

const EMBEDDED_TRUSTED_ROOT: &[u8] = include_bytes!("../../assets/sigstore-trusted-root.json");
const PINNED_TUF_ROOT: &[u8] = include_bytes!("../../assets/sigstore-tuf-root.json");
const EMBEDDED_TRUSTED_ROOT_SHA256: &str =
    "f44a1b88128e55ebfb62189becbc0fa48d4ec9915c65ac54ba0e46a008b12d5b";
const PINNED_TUF_ROOT_SHA256: &str =
    "5fe1e509a47277183a004745942c0a116379297da9f1cca529774320f1a5dfab";
const CACHED_ROOT_NAME: &str = "trusted_root.json";

/// Load the trust material for a verification run.
pub fn load(cfg: &VerifyConfig, cache_dir: &Path) -> Result<SigstoreTrustRoot> {
    match &cfg.trust_root {
        TrustRootKind::Path(path) => {
            let bytes = fs::read(path).map_err(|source| Error::Read {
                path: path.clone(),
                source,
            })?;
            parse(&bytes).map_err(|e| {
                Error::Attestation(format!(
                    "configured trust root {} is invalid: {e}",
                    path.display()
                ))
            })
        }
        TrustRootKind::Embedded => {
            prepare_cache_dir(cache_dir)?;
            let cached = cache_dir.join(CACHED_ROOT_NAME);
            let ttl = cfg.cache_ttl()?;
            if let Some(bytes) = fresh_cache(&cached, ttl)? {
                return parse(&bytes).map_err(|e| {
                    Error::Attestation(format!(
                        "cached trust root {} is invalid: {e}",
                        cached.display()
                    ))
                });
            }
            match refresh_via_tuf(cache_dir) {
                Ok(root) => Ok(root),
                Err(e) => {
                    crate::logging::journal(
                        "trust_root_fallback",
                        &[("reason", &crate::logging::one_line(&e.to_string()))],
                    );
                    parse(embedded_root()?).map_err(|e| {
                        Error::Attestation(format!("embedded trust root is invalid: {e}"))
                    })
                }
            }
        }
    }
}

/// Rebuild the same trust material for another candidate bundle. Verification
/// consumes the trust root, so each candidate gets a fresh instance.
pub fn reload(cfg: &VerifyConfig, cache_dir: &Path) -> Result<SigstoreTrustRoot> {
    load(cfg, cache_dir)
}

fn embedded_root() -> Result<&'static [u8]> {
    let digest = Sha256::digest(EMBEDDED_TRUSTED_ROOT);
    if crate::verify::hex_encode(&digest) != EMBEDDED_TRUSTED_ROOT_SHA256 {
        return Err(Error::Attestation(
            "embedded Sigstore trusted root failed its pinned checksum".to_string(),
        ));
    }
    Ok(EMBEDDED_TRUSTED_ROOT)
}

fn pinned_tuf_root() -> Result<&'static [u8]> {
    let digest = Sha256::digest(PINNED_TUF_ROOT);
    if crate::verify::hex_encode(&digest) != PINNED_TUF_ROOT_SHA256 {
        return Err(Error::Attestation(
            "pinned Sigstore TUF root failed its checksum".to_string(),
        ));
    }
    Ok(PINNED_TUF_ROOT)
}

fn parse(bytes: &[u8]) -> Result<SigstoreTrustRoot> {
    SigstoreTrustRoot::from_trusted_root_json_unchecked(bytes)
        .map_err(|e| Error::Attestation(format!("cannot parse Sigstore trusted root: {e}")))
}

fn prepare_cache_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).map_err(|source| Error::Write {
        path: dir.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::symlink_metadata(dir)
            .map_err(|source| Error::Write {
                path: dir.to_path_buf(),
                source,
            })?
            .permissions();
        if perms.mode() & 0o777 != 0o700 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|source| {
                Error::Write {
                    path: dir.to_path_buf(),
                    source,
                }
            })?;
        }
    }
    Ok(())
}

fn fresh_cache(path: &Path, ttl: Duration) -> Result<Option<Vec<u8>>> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(None);
    };
    if !metadata.is_file() || metadata.len() == 0 {
        return Ok(None);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(Error::Attestation(format!(
                "trust root cache {} has unsafe permissions; expected 0600",
                path.display()
            )));
        }
    }
    let modified = metadata.modified().map_err(|source| Error::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let age = SystemTime::now()
        .duration_since(modified)
        .unwrap_or(Duration::ZERO);
    if age > ttl {
        return Ok(None);
    }
    let bytes = fs::read(path).map_err(|source| Error::Read {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Some(bytes))
}

fn refresh_via_tuf(cache_dir: &Path) -> Result<SigstoreTrustRoot> {
    // Validate the pinned TUF root asset before every refresh. The sigstore
    // crate loads its equivalent embedded root internally; this check ensures
    // our vendored root has not drifted from the pinned bytes.
    let _ = pinned_tuf_root()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::Attestation(format!("cannot start TUF refresh runtime: {e}")))?;
    let root = runtime
        .block_on(SigstoreTrustRoot::new(Some(cache_dir)))
        .map_err(|e| Error::Attestation(format!("TUF trust root refresh failed: {e}")))?;
    let cached = cache_dir.join(CACHED_ROOT_NAME);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&cached, fs::Permissions::from_mode(0o600)).map_err(|source| {
            Error::Write {
                path: cached.clone(),
                source,
            }
        })?;
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_assets_match_expected_checksums() {
        assert_eq!(
            crate::verify::hex_encode(&Sha256::digest(EMBEDDED_TRUSTED_ROOT)),
            EMBEDDED_TRUSTED_ROOT_SHA256
        );
        assert_eq!(
            crate::verify::hex_encode(&Sha256::digest(PINNED_TUF_ROOT)),
            PINNED_TUF_ROOT_SHA256
        );
        parse(embedded_root().unwrap()).unwrap();
    }

    #[test]
    fn configured_path_must_exist() {
        let cfg = VerifyConfig {
            trust_root: TrustRootKind::Path(
                Path::new("/definitely/missing/trusted_root.json").to_path_buf(),
            ),
            ..VerifyConfig::default()
        };
        assert!(load(&cfg, Path::new("/tmp/unused-tuishou-verify")).is_err());
    }
}
