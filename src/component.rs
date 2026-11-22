use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Component {
    pub name: String,
    pub repo: String,
    pub asset_pattern: String,
    pub binaries: Vec<String>,
    #[serde(default)]
    pub service_unit: Option<String>,
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
}

pub fn builtin_components() -> Vec<Component> {
    vec![
        // Both `lu` (controller) and `ting` (node agent) ship in the same
        // release archive, named lu-<tag>-<target>.tar.gz, in the
        // ddproxy/tuishou-lu repository. `lu-agent` is accepted in the binary
        // list while the rename to `ting` lands.
        Component {
            name: "lu".to_string(),
            repo: "ddproxy/tuishou-lu".to_string(),
            asset_pattern: "lu-{tag}-{target}.tar.gz".to_string(),
            binaries: vec!["lu".to_string(), "ting".to_string(), "lu-agent".to_string()],
            service_unit: None,
        },
        Component {
            name: "ting".to_string(),
            repo: "ddproxy/tuishou-lu".to_string(),
            asset_pattern: "lu-{tag}-{target}.tar.gz".to_string(),
            binaries: vec!["ting".to_string(), "lu-agent".to_string()],
            service_unit: Some("lu-agent.service".to_string()),
        },
        Component {
            name: "tuishou".to_string(),
            repo: "ddproxy/tuishou".to_string(),
            asset_pattern: "tuishou-{tag}-{target}.tar.gz".to_string(),
            binaries: vec!["tuishou".to_string()],
            service_unit: None,
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
        assert!(lu.binaries.iter().any(|b| b == "lu"));
        assert!(ting.binaries.iter().any(|b| b == "ting"));
        assert!(ting.binaries.iter().any(|b| b == "lu-agent"));
    }
}
