use crate::backend::clipped_preview;
use crate::skillset::normalize_tool_name;
use crate::{
    ContractTestOptions, MCPServerProfile, McpToolCallOutcome, ProbeErrorKind, ProbeFailure,
    ToolSpec,
};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

pub(crate) fn introspect_tools(server: &MCPServerProfile) -> Result<Vec<String>> {
    let mut tools = introspect_tool_specs(server)?
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    tools.sort();
    tools.dedup();
    if tools.is_empty() {
        bail!("MCP introspection returned no tools for '{}'.", server.id);
    }
    Ok(tools)
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

pub(crate) fn execute_mcp_tool_probe(
    server: &MCPServerProfile,
    tool_name: &str,
    args: &Value,
    options: ContractTestOptions,
    setup_calls: &[(String, Value)],
) -> std::result::Result<McpToolCallOutcome, ProbeFailure> {
    let mut last_err: Option<ProbeFailure> = None;
    for _attempt in 0..=options.probe_retries {
        match execute_mcp_tool_probe_once(
            server,
            tool_name,
            args,
            options.probe_timeout_seconds,
            setup_calls,
        ) {
            Ok(outcome) => return Ok(outcome),
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err.unwrap_or(ProbeFailure {
        kind: ProbeErrorKind::Transport,
        message: "Probe execution failed without detailed error.".to_string(),
        response_preview: None,
    }))
}

fn execute_mcp_tool_probe_once(
    server: &MCPServerProfile,
    tool_name: &str,
    args: &Value,
    timeout_seconds: u64,
    setup_calls: &[(String, Value)],
) -> std::result::Result<McpToolCallOutcome, ProbeFailure> {
    let command = server.command.as_deref().ok_or_else(|| ProbeFailure {
        kind: ProbeErrorKind::Transport,
        message: "MCP server has no executable command.".to_string(),
        response_preview: None,
    })?;

    let mut child = Command::new(command)
        .args(&server.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| ProbeFailure {
            kind: ProbeErrorKind::Transport,
            message: format!("Failed to spawn MCP command '{command}': {err}"),
            response_preview: None,
        })?;

    let mut stdin = child.stdin.take().ok_or_else(|| ProbeFailure {
        kind: ProbeErrorKind::Transport,
        message: "Failed to open MCP stdin.".to_string(),
        response_preview: None,
    })?;
    let stdout = child.stdout.take().ok_or_else(|| ProbeFailure {
        kind: ProbeErrorKind::Transport,
        message: "Failed to open MCP stdout.".to_string(),
        response_preview: None,
    })?;
    let _stderr = child.stderr.take().ok_or_else(|| ProbeFailure {
        kind: ProbeErrorKind::Transport,
        message: "Failed to open MCP stderr.".to_string(),
        response_preview: None,
    })?;

    let (tx, rx) = mpsc::channel::<String>();
    let reader_handle = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let _ = tx.send(line.trim_end().to_string());
                }
                Err(_) => break,
            }
        }
    });

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
    let started = Instant::now();
    let deadline = started + Duration::from_secs(timeout_seconds.max(1));
    let mut buffered = BTreeMap::<i64, Value>::new();

    writeln!(stdin, "{init}").map_err(|err| ProbeFailure {
        kind: ProbeErrorKind::Transport,
        message: format!("Failed to write MCP initialize request: {err}"),
        response_preview: None,
    })?;
    let _ = wait_for_jsonrpc_response(1, &rx, &mut buffered, deadline);

    let mut next_setup_id = 100_i64;
    for (setup_tool, setup_args) in setup_calls {
        let setup_id = next_setup_id;
        next_setup_id += 1;
        let setup_call = serde_json::json!({
            "jsonrpc":"2.0",
            "id":setup_id,
            "method":"tools/call",
            "params":{"name":setup_tool,"arguments":setup_args}
        });
        writeln!(stdin, "{setup_call}").map_err(|err| ProbeFailure {
            kind: ProbeErrorKind::Transport,
            message: format!(
                "Failed to write setup tools/call for '{}': {err}",
                setup_tool
            ),
            response_preview: None,
        })?;
        let setup_response = wait_for_jsonrpc_response(setup_id, &rx, &mut buffered, deadline)
            .map_err(|mut err| {
                err.message = format!(
                    "Setup call '{}' failed before '{}' execution: {}",
                    setup_tool, tool_name, err.message
                );
                err
            })?;
        if setup_response.get("error").is_some()
            || setup_response
                .get("result")
                .and_then(|v| v.get("isError"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
            || setup_response
                .get("result")
                .and_then(|v| v.get("is_error"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            drop(stdin);
            let _ = child.kill();
            let _ = child.wait();
            return Err(ProbeFailure {
                kind: ProbeErrorKind::McpError,
                message: format!(
                    "Setup probe call for tool '{}' failed before '{}' execution.",
                    setup_tool, tool_name
                ),
                response_preview: Some(clipped_preview(
                    &value_to_compact_json(&setup_response),
                    260,
                )),
            });
        }
    }

    let target_call_id = 2_i64;
    let call = serde_json::json!({
        "jsonrpc":"2.0",
        "id":target_call_id,
        "method":"tools/call",
        "params":{"name":tool_name,"arguments":args}
    });
    writeln!(stdin, "{call}").map_err(|err| ProbeFailure {
        kind: ProbeErrorKind::Transport,
        message: format!("Failed to write MCP tools/call request: {err}"),
        response_preview: None,
    })?;
    let response = wait_for_jsonrpc_response(target_call_id, &rx, &mut buffered, deadline)?;

    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    // Some MCP servers spawn descendants that inherit stdout. Joining the
    // reader thread can then block even after the direct child has exited.
    drop(reader_handle);
    let duration_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;

    let response_preview = clipped_preview(&value_to_compact_json(&response), 260);
    if let Some(error) = response.get("error") {
        return Ok(McpToolCallOutcome {
            is_error: true,
            details: format!(
                "JSON-RPC error: {}",
                clipped_preview(&value_to_compact_json(error), 180)
            ),
            response_preview,
            duration_ms,
        });
    }

    let Some(result) = response.get("result") else {
        return Err(ProbeFailure {
            kind: ProbeErrorKind::Transport,
            message: "MCP response missing `result` payload for tools/call.".to_string(),
            response_preview: Some(response_preview),
        });
    };

    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || result
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let details = if is_error {
        "Tool returned result.isError=true".to_string()
    } else {
        "Tool returned success result.".to_string()
    };

    Ok(McpToolCallOutcome {
        is_error,
        details,
        response_preview,
        duration_ms,
    })
}

fn wait_for_jsonrpc_response(
    expected_id: i64,
    rx: &mpsc::Receiver<String>,
    buffered: &mut BTreeMap<i64, Value>,
    deadline: Instant,
) -> std::result::Result<Value, ProbeFailure> {
    if let Some(found) = buffered.remove(&expected_id) {
        return Ok(found);
    }

    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(ProbeFailure {
                kind: ProbeErrorKind::Timeout,
                message: format!("Timed out waiting for MCP response id={expected_id}."),
                response_preview: None,
            });
        }
        let remaining = deadline.saturating_duration_since(now);
        let wait = remaining.min(Duration::from_millis(200));
        match rx.recv_timeout(wait) {
            Ok(line) => {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let Some(id) = value.get("id").and_then(Value::as_i64) else {
                    continue;
                };
                if id == expected_id {
                    return Ok(value);
                }
                buffered.insert(id, value);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(ProbeFailure {
                    kind: ProbeErrorKind::Transport,
                    message: format!(
                        "MCP process closed before response id={expected_id} was received."
                    ),
                    response_preview: None,
                });
            }
        }
    }
}

pub(crate) fn value_to_compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<non-serializable>".to_string())
}
