//! Applying an eligible release. Agents use the drain path from `roll`; the
//! controller is guarded by policy, idleness, backups and schema checks.

use crate::auto_update::state::AutoState;
use crate::config::{AutoUpdateConfig, ControllerPolicy};
use crate::duration::{in_window, parse_duration, parse_hhmm};
use crate::error::{Error, Result};
use crate::install::{self, InstallOptions};
use crate::logging::{journal, one_line};
use crate::release::SCHEMA_MARKER;
use crate::roll;
use crate::rollback;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};
use time::OffsetDateTime;

pub struct ApplyContext<'a> {
    pub cfg: &'a AutoUpdateConfig,
    pub prefix: Option<&'a Path>,
}

pub fn install_options(ctx: &ApplyContext<'_>, version: &str) -> InstallOptions {
    InstallOptions {
        version: Some(version.to_string()),
        prefix: ctx.prefix.map(Path::to_path_buf),
        dry_run: false,
        // Auto mode refuses unattested installs; the configuration validator
        // also rejects require_attestation = false.
        allow_unattested: false,
        allow_major: ctx.cfg.policy == crate::config::AutoPolicy::Any,
        max_download_bytes: crate::http::max_download_bytes(),
        quiet: true,
        verbose: false,
        smoke_check: true,
        signer_workflow: ctx.cfg.attestation_signer.clone(),
    }
}

pub fn plain_install(
    component: &crate::component::Component,
    version: &str,
    ctx: &ApplyContext<'_>,
) -> Result<()> {
    let outcome = install::install_component(
        &crate::http::agent(),
        component,
        &install_options(ctx, version),
    )?;
    journal(
        "component_installed",
        &[
            ("component", &outcome.component),
            ("version", &outcome.version),
            ("path", "plain"),
        ],
    );
    Ok(())
}

fn node_name(cfg: &AutoUpdateConfig) -> String {
    cfg.node_name
        .clone()
        .or_else(|| std::env::var("TUISHOU_NODE_NAME").ok())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "localhost".to_string())
}

fn run_tool(name: &str, args: &[&str]) -> Result<String> {
    journal(
        "tool",
        &[("command", name), ("args", &one_line(&args.join(" ")))],
    );
    let output = Command::new(name)
        .args(args)
        .output()
        .map_err(|e| Error::msg(format!("failed to run {name}: {e}")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = one_line(&String::from_utf8_lossy(&output.stderr));
        Err(Error::msg(format!(
            "{name} {} failed: {}",
            args.join(" "),
            if stderr.is_empty() {
                output.status.to_string()
            } else {
                stderr
            }
        )))
    }
}

fn poll_until(timeout: Duration, interval: Duration, mut check: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if check() {
            return true;
        }
        let elapsed = start.elapsed();
        if elapsed >= timeout {
            return false;
        }
        thread::sleep(interval.min(timeout - elapsed));
    }
}

fn undrain(node: &str) {
    if let Err(e) = run_tool("lu", &["node", "undrain", node]) {
        journal(
            "agent_undrain_failed",
            &[("reason", &one_line(&e.to_string()))],
        );
    } else {
        journal("agent_undrain", &[("node", node)]);
    }
}

fn restart(unit: &str) -> Result<()> {
    run_tool("systemctl", &["restart", unit])?;
    journal("agent_restart", &[("unit", unit)]);
    Ok(())
}

/// Apply to a running node agent: drain -> install -> restart -> health ->
/// undrain. Any health failure restores `.prev`, restarts, undrains and marks
/// the version bad so it is never retried.
pub fn apply_agent(
    component: &crate::component::Component,
    version: &str,
    ctx: &ApplyContext<'_>,
    state: &mut AutoState,
) -> Result<()> {
    let node = node_name(ctx.cfg);
    let unit = &ctx.cfg.agent_unit;
    journal(
        "agent_update_start",
        &[
            ("component", &component.name),
            ("version", version),
            ("node", &node),
            ("unit", unit),
        ],
    );
    run_tool("lu", &["node", "drain", &node])?;
    journal("agent_drain", &[("node", &node)]);

    let drain_timeout = parse_duration(&ctx.cfg.drain_timeout)?;
    let drained = poll_until(
        drain_timeout,
        Duration::from_millis(200),
        || match run_tool("lu", &["node", &node, "--json"]) {
            Ok(text) => roll::is_drained(&text),
            Err(e) => {
                journal(
                    "agent_drain_poll_failed",
                    &[("reason", &one_line(&e.to_string()))],
                );
                false
            }
        },
    );
    if !drained {
        undrain(&node);
        return Err(Error::msg(format!(
            "timed out after {} waiting for node {node} to drain",
            ctx.cfg.drain_timeout
        )));
    }
    journal("agent_drained", &[("node", &node)]);

    if let Err(e) = install::install_component(
        &crate::http::agent(),
        component,
        &install_options(ctx, version),
    ) {
        undrain(&node);
        return Err(e);
    }
    journal(
        "agent_installed",
        &[("component", &component.name), ("version", version)],
    );

    if let Err(e) = restart(unit) {
        undrain(&node);
        return Err(e);
    }

    let health_timeout = parse_duration(&ctx.cfg.health_timeout)?;
    let healthy = poll_until(health_timeout, Duration::from_millis(200), || {
        let active = run_tool("systemctl", &["is-active", "--quiet", unit]).is_ok();
        if !active {
            return false;
        }
        match run_tool("lu", &["node", &node, "--json"]) {
            Ok(text) => node_reports_version(&text, version),
            Err(_) => false,
        }
    });
    if healthy {
        undrain(&node);
        journal(
            "agent_update_ok",
            &[
                ("component", &component.name),
                ("version", version),
                ("node", &node),
            ],
        );
        return Ok(());
    }

    journal(
        "agent_health_failed",
        &[
            ("component", &component.name),
            ("version", version),
            ("node", &node),
        ],
    );
    // Restore the previous binaries before putting the node back in service.
    let rollback_result =
        rollback::rollback_binaries(&component.name, &component.binaries, ctx.prefix, true);
    if let Err(e) = rollback_result {
        journal(
            "agent_rollback_failed",
            &[("reason", &one_line(&e.to_string()))],
        );
    } else {
        journal(
            "agent_rollback",
            &[("component", &component.name), ("version", version)],
        );
    }
    let _ = restart(unit);
    undrain(&node);
    state
        .bad_versions
        .entry(component.name.clone())
        .or_default()
        .push(version.to_string());
    Err(Error::msg(format!(
        "health check failed within {} for {} on {node}; restored .prev and marked {version} bad",
        ctx.cfg.health_timeout, component.name
    )))
}

/// Whether a `lu node <name> --json` document reports `version`.
pub fn node_reports_version(json: &str, version: &str) -> bool {
    if json.contains(version) {
        return true;
    }
    match serde_json::from_str::<serde_json::Value>(json) {
        Ok(value) => value_has_version(&value, version),
        Err(_) => false,
    }
}

fn value_has_version(value: &serde_json::Value, version: &str) -> bool {
    match value {
        serde_json::Value::String(s) => s == version,
        serde_json::Value::Array(items) => items.iter().any(|v| value_has_version(v, version)),
        serde_json::Value::Object(map) => map.values().any(|v| value_has_version(v, version)),
        _ => false,
    }
}

/// Check controller preconditions. `Some(reason)` means "skip this cycle".
fn controller_skip_reason(cfg: &AutoUpdateConfig, body: &str) -> Result<Option<String>> {
    if cfg.controller_policy == ControllerPolicy::Manual {
        return Ok(Some(
            "controller_policy=manual: controller updates require a human".to_string(),
        ));
    }
    if cfg.controller_policy == ControllerPolicy::Window {
        let (start, end) = match (&cfg.window_start, &cfg.window_end) {
            (Some(s), Some(e)) => (parse_hhmm(s)?, parse_hhmm(e)?),
            _ => {
                return Err(Error::msg(
                    "controller_policy 'window' requires window_start and window_end",
                ))
            }
        };
        if !in_window(OffsetDateTime::now_utc(), start, end) {
            return Ok(Some(format!(
                "outside controller update window {}-{} UTC",
                cfg.window_start.as_deref().unwrap_or("?"),
                cfg.window_end.as_deref().unwrap_or("?")
            )));
        }
    }
    if crate::release::requires_new_database(body) {
        return Ok(Some(format!(
            "{SCHEMA_MARKER}: controller update requires manual migration"
        )));
    }
    let jobs = match run_tool("lu", &["job", "list", "--json"]) {
        Ok(text) => text,
        Err(e) => {
            return Ok(Some(format!("cannot check controller idleness: {e}")));
        }
    };
    if !controller_idle(&jobs) {
        return Ok(Some(
            "controller is not idle (jobs leased or running)".to_string(),
        ));
    }
    Ok(None)
}

pub fn controller_idle(json: &str) -> bool {
    let value: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return false,
    };
    for key in ["running_jobs", "runningJobs", "leased_jobs", "leasedJobs"] {
        if let Some(n) = value.get(key).and_then(|v| v.as_u64()) {
            if n > 0 {
                return false;
            }
        }
    }
    if let Some(items) = value
        .get("jobs")
        .and_then(|v| v.as_array())
        .or_else(|| value.get("nodes").and_then(|v| v.as_array()))
    {
        for item in items {
            let status = item
                .get("status")
                .or_else(|| item.get("state"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(
                status.as_str(),
                "running" | "leased" | "active" | "in_progress" | "inprogress" | "draining"
            ) {
                return false;
            }
        }
    }
    true
}

fn take_backup(cfg: &AutoUpdateConfig) -> Result<()> {
    let dir = cfg
        .backup_dir
        .as_ref()
        .ok_or_else(|| Error::msg("backup_dir is required for controller auto-updates"))?;
    fs::create_dir_all(dir).map_err(|source| Error::Write {
        path: dir.clone(),
        source,
    })?;
    let dir_arg = dir.to_string_lossy().to_string();
    run_tool("lu", &["backup", "--dir", &dir_arg])?;
    let mut entries: Vec<_> = fs::read_dir(dir)
        .map_err(|source| Error::Read {
            path: dir.clone(),
            source,
        })?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .collect();
    entries.sort_by_key(|e| {
        e.metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    });
    let keep = cfg.backup_keep;
    if entries.len() > keep {
        for old in &entries[..entries.len() - keep] {
            journal(
                "backup_pruned",
                &[("path", &one_line(&old.path().to_string_lossy()))],
            );
            let _ = fs::remove_file(old.path());
        }
    }
    journal(
        "controller_backup",
        &[("dir", &one_line(&dir_arg)), ("kept", &keep.to_string())],
    );
    Ok(())
}

/// Apply to the controller. Returns `Some(reason)` when the controller was
/// deliberately left alone (policy, window, busy, schema marker).
pub fn apply_controller(
    component: &crate::component::Component,
    version: &str,
    body: &str,
    ctx: &ApplyContext<'_>,
) -> Result<Option<String>> {
    if let Some(reason) = controller_skip_reason(ctx.cfg, body)? {
        journal(
            "controller_skipped",
            &[
                ("component", &component.name),
                ("version", version),
                ("reason", &one_line(&reason)),
            ],
        );
        return Ok(Some(reason));
    }
    take_backup(ctx.cfg)?;
    let outcome = install::install_component(
        &crate::http::agent(),
        component,
        &install_options(ctx, version),
    )?;
    journal(
        "controller_updated",
        &[
            ("component", &outcome.component),
            ("version", &outcome.version),
        ],
    );
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_version_detection() {
        assert!(node_reports_version(
            r#"{"name":"n1","version":"v0.4.1","drained":true}"#,
            "v0.4.1"
        ));
        assert!(!node_reports_version(
            r#"{"name":"n1","version":"v0.4.0"}"#,
            "v0.4.1"
        ));
    }

    #[test]
    fn idle_detection() {
        assert!(controller_idle(r#"{"running_jobs":0,"jobs":[]}"#));
        assert!(!controller_idle(r#"{"running_jobs":2}"#));
        assert!(!controller_idle(r#"{"jobs":[{"status":"leased"}]}"#));
    }
}
