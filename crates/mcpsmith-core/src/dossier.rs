use crate::backend::{generate_tool_dossiers, prepare_backend_context};
use crate::inventory::{discover, resolve_server};
use crate::runtime::introspect_tool_specs;
use crate::skillset::{build_from_bundle, normalize_tool_name};
use crate::{
    BackendContext, BuildResult, ConvertInventory, ConvertV3Options, DOSSIER_FORMAT_VERSION,
    DossierBundle, MCPServerProfile, PermissionLevel, ProbeInputSource, ProbeInputs, RuntimeTool,
    ServerDossier, ServerGate, SourceEvidenceLevel, ToolContractTest, ToolDossier,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
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
            "No runtime tools available; cannot build deterministic per-tool recipes.".to_string(),
        );
    }

    let mut selected_backend = backend_ctx.selection.selected;
    let mut fallback_used = false;

    let mut dossiers = vec![];
    if !runtime_tools.is_empty() {
        match generate_tool_dossiers(
            selected_backend,
            server,
            &runtime_tools,
            options.backend_config.chunk_size.max(1),
            options.backend_config.timeout_seconds,
        ) {
            Ok(generated) => dossiers = generated,
            Err(err) => {
                diagnostics.push(format!(
                    "Primary backend '{}' failed: {err}",
                    selected_backend
                ));
                if backend_ctx.selection.auto_mode {
                    if let Some(fallback) = backend_ctx.selection.fallback {
                        fallback_used = true;
                        selected_backend = fallback;
                        diagnostics.push(format!(
                            "Retrying dossier generation with fallback backend '{}'.",
                            fallback
                        ));
                        match generate_tool_dossiers(
                            fallback,
                            server,
                            &runtime_tools,
                            options.backend_config.chunk_size.max(1),
                            options.backend_config.timeout_seconds,
                        ) {
                            Ok(generated) => dossiers = generated,
                            Err(fallback_err) => {
                                diagnostics.push(format!(
                                    "Warning: backend dossier generation failed on primary and fallback; using runtime fallback dossiers. fallback_error={fallback_err}"
                                ));
                            }
                        }
                    } else {
                        diagnostics.push(format!(
                            "Warning: backend dossier generation failed and no fallback backend is available; using runtime fallback dossiers. error={err}"
                        ));
                    }
                } else {
                    diagnostics.push(format!(
                        "Warning: backend dossier generation failed; using runtime fallback dossiers. error={err}"
                    ));
                }
            }
        }
    }

    let runtime_map = runtime_tools
        .iter()
        .map(|tool| (normalize_tool_name(&tool.name), tool.clone()))
        .collect::<BTreeMap<_, _>>();

    let mut dossier_map = dossiers
        .into_iter()
        .map(|tool| (normalize_tool_name(&tool.name), tool))
        .collect::<BTreeMap<_, _>>();

    let mut tool_dossiers = vec![];
    for name in runtime_map.keys() {
        let mut dossier = dossier_map
            .remove(name)
            .unwrap_or_else(|| fallback_tool_dossier(server, runtime_map.get(name).unwrap()));
        normalize_contract_tests(&mut dossier, server);
        let runtime_tool = runtime_map.get(name).unwrap();
        dossier.evidence = merge_source_grounding_evidence(dossier.evidence, server, runtime_tool);
        if dossier.recipe.is_empty() {
            gate_reasons.push(format!("Tool '{}' has no executable recipe.", name));
        }
        if !has_required_probes(&dossier.contract_tests) {
            gate_reasons.push(format!(
                "Tool '{}' is missing required contract tests (happy-path, invalid-input, side-effect-safety).",
                name
            ));
        }
        tool_dossiers.push(dossier);
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
        tool_dossiers,
        server_gate,
        gate_reasons,
        backend_used: selected_backend.to_string(),
        backend_fallback_used: fallback_used,
        backend_diagnostics,
    })
}

pub(crate) fn fallback_tool_dossier(
    server: &MCPServerProfile,
    runtime_tool: &RuntimeTool,
) -> ToolDossier {
    ToolDossier {
        name: normalize_tool_name(&runtime_tool.name),
        explanation: runtime_tool.description.clone().unwrap_or_else(|| {
            format!(
                "Execute '{}' actions for {}.",
                runtime_tool.name, server.purpose
            )
        }),
        recipe: vec![
            "Validate required inputs against runtime tool schema before execution.".to_string(),
            "Run the tool once with deterministic arguments and capture raw output.".to_string(),
            "Validate status/output shape and return concise structured result.".to_string(),
        ],
        evidence: merge_source_grounding_evidence(
            vec!["fallback: runtime metadata + deterministic defaults".to_string()],
            server,
            runtime_tool,
        ),
        confidence: 0.5,
        contract_tests: default_contract_tests(server),
        probe_inputs: ProbeInputs::default(),
        probe_input_source: ProbeInputSource::Synthesized,
    }
}

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

pub(crate) fn normalize_contract_tests(dossier: &mut ToolDossier, server: &MCPServerProfile) {
    for test in &mut dossier.contract_tests {
        test.probe = test.probe.trim().to_ascii_lowercase();
        test.expected = test.expected.trim().to_string();
        test.method = test.method.trim().to_string();
        if test.expected.is_empty() {
            test.expected = "Expected behavior not provided by backend.".to_string();
        }
        if test.method.is_empty() {
            test.method = "Method not provided by backend.".to_string();
        }
    }

    if !has_required_probes(&dossier.contract_tests) {
        let mut defaults = default_contract_tests(server);
        let existing = dossier
            .contract_tests
            .iter()
            .map(|test| test.probe.clone())
            .collect::<BTreeSet<_>>();
        for default in defaults.drain(..) {
            if !existing.contains(&default.probe) {
                dossier.contract_tests.push(default);
            }
        }
    }

    dossier.confidence = dossier.confidence.clamp(0.0, 1.0);
    dossier.recipe.retain(|step| !step.trim().is_empty());
    dossier.evidence.retain(|item| !item.trim().is_empty());
    dossier
        .recipe
        .iter_mut()
        .for_each(|step| *step = step.trim().to_string());
    dossier
        .evidence
        .iter_mut()
        .for_each(|item| *item = item.trim().to_string());

    if dossier.probe_input_source == ProbeInputSource::Synthesized
        && has_any_probe_inputs(&dossier.probe_inputs)
    {
        dossier.probe_input_source = ProbeInputSource::Backend;
    }
}

pub(crate) fn has_required_probes(tests: &[ToolContractTest]) -> bool {
    let probes = tests
        .iter()
        .map(|test| test.probe.trim().to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    probes.contains("happy-path")
        && probes.contains("invalid-input")
        && probes.contains("side-effect-safety")
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
}
