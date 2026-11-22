use crate::error::{Error, Result};
use flate2::read::GzDecoder;
use std::fs;
use std::io::Read;
use std::path::{Component as PathComponent, Path, PathBuf};
use tar::Archive;

/// A compressed asset may be small while its entries expand without bound.
/// These caps bound work done while listing and extracting an archive.
pub const MAX_ARCHIVE_ENTRIES: u64 = 4_096;
pub const MAX_ENTRY_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ExtractedBinary {
    pub name: String,
    pub path: PathBuf,
}

fn check_entry_count(index: u64, max_entries: u64) -> Result<()> {
    if index >= max_entries {
        return Err(Error::Archive(format!(
            "archive exceeds maximum of {max_entries} entries"
        )));
    }
    Ok(())
}

/// List the regular-file basenames in an archive without extracting anything.
/// Used to decide whether a release actually ships a component's binaries.
pub fn list_binary_names(archive_path: &Path) -> Result<Vec<String>> {
    list_binary_names_limited(archive_path, MAX_ARCHIVE_ENTRIES)
}

fn list_binary_names_limited(archive_path: &Path, max_entries: u64) -> Result<Vec<String>> {
    let file = fs::File::open(archive_path).map_err(|source| Error::Read {
        path: archive_path.to_path_buf(),
        source,
    })?;
    let mut archive = Archive::new(GzDecoder::new(file));
    let entries = archive
        .entries()
        .map_err(|e| Error::Archive(format!("failed to read archive entries: {e}")))?;
    let mut names = Vec::new();
    for (index, entry) in entries.enumerate() {
        check_entry_count(index as u64, max_entries)?;
        let entry = entry.map_err(|e| Error::Archive(format!("invalid archive entry: {e}")))?;
        let entry_type = entry.header().entry_type();
        let path = entry
            .path()
            .map_err(|e| Error::Archive(format!("invalid entry path: {e}")))?
            .to_path_buf();
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            return Err(Error::Archive(format!(
                "archive contains forbidden link: {}",
                path.display()
            )));
        }
        validate_entry_path(&path)?;
        if entry_type.is_file() {
            if let Some(name) = path.file_name().map(|s| s.to_string_lossy().into_owned()) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    Ok(names)
}

pub fn extract_binaries(
    archive_path: &Path,
    expected_binaries: &[String],
    dest_dir: &Path,
) -> Result<Vec<ExtractedBinary>> {
    extract_binaries_with_limits(
        archive_path,
        expected_binaries,
        dest_dir,
        MAX_ENTRY_BYTES,
        MAX_TOTAL_BYTES,
        MAX_ARCHIVE_ENTRIES,
    )
}

fn extract_binaries_with_limits(
    archive_path: &Path,
    expected_binaries: &[String],
    dest_dir: &Path,
    max_entry_bytes: u64,
    max_total_bytes: u64,
    max_entries: u64,
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
    // Scan entries in one pass, storing only candidate binaries and the
    // manifest. The tar iterator skips data for entries we do not read.
    let mut pending: Vec<PendingEntry> = Vec::new();
    let mut total: u64 = 0;

    for (index, entry) in entries.enumerate() {
        check_entry_count(index as u64, max_entries)?;
        let entry = entry.map_err(|e| Error::Archive(format!("invalid archive entry: {e}")))?;
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
            let base = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let wanted =
                is_manifest_path(&path) || expected_binaries.iter().any(|name| name == &base);
            if !wanted {
                continue;
            }
            let mut data = Vec::new();
            entry
                .take(max_entry_bytes.saturating_add(1))
                .read_to_end(&mut data)
                .map_err(|e| {
                    Error::Archive(format!("failed to read archive entry {path_str}: {e}"))
                })?;
            if data.len() as u64 > max_entry_bytes {
                return Err(Error::Archive(format!(
                    "archive entry {path_str} exceeds decompressed size limit of {max_entry_bytes} bytes"
                )));
            }
            total = total.saturating_add(data.len() as u64);
            if total > max_total_bytes {
                return Err(Error::Archive(format!(
                    "archive exceeds total decompressed size limit of {max_total_bytes} bytes"
                )));
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use tar::{Builder, Header};

    fn write_archive(path: &Path, files: &[(&str, &[u8])]) {
        let file = fs::File::create(path).unwrap();
        let gz = GzEncoder::new(file, Compression::fast());
        let mut builder = Builder::new(gz);
        for (name, data) in files {
            let mut header = Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, name, *data).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn extracts_expected_binary_within_limits() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.gz");
        write_archive(&archive, &[("bin/ting", b"hello")]);
        let dest = dir.path().join("out");
        let expected = vec!["ting".to_string()];
        let got = extract_binaries_with_limits(&archive, &expected, &dest, 32, 64, 10).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "ting");
        assert_eq!(fs::read(&got[0].path).unwrap(), b"hello");
    }

    #[test]
    fn rejects_oversized_entry() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.gz");
        write_archive(&archive, &[("ting", b"abcdefgh")]);
        let expected = vec!["ting".to_string()];
        let err =
            extract_binaries_with_limits(&archive, &expected, &dir.path().join("out"), 4, 64, 10)
                .unwrap_err();
        assert!(err.to_string().contains("decompressed size limit"), "{err}");
    }

    #[test]
    fn rejects_oversized_total() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.gz");
        write_archive(&archive, &[("ting", b"abcdef"), ("lu", b"ghijkl")]);
        let expected = vec!["ting".to_string(), "lu".to_string()];
        let err =
            extract_binaries_with_limits(&archive, &expected, &dir.path().join("out"), 8, 10, 10)
                .unwrap_err();
        assert!(
            err.to_string().contains("total decompressed size limit"),
            "{err}"
        );
    }

    #[test]
    fn ignores_unwanted_oversized_entries() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.gz");
        write_archive(&archive, &[("docs/notes.txt", b"a".repeat(64).as_slice())]);
        let expected = vec!["ting".to_string()];
        let got =
            extract_binaries_with_limits(&archive, &expected, &dir.path().join("out"), 4, 8, 10)
                .unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn rejects_too_many_entries_when_extracting() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.gz");
        write_archive(&archive, &[("a", b""), ("b", b""), ("c", b"")]);
        let err = extract_binaries_with_limits(&archive, &[], &dir.path().join("out"), 16, 16, 2)
            .unwrap_err();
        assert!(err.to_string().contains("maximum of 2 entries"), "{err}");
    }

    #[test]
    fn rejects_too_many_entries_when_listing() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.gz");
        write_archive(&archive, &[("a", b""), ("b", b""), ("c", b"")]);
        let err = list_binary_names_limited(&archive, 2).unwrap_err();
        assert!(err.to_string().contains("maximum of 2 entries"), "{err}");
    }
}
