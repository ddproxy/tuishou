use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledComponent {
    pub version: String,
    pub target: String,
    pub checksum: String,
    pub installed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct State {
    #[serde(default)]
    pub components: BTreeMap<String, InstalledComponent>,
}

pub fn default_state_path(prefix: Option<&Path>) -> PathBuf {
    if let Some(prefix) = prefix {
        return prefix.join(".tuishou.json");
    }
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("tuishou").join("installed.json");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home)
                .join(".local")
                .join("state")
                .join("tuishou")
                .join("installed.json");
        }
    }
    PathBuf::from(".tuishou.json")
}

pub fn load(path: &Path) -> Result<State> {
    if !path.exists() {
        return Ok(State::default());
    }
    let text = std::fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&text)
        .map_err(|e| Error::msg(format!("invalid state file {}: {e}", path.display())))
}

pub fn save(path: &Path, state: &State) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let text = serde_json::to_string_pretty(state)
        .map_err(|e| Error::msg(format!("failed to serialize state: {e}")))?;
    atomic_write(path, format!("{text}\n").as_bytes())
}

/// Write through a unique temp file, fsync the data, rename into place and
/// fsync the directory. Unique names avoid collisions between concurrent
/// writers using the same state path.
fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    let tmp = unique_temp_path(path);
    {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp).map_err(|source| Error::Write {
            path: tmp.clone(),
            source,
        })?;
        file.write_all(content)
            .and_then(|()| file.sync_all())
            .map_err(|source| Error::Write {
                path: tmp.clone(),
                source,
            })?;
    }
    fs::rename(&tmp, path).map_err(|source| Error::Write {
        path: path.to_path_buf(),
        source,
    })?;
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

fn unique_temp_path(path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "state".to_string());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(".{name}.tmp-{}-{nanos}-{seq}", std::process::id());
    match path.parent() {
        Some(parent) => parent.join(tmp_name),
        None => PathBuf::from(tmp_name),
    }
}

pub fn now_rfc3339() -> Result<String> {
    let now = OffsetDateTime::now_utc();
    now.format(&Rfc3339)
        .map_err(|e| Error::msg(format!("failed to format time: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("state.json");
        let mut st = State::default();
        st.components.insert(
            "lu".to_string(),
            InstalledComponent {
                version: "v0.4.0".to_string(),
                target: "x86_64-unknown-linux-musl".to_string(),
                checksum: "a".repeat(64),
                installed_at: "2026-01-01T00:00:00Z".to_string(),
                previous_version: Some("v0.3.0".to_string()),
            },
        );
        save(&path, &st).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(st, loaded);
    }

    #[test]
    fn saves_use_unique_temp_names_and_leave_no_temp_files() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("state.json");
        assert_ne!(unique_temp_path(&path), unique_temp_path(&path));
        assert_eq!(unique_temp_path(&path).parent(), Some(td.path()));
        save(&path, &State::default()).unwrap();
        save(&path, &State::default()).unwrap();
        assert!(!path.with_extension("json.tmp").exists());
        let leftovers: Vec<_> = fs::read_dir(td.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[cfg(unix)]
    #[test]
    fn state_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("state.json");
        save(&path, &State::default()).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
