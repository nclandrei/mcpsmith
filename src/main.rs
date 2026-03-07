mod commands;
mod config;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

const LONG_ABOUT: &str = "\
Convert MCP servers into standalone skill packs with an atomic runtime gate.

Default one-shot flow:
  mcpsmith <server>

Stepwise flow:
  mcpsmith discover <server|--all> --out dossier.json
  mcpsmith build --from-dossier dossier.json
  mcpsmith contract-test --from-dossier dossier.json
  mcpsmith apply --from-dossier dossier.json --yes

Backend selection is backend-agnostic:
  1) explicit --backend if provided
  2) config convert.backend_preference when available
  3) auto-detect installed backend (codex, then claude)

Use --backend-health to print backend diagnostics.
Use --allow-side-effects, --probe-timeout-seconds, and --probe-retries to control runtime probes.
Use --config <path> to include extra MCP config files.
";

#[derive(Parser)]
#[command(name = "mcpsmith", version, about = "Convert MCP servers into skill packs", long_about = LONG_ABOUT)]
struct Cli {
    /// Server id (source:name) or unique server name for one-shot conversion
    server: Option<String>,
    /// Emit machine-readable JSON output
    #[arg(long)]
    json: bool,
    /// Additional MCP config file paths to inspect
    #[arg(long = "config", value_name = "PATH")]
    config: Vec<PathBuf>,
    /// Override output directory for generated skills
    #[arg(long = "skills-dir", value_name = "PATH")]
    skills_dir: Option<PathBuf>,
    /// Force backend selection to codex or claude
    #[arg(long, value_parser = ["codex", "claude"])]
    backend: Option<String>,
    /// Enable backend auto-detect/fallback mode
    #[arg(long)]
    backend_auto: bool,
    /// Print backend availability diagnostics
    #[arg(long)]
    backend_health: bool,
    /// Allow executing explicit side-effectful probes during contract testing
    #[arg(long, global = true)]
    allow_side_effects: bool,
    /// Runtime probe timeout in seconds
    #[arg(long = "probe-timeout-seconds", value_name = "N", global = true)]
    probe_timeout_seconds: Option<u64>,
    /// Number of retries for failed runtime probes
    #[arg(long = "probe-retries", value_name = "N", global = true)]
    probe_retries: Option<u32>,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Discover runtime tools and generate backend-neutral dossiers
    Discover {
        /// Server id (source:name) or unique server name
        server: Option<String>,
        /// Discover all servers from config sources
        #[arg(long, conflicts_with = "server")]
        all: bool,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
        /// Optional path to write dossier JSON
        #[arg(long = "out", value_name = "PATH")]
        out: Option<PathBuf>,
        /// Additional MCP config file paths to inspect
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
    },
    /// Build skill files from an existing dossier JSON
    Build {
        /// Input dossier JSON path
        #[arg(long = "from-dossier", value_name = "PATH")]
        from_dossier: PathBuf,
        /// Override output directory for generated skills
        #[arg(long = "skills-dir", value_name = "PATH")]
        skills_dir: Option<PathBuf>,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
    /// Run contract tests from an existing dossier JSON
    ContractTest {
        /// Input dossier JSON path
        #[arg(long = "from-dossier", value_name = "PATH")]
        from_dossier: PathBuf,
        /// Optional path to write contract-test report JSON
        #[arg(long = "report", value_name = "PATH")]
        report: Option<PathBuf>,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
    /// Apply a fully passing dossier: write skills and remove MCP config entry
    Apply {
        /// Input dossier JSON path
        #[arg(long = "from-dossier", value_name = "PATH")]
        from_dossier: PathBuf,
        /// Required confirmation because this mutates MCP config
        #[arg(long)]
        yes: bool,
        /// Override output directory for generated skills
        #[arg(long = "skills-dir", value_name = "PATH")]
        skills_dir: Option<PathBuf>,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
    /// List discovered MCP servers
    List {
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
        /// Additional MCP config file paths to inspect
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
    },
    /// Inspect one MCP server by id or by unique name
    Inspect {
        /// Server id (source:name) or unique server name
        server: String,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
        /// Additional MCP config file paths to inspect
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
    },
    /// Generate a conversion plan for one MCP server
    Plan {
        /// Server id (source:name) or unique server name
        server: String,
        /// Planning mode (auto resolves from recommendation)
        #[arg(long, default_value = "auto", value_parser = ["auto", "hybrid", "replace"])]
        mode: String,
        /// Explicit no-op apply guard (planning only)
        #[arg(long)]
        dry_run: bool,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
        /// Additional MCP config file paths to inspect
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
    },
    /// Verify parity coverage between generated skills and live MCP tool list
    Verify {
        /// Server id (source:name) or unique server name
        server: String,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
        /// Additional MCP config file paths to inspect
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
        /// Override skills directory for generated files
        #[arg(long = "skills-dir", value_name = "PATH")]
        skills_dir: Option<PathBuf>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let app_config = config::Config::load().unwrap_or_default();

    match cli.command {
        Some(Commands::Discover {
            server,
            all,
            json,
            out,
            config,
        }) => {
            commands::convert::run_discover_v3(
                server.as_deref(),
                all,
                json,
                out,
                &config,
                cli.backend.as_deref(),
                cli.backend_auto,
                cli.backend_health,
                &app_config,
            )?;
        }
        Some(Commands::Build {
            from_dossier,
            skills_dir,
            json,
        }) => {
            commands::convert::run_build_v3(
                &from_dossier,
                skills_dir,
                json,
                cli.backend_health,
                &app_config,
            )?;
        }
        Some(Commands::ContractTest {
            from_dossier,
            report,
            json,
        }) => {
            commands::convert::run_contract_test_v3(
                &from_dossier,
                report.as_deref(),
                json,
                cli.backend_health,
                cli.allow_side_effects,
                cli.probe_timeout_seconds,
                cli.probe_retries,
                &app_config,
            )?;
        }
        Some(Commands::Apply {
            from_dossier,
            yes,
            skills_dir,
            json,
        }) => {
            commands::convert::run_apply_v3(
                &from_dossier,
                yes,
                skills_dir,
                json,
                cli.backend_health,
                cli.allow_side_effects,
                cli.probe_timeout_seconds,
                cli.probe_retries,
                &app_config,
            )?;
        }
        Some(Commands::List { json, config }) => {
            commands::convert::run_list(json, &config)?;
        }
        Some(Commands::Inspect {
            server,
            json,
            config,
        }) => {
            commands::convert::run_inspect(&server, json, &config)?;
        }
        Some(Commands::Plan {
            server,
            mode,
            dry_run,
            json,
            config,
        }) => {
            commands::convert::run_plan(&server, &mode, dry_run, json, &config)?;
        }
        Some(Commands::Verify {
            server,
            json,
            config,
            skills_dir,
        }) => {
            commands::convert::run_verify(&server, json, &config, skills_dir)?;
        }
        None => {
            if let Some(server) = cli.server {
                commands::convert::run_one_shot_v3(
                    &server,
                    cli.json,
                    &cli.config,
                    cli.skills_dir,
                    cli.backend.as_deref(),
                    cli.backend_auto,
                    cli.backend_health,
                    cli.allow_side_effects,
                    cli.probe_timeout_seconds,
                    cli.probe_retries,
                    &app_config,
                )?;
            } else {
                commands::convert::run_overview_v3(&cli.config)?;
            }
        }
    }

    Ok(())
}
