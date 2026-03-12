mod commands;
mod config;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

const LONG_ABOUT: &str = "\
Convert MCP servers into source-grounded, agent-native skills.

Primary one-shot flow:
  mcpsmith <server>
  mcpsmith run <server>

Staged flow:
  mcpsmith catalog sync
  mcpsmith resolve <server>
  mcpsmith snapshot <server> | --from-resolve <path>
  mcpsmith evidence <server> | --from-snapshot <path>
  mcpsmith synthesize <server>
  mcpsmith review <server>
  mcpsmith verify <server>

Every command is non-interactive. Use --json for machine-readable output and --dry-run to avoid mutating installed skills or MCP config.
";

#[derive(Parser)]
#[command(name = "mcpsmith", version, about = "Convert MCP servers into source-grounded skills", long_about = LONG_ABOUT)]
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
    #[arg(long, value_parser = ["codex", "claude"], requires = "server")]
    backend: Option<String>,
    /// Enable backend auto-detect/fallback mode
    #[arg(long, requires = "server")]
    backend_auto: bool,
    /// Run the full pipeline without mutating installed skills or MCP config
    #[arg(long, requires = "server")]
    dry_run: bool,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Sync and normalize public MCP catalog providers
    Catalog {
        #[command(subcommand)]
        command: CatalogCommands,
    },
    /// Resolve the exact source artifact for one installed MCP
    Resolve {
        server: String,
        #[arg(long)]
        json: bool,
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
    },
    /// Materialize a local source snapshot for one installed MCP
    Snapshot {
        server: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long = "from-resolve", value_name = "PATH")]
        from_resolve: Option<PathBuf>,
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
    },
    /// Build a per-tool evidence bundle from runtime tools plus source snapshot
    Evidence {
        server: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long = "tool", value_name = "NAME")]
        tool: Option<String>,
        #[arg(long = "from-snapshot", value_name = "PATH")]
        from_snapshot: Option<PathBuf>,
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
    },
    /// Synthesize grounded skill drafts from evidence
    Synthesize {
        server: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long = "tool", value_name = "NAME")]
        tool: Option<String>,
        #[arg(long = "from-evidence", value_name = "PATH")]
        from_evidence: Option<PathBuf>,
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
        #[arg(long, value_parser = ["codex", "claude"])]
        backend: Option<String>,
        #[arg(long)]
        backend_auto: bool,
    },
    /// Review synthesized skills with a second agent pass
    Review {
        server: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long = "from-bundle", value_name = "PATH")]
        from_bundle: Option<PathBuf>,
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
        #[arg(long, value_parser = ["codex", "claude"])]
        backend: Option<String>,
        #[arg(long)]
        backend_auto: bool,
    },
    /// Verify generated skills for format, grounding, and references
    Verify {
        server: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long = "from-bundle", value_name = "PATH")]
        from_bundle: Option<PathBuf>,
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
        #[arg(long, value_parser = ["codex", "claude"])]
        backend: Option<String>,
        #[arg(long)]
        backend_auto: bool,
    },
    /// Run the full source-grounded pipeline end-to-end
    Run {
        server: String,
        #[arg(long)]
        json: bool,
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
        #[arg(long = "skills-dir", value_name = "PATH")]
        skills_dir: Option<PathBuf>,
        #[arg(long, value_parser = ["codex", "claude"])]
        backend: Option<String>,
        #[arg(long)]
        backend_auto: bool,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum CatalogCommands {
    /// Fetch provider data and write a normalized catalog snapshot
    Sync {
        #[arg(long)]
        json: bool,
        #[arg(long = "provider", value_name = "NAME")]
        provider: Vec<String>,
    },
    /// Report catalog statistics from a saved snapshot or a fresh sync
    Stats {
        #[arg(long)]
        json: bool,
        #[arg(long = "from", value_name = "PATH")]
        from: Option<PathBuf>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let app_config = config::Config::load().unwrap_or_default();

    match cli.command {
        Some(Commands::Catalog { command }) => match command {
            CatalogCommands::Sync { json, provider } => {
                commands::agentic::run_catalog_sync_cmd(json, &provider)?;
            }
            CatalogCommands::Stats { json, from } => {
                commands::agentic::run_catalog_stats_cmd(json, from.as_deref())?;
            }
        },
        Some(Commands::Resolve {
            server,
            json,
            config,
        }) => {
            commands::agentic::run_resolve_cmd(&server, json, &config)?;
        }
        Some(Commands::Snapshot {
            server,
            json,
            from_resolve,
            config,
        }) => {
            commands::agentic::run_snapshot_cmd(
                server.as_deref(),
                from_resolve.as_deref(),
                json,
                &config,
            )?;
        }
        Some(Commands::Evidence {
            server,
            json,
            tool,
            from_snapshot,
            config,
        }) => {
            commands::agentic::run_evidence_cmd(
                server.as_deref(),
                from_snapshot.as_deref(),
                tool.as_deref(),
                json,
                &config,
            )?;
        }
        Some(Commands::Synthesize {
            server,
            json,
            tool,
            from_evidence,
            config,
            backend,
            backend_auto,
        }) => {
            commands::agentic::run_synthesize_cmd(
                server.as_deref(),
                from_evidence.as_deref(),
                tool.as_deref(),
                json,
                &config,
                backend.as_deref(),
                backend_auto,
                &app_config,
            )?;
        }
        Some(Commands::Review {
            server,
            json,
            from_bundle,
            config,
            backend,
            backend_auto,
        }) => {
            commands::agentic::run_review_cmd(
                server.as_deref(),
                from_bundle.as_deref(),
                json,
                &config,
                backend.as_deref(),
                backend_auto,
                &app_config,
            )?;
        }
        Some(Commands::Verify {
            server,
            json,
            from_bundle,
            config,
            backend,
            backend_auto,
        }) => {
            commands::agentic::run_verify_cmd(
                server.as_deref(),
                from_bundle.as_deref(),
                json,
                &config,
                backend.as_deref(),
                backend_auto,
                &app_config,
            )?;
        }
        Some(Commands::Run {
            server,
            json,
            config,
            skills_dir,
            backend,
            backend_auto,
            dry_run,
        }) => {
            commands::agentic::run_run_cmd(
                &server,
                json,
                &config,
                skills_dir,
                backend.as_deref(),
                backend_auto,
                dry_run,
                &app_config,
            )?;
        }
        None => {
            if let Some(server) = cli.server {
                commands::agentic::run_run_cmd(
                    &server,
                    cli.json,
                    &cli.config,
                    cli.skills_dir,
                    cli.backend.as_deref(),
                    cli.backend_auto,
                    cli.dry_run,
                    &app_config,
                )?;
            } else {
                commands::agentic::run_overview(cli.json)?;
            }
        }
    }

    Ok(())
}
