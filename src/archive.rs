use crate::error::{Error, Result};
use flate2::read::GzDecoder;
use std::fs;
use std::io::Read;
use std::path::{Component as PathComponent, Path, PathBuf};
use tar::Archive;

#[derive(Debug, Clone)]
pub struct ExtractedBinary {
    pub name: String,
    pub path: PathBuf,
}

pub fn extract_binaries(
    archive_path: &Path,
    expected_binaries: &[String],
    dest_dir: &Path,
) -> Result<Vec<ExtractedBinary>> {
    let file = fs::File::open(archive_path).map_err(|source| Error::Read {
        path: archive_path.to_path_buf(),
        source,
    })?;
    let gz = GzDecoder::new(file);
    let mut archive = Archive::new(gz);
    let entries = archive
        .entries()
        .map_err(|e| Error::Archive(format!("failed to read archive entries: {e}")))?;

    let mut manifest_paths: Option<Vec<String>> = None;
    // Scan entries in one pass, storing regular files. Manifest, if present, informs layout.
    let mut pending: Vec<PendingEntry> = Vec::new();

    for entry in entries {
        let mut entry = entry.map_err(|e| Error::Archive(format!("invalid archive entry: {e}")))?;
        let header = entry.header().clone();
        let entry_type = header.entry_type();
        let path = entry
            .path()
            .map_err(|e| Error::Archive(format!("invalid entry path: {e}")))?
            .to_path_buf();
        let path_str = path.to_string_lossy().to_string();

        // Reject unsafe types early
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            return Err(Error::Archive(format!(
                "archive contains forbidden link: {path_str}"
            )));
        }
        if entry_type.is_block_special()
            || entry_type.is_character_special()
            || entry_type.is_fifo()
        {
            return Err(Error::Archive(format!(
                "archive contains unsupported entry type: {path_str}"
            )));
        }
        validate_entry_path(&path)?;

        if entry_type.is_file() {
            let mut data = Vec::new();
            entry.read_to_end(&mut data).map_err(|e| {
                Error::Archive(format!("failed to read archive entry {path_str}: {e}"))
            })?;
            pending.push(PendingEntry {
                path: path.clone(),
                data,
            });
        }
    }

    // Discover manifest
    for e in &pending {
        if is_manifest_path(&e.path) {
            if let Ok(text) = std::str::from_utf8(&e.data) {
                if let Ok(m) = parse_manifest(text) {
                    manifest_paths = Some(m);
                    break;
                }
            }
        }
    }

    let allowed: Vec<String> = if let Some(mpaths) = manifest_paths {
        // manifest gives paths/names - only extract those that match expected binaries by basename
        mpaths
            .into_iter()
            .filter(|p| {
                let base = Path::new(p)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                expected_binaries.iter().any(|b| b == &base)
            })
            .collect()
    } else {
        Vec::new()
    };

    fs::create_dir_all(dest_dir).map_err(|source| Error::Write {
        path: dest_dir.to_path_buf(),
        source,
    })?;

    let mut extracted = Vec::new();
    for e in pending {
        let base = e
            .path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if !expected_binaries.iter().any(|b| b == &base) {
            continue;
        }
        if !allowed.is_empty() {
            let path_str = e.path.to_string_lossy().to_string();
            let path_str_norm = path_str.trim_start_matches("./");
            let matched = allowed.iter().any(|a| {
                let a_norm = a.trim_start_matches("./");
                a_norm == path_str_norm
                    || Path::new(a_norm).file_name() == Some(std::ffi::OsStr::new(base.as_str()))
            });
            if !matched {
                continue;
            }
        }
        // Avoid extracting duplicates ambiguously
        if extracted.iter().any(|x: &ExtractedBinary| x.name == base) {
            return Err(Error::Archive(format!(
                "archive contains multiple entries for binary '{base}'"
            )));
        }
        let out_path = dest_dir.join(&base);
        fs::write(&out_path, &e.data).map_err(|source| Error::Write {
            path: out_path.clone(),
            source,
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&out_path)
                .map_err(|source| Error::Write {
                    path: out_path.clone(),
                    source,
                })?
                .permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&out_path, perms).map_err(|source| Error::Write {
                path: out_path.clone(),
                source,
            })?;
        }
        extracted.push(ExtractedBinary {
            name: base,
            path: out_path,
        });
    }

    Ok(extracted)
}

struct PendingEntry {
    path: PathBuf,
    data: Vec<u8>,
}

fn validate_entry_path(path: &Path) -> Result<()> {
    if path.is_absolute() {
        return Err(Error::Archive(format!(
            "archive contains absolute path: {}",
            path.display()
        )));
    }
    for comp in path.components() {
        match comp {
            PathComponent::ParentDir => {
                return Err(Error::Archive(format!(
                    "archive contains path traversal: {}",
                    path.display()
                )))
            }
            PathComponent::Prefix(_) => {
                return Err(Error::Archive(format!(
                    "archive contains invalid path prefix: {}",
                    path.display()
                )))
            }
            _ => {}
        }
    }
    Ok(())
}

fn is_manifest_path(path: &Path) -> bool {
    let name = path.file_name().map(|s| s.to_string_lossy().to_lowercase());
    matches!(name.as_deref(), Some("manifest.json") | Some("manifest"))
        || path
            .to_string_lossy()
            .to_lowercase()
            .ends_with("/manifest.json")
}

fn parse_manifest(text: &str) -> std::result::Result<Vec<String>, ()> {
    // Try JSON: {"binaries": ["lu", ...]} or {"files": [...]} or {"paths":[...]} or array
    let v: serde_json::Value = serde_json::from_str(text).map_err(|_| ())?;
    if let Some(arr) = v.as_array() {
        return Ok(arr
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect());
    }
    for key in ["binaries", "files", "paths", "bin"] {
        if let Some(arr) = v.get(key).and_then(|x| x.as_array()) {
            let mut out = Vec::new();
            for item in arr {
                if let Some(s) = item.as_str() {
                    out.push(s.to_string());
                } else if let Some(p) = item.get("path").and_then(|x| x.as_str()) {
                    out.push(p.to_string());
                } else if let Some(n) = item.get("name").and_then(|x| x.as_str()) {
                    out.push(n.to_string());
                }
            }
            if !out.is_empty() {
                return Ok(out);
            }
        }
    }
    Err(())
}
