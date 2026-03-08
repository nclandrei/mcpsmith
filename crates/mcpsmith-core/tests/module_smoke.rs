use chrono::Utc;
use mcpsmith_core::{
    ConversionRecommendation, DossierBundle, MCPServerProfile, PermissionLevel, PlanMode,
    ProbeInputSource, ProbeInputs, RuntimeTool, ServerDossier, ServerGate, SourceGrounding,
    SourceKind, ToolContractTest, ToolDossier, build_from_bundle, build_from_dossier_path,
    discover, inspect, load_dossier_bundle, plan, write_dossier_bundle,
};
use serde_json::json;
use std::fs;
use std::path::Path;

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
        source_grounding: SourceGrounding::default(),
    }
}

fn sample_bundle(root: &Path) -> DossierBundle {
    let server = sample_server(root);
    let runtime_tool = RuntimeTool {
        name: "read_graph".to_string(),
        description: Some("Read graph state".to_string()),
        input_schema: Some(json!({
            "type": "object",
            "properties": {}
        })),
    };
    let tool_dossier = ToolDossier {
        name: "read_graph".to_string(),
        explanation: "Read the memory graph.".to_string(),
        recipe: vec![
            "Validate graph selection inputs.".to_string(),
            "Run the tool and capture the returned graph.".to_string(),
            "Summarize entities and relations.".to_string(),
        ],
        evidence: vec!["runtime metadata".to_string()],
        confidence: 0.9,
        contract_tests: vec![
            ToolContractTest {
                probe: "happy-path".to_string(),
                expected: "Returns graph content.".to_string(),
                method: "Run with valid input.".to_string(),
                applicable: true,
            },
            ToolContractTest {
                probe: "invalid-input".to_string(),
                expected: "Returns a validation error.".to_string(),
                method: "Run with malformed input.".to_string(),
                applicable: true,
            },
            ToolContractTest {
                probe: "side-effect-safety".to_string(),
                expected: "No mutation occurs.".to_string(),
                method: "Run with dry-run semantics.".to_string(),
                applicable: false,
            },
        ],
        probe_inputs: ProbeInputs::default(),
        probe_input_source: ProbeInputSource::Synthesized,
    };

    DossierBundle {
        format_version: 5,
        generated_at: Utc::now(),
        dossiers: vec![ServerDossier {
            generated_at: Utc::now(),
            format_version: 5,
            server,
            runtime_tools: vec![runtime_tool],
            tool_dossiers: vec![tool_dossier],
            server_gate: ServerGate::Ready,
            gate_reasons: vec![],
            backend_used: "claude".to_string(),
            backend_fallback_used: false,
            backend_diagnostics: vec![],
        }],
    }
}

#[test]
fn dossier_roundtrip_builds_skills_after_module_split() {
    let dir = tempfile::tempdir().unwrap();
    let dossier_path = dir.path().join("dossier.json");
    let skills_dir = dir.path().join("skills");
    let skills_dir_from_path = dir.path().join("skills-from-path");

    let bundle = sample_bundle(dir.path());
    write_dossier_bundle(&dossier_path, &bundle).unwrap();

    let loaded = load_dossier_bundle(&dossier_path).unwrap();
    assert_eq!(loaded.dossiers.len(), 1);
    assert_eq!(loaded.dossiers[0].server.name, "memory");

    let build = build_from_bundle(&loaded, Some(skills_dir)).unwrap();
    assert_eq!(build.servers.len(), 1);
    assert!(build.servers[0].orchestrator_skill_path.exists());
    assert_eq!(build.servers[0].tool_skill_paths.len(), 1);
    assert!(build.servers[0].tool_skill_paths[0].exists());
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
    assert!(!dir.path().join("skills").join("memory.md").exists());

    let build_from_path =
        build_from_dossier_path(&dossier_path, Some(skills_dir_from_path)).unwrap();
    assert_eq!(build_from_path.servers.len(), 1);
    assert!(build_from_path.servers[0].orchestrator_skill_path.exists());
}

#[test]
fn inventory_and_plan_still_resolve_servers_after_module_split() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("mcp.json");
    fs::write(
        &config_path,
        r#"{
  "mcpServers": {
    "memory": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-memory"],
      "readOnly": true
    }
  }
}"#,
    )
    .unwrap();

    let inventory = discover(std::slice::from_ref(&config_path)).unwrap();
    assert!(
        inventory
            .servers
            .iter()
            .any(|server| server.id == "custom-1:memory")
    );

    let server = inspect("custom-1:memory", std::slice::from_ref(&config_path)).unwrap();
    assert_eq!(
        server.recommendation,
        ConversionRecommendation::ReplaceCandidate
    );

    let conversion_plan = plan("custom-1:memory", PlanMode::Auto, &[config_path]).unwrap();
    assert!(!conversion_plan.blocked);
    assert_eq!(conversion_plan.recommended_mode, PlanMode::Replace);
    assert_eq!(conversion_plan.effective_mode, PlanMode::Replace);
}

#[test]
fn discover_reports_source_grounding_metadata() {
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

    let inventory = discover(&[config_path]).unwrap();
    let server = inventory
        .servers
        .iter()
        .find(|server| server.name == "playwright")
        .unwrap();
    assert_eq!(server.source_grounding.kind, SourceKind::LocalPath);
    assert_eq!(
        server.source_grounding.package_name.as_deref(),
        Some("@acme/local-mcp")
    );
    assert_eq!(
        server.source_grounding.repository_url.as_deref(),
        Some("https://github.com/acme/local-mcp")
    );
}
