use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

mod apply;
mod backend;
mod contract;
mod diagnostics;
mod dossier;
mod inventory;
mod runtime;
mod skillset;
mod source;

pub use apply::{
    apply, apply_from_bundle, apply_from_dossier_path, apply_with_options, run_one_shot_v3,
};
pub use backend::backend_health_report;
pub use contract::{contract_test_bundle, contract_test_from_dossier_path};
pub use diagnostics::verify;
pub use dossier::{
    build_from_dossier_path, discover_v3, discover_v3_to_path, load_dossier_bundle,
    write_dossier_bundle,
};
pub use inventory::{discover, inspect, plan};
pub use skillset::build_from_bundle;

const DOSSIER_FORMAT_VERSION: u32 = 6;
const DEFAULT_BACKEND_TIMEOUT_SECONDS: u64 = 240;
const DEFAULT_BACKEND_CHUNK_SIZE: usize = 4;
const DEFAULT_PROBE_TIMEOUT_SECONDS: u64 = 30;
const DEFAULT_PROBE_RETRIES: u32 = 0;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionLevel {
    ReadOnly,
    Write,
    Destructive,
    Unknown,
}

impl std::fmt::Display for PermissionLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PermissionLevel::ReadOnly => write!(f, "read-only"),
            PermissionLevel::Write => write!(f, "write"),
            PermissionLevel::Destructive => write!(f, "destructive"),
            PermissionLevel::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ConversionRecommendation {
    KeepMcp,
    Hybrid,
    ReplaceCandidate,
}

impl std::fmt::Display for ConversionRecommendation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConversionRecommendation::KeepMcp => write!(f, "keep-mcp"),
            ConversionRecommendation::Hybrid => write!(f, "hybrid"),
            ConversionRecommendation::ReplaceCandidate => write!(f, "replace-candidate"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PlanMode {
    Auto,
    Hybrid,
    Replace,
}

impl std::fmt::Display for PlanMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanMode::Auto => write!(f, "auto"),
            PlanMode::Hybrid => write!(f, "hybrid"),
            PlanMode::Replace => write!(f, "replace"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum EnrichmentAgent {
    Codex,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ApplyOptions {
    pub output_dir: Option<PathBuf>,
    pub enrichment_agent: Option<EnrichmentAgent>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    #[default]
    Unknown,
    LocalPath,
    NpmPackage,
    PypiPackage,
    RepositoryUrl,
    RemoteUrl,
}

impl std::fmt::Display for SourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceKind::Unknown => write!(f, "unknown"),
            SourceKind::LocalPath => write!(f, "local-path"),
            SourceKind::NpmPackage => write!(f, "npm-package"),
            SourceKind::PypiPackage => write!(f, "pypi-package"),
            SourceKind::RepositoryUrl => write!(f, "repository-url"),
            SourceKind::RemoteUrl => write!(f, "remote-url"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SourceEvidenceLevel {
    #[default]
    RuntimeOnly,
    ConfigOnly,
    SourceInspected,
}

impl std::fmt::Display for SourceEvidenceLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceEvidenceLevel::RuntimeOnly => write!(f, "runtime-only"),
            SourceEvidenceLevel::ConfigOnly => write!(f, "config-only"),
            SourceEvidenceLevel::SourceInspected => write!(f, "source-inspected"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SourceGrounding {
    #[serde(default)]
    pub kind: SourceKind,
    #[serde(default)]
    pub evidence_level: SourceEvidenceLevel,
    #[serde(default)]
    pub inspected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inspected_paths: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inspected_urls: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derivation_evidence: Vec<DerivationEvidence>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum DerivationEvidenceKind {
    ManifestSnippet,
    ReadmeSnippet,
    EntrypointSnippet,
    CliHelp,
    RemoteDocSnippet,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DerivationEvidence {
    pub kind: DerivationEvidenceKind,
    pub source: String,
    pub excerpt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MCPServerProfile {
    pub id: String,
    pub name: String,
    pub source_label: String,
    pub source_path: PathBuf,
    pub purpose: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub env_keys: Vec<String>,
    pub declared_tool_count: usize,
    pub permission_hints: Vec<String>,
    pub inferred_permission: PermissionLevel,
    pub recommendation: ConversionRecommendation,
    pub recommendation_reason: String,
    #[serde(default)]
    pub source_grounding: SourceGrounding,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConvertInventory {
    pub generated_at: DateTime<Utc>,
    pub searched_paths: Vec<PathBuf>,
    pub servers: Vec<MCPServerProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConvertPlan {
    pub generated_at: DateTime<Utc>,
    pub server: MCPServerProfile,
    pub requested_mode: PlanMode,
    pub recommended_mode: PlanMode,
    pub effective_mode: PlanMode,
    pub blocked: bool,
    pub actions: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConvertApplyResult {
    pub generated_at: DateTime<Utc>,
    pub server: MCPServerProfile,
    pub requested_mode: PlanMode,
    pub effective_mode: PlanMode,
    pub orchestrator_skill_path: PathBuf,
    pub skill_path: PathBuf,
    pub tool_skill_paths: Vec<PathBuf>,
    pub mcp_config_backup: Option<PathBuf>,
    pub mcp_config_updated: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConvertVerifyReport {
    pub generated_at: DateTime<Utc>,
    pub server: MCPServerProfile,
    pub orchestrator_skill_path: PathBuf,
    pub skill_path: PathBuf,
    pub tool_skill_paths: Vec<PathBuf>,
    pub introspection_ok: bool,
    pub introspected_tool_count: usize,
    pub required_tools: Vec<String>,
    pub missing_in_server: Vec<String>,
    pub missing_in_skill: Vec<String>,
    pub missing_skill_files: Vec<String>,
    pub passed: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ConvertBackendPreference {
    #[default]
    Auto,
    Codex,
    Claude,
}

impl std::fmt::Display for ConvertBackendPreference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConvertBackendPreference::Auto => write!(f, "auto"),
            ConvertBackendPreference::Codex => write!(f, "codex"),
            ConvertBackendPreference::Claude => write!(f, "claude"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum ConvertBackendName {
    Codex,
    Claude,
}

impl std::fmt::Display for ConvertBackendName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConvertBackendName::Codex => write!(f, "codex"),
            ConvertBackendName::Claude => write!(f, "claude"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConvertBackendConfig {
    #[serde(default)]
    pub preference: ConvertBackendPreference,
    #[serde(default = "default_backend_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_backend_chunk_size")]
    pub chunk_size: usize,
}

impl Default for ConvertBackendConfig {
    fn default() -> Self {
        Self {
            preference: ConvertBackendPreference::Auto,
            timeout_seconds: DEFAULT_BACKEND_TIMEOUT_SECONDS,
            chunk_size: DEFAULT_BACKEND_CHUNK_SIZE,
        }
    }
}

fn default_backend_timeout_seconds() -> u64 {
    DEFAULT_BACKEND_TIMEOUT_SECONDS
}

fn default_backend_chunk_size() -> usize {
    DEFAULT_BACKEND_CHUNK_SIZE
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConvertProbeConfig {
    #[serde(default = "default_probe_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_probe_retries")]
    pub retries: u32,
    #[serde(default)]
    pub allow_side_effect_probes: bool,
}

impl Default for ConvertProbeConfig {
    fn default() -> Self {
        Self {
            timeout_seconds: DEFAULT_PROBE_TIMEOUT_SECONDS,
            retries: DEFAULT_PROBE_RETRIES,
            allow_side_effect_probes: false,
        }
    }
}

fn default_probe_timeout_seconds() -> u64 {
    DEFAULT_PROBE_TIMEOUT_SECONDS
}

fn default_probe_retries() -> u32 {
    DEFAULT_PROBE_RETRIES
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConvertV3Options {
    #[serde(default)]
    pub backend: Option<ConvertBackendName>,
    #[serde(default = "default_backend_auto")]
    pub backend_auto: bool,
    #[serde(default)]
    pub backend_config: ConvertBackendConfig,
}

impl Default for ConvertV3Options {
    fn default() -> Self {
        Self {
            backend: None,
            backend_auto: true,
            backend_config: ConvertBackendConfig::default(),
        }
    }
}

fn default_backend_auto() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContractTestOptions {
    #[serde(default)]
    pub allow_side_effects: bool,
    #[serde(default = "default_probe_timeout_seconds")]
    pub probe_timeout_seconds: u64,
    #[serde(default = "default_probe_retries")]
    pub probe_retries: u32,
}

impl Default for ContractTestOptions {
    fn default() -> Self {
        Self {
            allow_side_effects: false,
            probe_timeout_seconds: DEFAULT_PROBE_TIMEOUT_SECONDS,
            probe_retries: DEFAULT_PROBE_RETRIES,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackendHealthStatus {
    pub backend: ConvertBackendName,
    pub available: bool,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConvertBackendHealthReport {
    pub checked_at: DateTime<Utc>,
    pub statuses: Vec<BackendHealthStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeTool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolContractTest {
    pub probe: String,
    pub expected: String,
    pub method: String,
    #[serde(default = "default_contract_applicable")]
    pub applicable: bool,
}

fn default_contract_applicable() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDossier {
    pub name: String,
    pub explanation: String,
    pub recipe: Vec<String>,
    pub evidence: Vec<String>,
    pub confidence: f32,
    pub contract_tests: Vec<ToolContractTest>,
    #[serde(default, skip_serializing_if = "probe_inputs_is_empty")]
    pub probe_inputs: ProbeInputs,
    #[serde(default)]
    pub probe_input_source: ProbeInputSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ProbeInputs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub happy_path: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid_input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_effect_safety: Option<Value>,
}

fn probe_inputs_is_empty(inputs: &ProbeInputs) -> bool {
    inputs.happy_path.is_none()
        && inputs.invalid_input.is_none()
        && inputs.side_effect_safety.is_none()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProbeInputSource {
    Backend,
    #[default]
    Synthesized,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeValidationSpec {
    pub tool_name: String,
    pub contract_tests: Vec<ToolContractTest>,
    #[serde(default, skip_serializing_if = "probe_inputs_is_empty")]
    pub probe_inputs: ProbeInputs,
    #[serde(default)]
    pub probe_input_source: ProbeInputSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowContextInput {
    pub name: String,
    pub guidance: String,
    #[serde(default = "default_context_required")]
    pub required: bool,
}

fn default_context_required() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeWorkflowStep {
    pub title: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowSkillSpec {
    pub id: String,
    pub title: String,
    pub goal: String,
    pub when_to_use: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trigger_phrases: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub origin_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prerequisite_workflows: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub followup_workflows: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_context: Vec<WorkflowContextInput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_acquisition: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branching_rules: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_and_ask: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_steps: Vec<NativeWorkflowStep>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub return_contract: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guardrails: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    pub confidence: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServerGate {
    Ready,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerDossier {
    pub generated_at: DateTime<Utc>,
    pub format_version: u32,
    pub server: MCPServerProfile,
    pub runtime_tools: Vec<RuntimeTool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_validations: Vec<RuntimeValidationSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workflow_skills: Vec<WorkflowSkillSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_dossiers: Vec<ToolDossier>,
    pub server_gate: ServerGate,
    pub gate_reasons: Vec<String>,
    pub backend_used: String,
    pub backend_fallback_used: bool,
    pub backend_diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DossierBundle {
    pub format_version: u32,
    pub generated_at: DateTime<Utc>,
    pub dossiers: Vec<ServerDossier>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuildServerResult {
    pub server_id: String,
    pub orchestrator_skill_path: PathBuf,
    pub tool_skill_paths: Vec<PathBuf>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuildResult {
    pub generated_at: DateTime<Utc>,
    pub skills_dir: PathBuf,
    pub servers: Vec<BuildServerResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContractProbeResult {
    pub probe: String,
    pub passed: bool,
    pub skipped: bool,
    #[serde(default)]
    pub executed: bool,
    pub details: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_args_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<ProbeErrorKind>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProbeErrorKind {
    Timeout,
    McpError,
    Transport,
    SchemaGap,
    Unsafe,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContractToolResult {
    pub tool: String,
    pub passed: bool,
    pub probes: Vec<ContractProbeResult>,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContractServerReport {
    pub server_id: String,
    pub passed: bool,
    pub introspection_ok: bool,
    pub missing_runtime_tools: Vec<String>,
    pub tools: Vec<ContractToolResult>,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContractTestReport {
    pub generated_at: DateTime<Utc>,
    pub passed: bool,
    pub servers: Vec<ContractServerReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApplyServerResult {
    pub server_id: String,
    pub applied: bool,
    pub orchestrator_skill_path: PathBuf,
    pub tool_skill_paths: Vec<PathBuf>,
    pub mcp_config_backup: Option<PathBuf>,
    pub mcp_config_updated: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApplyResultV3 {
    pub generated_at: DateTime<Utc>,
    pub skills_dir: PathBuf,
    pub servers: Vec<ApplyServerResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OneShotV3Result {
    pub generated_at: DateTime<Utc>,
    pub dossier: DossierBundle,
    pub contract_test: ContractTestReport,
    pub apply: ApplyResultV3,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackendSelection {
    pub selected: ConvertBackendName,
    pub fallback: Option<ConvertBackendName>,
    pub auto_mode: bool,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ManifestToolSkill {
    tool_name: String,
    skill_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct SkillParityManifest {
    format_version: u32,
    generated_at: DateTime<Utc>,
    server_id: String,
    server_name: String,
    #[serde(default)]
    orchestrator_skill: Option<String>,
    #[serde(default)]
    required_tools: Vec<String>,
    #[serde(default)]
    tool_skills: Vec<ManifestToolSkill>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    required_tool_hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ToolSpec {
    name: String,
    description: Option<String>,
    input_schema: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct ToolEnrichmentResponse {
    tools: Vec<ToolEnrichmentEntry>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct ToolEnrichmentEntry {
    name: String,
    #[serde(default)]
    what_it_does: Option<String>,
    #[serde(default)]
    when_to_use: Option<String>,
    #[serde(default)]
    inputs_hint: Vec<String>,
    #[serde(default)]
    success_signals: Vec<String>,
    #[serde(default)]
    pitfalls: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct ToolSkillHint {
    what_it_does: Option<String>,
    when_to_use: Option<String>,
    inputs_hint: Vec<String>,
    success_signals: Vec<String>,
    pitfalls: Vec<String>,
}

#[derive(Debug, Clone)]
struct ConfigSource {
    label: String,
    path: PathBuf,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct CapabilityPlaybook {
    title: String,
    goal: String,
    tool_hints: Vec<String>,
    steps: Vec<String>,
}

#[derive(Debug, Clone)]
struct BackendContext {
    selection: BackendSelection,
    health: ConvertBackendHealthReport,
}

#[derive(Debug, Clone)]
struct ProbeFailure {
    kind: ProbeErrorKind,
    message: String,
    response_preview: Option<String>,
}

#[derive(Debug, Clone)]
struct McpToolCallOutcome {
    is_error: bool,
    details: String,
    response_preview: String,
    duration_ms: u64,
}
