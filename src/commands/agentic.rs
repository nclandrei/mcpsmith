use crate::config::{Config as AppConfig, ConvertBackendPreference as AppBackendPreference};
use anyhow::{Context, Result, bail};
use mcpsmith_core::{
    CatalogProvider, CatalogSyncOptions, RunOptions, ServerConversionBundle, VerifyReport,
    catalog_stats, catalog_sync, load_cached_catalog_sync_result, load_catalog_sync_result,
    materialize_snapshot, resolve_artifact, review_conversion_bundle, run_pipeline,
    synthesize_from_evidence, verify_conversion_bundle,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

fn map_backend_preference(pref: &AppBackendPreference) -> mcpsmith_core::ConvertBackendPreference {
    match pref {
        AppBackendPreference::Auto => mcpsmith_core::ConvertBackendPreference::Auto,
        AppBackendPreference::Codex => mcpsmith_core::ConvertBackendPreference::Codex,
        AppBackendPreference::Claude => mcpsmith_core::ConvertBackendPreference::Claude,
    }
}

pub fn parse_backend(raw: &str) -> Result<mcpsmith_core::ConvertBackendName> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "codex" => Ok(mcpsmith_core::ConvertBackendName::Codex),
        "claude" => Ok(mcpsmith_core::ConvertBackendName::Claude),
        other => bail!("Unsupported backend '{other}'. Expected: codex or claude."),
    }
}

fn parse_provider(raw: &str) -> Result<CatalogProvider> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "official" => Ok(CatalogProvider::Official),
        "smithery" => Ok(CatalogProvider::Smithery),
        "glama" => Ok(CatalogProvider::Glama),
        other => bail!("Unsupported catalog provider '{other}'."),
    }
}

fn run_options(
    backend: Option<&str>,
    backend_auto_flag: bool,
    skills_dir: Option<PathBuf>,
    dry_run: bool,
    app_config: &AppConfig,
) -> Result<RunOptions> {
    let backend = backend.map(parse_backend).transpose()?;
    let backend_auto = backend_auto_flag || backend.is_none();

    Ok(RunOptions {
        backend,
        backend_auto,
        backend_config: mcpsmith_core::ConvertBackendConfig {
            preference: map_backend_preference(&app_config.backend.preference),
            timeout_seconds: app_config.backend.timeout_seconds,
            chunk_size: app_config.backend.chunk_size,
        },
        skills_dir,
        dry_run,
    })
}

fn stage_output_path(stage: &str, server: &str) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    cwd.join(".codex-runtime").join("stages").join(format!(
        "{}-{}-{}.json",
        sanitize_stage_slug(stage),
        sanitize_stage_slug(server),
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    ))
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(value)?))
        .with_context(|| format!("Failed to write {}", path.display()))
}

fn print_json_or_human<T: Serialize>(
    json: bool,
    path: &Path,
    value: &T,
    human: impl FnOnce(),
) -> Result<()> {
    if json {
        #[derive(Serialize)]
        struct Envelope<'a, T: Serialize> {
            artifact_path: &'a Path,
            result: &'a T,
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&Envelope {
                artifact_path: path,
                result: value,
            })?
        );
        return Ok(());
    }
    human();
    println!("\nArtifact: {}", path.display());
    Ok(())
}

fn load_json_payload<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let value: Value = serde_json::from_str(
        &fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?,
    )
    .with_context(|| format!("Failed to parse {}", path.display()))?;
    let payload = value.get("result").cloned().unwrap_or(value);
    serde_json::from_value(payload).with_context(|| format!("Failed to decode {}", path.display()))
}

fn load_cached_catalog() -> Option<mcpsmith_core::CatalogSyncResult> {
    load_cached_catalog_sync_result(None).ok()
}

fn resolve_with_catalog(
    server: &str,
    config_paths: &[PathBuf],
) -> Result<mcpsmith_core::ResolvedArtifact> {
    let direct = resolve_artifact(server, config_paths, None)?;
    if !direct.blocked {
        return Ok(direct);
    }
    if let Some(catalog) = load_cached_catalog() {
        return resolve_artifact(server, config_paths, Some(&catalog));
    }
    Ok(direct)
}

pub fn run_catalog_sync_cmd(json: bool, providers: &[String]) -> Result<()> {
    let providers = if providers.is_empty() {
        vec![
            CatalogProvider::Official,
            CatalogProvider::Smithery,
            CatalogProvider::Glama,
        ]
    } else {
        providers
            .iter()
            .map(|provider| parse_provider(provider))
            .collect::<Result<Vec<_>>>()?
    };
    let result = catalog_sync(&CatalogSyncOptions {
        providers,
        cache_root: None,
    })?;
    let path = stage_output_path("catalog-sync", "all");
    write_json(&path, &result)?;
    print_json_or_human(json, &path, &result, || {
        println!("Catalog sync complete.");
        for provider in &result.providers {
            println!(
                "- {}: supported={} records={}",
                provider.provider, provider.supported, provider.record_count
            );
        }
        println!(
            "Unique servers={} resolvable={} remote-only={} unresolved={}",
            result.stats.unique_servers,
            result.stats.source_resolvable,
            result.stats.remote_only,
            result.stats.unresolved
        );
    })
}

pub fn run_catalog_stats_cmd(json: bool, from: Option<&Path>) -> Result<()> {
    let result = if let Some(path) = from {
        load_catalog_sync_result(path)?
    } else {
        catalog_sync(&CatalogSyncOptions::default())?
    };
    let stats = catalog_stats(&result);
    let path = stage_output_path("catalog-stats", "all");
    write_json(&path, &stats)?;
    print_json_or_human(json, &path, &stats, || {
        println!("Catalog stats");
        println!("Unique servers: {}", stats.unique_servers);
        println!("Source resolvable: {}", stats.source_resolvable);
        println!("Remote only: {}", stats.remote_only);
        println!("Unresolved: {}", stats.unresolved);
    })
}

pub fn run_resolve_cmd(server: &str, json: bool, config_paths: &[PathBuf]) -> Result<()> {
    let result = resolve_with_catalog(server, config_paths)?;
    let path = stage_output_path("resolve", server);
    write_json(&path, &result)?;
    print_json_or_human(json, &path, &result, || {
        println!("Resolved {}", result.server.id);
        println!("Artifact kind: {:?}", result.kind);
        println!("Identity: {}", result.identity.value);
        if let Some(version) = &result.identity.version {
            println!("Version: {}", version);
        }
        if result.blocked {
            println!(
                "Blocked: {}",
                result
                    .block_reason
                    .as_deref()
                    .unwrap_or("unknown resolution block")
            );
        }
    })?;
    if result.blocked {
        bail!(
            "{}",
            result
                .block_reason
                .as_deref()
                .unwrap_or("Artifact resolution is blocked.")
        );
    }
    Ok(())
}

pub fn run_snapshot_cmd(
    server: Option<&str>,
    from_resolve: Option<&Path>,
    json: bool,
    config_paths: &[PathBuf],
) -> Result<()> {
    let resolved = if let Some(path) = from_resolve {
        load_json_payload::<mcpsmith_core::ResolvedArtifact>(path)?
    } else {
        let selector = server.context("snapshot requires <server> or --from-resolve")?;
        resolve_with_catalog(selector, config_paths)?
    };
    let result = materialize_snapshot(&resolved, None)?;
    let path = stage_output_path("snapshot", resolved.server.name.as_str());
    write_json(&path, &result)?;
    print_json_or_human(json, &path, &result, || {
        println!("Snapshot ready for {}", result.snapshot.artifact.server.id);
        println!("Source root: {}", result.snapshot.source_root.display());
        println!("Cache reused: {}", result.snapshot.reused_cache);
    })
}

pub fn run_evidence_cmd(
    server: Option<&str>,
    from_snapshot: Option<&Path>,
    tool: Option<&str>,
    json: bool,
    config_paths: &[PathBuf],
) -> Result<()> {
    let snapshot = if let Some(path) = from_snapshot {
        load_json_payload::<mcpsmith_core::SnapshotMaterialization>(path)?
    } else {
        let selector = server.context("evidence requires <server> or --from-snapshot")?;
        let resolved = resolve_with_catalog(selector, config_paths)?;
        materialize_snapshot(&resolved, None)?
    };
    let resolved = snapshot.snapshot.artifact.clone();
    let result = mcpsmith_core::build_evidence_bundle(&resolved, &snapshot.snapshot, tool)?;
    let path = stage_output_path("evidence", result.server.name.as_str());
    write_json(&path, &result)?;
    print_json_or_human(json, &path, &result, || {
        println!("Evidence bundle for {}", result.server.id);
        for pack in &result.tool_evidence {
            println!(
                "- {}: confidence={:.2} registration={} handler={}",
                pack.tool_name,
                pack.confidence,
                pack.registration.is_some(),
                pack.handler.is_some()
            );
        }
    })
}

#[allow(clippy::too_many_arguments)]
pub fn run_synthesize_cmd(
    server: Option<&str>,
    from_evidence: Option<&Path>,
    tool: Option<&str>,
    json: bool,
    config_paths: &[PathBuf],
    backend: Option<&str>,
    backend_auto: bool,
    app_config: &AppConfig,
) -> Result<()> {
    let evidence = if let Some(path) = from_evidence {
        load_json_payload::<mcpsmith_core::EvidenceBundle>(path)?
    } else {
        let selector = server.context("synthesize requires <server> or --from-evidence")?;
        let resolved = resolve_with_catalog(selector, config_paths)?;
        let snapshot = materialize_snapshot(&resolved, None)?;
        mcpsmith_core::build_evidence_bundle(&resolved, &snapshot.snapshot, tool)?
    };
    let options = run_options(backend, backend_auto, None, true, app_config)?;
    let result = synthesize_from_evidence(&evidence, &options)?;
    let path = stage_output_path("synthesize", evidence.server.name.as_str());
    write_json(&path, &result)?;
    print_json_or_human(json, &path, &result, || {
        println!("Synthesis for {}", result.bundle.evidence.server.id);
        println!("Backend: {}", result.bundle.backend_used);
        println!("Drafted tools: {}", result.bundle.tool_conversions.len());
        if result.bundle.blocked {
            println!("Blocked: {}", result.bundle.block_reasons.join(" | "));
        }
    })?;
    if result.bundle.blocked {
        bail!("{}", result.bundle.block_reasons.join(" | "));
    }
    Ok(())
}

pub fn run_review_cmd(
    server: Option<&str>,
    from_bundle: Option<&Path>,
    json: bool,
    config_paths: &[PathBuf],
    backend: Option<&str>,
    backend_auto: bool,
    app_config: &AppConfig,
) -> Result<()> {
    let bundle = if let Some(path) = from_bundle {
        load_conversion_bundle(path)?
    } else {
        let selector = server.context("review requires <server> or --from-bundle")?;
        synthesize_bundle_for_server(selector, config_paths, backend, backend_auto, app_config)?
    };
    let options = run_options(backend, backend_auto, None, true, app_config)?;
    let result = review_conversion_bundle(&bundle, &options)?;
    let path = stage_output_path("review", bundle.evidence.server.name.as_str());
    write_json(&path, &result)?;
    print_json_or_human(json, &path, &result, || {
        println!("Review for {}", result.bundle.evidence.server.id);
        println!("Approved: {}", result.approved);
        if !result.findings.is_empty() {
            println!("Findings:");
            for finding in &result.findings {
                println!("- [{}] {}", finding.tool_name, finding.message);
            }
        }
    })?;
    if !result.approved {
        bail!("Reviewer rejected one or more generated skills.");
    }
    Ok(())
}

pub fn run_verify_cmd(
    server: Option<&str>,
    from_bundle: Option<&Path>,
    json: bool,
    config_paths: &[PathBuf],
    backend: Option<&str>,
    backend_auto: bool,
    app_config: &AppConfig,
) -> Result<()> {
    let reviewed_bundle = if let Some(path) = from_bundle {
        load_conversion_bundle(path)?
    } else {
        let selector = server.context("verify requires <server> or --from-bundle")?;
        let synthesized = synthesize_bundle_for_server(
            selector,
            config_paths,
            backend,
            backend_auto,
            app_config,
        )?;
        let review = review_conversion_bundle(
            &synthesized,
            &run_options(backend, backend_auto, None, true, app_config)?,
        )?;
        review.bundle
    };
    let result = verify_conversion_bundle(&reviewed_bundle);
    let path = stage_output_path("verify", reviewed_bundle.evidence.server.name.as_str());
    write_json(&path, &result)?;
    print_json_or_human(json, &path, &result, || print_verify(&result))?;
    if !result.passed {
        bail!("Verification failed for one or more generated skills.");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn run_run_cmd(
    server: &str,
    json: bool,
    config_paths: &[PathBuf],
    skills_dir: Option<PathBuf>,
    backend: Option<&str>,
    backend_auto: bool,
    dry_run: bool,
    app_config: &AppConfig,
) -> Result<()> {
    let options = run_options(backend, backend_auto, skills_dir, dry_run, app_config)?;
    let catalog = load_cached_catalog();
    let result = run_pipeline(server, config_paths, &options, catalog.as_ref())?;
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("Run status: {}", result.status);
        if let Some(skills_dir) = &result.skills_dir {
            println!("Skills dir: {}", skills_dir.display());
        }
        if let Some(backup) = &result.config_backup {
            println!("Config backup: {}", backup.display());
        }
        for item in &result.diagnostics {
            println!("- {}", item);
        }
        println!("Artifacts:");
        println!("  resolve: {}", result.artifacts.resolve.display());
        println!("  snapshot: {}", result.artifacts.snapshot.display());
        println!("  evidence: {}", result.artifacts.evidence.display());
        println!("  synthesis: {}", result.artifacts.synthesis.display());
        println!("  review: {}", result.artifacts.review.display());
        println!("  verify: {}", result.artifacts.verify.display());
    }
    if result.status == "blocked" {
        bail!(
            "{}",
            if result.diagnostics.is_empty() {
                "Pipeline blocked.".to_string()
            } else {
                result.diagnostics.join(" | ")
            }
        );
    }
    Ok(())
}

pub fn run_overview(json: bool) -> Result<()> {
    if json {
        #[derive(Serialize)]
        struct Overview<'a> {
            workflow: &'a [&'a str],
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&Overview {
                workflow: &[
                    "mcpsmith catalog sync",
                    "mcpsmith resolve <server>",
                    "mcpsmith snapshot <server>",
                    "mcpsmith evidence <server>",
                    "mcpsmith synthesize <server>",
                    "mcpsmith review <server>",
                    "mcpsmith verify <server>",
                    "mcpsmith run <server>",
                ],
            })?
        );
        return Ok(());
    }

    println!("mcpsmith source-grounded pipeline:");
    println!("  mcpsmith catalog sync");
    println!("  mcpsmith resolve <server>");
    println!("  mcpsmith snapshot <server>");
    println!("  mcpsmith evidence <server>");
    println!("  mcpsmith synthesize <server>");
    println!("  mcpsmith review <server>");
    println!("  mcpsmith verify <server>");
    println!("  mcpsmith run <server>");
    Ok(())
}

fn synthesize_bundle_for_server(
    server: &str,
    config_paths: &[PathBuf],
    backend: Option<&str>,
    backend_auto: bool,
    app_config: &AppConfig,
) -> Result<ServerConversionBundle> {
    let resolved = resolve_with_catalog(server, config_paths)?;
    let snapshot = materialize_snapshot(&resolved, None)?;
    let evidence = mcpsmith_core::build_evidence_bundle(&resolved, &snapshot.snapshot, None)?;
    let synthesis = synthesize_from_evidence(
        &evidence,
        &run_options(backend, backend_auto, None, true, app_config)?,
    )?;
    Ok(synthesis.bundle)
}

fn load_conversion_bundle(path: &Path) -> Result<ServerConversionBundle> {
    let value: Value = serde_json::from_str(
        &fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?,
    )
    .with_context(|| format!("Failed to parse {}", path.display()))?;
    let payload = value.get("result").cloned().unwrap_or(value);

    serde_json::from_value::<ServerConversionBundle>(payload.clone())
        .or_else(|_| {
            serde_json::from_value::<mcpsmith_core::SynthesisReport>(payload.clone())
                .map(|report| report.bundle)
        })
        .or_else(|_| {
            serde_json::from_value::<mcpsmith_core::ReviewReport>(payload)
                .map(|report| report.bundle)
        })
        .with_context(|| format!("Failed to decode {}", path.display()))
}

fn print_verify(report: &VerifyReport) {
    println!("Verify passed: {}", report.passed);
    if !report.issues.is_empty() {
        println!("Issues:");
        for issue in &report.issues {
            println!("- [{}] {}", issue.tool_name, issue.message);
        }
    }
}

fn sanitize_stage_slug(input: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in input.chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            prev_dash = false;
            Some(ch.to_ascii_lowercase())
        } else if !prev_dash {
            prev_dash = true;
            Some('-')
        } else {
            None
        };
        if let Some(ch) = normalized {
            out.push(ch);
        }
    }
    out.trim_matches('-').to_string()
}
