use crate::component::Component;
use crate::error::Result;
use crate::release;
use crate::state;
use serde::Serialize;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Serialize)]
pub struct StatusEntry {
    pub component: String,
    pub installed_version: Option<String>,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub service_unit: Option<String>,
    pub service_active: Option<bool>,
}

pub fn collect_status(
    agent: &ureq::Agent,
    components: &[Component],
    prefix: Option<&Path>,
) -> Result<Vec<StatusEntry>> {
    let st = state::load(&state::default_state_path(prefix))?;
    let mut out = Vec::new();
    for c in components {
        let installed = st.components.get(&c.name).map(|i| i.version.clone());
        let latest = release::fetch_latest(agent, &c.repo).ok().map(|r| r.tag);
        let update_available = match (&installed, &latest) {
            (Some(i), Some(l)) => i != l,
            (None, Some(_)) => true,
            _ => false,
        };
        let service_active = c
            .service_unit
            .as_ref()
            .and_then(|u| check_service_active(u));
        out.push(StatusEntry {
            component: c.name.clone(),
            installed_version: installed,
            latest_version: latest,
            update_available,
            service_unit: c.service_unit.clone(),
            service_active,
        });
    }
    Ok(out)
}

fn check_service_active(unit: &str) -> Option<bool> {
    let systemctl = which("systemctl")?;
    let status = Command::new(systemctl)
        .arg("is-active")
        .arg("--quiet")
        .arg(unit)
        .status()
        .ok()?;
    Some(status.success())
}

fn which(name: &str) -> Option<String> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let cand = dir.join(name);
        if cand.is_file() {
            return Some(cand.to_string_lossy().to_string());
        }
    }
    None
}

pub fn print_status(entries: &[StatusEntry], json: bool, quiet: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(entries)
                .map_err(|e| crate::error::Error::msg(e.to_string()))?
        );
        return Ok(());
    }
    if quiet {
        return Ok(());
    }
    for e in entries {
        let installed = e.installed_version.as_deref().unwrap_or("-");
        let latest = e.latest_version.as_deref().unwrap_or("-");
        let upd = if e.update_available {
            "update-available"
        } else {
            "up-to-date"
        };
        let svc = match (&e.service_unit, e.service_active) {
            (Some(u), Some(true)) => format!("{u}:active"),
            (Some(u), Some(false)) => format!("{u}:inactive"),
            (Some(u), None) => format!("{u}:unknown"),
            _ => "-".to_string(),
        };
        println!(
            "{:<12} installed={:<12} latest={:<12} {} {}",
            e.component, installed, latest, upd, svc
        );
    }
    Ok(())
}
