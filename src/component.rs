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
        Component {
            name: "lu".to_string(),
            repo: "ddproxy/lu".to_string(),
            asset_pattern: "lu-{tag}-{target}.tar.gz".to_string(),
            binaries: vec!["lu".to_string(), "lu-agent".to_string(), "ting".to_string()],
            service_unit: None,
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
