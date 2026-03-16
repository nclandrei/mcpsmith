use assert_cmd::Command;
use serde_json::{Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub struct TestContext {
    tempdir: tempfile::TempDir,
}

impl TestContext {
    pub fn new() -> Self {
        Self {
            tempdir: tempfile::tempdir().unwrap(),
        }
    }

    pub fn home(&self) -> &Path {
        self.tempdir.path()
    }

    pub fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.home().join(relative)
    }

    pub fn cmd(&self) -> Command {
        let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("mcpsmith");
        cmd.current_dir(self.home());
        cmd.env("HOME", self.home());
        cmd
    }

    pub fn config_path(&self) -> PathBuf {
        self.path("settings.json")
    }

    pub fn skills_dir(&self) -> PathBuf {
        self.path("skills")
    }

    pub fn orchestrator_skill_path(&self, server_slug: &str) -> PathBuf {
        self.skills_dir().join(server_slug).join("SKILL.md")
    }

    pub fn tool_skill_path(&self, server_slug: &str, tool_slug: &str) -> PathBuf {
        self.skills_dir()
            .join(format!("{server_slug}--{tool_slug}"))
            .join("SKILL.md")
    }

    pub fn manifest_path(&self, server_slug: &str) -> PathBuf {
        self.skills_dir()
            .join(server_slug)
            .join(".mcpsmith")
            .join("manifest.json")
    }

    pub fn write_server_config(
        &self,
        server_name: &str,
        command: &Path,
        description: Option<&str>,
        read_only: Option<bool>,
    ) {
        let mut server = Map::new();
        server.insert(
            "command".to_string(),
            Value::String(command.to_string_lossy().into_owned()),
        );
        if let Some(description) = description {
            server.insert(
                "description".to_string(),
                Value::String(description.to_string()),
            );
        }
        if let Some(read_only) = read_only {
            server.insert("readOnly".to_string(), Value::Bool(read_only));
        }

        let mut servers = Map::new();
        servers.insert(server_name.to_string(), Value::Object(server));
        self.write_mcp_servers(servers);
    }

    pub fn write_server_config_to_path(
        &self,
        path: &Path,
        server_name: &str,
        command: &Path,
        description: Option<&str>,
        read_only: Option<bool>,
    ) {
        let mut server = Map::new();
        server.insert(
            "command".to_string(),
            Value::String(command.to_string_lossy().into_owned()),
        );
        if let Some(description) = description {
            server.insert(
                "description".to_string(),
                Value::String(description.to_string()),
            );
        }
        if let Some(read_only) = read_only {
            server.insert("readOnly".to_string(), Value::Bool(read_only));
        }

        let mut servers = Map::new();
        servers.insert(server_name.to_string(), Value::Object(server));
        self.write_mcp_servers_to_path(path, servers);
    }

    pub fn write_remote_server_config(&self, server_name: &str, url: &str) {
        let mut server = Map::new();
        server.insert("url".to_string(), Value::String(url.to_string()));

        let mut servers = Map::new();
        servers.insert(server_name.to_string(), Value::Object(server));
        self.write_mcp_servers(servers);
    }

    pub fn write_mcp_servers(&self, servers: Map<String, Value>) {
        self.write_mcp_servers_to_path(&self.config_path(), servers);
    }

    pub fn write_mcp_servers_to_path(&self, path: &Path, servers: Map<String, Value>) {
        let mut root = Map::new();
        root.insert("mcpServers".to_string(), Value::Object(servers));

        fs::write(
            path,
            serde_json::to_string_pretty(&Value::Object(root)).unwrap(),
        )
        .unwrap();
    }
}

pub fn count_backups(config_path: &Path) -> usize {
    let parent = config_path.parent().unwrap_or_else(|| Path::new("."));
    let prefix = format!(
        "{}.bak-",
        config_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
    );

    fs::read_dir(parent)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .count()
}

pub fn write_local_source_layout(ctx: &TestContext, tool_name: &str) {
    fs::create_dir_all(ctx.path("src")).unwrap();
    fs::create_dir_all(ctx.path("tests")).unwrap();
    fs::write(
        ctx.path("package.json"),
        format!(
            r#"{{
  "name": "@acme/{tool_name}-mcp",
  "version": "1.2.3",
  "repository": {{
    "type": "git",
    "url": "https://github.com/acme/{tool_name}-mcp.git"
  }}
}}
"#
        ),
    )
    .unwrap();
    fs::write(
        ctx.path("README.md"),
        format!(
            "# Demo MCP\n\nThe `{tool_name}` tool reads a query string and returns text output.\n"
        ),
    )
    .unwrap();
    fs::write(
        ctx.path("src/server.ts"),
        format!(
            "export function register(server) {{\n  server.tool(\"{tool_name}\", {{ description: \"Tool {tool_name}\", inputSchema: {{ type: \"object\", required: [\"query\"], properties: {{ query: {{ type: \"string\" }} }} }} }}, async (args) => handle{tool_name}(args));\n}}\n\nasync function handle{tool_name}(args) {{\n  return {{ content: [{{ type: \"text\", text: args.query }}], isError: false }};\n}}\n"
        ),
    )
    .unwrap();
    fs::write(
        ctx.path(format!("tests/{tool_name}.spec.ts")),
        format!(
            "it(\"handles {tool_name}\", async () => {{\n  const result = await callTool(\"{tool_name}\", {{ query: \"demo\" }});\n  expect(result.isError).toBe(false);\n}});\n"
        ),
    )
    .unwrap();
}

pub fn write_mock_mcp_script(path: &Path, tool_names: &[&str]) {
    let tools = tool_names
        .iter()
        .map(|name| {
            format!(
                r#"{{\"name\":\"{name}\",\"description\":\"Tool {name}\",\"inputSchema\":{{\"type\":\"object\",\"required\":[\"query\"],\"properties\":{{\"query\":{{\"type\":\"string\"}}}}}}}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let body = format!(
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2025-03-26","capabilities":{{}}}}}}\n'
      ;;
    *'"method":"tools/list"'*)
      printf '{{"jsonrpc":"2.0","id":2,"result":{{"tools":[{tools}]}}}}\n'
      ;;
    *'"method":"tools/call"'*)
      if echo "$line" | grep -q '"query":"'; then
        printf '{{"jsonrpc":"2.0","id":2,"result":{{"content":[{{"type":"text","text":"ok"}}],"isError":false}}}}\n'
      else
        printf '{{"jsonrpc":"2.0","id":2,"error":{{"code":-32602,"message":"invalid query"}}}}\n'
      fi
      ;;
  esac
done
"#
    );
    write_agent_script(path, &body);
}

pub fn write_mock_codex_script(path: &Path) {
    write_mock_backend_script(path, "mock-codex");
}

pub fn write_mock_codex_script_with_delay(path: &Path, delay_ms: u64) {
    write_mock_backend_script_with_delay(path, "mock-codex", delay_ms);
}

pub fn write_mock_claude_script(path: &Path) {
    write_mock_backend_script(path, "mock-claude");
}

pub fn write_mock_codex_script_with_review_fix(path: &Path) {
    write_mock_backend_script_with_mode(path, "mock-codex", true);
}

fn write_mock_backend_script(path: &Path, version_label: &str) {
    write_mock_backend_script_with_settings(path, version_label, false, 0);
}

fn write_mock_backend_script_with_delay(path: &Path, version_label: &str, delay_ms: u64) {
    write_mock_backend_script_with_settings(path, version_label, false, delay_ms);
}

fn write_mock_backend_script_with_mode(path: &Path, version_label: &str, revise_review: bool) {
    write_mock_backend_script_with_settings(path, version_label, revise_review, 0);
}

fn write_mock_backend_script_with_settings(
    path: &Path,
    version_label: &str,
    revise_review: bool,
    delay_ms: u64,
) {
    let body = r#"#!/usr/bin/env python3
import json
import os
import re
import sys
import time

VERSION = __VERSION__
REVISE_REVIEW = __REVISE__
DELAY_SECONDS = __DELAY_SECONDS__

def tool_name(prompt: str) -> str:
    match = re.search(r'"tool_name"\s*:\s*"([^"]+)"', prompt)
    if match:
        return match.group(1)
    match = re.search(r'"name"\s*:\s*"([^"]+)"', prompt)
    if match:
        return match.group(1)
    return "execute"

def build_draft(name: str, placeholder: bool):
    step_details = "TODO_REVIEW_FIX" if placeholder else "Collect the exact query before running the command."
    return {
        "tool_name": name,
        "semantic_summary": {
            "what_it_does": f"The {name} tool runs a grounded local workflow for the requested query.",
            "required_inputs": ["query"],
            "prerequisites": [],
            "side_effect_level": "read-only",
            "success_signals": ["Command exits successfully.", "The output includes the requested query."],
            "failure_modes": ["Missing required query input."],
            "citations": ["mock-mcp.sh", "src/server.ts", f"tests/{name}.spec.ts"],
            "confidence": 0.91
        },
        "workflow_skill": {
            "id": name,
            "title": f"{name} workflow",
            "goal": f"Run the {name} workflow without relying on the MCP transport.",
            "when_to_use": f"Use this when you need to run the {name} workflow locally.",
            "trigger_phrases": [f"run {name}", f"use {name}"],
            "origin_tools": [name],
            "required_context": [
                {
                    "name": "query",
                    "guidance": "Collect the exact query or target before running the workflow.",
                    "required": True
                }
            ],
            "context_acquisition": ["If the query is missing, ask the user for it instead of guessing."],
            "stop_and_ask": ["Stop if the query is ambiguous."],
            "native_steps": [
                {
                    "title": "Run the local command",
                    "command": "printf '%s\\n' \"$QUERY\"",
                    "details": step_details
                }
            ],
            "verification": ["Confirm the command returned output for the provided query."],
            "return_contract": ["Return the command output and the query that was used."],
            "guardrails": ["Do not invent query values."],
            "evidence": ["mock-mcp.sh", "src/server.ts", f"tests/{name}.spec.ts"],
            "confidence": 0.91
        },
        "helper_scripts": []
    }

def build_synthesis(name: str, placeholder: bool):
    draft = build_draft(name, placeholder)
    return {
        "semantic_summary": draft["semantic_summary"],
        "workflow_skill": draft["workflow_skill"]
    }

def mapper_paths(prompt: str):
    return re.findall(r'"path"\s*:\s*"([^"]+)"', prompt)

def build_mapper(prompt: str):
    paths = mapper_paths(prompt)
    registration = next(
        (path for path in paths if any(token in path for token in ("tool_index", "manifest", "server", "index"))),
        None,
    )
    handler = next(
        (path for path in paths if any(token in path for token in ("execute", "handler", "tool")) and path != registration),
        None,
    )
    relevant = []
    if registration:
        relevant.append({
            "path": registration,
            "role": "registration",
            "why": "Contains the tool registration entry.",
            "confidence": 0.86,
        })
    if handler:
        relevant.append({
            "path": handler,
            "role": "handler",
            "why": "Contains the tool implementation.",
            "confidence": 0.91,
        })
    return {"relevant_files": relevant}

if len(sys.argv) > 1 and sys.argv[1] in ("--version", "-v", "version"):
    print(VERSION)
    sys.exit(0)

output_path = None
for idx, arg in enumerate(sys.argv):
    if arg in ("--output-last-message", "-o") and idx + 1 < len(sys.argv):
        output_path = sys.argv[idx + 1]
        break

prompt = sys.stdin.read()
name = tool_name(prompt)
capture_path = os.environ.get("MCPSMITH_BACKEND_CAPTURE_PATH")

if DELAY_SECONDS > 0:
    time.sleep(DELAY_SECONDS)

if "reviewing a generated skill draft" in prompt:
    needs_fix = REVISE_REVIEW and "TODO_REVIEW_FIX" in prompt
    if needs_fix:
        payload = {
            "approved": False,
            "findings": ["Removed placeholder detail text and kept the workflow grounded."],
            "revised_draft": build_draft(name, False)
        }
    else:
        payload = {
            "approved": True,
            "findings": [],
            "revised_draft": None
        }
elif "mapping low-confidence tool evidence to relevant source files" in prompt:
    if capture_path:
        with open(capture_path, "w", encoding="utf-8") as handle:
            handle.write(prompt)
    payload = build_mapper(prompt)
elif "converting one MCP tool into a standalone local skill" in prompt:
    payload = build_synthesis(name, REVISE_REVIEW)
else:
    payload = {"approved": True, "findings": [], "revised_draft": None}

body = json.dumps(payload)
if output_path:
    with open(output_path, "w", encoding="utf-8") as handle:
        handle.write(body)
else:
    print(json.dumps({"output": body}))
"#
    .replace("__VERSION__", &format!("{version_label:?}"))
    .replace("__REVISE__", if revise_review { "True" } else { "False" })
    .replace("__DELAY_SECONDS__", &format!("{:.3}", delay_ms as f64 / 1000.0));
    write_agent_script(path, &body);
}

fn write_agent_script(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    }
}
