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
use crate::duration::{format_duration, parse_duration};
use crate::error::{Error, Result};
use crate::logging::{journal, journal_err, notify, one_line};
use crate::state::InstalledComponent;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

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
    let timeout = match parse_duration(&cfg.command_timeout) {
        Ok(t) => t,
        Err(e) => {
            journal_err("notify_failed", &[("reason", &one_line(&e.to_string()))]);
            return;
        }
    };
    if let Err(e) = notify(&cfg.notify_command, &summary, timeout) {
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

/// Run one cycle while holding the OS advisory lock. The kernel releases the
/// lock even if the process is killed, so a stale lock cannot block later runs.
pub fn handle_run(
    loaded: &LoadedConfig,
    prefix: Option<&Path>,
    once: bool,
    quiet: bool,
    allow_unattested: bool,
) -> Result<ExitCode> {
    let cfg = &loaded.auto_update;
    require_enabled(cfg)?;
    if allow_unattested {
        return Err(Error::msg(
            "--allow-unattested is refused in auto-update mode: attestations are mandatory",
        ));
    }
    let _ = once; // This tool never runs a daemon; one invocation is one cycle.
    let _guard = match state::CycleLock::try_acquire(prefix)? {
        Some(guard) => guard,
        None => {
            journal("auto_update_lock_busy", &[]);
            return Ok(ExitCode::SUCCESS);
        }
    };
    execute_cycle(loaded, prefix, quiet)
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
    let verifier = crate::verify::default_verifier();
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
        let ctx = apply::ApplyContext {
            cfg,
            prefix,
            config_path: Some(loaded.config_path.as_path()),
            verifier: Some(verifier.clone()),
            verify_config: Some(&loaded.verify),
        };
        let outcome = if is_controller {
            apply::apply_controller(&component, &release.tag, &release.body, &ctx, &mut st)
                .map(|skip| skip.map(|reason| ("skipped", reason)))
        } else if is_agent {
            apply::apply_agent(&component, &release.tag, &ctx, &mut st)
                .map(|skip| skip.map(|reason| ("skipped", reason)))
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
    state::with_cycle_lock(prefix, || handle_status_locked(loaded, prefix, json, quiet))
}

fn handle_status_locked(
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
    state::with_cycle_lock(prefix, || {
        handle_hold_locked(prefix, reason, expires_at, expires_in)
    })
}

fn handle_hold_locked(
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
    state::with_cycle_lock(prefix, || handle_release_hold_locked(prefix))
}

fn handle_release_hold_locked(prefix: Option<&Path>) -> Result<()> {
    if state::remove_hold(prefix)? {
        journal("hold_released", &[]);
    } else {
        journal("hold_absent", &[]);
    }
    Ok(())
}

pub fn handle_clear_bad(prefix: Option<&Path>, component: Option<&str>) -> Result<()> {
    state::with_cycle_lock(prefix, || handle_clear_bad_locked(prefix, component))
}

fn handle_clear_bad_locked(prefix: Option<&Path>, component: Option<&str>) -> Result<()> {
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

/// Print (never install) a systemd service/timer pair. Both `OnBootSec` and
/// `OnActiveSec` are emitted, with jitter and `Persistent=true`.
pub fn print_timer(
    interval_override: Option<&str>,
    config_path: Option<&Path>,
    prefix: Option<&Path>,
    configured_interval: &str,
) -> Result<()> {
    let (service, timer) =
        timer_units(interval_override, config_path, prefix, configured_interval)?;
    println!("{service}");
    println!("{timer}");
    Ok(())
}

fn absolute_path(path: Option<&Path>, label: &str) -> Result<PathBuf> {
    let path = match path {
        Some(p) => p.to_path_buf(),
        None => crate::config::default_config_path(),
    };
    if path.is_absolute() {
        return Ok(path);
    }
    let cwd = std::env::current_dir().map_err(|e| {
        Error::msg(format!(
            "cannot determine current directory for {label}: {e}"
        ))
    })?;
    Ok(cwd.join(path))
}

fn absolute_config_path(config_path: Option<&Path>) -> Result<PathBuf> {
    absolute_path(config_path, "config path")
}

fn timer_units(
    interval_override: Option<&str>,
    config_path: Option<&Path>,
    prefix: Option<&Path>,
    configured_interval: &str,
) -> Result<(String, String)> {
    let interval_text = interval_override.unwrap_or(configured_interval);
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
    let config = absolute_config_path(config_path)?;
    let prefix_arg = match prefix {
        Some(p) => format!(
            " --prefix {}",
            absolute_path(Some(p), "install prefix")?.display()
        ),
        None => String::new(),
    };
    let service = format!(
        "# tuishou-auto-update.service\n\
         [Unit]\n\
         Description=tuishou scheduled auto-update (one cycle, opt-in)\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={exe} --config {} auto-update run --once{}\n\
         EnvironmentFile=-/etc/tuishou/auto-update.env\n",
        config.display(),
        prefix_arg
    );
    let timer = format!(
        "# tuishou-auto-update.timer\n\
         [Unit]\n\
         Description=Run tuishou auto-update on a schedule\n\
         \n\
         [Timer]\n\
         OnBootSec={}\n\
         OnActiveSec={}\n\
         RandomizedDelaySec={}\n\
         Persistent=true\n\
         \n\
         [Install]\n\
         WantedBy=timers.target\n",
        format_duration(interval),
        format_duration(interval),
        format_duration(jitter)
    );
    Ok((service, timer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_defaults_are_ten_minutes_with_jitter() {
        assert_eq!(parse_duration("10m").unwrap(), Duration::from_secs(600));
        assert!(parse_duration("2m").unwrap() < Duration::from_secs(300));
    }

    #[test]
    fn generated_timer_uses_boot_and_active_timers_and_absolute_config() {
        let (service, timer) =
            timer_units(Some("10m"), Some(Path::new("relative.toml")), None, "10m").unwrap();
        assert!(timer.contains("OnBootSec=10m"));
        assert!(timer.contains("OnActiveSec=10m"));
        assert!(service.contains("--config /"));
        assert!(!service.contains("--config relative.toml"));
        assert!(!service.contains("--prefix"));
    }

    #[test]
    fn generated_timer_carries_an_absolute_prefix() {
        let (service, timer) = timer_units(
            None,
            Some(Path::new("tuishou.toml")),
            Some(Path::new("relative-prefix")),
            "15m",
        )
        .unwrap();
        assert!(timer.contains("OnBootSec=15m"));
        assert!(service.contains("--prefix /"));
        assert!(!service.contains("--prefix relative-prefix"));
    }
}
