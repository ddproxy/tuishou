use crate::auto_update;
use crate::completions;
use crate::component::{resolve_component, resolve_components};
use crate::config;
use crate::docs_static;
use crate::error::{Error, Result, EXIT_UPDATES_AVAILABLE};
use crate::http;
use crate::install::{self, InstallOptions};
use crate::release;
use crate::roll;
use crate::rollback;
use crate::self_update;
use crate::state;
use crate::status;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser, Debug)]
#[command(
    name = "tuishou",
    version,
    about = "Install, update and roll the Tuishou stack from GitHub releases"
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
        /// Allow install when gh attestation verification is unavailable
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
        /// Allow install when gh attestation verification is unavailable
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
        /// Allow install when gh attestation verification is unavailable
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

    /// Roll a component across a fleet over SSH
    Roll {
        /// Inventory file path or comma-separated node list
        #[arg(long)]
        nodes: String,
        /// Component to roll
        #[arg(long, default_value = "ting")]
        component: String,
        /// Release version tag
        #[arg(long)]
        version: Option<String>,
        /// Number of nodes to roll concurrently (currently serial batches; reserved)
        #[arg(long, default_value_t = 1)]
        batch: usize,
        /// Show the roll plan without making changes
        #[arg(long)]
        dry_run: bool,
        /// Continue after drain wait timeout by forcing the step
        #[arg(long)]
        force_after: Option<u64>,
        /// Allow crossing a major version boundary
        #[arg(long)]
        allow_major: bool,
        /// Allow install when gh attestation verification is unavailable
        #[arg(long)]
        allow_unattested: bool,
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
            let targets = resolve_components(&[component], &components)?;
            let opts = InstallOptions {
                version,
                prefix,
                dry_run,
                allow_unattested,
                allow_major,
                max_download_bytes: http::max_download_bytes(),
                quiet: cli.quiet,
                verbose: cli.verbose,
                smoke_check,
                signer_workflow: None,
                commit_state: true,
            };
            let mut outcomes = Vec::new();
            for c in &targets {
                outcomes.push(install::install_component(&agent, c, &opts)?);
            }
            if cli.json {
                let v: Vec<_> = outcomes
                    .iter()
                    .map(|o| {
                        serde_json::json!({
                            "component": o.component,
                            "version": o.version,
                            "binaries": o.binaries,
                            "dry_run": o.dry_run,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&v).map_err(|e| Error::msg(e.to_string()))?
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Commands::Update {
            component,
            check,
            allow_major,
            allow_unattested,
            prefix,
        } => {
            let st = crate::state::load(&crate::state::default_state_path(prefix.as_deref()))?;
            let targets: Vec<_> = if let Some(c) = component {
                resolve_components(&[c], &components)?
            } else {
                components
                    .iter()
                    .filter(|c| st.components.contains_key(&c.name))
                    .cloned()
                    .collect()
            };
            let mut updates = Vec::new();
            for c in &targets {
                let installed = st.components.get(&c.name);
                let latest = release::fetch_latest(&agent, &c.repo)?;
                let has_update = installed.map(|i| i.version != latest.tag).unwrap_or(true);
                if has_update {
                    if !cli.quiet {
                        let from = installed.map(|i| i.version.as_str()).unwrap_or("-");
                        println!("{}: {} -> {}", c.name, from, latest.tag);
                        if let Some(h) = release::changelog_heading(&latest.body, &latest.tag) {
                            println!("  {h}");
                        }
                    }
                    updates.push((c.clone(), latest));
                } else if !cli.quiet {
                    println!("{}: up to date ({})", c.name, latest.tag);
                }
            }
            if check {
                if !updates.is_empty() {
                    if cli.json {
                        let v: Vec<_> = updates
                            .iter()
                            .map(|(c, r)| serde_json::json!({"component": c.name, "latest": r.tag}))
                            .collect();
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&v)
                                .map_err(|e| Error::msg(e.to_string()))?
                        );
                    }
                    return Ok(ExitCode::from(EXIT_UPDATES_AVAILABLE));
                }
                if cli.json {
                    println!("[]");
                }
                return Ok(ExitCode::SUCCESS);
            }
            for (c, latest) in &updates {
                let opts = InstallOptions {
                    version: Some(latest.tag.clone()),
                    prefix: prefix.clone(),
                    dry_run: false,
                    allow_unattested,
                    allow_major,
                    max_download_bytes: http::max_download_bytes(),
                    quiet: cli.quiet,
                    verbose: cli.verbose,
                    smoke_check: false,
                    signer_workflow: None,
                    commit_state: true,
                };
                install::install_component(&agent, c, &opts)?;
            }
            if cli.json {
                let v: Vec<_> = updates
                    .iter()
                    .map(|(c, r)| serde_json::json!({"component": c.name, "version": r.tag, "updated": true}))
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&v).map_err(|e| Error::msg(e.to_string()))?
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Commands::Status { prefix } => {
            let entries = status::collect_status(&agent, &components, prefix.as_deref())?;
            status::print_status(&entries, cli.json, cli.quiet)?;
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
                smoke_check: false,
                signer_workflow: None,
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
        Commands::Roll {
            nodes,
            component,
            version,
            batch,
            dry_run,
            force_after,
            allow_major,
            allow_unattested,
        } => {
            let c = resolve_component(&component, &components)?;
            let plan = roll::RollPlan {
                nodes,
                component: c,
                version,
                batch,
                dry_run,
                force_after_secs: force_after,
                allow_major,
                allow_unattested,
                quiet: cli.quiet,
                verbose: cli.verbose,
                json: cli.json,
            };
            let runner = roll::SystemRollRunner;
            roll::execute_roll(&plan, &runner)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}
