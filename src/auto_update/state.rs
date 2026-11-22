//! State for scheduled auto-update. Everything lives in one 0700 directory.

use crate::error::{Error, Result};
use crate::release::Release;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentResult {
    pub result: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_eligible_version: Option<String>,
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
    let dir = ensure_state_dir(prefix)?;
    let path = state_path(prefix);
    let text = serde_json::to_string_pretty(state)
        .map_err(|e| Error::msg(format!("failed to serialize auto-update state: {e}")))?;
    let tmp = dir.join("auto.json.tmp");
    fs::write(&tmp, format!("{text}\n")).map_err(|source| Error::Write {
        path: tmp.clone(),
        source,
    })?;
    fs::rename(&tmp, &path).map_err(|source| Error::Write {
        path: path.clone(),
        source,
    })?;
    Ok(())
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

/// A hold with an expiry in the past is inactive and is removed on sight.
pub fn active_hold(prefix: Option<&Path>) -> Result<Option<Hold>> {
    match read_hold(prefix)? {
        None => Ok(None),
        Some(hold) => {
            if let Some(expires) = &hold.expires_at {
                let until = OffsetDateTime::parse(expires, &Rfc3339)
                    .map_err(|e| Error::msg(format!("invalid hold expiry '{expires}': {e}")))?;
                if OffsetDateTime::now_utc() >= until {
                    let _ = fs::remove_file(hold_path(prefix));
                    crate::logging::journal("hold_expired", &[]);
                    return Ok(None);
                }
            }
            Ok(Some(hold))
        }
    }
}

pub fn write_hold(prefix: Option<&Path>, hold: &Hold) -> Result<()> {
    let dir = ensure_state_dir(prefix)?;
    let path = hold_path(prefix);
    let text = serde_json::to_string_pretty(hold)
        .map_err(|e| Error::msg(format!("failed to serialize hold: {e}")))?;
    let tmp = dir.join("HOLD.tmp");
    fs::write(&tmp, format!("{text}\n")).map_err(|source| Error::Write {
        path: tmp.clone(),
        source,
    })?;
    fs::rename(&tmp, &path).map_err(|source| Error::Write {
        path: path.clone(),
        source,
    })?;
    Ok(())
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
    fn state_dir_is_scoped_to_prefix() {
        assert_eq!(
            state_dir(Some(Path::new("/opt/bin"))),
            PathBuf::from("/opt/bin/.tuishou")
        );
    }
}
