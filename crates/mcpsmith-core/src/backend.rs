use crate::dossier::{
    default_contract_tests, has_any_probe_inputs, merge_source_grounding_evidence,
};
use crate::skillset::normalize_tool_name;
use crate::{
    BackendContext, BackendHealthStatus, BackendSelection, ConvertBackendConfig,
    ConvertBackendHealthReport, ConvertBackendName, ConvertBackendPreference, ConvertV3Options,
    DEFAULT_BACKEND_TIMEOUT_SECONDS, MCPServerProfile, ProbeInputSource, RuntimeTool, ToolDossier,
    ToolEnrichmentResponse, ToolSkillHint, ToolSpec,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

    let mut child = match Command::new(command)
        .args([
            "exec",
            "--ephemeral",
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
            clipped_preview(stderr.trim(), 220)
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

pub fn backend_health_report(_config: &ConvertBackendConfig) -> ConvertBackendHealthReport {
    ConvertBackendHealthReport {
        checked_at: Utc::now(),
        statuses: vec![
            codex_backend().health_check(),
            claude_backend().health_check(),
        ],
    }
}

pub(crate) fn generate_tool_dossiers(
    backend_name: ConvertBackendName,
    server: &MCPServerProfile,
    runtime_tools: &[RuntimeTool],
    chunk_size: usize,
    timeout_seconds: u64,
) -> Result<Vec<ToolDossier>> {
    let chunks = runtime_tools
        .chunks(chunk_size.max(1))
        .map(|chunk| chunk.to_vec())
        .collect::<Vec<_>>();

    let backend = backend_by_name(backend_name, timeout_seconds);
    let schema = tool_dossier_chunk_schema();

    let mut out = vec![];
    for chunk in chunks {
        let runtime_map = chunk
            .iter()
            .map(|tool| (normalize_tool_name(&tool.name), tool.clone()))
            .collect::<BTreeMap<_, _>>();
        let prompt = build_tool_chunk_prompt(server, &chunk);
        let raw = backend.explain_tool_chunk(&prompt, &schema)?;
        let parsed = parse_backend_chunk_response(&raw)?;

        for mut dossier in parsed.tool_dossiers {
            dossier.name = normalize_tool_name(&dossier.name);
            if dossier.recipe.is_empty() {
                dossier.recipe = vec![
                    "Validate inputs against runtime schema.".to_string(),
                    "Execute tool with deterministic arguments.".to_string(),
                    "Verify output/error contract and summarize outcome.".to_string(),
                ];
            }
            if dossier.contract_tests.is_empty() {
                dossier.contract_tests = default_contract_tests(server);
            }
            let runtime_tool =
                runtime_map
                    .get(&dossier.name)
                    .cloned()
                    .unwrap_or_else(|| RuntimeTool {
                        name: dossier.name.clone(),
                        description: None,
                        input_schema: None,
                    });
            dossier.evidence =
                merge_source_grounding_evidence(dossier.evidence, server, &runtime_tool);
            dossier.probe_input_source = if has_any_probe_inputs(&dossier.probe_inputs) {
                ProbeInputSource::Backend
            } else {
                ProbeInputSource::Synthesized
            };
            out.push(dossier);
        }
    }

    Ok(out)
}

fn build_tool_chunk_prompt(server: &MCPServerProfile, tools: &[RuntimeTool]) -> String {
    let source_json = serde_json::to_string_pretty(&serde_json::json!({
        "command": &server.command,
        "args": &server.args,
        "url": &server.url,
        "source_grounding": &server.source_grounding,
    }))
    .unwrap_or_else(|_| "{}".to_string());
    let tools_json = serde_json::to_string_pretty(tools).unwrap_or_else(|_| "[]".to_string());
    format!(
        "You are generating deterministic tool dossiers for MCP -> skill conversion.\n\
Return only JSON matching the schema.\n\
Do not invent tool names. Use names exactly from runtime_tools.\n\
\n\
Server name: {}\n\
Server purpose: {}\n\
Source grounding:\n{}\n\
\n\
Runtime tools:\n{}\n\
\n\
Requirements per tool:\n\
- explanation: concise behavior summary\n\
- recipe: deterministic steps to execute and verify the tool\n\
- evidence: runtime/source grounding references\n\
- confidence: float between 0 and 1\n\
- contract_tests must include probes: happy-path, invalid-input, side-effect-safety\n\
- probe_inputs: optional deterministic request args for happy-path/invalid-input/side-effect-safety\n\
- probe_input_source: backend if probe_inputs came from backend, otherwise synthesized\n",
        server.name, server.purpose, source_json, tools_json
    )
}

fn tool_dossier_chunk_schema() -> String {
    r#"{
  \"type\": \"object\",
  \"additionalProperties\": false,
  \"required\": [\"tool_dossiers\"],
  \"properties\": {
    \"tool_dossiers\": {
      \"type\": \"array\",
      \"items\": {
        \"type\": \"object\",
        \"additionalProperties\": false,
        \"required\": [\"name\", \"explanation\", \"recipe\", \"evidence\", \"confidence\", \"contract_tests\"],
        \"properties\": {
          \"name\": { \"type\": \"string\" },
          \"explanation\": { \"type\": \"string\" },
          \"recipe\": { \"type\": \"array\", \"items\": { \"type\": \"string\" } },
          \"evidence\": { \"type\": \"array\", \"items\": { \"type\": \"string\" } },
          \"confidence\": { \"type\": \"number\" },
          \"probe_inputs\": {
            \"type\": \"object\",
            \"additionalProperties\": false,
            \"properties\": {
              \"happy_path\": { \"type\": \"object\" },
              \"invalid_input\": { \"type\": \"object\" },
              \"side_effect_safety\": { \"type\": \"object\" }
            }
          },
          \"probe_input_source\": { \"type\": \"string\", \"enum\": [\"backend\", \"synthesized\"] },
          \"contract_tests\": {
            \"type\": \"array\",
            \"items\": {
              \"type\": \"object\",
              \"additionalProperties\": false,
              \"required\": [\"probe\", \"expected\", \"method\", \"applicable\"],
              \"properties\": {
                \"probe\": { \"type\": \"string\" },
                \"expected\": { \"type\": \"string\" },
                \"method\": { \"type\": \"string\" },
                \"applicable\": { \"type\": \"boolean\" }
              }
            }
          }
        }
      }
    }
  }
}"#
        .to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BackendChunkResponse {
    tool_dossiers: Vec<ToolDossier>,
}

fn parse_backend_chunk_response(raw: &str) -> Result<BackendChunkResponse> {
    let trimmed = raw.trim();
    let response: BackendChunkResponse = serde_json::from_str(trimmed).with_context(|| {
        format!(
            "Backend response is not valid dossier JSON: {}",
            clipped_preview(trimmed, 300)
        )
    })?;
    if response.tool_dossiers.is_empty() {
        bail!("Backend response contained no tool_dossiers.");
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
        )
    }

    fn explain_tool_chunk(&self, prompt: &str, schema_json: &str) -> Result<String> {
        invoke_codex_structured_with_timeout(
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
        ConvertBackendName::Codex
    }
}

#[derive(Debug, Clone)]
struct ClaudeBackend {
    command: String,
    timeout_seconds: u64,
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
        match Command::new(command)
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
) -> Result<String> {
    let schema_path = create_temp_file_path("mcpsmith-v3-codex-schema", "json")?;
    let output_path = create_temp_file_path("mcpsmith-v3-codex-output", "txt")?;
    fs::write(&schema_path, schema_json)
        .with_context(|| format!("Failed to write {}", schema_path.display()))?;

    let output = run_command_with_timeout(
        Command::new(command)
            .args([
                "exec",
                "--ephemeral",
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
                clipped_preview(stderr.trim(), 220)
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

    let output = run_command_with_timeout(
        Command::new(command)
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
            clipped_preview(stderr.trim(), 220)
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
        && value.get("tool_dossiers").is_some()
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

    #[test]
    fn build_tool_chunk_prompt_includes_source_grounding_summary() {
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
            },
        };
        let tools = vec![RuntimeTool {
            name: "navigate".to_string(),
            description: Some("Open pages".to_string()),
            input_schema: None,
        }];

        let prompt = build_tool_chunk_prompt(&server, &tools);
        assert!(prompt.contains("Source grounding"));
        assert!(prompt.contains("@playwright/mcp"));
        assert!(prompt.contains("https://github.com/microsoft/playwright-mcp"));
    }
}
