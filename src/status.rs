use crate::auto_update::state;
use crate::component::Component;
use crate::error::{Error, Result};
use crate::report;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

pub type StatusEntry = report::ComponentReport;

/// Backwards-compatible collector used by `tuishou status`. The evaluation and
/// printed vocabulary are shared with `update --check` and auto-update.
pub fn collect_status(
    agent: &ureq::Agent,
    components: &[Component],
    prefix: Option<&Path>,
    hold: Option<&str>,
    bad: &BTreeMap<String, Vec<String>>,
) -> Result<Vec<StatusEntry>> {
    let installed = report::installed_versions(prefix)?;
    Ok(report::evaluate_all(
        agent, components, prefix, &installed, hold, bad,
    ))
}

/// Per-component result time, falling back to the last cycle's check time.
fn last_check_for<'a>(component: &str, auto: &'a state::AutoState) -> Option<&'a str> {
    auto.results
        .get(component)
        .map(|result| result.at.as_str())
        .or(auto.last_check.as_deref())
}

fn format_status_line(entry: &StatusEntry, last_check: Option<&str>) -> String {
    format!(
        "{}: {} | installed={} latest={} last_check={}",
        entry.component,
        entry.status,
        entry.installed_version.as_deref().unwrap_or("-"),
        entry.latest_version.as_deref().unwrap_or("-"),
        last_check.unwrap_or("never")
    )
}

#[derive(Serialize)]
struct JsonStatus<'a> {
    #[serde(flatten)]
    report: &'a StatusEntry,
    last_check: Option<&'a str>,
}

pub fn print_status(
    entries: &[StatusEntry],
    json: bool,
    quiet: bool,
    prefix: Option<&Path>,
) -> Result<()> {
    // A malformed auto-update state file must not hide the live status.
    let auto = state::load(prefix).unwrap_or_default();
    if json {
        let rows: Vec<JsonStatus> = entries
            .iter()
            .map(|entry| JsonStatus {
                report: entry,
                last_check: last_check_for(&entry.component, &auto),
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).map_err(|e| Error::msg(e.to_string()))?
        );
        return Ok(());
    }
    if quiet {
        return Ok(());
    }
    for entry in entries {
        println!(
            "{}",
            format_status_line(entry, last_check_for(&entry.component, &auto))
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::resolve_component;
    use crate::report::StatusText;

    #[test]
    fn status_line_carries_versions_and_check_time() {
        let component = resolve_component("lu", &crate::component::builtin_components()).unwrap();
        let mut entry = StatusEntry::new(
            &component,
            StatusText::UpdateAvailable {
                from: "v0.4.0".to_string(),
                to: "v0.5.0".to_string(),
            },
        );
        entry.installed_version = Some("v0.4.0".to_string());
        entry.latest_version = Some("v0.5.0".to_string());
        let line = format_status_line(&entry, Some("2026-01-02T03:04:05Z"));
        assert_eq!(
            line,
            "lu: update available v0.4.0 -> v0.5.0 | installed=v0.4.0 latest=v0.5.0 last_check=2026-01-02T03:04:05Z"
        );
        assert!(format_status_line(&entry, None).contains("last_check=never"));
    }
}
