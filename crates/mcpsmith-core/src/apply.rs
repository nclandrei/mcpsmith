use crate::backend::codex_enrichment_hints;
use crate::contract::contract_test_bundle;
use crate::diagnostics::verify_with_server_and_path;
use crate::dossier::{discover_v3, load_dossier_bundle};
use crate::inventory::plan;
use crate::runtime::introspect_tool_specs;
use crate::skillset::{
    default_agents_skills_dir, render_capability_skill_markdown,
    render_orchestrator_skill_markdown, required_tool_names, sanitize_slug, write_server_skills,
    write_skill_manifest,
};
use crate::{
    ApplyOptions, ApplyResultV3, ApplyServerResult, ContractTestOptions, ConvertApplyResult,
    ConvertV3Options, DossierBundle, EnrichmentAgent, ManifestToolSkill, OneShotV3Result, PlanMode,
    ServerGate, SkillParityManifest,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

fn default_skills_dir() -> PathBuf {
    default_agents_skills_dir()
}

pub fn apply(
    server_selector: &str,
    requested_mode: PlanMode,
    confirm_replace: bool,
    additional_paths: &[PathBuf],
    output_dir: Option<PathBuf>,
) -> Result<ConvertApplyResult> {
    apply_with_options(
        server_selector,
        requested_mode,
        confirm_replace,
        additional_paths,
        ApplyOptions {
            output_dir,
            enrichment_agent: None,
        },
    )
}

pub fn apply_with_options(
    server_selector: &str,
    requested_mode: PlanMode,
    confirm_replace: bool,
    additional_paths: &[PathBuf],
    options: ApplyOptions,
) -> Result<ConvertApplyResult> {
    let conversion_plan = plan(server_selector, requested_mode, additional_paths)?;

    if conversion_plan.blocked {
        bail!(
            "Conversion plan is blocked for '{}': {}",
            conversion_plan.server.id,
            conversion_plan.warnings.join(" | ")
        );
    }

    if conversion_plan.effective_mode == PlanMode::Replace && !confirm_replace {
        bail!("Replace mode requires explicit confirmation via --yes.");
    }

    let skills_dir = options.output_dir.unwrap_or_else(default_skills_dir);
    std::fs::create_dir_all(&skills_dir)
        .with_context(|| format!("Failed to create skills directory {}", skills_dir.display()))?;

    let introspected_specs = introspect_tool_specs(&conversion_plan.server).ok();
    let introspected_tools = introspected_specs.as_ref().map(|items| {
        items
            .iter()
            .map(|item| item.name.clone())
            .collect::<Vec<String>>()
    });

    let mut required_tools =
        required_tool_names(&conversion_plan.server, introspected_tools.as_deref());
    if required_tools.is_empty() {
        required_tools.push("general-orchestration".to_string());
    }

    let spec_by_name = introspected_specs
        .as_ref()
        .map(|items| {
            items
                .iter()
                .cloned()
                .map(|item| (item.name.clone(), item))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();

    let mut notes = vec![];
    let enrichment_hints = match options.enrichment_agent {
        Some(EnrichmentAgent::Codex) => {
            match codex_enrichment_hints(&conversion_plan.server, &required_tools, &spec_by_name) {
                Ok(hints) => {
                    notes.push(format!(
                        "Codex enrichment added optional hints for {} capability skill(s).",
                        hints.len()
                    ));
                    hints
                }
                Err(err) => {
                    notes.push(format!(
                        "Codex enrichment unavailable; used deterministic templates only ({err})."
                    ));
                    BTreeMap::new()
                }
            }
        }
        None => BTreeMap::new(),
    };

    let server_slug = sanitize_slug(&conversion_plan.server.name);
    let mut slug_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut tool_skills = Vec::new();
    let mut tool_skill_paths = Vec::new();
    for tool_name in &required_tools {
        let base_slug = sanitize_slug(tool_name);
        let counter = slug_counts.entry(base_slug.clone()).or_insert(0);
        let tool_slug = if *counter == 0 {
            base_slug.clone()
        } else {
            format!("{base_slug}-{}", *counter + 1)
        };
        *counter += 1;

        let file_name = format!("mcp-{server_slug}-tool-{tool_slug}.md");
        let skill_path = skills_dir.join(&file_name);
        let description = spec_by_name
            .get(tool_name)
            .and_then(|item| item.description.as_deref());
        std::fs::write(
            &skill_path,
            render_capability_skill_markdown(
                &conversion_plan.server,
                tool_name,
                description,
                enrichment_hints.get(tool_name),
            ),
        )
        .with_context(|| format!("Failed to write capability skill {}", skill_path.display()))?;
        tool_skills.push(ManifestToolSkill {
            tool_name: tool_name.clone(),
            skill_file: file_name,
        });
        tool_skill_paths.push(skill_path);
    }

    let orchestrator_filename = format!("mcp-{server_slug}.md");
    let skill_path = skills_dir.join(&orchestrator_filename);
    std::fs::write(
        &skill_path,
        render_orchestrator_skill_markdown(&conversion_plan, &tool_skills),
    )
    .with_context(|| {
        format!(
            "Failed to write converted orchestrator skill {}",
            skill_path.display()
        )
    })?;
    let manifest = SkillParityManifest {
        format_version: 2,
        generated_at: Utc::now(),
        server_id: conversion_plan.server.id.clone(),
        server_name: conversion_plan.server.name.clone(),
        orchestrator_skill: Some(orchestrator_filename),
        required_tools: required_tools.clone(),
        tool_skills: tool_skills.clone(),
        required_tool_hints: vec![],
    };
    write_skill_manifest(&skill_path, &manifest)?;

    notes.push(format!(
        "Generated 1 orchestrator skill and {} capability skills.",
        tool_skills.len()
    ));
    notes.push("Wrote internal parity manifest for verify checks.".to_string());
    let mut mcp_config_backup = None;
    let mut mcp_config_updated = false;

    match conversion_plan.effective_mode {
        PlanMode::Hybrid => {
            notes.push("MCP config left unchanged (hybrid mode).".to_string());
        }
        PlanMode::Replace => {
            if introspected_tools.is_none() {
                bail!(
                    "Replace mode requires successful live MCP tool introspection before config mutation."
                );
            }
            let verify = verify_with_server_and_path(
                &conversion_plan.server,
                &skill_path,
                introspected_tools.as_deref(),
            )?;
            if !verify.passed {
                bail!(
                    "Replace mode verification failed for '{}': missing_in_server=[{}], missing_in_skill=[{}], missing_skill_files=[{}]",
                    conversion_plan.server.id,
                    verify.missing_in_server.join(", "),
                    verify.missing_in_skill.join(", "),
                    verify.missing_skill_files.join(", ")
                );
            }
            let (backup, updated) = remove_server_from_config(
                &conversion_plan.server.source_path,
                &conversion_plan.server.name,
            )?;
            mcp_config_backup = backup;
            mcp_config_updated = updated;
            if updated {
                notes.push("Removed MCP server entry from config (replace mode).".to_string());
            } else {
                notes.push(
                    "No MCP server entry was removed (server key not found in config).".to_string(),
                );
            }
        }
        PlanMode::Auto => unreachable!("effective mode resolves away from auto"),
    }

    Ok(ConvertApplyResult {
        generated_at: Utc::now(),
        server: conversion_plan.server,
        requested_mode,
        effective_mode: conversion_plan.effective_mode,
        orchestrator_skill_path: skill_path.clone(),
        skill_path,
        tool_skill_paths,
        mcp_config_backup,
        mcp_config_updated,
        notes,
    })
}

pub fn apply_from_dossier_path(
    dossier_path: &Path,
    yes: bool,
    skills_dir: Option<PathBuf>,
    contract_options: ContractTestOptions,
) -> Result<ApplyResultV3> {
    let bundle = load_dossier_bundle(dossier_path)?;
    apply_from_bundle(&bundle, yes, skills_dir, contract_options)
}

pub fn run_one_shot_v3(
    selector: &str,
    additional_paths: &[PathBuf],
    options: &ConvertV3Options,
    skills_dir: Option<PathBuf>,
    contract_options: ContractTestOptions,
) -> Result<OneShotV3Result> {
    let bundle = discover_v3(Some(selector), false, additional_paths, options)?;
    let contract = contract_test_bundle(&bundle, contract_options)?;
    if !contract.passed {
        bail!(
            "Conversion blocked: contract tests failed. Run 'mcpsmith contract-test --from-dossier ...' for details."
        );
    }
    let apply = apply_from_bundle(&bundle, true, skills_dir, contract_options)?;
    Ok(OneShotV3Result {
        generated_at: Utc::now(),
        dossier: bundle,
        contract_test: contract,
        apply,
    })
}

pub fn apply_from_bundle(
    bundle: &DossierBundle,
    yes: bool,
    skills_dir: Option<PathBuf>,
    contract_options: ContractTestOptions,
) -> Result<ApplyResultV3> {
    if !yes {
        bail!("apply requires --yes because it mutates MCP config entries.");
    }

    if bundle.dossiers.is_empty() {
        bail!("No dossiers found to apply.");
    }

    let contract = contract_test_bundle(bundle, contract_options)?;
    if !contract.passed {
        bail!(
            "Conversion blocked: one or more contract tests failed. No files were applied or MCP configs mutated."
        );
    }

    let skills_root = skills_dir.unwrap_or_else(default_agents_skills_dir);
    fs::create_dir_all(&skills_root)
        .with_context(|| format!("Failed to create skills dir {}", skills_root.display()))?;

    let mut servers = Vec::with_capacity(bundle.dossiers.len());
    for dossier in &bundle.dossiers {
        if dossier.server_gate != ServerGate::Ready {
            bail!(
                "Server '{}' is blocked and cannot be applied: {}",
                dossier.server.id,
                dossier.gate_reasons.join(" | ")
            );
        }

        let (orchestrator, tool_paths, notes) = write_server_skills(dossier, &skills_root)?;

        let remove_result =
            remove_server_from_config(&dossier.server.source_path, &dossier.server.name);
        let (backup, updated) = match remove_result {
            Ok((backup, updated)) if updated => (backup, updated),
            Ok((_backup, _updated)) => {
                rollback_server_skill_files(&orchestrator, &tool_paths);
                bail!(
                    "MCP config entry '{}' not found in {}. Rolled back generated skills to keep conversion atomic.",
                    dossier.server.name,
                    dossier.server.source_path.display()
                );
            }
            Err(err) => {
                rollback_server_skill_files(&orchestrator, &tool_paths);
                return Err(err).with_context(|| {
                    format!(
                        "Failed to mutate MCP config for '{}'; generated skill files were rolled back.",
                        dossier.server.id
                    )
                });
            }
        };

        servers.push(ApplyServerResult {
            server_id: dossier.server.id.clone(),
            applied: true,
            orchestrator_skill_path: orchestrator,
            tool_skill_paths: tool_paths,
            mcp_config_backup: backup,
            mcp_config_updated: updated,
            notes,
        });
    }

    Ok(ApplyResultV3 {
        generated_at: Utc::now(),
        skills_dir: skills_root,
        servers,
    })
}

fn rollback_server_skill_files(orchestrator: &Path, tool_paths: &[PathBuf]) {
    for path in tool_paths {
        let _ = fs::remove_file(path);
    }
    let _ = fs::remove_file(orchestrator);
}

fn remove_server_from_config(path: &Path, server_name: &str) -> Result<(Option<PathBuf>, bool)> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read config {}", path.display()))?;

    if let Ok(mut root) = serde_json::from_str::<Value>(&raw) {
        let removed = remove_server_from_json(&mut root, server_name);
        if !removed {
            return Ok((None, false));
        }

        let backup = backup_file(path)?;
        let body = serde_json::to_string_pretty(&root)
            .context("Failed to serialize updated JSON MCP config")?;
        std::fs::write(path, format!("{body}\n"))
            .with_context(|| format!("Failed to write config {}", path.display()))?;
        return Ok((Some(backup), true));
    }

    if let Ok(mut root) = toml::from_str::<toml::Value>(&raw) {
        let removed = remove_server_from_toml(&mut root, server_name);
        if !removed {
            return Ok((None, false));
        }

        let backup = backup_file(path)?;
        let body =
            toml::to_string_pretty(&root).context("Failed to serialize updated TOML MCP config")?;
        std::fs::write(path, body)
            .with_context(|| format!("Failed to write config {}", path.display()))?;
        return Ok((Some(backup), true));
    }

    bail!(
        "Failed to parse {} as JSON or TOML for replace mode update.",
        path.display()
    )
}

fn remove_server_from_json(root: &mut Value, server_name: &str) -> bool {
    let mut removed = false;

    if let Some(obj) = root.get_mut("mcpServers").and_then(Value::as_object_mut) {
        removed |= obj.remove(server_name).is_some();
    }
    if let Some(obj) = root.get_mut("mcp_servers").and_then(Value::as_object_mut) {
        removed |= obj.remove(server_name).is_some();
    }
    if let Some(obj) = root.get_mut("servers").and_then(Value::as_object_mut) {
        removed |= obj.remove(server_name).is_some();
    }
    if let Some(obj) = root
        .get_mut("amp.mcpServers")
        .and_then(Value::as_object_mut)
    {
        removed |= obj.remove(server_name).is_some();
    }

    if let Some(amp) = root.get_mut("amp").and_then(Value::as_object_mut)
        && let Some(obj) = amp.get_mut("mcpServers").and_then(Value::as_object_mut)
    {
        removed |= obj.remove(server_name).is_some();
    }

    if let Some(obj) = root.as_object_mut() {
        let should_remove = obj.get(server_name).is_some_and(likely_server_object);
        if should_remove {
            removed |= obj.remove(server_name).is_some();
        }
    }

    removed
}

fn remove_server_from_toml(root: &mut toml::Value, server_name: &str) -> bool {
    let mut removed = false;

    if let Some(table) = root.as_table_mut() {
        if let Some(mcp_servers) = table
            .get_mut("mcp_servers")
            .and_then(toml::Value::as_table_mut)
        {
            removed |= mcp_servers.remove(server_name).is_some();
        }

        if let Some(amp_mcp) = table
            .get_mut("amp.mcpServers")
            .and_then(toml::Value::as_table_mut)
        {
            removed |= amp_mcp.remove(server_name).is_some();
        }

        if let Some(amp) = table.get_mut("amp").and_then(toml::Value::as_table_mut)
            && let Some(mcp) = amp
                .get_mut("mcpServers")
                .and_then(toml::Value::as_table_mut)
        {
            removed |= mcp.remove(server_name).is_some();
        }
    }

    removed
}

fn backup_file(path: &Path) -> Result<PathBuf> {
    let filename = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("mcp-config")
        .to_string();
    let backup_name = format!("{}.bak-{}", filename, Utc::now().format("%Y%m%d-%H%M%S"));
    let backup_path = path.with_file_name(backup_name);
    std::fs::copy(path, &backup_path).with_context(|| {
        format!(
            "Failed to create backup from {} to {}",
            path.display(),
            backup_path.display()
        )
    })?;
    Ok(backup_path)
}

fn likely_server_object(value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    let keys = [
        "command",
        "args",
        "url",
        "endpoint",
        "env",
        "description",
        "purpose",
        "permissions",
        "scopes",
        "capabilities",
        "tools",
    ];
    keys.iter().any(|key| obj.contains_key(*key))
}
