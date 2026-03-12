#![recursion_limit = "256"]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

mod backend;
mod catalog;
mod install;
mod pipeline;
mod runtime;
mod skillset;
mod source;

pub use backend::backend_health_report;
pub use catalog::{
    CatalogProvider, CatalogProviderRecord, CatalogProviderStatus, CatalogServer,
    CatalogSourceResolution, CatalogSourceResolutionStatus, CatalogStats, CatalogSyncOptions,
    CatalogSyncResult, catalog_stats, catalog_sync, load_cached_catalog_sync_result,
    load_catalog_sync_result,
};
pub use pipeline::{
    ArtifactIdentity, ArtifactKind, EvidenceBundle, HelperScript, ResolvedArtifact, ReviewFinding,
    ReviewReport, RunArtifacts, RunOptions, RunReport, ServerConversionBundle,
    SnapshotMaterialization, SnippetEvidence, SourceSnapshot, SynthesisReport, ToolConversionDraft,
    ToolEvidencePack, ToolSemanticSummary, VerifyIssue, VerifyReport, build_evidence_bundle,
    materialize_snapshot, resolve_artifact, review_conversion_bundle, run_pipeline,
    synthesize_from_evidence, verify_conversion_bundle,
};
pub use skillset::build_from_bundle;

const DEFAULT_BACKEND_TIMEOUT_SECONDS: u64 = 240;
const DEFAULT_BACKEND_CHUNK_SIZE: usize = 4;

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct BackendSelection {
    pub selected: ConvertBackendName,
    pub fallback: Option<ConvertBackendName>,
    pub auto_mode: bool,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ManifestToolSkill {
    pub tool_name: String,
    pub skill_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct SkillParityManifest {
    pub format_version: u32,
    pub generated_at: DateTime<Utc>,
    pub server_id: String,
    pub server_name: String,
    #[serde(default)]
    pub orchestrator_skill: Option<String>,
    #[serde(default)]
    pub required_tools: Vec<String>,
    #[serde(default)]
    pub tool_skills: Vec<ManifestToolSkill>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_tool_hints: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ConfigSource {
    pub label: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct BackendContext {
    pub selection: BackendSelection,
}
