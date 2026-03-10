use crate::backend::{generate_workflow_skills, prepare_backend_context};
use crate::inventory::{discover, resolve_server};
use crate::runtime::introspect_tool_specs;
use crate::skillset::{build_from_bundle, normalize_tool_name};
use crate::{
    BackendContext, BuildResult, ConvertInventory, ConvertV3Options, DOSSIER_FORMAT_VERSION,
    DossierBundle, MCPServerProfile, PermissionLevel, ProbeInputSource, ProbeInputs, RuntimeTool,
    RuntimeValidationSpec, ServerDossier, ServerGate, SourceEvidenceLevel, ToolContractTest,
    WorkflowContextInput, WorkflowSkillSpec,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

pub fn discover_v3(
    selector: Option<&str>,
    all: bool,
    additional_paths: &[PathBuf],
    options: &ConvertV3Options,
) -> Result<DossierBundle> {
    let inventory = discover(additional_paths)?;
    let targets = select_servers(&inventory, selector, all)?;
    if targets.is_empty() {
        bail!("No MCP servers selected for conversion discovery.");
    }

    let backend_ctx = prepare_backend_context(options)?;
    let mut dossiers = Vec::with_capacity(targets.len());

    for server in targets {
        let dossier = discover_server_dossier(&server, options, &backend_ctx)?;
        dossiers.push(dossier);
    }

    Ok(DossierBundle {
        format_version: DOSSIER_FORMAT_VERSION,
        generated_at: Utc::now(),
        dossiers,
    })
}

pub fn discover_v3_to_path(
    selector: Option<&str>,
    all: bool,
    additional_paths: &[PathBuf],
    options: &ConvertV3Options,
    out_path: &Path,
) -> Result<DossierBundle> {
    let bundle = discover_v3(selector, all, additional_paths, options)?;
    write_dossier_bundle(out_path, &bundle)?;
    Ok(bundle)
}

pub fn build_from_dossier_path(
    dossier_path: &Path,
    skills_dir: Option<PathBuf>,
) -> Result<BuildResult> {
    let bundle = load_dossier_bundle(dossier_path)?;
    build_from_bundle(&bundle, skills_dir)
}

pub fn write_dossier_bundle(path: &Path, bundle: &DossierBundle) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    let body =
        serde_json::to_string_pretty(bundle).context("Failed to serialize dossier bundle")?;
    fs::write(path, format!("{body}\n"))
        .with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

pub fn load_dossier_bundle(path: &Path) -> Result<DossierBundle> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;

    if let Ok(bundle) = serde_json::from_str::<DossierBundle>(&raw) {
        return Ok(upgrade_bundle_from_legacy(bundle));
    }

    if let Ok(single) = serde_json::from_str::<ServerDossier>(&raw) {
        let bundle = DossierBundle {
            format_version: DOSSIER_FORMAT_VERSION,
            generated_at: Utc::now(),
            dossiers: vec![single],
        };
        return Ok(upgrade_bundle_from_legacy(bundle));
    }

    bail!(
        "Invalid dossier JSON in {}. Expected either a DossierBundle or a ServerDossier.",
        path.display()
    )
}

fn upgrade_bundle_from_legacy(mut bundle: DossierBundle) -> DossierBundle {
    if bundle.format_version < DOSSIER_FORMAT_VERSION {
        bundle.format_version = DOSSIER_FORMAT_VERSION;
    }
    for dossier in &mut bundle.dossiers {
        if dossier.format_version < DOSSIER_FORMAT_VERSION {
            dossier.format_version = DOSSIER_FORMAT_VERSION;
        }
        for tool in &mut dossier.tool_dossiers {
            if tool.probe_input_source == ProbeInputSource::Synthesized
                && has_any_probe_inputs(&tool.probe_inputs)
            {
                tool.probe_input_source = ProbeInputSource::Backend;
            }
        }
        if dossier.runtime_validations.is_empty() && !dossier.tool_dossiers.is_empty() {
            dossier.runtime_validations = dossier
                .tool_dossiers
                .iter()
                .map(|tool| RuntimeValidationSpec {
                    tool_name: normalize_tool_name(&tool.name),
                    contract_tests: tool.contract_tests.clone(),
                    probe_inputs: tool.probe_inputs.clone(),
                    probe_input_source: tool.probe_input_source,
                })
                .collect();
        }
        if dossier.workflow_skills.is_empty() && !dossier.tool_dossiers.is_empty() {
            if !dossier.gate_reasons.iter().any(|reason| {
                reason.contains(
                    "Legacy tool-dossier bundles do not contain standalone workflow skills",
                )
            }) {
                dossier.gate_reasons.push(
                    "Legacy tool-dossier bundles do not contain standalone workflow skills; re-run `mcpsmith discover` with the current version.".to_string(),
                );
            }
            dossier.server_gate = ServerGate::Blocked;
        }
    }
    bundle
}

fn select_servers(
    inventory: &ConvertInventory,
    selector: Option<&str>,
    all: bool,
) -> Result<Vec<MCPServerProfile>> {
    if all {
        return Ok(inventory.servers.clone());
    }
    if let Some(selector) = selector {
        return Ok(vec![resolve_server(&inventory.servers, selector)?]);
    }

    if inventory.servers.len() == 1 {
        return Ok(inventory.servers.clone());
    }

    bail!(
        "Specify a server name/id or pass --all. Found {} discoverable server(s).",
        inventory.servers.len()
    )
}

fn discover_server_dossier(
    server: &MCPServerProfile,
    options: &ConvertV3Options,
    backend_ctx: &BackendContext,
) -> Result<ServerDossier> {
    let mut gate_reasons = vec![];
    let mut diagnostics = backend_ctx.selection.diagnostics.clone();

    let runtime_specs = introspect_tool_specs(server)
        .with_context(|| format!("Runtime MCP introspection failed for '{}'.", server.id));

    let runtime_tools = match runtime_specs {
        Ok(specs) => specs
            .iter()
            .map(|spec| RuntimeTool {
                name: normalize_tool_name(&spec.name),
                description: spec.description.clone(),
                input_schema: spec.input_schema.clone(),
            })
            .collect::<Vec<_>>(),
        Err(err) => {
            gate_reasons.push(format!("Runtime tools/list introspection failed: {err}"));
            vec![]
        }
    };

    if runtime_tools.is_empty() {
        gate_reasons.push(
            "No runtime tools available; cannot derive standalone replacement workflows."
                .to_string(),
        );
    }

    let mut selected_backend = backend_ctx.selection.selected;
    let mut fallback_used = false;
    let runtime_validations = synthesize_runtime_validations(server, &runtime_tools);
    let mut workflow_skills = vec![];
    if !runtime_tools.is_empty() {
        match generate_workflow_skills(
            selected_backend,
            server,
            &runtime_tools,
            options.backend_config.chunk_size.max(1),
            options.backend_config.timeout_seconds,
        ) {
            Ok(generated) => workflow_skills = generated,
            Err(err) => {
                diagnostics.push(format!(
                    "Primary backend '{}' failed to synthesize standalone workflows: {err}",
                    selected_backend
                ));
                if backend_ctx.selection.auto_mode {
                    if let Some(fallback) = backend_ctx.selection.fallback {
                        fallback_used = true;
                        selected_backend = fallback;
                        diagnostics.push(format!(
                            "Retrying standalone workflow synthesis with fallback backend '{}'.",
                            fallback
                        ));
                        match generate_workflow_skills(
                            fallback,
                            server,
                            &runtime_tools,
                            options.backend_config.chunk_size.max(1),
                            options.backend_config.timeout_seconds,
                        ) {
                            Ok(generated) => workflow_skills = generated,
                            Err(fallback_err) => {
                                diagnostics.push(format!(
                                    "Standalone workflow synthesis failed on primary and fallback backends: fallback_error={fallback_err}"
                                ));
                                gate_reasons.push(
                                    "Backend workflow synthesis failed on both primary and fallback backends."
                                        .to_string(),
                                );
                            }
                        }
                    } else {
                        diagnostics.push(format!(
                            "Standalone workflow synthesis failed and no fallback backend is available. error={err}"
                        ));
                        gate_reasons.push(
                            "Backend workflow synthesis failed and no fallback backend is available."
                                .to_string(),
                        );
                    }
                } else {
                    diagnostics.push(format!("Standalone workflow synthesis failed. error={err}"));
                    gate_reasons.push("Backend workflow synthesis failed.".to_string());
                }
            }
        }
    }

    let runtime_map = runtime_tools
        .iter()
        .map(|tool| (normalize_tool_name(&tool.name), tool.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut covered_tools = BTreeSet::new();
    for workflow in &mut workflow_skills {
        hydrate_workflow_from_runtime_tools(workflow, server, &runtime_map);
        merge_workflow_source_evidence(workflow, server, &runtime_map);
        for reason in workflow_gate_reasons(workflow) {
            gate_reasons.push(format!("Workflow '{}': {}", workflow.id, reason));
        }
        for tool_name in &workflow.origin_tools {
            covered_tools.insert(tool_name.clone());
        }
    }
    for (name, runtime_tool) in &runtime_map {
        if !tool_needs_standalone_workflow(runtime_tool) {
            continue;
        }
        if !covered_tools.contains(name) {
            gate_reasons.push(format!(
                "Tool '{}' has no concrete standalone replacement workflow.",
                name
            ));
        }
    }

    let mut backend_diagnostics = diagnostics;
    backend_diagnostics.extend(backend_ctx.health.statuses.iter().map(|status| {
        format!(
            "backend={} available={} diagnostics={}",
            status.backend,
            status.available,
            status.diagnostics.join(" | ")
        )
    }));

    let server_gate = if gate_reasons.is_empty() {
        ServerGate::Ready
    } else {
        ServerGate::Blocked
    };

    Ok(ServerDossier {
        generated_at: Utc::now(),
        format_version: DOSSIER_FORMAT_VERSION,
        server: server.clone(),
        runtime_tools,
        runtime_validations,
        workflow_skills,
        tool_dossiers: vec![],
        server_gate,
        gate_reasons,
        backend_used: selected_backend.to_string(),
        backend_fallback_used: fallback_used,
        backend_diagnostics,
    })
}

fn synthesize_runtime_validations(
    server: &MCPServerProfile,
    runtime_tools: &[RuntimeTool],
) -> Vec<RuntimeValidationSpec> {
    runtime_tools
        .iter()
        .map(|tool| RuntimeValidationSpec {
            tool_name: normalize_tool_name(&tool.name),
            contract_tests: default_contract_tests(server),
            probe_inputs: ProbeInputs::default(),
            probe_input_source: ProbeInputSource::Synthesized,
        })
        .collect()
}

fn merge_workflow_source_evidence(
    workflow: &mut WorkflowSkillSpec,
    server: &MCPServerProfile,
    runtime_tools: &BTreeMap<String, RuntimeTool>,
) {
    for tool_name in &workflow.origin_tools {
        if let Some(runtime_tool) = runtime_tools.get(tool_name) {
            workflow
                .evidence
                .extend(source_ground_evidence(server, runtime_tool));
        }
    }
    workflow.evidence.retain(|item| !item.trim().is_empty());
    workflow
        .evidence
        .iter_mut()
        .for_each(|item| *item = item.trim().to_string());
    workflow.evidence.sort();
    workflow.evidence.dedup();
}

fn hydrate_workflow_from_runtime_tools(
    workflow: &mut WorkflowSkillSpec,
    server: &MCPServerProfile,
    runtime_tools: &BTreeMap<String, RuntimeTool>,
) {
    if workflow.required_context.is_empty() {
        workflow.required_context = infer_required_context(workflow, runtime_tools);
    }

    if workflow.context_acquisition.is_empty() && !workflow.required_context.is_empty() {
        workflow.context_acquisition = workflow
            .required_context
            .iter()
            .map(|input| {
                format!(
                    "Ask for `{}` if it is not already known from prior workflow output or the local project.",
                    input.name
                )
            })
            .collect();
    }

    if workflow.guardrails.is_empty() {
        workflow.guardrails = default_workflow_guardrails(server, workflow);
    }
}

fn infer_required_context(
    workflow: &WorkflowSkillSpec,
    runtime_tools: &BTreeMap<String, RuntimeTool>,
) -> Vec<WorkflowContextInput> {
    let mut out = vec![];
    let mut seen = BTreeSet::new();

    for tool_name in &workflow.origin_tools {
        let Some(schema) = runtime_tools
            .get(tool_name)
            .and_then(|tool| tool.input_schema.as_ref())
        else {
            continue;
        };
        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let properties = schema.get("properties").and_then(Value::as_object);

        for name in required {
            if !seen.insert(name.clone()) {
                continue;
            }
            let guidance = properties
                .and_then(|props| props.get(&name))
                .and_then(Value::as_object)
                .and_then(|prop| prop.get("description"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(ToString::to_string)
                .unwrap_or_else(|| format!("Provide `{name}` before running this workflow."));
            out.push(WorkflowContextInput {
                name,
                guidance,
                required: true,
            });
        }
    }

    out
}

fn default_workflow_guardrails(
    server: &MCPServerProfile,
    workflow: &WorkflowSkillSpec,
) -> Vec<String> {
    let mut guardrails =
        vec!["Do not guess missing paths, identifiers, or simulator targets.".to_string()];
    if server.inferred_permission != PermissionLevel::ReadOnly {
        guardrails.push(
            "Double-check commands before mutating build artifacts, simulator state, or persisted configuration."
                .to_string(),
        );
    }
    if workflow
        .origin_tools
        .iter()
        .any(|tool| tool.contains("clean"))
    {
        guardrails.push(
            "Confirm the target build products before removing or cleaning anything.".to_string(),
        );
    }
    guardrails
}

fn tool_needs_standalone_workflow(tool: &RuntimeTool) -> bool {
    let name = normalize_tool_name(&tool.name);
    let description = tool
        .description
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();

    if name.starts_with("session_")
        && (description.contains("default")
            || description.contains("profile")
            || description.contains("session"))
    {
        return false;
    }

    true
}

fn workflow_gate_reasons(workflow: &WorkflowSkillSpec) -> Vec<String> {
    let mut reasons = vec![];
    if workflow.id.trim().is_empty() {
        reasons.push("workflow id is empty".to_string());
    }
    if workflow.goal.trim().is_empty() {
        reasons.push("goal is empty".to_string());
    }
    if workflow.when_to_use.trim().is_empty() {
        reasons.push("when_to_use is empty".to_string());
    }
    if workflow.origin_tools.is_empty() {
        reasons.push("origin_tools is empty".to_string());
    }
    if workflow.native_steps.is_empty() {
        reasons.push("native_steps is empty".to_string());
    }
    if workflow.verification.is_empty() {
        reasons.push("verification is empty".to_string());
    }
    if workflow.return_contract.is_empty() {
        reasons.push("return_contract is empty".to_string());
    }
    if workflow.stop_and_ask.is_empty() {
        reasons.push("stop_and_ask is empty".to_string());
    }
    reasons
}

#[cfg(test)]
pub(crate) fn merge_source_grounding_evidence(
    mut evidence: Vec<String>,
    server: &MCPServerProfile,
    runtime_tool: &RuntimeTool,
) -> Vec<String> {
    evidence.extend(source_ground_evidence(server, runtime_tool));
    evidence.sort();
    evidence.dedup();
    evidence
}

pub(crate) fn source_ground_evidence(
    server: &MCPServerProfile,
    runtime_tool: &RuntimeTool,
) -> Vec<String> {
    let mut evidence = vec![format!(
        "evidence-level: {}",
        server.source_grounding.evidence_level
    )];

    if let Some(url) = &server.url {
        evidence.push(format!("runtime-url: {url}"));
    }

    if let Some(command) = &server.command {
        let mut cmd = command.clone();
        if !server.args.is_empty() {
            cmd.push(' ');
            cmd.push_str(&server.args.join(" "));
        }
        evidence.push(format!("runtime-command: {cmd}"));
    }

    if let Some(description) = &runtime_tool.description {
        evidence.push(format!("runtime-tool-description: {description}"));
    }

    if let Some(entrypoint) = &server.source_grounding.entrypoint {
        evidence.push(format!("source-entrypoint: {}", entrypoint.display()));
    }

    if let Some(package_name) = &server.source_grounding.package_name {
        if let Some(package_version) = &server.source_grounding.package_version {
            evidence.push(format!("source-package: {package_name}@{package_version}"));
        } else {
            evidence.push(format!("source-package: {package_name}"));
        }
    }

    if let Some(homepage) = &server.source_grounding.homepage {
        evidence.push(format!("source-homepage: {homepage}"));
    }

    if let Some(repository_url) = &server.source_grounding.repository_url {
        evidence.push(format!("source-repository-url: {repository_url}"));
    }

    for path in &server.source_grounding.inspected_paths {
        evidence.push(format!("source-inspected-path: {}", path.display()));
    }

    for url in &server.source_grounding.inspected_urls {
        evidence.push(format!("source-inspected-url: {url}"));
    }

    if server.source_grounding.evidence_level == SourceEvidenceLevel::RuntimeOnly {
        evidence.push("runtime metadata + contract test fallback".to_string());
    }

    evidence.sort();
    evidence.dedup();
    evidence
}

pub(crate) fn default_contract_tests(server: &MCPServerProfile) -> Vec<ToolContractTest> {
    let side_effect_optional = matches!(
        server.inferred_permission,
        PermissionLevel::ReadOnly | PermissionLevel::Unknown
    );

    vec![
        ToolContractTest {
            probe: "happy-path".to_string(),
            expected: "Produces valid output for a representative valid request.".to_string(),
            method: "Run with canonical valid inputs and assert output schema/fields.".to_string(),
            applicable: true,
        },
        ToolContractTest {
            probe: "invalid-input".to_string(),
            expected: "Returns deterministic validation or error response for malformed input."
                .to_string(),
            method: "Run with malformed/unsupported inputs and assert predictable failure path."
                .to_string(),
            applicable: true,
        },
        ToolContractTest {
            probe: "side-effect-safety".to_string(),
            expected: "Confirms dry-run or explicit confirmation guard before destructive actions."
                .to_string(),
            method: "Run safety check path first; ensure mutations are gated behind confirmation."
                .to_string(),
            applicable: !side_effect_optional,
        },
    ]
}

pub(crate) fn has_any_probe_inputs(probe_inputs: &ProbeInputs) -> bool {
    probe_inputs.happy_path.is_some()
        || probe_inputs.invalid_input.is_some()
        || probe_inputs.side_effect_safety.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ConversionRecommendation;

    #[test]
    fn source_ground_evidence_includes_source_metadata_and_level() {
        let server = MCPServerProfile {
            id: "fixture:playwright".to_string(),
            name: "playwright".to_string(),
            source_label: "fixture".to_string(),
            source_path: PathBuf::from("/tmp/settings.json"),
            purpose: "Browser automation".to_string(),
            command: Some("npx".to_string()),
            args: vec!["-y".to_string(), "@playwright/mcp@1.55.0".to_string()],
            url: None,
            env_keys: vec![],
            declared_tool_count: 1,
            permission_hints: vec![],
            inferred_permission: PermissionLevel::ReadOnly,
            recommendation: ConversionRecommendation::ReplaceCandidate,
            recommendation_reason: "read-only".to_string(),
            source_grounding: crate::SourceGrounding {
                kind: crate::SourceKind::NpmPackage,
                evidence_level: SourceEvidenceLevel::ConfigOnly,
                inspected: false,
                entrypoint: None,
                package_name: Some("@playwright/mcp".to_string()),
                package_version: Some("1.55.0".to_string()),
                homepage: Some("https://playwright.dev".to_string()),
                repository_url: Some("https://github.com/microsoft/playwright-mcp".to_string()),
                inspected_paths: vec![],
                inspected_urls: vec![],
                derivation_evidence: vec![],
            },
        };
        let tool = RuntimeTool {
            name: "navigate".to_string(),
            description: Some("Open pages".to_string()),
            input_schema: None,
        };

        let evidence = source_ground_evidence(&server, &tool);
        assert!(
            evidence
                .iter()
                .any(|item| item == "evidence-level: config-only")
        );
        assert!(
            evidence
                .iter()
                .any(|item| item == "source-package: @playwright/mcp@1.55.0")
        );
        assert!(
            evidence
                .iter()
                .any(|item| item == "source-homepage: https://playwright.dev")
        );
    }

    #[test]
    fn merge_source_grounding_evidence_preserves_backend_and_source_items() {
        let server = MCPServerProfile {
            id: "fixture:playwright".to_string(),
            name: "playwright".to_string(),
            source_label: "fixture".to_string(),
            source_path: PathBuf::from("/tmp/settings.json"),
            purpose: "Browser automation".to_string(),
            command: Some("npx".to_string()),
            args: vec!["-y".to_string(), "@playwright/mcp@latest".to_string()],
            url: None,
            env_keys: vec![],
            declared_tool_count: 1,
            permission_hints: vec![],
            inferred_permission: PermissionLevel::ReadOnly,
            recommendation: ConversionRecommendation::ReplaceCandidate,
            recommendation_reason: "read-only".to_string(),
            source_grounding: crate::SourceGrounding {
                kind: crate::SourceKind::NpmPackage,
                evidence_level: SourceEvidenceLevel::ConfigOnly,
                inspected: false,
                entrypoint: None,
                package_name: Some("@playwright/mcp".to_string()),
                package_version: Some("latest".to_string()),
                homepage: None,
                repository_url: None,
                inspected_paths: vec![],
                inspected_urls: vec![],
                derivation_evidence: vec![],
            },
        };
        let tool = RuntimeTool {
            name: "navigate".to_string(),
            description: None,
            input_schema: None,
        };

        let merged = merge_source_grounding_evidence(
            vec!["backend-summary: navigate".to_string()],
            &server,
            &tool,
        );
        assert!(
            merged
                .iter()
                .any(|item| item == "backend-summary: navigate")
        );
        assert!(
            merged
                .iter()
                .any(|item| item == "evidence-level: config-only")
        );
        assert!(
            merged
                .iter()
                .any(|item| item == "source-package: @playwright/mcp@latest")
        );
    }

    #[test]
    fn session_default_tools_do_not_require_standalone_workflows() {
        let session_tool = RuntimeTool {
            name: "session_show_defaults".to_string(),
            description: Some(
                "Show current active defaults. Required before your first build/run/test call in a session."
                    .to_string(),
            ),
            input_schema: None,
        };
        let capability_tool = RuntimeTool {
            name: "screenshot".to_string(),
            description: Some("Capture screenshot.".to_string()),
            input_schema: None,
        };

        assert!(!tool_needs_standalone_workflow(&session_tool));
        assert!(tool_needs_standalone_workflow(&capability_tool));
    }
}
