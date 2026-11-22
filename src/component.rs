use crate::error::{Error, Result};
use crate::version::Version;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};

/// The node agent is shipped inside the `lu` binary until a release provides a
/// dedicated executable. Either name is accepted when it appears in an archive.
/// Keeping this as one constant means promoting the agent to a default again is
/// a one-line change in `builtin_components`.
pub const NODE_AGENT_BINARIES: [&str; 2] = ["lu-agent", "ting"];

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
    /// Configuration file checked by components that own a config (Reiki).
    #[serde(default)]
    pub config_path: Option<PathBuf>,
    /// Expected binaries that may be absent from a release without making the
    /// component "not in this release" (the Lu node agent today).
    #[serde(default)]
    pub optional_binaries: Vec<String>,
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

    /// Binaries that must be present for a release to count as shipping this
    /// component. Optional agent binaries are excluded so a controller-only
    /// Lu archive is still "in this release".
    pub fn required_binaries(&self, tag: &str) -> Vec<String> {
        self.expected_binaries(tag)
            .into_iter()
            .filter(|name| !self.optional_binaries.contains(name))
            .collect()
    }

    /// Configuration path to validate for components that own one.
    pub fn config_path(&self) -> Option<&Path> {
        self.config_path.as_deref()
    }
}

pub fn builtin_components() -> Vec<Component> {
    // Table order is the report order for every command. Reiki and Lu are
    // equal first-class defaults; `ting` is not a separate default until a
    // release archive actually contains a `ting` binary.
    vec![
        Component {
            name: "reiki".to_string(),
            repo: "ddproxy/reiki".to_string(),
            asset_pattern: "reiki-{tag}-{target}.tar.gz".to_string(),
            binaries: vec!["reiki".to_string()],
            service_unit: Some("reiki.service".to_string()),
            config_path: Some(PathBuf::from("/var/lib/reiki/reiki.toml")),
            optional_binaries: Vec::new(),
            binary_ranges: vec![BinaryRange {
                from: None,
                until: None,
                binaries: vec!["reiki".to_string()],
            }],
        },
        Component {
            name: "lu".to_string(),
            repo: "ddproxy/tuishou-lu".to_string(),
            asset_pattern: "lu-{tag}-{target}.tar.gz".to_string(),
            // The node agent runs as `lu agent` inside `lu` until a release
            // ships a separate executable. Either dedicated binary is accepted
            // when present; the controller binary is always required.
            binaries: std::iter::once("lu".to_string())
                .chain(NODE_AGENT_BINARIES.iter().map(|name| (*name).to_string()))
                .collect(),
            service_unit: Some("lu-agent.service".to_string()),
            config_path: None,
            optional_binaries: NODE_AGENT_BINARIES
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
            binary_ranges: vec![BinaryRange {
                from: None,
                until: None,
                binaries: std::iter::once("lu".to_string())
                    .chain(NODE_AGENT_BINARIES.iter().map(|name| (*name).to_string()))
                    .collect(),
            }],
        },
        Component {
            name: "tuishou".to_string(),
            repo: "ddproxy/tuishou".to_string(),
            asset_pattern: "tuishou-{tag}-{target}.tar.gz".to_string(),
            binaries: vec!["tuishou".to_string()],
            service_unit: None,
            config_path: None,
            optional_binaries: Vec::new(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_components_are_reiki_lu_tuishou_in_report_order() {
        let components = builtin_components();
        assert_eq!(
            components
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["reiki", "lu", "tuishou"]
        );
        assert!(resolve_component("ting", &components).is_err());
    }

    #[test]
    fn reiki_defaults_use_real_release_naming_and_config_check() {
        let components = builtin_components();
        let reiki = resolve_component("reiki", &components).unwrap();
        assert_eq!(reiki.repo, "ddproxy/reiki");
        assert_eq!(
            reiki.asset_name("v0.4.0", "x86_64-unknown-linux-musl"),
            "reiki-v0.4.0-x86_64-unknown-linux-musl.tar.gz"
        );
        assert_eq!(
            reiki.checksum_asset_name("v0.4.0", "x86_64-unknown-linux-musl"),
            "reiki-v0.4.0-x86_64-unknown-linux-musl.tar.gz.sha256"
        );
        assert_eq!(reiki.binaries, vec!["reiki".to_string()]);
        assert_eq!(reiki.service_unit.as_deref(), Some("reiki.service"));
        assert_eq!(
            reiki.config_path,
            Some(PathBuf::from("/var/lib/reiki/reiki.toml"))
        );
    }

    #[test]
    fn lu_defaults_recognise_optional_agent_binaries() {
        let components = builtin_components();
        let lu = resolve_component("lu", &components).unwrap();
        assert_eq!(lu.repo, "ddproxy/tuishou-lu");
        assert_eq!(
            lu.asset_name("v0.4.0", "x86_64-unknown-linux-musl"),
            "lu-v0.4.0-x86_64-unknown-linux-musl.tar.gz"
        );
        assert_eq!(
            lu.checksum_asset_name("v0.4.0", "x86_64-unknown-linux-musl"),
            "lu-v0.4.0-x86_64-unknown-linux-musl.tar.gz.sha256"
        );
        assert_eq!(lu.binaries[0], "lu");
        assert!(lu.binaries.contains(&"lu-agent".to_string()));
        assert!(lu.binaries.contains(&"ting".to_string()));
        assert_eq!(lu.service_unit.as_deref(), Some("lu-agent.service"));
        assert_eq!(
            lu.expected_binaries("v0.4.1"),
            vec!["lu".to_string(), "lu-agent".to_string(), "ting".to_string()]
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
            config_path: None,
            optional_binaries: Vec::new(),
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
