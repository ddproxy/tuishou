use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
#[cfg(all(unix, feature = "top"))]
use std::io::Read;
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

/// Host TUI state lives beside install state, in its own file so `top` never
/// rewrites the installer's state. With a prefix it stays under that prefix.
#[cfg(feature = "top")]
pub fn top_state_path(prefix: Option<&Path>) -> PathBuf {
    if let Some(prefix) = prefix {
        return prefix.join(".tuishou-top.json");
    }
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("tuishou").join("top.json");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home)
                .join(".local")
                .join("state")
                .join("tuishou")
                .join("top.json");
        }
    }
    PathBuf::from(".tuishou-top.json")
}

/// Scope TUI state to the config that produced it as well as the install
/// prefix. A different config must never inherit argv saved by another one.
#[cfg(feature = "top")]
pub fn top_state_path_for_config(prefix: Option<&Path>, config_path: Option<&Path>) -> PathBuf {
    use sha2::{Digest, Sha256};

    let base = top_state_path(prefix);
    let key = config_path
        .map(|path| path.as_os_str().as_encoded_bytes())
        .unwrap_or(b"default");
    let digest = Sha256::digest(key);
    let suffix = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let stem = base
        .file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "top".to_string());
    base.with_file_name(format!("{stem}-{suffix}.json"))
}

#[cfg(all(unix, feature = "top"))]
const MAX_PRIVATE_STATE_BYTES: u64 = 1024 * 1024;

/// Read a private state file from one checked descriptor: no symlink, no
/// foreign owner, no group/other write bit and a bounded size. Unsafe or
/// unreadable state is treated as absent by callers.
#[cfg(all(unix, feature = "top"))]
pub fn read_private_string(path: &Path) -> Option<String> {
    use rustix::fs::{open, Mode, OFlags};
    use std::os::unix::fs::MetadataExt;

    let fd = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    let mut file: File = fd.into();
    let meta = file.metadata().ok()?;
    if !meta.file_type().is_file() || meta.len() > MAX_PRIVATE_STATE_BYTES {
        return None;
    }
    if meta.mode() & 0o022 != 0 {
        return None;
    }
    if let Some(uid) = crate::config::current_uid() {
        if meta.uid() != uid && meta.uid() != 0 {
            return None;
        }
    }
    let mut text = String::new();
    file.read_to_string(&mut text).ok()?;
    Some(text)
}

#[cfg(all(not(unix), feature = "top"))]
pub fn read_private_string(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

/// Side-by-side record of the checksums of verified-good `.prev` binaries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KnownGood {
    #[serde(default)]
    pub checksums: BTreeMap<String, String>,
}

pub fn known_good_path(state_path: &Path) -> PathBuf {
    state_path.with_extension("known-good.json")
}

pub fn load_known_good(state_path: &Path) -> Result<KnownGood> {
    let path = known_good_path(state_path);
    if !path.exists() {
        return Ok(KnownGood::default());
    }
    let text = fs::read_to_string(&path).map_err(|source| Error::Read {
        path: path.clone(),
        source,
    })?;
    serde_json::from_str(&text)
        .map_err(|e| Error::msg(format!("invalid known-good record {}: {e}", path.display())))
}

pub fn save_known_good(state_path: &Path, record: &KnownGood) -> Result<()> {
    let text = serde_json::to_string_pretty(record)
        .map_err(|e| Error::msg(format!("failed to serialize known-good record: {e}")))?;
    atomic_write(&known_good_path(state_path), format!("{text}\n").as_bytes())
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
    save_value(path, state)
}

/// Atomic write for auxiliary state files with the same permissions and
/// durability as the install state file.
pub fn save_value<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let text = serde_json::to_string_pretty(value)
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

    #[cfg(feature = "top")]
    #[test]
    fn top_state_path_uses_state_dir_and_prefix() {
        let _g = crate::test_env_lock();
        let td = tempfile::tempdir().unwrap();
        let old = std::env::var_os("XDG_STATE_HOME");
        // Env mutation is serialised by test_env_lock.
        std::env::set_var("XDG_STATE_HOME", td.path());
        assert_eq!(
            top_state_path(None),
            td.path().join("tuishou").join("top.json")
        );
        assert_eq!(
            top_state_path(Some(Path::new("/opt/bin"))),
            PathBuf::from("/opt/bin/.tuishou-top.json")
        );
        match old {
            Some(value) => std::env::set_var("XDG_STATE_HOME", value),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
    }

    #[cfg(feature = "top")]
    #[test]
    fn top_state_path_is_scoped_to_the_config() {
        let _g = crate::test_env_lock();
        let td = tempfile::tempdir().unwrap();
        let old = std::env::var_os("XDG_STATE_HOME");
        std::env::set_var("XDG_STATE_HOME", td.path());
        let first = top_state_path_for_config(None, Some(Path::new("/etc/first.toml")));
        let second = top_state_path_for_config(None, Some(Path::new("/etc/second.toml")));
        assert_ne!(first, second);
        assert_eq!(first.parent(), second.parent());
        match old {
            Some(value) => std::env::set_var("XDG_STATE_HOME", value),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
    }

    #[cfg(all(unix, feature = "top"))]
    #[test]
    fn private_state_ignores_symlinks_and_writable_files() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("state.json");
        fs::write(&path, "trusted").unwrap();
        assert_eq!(read_private_string(&path).as_deref(), Some("trusted"));

        let link = td.path().join("link.json");
        symlink(&path, &link).unwrap();
        assert_eq!(read_private_string(&link), None);

        fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).unwrap();
        assert_eq!(read_private_string(&path), None);
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
