use crate::component::{builtin_components, BinaryRange, Component};
use crate::duration::parse_duration;
use crate::error::{Error, Result};
use crate::version;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    #[serde(default)]
    pub component: Vec<ComponentConfig>,

    #[serde(default)]
    pub auto_update: AutoUpdateConfig,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ComponentConfig {
    pub name: String,
    pub repo: Option<String>,
    pub asset_pattern: Option<String>,
    pub binaries: Option<Vec<String>>,
    pub service_unit: Option<String>,
    pub binary_ranges: Option<Vec<BinaryRange>>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AutoPolicy {
    #[default]
    Patch,
    Minor,
    Any,
}

impl From<AutoPolicy> for version::Policy {
    fn from(p: AutoPolicy) -> Self {
        match p {
            AutoPolicy::Patch => Self::Patch,
            AutoPolicy::Minor => Self::Minor,
            AutoPolicy::Any => Self::Any,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    #[default]
    Release,
    Prerelease,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ControllerPolicy {
    #[default]
    Manual,
    WhenIdle,
    Window,
}

impl ControllerPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::WhenIdle => "when_idle",
            Self::Window => "window",
        }
    }
}

/// The opt-in scheduled auto-update configuration. Every field has a safe
/// default; `enabled` is false, so no update can happen without an explicit
/// `[auto_update] enabled = true` in `tuishou.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct AutoUpdateConfig {
    pub enabled: bool,
    /// Install prefix whose `.tuishou.json`, `.tuishou/` state and binaries are
    /// managed by automatic runs. CLI `--prefix` overrides this value.
    pub prefix: Option<PathBuf>,
    pub interval: String,
    pub components: Vec<String>,
    pub policy: AutoPolicy,
    pub min_release_age: String,
    pub channel: Channel,
    pub max_version: Option<String>,
    /// Attestation is mandatory in auto mode; `false` is a config error.
    pub require_attestation: bool,
    /// Workflow identity passed to `gh attestation verify --signer-workflow`.
    pub attestation_signer: Option<String>,
    pub controller_policy: ControllerPolicy,
    pub backup_dir: Option<PathBuf>,
    pub backup_keep: usize,
    pub window_start: Option<String>,
    pub window_end: Option<String>,
    pub drain_timeout: String,
    pub health_timeout: String,
    pub node_name: Option<String>,
    pub agent_unit: String,
    /// Controller systemd unit restarted after a verified controller update.
    pub controller_unit: String,
    /// Deadline for each individual `lu`/`systemctl` child process.
    pub command_timeout: String,
    /// Argument vector (never a shell string). The JSON summary is appended.
    pub notify_command: Vec<String>,
    pub notify_after_failures: u32,
}

impl Default for AutoUpdateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            prefix: None,
            interval: "10m".to_string(),
            components: vec!["lu".to_string(), "ting".to_string()],
            policy: AutoPolicy::Patch,
            min_release_age: "10m".to_string(),
            channel: Channel::Release,
            max_version: None,
            require_attestation: true,
            attestation_signer: None,
            controller_policy: ControllerPolicy::Manual,
            backup_dir: None,
            backup_keep: 5,
            window_start: None,
            window_end: None,
            drain_timeout: "5m".to_string(),
            health_timeout: "5m".to_string(),
            node_name: None,
            agent_unit: "lu-agent.service".to_string(),
            controller_unit: "lu.service".to_string(),
            command_timeout: "30s".to_string(),
            notify_command: Vec::new(),
            notify_after_failures: 3,
        }
    }
}

impl AutoUpdateConfig {
    pub fn validate(&self) -> Result<()> {
        let interval = parse_duration(&self.interval)?;
        if interval < Duration::from_secs(300) {
            return Err(Error::msg(format!(
                "auto_update.interval '{}' is below the 5m minimum",
                self.interval
            )));
        }
        parse_duration(&self.min_release_age)?;
        parse_duration(&self.drain_timeout)?;
        parse_duration(&self.health_timeout)?;
        if parse_duration(&self.command_timeout)?.is_zero() {
            return Err(Error::msg(
                "auto_update.command_timeout must be greater than zero",
            ));
        }
        if self.agent_unit.is_empty() || self.controller_unit.is_empty() {
            return Err(Error::msg(
                "auto_update.agent_unit and controller_unit must not be empty",
            ));
        }
        if !self.require_attestation {
            return Err(Error::msg(
                "auto_update.require_attestation cannot be false: auto mode always verifies attestations",
            ));
        }
        if self.controller_policy != ControllerPolicy::Manual && self.backup_dir.is_none() {
            return Err(Error::msg(
                "auto_update.backup_dir is required when controller_policy is not 'manual'",
            ));
        }
        if self.controller_policy != ControllerPolicy::Manual && self.backup_keep == 0 {
            return Err(Error::msg(
                "auto_update.backup_keep must be at least 1 when controller updates are enabled",
            ));
        }
        match (&self.window_start, &self.window_end) {
            (Some(a), Some(b)) => {
                crate::duration::parse_hhmm(a)?;
                crate::duration::parse_hhmm(b)?;
            }
            (None, None) => {}
            _ => {
                return Err(Error::msg(
                    "auto_update.window_start and window_end must be set together",
                ))
            }
        }
        if self.controller_policy == ControllerPolicy::Window
            && (self.window_start.is_none() || self.window_end.is_none())
        {
            return Err(Error::msg(
                "auto_update.controller_policy 'window' requires window_start and window_end",
            ));
        }
        if let Some(first) = self.notify_command.first() {
            if first.is_empty() {
                return Err(Error::msg(
                    "auto_update.notify_command must be an argument vector with a command",
                ));
            }
        }
        Ok(())
    }
}

pub fn default_config_path() -> PathBuf {
    PathBuf::from("tuishou.toml")
}

#[derive(Debug)]
pub struct LoadedConfig {
    pub components: Vec<Component>,
    pub auto_update: AutoUpdateConfig,
    /// Path of the config file as loaded; passed to child processes that read
    /// the same controller configuration (for example `lu backup --config`).
    pub config_path: PathBuf,
}

pub fn load_config(config_path: Option<&Path>) -> Result<LoadedConfig> {
    let mut components = builtin_components();
    let path = config_path
        .map(PathBuf::from)
        .unwrap_or_else(default_config_path);
    let cfg: ConfigFile = if !path.exists() {
        if config_path.is_some() {
            return Err(Error::Read {
                path,
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "config file not found"),
            });
        }
        ConfigFile {
            component: Vec::new(),
            auto_update: AutoUpdateConfig::default(),
        }
    } else {
        let text = std::fs::read_to_string(&path).map_err(|source| Error::Read {
            path: path.clone(),
            source,
        })?;
        toml::from_str(&text).map_err(|e| config_error(&path, &e.to_string()))?
    };
    cfg.auto_update.validate().map_err(|e| Error::Config {
        path: path.clone(),
        source: Box::new(e),
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
            if let Some(binary_ranges) = o.binary_ranges {
                existing.binary_ranges = binary_ranges;
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
                binary_ranges: o.binary_ranges.unwrap_or_default(),
            });
        }
    }
    Ok(LoadedConfig {
        components,
        auto_update: cfg.auto_update,
        config_path: path,
    })
}

fn config_error(path: &Path, message: &str) -> Error {
    if message.contains("unknown field") {
        let key = message
            .split_once("unknown field")
            .and_then(|(_, rest)| rest.split(['`', '\'', '"']).nth(1))
            .unwrap_or("unknown")
            .to_string();
        return Error::Config {
            path: path.to_path_buf(),
            source: Box::new(Error::UnknownConfigKey(key)),
        };
    }
    Error::Config {
        path: path.to_path_buf(),
        source: Box::new(Error::msg(message.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(text: &str) -> Result<ConfigFile> {
        toml::from_str(text).map_err(|e| Error::msg(e.to_string()))
    }

    #[test]
    fn defaults_are_safe() {
        let c = AutoUpdateConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.interval, "10m");
        assert_eq!(c.policy, AutoPolicy::Patch);
        assert_eq!(c.controller_policy, ControllerPolicy::Manual);
        assert!(c.require_attestation);
        c.validate().unwrap();
    }

    #[test]
    fn interval_below_five_minutes_rejected() {
        let c = AutoUpdateConfig {
            interval: "4m".to_string(),
            ..AutoUpdateConfig::default()
        };
        assert!(c.validate().unwrap_err().to_string().contains("5m minimum"));
    }

    #[test]
    fn unknown_key_in_auto_update_section_is_rejected() {
        assert!(cfg("[auto_update]\nbogus = 1\n").is_err());
    }

    #[test]
    fn require_attestation_false_rejected() {
        let c = AutoUpdateConfig {
            require_attestation: false,
            ..AutoUpdateConfig::default()
        };
        assert!(c
            .validate()
            .unwrap_err()
            .to_string()
            .contains("require_attestation"));
    }
}
