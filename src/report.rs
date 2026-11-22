//! Shared per-component reporting. Every command that talks about components
//! uses this module so the table order and status vocabulary cannot drift.

use crate::component::Component;
use crate::install;
use crate::release;
use crate::state::{self, InstalledComponent};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusText {
    UpToDate { version: String },
    UpdateAvailable { from: String, to: String },
    Updated { from: String, to: String },
    NotInstalled { latest: String },
    NotInRelease { missing: Vec<String> },
    Held { reason: String },
    BadVersionSkipped { version: String },
    Skipped { reason: String },
    Failed { reason: String },
}

/// Rebuild a status from the serialised fields used by auto-update state.
pub fn status_text_from_kind(
    kind: &str,
    installed_version: Option<String>,
    reason: Option<String>,
    version: Option<String>,
    missing_binaries: Vec<String>,
) -> StatusText {
    let from = installed_version.unwrap_or_else(|| "-".to_string());
    match kind {
        "up_to_date" => StatusText::UpToDate {
            version: version.unwrap_or_default(),
        },
        "update_available" => StatusText::UpdateAvailable {
            from,
            to: version.unwrap_or_default(),
        },
        "updated" => StatusText::Updated {
            from,
            to: version.unwrap_or_default(),
        },
        "not_installed" => StatusText::NotInstalled {
            latest: version.unwrap_or_default(),
        },
        "not_in_release" => StatusText::NotInRelease {
            missing: missing_binaries,
        },
        "held" => StatusText::Held {
            reason: reason.unwrap_or_default(),
        },
        "bad_version_skipped" => StatusText::BadVersionSkipped {
            version: version.unwrap_or_default(),
        },
        "skipped" => StatusText::Skipped {
            reason: reason.unwrap_or_default(),
        },
        _ => StatusText::Failed {
            reason: reason.unwrap_or_default(),
        },
    }
}

impl StatusText {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::UpToDate { .. } => "up_to_date",
            Self::UpdateAvailable { .. } => "update_available",
            Self::Updated { .. } => "updated",
            Self::NotInstalled { .. } => "not_installed",
            Self::NotInRelease { .. } => "not_in_release",
            Self::Held { .. } => "held",
            Self::BadVersionSkipped { .. } => "bad_version_skipped",
            Self::Skipped { .. } => "skipped",
            Self::Failed { .. } => "failed",
        }
    }

    pub fn is_update_available(&self) -> bool {
        matches!(self, Self::UpdateAvailable { .. })
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }
}

impl fmt::Display for StatusText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UpToDate { version } => write!(f, "up to date ({version})"),
            Self::UpdateAvailable { from, to } => write!(f, "update available {from} -> {to}"),
            Self::Updated { from, to } => write!(f, "updated {from} -> {to}"),
            Self::NotInstalled { latest } => write!(f, "not installed (latest {latest})"),
            Self::NotInRelease { missing } => {
                write!(
                    f,
                    "not in this release (archive lacks: {})",
                    missing.join(", ")
                )
            }
            Self::Held { reason } => write!(f, "held ({reason})"),
            Self::BadVersionSkipped { version } => write!(f, "bad version skipped ({version})"),
            Self::Skipped { reason } => write!(f, "skipped ({reason})"),
            Self::Failed { reason } => write!(f, "failed ({reason})"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentReport {
    pub component: String,
    /// One of the fixed vocabulary phrases, without the component prefix.
    pub status: String,
    /// Machine-readable form of `status`.
    pub kind: String,
    pub installed_version: Option<String>,
    pub latest_version: Option<String>,
    pub missing_binaries: Vec<String>,
    pub reason: Option<String>,
    pub update_available: bool,
    pub service_unit: Option<String>,
    pub service_active: Option<bool>,
}

impl ComponentReport {
    pub fn new(component: &Component, status: StatusText) -> Self {
        Self::from_parts(component, status, None, None, Vec::new())
    }

    /// Build a report from an explicit outcome (used by install/update paths
    /// that already know the old and new versions).
    pub fn from_parts(
        component: &Component,
        status: StatusText,
        installed_version: Option<String>,
        latest_version: Option<String>,
        missing_binaries: Vec<String>,
    ) -> Self {
        let kind = status.kind().to_string();
        let text = status.to_string();
        let reason = match &status {
            StatusText::Held { reason }
            | StatusText::Skipped { reason }
            | StatusText::Failed { reason } => Some(reason.clone()),
            _ => None,
        };
        let has_update = status.is_update_available();
        Self {
            component: component.name.clone(),
            status: text,
            kind,
            installed_version,
            latest_version,
            missing_binaries,
            reason,
            update_available: has_update,
            service_unit: component.service_unit.clone(),
            service_active: component
                .service_unit
                .as_deref()
                .and_then(check_service_active),
        }
    }

    pub fn status_text(&self) -> StatusText {
        status_text_from_kind(
            &self.kind,
            self.installed_version.clone(),
            self.reason.clone(),
            self.latest_version.clone(),
            self.missing_binaries.clone(),
        )
    }
}

fn check_service_active(unit: &str) -> Option<bool> {
    let out = crate::process::run(
        "systemctl",
        &["is-active", "--quiet", unit],
        Duration::from_secs(10),
    )
    .ok()?;
    Some(out.status == 0)
}

/// Evaluate one component without letting a failure hide the others.
pub fn evaluate_component(
    agent: &ureq::Agent,
    component: &Component,
    installed: Option<&InstalledComponent>,
    hold: Option<&str>,
    bad: &[String],
) -> ComponentReport {
    if let Some(reason) = hold {
        return ComponentReport::new(
            component,
            StatusText::Held {
                reason: reason.to_string(),
            },
        );
    }

    let release = match release::fetch_latest(agent, &component.repo) {
        Ok(r) => r,
        Err(e) => {
            return ComponentReport::new(
                component,
                StatusText::Failed {
                    reason: e.to_string(),
                },
            )
        }
    };
    let installed_version = installed.map(|i| i.version.clone());
    let missing = match archive_missing_binaries(agent, component, &release.tag) {
        Ok(missing) => missing,
        Err(e) => {
            return ComponentReport::new(
                component,
                StatusText::Failed {
                    reason: e.to_string(),
                },
            )
        }
    };

    let status = if !missing.is_empty() {
        StatusText::NotInRelease { missing }
    } else if bad.iter().any(|tag| tag == &release.tag) {
        StatusText::BadVersionSkipped {
            version: release.tag.clone(),
        }
    } else {
        match installed_version.as_deref() {
            None => StatusText::NotInstalled {
                latest: release.tag.clone(),
            },
            Some(v) if v == release.tag => StatusText::UpToDate {
                version: release.tag.clone(),
            },
            Some(v) => StatusText::UpdateAvailable {
                from: v.to_string(),
                to: release.tag.clone(),
            },
        }
    };

    let mut report = ComponentReport::new(component, status);
    report.installed_version = installed_version;
    report.latest_version = Some(release.tag);
    report
}

pub fn archive_missing_binaries(
    agent: &ureq::Agent,
    component: &Component,
    tag: &str,
) -> crate::error::Result<Vec<String>> {
    let required = component.required_binaries(tag);
    if required.is_empty() {
        return Ok(Vec::new());
    }
    let present = install::archive_binary_names(agent, component, tag)?;
    Ok(required
        .iter()
        .filter(|name| !present.contains(name))
        .cloned()
        .collect())
}

/// Evaluate every default component in table order.
pub fn evaluate_all(
    agent: &ureq::Agent,
    components: &[Component],
    _prefix: Option<&Path>,
    installed: &BTreeMap<String, InstalledComponent>,
    hold: Option<&str>,
    bad: &BTreeMap<String, Vec<String>>,
) -> Vec<ComponentReport> {
    components
        .iter()
        .map(|c| {
            evaluate_component(
                agent,
                c,
                installed.get(&c.name),
                hold,
                bad.get(&c.name).map(Vec::as_slice).unwrap_or_default(),
            )
        })
        .collect()
}

pub fn installed_versions(
    prefix: Option<&Path>,
) -> crate::error::Result<BTreeMap<String, InstalledComponent>> {
    Ok(state::load(&state::default_state_path(prefix))?.components)
}

pub fn has_installable_update(reports: &[ComponentReport]) -> bool {
    reports.iter().any(|r| r.update_available)
}

pub fn has_failed(reports: &[ComponentReport]) -> bool {
    reports.iter().any(|r| r.status_text().is_failed())
}

pub fn print_reports(
    reports: &[ComponentReport],
    json: bool,
    quiet: bool,
) -> crate::error::Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(reports)
                .map_err(|e| crate::error::Error::msg(e.to_string()))?
        );
        return Ok(());
    }
    if quiet {
        return Ok(());
    }
    for report in reports {
        println!("{}: {}", report.component, report.status);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_phrases_match_the_fixed_vocabulary() {
        let cases = [
            (
                StatusText::UpToDate {
                    version: "v1".into(),
                },
                "up to date (v1)",
            ),
            (
                StatusText::UpdateAvailable {
                    from: "v1".into(),
                    to: "v2".into(),
                },
                "update available v1 -> v2",
            ),
            (
                StatusText::Updated {
                    from: "v1".into(),
                    to: "v2".into(),
                },
                "updated v1 -> v2",
            ),
            (
                StatusText::NotInstalled {
                    latest: "v2".into(),
                },
                "not installed (latest v2)",
            ),
            (
                StatusText::NotInRelease {
                    missing: vec!["reiki".into()],
                },
                "not in this release (archive lacks: reiki)",
            ),
            (
                StatusText::Held {
                    reason: "maintenance".into(),
                },
                "held (maintenance)",
            ),
            (
                StatusText::BadVersionSkipped {
                    version: "v2".into(),
                },
                "bad version skipped (v2)",
            ),
            (
                StatusText::Skipped {
                    reason: "policy".into(),
                },
                "skipped (policy)",
            ),
            (
                StatusText::Failed {
                    reason: "http".into(),
                },
                "failed (http)",
            ),
        ];
        for (status, expected) in cases {
            assert_eq!(status.to_string(), expected);
        }
    }

    #[test]
    fn unsupported_binaries_are_not_required_for_lu() {
        let lu = crate::component::resolve_component("lu", &crate::component::builtin_components())
            .unwrap();
        assert_eq!(lu.required_binaries("v0.4.0"), vec!["lu".to_string()]);
        assert!(lu
            .expected_binaries("v0.4.0")
            .contains(&"lu-agent".to_string()));
    }
}
