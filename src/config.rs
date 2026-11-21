use crate::component::{builtin_components, Component};
use crate::error::{Error, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    #[serde(default)]
    pub component: Vec<ComponentConfig>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ComponentConfig {
    pub name: String,
    pub repo: Option<String>,
    pub asset_pattern: Option<String>,
    pub binaries: Option<Vec<String>>,
    pub service_unit: Option<String>,
}

pub fn default_config_path() -> PathBuf {
    PathBuf::from("tuishou.toml")
}

pub fn load_components(config_path: Option<&Path>) -> Result<Vec<Component>> {
    let mut components = builtin_components();
    let path = config_path
        .map(PathBuf::from)
        .unwrap_or_else(default_config_path);
    if !path.exists() {
        if config_path.is_some() {
            return Err(Error::Read {
                path,
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "config file not found"),
            });
        }
        return Ok(components);
    }
    let text = std::fs::read_to_string(&path).map_err(|source| Error::Read {
        path: path.clone(),
        source,
    })?;
    let cfg: ConfigFile = toml::from_str(&text).map_err(|e| {
        let msg = e.to_string();
        if msg.contains("unknown field") {
            // extract key name best-effort
            let key = msg.split('`').nth(1).unwrap_or("unknown").to_string();
            Error::Config {
                path: path.clone(),
                source: Box::new(Error::UnknownConfigKey(key)),
            }
        } else {
            Error::Config {
                path: path.clone(),
                source: Box::new(Error::msg(msg)),
            }
        }
    })?;
    for o in cfg.component {
        if let Some(existing) = components.iter_mut().find(|c| c.name == o.name) {
            if let Some(repo) = o.repo {
                existing.repo = repo;
            }
            if let Some(asset_pattern) = o.asset_pattern {
                existing.asset_pattern = asset_pattern;
            }
            if let Some(binaries) = o.binaries {
                existing.binaries = binaries;
            }
            if o.service_unit.is_some() {
                existing.service_unit = o.service_unit;
            }
        } else {
            let repo = o.repo.ok_or_else(|| Error::Config {
                path: path.clone(),
                source: Box::new(Error::msg(format!("component '{}' missing repo", o.name))),
            })?;
            let asset_pattern = o.asset_pattern.ok_or_else(|| Error::Config {
                path: path.clone(),
                source: Box::new(Error::msg(format!(
                    "component '{}' missing asset_pattern",
                    o.name
                ))),
            })?;
            let binaries = o.binaries.ok_or_else(|| Error::Config {
                path: path.clone(),
                source: Box::new(Error::msg(format!(
                    "component '{}' missing binaries",
                    o.name
                ))),
            })?;
            components.push(Component {
                name: o.name,
                repo,
                asset_pattern,
                binaries,
                service_unit: o.service_unit,
            });
        }
    }
    Ok(components)
}
