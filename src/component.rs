use crate::error::{Error, Result};
use crate::version::Version;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};

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
    /// Per-release expected binaries. The first matching range wins.
    #[serde(default)]
    pub binary_ranges: Vec<BinaryRange>,
    /// Per-component opt-in for scheduled auto-update. Off by default.
    #[serde(default)]
    pub auto_update: bool,
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

    /// Every expected binary is required: a Lu release must ship both `lu` and
    /// `ting` or it is not a complete release for automatic installation.
    pub fn required_binaries(&self, tag: &str) -> Vec<String> {
        self.expected_binaries(tag)
    }

    /// Configuration path to validate for components that own one.
    pub fn config_path(&self) -> Option<&Path> {
        self.config_path.as_deref()
    }
}

pub fn builtin_components() -> Vec<Component> {
    // Table order is the report order for every command. Reiki and Lu are
    // equal first-class defaults; `ting` is part of the Lu release pair.
    vec![
        Component {
            name: "reiki".to_string(),
            repo: "ddproxy/reiki".to_string(),
            asset_pattern: "reiki-{tag}-{target}.tar.gz".to_string(),
            binaries: vec!["reiki".to_string()],
            service_unit: Some("reiki.service".to_string()),
            config_path: Some(PathBuf::from("/var/lib/reiki/reiki.toml")),
            binary_ranges: vec![BinaryRange {
                from: None,
                until: None,
                binaries: vec!["reiki".to_string()],
            }],
            auto_update: false,
        },
        Component {
            name: "lu".to_string(),
            repo: "ddproxy/tuishou-lu".to_string(),
            asset_pattern: "lu-{tag}-{target}.tar.gz".to_string(),
            // One release ships both the controller and the node agent. The
            // pair is installed and rolled back together.
            binaries: vec!["lu".to_string(), "ting".to_string()],
            service_unit: None,
            config_path: None,
            binary_ranges: vec![BinaryRange {
                from: None,
                until: None,
                binaries: vec!["lu".to_string(), "ting".to_string()],
            }],
            auto_update: false,
        },
        Component {
            name: "tuishou".to_string(),
            repo: "ddproxy/tuishou".to_string(),
            asset_pattern: "tuishou-{tag}-{target}.tar.gz".to_string(),
            binaries: vec!["tuishou".to_string()],
            service_unit: None,
            config_path: None,
            binary_ranges: vec![BinaryRange {
                from: None,
                until: None,
                binaries: vec!["tuishou".to_string()],
            }],
            auto_update: false,
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
        assert!(components.iter().all(|c| !c.auto_update));
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
    fn lu_defaults_require_both_controller_and_agent_binaries() {
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
        assert_eq!(lu.binaries, vec!["lu".to_string(), "ting".to_string()]);
        assert_eq!(lu.service_unit, None);
        assert_eq!(
            lu.expected_binaries("v0.4.1"),
            vec!["lu".to_string(), "ting".to_string()]
        );
        assert_eq!(
            lu.required_binaries("v0.4.1"),
            vec!["lu".to_string(), "ting".to_string()]
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
            auto_update: false,
        };
        assert!(c.expected_binaries("v0.4.0").is_empty());
        assert_eq!(c.expected_binaries("v0.4.1"), vec!["app-agent"]);
        assert_eq!(c.expected_binaries("v1.0.0"), vec!["app-agent"]);
    }
}
