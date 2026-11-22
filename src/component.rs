use crate::error::{Error, Result};
use crate::version::Version;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// Binaries a component is expected to contribute to an archive in a given
/// release range. Bounds are inclusive; an omitted bound is open-ended.
/// An empty `binaries` list means the component is not shipped in that range.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BinaryRange {
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub until: Option<String>,
    #[serde(default)]
    pub binaries: Vec<String>,
}

impl BinaryRange {
    fn contains(&self, tag: &str) -> bool {
        if let Some(from) = &self.from {
            if compare_tags(tag, from) == Ordering::Less {
                return false;
            }
        }
        if let Some(until) = &self.until {
            if compare_tags(tag, until) == Ordering::Greater {
                return false;
            }
        }
        true
    }
}

fn compare_tags(left: &str, right: &str) -> Ordering {
    match (Version::parse(left), Version::parse(right)) {
        (Ok(a), Ok(b)) => a.cmp(&b),
        _ => left.cmp(right),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Component {
    pub name: String,
    pub repo: String,
    pub asset_pattern: String,
    /// Default expected binaries when no range matches.
    pub binaries: Vec<String>,
    #[serde(default)]
    pub service_unit: Option<String>,
    /// Per-release expected binaries. The first matching range wins.
    #[serde(default)]
    pub binary_ranges: Vec<BinaryRange>,
}

impl Component {
    pub fn asset_name(&self, tag: &str, target: &str) -> String {
        self.asset_pattern
            .replace("{tag}", tag)
            .replace("{target}", target)
            .replace("{name}", &self.name)
    }

    pub fn checksum_asset_name(&self, tag: &str, target: &str) -> String {
        format!("{}.sha256", self.asset_name(tag, target))
    }

    /// Binaries expected for this release. Until a release actually contains
    /// one of them, update callers report "not in this release" and skip the
    /// component instead of failing the cycle.
    pub fn expected_binaries(&self, tag: &str) -> Vec<String> {
        self.binary_ranges
            .iter()
            .find(|range| range.contains(tag))
            .map(|range| range.binaries.clone())
            .unwrap_or_else(|| self.binaries.clone())
    }
}

pub fn builtin_components() -> Vec<Component> {
    vec![
        // Both `lu` (controller) and `ting` (node agent) ship in the same
        // release archive, named lu-<tag>-<target>.tar.gz, in the
        // ddproxy/tuishou-lu repository. Binary ownership does not overlap:
        // the controller owns only `lu`, the agent owns `ting`/`lu-agent`.
        Component {
            name: "lu".to_string(),
            repo: "ddproxy/tuishou-lu".to_string(),
            asset_pattern: "lu-{tag}-{target}.tar.gz".to_string(),
            binaries: vec!["lu".to_string()],
            service_unit: None,
            binary_ranges: vec![BinaryRange {
                from: None,
                until: None,
                binaries: vec!["lu".to_string()],
            }],
        },
        Component {
            name: "ting".to_string(),
            repo: "ddproxy/tuishou-lu".to_string(),
            asset_pattern: "lu-{tag}-{target}.tar.gz".to_string(),
            binaries: vec!["ting".to_string(), "lu-agent".to_string()],
            service_unit: Some("lu-agent.service".to_string()),
            // The Lu release workflow still packages only `lu`; the node agent
            // ships in the same workspace as `lu-agent` and is being renamed
            // to `ting`. Either name counts as "in this release"; when neither
            // is present the component is skipped, not failed.
            binary_ranges: vec![BinaryRange {
                from: None,
                until: None,
                binaries: vec!["ting".to_string(), "lu-agent".to_string()],
            }],
        },
        Component {
            name: "tuishou".to_string(),
            repo: "ddproxy/tuishou".to_string(),
            asset_pattern: "tuishou-{tag}-{target}.tar.gz".to_string(),
            binaries: vec!["tuishou".to_string()],
            service_unit: None,
            binary_ranges: vec![BinaryRange {
                from: None,
                until: None,
                binaries: vec!["tuishou".to_string()],
            }],
        },
    ]
}

pub fn resolve_component(name: &str, components: &[Component]) -> Result<Component> {
    components
        .iter()
        .find(|c| c.name == name)
        .cloned()
        .ok_or_else(|| Error::UnknownComponent(name.to_string()))
}

pub fn resolve_components(names: &[String], components: &[Component]) -> Result<Vec<Component>> {
    if names.len() == 1 && names[0] == "all" {
        return Ok(components.to_vec());
    }
    names
        .iter()
        .map(|n| resolve_component(n, components))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_components_use_real_release_naming() {
        let components = builtin_components();
        let lu = resolve_component("lu", &components).unwrap();
        let ting = resolve_component("ting", &components).unwrap();
        for c in [&lu, &ting] {
            assert_eq!(c.repo, "ddproxy/tuishou-lu");
            assert_eq!(
                c.asset_name("v0.4.0", "x86_64-unknown-linux-musl"),
                "lu-v0.4.0-x86_64-unknown-linux-musl.tar.gz"
            );
            assert_eq!(
                c.checksum_asset_name("v0.4.0", "x86_64-unknown-linux-musl"),
                "lu-v0.4.0-x86_64-unknown-linux-musl.tar.gz.sha256"
            );
        }
        assert_eq!(lu.binaries, vec!["lu".to_string()]);
        assert_eq!(
            ting.binaries,
            vec!["ting".to_string(), "lu-agent".to_string()]
        );
        assert!(
            lu.binaries.iter().all(|b| !ting.binaries.contains(b)),
            "controller and agent binary ownership must not overlap"
        );
        assert_eq!(
            ting.expected_binaries("v0.4.1"),
            vec!["ting".to_string(), "lu-agent".to_string()]
        );
    }

    #[test]
    fn expected_binaries_follow_release_ranges() {
        let c = Component {
            name: "app".to_string(),
            repo: "o/r".to_string(),
            asset_pattern: "app-{tag}.tar.gz".to_string(),
            binaries: vec!["app".to_string()],
            service_unit: None,
            binary_ranges: vec![
                BinaryRange {
                    from: None,
                    until: Some("v0.4.0".to_string()),
                    binaries: Vec::new(),
                },
                BinaryRange {
                    from: Some("v0.4.1".to_string()),
                    until: None,
                    binaries: vec!["app-agent".to_string()],
                },
            ],
        };
        assert!(c.expected_binaries("v0.4.0").is_empty());
        assert_eq!(c.expected_binaries("v0.4.1"), vec!["app-agent"]);
        assert_eq!(c.expected_binaries("v1.0.0"), vec!["app-agent"]);
    }
}
