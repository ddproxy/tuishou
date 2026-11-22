//! Applying an eligible release. Agents use the real Lu drain path; the
//! controller is guarded by policy, idleness, backups and schema checks.

use crate::auto_update::state::AutoState;
use crate::component::Component;
use crate::config::{AutoUpdateConfig, ControllerPolicy};
use crate::duration::{in_window, parse_duration, parse_hhmm};
use crate::error::{Error, Result};
use crate::install::{self, InstallOptions, InstallOutcome};
use crate::logging::{journal, one_line};
use crate::process::{self, CommandOutput};
use crate::release::SCHEMA_MARKER;
use crate::rollback;
use crate::{lu, state};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::thread;
use std::time::{Duration, Instant};
use time::OffsetDateTime;

pub struct ApplyContext<'a> {
    pub cfg: &'a AutoUpdateConfig,
    pub prefix: Option<&'a Path>,
    /// Config path passed as `lu backup --config` so the controller backs up
    /// the same configuration the CLI loaded.
    pub config_path: Option<&'a Path>,
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
        commit_state,
        signer_workflow: ctx.cfg.attestation_signer.clone(),
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
    Ok(lu::node_reports_version(&out.stdout, node, version))
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
/// undrain. Any health failure restores `.prev`, restarts, verifies restored
/// health, marks the version bad and leaves the node drained if recovery fails.
/// Returns `Some(reason)` when the release archive does not contain an agent
/// binary for this component. Such a release is skipped, not failed: draining
/// and restarting a node would be pointless and disruptive.
fn agent_binary_skip_reason(
    component: &Component,
    version: &str,
    runner: &dyn ApplyRunner,
) -> Result<Option<String>> {
    let expected = component.expected_binaries(version);
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

pub fn apply_agent_with_runner(
    component: &Component,
    version: &str,
    ctx: &ApplyContext<'_>,
    state: &mut AutoState,
    runner: &dyn ApplyRunner,
) -> Result<Option<String>> {
    if let Some(reason) = agent_binary_skip_reason(component, version, runner)? {
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
    let unit = &ctx.cfg.agent_unit;
    let cmd_timeout = command_timeout(ctx)?;
    let drain_timeout = parse_duration(&ctx.cfg.drain_timeout)?;
    journal(
        "agent_update_start",
        &[
            ("component", &component.name),
            ("version", version),
            ("node", &node),
            ("unit", unit),
        ],
    );

    if let Err(e) = checked(runner, "lu", &["drain", &node], cmd_timeout) {
        // The command may have partially succeeded; never leave the node
        // drained because of an ambiguous drain failure.
        let _ = undrain(runner, &node, ctx);
        return Err(Error::msg(format!(
            "failed to drain node {node}: {}",
            one_line(&e.to_string())
        )));
    }
    journal("agent_drain", &[("node", &node)]);

    let (drained, drain_error) = poll_until(drain_timeout, || {
        match checked(runner, "lu", &["nodes", "--json"], cmd_timeout) {
            Ok(out) => {
                if lu::nodes_report_drained(&out.stdout, &node) {
                    Ok(true)
                } else {
                    journal("agent_drain_pending", &[("node", &node)]);
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
        let _ = undrain(runner, &node, ctx);
        return Err(Error::msg(format!(
            "timed out after {} waiting for node {node} to drain{}",
            ctx.cfg.drain_timeout,
            detail_suffix(drain_error)
        )));
    }
    journal("agent_drained", &[("node", &node)]);

    let opts = install_options(ctx, version, false);
    let outcome = match runner.install(component, &opts) {
        Ok(o) => o,
        Err(e) => {
            let _ = undrain(runner, &node, ctx);
            return Err(e);
        }
    };
    journal(
        "agent_installed",
        &[("component", &component.name), ("version", version)],
    );

    let health_timeout = parse_duration(&ctx.cfg.health_timeout)?;
    let mut startup_error = None;
    if let Err(e) = restart(runner, unit, ctx) {
        startup_error = Some(e.to_string());
    }
    let (healthy, health_error) = if startup_error.is_some() {
        (false, startup_error)
    } else {
        poll_until(health_timeout, || node_healthy(runner, &node, version, ctx))
    };
    if healthy {
        // Only advance state once the new version is confirmed live.
        install::record_installed(component, version, receipt(&outcome)?, ctx.prefix)?;
        undrain(runner, &node, ctx)?;
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
            "node {node} did not report version {version} within {}",
            ctx.cfg.health_timeout
        )
    });
    recover_agent(
        component,
        version,
        &node,
        ctx,
        state,
        runner,
        &health_reason,
        health_timeout,
    )
    .map(|()| None)
}

/// Restore the previous version after a failed start or health check. Returns
/// an `Err` describing both the original failure and any recovery failure.
#[allow(clippy::too_many_arguments)]
fn recover_agent(
    component: &Component,
    version: &str,
    node: &str,
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

    if !recovery_errors.is_empty() {
        // Intentionally leave the node drained: an unverified node must not
        // accept jobs. The bad version is already persisted.
        return Err(Error::msg(format!(
            "health check failed within {} for {} on {node}: {}; recovery failed: {}",
            ctx.cfg.health_timeout,
            component.name,
            failure_reason,
            one_line(&recovery_errors.join("; "))
        )));
    }

    undrain(runner, node, ctx)?;
    Err(Error::msg(format!(
        "health check failed within {} for {} on {node}: {}; restored .prev, verified restored health and marked {version} bad",
        ctx.cfg.health_timeout,
        component.name,
        failure_reason
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
    let out = match checked(runner, "lu", &["status", "--json"], cmd_timeout) {
        Ok(out) => out,
        Err(e) => {
            return Ok(Some(format!(
                "cannot check controller idleness: {}",
                one_line(&e.to_string())
            )))
        }
    };
    if !lu::controller_idle(&out.stdout) {
        return Ok(Some(
            "controller is not idle (jobs leased, running, recovering or needing input)"
                .to_string(),
        ));
    }
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

/// Back up the controller before installing a new binary. The real Lu 0.4
/// interface is `lu backup --out <FILE>`; a failed or empty backup refuses the
/// update so a controller is never replaced without a restore point.
fn take_backup(
    cfg: &AutoUpdateConfig,
    runner: &dyn ApplyRunner,
    cmd_timeout: Duration,
    config_path: Option<&Path>,
) -> Result<()> {
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

    let metadata = fs::metadata(&path).map_err(|source| Error::Read {
        path: path.clone(),
        source,
    })?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(Error::msg(format!(
            "lu backup did not produce a non-empty backup file at {}",
            path.display()
        )));
    }

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
            ("kept", &keep.to_string()),
        ],
    );
    Ok(())
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
    take_backup(ctx.cfg, runner, cmd_timeout, ctx.config_path)?;
    let opts = install_options(ctx, version, false);
    let outcome = runner.install(component, &opts)?;

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
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    const NODES_OLD: &str =
        r#"[{"name":"n1","version":"0.4.0","draining":true,"current_job":null}]"#;
    const NODES_NEW: &str =
        r#"[{"name":"n1","version":"0.4.1","draining":true,"current_job":null}]"#;
    const STATUS_IDLE_OLD: &str =
        r#"{"version":"0.4.0","jobs":{"leased":0,"running":0,"recovering":0,"needs_input":0}}"#;
    const STATUS_IDLE_NEW: &str =
        r#"{"version":"0.4.1","jobs":{"leased":0,"running":0,"recovering":0,"needs_input":0}}"#;

    #[derive(Default)]
    struct Calls {
        commands: Vec<(String, Vec<String>)>,
        installs: usize,
        archive_checks: usize,
    }

    struct FakeRunner {
        calls: Arc<Mutex<Calls>>,
        fail: BTreeMap<String, String>,
        nodes: Arc<Mutex<String>>,
        status: Arc<Mutex<String>>,
        receipt: crate::install::InstallReceipt,
        archive_names: Option<Vec<String>>,
    }

    impl FakeRunner {
        fn new() -> Self {
            Self {
                calls: Arc::new(Mutex::new(Calls::default())),
                fail: BTreeMap::new(),
                nodes: Arc::new(Mutex::new(NODES_OLD.to_string())),
                status: Arc::new(Mutex::new(STATUS_IDLE_OLD.to_string())),
                receipt: crate::install::InstallReceipt {
                    checksum: "abc".to_string(),
                    target: "test".to_string(),
                    installed_at: "now".to_string(),
                },
                archive_names: None,
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
            let body = match (program, args.first().copied(), args.get(1).copied()) {
                ("lu", Some("nodes"), Some("--json")) => self.nodes.lock().unwrap().clone(),
                ("lu", Some("status"), Some("--json")) => self.status.lock().unwrap().clone(),
                ("lu", Some("backup"), _) => {
                    if let Some(pos) = args.iter().position(|a| *a == "--out") {
                        if let Some(path) = args.get(pos + 1) {
                            let _ = fs::write(path, b"backup payload");
                        }
                    }
                    String::new()
                }
                _ => String::new(),
            };
            Ok(CommandOutput {
                status: 0,
                stdout: body,
                stderr: String::new(),
            })
        }

        fn install(&self, c: &Component, _opts: &InstallOptions) -> Result<InstallOutcome> {
            self.calls.lock().unwrap().installs += 1;
            Ok(InstallOutcome {
                component: c.name.clone(),
                version: "v0.4.1".to_string(),
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
    }

    fn cfg(backup_dir: &Path) -> AutoUpdateConfig {
        AutoUpdateConfig {
            enabled: true,
            node_name: Some("n1".to_string()),
            drain_timeout: "1s".to_string(),
            health_timeout: "1s".to_string(),
            command_timeout: "1s".to_string(),
            controller_policy: ControllerPolicy::WhenIdle,
            backup_dir: Some(backup_dir.to_path_buf()),
            ..AutoUpdateConfig::default()
        }
    }

    fn component() -> Component {
        component::builtin_components()
            .into_iter()
            .find(|c| c.name == "ting")
            .unwrap()
    }

    #[test]
    fn agent_uses_real_commands_and_waits_for_exact_version() {
        let td = tempfile::tempdir().unwrap();
        let ctx = ApplyContext {
            cfg: &cfg(&td.path().join("backups")),
            prefix: Some(td.path()),
            config_path: Some(Path::new("tuishou.toml")),
        };
        let c = component();
        let mut st = AutoState::default();
        let mut existing = state::State::default();
        existing.components.insert(
            "ting".to_string(),
            state::InstalledComponent {
                version: "v0.4.0".to_string(),
                target: "test".to_string(),
                checksum: "abc".to_string(),
                installed_at: "before".to_string(),
                previous_version: None,
            },
        );
        state::save(&state::default_state_path(Some(td.path())), &existing).unwrap();

        let runner = FakeRunner::new();
        let nodes = runner.nodes.clone();
        let calls = runner.calls.clone();
        *nodes.lock().unwrap() = NODES_NEW.to_string();
        apply_agent_with_runner(&c, "v0.4.1", &ctx, &mut st, &runner).unwrap();

        let cmds = calls.lock().unwrap().commands.clone();
        assert!(cmds.iter().any(|(p, a)| p == "lu" && a == &["drain", "n1"]));
        assert!(cmds
            .iter()
            .any(|(p, a)| p == "lu" && a == &["nodes", "--json"]));
        assert!(cmds
            .iter()
            .any(|(p, a)| p == "lu" && a == &["undrain", "n1"]));
        assert!(!cmds
            .iter()
            .any(|(p, a)| p == "lu" && a.iter().any(|x| x == "node")));
        assert_eq!(calls.lock().unwrap().installs, 1);
    }

    #[test]
    fn agent_absent_from_archive_is_skipped_before_drain() {
        let td = tempfile::tempdir().unwrap();
        let ctx = ApplyContext {
            cfg: &cfg(&td.path().join("backups")),
            prefix: Some(td.path()),
            config_path: Some(Path::new("tuishou.toml")),
        };
        let mut runner = FakeRunner::new();
        runner.archive_names = Some(vec!["lu".to_string()]);
        let reason = apply_agent_with_runner(
            &component(),
            "v0.4.1",
            &ctx,
            &mut AutoState::default(),
            &runner,
        )
        .unwrap()
        .expect("missing agent must be skipped");
        assert!(reason.contains("not in this release"), "{reason}");
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.archive_checks, 1);
        assert_eq!(calls.installs, 0);
        assert!(!calls
            .commands
            .iter()
            .any(|(p, a)| p == "lu" && a.first().map(String::as_str) == Some("drain")));
    }

    #[test]
    fn failed_undrain_is_propagated() {
        let td = tempfile::tempdir().unwrap();
        let ctx = ApplyContext {
            cfg: &cfg(&td.path().join("backups")),
            prefix: Some(td.path()),
            config_path: Some(Path::new("tuishou.toml")),
        };
        let mut runner = FakeRunner::new();
        *runner.nodes.lock().unwrap() = NODES_NEW.to_string();
        runner
            .fail
            .insert("lu undrain n1".to_string(), "permission denied".to_string());
        let err = apply_agent_with_runner(
            &component(),
            "v0.4.1",
            &ctx,
            &mut AutoState::default(),
            &runner,
        )
        .unwrap_err();
        assert!(err.to_string().contains("permission denied"));
    }

    #[test]
    fn recovery_failure_keeps_node_drained_and_persists_bad_mark() {
        let td = tempfile::tempdir().unwrap();
        let prefix = Some(td.path());
        let ctx = ApplyContext {
            cfg: &cfg(&td.path().join("backups")),
            prefix,
            config_path: Some(Path::new("tuishou.toml")),
        };
        let mut runner = FakeRunner::new();
        runner.fail.insert(
            "systemctl is-active --quiet lu-agent.service".to_string(),
            "agent unhealthy".to_string(),
        );
        let mut st = AutoState::default();
        let err =
            apply_agent_with_runner(&component(), "v0.4.1", &ctx, &mut st, &runner).unwrap_err();
        assert!(err.to_string().contains("agent unhealthy"));
        assert!(err.to_string().contains("recovery failed"));
        assert!(st
            .bad_versions
            .get("ting")
            .unwrap()
            .contains(&"v0.4.1".to_string()));
        let saved = crate::auto_update::state::load(prefix).unwrap();
        assert!(saved
            .bad_versions
            .get("ting")
            .unwrap()
            .contains(&"v0.4.1".to_string()));
        let cmds = runner.calls.lock().unwrap().commands.clone();
        assert!(!cmds
            .iter()
            .any(|(p, a)| p == "lu" && a.first().map(String::as_str) == Some("undrain")));
    }

    #[test]
    fn restart_failure_triggers_recovery_without_undrain() {
        let td = tempfile::tempdir().unwrap();
        let prefix = Some(td.path());
        let ctx = ApplyContext {
            cfg: &cfg(&td.path().join("backups")),
            prefix,
            config_path: Some(Path::new("tuishou.toml")),
        };
        let mut runner = FakeRunner::new();
        runner.fail.insert(
            "systemctl restart lu-agent.service".to_string(),
            "restart exploded".to_string(),
        );
        let mut existing = state::State::default();
        existing.components.insert(
            "ting".to_string(),
            state::InstalledComponent {
                version: "v0.4.0".to_string(),
                target: "test".to_string(),
                checksum: "abc".to_string(),
                installed_at: "before".to_string(),
                previous_version: None,
            },
        );
        state::save(&state::default_state_path(prefix), &existing).unwrap();
        let mut st = AutoState::default();
        let err =
            apply_agent_with_runner(&component(), "v0.4.1", &ctx, &mut st, &runner).unwrap_err();
        assert!(err.to_string().contains("restart exploded"));
        assert!(st
            .bad_versions
            .get("ting")
            .unwrap()
            .contains(&"v0.4.1".to_string()));
        let cmds = runner.calls.lock().unwrap().commands.clone();
        assert!(!cmds
            .iter()
            .any(|(p, a)| p == "lu" && a.first().map(String::as_str) == Some("undrain")));
    }

    #[test]
    fn controller_backs_up_to_a_unique_out_file_with_the_cli_config() {
        let td = tempfile::tempdir().unwrap();
        let ctx = ApplyContext {
            cfg: &cfg(&td.path().join("backups")),
            prefix: Some(td.path()),
            config_path: Some(Path::new("tuishou.toml")),
        };
        let runner = FakeRunner::new();
        *runner.status.lock().unwrap() = STATUS_IDLE_NEW.to_string();
        let mut st = AutoState::default();
        let lu = crate::component::builtin_components()
            .into_iter()
            .find(|c| c.name == "lu")
            .unwrap();
        let skipped =
            apply_controller_with_runner(&lu, "v0.4.1", "release notes", &ctx, &mut st, &runner)
                .unwrap();
        assert!(skipped.is_none());
        let cmds = runner.calls.lock().unwrap().commands.clone();
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
        assert!(cmds
            .iter()
            .any(|(p, a)| p == "lu" && a == &["status", "--json"]));
        assert!(cmds
            .iter()
            .any(|(p, a)| p == "systemctl" && a == &["restart", "lu.service"]));
    }

    #[test]
    fn controller_fails_closed_on_busy_status() {
        let td = tempfile::tempdir().unwrap();
        let ctx = ApplyContext {
            cfg: &cfg(&td.path().join("backups")),
            prefix: Some(td.path()),
            config_path: Some(Path::new("tuishou.toml")),
        };
        let runner = FakeRunner::new();
        *runner.status.lock().unwrap() =
            r#"{"version":"0.4.0","jobs":{"leased":1,"running":0,"recovering":0,"needs_input":0}}"#
                .to_string();
        let reason = apply_controller_with_runner(
            &component::builtin_components()
                .into_iter()
                .find(|c| c.name == "lu")
                .unwrap(),
            "v0.4.1",
            "release notes",
            &ctx,
            &mut AutoState::default(),
            &runner,
        )
        .unwrap()
        .unwrap();
        assert!(reason.contains("not idle"));
        assert_eq!(runner.calls.lock().unwrap().installs, 0);
    }
}
