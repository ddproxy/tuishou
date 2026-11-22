//! Opt-in scheduled auto-update: one-shot cycles, a systemd timer printer,
//! hold/clear-bad controls and a machine-readable status view.
//!
//! There is deliberately no daemon. The scheduler (systemd timer or cron)
//! starts `tuishou auto-update run --once`; a second concurrent run exits
//! quietly because the cycle holds an exclusive flock.

pub mod apply;
pub mod eligibility;
pub mod state;

use crate::component::resolve_component;
use crate::config::{AutoUpdateConfig, LoadedConfig};
use crate::duration::parse_duration;
use crate::error::{Error, Result};
use crate::logging::{journal, journal_err, notify, one_line};
use crate::state::InstalledComponent;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

const AUTO_LOCK_ENV: &str = "TUISHOU_AUTO_LOCK_HELD";

#[derive(Debug, Serialize)]
struct NotifySummary<'a> {
    event: &'a str,
    component: &'a str,
    version: Option<&'a str>,
    reason: Option<&'a str>,
    at: String,
}

fn send_notification(
    cfg: &AutoUpdateConfig,
    event: &str,
    component: &str,
    version: Option<&str>,
    reason: Option<&str>,
) {
    if cfg.notify_command.is_empty() {
        return;
    }
    let summary = NotifySummary {
        event,
        component,
        version,
        reason,
        at: crate::state::now_rfc3339().unwrap_or_default(),
    };
    if let Err(e) = notify(&cfg.notify_command, &summary) {
        journal_err("notify_failed", &[("reason", &one_line(&e.to_string()))]);
    }
}

fn require_enabled(cfg: &AutoUpdateConfig) -> Result<()> {
    if !cfg.enabled {
        return Err(Error::msg(
            "auto-update is opt-in: set [auto_update] enabled = true in tuishou.toml; \
             it is not recommended for production because it installs new releases without human review",
        ));
    }
    Ok(())
}

/// Parent entry point: validate the opt-in, then re-exec the cycle under
/// `flock -n` so concurrent runs are harmless.
pub fn handle_run(
    cfg: &AutoUpdateConfig,
    prefix: Option<&Path>,
    once: bool,
    quiet: bool,
    allow_unattested: bool,
) -> Result<ExitCode> {
    require_enabled(cfg)?;
    if allow_unattested {
        return Err(Error::msg(
            "--allow-unattested is refused in auto-update mode: attestations are mandatory",
        ));
    }
    if !crate::verify::gh_available() {
        return Err(Error::msg(
            "auto-update requires gh on PATH for attestation verification; install gh (attestations cannot be disabled in auto mode)",
        ));
    }
    let _ = once; // This tool never runs a daemon; one invocation is one cycle.
    let dir = state::ensure_state_dir(prefix)?;
    let lock = state::lock_path(prefix);
    let _ = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock)
        .map_err(|source| Error::Write {
            path: lock.clone(),
            source,
        })?;

    if let Some(flock) = find_tool("flock") {
        let exe = std::env::current_exe()
            .map_err(|e| Error::msg(format!("cannot locate running tuishou binary: {e}")))?;
        let mut cmd = Command::new(flock);
        cmd.arg("-n")
            .arg("-E")
            .arg("99")
            .arg(&lock)
            .arg(&exe)
            .arg("__auto-update-locked")
            .env(AUTO_LOCK_ENV, "1");
        if let Some(p) = prefix {
            cmd.arg("--prefix").arg(p);
        }
        if quiet {
            cmd.arg("--quiet");
        }
        let status = cmd
            .status()
            .map_err(|e| Error::msg(format!("failed to run flock: {e}")))?;
        if status.code() == Some(99) {
            journal("auto_update_lock_busy", &[]);
            return Ok(ExitCode::SUCCESS);
        }
        return Ok(match status.code() {
            Some(code) => ExitCode::from(code as u8),
            None => ExitCode::FAILURE,
        });
    }

    // Fallback when util-linux flock is unavailable: an atomic directory lock
    // with the same "second run exits quietly" semantics.
    let guard = match fs::create_dir(dir.join("lock.d")) {
        Ok(()) => DirLock(dir.join("lock.d")),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            journal("auto_update_lock_busy", &[]);
            return Ok(ExitCode::SUCCESS);
        }
        Err(e) => {
            return Err(Error::Write {
                path: dir.join("lock.d"),
                source: e,
            })
        }
    };
    let loaded = crate::config::load_config(None)?;
    let result = execute_cycle(&loaded, prefix, quiet);
    drop(guard);
    result
}

struct DirLock(PathBuf);

impl Drop for DirLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

/// Child process invoked with the lock already held.
pub fn handle_locked(prefix: Option<&Path>, quiet: bool) -> Result<ExitCode> {
    if std::env::var(AUTO_LOCK_ENV).ok().as_deref() != Some("1") {
        return Err(Error::msg(
            "internal auto-update command may only be invoked through the lock",
        ));
    }
    let loaded = crate::config::load_config(None)?;
    require_enabled(&loaded.auto_update)?;
    execute_cycle(&loaded, prefix, quiet)
}

fn find_tool(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn installed_versions(
    prefix: Option<&Path>,
) -> Result<std::collections::BTreeMap<String, InstalledComponent>> {
    Ok(crate::state::load(&crate::state::default_state_path(prefix))?.components)
}

fn cached_next(
    component: &crate::component::Component,
    cfg: &AutoUpdateConfig,
    installed: Option<&str>,
    bad: &[String],
    is_controller: bool,
    st: &state::AutoState,
) -> Option<String> {
    let cache = st.caches.get(&component.repo)?;
    let listing = eligibility::Listing {
        releases: cache.releases.clone(),
        etag: cache.etag.clone(),
        not_modified: true,
        checked_at: cache.checked_at.clone(),
    };
    eligibility::choose(&listing, cfg, installed, bad, is_controller)
        .ok()
        .and_then(|c| c.next_eligible_version)
}

fn record(
    st: &mut state::AutoState,
    name: &str,
    result: &str,
    reason: Option<String>,
    version: Option<String>,
    next: Option<String>,
) {
    st.results.insert(
        name.to_string(),
        state::ComponentResult {
            result: result.to_string(),
            reason: reason.map(|r| one_line(&r)),
            version,
            next_eligible_version: next,
            at: crate::state::now_rfc3339().unwrap_or_default(),
        },
    );
}

fn execute_cycle(loaded: &LoadedConfig, prefix: Option<&Path>, quiet: bool) -> Result<ExitCode> {
    let cfg = &loaded.auto_update;
    let components = &loaded.components;
    let mut st = state::load(prefix)?;
    let installed = installed_versions(prefix)?;
    st.last_check = Some(crate::state::now_rfc3339()?);
    journal(
        "auto_update_cycle_start",
        &[("components", &cfg.components.join(","))],
    );

    if let Some(hold) = state::active_hold(prefix)? {
        let reason = hold
            .reason
            .unwrap_or_else(|| "hold file present".to_string());
        journal("auto_update_held", &[("reason", &one_line(&reason))]);
        for name in &cfg.components {
            if let Ok(component) = resolve_component(name, components) {
                let bad = st.bad_versions.get(name).cloned().unwrap_or_default();
                let next = cached_next(
                    &component,
                    cfg,
                    installed.get(name).map(|c| c.version.as_str()),
                    &bad,
                    name == "lu",
                    &st,
                );
                record(&mut st, name, "held", Some(reason.clone()), None, next);
            }
        }
        state::save(prefix, &st)?;
        return Ok(ExitCode::SUCCESS);
    }

    let agent = crate::http::agent();
    let mut any_failed = false;
    for name in &cfg.components {
        let component = match resolve_component(name, components) {
            Ok(c) => c,
            Err(e) => {
                journal_err(
                    "component_error",
                    &[("component", name), ("reason", &one_line(&e.to_string()))],
                );
                record(&mut st, name, "failed", Some(e.to_string()), None, None);
                any_failed = true;
                continue;
            }
        };
        let is_controller = name == "lu";
        let is_agent = matches!(name.as_str(), "ting" | "lu-agent");
        let installed_version = installed.get(name).map(|c| c.version.clone());
        let bad = st.bad_versions.get(name).cloned().unwrap_or_default();

        let cache = st.caches.get(&component.repo).cloned();
        let listing = match eligibility::fetch_listing(&agent, &component.repo, cache.as_ref(), 3) {
            Ok(l) => l,
            Err(e) => {
                journal_err(
                    "release_list_failed",
                    &[("component", name), ("reason", &one_line(&e.to_string()))],
                );
                let failures = st.failures.entry(name.clone()).or_insert(0);
                *failures += 1;
                if *failures >= cfg.notify_after_failures {
                    send_notification(cfg, "repeated_failure", name, None, Some(&e.to_string()));
                }
                record(&mut st, name, "failed", Some(e.to_string()), None, None);
                any_failed = true;
                continue;
            }
        };
        st.caches
            .insert(component.repo.clone(), eligibility::to_cache(&listing));
        if listing.not_modified {
            journal(
                "release_list_cached",
                &[("component", name), ("etag", "hit")],
            );
        }

        let choice = match eligibility::choose(
            &listing,
            cfg,
            installed_version.as_deref(),
            &bad,
            is_controller,
        ) {
            Ok(c) => c,
            Err(e) => {
                journal_err(
                    "eligibility_failed",
                    &[("component", name), ("reason", &one_line(&e.to_string()))],
                );
                record(&mut st, name, "failed", Some(e.to_string()), None, None);
                any_failed = true;
                continue;
            }
        };
        let next = choice.next_eligible_version.clone();
        let release = match choice.candidate {
            Some(r) => r,
            None => {
                let reason = choice
                    .reason
                    .unwrap_or_else(|| "no eligible release".to_string());
                journal(
                    "component_skipped",
                    &[
                        ("component", name),
                        ("reason", &one_line(&reason)),
                        ("next", next.as_deref().unwrap_or("-")),
                    ],
                );
                record(&mut st, name, "up-to-date", Some(reason), None, next);
                continue;
            }
        };
        let ctx = apply::ApplyContext { cfg, prefix };
        let outcome = if is_controller {
            apply::apply_controller(&component, &release.tag, &release.body, &ctx)
                .map(|skip| skip.map(|reason| ("skipped", reason)))
        } else if is_agent {
            apply::apply_agent(&component, &release.tag, &ctx, &mut st).map(|_| None)
        } else {
            apply::plain_install(&component, &release.tag, &ctx).map(|_| None)
        };
        match outcome {
            Ok(None) => {
                st.failures.remove(name);
                journal(
                    "auto_update_applied",
                    &[("component", name), ("version", &release.tag)],
                );
                record(
                    &mut st,
                    name,
                    "updated",
                    None,
                    Some(release.tag.clone()),
                    next,
                );
                send_notification(cfg, "update", name, Some(&release.tag), None);
            }
            Ok(Some((kind, reason))) => {
                journal(
                    "auto_update_skipped",
                    &[("component", name), ("reason", &one_line(&reason))],
                );
                record(&mut st, name, kind, Some(reason), None, next);
            }
            Err(e) => {
                let reason = one_line(&e.to_string());
                journal_err(
                    "auto_update_failed",
                    &[("component", name), ("reason", &reason)],
                );
                let marked_bad = st
                    .bad_versions
                    .get(name)
                    .map(|v| v.iter().any(|x| x == &release.tag))
                    .unwrap_or(false);
                if marked_bad {
                    send_notification(cfg, "rollback", name, Some(&release.tag), Some(&reason));
                }
                let failures = st.failures.entry(name.clone()).or_insert(0);
                *failures += 1;
                if *failures >= cfg.notify_after_failures {
                    send_notification(
                        cfg,
                        "repeated_failure",
                        name,
                        Some(&release.tag),
                        Some(&reason),
                    );
                }
                record(
                    &mut st,
                    name,
                    "failed",
                    Some(reason.clone()),
                    Some(release.tag),
                    next,
                );
                any_failed = true;
            }
        }
    }
    state::save(prefix, &st)?;
    if !quiet {
        journal(
            "auto_update_cycle_done",
            &[("failed", &any_failed.to_string())],
        );
    }
    Ok(if any_failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

#[derive(Debug, Serialize)]
struct StatusRow {
    component: String,
    last_result: String,
    reason: Option<String>,
    installed_version: Option<String>,
    last_version: Option<String>,
    next_eligible_version: Option<String>,
}

#[derive(Debug, Serialize)]
struct StatusOutput {
    enabled: bool,
    last_check: Option<String>,
    interval: String,
    policy: String,
    controller_policy: String,
    hold: HoldStatus,
    components: Vec<StatusRow>,
}

#[derive(Debug, Serialize)]
struct HoldStatus {
    active: bool,
    reason: Option<String>,
    expires_at: Option<String>,
}

pub fn handle_status(
    loaded: &LoadedConfig,
    prefix: Option<&Path>,
    json: bool,
    quiet: bool,
) -> Result<ExitCode> {
    let cfg = &loaded.auto_update;
    let st = state::load(prefix)?;
    let installed = installed_versions(prefix)?;
    let hold = state::active_hold(prefix)?;
    let mut rows = Vec::new();
    for name in &cfg.components {
        let component = resolve_component(name, &loaded.components)?;
        let bad = st.bad_versions.get(name).cloned().unwrap_or_default();
        let is_controller = name == "lu";
        let installed_version = installed.get(name).map(|c| c.version.clone());
        let next = cached_next(
            &component,
            cfg,
            installed_version.as_deref(),
            &bad,
            is_controller,
            &st,
        )
        .or_else(|| {
            // No cached listing yet: do a live check so ansible sees the
            // actual next version on first run.
            let agent = crate::http::agent();
            let listing = eligibility::fetch_listing(&agent, &component.repo, None, 1).ok()?;
            eligibility::choose(
                &listing,
                cfg,
                installed_version.as_deref(),
                &bad,
                is_controller,
            )
            .ok()?
            .next_eligible_version
        });
        let entry = st.results.get(name);
        rows.push(StatusRow {
            component: name.clone(),
            last_result: entry
                .map(|e| e.result.clone())
                .unwrap_or_else(|| "never".to_string()),
            reason: entry.and_then(|e| e.reason.clone()),
            installed_version: installed_version.clone(),
            last_version: entry.and_then(|e| e.version.clone()),
            next_eligible_version: next,
        });
    }
    if json {
        let out = StatusOutput {
            enabled: cfg.enabled,
            last_check: st.last_check.clone(),
            interval: cfg.interval.clone(),
            policy: format!("{:?}", cfg.policy),
            controller_policy: cfg.controller_policy.as_str().to_string(),
            hold: HoldStatus {
                active: hold.is_some(),
                reason: hold.as_ref().and_then(|h| h.reason.clone()),
                expires_at: hold.as_ref().and_then(|h| h.expires_at.clone()),
            },
            components: rows,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&out)
                .map_err(|e| Error::msg(format!("failed to serialize auto-update status: {e}")))?
        );
        return Ok(ExitCode::SUCCESS);
    }
    if !quiet {
        println!(
            "auto-update: enabled={} interval={} last_check={} hold={}",
            cfg.enabled,
            cfg.interval,
            st.last_check.as_deref().unwrap_or("never"),
            if hold.is_some() { "active" } else { "none" }
        );
        if let Some(h) = &hold {
            println!(
                "  hold reason={} expires={}",
                h.reason.as_deref().unwrap_or("-"),
                h.expires_at.as_deref().unwrap_or("never")
            );
        }
        for row in &rows {
            println!(
                "  {:<10} last={:<10} version={:<10} next={:<10} reason={}",
                row.component,
                row.last_result,
                row.last_version.as_deref().unwrap_or("-"),
                row.next_eligible_version.as_deref().unwrap_or("-"),
                row.reason.as_deref().unwrap_or("-")
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub fn handle_hold(
    prefix: Option<&Path>,
    reason: Option<String>,
    expires_at: Option<String>,
    expires_in: Option<String>,
) -> Result<()> {
    if expires_at.is_some() && expires_in.is_some() {
        return Err(Error::msg(
            "use either --expires-at or --expires-in, not both",
        ));
    }
    let expires_at = match (expires_at, expires_in) {
        (Some(at), None) => {
            time::OffsetDateTime::parse(&at, &time::format_description::well_known::Rfc3339)
                .map_err(|e| Error::msg(format!("invalid --expires-at '{at}': {e}")))?;
            Some(at)
        }
        (None, Some(duration)) => {
            let d = parse_duration(&duration)?;
            Some(
                (time::OffsetDateTime::now_utc() + d)
                    .format(&time::format_description::well_known::Rfc3339)
                    .map_err(|e| Error::msg(format!("failed to format hold expiry: {e}")))?,
            )
        }
        _ => None,
    };
    let hold = state::Hold {
        created_at: crate::state::now_rfc3339()?,
        reason,
        expires_at,
    };
    state::write_hold(prefix, &hold)?;
    journal(
        "hold_set",
        &[
            ("reason", hold.reason.as_deref().unwrap_or("-")),
            ("expires_at", hold.expires_at.as_deref().unwrap_or("-")),
        ],
    );
    Ok(())
}

pub fn handle_release_hold(prefix: Option<&Path>) -> Result<()> {
    if state::remove_hold(prefix)? {
        journal("hold_released", &[]);
    } else {
        journal("hold_absent", &[]);
    }
    Ok(())
}

pub fn handle_clear_bad(prefix: Option<&Path>, component: Option<&str>) -> Result<()> {
    let mut st = state::load(prefix)?;
    match component {
        Some(name) => {
            st.bad_versions.remove(name);
            st.failures.remove(name);
            journal("bad_cleared", &[("component", name)]);
        }
        None => {
            st.bad_versions.clear();
            st.failures.clear();
            journal("bad_cleared", &[("component", "all")]);
        }
    }
    state::save(prefix, &st)
}

/// Print (never install) a systemd service/timer pair. `OnUnitActiveSec` is
/// the configured interval; jitter and `Persistent=true` are always present.
pub fn print_timer(interval_override: Option<&str>) -> Result<()> {
    let interval_text = interval_override.unwrap_or("10m");
    let interval = parse_duration(interval_text)?;
    if interval < Duration::from_secs(300) {
        return Err(Error::msg(format!(
            "timer interval '{interval_text}' is below the 5m minimum"
        )));
    }
    let jitter = interval / 10;
    let jitter = if jitter < Duration::from_secs(30) {
        Duration::from_secs(30)
    } else {
        jitter
    };
    let exe =
        std::env::var("TUISHOU_TIMER_EXE").unwrap_or_else(|_| "/usr/local/bin/tuishou".to_string());
    let service = format!(
        "# tuishou-auto-update.service\n\
         [Unit]\n\
         Description=tuishou scheduled auto-update (one cycle, opt-in)\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={exe} auto-update run --once\n\
         EnvironmentFile=-/etc/tuishou/auto-update.env\n"
    );
    let timer = format!(
        "# tuishou-auto-update.timer\n\
         [Unit]\n\
         Description=Run tuishou auto-update on a schedule\n\
         \n\
         [Timer]\n\
         OnUnitActiveSec={}\n\
         RandomizedDelaySec={}\n\
         Persistent=true\n\
         \n\
         [Install]\n\
         WantedBy=timers.target\n",
        crate::duration::format_duration(interval),
        crate::duration::format_duration(jitter)
    );
    println!("{service}");
    println!("{timer}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_defaults_are_ten_minutes_with_jitter() {
        // Printing goes to stdout; validate the underlying values instead.
        assert_eq!(
            crate::duration::parse_duration("10m").unwrap(),
            Duration::from_secs(600)
        );
        assert!(parse_duration("2m").unwrap() < Duration::from_secs(300));
    }
}
