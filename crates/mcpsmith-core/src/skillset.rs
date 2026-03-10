use crate::{
    BuildResult, BuildServerResult, CapabilityPlaybook, ConvertPlan, DossierBundle,
    MCPServerProfile, ManifestToolSkill, ServerDossier, ServerGate, SkillParityManifest,
    ToolSkillHint, WorkflowSkillSpec,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

pub fn build_from_bundle(
    bundle: &DossierBundle,
    skills_dir: Option<PathBuf>,
) -> Result<BuildResult> {
    for dossier in &bundle.dossiers {
        if dossier.workflow_skills.is_empty() && !dossier.tool_dossiers.is_empty() {
            bail!(
                "Legacy tool-dossier bundle '{}' cannot build standalone skills; re-run `mcpsmith discover` with the current version.",
                dossier.server.id
            );
        }
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
    let mut workflow_files = BTreeMap::new();
    let mut manifest_tool_skills = vec![];
    let mut slug_counts = BTreeMap::<String, usize>::new();
    for workflow in &dossier.workflow_skills {
        let base_slug = sanitize_slug(&workflow.id);
        let counter = slug_counts.entry(base_slug.clone()).or_insert(0);
        let workflow_slug = if *counter == 0 {
            base_slug.clone()
        } else {
            format!("{base_slug}-{}", *counter + 1)
        };
        *counter += 1;

        let dir_name = format!("{server_slug}--{workflow_slug}");
        let path = root.join(&dir_name).join("SKILL.md");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        let body = render_workflow_skill_markdown(dossier, workflow);
        fs::write(&path, body)
            .with_context(|| format!("Failed to write workflow skill {}", path.display()))?;
        tool_skill_paths.push(path.clone());
        workflow_files.insert(workflow.id.clone(), format!("../{dir_name}/SKILL.md"));

        let coverage = if workflow.origin_tools.is_empty() {
            vec![workflow.id.clone()]
        } else {
            workflow.origin_tools.clone()
        };
        for tool_name in coverage {
            manifest_tool_skills.push(ManifestToolSkill {
                tool_name: normalize_tool_name(&tool_name),
                skill_file: format!("../{dir_name}/SKILL.md"),
            });
        }
    }

    let orchestrator = render_orchestrator_v3_markdown(dossier, &workflow_files);
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
            .runtime_tools
            .iter()
            .map(|tool| normalize_tool_name(&tool.name))
            .collect(),
        tool_skills: manifest_tool_skills,
        required_tool_hints: vec![],
    };
    write_skill_manifest(&orchestrator_path, &manifest)?;

    let mut notes = vec![format!(
        "Generated 1 orchestrator skill and {} workflow skills.",
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
    workflow_files: &BTreeMap<String, String>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Skill: {}\n\n", dossier.server.name.trim()));
    out.push_str("## Use This When\n\n");
    out.push_str(&format!("{}\n\n", dossier.server.purpose.trim()));

    out.push_str("## How To Route Requests\n\n");
    out.push_str(
        "1. Match the user request to the workflow whose goal and trigger phrases fit best.\n",
    );
    out.push_str("2. If required context is missing, use the workflow's context-acquisition guidance before running commands.\n");
    out.push_str(
        "3. If a workflow lists prerequisite workflows, run those sibling workflows first.\n",
    );
    out.push_str("4. Finish by following the workflow's verification and return-contract sections before reporting success.\n\n");

    out.push_str("## Workflow Skills\n\n");
    if dossier.workflow_skills.is_empty() {
        out.push_str("- No standalone workflows are available.\n\n");
    } else {
        for workflow in &dossier.workflow_skills {
            let file = workflow_files
                .get(&workflow.id)
                .map(String::as_str)
                .unwrap_or("SKILL.md");
            let skill_name = installed_skill_reference_name(file);
            out.push_str(&format!(
                "- `${skill_name}`: {}\n",
                concise_sentence(&workflow.when_to_use)
            ));
        }
        out.push('\n');
    }

    out.push_str("## Shared Guardrails\n\n");
    out.push_str("- Do not guess missing identifiers, paths, or simulator/project context.\n");
    out.push_str("- Prefer the concrete native commands written in each workflow over improvising a different procedure.\n");
    out.push_str("- Stop and ask the user before destructive or irreversible steps.\n\n");

    if dossier.server_gate == ServerGate::Blocked {
        out.push_str("## Gate Status\n\n");
        out.push_str("Standalone conversion is currently blocked:\n");
        for reason in &dossier.gate_reasons {
            out.push_str(&format!("- {}\n", reason));
        }
    }

    out
}

fn render_workflow_skill_markdown(dossier: &ServerDossier, workflow: &WorkflowSkillSpec) -> String {
    let server_slug = sanitize_slug(&dossier.server.name);
    let mut out = String::new();
    out.push_str(&format!("# Skill: {}\n\n", workflow.title.trim()));
    out.push_str("## Use This When\n\n");
    out.push_str(&format!("{}\n\n", workflow.when_to_use.trim()));

    out.push_str("## Goal\n\n");
    out.push_str(&format!("{}\n\n", workflow.goal.trim()));

    if !workflow.trigger_phrases.is_empty() {
        out.push_str("## Trigger Phrases\n\n");
        for phrase in &workflow.trigger_phrases {
            out.push_str(&format!("- {}\n", phrase));
        }
        out.push('\n');
    }

    if !workflow.required_context.is_empty() {
        out.push_str("## Required Context\n\n");
        for input in &workflow.required_context {
            out.push_str(&format!(
                "- `{}`: {}\n",
                input.name.trim(),
                concise_sentence(&input.guidance)
            ));
        }
        out.push('\n');
    }

    if !workflow.context_acquisition.is_empty() {
        out.push_str("## If Context Is Missing\n\n");
        for item in &workflow.context_acquisition {
            out.push_str(&format!("- {}\n", item));
        }
        out.push('\n');
    }

    if !workflow.prerequisite_workflows.is_empty() || !workflow.followup_workflows.is_empty() {
        out.push_str("## Related Skills\n\n");
        for prerequisite in &workflow.prerequisite_workflows {
            out.push_str(&format!(
                "- Prerequisite: `${server_slug}--{}`\n",
                sanitize_slug(prerequisite)
            ));
        }
        for followup in &workflow.followup_workflows {
            out.push_str(&format!(
                "- Follow-up: `${server_slug}--{}`\n",
                sanitize_slug(followup)
            ));
        }
        out.push('\n');
    }

    out.push_str("## Native Steps\n\n");
    for (idx, step) in workflow.native_steps.iter().enumerate() {
        out.push_str(&format!("{}. {}\n\n", idx + 1, step.title.trim()));
        out.push_str("```bash\n");
        out.push_str(step.command.trim());
        out.push_str("\n```\n");
        if let Some(details) = step
            .details
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            out.push_str(details);
            out.push('\n');
        }
        out.push('\n');
    }

    if !workflow.branching_rules.is_empty() {
        out.push_str("## Branching Rules\n\n");
        for rule in &workflow.branching_rules {
            out.push_str(&format!("- {}\n", rule));
        }
        out.push('\n');
    }

    out.push_str("## Verification\n\n");
    for item in &workflow.verification {
        out.push_str(&format!("- {}\n", item));
    }
    out.push('\n');

    out.push_str("## What To Return\n\n");
    for item in &workflow.return_contract {
        out.push_str(&format!("- {}\n", item));
    }
    out.push('\n');

    out.push_str("## Stop And Ask\n\n");
    for item in &workflow.stop_and_ask {
        out.push_str(&format!("- {}\n", item));
    }
    out.push('\n');

    out.push_str("## Guardrails\n\n");
    if workflow.guardrails.is_empty() {
        out.push_str("- Keep the workflow deterministic and avoid destructive improvisation.\n");
    } else {
        for item in &workflow.guardrails {
            out.push_str(&format!("- {}\n", item));
        }
    }
    out.push('\n');

    out.push_str("## Evidence\n\n");
    for evidence in &workflow.evidence {
        out.push_str(&format!("- {}\n", evidence));
    }
    out.push('\n');

    out.push_str("## Confidence\n\n");
    out.push_str(&format!("- {:.2}\n", workflow.confidence.clamp(0.0, 1.0)));

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
    let purpose = server.purpose.clone();
    let introspected_set = introspected_tools
        .map(|items| {
            items
                .iter()
                .map(|item| normalize_tool_name(item))
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();

    let mut steps = vec![
        format!(
            "Identify the concrete workflow needed for {}.",
            purpose.to_lowercase()
        ),
        "Collect only the context the selected workflow actually needs.".to_string(),
        "Run the selected workflow deterministically and verify the result before moving on."
            .to_string(),
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

fn concise_sentence(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.ends_with('.') || trimmed.ends_with('!') || trimmed.ends_with('?') {
        trimmed.to_string()
    } else {
        format!("{trimmed}.")
    }
}

fn tool_hint_prefix(server: &MCPServerProfile) -> String {
    format!("mcp__{}__", server.name)
}
