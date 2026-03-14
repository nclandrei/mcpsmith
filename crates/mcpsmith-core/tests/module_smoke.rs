use chrono::Utc;
use mcpsmith_core::{
    ArtifactIdentity, ArtifactKind, ConversionRecommendation, EvidenceBundle, HelperScript,
    MCPServerProfile, NativeWorkflowStep, PermissionLevel, ResolvedArtifact, ReviewReport,
    RuntimeTool, ServerConversionBundle, SnippetEvidence, SourceGrounding, SourceKind,
    SourceSnapshot, ToolConversionDraft, ToolEvidencePack, ToolSemanticSummary,
    WorkflowContextInput, WorkflowSkillSpec, build_from_bundle, resolve_artifact,
};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

fn sample_server(root: &Path) -> MCPServerProfile {
    MCPServerProfile {
        id: "fixture:memory".to_string(),
        name: "memory".to_string(),
        source_label: "fixture".to_string(),
        source_path: root.join("mcp.json"),
        purpose: "Memory and knowledge graph workflows".to_string(),
        command: Some("npx".to_string()),
        args: vec![
            "-y".to_string(),
            "@modelcontextprotocol/server-memory".to_string(),
        ],
        url: None,
        env_keys: vec![],
        declared_tool_count: 1,
        permission_hints: vec!["read-only".to_string()],
        inferred_permission: PermissionLevel::ReadOnly,
        recommendation: ConversionRecommendation::ReplaceCandidate,
        recommendation_reason: "read-only".to_string(),
        source_grounding: SourceGrounding {
            kind: SourceKind::LocalPath,
            evidence_level: mcpsmith_core::SourceEvidenceLevel::SourceInspected,
            inspected: true,
            entrypoint: Some(root.join("source").join("bin").join("server.sh")),
            package_name: Some("@acme/memory".to_string()),
            package_version: Some("1.2.3".to_string()),
            homepage: Some("https://example.com/memory".to_string()),
            repository_url: Some("https://github.com/acme/memory".to_string()),
            inspected_paths: vec![root.join("source").join("bin").join("server.sh")],
            inspected_urls: vec![],
            derivation_evidence: vec![],
        },
    }
}

fn sample_review(root: &Path) -> ReviewReport {
    let source_root = root.join("source");
    fs::create_dir_all(source_root.join("bin")).unwrap();
    fs::create_dir_all(source_root.join("docs")).unwrap();
    fs::write(
        source_root.join("bin").join("server.sh"),
        "#!/bin/sh\nexit 0\n",
    )
    .unwrap();
    fs::write(
        source_root.join("docs").join("read-graph.md"),
        "# Read Graph\n\nSample citation.\n",
    )
    .unwrap();

    let server = sample_server(root);
    let runtime_tool = RuntimeTool {
        name: "read_graph".to_string(),
        description: Some("Read graph state".to_string()),
        input_schema: Some(json!({
            "type": "object",
            "properties": {}
        })),
    };
    let resolved = ResolvedArtifact {
        generated_at: Utc::now(),
        server: server.clone(),
        kind: ArtifactKind::LocalPath,
        identity: ArtifactIdentity {
            value: source_root
                .join("bin")
                .join("server.sh")
                .display()
                .to_string(),
            version: Some("1.2.3".to_string()),
            source_url: Some("https://github.com/acme/memory".to_string()),
        },
        source_root_hint: Some(source_root.clone()),
        blocked: false,
        block_reason: None,
        diagnostics: vec![],
    };
    let snapshot = SourceSnapshot {
        generated_at: Utc::now(),
        artifact: resolved.clone(),
        cache_root: root.join("cache"),
        source_root: source_root.clone(),
        reused_cache: false,
        manifest_paths: vec![source_root.join("package.json")],
        diagnostics: vec![],
    };
    let evidence = EvidenceBundle {
        generated_at: Utc::now(),
        server: server.clone(),
        artifact: resolved,
        snapshot,
        runtime_tools: vec![runtime_tool.clone()],
        tool_evidence: vec![ToolEvidencePack {
            tool_name: "read_graph".to_string(),
            runtime_tool,
            registration: Some(SnippetEvidence {
                file_path: PathBuf::from("src/server.ts"),
                start_line: 10,
                end_line: 20,
                excerpt: "registerTool('read_graph', ...)".to_string(),
                score: 0.95,
            }),
            handler: Some(SnippetEvidence {
                file_path: PathBuf::from("src/handlers/read_graph.ts"),
                start_line: 30,
                end_line: 60,
                excerpt: "export async function readGraph() {}".to_string(),
                score: 0.93,
            }),
            supporting_snippets: vec![],
            test_snippets: vec![],
            doc_snippets: vec![],
            required_inputs: vec!["graph_scope".to_string()],
            mapper_fallback: None,
            diagnostics: vec![],
            confidence: 0.91,
        }],
        diagnostics: vec![],
    };
    let draft = ToolConversionDraft {
        tool_name: "read_graph".to_string(),
        semantic_summary: ToolSemanticSummary {
            what_it_does: "Reads the graph backing store and summarizes the current state."
                .to_string(),
            required_inputs: vec!["graph_scope".to_string()],
            prerequisites: vec![],
            side_effect_level: "read-only".to_string(),
            success_signals: vec!["Graph content returned".to_string()],
            failure_modes: vec!["Graph export missing".to_string()],
            citations: vec![PathBuf::from("docs/read-graph.md")],
            confidence: 0.91,
        },
        workflow_skill: WorkflowSkillSpec {
            id: "read_graph".to_string(),
            title: "Read graph".to_string(),
            goal: "Inspect the memory graph without relying on the MCP server.".to_string(),
            when_to_use: "Use this when you need to inspect graph state and summarize it."
                .to_string(),
            trigger_phrases: vec!["read the graph".to_string()],
            origin_tools: vec!["read_graph".to_string()],
            prerequisite_workflows: vec![],
            followup_workflows: vec![],
            required_context: vec![WorkflowContextInput {
                name: "graph_scope".to_string(),
                guidance: "Know which graph or entity scope you want to inspect.".to_string(),
                required: true,
            }],
            context_acquisition: vec![
                "If the graph scope is unclear, ask which entities or namespaces to inspect."
                    .to_string(),
            ],
            branching_rules: vec![
                "If the local graph export is missing, stop and ask where to read it from."
                    .to_string(),
            ],
            stop_and_ask: vec![
                "Stop if the graph location or query is ambiguous instead of assuming defaults."
                    .to_string(),
            ],
            native_steps: vec![NativeWorkflowStep {
                title: "Read the graph export".to_string(),
                command: "./scripts/read-graph.sh \"$GRAPH_EXPORT_PATH\"".to_string(),
                details: Some(
                    "Replace $GRAPH_EXPORT_PATH with the concrete graph export path you collected."
                        .to_string(),
                ),
            }],
            verification: vec!["Confirm the file exists and the output contains graph data."
                .to_string()],
            return_contract: vec![
                "Return the graph path you read together with a concise summary of entities and relations."
                    .to_string(),
            ],
            guardrails: vec![
                "Do not invent a graph path or silently fall back to another file.".to_string(),
            ],
            evidence: vec!["runtime metadata".to_string()],
            confidence: 0.9,
        },
        helper_scripts: vec![HelperScript {
            relative_path: PathBuf::from("./scripts/read-graph.sh"),
            body: "#!/bin/sh\ncat \"$1\"\n".to_string(),
            executable: true,
        }],
    };
    let bundle = ServerConversionBundle {
        generated_at: Utc::now(),
        evidence,
        backend_used: "codex".to_string(),
        backend_fallback_used: false,
        tool_conversions: vec![draft],
        blocked: false,
        block_reasons: vec![],
        diagnostics: vec![],
    };

    ReviewReport {
        generated_at: Utc::now(),
        approved: true,
        bundle,
        findings: vec![],
    }
}

#[test]
fn reviewed_bundle_builds_skills_and_helper_scripts() {
    let dir = tempfile::tempdir().unwrap();
    let skills_dir = dir.path().join("skills");

    let review = sample_review(dir.path());
    let build = build_from_bundle(&review.bundle, Some(skills_dir)).unwrap();
    assert_eq!(build.servers.len(), 1);
    assert!(build.servers[0].orchestrator_skill_path.exists());
    assert_eq!(build.servers[0].tool_skill_paths.len(), 1);
    assert!(build.servers[0].tool_skill_paths[0].exists());
    let orchestrator_body =
        std::fs::read_to_string(&build.servers[0].orchestrator_skill_path).unwrap();
    let workflow_body = std::fs::read_to_string(&build.servers[0].tool_skill_paths[0]).unwrap();
    assert!(!orchestrator_body.contains("mcp__"));
    assert!(!workflow_body.contains("mcp__"));
    assert!(!workflow_body.contains("maps to"));
    assert!(workflow_body.contains("./scripts/read-graph.sh"));
    assert_eq!(
        build.servers[0].orchestrator_skill_path,
        dir.path().join("skills").join("memory").join("SKILL.md")
    );
    assert_eq!(
        build.servers[0].tool_skill_paths[0],
        dir.path()
            .join("skills")
            .join("memory--read-graph")
            .join("SKILL.md")
    );
    assert!(
        dir.path()
            .join("skills")
            .join("memory")
            .join(".mcpsmith")
            .join("manifest.json")
            .exists()
    );
    let helper_script = dir
        .path()
        .join("skills")
        .join("memory--read-graph")
        .join("scripts")
        .join("read-graph.sh");
    assert!(helper_script.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = fs::metadata(&helper_script).unwrap().permissions().mode();
        assert_ne!(mode & 0o111, 0, "helper script should be executable");
    }
    assert!(!dir.path().join("skills").join("memory.md").exists());
}

#[test]
fn build_rejects_blocked_reviewed_bundle() {
    let dir = tempfile::tempdir().unwrap();
    let skills_dir = dir.path().join("skills");
    let mut review = sample_review(dir.path());
    review.bundle.blocked = true;
    review.bundle.block_reasons =
        vec!["Reviewer rejected the bundle without a usable revision.".to_string()];

    let err = build_from_bundle(&review.bundle, Some(skills_dir)).unwrap_err();
    assert!(
        err.to_string()
            .contains("Cannot build standalone skills from blocked review bundle")
    );
}

#[test]
fn resolve_artifact_reports_source_grounding_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let tool_root = dir.path().join("local-tool");
    let bin_dir = tool_root.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let executable = bin_dir.join("server.sh");
    std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::write(
        tool_root.join("package.json"),
        r#"{
  "name": "@acme/local-mcp",
  "version": "1.2.3",
  "homepage": "https://example.com/local-mcp",
  "repository": {
    "type": "git",
    "url": "https://github.com/acme/local-mcp"
  }
}"#,
    )
    .unwrap();

    let config_path = dir.path().join("mcp.json");
    fs::write(
        &config_path,
        format!(
            r#"{{
  "mcpServers": {{
    "playwright": {{
      "command": "{}",
      "readOnly": true
    }}
  }}
}}"#,
            executable.display()
        ),
    )
    .unwrap();

    let resolved = resolve_artifact("playwright", &[config_path], None).unwrap();
    assert_eq!(resolved.kind, ArtifactKind::LocalPath);
    let expected_executable = fs::canonicalize(&executable).unwrap();
    let actual_executable = fs::canonicalize(&resolved.identity.value).unwrap();
    assert_eq!(actual_executable, expected_executable);
    assert_eq!(resolved.server.source_grounding.kind, SourceKind::LocalPath);
    assert_eq!(
        resolved.server.source_grounding.package_name.as_deref(),
        Some("@acme/local-mcp")
    );
    assert_eq!(
        resolved.server.source_grounding.repository_url.as_deref(),
        Some("https://github.com/acme/local-mcp")
    );
    let expected_root = fs::canonicalize(&tool_root).unwrap();
    let actual_root = fs::canonicalize(resolved.source_root_hint.as_ref().unwrap()).unwrap();
    assert_eq!(actual_root, expected_root,);
}
