use crate::archive;
use crate::component::Component;
use crate::config::VerifyConfig;
use crate::download;
use crate::error::{Error, Result};
use crate::release;
use crate::state::{self, InstalledComponent};
use crate::target::Target;
use crate::verify::{self, AttestationRequest, AttestationVerifier};
use std::fs;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::NamedTempFile;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

/// A check run against a staged release binary before any installed file is
/// replaced. Only the verified, extracted binary is executed.
#[derive(Debug, Clone)]
pub struct StagedCheck {
    pub binary: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub version: Option<String>,
    pub prefix: Option<PathBuf>,
    pub dry_run: bool,
    pub allow_unattested: bool,
    pub allow_major: bool,
    pub max_download_bytes: u64,
    pub quiet: bool,
    #[allow(dead_code)]
    pub verbose: bool,
    /// Run `<binary> --version` on the staged file before commit. This is the
    /// only code from a release archive that tuishou ever executes.
    pub smoke_check: bool,
    /// Optional check of a staged binary before commit (Reiki config check).
    pub staged_check: Option<StagedCheck>,
    /// Write the new version to state. Deferred callers (agent/controller
    /// auto-update) only record it after their health check succeeds.
    pub commit_state: bool,
    /// Attestation verifier injected by the caller (real or test fake).
    pub verifier: Arc<dyn AttestationVerifier>,
    /// Verification configuration, including per-component workflow overrides.
    pub verify_config: VerifyConfig,
}

#[derive(Debug, Clone)]
pub struct InstallReceipt {
    pub checksum: String,
    pub target: String,
    pub installed_at: String,
}

pub struct InstallOutcome {
    pub component: String,
    pub version: String,
    pub binaries: Vec<String>,
    pub dry_run: bool,
    pub receipt: Option<InstallReceipt>,
}

/// Names of the files shipped in a release archive, used as a preflight check
/// before draining a node for a component that may not be in this release.
pub fn archive_binary_names(
    agent: &ureq::Agent,
    component: &Component,
    version: &str,
) -> Result<Vec<String>> {
    let target = Target::detect()?;
    let rel = release::fetch_tagged(agent, &component.repo, version)?;
    let asset_name = component.asset_name(&rel.tag, target.as_str());
    let checksum_name = component.checksum_asset_name(&rel.tag, target.as_str());
    let asset = release::find_asset(&rel, &asset_name)?;
    let checksum_asset = release::find_asset(&rel, &checksum_name)?;
    let tmp = tempfile_dir()?;
    let downloaded = download::download_release_assets(
        agent,
        &asset.url,
        &checksum_asset.url,
        &asset_name,
        &checksum_name,
        tmp.path(),
        crate::http::max_download_bytes(),
    );
    downloaded.and_then(|d| {
        let content = download::read_checksum_file(&d.checksum_path)?;
        let expected = verify::parse_checksum_file(&content, &asset_name)?;
        verify::verify_checksum(&d.archive_path, &expected)?;
        archive::list_binary_names(&d.archive_path)
    })
}

pub fn install_component(
    agent: &ureq::Agent,
    component: &Component,
    opts: &InstallOptions,
) -> Result<InstallOutcome> {
    let target = Target::detect()?;
    if !target.is_verified() && !opts.quiet {
        eprintln!(
            "tuishou: warning: target {} is listed but unverified",
            target.as_str()
        );
    }
    let rel = match &opts.version {
        Some(v) => release::fetch_tagged(agent, &component.repo, v)?,
        None => release::fetch_latest(agent, &component.repo)?,
    };
    let tag = rel.tag.clone();
    let expected_binaries = component.expected_binaries(&tag);
    let asset_name = component.asset_name(&tag, target.as_str());
    let checksum_name = component.checksum_asset_name(&tag, target.as_str());
    let asset = release::find_asset(&rel, &asset_name)?;
    let checksum_asset = release::find_asset(&rel, &checksum_name)?;

    let state_path = state::default_state_path(opts.prefix.as_deref());
    let st = state::load(&state_path)?;

    if let Some(installed) = st.components.get(&component.name) {
        if !opts.allow_major {
            if let (Some(i_major), Some(n_major)) =
                (major_version(&installed.version), major_version(&tag))
            {
                if n_major != i_major {
                    return Err(Error::MajorVersion {
                        installed: installed.version.clone(),
                        latest: tag.clone(),
                    });
                }
            }
        }
    }

    if opts.dry_run {
        if !opts.quiet {
            println!(
                "dry-run: install {} {} for {} from {}",
                component.name,
                tag,
                target.as_str(),
                component.repo
            );
        }
        return Ok(InstallOutcome {
            component: component.name.clone(),
            version: tag,
            binaries: expected_binaries,
            dry_run: true,
            receipt: None,
        });
    }

    let tmp = tempfile_dir()?;
    let downloaded = download::download_release_assets(
        agent,
        &asset.url,
        &checksum_asset.url,
        &asset_name,
        &checksum_name,
        tmp.path(),
        opts.max_download_bytes,
    )?;
    let checksum_content = download::read_checksum_file(&downloaded.checksum_path)?;
    let expected = verify::parse_checksum_file(&checksum_content, &asset_name)?;
    verify::verify_checksum(&downloaded.archive_path, &expected)?;
    let source_digest = release::fetch_source_digest(agent, &component.repo, &rel)?;
    let request = AttestationRequest {
        path: downloaded.archive_path.clone(),
        repo: component.repo.clone(),
        signer_workflow: opts.verify_config.workflow_for(&component.name).to_string(),
        tag: Some(tag.clone()),
        source_digest,
        trust_root: opts.verify_config.trust_root.clone(),
        cache_dir: opts.verify_config.cache_dir(opts.prefix.as_deref()),
        cache_ttl: opts.verify_config.cache_ttl()?,
    };
    let verified = match opts.verifier.verify(&request) {
        Ok(_) => true,
        Err(e) => {
            if !opts.allow_unattested {
                return Err(e);
            }
            eprintln!(
                "tuishou: attestation verification failed; continuing because --allow-unattested was set: {}",
                crate::http::redact_secrets(&e.to_string())
            );
            false
        }
    };
    if !verified && (opts.smoke_check || opts.staged_check.is_some()) {
        eprintln!(
            "tuishou: refusing to execute release code from an unverified archive; staged checks are skipped"
        );
    }

    let extract_dir = tmp.path().join("extract");
    let extracted =
        archive::extract_binaries(&downloaded.archive_path, &expected_binaries, &extract_dir)?;
    let installed_names: Vec<String> = extracted.iter().map(|b| b.name.clone()).collect();
    let missing: Vec<String> = component
        .required_binaries(&tag)
        .into_iter()
        .filter(|name| !installed_names.contains(name))
        .collect();
    if !missing.is_empty() {
        return Err(Error::Archive(format!(
            "archive {} is missing required binaries ({})",
            asset_name,
            missing.join(", ")
        )));
    }

    let bin_dir = install_dir(opts.prefix.as_deref());
    fs::create_dir_all(&bin_dir).map_err(|source| Error::Write {
        path: bin_dir.clone(),
        source,
    })?;
    check_bin_dir(&bin_dir)?;

    // A verified archive may run staged checks. An explicitly accepted
    // unattested archive is installed, but its code is never executed.
    if verified {
        if let Some(check) = &opts.staged_check {
            let staged = extract_dir.join(&check.binary);
            let args: Vec<&str> = check.args.iter().map(String::as_str).collect();
            let out = crate::process::checked(
                &staged.to_string_lossy(),
                &args,
                std::time::Duration::from_secs(30),
            )
            .map_err(|e| Error::StagedCheckFailed {
                binary: check.binary.clone(),
                detail: e.to_string(),
            })?;
            if out.status != 0 {
                return Err(Error::StagedCheckFailed {
                    binary: check.binary.clone(),
                    detail: format!("exit status {}", out.status),
                });
            }
            crate::logging::journal(
                "staged_check_ok",
                &[("component", &component.name), ("binary", &check.binary)],
            );
        }
        if opts.smoke_check {
            // Validate the staged binaries in place: a failed smoke check must
            // leave the installed binaries and their `.prev` files untouched.
            smoke_check(&extract_dir, &installed_names)?;
        }
    }

    let receipt = InstallReceipt {
        checksum: expected,
        target: target.as_str().to_string(),
        installed_at: state::now_rfc3339()?,
    };
    // Deferred callers confirm health before recording state. Keep `.prev`
    // intact for them so it still names the last confirmed-good version.
    commit_binaries(&extracted, &bin_dir, !opts.commit_state)?;
    // Bind `.prev` before any deferred health check: a failed check must be
    // able to restore a recorded known-good backup even on the first update
    // of an install made before this record existed.
    if !opts.commit_state {
        record_known_good(component, &tag, opts.prefix.as_deref())?;
    }
    if opts.commit_state {
        record_installed(component, &tag, &receipt, opts.prefix.as_deref())?;
    }

    if !opts.quiet {
        println!("installed {} {} ({})", component.name, tag, target.as_str());
    }
    Ok(InstallOutcome {
        component: component.name.clone(),
        version: tag,
        binaries: installed_names,
        dry_run: false,
        receipt: Some(receipt),
    })
}

/// Record checksums for the preserved `.prev` binaries.
///
/// Deferred callers leave `.prev` untouched until health succeeds, so this
/// record names the last confirmed-good bytes rather than the last bytes
/// merely installed. It must be written before any rollback can be attempted.
pub fn record_known_good(
    component: &Component,
    version: &str,
    prefix: Option<&Path>,
) -> Result<()> {
    let state_path = state::default_state_path(prefix);
    let bin_dir = install_dir(prefix);
    let mut good = state::load_known_good(&state_path)?;
    for name in component.expected_binaries(version) {
        let prev = bin_dir.join(format!("{name}.prev"));
        if is_safe_regular_file(&prev) {
            good.checksums
                .insert(name, crate::verify::sha256_file(&prev)?);
        }
    }
    state::save_known_good(&state_path, &good)
}

/// Record an install after a deferred health check has confirmed it.
pub fn record_installed(
    component: &Component,
    version: &str,
    receipt: &InstallReceipt,
    prefix: Option<&Path>,
) -> Result<()> {
    let state_path = state::default_state_path(prefix);
    let mut st = state::load(&state_path)?;
    let previous_version = st
        .components
        .get(&component.name)
        .map(|c| c.version.clone());
    st.components.insert(
        component.name.clone(),
        InstalledComponent {
            version: version.to_string(),
            target: receipt.target.clone(),
            checksum: receipt.checksum.clone(),
            installed_at: receipt.installed_at.clone(),
            previous_version,
        },
    );
    state::save(&state_path, &st)?;
    record_known_good(component, version, prefix)
}

/// Commit every staged binary, restoring destinations and the last good
/// `.prev` if any step fails. `.prev` is only replaced after all new binaries
/// are in place. All temporary files use random `O_EXCL` names and are kept as
/// RAII guards so a failed step cannot leave orphaned backups behind.
fn commit_binaries(
    extracted: &[archive::ExtractedBinary],
    bin_dir: &Path,
    keep_prev: bool,
) -> Result<()> {
    fs::create_dir_all(bin_dir).map_err(|source| Error::Write {
        path: bin_dir.to_path_buf(),
        source,
    })?;
    check_bin_dir(bin_dir)?;

    struct Pending {
        dest: PathBuf,
        staged: Option<NamedTempFile>,
        dest_backup: Option<NamedTempFile>,
        prev: PathBuf,
        prev_backup: Option<NamedTempFile>,
        dest_committed: bool,
        prev_committed: bool,
    }

    let mut pending: Vec<Pending> = Vec::new();
    let result = (|| {
        for bin in extracted {
            let dest = bin_dir.join(&bin.name);
            let prev = bin_dir.join(format!("{}.prev", bin.name));
            let staged = copy_into_temp(&bin.path, bin_dir, &format!(".{}.commit-", bin.name))?;
            set_executable(&staged)?;
            let dest_backup = backup_if_present(&dest, bin_dir, &format!(".{}.backup-", bin.name))?;
            let prev_backup =
                backup_if_present(&prev, bin_dir, &format!(".{}.prevkeep-", bin.name))?;
            pending.push(Pending {
                dest,
                staged: Some(staged),
                dest_backup,
                prev,
                prev_backup,
                dest_committed: false,
                prev_committed: false,
            });
        }
        for item in &mut pending {
            let staged = item
                .staged
                .take()
                .ok_or_else(|| Error::msg("internal error: staged binary was already committed"))?;
            persist_temp(staged, &item.dest)?;
            item.dest_committed = true;
        }
        if keep_prev {
            // Deferred callers confirm health before committing state. If no
            // `.prev` exists yet, the current binary is the last known good
            // one and must be preserved as the rollback target now.
            for item in &mut pending {
                if is_safe_regular_file(&item.prev) {
                    continue;
                }
                if fs::symlink_metadata(&item.prev).is_ok() {
                    return Err(Error::msg(format!(
                        "refusing unsafe backup target {}",
                        item.prev.display()
                    )));
                }
                if let Some(backup) = &item.dest_backup {
                    let name = item.dest.file_name().unwrap_or_default().to_string_lossy();
                    let staged =
                        copy_into_temp(backup.path(), bin_dir, &format!(".{name}.previnit-"))?;
                    set_executable(&staged)?;
                    persist_temp(staged, &item.prev)?;
                    item.prev_committed = true;
                }
            }
        } else {
            for item in &mut pending {
                if let Some(backup) = item.dest_backup.take() {
                    persist_temp(backup, &item.prev)?;
                    item.prev_committed = true;
                }
            }
        }
        Ok(())
    })();

    if let Err(e) = result {
        // Restore `.prev` first, then the installed binaries. NamedTempFile
        // drops clean up any staging files that were never committed.
        for item in pending.iter_mut().rev() {
            if let Some(backup) = item.prev_backup.take() {
                let _ = persist_temp(backup, &item.prev);
            } else if item.prev_committed {
                let _ = fs::remove_file(&item.prev);
            }
            if let Some(backup) = item.dest_backup.take() {
                let _ = persist_temp(backup, &item.dest);
            } else if item.dest_committed {
                let _ = fs::remove_file(&item.dest);
            }
        }
        return Err(e);
    }
    Ok(())
}

/// Restore `<name>.prev` over the installed binary. The `.prev` file itself is
/// left untouched so it remains the recorded last-known-good snapshot.
pub fn restore_previous_binary(bin_dir: &Path, name: &str) -> Result<()> {
    check_bin_dir(bin_dir)?;
    let dest = bin_dir.join(name);
    let prev = bin_dir.join(format!("{name}.prev"));
    if !is_safe_regular_file(&prev) {
        return Err(Error::NoPrevious(name.to_string()));
    }
    let restored = copy_into_temp(&prev, bin_dir, &format!(".{name}.restore-"))?;
    set_executable(&restored)?;
    persist_temp(restored, &dest)
}

/// Reject a prefix that is a symlink, has a symlinked parent, is not a
/// directory, is group/other-writable, or is owned by another user. A
/// privileged install into a writable prefix must fail closed.
fn check_bin_dir(bin_dir: &Path) -> Result<()> {
    if bin_dir.as_os_str().is_empty() {
        return Err(Error::msg("install prefix must not be empty"));
    }
    let components: Vec<_> = bin_dir.components().collect();
    let mut current = PathBuf::new();
    for (index, component) in components.iter().enumerate() {
        current.push(component.as_os_str());
        let meta = fs::symlink_metadata(&current).map_err(|source| Error::Read {
            path: current.clone(),
            source,
        })?;
        if meta.file_type().is_symlink() {
            return Err(Error::msg(format!(
                "refusing symlink in install prefix: {}",
                current.display()
            )));
        }
        if index + 1 < components.len() && !meta.is_dir() {
            return Err(Error::msg(format!(
                "install prefix component is not a directory: {}",
                current.display()
            )));
        }
    }
    let meta = fs::symlink_metadata(bin_dir).map_err(|source| Error::Read {
        path: bin_dir.to_path_buf(),
        source,
    })?;
    if !meta.is_dir() {
        return Err(Error::msg(format!(
            "install prefix is not a directory: {}",
            bin_dir.display()
        )));
    }
    #[cfg(unix)]
    {
        if meta.permissions().mode() & 0o022 != 0 {
            return Err(Error::msg(format!(
                "refusing group/other-writable install prefix: {}",
                bin_dir.display()
            )));
        }
        let euid = rustix::process::geteuid().as_raw();
        if meta.uid() != euid {
            return Err(Error::msg(format!(
                "refusing install prefix owned by uid {} ({})",
                meta.uid(),
                bin_dir.display()
            )));
        }
    }
    Ok(())
}

fn is_safe_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_file())
        .unwrap_or(false)
}

/// Make a private, unpredictable temporary file inside `dir`. `tempfile`
/// creates it with `O_EXCL` and mode 0600, so an attacker cannot substitute it
/// after the name is chosen.
fn private_temp(dir: &Path, prefix: &str) -> Result<NamedTempFile> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix).rand_bytes(16);
    builder.tempfile_in(dir).map_err(|source| {
        Error::msg(format!(
            "failed to create temporary file in {}: {source}",
            dir.display()
        ))
    })
}

#[cfg(unix)]
fn open_regular_no_follow(path: &Path) -> Result<fs::File> {
    use rustix::fs::{open, Mode, OFlags};
    let fd = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| Error::msg(format!("refusing unsafe source {}: {e}", path.display())))?;
    let file: fs::File = fd.into();
    let meta = file.metadata().map_err(|source| Error::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if !meta.file_type().is_file() {
        return Err(Error::msg(format!(
            "refusing unsafe source {}: not a regular file",
            path.display()
        )));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_regular_no_follow(path: &Path) -> Result<fs::File> {
    if !is_safe_regular_file(path) {
        return Err(Error::msg(format!(
            "refusing unsafe source {}",
            path.display()
        )));
    }
    fs::File::open(path).map_err(|source| Error::Read {
        path: path.to_path_buf(),
        source,
    })
}

fn copy_into_temp(src: &Path, dir: &Path, prefix: &str) -> Result<NamedTempFile> {
    let mut input = open_regular_no_follow(src)?;
    let mut tmp = private_temp(dir, prefix)?;
    std::io::copy(&mut input, tmp.as_file_mut()).map_err(|source| Error::Write {
        path: tmp.path().to_path_buf(),
        source,
    })?;
    Ok(tmp)
}

fn backup_if_present(path: &Path, dir: &Path, prefix: &str) -> Result<Option<NamedTempFile>> {
    if !is_safe_regular_file(path) {
        if fs::symlink_metadata(path).is_ok() {
            return Err(Error::msg(format!(
                "refusing unsafe install target {}",
                path.display()
            )));
        }
        return Ok(None);
    }
    copy_into_temp(path, dir, prefix).map(Some)
}

fn persist_temp(temp: NamedTempFile, dest: &Path) -> Result<()> {
    temp.persist(dest)
        .map_err(|e| Error::msg(format!("failed to commit {}: {}", dest.display(), e.error)))?;
    Ok(())
}

fn set_executable(file: &NamedTempFile) -> Result<()> {
    #[cfg(unix)]
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o755))
        .map_err(|source| Error::Write {
            path: file.path().to_path_buf(),
            source,
        })?;
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

pub fn install_dir(prefix: Option<&Path>) -> PathBuf {
    match prefix {
        Some(p) => p.to_path_buf(),
        None => default_bin_dir(),
    }
}

fn default_bin_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".local").join("bin");
        }
    }
    PathBuf::from("/usr/local/bin")
}

/// The only release-provided code tuishou executes: `--version` on each
/// installed binary. A non-zero exit or empty output fails the install.
pub fn smoke_check(bin_dir: &Path, binaries: &[String]) -> Result<()> {
    let timeout = std::time::Duration::from_secs(30);
    for name in binaries {
        let path = bin_dir.join(name);
        let output = crate::process::checked(&path.to_string_lossy(), &["--version"], timeout)
            .map_err(|e| Error::msg(format!("smoke check failed for {name}: {e}")))?;
        if output.stdout.trim().is_empty() {
            return Err(Error::msg(format!(
                "smoke check failed for {name}: --version produced no output"
            )));
        }
    }
    Ok(())
}

fn tempfile_dir() -> Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("tuishou-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    builder
        .tempdir()
        .map_err(|source| Error::msg(format!("failed to create temporary directory: {source}")))
}

pub fn major_version(tag: &str) -> Option<u64> {
    let v = tag.strip_prefix('v').unwrap_or(tag);
    let major = v.split('.').next()?;
    major.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tempdir_is_private_and_unpredictable() {
        let a = tempfile_dir().unwrap();
        let b = tempfile_dir().unwrap();
        assert!(a.path().is_dir());
        assert_ne!(a.path(), b.path());
        assert!(a
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("tuishou-"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(a.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
        let path = a.path().to_path_buf();
        drop(a);
        assert!(!path.exists());
    }

    #[test]
    fn temp_files_are_private_and_random() {
        let td = tempfile::tempdir().unwrap();
        let a = private_temp(td.path(), ".lu.commit-").unwrap();
        let b = private_temp(td.path(), ".lu.commit-").unwrap();
        assert_ne!(a.path(), b.path());
        assert!(a
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(".lu.commit-"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(a.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn commit_rejects_symlinked_backup_target() {
        #[cfg(unix)]
        {
            let td = tempfile::tempdir().unwrap();
            let bin_dir = td.path().join("bin");
            fs::create_dir_all(&bin_dir).unwrap();
            let outside = td.path().join("outside");
            fs::write(&outside, b"do-not-touch").unwrap();
            std::os::unix::fs::symlink(&outside, bin_dir.join("lu.prev")).unwrap();
            let staged = td.path().join("staged-lu");
            fs::write(&staged, b"new").unwrap();
            let extracted = vec![archive::ExtractedBinary {
                name: "lu".to_string(),
                path: staged,
            }];

            let err = commit_binaries(&extracted, &bin_dir, false).unwrap_err();
            assert!(
                err.to_string().contains("refusing unsafe install target"),
                "{err}"
            );
            assert_eq!(fs::read(&outside).unwrap(), b"do-not-touch");
        }
    }

    #[test]
    fn commit_leaves_no_temp_files() {
        let td = tempfile::tempdir().unwrap();
        let bin_dir = td.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        fs::write(bin_dir.join("lu"), b"old").unwrap();
        let staged = td.path().join("staged-lu");
        fs::write(&staged, b"new").unwrap();
        let extracted = vec![archive::ExtractedBinary {
            name: "lu".to_string(),
            path: staged,
        }];

        commit_binaries(&extracted, &bin_dir, false).unwrap();
        assert_eq!(fs::read(bin_dir.join("lu")).unwrap(), b"new");
        assert_eq!(fs::read(bin_dir.join("lu.prev")).unwrap(), b"old");
        let hidden: Vec<String> = fs::read_dir(&bin_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with('.'))
            .collect();
        assert!(hidden.is_empty(), "orphaned temp files: {hidden:?}");
    }

    #[test]
    fn deferred_commit_preserves_current_binary_as_prev() {
        let td = tempfile::tempdir().unwrap();
        let bin_dir = td.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        fs::write(bin_dir.join("lu"), b"known good").unwrap();
        let staged = td.path().join("staged-lu");
        fs::write(&staged, b"candidate").unwrap();
        let extracted = vec![archive::ExtractedBinary {
            name: "lu".to_string(),
            path: staged,
        }];

        commit_binaries(&extracted, &bin_dir, true).unwrap();
        assert_eq!(fs::read(bin_dir.join("lu")).unwrap(), b"candidate");
        assert_eq!(fs::read(bin_dir.join("lu.prev")).unwrap(), b"known good");
        let hidden: Vec<String> = fs::read_dir(&bin_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with('.'))
            .collect();
        assert!(hidden.is_empty(), "orphaned temp files: {hidden:?}");
    }

    #[cfg(unix)]
    #[test]
    fn check_bin_dir_rejects_symlink_and_writable_prefix() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let td = tempfile::tempdir().unwrap();
        let real = td.path().join("real");
        fs::create_dir_all(&real).unwrap();
        let link = td.path().join("link");
        symlink(&real, &link).unwrap();
        let err = check_bin_dir(&link).unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");

        fs::set_permissions(&real, fs::Permissions::from_mode(0o777)).unwrap();
        let err = check_bin_dir(&real).unwrap_err();
        assert!(err.to_string().contains("group/other-writable"), "{err}");
    }
}
