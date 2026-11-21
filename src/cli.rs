use crate::component::{resolve_component, resolve_components};
use crate::config;
use crate::error::{Error, Result, EXIT_UPDATES_AVAILABLE};
use crate::http;
use crate::install::{self, InstallOptions};
use crate::release;
use crate::roll;
use crate::rollback;
use crate::status;
use clap::{Parser, Subcommand};
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

pub fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    // NO_COLOR respected - we emit no ANSI color; keep placeholder for future.
    let _ = std::env::var_os("NO_COLOR");

    let components = config::load_components(None)?;
    let agent = http::agent();

    match cli.command {
        Commands::Install {
            component,
            version,
            prefix,
            dry_run,
            allow_unattested,
            allow_major,
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
