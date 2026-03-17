mod support;

use predicates::prelude::*;
use serde_json::{Map, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::Duration;
use support::{
    TestContext, count_backups, write_local_source_layout, write_mock_claude_script,
    write_mock_codex_script, write_mock_codex_script_with_delay,
    write_mock_codex_script_with_review_fix, write_mock_mcp_script,
};

fn write_playwright_config(ctx: &TestContext, command: &Path, read_only: Option<bool>) {
    ctx.write_server_config(
        "playwright",
        command,
        Some("Read-only browser helpers"),
        read_only,
    );
}

fn write_playwright_config_to_path(
    ctx: &TestContext,
    path: &Path,
    command: &Path,
    read_only: Option<bool>,
) {
    ctx.write_server_config_to_path(
        path,
        "playwright",
        command,
        Some("Read-only browser helpers"),
        read_only,
    );
}

fn write_low_confidence_source_layout(ctx: &TestContext, tool_name: &str) {
    fs::create_dir_all(ctx.path("source/src")).unwrap();
    fs::write(
        ctx.path("source/package.json"),
        format!(
            r#"{{
  "name": "@acme/{tool_name}-mcp",
  "version": "1.2.3"
}}
"#
        ),
    )
    .unwrap();
    fs::write(
        ctx.path("source/README.md"),
        format!("# Demo MCP\n\nThe `{tool_name}` tool runs a local query.\n"),
    )
    .unwrap();
    fs::write(
        ctx.path("source/src/tool_index.ts"),
        format!(
            r#"export const TOOL_REGISTRY = {{
  {tool_name}: {{
    summary: "Run {tool_name}",
    schema: {{
      query: "string",
    }},
    run: run{title_case},
  }},
}};"#,
            title_case = "Execute"
        ),
    )
    .unwrap();
    fs::write(
        ctx.path(format!("source/src/{tool_name}.ts")),
        format!(
            r#"export async function run{title_case}(args) {{
  return args.query;
}}"#,
            title_case = "Execute"
        ),
    )
    .unwrap();
    for idx in 0..8 {
        fs::write(
            ctx.path(format!("source/src/noise-{idx:02}.ts")),
            format!(r#"export const note{idx} = "{tool_name} background reference {idx}";"#),
        )
        .unwrap();
    }
}

fn parse_json_output(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}

fn artifact_path(value: &Value) -> PathBuf {
    PathBuf::from(value["artifact_path"].as_str().unwrap())
}

struct StubRegistryServer {
    base_url: String,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl StubRegistryServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        handle_registry_request(&mut stream);
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            base_url: format!("http://{}", addr),
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for StubRegistryServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.base_url.trim_start_matches("http://"));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct StrictOfficialLimitRegistryServer {
    base_url: String,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl StrictOfficialLimitRegistryServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        handle_strict_official_registry_request(&mut stream);
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            base_url: format!("http://{}", addr),
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for StrictOfficialLimitRegistryServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.base_url.trim_start_matches("http://"));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn handle_registry_request(stream: &mut TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));

    let mut request_bytes = Vec::new();
    let mut buffer = [0u8; 1024];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                request_bytes.extend_from_slice(&buffer[..read]);
                if request_bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if !request_bytes.is_empty() {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    let request = String::from_utf8_lossy(&request_bytes);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    let body = if path.starts_with("/official/servers") {
        r#"{
  "servers": [
    {
      "server": {
        "name": "memory",
        "title": "Memory",
        "description": "Shared memory server",
        "repository": { "url": "https://github.com/modelcontextprotocol/servers" },
        "packages": [
          {
            "registryType": "npm",
            "identifier": "@modelcontextprotocol/server-memory",
            "version": "1.0.0"
          }
        ]
      }
    }
  ],
  "metadata": { "nextCursor": null }
}"#
    } else if path.starts_with("/smithery/servers") {
        r#"{
  "servers": [
    {
      "qualifiedName": "example/remote-only",
      "displayName": "Remote Only",
      "description": "Hosted remote server",
      "namespace": "example",
      "slug": "remote-only",
      "remote": true
    }
  ],
  "pagination": { "totalPages": 1 }
}"#
    } else if path.starts_with("/glama/servers") {
        r#"{
  "pageInfo": {
    "endCursor": null,
    "hasNextPage": false,
    "hasPreviousPage": false,
    "startCursor": null
  },
  "servers": [
    {
      "id": "glama-test-1",
      "name": "glama-demo",
      "namespace": "acme",
      "slug": "glama-demo",
      "description": "Glama test server with repo",
      "repository": { "url": "https://github.com/acme/glama-demo" },
      "attributes": ["hosting:local-only"],
      "url": "https://glama.ai/mcp/servers/glama-test-1",
      "tools": [],
      "spdxLicense": null
    },
    {
      "id": "glama-test-2",
      "name": "glama-remote",
      "namespace": "acme",
      "slug": "glama-remote",
      "description": "Glama remote-only test server",
      "attributes": ["hosting:remote-capable"],
      "url": "https://glama.ai/mcp/servers/glama-test-2",
      "tools": [],
      "spdxLicense": null
    }
  ]
}"#
    } else {
        r#"{"error":"not-found"}"#
    };

    let status = if body.contains("not-found") {
        "404 Not Found"
    } else {
        "200 OK"
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}

fn handle_strict_official_registry_request(stream: &mut TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));

    let mut request_bytes = Vec::new();
    let mut buffer = [0u8; 1024];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                request_bytes.extend_from_slice(&buffer[..read]);
                if request_bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if !request_bytes.is_empty() {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    let request = String::from_utf8_lossy(&request_bytes);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    let (status, body) = if path.starts_with("/official/servers?limit=200") {
        (
            "422 Unprocessable Entity",
            r#"{"title":"Unprocessable Entity","status":422,"detail":"validation failed","errors":[{"message":"expected number <= 100","location":"query.limit","value":200}]}"#,
        )
    } else if path.starts_with("/official/servers?limit=100") {
        (
            "200 OK",
            r#"{
  "servers": [
    {
      "server": {
        "name": "memory",
        "title": "Memory",
        "description": "Shared memory server",
        "repository": { "url": "https://github.com/modelcontextprotocol/servers" },
        "packages": [
          {
            "registryType": "npm",
            "identifier": "@modelcontextprotocol/server-memory",
            "version": "1.0.0"
          }
        ]
      }
    }
  ],
  "metadata": { "nextCursor": null }
}"#,
        )
    } else {
        ("404 Not Found", r#"{"error":"not-found"}"#)
    };

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}

#[test]
fn test_mcpsmith_root_help_lists_agentic_pipeline() {
    let ctx = TestContext::new();

    ctx.cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("One-shot conversion:"))
        .stdout(predicate::str::contains("mcpsmith run <server>"))
        .stdout(predicate::str::contains("mcpsmith discover"))
        .stdout(predicate::str::contains("mcpsmith resolve <server>"))
        .stdout(predicate::str::contains("mcpsmith verify <server>"))
        .stdout(predicate::str::contains(
            "Artifacts are written under .codex-runtime/stages/.",
        ))
        .stdout(predicate::str::contains(
            "Catalog sync defaults to official + smithery + glama.",
        ))
        .stdout(predicate::str::contains(
            "Every command is non-interactive.",
        ))
        .stdout(predicate::str::contains("\n  apply").not())
        .stdout(predicate::str::contains("contract-test").not());
}

#[test]
fn test_mcpsmith_discover_help_explains_local_inventory() {
    let ctx = TestContext::new();

    ctx.cmd()
        .args(["discover", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Discover installed MCP servers from local config files.",
        ))
        .stdout(predicate::str::contains(
            "Searches the standard local MCP config locations plus any --config paths you provide.",
        ))
        .stdout(predicate::str::contains(
            "Use this before resolve or run when you want to see exactly which MCP entries mcpsmith can inspect.",
        ))
        .stdout(predicate::str::contains("--config <PATH>"));
}

#[test]
fn test_mcpsmith_discover_lists_local_servers() {
    let ctx = TestContext::new();
    let config_path = ctx.config_path();
    let mock_mcp = ctx.path("mock-mcp.sh");
    write_mock_mcp_script(&mock_mcp, &["execute"]);

    let mut servers = Map::new();
    let mut local = Map::new();
    local.insert(
        "command".to_string(),
        Value::String(mock_mcp.to_string_lossy().into_owned()),
    );
    local.insert(
        "description".to_string(),
        Value::String("Read-only browser helpers".to_string()),
    );
    local.insert("readOnly".to_string(), Value::Bool(true));
    servers.insert("playwright".to_string(), Value::Object(local));

    let mut remote = Map::new();
    remote.insert(
        "url".to_string(),
        Value::String("https://example.com/mcp".to_string()),
    );
    servers.insert("remote-demo".to_string(), Value::Object(remote));

    ctx.write_mcp_servers(servers);

    ctx.cmd()
        .args(["discover", "--config"])
        .arg(&config_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("Discovered 2 MCP servers."))
        .stdout(predicate::str::contains("- playwright"))
        .stdout(predicate::str::contains("- remote-demo"))
        .stdout(predicate::str::contains("Read-only browser helpers"))
        .stdout(predicate::str::contains(
            config_path.to_string_lossy().to_string(),
        ))
        .stdout(predicate::str::contains("https://example.com/mcp"));
}

#[test]
fn test_mcpsmith_discover_merges_duplicate_server_configs() {
    let ctx = TestContext::new();
    let config_one = ctx.path("codex.json");
    let config_two = ctx.path("claude.json");
    let mock_mcp = ctx.path("mock-mcp.sh");
    write_mock_mcp_script(&mock_mcp, &["execute"]);

    write_playwright_config_to_path(&ctx, &config_one, &mock_mcp, Some(true));
    write_playwright_config_to_path(&ctx, &config_two, &mock_mcp, Some(true));

    ctx.cmd()
        .args(["discover", "--config"])
        .arg(&config_one)
        .args(["--config"])
        .arg(&config_two)
        .assert()
        .success()
        .stdout(predicate::str::contains("Discovered 1 MCP server."))
        .stdout(predicate::str::contains("- playwright"))
        .stdout(predicate::str::contains("configs:"))
        .stdout(predicate::str::contains(
            config_one.to_string_lossy().to_string(),
        ))
        .stdout(predicate::str::contains(
            config_two.to_string_lossy().to_string(),
        ));
}

#[test]
fn test_mcpsmith_resolve_accepts_legacy_source_selector_for_merged_server() {
    let ctx = TestContext::new();
    let config_one = ctx.path("codex.json");
    let config_two = ctx.path("claude.json");
    let mock_mcp = ctx.path("mock-mcp.sh");
    write_mock_mcp_script(&mock_mcp, &["execute"]);

    write_playwright_config_to_path(&ctx, &config_one, &mock_mcp, Some(true));
    write_playwright_config_to_path(&ctx, &config_two, &mock_mcp, Some(true));

    ctx.cmd()
        .args(["resolve", "custom-1:playwright", "--config"])
        .arg(&config_one)
        .args(["--config"])
        .arg(&config_two)
        .assert()
        .success()
        .stdout(predicate::str::contains("Resolved playwright"))
        .stdout(predicate::str::contains("Artifact kind: LocalPath"));
}

#[test]
fn test_mcpsmith_resolve_help_explains_artifact_flow() {
    let ctx = TestContext::new();

    ctx.cmd()
        .args(["resolve", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Resolve the exact source artifact for one installed MCP.",
        ))
        .stdout(predicate::str::contains(
            "Writes a resolve artifact that snapshot can consume with --from-resolve.",
        ))
        .stdout(predicate::str::contains(
            "Blocks remote-only or source-unavailable servers instead of converting metadata alone.",
        ))
        .stdout(predicate::str::contains(
            "Repeat to inspect multiple MCP config files",
        ));
}

#[test]
fn test_mcpsmith_run_help_explains_install_and_dry_run() {
    let ctx = TestContext::new();

    ctx.cmd()
        .args(["run", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Run resolve, snapshot, evidence, synthesize, review, and verify in one command.",
        ))
        .stdout(predicate::str::contains(
            "Installs reviewed skills and removes the MCP config entry unless --dry-run is set.",
        ))
        .stdout(predicate::str::contains(
            "Use --skills-dir to write into an isolated preview directory.",
        ))
        .stdout(predicate::str::contains("Examples:"));
}

#[test]
fn test_mcpsmith_run_human_output_streams_stage_progress_updates() {
    let ctx = TestContext::new();
    let config_path = ctx.config_path();
    let mock_mcp = ctx.path("mock-mcp.sh");
    let mock_codex = ctx.path("mock-codex-slow.py");

    write_local_source_layout(&ctx, "execute");
    write_mock_mcp_script(&mock_mcp, &["execute"]);
    write_mock_codex_script_with_delay(&mock_codex, 250);
    write_playwright_config(&ctx, &mock_mcp, Some(true));

    ctx.cmd()
        .env("MCPSMITH_CODEX_COMMAND", &mock_codex)
        .env("MCPSMITH_PROGRESS_INTERVAL_MS", "50")
        .args([
            "run",
            "playwright",
            "--dry-run",
            "--backend",
            "codex",
            "--config",
        ])
        .arg(&config_path)
        .assert()
        .success()
        .stderr(predicate::str::contains("Progress: resolve"))
        .stderr(predicate::str::contains("Progress: synthesize"))
        .stderr(predicate::str::contains("still running"))
        .stdout(predicate::str::contains("Run status: dry-run"));
}

#[test]
fn test_mcpsmith_catalog_sync_help_lists_default_providers() {
    let ctx = TestContext::new();

    ctx.cmd()
        .args(["catalog", "sync", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Defaults to the official registry, Smithery, and Glama.",
        ))
        .stdout(predicate::str::contains(
            "Repeat --provider to override the default provider set.",
        ))
        .stdout(predicate::str::contains("--provider <NAME>"));
}

#[test]
fn test_mcpsmith_rejects_legacy_subcommands() {
    let ctx = TestContext::new();

    for legacy in ["build", "contract-test", "apply"] {
        ctx.cmd()
            .args(["help", legacy])
            .assert()
            .failure()
            .stderr(predicate::str::contains(format!(
                "unrecognized subcommand '{legacy}'"
            )));
    }
}

#[test]
fn test_mcpsmith_resolve_blocks_remote_only_servers() {
    let ctx = TestContext::new();
    let config_path = ctx.config_path();
    ctx.write_remote_server_config("remote-demo", "https://example.com/mcp");

    ctx.cmd()
        .args(["resolve", "remote-demo", "--json", "--config"])
        .arg(&config_path)
        .assert()
        .failure()
        .stdout(predicate::str::contains("\"blocked\": true"))
        .stderr(predicate::str::contains("URL-backed"));
}

#[test]
fn test_mcpsmith_staged_pipeline_accepts_prior_artifacts() {
    let ctx = TestContext::new();
    let config_path = ctx.config_path();
    let mock_mcp = ctx.path("mock-mcp.sh");
    let mock_codex = ctx.path("mock-codex.py");

    write_local_source_layout(&ctx, "execute");
    write_mock_mcp_script(&mock_mcp, &["execute"]);
    write_mock_codex_script(&mock_codex);
    write_playwright_config(&ctx, &mock_mcp, Some(true));

    let resolve = parse_json_output(
        &ctx.cmd()
            .args(["resolve", "playwright", "--json", "--config"])
            .arg(&config_path)
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    let resolve_artifact = artifact_path(&resolve);

    let snapshot = parse_json_output(
        &ctx.cmd()
            .args(["snapshot", "--json", "--from-resolve"])
            .arg(&resolve_artifact)
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    let snapshot_artifact = artifact_path(&snapshot);

    let evidence = parse_json_output(
        &ctx.cmd()
            .args(["evidence", "--json", "--from-snapshot"])
            .arg(&snapshot_artifact)
            .args(["--tool", "execute"])
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    let evidence_artifact = artifact_path(&evidence);
    assert_eq!(
        evidence["result"]["tool_evidence"][0]["tool_name"]
            .as_str()
            .unwrap(),
        "execute"
    );

    let synthesis = parse_json_output(
        &ctx.cmd()
            .env("MCPSMITH_CODEX_COMMAND", &mock_codex)
            .args(["synthesize", "--json", "--from-evidence"])
            .arg(&evidence_artifact)
            .args(["--backend", "codex"])
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    let synthesis_artifact = artifact_path(&synthesis);

    let review = parse_json_output(
        &ctx.cmd()
            .env("MCPSMITH_CODEX_COMMAND", &mock_codex)
            .args(["review", "--json", "--from-bundle"])
            .arg(&synthesis_artifact)
            .args(["--backend", "codex"])
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    let review_artifact = artifact_path(&review);
    assert!(review["result"]["approved"].as_bool().unwrap());

    let verify = parse_json_output(
        &ctx.cmd()
            .args(["verify", "--json", "--from-bundle"])
            .arg(&review_artifact)
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    assert!(verify["result"]["passed"].as_bool().unwrap());
}

#[test]
fn test_mcpsmith_evidence_human_output_explains_confidence() {
    let ctx = TestContext::new();
    let config_path = ctx.config_path();
    let mock_mcp = ctx.path("mock-mcp.sh");

    write_local_source_layout(&ctx, "execute");
    write_mock_mcp_script(&mock_mcp, &["execute"]);
    write_playwright_config(&ctx, &mock_mcp, Some(true));

    ctx.cmd()
        .args(["evidence", "playwright", "--config"])
        .arg(&config_path)
        .args(["--tool", "execute"])
        .assert()
        .success()
        .stdout(predicate::str::contains("confidence="))
        .stdout(predicate::str::contains("(high)"))
        .stdout(predicate::str::contains("tests=1"))
        .stdout(predicate::str::contains("docs=1"))
        .stdout(predicate::str::contains("Confidence: high"));
}

#[test]
fn test_mcpsmith_synthesize_uses_mapper_fallback_for_low_confidence_evidence() {
    let ctx = TestContext::new();
    let config_path = ctx.config_path();
    let mock_mcp = ctx.path("source/bin/mock-mcp.sh");
    let mock_codex = ctx.path("mock-codex.py");
    let mapper_prompt = ctx.path("mapper-prompt.txt");

    write_low_confidence_source_layout(&ctx, "execute");
    fs::create_dir_all(mock_mcp.parent().unwrap()).unwrap();
    write_mock_mcp_script(&mock_mcp, &["execute"]);
    write_mock_codex_script(&mock_codex);
    write_playwright_config(&ctx, &mock_mcp, Some(true));

    let synthesis = parse_json_output(
        &ctx.cmd()
            .env("MCPSMITH_CODEX_COMMAND", &mock_codex)
            .env("MCPSMITH_BACKEND_CAPTURE_PATH", &mapper_prompt)
            .args([
                "synthesize",
                "playwright",
                "--json",
                "--backend",
                "codex",
                "--config",
            ])
            .arg(&config_path)
            .assert()
            .success()
            .get_output()
            .stdout,
    );

    let pack = &synthesis["result"]["bundle"]["evidence"]["tool_evidence"][0];
    assert_eq!(pack["tool_name"].as_str().unwrap(), "execute");
    assert!(pack["confidence"].as_f64().unwrap() >= 0.75);
    assert_eq!(
        pack["registration"]["file_path"].as_str().unwrap(),
        "src/tool_index.ts"
    );
    assert_eq!(
        pack["handler"]["file_path"].as_str().unwrap(),
        "src/execute.ts"
    );
    assert_eq!(
        pack["mapper_fallback"]["backend"].as_str().unwrap(),
        "codex"
    );
    assert_eq!(
        pack["mapper_fallback"]["relevant_files"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let prompt = fs::read_to_string(&mapper_prompt).unwrap();
    assert!(prompt.contains("src/tool_index.ts"));
    assert!(prompt.contains("src/execute.ts"));
    assert!(!prompt.contains("src/noise-07.ts"));
}

#[test]
fn test_mcpsmith_synthesize_human_output_reports_mapper_fallback() {
    let ctx = TestContext::new();
    let config_path = ctx.config_path();
    let mock_mcp = ctx.path("source/bin/mock-mcp.sh");
    let mock_codex = ctx.path("mock-codex.py");

    write_low_confidence_source_layout(&ctx, "execute");
    fs::create_dir_all(mock_mcp.parent().unwrap()).unwrap();
    write_mock_mcp_script(&mock_mcp, &["execute"]);
    write_mock_codex_script(&mock_codex);
    write_playwright_config(&ctx, &mock_mcp, Some(true));

    ctx.cmd()
        .env("MCPSMITH_CODEX_COMMAND", &mock_codex)
        .args(["synthesize", "playwright", "--backend", "codex", "--config"])
        .arg(&config_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("Mapper fallback: 1 tool(s)"))
        .stdout(predicate::str::contains("registration=src/tool_index.ts"))
        .stdout(predicate::str::contains("handler=src/execute.ts"));
}

#[test]
fn test_mcpsmith_synthesize_skips_mapper_fallback_for_high_confidence_evidence() {
    let ctx = TestContext::new();
    let config_path = ctx.config_path();
    let mock_mcp = ctx.path("mock-mcp.sh");
    let mock_codex = ctx.path("mock-codex.py");
    let mapper_prompt = ctx.path("mapper-prompt.txt");

    write_local_source_layout(&ctx, "execute");
    write_mock_mcp_script(&mock_mcp, &["execute"]);
    write_mock_codex_script(&mock_codex);
    write_playwright_config(&ctx, &mock_mcp, Some(true));

    let synthesis = parse_json_output(
        &ctx.cmd()
            .env("MCPSMITH_CODEX_COMMAND", &mock_codex)
            .env("MCPSMITH_BACKEND_CAPTURE_PATH", &mapper_prompt)
            .args([
                "synthesize",
                "playwright",
                "--json",
                "--backend",
                "codex",
                "--config",
            ])
            .arg(&config_path)
            .assert()
            .success()
            .get_output()
            .stdout,
    );

    let pack = &synthesis["result"]["bundle"]["evidence"]["tool_evidence"][0];
    assert!(pack.get("mapper_fallback").is_none());
    assert!(
        fs::read_to_string(&mapper_prompt)
            .map(|body| body.trim().is_empty())
            .unwrap_or(true)
    );
}

#[test]
fn test_mcpsmith_bare_one_shot_dry_run_writes_preview_and_keeps_config() {
    let ctx = TestContext::new();
    let config_path = ctx.config_path();
    let mock_mcp = ctx.path("mock-mcp.sh");
    let mock_claude = ctx.path("mock-claude.py");

    write_local_source_layout(&ctx, "execute");
    write_mock_mcp_script(&mock_mcp, &["execute"]);
    write_mock_claude_script(&mock_claude);
    write_playwright_config(&ctx, &mock_mcp, Some(true));

    let run = parse_json_output(
        &ctx.cmd()
            .env("MCPSMITH_CLAUDE_COMMAND", &mock_claude)
            .env("MCPSMITH_CODEX_COMMAND", ctx.path("missing-codex"))
            .args([
                "playwright",
                "--json",
                "--dry-run",
                "--backend",
                "claude",
                "--config",
            ])
            .arg(&config_path)
            .assert()
            .success()
            .get_output()
            .stdout,
    );

    assert_eq!(run["status"].as_str().unwrap(), "dry-run");
    let preview_dir = PathBuf::from(run["skills_dir"].as_str().unwrap());
    assert!(preview_dir.exists());
    assert!(preview_dir.join("playwright").join("SKILL.md").exists());
    assert!(
        preview_dir
            .join("playwright--execute")
            .join("SKILL.md")
            .exists()
    );

    let updated = fs::read_to_string(&config_path).unwrap();
    assert!(updated.contains("playwright"));
    assert_eq!(count_backups(&config_path), 0);
}

#[test]
fn test_mcpsmith_bare_one_shot_applies_skills_and_updates_config() {
    let ctx = TestContext::new();
    let config_path = ctx.config_path();
    let skills_dir = ctx.skills_dir();
    let mock_mcp = ctx.path("mock-mcp.sh");
    let mock_codex = ctx.path("mock-codex.py");

    write_local_source_layout(&ctx, "execute");
    write_mock_mcp_script(&mock_mcp, &["execute"]);
    write_mock_codex_script(&mock_codex);
    write_playwright_config(&ctx, &mock_mcp, Some(true));

    let run = parse_json_output(
        &ctx.cmd()
            .env("MCPSMITH_CODEX_COMMAND", &mock_codex)
            .args(["playwright", "--json", "--backend", "codex", "--config"])
            .arg(&config_path)
            .args(["--skills-dir"])
            .arg(&skills_dir)
            .assert()
            .success()
            .get_output()
            .stdout,
    );

    assert_eq!(run["status"].as_str().unwrap(), "applied");
    assert!(ctx.orchestrator_skill_path("playwright").exists());
    assert!(ctx.tool_skill_path("playwright", "execute").exists());
    assert!(ctx.manifest_path("playwright").exists());

    let updated = fs::read_to_string(&config_path).unwrap();
    assert!(!updated.contains("playwright"));
    assert_eq!(count_backups(&config_path), 1);
}

#[test]
fn test_mcpsmith_bare_one_shot_applies_skills_and_updates_all_matching_configs() {
    let ctx = TestContext::new();
    let config_one = ctx.path("codex.json");
    let config_two = ctx.path("claude.json");
    let skills_dir = ctx.skills_dir();
    let mock_mcp = ctx.path("mock-mcp.sh");
    let mock_codex = ctx.path("mock-codex.py");

    write_local_source_layout(&ctx, "execute");
    write_mock_mcp_script(&mock_mcp, &["execute"]);
    write_mock_codex_script(&mock_codex);
    write_playwright_config_to_path(&ctx, &config_one, &mock_mcp, Some(true));
    write_playwright_config_to_path(&ctx, &config_two, &mock_mcp, Some(true));

    let run = parse_json_output(
        &ctx.cmd()
            .env("MCPSMITH_CODEX_COMMAND", &mock_codex)
            .args(["playwright", "--json", "--backend", "codex", "--config"])
            .arg(&config_one)
            .args(["--config"])
            .arg(&config_two)
            .args(["--skills-dir"])
            .arg(&skills_dir)
            .assert()
            .success()
            .get_output()
            .stdout,
    );

    assert_eq!(run["status"].as_str().unwrap(), "applied");
    assert!(ctx.orchestrator_skill_path("playwright").exists());
    assert!(ctx.tool_skill_path("playwright", "execute").exists());
    assert!(ctx.manifest_path("playwright").exists());

    let config_backups = run["config_backups"].as_array().unwrap();
    assert_eq!(config_backups.len(), 2);

    let updated_one = fs::read_to_string(&config_one).unwrap();
    let updated_two = fs::read_to_string(&config_two).unwrap();
    assert!(!updated_one.contains("playwright"));
    assert!(!updated_two.contains("playwright"));
    assert_eq!(count_backups(&config_one), 1);
    assert_eq!(count_backups(&config_two), 1);
}

#[test]
fn test_mcpsmith_uninstall_removes_installed_skills() {
    let ctx = TestContext::new();
    let config_path = ctx.config_path();
    let skills_dir = ctx.skills_dir();
    let mock_mcp = ctx.path("mock-mcp.sh");
    let mock_codex = ctx.path("mock-codex.py");

    write_local_source_layout(&ctx, "execute");
    write_mock_mcp_script(&mock_mcp, &["execute"]);
    write_mock_codex_script(&mock_codex);
    write_playwright_config(&ctx, &mock_mcp, Some(true));

    let run = parse_json_output(
        &ctx.cmd()
            .env("MCPSMITH_CODEX_COMMAND", &mock_codex)
            .args(["playwright", "--json", "--backend", "codex", "--config"])
            .arg(&config_path)
            .args(["--skills-dir"])
            .arg(&skills_dir)
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    assert_eq!(run["status"].as_str().unwrap(), "applied");
    assert!(ctx.orchestrator_skill_path("playwright").exists());
    assert!(ctx.tool_skill_path("playwright", "execute").exists());

    ctx.cmd()
        .args(["uninstall", "playwright", "--skills-dir"])
        .arg(&skills_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("Uninstalled"))
        .stdout(predicate::str::contains("removed"));

    assert!(!ctx.orchestrator_skill_path("playwright").exists());
    assert!(!ctx.tool_skill_path("playwright", "execute").exists());
    assert!(!ctx.manifest_path("playwright").exists());
}

#[test]
fn test_mcpsmith_uninstall_json_returns_structured_report() {
    let ctx = TestContext::new();
    let config_path = ctx.config_path();
    let skills_dir = ctx.skills_dir();
    let mock_mcp = ctx.path("mock-mcp.sh");
    let mock_codex = ctx.path("mock-codex.py");

    write_local_source_layout(&ctx, "execute");
    write_mock_mcp_script(&mock_mcp, &["execute"]);
    write_mock_codex_script(&mock_codex);
    write_playwright_config(&ctx, &mock_mcp, Some(true));

    ctx.cmd()
        .env("MCPSMITH_CODEX_COMMAND", &mock_codex)
        .args(["playwright", "--json", "--backend", "codex", "--config"])
        .arg(&config_path)
        .args(["--skills-dir"])
        .arg(&skills_dir)
        .assert()
        .success();

    let report = parse_json_output(
        &ctx.cmd()
            .args(["uninstall", "playwright", "--json", "--skills-dir"])
            .arg(&skills_dir)
            .assert()
            .success()
            .get_output()
            .stdout,
    );

    assert_eq!(report["server_slug"].as_str().unwrap(), "playwright");
    assert!(!report["removed_paths"].as_array().unwrap().is_empty());
}

#[test]
fn test_mcpsmith_uninstall_nonexistent_server_fails() {
    let ctx = TestContext::new();
    let skills_dir = ctx.skills_dir();

    ctx.cmd()
        .args(["uninstall", "nonexistent", "--skills-dir"])
        .arg(&skills_dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains("No installed skill found"));
}

#[test]
fn test_mcpsmith_uninstall_help_shows_subcommand() {
    let ctx = TestContext::new();

    ctx.cmd()
        .args(["uninstall", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Remove previously installed skills",
        ))
        .stdout(predicate::str::contains("--skills-dir"));
}

#[test]
fn test_mcpsmith_review_second_pass_applies_revision_before_verify() {
    let ctx = TestContext::new();
    let config_path = ctx.config_path();
    let mock_mcp = ctx.path("mock-mcp.sh");
    let mock_codex = ctx.path("mock-codex-review.py");

    write_local_source_layout(&ctx, "execute");
    write_mock_mcp_script(&mock_mcp, &["execute"]);
    write_mock_codex_script_with_review_fix(&mock_codex);
    write_playwright_config(&ctx, &mock_mcp, Some(true));

    let synthesis = parse_json_output(
        &ctx.cmd()
            .env("MCPSMITH_CODEX_COMMAND", &mock_codex)
            .args([
                "synthesize",
                "playwright",
                "--json",
                "--backend",
                "codex",
                "--config",
            ])
            .arg(&config_path)
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    let synthesis_artifact = artifact_path(&synthesis);

    let review = parse_json_output(
        &ctx.cmd()
            .env("MCPSMITH_CODEX_COMMAND", &mock_codex)
            .args(["review", "--json", "--from-bundle"])
            .arg(&synthesis_artifact)
            .args(["--backend", "codex"])
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    let review_artifact = artifact_path(&review);
    assert!(review["result"]["approved"].as_bool().unwrap());
    assert_eq!(review["result"]["findings"].as_array().unwrap().len(), 1);

    let verify = parse_json_output(
        &ctx.cmd()
            .args(["verify", "--json", "--from-bundle"])
            .arg(&review_artifact)
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    assert!(verify["result"]["passed"].as_bool().unwrap());
}

#[test]
fn test_mcpsmith_catalog_sync_and_stats_use_machine_readable_endpoints() {
    let ctx = TestContext::new();
    let registry = StubRegistryServer::start();

    let sync = parse_json_output(
        &ctx.cmd()
            .env(
                "MCPSMITH_OFFICIAL_REGISTRY_BASE_URL",
                format!("{}/official", registry.base_url),
            )
            .env(
                "MCPSMITH_SMITHERY_REGISTRY_BASE_URL",
                format!("{}/smithery", registry.base_url),
            )
            .args([
                "catalog",
                "sync",
                "--json",
                "--provider",
                "official",
                "--provider",
                "smithery",
            ])
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    let sync_artifact = artifact_path(&sync);
    assert_eq!(
        sync["result"]["stats"]["unique_servers"].as_u64().unwrap(),
        2
    );
    assert_eq!(
        sync["result"]["stats"]["source_resolvable"]
            .as_u64()
            .unwrap(),
        1
    );
    assert_eq!(sync["result"]["stats"]["remote_only"].as_u64().unwrap(), 1);

    let stats = parse_json_output(
        &ctx.cmd()
            .args(["catalog", "stats", "--json", "--from"])
            .arg(&sync_artifact)
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    assert_eq!(stats["result"]["unique_servers"].as_u64().unwrap(), 2);
    assert_eq!(stats["result"]["source_resolvable"].as_u64().unwrap(), 1);
}

#[test]
fn test_mcpsmith_catalog_sync_glama_provider_fetches_and_normalizes_records() {
    let ctx = TestContext::new();
    let registry = StubRegistryServer::start();

    let sync = parse_json_output(
        &ctx.cmd()
            .env(
                "MCPSMITH_GLAMA_REGISTRY_BASE_URL",
                format!("{}/glama", registry.base_url),
            )
            .args(["catalog", "sync", "--json", "--provider", "glama"])
            .assert()
            .success()
            .get_output()
            .stdout,
    );

    let providers = sync["result"]["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0]["provider"].as_str().unwrap(), "glama");
    assert!(providers[0]["supported"].as_bool().unwrap());
    assert_eq!(providers[0]["record_count"].as_u64().unwrap(), 2);

    let servers = sync["result"]["servers"].as_array().unwrap();
    assert_eq!(servers.len(), 2);

    let resolvable = servers
        .iter()
        .find(|s| s["canonical_name"].as_str().unwrap() == "glama-demo")
        .unwrap();
    assert_eq!(
        resolvable["source_resolution"]["status"].as_str().unwrap(),
        "resolvable"
    );
    assert_eq!(
        resolvable["source_resolution"]["kind"].as_str().unwrap(),
        "repository-url"
    );

    let remote = servers
        .iter()
        .find(|s| s["canonical_name"].as_str().unwrap() == "glama-remote")
        .unwrap();
    assert_eq!(
        remote["source_resolution"]["status"].as_str().unwrap(),
        "remote-only"
    );
}

#[test]
fn test_mcpsmith_catalog_sync_glama_deduplicates_with_official() {
    let ctx = TestContext::new();
    let registry = StubRegistryServer::start();

    let sync = parse_json_output(
        &ctx.cmd()
            .env(
                "MCPSMITH_OFFICIAL_REGISTRY_BASE_URL",
                format!("{}/official", registry.base_url),
            )
            .env(
                "MCPSMITH_GLAMA_REGISTRY_BASE_URL",
                format!("{}/glama", registry.base_url),
            )
            .args([
                "catalog",
                "sync",
                "--json",
                "--provider",
                "official",
                "--provider",
                "glama",
            ])
            .assert()
            .success()
            .get_output()
            .stdout,
    );

    let providers = sync["result"]["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 2);

    let stats = &sync["result"]["stats"];
    assert!(stats["source_resolvable"].as_u64().unwrap() >= 2);
}

#[test]
fn test_mcpsmith_catalog_sync_glama_writes_raw_capture() {
    let ctx = TestContext::new();
    let registry = StubRegistryServer::start();

    let sync = parse_json_output(
        &ctx.cmd()
            .env(
                "MCPSMITH_GLAMA_REGISTRY_BASE_URL",
                format!("{}/glama", registry.base_url),
            )
            .args(["catalog", "sync", "--json", "--provider", "glama"])
            .assert()
            .success()
            .get_output()
            .stdout,
    );

    let providers = sync["result"]["providers"].as_array().unwrap();
    let glama = &providers[0];
    let capture_path = glama["raw_capture_path"].as_str().unwrap();
    assert!(PathBuf::from(capture_path).exists());
    let raw: Value = serde_json::from_str(&fs::read_to_string(capture_path).unwrap()).unwrap();
    let pages = raw.as_array().unwrap();
    assert!(!pages.is_empty());
    assert!(pages[0].get("servers").is_some());
    assert!(pages[0].get("pageInfo").is_some());
}

#[test]
fn test_mcpsmith_catalog_sync_human_output_shows_glama_stats() {
    let ctx = TestContext::new();
    let registry = StubRegistryServer::start();

    ctx.cmd()
        .env(
            "MCPSMITH_GLAMA_REGISTRY_BASE_URL",
            format!("{}/glama", registry.base_url),
        )
        .args(["catalog", "sync", "--provider", "glama"])
        .assert()
        .success()
        .stdout(predicate::str::contains("glama: supported=true records=2"))
        .stdout(predicate::str::contains("resolvable=1"));
}

#[test]
fn test_mcpsmith_catalog_sync_respects_official_registry_limit_cap() {
    let ctx = TestContext::new();
    let registry = StrictOfficialLimitRegistryServer::start();

    ctx.cmd()
        .env(
            "MCPSMITH_OFFICIAL_REGISTRY_BASE_URL",
            format!("{}/official", registry.base_url),
        )
        .args(["catalog", "sync", "--json", "--provider", "official"])
        .assert()
        .success();
}

#[test]
fn test_mcpsmith_completions_generates_output_for_each_shell() {
    let ctx = TestContext::new();
    for shell in ["bash", "zsh", "fish", "elvish", "powershell"] {
        ctx.cmd()
            .args(["completions", shell])
            .assert()
            .success()
            .stdout(predicate::str::contains("mcpsmith"));
    }
}

#[test]
fn test_mcpsmith_completions_rejects_invalid_shell() {
    let ctx = TestContext::new();
    ctx.cmd()
        .args(["completions", "nushell"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
}

#[test]
fn test_mcpsmith_completions_is_hidden_from_help() {
    let ctx = TestContext::new();
    ctx.cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("completions").not());
}

#[test]
fn test_mcpsmith_catalog_sync_defaults_to_all_three_providers() {
    let ctx = TestContext::new();
    let registry = StubRegistryServer::start();

    let sync = parse_json_output(
        &ctx.cmd()
            .env(
                "MCPSMITH_OFFICIAL_REGISTRY_BASE_URL",
                format!("{}/official", registry.base_url),
            )
            .env(
                "MCPSMITH_SMITHERY_REGISTRY_BASE_URL",
                format!("{}/smithery", registry.base_url),
            )
            .env(
                "MCPSMITH_GLAMA_REGISTRY_BASE_URL",
                format!("{}/glama", registry.base_url),
            )
            .args(["catalog", "sync", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout,
    );

    let providers = sync["result"]["providers"].as_array().unwrap();
    let provider_names = providers
        .iter()
        .map(|provider| provider["provider"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(provider_names, vec!["official", "smithery", "glama"]);
    assert!(
        providers
            .iter()
            .all(|provider| provider["supported"].as_bool().unwrap())
    );
    assert_eq!(
        sync["result"]["stats"]["unsupported_provider_records"]
            .as_u64()
            .unwrap(),
        0
    );
}
