//! Opt-in scheduled auto-update: one-shot cycles, a systemd timer printer,
//! hold/clear-bad controls and a machine-readable status view.
//!
//! There is deliberately no daemon. The scheduler (systemd timer or cron)
//! starts `tuishou auto-update run --once`; a second concurrent run exits
//! quietly because the cycle holds an exclusive flock.

pub mod apply;
pub mod eligibility;
pub mod self_apply;
pub mod state;

use crate::component::{resolve_component, Component};
use crate::config::{AutoUpdateConfig, LoadedConfig};
use crate::duration::{format_duration, parse_duration};
use crate::error::{Error, Result};
use crate::logging::{journal, journal_err, notify, one_line};
use crate::report::{self, ComponentReport, StatusText};
use crate::self_update::SELF_COMPONENT;
use crate::state::InstalledComponent;
use serde::Serialize;
use std::collections::BTreeMap;
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
    json: bool,
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
    execute_cycle(loaded, prefix, quiet, json)
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

/// Best-effort role lookup for read-only reporting. Apply paths use the
/// strict detector and fail closed on ambiguity; status simply avoids showing
/// controller gating for a host where the role cannot be confirmed.
fn installed_lu_is_controller(
    ctx: &apply::ApplyContext<'_>,
    installed: &BTreeMap<String, InstalledComponent>,
) -> bool {
    installed.contains_key("lu")
        && apply::detect_lu_role(ctx)
            .map(|r| r == apply::LuRole::Controller)
            .unwrap_or(false)
}

fn record(
    st: &mut state::AutoState,
    name: &str,
    status: StatusText,
    installed_version: Option<String>,
    version: Option<String>,
    next: Option<String>,
) {
    let kind = status.kind().to_string();
    let phrase = status.to_string();
    let reason = match &status {
        StatusText::Held { reason }
        | StatusText::Skipped { reason }
        | StatusText::Failed { reason } => Some(one_line(reason)),
        _ => None,
    };
    let missing_binaries = match &status {
        StatusText::NotInRelease { missing } => missing.clone(),
        _ => Vec::new(),
    };
    st.results.insert(
        name.to_string(),
        state::ComponentResult {
            result: phrase,
            status_kind: Some(kind),
            reason,
            version,
            installed_version,
            next_eligible_version: next,
            missing_binaries,
            at: crate::state::now_rfc3339().unwrap_or_default(),
        },
    );
}

fn cycle_names(cfg: &AutoUpdateConfig) -> Vec<String> {
    let mut names = cfg.components.clone();
    // Reiki precedes Lu, and a self-update is always last so it can never run
    // while another component's apply path is in flight. Other components keep
    // their configured order.
    names.sort_by_key(|name| match name.as_str() {
        "reiki" => 0,
        "lu" => 1,
        SELF_COMPONENT => 3,
        _ => 2,
    });
    names
}

fn status_from_skip(component: &Component, reason: &str, version: &str) -> StatusText {
    if reason.contains("not in this release") {
        let missing = reason
            .split_once('(')
            .and_then(|(_, rest)| rest.split_once(')'))
            .map(|(list, _)| {
                list.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| component.required_binaries(version));
        StatusText::NotInRelease { missing }
    } else {
        StatusText::Skipped {
            reason: one_line(reason),
        }
    }
}

/// One report per configured component, in cycle order, using the shared
/// status vocabulary. Stored results keep their phrase so a failed apply is
/// not rewritten into a misleading "up to date" after the fact.
fn cycle_reports(
    loaded: &LoadedConfig,
    st: &state::AutoState,
    installed: &BTreeMap<String, InstalledComponent>,
) -> Vec<ComponentReport> {
    cycle_names(&loaded.auto_update)
        .iter()
        .filter_map(|name| {
            let component = resolve_component(name, &loaded.components).ok()?;
            let entry = st.results.get(name);
            let (status, entry_installed, latest, missing) = match entry {
                Some(e) => (
                    report::status_text_from_kind(
                        e.status_kind.as_deref().unwrap_or("failed"),
                        e.installed_version.clone(),
                        e.reason.clone(),
                        e.version.clone(),
                        e.missing_binaries.clone(),
                    ),
                    e.installed_version.clone(),
                    e.version.clone(),
                    e.missing_binaries.clone(),
                ),
                None => (
                    StatusText::Skipped {
                        reason: "no result recorded".to_string(),
                    },
                    None,
                    None,
                    Vec::new(),
                ),
            };
            let current = installed
                .get(name)
                .map(|i| i.version.clone())
                .or(entry_installed);
            Some(ComponentReport::from_parts(
                &component, status, current, latest, missing,
            ))
        })
        .collect()
}

fn execute_cycle(
    loaded: &LoadedConfig,
    prefix: Option<&Path>,
    quiet: bool,
    json: bool,
) -> Result<ExitCode> {
    validate_environment_file()?;
    let cfg = &loaded.auto_update;
    let components = &loaded.components;
    let names = cycle_names(cfg);
    let mut st = state::load(prefix)?;
    let mut installed = installed_versions(prefix)?;
    st.last_check = Some(crate::state::now_rfc3339()?);
    journal(
        "auto_update_cycle_start",
        &[("components", &names.join(","))],
    );

    let verifier = crate::verify::default_verifier();
    let ctx = apply::ApplyContext {
        cfg,
        prefix,
        config_path: Some(loaded.config_path.as_path()),
        verifier: Some(verifier.clone()),
        verify_config: Some(&loaded.verify),
    };
    let command_timeout = parse_duration(&cfg.command_timeout).unwrap_or(Duration::from_secs(30));
    let mut any_failed = false;
    let mut self_handled = false;

    // A replaced binary must prove it can start on the first scheduled pass.
    // This runs before hold/fleet gates so a broken binary is never left in
    // place because the operator paused the rest of the cycle.
    if cfg.self_update {
        match resolve_component(SELF_COMPONENT, components) {
            Ok(component) => {
                let outcome =
                    self_apply::confirm_pending(&component, &ctx, &mut st, &apply::SystemRunner);
                installed = installed_versions(prefix)?;
                match outcome {
                    Ok(None) => {}
                    Ok(Some(reason)) => {
                        self_handled = true;
                        journal_err("self_update_rollback", &[("reason", &one_line(&reason))]);
                        record(
                            &mut st,
                            SELF_COMPONENT,
                            StatusText::Failed { reason },
                            installed.get(SELF_COMPONENT).map(|c| c.version.clone()),
                            None,
                            None,
                        );
                        any_failed = true;
                    }
                    Err(e) => {
                        self_handled = true;
                        let reason = one_line(&e.to_string());
                        journal_err("self_update_confirm_failed", &[("reason", &reason)]);
                        record(
                            &mut st,
                            SELF_COMPONENT,
                            StatusText::Failed {
                                reason: reason.clone(),
                            },
                            installed.get(SELF_COMPONENT).map(|c| c.version.clone()),
                            None,
                            None,
                        );
                        any_failed = true;
                    }
                }
            }
            Err(e) => {
                self_handled = true;
                let reason = one_line(&e.to_string());
                record(
                    &mut st,
                    SELF_COMPONENT,
                    StatusText::Failed {
                        reason: reason.clone(),
                    },
                    installed.get(SELF_COMPONENT).map(|c| c.version.clone()),
                    None,
                    None,
                );
                any_failed = true;
            }
        }
    }

    if let Some(reason) = crate::fleet::before_cycle(&loaded.update, command_timeout) {
        journal(
            "auto_update_fleet_skipped",
            &[("reason", &one_line(&reason))],
        );
        for name in &names {
            if self_handled && name == SELF_COMPONENT {
                continue;
            }
            if resolve_component(name, components).is_ok() {
                record(
                    &mut st,
                    name,
                    StatusText::Skipped {
                        reason: reason.clone(),
                    },
                    installed.get(name).map(|c| c.version.clone()),
                    None,
                    None,
                );
            }
        }
        state::save(prefix, &st)?;
        let reports = cycle_reports(loaded, &st, &installed);
        report::print_reports(&reports, json, quiet)?;
        return Ok(ExitCode::SUCCESS);
    }

    state::expire_hold(prefix)?;
    if let Some(hold) = state::active_hold(prefix)? {
        let reason = hold
            .reason
            .unwrap_or_else(|| "hold file present".to_string());
        journal("auto_update_held", &[("reason", &one_line(&reason))]);
        let lu_is_controller = installed_lu_is_controller(&ctx, &installed);
        for name in &names {
            if self_handled && name == SELF_COMPONENT {
                continue;
            }
            if let Ok(component) = resolve_component(name, components) {
                let bad = st.bad_versions.get(name).cloned().unwrap_or_default();
                let next = cached_next(
                    &component,
                    cfg,
                    installed.get(name).map(|c| c.version.as_str()),
                    &bad,
                    name == "lu" && lu_is_controller,
                    &st,
                );
                record(
                    &mut st,
                    name,
                    StatusText::Held {
                        reason: reason.clone(),
                    },
                    installed.get(name).map(|c| c.version.clone()),
                    None,
                    next,
                );
            }
        }
        state::save(prefix, &st)?;
        let reports = cycle_reports(loaded, &st, &installed);
        report::print_reports(&reports, json, quiet)?;
        return Ok(ExitCode::SUCCESS);
    }

    let agent = crate::http::agent();
    for name in &names {
        if self_handled && name == SELF_COMPONENT {
            continue;
        }
        let component = match resolve_component(name, components) {
            Ok(c) => c,
            Err(e) => {
                let reason = one_line(&e.to_string());
                journal_err(
                    "component_error",
                    &[("component", name), ("reason", &reason)],
                );
                record(
                    &mut st,
                    name,
                    StatusText::Failed {
                        reason: reason.clone(),
                    },
                    installed.get(name).map(|c| c.version.clone()),
                    None,
                    None,
                );
                any_failed = true;
                continue;
            }
        };
        let is_self = cfg.self_update && name == SELF_COMPONENT;
        if !component.auto_update && !is_self {
            let reason =
                format!("auto_update is disabled for {name}; set [[component]] auto_update = true");
            journal("component_auto_update_disabled", &[("component", name)]);
            record(
                &mut st,
                name,
                StatusText::Skipped {
                    reason: reason.clone(),
                },
                installed.get(name).map(|c| c.version.clone()),
                None,
                None,
            );
            continue;
        }

        let is_reiki = name == "reiki";
        let is_lu = name == "lu";
        let installed_version = installed.get(name).map(|c| c.version.clone());
        let bad = st.bad_versions.get(name).cloned().unwrap_or_default();
        // Lu's role is a property of the installed host, not of the release.
        // Detect it once from systemd and use it for both eligibility and
        // apply. Ambiguous hosts fail closed instead of being guessed at.
        let role = if is_lu && installed_version.is_some() {
            match apply::detect_lu_role(&ctx) {
                Ok(role) => Some(role),
                Err(e) => {
                    let reason = one_line(&e.to_string());
                    journal_err(
                        "lu_role_failed",
                        &[("component", name), ("reason", &reason)],
                    );
                    record(
                        &mut st,
                        name,
                        StatusText::Failed {
                            reason: reason.clone(),
                        },
                        installed_version.clone(),
                        None,
                        None,
                    );
                    any_failed = true;
                    continue;
                }
            }
        } else {
            None
        };
        let is_controller = role == Some(apply::LuRole::Controller);

        let cache = st.caches.get(&component.repo).cloned();
        let listing = match eligibility::fetch_listing(&agent, &component.repo, cache.as_ref(), 3) {
            Ok(l) => l,
            Err(e) => {
                let reason = one_line(&e.to_string());
                journal_err(
                    "release_list_failed",
                    &[("component", name), ("reason", &reason)],
                );
                let failures = st.failures.entry(name.clone()).or_insert(0);
                *failures += 1;
                if *failures >= cfg.notify_after_failures {
                    send_notification(cfg, "repeated_failure", name, None, Some(&reason));
                }
                record(
                    &mut st,
                    name,
                    StatusText::Failed {
                        reason: reason.clone(),
                    },
                    installed_version.clone(),
                    None,
                    None,
                );
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
                let reason = one_line(&e.to_string());
                journal_err(
                    "eligibility_failed",
                    &[("component", name), ("reason", &reason)],
                );
                record(
                    &mut st,
                    name,
                    StatusText::Failed {
                        reason: reason.clone(),
                    },
                    installed_version.clone(),
                    None,
                    None,
                );
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
                let status = if reason.contains("not installed") {
                    StatusText::NotInstalled {
                        latest: next.clone().unwrap_or_default(),
                    }
                } else if next.as_deref().is_some_and(|v| bad.iter().any(|b| b == v)) {
                    StatusText::BadVersionSkipped {
                        version: next.clone().unwrap_or_default(),
                    }
                } else {
                    StatusText::Skipped {
                        reason: one_line(&reason),
                    }
                };
                record(&mut st, name, status, installed_version.clone(), None, next);
                continue;
            }
        };
        let outcome = if is_reiki {
            apply::apply_reiki(&component, &release.tag, &ctx, &mut st)
        } else if is_lu {
            // `role` was detected from installed/enabled systemd units above.
            let Some(role) = role else {
                return Err(Error::msg(
                    "internal error: Lu role was not detected for an installed component",
                ));
            };
            apply::apply_lu(&component, &release.tag, &release.body, role, &ctx, &mut st)
        } else if is_self {
            // Self runs last in the cycle, under the same lock, so no component
            // apply can overlap the binary replacement.
            self_apply::apply_self(
                &component,
                &release.tag,
                installed_version.as_deref(),
                &ctx,
                &mut st,
            )
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
                    StatusText::Updated {
                        from: installed_version.clone().unwrap_or_else(|| "-".to_string()),
                        to: release.tag.clone(),
                    },
                    installed_version.clone(),
                    Some(release.tag.clone()),
                    next,
                );
                send_notification(cfg, "update", name, Some(&release.tag), None);
            }
            Ok(Some(reason)) => {
                journal(
                    "auto_update_skipped",
                    &[("component", name), ("reason", &one_line(&reason))],
                );
                record(
                    &mut st,
                    name,
                    status_from_skip(&component, &reason, &release.tag),
                    installed_version.clone(),
                    Some(release.tag.clone()),
                    next,
                );
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
                    StatusText::Failed {
                        reason: reason.clone(),
                    },
                    installed_version.clone(),
                    Some(release.tag.clone()),
                    next,
                );
                any_failed = true;
            }
        }
    }
    state::save(prefix, &st)?;
    let installed_after = installed_versions(prefix)?;
    let reports = cycle_reports(loaded, &st, &installed_after);
    report::print_reports(&reports, json, quiet)?;
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
    status: String,
    last_result: String,
    reason: Option<String>,
    installed_version: Option<String>,
    last_version: Option<String>,
    next_eligible_version: Option<String>,
    last_check: Option<String>,
    bad_versions: Vec<String>,
    auto_update: bool,
}

#[derive(Debug, Serialize)]
struct StatusOutput {
    enabled: bool,
    self_update: bool,
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
    state::expire_hold(prefix)?;
    let hold = state::active_hold(prefix)?;
    let names = cycle_names(cfg);
    let ordered: Vec<_> = names
        .iter()
        .filter_map(|name| resolve_component(name, &loaded.components).ok())
        .collect();
    let hold_reason = hold.as_ref().map(|h| {
        h.reason
            .clone()
            .unwrap_or_else(|| "hold file present".to_string())
    });
    let status_ctx = apply::ApplyContext {
        cfg,
        prefix,
        config_path: Some(loaded.config_path.as_path()),
        verifier: None,
        verify_config: None,
    };
    let lu_is_controller = installed_lu_is_controller(&status_ctx, &installed);
    // The same evaluator backs `status`, `update --check` and auto-update so
    // their per-component lines cannot disagree.
    let reports = report::evaluate_all(
        &crate::http::agent(),
        &ordered,
        prefix,
        &installed,
        hold_reason.as_deref(),
        &st.bad_versions,
    );
    let mut rows = Vec::new();
    for (name, current) in names.iter().zip(reports) {
        let component = match ordered.iter().find(|c| c.name == *name) {
            Some(c) => c,
            None => continue,
        };
        let bad = st.bad_versions.get(name).cloned().unwrap_or_default();
        let installed_version = installed.get(name).map(|c| c.version.clone());
        let next = cached_next(
            component,
            cfg,
            installed_version.as_deref(),
            &bad,
            name == "lu" && lu_is_controller,
            &st,
        )
        .or_else(|| current.latest_version.clone());
        let entry = st.results.get(name);
        rows.push(StatusRow {
            component: name.clone(),
            status: current.status.clone(),
            last_result: entry
                .map(|e| e.result.clone())
                .unwrap_or_else(|| "never".to_string()),
            reason: current
                .reason
                .clone()
                .or_else(|| entry.and_then(|e| e.reason.clone())),
            installed_version: current.installed_version.clone(),
            last_version: entry.and_then(|e| e.version.clone()),
            next_eligible_version: next,
            last_check: entry.map(|e| e.at.clone()),
            bad_versions: bad,
            auto_update: component.auto_update || (*name == SELF_COMPONENT && cfg.self_update),
        });
    }
    if json {
        let out = StatusOutput {
            enabled: cfg.enabled,
            self_update: cfg.self_update,
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
            "auto-update: enabled={} self={} interval={} last_check={} hold={}",
            cfg.enabled,
            cfg.self_update,
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
                "{}: {} | auto_update={} installed={} latest={} last_check={} last_result={} bad={}",
                row.component,
                row.status,
                row.auto_update,
                row.installed_version.as_deref().unwrap_or("-"),
                row.next_eligible_version.as_deref().unwrap_or("-"),
                row.last_check.as_deref().unwrap_or("never"),
                row.last_result,
                if row.bad_versions.is_empty() {
                    "-".to_string()
                } else {
                    row.bad_versions.join(",")
                }
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

/// Reject values that systemd would split, expand or reinterpret. The unit is
/// printed for an operator to install, so it must not contain surprises.
fn validate_unit_value(value: &str, label: &str) -> Result<()> {
    let unsafe_char = value
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '%' | '$' | '"' | '\'' | '\\'));
    let parent_dir = value.split(['/', '\\']).any(|part| part == "..");
    if value.is_empty() || unsafe_char || parent_dir {
        return Err(Error::msg(format!(
            "{label} contains characters that cannot be safely embedded in a systemd unit"
        )));
    }
    Ok(())
}

const TRUSTED_TIMER_DIRS: [&str; 4] = ["/usr/local/bin", "/usr/bin", "/bin", "/opt/tuishou/bin"];
const DEFAULT_TIMER_EXE: &str = "/usr/local/bin/tuishou";

fn timer_executable() -> Result<String> {
    let override_exe = std::env::var("TUISHOU_TIMER_EXE").ok();
    let exe = override_exe
        .clone()
        .unwrap_or_else(|| DEFAULT_TIMER_EXE.to_string());
    validate_unit_value(&exe, "TUISHOU_TIMER_EXE")?;
    let path = Path::new(&exe);
    if !path.is_absolute() {
        return Err(Error::msg(
            "TUISHOU_TIMER_EXE must be an absolute path in a trusted directory",
        ));
    }
    let in_trusted_dir = |candidate: &Path| {
        TRUSTED_TIMER_DIRS
            .iter()
            .any(|dir| candidate.starts_with(dir))
    };
    if !in_trusted_dir(path) {
        return Err(Error::msg(
            "TUISHOU_TIMER_EXE must be under /usr/local/bin, /usr/bin, /bin or /opt/tuishou/bin",
        ));
    }
    let Some(_) = override_exe else {
        // The compiled-in default is a fixed, trusted absolute path. It is
        // printed for an operator to install; the file need not exist on the
        // machine generating the unit.
        return Ok(exe);
    };
    let canonical = std::fs::canonicalize(path).map_err(|e| {
        Error::msg(format!(
            "TUISHOU_TIMER_EXE must name an existing regular file: {e}"
        ))
    })?;
    let trusted: Vec<PathBuf> = TRUSTED_TIMER_DIRS
        .iter()
        .filter_map(|dir| std::fs::canonicalize(dir).ok())
        .collect();
    if !trusted.iter().any(|dir| canonical.starts_with(dir)) {
        return Err(Error::msg(
            "TUISHOU_TIMER_EXE must resolve inside a trusted directory",
        ));
    }
    crate::process::validate_executable(&canonical)?;
    Ok(canonical.to_string_lossy().into_owned())
}

/// The shipped service reads an optional EnvironmentFile. A file writable by
/// other users could replace PATH before children are resolved, so refuse to
/// start a cycle when that file is not root-owned and private.
fn validate_environment_file() -> Result<()> {
    validate_environment_file_at(Path::new("/etc/tuishou/auto-update.env"))
}

#[cfg(unix)]
fn validate_environment_file_at(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if !meta.file_type().is_file() {
        return Err(Error::msg(
            "auto-update environment file must be a regular file",
        ));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(Error::msg(
            "auto-update environment file must not be writable by group or others",
        ));
    }
    if meta.uid() != 0 {
        return Err(Error::msg(
            "auto-update environment file must be owned by root",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_environment_file_at(_path: &Path) -> Result<()> {
    Ok(())
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
    let exe = timer_executable()?;
    let config = absolute_config_path(config_path)?;
    validate_unit_value(&config.to_string_lossy(), "config path")?;
    let prefix_arg = match prefix {
        Some(p) => {
            let path = absolute_path(Some(p), "install prefix")?;
            validate_unit_value(&path.to_string_lossy(), "install prefix")?;
            format!(" --prefix {}", path.display())
        }
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
         EnvironmentFile=-/etc/tuishou/auto-update.env\n\
         Environment=PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n\
         ExecStart={exe} --config {} auto-update run --once{}\n",
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
        let _guard = crate::test_env_lock();
        let (service, timer) =
            timer_units(Some("10m"), Some(Path::new("relative.toml")), None, "10m").unwrap();
        assert!(timer.contains("OnBootSec=10m"));
        assert!(timer.contains("OnActiveSec=10m"));
        assert!(service.contains("--config /"));
        assert!(!service.contains("--config relative.toml"));
        assert!(!service.contains("--prefix"));
    }

    #[test]
    fn cycle_names_puts_reiki_before_lu_even_when_config_is_reversed() {
        let cfg = AutoUpdateConfig {
            components: vec!["tuishou".to_string(), "lu".to_string(), "reiki".to_string()],
            ..AutoUpdateConfig::default()
        };
        assert_eq!(
            cycle_names(&cfg),
            vec!["reiki".to_string(), "lu".to_string(), "tuishou".to_string()]
        );
    }

    #[test]
    fn generated_timer_pins_path_and_rejects_unsafe_values() {
        let _guard = crate::test_env_lock();
        let (service, _) = timer_units(None, Some(Path::new("tuishou.toml")), None, "15m").unwrap();
        assert!(
            service.contains("Environment=PATH=/usr/local/sbin:/usr/local/bin"),
            "{service}"
        );
        assert!(
            service.find("EnvironmentFile=").unwrap() < service.find("Environment=PATH=").unwrap(),
            "PATH pin must follow the EnvironmentFile: {service}"
        );

        let err = timer_units(None, Some(Path::new("bad%i.toml")), None, "15m").unwrap_err();
        assert!(err.to_string().contains("systemd unit"), "{err}");

        let err = timer_units(
            None,
            Some(Path::new("tuishou.toml")),
            Some(Path::new("bad prefix")),
            "15m",
        )
        .unwrap_err();
        assert!(err.to_string().contains("systemd unit"), "{err}");
    }

    #[test]
    fn timer_executable_must_be_trusted_and_absolute() {
        let _guard = crate::test_env_lock();
        std::env::set_var("TUISHOU_TIMER_EXE", "/tmp/attacker-bin");
        let err = timer_units(None, Some(Path::new("tuishou.toml")), None, "15m").unwrap_err();
        assert!(err.to_string().contains("must be under"), "{err}");
        std::env::set_var("TUISHOU_TIMER_EXE", "tuishou");
        let err = timer_units(None, Some(Path::new("tuishou.toml")), None, "15m").unwrap_err();
        assert!(err.to_string().contains("absolute path"), "{err}");
        std::env::remove_var("TUISHOU_TIMER_EXE");
    }

    #[test]
    fn timer_executable_rejects_parent_dirs_and_canonicalizes_trusted_files() {
        let _guard = crate::test_env_lock();
        std::env::set_var("TUISHOU_TIMER_EXE", "/usr/local/bin/../../../tmp/evil");
        let err = timer_executable().unwrap_err().to_string();
        assert!(err.contains("systemd unit"), "{err}");

        std::env::set_var("TUISHOU_TIMER_EXE", "/bin/true");
        let exe = timer_executable().unwrap();
        let canonical = std::fs::canonicalize("/bin/true").unwrap();
        assert_eq!(Path::new(&exe), canonical.as_path());

        std::env::set_var("TUISHOU_TIMER_EXE", "/usr/local/bin/missing-tuishou");
        let err = timer_executable().unwrap_err().to_string();
        assert!(err.contains("existing regular file"), "{err}");
        std::env::remove_var("TUISHOU_TIMER_EXE");
    }

    #[cfg(unix)]
    #[test]
    fn environment_file_must_be_root_owned_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("auto-update.env");
        std::fs::write(&path, "").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).unwrap();
        let err = validate_environment_file_at(&path).unwrap_err().to_string();
        assert!(err.contains("writable by group or others"), "{err}");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let err = validate_environment_file_at(&path).unwrap_err().to_string();
        assert!(err.contains("owned by root"), "{err}");
    }

    #[test]
    fn generated_timer_carries_an_absolute_prefix() {
        let _guard = crate::test_env_lock();
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
