use crate::skillset::normalize_tool_name;
use crate::{
    BackendContext, BackendHealthStatus, BackendSelection, ConvertBackendConfig,
    ConvertBackendHealthReport, ConvertBackendName, ConvertBackendPreference, ConvertV3Options,
    DEFAULT_BACKEND_TIMEOUT_SECONDS, MCPServerProfile, RuntimeTool, ToolEnrichmentResponse,
    ToolSkillHint, ToolSpec, WorkflowSkillSpec,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

const CODEX_REASONING_EFFORT_MEDIUM: &str = "medium";
const CODEX_REASONING_EFFORT_LOW: &str = "low";
const WORKFLOW_PROMPT_MAX_DERIVATION_EVIDENCE: usize = 4;
const WORKFLOW_PROMPT_EXCERPT_PREVIEW_CHARS: usize = 220;
const WORKFLOW_PROMPT_MAX_INPUT_NAMES: usize = 12;
const WORKFLOW_PROMPT_MAX_INSPECTED_ITEMS: usize = 6;

const TOOL_ENRICHMENT_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["tools"],
  "properties": {
    "tools": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": [
          "name",
          "what_it_does",
          "when_to_use",
          "inputs_hint",
          "success_signals",
          "pitfalls"
        ],
        "properties": {
          "name": { "type": "string" },
          "what_it_does": { "type": ["string", "null"] },
          "when_to_use": { "type": ["string", "null"] },
          "inputs_hint": {
            "type": "array",
            "items": { "type": "string" }
          },
          "success_signals": {
            "type": "array",
            "items": { "type": "string" }
          },
          "pitfalls": {
            "type": "array",
            "items": { "type": "string" }
          }
        }
      }
    }
  }
}"#;

pub(crate) fn codex_enrichment_hints(
    server: &MCPServerProfile,
    required_tools: &[String],
    spec_by_name: &BTreeMap<String, ToolSpec>,
) -> Result<BTreeMap<String, ToolSkillHint>> {
    if required_tools.is_empty() {
        return Ok(BTreeMap::new());
    }

    #[derive(Serialize)]
    struct PromptTool<'a> {
        name: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<&'a str>,
    }

    let tools = required_tools
        .iter()
        .map(|tool_name| PromptTool {
            name: tool_name,
            description: spec_by_name
                .get(tool_name)
                .and_then(|item| item.description.as_deref()),
        })
        .collect::<Vec<_>>();
    let tools_json = serde_json::to_string_pretty(&tools)
        .context("Failed to serialize tool list for Codex enrichment prompt")?;

    let prompt = format!(
        "You are writing OPTIONAL hint text for agent skills.\n\
Do not invent capabilities that are not implied by the tool name/description.\n\
If unknown, leave fields empty.\n\
Keep each string concise (one sentence or short phrase).\n\n\
Server: {}\n\
Purpose: {}\n\
Tools (JSON):\n{}\n\n\
Return ONLY JSON matching the provided schema.\n\
Use normalized tool names exactly as provided in the tool list.\n",
        server.name, server.purpose, tools_json
    );

    let raw = invoke_codex_structured(&prompt, TOOL_ENRICHMENT_SCHEMA)?;
    let required_set = required_tools
        .iter()
        .map(|tool| normalize_tool_name(tool))
        .collect::<BTreeSet<_>>();
    parse_codex_enrichment_response(&raw, &required_set)
}

fn codex_command() -> String {
    std::env::var("MCPSMITH_CODEX_COMMAND").unwrap_or_else(|_| "codex".to_string())
}

fn invoke_codex_structured(prompt: &str, schema_json: &str) -> Result<String> {
    invoke_codex_structured_with_command(&codex_command(), prompt, schema_json)
}

fn invoke_codex_structured_with_command(
    command: &str,
    prompt: &str,
    schema_json: &str,
) -> Result<String> {
    let schema_path = create_temp_file_path("mcpsmith-codex-schema", "json")?;
    let output_path = create_temp_file_path("mcpsmith-codex-output", "txt")?;
    std::fs::write(&schema_path, schema_json)
        .with_context(|| format!("Failed to write {}", schema_path.display()))?;
    let temp_files = vec![schema_path.clone(), output_path.clone()];

    let backend_home = prepare_backend_home(ConvertBackendName::Codex)?;
    let mut codex = Command::new(command);
    apply_backend_home(&mut codex, backend_home.as_ref());
    let mut child = match codex
        .args([
            "exec",
            "--ephemeral",
            "-c",
            &codex_reasoning_option(CODEX_REASONING_EFFORT_MEDIUM),
            "--output-schema",
            schema_path.to_string_lossy().as_ref(),
            "--output-last-message",
            output_path.to_string_lossy().as_ref(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            cleanup_temp_files(&temp_files);
            return Err(err).with_context(|| format!("Failed to spawn `{command} exec`"));
        }
    };

    if let Some(mut stdin) = child.stdin.take()
        && let Err(err) = stdin.write_all(prompt.as_bytes())
    {
        cleanup_temp_files(&temp_files);
        return Err(err).context("Failed to write enrichment prompt to codex stdin");
    }

    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(err) => {
            cleanup_temp_files(&temp_files);
            return Err(err).context("Failed while waiting for codex enrichment output");
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        cleanup_temp_files(&temp_files);
        bail!(
            "Codex enrichment failed with status {}: {}",
            output.status,
            clipped_tail_preview(stderr.trim(), 1600)
        );
    }

    let stdout = String::from_utf8(output.stdout).unwrap_or_default();
    let final_output = std::fs::read_to_string(&output_path)
        .ok()
        .filter(|text| !text.trim().is_empty())
        .unwrap_or(stdout);

    cleanup_temp_files(&temp_files);
    Ok(final_output)
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

fn parse_codex_enrichment_response(
    raw: &str,
    required_tools: &BTreeSet<String>,
) -> Result<BTreeMap<String, ToolSkillHint>> {
    let response: ToolEnrichmentResponse = serde_json::from_str(raw.trim()).with_context(|| {
        format!(
            "Codex enrichment response is not valid JSON: {}",
            clipped_preview(raw.trim(), 220)
        )
    })?;

    let mut hints = BTreeMap::new();
    for entry in response.tools {
        let name = normalize_tool_name(&entry.name);
        if !required_tools.contains(&name) {
            continue;
        }
        hints.insert(
            name,
            ToolSkillHint {
                what_it_does: clean_optional_text(entry.what_it_does),
                when_to_use: clean_optional_text(entry.when_to_use),
                inputs_hint: clean_hint_list(entry.inputs_hint),
                success_signals: clean_hint_list(entry.success_signals),
                pitfalls: clean_hint_list(entry.pitfalls),
            },
        );
    }
    Ok(hints)
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

pub fn backend_health_report(_config: &ConvertBackendConfig) -> ConvertBackendHealthReport {
    ConvertBackendHealthReport {
        checked_at: Utc::now(),
        statuses: vec![
            codex_backend().health_check(),
            claude_backend().health_check(),
        ],
    }
}

pub(crate) fn generate_workflow_skills(
    backend_name: ConvertBackendName,
    server: &MCPServerProfile,
    runtime_tools: &[RuntimeTool],
    chunk_size: usize,
    timeout_seconds: u64,
) -> Result<Vec<WorkflowSkillSpec>> {
    let backend = backend_by_name(backend_name, timeout_seconds);
    let schema = workflow_chunk_schema();

    let mut out = vec![];
    for chunk in runtime_tools.chunks(chunk_size.max(1)) {
        out.extend(generate_workflow_chunk_specs(
            backend.as_ref(),
            server,
            chunk,
            runtime_tools,
            &schema,
        )?);
    }

    Ok(out)
}

fn generate_workflow_chunk_specs(
    backend: &dyn AgentBackend,
    server: &MCPServerProfile,
    chunk: &[RuntimeTool],
    all_runtime_tools: &[RuntimeTool],
    schema: &str,
) -> Result<Vec<WorkflowSkillSpec>> {
    let prompt = build_workflow_chunk_prompt(server, chunk, all_runtime_tools);
    let raw = match backend.explain_tool_chunk(&prompt, schema) {
        Ok(raw) => raw,
        Err(err) if chunk.len() > 1 && should_split_workflow_chunk(&err) => {
            return split_workflow_chunk_specs(backend, server, chunk, all_runtime_tools, schema);
        }
        Err(err) => return Err(err),
    };
    let parsed = match parse_backend_workflow_response(&raw) {
        Ok(parsed) => parsed,
        Err(err) if chunk.len() > 1 && should_split_workflow_chunk(&err) => {
            return split_workflow_chunk_specs(backend, server, chunk, all_runtime_tools, schema);
        }
        Err(err) => return Err(err),
    };
    let known_tools = all_runtime_tools
        .iter()
        .map(|tool| normalize_tool_name(&tool.name))
        .collect::<BTreeSet<_>>();

    Ok(parsed
        .workflow_skills
        .into_iter()
        .map(|workflow| normalize_workflow_skill(workflow, &known_tools))
        .collect())
}

fn split_workflow_chunk_specs(
    backend: &dyn AgentBackend,
    server: &MCPServerProfile,
    chunk: &[RuntimeTool],
    all_runtime_tools: &[RuntimeTool],
    schema: &str,
) -> Result<Vec<WorkflowSkillSpec>> {
    let midpoint = chunk.len() / 2;
    let mut left = generate_workflow_chunk_specs(
        backend,
        server,
        &chunk[..midpoint],
        all_runtime_tools,
        schema,
    )?;
    let mut right = generate_workflow_chunk_specs(
        backend,
        server,
        &chunk[midpoint..],
        all_runtime_tools,
        schema,
    )?;
    left.append(&mut right);
    Ok(left)
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

fn is_backend_timeout_error(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| cause.to_string().to_ascii_lowercase().contains("timed out"))
}

fn should_split_workflow_chunk(err: &anyhow::Error) -> bool {
    if is_backend_timeout_error(err) {
        return true;
    }
    err.chain().any(|cause| {
        let message = cause.to_string().to_ascii_lowercase();
        message.contains("backend failed with status")
            || message.contains("not valid workflow json")
            || message.contains("contained no workflow_skills")
    })
}

fn build_workflow_chunk_prompt(
    server: &MCPServerProfile,
    tools: &[RuntimeTool],
    all_runtime_tools: &[RuntimeTool],
) -> String {
    let source_json = serde_json::to_string_pretty(&workflow_source_grounding_summary(server))
        .unwrap_or_else(|_| "{}".to_string());
    let tools_json = serde_json::to_string_pretty(&workflow_prompt_tool_summaries(tools))
        .unwrap_or_else(|_| "[]".to_string());
    let all_tool_names = all_runtime_tools
        .iter()
        .map(|tool| normalize_tool_name(&tool.name))
        .collect::<Vec<_>>();
    let all_tools_json =
        serde_json::to_string_pretty(&all_tool_names).unwrap_or_else(|_| "[]".to_string());
    format!(
        "You are generating standalone workflow skills that will replace an MCP server after the MCP is removed.\n\
Return only JSON matching the schema.\n\
Do not run shell commands, inspect files, browse repositories, or call any MCP tools while answering.\n\
Treat the prompt as the complete evidence set; author the workflow JSON directly from the embedded source/runtime data.\n\
You are not executing the workflow now. You are only writing reusable instructions for a future agent.\n\
Do not mention MCP, tools/list, tools/call, or `mcp__...` names in the workflow text.\n\
Assume the generated SKILL.md files must work using only native system tools, shell commands, files, and already-installed local skills.\n\
Use the inspected source/docs evidence to derive concrete native commands whenever possible.\n\
If the evidence is insufficient for a concrete replacement workflow, leave the workflow out instead of inventing it.\n\
\n\
Server name: {}\n\
Server purpose: {}\n\
Source grounding summary:\n{}\n\
\n\
All runtime tool names:\n{}\n\
\n\
Runtime tools in this chunk (compact summary):\n{}\n\
\n\
Requirements per workflow:\n\
- id: stable slug-like workflow id\n\
- title: user-facing workflow title\n\
- goal: what the workflow accomplishes\n\
- when_to_use: trigger guidance for when an agent should choose this workflow\n\
- origin_tools: runtime tool names this workflow replaces or was derived from\n\
- stop_and_ask: situations that require stopping and asking the user\n\
- trigger_phrases: short user request examples\n\
- native_steps: concrete native commands or local-skill calls. Commands must be executable shell snippets, not prose placeholders. Each step must include title, command, and details (use null when no extra details are needed).\n\
- verification: how to confirm the workflow succeeded\n\
- return_contract: what the agent must return to the user after running it\n\
- confidence: float between 0 and 1\n\
Return only those fields. mcpsmith will derive any remaining optional sections after parsing.\n\
Generate at least one workflow for each runtime tool in the chunk when the evidence supports a concrete standalone procedure.\n",
        server.name, server.purpose, source_json, all_tools_json, tools_json
    )
}

fn workflow_source_grounding_summary(server: &MCPServerProfile) -> Value {
    serde_json::json!({
        "kind": &server.source_grounding.kind,
        "evidence_level": &server.source_grounding.evidence_level,
        "inspected": server.source_grounding.inspected,
        "entrypoint": &server.source_grounding.entrypoint,
        "package_name": &server.source_grounding.package_name,
        "package_version": &server.source_grounding.package_version,
        "homepage": &server.source_grounding.homepage,
        "repository_url": &server.source_grounding.repository_url,
        "inspected_paths": server
            .source_grounding
            .inspected_paths
            .iter()
            .take(WORKFLOW_PROMPT_MAX_INSPECTED_ITEMS)
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>(),
        "inspected_urls": server
            .source_grounding
            .inspected_urls
            .iter()
            .take(WORKFLOW_PROMPT_MAX_INSPECTED_ITEMS)
            .cloned()
            .collect::<Vec<_>>(),
        "derivation_evidence": server
            .source_grounding
            .derivation_evidence
            .iter()
            .take(WORKFLOW_PROMPT_MAX_DERIVATION_EVIDENCE)
            .map(|item| {
                serde_json::json!({
                    "kind": item.kind,
                    "source": item.source,
                    "excerpt_preview": clipped_preview(
                        item.excerpt.trim(),
                        WORKFLOW_PROMPT_EXCERPT_PREVIEW_CHARS,
                    ),
                })
            })
            .collect::<Vec<_>>(),
    })
}

fn workflow_prompt_tool_summaries(tools: &[RuntimeTool]) -> Vec<Value> {
    tools
        .iter()
        .map(workflow_prompt_tool_summary)
        .collect::<Vec<_>>()
}

fn workflow_prompt_tool_summary(tool: &RuntimeTool) -> Value {
    let (required_inputs, input_names) = summarize_runtime_tool_inputs(tool.input_schema.as_ref());
    serde_json::json!({
        "name": normalize_tool_name(&tool.name),
        "description": &tool.description,
        "required_inputs": required_inputs,
        "input_names": input_names,
    })
}

fn summarize_runtime_tool_inputs(schema: Option<&Value>) -> (Vec<String>, Vec<String>) {
    let Some(schema) = schema else {
        return (vec![], vec![]);
    };

    let mut required_inputs = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    required_inputs.sort();
    required_inputs.dedup();

    let mut input_names = schema
        .get("properties")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|properties| properties.keys())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    input_names.sort();
    input_names.dedup();
    input_names.truncate(WORKFLOW_PROMPT_MAX_INPUT_NAMES);

    (required_inputs, input_names)
}

fn codex_reasoning_option(reasoning_effort: &str) -> String {
    format!("model_reasoning_effort=\"{reasoning_effort}\"")
}

fn workflow_chunk_schema() -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["workflow_skills"],
        "properties": {
            "workflow_skills": {
                "type": "array",
                "items": {
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
                        "trigger_phrases": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "origin_tools": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "stop_and_ask": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
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
                        "verification": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "return_contract": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "confidence": { "type": "number" }
                    }
                }
            }
        }
    }))
    .expect("workflow schema should serialize")
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BackendWorkflowResponse {
    workflow_skills: Vec<WorkflowSkillSpec>,
}

fn parse_backend_workflow_response(raw: &str) -> Result<BackendWorkflowResponse> {
    let trimmed = raw.trim();
    let response: BackendWorkflowResponse = serde_json::from_str(trimmed).with_context(|| {
        format!(
            "Backend response is not valid workflow JSON: {}",
            clipped_preview(trimmed, 300)
        )
    })?;
    if response.workflow_skills.is_empty() {
        bail!("Backend response contained no workflow_skills.");
    }
    Ok(response)
}

trait AgentBackend {
    #[allow(dead_code)]
    fn discover_tools_dossier(&self, prompt: &str, schema_json: &str) -> Result<String>;
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
    fn discover_tools_dossier(&self, prompt: &str, schema_json: &str) -> Result<String> {
        invoke_codex_structured_with_timeout(
            &self.command,
            prompt,
            schema_json,
            self.timeout_seconds,
            CODEX_REASONING_EFFORT_MEDIUM,
        )
    }

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
    fn discover_tools_dossier(&self, prompt: &str, schema_json: &str) -> Result<String> {
        invoke_claude_structured_with_timeout(
            &self.command,
            prompt,
            schema_json,
            self.timeout_seconds,
        )
    }

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

pub(crate) fn prepare_backend_context(options: &ConvertV3Options) -> Result<BackendContext> {
    let health = backend_health_report(&options.backend_config);
    let selection = select_backend(&health, options)?;
    Ok(BackendContext { selection, health })
}

fn select_backend(
    health: &ConvertBackendHealthReport,
    options: &ConvertV3Options,
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

    if let Some(explicit) = options.backend {
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

    let preferred = match options.backend_config.preference {
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
        let reason = if options.backend_config.preference == ConvertBackendPreference::Auto {
            "Auto-selected first available backend (codex, then claude)."
        } else {
            "Configured backend preference unavailable; auto-selected first available backend."
        };
        return Ok(BackendSelection {
            selected,
            fallback: if options.backend_auto { fallback } else { None },
            auto_mode: options.backend_auto,
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
    use crate::{ConversionRecommendation, PermissionLevel, SourceEvidenceLevel};
    use anyhow::anyhow;
    use std::cell::RefCell;
    use std::sync::{Mutex, OnceLock};

    fn backend_env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn build_workflow_chunk_prompt_includes_source_grounding_summary() {
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
                homepage: Some("https://playwright.dev".to_string()),
                repository_url: Some("https://github.com/microsoft/playwright-mcp".to_string()),
                inspected_paths: vec![],
                inspected_urls: vec![],
                derivation_evidence: vec![],
            },
        };
        let tools = vec![RuntimeTool {
            name: "navigate".to_string(),
            description: Some("Open pages".to_string()),
            input_schema: None,
        }];

        let prompt = build_workflow_chunk_prompt(&server, &tools, &tools);
        assert!(prompt.contains("Source grounding"));
        assert!(prompt.contains("@playwright/mcp"));
        assert!(prompt.contains("https://github.com/microsoft/playwright-mcp"));
        assert!(prompt.contains("native_steps"));
        assert!(prompt.contains("Do not run shell commands"));
    }

    #[test]
    fn build_workflow_chunk_prompt_clips_derivation_excerpts() {
        let server = MCPServerProfile {
            id: "fixture:slow".to_string(),
            name: "slow-server".to_string(),
            source_label: "fixture".to_string(),
            source_path: PathBuf::from("/tmp/settings.json"),
            purpose: "Slow workflow synthesis".to_string(),
            command: Some("npx".to_string()),
            args: vec!["-y".to_string(), "@acme/slow".to_string()],
            url: None,
            env_keys: vec![],
            declared_tool_count: 1,
            permission_hints: vec![],
            inferred_permission: PermissionLevel::ReadOnly,
            recommendation: ConversionRecommendation::ReplaceCandidate,
            recommendation_reason: "fixture".to_string(),
            source_grounding: crate::SourceGrounding {
                kind: crate::SourceKind::NpmPackage,
                evidence_level: SourceEvidenceLevel::SourceInspected,
                inspected: true,
                entrypoint: None,
                package_name: Some("@acme/slow".to_string()),
                package_version: Some("1.0.0".to_string()),
                homepage: Some("https://example.com/slow".to_string()),
                repository_url: Some("https://github.com/acme/slow".to_string()),
                inspected_paths: vec![],
                inspected_urls: vec!["https://example.com/slow/readme".to_string()],
                derivation_evidence: vec![crate::DerivationEvidence {
                    kind: crate::DerivationEvidenceKind::ReadmeSnippet,
                    source: "https://example.com/slow/readme".to_string(),
                    excerpt: "A".repeat(600),
                }],
            },
        };
        let tools = vec![RuntimeTool {
            name: "slow_tool".to_string(),
            description: Some("Slow tool".to_string()),
            input_schema: None,
        }];

        let prompt = build_workflow_chunk_prompt(&server, &tools, &tools);
        assert!(prompt.contains("\"excerpt_preview\""));
        assert!(!prompt.contains(&"A".repeat(320)));
    }

    #[test]
    fn build_workflow_chunk_prompt_uses_compact_tool_summaries() {
        let server = MCPServerProfile {
            id: "fixture:compact".to_string(),
            name: "compact-server".to_string(),
            source_label: "fixture".to_string(),
            source_path: PathBuf::from("/tmp/settings.json"),
            purpose: "Compact prompts".to_string(),
            command: Some("npx".to_string()),
            args: vec!["-y".to_string(), "@acme/compact".to_string()],
            url: None,
            env_keys: vec![],
            declared_tool_count: 1,
            permission_hints: vec![],
            inferred_permission: PermissionLevel::ReadOnly,
            recommendation: ConversionRecommendation::ReplaceCandidate,
            recommendation_reason: "fixture".to_string(),
            source_grounding: crate::SourceGrounding {
                kind: crate::SourceKind::NpmPackage,
                evidence_level: SourceEvidenceLevel::ConfigOnly,
                inspected: false,
                entrypoint: None,
                package_name: Some("@acme/compact".to_string()),
                package_version: Some("1.0.0".to_string()),
                homepage: None,
                repository_url: None,
                inspected_paths: vec![],
                inspected_urls: vec![],
                derivation_evidence: vec![],
            },
        };
        let tools = vec![RuntimeTool {
            name: "compact_tool".to_string(),
            description: Some("Compact tool".to_string()),
            input_schema: Some(serde_json::json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "required": ["projectPath"],
                "properties": {
                    "projectPath": {
                        "type": "string",
                        "description": "Path to the Xcode project."
                    },
                    "extraArgs": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                }
            })),
        }];

        let prompt = build_workflow_chunk_prompt(&server, &tools, &tools);
        assert!(prompt.contains("\"required_inputs\""));
        assert!(prompt.contains("\"input_names\""));
        assert!(!prompt.contains("\"$schema\""));
        assert!(!prompt.contains("\"properties\""));
    }

    #[test]
    fn workflow_chunk_schema_is_valid_json() {
        let schema = workflow_chunk_schema();
        let parsed: Value = serde_json::from_str(&schema).expect("schema should parse as JSON");
        assert_eq!(parsed["type"], "object");
        assert_eq!(parsed["properties"]["workflow_skills"]["type"], "array");
    }

    #[test]
    fn clipped_tail_preview_keeps_end_of_text() {
        let preview = clipped_tail_preview("abcdefghijklmnopqrstuvwxyz", 6);
        assert_eq!(preview, "...uvwxyz");
    }

    #[test]
    fn generate_workflow_skills_splits_chunks_on_backend_failure() {
        #[derive(Default)]
        struct SplitBackend {
            seen_chunk_sizes: RefCell<Vec<usize>>,
        }

        impl AgentBackend for SplitBackend {
            fn discover_tools_dossier(&self, _prompt: &str, _schema_json: &str) -> Result<String> {
                unreachable!("not used in workflow synthesis tests")
            }

            fn explain_tool_chunk(&self, prompt: &str, _schema_json: &str) -> Result<String> {
                let chunk_size = prompt.matches("\"name\":").count();
                self.seen_chunk_sizes.borrow_mut().push(chunk_size);
                if chunk_size > 1 {
                    return Err(anyhow!(
                        "Codex backend failed with status exit status: 1: prompt too large"
                    ));
                }

                let tool_name = prompt
                    .lines()
                    .find(|line| line.contains("\"name\":"))
                    .and_then(|line| line.split("\"name\":").nth(1))
                    .and_then(|line| line.split('"').nth(1))
                    .unwrap_or("unknown");

                Ok(format!(
                    r#"{{
  "workflow_skills": [
    {{
      "id": "{tool_name}",
      "title": "Use {tool_name}",
      "goal": "Run {tool_name} natively",
      "when_to_use": "Use this when you need {tool_name}.",
      "trigger_phrases": ["run {tool_name}"],
      "origin_tools": ["{tool_name}"],
      "required_context": [
        {{
          "name": "target",
          "guidance": "Know which target to operate on.",
          "required": true
        }}
      ],
      "context_acquisition": ["Ask for the target if it is missing."],
      "branching_rules": ["If the target is unavailable, stop."],
      "stop_and_ask": ["Stop when the target is ambiguous."],
      "native_steps": [
        {{
          "title": "Echo the tool",
          "command": "printf '%s\\n' '{tool_name}'"
        }}
      ],
      "verification": ["Confirm the command completed successfully."],
      "return_contract": ["Return the command output."],
      "guardrails": ["Do not guess the target."],
      "evidence": ["tool:{tool_name}"],
      "confidence": 0.8
    }}
  ]
}}"#
                ))
            }

            fn health_check(&self) -> BackendHealthStatus {
                BackendHealthStatus {
                    backend: ConvertBackendName::Codex,
                    available: true,
                    diagnostics: vec![],
                }
            }

            fn backend_name(&self) -> ConvertBackendName {
                ConvertBackendName::Codex
            }
        }

        let backend = SplitBackend::default();
        let server = MCPServerProfile {
            id: "fixture:split".to_string(),
            name: "split-server".to_string(),
            source_label: "fixture".to_string(),
            source_path: PathBuf::from("/tmp/settings.json"),
            purpose: "Split prompts".to_string(),
            command: Some("npx".to_string()),
            args: vec!["-y".to_string(), "@acme/split".to_string()],
            url: None,
            env_keys: vec![],
            declared_tool_count: 2,
            permission_hints: vec![],
            inferred_permission: PermissionLevel::ReadOnly,
            recommendation: ConversionRecommendation::ReplaceCandidate,
            recommendation_reason: "fixture".to_string(),
            source_grounding: crate::SourceGrounding {
                kind: crate::SourceKind::NpmPackage,
                evidence_level: SourceEvidenceLevel::ConfigOnly,
                inspected: false,
                entrypoint: None,
                package_name: Some("@acme/split".to_string()),
                package_version: Some("1.0.0".to_string()),
                homepage: None,
                repository_url: None,
                inspected_paths: vec![],
                inspected_urls: vec![],
                derivation_evidence: vec![],
            },
        };
        let tools = vec![
            RuntimeTool {
                name: "alpha".to_string(),
                description: Some("First tool".to_string()),
                input_schema: None,
            },
            RuntimeTool {
                name: "beta".to_string(),
                description: Some("Second tool".to_string()),
                input_schema: None,
            },
        ];

        let generated = generate_workflow_chunk_specs(
            &backend,
            &server,
            &tools,
            &tools,
            &workflow_chunk_schema(),
        )
        .expect("chunk splitting should recover");

        assert_eq!(generated.len(), 2);
        assert_eq!(backend.seen_chunk_sizes.borrow().as_slice(), &[2, 1, 1]);
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
}
