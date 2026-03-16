use crate::backend::{
    map_low_confidence_tool_with_backend, prepare_backend_context,
    review_tool_conversion_with_backend, synthesize_tool_conversion_with_backend,
};
use crate::install::{remove_servers_from_config, rollback_server_skill_files};
use crate::runtime::introspect_tool_specs;
use crate::skillset::{build_from_bundle, default_agents_skills_dir};
use crate::{
    CatalogSourceResolutionStatus, CatalogSyncResult, ConvertBackendConfig, ConvertBackendName,
    MCPServerProfile, RuntimeTool, SourceKind, WorkflowSkillSpec, discover_inventory,
};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, hash_map::DefaultHasher};
use std::ffi::OsStr;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};
use tar::Archive;
use walkdir::{DirEntry, WalkDir};
use zip::ZipArchive;

const MAX_SUPPORTING_SNIPPETS: usize = 4;
const MAX_TEST_SNIPPETS: usize = 3;
const MAX_MAPPER_CANDIDATES: usize = 6;
const LOW_CONFIDENCE_THRESHOLD: f32 = 0.60;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactKind {
    LocalPath,
    NpmPackage,
    PypiPackage,
    RepositoryUrl,
    RemoteOnly,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactIdentity {
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedArtifact {
    pub generated_at: DateTime<Utc>,
    pub server: MCPServerProfile,
    pub kind: ArtifactKind,
    pub identity: ArtifactIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_root_hint: Option<PathBuf>,
    #[serde(default)]
    pub blocked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceSnapshot {
    pub generated_at: DateTime<Utc>,
    pub artifact: ResolvedArtifact,
    pub cache_root: PathBuf,
    pub source_root: PathBuf,
    #[serde(default)]
    pub reused_cache: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub manifest_paths: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotMaterialization {
    pub generated_at: DateTime<Utc>,
    pub snapshot: SourceSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnippetEvidence {
    pub file_path: PathBuf,
    pub start_line: usize,
    pub end_line: usize,
    pub excerpt: String,
    pub score: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum MapperRelevantFileRole {
    Registration,
    Handler,
    Supporting,
}

impl std::fmt::Display for MapperRelevantFileRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MapperRelevantFileRole::Registration => write!(f, "registration"),
            MapperRelevantFileRole::Handler => write!(f, "handler"),
            MapperRelevantFileRole::Supporting => write!(f, "supporting"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MapperRelevantFile {
    pub path: PathBuf,
    pub role: MapperRelevantFileRole,
    pub why: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MapperFallbackEvidence {
    pub backend: String,
    pub relevant_files: Vec<MapperRelevantFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolEvidencePack {
    pub tool_name: String,
    pub runtime_tool: RuntimeTool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration: Option<SnippetEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handler: Option<SnippetEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supporting_snippets: Vec<SnippetEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_snippets: Vec<SnippetEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub doc_snippets: Vec<SnippetEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_inputs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapper_fallback: Option<MapperFallbackEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvidenceBundle {
    pub generated_at: DateTime<Utc>,
    pub server: MCPServerProfile,
    pub artifact: ResolvedArtifact,
    pub snapshot: SourceSnapshot,
    pub runtime_tools: Vec<RuntimeTool>,
    pub tool_evidence: Vec<ToolEvidencePack>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolSemanticSummary {
    pub what_it_does: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_inputs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prerequisites: Vec<String>,
    pub side_effect_level: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub success_signals: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failure_modes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<PathBuf>,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelperScript {
    pub relative_path: PathBuf,
    pub body: String,
    #[serde(default)]
    pub executable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolConversionDraft {
    pub tool_name: String,
    pub semantic_summary: ToolSemanticSummary,
    pub workflow_skill: WorkflowSkillSpec,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub helper_scripts: Vec<HelperScript>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerConversionBundle {
    pub generated_at: DateTime<Utc>,
    pub evidence: EvidenceBundle,
    pub backend_used: String,
    pub backend_fallback_used: bool,
    pub tool_conversions: Vec<ToolConversionDraft>,
    #[serde(default)]
    pub blocked: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub block_reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SynthesisReport {
    pub generated_at: DateTime<Utc>,
    pub bundle: ServerConversionBundle,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewFinding {
    pub tool_name: String,
    pub severity: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewReport {
    pub generated_at: DateTime<Utc>,
    pub approved: bool,
    pub bundle: ServerConversionBundle,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<ReviewFinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifyIssue {
    pub tool_name: String,
    pub severity: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifyReport {
    pub generated_at: DateTime<Utc>,
    pub passed: bool,
    pub issues: Vec<VerifyIssue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RunOptions {
    #[serde(default)]
    pub backend: Option<ConvertBackendName>,
    #[serde(default = "default_backend_auto")]
    pub backend_auto: bool,
    #[serde(default)]
    pub backend_config: ConvertBackendConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills_dir: Option<PathBuf>,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunArtifacts {
    pub resolve: PathBuf,
    pub snapshot: PathBuf,
    pub evidence: PathBuf,
    pub synthesis: PathBuf,
    pub review: PathBuf,
    pub verify: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunReport {
    pub generated_at: DateTime<Utc>,
    pub status: String,
    pub artifacts: RunArtifacts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_backup: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config_backups: Vec<PathBuf>,
    #[serde(default)]
    pub mcp_config_updated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineRunStage {
    Resolve,
    Snapshot,
    Evidence,
    Synthesize,
    Review,
    Verify,
    WriteSkills,
    UpdateConfig,
}

impl std::fmt::Display for PipelineRunStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PipelineRunStage::Resolve => write!(f, "resolve"),
            PipelineRunStage::Snapshot => write!(f, "snapshot"),
            PipelineRunStage::Evidence => write!(f, "evidence"),
            PipelineRunStage::Synthesize => write!(f, "synthesize"),
            PipelineRunStage::Review => write!(f, "review"),
            PipelineRunStage::Verify => write!(f, "verify"),
            PipelineRunStage::WriteSkills => write!(f, "write-skills"),
            PipelineRunStage::UpdateConfig => write!(f, "update-config"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineProgressEventKind {
    Started,
    Heartbeat,
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineProgressOutcome {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipelineProgressUpdate {
    pub stage: PipelineRunStage,
    pub kind: PipelineProgressEventKind,
    pub outcome: Option<PipelineProgressOutcome>,
    pub step: usize,
    pub total_steps: usize,
    pub elapsed: Duration,
}

type PipelineProgressCallback = Arc<dyn Fn(PipelineProgressUpdate) + Send + Sync + 'static>;

fn default_backend_auto() -> bool {
    true
}

fn pipeline_progress_interval() -> Duration {
    std::env::var("MCPSMITH_PROGRESS_INTERVAL_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_secs(5))
}

fn emit_pipeline_progress(
    callback: Option<&PipelineProgressCallback>,
    update: PipelineProgressUpdate,
) {
    if let Some(callback) = callback {
        callback(update);
    }
}

fn run_pipeline_stage<T, F>(
    callback: Option<&PipelineProgressCallback>,
    stage: PipelineRunStage,
    step: usize,
    total_steps: usize,
    operation: F,
) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    let started_at = Instant::now();
    emit_pipeline_progress(
        callback,
        PipelineProgressUpdate {
            stage,
            kind: PipelineProgressEventKind::Started,
            outcome: None,
            step,
            total_steps,
            elapsed: Duration::from_secs(0),
        },
    );

    let heartbeat = callback.map(|callback| {
        let callback = Arc::clone(callback);
        let interval = pipeline_progress_interval();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                thread::sleep(interval);
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
                callback(PipelineProgressUpdate {
                    stage,
                    kind: PipelineProgressEventKind::Heartbeat,
                    outcome: None,
                    step,
                    total_steps,
                    elapsed: started_at.elapsed(),
                });
            }
        });
        (stop, handle)
    });

    let result = operation();

    if let Some((stop, handle)) = heartbeat {
        stop.store(true, Ordering::Relaxed);
        let _ = handle.join();
    }

    emit_pipeline_progress(
        callback,
        PipelineProgressUpdate {
            stage,
            kind: PipelineProgressEventKind::Finished,
            outcome: Some(if result.is_ok() {
                PipelineProgressOutcome::Succeeded
            } else {
                PipelineProgressOutcome::Failed
            }),
            step,
            total_steps,
            elapsed: started_at.elapsed(),
        },
    );

    result
}

fn inspect_installed_server(
    server_selector: &str,
    additional_paths: &[PathBuf],
) -> Result<MCPServerProfile> {
    let inventory = discover_inventory(additional_paths)?;
    resolve_server(&inventory.servers, server_selector)
}

fn resolve_server(servers: &[MCPServerProfile], server_selector: &str) -> Result<MCPServerProfile> {
    let selector = server_selector.trim();
    if selector.is_empty() {
        bail!("Server selector must be non-empty.");
    }

    if let Some(found) = servers
        .iter()
        .find(|s| s.matches_selector(selector))
        .cloned()
    {
        return Ok(found);
    }

    let mut by_name = servers
        .iter()
        .filter(|s| {
            s.configured_names()
                .iter()
                .any(|name| name.eq_ignore_ascii_case(selector))
        })
        .cloned()
        .collect::<Vec<_>>();

    if by_name.is_empty() {
        let known = servers.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        if known.is_empty() {
            bail!(
                "No MCP servers discovered in the inspected config paths. Pass --config with an MCP config file or add a local MCP entry first."
            );
        }
        bail!(
            "No MCP server matched '{selector}'. Known server ids: {}",
            known.join(", ")
        );
    }

    by_name.sort_by(|a, b| a.id.cmp(&b.id));
    if by_name.len() > 1 {
        let ids = by_name.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        bail!(
            "Server name '{selector}' is ambiguous. Use one of: {}",
            ids.join(", ")
        );
    }

    Ok(by_name.remove(0))
}

fn grouped_config_targets(server: &MCPServerProfile) -> Vec<(PathBuf, Vec<String>)> {
    let mut grouped = BTreeMap::<PathBuf, Vec<String>>::new();
    for config_ref in server.config_refs_or_primary() {
        let entry = grouped.entry(config_ref.source_path).or_default();
        if !entry
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&config_ref.server_name))
        {
            entry.push(config_ref.server_name);
        }
    }
    grouped.into_iter().collect()
}

pub fn resolve_artifact(
    server_selector: &str,
    additional_paths: &[PathBuf],
    catalog: Option<&CatalogSyncResult>,
) -> Result<ResolvedArtifact> {
    let server = inspect_installed_server(server_selector, additional_paths)?;
    let mut diagnostics = vec![];

    let mut resolved = if let Some(entrypoint) = server.source_grounding.entrypoint.as_ref() {
        let source_root_hint = find_local_project_root(entrypoint);
        ResolvedArtifact {
            generated_at: Utc::now(),
            server: server.clone(),
            kind: ArtifactKind::LocalPath,
            identity: ArtifactIdentity {
                value: entrypoint.display().to_string(),
                version: server.source_grounding.package_version.clone(),
                source_url: server.source_grounding.repository_url.clone(),
            },
            source_root_hint,
            blocked: false,
            block_reason: None,
            diagnostics,
        }
    } else if server.source_grounding.kind == SourceKind::NpmPackage
        && server.source_grounding.package_name.is_some()
    {
        ResolvedArtifact {
            generated_at: Utc::now(),
            server: server.clone(),
            kind: ArtifactKind::NpmPackage,
            identity: ArtifactIdentity {
                value: server
                    .source_grounding
                    .package_name
                    .clone()
                    .unwrap_or_default(),
                version: server.source_grounding.package_version.clone(),
                source_url: server.source_grounding.repository_url.clone(),
            },
            source_root_hint: None,
            blocked: false,
            block_reason: None,
            diagnostics,
        }
    } else if server.source_grounding.kind == SourceKind::PypiPackage
        && server.source_grounding.package_name.is_some()
    {
        ResolvedArtifact {
            generated_at: Utc::now(),
            server: server.clone(),
            kind: ArtifactKind::PypiPackage,
            identity: ArtifactIdentity {
                value: server
                    .source_grounding
                    .package_name
                    .clone()
                    .unwrap_or_default(),
                version: server.source_grounding.package_version.clone(),
                source_url: server.source_grounding.repository_url.clone(),
            },
            source_root_hint: None,
            blocked: false,
            block_reason: None,
            diagnostics,
        }
    } else if let Some(repository_url) = server.source_grounding.repository_url.clone() {
        ResolvedArtifact {
            generated_at: Utc::now(),
            server: server.clone(),
            kind: ArtifactKind::RepositoryUrl,
            identity: ArtifactIdentity {
                value: repository_url.clone(),
                version: server.source_grounding.package_version.clone(),
                source_url: Some(repository_url),
            },
            source_root_hint: None,
            blocked: false,
            block_reason: None,
            diagnostics,
        }
    } else if server.url.is_some() {
        ResolvedArtifact {
            generated_at: Utc::now(),
            server: server.clone(),
            kind: ArtifactKind::RemoteOnly,
            identity: ArtifactIdentity {
                value: server.url.clone().unwrap_or_default(),
                version: None,
                source_url: server.url.clone(),
            },
            source_root_hint: None,
            blocked: true,
            block_reason: Some(
                "Server is URL-backed and no installable source artifact was discovered."
                    .to_string(),
            ),
            diagnostics,
        }
    } else {
        ResolvedArtifact {
            generated_at: Utc::now(),
            server,
            kind: ArtifactKind::Unknown,
            identity: ArtifactIdentity {
                value: server_selector.to_string(),
                version: None,
                source_url: None,
            },
            source_root_hint: None,
            blocked: true,
            block_reason: Some(
                "Unable to resolve a source artifact from the local launch identity.".to_string(),
            ),
            diagnostics,
        }
    };

    if resolved.blocked
        && let Some(catalog) = catalog
        && let Some(match_server) = match_catalog_server(&resolved.server, catalog)
    {
        diagnostics = resolved.diagnostics.clone();
        diagnostics.push(format!(
            "Catalog fallback matched '{}'.",
            match_server.canonical_name
        ));
        let resolution = &match_server.source_resolution;
        resolved = match resolution.status {
            CatalogSourceResolutionStatus::Resolvable => ResolvedArtifact {
                generated_at: Utc::now(),
                server: resolved.server.clone(),
                kind: match resolution.kind {
                    Some(SourceKind::NpmPackage) => ArtifactKind::NpmPackage,
                    Some(SourceKind::PypiPackage) => ArtifactKind::PypiPackage,
                    Some(SourceKind::RepositoryUrl) => ArtifactKind::RepositoryUrl,
                    _ => ArtifactKind::Unknown,
                },
                identity: ArtifactIdentity {
                    value: resolution
                        .identity
                        .clone()
                        .unwrap_or_else(|| match_server.canonical_name.clone()),
                    version: resolved.server.source_grounding.package_version.clone(),
                    source_url: resolution.source_url.clone(),
                },
                source_root_hint: None,
                blocked: false,
                block_reason: None,
                diagnostics,
            },
            CatalogSourceResolutionStatus::RemoteOnly => ResolvedArtifact {
                generated_at: Utc::now(),
                server: resolved.server.clone(),
                kind: ArtifactKind::RemoteOnly,
                identity: ArtifactIdentity {
                    value: resolution
                        .identity
                        .clone()
                        .unwrap_or_else(|| match_server.canonical_name.clone()),
                    version: None,
                    source_url: resolution.source_url.clone(),
                },
                source_root_hint: None,
                blocked: true,
                block_reason: Some(
                    "Catalog record only exposes a remote deployment, not source code.".to_string(),
                ),
                diagnostics,
            },
            _ => resolved,
        };
    }

    Ok(resolved)
}

pub fn materialize_snapshot(
    artifact: &ResolvedArtifact,
    cache_root: Option<PathBuf>,
) -> Result<SnapshotMaterialization> {
    if artifact.blocked {
        bail!(
            "{}",
            artifact
                .block_reason
                .clone()
                .unwrap_or_else(|| "Artifact resolution is blocked.".to_string())
        );
    }

    let root = cache_root.unwrap_or_else(default_snapshot_cache_root);
    fs::create_dir_all(&root).with_context(|| format!("Failed to create {}", root.display()))?;
    let cache_dir = root.join(snapshot_cache_key(artifact));
    let metadata_path = cache_dir.join("snapshot.json");
    let source_root = cache_dir.join("source");

    if metadata_path.exists() && source_root.exists() {
        let mut snapshot: SourceSnapshot = serde_json::from_str(
            &fs::read_to_string(&metadata_path)
                .with_context(|| format!("Failed to read {}", metadata_path.display()))?,
        )
        .with_context(|| format!("Failed to parse {}", metadata_path.display()))?;
        snapshot.reused_cache = true;
        return Ok(SnapshotMaterialization {
            generated_at: Utc::now(),
            snapshot,
        });
    }

    if cache_dir.exists() {
        fs::remove_dir_all(&cache_dir)
            .with_context(|| format!("Failed to clear {}", cache_dir.display()))?;
    }
    fs::create_dir_all(&source_root)
        .with_context(|| format!("Failed to create {}", source_root.display()))?;

    let mut diagnostics = vec![];
    match artifact.kind {
        ArtifactKind::LocalPath => snapshot_local_path(artifact, &source_root)?,
        ArtifactKind::RepositoryUrl => {
            snapshot_repository(artifact, &source_root, &mut diagnostics)?
        }
        ArtifactKind::NpmPackage => snapshot_npm_package(artifact, &source_root, &mut diagnostics)?,
        ArtifactKind::PypiPackage => {
            snapshot_pypi_package(artifact, &source_root, &mut diagnostics)?
        }
        ArtifactKind::RemoteOnly | ArtifactKind::Unknown => {
            bail!(
                "Artifact kind '{}' cannot be snapshotted.",
                artifact.identity.value
            )
        }
    }

    let manifest_paths = discover_manifest_paths(&source_root);
    let snapshot = SourceSnapshot {
        generated_at: Utc::now(),
        artifact: artifact.clone(),
        cache_root: cache_dir.clone(),
        source_root: source_root.clone(),
        reused_cache: false,
        manifest_paths,
        diagnostics,
    };
    fs::write(
        &metadata_path,
        format!("{}\n", serde_json::to_string_pretty(&snapshot)?),
    )
    .with_context(|| format!("Failed to write {}", metadata_path.display()))?;

    Ok(SnapshotMaterialization {
        generated_at: Utc::now(),
        snapshot,
    })
}

pub fn build_evidence_bundle(
    artifact: &ResolvedArtifact,
    snapshot: &SourceSnapshot,
    tool_filter: Option<&str>,
) -> Result<EvidenceBundle> {
    let runtime_tools = introspect_tool_specs(&artifact.server)?
        .into_iter()
        .map(|tool| RuntimeTool {
            name: tool.name,
            description: tool.description,
            input_schema: tool.input_schema,
        })
        .collect::<Vec<_>>();
    let selected_runtime_tools = runtime_tools
        .iter()
        .filter(|tool| {
            tool_filter
                .map(|filter| tool.name.eq_ignore_ascii_case(filter))
                .unwrap_or(true)
        })
        .cloned()
        .collect::<Vec<_>>();

    if selected_runtime_tools.is_empty() {
        bail!("No runtime tools matched the selected filter.");
    }

    let index = index_snapshot(&snapshot.source_root)?;
    let tool_evidence = selected_runtime_tools
        .iter()
        .map(|tool| locate_tool_evidence(&snapshot.source_root, tool, &index))
        .collect::<Vec<_>>();

    Ok(EvidenceBundle {
        generated_at: Utc::now(),
        server: artifact.server.clone(),
        artifact: artifact.clone(),
        snapshot: snapshot.clone(),
        runtime_tools: selected_runtime_tools,
        tool_evidence,
        diagnostics: vec![],
    })
}

fn enrich_low_confidence_evidence(
    evidence: &EvidenceBundle,
    backend_name: ConvertBackendName,
    timeout_seconds: u64,
) -> Result<EvidenceBundle> {
    let index = index_snapshot(&evidence.snapshot.source_root)?;
    let mut enriched = evidence.clone();

    for pack in &mut enriched.tool_evidence {
        if pack.confidence >= LOW_CONFIDENCE_THRESHOLD {
            continue;
        }

        let match_set = collect_tool_match_set(&pack.runtime_tool, &index);
        let candidates = mapper_candidates(&match_set);
        if candidates.is_empty() {
            enriched.diagnostics.push(format!(
                "Mapper fallback skipped for '{}': no candidate files survived deterministic narrowing.",
                pack.tool_name
            ));
            continue;
        }

        match map_low_confidence_tool_with_backend(
            backend_name,
            &evidence.server,
            pack,
            &candidates,
            timeout_seconds,
        ) {
            Ok(fallback) if !fallback.relevant_files.is_empty() => {
                let runtime_tool = pack.runtime_tool.clone();
                *pack = build_tool_evidence_pack(&runtime_tool, &match_set, Some(fallback));
            }
            Ok(_) => enriched.diagnostics.push(format!(
                "Mapper fallback returned no additional files for '{}'.",
                pack.tool_name
            )),
            Err(err) => enriched.diagnostics.push(format!(
                "Mapper fallback failed for '{}': {}",
                pack.tool_name, err
            )),
        }
    }

    Ok(enriched)
}

pub fn synthesize_from_evidence(
    evidence: &EvidenceBundle,
    options: &RunOptions,
) -> Result<SynthesisReport> {
    let backend_ctx = prepare_backend_context(
        options.backend,
        options.backend_auto,
        &options.backend_config,
    )?;

    let mut diagnostics = backend_ctx.selection.diagnostics.clone();
    let enriched_evidence = match enrich_low_confidence_evidence(
        evidence,
        backend_ctx.selection.selected,
        options.backend_config.timeout_seconds,
    ) {
        Ok(enriched) => enriched,
        Err(err) => {
            diagnostics.push(format!("Low-confidence mapper preparation failed: {}", err));
            evidence.clone()
        }
    };
    let mut tool_conversions = Vec::with_capacity(enriched_evidence.tool_evidence.len());
    let mut blocked = false;
    let mut block_reasons = Vec::new();
    let selected_backend = backend_ctx.selection.selected;
    let mut fallback_used = false;

    for pack in &enriched_evidence.tool_evidence {
        match synthesize_tool_conversion_with_backend(
            selected_backend,
            &enriched_evidence.server,
            pack,
            options.backend_config.timeout_seconds,
        ) {
            Ok(draft) => tool_conversions.push(draft),
            Err(err) => {
                if options.backend_auto
                    && let Some(fallback) = backend_ctx.selection.fallback
                {
                    fallback_used = true;
                    diagnostics.push(format!(
                        "Synthesis failed on '{}', retrying '{}' for tool '{}': {}",
                        selected_backend, fallback, pack.tool_name, err
                    ));
                    match synthesize_tool_conversion_with_backend(
                        fallback,
                        &enriched_evidence.server,
                        pack,
                        options.backend_config.timeout_seconds,
                    ) {
                        Ok(draft) => tool_conversions.push(draft),
                        Err(fallback_err) => {
                            blocked = true;
                            block_reasons.push(format!(
                                "Tool '{}' synthesis failed on primary and fallback backends: {}",
                                pack.tool_name, fallback_err
                            ));
                        }
                    }
                } else {
                    blocked = true;
                    block_reasons.push(format!(
                        "Tool '{}' synthesis failed: {}",
                        pack.tool_name, err
                    ));
                }
            }
        }
    }

    let bundle = ServerConversionBundle {
        generated_at: Utc::now(),
        evidence: enriched_evidence,
        backend_used: selected_backend.to_string(),
        backend_fallback_used: fallback_used,
        tool_conversions,
        blocked,
        block_reasons,
        diagnostics,
    };

    Ok(SynthesisReport {
        generated_at: Utc::now(),
        bundle,
    })
}

pub fn review_conversion_bundle(
    bundle: &ServerConversionBundle,
    options: &RunOptions,
) -> Result<ReviewReport> {
    let backend_ctx = prepare_backend_context(
        options.backend,
        options.backend_auto,
        &options.backend_config,
    )?;
    let backend = backend_ctx.selection.selected;
    let evidence_by_tool = bundle
        .evidence
        .tool_evidence
        .iter()
        .map(|tool| (tool.tool_name.clone(), tool))
        .collect::<BTreeMap<_, _>>();

    let mut findings = vec![];
    let mut reviewed = bundle.clone();
    reviewed
        .diagnostics
        .extend(backend_ctx.selection.diagnostics);

    for draft in &mut reviewed.tool_conversions {
        let Some(pack) = evidence_by_tool.get(&draft.tool_name) else {
            findings.push(ReviewFinding {
                tool_name: draft.tool_name.clone(),
                severity: "error".to_string(),
                message: "Missing evidence pack for drafted tool.".to_string(),
            });
            reviewed.blocked = true;
            continue;
        };

        match review_tool_conversion_with_backend(
            backend,
            &reviewed.evidence.server,
            pack,
            draft,
            options.backend_config.timeout_seconds,
        ) {
            Ok(review) => {
                if !review.approved {
                    findings.extend(review.findings.into_iter().map(|message| ReviewFinding {
                        tool_name: draft.tool_name.clone(),
                        severity: "warning".to_string(),
                        message,
                    }));
                    if let Some(revised) = review.revised_draft {
                        *draft = revised;
                    } else {
                        reviewed.blocked = true;
                        reviewed.block_reasons.push(format!(
                            "Reviewer rejected '{}' without a usable revision.",
                            draft.tool_name
                        ));
                    }
                }
            }
            Err(err) => {
                reviewed.blocked = true;
                reviewed.block_reasons.push(format!(
                    "Reviewer failed for '{}': {}",
                    draft.tool_name, err
                ));
            }
        }
    }

    let approved = !reviewed.blocked;
    Ok(ReviewReport {
        generated_at: Utc::now(),
        approved,
        bundle: reviewed,
        findings,
    })
}

pub fn verify_conversion_bundle(bundle: &ServerConversionBundle) -> VerifyReport {
    let mut issues = vec![];
    let notes = vec![
        "Verification is artifact-level: skill format, grounding, and file/script references."
            .to_string(),
    ];

    for draft in &bundle.tool_conversions {
        verify_draft(&bundle.evidence.snapshot.source_root, draft, &mut issues);
    }

    if bundle.blocked {
        issues.push(VerifyIssue {
            tool_name: "server".to_string(),
            severity: "error".to_string(),
            message: bundle.block_reasons.join(" | "),
        });
    }

    VerifyReport {
        generated_at: Utc::now(),
        passed: !issues
            .iter()
            .any(|issue| issue.severity.eq_ignore_ascii_case("error")),
        issues,
        notes,
    }
}

pub fn run_pipeline(
    server_selector: &str,
    additional_paths: &[PathBuf],
    options: &RunOptions,
    catalog: Option<&CatalogSyncResult>,
) -> Result<RunReport> {
    run_pipeline_inner(server_selector, additional_paths, options, catalog, None)
}

pub fn run_pipeline_with_progress<F>(
    server_selector: &str,
    additional_paths: &[PathBuf],
    options: &RunOptions,
    catalog: Option<&CatalogSyncResult>,
    progress: F,
) -> Result<RunReport>
where
    F: Fn(PipelineProgressUpdate) + Send + Sync + 'static,
{
    run_pipeline_inner(
        server_selector,
        additional_paths,
        options,
        catalog,
        Some(Arc::new(progress)),
    )
}

fn run_pipeline_inner(
    server_selector: &str,
    additional_paths: &[PathBuf],
    options: &RunOptions,
    catalog: Option<&CatalogSyncResult>,
    progress: Option<PipelineProgressCallback>,
) -> Result<RunReport> {
    let run_root = create_run_root(server_selector)?;
    let artifacts = RunArtifacts {
        resolve: run_root.join("resolve.json"),
        snapshot: run_root.join("snapshot.json"),
        evidence: run_root.join("evidence.json"),
        synthesis: run_root.join("synthesis.json"),
        review: run_root.join("review.json"),
        verify: run_root.join("verify.json"),
    };

    let total_steps = if options.dry_run { 7 } else { 8 };
    let progress = progress.as_ref();

    let resolved = run_pipeline_stage(progress, PipelineRunStage::Resolve, 1, total_steps, || {
        let resolved = resolve_artifact(server_selector, additional_paths, catalog)?;
        write_json_artifact(&artifacts.resolve, &resolved)?;
        Ok(resolved)
    })?;
    let snapshot =
        run_pipeline_stage(progress, PipelineRunStage::Snapshot, 2, total_steps, || {
            let snapshot = materialize_snapshot(&resolved, None)?;
            write_json_artifact(&artifacts.snapshot, &snapshot)?;
            Ok(snapshot)
        })?;
    let evidence =
        run_pipeline_stage(progress, PipelineRunStage::Evidence, 3, total_steps, || {
            let evidence = build_evidence_bundle(&resolved, &snapshot.snapshot, None)?;
            write_json_artifact(&artifacts.evidence, &evidence)?;
            Ok(evidence)
        })?;
    let synthesis = run_pipeline_stage(
        progress,
        PipelineRunStage::Synthesize,
        4,
        total_steps,
        || {
            let synthesis = synthesize_from_evidence(&evidence, options)?;
            write_json_artifact(&artifacts.evidence, &synthesis.bundle.evidence)?;
            write_json_artifact(&artifacts.synthesis, &synthesis)?;
            Ok(synthesis)
        },
    )?;
    let review = run_pipeline_stage(progress, PipelineRunStage::Review, 5, total_steps, || {
        let review = review_conversion_bundle(&synthesis.bundle, options)?;
        write_json_artifact(&artifacts.review, &review)?;
        Ok(review)
    })?;
    let verify = run_pipeline_stage(progress, PipelineRunStage::Verify, 6, total_steps, || {
        let verify = verify_conversion_bundle(&review.bundle);
        write_json_artifact(&artifacts.verify, &verify)?;
        Ok(verify)
    })?;

    if !verify.passed {
        return Ok(RunReport {
            generated_at: Utc::now(),
            status: "blocked".to_string(),
            artifacts,
            skills_dir: None,
            config_backup: None,
            config_backups: vec![],
            mcp_config_updated: false,
            diagnostics: verify
                .issues
                .iter()
                .map(|issue| format!("{}: {}", issue.tool_name, issue.message))
                .collect(),
            next_action: Some("Inspect review and verify artifacts before retrying.".to_string()),
        });
    }

    let skills_dir = if options.dry_run {
        run_root.join("skills-preview")
    } else {
        options
            .skills_dir
            .clone()
            .unwrap_or_else(default_agents_skills_dir)
    };
    let built = run_pipeline_stage(
        progress,
        PipelineRunStage::WriteSkills,
        7,
        total_steps,
        || {
            let build = build_from_bundle(&review.bundle, Some(skills_dir.clone()))?;
            build
                .servers
                .first()
                .cloned()
                .context("Build result did not contain a generated server entry")
        },
    )?;
    let orchestrator = built.orchestrator_skill_path.clone();
    let tool_paths = built.tool_skill_paths.clone();
    let mut diagnostics = built.notes.clone();
    let mut config_backups = Vec::new();
    let mut config_updated = false;

    if !options.dry_run {
        let (backups, updated) = run_pipeline_stage(
            progress,
            PipelineRunStage::UpdateConfig,
            8,
            total_steps,
            || {
                let mut backups = Vec::new();
                let mut updated = false;

                for (config_path, server_names) in
                    grouped_config_targets(&review.bundle.evidence.server)
                {
                    match remove_servers_from_config(&config_path, &server_names) {
                        Ok((backup, removed_names)) => {
                            if removed_names.len() != server_names.len() {
                                rollback_server_skill_files(&orchestrator, &tool_paths);
                                bail!(
                                    "MCP config entries '{}' not found in {}. Rolled back generated skills to keep conversion atomic.",
                                    server_names.join(", "),
                                    config_path.display()
                                );
                            }
                            if let Some(backup) = backup {
                                backups.push(backup);
                            }
                            updated = true;
                        }
                        Err(err) => {
                            rollback_server_skill_files(&orchestrator, &tool_paths);
                            return Err(err).with_context(|| {
                                format!(
                                    "Failed to mutate MCP config for '{}'; generated skill files were rolled back.",
                                    review.bundle.evidence.server.id
                                )
                            });
                        }
                    }
                }

                Ok((backups, updated))
            },
        )?;
        config_backups = backups;
        config_updated = updated;
        diagnostics.push(format!(
            "Installed skills under {}.",
            orchestrator.parent().unwrap_or(&skills_dir).display()
        ));
    } else {
        diagnostics.push(format!(
            "Wrote preview skills under {}.",
            skills_dir.display()
        ));
    }

    Ok(RunReport {
        generated_at: Utc::now(),
        status: if options.dry_run {
            "dry-run".to_string()
        } else {
            "applied".to_string()
        },
        artifacts,
        skills_dir: Some(skills_dir),
        config_backup: config_backups.first().cloned(),
        config_backups,
        mcp_config_updated: config_updated,
        diagnostics,
        next_action: None,
    })
}

fn match_catalog_server<'a>(
    server: &MCPServerProfile,
    catalog: &'a CatalogSyncResult,
) -> Option<&'a crate::CatalogServer> {
    let name = server.name.to_ascii_lowercase();
    catalog.servers.iter().find(|entry| {
        entry.canonical_name.eq_ignore_ascii_case(&name)
            || entry.display_name.eq_ignore_ascii_case(&server.name)
            || entry
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(&server.name))
    })
}

fn default_snapshot_cache_root() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".mcpsmith")
        .join("cache")
        .join("snapshots")
}

fn snapshot_cache_key(artifact: &ResolvedArtifact) -> String {
    let mut hasher = DefaultHasher::new();
    artifact.kind.hash(&mut hasher);
    artifact.identity.value.hash(&mut hasher);
    artifact.identity.version.hash(&mut hasher);
    let digest = hasher.finish();
    format!(
        "{}-{:x}",
        crate::skillset::sanitize_slug(&artifact.server.name),
        digest
    )
}

fn snapshot_local_path(artifact: &ResolvedArtifact, destination_root: &Path) -> Result<()> {
    let source = artifact
        .source_root_hint
        .as_deref()
        .or_else(|| Path::new(&artifact.identity.value).parent())
        .ok_or_else(|| anyhow::anyhow!("Local artifact has no source root hint."))?;
    copy_recursively(source, destination_root)
}

fn snapshot_repository(
    artifact: &ResolvedArtifact,
    destination_root: &Path,
    diagnostics: &mut Vec<String>,
) -> Result<()> {
    let url = artifact
        .identity
        .source_url
        .as_deref()
        .or(Some(artifact.identity.value.as_str()))
        .ok_or_else(|| anyhow::anyhow!("Repository artifact is missing source URL."))?;
    let status = Command::new("git")
        .args(["clone", "--depth", "1", url])
        .arg(destination_root)
        .status()
        .with_context(|| format!("Failed to spawn git clone for {url}"))?;
    if !status.success() {
        bail!("git clone failed for {url} with status {status}");
    }

    if let Some(version) = artifact.identity.version.as_deref() {
        let checkout = Command::new("git")
            .arg("-C")
            .arg(destination_root)
            .args(["checkout", version])
            .status()
            .with_context(|| format!("Failed to spawn git checkout {version}"))?;
        if !checkout.success() {
            diagnostics.push(format!(
                "Repository checkout for version '{}' failed; kept default branch snapshot.",
                version
            ));
        }
    }
    Ok(())
}

fn snapshot_npm_package(
    artifact: &ResolvedArtifact,
    destination_root: &Path,
    diagnostics: &mut Vec<String>,
) -> Result<()> {
    let base = std::env::var("MCPSMITH_NPM_REGISTRY_BASE_URL")
        .unwrap_or_else(|_| "https://registry.npmjs.org".to_string());
    let package_name = &artifact.identity.value;
    let version = artifact.identity.version.as_deref().unwrap_or("latest");
    let url = format!(
        "{}/{}/{}",
        base.trim_end_matches('/'),
        url_encode_path_segment(package_name),
        url_encode_path_segment(version)
    );
    let manifest = fetch_json(&url)?;
    let tarball_url = manifest
        .get("dist")
        .and_then(|value| value.get("tarball"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    if let Some(tarball_url) = tarball_url {
        let mut bytes = Vec::new();
        ureq::get(&tarball_url)
            .set("User-Agent", "mcpsmith")
            .call()
            .with_context(|| format!("Failed to fetch npm tarball {tarball_url}"))?
            .into_reader()
            .read_to_end(&mut bytes)
            .with_context(|| format!("Failed to read npm tarball {tarball_url}"))?;
        extract_tar_gz(&bytes, destination_root)?;
        strip_single_top_level_dir(destination_root)?;
        return Ok(());
    }

    if artifact.identity.source_url.as_deref().is_some() {
        diagnostics.push(
            "npm registry manifest did not expose a tarball; fell back to repository snapshot."
                .to_string(),
        );
        return snapshot_repository(
            &ResolvedArtifact {
                kind: ArtifactKind::RepositoryUrl,
                ..artifact.clone()
            },
            destination_root,
            diagnostics,
        );
    }

    bail!(
        "npm package '{}' did not expose a tarball or repository URL.",
        package_name
    );
}

fn snapshot_pypi_package(
    artifact: &ResolvedArtifact,
    destination_root: &Path,
    diagnostics: &mut Vec<String>,
) -> Result<()> {
    let base =
        std::env::var("MCPSMITH_PYPI_BASE_URL").unwrap_or_else(|_| "https://pypi.org".to_string());
    let package_name = &artifact.identity.value;
    let url = if let Some(version) = artifact.identity.version.as_deref() {
        format!(
            "{}/pypi/{}/{}/json",
            base.trim_end_matches('/'),
            url_encode_path_segment(package_name),
            url_encode_path_segment(version)
        )
    } else {
        format!(
            "{}/pypi/{}/json",
            base.trim_end_matches('/'),
            url_encode_path_segment(package_name)
        )
    };
    let metadata = fetch_json(&url)?;
    let releases = metadata
        .get("urls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if let Some(file) = releases.iter().find(|entry| {
        entry
            .get("packagetype")
            .and_then(Value::as_str)
            .map(|value| value == "sdist")
            .unwrap_or(false)
    }) {
        let download_url = file
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("PyPI sdist entry is missing url"))?;
        let filename = file
            .get("filename")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();

        let mut bytes = Vec::new();
        ureq::get(download_url)
            .set("User-Agent", "mcpsmith")
            .call()
            .with_context(|| format!("Failed to fetch PyPI source distribution {download_url}"))?
            .into_reader()
            .read_to_end(&mut bytes)
            .with_context(|| format!("Failed to read PyPI source distribution {download_url}"))?;

        if filename.ends_with(".zip") {
            extract_zip(&bytes, destination_root)?;
        } else {
            extract_tar_gz(&bytes, destination_root)?;
        }
        strip_single_top_level_dir(destination_root)?;
        return Ok(());
    }

    if artifact.identity.source_url.as_deref().is_some() {
        diagnostics.push(
            "PyPI metadata did not expose an sdist; fell back to repository snapshot.".to_string(),
        );
        return snapshot_repository(
            &ResolvedArtifact {
                kind: ArtifactKind::RepositoryUrl,
                ..artifact.clone()
            },
            destination_root,
            diagnostics,
        );
    }

    bail!(
        "PyPI package '{}' did not expose a source distribution or repository URL.",
        package_name
    );
}

fn fetch_json(url: &str) -> Result<Value> {
    let response = ureq::get(url)
        .set("User-Agent", "mcpsmith")
        .call()
        .with_context(|| format!("Failed to fetch {url}"))?;
    response
        .into_json::<Value>()
        .with_context(|| format!("Failed to parse JSON from {url}"))
}

fn extract_tar_gz(bytes: &[u8], destination_root: &Path) -> Result<()> {
    let decoder = GzDecoder::new(Cursor::new(bytes));
    let mut archive = Archive::new(decoder);
    archive.unpack(destination_root).with_context(|| {
        format!(
            "Failed to unpack archive into {}",
            destination_root.display()
        )
    })
}

fn extract_zip(bytes: &[u8], destination_root: &Path) -> Result<()> {
    let reader = Cursor::new(bytes);
    let mut archive = ZipArchive::new(reader).context("Failed to open zip archive")?;
    for idx in 0..archive.len() {
        let mut file = archive.by_index(idx).context("Failed to read zip entry")?;
        let outpath = destination_root.join(file.name());
        if file.name().ends_with('/') {
            fs::create_dir_all(&outpath)
                .with_context(|| format!("Failed to create {}", outpath.display()))?;
            continue;
        }
        if let Some(parent) = outpath.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        let mut output = fs::File::create(&outpath)
            .with_context(|| format!("Failed to create {}", outpath.display()))?;
        std::io::copy(&mut file, &mut output)
            .with_context(|| format!("Failed to write {}", outpath.display()))?;
    }
    Ok(())
}

fn strip_single_top_level_dir(destination_root: &Path) -> Result<()> {
    let entries = fs::read_dir(destination_root)
        .with_context(|| format!("Failed to inspect {}", destination_root.display()))?
        .filter_map(|entry| entry.ok())
        .collect::<Vec<_>>();
    if entries.len() != 1 || !entries[0].path().is_dir() {
        return Ok(());
    }
    let nested = entries[0].path();
    let temp = destination_root.join(".tmp-unwrap");
    fs::rename(&nested, &temp).with_context(|| format!("Failed to move {}", nested.display()))?;
    for entry in
        fs::read_dir(&temp).with_context(|| format!("Failed to inspect {}", temp.display()))?
    {
        let entry = entry.with_context(|| format!("Failed to read {}", temp.display()))?;
        fs::rename(entry.path(), destination_root.join(entry.file_name())).with_context(|| {
            format!(
                "Failed to move {} into {}",
                entry.path().display(),
                destination_root.display()
            )
        })?;
    }
    fs::remove_dir_all(&temp).with_context(|| format!("Failed to remove {}", temp.display()))
}

fn copy_recursively(source: &Path, destination: &Path) -> Result<()> {
    for entry in WalkDir::new(source)
        .into_iter()
        .filter_entry(|entry| should_copy_entry(entry, destination))
    {
        let entry = entry.with_context(|| format!("Failed to walk {}", source.display()))?;
        let relative = entry
            .path()
            .strip_prefix(source)
            .with_context(|| format!("Failed to strip {}", source.display()))?;
        let target = destination.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)
                .with_context(|| format!("Failed to create {}", target.display()))?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create {}", parent.display()))?;
            }
            fs::copy(entry.path(), &target).with_context(|| {
                format!(
                    "Failed to copy {} to {}",
                    entry.path().display(),
                    target.display()
                )
            })?;
        }
    }
    Ok(())
}

fn should_copy_entry(entry: &DirEntry, destination: &Path) -> bool {
    if entry.depth() == 0 {
        return true;
    }
    if entry.path().starts_with(destination) {
        return false;
    }
    let name = entry.file_name().to_string_lossy();
    !matches!(
        name.as_ref(),
        ".git"
            | "node_modules"
            | "dist"
            | "build"
            | "target"
            | ".venv"
            | "__pycache__"
            | ".cargo"
            | ".rustup"
            | ".npm"
            | ".mcpsmith"
            | ".codex-runtime"
    )
}

fn discover_manifest_paths(root: &Path) -> Vec<PathBuf> {
    let candidates = ["package.json", "pyproject.toml", "Cargo.toml", "README.md"];
    WalkDir::new(root)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy();
            if candidates.iter().any(|candidate| *candidate == name) {
                entry.path().strip_prefix(root).ok().map(PathBuf::from)
            } else {
                None
            }
        })
        .collect()
}

#[derive(Debug)]
struct IndexedFile {
    relative_path: PathBuf,
    contents: String,
}

fn index_snapshot(root: &Path) -> Result<Vec<IndexedFile>> {
    let mut files = vec![];
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(should_include_entry)
        .filter_map(|entry| entry.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        if !is_text_candidate(entry.path()) {
            continue;
        }
        let contents = match fs::read_to_string(entry.path()) {
            Ok(contents) => contents,
            Err(_) => continue,
        };
        let relative_path = entry
            .path()
            .strip_prefix(root)
            .with_context(|| format!("Failed to strip {}", root.display()))?
            .to_path_buf();
        files.push(IndexedFile {
            relative_path,
            contents,
        });
    }
    Ok(files)
}

fn should_include_entry(entry: &DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy();
    !matches!(
        name.as_ref(),
        ".git"
            | "node_modules"
            | "target"
            | ".venv"
            | "__pycache__"
            | ".cargo"
            | ".rustup"
            | ".npm"
    )
}

fn is_text_candidate(path: &Path) -> bool {
    matches!(
        path.extension().and_then(OsStr::to_str).unwrap_or_default(),
        "rs" | "js"
            | "ts"
            | "tsx"
            | "mjs"
            | "cjs"
            | "py"
            | "md"
            | "toml"
            | "json"
            | "yaml"
            | "yml"
            | "sh"
    ) || path.file_name().and_then(OsStr::to_str) == Some("README")
}

#[derive(Debug)]
struct ToolMatchSet {
    required_inputs: Vec<String>,
    source_matches: Vec<FileMatch>,
    test_matches: Vec<FileMatch>,
    doc_matches: Vec<FileMatch>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ToolMapperCandidate {
    pub(crate) path: PathBuf,
    pub(crate) score: f32,
    pub(crate) registration_hint: bool,
    pub(crate) handler_hint: bool,
    pub(crate) excerpt: String,
}

fn required_inputs_from_runtime_tool(runtime_tool: &RuntimeTool) -> Vec<String> {
    let mut required_inputs = runtime_tool
        .input_schema
        .as_ref()
        .and_then(|schema| schema.get("required"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    required_inputs.sort();
    required_inputs.dedup();
    required_inputs
}

fn collect_tool_match_set(runtime_tool: &RuntimeTool, index: &[IndexedFile]) -> ToolMatchSet {
    let search_terms = tool_search_terms(&runtime_tool.name);
    let required_inputs = required_inputs_from_runtime_tool(runtime_tool);
    let mut source_matches = vec![];
    let mut test_matches = vec![];
    let mut doc_matches = vec![];

    for indexed in index {
        if let Some(match_info) = score_indexed_file(indexed, &search_terms, &required_inputs) {
            if is_test_path(&indexed.relative_path) {
                test_matches.push(match_info);
            } else if is_doc_path(&indexed.relative_path) {
                doc_matches.push(match_info);
            } else {
                source_matches.push(match_info);
            }
        }
    }

    source_matches.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    test_matches.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    doc_matches.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });

    ToolMatchSet {
        required_inputs,
        source_matches,
        test_matches,
        doc_matches,
    }
}

fn deterministic_registration_match(match_set: &ToolMatchSet) -> Option<&FileMatch> {
    let registration_match = match_set
        .source_matches
        .iter()
        .filter(|item| item.registration_like)
        .filter(|item| !looks_like_generated_bundle_path(&item.relative_path))
        .max_by(|left, right| left.registration_score.total_cmp(&right.registration_score));
    registration_match.or_else(|| {
        match_set
            .source_matches
            .iter()
            .filter(|item| item.registration_like)
            .max_by(|left, right| left.registration_score.total_cmp(&right.registration_score))
    })
}

fn deterministic_handler_match(match_set: &ToolMatchSet) -> Option<&FileMatch> {
    match_set
        .source_matches
        .iter()
        .filter(|item| item.handler_like && !looks_like_generated_bundle_path(&item.relative_path))
        .max_by(|left, right| left.handler_score.total_cmp(&right.handler_score))
        .or_else(|| {
            match_set
                .source_matches
                .iter()
                .filter(|item| item.handler_like)
                .max_by(|left, right| left.handler_score.total_cmp(&right.handler_score))
        })
}

fn snippet_from_selected_match(file_match: &FileMatch, role: MatchRole) -> SnippetEvidence {
    match role {
        MatchRole::Registration if file_match.registration_score > 0.0 => {
            snippet_from_match(file_match, MatchRole::Registration)
        }
        MatchRole::Handler if file_match.handler_score > 0.0 => {
            snippet_from_match(file_match, MatchRole::Handler)
        }
        _ => snippet_from_match(file_match, MatchRole::Best),
    }
}

fn mapper_file_matches<'a>(
    match_set: &'a ToolMatchSet,
    mapper_fallback: &MapperFallbackEvidence,
) -> (
    Option<&'a FileMatch>,
    Option<&'a FileMatch>,
    Vec<&'a FileMatch>,
) {
    let mut registration_match = None;
    let mut handler_match = None;
    let mut supporting_matches = vec![];

    for file in &mapper_fallback.relevant_files {
        let Some(found) = match_set
            .source_matches
            .iter()
            .find(|candidate| candidate.relative_path == file.path)
        else {
            continue;
        };
        match file.role {
            MapperRelevantFileRole::Registration => {
                if registration_match.is_none() {
                    registration_match = Some(found);
                }
            }
            MapperRelevantFileRole::Handler => {
                if handler_match.is_none() {
                    handler_match = Some(found);
                }
            }
            MapperRelevantFileRole::Supporting => {
                if !supporting_matches
                    .iter()
                    .any(|candidate: &&FileMatch| candidate.relative_path == found.relative_path)
                {
                    supporting_matches.push(found);
                }
            }
        }
    }

    (registration_match, handler_match, supporting_matches)
}

fn mapper_candidates(match_set: &ToolMatchSet) -> Vec<ToolMapperCandidate> {
    match_set
        .source_matches
        .iter()
        .take(MAX_MAPPER_CANDIDATES)
        .map(|item| ToolMapperCandidate {
            path: item.relative_path.clone(),
            score: item.score,
            registration_hint: item.registration_like,
            handler_hint: item.handler_like,
            excerpt: snippet_from_match(item, MatchRole::Best).excerpt,
        })
        .collect()
}

fn build_tool_evidence_pack(
    runtime_tool: &RuntimeTool,
    match_set: &ToolMatchSet,
    mapper_fallback: Option<MapperFallbackEvidence>,
) -> ToolEvidencePack {
    let deterministic_registration = deterministic_registration_match(match_set);
    let deterministic_handler = deterministic_handler_match(match_set);
    let (fallback_registration, fallback_handler, fallback_supporting) = mapper_fallback
        .as_ref()
        .map(|fallback| mapper_file_matches(match_set, fallback))
        .unwrap_or((None, None, vec![]));

    let registration_match = fallback_registration.or(deterministic_registration);
    let handler_match = fallback_handler.or(deterministic_handler);
    let registration =
        registration_match.map(|item| snippet_from_selected_match(item, MatchRole::Registration));
    let handler = handler_match.map(|item| snippet_from_selected_match(item, MatchRole::Handler));

    let mut supporting_snippets = vec![];
    for item in fallback_supporting {
        if registration_match
            .map(|registration| registration.relative_path == item.relative_path)
            .unwrap_or(false)
            || handler_match
                .map(|handler| handler.relative_path == item.relative_path)
                .unwrap_or(false)
        {
            continue;
        }
        supporting_snippets.push(snippet_from_selected_match(item, MatchRole::Best));
        if supporting_snippets.len() >= MAX_SUPPORTING_SNIPPETS {
            break;
        }
    }
    for item in &match_set.source_matches {
        if registration_match
            .map(|registration| registration.relative_path == item.relative_path)
            .unwrap_or(false)
            || handler_match
                .map(|handler| handler.relative_path == item.relative_path)
                .unwrap_or(false)
            || supporting_snippets
                .iter()
                .any(|snippet| snippet.file_path == item.relative_path)
        {
            continue;
        }
        supporting_snippets.push(snippet_from_match(item, MatchRole::Best));
        if supporting_snippets.len() >= MAX_SUPPORTING_SNIPPETS {
            break;
        }
    }

    let test_snippets = match_set
        .test_matches
        .iter()
        .take(MAX_TEST_SNIPPETS)
        .map(|item| snippet_from_match(item, MatchRole::Best))
        .collect::<Vec<_>>();
    let doc_snippets = match_set
        .doc_matches
        .iter()
        .take(MAX_TEST_SNIPPETS)
        .map(|item| snippet_from_match(item, MatchRole::Best))
        .collect::<Vec<_>>();

    let confidence = compute_tool_confidence(
        registration_match,
        handler_match,
        test_snippets.len(),
        doc_snippets.len(),
    );
    let mut diagnostics = build_tool_evidence_diagnostics(
        confidence,
        registration_match,
        handler_match,
        test_snippets.len(),
        doc_snippets.len(),
        &match_set.required_inputs,
    );
    if let Some(fallback) = mapper_fallback.as_ref() {
        diagnostics.push(format!(
            "Mapper fallback via {} returned {} relevant file(s).",
            fallback.backend,
            fallback.relevant_files.len()
        ));
        for file in &fallback.relevant_files {
            diagnostics.push(format!(
                "Mapper fallback: {}={} ({:.2}) - {}.",
                file.role,
                file.path.display(),
                file.confidence,
                file.why
            ));
        }
    }

    ToolEvidencePack {
        tool_name: runtime_tool.name.clone(),
        runtime_tool: runtime_tool.clone(),
        registration,
        handler,
        supporting_snippets,
        test_snippets,
        doc_snippets,
        required_inputs: match_set.required_inputs.clone(),
        mapper_fallback,
        diagnostics,
        confidence,
    }
}

fn locate_tool_evidence(
    root: &Path,
    runtime_tool: &RuntimeTool,
    index: &[IndexedFile],
) -> ToolEvidencePack {
    let match_set = collect_tool_match_set(runtime_tool, index);
    let _ = root;
    build_tool_evidence_pack(runtime_tool, &match_set, None)
}

#[derive(Debug)]
struct ToolSearchTerms {
    raw_terms: Vec<String>,
    compact_terms: Vec<String>,
    compact_tool: String,
}

#[derive(Debug)]
struct FileMatch {
    relative_path: PathBuf,
    score: f32,
    registration_like: bool,
    registration_score: f32,
    handler_score: f32,
    handler_like: bool,
    best_line_index: usize,
    registration_line_index: usize,
    handler_line_index: usize,
    contents: String,
}

#[derive(Debug, Clone, Copy)]
enum MatchRole {
    Best,
    Registration,
    Handler,
}

fn tool_search_terms(tool_name: &str) -> ToolSearchTerms {
    let normalized = tool_name.to_ascii_lowercase();
    let mut raw_terms = vec![
        normalized.clone(),
        normalized.replace(['-', ' '], "_"),
        normalized.replace(['_', ' '], "-"),
        normalized.replace(['_', '-'], " "),
    ];
    raw_terms.retain(|term| !term.is_empty());
    raw_terms.sort();
    raw_terms.dedup();

    let compact_tool = compact_ascii(tool_name);
    let mut compact_terms = vec![compact_tool.clone()];
    compact_terms.retain(|term| !term.is_empty() && !raw_terms.iter().any(|raw| raw == term));
    compact_terms.sort();
    compact_terms.dedup();

    ToolSearchTerms {
        raw_terms,
        compact_terms,
        compact_tool,
    }
}

fn compact_ascii(text: &str) -> String {
    text.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

fn unique_term_hits(text: &str, compact_text: &str, search_terms: &ToolSearchTerms) -> usize {
    search_terms
        .raw_terms
        .iter()
        .filter(|term| text.contains(term.as_str()))
        .count()
        + search_terms
            .compact_terms
            .iter()
            .filter(|term| compact_text.contains(term.as_str()))
            .count()
}

fn exact_path_component_match(path: &Path, compact_tool: &str) -> bool {
    path.file_stem()
        .and_then(OsStr::to_str)
        .map(|stem| compact_ascii(stem) == compact_tool)
        .unwrap_or(false)
        || path
            .parent()
            .into_iter()
            .flat_map(Path::components)
            .any(|component| {
                compact_ascii(component.as_os_str().to_string_lossy().as_ref()) == compact_tool
            })
}

fn has_named_path_component(path: &Path, names: &[&str]) -> bool {
    path.components().any(|component| {
        let value = component.as_os_str().to_string_lossy();
        names.iter().any(|name| value.eq_ignore_ascii_case(name))
    })
}

fn looks_like_generated_bundle_path(path: &Path) -> bool {
    let text = path.to_string_lossy().to_ascii_lowercase();
    has_named_path_component(path, &["third_party", "vendor", "vendors", "bundled"])
        || text.ends_with("bundle.js")
        || text.contains("-bundle.")
        || text.contains("_bundle.")
}

fn looks_like_tool_source_path(path: &Path) -> bool {
    has_named_path_component(
        path,
        &[
            "tools", "tool", "handlers", "handler", "commands", "command",
        ],
    )
}

fn is_registration_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.contains("\\\"name\\\"")
        && (trimmed.contains("\\\"inputschema\\\"") || trimmed.contains("\\\"input_schema\\\""))
    {
        return false;
    }
    if trimmed.contains("printf")
        && trimmed.contains("\"name\"")
        && (trimmed.contains("\"inputschema\"") || trimmed.contains("\"input_schema\""))
    {
        return false;
    }
    trimmed.contains("server.tool(")
        || trimmed.contains("server.tool (")
        || trimmed.contains("registertool")
        || trimmed.contains("register_tool")
        || trimmed.contains("definetool(")
        || trimmed.contains("definepagetool(")
        || trimmed.contains("@mcp.tool")
        || trimmed.contains("mcp.tool(")
        || trimmed.starts_with("id:")
        || trimmed.starts_with("mcp:")
        || trimmed.starts_with("toolid:")
        || (trimmed.contains("name")
            && (trimmed.contains("description")
                || trimmed.contains("inputschema")
                || trimmed.contains("input_schema")))
}

fn is_handler_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("export async function ")
        || trimmed.starts_with("export function ")
        || trimmed.starts_with("async function ")
        || trimmed.starts_with("function ")
        || trimmed.starts_with("const ") && trimmed.contains(" = async")
        || trimmed.starts_with("let ") && trimmed.contains(" = async")
        || trimmed.starts_with("async def ")
        || trimmed.starts_with("def ")
        || trimmed.starts_with("pub async fn ")
        || trimmed.starts_with("pub fn ")
        || trimmed.starts_with("async fn ")
        || trimmed.starts_with("fn ")
        || trimmed.starts_with("handler:")
        || trimmed.contains("handler: async")
        || trimmed.contains("logicfunction:")
        || ((trimmed.contains("=> {") || trimmed.contains("=>{"))
            && (trimmed.contains("server.tool(")
                || trimmed.contains("server.tool (")
                || trimmed.contains("registertool")
                || trimmed.contains("register_tool")))
}

fn has_registration_context(contents: &str, required_inputs: &[String]) -> bool {
    contents.contains("server.tool")
        || contents.contains("registertool")
        || contents.contains("register_tool")
        || contents.contains("definetool(")
        || contents.contains("definepagetool(")
        || contents.contains("@mcp.tool")
        || contents.contains("mcp.tool(")
        || contents.contains("\nid:")
        || contents.contains("\nmcp:")
        || contents.contains("\ntoolid:")
        || contents.contains("inputschema")
        || contents.contains("input_schema")
        || required_inputs.iter().any(|input| {
            let quoted = format!("\"{}\"", input.to_ascii_lowercase());
            contents.contains(&quoted)
        })
}

fn contextual_handler_bonus(
    contents: &str,
    registration_line_index: usize,
    search_terms: &ToolSearchTerms,
) -> Option<(usize, f32)> {
    let lines = contents.lines().collect::<Vec<_>>();
    let mut best: Option<(usize, f32)> = None;
    for (idx, line) in lines.iter().enumerate().skip(registration_line_index) {
        let line_lower = line.to_ascii_lowercase();
        if idx > registration_line_index && is_tool_definition_boundary(&line_lower) {
            break;
        }
        if !is_handler_line(&line_lower) {
            continue;
        }

        let distance = idx.abs_diff(registration_line_index);
        let compact_line = compact_ascii(line);
        let line_hits = unique_term_hits(&line_lower, &compact_line, search_terms);
        let mut score = 9.0 - distance.min(120) as f32 * 0.02;
        if line_lower.contains("handler:") {
            score += 2.5;
        }
        if line_hits > 0 {
            score += line_hits as f32 * 1.5;
        }

        match best {
            Some((_, best_score)) if best_score >= score => {}
            _ => best = Some((idx, score)),
        }
    }
    best
}

fn is_tool_definition_boundary(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.contains("server.tool(")
        || trimmed.contains("server.tool (")
        || trimmed.contains("registertool")
        || trimmed.contains("register_tool")
        || trimmed.contains("definetool(")
        || trimmed.contains("definepagetool(")
        || trimmed.contains("@mcp.tool")
        || trimmed.contains("mcp.tool(")
}

fn score_indexed_file(
    indexed: &IndexedFile,
    search_terms: &ToolSearchTerms,
    required_inputs: &[String],
) -> Option<FileMatch> {
    let path_text = indexed.relative_path.to_string_lossy().to_ascii_lowercase();
    let compact_path = compact_ascii(&path_text);
    let haystack = indexed.contents.to_ascii_lowercase();
    let compact_haystack = compact_ascii(&indexed.contents);
    let path_hits = unique_term_hits(&path_text, &compact_path, search_terms);
    let content_hits = unique_term_hits(&haystack, &compact_haystack, search_terms);
    let exact_path_match =
        exact_path_component_match(&indexed.relative_path, &search_terms.compact_tool);
    if path_hits == 0 && content_hits == 0 && !exact_path_match {
        return None;
    }

    let file_support = path_hits > 0 || content_hits > 0 || exact_path_match;
    let required_input_hits = required_inputs
        .iter()
        .filter(|input| haystack.contains(&input.to_ascii_lowercase()))
        .count();
    let has_handler_context = indexed
        .contents
        .lines()
        .any(|line| is_handler_line(&line.to_ascii_lowercase()));
    let tool_source_path = looks_like_tool_source_path(&indexed.relative_path);
    let generated_bundle_path = looks_like_generated_bundle_path(&indexed.relative_path);

    let mut best_line_index = 0usize;
    let mut best_line_score = 0.0f32;
    let mut registration_line_index = 0usize;
    let mut best_registration_line_score = 0.0f32;
    let mut handler_line_index = 0usize;
    let mut best_handler_line_score = 0.0f32;

    for (idx, line) in indexed.contents.lines().enumerate() {
        let line_lower = line.to_ascii_lowercase();
        let compact_line = compact_ascii(line);
        let line_hits = unique_term_hits(&line_lower, &compact_line, search_terms);
        let line_required_input_hits = required_inputs
            .iter()
            .filter(|input| line_lower.contains(&input.to_ascii_lowercase()))
            .count();
        let base_score = line_hits as f32 * 2.5 + line_required_input_hits as f32;
        let registration_line = is_registration_line(&line_lower);
        let handler_line = is_handler_line(&line_lower);
        let general_score = base_score
            + if registration_line || handler_line {
                2.0 + if file_support { 1.0 } else { 0.0 }
            } else {
                0.0
            };
        if general_score > best_line_score {
            best_line_score = general_score;
            best_line_index = idx;
        }

        let registration_score = if registration_line && file_support {
            base_score + 6.0 + line_required_input_hits as f32 * 0.5
        } else {
            0.0
        };
        if registration_score > best_registration_line_score {
            best_registration_line_score = registration_score;
            registration_line_index = idx;
        }

        let handler_score = if handler_line && (line_hits > 0 || exact_path_match) {
            base_score + 6.0
        } else {
            0.0
        };
        if handler_score > best_handler_line_score {
            best_handler_line_score = handler_score;
            handler_line_index = idx;
        }
    }

    if best_handler_line_score == 0.0
        && best_registration_line_score > 0.0
        && let Some((idx, contextual_score)) =
            contextual_handler_bonus(&indexed.contents, registration_line_index, search_terms)
    {
        best_handler_line_score = contextual_score;
        handler_line_index = idx;
    }

    let registration_like = best_registration_line_score > 0.0
        || (exact_path_match && has_registration_context(&haystack, required_inputs));
    let handler_like = best_handler_line_score > 0.0 || (exact_path_match && has_handler_context);

    let mut score = content_hits as f32 * 2.0 + path_hits as f32 * 2.5 + best_line_score;
    if exact_path_match {
        score += 4.0;
    }
    if tool_source_path {
        score += 2.0;
    }
    if generated_bundle_path {
        score -= 8.0;
    }
    if registration_like {
        score += 2.0;
    }
    if handler_like {
        score += 2.0;
    }
    if required_input_hits > 0 {
        score += required_input_hits as f32 * 0.5;
    }
    if is_test_path(&indexed.relative_path) {
        score += 1.0;
    }
    if is_doc_path(&indexed.relative_path) {
        score += 0.5;
    }

    Some(FileMatch {
        relative_path: indexed.relative_path.clone(),
        score,
        registration_like,
        registration_score: if registration_like {
            score + best_registration_line_score + required_input_hits as f32 * 0.5
        } else {
            0.0
        },
        handler_score: if handler_like {
            score
                + best_handler_line_score
                + if path_hits > 0 { 1.5 } else { 0.0 }
                + if exact_path_match { 2.5 } else { 0.0 }
        } else {
            0.0
        },
        handler_like,
        best_line_index,
        registration_line_index,
        handler_line_index,
        contents: indexed.contents.clone(),
    })
}

fn snippet_from_match(file_match: &FileMatch, role: MatchRole) -> SnippetEvidence {
    let lines = file_match.contents.lines().collect::<Vec<_>>();
    let (line_index, score) = match role {
        MatchRole::Best => (file_match.best_line_index, file_match.score),
        MatchRole::Registration => (
            file_match.registration_line_index,
            file_match.registration_score,
        ),
        MatchRole::Handler => (file_match.handler_line_index, file_match.handler_score),
    };
    let start = line_index.saturating_sub(4);
    let end = (line_index + 5).min(lines.len());
    let excerpt = lines[start..end].join("\n");
    SnippetEvidence {
        file_path: file_match.relative_path.clone(),
        start_line: start + 1,
        end_line: end,
        excerpt,
        score,
    }
}

fn compute_tool_confidence(
    registration: Option<&FileMatch>,
    handler: Option<&FileMatch>,
    test_count: usize,
    doc_count: usize,
) -> f32 {
    let mut confidence: f32 = 0.15;
    if registration.is_some() {
        confidence += 0.30;
    }
    if handler.is_some() {
        confidence += 0.30;
    }
    if test_count > 0 {
        confidence += 0.12;
    }
    if doc_count > 0 {
        confidence += 0.08;
    }
    if registration
        .map(|item| item.registration_score >= 14.0)
        .unwrap_or(false)
    {
        confidence += 0.05;
    }
    if handler
        .map(|item| item.handler_score >= 14.0)
        .unwrap_or(false)
    {
        confidence += 0.05;
    }
    if registration
        .zip(handler)
        .map(|(reg, hand)| reg.relative_path != hand.relative_path)
        .unwrap_or(false)
    {
        confidence += 0.05;
    }
    confidence.clamp(0.15, 0.95)
}

fn confidence_label(confidence: f32) -> &'static str {
    if confidence >= 0.85 {
        "high"
    } else if confidence >= 0.60 {
        "medium"
    } else {
        "low"
    }
}

fn build_tool_evidence_diagnostics(
    confidence: f32,
    registration: Option<&FileMatch>,
    handler: Option<&FileMatch>,
    test_count: usize,
    doc_count: usize,
    required_inputs: &[String],
) -> Vec<String> {
    let registration_text = registration
        .map(|item| {
            format!(
                "{} ({:.2})",
                item.relative_path.display(),
                item.registration_score
            )
        })
        .unwrap_or_else(|| "missing".to_string());
    let handler_text = handler
        .map(|item| {
            format!(
                "{} ({:.2})",
                item.relative_path.display(),
                item.handler_score
            )
        })
        .unwrap_or_else(|| "missing".to_string());

    let mut diagnostics = vec![format!(
        "Confidence: {} ({:.2}); registration={}, handler={}, tests={}, docs={}.",
        confidence_label(confidence),
        confidence,
        registration_text,
        handler_text,
        test_count,
        doc_count
    )];
    if !required_inputs.is_empty() {
        diagnostics.push(format!(
            "Required inputs from runtime schema: {}.",
            required_inputs.join(", ")
        ));
    }
    if registration.is_none() {
        diagnostics.push("No registration-like source match was found.".to_string());
    }
    if handler.is_none() {
        diagnostics.push("No handler-like source match was found.".to_string());
    }
    diagnostics
}

fn is_test_path(path: &Path) -> bool {
    if path.components().any(|component| {
        matches!(
            component
                .as_os_str()
                .to_string_lossy()
                .to_ascii_lowercase()
                .as_str(),
            "test" | "tests" | "__tests__" | "spec" | "specs" | "__snapshots__"
        )
    }) {
        return true;
    }

    let stem = path
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if stem.ends_with(".test")
        || stem.ends_with(".spec")
        || stem.ends_with("_test")
        || stem.ends_with("_spec")
    {
        return true;
    }

    matches!(path.extension().and_then(OsStr::to_str), Some("py")) && stem.starts_with("test_")
}

fn is_doc_path(path: &Path) -> bool {
    let text = path.to_string_lossy().to_ascii_lowercase();
    text.contains("readme") || text.contains("docs/") || text.contains("examples/")
}

fn verify_draft(root: &Path, draft: &ToolConversionDraft, issues: &mut Vec<VerifyIssue>) {
    let placeholder_patterns = ["todo", "tbd", "placeholder", "<fill", "fixme"];
    let collected_fields = gather_workflow_text_fields(&draft.workflow_skill);
    if collected_fields.iter().any(|value| {
        let lower = value.to_ascii_lowercase();
        placeholder_patterns
            .iter()
            .any(|pattern| lower.contains(pattern))
    }) {
        issues.push(VerifyIssue {
            tool_name: draft.tool_name.clone(),
            severity: "error".to_string(),
            message: "Workflow contains unresolved placeholder text.".to_string(),
        });
    }

    if collected_fields.iter().any(|value| {
        let lower = value.to_ascii_lowercase();
        lower.contains("mcp__")
            || lower.contains("tools/list")
            || lower.contains("tools/call")
            || lower.contains("maps to")
    }) {
        issues.push(VerifyIssue {
            tool_name: draft.tool_name.clone(),
            severity: "error".to_string(),
            message: "Workflow still references MCP transport details.".to_string(),
        });
    }

    if draft.semantic_summary.citations.is_empty() {
        issues.push(VerifyIssue {
            tool_name: draft.tool_name.clone(),
            severity: "error".to_string(),
            message: "Semantic summary has no source citations.".to_string(),
        });
    }
    for citation in &draft.semantic_summary.citations {
        let full = root.join(citation);
        if !full.exists() {
            issues.push(VerifyIssue {
                tool_name: draft.tool_name.clone(),
                severity: "error".to_string(),
                message: format!(
                    "Citation '{}' does not exist in the source snapshot.",
                    citation.display()
                ),
            });
        }
    }

    for script in &draft.helper_scripts {
        if script.body.trim().is_empty() {
            issues.push(VerifyIssue {
                tool_name: draft.tool_name.clone(),
                severity: "error".to_string(),
                message: format!(
                    "Helper script '{}' has an empty body.",
                    script.relative_path.display()
                ),
            });
        }
    }

    for step in &draft.workflow_skill.native_steps {
        if let Some(command) = step.command.split_whitespace().next() {
            if command.starts_with("./") || command.starts_with("../") {
                if !draft
                    .helper_scripts
                    .iter()
                    .any(|script| script.relative_path == Path::new(command))
                {
                    issues.push(VerifyIssue {
                        tool_name: draft.tool_name.clone(),
                        severity: "error".to_string(),
                        message: format!(
                            "Command '{}' references a helper script that was not generated.",
                            command
                        ),
                    });
                }
            } else if is_executable_token(command).is_none() && !looks_like_shell_builtin(command) {
                issues.push(VerifyIssue {
                    tool_name: draft.tool_name.clone(),
                    severity: "warning".to_string(),
                    message: format!(
                        "Command '{}' is not present in PATH on this machine.",
                        command
                    ),
                });
            }
        }
    }
}

fn gather_workflow_text_fields(workflow: &WorkflowSkillSpec) -> Vec<String> {
    let mut out = vec![
        workflow.title.clone(),
        workflow.goal.clone(),
        workflow.when_to_use.clone(),
    ];
    out.extend(workflow.trigger_phrases.clone());
    out.extend(workflow.context_acquisition.clone());
    out.extend(workflow.branching_rules.clone());
    out.extend(workflow.stop_and_ask.clone());
    out.extend(workflow.verification.clone());
    out.extend(workflow.return_contract.clone());
    out.extend(workflow.guardrails.clone());
    out.extend(workflow.evidence.clone());
    for step in &workflow.native_steps {
        out.push(step.title.clone());
        out.push(step.command.clone());
        if let Some(details) = step.details.as_ref() {
            out.push(details.clone());
        }
    }
    out
}

fn looks_like_shell_builtin(token: &str) -> bool {
    matches!(
        token,
        "cd" | "echo" | "printf" | "test" | "[" | "if" | "then" | "fi" | "for" | "while"
    )
}

fn is_executable_token(token: &str) -> Option<PathBuf> {
    let path = Path::new(token);
    if path.is_absolute() && path.exists() {
        return Some(path.to_path_buf());
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(token);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn find_local_project_root(path: &Path) -> Option<PathBuf> {
    let start = if path.is_dir() { path } else { path.parent()? };
    let markers = [".git", "package.json", "pyproject.toml", "Cargo.toml"];
    for dir in start.ancestors() {
        if markers.iter().any(|marker| dir.join(marker).exists()) {
            return Some(dir.to_path_buf());
        }
    }
    Some(start.to_path_buf())
}

fn write_json_artifact<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(value)?))
        .with_context(|| format!("Failed to write {}", path.display()))
}

fn create_run_root(server_selector: &str) -> Result<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = cwd.join(".codex-runtime").join("runs").join(format!(
        "{}-{}",
        crate::skillset::sanitize_slug(server_selector),
        Utc::now().format("%Y%m%d-%H%M%S")
    ));
    fs::create_dir_all(&root).with_context(|| format!("Failed to create {}", root.display()))?;
    Ok(root)
}

fn url_encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => {
                encoded.push('%');
                encoded.push_str(&format!("{byte:02X}"));
            }
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConversionRecommendation, PermissionLevel, SourceGrounding};
    use serde_json::json;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Mutex, OnceLock};

    fn backend_command_env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn write_executable_script(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
        #[cfg(unix)]
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn sample_server(root: &Path) -> MCPServerProfile {
        MCPServerProfile {
            id: "fixture:demo".to_string(),
            name: "demo".to_string(),
            source_label: "fixture".to_string(),
            source_path: root.join("mcp.json"),
            purpose: "Demo".to_string(),
            command: Some("npx".to_string()),
            args: vec!["-y".to_string(), "@acme/demo".to_string()],
            url: None,
            env_keys: vec![],
            declared_tool_count: 1,
            permission_hints: vec![],
            inferred_permission: PermissionLevel::ReadOnly,
            recommendation: ConversionRecommendation::ReplaceCandidate,
            recommendation_reason: "read-only".to_string(),
            source_grounding: SourceGrounding {
                kind: SourceKind::NpmPackage,
                evidence_level: crate::SourceEvidenceLevel::ConfigOnly,
                inspected: false,
                entrypoint: None,
                package_name: Some("@acme/demo".to_string()),
                package_version: Some("1.0.0".to_string()),
                homepage: None,
                repository_url: Some("https://github.com/acme/demo".to_string()),
                inspected_paths: vec![],
                inspected_urls: vec![],
                derivation_evidence: vec![],
            },
            config_refs: vec![],
        }
    }

    fn runtime_tool(name: &str, required_inputs: &[&str]) -> RuntimeTool {
        RuntimeTool {
            name: name.to_string(),
            description: Some(format!("Tool {name}")),
            input_schema: Some(json!({
                "type": "object",
                "required": required_inputs,
                "properties": required_inputs
                    .iter()
                    .map(|input| ((*input).to_string(), json!({ "type": "string" })))
                    .collect::<serde_json::Map<String, serde_json::Value>>()
            })),
        }
    }

    fn sample_evidence_bundle(root: &Path, runtime_tools: Vec<RuntimeTool>) -> EvidenceBundle {
        let server = sample_server(root);
        let artifact = ResolvedArtifact {
            generated_at: Utc::now(),
            server: server.clone(),
            kind: ArtifactKind::NpmPackage,
            identity: ArtifactIdentity {
                value: "@acme/demo".to_string(),
                version: Some("1.0.0".to_string()),
                source_url: Some("https://github.com/acme/demo".to_string()),
            },
            source_root_hint: None,
            blocked: false,
            block_reason: None,
            diagnostics: vec![],
        };
        let snapshot = SourceSnapshot {
            generated_at: Utc::now(),
            artifact: artifact.clone(),
            cache_root: root.join("cache"),
            source_root: root.to_path_buf(),
            reused_cache: false,
            manifest_paths: vec![],
            diagnostics: vec![],
        };
        let tool_evidence = runtime_tools
            .iter()
            .cloned()
            .map(|runtime_tool| ToolEvidencePack {
                tool_name: runtime_tool.name.clone(),
                required_inputs: required_inputs_from_runtime_tool(&runtime_tool),
                runtime_tool,
                registration: None,
                handler: None,
                supporting_snippets: vec![],
                test_snippets: vec![],
                doc_snippets: vec![],
                mapper_fallback: None,
                diagnostics: vec![],
                confidence: 0.95,
            })
            .collect::<Vec<_>>();

        EvidenceBundle {
            generated_at: Utc::now(),
            server,
            artifact,
            snapshot,
            runtime_tools,
            tool_evidence,
            diagnostics: vec![],
        }
    }

    fn indexed_file(path: &str, contents: &str) -> IndexedFile {
        IndexedFile {
            relative_path: PathBuf::from(path),
            contents: contents.to_string(),
        }
    }

    #[test]
    fn resolve_artifact_prefers_local_identity() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("tool.sh");
        fs::write(&executable, "#!/bin/sh\n").unwrap();
        let server = MCPServerProfile {
            source_grounding: SourceGrounding {
                kind: SourceKind::LocalPath,
                evidence_level: crate::SourceEvidenceLevel::SourceInspected,
                inspected: true,
                entrypoint: Some(executable.clone()),
                package_name: None,
                package_version: None,
                homepage: None,
                repository_url: None,
                inspected_paths: vec![],
                inspected_urls: vec![],
                derivation_evidence: vec![],
            },
            ..sample_server(dir.path())
        };
        let artifact = ResolvedArtifact {
            generated_at: Utc::now(),
            server,
            kind: ArtifactKind::LocalPath,
            identity: ArtifactIdentity {
                value: executable.display().to_string(),
                version: None,
                source_url: None,
            },
            source_root_hint: Some(dir.path().to_path_buf()),
            blocked: false,
            block_reason: None,
            diagnostics: vec![],
        };
        assert_eq!(artifact.kind, ArtifactKind::LocalPath);
        assert!(!artifact.blocked);
    }

    #[test]
    fn verify_conversion_bundle_flags_missing_citations() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = ServerConversionBundle {
            generated_at: Utc::now(),
            evidence: EvidenceBundle {
                generated_at: Utc::now(),
                server: sample_server(dir.path()),
                artifact: ResolvedArtifact {
                    generated_at: Utc::now(),
                    server: sample_server(dir.path()),
                    kind: ArtifactKind::NpmPackage,
                    identity: ArtifactIdentity {
                        value: "@acme/demo".to_string(),
                        version: Some("1.0.0".to_string()),
                        source_url: None,
                    },
                    source_root_hint: None,
                    blocked: false,
                    block_reason: None,
                    diagnostics: vec![],
                },
                snapshot: SourceSnapshot {
                    generated_at: Utc::now(),
                    artifact: ResolvedArtifact {
                        generated_at: Utc::now(),
                        server: sample_server(dir.path()),
                        kind: ArtifactKind::NpmPackage,
                        identity: ArtifactIdentity {
                            value: "@acme/demo".to_string(),
                            version: Some("1.0.0".to_string()),
                            source_url: None,
                        },
                        source_root_hint: None,
                        blocked: false,
                        block_reason: None,
                        diagnostics: vec![],
                    },
                    cache_root: dir.path().join("cache"),
                    source_root: dir.path().to_path_buf(),
                    reused_cache: false,
                    manifest_paths: vec![],
                    diagnostics: vec![],
                },
                runtime_tools: vec![],
                tool_evidence: vec![],
                diagnostics: vec![],
            },
            backend_used: "codex".to_string(),
            backend_fallback_used: false,
            tool_conversions: vec![ToolConversionDraft {
                tool_name: "demo".to_string(),
                semantic_summary: ToolSemanticSummary {
                    what_it_does: "Demo".to_string(),
                    required_inputs: vec![],
                    prerequisites: vec![],
                    side_effect_level: "read-only".to_string(),
                    success_signals: vec![],
                    failure_modes: vec![],
                    citations: vec![],
                    confidence: 0.5,
                },
                workflow_skill: WorkflowSkillSpec {
                    id: "demo".to_string(),
                    title: "Demo".to_string(),
                    goal: "Do demo".to_string(),
                    when_to_use: "Use demo".to_string(),
                    trigger_phrases: vec![],
                    origin_tools: vec!["demo".to_string()],
                    prerequisite_workflows: vec![],
                    followup_workflows: vec![],
                    required_context: vec![],
                    context_acquisition: vec![],
                    branching_rules: vec![],
                    stop_and_ask: vec![],
                    native_steps: vec![crate::NativeWorkflowStep {
                        title: "Run".to_string(),
                        command: "printf 'demo'".to_string(),
                        details: None,
                    }],
                    verification: vec!["Check output".to_string()],
                    return_contract: vec!["Return output".to_string()],
                    guardrails: vec![],
                    evidence: vec![],
                    confidence: 0.5,
                },
                helper_scripts: vec![],
            }],
            blocked: false,
            block_reasons: vec![],
            diagnostics: vec![],
        };

        let verify = verify_conversion_bundle(&bundle);
        assert!(!verify.passed);
        assert!(
            verify
                .issues
                .iter()
                .any(|issue| issue.message.contains("no source citations"))
        );
    }

    #[test]
    fn locate_tool_evidence_matches_handler_from_tool_path_and_symbol_forms() {
        let dir = tempfile::tempdir().unwrap();
        let pack = locate_tool_evidence(
            dir.path(),
            &runtime_tool("list_pages", &["browser_id"]),
            &[
                indexed_file(
                    "src/index.ts",
                    r#"server.tool("list_pages", { description: "List pages", inputSchema: { type: "object", required: ["browser_id"] } }, async (args) => listPages(args));"#,
                ),
                indexed_file(
                    "src/list-pages.ts",
                    r#"export async function listPages(args) {
  return getOpenPages(args.browser_id);
}"#,
                ),
                indexed_file(
                    "tests/list-pages.spec.ts",
                    r#"it("lists pages", async () => {
  await callTool("list_pages", { browser_id: "demo" });
});"#,
                ),
            ],
        );

        assert_eq!(
            pack.registration
                .as_ref()
                .map(|snippet| snippet.file_path.clone()),
            Some(PathBuf::from("src/index.ts"))
        );
        assert_eq!(
            pack.handler
                .as_ref()
                .map(|snippet| snippet.file_path.clone()),
            Some(PathBuf::from("src/list-pages.ts"))
        );
        assert_eq!(pack.required_inputs, vec!["browser_id".to_string()]);
    }

    #[test]
    fn locate_tool_evidence_matches_camel_case_handler_symbols() {
        let dir = tempfile::tempdir().unwrap();
        let pack = locate_tool_evidence(
            dir.path(),
            &runtime_tool("read_graph", &[]),
            &[
                indexed_file(
                    "src/server.ts",
                    r#"server.tool("read_graph", { description: "Read graph" }, async () => readGraph());"#,
                ),
                indexed_file(
                    "src/handlers.ts",
                    r#"export async function readGraph() {
  return loadGraph();
}"#,
                ),
            ],
        );

        assert_eq!(
            pack.handler
                .as_ref()
                .map(|snippet| snippet.file_path.clone()),
            Some(PathBuf::from("src/handlers.ts"))
        );
    }

    #[test]
    fn locate_tool_evidence_reports_confidence_reasoning() {
        let dir = tempfile::tempdir().unwrap();
        let pack = locate_tool_evidence(
            dir.path(),
            &runtime_tool("archive_notes", &[]),
            &[indexed_file(
                "README.md",
                "The archive_notes tool stores notes for later retrieval.",
            )],
        );

        assert!(pack.confidence <= 0.35);
        assert!(
            pack.diagnostics
                .iter()
                .any(|line| line.contains("Confidence:")),
            "expected confidence summary in diagnostics, got {:?}",
            pack.diagnostics
        );
    }

    #[test]
    fn locate_tool_evidence_ignores_escaped_runtime_tool_payloads() {
        let dir = tempfile::tempdir().unwrap();
        let pack = locate_tool_evidence(
            dir.path(),
            &runtime_tool("execute", &["query"]),
            &[
                indexed_file(
                    "bin/mock-mcp.sh",
                    r#"printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{\"name\":\"execute\",\"description\":\"Tool execute\",\"inputSchema\":{\"type\":\"object\",\"required\":[\"query\"]}}]}}'"#,
                ),
                indexed_file(
                    "src/execute.ts",
                    r#"export async function runExecute(args) {
  return args.query;
}"#,
                ),
            ],
        );

        assert!(
            pack.registration.is_none(),
            "unexpected registration: {:?}",
            pack.registration
        );
        assert_eq!(
            pack.handler
                .as_ref()
                .map(|snippet| snippet.file_path.clone()),
            Some(PathBuf::from("src/execute.ts"))
        );
        assert!(pack.confidence < LOW_CONFIDENCE_THRESHOLD);
    }

    #[test]
    fn locate_tool_evidence_ignores_printf_runtime_tool_payloads() {
        let dir = tempfile::tempdir().unwrap();
        let pack = locate_tool_evidence(
            dir.path(),
            &runtime_tool("execute", &["query"]),
            &[
                indexed_file(
                    "bin/mock-mcp.sh",
                    r#"printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"execute","description":"Tool execute","inputSchema":{"type":"object","required":["query"]}}]}}'"#,
                ),
                indexed_file(
                    "src/execute.ts",
                    r#"export async function runExecute(args) {
  return args.query;
}"#,
                ),
            ],
        );

        assert!(
            pack.registration.is_none(),
            "unexpected registration: {:?}",
            pack.registration
        );
        assert_eq!(
            pack.handler
                .as_ref()
                .map(|snippet| snippet.file_path.clone()),
            Some(PathBuf::from("src/execute.ts"))
        );
        assert!(pack.confidence < LOW_CONFIDENCE_THRESHOLD);
    }

    #[test]
    fn mapper_candidates_stay_narrow_for_low_confidence_tools() {
        let mut index = vec![
            indexed_file(
                "src/tool_index.ts",
                r#"export const TOOL_REGISTRY = {
  execute: {
    summary: "Run execute",
    schema: {
      query: "string",
    },
    run: runExecute,
  },
};"#,
            ),
            indexed_file(
                "src/execute.ts",
                r#"export async function runExecute(args) {
  return args.query;
}"#,
            ),
        ];
        for idx in 0..8 {
            index.push(indexed_file(
                &format!("src/noise-{idx:02}.ts"),
                &format!(r#"export const note{idx} = "execute background reference {idx}";"#),
            ));
        }

        let match_set = collect_tool_match_set(&runtime_tool("execute", &["query"]), &index);
        let candidates = mapper_candidates(&match_set);
        let paths = candidates
            .iter()
            .map(|candidate| candidate.path.clone())
            .collect::<Vec<_>>();

        assert_eq!(candidates.len(), MAX_MAPPER_CANDIDATES);
        assert!(paths.contains(&PathBuf::from("src/tool_index.ts")));
        assert!(paths.contains(&PathBuf::from("src/execute.ts")));
        assert!(!paths.contains(&PathBuf::from("src/noise-07.ts")));
    }

    #[test]
    fn locate_tool_evidence_does_not_invent_handler_from_generic_helpers() {
        let dir = tempfile::tempdir().unwrap();
        let pack = locate_tool_evidence(
            dir.path(),
            &runtime_tool("execute", &["query"]),
            &[
                indexed_file(
                    "src/index.ts",
                    r#"server.tool("execute", { description: "Run execute", inputSchema: { type: "object", required: ["query"] } }, async (args) => runner(args));"#,
                ),
                indexed_file(
                    "src/source.rs",
                    r#"// execute command helpers live here
pub fn helper() {
  println!("helper");
}"#,
                ),
                indexed_file(
                    "src/command.ts",
                    r#"// execute pipeline internals
async function callBackend() {
  return "ok";
}"#,
                ),
            ],
        );

        assert!(
            pack.handler.is_none(),
            "unexpected handler: {:?}",
            pack.handler
        );
        assert!(
            pack.confidence < 0.70,
            "unexpected confidence: {}",
            pack.confidence
        );
        assert!(
            pack.diagnostics
                .iter()
                .any(|line| line.contains("No handler-like")),
            "expected missing handler diagnostic, got {:?}",
            pack.diagnostics
        );
    }

    #[test]
    fn locate_tool_evidence_matches_fastmcp_python_tool() {
        let dir = tempfile::tempdir().unwrap();
        let pack = locate_tool_evidence(
            dir.path(),
            &runtime_tool("read_docs", &["query"]),
            &[
                indexed_file(
                    "server.py",
                    r#"@mcp.tool()
def read_docs(query: str) -> str:
    """Read docs for a query."""
    return query"#,
                ),
                indexed_file(
                    "tests/test_server.py",
                    r#"def test_read_docs():
    result = call_tool("read_docs", {"query": "demo"})
    assert result == "demo""#,
                ),
            ],
        );

        assert_eq!(
            pack.registration
                .as_ref()
                .map(|snippet| snippet.file_path.clone()),
            Some(PathBuf::from("server.py"))
        );
        assert_eq!(
            pack.handler
                .as_ref()
                .map(|snippet| snippet.file_path.clone()),
            Some(PathBuf::from("server.py"))
        );
        assert_eq!(pack.required_inputs, vec!["query".to_string()]);
    }

    #[test]
    fn locate_tool_evidence_prefers_first_party_tool_files_over_third_party_bundles() {
        let dir = tempfile::tempdir().unwrap();
        let pack = locate_tool_evidence(
            dir.path(),
            &runtime_tool("click", &["uid"]),
            &[
                indexed_file(
                    "build/src/tools/input.js",
                    r#"export const click = definePageTool({
  name: "click",
  description: "Clicks on the provided element",
  schema: {
    uid: zod.string(),
  },
  handler: async (request) => request.params.uid,
});"#,
                ),
                indexed_file(
                    "build/src/third_party/lighthouse-devtools-mcp-bundle.js",
                    r#"export const click = definePageTool({
  name: "click",
  description: "Clicks on the provided element",
  inputSchema: {
    required: ["uid"],
  },
  handler: async (request) => {
    return request.params.uid;
  },
});

const clickHint = "click";
const clickSummary = "click click click";"#,
                ),
            ],
        );

        assert_eq!(
            pack.registration
                .as_ref()
                .map(|snippet| snippet.file_path.clone()),
            Some(PathBuf::from("build/src/tools/input.js"))
        );
        assert_eq!(
            pack.handler
                .as_ref()
                .map(|snippet| snippet.file_path.clone()),
            Some(PathBuf::from("build/src/tools/input.js"))
        );
    }

    #[test]
    fn locate_tool_evidence_matches_same_file_handler_when_tool_name_differs_from_file_stem() {
        let dir = tempfile::tempdir().unwrap();
        let pack = locate_tool_evidence(
            dir.path(),
            &runtime_tool("take_screenshot", &["filePath"]),
            &[
                indexed_file(
                    "build/src/tools/screenshot.js",
                    r#"export const screenshot = definePageTool({
  name: "take_screenshot",
  description: "Take a screenshot of the page or element.",
  schema: {
    filePath: zod.string().optional(),
  },
  handler: async (request, response, context) => {
    return context.saveFile("data", request.params.filePath);
  },
});"#,
                ),
                indexed_file(
                    "build/src/index.js",
                    r#"server.registerTool(tool.name, {
  description: tool.description,
  inputSchema: tool.schema,
}, async (params) => tool.handler(params));"#,
                ),
            ],
        );

        assert_eq!(
            pack.registration
                .as_ref()
                .map(|snippet| snippet.file_path.clone()),
            Some(PathBuf::from("build/src/tools/screenshot.js"))
        );
        assert_eq!(
            pack.handler
                .as_ref()
                .map(|snippet| snippet.file_path.clone()),
            Some(PathBuf::from("build/src/tools/screenshot.js"))
        );
    }

    #[test]
    fn locate_tool_evidence_matches_define_tool_factory_handlers_in_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let pack = locate_tool_evidence(
            dir.path(),
            &runtime_tool("evaluate_script", &["function"]),
            &[
                indexed_file(
                    "build/src/tools/script.js",
                    r#"export const evaluateScript = defineTool(cliArgs => {
  return {
    name: "evaluate_script",
    description: "Evaluate a script",
    schema: {
      function: zod.string(),
    },
    handler: async (request, response) => {
      response.appendResponseLine(request.params.function);
    },
  };
});"#,
                ),
                indexed_file(
                    "build/src/index.js",
                    r#"server.registerTool(tool.name, {
  description: tool.description,
  inputSchema: tool.schema,
}, async (params) => tool.handler(params));"#,
                ),
            ],
        );

        assert_eq!(
            pack.registration
                .as_ref()
                .map(|snippet| snippet.file_path.clone()),
            Some(PathBuf::from("build/src/tools/script.js"))
        );
        assert_eq!(
            pack.handler
                .as_ref()
                .map(|snippet| snippet.file_path.clone()),
            Some(PathBuf::from("build/src/tools/script.js"))
        );
    }

    #[test]
    fn locate_tool_evidence_finds_same_file_handlers_after_long_schema_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let pack = locate_tool_evidence(
            dir.path(),
            &runtime_tool("take_screenshot", &["filePath"]),
            &[
                indexed_file(
                    "build/src/tools/screenshot.js",
                    r#"export const screenshot = definePageTool({
  name: "take_screenshot",
  description: "Take a screenshot of the page or element.",
  schema: {
    alpha: zod.string().optional(),
    beta: zod.string().optional(),
    gamma: zod.string().optional(),
    delta: zod.string().optional(),
    epsilon: zod.string().optional(),
    zeta: zod.string().optional(),
    eta: zod.string().optional(),
    theta: zod.string().optional(),
    iota: zod.string().optional(),
    kappa: zod.string().optional(),
    lambda: zod.string().optional(),
    mu: zod.string().optional(),
    nu: zod.string().optional(),
    xi: zod.string().optional(),
    omicron: zod.string().optional(),
    pi: zod.string().optional(),
    rho: zod.string().optional(),
    sigma: zod.string().optional(),
    tau: zod.string().optional(),
    upsilon: zod.string().optional(),
    phi: zod.string().optional(),
    chi: zod.string().optional(),
    psi: zod.string().optional(),
    omega: zod.string().optional(),
    filePath: zod.string().optional(),
  },
  handler: async (request, response, context) => {
    return context.saveFile("data", request.params.filePath);
  },
});"#,
                ),
                indexed_file(
                    "build/src/index.js",
                    r#"server.registerTool(tool.name, {
  description: tool.description,
  inputSchema: tool.schema,
}, async (params) => tool.handler(params));"#,
                ),
            ],
        );

        assert_eq!(
            pack.registration
                .as_ref()
                .map(|snippet| snippet.file_path.clone()),
            Some(PathBuf::from("build/src/tools/screenshot.js"))
        );
        assert_eq!(
            pack.handler
                .as_ref()
                .map(|snippet| snippet.file_path.clone()),
            Some(PathBuf::from("build/src/tools/screenshot.js"))
        );
    }

    #[test]
    fn synthesize_from_evidence_retries_primary_backend_for_each_tool() {
        let _guard = backend_command_env_lock().lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let codex_script = dir.path().join("fake-codex.sh");
        let claude_script = dir.path().join("fake-claude.sh");

        write_executable_script(
            &codex_script,
            r#"#!/bin/sh
if [ "${1:-}" = "--version" ] || [ "${1:-}" = "-v" ] || [ "${1:-}" = "version" ]; then
  exit 0
fi

output_path=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output-last-message)
      shift
      output_path="$1"
      ;;
  esac
  shift
done

prompt="$(cat)"

case "$prompt" in
  *'"tool_name": "click"'*)
    echo "codex could not synthesize click" >&2
    exit 1
    ;;
  *'"tool_name": "close_page"'*)
    cat <<'EOF' > "$output_path"
{"semantic_summary":{"what_it_does":"Closes a page.","required_inputs":[],"prerequisites":[],"side_effect_level":"write","success_signals":["Page closed"],"failure_modes":["Page missing"],"citations":["src/close_page.ts"],"confidence":0.8},"workflow_skill":{"id":"close_page","title":"Close page","goal":"Close a page.","when_to_use":"Use when you need to close a page.","trigger_phrases":[],"origin_tools":["close_page"],"stop_and_ask":[],"native_steps":[{"title":"Close","command":"echo close"}],"verification":["Verify the page closes."],"return_contract":["Return the close result."],"confidence":0.8}}
EOF
    exit 0
    ;;
  *)
    echo "unexpected prompt" >&2
    exit 2
    ;;
esac
"#,
        );
        write_executable_script(
            &claude_script,
            r#"#!/bin/sh
if [ "${1:-}" = "--version" ] || [ "${1:-}" = "-v" ] || [ "${1:-}" = "version" ]; then
  exit 0
fi

prompt="$(cat)"

case "$prompt" in
  *'"tool_name": "click"'*)
    printf '%s\n' '{"output":"{\"semantic_summary\":{\"what_it_does\":\"Clicks an element.\",\"required_inputs\":[\"uid\"],\"prerequisites\":[],\"side_effect_level\":\"write\",\"success_signals\":[\"Click dispatched\"],\"failure_modes\":[\"Element missing\"],\"citations\":[\"src/click.ts\"],\"confidence\":0.8},\"workflow_skill\":{\"id\":\"click\",\"title\":\"Click\",\"goal\":\"Click an element.\",\"when_to_use\":\"Use when you need to click an element.\",\"trigger_phrases\":[],\"origin_tools\":[\"click\"],\"stop_and_ask\":[],\"native_steps\":[{\"title\":\"Click\",\"command\":\"echo click\"}],\"verification\":[\"Verify the click happens.\"],\"return_contract\":[\"Return the click result.\"],\"confidence\":0.8}}"}'
    exit 0
    ;;
  *)
    echo "claude should not handle this tool" >&2
    exit 1
    ;;
esac
"#,
        );

        unsafe {
            std::env::set_var("MCPSMITH_CODEX_COMMAND", &codex_script);
            std::env::set_var("MCPSMITH_CLAUDE_COMMAND", &claude_script);
        }

        let evidence = sample_evidence_bundle(
            dir.path(),
            vec![
                runtime_tool("click", &["uid"]),
                runtime_tool("close_page", &[]),
            ],
        );
        let options = RunOptions {
            backend_auto: true,
            ..RunOptions::default()
        };

        let synthesis = synthesize_from_evidence(&evidence, &options).unwrap();

        assert!(
            !synthesis.bundle.blocked,
            "expected synthesis to recover after one fallback, got {:?}",
            synthesis.bundle.block_reasons
        );
        assert_eq!(synthesis.bundle.backend_used, "codex");
        assert!(synthesis.bundle.backend_fallback_used);
        assert_eq!(synthesis.bundle.tool_conversions.len(), 2);

        unsafe {
            std::env::remove_var("MCPSMITH_CODEX_COMMAND");
            std::env::remove_var("MCPSMITH_CLAUDE_COMMAND");
        }
    }
}
