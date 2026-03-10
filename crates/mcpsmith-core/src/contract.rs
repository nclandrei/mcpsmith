use crate::backend::clipped_preview;
use crate::dossier::load_dossier_bundle;
use crate::runtime::{execute_mcp_tool_probe, introspect_tool_specs, value_to_compact_json};
use crate::skillset::normalize_tool_name;
use crate::{
    ContractProbeResult, ContractServerReport, ContractTestOptions, ContractTestReport,
    ContractToolResult, DossierBundle, MCPServerProfile, PermissionLevel, ProbeErrorKind,
    ProbeFailure, ProbeInputSource, RuntimeTool, RuntimeValidationSpec, ServerGate, ToolDossier,
};
use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

pub fn contract_test_from_dossier_path(
    dossier_path: &Path,
    report_path: Option<&Path>,
    options: ContractTestOptions,
) -> Result<ContractTestReport> {
    let bundle = load_dossier_bundle(dossier_path)?;
    let report = contract_test_bundle(&bundle, options)?;
    if let Some(path) = report_path {
        let body = serde_json::to_string_pretty(&report)
            .context("Failed to serialize contract test report")?;
        fs::write(path, format!("{body}\n"))
            .with_context(|| format!("Failed to write {}", path.display()))?;
    }
    Ok(report)
}

pub fn contract_test_bundle(
    bundle: &DossierBundle,
    options: ContractTestOptions,
) -> Result<ContractTestReport> {
    let mut servers = Vec::with_capacity(bundle.dossiers.len());
    let mut all_passed = true;

    for dossier in &bundle.dossiers {
        let runtime_specs = introspect_tool_specs(&dossier.server);
        let (introspection_ok, runtime_map, mut reasons) = match runtime_specs {
            Ok(specs) => {
                let map = specs
                    .into_iter()
                    .map(|spec| {
                        let normalized = normalize_tool_name(&spec.name);
                        (
                            normalized.clone(),
                            RuntimeTool {
                                name: normalized,
                                description: spec.description,
                                input_schema: spec.input_schema,
                            },
                        )
                    })
                    .collect::<BTreeMap<_, _>>();
                (true, map, vec![])
            }
            Err(err) => {
                let map = dossier
                    .runtime_tools
                    .iter()
                    .map(|tool| (normalize_tool_name(&tool.name), tool.clone()))
                    .collect::<BTreeMap<_, _>>();
                (
                    false,
                    map,
                    vec![format!(
                        "Runtime introspection unavailable; falling back to dossier runtime metadata: {err}"
                    )],
                )
            }
        };
        let runtime_names = runtime_map.keys().cloned().collect::<BTreeSet<_>>();

        let validation_specs = if dossier.runtime_validations.is_empty() {
            dossier
                .tool_dossiers
                .iter()
                .map(runtime_validation_from_legacy_tool)
                .collect::<Vec<_>>()
        } else {
            dossier.runtime_validations.clone()
        };

        let mut missing_runtime_tools = vec![];
        let mut tools = Vec::with_capacity(validation_specs.len());
        let mut server_passed = dossier.server_gate == ServerGate::Ready;
        if dossier.server_gate == ServerGate::Blocked {
            reasons.push(format!(
                "Server gate is blocked: {}",
                dossier.gate_reasons.join(" | ")
            ));
        }

        for validation in &validation_specs {
            let normalized = normalize_tool_name(&validation.tool_name);
            if !runtime_names.contains(&normalized) {
                missing_runtime_tools.push(normalized.clone());
            }
            let runtime_spec = runtime_map.get(&normalized);
            let result = evaluate_tool_contract(
                &validation_as_tool_dossier(validation),
                &dossier.server,
                runtime_spec,
                &runtime_map,
                options,
            );
            if !result.passed {
                server_passed = false;
            }
            tools.push(result);
        }

        missing_runtime_tools.sort();
        missing_runtime_tools.dedup();
        if !missing_runtime_tools.is_empty() {
            server_passed = false;
            reasons.push(format!(
                "Missing runtime tools for contract coverage: {}",
                missing_runtime_tools.join(", ")
            ));
        }

        if !server_passed {
            all_passed = false;
        }

        servers.push(ContractServerReport {
            server_id: dossier.server.id.clone(),
            passed: server_passed,
            introspection_ok,
            missing_runtime_tools,
            tools,
            reasons,
        });
    }

    Ok(ContractTestReport {
        generated_at: Utc::now(),
        passed: all_passed,
        servers,
    })
}

fn runtime_validation_from_legacy_tool(tool: &ToolDossier) -> RuntimeValidationSpec {
    RuntimeValidationSpec {
        tool_name: normalize_tool_name(&tool.name),
        contract_tests: tool.contract_tests.clone(),
        probe_inputs: tool.probe_inputs.clone(),
        probe_input_source: tool.probe_input_source,
    }
}

fn validation_as_tool_dossier(validation: &RuntimeValidationSpec) -> ToolDossier {
    ToolDossier {
        name: validation.tool_name.clone(),
        explanation: String::new(),
        recipe: vec![],
        evidence: vec![],
        confidence: 0.0,
        contract_tests: validation.contract_tests.clone(),
        probe_inputs: validation.probe_inputs.clone(),
        probe_input_source: validation.probe_input_source,
    }
}

fn evaluate_tool_contract(
    tool: &ToolDossier,
    server: &MCPServerProfile,
    runtime_tool: Option<&RuntimeTool>,
    runtime_tools: &BTreeMap<String, RuntimeTool>,
    options: ContractTestOptions,
) -> ContractToolResult {
    let mut probes = vec![];
    let mut reasons = vec![];
    let tool_name = normalize_tool_name(&tool.name);

    let probe_map = tool
        .contract_tests
        .iter()
        .map(|test| (test.probe.trim().to_ascii_lowercase(), test))
        .collect::<BTreeMap<_, _>>();

    let required = ["happy-path", "invalid-input", "side-effect-safety"];
    let Some(runtime_tool) = runtime_tool else {
        for probe in required {
            probes.push(ContractProbeResult {
                probe: probe.to_string(),
                passed: false,
                skipped: false,
                executed: false,
                details: "Runtime tool is missing from tools/list introspection.".to_string(),
                request_args_preview: None,
                response_preview: None,
                duration_ms: None,
                error_kind: Some(ProbeErrorKind::Transport),
            });
        }
        reasons.push("Runtime tool is missing from tools/list introspection.".to_string());
        return ContractToolResult {
            tool: tool_name,
            passed: false,
            probes,
            reasons,
        };
    };

    let happy_probe_args = resolve_happy_probe_args(tool, runtime_tool);

    for probe in required {
        match probe_map.get(probe) {
            Some(test) if !test.applicable => {
                probes.push(ContractProbeResult {
                    probe: probe.to_string(),
                    passed: true,
                    skipped: true,
                    executed: false,
                    details: "Probe marked optional for this tool.".to_string(),
                    request_args_preview: None,
                    response_preview: None,
                    duration_ms: None,
                    error_kind: None,
                });
            }
            Some(test) => {
                let content_ok = !test.expected.trim().is_empty() && !test.method.trim().is_empty();
                if !content_ok {
                    probes.push(ContractProbeResult {
                        probe: probe.to_string(),
                        passed: false,
                        skipped: false,
                        executed: false,
                        details: "Probe is missing expected behavior or execution method."
                            .to_string(),
                        request_args_preview: None,
                        response_preview: None,
                        duration_ms: None,
                        error_kind: Some(ProbeErrorKind::SchemaGap),
                    });
                    reasons.push(format!("Probe '{probe}' is missing required details."));
                    continue;
                }

                let result = match probe {
                    "happy-path" => run_happy_probe(
                        server,
                        runtime_tools,
                        &tool_name,
                        &tool.name,
                        happy_probe_args.clone(),
                        options,
                    ),
                    "invalid-input" => run_invalid_probe(
                        server,
                        runtime_tool,
                        &tool_name,
                        tool,
                        &happy_probe_args,
                        options,
                    ),
                    "side-effect-safety" => run_side_effect_probe(
                        server,
                        runtime_tool,
                        &tool_name,
                        tool,
                        &happy_probe_args,
                        options,
                    ),
                    _ => unreachable!("required probe list is fixed"),
                };
                if !result.passed {
                    reasons.push(format!("Probe '{probe}' failed: {}", result.details));
                }
                probes.push(result);
            }
            None => {
                probes.push(ContractProbeResult {
                    probe: probe.to_string(),
                    passed: false,
                    skipped: false,
                    executed: false,
                    details: "Probe missing from tool dossier.".to_string(),
                    request_args_preview: None,
                    response_preview: None,
                    duration_ms: None,
                    error_kind: Some(ProbeErrorKind::SchemaGap),
                });
                reasons.push(format!("Probe '{probe}' is missing."));
            }
        }
    }

    let passed = probes.iter().all(|probe| probe.passed);

    ContractToolResult {
        tool: tool_name,
        passed,
        probes,
        reasons,
    }
}

fn run_happy_probe(
    server: &MCPServerProfile,
    runtime_tools: &BTreeMap<String, RuntimeTool>,
    tool_name: &str,
    original_tool_name: &str,
    happy_probe_args: std::result::Result<(Value, ProbeInputSource), ProbeFailure>,
    options: ContractTestOptions,
) -> ContractProbeResult {
    match happy_probe_args {
        Ok((args, source)) => {
            let baseline = execute_probe_expect_success(
                server,
                tool_name,
                "happy-path",
                args.clone(),
                source,
                options,
                false,
                &[],
            );
            if baseline.passed {
                return baseline;
            }

            let retryable_not_found = baseline.error_kind == Some(ProbeErrorKind::McpError)
                && baseline
                    .response_preview
                    .as_deref()
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .contains("not found");
            if !retryable_not_found {
                return baseline;
            }

            let setup_calls = derive_setup_calls(runtime_tools, original_tool_name, &args);
            if setup_calls.is_empty() {
                return baseline;
            }

            let retried = execute_probe_expect_success(
                server,
                tool_name,
                "happy-path",
                args,
                source,
                options,
                false,
                &setup_calls,
            );
            if retried.passed {
                return ContractProbeResult {
                    details: format!("{} (after deterministic setup prelude).", retried.details),
                    ..retried
                };
            }
            baseline
        }
        Err(err) => probe_failure_result("happy-path", false, None, err),
    }
}

fn run_invalid_probe(
    server: &MCPServerProfile,
    runtime_tool: &RuntimeTool,
    tool_name: &str,
    tool: &ToolDossier,
    happy_probe_args: &std::result::Result<(Value, ProbeInputSource), ProbeFailure>,
    options: ContractTestOptions,
) -> ContractProbeResult {
    let resolved = resolve_invalid_probe_args(tool, runtime_tool, happy_probe_args);
    match resolved {
        Ok((args, source)) => {
            execute_probe_expect_error(server, tool_name, "invalid-input", args, source, options)
        }
        Err(err) => probe_failure_result("invalid-input", false, None, err),
    }
}

fn run_side_effect_probe(
    server: &MCPServerProfile,
    runtime_tool: &RuntimeTool,
    tool_name: &str,
    tool: &ToolDossier,
    happy_probe_args: &std::result::Result<(Value, ProbeInputSource), ProbeFailure>,
    options: ContractTestOptions,
) -> ContractProbeResult {
    if !is_likely_side_effect_tool(server, tool, runtime_tool) {
        return ContractProbeResult {
            probe: "side-effect-safety".to_string(),
            passed: true,
            skipped: true,
            executed: false,
            details: "Tool classified as read-only/unknown; side-effect probe safely skipped."
                .to_string(),
            request_args_preview: None,
            response_preview: None,
            duration_ms: None,
            error_kind: None,
        };
    }

    if options.allow_side_effects {
        let explicit = tool
            .probe_inputs
            .side_effect_safety
            .clone()
            .map(|args| (args, ProbeInputSource::Backend))
            .or_else(|| {
                happy_probe_args
                    .as_ref()
                    .ok()
                    .map(|(args, source)| (args.clone(), *source))
            });
        let Some((args, source)) = explicit else {
            return probe_failure_result(
                "side-effect-safety",
                false,
                None,
                ProbeFailure {
                    kind: ProbeErrorKind::SchemaGap,
                    message:
                        "No explicit side-effect probe args were provided and happy-path args are unavailable."
                            .to_string(),
                    response_preview: None,
                },
            );
        };
        return execute_probe_expect_success(
            server,
            tool_name,
            "side-effect-safety",
            args,
            source,
            options,
            true,
            &[],
        );
    }

    let guarded = resolve_guarded_side_effect_args(tool, runtime_tool, happy_probe_args);
    match guarded {
        Ok((args, source)) => execute_probe_expect_success(
            server,
            tool_name,
            "side-effect-safety",
            args,
            source,
            options,
            false,
            &[],
        ),
        Err(err) => probe_failure_result("side-effect-safety", false, None, err),
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_probe_expect_success(
    server: &MCPServerProfile,
    tool_name: &str,
    probe: &str,
    args: Value,
    source: ProbeInputSource,
    options: ContractTestOptions,
    side_effectful: bool,
    setup_calls: &[(String, Value)],
) -> ContractProbeResult {
    let request_preview = Some(clipped_preview(&value_to_compact_json(&args), 220));
    match execute_mcp_tool_probe(server, tool_name, &args, options, setup_calls) {
        Ok(call) if !call.is_error => ContractProbeResult {
            probe: probe.to_string(),
            passed: true,
            skipped: false,
            executed: true,
            details: if side_effectful {
                format!(
                    "Probe executed successfully (source={}, side-effects allowed).",
                    probe_input_source_label(source)
                )
            } else {
                format!(
                    "Probe executed successfully (source={}).",
                    probe_input_source_label(source)
                )
            },
            request_args_preview: request_preview,
            response_preview: Some(call.response_preview),
            duration_ms: Some(call.duration_ms),
            error_kind: None,
        },
        Ok(call) => ContractProbeResult {
            probe: probe.to_string(),
            passed: false,
            skipped: false,
            executed: true,
            details: format!("Tool returned an error path: {}", call.details),
            request_args_preview: request_preview,
            response_preview: Some(call.response_preview),
            duration_ms: Some(call.duration_ms),
            error_kind: Some(ProbeErrorKind::McpError),
        },
        Err(err) => probe_failure_result(probe, true, request_preview, err),
    }
}

fn execute_probe_expect_error(
    server: &MCPServerProfile,
    tool_name: &str,
    probe: &str,
    args: Value,
    source: ProbeInputSource,
    options: ContractTestOptions,
) -> ContractProbeResult {
    let request_preview = Some(clipped_preview(&value_to_compact_json(&args), 220));
    match execute_mcp_tool_probe(server, tool_name, &args, options, &[]) {
        Ok(call) if call.is_error => ContractProbeResult {
            probe: probe.to_string(),
            passed: true,
            skipped: false,
            executed: true,
            details: format!(
                "Probe produced expected error path (source={}).",
                probe_input_source_label(source)
            ),
            request_args_preview: request_preview,
            response_preview: Some(call.response_preview),
            duration_ms: Some(call.duration_ms),
            error_kind: None,
        },
        Ok(call) => ContractProbeResult {
            probe: probe.to_string(),
            passed: false,
            skipped: false,
            executed: true,
            details: "Expected invalid-input probe to fail, but it succeeded.".to_string(),
            request_args_preview: request_preview,
            response_preview: Some(call.response_preview),
            duration_ms: Some(call.duration_ms),
            error_kind: Some(ProbeErrorKind::McpError),
        },
        Err(err) => probe_failure_result(probe, true, request_preview, err),
    }
}

fn probe_failure_result(
    probe: &str,
    executed: bool,
    request_preview: Option<String>,
    err: ProbeFailure,
) -> ContractProbeResult {
    ContractProbeResult {
        probe: probe.to_string(),
        passed: false,
        skipped: false,
        executed,
        details: err.message,
        request_args_preview: request_preview,
        response_preview: err.response_preview,
        duration_ms: None,
        error_kind: Some(err.kind),
    }
}

fn probe_input_source_label(source: ProbeInputSource) -> &'static str {
    match source {
        ProbeInputSource::Backend => "backend",
        ProbeInputSource::Synthesized => "synthesized",
    }
}

fn resolve_happy_probe_args(
    tool: &ToolDossier,
    runtime_tool: &RuntimeTool,
) -> std::result::Result<(Value, ProbeInputSource), ProbeFailure> {
    if let Some(args) = &tool.probe_inputs.happy_path {
        return Ok((args.clone(), ProbeInputSource::Backend));
    }
    let schema = runtime_tool
        .input_schema
        .as_ref()
        .ok_or_else(|| ProbeFailure {
            kind: ProbeErrorKind::SchemaGap,
            message: "Missing runtime input schema and no backend happy-path args available."
                .to_string(),
            response_preview: None,
        })?;
    let args = synthesize_happy_args(schema).ok_or_else(|| ProbeFailure {
        kind: ProbeErrorKind::SchemaGap,
        message: "Unable to synthesize deterministic happy-path args from runtime input schema."
            .to_string(),
        response_preview: None,
    })?;
    Ok((args, ProbeInputSource::Synthesized))
}

fn resolve_invalid_probe_args(
    tool: &ToolDossier,
    runtime_tool: &RuntimeTool,
    happy_probe_args: &std::result::Result<(Value, ProbeInputSource), ProbeFailure>,
) -> std::result::Result<(Value, ProbeInputSource), ProbeFailure> {
    if let Some(args) = &tool.probe_inputs.invalid_input {
        return Ok((args.clone(), ProbeInputSource::Backend));
    }
    let schema = runtime_tool
        .input_schema
        .as_ref()
        .ok_or_else(|| ProbeFailure {
            kind: ProbeErrorKind::SchemaGap,
            message: "Missing runtime input schema and no backend invalid-input args available."
                .to_string(),
            response_preview: None,
        })?;
    let fallback_happy = happy_probe_args
        .as_ref()
        .ok()
        .map(|(args, _)| args.clone())
        .or_else(|| synthesize_happy_args(schema))
        .ok_or_else(|| ProbeFailure {
            kind: ProbeErrorKind::SchemaGap,
            message: "Unable to derive base args for invalid-input synthesis.".to_string(),
            response_preview: None,
        })?;
    let invalid = synthesize_invalid_args(schema, &fallback_happy).ok_or_else(|| ProbeFailure {
        kind: ProbeErrorKind::SchemaGap,
        message: "Unable to synthesize deterministic invalid-input args from runtime schema."
            .to_string(),
        response_preview: None,
    })?;
    Ok((invalid, ProbeInputSource::Synthesized))
}

fn resolve_guarded_side_effect_args(
    tool: &ToolDossier,
    runtime_tool: &RuntimeTool,
    happy_probe_args: &std::result::Result<(Value, ProbeInputSource), ProbeFailure>,
) -> std::result::Result<(Value, ProbeInputSource), ProbeFailure> {
    if let Some(args) = &tool.probe_inputs.side_effect_safety {
        if has_non_mutating_guard(args) {
            return Ok((args.clone(), ProbeInputSource::Backend));
        }
        return Err(ProbeFailure {
            kind: ProbeErrorKind::Unsafe,
            message:
                "Provided side-effect-safety probe args do not include required non-mutating guards."
                    .to_string(),
            response_preview: None,
        });
    }

    if let Ok((happy_args, source)) = happy_probe_args
        && let Some(guarded) = apply_non_mutating_guards(happy_args)
    {
        return Ok((guarded, *source));
    }

    if let Some(schema) = &runtime_tool.input_schema
        && let Some(mut guarded) = synthesize_happy_args(schema)
        && let Some(with_guards) = apply_non_mutating_guards(&guarded)
    {
        guarded = with_guards;
        return Ok((guarded, ProbeInputSource::Synthesized));
    }

    Err(ProbeFailure {
        kind: ProbeErrorKind::Unsafe,
        message: "No safe guard path found for side-effect probe (dry_run/check/preview/noop/confirm=false/force=false).".to_string(),
        response_preview: None,
    })
}

fn derive_setup_calls(
    runtime_tools: &BTreeMap<String, RuntimeTool>,
    tool_name: &str,
    args: &Value,
) -> Vec<(String, Value)> {
    let normalized = normalize_tool_name(tool_name).to_ascii_lowercase();
    let mut calls = vec![];

    if runtime_tools.contains_key("create_entities") {
        let entity_names = collect_entity_names(args);
        if !entity_names.is_empty()
            && (normalized.contains("observation")
                || normalized.contains("relation")
                || normalized.contains("entity"))
        {
            let entities = entity_names
                .into_iter()
                .map(|name| {
                    serde_json::json!({
                        "name": name,
                        "entityType": "sample",
                        "observations": ["sample"]
                    })
                })
                .collect::<Vec<_>>();
            calls.push((
                "create_entities".to_string(),
                serde_json::json!({ "entities": entities }),
            ));
        }
    }

    calls
}

fn collect_entity_names(root: &Value) -> Vec<String> {
    let mut out = BTreeSet::new();
    collect_entity_names_recursive(root, &mut out);
    out.into_iter().collect()
}

fn collect_entity_names_recursive(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, entry) in map {
                let key_lower = key.to_ascii_lowercase();
                if matches!(key_lower.as_str(), "entityname" | "name" | "from" | "to")
                    && let Some(name) = entry.as_str()
                {
                    let trimmed = name.trim();
                    if !trimmed.is_empty() {
                        out.insert(trimmed.to_string());
                    }
                }
                collect_entity_names_recursive(entry, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_entity_names_recursive(item, out);
            }
        }
        _ => {}
    }
}

fn is_likely_side_effect_tool(
    server: &MCPServerProfile,
    tool: &ToolDossier,
    runtime_tool: &RuntimeTool,
) -> bool {
    if matches!(
        server.inferred_permission,
        PermissionLevel::ReadOnly | PermissionLevel::Unknown
    ) {
        return false;
    }

    let mut text = tool.name.to_ascii_lowercase();
    text.push(' ');
    text.push_str(&tool.explanation.to_ascii_lowercase());
    if let Some(description) = &runtime_tool.description {
        text.push(' ');
        text.push_str(&description.to_ascii_lowercase());
    }

    const MUTATING: &[&str] = &[
        "create",
        "update",
        "delete",
        "remove",
        "write",
        "set",
        "apply",
        "install",
        "uninstall",
        "push",
        "commit",
        "merge",
        "stop",
        "start",
        "run",
        "execute",
        "launch",
    ];
    const READ_ONLY: &[&str] = &[
        "list",
        "get",
        "read",
        "fetch",
        "query",
        "search",
        "find",
        "inspect",
        "show",
        "status",
        "snapshot",
        "screenshot",
    ];

    let mutating = MUTATING.iter().any(|token| text.contains(token));
    let read_only = READ_ONLY.iter().any(|token| text.contains(token));
    if mutating {
        return true;
    }

    match server.inferred_permission {
        PermissionLevel::Destructive => true,
        PermissionLevel::Write => !read_only,
        PermissionLevel::ReadOnly | PermissionLevel::Unknown => false,
    }
}

fn has_non_mutating_guard(args: &Value) -> bool {
    let Some(map) = args.as_object() else {
        return false;
    };

    let true_guards = ["dry_run", "dryRun", "check", "preview", "noop", "no_op"];
    if true_guards
        .iter()
        .any(|key| map.get(*key).and_then(Value::as_bool) == Some(true))
    {
        return true;
    }

    let false_guards = ["confirm", "force"];
    false_guards
        .iter()
        .any(|key| map.get(*key).and_then(Value::as_bool) == Some(false))
}

fn apply_non_mutating_guards(args: &Value) -> Option<Value> {
    let mut map = args.as_object()?.clone();
    let mut touched = false;

    for key in ["dry_run", "dryRun", "check", "preview", "noop", "no_op"] {
        if map.contains_key(key) {
            map.insert(key.to_string(), Value::Bool(true));
            touched = true;
        }
    }
    for key in ["confirm", "force"] {
        if map.contains_key(key) {
            map.insert(key.to_string(), Value::Bool(false));
            touched = true;
        }
    }

    touched.then_some(Value::Object(map))
}

fn synthesize_happy_args(schema: &Value) -> Option<Value> {
    if let Some(constant) = schema.get("const") {
        return Some(constant.clone());
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array)
        && let Some(first) = values.first()
    {
        return Some(first.clone());
    }
    if let Some(default) = schema.get("default") {
        return Some(default.clone());
    }
    if let Some(branch) = schema
        .get("anyOf")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .or_else(|| {
            schema
                .get("oneOf")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
        })
        .or_else(|| {
            schema
                .get("allOf")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
        })
    {
        return synthesize_happy_args(branch);
    }

    let kinds = schema_type_kinds(schema);
    if kinds.iter().any(|kind| kind == "object") || schema.get("properties").is_some() {
        let mut out = serde_json::Map::new();
        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();

        for field in required {
            let Some(name) = field.as_str() else {
                continue;
            };
            let value = properties
                .get(name)
                .and_then(synthesize_happy_args)
                .unwrap_or_else(|| Value::String("sample".to_string()));
            out.insert(name.to_string(), value);
        }
        return Some(Value::Object(out));
    }

    if kinds.iter().any(|kind| kind == "array") {
        if let Some(items) = schema.get("items") {
            let value = synthesize_happy_args(items)?;
            return Some(Value::Array(vec![value]));
        }
        return Some(Value::Array(vec![]));
    }
    if kinds.iter().any(|kind| kind == "string") {
        return Some(Value::String("sample".to_string()));
    }
    if kinds.iter().any(|kind| kind == "integer") {
        return Some(Value::Number(1_i64.into()));
    }
    if kinds.iter().any(|kind| kind == "number") {
        return serde_json::Number::from_f64(1.0).map(Value::Number);
    }
    if kinds.iter().any(|kind| kind == "boolean") {
        return Some(Value::Bool(true));
    }
    if kinds.iter().any(|kind| kind == "null") {
        return Some(Value::Null);
    }

    None
}

fn synthesize_invalid_args(schema: &Value, happy_args: &Value) -> Option<Value> {
    let kinds = schema_type_kinds(schema);

    if (kinds.iter().any(|kind| kind == "object") || schema.get("properties").is_some())
        && let Some(mut obj) = happy_args.as_object().cloned()
    {
        if let Some(required) = schema.get("required").and_then(Value::as_array)
            && let Some(first_required) = required.first().and_then(Value::as_str)
            && obj.remove(first_required).is_some()
        {
            return Some(Value::Object(obj));
        }

        if let Some((name, property_schema)) = schema
            .get("properties")
            .and_then(Value::as_object)
            .and_then(|properties| {
                properties
                    .iter()
                    .next()
                    .map(|(name, schema)| (name.as_str(), schema))
            })
        {
            let wrong = wrong_type_value(schema_type_kinds(property_schema).as_slice());
            obj.insert(name.to_string(), wrong);
            return Some(Value::Object(obj));
        }
    }

    Some(wrong_type_value(kinds.as_slice()))
}

fn wrong_type_value(expected: &[String]) -> Value {
    if expected.iter().any(|kind| kind == "string") {
        return Value::Bool(true);
    }
    if expected
        .iter()
        .any(|kind| kind == "integer" || kind == "number")
    {
        return Value::String("invalid".to_string());
    }
    if expected.iter().any(|kind| kind == "boolean") {
        return Value::String("invalid".to_string());
    }
    if expected.iter().any(|kind| kind == "array") {
        return Value::Object(serde_json::Map::new());
    }
    if expected.iter().any(|kind| kind == "object") {
        return Value::String("invalid".to_string());
    }
    Value::Null
}

fn schema_type_kinds(schema: &Value) -> Vec<String> {
    match schema.get("type") {
        Some(Value::String(kind)) => vec![kind.to_ascii_lowercase()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(|kind| kind.to_ascii_lowercase())
            .collect::<Vec<_>>(),
        _ => vec![],
    }
}
