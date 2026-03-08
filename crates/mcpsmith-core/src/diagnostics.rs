use crate::inventory::inspect;
use crate::runtime::introspect_tools;
use crate::skillset::{
    extract_tool_hints_from_skill, hint_to_tool_name, manifest_path_for_skill, normalize_tool_name,
    required_tool_names, sanitize_slug,
};
use crate::{ConvertVerifyReport, MCPServerProfile, ManifestToolSkill, SkillParityManifest};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub fn verify(
    server_selector: &str,
    additional_paths: &[PathBuf],
    skills_dir: Option<PathBuf>,
) -> Result<ConvertVerifyReport> {
    let server = inspect(server_selector, additional_paths)?;
    let skills_dir = skills_dir.unwrap_or_else(default_skills_dir);
    let skill_path = skills_dir.join(format!("mcp-{}.md", sanitize_slug(&server.name)));

    let introspected = introspect_tools(&server).ok();
    verify_with_server_and_path(&server, &skill_path, introspected.as_deref())
}

fn default_skills_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    home.join(".agents").join("skills")
}

pub(crate) fn verify_with_server_and_path(
    server: &MCPServerProfile,
    skill_path: &Path,
    introspected_tools: Option<&[String]>,
) -> Result<ConvertVerifyReport> {
    if !skill_path.exists() {
        bail!(
            "Generated skill file not found for verification: {}",
            skill_path.display()
        );
    }

    let skill_content = std::fs::read_to_string(skill_path)
        .with_context(|| format!("Failed to read {}", skill_path.display()))?;
    let mut required_tools = required_tool_names(server, introspected_tools);

    let mut notes = vec![];
    let introspected_set = introspected_tools
        .map(|items| {
            items
                .iter()
                .map(|item| normalize_tool_name(item))
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let introspection_ok = !introspected_set.is_empty();
    if introspection_ok {
        notes.push(format!(
            "Live MCP introspection returned {} tools.",
            introspected_set.len()
        ));
    } else {
        notes.push(
            "Live MCP introspection unavailable; verification is heuristic-only.".to_string(),
        );
    }

    let manifest_path = manifest_path_for_skill(skill_path)?;
    let mut manifest_tool_skills = vec![];
    let mut orchestrator_skill_path = skill_path.to_path_buf();
    if manifest_path.exists() {
        let manifest_raw = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
        let manifest: SkillParityManifest =
            serde_json::from_str(&manifest_raw).with_context(|| {
                format!(
                    "Failed to parse parity manifest {}",
                    manifest_path.display()
                )
            })?;
        notes.push(format!(
            "Loaded parity manifest from {}.",
            manifest_path.display()
        ));
        if let Some(orchestrator) = manifest.orchestrator_skill
            && let Some(parent) = skill_path.parent()
        {
            orchestrator_skill_path = parent.join(orchestrator);
        }
        if !manifest.required_tools.is_empty() {
            required_tools = manifest
                .required_tools
                .iter()
                .map(|tool| normalize_tool_name(tool))
                .collect();
        } else if !manifest.required_tool_hints.is_empty() {
            required_tools = manifest
                .required_tool_hints
                .iter()
                .map(|hint| hint_to_tool_name(hint))
                .collect();
        }
        manifest_tool_skills = manifest.tool_skills;
    } else {
        let legacy = extract_tool_hints_from_skill(&skill_content)
            .iter()
            .map(|hint| hint_to_tool_name(hint))
            .collect::<Vec<_>>();
        notes.push(
            "Parity manifest missing; falling back to legacy tool-hint parsing from markdown."
                .to_string(),
        );
        if required_tools.is_empty() {
            required_tools = legacy.clone();
        }
        if !legacy.is_empty() {
            let current_file = skill_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("skill.md")
                .to_string();
            manifest_tool_skills = legacy
                .iter()
                .map(|tool_name| ManifestToolSkill {
                    tool_name: tool_name.clone(),
                    skill_file: current_file.clone(),
                })
                .collect();
        }
    }

    required_tools.sort();
    required_tools.dedup();

    let parent_dir = skill_path
        .parent()
        .context("Skill path has no parent directory for verification")?;
    let tool_skill_paths = manifest_tool_skills
        .iter()
        .map(|tool| parent_dir.join(&tool.skill_file))
        .collect::<Vec<_>>();
    let mapped_tool_names = manifest_tool_skills
        .iter()
        .map(|tool| normalize_tool_name(&tool.tool_name))
        .collect::<BTreeSet<_>>();

    let missing_in_skill = required_tools
        .iter()
        .filter(|tool| !mapped_tool_names.contains(*tool))
        .cloned()
        .collect::<Vec<_>>();

    let required_tool_names = required_tools.iter().cloned().collect::<BTreeSet<_>>();
    let missing_in_server = if introspection_ok {
        required_tool_names
            .iter()
            .filter(|name| !introspected_set.contains(*name))
            .cloned()
            .collect::<Vec<_>>()
    } else {
        vec![]
    };

    let mut missing_skill_files = manifest_tool_skills
        .iter()
        .zip(tool_skill_paths.iter())
        .filter_map(|(tool, path)| {
            if path.exists() {
                None
            } else {
                Some(tool.skill_file.clone())
            }
        })
        .collect::<Vec<_>>();
    if !orchestrator_skill_path.exists() {
        missing_skill_files.push(
            orchestrator_skill_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("orchestrator-skill")
                .to_string(),
        );
    }

    let passed = missing_in_skill.is_empty()
        && missing_skill_files.is_empty()
        && (missing_in_server.is_empty() || !introspection_ok)
        && !tool_skill_paths.is_empty();

    Ok(ConvertVerifyReport {
        generated_at: Utc::now(),
        server: server.clone(),
        orchestrator_skill_path: orchestrator_skill_path.clone(),
        skill_path: skill_path.to_path_buf(),
        tool_skill_paths,
        introspection_ok,
        introspected_tool_count: introspected_set.len(),
        required_tools,
        missing_in_server,
        missing_in_skill,
        missing_skill_files,
        passed,
        notes,
    })
}
