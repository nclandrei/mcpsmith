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
        cmd.env("HOME", self.home());
        cmd
    }

    pub fn config_path(&self) -> PathBuf {
        self.path("settings.json")
    }

    pub fn dossier_path(&self) -> PathBuf {
        self.path("dossier.json")
    }

    pub fn report_path(&self) -> PathBuf {
        self.path("contract-report.json")
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
        write_server_config(
            &self.config_path(),
            server_name,
            command,
            description,
            read_only,
        );
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

pub fn write_counting_mock_mcp_script(path: &Path, count_path: &Path, tool_names: &[&str]) {
    let tools = tool_names
        .iter()
        .map(|name| {
            format!(
                r#"{{\"name\":\"{name}\",\"description\":\"Tool {name}\",\"inputSchema\":{{\"type\":\"object\",\"required\":[\"query\"],\"properties\":{{\"query\":{{\"type\":\"string\"}}}}}}}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let count_path = count_path.to_string_lossy();
    let body = format!(
        r#"#!/bin/sh
printf 'start\n' >> '{count_path}'
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
    write_mock_codex_script_for(path, &["execute"]);
}

pub fn write_mock_codex_script_for(path: &Path, tools: &[&str]) {
    let payload = render_workflow_payload(tools, 0.9);
    let body = format!(
        r#"#!/bin/sh
if [ "$1" = "--version" ] || [ "$1" = "-v" ] || [ "$1" = "version" ]; then
  echo "mock-codex"
  exit 0
fi
last_message_file=""
while [ $# -gt 0 ]; do
  case "$1" in
    --output-last-message|-o)
      last_message_file="$2"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done
cat > /dev/null
[ -n "$last_message_file" ] || exit 12
cat > "$last_message_file" <<'JSON'
{payload}
JSON
"#
    );
    write_agent_script(path, &body);
}

pub fn write_mock_claude_script(path: &Path) {
    write_mock_claude_script_for(path, &["execute"]);
}

pub fn write_mock_claude_script_for(path: &Path, tools: &[&str]) {
    let payload = render_workflow_payload(tools, 0.85);
    let body = format!(
        r#"#!/bin/sh
if [ "$1" = "--version" ] || [ "$1" = "-v" ] || [ "$1" = "version" ]; then
  echo "mock-claude"
  exit 0
fi
cat > /dev/null
cat <<'JSON'
{payload}
JSON
"#
    );
    write_agent_script(path, &body);
}

pub fn write_mock_mcp_no_schema_script(path: &Path, tool_name: &str) {
    let body = format!(
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2025-03-26","capabilities":{{}}}}}}\n'
      ;;
    *'"method":"tools/list"'*)
      printf '{{"jsonrpc":"2.0","id":2,"result":{{"tools":[{{"name":"{tool_name}"}}]}}}}\n'
      ;;
    *'"method":"tools/call"'*)
      printf '{{"jsonrpc":"2.0","id":2,"result":{{"content":[{{"type":"text","text":"ok"}}],"isError":false}}}}\n'
      ;;
  esac
done
"#
    );
    write_agent_script(path, &body);
}

pub fn write_mock_mcp_id_schema_script(path: &Path, tool_name: &str) {
    let body = format!(
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2025-03-26","capabilities":{{}}}}}}\n'
      ;;
    *'"method":"tools/list"'*)
      printf '{{"jsonrpc":"2.0","id":2,"result":{{"tools":[{{"name":"{tool_name}","description":"delete item","inputSchema":{{"type":"object","required":["id"],"properties":{{"id":{{"type":"string"}}}}}}}}]}}}}\n'
      ;;
    *'"method":"tools/call"'*)
      if echo "$line" | grep -q '"id":"'; then
        printf '{{"jsonrpc":"2.0","id":2,"result":{{"content":[{{"type":"text","text":"ok"}}],"isError":false}}}}\n'
      else
        printf '{{"jsonrpc":"2.0","id":2,"error":{{"code":-32602,"message":"missing id"}}}}\n'
      fi
      ;;
  esac
done
"#
    );
    write_agent_script(path, &body);
}

pub fn write_mock_mcp_context_error_script(path: &Path, tool_name: &str) {
    let body = format!(
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2025-03-26","capabilities":{{}}}}}}\n'
      ;;
    *'"method":"tools/list"'*)
      printf '{{"jsonrpc":"2.0","id":2,"result":{{"tools":[{{"name":"{tool_name}","description":"inspect workspace state","inputSchema":{{"type":"object","required":["workspaceRoot"],"properties":{{"workspaceRoot":{{"type":"string"}}}}}}}}]}}}}\n'
      ;;
    *'"method":"tools/call"'*)
      if echo "$line" | grep -q '"workspaceRoot":"sample"'; then
        printf '{{"jsonrpc":"2.0","id":2,"result":{{"content":[{{"type":"text","text":"ENOENT: no such file or directory, stat '\''sample'\''"}}],"isError":true}}}}\n'
      else
        printf '{{"jsonrpc":"2.0","id":2,"error":{{"code":-32602,"message":"missing workspaceRoot"}}}}\n'
      fi
      ;;
  esac
done
"#
    );
    write_agent_script(path, &body);
}

pub fn write_mock_mcp_session_defaults_script(path: &Path, tool_name: &str) {
    let body = format!(
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2025-03-26","capabilities":{{}}}}}}\n'
      ;;
    *'"method":"tools/list"'*)
      printf '{{"jsonrpc":"2.0","id":2,"result":{{"tools":[{{"name":"{tool_name}","description":"Manage session defaults","inputSchema":{{"type":"object","properties":{{"arch":{{"type":"string","enum":["arm64","x86_64"]}}}}}}}}]}}}}\n'
      ;;
    *'"method":"tools/call"'*)
      if echo "$line" | grep -q '"arch":"invalid"'; then
        printf '{{"jsonrpc":"2.0","id":2,"error":{{"code":-32602,"message":"invalid arch"}}}}\n'
      elif echo "$line" | grep -q '"arguments":{{}}'; then
        printf '{{"jsonrpc":"2.0","id":2,"result":{{"content":[{{"type":"text","text":"Defaults updated"}}],"isError":false}}}}\n'
      else
        printf '{{"jsonrpc":"2.0","id":2,"error":{{"code":-32602,"message":"unexpected arguments"}}}}\n'
      fi
      ;;
  esac
done
"#
    );
    write_agent_script(path, &body);
}

fn write_server_config(
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

    let mut root = Map::new();
    root.insert("mcpServers".to_string(), Value::Object(servers));

    fs::write(
        path,
        serde_json::to_string_pretty(&Value::Object(root)).unwrap(),
    )
    .unwrap();
}

fn render_workflow_payload(tools: &[&str], confidence: f64) -> String {
    let workflows = tools
        .iter()
        .map(|name| {
            format!(
                r#"{{"id":"{name}","title":"{name} workflow","goal":"Perform {name} operations without relying on the MCP server.","when_to_use":"Use this when you need to run the {name} workflow with native commands.","trigger_phrases":["run {name}","use {name}"],"origin_tools":["{name}"],"prerequisite_workflows":[],"followup_workflows":[],"required_context":[{{"name":"query","guidance":"Collect the exact query or target before running the workflow.","required":true}}],"context_acquisition":["If the query is missing, ask the user for it instead of guessing."],"branching_rules":["If the target context is not ready, stop and gather it before running native commands."],"stop_and_ask":["Stop if the workflow would mutate state unexpectedly or the query is ambiguous."],"native_steps":[{{"title":"Run the native command","command":"printf '%s\\n' '{name}:$QUERY'","details":"Replace $QUERY with the exact collected query value."}}],"verification":["Confirm the native command completed successfully and returned output."],"return_contract":["Return the command output together with the exact query that was used."],"guardrails":["Do not invent query values or hidden defaults."],"evidence":["runtime metadata"],"confidence":{confidence}}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");

    format!(r#"{{"workflow_skills":[{workflows}]}}"#)
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
