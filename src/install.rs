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
        &tmp,
        crate::http::max_download_bytes(),
    );
    let result = downloaded.and_then(|d| {
        let content = download::read_checksum_file(&d.checksum_path)?;
        let expected = verify::parse_checksum_file(&content, &asset_name)?;
        verify::verify_checksum(&d.archive_path, &expected)?;
        archive::list_binary_names(&d.archive_path)
    });
    let _ = fs::remove_dir_all(&tmp);
    result
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
        &tmp,
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
    if let Err(e) = opts.verifier.verify(&request) {
        if opts.allow_unattested {
            eprintln!(
                "tuishou: attestation verification failed; continuing because --allow-unattested was set: {}",
                crate::http::redact_secrets(&e.to_string())
            );
        } else {
            return Err(e);
        }
    }

    let extract_dir = tmp.join("extract");
    let extracted =
        archive::extract_binaries(&downloaded.archive_path, &expected_binaries, &extract_dir)?;
    if extracted.is_empty() {
        return Err(Error::Archive(format!(
            "archive {} contained no expected binaries ({})",
            asset_name,
            expected_binaries.join(", ")
        )));
    }

    let bin_dir = install_dir(opts.prefix.as_deref());
    fs::create_dir_all(&bin_dir).map_err(|source| Error::Write {
        path: bin_dir.clone(),
        source,
    })?;

    let installed_names: Vec<String> = extracted.iter().map(|b| b.name.clone()).collect();
    if opts.smoke_check {
        // Validate the staged binaries in place: a failed smoke check must
        // leave the installed binaries and their `.prev` files untouched.
        smoke_check(&extract_dir, &installed_names)?;
    }

    let receipt = InstallReceipt {
        checksum: expected,
        target: target.as_str().to_string(),
        installed_at: state::now_rfc3339()?,
    };
    commit_binaries(&extracted, &bin_dir)?;
    if opts.commit_state {
        record_installed(component, &tag, &receipt, opts.prefix.as_deref())?;
    }

    let _ = fs::remove_dir_all(&tmp);

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
    state::save(&state_path, &st)
}

/// Commit every staged binary, restoring destinations and the last good
/// `.prev` if any step fails. `.prev` is only replaced after all new binaries
/// are in place.
fn commit_binaries(extracted: &[archive::ExtractedBinary], bin_dir: &Path) -> Result<()> {
    fs::create_dir_all(bin_dir).map_err(|source| Error::Write {
        path: bin_dir.to_path_buf(),
        source,
    })?;

    struct Pending {
        dest: PathBuf,
        staged: PathBuf,
        dest_backup: Option<PathBuf>,
        prev: PathBuf,
        prev_backup: Option<PathBuf>,
    }

    let mut pending: Vec<Pending> = Vec::new();
    let result = (|| {
        for bin in extracted {
            let dest = bin_dir.join(&bin.name);
            let prev = bin_dir.join(format!("{}.prev", bin.name));
            let dest_backup = if dest.exists() {
                let path = unique_temp(bin_dir, &format!(".{}.backup-", bin.name));
                fs::copy(&dest, &path).map_err(|source| Error::Write {
                    path: path.clone(),
                    source,
                })?;
                Some(path)
            } else {
                None
            };
            let prev_backup = if prev.exists() {
                let path = unique_temp(bin_dir, &format!(".{}.prevkeep-", bin.name));
                fs::copy(&prev, &path).map_err(|source| Error::Write {
                    path: path.clone(),
                    source,
                })?;
                Some(path)
            } else {
                None
            };
            let staged = unique_temp(bin_dir, &format!(".{}.commit-", bin.name));
            fs::copy(&bin.path, &staged).map_err(|source| Error::Write {
                path: staged.clone(),
                source,
            })?;
            make_executable(&staged)?;
            pending.push(Pending {
                dest,
                staged,
                dest_backup,
                prev,
                prev_backup,
            });
        }
        for item in &pending {
            fs::rename(&item.staged, &item.dest).map_err(|source| Error::Write {
                path: item.dest.clone(),
                source,
            })?;
        }
        for item in &pending {
            if let Some(backup) = &item.dest_backup {
                fs::rename(backup, &item.prev).map_err(|source| Error::Write {
                    path: item.prev.clone(),
                    source,
                })?;
            }
        }
        Ok(())
    })();

    if let Err(e) = result {
        // Restore the last good `.prev` first, then the installed binaries.
        for item in pending.iter().rev() {
            match &item.prev_backup {
                Some(backup) => {
                    let _ = fs::rename(backup, &item.prev);
                }
                None => {
                    let _ = fs::remove_file(&item.prev);
                }
            }
        }
        for item in pending.iter().rev() {
            match &item.dest_backup {
                Some(backup) => {
                    let _ = fs::rename(backup, &item.dest);
                }
                None => {
                    let _ = fs::remove_file(&item.dest);
                }
            }
        }
        for item in &pending {
            let _ = fs::remove_file(&item.staged);
        }
        return Err(e);
    }

    for item in &pending {
        if let Some(backup) = &item.prev_backup {
            let _ = fs::remove_file(backup);
        }
    }
    Ok(())
}

/// Restore `<name>.prev` over the installed binary, keeping the last known
/// good `.prev` in place so a later retry cannot overwrite it with a bad build.
pub fn restore_previous_binary(bin_dir: &Path, name: &str) -> Result<()> {
    let dest = bin_dir.join(name);
    let prev = bin_dir.join(format!("{name}.prev"));
    if !prev.exists() {
        return Err(Error::NoPrevious(name.to_string()));
    }
    let tmp = unique_temp(bin_dir, &format!(".{name}.restore-"));
    fs::copy(&prev, &tmp).map_err(|source| Error::Write {
        path: tmp.clone(),
        source,
    })?;
    make_executable(&tmp)?;
    fs::rename(&tmp, &dest).map_err(|source| Error::Write {
        path: dest.clone(),
        source,
    })?;
    // Refresh `.prev` from the restored good binary (same bytes).
    let keep = unique_temp(bin_dir, &format!(".{name}.keep-"));
    fs::copy(&dest, &keep).map_err(|source| Error::Write {
        path: keep.clone(),
        source,
    })?;
    fs::rename(&keep, &prev).map_err(|source| Error::Write { path: prev, source })?;
    Ok(())
}

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)
            .map_err(|source| Error::Write {
                path: path.to_path_buf(),
                source,
            })?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).map_err(|source| Error::Write {
            path: path.to_path_buf(),
            source,
        })?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn unique_temp(dir: &Path, prefix: &str) -> PathBuf {
    let nanos = now_nanos();
    dir.join(format!("{prefix}{}-{nanos}", std::process::id()))
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

fn tempfile_dir() -> Result<PathBuf> {
    let base = std::env::temp_dir().join("tuishou");
    fs::create_dir_all(&base).map_err(|source| Error::Write {
        path: base.clone(),
        source,
    })?;
    let dir = base.join(format!("dl-{}-{}", std::process::id(), now_nanos()));
    fs::create_dir_all(&dir).map_err(|source| Error::Write {
        path: dir.clone(),
        source,
    })?;
    Ok(dir)
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

pub fn major_version(tag: &str) -> Option<u64> {
    let v = tag.strip_prefix('v').unwrap_or(tag);
    let major = v.split('.').next()?;
    major.parse().ok()
}
