use crate::{
    BuildResult, BuildServerResult, HelperScript, ManifestToolSkill, ServerConversionBundle,
    SkillParityManifest, WorkflowSkillSpec,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub fn build_from_bundle(
    bundle: &ServerConversionBundle,
    skills_dir: Option<PathBuf>,
) -> Result<BuildResult> {
    if bundle.blocked {
        let reasons = if bundle.block_reasons.is_empty() {
            "no block reason recorded".to_string()
        } else {
            bundle.block_reasons.join(" | ")
        };
        bail!(
            "Cannot build standalone skills from blocked review bundle '{}': {}",
            bundle.evidence.server.id,
            reasons
        );
    }

    let skills_root = skills_dir.unwrap_or_else(default_agents_skills_dir);
    fs::create_dir_all(&skills_root)
        .with_context(|| format!("Failed to create skills dir {}", skills_root.display()))?;

    let (orchestrator, tool_paths, notes) = write_server_skills(bundle, &skills_root)?;
    Ok(BuildResult {
        generated_at: Utc::now(),
        skills_dir: skills_root,
        servers: vec![BuildServerResult {
            server_id: bundle.evidence.server.id.clone(),
            orchestrator_skill_path: orchestrator,
            tool_skill_paths: tool_paths,
            notes,
        }],
    })
}

pub(crate) fn write_server_skills(
    bundle: &ServerConversionBundle,
    root: &Path,
) -> Result<(PathBuf, Vec<PathBuf>, Vec<String>)> {
    let server = &bundle.evidence.server;
    let server_slug = sanitize_slug(&server.name);
    let orchestrator_dir = root.join(&server_slug);
    let orchestrator_path = orchestrator_dir.join("SKILL.md");
    fs::create_dir_all(&orchestrator_dir)
        .with_context(|| format!("Failed to create {}", orchestrator_dir.display()))?;

    let mut tool_skill_paths = vec![];
    let mut workflow_files = BTreeMap::new();
    let mut manifest_tool_skills = vec![];
    let mut slug_counts = BTreeMap::<String, usize>::new();

    for draft in &bundle.tool_conversions {
        let workflow = &draft.workflow_skill;
        let base_slug = sanitize_slug(&workflow.id);
        let counter = slug_counts.entry(base_slug.clone()).or_insert(0);
        let workflow_slug = if *counter == 0 {
            base_slug.clone()
        } else {
            format!("{base_slug}-{}", *counter + 1)
        };
        *counter += 1;

        let dir_name = format!("{server_slug}--{workflow_slug}");
        let tool_dir = root.join(&dir_name);
        let skill_path = tool_dir.join("SKILL.md");
        fs::create_dir_all(&tool_dir)
            .with_context(|| format!("Failed to create {}", tool_dir.display()))?;

        for helper in &draft.helper_scripts {
            write_helper_script(&tool_dir, helper)?;
        }

        let body = render_workflow_skill_markdown(&server.name, workflow);
        fs::write(&skill_path, body)
            .with_context(|| format!("Failed to write workflow skill {}", skill_path.display()))?;
        tool_skill_paths.push(skill_path.clone());
        workflow_files.insert(workflow.id.clone(), format!("../{dir_name}/SKILL.md"));

        let coverage = if workflow.origin_tools.is_empty() {
            vec![draft.tool_name.clone()]
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

    let orchestrator = render_orchestrator_skill_markdown(bundle, &workflow_files);
    fs::write(&orchestrator_path, orchestrator).with_context(|| {
        format!(
            "Failed to write orchestrator skill {}",
            orchestrator_path.display()
        )
    })?;

    let manifest = SkillParityManifest {
        format_version: 2,
        generated_at: Utc::now(),
        server_id: server.id.clone(),
        server_name: server.name.clone(),
        orchestrator_skill: Some("SKILL.md".to_string()),
        required_tools: bundle
            .evidence
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
    if bundle.backend_fallback_used {
        notes.push("Backend fallback was used during synthesis or review.".to_string());
    }
    Ok((orchestrator_path, tool_skill_paths, notes))
}

fn write_helper_script(tool_dir: &Path, helper: &HelperScript) -> Result<PathBuf> {
    if helper.relative_path.is_absolute() {
        bail!(
            "Helper script path '{}' must be relative.",
            helper.relative_path.display()
        );
    }
    let path = tool_dir.join(&helper.relative_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    fs::write(&path, &helper.body)
        .with_context(|| format!("Failed to write helper script {}", path.display()))?;
    #[cfg(unix)]
    if helper.executable {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(&path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions)
            .with_context(|| format!("Failed to chmod {}", path.display()))?;
    }
    Ok(path)
}

fn render_orchestrator_skill_markdown(
    bundle: &ServerConversionBundle,
    workflow_files: &BTreeMap<String, String>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Skill: {}\n\n",
        bundle.evidence.server.name.trim()
    ));
    out.push_str("## Use This When\n\n");
    out.push_str(&format!("{}\n\n", bundle.evidence.server.purpose.trim()));

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
    if bundle.tool_conversions.is_empty() {
        out.push_str("- No standalone workflows are available.\n\n");
    } else {
        for draft in &bundle.tool_conversions {
            let workflow = &draft.workflow_skill;
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
    out.push_str("- Stop and ask the user before destructive or irreversible steps.\n");
    if bundle.backend_fallback_used {
        out.push_str("- Synthesis or review used a backend fallback; treat unusual outputs with extra scrutiny.\n");
    }
    out.push('\n');

    out
}

fn render_workflow_skill_markdown(server_name: &str, workflow: &WorkflowSkillSpec) -> String {
    let server_slug = sanitize_slug(server_name);
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
    if workflow.verification.is_empty() {
        out.push_str("- Confirm the workflow completed as expected.\n");
    } else {
        for item in &workflow.verification {
            out.push_str(&format!("- {}\n", item));
        }
    }
    out.push('\n');

    out.push_str("## What To Return\n\n");
    if workflow.return_contract.is_empty() {
        out.push_str("- Return the concrete result and any important follow-up context.\n");
    } else {
        for item in &workflow.return_contract {
            out.push_str(&format!("- {}\n", item));
        }
    }
    out.push('\n');

    out.push_str("## Stop And Ask\n\n");
    if workflow.stop_and_ask.is_empty() {
        out.push_str("- Stop if the workflow cannot proceed safely with the available context.\n");
    } else {
        for item in &workflow.stop_and_ask {
            out.push_str(&format!("- {}\n", item));
        }
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
    if workflow.evidence.is_empty() {
        out.push_str("- Grounded in the reviewed evidence bundle for this MCP.\n");
    } else {
        for evidence in &workflow.evidence {
            out.push_str(&format!("- {}\n", evidence));
        }
    }
    out.push('\n');

    out.push_str("## Confidence\n\n");
    out.push_str(&format!("- {:.2}\n", workflow.confidence.clamp(0.0, 1.0)));

    out
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

pub(crate) fn normalize_tool_name(name: &str) -> String {
    let trimmed = name.trim();
    if let Some(stripped) = trimmed.strip_prefix("mcp__") {
        stripped
            .rsplit("__")
            .next()
            .unwrap_or(trimmed)
            .trim()
            .to_string()
    } else {
        trimmed.to_string()
    }
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

pub fn default_agents_skills_dir() -> PathBuf {
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
