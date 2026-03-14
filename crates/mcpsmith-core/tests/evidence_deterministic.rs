use chrono::Utc;
use mcpsmith_core::{
    ArtifactIdentity, ArtifactKind, ConversionRecommendation, MCPServerProfile, PermissionLevel,
    ResolvedArtifact, SourceEvidenceLevel, SourceGrounding, SourceKind, SourceSnapshot,
    build_evidence_bundle,
};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

fn write_mock_mcp_script(path: &Path, tool_name: &str, required_inputs: &[&str]) {
    let tools_json = serde_json::to_string(&vec![json!({
        "name": tool_name,
        "description": format!("Tool {tool_name}"),
        "inputSchema": {
            "type": "object",
            "required": required_inputs,
            "properties": required_inputs.iter().map(|value| {
                ((*value).to_string(), json!({ "type": "string" }))
            }).collect::<serde_json::Map<String, serde_json::Value>>()
        }
    })])
    .unwrap();
    let body = r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2025-03-26","capabilities":{{}}}}}}\n'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":__TOOLS__}}'
      ;;
  esac
done
"#
    .replace("__TOOLS__", &tools_json);
    fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    }
}

fn sample_server(
    root: &Path,
    command: &Path,
    args: &[String],
    kind: SourceKind,
    package_name: Option<&str>,
    package_version: Option<&str>,
) -> MCPServerProfile {
    MCPServerProfile {
        id: "fixture:demo".to_string(),
        name: "demo".to_string(),
        source_label: "fixture".to_string(),
        source_path: root.join("mcp.json"),
        purpose: "Deterministic evidence test".to_string(),
        command: Some(command.display().to_string()),
        args: args.to_vec(),
        url: None,
        env_keys: vec![],
        declared_tool_count: 1,
        permission_hints: vec!["read-only".to_string()],
        inferred_permission: PermissionLevel::ReadOnly,
        recommendation: ConversionRecommendation::ReplaceCandidate,
        recommendation_reason: "read-only".to_string(),
        source_grounding: SourceGrounding {
            kind,
            evidence_level: SourceEvidenceLevel::SourceInspected,
            inspected: true,
            entrypoint: None,
            package_name: package_name.map(ToString::to_string),
            package_version: package_version.map(ToString::to_string),
            homepage: None,
            repository_url: Some("https://github.com/acme/demo".to_string()),
            inspected_paths: vec![],
            inspected_urls: vec![],
            derivation_evidence: vec![],
        },
    }
}

fn sample_snapshot(root: &Path, server: MCPServerProfile, kind: ArtifactKind) -> SourceSnapshot {
    let artifact = ResolvedArtifact {
        generated_at: Utc::now(),
        server,
        kind,
        identity: ArtifactIdentity {
            value: root.display().to_string(),
            version: Some("1.0.0".to_string()),
            source_url: Some("https://github.com/acme/demo".to_string()),
        },
        source_root_hint: Some(root.to_path_buf()),
        blocked: false,
        block_reason: None,
        diagnostics: vec![],
    };

    SourceSnapshot {
        generated_at: Utc::now(),
        artifact: artifact.clone(),
        cache_root: root.join(".cache"),
        source_root: root.to_path_buf(),
        reused_cache: false,
        manifest_paths: vec![],
        diagnostics: vec![],
    }
}

#[test]
fn evidence_bundle_locates_local_tool_deterministically() {
    let dir = tempfile::tempdir().unwrap();
    let source_root = dir.path().join("source");
    let runtime_root = dir.path().join("runtime");
    fs::create_dir_all(&source_root).unwrap();
    fs::create_dir_all(&runtime_root).unwrap();
    let mock_mcp = runtime_root.join("mock-mcp.sh");
    write_mock_mcp_script(&mock_mcp, "execute", &["query"]);

    fs::create_dir_all(source_root.join("src")).unwrap();
    fs::create_dir_all(source_root.join("tests")).unwrap();
    fs::write(
        source_root.join("README.md"),
        "# Demo MCP\n\nThe `execute` tool runs a local query.\n",
    )
    .unwrap();
    fs::write(
        source_root.join("src/server.ts"),
        r#"export function register(server) {
  server.tool("execute", { description: "Tool execute", inputSchema: { type: "object", required: ["query"] } }, async (args) => handleExecute(args));
}

export async function handleExecute(args) {
  return args.query;
}"#,
    )
    .unwrap();
    fs::write(
        source_root.join("tests/execute.spec.ts"),
        r#"it("handles execute", async () => {
  await callTool("execute", { query: "demo" });
});"#,
    )
    .unwrap();

    let server = sample_server(
        dir.path(),
        &mock_mcp,
        &[],
        SourceKind::LocalPath,
        None,
        None,
    );
    let snapshot = sample_snapshot(&source_root, server.clone(), ArtifactKind::LocalPath);
    let evidence = build_evidence_bundle(&snapshot.artifact, &snapshot, Some("execute")).unwrap();
    let pack = &evidence.tool_evidence[0];

    assert_eq!(
        pack.registration.as_ref().unwrap().file_path,
        PathBuf::from("src/server.ts")
    );
    assert_eq!(
        pack.handler.as_ref().unwrap().file_path,
        PathBuf::from("src/server.ts")
    );
    assert_eq!(pack.test_snippets.len(), 1);
    assert_eq!(pack.doc_snippets.len(), 1);
    assert!(pack.confidence >= 0.90);
}

#[test]
fn evidence_bundle_locates_npm_tool_deterministically() {
    let dir = tempfile::tempdir().unwrap();
    let source_root = dir.path().join("source");
    let runtime_root = dir.path().join("runtime");
    fs::create_dir_all(&source_root).unwrap();
    fs::create_dir_all(&runtime_root).unwrap();
    let mock_mcp = runtime_root.join("mock-mcp.sh");
    write_mock_mcp_script(&mock_mcp, "list_pages", &["browser_id"]);

    fs::create_dir_all(source_root.join("src")).unwrap();
    fs::create_dir_all(source_root.join("tests")).unwrap();
    fs::create_dir_all(source_root.join("docs")).unwrap();
    fs::write(
        source_root.join("package.json"),
        r#"{"name":"chrome-devtools-mcp","version":"1.0.0"}"#,
    )
    .unwrap();
    fs::write(
        source_root.join("src/index.ts"),
        r#"server.tool("list_pages", { description: "List pages", inputSchema: { type: "object", required: ["browser_id"] } }, async (args) => listPages(args));"#,
    )
    .unwrap();
    fs::write(
        source_root.join("src/list-pages.ts"),
        r#"export async function listPages(args) {
  return getOpenPages(args.browser_id);
}"#,
    )
    .unwrap();
    fs::write(
        source_root.join("tests/list-pages.spec.ts"),
        r#"it("lists pages", async () => {
  await callTool("list_pages", { browser_id: "demo" });
});"#,
    )
    .unwrap();
    fs::write(
        source_root.join("docs/list-pages.md"),
        "# List Pages\n\nUse the browser id to fetch open pages.\n",
    )
    .unwrap();

    let server = sample_server(
        dir.path(),
        &mock_mcp,
        &[],
        SourceKind::NpmPackage,
        Some("chrome-devtools-mcp"),
        Some("1.0.0"),
    );
    let snapshot = sample_snapshot(&source_root, server, ArtifactKind::NpmPackage);
    let evidence =
        build_evidence_bundle(&snapshot.artifact, &snapshot, Some("list_pages")).unwrap();
    let pack = &evidence.tool_evidence[0];

    assert_eq!(
        pack.registration.as_ref().unwrap().file_path,
        PathBuf::from("src/index.ts")
    );
    assert_eq!(
        pack.handler.as_ref().unwrap().file_path,
        PathBuf::from("src/list-pages.ts")
    );
    assert_eq!(pack.required_inputs, vec!["browser_id".to_string()]);
    assert_eq!(pack.test_snippets.len(), 1);
    assert_eq!(pack.doc_snippets.len(), 1);
    assert!(pack.confidence >= 0.90);
}

#[test]
fn evidence_bundle_locates_pypi_tool_deterministically() {
    let dir = tempfile::tempdir().unwrap();
    let source_root = dir.path().join("source");
    let runtime_root = dir.path().join("runtime");
    fs::create_dir_all(&source_root).unwrap();
    fs::create_dir_all(&runtime_root).unwrap();
    let mock_mcp = runtime_root.join("mock-mcp.sh");
    write_mock_mcp_script(&mock_mcp, "read_docs", &["query"]);

    fs::create_dir_all(source_root.join("tests")).unwrap();
    fs::write(
        source_root.join("pyproject.toml"),
        r#"[project]
name = "demo-mcp"
version = "0.1.0"
"#,
    )
    .unwrap();
    fs::write(
        source_root.join("README.md"),
        "# Demo MCP\n\nRead docs from the local package.\n",
    )
    .unwrap();
    fs::write(
        source_root.join("server.py"),
        r#"@mcp.tool()
def read_docs(query: str) -> str:
    """Read docs for a query."""
    return query
"#,
    )
    .unwrap();
    fs::write(
        source_root.join("tests/test_server.py"),
        r#"def test_read_docs():
    result = call_tool("read_docs", {"query": "demo"})
    assert result == "demo"
"#,
    )
    .unwrap();

    let server = sample_server(
        dir.path(),
        &mock_mcp,
        &[],
        SourceKind::PypiPackage,
        Some("demo-mcp"),
        Some("0.1.0"),
    );
    let snapshot = sample_snapshot(&source_root, server, ArtifactKind::PypiPackage);
    let evidence = build_evidence_bundle(&snapshot.artifact, &snapshot, Some("read_docs")).unwrap();
    let pack = &evidence.tool_evidence[0];

    assert_eq!(
        pack.registration.as_ref().unwrap().file_path,
        PathBuf::from("server.py")
    );
    assert_eq!(
        pack.handler.as_ref().unwrap().file_path,
        PathBuf::from("server.py")
    );
    assert_eq!(pack.required_inputs, vec!["query".to_string()]);
    assert_eq!(pack.test_snippets.len(), 1);
    assert_eq!(pack.doc_snippets.len(), 1);
    assert!(pack.confidence >= 0.90);
}

#[test]
fn evidence_bundle_locates_built_npm_tool_deterministically() {
    let dir = tempfile::tempdir().unwrap();
    let source_root = dir.path().join("source");
    let runtime_root = dir.path().join("runtime");
    fs::create_dir_all(&source_root).unwrap();
    fs::create_dir_all(&runtime_root).unwrap();
    let mock_mcp = runtime_root.join("mock-mcp.sh");
    write_mock_mcp_script(&mock_mcp, "click", &["uid"]);

    fs::create_dir_all(source_root.join("build/src/tools")).unwrap();
    fs::create_dir_all(source_root.join("docs")).unwrap();
    fs::write(
        source_root.join("package.json"),
        r#"{"name":"chrome-devtools-mcp","version":"1.0.0"}"#,
    )
    .unwrap();
    fs::write(
        source_root.join("build/src/index.js"),
        r#"function registerTool(tool) {
  server.registerTool(tool.name, {
    description: tool.description,
    inputSchema: tool.schema,
  }, async (params) => tool.handler(params));
}"#,
    )
    .unwrap();
    fs::write(
        source_root.join("build/src/tools/input.js"),
        r#"export const click = definePageTool({
  name: "click",
  description: "Clicks on the provided element",
  schema: {
    uid: zod.string(),
  },
  handler: async (request) => {
    return request.params.uid;
  },
});"#,
    )
    .unwrap();
    fs::write(
        source_root.join("docs/tool-reference.md"),
        "# Tool Reference\n\n- `click`\n",
    )
    .unwrap();

    let server = sample_server(
        dir.path(),
        &mock_mcp,
        &[],
        SourceKind::NpmPackage,
        Some("chrome-devtools-mcp"),
        Some("1.0.0"),
    );
    let snapshot = sample_snapshot(&source_root, server, ArtifactKind::NpmPackage);
    let evidence = build_evidence_bundle(&snapshot.artifact, &snapshot, Some("click")).unwrap();
    let pack = &evidence.tool_evidence[0];

    assert_eq!(
        pack.registration.as_ref().unwrap().file_path,
        PathBuf::from("build/src/tools/input.js")
    );
    assert_eq!(
        pack.handler.as_ref().unwrap().file_path,
        PathBuf::from("build/src/tools/input.js")
    );
    assert_eq!(pack.required_inputs, vec!["uid".to_string()]);
    assert_eq!(pack.doc_snippets.len(), 1);
    assert!(pack.confidence >= 0.80);
}

#[test]
fn evidence_bundle_locates_manifest_registered_npm_tool_deterministically() {
    let dir = tempfile::tempdir().unwrap();
    let source_root = dir.path().join("source");
    let runtime_root = dir.path().join("runtime");
    fs::create_dir_all(&source_root).unwrap();
    fs::create_dir_all(&runtime_root).unwrap();
    let mock_mcp = runtime_root.join("mock-mcp.sh");
    write_mock_mcp_script(&mock_mcp, "boot_sim", &[]);

    fs::create_dir_all(source_root.join("manifests/tools")).unwrap();
    fs::create_dir_all(source_root.join("manifests/workflows")).unwrap();
    fs::create_dir_all(source_root.join("build/mcp/tools/simulator")).unwrap();
    fs::write(
        source_root.join("package.json"),
        r#"{"name":"xcodebuildmcp","version":"1.0.0"}"#,
    )
    .unwrap();
    fs::write(
        source_root.join("manifests/tools/boot_sim.yaml"),
        r#"id: boot_sim
module: mcp/tools/simulator/boot_sim
names:
  mcp: boot_sim
  cli: boot
description: Boot iOS simulator."#,
    )
    .unwrap();
    fs::write(
        source_root.join("manifests/workflows/simulator.yaml"),
        r#"title: Simulator
tools:
  - boot_sim"#,
    )
    .unwrap();
    fs::write(
        source_root.join("build/mcp/tools/simulator/boot_sim.js"),
        r#"async function boot_simLogic(params, executor) {
  return executor(["xcrun", "simctl", "boot", params.simulatorId]);
}

const handler = createSessionAwareTool({
  logicFunction: boot_simLogic,
});

export {
  boot_simLogic,
  handler,
};"#,
    )
    .unwrap();

    let server = sample_server(
        dir.path(),
        &mock_mcp,
        &[],
        SourceKind::NpmPackage,
        Some("xcodebuildmcp"),
        Some("1.0.0"),
    );
    let snapshot = sample_snapshot(&source_root, server, ArtifactKind::NpmPackage);
    let evidence = build_evidence_bundle(&snapshot.artifact, &snapshot, Some("boot_sim")).unwrap();
    let pack = &evidence.tool_evidence[0];

    assert_eq!(
        pack.registration.as_ref().unwrap().file_path,
        PathBuf::from("manifests/tools/boot_sim.yaml")
    );
    assert_eq!(
        pack.handler.as_ref().unwrap().file_path,
        PathBuf::from("build/mcp/tools/simulator/boot_sim.js")
    );
    assert!(pack.confidence >= 0.75);
}

#[test]
fn evidence_bundle_does_not_treat_tool_named_test_sim_as_test_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let source_root = dir.path().join("source");
    let runtime_root = dir.path().join("runtime");
    fs::create_dir_all(&source_root).unwrap();
    fs::create_dir_all(&runtime_root).unwrap();
    let mock_mcp = runtime_root.join("mock-mcp.sh");
    write_mock_mcp_script(&mock_mcp, "test_sim", &[]);

    fs::create_dir_all(source_root.join("manifests/tools")).unwrap();
    fs::create_dir_all(source_root.join("manifests/workflows")).unwrap();
    fs::create_dir_all(source_root.join("build/mcp/tools/simulator")).unwrap();
    fs::write(
        source_root.join("package.json"),
        r#"{"name":"xcodebuildmcp","version":"1.0.0"}"#,
    )
    .unwrap();
    fs::write(
        source_root.join("manifests/tools/test_sim.yaml"),
        r#"id: test_sim
module: mcp/tools/simulator/test_sim
names:
  mcp: test_sim
  cli: test
description: Test on iOS sim."#,
    )
    .unwrap();
    fs::write(
        source_root.join("manifests/workflows/simulator.yaml"),
        r#"title: Simulator
tools:
  - test_sim"#,
    )
    .unwrap();
    fs::write(
        source_root.join("build/mcp/tools/simulator/test_sim.js"),
        r#"async function test_simLogic(params, executor) {
  return executor(["xcodebuild", "test"]);
}

const handler = createSessionAwareTool({
  logicFunction: test_simLogic,
});

export {
  test_simLogic,
  handler,
};"#,
    )
    .unwrap();

    let server = sample_server(
        dir.path(),
        &mock_mcp,
        &[],
        SourceKind::NpmPackage,
        Some("xcodebuildmcp"),
        Some("1.0.0"),
    );
    let snapshot = sample_snapshot(&source_root, server, ArtifactKind::NpmPackage);
    let evidence = build_evidence_bundle(&snapshot.artifact, &snapshot, Some("test_sim")).unwrap();
    let pack = &evidence.tool_evidence[0];

    assert_eq!(
        pack.registration.as_ref().unwrap().file_path,
        PathBuf::from("manifests/tools/test_sim.yaml")
    );
    assert_eq!(
        pack.handler.as_ref().unwrap().file_path,
        PathBuf::from("build/mcp/tools/simulator/test_sim.js")
    );
    assert!(pack.test_snippets.is_empty());
    assert!(pack.confidence >= 0.75);
}
