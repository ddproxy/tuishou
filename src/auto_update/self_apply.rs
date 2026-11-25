//! Self-update applied at the end of an auto-update cycle. The running process
//! keeps its cycle lock for the whole apply, so no component update can be in
//! flight while its own binary is replaced.

use crate::auto_update::apply::{self, ApplyContext, ApplyRunner, SystemRunner};
use crate::auto_update::state::{self, AutoState, SelfPending};
use crate::component::Component;
use crate::duration::parse_duration;
use crate::error::{Error, Result};
use crate::install::{self, InstallOutcome, InstallReceipt};
use crate::logging::{journal, journal_err, one_line};
use crate::rollback;
use crate::state as install_state;
use std::path::Path;
use std::time::Duration;

pub const SELF_COMPONENT: &str = crate::self_update::SELF_COMPONENT;
/// Only the timer is restarted. Restarting the oneshot service here would kill
/// the cycle that is applying the update.
pub const SELF_TIMER_UNIT: &str = "tuishou-auto-update.timer";

fn receipt(outcome: &InstallOutcome) -> Result<InstallReceipt> {
    outcome
        .receipt
        .clone()
        .ok_or_else(|| Error::msg("self-update returned no receipt; refusing to record state"))
}

fn is_smoke_failure(err: &Error) -> bool {
    one_line(&err.to_string()).contains("smoke check failed")
}

fn command_timeout(ctx: &ApplyContext<'_>) -> Duration {
    parse_duration(&ctx.cfg.command_timeout).unwrap_or(Duration::from_secs(30))
}

/// Restart only the timer. A failed restart is reported but does not roll back
/// a verified binary: cron deployments have no systemd unit to restart.
fn restart_timer(runner: &dyn ApplyRunner, ctx: &ApplyContext<'_>) {
    match runner.command(
        "systemctl",
        &["try-restart", SELF_TIMER_UNIT],
        command_timeout(ctx),
    ) {
        Ok(out) if out.status == 0 => {
            journal("self_timer_restarted", &[("unit", SELF_TIMER_UNIT)]);
        }
        Ok(out) => journal_err(
            "self_timer_restart_failed",
            &[("status", &out.status.to_string())],
        ),
        Err(e) => journal_err(
            "self_timer_restart_failed",
            &[("reason", &one_line(&e.to_string()))],
        ),
    }
}

fn recorded_version(prefix: Option<&Path>) -> Option<String> {
    install_state::load(&install_state::default_state_path(prefix))
        .ok()?
        .components
        .get(SELF_COMPONENT)
        .map(|c| c.version.clone())
}

/// Apply an eligible tuishou release. The install path verifies checksum and
/// attestation, then runs `--version` on the staged binary before any installed
/// file is replaced. The new version stays pending until the next cycle starts
/// and confirms that the binary can run.
pub fn apply_self(
    component: &Component,
    version: &str,
    installed_version: Option<&str>,
    ctx: &ApplyContext<'_>,
    st: &mut AutoState,
) -> Result<Option<String>> {
    apply_self_with_runner(
        component,
        version,
        installed_version,
        ctx,
        st,
        &SystemRunner,
    )
}

pub fn apply_self_with_runner(
    component: &Component,
    version: &str,
    installed_version: Option<&str>,
    ctx: &ApplyContext<'_>,
    st: &mut AutoState,
    runner: &dyn ApplyRunner,
) -> Result<Option<String>> {
    let bin_dir = crate::self_update::binary_dir(ctx.prefix)?;
    let previous_version = installed_version
        .map(str::to_string)
        .or_else(|| recorded_version(Some(&bin_dir)));

    let mut opts = apply::install_options(ctx, version, true);
    // The managed binary lives beside the running one. State written by the
    // installer follows that prefix; the cycle state is recorded separately.
    opts.prefix = Some(bin_dir.clone());
    opts.smoke_check = true;
    opts.staged_check = None;

    let outcome = match runner.install(component, &opts) {
        Ok(outcome) => outcome,
        Err(e) if is_smoke_failure(&e) => {
            apply::mark_bad(st, SELF_COMPONENT, version, ctx.prefix);
            return Err(Error::msg(format!(
                "{e}; marked {version} bad and the previous tuishou binary was kept"
            )));
        }
        Err(e) => return Err(e),
    };
    let receipt = receipt(&outcome)?;
    // The installer already recorded state under the binary directory. Mirror
    // it into the cycle prefix only when that is a different state file.
    if ctx.prefix != Some(bin_dir.as_path()) {
        if let Err(e) = install::record_installed(component, version, &receipt, ctx.prefix) {
            let _ = restore_previous(
                component,
                version,
                previous_version.as_deref(),
                &bin_dir,
                ctx.prefix,
            );
            return Err(Error::msg(format!(
                "self-update state commit failed after installing {version}: {}",
                one_line(&e.to_string())
            )));
        }
    }

    st.self_pending = Some(SelfPending {
        version: version.to_string(),
        previous_version,
        path: bin_dir.join(&component.name),
        at: crate::state::now_rfc3339()?,
    });
    state::save(ctx.prefix, st)?;
    journal(
        "self_updated",
        &[
            ("component", SELF_COMPONENT),
            ("version", version),
            ("path", &bin_dir.join(&component.name).to_string_lossy()),
        ],
    );
    restart_timer(runner, ctx);
    Ok(None)
}

/// Compare a `--version` line to a release tag. Both `tuishou 0.3.6` and
/// `tuishou v0.3.6-dev.1.gabc` identify their tags.
fn version_matches(output: &str, tag: &str) -> bool {
    let expected = tag.strip_prefix('v').unwrap_or(tag).trim();
    output
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .any(|word| {
            word.strip_prefix('v')
                .unwrap_or(word)
                .trim_matches(|c: char| c == ',' || c == ';')
                == expected
        })
}

fn revert_version(prefix: Option<&Path>, version: &str, previous: Option<&str>) -> Result<()> {
    let path = install_state::default_state_path(prefix);
    let mut st = install_state::load(&path)?;
    if let Some(entry) = st.components.get_mut(SELF_COMPONENT) {
        if entry.version == version {
            entry.version = previous
                .filter(|p| !p.is_empty() && *p != version)
                .unwrap_or("restored")
                .to_string();
            entry.previous_version = Some(version.to_string());
        }
    }
    install_state::save(&path, &st)
}

fn restore_previous(
    component: &Component,
    version: &str,
    previous: Option<&str>,
    bin_dir: &Path,
    cycle_prefix: Option<&Path>,
) -> Result<()> {
    rollback::restore_binaries(component, Some(bin_dir), false)?;
    revert_version(Some(bin_dir), version, previous)?;
    revert_version(cycle_prefix, version, previous)?;
    Ok(())
}

/// Confirm a self-update on the first scheduled pass after replacement. A
/// binary that cannot run or does not report the expected tag is restored from
/// `.prev`, the tag is marked bad, and the pending record is cleared.
pub fn confirm_pending(
    component: &Component,
    ctx: &ApplyContext<'_>,
    st: &mut AutoState,
    runner: &dyn ApplyRunner,
) -> Result<Option<String>> {
    let Some(pending) = st.self_pending.clone() else {
        return Ok(None);
    };
    let path = pending.path.to_string_lossy().into_owned();
    let startup = runner.command(&path, &["--version"], command_timeout(ctx));
    let confirmed = matches!(&startup, Ok(out) if out.status == 0 && version_matches(&out.stdout, &pending.version));
    if confirmed {
        st.self_pending = None;
        state::save(ctx.prefix, st)?;
        journal(
            "self_update_confirmed",
            &[("version", &pending.version), ("path", &path)],
        );
        return Ok(None);
    }

    let detail = match startup {
        Ok(out) if out.status == 0 => format!(
            "reported version {:?}, expected {}",
            one_line(out.stdout.trim()),
            pending.version
        ),
        Ok(out) => format!("exit status {}", out.status),
        Err(e) => one_line(&e.to_string()),
    };
    let bin_dir = pending
        .path
        .parent()
        .ok_or_else(|| Error::msg("pending self-update has no binary directory"))?;
    apply::mark_bad(st, SELF_COMPONENT, &pending.version, ctx.prefix);
    let recovery = restore_previous(
        component,
        &pending.version,
        pending.previous_version.as_deref(),
        bin_dir,
        ctx.prefix,
    );
    st.self_pending = None;
    state::save(ctx.prefix, st)?;
    let reason = match recovery {
        Ok(()) => format!(
            "first scheduled pass could not start {}: {}; restored .prev and marked {} bad",
            pending.version, detail, pending.version
        ),
        Err(e) => format!(
            "first scheduled pass could not start {}: {}; marked {} bad; rollback failed: {}",
            pending.version,
            detail,
            pending.version,
            one_line(&e.to_string())
        ),
    };
    journal_err(
        "self_update_startup_failed",
        &[
            ("version", &pending.version),
            ("reason", &one_line(&reason)),
        ],
    );
    Ok(Some(reason))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto_update::apply::ApplyRunner;
    use crate::component;
    use crate::config::AutoUpdateConfig;
    use crate::install::{InstallOptions, InstallReceipt};
    use crate::process::CommandOutput;
    use crate::state::{self as install_state, InstalledComponent};
    use std::collections::VecDeque;
    use std::fs;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct FakeRunner {
        commands: Arc<Mutex<Vec<String>>>,
        outputs: Arc<Mutex<VecDeque<(i32, String)>>>,
        install_ok: Arc<Mutex<bool>>,
    }

    impl ApplyRunner for FakeRunner {
        fn command(
            &self,
            program: &str,
            args: &[&str],
            _timeout: Duration,
        ) -> Result<CommandOutput> {
            self.commands
                .lock()
                .unwrap()
                .push(format!("{program} {}", args.join(" ")));
            let (status, stdout) = self
                .outputs
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or((0, String::new()));
            Ok(CommandOutput {
                status,
                stdout,
                stderr: String::new(),
            })
        }

        fn install(&self, component: &Component, opts: &InstallOptions) -> Result<InstallOutcome> {
            if !*self.install_ok.lock().unwrap() {
                return Err(Error::msg("smoke check failed for tuishou: exit status 1"));
            }
            let bin_dir = opts
                .prefix
                .as_ref()
                .expect("self install must set a prefix");
            fs::create_dir_all(bin_dir).unwrap();
            fs::write(bin_dir.join(&component.name), b"new").unwrap();
            let state_path = install_state::default_state_path(opts.prefix.as_deref());
            let mut state = install_state::load(&state_path).unwrap_or_default();
            let previous_version = state
                .components
                .get(&component.name)
                .map(|entry| entry.version.clone());
            state.components.insert(
                component.name.clone(),
                InstalledComponent {
                    version: opts.version.clone().unwrap_or_default(),
                    target: "test".to_string(),
                    checksum: "a".repeat(64),
                    installed_at: "2026-01-01T00:00:00Z".to_string(),
                    previous_version,
                },
            );
            install_state::save(&state_path, &state).unwrap();
            Ok(InstallOutcome {
                component: component.name.clone(),
                version: opts.version.clone().unwrap_or_default(),
                binaries: vec![component.name.clone()],
                dry_run: false,
                receipt: Some(InstallReceipt {
                    checksum: "a".repeat(64),
                    target: "test".to_string(),
                    installed_at: "2026-01-01T00:00:00Z".to_string(),
                }),
            })
        }

        fn archive_binaries(&self, _component: &Component, _version: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }

        fn health_endpoint(
            &self,
            _url: &str,
            _timeout: Duration,
        ) -> std::result::Result<bool, String> {
            Ok(true)
        }
    }

    fn ctx<'a>(prefix: Option<&'a Path>, cfg: &'a AutoUpdateConfig) -> ApplyContext<'a> {
        ApplyContext {
            cfg,
            prefix,
            config_path: None,
            verifier: None,
            verify_config: None,
        }
    }

    fn seed_installed(bin_dir: &Path, version: &str) {
        let path = install_state::default_state_path(Some(bin_dir));
        let mut st = install_state::State::default();
        st.components.insert(
            SELF_COMPONENT.to_string(),
            InstalledComponent {
                version: version.to_string(),
                target: "test".to_string(),
                checksum: "a".repeat(64),
                installed_at: "2026-01-01T00:00:00Z".to_string(),
                previous_version: None,
            },
        );
        install_state::save(&path, &st).unwrap();
    }

    #[test]
    fn version_matches_tag_with_or_without_leading_v() {
        assert!(version_matches("tuishou 0.3.6\n", "v0.3.6"));
        assert!(version_matches(
            "tuishou v0.3.6-dev.1.gabc\n",
            "v0.3.6-dev.1.gabc"
        ));
        assert!(!version_matches("tuishou 0.3.5\n", "v0.3.6"));
    }

    #[test]
    fn apply_self_restarts_only_the_timer_and_records_pending() {
        let td = tempfile::tempdir().unwrap();
        let bin_dir = td.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        seed_installed(&bin_dir, "v0.3.5");
        let cfg = AutoUpdateConfig::default();
        let context = ctx(Some(&bin_dir), &cfg);
        let commands = Arc::new(Mutex::new(Vec::new()));
        let outputs = Arc::new(Mutex::new(VecDeque::from([(0, String::new())])));
        let runner = FakeRunner {
            commands: commands.clone(),
            outputs,
            install_ok: Arc::new(Mutex::new(true)),
        };
        let mut st = AutoState::default();
        let c =
            component::resolve_component(SELF_COMPONENT, &component::builtin_components()).unwrap();
        apply_self_with_runner(&c, "v0.3.6", Some("v0.3.5"), &context, &mut st, &runner).unwrap();

        let calls = commands.lock().unwrap().clone();
        assert!(
            calls.contains(&format!("systemctl try-restart {SELF_TIMER_UNIT}")),
            "{calls:?}"
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.contains("restart tuishou-auto-update.service")),
            "{calls:?}"
        );
        let pending = st.self_pending.expect("pending must be recorded");
        assert_eq!(pending.version, "v0.3.6");
        assert_eq!(pending.previous_version.as_deref(), Some("v0.3.5"));
        assert_eq!(pending.path, bin_dir.join("tuishou"));
        assert_eq!(
            install_state::load(&install_state::default_state_path(Some(&bin_dir)))
                .unwrap()
                .components[SELF_COMPONENT]
                .version,
            "v0.3.6"
        );
    }

    #[test]
    fn apply_self_smoke_failure_marks_bad_and_keeps_installed_version() {
        let td = tempfile::tempdir().unwrap();
        let bin_dir = td.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        seed_installed(&bin_dir, "v0.3.5");
        let cfg = AutoUpdateConfig::default();
        let context = ctx(Some(td.path()), &cfg);
        let runner = FakeRunner {
            commands: Arc::new(Mutex::new(Vec::new())),
            outputs: Arc::new(Mutex::new(VecDeque::new())),
            install_ok: Arc::new(Mutex::new(false)),
        };
        let mut st = AutoState::default();
        let c =
            component::resolve_component(SELF_COMPONENT, &component::builtin_components()).unwrap();
        let err = apply_self_with_runner(&c, "v0.3.6", Some("v0.3.5"), &context, &mut st, &runner)
            .unwrap_err()
            .to_string();
        assert!(err.contains("smoke check failed"), "{err}");
        assert_eq!(st.bad_versions[SELF_COMPONENT][0], "v0.3.6");
        assert!(st.self_pending.is_none());
        assert_eq!(
            install_state::load(&install_state::default_state_path(Some(td.path())))
                .unwrap()
                .components
                .get(SELF_COMPONENT),
            None
        );
    }

    #[test]
    fn failed_first_pass_restores_prev_and_marks_bad() {
        let td = tempfile::tempdir().unwrap();
        let bin_dir = td.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        fs::write(bin_dir.join("tuishou"), b"new").unwrap();
        fs::write(bin_dir.join("tuishou.prev"), b"good").unwrap();
        seed_installed(&bin_dir, "v0.3.6");
        let state_path = install_state::default_state_path(Some(&bin_dir));
        let mut good = install_state::KnownGood::default();
        good.checksums.insert(
            SELF_COMPONENT.to_string(),
            crate::verify::sha256_file(&bin_dir.join("tuishou.prev")).unwrap(),
        );
        install_state::save_known_good(&state_path, &good).unwrap();
        let cfg = AutoUpdateConfig::default();
        let context = ctx(Some(td.path()), &cfg);
        let outputs = Arc::new(Mutex::new(VecDeque::from([(1, String::new())])));
        let runner = FakeRunner {
            commands: Arc::new(Mutex::new(Vec::new())),
            outputs,
            install_ok: Arc::new(Mutex::new(true)),
        };
        let mut st = AutoState {
            self_pending: Some(SelfPending {
                version: "v0.3.6".to_string(),
                previous_version: Some("v0.3.5".to_string()),
                path: bin_dir.join("tuishou"),
                at: "2026-01-01T00:00:00Z".to_string(),
            }),
            ..AutoState::default()
        };
        let c =
            component::resolve_component(SELF_COMPONENT, &component::builtin_components()).unwrap();
        let reason = confirm_pending(&c, &context, &mut st, &runner)
            .unwrap()
            .expect("rollback must be reported");
        assert!(reason.contains("restored .prev"), "{reason}");
        assert!(reason.contains("marked v0.3.6 bad"), "{reason}");
        assert_eq!(fs::read(bin_dir.join("tuishou")).unwrap(), b"good");
        assert!(st.self_pending.is_none());
        assert_eq!(st.bad_versions[SELF_COMPONENT][0], "v0.3.6");
        assert_eq!(
            install_state::load(&state_path).unwrap().components[SELF_COMPONENT].version,
            "v0.3.5"
        );
    }
}
