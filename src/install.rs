use crate::archive;
use crate::component::Component;
use crate::download;
use crate::error::{Error, Result};
use crate::release;
use crate::state::{self, InstalledComponent};
use crate::target::Target;
use crate::verify;
use std::fs;
use std::path::{Path, PathBuf};

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
}

pub struct InstallOutcome {
    pub component: String,
    pub version: String,
    pub binaries: Vec<String>,
    pub dry_run: bool,
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
    let asset_name = component.asset_name(&tag, target.as_str());
    let checksum_name = component.checksum_asset_name(&tag, target.as_str());
    let asset = release::find_asset(&rel, &asset_name)?;
    let checksum_asset = release::find_asset(&rel, &checksum_name)?;

    let state_path = state::default_state_path(opts.prefix.as_deref());
    let mut st = state::load(&state_path)?;

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
            binaries: component.binaries.clone(),
            dry_run: true,
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
    verify::verify_attestation(
        &downloaded.archive_path,
        &component.repo,
        opts.allow_unattested,
    )?;

    let extract_dir = tmp.join("extract");
    let extracted =
        archive::extract_binaries(&downloaded.archive_path, &component.binaries, &extract_dir)?;
    if extracted.is_empty() {
        return Err(Error::Archive(format!(
            "archive {} contained no expected binaries ({})",
            asset_name,
            component.binaries.join(", ")
        )));
    }

    let bin_dir = install_dir(opts.prefix.as_deref());
    fs::create_dir_all(&bin_dir).map_err(|source| Error::Write {
        path: bin_dir.clone(),
        source,
    })?;

    let mut installed_names = Vec::new();
    for bin in &extracted {
        install_binary(&bin.path, &bin_dir, &bin.name)?;
        installed_names.push(bin.name.clone());
    }

    let prev_version = st
        .components
        .get(&component.name)
        .map(|c| c.version.clone());
    st.components.insert(
        component.name.clone(),
        InstalledComponent {
            version: tag.clone(),
            target: target.as_str().to_string(),
            checksum: expected,
            installed_at: state::now_rfc3339()?,
            previous_version: prev_version,
        },
    );
    state::save(&state_path, &st)?;

    let _ = fs::remove_dir_all(&tmp);

    if !opts.quiet {
        println!("installed {} {} ({})", component.name, tag, target.as_str());
    }
    Ok(InstallOutcome {
        component: component.name.clone(),
        version: tag,
        binaries: installed_names,
        dry_run: false,
    })
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

fn install_binary(src: &Path, bin_dir: &Path, name: &str) -> Result<()> {
    let dest = bin_dir.join(name);
    let prev = bin_dir.join(format!("{name}.prev"));
    let tmp = bin_dir.join(format!(".{name}.tmp-{}", std::process::id()));

    fs::copy(src, &tmp).map_err(|source| Error::Write {
        path: tmp.clone(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&tmp)
            .map_err(|source| Error::Write {
                path: tmp.clone(),
                source,
            })?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&tmp, perms).map_err(|source| Error::Write {
            path: tmp.clone(),
            source,
        })?;
    }

    if dest.exists() {
        // Copy current to .prev first so a failed rename leaves dest in place.
        if prev.exists() {
            let _ = fs::remove_file(&prev);
        }
        fs::copy(&dest, &prev).map_err(|source| Error::Write {
            path: prev.clone(),
            source,
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = fs::metadata(&prev) {
                let mut perms = meta.permissions();
                perms.set_mode(0o755);
                let _ = fs::set_permissions(&prev, perms);
            }
        }
    }

    fs::rename(&tmp, &dest).map_err(|source| {
        let _ = fs::remove_file(&tmp);
        Error::Write {
            path: dest.clone(),
            source,
        }
    })?;
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
