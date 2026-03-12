use crate::backend::{
    prepare_backend_context, review_tool_conversion_with_backend,
    synthesize_tool_conversion_with_backend,
};
use crate::install::{remove_server_from_config, rollback_server_skill_files};
use crate::runtime::introspect_tool_specs;
use crate::skillset::{build_from_bundle, default_agents_skills_dir};
use crate::source::{default_sources, discover_from_sources};
use crate::{
    CatalogSourceResolutionStatus, CatalogSyncResult, ConvertBackendConfig, ConvertBackendName,
    MCPServerProfile, RuntimeTool, SourceKind, WorkflowSkillSpec,
};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use flate2::read::GzDecoder;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, hash_map::DefaultHasher};
use std::ffi::OsStr;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use tar::Archive;
use walkdir::{DirEntry, WalkDir};
use zip::ZipArchive;

const MAX_SUPPORTING_SNIPPETS: usize = 4;
const MAX_TEST_SNIPPETS: usize = 3;

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
    #[serde(default)]
    pub mcp_config_updated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
}

fn default_backend_auto() -> bool {
    true
}

fn inspect_installed_server(
    server_selector: &str,
    additional_paths: &[PathBuf],
) -> Result<MCPServerProfile> {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let sources = default_sources(&home, &cwd, additional_paths);
    let inventory = discover_from_sources(&sources)?;
    resolve_server(&inventory.servers, server_selector)
}

fn resolve_server(servers: &[MCPServerProfile], server_selector: &str) -> Result<MCPServerProfile> {
    let selector = server_selector.trim();
    if selector.is_empty() {
        bail!("Server selector must be non-empty.");
    }

    if let Some(found) = servers
        .iter()
        .find(|s| s.id.eq_ignore_ascii_case(selector))
        .cloned()
    {
        return Ok(found);
    }

    let mut by_name = servers
        .iter()
        .filter(|s| s.name.eq_ignore_ascii_case(selector))
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

pub fn synthesize_from_evidence(
    evidence: &EvidenceBundle,
    options: &RunOptions,
) -> Result<SynthesisReport> {
    let backend_ctx = prepare_backend_context(
        options.backend,
        options.backend_auto,
        &options.backend_config,
    )?;

    let mut tool_conversions = Vec::with_capacity(evidence.tool_evidence.len());
    let mut diagnostics = backend_ctx.selection.diagnostics.clone();
    let mut blocked = false;
    let mut block_reasons = Vec::new();
    let mut selected_backend = backend_ctx.selection.selected;
    let mut fallback_used = false;

    for pack in &evidence.tool_evidence {
        match synthesize_tool_conversion_with_backend(
            selected_backend,
            &evidence.server,
            pack,
            options.backend_config.timeout_seconds,
        ) {
            Ok(draft) => tool_conversions.push(draft),
            Err(err) => {
                if options.backend_auto
                    && let Some(fallback) = backend_ctx.selection.fallback
                {
                    fallback_used = true;
                    selected_backend = fallback;
                    diagnostics.push(format!(
                        "Synthesis failed on '{}', retrying '{}' for tool '{}': {}",
                        backend_ctx.selection.selected, fallback, pack.tool_name, err
                    ));
                    match synthesize_tool_conversion_with_backend(
                        fallback,
                        &evidence.server,
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
        evidence: evidence.clone(),
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
    let run_root = create_run_root(server_selector)?;
    let artifacts = RunArtifacts {
        resolve: run_root.join("resolve.json"),
        snapshot: run_root.join("snapshot.json"),
        evidence: run_root.join("evidence.json"),
        synthesis: run_root.join("synthesis.json"),
        review: run_root.join("review.json"),
        verify: run_root.join("verify.json"),
    };

    let resolved = resolve_artifact(server_selector, additional_paths, catalog)?;
    write_json_artifact(&artifacts.resolve, &resolved)?;
    let snapshot = materialize_snapshot(&resolved, None)?;
    write_json_artifact(&artifacts.snapshot, &snapshot)?;
    let evidence = build_evidence_bundle(&resolved, &snapshot.snapshot, None)?;
    write_json_artifact(&artifacts.evidence, &evidence)?;
    let synthesis = synthesize_from_evidence(&evidence, options)?;
    write_json_artifact(&artifacts.synthesis, &synthesis)?;
    let review = review_conversion_bundle(&synthesis.bundle, options)?;
    write_json_artifact(&artifacts.review, &review)?;
    let verify = verify_conversion_bundle(&review.bundle);
    write_json_artifact(&artifacts.verify, &verify)?;

    if !verify.passed {
        return Ok(RunReport {
            generated_at: Utc::now(),
            status: "blocked".to_string(),
            artifacts,
            skills_dir: None,
            config_backup: None,
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
    let build = build_from_bundle(&review.bundle, Some(skills_dir.clone()))?;
    let built = build
        .servers
        .first()
        .context("Build result did not contain a generated server entry")?;
    let orchestrator = built.orchestrator_skill_path.clone();
    let tool_paths = built.tool_skill_paths.clone();
    let mut diagnostics = built.notes.clone();
    let mut config_backup = None;
    let mut config_updated = false;

    if !options.dry_run {
        match remove_server_from_config(
            &review.bundle.evidence.server.source_path,
            &review.bundle.evidence.server.name,
        ) {
            Ok((backup, true)) => {
                config_backup = backup;
                config_updated = true;
            }
            Ok((_backup, false)) => {
                rollback_server_skill_files(&orchestrator, &tool_paths);
                bail!(
                    "MCP config entry '{}' not found in {}. Rolled back generated skills to keep conversion atomic.",
                    review.bundle.evidence.server.name,
                    review.bundle.evidence.server.source_path.display()
                );
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
        config_backup,
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
            | "dist"
            | "build"
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

fn locate_tool_evidence(
    root: &Path,
    runtime_tool: &RuntimeTool,
    index: &[IndexedFile],
) -> ToolEvidencePack {
    let search_terms = tool_search_terms(&runtime_tool.name);
    let mut source_matches = vec![];
    let mut test_matches = vec![];
    let mut doc_matches = vec![];

    for indexed in index {
        if let Some(match_info) = score_indexed_file(indexed, &search_terms) {
            if is_test_path(&indexed.relative_path) {
                test_matches.push(match_info);
            } else if is_doc_path(&indexed.relative_path) {
                doc_matches.push(match_info);
            } else {
                source_matches.push(match_info);
            }
        }
    }

    source_matches.sort_by(|left, right| right.score.total_cmp(&left.score));
    test_matches.sort_by(|left, right| right.score.total_cmp(&left.score));
    doc_matches.sort_by(|left, right| right.score.total_cmp(&left.score));

    let registration = source_matches
        .iter()
        .find(|item| item.registration_like)
        .map(|item| snippet_from_match(root, item));
    let handler = source_matches
        .iter()
        .find(|item| !item.registration_like)
        .map(|item| snippet_from_match(root, item))
        .or_else(|| {
            source_matches
                .first()
                .map(|item| snippet_from_match(root, item))
        });
    let supporting_snippets = source_matches
        .iter()
        .skip(1)
        .take(MAX_SUPPORTING_SNIPPETS)
        .map(|item| snippet_from_match(root, item))
        .collect::<Vec<_>>();
    let test_snippets = test_matches
        .iter()
        .take(MAX_TEST_SNIPPETS)
        .map(|item| snippet_from_match(root, item))
        .collect::<Vec<_>>();
    let doc_snippets = doc_matches
        .iter()
        .take(MAX_TEST_SNIPPETS)
        .map(|item| snippet_from_match(root, item))
        .collect::<Vec<_>>();

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

    let signals = [
        registration.is_some(),
        handler.is_some(),
        !test_snippets.is_empty(),
        !doc_snippets.is_empty(),
    ]
    .into_iter()
    .filter(|flag| *flag)
    .count();
    let confidence = (signals as f32 / 4.0).max(0.25);

    let mut diagnostics = vec![];
    if registration.is_none() {
        diagnostics.push("No registration-like source match was found.".to_string());
    }
    if handler.is_none() {
        diagnostics.push("No handler-like source match was found.".to_string());
    }

    ToolEvidencePack {
        tool_name: runtime_tool.name.clone(),
        runtime_tool: runtime_tool.clone(),
        registration,
        handler,
        supporting_snippets,
        test_snippets,
        doc_snippets,
        required_inputs,
        diagnostics,
        confidence,
    }
}

#[derive(Debug)]
struct FileMatch {
    relative_path: PathBuf,
    score: f32,
    registration_like: bool,
    line_index: usize,
    contents: String,
}

fn tool_search_terms(tool_name: &str) -> Vec<String> {
    let mut out = vec![tool_name.to_ascii_lowercase()];
    out.push(tool_name.replace('_', "-").to_ascii_lowercase());
    out.push(tool_name.replace('_', " ").to_ascii_lowercase());
    out.sort();
    out.dedup();
    out
}

fn score_indexed_file(indexed: &IndexedFile, search_terms: &[String]) -> Option<FileMatch> {
    let haystack = indexed.contents.to_ascii_lowercase();
    let mut score = 0.0f32;
    let mut line_index = None;
    for term in search_terms {
        let term_score = haystack.matches(term).count() as f32;
        if term_score > 0.0 {
            score += term_score * 3.0;
            if line_index.is_none() {
                line_index = indexed
                    .contents
                    .lines()
                    .enumerate()
                    .find(|(_, line)| line.to_ascii_lowercase().contains(term))
                    .map(|(idx, _)| idx);
            }
        }
    }
    if score == 0.0 {
        return None;
    }

    let registration_like = Regex::new(r#"(?i)(tool|register|name|description|schema)"#)
        .expect("valid regex")
        .is_match(&indexed.contents);
    if registration_like {
        score += 2.0;
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
        line_index: line_index.unwrap_or(0),
        contents: indexed.contents.clone(),
    })
}

fn snippet_from_match(root: &Path, file_match: &FileMatch) -> SnippetEvidence {
    let _ = root;
    let lines = file_match.contents.lines().collect::<Vec<_>>();
    let start = file_match.line_index.saturating_sub(4);
    let end = (file_match.line_index + 5).min(lines.len());
    let excerpt = lines[start..end].join("\n");
    SnippetEvidence {
        file_path: file_match.relative_path.clone(),
        start_line: start + 1,
        end_line: end,
        excerpt,
        score: file_match.score,
    }
}

fn is_test_path(path: &Path) -> bool {
    let text = path.to_string_lossy().to_ascii_lowercase();
    text.contains("test") || text.contains("spec")
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
}
