use crate::MCPServerProfile;
use crate::skillset::normalize_tool_name;
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolSpec {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) input_schema: Option<Value>,
}

pub(crate) fn introspect_tool_specs(server: &MCPServerProfile) -> Result<Vec<ToolSpec>> {
    let command = server
        .command
        .as_deref()
        .context("MCP server has no command to introspect")?;

    let mut child = Command::new(command)
        .args(&server.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to spawn MCP command: {command}"))?;

    let init = serde_json::json!({
        "jsonrpc":"2.0",
        "id":1,
        "method":"initialize",
        "params":{
            "protocolVersion":"2025-03-26",
            "capabilities":{},
            "clientInfo":{"name":"mcpsmith","version":"0.1"}
        }
    });
    let list = serde_json::json!({
        "jsonrpc":"2.0",
        "id":2,
        "method":"tools/list",
        "params":{}
    });

    {
        let mut stdin = child.stdin.take().context("Failed to open MCP stdin")?;
        writeln!(stdin, "{init}").context("Failed to write MCP initialize request")?;
        writeln!(stdin, "{list}").context("Failed to write MCP tools/list request")?;
    }

    let output = child
        .wait_with_output()
        .context("Failed while waiting for MCP introspection output")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut tools = vec![];
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(id) = value.get("id").and_then(Value::as_i64) else {
            continue;
        };
        if id != 2 {
            continue;
        }
        let Some(items) = value
            .get("result")
            .and_then(|result| result.get("tools"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        tools = items
            .iter()
            .filter_map(|item| {
                let name = item.get("name").and_then(Value::as_str)?;
                let description = item
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(ToString::to_string);
                let input_schema = item
                    .get("inputSchema")
                    .or_else(|| item.get("input_schema"))
                    .filter(|schema| !schema.is_null())
                    .cloned();
                Some(ToolSpec {
                    name: normalize_tool_name(name),
                    description,
                    input_schema,
                })
            })
            .collect::<Vec<_>>();
        break;
    }

    let mut deduped: BTreeMap<String, ToolSpec> = BTreeMap::new();
    for tool in tools {
        deduped.entry(tool.name.clone()).or_insert(tool);
    }
    let tools = deduped.into_values().collect::<Vec<_>>();

    if tools.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "MCP introspection returned no tools for '{}'. stderr: {}",
            server.id,
            stderr.lines().take(5).collect::<Vec<_>>().join(" | ")
        );
    }

    Ok(tools)
}
