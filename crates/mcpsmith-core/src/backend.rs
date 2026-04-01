use crate::pipeline::{
    MapperFallbackEvidence, MapperRelevantFile, MapperRelevantFileRole, ToolMapperCandidate,
};
use crate::skillset::normalize_tool_name;
use crate::{
    BackendContext, BackendHealthStatus, BackendSelection, ConvertBackendConfig,
    ConvertBackendHealthReport, ConvertBackendName, ConvertBackendPreference,
    DEFAULT_BACKEND_TIMEOUT_SECONDS, MCPServerProfile, ToolConversionDraft, ToolEvidencePack,
    ToolSemanticSummary, WorkflowSkillSpec,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

/// Reasoning effort level passed to the Codex backend. Using "low" keeps
/// backend latency and cost down for structured extraction tasks (synthesis,
/// review, mapper) where the prompt already provides strong grounding context.
const CODEX_REASONING_EFFORT_LOW: &str = "low";

pub(crate) fn clipped_preview(input: &str, max_chars: usize) -> String {
    let mut chars = input.chars();
    let clipped: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{clipped}...")
    } else {
        clipped
    }
}

pub(crate) fn clipped_tail_preview(input: &str, max_chars: usize) -> String {
    let chars = input.chars().collect::<Vec<_>>();
    if chars.len() <= max_chars {
        return input.to_string();
    }
    let start = chars.len().saturating_sub(max_chars);
    format!("...{}", chars[start..].iter().collect::<String>())
}

fn create_temp_file_path(prefix: &str, extension: &str) -> Result<PathBuf> {
    let tmp_dir = std::env::temp_dir();
    let pid = std::process::id();
    let mut attempt = 0u32;
    loop {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = tmp_dir.join(format!("{prefix}-{pid}-{nanos}-{attempt}.{extension}"));
        match std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                attempt += 1;
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("Failed to create temporary file {}", path.display())
                });
            }
        }
    }
}

fn cleanup_temp_files(paths: &[PathBuf]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

fn clean_optional_text(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn clean_hint_list(items: Vec<String>) -> Vec<String> {
    let mut out = items
        .into_iter()
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>();
    out.sort();
    out.dedup();
    out
}

pub fn backend_health_report(_config: &ConvertBackendConfig) -> ConvertBackendHealthReport {
    ConvertBackendHealthReport {
        checked_at: Utc::now(),
        statuses: vec![
            codex_backend().health_check(),
            claude_backend().health_check(),
        ],
    }
}

fn normalize_workflow_skill(
    mut workflow: WorkflowSkillSpec,
    known_tools: &BTreeSet<String>,
) -> WorkflowSkillSpec {
    workflow.id = normalize_tool_name(&workflow.id);
    workflow.title = workflow.title.trim().to_string();
    workflow.goal = workflow.goal.trim().to_string();
    workflow.when_to_use = workflow.when_to_use.trim().to_string();
    workflow.trigger_phrases = clean_hint_list(workflow.trigger_phrases);
    workflow.origin_tools = workflow
        .origin_tools
        .into_iter()
        .map(|tool| normalize_tool_name(&tool))
        .filter(|tool| known_tools.contains(tool))
        .collect::<Vec<_>>();
    workflow.origin_tools.sort();
    workflow.origin_tools.dedup();
    workflow.prerequisite_workflows = workflow
        .prerequisite_workflows
        .into_iter()
        .map(|item| normalize_tool_name(&item))
        .collect::<Vec<_>>();
    workflow.prerequisite_workflows.sort();
    workflow.prerequisite_workflows.dedup();
    workflow.followup_workflows = workflow
        .followup_workflows
        .into_iter()
        .map(|item| normalize_tool_name(&item))
        .collect::<Vec<_>>();
    workflow.followup_workflows.sort();
    workflow.followup_workflows.dedup();
    for item in &mut workflow.required_context {
        item.name = item.name.trim().to_string();
        item.guidance = item.guidance.trim().to_string();
    }
    workflow
        .required_context
        .retain(|item| !item.name.is_empty() && !item.guidance.is_empty());
    workflow.context_acquisition = clean_hint_list(workflow.context_acquisition);
    workflow.branching_rules = clean_hint_list(workflow.branching_rules);
    workflow.stop_and_ask = clean_hint_list(workflow.stop_and_ask);
    for step in &mut workflow.native_steps {
        step.title = step.title.trim().to_string();
        step.command = step.command.trim().to_string();
        step.details = clean_optional_text(step.details.take());
    }
    workflow
        .native_steps
        .retain(|step| !step.title.is_empty() && !step.command.is_empty());
    workflow.verification = clean_hint_list(workflow.verification);
    workflow.return_contract = clean_hint_list(workflow.return_contract);
    workflow.guardrails = clean_hint_list(workflow.guardrails);
    workflow.evidence = clean_hint_list(workflow.evidence);
    workflow.confidence = workflow.confidence.clamp(0.0, 1.0);
    workflow
}

fn codex_reasoning_option(reasoning_effort: &str) -> String {
    format!("model_reasoning_effort=\"{reasoning_effort}\"")
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BackendSkillSynthesisResponse {
    semantic_summary: ToolSemanticSummary,
    workflow_skill: WorkflowSkillSpec,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BackendReviewResponse {
    pub(crate) approved: bool,
    #[serde(default)]
    pub(crate) findings: Vec<String>,
    #[serde(default)]
    pub(crate) revised_draft: Option<ToolConversionDraft>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BackendMapperResponse {
    #[serde(default)]
    relevant_files: Vec<BackendMapperFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BackendMapperFile {
    path: String,
    role: MapperRelevantFileRole,
    why: String,
    confidence: f32,
}

pub(crate) fn synthesize_tool_conversion_with_backend(
    backend_name: ConvertBackendName,
    server: &MCPServerProfile,
    evidence: &ToolEvidencePack,
    timeout_seconds: u64,
) -> Result<ToolConversionDraft> {
    let backend = backend_by_name(backend_name, timeout_seconds);
    let raw = backend.explain_tool_chunk(
        &build_tool_synthesis_prompt(server, evidence),
        &tool_synthesis_schema()?,
    )?;
    let parsed: BackendSkillSynthesisResponse =
        serde_json::from_str(raw.trim()).with_context(|| {
            format!(
                "Backend response is not valid synthesis JSON: {}",
                clipped_preview(raw.trim(), 280)
            )
        })?;

    let mut known_tools = BTreeSet::new();
    known_tools.insert(normalize_tool_name(&evidence.tool_name));

    Ok(ToolConversionDraft {
        tool_name: evidence.tool_name.clone(),
        semantic_summary: parsed.semantic_summary,
        workflow_skill: normalize_workflow_skill(parsed.workflow_skill, &known_tools),
        helper_scripts: vec![],
    })
}

pub(crate) fn review_tool_conversion_with_backend(
    backend_name: ConvertBackendName,
    server: &MCPServerProfile,
    evidence: &ToolEvidencePack,
    draft: &ToolConversionDraft,
    timeout_seconds: u64,
) -> Result<BackendReviewResponse> {
    let backend = backend_by_name(backend_name, timeout_seconds);
    let raw = backend.explain_tool_chunk(
        &build_tool_review_prompt(server, evidence, draft),
        &tool_review_schema()?,
    )?;
    serde_json::from_str(raw.trim()).with_context(|| {
        format!(
            "Backend response is not valid review JSON: {}",
            clipped_preview(raw.trim(), 280)
        )
    })
}

pub(crate) fn map_low_confidence_tool_with_backend(
    backend_name: ConvertBackendName,
    server: &MCPServerProfile,
    evidence: &ToolEvidencePack,
    candidates: &[ToolMapperCandidate],
    timeout_seconds: u64,
) -> Result<MapperFallbackEvidence> {
    let backend = backend_by_name(backend_name, timeout_seconds);
    let raw = backend.explain_tool_chunk(
        &build_tool_mapper_prompt(server, evidence, candidates),
        &tool_mapper_schema()?,
    )?;
    let parsed: BackendMapperResponse = serde_json::from_str(raw.trim()).with_context(|| {
        format!(
            "Backend response is not valid mapper JSON: {}",
            clipped_preview(raw.trim(), 280)
        )
    })?;

    let known_paths = candidates
        .iter()
        .map(|candidate| candidate.path.clone())
        .collect::<BTreeSet<_>>();
    let mut relevant_files = parsed
        .relevant_files
        .into_iter()
        .filter_map(|file| {
            let path = PathBuf::from(file.path.trim());
            if path.as_os_str().is_empty() || !known_paths.contains(&path) {
                return None;
            }
            let why = file.why.trim().to_string();
            if why.is_empty() {
                return None;
            }
            Some(MapperRelevantFile {
                path,
                role: file.role,
                why,
                confidence: file.confidence.clamp(0.0, 1.0),
            })
        })
        .collect::<Vec<_>>();
    relevant_files.sort_by(|left, right| {
        right
            .confidence
            .total_cmp(&left.confidence)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.role.to_string().cmp(&right.role.to_string()))
    });
    relevant_files.dedup_by(|left, right| left.path == right.path && left.role == right.role);

    Ok(MapperFallbackEvidence {
        backend: backend_name.to_string(),
        relevant_files,
    })
}

fn build_tool_synthesis_prompt(server: &MCPServerProfile, evidence: &ToolEvidencePack) -> String {
    let evidence_json = serde_json::to_string_pretty(evidence).unwrap_or_else(|_| "{}".to_string());
    format!(
        "You are converting one MCP tool into a standalone local skill.\n\
Return ONLY JSON matching the provided schema.\n\
Do not invent behavior that is not supported by the evidence.\n\
Prefer handler code and tests over README claims.\n\
Do not mention MCP transport names like tools/list, tools/call, or `mcp__...` in the workflow text.\n\
Use plain-English workflow instructions plus concrete native commands when the evidence supports them.\n\
If helper scripts are not necessary, do not invent them.\n\
\n\
Server: {}\n\
Purpose: {}\n\
Tool evidence pack JSON:\n{}\n\
\n\
Requirements:\n\
- semantic_summary.what_it_does: one concise paragraph\n\
- semantic_summary.required_inputs: stable input names inferred from schema/evidence\n\
- semantic_summary.prerequisites: explicit prerequisites only\n\
- semantic_summary.side_effect_level: one of read-only, write, destructive, unknown\n\
- semantic_summary.success_signals and failure_modes: short, concrete items\n\
- semantic_summary.citations: relative source paths from the evidence pack\n\
- workflow_skill: produce a valid grounded workflow skill for this one tool\n\
- workflow_skill.origin_tools must include the tool name\n\
- workflow_skill.evidence should include short evidence/citation lines\n\
- native_steps commands must be executable shell snippets, not prose placeholders\n\
- If evidence is weak, stay conservative and use stop-and-ask guidance instead of guessing.\n",
        server.name, server.purpose, evidence_json
    )
}

fn build_tool_mapper_prompt(
    server: &MCPServerProfile,
    evidence: &ToolEvidencePack,
    candidates: &[ToolMapperCandidate],
) -> String {
    let candidates_json =
        serde_json::to_string_pretty(candidates).unwrap_or_else(|_| "[]".to_string());
    format!(
        "You are mapping low-confidence tool evidence to relevant source files.\n\
Return ONLY JSON matching the provided schema.\n\
Use ONLY the candidate files provided below.\n\
Pick at most four files.\n\
Prefer registration and handler files. Use supporting only when it materially explains the tool.\n\
Do not invent files, behaviors, or line numbers.\n\
\n\
Server: {}\n\
Purpose: {}\n\
Tool name: {}\n\
Tool description: {}\n\
Required inputs: {}\n\
Current deterministic confidence: {:.2}\n\
Current diagnostics: {}\n\
\n\
Candidate files JSON:\n{}\n\
\n\
Requirements:\n\
- relevant_files.path must exactly match one candidate path\n\
- relevant_files.role must be one of registration, handler, supporting\n\
- relevant_files.why must be one short grounded sentence\n\
- relevant_files.confidence must be a 0-1 number\n\
- omit files that do not materially help locate the tool\n",
        server.name,
        server.purpose,
        evidence.tool_name,
        evidence.runtime_tool.description.as_deref().unwrap_or(""),
        if evidence.required_inputs.is_empty() {
            "none".to_string()
        } else {
            evidence.required_inputs.join(", ")
        },
        evidence.confidence,
        evidence
            .diagnostics
            .iter()
            .take(2)
            .cloned()
            .collect::<Vec<_>>()
            .join(" | "),
        candidates_json
    )
}

fn build_tool_review_prompt(
    server: &MCPServerProfile,
    evidence: &ToolEvidencePack,
    draft: &ToolConversionDraft,
) -> String {
    let evidence_json = serde_json::to_string_pretty(evidence).unwrap_or_else(|_| "{}".to_string());
    let draft_json = serde_json::to_string_pretty(draft).unwrap_or_else(|_| "{}".to_string());
    format!(
        "You are reviewing a generated skill draft for correctness and grounding.\n\
Return ONLY JSON matching the provided schema.\n\
Apply this rubric strictly:\n\
- no placeholders/TODO/TBD\n\
- no MCP transport references\n\
- prerequisites must be explicit and conservative\n\
- claims must be grounded in the provided evidence\n\
- workflow must be in plain English and executable where commands are provided\n\
- prefer short concrete fixes over rewrites\n\
\n\
Server: {}\n\
Purpose: {}\n\
Tool evidence pack JSON:\n{}\n\
\n\
Draft JSON:\n{}\n\
\n\
If the draft is acceptable, set approved=true and findings=[].\n\
If not, set approved=false, provide findings, and include revised_draft with fixes applied.\n",
        server.name, server.purpose, evidence_json, draft_json
    )
}

fn tool_mapper_schema() -> Result<String> {
    serde_json::to_string_pretty(&serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["relevant_files"],
        "properties": {
            "relevant_files": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["path", "role", "why", "confidence"],
                    "properties": {
                        "path": { "type": "string" },
                        "role": {
                            "type": "string",
                            "enum": ["registration", "handler", "supporting"]
                        },
                        "why": { "type": "string" },
                        "confidence": { "type": "number" }
                    }
                }
            }
        }
    }))
    .context("failed to serialize mapper schema")
}

fn tool_synthesis_schema() -> Result<String> {
    serde_json::to_string_pretty(&serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["semantic_summary", "workflow_skill"],
        "properties": {
            "semantic_summary": {
                "type": "object",
                "additionalProperties": false,
                "required": [
                    "what_it_does",
                    "required_inputs",
                    "prerequisites",
                    "side_effect_level",
                    "success_signals",
                    "failure_modes",
                    "citations",
                    "confidence"
                ],
                "properties": {
                    "what_it_does": { "type": "string" },
                    "required_inputs": { "type": "array", "items": { "type": "string" } },
                    "prerequisites": { "type": "array", "items": { "type": "string" } },
                    "side_effect_level": { "type": "string" },
                    "success_signals": { "type": "array", "items": { "type": "string" } },
                    "failure_modes": { "type": "array", "items": { "type": "string" } },
                    "citations": { "type": "array", "items": { "type": "string" } },
                    "confidence": { "type": "number" }
                }
            },
            "workflow_skill": {
                "type": "object",
                "additionalProperties": false,
                "required": [
                    "id",
                    "title",
                    "goal",
                    "when_to_use",
                    "trigger_phrases",
                    "origin_tools",
                    "stop_and_ask",
                    "native_steps",
                    "verification",
                    "return_contract",
                    "confidence"
                ],
                "properties": {
                    "id": { "type": "string" },
                    "title": { "type": "string" },
                    "goal": { "type": "string" },
                    "when_to_use": { "type": "string" },
                    "trigger_phrases": { "type": "array", "items": { "type": "string" } },
                    "origin_tools": { "type": "array", "items": { "type": "string" } },
                    "prerequisite_workflows": { "type": "array", "items": { "type": "string" } },
                    "followup_workflows": { "type": "array", "items": { "type": "string" } },
                    "required_context": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["name", "guidance", "required"],
                            "properties": {
                                "name": { "type": "string" },
                                "guidance": { "type": "string" },
                                "required": { "type": "boolean" }
                            }
                        }
                    },
                    "context_acquisition": { "type": "array", "items": { "type": "string" } },
                    "branching_rules": { "type": "array", "items": { "type": "string" } },
                    "stop_and_ask": { "type": "array", "items": { "type": "string" } },
                    "native_steps": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["title", "command", "details"],
                            "properties": {
                                "title": { "type": "string" },
                                "command": { "type": "string" },
                                "details": { "type": ["string", "null"] }
                            }
                        }
                    },
                    "verification": { "type": "array", "items": { "type": "string" } },
                    "return_contract": { "type": "array", "items": { "type": "string" } },
                    "guardrails": { "type": "array", "items": { "type": "string" } },
                    "evidence": { "type": "array", "items": { "type": "string" } },
                    "confidence": { "type": "number" }
                }
            }
        }
    }))
    .context("failed to serialize synthesis schema")
}

fn tool_review_schema() -> Result<String> {
    serde_json::to_string_pretty(&serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["approved", "findings", "revised_draft"],
        "properties": {
            "approved": { "type": "boolean" },
            "findings": { "type": "array", "items": { "type": "string" } },
            "revised_draft": {
                "type": ["object", "null"],
                "additionalProperties": false,
                "required": ["tool_name", "semantic_summary", "workflow_skill", "helper_scripts"],
                "properties": {
                    "tool_name": { "type": "string" },
                    "semantic_summary": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": [
                            "what_it_does",
                            "required_inputs",
                            "prerequisites",
                            "side_effect_level",
                            "success_signals",
                            "failure_modes",
                            "citations",
                            "confidence"
                        ],
                        "properties": {
                            "what_it_does": { "type": "string" },
                            "required_inputs": { "type": "array", "items": { "type": "string" } },
                            "prerequisites": { "type": "array", "items": { "type": "string" } },
                            "side_effect_level": { "type": "string" },
                            "success_signals": { "type": "array", "items": { "type": "string" } },
                            "failure_modes": { "type": "array", "items": { "type": "string" } },
                            "citations": { "type": "array", "items": { "type": "string" } },
                            "confidence": { "type": "number" }
                        }
                    },
                    "workflow_skill": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": [
                            "id",
                            "title",
                            "goal",
                            "when_to_use",
                            "trigger_phrases",
                            "origin_tools",
                            "stop_and_ask",
                            "native_steps",
                            "verification",
                            "return_contract",
                            "confidence"
                        ],
                        "properties": {
                            "id": { "type": "string" },
                            "title": { "type": "string" },
                            "goal": { "type": "string" },
                            "when_to_use": { "type": "string" },
                            "trigger_phrases": { "type": "array", "items": { "type": "string" } },
                            "origin_tools": { "type": "array", "items": { "type": "string" } },
                            "prerequisite_workflows": { "type": "array", "items": { "type": "string" } },
                            "followup_workflows": { "type": "array", "items": { "type": "string" } },
                            "required_context": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "required": ["name", "guidance", "required"],
                                    "properties": {
                                        "name": { "type": "string" },
                                        "guidance": { "type": "string" },
                                        "required": { "type": "boolean" }
                                    }
                                }
                            },
                            "context_acquisition": { "type": "array", "items": { "type": "string" } },
                            "branching_rules": { "type": "array", "items": { "type": "string" } },
                            "stop_and_ask": { "type": "array", "items": { "type": "string" } },
                            "native_steps": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "required": ["title", "command", "details"],
                                    "properties": {
                                        "title": { "type": "string" },
                                        "command": { "type": "string" },
                                        "details": { "type": ["string", "null"] }
                                    }
                                }
                            },
                            "verification": { "type": "array", "items": { "type": "string" } },
                            "return_contract": { "type": "array", "items": { "type": "string" } },
                            "guardrails": { "type": "array", "items": { "type": "string" } },
                            "evidence": { "type": "array", "items": { "type": "string" } },
                            "confidence": { "type": "number" }
                        }
                    },
                    "helper_scripts": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["relative_path", "body", "executable"],
                            "properties": {
                                "relative_path": { "type": "string" },
                                "body": { "type": "string" },
                                "executable": { "type": "boolean" }
                            }
                        }
                    }
                }
            }
        }
    }))
    .context("failed to serialize review schema")
}

trait AgentBackend {
    fn explain_tool_chunk(&self, prompt: &str, schema_json: &str) -> Result<String>;
    fn health_check(&self) -> BackendHealthStatus;
    fn backend_name(&self) -> ConvertBackendName;
}

#[derive(Debug, Clone)]
struct CodexBackend {
    command: String,
    timeout_seconds: u64,
}

impl AgentBackend for CodexBackend {
    fn explain_tool_chunk(&self, prompt: &str, schema_json: &str) -> Result<String> {
        invoke_codex_structured_with_timeout(
            &self.command,
            prompt,
            schema_json,
            self.timeout_seconds,
            CODEX_REASONING_EFFORT_LOW,
        )
    }

    fn health_check(&self) -> BackendHealthStatus {
        command_health(self.backend_name(), &self.command)
    }

    fn backend_name(&self) -> ConvertBackendName {
        ConvertBackendName::Codex
    }
}

#[derive(Debug, Clone)]
struct ClaudeBackend {
    command: String,
    timeout_seconds: u64,
}

#[derive(Debug)]
struct PreparedBackendHome {
    _tempdir: Option<TempDir>,
    #[cfg_attr(not(test), allow(dead_code))]
    home_path: PathBuf,
    env_overrides: Vec<(String, PathBuf)>,
}

impl AgentBackend for ClaudeBackend {
    fn explain_tool_chunk(&self, prompt: &str, schema_json: &str) -> Result<String> {
        invoke_claude_structured_with_timeout(
            &self.command,
            prompt,
            schema_json,
            self.timeout_seconds,
        )
    }

    fn health_check(&self) -> BackendHealthStatus {
        command_health(self.backend_name(), &self.command)
    }

    fn backend_name(&self) -> ConvertBackendName {
        ConvertBackendName::Claude
    }
}

fn codex_backend() -> CodexBackend {
    CodexBackend {
        command: std::env::var("MCPSMITH_CODEX_COMMAND").unwrap_or_else(|_| "codex".to_string()),
        timeout_seconds: DEFAULT_BACKEND_TIMEOUT_SECONDS,
    }
}

fn claude_backend() -> ClaudeBackend {
    ClaudeBackend {
        command: std::env::var("MCPSMITH_CLAUDE_COMMAND").unwrap_or_else(|_| "claude".to_string()),
        timeout_seconds: DEFAULT_BACKEND_TIMEOUT_SECONDS,
    }
}

fn backend_by_name(name: ConvertBackendName, timeout_seconds: u64) -> Box<dyn AgentBackend> {
    match name {
        ConvertBackendName::Codex => Box::new(CodexBackend {
            command: std::env::var("MCPSMITH_CODEX_COMMAND")
                .unwrap_or_else(|_| "codex".to_string()),
            timeout_seconds,
        }),
        ConvertBackendName::Claude => Box::new(ClaudeBackend {
            command: std::env::var("MCPSMITH_CLAUDE_COMMAND")
                .unwrap_or_else(|_| "claude".to_string()),
            timeout_seconds,
        }),
    }
}

fn command_health(name: ConvertBackendName, command: &str) -> BackendHealthStatus {
    let checks = [["--version"], ["-v"], ["version"]];
    let mut diagnostics = vec![];

    for args in checks {
        let backend_home = match prepare_backend_home(name) {
            Ok(home) => home,
            Err(err) => {
                diagnostics.push(format!("{command} backend home setup failed: {err}"));
                continue;
            }
        };
        let mut probe = Command::new(command);
        apply_backend_home(&mut probe, backend_home.as_ref());
        match probe
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Ok(status) if status.success() => {
                diagnostics.push(format!("{} responded to '{}'.", command, args.join(" ")));
                return BackendHealthStatus {
                    backend: name,
                    available: true,
                    diagnostics,
                };
            }
            Ok(status) => {
                diagnostics.push(format!(
                    "{} '{}' exited with status {}.",
                    command,
                    args.join(" "),
                    status
                ));
            }
            Err(err) => {
                diagnostics.push(format!("{} '{}' failed: {err}", command, args.join(" ")));
            }
        }
    }

    BackendHealthStatus {
        backend: name,
        available: false,
        diagnostics,
    }
}

pub(crate) fn prepare_backend_context(
    backend: Option<ConvertBackendName>,
    backend_auto: bool,
    backend_config: &ConvertBackendConfig,
) -> Result<BackendContext> {
    let health = backend_health_report(backend_config);
    let selection = select_backend(&health, backend, backend_auto, backend_config)?;
    Ok(BackendContext { selection })
}

fn select_backend(
    health: &ConvertBackendHealthReport,
    backend: Option<ConvertBackendName>,
    backend_auto: bool,
    backend_config: &ConvertBackendConfig,
) -> Result<BackendSelection> {
    let by_backend = health
        .statuses
        .iter()
        .map(|status| (status.backend, status.available))
        .collect::<BTreeMap<_, _>>();
    let codex_available = *by_backend.get(&ConvertBackendName::Codex).unwrap_or(&false);
    let claude_available = *by_backend
        .get(&ConvertBackendName::Claude)
        .unwrap_or(&false);

    let available_ordered = [ConvertBackendName::Codex, ConvertBackendName::Claude]
        .into_iter()
        .filter(|backend| *by_backend.get(backend).unwrap_or(&false))
        .collect::<Vec<_>>();

    if let Some(explicit) = backend {
        if !*by_backend.get(&explicit).unwrap_or(&false) {
            bail!(
                "Requested backend '{}' is unavailable. Install/configure it, or rerun with --backend-auto. Diagnostics: {}",
                explicit,
                health
                    .statuses
                    .iter()
                    .map(|status| format!("{}: {}", status.backend, status.diagnostics.join(" | ")))
                    .collect::<Vec<_>>()
                    .join(" || ")
            );
        }
        return Ok(BackendSelection {
            selected: explicit,
            fallback: None,
            auto_mode: false,
            diagnostics: vec![format!("Using explicit backend override '{}'.", explicit)],
        });
    }

    let preferred = match backend_config.preference {
        ConvertBackendPreference::Codex => Some(ConvertBackendName::Codex),
        ConvertBackendPreference::Claude => Some(ConvertBackendName::Claude),
        ConvertBackendPreference::Auto => None,
    };

    if let Some(pref) = preferred
        && *by_backend.get(&pref).unwrap_or(&false)
    {
        return Ok(BackendSelection {
            selected: pref,
            fallback: None,
            auto_mode: false,
            diagnostics: vec![format!("Using configured backend preference '{}'.", pref)],
        });
    }

    if let Some(selected) = available_ordered.first().copied() {
        let fallback = available_ordered
            .iter()
            .copied()
            .find(|backend| *backend != selected);
        let reason = if backend_config.preference == ConvertBackendPreference::Auto {
            "Auto-selected first available backend (codex, then claude)."
        } else {
            "Configured backend preference unavailable; auto-selected first available backend."
        };
        return Ok(BackendSelection {
            selected,
            fallback: if backend_auto { fallback } else { None },
            auto_mode: backend_auto,
            diagnostics: vec![reason.to_string()],
        });
    }

    let mut guidance = vec![
        "No supported backend is installed. Install Codex CLI (`codex`) or Claude Code CLI (`claude`)."
            .to_string(),
        "Then rerun with --backend codex|claude or keep --backend-auto.".to_string(),
    ];
    if !codex_available {
        guidance.push("Codex backend check failed.".to_string());
    }
    if !claude_available {
        guidance.push("Claude backend check failed.".to_string());
    }
    guidance.extend(health.statuses.iter().map(|status| {
        format!(
            "{} diagnostics: {}",
            status.backend,
            status.diagnostics.join(" | ")
        )
    }));

    bail!(guidance.join(" "))
}

fn invoke_codex_structured_with_timeout(
    command: &str,
    prompt: &str,
    schema_json: &str,
    timeout_seconds: u64,
    reasoning_effort: &str,
) -> Result<String> {
    let schema_path = create_temp_file_path("mcpsmith-v3-codex-schema", "json")?;
    let output_path = create_temp_file_path("mcpsmith-v3-codex-output", "txt")?;
    fs::write(&schema_path, schema_json)
        .with_context(|| format!("Failed to write {}", schema_path.display()))?;

    let backend_home = prepare_backend_home(ConvertBackendName::Codex)?;
    let mut codex = Command::new(command);
    apply_backend_home(&mut codex, backend_home.as_ref());
    let output = run_command_with_timeout(
        codex
            .args([
                "exec",
                "--skip-git-repo-check",
                "--ephemeral",
                "-c",
                &codex_reasoning_option(reasoning_effort),
                "--output-schema",
                schema_path.to_string_lossy().as_ref(),
                "--output-last-message",
                output_path.to_string_lossy().as_ref(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("Failed to spawn `{command} exec`"))?,
        prompt.as_bytes(),
        timeout_seconds,
    );

    let result = match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8(output.stdout).unwrap_or_default();
            fs::read_to_string(&output_path)
                .ok()
                .filter(|text| !text.trim().is_empty())
                .unwrap_or(stdout)
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = format!(
                "Codex backend failed with status {}: {}",
                output.status,
                clipped_tail_preview(stderr.trim(), 1600)
            );
            cleanup_temp_files(&[schema_path, output_path]);
            bail!(message);
        }
        Err(err) => {
            cleanup_temp_files(&[schema_path, output_path]);
            return Err(err);
        }
    };

    cleanup_temp_files(&[schema_path, output_path]);
    Ok(result)
}

fn invoke_claude_structured_with_timeout(
    command: &str,
    prompt: &str,
    schema_json: &str,
    timeout_seconds: u64,
) -> Result<String> {
    let full_prompt = format!(
        "Return ONLY JSON matching this schema:\n{}\n\nPrompt:\n{}",
        schema_json, prompt
    );

    let backend_home = prepare_backend_home(ConvertBackendName::Claude)?;
    let mut claude = Command::new(command);
    apply_backend_home(&mut claude, backend_home.as_ref());
    let output = run_command_with_timeout(
        claude
            .args([
                "--print",
                "--no-session-persistence",
                "--output-format",
                "json",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("Failed to spawn `{command}`"))?,
        full_prompt.as_bytes(),
        timeout_seconds,
    )?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Claude backend failed with status {}: {}",
            output.status,
            clipped_tail_preview(stderr.trim(), 1600)
        );
    }

    let stdout = String::from_utf8(output.stdout).unwrap_or_default();
    extract_claude_json_payload(&stdout).with_context(|| {
        format!(
            "Claude response did not contain valid JSON payload. Output preview: {}",
            clipped_preview(stdout.trim(), 260)
        )
    })
}

fn run_command_with_timeout(
    mut child: std::process::Child,
    stdin_payload: &[u8],
    timeout_seconds: u64,
) -> Result<std::process::Output> {
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(stdin_payload)
            .context("Failed to write backend prompt to stdin")?;
    }

    let timeout = Duration::from_secs(timeout_seconds.max(1));
    let deadline = Instant::now() + timeout;

    loop {
        if let Some(status) = child
            .try_wait()
            .context("Failed while waiting for backend process")?
        {
            let mut stdout = vec![];
            let mut stderr = vec![];
            if let Some(mut out) = child.stdout.take() {
                let _ = std::io::Read::read_to_end(&mut out, &mut stdout);
            }
            if let Some(mut err) = child.stderr.take() {
                let _ = std::io::Read::read_to_end(&mut err, &mut stderr);
            }
            return Ok(std::process::Output {
                status,
                stdout,
                stderr,
            });
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "Backend command timed out after {} seconds",
                timeout_seconds.max(1)
            );
        }

        thread::sleep(Duration::from_millis(30));
    }
}

fn extract_claude_json_payload(stdout: &str) -> Result<String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        bail!("Claude output is empty.");
    }

    if let Ok(value) = serde_json::from_str::<Value>(trimmed)
        && value.get("workflow_skills").is_some()
    {
        return Ok(trimmed.to_string());
    }

    let envelope: Value =
        serde_json::from_str(trimmed).context("Claude output is not valid JSON envelope")?;

    if let Some(text) = envelope
        .get("output")
        .and_then(Value::as_str)
        .or_else(|| envelope.get("text").and_then(Value::as_str))
        .or_else(|| envelope.get("completion").and_then(Value::as_str))
        && let Some(json_payload) = extract_embedded_json(text)
    {
        return Ok(json_payload);
    }

    if let Some(message) = envelope.get("message") {
        if let Some(text) = message.get("text").and_then(Value::as_str)
            && let Some(json_payload) = extract_embedded_json(text)
        {
            return Ok(json_payload);
        }

        if let Some(content) = message.get("content") {
            if let Some(text) = content.as_str()
                && let Some(json_payload) = extract_embedded_json(text)
            {
                return Ok(json_payload);
            }
            if let Some(items) = content.as_array() {
                for item in items {
                    if let Some(text) = item.get("text").and_then(Value::as_str)
                        && let Some(json_payload) = extract_embedded_json(text)
                    {
                        return Ok(json_payload);
                    }
                }
            }
        }
    }

    if let Some(items) = envelope.get("content").and_then(Value::as_array) {
        for item in items {
            if let Some(text) = item.get("text").and_then(Value::as_str)
                && let Some(json_payload) = extract_embedded_json(text)
            {
                return Ok(json_payload);
            }
        }
    }

    bail!("No JSON payload found in Claude envelope.")
}

fn apply_backend_home(command: &mut Command, backend_home: Option<&PreparedBackendHome>) {
    if let Some(home) = backend_home {
        for (key, value) in &home.env_overrides {
            command.env(key, value);
        }
    }
}

fn backend_home_override(backend: ConvertBackendName) -> Option<String> {
    let env_key = match backend {
        ConvertBackendName::Codex => "MCPSMITH_CODEX_HOME",
        ConvertBackendName::Claude => "MCPSMITH_CLAUDE_HOME",
    };
    std::env::var(env_key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn prepare_backend_home(backend: ConvertBackendName) -> Result<Option<PreparedBackendHome>> {
    match backend {
        ConvertBackendName::Codex => prepare_codex_backend_home(),
        ConvertBackendName::Claude => Ok(backend_home_override(ConvertBackendName::Claude).map(
            |home| {
                let home_path = PathBuf::from(&home);
                PreparedBackendHome {
                    _tempdir: None,
                    home_path: home_path.clone(),
                    env_overrides: vec![("HOME".to_string(), home_path)],
                }
            },
        )),
    }
}

fn prepare_codex_backend_home() -> Result<Option<PreparedBackendHome>> {
    let Some(source_codex_dir) = resolve_codex_source_dir() else {
        return Ok(None);
    };

    let tempdir = tempfile::tempdir().context("Failed to create isolated Codex home")?;
    let isolated_path = tempdir.path().to_path_buf();
    copy_codex_control_plane(&source_codex_dir, &isolated_path)?;

    Ok(Some(PreparedBackendHome {
        _tempdir: Some(tempdir),
        home_path: isolated_path.clone(),
        env_overrides: vec![("CODEX_HOME".to_string(), isolated_path)],
    }))
}

fn current_home_path() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|home| home.trim().to_string())
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

fn resolve_codex_source_dir() -> Option<PathBuf> {
    let mut candidates = vec![];
    if let Some(path) = backend_home_override(ConvertBackendName::Codex).map(PathBuf::from) {
        candidates.push(path);
    }
    if let Some(path) = std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    {
        candidates.push(path);
    }
    if let Some(path) = current_home_path() {
        candidates.push(path);
    }

    for candidate in candidates {
        if let Some(path) = normalize_codex_source_dir(&candidate) {
            return Some(path);
        }
    }

    None
}

fn normalize_codex_source_dir(candidate: &Path) -> Option<PathBuf> {
    let direct_auth = candidate.join("auth.json");
    if direct_auth.is_file() {
        return Some(candidate.to_path_buf());
    }

    let nested = candidate.join(".codex");
    if nested.join("auth.json").is_file() {
        return Some(nested);
    }

    None
}
fn copy_codex_control_plane(source_root: &Path, destination_root: &Path) -> Result<()> {
    for entry in [
        "auth.json",
        "config.toml",
        "AGENTS.md",
        "rules",
        "skills",
        "vendor_imports",
    ] {
        copy_control_plane_path(&source_root.join(entry), &destination_root.join(entry))?;
    }
    Ok(())
}

fn copy_control_plane_path(source: &Path, destination: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(source) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "Failed to stat Codex control-plane path {}",
                    source.display()
                )
            });
        }
    };

    if metadata.is_dir() {
        fs::create_dir_all(destination)
            .with_context(|| format!("Failed to create {}", destination.display()))?;
        for entry in
            fs::read_dir(source).with_context(|| format!("Failed to read {}", source.display()))?
        {
            let entry = entry
                .with_context(|| format!("Failed to read entry under {}", source.display()))?;
            copy_control_plane_path(&entry.path(), &destination.join(entry.file_name()))?;
        }
        return Ok(());
    }

    if metadata.file_type().is_symlink() {
        let target = fs::canonicalize(source)
            .with_context(|| format!("Failed to resolve symlink {}", source.display()))?;
        return copy_control_plane_path(&target, destination);
    }

    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    fs::copy(source, destination).with_context(|| {
        format!(
            "Failed to copy Codex control-plane file from {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

fn extract_embedded_json(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if serde_json::from_str::<Value>(trimmed).is_ok() {
        return Some(trimmed.to_string());
    }

    let no_fence = trimmed
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    if serde_json::from_str::<Value>(no_fence).is_ok() {
        return Some(no_fence.to_string());
    }
    let start = no_fence.find('{')?;
    let end = no_fence.rfind('}')?;
    if start >= end {
        return None;
    }
    let candidate = &no_fence[start..=end];
    if serde_json::from_str::<Value>(candidate).is_ok() {
        return Some(candidate.to_string());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use crate::test_env::backend_env_lock;

    #[test]
    fn clipped_tail_preview_keeps_end_of_text() {
        let preview = clipped_tail_preview("abcdefghijklmnopqrstuvwxyz", 6);
        assert_eq!(preview, "...uvwxyz");
    }

    #[test]
    fn backend_home_override_uses_backend_specific_env_var() {
        let _guard = backend_env_lock().lock().unwrap();
        unsafe {
            std::env::remove_var("MCPSMITH_CODEX_HOME");
            std::env::remove_var("MCPSMITH_CLAUDE_HOME");
        }
        assert_eq!(backend_home_override(ConvertBackendName::Codex), None);
        assert_eq!(backend_home_override(ConvertBackendName::Claude), None);

        unsafe {
            std::env::set_var("MCPSMITH_CODEX_HOME", " /tmp/codex-home ");
            std::env::set_var("MCPSMITH_CLAUDE_HOME", "/tmp/claude-home");
        }

        assert_eq!(
            backend_home_override(ConvertBackendName::Codex).as_deref(),
            Some("/tmp/codex-home")
        );
        assert_eq!(
            backend_home_override(ConvertBackendName::Claude).as_deref(),
            Some("/tmp/claude-home")
        );

        unsafe {
            std::env::remove_var("MCPSMITH_CODEX_HOME");
            std::env::remove_var("MCPSMITH_CLAUDE_HOME");
        }
    }

    #[test]
    fn prepare_backend_home_for_codex_supports_codex_home_override_and_preserves_control_plane() {
        let _guard = backend_env_lock().lock().unwrap();
        let source_codex_dir = tempfile::tempdir().expect("source codex home tempdir");
        fs::write(
            source_codex_dir.path().join("auth.json"),
            "{\"token\":\"abc\"}",
        )
        .expect("write auth");
        fs::write(
            source_codex_dir.path().join("config.toml"),
            "model = \"gpt-5.4\"",
        )
        .expect("write config");
        fs::create_dir_all(source_codex_dir.path().join("skills")).expect("create skills dir");
        fs::write(
            source_codex_dir.path().join("skills").join("demo.txt"),
            "skill body",
        )
        .expect("write skill");

        unsafe {
            std::env::set_var("MCPSMITH_CODEX_HOME", source_codex_dir.path());
        }

        let prepared = prepare_backend_home(ConvertBackendName::Codex)
            .expect("prepare backend home")
            .expect("isolated backend home");

        assert_ne!(prepared.home_path, source_codex_dir.path());
        assert_eq!(
            fs::read_to_string(prepared.home_path.join("auth.json")).expect("read copied auth"),
            "{\"token\":\"abc\"}"
        );
        assert_eq!(
            fs::read_to_string(prepared.home_path.join("config.toml")).expect("read copied config"),
            "model = \"gpt-5.4\""
        );
        assert_eq!(
            fs::read_to_string(prepared.home_path.join("skills").join("demo.txt"))
                .expect("read copied skill"),
            "skill body"
        );

        let mut command = Command::new("env");
        apply_backend_home(&mut command, Some(&prepared));
        let applied_envs = command
            .get_envs()
            .filter_map(|(key, value)| Some((key.to_owned(), value?.to_owned())))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            applied_envs
                .get(std::ffi::OsStr::new("CODEX_HOME"))
                .map(|value| PathBuf::from(value.clone())),
            Some(prepared.home_path.clone())
        );
        assert!(!applied_envs.contains_key(std::ffi::OsStr::new("HOME")));

        unsafe {
            std::env::remove_var("MCPSMITH_CODEX_HOME");
        }
    }

    #[test]
    fn codex_structured_invocation_skips_git_repo_trust_check() {
        let _guard = backend_env_lock().lock().unwrap();
        let dir = tempfile::tempdir().expect("backend tempdir");
        let script_path = dir.path().join("fake-codex.sh");
        fs::write(
            &script_path,
            r#"#!/bin/sh
if [ "${1:-}" = "--version" ] || [ "${1:-}" = "-v" ] || [ "${1:-}" = "version" ]; then
  exit 0
fi

skip_git_check=0
output_path=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --skip-git-repo-check)
      skip_git_check=1
      ;;
    --output-last-message)
      shift
      output_path="$1"
      ;;
  esac
  shift
done

if [ "$skip_git_check" -ne 1 ]; then
  echo "Not inside a trusted directory and --skip-git-repo-check was not specified." >&2
  exit 1
fi

printf '%s' '{"ok":true}' > "$output_path"
"#,
        )
        .expect("write fake codex");
        #[cfg(unix)]
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755))
            .expect("chmod fake codex");

        // Use a minimal stub home so the backend does not try to copy from the
        // real ~/.codex (which may contain broken symlinks).
        let codex_home = tempfile::tempdir().expect("codex home tempdir");
        fs::write(codex_home.path().join("auth.json"), "{}").expect("write auth stub");
        unsafe {
            std::env::set_var("MCPSMITH_CODEX_HOME", codex_home.path());
        }

        let result = invoke_codex_structured_with_timeout(
            script_path.to_string_lossy().as_ref(),
            "ignored prompt",
            r#"{"type":"object"}"#,
            5,
            CODEX_REASONING_EFFORT_LOW,
        )
        .expect("codex invocation should succeed");

        assert_eq!(result, r#"{"ok":true}"#);

        unsafe {
            std::env::remove_var("MCPSMITH_CODEX_HOME");
        }
    }
}
