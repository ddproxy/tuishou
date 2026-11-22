//! Version comparison and auto-update policy.
//!
//! Ordering follows SemVer precedence: the numeric `major.minor.patch` core
//! comes first, then pre-release identifiers (`-rc1`, `-beta.2`, ...). The
//! release channel is a separate filter and does not change precedence.

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

    /// Compare numeric cores only. Used by policy and controller schema gates,
    /// where the release/channel decision is deliberately separate.
    pub fn cmp_core(&self, other: &Self) -> Ordering {
        self.core().cmp(&other.core())
    }

    /// Pre-release precedence. A stable release ranks above all of its
    /// pre-releases. Alphanumeric runs are compared token by token; digit runs
    /// compare numerically, so both `rc2` (read as `rc.2`) and `rc.10` order by
    /// their release-candidate number while `beta.2` stays below `beta.11`.
    fn cmp_prerelease(&self, other: &Self) -> Ordering {
        match (self.prerelease.as_deref(), other.prerelease.as_deref()) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) => cmp_prerelease_tokens(a, b),
        }
    }

    pub fn is_newer_than(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Greater
    }
}

/// Split a pre-release string into alphanumeric tokens, ignoring separators.
/// This keeps SemVer's dot-separated numeric ordering while also ordering
/// compact suffixes such as `rc2` and `rc.10` by their numeric component.
fn prerelease_tokens(value: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start = None;
    let mut started_as_digit = false;
    for (index, byte) in value.bytes().enumerate() {
        if !byte.is_ascii_alphanumeric() {
            if let Some(first) = start.take() {
                tokens.push(&value[first..index]);
            }
            continue;
        }
        let is_digit = byte.is_ascii_digit();
        match start {
            None => {
                start = Some(index);
                started_as_digit = is_digit;
            }
            Some(first) if started_as_digit != is_digit => {
                tokens.push(&value[first..index]);
                start = Some(index);
                started_as_digit = is_digit;
            }
            Some(_) => {}
        }
    }
    if let Some(first) = start {
        tokens.push(&value[first..]);
    }
    tokens
}

fn cmp_prerelease_tokens(left: &str, right: &str) -> Ordering {
    let left = prerelease_tokens(left);
    let right = prerelease_tokens(right);
    for pair in left.iter().zip(right.iter()) {
        let ord = cmp_identifier(pair.0, pair.1);
        if ord != Ordering::Equal {
            return ord;
        }
    }
    left.len().cmp(&right.len())
}

fn cmp_identifier(left: &str, right: &str) -> Ordering {
    let left_numeric = !left.is_empty() && left.bytes().all(|b| b.is_ascii_digit());
    let right_numeric = !right.is_empty() && right.bytes().all(|b| b.is_ascii_digit());
    match (left_numeric, right_numeric) {
        (true, true) => cmp_numeric_identifier(left, right),
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => left.cmp(right),
    }
}

fn cmp_numeric_identifier(left: &str, right: &str) -> Ordering {
    // Compare by significant digits so overflowing identifiers still order
    // numerically without pulling in a big-integer dependency.
    let left = left.trim_start_matches('0');
    let right = right.trim_start_matches('0');
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.cmp_core(other)
            .then_with(|| self.cmp_prerelease(other))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
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
/// - it must rank strictly above the installed version (SemVer precedence);
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
        if candidate.cmp(&max) == Ordering::Greater {
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

    #[test]
    fn semver_precedence_follows_the_specification_examples() {
        let order = [
            "v1.0.0-alpha",
            "v1.0.0-alpha.1",
            "v1.0.0-alpha.beta",
            "v1.0.0-beta",
            "v1.0.0-beta.2",
            "v1.0.0-beta.11",
            "v1.0.0-rc.1",
            "v1.0.0",
        ];
        for pair in order.windows(2) {
            let lower = Version::parse(pair[0]).unwrap();
            let higher = Version::parse(pair[1]).unwrap();
            assert!(
                higher.is_newer_than(&lower),
                "{} must rank above {}",
                pair[1],
                pair[0]
            );
            assert!(
                !lower.is_newer_than(&higher),
                "{} must not rank above {}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn prereleases_of_the_installed_core_are_upgradable() {
        let rc1 = Version::parse("v1.2.1-rc1").unwrap();
        let rc2 = Version::parse("v1.2.1-rc2").unwrap();
        let rc10 = Version::parse("v1.2.1-rc.10").unwrap();
        let stable = Version::parse("v1.2.1").unwrap();
        assert!(rc2.is_newer_than(&rc1));
        assert!(rc10.is_newer_than(&rc2));
        assert!(stable.is_newer_than(&rc10));
        assert!(!rc1.is_newer_than(&stable));
        assert!(
            eligible("v1.2.1-rc1", "v1.2.1", Policy::Patch, None)
                .unwrap()
                .0
        );
        assert!(
            !eligible("v1.2.1", "v1.2.1-rc2", Policy::Patch, None)
                .unwrap()
                .0
        );
    }
}
