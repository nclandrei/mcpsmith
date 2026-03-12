use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn rollback_server_skill_files(orchestrator: &Path, tool_paths: &[PathBuf]) {
    for path in tool_paths {
        let _ = fs::remove_file(path);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }
    let _ = fs::remove_file(orchestrator);
    if let Some(parent) = orchestrator.parent() {
        let _ = fs::remove_dir_all(parent.join(".mcpsmith"));
        let _ = fs::remove_dir(parent);
    }
}

pub(crate) fn remove_server_from_config(
    path: &Path,
    server_name: &str,
) -> Result<(Option<PathBuf>, bool)> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read config {}", path.display()))?;

    if let Ok(mut root) = serde_json::from_str::<Value>(&raw) {
        let removed = remove_server_from_json(&mut root, server_name);
        if !removed {
            return Ok((None, false));
        }

        let backup = backup_file(path)?;
        let body = serde_json::to_string_pretty(&root)
            .context("Failed to serialize updated JSON MCP config")?;
        std::fs::write(path, format!("{body}\n"))
            .with_context(|| format!("Failed to write config {}", path.display()))?;
        return Ok((Some(backup), true));
    }

    if let Ok(mut root) = toml::from_str::<toml::Value>(&raw) {
        let removed = remove_server_from_toml(&mut root, server_name);
        if !removed {
            return Ok((None, false));
        }

        let backup = backup_file(path)?;
        let body =
            toml::to_string_pretty(&root).context("Failed to serialize updated TOML MCP config")?;
        std::fs::write(path, body)
            .with_context(|| format!("Failed to write config {}", path.display()))?;
        return Ok((Some(backup), true));
    }

    bail!(
        "Failed to parse {} as JSON or TOML for MCP config update.",
        path.display()
    )
}

fn remove_server_from_json(root: &mut Value, server_name: &str) -> bool {
    let mut removed = false;

    if let Some(obj) = root.get_mut("mcpServers").and_then(Value::as_object_mut) {
        removed |= obj.remove(server_name).is_some();
    }
    if let Some(obj) = root.get_mut("mcp_servers").and_then(Value::as_object_mut) {
        removed |= obj.remove(server_name).is_some();
    }
    if let Some(obj) = root.get_mut("servers").and_then(Value::as_object_mut) {
        removed |= obj.remove(server_name).is_some();
    }
    if let Some(obj) = root
        .get_mut("amp.mcpServers")
        .and_then(Value::as_object_mut)
    {
        removed |= obj.remove(server_name).is_some();
    }

    if let Some(amp) = root.get_mut("amp").and_then(Value::as_object_mut)
        && let Some(obj) = amp.get_mut("mcpServers").and_then(Value::as_object_mut)
    {
        removed |= obj.remove(server_name).is_some();
    }

    if let Some(obj) = root.as_object_mut() {
        let should_remove = obj.get(server_name).is_some_and(likely_server_object);
        if should_remove {
            removed |= obj.remove(server_name).is_some();
        }
    }

    removed
}

fn remove_server_from_toml(root: &mut toml::Value, server_name: &str) -> bool {
    let mut removed = false;

    if let Some(table) = root.as_table_mut() {
        if let Some(mcp_servers) = table
            .get_mut("mcp_servers")
            .and_then(toml::Value::as_table_mut)
        {
            removed |= mcp_servers.remove(server_name).is_some();
        }

        if let Some(amp_mcp) = table
            .get_mut("amp.mcpServers")
            .and_then(toml::Value::as_table_mut)
        {
            removed |= amp_mcp.remove(server_name).is_some();
        }

        if let Some(amp) = table.get_mut("amp").and_then(toml::Value::as_table_mut)
            && let Some(mcp) = amp
                .get_mut("mcpServers")
                .and_then(toml::Value::as_table_mut)
        {
            removed |= mcp.remove(server_name).is_some();
        }
    }

    removed
}

fn backup_file(path: &Path) -> Result<PathBuf> {
    let filename = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("mcp-config")
        .to_string();
    let backup_name = format!("{}.bak-{}", filename, Utc::now().format("%Y%m%d-%H%M%S"));
    let backup_path = path.with_file_name(backup_name);
    std::fs::copy(path, &backup_path).with_context(|| {
        format!(
            "Failed to create backup from {} to {}",
            path.display(),
            backup_path.display()
        )
    })?;
    Ok(backup_path)
}

fn likely_server_object(value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    let keys = [
        "command",
        "args",
        "url",
        "endpoint",
        "env",
        "description",
        "purpose",
        "permissions",
        "scopes",
        "capabilities",
        "tools",
    ];
    keys.iter().any(|key| obj.contains_key(*key))
}
