//! Version comparison and auto-update policy.
//!
//! Only the numeric `major.minor.patch` core is used for ordering; a
//! pre-release suffix (`-rc1`, `-beta.2`, ...) is tracked separately so the
//! release channel can exclude it.

use crate::error::{Error, Result};
use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub prerelease: Option<String>,
}

impl Version {
    pub fn parse(tag: &str) -> Result<Self> {
        let raw = tag.strip_prefix('v').unwrap_or(tag).trim();
        let (core, pre) = match raw.split_once('-') {
            Some((core, pre)) if !pre.is_empty() => (core, Some(pre.to_string())),
            _ => (raw, None),
        };
        let mut parts = core.split('.');
        let major = parts
            .next()
            .and_then(|p| p.parse::<u64>().ok())
            .ok_or_else(|| Error::msg(format!("invalid version tag '{tag}'")))?;
        let minor = parts
            .next()
            .and_then(|p| p.parse::<u64>().ok())
            .ok_or_else(|| Error::msg(format!("invalid version tag '{tag}'")))?;
        let patch = parts
            .next()
            .and_then(|p| p.parse::<u64>().ok())
            .ok_or_else(|| Error::msg(format!("invalid version tag '{tag}'")))?;
        if parts.next().is_some() {
            return Err(Error::msg(format!("invalid version tag '{tag}'")));
        }
        Ok(Self {
            major,
            minor,
            patch,
            prerelease: pre,
        })
    }

    pub fn core(&self) -> (u64, u64, u64) {
        (self.major, self.minor, self.patch)
    }

    pub fn is_prerelease(&self) -> bool {
        self.prerelease.is_some()
    }

    /// Compare numeric cores. Pre-release status is intentionally ignored
    /// here: the channel filter decides whether pre-releases are considered.
    pub fn cmp_core(&self, other: &Self) -> Ordering {
        self.core().cmp(&other.core())
    }

    pub fn is_newer_than(&self, other: &Self) -> bool {
        self.cmp_core(other) == Ordering::Greater
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    Patch,
    Minor,
    Any,
}

impl Policy {
    #[allow(dead_code)]
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "patch" => Ok(Self::Patch),
            "minor" => Ok(Self::Minor),
            "any" => Ok(Self::Any),
            other => Err(Error::msg(format!(
                "invalid auto-update policy '{other}' (expected patch, minor or any)"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Patch => "patch",
            Self::Minor => "minor",
            Self::Any => "any",
        }
    }
}

impl std::fmt::Display for Policy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Decide whether `candidate` may be applied on top of `installed`.
///
/// Rules:
/// - it must be a strictly newer version;
/// - `max_version`, when present, is an inclusive cap;
/// - `patch` requires the same major.minor;
/// - `minor` requires the same major, except that for `0.y.z` a minor bump is
///   a breaking change and is therefore refused (same effect as `patch`);
/// - `any` allows every newer version.
pub fn eligible(
    installed: &str,
    candidate: &str,
    policy: Policy,
    max_version: Option<&str>,
) -> Result<(bool, Option<String>)> {
    let installed = Version::parse(installed)?;
    let candidate = Version::parse(candidate)?;
    if let Some(max) = max_version {
        let max = Version::parse(max)?;
        if candidate.cmp_core(&max) == Ordering::Greater {
            return Ok((false, Some(format!("above max_version {max:?}"))));
        }
    }
    if !candidate.is_newer_than(&installed) {
        return Ok((false, Some("not newer than installed version".to_string())));
    }
    let allowed = match policy {
        Policy::Any => true,
        Policy::Patch => installed.major == candidate.major && installed.minor == candidate.minor,
        Policy::Minor => {
            installed.major == candidate.major
                && !(installed.major == 0 && installed.minor != candidate.minor)
        }
    };
    let reason = if allowed {
        None
    } else {
        Some(format!(
            "policy '{policy}' forbids {} -> {} (0.y minor bumps are breaking)",
            format_version(&installed),
            format_version(&candidate)
        ))
    };
    Ok((allowed, reason))
}

pub fn format_version(v: &Version) -> String {
    let mut out = format!("v{}.{}.{}", v.major, v.minor, v.patch);
    if let Some(pre) = &v.prerelease {
        out.push('-');
        out.push_str(pre);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_patch() {
        assert!(eligible("v0.4.0", "v0.4.1", Policy::Patch, None).unwrap().0);
        assert!(!eligible("v0.4.0", "v0.5.0", Policy::Patch, None).unwrap().0);
        assert!(!eligible("v0.4.0", "v1.0.0", Policy::Patch, None).unwrap().0);
    }

    #[test]
    fn zero_minor_is_breaking_for_patch_and_minor() {
        for policy in [Policy::Patch, Policy::Minor] {
            assert!(
                !eligible("v0.4.0", "v0.5.0", policy, None).unwrap().0,
                "policy {policy}"
            );
            assert!(eligible("v0.4.0", "v0.4.3", policy, None).unwrap().0);
        }
        assert!(eligible("v0.4.0", "v0.5.0", Policy::Any, None).unwrap().0);
    }

    #[test]
    fn minor_and_any() {
        assert!(eligible("v1.2.0", "v1.3.0", Policy::Minor, None).unwrap().0);
        assert!(!eligible("v1.2.0", "v2.0.0", Policy::Minor, None).unwrap().0);
        assert!(eligible("v1.2.0", "v2.0.0", Policy::Any, None).unwrap().0);
        assert!(!eligible("v2.0.0", "v1.9.9", Policy::Any, None).unwrap().0);
    }

    #[test]
    fn max_version_caps() {
        assert!(
            eligible("v1.2.0", "v1.3.0", Policy::Minor, Some("v1.3.0"))
                .unwrap()
                .0
        );
        assert!(
            !eligible("v1.2.0", "v1.4.0", Policy::Minor, Some("v1.3.0"))
                .unwrap()
                .0
        );
    }

    #[test]
    fn prerelease_detected() {
        assert!(Version::parse("v0.5.0-rc1").unwrap().is_prerelease());
        assert!(!Version::parse("v0.5.0").unwrap().is_prerelease());
    }
}
