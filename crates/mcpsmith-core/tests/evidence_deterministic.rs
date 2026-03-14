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

    assert_eq!(pack.registration.as_ref().unwrap().file_path, PathBuf::from("src/server.ts"));
    assert_eq!(pack.handler.as_ref().unwrap().file_path, PathBuf::from("src/server.ts"));
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
    let evidence = build_evidence_bundle(&snapshot.artifact, &snapshot, Some("list_pages")).unwrap();
    let pack = &evidence.tool_evidence[0];

    assert_eq!(pack.registration.as_ref().unwrap().file_path, PathBuf::from("src/index.ts"));
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

    assert_eq!(pack.registration.as_ref().unwrap().file_path, PathBuf::from("server.py"));
    assert_eq!(pack.handler.as_ref().unwrap().file_path, PathBuf::from("server.py"));
    assert_eq!(pack.required_inputs, vec!["query".to_string()]);
    assert_eq!(pack.test_snippets.len(), 1);
    assert_eq!(pack.doc_snippets.len(), 1);
    assert!(pack.confidence >= 0.90);
}
