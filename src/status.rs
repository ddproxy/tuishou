use crate::component::Component;
use crate::error::Result;
use crate::report;
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

pub fn print_status(entries: &[StatusEntry], json: bool, quiet: bool) -> Result<()> {
    report::print_reports(entries, json, quiet)
}
