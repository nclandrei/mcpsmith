use crate::{
    ConversionRecommendation, ConvertInventory, ConvertPlan, MCPServerProfile, PermissionLevel,
    PlanMode,
    source::{default_sources, discover_from_sources},
};
use anyhow::{Result, bail};
use chrono::Utc;
use std::path::PathBuf;

pub fn discover(additional_paths: &[PathBuf]) -> Result<ConvertInventory> {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let sources = default_sources(&home, &cwd, additional_paths);
    discover_from_sources(&sources)
}

pub fn inspect(server_selector: &str, additional_paths: &[PathBuf]) -> Result<MCPServerProfile> {
    let inventory = discover(additional_paths)?;
    resolve_server(&inventory.servers, server_selector)
}

pub fn plan(
    server_selector: &str,
    requested_mode: PlanMode,
    additional_paths: &[PathBuf],
) -> Result<ConvertPlan> {
    let server = inspect(server_selector, additional_paths)?;

    let recommended_mode = match server.recommendation {
        ConversionRecommendation::ReplaceCandidate => PlanMode::Replace,
        ConversionRecommendation::Hybrid | ConversionRecommendation::KeepMcp => PlanMode::Hybrid,
    };

    let effective_mode = if requested_mode == PlanMode::Auto {
        recommended_mode
    } else {
        requested_mode
    };

    let mut actions = vec![
        "Capture the current MCP server config and create a rollback backup.".to_string(),
        "Generate one orchestrator skill and a capability skill set mapped to tool behaviors."
            .to_string(),
    ];
    let mut warnings = vec![];
    let mut blocked = false;

    match effective_mode {
        PlanMode::Hybrid => {
            actions.push(
                "Keep MCP enabled and use the generated skill set as the default orchestration layer."
                    .to_string(),
            );
            actions.push(
                "Validate skill instructions against a task corpus while MCP remains fallback."
                    .to_string(),
            );
        }
        PlanMode::Replace => {
            actions.push(
                "Generate replacement skill set and parity checks for tool-level behaviors."
                    .to_string(),
            );
            actions.push(
                "Disable MCP config entry only after parity checks pass and user confirms apply."
                    .to_string(),
            );
        }
        PlanMode::Auto => unreachable!("effective mode resolves away from auto"),
    }

    if effective_mode == PlanMode::Replace
        && server.recommendation != ConversionRecommendation::ReplaceCandidate
    {
        blocked = true;
        warnings.push(
            "Replace mode is blocked because this server is not a safe replace candidate."
                .to_string(),
        );
        warnings.push(format!(
            "Recommendation is '{}' ({})",
            server.recommendation, server.recommendation_reason
        ));
    }

    if server.inferred_permission == PermissionLevel::Destructive {
        warnings.push(
            "Destructive capability detected; keep MCP with explicit human review gates."
                .to_string(),
        );
    }

    if server.url.is_some() {
        warnings.push(
            "Remote URL-backed MCP servers are typically dynamic; replacement is usually unsafe."
                .to_string(),
        );
    }

    Ok(ConvertPlan {
        generated_at: Utc::now(),
        server,
        requested_mode,
        recommended_mode,
        effective_mode,
        blocked,
        actions,
        warnings,
    })
}

pub(crate) fn resolve_server(
    servers: &[MCPServerProfile],
    server_selector: &str,
) -> Result<MCPServerProfile> {
    let selector = server_selector.trim();
    if selector.is_empty() {
        bail!("Server selector must be non-empty.");
    }

    if let Some(found) = servers
        .iter()
        .find(|s| s.id.eq_ignore_ascii_case(selector))
        .cloned()
    {
        return Ok(found);
    }

    let mut by_name = servers
        .iter()
        .filter(|s| s.name.eq_ignore_ascii_case(selector))
        .cloned()
        .collect::<Vec<_>>();

    if by_name.is_empty() {
        let known = servers.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        if known.is_empty() {
            bail!(
                "No MCP servers discovered. Run 'mcpsmith discover --all' to inspect searched paths."
            );
        }
        bail!(
            "No MCP server matched '{selector}'. Known server ids: {}",
            known.join(", ")
        );
    }

    by_name.sort_by(|a, b| a.id.cmp(&b.id));
    if by_name.len() > 1 {
        let ids = by_name.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        bail!(
            "Server name '{selector}' is ambiguous. Use one of: {}",
            ids.join(", ")
        );
    }

    Ok(by_name.remove(0))
}
