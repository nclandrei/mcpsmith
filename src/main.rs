mod commands;
mod config;

use clap::{CommandFactory, Parser, Subcommand};
use std::path::PathBuf;

const LONG_ABOUT: &str = "\
Convert installed MCP servers into source-grounded, agent-native skills.

Inspect local MCP inventory:
  mcpsmith discover [--json]

One-shot conversion:
  mcpsmith <server> [--dry-run] [--json]
  mcpsmith run <server> [--dry-run] [--json]

Inspection and staged flow:
  mcpsmith catalog sync
  mcpsmith resolve <server>
  mcpsmith snapshot <server> | --from-resolve <path>
  mcpsmith evidence <server> | --from-snapshot <path>
  mcpsmith synthesize <server> | --from-evidence <path>
  mcpsmith review <server> | --from-bundle <path>
  mcpsmith verify <server> | --from-bundle <path>

Every command is non-interactive. Use --json for machine-readable output and --dry-run to avoid mutating installed skills or MCP config.
Artifacts are written under .codex-runtime/stages/.
Catalog sync defaults to official + smithery + glama.
";

const ROOT_AFTER_HELP: &str = "\
Defaults:
  Config file: ~/.mcpsmith/config.yaml
  Installed skills: ~/.agents/skills/

Examples:
  mcpsmith discover --config /tmp/mcp.json
  mcpsmith playwright --dry-run --config /tmp/mcp.json --skills-dir /tmp/skills
  mcpsmith resolve playwright --json --config /tmp/mcp.json
  mcpsmith snapshot --from-resolve .codex-runtime/stages/resolve-playwright.json
  mcpsmith synthesize --from-evidence .codex-runtime/stages/evidence-playwright.json --backend codex
";

const CATALOG_LONG_ABOUT: &str = "\
Inspect or refresh the public MCP catalog snapshot that mcpsmith uses for census data and limited resolution fallback.

`catalog sync` fetches providers and writes a normalized snapshot artifact.
`catalog stats` reports aggregate counts from a saved snapshot or a fresh sync.
";

const CATALOG_SYNC_LONG_ABOUT: &str = "\
Fetch provider data and write a normalized catalog snapshot.

Defaults to the official registry, Smithery, and Glama.
Repeat --provider to override the default provider set.
";

const CATALOG_SYNC_AFTER_HELP: &str = "\
Examples:
  mcpsmith catalog sync
  mcpsmith catalog sync --provider official --provider smithery
  mcpsmith catalog sync --json
";

const CATALOG_STATS_LONG_ABOUT: &str = "\
Report catalog statistics from a saved snapshot or from a fresh sync.

Use --from when you want stats for a specific catalog artifact instead of a new network fetch.
";

const DISCOVER_LONG_ABOUT: &str = "\
Discover installed MCP servers from local config files.

Searches the standard local MCP config locations plus any --config paths you provide.
Use this before resolve or run when you want to see exactly which MCP entries mcpsmith can inspect.
";

const DISCOVER_AFTER_HELP: &str = "\
Examples:
  mcpsmith discover
  mcpsmith discover --config /tmp/mcp.json
  mcpsmith discover --json --config /tmp/mcp.json
";

const RESOLVE_LONG_ABOUT: &str = "\
Resolve the exact source artifact for one installed MCP.

Use this to pin the local path, npm package, PyPI package, or repository revision before snapshotting.
Writes a resolve artifact that snapshot can consume with --from-resolve.
Blocks remote-only or source-unavailable servers instead of converting metadata alone.
";

const RESOLVE_AFTER_HELP: &str = "\
Examples:
  mcpsmith resolve playwright --config /tmp/mcp.json
  mcpsmith resolve source:playwright --json --config /tmp/mcp.json

Direct identity wins. Cached catalog fallback is only used when direct source identity is insufficient.
";

const SNAPSHOT_LONG_ABOUT: &str = "\
Materialize a local source snapshot for one installed MCP.

Run this after resolve when you want the exact source tree captured under the local snapshot cache.
Accepts a server name or a prior resolve artifact via --from-resolve.
";

const SNAPSHOT_AFTER_HELP: &str = "\
Examples:
  mcpsmith snapshot playwright --config /tmp/mcp.json
  mcpsmith snapshot --json --from-resolve .codex-runtime/stages/resolve-playwright.json
";

const EVIDENCE_LONG_ABOUT: &str = "\
Build a per-tool evidence bundle from runtime tools plus a source snapshot.

Deterministic matching is the default path. The artifact records the chosen registration, handler, tests, docs, and confidence for each tool.
Accepts a server name or a prior snapshot artifact via --from-snapshot.
";

const EVIDENCE_AFTER_HELP: &str = "\
Examples:
  mcpsmith evidence playwright --config /tmp/mcp.json
  mcpsmith evidence --tool execute --from-snapshot .codex-runtime/stages/snapshot-playwright.json
";

const SYNTHESIZE_LONG_ABOUT: &str = "\
Synthesize grounded skill drafts from evidence.

This stage reads the evidence bundle, asks the selected backend to draft skills, and preserves the reviewed source citations.
Low-confidence mapper fallback only runs for tools whose deterministic evidence is still weak.
";

const SYNTHESIZE_AFTER_HELP: &str = "\
Examples:
  mcpsmith synthesize playwright --backend codex --config /tmp/mcp.json
  mcpsmith synthesize --json --from-evidence .codex-runtime/stages/evidence-playwright.json --backend-auto
";

const REVIEW_LONG_ABOUT: &str = "\
Review synthesized skills with a second agent pass.

This stage checks the drafted skills for correctness and grounding, applies revisions when possible, and blocks the bundle when the output is unsafe to install.
";

const REVIEW_AFTER_HELP: &str = "\
Examples:
  mcpsmith review playwright --backend codex --config /tmp/mcp.json
  mcpsmith review --from-bundle .codex-runtime/stages/synthesize-playwright.json --json --backend-auto
";

const VERIFY_LONG_ABOUT: &str = "\
Verify generated skills for format, grounding, and references.

Verify can inspect an existing synthesized or reviewed bundle from --from-bundle, or it can generate the prerequisite bundle inline from a server name.
";

const VERIFY_AFTER_HELP: &str = "\
Examples:
  mcpsmith verify playwright --backend codex --config /tmp/mcp.json
  mcpsmith verify --from-bundle .codex-runtime/stages/review-playwright.json --json
";

const UNINSTALL_LONG_ABOUT: &str = "\
Remove previously installed skills for a server.

Reads the parity manifest to identify all skill directories and removes them.
Does not modify MCP config files.
";

const UNINSTALL_AFTER_HELP: &str = "\
Examples:
  mcpsmith uninstall playwright
  mcpsmith uninstall playwright --json
  mcpsmith uninstall playwright --skills-dir /tmp/skills
";

const RUN_LONG_ABOUT: &str = "\
Run the full source-grounded pipeline end-to-end.

Run resolve, snapshot, evidence, synthesize, review, and verify in one command.
Installs reviewed skills and removes the MCP config entry unless --dry-run is set.
Writes resolve, snapshot, evidence, synthesis, review, and verify artifacts for later inspection.
";

const RUN_AFTER_HELP: &str = "\
Examples:
  mcpsmith run playwright --config /tmp/mcp.json --skills-dir /tmp/skills --dry-run
  mcpsmith playwright --json --config /tmp/mcp.json --skills-dir /tmp/skills

Use --skills-dir to write into an isolated preview directory.
";

#[derive(Parser)]
#[command(
    name = "mcpsmith",
    version,
    about = "Convert installed MCP servers into source-grounded skills",
    long_about = LONG_ABOUT,
    after_help = ROOT_AFTER_HELP
)]
struct Cli {
    /// Server id (`source:name`) or unique configured MCP name for one-shot conversion
    server: Option<String>,
    /// Emit machine-readable JSON instead of the default human summary
    #[arg(long)]
    json: bool,
    /// Repeat to inspect multiple MCP config files.
    #[arg(long = "config", value_name = "PATH")]
    config: Vec<PathBuf>,
    /// Write generated skills into this directory instead of `~/.agents/skills/`
    #[arg(long = "skills-dir", value_name = "PATH")]
    skills_dir: Option<PathBuf>,
    /// Force the synthesis/review backend for one-shot conversion
    #[arg(long, value_parser = ["codex", "claude"], requires = "server")]
    backend: Option<String>,
    /// Allow backend auto-detection or fallback during one-shot conversion
    #[arg(long, requires = "server")]
    backend_auto: bool,
    /// Run one-shot conversion without installing skills or editing MCP config
    #[arg(long, requires = "server")]
    dry_run: bool,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    #[command(long_about = CATALOG_LONG_ABOUT)]
    Catalog {
        #[command(subcommand)]
        command: CatalogCommands,
    },
    #[command(alias = "list", long_about = DISCOVER_LONG_ABOUT, after_help = DISCOVER_AFTER_HELP)]
    Discover {
        /// Emit machine-readable JSON instead of the default human summary
        #[arg(long)]
        json: bool,
        /// Repeat to inspect multiple MCP config files.
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
    },
    #[command(long_about = RESOLVE_LONG_ABOUT, after_help = RESOLVE_AFTER_HELP)]
    Resolve {
        /// Server id (`source:name`) or unique configured MCP name
        server: String,
        /// Emit machine-readable JSON instead of the default human summary
        #[arg(long)]
        json: bool,
        /// Repeat to inspect multiple MCP config files.
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
    },
    #[command(long_about = SNAPSHOT_LONG_ABOUT, after_help = SNAPSHOT_AFTER_HELP)]
    Snapshot {
        /// Server id (`source:name`) or unique configured MCP name
        server: Option<String>,
        /// Emit machine-readable JSON instead of the default human summary
        #[arg(long)]
        json: bool,
        /// Read a previously written resolve artifact instead of resolving again
        #[arg(long = "from-resolve", value_name = "PATH")]
        from_resolve: Option<PathBuf>,
        /// Repeat to inspect multiple MCP config files.
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
    },
    #[command(long_about = EVIDENCE_LONG_ABOUT, after_help = EVIDENCE_AFTER_HELP)]
    Evidence {
        /// Server id (`source:name`) or unique configured MCP name
        server: Option<String>,
        /// Emit machine-readable JSON instead of the default human summary
        #[arg(long)]
        json: bool,
        /// Limit evidence extraction to one tool name
        #[arg(long = "tool", value_name = "NAME")]
        tool: Option<String>,
        /// Read a previously written snapshot artifact instead of snapshotting again
        #[arg(long = "from-snapshot", value_name = "PATH")]
        from_snapshot: Option<PathBuf>,
        /// Repeat to inspect multiple MCP config files.
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
    },
    #[command(long_about = SYNTHESIZE_LONG_ABOUT, after_help = SYNTHESIZE_AFTER_HELP)]
    Synthesize {
        /// Server id (`source:name`) or unique configured MCP name
        server: Option<String>,
        /// Emit machine-readable JSON instead of the default human summary
        #[arg(long)]
        json: bool,
        /// Limit synthesis to one tool from the evidence bundle
        #[arg(long = "tool", value_name = "NAME")]
        tool: Option<String>,
        /// Read a previously written evidence artifact instead of rebuilding evidence
        #[arg(long = "from-evidence", value_name = "PATH")]
        from_evidence: Option<PathBuf>,
        /// Repeat to inspect multiple MCP config files.
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
        /// Force the synthesis backend
        #[arg(long, value_parser = ["codex", "claude"])]
        backend: Option<String>,
        /// Allow backend auto-detection or fallback
        #[arg(long)]
        backend_auto: bool,
    },
    #[command(long_about = REVIEW_LONG_ABOUT, after_help = REVIEW_AFTER_HELP)]
    Review {
        /// Server id (`source:name`) or unique configured MCP name
        server: Option<String>,
        /// Emit machine-readable JSON instead of the default human summary
        #[arg(long)]
        json: bool,
        /// Read a synthesized or reviewed bundle instead of rebuilding it
        #[arg(long = "from-bundle", value_name = "PATH")]
        from_bundle: Option<PathBuf>,
        /// Repeat to inspect multiple MCP config files.
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
        /// Force the review backend
        #[arg(long, value_parser = ["codex", "claude"])]
        backend: Option<String>,
        /// Allow backend auto-detection or fallback
        #[arg(long)]
        backend_auto: bool,
    },
    #[command(long_about = VERIFY_LONG_ABOUT, after_help = VERIFY_AFTER_HELP)]
    Verify {
        /// Server id (`source:name`) or unique configured MCP name
        server: Option<String>,
        /// Emit machine-readable JSON instead of the default human summary
        #[arg(long)]
        json: bool,
        /// Read a synthesized or reviewed bundle instead of rebuilding it
        #[arg(long = "from-bundle", value_name = "PATH")]
        from_bundle: Option<PathBuf>,
        /// Repeat to inspect multiple MCP config files.
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
        /// Force the synthesis or review backend when verify builds the bundle inline
        #[arg(long, value_parser = ["codex", "claude"])]
        backend: Option<String>,
        /// Allow backend auto-detection or fallback
        #[arg(long)]
        backend_auto: bool,
    },
    #[command(long_about = UNINSTALL_LONG_ABOUT, after_help = UNINSTALL_AFTER_HELP)]
    Uninstall {
        /// Server slug (directory name under skills dir)
        server: String,
        /// Emit machine-readable JSON instead of the default human summary
        #[arg(long)]
        json: bool,
        /// Look for skills in this directory instead of `~/.agents/skills/`
        #[arg(long = "skills-dir", value_name = "PATH")]
        skills_dir: Option<PathBuf>,
    },
    /// Generate shell completions
    #[command(hide = true)]
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },
    #[command(long_about = RUN_LONG_ABOUT, after_help = RUN_AFTER_HELP)]
    Run {
        /// Server id (`source:name`) or unique configured MCP name
        server: String,
        /// Emit machine-readable JSON instead of the default human summary
        #[arg(long)]
        json: bool,
        /// Repeat to inspect multiple MCP config files.
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
        /// Write generated skills into this directory instead of `~/.agents/skills/`
        #[arg(long = "skills-dir", value_name = "PATH")]
        skills_dir: Option<PathBuf>,
        /// Force the synthesis and review backend
        #[arg(long, value_parser = ["codex", "claude"])]
        backend: Option<String>,
        /// Allow backend auto-detection or fallback
        #[arg(long)]
        backend_auto: bool,
        /// Run the full pipeline without installing skills or editing MCP config
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum CatalogCommands {
    #[command(long_about = CATALOG_SYNC_LONG_ABOUT, after_help = CATALOG_SYNC_AFTER_HELP)]
    Sync {
        /// Emit machine-readable JSON instead of the default human summary
        #[arg(long)]
        json: bool,
        /// Repeat to override the default provider set (`official`, `smithery`, `glama`)
        #[arg(long = "provider", value_name = "NAME")]
        provider: Vec<String>,
    },
    #[command(long_about = CATALOG_STATS_LONG_ABOUT)]
    Stats {
        /// Emit machine-readable JSON instead of the default human summary
        #[arg(long)]
        json: bool,
        /// Read a previously written catalog sync artifact instead of syncing again
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
        Some(Commands::Discover { json, config }) => {
            commands::agentic::run_discover_cmd(json, &config)?;
        }
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
        Some(Commands::Uninstall {
            server,
            json,
            skills_dir,
        }) => {
            commands::agentic::run_uninstall_cmd(&server, json, skills_dir)?;
        }
        Some(Commands::Completions { shell }) => {
            clap_complete::generate(
                shell,
                &mut Cli::command(),
                "mcpsmith",
                &mut std::io::stdout(),
            );
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
