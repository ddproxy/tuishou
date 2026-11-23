//! Applying an eligible release. Agents use the real Lu drain path; the
//! controller is guarded by policy, idleness, backups and schema checks.

use crate::auto_update::state::{AutoState, ControllerBackup};
use crate::component::Component;
use crate::config::{AutoUpdateConfig, ControllerPolicy, VerifyConfig};
use crate::duration::{in_window, parse_duration, parse_hhmm};
use crate::error::{Error, Result};
use crate::install::{self, InstallOptions, InstallOutcome, StagedCheck};
use crate::logging::{journal, one_line};
use crate::process::{self, CommandOutput};
use crate::release::SCHEMA_MARKER;
use crate::rollback;
use crate::verify::AttestationVerifier;
use crate::{lu, state};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use time::OffsetDateTime;

/// Which Lu unit is installed and enabled on this host. This is detected from
/// systemd, never inferred from archive contents or configured assumptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LuRole {
    Agent,
    Controller,
}

impl LuRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Controller => "controller",
        }
    }
}

pub struct ApplyContext<'a> {
    pub cfg: &'a AutoUpdateConfig,
    pub prefix: Option<&'a Path>,
    /// Config path passed as `lu backup --config` so the controller backs up
    /// the same configuration the CLI loaded.
    pub config_path: Option<&'a Path>,
    /// Production callers inject the configured verifier; tests that replace
    /// `install` can leave it unset.
    #[allow(dead_code)]
    pub verifier: Option<Arc<dyn AttestationVerifier>>,
    #[allow(dead_code)]
    pub verify_config: Option<&'a VerifyConfig>,
}

/// Everything that touches the outside world during an apply, kept behind a
/// thin trait so failure paths can be tested without running real commands.
pub trait ApplyRunner {
    fn command(&self, program: &str, args: &[&str], timeout: Duration) -> Result<CommandOutput>;

    fn install(&self, component: &Component, opts: &InstallOptions) -> Result<InstallOutcome>;

    /// Regular-file basenames in the release archive, verified by checksum but
    /// not extracted. Used to avoid draining a node for a component that this
    /// release does not ship.
    fn archive_binaries(&self, component: &Component, version: &str) -> Result<Vec<String>>;

    /// Check a component's admin health endpoint. A non-2xx response is an
    /// unhealthy component, not a command failure.
    fn health_endpoint(&self, url: &str, timeout: Duration) -> std::result::Result<bool, String>;
}

pub struct SystemRunner;

impl ApplyRunner for SystemRunner {
    fn command(&self, program: &str, args: &[&str], timeout: Duration) -> Result<CommandOutput> {
        process::checked(program, args, timeout)
    }

    fn install(&self, component: &Component, opts: &InstallOptions) -> Result<InstallOutcome> {
        install::install_component(&crate::http::agent(), component, opts)
    }

    fn archive_binaries(&self, component: &Component, version: &str) -> Result<Vec<String>> {
        install::archive_binary_names(&crate::http::agent(), component, version)
    }

    fn health_endpoint(&self, url: &str, timeout: Duration) -> std::result::Result<bool, String> {
        crate::http::health_check(url, timeout).map_err(|e| e.to_string())
    }
}

pub fn install_options(
    ctx: &ApplyContext<'_>,
    version: &str,
    commit_state: bool,
) -> InstallOptions {
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
        // The staged file is smoke-checked before any binary is replaced.
        smoke_check: true,
        staged_check: None,
        commit_state,
        verifier: ctx
            .verifier
            .clone()
            .unwrap_or_else(crate::verify::default_verifier),
        verify_config: ctx.verify_config.cloned().unwrap_or_default(),
    }
}

pub fn plain_install(component: &Component, version: &str, ctx: &ApplyContext<'_>) -> Result<()> {
    let runner = SystemRunner;
    let outcome = runner.install(component, &install_options(ctx, version, true))?;
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

fn tool(
    runner: &dyn ApplyRunner,
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<CommandOutput> {
    journal(
        "tool",
        &[("command", program), ("args", &one_line(&args.join(" ")))],
    );
    runner.command(program, args, timeout)
}

fn checked(
    runner: &dyn ApplyRunner,
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<CommandOutput> {
    tool(runner, program, args, timeout)
}

fn poll_until(
    timeout: Duration,
    mut check: impl FnMut() -> std::result::Result<bool, String>,
) -> (bool, Option<String>) {
    let start = Instant::now();
    let mut last_error = None;
    loop {
        match check() {
            Ok(true) => return (true, None),
            Ok(false) => {
                last_error.take();
            }
            Err(e) => last_error = Some(e),
        }
        if start.elapsed() >= timeout {
            return (false, last_error);
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn command_timeout(ctx: &ApplyContext<'_>) -> Result<Duration> {
    parse_duration(&ctx.cfg.command_timeout)
}

/// A unit only counts when its file exists (`systemctl cat`) and it is enabled
/// (`systemctl is-enabled --quiet`). Both checks must succeed; a missing,
/// disabled or ambiguous pair is never guessed at.
fn unit_present_and_enabled(runner: &dyn ApplyRunner, unit: &str, timeout: Duration) -> bool {
    checked(runner, "systemctl", &["cat", unit], timeout).is_ok()
        && checked(
            runner,
            "systemctl",
            &["is-enabled", "--quiet", unit],
            timeout,
        )
        .is_ok()
}

pub fn detect_lu_role_with_runner(
    ctx: &ApplyContext<'_>,
    runner: &dyn ApplyRunner,
) -> Result<LuRole> {
    let timeout = command_timeout(ctx)?;
    let agent = unit_present_and_enabled(runner, &ctx.cfg.agent_unit, timeout);
    let controller = unit_present_and_enabled(runner, &ctx.cfg.controller_unit, timeout);
    let role = match (agent, controller) {
        (true, false) => LuRole::Agent,
        (false, true) => LuRole::Controller,
        (true, true) => {
            return Err(Error::msg(format!(
                "both {} and {} are installed and enabled; refusing to guess the Lu role",
                ctx.cfg.agent_unit, ctx.cfg.controller_unit
            )))
        }
        (false, false) => {
            return Err(Error::msg(format!(
                "neither {} nor {} is installed and enabled; refusing to guess the Lu role",
                ctx.cfg.agent_unit, ctx.cfg.controller_unit
            )))
        }
    };
    journal("lu_role_detected", &[("role", role.as_str())]);
    Ok(role)
}

pub fn detect_lu_role(ctx: &ApplyContext<'_>) -> Result<LuRole> {
    detect_lu_role_with_runner(ctx, &SystemRunner)
}

/// Apply a Lu release according to the detected systemd role. The archive is
/// the same for both; only the installed/enabled unit decides what is
/// restarted and which safety procedure runs.
pub fn apply_lu(
    component: &Component,
    version: &str,
    body: &str,
    role: LuRole,
    ctx: &ApplyContext<'_>,
    state: &mut AutoState,
) -> Result<Option<String>> {
    match role {
        LuRole::Agent => apply_agent(component, version, ctx, state),
        LuRole::Controller => apply_controller(component, version, body, ctx, state),
    }
}

/// The controller is reachable when `lu status --json` returns a complete
/// status document. An unreachable controller means there is no drain to do.
fn controller_reachable(runner: &dyn ApplyRunner, ctx: &ApplyContext<'_>) -> bool {
    match checked(
        runner,
        "lu",
        &["status", "--json"],
        command_timeout(ctx).unwrap_or(Duration::from_secs(30)),
    ) {
        Ok(out) => lu::status_available(&out.stdout),
        Err(_) => false,
    }
}

fn undrain(runner: &dyn ApplyRunner, node: &str, ctx: &ApplyContext<'_>) -> Result<()> {
    checked(runner, "lu", &["undrain", node], command_timeout(ctx)?)?;
    journal("agent_undrain", &[("node", node)]);
    Ok(())
}

fn restart(runner: &dyn ApplyRunner, unit: &str, ctx: &ApplyContext<'_>) -> Result<()> {
    checked(
        runner,
        "systemctl",
        &["restart", unit],
        command_timeout(ctx)?,
    )?;
    journal("service_restart", &[("unit", unit)]);
    Ok(())
}

fn drain_and_wait(runner: &dyn ApplyRunner, node: &str, ctx: &ApplyContext<'_>) -> Result<()> {
    let cmd_timeout = command_timeout(ctx)?;
    if let Err(e) = checked(runner, "lu", &["drain", node], cmd_timeout) {
        let _ = undrain(runner, node, ctx);
        return Err(Error::msg(format!(
            "failed to drain node {node}: {}",
            one_line(&e.to_string())
        )));
    }
    journal("agent_drain", &[("node", node)]);

    let drain_timeout = parse_duration(&ctx.cfg.drain_timeout)?;
    let (drained, drain_error) = poll_until(drain_timeout, || {
        match checked(runner, "lu", &["nodes", "--json"], cmd_timeout) {
            Ok(out) => {
                if lu::nodes_report_drained(&out.stdout, node) {
                    Ok(true)
                } else {
                    journal("agent_drain_pending", &[("node", node)]);
                    Ok(false)
                }
            }
            Err(e) => {
                journal(
                    "agent_drain_poll_failed",
                    &[("reason", &one_line(&e.to_string()))],
                );
                Err(e.to_string())
            }
        }
    });
    if !drained {
        let _ = undrain(runner, node, ctx);
        return Err(Error::msg(format!(
            "timed out after {} waiting for node {node} to drain{}",
            ctx.cfg.drain_timeout,
            detail_suffix(drain_error)
        )));
    }
    journal("agent_drained", &[("node", node)]);
    Ok(())
}

fn reiki_config_path(component: &Component, cfg: &AutoUpdateConfig) -> PathBuf {
    component
        .config_path()
        .map(Path::to_path_buf)
        .or_else(|| cfg.reiki_config_path.clone())
        .unwrap_or_else(|| PathBuf::from("/var/lib/reiki/reiki.toml"))
}

/// The config check is attached to the staged binary so it runs before any
/// installed file is replaced. A failure leaves the old binary in place.
fn reiki_staged_check(component: &Component, cfg: &AutoUpdateConfig) -> StagedCheck {
    let path = reiki_config_path(component, cfg);
    StagedCheck {
        binary: "reiki".to_string(),
        args: vec![
            "check".to_string(),
            "--config".to_string(),
            path.to_string_lossy().into_owned(),
        ],
    }
}

fn reiki_check(
    runner: &dyn ApplyRunner,
    component: &Component,
    ctx: &ApplyContext<'_>,
) -> Result<()> {
    let path = reiki_config_path(component, ctx.cfg);
    checked(
        runner,
        "reiki",
        &["check", "--config", &path.to_string_lossy()],
        command_timeout(ctx)?,
    )?;
    journal(
        "reiki_config_check",
        &[("config", &one_line(&path.to_string_lossy()))],
    );
    Ok(())
}

fn reiki_healthy(
    runner: &dyn ApplyRunner,
    component: &Component,
    ctx: &ApplyContext<'_>,
) -> std::result::Result<bool, String> {
    let unit = component.service_unit.as_deref().unwrap_or("reiki.service");
    let timeout = command_timeout(ctx).map_err(|e| e.to_string())?;
    let active = checked(
        runner,
        "systemctl",
        &["is-active", "--quiet", unit],
        timeout,
    )
    .map_err(|e| e.to_string())?;
    if active.status != 0 {
        return Ok(false);
    }
    reiki_check(runner, component, ctx).map_err(|e| e.to_string())?;
    let url = crate::http::redact_secrets(&ctx.cfg.reiki_health_url);
    let healthy = runner.health_endpoint(&url, timeout)?;
    journal(
        "reiki_health_endpoint",
        &[("url", &one_line(&url)), ("healthy", &healthy.to_string())],
    );
    Ok(healthy)
}

fn node_runs_lu(runner: &dyn ApplyRunner, node: &str, ctx: &ApplyContext<'_>) -> bool {
    let timeout = match command_timeout(ctx) {
        Ok(t) => t,
        Err(_) => return false,
    };
    // A host is a Lu agent node only when its agent unit is installed and enabled. Being able to
    // reach a controller with the operator CLI says nothing about this host (a workstation with
    // the CLI would otherwise be drained and have a unit restarted), and a node name found in the
    // controller's list is not enough on its own either: the unit is what gets restarted.
    if !unit_present_and_enabled(runner, &ctx.cfg.agent_unit, timeout) {
        return false;
    }
    // The controller's node list is not consulted: the apply path drains by name and fails closed
    // when the controller does not know it, and an unreachable controller is handled there too.
    let _ = node;
    true
}

fn node_healthy(
    runner: &dyn ApplyRunner,
    node: &str,
    version: &str,
    ctx: &ApplyContext<'_>,
) -> std::result::Result<bool, String> {
    let timeout = command_timeout(ctx).map_err(|e| e.to_string())?;
    let active = checked(
        runner,
        "systemctl",
        &["is-active", "--quiet", &ctx.cfg.agent_unit],
        timeout,
    )
    .map(|_| true)
    .map_err(|e| e.to_string())?;
    if !active {
        return Ok(false);
    }
    let out = checked(runner, "lu", &["nodes", "--json"], timeout).map_err(|e| e.to_string())?;
    let heartbeat_max_age =
        parse_duration(&ctx.cfg.heartbeat_max_age).map_err(|e| e.to_string())?;
    Ok(lu::node_healthy(
        &out.stdout,
        node,
        version,
        heartbeat_max_age,
    ))
}

fn controller_healthy(
    runner: &dyn ApplyRunner,
    version: &str,
    ctx: &ApplyContext<'_>,
) -> std::result::Result<bool, String> {
    let timeout = command_timeout(ctx).map_err(|e| e.to_string())?;
    checked(
        runner,
        "systemctl",
        &["is-active", "--quiet", &ctx.cfg.controller_unit],
        timeout,
    )
    .map_err(|e| e.to_string())?;
    let out = checked(runner, "lu", &["status", "--json"], timeout).map_err(|e| e.to_string())?;
    Ok(lu::status_reports_version(&out.stdout, version))
}

fn mark_bad(state: &mut AutoState, component: &str, version: &str, prefix: Option<&Path>) {
    let entry = state.bad_versions.entry(component.to_string()).or_default();
    if !entry.iter().any(|v| v == version) {
        entry.push(version.to_string());
    }
    // Persist immediately: a later recovery failure must not lose the mark.
    let _ = crate::auto_update::state::save(prefix, state);
}

fn receipt(outcome: &InstallOutcome) -> Result<&crate::install::InstallReceipt> {
    outcome.receipt.as_ref().ok_or_else(|| {
        Error::msg(format!(
            "install of {} returned no receipt; refusing to record state",
            outcome.component
        ))
    })
}

fn previous_version(component: &Component, prefix: Option<&Path>) -> Option<String> {
    state::load(&state::default_state_path(prefix))
        .ok()?
        .components
        .get(&component.name)
        .map(|c| c.version.clone())
}

/// Apply to a running node agent: drain -> install -> restart -> health ->
/// recheck drain -> commit state -> undrain. Health, drain recheck and state
/// commit are one transaction: any failure restores `.prev`, restarts, verifies
/// restored health, marks the version bad and leaves the node drained if
/// recovery fails.
/// Returns `Some(reason)` when the release archive does not contain an agent
/// binary for this component. Such a release is skipped, not failed: draining
/// and restarting a node would be pointless and disruptive.
fn component_binary_skip_reason(
    component: &Component,
    version: &str,
    runner: &dyn ApplyRunner,
) -> Result<Option<String>> {
    let expected = component.required_binaries(version);
    if expected.is_empty() {
        return Ok(Some(format!(
            "{version} has no expected binaries for {}; not in this release",
            component.name
        )));
    }
    let present = runner.archive_binaries(component, version)?;
    if expected.iter().any(|name| present.contains(name)) {
        return Ok(None);
    }
    Ok(Some(format!(
        "{version} does not contain any of ({}); not in this release",
        expected.join(", ")
    )))
}

pub fn apply_agent(
    component: &Component,
    version: &str,
    ctx: &ApplyContext<'_>,
    state: &mut AutoState,
) -> Result<Option<String>> {
    apply_agent_with_runner(component, version, ctx, state, &SystemRunner)
}

/// Manual `update` uses the same drain/install/restart/health/rollback/undrain
/// procedure as auto-update when the host is a Lu node with an agent to
/// update. Returns `false` when this is not such a node (or the release has no
/// agent binary), so the caller can fall back to a plain install.
pub fn apply_agent_on_lu_node(
    component: &Component,
    version: &str,
    ctx: &ApplyContext<'_>,
    state: &mut AutoState,
) -> Result<bool> {
    let node = node_name(ctx.cfg);
    if !node_runs_lu(&SystemRunner, &node, ctx) {
        return Ok(false);
    }
    Ok(apply_agent_with_runner(component, version, ctx, state, &SystemRunner)?.is_none())
}

pub fn apply_agent_with_runner(
    component: &Component,
    version: &str,
    ctx: &ApplyContext<'_>,
    state: &mut AutoState,
    runner: &dyn ApplyRunner,
) -> Result<Option<String>> {
    if let Some(reason) = component_binary_skip_reason(component, version, runner)? {
        journal(
            "agent_skipped",
            &[
                ("component", &component.name),
                ("version", version),
                ("reason", &one_line(&reason)),
            ],
        );
        return Ok(Some(reason));
    }
    let node = node_name(ctx.cfg);
    let unit = ctx.cfg.agent_unit.clone();
    // Drain only when the controller can confirm the drain. An unreachable
    // controller is not a reason to restart an agent without coordination.
    let drained = controller_reachable(runner, ctx);
    journal(
        "agent_update_start",
        &[
            ("component", &component.name),
            ("version", version),
            ("node", &node),
            ("unit", &unit),
            ("drain", &drained.to_string()),
        ],
    );

    // `drain_and_wait` undrains itself if the drain or its deadline fails.
    if drained {
        drain_and_wait(runner, &node, ctx)?;
    }

    let opts = install_options(ctx, version, false);
    let outcome = match runner.install(component, &opts) {
        Ok(o) => o,
        Err(e) => {
            if drained {
                let _ = undrain(runner, &node, ctx);
            }
            return Err(e);
        }
    };
    journal(
        "agent_installed",
        &[("component", &component.name), ("version", version)],
    );

    let health_timeout = parse_duration(&ctx.cfg.health_timeout)?;
    let mut startup_error = None;
    if let Err(e) = restart(runner, &unit, ctx) {
        startup_error = Some(e.to_string());
    }
    let (healthy, health_error) = if startup_error.is_some() {
        (false, startup_error)
    } else {
        poll_until(health_timeout, || node_healthy(runner, &node, version, ctx))
    };
    if healthy {
        let mut commit_error = None;
        if drained {
            // The drain is part of the apply transaction: re-check it before
            // recording the new version or releasing the node.
            match checked(runner, "lu", &["nodes", "--json"], command_timeout(ctx)?) {
                Ok(out) if lu::nodes_report_drained(&out.stdout, &node) => {}
                Ok(_) => {
                    commit_error = Some(format!(
                        "node {node} no longer reports drained with no active job before state commit"
                    ));
                }
                Err(e) => {
                    commit_error = Some(format!(
                        "cannot re-check drain state for node {node} before state commit: {}",
                        one_line(&e.to_string())
                    ));
                }
            }
        }
        if commit_error.is_none() {
            // A failed state commit leaves the new binary live with a stale
            // recorded version, so it is a transaction failure too.
            if let Err(e) = receipt(&outcome)
                .and_then(|r| install::record_installed(component, version, r, ctx.prefix))
            {
                commit_error = Some(format!("state commit failed: {}", one_line(&e.to_string())));
            }
        }
        if let Some(reason) = commit_error {
            journal(
                "agent_commit_recheck_failed",
                &[("node", &node), ("reason", &one_line(&reason))],
            );
            return recover_agent(
                component,
                version,
                &node,
                drained,
                ctx,
                state,
                runner,
                &reason,
                health_timeout,
            )
            .map(|()| None);
        }
        if drained {
            undrain(runner, &node, ctx)?;
        }
        journal(
            "agent_update_ok",
            &[
                ("component", &component.name),
                ("version", version),
                ("node", &node),
            ],
        );
        return Ok(None);
    }

    let health_reason = health_error.unwrap_or_else(|| {
        format!(
            "node {node} did not report version {version} with a fresh heartbeat within {}",
            ctx.cfg.health_timeout
        )
    });
    recover_agent(
        component,
        version,
        &node,
        drained,
        ctx,
        state,
        runner,
        &health_reason,
        health_timeout,
    )
    .map(|()| None)
}

/// Restore the previous version after a failed start or health check. A drain
/// is always released, including when recovery itself fails; the bad-version
/// mark is persisted before any recovery work so a later retry cannot miss it.
#[allow(clippy::too_many_arguments)]
fn recover_agent(
    component: &Component,
    version: &str,
    node: &str,
    drained: bool,
    ctx: &ApplyContext<'_>,
    state: &mut AutoState,
    runner: &dyn ApplyRunner,
    failure_reason: &str,
    health_timeout: Duration,
) -> Result<()> {
    journal(
        "agent_health_failed",
        &[
            ("component", &component.name),
            ("version", version),
            ("node", node),
            ("reason", &one_line(failure_reason)),
        ],
    );
    mark_bad(state, &component.name, version, ctx.prefix);

    let mut recovery_errors = Vec::new();
    if let Err(e) = rollback::restore_binaries(component, ctx.prefix, false) {
        recovery_errors.push(format!("restore .prev failed: {e}"));
    }
    if let Err(e) = restart(runner, &ctx.cfg.agent_unit, ctx) {
        recovery_errors.push(format!("restart failed: {e}"));
    }
    if recovery_errors.is_empty() {
        match previous_version(component, ctx.prefix) {
            Some(old) => {
                let (ok, err) =
                    poll_until(health_timeout, || node_healthy(runner, node, &old, ctx));
                if !ok {
                    recovery_errors.push(format!(
                        "restored node did not report version {old}{}",
                        detail_suffix(err)
                    ));
                }
            }
            None => recovery_errors.push("no previous version is recorded for verification".into()),
        }
    }
    if drained {
        if let Err(e) = undrain(runner, node, ctx) {
            recovery_errors.push(format!("undrain failed: {e}"));
        }
    }

    if !recovery_errors.is_empty() {
        return Err(Error::msg(format!(
            "health check failed within {} for {} on {node}: {}; marked {version} bad; recovery failed: {}",
            ctx.cfg.health_timeout,
            component.name,
            failure_reason,
            one_line(&recovery_errors.join("; "))
        )));
    }
    Err(Error::msg(format!(
        "health check failed within {} for {} on {node}: {}; restored .prev, verified restored health, marked {version} bad and undrained",
        ctx.cfg.health_timeout,
        component.name,
        failure_reason
    )))
}

/// Apply to Reiki. On a node that runs Lu this follows the same drain/deadline
/// discipline as the agent path, then checks Reiki's config before restarting
/// its unit and polling health. Failures restore the old binary and mark the
/// version bad; a drained node is only undrained after restored health is
/// verified. Failed recovery leaves it drained for operator action.
pub fn apply_reiki(
    component: &Component,
    version: &str,
    ctx: &ApplyContext<'_>,
    state: &mut AutoState,
) -> Result<Option<String>> {
    apply_reiki_with_runner(component, version, ctx, state, &SystemRunner)
}

pub fn apply_reiki_with_runner(
    component: &Component,
    version: &str,
    ctx: &ApplyContext<'_>,
    state: &mut AutoState,
    runner: &dyn ApplyRunner,
) -> Result<Option<String>> {
    let opts = install_options(ctx, version, false);
    apply_reiki_with_options(component, version, ctx, state, runner, opts)
}

/// Reiki apply with caller-supplied install options. Auto-update uses the
/// restricted options above; the CLI passes its own flags but still gets the
/// same drain/config-check/restart/health/rollback path.
pub fn apply_reiki_with_options(
    component: &Component,
    version: &str,
    ctx: &ApplyContext<'_>,
    state: &mut AutoState,
    runner: &dyn ApplyRunner,
    opts: InstallOptions,
) -> Result<Option<String>> {
    if let Some(reason) = component_binary_skip_reason(component, version, runner)? {
        journal(
            "reiki_skipped",
            &[
                ("component", &component.name),
                ("version", version),
                ("reason", &one_line(&reason)),
            ],
        );
        return Ok(Some(reason));
    }

    // A host that has neither a Reiki configuration nor an enabled Reiki unit is not running
    // Reiki (a workstation with the binary, for example): swap the binary and stop. Checking a
    // configuration that does not exist would fail and wrongly mark the release as bad.
    let timeout = command_timeout(ctx)?;
    let unit = component.service_unit.as_deref().unwrap_or("reiki.service");
    let config_present = reiki_config_path(component, ctx.cfg).exists();
    if !config_present && !unit_present_and_enabled(runner, unit, timeout) {
        journal(
            "reiki_plain_install",
            &[("component", &component.name), ("version", version)],
        );
        // Nothing else will record this install, so commit the state here.
        let mut plain = opts.clone();
        plain.commit_state = true;
        runner.install(component, &plain)?;
        return Ok(None);
    }

    let node = node_name(ctx.cfg);
    let drained = node_runs_lu(runner, &node, ctx);
    journal(
        "reiki_update_start",
        &[
            ("component", &component.name),
            ("version", version),
            ("node", &node),
            ("drain", &drained.to_string()),
        ],
    );
    if drained {
        drain_and_wait(runner, &node, ctx)?;
    }

    let mut opts = opts;
    opts.staged_check = Some(reiki_staged_check(component, ctx.cfg));

    let outcome = match runner.install(component, &opts) {
        Ok(outcome) => outcome,
        Err(e) if matches!(e, Error::StagedCheckFailed { .. }) => {
            // No file was replaced: keep the old binary in place, only record
            // that this release must not be retried.
            journal(
                "reiki_staged_check_failed",
                &[("component", &component.name), ("version", version)],
            );
            mark_bad(state, &component.name, version, ctx.prefix);
            if drained {
                let _ = undrain(runner, &node, ctx);
            }
            return Err(Error::msg(format!(
                "{e}; marked {version} bad and old reiki binary kept"
            )));
        }
        Err(e) => {
            return recover_reiki(
                component,
                version,
                &node,
                drained,
                ctx,
                state,
                runner,
                &e.to_string(),
            )
        }
    };

    let install_result = (|| -> Result<()> {
        restart(
            runner,
            component.service_unit.as_deref().unwrap_or("reiki.service"),
            ctx,
        )?;
        let health_timeout = parse_duration(&ctx.cfg.health_timeout)?;
        let (healthy, health_error) =
            poll_until(health_timeout, || reiki_healthy(runner, component, ctx));
        if !healthy {
            return Err(Error::msg(format!(
                "reiki health check failed within {} for {version}{}",
                ctx.cfg.health_timeout,
                detail_suffix(health_error)
            )));
        }
        install::record_installed(component, version, receipt(&outcome)?, ctx.prefix)?;
        Ok(())
    })();
    if let Err(e) = install_result {
        return recover_reiki(
            component,
            version,
            &node,
            drained,
            ctx,
            state,
            runner,
            &e.to_string(),
        );
    }

    if drained {
        undrain(runner, &node, ctx)?;
    }
    journal(
        "reiki_update_ok",
        &[("component", &component.name), ("version", version)],
    );
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn recover_reiki(
    component: &Component,
    version: &str,
    node: &str,
    drained: bool,
    ctx: &ApplyContext<'_>,
    state: &mut AutoState,
    runner: &dyn ApplyRunner,
    failure_reason: &str,
) -> Result<Option<String>> {
    journal(
        "reiki_health_failed",
        &[
            ("component", &component.name),
            ("version", version),
            ("node", node),
            ("reason", &one_line(failure_reason)),
        ],
    );
    mark_bad(state, &component.name, version, ctx.prefix);

    let mut recovery_errors = Vec::new();
    if let Err(e) = rollback::restore_binaries(component, ctx.prefix, false) {
        recovery_errors.push(format!("restore .prev failed: {e}"));
    }
    if let Err(e) = restart(
        runner,
        component.service_unit.as_deref().unwrap_or("reiki.service"),
        ctx,
    ) {
        recovery_errors.push(format!("restart failed: {e}"));
    }
    if recovery_errors.is_empty() {
        let health_timeout = parse_duration(&ctx.cfg.health_timeout)?;
        let (healthy, health_error) =
            poll_until(health_timeout, || reiki_healthy(runner, component, ctx));
        if !healthy {
            recovery_errors.push(format!(
                "restored reiki did not pass health check{}",
                detail_suffix(health_error)
            ));
        }
    }
    let mut left_drained = false;
    if drained {
        if recovery_errors.is_empty() {
            if let Err(e) = undrain(runner, node, ctx) {
                recovery_errors.push(format!("undrain failed: {e}"));
                left_drained = true;
            }
        } else {
            // Never release a node whose recovery is unverified.
            left_drained = true;
        }
    }
    if !recovery_errors.is_empty() {
        let drain_note = if left_drained {
            format!("; node {node} left drained for operator action")
        } else {
            String::new()
        };
        return Err(Error::msg(format!(
            "reiki update failed for {version}: {}; recovery failed: {}{}",
            one_line(failure_reason),
            one_line(&recovery_errors.join("; ")),
            drain_note
        )));
    }
    Err(Error::msg(format!(
        "reiki update failed for {version}: {}; restored .prev, verified restored health, marked {version} bad and undrained",
        one_line(failure_reason)
    )))
}

fn detail_suffix(detail: Option<String>) -> String {
    detail
        .filter(|d| !d.trim().is_empty())
        .map(|d| format!(" (last error: {})", one_line(&d)))
        .unwrap_or_default()
}

/// Check controller preconditions. `Some(reason)` means "skip this cycle".
fn controller_skip_reason(
    cfg: &AutoUpdateConfig,
    body: &str,
    runner: &dyn ApplyRunner,
    cmd_timeout: Duration,
) -> Result<Option<String>> {
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
    if body.trim().is_empty() {
        return Ok(Some(
            "controller release notes are missing; refusing to cross an unverified schema change"
                .to_string(),
        ));
    }
    let wait_timeout = parse_duration(&cfg.controller_wait_timeout)?;
    journal(
        "controller_wait_start",
        &[("timeout", &cfg.controller_wait_timeout)],
    );
    let (idle, last_error) = poll_until(wait_timeout, || {
        match checked(runner, "lu", &["status", "--json"], cmd_timeout) {
            Ok(out) if lu::controller_idle(&out.stdout) => Ok(true),
            Ok(_) => {
                journal("controller_wait_busy", &[]);
                Ok(false)
            }
            Err(e) => Err(e.to_string()),
        }
    });
    if !idle {
        return Ok(Some(match last_error {
            Some(e) => format!("cannot check controller idleness: {}", one_line(&e)),
            None => format!(
                "controller is not idle after {} (jobs leased, running, recovering or needing input)",
                cfg.controller_wait_timeout
            ),
        }));
    }
    journal("controller_idle", &[]);
    Ok(None)
}

/// Unpredictable per-backup suffix. `/dev/urandom` is used where available;
/// the fallback only has to avoid collisions, not to be a secret.
fn random_suffix() -> String {
    #[cfg(unix)]
    {
        use std::io::Read as _;
        if let Ok(mut source) = fs::File::open("/dev/urandom") {
            let mut bytes = [0u8; 8];
            if source.read_exact(&mut bytes).is_ok() {
                return bytes.iter().map(|b| format!("{b:02x}")).collect();
            }
        }
    }
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seed = format!(
        "{}:{}:{}",
        std::process::id(),
        nanos,
        COUNTER.fetch_add(1, AtomicOrdering::Relaxed)
    );
    let mut hash = 0xcbf29ce484222325u64;
    for byte in seed.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn backup_file_name() -> String {
    let now = OffsetDateTime::now_utc();
    format!(
        "lu-backup-{:04}{:02}{:02}T{:02}{:02}{:02}Z-{}.sql",
        now.year(),
        now.month() as u8,
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        random_suffix()
    )
}

fn create_backup_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(dir).map_err(|source| Error::Write {
            path: dir.to_path_buf(),
            source,
        })?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|source| {
            Error::Write {
                path: dir.to_path_buf(),
                source,
            }
        })?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(dir).map_err(|source| Error::Write {
        path: dir.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn create_backup_file(path: &Path) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|source| Error::Write {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// Hash a backup after checking it is a regular, non-empty, 0600, readable
/// file. Symlinks and unusual file types fail closed.
fn backup_checksum(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path).map_err(|source| Error::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.file_type().is_file() {
        return Err(Error::msg(format!(
            "backup {} is not a regular file",
            path.display()
        )));
    }
    if metadata.len() == 0 {
        return Err(Error::msg(format!("backup {} is empty", path.display())));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode() & 0o777;
        if mode != 0o600 {
            return Err(Error::msg(format!(
                "backup {} must be mode 0600, found {mode:04o}",
                path.display()
            )));
        }
    }
    crate::verify::sha256_file(path)
}

fn verify_backup_record(record: &ControllerBackup) -> Result<()> {
    let checksum = backup_checksum(&record.path)?;
    if checksum != record.checksum {
        return Err(Error::msg(format!(
            "backup {} no longer matches the recorded checksum",
            record.path.display()
        )));
    }
    Ok(())
}

/// Back up the controller before installing a new binary. The real Lu 0.4
/// interface is `lu backup --out <FILE>`; a failed or unusable backup refuses
/// the update so a controller is never replaced without a restore point.
fn take_backup(
    cfg: &AutoUpdateConfig,
    runner: &dyn ApplyRunner,
    cmd_timeout: Duration,
    config_path: Option<&Path>,
) -> Result<ControllerBackup> {
    let dir = cfg
        .backup_dir
        .as_ref()
        .ok_or_else(|| Error::msg("backup_dir is required for controller auto-updates"))?;
    if cfg.backup_keep == 0 {
        return Err(Error::msg(
            "backup_keep must be at least 1 to back up the controller",
        ));
    }
    create_backup_dir(dir)?;
    let path = dir.join(backup_file_name());
    create_backup_file(&path)?;

    let out_arg = path.to_string_lossy().into_owned();
    let config_arg = config_path.map(|p| p.to_string_lossy().into_owned());
    let mut args: Vec<&str> = vec!["backup"];
    if let Some(config) = &config_arg {
        args.extend_from_slice(&["--config", config]);
    }
    args.extend_from_slice(&["--out", &out_arg]);
    checked(runner, "lu", &args, cmd_timeout)?;

    let checksum = backup_checksum(&path)?;

    let mut entries: Vec<_> = fs::read_dir(dir)
        .map_err(|source| Error::Read {
            path: dir.clone(),
            source,
        })?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file() && e.path() != path)
        .collect();
    entries.sort_by_key(|e| {
        (
            e.metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
            e.file_name(),
        )
    });
    let keep = cfg.backup_keep;
    if entries.len() >= keep {
        for old in &entries[..entries.len() - keep + 1] {
            journal(
                "backup_pruned",
                &[("path", &one_line(&old.path().to_string_lossy()))],
            );
            let _ = fs::remove_file(old.path());
        }
    }
    journal(
        "controller_backup",
        &[
            ("path", &one_line(&path.to_string_lossy())),
            ("checksum", &checksum),
            ("kept", &keep.to_string()),
        ],
    );
    Ok(ControllerBackup { path, checksum })
}

/// Apply to the controller. Returns `Some(reason)` when the controller was
/// deliberately left alone (policy, window, busy, schema marker).
pub fn apply_controller(
    component: &Component,
    version: &str,
    body: &str,
    ctx: &ApplyContext<'_>,
    state: &mut AutoState,
) -> Result<Option<String>> {
    apply_controller_with_runner(component, version, body, ctx, state, &SystemRunner)
}

pub fn apply_controller_with_runner(
    component: &Component,
    version: &str,
    body: &str,
    ctx: &ApplyContext<'_>,
    state: &mut AutoState,
    runner: &dyn ApplyRunner,
) -> Result<Option<String>> {
    let cmd_timeout = command_timeout(ctx)?;
    let health_timeout = parse_duration(&ctx.cfg.health_timeout)?;
    if let Some(reason) = controller_skip_reason(ctx.cfg, body, runner, cmd_timeout)? {
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
    let backup = take_backup(ctx.cfg, runner, cmd_timeout, ctx.config_path)?;
    state.controller_backup = Some(backup.clone());
    crate::auto_update::state::save(ctx.prefix, state)?;
    journal(
        "controller_backup_recorded",
        &[("path", &one_line(&backup.path.to_string_lossy()))],
    );
    let opts = install_options(ctx, version, false);
    let outcome = runner.install(component, &opts)?;

    // The idleness check before backup/install is not a lease. Re-check it
    // after install, immediately before restarting the new binary.
    let (idle, idle_error) = match checked(runner, "lu", &["status", "--json"], cmd_timeout) {
        Ok(out) if lu::controller_idle(&out.stdout) => (true, None),
        Ok(_) => (false, None),
        Err(e) => (false, Some(e.to_string())),
    };
    if !idle {
        let reason = match idle_error {
            Some(e) => format!(
                "cannot re-check controller idleness before restart: {}",
                one_line(&e)
            ),
            None => "controller is no longer idle after backup and install".to_string(),
        };
        journal(
            "controller_idle_recheck_failed",
            &[("reason", &one_line(&reason))],
        );
        return recover_controller(
            component,
            version,
            ctx,
            state,
            runner,
            &reason,
            health_timeout,
        )
        .map(|()| None);
    }
    journal("controller_idle_rechecked", &[]);

    let mut startup_error = None;
    if let Err(e) = restart(runner, &ctx.cfg.controller_unit, ctx) {
        startup_error = Some(e.to_string());
    }
    let (healthy, health_error) = if startup_error.is_some() {
        (false, startup_error)
    } else {
        poll_until(health_timeout, || controller_healthy(runner, version, ctx))
    };
    if healthy {
        install::record_installed(component, version, receipt(&outcome)?, ctx.prefix)?;
        journal(
            "controller_updated",
            &[
                ("component", &outcome.component),
                ("version", &outcome.version),
            ],
        );
        return Ok(None);
    }

    let health_reason = health_error.unwrap_or_else(|| {
        format!(
            "controller did not report version {version} within {}",
            ctx.cfg.health_timeout
        )
    });
    recover_controller(
        component,
        version,
        ctx,
        state,
        runner,
        &health_reason,
        health_timeout,
    )
    .map(|()| None)
}

fn recover_controller(
    component: &Component,
    version: &str,
    ctx: &ApplyContext<'_>,
    state: &mut AutoState,
    runner: &dyn ApplyRunner,
    failure_reason: &str,
    health_timeout: Duration,
) -> Result<()> {
    journal(
        "controller_health_failed",
        &[
            ("component", &component.name),
            ("version", version),
            ("reason", &one_line(failure_reason)),
        ],
    );
    mark_bad(state, &component.name, version, ctx.prefix);

    let mut recovery_errors = Vec::new();
    match &state.controller_backup {
        Some(backup) => {
            if let Err(e) = verify_backup_record(backup) {
                recovery_errors.push(format!(
                    "controller backup at {} is unusable: {}; keep this file for manual restore",
                    backup.path.display(),
                    one_line(&e.to_string())
                ));
            }
        }
        None => recovery_errors.push(
            "no controller backup checksum recorded; manual restore may be required".to_string(),
        ),
    }
    if let Err(e) = rollback::restore_binaries(component, ctx.prefix, false) {
        recovery_errors.push(format!("restore .prev failed: {e}"));
    }
    if let Err(e) = restart(runner, &ctx.cfg.controller_unit, ctx) {
        recovery_errors.push(format!("restart failed: {e}"));
    }
    if recovery_errors.is_empty() {
        match previous_version(component, ctx.prefix) {
            Some(old) => {
                let (ok, err) =
                    poll_until(health_timeout, || controller_healthy(runner, &old, ctx));
                if !ok {
                    recovery_errors.push(format!(
                        "restored controller did not report version {old}{}",
                        detail_suffix(err)
                    ));
                }
            }
            None => recovery_errors.push("no previous version is recorded for verification".into()),
        }
    }
    if !recovery_errors.is_empty() {
        return Err(Error::msg(format!(
            "controller health check failed within {}: {}; recovery failed: {}",
            ctx.cfg.health_timeout,
            failure_reason,
            one_line(&recovery_errors.join("; "))
        )));
    }
    Err(Error::msg(format!(
        "controller health check failed within {}: {}; restored .prev, verified restored health and marked {version} bad",
        ctx.cfg.health_timeout,
        failure_reason
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component;
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::{Arc, Mutex};
    use time::format_description::well_known::Rfc3339;

    fn timestamp(value: OffsetDateTime) -> String {
        value.format(&Rfc3339).unwrap()
    }

    fn recent() -> String {
        timestamp(OffsetDateTime::now_utc())
    }

    fn node_json(version: &str, last_seen: &str) -> String {
        format!(
            r#"[{{"name":"n1","version":"{version}","draining":true,"current_job":null,"online":true,"last_seen":"{last_seen}"}}]"#
        )
    }

    fn node_json_busy(version: &str, last_seen: &str) -> String {
        format!(
            r#"[{{"name":"n1","version":"{version}","draining":false,"current_job":{{"id":"j1"}},"online":true,"last_seen":"{last_seen}"}}]"#
        )
    }

    fn status_json(version: &str, leased: u32, running: u32) -> String {
        format!(
            r#"{{"version":"{version}","jobs":{{"leased":{leased},"running":{running},"recovering":0,"needs_input":0}}}}"#
        )
    }

    #[derive(Default)]
    struct Calls {
        commands: Vec<(String, Vec<String>)>,
        installs: usize,
        archive_checks: usize,
        health_checks: usize,
    }

    struct FakeRunner {
        calls: Arc<Mutex<Calls>>,
        fail: BTreeMap<String, String>,
        fail_once: Arc<Mutex<BTreeMap<String, String>>>,
        node: Arc<Mutex<(String, String)>>,
        nodes: Arc<Mutex<VecDeque<String>>>,
        status: Arc<Mutex<VecDeque<String>>>,
        enabled: Arc<Mutex<Vec<String>>>,
        present: Vec<String>,
        receipt: crate::install::InstallReceipt,
        archive_names: Option<Vec<String>>,
        install_version: String,
        health: Arc<Mutex<VecDeque<bool>>>,
        recovery_health: Option<bool>,
        reiki_restarts: Arc<Mutex<usize>>,
        staged_fail: Arc<Mutex<bool>>,
        last_staged: Arc<Mutex<Option<StagedCheck>>>,
    }

    impl FakeRunner {
        fn new() -> Self {
            Self {
                calls: Arc::new(Mutex::new(Calls::default())),
                fail: BTreeMap::new(),
                fail_once: Arc::new(Mutex::new(BTreeMap::new())),
                node: Arc::new(Mutex::new(("0.4.0".to_string(), recent()))),
                nodes: Arc::new(Mutex::new(VecDeque::new())),
                status: Arc::new(Mutex::new(VecDeque::from([status_json("0.4.0", 0, 0)]))),
                enabled: Arc::new(Mutex::new(vec![
                    "ting.service".to_string(),
                    "reiki.service".to_string(),
                ])),
                present: vec![
                    "ting.service".to_string(),
                    "reiki.service".to_string(),
                    "lu-controller.service".to_string(),
                ],
                receipt: crate::install::InstallReceipt {
                    checksum: "abc".to_string(),
                    target: "test".to_string(),
                    installed_at: "now".to_string(),
                },
                archive_names: None,
                install_version: "v0.4.1".to_string(),
                health: Arc::new(Mutex::new(VecDeque::from([true]))),
                recovery_health: None,
                reiki_restarts: Arc::new(Mutex::new(0)),
                staged_fail: Arc::new(Mutex::new(false)),
                last_staged: Arc::new(Mutex::new(None)),
            }
        }

        fn set_node(&self, version: &str, last_seen: &str) {
            *self.node.lock().unwrap() = (version.to_string(), last_seen.to_string());
        }

        fn set_enabled(&self, units: &[&str]) {
            *self.enabled.lock().unwrap() = units.iter().map(|s| s.to_string()).collect();
        }

        fn set_status(&self, body: &str) {
            let mut status = self.status.lock().unwrap();
            status.clear();
            status.push_back(body.to_string());
        }

        fn set_health(&self, healthy: bool) {
            let mut health = self.health.lock().unwrap();
            health.clear();
            health.push_back(healthy);
        }

        fn push_health(&self, healthy: bool) {
            self.health.lock().unwrap().push_back(healthy);
        }

        fn set_recovery_health(&mut self, healthy: bool) {
            self.recovery_health = Some(healthy);
        }

        fn push_status(&self, body: &str) {
            self.status.lock().unwrap().push_back(body.to_string());
        }

        fn push_nodes(&self, body: &str) {
            self.nodes.lock().unwrap().push_back(body.to_string());
        }

        fn next_nodes(&self) -> Option<String> {
            let mut nodes = self.nodes.lock().unwrap();
            if nodes.len() > 1 {
                nodes.pop_front()
            } else {
                nodes.front().cloned()
            }
        }

        fn next_status(&self) -> String {
            let mut status = self.status.lock().unwrap();
            if status.len() > 1 {
                status.pop_front().unwrap()
            } else {
                status.front().cloned().unwrap_or_default()
            }
        }
    }

    impl ApplyRunner for FakeRunner {
        fn command(
            &self,
            program: &str,
            args: &[&str],
            _timeout: Duration,
        ) -> Result<CommandOutput> {
            let key = format!("{program} {}", args.join(" "));
            self.calls.lock().unwrap().commands.push((
                program.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
            ));
            if let Some(reason) = self.fail.get(&key) {
                return Err(Error::msg(reason.clone()));
            }
            if let Some(reason) = self.fail_once.lock().unwrap().remove(&key) {
                return Err(Error::msg(reason));
            }
            if program == "systemctl" && args == ["restart", "reiki.service"] {
                let mut restarts = self.reiki_restarts.lock().unwrap();
                *restarts += 1;
                if *restarts == 2 {
                    self.push_health(self.recovery_health.unwrap_or(true));
                }
            }
            let body = match (program, args.first().copied(), args.get(1).copied()) {
                ("lu", Some("nodes"), Some("--json")) => match self.next_nodes() {
                    Some(body) => body,
                    None => {
                        let (version, seen) = self.node.lock().unwrap().clone();
                        node_json(&version, &seen)
                    }
                },
                ("lu", Some("status"), Some("--json")) => self.next_status(),
                ("lu", Some("backup"), _) => {
                    if let Some(pos) = args.iter().position(|a| *a == "--out") {
                        if let Some(path) = args.get(pos + 1) {
                            let _ = fs::write(path, b"backup payload");
                        }
                    }
                    String::new()
                }
                ("systemctl", Some("cat"), Some(unit)) => {
                    if self.present.iter().any(|u| u == unit) {
                        String::new()
                    } else {
                        return Err(Error::msg(format!("unit {unit} not found")));
                    }
                }
                ("systemctl", Some("is-enabled"), Some("--quiet")) => {
                    let unit = args.get(2).copied().unwrap_or("");
                    if self.enabled.lock().unwrap().iter().any(|u| u == unit) {
                        String::new()
                    } else {
                        return Err(Error::msg(format!("unit {unit} is not enabled")));
                    }
                }
                ("reiki", Some("check"), _) => String::new(),
                _ => String::new(),
            };
            Ok(CommandOutput {
                status: 0,
                stdout: body,
                stderr: String::new(),
            })
        }

        fn install(&self, c: &Component, opts: &InstallOptions) -> Result<InstallOutcome> {
            let mut calls = self.calls.lock().unwrap();
            calls.installs += 1;
            if let Some(check) = &opts.staged_check {
                *self.last_staged.lock().unwrap() = Some(check.clone());
                if *self.staged_fail.lock().unwrap() {
                    return Err(Error::StagedCheckFailed {
                        binary: check.binary.clone(),
                        detail: "staged config check failed".to_string(),
                    });
                }
            }
            Ok(InstallOutcome {
                component: c.name.clone(),
                version: self.install_version.clone(),
                binaries: c.binaries.clone(),
                dry_run: false,
                receipt: Some(self.receipt.clone()),
            })
        }

        fn archive_binaries(&self, component: &Component, version: &str) -> Result<Vec<String>> {
            self.calls.lock().unwrap().archive_checks += 1;
            Ok(self
                .archive_names
                .clone()
                .unwrap_or_else(|| component.expected_binaries(version)))
        }

        fn health_endpoint(
            &self,
            _url: &str,
            _timeout: Duration,
        ) -> std::result::Result<bool, String> {
            self.calls.lock().unwrap().health_checks += 1;
            let mut health = self.health.lock().unwrap();
            Ok(if health.len() > 1 {
                health.pop_front().unwrap()
            } else {
                *health.front().unwrap_or(&false)
            })
        }
    }

    fn cfg(backup_dir: &Path) -> AutoUpdateConfig {
        AutoUpdateConfig {
            enabled: true,
            node_name: Some("n1".to_string()),
            drain_timeout: "1s".to_string(),
            controller_wait_timeout: "1s".to_string(),
            health_timeout: "1s".to_string(),
            command_timeout: "1s".to_string(),
            controller_policy: ControllerPolicy::WhenIdle,
            backup_dir: Some(backup_dir.to_path_buf()),
            reiki_config_path: Some(PathBuf::from("/var/lib/reiki/reiki.toml")),
            heartbeat_max_age: "1h".to_string(),
            ..AutoUpdateConfig::default()
        }
    }

    fn ctx<'a>(cfg: &'a AutoUpdateConfig, prefix: &'a Path) -> ApplyContext<'a> {
        ApplyContext {
            cfg,
            prefix: Some(prefix),
            config_path: Some(Path::new("tuishou.toml")),
            verifier: None,
            verify_config: None,
        }
    }

    fn lu_component() -> Component {
        component::builtin_components()
            .into_iter()
            .find(|c| c.name == "lu")
            .unwrap()
    }

    fn reiki_component() -> Component {
        component::builtin_components()
            .into_iter()
            .find(|c| c.name == "reiki")
            .unwrap()
    }

    fn seed_state(prefix: &Path, component: &str, version: &str) {
        let mut existing = state::State::default();
        existing.components.insert(
            component.to_string(),
            state::InstalledComponent {
                version: version.to_string(),
                target: "test".to_string(),
                checksum: "abc".to_string(),
                installed_at: "before".to_string(),
                previous_version: None,
            },
        );
        let state_path = state::default_state_path(Some(prefix));
        state::save(&state_path, &existing).unwrap();
        let mut good = state::KnownGood::default();
        if let Some(c) = component::builtin_components()
            .into_iter()
            .find(|c| c.name == component)
        {
            for name in &c.binaries {
                let prev = prefix.join(format!("{name}.prev"));
                if prev.exists() {
                    good.checksums
                        .insert(name.clone(), crate::verify::sha256_file(&prev).unwrap());
                }
            }
        }
        state::save_known_good(&state_path, &good).unwrap();
    }

    fn seed_controller(prefix: &Path) {
        fs::write(prefix.join("lu"), b"old").unwrap();
        fs::write(prefix.join("lu.prev"), b"old prev").unwrap();
        seed_state(prefix, "lu", "v0.4.0");
    }

    #[derive(Clone, Copy)]
    enum BackupFault {
        LooseMode,
        Empty,
        TamperAfterInstall,
        RemoveAfterInstall,
    }

    struct FaultyBackup(FakeRunner, BackupFault, Mutex<Option<PathBuf>>);

    impl FaultyBackup {
        fn new(inner: FakeRunner, fault: BackupFault) -> Self {
            Self(inner, fault, Mutex::new(None))
        }
    }

    impl ApplyRunner for FaultyBackup {
        fn command(&self, p: &str, a: &[&str], t: Duration) -> Result<CommandOutput> {
            let out = self.0.command(p, a, t)?;
            if a.first().copied() == Some("backup") {
                if let Some(pos) = a.iter().position(|arg| *arg == "--out") {
                    if let Some(arg) = a.get(pos + 1) {
                        let path = PathBuf::from(arg);
                        match self.1 {
                            BackupFault::LooseMode => {
                                #[cfg(unix)]
                                {
                                    use std::os::unix::fs::PermissionsExt;
                                    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
                                        .unwrap();
                                }
                            }
                            BackupFault::Empty => fs::write(&path, b"").unwrap(),
                            BackupFault::TamperAfterInstall | BackupFault::RemoveAfterInstall => {}
                        }
                        *self.2.lock().unwrap() = Some(path);
                    }
                }
            }
            Ok(out)
        }

        fn install(&self, c: &Component, o: &InstallOptions) -> Result<InstallOutcome> {
            let out = self.0.install(c, o)?;
            let path = self.2.lock().unwrap().clone().expect("backup path");
            match self.1 {
                BackupFault::TamperAfterInstall => fs::write(path, b"tampered").unwrap(),
                BackupFault::RemoveAfterInstall => fs::remove_file(path).unwrap(),
                BackupFault::LooseMode | BackupFault::Empty => {}
            }
            Ok(out)
        }

        fn archive_binaries(&self, c: &Component, v: &str) -> Result<Vec<String>> {
            self.0.archive_binaries(c, v)
        }

        fn health_endpoint(&self, u: &str, t: Duration) -> std::result::Result<bool, String> {
            self.0.health_endpoint(u, t)
        }
    }

    #[test]
    fn a_host_without_the_agent_unit_is_not_a_lu_agent_node() {
        let td = tempfile::tempdir().unwrap();
        let config = cfg(&td.path().join("backups"));
        let context = ctx(&config, td.path());
        let runner = FakeRunner::new();

        // An operator workstation can reach the controller (lu status prints a document) but has
        // no agent unit: it must never be treated as a node to drain and restart.
        runner.set_enabled(&[]);
        assert!(!node_runs_lu(&runner, "workstation", &context));
        runner.set_enabled(&["lu-controller.service"]);
        assert!(!node_runs_lu(&runner, "workstation", &context));
        runner.set_enabled(&["ting.service"]);
        assert!(node_runs_lu(&runner, "dev-ct-9", &context));
    }

    #[test]
    fn role_detection_requires_exactly_one_installed_and_enabled_unit() {
        let td = tempfile::tempdir().unwrap();
        let config = cfg(&td.path().join("backups"));
        let context = ctx(&config, td.path());
        let runner = FakeRunner::new();

        assert_eq!(
            detect_lu_role_with_runner(&context, &runner).unwrap(),
            LuRole::Agent
        );
        runner.set_enabled(&["lu-controller.service"]);
        assert_eq!(
            detect_lu_role_with_runner(&context, &runner).unwrap(),
            LuRole::Controller
        );
        runner.set_enabled(&[]);
        assert!(detect_lu_role_with_runner(&context, &runner)
            .unwrap_err()
            .to_string()
            .contains("neither"));
        runner.set_enabled(&["ting.service", "lu-controller.service"]);
        assert!(detect_lu_role_with_runner(&context, &runner)
            .unwrap_err()
            .to_string()
            .contains("both"));

        let mut missing = FakeRunner::new();
        missing.fail.insert(
            "systemctl cat ting.service".to_string(),
            "missing".to_string(),
        );
        missing.set_enabled(&["ting.service", "lu-controller.service"]);
        assert_eq!(
            detect_lu_role_with_runner(&context, &missing).unwrap(),
            LuRole::Controller,
            "a missing unit file must not satisfy role detection"
        );
    }

    #[test]
    fn agent_uses_real_commands_and_waits_for_exact_version() {
        let td = tempfile::tempdir().unwrap();
        let config = cfg(&td.path().join("backups"));
        let context = ctx(&config, td.path());
        let runner = FakeRunner::new();
        runner.set_node("0.4.1", &recent());
        seed_state(td.path(), "lu", "v0.4.0");
        let calls = runner.calls.clone();

        apply_agent_with_runner(
            &lu_component(),
            "v0.4.1",
            &context,
            &mut AutoState::default(),
            &runner,
        )
        .unwrap();

        let cmds = calls.lock().unwrap().commands.clone();
        assert!(cmds.iter().any(|(p, a)| p == "lu" && a == &["drain", "n1"]));
        assert!(cmds
            .iter()
            .any(|(p, a)| p == "lu" && a == &["nodes", "--json"]));
        assert!(cmds
            .iter()
            .any(|(p, a)| p == "systemctl" && a == &["restart", "ting.service"]));
        assert!(cmds
            .iter()
            .any(|(p, a)| p == "lu" && a == &["undrain", "n1"]));
        assert!(!cmds
            .iter()
            .any(|(p, a)| p == "lu" && a.iter().any(|x| x == "node")));
        assert_eq!(calls.lock().unwrap().installs, 1);
    }

    #[test]
    fn agent_skips_drain_when_controller_is_unreachable() {
        let td = tempfile::tempdir().unwrap();
        let config = cfg(&td.path().join("backups"));
        let context = ctx(&config, td.path());
        let runner = FakeRunner::new();
        runner.set_node("0.4.1", &recent());
        runner.set_status("not json");
        let calls = runner.calls.clone();

        apply_agent_with_runner(
            &lu_component(),
            "v0.4.1",
            &context,
            &mut AutoState::default(),
            &runner,
        )
        .unwrap();

        let cmds = calls.lock().unwrap().commands.clone();
        assert!(!cmds
            .iter()
            .any(|(p, a)| p == "lu" && a.first().map(String::as_str) == Some("drain")));
        assert!(!cmds
            .iter()
            .any(|(p, a)| p == "lu" && a.first().map(String::as_str) == Some("undrain")));
        assert!(cmds
            .iter()
            .any(|(p, a)| p == "systemctl" && a == &["restart", "ting.service"]));
    }

    #[test]
    fn missing_required_binaries_skip_before_drain() {
        let td = tempfile::tempdir().unwrap();
        let config = cfg(&td.path().join("backups"));
        let context = ctx(&config, td.path());
        let mut runner = FakeRunner::new();
        runner.archive_names = Some(vec!["reiki".to_string()]);

        let reason = apply_agent_with_runner(
            &lu_component(),
            "v0.4.1",
            &context,
            &mut AutoState::default(),
            &runner,
        )
        .unwrap()
        .expect("missing binaries must be skipped");
        assert!(reason.contains("lu, ting"), "{reason}");
        assert_eq!(runner.calls.lock().unwrap().installs, 0);
        assert!(!runner
            .calls
            .lock()
            .unwrap()
            .commands
            .iter()
            .any(|(p, a)| p == "lu" && a.first().map(String::as_str) == Some("drain")));
    }

    #[test]
    fn stale_heartbeat_fails_health_and_rolls_back() {
        let td = tempfile::tempdir().unwrap();
        let config = cfg(&td.path().join("backups"));
        let context = ctx(&config, td.path());
        let runner = FakeRunner::new();
        runner.set_node(
            "0.4.1",
            &timestamp(OffsetDateTime::now_utc() - time::Duration::hours(2)),
        );
        seed_state(td.path(), "lu", "v0.4.0");
        let calls = runner.calls.clone();
        let mut st = AutoState::default();

        let err = apply_agent_with_runner(&lu_component(), "v0.4.1", &context, &mut st, &runner)
            .unwrap_err();
        assert!(
            err.to_string().contains("marked v0.4.1 bad"),
            "unexpected error: {err}"
        );
        assert!(st.bad_versions["lu"].contains(&"v0.4.1".to_string()));
        assert!(crate::auto_update::state::load(Some(td.path()))
            .unwrap()
            .bad_versions["lu"]
            .contains(&"v0.4.1".to_string()));
        let cmds = calls.lock().unwrap().commands.clone();
        assert_eq!(
            cmds.iter()
                .filter(|(p, a)| p == "systemctl" && a == &["restart", "ting.service"])
                .count(),
            2,
            "new and restored binaries must each be started"
        );
        assert!(cmds
            .iter()
            .any(|(p, a)| p == "lu" && a == &["undrain", "n1"]));
    }

    #[test]
    fn recovery_failure_still_undrains_and_persists_bad_mark() {
        let td = tempfile::tempdir().unwrap();
        let config = cfg(&td.path().join("backups"));
        let context = ctx(&config, td.path());
        let mut runner = FakeRunner::new();
        runner.fail.insert(
            "systemctl is-active --quiet ting.service".to_string(),
            "agent unhealthy".to_string(),
        );
        seed_state(td.path(), "lu", "v0.4.0");
        let calls = runner.calls.clone();
        let mut st = AutoState::default();

        let err = apply_agent_with_runner(&lu_component(), "v0.4.1", &context, &mut st, &runner)
            .unwrap_err();
        assert!(err.to_string().contains("agent unhealthy"));
        assert!(err.to_string().contains("recovery failed"));
        assert!(st.bad_versions["lu"].contains(&"v0.4.1".to_string()));
        let cmds = calls.lock().unwrap().commands.clone();
        assert!(cmds
            .iter()
            .any(|(p, a)| p == "lu" && a == &["undrain", "n1"]));
    }

    #[test]
    fn failed_undrain_is_propagated_after_successful_health() {
        let td = tempfile::tempdir().unwrap();
        let config = cfg(&td.path().join("backups"));
        let context = ctx(&config, td.path());
        let mut runner = FakeRunner::new();
        runner.set_node("0.4.1", &recent());
        runner
            .fail
            .insert("lu undrain n1".to_string(), "permission denied".to_string());

        let err = apply_agent_with_runner(
            &lu_component(),
            "v0.4.1",
            &context,
            &mut AutoState::default(),
            &runner,
        )
        .unwrap_err();
        assert!(err.to_string().contains("permission denied"));
    }

    #[test]
    fn agent_precommit_drain_recheck_rolls_back_and_skips_state_commit() {
        let td = tempfile::tempdir().unwrap();
        let prefix = td.path();
        seed_controller(prefix);
        let config = cfg(&prefix.join("backups"));
        let context = ctx(&config, prefix);
        let runner = FakeRunner::new();
        let seen = recent();
        runner.push_nodes(&node_json("0.4.1", &seen));
        runner.push_nodes(&node_json("0.4.1", &seen));
        runner.push_nodes(&node_json_busy("0.4.1", &seen));
        runner.push_nodes(&node_json("0.4.0", &seen));
        let calls = runner.calls.clone();
        let mut st = AutoState::default();

        let err = apply_agent_with_runner(&lu_component(), "v0.4.1", &context, &mut st, &runner)
            .unwrap_err();
        assert!(
            err.to_string().contains("no longer reports drained"),
            "unexpected error: {err}"
        );
        let recorded = state::load(&state::default_state_path(Some(prefix))).unwrap();
        assert_eq!(recorded.components["lu"].version, "v0.4.0");
        let cmds = calls.lock().unwrap().commands.clone();
        assert_eq!(
            cmds.iter()
                .filter(|(p, a)| p == "systemctl" && a == &["restart", "ting.service"])
                .count(),
            2,
            "new and restored binaries must each be started"
        );
        assert!(cmds
            .iter()
            .any(|(p, a)| p == "lu" && a == &["undrain", "n1"]));
    }

    #[test]
    fn agent_state_commit_failure_recovers_and_undrains() {
        let td = tempfile::tempdir().unwrap();
        let prefix = td.path();
        let state_path = state::default_state_path(Some(prefix));
        fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        fs::write(&state_path, "not json").unwrap();
        let config = cfg(&prefix.join("backups"));
        let context = ctx(&config, prefix);
        let runner = FakeRunner::new();
        runner.set_node("0.4.1", &recent());
        let calls = runner.calls.clone();
        let mut st = AutoState::default();

        let err = apply_agent_with_runner(&lu_component(), "v0.4.1", &context, &mut st, &runner)
            .unwrap_err();
        assert!(
            err.to_string().contains("state commit failed"),
            "unexpected error: {err}"
        );
        assert!(st.bad_versions["lu"].contains(&"v0.4.1".to_string()));
        let cmds = calls.lock().unwrap().commands.clone();
        assert_eq!(
            cmds.iter()
                .filter(|(p, a)| p == "systemctl" && a == &["restart", "ting.service"])
                .count(),
            2
        );
        assert!(cmds
            .iter()
            .any(|(p, a)| p == "lu" && a == &["undrain", "n1"]));
    }

    #[test]
    fn controller_backs_up_waits_for_idle_and_restarts_detected_unit() {
        let td = tempfile::tempdir().unwrap();
        let config = cfg(&td.path().join("backups"));
        let context = ctx(&config, td.path());
        let runner = FakeRunner::new();
        runner.set_status(&status_json("0.4.0", 1, 0));
        runner.push_status(&status_json("0.4.1", 0, 0));
        let calls = runner.calls.clone();

        let skipped = apply_controller_with_runner(
            &lu_component(),
            "v0.4.1",
            "release notes",
            &context,
            &mut AutoState::default(),
            &runner,
        )
        .unwrap();
        assert!(skipped.is_none());
        let cmds = calls.lock().unwrap().commands.clone();
        let backup = cmds
            .iter()
            .find(|(p, a)| p == "lu" && a.first().map(String::as_str) == Some("backup"))
            .expect("backup must be called");
        assert!(backup.1.contains(&"--config".to_string()));
        assert!(backup.1.contains(&"tuishou.toml".to_string()));
        let out = backup.1.iter().position(|a| a == "--out").unwrap();
        let out_path = Path::new(&backup.1[out + 1]);
        assert!(out_path.starts_with(td.path().join("backups")));
        assert!(out_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("lu-backup-"));
        assert!(fs::metadata(out_path).unwrap().len() > 0);
        let wait = cmds
            .iter()
            .position(|(p, a)| p == "lu" && a == &["status", "--json"])
            .unwrap();
        let restart = cmds
            .iter()
            .position(|(p, a)| p == "systemctl" && a == &["restart", "lu-controller.service"])
            .expect("controller unit must be restarted");
        assert!(wait < restart);
        assert!(!cmds
            .iter()
            .any(|(p, a)| p == "lu" && a.first().map(String::as_str) == Some("drain")));
    }

    #[test]
    fn failed_backup_refuses_controller_update() {
        struct BackupFails(FakeRunner);
        impl ApplyRunner for BackupFails {
            fn command(&self, p: &str, a: &[&str], t: Duration) -> Result<CommandOutput> {
                if a.first().copied() == Some("backup") {
                    return Err(Error::msg("backup failed"));
                }
                self.0.command(p, a, t)
            }
            fn install(&self, _c: &Component, _o: &InstallOptions) -> Result<InstallOutcome> {
                Err(Error::msg("install must not run after a failed backup"))
            }
            fn archive_binaries(&self, c: &Component, v: &str) -> Result<Vec<String>> {
                self.0.archive_binaries(c, v)
            }
            fn health_endpoint(&self, u: &str, t: Duration) -> std::result::Result<bool, String> {
                self.0.health_endpoint(u, t)
            }
        }

        let td = tempfile::tempdir().unwrap();
        let config = cfg(&td.path().join("backups"));
        let context = ctx(&config, td.path());
        let inner = FakeRunner::new();
        let installs = inner.calls.clone();
        let runner = BackupFails(inner);

        let err = apply_controller_with_runner(
            &lu_component(),
            "v0.4.1",
            "release notes",
            &context,
            &mut AutoState::default(),
            &runner,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("backup failed"),
            "unexpected: {err}"
        );
        assert_eq!(installs.lock().unwrap().installs, 0);
    }

    #[test]
    fn controller_waits_for_busy_jobs_then_skips_on_timeout() {
        let td = tempfile::tempdir().unwrap();
        let config = cfg(&td.path().join("backups"));
        let context = ctx(&config, td.path());
        let runner = FakeRunner::new();
        runner.set_status(&status_json("0.4.0", 1, 1));

        let reason = apply_controller_with_runner(
            &lu_component(),
            "v0.4.1",
            "release notes",
            &context,
            &mut AutoState::default(),
            &runner,
        )
        .unwrap()
        .expect("busy controller must be skipped");
        assert!(reason.contains("not idle"), "{reason}");
        assert_eq!(runner.calls.lock().unwrap().installs, 0);
        assert!(!runner
            .calls
            .lock()
            .unwrap()
            .commands
            .iter()
            .any(|(p, a)| p == "lu" && a.first().map(String::as_str) == Some("backup")));
    }

    #[test]
    fn controller_idle_recheck_aborts_before_new_binary_restart() {
        let td = tempfile::tempdir().unwrap();
        let prefix = td.path();
        seed_controller(prefix);
        let config = cfg(&prefix.join("backups"));
        let context = ctx(&config, prefix);
        let runner = FakeRunner::new();
        runner.set_status(&status_json("0.4.0", 0, 0));
        runner.push_status(&status_json("0.4.0", 1, 0));
        let calls = runner.calls.clone();

        let err = apply_controller_with_runner(
            &lu_component(),
            "v0.4.1",
            "release notes",
            &context,
            &mut AutoState::default(),
            &runner,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("no longer idle"),
            "unexpected error: {err}"
        );
        let recorded = state::load(&state::default_state_path(Some(prefix))).unwrap();
        assert_eq!(recorded.components["lu"].version, "v0.4.0");
        let cmds = calls.lock().unwrap().commands.clone();
        let status_calls: Vec<_> = cmds
            .iter()
            .enumerate()
            .filter(|(_, (p, a))| p == "lu" && a == &["status", "--json"])
            .collect();
        let recheck = status_calls[1].0;
        let restarts: Vec<_> = cmds
            .iter()
            .enumerate()
            .filter(|(_, (p, a))| p == "systemctl" && a == &["restart", "lu-controller.service"])
            .collect();
        assert_eq!(
            restarts.len(),
            1,
            "only the recovery restart of the old binary may run"
        );
        assert!(restarts[0].0 > recheck);
    }

    #[test]
    fn controller_recovery_fails_when_backup_changes_after_install() {
        let td = tempfile::tempdir().unwrap();
        let prefix = td.path();
        let backup_dir = prefix.join("backups");
        seed_controller(prefix);
        let config = cfg(&backup_dir);
        let context = ctx(&config, prefix);
        let inner = FakeRunner::new();
        inner.fail_once.lock().unwrap().insert(
            "systemctl is-active --quiet lu-controller.service".to_string(),
            "controller down".to_string(),
        );
        let mut st = AutoState::default();
        let runner = FaultyBackup::new(inner, BackupFault::TamperAfterInstall);

        let err = apply_controller_with_runner(
            &lu_component(),
            "v0.4.1",
            "release notes",
            &context,
            &mut st,
            &runner,
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("no longer matches the recorded checksum"),
            "unexpected error: {err}"
        );
        assert!(!err
            .to_string()
            .contains("restored .prev, verified restored health"));
        let backup = st
            .controller_backup
            .clone()
            .expect("checksum must be recorded");
        assert!(backup.path.starts_with(&backup_dir));
        assert_ne!(
            crate::verify::sha256_file(&backup.path).unwrap(),
            backup.checksum
        );
        assert!(backup.path.exists(), "keep the backup for manual restore");
        assert!(st.bad_versions["lu"].contains(&"v0.4.1".to_string()));
    }

    #[test]
    fn controller_recovery_fails_when_backup_is_missing_after_install() {
        let td = tempfile::tempdir().unwrap();
        let prefix = td.path();
        let backup_dir = prefix.join("backups");
        seed_controller(prefix);
        let config = cfg(&backup_dir);
        let context = ctx(&config, prefix);
        let inner = FakeRunner::new();
        inner.fail_once.lock().unwrap().insert(
            "systemctl is-active --quiet lu-controller.service".to_string(),
            "controller down".to_string(),
        );
        let mut st = AutoState::default();
        let runner = FaultyBackup::new(inner, BackupFault::RemoveAfterInstall);

        let err = apply_controller_with_runner(
            &lu_component(),
            "v0.4.1",
            "release notes",
            &context,
            &mut st,
            &runner,
        )
        .unwrap_err();
        let backup = st
            .controller_backup
            .clone()
            .expect("checksum must be recorded");
        assert!(
            err.to_string().contains(&format!(
                "controller backup at {} is unusable",
                backup.path.display()
            )),
            "unexpected error: {err}"
        );
        assert!(err.to_string().contains("manual restore"));
        assert!(st.bad_versions["lu"].contains(&"v0.4.1".to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn controller_backup_must_be_regular_nonempty_and_restricted() {
        let td = tempfile::tempdir().unwrap();
        let prefix = td.path();
        seed_controller(prefix);
        let config = cfg(&prefix.join("backups"));
        let context = ctx(&config, prefix);
        let inner = FakeRunner::new();
        let runner = FaultyBackup::new(inner, BackupFault::LooseMode);
        let err = apply_controller_with_runner(
            &lu_component(),
            "v0.4.1",
            "release notes",
            &context,
            &mut AutoState::default(),
            &runner,
        )
        .unwrap_err();
        assert!(err.to_string().contains("0600"), "unexpected: {err}");
        assert_eq!(runner.0.calls.lock().unwrap().installs, 0);

        let inner = FakeRunner::new();
        let runner = FaultyBackup::new(inner, BackupFault::Empty);
        let err = apply_controller_with_runner(
            &lu_component(),
            "v0.4.1",
            "release notes",
            &context,
            &mut AutoState::default(),
            &runner,
        )
        .unwrap_err();
        assert!(err.to_string().contains("empty"), "unexpected: {err}");
        assert_eq!(runner.0.calls.lock().unwrap().installs, 0);

        let not_a_file = prefix.join("backups").join("directory.sql");
        fs::create_dir(&not_a_file).unwrap();
        assert!(backup_checksum(&not_a_file)
            .unwrap_err()
            .to_string()
            .contains("not a regular file"));
    }

    #[test]
    fn a_host_without_reiki_gets_a_plain_binary_swap_and_no_bad_mark() {
        let td = tempfile::tempdir().unwrap();
        let config = cfg(&td.path().join("backups"));
        let context = ctx(&config, td.path());
        let runner = FakeRunner::new();
        runner.set_enabled(&[]);
        let calls = runner.calls.clone();
        let mut state = AutoState::default();

        let outcome =
            apply_reiki_with_runner(&reiki_component(), "v0.4.1", &context, &mut state, &runner)
                .unwrap();

        assert!(outcome.is_none());
        assert!(
            runner.last_staged.lock().unwrap().is_none(),
            "no config check"
        );
        let cmds = calls.lock().unwrap().commands.clone();
        assert!(!cmds
            .iter()
            .any(|(p, a)| p == "lu" && a.first().map(String::as_str) == Some("drain")));
        assert!(!cmds
            .iter()
            .any(|(p, a)| p == "systemctl" && a.first().map(String::as_str) == Some("restart")));
        assert!(state.bad_versions.is_empty());
    }

    #[test]
    fn reiki_checks_staged_config_before_restart_then_health_and_undrain() {
        let td = tempfile::tempdir().unwrap();
        let config = cfg(&td.path().join("backups"));
        let context = ctx(&config, td.path());
        let runner = FakeRunner::new();
        let calls = runner.calls.clone();

        apply_reiki_with_runner(
            &reiki_component(),
            "v0.4.1",
            &context,
            &mut AutoState::default(),
            &runner,
        )
        .unwrap();

        let staged = runner.last_staged.lock().unwrap().clone().unwrap();
        assert_eq!(staged.binary, "reiki");
        assert_eq!(
            staged.args,
            vec!["check", "--config", "/var/lib/reiki/reiki.toml"]
        );
        let cmds = calls.lock().unwrap().commands.clone();
        let drain = cmds
            .iter()
            .position(|(p, a)| p == "lu" && a == &["drain", "n1"])
            .unwrap();
        let restart = cmds
            .iter()
            .position(|(p, a)| p == "systemctl" && a == &["restart", "reiki.service"])
            .unwrap();
        let check = cmds
            .iter()
            .position(|(p, a)| p == "reiki" && a.first().map(String::as_str) == Some("check"))
            .unwrap();
        let health = calls.lock().unwrap().health_checks;
        let undrain = cmds
            .iter()
            .position(|(p, a)| p == "lu" && a == &["undrain", "n1"])
            .unwrap();
        assert!(drain < restart && restart < check && check < undrain);
        assert_eq!(health, 1, "admin health endpoint must be checked");
    }

    #[test]
    fn staged_config_failure_keeps_old_binary_and_marks_version_bad() {
        let td = tempfile::tempdir().unwrap();
        let prefix = td.path();
        fs::write(prefix.join("reiki"), b"old").unwrap();
        fs::write(prefix.join("reiki.prev"), b"old prev").unwrap();
        seed_state(prefix, "reiki", "v0.4.0");
        let config = cfg(&td.path().join("backups"));
        let context = ctx(&config, prefix);
        let runner = FakeRunner::new();
        *runner.staged_fail.lock().unwrap() = true;
        let calls = runner.calls.clone();
        let mut st = AutoState::default();

        let err = apply_reiki_with_runner(&reiki_component(), "v0.4.1", &context, &mut st, &runner)
            .unwrap_err();
        assert!(
            err.to_string().contains("marked v0.4.1 bad"),
            "unexpected error: {err}"
        );
        assert_eq!(fs::read(prefix.join("reiki")).unwrap(), b"old");
        assert!(st.bad_versions["reiki"].contains(&"v0.4.1".to_string()));
        assert!(!calls
            .lock()
            .unwrap()
            .commands
            .iter()
            .any(|(p, a)| p == "systemctl" && a.first().map(String::as_str) == Some("restart")));
        assert!(calls
            .lock()
            .unwrap()
            .commands
            .iter()
            .any(|(p, a)| p == "lu" && a == &["undrain", "n1"]));
    }

    #[test]
    fn failed_reiki_health_rolls_back_restarts_and_marks_version_bad() {
        let td = tempfile::tempdir().unwrap();
        let prefix = td.path();
        fs::write(prefix.join("reiki"), b"old").unwrap();
        fs::write(prefix.join("reiki.prev"), b"old prev").unwrap();
        seed_state(prefix, "reiki", "v0.4.0");
        let config = cfg(&td.path().join("backups"));
        let context = ctx(&config, prefix);
        let mut runner = FakeRunner::new();
        runner.set_health(false);
        runner.set_recovery_health(false);
        let calls = runner.calls.clone();
        let mut st = AutoState::default();

        let err = apply_reiki_with_runner(&reiki_component(), "v0.4.1", &context, &mut st, &runner)
            .unwrap_err();
        assert!(
            err.to_string().contains("recovery failed"),
            "unexpected error: {err}"
        );
        assert_eq!(
            fs::read(prefix.join("reiki")).unwrap(),
            b"old prev",
            "error: {err}"
        );
        assert!(st.bad_versions["reiki"].contains(&"v0.4.1".to_string()));
        assert_eq!(
            calls
                .lock()
                .unwrap()
                .commands
                .iter()
                .filter(|(p, a)| p == "systemctl" && a == &["restart", "reiki.service"])
                .count(),
            2
        );
        assert!(err.to_string().contains("left drained"), "{err}");
        assert!(!calls
            .lock()
            .unwrap()
            .commands
            .iter()
            .any(|(p, a)| p == "lu" && a == &["undrain", "n1"]));
    }

    #[test]
    fn reiki_recovery_failure_leaves_node_drained() {
        let td = tempfile::tempdir().unwrap();
        let prefix = td.path();
        fs::write(prefix.join("reiki"), b"old").unwrap();
        fs::write(prefix.join("reiki.prev"), b"old prev").unwrap();
        seed_state(prefix, "reiki", "v0.4.0");
        let config = cfg(&prefix.join("backups"));
        let context = ctx(&config, prefix);
        let mut runner = FakeRunner::new();
        runner.fail.insert(
            "systemctl restart reiki.service".to_string(),
            "restart unavailable".to_string(),
        );
        let calls = runner.calls.clone();
        let mut st = AutoState::default();

        let err = apply_reiki_with_runner(&reiki_component(), "v0.4.1", &context, &mut st, &runner)
            .unwrap_err();
        assert!(err.to_string().contains("left drained"), "{err}");
        assert!(st.bad_versions["reiki"].contains(&"v0.4.1".to_string()));
        assert!(!calls
            .lock()
            .unwrap()
            .commands
            .iter()
            .any(|(p, a)| p == "lu" && a == &["undrain", "n1"]));
    }
}
