use crate::error::{Error, Result};
use crate::http;
use std::fs::File;
use std::path::{Path, PathBuf};

pub struct Downloaded {
    pub archive_path: PathBuf,
    pub checksum_path: PathBuf,
}

pub fn download_release_assets(
    agent: &ureq::Agent,
    archive_url: &str,
    checksum_url: &str,
    archive_name: &str,
    checksum_name: &str,
    dest_dir: &Path,
    max_bytes: u64,
) -> Result<Downloaded> {
    let archive_path = dest_dir.join(archive_name);
    let checksum_path = dest_dir.join(checksum_name);
    download_file(agent, archive_url, &archive_path, max_bytes)?;
    download_file(agent, checksum_url, &checksum_path, 1024 * 1024)?;
    Ok(Downloaded {
        archive_path,
        checksum_path,
    })
}

pub fn download_file(agent: &ureq::Agent, url: &str, path: &Path, max_bytes: u64) -> Result<u64> {
    let file = File::create(path).map_err(|source| Error::Write {
        path: path.to_path_buf(),
        source,
    })?;
    let bytes = http::download_to_writer(agent, url, file, max_bytes)?;
    Ok(bytes)
}

pub fn read_checksum_file(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.to_path_buf(),
        source,
    })
}
