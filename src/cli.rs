use crate::auto_update::{self, apply};
use crate::completions;
use crate::component::{resolve_component, Component};
use crate::config::{self, LoadedConfig};
use crate::docs_static;
use crate::error::{Error, Result, EXIT_UPDATES_AVAILABLE};
use crate::fleet;
use crate::http;
use crate::install::{self, InstallOptions};
use crate::release;
use crate::report::{self, ComponentReport, StatusText};
use crate::rollback;
use crate::self_update;
use crate::state;
use crate::status;
use crate::verify::{self, AttestationRequest, AttestationVerifier};
use clap::{Parser, Subcommand, ValueEnum};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(
    name = "tuishou",
    version,
    about = "Install and update the Tuishou stack from GitHub releases"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Output JSON where supported
    #[arg(long, global = true)]
    pub json: bool,

    /// Suppress non-essential output
    #[arg(long, global = true)]
    pub quiet: bool,

    /// Verbose output
    #[arg(long, global = true)]
    pub verbose: bool,

    /// Path to tuishou.toml (defaults to ./tuishou.toml; env TUISHOU_CONFIG)
    #[arg(long, global = true, env = "TUISHOU_CONFIG")]
    pub config: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Install a component from GitHub releases
    Install {
        /// Component name or "all"
        component: String,
        /// Release version tag (e.g. v0.4.0)
        #[arg(long)]
        version: Option<String>,
        /// Install prefix directory
        #[arg(long)]
        prefix: Option<PathBuf>,
        /// Show what would be done without making changes
        #[arg(long)]
        dry_run: bool,
        /// Warn and continue when attestation verification fails
        #[arg(long)]
        allow_unattested: bool,
        /// Allow crossing a major version boundary
        #[arg(long)]
        allow_major: bool,
        /// Run <binary> --version after install (the only release code executed)
        #[arg(long)]
        smoke_check: bool,
    },
    /// Update installed components
    Update {
        /// Component name or "all" (default: all installed)
        component: Option<String>,
        /// Only report whether updates exist (exit 10 when updates exist)
        #[arg(long)]
        check: bool,
        /// Allow crossing a major version boundary
        #[arg(long)]
        allow_major: bool,
        /// Warn and continue when attestation verification fails
        #[arg(long)]
        allow_unattested: bool,
        /// Install prefix directory
        #[arg(long)]
        prefix: Option<PathBuf>,
    },
    /// Show installed and latest versions
    Status {
        /// Install prefix directory
        #[arg(long)]
        prefix: Option<PathBuf>,
    },
    /// Restore the previous binary for a component
    Rollback {
        /// Component name
        component: String,
        /// Install prefix directory
        #[arg(long)]
        prefix: Option<PathBuf>,
    },
    /// Update the tuishou binary itself from its own release archive
    SelfUpdate {
        /// Release version tag (e.g. v0.2.0)
        #[arg(long)]
        version: Option<String>,
        /// Directory to install into (default: directory of the running binary)
        #[arg(long)]
        prefix: Option<PathBuf>,
        /// Warn and continue when attestation verification fails
        #[arg(long)]
        allow_unattested: bool,
        /// Allow crossing a major version boundary
        #[arg(long)]
        allow_major: bool,
        /// Show what would be done without making changes
        #[arg(long)]
        dry_run: bool,
    },
    /// Print a shell completion script
    Completions {
        /// Shell to generate for
        #[arg(value_enum)]
        shell: CompletionShell,
    },
    /// Print the tuishou man page
    Man,
    /// Verify a local artifact against its GitHub attestation bundle
    Verify {
        /// Artifact to verify
        file: PathBuf,
        /// GitHub repository in owner/name form
        #[arg(long)]
        repo: String,
        /// Override the expected signer workflow path
        #[arg(long)]
        signer_workflow: Option<String>,
        /// Expected release tag (otherwise read from the certificate SAN)
        #[arg(long)]
        tag: Option<String>,
    },
    #[command(
        hide = true,
        name = "__complete",
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    Complete {
        /// Words typed so far, with the current word last
        words: Vec<String>,
    },
    /// Opt-in scheduled auto-update (off by default, not recommended for production)
    #[command(subcommand)]
    AutoUpdate(AutoUpdateAction),

    /// Record an existing binary in state without replacing it
    Adopt {
        /// Component name
        component: String,
        /// Release version tag the existing binary represents
        #[arg(long)]
        version: String,
        /// Install prefix directory
        #[arg(long)]
        prefix: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum AutoUpdateAction {
    /// Run one update cycle (no daemon; the timer or cron schedules cycles)
    Run {
        /// Explicit one-shot marker; accepted for systemd unit clarity
        #[arg(long)]
        once: bool,
        /// Always refused: attestations are mandatory in auto mode
        #[arg(long)]
        allow_unattested: bool,
        /// Install prefix (overrides [auto_update].prefix)
        #[arg(long)]
        prefix: Option<PathBuf>,
    },
    /// Print, but do not install, a systemd service and timer unit pair
    InstallTimer {
        /// OnBootSec/OnActiveSec value (default: configured interval, minimum 5m)
        #[arg(long)]
        interval: Option<String>,
        /// Install prefix passed to the generated service (overrides config)
        #[arg(long)]
        prefix: Option<PathBuf>,
    },
    /// Show last check, per-component results, hold state and next version
    Status {
        /// Install prefix (overrides [auto_update].prefix)
        #[arg(long)]
        prefix: Option<PathBuf>,
    },
    /// Pause all automatic updates by creating the HOLD file
    Hold {
        /// Why updates are paused
        #[arg(long)]
        reason: Option<String>,
        /// RFC3339 time at which the hold expires
        #[arg(long)]
        expires_at: Option<String>,
        /// Duration after which the hold expires (e.g. 2h)
        #[arg(long)]
        expires_in: Option<String>,
        /// Install prefix (overrides [auto_update].prefix)
        #[arg(long)]
        prefix: Option<PathBuf>,
    },
    /// Remove the HOLD file and resume automatic updates
    Release {
        /// Install prefix (overrides [auto_update].prefix)
        #[arg(long)]
        prefix: Option<PathBuf>,
    },
    /// Forget bad versions so they can be retried
    ClearBad {
        /// Component to clear (default: all)
        component: Option<String>,
        /// Install prefix (overrides [auto_update].prefix)
        #[arg(long)]
        prefix: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
}

impl CompletionShell {
    fn as_str(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
        }
    }
}

/// CLI `--prefix` wins over the configured value so one config can serve
/// multiple roots without editing it.
fn auto_update_prefix(
    loaded: &crate::config::LoadedConfig,
    flag: Option<PathBuf>,
) -> Option<PathBuf> {
    flag.or_else(|| loaded.auto_update.prefix.clone())
}

fn known_component_names(components: &[Component]) -> String {
    components
        .iter()
        .map(|c| c.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `None` means "all"; "all" is explicit and equivalent. Unknown names are
/// usage errors (exit 2) that list the built-in names.
fn resolve_targets(name: Option<&str>, components: &[Component]) -> Result<Vec<Component>> {
    match name {
        None | Some("all") => Ok(components.to_vec()),
        Some(wanted) => components
            .iter()
            .find(|c| c.name == wanted)
            .cloned()
            .map(|c| vec![c])
            .ok_or_else(|| {
                Error::Usage(format!(
                    "unknown component '{wanted}'; known components: {} (or 'all')",
                    known_component_names(components)
                ))
            }),
    }
}

type EvaluationContext = (Option<String>, BTreeMap<String, Vec<String>>);

fn evaluation_context(loaded: &LoadedConfig, prefix: Option<&Path>) -> Result<EvaluationContext> {
    let auto_prefix = auto_update_prefix(loaded, prefix.map(Path::to_path_buf));
    let bad = auto_update::state::load(auto_prefix.as_deref())?.bad_versions;
    let hold = auto_update::state::active_hold(auto_prefix.as_deref())?
        .map(|h| h.reason.unwrap_or_else(|| "hold file present".to_string()));
    Ok((hold, bad))
}

#[allow(clippy::too_many_arguments)]
fn base_install_options(
    loaded: &LoadedConfig,
    verbose: bool,
    version: Option<String>,
    prefix: Option<PathBuf>,
    allow_unattested: bool,
    allow_major: bool,
    smoke_check: bool,
    commit_state: bool,
) -> InstallOptions {
    InstallOptions {
        version,
        prefix,
        // All per-component reporting uses the shared vocabulary, so the
        // installer itself must not print an extra line.
        dry_run: false,
        allow_unattested,
        allow_major,
        max_download_bytes: http::max_download_bytes(),
        quiet: true,
        verbose,
        smoke_check,
        staged_check: None,
        verifier: verify::default_verifier(),
        verify_config: loaded.verify.clone(),
        commit_state,
    }
}

fn perform_install(
    agent: &ureq::Agent,
    component: &Component,
    tag: &str,
    opts: &InstallOptions,
    loaded: &LoadedConfig,
    prefix: Option<&Path>,
) -> Result<()> {
    if !opts.dry_run && (component.name == "reiki" || component.name == "lu") {
        let mut auto = auto_update::state::load(prefix)?;
        let mut local_opts = opts.clone();
        // The local apply paths record state only after config/health pass.
        local_opts.commit_state = false;
        local_opts.quiet = true;
        let ctx = apply::ApplyContext {
            cfg: &loaded.auto_update,
            prefix,
            config_path: Some(loaded.config_path.as_path()),
            verifier: Some(opts.verifier.clone()),
            verify_config: Some(&opts.verify_config),
        };
        let applied = if component.name == "reiki" {
            apply::apply_reiki_with_options(
                component,
                tag,
                &ctx,
                &mut auto,
                &apply::SystemRunner,
                local_opts,
            )?;
            true
        } else {
            // On a Lu node the agent follows the same drain/restart/health
            // procedure as auto-update, with Reiki applied first in a normal
            // `update all` run. Elsewhere a plain install is sufficient.
            apply::apply_agent_on_lu_node(component, tag, &ctx, &mut auto)?
        };
        if applied {
            return Ok(());
        }
    }
    install::install_component(agent, component, opts)?;
    Ok(())
}

fn updated_report(component: &Component, previous: Option<&str>, tag: &str) -> ComponentReport {
    let status = match previous {
        Some(old) if old == tag => StatusText::UpToDate {
            version: tag.to_string(),
        },
        Some(old) => StatusText::Updated {
            from: old.to_string(),
            to: tag.to_string(),
        },
        None => StatusText::Updated {
            from: "-".to_string(),
            to: tag.to_string(),
        },
    };
    ComponentReport::from_parts(
        component,
        status,
        Some(tag.to_string()),
        Some(tag.to_string()),
        Vec::new(),
    )
}

fn failed_report(
    component: &Component,
    err: &Error,
    previous: Option<String>,
    latest: Option<String>,
) -> ComponentReport {
    ComponentReport::from_parts(
        component,
        StatusText::Failed {
            reason: crate::logging::one_line(&err.to_string()),
        },
        previous,
        latest,
        Vec::new(),
    )
}

pub fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    // NO_COLOR respected - we emit no ANSI color; keep placeholder for future.
    let _ = std::env::var_os("NO_COLOR");

    // Completion must work even if the working directory has a broken config.
    if let Commands::Complete { words } = &cli.command {
        println!("{}", completions::candidates(words));
        return Ok(ExitCode::SUCCESS);
    }

    let config_path = cli.config.as_deref();
    let loaded = config::load_config(config_path)?;
    let components = loaded.components.clone();
    let agent = http::agent();

    match cli.command {
        Commands::Install {
            component,
            version,
            prefix,
            dry_run,
            allow_unattested,
            allow_major,
            smoke_check,
        } => {
            let targets = resolve_targets(Some(&component), &components)?;
            let before = report::installed_versions(prefix.as_deref())?;
            let mut reports = Vec::new();
            for c in &targets {
                let tag = match &version {
                    Some(v) => release::fetch_tagged(&agent, &c.repo, v)?.tag,
                    None => release::fetch_latest(&agent, &c.repo)?.tag,
                };
                let missing = match report::archive_missing_binaries(&agent, c, &tag) {
                    Ok(m) => m,
                    Err(e) => {
                        reports.push(failed_report(c, &e, None, Some(tag)));
                        continue;
                    }
                };
                if !missing.is_empty() {
                    reports.push(ComponentReport::from_parts(
                        c,
                        StatusText::NotInRelease {
                            missing: missing.clone(),
                        },
                        None,
                        Some(tag),
                        missing,
                    ));
                    continue;
                }
                let previous = before.get(&c.name).map(|i| i.version.clone());
                let mut opts = base_install_options(
                    &loaded,
                    cli.verbose,
                    Some(tag.clone()),
                    prefix.clone(),
                    allow_unattested,
                    allow_major,
                    smoke_check,
                    true,
                );
                opts.dry_run = dry_run;
                match perform_install(&agent, c, &tag, &opts, &loaded, prefix.as_deref()) {
                    Ok(()) => reports.push(updated_report(c, previous.as_deref(), &tag)),
                    Err(e) => reports.push(failed_report(c, &e, previous, Some(tag))),
                }
            }
            report::print_reports(&reports, cli.json, cli.quiet)?;
            Ok(if report::has_failed(&reports) {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }
        Commands::Update {
            component,
            check,
            allow_major,
            allow_unattested,
            prefix,
        } => {
            let targets = resolve_targets(component.as_deref(), &components)?;
            let explicit = component.is_some();
            let installed = report::installed_versions(prefix.as_deref())?;
            let (hold, bad) = evaluation_context(&loaded, prefix.as_deref())?;
            let evaluated = report::evaluate_all(
                &agent,
                &targets,
                prefix.as_deref(),
                &installed,
                hold.as_deref(),
                &bad,
            );
            if check {
                report::print_reports(&evaluated, cli.json, cli.quiet)?;
                return Ok(if report::has_failed(&evaluated) {
                    ExitCode::FAILURE
                } else if report::has_installable_update(&evaluated) {
                    ExitCode::from(EXIT_UPDATES_AVAILABLE)
                } else {
                    ExitCode::SUCCESS
                });
            }

            let command_timeout =
                crate::duration::parse_duration(&loaded.auto_update.command_timeout)
                    .unwrap_or(std::time::Duration::from_secs(30));
            if let Some(reason) = fleet::before_cycle(&loaded.update, command_timeout) {
                let skipped: Vec<ComponentReport> = targets
                    .iter()
                    .map(|c| {
                        ComponentReport::new(
                            c,
                            StatusText::Skipped {
                                reason: reason.clone(),
                            },
                        )
                    })
                    .collect();
                report::print_reports(&skipped, cli.json, cli.quiet)?;
                return Ok(ExitCode::SUCCESS);
            }

            let mut reports = Vec::new();
            for (c, current) in targets.iter().zip(evaluated) {
                let previous = current.installed_version.clone();
                let latest = current.latest_version.clone();
                match current.status_text() {
                    StatusText::UpdateAvailable { .. } | StatusText::NotInstalled { .. } => {
                        // An explicit `update <name>` installs a component that
                        // is not installed yet; a bare `update` does not.
                        if matches!(current.status_text(), StatusText::NotInstalled { .. })
                            && !explicit
                        {
                            reports.push(current);
                            continue;
                        }
                        let Some(tag) = latest.clone() else {
                            reports.push(current);
                            continue;
                        };
                        let opts = base_install_options(
                            &loaded,
                            cli.verbose,
                            Some(tag.clone()),
                            prefix.clone(),
                            allow_unattested,
                            allow_major,
                            false,
                            true,
                        );
                        match perform_install(&agent, c, &tag, &opts, &loaded, prefix.as_deref()) {
                            Ok(()) => reports.push(updated_report(c, previous.as_deref(), &tag)),
                            Err(e) => reports.push(failed_report(c, &e, previous, latest)),
                        }
                    }
                    _ => reports.push(current),
                }
            }
            report::print_reports(&reports, cli.json, cli.quiet)?;
            Ok(if report::has_failed(&reports) {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }
        Commands::Status { prefix } => {
            let (hold, bad) = evaluation_context(&loaded, prefix.as_deref())?;
            let entries = status::collect_status(
                &agent,
                &components,
                prefix.as_deref(),
                hold.as_deref(),
                &bad,
            )?;
            status::print_status(&entries, cli.json, cli.quiet, prefix.as_deref())?;
            Ok(ExitCode::SUCCESS)
        }
        Commands::Rollback { component, prefix } => {
            let c = resolve_component(&component, &components)?;
            rollback::rollback_binaries(&c.name, &c.binaries, prefix.as_deref(), cli.quiet)?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({"component": c.name, "rolled_back": true})
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Commands::SelfUpdate {
            version,
            prefix,
            allow_unattested,
            allow_major,
            dry_run,
        } => {
            let component = self_update::self_component(&components);
            let bin_dir = self_update::binary_dir(prefix.as_deref())?;
            let opts = InstallOptions {
                version,
                prefix: Some(bin_dir),
                dry_run,
                allow_unattested,
                allow_major,
                max_download_bytes: http::max_download_bytes(),
                quiet: cli.quiet,
                verbose: cli.verbose,
                smoke_check: true,
                staged_check: None,
                verifier: verify::default_verifier(),
                verify_config: loaded.verify.clone(),
                commit_state: true,
            };
            let outcome = install::install_component(&agent, &component, &opts)?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "component": outcome.component,
                        "version": outcome.version,
                        "binaries": outcome.binaries,
                        "dry_run": outcome.dry_run,
                        "self": true,
                    })
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Commands::Completions { shell } => {
            let file = format!("tuishou.{}", shell.as_str());
            let body = docs_static::DIST_FILES
                .iter()
                .find(|(name, _)| *name == file)
                .map(|(_, body)| *body)
                .ok_or_else(|| {
                    Error::msg(format!("no completion script for {}", shell.as_str()))
                })?;
            print!("{body}");
            Ok(ExitCode::SUCCESS)
        }
        Commands::Man => {
            print!("{}", docs_static::MAN_PAGE);
            Ok(ExitCode::SUCCESS)
        }
        Commands::Verify {
            file,
            repo,
            signer_workflow,
            tag,
        } => {
            verify::github::validate_repo(&repo)
                .map_err(|e| Error::Usage(format!("invalid --repo: {e}")))?;
            let workflow = signer_workflow.unwrap_or_else(|| loaded.verify.signer_workflow.clone());
            verify::validate_signer_workflow(&workflow, "--signer-workflow")
                .map_err(|e| Error::Usage(e.to_string()))?;
            if let Some(value) = &tag {
                verify::validate_tag(value, "--tag").map_err(|e| Error::Usage(e.to_string()))?;
            }
            let mut verify_config = loaded.verify.clone();
            verify_config.signer_workflow = workflow.clone();
            let request = AttestationRequest {
                path: file,
                repo,
                signer_workflow: workflow,
                tag,
                source_digest: None,
                trust_root: verify_config.trust_root.clone(),
                cache_dir: verify_config.cache_dir(None),
                cache_ttl: verify_config.cache_ttl()?,
            };
            let verifier: Arc<dyn AttestationVerifier> = verify::default_verifier();
            let report = verifier.verify(&request)?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "digest": report.digest,
                        "issuer": report.issuer,
                        "identity": report.identity,
                        "workflow": report.workflow,
                        "ref": report.ref_value,
                        "log_index": report.log_index,
                        "predicate_type": report.predicate_type,
                    }))
                    .map_err(|e| Error::msg(e.to_string()))?
                );
            } else {
                println!("digest: {}", report.digest);
                println!("issuer: {}", report.issuer);
                println!("identity: {}", report.identity);
                println!("workflow: {}", report.workflow);
                println!("ref: {}", report.ref_value);
                println!("log index: {}", report.log_index);
            }
            Ok(ExitCode::SUCCESS)
        }
        Commands::Complete { .. } => unreachable!("handled before config load"),
        Commands::Adopt {
            component,
            version,
            prefix,
        } => {
            let c = resolve_component(&component, &components)?;
            let tag = release::normalize_tag(&version);
            let prefix = auto_update_prefix(&loaded, prefix);
            let bin_dir = install::install_dir(prefix.as_deref());
            let present: Vec<String> = c
                .binaries
                .iter()
                .filter(|name| bin_dir.join(name).is_file())
                .cloned()
                .collect();
            if present.is_empty() {
                return Err(Error::msg(format!(
                    "adopt {}: none of the component binaries ({}) exist in {}; nothing to record",
                    c.name,
                    c.binaries.join(", "),
                    bin_dir.display()
                )));
            }
            let checksum = crate::verify::sha256_file(&bin_dir.join(&present[0]))?;
            let state_path = state::default_state_path(prefix.as_deref());
            let mut st = state::load(&state_path)?;
            let previous_version = st.components.get(&c.name).map(|e| e.version.clone());
            st.components.insert(
                c.name.clone(),
                state::InstalledComponent {
                    version: tag.clone(),
                    target: crate::target::Target::detect()?.as_str().to_string(),
                    checksum,
                    installed_at: state::now_rfc3339()?,
                    previous_version,
                },
            );
            state::save(&state_path, &st)?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "component": c.name,
                        "version": tag,
                        "binaries": present,
                        "adopted": true,
                    })
                );
            } else if !cli.quiet {
                println!("adopted {} {} ({})", c.name, tag, present.join(", "));
            }
            Ok(ExitCode::SUCCESS)
        }
        Commands::AutoUpdate(action) => match action {
            AutoUpdateAction::Run {
                once,
                allow_unattested,
                prefix,
            } => auto_update::handle_run(
                &loaded,
                auto_update_prefix(&loaded, prefix).as_deref(),
                once,
                cli.quiet,
                allow_unattested,
                cli.json,
            ),
            AutoUpdateAction::InstallTimer { interval, prefix } => {
                let prefix = auto_update_prefix(&loaded, prefix);
                auto_update::print_timer(
                    interval.as_deref(),
                    config_path,
                    prefix.as_deref(),
                    &loaded.auto_update.interval,
                )?;
                Ok(ExitCode::SUCCESS)
            }
            AutoUpdateAction::Status { prefix } => auto_update::handle_status(
                &loaded,
                auto_update_prefix(&loaded, prefix).as_deref(),
                cli.json,
                cli.quiet,
            ),
            AutoUpdateAction::Hold {
                reason,
                expires_at,
                expires_in,
                prefix,
            } => {
                let prefix = auto_update_prefix(&loaded, prefix);
                auto_update::handle_hold(prefix.as_deref(), reason, expires_at, expires_in)?;
                Ok(ExitCode::SUCCESS)
            }
            AutoUpdateAction::Release { prefix } => {
                let prefix = auto_update_prefix(&loaded, prefix);
                auto_update::handle_release_hold(prefix.as_deref())?;
                Ok(ExitCode::SUCCESS)
            }
            AutoUpdateAction::ClearBad { component, prefix } => {
                let prefix = auto_update_prefix(&loaded, prefix);
                auto_update::handle_clear_bad(prefix.as_deref(), component.as_deref())?;
                Ok(ExitCode::SUCCESS)
            }
        },
    }
}
