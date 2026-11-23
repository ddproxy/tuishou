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

/// Read trust material once for a verification run. Parse the returned bytes
/// with [`parse_bytes`] for each candidate bundle.
pub fn load_bytes(cfg: &VerifyConfig, cache_dir: &Path) -> Result<Vec<u8>> {
    let (bytes, _) = load_material(cfg, cache_dir)?;
    Ok(bytes)
}

/// Load and parse the trust material for a verification run. Kept for callers
/// that need a parsed root; the attestation path loads bytes once instead.
#[allow(dead_code)]
pub fn load(cfg: &VerifyConfig, cache_dir: &Path) -> Result<SigstoreTrustRoot> {
    let (bytes, source) = load_material(cfg, cache_dir)?;
    parse(&bytes).map_err(|e| Error::Attestation(format!("{source} is invalid: {e}")))
}

/// Rebuild the same trust material for another candidate bundle. Verification
/// consumes the trust root, so each candidate gets a fresh instance. Retained
/// for callers that reload; the attestation path parses in-memory bytes.
#[allow(dead_code)]
pub fn reload(cfg: &VerifyConfig, cache_dir: &Path) -> Result<SigstoreTrustRoot> {
    load(cfg, cache_dir)
}

/// Parse in-memory trust bytes into a fresh verifier instance.
pub fn parse_bytes(bytes: &[u8]) -> Result<SigstoreTrustRoot> {
    parse(bytes)
}

/// Returns the raw bytes and a label for parse errors. Disk and network access
/// happens here, never in the per-bundle parse.
fn load_material(cfg: &VerifyConfig, cache_dir: &Path) -> Result<(Vec<u8>, String)> {
    match &cfg.trust_root {
        TrustRootKind::Path(path) => {
            check_trust_file(path)?;
            let bytes = fs::read(path).map_err(|source| Error::Read {
                path: path.clone(),
                source,
            })?;
            Ok((bytes, format!("configured trust root {}", path.display())))
        }
        TrustRootKind::Embedded => {
            prepare_cache_dir(cache_dir)?;
            let cached = cache_dir.join(CACHED_ROOT_NAME);
            let ttl = cfg.cache_ttl()?;
            if let Some(bytes) = fresh_cache(&cached, ttl)? {
                return Ok((bytes, format!("cached trust root {}", cached.display())));
            }
            match refresh_via_tuf(cache_dir) {
                Ok(bytes) => Ok((bytes, "refreshed trust root".to_string())),
                Err(e) => {
                    crate::logging::journal(
                        "trust_root_fallback",
                        &[("reason", &crate::logging::one_line(&e.to_string()))],
                    );
                    Ok((embedded_root()?.to_vec(), "embedded trust root".to_string()))
                }
            }
        }
    }
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

fn untrusted(path: &Path, label: &str, reason: &str) -> Error {
    Error::Attestation(format!("untrusted {label} {}: {reason}", path.display()))
}

/// Reject files another user could replace or modify, before reading them.
fn check_trust_file(path: &Path) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path).map_err(|source| Error::Read {
        path: path.to_path_buf(),
        source,
    })?;
    check_regular_file(path, &metadata, "trust root")?;
    check_owner_and_mode(path, &metadata, "trust root")?;
    Ok(metadata)
}

fn check_regular_file(path: &Path, metadata: &fs::Metadata, label: &str) -> Result<()> {
    let kind = metadata.file_type();
    if kind.is_symlink() {
        return Err(untrusted(
            path,
            label,
            "must be a regular file, not a symlink",
        ));
    }
    if !kind.is_file() {
        return Err(untrusted(path, label, "must be a regular file"));
    }
    Ok(())
}

fn check_cache_dir(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    let kind = metadata.file_type();
    if kind.is_symlink() {
        return Err(untrusted(
            path,
            "trust root cache directory",
            "must be a directory, not a symlink",
        ));
    }
    if !kind.is_dir() {
        return Err(untrusted(
            path,
            "trust root cache directory",
            "must be a directory",
        ));
    }
    check_owner_and_mode(path, metadata, "trust root cache directory")
}

#[cfg(unix)]
fn check_owner_and_mode(path: &Path, metadata: &fs::Metadata, label: &str) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    if metadata.mode() & 0o022 != 0 {
        return Err(untrusted(
            path,
            label,
            "must not be writable by group or others",
        ));
    }
    let owner = metadata.uid();
    if let Some(uid) = crate::config::current_uid() {
        if owner != uid && owner != 0 {
            return Err(untrusted(
                path,
                label,
                &format!("owner uid {owner} is neither the running user ({uid}) nor root"),
            ));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_owner_and_mode(_path: &Path, _metadata: &fs::Metadata, _label: &str) -> Result<()> {
    Ok(())
}

fn prepare_cache_dir(dir: &Path) -> Result<()> {
    // Validate an existing directory before create_dir_all can chmod through a
    // symlink or adopt a directory owned by another user.
    if let Ok(metadata) = fs::symlink_metadata(dir) {
        check_cache_dir(dir, &metadata)?;
    }
    fs::create_dir_all(dir).map_err(|source| Error::Write {
        path: dir.to_path_buf(),
        source,
    })?;
    let metadata = fs::symlink_metadata(dir).map_err(|source| Error::Write {
        path: dir.to_path_buf(),
        source,
    })?;
    check_cache_dir(dir, &metadata)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != 0o700 {
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
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(Error::Read {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    check_regular_file(path, &metadata, "cached trust root")?;
    check_owner_and_mode(path, &metadata, "cached trust root")?;
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
    if metadata.len() == 0 {
        return Ok(None);
    }
    let modified = metadata.modified().map_err(|source| Error::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let age = SystemTime::now().duration_since(modified).map_err(|_| {
        Error::Attestation(format!(
            "trust root cache {} has a future modification time",
            path.display()
        ))
    })?;
    if age > ttl {
        return Ok(None);
    }
    let bytes = fs::read(path).map_err(|source| Error::Read {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Some(bytes))
}

fn refresh_via_tuf(cache_dir: &Path) -> Result<Vec<u8>> {
    // Validate the pinned TUF root asset before every refresh. The sigstore
    // crate loads its equivalent embedded root internally; this check ensures
    // our vendored root has not drifted from the pinned bytes.
    let _ = pinned_tuf_root()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::Attestation(format!("cannot start TUF refresh runtime: {e}")))?;
    let _root = runtime
        .block_on(SigstoreTrustRoot::new(Some(cache_dir)))
        .map_err(|e| Error::Attestation(format!("TUF trust root refresh failed: {e}")))?;
    let cached = cache_dir.join(CACHED_ROOT_NAME);
    check_trust_file(&cached)?;
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
    let bytes = fs::read(&cached).map_err(|source| Error::Read {
        path: cached.clone(),
        source,
    })?;
    if bytes.is_empty() {
        return Err(Error::Attestation(
            "TUF trust root refresh produced an empty cache".to_string(),
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn configured(path: &Path) -> VerifyConfig {
        VerifyConfig {
            trust_root: TrustRootKind::Path(path.to_path_buf()),
            ..VerifyConfig::default()
        }
    }

    fn embedded(cache: &Path) -> VerifyConfig {
        VerifyConfig {
            trust_root: TrustRootKind::Embedded,
            cache_dir: Some(cache.to_path_buf()),
            ..VerifyConfig::default()
        }
    }

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
        let cfg = configured(Path::new("/definitely/missing/trusted_root.json"));
        assert!(load(&cfg, Path::new("/tmp/unused-tuishou-verify")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn configured_group_writable_root_is_rejected() {
        use std::os::unix::fs::PermissionsExt;

        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("trusted_root.json");
        fs::write(&path, embedded_root().unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).unwrap();

        let err = load(&configured(&path), td.path()).unwrap_err();
        assert!(
            err.to_string().contains("writable by group or others"),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn configured_symlink_root_is_rejected() {
        let td = tempfile::tempdir().unwrap();
        let real = td.path().join("real.json");
        fs::write(&real, embedded_root().unwrap()).unwrap();
        let link = td.path().join("link.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let err = load(&configured(&link), td.path()).unwrap_err();
        assert!(err.to_string().contains("not a symlink"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn cache_dir_group_writable_is_rejected() {
        use std::os::unix::fs::PermissionsExt;

        let td = tempfile::tempdir().unwrap();
        let cache = td.path().join("cache");
        fs::create_dir(&cache).unwrap();
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o770)).unwrap();

        let err = load(&embedded(&cache), &cache).unwrap_err();
        assert!(err.to_string().contains("cache directory"), "{err}");
        assert!(err.to_string().contains("writable by group"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn cache_dir_other_owner_is_rejected_when_root() {
        use std::os::unix::fs::chown;

        let td = tempfile::tempdir().unwrap();
        let cache = td.path().join("cache");
        fs::create_dir(&cache).unwrap();
        // Only root can simulate foreign ownership; other users keep the
        // group-writable and symlink cache checks above.
        if crate::config::current_uid() == Some(0) {
            chown(&cache, Some(1), None).unwrap();
            let err = load(&embedded(&cache), &cache).unwrap_err();
            assert!(err.to_string().contains("owner uid"), "{err}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn cached_root_group_writable_is_rejected() {
        use std::os::unix::fs::PermissionsExt;

        let td = tempfile::tempdir().unwrap();
        let cache = td.path().join("cache");
        fs::create_dir(&cache).unwrap();
        let cached = cache.join(CACHED_ROOT_NAME);
        fs::write(&cached, embedded_root().unwrap()).unwrap();
        fs::set_permissions(&cached, fs::Permissions::from_mode(0o664)).unwrap();

        let err = load(&embedded(&cache), &cache).unwrap_err();
        assert!(err.to_string().contains("cached trust root"), "{err}");
    }

    #[test]
    fn cached_root_with_future_mtime_is_rejected() {
        let td = tempfile::tempdir().unwrap();
        let cache = td.path().join("cache");
        fs::create_dir(&cache).unwrap();
        let cached = cache.join(CACHED_ROOT_NAME);
        fs::write(&cached, embedded_root().unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&cached, fs::Permissions::from_mode(0o600)).unwrap();
        }

        let file = fs::File::options().write(true).open(&cached).unwrap();
        file.set_times(
            std::fs::FileTimes::new().set_modified(SystemTime::now() + Duration::from_secs(3600)),
        )
        .unwrap();
        drop(file);

        let err = load(&embedded(&cache), &cache).unwrap_err();
        assert!(
            err.to_string().contains("future modification time"),
            "{err}"
        );
    }
}
