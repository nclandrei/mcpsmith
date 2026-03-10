use crate::{
    BuildResult, BuildServerResult, CapabilityPlaybook, ConvertPlan, DossierBundle,
    MCPServerProfile, ManifestToolSkill, ServerDossier, ServerGate, SkillParityManifest,
    ToolDossier, ToolSkillHint,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub fn build_from_bundle(
    bundle: &DossierBundle,
    skills_dir: Option<PathBuf>,
) -> Result<BuildResult> {
    for dossier in &bundle.dossiers {
        if dossier.server_gate == ServerGate::Blocked {
            let reasons = if dossier.gate_reasons.is_empty() {
                "no gate reason recorded".to_string()
            } else {
                dossier.gate_reasons.join(" | ")
            };
            bail!(
                "Cannot build standalone skills from blocked dossier '{}': {}",
                dossier.server.id,
                reasons
            );
        }
    }

    let skills_root = skills_dir.unwrap_or_else(default_agents_skills_dir);
    fs::create_dir_all(&skills_root)
        .with_context(|| format!("Failed to create skills dir {}", skills_root.display()))?;

    let mut servers = Vec::with_capacity(bundle.dossiers.len());
    for dossier in &bundle.dossiers {
        let (orchestrator, tool_paths, notes) = write_server_skills(dossier, &skills_root)?;
        servers.push(BuildServerResult {
            server_id: dossier.server.id.clone(),
            orchestrator_skill_path: orchestrator,
            tool_skill_paths: tool_paths,
            notes,
        });
    }

    Ok(BuildResult {
        generated_at: Utc::now(),
        skills_dir: skills_root,
        servers,
    })
}

pub(crate) fn write_server_skills(
    dossier: &ServerDossier,
    root: &Path,
) -> Result<(PathBuf, Vec<PathBuf>, Vec<String>)> {
    let server_slug = sanitize_slug(&dossier.server.name);
    let orchestrator_dir = root.join(&server_slug);
    let orchestrator_path = orchestrator_dir.join("SKILL.md");
    fs::create_dir_all(&orchestrator_dir)
        .with_context(|| format!("Failed to create {}", orchestrator_dir.display()))?;

    let mut tool_skill_paths = vec![];
    let mut tool_refs = vec![];
    let mut manifest_tool_skills = vec![];
    for tool in &dossier.tool_dossiers {
        let tool_slug = sanitize_slug(&tool.name);
        let dir_name = format!("{server_slug}--{tool_slug}");
        let path = root.join(&dir_name).join("SKILL.md");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        let body = render_tool_skill_markdown(dossier, tool);
        fs::write(&path, body)
            .with_context(|| format!("Failed to write tool skill {}", path.display()))?;
        tool_skill_paths.push(path.clone());
        tool_refs.push((tool.name.clone(), format!("../{dir_name}/SKILL.md")));
        manifest_tool_skills.push(ManifestToolSkill {
            tool_name: normalize_tool_name(&tool.name),
            skill_file: format!("../{dir_name}/SKILL.md"),
        });
    }

    let orchestrator = render_orchestrator_v3_markdown(dossier, &tool_refs);
    fs::write(&orchestrator_path, orchestrator).with_context(|| {
        format!(
            "Failed to write orchestrator skill {}",
            orchestrator_path.display()
        )
    })?;

    let manifest = SkillParityManifest {
        format_version: 2,
        generated_at: Utc::now(),
        server_id: dossier.server.id.clone(),
        server_name: dossier.server.name.clone(),
        orchestrator_skill: Some("SKILL.md".to_string()),
        required_tools: dossier
            .tool_dossiers
            .iter()
            .map(|tool| normalize_tool_name(&tool.name))
            .collect(),
        tool_skills: manifest_tool_skills,
        required_tool_hints: vec![],
    };
    write_skill_manifest(&orchestrator_path, &manifest)?;

    let mut notes = vec![format!(
        "Generated 1 orchestrator skill and {} tool skills.",
        tool_skill_paths.len()
    )];
    notes.push("Wrote internal parity manifest for verify checks.".to_string());
    if dossier.backend_fallback_used {
        notes.push("Backend fallback was used during dossier discovery.".to_string());
    }
    Ok((orchestrator_path, tool_skill_paths, notes))
}

fn render_orchestrator_v3_markdown(
    dossier: &ServerDossier,
    tool_refs: &[(String, String)],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Workflow: {}\n\n", dossier.server.name.trim()));
    out.push_str("## Purpose\n\n");
    out.push_str(&format!("{}\n\n", dossier.server.purpose.trim()));

    out.push_str("## Capability Skills\n\n");
    if tool_refs.is_empty() {
        out.push_str("- No tool skills available.\n\n");
    } else {
        for (tool, file) in tool_refs {
            let skill_name = installed_skill_reference_name(file);
            out.push_str(&format!("- `${skill_name}` for `{tool}`.\n"));
        }
        out.push('\n');
    }

    out.push_str("## Flow\n\n");
    out.push_str("1. Confirm user intent and choose the minimum capability skills needed.\n");
    out.push_str("2. Execute one tool skill at a time and keep outputs deterministic.\n");
    out.push_str("3. Validate outcomes after each step; stop on mismatch and report root cause.\n");
    out.push_str(
        "4. Ask for explicit confirmation before destructive or irreversible operations.\n\n",
    );

    if dossier.server_gate == ServerGate::Blocked {
        out.push_str("## Gate Status\n\n");
        out.push_str("Server conversion is currently blocked:\n");
        for reason in &dossier.gate_reasons {
            out.push_str(&format!("- {}\n", reason));
        }
    }

    out
}

fn render_tool_skill_markdown(dossier: &ServerDossier, tool: &ToolDossier) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Capability: {}\n\n", tool.name));
    out.push_str("## What It Does\n\n");
    out.push_str(&format!("{}\n\n", tool.explanation.trim()));

    out.push_str("## Recipe\n\n");
    if tool.recipe.is_empty() {
        out.push_str("1. Validate inputs and preconditions.\n");
        out.push_str("2. Execute the operation and capture output.\n");
        out.push_str("3. Validate result and report outcome.\n\n");
    } else {
        for (idx, step) in tool.recipe.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", idx + 1, step));
        }
        out.push('\n');
    }

    out.push_str("## Contract Tests\n\n");
    for test in &tool.contract_tests {
        let applicability = if test.applicable {
            "required"
        } else {
            "optional"
        };
        out.push_str(&format!(
            "- `{}` ({applicability}): {}. Method: {}\n",
            test.probe, test.expected, test.method
        ));
    }
    out.push('\n');

    out.push_str("## Evidence\n\n");
    if tool.evidence.is_empty() {
        out.push_str("- Runtime metadata + contract checks (source not available).\n");
    } else {
        for evidence in &tool.evidence {
            out.push_str(&format!("- {}\n", evidence));
        }
    }
    out.push('\n');

    out.push_str("## Confidence\n\n");
    out.push_str(&format!("- {:.2}\n\n", tool.confidence.clamp(0.0, 1.0)));

    out.push_str("## Scope\n\n");
    out.push_str(&format!(
        "- Generated from `{}` dossier entry.\n",
        dossier.server.id
    ));

    out
}

pub(crate) fn render_orchestrator_skill_markdown(
    plan: &ConvertPlan,
    tool_skills: &[ManifestToolSkill],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Workflow: {}\n\n", plan.server.name.trim()));
    out.push_str("## Purpose\n\n");
    out.push_str(&format!("{}\n\n", plan.server.purpose));

    out.push_str("## Capability Skills\n\n");
    if tool_skills.is_empty() {
        out.push_str("- No capability skills were generated.\n\n");
    } else {
        for skill in tool_skills {
            let skill_name = installed_skill_reference_name(&skill.skill_file);
            out.push_str(&format!(
                "- `${skill_name}`: Executes `{}` operations.\n",
                skill.tool_name
            ));
        }
        out.push('\n');
    }

    out.push_str("## Orchestration\n\n");
    out.push_str("1. Clarify the user goal and select the minimum capability skills needed.\n");
    out.push_str(
        "2. Execute capability skills in dependency order, one focused action at a time.\n",
    );
    out.push_str("3. After each capability run, validate output and decide next step.\n");
    out.push_str("4. Stop immediately on errors, report root cause, and suggest recovery.\n\n");

    out.push_str("## Guardrails\n\n");
    out.push_str(
        "- Keep explicit user confirmation before destructive or production-impacting steps.\n",
    );
    out.push_str(
        "- When behavior is unclear, inspect tool schemas or run a dry-run/check command first.\n",
    );
    if !plan.warnings.is_empty() {
        for warning in &plan.warnings {
            out.push_str(&format!("- {}\n", warning));
        }
    }

    out
}

pub(crate) fn render_capability_skill_markdown(
    server: &MCPServerProfile,
    tool_name: &str,
    description: Option<&str>,
    hint: Option<&ToolSkillHint>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Capability: {tool_name}\n\n"));
    out.push_str("## Purpose\n\n");
    if let Some(text) = description.map(str::trim).filter(|text| !text.is_empty()) {
        out.push_str(text);
        out.push_str("\n\n");
    } else {
        out.push_str(&format!(
            "Execute `{tool_name}` tasks for {}.\n\n",
            server.purpose.to_lowercase()
        ));
    }

    out.push_str("## Execution\n\n");
    out.push_str("1. Confirm prerequisites and collect only required inputs.\n");
    out.push_str(&format!(
        "2. Run the `{tool_name}` capability and capture raw output.\n"
    ));
    out.push_str("3. Validate errors, status, and key fields before continuing.\n");
    out.push_str("4. Return a concise result and next-step recommendation.\n\n");

    out.push_str("## Safety\n\n");
    out.push_str("- Ask for confirmation before destructive or irreversible actions.\n");
    out.push_str("- If arguments are unclear, run a non-destructive check first.\n");

    if let Some(hint) = hint {
        let has_hint = hint.what_it_does.is_some()
            || hint.when_to_use.is_some()
            || !hint.inputs_hint.is_empty()
            || !hint.success_signals.is_empty()
            || !hint.pitfalls.is_empty();
        if has_hint {
            out.push_str("\n## Optional Hints\n\n");
            if let Some(what_it_does) = &hint.what_it_does {
                out.push_str(&format!("- What it does: {what_it_does}\n"));
            }
            if let Some(when_to_use) = &hint.when_to_use {
                out.push_str(&format!("- When to use: {when_to_use}\n"));
            }
            if !hint.inputs_hint.is_empty() {
                out.push_str(&format!("- Input hints: {}\n", hint.inputs_hint.join("; ")));
            }
            if !hint.success_signals.is_empty() {
                out.push_str(&format!(
                    "- Success signals: {}\n",
                    hint.success_signals.join("; ")
                ));
            }
            if !hint.pitfalls.is_empty() {
                out.push_str(&format!("- Pitfalls: {}\n", hint.pitfalls.join("; ")));
            }
        }
    }

    out
}

fn capability_playbooks(
    server: &MCPServerProfile,
    fallback_actions: &[String],
    introspected_tools: Option<&[String]>,
) -> Vec<CapabilityPlaybook> {
    let name = server.name.to_lowercase();
    let purpose = server.purpose.clone();
    let introspected_set = introspected_tools
        .map(|items| {
            items
                .iter()
                .map(|item| normalize_tool_name(item))
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();

    if name.contains("xcodebuildmcp") || name.contains("xcode") {
        return vec![
            CapabilityPlaybook {
                title: "Build and launch in simulator".to_string(),
                goal: "Compile and run iOS code paths quickly during iteration.".to_string(),
                tool_hints: filter_hints_by_introspection(
                    vec![
                        "mcp__XcodeBuildMCP__build_run_sim".to_string(),
                        "mcp__XcodeBuildMCP__launch_app_sim".to_string(),
                        "mcp__XcodeBuildMCP__list_sims".to_string(),
                    ],
                    &introspected_set,
                ),
                steps: vec![
                    "List available simulators and choose target device/OS.".to_string(),
                    "Build and run in simulator with project defaults.".to_string(),
                    "Capture immediate app behavior and regressions before deeper debugging."
                        .to_string(),
                ],
            },
            CapabilityPlaybook {
                title: "UI interaction and visual checks".to_string(),
                goal: "Drive screens deterministically and confirm UI state.".to_string(),
                tool_hints: filter_hints_by_introspection(
                    vec![
                        "mcp__XcodeBuildMCP__snapshot_ui".to_string(),
                        "mcp__XcodeBuildMCP__tap".to_string(),
                        "mcp__XcodeBuildMCP__type_text".to_string(),
                        "mcp__XcodeBuildMCP__screenshot".to_string(),
                    ],
                    &introspected_set,
                ),
                steps: vec![
                    "Take a UI snapshot to identify accessible targets.".to_string(),
                    "Trigger interactions by accessibility id/label first, coordinates last."
                        .to_string(),
                    "Capture screenshots for before/after evidence of state transitions."
                        .to_string(),
                ],
            },
            CapabilityPlaybook {
                title: "Attach debugger and inspect failures".to_string(),
                goal: "Investigate crashes, stuck flows, and state mismatches.".to_string(),
                tool_hints: filter_hints_by_introspection(
                    vec![
                        "mcp__XcodeBuildMCP__debug_attach_sim".to_string(),
                        "mcp__XcodeBuildMCP__debug_stack".to_string(),
                        "mcp__XcodeBuildMCP__debug_variables".to_string(),
                        "mcp__XcodeBuildMCP__debug_lldb_command".to_string(),
                    ],
                    &introspected_set,
                ),
                steps: vec![
                    "Attach debugger to running app process.".to_string(),
                    "Collect backtrace and inspect frame variables at failure point.".to_string(),
                    "Apply fix and rerun the same flow to confirm closure.".to_string(),
                ],
            },
        ];
    }

    if name.contains("chrome-devtools") || name.contains("devtools") || name.contains("chrome") {
        return vec![
            CapabilityPlaybook {
                title: "Navigate and inspect page state".to_string(),
                goal: "Understand DOM/accessibility state before automation actions.".to_string(),
                tool_hints: filter_hints_by_introspection(
                    vec![
                        "mcp__chrome-devtools__navigate_page".to_string(),
                        "mcp__chrome-devtools__take_snapshot".to_string(),
                        "mcp__chrome-devtools__click".to_string(),
                        "mcp__chrome-devtools__fill".to_string(),
                    ],
                    &introspected_set,
                ),
                steps: vec![
                    "Open target URL and wait for primary content.".to_string(),
                    "Capture a text snapshot and locate stable element identifiers.".to_string(),
                    "Perform interactions and re-snapshot to validate results.".to_string(),
                ],
            },
            CapabilityPlaybook {
                title: "Trace network and console failures".to_string(),
                goal: "Root-cause runtime errors and bad responses.".to_string(),
                tool_hints: filter_hints_by_introspection(
                    vec![
                        "mcp__chrome-devtools__list_network_requests".to_string(),
                        "mcp__chrome-devtools__get_network_request".to_string(),
                        "mcp__chrome-devtools__list_console_messages".to_string(),
                        "mcp__chrome-devtools__get_console_message".to_string(),
                    ],
                    &introspected_set,
                ),
                steps: vec![
                    "List recent network requests and inspect failing responses.".to_string(),
                    "Collect console errors and correlate them with failing endpoints.".to_string(),
                    "Re-run interaction after fix to verify errors disappear.".to_string(),
                ],
            },
            CapabilityPlaybook {
                title: "Run performance diagnostics".to_string(),
                goal: "Capture page performance issues and actionable insights.".to_string(),
                tool_hints: filter_hints_by_introspection(
                    vec![
                        "mcp__chrome-devtools__performance_start_trace".to_string(),
                        "mcp__chrome-devtools__performance_stop_trace".to_string(),
                        "mcp__chrome-devtools__performance_analyze_insight".to_string(),
                    ],
                    &introspected_set,
                ),
                steps: vec![
                    "Start trace recording for the target journey.".to_string(),
                    "Stop trace and inspect key insights (latency, LCP breakdown, etc.)."
                        .to_string(),
                    "Prioritize fixes and rerun trace to measure impact.".to_string(),
                ],
            },
        ];
    }

    let mut steps = vec![
        "Confirm MCP server availability and auth prerequisites.".to_string(),
        format!(
            "Use MCP tools to execute {} tasks with explicit checks.",
            purpose
        ),
    ];
    steps.extend(fallback_actions.iter().cloned());

    let fallback_hints = if introspected_set.is_empty() {
        vec![]
    } else {
        let mut names = introspected_set.iter().cloned().collect::<Vec<_>>();
        names.sort();
        names
            .into_iter()
            .take(6)
            .map(|tool| format!("{}{}", tool_hint_prefix(server), tool))
            .collect::<Vec<_>>()
    };

    vec![CapabilityPlaybook {
        title: "General orchestration".to_string(),
        goal: format!(
            "Perform {} with reproducible sequencing.",
            purpose.to_lowercase()
        ),
        tool_hints: fallback_hints,
        steps,
    }]
}

fn filter_hints_by_introspection(
    hints: Vec<String>,
    introspected: &BTreeSet<String>,
) -> Vec<String> {
    if introspected.is_empty() {
        return hints;
    }
    hints
        .iter()
        .filter(|hint| introspected.contains(&hint_to_tool_name(hint)))
        .cloned()
        .collect::<Vec<_>>()
}

pub(crate) fn required_tool_hints(
    server: &MCPServerProfile,
    introspected_tools: Option<&[String]>,
) -> Vec<String> {
    let mut hints = capability_playbooks(server, &[], introspected_tools)
        .into_iter()
        .flat_map(|playbook| playbook.tool_hints)
        .collect::<Vec<_>>();
    hints.sort();
    hints.dedup();
    hints
}

pub(crate) fn required_tool_names(
    server: &MCPServerProfile,
    introspected_tools: Option<&[String]>,
) -> Vec<String> {
    if let Some(items) = introspected_tools {
        let mut names = items
            .iter()
            .map(|item| normalize_tool_name(item))
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        if !names.is_empty() {
            return names;
        }
    }

    let mut fallback = required_tool_hints(server, None)
        .iter()
        .map(|hint| hint_to_tool_name(hint))
        .collect::<Vec<_>>();
    fallback.sort();
    fallback.dedup();
    fallback
}

pub(crate) fn extract_tool_hints_from_skill(content: &str) -> Vec<String> {
    let mut hints = vec![];
    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("- `") || !trimmed.ends_with('`') {
            continue;
        }
        let value = trimmed.trim_start_matches("- `").trim_end_matches('`');
        if value.starts_with("mcp__") {
            hints.push(value.to_string());
        }
    }
    hints.sort();
    hints.dedup();
    hints
}

pub(crate) fn hint_to_tool_name(hint: &str) -> String {
    hint.rsplit("__").next().unwrap_or(hint).trim().to_string()
}

pub(crate) fn normalize_tool_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.starts_with("mcp__") {
        hint_to_tool_name(trimmed)
    } else {
        trimmed.to_string()
    }
}

pub(crate) fn manifest_path_for_skill(skill_path: &Path) -> Result<PathBuf> {
    let parent = skill_path
        .parent()
        .context("Skill path has no parent directory for manifest")?;
    if skill_path
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value == "SKILL.md")
    {
        return Ok(parent.join(".mcpsmith").join("manifest.json"));
    }
    let stem = skill_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("skill");
    Ok(parent
        .join(".mcpsmith-manifests")
        .join(format!("{stem}.json")))
}

pub(crate) fn write_skill_manifest(
    skill_path: &Path,
    manifest: &SkillParityManifest,
) -> Result<PathBuf> {
    let manifest_path = manifest_path_for_skill(skill_path)?;
    if let Some(dir) = manifest_path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
    }
    let body = serde_json::to_string_pretty(manifest).context("Failed to serialize manifest")?;
    std::fs::write(&manifest_path, format!("{body}\n"))
        .with_context(|| format!("Failed to write {}", manifest_path.display()))?;
    Ok(manifest_path)
}

pub(crate) fn sanitize_slug(input: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in input.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            out.push(lower);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "mcp-server".to_string()
    } else {
        trimmed
    }
}

pub(crate) fn default_agents_skills_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    home.join(".agents").join("skills")
}

fn installed_skill_reference_name(reference: &str) -> String {
    let trimmed = reference.trim_end_matches('/');
    if trimmed.ends_with("/SKILL.md") {
        return Path::new(trimmed)
            .parent()
            .and_then(|path| path.file_name())
            .and_then(|value| value.to_str())
            .unwrap_or("skill")
            .to_string();
    }

    trimmed
        .trim_end_matches(".md")
        .rsplit('/')
        .next()
        .unwrap_or(trimmed)
        .to_string()
}

fn tool_hint_prefix(server: &MCPServerProfile) -> String {
    let lower = server.name.to_lowercase();
    if lower.contains("xcodebuildmcp") {
        return "mcp__XcodeBuildMCP__".to_string();
    }
    if lower.contains("chrome-devtools") || lower.contains("devtools") {
        return "mcp__chrome-devtools__".to_string();
    }
    format!("mcp__{}__", server.name)
}
