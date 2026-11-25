//! State for scheduled auto-update. Everything lives in one 0700 directory.

use crate::error::{Error, Result};
use crate::release::Release;
use rustix::fs::{flock, FlockOperation};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AutoState {
    #[serde(default)]
    pub last_check: Option<String>,
    #[serde(default)]
    pub results: BTreeMap<String, ComponentResult>,
    #[serde(default)]
    pub bad_versions: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub failures: BTreeMap<String, u32>,
    #[serde(default)]
    pub caches: BTreeMap<String, ReleaseCache>,
    /// Checksum of the controller DB backup kept for manual restore.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_backup: Option<ControllerBackup>,
    /// A self-update waiting for its first scheduled pass to start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_pending: Option<SelfPending>,
}

/// A self-update whose first scheduled pass has not confirmed the new binary
/// yet. The recorded path is the binary that the timer will start next.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfPending {
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<String>,
    pub path: PathBuf,
    pub at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControllerBackup {
    pub path: PathBuf,
    pub checksum: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentResult {
    /// Fixed-vocabulary status phrase from `report::StatusText`.
    pub result: String,
    /// Machine-readable form of `result`, used by `auto-update status`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_eligible_version: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_binaries: Vec<String>,
    pub at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseCache {
    pub etag: Option<String>,
    pub checked_at: String,
    pub releases: Vec<Release>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hold {
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

pub fn state_dir(prefix: Option<&Path>) -> PathBuf {
    if let Some(prefix) = prefix {
        return prefix.join(".tuishou");
    }
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("tuishou");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home)
                .join(".local")
                .join("state")
                .join("tuishou");
        }
    }
    PathBuf::from(".tuishou")
}

pub fn ensure_state_dir(prefix: Option<&Path>) -> Result<PathBuf> {
    let dir = state_dir(prefix);
    if !dir.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder.create(&dir).map_err(|source| Error::Write {
                path: dir.clone(),
                source,
            })?;
        }
        #[cfg(not(unix))]
        fs::create_dir_all(&dir).map_err(|source| Error::Write {
            path: dir.clone(),
            source,
        })?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    Ok(dir)
}

pub fn state_path(prefix: Option<&Path>) -> PathBuf {
    state_dir(prefix).join("auto.json")
}

pub fn hold_path(prefix: Option<&Path>) -> PathBuf {
    state_dir(prefix).join("HOLD")
}

pub fn lock_path(prefix: Option<&Path>) -> PathBuf {
    state_dir(prefix).join("lock")
}

/// An OS advisory lock. The kernel releases it when the process exits (or is
/// killed), so a stale lock can never block later cycles after a reboot.
pub struct CycleLock {
    file: File,
}

impl CycleLock {
    pub fn acquire(prefix: Option<&Path>) -> Result<Self> {
        let path = lock_path(prefix);
        let _ = ensure_state_dir(prefix)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| Error::Write {
                path: path.clone(),
                source,
            })?;
        flock(&file, FlockOperation::LockExclusive).map_err(|source| Error::Write {
            path: path.clone(),
            source: source.into(),
        })?;
        Ok(Self { file })
    }

    /// Non-blocking variant: a concurrent cycle exits quietly instead of
    /// waiting, and the OS releases the lock if the holder dies.
    pub fn try_acquire(prefix: Option<&Path>) -> Result<Option<Self>> {
        let path = lock_path(prefix);
        let _ = ensure_state_dir(prefix)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| Error::Write {
                path: path.clone(),
                source,
            })?;
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Some(Self { file })),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(source) => Err(Error::Write {
                path,
                source: source.into(),
            }),
        }
    }
}

impl Drop for CycleLock {
    fn drop(&mut self) {
        let _ = flock(&self.file, FlockOperation::Unlock);
    }
}

/// Run a closure while holding the same cycle lock used by `auto-update run`.
/// All state readers/writers that modify state must go through this helper.
pub fn with_cycle_lock<T>(prefix: Option<&Path>, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let _guard = CycleLock::acquire(prefix)?;
    f()
}

pub fn load(prefix: Option<&Path>) -> Result<AutoState> {
    let path = state_path(prefix);
    if !path.exists() {
        return Ok(AutoState::default());
    }
    let text = fs::read_to_string(&path).map_err(|source| Error::Read {
        path: path.clone(),
        source,
    })?;
    serde_json::from_str(&text)
        .map_err(|e| Error::msg(format!("invalid auto-update state {}: {e}", path.display())))
}

pub fn save(prefix: Option<&Path>, state: &AutoState) -> Result<()> {
    let _ = ensure_state_dir(prefix)?;
    let text = serde_json::to_string_pretty(state)
        .map_err(|e| Error::msg(format!("failed to serialize auto-update state: {e}")))?;
    atomic_write(&state_path(prefix), format!("{text}\n").as_bytes())
}

/// Write through a unique temp file, fsync the data, rename into place and
/// fsync the directory. Unique names avoid the previous collision between two
/// writers that both used `<state dir>/auto.json.tmp`.
fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let tmp = unique_temp_path(path);
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|source| Error::Write {
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
        // Directory fsync is best-effort: some filesystems reject it.
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

/// Parse a hold file. Plain text is accepted as a reason for hand-written
/// files; the CLI writes JSON so expiry can be recorded.
pub fn parse_hold(text: &str) -> Hold {
    if let Ok(hold) = serde_json::from_str::<Hold>(text) {
        return hold;
    }
    Hold {
        created_at: String::new(),
        reason: if text.trim().is_empty() {
            None
        } else {
            Some(crate::logging::one_line(text.trim()))
        },
        expires_at: None,
    }
}

pub fn read_hold(prefix: Option<&Path>) -> Result<Option<Hold>> {
    let path = hold_path(prefix);
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|source| Error::Read {
        path: path.clone(),
        source,
    })?;
    Ok(Some(parse_hold(&text)))
}

fn hold_expired(hold: &Hold) -> Result<bool> {
    let Some(expires) = &hold.expires_at else {
        return Ok(false);
    };
    let until = OffsetDateTime::parse(expires, &Rfc3339)
        .map_err(|e| Error::msg(format!("invalid hold expiry '{expires}': {e}")))?;
    Ok(OffsetDateTime::now_utc() >= until)
}

/// Read-only hold lookup: an expired hold is reported as inactive but is never
/// removed here. Removing state is reserved for locked, mutating paths.
pub fn active_hold(prefix: Option<&Path>) -> Result<Option<Hold>> {
    match read_hold(prefix)? {
        None => Ok(None),
        Some(hold) if hold_expired(&hold)? => Ok(None),
        Some(hold) => Ok(Some(hold)),
    }
}

/// Remove an expired hold. Callers must hold the cycle lock.
pub fn expire_hold(prefix: Option<&Path>) -> Result<()> {
    if let Some(hold) = read_hold(prefix)? {
        if hold_expired(&hold)? {
            let path = hold_path(prefix);
            if fs::symlink_metadata(&path).is_ok() {
                let _ = fs::remove_file(&path);
                crate::logging::journal("hold_expired", &[]);
            }
        }
    }
    Ok(())
}

pub fn write_hold(prefix: Option<&Path>, hold: &Hold) -> Result<()> {
    let text = serde_json::to_string_pretty(hold)
        .map_err(|e| Error::msg(format!("failed to serialize hold: {e}")))?;
    atomic_write(&hold_path(prefix), format!("{text}\n").as_bytes())
}

pub fn remove_hold(prefix: Option<&Path>) -> Result<bool> {
    let path = hold_path(prefix);
    if !path.exists() {
        return Ok(false);
    }
    fs::remove_file(&path).map_err(|source| Error::Write { path, source })?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hold_text_and_json_parse() {
        let plain = parse_hold("maintenance window");
        assert_eq!(plain.reason.as_deref(), Some("maintenance window"));
        let json = r#"{"created_at":"2026-01-01T00:00:00Z","reason":"x","expires_at":"2030-01-01T00:00:00Z"}"#;
        let hold = parse_hold(json);
        assert_eq!(hold.reason.as_deref(), Some("x"));
        assert!(hold.expires_at.is_some());
    }

    #[test]
    fn active_hold_is_read_only_and_expiry_is_explicit() {
        let td = tempfile::tempdir().unwrap();
        let prefix = td.path();
        write_hold(
            Some(prefix),
            &Hold {
                created_at: "2020-01-01T00:00:00Z".to_string(),
                reason: None,
                expires_at: Some("2020-01-02T00:00:00Z".to_string()),
            },
        )
        .unwrap();
        let path = hold_path(Some(prefix));
        assert!(active_hold(Some(prefix)).unwrap().is_none());
        assert!(path.exists(), "reader deleted the hold file");
        expire_hold(Some(prefix)).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn state_dir_is_scoped_to_prefix() {
        assert_eq!(
            state_dir(Some(Path::new("/opt/bin"))),
            PathBuf::from("/opt/bin/.tuishou")
        );
    }

    #[test]
    fn cycle_lock_is_os_released_and_rejects_a_second_holder() {
        let td = tempfile::tempdir().unwrap();
        let prefix = td.path();
        let first = CycleLock::acquire(Some(prefix)).unwrap();
        assert!(CycleLock::try_acquire(Some(prefix)).unwrap().is_none());
        drop(first);
        assert!(CycleLock::try_acquire(Some(prefix)).unwrap().is_some());
    }

    #[test]
    fn stale_directory_lock_does_not_block_the_cycle_lock() {
        let td = tempfile::tempdir().unwrap();
        let prefix = td.path();
        let stale = state_dir(Some(prefix)).join("lock.d");
        fs::create_dir_all(&stale).unwrap();
        assert!(CycleLock::acquire(Some(prefix)).is_ok());
    }

    #[test]
    fn old_state_without_backup_record_still_loads() {
        let td = tempfile::tempdir().unwrap();
        ensure_state_dir(Some(td.path())).unwrap();
        fs::write(state_path(Some(td.path())), r#"{"last_check":"old"}"#).unwrap();
        let loaded = load(Some(td.path())).unwrap();
        assert_eq!(loaded.last_check.as_deref(), Some("old"));
        assert!(loaded.controller_backup.is_none());
        assert!(loaded.self_pending.is_none());
    }

    #[test]
    fn self_pending_record_round_trips() {
        let td = tempfile::tempdir().unwrap();
        let state = AutoState {
            self_pending: Some(SelfPending {
                version: "v0.3.6".to_string(),
                previous_version: Some("v0.3.5".to_string()),
                path: td.path().join("tuishou"),
                at: "2026-01-01T00:00:00Z".to_string(),
            }),
            ..AutoState::default()
        };
        save(Some(td.path()), &state).unwrap();
        let loaded = load(Some(td.path())).unwrap();
        let pending = loaded.self_pending.expect("pending must persist");
        assert_eq!(pending.version, "v0.3.6");
        assert_eq!(pending.previous_version.as_deref(), Some("v0.3.5"));
        assert_eq!(pending.path, td.path().join("tuishou"));
    }

    #[test]
    fn controller_backup_record_round_trips() {
        let td = tempfile::tempdir().unwrap();
        let state = AutoState {
            controller_backup: Some(ControllerBackup {
                path: td.path().join("backup.sql"),
                checksum: "0123456789abcdef".to_string(),
            }),
            ..AutoState::default()
        };
        save(Some(td.path()), &state).unwrap();
        let loaded = load(Some(td.path())).unwrap();
        let backup = loaded
            .controller_backup
            .expect("backup record must persist");
        assert_eq!(backup.path, td.path().join("backup.sql"));
        assert_eq!(backup.checksum, "0123456789abcdef");
    }

    #[test]
    fn state_writes_use_unique_temp_names() {
        let td = tempfile::tempdir().unwrap();
        let prefix = td.path();
        let first = AutoState {
            last_check: Some("first".to_string()),
            ..AutoState::default()
        };
        let second = AutoState {
            last_check: Some("second".to_string()),
            ..AutoState::default()
        };
        save(Some(prefix), &first).unwrap();
        save(Some(prefix), &second).unwrap();
        assert_eq!(
            load(Some(prefix)).unwrap().last_check.as_deref(),
            Some("second")
        );
        assert!(!state_path(Some(prefix))
            .with_file_name("auto.json.tmp")
            .exists());
    }
}
