use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde_json::Value;
use std::collections::BTreeSet;
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

pub(crate) fn remove_servers_from_config(
    path: &Path,
    server_names: &[String],
) -> Result<(Option<PathBuf>, Vec<String>)> {
    let requested = server_names
        .iter()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .map(ToString::to_string)
        .collect::<BTreeSet<_>>();
    if requested.is_empty() {
        return Ok((None, vec![]));
    }

    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read config {}", path.display()))?;

    if let Ok(mut root) = serde_json::from_str::<Value>(&raw) {
        let removed = remove_servers_from_json(&mut root, &requested);
        if removed.is_empty() {
            return Ok((None, vec![]));
        }

        let backup = backup_file(path)?;
        let body = serde_json::to_string_pretty(&root)
            .context("Failed to serialize updated JSON MCP config")?;
        std::fs::write(path, format!("{body}\n"))
            .with_context(|| format!("Failed to write config {}", path.display()))?;
        return Ok((Some(backup), removed));
    }

    if let Ok(mut root) = toml::from_str::<toml::Value>(&raw) {
        let removed = remove_servers_from_toml(&mut root, &requested);
        if removed.is_empty() {
            return Ok((None, vec![]));
        }

        let backup = backup_file(path)?;
        let body =
            toml::to_string_pretty(&root).context("Failed to serialize updated TOML MCP config")?;
        std::fs::write(path, body)
            .with_context(|| format!("Failed to write config {}", path.display()))?;
        return Ok((Some(backup), removed));
    }

    bail!(
        "Failed to parse {} as JSON or TOML for MCP config update.",
        path.display()
    )
}

fn remove_servers_from_json(root: &mut Value, server_names: &BTreeSet<String>) -> Vec<String> {
    let mut removed = BTreeSet::new();

    if let Some(obj) = root.get_mut("mcpServers").and_then(Value::as_object_mut) {
        remove_names_from_json_object(obj, server_names, &mut removed);
    }
    if let Some(obj) = root.get_mut("mcp_servers").and_then(Value::as_object_mut) {
        remove_names_from_json_object(obj, server_names, &mut removed);
    }
    if let Some(obj) = root.get_mut("servers").and_then(Value::as_object_mut) {
        remove_names_from_json_object(obj, server_names, &mut removed);
    }
    if let Some(obj) = root
        .get_mut("amp.mcpServers")
        .and_then(Value::as_object_mut)
    {
        remove_names_from_json_object(obj, server_names, &mut removed);
    }

    if let Some(amp) = root.get_mut("amp").and_then(Value::as_object_mut)
        && let Some(obj) = amp.get_mut("mcpServers").and_then(Value::as_object_mut)
    {
        remove_names_from_json_object(obj, server_names, &mut removed);
    }

    if let Some(obj) = root.as_object_mut() {
        for server_name in server_names {
            let should_remove = obj.get(server_name).is_some_and(likely_server_object);
            if should_remove && obj.remove(server_name).is_some() {
                removed.insert(server_name.clone());
            }
        }
    }

    removed.into_iter().collect()
}

fn remove_servers_from_toml(
    root: &mut toml::Value,
    server_names: &BTreeSet<String>,
) -> Vec<String> {
    let mut removed = BTreeSet::new();

    if let Some(table) = root.as_table_mut() {
        if let Some(mcp_servers) = table
            .get_mut("mcp_servers")
            .and_then(toml::Value::as_table_mut)
        {
            remove_names_from_toml_table(mcp_servers, server_names, &mut removed);
        }

        if let Some(amp_mcp) = table
            .get_mut("amp.mcpServers")
            .and_then(toml::Value::as_table_mut)
        {
            remove_names_from_toml_table(amp_mcp, server_names, &mut removed);
        }

        if let Some(amp) = table.get_mut("amp").and_then(toml::Value::as_table_mut)
            && let Some(mcp) = amp
                .get_mut("mcpServers")
                .and_then(toml::Value::as_table_mut)
        {
            remove_names_from_toml_table(mcp, server_names, &mut removed);
        }
    }

    removed.into_iter().collect()
}

fn remove_names_from_json_object(
    obj: &mut serde_json::Map<String, Value>,
    server_names: &BTreeSet<String>,
    removed: &mut BTreeSet<String>,
) {
    for server_name in server_names {
        if obj.remove(server_name).is_some() {
            removed.insert(server_name.clone());
        }
    }
}

fn remove_names_from_toml_table(
    table: &mut toml::map::Map<String, toml::Value>,
    server_names: &BTreeSet<String>,
    removed: &mut BTreeSet<String>,
) {
    for server_name in server_names {
        if table.remove(server_name).is_some() {
            removed.insert(server_name.clone());
        }
    }
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
